//! 内置语音计时提醒工具（2026-11-28，融合技术书 r77 / 内置语音技术书 n144）：
//! - 仅在内置语音链路提供，不暴露给官方小智云端接入点，不涉及激光机，不需要桌面审批卡；
//! - 支持 `set`（10 秒 ~ 3 小时，同时最多 5 个，仅存内存重启丢失）、`list`、`cancel`；
//! - 到点后仅向发起提醒的那台小智播报“提醒：<内容>”；设备正在对话时等本轮结束再播、不打断；
//!   设备不在线或已撤销时记一条来源为小智（`DiagSource::Xiaozhi`）的事件到「事件记录」，绝不改投别的设备。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use moss_voice_providers::{BoxFuture, ToolSpec};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::conversation::{ToolContext, ToolExecutor, ToolOutcome};

/// 内置语音计时提醒工具名。
pub const TIMER_TOOL: &str = "voice_timer_tool";
/// 工具说明（发给大模型；保持精简、面向用户意图）。
pub const TIMER_TOOL_DESCRIPTION: &str = "设置、查看或取消倒计时提醒（10秒到3小时，最多5个，仅存内存重启丢失）。用户说“几分钟后提醒我…”“查一下提醒”“取消提醒”时用。";
/// 最短倒计时（10 秒）。
pub const MIN_TIMER_SECONDS: u64 = 10;
/// 最长倒计时（3 小时）。
pub const MAX_TIMER_SECONDS: u64 = 3 * 3600;
/// 同时最多保持的计时提醒数。
pub const MAX_ACTIVE_TIMERS: usize = 5;
/// 提醒内容最长字符数。
pub const MAX_CONTENT_CHARS: usize = 120;

/// 可注入单调时钟：生产用系统单调时钟，单测用手动时钟，不真等。
pub trait TimerClock: Send + Sync {
    fn now(&self) -> Instant;
}

struct SystemTimerClock;

impl TimerClock for SystemTimerClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// 单测用可推进时钟（零真实等待）。
pub struct ManualTimerClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl ManualTimerClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    pub fn advance(&self, delta: Duration) {
        let mut guard = self
            .offset
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = guard.saturating_add(delta);
    }
}

impl Default for ManualTimerClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TimerClock for ManualTimerClock {
    fn now(&self) -> Instant {
        let offset = *self
            .offset
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.base + offset
    }
}

#[derive(Debug, Clone)]
struct TimerEntry {
    id: String,
    device_id: String,
    content: String,
    duration_seconds: u64,
    due_at: Instant,
}

/// 查询或操作返回的计时提醒快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerSnapshot {
    pub id: String,
    pub device_id: String,
    pub content: String,
    pub duration_seconds: u64,
    pub remaining_seconds: u64,
}

/// 到点待播报的计时提醒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiredTimer {
    pub id: String,
    pub device_id: String,
    pub content: String,
}

impl FiredTimer {
    /// 到点播报文案：“提醒：<内容>”。
    pub fn speech(&self) -> String {
        if self.content.is_empty() {
            "提醒：计时时间到了。".into()
        } else {
            format!("提醒：{}", self.content)
        }
    }
}

#[derive(Default)]
struct StoreInner {
    next_id: u64,
    entries: Vec<TimerEntry>,
}

/// 进程内存中的计时提醒存储（重启后自然清空）。
pub struct TimerStore {
    inner: Mutex<StoreInner>,
    clock: Arc<dyn TimerClock>,
    changed: Notify,
}

