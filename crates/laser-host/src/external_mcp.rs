//! NV9: desktop configuration, connection lifetime, persistent approvals, audit and voice dispatch.
//! No external configuration is accepted from HTTP/MCP/voice callers.
mod store;
#[cfg(test)]
mod tests;
use laser_api::{
    ApiService,
    approval::{ApprovalError, NewApproval, approval_digest},
    external_tools::ExternalTools,
};
use laser_contracts::*;
use laser_runtime::RuntimeDb;
use moss_mcp_client::{CallIntent, ClientError, DispatchGuard, Manager, Permission, PreparedCall};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
pub use store::SaveInput;
use store::{Store, now};

pub struct ExternalMcp {
    store: Store,
    api: Weak<ApiService>,
    manager: Manager,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    handle: tokio::runtime::Handle,
    admin: Mutex<()>,
    startup_error: Mutex<Option<String>>,
    stopped: AtomicBool,
}
impl ExternalMcp {
    pub fn attach(
        api: &Arc<ApiService>,
        db: Arc<Mutex<RuntimeDb>>,
        file: PathBuf,
    ) -> Result<Arc<Self>, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moss-external-mcp")
            .enable_all()
            .build()
            .map_err(|_| "无法启动 MCP 连接线程")?;
        let handle = runtime.handle().clone();
        let reserved: HashSet<String> = laser_mcp::tools_list_for(true, false)["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect();
        let external = Arc::new(Self {
            store: Store { file, db },
            api: Arc::downgrade(api),
            manager: Manager::new(handle.clone(), reserved),
            runtime: Mutex::new(Some(runtime)),
            handle,
            admin: Mutex::new(()),
            startup_error: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });
        api.set_external_tools(external.clone())?;
        match external.store.configs() {
            Ok(configs) => {
                for config in configs {
                    match external.store.secrets(&config).and_then(|values| {
                        external
                            .manager
                            .configure(config, values)
                            .map_err(|e| e.to_string())
                    }) {
                        Ok(()) => {}
                        Err(error) => {
                            if let Ok(mut slot) = external.startup_error.lock() {
                                *slot = Some(error);
                            }
                        }
                    }
                }
            }
            Err(error) => {
                if let Ok(mut slot) = external.startup_error.lock() {
                    *slot = Some(error);
                }
            }
        }
        Ok(external)
    }
    pub fn status(&self) -> Result<Value, String> {
        let _guard = self.admin.lock().map_err(|_| "MCP 配置锁不可用")?;
        let mut view = self.store.snapshot()?;
        view["connections"] =
            serde_json::to_value(self.manager.snapshot()).map_err(|_| "MCP 状态不可用")?;
        view["max_servers"] = json!(moss_mcp_client::MAX_SERVERS);
        view["max_tools"] = json!(moss_mcp_client::MAX_TOOLS);
        view["last_error"] = json!(self.startup_error.lock().ok().and_then(|v| v.clone()));
        let db = self.store.db.lock().map_err(|_| "审计库不可用")?;
        // Show only structured audit fields, never request/response bodies or credential values.
        let audit: Vec<_> = db
            .recent_audit(200)
            .map_err(|_| "无法读取审计")?
            .into_iter()
            .filter(|e| {
                e.detail.kind == Some(ApprovalKind::ExternalTool)
                    || matches!(
                        e.action,
                        AuditAction::ExternalToolRequested
                            | AuditAction::ExternalToolCompleted
                            | AuditAction::ExternalToolDenied
                            | AuditAction::ExternalMcpConfigChanged
                    )
            })
            .take(50)
            .collect();
        view["audit"] = serde_json::to_value(audit).map_err(|_| "审计不可用")?;
        Ok(view)
    }
    pub fn save(&self, input: SaveInput) -> Result<Value, String> {
        let guard = self.admin.lock().map_err(|_| "MCP 配置锁不可用")?;
        if self.stopped.load(Ordering::Acquire) {
            return Err("MCP 已停止".into());
        }
        let name = input.config.name.clone();
        let config = self.store.save(input, || self.manager.suspend(&name))?;
        self.manager
            .configure(config.clone(), self.store.secrets(&config)?)
            .map_err(|e| e.to_string())?;
        if let Ok(mut error) = self.startup_error.lock() {
            *error = None;
        }
        drop(guard);
        self.status()
    }
    pub fn delete(&self, name: &str, revision: &str, confirmed: bool) -> Result<Value, String> {
        let guard = self.admin.lock().map_err(|_| "MCP 配置锁不可用")?;
        self.store
            .delete(name, revision, confirmed, || self.manager.remove(name))?;
        drop(guard);
        self.status()
    }
    pub fn reconnect(&self, name: &str, confirmed: bool) -> Result<Value, String> {
        if !confirmed {
            return Err("请确认重新连接；本机服务可能重新启动程序".into());
        }
        let guard = self.admin.lock().map_err(|_| "MCP 配置锁不可用")?;
        if self.stopped.load(Ordering::Acquire) {
            return Err("MCP 已停止".into());
        }
        let config = self
            .store
            .configs()?
            .into_iter()
            .find(|c| c.name == name && c.enabled)
            .ok_or("服务不存在或尚未启用")?;
        self.manager
            .configure(config.clone(), self.store.secrets(&config)?)
            .map_err(|e| e.to_string())?;
        drop(guard);
        self.status()
    }
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        self.manager.shutdown();
        if let Ok(mut runtime) = self.runtime.lock()
            && let Some(runtime) = runtime.take()
        {
            runtime.shutdown_background();
        }
    }
    fn prepare(&self, name: &str, args: Value) -> Result<PreparedCall, ClientError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(ClientError::Unavailable);
        }
        self.handle.block_on(self.manager.prepare(name, args))
    }
    fn pending(
        &self,
        context: &CallerContext,
        prepared: &PreparedCall,
        permit: Arc<AtomicBool>,
    ) -> Result<ToolCallResult, String> {
        let api = self.api.upgrade().ok_or("MOSS 已停止")?;
        let service = api.approvals().ok_or("审批服务不可用")?;
        let CallerContext::Voice {
            device_id,
            session_id,
        } = context
        else {
            return Err("不是内置语音会话".into());
        };
        // Bound outstanding approvals from a noisy device; no caller-supplied id is reused.
        let outstanding = service
            .list(Some(ApprovalStatus::Pending), 512)
            .map_err(|_| "无法检查待审批请求")?;
        if outstanding.len() >= 256
            || outstanding
                .iter()
                .filter(|r| {
                    r.source == ApprovalSource::Voice && r.session_id.as_deref() == Some(device_id)
                })
                .count()
                >= 16
        {
            return Err("待审批请求过多，请先处理电脑上的请求".into());
        }
        let alias = moss_mcp_client::tool_alias(&prepared.intent.server, &prepared.intent.tool)
            .map_err(|e| e.to_string())?;
        let request=json!({"arguments":prepared.intent.arguments,"client_digest":prepared.intent.digest,"device_id":device_id,"voice_session_id":session_id}).as_object().cloned().unwrap();
        let connection = connection(&prepared.intent);
        let record = service
            .create(NewApproval {
                owner: Some(json!([device_id, session_id]).to_string()),
                voice_permit: Some(permit),
                kind: ApprovalKind::ExternalTool,
                target_record: alias.clone(),
                source: ApprovalSource::Voice,
                session_id: Some(device_id.clone()),
                request_id: None,
                summary: format!("调用第三方工具 {alias}；请核对参数与数据访问范围"),
                facts: request.clone(),
                connection,
                request,
                client: audit_client(context),
                removed_fields: vec![],
            })
            .map_err(|error| error.message())?;
        let speech = "已发到电脑，请核对第三方工具和参数后批准。本次尚未执行。";
        Ok(ToolCallResult::with_detail(true,speech,json!({"result":{"status":"approval_required","approval_id":record.approval_id,"speech":speech}}).to_string()))
    }
    fn prepare_record(
        &self,
        record: &ApprovalRecord,
    ) -> Result<(PreparedCall, CallerContext, Arc<AtomicBool>), ApprovalError> {
        if record.kind != ApprovalKind::ExternalTool || record.source != ApprovalSource::Voice {
            return Err(ApprovalError::Mismatch);
        }
        let request = record.request.as_ref().ok_or(ApprovalError::Mismatch)?;
        let device = request
            .get("device_id")
            .and_then(Value::as_str)
            .ok_or(ApprovalError::Mismatch)?;
        let session = request
            .get("voice_session_id")
            .and_then(Value::as_str)
            .ok_or(ApprovalError::Mismatch)?;
        if record.session_id.as_deref() != Some(device) {
            return Err(ApprovalError::Mismatch);
        }
        let ctx = CallerContext::Voice {
            device_id: device.into(),
            session_id: session.into(),
        };
        let api = self.api.upgrade().ok_or(ApprovalError::Mismatch)?;
        let permit = api.external_voice_permit(&ctx)?;
        let prepared = self
            .prepare(
                &record.target_record,
                request
                    .get("arguments")
                    .cloned()
                    .ok_or(ApprovalError::Mismatch)?,
            )
            .map_err(|_| ApprovalError::Mismatch)?;
        if prepared.permission != Permission::Ask
            || request.get("client_digest").and_then(Value::as_str)
                != Some(prepared.intent.digest.as_str())
            || record.connection.as_ref() != Some(&connection(&prepared.intent))
        {
            return Err(ApprovalError::Mismatch);
        }
        Ok((prepared, ctx, permit))
    }
}
impl Drop for ExternalMcp {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn connection(intent: &CallIntent) -> ApprovalConnection {
    ApprovalConnection::External {
        server: intent.server.clone(),
        fingerprint: intent.digest.clone(),
    }
}
fn result_text(value: &Value) -> String {
    let mut text = String::new();
    for item in value["content"].as_array().into_iter().flatten().take(64) {
        if text.chars().count() >= 1500 {
            break;
        }
        if item["type"] == "text"
            && let Some(part) = item["text"].as_str().filter(|text| !text.is_empty())
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.extend(part.chars().take(1500 - text.chars().count().min(1500)));
        }
    }
    if text.is_empty() {
        "第三方工具已返回非文本结果，请在对应服务中查看。".into()
    } else {
        text
    }
}
fn tool_result(result: Result<Value, ClientError>) -> ToolCallResult {
    match result {
        Ok(value) => ToolCallResult {
            success: value["isError"] != true,
            result: result_text(&value),
            detail: None,
        },
        Err(error) => ToolCallResult::err(error.to_string()),
    }
}
impl ExternalTools for ExternalMcp {
    fn tools(&self) -> Value {
        json!({"tools":self.manager.tools().into_iter().map(|t|json!({"name":t.name,"description":t.description,"inputSchema":t.parameters})).collect::<Vec<_>>()})
    }
    fn call(&self, context: &CallerContext, name: &str, arguments: &Value) -> ToolCallResult {
        let Ok(lock) = self.admin.lock() else {
            return ToolCallResult::err("第三方 MCP 配置不可用");
        };
        let Some(api) = self.api.upgrade() else {
            return ToolCallResult::err("MOSS 已停止");
        };
        let permit = match api.external_voice_permit(context) {
            Ok(p) => p,
            Err(e) => return ToolCallResult::err(e.message()),
        };
        let prepared = match self.prepare(name, arguments.clone()) {
            Ok(p) => p,
            Err(error) => {
                let _ = api.record_audit(&audit(
                    context,
                    AuditAction::ExternalToolDenied,
                    name,
                    None,
                    AuditResult::Denied,
                ));
                return ToolCallResult::err(error.to_string());
            }
        };
        if prepared.permission == Permission::Ask {
            return self
                .pending(context, &prepared, permit)
                .unwrap_or_else(ToolCallResult::err);
        }
        let guard = Arc::new(Guard {
            api: self.api.clone(),
            context: context.clone(),
            permit,
            alias: name.into(),
            approval: None,
        });
        drop(lock);
        tool_result(self.handle.block_on(self.manager.execute(prepared, guard)))
    }
    fn current_digest(&self, record: &ApprovalRecord) -> Result<String, ApprovalError> {
        let _lock = self.admin.lock().map_err(|_| ApprovalError::Mismatch)?;
        let (prepared, _, _) = self.prepare_record(record)?;
        Ok(approval_digest(
            record.kind,
            &record.target_record,
            record.request.as_ref().ok_or(ApprovalError::Mismatch)?,
            &connection(&prepared.intent),
        ))
    }
    fn execute(&self, record: &ApprovalRecord) -> (u16, Value) {
        let prepared = self
            .admin
            .lock()
            .map_err(|_| ApprovalError::Mismatch)
            .and_then(|_lock| self.prepare_record(record));
        let (prepared, context, permit) = match prepared {
            Ok(v) => v,
            Err(e) => {
                return (
                    e.http_status(),
                    json!({"success":false,"result":e.message(),"detail":{"error_code":e.code()}}),
                );
            }
        };
        let guard = Arc::new(Guard {
            api: self.api.clone(),
            context,
            permit,
            alias: record.target_record.clone(),
            approval: Some(record.clone()),
        });
        let result = tool_result(self.handle.block_on(self.manager.execute(prepared, guard)));
        (
            if result.success { 200 } else { 409 },
            json!({"success":result.success,"result":result.result}),
        )
    }
}

