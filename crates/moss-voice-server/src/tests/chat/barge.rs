//! NV8b：仅回环假设备与假模型，无真实麦克风、云端或激光设备。
use super::*;

struct LongTts;
impl Tts for LongTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            let ms = if text == "好。" { 240 } else { 1200 };
            Ok(Pcm {
                samples: tone(UP, 300.0, ms, 6000.0),
                sample_rate: UP,
            })
        })
    }
}

#[derive(Default)]
struct CaptureStream {
    sizes: Arc<Mutex<Vec<usize>>>,
}
impl StreamingAsr for CaptureStream {
    fn start(&self) -> AsrStream {
        let sizes = self.sizes.clone();
        let index = {
            let mut values = sizes.lock().unwrap();
            let index = values.len();
            values.push(0);
            index
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let (done, result) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            while let Some(pcm) = rx.recv().await {
                sizes.lock().unwrap()[index] += pcm.len();
            }
            let text = if index == 0 { "讲故事" } else { "换话题" };
            let _ = done.send(Ok(text.into()));
        });
        AsrStream::new(tx, result, handle)
    }
}

fn start_barge(
    enabled: bool,
    streaming: Option<Arc<CaptureStream>>,
    first_text: &str,
    round: Round,
) -> (VoiceServer, Arc<FakeRegistry>, Arc<FakeAsr>, Arc<FakeLlm>) {
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok(first_text.into()), Ok("换话题".into())])),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([round, say(&["好。"])])),
        ..Default::default()
    });
    let mut conversation = Conversation::new(
        asr.clone(),
        llm.clone(),
        Arc::new(LongTts),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    );
    if let Some(streaming) = streaming {
        conversation = conversation.with_streaming_asr(streaming);
    }
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            barge_in: enabled,
            barge_in_ms: 400,
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry, asr, llm)
}

fn begin(device: &mut Device, mode: &str) {
    send_json(
        device,
        &serde_json::json!({"type":"listen","state":"start","mode":mode}).to_string(),
    );
    send_audio(device, 1, &uplink(&tone(UP, 300.0, 600, 8000.0)));
    send_json(device, r#"{"type":"listen","state":"stop"}"#);
}

fn first_frame(device: &mut Device) {
    while !matches!(device.read().unwrap(), Message::Binary(_)) {}
}

fn interrupted(server: &VoiceServer) -> bool {
    server
        .status()
        .events
        .iter()
        .any(|event| event.text.starts_with("自然插话："))
}

#[test]
fn realtime_barge_replays_head_immediately_and_keeps_only_started_sentences() {
    for stream in [false, true] {
        let capture = Arc::new(CaptureStream::default());
        let (server, registry, asr, llm) = start_barge(
            true,
            stream.then(|| capture.clone()),
            "讲故事",
            say(&["第一句。", "未播出的第二句。"]),
        );
        let mut device = chat_device(&server, &registry, 1);
        begin(&mut device, "realtime");
        first_frame(&mut device);
        std::thread::sleep(Duration::from_millis(350));
        // 听到插话先暂停；说完识别出“换话题”才确认打断（此时才发 tts stop）
        let mut barge = tone(UP, 500.0, 480, 8000.0);
        barge.extend(silence(UP, 500));
        send_audio(&mut device, 1, &uplink(&barge));
        let previous = texts(&collect_items(&mut device));
        assert!(!previous.iter().any(|text| text.contains("未播出的第二句")));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !interrupted(&server) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            interrupted(&server),
            "tts stop may reach the device before the status event is published"
        );
        if stream {
            let deadline = Instant::now() + Duration::from_secs(2);
            while capture.sizes.lock().unwrap().get(1).copied().unwrap_or(0) < 16 * 420
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                capture.sizes.lock().unwrap()[1] >= 16 * 420,
                "判停前已经补送到流式识别"
            );
        }
        // 某些固件在 stop 后再次发 start：不能丢掉已经收下的开头。
        send_json(
            &mut device,
            r#"{"type":"listen","state":"start","mode":"realtime"}"#,
        );
        send_audio(&mut device, 1, &uplink(&silence(UP, 500)));
        assert!(texts(&collect_items(&mut device)).contains(&"听:换话题".into()));
        if !stream {
            assert!(
                asr.heard.lock().unwrap()[1] >= 16 * 480,
                "整段识别保留插话开头"
            );
        }
        let seen = llm.seen.lock().unwrap();
        let history = &seen[1].0;
        assert_eq!(history[1].content.as_deref(), Some("讲故事"));
        assert_eq!(
            history[2].content.as_deref(),
            Some("第一句。"),
            "模型生成已完成但未播内容不能入历史"
        );
        drop(seen);
        assert!(
            server
                .status()
                .timings
                .iter()
                .any(|timing| timing.outcome == "interrupted")
        );
        // 实时模式正常播完后无须另发 start，直接下一句话。
        let mut next = tone(UP, 300.0, 480, 8000.0);
        next.extend(silence(UP, 500));
        send_audio(&mut device, 1, &uplink(&next));
        collect_items(&mut device);
        assert_eq!(llm.seen.lock().unwrap().len(), 3);
    }
}

