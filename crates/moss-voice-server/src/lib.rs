//! MOSS 内置语音服务（`docs/integration/moss-native-voice-spec.md`）。
//!
//! NV1 内容：
//! - OTA 接口（默认 8003 端口）：`GET/POST /xiaozhi/ota/`，登记设备，已允许的设备下发 WebSocket 地址与 token；
//! - WebSocket（默认 8000 端口，任意路径，固件默认 `/xiaozhi/v1/`）：校验 token，hello 握手，每台设备一个会话；
//! - 设备允许流程：未允许 / 已撤销 / token 失效的设备在握手后收到提示（`alert`）并断开；
//! - 回声模式：把设备说的话原样播回，用于验证连接、音频帧格式与播放节奏。
//!
//! NV2 内容（[`audio`]）：上行 Opus 解码、自动拾音模式的语音活动检测（静音 200 ms 判停）、
//! 下行重采样与 Opus 编码、播放节奏泵（起播保护间隔、按设备端缓冲余量发送、供不上时不发帧）、设备打断。
//! 回声模式改为“解码 → 判停 → 重采样到 24 kHz → 重新编码”播回，覆盖整条音频链路。
//!
//! NV3 内容（[`conversation`]）：对话模式——OpenAI 兼容识别 / 大模型（流式 + 工具调用）/ 合成（提供方见
//! `moss-voice-providers`），切句后逐句合成并与文字同步播放；对话历史按长度截断；唤醒回应、退出词、空闲说再见。
//!
//! 运行在专用的 tokio 运行时中（2 个工作线程），只在用户启用内置语音时创建；停止时整个运行时随之关闭，
//! 不影响 MOSS 现有的 HTTP 服务、发送线程与急停路径。设备登记与 token 密钥由宿主通过 [`DeviceRegistry`] 提供。

pub mod asr_corpus; // VB1/VB2 识别语料采集与标注（默认关，不进诊断包与过程记录）
pub mod audio;
mod barge;
pub mod context; // NV19-01 structured long-term context model (not wired yet)
pub mod conversation;
pub mod debug_api; // VA2 #4–#5 本机 127.0.0.1 调试文字直通入口（独立于 0.0.0.0 OTA 路由）
pub mod fast;
pub mod firmware; // NV17 固件 OTA：逐台确认推送（2026-10-15）
pub use moss_xiaozhi_display::{glyph_font, goggle_bot, screen_pack};
pub mod home_tools;
pub mod jev;
pub mod jobs; // 加工结束主动播报（2026-11-02 用户决定）
mod link; // n71 会话底下的设备连接：WebSocket 或常连模式
pub mod memory;
mod mqtt; // n71 常连模式：内置精简 MQTT 服务端 + UDP 语音（2026-11-08 用户决定）
pub mod music;
mod ota;
pub mod person_context;
pub mod persona; // n59 说话角色（只改语气，安全铁律锁死在末尾）
pub mod plugins;
pub mod preview_push;
mod safe_fs; // shared read-only path safety (NV14 music, NV19-05 knowledge)
mod session;
pub mod sim_guard; // VA2 模拟轮次工具门禁（独立于提示词与桌面审批）
#[cfg(test)]
mod speaker; // NV16 synthetic-only policy, default disabled
pub mod speaker_enrollment;
mod state;
mod tools;
pub mod vision;
pub mod wake_assets; // n61 设备唤醒词资源包：本机主窗口逐台确认推送（n67 起取消独立安全窗口）

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

pub use moss_xiaozhi_proto::TextFontCapability;
use moss_xiaozhi_proto::TokenSigner;
use serde::Serialize;

pub use asr_corpus::{
    AsrCorpusConfig, AsrCorpusItem, AsrCorpusLabel, AsrCorpusMeta, AsrCorpusStore,
    CapturedUtterance,
};
pub use audio::{EnergyVad, LocalVadFactory, Vad, VadConfig, VadEvent, VadFactory};
pub use conversation::{
    Conversation, ConversationSettings, NoTools, ToolContext, ToolExecutor, ToolOutcome,
    TurnEndPredictor,
};
pub use debug_api::{BenchWindowView, DEFAULT_BENCH_WINDOW_MINUTES, MAX_BENCH_WINDOW_MINUTES};
pub use goggle_bot::{BotPose, GoggleBotGifItem, ScreenCardStyle, cached_goggle_bot_gifs};
pub use jev::{
    DecisionClient, DecisionError, JevFilterOptions, OpenRouterDecisionClient, VoiceJevFilter,
};
pub use ota::{
    MDNS_HOST_LABEL, is_virtual_or_tunnel_ipv4, lan_ipv4_addrs, local_ip as primary_lan_ip,
};
pub use preview_push::{StyleSceneGifCatalog, cached_style_scene_gifs};
pub use state::{
    DebugToolTrace, DiagCategory, DiagSource, DiagnosticsLog, EventDrops, EventLevel, EventRecord,
    MAX_INFO_EVENTS, MAX_PROBLEM_EVENTS, MAX_TIMINGS, PlaybackDetail, ServerEvent, SessionView,
    ToolTiming, TurnDebugTrace, TurnDetail, TurnTiming, TurnToolDetail, scrub_debug_text,
};
pub use tools::{HostTools, ToolHost, ToolHostResult};
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
/// 上位机为单台设备记忆的硬件偏好（断电重连握手后按设备已声明能力静默回放）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HardwarePrefs {
    pub volume: Option<u8>,
    pub brightness: Option<u8>,
    pub theme: Option<String>,
    pub talk_mode: Option<String>,
    /// n70 已刷入的 MOSS 全屏画风包标记（`moss-screen-1:moss_hud`），见 [`screen_pack`]。
    pub screen_pack: Option<String>,
}

/// 宿主提供的设备登记与密钥。实现须线程安全；调用很快（本地数据库），在运行时线程中直接调用。
pub trait DeviceRegistry: Send + Sync {
    /// 记录来访并返回登记状态（新设备登记为待允许）。
    fn seen(&self, visit: &DeviceVisit) -> Result<DeviceStatus, String>;
    /// 设备 token 的签名密钥（首次调用时由宿主生成并保存）。
    fn token_secret(&self) -> Result<Vec<u8>, String>;
    /// Register a token-verified session against CURRENT authorization, after hello.
    fn open_session(
        &self,
        device: &str,
        client: &str,
        session: &str,
    ) -> Result<Arc<std::sync::atomic::AtomicBool>, String>;
    /// NV12 pure read-only authorization, no seen/last_seen writes or session creation.
    /// Default deny protects other registry implementations until explicitly updated.
    /// Returns the current grant generation; a previously acquired HTTP permit may
    /// not revive after revoke and allow, even if both happen while a model is working.
    fn authorize_http(&self, _device: &str, _client: &str) -> Result<Option<u64>, String> {
        Ok(None)
    }
    /// n133：设备在 `Pending` 状态下已跑过开机 OTA（当时未获发 token），随后用户在桌面端点击了「允许」：
    /// 宿主可在宽限期内（允许后 7 天内且尚未完成下次已绑定的开机 OTA）按已绑定的 `(device_id, client_id)` 放行无 token WebSocket 连接，
    /// 使设备免去手动重启即可直接唤醒对话；默认返回 `false`。
    fn allow_tokenless_session(&self, _device: &str, _client: &str) -> bool {
        false
    }
    fn close_session(&self, session: &str);
    /// 已连接（通过授权）的设备会话数变化（NV5：桌面端据此更新顶栏的设备状态）。
    fn sessions_changed(&self, _count: usize) {}
    /// 汇总在线会话的主状态（`idle`/`listening`/`thinking`/`speaking`，无会话时 `None`）与归一化音量（0..=100）。
    fn pulse_changed(&self, _phase: Option<&str>, _level: u8) {}

    /// 记录设备快照（开机 OTA 上报 / hello 握手 / 实时状态），`kind` 取 [`SNAPSHOT_INFO`] 等。
    /// 默认忽略：不关心设备信息的宿主不必实现。
    fn device_snapshot(&self, _device: &str, _kind: &str, _json: &str) {}

