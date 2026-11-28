//! 运行中的会话与最近事件（供语音中心显示）。事件不含 token 与音频内容。
//!
//! 2026-11-07（技术书 n68）：事件带毫秒时间、级别、分类、大白话下一步与技术细节；事件与耗时记录放在
//! [`DiagnosticsLog`] 里，由宿主（桌面端 `BuiltinVoice`）持有，服务停止、重启或崩溃重启后仍保留。
//! 错误和警告单独一个环形缓冲，日常记录再多也挤不掉它们；任何一条被覆盖都计数并显示，绝不悄悄丢。

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde::Serialize;

pub use moss_voice_providers::DiagCategory;

/// 日常记录（info）最多保留条数。
pub const MAX_INFO_EVENTS: usize = 300;
/// 错误与警告最多保留条数（独立缓冲，info 挤不掉）。
pub const MAX_PROBLEM_EVENTS: usize = 200;
/// 对话耗时最多保留轮数。
pub const MAX_TIMINGS: usize = 50;
const MAX_DEBUG_TRACES: usize = 100;
const MAX_TURN_DETAILS: usize = 100;
const MAX_DEBUG_TEXT_CHARS: usize = 2000;
const MAX_DEBUG_TOOLS: usize = 16;

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DebugToolTrace {
    pub name: String,
    pub arguments: String,
    pub result: String,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
}

/// VA3 单次工具调用的完整联调明细（含耗时与模拟门禁拦截信息）。
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TurnToolDetail {
    pub name: String,
    pub arguments: String,
    pub result: String,
    pub ms: u64,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
}

/// VA3 播放欠载与卡顿统计。
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PlaybackDetail {
    pub frames: u64,
    pub underruns: u64,
    pub stall_ms: u64,
}

fn default_true() -> bool {
    true
}

/// VA3/VC3 每轮完整联调明细（支持轮次运行中与结束后经本机调试入口查询；正文已脱敏）。
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TurnDetail {
    pub turn_id: String,
    pub device_id: String,
    pub session_id: String,
    pub turn: u64,
    pub simulated: bool,
    #[serde(default = "default_true")]
    pub audio_channel_open: bool,
    #[serde(default)]
    pub bench_window: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulated_label: Option<String>,
    #[serde(default)]
    pub digital_audio: bool,
    #[serde(default)]
    pub input_source: String,
    #[serde(default)]
    pub input_source_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_audio_file: Option<String>,
    pub status: String,
    pub outcome: String,
    pub route: String,
    pub injected_text: String,
    pub corrected_text: String,
    pub stages_ms: BTreeMap<String, u64>,
    pub tools: Vec<TurnToolDetail>,
    pub reply: String,
    pub spoken: String,
    pub playback: PlaybackDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_skipped_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_began_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_end_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_audio_frame_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_uplink_level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus_turn_id: Option<String>,
}

/// 已脱敏的语音对话记录（说了什么、调了什么工具、回了什么）。
/// 桌面端默认开启并持久化到 SQLite `voice_chat_logs`，内存同时缓存最近 100 轮；关闭开关立即停写并清内存。
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TurnDebugTrace {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub at_ms: u64,
    #[serde(default)]
    pub device_id: String,
    pub session_id: String,
    pub turn: u64,
    pub at: u64,
    pub raw_asr: String,
    pub corrected_asr: String,
    pub tools: Vec<DebugToolTrace>,
    pub reply: String,
    /// 实际已下发 `sentence_start` 到设备屏幕播报的句子文本（被打断时可能短于 `reply`）。
    #[serde(default)]
    pub spoken: String,
    pub outcome: String,
    /// 有文字被截到上限、或工具调用超过上限只留前若干个时为真（界面显示“已截断”）。
    #[serde(default)]
    pub truncated: bool,
    /// 超过上限而没有保留的工具调用数。
    #[serde(default)]
    pub tools_dropped: usize,
    /// VA2 #3 模拟联调轮次标记：前端对话记录面板据此标识模拟轮次。
    #[serde(default)]
    pub simulated: bool,
    /// VC1 整体测试时间窗标记：窗口内真实音频轮次按模拟处理，前端对话记录显示“模拟（整体测试）”。
    #[serde(default)]
    pub bench_window: bool,
    /// 本轮音源标识（`live_mic` / `e2e_playback` / `digital_inject` / `text_inject`）。
    #[serde(default)]
    pub input_source: String,
    /// 完整过程记录开启时落盘的用户录音相对路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_audio_file: Option<String>,
}

pub fn scrub_debug_text(raw: &str) -> String {
    scrub_debug_text_flagged(raw).0
}

