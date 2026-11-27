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
        // n100 起先让大模型判断是不是对小智说的，识别结果在作废原回答（tts stop）之前就已发给设备
        let later = texts(&collect_items(&mut device));
        assert!(
            previous
                .iter()
                .chain(&later)
                .any(|text| text == "听:换话题"),
            "{previous:?} {later:?}"
        );
        if !stream {
            assert!(
                asr.heard.lock().unwrap()[1] >= 16 * 480,
                "整段识别保留插话开头"
            );
        }
        let seen = llm.seen.lock().unwrap();
        let history = &seen[1].0;
        assert_eq!(history[1].content.as_deref(), Some("讲故事"));
        // 只记实际播出来的部分：这句 1.2 秒，插话时只播了一半；已合成但没播的第二句标为没说出口（n100）
        let reply = history[2].content.as_deref().unwrap();
        let (heard, unsaid) = reply.split_once("【被打断，没说出口的：").expect(reply);
        assert!(heard.starts_with("第一"), "{reply}");
        assert!(
            !heard.contains("第二句"),
            "模型生成已完成但未播内容不能当作已说：{reply}"
        );
        assert!(unsaid.contains("未播出的第二句。"), "{reply}");
        assert!(
            history[0]
                .content
                .as_deref()
                .unwrap()
                .contains("被打断约定"),
            "下一轮提示词说明标记的含义，话题回来时能接上"
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

/// 记下每次请求给了大模型哪些工具（检查“不是对我说的”选项只在自动 / 实时拾音时出现）。
struct NamesLlm {
    inner: FakeLlm,
    tools: Mutex<Vec<Vec<String>>>,
}

impl Llm for NamesLlm {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolSpec],
        deltas: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
        self.tools
            .lock()
            .unwrap()
            .push(tools.iter().map(|tool| tool.name.clone()).collect());
        self.inner.chat(messages, tools, deltas)
    }
}

fn start_addressee(
    barge_in: bool,
    heard: &[&str],
    rounds: Vec<Round>,
) -> (VoiceServer, Arc<FakeRegistry>, Arc<NamesLlm>) {
    let (server, registry, llm, _) = start_addressee_with_tools(barge_in, heard, rounds);
    (server, registry, llm)
}

fn start_addressee_with_tools(
    barge_in: bool,
    heard: &[&str],
    rounds: Vec<Round>,
) -> (
    VoiceServer,
    Arc<FakeRegistry>,
    Arc<NamesLlm>,
    Arc<FakeTools>,
) {
    let tools = Arc::new(FakeTools::default());
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(heard.iter().map(|text| Ok((*text).to_owned())).collect()),
        ..Default::default()
    });
    let llm = Arc::new(NamesLlm {
        inner: FakeLlm {
            rounds: Mutex::new(rounds.into()),
            ..Default::default()
        },
        tools: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
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
            barge_in,
            barge_in_ms: 400,
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry, llm, tools)
}

fn said_anywhere(messages: &[ChatMessage], needle: &str) -> bool {
    messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|text| text.contains(needle))
    })
}

/// n100：小智说着话，用户转头跟旁人说了一句——大模型判断不是对小智说的，被暂停的回答接着说完，
/// 这句话不进历史；原回答完整记入历史。
#[test]
fn speech_to_someone_else_while_speaking_lets_the_paused_answer_carry_on() {
    let (server, registry, llm) = start_addressee(
        true,
        &["讲故事", "老婆把螺丝刀递给我", "现在几点"],
        vec![
            say(&["第一句。", "第二句。"]),
            call("stay_silent"),
            say(&["三点。"]),
        ],
    );
    let mut device = chat_device(&server, &registry, 1);
    begin(&mut device, "realtime");
    first_frame(&mut device);
    std::thread::sleep(Duration::from_millis(350));
    let mut aside = tone(UP, 500.0, 480, 8000.0);
    aside.extend(silence(UP, 500));
    send_audio(&mut device, 1, &uplink(&aside));
    let items = texts(&collect_items(&mut device));
    assert!(
        items.contains(&"句:第二句。".into()),
        "原回答接着说完：{items:?}"
    );
    assert_eq!(
        items.iter().filter(|text| *text == "tts:stop").count(),
        1,
        "{items:?}"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text == "插话不是对小智说的，接着说")
    );
    assert!(llm.tools.lock().unwrap()[1].contains(&"stay_silent".into()));
    // 实时模式接着说下一句：历史里有完整的原回答，没有跟旁人说的那句
    let mut next = tone(UP, 300.0, 480, 8000.0);
    next.extend(silence(UP, 500));
    send_audio(&mut device, 1, &uplink(&next));
    collect_items(&mut device);
    let seen = llm.inner.seen.lock().unwrap();
    let history = &seen[2].0;
    assert!(!said_anywhere(history, "螺丝刀"), "{history:?}");
    assert!(
        history
            .iter()
            .any(|message| message.content.as_deref() == Some("第一句。第二句。")),
        "{history:?}"
    );
    assert!(!said_anywhere(history, "被打断"));
}