    /// 读取用户在上位机为该设备记忆的硬件偏好；无设置时返回 `None`，连接握手不额外发指令。
    fn device_hardware_prefs(&self, _device: &str) -> Option<HardwarePrefs> {
        None
    }

    /// 读取设备最近一次 `hello` 握手快照中已确认支持 `glyph_push` 的字库规格（常连模式开机尚未唤醒前恢复动态补字）。
    fn device_text_font(&self, _device: &str) -> Option<TextFontCapability> {
        None
    }

    /// 设备把推送的资源包整包下载完（最后一块已交给连接）。宿主据此把“已推送、等下载”的
    /// 全屏画风标记转为已装（2026-11-09：此前一推送就记为已刷，设备没下载也显示“已刷”）。
    fn assets_downloaded(&self, _device: &str) {}

    /// n81 记录一轮已脱敏的语音对话（说了什么、调了什么工具、回了什么），由宿主持久化到本机数据库。
    fn record_chat_turn(&self, _trace: &TurnDebugTrace) {}

    /// n81/#19 带代次快照的持久化入口：宿主在写入 SQLite 前校验开关与代次未发生停用/清空。
    fn record_chat_turn_at(&self, trace: &TurnDebugTrace, _epoch: u64) {
        self.record_chat_turn(trace);
    }
}
/// A live permission that cannot become valid again after revoke, disconnect or shutdown.
pub(crate) struct SessionLease {
    pub allowed: Arc<std::sync::atomic::AtomicBool>,
    pub registry: Arc<dyn DeviceRegistry>,
    pub session: String,
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        self.allowed
            .store(false, std::sync::atomic::Ordering::Release);
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
    /// NV17：固件推送记录与文件；None 时 OTA 不下发固件、下载地址一律 404。
    pub firmware: Option<Arc<dyn firmware::FirmwareHost>>,
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
    /// 加工结束播报与实时副屏 HUD 的轮询间隔；默认 4 秒，测试用小值加速。
    pub job_poll_interval: Duration,
    /// 彩屏雕刻预览确认窗口内的无声续期节拍（克服官方固件 5 秒自动隐藏预览计时器，默认 4 秒）。
    pub preview_hold_interval: Duration,
    /// 彩屏雕刻预览确认窗口的最长保留时长（默认 24 秒；用户开口/确认/打断立即终止）。
    pub preview_hold_duration: Duration,
    /// 小智彩屏卡片视觉画风（`MossHud` 硬核太空舱 / `GoggleBot` 可爱护目镜小机器人）。
    pub screen_card_style: ScreenCardStyle,
    /// 诊断记录本（事件与耗时）。宿主传入同一个 `Arc`，服务停止、重启后记录仍在（2026-11-07 n68）。
    pub diagnostics: Arc<DiagnosticsLog>,
    /// n71 常连模式（MQTT + UDP）：`None` 为关（默认），设备照旧用 WebSocket。
    pub mqtt: Option<MqttConfig>,
    /// n83 小智官方服务端全量动态补字分片目录（`models/noto_v1_full`）；`None` 或尚未下载完成时对 `basic` 设备回退内置 `common` 字库，下载就位后热生效。
    pub full_font_dir: Option<std::path::PathBuf>,
    /// #19 与宿主共享的对话记录代次计数器：关闭/清空会递增它，使在途记录被宿主识别为过期。
    pub debug_epoch: Arc<std::sync::atomic::AtomicU64>,
    /// 回声模式（尚未配置对话模型）下连接建立时是否同样立即执行设备端 MCP `initialize` 与 `tools/list`，
    /// 以便新装机未填模型密钥时桌面端也能读取 `self.assets.set_download_url` 与屏幕分辨率并推送唤醒词/全屏画风。
    pub eager_device_mcp: bool,
    /// 等设备端 MCP 请求回包的最长时间（默认 60 秒；测试可设短值加速验证超时重试）。
    pub mcp_pending_ttl: Duration,
    /// VA2/VA4 可选只读回调：查询当前是否有活跃的激光加工/走边框任务（不引入对 `laser-api` 的 crate 依赖）。
    pub active_laser_job: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// VA2 #5 本机调试入口的过程记录启停控制（由宿主注入，读写 `settings.json` 与 `ProcessTrace`）。
    pub debug_process_trace: Option<Arc<dyn debug_api::ProcessTraceControl>>,
    /// VB1 识别（ASR）真实录音语料库（默认关闭；模拟轮次不保存，不进诊断包与过程记录）。
    pub asr_corpus: Option<Arc<asr_corpus::AsrCorpusStore>>,
}

/// n71 常连模式的两个端口。MQTT 端口不能用 8883：官方固件见到 8883 会改走 TLS。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MqttConfig {
    pub port: u16,
    pub udp_port: u16,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            port: 1883,
            udp_port: 8888,
        }
    }
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
            firmware: None,
            vad: VadConfig::default(),
            barge_in: false,
            barge_in_ms: 400,
            vad_factory: None,
            downlink_sample_rate: 24_000,
            playback_start_ms: 300,
            idle_timeout: Duration::from_secs(120),
            protocol_version: 1,
            job_poll_interval: jobs::DEFAULT_POLL_INTERVAL,
            preview_hold_interval: Duration::from_secs(4),
            preview_hold_duration: Duration::from_secs(24),
            mqtt: None,
            full_font_dir: None,
            debug_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            eager_device_mcp: false,
            mcp_pending_ttl: session::MCP_PENDING_TTL,
            active_laser_job: None,
            debug_process_trace: None,
            asr_corpus: None,
            screen_card_style: ScreenCardStyle::MossHud,
            diagnostics: DiagnosticsLog::new(),
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
    /// n71 常连模式实际监听的 MQTT 端口（没开为 `None`）
    #[serde(default)]
    pub mqtt_port: Option<u16>,
    /// n71 常连模式语音 UDP 端口
    #[serde(default)]
    pub udp_port: Option<u16>,
    pub sessions: Vec<SessionView>,
    pub events: Vec<ServerEvent>,
    /// 被覆盖的事件 / 耗时计数（界面显示“有 N 条较早的记录已被覆盖”）
    pub events_dropped: EventDrops,
    pub timings: Vec<TurnTiming>,
    #[serde(default)]
    pub debug_enabled: bool,
    #[serde(default)]
    pub debug_traces: Vec<TurnDebugTrace>,
}

/// 设备快照种类（与 `laser_runtime::VOICE_SNAPSHOT_*` 是同一组字符串）。
pub const SNAPSHOT_INFO: &str = "info";
pub const SNAPSHOT_HELLO: &str = "hello";
pub const SNAPSHOT_STATUS: &str = "status";
/// 设备实际使用的拾音模式（`auto` / `manual` / `realtime`）。
pub const SNAPSHOT_LISTEN: &str = "listen";

/// VA1 文字直通注入成功回执。
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct InjectTurnAck {
    pub turn_id: String,
    pub device_id: String,
    pub session_id: String,
    pub turn: u64,
    pub simulated: bool,
}

/// VA1 文字直通注入拒绝/失败信息。
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct InjectTurnError {
    pub code: String,
    pub http_status: u16,
    pub message: String,
}

impl InjectTurnError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_text".into(),
            http_status: 400,
            message: message.into(),
        }
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            code: "device_revoked".into(),
            http_status: 403,
            message: message.into(),
        }
    }
    pub fn offline(message: impl Into<String>) -> Self {
        Self {
            code: "device_offline".into(),
            http_status: 404,
            message: message.into(),
        }
    }
    pub fn busy(phase: &str) -> Self {
        Self {
            code: "device_busy".into(),
            http_status: 409,
            message: format!("设备当前正忙（{phase}），为不打断正在进行的对话，本次文字注入已拒绝"),
        }
    }
    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            http_status: 409,
            message: message.into(),
        }
    }
}

