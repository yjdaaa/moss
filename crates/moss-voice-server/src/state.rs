//! 运行中的会话与最近事件（供语音中心显示）。事件不含 token 与音频内容。

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::Serialize;

const MAX_EVENTS: usize = 100;
const MAX_TIMINGS: usize = 20;
const MAX_DEBUG_TRACES: usize = 10;
const MAX_DEBUG_TEXT_CHARS: usize = 240;

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
}

pub fn scrub_debug_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_DEBUG_TEXT_CHARS));
    for token in raw.split_whitespace() {
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
        if out.chars().count() >= MAX_DEBUG_TEXT_CHARS {
            break;
        }
    }
    out.chars().take(MAX_DEBUG_TEXT_CHARS).collect()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolTiming { pub name: String, pub ms: u64 }

/// 内存诊断：受控字段，不保存对话、音频、地址或凭据。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TurnTiming {
    pub session_id: String,
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServerEvent {
    pub at: u64,
    pub text: String,
}

#[derive(Default)]
pub struct LiveState {
    sessions: Mutex<BTreeMap<String, SessionView>>,
    events: Mutex<VecDeque<ServerEvent>>,
    timings: Mutex<VecDeque<TurnTiming>>,
    debug_enabled: std::sync::atomic::AtomicBool,
    debug_traces: Mutex<VecDeque<TurnDebugTrace>>,
}

impl LiveState {
    pub fn finish_timing(&self, timing: TurnTiming) {
        if let Ok(mut timings) = self.timings.lock() {
            timings.push_back(timing);
            while timings.len() > MAX_TIMINGS { timings.pop_front(); }
        }
    }
    pub fn timings(&self) -> Vec<TurnTiming> {
        self.timings.lock().map(|items| items.iter().rev().cloned().collect()).unwrap_or_default()
    }
    pub fn debug_enabled(&self) -> bool {
        self.debug_enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set_debug_enabled(&self, enabled: bool) {
        self.debug_enabled.store(enabled, std::sync::atomic::Ordering::Release);
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
        trace.raw_asr = scrub_debug_text(&trace.raw_asr);
        trace.corrected_asr = scrub_debug_text(&trace.corrected_asr);
        trace.reply = scrub_debug_text(&trace.reply);
        trace.tools = trace
            .tools
            .into_iter()
            .take(8)
            .map(|t| DebugToolTrace {
                name: t.name.chars().take(64).collect(),
                arguments: scrub_debug_text(&t.arguments),
                result: scrub_debug_text(&t.result),
            })
            .collect();
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
    pub fn event(&self, text: impl Into<String>) {
        if let Ok(mut events) = self.events.lock() {
            events.push_back(ServerEvent { at: crate::unix_now(), text: text.into() });
            while events.len() > MAX_EVENTS {
                events.pop_front();
            }
        }
    }

    pub fn events(&self) -> Vec<ServerEvent> {
        self.events.lock().map(|events| events.iter().rev().cloned().collect()).unwrap_or_default()
    }

    pub fn open(&self, view: SessionView) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(view.session_id.clone(), view);
        }
    }

    pub fn set_state(&self, session_id: &str, state: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
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
    }

    pub fn set_level(&self, session_id: &str, level: u8) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
                view.level = level.min(100);
            }
        }
    }

    pub fn set_emotion(&self, session_id: &str, emotion: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
                view.emotion = emotion.chars().take(32).collect();
            }
        }
    }

    pub fn set_active_tool(&self, session_id: &str, tool: Option<String>) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
                view.active_tool = tool.map(|name| name.chars().take(64).collect());
            }
        }
    }

    pub fn set_utterance_chars(&self, session_id: &str, chars: u32) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
                view.utterance_chars = chars;
            }
        }
    }

    pub fn inc_spoken_sentences(&self, session_id: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
                view.spoken_sentences = view.spoken_sentences.saturating_add(1);
            }
        }
    }

    pub fn set_device_tools(&self, session_id: &str, tools: Vec<String>) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
                view.device_tools = tools;
            }
        }
    }

    pub fn set_screen_info(
        &self,
        session_id: &str,
        monochrome: Option<bool>,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(view) = sessions.get_mut(session_id) {
                if monochrome.is_some() {
                    view.screen_monochrome = monochrome;
                }
                if let (Some(w), Some(h)) = (width, height) {
                    view.screen_width = Some(w);
                    view.screen_height = Some(h);
                }
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
            let effective_level = if matches!(r, 2 | 4) { view.level.min(100) } else { 0 };
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
        self.sessions.lock().map(|sessions| sessions.values().cloned().collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_history_is_bounded_and_contains_only_diagnostics() {
        let state = LiveState::default();
        for turn in 0..25 { state.finish_timing(TurnTiming { session_id: "s".into(), turn, models: vec!["fake-asr".into()], stages: BTreeMap::from([("final_asr".into(), 12)]), tools: vec![], outcome: "done".into() }); }
        assert_eq!(state.timings().len(), 20);
        assert_eq!(state.timings().last().unwrap().turn, 5);
        let v = serde_json::to_value(&state.timings()[0]).unwrap();
        assert_eq!(v.as_object().unwrap().keys().cloned().collect::<Vec<_>>(), ["models", "outcome", "session_id", "stages", "tools", "turn"]);
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
        assert_eq!(state.aggregate_pulse(), (Some("thinking".into()), 0), "进入思考态时音量归零");
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
        assert_eq!(state.aggregate_pulse(), (Some("speaking".into()), 10), "speaking 优先级高于 listening，level 必须来自同一 s1 会话");
        state.set_state("s1", "idle");
        let s1 = state.sessions().into_iter().find(|s| s.session_id == "s1").unwrap();
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
        };
        state.finish_debug_trace(sample(1));
        assert!(state.debug_traces().is_empty(), "默认关闭时绝不留存任何对话文本");

        state.set_debug_enabled(true);
        for t in 1..=12 {
            state.finish_debug_trace(sample(t));
        }
        let traces = state.debug_traces();
        assert_eq!(traces.len(), 10);
        assert_eq!(traces[0].turn, 12);
        assert!(traces[0].raw_asr.contains("[REDACTED]") && !traces[0].raw_asr.contains("abc123"));
        assert!(traces[0].tools[0].result.contains("[REDACTED]") && !traces[0].tools[0].result.contains("sk-secret999"));

        state.set_debug_enabled(false);
        assert!(state.debug_traces().is_empty(), "关闭开关立即清空内存缓存");
    }
}
