use crate::{
    ClientError, MAX_DESCRIPTION_CHARS, MAX_TOOLS, Permission, ServerConfig, TransportConfig,
    tool_alias,
};
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation, ProtocolVersion};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{ClientLifecycleMode, ClientServiceExt, RoleClient};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const MAX_ARGUMENT_BYTES: usize = 32 * 1024;
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Service = RunningService<RoleClient, ClientConfig>;

pub trait SecretStore: Send + Sync {
    fn get(&self, reference: &str) -> Option<String>;
}
impl SecretStore for BTreeMap<String, String> {
    fn get(&self, reference: &str) -> Option<String> {
        BTreeMap::get(self, reference).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct CallStats {
    pub calls: u64,
    pub failures: u64,
    pub approval_requests: u64,
    pub last_call_at: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallIntent {
    pub server: String,
    pub tool: String,
    pub arguments: Map<String, Value>,
    pub digest: String,
}

pub enum Authorization {
    Pending,
    Approved { digest: String },
}
/// 宿主必须持久化审批、检验一次性消费、有效期和当前权限；不得信任模型传来的 confirmed / approval_id。
pub trait ApprovalGate: Send + Sync {
    fn authorize<'a>(
        &'a self,
        intent: &'a CallIntent,
    ) -> BoxFuture<'a, Result<Authorization, ClientError>>;
}
pub struct RequireApproval;
impl ApprovalGate for RequireApproval {
    fn authorize<'a>(
        &'a self,
        _: &'a CallIntent,
    ) -> BoxFuture<'a, Result<Authorization, ClientError>> {
        Box::pin(async { Ok(Authorization::Pending) })
    }
}
#[derive(Debug)]
pub enum CallOutcome {
    ApprovalRequired(CallIntent),
    Completed(Value),
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 一条已明确配置的连接。所有调用串行借用；不隐式启动外部服务，不进行工具调用重试。
/// 宿主须每 HEARTBEAT_INTERVAL 调用 heartbeat，失败后通过 ReconnectBackoff 调度 reconnect。
pub struct Client {
    config: ServerConfig,
    session: Option<Service>,
    definitions: BTreeMap<String, (String, ToolDefinition)>,
    stats: BTreeMap<String, CallStats>,
    connection_digest: String,
    gate: Arc<dyn ApprovalGate>,
    live: bool,
}

impl Client {
    pub async fn connect(
        config: ServerConfig,
        secrets: &dyn SecretStore,
        gate: Arc<dyn ApprovalGate>,
    ) -> Result<Self, ClientError> {
        config.validate()?;
        if !config.enabled {
            return Err(ClientError::Disabled);
        }
        let references = match &config.transport {
            TransportConfig::Stdio { env_refs, .. } => env_refs,
            TransportConfig::StreamableHttp { header_refs, .. } => header_refs,
        };
        let mut values = BTreeMap::new();
        for (key, reference) in references {
            let value = secrets
                .get(reference)
                .filter(|value| {
                    !value.is_empty() && value.len() <= 16 * 1024 && !value.contains('\0')
                })
                .ok_or(ClientError::MissingSecret)?;
            values.insert(key.clone(), value);
        }
        // 凭据轮换、连接目标或工具 schema 变化都会使旧审批摘要失效；摘要不暴露原文。
        let connection_digest =
            hash(&serde_json::to_vec(&(&config, &values)).map_err(|_| ClientError::InvalidConfig)?);
        let info = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("moss-mcp-client", env!("CARGO_PKG_VERSION")),
        );
        let lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        };
        let connect = async {
            match &config.transport {
                TransportConfig::Stdio { command, args, .. } => {
                    let mut command = tokio::process::Command::new(command);
                    command.args(args).env_clear().kill_on_drop(true);
                    #[cfg(windows)]
                    command.creation_flags(0x0800_0000);
                    // 不继承模型 API key / token 等任意宿主环境变量。
                    for key in [
                        "PATH",
                        "SystemRoot",
                        "WINDIR",
                        "TEMP",
                        "TMP",
                        "HOME",
                        "USERPROFILE",
                        "LANG",
                    ] {
                        if let Some(value) = std::env::var_os(key) {
                            command.env(key, value);
                        }
                    }
                    command.envs(&values);
                    let (transport, _) = TokioChildProcess::builder(command)
                        .stderr(Stdio::null())
                        .spawn()
                        .map_err(|_| ClientError::ConnectionFailed)?;
                    info.serve_with_lifecycle(transport, lifecycle)
                        .await
                        .map_err(|_| ClientError::ConnectionFailed)
                }
                TransportConfig::StreamableHttp { url, .. } => {
                    let mut headers = reqwest::header::HeaderMap::new();
                    for (key, value) in &values {
                        let name = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                            .map_err(|_| ClientError::InvalidSecretRef)?;
                        let mut value = reqwest::header::HeaderValue::from_str(value)
                            .map_err(|_| ClientError::InvalidSecretRef)?;
                        value.set_sensitive(true);
                        headers.insert(name, value);
                    }
                    let http = reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .connect_timeout(Duration::from_secs(10))
                        .default_headers(headers)
                        .build()
                        .map_err(|_| ClientError::ConnectionFailed)?;
                    let transport_config =
                        StreamableHttpClientTransportConfig::with_uri(url.clone())
                            .max_sse_event_size(MAX_RESPONSE_BYTES)
                            .max_concurrent_requests(1)
                            .reinit_on_expired_session(false);
                    let transport =
                        StreamableHttpClientTransport::with_client(http, transport_config);
                    info.serve_with_lifecycle(transport, lifecycle)
                        .await
                        .map_err(|_| ClientError::ConnectionFailed)
                }
            }
        };
        let session = tokio::time::timeout(CALL_TIMEOUT, connect)
            .await
            .map_err(|_| ClientError::ConnectionFailed)??;
        let mut client = Self {
            config,
            session: Some(session),
            definitions: BTreeMap::new(),
            stats: BTreeMap::new(),
            connection_digest,
            gate,
            live: false,
        };
        match tokio::time::timeout(CALL_TIMEOUT, client.discover()).await {
            Ok(Ok(())) => {
                client.live = true;
                Ok(client)
            }
            result => {
                client.disconnect().await;
                match result {
                    Ok(Err(error)) => Err(error),
                    _ => Err(ClientError::DiscoveryFailed),
                }
            }
        }
    }

    async fn discover(&mut self) -> Result<(), ClientError> {
        let session = self.session.as_ref().ok_or(ClientError::Unavailable)?;
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        for _ in 0..8 {
            let params = serde_json::from_value(serde_json::json!({"cursor":cursor}))
                .map_err(|_| ClientError::DiscoveryFailed)?;
            let page = session
                .list_tools(params)
                .await
                .map_err(|_| ClientError::DiscoveryFailed)?;
            for tool in page.tools {
                if self.definitions.len() >= crate::MAX_DISCOVERED_TOOLS
                    || serde_json::to_vec(&tool.input_schema)
                        .map_err(|_| ClientError::DiscoveryFailed)?
                        .len()
                        > MAX_ARGUMENT_BYTES
                {
                    return Err(ClientError::CatalogTooLarge);
                }
                let original = tool.name.to_string();
                let name = tool_alias(&self.config.name, &original)?;
                let prefix = format!("第三方 {}：", self.config.name);
                let description = format!(
                    "{prefix}{}",
                    tool.description
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(MAX_DESCRIPTION_CHARS.saturating_sub(prefix.chars().count()))
                        .collect::<String>()
                );
                let definition = ToolDefinition {
                    name: name.clone(),
                    description,
                    parameters: Value::Object((*tool.input_schema).clone()),
                };
                if self
                    .definitions
                    .insert(name, (original, definition))
                    .is_some()
                {
                    return Err(ClientError::DuplicateName);
                }
            }
            cursor = page.next_cursor;
            let Some(next) = cursor.as_ref() else {
                return Ok(());
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(ClientError::DiscoveryFailed);
            }
        }
        Err(ClientError::CatalogTooLarge)
    }

    /// 宿主传入剩余预算及已占用名称，不能覆盖 MOSS / 设备自带工具。
    pub fn tools(&self, reserved: &HashSet<String>, remaining: usize) -> Vec<ToolDefinition> {
        if !self.live || !self.config.enabled {
            return Vec::new();
        }
        self.definitions
            .values()
            .filter(|(original, tool)| {
                self.config.permission(original) != Permission::Disabled
                    && !reserved.contains(&tool.name)
            })
            .take(remaining.min(MAX_TOOLS))
            .map(|(_, tool)| tool.clone())
            .collect()
    }

    pub fn stats(&self) -> &BTreeMap<String, CallStats> {
        &self.stats
    }
    pub fn is_connected(&self) -> bool {
        self.live
    }
    pub fn set_permission(
        &mut self,
        original: &str,
        permission: Permission,
    ) -> Result<(), ClientError> {
        let alias = tool_alias(&self.config.name, original)?;
        if !self.definitions.contains_key(&alias) {
            return Err(ClientError::InvalidName);
        }
        self.config.policies.insert(original.to_owned(), permission);
        Ok(())
    }

    /// Prepare is observational. The host must authorize this exact digest before dispatch.
    pub fn prepare(
        &self,
        alias: &str,
        arguments: Value,
    ) -> Result<(CallIntent, Permission), ClientError> {
        let (original, definition) = self
            .definitions
            .get(alias)
            .ok_or(ClientError::InvalidName)?;
        let permission = self.config.permission(original);
        if permission == Permission::Disabled {
            return Err(ClientError::Disabled);
        }
        if !self.live {
            return Err(ClientError::Unavailable);
        }
        let arguments = arguments
            .as_object()
            .cloned()
            .ok_or(ClientError::InvalidArguments)?;
        if serde_json::to_vec(&arguments)
            .map_err(|_| ClientError::InvalidArguments)?
            .len()
            > MAX_ARGUMENT_BYTES
        {
            return Err(ClientError::InvalidArguments);
        }
        let digest = hash(
            &serde_json::to_vec(&(
                &self.connection_digest,
                &self.config,
                original,
                &definition.parameters,
                &arguments,
            ))
            .map_err(|_| ClientError::InvalidArguments)?,
        );
        Ok((
            CallIntent {
                server: self.config.name.clone(),
                tool: original.clone(),
                arguments,
                digest,
            },
            permission,
        ))
    }

    pub fn catalog(&self) -> Vec<(String, ToolDefinition, Permission)> {
        self.definitions
            .values()
            .map(|(original, definition)| {
                (
                    original.clone(),
                    definition.clone(),
                    self.config.permission(original),
                )
            })
            .collect()
    }

    pub async fn refresh_tools(&mut self) -> Result<(), ClientError> {
        self.definitions.clear();
        let result = tokio::time::timeout(CALL_TIMEOUT, self.discover())
            .await
            .unwrap_or(Err(ClientError::DiscoveryFailed));
        if result.is_err() {
            self.live = false;
        }
        result
    }

    pub(crate) fn restore_stats(&mut self, stats: BTreeMap<String, CallStats>) {
        self.stats = stats;
    }

    /// Only Manager may use this path, after its trusted host guard has authorized the digest.
    pub(crate) async fn execute_prepared(
        &mut self,
        intent: &CallIntent,
    ) -> Result<Value, ClientError> {
        let alias = tool_alias(&intent.server, &intent.tool)?;
        let (current, _) = self.prepare(&alias, Value::Object(intent.arguments.clone()))?;
        if current.digest != intent.digest {
            return Err(ClientError::ApprovalFailed);
        }
        let stats = self.stats.entry(alias).or_default();
        stats.calls = stats.calls.saturating_add(1);
        stats.last_call_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs());
        let params = serde_json::from_value(
            serde_json::json!({"name":intent.tool,"arguments":intent.arguments}),
        )
        .map_err(|_| ClientError::InvalidArguments)?;
        let session = self.session.as_ref().ok_or(ClientError::Unavailable)?;
        match tokio::time::timeout(CALL_TIMEOUT, session.call_tool(params)).await {
            Ok(Ok(result)) => {
                let failed = result.is_error == Some(true);
                let value =
                    serde_json::to_value(result).map_err(|_| ClientError::ResponseTooLarge)?;
                if serde_json::to_vec(&value)
                    .map_err(|_| ClientError::ResponseTooLarge)?
                    .len()
                    > MAX_RESPONSE_BYTES
                {
                    stats.failures = stats.failures.saturating_add(1);
                    return Err(ClientError::ResponseTooLarge);
                }
                if failed {
                    stats.failures = stats.failures.saturating_add(1);
                }
                Ok(value)
            }
            _ => {
                stats.failures = stats.failures.saturating_add(1);
                self.live = false;
                Err(ClientError::OutcomeUnknown)
            }
        }
    }

    pub async fn call(
        &mut self,
        alias: &str,
        arguments: Value,
    ) -> Result<CallOutcome, ClientError> {
        let (original, definition) = self
            .definitions
            .get(alias)
            .ok_or(ClientError::InvalidName)?;
        let permission = self.config.permission(original);
        if permission == Permission::Disabled {
            return Err(ClientError::Disabled);
        }
        if !self.live {
            return Err(ClientError::Unavailable);
        }
        let arguments = arguments
            .as_object()
            .cloned()
            .ok_or(ClientError::InvalidArguments)?;
        let encoded = serde_json::to_vec(&arguments).map_err(|_| ClientError::InvalidArguments)?;
        if encoded.len() > MAX_ARGUMENT_BYTES {
            return Err(ClientError::InvalidArguments);
        }
        let digest = hash(
            &serde_json::to_vec(&(
                &self.connection_digest,
                &self.config,
                original,
                &definition.parameters,
                &arguments,
            ))
            .map_err(|_| ClientError::InvalidArguments)?,
        );
        let intent = CallIntent {
            server: self.config.name.clone(),
            tool: original.clone(),
            arguments: arguments.clone(),
            digest,
        };
        if permission == Permission::Ask {
            let stats = self.stats.entry(alias.into()).or_default();
            stats.approval_requests = stats.approval_requests.saturating_add(1);
            match tokio::time::timeout(CALL_TIMEOUT, self.gate.authorize(&intent))
                .await
                .map_err(|_| ClientError::ApprovalFailed)??
            {
                Authorization::Pending => return Ok(CallOutcome::ApprovalRequired(intent)),
                Authorization::Approved { digest } if digest == intent.digest => {}
                Authorization::Approved { .. } => return Err(ClientError::ApprovalFailed),
            }
        }
        let stats = self.stats.entry(alias.into()).or_default();
        stats.calls = stats.calls.saturating_add(1);
        stats.last_call_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|time| time.as_secs());
        let params =
            serde_json::from_value(serde_json::json!({"name":original,"arguments":arguments}))
                .map_err(|_| ClientError::InvalidArguments)?;
        let session = self.session.as_ref().ok_or(ClientError::Unavailable)?;
        let result = tokio::time::timeout(CALL_TIMEOUT, session.call_tool(params)).await;
        match result {
            Ok(Ok(result)) => {
                if result.is_error == Some(true) {
                    stats.failures = stats.failures.saturating_add(1);
                }
                let value =
                    serde_json::to_value(result).map_err(|_| ClientError::ResponseTooLarge)?;
                if serde_json::to_vec(&value)
                    .map_err(|_| ClientError::ResponseTooLarge)?
                    .len()
                    > MAX_RESPONSE_BYTES
                {
                    return Err(ClientError::ResponseTooLarge);
                }
                Ok(CallOutcome::Completed(value))
            }
            _ => {
                stats.failures = stats.failures.saturating_add(1);
                self.live = false;
                Err(ClientError::OutcomeUnknown)
            }
        }
    }

    pub async fn heartbeat(&mut self) -> Result<(), ClientError> {
        let Some(session) = self.session.as_ref() else {
            return Err(ClientError::Unavailable);
        };
        if !matches!(
            tokio::time::timeout(
                CALL_TIMEOUT,
                session.send_request(
                    rmcp::model::PingRequest {
                        method: Default::default(),
                        extensions: Default::default()
                    }
                    .into()
                )
            )
            .await,
            Ok(Ok(_))
        ) {
            self.live = false;
            return Err(ClientError::Unavailable);
        }
        Ok(())
    }

    pub async fn disconnect(&mut self) {
        self.live = false;
        if let Some(session) = self.session.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), session.cancel()).await;
        }
    }

    pub async fn reconnect(&mut self, secrets: &dyn SecretStore) -> Result<(), ClientError> {
        self.disconnect().await;
        let mut replacement =
            Self::connect(self.config.clone(), secrets, self.gate.clone()).await?;
        replacement.stats = std::mem::take(&mut self.stats);
        *self = replacement;
        Ok(())
    }
}

#[derive(Default)]
pub struct ReconnectBackoff {
    attempts: u32,
}
impl ReconnectBackoff {
    pub fn next_delay(&mut self) -> Duration {
        let seconds = 1_u64
            .checked_shl(self.attempts.min(6))
            .unwrap_or(60)
            .min(60);
        self.attempts = self.attempts.saturating_add(1);
        Duration::from_secs(seconds)
    }
    pub fn reset_after_healthy_connection(&mut self) {
        self.attempts = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_backoff_resets_after_success() {
        let mut backoff = ReconnectBackoff::default();
        assert_eq!(
            (0..9)
                .map(|_| backoff.next_delay().as_secs())
                .collect::<Vec<_>>(),
            [1, 2, 4, 8, 16, 32, 60, 60, 60]
        );
        backoff.reset_after_healthy_connection();
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
    }
}