/// 桌面端可以对一台已连接设备发起的即时请求（2026-11-02：设备信息刷新与重启）。
#[derive(Debug)]
pub enum DeviceRequest {
    /// VA1 文字直通注入：在设备空闲时把文字作为“刚识别出的话”送入完整对话链路。
    InjectText {
        text: String,
        simulated: bool,
        reply: tokio::sync::oneshot::Sender<Result<InjectTurnAck, InjectTurnError>>,
    },
    /// VC2 数字音频对照注入：仅允许在测试时间窗内按设备上行帧节奏注入 16/24 kHz PCM。
    InjectAudio {
        pcm: Vec<i16>,
        sample_rate: u32,
        frame_ms: u32,
        lead_silence_ms: u32,
        tail_silence_ms: u32,
        reply: tokio::sync::oneshot::Sender<Result<InjectTurnAck, InjectTurnError>>,
    },
    /// 取设备自带 MCP 的 `self.get_device_status`，结果写成设备快照
    RefreshInfo,
    /// 取 `self.reboot`：设备重启后会重新请求 OTA，固件版本随之刷新
    Reboot,
    /// 设置设备扬声器音量 `0..=100` 并随后刷新状态快照
    SetVolume(u8),
    /// 设置设备屏幕亮度 `0..=100` 并随后刷新状态快照
    SetBrightness(u8),
    /// 设置设备屏幕主题 `"light"` / `"dark"` 并随后刷新状态快照
    SetTheme(String),
    /// 切换设备按键对话模式（`self.set_press_to_talk`：`"click_to_talk"` / `"press_to_talk"`）
    SetTalkMode(String),
    /// 自适应推送雕刻预览：彩屏（支持 `self.screen.preview_image` 且非单色 OLED）推送 1:1 PNG 缩略图与参数底栏；
    /// 普通单色 OLED 或无图片预览能力的官方固件自动推送屏幕文字预览卡。
    PreviewScreen {
        token: Option<String>,
        summary: String,
    },
    /// n61：让设备下载已暂存的唤醒词资源包（`self.assets.set_download_url` 后 `self.reboot`）。
    /// 只由私密窗口逐台确认后发起；设备固件没有该工具时不发送。
    InstallAssets { token: String },
    /// 桌面端「摄像头」标签页主动抓拍一张画面（下发一次性票据，仅存内存，不调外部视觉模型）。
    CapturePhoto { ticket: String },
    /// n70 已刷全屏画风包的设备：直接切到某个全屏场景试看（发 `llm` 表情名，不写闪存、不推图）。
    ShowScene {
        scene: BotPose,
        style: ScreenCardStyle,
    },
    /// 按需握手并拉取设备自带 MCP 工具清单（幂等，已拉取完成或正在拉取时忽略）。
    SyncTools,
}

pub(crate) struct DeviceRequestHandle {
    pub session_id: String,
    pub supports_mcp: bool,
    pub sender: tokio::sync::mpsc::UnboundedSender<DeviceRequest>,
    /// 这个会话最近开始的对话轮次编号（0 为还没有；完整过程记录把看图等旁路请求归到这一轮，n110）
    pub turn: u64,
}

pub(crate) struct Shared {
    pub config: ServerConfig,
    pub http_port: u16,
    pub vision_quotas: Arc<vision::VisionQuotas>,
    /// 2026-10-16：待接进对话上下文的照片（只在用户打开对应开关时写入）。
    pub vision_photos: Arc<vision::VisionPhotos>,
    /// 2026-11-07：桌面端「摄像头」标签页的内存帧与一次性抓拍票据。
    pub camera_preview: Arc<vision::CameraPreviewStore>,
    pub preview_store: Arc<preview_push::PreviewStore>,
    /// n61：待设备下载的唤醒词资源包（只在内存里，30 分钟过期）
    pub assets_store: wake_assets::AssetsStore,
    /// n83：小智三层字库服务端 `glyph_push` 协调器（支持 `noto-v1-full` 热加载与内置 `common` 离线兜底）
    pub glyph_fonts: Arc<glyph_font::GlyphFontService>,
    pub screen_card_style: std::sync::Mutex<ScreenCardStyle>,
    pub ws_port: u16,
    /// n71 常连模式实际监听的 MQTT / UDP 端口；没开或端口开不了为 `None`（OTA 不下发 MQTT）
    pub mqtt_ports: Option<MqttConfig>,
    pub signer: TokenSigner,
    pub registry: Arc<dyn DeviceRegistry>,
    pub state: state::LiveState,
    pub shutdown: tokio::sync::watch::Receiver<bool>,
    /// 同一条加工任务只让一个在线会话播报结束。
    pub job_announcements: jobs::AnnounceLog,
    /// 桌面端 → 已连接设备的即时请求：按设备 MAC 绑定当前活跃会话（带 session_id 与 MCP 能力标志）
    pub device_requests:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, DeviceRequestHandle>>>,
    /// 设备快照后台落库序号门禁：保证按会话提交顺序落库，避免并发 `spawn_blocking` 旧快照覆盖新快照。
    snapshot_seq: std::sync::atomic::AtomicU64,
    snapshot_write_gate:
        Arc<std::sync::Mutex<std::collections::HashMap<(String, &'static str), u64>>>,
    /// 全网卡监听的并发连接与固件下载上限，防止局域网洪泛拖垮桌面主库或内存。
    pub ws_connections: Arc<tokio::sync::Semaphore>,
    pub http_requests: Arc<tokio::sync::Semaphore>,
    pub firmware_downloads: Arc<tokio::sync::Semaphore>,
    /// VA2/VC1 本机 127.0.0.1 调试入口鉴权门禁与测试时间窗存储。
    pub debug_gate: Arc<debug_api::DebugApiGate>,
    pub bench_windows: debug_api::BenchWindowStore,
    pub bench_window_changed: tokio::sync::Notify,
}

impl Shared {
    pub(crate) fn card_style(&self) -> ScreenCardStyle {
        self.screen_card_style
            .lock()
            .map(|g| *g)
            .unwrap_or(ScreenCardStyle::MossHud)
    }

    /// 异步落库设备快照并按 `(device_id, kind)` 单调序号串行防乱序覆盖，绝不阻塞 2-worker Tokio 音频线程。
    pub(crate) fn schedule_device_snapshot(
        &self,
        device_id: &str,
        kind: &'static str,
        payload: String,
    ) {
        let seq = self
            .snapshot_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        let registry = self.registry.clone();
        let gate = self.snapshot_write_gate.clone();
        let device = device_id.to_owned();
        let run = move || {
            let mut latest = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let key = (device.clone(), kind);
            if latest.get(&key).copied().is_some_and(|prev| seq < prev) {
                return;
            }
            latest.insert(key, seq);
            registry.device_snapshot(&device, kind, &payload);
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(run);
        } else {
            run();
        }
    }

    /// 取指定设备当前活跃会话（严格匹配 `device_requests` 当前登记的 `session_id`），
    /// 避免设备重启断网后旧半开连接阴影覆盖新连接，或在新连接正常断开后“复活”旧半开僵尸连接。
    pub(crate) fn active_session(&self, device_id: &str) -> Option<state::SessionView> {
        let sid = self
            .device_requests
            .lock()
            .ok()
            .and_then(|map| map.get(device_id).map(|h| h.session_id.clone()))?;
        self.state
            .session_by_id(&sid)
            .filter(|s| s.device_id == device_id)
    }

    /// 每台在线设备只保留一条当前活跃会话（规则与 [`Self::active_session`] 一致，按 `device_id` 升序）。
    pub(crate) fn active_device_sessions(&self) -> Vec<state::SessionView> {
        self.sync_bench_windows();
        let mut handles: Vec<(String, String)> = self
            .device_requests
            .lock()
            .map(|map| {
                map.iter()
                    .map(|(dev, h)| (dev.clone(), h.session_id.clone()))
                    .collect()
            })
            .unwrap_or_default();
        handles.sort_by(|a, b| a.0.cmp(&b.0));
        self.state.sessions_for_handles(&handles)
    }

