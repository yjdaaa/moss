//! 判停抢跑：口语状态直达、半句等待续说、慢判停与并发任务、Smart Turn 闸门。
use super::*;

#[tokio::test]
async fn jev_filter_routes_colloquial_status_without_llm_and_prunes_unrelated_tools_for_chat() {
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    use crate::jev::{DecisionClient, DecisionError, JevFilterOptions, VoiceJevFilter};

    struct ScriptedJev(Mutex<VecDeque<Result<Value, DecisionError>>>);
    impl DecisionClient for ScriptedJev {
        fn decide<'a>(
            &'a self,
            _payload: &'a Value,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Value, DecisionError>> {
            let next = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(DecisionError::Failed));
            Box::pin(async move { next })
        }
    }

    struct MultiDomainTools {
        inner: FastTools,
    }
    impl ToolExecutor for MultiDomainTools {
        fn tools(&self) -> Vec<ToolSpec> {
            [
                "laser_workflow_tool",
                "get_weather",
                "hass_set_state",
                "play_music",
            ]
            .into_iter()
            .map(|name| ToolSpec {
                name: name.into(),
                description: name.into(),
                parameters: serde_json::json!({"type": "object"}),
            })
            .collect()
        }
        fn call<'a>(
            &'a self,
            c: &'a ToolContext,
            n: &'a str,
            a: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            self.inner.call(c, n, a)
        }
        fn call_raw<'a>(
            &'a self,
            c: &'a ToolContext,
            n: &'a str,
            a: &'a str,
        ) -> BoxFuture<'a, crate::ToolHostResult> {
            self.inner.call_raw(c, n, a)
        }
    }

    let jev_client = Arc::new(ScriptedJev(Mutex::new(VecDeque::from([
        // 第 1 轮：花式口语问进度 -> Jev quick_read 命中 progress，直接走本地快通道跳过大模型
        Ok(serde_json::json!({
            "answers": {
                "quick_read": {
                    "type": "choice",
                    "choice": "progress",
                    "confidence": 0.93,
                    "probabilities": {"progress": 0.94, "none": 0.06}
                },
                "tool_laser_query": {"type": "noul", "noul": 0.90},
                "tool_laser_generate": {"type": "noul", "noul": 0.02},
                "tool_laser_control": {"type": "noul", "noul": 0.02},
                "tool_info_plugin": {"type": "noul", "noul": 0.01},
                "tool_home_agent": {"type": "noul", "noul": 0.01},
                "tool_music_device": {"type": "noul", "noul": 0.01}
            }
        })),
        // 第 2 轮：纯闲聊 -> Jev 将全部工具组剪为 0 个工具发给大模型
        Ok(serde_json::json!({
            "answers": {
                "quick_read": {
                    "type": "choice",
                    "choice": "none",
                    "confidence": 0.96,
                    "probabilities": {"none": 0.97, "status": 0.03}
                },
                "tool_laser_query": {"type": "noul", "noul": 0.02},
                "tool_laser_generate": {"type": "noul", "noul": 0.01},
                "tool_laser_control": {"type": "noul", "noul": 0.01},
                "tool_info_plugin": {"type": "noul", "noul": 0.03},
                "tool_home_agent": {"type": "noul", "noul": 0.01},
                "tool_music_device": {"type": "noul", "noul": 0.01}
            }
        })),
    ]))));
    let filter = Arc::new(VoiceJevFilter::new(
        jev_client,
        JevFilterOptions {
            enabled: true,
            ..Default::default()
        },
    ));
    let llm = Arc::new(FakeLlm::default());
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("师傅那块木板现在干到几成啦".into()),
            Ok("给我讲个冷笑话乐呵一下".into()),
        ])),
        ..Default::default()
    });
    let tools = Arc::new(MultiDomainTools {
        inner: FastTools::default(),
    });
    let conversation = Arc::new(
        Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            tools,
            ConversationSettings::default(),
        )
        .with_jev_filter(filter),
    );
    let ctx = ToolContext {
        device_id: "d-jev".into(),
        session_id: "s-jev".into(),
    };

    // 第 1 轮断言：走 fast_route，LLM 调用次数为 0，播报真实 40% 进度
    let (h1, mut rx1) = spawn_turn(
        conversation.clone(),
        ctx.clone(),
        TurnInput::Utterance {
            pcm: vec![0; 960],
            sample_rate: UP,
            stream: None,
        },
        vec![],
        None,
    );
    let mut reply1 = String::new();
    let mut stages1 = Vec::new();
    while let Some(ev) = rx1.recv().await {
        match ev {
            TurnEvent::Speech { text, .. } | TurnEvent::TextReply(text) => reply1 = text,
            TurnEvent::Timing { stage, .. } => stages1.push(stage),
            _ => {}
        }
    }
    h1.await.unwrap();
    assert!(reply1.contains("百分之40"), "{reply1}");
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "口语快答不应调用主大模型"
    );
    assert!(stages1.contains(&"jev_filter") && stages1.contains(&"fast_route"));

    // 第 2 轮断言：走主大模型，但 4 个无关工具已被裁剪为 0 个
    let (h2, mut rx2) = spawn_turn(
        conversation,
        ctx,
        TurnInput::Utterance {
            pcm: vec![0; 960],
            sample_rate: UP,
            stream: None,
        },
        vec![],
        None,
    );
    while rx2.recv().await.is_some() {}
    h2.await.unwrap();
    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].1, 1,
        "闲聊轮次只给主大模型挂宿主保留的 end_conversation，业务工具为 0"
    );
}

