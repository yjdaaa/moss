//! 桌面审批服务（技术书 §5.3–§5.5；实施清单 P1-03）。
//!
//! 负责：解析显式连接目标（拿不到就 `target_required`，绝不自动选串口）、计算动作摘要、
//! 创建 / 批准 / 拒绝 / 过期、单次消费与执行结果记录。这里只管审批状态，不碰设备；
//! 严格模式下由执行入口调用（P1-04），桌面审批接口在 P1-07 接入。

use std::sync::{Arc, Mutex};

use laser_contracts::{
    APPROVAL_ID_PREFIX, ApprovalConnection, ApprovalKind, ApprovalOutcome, ApprovalPhase,
    ApprovalRecord, ApprovalSource, ApprovalStatus, AuditAction, AuditDetail, AuditEntry,
    AuditResult, CallerContext, approval_ttl_seconds,
};
use laser_runtime::{RuntimeDb, SchemaError};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

mod desktop;
mod home_agent;
mod lifecycle;
mod strict;

pub use desktop::DESKTOP_SESSION_HEADER;
pub(crate) use desktop::{framing_digest_facts, safe_action_digest_facts};
pub(crate) use strict::{R3Action, R3Authority, error_response, object_with_connection};

/// 审批服务错误，`code()` 即接口中的 `error_code`（技术书 §5.1 错误码表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalError {
    /// 无法得到显式连接目标（串口留空且无 `.env` 默认值等）
    TargetRequired(String),
    InvalidRequest(String),
    NotFound,
    Expired,
    /// 已被使用或已失败
    Consumed,
    Rejected,
    /// 尚未批准就尝试执行
    NotApproved,
    /// 摘要与批准时 / 当前状态不一致
    Mismatch,
    RequestConflict,
    /// 数据库不可用
    Unavailable(String),
}

impl ApprovalError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TargetRequired(_) => "target_required",
            Self::InvalidRequest(_) => "approval_invalid_request",
            Self::NotFound => "approval_not_found",
            Self::Expired => "approval_expired",
            Self::Consumed => "approval_consumed",
            Self::Rejected => "approval_rejected",
            Self::NotApproved => "approval_not_approved",
            Self::Mismatch => "approval_mismatch",
            Self::RequestConflict => "approval_request_conflict",
            Self::Unavailable(_) => "approval_unavailable",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            Self::TargetRequired(_) | Self::InvalidRequest(_) => 400,
            Self::NotFound => 404,
            Self::Expired => 410,
            Self::Consumed
            | Self::Rejected
            | Self::NotApproved
            | Self::Mismatch
            | Self::RequestConflict => 409,
            Self::Unavailable(_) => 503,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::TargetRequired(detail) => format!("需要明确的目标设备：{detail}"),
            Self::InvalidRequest(detail) => format!("审批请求无效：{detail}"),
            Self::NotFound => "审批请求不存在".into(),
            Self::Expired => "授权已过期，请重新发起".into(),
            Self::Consumed => "授权已被使用或已失败，不能重复执行".into(),
            Self::Rejected => "该请求已被拒绝".into(),
            Self::NotApproved => "该请求尚未在桌面批准".into(),
            Self::Mismatch => "加工内容或目标设备已变化，与批准时不一致，请重新发起".into(),
            Self::RequestConflict => {
                "同一请求编号对应的内容或目标已改变，请使用新编号重新发起".into()
            }
            Self::Unavailable(detail) => format!("审批服务不可用：{detail}"),
        }
    }
}

impl From<SchemaError> for ApprovalError {
    fn from(error: SchemaError) -> Self {
        Self::Unavailable(format!("{error:?}"))
    }
}

/// 创建审批所需的信息。`facts` 是参与摘要的动作要素（G-code SHA、材料、参数、发送方式等），
/// 由各执行入口按类别整理；连接目标单独给出，摘要中只放它的哈希。
#[derive(Debug, Clone)]
pub struct NewApproval {
    /// Trusted identity derived at entry, never from request JSON. None disables request-id reuse.
    pub owner: Option<String>,
    /// Live voice permission checked under the database lock to fence revoke/re-allow races.
    pub voice_permit: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub kind: ApprovalKind,
    pub target_record: String,
    pub source: ApprovalSource,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub summary: String,
    pub facts: Map<String, Value>,
    pub connection: ApprovalConnection,
    /// 原始执行参数（调用方已去掉连接字段与身份字段）；批准后据此重放执行
    pub request: Map<String, Value>,
    /// 审计 `client`：发起方（`audit_client(&CallerContext)`）
    pub client: String,
    /// 从请求中删除的字段（如模型自行填写的 `confirmed`），单独记一条审计
    pub removed_fields: Vec<String>,
}

/// 时钟（Unix 秒）。测试注入固定时间以验证过期。
pub type ApprovalClock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn system_clock() -> ApprovalClock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0)
    })
}