impl TimerStore {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemTimerClock))
    }

    pub fn with_clock(clock: Arc<dyn TimerClock>) -> Self {
        Self {
            inner: Mutex::new(StoreInner::default()),
            clock,
            changed: Notify::new(),
        }
    }

    pub fn now(&self) -> Instant {
        self.clock.now()
    }

    pub fn active_count(&self) -> usize {
        self.inner.lock().map(|g| g.entries.len()).unwrap_or(0)
    }

    /// 新增一条计时提醒（校验 10 秒 ~ 3 小时、总上限 5 个）。
    pub fn set(
        &self,
        device_id: &str,
        duration_seconds: u64,
        content: Option<&str>,
    ) -> Result<TimerSnapshot, String> {
        if !(MIN_TIMER_SECONDS..=MAX_TIMER_SECONDS).contains(&duration_seconds) {
            return Err(format!(
                "计时时长须在 {MIN_TIMER_SECONDS} 秒到 3 小时（{MAX_TIMER_SECONDS} 秒）之间"
            ));
        }
        let cleaned_content: String = content
            .unwrap_or("")
            .trim()
            .chars()
            .filter(|ch| !ch.is_control())
            .take(MAX_CONTENT_CHARS)
            .collect();
        let now = self.clock.now();
        let snapshot = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if inner.entries.len() >= MAX_ACTIVE_TIMERS {
                return Err(format!(
                    "同时最多只能设置 {MAX_ACTIVE_TIMERS} 个计时提醒，请先取消已有的提醒"
                ));
            }
            inner.next_id += 1;
            let id = format!("t-{}", inner.next_id);
            let due_at = now + Duration::from_secs(duration_seconds);
            inner.entries.push(TimerEntry {
                id: id.clone(),
                device_id: device_id.to_owned(),
                content: cleaned_content.clone(),
                duration_seconds,
                due_at,
            });
            TimerSnapshot {
                id,
                device_id: device_id.to_owned(),
                content: cleaned_content,
                duration_seconds,
                remaining_seconds: duration_seconds,
            }
        };
        self.changed.notify_one();
        Ok(snapshot)
    }

    /// 列出指定设备当前进行中的计时提醒（按到期先后排序）。
    pub fn list(&self, device_id: &str) -> Vec<TimerSnapshot> {
        let now = self.clock.now();
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut items: Vec<(Instant, TimerSnapshot)> = inner
            .entries
            .iter()
            .filter(|entry| entry.device_id == device_id)
            .map(|entry| {
                let remaining = entry.due_at.saturating_duration_since(now).as_secs();
                (
                    entry.due_at,
                    TimerSnapshot {
                        id: entry.id.clone(),
                        device_id: entry.device_id.clone(),
                        content: entry.content.clone(),
                        duration_seconds: entry.duration_seconds,
                        remaining_seconds: remaining,
                    },
                )
            })
            .collect();
        items.sort_by_key(|(due, _)| *due);
        items.into_iter().map(|(_, snap)| snap).collect()
    }

    /// 取消指定设备的计时提醒（仅允许取消发起设备自己的提醒；不给 `timer_id` 且只剩 1 个时自动取消该条）。
    pub fn cancel(&self, device_id: &str, timer_id: Option<&str>) -> Result<TimerSnapshot, String> {
        let now = self.clock.now();
        let removed = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let requested = timer_id.map(str::trim).filter(|s| !s.is_empty());
            let pos = if let Some(target_id) = requested {
                inner
                    .entries
                    .iter()
                    .position(|entry| entry.device_id == device_id && entry.id == target_id)
                    .ok_or_else(|| format!("没有找到编号为 {target_id} 的计时提醒"))?
            } else {
                let mine: Vec<usize> = inner
                    .entries
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, entry)| (entry.device_id == device_id).then_some(idx))
                    .collect();
                match mine.as_slice() {
                    [] => return Err("当前没有进行中的计时提醒".into()),
                    [only] => *only,
                    many => {
                        let ids: Vec<&str> = many
                            .iter()
                            .map(|&idx| inner.entries[idx].id.as_str())
                            .collect();
                        return Err(format!(
                            "当前有 {} 个计时提醒（{}），请指定要取消的 timer_id",
                            ids.len(),
                            ids.join("、")
                        ));
                    }
                }
            };
            let entry = inner.entries.remove(pos);
            TimerSnapshot {
                id: entry.id,
                device_id: entry.device_id,
                content: entry.content,
                duration_seconds: entry.duration_seconds,
                remaining_seconds: entry.due_at.saturating_duration_since(now).as_secs(),
            }
        };
        self.changed.notify_one();
        Ok(removed)
    }

    /// 取出全部已到点的计时提醒（按到期先后顺序）。
    pub fn poll_due(&self) -> Vec<FiredTimer> {
        let now = self.clock.now();
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut due = Vec::new();
        let mut keep = Vec::with_capacity(inner.entries.len());
        for entry in inner.entries.drain(..) {
            if entry.due_at <= now {
                due.push(entry);
            } else {
                keep.push(entry);
            }
        }
        inner.entries = keep;
        due.sort_by_key(|entry| entry.due_at);
        due.into_iter()
            .map(|entry| FiredTimer {
                id: entry.id,
                device_id: entry.device_id,
                content: entry.content,
            })
            .collect()
    }

    /// 返回下一条最快到点的时刻；没有活动计时器时返回 `None`。
    pub fn next_due(&self) -> Option<Instant> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.entries.iter().map(|entry| entry.due_at).min()
    }

    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }
}