#[test]
fn jev_unfinished_hold_waits_for_speaker_continuation_instead_of_cutting_in_early() {
    use crate::jev::{DecisionClient, DecisionError, JevFilterOptions, VoiceJevFilter};

    struct TwoPartStreamingAsr;
    impl StreamingAsr for TwoPartStreamingAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            let handle = tokio::spawn(async move {
                let mut total_samples = 0usize;
                while let Some(chunk) = rx.recv().await {
                    total_samples += chunk.len();
                    // 前半段（~300ms = 4800 采样点）只说了半截，续说后超过 7000 点变成完整句
                    if total_samples < 7000 {
                        interim_tx.send_replace("把激光功率调到".into());
                    } else {
                        interim_tx.send_replace("把激光功率调到四百".into());
                    }
                }
                let final_text = if total_samples >= 7000 {
                    "把激光功率调到四百"
                } else {
                    "把激光功率调到"
                };
                let _ = done_tx.send(Ok(final_text.into()));
            });
            AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
        }
    }

    struct UnfinishedJev;
    impl DecisionClient for UnfinishedJev {
        fn decide<'a>(
            &'a self,
            payload: &'a Value,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Value, DecisionError>> {
            let utterance = payload["state"]["utterance"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            Box::pin(async move {
                let unfinished = if utterance == "把激光功率调到" {
                    0.92
                } else {
                    0.05
                };
                Ok(serde_json::json!({
                    "answers": {
                        "unfinished": {"type": "noul", "noul": unfinished},
                        "quick_read": {
                            "type": "choice",
                            "choice": "none",
                            "confidence": 0.95,
                            "probabilities": {"none": 0.96, "status": 0.04}
                        }
                    }
                }))
            })
        }
    }

    let llm = Arc::new(FakeLlm::default());
    let tts = Arc::new(FakeTts::default());
    let filter = Arc::new(VoiceJevFilter::new(
        Arc::new(UnfinishedJev),
        JevFilterOptions {
            enabled: true,
            tool_prune: false,
            memory_filter: false,
            ..Default::default()
        },
    ));
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        tts,
        Arc::new(NoTools),
        ConversationSettings::default(),
    )
    .with_streaming_asr(Arc::new(TwoPartStreamingAsr))
    .with_jev_filter(filter);

    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );

    // 第 1 段：说 300ms（半截话“把激光功率调到”）+ 停顿 260ms 触发首次 VAD SpeechEnd
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 300, 8000.0)));
    std::thread::sleep(Duration::from_millis(40));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));
    let wait_deadline = Instant::now() + Duration::from_millis(2500);
    while !server
        .status()
        .events
        .iter()
        .any(|e| e.text.contains("尚未说完"))
        && Instant::now() < wait_deadline
    {
        std::thread::sleep(Duration::from_millis(15));
    }

    // 第 2 段：在 1.2s 宽限窗内继续说 300ms（“四百”）+ 停顿 260ms 结束整句
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 300, 8000.0)));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));

    let (texts, _, _) = collect_playback(&mut device, 1);
    let stt = texts
        .iter()
        .find(|m| m["type"] == "stt")
        .and_then(|m| m["text"].as_str())
        .unwrap_or("");
    assert_eq!(
        stt, "把激光功率调到四百",
        "半截话不应提前抢话截断: {texts:?}"
    );
    assert_eq!(
        llm.seen.lock().unwrap().len(),
        1,
        "整句只应调用一次大模型，而不是被切成两轮"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("尚未说完")),
        "应记录延长拾音事件"
    );
}

