//! 设备 WebSocket 会话（每台设备一个，修复海门全进程共用一个策略实例的问题）。
//!
//! 流程：读请求头（`Device-Id` / `Client-Id` / `Authorization` / `Protocol-Version`，缺失时读 URL 参数）→
//! 登记来访 → 等设备 hello（10 秒）→ 回 hello → 未通过授权的发 `alert` 后断开 → 已授权的进入会话循环。
//!
//! 会话循环用单个 `select!` 同时等待设备消息、播放节奏定时器与空闲超时（NV2）：
//! - 上行 Opus 解码为 PCM；自动拾音模式由 VAD 判断说完（静音 200 ms），手动模式以 `listen stop` 为准；
//! - 一段话结束后交给应答：回声模式重采样后播回；对话模式（NV3）交给对话任务（识别 → 大模型 → 逐句合成），
//!   每句音频带文字标记推入节奏泵，播到这句时发 `sentence_start`；
//! - 下行经 [`PlaybackPump`] 编码，先连发 3 帧预缓冲，之后按绝对时钟每 60 ms 发 1 帧，供不上时补静音帧
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
    AudioParams, ClientMessage, ListenState, ServerMessage, TtsState, decode_frame, encode_frame,
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
    Conversation, DeviceCall, DeviceTools, ToolContext, TurnEvent, TurnInput, append_history,
    spawn_turn,
};
use crate::music::playback::{MusicController, MusicProgress};
use crate::state::SessionView;
use crate::{DeviceStatus, DeviceVisit, ServerMode, Shared, unix_now};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const FRAME_MS: u64 = 60;
/// 开始播放时先连发的帧数（设备端抖动缓冲）
const PREBUFFER_FRAMES: usize = 3;
/// 一段话最长 30 秒，超过即按说完处理
const MAX_UTTERANCE_MS: usize = 30_000;
/// 自动模式下保留语音开始前 1 秒，避免吞掉开头的字：本地判停确认“开始说话”实测要晚 0.4–0.9 秒
/// （Silero / TEN-VAD 都要先攒够 250 ms 语音），原来的 300 ms 会丢掉开头一两个字
const PREROLL_MS: usize = 1_000;

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new().fallback(upgrade).with_state(shared)
}

struct Identity {
    device_id: Option<String>,
    client_id: Option<String>,
    authorization: Option<String>,
    protocol_version: Option<u32>,
    host: Option<String>,
    ip: String,
}

async fn upgrade(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let pick = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                query
                    .get(name)
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
            })
    };
    let identity = Identity {
        device_id: pick("device-id"),
        client_id: pick("client-id"),
        authorization: pick("authorization"),
        protocol_version: pick("protocol-version").and_then(|value| value.parse().ok()),
        host: headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::ota::host_without_port),
        ip: peer.ip().to_string(),
    };
    ws.max_message_size(64 * 1024)
        .on_upgrade(move |socket| run(shared, socket, identity))
}

fn random_id() -> String {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn send_json(socket: &mut WebSocket, message: &ServerMessage) -> bool {
    socket
        .send(Message::Text(message.to_text().into()))
        .await
        .is_ok()
}

/// 等设备 hello，返回其声明的协议版本、上行音频参数与是否支持设备 MCP。
async fn wait_hello(socket: &mut WebSocket) -> Option<(u32, AudioParams, bool)> {
    let deadline = Instant::now() + HELLO_TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.recv())
            .await
            .ok()??
            .ok()?;
        match message {
            Message::Text(text) => {
                if let Ok(ClientMessage::Hello {
                    version,
                    audio_params,
                    features,
                    ..
                }) = ClientMessage::parse(text.as_str())
                {
                    return Some((version, audio_params, features.mcp));
                }
            }
            Message::Close(_) => return None,
            _ => {}
        }
    }
}

