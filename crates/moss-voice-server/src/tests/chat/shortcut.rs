//! 快路径：固定路由跳过大模型、整句停止抢先执行、预热话术缓存与慢工具垫话的安全边界。
use super::*;

#[tokio::test]
async fn fixed_routes_skip_llm_keep_full_progress_and_report_failure() {
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    for (text, expect, success) in [
        ("急停", "已发送停止请求", true),
        ("停止", "发送失败", false),
        ("当前进度", "百分之40", true),
        ("还要多久", "没有可信", true),
        ("当前材料参数", "材料库建议功率400", true),
    ] {
        let tools = Arc::new(FastTools {
            fail: !success,
            ..Default::default()
        });
        let llm = Arc::new(FakeLlm::default());
        let asr = Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok(text.into())])),
            ..Default::default()
        });
        let conversation = Arc::new(Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            tools.clone(),
            ConversationSettings::default(),
        ));
        let context = ToolContext {
            device_id: "fake-device".into(),
            session_id: "fake-session".into(),
        };
        let (handle, mut events) = spawn_turn(
            conversation,
            context.clone(),
            TurnInput::Utterance {
                pcm: vec![0; 960],
                sample_rate: UP,
                stream: None,
            },
            vec![],
            None,
        );
        let mut reply = String::new();
        let mut stages = Vec::new();
        let mut durations = Vec::new();
        while let Some(event) = events.recv().await {
            match event {
                TurnEvent::Speech { text, .. } | TurnEvent::TextReply(text) => reply = text,
                TurnEvent::Timing { stage, .. } => stages.push(stage),
                TurnEvent::ToolTiming { ms, .. } => durations.push(ms),
                _ => {}
            }
        }
        handle.await.unwrap();
        assert!(reply.contains(expect), "{text}: {reply}");
        assert!(llm.seen.lock().unwrap().is_empty());
        assert!(stages.contains(&"final_asr") && stages.contains(&"fast_route"));
        assert!(!stages.contains(&"first_llm_delta"));
        assert!(durations.iter().all(|ms| *ms >= 10));
        let calls = tools.calls.lock().unwrap();
        assert!(calls.iter().all(|(_, _, ctx)| *ctx == context));
        assert!(calls.iter().all(|(_, args, _)| {
            [
                None,
                Some("status"),
                Some("recent"),
                Some("get"),
                Some("emergency_stop"),
            ]
            .contains(&args["action"].as_str())
        }));
        if crate::fast::is_stop(text) {
            assert_eq!(
                calls[0].1,
                serde_json::json!({"action":"emergency_stop","confirmed":true})
            );
            assert_eq!(calls.len(), 1);
        }
    }
    // 关闭只读路由只影响只读词，停止始终保留；否定/嵌入词转正常 LLM。
    for text in ["当前进度", "别停", "停止之后怎么办"] {
        let llm = Arc::new(FakeLlm::default());
        let tools = Arc::new(FastTools::default());
        let asr = Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok(text.into())])),
            ..Default::default()
        });
        let c = Arc::new(Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            tools.clone(),
            ConversationSettings {
                read_only_shortcuts: false,
                ..Default::default()
            },
        ));
        let (handle, mut rx) = spawn_turn(
            c,
            ToolContext {
                device_id: "d".into(),
                session_id: "s".into(),
            },
            TurnInput::Utterance {
                pcm: vec![0; 960],
                sample_rate: UP,
                stream: None,
            },
            vec![],
            None,
        );
        while rx.recv().await.is_some() {}
        handle.await.unwrap();
        assert_eq!(llm.seen.lock().unwrap().len(), 1);
        assert!(tools.calls.lock().unwrap().is_empty());
    }
}

