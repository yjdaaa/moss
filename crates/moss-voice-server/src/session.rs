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
    spawn_turn_with_person_hint,
};
use crate::music::playback::{MusicController, MusicProgress};
use crate::state::{DiagCategory, EventLevel, EventRecord, SessionView};
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
async fn wait_hello(socket: &mut WebSocket) -> Option<(u32, AudioParams, bool, bool)> {
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
                    return Some((version, audio_params, features.mcp, features.aec));
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
            .event(
                EventRecord::warn(DiagCategory::Device, format!("拒绝连接（{}）：设备没有带设备编号（Device-Id）", identity.ip))
                    .hint("这台设备的固件没按小智协议上报设备编号：换用标准小智固件，或检查自编固件的连接请求头"),
            );
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
    // 登记走本地 SQLite（同步锁）：和 open_session 一样放到阻塞线程，不占 tokio 工作线程
    let registry = shared.registry.clone();
    let seen = tokio::task::spawn_blocking(move || registry.seen(&visit))
        .await
        .unwrap_or_else(|error| Err(format!("登记任务失败：{error}")));
    let status = match seen {
        Ok(status) => status,
        Err(error) => {
            shared.state.event(
                EventRecord::error(
                    DiagCategory::Internal,
                    format!("登记设备 {device_id} 失败：本机设备记录读写出错"),
                )
                .hint("重启 MOSS 再试；一直失败请导出诊断包反馈")
                .detail(&error),
            );
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
    let Some((hello_version, uplink, device_mcp, device_aec)) = wait_hello(&mut socket).await
    else {
        shared.state.event(
            EventRecord::warn(
                DiagCategory::Device,
                format!("设备 {device_id} 连上后 10 秒内没有完成握手（hello），已断开"),
            )
            .hint("多半是设备网络太差或固件版本太旧：靠近路由器、重启设备，或升级小智固件"),
        );
        return;
    };
    let version = if matches!(hello_version, 1..=3) {
        hello_version
    } else {
        identity.protocol_version.unwrap_or(1)
    };

    // 2026-11-02：连接握手快照（协议版本、音频参数、是否带 MCP、设备端 AEC）。
    // 桌面端「设备信息」用它显示 AEC 是否真的开启，而不是按板型推断。
    // 2026-11-05：只有 token 校验通过且 open_session 按当前授权（已允许 + Client-Id 一致）登记成功后才写入，
    // 冒用已允许设备 Device-Id 的未认证连接不能改写设备信息。
    let hello_snapshot = json!({
        "protocol_version": version,
        "transport": "websocket",
        "audio": {
            "format": uplink.format,
            "sample_rate": uplink.sample_rate,
            "channels": uplink.channels,
            "frame_duration": uplink.frame_duration,
        },
        "mcp": device_mcp,
        "aec": device_aec,
    });
    let session_id = random_id();
    let lease = if token_ok {
        let (registry, d, c, s) = (
            shared.registry.clone(),
            device_id.clone(),
            client_id.clone(),
            session_id.clone(),
        );
        let snapshot = hello_snapshot.to_string();
        let opened = tokio::task::spawn_blocking(move || {
            let allowed = registry.open_session(&d, &c, &s)?;
            registry.device_snapshot(&d, crate::SNAPSHOT_HELLO, &snapshot);
            Ok::<_, String>(allowed)
        });
        match opened.await {
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
        let uplink_rate =
            if uplink.format == "opus" && supported_rate(uplink.sample_rate) {
                uplink.sample_rate
            } else {
                shared.state.event(EventRecord::warn(
                DiagCategory::Audio,
                format!(
                    "设备 {device_id} 声明的上行音频为 {} {} Hz，不支持，按 Opus 16000 Hz 处理",
                    uplink.format, uplink.sample_rate
                ),
            ).hint("如果听不清或识别全错，换用标准小智固件"));
                16_000
            };
        match Session::new(
            &shared,
            &session_id,
            &device_id,
            &client_id,
            version,
            uplink_rate,
            lease.as_ref().unwrap().allowed.clone(),
        ) {
            Ok(mut session) => {
                if device_mcp {
                    session.device_mcp = Some(DeviceMcp::new());
                    session.vision_host = identity.host.clone();
                    if session.conversation.is_some() {
                        session.vision_token = identity.authorization.clone().map(|value| {
                            value
                                .trim()
                                .strip_prefix("Bearer ")
                                .unwrap_or(value.trim())
                                .to_owned()
                        });
                    }
                }
                shared.state.open(SessionView {
                    session_id: session_id.clone(),
                    device_id: device_id.clone(),
                    ip: identity.ip.clone(),
                    protocol_version: version,
                    connected_at: unix_now(),
                    state: "idle".into(),
                    level: 0,
                    emotion: String::new(),
                    active_tool: None,
                    utterance_chars: 0,
                    spoken_sentences: 0,
                    device_tools: Vec::new(),
                    screen_monochrome: None,
                    screen_width: None,
                    screen_height: None,
                    screen_pack: false,
                });
                shared
                    .registry
                    .sessions_changed(shared.state.sessions().len());
                let (phase, level) = shared.state.aggregate_pulse();
                shared.registry.pulse_changed(phase.as_deref(), level);
                shared.state.event(format!(
                    "设备 {device_id} 已连接（帧协议 v{version}，上行 {uplink_rate} Hz，下行 {} Hz）",
                    shared.config.downlink_sample_rate
                ));
                Some(session)
            }
            Err(error) => {
                shared.state.event(
                    EventRecord::error(
                        DiagCategory::Audio,
                        format!("设备 {device_id} 的音频初始化失败，已断开"),
                    )
                    .hint("设备声明的音频格式 MOSS 处理不了：换用标准小智固件后重试")
                    .detail(&error),
                );
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        }
    } else {
        None
    };
    let mut requests = session.as_ref().map(|s| {
        let (requests_tx, requests_rx) = tokio::sync::mpsc::unbounded_channel();
        if let Ok(mut map) = shared.device_requests.lock() {
            map.insert(
                device_id.clone(),
                crate::DeviceRequestHandle {
                    session_id: session_id.clone(),
                    supports_mcp: s.device_mcp.is_some(),
                    sender: requests_tx,
                },
            );
        }
        requests_rx
    });
    let hello = ServerMessage::Hello {
        version,
        transport: "websocket".into(),
        session_id: session_id.clone(),
        audio_params: AudioParams::opus(shared.config.downlink_sample_rate, FRAME_MS as u32),
    };
    if !send_json(&mut socket, &hello).await {
        if let Ok(mut map) = shared.device_requests.lock()
            && map
                .get(&device_id)
                .is_some_and(|handle| handle.session_id == session_id)
        {
            map.remove(&device_id);
        }
        shared.state.close(&session_id);
        shared
            .registry
            .sessions_changed(shared.state.sessions().len());
        let (phase, level) = shared.state.aggregate_pulse();
        shared.registry.pulse_changed(phase.as_deref(), level);
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
        let record = if status == DeviceStatus::Pending {
            EventRecord::info(DiagCategory::Device, format!("设备 {device_id}：{log}"))
                .hint(message)
        } else {
            EventRecord::warn(DiagCategory::Device, format!("设备 {device_id}：{log}"))
                .hint(message)
        };
        shared.state.event(record);
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
    let mut requests_rx = requests
        .take()
        .expect("requests channel initialized with session");
    let reason = tokio::select! {
        biased;
        _ = async {
            while permission.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } => "设备授权已撤销".to_owned(),
        reason = session.run(&mut socket, &mut requests_rx) => reason,
    };
    // NV10: background summary uses only committed, bounded history and never delays disconnect.
    if permission.load(std::sync::atomic::Ordering::Acquire) {
        if let Some(conversation) = &session.conversation {
            let end_ctx = ToolContext {
                device_id: device_id.clone(),
                session_id: session_id.clone(),
            };
            if let Some(personal) = &conversation.personal_context {
                let person_histories = personal.session_closed(&end_ctx, conversation.llm.clone());
                if let Some(extraction) = &conversation.context_extraction {
                    for (scope, person_history) in &person_histories {
                        extraction.session_closed_for_person(
                            &end_ctx,
                            Some(scope),
                            person_history,
                            conversation.llm.clone(),
                        );
                    }
                }
            } else {
                // n57: structured extraction replaces the NV10 whole-summary rewrite once it is on.
                let extracting = conversation
                    .context_extraction
                    .as_ref()
                    .is_some_and(|e| e.extractor.enabled());
                if let Some(memory) = conversation.memory.as_ref().filter(|_| !extracting) {
                    memory.queue_summary(&device_id, &session.history, conversation.llm.clone());
                }
                // NV19-03: independent of the NV10 summary; off unless the user enabled it.
                if let Some(extraction) = &conversation.context_extraction {
                    extraction.session_closed(&end_ctx, &session.history, conversation.llm.clone());
                }
            }
        }
    } else {
        shared.camera_preview.clear_device(&device_id);
        let _ = shared.vision_photos.take(&device_id);
    }
    let mut was_active_device_handle = false;
    if let Ok(mut map) = shared.device_requests.lock()
        && map
            .get(&device_id)
            .is_some_and(|handle| handle.session_id == session_id)
    {
        map.remove(&device_id);
        was_active_device_handle = true;
    }
    if was_active_device_handle && permission.load(std::sync::atomic::Ordering::Acquire) {
        shared
            .camera_preview
            .fail_if_pending(&device_id, "设备连接已断开，画面未回传".into());
    }
    session.finish_timing("disconnected");
    session.last_verified_person = None;
    session.turn = None;
    session.pump.clear();
    drop(lease);
    shared.state.close(&session_id);
    shared
        .registry
        .sessions_changed(shared.state.sessions().len());
    let (phase, level) = shared.state.aggregate_pulse();
    shared.registry.pulse_changed(phase.as_deref(), level);
    shared.state.event(disconnect_event(&device_id, &reason));
    let _ = socket.send(Message::Close(None)).await;
}

/// 断开原因 → 大白话事件：正常结束记为日常记录，异常断线记为警告并说明最可能的原因。
fn disconnect_event(device_id: &str, reason: &str) -> EventRecord {
    let text = format!("设备 {device_id} 已断开：{reason}");
    let likely = "最可能是 Wi-Fi 掉线、设备重启或断电";
    match reason {
        "连接出错" => EventRecord::warn(
            DiagCategory::Device,
            format!("设备 {device_id} 已断开：连接突然中断（{likely}）"),
        )
        .hint("检查设备供电和 Wi-Fi 信号；设备会自动重连，频繁出现就把设备挪近路由器")
        .detail(reason),
        "发送失败" => EventRecord::warn(
            DiagCategory::Device,
            format!("设备 {device_id} 已断开：向设备发数据失败，连接已经断了（{likely}）"),
        )
        .hint("检查设备供电和 Wi-Fi 信号；设备会自动重连，频繁出现就把设备挪近路由器")
        .detail(reason),
        "设备授权已撤销" => EventRecord::warn(DiagCategory::Device, text)
            .hint("在设备列表里重新允许这台设备后，重启设备"),
        _ if reason.contains("失败") || reason.contains("出错") => {
            EventRecord::warn(DiagCategory::Device, text)
        }
        _ => EventRecord::info(DiagCategory::Device, text),
    }
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
    /// 已向设备发出 sentence_start 的句子（不是仅合成或排队的句子）。
    spoken: Vec<String>,
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

/// 等待非阻塞的半截话续听判定结果（不阻塞 socket/VAD/打断事件轮询）。
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
async fn next_turn_event(turn: &mut Option<ActiveTurn>) -> Option<TurnEvent> {
    match turn {
        Some(turn) if !turn.done => turn.events.recv().await,
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
    hold_check: Option<PendingHoldCheck>,
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
    last_verified_person: Option<VerifiedPersonHint>,
    vision_host: Option<String>,
    vision_token: Option<String>,
    device_mcp: Option<DeviceMcp>,
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
    /// 加工结束播报：只读观察最近任务，发现“发送中 → 终态”时在空闲时播报一次。
    jobs: crate::jobs::JobWatcher,
    next_job_poll: Instant,
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
            timing.at_ms = crate::unix_now_ms();
            let turn_no = timing.turn;
            self.shared.state.finish_timing(timing);
            if turn.kind == TurnKind::Answer
                && self.shared.state.debug_enabled()
                && let Some(raw_asr) = turn.debug_raw_asr.take()
            {
                let corrected_asr = turn
                    .debug_corrected_asr
                    .take()
                    .unwrap_or_else(|| raw_asr.clone());
                let reply = if !turn.spoken.is_empty() {
                    turn.spoken.join("")
                } else {
                    turn.debug_text_reply.join("")
                };
                self.shared.state.finish_debug_trace(crate::TurnDebugTrace {
                    session_id: self.session_id.into(),
                    turn: turn_no,
                    at: crate::unix_now(),
                    raw_asr,
                    corrected_asr,
                    tools: std::mem::take(&mut turn.debug_tools),
                    reply,
                    outcome: outcome.into(),
                    truncated: false,
                    tools_dropped: 0,
                });
            }
        }
    }
    fn new(
        shared: &'a Shared,
        session_id: &'a str,
        device_id: &'a str,
        client_id: &'a str,
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
            client_id,
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
            hold_check: None,
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
            last_verified_person: None,
            vision_host: None,
            vision_token: None,
            device_mcp: None,
            pending_oled_preview: None,
            preview_hold: None,
            last_hud_workflow: None,
            hud_frame: 0,
            last_approval_summary: None,
            one_shot_badge_url: None,
            music_hud: None,
            screen_pack: false,
            screen_scene: None,
            last_screen_emotion: None,
            engrave_preview: None,
            turn_scene: None,
            jobs: crate::jobs::JobWatcher::new(),
            next_job_poll: Instant::now() + shared.config.job_poll_interval,
        })
    }

    fn sync_pulse(&self) {
        let (phase, level) = self.shared.state.aggregate_pulse();
        self.shared.registry.pulse_changed(phase.as_deref(), level);
    }

    fn set_state(&self, state: &str) {
        self.shared.state.set_state(self.session_id, state);
        self.sync_pulse();
    }

    fn set_level(&self, level: u8) {
        self.shared.state.set_level(self.session_id, level);
        self.sync_pulse();
    }
    fn event(&self, record: impl Into<EventRecord>) {
        self.shared.state.event(record);
    }

    async fn tts(
        &mut self,
        socket: &mut WebSocket,
        state: TtsState,
        text: Option<String>,
    ) -> Result<(), String> {
        let starting = matches!(state, TtsState::Start);
        let message = ServerMessage::Tts {
            session_id: self.session_id.into(),
            state,
            text,
        };
        if !send_json(socket, &message).await {
            return Err("发送失败".into());
        }
        if starting {
            // n70 开口说话：全屏画风切到“说话”（听歌、完工、急停画面说话时保持）
            let scene = self
                .sticky_while_talking()
                .or(self.turn_scene)
                .unwrap_or(crate::BotPose::Talking);
            self.show_screen(socket, scene, true).await?;
        }
        Ok(())
    }

    /// n70 这台设备是否按全屏画风包显示（单色屏不算）。
    fn pack_active(&self) -> bool {
        self.screen_pack
            && self
                .device_mcp
                .as_ref()
                .is_none_or(|d| d.screen_monochrome != Some(true))
    }

    /// n70 当前有效的常驻场景（到期的完工 / 急停徽章、已停播的音乐不算）。
    fn sticky_scene(&self) -> Option<&ScreenSceneState> {
        let scene = self.screen_scene.as_ref()?;
        if scene.until.is_some_and(|t| Instant::now() >= t) {
            return None;
        }
        if scene.scene == crate::BotPose::MusicGroove
            && !self.music.as_ref().is_some_and(|m| m.active())
        {
            return None;
        }
        Some(scene)
    }

    /// n70 说话时也保持的常驻场景：听歌、完工、急停。
    fn sticky_while_talking(&self) -> Option<crate::BotPose> {
        self.sticky_scene().map(|s| s.scene).filter(|scene| {
            matches!(
                scene,
                crate::BotPose::MusicGroove
                    | crate::BotPose::CelebrateDone
                    | crate::BotPose::HaltedShield
            )
        })
    }

    /// n70 让设备切到当前画风的某个全屏场景（发 `llm` 表情名）。没刷画风包的设备什么也不发。
    async fn show_screen(
        &mut self,
        socket: &mut WebSocket,
        scene: crate::BotPose,
        force: bool,
    ) -> Result<(), String> {
        if !self.pack_active() {
            return Ok(());
        }
        let name = crate::screen_pack::emotion_name(self.shared.card_style(), scene);
        if !force && self.last_screen_emotion.as_deref() == Some(name.as_str()) {
            return Ok(());
        }
        let message = ServerMessage::Llm {
            session_id: self.session_id.into(),
            text: name.clone(),
            emotion: name.clone(),
        };
        if !send_json(socket, &message).await {
            return Err("发送失败".into());
        }
        self.last_screen_emotion = Some(name);
        Ok(())
    }

    /// n70 在全屏画面上叠一行字（固件字幕位置）。
    async fn screen_text(&mut self, socket: &mut WebSocket, text: String) -> Result<(), String> {
        if send_json(
            socket,
            &ServerMessage::Stt {
                session_id: self.session_id.into(),
                text,
            },
        )
        .await
        {
            Ok(())
        } else {
            Err("发送失败".into())
        }
    }

    /// n70 回到常驻场景（没有就显示 `fallback`）并重发那行字；设备状态切换（固件会重置成 `neutral`）后调用。
    async fn show_ambient(
        &mut self,
        socket: &mut WebSocket,
        fallback: crate::BotPose,
    ) -> Result<(), String> {
        if !self.pack_active() {
            return Ok(());
        }
        let (scene, text) = match self.sticky_scene() {
            Some(sticky) => (sticky.scene, sticky.text.clone()),
            None => (fallback, None),
        };
        self.show_screen(socket, scene, true).await?;
        if let Some(text) = text {
            self.screen_text(socket, text).await?;
        }
        Ok(())
    }

    /// n70 设定常驻场景；画面或那行字变了才发给设备。
    async fn set_sticky(
        &mut self,
        socket: &mut WebSocket,
        scene: crate::BotPose,
        text: Option<String>,
        ttl: Option<Duration>,
    ) -> Result<(), String> {
        let text_changed = self
            .screen_scene
            .as_ref()
            .is_none_or(|old| old.scene != scene || old.text != text);
        self.screen_scene = Some(ScreenSceneState {
            scene,
            text: text.clone(),
            until: ttl.map(|d| Instant::now() + d),
        });
        self.show_screen(socket, scene, false).await?;
        if text_changed && let Some(text) = text {
            self.screen_text(socket, text).await?;
        }
        Ok(())
    }

    /// n70 结束雕刻 / 待批准这类常驻场景，回到聆听或待命画面。
    async fn clear_sticky(
        &mut self,
        socket: &mut WebSocket,
        scenes: &[crate::BotPose],
    ) -> Result<(), String> {
        if !self
            .screen_scene
            .as_ref()
            .is_some_and(|s| scenes.contains(&s.scene))
        {
            return Ok(());
        }
        self.screen_scene = None;
        if !self.busy() {
            let fallback = if matches!(self.phase, Phase::Listening(_)) {
                crate::BotPose::Listening
            } else {
                crate::BotPose::IdleBlink
            };
            self.show_screen(socket, fallback, false).await?;
        }
        Ok(())
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
        self.hold_check = None;
        self.listen_manual = manual;
        // 手动模式按住即说：马上建立识别连接；自动模式等 VAD 确认开始说话（避免为环境声建连接）
        let stream = if manual { self.open_stream() } else { None };
        self.phase = Phase::Listening(Listening {
            manual,
            speaking: false,
            utterance: Vec::new(),
            preroll: VecDeque::new(),
            stream,
            last_interim: String::new(),
            prewarm_task: None,
            hold_used: false,
            hold_deadline: None,
            finalize_after_vad_seq: None,
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
                self.shared.state.event(
                    EventRecord::warn(DiagCategory::Audio, notice)
                        .hint("本地判停模型出了问题，这次连接先用音量判断你是否说完；重启内置服务会重新尝试本地模型"),
                );
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
                        self.recording_marks.insert(
                            "vad_end".into(),
                            self.recording_started.elapsed().as_millis() as u64,
                        );
                    }
                }
            }
            if listening.speaking && !was_speaking {
                if self.hold_check.take().is_some()
                    || listening.hold_deadline.is_some()
                    || listening.finalize_after_vad_seq.is_some()
                {
                    listening.hold_used = true;
                }
                listening.hold_deadline = None;
                listening.finalize_after_vad_seq = None;
                if listening.utterance.is_empty() {
                    listening.utterance = listening.preroll.drain(..).collect();
                    began = true;
                } else {
                    // 半截话续听后再次开口：将中间短暂过渡音频无缝拼入同一段 utterance 与流式识别
                    let bridge: Vec<i16> = listening.preroll.drain(..).collect();
                    listening.feed(&bridge);
                    listening.utterance.extend_from_slice(&bridge);
                }
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
            self.preview_hold = None;
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
        if ended && self.hold_check.is_none() {
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

    /// NV8a-1：完整中间识别已是停止词时立刻发起一次急停轮次；
    /// 普通句子若开启了 Jev 筛选，则按 120ms 防抖在后台预热未说完/路由/工具缓存（连续跳字不风暴）。
    fn on_interim(&mut self, interim: String) {
        let Some(conversation) = self.conversation.clone() else {
            return;
        };
        if !matches!(self.phase, Phase::Listening(_)) {
            return;
        }
        if crate::fast::is_stop(&interim) {
            self.pending_listen_stop = false;
            self.hold_check = None;
            if let Some(worker) = self.vad_worker.as_mut() {
                worker.reset();
            }
            // 替换 Listening 会丢弃旧 ASR 接收器与预热任务；本次语音只开始一个 FastStop 轮次，
            // 后续最终结果和设备 listen stop 不会再次触发。
            self.phase = Phase::Idle;
            self.event(format!(
                "流式中间结果整句命中急停词（{}）：立即请求急停",
                crate::fast::normalized(&interim)
            ));
            self.start_turn(TurnInput::FastStop { text: interim }, TurnKind::Answer);
            return;
        }
        let device_specs: Vec<_> = self
            .device_mcp
            .as_ref()
            .and_then(DeviceMcp::tool_set)
            .map(|device| {
                device
                    .specs()
                    .iter()
                    .filter(|spec| spec.name != "play_music")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let Phase::Listening(listening) = &mut self.phase else {
            return;
        };
        if listening.last_interim == interim {
            return;
        }
        listening.last_interim = interim.clone();
        if let Some(prev) = listening.prewarm_task.take() {
            prev.abort();
        }
        if let Some(jev) = conversation
            .jev_filter
            .clone()
            .filter(|f| f.options().enabled && f.options().unfinished_hold)
            && crate::fast::normalized(&interim).chars().count() >= 2
        {
            let history = Self::jev_prewarm_history(
                &self.shared.vision_photos,
                self.device_id,
                &self.history,
                &conversation,
            );
            listening.prewarm_task = Some(tokio::spawn(async move {
                tokio::time::sleep(INTERIM_PREWARM_DEBOUNCE).await;
                let mut tools = conversation.tools.tools();
                tools.extend(device_specs);
                let _ = jev.should_hold_unfinished(&interim, &history, &tools).await;
            }));
        }
    }

    fn jev_prewarm_history(
        vision_photos: &crate::vision::VisionPhotos,
        device_id: &str,
        base_history: &[ChatMessage],
        conversation: &Conversation,
    ) -> Vec<ChatMessage> {
        // 声纹个人模式下本轮说话人尚未在拾音阶段完成验声，严禁把上一位说话人的私密历史送往预热端点。
        if conversation.personal_context.is_some() {
            return Vec::new();
        }
        let mut history = base_history.to_vec();
        if let Some(photo_msg) = vision_photos.peek_message(device_id) {
            history.push(photo_msg);
        }
        history
    }

    /// VAD 静音判停时：若开启了 Jev 未说完防抢话，启动非阻塞判定任务挂在 `Session::hold_check` 上，
    /// 会话循环继续实时响应 `Abort`、`ListenStop`、`Ping`、停止词与提前续说音频。
    async fn finish_or_hold_utterance(
        &mut self,
        socket: &mut WebSocket,
        pcm: Vec<i16>,
        end: UtteranceEnd,
    ) -> Result<(), String> {
        if matches!(end, UtteranceEnd::Silence)
            && !self.pending_listen_stop
            && self.hold_check.is_none()
            && let Some(conversation) = self.conversation.clone()
            && let Some(jev) = conversation
                .jev_filter
                .clone()
                .filter(|f| f.options().enabled && f.options().unfinished_hold)
        {
            let mut tools = conversation.tools.tools();
            if let Some(device) = self.device_mcp.as_ref().and_then(DeviceMcp::tool_set) {
                tools.extend(
                    device
                        .specs()
                        .iter()
                        .filter(|spec| spec.name != "play_music")
                        .cloned(),
                );
            }
            if let Phase::Listening(listening) = &mut self.phase
                && !listening.manual
                && !listening.hold_used
            {
                if let Some(task) = listening.prewarm_task.take()
                    && !task.is_finished()
                {
                    task.abort();
                }
                let latest = listening
                    .stream
                    .as_ref()
                    .and_then(|(s, _)| s.latest_interim())
                    .unwrap_or_else(|| listening.last_interim.clone());
                listening.last_interim = latest.clone();
                if !latest.is_empty() {
                    let paused_at = Instant::now();
                    let history = Self::jev_prewarm_history(
                        &self.shared.vision_photos,
                        self.device_id,
                        &self.history,
                        &conversation,
                    );
                    // 暂存前半句录音、保持 Phase::Listening 不中断，交由 select! 异步接收结果
                    listening.utterance = pcm;
                    listening.speaking = false;
                    self.hold_check = Some(PendingHoldCheck {
                        paused_at,
                        task: tokio::spawn(async move {
                            tokio::task::yield_now().await;
                            jev.should_hold_unfinished(&latest, &history, &tools).await
                        }),
                    });
                    self.set_level(0);
                    return Ok(());
                }
            }
        }
        self.finish_utterance(socket, pcm, end).await
    }

    async fn on_hold_check_done(
        &mut self,
        socket: &mut WebSocket,
        paused_at: Instant,
        held: bool,
    ) -> Result<(), String> {
        if self.pending_listen_stop {
            return self.finish_listen_stop(socket).await;
        }
        let deadline = paused_at + UNFINISHED_HOLD_WINDOW;
        if held
            && Instant::now() < deadline
            && let Phase::Listening(listening) = &mut self.phase
        {
            listening.hold_used = true;
            listening.hold_deadline = Some(deadline);
            listening.finalize_after_vad_seq = None;
            self.set_level(0);
            self.event("检测到语句尚未说完，延长拾音等待续说");
            return Ok(());
        }
        self.finalize_held_utterance(socket).await
    }

    async fn on_hold_expired(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        let active = matches!(&self.phase, Phase::Listening(l) if l.hold_deadline.is_some());
        if !active {
            return Ok(());
        }
        self.finalize_held_utterance(socket).await
    }

    /// 结束挂起中的半句录音：若此刻 `VadWorker` 仍有已接收但尚未消费的在途帧，
    /// 先记录帧序号屏障，待这些在途帧经 `hear` 确认无续说后再提交，防止 `worker.reset()` 丢弃已入队续说语音。
    async fn finalize_held_utterance(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        if !self.listen_manual
            && let Some(target) = self
                .vad_worker
                .as_ref()
                .and_then(crate::audio::worker::VadWorker::pending_target)
            && let Phase::Listening(listening) = &mut self.phase
        {
            listening.hold_deadline = None;
            listening.finalize_after_vad_seq = Some(target);
            return Ok(());
        }
        let pcm = match &mut self.phase {
            Phase::Listening(listening) => {
                listening.hold_deadline = None;
                listening.finalize_after_vad_seq = None;
                std::mem::take(&mut listening.utterance)
            }
            _ => return Ok(()),
        };
        self.finish_utterance(socket, pcm, UtteranceEnd::Silence)
            .await
    }

    /// 一段话结束：记录并交给应答。
    async fn finish_utterance(
        &mut self,
        socket: &mut WebSocket,
        pcm: Vec<i16>,
        end: UtteranceEnd,
    ) -> Result<(), String> {
        self.pending_listen_stop = false;
        self.hold_check = None;
        if let Some(worker) = self.vad_worker.as_mut() {
            worker.reset();
        }
        let stream = match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Listening(mut listening) => listening.stream.take().map(|(stream, _)| stream),
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
        if let Some(enrollment) = self
            .conversation
            .as_ref()
            .and_then(|c| c.speaker_enrollment.as_ref())
        {
            enrollment.submit(self.device_id, self.session_id, &pcm, sample_rate);
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
        self.turn_scene = None;
        if matches!(kind, TurnKind::Answer | TurnKind::Goodbye) {
            self.preview_hold = None;
            self.pending_oled_preview = None;
            self.one_shot_badge_url = None;
            self.music_hud = None;
        }
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
                device_id: self.device_id.into(),
                at_ms: 0,
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
        let mut pending_photo = None;
        let history = if kind == TurnKind::Answer {
            if conversation.personal_context.is_some() {
                if let Some(photo) = self.shared.vision_photos.take(self.device_id) {
                    let msg = photo.into_message();
                    pending_photo = Some(msg.clone());
                    vec![msg]
                } else {
                    Vec::new()
                }
            } else {
                if let Some(photo) = self.shared.vision_photos.take(self.device_id) {
                    self.history.push(photo.into_message());
                    append_history(
                        &mut self.history,
                        Vec::new(),
                        conversation.settings.history_budget,
                    );
                }
                self.history.clone()
            }
        } else {
            Vec::new()
        };
        let person_hint = if kind == TurnKind::Answer {
            if let Some(personal) = &conversation.personal_context {
                let valid = self.last_verified_person.as_ref().is_some_and(|hint| {
                    hint.verified_at.elapsed() <= SHORT_UTTERANCE_CONTINUITY_WINDOW
                        && hint.consecutive_short < MAX_CONSECUTIVE_SHORT_UTTERANCES
                        && personal.permitted(&context, Some(&hint.scope))
                });
                if !valid {
                    self.last_verified_person = None;
                }
            } else {
                self.last_verified_person = None;
            }
            self.last_verified_person
                .as_ref()
                .map(|hint| hint.scope.clone())
        } else {
            None
        };
        let device = self.device_mcp.as_ref().and_then(DeviceMcp::tool_set);
        let (handle, events) =
            spawn_turn_with_person_hint(conversation, context, input, history, device, person_hint);
        self.turn = Some(ActiveTurn {
            person_scope: None,
            person_verified: false,
            pending_photo,
            timing,
            recording_offset,
            kind,
            started: Instant::now(),
            handle,
            events,
            user: None,
            spoken: Vec::new(),
            debug_raw_asr: None,
            debug_corrected_asr: None,
            debug_tools: Vec::new(),
            debug_text_reply: Vec::new(),
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
            self.event(EventRecord::warn(
                DiagCategory::Audio,
                "音乐请求被拒绝：设备权限或播放采样率不符合要求",
            ));
            return Ok(());
        }
        let Some(index) = self.shared.config.music.clone() else {
            self.event(
                EventRecord::warn(DiagCategory::Config, "音乐功能未启用")
                    .hint("在“能力”标签里打开本地音乐并选好音乐文件夹"),
            );
            return Ok(());
        };
        // No pathname or file descriptor is supplied by the model.
        let opened = tokio::task::spawn_blocking(move || index.open(&track_id)).await;
        let (file, extension) = match opened {
            Ok(Ok(track)) => track,
            _ => {
                self.event(
                    EventRecord::warn(DiagCategory::Audio, "所选歌曲不存在、已变化或不允许读取")
                        .hint("音乐文件可能被移动或删除了：在“能力”标签重新选择音乐文件夹"),
                );
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
            self.event(EventRecord::error(
                DiagCategory::Audio,
                "本地音乐解码任务未能启动",
            ));
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
        self.preview_hold = None;
        let want_hud = self
            .conversation
            .as_ref()
            .is_none_or(|c| c.settings.screen_preview_sync);
        if want_hud && self.pack_active() {
            // n70 刷了画风包：全屏唱机 / 听歌小机器人一直显示到停播，不再每 4 秒推半屏卡
            let short_title: String = title.chars().take(48).collect();
            self.set_sticky(
                socket,
                crate::BotPose::MusicGroove,
                Some(format!("[音乐] {short_title}")),
                None,
            )
            .await?;
            return Ok(());
        }
        let can_color_music = want_hud
            && self.device_mcp.as_ref().is_some_and(|d| {
                d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
            });
        if can_color_music {
            let ascii_raw: String = title
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
                .take(14)
                .collect::<String>()
                .trim()
                .to_ascii_uppercase();
            let title_ascii = if ascii_raw.is_empty() {
                "LOCAL TRACK".to_owned()
            } else {
                ascii_raw
            };
            let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
            let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
            if let Ok(png) = crate::preview_push::make_music_card_png(
                &crate::preview_push::MusicCardSpec {
                    title_ascii: title_ascii.clone(),
                },
                self.shared.card_style(),
                0,
                cw,
                ch,
            ) && let Ok(tok) = self.shared.preview_store.stage_prebuilt_png(png)
            {
                let host = self.preview_http_host();
                let url = format!(
                    "http://{host}:{}/mcp/preview/{tok}.png",
                    self.shared.http_port
                );
                self.music_hud = Some(MusicHud {
                    title_ascii,
                    frame: 1,
                    next_refresh: Instant::now() + self.shared.config.preview_hold_interval,
                });
                self.send_quiet_device_call(
                    socket,
                    "self.screen.preview_image",
                    json!({ "url": url }),
                )
                .await?;
                let short_title: String = title.chars().take(48).collect();
                let _ = send_json(
                    socket,
                    &ServerMessage::Stt {
                        session_id: self.session_id.into(),
                        text: format!("[音乐] {short_title}"),
                    },
                )
                .await;
            }
        }
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
            TurnEvent::PersonResolved { scope, verified } => {
                match (&scope, verified) {
                    (Some(resolved), true) => {
                        self.last_verified_person = Some(VerifiedPersonHint {
                            scope: resolved.clone(),
                            verified_at: Instant::now(),
                            consecutive_short: 0,
                        });
                    }
                    (Some(inherited), false) => {
                        if let Some(prev) = self.last_verified_person.as_mut()
                            && prev.scope == *inherited
                        {
                            prev.consecutive_short = prev.consecutive_short.saturating_add(1);
                        } else {
                            self.last_verified_person = None;
                        }
                    }
                    (None, _) => {
                        self.last_verified_person = None;
                    }
                }
                if let Some(turn) = self.turn.as_mut() {
                    turn.person_scope = scope;
                    turn.person_verified = verified;
                }
            }
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
                self.shared
                    .state
                    .set_active_tool(self.session_id, Some(name.clone()));
                if let Some(turn) = self.turn.as_mut()
                    && let Some(timing) = turn.timing.as_mut()
                    && timing.tools.len() < 50
                {
                    // 只保留注册名称，不保存参数或工具结果。
                    timing.tools.push(crate::ToolTiming { name, ms });
                }
            }
            TurnEvent::RawHeard { raw, corrected } => {
                if let Some(turn) = self.turn.as_mut() {
                    turn.debug_raw_asr = Some(raw);
                    turn.debug_corrected_asr = Some(corrected);
                }
            }
            TurnEvent::ToolDebug {
                name,
                arguments,
                result,
            } => {
                if self.shared.state.debug_enabled()
                    && let Some(turn) = self.turn.as_mut()
                    && turn.debug_tools.len() < 8
                {
                    turn.debug_tools.push(crate::DebugToolTrace {
                        name,
                        arguments,
                        result,
                    });
                }
            }
            TurnEvent::TextReply(text) => {
                // 合成不可用仍给设备文字，不用静音音频冒充已播报。
                if let Some(turn) = self.turn.as_mut() {
                    turn.debug_text_reply.push(text.clone());
                }
                self.tts(socket, TtsState::Start, None).await?;
                self.tts(socket, TtsState::SentenceStart, Some(text))
                    .await?;
                self.tts(socket, TtsState::Stop, None).await?;
            }
            TurnEvent::Heard(text) => {
                let chars = text.chars().count();
                self.shared
                    .state
                    .set_utterance_chars(self.session_id, u32::try_from(chars).unwrap_or(u32::MAX));
                self.event(format!("识别完成：{chars} 字"));
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
                // n70 听完开始想：全屏画风切到“思考”
                if self.sticky_while_talking().is_none() {
                    self.show_screen(socket, crate::BotPose::Thinking, true)
                        .await?;
                }
            }
            TurnEvent::Ignored => {
                self.finish_timing("ignored");
                self.turn = None;
                self.event("没有识别出文字，继续拾音");
                if self.listen_manual {
                    self.phase = Phase::Idle;
                    self.set_state("idle");
                } else {
                    // 自动模式下设备仍在拾音（还没收到 tts start）
                    self.start_listening(false);
                }
            }
            TurnEvent::Emotion(emotion) if self.pack_active() => {
                // n70 刷了画风包：大模型的表情换成当前画风的对应全屏场景
                self.shared.state.set_emotion(self.session_id, emotion);
                let scene = crate::screen_pack::speaking_scene(emotion);
                self.turn_scene = Some(scene);
                // 已经开口就马上换；还没开口的等发 tts start 时一起换
                if self.sticky_while_talking().is_none()
                    && self.turn.as_ref().is_some_and(|t| t.audio_started)
                {
                    self.show_screen(socket, scene, true).await?;
                }
            }
            TurnEvent::Emotion(emotion) => {
                self.shared.state.set_emotion(self.session_id, emotion);
                let message = ServerMessage::Llm {
                    session_id: self.session_id.into(),
                    text: emotion.into(),
                    emotion: emotion.into(),
                };
                if !send_json(socket, &message).await {
                    return Err("发送失败".into());
                }
                let restore_url = self
                    .preview_hold
                    .as_ref()
                    .map(|h| h.url.clone())
                    .or_else(|| self.one_shot_badge_url.take());
                if let Some(url) = restore_url {
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
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
            TurnEvent::Failed(error) => self.event(EventRecord::from_provider(
                EventLevel::Error,
                "对话出错：",
                &error,
            )),
            TurnEvent::Warning(error) => {
                self.event(EventRecord::from_provider(EventLevel::Warn, "", &error))
            }
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
            let messages = std::mem::take(&mut turn.pending_messages);
            let turn_ctx = ToolContext {
                device_id: self.device_id.into(),
                session_id: self.session_id.into(),
            };
            if let Some(personal) = &conversation.personal_context {
                let commit_scope = turn.person_scope.as_ref().filter(|_| turn.person_verified);
                if let (Some(extraction), Some(verified_person)) =
                    (&conversation.context_extraction, commit_scope)
                {
                    extraction.turn_completed(
                        &turn_ctx,
                        Some(verified_person),
                        &messages,
                        conversation.llm.clone(),
                    );
                }
                personal.commit(
                    &turn_ctx,
                    commit_scope,
                    messages,
                    conversation.settings.history_budget,
                );
            } else {
                if let Some(extraction) = &conversation.context_extraction {
                    extraction.turn_completed(&turn_ctx, None, &messages, conversation.llm.clone());
                }
                append_history(
                    &mut self.history,
                    messages,
                    conversation.settings.history_budget,
                );
            }
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
        self.preview_hold = None;
        self.pending_oled_preview = None;
        self.one_shot_badge_url = None;
        self.music_hud = None;
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
        } else if let Some(personal) = self
            .conversation
            .as_ref()
            .and_then(|c| c.personal_context.as_ref())
        {
            let ctx = ToolContext {
                device_id: self.device_id.to_owned(),
                session_id: self.session_id.to_owned(),
            };
            if self
                .last_verified_person
                .as_ref()
                .is_some_and(|hint| !personal.permitted(&ctx, Some(&hint.scope)))
            {
                self.last_verified_person = None;
            }
        }
        if let Some(music) = self.music.as_mut()
            && music.active()
        {
            music.cancel(&mut self.pump);
        }
        if let Some(mut turn) = self.turn.take()
            && turn.kind == TurnKind::Answer
            && let Some(user) = turn.user.clone()
            && let Some(conversation) = &self.conversation
        {
            let mut messages = Vec::with_capacity(3);
            if let Some(photo) = turn.pending_photo.take() {
                messages.push(photo);
            }
            messages.push(ChatMessage::user(user));
            if !turn.spoken.is_empty() {
                messages.push(ChatMessage::assistant(turn.spoken.join("")));
            }
            if let Some(personal) = &conversation.personal_context {
                let commit_scope = turn.person_scope.as_ref().filter(|_| turn.person_verified);
                personal.commit(
                    &ToolContext {
                        device_id: self.device_id.into(),
                        session_id: self.session_id.into(),
                    },
                    commit_scope,
                    messages,
                    conversation.settings.history_budget,
                );
            } else {
                append_history(
                    &mut self.history,
                    messages,
                    conversation.settings.history_budget,
                );
            }
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
                MusicProgress::Failed(reason) => self.event(
                    EventRecord::error(DiagCategory::Audio, "音乐解码或缓冲失败，已终止播放")
                        .hint("这首歌的文件可能损坏或格式不支持：换一首试试")
                        .detail(format!("{reason:?}")),
                ),
                MusicProgress::Cancelled => self.event(EventRecord::warn(
                    DiagCategory::Device,
                    "设备权限撤销，音乐已停止",
                )),
                _ => {}
            }
        }
        loop {
            match self.pump.next_frame()? {
                PumpTick::Frame { data, marks } => {
                    // Check permission immediately before every outgoing frame,
                    // including any initial prebuffer burst.
                    if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
                        self.last_verified_person = None;
                        if let Some(music) = self.music.as_mut() {
                            music.cancel(&mut self.pump);
                        }
                        return Err("设备权限已撤销".into());
                    }
                    for text in marks {
                        self.tts(socket, TtsState::SentenceStart, Some(text.clone()))
                            .await?;
                        self.shared.state.inc_spoken_sentences(self.session_id);
                        if let Some(turn) = self.turn.as_mut() {
                            turn.spoken.push(text);
                        }
                    }
                    self.send_frame(socket, &data).await?;
                    self.set_level(self.pump.last_level());
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
                    if was_music {
                        self.music_hud = None;
                        if let Some(music) = self.music.as_mut() {
                            music.finish_after_pump_done();
                        }
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
            self.pending_oled_preview = None;
            self.preview_hold = None;
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
        self.tts(socket, TtsState::Stop, None).await?;
        if interrupted.is_none() {
            if let Some(hold_url) = self.preview_hold.as_mut().map(|hold| {
                let now = Instant::now();
                hold.until = now + self.shared.config.preview_hold_duration;
                hold.next_refresh = now + self.shared.config.preview_hold_interval;
                hold.url.clone()
            }) {
                self.send_quiet_device_call(
                    socket,
                    "self.screen.preview_image",
                    json!({ "url": hold_url }),
                )
                .await?;
            }
            if let Some(card) = self.pending_oled_preview.take() {
                let msg = ServerMessage::Stt {
                    session_id: self.session_id.into(),
                    text: format!("[预览] {card}"),
                };
                if !send_json(socket, &msg).await {
                    return Err("发送失败".into());
                }
            }
        }
        Ok(())
    }

    async fn on_binary(&mut self, socket: &mut WebSocket, data: &[u8]) -> Result<(), String> {
        let listening = matches!(self.phase, Phase::Listening(_));
        let may_interrupt = self.shared.config.barge_in
            && self.listen_realtime
            && matches!(self.phase, Phase::Thinking | Phase::Speaking { .. })
            && (self.turn.as_ref().is_some_and(|turn| {
                matches!(turn.kind, TurnKind::Answer | TurnKind::Notice) && !turn.close
            }) || self.music.as_ref().is_some_and(|music| music.active()));
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
            let head_level = crate::audio::pcm_rms_level(&head);
            if let Phase::Listening(recording) = &mut self.phase {
                recording.speaking = true;
                recording.stream = stream;
                recording.feed(&head);
                recording.utterance = head;
            }
            self.set_level(head_level);
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
        self.set_level(crate::audio::pcm_rms_level(&pcm));
        if !self.listen_manual
            && let Some(worker) = self.vad_worker.as_mut()
        {
            worker.submit(pcm, false)?;
            return Ok(());
        }
        match self.hear(&pcm, None) {
            Some((utterance, end)) => self.finish_or_hold_utterance(socket, utterance, end).await,
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
            let utterance =
                if listening.manual || listening.speaking || !listening.utterance.is_empty() {
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
        if !frame.prime
            && matches!(self.phase, Phase::Listening(_))
            && !self.listen_manual
            && let Some((pcm, end)) = self.hear(&frame.samples, Some((frame.events, frame.notice)))
        {
            self.finish_or_hold_utterance(socket, pcm, end).await?;
        }
        if self.pending_listen_stop {
            self.finish_listen_stop(socket).await?;
            return Ok(());
        }
        let reached_barrier = self
            .vad_worker
            .as_ref()
            .zip(match &self.phase {
                Phase::Listening(listening) => listening.finalize_after_vad_seq,
                _ => None,
            })
            .is_some_and(|(worker, target)| worker.completed_seq() >= target);
        if reached_barrier {
            let pcm = match &mut self.phase {
                Phase::Listening(listening) if !listening.speaking => {
                    listening.finalize_after_vad_seq = None;
                    std::mem::take(&mut listening.utterance)
                }
                _ => Vec::new(),
            };
            if !pcm.is_empty() {
                self.finish_utterance(socket, pcm, UtteranceEnd::Silence)
                    .await?;
            }
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

    /// 对话模式会话开始时向声明了 MCP 的设备发 `initialize`（回声模式按需在首次桌面端请求时握手）。
    async fn start_device_mcp(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        if self.conversation.is_none() {
            return Ok(());
        }
        self.send_mcp_initialize(socket).await
    }

    async fn send_mcp_initialize(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        if device.init_sent {
            return Ok(());
        }
        device.init_sent = true;
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
                device.vision_url_sent = true;
                device.vision_token_issued_at = Some(Instant::now());
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
        match device.pending.remove(&id).map(|(_, pending)| pending) {
            Some(McpPending::Initialize) => {
                device.initialized = true;
                let deferred = std::mem::take(&mut device.deferred_requests);
                let list = self.conversation.is_some().then(|| {
                    device.request(
                        "tools/list",
                        json!({"withUserTools": true}),
                        McpPending::ToolsList,
                    )
                });
                self.send_mcp(
                    socket,
                    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                )
                .await?;
                if let Some(list) = list {
                    self.send_mcp(socket, list).await?;
                }
                for req in deferred {
                    self.dispatch_device_request(socket, req).await?;
                }
                Ok(())
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
                            json!({"cursor": cursor, "withUserTools": true}),
                            McpPending::ToolsList,
                        );
                        self.send_mcp(socket, next).await
                    }
                    _ => {
                        device.ready = true;
                        let all_names = device.all_tool_names();
                        let count = device.model_tools().len();
                        self.shared
                            .state
                            .set_device_tools(self.session_id, all_names);
                        self.event(format!("设备提供 {count} 个自带工具"));
                        if self
                            .device_mcp
                            .as_ref()
                            .is_some_and(|d| d.has_tool("self.screen.get_info"))
                            && let Some(dev) = self.device_mcp.as_mut()
                        {
                            let req = dev.request(
                                "tools/call",
                                json!({ "name": "self.screen.get_info", "arguments": {} }),
                                McpPending::ScreenInfo,
                            );
                            self.send_mcp(socket, req).await?;
                        }
                        if let Some(prefs) =
                            self.shared.registry.device_hardware_prefs(self.device_id)
                        {
                            self.screen_pack = prefs
                                .screen_pack
                                .as_deref()
                                .is_some_and(crate::screen_pack::marker_installed);
                            self.shared
                                .state
                                .set_screen_pack(self.session_id, self.screen_pack);
                            let mut replayed = false;
                            if let Some(v) = prefs.volume
                                && self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.audio_speaker.set_volume"))
                            {
                                self.send_quiet_device_call(
                                    socket,
                                    "self.audio_speaker.set_volume",
                                    json!({"volume": v.min(100)}),
                                )
                                .await?;
                                replayed = true;
                            }
                            if let Some(b) = prefs.brightness {
                                if self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.screen.set_brightness"))
                                {
                                    self.send_quiet_device_call(
                                        socket,
                                        "self.screen.set_brightness",
                                        json!({"brightness": b.min(100)}),
                                    )
                                    .await?;
                                    replayed = true;
                                } else if self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.disp.setbacklight"))
                                {
                                    let level = ((b.min(100) as u16) * 255) / 100;
                                    self.send_quiet_device_call(
                                        socket,
                                        "self.disp.setbacklight",
                                        json!({"level": level}),
                                    )
                                    .await?;
                                    replayed = true;
                                }
                            }
                            if let Some(th) = prefs.theme
                                && self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.screen.set_theme"))
                            {
                                self.send_quiet_device_call(
                                    socket,
                                    "self.screen.set_theme",
                                    json!({"theme": th}),
                                )
                                .await?;
                                replayed = true;
                            }
                            if let Some(mode) = prefs.talk_mode
                                && self
                                    .device_mcp
                                    .as_ref()
                                    .is_some_and(|d| d.has_tool("self.set_press_to_talk"))
                            {
                                self.send_quiet_device_call(
                                    socket,
                                    "self.set_press_to_talk",
                                    json!({"mode": mode}),
                                )
                                .await?;
                                replayed = true;
                            }
                            if replayed {
                                self.dispatch_device_request(
                                    socket,
                                    crate::DeviceRequest::RefreshInfo,
                                )
                                .await?;
                            }
                        }
                        let ready_deferred = self
                            .device_mcp
                            .as_mut()
                            .map(|d| std::mem::take(&mut d.deferred_requests))
                            .unwrap_or_default();
                        for req in ready_deferred {
                            self.dispatch_device_request(socket, req).await?;
                        }
                        Ok(())
                    }
                }
            }
            Some(McpPending::Call(reply)) => {
                let _ = reply.send(device_result_text(&payload));
                Ok(())
            }
            Some(McpPending::Snapshot(kind)) => {
                let text = device_result_text(&payload);
                self.shared
                    .registry
                    .device_snapshot(self.device_id, kind, &text);
                Ok(())
            }
            Some(McpPending::Quiet(tool)) => {
                if payload.get("error").is_some()
                    || payload
                        .pointer("/result/isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    let msg = device_result_text(&payload);
                    self.event(
                        EventRecord::warn(
                            DiagCategory::Device,
                            format!("桌面端请求 {tool} 未成功：设备回复了错误"),
                        )
                        .hint("设备固件可能不支持这个操作，或设备正忙：稍后再试，必要时升级固件")
                        .detail(&msg),
                    );
                }
                Ok(())
            }
            Some(McpPending::ScreenInfo) => {
                let text = device_result_text(&payload);
                if let Ok(info) = serde_json::from_str::<Value>(&text) {
                    let mono = info.get("monochrome").and_then(Value::as_bool);
                    let width = info
                        .get("width")
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|v| (64..=1024).contains(v));
                    let height = info
                        .get("height")
                        .and_then(Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .filter(|v| (32..=1024).contains(v));
                    if let Some(dev) = self.device_mcp.as_mut() {
                        if mono.is_some() {
                            dev.screen_monochrome = mono;
                        }
                        if let (Some(w), Some(h)) = (width, height) {
                            dev.screen_size = Some((w, h));
                        }
                    }
                    self.shared
                        .state
                        .set_screen_info(self.session_id, mono, width, height);
                }
                Ok(())
            }
            Some(McpPending::CameraCapture { ticket }) => {
                let is_rpc_error = payload.get("error").is_some();
                let is_tool_error = payload
                    .pointer("/result/isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let text = device_result_text(&payload);
                let json_fail_message = if !is_rpc_error && !is_tool_error {
                    serde_json::from_str::<Value>(&text)
                        .ok()
                        .filter(|v| v.get("success").and_then(Value::as_bool) == Some(false))
                        .map(|v| {
                            v.get("message")
                                .or_else(|| v.pointer("/error/message"))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                                .unwrap_or_else(|| text.clone())
                        })
                } else {
                    None
                };
                if is_rpc_error || is_tool_error || json_fail_message.is_some() {
                    let detail = json_fail_message.unwrap_or(text);
                    let human = if detail.contains("Failed to connect to explain URL") {
                        format!(
                            "设备无法连接电脑视觉接口（{detail}），请检查电脑防火墙是否放行端口 {}",
                            self.shared.http_port
                        )
                    } else if detail.contains("Failed to capture photo") {
                        "设备摄像头抓拍失败（Failed to capture photo），请检查摄像头排线或供电"
                            .into()
                    } else {
                        detail
                    };
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        human.clone(),
                    );
                    self.event(EventRecord::warn(
                        DiagCategory::Device,
                        format!("摄像头抓拍未成功：{human}"),
                    ));
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// 把对话任务的设备工具调用转发给设备。
    /// 桌面端请求（2026-11-02）：刷新设备信息 / 重启设备。设备自带 MCP 未就绪时只记事件。
    /// 2026-11-02：记录设备实际使用的拾音模式（每次开始拾音写一次，条数有界）。
    /// 这是判断设备端 AEC 是否开启的行为证据：固件的 `GetDefaultListeningMode()` 在 AEC
    /// 开启时返回 `realtime`、关闭时返回 `auto`，所以 `mode` 比固件自称可靠。
    /// 只记录、不做任何门禁。
    fn record_listen_mode(&mut self, manual: bool, realtime: bool) {
        let mode = if realtime {
            "realtime"
        } else if manual {
            "manual"
        } else {
            "auto"
        };
        let snapshot = json!({ "mode": mode }).to_string();
        self.shared
            .registry
            .device_snapshot(self.device_id, crate::SNAPSHOT_LISTEN, &snapshot);
    }

    async fn on_device_request(
        &mut self,
        socket: &mut WebSocket,
        request: crate::DeviceRequest,
    ) -> Result<(), String> {
        if let crate::DeviceRequest::ShowScene { scene, style } = request {
            return self.show_scene_request(socket, scene, style).await;
        }
        if matches!(request, crate::DeviceRequest::PreviewScreen { .. })
            && self.device_mcp.is_none()
        {
            return self.dispatch_device_request(socket, request).await;
        }
        let should_send_init = match self.device_mcp.as_mut() {
            Some(device) if !device.initialized => {
                if device.deferred_requests.len() < 16 {
                    device.deferred_requests.push(request);
                } else if let crate::DeviceRequest::CapturePhoto { ticket } = request {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "设备待处理请求过多，请稍后再试".into(),
                    );
                }
                !device.init_sent
            }
            Some(_) => {
                return self.dispatch_device_request(socket, request).await;
            }
            None => {
                if let crate::DeviceRequest::CapturePhoto { ticket } = request {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "该设备未启用设备端 MCP，不支持摄像头抓拍".into(),
                    );
                }
                self.event(
                    EventRecord::warn(
                        DiagCategory::Device,
                        "设备没有提供自带 MCP，桌面端请求已忽略",
                    )
                    .hint("这台设备的固件不支持从电脑控制：升级到支持设备端 MCP 的小智固件"),
                );
                return Ok(());
            }
        };
        if should_send_init {
            return self.send_mcp_initialize(socket).await;
        }
        Ok(())
    }

    async fn send_quiet_device_call(
        &mut self,
        socket: &mut WebSocket,
        tool: &'static str,
        arguments: Value,
    ) -> Result<(), String> {
        let Some(device) = self.device_mcp.as_mut() else {
            return Ok(());
        };
        let payload = device.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
            McpPending::Quiet(tool),
        );
        self.send_mcp(socket, payload).await
    }

    /// n70 语音中心“推到小智试看”：已刷全屏画风包的设备直接切到该场景（按指定画风）。
    async fn show_scene_request(
        &mut self,
        socket: &mut WebSocket,
        scene: crate::BotPose,
        style: crate::ScreenCardStyle,
    ) -> Result<(), String> {
        if !self.pack_active() {
            return Ok(());
        }
        let name = crate::screen_pack::emotion_name(style, scene);
        let message = ServerMessage::Llm {
            session_id: self.session_id.into(),
            text: name.clone(),
            emotion: name.clone(),
        };
        if !send_json(socket, &message).await {
            return Err("发送失败".into());
        }
        self.last_screen_emotion = Some(name);
        self.event(format!("小智全屏试看：{}", scene.label_zh()));
        Ok(())
    }

    async fn dispatch_device_request(
        &mut self,
        socket: &mut WebSocket,
        request: crate::DeviceRequest,
    ) -> Result<(), String> {
        match request {
            crate::DeviceRequest::ShowScene { scene, style } => {
                return self.show_scene_request(socket, scene, style).await;
            }
            crate::DeviceRequest::RefreshInfo => {
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let payload = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.event("桌面端请求：self.get_device_status");
                self.send_mcp(socket, payload).await
            }
            crate::DeviceRequest::Reboot => {
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                // 重启不会回响应：设备随后断开，等它重新连接上报新的 OTA；不登记等待，免得留在 pending
                let payload = device.request_untracked(
                    "tools/call",
                    json!({ "name": "self.reboot", "arguments": {} }),
                );
                self.event("桌面端请求：self.reboot");
                self.send_mcp(socket, payload).await
            }
            crate::DeviceRequest::InstallAssets { token } => {
                if !self
                    .device_mcp
                    .as_ref()
                    .is_some_and(|d| d.has_tool("self.assets.set_download_url"))
                {
                    self.event(
                        EventRecord::warn(DiagCategory::Firmware, "设备固件不支持网络更新资源包（没有 self.assets.set_download_url），唤醒词未推送")
                            .hint("需要小智固件 2.0.2 以上、v2 分区表"),
                    );
                    return Ok(());
                }
                let host = self.preview_http_host();
                let url = format!(
                    "http://{host}:{}/xiaozhi/assets/{token}.bin",
                    self.shared.http_port
                );
                self.event("桌面端推送唤醒词资源包：设备将重启并自行下载写入");
                // 固件把两次调用按顺序排进主循环：先记下下载地址，再在 1 秒后重启。
                self.send_quiet_device_call(
                    socket,
                    "self.assets.set_download_url",
                    json!({ "url": url }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let payload = device.request_untracked(
                    "tools/call",
                    json!({ "name": "self.reboot", "arguments": {} }),
                );
                self.send_mcp(socket, payload).await
            }
            crate::DeviceRequest::CapturePhoto { ticket } => {
                if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
                    self.shared.camera_preview.clear_device(self.device_id);
                    return Err("设备授权已撤销".into());
                }
                if self.conversation.is_some() && self.device_mcp.as_ref().is_some_and(|d| !d.ready)
                {
                    if let Some(dev) = self.device_mcp.as_mut() {
                        if dev.deferred_requests.len() < 16 {
                            dev.deferred_requests
                                .push(crate::DeviceRequest::CapturePhoto { ticket });
                        } else {
                            self.shared.camera_preview.record_error(
                                self.device_id,
                                Some(&ticket),
                                "设备工具清单同步繁忙，请稍后再试".into(),
                            );
                        }
                    }
                    return Ok(());
                }
                if self
                    .device_mcp
                    .as_ref()
                    .is_some_and(|d| d.ready && !d.has_tool("self.camera.take_photo"))
                {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "该小智固件未上报摄像头工具（self.camera.take_photo）".into(),
                    );
                    return Ok(());
                }
                if self.client_id.trim().is_empty() {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "设备连接未携带 Client-Id，无法建立摄像头画面上传凭证".into(),
                    );
                    return Ok(());
                }
                self.last_activity = Instant::now();
                let need_vision_init = self.device_mcp.as_ref().is_some_and(|d| {
                    !d.vision_url_sent
                        || d.vision_token_issued_at
                            .is_none_or(|at| at.elapsed() > Duration::from_secs(3600))
                });
                if need_vision_init {
                    let host = self.preview_http_host();
                    let token =
                        self.shared
                            .signer
                            .issue(self.client_id, self.device_id, crate::unix_now());
                    let params = json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "vision": {
                                "url": format!("http://{host}:{}/mcp/vision/explain", self.shared.http_port),
                                "token": token,
                            }
                        },
                        "clientInfo": {"name": "MOSS", "version": env!("CARGO_PKG_VERSION")},
                    });
                    if let Some(dev) = self.device_mcp.as_mut() {
                        let init_payload = dev.request(
                            "initialize",
                            params,
                            McpPending::Quiet("initialize_vision"),
                        );
                        dev.vision_url_sent = true;
                        dev.vision_token_issued_at = Some(Instant::now());
                        if let Err(err) = self.send_mcp(socket, init_payload).await {
                            self.shared.camera_preview.record_error(
                                self.device_id,
                                Some(&ticket),
                                format!("发送摄像头能力握手失败：{err}"),
                            );
                            return Err(err);
                        }
                    }
                }
                let Some(device) = self.device_mcp.as_mut() else {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        "该设备未启用设备端 MCP，不支持摄像头抓拍".into(),
                    );
                    return Ok(());
                };
                let question = format!("{}{ticket}", crate::vision::CAMERA_PREVIEW_PREFIX);
                let payload = device.request(
                    "tools/call",
                    json!({
                        "name": "self.camera.take_photo",
                        "arguments": { "question": question },
                    }),
                    McpPending::CameraCapture {
                        ticket: ticket.clone(),
                    },
                );
                if let Err(err) = self.send_mcp(socket, payload).await {
                    self.shared.camera_preview.record_error(
                        self.device_id,
                        Some(&ticket),
                        format!("发送摄像头抓拍指令失败：{err}"),
                    );
                    return Err(err);
                }
                Ok(())
            }
            crate::DeviceRequest::SetVolume(volume) => {
                let v = volume.min(100);
                self.event(format!("桌面端调节设备音量：{v}%"));
                self.send_quiet_device_call(
                    socket,
                    "self.audio_speaker.set_volume",
                    json!({ "volume": v }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let refresh = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.send_mcp(socket, refresh).await
            }
            crate::DeviceRequest::SetBrightness(brightness) => {
                let b = brightness.min(100);
                self.event(format!("桌面端调节屏幕亮度：{b}%"));
                let use_amoled = self.device_mcp.as_ref().is_some_and(|d| {
                    !d.has_tool("self.screen.set_brightness")
                        && d.has_tool("self.disp.setbacklight")
                });
                if use_amoled {
                    let level = ((b as u16) * 255) / 100;
                    self.send_quiet_device_call(
                        socket,
                        "self.disp.setbacklight",
                        json!({ "level": level }),
                    )
                    .await?;
                } else {
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.set_brightness",
                        json!({ "brightness": b }),
                    )
                    .await?;
                }
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let refresh = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.send_mcp(socket, refresh).await
            }
            crate::DeviceRequest::SetTheme(theme) => {
                self.event(format!("桌面端切换屏幕主题：{theme}"));
                self.send_quiet_device_call(
                    socket,
                    "self.screen.set_theme",
                    json!({ "theme": theme }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                let refresh = device.request(
                    "tools/call",
                    json!({ "name": "self.get_device_status", "arguments": {} }),
                    McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                );
                self.send_mcp(socket, refresh).await
            }
            crate::DeviceRequest::SetTalkMode(mode) => {
                self.event(format!("桌面端切换按键对话模式：{mode}"));
                self.send_quiet_device_call(
                    socket,
                    "self.set_press_to_talk",
                    json!({ "mode": mode }),
                )
                .await?;
                let Some(device) = self.device_mcp.as_mut() else {
                    return Ok(());
                };
                if device.has_tool("self.get_device_status") {
                    let refresh = device.request(
                        "tools/call",
                        json!({ "name": "self.get_device_status", "arguments": {} }),
                        McpPending::Snapshot(crate::SNAPSHOT_STATUS),
                    );
                    self.send_mcp(socket, refresh).await?;
                }
                Ok(())
            }
            crate::DeviceRequest::PreviewScreen { token, summary } => {
                let clean_card: String = summary
                    .replace(['\r', '\n'], " ")
                    .trim()
                    .chars()
                    .take(96)
                    .collect();
                let can_color_image = token.is_some()
                    && self.device_mcp.as_ref().is_some_and(|d| {
                        d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
                    });
                if let (true, Some(tok)) = (can_color_image, token) {
                    let host = self.preview_http_host();
                    let url = format!(
                        "http://{host}:{}/mcp/preview/{tok}.png",
                        self.shared.http_port
                    );
                    let now = Instant::now();
                    self.preview_hold = Some(PreviewHold {
                        url: url.clone(),
                        until: now + self.shared.config.preview_hold_duration,
                        next_refresh: now + self.shared.config.preview_hold_interval,
                    });
                    self.event("向小智彩屏推送激光预览缩略图");
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
                    if !clean_card.is_empty() {
                        if self.busy() {
                            self.pending_oled_preview = Some(clean_card);
                        } else {
                            let msg = ServerMessage::Stt {
                                session_id: self.session_id.into(),
                                text: format!("[预览] {clean_card}"),
                            };
                            if !send_json(socket, &msg).await {
                                return Err("发送失败".into());
                            }
                        }
                    }
                    return Ok(());
                }
                self.preview_hold = None;
                // 普通单色 OLED（`monochrome: true`）或未启用彩屏图片工具的官方固件：自动降级为滚动文字预览卡
                let card = if clean_card.is_empty() {
                    "激光雕刻预览已就绪".to_owned()
                } else {
                    clean_card
                };
                if self.busy() {
                    self.pending_oled_preview = Some(card);
                    self.event("已暂存小智 OLED 雕刻预览文字卡（本轮语音播完后自动上屏）");
                    Ok(())
                } else if matches!(self.phase, Phase::Listening(_)) {
                    self.event("向小智 OLED 屏幕静默刷新雕刻预览参数字幕");
                    let msg = ServerMessage::Stt {
                        session_id: self.session_id.into(),
                        text: format!("[预览] {card}"),
                    };
                    if send_json(socket, &msg).await {
                        Ok(())
                    } else {
                        Err("发送失败".into())
                    }
                } else {
                    self.event("向小智 OLED 屏幕推送雕刻预览文字卡");
                    let alert = ServerMessage::Alert {
                        status: "雕刻预览".into(),
                        message: card,
                        emotion: "cool".into(),
                    };
                    if send_json(socket, &alert).await {
                        Ok(())
                    } else {
                        Err("发送失败".into())
                    }
                }
            }
        }
    }

    async fn forward_device_call(
        &mut self,
        socket: &mut WebSocket,
        call: Option<DeviceCall>,
    ) -> Result<(), String> {
        if !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            self.last_verified_person = None;
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
                if manual {
                    self.preview_hold = None;
                }
                self.record_listen_mode(manual, realtime);
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
                // n70 固件进入聆听时把表情重置成 neutral：换回常驻场景或“聆听”
                self.show_ambient(socket, crate::BotPose::Listening).await?;
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
                let reason = reason.unwrap_or_else(|| "设备打断".into());
                self.preview_hold = None;
                self.pending_oled_preview = None;
                self.one_shot_badge_url = None;
                self.music_hud = None;
                if self.busy() {
                    self.interrupt(socket, &reason).await?;
                } else if matches!(self.phase, Phase::Listening(_)) {
                    self.pending_listen_stop = false;
                    self.hold_check = None;
                    if let Some(worker) = self.vad_worker.as_mut() {
                        worker.reset();
                    }
                    self.phase = Phase::Idle;
                    self.set_state("idle");
                    self.event(format!("拾音已取消（{reason}）"));
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
            Err(error) => self.event(
                EventRecord::warn(
                    DiagCategory::Device,
                    "收到设备发来的一条看不懂的消息，已忽略",
                )
                .hint("通常是设备固件和 MOSS 版本不匹配；偶尔一次不影响使用，频繁出现请升级固件")
                .detail(&error),
            ),
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

    /// 正在拾音或存在尚未提交的半句录音/续听判定时，延后加工完成播报，待本轮对话安全结束后再播。
    fn listening_occupied(&self) -> bool {
        if self.hold_check.is_some() || self.pending_listen_stop {
            return true;
        }
        match &self.phase {
            Phase::Listening(listening) => {
                listening.manual
                    || !self.listen_realtime
                    || listening.speaking
                    || !listening.utterance.is_empty()
                    || listening.hold_deadline.is_some()
                    || listening.finalize_after_vad_seq.is_some()
                    || self
                        .vad_worker
                        .as_ref()
                        .is_some_and(crate::audio::worker::VadWorker::pending)
            }
            _ => false,
        }
    }
    fn preview_http_host(&self) -> String {
        self.vision_host
            .clone()
            .or_else(|| crate::ota::local_ip().map(|ip| ip.to_string()))
            .unwrap_or_else(|| "127.0.0.1".into())
    }

    fn actively_speaking_user(&self) -> bool {
        if self.hold_check.is_some() || self.pending_listen_stop {
            return true;
        }
        match &self.phase {
            Phase::Listening(listening) => {
                listening.manual
                    || listening.speaking
                    || !listening.utterance.is_empty()
                    || listening.hold_deadline.is_some()
                    || listening.finalize_after_vad_seq.is_some()
            }
            _ => false,
        }
    }

    /// 彩屏预览确认窗口内的 4 秒无声续期：重置官方固件 5000ms `preview_timer_`，用户一旦开口或到期立即停止。
    async fn on_preview_hold_tick(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        let now = Instant::now();
        let speaking = self.actively_speaking_user();
        let Some(hold) = self.preview_hold.as_mut() else {
            return Ok(());
        };
        if now >= hold.until || speaking {
            self.preview_hold = None;
            return Ok(());
        }
        let url = hold.url.clone();
        hold.next_refresh = now + self.shared.config.preview_hold_interval;
        if !self.busy() {
            self.send_quiet_device_call(socket, "self.screen.preview_image", json!({ "url": url }))
                .await?;
        }
        Ok(())
    }

    /// 本地音乐播放期间每 4 秒刷新一次黑胶旋转 / 护目镜小机器人听歌摇摆帧；停播自动停止。
    async fn on_music_hud_tick(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        let music_active = self.music.as_ref().is_some_and(|m| m.active());
        let can_color = self.device_mcp.as_ref().is_some_and(|d| {
            d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
        });
        if !music_active || !can_color {
            self.music_hud = None;
            return Ok(());
        }
        let Some((title_ascii, frame)) = self.music_hud.as_mut().map(|hud| {
            let current = hud.frame;
            hud.frame = hud.frame.wrapping_add(1);
            hud.next_refresh = Instant::now() + self.shared.config.preview_hold_interval;
            (hud.title_ascii.clone(), current)
        }) else {
            return Ok(());
        };
        let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
        let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
        if let Ok(png) = crate::preview_push::make_music_card_png(
            &crate::preview_push::MusicCardSpec { title_ascii },
            self.shared.card_style(),
            frame,
            cw,
            ch,
        ) && let Ok(tok) = self.shared.preview_store.stage_prebuilt_png(png)
        {
            let host = self.preview_http_host();
            let url = format!(
                "http://{host}:{}/mcp/preview/{tok}.png",
                self.shared.http_port
            );
            self.send_quiet_device_call(socket, "self.screen.preview_image", json!({ "url": url }))
                .await?;
        }
        Ok(())
    }

    fn stage_terminal_badge_url(&self, job: &crate::jobs::FinishedJob) -> Option<String> {
        let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
        let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
        let completed = job.status == "completed";
        let lines = if completed {
            job.total_lines
        } else {
            job.lines_sent
        };
        let png = crate::preview_push::make_terminal_badge_png(
            &crate::preview_push::TerminalBadgeSpec { completed, lines },
            self.shared.card_style(),
            cw,
            ch,
        )
        .ok()?;
        let tok = self.shared.preview_store.stage_prebuilt_png(png).ok()?;
        let host = self.preview_http_host();
        Some(format!(
            "http://{host}:{}/mcp/preview/{tok}.png",
            self.shared.http_port
        ))
    }

    /// 加工结束播报、终态 5 秒徽章卡、实时加工副屏 HUD 与待电脑审批警戒卡（全流程零 Flash 擦写）：
    /// - 只读 `laser_workflow_tool(action=recent)`（进程内合并 `stop-targets` 与无敏待审批状态）；
    /// - 任务从 `sending` 进入终态时：彩屏弹出 5 秒完工庆祝/急停断点徽章卡（自动淡出）并语音播报；
    /// - 任务处于 `sending` 时：彩屏刷新双画风实时进度副屏 HUD；
    /// - 严格模式有待电脑批准请求时：彩屏刷新待电脑审批警戒卡（含倒计时条）。
    async fn poll_jobs(&mut self, socket: &mut WebSocket) -> Result<(), String> {
        self.next_job_poll = Instant::now() + self.shared.config.job_poll_interval;
        if self.busy() || !self.permission.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        // n70 完工 / 急停画面到时间了：回到聆听或待命
        if self
            .screen_scene
            .as_ref()
            .is_some_and(|scene| scene.until.is_some_and(|t| Instant::now() >= t))
            && let Some(scene) = self.screen_scene.as_ref().map(|s| s.scene)
        {
            self.clear_sticky(socket, &[scene]).await?;
        }
        let Some(conversation) = self.conversation.clone() else {
            return Ok(());
        };
        let want_announce = conversation.settings.announce_job_done;
        let want_hud = conversation.settings.screen_preview_sync;
        if !want_announce && !want_hud {
            return Ok(());
        }
        // 若用户正在说话或处于非实时拾音占线态，延后轮询以免丢失终态边沿或干扰拾音
        if self.listening_occupied() {
            return Ok(());
        }
        let context = ToolContext {
            device_id: self.device_id.to_owned(),
            session_id: self.session_id.to_owned(),
        };
        let snap = self
            .jobs
            .poll_full(conversation.tools.as_ref(), &context)
            .await;
        let can_color_hud = self.device_mcp.as_ref().is_some_and(|d| {
            d.has_tool("self.screen.preview_image") && d.screen_monochrome != Some(true)
        });
        let is_mono_oled = self
            .device_mcp
            .as_ref()
            .is_some_and(|d| d.screen_monochrome == Some(true));

        if !snap.finished.is_empty() {
            for job in snap.finished {
                if want_announce {
                    if !self.shared.job_announcements.claim(&job.workflow_id) {
                        continue;
                    }
                    self.preview_hold = None;
                    self.last_hud_workflow = None;
                    self.last_approval_summary = None;
                    self.engrave_preview = None;
                    if want_hud && self.pack_active() {
                        self.show_finished_scene(socket, &job).await?;
                    } else if want_hud
                        && can_color_hud
                        && let Some(badge_url) = self.stage_terminal_badge_url(&job)
                    {
                        self.one_shot_badge_url = Some(badge_url.clone());
                        self.send_quiet_device_call(
                            socket,
                            "self.screen.preview_image",
                            json!({ "url": badge_url }),
                        )
                        .await?;
                    }
                    self.event("加工任务已结束，播报结果");
                    self.start_turn(
                        TurnInput::Say {
                            text: job.speech(),
                            close: false,
                        },
                        TurnKind::Notice,
                    );
                    return Ok(());
                } else if want_hud && self.pack_active() {
                    self.preview_hold = None;
                    self.last_hud_workflow = None;
                    self.engrave_preview = None;
                    self.show_finished_scene(socket, &job).await?;
                    return Ok(());
                } else if want_hud && can_color_hud {
                    self.preview_hold = None;
                    self.last_hud_workflow = None;
                    if let Some(badge_url) = self.stage_terminal_badge_url(&job) {
                        self.send_quiet_device_call(
                            socket,
                            "self.screen.preview_image",
                            json!({ "url": badge_url }),
                        )
                        .await?;
                    }
                    return Ok(());
                }
            }
        }

        if let Some(progress) = snap.active_job.filter(|_| want_hud) {
            self.preview_hold = None;
            self.last_approval_summary = None;
            let frame = self.hud_frame;
            self.hud_frame = self.hud_frame.wrapping_add(1);
            // n70 开始雕刻后才取这个任务的刀路预览（每个任务只取一次）
            if self
                .engrave_preview
                .as_ref()
                .is_none_or(|(id, _)| id != &progress.workflow_id)
            {
                let bytes = conversation
                    .tools
                    .workflow_preview(&context, &progress.workflow_id)
                    .await
                    .map(Arc::new);
                self.engrave_preview = Some((progress.workflow_id.clone(), bytes));
            }
            let toolpath = self.engrave_preview.as_ref().and_then(|(_, b)| b.clone());
            if self.pack_active() {
                // 全屏画风：先把底层切到“雕刻”并叠一行进度字，再在中央盖刀路进度卡（有刀路时）
                self.set_sticky(
                    socket,
                    crate::BotPose::GogglesEngrave,
                    Some(format!("[雕刻中] {}", progress.subtitle())),
                    None,
                )
                .await?;
            }
            if can_color_hud && (toolpath.is_some() || !self.pack_active()) {
                let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
                let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
                let png = match toolpath.as_deref() {
                    Some(source) => crate::preview_push::make_engrave_progress_png(
                        source,
                        &progress.to_hud_spec(),
                        self.shared.card_style(),
                        frame,
                        cw,
                        ch,
                    )
                    .or_else(|_| {
                        crate::preview_push::make_job_hud_png_for_style(
                            &progress.to_hud_spec(),
                            self.shared.card_style(),
                            frame,
                            cw,
                            ch,
                        )
                    }),
                    None => crate::preview_push::make_job_hud_png_for_style(
                        &progress.to_hud_spec(),
                        self.shared.card_style(),
                        frame,
                        cw,
                        ch,
                    ),
                };
                if let Ok(png) = png
                    && let Ok(tok) = self.shared.preview_store.stage_prebuilt_png(png)
                {
                    let host = self.preview_http_host();
                    let url = format!(
                        "http://{host}:{}/mcp/preview/{tok}.png",
                        self.shared.http_port
                    );
                    let first_for_job = self
                        .last_hud_workflow
                        .as_ref()
                        .is_none_or(|(id, _)| id != &progress.workflow_id);
                    self.last_hud_workflow = Some((progress.workflow_id.clone(), progress.percent));
                    self.last_activity = Instant::now();
                    if first_for_job {
                        self.event("向小智彩屏开启实时激光加工进度副屏 HUD");
                    }
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
                }
            } else if is_mono_oled {
                let should_refresh = match &self.last_hud_workflow {
                    Some((id, prev_pct)) if id == &progress.workflow_id => {
                        progress.percent.abs_diff(*prev_pct) >= 10
                    }
                    _ => true,
                };
                if should_refresh {
                    self.last_hud_workflow = Some((progress.workflow_id.clone(), progress.percent));
                    self.last_activity = Instant::now();
                    let alert = ServerMessage::Alert {
                        status: "加工中".into(),
                        message: progress.subtitle(),
                        emotion: "cool".into(),
                    };
                    let _ = send_json(socket, &alert).await;
                }
            }
            return Ok(());
        }
        self.last_hud_workflow = None;
        self.engrave_preview = None;
        self.clear_sticky(socket, &[crate::BotPose::GogglesEngrave])
            .await?;

        if let Some(pending) = snap.pending_approval.filter(|_| want_hud) {
            self.preview_hold = None;
            let frame = self.hud_frame;
            self.hud_frame = self.hud_frame.wrapping_add(1);
            let first_for_approval =
                self.last_approval_summary.as_deref() != Some(pending.summary.as_str());
            if self.pack_active() {
                // n70 全屏警戒画面一直显示，倒计时写在那行字里
                self.last_activity = Instant::now();
                if first_for_approval {
                    self.last_approval_summary = Some(pending.summary.clone());
                    self.event("小智全屏切到待电脑批准画面");
                }
                self.set_sticky(
                    socket,
                    crate::BotPose::ApprovalWave,
                    Some(format!(
                        "[待电脑批准] {} · 剩 {} 秒",
                        pending.summary, pending.remaining_secs
                    )),
                    None,
                )
                .await?;
            } else if can_color_hud {
                let screen_size = self.device_mcp.as_ref().and_then(|d| d.screen_size);
                let (cw, ch) = crate::preview_push::optimal_canvas_for_screen(screen_size);
                if let Ok(png) = crate::preview_push::make_approval_card_png(
                    &pending.to_card_spec(),
                    self.shared.card_style(),
                    frame,
                    cw,
                    ch,
                ) && let Ok(tok) = self.shared.preview_store.stage_prebuilt_png(png)
                {
                    let host = self.preview_http_host();
                    let url = format!(
                        "http://{host}:{}/mcp/preview/{tok}.png",
                        self.shared.http_port
                    );
                    self.last_activity = Instant::now();
                    self.send_quiet_device_call(
                        socket,
                        "self.screen.preview_image",
                        json!({ "url": url }),
                    )
                    .await?;
                    if first_for_approval {
                        self.last_approval_summary = Some(pending.summary.clone());
                        self.event("向小智彩屏推送待电脑审批警戒卡");
                        let _ = send_json(
                            socket,
                            &ServerMessage::Stt {
                                session_id: self.session_id.into(),
                                text: format!("[待电脑批准] {}", pending.summary),
                            },
                        )
                        .await;
                    }
                }
            } else if is_mono_oled && first_for_approval {
                self.last_approval_summary = Some(pending.summary.clone());
                self.last_activity = Instant::now();
                let _ = send_json(
                    socket,
                    &ServerMessage::Alert {
                        status: "待电脑批准".into(),
                        message: pending.summary,
                        emotion: "thinking".into(),
                    },
                )
                .await;
            }
            return Ok(());
        }
        self.last_approval_summary = None;
        self.clear_sticky(socket, &[crate::BotPose::ApprovalWave])
            .await?;
        Ok(())
    }

    /// n70 加工结束：全屏切到完工 / 急停画面 10 秒，叠一行结果；之后回到聆听或待命。
    async fn show_finished_scene(
        &mut self,
        socket: &mut WebSocket,
        job: &crate::jobs::FinishedJob,
    ) -> Result<(), String> {
        let (scene, text) = if job.status == "completed" {
            (
                crate::BotPose::CelebrateDone,
                format!("[雕刻完成] 共 {} 行", job.total_lines),
            )
        } else {
            (
                crate::BotPose::HaltedShield,
                format!("[已停止] 停在第 {} 行", job.lines_sent),
            )
        };
        self.set_sticky(socket, scene, Some(text), Some(FINISHED_SCENE_HOLD))
            .await
    }

    /// 会话循环；返回断开原因。
    async fn run(
        &mut self,
        socket: &mut WebSocket,
        requests: &mut tokio::sync::mpsc::UnboundedReceiver<crate::DeviceRequest>,
    ) -> String {
        let mut shutdown = self.shared.shutdown.clone();
        let idle_timeout = self.shared.config.idle_timeout;
        let mut requests_closed = false;
        if let Err(reason) = self.start_device_mcp(socket).await {
            return reason;
        }
        loop {
            if *shutdown.borrow() {
                return "内置语音服务已停止".into();
            }
            if let Some(device) = self.device_mcp.as_mut() {
                device.prune_pending(Instant::now());
            }
            let deadline = match &self.phase {
                Phase::Speaking { next, .. } => *next,
                // 对话任务有自己的超时（识别 / 合成 30 秒，大模型 120 秒），等待期间不算空闲
                Phase::Thinking => Instant::now() + Duration::from_secs(3600),
                _ => self.last_activity + idle_timeout,
            };
            let hold_deadline = match &self.phase {
                Phase::Listening(listening) => listening.hold_deadline,
                _ => None,
            };
            let preview_hold_deadline = self.preview_hold.as_ref().map(|h| h.next_refresh);
            let music_hud_deadline = self.music_hud.as_ref().map(|m| m.next_refresh);
            // 加工结束播报与副屏 HUD 用固定节拍（不随消息重置），否则 realtime 一直上传音频时永远轮不到
            let job_deadline = self.next_job_poll;
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
                (paused_at, held) = next_listening_hold_check(&mut self.hold_check) => self.on_hold_check_done(socket, paused_at, held).await,
                call = next_device_call(&mut self.device_mcp) => self.forward_device_call(socket, call).await,
                request = async {
                    if requests_closed {
                        std::future::pending().await
                    } else {
                        requests.recv().await
                    }
                } => match request {
                    Some(request) => self.on_device_request(socket, request).await,
                    None => {
                        requests_closed = true;
                        Ok(())
                    }
                },
                _ = async {
                    match hold_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.on_hold_expired(socket).await,
                _ = async {
                    match preview_hold_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.on_preview_hold_tick(socket).await,
                _ = async {
                    match music_hud_deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => self.on_music_hud_tick(socket).await,
                _ = tokio::time::sleep_until(deadline) => {
                    match self.phase {
                        Phase::Speaking { .. } => self.tick(socket).await,
                        Phase::Thinking => Ok(()),
                        _ => self.on_idle(),
                    }
                }
                _ = tokio::time::sleep_until(job_deadline) => self.poll_jobs(socket).await,
            };
            if let Err(reason) = result {
                return reason;
            }
        }
    }
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
}