/// 同 [`scrub_debug_text`]，另返回是否截断过（超过 2000 字只留前 2000 字时为真）。
pub fn scrub_debug_text_flagged(raw: &str) -> (String, bool) {
    moss_voice_providers::scrub_text_with_options(raw, false, MAX_DEBUG_TEXT_CHARS, false)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolTiming {
    pub name: String,
    pub ms: u64,
}

/// 内存诊断：受控字段，不保存对话、音频、地址或凭据。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TurnTiming {
    pub session_id: String,
    /// 本轮所在设备（2026-11-07 n68：排障时要知道是哪台小智）。
    pub device_id: String,
    /// 本轮结束时间（Unix 毫秒）。
    pub at_ms: u64,
    pub turn: u64,
    pub models: Vec<String>,
    /// 相对本轮开始拾音的毫秒数；手动模式没有 VAD 点。
    pub stages: BTreeMap<String, u64>,
    pub tools: Vec<ToolTiming>,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionView {
    pub session_id: String,
    pub device_id: String,
    pub ip: String,
    pub protocol_version: u32,
    pub connected_at: u64,
    /// `idle` / `listening` / `thinking` / `speaking`
    pub state: String,
    /// 当前 60 ms 音频帧的归一化音量（0..=100）；`idle` / `thinking` 时为 0。不含音频样本。
    #[serde(default)]
    pub level: u8,
    /// 当前轮次下发给设备的情绪标识（如 `neutral` / `happy` / `thinking`）。
    #[serde(default)]
    pub emotion: String,
    /// 当前轮次最近调用的已注册工具名（不含参数与工具结果）。
    #[serde(default)]
    pub active_tool: Option<String>,
    /// 当前或最近一轮识别完成的字数（对齐事件“识别完成：N 字”，不保存识别原文）。
    #[serde(default)]
    pub utterance_chars: u32,
    /// 当前轮次已开始播报的句子数（不保存句子原文）。
    #[serde(default)]
    pub spoken_sentences: u32,
    /// 当前连接是否声明启用了设备端 MCP（用于区分“工具清单仍在读取”与“固件未启用 MCP”）。
    #[serde(default)]
    pub device_mcp_enabled: bool,
    /// 设备端 `tools/list(withUserTools=true)` 是否已完成（未启用设备端 MCP 时恒为 `true`）。
    #[serde(default)]
    pub device_tools_ready: bool,
    /// 当前连接设备通过 `tools/list(withUserTools=true)` 发现的工具名称列表（用于桌面端按能力渲染在线控制条）。
    #[serde(default)]
    pub device_tools: Vec<String>,
    /// 设备屏幕是否为单色屏（由固件 `self.screen.get_info` 的 `monochrome` 字段上报；`Some(true)` 为普通单色 OLED）。
    #[serde(default)]
    pub screen_monochrome: Option<bool>,
    /// 设备屏幕像素宽高（由固件 `self.screen.get_info` 上报，如立创·实战派 `320×240`）。
    #[serde(default)]
    pub screen_width: Option<u32>,
    #[serde(default)]
    pub screen_height: Option<u32>,
    /// n70 这台设备已刷 MOSS 全屏画风包（对话与加工画面按全屏场景切换）。
    #[serde(default)]
    pub screen_pack: bool,
    /// n71 连接方式：`websocket`（唤醒时才连）/ `mqtt`（常连模式）。
    #[serde(default)]
    pub transport: String,
    /// n146 语音音频通道是否已打开（WebSocket 恒为 `true`；常连 MQTT 待机为 `false`，唤醒完成 `hello` 后为 `true`，`goodbye` 后恢复 `false`）。
    #[serde(default)]
    pub audio_channel_open: bool,
    /// n81 设备在 `hello` 中上报的文字字库字号与位深（供推送资源包选匹配的内置中文字库）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_font_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_font_bpp: Option<u8>,
    /// VC1 整体测试时间窗状态（窗口打开期间该设备全部轮次强制按 simulated 处理，物理动作锁定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bench_window: Option<crate::debug_api::BenchWindowView>,
    /// 最近发起的对话轮次编号（0 表示尚无轮次，供本机联调脚本跟踪新轮次）。
    #[serde(default)]
    pub latest_turn: u64,
    /// 最近约 3 秒内上行拾音帧的峰值音量（0..=100，供整体测试校准判断摆放与音量）。
    #[serde(default)]
    pub recent_peak_level: u8,
}

/// 事件级别。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    #[default]
    Info,
    Warn,
    Error,
}

/// 事件来自哪里（2026-11-09 用户要求：语音中心“记录与诊断”同时显示三类，前面标明来源，方便判断是哪里出的错）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagSource {
    /// 小智设备与对话链路（连接、识别、大模型、合成、设备请求等）
    #[default]
    Xiaozhi,
    /// 激光雕刻机（握手、加工完成 / 取消 / 失败、报警、急停、走边框）
    Laser,
    /// MOSS 软件自身（服务启停、端口、本地模型进程、更新、配置与内部错误）
    Moss,
}

/// 一条诊断事件（语音中心“记录与诊断”与诊断包）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServerEvent {
    /// 递增序号（两个缓冲合并排序、前端去重用）。
    pub seq: u64,
    /// Unix 秒（兼容旧界面）。
    pub at: u64,
    /// Unix 毫秒。
    pub at_ms: u64,
    pub level: EventLevel,
    pub category: DiagCategory,
    /// 来源：小智 / 激光雕刻 / MOSS 软件
    pub source: DiagSource,
    /// 大白话说明发生了什么。
    pub text: String,
    /// 大白话的下一步建议。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 原始技术信息（已脱敏），排障时对照。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 待写入的一条事件；用 [`EventRecord::info`] / [`EventRecord::warn`] / [`EventRecord::error`] 构造。
#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    pub level: EventLevel,
    pub category: DiagCategory,
    pub source: DiagSource,
    pub text: String,
    pub hint: Option<String>,
    pub detail: Option<String>,
}

impl EventRecord {
    pub fn new(level: EventLevel, category: DiagCategory, text: impl Into<String>) -> Self {
        Self {
            level,
            category,
            // 程序内部错误一律算 MOSS 软件；其余默认算小智链路，宿主与激光侧写入时显式改来源
            source: if category == DiagCategory::Internal {
                DiagSource::Moss
            } else {
                DiagSource::Xiaozhi
            },
            text: text.into(),
            hint: None,
            detail: None,
        }
    }
    pub fn info(category: DiagCategory, text: impl Into<String>) -> Self {
        Self::new(EventLevel::Info, category, text)
    }
    pub fn warn(category: DiagCategory, text: impl Into<String>) -> Self {
        Self::new(EventLevel::Warn, category, text)
    }
    pub fn error(category: DiagCategory, text: impl Into<String>) -> Self {
        Self::new(EventLevel::Error, category, text)
    }
    /// 指定来源（小智 / 激光雕刻 / MOSS 软件）。
    pub fn source(mut self, source: DiagSource) -> Self {
        self.source = source;
        self
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        self.hint = (!hint.trim().is_empty()).then_some(hint);
        self
    }
    /// 技术细节，写入前脱敏。
    pub fn detail(mut self, detail: impl AsRef<str>) -> Self {
        let detail = moss_voice_providers::scrub_diagnostic_text(detail.as_ref());
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }
    /// 由提供方错误生成：沿用它的分类、建议与技术细节。
    pub fn from_provider(
        level: EventLevel,
        prefix: &str,
        error: &moss_voice_providers::ProviderError,
    ) -> Self {
        Self {
            level,
            category: error.category,
            source: DiagSource::Xiaozhi,
            text: format!("{prefix}{}", error.message),
            hint: error.hint.clone(),
            detail: error.detail.clone(),
        }
    }
}

