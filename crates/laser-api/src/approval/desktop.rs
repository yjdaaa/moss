//! 桌面会话与桌面审批接口（技术书 §5.3；实施清单 P1-07）。

use super::*;

/// 桌面会话请求头。只在回环连接上有效；值由 Tauri 命令 `get_desktop_session` 交给桌面页，只存在内存。
pub const DESKTOP_SESSION_HEADER: &str = "x-moss-desktop-session";

/// 长度无关的逐字节比较，避免按前缀逐位猜测会话值。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn desktop_session_invalid() -> Vec<u8> {
    crate::http_json(
        403,
        crate::failure(
            "桌面会话无效，请重新打开 MOSS 桌面端",
            Some(serde_json::json!({"error_code": "desktop_session_invalid"})),
        ),
    )
}

/// 审批卡上显示的目标设备：只给连接方式与串口名，网络设备不显示地址（技术书 §9.5.2）。
fn target_view(connection: Option<&ApprovalConnection>) -> Value {
    match connection {
        Some(ApprovalConnection::Home { .. }) => {
            serde_json::json!({"mode":"home","label":"Home Assistant · 需独立桌面审批"})
        }
        Some(ApprovalConnection::Agent { provider, .. }) => {
            serde_json::json!({"mode":"agent","label":provider})
        }
        Some(ApprovalConnection::External { server, .. }) => {
            serde_json::json!({"mode":"external","label":server})
        }
        Some(ApprovalConnection::Serial { port, .. }) => {
            serde_json::json!({"mode": "serial", "label": port})
        }
        Some(ApprovalConnection::Network { .. }) => {
            serde_json::json!({"mode": "network", "label": "网络设备"})
        }
        None => serde_json::json!({"mode": "unknown", "label": "未知设备"}),
    }
}

pub(crate) fn approval_view(record: &ApprovalRecord) -> Value {
    let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.insert("target".into(), target_view(record.connection.as_ref()));
        if record.kind == ApprovalKind::AgentRun
            && let Some(request) = &record.request
        {
            let workspace = request
                .get("workspace")
                .and_then(Value::as_str)
                .unwrap_or("");
            let folder = std::path::Path::new(workspace)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("未提供");
            map.insert(
                "agent".into(),
                serde_json::json!({
                    "provider":request.get("provider").and_then(Value::as_str).unwrap_or(""),
                    "task":request.get("task").and_then(Value::as_str).unwrap_or(""),
                    "workspace_label":folder,
                    "version":request.get("version").and_then(Value::as_str).unwrap_or(""),
                    "permission":"workspace_write",
                    "uses_installed_cli_account":true,
                    "may_incur_cost":true,
                }),
            );
        }
        if record.kind == ApprovalKind::ExternalTool
            && let Some(request) = &record.request
        {
            map.insert("external".into(), serde_json::json!({"tool":record.target_record,"arguments":crate::external_tools::parameter_preview(request.get("arguments").unwrap_or(&Value::Null))}));
        }
    }
    value
}

/// 安全动作的摘要要素（路由创建审批与批准时重算共用）。
pub(crate) fn safe_action_digest_facts(object: &Map<String, Value>) -> Map<String, Value> {
    let mut facts = Map::new();
    let action = object
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    facts.insert("action".into(), Value::String(action));
    // job_id（2026-11-18）：继续走边框绑定到具体作业，批准的就是执行的那一次；
    // override_job_id（2026-11-27）：实时倍率提高绑定到原加工任务，任务结束后批准不再作用于后续任务
    for key in [
        "x",
        "y",
        "z",
        "feed_rate",
        "power",
        "job_id",
        "override_job_id",
    ] {
        if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
            facts.insert(key.into(), value.clone());
        }
    }
    facts
}

/// 走边框的摘要要素（路由创建审批与批准时重算共用）。
pub(crate) fn framing_digest_facts(object: &Map<String, Value>) -> Map<String, Value> {
    let mut facts = Map::new();
    // relative_to_head（2026-11-06）改变走边框的参照点，必须进入摘要；未带该字段的请求摘要不变
    for key in [
        "min_x",
        "min_y",
        "max_x",
        "max_y",
        "feed_rate",
        "power",
        "relative_to_head",
        // 2026-11-16 橡皮筋走边框：实际走的多边形进入摘要，批准的就是要走的
        "outline",
    ] {
        if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
            facts.insert(key.into(), value.clone());
        }
    }
    facts
}

