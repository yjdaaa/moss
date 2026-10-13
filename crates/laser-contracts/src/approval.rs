//! 桌面审批与审计的强类型（技术书 §5.3–§5.5、§7.1、§7.3；实施清单 P1-01）。
//!
//! 这里只定义数据结构与状态规则。摘要计算、存储、过期与单次消费分别在
//! `laser-runtime`（schema v4）与 `laser-api`（审批服务，P1-03）中实现。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::CallerContext;

/// 审批 ID 前缀（`apr_` + 随机）。
pub const APPROVAL_ID_PREFIX: &str = "apr_";
/// 授权默认有效期（秒），对应 `MOSS_APPROVAL_TTL_SECONDS`（§5.5）。
pub const APPROVAL_TTL_DEFAULT_SECONDS: u64 = 120;
/// 授权有效期允许的最小值（秒）。
pub const APPROVAL_TTL_MIN_SECONDS: u64 = 30;
/// 授权有效期允许的最大值（秒）。
pub const APPROVAL_TTL_MAX_SECONDS: u64 = 600;

/// 把配置值限制在 30..600 秒；未配置或无法解析时用默认 120 秒。
pub fn approval_ttl_seconds(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .map(|value| value.clamp(APPROVAL_TTL_MIN_SECONDS, APPROVAL_TTL_MAX_SECONDS))
        .unwrap_or(APPROVAL_TTL_DEFAULT_SECONDS)
}

/// `laser_safe_action_tool` / `/api/safe-action` 的动作是否属于 R3（运动、解锁、提高功率或速度，技术书 §6.1）。
/// 只有明确的降低风险或只读动作返回 `false`：查询状态、急停、软复位、关激光、倍率复位与降低。
/// 其他一律按 R3 处理（含未来新增的动作），严格模式下需要桌面授权。
pub fn safe_action_requires_approval(action: &str) -> bool {
    let action = action.trim().to_ascii_lowercase();
    let lowers_risk = matches!(action.as_str(), "status" | "emergency_stop" | "soft_reset" | "laser_off")
        || action.ends_with("_reset")
        || action.ends_with("_100")
        || action.contains("minus")
        || action.contains("_-");
    !lowers_risk
}

/// 审批状态（§5.2）：`pending | approved | rejected | expired | executed | failed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
    Executed,
    Failed,
}

impl ApprovalStatus {
    pub const ALL: [Self; 6] = [
        Self::Pending,
        Self::Approved,
        Self::Rejected,
        Self::Expired,
        Self::Executed,
        Self::Failed,
    ];

    /// 数据库 `status` 列与接口中使用的稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Executed => "executed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == value)
    }

    /// 仍可能导致执行的状态；进程重启时一律改为 `expired`（§5.5）。
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Approved)
    }

    /// 允许的状态迁移。没有任何状态能回到 `pending` 或 `approved`，
    /// 因此一次授权最多执行一次；执行失败记 `failed`，不可重用（§5.5）。
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Approved)
                | (Self::Pending, Self::Rejected)
                | (Self::Pending, Self::Expired)
                | (Self::Approved, Self::Executed)
                | (Self::Approved, Self::Expired)
                | (Self::Approved, Self::Failed)
                | (Self::Executed, Self::Failed)
        )
    }
}

/// 需要审批的动作类别（§7.1 `kind` 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    /// NV13: opt-in, separately approved read-only Claude/Codex task (never ExternalTool).
    /// NV15: independently authorized home service intent. Not an external
    /// tool, laser command, or generic confirmed action.
    HomeControl,
    AgentRun,
    ExternalTool,
    WorkflowConfirmSend,
    CalibrationSend,
    SafeAction,
    Framing,
}

