//! 运行中的会话与最近事件（供语音中心显示）。事件不含 token 与音频内容。

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::Serialize;

const MAX_EVENTS: usize = 100;
const MAX_TIMINGS: usize = 20;

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
    /// `idle` / `recording` / `playing`
    pub state: String,
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
            }
        }
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
}