impl Default for TimerStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 将倒计时秒数格式化为适合语音播报的中文时长。
pub fn format_timer_duration_zh(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds} 秒")
    } else if seconds < 3600 {
        let mins = seconds / 60;
        let secs = seconds % 60;
        if secs == 0 {
            format!("{mins} 分钟")
        } else {
            format!("{mins} 分 {secs} 秒")
        }
    } else {
        let hours = seconds / 3600;
        let mins = (seconds % 3600) / 60;
        let secs = seconds % 60;
        match (mins, secs) {
            (0, 0) => format!("{hours} 小时"),
            (_, 0) => format!("{hours} 小时 {mins} 分钟"),
            (0, s) => format!("{hours} 小时 {s} 秒"),
            (m, s) => format!("{hours} 小时 {m} 分 {s} 秒"),
        }
    }
}

/// 交给大模型的计时提醒工具规格。
pub fn timer_tool_spec() -> ToolSpec {
    ToolSpec {
        name: TIMER_TOOL.into(),
        description: TIMER_TOOL_DESCRIPTION.into(),
        parameters: json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["set", "list", "cancel"],
                    "description": "set 新增计时提醒；list 查看当前提醒；cancel 取消提醒"
                },
                "duration_seconds": {
                    "type": "integer",
                    "minimum": MIN_TIMER_SECONDS,
                    "maximum": MAX_TIMER_SECONDS,
                    "description": "set：倒计时秒数，10~10800（10秒到3小时），如 5 分钟填 300"
                },
                "content": {
                    "type": "string",
                    "description": "set：到点播报的提醒内容（可选，如“关机”“检查木板”）"
                },
                "timer_id": {
                    "type": "string",
                    "description": "cancel：要取消的提醒编号（list 返回的 id；只剩 1 个时可省略）"
                }
            },
            "additionalProperties": false
        }),
    }
}

fn parse_duration_seconds(args: &Value) -> Result<u64, String> {
    let raw = args
        .get("duration_seconds")
        .ok_or_else(|| "set 需要提供 duration_seconds（10 ~ 10800 秒）".to_owned())?;
    if let Some(n) = raw.as_u64() {
        return Ok(n);
    }
    if let Some(f) = raw.as_f64()
        && f.is_finite()
        && f >= 0.0
        && f.fract() == 0.0
        && f <= u64::MAX as f64
    {
        return Ok(f as u64);
    }
    if let Some(s) = raw.as_str()
        && let Ok(n) = s.trim().parse::<u64>()
    {
        return Ok(n);
    }
    Err(format!(
        "duration_seconds 必须是 {MIN_TIMER_SECONDS} 到 {MAX_TIMER_SECONDS} 之间的整数秒"
    ))
}

