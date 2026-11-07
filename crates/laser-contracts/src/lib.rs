use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod approval;
pub mod geometry;
pub mod risk;

pub use risk::{
    ALWAYS_ENABLED_TOOLS, RiskLevel, tool_can_be_disabled, tool_never_exposed, tool_risk,
};

pub use approval::{
    APPROVAL_ID_PREFIX, ApprovalConnection, ApprovalKind, ApprovalOutcome, ApprovalPhase,
    ApprovalRecord, ApprovalSource, ApprovalStatus, AuditAction, AuditDetail, AuditEntry,
    AuditResult, approval_ttl_seconds, audit_client, safe_action_requires_approval,
};

pub const MATERIAL_LIBRARY_VERSION: u32 = 1;
pub const DEFAULT_POWER_SCALE: u32 = 1000;

/// 进程内共享的 `settings.json` 读-改-写互斥锁（`laser-api` 与 `moss-voice` 共用），
/// 避免 `/api/settings` 与语音中心并发保存时互相覆盖对方刚写入的字段。
pub fn with_settings_file_lock<R>(f: impl FnOnce() -> R) -> R {
    static SETTINGS_FILE_LOCK: Mutex<()> = Mutex::new(());
    let _guard = SETTINGS_FILE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

/// 调用方上下文（技术书 §6.4）：由 HTTP 入口或进程内调用方**构造**，绝不从请求体读取。
/// P1-00 只做传递，不参与任何判定；严格模式（P1-04）将据此决定 R3 动作是否放行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerContext {
    /// `X-Moss-Desktop-Session` 校验通过的桌面会话（P1-07 接入）
    Desktop,
    /// 审批服务在桌面批准后发起的执行
    Approval { approval_id: String },
    /// `/v1/gateway/*` 网关令牌（可选 P2b）
    Gateway { session_id: String },
    /// 进程内 `InProcessBackend`（小智 MCP 桥 / 接入点转发）
    McpBridge,
    /// 局域网配对 token
    Lan { pairing_id: String },
    /// MOSS 内置语音服务（内置语音技术书 §5.2，NV4）：设备与会话由语音服务提供，审计记为 `voice:<设备>`
    Voice {
        device_id: String,
        session_id: String,
    },
    /// 回环且无任何凭据（本机其他进程或网页、测试）
    Loopback,
}

impl CallerContext {
    /// 审计与日志用的稳定标签
    pub fn label(&self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Approval { .. } => "approval",
            Self::Gateway { .. } => "gateway",
            Self::McpBridge => "mcp_bridge",
            Self::Lan { .. } => "lan",
            Self::Voice { .. } => "voice",
            Self::Loopback => "loopback",
        }
    }
}

/// 小智 MCP 桥接遥测快照。只记录真实发生的事情，没有数据时保持空值（技术书 §9.5.3）。
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct McpTelemetrySnapshot {
    /// 桥当前是否与接入点保持 WebSocket 连接（握手成功后为真，断开后为假）。
    #[serde(default)]
    pub connected: bool,
    pub client_name: String,
    pub client_version: String,
    pub last_request: String,
    pub last_request_time: String,
    /// 从接入点收到的 JSON-RPC 文本帧数量（真实计数）。
    pub messages_count: u64,
    pub recent_events: Vec<String>,
    /// 每个接入点的连接状态（NV8 多接入点；`connected` 为其中任一已连接）。
    #[serde(default)]
    pub endpoints: Vec<EndpointLink>,
    /// 工具调用统计（NV8；本次运行以来，重启后清零）。键为工具名。
    #[serde(default)]
    pub tool_stats: BTreeMap<String, ToolCallStat>,
    /// 最近一次 `initialize` 握手里 MOSS 实际应答的 MCP 协议版本；还没握手时为空（2026-11-07 n68，不再写死）。
    #[serde(default)]
    pub protocol_version: String,
}

