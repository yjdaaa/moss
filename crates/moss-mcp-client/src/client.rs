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

/// Unix 下挂在 `ProcessGroup::leader()` 之上的进程组收尾包装器：
/// - 正常等待退出（`wait` / `try_wait`）时先用 `waitid(P_PID, WEXITED | WNOWAIT)` 保持直接子进程僵尸态占位
///   （POSIX 保证未回收前其 PID/PGID 绝不会被内核复用），随即对整组发送 `SIGKILL` 清扫残留孙进程，
///   然后再委托内层回收直接子进程；
/// - 未回收前同步 `Drop` 时直接向 `-pgid` 发 `SIGKILL`，覆盖建连失败、超时与 Tokio 运行时关闭场景。
#[cfg(unix)]
#[derive(Debug)]
struct KillProcessGroupOnExit;

#[cfg(unix)]
#[derive(Debug)]
struct KillProcessGroupChild {
    inner: Option<Box<dyn process_wrap::tokio::ChildWrapper>>,
    pgid: i32,
    reaped: bool,
}

#[cfg(unix)]
impl KillProcessGroupChild {
    /// 返回 `Ok(true)` 表示直接子进程已退出且僵尸仍占位（可安全向 `-pgid` 发 SIGKILL），
    /// `Ok(false)` 表示非阻塞检查下仍在运行，`Err` 表示系统调用失败（如 `ECHILD`）。
    fn wait_exited_without_reaping(pgid: i32, nohang: bool) -> std::io::Result<bool> {
        let mut flags = libc::WEXITED | libc::WNOWAIT;
        if nohang {
            flags |= libc::WNOHANG;
        }
        loop {
            // SAFETY: siginfo_t is a plain C struct; zero-init is required before waitid with WNOHANG.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::waitid(libc::P_PID, pgid as libc::id_t, &mut info, flags) };
            if rc == 0 {
                // SAFETY: waitid returned 0 and info was zero-initialized; si_pid() works across macOS, Linux and Android.
                return Ok(unsafe { info.si_pid() } == pgid);
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }
    }

    fn kill_group(&self) {
        if !self.reaped && self.pgid > 0 {
            // SAFETY: 直接子进程尚未被 waitpid 回收，POSIX 保证其 PID/PGID 未被复用，
            // 向 -pgid 发送 SIGKILL 仅作用于 ProcessGroup::leader() 创建的独占进程组。
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for KillProcessGroupChild {
    fn drop(&mut self) {
        self.kill_group();
    }
}

#[cfg(unix)]
impl process_wrap::tokio::CommandWrapper for KillProcessGroupOnExit {
    fn wrap_child(
        &mut self,
        inner: Box<dyn process_wrap::tokio::ChildWrapper>,
        _core: &process_wrap::tokio::CommandWrap,
    ) -> std::io::Result<Box<dyn process_wrap::tokio::ChildWrapper>> {
        let pgid = inner
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .filter(|&id| id > 0)
            .ok_or_else(|| std::io::Error::other("missing child pid for process group guard"))?;
        Ok(Box::new(KillProcessGroupChild {
            inner: Some(inner),
            pgid,
            reaped: false,
        }))
    }
}

#[cfg(unix)]
impl process_wrap::tokio::ChildWrapper for KillProcessGroupChild {
    fn inner(&self) -> &dyn process_wrap::tokio::ChildWrapper {
        self.inner
            .as_deref()
            .expect("KillProcessGroupChild inner missing")
    }

    fn inner_mut(&mut self) -> &mut dyn process_wrap::tokio::ChildWrapper {
        self.inner
            .as_deref_mut()
            .expect("KillProcessGroupChild inner missing")
    }

    fn into_inner(mut self: Box<Self>) -> Box<dyn process_wrap::tokio::ChildWrapper> {
        self.reaped = true;
        self.inner
            .take()
            .expect("KillProcessGroupChild inner missing")
    }

    fn start_kill(&mut self) -> std::io::Result<()> {
        self.kill_group();
        self.inner_mut().start_kill()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if !self.reaped {
            match Self::wait_exited_without_reaping(self.pgid, true) {
                Ok(false) => return Ok(None),
                Ok(true) => self.kill_group(),
                Err(_) => {}
            }
        }
        let status = self.inner_mut().try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn wait(
        &mut self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_,
        >,
    > {
        Box::pin(async move {
            if !self.reaped {
                let pgid = self.pgid;
                let exited = tokio::task::spawn_blocking(move || {
                    Self::wait_exited_without_reaping(pgid, false)
                })
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(false);
                if exited {
                    self.kill_group();
                }
            }
            let status = self.inner_mut().wait().await?;
            self.reaped = true;
            Ok(status)
        })
    }
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
                    command.args(args).env_clear();
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
                    // 绑定 OS 进程树（KillOnDrop + Unix 独立进程组及退前清扫 / Windows CREATE_NO_WINDOW + KILL_ON_JOB_CLOSE Job Object）
                    let mut wrapped = process_wrap::tokio::CommandWrap::from(command);
                    wrapped.wrap(process_wrap::tokio::KillOnDrop);
                    #[cfg(unix)]
                    {
                        wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
                        wrapped.wrap(KillProcessGroupOnExit);
                    }
                    #[cfg(windows)]
                    {
                        wrapped.wrap(process_wrap::tokio::CreationFlags(
                            windows::Win32::System::Threading::CREATE_NO_WINDOW,
                        ));
                        wrapped.wrap(process_wrap::tokio::JobObject);
                    }
                    let (transport, _) = TokioChildProcess::builder(wrapped)
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
                    // 禁用系统/环境代理与重定向；明文 http://localhost 钉死解析到回环 127.0.0.1，防凭据外泄
                    let mut builder = reqwest::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .connect_timeout(Duration::from_secs(10))
                        .default_headers(headers);
                    if let Ok(parsed) = url::Url::parse(url)
                        && parsed.scheme() == "http"
                        && parsed.host_str() == Some("localhost")
                    {
                        let port = parsed.port_or_known_default().unwrap_or(80);
                        builder = builder.resolve(
                            "localhost",
                            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                        );
                    }
                    let http = builder.build().map_err(|_| ClientError::ConnectionFailed)?;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_failed_connect_kills_orphaned_grandchild_process_group() {
        struct EmptySecrets;
        impl SecretStore for EmptySecrets {
            fn get(&self, _: &str) -> Option<String> {
                None
            }
        }
        let pid_file = std::env::temp_dir().join(format!(
            "moss-mcp-grandchild-{}-{}.pid",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let script = format!(
            "sleep 30 >/dev/null 2>&1 </dev/null & echo $! > '{}'; exit 0",
            pid_file.display()
        );
        let config = ServerConfig {
            name: "grandchild_test".into(),
            enabled: true,
            transport: TransportConfig::Stdio {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), script],
                env_refs: BTreeMap::new(),
            },
            policies: BTreeMap::new(),
        };
        let err = Client::connect(config, &EmptySecrets, Arc::new(RequireApproval)).await;
        assert!(err.is_err());

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut grandchild_pid: Option<i32> = None;
        while std::time::Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse::<i32>()
                && pid > 0
            {
                grandchild_pid = Some(pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let _ = std::fs::remove_file(&pid_file);
        let pid = grandchild_pid.expect("script should have written grandchild pid");

        let wait_dead = std::time::Instant::now() + Duration::from_secs(2);
        let mut dead = false;
        while std::time::Instant::now() < wait_dead {
            // SAFETY: kill(pid, 0) only probes process existence without delivering a signal.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !dead {
            // Clean up defensively if assertion is about to fail.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        assert!(
            dead,
            "grandchild process {pid} survived MCP stdio process-group cleanup"
        );
    }
}