impl crate::ApiService {
    /// 开启桌面会话并返回会话值（Tauri 桌面壳启动时调用一次；重复调用返回同一个值）。
    /// 独立 `moss-host serve` 不调用，因此没有桌面会话，严格模式下无法批准，只能拒绝或过期。
    pub fn enable_desktop_session(&self) -> Result<String, String> {
        if let Some(existing) = self.desktop_session.get() {
            return Ok(existing.clone());
        }
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|error| error.to_string())?;
        let _ = self.desktop_session.set(hex::encode(random));
        self.desktop_session
            .get()
            .cloned()
            .ok_or_else(|| "桌面会话初始化失败".to_string())
    }

    /// 校验请求头中的桌面会话：没有该头返回 `Ok(false)`；有且有效（仅回环）返回 `Ok(true)`；
    /// 有但无效返回 403 `desktop_session_invalid`，前端据此重新获取会话并只重试一次。
    pub(crate) fn check_desktop_session(
        &self,
        head: &str,
        loopback: bool,
    ) -> Result<bool, Vec<u8>> {
        let Some(value) = crate::header_value(head, DESKTOP_SESSION_HEADER) else {
            return Ok(false);
        };
        let Some(expected) = self.desktop_session.get() else {
            return Err(desktop_session_invalid());
        };
        if loopback && constant_time_eq(value.trim().as_bytes(), expected.as_bytes()) {
            Ok(true)
        } else {
            Err(desktop_session_invalid())
        }
    }

    /// 仅供内置语音副屏展示的只读待电脑审批无敏摘要（不含 approval_id、digest、路径或凭据）。
    pub fn pending_approval_display_summary(&self) -> Option<serde_json::Value> {
        let service = self.approval_service().ok()?;
        let now = service.now();
        let records = service.list(Some(ApprovalStatus::Pending), 5).ok()?;
        let rec = records.into_iter().find(|r| !r.is_expired_at(now))?;
        let rem = (rec.expires_at - now).ceil().max(1.0) as u64;
        let total = (rec.expires_at - rec.created_at).round().clamp(30.0, 600.0) as u64;
        Some(serde_json::json!({
            "kind": rec.kind.as_str(),
            "summary": rec.summary,
            "remaining_secs": rem,
            "total_secs": total,
        }))
    }

    /// `/v1/approvals`（列表）、`/v1/approvals/{id}/approve`、`/v1/approvals/{id}/reject`，仅桌面会话。
    pub(crate) fn handle_approval_route(
        &self,
        method: &str,
        route: &str,
        query: &str,
        body: &[u8],
        ctx: &CallerContext,
    ) -> Vec<u8> {
        if !matches!(ctx, CallerContext::Desktop) {
            return crate::http_json(
                403,
                crate::failure(
                    "审批只能在 MOSS 桌面端操作",
                    Some(serde_json::json!({"error_code": "desktop_session_required"})),
                ),
            );
        }
        let service = match self.approval_service() {
            Ok(service) => service,
            Err((status, body)) => return crate::http_json(status, body),
        };
        if method == "GET" && route == "/v1/approvals" {
            let status = match crate::query_param(query, "status") {
                None => None,
                Some(raw) if raw.trim().is_empty() || raw == "all" => None,
                Some(raw) => match ApprovalStatus::parse(raw.trim()) {
                    Some(status) => Some(status),
                    None => {
                        return crate::http_json(
                            400,
                            crate::failure(format!("未知的审批状态: {raw}"), None),
                        );
                    }
                },
            };
            const LIST_LIMIT: usize = 50;
            return match service.list(status, LIST_LIMIT) {
                Ok(records) => crate::http_json(
                    200,
                    crate::success(serde_json::json!({
                        "approvals": records.iter().map(approval_view).collect::<Vec<_>>(),
                        // 最多返回最近 50 条；界面据此说“最近 N 条”，不把它当总数（2026-11-07 n68）
                        "limit": LIST_LIMIT,
                        "server_time": (service.clock)(),
                        "ttl_seconds": service.ttl_seconds(),
                    })),
                ),
                Err(error) => {
                    let (status, body) = error_response(&error);
                    crate::http_json(status, body)
                }
            };
        }
        let Some(rest) = route.strip_prefix("/v1/approvals/") else {
            return crate::http_json(404, crate::failure("未找到路径", None));
        };
        let (approval_id, action) = match rest.split_once('/') {
            Some((id, action)) if !id.is_empty() => (id, action),
            _ => return crate::http_json(404, crate::failure("未找到路径", None)),
        };
        if method != "POST" {
            return crate::http_json(404, crate::failure("未找到路径", None));
        }
        let object = if body.is_empty() {
            Map::new()
        } else {
            match crate::parse_json_object(body, 16 * 1024) {
                Ok(object) => object,
                Err(error) => return crate::http_json(400, crate::failure(error, None)),
            }
        };
        match action {
            "reject" => {
                let reason = object.get("reason").and_then(Value::as_str);
                match service.reject(approval_id, reason) {
                    Ok(record) => crate::http_json(200, crate::success(approval_view(&record))),
                    Err(error) => {
                        let (status, body) = error_response(&error);
                        crate::http_json(status, body)
                    }
                }
            }
            "approve" => {
                let Some(submitted) = object
                    .get("digest")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                else {
                    return crate::http_json(
                        400,
                        crate::failure("批准需要提交审批卡上的摘要 digest", None),
                    );
                };
                self.approve_and_execute(service, approval_id, submitted)
            }
            _ => crate::http_json(404, crate::failure("未找到路径", None)),
        }
    }

    /// 批准并立即以审批身份执行（技术书 §4.3 第 6 步）。批准前按当前记录重算摘要；
    /// 执行走现有路由与全部门禁，由执行入口单次消费授权并写回结果。
    fn approve_and_execute(
        &self,
        service: &ApprovalService,
        approval_id: &str,
        submitted: &str,
    ) -> Vec<u8> {
        let stored = match service.get(approval_id) {
            Ok(record) => record,
            Err(error) => {
                let (status, body) = error_response(&error);
                return crate::http_json(status, body);
            }
        };
        // 记录已删除、刀路不可用等都会让摘要算不出来，按“不一致”处理
        let current = self.current_approval_digest(&stored).unwrap_or_default();
        let approved = match service.approve(approval_id, submitted, &current) {
            Ok(record) => record,
            Err(error) => {
                let (status, mut body) = error_response(&error);
                if let Ok(latest) = service.get(approval_id) {
                    body["detail"]["approval"] = approval_view(&latest);
                }
                return crate::http_json(status, body);
            }
        };
        let (status, response) = self.execute_approval(&approved);
        let latest = service
            .get(approval_id)
            .map(|record| approval_view(&record))
            .unwrap_or(Value::Null);
        let succeeded =
            status < 300 && response.get("success").and_then(Value::as_bool) == Some(true);
        if succeeded {
            crate::http_json(
                200,
                crate::success(serde_json::json!({
                    "approval": latest,
                    "result": response.get("result").cloned().unwrap_or(Value::Null),
                })),
            )
        } else {
            let message = response
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("执行失败 (HTTP {status})"));
            let error_code = response
                .get("detail")
                .and_then(|detail| {
                    detail.get("error_code").or_else(|| {
                        detail
                            .as_str()
                            .filter(|s| {
                                !s.is_empty()
                                    && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                            })
                            .map(|_| detail)
                    })
                })
                .cloned()
                .unwrap_or(Value::Null);
            let status = if status < 400 { 500 } else { status };
            crate::http_json(
                status,
                crate::failure(
                    message,
                    Some(serde_json::json!({"error_code": error_code, "approval": latest})),
                ),
            )
        }
    }

    /// 按当前记录、审批保存的执行参数与连接重算摘要（批准时，技术书 §5.4）。
    pub(crate) fn current_approval_digest(
        &self,
        record: &ApprovalRecord,
    ) -> Result<String, ApprovalError> {
        // No general HTTP replay path can authorize HomeControl. Only a
        // dedicated trusted HA module may re-evaluate its policy revision.
        if record.kind == ApprovalKind::HomeControl {
            return Err(ApprovalError::Mismatch);
        }
        if record.kind == ApprovalKind::AgentRun {
            return self
                .agent_runs
                .get()
                .ok_or(ApprovalError::Mismatch)?
                .current_digest(record);
        }
        if record.kind == ApprovalKind::ExternalTool {
            return self
                .external_tools
                .get()
                .ok_or(ApprovalError::Mismatch)?
                .current_digest(record);
        }
        if matches!(record.connection, Some(ApprovalConnection::External { .. })) {
            return Err(ApprovalError::Mismatch);
        }
        let connection = record.connection.as_ref().ok_or(ApprovalError::Mismatch)?;
        let empty = Map::new();
        let request = record.request.as_ref().unwrap_or(&empty);
        let facts = match record.kind {
            ApprovalKind::WorkflowConfirmSend | ApprovalKind::CalibrationSend => {
                let kind = if record.kind == ApprovalKind::CalibrationSend {
                    laser_runtime::RecordKind::Calibration
                } else {
                    laser_runtime::RecordKind::Workflow
                };
                let payload = self
                    .load_record_payload(kind, &record.target_record)
                    .ok_or(ApprovalError::Mismatch)?;
                let roots = self.send_gcode_roots();
                let (_, gcode_sha, _) = crate::build_send_preflight(&payload, &roots)
                    .map_err(|_| ApprovalError::Mismatch)?;
                self.send_digest_facts(kind, &gcode_sha, &record.target_record, request)
            }
            ApprovalKind::SafeAction => safe_action_digest_facts(request),
            ApprovalKind::Framing => framing_digest_facts(request),
            ApprovalKind::ExternalTool | ApprovalKind::AgentRun | ApprovalKind::HomeControl => {
                return Err(ApprovalError::Mismatch);
            }
        };
        Ok(approval_digest(
            record.kind,
            &record.target_record,
            &facts,
            connection,
        ))
    }

    /// 以 `CallerContext::Approval` 重放原始请求：路由由类别决定，连接参数用审批保存的值，
    /// `confirmed=true` 由桌面批准提供。返回（HTTP 状态，响应体）。
    fn execute_approval(&self, record: &ApprovalRecord) -> (u16, Value) {
        if record.kind == ApprovalKind::AgentRun {
            return self
                .agent_runs
                .get()
                .map(|runner| runner.execute(record))
                .unwrap_or_else(|| error_response(&ApprovalError::Mismatch));
        }
        if record.kind == ApprovalKind::ExternalTool {
            return self
                .external_tools
                .get()
                .map(|tools| tools.execute(record))
                .unwrap_or_else(|| error_response(&ApprovalError::Mismatch));
        }
        if matches!(record.connection, Some(ApprovalConnection::External { .. })) {
            return error_response(&ApprovalError::Mismatch);
        }
        let Some(connection) = record.connection.as_ref() else {
            return error_response(&ApprovalError::Mismatch);
        };
        let mut body =
            object_with_connection(&record.request.clone().unwrap_or_default(), connection);
        body.insert("confirmed".into(), Value::Bool(true));
        let route = match record.kind {
            ApprovalKind::WorkflowConfirmSend => {
                let action = body.get("action").and_then(Value::as_str).unwrap_or("");
                if !matches!(action, "confirm_send" | "send" | "resume") {
                    body.insert("action".into(), Value::String("confirm_send".into()));
                }
                body.insert(
                    "workflow_id".into(),
                    Value::String(record.target_record.clone()),
                );
                "/api/workflow"
            }
            ApprovalKind::CalibrationSend => {
                body.insert(
                    "calibration_id".into(),
                    Value::String(record.target_record.clone()),
                );
                "/api/material-lab/send"
            }
            ApprovalKind::SafeAction => "/api/safe-action",
            ApprovalKind::Framing => "/api/device/framing",
            ApprovalKind::ExternalTool | ApprovalKind::AgentRun | ApprovalKind::HomeControl => {
                return error_response(&ApprovalError::Mismatch);
            }
        };
        self.dispatch_json(
            "POST",
            route,
            &Value::Object(body),
            CallerContext::Approval {
                approval_id: record.approval_id.clone(),
            },
        )
    }
}