impl ApprovalKind {
    pub const ALL: [Self; 7] = [
        Self::AgentRun,
        Self::HomeControl,
        Self::ExternalTool,
        Self::WorkflowConfirmSend,
        Self::CalibrationSend,
        Self::SafeAction,
        Self::Framing,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::HomeControl => "home_control",
            Self::AgentRun => "agent_run",
            Self::ExternalTool => "external_tool",
            Self::WorkflowConfirmSend => "workflow_confirm_send",
            Self::CalibrationSend => "calibration_send",
            Self::SafeAction => "safe_action",
            Self::Framing => "framing",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// 发起审批的一方（§7.1 `source` 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    Gateway,
    McpBridge,
    Lan,
    /// 内置语音服务（NV4）
    Voice,
}

impl ApprovalSource {
    pub const ALL: [Self; 4] = [Self::Gateway, Self::McpBridge, Self::Lan, Self::Voice];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::McpBridge => "mcp_bridge",
            Self::Lan => "lan",
            Self::Voice => "voice",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.as_str() == value)
    }

    /// 由调用方上下文得到审批来源。桌面会话本身就是批准方，审批执行与本机回环
    /// 不发起审批，返回 `None`（回环在严格模式下如何处理由 P1-04 决定）。
    pub fn from_caller(ctx: &CallerContext) -> Option<Self> {
        match ctx {
            CallerContext::Gateway { .. } => Some(Self::Gateway),
            CallerContext::McpBridge => Some(Self::McpBridge),
            CallerContext::Lan { .. } => Some(Self::Lan),
            CallerContext::Voice { .. } => Some(Self::Voice),
            CallerContext::Desktop | CallerContext::Approval { .. } | CallerContext::Loopback => None,
        }
    }
}

/// 创建审批时解析出的显式连接参数（§7.1 `connection` 列）。执行时直接使用，不重新解析，
/// 保证“批准的就是执行的”。只在本地库保存，接口与摘要不返回明文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ApprovalConnection {
    /// Opaque client/credential/configuration fingerprint, never an executable or URL.
    /// Read-only CLI execution intent; fingerprint binds provider, executable, workspace and policy.
    /// Opaque HA configuration/credential/allowlist fingerprint. No URL,
    /// token or entity action is disclosed through the approval response.
    Home { fingerprint: String },
    Agent { provider: String, fingerprint: String },
    External { server: String, fingerprint: String },
    Network { host: String, port: u16 },
    Serial { port: String, baud_rate: u32 },
}

impl ApprovalConnection {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Home { .. } => "home",
            Self::Agent { .. } => "agent",
            Self::External { .. } => "external",
            Self::Network { .. } => "network",
            Self::Serial { .. } => "serial",
        }
    }

    /// 与 `laser_device::ConnectionSpec::describe()` 同一格式；摘要里只放它的哈希（§5.4）。
    pub fn describe(&self) -> String {
        match self {
            Self::Agent { provider, fingerprint } => format!("agent:{provider}:{fingerprint}"),
            Self::Home { fingerprint } => format!("home:{fingerprint}"),
            Self::External { server, fingerprint } => format!("external:{server}:{fingerprint}"),
            Self::Network { host, port } => format!("{host}:{port}"),
            Self::Serial { port, baud_rate } => format!("{port}@{baud_rate}"),
        }
    }
}

/// 经审批执行的作业进展（技术书 §5.5、验收 A-33）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPhase {
    /// 已开始向设备发送，尚未结束
    Running,
    Completed,
    Failed,
    Cancelled,
    /// 进程在发送过程中中断，设备上实际执行到哪里无法确认；不会自动重发
    Unknown,
}

/// 执行结果摘要（§7.1 `outcome` 列）。不含文件路径与连接地址。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalOutcome {
    pub success: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// 作业进展；v5 之前写入的结果没有该字段
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<ApprovalPhase>,
}

/// 一条审批记录（§7.1）。序列化结果就是桌面审批接口返回的内容，
/// 因此 `connection` 不参与序列化，由存储层单独读写。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalRecord {
    pub approval_id: String,
    pub status: ApprovalStatus,
    pub kind: ApprovalKind,
    /// workflow_id / calibration_id
    pub target_record: String,
    /// §5.4 摘要（SHA-256 十六进制）
    pub digest: String,
    /// 人类可读摘要，不含路径和凭据
    pub summary: String,
    pub source: ApprovalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Scoped replay key: scope-v1 hash of authenticated caller and original id; legacy rows remain readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip)]
    pub connection: Option<ApprovalConnection>,
    /// 原始执行参数（已去掉连接字段与身份字段，schema v5 `request` 列）。批准后据此重放执行，
    /// 并据此重算摘要；同样只在本地库保存，不参与序列化。
    #[serde(skip)]
    pub request: Option<serde_json::Map<String, serde_json::Value>>,
    pub created_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<f64>,
    pub expires_at: f64,
    pub updated_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ApprovalOutcome>,
}

