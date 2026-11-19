//! 设备 WebSocket 会话（每台设备一个，修复海门全进程共用一个策略实例的问题）。
//!
//! 流程：读请求头（`Device-Id` / `Client-Id` / `Authorization` / `Protocol-Version`，缺失时读 URL 参数）→
//! 登记来访 → 等设备 hello（10 秒）→ 回 hello → 未通过授权的发 `alert` 后断开 → 已授权的进入会话循环。
//!
//! 会话循环用单个 `select!` 同时等待设备消息、播放节奏定时器与空闲超时（NV2）：
//! - 上行 Opus 解码为 PCM；自动拾音模式由 VAD 判断说完（静音 200 ms），手动模式以 `listen stop` 为准；
//! - 一段话结束后交给应答：回声模式重采样后播回；对话模式（NV3）交给对话任务（识别 → 大模型 → 逐句合成），
//!   每句音频带文字标记推入节奏泵，播到这句时发 `sentence_start`；
//! - 下行经 [`PlaybackPump`] 编码：`tts start` 后先留出保护间隔等设备切到说话状态，再按“设备端缓冲余量”发送
//!   （余量不足就连发补到目标，之后按绝对时间轴每 60 ms 一帧）；供不上时不发帧，由设备缓冲吸收
//!   （节奏参考海门 `ws.rs`，`513d9e6`，MIT）；
//! - 播放期间收到 `abort` 或新的 `listen start` 立即停止（打断）；
//! - 配置了流式识别（NV7）时，自动模式在 VAD 确认开始说话、手动模式在 `listen start` 时就建立识别连接，
//!   之后每帧（转为 16 kHz）随收随送；说完时把进行中的识别交给对话任务。重新拾音或打断时丢弃（连接随之关闭）。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use moss_xiaozhi_proto::{
    AudioParams, ClientMessage, ListenState, ServerMessage, TextFontCapability, TtsState,
    decode_frame, encode_frame,
};
use tokio::time::Instant;

use moss_voice_providers::{AsrStream, ChatMessage};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::audio::{
    EnergyVad, OpusDecoder, OpusEncoder, PlaybackPump, PumpTick, Resampler, Vad, VadEvent,
    supported_rate,
};
use crate::conversation::{
    Conversation, DeviceCall, DeviceTools, ToolContext, TurnEvent, TurnGate, TurnInput,
    append_history, spawn_turn_gated,
};
use crate::music::playback::{MusicController, MusicProgress};
use crate::state::{DiagCategory, EventLevel, EventRecord, SessionView};
use crate::{DeviceStatus, DeviceVisit, ServerMode, Shared, unix_now};

mod connect;
mod device;
mod heard;
mod input;
mod job_watch;
mod lifecycle;
mod listen;
mod playback;
mod screen;
mod turn;

pub use connect::router;
use connect::send_json;
pub(crate) use connect::{SessionStart, random_id, serve_session};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_MS: u64 = 60;
/// 开始播放时先连发的帧数（设备端抖动缓冲）
/// 设备端缓冲余量目标（帧）：余量低于它且有音频时连发补足。官方固件解码队列上限 20 帧，
/// 超出即丢，这里留足余地；官方服务端为 3 帧，多出的 2 帧用来吸收 Wi-Fi 与流式合成的抖动。
const LEAD_FRAMES: u32 = 5;
/// `tts start` 到首帧音频的保护间隔：官方固件收到 start 后要经主任务调度才切到说话状态并清空解码器，
/// 此前到达的音频帧会被丢弃（首字被吞、设备端缓冲余量归零）。
const TTS_START_GUARD: Duration = Duration::from_millis(180);
/// 上游供不上或还在攒启动缓冲时的重检间隔
const STARVED_POLL: Duration = Duration::from_millis(20);
/// 判停器的静音时长：到这里就把这段话交给识别与大模型抢跑（须与 `VadConfig` / `VadParams` 默认值一致）
const BASE_SILENCE_MS: u64 = 250;
/// 抢跑等待到点时用户还在出声，最多再顺延这么久
const MAX_HOLD_EXTENSION: Duration = Duration::from_millis(1500);
/// 最后一帧发出后，在预计设备播完的时刻之上再等的余量，之后才发 `tts stop`
/// （常连模式下 stop 走 MQTT、音频走 UDP，不保证先后；官方服务端同样等队列播完再多等 2 帧）
const DRAIN_MARGIN: Duration = Duration::from_millis(120);
/// 一份查完、等播报的后台查询结果（n100）。
struct BackgroundReport {
    question: String,
    results: String,
    done_at: Instant,
    /// 已经开始播报过几次（开口前被打断会重来，最多 [`MAX_REPORT_ATTEMPTS`] 次）
    attempts: u8,
}