fn audit(
    context: &CallerContext,
    action: AuditAction,
    name: &str,
    intent: Option<&CallIntent>,
    result: AuditResult,
) -> AuditEntry {
    AuditEntry {
        at: now(),
        client: audit_client(context),
        action,
        target: name.chars().take(64).collect(),
        result,
        detail: AuditDetail {
            kind: Some(ApprovalKind::ExternalTool),
            tool: Some(name.chars().take(64).collect()),
            digest: intent.map(|i| i.digest.clone()),
            ..Default::default()
        },
    }
}
struct Guard {
    api: Weak<ApiService>,
    context: CallerContext,
    permit: Arc<AtomicBool>,
    alias: String,
    approval: Option<ApprovalRecord>,
}
impl DispatchGuard for Guard {
    fn is_valid(&self) -> bool {
        self.permit.load(Ordering::Acquire)
            && self
                .api
                .upgrade()
                .is_some_and(|api| !api.tool_disabled(&self.alias))
    }
    fn authorize(&self, intent: &CallIntent, permission: Permission) -> Result<(), ClientError> {
        let api = self.api.upgrade().ok_or(ClientError::Unavailable)?;
        if !self.is_valid() || api.external_voice_permit(&self.context).is_err() {
            return Err(ClientError::ApprovalFailed);
        }
        let alias = moss_mcp_client::tool_alias(&intent.server, &intent.tool)?;
        if let Some(record) = &self.approval {
            if permission != Permission::Ask || record.target_record != alias {
                return Err(ClientError::ApprovalFailed);
            }
            let request = record.request.as_ref().ok_or(ClientError::ApprovalFailed)?;
            if request.get("client_digest").and_then(Value::as_str) != Some(&intent.digest)
                || request.get("arguments") != Some(&Value::Object(intent.arguments.clone()))
            {
                return Err(ClientError::ApprovalFailed);
            }
            let digest = approval_digest(
                ApprovalKind::ExternalTool,
                &alias,
                request,
                &connection(intent),
            );
            api.approvals()
                .ok_or(ClientError::ApprovalFailed)?
                .consume(&record.approval_id, &digest)
                .map_err(|_| ClientError::ApprovalFailed)?;
        } else {
            if permission != Permission::Allow {
                return Err(ClientError::ApprovalFailed);
            }
            api.record_audit(&audit(
                &self.context,
                AuditAction::ExternalToolRequested,
                &alias,
                Some(intent),
                AuditResult::Ok,
            ))
            .map_err(|_| ClientError::ApprovalFailed)?;
        }
        Ok(())
    }
    fn completed(
        &self,
        intent: &CallIntent,
        result: &Result<Value, ClientError>,
    ) -> Result<(), ClientError> {
        let api = self.api.upgrade().ok_or(ClientError::OutcomeNotRecorded)?;
        let mut recorded = true;
        let success = matches!(result,Ok(v) if v["isError"]!=true);
        let alias = moss_mcp_client::tool_alias(&intent.server, &intent.tool).unwrap_or_default();
        if let Some(record) = &self.approval {
            if let Some(service) = api.approvals() {
                let unknown = matches!(result, Err(ClientError::OutcomeUnknown));
                let outcome = ApprovalOutcome {
                    success,
                    message: if unknown {
                        "第三方调用结果未知，不会自动重试"
                    } else if success {
                        "第三方工具执行完成"
                    } else {
                        "第三方工具未完成，请核对服务结果后再决定下一步"
                    }
                    .into(),
                    phase: Some(if unknown {
                        ApprovalPhase::Unknown
                    } else if success {
                        ApprovalPhase::Completed
                    } else {
                        ApprovalPhase::Failed
                    }),
                    error_code: if success {
                        None
                    } else {
                        Some(
                            if unknown {
                                "result_unknown"
                            } else {
                                "external_tool_failed"
                            }
                            .into(),
                        )
                    },
                    job_id: None,
                };
                if service
                    .record_outcome(&record.approval_id, outcome)
                    .is_err()
                {
                    recorded = false;
                }
            } else {
                recorded = false;
            }
        }
        if api
            .record_audit(&audit(
                &self.context,
                AuditAction::ExternalToolCompleted,
                &alias,
                Some(intent),
                if success {
                    AuditResult::Ok
                } else {
                    AuditResult::Failed
                },
            ))
            .is_err()
        {
            recorded = false;
        }
        if recorded {
            Ok(())
        } else {
            Err(ClientError::OutcomeNotRecorded)
        }
    }
}