/// 任务结束后问“当前进度”必须回答终态，不能再说“没有找到正在发送的加工任务”（2026-11-02）。
#[tokio::test]
async fn progress_shortcut_reports_terminal_state_after_job_ends() {
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    struct EndedTools {
        status: &'static str,
        sent: u64,
        total: u64,
    }
    impl ToolExecutor for EndedTools {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![]
        }
        fn call<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async { panic!("固定路由必须使用完整结果") })
        }
        fn call_raw<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, crate::ToolHostResult> {
            Box::pin(async move {
                let body = serde_json::json!({"workflows":[{
                    "workflow_id":"wf-done",
                    "status": self.status,
                    "updated_at": 10,
                    "send_result": {"lines_sent": self.sent, "total_lines": self.total}
                }]});
                crate::ToolHostResult {
                    success: true,
                    result: "已列出最近的激光 workflow".into(),
                    detail: Some(serde_json::json!({"success":true,"result":body}).to_string()),
                }
            })
        }
    }
    for (status, sent, total, expect) in [
        ("completed", 342u64, 342u64, "上一个任务已经完成，共 342 行"),
        ("cancelled", 120, 342, "上一个任务已经取消，停在第 120 行"),
        ("failed", 88, 342, "上一个任务失败了"),
        ("preview_ready", 0, 0, "最近一条任务还没开始发送"),
    ] {
        let tools = Arc::new(EndedTools {
            status,
            sent,
            total,
        });
        let llm = Arc::new(FakeLlm::default());
        let asr = Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("当前进度".into())])),
            ..Default::default()
        });
        let conversation = Arc::new(Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            tools,
            ConversationSettings::default(),
        ));
        let context = ToolContext {
            device_id: "d".into(),
            session_id: "s".into(),
        };
        let (handle, mut events) = spawn_turn(
            conversation,
            context,
            TurnInput::Utterance {
                pcm: vec![0; 960],
                sample_rate: UP,
                stream: None,
            },
            vec![],
            None,
        );
        let mut reply = String::new();
        while let Some(event) = events.recv().await {
            match event {
                TurnEvent::Speech { text, .. } | TurnEvent::TextReply(text) => reply = text,
                _ => {}
            }
        }
        handle.await.unwrap();
        assert!(reply.contains(expect), "{status}: {reply}");
        assert!(
            llm.seen.lock().unwrap().is_empty(),
            "{status} 不该绕道大模型"
        );
    }
}

struct InterimStopAsr {
    final_text: &'static str,
    sent: Arc<std::sync::atomic::AtomicBool>,
}

impl StreamingAsr for InterimStopAsr {
    fn start(&self) -> AsrStream {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
        let (final_text, sent) = (self.final_text, self.sent.clone());
        let handle = tokio::spawn(async move {
            while rx.recv().await.is_some() {
                interim_tx.send_replace("停止".into());
                sent.store(true, std::sync::atomic::Ordering::Release);
            }
            let _ = done_tx.send(Ok(final_text.into()));
        });
        AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
    }
}