const MAX_REPORT_ATTEMPTS: u8 = 2;
/// 用户安静这么久（且最近没有别的交互）才主动播报，免得和用户刚开口撞上
const QUIET_BEFORE_REPORT: Duration = Duration::from_millis(1500);
/// 一段话说完后继续把尾音送进判停流（只为收命令词结果）这么久
const KWS_TAIL: Duration = Duration::from_millis(800);
/// 同时在后台查的慢查询上限（再多就中止最早的）
const MAX_BACKGROUND: usize = 3;
/// 查完的结果最多等这么久没找到空闲播报就放弃（话题早过去了）
const BACKGROUND_REPORT_TTL: Duration = Duration::from_secs(300);
/// 一段话最长 30 秒，超过即按说完处理
const MAX_UTTERANCE_MS: usize = 30_000;
/// 自动模式下保留语音开始前 1 秒，避免吞掉开头的字：本地判停确认“开始说话”实测要晚 0.4–0.9 秒
/// （Silero / TEN-VAD 都要先攒够 250 ms 语音），原来的 300 ms 会丢掉开头一两个字
const PREROLL_MS: usize = 1_000;

enum Phase {
    Idle,
    Listening(Listening),
    /// 对话模式：一段话已交给对话任务，还没有可播放的音频
    Thinking,
    /// 播放中：`next` 为下一次发送检查的时间，`not_before` 为 `tts start` 保护间隔的结束时刻；
    /// `anchor + sent × 60 ms` 是设备按实时播放把已发音频播完的时刻，减去当前时间即设备端缓冲余量
    Speaking {
        next: Instant,
        not_before: Instant,
        anchor: Instant,
        sent: u32,
        /// 上游已播完：到这个时刻（预计设备播完 + 余量）再发 `tts stop`
        stop_at: Option<Instant>,
    },
}

impl Phase {
    /// 刚发出 `tts start`：首帧不早于保护间隔结束。
    fn speaking_after_tts_start() -> Self {
        let not_before = Instant::now() + TTS_START_GUARD;
        Phase::Speaking {
            next: not_before,
            not_before,
            anchor: not_before,
            sent: 0,
            stop_at: None,
        }
    }

    /// 暂停后接着说：设备一直在说话状态，不需要保护间隔；时间轴从此刻重新对齐。
    fn speaking_resumed() -> Self {
        let now = Instant::now();
        Phase::Speaking {
            next: now,
            not_before: now,
            anchor: now,
            sent: 0,
            stop_at: None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Listening(_) => "listening",
            Phase::Thinking => "thinking",
            Phase::Speaking { .. } => "speaking",
        }
    }
}

struct Listening {
    manual: bool,
    /// 自动模式下 VAD 已确认开始说话
    speaking: bool,
    /// 本轮确认开始说话的时刻（手动模式为进入拾音时刻，自动模式按尾部有效起音跨度回推）
    speech_began_at: Option<Instant>,
    /// 本轮已累计的有效说话采样数（不含自动模式起音前的纯静音 preroll）
    voiced_samples: usize,
    /// 最近一次向小智屏幕推送实时中间识别字幕的时刻
    last_interim_stt_at: Option<Instant>,
    utterance: Vec<i16>,
    preroll: VecDeque<i16>,
    /// 进行中的流式识别（NV7）与上行 → 16 kHz 的重采样（上行已是 16 kHz 时为空）
    stream: Option<(AsrStream, Option<Resampler>)>,
    /// 最新流式中间识别文字（用于半截话防抢话判定与路由预热）
    last_interim: String,
    /// 防抖中的流式预热任务（新 interim 到达或离开拾音时立即中止旧任务）
    prewarm_task: Option<tokio::task::JoinHandle<()>>,
    /// 每轮拾音最多允许一次半截话续听延长，防止背景人声无限挂起
    hold_used: bool,
    /// 半截话续听宽限窗截止时刻（自首次静音判停时刻起算 1.2 秒）
    hold_deadline: Option<Instant>,
    /// 续听否定或到期时，等待此前已入队 VAD 帧排空的目标序号屏障
    finalize_after_vad_seq: Option<u64>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        if let Some(task) = self.prewarm_task.take() {
            task.abort();
        }
    }
}

struct PendingHoldCheck {
    paused_at: Instant,
    task: tokio::task::JoinHandle<bool>,
}