#[test]
fn jev_unfinished_hold_with_async_vad_worker_and_rapid_interims_keeps_continuation_without_storm() {
    use crate::audio::{EnergyVad, Vad, VadConfig, VadFactory};
    use crate::jev::{DecisionClient, DecisionError, JevFilterOptions, VoiceJevFilter};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct WorkerEnergyVadFactory;
    impl VadFactory for WorkerEnergyVadFactory {
        fn create(&self, sample_rate: u32, config: &VadConfig) -> Option<Box<dyn Vad>> {
            Some(Box::new(EnergyVad::new(sample_rate, config.clone())))
        }
    }

    struct RapidInterimStreamingAsr;
    impl StreamingAsr for RapidInterimStreamingAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            let handle = tokio::spawn(async move {
                let mut total_samples = 0usize;
                let mut burst_sent = false;
                while let Some(chunk) = rx.recv().await {
                    total_samples += chunk.len();
                    if total_samples < 7000 {
                        if !burst_sent {
                            burst_sent = true;
                            // 模拟流式识别高频连续跳字，验证 120ms 防抖取消不会引发请求风暴或耗尽信号量
                            for part in [
                                "把",
                                "把激",
                                "把激光",
                                "把激光功",
                                "把激光功率",
                                "把激光功率调",
                                "把激光功率调到",
                            ] {
                                interim_tx.send_replace(part.into());
                                tokio::time::sleep(Duration::from_millis(8)).await;
                            }
                        } else {
                            interim_tx.send_replace("把激光功率调到".into());
                        }
                    } else {
                        interim_tx.send_replace("把激光功率调到四百".into());
                    }
                }
                let final_text = if total_samples >= 7000 {
                    "把激光功率调到四百"
                } else {
                    "把激光功率调到"
                };
                let _ = done_tx.send(Ok(final_text.into()));
            });
            AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
        }
    }

    struct CountingUnfinishedJev(AtomicUsize);
    impl DecisionClient for CountingUnfinishedJev {
        fn decide<'a>(
            &'a self,
            payload: &'a Value,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Value, DecisionError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            let utterance = payload["state"]["utterance"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(35)).await;
                let unfinished = if utterance == "把激光功率调到" {
                    0.92
                } else {
                    0.05
                };
                Ok(serde_json::json!({
                    "answers": {
                        "unfinished": {"type": "noul", "noul": unfinished},
                        "quick_read": {
                            "type": "choice",
                            "choice": "none",
                            "confidence": 0.95,
                            "probabilities": {"none": 0.96, "status": 0.04}
                        }
                    }
                }))
            })
        }
    }

    let llm = Arc::new(FakeLlm::default());
    let jev_client = Arc::new(CountingUnfinishedJev(AtomicUsize::new(0)));
    let filter = Arc::new(VoiceJevFilter::new(
        jev_client.clone(),
        JevFilterOptions {
            enabled: true,
            tool_prune: false,
            memory_filter: false,
            ..Default::default()
        },
    ));
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        Arc::new(FakeTts::default()),
        Arc::new(NoTools),
        ConversationSettings::default(),
    )
    .with_streaming_asr(Arc::new(RapidInterimStreamingAsr))
    .with_jev_filter(filter);

    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            vad_factory: Some(Arc::new(WorkerEnergyVadFactory)),
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );

    // 第 1 段语音 + 260ms 停顿，紧接着不等 35ms Jev 返回就送入第 2 段开头，验证 VadWorker 在途帧绝不被清空
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 300, 8000.0)));
    std::thread::sleep(Duration::from_millis(80));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));
    std::thread::sleep(Duration::from_millis(10));
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 320, 8000.0)));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));

    let (texts, _, _) = collect_playback(&mut device, 1);
    let stt = texts
        .iter()
        .find(|m| m["type"] == "stt")
        .and_then(|m| m["text"].as_str())
        .unwrap_or("");
    assert_eq!(
        stt, "把激光功率调到四百",
        "VadWorker 队列中的续说帧不应丢失: {texts:?}"
    );
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    let total_jev_calls = jev_client.0.load(Ordering::SeqCst);
    assert!(
        total_jev_calls <= 3,
        "7 次快速跳字不应引发请求风暴，实际请求数={total_jev_calls}"
    );
}

