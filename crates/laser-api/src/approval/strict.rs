//! 严格模式 R3 授权检查（技术书 §6.2 / §6.4 规则 3；实施清单 P1-04）。

use super::*;

/// 接口失败值：HTTP 状态 + `{success, result, detail}` 包装体。
pub(crate) type HttpFailure = (u16, Value);

/// 一个 R3 动作（运动或出光）的授权检查输入。
pub(crate) struct R3Action<'a> {
    pub kind: ApprovalKind,
    pub target_record: &'a str,
    /// 参与摘要的动作要素（不含连接目标）
    pub facts: Map<String, Value>,
    /// 原始请求：非桌面来源创建审批时从中解析显式目标、读取 `request_id`
    pub request: &'a Map<String, Value>,
    /// 审批卡上显示的人类可读摘要，不含路径与凭据
    pub summary: String,
}

/// 授权检查结果。
#[derive(Debug)]
pub(crate) enum R3Authority {
    /// 严格模式关闭：不追加任何条件，行为与之前一致
    Unchecked,
    /// 桌面会话
    Desktop,
    /// 已消费的审批授权；执行必须使用其中保存的连接参数
    Approval(ApprovalRecord),
}

pub(crate) fn error_response(error: &ApprovalError) -> HttpFailure {
    (
        error.http_status(),
        crate::failure(
            error.message(),
            Some(serde_json::json!({"error_code": error.code()})),
        ),
    )
}

fn strict_blocked() -> HttpFailure {
    (
        403,
        crate::failure(
            "严格模式下，运动或出光动作需要在 MOSS 桌面端批准后才能执行",
            Some(serde_json::json!({"error_code": "strict_mode_blocked"})),
        ),
    )
}

/// 已创建审批、等待桌面批准（技术书 §5.1 响应示例）。`success=true` 让小智播报 `speech`，
/// HTTP 202 让调用方知道动作尚未执行；调用方不得据此改动记录状态。
pub(crate) fn approval_required_response(record: &ApprovalRecord) -> HttpFailure {
    (
        202,
        crate::success(serde_json::json!({
            "status": "approval_required",
            "approval_id": record.approval_id,
            "expires_at": record.expires_at,
            "summary": record.summary,
            "speech": "已提交，请在电脑上的 MOSS 里确认后开始。",
        })),
    )
}

/// 请求里的连接字段：审批单独保存解析后的连接，执行时再用它覆盖。
const CONNECTION_FIELDS: [&str; 10] = [
    "connection_mode",
    "preferred_mode",
    "port",
    "serial_port",
    "baudrate",
    "baud",
    "network_host",
    "host",
    "network_telnet_port",
    "telnet_port",
];

/// 审批保存的执行参数：去掉连接字段（单独保存）与幂等键。身份字段在 HTTP 入口已删除。
fn stored_request(request: &Map<String, Value>) -> Map<String, Value> {
    let mut stored = request.clone();
    for key in CONNECTION_FIELDS {
        stored.remove(key);
    }
    stored.remove("request_id");
    // 调用方自报的确认不构成授权（技术书 §4.3 r5 说明）；批准后执行时由桌面批准提供 confirmed=true
    stored.remove("confirmed");
    stored
}

