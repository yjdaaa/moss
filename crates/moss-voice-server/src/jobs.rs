//! 加工结束后在空闲会话里主动播报（2026-11-02 用户决定）。
//!
//! 约定（用户决定）：只在**设备在线**（有会话）且**当前不在说话/思考**时播报；播报走固定话术
//! 轮次，因此和普通回答一样能被已有插话机制打断，被打断就按插话机制丢弃未播音频。
//!
//! 数据来源只读：每 `job_poll_interval` 向宿主工具读一次 `laser_workflow_tool(action=recent)`，
//! 只读存储、不碰设备、不发起任何动作。只在“上一次看见它还在发送、这一次已经是终态”时播报，
//! 所以刚连上就会看到的历史终态任务不会被念出来；第一次轮询只记基线。

use std::collections::HashMap;
use std::time::Duration;

use crate::conversation::{ToolContext, ToolExecutor};


/// 默认轮询间隔（4 秒：严格小于官方固件 `PREVIEW_IMAGE_DURATION_MS = 5000`，确保加工副屏 HUD 常驻不断帧）。
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(4);
/// 最多记住多少条任务状态；超过就整表重来（重新记基线，绝不会因此多播一次）。
const MAX_TRACKED: usize = 32;
/// 一个刚刚结束的加工任务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedJob {
    pub workflow_id: String,
    pub status: String,
    pub lines_sent: u64,
    pub total_lines: u64,
}

impl FinishedJob {
    /// 播报话术：只说记录里已有的行数，不编造物理加工进度或剩余时间。
    pub fn speech(&self) -> String {
        match self.status.as_str() {
            "completed" if self.total_lines > 0 => {
                format!("雕刻任务已完成，共 {} 行。", self.total_lines)
            }
            "completed" => "雕刻任务已完成。".into(),
            "cancelled" if self.lines_sent > 0 => {
                format!("任务已停止，停在第 {} 行。", self.lines_sent)
            }
            "cancelled" => "任务已停止。".into(),
            "failed" if self.lines_sent > 0 => {
                format!("任务失败了，停在第 {} 行，请检查设备。", self.lines_sent)
            }
            "failed" => "任务失败了，请检查设备。".into(),
            _ => "加工任务已结束。".into(),
        }
    }
}
/// 当前正在发送中的实时加工进度快照（用于驱动小智彩屏太空舱副屏 HUD / OLED 进度卡）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveJobProgress {
    pub workflow_id: String,
    pub percent: u8,
    pub lines_sent: u64,
    pub total_lines: u64,
    pub power: Option<i64>,
    pub feed_rate: Option<i64>,
    pub eta_seconds: Option<u64>,
}

impl ActiveJobProgress {
    pub fn to_hud_spec(&self) -> crate::preview_push::JobHudSpec {
        crate::preview_push::JobHudSpec {
            percent: self.percent,
            lines_sent: self.lines_sent,
            total_lines: self.total_lines,
            power: self.power,
            feed_rate: self.feed_rate,
            eta_seconds: self.eta_seconds,
        }
    }

    pub fn subtitle(&self) -> String {
        let mut parts = vec![format!("{}%", self.percent)];
        if self.total_lines > 0 {
            parts.push(format!("{}/{}行", self.lines_sent, self.total_lines));
        }
        if let Some(eta) = self.eta_seconds {
            if eta >= 60 {
                parts.push(format!("剩余{}分{}秒", eta / 60, eta % 60));
            } else {
                parts.push(format!("剩余{eta}秒"));
            }
        }
        let mut pf = Vec::new();
        if let Some(p) = self.power {
            pf.push(format!("S{p}"));
        }
        if let Some(f) = self.feed_rate {
            pf.push(format!("F{f}"));
        }
        if !pf.is_empty() {
            parts.push(pf.join(" "));
        }
        parts.join(" · ")
    }
}

/// 当前待用户在电脑桌面确认的只读审批提示（无敏字段，供小智屏幕显示「待电脑审批警戒卡」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApprovalNotice {
    pub kind_ascii: String,
    pub summary: String,
    pub remaining_secs: u64,
    pub total_secs: u64,
}

