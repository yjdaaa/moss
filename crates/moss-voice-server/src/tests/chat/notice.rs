//! 加工 / Agent 任务结束播报：空闲设备即时播报，并发通知排队不丢。
use super::*;

/// 加工结束播报（2026-11-02 用户决定）：设备在线且空闲时，把刚结束的任务念出来，不经过大模型。
#[test]
fn finished_job_is_announced_to_an_idle_online_device() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    /// 只读最近任务：第一拍“发送中”，之后“已完成”，制造真实的转变。
    struct JobTools {
        polls: AtomicUsize,
    }
    impl ToolExecutor for JobTools {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![]
        }
        fn call<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async { panic!("播报只走只读通道") })
        }
        fn recent_workflows<'a>(&'a self, _: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
            Box::pin(async move {
                let first = self.polls.fetch_add(1, Ordering::SeqCst) == 0;
                Some(serde_json::json!({"workflows":[{
                    "workflow_id":"wf-announce",
                    "status": if first { "sending" } else { "completed" },
                    "send_result":{"lines_sent":342,"total_lines":342}
                }]}))
            })
        }
    }

    let tts = Arc::new(FakeTts::default());
    let conversation = Arc::new(Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(FakeLlm::default()),
        tts.clone(),
        Arc::new(JobTools {
            polls: AtomicUsize::new(0),
        }),
        ConversationSettings::default(),
    ));
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(conversation),
            job_poll_interval: Duration::from_millis(30),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    // 全程不发语音、不发文字：设备在线且空闲就够了
    let (texts, frames, _) = collect_playback(&mut device, 1);
    assert_eq!(texts[0]["type"], "tts");
    assert_eq!(texts[0]["state"], "start");
    assert!(!frames.is_empty(), "播报必须有音频");
    assert!(
        !texts.iter().any(|value| value["type"] == "stt"),
        "播报不是识别结果"
    );
    let said = tts.said.lock().unwrap().clone();
    assert!(
        said.iter().any(|text| text.contains("雕刻任务已完成")),
        "{said:?}"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("播报结果")),
        "事件里要有播报记录"
    );
}

#[test]
fn concurrent_finished_job_and_agent_notices_are_queued_and_delivered_without_dropping() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct ConcurrentTools {
        polls: AtomicUsize,
    }
    impl ToolExecutor for ConcurrentTools {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![]
        }
        fn call<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async { panic!("播报只走只读通道") })
        }
        fn recent_workflows<'a>(&'a self, _: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
            Box::pin(async move {
                let first = self.polls.fetch_add(1, Ordering::SeqCst) == 0;
                if first {
                    Some(serde_json::json!({
                        "workflows": [{
                            "workflow_id": "wf-concurrent",
                            "status": "sending",
                            "send_result": {"lines_sent": 10, "total_lines": 100}
                        }],
                        "agent_runs": [{
                            "id": "agentrun-live-1",
                            "provider": "codex",
                            "status": "running",
                            "elapsed_secs": 5,
                            "silent_secs": 0,
                            "output_lines": 1
                        }]
                    }))
                } else {
                    Some(serde_json::json!({
                        "workflows": [{
                            "workflow_id": "wf-concurrent",
                            "status": "completed",
                            "send_result": {"lines_sent": 100, "total_lines": 100}
                        }],
                        "agent_runs": [
                            {
                                "id": "agentrun-live-1",
                                "provider": "codex",
                                "status": "completed",
                                "elapsed_secs": 12,
                                "silent_secs": 0,
                                "output_lines": 12
                            },
                            {
                                "id": "agentrun-fast-2",
                                "provider": "claude_code",
                                "status": "completed",
                                "elapsed_secs": 2,
                                "silent_secs": 0,
                                "output_lines": 4
                            }
                        ]
                    }))
                }
            })
        }
    }

    let tts = Arc::new(FakeTts::default());
    let conversation = Arc::new(Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(FakeLlm::default()),
        tts.clone(),
        Arc::new(ConcurrentTools {
            polls: AtomicUsize::new(0),
        }),
        ConversationSettings::default(),
    ));
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(conversation),
            job_poll_interval: Duration::from_millis(30),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    for _ in 0..3 {
        let _ = collect_playback(&mut device, 1);
    }
    let said = tts.said.lock().unwrap().clone();
    assert!(
        said.iter().any(|text| text.contains("雕刻任务已完成")),
        "missing workflow completion: {said:?}"
    );
    assert!(
        said.iter().any(|text| text.contains("Codex 任务已完成")),
        "missing live->completed agent notice: {said:?}"
    );
    assert!(
        said.iter()
            .any(|text| text.contains("Claude Code 任务已完成")),
        "missing short-lived post-baseline agent notice: {said:?}"
    );
}