/// 请求里是否带了 `confirmed=true`（小智模型自行填写）。
fn model_confirmed(request: &Map<String, Value>) -> bool {
    match request.get("confirmed") {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => value.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// 用审批记录里保存的连接参数覆盖请求中的连接字段（“批准的就是执行的”，技术书 §5.4）。
pub(crate) fn object_with_connection(
    object: &Map<String, Value>,
    connection: &ApprovalConnection,
) -> Map<String, Value> {
    let mut effective = object.clone();
    for key in CONNECTION_FIELDS {
        effective.remove(key);
    }
    effective.insert(
        "connection_mode".into(),
        Value::String(connection.mode().into()),
    );
    match connection {
        ApprovalConnection::External { .. }
        | ApprovalConnection::Agent { .. }
        | ApprovalConnection::Home { .. } => {}
        ApprovalConnection::Network { host, port } => {
            effective.insert("network_host".into(), Value::String(host.clone()));
            effective.insert("network_telnet_port".into(), Value::from(*port));
        }
        ApprovalConnection::Serial { port, baud_rate } => {
            effective.insert("port".into(), Value::String(port.clone()));
            effective.insert("baudrate".into(), Value::from(*baud_rate));
        }
    }
    effective
}

/// Source identity for replay protection. The legacy bridge has no authenticated per-peer id;
/// it deliberately gets no deduplication rather than sharing authorization across endpoints.
fn request_owner(ctx: &CallerContext) -> Option<String> {
    let identity = match ctx {
        CallerContext::Voice {
            device_id,
            session_id,
        } if !device_id.is_empty() && !session_id.is_empty() => {
            serde_json::json!(["voice", device_id, session_id])
        }
        CallerContext::Lan { pairing_id } if !pairing_id.is_empty() => {
            serde_json::json!(["lan", pairing_id])
        }
        CallerContext::Gateway { session_id } if !session_id.is_empty() => {
            serde_json::json!(["gateway", session_id])
        }
        _ => return None,
    };
    Some(identity.to_string())
}

impl crate::ApiService {
    /// 唯一的 R3 授权检查点（技术书 §6.4 规则 3）。
    ///
    /// - `CallerContext::Approval`：按当前要素与审批保存的连接重算摘要并单次消费，不论严格模式开关；
    /// - 严格模式关闭：`Unchecked`，不改变任何现有判定（A-40），内置语音同样如此（2026-10-07 用户决定，技术书 §5.2）；
    /// - 桌面会话：放行；
    /// - 小智桥接 / 局域网配对 / 网关 / 内置语音：解析显式目标（拿不到即 `target_required`）并创建审批，
    ///   返回 `approval_required`，动作不执行；
    /// - 本机回环无凭据：`strict_mode_blocked`。
    pub(crate) fn require_r3_authority(
        &self,
        ctx: &CallerContext,
        action: R3Action<'_>,
    ) -> Result<R3Authority, HttpFailure> {
        if let CallerContext::Approval { approval_id } = ctx {
            let service = self.approval_service()?;
            let stored = service
                .get(approval_id)
                .map_err(|error| error_response(&error))?;
            let connection = stored
                .connection
                .ok_or_else(|| error_response(&ApprovalError::Mismatch))?;
            let digest = approval_digest(
                action.kind,
                action.target_record,
                &action.facts,
                &connection,
            );
            let record = service
                .consume(approval_id, &digest)
                .map_err(|error| error_response(&error))?;
            return Ok(R3Authority::Approval(record));
        }
        if !self.strict_mode() {
            return Ok(R3Authority::Unchecked);
        }
        if matches!(ctx, CallerContext::Desktop) {
            return Ok(R3Authority::Desktop);
        }
        let Some(source) = ApprovalSource::from_caller(ctx) else {
            // 本机回环无凭据：拒绝并尽力记审计（拒绝本身不依赖审计）
            if let Some(service) = self.approvals.as_deref() {
                let entry = AuditEntry {
                    at: (service.clock)(),
                    client: laser_contracts::audit_client(ctx),
                    action: AuditAction::StrictModeBlocked,
                    target: action.target_record.to_owned(),
                    result: AuditResult::Denied,
                    detail: AuditDetail {
                        kind: Some(action.kind),
                        error_code: Some("strict_mode_blocked".into()),
                        ..AuditDetail::default()
                    },
                };
                if let Err(error) = service.audit(&entry) {
                    eprintln!("[laser-api] {}", error.message());
                }
            }
            return Err(strict_blocked());
        };
        let service = self.approval_service()?;
        let connection = resolve_approval_target_from_env(action.request)
            .map_err(|error| error_response(&error))?;
        let session_id = match ctx {
            CallerContext::Gateway { session_id } => Some(session_id.clone()),
            // 内置语音：记录发起的设备，审批卡据此显示来源设备
            CallerContext::Voice { device_id, .. } => Some(device_id.chars().take(128).collect()),
            _ => None,
        };
        let request_id = action
            .request
            .get("request_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let record = service
            .create(NewApproval {
                owner: request_owner(ctx),
                voice_permit: self.voice_permit(ctx)?,
                kind: action.kind,
                target_record: action.target_record.to_owned(),
                source,
                session_id,
                request_id,
                summary: action.summary,
                facts: action.facts,
                connection,
                request: stored_request(action.request),
                client: laser_contracts::audit_client(ctx),
                removed_fields: if model_confirmed(action.request) {
                    vec!["confirmed".into()]
                } else {
                    Vec::new()
                },
            })
            .map_err(|error| error_response(&error))?;
        Err(approval_required_response(&record))
    }

    /// 记一条审计（R4 拒绝等由接入点发生的事件）。没有本地数据库时返回错误，调用方自行决定是否忽略。
    pub fn record_audit(&self, entry: &AuditEntry) -> Result<(), String> {
        match self.approvals.as_deref() {
            Some(service) => service.audit(entry).map_err(|error| error.message()),
            None => Err("未启用本地数据库，无法写审计".into()),
        }
    }

    pub(super) fn approval_service(&self) -> Result<&ApprovalService, HttpFailure> {
        self.approvals.as_deref().ok_or_else(|| {
            error_response(&ApprovalError::Unavailable(
                "未启用本地数据库，无法使用桌面审批".into(),
            ))
        })
    }

    /// 经审批执行后记录结果：成功写入作业 ID，失败记 failed（不可重用）。审批记录写入失败不影响已发生的执行结果。
    pub(crate) fn finish_r3_authority(
        &self,
        authority: &R3Authority,
        result: &Result<Value, HttpFailure>,
        job_id: Option<&str>,
    ) {
        let R3Authority::Approval(record) = authority else {
            return;
        };
        let Some(service) = self.approvals.as_deref() else {
            return;
        };
        let outcome = match result {
            Ok(_) => ApprovalOutcome {
                success: true,
                message: "已执行".into(),
                job_id: job_id.map(str::to_owned),
                error_code: None,
                phase: Some(ApprovalPhase::Completed),
            },
            Err((status, body)) => ApprovalOutcome {
                success: false,
                message: body
                    .get("result")
                    .and_then(Value::as_str)
                    .map(|text| text.chars().take(200).collect())
                    .unwrap_or_else(|| format!("执行失败 (HTTP {status})")),
                job_id: None,
                error_code: body
                    .get("detail")
                    .and_then(|detail| detail.get("error_code"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                phase: Some(ApprovalPhase::Failed),
            },
        };
        if let Err(error) = service.record_outcome(&record.approval_id, outcome) {
            eprintln!(
                "[laser-api] 审批 {} 记录执行结果失败: {}",
                record.approval_id,
                error.message()
            );
        }
    }
}