impl PendingApprovalNotice {
    pub fn to_card_spec(&self) -> crate::preview_push::ApprovalCardSpec {
        crate::preview_push::ApprovalCardSpec {
            kind_ascii: self.kind_ascii.clone(),
            remaining_secs: self.remaining_secs,
            total_secs: self.total_secs,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollFullSnapshot {
    pub active_job: Option<ActiveJobProgress>,
    pub pending_approval: Option<PendingApprovalNotice>,
    pub finished: Vec<FinishedJob>,
}

fn extract_pending_approval(body: &serde_json::Value) -> Option<PendingApprovalNotice> {
    let p = body.get("pending_approval")?.as_object()?;
    let rem = p.get("remaining_secs").and_then(|v| v.as_u64()).unwrap_or(0);
    if rem == 0 {
        return None;
    }
    let total = p.get("total_secs").and_then(|v| v.as_u64()).unwrap_or(120).max(rem);
    let kind_raw = p.get("kind").and_then(|v| v.as_str()).unwrap_or("laser");
    let kind_ascii = match kind_raw {
        "home_control" => "HOME CONTROL",
        "agent_run" => "LOCAL AGENT",
        "external_tool" => "MCP TOOL",
        "calibration" => "CALIBRATION",
        _ => "LASER ACTION",
    }
    .to_owned();
    let summary = p
        .get("summary")
        .and_then(|v| v.as_str())
        .map(|s| s.replace(['\r', '\n'], " ").trim().chars().take(72).collect::<String>())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "请在电脑 MOSS 点击批准后开始".to_owned());
    Some(PendingApprovalNotice {
        kind_ascii,
        summary,
        remaining_secs: rem,
        total_secs: total,
    })
}

fn extract_active_progress(item: &serde_json::Value, wf_id: &str) -> ActiveJobProgress {
    let live = &item["live_progress"];
    let send = &item["send_result"];
    let estimate = item
        .get("estimate")
        .or_else(|| item.pointer("/artifacts/estimate"))
        .unwrap_or(item);
    let snap = item.get("confirmation_snapshot").unwrap_or(item);
    let input = item.get("input").unwrap_or(item);

    let lines_sent = live["lines_sent"]
        .as_u64()
        .or_else(|| send["lines_sent"].as_u64())
        .unwrap_or(0);
    let total_lines = live["total_lines"]
        .as_u64()
        .or_else(|| send["total_lines"].as_u64())
        .or_else(|| estimate["line_count"].as_u64())
        .unwrap_or(0);
    let percent = if total_lines > 0 {
        ((lines_sent.saturating_mul(100)) / total_lines).min(100) as u8
    } else {
        0
    };
    let elapsed_ms = live["elapsed_ms"].as_u64().unwrap_or(0);
    let eta_seconds = if elapsed_ms >= 500 && lines_sent > 0 && total_lines > lines_sent {
        let rem_lines = total_lines - lines_sent;
        let rem_ms = rem_lines.saturating_mul(elapsed_ms) / lines_sent;
        Some((rem_ms / 1000).max(1))
    } else if total_lines > lines_sent
        && let Some(est) = estimate["estimated_seconds"]
            .as_f64()
            .or_else(|| item["estimated_seconds"].as_f64())
            .filter(|v| v.is_finite() && *v >= 1.0)
    {
        let ratio = (total_lines - lines_sent) as f64 / total_lines as f64;
        Some(((est * ratio).round() as u64).max(1))
    } else {
        None
    };
    let power = snap["power"]
        .as_i64()
        .or_else(|| input["power"].as_i64())
        .or_else(|| item["power"].as_i64());
    let feed_rate = snap["feed_rate"]
        .as_i64()
        .or_else(|| input["feed_rate"].as_i64())
        .or_else(|| item["feed_rate"].as_i64());

    ActiveJobProgress {
        workflow_id: wf_id.to_owned(),
        percent,
        lines_sent,
        total_lines,
        power,
        feed_rate,
        eta_seconds,
    }
}

/// 每个会话一份：记住上一次看到的任务状态，用来发现“发送中 → 终态”的转变，并提取当前正在发送的实时进度。
#[derive(Default)]
pub struct JobWatcher {
    seen: HashMap<String, String>,
    baseline: bool,
}

impl JobWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// 只读查询最近任务与待电脑审批状态：返回完整快照。
    pub async fn poll_full(
        &mut self,
        tools: &dyn ToolExecutor,
        context: &ToolContext,
    ) -> PollFullSnapshot {
        let Some(body) = tools.recent_workflows(context).await else {
            return PollFullSnapshot::default();
        };
        let pending_approval = extract_pending_approval(&body);
        let Some(items) = body["workflows"].as_array() else {
            return PollFullSnapshot {
                active_job: None,
                pending_approval,
                finished: Vec::new(),
            };
        };
        let mut active: Option<ActiveJobProgress> = None;
        let mut finished = Vec::new();
        for item in items {
            let Some(id) = item["workflow_id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            let status = item["status"].as_str().unwrap_or("");
            if status == "sending" && active.is_none() {
                active = Some(extract_active_progress(item, id));
            }
            let was_sending =
                self.baseline && self.seen.get(id).is_some_and(|old| old == "sending");
            if was_sending && status != "sending" {
                let send = &item["send_result"];
                finished.push(FinishedJob {
                    workflow_id: id.to_owned(),
                    status: status.to_owned(),
                    lines_sent: send["lines_sent"].as_u64().unwrap_or(0),
                    total_lines: send["total_lines"].as_u64().unwrap_or(0),
                });
            }
            self.seen.insert(id.to_owned(), status.to_owned());
        }
        if !self.baseline {
            self.baseline = true;
            return PollFullSnapshot {
                active_job: active,
                pending_approval,
                finished: Vec::new(),
            };
        }
        if self.seen.len() > MAX_TRACKED {
            self.seen.clear();
            self.baseline = false;
        }
        PollFullSnapshot {
            active_job: active,
            pending_approval,
            finished,
        }
    }

    /// 只读查询最近任务：返回 `(当前活跃发送中进度, 本拍刚结束的任务列表)`。
    pub async fn poll_snapshot(
        &mut self,
        tools: &dyn ToolExecutor,
        context: &ToolContext,
    ) -> (Option<ActiveJobProgress>, Vec<FinishedJob>) {
        let snap = self.poll_full(tools, context).await;
        (snap.active_job, snap.finished)
    }

    /// 读一次最近的任务，返回这一拍刚结束的任务（第一次调用只记基线，返回空）。
    /// 宿主没有提供只读通道、或后端出错时静默返回空，不把错误念给用户。
    pub async fn poll(
        &mut self,
        tools: &dyn ToolExecutor,
        context: &ToolContext,
    ) -> Vec<FinishedJob> {
        self.poll_full(tools, context).await.finished
    }
}
/// 同一条任务只让一个在线会话播报：多台设备同时在线时不至于一起说话。
#[derive(Default)]
pub struct AnnounceLog {
    recent: std::sync::Mutex<Vec<String>>,
}

impl AnnounceLog {
    /// 抢到这条任务的播报权返回 true；已经播过或锁不可用返回 false（宁可不播，也不重复播）。
    pub fn claim(&self, workflow_id: &str) -> bool {
        let Ok(mut recent) = self.recent.lock() else {
            return false;
        };
        if recent.iter().any(|id| id == workflow_id) {
            return false;
        }
        recent.push(workflow_id.to_owned());
        if recent.len() > MAX_TRACKED {
            recent.remove(0);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ToolOutcome};

    use moss_voice_providers::{BoxFuture, ToolSpec};
    use serde_json::{json, Value};
    use std::sync::Mutex;

    /// 假的宿主：`body` 是下一次 `recent` 返回的 result，可按脚本替换。
    struct FakeHost {
        body: Mutex<Value>,
        fail: Mutex<bool>,
        calls: Mutex<usize>,
    }

    impl FakeHost {
        fn new(items: Value) -> Self {
            Self {
                body: Mutex::new(json!({ "workflows": items })),
                fail: Mutex::new(false),
                calls: Mutex::new(0),
            }
        }

        fn set(&self, items: Value) {
            *self.body.lock().unwrap() = json!({ "workflows": items });
        }
    }

    impl ToolExecutor for FakeHost {
        fn tools(&self) -> Vec<ToolSpec> {
            Vec::new()
        }
        fn call<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async { panic!("任务观察器只走只读通道，不应调用普通工具") })
        }
        fn recent_workflows<'a>(
            &'a self,
            _: &'a ToolContext,
        ) -> BoxFuture<'a, Option<Value>> {
            Box::pin(async move {
                *self.calls.lock().unwrap() += 1;
                if *self.fail.lock().unwrap() {
                    return None;
                }
                Some(self.body.lock().unwrap().clone())
            })
        }
    }

    fn context() -> ToolContext {
        ToolContext { device_id: "d".into(), session_id: "s".into() }
    }

    fn job(id: &str, status: &str, sent: u64, total: u64) -> Value {
        json!({
            "workflow_id": id,
            "status": status,
            "updated_at": 1,
            "send_result": { "lines_sent": sent, "total_lines": total }
        })
    }

    #[tokio::test]
    async fn first_poll_is_baseline_and_never_announces_history() {
        let host = FakeHost::new(json!([job("wf-old", "completed", 100, 100)]));
        let mut watcher = JobWatcher::new();
        assert!(watcher.poll(&host, &context()).await.is_empty(), "基线不该播报");
        assert!(watcher.poll(&host, &context()).await.is_empty(), "早完成的任务不能补播");
        assert_eq!(*host.calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn sending_then_completed_announces_once_with_line_count() {
        let host = FakeHost::new(json!([job("wf-1", "sending", 12, 342)]));
        let mut watcher = JobWatcher::new();
        assert!(watcher.poll(&host, &context()).await.is_empty(), "基线");
        host.set(json!([job("wf-1", "sending", 300, 342)]));
        assert!(watcher.poll(&host, &context()).await.is_empty(), "还在发");
        host.set(json!([job("wf-1", "completed", 342, 342)]));
        let finished = watcher.poll(&host, &context()).await;
        assert_eq!(finished.len(), 1, "{finished:?}");
        assert_eq!(finished[0].workflow_id, "wf-1");
        assert_eq!(finished[0].speech(), "雕刻任务已完成，共 342 行。");
        assert!(watcher.poll(&host, &context()).await.is_empty(), "同一条只播一次");
    }

    #[tokio::test]
    async fn terminal_state_without_prior_sending_is_silent() {
        let host = FakeHost::new(json!([job("wf-1", "preview_ready", 0, 0)]));
        let mut watcher = JobWatcher::new();
        watcher.poll(&host, &context()).await;
        // 没看见 sending 就直接是终态：多半是这一拍里开始又结束，也可能是历史记录
        host.set(json!([job("wf-1", "completed", 10, 10)]));
        assert!(watcher.poll(&host, &context()).await.is_empty());
    }

    #[tokio::test]
    async fn stopped_and_failed_jobs_say_where_they_stopped() {
        let host = FakeHost::new(json!([job("wf-c", "sending", 1, 100), job("wf-f", "sending", 2, 100)]));
        let mut watcher = JobWatcher::new();
        watcher.poll(&host, &context()).await;
        host.set(json!([job("wf-c", "cancelled", 120, 342), job("wf-f", "failed", 88, 342)]));
        let finished = watcher.poll(&host, &context()).await;
        let speech: Vec<String> = finished.iter().map(FinishedJob::speech).collect();
        assert!(speech.iter().any(|text| text == "任务已停止，停在第 120 行。"), "{speech:?}");
        assert!(speech.iter().any(|text| text.contains("任务失败了，停在第 88 行")), "{speech:?}");
    }

    #[tokio::test]
    async fn host_failure_and_empty_records_stay_silent() {
        let host = FakeHost::new(json!([]));
        let mut watcher = JobWatcher::new();
        watcher.poll(&host, &context()).await;
        *host.fail.lock().unwrap() = true;
        assert!(watcher.poll(&host, &context()).await.is_empty());
        *host.fail.lock().unwrap() = false;
        host.set(json!([job("wf-1", "sending", 1, 10)]));
        assert!(watcher.poll(&host, &context()).await.is_empty());
    }

    #[test]
    fn announce_log_lets_only_one_session_speak_per_job() {
        let log = AnnounceLog::default();
        assert!(log.claim("wf-1"));
        assert!(!log.claim("wf-1"), "同一个任务不能两台设备各播一次");
        assert!(log.claim("wf-2"));
    }

    #[tokio::test]
    async fn active_sending_job_extracts_live_progress_eta_and_laser_params() {
        let host = FakeHost::new(json!([{
            "workflow_id": "wf-live",
            "status": "sending",
            "confirmation_snapshot": { "power": 600, "feed_rate": 1200 },
            "live_progress": { "lines_sent": 250, "total_lines": 1000, "elapsed_ms": 10_000 }
        }]));
        let mut watcher = JobWatcher::new();
        let (active, finished) = watcher.poll_snapshot(&host, &context()).await;
        assert!(finished.is_empty(), "首拍只记基线，不误报终态");
        let active = active.expect("sending 状态应立即提取实时进度");
        assert_eq!(active.workflow_id, "wf-live");
        assert_eq!(active.percent, 25);
        assert_eq!(active.lines_sent, 250);
        assert_eq!(active.total_lines, 1000);
        assert_eq!(active.power, Some(600));
        assert_eq!(active.feed_rate, Some(1200));
        assert_eq!(active.eta_seconds, Some(30));
        assert!(active.subtitle().contains("25%") && active.subtitle().contains("S600 F1200"));
    }
}