impl From<String> for EventRecord {
    fn from(text: String) -> Self {
        Self::info(DiagCategory::General, text)
    }
}

impl From<&str> for EventRecord {
    fn from(text: &str) -> Self {
        Self::info(DiagCategory::General, text)
    }
}

/// 被覆盖（丢弃）的事件计数：前端据此显示“有 N 条较早的记录已被覆盖”。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct EventDrops {
    /// 被覆盖的日常记录条数
    pub info: u64,
    /// 被覆盖的错误与警告条数
    pub problems: u64,
    /// 被覆盖的耗时记录轮数
    pub timings: u64,
    pub max_info: usize,
    pub max_problems: usize,
    pub max_timings: usize,
}

#[derive(Default)]
struct LogInner {
    next_seq: u64,
    info: VecDeque<ServerEvent>,
    problems: VecDeque<ServerEvent>,
    timings: VecDeque<TurnTiming>,
    drops: EventDrops,
}

/// 诊断记录本：事件（info 与 warn/error 分开两个环形缓冲）和对话耗时。
///
/// 只在内存；宿主持有同一个 `Arc`，传给每次启动的服务（[`crate::ServerConfig::diagnostics`]），
/// 所以停止、重启和崩溃重启都不会清空。退出 MOSS 时随进程结束。
pub struct DiagnosticsLog {
    inner: Mutex<LogInner>,
    /// n105 完整过程记录（默认关）：开着时每条事件与每轮耗时另写一份到 `logs/process-trace/`，
    /// 退出 MOSS 后仍可查（内存里的这份照旧随进程结束）
    trace: std::sync::OnceLock<laser_runtime::ProcessTrace>,
}

impl Default for DiagnosticsLog {
    fn default() -> Self {
        Self {
            inner: Mutex::new(LogInner {
                drops: EventDrops {
                    max_info: MAX_INFO_EVENTS,
                    max_problems: MAX_PROBLEM_EVENTS,
                    max_timings: MAX_TIMINGS,
                    ..EventDrops::default()
                },
                ..LogInner::default()
            }),
            trace: std::sync::OnceLock::new(),
        }
    }
}

impl DiagnosticsLog {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 宿主启动时接上完整过程记录器（只接一次）。
    pub fn set_process_trace(&self, trace: laser_runtime::ProcessTrace) {
        let _ = self.trace.set(trace);
    }

    fn trace_on(&self) -> Option<&laser_runtime::ProcessTrace> {
        self.trace.get().filter(|trace| trace.enabled())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LogInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn push(&self, record: impl Into<EventRecord>) {
        self.push_in_flow(record, None);
    }

    /// 同 [`Self::push`]；`flow` 为完整过程记录里这条事件所属的流程（会话事件带上“语音·设备·第几轮”）。
    pub fn push_in_flow(&self, record: impl Into<EventRecord>, flow: Option<String>) {
        let record = record.into();
        let at_ms = crate::unix_now_ms();
        let mut inner = self.lock();
        inner.next_seq += 1;
        let event = ServerEvent {
            seq: inner.next_seq,
            at: at_ms / 1000,
            at_ms,
            level: record.level,
            category: record.category,
            source: record.source,
            text: record.text,
            hint: record.hint,
            detail: record.detail,
        };
        let traced = self
            .trace_on()
            .map(|trace| (trace.clone(), serde_json::to_value(&event)));
        if event.level == EventLevel::Info {
            inner.info.push_back(event);
            while inner.info.len() > MAX_INFO_EVENTS {
                inner.info.pop_front();
                inner.drops.info += 1;
            }
        } else {
            inner.problems.push_back(event);
            while inner.problems.len() > MAX_PROBLEM_EVENTS {
                inner.problems.pop_front();
                inner.drops.problems += 1;
            }
        }
        drop(inner);
        if let Some((trace, Ok(mut value))) = traced {
            crate::session::scrub_trace_value(&mut value);
            let step = crate::session::event_step_text(&value);
            let flow = flow.unwrap_or_else(|| crate::session::event_flow(&value));
            trace.record("diag", &flow, "event", &step, value);
        }
    }

    /// 按指定来源写入（宿主写 MOSS 软件事件、激光侧写激光雕刻事件时用）。
    pub fn push_from(&self, source: DiagSource, record: impl Into<EventRecord>) {
        self.push(record.into().source(source));
    }

    /// 全部保留的事件，新的在前（两个缓冲按序号合并）。
    pub fn events(&self) -> Vec<ServerEvent> {
        let inner = self.lock();
        let mut all: Vec<ServerEvent> = inner
            .info
            .iter()
            .chain(inner.problems.iter())
            .cloned()
            .collect();
        all.sort_by_key(|entry| std::cmp::Reverse(entry.seq));
        all
    }

    pub fn drops(&self) -> EventDrops {
        self.lock().drops
    }

    pub fn finish_timing(&self, timing: TurnTiming) {
        self.finish_timing_with_trace(timing, None);
    }

    pub(crate) fn finish_timing_with_trace(
        &self,
        timing: TurnTiming,
        trace_override: Option<&laser_runtime::ProcessTrace>,
    ) {
        let trace = trace_override
            .filter(|t| t.enabled())
            .cloned()
            .or_else(|| self.trace_on().cloned());
        if let Some(trace) = trace
            && let Ok(value) = serde_json::to_value(&timing)
        {
            let step = crate::session::timing_step_text(&value);
            let flow = crate::session::turn_flow(&timing.device_id, timing.turn);
            trace.record("voice", &flow, "timing", &step, value);
        }
        let mut inner = self.lock();
        inner.timings.push_back(timing);
        while inner.timings.len() > MAX_TIMINGS {
            inner.timings.pop_front();
            inner.drops.timings += 1;
        }
    }

    /// 耗时记录，新的在前。
    pub fn timings(&self) -> Vec<TurnTiming> {
        self.lock().timings.iter().rev().cloned().collect()
    }
}

#[derive(Default)]
pub struct LiveState {
    sessions: Mutex<BTreeMap<String, SessionView>>,
    log: Arc<DiagnosticsLog>,
    debug_enabled: std::sync::atomic::AtomicBool,
    debug_epoch: Arc<std::sync::atomic::AtomicU64>,
    debug_traces: Mutex<VecDeque<TurnDebugTrace>>,
    turn_details: Mutex<VecDeque<TurnDetail>>,
}

impl LiveState {
    /// 事件与耗时写进宿主持有的记录本（服务停止后仍保留）。
    #[cfg(test)]
    pub fn with_log(log: Arc<DiagnosticsLog>) -> Self {
        Self::with_log_and_epoch(log, Arc::new(std::sync::atomic::AtomicU64::new(0)))
    }