/// 一个接入点的连接状态。`endpoint` 已脱敏（token 显示为 `***`）。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointLink {
    /// 接入点编号：`primary` 为顶层 `mcp_endpoint`，附加接入点为 `extra-1`、`extra-2`……
    pub id: String,
    pub endpoint: String,
    pub connected: bool,
    /// 本次连接建立的时间（Unix 秒）；未连接时为空
    pub connected_at: Option<f64>,
    /// 累计成功连接次数（大于 1 表示发生过断线重连）
    pub connections: u64,
    /// 最近一次断开或连接失败的原因（不含 token）
    pub last_error: Option<String>,
    /// 当前连接上对方 initialize 时上报的客户端（没有上报时为空）
    pub client: String,
}

/// 一个工具的调用统计。`denied` 为因开关关闭或安全分级被拒绝的次数（不计入失败）。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallStat {
    pub calls: u64,
    pub failures: u64,
    pub denied: u64,
    /// 最近一次调用的时间（Unix 秒）
    pub last_at: f64,
    /// 最近一次调用方（审计标签：`mcp`、`voice:<设备>`……）
    pub last_caller: String,
}

/// 一次工具调用的结果分类（统计用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallOutcome {
    Ok,
    Failed,
    Denied,
}

pub struct McpTelemetry {
    data: Mutex<McpTelemetrySnapshot>,
}

static MCP_TELEMETRY: OnceLock<McpTelemetry> = OnceLock::new();

pub fn global_mcp_telemetry() -> &'static McpTelemetry {
    MCP_TELEMETRY.get_or_init(McpTelemetry::new)
}

/// 自建链路的设备会话数（技术书 §9.4.5）：由桌面端语音中心轮询接入点 health 后写入，
/// `/api/system/info` 读取。负数表示未知（未配置健康检查地址、查询失败或官方链路）。
pub struct VoiceTelemetry {
    device_sessions: std::sync::atomic::AtomicI64,
    /// 高 8 位存储 `phase_code`（0=None, 1=idle, 2=listening, 3=thinking, 4=speaking），低 8 位存储 `level`（0..=100），
    /// 单次原子读写保证 `phase` 与 `level` 永远来自同一快照，消除跨原子变量读取撕裂。
    pulse_packed: std::sync::atomic::AtomicU16,
}

impl VoiceTelemetry {
    const fn new() -> Self {
        Self {
            device_sessions: std::sync::atomic::AtomicI64::new(-1),
            pulse_packed: std::sync::atomic::AtomicU16::new(0),
        }
    }

    pub fn set_device_sessions(&self, sessions: Option<u32>) {
        let value = sessions.map(i64::from).unwrap_or(-1);
        self.device_sessions
            .store(value, std::sync::atomic::Ordering::SeqCst);
        if sessions.unwrap_or(0) == 0 {
            self.set_pulse(None, 0);
        }
    }

    pub fn device_sessions(&self) -> Option<u32> {
        u32::try_from(
            self.device_sessions
                .load(std::sync::atomic::Ordering::SeqCst),
        )
        .ok()
    }

