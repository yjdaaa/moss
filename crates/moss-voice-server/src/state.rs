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
const MAX_DEBUG_TRACES: usize = 10;
const MAX_DEBUG_TEXT_CHARS: usize = 240;
const MAX_DEBUG_TOOLS: usize = 8;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DebugToolTrace {
    pub name: String,
    pub arguments: String,
    pub result: String,
}

/// 默认关闭、仅驻留内存（最多 10 轮，关闭或重启立即清空）的排障流；绝不落盘。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TurnDebugTrace {
    pub session_id: String,
    pub turn: u64,
    pub at: u64,
    pub raw_asr: String,
    pub corrected_asr: String,
    pub tools: Vec<DebugToolTrace>,
    pub reply: String,
    pub outcome: String,
    /// 有文字被截到 240 字、或工具调用超过 8 个只留前 8 个时为真（界面显示“已截断”）。
    #[serde(default)]
    pub truncated: bool,
    /// 超过 8 个而没有保留的工具调用数。
    #[serde(default)]
    pub tools_dropped: usize,
}

pub fn scrub_debug_text(raw: &str) -> String {
    scrub_debug_text_flagged(raw).0
}

/// 同 [`scrub_debug_text`]，另返回是否截断过（超过 240 字只留前 240 字时为真）。
pub fn scrub_debug_text_flagged(raw: &str) -> (String, bool) {
    let mut out = String::with_capacity(raw.len().min(MAX_DEBUG_TEXT_CHARS));
    let mut words = raw.split_whitespace().peekable();
    let mut truncated = false;
    while let Some(token) = words.next() {
        if !out.is_empty() {
            out.push(' ');
        }
        let lower = token.to_ascii_lowercase();
        let masked = if lower.starts_with("sk-")
            || lower.contains("token=")
            || lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("password")
            || lower.contains("secret")
            || lower.starts_with("bearer")
        {
            "[REDACTED]"
        } else {
            token
        };
        out.push_str(masked);
        let count = out.chars().count();
        if count >= MAX_DEBUG_TEXT_CHARS {
            truncated = count > MAX_DEBUG_TEXT_CHARS || words.peek().is_some();
            break;
        }
    }
    (out.chars().take(MAX_DEBUG_TEXT_CHARS).collect(), truncated)
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
    pub text: String,
    pub hint: Option<String>,
    pub detail: Option<String>,
}

impl EventRecord {
    pub fn new(level: EventLevel, category: DiagCategory, text: impl Into<String>) -> Self {
        Self {
            level,
            category,
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
        }
    }
}

impl DiagnosticsLog {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LogInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn push(&self, record: impl Into<EventRecord>) {
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
            text: record.text,
            hint: record.hint,
            detail: record.detail,
        };
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
    debug_traces: Mutex<VecDeque<TurnDebugTrace>>,
}