impl ApprovalRecord {
    /// 仍处于可执行状态但已过有效期。
    pub fn is_expired_at(&self, now: f64) -> bool {
        self.status.is_open() && now >= self.expires_at
    }
}

/// 审计动作（§7.3 写入时机）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    ExternalToolRequested,
    ExternalToolCompleted,
    ExternalToolDenied,
    ExternalMcpConfigChanged,
    /// 内置语音发出的急停请求；结果表示请求写入，不能证明物理制动。
    VoiceStopRequested,
    ApprovalCreated,
    ApprovalApproved,
    ApprovalRejected,
    ApprovalExpired,
    ApprovalExecuted,
    ApprovalFailed,
    /// 网关请求里的 `confirmed` 字段被删除
    ConfirmedFieldStripped,
    /// R4 工具被拒绝
    R4Denied,
    /// 严格模式拦截了缺少授权的 R3 动作
    StrictModeBlocked,
}

impl AuditAction {
    pub const ALL: [Self; 14] = [
        Self::ExternalToolRequested,
        Self::ExternalToolCompleted,
        Self::ExternalToolDenied,
        Self::ExternalMcpConfigChanged,
        Self::VoiceStopRequested,
        Self::ApprovalCreated,
        Self::ApprovalApproved,
        Self::ApprovalRejected,
        Self::ApprovalExpired,
        Self::ApprovalExecuted,
        Self::ApprovalFailed,
        Self::ConfirmedFieldStripped,
        Self::R4Denied,
        Self::StrictModeBlocked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalToolRequested => "external_tool_requested",
            Self::ExternalToolCompleted => "external_tool_completed",
            Self::ExternalToolDenied => "external_tool_denied",
            Self::ExternalMcpConfigChanged => "external_mcp_config_changed",
            Self::VoiceStopRequested => "voice_stop_requested",
            Self::ApprovalCreated => "approval_created",
            Self::ApprovalApproved => "approval_approved",
            Self::ApprovalRejected => "approval_rejected",
            Self::ApprovalExpired => "approval_expired",
            Self::ApprovalExecuted => "approval_executed",
            Self::ApprovalFailed => "approval_failed",
            Self::ConfirmedFieldStripped => "confirmed_field_stripped",
            Self::R4Denied => "r4_denied",
            Self::StrictModeBlocked => "strict_mode_blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.as_str() == value)
    }
}

/// 审计结果（`audit_log.result` 列）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditResult {
    Ok,
    Denied,
    Failed,
}

impl AuditResult {
    pub const ALL: [Self; 3] = [Self::Ok, Self::Denied, Self::Failed];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Denied => "denied",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|result| result.as_str() == value)
    }
}

/// `audit_log.detail` 列内容（§7.3）。只放标识与原因；不放凭据、令牌、连接地址或文件路径。
/// 工具层只构造这个类型，不拼裸 JSON。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ApprovalKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 从请求中删除的字段名（如 `confirmed`）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_fields: Vec<String>,
}

/// 一行审计记录（`audit_log`，§7.3；表结构见 db-schema.md §3.11）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AuditEntry {
    pub at: f64,
    /// `desktop` / `gateway` / `mcp` / 配对 ID / `local`
    pub client: String,
    pub action: AuditAction,
    /// workflow_id / calibration_id / approval_id 等
    pub target: String,
    pub result: AuditResult,
    pub detail: AuditDetail,
}

/// 审计 `client` 列取值（§7.3）：桌面会话与经桌面批准的执行记 `desktop`，
/// 网关 `gateway`，小智桥接 `mcp`，局域网记配对 ID，本机回环记 `local`。
pub fn audit_client(ctx: &CallerContext) -> String {
    match ctx {
        CallerContext::Desktop | CallerContext::Approval { .. } => "desktop".to_owned(),
        CallerContext::Gateway { .. } => "gateway".to_owned(),
        CallerContext::McpBridge => "mcp".to_owned(),
        CallerContext::Lan { pairing_id } if !pairing_id.is_empty() => pairing_id.clone(),
        CallerContext::Lan { .. } => "lan".to_owned(),
        CallerContext::Voice { device_id, .. } => format!("voice:{}", device_id.chars().take(64).collect::<String>()),
        CallerContext::Loopback => "local".to_owned(),
    }
}