#[test]
fn abort_and_ping_during_inflight_jev_hold_check_respond_without_blocking_and_cancel_utterance() {
    use crate::jev::{DecisionClient, DecisionError, JevFilterOptions, VoiceJevFilter};

    struct PartialAsr;
    impl StreamingAsr for PartialAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            let handle = tokio::spawn(async move {
                while rx.recv().await.is_some() {
                    interim_tx.send_replace("把激光功率调到".into());
                }
                let _ = done_tx.send(Ok("把激光功率调到".into()));
            });
            AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
        }
    }

    struct SlowHoldJev;
    impl DecisionClient for SlowHoldJev {
        fn decide<'a>(
            &'a self,
            _payload: &'a Value,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Value, DecisionError>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(280)).await;
                Ok(serde_json::json!({
                    "answers": {
                        "unfinished": {"type": "noul", "noul": 0.95}
                    }
                }))
            })
        }
    }

    let llm = Arc::new(FakeLlm::default());
    let filter = Arc::new(VoiceJevFilter::new(
        Arc::new(SlowHoldJev),
        JevFilterOptions {
            enabled: true,
            fast_route: false,
            tool_prune: false,
            memory_filter: false,
            ..Default::default()
        },
    ));
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        Arc::new(FakeTts::default()),
        Arc::new(NoTools),
        ConversationSettings::default(),
    )
    .with_streaming_asr(Arc::new(PartialAsr))
    .with_jev_filter(filter);

    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 300, 8000.0)));
    std::thread::sleep(Duration::from_millis(30));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));

    // Jev 280ms 续听检查仍在途：此时发 ping 必须零阻塞立即收到 pong（远早于 280ms）
    let ping_at = Instant::now();
    send_json(&mut device, r#"{"type":"ping"}"#);
    loop {
        let msg = read_json(&mut device);
        if msg["type"] == "pong" {
            break;
        }
    }
    assert!(
        ping_at.elapsed() < Duration::from_millis(180),
        "在途 Jev 续听检查不得阻塞会话消息循环: {:?}",
        ping_at.elapsed()
    );

    // 在途或挂起期间发 abort：必须立即取消挂起录音，宽限窗过后也绝不把半截话送进大模型
    send_json(&mut device, r#"{"type":"abort","reason":"user_cancel"}"#);
    std::thread::sleep(Duration::from_millis(1450));
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "Abort 后挂起的半截话绝不能在宽限窗到期后发给大模型"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("拾音已取消")),
        "应记录拾音取消事件"
    );
}