impl LiveState {
    /// 事件与耗时写进宿主持有的记录本（服务停止后仍保留）。
    pub fn with_log(log: Arc<DiagnosticsLog>) -> Self {
        Self {
            log,
            ..Self::default()
        }
    }
    pub fn finish_timing(&self, timing: TurnTiming) {
        self.log.finish_timing(timing);
    }
    pub fn timings(&self) -> Vec<TurnTiming> {
        self.log.timings()
    }
    pub fn debug_enabled(&self) -> bool {
        self.debug_enabled
            .load(std::sync::atomic::Ordering::Acquire)
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
        }
    }

    pub fn finish_debug_trace(&self, mut trace: TurnDebugTrace) {
        if !self.debug_enabled() {
            return;
        }
        let mut truncated = false;
        let mut scrub = |text: &str| {
            let (out, cut) = scrub_debug_text_flagged(text);
            truncated |= cut;
            out
        };
        trace.raw_asr = scrub(&trace.raw_asr);
        trace.corrected_asr = scrub(&trace.corrected_asr);
        trace.reply = scrub(&trace.reply);
        let total_tools = trace.tools.len();
        trace.tools = trace
            .tools
            .into_iter()
            .take(MAX_DEBUG_TOOLS)
            .map(|t| DebugToolTrace {
                name: t.name.chars().take(64).collect(),
                arguments: scrub(&t.arguments),
                result: scrub(&t.result),
            })
            .collect();
        trace.tools_dropped = total_tools.saturating_sub(MAX_DEBUG_TOOLS);
        trace.truncated = truncated || trace.tools_dropped > 0;
        if let Ok(mut traces) = self.debug_traces.lock() {
            if !self.debug_enabled() {
                traces.clear();
                return;
            }
            traces.push_back(trace);
            while traces.len() > MAX_DEBUG_TRACES {
                traces.pop_front();
            }
        }
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
    /// 写一条事件：传字符串为日常记录（info / general），传 [`EventRecord`] 可指定级别与分类。
    pub fn event(&self, record: impl Into<EventRecord>) {
        self.log.push(record);
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

    pub fn session_screen_size(&self, device_id: &str) -> Option<(u32, u32)> {
        let sessions = self.sessions.lock().ok()?;
        sessions
            .values()
            .find(|s| s.device_id == device_id)
            .and_then(|s| s.screen_width.zip(s.screen_height))
    }
    /// 选出当前优先级最高的主会话（`speaking` > `thinking` > `listening` > `idle`，同优先级取较大 `level`），
    /// 并返回该同一会话的 `(state, level)`，避免跨设备拼接出“A 会话的 state + B 会话的 level”。
    pub fn aggregate_pulse(&self) -> (Option<String>, u8) {
        let Ok(sessions) = self.sessions.lock() else {
            return (None, 0);
        };
        if sessions.is_empty() {
            return (None, 0);
        }
        let rank = |state: &str| match state {
            "speaking" => 4u8,
            "thinking" => 3u8,
            "listening" => 2u8,
            _ => 1u8,
        };
        let mut best_state = "idle";
        let mut best_key = (0u8, 0u8);
        for view in sessions.values() {
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
            device_tools: Vec::new(),
            screen_monochrome: None,
            screen_width: None,
            screen_height: None,
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
            device_tools: Vec::new(),
            screen_monochrome: None,
            screen_width: None,
            screen_height: None,
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
            session_id: "s1".into(),
            turn,
            at: 100 + turn,
            raw_asr: "切 3 毫米段木 token=abc123".into(),
            corrected_asr: "切 3 毫米椴木 token=abc123".into(),
            tools: vec![DebugToolTrace {
                name: "generate_text_laser_task_tool".into(),
                arguments: "{\"material\":\"椴木\"}".into(),
                result: "ok sk-secret999".into(),
            }],
            reply: "已生成预览。".into(),
            outcome: "done".into(),
            truncated: false,
            tools_dropped: 0,
        };
        state.finish_debug_trace(sample(1));
        assert!(
            state.debug_traces().is_empty(),
            "默认关闭时绝不留存任何对话文本"
        );

        state.set_debug_enabled(true);
        for t in 1..=12 {
            state.finish_debug_trace(sample(t));
        }
        let traces = state.debug_traces();
        assert_eq!(traces.len(), 10);
        assert_eq!(traces[0].turn, 12);
        assert!(traces[0].raw_asr.contains("[REDACTED]") && !traces[0].raw_asr.contains("abc123"));
        assert!(
            traces[0].tools[0].result.contains("[REDACTED]")
                && !traces[0].tools[0].result.contains("sk-secret999")
        );

        assert!(!traces[0].truncated, "短文本不标截断");

        let mut long = sample(13);
        long.reply = "很长".repeat(200);
        long.tools = (0..11)
            .map(|i| DebugToolTrace {
                name: format!("tool_{i}"),
                arguments: "{}".into(),
                result: "ok".into(),
            })
            .collect();
        state.finish_debug_trace(long);
        let trace = &state.debug_traces()[0];
        assert!(
            trace.truncated,
            "回复超过 240 字、工具超过 8 个都要标“已截断”"
        );
        assert_eq!(trace.tools.len(), 8);
        assert_eq!(trace.tools_dropped, 3);
        assert_eq!(trace.reply.chars().count(), 240);

        state.set_debug_enabled(false);
        assert!(state.debug_traces().is_empty(), "关闭开关立即清空内存缓存");
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
}