/// 执行一次 `voice_timer_tool` 调用。
pub fn execute_timer_call(store: &TimerStore, context: &ToolContext, arguments: &str) -> String {
    let args: Value = if arguments.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(arguments) {
            Ok(val) => val,
            Err(_) => {
                return json!({
                    "success": false,
                    "error_code": "invalid_arguments",
                    "result": "计时提醒参数不是合法 JSON",
                })
                .to_string();
            }
        }
    };
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match action.as_str() {
        "set" => {
            let duration = match parse_duration_seconds(&args) {
                Ok(secs) => secs,
                Err(error) => {
                    return json!({
                        "success": false,
                        "error_code": "invalid_duration",
                        "result": error,
                    })
                    .to_string();
                }
            };
            let content = args.get("content").and_then(Value::as_str);
            match store.set(&context.device_id, duration, content) {
                Ok(snap) => {
                    let dur_text = format_timer_duration_zh(snap.duration_seconds);
                    let content_note = if snap.content.is_empty() {
                        String::new()
                    } else {
                        format!("「{}」", snap.content)
                    };
                    let result = format!(
                        "已设置 {dur_text} 后提醒{content_note}（编号 {}）。计时提醒仅保存在内存中，重启 MOSS 后会丢失。",
                        snap.id
                    );
                    json!({
                        "success": true,
                        "timer_id": snap.id,
                        "duration_seconds": snap.duration_seconds,
                        "content": snap.content,
                        "active_count": store.active_count(),
                        "memory_only": true,
                        "result": result,
                    })
                    .to_string()
                }
                Err(error) => json!({
                    "success": false,
                    "error_code": "timer_set_rejected",
                    "result": error,
                })
                .to_string(),
            }
        }
        "list" => {
            let timers = store.list(&context.device_id);
            let items: Vec<Value> = timers
                .iter()
                .map(|t| {
                    json!({
                        "timer_id": t.id,
                        "content": t.content,
                        "duration_seconds": t.duration_seconds,
                        "remaining_seconds": t.remaining_seconds,
                    })
                })
                .collect();
            let result = if timers.is_empty() {
                "当前没有进行中的计时提醒。".to_owned()
            } else {
                let parts: Vec<String> = timers
                    .iter()
                    .map(|t| {
                        let rem = format_timer_duration_zh(t.remaining_seconds);
                        if t.content.is_empty() {
                            format!("{}（还剩 {}）", t.id, rem)
                        } else {
                            format!("{}（还剩 {}，提醒「{}」）", t.id, rem, t.content)
                        }
                    })
                    .collect();
                format!("当前有 {} 个计时提醒：{}。", timers.len(), parts.join("；"))
            };
            json!({
                "success": true,
                "timers": items,
                "active_count": timers.len(),
                "result": result,
            })
            .to_string()
        }
        "cancel" => {
            let timer_id = args.get("timer_id").and_then(Value::as_str);
            match store.cancel(&context.device_id, timer_id) {
                Ok(snap) => {
                    let content_note = if snap.content.is_empty() {
                        String::new()
                    } else {
                        format!("「{}」", snap.content)
                    };
                    json!({
                        "success": true,
                        "cancelled_timer_id": snap.id,
                        "content": snap.content,
                        "result": format!("已取消计时提醒 {}（{}）", snap.id, if content_note.is_empty() { "无备注" } else { content_note.as_str() }),
                    })
                    .to_string()
                }
                Err(error) => json!({
                    "success": false,
                    "error_code": "timer_cancel_failed",
                    "result": error,
                })
                .to_string(),
            }
        }
        other => json!({
            "success": false,
            "error_code": "invalid_action",
            "result": format!("不支持的计时提醒动作 {other}，只支持 set / list / cancel"),
        })
        .to_string(),
    }
}