#[test]
fn slow_vad_negative_jev_and_concurrent_job_done_preserve_queued_continuation_and_defer_notice() {
    use crate::audio::{EnergyVad, Vad, VadConfig, VadEvent, VadFactory};
    use crate::jev::{DecisionClient, DecisionError, JevFilterOptions, VoiceJevFilter};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DelayedContinuationVad {
        inner: EnergyVad,
        slow_mode: bool,
    }
    impl Vad for DelayedContinuationVad {
        fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent> {
            if self.slow_mode {
                std::thread::sleep(Duration::from_millis(12));
            }
            let events = self.inner.accept(pcm);
            if events.iter().any(|e| matches!(e, VadEvent::SpeechEnd)) {
                self.slow_mode = true;
            }
            events
        }
        fn reset(&mut self) {
            self.slow_mode = false;
            self.inner.reset();
        }
        fn in_speech(&self) -> bool {
            self.inner.in_speech()
        }
    }

    struct SlowContinuationVadFactory;
    impl VadFactory for SlowContinuationVadFactory {
        fn create(&self, sample_rate: u32, config: &VadConfig) -> Option<Box<dyn Vad>> {
            Some(Box::new(DelayedContinuationVad {
                inner: EnergyVad::new(sample_rate, config.clone()),
                slow_mode: false,
            }))
        }
    }

    struct TwoPartStreamingAsr;
    impl StreamingAsr for TwoPartStreamingAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            let handle = tokio::spawn(async move {
                let mut total_samples = 0usize;
                while let Some(chunk) = rx.recv().await {
                    total_samples += chunk.len();
                    if total_samples < 7000 {
                        interim_tx.send_replace("把激光功率调到".into());
                    } else {
                        interim_tx.send_replace("把激光功率调到四百".into());
                    }
                }
                let text = if total_samples >= 7000 {
                    "把激光功率调到四百"
                } else {
                    "把激光功率调到"
                };
                let _ = done_tx.send(Ok(text.into()));
            });
            AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
        }
    }

    // 快速返回 held=false（35ms），早于慢速 VadWorker 处理完已入队的第 2 段语音开头
    struct FastNegativeJev;
    impl DecisionClient for FastNegativeJev {
        fn decide<'a>(
            &'a self,
            _payload: &'a Value,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Value, DecisionError>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(35)).await;
                Ok(serde_json::json!({
                    "answers": {
                        "unfinished": {"type": "noul", "noul": 0.05}
                    }
                }))
            })
        }
    }

    struct SwitchableJobTools {
        completed: Arc<AtomicBool>,
    }
    impl ToolExecutor for SwitchableJobTools {
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
            let done = self.completed.load(Ordering::SeqCst);
            Box::pin(async move {
                Some(serde_json::json!({"workflows":[{
                    "workflow_id": "wf-during-hold",
                    "status": if done { "completed" } else { "sending" },
                    "send_result": {"lines_sent": 342, "total_lines": 342}
                }]}))
            })
        }
    }

    let completed = Arc::new(AtomicBool::new(false));
    let llm = Arc::new(FakeLlm::default());
    let tts = Arc::new(FakeTts::default());
    let filter = Arc::new(VoiceJevFilter::new(
        Arc::new(FastNegativeJev),
        JevFilterOptions {
            enabled: true,
            fast_route: false,
            tool_prune: false,
            memory_filter: false,
            ..Default::default()
        },
    ));
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        tts.clone(),
        Arc::new(SwitchableJobTools {
            completed: completed.clone(),
        }),
        ConversationSettings::default(),
    )
    .with_streaming_asr(Arc::new(TwoPartStreamingAsr))
    .with_jev_filter(filter);

    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            vad_factory: Some(Arc::new(SlowContinuationVadFactory)),
            job_poll_interval: Duration::from_millis(25),
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);

    // 先让空闲轮询记录一次 sending 基线
    std::thread::sleep(Duration::from_millis(45));
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    // 录音与续听挂起期间任务变为 completed，不得抢占覆盖正在拾音/挂起的半句话
    completed.store(true, Ordering::SeqCst);

    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 300, 8000.0)));
    std::thread::sleep(Duration::from_millis(50));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));
    std::thread::sleep(Duration::from_millis(10));
    // 第 2 段音频在 Jev 35ms 返回 false 之前已经送入慢速 VadWorker（每帧 12ms，尚未吐出 SpeechStart）
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 320, 8000.0)));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));

    // 第 1 次播报必须是完整拼接后的用户回答（不能被 worker.reset 截断，也不能被任务播报抢断）
    let (turn_texts, _, _) = collect_playback(&mut device, 1);
    let stt = turn_texts
        .iter()
        .find(|m| m["type"] == "stt")
        .and_then(|m| m["text"].as_str())
        .unwrap_or("");
    assert_eq!(
        stt, "把激光功率调到四百",
        "慢速 VAD 已入队的续说帧在快速否定判定返回时不得丢失: {turn_texts:?}"
    );
    assert_eq!(llm.seen.lock().unwrap().len(), 1);

    // 用户本轮安全结束后回到 Idle，延后的加工完成通知随后正常播报且不丢失
    let (notice_texts, notice_frames, _) = collect_playback(&mut device, 1);
    assert!(
        !notice_frames.is_empty(),
        "延后的任务完成通知必须正常播报: {notice_texts:?}"
    );
    let said = tts.said.lock().unwrap().clone();
    assert!(
        said.iter().any(|t| t.contains("雕刻任务已完成")),
        "应在用户对话结束后补播加工完成结果: {said:?}"
    );
}

