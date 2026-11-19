use base64::Engine;
use sha1::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;

pub mod agent_runs;
pub mod approval;
pub mod external_tools;
mod feedback;
mod voice_authority;

mod access;
mod auth;
mod connection;
mod draw_state;
mod engrave_route;
mod events;
mod files;
mod generation;
mod http;
mod jobs;
mod material_lab;
mod material_lab_patterns;
mod materials;
mod registration;
mod resume;
mod routes;
mod safe_action;
mod send;
mod server;
mod service;
mod settings;
mod sketch;
mod time_calibration;
mod update;
mod workflow;

pub use access::*;
pub use auth::*;
pub use connection::*;
pub use events::*;
pub use files::*;
use generation::*;
pub use http::*;
pub use jobs::*;
use materials::*;
use resume::*;
use safe_action::*;
use send::*;
#[cfg(test)]
use server::*;
pub use service::*;
use settings::*;
pub use update::*;
use workflow::*;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../frontend/dist/"]
pub struct FrontendFixture;

/// 移动端窗口直接加载 Tauri 资源协议内嵌包且不对外提供局域网 Web 页面，跳过重复内嵌 `frontend/dist`。
#[cfg(any(target_os = "android", target_os = "ios"))]
pub struct FrontendFixture;

#[cfg(any(target_os = "android", target_os = "ios"))]
impl FrontendFixture {
    pub fn get(_file_path: &str) -> Option<rust_embed::EmbeddedFile> {
        None
    }
}

pub struct ApiService {
    settings: laser_runtime::LaserSettings,
    materials: laser_contracts::MaterialLibrary,
    upload_root: Option<std::path::PathBuf>,
    preview_roots: Vec<std::path::PathBuf>,
    auth: Option<std::sync::Arc<AuthManager>>,
    workflows:
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    /// 无 SQLite 时的标定矩阵内存表（与 workflows 同构，按 calibration_id 键控）。
    calibrations:
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    store: Option<std::sync::Arc<dyn laser_runtime::RuntimeStore + Send + Sync>>,
    runtime_db: Option<std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    read_db: Option<std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    generated_root: Option<std::path::PathBuf>,
    actual_port: std::sync::Arc<std::sync::atomic::AtomicU16>,
    runtime_path: Option<std::path::PathBuf>,
    active_jobs: ActiveJobs,
    /// 已结束但物理停机未获确认的任务：保留原设备实时通道供 0x18 重试，绝不猜测其他目标。
    uncertain_jobs: UncertainJobs,
    launching_workflows: LaunchReservations,
    target_open_locks: TargetOpenLocks,
    material_write_lock: std::sync::Mutex<()>,
    feedback_adoption: std::sync::Mutex<()>,
    draw_state_lock: std::sync::Mutex<()>,
    draw_state_memory: std::sync::Mutex<draw_state::DrawStateRecord>,
    transport_factory: TransportFactory,
    /// `MOSS_ALLOWED_ORIGINS` 额外允许的浏览器来源（精确匹配，已拒绝 `*`）。
    extra_allowed_origins: Vec<String>,
    /// 系统串口枚举：默认查询本机真实串口；测试注入固定列表，避免结果随插拔的设备变化。
    serial_port_lister: SerialPortLister,
    controller_candidates: ControllerCandidateFinder,
    /// 激光侧诊断事件出口（桌面端接到语音中心“记录与诊断”）
    event_sink: SharedLaserEventSink,
    /// 局域网扫描时等每台设备回应的时限（默认 1.5 秒；收齐回应即返回）
    discovery_reply_timeout: std::time::Duration,
    /// 严格模式（`MOSS_FUSION_STRICT`，技术书 §6.2）：开启后 R3 动作需要桌面会话或审批授权。
    /// 语音中心的严格模式开关可在运行中改变（n27），因此用原子量。
    strict_mode: std::sync::atomic::AtomicBool,
    /// 桌面审批服务（有本地数据库时可用）。
    approvals: Option<std::sync::Arc<approval::ApprovalService>>,
    /// 桌面会话（`X-Moss-Desktop-Session`，技术书 §5.3）：只有桌面壳调用 `enable_desktop_session` 后才存在。
    desktop_session: std::sync::OnceLock<String>,
    agent_runs: std::sync::OnceLock<std::sync::Arc<dyn agent_runs::AgentRuns>>,
    external_tools: std::sync::OnceLock<std::sync::Arc<dyn external_tools::ExternalTools>>,
    /// 用户关闭的工具（`settings.json` 的 `mcp_disabled_tools`，NV8）：接入点与内置语音都不列出、不可调用。
    disabled_tools: std::sync::RwLock<std::collections::BTreeSet<String>>,
    /// n72 “完整工具（高级）”（`settings.json` 的 `voice.mcp_full_tool_list`）：关（默认）时接入点与内置语音
    /// 只拿到精简工具清单（`laser_mcp::compact_tools_list_json`）。
    full_tool_list: std::sync::atomic::AtomicBool,
    /// n73 工作台最近一次握手成功的连接（`settings.json` 的 `laser_verified_connection`）：
    /// 语音与接入点调用激光工具时没给串口号 / IP，就用它（2026-11-08 用户决定）。
    verified_connection: std::sync::RwLock<Option<serde_json::Value>>,
    voice_sessions: voice_authority::VoiceSessions,
    /// 宿主关停门禁：关停清点前置位，拒绝所有新启动的连接、发码与设备操作（急停现有在途/待确认通道除外）。
    shutdown_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
