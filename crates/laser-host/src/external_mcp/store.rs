//! Desktop-only persistent configuration. No credential value is returned by snapshot().
use laser_contracts::*;
use laser_runtime::RuntimeDb;
use moss_mcp_client::{Permission, ServerConfig, TransportConfig, secret_ref, validate_servers};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveInput {
    pub expected_revision: String,
    pub config: ServerConfig,
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub allow_without_approval: bool,
}

pub(super) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_secs_f64())
        .unwrap_or(0.0)
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(super) fn refs(config: &ServerConfig) -> &BTreeMap<String, String> {
    match &config.transport {
        TransportConfig::Stdio { env_refs, .. } => env_refs,
        TransportConfig::StreamableHttp { header_refs, .. } => header_refs,
    }
}
fn revision(configs: &[ServerConfig]) -> String {
    hash(&serde_json::to_vec(configs).expect("configuration serializes"))
}

pub(super) struct Store {
    pub file: PathBuf,
    pub db: Arc<Mutex<RuntimeDb>>,
}
impl Store {
    pub fn configs(&self) -> Result<Vec<ServerConfig>, String> {
        moss_voice::settings::read_mcp_servers(&self.file)
    }
    pub fn secrets(&self, config: &ServerConfig) -> Result<BTreeMap<String, String>, String> {
        let db = self.db.lock().map_err(|_| "凭据库不可用")?;
        let mut values = BTreeMap::new();
        for reference in refs(config).values() {
            if let Some(value) = db.get_secret(reference).map_err(|_| "无法读取 MCP 凭据")? {
                values.insert(reference.clone(), value);
            }
        }
        Ok(values)
    }
    pub fn snapshot(&self) -> Result<Value, String> {
        let configs = self.configs()?;
        let db = self.db.lock().map_err(|_| "凭据库不可用")?;
        let mut servers = Vec::new();
        for config in &configs {
            let mut secret_set = BTreeMap::new();
            for reference in refs(config).values() {
                secret_set.insert(
                    reference.clone(),
                    db.get_secret(reference)
                        .map_err(|_| "无法读取凭据状态")?
                        .is_some_and(|v| !v.is_empty()),
                );
            }
            servers.push(json!({"config":config,"secret_set":secret_set}));
        }
        Ok(json!({"revision":revision(&configs),"servers":servers}))
    }
    pub fn save(&self, mut input: SaveInput, stop: impl FnOnce()) -> Result<ServerConfig, String> {
        if !input.confirmed {
            return Err("请确认第三方程序及其数据访问权限后再保存".into());
        }
        input.config.validate().map_err(|e| e.to_string())?;
        if input
            .config
            .policies
            .values()
            .any(|p| *p == Permission::Allow)
            && !input.allow_without_approval
        {
            return Err("免批准工具需要单独确认只读风险".into());
        }
        if input.secrets.len() > 32
            || input
                .secrets
                .values()
                .any(|v| v.is_empty() || v.len() > 16 * 1024 || v.contains('\0'))
        {
            return Err("凭据字段无效".into());
        }
        if serde_json::to_vec(&input.secrets)
            .map_err(|_| "凭据字段无效")?
            .len()
            > 64 * 1024
        {
            return Err("凭据总长度超过限制".into());
        }
        let mut configs = self.configs()?;
        if revision(&configs) != input.expected_revision {
            return Err("配置已被修改，请刷新后重试".into());
        }
        let old = configs
            .iter()
            .find(|c| c.name == input.config.name)
            .cloned();
        let requested: BTreeSet<_> = refs(&input.config).values().cloned().collect();
        if input.secrets.keys().any(|key| !requested.contains(key)) {
            return Err("不能修改其他服务或模型的凭据".into());
        }
        let db = self.db.lock().map_err(|_| "凭据库不可用")?;
        let mut values = BTreeMap::new();
        for (key, reference) in refs(&input.config) {
            let value = match input.secrets.remove(reference) {
                Some(value) => Some(value),
                None if old
                    .as_ref()
                    .is_some_and(|old| refs(old).get(key) == Some(reference)) =>
                {
                    db.get_secret(reference).map_err(|_| "无法读取原凭据")?
                }
                None => None,
            };
            if input.config.enabled && value.is_none() {
                return Err("启用前请填写所有凭据；留空只保留本服务已保存的值".into());
            }
            values.insert(key.clone(), value);
        }
        // Unique immutable references are committed BEFORE the settings file. A crash cannot pair a new target with stale keys.
        let stamp = hash(
            format!(
                "{}:{}:{}",
                input.config.name,
                input.expected_revision,
                now()
            )
            .as_bytes(),
        );
        let kind = match input.config.transport {
            TransportConfig::Stdio { .. } => "env",
            TransportConfig::StreamableHttp { .. } => "header",
        };
        let new_refs: BTreeMap<_, _> = values
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    format!(
                        "{}.v-{}",
                        secret_ref(&input.config.name, kind, key),
                        &stamp[..32]
                    ),
                )
            })
            .collect();
        match &mut input.config.transport {
            TransportConfig::Stdio { env_refs, .. } => *env_refs = new_refs.clone(),
            TransportConfig::StreamableHttp { header_refs, .. } => *header_refs = new_refs.clone(),
        };
        configs.retain(|c| c.name != input.config.name);
        configs.push(input.config.clone());
        configs.sort_by(|a, b| a.name.cmp(&b.name));
        validate_servers(&configs).map_err(|e| e.to_string())?;
        stop();
        let tx = db
            .connection
            .unchecked_transaction()
            .map_err(|_| "无法保存 MCP 凭据")?;
        for (key, value) in values {
            if let Some(value) = value {
                db.set_secret(&new_refs[&key], &value, now())
                    .map_err(|_| "无法保存 MCP 凭据")?;
            }
        }
        invalidate(&db, &input.config.name)?;
        db.append_audit(&config_audit(
            &input.config.name,
            "准备保存配置；服务已暂停，尚未声明文件写入成功",
            AuditResult::Ok,
        ))
        .map_err(|_| "审计写入失败，未启用服务")?;
        tx.commit().map_err(|_| "凭据事务提交失败，未启用服务")?;
        drop(db);
        // Old credential references are retained on failure (and after crashes); they are unreachable from the new config.
        moss_voice::settings::save_mcp_servers(&self.file, &configs)?;
        let db = self.db.lock().map_err(|_| "凭据库不可用；服务保持停止")?;
        db.append_audit(&config_audit(
            &input.config.name,
            "配置保存成功",
            AuditResult::Ok,
        ))
        .map_err(|_| "配置已保存但审计失败；服务保持停止")?;
        if let Some(old) = old {
            for reference in refs(&old).values() {
                if !new_refs.values().any(|v| v == reference) {
                    let _ = db.delete_secret(reference);
                }
            }
        }
        Ok(input.config)
    }
    pub fn delete(
        &self,
        name: &str,
        expected: &str,
        confirmed: bool,
        stop: impl FnOnce(),
    ) -> Result<(), String> {
        if !confirmed {
            return Err("请确认删除此第三方服务".into());
        }
        let mut configs = self.configs()?;
        if revision(&configs) != expected {
            return Err("配置已被修改，请刷新后重试".into());
        }
        let old = configs
            .iter()
            .find(|c| c.name == name)
            .cloned()
            .ok_or("未找到该服务")?;
        stop();
        let db = self.db.lock().map_err(|_| "凭据库不可用")?;
        invalidate(&db, name)?;
        db.append_audit(&config_audit(name, "准备移除第三方配置", AuditResult::Ok))
            .map_err(|_| "审计写入失败，服务保持停止")?;
        drop(db);
        configs.retain(|c| c.name != name);
        moss_voice::settings::save_mcp_servers(&self.file, &configs)?;
        let db = self.db.lock().map_err(|_| "凭据库不可用")?;
        for reference in refs(&old).values() {
            db.delete_secret(reference)
                .map_err(|_| "配置已移除，凭据清理未完成")?;
        }
        db.append_audit(&config_audit(name, "第三方配置已移除", AuditResult::Ok))
            .map_err(|_| "配置已移除，审计写入失败")?;
        Ok(())
    }
}