#[test]
fn smart_turn_low_probability_holds_turn_gate_and_caps_audio_to_8_seconds() {
    use crate::conversation::TurnEndPredictor;

    struct FakeTurnPredictor {
        seen: Mutex<Vec<(usize, u32)>>,
        prob: f32,
    }
    impl TurnEndPredictor for FakeTurnPredictor {
        fn predict_turn_end<'a>(
            &'a self,
            pcm: &'a [i16],
            sample_rate: u32,
        ) -> BoxFuture<'a, Option<f32>> {
            Box::pin(async move {
                self.seen.lock().unwrap().push((pcm.len(), sample_rate));
                Some(self.prob)
            })
        }
    }

    let predictor = Arc::new(FakeTurnPredictor {
        seen: Mutex::new(Vec::new()),
        prob: 0.15,
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([call("get_time"), say(&["十二点。"])])),
        ..FakeLlm::default()
    });
    let tools = Arc::new(FakeTools::default());
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("好了".to_owned())])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        asr,
        llm,
        Arc::new(FakeTts::default()),
        tools.clone(),
        ConversationSettings::default(),
    )
    .with_turn_predictor(predictor.clone());
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    // 送入 8.5 秒语音 + 600ms 静音，验证传给 Smart Turn 的音频裁剪至不超过 8 秒
    let mut audio = tone(UP, 300.0, 8_500, 8000.0);
    audio.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&audio));
    assert_eq!(read_json(&mut device)["text"], "好了");
    // 语义判停概率 0.15 将等待拉长到 1250ms（超过 250ms 基础静音后还要等 1000ms）：
    // 350ms 内大模型发起的 get_time 工具调用必须仍被 TurnGate 扣住
    std::thread::sleep(Duration::from_millis(350));
    assert!(
        tools.calls.lock().unwrap().is_empty(),
        "低说完概率下工具调用在 TurnGate 放行前不得执行"
    );
    let items = collect_items(&mut device);
    assert_eq!(texts(&items), vec!["tts:start", "句:十二点。", "tts:stop"]);
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    let seen = predictor.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, UP);
    assert!(
        seen[0].0 <= UP as usize * 8,
        "传入 Smart Turn 的音频应裁剪至最多 8 秒: {}",
        seen[0].0
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("这句话像是还没说完")),
        "应记录延长等待事件"
    );
}

#[test]
fn smart_turn_slow_predictor_never_delays_stop_fast_path() {
    use crate::conversation::TurnEndPredictor;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SlowTurnPredictor {
        finished: AtomicBool,
    }
    impl TurnEndPredictor for SlowTurnPredictor {
        fn predict_turn_end<'a>(&'a self, _: &'a [i16], _: u32) -> BoxFuture<'a, Option<f32>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                self.finished.store(true, Ordering::SeqCst);
                Some(0.05)
            })
        }
    }

    struct TimedStopAsr {
        asr_done_at: Arc<Mutex<Option<std::time::Instant>>>,
    }
    impl Asr for TimedStopAsr {
        fn transcribe<'a>(
            &'a self,
            _: &'a [i16],
            _: u32,
        ) -> BoxFuture<'a, Result<String, ProviderError>> {
            Box::pin(async move {
                *self.asr_done_at.lock().unwrap() = Some(std::time::Instant::now());
                Ok("急停".to_owned())
            })
        }
    }

    let predictor = Arc::new(SlowTurnPredictor {
        finished: AtomicBool::new(false),
    });
    let asr_done_at = Arc::new(Mutex::new(None));
    let tools = Arc::new(FastTools::default());
    let llm = Arc::new(FakeLlm::default());
    let conversation = Conversation::new(
        Arc::new(TimedStopAsr {
            asr_done_at: asr_done_at.clone(),
        }),
        llm.clone(),
        Arc::new(FakeTts::default()),
        tools.clone(),
        ConversationSettings::default(),
    )
    .with_turn_predictor(predictor.clone());
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut audio = tone(UP, 300.0, 500, 8000.0);
    audio.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&audio));

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while tools.calls.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let tool_ran_at = std::time::Instant::now();
    let asr_at = asr_done_at.lock().unwrap().expect("ASR 应已运行");
    let after_asr_ms = tool_ran_at.duration_since(asr_at).as_millis();
    let items = collect_items(&mut device);
    assert!(texts(&items).contains(&"听:急停".to_owned()), "{items:?}");
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    assert_eq!(tools.calls.lock().unwrap()[0].0, "laser_safe_action_tool");
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "急停快路径不应请求大模型"
    );
    assert!(
        !predictor.finished.load(Ordering::SeqCst),
        "急停命中后应立即中止慢速尾音判停任务，不得等其 2 秒完成"
    );
    assert!(
        after_asr_ms < 500,
        "急停快路径不应被尾音判停超时或额外等待阻塞（耗时 {after_asr_ms} ms）"
    );
}