pub struct ApprovalService {
    db: Arc<Mutex<RuntimeDb>>,
    read_db: Option<Arc<Mutex<RuntimeDb>>>,
    ttl_seconds: u64,
    clock: ApprovalClock,
}

/// 审计写入失败：R3 路径据此拒绝（技术书 §7.3，验收 A-34）。
fn audit_failed(error: SchemaError) -> ApprovalError {
    ApprovalError::Unavailable(format!("审计记录写入失败，已拒绝执行：{error:?}"))
}

fn audit_entry(
    now: f64,
    client: &str,
    action: AuditAction,
    result: AuditResult,
    record: &ApprovalRecord,
    detail: AuditDetail,
) -> AuditEntry {
    AuditEntry {
        at: now,
        client: client.to_owned(),
        action,
        target: record.target_record.clone(),
        result,
        detail: AuditDetail {
            approval_id: Some(record.approval_id.clone()),
            kind: Some(record.kind),
            digest: Some(record.digest.clone()),
            ..detail
        },
    }
}

/// 降低风险的状态变化（拒绝、过期、失败）不因审计写入失败而回退，只记录到错误输出。
fn audit_best_effort(db: &RuntimeDb, entry: &AuditEntry) {
    if let Err(error) = db.append_audit(entry) {
        eprintln!(
            "[laser-api] 审计记录写入失败（{}）: {error:?}",
            entry.action.as_str()
        );
    }
}

/// 审计 `client`：审批过期等由服务自身触发的变化。
const SYSTEM_CLIENT: &str = "system";
/// 审计 `client`：桌面端的批准、拒绝与经批准的执行。
const DESKTOP_CLIENT: &str = "desktop";

fn valid_home_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 64
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_home_target(target: &str) -> bool {
    let Some((domain, name)) = target.split_once('.') else {
        return false;
    };
    matches!(
        domain,
        "light" | "switch" | "climate" | "lock" | "alarm_control_panel" | "cover" | "media_player"
    ) && !name.is_empty()
        && target.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// 条件更新没有生效时（并发改了状态），按最新状态给出错误。
fn status_error(db: &RuntimeDb, approval_id: &str) -> Result<ApprovalError, ApprovalError> {
    let record = db
        .get_approval(approval_id)?
        .ok_or(ApprovalError::NotFound)?;
    Ok(match record.status {
        ApprovalStatus::Pending => ApprovalError::NotApproved,
        ApprovalStatus::Expired => ApprovalError::Expired,
        ApprovalStatus::Rejected => ApprovalError::Rejected,
        ApprovalStatus::Approved | ApprovalStatus::Executed | ApprovalStatus::Failed => {
            ApprovalError::Consumed
        }
    })
}

fn new_approval_id() -> Result<String, ApprovalError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| ApprovalError::Unavailable(error.to_string()))?;
    Ok(format!("{APPROVAL_ID_PREFIX}{}", hex::encode(random)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 规范 JSON（技术书 §5.4）：键名按字节序排序、无多余空白、UTF-8。
/// 不依赖 `serde_json::Map` 的内部顺序（启用 `preserve_order` 时会保留插入顺序）。
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

fn target_key(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::WorkflowConfirmSend => "workflow_id",
        ApprovalKind::CalibrationSend => "calibration_id",
        ApprovalKind::SafeAction
        | ApprovalKind::Framing
        | ApprovalKind::ExternalTool
        | ApprovalKind::AgentRun
        | ApprovalKind::HomeControl => "target_record",
    }
}

/// 动作摘要：`SHA-256(canonical_json(payload))`（技术书 §5.4）。payload = 动作要素 + 类别 + 目标记录
/// 加 `target{mode, host_fingerprint}`；`host_fingerprint` 是解析后连接描述的 SHA-256，摘要里不出现地址明文。
/// 保留键（kind、目标记录键、target）以服务端的值为准，`facts` 不能覆盖。
pub fn approval_digest(
    kind: ApprovalKind,
    target_record: &str,
    facts: &Map<String, Value>,
    connection: &ApprovalConnection,
) -> String {
    let mut payload = facts.clone();
    payload.insert("kind".into(), Value::String(kind.as_str().into()));
    payload.insert(target_key(kind).into(), Value::String(target_record.into()));
    payload.insert(
        "target".into(),
        serde_json::json!({
            "mode": connection.mode(),
            "host_fingerprint": sha256_hex(connection.describe().as_bytes()),
        }),
    );
    sha256_hex(canonical_json(&Value::Object(payload)).as_bytes())
}

/// 从工作流 / 标定记录与执行请求整理摘要要素（§5.4）：G-code SHA、材料、厚度、参数、发送方式。
/// 重新预览会改变 G-code SHA，改发送方式会改变 `send_mode`，都会让旧授权失效。
pub fn record_digest_facts(record: &Value, request: &Map<String, Value>) -> Map<String, Value> {
    let lookup = |section: &str, key: &str| {
        record
            .get(section)
            .and_then(|value| value.get(key))
            .cloned()
    };
    let first = |key: &str| {
        lookup("artifacts", key)
            .or_else(|| lookup("confirmation_snapshot", key))
            .or_else(|| lookup("input", key))
            .or_else(|| record.get(key).cloned())
            .filter(|value| !value.is_null())
    };
    let mut facts = Map::new();
    for key in ["gcode_sha256", "material", "thickness_mm"] {
        if let Some(value) = first(key) {
            facts.insert(key.into(), value);
        }
    }
    let mut params = Map::new();
    for key in [
        "power",
        "laser_max_power",
        "laser_min_power",
        "power_scale",
        "feed_rate",
        "passes",
    ] {
        if let Some(value) = lookup("input", key)
            .or_else(|| lookup("confirmation_snapshot", key))
            .or_else(|| lookup("params", key))
            .filter(|value| !value.is_null())
        {
            params.insert(key.into(), value);
        }
    }
    if !params.is_empty() {
        facts.insert("params".into(), Value::Object(params));
    }
    let offline = request
        .get("offline_sd")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let send_mode = request
        .get("send_mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if offline {
                "sd_offline".into()
            } else {
                "stream".into()
            }
        });
    facts.insert("send_mode".into(), Value::String(send_mode));
    if let Some(line) = request
        .get("start_from_line")
        .and_then(Value::as_u64)
        .filter(|line| *line > 0)
    {
        facts.insert("start_from_line".into(), Value::from(line));
    }
    facts
}

