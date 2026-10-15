//! MOSS 内置语音服务（`docs/integration/moss-native-voice-spec.md`）。
//!
//! NV1 内容：
//! - OTA 接口（默认 8003 端口）：`GET/POST /xiaozhi/ota/`，登记设备，已允许的设备下发 WebSocket 地址与 token；
//! - WebSocket（默认 8000 端口，任意路径，固件默认 `/xiaozhi/v1/`）：校验 token，hello 握手，每台设备一个会话；
//! - 设备允许流程：未允许 / 已撤销 / token 失效的设备在握手后收到提示（`alert`）并断开；
//! - 回声模式：把设备说的话原样播回，用于验证连接、音频帧格式与播放节奏。
//!
//! NV2 内容（[`audio`]）：上行 Opus 解码、自动拾音模式的语音活动检测（静音 200 ms 判停）、
//! 下行重采样与 Opus 编码、播放节奏泵（预缓冲、绝对时钟、供不上时补静音）、设备打断。
//! 回声模式改为“解码 → 判停 → 重采样到 24 kHz → 重新编码”播回，覆盖整条音频链路。
//!
//! NV3 内容（[`conversation`]）：对话模式——OpenAI 兼容识别 / 大模型（流式 + 工具调用）/ 合成（提供方见
//! `moss-voice-providers`），切句后逐句合成并与文字同步播放；对话历史按长度截断；唤醒回应、退出词、空闲说再见。
//!
//! 运行在专用的 tokio 运行时中（2 个工作线程），只在用户启用内置语音时创建；停止时整个运行时随之关闭，
//! 不影响 MOSS 现有的 HTTP 服务、发送线程与急停路径。设备登记与 token 密钥由宿主通过 [`DeviceRegistry`] 提供。

pub mod audio;
mod barge;
mod safe_fs; // shared read-only path safety (NV14 music, NV19-05 knowledge)
pub mod conversation;
pub mod fast;
pub mod vision;
mod firmware; // NV17 inert local fixture; production OTA unchanged
mod speaker; // NV16 synthetic-only policy, default disabled
pub mod speaker_enrollment;
pub mod person_context;
pub mod home_tools;
pub mod music;
pub mod memory;
pub mod context; // NV19-01 structured long-term context model (not wired yet)
pub mod plugins;
mod ota;
mod session;
mod state;
pub mod tools;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use moss_xiaozhi_proto::TokenSigner;
use serde::Serialize;

pub use audio::{LocalVadFactory, VadConfig, VadFactory};
pub use conversation::{Conversation, ConversationSettings, NoTools, ToolContext, ToolExecutor, ToolOutcome};
pub use tools::{HostTools, ToolHost, ToolHostResult};
pub use state::{ServerEvent, SessionView, TurnTiming, ToolTiming};

/// 设备登记状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    Pending,
    Allowed,
    Revoked,
}

/// 一次设备来访（OTA 或 WebSocket）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceVisit {
    pub device_id: String,
    pub client_id: String,
    pub model: String,
    pub firmware_version: String,
    pub ip: String,
}

/// 宿主提供的设备登记与密钥。实现须线程安全；调用很快（本地数据库），在运行时线程中直接调用。
pub trait DeviceRegistry: Send + Sync {
    /// 记录来访并返回登记状态（新设备登记为待允许）。
    fn seen(&self, visit: &DeviceVisit) -> Result<DeviceStatus, String>;
    /// 设备 token 的签名密钥（首次调用时由宿主生成并保存）。
    fn token_secret(&self) -> Result<Vec<u8>, String>;
    /// Register a token-verified session against CURRENT authorization, after hello.
    fn open_session(&self, device: &str, client: &str, session: &str) -> Result<Arc<std::sync::atomic::AtomicBool>, String>;
    /// NV12 pure read-only authorization, no seen/last_seen writes or session creation.
    /// Default deny protects other registry implementations until explicitly updated.
    /// Returns the current grant generation; a previously acquired HTTP permit may
    /// not revive after revoke and allow, even if both happen while a model is working.
    fn authorize_http(&self, _device: &str, _client: &str) -> Result<Option<u64>, String> { Ok(None) }
    fn close_session(&self, session: &str);
    /// 已连接（通过授权）的设备会话数变化（NV5：桌面端据此更新顶栏的设备状态）。
    fn sessions_changed(&self, _count: usize) {}
}