    pub fn with_log_and_epoch(
        log: Arc<DiagnosticsLog>,
        debug_epoch: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            log,
            debug_epoch,
            ..Self::default()
        }
    }
    #[cfg(test)]
    pub fn finish_timing(&self, timing: TurnTiming) {
        self.log.finish_timing(timing);
    }
    pub(crate) fn finish_timing_with_trace(
        &self,
        timing: TurnTiming,
        trace_override: Option<&laser_runtime::ProcessTrace>,
    ) {
        self.log.finish_timing_with_trace(timing, trace_override);
    }
    pub fn timings(&self) -> Vec<TurnTiming> {
        self.log.timings()
    }
    pub fn debug_enabled(&self) -> bool {
        self.debug_enabled
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// 在对话轮开始时绑定当前调试记录代次。
    pub(crate) fn debug_capture_epoch(&self) -> Option<u64> {
        let epoch = self.debug_epoch.load(std::sync::atomic::Ordering::Acquire);
        self.debug_enabled().then_some(epoch)
    }

    pub fn set_debug_enabled(&self, enabled: bool) {
        self.debug_enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
        if !enabled {
            self.clear_debug_traces();
        }
    }

    pub fn clear_debug_traces(&self) {
        if let Ok(mut traces) = self.debug_traces.lock() {
            traces.clear();
            self.debug_epoch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        } else {
            self.debug_epoch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[cfg(test)]
    pub fn finish_debug_trace(&self, trace: TurnDebugTrace) -> Option<TurnDebugTrace> {
        self.finish_debug_trace_with_epoch(trace, self.debug_capture_epoch()?)
            .map(|(scrubbed, _)| scrubbed)
    }

    pub fn finish_debug_trace_with_epoch(
        &self,
        mut trace: TurnDebugTrace,
        captured_epoch: u64,
    ) -> Option<(TurnDebugTrace, u64)> {
        if !self.debug_enabled() {
            return None;
        }
        let mut truncated = false;
        let mut scrub = |text: &str| {
            let (out, cut) = scrub_debug_text_flagged(text);
            truncated |= cut;
            out
        };
        if trace.at_ms == 0 {
            trace.at_ms = trace.at.saturating_mul(1000);
        }
        trace.device_id = trace.device_id.chars().take(64).collect();
        trace.raw_asr = scrub(&trace.raw_asr);
        trace.corrected_asr = scrub(&trace.corrected_asr);
        trace.reply = scrub(&trace.reply);
        trace.spoken = scrub(&trace.spoken);
        let total_tools = trace.tools.len();
        trace.tools = trace
            .tools
            .into_iter()
            .take(MAX_DEBUG_TOOLS)
            .map(|t| DebugToolTrace {
                name: t.name.chars().take(64).collect(),
                arguments: scrub(&t.arguments),
                result: scrub(&t.result),
                blocked: t.blocked,
                block_reason: t.block_reason.map(|reason| scrub(&reason)),
            })
            .collect();
        trace.tools_dropped = total_tools.saturating_sub(MAX_DEBUG_TOOLS);
        trace.truncated = truncated || trace.tools_dropped > 0;
        let epoch = if let Ok(mut traces) = self.debug_traces.lock() {
            if !self.debug_enabled() {
                traces.clear();
                return None;
            }
            if captured_epoch != self.debug_epoch.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            traces.push_back(trace.clone());
            while traces.len() > MAX_DEBUG_TRACES {
                traces.pop_front();
            }
            captured_epoch
        } else {
            return None;
        };
        Some((trace, epoch))
    }

    pub fn debug_traces(&self) -> Vec<TurnDebugTrace> {
        if !self.debug_enabled() {
            return Vec::new();
        }
        self.debug_traces
            .lock()
            .map(|items| items.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// VA3 登记一轮刚启动的联调明细（运行中即可经调试入口轮询）。
    pub fn upsert_turn_detail(&self, mut detail: TurnDetail) {
        detail.device_id = detail.device_id.chars().take(64).collect();
        detail.injected_text = scrub_debug_text(&detail.injected_text);
        detail.corrected_text = scrub_debug_text(&detail.corrected_text);
        detail.reply = scrub_debug_text(&detail.reply);
        detail.spoken = scrub_debug_text(&detail.spoken);
        detail.error = detail.error.map(|e| scrub_debug_text(&e));
        detail.stop_skipped_reason = detail.stop_skipped_reason.map(|r| scrub_debug_text(&r));
        detail.tools = detail
            .tools
            .into_iter()
            .take(MAX_DEBUG_TOOLS)
            .map(|t| TurnToolDetail {
                name: t.name.chars().take(64).collect(),
                arguments: scrub_debug_text(&t.arguments),
                result: scrub_debug_text(&t.result),
                ms: t.ms,
                blocked: t.blocked,
                block_reason: t.block_reason.map(|r| scrub_debug_text(&r)),
            })
            .collect();
        if let Ok(mut details) = self.turn_details.lock() {
            if let Some(existing) = details.iter_mut().find(|d| d.turn_id == detail.turn_id) {
                *existing = detail;
            } else {
                details.push_back(detail);
                while details.len() > MAX_TURN_DETAILS {
                    details.pop_front();
                }
            }
        }
    }

    /// VA3 按 `turn_id` 查询某一轮的完整明细（运行中或已结束）。
    pub fn turn_detail(&self, turn_id: &str) -> Option<TurnDetail> {
        self.turn_details
            .lock()
            .ok()?
            .iter()
            .rev()
            .find(|d| d.turn_id == turn_id)
            .cloned()
    }

    /// VC3 按设备编号查询最近的轮次明细（新到旧，最多 `limit` 条）。
    pub fn turn_details_for_device(&self, device_id: &str, limit: usize) -> Vec<TurnDetail> {
        let cap = limit.clamp(1, MAX_TURN_DETAILS);
        self.turn_details
            .lock()
            .map(|items| {
                items
                    .iter()
                    .rev()
                    .filter(|d| d.device_id == device_id)
                    .take(cap)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    /// 写一条事件：传字符串为日常记录（info / general），传 [`EventRecord`] 可指定级别与分类。
    pub fn event(&self, record: impl Into<EventRecord>) {
        self.log.push(record);
    }

    /// 会话里的事件：完整过程记录打开时归到这台设备这一轮的流程下。
    pub fn event_in_flow(&self, record: impl Into<EventRecord>, flow: String) {
        self.log.push_in_flow(record, Some(flow));
    }

    pub fn events(&self) -> Vec<ServerEvent> {
        self.log.events()
    }

    pub fn event_drops(&self) -> EventDrops {
        self.log.drops()
    }

    pub fn open(&self, view: SessionView) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(view.session_id.clone(), view);
        }
    }

    pub fn set_state(&self, session_id: &str, state: &str) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.state = state.to_owned();
            if state == "idle" {
                view.level = 0;
                view.active_tool = None;
                view.emotion.clear();
            } else if state == "thinking" {
                view.level = 0;
                view.active_tool = None;
                view.spoken_sentences = 0;
            } else if state == "listening" {
                view.level = 0;
                view.emotion.clear();
                view.active_tool = None;
                view.spoken_sentences = 0;
            }
        }
    }

    pub fn set_level(&self, session_id: &str, level: u8) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.level = level.min(100);
        }
    }

    pub fn set_recent_peak_level(&self, session_id: &str, peak: u8) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.recent_peak_level = peak.min(100);
        }
    }