#[test]
fn interim_whole_stop_executes_without_waiting_for_final_and_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    for text in ["停止之后怎么办", "不要停", "停止"] {
        let tools = Arc::new(FastTools::default());
        let llm = Arc::new(FakeLlm::default());
        let sent = Arc::new(AtomicBool::new(false));
        let conversation = Conversation::new(
            Arc::new(FakeAsr::default()),
            llm.clone(),
            Arc::new(FakeTts::default()),
            tools.clone(),
            ConversationSettings::default(),
        )
        .with_streaming_asr(Arc::new(InterimStopAsr {
            final_text: text,
            sent: sent.clone(),
        }));
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
            r#"{"type":"listen","state":"start","mode":"manual"}"#,
        );
        send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 300, 8000.0)));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !sent.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(sent.load(Ordering::Acquire));
        send_json(&mut device, r#"{"type":"ping"}"#);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let message = read_json(&mut device);
            if message["type"] == "pong" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ping reply must not be lost during early stop"
            );
        }
        // 即使最终识别稍后会给出否定句，完整中间识别为“停止”也应立即触发；
        // 这是 NV8a-1 用户选定的低延迟策略，误触发风险须在 UI 和实机验收说明。
        let deadline = Instant::now() + Duration::from_secs(3);
        while tools.calls.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            tools.calls.lock().unwrap().len(),
            1,
            "whole interim stop must act before listen stop"
        );
        send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
        let _items = collect_items(&mut device);
        assert_eq!(
            tools.calls.lock().unwrap().len(),
            1,
            "interim and final must not trigger twice"
        );
        assert!(llm.seen.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn prewarm_speech_cache_and_slow_tool_filler_respect_safety_boundaries() {
    use crate::conversation::{
        SLOW_CHECK_FILLER, SLOW_GENERATE_FILLER, SLOW_QUERY_FILLER, STOP_ERR_SPEECH,
        STOP_OK_SPEECH, TurnEvent, TurnInput, slow_tool_filler, spawn_turn,
    };
    let mk_call = |name: &str, args: &str| ToolCall::new(format!("call-{name}"), name, args);

    // 1. 慢速外部查询工具触发查询过渡垫音
    for name in ["web_search", "search_rag", "get_weather", "get_news"] {
        assert_eq!(
            slow_tool_filler(&[mk_call(name, "{}")]),
            Some(SLOW_QUERY_FILLER),
            "{name} should trigger query filler"
        );
    }

    // 2. 慢速 AI 生图/刀路预览生成（非 R3 发送审批）触发生成预览垫音
    for name in [
        "generate_text_image_tool",
        "ai_laser_gcode_tool",
        "generate_text_image_gcode_tool",
        "generate_text_laser_task_tool",
        "regenerate_text_laser_task_tool",
    ] {
        assert_eq!(
            slow_tool_filler(&[mk_call(name, r#"{"prompt":"小猫"}"#)]),
            Some(SLOW_GENERATE_FILLER),
            "{name} preview should trigger generate filler"
        );
    }

    // n72：精简清单的刻字预览、测试矩阵生成播“生成预览”，只读检查机器播“看一下机器”
    assert_eq!(
        slow_tool_filler(&[mk_call(
            "laser_workflow_tool",
            r#"{"action":"preview","text":"你好"}"#
        )]),
        Some(SLOW_GENERATE_FILLER)
    );
    assert_eq!(
        slow_tool_filler(&[mk_call(
            "run_calibration_grid_tool",
            r#"{"material":"椴木"}"#
        )]),
        Some(SLOW_GENERATE_FILLER)
    );
    assert_eq!(
        slow_tool_filler(&[mk_call("check_laser_connection_tool", "{}")]),
        Some(SLOW_CHECK_FILLER)
    );
    // 开始刻矩阵、确认发送、续雕都是 R3：绝不播垫音
    for (name, args) in [
        (
            "run_calibration_grid_tool",
            r#"{"calibration_id":"cal-1","confirmed":true}"#,
        ),
        (
            "laser_workflow_tool",
            r#"{"action":"confirm_send","workflow_id":"wf-1","confirmed":true}"#,
        ),
        (
            "laser_workflow_tool",
            r#"{"action":"resume","workflow_id":"wf-1","confirmed":true}"#,
        ),
    ] {
        assert_eq!(
            slow_tool_filler(&[mk_call(name, args)]),
            None,
            "{name} {args}"
        );
    }

    // 3. 安全边界：一旦携带 R3 动作参数或属于控制/审批/本地毫秒级快工具，绝不播垫音
    assert_eq!(
        slow_tool_filler(&[mk_call(
            "ai_laser_gcode_tool",
            r#"{"confirmed":true,"send_after_generate":true}"#
        )]),
        None
    );
    assert_eq!(
        slow_tool_filler(&[mk_call(
            "regenerate_text_laser_task_tool",
            r#"{"feedback_id":"fb-1"}"#
        )]),
        None
    );
    for forbidden in [
        "laser_workflow_tool",
        "laser_safe_action_tool",
        "start_tuned_job_tool",
        "material_params_tool",
        "get_lunar",
        "hass_set_state",
        "agent_codex",
        "agent_claude_code",
        "play_music",
        "ext__search",
        "device__set_volume",
    ] {
        assert_eq!(
            slow_tool_filler(&[mk_call(forbidden, "{}")]),
            None,
            "{forbidden} must never trigger slow tool filler"
        );
    }
    // 混合慢查询与控制工具时也绝不触发垫音
    assert_eq!(
        slow_tool_filler(&[
            mk_call("get_weather", "{}"),
            mk_call("laser_workflow_tool", "{}")
        ]),
        None
    );

    // 4. 验证 prewarm_cache 提前合成后，唤醒、慢查询垫音、R3 审批播报全部直接命中内存缓存
    struct SlowAndApprovalTools;
    impl ToolExecutor for SlowAndApprovalTools {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![
                ToolSpec {
                    name: "web_search".into(),
                    description: "联网搜索".into(),
                    parameters: serde_json::json!({"type": "object"}),
                },
                ToolSpec {
                    name: "laser_workflow_tool".into(),
                    description: "激光流程".into(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            ]
        }
        fn call<'a>(
            &'a self,
            _: &'a ToolContext,
            name: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move {
                match name {
                    "web_search" => {
                        // 比垫音起说延迟慢：工具迟迟不回才说垫音
                        tokio::time::sleep(
                            crate::conversation::SLOW_TOOL_FILLER_DELAY
                                + Duration::from_millis(200),
                        )
                        .await;
                        ToolOutcome::Reply("今日晴，25度".into())
                    }
                    "laser_workflow_tool" => {
                        ToolOutcome::Speak(crate::tools::APPROVAL_SPEECH.into())
                    }
                    _ => ToolOutcome::Reply("ok".into()),
                }
            })
        }
    }

    let tts = Arc::new(FakeTts::default());
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            // 第 1 轮对话：模型不先说话，直接调慢工具 web_search，且工具超过 1 秒没返回 -> 播预缓存的 SLOW_QUERY_FILLER
            call("web_search"),
            say(&["今天天气晴朗，25度。"]),
            // 第 2 轮对话：模型调 R3 审批工具 -> 直接播预缓存的 APPROVAL_SPEECH，不走模型二轮
            call("laser_workflow_tool"),
            // 第 3 轮对话：模型在调慢工具的同时自己先流式说了一句过渡话 -> 绝不叠播系统垫音
            Round::Reply {
                deltas: vec!["好的，我帮您查一下。"],
                calls: vec![ToolCall::new("call-web_search-2", "web_search", "{}")],
                delay_ms: 0,
            },
            say(&["查到了，明天也是晴天。"]),
        ])),
        ..FakeLlm::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("帮我搜一下今天天气".into()),
            Ok("确认开始雕刻".into()),
            Ok("明天呢".into()),
        ])),
        ..Default::default()
    });
    let conversation = Arc::new(Conversation::new(
        asr,
        llm.clone(),
        tts.clone(),
        Arc::new(SlowAndApprovalTools),
        ConversationSettings::default(),
    ));

    let phrases = conversation.fixed_phrases();
    assert!(phrases.contains(&"我在".to_owned()));
    assert!(phrases.contains(&STOP_OK_SPEECH.to_owned()));
    assert!(phrases.contains(&STOP_ERR_SPEECH.to_owned()));
    assert!(phrases.contains(&crate::tools::APPROVAL_SPEECH.to_owned()));
    assert!(phrases.contains(&SLOW_QUERY_FILLER.to_owned()));
    assert!(phrases.contains(&SLOW_GENERATE_FILLER.to_owned()));

    let warmed = conversation.prewarm_cache().await;
    // FakeTts 对含“失败”字样的 STOP_ERR_SPEECH 模拟合成报错，验证单条失败不阻断其余 8 条预热
    assert_eq!(warmed, phrases.len() - 1);
    let count_after_prewarm = tts.said.lock().unwrap().len();
    assert_eq!(count_after_prewarm, phrases.len());

    let ctx = ToolContext {
        device_id: "d-cache".into(),
        session_id: "s-cache".into(),
    };

    // 触发慢查询：应先收到缓存垫音 "稍等，正在查询。"，再收到模型最终回答；且垫音不写入历史且不重调 TTS
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
    let mut spoken1 = Vec::new();
    let mut history1 = Vec::new();
    while let Some(ev) = rx1.recv().await {
        match ev {
            TurnEvent::Speech { text, .. } => spoken1.push(text),
            TurnEvent::Done { messages, .. } => history1 = messages,
            _ => {}
        }
    }
    h1.await.unwrap();
    assert_eq!(
        spoken1,
        vec![
            SLOW_QUERY_FILLER.to_owned(),
            "今天天气晴朗，".to_owned(),
            "25度。".to_owned()
        ]
    );
    assert!(
        !history1
            .iter()
            .any(|m| m.content.as_deref() == Some(SLOW_QUERY_FILLER)),
        "过渡垫音不得写入对话历史"
    );
    // TTS 只多合成了模型最终回答切出的 2 句，垫音直接命中预热缓存
    assert_eq!(tts.said.lock().unwrap().len(), count_after_prewarm + 2);

    // 触发 R3 审批工具：不播慢工具垫音，且 APPROVAL_SPEECH 直接命中预热缓存（TTS 调用次数不变）
    let (h2, mut rx2) = spawn_turn(
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
    let mut spoken2 = Vec::new();
    while let Some(ev) = rx2.recv().await {
        if let TurnEvent::Speech { text, .. } = ev {
            spoken2.push(text);
        }
    }
    h2.await.unwrap();
    assert_eq!(spoken2, vec![crate::tools::APPROVAL_SPEECH.to_owned()]);
    assert_eq!(
        tts.said.lock().unwrap().len(),
        count_after_prewarm + 2,
        "R3 审批固定话术应直接命中预热缓存，不再调用底层 TTS"
    );

    // 第 3 轮：模型在返回慢工具时已经自己说了话（spoke_any == true），不得再叠播 SLOW_QUERY_FILLER
    let (h3, mut rx3) = spawn_turn(
        conversation.clone(),
        ctx,
        TurnInput::Utterance {
            pcm: vec![0; 960],
            sample_rate: UP,
            stream: None,
        },
        vec![],
        None,
    );
    let mut spoken3 = Vec::new();
    while let Some(ev) = rx3.recv().await {
        if let TurnEvent::Speech { text, .. } = ev {
            spoken3.push(text);
        }
    }
    h3.await.unwrap();
    assert_eq!(
        spoken3,
        vec![
            "好的，我帮您查一下。".to_owned(),
            "查到了，明天也是晴天。".to_owned()
        ]
    );
}