fn config_audit(name: &str, reason: &str, result: AuditResult) -> AuditEntry {
    AuditEntry {
        at: now(),
        client: "desktop".into(),
        action: AuditAction::ExternalMcpConfigChanged,
        target: name.into(),
        result,
        detail: AuditDetail {
            reason: Some(reason.into()),
            ..Default::default()
        },
    }
}
fn invalidate(db: &RuntimeDb, server: &str) -> Result<(), String> {
    for record in db
        .approvals_due_for_expiry(now(), true)
        .map_err(|_| "无法检查旧审批")?
    {
        if record.kind != ApprovalKind::ExternalTool
            || !matches!(&record.connection,Some(ApprovalConnection::External{server:name,..}) if name==server)
        {
            continue;
        }
        db.transition_approval(
            &record.approval_id,
            record.status,
            ApprovalStatus::Expired,
            now(),
            None,
        )
        .map_err(|_| "无法作废旧审批")?;
        db.append_audit(&AuditEntry {
            at: now(),
            client: "desktop".into(),
            action: AuditAction::ApprovalExpired,
            target: record.target_record,
            result: AuditResult::Denied,
            detail: AuditDetail {
                approval_id: Some(record.approval_id),
                kind: Some(ApprovalKind::ExternalTool),
                reason: Some("第三方配置、凭据或权限已修改".into()),
                ..Default::default()
            },
        })
        .map_err(|_| "无法记录审批作废")?;
    }
    Ok(())
}