#[test]
fn reduplicated_exit_skips_jev_hold_and_same_interim_prewarm_is_reused_without_duplicate_call() {
    use crate::jev::{DecisionClient, DecisionError, JevFilterOptions, VoiceJevFilter};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScriptedInterimAsr(Mutex<VecDeque<&'static str>>);
    impl StreamingAsr for ScriptedInterimAsr {
        fn start(&self) -> AsrStream {
            let text = self.0.lock().unwrap().pop_front().unwrap_or("你好");
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            let handle = tokio::spawn(async move {
                let mut pushed = false;
                while rx.recv().await.is_some() {
                    if !pushed {
                        pushed = true;
                        interim_tx.send_replace(text.into());
                    }
                }
                let _ = done_tx.send(Ok(text.into()));
            });
            AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
        }
    }

    struct CountingJev(AtomicUsize);
    impl DecisionClient for CountingJev {
        fn decide<'a>(
            &'a self,
            _payload: &'a Value,
            _timeout: Duration,
        ) -> BoxFuture<'a, Result<Value, DecisionError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok(serde_json::json!({
                    "answers": {
                        "unfinished": {"type": "noul", "noul": 0.05},
                        "quick_read": {
                            "type": "choice",
                            "choice": "none",
                            "confidence": 0.96,
                            "probabilities": {"none": 0.97, "status": 0.03}
                        }
                    }
                }))
            })
        }
    }

    let jev_client = Arc::new(CountingJev(AtomicUsize::new(0)));
    let filter = Arc::new(VoiceJevFilter::new(
        jev_client.clone(),
        JevFilterOptions {
            enabled: true,
            tool_prune: false,
            memory_filter: false,
            ..Default::default()
        },
    ));
    let llm = Arc::new(FakeLlm::default());
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        Arc::new(FakeTts::default()),
        Arc::new(NoTools),
        ConversationSettings::default(),
    )
    .with_streaming_asr(Arc::new(ScriptedInterimAsr(Mutex::new(VecDeque::from([
        "你是什么",
        "退下退下",
    ])))))
    .with_jev_filter(filter);

    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);

    // 第 1 轮：“你是什么”在说话期已起预热，静音判停直接复用同一请求并写缓存，整轮 Jev 只请求 1 次
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 360, 8000.0)));
    std::thread::sleep(Duration::from_millis(160));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));
    let _ = collect_items(&mut device);
    assert_eq!(
        jev_client.0.load(Ordering::SeqCst),
        1,
        "说话期预热、静音判停与正式轮次应共享同一次 Jev 推理"
    );
    assert_eq!(llm.seen.lock().unwrap().len(), 1);

    // 第 2 轮：“退下退下”走整句退出快路径，0 次 Jev、0 次新增大模型请求，播完再见断开
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 320, 8000.0)));
    std::thread::sleep(Duration::from_millis(40));
    send_audio(&mut device, 1, &uplink(&silence(UP, 460)));
    let exit_items = texts(&collect_items(&mut device));
    assert!(
        exit_items.contains(&"听:退下退下".to_owned()),
        "{exit_items:?}"
    );
    expect_closed(&mut device);
    assert_eq!(
        jev_client.0.load(Ordering::SeqCst),
        1,
        "连说退出词不得触发 Jev 预热或续听等待"
    );
    assert_eq!(
        llm.seen.lock().unwrap().len(),
        1,
        "连说退出词不得落入大模型"
    );
}