    pub fn set_latest_turn(&self, session_id: &str, turn: u64) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.latest_turn = turn;
        }
    }

    pub fn set_audio_channel_open(&self, session_id: &str, open: bool) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.audio_channel_open = open;
        }
    }

    pub fn set_bench_window(
        &self,
        device_id: &str,
        window: Option<crate::debug_api::BenchWindowView>,
    ) {
        if let Ok(mut sessions) = self.sessions.lock() {
            for view in sessions.values_mut() {
                if view.device_id == device_id {
                    view.bench_window = window
                        .as_ref()
                        .filter(|w| w.session_id == view.session_id)
                        .cloned();
                }
            }
        }
    }

    pub fn set_emotion(&self, session_id: &str, emotion: &str) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.emotion = emotion.chars().take(32).collect();
        }
    }

    pub fn set_active_tool(&self, session_id: &str, tool: Option<String>) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.active_tool = tool.map(|name| name.chars().take(64).collect());
        }
    }

    pub fn set_utterance_chars(&self, session_id: &str, chars: u32) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.utterance_chars = chars;
        }
    }

    pub fn inc_spoken_sentences(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.spoken_sentences = view.spoken_sentences.saturating_add(1);
        }
    }

    pub fn set_device_tools(&self, session_id: &str, tools: Vec<String>) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.device_tools = tools;
            view.device_tools_ready = true;
        }
    }

    pub fn disable_device_mcp(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.device_mcp_enabled = false;
            view.device_tools_ready = true;
            view.device_tools.clear();
        }
    }

    pub fn set_screen_pack(&self, session_id: &str, installed: bool) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.screen_pack = installed;
        }
    }

    pub fn set_text_font_spec(&self, session_id: &str, size: Option<u32>, bpp: Option<u8>) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            view.text_font_size = size;
            view.text_font_bpp = bpp;
        }
    }

    pub fn set_screen_info(
        &self,
        session_id: &str,
        monochrome: Option<bool>,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(view) = sessions.get_mut(session_id)
        {
            if monochrome.is_some() {
                view.screen_monochrome = monochrome;
            }
            if let (Some(w), Some(h)) = (width, height) {
                view.screen_width = Some(w);
                view.screen_height = Some(h);
            }
        }
    }

    pub(crate) fn session_by_id(&self, session_id: &str) -> Option<SessionView> {
        self.sessions.lock().ok()?.get(session_id).cloned()
    }

    pub(crate) fn sessions_for_handles(&self, handles: &[(String, String)]) -> Vec<SessionView> {
        let Ok(sessions) = self.sessions.lock() else {
            return Vec::new();
        };
        handles
            .iter()
            .filter_map(|(dev, sid)| sessions.get(sid).filter(|s| &s.device_id == dev).cloned())
            .collect()
    }

    #[cfg(test)]
    pub fn session_screen_size(&self, device_id: &str) -> Option<(u32, u32)> {
        let sessions = self.sessions.lock().ok()?;
        sessions
            .values()
            .filter(|s| s.device_id == device_id)
            .max_by(|a, b| {
                a.connected_at
                    .cmp(&b.connected_at)
                    .then_with(|| a.session_id.cmp(&b.session_id))
            })
            .and_then(|s| s.screen_width.zip(s.screen_height))
    }
    /// 选出当前优先级最高的主会话（`speaking` > `thinking` > `listening` > `idle`，同优先级取较大 `level`），
    /// 并返回该同一会话的 `(state, level)`，避免跨设备拼接出“A 会话的 state + B 会话的 level”。
    #[cfg(test)]
    pub fn aggregate_pulse(&self) -> (Option<String>, u8) {
        let Ok(sessions) = self.sessions.lock() else {
            return (None, 0);
        };
        Self::aggregate_pulse_of(sessions.values())
    }

    pub(crate) fn aggregate_pulse_of<'a>(
        views: impl IntoIterator<Item = &'a SessionView>,
    ) -> (Option<String>, u8) {
        let rank = |state: &str| match state {
            "speaking" => 4u8,
            "thinking" => 3u8,
            "listening" => 2u8,
            _ => 1u8,
        };
        let mut any = false;
        let mut best_state = "idle";
        let mut best_key = (0u8, 0u8);
        for view in views {
            any = true;
            let r = rank(&view.state);
            let effective_level = if matches!(r, 2 | 4) {
                view.level.min(100)
            } else {
                0
            };
            let key = (r, effective_level);
            if key > best_key {
                best_key = key;
                best_state = match r {
                    4 => "speaking",
                    3 => "thinking",
                    2 => "listening",
                    _ => "idle",
                };
            }
        }
        if !any {
            return (None, 0);
        }
        (Some(best_state.to_owned()), best_key.1)
    }
    pub fn close(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(session_id);
        }
    }

    pub fn sessions(&self) -> Vec<SessionView> {
        self.sessions
            .lock()
            .map(|sessions| sessions.values().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_history_is_bounded_and_contains_only_diagnostics() {
        let state = LiveState::default();
        for turn in 0..55 {
            state.finish_timing(TurnTiming {
                session_id: "s".into(),
                device_id: "dev-1".into(),
                at_ms: 1_000 + turn,
                turn,
                models: vec!["fake-asr".into()],
                stages: BTreeMap::from([("final_asr".into(), 12)]),
                tools: vec![],
                outcome: "done".into(),
            });
        }
        assert_eq!(state.timings().len(), MAX_TIMINGS);
        assert_eq!(state.timings().last().unwrap().turn, 5);
        assert_eq!(state.event_drops().timings, 5, "被覆盖的耗时轮数如实计数");
        let v = serde_json::to_value(&state.timings()[0]).unwrap();
        assert_eq!(
            v.as_object().unwrap().keys().cloned().collect::<Vec<_>>(),
            [
                "at_ms",
                "device_id",
                "models",
                "outcome",
                "session_id",
                "stages",
                "tools",
                "turn"
            ]
        );
    }

    #[test]
    fn session_view_tracks_non_sensitive_pulse_and_resets_on_phase_change() {
        let state = LiveState::default();
        assert_eq!(state.aggregate_pulse(), (None, 0));
        state.open(SessionView {
            session_id: "s1".into(),
            device_id: "dev-1".into(),
            ip: "127.0.0.1".into(),
            protocol_version: 1,
            connected_at: 100,
            state: "idle".into(),
            level: 0,
            emotion: String::new(),
            active_tool: None,
            utterance_chars: 0,
            spoken_sentences: 0,
            device_mcp_enabled: false,
            device_tools_ready: true,
            device_tools: Vec::new(),
            screen_monochrome: None,
            screen_width: None,
            screen_height: None,
            screen_pack: false,
            transport: "websocket".into(),
            audio_channel_open: true,
            text_font_size: None,
            text_font_bpp: None,
            bench_window: None,
            latest_turn: 0,
            recent_peak_level: 0,
        });
        assert_eq!(state.aggregate_pulse(), (Some("idle".into()), 0));
        state.set_state("s1", "listening");
        state.set_level("s1", 68);
        assert_eq!(state.aggregate_pulse(), (Some("listening".into()), 68));
        state.set_utterance_chars("s1", 9);
        state.set_state("s1", "thinking");
        state.set_active_tool("s1", Some("laser_workflow_tool".into()));
        state.set_emotion("s1", "thinking");
        assert_eq!(
            state.aggregate_pulse(),
            (Some("thinking".into()), 0),
            "进入思考态时音量归零"
        );
        state.set_state("s1", "speaking");
        state.set_level("s1", 82);
        state.inc_spoken_sentences("s1");
        state.inc_spoken_sentences("s1");
        let view = &state.sessions()[0];
        assert_eq!(view.state, "speaking");
        assert_eq!(view.level, 82);
        assert_eq!(view.emotion, "thinking");
        assert_eq!(view.active_tool.as_deref(), Some("laser_workflow_tool"));
        assert_eq!(view.utterance_chars, 9);
        assert_eq!(view.spoken_sentences, 2);
        // 多设备聚合：取优先级最高会话自身的 (state, level)，不跨设备拼接
        state.open(SessionView {
            session_id: "s2".into(),
            device_id: "dev-2".into(),
            ip: "127.0.0.1".into(),
            protocol_version: 1,
            connected_at: 101,
            state: "listening".into(),
            level: 95,
            emotion: String::new(),
            active_tool: None,
            utterance_chars: 0,
            spoken_sentences: 0,
            device_mcp_enabled: false,
            device_tools_ready: true,
            device_tools: Vec::new(),
            screen_monochrome: None,
            screen_width: None,
            screen_height: None,
            screen_pack: false,
            transport: "websocket".into(),
            audio_channel_open: true,
            text_font_size: None,
            text_font_bpp: None,
            bench_window: None,
            latest_turn: 0,
            recent_peak_level: 0,
        });
        state.set_level("s1", 10);
        assert_eq!(
            state.aggregate_pulse(),
            (Some("speaking".into()), 10),
            "speaking 优先级高于 listening，level 必须来自同一 s1 会话"
        );
        state.set_state("s1", "idle");
        let s1 = state
            .sessions()
            .into_iter()
            .find(|s| s.session_id == "s1")
            .unwrap();
        assert_eq!(s1.level, 0, "回到空闲态时音量归零");
        assert_eq!(s1.active_tool, None, "回到空闲态时清理本轮工具名");
        assert_eq!(s1.emotion, "", "回到空闲态时清理本轮情绪");
        assert_eq!(state.aggregate_pulse(), (Some("listening".into()), 95));
    }

    #[test]
    fn debug_traces_stay_off_by_default_scrub_secrets_and_wipe_on_disable() {
        let state = LiveState::default();
        assert!(!state.debug_enabled());
        let sample = |turn: u64| TurnDebugTrace {
            id: 0,
            at_ms: (100 + turn) * 1000,
            device_id: "dev-1".into(),
            session_id: "s1".into(),
            turn,
            at: 100 + turn,
            raw_asr: "切 3 毫米段木 token=abc123".into(),
            corrected_asr: "切 3 毫米椴木 token=abc123".into(),
            tools: vec![DebugToolTrace {
                name: "laser_workflow_tool".into(),
                arguments: "{\"action\":\"preview\",\"material\":\"椴木\"}".into(),
                result: "ok sk-secret999".into(),
                ..DebugToolTrace::default()
            }],
            reply: "已生成预览。".into(),
            spoken: "已生成预览。".into(),
            outcome: "done".into(),
            truncated: false,
            tools_dropped: 0,
            simulated: false,
            bench_window: false,
            ..TurnDebugTrace::default()
        };
        assert!(state.finish_debug_trace(sample(1)).is_none());
        assert!(
            state.debug_traces().is_empty(),
            "默认关闭时绝不留存任何对话文本"
        );

        state.set_debug_enabled(true);
        for t in 1..=(MAX_DEBUG_TRACES as u64 + 2) {
            assert!(state.finish_debug_trace(sample(t)).is_some());
        }
        let traces = state.debug_traces();
        assert_eq!(traces.len(), MAX_DEBUG_TRACES);
        assert_eq!(traces[0].turn, MAX_DEBUG_TRACES as u64 + 2);
        assert!(traces[0].raw_asr.contains("[REDACTED]") && !traces[0].raw_asr.contains("abc123"));
        assert!(
            traces[0].tools[0].result.contains("[REDACTED]")
                && !traces[0].tools[0].result.contains("sk-secret999")
        );

        assert!(!traces[0].truncated, "短文本不标截断");

        let mut long = sample(999);
        long.reply = "很长".repeat(MAX_DEBUG_TEXT_CHARS);
        long.tools = (0..(MAX_DEBUG_TOOLS + 3))
            .map(|i| DebugToolTrace {
                name: format!("tool_{i}"),
                arguments: "{}".into(),
                result: "ok".into(),
                ..DebugToolTrace::default()
            })
            .collect();
        state.finish_debug_trace(long);
        let trace = &state.debug_traces()[0];
        assert!(trace.truncated, "回复超过上限、工具超过上限都要标“已截断”");
        assert_eq!(trace.tools.len(), MAX_DEBUG_TOOLS);
        assert_eq!(trace.tools_dropped, 3);
        assert_eq!(trace.reply.chars().count(), MAX_DEBUG_TEXT_CHARS);

        state.set_debug_enabled(false);
        assert!(state.debug_traces().is_empty(), "关闭开关立即清空内存缓存");

        // 停用与清空都会推进 epoch，便于持久化侧识别在途/过期轮次
        let epoch_shared = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let epoch_state =
            LiveState::with_log_and_epoch(DiagnosticsLog::new(), epoch_shared.clone());
        epoch_state.set_debug_enabled(true);
        let (_, ep0) = epoch_state
            .finish_debug_trace_with_epoch(sample(1), 0)
            .unwrap();
        assert_eq!(ep0, 0);
        epoch_state.clear_debug_traces();
        assert!(
            epoch_state
                .finish_debug_trace_with_epoch(sample(2), ep0)
                .is_none()
        );
        assert!(epoch_state.debug_traces().is_empty());
        let current_epoch = epoch_state.debug_capture_epoch().unwrap();
        let (_, ep1) = epoch_state
            .finish_debug_trace_with_epoch(sample(2), current_epoch)
            .unwrap();
        assert_eq!(ep1, 1);
        epoch_state.set_debug_enabled(false);
        assert_eq!(epoch_shared.load(std::sync::atomic::Ordering::Acquire), 2);
        assert!(
            epoch_state
                .finish_debug_trace_with_epoch(sample(3), ep1)
                .is_none()
        );
    }

    /// #11：空格分隔、Authorization Bearer、键值引号写法及带凭据 URL 都必须连带掩码实际值。
    #[test]
    fn finish_debug_trace_redacts_space_separated_and_kv_credentials() {
        let state = LiveState::default();
        state.set_debug_enabled(true);
        let trace = state
            .finish_debug_trace(TurnDebugTrace {
                id: 0,
                at_ms: 2_000,
                device_id: "dev-sec".into(),
                session_id: "s-sec".into(),
                turn: 1,
                at: 2,
                raw_asr: "Bearer fake_val Authorization Bearer fake_bearer2".into(),
                corrected_asr: "api_key fake_api_val password fake_pw_val --token fake_cli_tok"
                    .into(),
                tools: vec![DebugToolTrace {
                    name: "call".into(),
                    arguments: r#"{"token":"fake_json_tok","ok":"椴木"}"#.into(),
                    result: "wss://fakeuser:fakepass@example.invalid/ws?k=fake1&token=fake2".into(),
                    ..DebugToolTrace::default()
                }],
                reply: "tokens_used: 120 done".into(),
                spoken: "done".into(),
                outcome: "done".into(),
                truncated: false,
                tools_dropped: 0,
                simulated: false,
                bench_window: false,
                ..TurnDebugTrace::default()
            })
            .unwrap();
        let serialized = serde_json::to_string(&trace).unwrap();
        for leak in [
            "fake_val",
            "fake_bearer2",
            "fake_api_val",
            "fake_pw_val",
            "fake_cli_tok",
            "fake_json_tok",
            "fakeuser",
            "fakepass",
            "fake1",
            "fake2",
        ] {
            assert!(!serialized.contains(leak), "{leak} leaked in {serialized}");
        }
        assert!(serialized.contains("tokens_used: 120"));
        assert!(serialized.contains("椴木"));
    }

    /// n68：错误与警告单独缓冲，日常记录再多也挤不掉；被覆盖的条数如实计数。
    #[test]
    fn problems_survive_info_flood_and_drops_are_counted() {
        let log = DiagnosticsLog::new();
        log.push(
            EventRecord::error(DiagCategory::Auth, "大模型密钥不对")
                .hint("重新填写密钥")
                .detail("HTTP 401 token=abc"),
        );
        log.push(EventRecord::warn(
            DiagCategory::Network,
            "流式识别失败，改用整段识别",
        ));
        for i in 0..(MAX_INFO_EVENTS + 40) {
            log.push(format!("识别完成：{i} 字"));
        }
        let events = log.events();
        assert_eq!(events.len(), MAX_INFO_EVENTS + 2);
        let error = events
            .iter()
            .find(|e| e.level == EventLevel::Error)
            .expect("错误没被日常记录挤掉");
        assert_eq!(error.category, DiagCategory::Auth);
        assert_eq!(error.hint.as_deref(), Some("重新填写密钥"));
        assert!(
            error.detail.as_deref().unwrap().contains("[REDACTED]")
                && !error.detail.as_deref().unwrap().contains("abc")
        );
        assert!(error.at_ms >= error.at * 1000 && error.at_ms < (error.at + 1) * 1000);
        assert!(events.iter().any(|e| e.level == EventLevel::Warn));
        assert!(events.windows(2).all(|w| w[0].seq > w[1].seq), "新的在前");
        let drops = log.drops();
        assert_eq!(drops.info, 40);
        assert_eq!(drops.problems, 0);
        assert_eq!(
            (drops.max_info, drops.max_problems),
            (MAX_INFO_EVENTS, MAX_PROBLEM_EVENTS)
        );

        for i in 0..(MAX_PROBLEM_EVENTS + 5) {
            log.push(EventRecord::warn(
                DiagCategory::Device,
                format!("设备断开 {i}"),
            ));
        }
        assert_eq!(log.drops().problems, 7);
        assert_eq!(
            log.events()
                .iter()
                .filter(|e| e.level != EventLevel::Info)
                .count(),
            MAX_PROBLEM_EVENTS
        );

        let json = serde_json::to_value(&log.events()[0]).unwrap();
        assert_eq!(json["level"], "warn");
        assert_eq!(json["category"], "device");
        assert!(json.get("hint").is_none(), "没有建议时不输出 hint 字段");
    }

    /// 同一个记录本挂到多个 LiveState（模拟服务停止后重启），记录仍在。
    #[test]
    fn log_shared_across_live_states_keeps_history() {
        let log = DiagnosticsLog::new();
        LiveState::with_log(log.clone())
            .event(EventRecord::error(DiagCategory::Model, "第一次运行出错"));
        let restarted = LiveState::with_log(log.clone());
        restarted.event("重启后的记录");
        let texts: Vec<_> = restarted.events().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["重启后的记录", "第一次运行出错"]);
    }

    /// 2026-11-09：事件带来源。程序内部错误默认算 MOSS 软件，其余默认算小智；宿主与激光侧写入时指定来源，导出时带上。
    #[test]
    fn events_carry_their_source() {
        let log = DiagnosticsLog::new();
        log.push(EventRecord::warn(DiagCategory::Device, "设备断开"));
        log.push(EventRecord::error(DiagCategory::Internal, "数据库锁失败"));
        log.push_from(
            DiagSource::Laser,
            EventRecord::error(DiagCategory::Device, "加工失败：ALARM:2"),
        );
        log.push_from(
            DiagSource::Moss,
            EventRecord::warn(DiagCategory::Model, "本地视觉模型启动失败"),
        );
        let events = log.events();
        let source_of = |text: &str| events.iter().find(|e| e.text == text).unwrap().source;
        assert_eq!(source_of("设备断开"), DiagSource::Xiaozhi);
        assert_eq!(source_of("数据库锁失败"), DiagSource::Moss);
        assert_eq!(source_of("加工失败：ALARM:2"), DiagSource::Laser);
        assert_eq!(source_of("本地视觉模型启动失败"), DiagSource::Moss);
        let json = serde_json::to_value(&events).unwrap();
        assert!(
            json.as_array()
                .unwrap()
                .iter()
                .any(|e| e["source"] == "laser"),
            "{json}"
        );
    }
}