/// 按请求中的显式参数解析审批目标；缺省时只用环境变量（`.env`）默认值：
/// `LASER_DEFAULT_CONNECTION_MODE`、`LASER_NETWORK_HOST`、`LASER_NETWORK_TELNET_PORT`、
/// `GRBL_DEFAULT_PORT`、`GRBL_BAUDRATE`。**不枚举系统串口自动选择**（验收 A-35）。
pub fn resolve_approval_target(
    request: &Map<String, Value>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ApprovalConnection, ApprovalError> {
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| request.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let number = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            request.get(*key).and_then(|value| {
                value.as_u64().or_else(|| {
                    value
                        .as_str()
                        .and_then(|text| text.trim().parse::<u64>().ok())
                })
            })
        })
    };
    let env_text = |name: &str| {
        env(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && !value.starts_with('<'))
    };
    let port = text(&["port", "serial_port"][..]);
    let host = text(&["network_host", "host"][..]);
    let mode = text(&["connection_mode", "preferred_mode"][..])
        .map(|mode| mode.to_ascii_lowercase())
        .filter(|mode| mode == "serial" || mode == "network");
    let serial = match mode.as_deref() {
        Some("serial") => true,
        Some(_) => false,
        None if port.is_some() && host.is_none() => true,
        None if host.is_some() => false,
        None => env_text("LASER_DEFAULT_CONNECTION_MODE")
            .is_some_and(|mode| mode.eq_ignore_ascii_case("serial")),
    };
    if serial {
        let port = port
            .or_else(|| env_text("GRBL_DEFAULT_PORT"))
            .ok_or_else(|| {
                ApprovalError::TargetRequired(
                    "串口模式需要指定串口（或在 .env 配置 GRBL_DEFAULT_PORT），不会自动选择".into(),
                )
            })?;
        let baud = number(&["baudrate", "baud"][..])
            .or_else(|| env_text("GRBL_BAUDRATE").and_then(|value| value.parse().ok()))
            .unwrap_or(115_200);
        if !(1200..=4_000_000).contains(&baud) {
            return Err(ApprovalError::InvalidRequest(
                "baudrate 超出合法范围 (1200 ~ 4000000)".into(),
            ));
        }
        Ok(ApprovalConnection::Serial {
            port,
            baud_rate: baud as u32,
        })
    } else {
        let host = host
            .or_else(|| env_text("LASER_NETWORK_HOST"))
            .ok_or_else(|| {
                ApprovalError::TargetRequired(
                    "网络模式需要指定设备地址（或在 .env 配置 LASER_NETWORK_HOST）".into(),
                )
            })?;
        let port = number(&["network_telnet_port", "telnet_port"][..])
            .or_else(|| env_text("LASER_NETWORK_TELNET_PORT").and_then(|value| value.parse().ok()))
            .unwrap_or(23);
        if !(1..=65535).contains(&port) {
            return Err(ApprovalError::InvalidRequest(
                "network_telnet_port 超出合法范围".into(),
            ));
        }
        Ok(ApprovalConnection::Network {
            host,
            port: port as u16,
        })
    }
}

/// 用进程环境变量（启动时已加载 `.env`）解析审批目标。
pub fn resolve_approval_target_from_env(
    request: &Map<String, Value>,
) -> Result<ApprovalConnection, ApprovalError> {
    resolve_approval_target(request, &|name: &str| std::env::var(name).ok())
}

#[cfg(test)]
mod tests;