#[test]
fn disabled_non_realtime_short_bursts_and_startup_guard_do_not_interrupt() {
    for (enabled, mode, wait, speech) in [
        (false, "realtime", 350, 600),
        (true, "auto", 350, 600),
        (true, "manual", 350, 600),
        (true, "realtime", 350, 180),
        (true, "realtime", 0, 600),
    ] {
        let (server, registry, asr, _) =
            start_barge(enabled, None, "讲故事", say(&["第一句。", "第二句。"]));
        let mut device = chat_device(&server, &registry, 1);
        begin(&mut device, mode);
        first_frame(&mut device);
        std::thread::sleep(Duration::from_millis(wait));
        let mut audio = tone(UP, 500.0, speech, 8000.0);
        audio.extend(silence(UP, 180));
        if speech < 400 {
            audio.extend(tone(UP, 500.0, speech, 8000.0));
            audio.extend(silence(UP, 180));
        }
        send_audio(&mut device, 1, &uplink(&audio));
        collect_items(&mut device);
        assert!(
            !interrupted(&server),
            "enabled={enabled} mode={mode} wait={wait} speech={speech}"
        );
        assert_eq!(asr.heard.lock().unwrap().len(), 1);
    }
}

#[test]
fn greetings_and_exit_goodbyes_are_protected() {
    for goodbye in [false, true] {
        let (server, registry, _, _) = start_barge(true, None, "再见", say(&["不用模型。"]));
        let mut device = chat_device(&server, &registry, 1);
        if goodbye {
            begin(&mut device, "realtime");
        } else {
            send_json(
                &mut device,
                r#"{"type":"listen","state":"detect","text":"你好小智"}"#,
            );
            send_json(
                &mut device,
                r#"{"type":"listen","state":"start","mode":"realtime"}"#,
            );
        }
        first_frame(&mut device);
        std::thread::sleep(Duration::from_millis(350));
        send_audio(&mut device, 1, &uplink(&tone(UP, 500.0, 600, 8000.0)));
        collect_items(&mut device);
        assert!(!interrupted(&server));
        if goodbye {
            expect_closed(&mut device);
        }
    }
}

#[test]
fn realtime_can_interrupt_thinking_before_any_audio() {
    let slow = Round::Reply {
        deltas: vec!["原回答。"],
        calls: vec![],
        delay_ms: 1500,
    };
    let (server, registry, _, llm) = start_barge(true, None, "讲故事", slow);
    let mut device = chat_device(&server, &registry, 1);
    begin(&mut device, "realtime");
    assert_eq!(read_json(&mut device)["type"], "stt");
    send_audio(&mut device, 1, &uplink(&tone(UP, 500.0, 480, 8000.0)));
    assert_eq!(texts(&collect_items(&mut device)), ["tts:stop"]);
    send_audio(&mut device, 1, &uplink(&silence(UP, 500)));
    assert!(texts(&collect_items(&mut device)).contains(&"听:换话题".into()));
    assert!(interrupted(&server));
    let seen = llm.seen.lock().unwrap();
    assert_eq!(
        seen[1]
            .0
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count(),
        0
    );
}