    /// VC1 同步检查调试令牌过期、调试开关状态与时间窗到期，到期立即恢复普通轮次并记录事件。
    pub(crate) fn sync_bench_windows(&self) {
        if self.debug_gate.check_expiry() {
            self.close_all_bench_windows("调试令牌已失效");
            return;
        }
        if !self.debug_gate.snapshot().enabled && !self.bench_windows.list_active().is_empty() {
            self.close_all_bench_windows("调试开关已关闭");
            return;
        }
        for expired in self.bench_windows.expire_due() {
            self.state.set_bench_window(&expired.device_id, None);
            self.state.event_in_flow(
                EventRecord::info(
                    DiagCategory::Config,
                    format!(
                        "测试时间窗已关闭（到达最长时长）：设备 {} 已恢复普通轮次",
                        expired.device_id
                    ),
                )
                .source(DiagSource::Moss),
                session::device_flow(&expired.device_id),
            );
        }
    }

    pub(crate) fn is_bench_window_active(&self, device_id: &str) -> bool {
        self.sync_bench_windows();
        self.bench_windows.active_for(device_id).is_some()
    }

    pub(crate) fn bench_window_deadline_for(
        &self,
        device_id: &str,
    ) -> Option<tokio::time::Instant> {
        let win_deadline = self.bench_windows.next_deadline_for(device_id)?;
        let token_deadline = self
            .debug_gate
            .remaining_until_expiry()
            .map(|rem| tokio::time::Instant::now() + rem);
        Some(match token_deadline {
            Some(tok) => win_deadline.min(tok),
            None => win_deadline,
        })
    }

    pub(crate) fn open_bench_window(
        &self,
        device_id: &str,
        max_minutes: u32,
    ) -> Result<BenchWindowView, InjectTurnError> {
        self.sync_bench_windows();
        if !self.debug_gate.is_enabled_valid() {
            return Err(InjectTurnError {
                code: "debug_disabled".into(),
                http_status: 403,
                message: "本机 AI 调试注入开关未开启或令牌已失效，无法打开测试时间窗".into(),
            });
        }
        if max_minutes == 0 || max_minutes > MAX_BENCH_WINDOW_MINUTES {
            return Err(InjectTurnError::invalid(format!(
                "max_minutes 须为 1 ~ {MAX_BENCH_WINDOW_MINUTES} 分钟"
            )));
        }
        let Some(session) = self.active_session(device_id) else {
            return Err(InjectTurnError::offline(format!(
                "设备 {device_id} 当前不在线"
            )));
        };
        let view = self
            .bench_windows
            .open(device_id, &session.session_id, max_minutes);
        self.state.set_bench_window(device_id, Some(view.clone()));
        self.bench_window_changed.notify_waiters();
        self.state.event_in_flow(
            EventRecord::info(
                DiagCategory::Config,
                format!(
                    "测试时间窗已打开（最长 {} 分钟）：设备 {} 的全部轮次将按模拟处理，物理动作已锁定",
                    view.max_minutes, device_id
                ),
            )
            .source(DiagSource::Moss),
            session::device_flow(device_id),
        );
        Ok(view)
    }

    #[cfg(test)]
    pub(crate) fn open_bench_window_for_test(
        &self,
        device_id: &str,
        ttl: Duration,
    ) -> Result<BenchWindowView, InjectTurnError> {
        self.sync_bench_windows();
        if !self.debug_gate.is_enabled_valid() {
            return Err(InjectTurnError {
                code: "debug_disabled".into(),
                http_status: 403,
                message: "调试开关未开启".into(),
            });
        }
        let Some(session) = self.active_session(device_id) else {
            return Err(InjectTurnError::offline(format!(
                "设备 {device_id} 当前不在线"
            )));
        };
        let view = self
            .bench_windows
            .open_with_duration(device_id, &session.session_id, ttl);
        self.state.set_bench_window(device_id, Some(view.clone()));
        self.bench_window_changed.notify_waiters();
        Ok(view)
    }

    pub(crate) fn close_bench_window(&self, device_id: &str, reason: &str) -> bool {
        let Some(closed) = self.bench_windows.close(device_id) else {
            self.state.set_bench_window(device_id, None);
            return false;
        };
        self.state.set_bench_window(&closed.device_id, None);
        self.bench_window_changed.notify_waiters();
        self.state.event_in_flow(
            EventRecord::info(
                DiagCategory::Config,
                format!(
                    "测试时间窗已关闭（{reason}）：设备 {} 已恢复普通轮次",
                    closed.device_id
                ),
            )
            .source(DiagSource::Moss),
            session::device_flow(&closed.device_id),
        );
        true
    }

    pub(crate) fn close_bench_window_for_session(
        &self,
        device_id: &str,
        session_id: &str,
        reason: &str,
    ) -> bool {
        let Some(closed) = self.bench_windows.close_if_session(device_id, session_id) else {
            return false;
        };
        self.state.set_bench_window(&closed.device_id, None);
        self.bench_window_changed.notify_waiters();
        self.state.event_in_flow(
            EventRecord::info(
                DiagCategory::Config,
                format!(
                    "测试时间窗已关闭（{reason}）：设备 {} 已恢复普通轮次",
                    closed.device_id
                ),
            )
            .source(DiagSource::Moss),
            session::device_flow(&closed.device_id),
        );
        true
    }

    pub(crate) fn close_all_bench_windows(&self, reason: &str) -> usize {
        let closed = self.bench_windows.close_all();
        let count = closed.len();
        for view in closed {
            self.state.set_bench_window(&view.device_id, None);
            self.state.event_in_flow(
                EventRecord::info(
                    DiagCategory::Config,
                    format!(
                        "测试时间窗已关闭（{reason}）：设备 {} 已恢复普通轮次",
                        view.device_id
                    ),
                )
                .source(DiagSource::Moss),
                session::device_flow(&view.device_id),
            );
        }
        if count > 0 {
            self.bench_window_changed.notify_waiters();
        }
        count
    }