/// 在原有工具之外挂载 `voice_timer_tool`；支持工具开关关闭与设备会话授权复核。
pub struct TimerTools {
    pub original: Arc<dyn ToolExecutor>,
    pub timers: Arc<TimerStore>,
    /// 设备授权检查（同 `PersonaTools`，撤销中的设备拒绝设置或取消计时器）。
    pub permitted: Option<Arc<dyn Fn(&ToolContext) -> bool + Send + Sync>>,
    /// 工具开关检查（对应 `mcp_disabled_tools` 中的 `voice_timer_tool`）。
    pub disabled: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl TimerTools {
    fn is_disabled(&self) -> bool {
        self.disabled.as_ref().is_some_and(|check| check())
    }
}

impl ToolExecutor for TimerTools {
    fn tools(&self) -> Vec<ToolSpec> {
        let mut list: Vec<ToolSpec> = self
            .original
            .tools()
            .into_iter()
            .filter(|tool| tool.name != TIMER_TOOL)
            .collect();
        if !self.is_disabled() {
            list.push(timer_tool_spec());
        }
        list
    }

    fn trusted_read_only(&self, name: &str) -> bool {
        if name == TIMER_TOOL {
            false
        } else {
            self.original.trusted_read_only(name)
        }
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        if name != TIMER_TOOL {
            return self.original.call(context, name, arguments);
        }
        Box::pin(async move {
            if self.is_disabled() {
                return ToolOutcome::Reply(
                    json!({
                        "success": false,
                        "error_code": "tool_disabled",
                        "result": format!("{TIMER_TOOL} 已在 MOSS 中关闭，需要时请在 MOSS 语音中心的工具开关里打开"),
                    })
                    .to_string(),
                );
            }
            if let Some(check) = &self.permitted
                && !check(context)
            {
                return ToolOutcome::Reply(
                    json!({
                        "success": false,
                        "error_code": "device_revoked",
                        "result": "这台设备尚未允许或已被撤销，无法使用计时提醒",
                    })
                    .to_string(),
                );
            }
            ToolOutcome::Reply(execute_timer_call(&self.timers, context, arguments))
        })
    }

    fn recent_workflows<'a>(&'a self, context: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
        self.original.recent_workflows(context)
    }

    fn has_active_laser_job<'a>(&'a self, context: &'a ToolContext) -> BoxFuture<'a, Option<bool>> {
        self.original.has_active_laser_job(context)
    }