async fn run(shared: Arc<Shared>, mut socket: WebSocket, identity: Identity) {
    let Some(device_id) = identity.device_id.clone() else {
        shared
            .state
            .event(format!("拒绝连接（{}）：缺少 Device-Id", identity.ip));
        let _ = socket
            .send(Message::Text("缺少 Device-Id，无法识别设备".into()))
            .await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let client_id = identity.client_id.clone().unwrap_or_default();
    let visit = DeviceVisit {
        device_id: device_id.clone(),
        client_id: client_id.clone(),
        model: String::new(),
        firmware_version: String::new(),
        ip: identity.ip.clone(),
    };
    let status = match shared.registry.seen(&visit) {
        Ok(status) => status,
        Err(error) => {
            shared
                .state
                .event(format!("登记设备 {device_id} 失败：{error}"));
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let token_ok = status == DeviceStatus::Allowed
        && identity
            .authorization
            .as_deref()
            .map(|token| {
                shared
                    .signer
                    .verify(token, &client_id, &device_id, unix_now())
            })
            .unwrap_or(false);
    let Some((hello_version, uplink, device_mcp)) = wait_hello(&mut socket).await else {
        shared
            .state
            .event(format!("设备 {device_id} 未在 10 秒内发送 hello，已断开"));
        return;
    };
    let version = if matches!(hello_version, 1..=3) {
        hello_version
    } else {
        identity.protocol_version.unwrap_or(1)
    };
    let session_id = random_id();
    let lease = if token_ok {
        let (registry, d, c, s) = (
            shared.registry.clone(),
            device_id.clone(),
            client_id.clone(),
            session_id.clone(),
        );
        match tokio::task::spawn_blocking(move || registry.open_session(&d, &c, &s)).await {
            Ok(Ok(allowed)) => Some(crate::SessionLease {
                allowed,
                registry: shared.registry.clone(),
                session: session_id.clone(),
            }),
            _ => {
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    } else {
        None
    };
    // 已授权的设备先建好会话（音频编解码器）并登记，再回 hello：设备收到 hello 时语音中心已能看到它
    let mut session = if token_ok {
        let uplink_rate = if uplink.format == "opus" && supported_rate(uplink.sample_rate) {
            uplink.sample_rate
        } else {
            shared.state.event(format!(
                "设备 {device_id} 声明的上行音频为 {} {} Hz，不支持，按 Opus 16000 Hz 处理",
                uplink.format, uplink.sample_rate
            ));
            16_000
        };
        match Session::new(
            &shared,
            &session_id,
            &device_id,
            version,
            uplink_rate,
            lease.as_ref().unwrap().allowed.clone(),
        ) {
            Ok(mut session) => {
                if device_mcp && session.conversation.is_some() {
                    session.device_mcp = Some(DeviceMcp::new());
                    session.vision_host = identity.host.clone();
                    session.vision_token = identity.authorization.clone().map(|value| {
                        value
                            .trim()
                            .strip_prefix("Bearer ")
                            .unwrap_or(value.trim())
                            .to_owned()
                    });
                }
                shared.state.open(SessionView {
                    session_id: session_id.clone(),
                    device_id: device_id.clone(),
                    ip: identity.ip.clone(),
                    protocol_version: version,
                    connected_at: unix_now(),
                    state: "idle".into(),
                });
                shared
                    .registry
                    .sessions_changed(shared.state.sessions().len());
                shared.state.event(format!(
                    "设备 {device_id} 已连接（帧协议 v{version}，上行 {uplink_rate} Hz，下行 {} Hz）",
                    shared.config.downlink_sample_rate
                ));
                Some(session)
            }
            Err(error) => {
                shared
                    .state
                    .event(format!("设备 {device_id} 的音频初始化失败：{error}"));
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    } else {
        None
    };
    let hello = ServerMessage::Hello {
        version,
        transport: "websocket".into(),
        session_id: session_id.clone(),
        audio_params: AudioParams::opus(shared.config.downlink_sample_rate, FRAME_MS as u32),
    };
    if !send_json(&mut socket, &hello).await {
        shared.state.close(&session_id);
        shared
            .registry
            .sessions_changed(shared.state.sessions().len());
        return;
    }
    let Some(session) = session.as_mut() else {
        let (status_text, message, log) = match status {
            DeviceStatus::Pending => (
                "等待允许",
                "请在电脑 MOSS 语音中心允许此设备，然后重启设备",
                "待允许，已提示用户在电脑上允许",
            ),
            DeviceStatus::Revoked => (
                "已撤销",
                "此设备已被撤销，请在电脑 MOSS 语音中心重新允许",
                "已撤销，拒绝连接",
            ),
            DeviceStatus::Allowed => (
                "需要重新授权",
                "请重启设备，从 MOSS 重新获取授权",
                "已允许但 token 缺失或无效（允许后尚未重启设备？），已提示重启",
            ),
        };
        shared.state.event(format!("设备 {device_id}：{log}"));
        let alert = ServerMessage::Alert {
            status: status_text.into(),
            message: message.into(),
            emotion: "sad".into(),
        };
        send_json(&mut socket, &alert).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let permission = lease.as_ref().unwrap().allowed.clone();
    let reason = tokio::select! {
        biased;
        _ = async {
            while permission.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } => "设备授权已撤销".to_owned(),
        reason = session.run(&mut socket) => reason,
    };
    // NV10: background summary uses only committed, bounded history and never delays disconnect.
    if permission.load(std::sync::atomic::Ordering::Acquire) {
        if let Some(conversation) = &session.conversation {
            if let Some(personal)=&conversation.personal_context {
                personal.session_closed(&ToolContext{device_id:device_id.clone(),session_id:session_id.clone()},conversation.llm.clone());
            }else if let Some(memory) = &conversation.memory {
                memory.queue_summary(&device_id, &session.history, conversation.llm.clone());
            }
            // NV19-03: independent of the NV10 summary; off unless the user enabled it.
            if let Some(extraction) = &conversation.context_extraction {
                extraction.session_closed(&ToolContext{device_id:device_id.clone(),session_id:session_id.clone()}, &session.history, conversation.llm.clone());
            }
        }
    }
    session.finish_timing("disconnected");
    session.turn = None;
    session.pump.clear();
    drop(lease);
    shared.state.close(&session_id);
    shared
        .registry
        .sessions_changed(shared.state.sessions().len());
    shared
        .state
        .event(format!("设备 {device_id} 已断开：{reason}"));
    let _ = socket.send(Message::Close(None)).await;
}

enum Phase {
    Idle,
    Listening(Listening),
    /// 对话模式：一段话已交给对话任务，还没有可播放的音频
    Thinking,
    /// 播放中：`next` 为下一拍的计划时间，`burst` 为开头还要连发的预缓冲帧数
    Speaking {
        next: Instant,
        burst: usize,
    },
}

struct Listening {
    manual: bool,
    /// 自动模式下 VAD 已确认开始说话
    speaking: bool,
    utterance: Vec<i16>,
    preroll: VecDeque<i16>,
    /// 进行中的流式识别（NV7）与上行 → 16 kHz 的重采样（上行已是 16 kHz 时为空）
    stream: Option<(AsrStream, Option<Resampler>)>,
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

/// 一段话结束的原因。
#[derive(Clone, Copy)]
enum UtteranceEnd {
    Silence,
    ListenStop,
    TooLong,
}

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
    Goodbye,
}

/// 正在进行的一轮对话。丢弃时中止对话任务（打断、断开连接）。
struct ActiveTurn {
    person_scope:Option<crate::person_context::PersonScope>,
    timing: Option<crate::TurnTiming>,
    recording_offset: u64,
    kind: TurnKind,
    /// 开始时刻（说完话的时刻），用于记录首句延迟
    started: Instant,
    handle: tokio::task::JoinHandle<()>,
    events: UnboundedReceiver<TurnEvent>,
    /// 识别出的用户原话（打断时记入历史）
    user: Option<String>,
    /// 已向设备发出 sentence_start 的句子（不是仅合成或排队的句子）。
    spoken: Vec<String>,
    /// 生成完成不等于播放完成；正常播完后才提交完整历史。
    pending_messages: Vec<ChatMessage>,
    /// 当前句子的重采样器（流式合成的后续分段沿用，保证分段之间连续）
    resampler: Option<Resampler>,
    audio_started: bool,
    done: bool,
    close: bool,
    /// 唤醒回应期间设备发来的 `listen start`：回应播完后按这个模式开始拾音
    listen_after: Option<bool>,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 设备自带 MCP（内置语音技术书 §5.3，NV4）：握手、分页取工具清单、转发工具调用。
struct DeviceMcp {
    next_id: u64,
    pending: HashMap<u64, McpPending>,
    tools: Vec<Value>,
    /// 已请求的 tools/list 页数
    pages: usize,
    ready: bool,
    calls_tx: UnboundedSender<DeviceCall>,
    calls_rx: UnboundedReceiver<DeviceCall>,
}

enum McpPending {
    Initialize,
    ToolsList,
    Call(oneshot::Sender<String>),
}

/// 设备工具清单最多取多少页（防止设备返回循环的游标）
const MAX_TOOL_PAGES: usize = 20;
/// 交给模型的设备工具结果最长字符数
const MAX_DEVICE_RESULT_CHARS: usize = 1500;

impl DeviceMcp {
    fn new() -> Self {
        let (calls_tx, calls_rx) = unbounded_channel();
        Self {
            next_id: 1,
            pending: HashMap::new(),
            tools: Vec::new(),
            pages: 0,
            ready: false,
            calls_tx,
            calls_rx,
        }
    }

    fn request(&mut self, method: &str, params: Value, pending: McpPending) -> Value {
        if matches!(pending, McpPending::ToolsList) {
            self.pages += 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, pending);
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn tool_set(&self) -> Option<DeviceTools> {
        (self.ready && !self.tools.is_empty())
            .then(|| DeviceTools::new(&self.tools, self.calls_tx.clone()))
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

/// 监听流式 ASR 完整中间假设。仅整句匹配停止词可越过最终识别结果。
async fn next_listening_interim(phase: &mut Phase) -> String {
    match phase {
        Phase::Listening(listening) => match listening.stream.as_mut() {
            Some((stream, _)) => stream.next_interim().await,
            None => std::future::pending().await,
        },
        _ => std::future::pending().await,
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
async fn next_turn_event(turn: &mut Option<ActiveTurn>) -> Option<TurnEvent> {
    match turn {
        Some(turn) if !turn.done => turn.events.recv().await,
        _ => std::future::pending().await,
    }
}

struct Session<'a> {
    permission: Arc<std::sync::atomic::AtomicBool>,
    recording_started: Instant,
    recording_marks: BTreeMap<String, u64>,
    turn_number: u64,
    shared: &'a Shared,
    session_id: &'a str,
    device_id: &'a str,
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
    /// 最近一次 `listen start` 是否为手动模式
    listen_manual: bool,
    listen_realtime: bool,
    barge: crate::barge::BargeIn,
    playback_started: Option<Instant>,
    /// 兼容收到 tts stop 后再次发 listen start 的固件，不能清掉已补送的开头。
    barge_resumed: bool,
    conversation: Option<Arc<Conversation>>,
    history: Vec<ChatMessage>,
    turn: Option<ActiveTurn>,
    vision_host: Option<String>,
    vision_token: Option<String>,
    device_mcp: Option<DeviceMcp>,
}

impl<'a> Session<'a> {
    fn finish_timing(&mut self, outcome: &str) {
        if let Some(turn) = self.turn.as_mut()
            && let Some(mut timing) = turn.timing.take()
        {
            timing.stages.insert(
                "turn_end".into(),
                turn.recording_offset
                    .saturating_add(turn.started.elapsed().as_millis() as u64),
            );
            timing.outcome = outcome.into();
            self.shared.state.finish_timing(timing);
        }
    }
    fn new(
        shared: &'a Shared,
        session_id: &'a str,
        device_id: &'a str,
        version: u32,
        uplink_rate: u32,
        permission: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self, String> {
        let downlink = shared.config.downlink_sample_rate;
        Ok(Self {
            recording_started: Instant::now(),
            permission: permission.clone(),
            recording_marks: BTreeMap::new(),
            turn_number: 0,
            shared,
            session_id,
            device_id,
            version,
            timestamp: 0,
            uplink_rate,
            decoder: OpusDecoder::new(uplink_rate)?,
            vad: Box::new(EnergyVad::new(uplink_rate, shared.config.vad.clone())),
            vad_worker: shared
                .config
                .vad_factory
                .as_ref()
                .map(|factory| {
                    crate::audio::worker::VadWorker::spawn(
                        factory.clone(),
                        uplink_rate,
                        shared.config.vad.clone(),
                    )
                })
                .transpose()?,
            pending_listen_stop: false,
            music: shared
                .config
                .music
                .as_ref()
                .map(|_| MusicController::new(permission.clone())),
            pump: PlaybackPump::new(
                OpusEncoder::new(downlink, FRAME_MS as u32)?,
                downlink,
                shared.config.playback_start_ms,
            ),
            decode_errors: 0,
            phase: Phase::Idle,
            last_activity: Instant::now(),
            listen_manual: false,
            listen_realtime: false,
            barge: crate::barge::BargeIn::new(
                uplink_rate,
                shared.config.barge_in_ms,
                &shared.config.vad,
            ),
            playback_started: None,
            barge_resumed: false,
            conversation: match shared.config.mode {
                ServerMode::Echo => None,
                ServerMode::Chat => shared.config.conversation.clone(),
            },
            history: Vec::new(),
            turn: None,
            vision_host: None,
            vision_token: None,
            device_mcp: None,
        })
    }

    fn set_state(&self, state: &str) {
        self.shared.state.set_state(self.session_id, state);
    }

    fn event(&self, text: String) {
        self.shared.state.event(text);
    }

    async fn tts(
        &mut self,
        socket: &mut WebSocket,
        state: TtsState,
        text: Option<String>,
    ) -> Result<(), String> {
        let message = ServerMessage::Tts {
            session_id: self.session_id.into(),
            state,
            text,
        };
        if send_json(socket, &message).await {
            Ok(())
        } else {
            Err("发送失败".into())
        }
    }

    async fn send_frame(&mut self, socket: &mut WebSocket, payload: &[u8]) -> Result<(), String> {
        let frame = encode_frame(self.version, payload, self.timestamp)
            .map_err(|error| error.to_string())?;
        self.timestamp = self.timestamp.wrapping_add(FRAME_MS as u32);
        socket
            .send(Message::Binary(frame.into()))
            .await
            .map_err(|_| "发送失败".to_owned())
    }

    fn samples(&self, ms: usize) -> usize {
        self.uplink_rate as usize * ms / 1000
    }

    /// 开始一段流式识别（没有配置流式识别时为空）。
    fn open_stream(&self) -> Option<(AsrStream, Option<Resampler>)> {
        let streaming = self.conversation.as_ref()?.streaming_asr.as_ref()?;
        let resampler = (self.uplink_rate != STREAM_ASR_RATE)
            .then(|| Resampler::new(self.uplink_rate, STREAM_ASR_RATE));
        Some((streaming.start(), resampler))
    }

    fn start_listening(&mut self, manual: bool) {
        self.barge_resumed = false;
        self.recording_started = Instant::now();
        self.recording_marks = BTreeMap::from([("recording_start".into(), 0)]);
        self.vad.reset();
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        self.pending_listen_stop = false;
        self.listen_manual = manual;
        // 手动模式按住即说：马上建立识别连接；自动模式等 VAD 确认开始说话（避免为环境声建连接）
        let stream = if manual { self.open_stream() } else { None };
        self.phase = Phase::Listening(Listening {
            manual,
            speaking: false,
            utterance: Vec::new(),
            preroll: VecDeque::new(),
            stream,
        });
        self.set_state("listening");
    }

    /// 处理一帧上行 PCM；一段话结束时返回它。
    fn hear(
        &mut self,
        pcm: &[i16],
        detected: Option<(Vec<VadEvent>, Option<String>)>,
    ) -> Option<(Vec<i16>, UtteranceEnd)> {
        let max = self.samples(MAX_UTTERANCE_MS);
        let preroll_max = self.samples(PREROLL_MS);
        let Phase::Listening(listening) = &mut self.phase else {
            return None;
        };
        let mut ended = false;
        let mut began = false;
        if listening.manual {
            listening.utterance.extend_from_slice(pcm);
            listening.feed(pcm);
            self.last_activity = Instant::now();
        } else {
            let was_speaking = listening.speaking;
            let (events, notice) = match detected {
                Some(value) => value,
                None => {
                    let events = self.vad.accept(pcm);
                    (events, self.vad.take_notice())
                }
            };
            if let Some(notice) = notice {
                self.shared.state.event(notice);
            }
            for event in events {
                match event {
                    VadEvent::SpeechStart => {
                        listening.speaking = true;
                        self.recording_marks
                            .entry("vad_start".into())
                            .or_insert(self.recording_started.elapsed().as_millis() as u64);
                    }
                    VadEvent::SpeechEnd => {
                        ended = true;
                        self.recording_marks
                            .entry("vad_end".into())
                            .or_insert(self.recording_started.elapsed().as_millis() as u64);
                    }
                }
            }
            if listening.speaking && !was_speaking {
                listening.utterance = listening.preroll.drain(..).collect();
                began = true;
            }
            if listening.speaking {
                listening.utterance.extend_from_slice(pcm);
                if !began {
                    listening.feed(pcm);
                }
                self.last_activity = Instant::now();
            } else {
                listening.preroll.extend(pcm.iter().copied());
                let excess = listening.preroll.len().saturating_sub(preroll_max);
                listening.preroll.drain(..excess);
            }
        }
        if began {
            // VAD 刚确认开始说话：建立流式识别，先补送保留的开头与这一帧
            let stream = self.open_stream();
            if let Phase::Listening(listening) = &mut self.phase {
                listening.stream = stream;
                let head = listening.utterance.clone();
                listening.feed(&head);
            }
        }
        let Phase::Listening(listening) = &mut self.phase else {
            return None;
        };
        if ended {
            return Some((
                std::mem::take(&mut listening.utterance),
                UtteranceEnd::Silence,
            ));
        }
        if listening.utterance.len() >= max {
            return Some((
                std::mem::take(&mut listening.utterance),
                UtteranceEnd::TooLong,
            ));
        }
        None
    }

    /// NV8a-1：完整中间识别已是停止词时立刻发起一次急停轮次；普通前缀不触发。
    fn on_interim(&mut self, interim: String) {
        if !crate::fast::is_stop(&interim) || self.conversation.is_none() {
            return;
        }
        if !matches!(self.phase, Phase::Listening(_)) {
            return;
        }
        self.pending_listen_stop = false;
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        // 替换 Listening 会丢弃旧 ASR 接收器；本次语音只开始一个 FastStop 轮次，
        // 后续最终结果和设备 listen stop 不会再次触发。
        self.phase = Phase::Idle;
        self.event(format!(
            "流式中间结果整句命中急停词（{}）：立即请求急停",
            crate::fast::normalized(&interim)
        ));
        self.start_turn(TurnInput::FastStop { text: interim }, TurnKind::Answer);
    }

    /// 一段话结束：记录并交给应答。
    async fn finish_utterance(
        &mut self,
        socket: &mut WebSocket,
        pcm: Vec<i16>,
        end: UtteranceEnd,
    ) -> Result<(), String> {
        self.pending_listen_stop = false;
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        let stream = match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Listening(listening) => listening.stream.map(|(stream, _)| stream),
            _ => None,
        };
        self.set_state("idle");
        let seconds = pcm.len() as f64 / self.uplink_rate as f64;
        self.event(format!("收到一段语音：{seconds:.1} 秒（{}）", end.label()));
        if self.conversation.is_none() {
            return self.echo(socket, pcm).await;
        }
        if pcm.is_empty() {
            // 手动模式松开但没说话：设备已回到空闲
            return Ok(());
        }
        let sample_rate = self.uplink_rate;
        if let Some(enrollment)=self.conversation.as_ref().and_then(|c|c.speaker_enrollment.as_ref()) {
            enrollment.submit(self.device_id,self.session_id,&pcm,sample_rate);
        }
        self.start_turn(
            TurnInput::Utterance {
                pcm,
                sample_rate,
                stream,
            },
            TurnKind::Answer,
        );
        Ok(())
    }

    fn start_turn(&mut self, input: TurnInput, kind: TurnKind) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        self.barge.reset();
        self.playback_started = None;
        self.barge_resumed = false;
        self.turn_number += 1;
        let recording_offset = if kind == TurnKind::Answer {
            self.recording_started.elapsed().as_millis() as u64
        } else {
            0
        };
        let timing = if kind == TurnKind::Answer {
            let mut stages = std::mem::take(&mut self.recording_marks);
            let end = if matches!(&input, TurnInput::FastStop { .. }) {
                "interim_stop"
            } else {
                "utterance_end"
            };
            stages.insert(end.into(), recording_offset);
            Some(crate::TurnTiming {
                session_id: self.session_id.into(),
                turn: self.turn_number,
                models: conversation.settings.model_labels.clone(),
                stages,
                tools: Vec::new(),
                outcome: "running".into(),
            })
        } else {
            None
        };
        let context = ToolContext {
            device_id: self.device_id.to_owned(),
            session_id: self.session_id.to_owned(),
        };
        // 2026-10-16：视觉卡片打开“把照片留在对话上下文里”时，把这一张作为多模态用户消息
        // 接进历史；只在会话内存里，随历史截断丢弃，不落盘。
        if kind == TurnKind::Answer {
            if let Some(photo) = self.shared.vision_photos.take(&self.device_id) {
                self.history.push(photo.into_message());
                append_history(&mut self.history, Vec::new(), conversation.settings.history_budget);
            }
        }
        let history = if kind == TurnKind::Answer {
            self.history.clone()
        } else {
            Vec::new()
        };
        let device = self.device_mcp.as_ref().and_then(DeviceMcp::tool_set);
        let (handle, events) = spawn_turn(conversation, context, input, history, device);
        self.turn = Some(ActiveTurn {
            person_scope:None,
            timing,
            recording_offset,
            kind,
            started: Instant::now(),
            handle,
            events,
            user: None,
            spoken: Vec::new(),
            pending_messages: Vec::new(),
            resampler: None,
            audio_started: false,
            done: false,
            close: false,
            listen_after: None,
        });
        self.phase = Phase::Thinking;
        self.set_state("thinking");
    }

    /// 回声：把这段话重采样到下行采样率后重新编码播回，验证编解码、重采样与播放节奏。
    async fn echo(&mut self, socket: &mut WebSocket, pcm: Vec<i16>) -> Result<(), String> {
        let seconds = pcm.len() as f64 / self.uplink_rate as f64;
        let text = if pcm.is_empty() {
            "回声测试：没有听到声音".to_owned()
        } else {
            format!("回声测试：约 {seconds:.1} 秒")
        };
        let stt = ServerMessage::Stt {
            session_id: self.session_id.into(),
            text,
        };
        if !send_json(socket, &stt).await {
            return Err("发送失败".into());
        }
        self.tts(socket, TtsState::Start, None).await?;
        if pcm.is_empty() {
            self.last_activity = Instant::now();
            return self.tts(socket, TtsState::Stop, None).await;
        }
        let mut resampler =
            Resampler::new(self.uplink_rate, self.shared.config.downlink_sample_rate);
        self.pump.begin();
        self.pump
            .push_marked(&resampler.process(&pcm), "（回声）".into());
        self.pump.finish();
        self.phase = Phase::Speaking {
            next: Instant::now(),
            burst: PREBUFFER_FRAMES,
        };
        self.set_state("speaking");
        Ok(())
    }

    /// Music is a separate audio source, never a TTS sentence or assistant
    /// message in conversation history. Only the trusted catalog opens a song.
    async fn start_music(
        &mut self,
        socket: &mut WebSocket,
        track_id: String,
        title: String,
    ) -> Result<(), String> {
        if !self.permission.load(std::sync::atomic::Ordering::Acquire)
            || self.shared.config.downlink_sample_rate != 24_000
        {
            self.event("音乐请求被拒绝：设备权限或播放采样率不符合要求".into());
            return Ok(());
        }
        let Some(index) = self.shared.config.music.clone() else {
            self.event("音乐功能未启用".into());
            return Ok(());
        };
        // No pathname or file descriptor is supplied by the model.
        let opened = tokio::task::spawn_blocking(move || index.open(&track_id)).await;
        let (file, extension) = match opened {
            Ok(Ok(track)) => track,
            _ => {
                self.event("所选歌曲不存在、已变化或不允许读取".into());
                return Ok(());
            }
        };
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        if matches!(self.phase, Phase::Speaking { .. }) {
            self.stop_speaking(socket, Some("开始新歌曲")).await?;
        }
        let Some(music) = self.music.as_mut() else {
            return Ok(());
        };
        if music.start(file, &extension, &mut self.pump).is_err() {
            self.event("本地音乐解码任务未能启动".into());
            return Ok(());
        }
        self.tts(socket, TtsState::Start, None).await?;
        self.phase = Phase::Speaking {
            next: Instant::now(),
            burst: PREBUFFER_FRAMES,
        };
        self.set_state("speaking");
        // Title is bounded by the filesystem scanner; no absolute path/URL.
        self.event(format!("开始播放本地歌曲：{title}"));
        self.playback_started = None;
        self.barge.reset();
        Ok(())
    }

    async fn on_turn_event(
        &mut self,
        socket: &mut WebSocket,
        event: Option<TurnEvent>,
    ) -> Result<(), String> {
        if self.turn.is_none() {
            return Ok(());
        }
        // 任务意外结束（没有发 Done）按“结束、无新消息”处理
        match event.unwrap_or(TurnEvent::Done {
            messages: Vec::new(),
            close: false,
        }) {
            TurnEvent::PersonResolved(scope)=>{if let Some(turn)=self.turn.as_mut(){turn.person_scope=scope;}},
            TurnEvent::Closing => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.close = true;
                }
                self.barge.reset();
            }
            TurnEvent::Timing { stage, ms } => {
                if let Some(turn) = self.turn.as_mut()
                    && let Some(timing) = turn.timing.as_mut()
                {
                    timing
                        .stages
                        .entry(stage.into())
                        .or_insert(turn.recording_offset.saturating_add(ms));
                }
            }
            TurnEvent::ToolTiming { name, ms } => {
                if let Some(turn) = self.turn.as_mut()
                    && let Some(timing) = turn.timing.as_mut()
                    && timing.tools.len() < 50
                {
                    // 只保留注册名称，不保存参数或工具结果。
                    timing.tools.push(crate::ToolTiming { name, ms });
                }
            }
            TurnEvent::TextReply(text) => {
                // 合成不可用仍给设备文字，不用静音音频冒充已播报。
                self.tts(socket, TtsState::Start, None).await?;
                self.tts(socket, TtsState::SentenceStart, Some(text))
                    .await?;
                self.tts(socket, TtsState::Stop, None).await?;
            }
            TurnEvent::Heard(text) => {
                self.event(format!("识别完成：{} 字", text.chars().count()));
                if let Some(turn) = self.turn.as_mut() {
                    turn.user = Some(text.clone());
                }
                if !send_json(
                    socket,
                    &ServerMessage::Stt {
                        session_id: self.session_id.into(),
                        text,
                    },
                )
                .await
                {
                    return Err("发送失败".into());
                }
            }
            TurnEvent::Ignored => {
                self.finish_timing("ignored");
                self.turn = None;
                self.event("没有识别出文字，继续拾音".into());
                if self.listen_manual {
                    self.phase = Phase::Idle;
                    self.set_state("idle");
                } else {
                    // 自动模式下设备仍在拾音（还没收到 tts start）
                    self.start_listening(false);
                }
            }
            TurnEvent::Emotion(emotion) => {
                let message = ServerMessage::Llm {
                    session_id: self.session_id.into(),
                    text: emotion.into(),
                    emotion: emotion.into(),
                };
                if !send_json(socket, &message).await {
                    return Err("发送失败".into());
                }
            }
            TurnEvent::PlayMusic { track_id, title } => {
                self.start_music(socket, track_id, title).await?;
            }
            TurnEvent::Speech { text, pcm } => {
                let Some(turn) = self.turn.as_mut() else {
                    return Ok(());
                };
                let first = !turn.audio_started;
                turn.audio_started = true;
                if first && turn.kind == TurnKind::Answer {
                    let latency = turn.started.elapsed().as_millis();
                    self.event(format!("首句延迟：{latency} ms（说完到首句音频就绪）"));
                }
                if first {
                    self.pump.begin();
                    self.tts(socket, TtsState::Start, None).await?;
                    self.phase = Phase::Speaking {
                        next: Instant::now(),
                        burst: PREBUFFER_FRAMES,
                    };
                    self.set_state("speaking");
                }
                let mut resampler =
                    Resampler::new(pcm.sample_rate, self.shared.config.downlink_sample_rate);
                self.pump
                    .push_marked(&resampler.process(&pcm.samples), text);
                if let Some(turn) = self.turn.as_mut() {
                    turn.resampler = Some(resampler);
                }
            }
            TurnEvent::SpeechMore(pcm) => {
                let downlink = self.shared.config.downlink_sample_rate;
                let Some(turn) = self.turn.as_mut() else {
                    return Ok(());
                };
                if !turn.audio_started {
                    return Ok(());
                }
                let resampler = turn
                    .resampler
                    .get_or_insert_with(|| Resampler::new(pcm.sample_rate, downlink));
                let samples = resampler.process(&pcm.samples);
                self.pump.push(&samples);
            }
            TurnEvent::Failed(error) => self.event(format!("对话出错：{error}")),
            TurnEvent::Done { messages, close } => {
                if self.music.as_ref().is_some_and(|music| music.active()) {
                    self.finish_timing("music_started");
                    self.turn = None;
                    return Ok(());
                }
                let Some(turn) = self.turn.as_mut() else {
                    return Ok(());
                };
                turn.done = true;
                turn.close = close;
                turn.pending_messages = messages;
                if turn.audio_started {
                    // 播完剩余音频后由 tick 收尾
                    self.pump.finish();
                } else {
                    return self.end_turn(false).await;
                }
            }
        }
        Ok(())
    }

    /// 一轮对话结束（音频已播完或没有音频）。返回 `Err` 表示结束会话。
    async fn end_turn(&mut self, played: bool) -> Result<(), String> {
        self.finish_timing("done");
        let Some(mut turn) = self.turn.take() else {
            return Ok(());
        };
        if turn.kind == TurnKind::Answer
            && let Some(conversation) = &self.conversation
        {
            let messages=std::mem::take(&mut turn.pending_messages);
            if let Some(personal)=&conversation.personal_context {
                personal.commit(&ToolContext{device_id:self.device_id.into(),session_id:self.session_id.into()},turn.person_scope.as_ref(),messages,conversation.settings.history_budget);
            }else{append_history(&mut self.history,messages,conversation.settings.history_budget);}
        }
        self.phase = Phase::Idle;
        self.set_state("idle");
        self.last_activity = Instant::now();
        if turn.close {
            if played {
                // 给设备时间播完已发出的帧
                tokio::time::sleep(Duration::from_millis(600)).await;
            }
            return Err(match turn.kind {
                TurnKind::Goodbye => format!(
                    "{} 秒没有对话，已说再见",
                    self.shared.config.idle_timeout.as_secs()
                ),
                _ => "用户说了退出词".into(),
            });
        }
        if let Some(manual) = turn.listen_after {
            self.start_listening(manual);
        } else if self.listen_realtime
            || (!played && turn.kind == TurnKind::Answer && !self.listen_manual)
        {
            // realtime 固件一直上传声音，不保证再发 listen start；无音频的 auto 模式也仍在拾音
            self.start_listening(false);
        }
        Ok(())
    }

    /// 打断：中止对话任务、丢弃未播音频；已识别的原话与已播的句子记入历史。
    async fn interrupt(&mut self, socket: &mut WebSocket, reason: &str) -> Result<(), String> {
        self.finish_timing("interrupted");
        if let Some(music) = self.music.as_mut() {
            if music.active() {
                music.cancel(&mut self.pump);
            }
        }
        if let Some(turn) = self.turn.take()
            && turn.kind == TurnKind::Answer
            && let Some(user) = turn.user.clone()
            && let Some(conversation) = &self.conversation
        {
            let mut messages = vec![ChatMessage::user(user)];
            if !turn.spoken.is_empty() {
                messages.push(ChatMessage::assistant(turn.spoken.join("")));
            }
            if let Some(personal)=&conversation.personal_context {
                personal.commit(&ToolContext{device_id:self.device_id.into(),session_id:self.session_id.into()},turn.person_scope.as_ref(),messages,conversation.settings.history_budget);
            }else{append_history(&mut self.history,messages,conversation.settings.history_budget);}
        }
        match self.phase {
            Phase::Speaking { .. } => self.stop_speaking(socket, Some(reason)).await,
            Phase::Thinking => {
                self.event(format!("对话被打断（{reason}）"));
                self.phase = Phase::Idle;
                self.set_state("idle");
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// 播放节奏的一拍：开头连发预缓冲帧，之后每拍一帧；按绝对时钟推进，不受发送耗时影响。
    async fn tick(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        let was_music = self.music.as_ref().is_some_and(|music| music.active());
        if was_music && let Some(music) = self.music.as_mut() {
            match music.refill(&mut self.pump) {
                MusicProgress::Failed(_) => self.event("音乐解码或缓冲失败，已终止播放".into()),
                MusicProgress::Cancelled => self.event("设备权限撤销，音乐已停止".into()),
                _ => {}
            }
        }
        loop {
            match self.pump.next_frame()? {
                PumpTick::Frame { data, marks } => {
                    // Check permission immediately before every outgoing frame,
                    // including any initial prebuffer burst.
                    if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
                        if let Some(music) = self.music.as_mut() {
                            music.cancel(&mut self.pump);
                        }
                        return Err("设备权限已撤销".into());
                    }
                    for text in marks {
                        self.tts(socket, TtsState::SentenceStart, Some(text.clone()))
                            .await?;
                        if let Some(turn) = self.turn.as_mut() {
                            turn.spoken.push(text);
                        }
                    }
                    self.send_frame(socket, &data).await?;
                    if self.playback_started.is_none() {
                        self.playback_started = Some(Instant::now());
                        self.barge.reset();
                    }
                    if let Some(turn) = self.turn.as_mut()
                        && let Some(timing) = turn.timing.as_mut()
                    {
                        timing.stages.entry("first_audio_frame".into()).or_insert(
                            turn.recording_offset
                                .saturating_add(turn.started.elapsed().as_millis() as u64),
                        );
                    }
                    if let Phase::Speaking { burst, .. } = &mut self.phase {
                        if *burst > 1 {
                            *burst -= 1;
                            continue;
                        }
                        *burst = 0;
                    }
                    break;
                }
                PumpTick::Waiting => break,
                PumpTick::Done => {
                    if was_music && let Some(music) = self.music.as_mut() {
                        music.finish_after_pump_done();
                    }
                    self.stop_speaking(socket, None).await?;
                    return if was_music {
                        if self.listen_realtime {
                            self.start_listening(false);
                        }
                        Ok(())
                    } else {
                        self.end_turn(true).await
                    };
                }
            }
        }
        if let Phase::Speaking { next, .. } = &mut self.phase {
            *next += Duration::from_millis(FRAME_MS);
        }
        Ok(())
    }

    /// 结束播放。`interrupted` 为打断原因时立即丢弃未播内容。
    async fn stop_speaking(
        &mut self,
        socket: &mut WebSocket,
        interrupted: Option<&str>,
    ) -> Result<(), String> {
        let stats = self.pump.stats();
        if let Some(reason) = interrupted {
            self.pump.clear();
            self.event(format!("播放被打断（{reason}）：已播 {} 帧", stats.frames));
        } else {
            self.event(format!(
                "播放完成：{} 帧，补静音 {} 帧",
                stats.frames, stats.underruns
            ));
        }
        self.phase = Phase::Idle;
        self.set_state("idle");
        self.last_activity = Instant::now();
        self.tts(socket, TtsState::Stop, None).await
    }

    async fn on_binary(&mut self, socket: &mut WebSocket, data: &[u8]) -> Result<(), String> {
        let listening = matches!(self.phase, Phase::Listening(_));
        let may_interrupt = self.shared.config.barge_in
            && self.listen_realtime
            && matches!(self.phase, Phase::Thinking | Phase::Speaking { .. })
            && (self
                .turn
                .as_ref()
                .is_some_and(|turn| turn.kind == TurnKind::Answer && !turn.close)
                || self.music.as_ref().is_some_and(|music| music.active()));
        if !listening && !may_interrupt {
            return Ok(());
        }
        let decoded = decode_frame(self.version, data).map_err(|error| error.to_string());
        let pcm = match decoded.and_then(|frame| self.decoder.decode(&frame.payload)) {
            Ok(pcm) => pcm,
            Err(error) => {
                self.decode_errors += 1;
                if self.decode_errors == 1 {
                    self.event(format!(
                        "上行音频无法解码（已忽略，之后同类错误不再记录）：{error}"
                    ));
                }
                return Ok(());
            }
        };
        if !listening {
            let guarded = matches!(self.phase, Phase::Speaking { .. })
                && self
                    .playback_started
                    .is_none_or(|at| at.elapsed() < Duration::from_millis(300));
            let Some(head) = self.barge.accept(&pcm, guarded) else {
                return Ok(());
            };
            let elapsed = self.playback_started.map(|at| at.elapsed().as_millis());
            let thinking = matches!(self.phase, Phase::Thinking);
            self.interrupt(socket, "自然插话").await?;
            if thinking {
                self.tts(socket, TtsState::Stop, None).await?;
            }
            self.start_listening(false);
            self.barge_resumed = true;
            // 补送一次完整开头，并让普通判停器获得同样的语音状态。
            let head_ms = head.len() as u64 * 1000 / u64::from(self.uplink_rate);
            self.recording_started = Instant::now() - Duration::from_millis(head_ms);
            self.recording_marks.insert("barge_in".into(), head_ms);
            if let Some(worker) = self.vad_worker.as_mut() {
                worker.submit(head.clone(), true)?;
            } else {
                let _ = self.vad.accept(&head);
            }
            let stream = self.open_stream();
            if let Phase::Listening(recording) = &mut self.phase {
                recording.speaking = true;
                recording.stream = stream;
                recording.feed(&head);
                recording.utterance = head;
            }
            self.last_activity = Instant::now();
            self.event(format!(
                "自然插话：连续语音至少 {} ms，距开播 {}，保留开头 {head_ms} ms",
                self.shared.config.barge_in_ms,
                elapsed
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "尚未开播".into())
            ));
            return Ok(());
        }
        if !self.listen_manual
            && let Some(worker) = self.vad_worker.as_mut()
        {
            worker.submit(pcm, false)?;
            return Ok(());
        }
        match self.hear(&pcm, None) {
            Some((utterance, end)) => self.finish_utterance(socket, utterance, end).await,
            None => Ok(()),
        }
    }

    async fn finish_listen_stop(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        if !self.listen_manual
            && self
                .vad_worker
                .as_ref()
                .is_some_and(|worker| worker.pending())
        {
            self.pending_listen_stop = true;
            return Ok(());
        }
        if let Phase::Listening(listening) = &mut self.phase {
            let utterance = if listening.manual || listening.speaking {
                std::mem::take(&mut listening.utterance)
            } else {
                Vec::new()
            };
            self.finish_utterance(socket, utterance, UtteranceEnd::ListenStop)
                .await?;
        }
        Ok(())
    }

    async fn on_vad_result(
        &mut self,
        socket: &mut WebSocket,
        frame: Result<crate::audio::worker::VadFrame, String>,
    ) -> Result<(), String> {
        let frame = frame?;
        if !frame.prime && matches!(self.phase, Phase::Listening(_)) && !self.listen_manual {
            if let Some((pcm, end)) = self.hear(&frame.samples, Some((frame.events, frame.notice)))
            {
                self.finish_utterance(socket, pcm, end).await?;
            }
        }
        if self.pending_listen_stop {
            self.finish_listen_stop(socket).await?;
        }
        Ok(())
    }

    async fn send_mcp(&mut self, socket: &mut WebSocket, payload: Value) -> Result<(), String> {
        let message = ServerMessage::Mcp {
            session_id: self.session_id.into(),
            payload,
        };
        if send_json(socket, &message).await {
            Ok(())
        } else {
            Err("发送失败".into())
        }
    }

    /// 会话开始时向声明了 MCP 的设备发 `initialize`。
    async fn start_device_mcp(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        // Only an already-authorized session gets a currently configured vision capability.
        // Host comes from the validated websocket Host header; port is the ACTUAL bound HTTP port.
        let capability = match (
            &self.vision_host,
            &self.vision_token,
            &self.shared.config.vision,
        ) {
            (Some(host), Some(token), Some(factory))
                if self.permission.load(std::sync::atomic::Ordering::Acquire)
                    && factory.ready() =>
            {
                json!({"vision":{"url":format!("http://{host}:{}/mcp/vision/explain",self.shared.http_port),"token":token}})
            }
            _ => json!({}),
        };
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": capability,
            "clientInfo": {"name": "MOSS", "version": env!("CARGO_PKG_VERSION")},
        });
        let payload = device.request("initialize", params, McpPending::Initialize);
        self.send_mcp(socket, payload).await
    }

    /// 设备发来的 MCP 消息：只处理对我们请求的响应，设备主动发来的请求与通知忽略。
    async fn on_device_mcp(
        &mut self,
        socket: &mut WebSocket,
        payload: Value,
    ) -> Result<(), String> {
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        let Some(id) = payload.get("id").and_then(Value::as_u64) else {
            return Ok(());
        };
        if payload.get("method").is_some() {
            return Ok(());
        }
        match device.pending.remove(&id) {
            Some(McpPending::Initialize) => {
                let list = device.request("tools/list", json!({}), McpPending::ToolsList);
                self.send_mcp(
                    socket,
                    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                )
                .await?;
                self.send_mcp(socket, list).await
            }
            Some(McpPending::ToolsList) => {
                if let Some(tools) = payload.pointer("/result/tools").and_then(Value::as_array) {
                    device.tools.extend(tools.iter().cloned());
                }
                let cursor = payload
                    .pointer("/result/nextCursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty());
                match cursor {
                    Some(cursor) if device.pages < MAX_TOOL_PAGES => {
                        let next = device.request(
                            "tools/list",
                            json!({"cursor": cursor}),
                            McpPending::ToolsList,
                        );
                        self.send_mcp(socket, next).await
                    }
                    _ => {
                        device.ready = true;
                        let count = device.tools.len();
                        self.event(format!("设备提供 {count} 个自带工具"));
                        Ok(())
                    }
                }
            }
            Some(McpPending::Call(reply)) => {
                let _ = reply.send(device_result_text(&payload));
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// 把对话任务的设备工具调用转发给设备。
    async fn forward_device_call(
        &mut self,
        socket: &mut WebSocket,
        call: Option<DeviceCall>,
    ) -> Result<(), String> {
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            return Err("设备授权已撤销".into());
        }
        let (Some(call), Some(device)) = (call, self.device_mcp.as_mut()) else {
            return Ok(());
        };
        let params = json!({"name": call.name, "arguments": call.arguments});
        let payload = device.request("tools/call", params, McpPending::Call(call.reply));
        self.send_mcp(socket, payload).await
    }

    fn busy(&self) -> bool {
        self.turn.is_some() || matches!(self.phase, Phase::Speaking { .. } | Phase::Thinking)
    }

    async fn on_text(&mut self, socket: &mut WebSocket, text: &str) -> Result<(), String> {
        self.last_activity = Instant::now();
        match ClientMessage::parse(text) {
            Ok(ClientMessage::Listen {
                state: ListenState::Start,
                mode,
                ..
            }) => {
                let manual = mode.as_deref() == Some("manual");
                let realtime = mode.as_deref() == Some("realtime");
                if realtime
                    && self.listen_realtime
                    && self.barge_resumed
                    && matches!(self.phase, Phase::Listening(_))
                {
                    self.barge_resumed = false;
                    return Ok(());
                }
                self.listen_realtime = realtime;
                // 唤醒后设备紧接着发 listen start：不打断唤醒回应，播完再拾音
                if let Some(turn) = self.turn.as_mut()
                    && turn.kind == TurnKind::Greeting
                {
                    turn.listen_after = Some(manual);
                    self.listen_manual = manual;
                    return Ok(());
                }
                if self.busy() {
                    self.interrupt(socket, "重新开始拾音").await?;
                }
                self.start_listening(manual);
            }
            Ok(ClientMessage::Listen {
                state: ListenState::Stop,
                ..
            }) => {
                self.finish_listen_stop(socket).await?;
            }
            Ok(ClientMessage::Listen {
                state: ListenState::Detect,
                text,
                ..
            }) => {
                self.event(format!("唤醒：{}", text.unwrap_or_default()));
                let reply = self
                    .conversation
                    .as_ref()
                    .and_then(|conversation| conversation.settings.wake_reply.clone());
                if let Some(reply) = reply
                    && !self.busy()
                {
                    self.start_turn(
                        TurnInput::Say {
                            text: reply,
                            close: false,
                        },
                        TurnKind::Greeting,
                    );
                }
            }
            Ok(ClientMessage::Abort { reason }) => {
                if self.busy() {
                    let reason = reason.unwrap_or_else(|| "设备打断".into());
                    self.interrupt(socket, &reason).await?;
                }
            }
            Ok(ClientMessage::Ping) => {
                if !send_json(socket, &ServerMessage::Pong).await {
                    return Err("发送失败".into());
                }
            }
            Ok(ClientMessage::Goodbye) => return Err("设备结束会话".into()),
            Ok(ClientMessage::Mcp { payload }) => self.on_device_mcp(socket, payload).await?,
            Ok(_) => {}
            Err(error) => self.event(format!("忽略无法解析的消息：{error}")),
        }
        Ok(())
    }

    /// 空闲到期：对话模式先说再见（说完断开），回声模式直接断开。
    fn on_idle(&mut self) -> Result<(), String> {
        let idle = self.shared.config.idle_timeout.as_secs();
        let goodbye = self
            .conversation
            .as_ref()
            .map(|conversation| conversation.settings.goodbye.clone());
        match goodbye {
            Some(text) if self.turn.is_none() => {
                self.event(format!("{idle} 秒没有对话，说再见后断开"));
                self.last_activity = Instant::now();
                self.start_turn(TurnInput::Say { text, close: true }, TurnKind::Goodbye);
                Ok(())
            }
            _ => Err(format!("{idle} 秒无语音或消息")),
        }
    }

    /// 会话循环；返回断开原因。
    async fn run(&mut self, socket: &mut WebSocket) -> String {
        let mut shutdown = self.shared.shutdown.clone();
        let idle_timeout = self.shared.config.idle_timeout;
        if let Err(reason) = self.start_device_mcp(socket).await {
            return reason;
        }
        loop {
            if *shutdown.borrow() {
                return "内置语音服务已停止".into();
            }
            let deadline = match &self.phase {
                Phase::Speaking { next, .. } => *next,
                // 对话任务有自己的超时（识别 / 合成 30 秒，大模型 120 秒），等待期间不算空闲
                Phase::Thinking => Instant::now() + Duration::from_secs(3600),
                _ => self.last_activity + idle_timeout,
            };
            // 按顺序检查（biased）：对话任务的事件先于它发出的设备工具调用处理，设备看到的顺序与任务一致
            // （先 stt 再 tools/call）；设备消息先于播放定时器，打断不被积压的播放拍延迟。
            let result = tokio::select! {
                biased;
                _ = shutdown.changed() => return "内置语音服务已停止".into(),
                frame = async {
                    match self.vad_worker.as_mut() {
                        Some(worker) => worker.next().await,
                        None => std::future::pending().await,
                    }
                } => self.on_vad_result(socket, frame).await,
                event = next_turn_event(&mut self.turn) => self.on_turn_event(socket, event).await,
                interim = next_listening_interim(&mut self.phase) => { self.on_interim(interim); Ok(()) },
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Binary(data))) => self.on_binary(socket, &data).await,
                    Some(Ok(Message::Text(text))) => self.on_text(socket, text.as_str()).await,
                    Some(Ok(Message::Close(_))) | None => return "设备关闭了连接".into(),
                    Some(Ok(_)) => Ok(()),
                    Some(Err(_)) => return "连接出错".into(),
                },
                call = next_device_call(&mut self.device_mcp) => self.forward_device_call(socket, call).await,
                _ = tokio::time::sleep_until(deadline) => {
                    match self.phase {
                        Phase::Speaking { .. } => self.tick(socket).await,
                        Phase::Thinking => Ok(()),
                        _ => self.on_idle(),
                    }
                }
            };
            if let Err(reason) = result {
                return reason;
            }
        }
    }
}