pub fn approval_record_json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ApprovalRecord)).expect("approval record schema must serialize")
}

pub fn audit_entry_json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(AuditEntry)).expect("audit entry schema must serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> ApprovalRecord {
        ApprovalRecord {
            approval_id: format!("{APPROVAL_ID_PREFIX}test1"),
            status: ApprovalStatus::Pending,
            kind: ApprovalKind::WorkflowConfirmSend,
            target_record: "wf_placeholder".into(),
            digest: "0".repeat(64),
            summary: "椴木板 3mm，功率 30%，速度 1500".into(),
            source: ApprovalSource::McpBridge,
            session_id: None,
            request_id: Some("req-1".into()),
            connection: Some(ApprovalConnection::Network { host: "192.0.2.10".into(), port: 23 }),
            request: Some(serde_json::json!({"send_mode": "stream"}).as_object().cloned().unwrap()),
            created_at: 100.0,
            decided_at: None,
            expires_at: 220.0,
            updated_at: 100.0,
            outcome: None,
        }
    }

    #[test]
    fn safe_actions_are_graded_fail_closed() {
        for action in [
            "status", "emergency_stop", "soft_reset", "laser_off", "feed_override_reset", "feed_100",
            "power_reset", "spindle_override_minus_10", "feed_minus_1", "power_-10%", "feed_-1%",
        ] {
            assert!(!safe_action_requires_approval(action), "{action} 应为 R0/R2");
        }
        for action in [
            "unlock", "home", "set_origin", "absolute_move", "relative_move", "feed_override_plus_10",
            "power_plus_1", "power_+10%", "feed_+1%", "spindle_override_plus_10", "some_future_action",
        ] {
            assert!(safe_action_requires_approval(action), "{action} 应为 R3");
        }
    }

    #[test]
    fn status_kind_source_strings_are_stable_and_match_serde() {
        for status in ApprovalStatus::ALL {
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
            assert_eq!(ApprovalStatus::parse(status.as_str()), Some(status));
        }
        for kind in ApprovalKind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
            assert_eq!(ApprovalKind::parse(kind.as_str()), Some(kind));
        }
        for source in ApprovalSource::ALL {
            assert_eq!(serde_json::to_value(source).unwrap(), source.as_str());
            assert_eq!(ApprovalSource::parse(source.as_str()), Some(source));
        }
        for action in AuditAction::ALL {
            assert_eq!(serde_json::to_value(action).unwrap(), action.as_str());
            assert_eq!(AuditAction::parse(action.as_str()), Some(action));
        }
        for result in AuditResult::ALL {
            assert_eq!(serde_json::to_value(result).unwrap(), result.as_str());
            assert_eq!(AuditResult::parse(result.as_str()), Some(result));
        }
        assert_eq!(ApprovalStatus::parse("PENDING"), None);
        assert_eq!(ApprovalKind::parse(""), None);
    }

    #[test]
    fn approval_can_be_consumed_at_most_once() {
        use super::ApprovalStatus as S;
        let allowed = [
            (S::Pending, S::Approved),
            (S::Pending, S::Rejected),
            (S::Pending, S::Expired),
            (S::Approved, S::Executed),
            (S::Approved, S::Expired),
            (S::Approved, S::Failed),
            (S::Executed, S::Failed),
        ];
        for from in S::ALL {
            for to in S::ALL {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "{} -> {}",
                    from.as_str(),
                    to.as_str()
                );
            }
        }
        // 任何状态都回不到 pending / approved，所以授权不能被重复使用
        for from in S::ALL {
            assert!(!from.can_transition_to(S::Pending));
            assert!(from == S::Pending || !from.can_transition_to(S::Approved));
        }
        assert!(S::Pending.is_open() && S::Approved.is_open());
        assert!(!S::Executed.is_open() && !S::Failed.is_open());
    }

    #[test]
    fn record_json_never_contains_connection() {
        let record = sample_record();
        let text = serde_json::to_string(&record).unwrap();
        assert!(!text.contains("connection"), "{text}");
        assert!(!text.contains("192.0.2.10"), "{text}");
        assert!(!text.contains("send_mode"), "{text}");
        let back: ApprovalRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(back.connection, None);
        assert_eq!(back.request, None);
        assert_eq!(ApprovalRecord { connection: None, request: None, ..record }, back);

        let schema = approval_record_json_schema();
        assert_eq!(schema["title"], "ApprovalRecord");
        assert!(schema["properties"].get("connection").is_none());
        assert!(schema["properties"].get("request").is_none());
        assert!(schema["properties"].get("digest").is_some());
    }

    #[test]
    fn connection_round_trips_for_storage_and_describes_like_device_spec() {
        let network = ApprovalConnection::Network { host: "192.0.2.10".into(), port: 23 };
        let serial = ApprovalConnection::Serial { port: "COMX".into(), baud_rate: 115200 };
        assert_eq!(network.describe(), "192.0.2.10:23");
        assert_eq!(serial.describe(), "COMX@115200");
        assert_eq!(network.mode(), "network");
        assert_eq!(serial.mode(), "serial");
        for connection in [network, serial] {
            let value = serde_json::to_value(&connection).unwrap();
            assert_eq!(value["mode"], connection.mode());
            assert_eq!(serde_json::from_value::<ApprovalConnection>(value).unwrap(), connection);
        }
    }

    #[test]
    fn expiry_only_applies_to_open_records() {
        let mut record = sample_record();
        assert!(!record.is_expired_at(219.9));
        assert!(record.is_expired_at(220.0));
        record.status = ApprovalStatus::Executed;
        assert!(!record.is_expired_at(1000.0));
    }

    #[test]
    fn ttl_is_clamped_to_allowed_range() {
        assert_eq!(approval_ttl_seconds(None), 120);
        assert_eq!(approval_ttl_seconds(Some("abc")), 120);
        assert_eq!(approval_ttl_seconds(Some(" 90 ")), 90);
        assert_eq!(approval_ttl_seconds(Some("10")), 30);
        assert_eq!(approval_ttl_seconds(Some("9999")), 600);
    }

    #[test]
    fn audit_detail_is_typed_and_compact() {
        assert_eq!(serde_json::to_string(&AuditDetail::default()).unwrap(), "{}");
        let detail = AuditDetail {
            approval_id: Some("apr_test1".into()),
            kind: Some(ApprovalKind::Framing),
            removed_fields: vec!["confirmed".into()],
            ..AuditDetail::default()
        };
        let value = serde_json::to_value(&detail).unwrap();
        assert_eq!(value["kind"], "framing");
        assert_eq!(value["removed_fields"][0], "confirmed");
        assert_eq!(serde_json::from_value::<AuditDetail>(value).unwrap(), detail);
        assert_eq!(audit_entry_json_schema()["title"], "AuditEntry");
    }

    #[test]
    fn audit_client_and_approval_source_follow_caller_context() {
        assert_eq!(audit_client(&CallerContext::Desktop), "desktop");
        assert_eq!(audit_client(&CallerContext::Approval { approval_id: "apr_1".into() }), "desktop");
        assert_eq!(audit_client(&CallerContext::Gateway { session_id: "s".into() }), "gateway");
        assert_eq!(audit_client(&CallerContext::McpBridge), "mcp");
        assert_eq!(audit_client(&CallerContext::Lan { pairing_id: "pair_1".into() }), "pair_1");
        assert_eq!(audit_client(&CallerContext::Lan { pairing_id: String::new() }), "lan");
        assert_eq!(audit_client(&CallerContext::Loopback), "local");
        let voice = CallerContext::Voice { device_id: "aa:bb:cc:dd:ee:ff".into(), session_id: "s1".into() };
        assert_eq!(audit_client(&voice), "voice:aa:bb:cc:dd:ee:ff");
        assert_eq!(voice.label(), "voice");
        assert_eq!(ApprovalSource::from_caller(&voice), Some(ApprovalSource::Voice));
        assert_eq!(ApprovalSource::parse("voice"), Some(ApprovalSource::Voice));

        assert_eq!(ApprovalSource::from_caller(&CallerContext::McpBridge), Some(ApprovalSource::McpBridge));
        assert_eq!(
            ApprovalSource::from_caller(&CallerContext::Lan { pairing_id: "pair_1".into() }),
            Some(ApprovalSource::Lan)
        );
        assert_eq!(ApprovalSource::from_caller(&CallerContext::Desktop), None);
        assert_eq!(ApprovalSource::from_caller(&CallerContext::Loopback), None);
    }
}