/// 回答播到一半用户应了一声“嗯嗯”：先暂停，识别出只是附和就接着把话说完，不打断、不进历史。
#[test]
fn backchannel_during_playback_pauses_then_resumes_the_answer() {
    let (server, registry, asr, llm) =
        start_barge(true, None, "讲故事", say(&["第一句。", "后面的第二句。"]));
    asr.replies.lock().unwrap()[1] = Ok("嗯嗯。".into());
    let mut device = chat_device(&server, &registry, 1);
    begin(&mut device, "realtime");
    first_frame(&mut device);
    std::thread::sleep(Duration::from_millis(350));
    let mut hum = tone(UP, 500.0, 480, 8000.0);
    hum.extend(silence(UP, 500));
    send_audio(&mut device, 1, &uplink(&hum));
    let rest = texts(&collect_items(&mut device));
    assert!(
        rest.contains(&"句:后面的第二句。".into()),
        "附和之后接着说完：{rest:?}"
    );
    assert!(!rest.iter().any(|text| text.starts_with("听:")), "{rest:?}");
    assert_eq!(asr.heard.lock().unwrap().len(), 2, "插话照常送去识别");
    assert_eq!(llm.seen.lock().unwrap().len(), 1, "附和不交给大模型");
    assert!(!interrupted(&server));
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("只是附和，接着说"))
    );
}

#[test]
fn invalid_barge_duration_is_rejected_before_listening() {
    for ms in [0, 299, 601, u32::MAX] {
        let config = ServerConfig {
            barge_in_ms: ms,
            ..Default::default()
        };
        assert!(
            VoiceServer::start(config, Arc::new(FakeRegistry::default()))
                .err()
                .unwrap()
                .contains("插话时长")
        );
    }
}

/// 播报暂停等待自然插话结果时若识别报错：记录警告并继续原回答，绝不能播报错话术把好好的原回答杀掉。
#[test]
fn barge_asr_error_during_pause_resumes_paused_answer() {
    let (server, registry, asr, llm) =
        start_barge(true, None, "讲故事", say(&["第一句。", "后面的第二句。"]));
    asr.replies.lock().unwrap()[1] = Err(ProviderError::new("asr boom"));
    let mut device = chat_device(&server, &registry, 1);
    begin(&mut device, "realtime");
    first_frame(&mut device);
    std::thread::sleep(Duration::from_millis(350));
    let mut barge = tone(UP, 500.0, 480, 8000.0);
    barge.extend(silence(UP, 500));
    send_audio(&mut device, 1, &uplink(&barge));
    let rest = texts(&collect_items(&mut device));
    assert!(
        rest.contains(&"句:后面的第二句。".into()),
        "插话识别出错后应继续原回答：{rest:?}"
    );
    assert!(
        !rest.iter().any(|text| text.starts_with("听:")),
        "识别失败不应误当作真插话：{rest:?}"
    );
    assert_eq!(asr.heard.lock().unwrap().len(), 2);
    assert_eq!(llm.seen.lock().unwrap().len(), 1);
    assert!(!interrupted(&server));
    let events = server.status().events;
    assert!(
        events
            .iter()
            .any(|event| event.text.contains("插话识别出错，继续原回答")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.text.contains("识别失败，接着说")),
        "{events:?}"
    );
}

/// 暂停播报并处于插话拾音期间收到 abort：立即停播并将残留 Listening 清回 Idle，后续静音不再误触发新轮次。
#[test]
fn abort_during_paused_barge_listening_clears_buffer_and_stops_tts() {
    let (server, registry, asr, llm) =
        start_barge(true, None, "讲故事", say(&["第一句。", "未播出的第二句。"]));
    let mut device = chat_device(&server, &registry, 1);
    begin(&mut device, "realtime");
    first_frame(&mut device);
    std::thread::sleep(Duration::from_millis(350));
    send_audio(&mut device, 1, &uplink(&tone(UP, 500.0, 480, 8000.0)));
    send_json(
        &mut device,
        r#"{"type":"abort","reason":"wake_word_detected"}"#,
    );
    let items = texts(&collect_items(&mut device));
    assert!(items.contains(&"tts:stop".into()), "{items:?}");
    assert!(
        !items.iter().any(|text| text.contains("未播出的第二句")),
        "{items:?}"
    );
    // 此前积压在 paused-listening 里的 480ms 人声已被清掉，之后发静音不会误提交第二轮识别
    send_audio(&mut device, 1, &uplink(&silence(UP, 600)));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(asr.heard.lock().unwrap().len(), 1, "残留录音不应再送识别");
    assert_eq!(llm.seen.lock().unwrap().len(), 1, "残留录音不应触发大模型");
}