#[test]
fn homophone_corrected_interim_stop_triggers_fast_stop_before_listen_stop() {
    struct HomophoneStopInterimAsr;
    impl StreamingAsr for HomophoneStopInterimAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            let handle = tokio::spawn(async move {
                if rx.recv().await.is_some() {
                    interim_tx.send_replace("急廷".into());
                }
                while rx.recv().await.is_some() {}
                let _ = done_tx.send(Ok("急廷".into()));
            });
            AsrStream::new(tx, done_rx, handle).with_interim(interim_rx)
        }
    }

    let tools = Arc::new(FastTools {
        fail: true,
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm::default());
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm.clone(),
        Arc::new(FakeTts::default()),
        tools.clone(),
        ConversationSettings {
            asr_corrections: vec![("急廷".into(), "急停".into())],
            ..ConversationSettings::default()
        },
    )
    .with_streaming_asr(Arc::new(HomophoneStopInterimAsr));
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            barge_in: true,
            barge_in_ms: 300,
            conversation: Some(Arc::new(conversation)),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    // 开启 realtime 自然插话，模拟用户喊“急廷——啊！”（前半段触发 FastStop 后，嘴里尾音继续上行 480ms）
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"realtime"}"#,
    );
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 360, 8000.0)));

    let deadline = Instant::now() + Duration::from_secs(3);
    while tools.calls.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        tools.calls.lock().unwrap().len(),
        1,
        "同音纠错后的急停词在说话未结束前必须立即触发急停"
    );
    // 继续送入 480ms 大音量尾音：不得通过自然插话把正在执行的 FastStop 自杀打断或吞掉失败安全播报
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 480, 9000.0)));
    let items = collect_items(&mut device);
    let spoken = texts(&items);
    assert!(spoken.contains(&"听:急停".to_owned()), "{spoken:?}");
    assert!(
        spoken.iter().any(|line| line.contains("发送失败")),
        "尾音不得打断或吞掉急停失败安全告警播报：{spoken:?}"
    );
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "尾音不得被当成新一轮送给大模型"
    );
}