/// A live permission that cannot become valid again after revoke, disconnect or shutdown.
pub(crate) struct SessionLease {
    pub allowed: Arc<std::sync::atomic::AtomicBool>,
    pub registry: Arc<dyn DeviceRegistry>,
    pub session: String,
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        self.allowed.store(false, std::sync::atomic::Ordering::Release);
        self.registry.close_session(&self.session);
    }
}

/// 会话模式。后续阶段（NV3 起）增加对话模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMode {
    /// 把设备说的话播回（经过完整的解码、判停、重采样与编码）
    Echo,
    /// 对话（NV3）：识别 → 大模型 → 合成；需要 [`ServerConfig::conversation`]
    Chat,
}

#[derive(Clone)]
pub struct ServerConfig {
    pub bind: IpAddr,
    /// WebSocket 端口（0 表示由系统分配，测试用）
    pub ws_port: u16,
    /// OTA 端口（0 表示由系统分配，测试用）
    pub http_port: u16,
    pub mode: ServerMode,
    /// 对话模式使用的模型与工具（回声模式忽略）
    pub conversation: Option<Arc<Conversation>>,
    /// NV14: user-approved read-only local music index; None never scans.
    pub music: Option<Arc<music::MusicIndex>>,
    /// NV12: independently opt-in HTTP image explanation, no device camera access.
    pub vision: Option<Arc<dyn vision::VisionFactory>>,
    /// NV10: optional opt-in memory service.
    pub memory: Option<Arc<memory::MemoryService>>,
    /// 自动拾音模式下的语音活动检测参数
    pub vad: VadConfig,
    /// NV8b：仅 realtime 模式检测插话，须先在设备启用回声消除。
    pub barge_in: bool,
    /// 连续语音阈值，300..=600 ms；默认 400 ms。
    pub barge_in_ms: u32,
    /// 判停器工厂（NV6：本地 Silero）；为空或创建失败时用能量判停
    pub vad_factory: Option<Arc<dyn VadFactory>>,
    /// 下行（播放）采样率，与 xiaozhi-esp32-server 一致默认 24 kHz
    pub downlink_sample_rate: u32,
    /// 流式播放开始前先攒多少毫秒音频（NV3 起用于合成；回声模式数据一次给齐，不受影响）
    pub playback_start_ms: u32,
    /// 多久没有说话也没有文本消息就断开（自动模式下设备上传的静音帧不算）
    pub idle_timeout: Duration,
    /// 下发给设备的帧协议版本（OTA 响应 `websocket.version`）
    pub protocol_version: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ws_port: 8000,
            http_port: 8003,
            mode: ServerMode::Echo,
            conversation: None,
            music: None,
            vision: None,
            memory: None,
            vad: VadConfig::default(),
            barge_in: false,
            barge_in_ms: 400,
            vad_factory: None,
            downlink_sample_rate: 24_000,
            playback_start_ms: 600,
            idle_timeout: Duration::from_secs(120),
            protocol_version: 1,
        }
    }
}

/// 页面显示用的服务状态。
#[derive(Debug, Clone, Serialize)]
pub struct ServerStatusView {
    pub running: bool,
    pub mode: ServerMode,
    pub ws_port: u16,
    pub http_port: u16,
    pub sessions: Vec<SessionView>,
    pub events: Vec<ServerEvent>,
    pub timings: Vec<TurnTiming>,
}

pub(crate) struct Shared {
    pub config: ServerConfig,
    pub http_port: u16,
    pub vision_quotas: Arc<vision::VisionQuotas>,
    pub ws_port: u16,
    pub registry: Arc<dyn DeviceRegistry>,
    pub signer: TokenSigner,
    pub state: state::LiveState,
    pub shutdown: tokio::sync::watch::Receiver<bool>,
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 运行中的内置语音服务。丢弃或调用 [`stop`](Self::stop) 时关闭。
pub struct VoiceServer {
    runtime: Option<tokio::runtime::Runtime>,
    shutdown: tokio::sync::watch::Sender<bool>,
    shared: Arc<Shared>,
    ws_port: u16,
    http_port: u16,
}

fn bind(ip: IpAddr, port: u16, label: &str) -> Result<std::net::TcpListener, String> {
    let listener = std::net::TcpListener::bind(SocketAddr::new(ip, port)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            format!("{label}端口 {port} 已被占用（可能有外部 xiaozhi-server 正在运行），请先停止它或在设置中换端口")
        } else {
            format!("{label}端口 {port} 无法监听：{error}")
        }
    })?;
    listener.set_nonblocking(true).map_err(|error| format!("{label}端口设置失败：{error}"))?;
    Ok(listener)
}

