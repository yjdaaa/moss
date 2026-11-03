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


/// 默认轮询间隔（`ServerConfig::job_poll_interval` 的默认值）。
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

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

/// 每个会话一份：记住上一次看到的任务状态，用来发现“发送中 → 终态”的转变。
#[derive(Default)]
pub struct JobWatcher {
    seen: HashMap<String, String>,
    baseline: bool,
}

impl JobWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读一次最近的任务，返回这一拍刚结束的任务（第一次调用只记基线，返回空）。
    /// 宿主没有提供只读通道、或后端出错时静默返回空，不把错误念给用户。
    pub async fn poll(
        &mut self,
        tools: &dyn ToolExecutor,
        context: &ToolContext,
    ) -> Vec<FinishedJob> {
        let Some(body) = tools.recent_workflows(context).await else {
            return Vec::new();
        };
        let Some(items) = body["workflows"].as_array() else {
            return Vec::new();
        };
        let mut finished = Vec::new();
        for item in items {
            let Some(id) = item["workflow_id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            let status = item["status"].as_str().unwrap_or("");
            let was_sending = self.baseline && self.seen.get(id).is_some_and(|old| old == "sending");
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
            return Vec::new();
        }
        if self.seen.len() > MAX_TRACKED {
            self.seen.clear();
            self.baseline = false;
        }
        finished
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
}