impl Drop for PendingHoldCheck {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Listening {
    fn feed(&mut self, pcm: &[i16]) {
        if let Some((stream, resampler)) = self.stream.as_mut() {
            match resampler {
                Some(resampler) => stream.feed(&resampler.process(pcm)),
                None => stream.feed(pcm),
            }
        }
    }
}

/// 流式识别要求的上行采样率。
const STREAM_ASR_RATE: u32 = 16_000;
/// 半截话未说完时的单次最长续听宽限窗（1.2 秒硬上限）。
const UNFINISHED_HOLD_WINDOW: Duration = Duration::from_millis(1200);
// 判断结果至少要比续听窗口早 300ms 回来，续听才有意义（见 `jev::MAX_HOLD_TIMEOUT`）。
const _: () =
    assert!(UNFINISHED_HOLD_WINDOW.as_millis() >= crate::jev::MAX_HOLD_TIMEOUT.as_millis() + 300);
/// 流式中间结果停顿防抖预热阈值：连续流利说话时 0 请求，停顿满 120ms 才预热 1 次。
const INTERIM_PREWARM_DEBOUNCE: Duration = Duration::from_millis(120);
/// 说话期间首次向小智屏幕推送实时识别字幕的最短开口墙钟门槛（对齐离线 SenseVoice 320ms 首认节拍，留 20ms 余量）。
const INTERIM_STT_FIRST_ELAPSED: Duration = Duration::from_millis(300);
/// 说话期间两次向小智屏幕推送实时字幕的最小冷却间隔（比离线 500ms 采样步长留出 100ms 推理/网络抖动余量，防吞拍）。
const INTERIM_STT_COOLDOWN: Duration = Duration::from_millis(400);
/// 说话有效音频至少累计满 320ms（对齐 OFFLINE_INTERIM_FIRST_SAMPLES）后才向屏幕推中间字幕，避免极短气音或瞬发短帧抢先刷屏。
const INTERIM_STT_MIN_AUDIO_MS: usize = 320;
/// 从 `preroll + 当前帧` 末尾往前扫描 20 ms 窗口，测算首次 `SpeechStart` 时已包含的实际有效语音采样数
///（排除起音前的安静底噪，兼容确认较晚的本地 Silero / TEN-VAD 与能量判停，容忍词内 <=80 ms 塞音闭塞间隙）。
fn trailing_voiced_samples(
    samples: &[i16],
    sample_rate: u32,
    vad: &crate::audio::VadConfig,
) -> usize {
    let win = (sample_rate as usize * 20 / 1000).max(1);
    if samples.len() < win {
        return samples.len();
    }
    let mut rms_windows = Vec::with_capacity(samples.len() / win);
    let mut end = samples.len();
    while end >= win {
        let slice = &samples[end - win..end];
        let sum_sq: f64 = slice.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        rms_windows.push((sum_sq / slice.len() as f64).sqrt());
        end -= win;
    }
    let Some(noise_floor) = rms_windows
        .iter()
        .copied()
        .filter(|&r| r < vad.start_rms)
        .reduce(f64::min)
    else {
        return samples.len();
    };
    let threshold = (noise_floor * vad.keep_over_noise)
        .max(vad.keep_rms * 0.5)
        .min(vad.start_rms);
    let mut last_voiced_idx: Option<usize> = None;
    let mut quiet_run = 0usize;
    for (idx, &r) in rms_windows.iter().enumerate() {
        if r >= threshold {
            last_voiced_idx = Some(idx);
            quiet_run = 0;
        } else {
            quiet_run += 1;
            if quiet_run > 4 {
                break;
            }
        }
    }
    last_voiced_idx.map_or(0, |idx| ((idx + 1) * win).min(samples.len()))
}

/// 一段话结束的原因。
#[derive(Clone, Copy)]
enum UtteranceEnd {
    Silence,
    ListenStop,
    TooLong,
}

/// 思考期间能量检测听到的“接着说”候选：开头音频（含确认期间陆续到的帧）与确认截止时刻。
struct FollowProbe {
    head: Vec<i16>,
    until: Instant,
}

/// 候选交给本地判停模型确认的最长时间：人声通常 200–400 ms 内确认，气泵咔嗒、敲击始终确认不了。
const FOLLOW_CONFIRM: Duration = Duration::from_millis(700);

impl UtteranceEnd {
    fn label(self) -> &'static str {
        match self {
            Self::Silence => "静音判停",
            Self::ListenStop => "设备停止拾音",
            Self::TooLong => "超过 30 秒",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnKind {
    /// 回答用户的一段话
    Answer,
    /// 唤醒回应
    Greeting,
    /// 空闲断开前的再见
    /// 空闲断开前的再见
    Goodbye,
    /// 加工结束播报：固定话术，不经过大模型，可被插话打断
    Notice,
}

/// 正在进行的一轮对话。丢弃时中止对话任务（打断、断开连接）。
struct VerifiedPersonHint {
    scope: crate::person_context::PersonScope,
    verified_at: Instant,
    consecutive_short: u8,
}

const SHORT_UTTERANCE_CONTINUITY_WINDOW: Duration = Duration::from_secs(30);
const MAX_CONSECUTIVE_SHORT_UTTERANCES: u8 = 2;

struct ActiveTurn {
    person_scope: Option<crate::person_context::PersonScope>,
    person_verified: bool,
    timing: Option<crate::TurnTiming>,
    recording_offset: u64,
    kind: TurnKind,
    /// 开始时刻（说完话的时刻），用于记录首句延迟
    started: Instant,
    handle: tokio::task::JoinHandle<()>,
    events: UnboundedReceiver<TurnEvent>,
    /// 声纹按人记忆开启时，本轮携带的视觉照片上下文（打断时随已识别说话人写入其个人历史）
    pending_photo: Option<ChatMessage>,
    /// 识别出的用户原话（打断时记入历史）
    user: Option<String>,
    /// 尾音语义判停预测的说完概率（`0.0..=1.0`）
    smart_turn_prob: Option<f32>,
    /// 已向设备发出 sentence_start 的句子（不是仅合成或排队的句子）。
    spoken: Vec<String>,
    /// `spoken` 每段从这一轮第几帧开始播（一一对应），打断时据此估算用户实际听到哪个字
    spoken_at: Vec<u32>,
    /// 这一轮已发给设备的音频帧数
    frames_sent: u32,
    /// 与对话任务共享的命令词结论：这句话的命令词结果晚到时会话补写（n101）
    command_hint: Option<
        std::sync::Arc<std::sync::Mutex<Option<moss_voice_providers::local::CommandKeyword>>>,
    >,
    /// 后台查询结果的主动播报轮次（没有用户原话；思考中用户开口照样能接管，n100）
    report: bool,
    debug_capture_epoch: Option<u64>,
    debug_raw_asr: Option<String>,
    debug_corrected_asr: Option<String>,
    debug_tools: Vec<crate::DebugToolTrace>,
    debug_text_reply: Vec<String>,
    /// 生成完成不等于播放完成；正常播完后才提交完整历史。
    pending_messages: Vec<ChatMessage>,
    /// 当前句子的重采样器（流式合成的后续分段沿用，保证分段之间连续）
    resampler: Option<Resampler>,
    audio_started: bool,
    done: bool,
    close: bool,
    /// 唤醒回应期间设备发来的 `listen start`：回应播完后按这个模式开始拾音
    listen_after: Option<bool>,
    /// 这一轮已开始执行工具（之后不再因用户接着说话而整轮重来）
    tools_started: bool,
    /// 与对话任务共享的工具执行闩（没有闸门的轮次为空）
    tool_latch: Option<std::sync::Arc<crate::conversation::ToolLatch>>,
    /// 这一轮是抢跑开始的（识别出文字后要按句子是否说完决定还要等多久才开口）
    speculative: bool,
    /// 流式中间结果触发的急停快路径轮次：禁止被用户同一句话的后续尾音通过自然插话自杀打断
    fast_stop: bool,
    /// 抢跑闸门的发送端：置 `true` 前对话任务不执行大模型发起的工具调用
    release: Option<tokio::sync::watch::Sender<bool>>,
    /// 放行时交给对话任务的最新历史（见 [`TurnGate::fresh_history`]）
    fresh_history: std::sync::Arc<std::sync::Mutex<Option<Vec<ChatMessage>>>>,
    /// 抢跑中：到这个时刻（且用户没在出声）才认定这句话说完、开始处理对话任务的输出；
    /// 第二项是最晚放行时刻。期间任务照常识别、请求大模型、合成，但不开口、不执行工具。
    hold: Option<(Instant, Instant)>,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 设备自带 MCP（内置语音技术书 §5.3，NV4）：握手、分页取工具清单、转发工具调用。
struct DeviceMcp {
    next_id: u64,
    /// 等设备回包的请求（发出时刻、种类）。2026-11-05：有界并按 [`MCP_PENDING_TTL`] 过期，
    /// 不回包的请求不再一直留到会话结束。
    pending: HashMap<u64, (Instant, McpPending)>,
    tools: Vec<Value>,
    /// 已请求的 tools/list 页数
    pages: usize,
    init_sent: bool,
    initialized: bool,
    ready: bool,
    vision_url_sent: bool,
    vision_token_issued_at: Option<Instant>,
    screen_monochrome: Option<bool>,
    screen_size: Option<(u32, u32)>,
    deferred_requests: Vec<crate::DeviceRequest>,
    calls_tx: UnboundedSender<DeviceCall>,
    calls_rx: UnboundedReceiver<DeviceCall>,
}

enum McpPending {
    Initialize,
    ToolsList,
    Call(oneshot::Sender<String>),
    /// 桌面端请求 `self.get_device_status`：结果不交给模型，直接写成设备快照
    Snapshot(&'static str),
    /// 上位机调参/预览推送：回包只检查是否报错记事件，不覆盖设备状态快照
    Quiet(&'static str),
    /// 连接握手后查询 `self.screen.get_info` 以识别单色 OLED (`monochrome: true`) vs 彩屏 LCD
    ScreenInfo,
    /// 桌面端「摄像头」标签页抓拍请求：检查设备侧拍照/上传是否报错并即时写入预览错误态
    CameraCapture {
        ticket: String,
    },
}

/// 设备工具清单最多取多少页（防止设备返回循环的游标）
const MAX_TOOL_PAGES: usize = 20;
/// 等设备 MCP 回包的最长时间：超时的请求从 `pending` 移除；工具调用的等待方随之收到“设备没有返回结果”。
const MCP_PENDING_TTL: Duration = Duration::from_secs(60);
/// 同时等回包的设备 MCP 请求上限；超出时丢弃最早的一条。
const MAX_MCP_PENDING: usize = 64;
/// 交给模型的设备工具结果最长字符数
const MAX_DEVICE_RESULT_CHARS: usize = 1500;

fn is_user_only_device_tool(name: &str) -> bool {
    matches!(
        name,
        "self.reboot"
            | "self.upgrade_firmware"
            | "self.get_system_info"
            | "self.screen.get_info"
            | "self.screen.snapshot"
            | "self.screen.preview_image"
            | "self.assets.set_download_url"
            | "self.system.reconfigure_wifi"
            | "self.disp.network"
    )
}

impl DeviceMcp {
    fn new() -> Self {
        let (calls_tx, calls_rx) = unbounded_channel();
        Self {
            next_id: 1,
            pending: HashMap::new(),
            tools: Vec::new(),
            pages: 0,
            init_sent: false,
            initialized: false,
            ready: false,
            vision_url_sent: false,
            vision_token_issued_at: None,
            screen_monochrome: None,
            screen_size: None,
            deferred_requests: Vec::new(),
            calls_tx,
            calls_rx,
        }
    }

    fn request(&mut self, method: &str, params: Value, pending: McpPending) -> Value {
        self.request_at(method, params, pending, Instant::now())
    }

    fn request_at(
        &mut self,
        method: &str,
        params: Value,
        pending: McpPending,
        now: Instant,
    ) -> Value {
        if matches!(pending, McpPending::ToolsList) {
            self.pages += 1;
        }
        self.prune_pending(now);
        while self.pending.len() >= MAX_MCP_PENDING {
            // id 单调递增：最小的就是最早发出的
            let Some(oldest) = self.pending.keys().min().copied() else {
                break;
            };
            self.pending.remove(&oldest);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, (now, pending));
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    /// 设备按设计不回包的请求（如 `self.reboot`，设备随即重启断开）：分配 id 但不登记等待。
    fn request_untracked(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    /// 移除超过 [`MCP_PENDING_TTL`] 仍未回包的请求，以及等待方已放弃（对话任务结束）的工具调用。
    fn prune_pending(&mut self, now: Instant) {
        self.pending.retain(|_, (at, pending)| {
            now.saturating_duration_since(*at) < MCP_PENDING_TTL
                && !matches!(pending, McpPending::Call(reply) if reply.is_closed())
        });
    }

    fn model_tools(&self) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| !is_user_only_device_tool(name))
            })
            .cloned()
            .collect()
    }

    fn all_tool_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for item in &self.tools {
            if let Some(name) = item.get("name").and_then(Value::as_str)
                && !names.iter().any(|n| n == name)
            {
                names.push(name.to_owned());
            }
        }
        names
    }

    fn has_tool(&self, name: &str) -> bool {
        self.tools
            .iter()
            .any(|item| item.get("name").and_then(Value::as_str) == Some(name))
    }

    fn tool_set(&self) -> Option<DeviceTools> {
        if !self.ready {
            return None;
        }
        let visible = self.model_tools();
        (!visible.is_empty()).then(|| DeviceTools::new(&visible, self.calls_tx.clone()))
    }
}

/// 设备 `tools/call` 响应 → 交给模型的文字。
pub(crate) fn device_result_text(payload: &Value) -> String {
    if let Some(error) = payload.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return format!("设备工具出错：{message}");
    }
    let result = payload.get("result").cloned().unwrap_or(Value::Null);
    let texts: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let body = if texts.is_empty() {
        result.to_string()
    } else {
        texts.join("\n")
    };
    let body: String = body.chars().take(MAX_DEVICE_RESULT_CHARS).collect();
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        format!("设备工具执行失败：{body}")
    } else {
        body
    }
}

/// 监听流式 ASR 中间识别文字（用于说话期屏幕实时字幕、整句急停快路由与未说完预热）；仅整句匹配停止词可越过最终识别结果。
async fn next_listening_interim(phase: &mut Phase) -> String {
    match phase {
        Phase::Listening(listening) => match listening.stream.as_mut() {
            Some((stream, _)) => stream.next_interim().await,
            None => std::future::pending().await,
        },
        _ => std::future::pending().await,
    }
}

/// 等待非阻塞的半截话续听判定结果（不阻塞 socket/VAD/打断事件轮询）。
/// 思考中那阵判停模型没认出人声的声音，识别一下是不是“停”（审查修正：短促的“停”可能短于模型判开口的最短语音）。
/// 返回识别出的停止说法；不是停止说法或识别失败为 `None`。没有在查时永远等待。
async fn next_stop_check(
    slot: &mut Option<tokio::task::JoinHandle<Option<String>>>,
) -> Option<String> {
    match slot.as_mut() {
        Some(task) => {
            let text = task.await.ok().flatten();
            *slot = None;
            text
        }
        None => std::future::pending().await,
    }
}

async fn next_listening_hold_check(slot: &mut Option<PendingHoldCheck>) -> (Instant, bool) {
    match slot.as_mut() {
        Some(pending) => {
            let at = pending.paused_at;
            let held = (&mut pending.task).await.unwrap_or(false);
            *slot = None;
            (at, held)
        }
        None => std::future::pending().await,
    }
}

/// 等对话任务发来的下一个设备工具调用；没有设备 MCP 时永远等待（用于 `select!`）。
async fn next_device_call(device: &mut Option<DeviceMcp>) -> Option<DeviceCall> {
    match device {
        Some(device) => device.calls_rx.recv().await,
        None => std::future::pending().await,
    }
}

/// 等对话任务的下一个事件；没有进行中的对话时永远等待（用于 `select!`）。
/// 一帧 PCM 的均方根幅度（满幅 32767）。
fn frame_rms(pcm: &[i16]) -> f64 {
    if pcm.is_empty() {
        return 0.0;
    }
    (pcm.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt()
}

/// 后台查询有一个查完了：取出它的问题与结果（n100）。没有在查的时候永远等待。
async fn next_background_done(
    running: &mut Vec<crate::conversation::BackgroundQuery>,
) -> (
    String,
    Vec<(
        moss_voice_providers::ToolCall,
        crate::conversation::ToolOutcome,
    )>,
) {
    if running.is_empty() {
        return std::future::pending().await;
    }
    std::future::poll_fn(|cx| {
        for index in 0..running.len() {
            if let std::task::Poll::Ready(result) =
                std::future::Future::poll(std::pin::Pin::new(&mut running[index].handle), cx)
            {
                let mut done = running.remove(index);
                let question = std::mem::take(&mut done.question);
                return std::task::Poll::Ready((question, result.unwrap_or_default()));
            }
        }
        std::task::Poll::Pending
    })
    .await
}

async fn next_turn_event(turn: &mut Option<ActiveTurn>) -> Option<TurnEvent> {
    match turn {
        Some(turn) if !turn.done && turn.hold.is_none() => turn.events.recv().await,
        _ => std::future::pending().await,
    }
}
struct PreviewHold {
    url: String,
    until: Instant,
    next_refresh: Instant,
}

struct MusicHud {
    title_ascii: String,
    frame: u8,
    next_refresh: Instant,
}

/// 副屏渲染、画风包、底栏字幕与动态字模状态（从 `Session` 67 个扁平字段中收敛）。
#[derive(Default)]
struct SessionScreenState {
    /// 已提示过“屏幕显示不出”的字：同一个字每次连接只提示一次
    glyph_gaps_reported: std::collections::BTreeSet<char>,
    /// 对话播报期间暂存的雕刻预览底部摘要（播完 `tts:stop` 后无声刷到 `bottom_bar_` 供用户核对）
    pending_oled_preview: Option<String>,
    /// 彩屏预览确认窗口内的 4 秒无声续期句柄（克服官方固件 5 秒自动隐藏预览计时器；用户开口即停）
    preview_hold: Option<PreviewHold>,
    /// 当前加工发送期间最近一次刷新的实时副屏 HUD 任务与百分比
    last_hud_workflow: Option<(String, u8)>,
    /// 副屏动态帧序号（用于驱动护目镜小机器人火花、光学红核与黑胶旋转）
    hud_frame: u8,
    /// 最近一次已在屏幕上提示过的待电脑审批摘要（避免重复刷底栏字幕）
    last_approval_summary: Option<String>,
    /// 完工庆祝 / 急停断点 5 秒徽章卡的单次重绘地址（不设续期，交由官方固件 5 秒自动淡出）
    one_shot_badge_url: Option<String>,
    /// 本地音乐播放期间的黑胶/听歌小机器人动态卡状态
    music_hud: Option<MusicHud>,
    /// n70 设备已刷 MOSS 全屏画风包（设备偏好 `screen_pack`）：对话与加工场景改发表情名，切换全屏动图
    screen_pack: bool,
    /// n70 常驻场景（雕刻 / 待批准 / 听歌 / 完工 / 急停）与叠在画面上的一行字
    screen_scene: Option<ScreenSceneState>,
    /// n70 最近一次发给设备的画风表情名（轮询拍内去重；设备状态切换后强制重发）
    last_screen_emotion: Option<String>,
    /// n70 正在雕刻的任务与它的刀路预览原图（每个任务只向宿主取一次）
    engrave_preview: Option<(String, Option<Arc<Vec<u8>>>)>,
    /// n70 本轮回复的表情场景（大模型的表情往往先于第一句音频到，开口时再显示）
    turn_scene: Option<crate::BotPose>,
    /// n81 设备在 `hello` 中声明的字库与 `glyph_push` 能力
    text_font_cap: Option<TextFontCapability>,
    /// 当前小智屏幕底栏最近一次发出的 `stt` 文本（最终识别文字未变化时免重复下发，避免重置固件跑马灯滚动）
    last_screen_stt: Option<String>,
}

/// 后台慢查询队列与空闲播报状态（n100）。
#[derive(Default)]
struct BackgroundQueryState {
    /// 转到后台还在查的只读慢查询（n100，最多 [`MAX_BACKGROUND`] 个）
    background: Vec<crate::conversation::BackgroundQuery>,
    /// 查完了、等空闲时播报的结果
    background_ready: VecDeque<BackgroundReport>,
    /// 正在播报的那份结果与它所在的轮次：开口前被用户的话取代就放回队列重来
    reporting: Option<(u64, BackgroundReport)>,
    /// 聆听中最近一次听到有响度的声音（后台结果等用户安静一会儿再播报）
    last_heard_loud: Option<Instant>,
}

/// 加工与 Agent 终态轮询及有界播报队列。
struct JobNoticeState {
    /// 加工结束播报：只读观察最近任务，发现“发送中 → 终态”时在空闲时播报一次。
    jobs: crate::jobs::JobWatcher,
    next_job_poll: Instant,
    /// 同拍内可能同时结束多个加工/Agent 任务：一次空闲拍只播 1 条，其余暂存于此（上限 16）等下一拍轮到再播，防并发通知被吞。
    pending_finished: std::collections::VecDeque<crate::jobs::FinishedJob>,
    pending_agent_notices: std::collections::VecDeque<crate::jobs::AgentRunNotice>,
}

/// 拾音模式、打断、暂停恢复与续说检测状态。
struct ListenAndBargeState {
    hold_check: Option<PendingHoldCheck>,
    /// 这一段话里命令词检测模型听到的停止 / 退出命令词（交给这一轮对话纠正识别结果，n101）
    utterance_keyword: Option<moss_voice_providers::local::CommandKeyword>,
    /// 一段话说完后继续把尾音送进判停流到这个时刻：命令词模型要在词说完后再听一小段才出结果（n101）
    kws_tail_until: Option<Instant>,
    /// 思考中判停模型没认出人声的那阵声音正在识别是不是“停”
    stop_check: Option<tokio::task::JoinHandle<Option<String>>>,
    /// 最近一次 `listen start` 是否为手动模式
    listen_manual: bool,
    listen_realtime: bool,
    barge: crate::barge::BargeIn,
    playback_started: Option<Instant>,
    /// 兼容收到 tts stop 后再次发 listen start 的固件，不能清掉已补送的开头。
    barge_resumed: bool,
    /// 说话中听到插话而暂停的那一轮：等插话识别出来再决定——真有话就打断它，只是附和或没听清就接着说
    paused: Option<ActiveTurn>,
    /// 插话已识别出文字、正等大模型判断是不是对小智说的（期间原回答保持暂停，n100）
    paused_waits_for_model: bool,
    /// 思考期间“用户接着说”的检测器（比播放中的插话检测灵敏）
    follow: crate::barge::BargeIn,
    /// 用了本地判停模型时，能量检测的“接着说”候选要等模型也听到人声才算（2026-11-15 整链实测：
    /// 突发噪声下每次敲击都会放弃已备好的回答）
    follow_probe: Option<FollowProbe>,
    /// 下一轮由静音判停触发（不是设备 listen stop）：可以抢跑
    gate_next_turn: bool,
    /// 思考期间最近一次听到明显人声的时刻
    last_loud: Option<Instant>,
    /// 自动拾音时交给当前这一轮的那段录音：思考期间用户接着说话时，与后半句拼成完整的一段重新识别
    last_utterance: Option<Vec<i16>>,
}

struct Session<'a> {
    permission: Arc<std::sync::atomic::AtomicBool>,
    recording_started: Instant,
    recording_marks: BTreeMap<String, u64>,
    turn_number: u64,
    shared: &'a Shared,
    session_id: &'a str,
    device_id: &'a str,
    client_id: &'a str,
    version: u32,
    timestamp: u32,
    uplink_rate: u32,
    decoder: OpusDecoder,
    vad: Box<dyn Vad>,
    vad_worker: Option<crate::audio::worker::VadWorker>,
    pending_listen_stop: bool,
    music: Option<MusicController>,
    pump: PlaybackPump,
    decode_errors: u64,
    phase: Phase,
    /// 最近一次文本消息或说话的时间（自动模式下设备持续上传静音，静音帧不算活动）
    last_activity: Instant,
    listen: ListenAndBargeState,
    bg: BackgroundQueryState,
    screen: SessionScreenState,
    notices: JobNoticeState,
    conversation: Option<Arc<Conversation>>,
    history: Vec<ChatMessage>,
    turn: Option<ActiveTurn>,
    /// 个人模式下尚未提交到任何说话人历史的视觉照片上下文（被 Ignored / 暂停切回 / 思考合并 / 打断且未识别出原话时暂存到下一轮）
    carried_photo: Option<ChatMessage>,
    last_verified_person: Option<VerifiedPersonHint>,
    vision_host: Option<String>,
    vision_token: Option<String>,
    device_mcp: Option<DeviceMcp>,
    /// n71 常连模式（MQTT）：会话跟着控制通道一直在，一轮对话结束只关语音通道
    mqtt: bool,
    /// n71 常连模式下这一轮对话要结束（说完再见、用户说退出、空闲到期）：在会话循环里关语音通道
    pending_goodbye: Option<String>,
}

/// n70 完工 / 急停全屏画面停留多久（雕刻时的刀路卡最多再挡 5 秒，固件计时到了才露出底下的完工画面）。
const FINISHED_SCENE_HOLD: Duration = Duration::from_secs(10);

/// n70 全屏画风的常驻场景。
struct ScreenSceneState {
    scene: crate::BotPose,
    /// 叠在画面上的一行字（经 `stt` 显示在固件字幕位置）
    text: Option<String>,
    /// 到期自动回到聆听 / 待命（完工、急停徽章）
    until: Option<Instant>,
}

fn enqueue_bounded_notice<T>(
    queue: &mut std::collections::VecDeque<T>,
    item: T,
    mut same: impl FnMut(&T, &T) -> bool,
) {
    if queue.iter().any(|queued| same(queued, &item)) {
        return;
    }
    if queue.len() >= 16 {
        queue.pop_front();
    }
    queue.push_back(item);
}

#[cfg(test)]
mod device_mcp_pending_tests {
    use super::*;