    /// VC2 异步在测试时间窗内按设备上行帧节奏注入数字音频，用于与喇叭外放对照。
    pub(crate) async fn inject_audio_async(
        &self,
        device_id: &str,
        pcm: Vec<i16>,
        sample_rate: u32,
        frame_ms: u32,
        lead_silence_ms: u32,
        tail_silence_ms: u32,
    ) -> Result<InjectTurnAck, InjectTurnError> {
        if !self.is_bench_window_active(device_id) {
            return Err(InjectTurnError {
                code: "bench_window_required".into(),
                http_status: 403,
                message: "数字音频注入仅允许在已打开的测试时间窗内使用".into(),
            });
        }
        if pcm.is_empty() {
            return Err(InjectTurnError::invalid("数字音频不能为空"));
        }
        let sender = {
            let map = self.device_requests.lock().map_err(|_| {
                InjectTurnError::conflict("session_lock_failed", "设备会话表锁失败")
            })?;
            let Some(handle) = map.get(device_id) else {
                return Err(InjectTurnError::offline(format!(
                    "设备 {device_id} 当前不在线"
                )));
            };
            handle.sender.clone()
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if sender
            .send(DeviceRequest::InjectAudio {
                pcm,
                sample_rate,
                frame_ms,
                lead_silence_ms,
                tail_silence_ms,
                reply: reply_tx,
            })
            .is_err()
        {
            return Err(InjectTurnError::offline(format!(
                "设备 {device_id} 的会话刚刚结束，数字音频注入未送达"
            )));
        }
        match tokio::time::timeout(Duration::from_secs(45), reply_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(InjectTurnError::offline(format!(
                "设备 {device_id} 会话在数字音频注入完成前断开"
            ))),
            Err(_) => Err(InjectTurnError::conflict(
                "inject_audio_timeout",
                "等待数字音频注入触发轮次超时",
            )),
        }
    }

    /// 按每台设备的当前活跃会话聚合主状态与音量（避免重启残留的半开旧连接卡死顶栏脉冲）。
    pub(crate) fn active_aggregate_pulse(&self) -> (Option<String>, u8) {
        let active = self.active_device_sessions();
        state::LiveState::aggregate_pulse_of(active.iter())
    }

    /// VA1 异步向指定在线设备注入一段文字，等待会话同步受理或返回明确忙碌/离线错误。
    pub(crate) async fn inject_text_async(
        &self,
        device_id: &str,
        text: String,
        simulated: bool,
    ) -> Result<InjectTurnAck, InjectTurnError> {
        let trimmed = text.trim();
        if trimmed.is_empty()
            || trimmed.chars().count() > 500
            || trimmed.chars().any(char::is_control)
        {
            return Err(InjectTurnError::invalid(
                "注入文字须为 1~500 字且不含控制字符",
            ));
        }
        let sender = {
            let map = self.device_requests.lock().map_err(|_| {
                InjectTurnError::conflict("session_lock_failed", "设备会话表锁失败")
            })?;
            let Some(handle) = map.get(device_id) else {
                return Err(InjectTurnError::offline(format!(
                    "设备 {device_id} 当前不在线"
                )));
            };
            handle.sender.clone()
        };
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if sender
            .send(DeviceRequest::InjectText {
                text: trimmed.to_owned(),
                simulated,
                reply: reply_tx,
            })
            .is_err()
        {
            return Err(InjectTurnError::offline(format!(
                "设备 {device_id} 的会话刚刚结束，注入未送达"
            )));
        }
        match tokio::time::timeout(Duration::from_secs(5), reply_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(InjectTurnError::offline(format!(
                "设备 {device_id} 会话在受理注入前断开"
            ))),
            Err(_) => Err(InjectTurnError::conflict(
                "inject_timeout",
                "等待设备会话受理注入超时，请稍后再试",
            )),
        }
    }
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
/// 内置语音服务专用 Tokio 线程名，供同步宿主入口检查调用上下文。
pub const VOICE_SERVER_RUNTIME_THREAD_NAME: &str = "moss-voice";

/// 运行中的内置语音服务。丢弃或调用 [`stop`](Self::stop) 时关闭。
pub struct VoiceServer {
    runtime: Option<tokio::runtime::Runtime>,
    shutdown: tokio::sync::watch::Sender<bool>,
    shared: Arc<Shared>,
    ws_port: u16,
    http_port: u16,
    mdns: Option<mdns_sd::ServiceDaemon>,
    debug_listener: std::sync::Mutex<Option<(u16, tokio::sync::watch::Sender<bool>)>>,
}

fn bind(ip: IpAddr, port: u16, label: &str) -> Result<std::net::TcpListener, String> {
    let listener = std::net::TcpListener::bind(SocketAddr::new(ip, port)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            format!("{label}端口 {port} 已被占用（可能有外部 xiaozhi-server 正在运行），请先停止它或在设置中换端口")
        } else {
            format!("{label}端口 {port} 无法监听：{error}")
        }
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("{label}端口设置失败：{error}"))?;
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
        // n71 常连模式端口开不了时不影响主服务：记一条事件，OTA 照旧只下发 WebSocket
        let mut mqtt_error = None;
        let mqtt_sockets = match config.mqtt {
            Some(mqtt) if mqtt.port == 8883 => {
                mqtt_error = Some("MQTT 端口不能用 8883（小智固件会改走加密连接）".to_owned());
                None
            }
            Some(mqtt) => match (
                bind(config.bind, mqtt.port, "常连（MQTT）"),
                std::net::UdpSocket::bind(SocketAddr::new(config.bind, mqtt.udp_port)),
            ) {
                (Ok(tcp), Ok(udp)) if udp.set_nonblocking(true).is_ok() => Some((tcp, udp)),
                (Err(error), _) => {
                    mqtt_error = Some(error);
                    None
                }
                (_, Err(error)) => {
                    mqtt_error = Some(format!(
                        "常连语音 UDP 端口 {} 无法监听：{error}",
                        mqtt.udp_port
                    ));
                    None
                }
                _ => {
                    mqtt_error = Some("常连语音 UDP 端口设置失败".to_owned());
                    None
                }
            },
            None => None,
        };
        let mqtt_ports = match &mqtt_sockets {
            Some((tcp, udp)) => Some(MqttConfig {
                port: tcp.local_addr().map_err(|e| e.to_string())?.port(),
                udp_port: udp.local_addr().map_err(|e| e.to_string())?.port(),
            }),
            None => None,
        };
        let ws_port = ws_listener.local_addr().map_err(|e| e.to_string())?.port();
        let http_port = http_listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .port();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name(VOICE_SERVER_RUNTIME_THREAD_NAME)
            .enable_all()
            .build()
            .map_err(|error| format!("创建语音服务运行时失败：{error}"))?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Shared {
            config: config.clone(),
            ws_port,
            http_port,
            mqtt_ports,
            vision_quotas: Arc::new(vision::VisionQuotas::default()),
            vision_photos: Arc::new(vision::VisionPhotos::default()),
            camera_preview: Arc::new(vision::CameraPreviewStore::default()),
            preview_store: Arc::new(preview_push::PreviewStore::default()),
            assets_store: wake_assets::AssetsStore::default(),
            glyph_fonts: Arc::new(glyph_font::GlyphFontService::new(
                config.full_font_dir.clone(),
            )),
            screen_card_style: std::sync::Mutex::new(config.screen_card_style),
            signer: TokenSigner::new(secret),
            registry,
            state: state::LiveState::with_log_and_epoch(
                config.diagnostics.clone(),
                config.debug_epoch.clone(),
            ),
            shutdown: shutdown_rx,
            job_announcements: jobs::AnnounceLog::default(),
            device_requests: Default::default(),
            snapshot_seq: std::sync::atomic::AtomicU64::new(0),
            snapshot_write_gate: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            ws_connections: Arc::new(tokio::sync::Semaphore::new(64)),
            http_requests: Arc::new(tokio::sync::Semaphore::new(64)),
            firmware_downloads: Arc::new(tokio::sync::Semaphore::new(2)),
            debug_gate: Arc::new(debug_api::DebugApiGate::new()),
            bench_windows: debug_api::BenchWindowStore::new(),
            bench_window_changed: tokio::sync::Notify::new(),
        });
        shared.state.event(
            EventRecord::from(format!(
                "内置语音服务已启动：设备连接端口 {ws_port}，OTA 端口 {http_port}"
            ))
            .source(DiagSource::Moss),
        );
        let ws_app = session::router(shared.clone());
        let http_limit = shared.http_requests.clone();
        let http_app = ota::router(shared.clone())
            .merge(vision::router(shared.clone()))
            .merge(firmware::router(shared.clone()))
            .merge(wake_assets::router(shared.clone()))
            .merge(preview_push::router(shared.clone()))
            .layer(axum::middleware::from_fn(
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let limit = http_limit.clone();
                    async move {
                        let Ok(_permit) = limit.try_acquire_owned() else {
                            return axum::response::IntoResponse::into_response((
                                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                "并发请求过多，请稍后重试",
                            ));
                        };
                        next.run(req).await
                    }
                },
            ));
        let mut stop_ws = shutdown_tx.subscribe();
        let mut stop_http = shutdown_tx.subscribe();
        // 不用 block_on：调用方可能处在另一个 tokio 运行时的阻塞线程里（Tauri 命令），嵌套 block_on 会 panic
        let events = shared.clone();
        runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(ws_listener) {
                Ok(listener) => {
                    let _ = axum::serve(
                        listener,
                        ws_app.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .with_graceful_shutdown(async move {
                        let _ = stop_ws.wait_for(|stop| *stop).await;
                    })
                    .await;
                }
                Err(error) => events.state.event(
                    EventRecord::error(
                        DiagCategory::Network,
                        "设备连接端口启动失败，小智连不上 MOSS",
                    )
                    .hint("端口可能被别的程序占用：关掉占用的程序或在设置里换端口后重启内置服务")
                    .detail(error.to_string()),
                ),
            }
        });
        let events = shared.clone();
        runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(http_listener) {
                Ok(listener) => {
                    let _ = axum::serve(
                        listener,
                        http_app.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .with_graceful_shutdown(async move {
                        let _ = stop_http.wait_for(|stop| *stop).await;
                    })
                    .await;
                }
                Err(error) => events.state.event(
                    EventRecord::error(
                        DiagCategory::Network,
                        "OTA 端口启动失败，小智开机拿不到连接地址",
                    )
                    .hint("端口可能被别的程序占用：关掉占用的程序或在设置里换端口后重启内置服务")
                    .detail(error.to_string()),
                ),
            }
        });
        if let Some(error) = mqtt_error {
            shared.state.event(
                EventRecord::error(
                    DiagCategory::Network,
                    "常连模式没能启动，设备照旧用唤醒后连接",
                )
                .source(DiagSource::Moss)
                .hint("端口被占用或被系统拒绝：在设置里换常连端口后重启内置服务")
                .detail(&error),
            );
        }
        if let (Some((tcp, udp)), Some(ports)) = (mqtt_sockets, mqtt_ports) {
            shared.state.event(format!(
                "常连模式已开启：MQTT 端口 {}，语音 UDP 端口 {}；设备重启一次后改用常连",
                ports.port, ports.udp_port
            ));
            let events = shared.clone();
            let mqtt_shared = shared.clone();
            let stop_mqtt = shutdown_tx.subscribe();
            let stop_udp = shutdown_tx.subscribe();
            runtime.spawn(async move {
                match (
                    tokio::net::TcpListener::from_std(tcp),
                    tokio::net::UdpSocket::from_std(udp),
                ) {
                    (Ok(listener), Ok(udp)) => {
                        let hub = mqtt::udp::UdpHub::new(udp);
                        tokio::spawn(hub.clone().run(stop_udp));
                        mqtt::serve(mqtt_shared, listener, hub, stop_mqtt).await;
                    }
                    (Err(error), _) | (_, Err(error)) => events.state.event(
                        EventRecord::error(DiagCategory::Network, "常连模式端口启动失败")
                            .hint("在设置里换常连端口后重启内置服务")
                            .detail(error.to_string()),
                    ),
                }
            });
        }
        let mdns = if config.bind.is_unspecified() {
            ota::start_mdns_responder(http_port, ws_port)
        } else {
            None
        };
        Ok(Self {
            runtime: Some(runtime),
            shutdown: shutdown_tx,
            shared,
            ws_port,
            http_port,
            mdns,
            debug_listener: std::sync::Mutex::new(None),
        })
    }

    pub fn ws_port(&self) -> u16 {
        self.ws_port
    }

    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    /// n71 常连模式实际监听的端口；没开或没开成为 `None`。
    pub fn mqtt_ports(&self) -> Option<MqttConfig> {
        self.shared.mqtt_ports
    }

    pub fn status(&self) -> ServerStatusView {
        ServerStatusView {
            running: self.runtime.is_some(),
            mode: self.shared.config.mode,
            ws_port: self.ws_port,
            http_port: self.http_port,
            mqtt_port: self.shared.mqtt_ports.map(|p| p.port),
            udp_port: self.shared.mqtt_ports.map(|p| p.udp_port),
            sessions: self.shared.state.sessions(),
            events: self.shared.state.events(),
            events_dropped: self.shared.state.event_drops(),
            timings: self.shared.state.timings(),
            debug_enabled: self.shared.state.debug_enabled(),
            debug_traces: self.shared.state.debug_traces(),
        }
    }

    /// 轻量查询当前是否有设备正处于活跃语音交互（正在说话拾音 `level > 0`、思考推理或语音播报）。
    /// 不拷贝事件日志与对话追踪记录，供后台预热线程低开销让出 CPU。
    pub fn has_busy_voice_session(&self) -> bool {
        if self.runtime.is_none() {
            return false;
        }
        self.shared.active_device_sessions().iter().any(|r| {
            r.state == "thinking"
                || r.state == "speaking"
                || (r.state == "listening" && r.level > 0)
        })
    }

    /// 向一台已连接的设备发起即时请求（刷新设备信息 / 重启设备）。
    ///
    /// 设备不在线、未声明设备端 MCP 或会话刚断开时同步返回错误；请求入队后异步发往设备，
    /// `self.get_device_status` 的结果经 [`DeviceRegistry::device_snapshot`] 写回。
    pub fn device_request(&self, device_id: &str, request: DeviceRequest) -> Result<(), String> {
        if self.runtime.is_none() {
            return Err("内置语音服务未运行".into());
        }
        let sender = {
            let map = self
                .shared
                .device_requests
                .lock()
                .map_err(|_| "设备会话表锁失败".to_owned())?;
            let Some(handle) = map.get(device_id) else {
                return Err(format!("设备 {device_id} 当前不在线"));
            };
            if !handle.supports_mcp
                && !matches!(
                    request,
                    DeviceRequest::PreviewScreen { .. }
                        | DeviceRequest::ShowScene { .. }
                        | DeviceRequest::InjectText { .. }
                        | DeviceRequest::InjectAudio { .. }
                )
            {
                return Err(format!("设备 {device_id} 未启用设备端 MCP，无法执行该操作"));
            }
            handle.sender.clone()
        };
        if sender.send(request).is_err() {
            let message = format!("设备 {device_id} 的会话刚刚结束，请求未送达");
            self.shared.state.event(
                EventRecord::warn(DiagCategory::Device, message.clone())
                    .hint("设备刚断开：等它重新连上后再试"),
            );
            return Err(message);
        }
        Ok(())
    }

    /// 每台在线设备的当前活跃会话（避免重启残留的半开旧连接遮蔽新连接状态）。
    pub fn active_device_sessions(&self) -> Vec<SessionView> {
        self.shared.active_device_sessions()
    }

    /// 热更新小智全量动态补字分片目录（运行期卸载旧目录字库并重下到新主目录后立即生效）。
    pub fn set_full_font_dir(&self, full_font_dir: Option<std::path::PathBuf>) {
        self.shared.glyph_fonts.set_full_dir(full_font_dir);
    }

    /// 向已启用设备端 MCP 但尚未完成 `tools/list` 的在线会话发起一次非阻塞工具清单同步。
    pub fn sync_device_tools(&self) {
        if self.runtime.is_none() {
            return;
        }
        for s in self.shared.active_device_sessions() {
            if s.device_mcp_enabled && !s.device_tools_ready {
                let _ = self.device_request(&s.device_id, DeviceRequest::SyncTools);
            }
        }
    }

    /// n61：暂存唤醒词资源包并让这台在线设备去下载（设备随后重启、自己写入资源分区）。
    ///
    /// 只在设备当前活跃会话上报了 user-only 工具 `self.assets.set_download_url` 时发送；
    /// 调用方（私密窗口）负责逐台明确确认。
    pub fn install_wake_assets(&self, device_id: &str, package: Vec<u8>) -> Result<(), String> {
        let supported = self.shared.active_session(device_id).is_some_and(|s| {
            s.device_tools
                .iter()
                .any(|t| t == "self.assets.set_download_url")
        });
        if !supported {
            return Err(format!(
                "设备 {device_id} 不在线，或固件不支持网络更新资源包（需要小智固件 2.0.2 以上、v2 分区表）"
            ));
        }
        let token = self.shared.assets_store.stage(device_id, package)?;
        self.device_request(device_id, DeviceRequest::InstallAssets { token })
    }

    /// n70 能刷全屏画风包的在线设备返回其屏幕宽高：彩屏（`self.screen.get_info` 没报单色）且报了分辨率。
    pub fn screen_pack_size(&self, device_id: &str) -> Option<(u32, u32)> {
        self.shared
            .active_session(device_id)
            .filter(|s| s.screen_monochrome != Some(true))
            .and_then(|s| s.screen_width.zip(s.screen_height))
    }

    /// n81 返回在线设备已知的屏幕与字体规格 `(preferred_font, screen_size, monochrome)`，供推送资源包选内置字库。
    pub fn device_font_context(
        &self,
        device_id: &str,
    ) -> (Option<(u32, u8)>, Option<(u32, u32)>, bool) {
        let Some(s) = self.shared.active_session(device_id) else {
            return (None, None, false);
        };
        (
            s.text_font_size.zip(s.text_font_bpp),
            s.screen_width.zip(s.screen_height),
            s.screen_monochrome == Some(true),
        )
    }

    /// 桌面端「摄像头」标签页向指定在线设备发起一次抓拍：签发 15 秒一次性票据并触发 `self.camera.take_photo`。
    pub fn request_camera_capture(&self, device_id: &str) -> Result<u64, String> {
        if self.runtime.is_none() {
            return Err("内置语音服务未运行".into());
        }
        if let Some(session) = self.shared.active_session(device_id)
            && !session.device_tools.is_empty()
            && !session
                .device_tools
                .iter()
                .any(|t| t == "self.camera.take_photo")
        {
            return Err("该小智设备固件未上报摄像头工具（self.camera.take_photo）".into());
        }
        let (ticket, seq) = self
            .shared
            .camera_preview
            .issue_ticket(device_id, unix_now())?;
        if let Err(err) = self.device_request(
            device_id,
            DeviceRequest::CapturePhoto {
                ticket: ticket.clone(),
            },
        ) {
            self.shared.camera_preview.revoke_ticket(device_id, &ticket);
            return Err(err);
        }
        Ok(seq)
    }

    /// 读取指定设备的最新摄像头内存帧与在线会话状态；`after_seq` 命中时只返回 `unchanged: true`，不重复拷贝图片字节。
    pub fn camera_frame_view(&self, device_id: &str, after_seq: Option<u64>) -> serde_json::Value {
        let session = self.shared.active_session(device_id);
        let online = session.is_some();
        let session_state = session.as_ref().map(|s| s.state.clone());
        let has_camera_tool = session.as_ref().and_then(|s| {
            if s.device_mcp_enabled && !s.device_tools_ready {
                None
            } else {
                Some(s.device_tools.iter().any(|t| t == "self.camera.take_photo"))
            }
        });
        let snap = self
            .shared
            .camera_preview
            .snapshot(device_id, after_seq, unix_now());
        serde_json::json!({
            "running": self.runtime.is_some(),
            "online": online,
            "session_state": session_state,
            "has_camera_tool": has_camera_tool,
            "unchanged": snap.unchanged,
            "pending": snap.pending,
            "last_error": snap.last_error,
            "error_seq": snap.error_seq,
            "frame_seq": snap.frame_seq,
            "frame": snap.frame,
        })
    }

    /// 清除指定设备的摄像头内存帧、待处理票据与最近错误（撤销设备或手动清空时调用）。
    pub fn clear_camera_frame(&self, device_id: &str) {
        self.shared.camera_preview.clear_device(device_id);
    }

    /// 移除设备时清理服务端内存中绑定该设备的摄像头缓存、待接进对话的照片与待下载唤醒词/画风包（n134）。
    pub fn clear_device_state(&self, device_id: &str) {
        self.shared.close_bench_window(device_id, "设备已移除");
        self.shared.camera_preview.clear_device(device_id);
        let _ = self.shared.vision_photos.take(device_id);
        self.shared.assets_store.clear_device(device_id);
    }

    /// 动态开关仅内存对话排障流（无需重启服务；关闭时立即清空内存缓冲）。
    pub fn set_debug_enabled(&self, enabled: bool) {
        self.shared.state.set_debug_enabled(enabled);
    }
    pub fn clear_debug_traces(&self) {
        self.shared.state.clear_debug_traces();
    }

    /// 热切换小智彩屏卡片画风（无需重启 WebSocket 服务，在线设备不断连立即生效）。
    pub fn set_screen_card_style(&self, style: ScreenCardStyle) {
        if let Ok(mut guard) = self.shared.screen_card_style.lock() {
            *guard = style;
        }
    }

    pub fn screen_card_style(&self) -> ScreenCardStyle {
        self.shared.card_style()
    }

    /// 将指定场景展示卡（按当前或指定画风：`MossHud` 太空舱 / `GoggleBot` 小机器人）投送到在线小智屏幕上试看。
    pub fn push_scene_card(
        &self,
        pose: BotPose,
        style_override: Option<ScreenCardStyle>,
        target_device: Option<&str>,
    ) -> Result<usize, String> {
        if self.runtime.is_none() {
            return Err("内置语音服务未运行".into());
        }
        let active_style = style_override.unwrap_or_else(|| self.shared.card_style());
        let summary = match active_style {
            ScreenCardStyle::MossHud => format!("MOSS太空舱 · {}", pose.moss_hud_label_zh()),
            ScreenCardStyle::GoggleBot => format!("小MOSS护目镜机器人 · {}", pose.label_zh()),
        };
        let sessions = self.shared.active_device_sessions();
        // n70 已刷全屏画风包的设备直接切全屏场景；没刷的照旧推半屏试看卡
        let request_for = |device_id: &str| -> Result<DeviceRequest, String> {
            let active = sessions.iter().find(|s| s.device_id == device_id);
            if active.is_some_and(|s| s.screen_pack) {
                return Ok(DeviceRequest::ShowScene {
                    scene: pose,
                    style: active_style,
                });
            }
            let screen_size = active.and_then(|s| s.screen_width.zip(s.screen_height));
            let (cw, ch) = preview_push::optimal_canvas_for_screen(screen_size);
            let png = preview_push::make_scene_showcase_png(pose, active_style, 1, cw, ch)?;
            Ok(DeviceRequest::PreviewScreen {
                token: Some(self.shared.preview_store.stage_prebuilt_png(png)?),
                summary: summary.clone(),
            })
        };
        if let Some(device_id) = target_device {
            self.device_request(device_id, request_for(device_id)?)?;
            return Ok(1);
        }
        let mut pushed = 0usize;
        for device_id in sessions.iter().map(|s| s.device_id.clone()) {
            if self
                .device_request(&device_id, request_for(&device_id)?)
                .is_ok()
            {
                pushed += 1;
            }
        }
        Ok(pushed)
    }

    /// 带底部 5×7 ASCII 尺寸/功率标尺的自适应预览推送入口。
    pub fn push_preview_adaptive_with_ruler(
        &self,
        source_bytes: Option<&[u8]>,
        summary: &str,
        ruler_ascii: Option<&str>,
        target_device: Option<&str>,
    ) -> Result<usize, String> {
        if self.runtime.is_none() {
            return Err("内置语音服务未运行".into());
        }
        let clean_summary: String = summary
            .replace(['\r', '\n'], " ")
            .trim()
            .chars()
            .take(96)
            .collect();
        let mut staged_by_canvas: std::collections::HashMap<(u32, u32), String> =
            std::collections::HashMap::new();
        let mut stage_for_device = |device_id: &str| -> Result<Option<String>, String> {
            let Some(bytes) = source_bytes.filter(|b| !b.is_empty()) else {
                return Ok(None);
            };
            let screen_size = self
                .shared
                .active_session(device_id)
                .and_then(|s| s.screen_width.zip(s.screen_height));
            let (cw, ch) = preview_push::optimal_canvas_for_screen(screen_size);
            if let Some(tok) = staged_by_canvas.get(&(cw, ch)) {
                return Ok(Some(tok.clone()));
            }
            let tok = self.shared.preview_store.stage_thumbnail_for_style(
                bytes,
                cw,
                ch,
                ruler_ascii,
                self.shared.card_style(),
            )?;
            staged_by_canvas.insert((cw, ch), tok.clone());
            Ok(Some(tok))
        };

        if let Some(device_id) = target_device {
            let token = stage_for_device(device_id)?;
            self.device_request(
                device_id,
                DeviceRequest::PreviewScreen {
                    token,
                    summary: clean_summary,
                },
            )?;
            return Ok(1);
        }
        let online_devices: Vec<String> = self
            .shared
            .active_device_sessions()
            .into_iter()
            .map(|s| s.device_id)
            .collect();
        let mut pushed = 0usize;
        for device_id in online_devices {
            let token = stage_for_device(&device_id)?;
            if self
                .device_request(
                    &device_id,
                    DeviceRequest::PreviewScreen {
                        token,
                        summary: clean_summary.clone(),
                    },
                )
                .is_ok()
            {
                pushed += 1;
            }
        }
        Ok(pushed)
    }

    /// 后台异步预热高频固定话术与慢工具过渡垫音缓存（不阻塞启动，服务关闭时自动中止）。
    pub fn prewarm_speech_cache(&self) {
        let (Some(runtime), Some(conversation)) = (
            self.runtime.as_ref(),
            self.shared.config.conversation.clone(),
        ) else {
            return;
        };
        let mut stop = self.shutdown.subscribe();
        runtime.spawn(async move {
            tokio::select! {
                _ = conversation.prewarm_cache() => {}
                _ = stop.wait_for(|stopped| *stopped) => {}
            }
        });
    }

    /// 清空固定话术缓存，并可选在后台按新音色重新预热（n131 本地克隆音色热更换时调用）。
    pub fn invalidate_speech_cache(&self, rewarm: bool) {
        if let Some(conversation) = self.shared.config.conversation.as_ref() {
            conversation.invalidate_speech_cache();
        }
        if rewarm {
            self.prewarm_speech_cache();
        }
    }

    /// VA1 同步向指定在线设备注入一段文字（供桌面 UI 与测试调用；不阻塞或死锁 Tokio 运行时线程）。
    pub fn inject_text(
        &self,
        device_id: &str,
        text: &str,
        simulated: bool,
    ) -> Result<InjectTurnAck, InjectTurnError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Err(InjectTurnError::conflict(
                "voice_builtin_stopped",
                "内置语音服务未运行",
            ));
        };
        let shared = self.shared.clone();
        let dev = device_id.to_owned();
        let txt = text.to_owned();
        let handle = runtime.handle().clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            handle.spawn(async move {
                let res = shared.inject_text_async(&dev, txt, simulated).await;
                let _ = tx.send(res);
            });
            rx.recv().unwrap_or_else(|_| {
                Err(InjectTurnError::conflict(
                    "inject_aborted",
                    "文字注入任务已中止",
                ))
            })
        } else {
            handle.block_on(async move { shared.inject_text_async(&dev, txt, simulated).await })
        }
    }

    /// VA3 查询某一轮的完整联调明细。
    pub fn turn_detail(&self, turn_id: &str) -> Option<TurnDetail> {
        self.shared.state.turn_detail(turn_id)
    }

    /// VC3 按设备查询最近的联调明细（新到旧）。
    pub fn turn_details_for_device(&self, device_id: &str, limit: usize) -> Vec<TurnDetail> {
        self.shared.state.turn_details_for_device(device_id, limit)
    }

    /// VC1 打开指定设备的测试时间窗。
    pub fn open_bench_window(
        &self,
        device_id: &str,
        max_minutes: u32,
    ) -> Result<BenchWindowView, InjectTurnError> {
        self.shared.open_bench_window(device_id, max_minutes)
    }

    /// VC1 立即关闭指定设备的测试时间窗（桌面“立即结束测试”按钮或脚本调用）。
    pub fn close_bench_window(&self, device_id: &str, reason: &str) -> bool {
        self.shared.close_bench_window(device_id, reason)
    }

    /// VC1 当前全部处于打开状态的测试时间窗。
    pub fn bench_windows(&self) -> Vec<BenchWindowView> {
        self.shared.sync_bench_windows();
        self.shared.bench_windows.list_active()
    }

    /// VA2 #4 热启动或更新本机 `127.0.0.1` 调试 HTTP 入口（不断开已连接小智会话）；返回实际监听端口。
    pub fn enable_debug_api(
        &self,
        preferred_port: u16,
        token: String,
        issued_at: u64,
        expires_at: u64,
        on_expired: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<u16, String> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Err("内置语音服务未运行".into());
        };
        let shared_for_expiry = self.shared.clone();
        let wrapped_expired: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            shared_for_expiry.close_all_bench_windows("调试令牌已失效");
            if let Some(cb) = on_expired.as_ref() {
                cb();
            }
        });
        self.shared.debug_gate.set_on_expired(Some(wrapped_expired));
        self.shared.debug_gate.enable(token, issued_at, expires_at);
        self.shared.bench_window_changed.notify_waiters();
        let mut slot = self
            .debug_listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((port, _)) = slot.as_ref() {
            return Ok(*port);
        }
        let (std_listener, actual_port) = debug_api::bind_loopback_listener(preferred_port)?;
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        let mut server_stop = self.shutdown.subscribe();
        let app = debug_api::router(debug_api::DebugApiState {
            shared: self.shared.clone(),
            gate: self.shared.debug_gate.clone(),
            trace_control: self.shared.config.debug_process_trace.clone(),
            active_laser_job: self.shared.config.active_laser_job.clone(),
        });
        let events = self.shared.clone();
        runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(std_listener) {
                Ok(listener) => {
                    let _ = axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .with_graceful_shutdown(async move {
                        tokio::select! {
                            _ = stop_rx.wait_for(|stop| *stop) => {}
                            _ = server_stop.wait_for(|stop| *stop) => {}
                        }
                    })
                    .await;
                }
                Err(error) => events.state.event(
                    EventRecord::error(DiagCategory::Network, "本机调试入口启动失败")
                        .source(DiagSource::Moss)
                        .detail(error.to_string()),
                ),
            }
        });
        *slot = Some((actual_port, stop_tx));
        self.shared.state.event(
            EventRecord::info(
                DiagCategory::Config,
                format!("本机 AI 调试注入入口已开启（仅监听 127.0.0.1:{actual_port}）"),
            )
            .source(DiagSource::Moss),
        );
        Ok(actual_port)
    }

    /// VA2 #4 立即作废令牌、关闭全部测试时间窗并关闭本机调试入口（不断开已连接小智会话）。
    pub fn disable_debug_api(&self) {
        self.shared.debug_gate.disable();
        self.shared.close_all_bench_windows("调试开关已关闭");
        if let Ok(mut slot) = self.debug_listener.lock()
            && let Some((_, stop_tx)) = slot.take()
        {
            let _ = stop_tx.send(true);
        }
    }

    /// 本机调试入口当前监听的回环端口；未开启或未运行时为 `None`。
    pub fn debug_api_port(&self) -> Option<u16> {
        self.shared.sync_bench_windows();
        if !self.shared.debug_gate.snapshot().enabled {
            return None;
        }
        self.debug_listener
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|(port, _)| *port))
    }

    /// 通知所有会话结束并关闭运行时（最多等 2 秒）。
    pub fn stop(mut self) {
        self.shutdown_now();
    }

    fn shutdown_now(&mut self) {
        self.disable_debug_api();
        self.shared.camera_preview.clear_all();
        if let Some(daemon) = self.mdns.take() {
            let _ = daemon.shutdown();
        }
        let _ = self.shutdown.send(true);
        if let Some(runtime) = self.runtime.take() {
            // 调用方可能处在别的 tokio 运行时的异步任务里（如应用内更新收尾），在那里直接
            // shutdown_timeout 会 panic；换到普通线程关闭并等待，仍最多 2 秒（2026-11-08）。
            let _ = std::thread::Builder::new()
                .name("moss-voice-stop".into())
                .spawn(move || runtime.shutdown_timeout(Duration::from_secs(2)))
                .map(|handle| handle.join());
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