    fn workflow_preview<'a>(
        &'a self,
        context: &'a ToolContext,
        workflow_id: &'a str,
    ) -> BoxFuture<'a, Option<Vec<u8>>> {
        self.original.workflow_preview(context, workflow_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{
        Conversation, ConversationSettings, NoTools, ToolLatch, TurnEvent, TurnGate, TurnInput,
        spawn_turn_gated,
    };
    use crate::state::{DiagSource, DiagnosticsLog, LiveState};
    use crate::{DeviceRequest, DeviceRequestHandle, dispatch_fired_timer_to};
    use moss_voice_providers::{
        Asr, ChatMessage, Llm, LlmReply, Pcm, ProviderError, ToolCall, Tts,
    };
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::mpsc::unbounded_channel;

    fn ctx(dev: &str) -> ToolContext {
        ToolContext {
            device_id: dev.into(),
            session_id: format!("sess-{dev}"),
        }
    }

    #[test]
    fn timer_store_enforces_duration_bounds_max_five_and_cancel() {
        let clock = Arc::new(ManualTimerClock::new());
        let store = TimerStore::with_clock(clock.clone());

        assert!(store.set("dev-a", 9, Some("太短")).is_err());
        assert!(store.set("dev-a", 10_801, Some("太长")).is_err());

        // 边界 10 秒与 3 小时（10800 秒）都允许，最多同时 5 个
        let t1 = store.set("dev-a", 10, Some("关机")).unwrap();
        let _t2 = store.set("dev-a", 60, Some("翻面")).unwrap();
        let _t3 = store.set("dev-a", 120, None).unwrap();
        let _t4 = store.set("dev-b", 300, Some("B 的提醒")).unwrap();
        let _t5 = store.set("dev-a", 10_800, Some("三小时")).unwrap();
        assert_eq!(store.active_count(), 5);
        assert!(
            store.set("dev-a", 30, Some("第 6 个")).is_err(),
            "超过 5 个上限必须拒绝"
        );

        // 列表按设备隔离且按剩余时间排序
        let a_list = store.list("dev-a");
        assert_eq!(a_list.len(), 4);
        assert_eq!(a_list[0].id, t1.id);
        let b_list = store.list("dev-b");
        assert_eq!(b_list.len(), 1);
        assert_eq!(b_list[0].content, "B 的提醒");

        // 不能跨设备取消别人的提醒
        assert!(store.cancel("dev-a", Some(&b_list[0].id)).is_err());
        // 设备 B 只有 1 个提醒时省略 timer_id 可直接取消
        let cancelled_b = store.cancel("dev-b", None).unwrap();
        assert_eq!(cancelled_b.id, b_list[0].id);
        assert_eq!(store.active_count(), 4);

        // 设备 A 有多个提醒时省略 timer_id 须报错要求指定
        assert!(store.cancel("dev-a", None).is_err());
        let cancelled_a1 = store.cancel("dev-a", Some(&t1.id)).unwrap();
        assert_eq!(cancelled_a1.id, t1.id);
        assert_eq!(store.active_count(), 3);
    }

    #[test]
    fn timer_store_fires_at_deadline_using_manual_clock() {
        let clock = Arc::new(ManualTimerClock::new());
        let store = TimerStore::with_clock(clock.clone());
        let t1 = store.set("dev-a", 30, Some("检查木板")).unwrap();
        let t2 = store.set("dev-a", 90, None).unwrap();

        clock.advance(Duration::from_secs(29));
        assert!(store.poll_due().is_empty());
        assert_eq!(store.list("dev-a")[0].remaining_seconds, 1);

        clock.advance(Duration::from_secs(1));
        let due1 = store.poll_due();
        assert_eq!(due1.len(), 1);
        assert_eq!(due1[0].id, t1.id);
        assert_eq!(due1[0].speech(), "提醒：检查木板");

        clock.advance(Duration::from_secs(60));
        let due2 = store.poll_due();
        assert_eq!(due2.len(), 1);
        assert_eq!(due2[0].id, t2.id);
        assert_eq!(due2[0].speech(), "提醒：计时时间到了。");
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn timer_tools_mentions_restart_loss_and_honors_disable_switch_and_permission() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let clock = Arc::new(ManualTimerClock::new());
        let store = Arc::new(TimerStore::with_clock(clock));
        let disabled = Arc::new(AtomicBool::new(false));
        let allowed = Arc::new(AtomicBool::new(true));
        let tools = TimerTools {
            original: Arc::new(NoTools),
            timers: store.clone(),
            permitted: Some(Arc::new({
                let allowed = allowed.clone();
                move |_| allowed.load(Ordering::Acquire)
            })),
            disabled: Some(Arc::new({
                let disabled = disabled.clone();
                move || disabled.load(Ordering::Acquire)
            })),
        };

        assert!(tools.tools().iter().any(|t| t.name == TIMER_TOOL));
        let reply = rt.block_on(tools.call(
            &ctx("dev-a"),
            TIMER_TOOL,
            r#"{"action":"set","duration_seconds":300,"content":"关机"}"#,
        ));
        let ToolOutcome::Reply(body) = reply else {
            panic!("expected Reply");
        };
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["success"], true);
        let text = parsed["result"].as_str().unwrap();
        assert!(text.contains("内存") && text.contains("重启"), "{text}");

        // 撤销设备权限时拒绝
        allowed.store(false, Ordering::Release);
        let denied = rt.block_on(tools.call(&ctx("dev-a"), TIMER_TOOL, r#"{"action":"list"}"#));
        let ToolOutcome::Reply(denied_body) = denied else {
            panic!("expected Reply");
        };
        assert!(denied_body.contains("device_revoked"), "{denied_body}");
        allowed.store(true, Ordering::Release);

        // 工具开关关闭后隐藏并拒绝调用
        disabled.store(true, Ordering::Release);
        assert!(!tools.tools().iter().any(|t| t.name == TIMER_TOOL));
        let off = rt.block_on(tools.call(&ctx("dev-a"), TIMER_TOOL, r#"{"action":"list"}"#));
        let ToolOutcome::Reply(off_body) = off else {
            panic!("expected Reply");
        };
        assert!(off_body.contains("tool_disabled"), "{off_body}");
    }

    #[test]
    fn dispatch_fired_timer_targets_originating_device_only_and_logs_xiaozhi_event_when_offline() {
        let state = LiveState::with_log(DiagnosticsLog::new());
        let (tx_b, mut rx_b) = unbounded_channel();
        let mut map = HashMap::new();
        map.insert(
            "dev-b".to_owned(),
            DeviceRequestHandle {
                session_id: "sess-b".into(),
                supports_mcp: false,
                permission: Arc::new(AtomicBool::new(true)),
                sender: tx_b,
                turn: 0,
            },
        );
        let requests = Arc::new(Mutex::new(map));

        // dev-a 不在线（dev-b 在线）：绝不改投 dev-b，记来源小智事件
        dispatch_fired_timer_to(
            &requests,
            &state,
            FiredTimer {
                id: "t-1".into(),
                device_id: "dev-a".into(),
                content: "关掉气泵".into(),
            },
        );
        assert!(rx_b.try_recv().is_err(), "绝不能改投其他在线设备");
        let events = state.events();
        assert!(
            events.iter().any(|e| {
                e.source == DiagSource::Xiaozhi
                    && e.text.contains("dev-a")
                    && e.text.contains("提醒：关掉气泵")
            }),
            "离线设备到点须写入来源为小智的事件记录: {events:?}"
        );

        // dev-b 在线时仅发往 dev-b
        dispatch_fired_timer_to(
            &requests,
            &state,
            FiredTimer {
                id: "t-2".into(),
                device_id: "dev-b".into(),
                content: "收木牌".into(),
            },
        );
        match rx_b.try_recv().expect("dev-b 应收到提醒") {
            DeviceRequest::TimerReminder { id, speech } => {
                assert_eq!(id, "t-2");
                assert_eq!(speech, "提醒：收木牌");
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn timer_set_and_cancel_execute_only_after_turn_gate_releases() {
        struct DummyAsr;
        impl Asr for DummyAsr {
            fn transcribe<'a>(
                &'a self,
                _: &'a [i16],
                _: u32,
            ) -> BoxFuture<'a, Result<String, ProviderError>> {
                Box::pin(async { Ok("五分钟后提醒我关机".into()) })
            }
        }
        struct DummyTts;
        impl Tts for DummyTts {
            fn synthesize<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
                Box::pin(async {
                    Ok(Pcm {
                        samples: vec![0; 480],
                        sample_rate: 24_000,
                    })
                })
            }
        }
        struct ScriptedLlm(Mutex<VecDeque<LlmReply>>);
        impl Llm for ScriptedLlm {
            fn chat<'a>(
                &'a self,
                _: &'a [ChatMessage],
                _: &'a [ToolSpec],
                deltas: tokio::sync::mpsc::UnboundedSender<String>,
            ) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
                Box::pin(async move {
                    let reply = self
                        .0
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| LlmReply {
                            text: "好的。".into(),
                            tool_calls: Vec::new(),
                        });
                    if !reply.text.is_empty() {
                        let _ = deltas.send(reply.text.clone());
                    }
                    Ok(reply)
                })
            }
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let clock = Arc::new(ManualTimerClock::new());
            let store = Arc::new(TimerStore::with_clock(clock));
            let tools: Arc<dyn ToolExecutor> = Arc::new(TimerTools {
                original: Arc::new(NoTools),
                timers: store.clone(),
                permitted: None,
                disabled: None,
            });

            // 1. 验证 set 排在 TurnGate 之后
            let llm_set = Arc::new(ScriptedLlm(Mutex::new(VecDeque::from([
                LlmReply {
                    text: String::new(),
                    tool_calls: vec![ToolCall::new(
                        "c1",
                        TIMER_TOOL,
                        r#"{"action":"set","duration_seconds":300,"content":"关机"}"#,
                    )],
                },
                LlmReply {
                    text: "已设好提醒。".into(),
                    tool_calls: Vec::new(),
                },
            ]))));
            let conv = Arc::new(Conversation::new(
                Arc::new(DummyAsr),
                llm_set,
                Arc::new(DummyTts),
                tools.clone(),
                ConversationSettings::default(),
            ));
            let (rel_tx, rel_rx) = tokio::sync::watch::channel(false);
            let gate = TurnGate {
                release: rel_rx,
                tool_latch: Arc::new(ToolLatch::default()),
                speculative: true,
                hold_llm: false,
                fresh_history: Arc::new(Mutex::new(None)),
                allow_silent: false,
                allow_background: false,
                command_hint: Arc::new(Mutex::new(None)),
                command_hint_until: None,
            };
            let (_handle, mut events) = spawn_turn_gated(
                conv,
                ctx("dev-gate"),
                TurnInput::Utterance {
                    pcm: vec![1000; 1600],
                    sample_rate: 16_000,
                    stream: None,
                },
                Vec::new(),
                None,
                None,
                Some(gate),
            );
            // 推进事件循环，闸门未放行时 set 不得执行
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert_eq!(store.active_count(), 0, "TurnGate 放行前 set 不得执行");
            let _ = rel_tx.send(true);
            while let Some(ev) = events.recv().await {
                if matches!(ev, TurnEvent::Done { .. }) {
                    break;
                }
            }
            assert_eq!(store.active_count(), 1, "TurnGate 放行后 set 应执行");

            // 2. 验证 cancel 同样排在 TurnGate 之后
            let llm_cancel = Arc::new(ScriptedLlm(Mutex::new(VecDeque::from([
                LlmReply {
                    text: String::new(),
                    tool_calls: vec![ToolCall::new("c2", TIMER_TOOL, r#"{"action":"cancel"}"#)],
                },
                LlmReply {
                    text: "已取消提醒。".into(),
                    tool_calls: Vec::new(),
                },
            ]))));
            let conv2 = Arc::new(Conversation::new(
                Arc::new(DummyAsr),
                llm_cancel,
                Arc::new(DummyTts),
                tools,
                ConversationSettings::default(),
            ));
            let (rel_tx2, rel_rx2) = tokio::sync::watch::channel(false);
            let gate2 = TurnGate {
                release: rel_rx2,
                tool_latch: Arc::new(ToolLatch::default()),
                speculative: true,
                hold_llm: false,
                fresh_history: Arc::new(Mutex::new(None)),
                allow_silent: false,
                allow_background: false,
                command_hint: Arc::new(Mutex::new(None)),
                command_hint_until: None,
            };
            let (_handle2, mut events2) = spawn_turn_gated(
                conv2,
                ctx("dev-gate"),
                TurnInput::Utterance {
                    pcm: vec![1000; 1600],
                    sample_rate: 16_000,
                    stream: None,
                },
                Vec::new(),
                None,
                None,
                Some(gate2),
            );
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert_eq!(store.active_count(), 1, "TurnGate 放行前 cancel 不得执行");
            let _ = rel_tx2.send(true);
            while let Some(ev) = events2.recv().await {
                if matches!(ev, TurnEvent::Done { .. }) {
                    break;
                }
            }
            assert_eq!(store.active_count(), 0, "TurnGate 放行后 cancel 应执行");
        });
    }
}