/// n100：自动拾音下小智说完后旁人说了一句：不回应（不发 tts start），也不进历史；之后对小智说的照常回答。
#[test]
fn speech_to_someone_else_after_an_answer_gets_no_reply() {
    let (server, registry, llm) = start_addressee(
        false,
        &["讲故事", "你先去吃饭吧", "现在几点"],
        vec![say(&["好的。"]), call("stay_silent"), say(&["三点。"])],
    );
    let mut device = chat_device(&server, &registry, 1);
    let listen = r#"{"type":"listen","state":"start","mode":"auto"}"#;
    let mut speech = tone(UP, 300.0, 600, 8000.0);
    speech.extend(silence(UP, 700));
    send_json(&mut device, listen);
    send_audio(&mut device, 1, &uplink(&speech));
    collect_items(&mut device);
    // 固件播完重新拾音：先是旁人的一句，再是对小智说的一句
    send_json(&mut device, listen);
    send_audio(&mut device, 1, &uplink(&speech));
    wait_until(
        "旁人话语已判定非对小智说并回到自动拾音",
        || {
            let status = server.status();
            status
                .events
                .iter()
                .any(|event| event.text.starts_with("这句不是对小智说的"))
                && status.sessions.iter().any(|s| s.state == "listening")
        },
    );
    send_audio(&mut device, 1, &uplink(&speech));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:你先去吃饭吧",
            "听:现在几点",
            "tts:start",
            "句:三点。",
            "tts:stop"
        ]
    );
    let seen = llm.inner.seen.lock().unwrap();
    assert!(!said_anywhere(&seen[2].0, "吃饭"));
    drop(seen);
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.starts_with("这句不是对小智说的"))
    );
}

/// 按键说话（manual）一定是对小智说的：不给“不是对我说的”选项。
#[test]
fn push_to_talk_never_offers_staying_silent() {
    let (server, registry, llm) = start_addressee(false, &["讲故事"], vec![say(&["好的。"])]);
    let mut device = chat_device(&server, &registry, 1);
    begin(&mut device, "manual");
    collect_items(&mut device);
    let tools = llm.tools.lock().unwrap();
    assert!(!tools[0].contains(&"stay_silent".into()), "{tools:?}");
    assert!(tools[0].contains(&"end_conversation".into()));
}

/// 审查修正：`stay_silent` 混在别的工具里（比如大模型同时调了急停）时不当真，别的工具照常执行。
#[test]
fn staying_silent_never_swallows_other_tools_in_the_same_batch() {
    let (server, registry, _, tools) = start_addressee_with_tools(
        false,
        &["快把激光关掉"],
        vec![
            Round::Reply {
                deltas: Vec::new(),
                calls: vec![
                    ToolCall::new("call-silent", "stay_silent", "{}"),
                    ToolCall::new("call-stop", "laser_safe_action_tool", "{}"),
                ],
                delay_ms: 0,
            },
            say(&["已经停了。"]),
        ],
    );
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut speech = tone(UP, 300.0, 600, 8000.0);
    speech.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&speech));
    let items = texts(&collect_items(&mut device));
    assert!(items.contains(&"句:已经停了。".into()), "{items:?}");
    let calls = tools.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|(name, _)| name == "laser_safe_action_tool"),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|(name, _)| name == "stay_silent"));
}

/// 审查修正：已经执行过工具后才调 `stay_silent`，当这一轮正常结束（不当成“不是对我说的”丢掉整轮）。
#[test]
fn staying_silent_after_a_tool_round_ends_the_turn_normally() {
    let (server, registry, llm, tools) = start_addressee_with_tools(
        false,
        &["现在几点", "好的谢谢"],
        vec![call("get_time"), call("stay_silent"), say(&["不客气。"])],
    );
    let mut device = chat_device(&server, &registry, 1);
    let listen = r#"{"type":"listen","state":"start","mode":"auto"}"#;
    let mut speech = tone(UP, 300.0, 600, 8000.0);
    speech.extend(silence(UP, 700));
    send_json(&mut device, listen);
    send_audio(&mut device, 1, &uplink(&speech));
    wait_until(
        "执行过工具的无声轮次正常收尾并回到自动拾音",
        || {
            let status = server.status();
            status.timings.iter().any(|t| t.outcome == "done")
                && status.sessions.iter().any(|s| s.state == "listening")
        },
    );
    assert!(
        !server
            .status()
            .events
            .iter()
            .any(|event| event.text.starts_with("这句不是对小智说的")),
        "执行过工具的轮次不能当成没听见"
    );
    send_audio(&mut device, 1, &uplink(&speech));
    collect_items(&mut device);
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    // 这一轮（问题 + 工具调用与结果）照常进了历史
    let seen = llm.inner.seen.lock().unwrap();
    assert!(said_anywhere(&seen[2].0, "现在几点"), "{:?}", seen[2].0);
}