    fn id(payload: &Value) -> u64 {
        payload["id"].as_u64().unwrap()
    }

    /// 2026-11-05：重启不登记等待；超时、等待方已放弃的请求被移除；同时等待的请求有上限。
    #[test]
    fn pending_requests_are_bounded_and_expire() {
        let mut device = DeviceMcp::new();
        let start = Instant::now();

        let reboot = device.request_untracked("tools/call", json!({"name": "self.reboot"}));
        assert!(
            !device.pending.contains_key(&id(&reboot)),
            "设备不回包的重启不应留在 pending"
        );

        let quiet = device.request_at(
            "tools/call",
            json!({}),
            McpPending::Quiet("self.audio_speaker.set_volume"),
            start,
        );
        let (reply, waiting) = oneshot::channel();
        let call = device.request_at("tools/call", json!({}), McpPending::Call(reply), start);
        let (dropped_reply, dropped) = oneshot::channel();
        let abandoned = device.request_at(
            "tools/call",
            json!({}),
            McpPending::Call(dropped_reply),
            start,
        );
        drop(dropped);
        device.prune_pending(start + Duration::from_secs(1));
        assert!(device.pending.contains_key(&id(&quiet)));
        assert!(device.pending.contains_key(&id(&call)));
        assert!(
            !device.pending.contains_key(&id(&abandoned)),
            "对话任务已放弃的调用不再等待"
        );

        device.prune_pending(start + MCP_PENDING_TTL);
        assert!(device.pending.is_empty(), "超时未回包的请求被移除");
        assert!(
            waiting.blocking_recv().is_err(),
            "超时的工具调用等待方立即得到“设备没有返回结果”"
        );

        let mut first = None;
        for _ in 0..MAX_MCP_PENDING + 10 {
            let payload = device.request_at("tools/call", json!({}), McpPending::Quiet("q"), start);
            first.get_or_insert(id(&payload));
        }
        assert_eq!(device.pending.len(), MAX_MCP_PENDING);
        assert!(
            !device.pending.contains_key(&first.unwrap()),
            "超出上限时丢弃最早的请求"
        );
    }

    #[test]
    fn enqueue_bounded_notice_dedups_and_caps_at_sixteen() {
        let mut q = std::collections::VecDeque::new();
        for idx in 0..20 {
            enqueue_bounded_notice(&mut q, idx, |a, b| a == b);
        }
        // 重复插入不会顶掉队头
        enqueue_bounded_notice(&mut q, 19, |a, b| a == b);
        assert_eq!(q.len(), 16);
        assert_eq!(q.front().copied(), Some(4));
        assert_eq!(q.back().copied(), Some(19));
    }
}