impl VoiceServer {
    /// 绑定端口并启动。端口被占用等错误在这里同步返回。
    pub fn start(config: ServerConfig, registry: Arc<dyn DeviceRegistry>) -> Result<Self, String> {
        if !(300..=600).contains(&config.barge_in_ms) {
            return Err("插话时长须为 300 ~ 600 毫秒".into());
        }
        if config.mode == ServerMode::Chat && config.conversation.is_none() {
            return Err("对话模式需要先配置语音识别、大模型与语音合成".into());
        }
        let secret = registry.token_secret()?;
        if secret.len() < 16 {
            return Err("设备 token 密钥过短".into());
        }
        let ws_listener = bind(config.bind, config.ws_port, "设备连接（WebSocket）")?;
        let http_listener = bind(config.bind, config.http_port, "OTA ")?;
        let ws_port = ws_listener.local_addr().map_err(|e| e.to_string())?.port();
        let http_port = http_listener.local_addr().map_err(|e| e.to_string())?.port();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moss-voice")
            .enable_all()
            .build()
            .map_err(|error| format!("创建语音服务运行时失败：{error}"))?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Shared {
            config: config.clone(),
            ws_port,
            http_port,
            vision_quotas:Arc::new(vision::VisionQuotas::default()),
            registry,
            signer: TokenSigner::new(secret),
            state: state::LiveState::default(),
            shutdown: shutdown_rx,
        });
        shared.state.event(format!("内置语音服务已启动：设备连接端口 {ws_port}，OTA 端口 {http_port}"));
        let ws_app = session::router(shared.clone());
        let http_app = ota::router(shared.clone()).merge(vision::router(shared.clone()));
        let mut stop_ws = shutdown_tx.subscribe();
        let mut stop_http = shutdown_tx.subscribe();
        // 不用 block_on：调用方可能处在另一个 tokio 运行时的阻塞线程里（Tauri 命令），嵌套 block_on 会 panic
        let events = shared.clone();
        runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(ws_listener) {
                Ok(listener) => {
                    let _ = axum::serve(listener, ws_app.into_make_service_with_connect_info::<SocketAddr>())
                        .with_graceful_shutdown(async move {
                            let _ = stop_ws.wait_for(|stop| *stop).await;
                        })
                        .await;
                }
                Err(error) => events.state.event(format!("设备连接端口启动失败：{error}")),
            }
        });
        let events = shared.clone();
        runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(http_listener) {
                Ok(listener) => {
                    let _ = axum::serve(listener, http_app.into_make_service_with_connect_info::<SocketAddr>())
                        .with_graceful_shutdown(async move {
                            let _ = stop_http.wait_for(|stop| *stop).await;
                        })
                        .await;
                }
                Err(error) => events.state.event(format!("OTA 端口启动失败：{error}")),
            }
        });
        Ok(Self { runtime: Some(runtime), shutdown: shutdown_tx, shared, ws_port, http_port })
    }

    pub fn ws_port(&self) -> u16 {
        self.ws_port
    }

    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    pub fn status(&self) -> ServerStatusView {
        ServerStatusView {
            running: self.runtime.is_some(),
            mode: self.shared.config.mode,
            ws_port: self.ws_port,
            http_port: self.http_port,
            sessions: self.shared.state.sessions(),
            events: self.shared.state.events(),
            timings: self.shared.state.timings(),
        }
    }

    /// 通知所有会话结束并关闭运行时（最多等 2 秒）。
    pub fn stop(mut self) {
        self.shutdown_now();
    }

    fn shutdown_now(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

impl Drop for VoiceServer {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

#[cfg(test)]
mod tests;
