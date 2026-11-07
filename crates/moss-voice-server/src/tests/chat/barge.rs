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
        send_audio(&mut device, 1, &uplink(&tone(UP, 500.0, 480, 8000.0)));
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
        send_audio(&mut device, 1, &uplink(&silence(UP, 300)));
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
        next.extend(silence(UP, 300));
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
    send_audio(&mut device, 1, &uplink(&silence(UP, 300)));
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