    pub fn set_pulse(&self, phase: Option<&str>, level: u8) {
        let code: u8 = match phase {
            Some("speaking") => 4,
            Some("thinking") => 3,
            Some("listening") => 2,
            Some("idle") => 1,
            _ => 0,
        };
        let clamped: u8 = if matches!(code, 2 | 4) {
            level.min(100)
        } else {
            0
        };
        let packed = (u16::from(code) << 8) | u16::from(clamped);
        self.pulse_packed
            .store(packed, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn pulse(&self) -> (Option<&'static str>, u8) {
        let packed = self.pulse_packed.load(std::sync::atomic::Ordering::SeqCst);
        let code = (packed >> 8) as u8;
        let level = (packed & 0xFF) as u8;
        let phase = match code {
            4 => Some("speaking"),
            3 => Some("thinking"),
            2 => Some("listening"),
            1 => Some("idle"),
            _ => None,
        };
        (phase, level)
    }
}

static VOICE_TELEMETRY: VoiceTelemetry = VoiceTelemetry::new();

pub fn global_voice_telemetry() -> &'static VoiceTelemetry {
    &VOICE_TELEMETRY
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn current_time_str() -> String {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let h = (secs / 3600 % 24 + 8) % 24;
    let m = secs / 60 % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

impl Default for McpTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl McpTelemetry {
    pub fn new() -> Self {
        Self {
            data: Mutex::new(McpTelemetrySnapshot::default()),
        }
    }

    /// 记录一条事件日志（只进日志，不计入消息数）。
    pub fn record_event(&self, event: &str) {
        if let Ok(mut lock) = self.data.lock() {
            let now = current_time_str();
            lock.recent_events.push(format!("{now} {event}"));
            if lock.recent_events.len() > 30 {
                lock.recent_events.remove(0);
            }
        }
    }

    /// 收到一条来自接入点的消息。
    pub fn record_message(&self) {
        if let Ok(mut lock) = self.data.lock() {
            lock.messages_count += 1;
        }
    }

    /// WebSocket 连接建立或断开。断开时清空客户端标识（它只对当前连接有效），保留历史事件与计数。
    pub fn set_connected(&self, connected: bool) {
        if let Ok(mut lock) = self.data.lock() {
            lock.connected = connected;
            if !connected {
                lock.client_name.clear();
                lock.client_version.clear();
            }
        }
    }

    /// 记录本次握手实际应答的协议版本。
    pub fn record_protocol_version(&self, version: &str) {
        if let Ok(mut lock) = self.data.lock() {
            lock.protocol_version = version.to_string();
        }
    }

    pub fn record_client_info(&self, name: &str, version: &str) {
        if let Ok(mut lock) = self.data.lock() {
            lock.client_name = name.to_string();
            lock.client_version = version.to_string();
        }
    }

    pub fn record_request(&self, req: &str) {
        if let Ok(mut lock) = self.data.lock() {
            lock.last_request = req.to_string();
            lock.last_request_time = current_time_str();
        }
    }

    pub fn snapshot(&self) -> McpTelemetrySnapshot {
        self.data.lock().map(|d| d.clone()).unwrap_or_default()
    }

    /// 登记本次要连接的接入点（`(编号, 脱敏地址)`，重启桥时调用）：列表外的旧条目删除，
    /// 编号与地址都没变的保留计数，否则重置。
    pub fn register_endpoints(&self, endpoints: &[(String, String)]) {
        if let Ok(mut lock) = self.data.lock() {
            let previous = std::mem::take(&mut lock.endpoints);
            lock.endpoints = endpoints
                .iter()
                .map(|(id, endpoint)| {
                    previous
                        .iter()
                        .find(|link| &link.id == id && &link.endpoint == endpoint)
                        .cloned()
                        .unwrap_or_else(|| EndpointLink {
                            id: id.clone(),
                            endpoint: endpoint.clone(),
                            ..EndpointLink::default()
                        })
                })
                .collect();
            lock.connected = lock.endpoints.iter().any(|link| link.connected);
        }
    }

    /// 接入点 `id`（脱敏地址 `endpoint`）已连接；未登记过的编号自动登记。
    pub fn link_up(&self, id: &str, endpoint: &str) {
        if let Ok(mut lock) = self.data.lock() {
            if !lock.endpoints.iter().any(|link| link.id == id) {
                lock.endpoints.push(EndpointLink {
                    id: id.to_owned(),
                    endpoint: endpoint.to_owned(),
                    ..EndpointLink::default()
                });
            }
        }
        self.update_link(id, |link| {
            link.endpoint = endpoint.to_owned();
            link.connected = true;
            link.connected_at = Some(unix_now());
            link.connections += 1;
            link.last_error = None;
        });
    }

    /// 接入点 `id` 断开或连接失败（`error` 为空表示正常关闭）。
    pub fn link_down(&self, id: &str, error: Option<&str>) {
        self.update_link(id, |link| {
            link.connected = false;
            link.connected_at = None;
            link.client.clear();
            if let Some(error) = error {
                link.last_error = Some(error.chars().take(200).collect());
            }
        });
    }

    /// 接入点 `id` 上对方 initialize 时上报的客户端名称与版本。
    pub fn record_link_client(&self, id: &str, name: &str, version: &str) {
        let shown = match (name.is_empty(), version.is_empty()) {
            (true, _) => String::new(),
            (false, true) => name.to_owned(),
            (false, false) => format!("{name} v{version}"),
        };
        self.update_link(id, |link| link.client = shown);
    }

    /// 改一个接入点的状态并重算总连接状态；全部断开时清空客户端标识（与 `set_connected(false)` 一致）。
    fn update_link(&self, id: &str, change: impl FnOnce(&mut EndpointLink)) {
        if let Ok(mut lock) = self.data.lock() {
            if let Some(link) = lock.endpoints.iter_mut().find(|link| link.id == id) {
                change(link);
            }
            let any = lock.endpoints.iter().any(|link| link.connected);
            if !any {
                lock.client_name.clear();
                lock.client_version.clear();
            }
            lock.connected = any;
        }
    }

    /// 记一次工具调用（接入点与内置语音共用）。
    pub fn record_tool_call(&self, name: &str, caller: &str, outcome: ToolCallOutcome) {
        if let Ok(mut lock) = self.data.lock() {
            let stat = lock
                .tool_stats
                .entry(name.chars().take(64).collect())
                .or_default();
            stat.calls += 1;
            match outcome {
                ToolCallOutcome::Ok => {}
                ToolCallOutcome::Failed => stat.failures += 1,
                ToolCallOutcome::Denied => stat.denied += 1,
            }
            stat.last_at = unix_now();
            stat.last_caller = caller.chars().take(80).collect();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialLibrary {
    #[serde(default = "material_library_version")]
    pub version: u32,
    #[serde(default = "default_power_scale")]
    pub power_scale: u32,
    #[serde(default)]
    pub materials: BTreeMap<String, MaterialEntry>,
}

fn material_library_version() -> u32 {
    MATERIAL_LIBRARY_VERSION
}

fn default_power_scale() -> u32 {
    DEFAULT_POWER_SCALE
}

impl MaterialLibrary {
    /// 按目标功率标尺（如 255 或 1000）等比例重算整个材料库所有配方参数
    pub fn rescaled_to(&self, target_scale: u32) -> Self {
        if self.power_scale == target_scale || self.power_scale == 0 || target_scale == 0 {
            return self.clone();
        }
        let mut clone = self.clone();
        clone.power_scale = target_scale;
        for entry in clone.materials.values_mut() {
            for thickness in entry.thicknesses.values_mut() {
                if let Some(engrave) = &mut thickness.engrave {
                    if let Some(raster) = &mut engrave.raster {
                        *raster = raster.rescaled_to(target_scale as f64);
                    }
                    if let Some(outline) = &mut engrave.outline {
                        *outline = outline.rescaled_to(target_scale as f64);
                    }
                }
                if let Some(cut) = &mut thickness.cut {
                    *cut = cut.rescaled_to(target_scale as f64);
                }
            }
        }
        clone
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct MaterialEntry {
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub thicknesses: BTreeMap<String, ThicknessEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ThicknessEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engrave: Option<EngraveModes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<ParamCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EngraveModes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raster: Option<ParamCell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<ParamCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParamCell {
    pub laser_min_power: i32,
    pub laser_max_power: i32,
    pub feed_rate: i32,
    pub travel_rate: i32,
    pub threshold: i32,
    pub passes: i32,
    pub pixel_size_mm: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_scale: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl ParamCell {
    pub fn new(laser_max_power: i32, feed_rate: i32, threshold: i32) -> Self {
        Self {
            laser_min_power: 0,
            laser_max_power,
            feed_rate,
            travel_rate: 3000,
            threshold,
            passes: 1,
            pixel_size_mm: 0.1,
            power_scale: Some(1000),
            source: None,
            notes: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }
    }

    /// 将当前参数单元按目标功率标尺（如 255 或 1000）等比例重算
    pub fn rescaled_to(&self, target_scale: f64) -> Self {
        let current_scale = self.power_scale.unwrap_or(1000) as f64;
        if (current_scale - target_scale).abs() < 1e-6
            || current_scale <= 0.0
            || target_scale <= 0.0
        {
            return self.clone();
        }
        let ratio = target_scale / current_scale;
        let rescaled_min = (self.laser_min_power as f64 * ratio)
            .round()
            .clamp(0.0, target_scale) as i32;
        let rescaled_max = (self.laser_max_power as f64 * ratio)
            .round()
            .clamp(0.0, target_scale) as i32;
        let mut clone = self.clone();
        clone.laser_min_power = rescaled_min;
        clone.laser_max_power = rescaled_max;
        clone.power_scale = Some(target_scale.round() as i32);
        clone
    }

    pub fn validate(&self, s_max: i32, max_passes: i32) -> Result<(), String> {
        if !(0..=s_max).contains(&self.laser_min_power)
            || !(0..=s_max).contains(&self.laser_max_power)
            || self.laser_min_power > self.laser_max_power
        {
            return Err("power must be within 0..S_MAX".into());
        }
        if self.feed_rate < 1 || self.travel_rate < 1 {
            return Err("feed_rate and travel_rate must be positive".into());
        }
        if !(-1..=255).contains(&self.threshold) {
            return Err("threshold must be -1..255".into());
        }
        if !(1..=max_passes).contains(&self.passes) {
            return Err("passes are outside the configured range".into());
        }
        if !self.pixel_size_mm.is_finite() || self.pixel_size_mm <= 0.0 {
            return Err("pixel_size_mm must be positive".into());
        }
        Ok(())
    }
}

pub fn material_library_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(MaterialLibrary)
}

pub fn material_library_json_schema() -> serde_json::Value {
    serde_json::to_value(material_library_schema()).expect("material library schema must serialize")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowRecord {
    pub workflow_id: String,
    pub status: String,
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub input: WorkflowInput,
    #[serde(default)]
    pub artifacts: WorkflowArtifacts,
    #[serde(default)]
    pub confirmation_snapshot: Option<WorkflowSnapshot>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub updated_at: f64,
    #[serde(default)]
    pub stale: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowInput {
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub connection_mode: Option<String>,
    #[serde(default)]
    pub font_size: Option<i32>,
    #[serde(default)]
    pub network_telnet_port: Option<i32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowArtifacts {
    #[serde(default)]
    pub gcode_file: Option<String>,
    #[serde(default)]
    pub gcode_sha256: Option<String>,
    #[serde(default)]
    pub image_file: Option<String>,
    #[serde(default)]
    pub preview_file: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowSnapshot {
    #[serde(default)]
    pub power: Option<i32>,
    #[serde(default)]
    pub feed_rate: Option<i32>,
    #[serde(default)]
    pub passes: Option<i32>,
    #[serde(default)]
    pub pixel_size_mm: Option<f64>,
    #[serde(default)]
    pub overscan_mm: Option<f64>,
    #[serde(default)]
    pub threshold: Option<i32>,
    #[serde(default)]
    pub gcode_sha256: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct JobRecord {
    pub job_id: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub calibration_id: Option<String>,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub baudrate: Option<i32>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub http_port: Option<i32>,
    #[serde(default)]
    pub telnet_port: Option<i32>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub expected_gcode_sha256: Option<String>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub started_at: Option<f64>,
    #[serde(default)]
    pub finished_at: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct TextTaskRecord {
    pub task_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub task_dir: Option<String>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub updated_at: f64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CalibrationRecord {
    pub calibration_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub engraving_mode: Option<String>,
    #[serde(default)]
    pub matrix_kind: Option<String>,
    #[serde(default)]
    pub gcode_file: Option<String>,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub selected_cell_number: Option<i32>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub updated_at: f64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ToolCallResult {
    pub success: bool,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ToolCallResult {
    pub fn ok(result: impl Into<String>) -> Self {
        Self {
            success: true,
            result: result.into(),
            detail: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            result: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(
        success: bool,
        result: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            success,
            result: result.into(),
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CheckLaserConnectionArgs {
    #[serde(default)]
    pub baudrate: Option<i32>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub http_port: Option<i32>,
    #[serde(default)]
    pub include_detail: Option<bool>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub telnet_port: Option<i32>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub transport: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct LaserSafeActionArgs {
    pub action: String,
    #[serde(default)]
    pub baudrate: Option<i32>,
    #[serde(default)]
    pub confirmed: Option<bool>,
    #[serde(default)]
    pub connection_mode: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
    #[serde(default)]
    pub feed_rate: Option<f64>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub http_port: Option<i32>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub power: Option<f64>,
    #[serde(default)]
    pub telnet_port: Option<i32>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub z: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RecommendLaserParamsArgs {
    pub material: String,
    pub thickness_mm: f64,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub engraving_mode: Option<String>,
    #[serde(default)]
    pub power_scale: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SelectCalibrationCellArgs {
    pub calibration_id: String,
    pub cell_number: i32,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub engraving_mode: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CommandExecutionArgs {
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct OpenAppArgs {
    pub argument: String,
    #[serde(default)]
    pub payload: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ShortcutKeyExecutionArgs {
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct GenerateTextImageArgs {
    pub text: String,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
    #[serde(default)]
    pub font_size: Option<i32>,
    #[serde(default)]
    pub line_spacing: Option<f64>,
    #[serde(default)]
    pub auto_wrap: Option<bool>,
    #[serde(default)]
    pub max_lines: Option<i32>,
    #[serde(default)]
    pub layout_mode: Option<String>,
    #[serde(default)]
    pub output_file: Option<String>,
    #[serde(default)]
    pub font_path: Option<String>,
}

pub fn builtin_material_library() -> MaterialLibrary {
    let presets = [
        (
            "椴木",
            vec![
                "basswood",
                "wood",
                "plywood",
                "木头",
                "木板",
                "木料",
                "木牌",
                "木片",
                "木质",
                "椴木板",
            ],
            "3",
            420,
            1800,
            900,
            360,
        ),
        (
            "亚克力",
            vec!["acrylic", "plexiglass", "有机玻璃"],
            "3",
            340,
            1600,
            850,
            260,
        ),
        ("皮革", vec!["leather", "皮料"], "1.5", 280, 1900, 650, 520),
        (
            "纸板",
            vec!["cardboard", "paperboard", "瓦楞纸", "卡纸"],
            "1",
            220,
            2500,
            520,
            760,
        ),
    ];
    let mut materials = BTreeMap::new();
    for (name, aliases, thickness, engrave_power, engrave_feed, cut_power, cut_feed) in presets {
        let mut thicknesses = BTreeMap::new();
        thicknesses.insert(
            thickness.to_owned(),
            ThicknessEntry {
                engrave: Some(EngraveModes {
                    raster: Some(ParamCell::new(engrave_power, engrave_feed, -1)),
                    outline: Some(ParamCell::new(engrave_power, engrave_feed, -1)),
                }),
                cut: Some(ParamCell::new(cut_power, cut_feed, 128)),
            },
        );
        materials.insert(
            name.to_owned(),
            MaterialEntry {
                aliases: aliases.into_iter().map(str::to_owned).collect(),
                thicknesses,
            },
        );
    }
    MaterialLibrary {
        version: MATERIAL_LIBRARY_VERSION,
        power_scale: DEFAULT_POWER_SCALE,
        materials,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_integer_and_schema_is_exportable() {
        let library = MaterialLibrary {
            version: MATERIAL_LIBRARY_VERSION,
            power_scale: DEFAULT_POWER_SCALE,
            materials: BTreeMap::new(),
        };
        assert_eq!(serde_json::to_value(library).unwrap()["version"], 1);
        assert_eq!(material_library_json_schema()["title"], "MaterialLibrary");
    }

    #[test]
    fn unknown_cell_fields_round_trip_through_extra() {
        let input = serde_json::json!({
            "laser_min_power": 0, "laser_max_power": 420,
            "feed_rate": 1800, "travel_rate": 3000,
            "threshold": -1, "passes": 1, "pixel_size_mm": 0.1,
            "confidence": "verified"
        });
        let cell: ParamCell = serde_json::from_value(input).unwrap();
        assert_eq!(cell.extra["confidence"], "verified");
        assert_eq!(
            serde_json::to_value(cell).unwrap()["confidence"],
            "verified"
        );
    }

    #[test]
    fn builtin_presets_have_four_materials_and_twelve_cells() {
        let library = builtin_material_library();
        assert_eq!(library.materials.len(), 4);
        let cells: usize = library
            .materials
            .values()
            .flat_map(|entry| entry.thicknesses.values())
            .map(|entry| {
                entry.engrave.as_ref().map_or(0, |m| {
                    usize::from(m.raster.is_some()) + usize::from(m.outline.is_some())
                }) + usize::from(entry.cut.is_some())
            })
            .sum();
        assert_eq!(cells, 12);
    }

    #[test]
    fn parameter_cell_range_validation_is_strict() {
        assert!(ParamCell::new(800, 1200, -1).validate(1000, 10).is_ok());
        let mut invalid = ParamCell::new(800, 1200, -1);
        invalid.threshold = 256;
        assert!(invalid.validate(1000, 10).is_err());
        invalid = ParamCell::new(1001, 1200, -1);
        assert!(invalid.validate(1000, 10).is_err());
    }

    #[test]
    fn param_cell_and_material_library_rescale_properly() {
        let cell = ParamCell::new(420, 1800, -1);
        assert_eq!(cell.power_scale, Some(1000));
        let rescaled = cell.rescaled_to(255.0);
        assert_eq!(rescaled.power_scale, Some(255));
        assert_eq!(rescaled.laser_max_power, 107); // 420 * 255 / 1000 = 107.1 -> 107

        let lib = builtin_material_library();
        assert_eq!(lib.power_scale, 1000);
        let lib_255 = lib.rescaled_to(255);
        assert_eq!(lib_255.power_scale, 255);
        let wood = &lib_255.materials["椴木"].thicknesses["3"];
        let raster = wood.engrave.as_ref().unwrap().raster.as_ref().unwrap();
        assert_eq!(raster.power_scale, Some(255));
        assert_eq!(raster.laser_max_power, 107);
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;

    #[test]
    fn telemetry_starts_empty_and_tracks_real_link_state() {
        let telemetry = McpTelemetry::new();
        let fresh = telemetry.snapshot();
        assert!(!fresh.connected);
        assert!(fresh.client_name.is_empty());
        assert!(fresh.last_request.is_empty());
        assert!(fresh.recent_events.is_empty());
        assert_eq!(fresh.messages_count, 0);

        telemetry.set_connected(true);
        telemetry.record_client_info("client-a", "1.2");
        telemetry.record_message();
        telemetry.record_message();
        telemetry.record_event("[RECV] initialize");
        let live = telemetry.snapshot();
        assert!(live.connected);
        assert_eq!(live.client_name, "client-a");
        assert_eq!(live.messages_count, 2, "事件日志不得计入消息数");
        assert_eq!(live.recent_events.len(), 1);

        telemetry.set_connected(false);
        let after = telemetry.snapshot();
        assert!(!after.connected);
        assert!(after.client_name.is_empty() && after.client_version.is_empty());
        assert_eq!(after.messages_count, 2);
        assert_eq!(after.recent_events.len(), 1);
    }

    /// NV8：多个接入点各自记录状态；任一连接即为已连接，全部断开才算断开；重新登记时保留同地址的计数。
    #[test]
    fn telemetry_tracks_each_endpoint_separately() {
        let telemetry = McpTelemetry::new();
        telemetry.register_endpoints(&[
            (
                "primary".to_owned(),
                "wss://a.test/mcp/?token=***".to_owned(),
            ),
            (
                "extra-1".to_owned(),
                "wss://b.test/mcp/?token=***".to_owned(),
            ),
        ]);
        telemetry.link_up("primary", "wss://a.test/mcp/?token=***");
        telemetry.link_up("extra-1", "wss://b.test/mcp/?token=***");
        telemetry.record_link_client("extra-1", "xiaozhi", "2.0");
        telemetry.record_client_info("xiaozhi", "2.0");
        telemetry.link_down("primary", Some("连接被重置"));
        let snapshot = telemetry.snapshot();
        assert!(snapshot.connected, "另一个接入点仍在线");
        assert_eq!(snapshot.client_name, "xiaozhi");
        assert_eq!(
            snapshot.endpoints[0].last_error.as_deref(),
            Some("连接被重置")
        );
        assert!(!snapshot.endpoints[0].connected && snapshot.endpoints[0].connected_at.is_none());
        assert_eq!(snapshot.endpoints[1].client, "xiaozhi v2.0");
        telemetry.link_up("primary", "wss://a.test/mcp/?token=***");
        assert_eq!(
            telemetry.snapshot().endpoints[0].connections,
            2,
            "重连后计数增加"
        );
        assert!(telemetry.snapshot().endpoints[0].last_error.is_none());
        telemetry.link_down("extra-1", None);
        telemetry.link_down("primary", None);
        let snapshot = telemetry.snapshot();
        assert!(!snapshot.connected);
        assert!(snapshot.client_name.is_empty(), "全部断开后清空客户端标识");
        telemetry.link_down("nope", Some("x"));
        assert_eq!(
            telemetry.snapshot().endpoints.len(),
            2,
            "断开未登记的编号不新增条目"
        );
        telemetry.link_up("solo", "ws://127.0.0.1/mcp/");
        assert!(telemetry.snapshot().connected, "单独运行的桥自动登记");
        telemetry.link_down("solo", None);
        // 重新登记：编号与地址都没变的保留计数，新的从零开始，列表外的删除
        telemetry.register_endpoints(&[
            (
                "primary".to_owned(),
                "wss://a.test/mcp/?token=***".to_owned(),
            ),
            ("extra-2".to_owned(), "wss://c.test/mcp/".to_owned()),
        ]);
        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.endpoints.len(), 2);
        assert_eq!(snapshot.endpoints[0].connections, 2);
        assert_eq!(
            (
                snapshot.endpoints[1].id.as_str(),
                snapshot.endpoints[1].connections
            ),
            ("extra-2", 0)
        );
    }

    #[test]
    fn telemetry_counts_tool_calls_by_outcome() {
        let telemetry = McpTelemetry::new();
        telemetry.record_tool_call("laser_workflow_tool", "mcp", ToolCallOutcome::Ok);
        telemetry.record_tool_call("laser_workflow_tool", "voice:aa", ToolCallOutcome::Failed);
        telemetry.record_tool_call("laser_workflow_tool", "mcp", ToolCallOutcome::Denied);
        let stat = telemetry.snapshot().tool_stats["laser_workflow_tool"].clone();
        assert_eq!((stat.calls, stat.failures, stat.denied), (3, 1, 1));
        assert_eq!(stat.last_caller, "mcp");
        assert!(stat.last_at > 0.0);
    }
}

#[cfg(test)]
mod caller_context_tests {
    use super::CallerContext;

    #[test]
    fn caller_context_labels_are_stable() {
        assert_eq!(CallerContext::Desktop.label(), "desktop");
        assert_eq!(
            CallerContext::Approval {
                approval_id: "apr_1".into()
            }
            .label(),
            "approval"
        );
        assert_eq!(
            CallerContext::Gateway {
                session_id: "s".into()
            }
            .label(),
            "gateway"
        );
        assert_eq!(CallerContext::McpBridge.label(), "mcp_bridge");
        assert_eq!(
            CallerContext::Lan {
                pairing_id: "p".into()
            }
            .label(),
            "lan"
        );
        assert_eq!(CallerContext::Loopback.label(), "loopback");
    }
}