/// 实时模式开启自然插话且由静音判停进入思考时：短续说（160–400ms）照常合并重认，不漏工具闸门，也不多发 tts stop。
#[test]
fn realtime_barge_thinking_continuation_merges_short_follow_up_and_gates_tools() {
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("帮我把功率调到".into()),
            Ok("帮我把功率调到八十".into()),
        ])),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([call("get_time"), say(&["调好了。"])])),
        ..Default::default()
    });
    let tools = Arc::new(FakeTools::default());
    let conversation = Conversation::new(
        asr.clone(),
        llm.clone(),
        Arc::new(LongTts),
        tools.clone(),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            barge_in: true,
            barge_in_ms: 400,
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"realtime"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "帮我把功率调到");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        tools.calls.lock().unwrap().is_empty(),
        "抢跑闸门未放行不应调工具"
    );

    // 240ms 短续说（大于 follow 160ms，小于 barge_in_ms 400ms），过去在 realtime+barge_in 下会被丢弃或退化
    let mut second = tone(UP, 300.0, 240, 8000.0);
    second.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&second));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:帮我把功率调到八十",
            "tts:start",
            "句:调好了。",
            "tts:stop"
        ]
    );
    let heard = asr.heard.lock().unwrap().clone();
    assert_eq!(heard.len(), 2);
    assert!(heard[1] > heard[0], "合并前后两段重新识别：{heard:?}");
    assert!(
        tools.calls.lock().unwrap().is_empty(),
        "被放弃轮的工具未执行"
    );
    let seen = llm.seen.lock().unwrap();
    let users: Vec<_> = seen
        .last()
        .unwrap()
        .0
        .iter()
        .filter(|message| message.role == Role::User)
        .filter_map(|message| message.content.clone())
        .collect();
    assert_eq!(users, vec!["帮我把功率调到八十"], "前半句不单独进历史");
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.starts_with("思考中用户接着说"))
    );
}

/// 回答播到一半被插话：先暂停，此时还在生成的这一轮后面才要发起的工具调用必须停住——
/// 插话确认是真打断就一次都不执行；只是附和就接着说完并照常执行一次。
#[test]
fn paused_answer_cannot_start_tools_until_the_barge_in_is_decided() {
    for (barge_text, expected_calls) in [("换话题", 0usize), ("嗯嗯。", 1)] {
        let asr = Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("讲故事".into()), Ok(barge_text.into())])),
            ..Default::default()
        });
        let llm = Arc::new(FakeLlm {
            rounds: Mutex::new(VecDeque::from([
                // 三句话慢慢吐出，最后才要调工具：暂停发生在工具调用之前
                Round::Reply {
                    deltas: vec!["第一句。", "第二句。", "第三句。"],
                    calls: vec![ToolCall::new("call-get_time", "get_time", "{}")],
                    delay_ms: 700,
                },
                say(&["好。"]),
                say(&["好。"]),
            ])),
            ..Default::default()
        });
        let tools = Arc::new(FakeTools::default());
        let conversation = Conversation::new(
            asr.clone(),
            llm.clone(),
            Arc::new(LongTts),
            tools.clone(),
            ConversationSettings::default(),
        );
        let registry = Arc::new(FakeRegistry::default());
        let server = VoiceServer::start(
            ServerConfig {
                bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
                ws_port: 0,
                http_port: 0,
                mode: ServerMode::Chat,
                conversation: Some(Arc::new(conversation)),
                barge_in: true,
                barge_in_ms: 400,
                ..Default::default()
            },
            registry.clone(),
        )
        .unwrap();
        let mut device = chat_device(&server, &registry, 1);
        begin(&mut device, "realtime");
        first_frame(&mut device);
        std::thread::sleep(Duration::from_millis(350));
        // 插话说得长一些：约 0.4 秒时已暂停，约 1.6 秒后才判停识别——第一轮的工具调用（约 2.1 秒）
        // 恰好落在“已暂停、还没决定”的窗口里
        let mut barge = tone(UP, 500.0, 1200, 8000.0);
        barge.extend(silence(UP, 500));
        // 按真实时间逐帧上传（一次发完会让暂停与判定只隔几毫秒，测不到中间的窗口）
        for frame in uplink(&barge) {
            send_audio(&mut device, 1, std::slice::from_ref(&frame));
            std::thread::sleep(Duration::from_millis(60));
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        while asr.heard.lock().unwrap().len() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(3000));
        assert_eq!(
            tools.calls.lock().unwrap().len(),
            expected_calls,
            "插话“{barge_text}”之后的工具执行次数"
        );
        drop(server);
    }
}
