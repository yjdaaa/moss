//! 思考期间“用户接着说”与本地判停模型的配合（2026-11-15 整链实测发现）：
//! 用了本地判停模型时，能量检测听到的声响要等模型也认为是人声才放弃已备好的回答；
//! 模型不认的声响（气泵咔嗒、敲击）照常回答，也不会把会话卡在“等说完”。
use super::*;
use crate::audio::{Vad, VadConfig, VadEvent, VadFactory};
use crate::conversation::{STOP_ERR_SPEECH, STOP_OK_SPEECH};

/// 像 Silero 那样只认“有声调的语音”：以 20 ms 为一格，响度够且过零率低（浊音）才算人声；
/// 连续 160 ms 人声判开口，开口后连续 400 ms 非人声判说完。宽带噪声冲击过零率高，永远不算。
#[derive(Default)]
struct VoicedOnlyVad {
    pending: Vec<i16>,
    voiced_ms: u32,
    quiet_ms: u32,
    speaking: bool,
}

impl Vad for VoicedOnlyVad {
    fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent> {
        const CELL: usize = 320;
        self.pending.extend_from_slice(pcm);
        let mut events = Vec::new();
        while self.pending.len() >= CELL {
            let cell: Vec<i16> = self.pending.drain(..CELL).collect();
            let crossings = cell.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
            let voiced = rms(&cell) > 1000.0 && crossings < 40;
            if voiced {
                self.voiced_ms += 20;
                self.quiet_ms = 0;
                if !self.speaking && self.voiced_ms >= 160 {
                    self.speaking = true;
                    events.push(VadEvent::SpeechStart);
                }
            } else {
                self.voiced_ms = 0;
                self.quiet_ms += 20;
                if self.speaking && self.quiet_ms >= 400 {
                    self.speaking = false;
                    events.push(VadEvent::SpeechEnd);
                }
            }
        }
        events
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn in_speech(&self) -> bool {
        self.speaking
    }
}

struct VoicedOnlyFactory;

impl VadFactory for VoicedOnlyFactory {
    fn create(&self, _rate: u32, _config: &VadConfig) -> Option<Box<dyn Vad>> {
        Some(Box::new(VoicedOnlyVad::default()))
    }
}

/// 宽带噪声冲击（敲击、气泵咔嗒）：响度足以让能量检测认为“有人接着说”。
fn bang(ms: u32) -> Vec<i16> {
    let mut seed = 0x2545_f491_u32;
    (0..UP * ms / 1000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 16) as i16 / 3
        })
        .collect()
}

fn start_with_model_vad(
    llm: FakeLlm,
    asr: &[&str],
) -> (VoiceServer, Arc<FakeRegistry>, Arc<FakeAsr>) {
    start_with_model_vad_config(llm, asr, false)
}

fn start_with_model_vad_config(
    llm: FakeLlm,
    asr: &[&str],
    barge_in: bool,
) -> (VoiceServer, Arc<FakeRegistry>, Arc<FakeAsr>) {
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(asr.iter().map(|text| Ok((*text).to_owned())).collect()),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr.clone(),
        Arc::new(llm),
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
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
            vad_factory: Some(Arc::new(VoicedOnlyFactory)),
            barge_in,
            barge_in_ms: 400,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry, asr)
}

fn thinking_llm() -> FakeLlm {
    FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: vec!["第一轮的回答。"],
                calls: Vec::new(),
                delay_ms: 1500,
            },
            say(&["合并后的回答。"]),
        ])),
        ..FakeLlm::default()
    }
}

/// 思考中传来一阵敲击声：能量检测会当成“接着说”，判停模型不认——照常回答，不放弃、不重新识别。
#[test]
fn a_bang_while_thinking_does_not_throw_away_the_answer_when_the_model_vad_hears_no_speech() {
    let (server, registry, asr) =
        start_with_model_vad(thinking_llm(), &["帮我看看状态", "不该识别"]);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "帮我看看状态");
    // 设备按实时节奏上传：敲击声之后确认窗口到期
    send_audio(&mut device, 1, &uplink(&bang(300)));
    std::thread::sleep(Duration::from_millis(800));
    send_audio(&mut device, 1, &uplink(&silence(UP, 120)));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["tts:start", "句:第一轮的回答。", "tts:stop"]
    );
    // 那阵声音只单独识别一次看是不是“停”（不是），没有和前半句合并重新识别
    let heard = asr.heard.lock().unwrap().clone();
    assert_eq!(heard.len(), 2, "{heard:?}");
    assert!(heard[1] < UP as usize, "只送了那阵声音：{heard:?}");
    let events = server.status().events;
    assert!(
        !events
            .iter()
            .any(|event| event.text.starts_with("思考中用户接着说")),
        "{:?}",
        events.iter().map(|event| &event.text).collect::<Vec<_>>()
    );
}

/// 真的接着说（判停模型也听到人声）：仍然放弃还没开口的回答，前后半句合并成一段识别。
#[test]
fn speaking_on_while_thinking_still_merges_when_the_model_vad_confirms_speech() {
    let (server, registry, asr) =
        start_with_model_vad(thinking_llm(), &["帮我把功率", "帮我把功率调到八十"]);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "帮我把功率");
    let mut second = tone(UP, 300.0, 600, 8000.0);
    second.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&second));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:帮我把功率调到八十",
            "tts:start",
            "句:合并后的回答。",
            "tts:stop"
        ]
    );
    let heard = asr.heard.lock().unwrap().clone();
    assert_eq!(heard.len(), 2);
    assert!(
        heard[1] > heard[0] + UP as usize / 2,
        "前半句 + 后半句：{heard:?}"
    );
}

/// 审查修正：思考中短促的一声“停”短于判停模型判开口所需的最短语音、模型不认——仍识别一遍，
/// 是停止说法就作废准备好的回答、执行停止，不能让回答（和它的工具）照常进行。
#[test]
fn a_short_stop_while_thinking_is_not_lost_when_the_model_vad_misses_it() {
    let (server, registry, asr) = start_with_model_vad(thinking_llm(), &["帮我看看状态", "停"]);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "帮我看看状态");
    // 判停模型不认的一阵声音（这里用宽带冲击模拟）；设备按实时节奏继续上传，确认窗口到期后的下一帧触发识别
    send_audio(&mut device, 1, &uplink(&bang(300)));
    std::thread::sleep(Duration::from_millis(800));
    send_audio(&mut device, 1, &uplink(&silence(UP, 120)));
    let items = texts(&collect_items(&mut device));
    assert!(
        !items.contains(&"句:第一轮的回答。".into()),
        "准备好的回答不能照常播出：{items:?}"
    );
    assert_eq!(asr.heard.lock().unwrap().len(), 2, "那阵声音送去识别了");
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.starts_with("思考中听到“停”"))
    );
}

/// 实时模式下说话中一声敲击触发了插话暂停：补送开头后判停模型没听到人声，直接收尾识别
/// （识别不出内容就接着说），不会卡在“等说完”。
#[test]
fn a_bang_during_playback_resumes_the_answer_when_the_model_vad_hears_no_speech() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&[
            "第一句。",
            "第二句。",
            "第三句。",
            "第四句。",
        ])])),
        ..FakeLlm::default()
    };
    let (server, registry, _) = start_with_model_vad_config(llm, &["讲故事", ""], true);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"realtime"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first));
    while !matches!(device.read().unwrap(), Message::Binary(_)) {}
    std::thread::sleep(Duration::from_millis(350));
    send_audio(&mut device, 1, &uplink(&bang(500)));
    let items = texts(&collect_items(&mut device));
    assert!(
        items.contains(&"句:第四句。".into()),
        "回答接着说完：{items:?}"
    );
    let events: Vec<String> = server
        .status()
        .events
        .iter()
        .map(|event| event.text.clone())
        .collect();
    assert!(
        events
            .iter()
            .any(|text| text.starts_with("补送的开头里判停模型没听到")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|text| text.starts_with("插话没听出内容")),
        "{events:?}"
    );
}

/// 第二次起每次识别都慢 1.2 秒（模拟云端识别）：识别回来时准备好的回答已经开口了。
struct SlowLaterAsr {
    inner: Arc<FakeAsr>,
    calls: Mutex<usize>,
}

impl Asr for SlowLaterAsr {
    fn transcribe<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            let call = {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            if call > 1 {
                tokio::time::sleep(Duration::from_millis(1200)).await;
            }
            self.inner.transcribe(pcm, sample_rate).await
        })
    }
}

/// 增量审查修正：识别出“停”时回答已经开口（或已开始执行工具），照样打断、执行停止，不能因为错过了思考阶段就吞掉。
#[test]
fn a_late_recognized_stop_still_interrupts_an_answer_that_already_started() {
    let asr = Arc::new(SlowLaterAsr {
        inner: Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("讲个长故事".into()), Ok("停".into())])),
            heard: Mutex::default(),
        }),
        calls: Mutex::default(),
    });
    let llm = FakeLlm {
        // 每句前等 1.1 秒：第一句在确认窗口到期（约 0.8 秒）之后、“停”识别回来（约 2 秒）之前开口
        rounds: Mutex::new(VecDeque::from([Round::Reply {
            deltas: vec!["第一句。", "第二句。", "第三句。", "第四句。", "第五句。"],
            calls: Vec::new(),
            delay_ms: 1100,
        }])),
        ..FakeLlm::default()
    };
    let conversation = Conversation::new(
        asr,
        Arc::new(llm),
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
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
            vad_factory: Some(Arc::new(VoicedOnlyFactory)),
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
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "讲个长故事");
    // 思考中一声判停模型不认的短促声音；确认窗口到期后送去识别（要 1.2 秒），期间回答开口了
    send_audio(&mut device, 1, &uplink(&bang(300)));
    std::thread::sleep(Duration::from_millis(800));
    send_audio(&mut device, 1, &uplink(&silence(UP, 120)));
    let answer = texts(&collect_items(&mut device));
    assert!(
        answer.contains(&"tts:start".into()),
        "回答已经开口：{answer:?}"
    );
    assert!(
        !answer.contains(&"句:第五句。".into()),
        "回答被打断：{answer:?}"
    );
    let stop = texts(&collect_items(&mut device));
    assert!(
        stop.iter()
            .any(|text| text == &format!("句:{STOP_OK_SPEECH}")
                || text == &format!("句:{STOP_ERR_SPEECH}")),
        "执行了停止：{stop:?}"
    );
}

/// 像命令词模型那样：在宽带噪声冲击里“听到”指定的命令词（模拟噪声里识别听错、命令词模型认出来），
/// 判停仍按 [`VoicedOnlyVad`]（只认浊音）。
struct KeywordVad {
    inner: VoicedOnlyVad,
    keyword: moss_voice_providers::local::CommandKeyword,
    reported: bool,
    /// 听到后再过这么多样本才报（模拟真实命令词模型要在词后再听一小段才出结果）
    delay: usize,
    /// 听到后已经过去的样本数
    since: Option<usize>,
}

impl Vad for KeywordVad {
    fn accept(&mut self, pcm: &[i16]) -> Vec<VadEvent> {
        let mut events = self.inner.accept(pcm);
        let crossings = pcm.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
        if self.since.is_none() && rms(pcm) > 1000.0 && crossings * 320 > pcm.len() * 40 {
            self.since = Some(0);
        }
        if let Some(since) = self.since.as_mut() {
            if !self.reported && *since >= self.delay {
                self.reported = true;
                events.push(VadEvent::Keyword(self.keyword));
            }
            *since += pcm.len();
        }
        events
    }

    fn reset(&mut self) {
        self.inner.reset();
        self.reported = false;
        self.since = None;
    }

    fn in_speech(&self) -> bool {
        self.inner.in_speech()
    }
}

struct KeywordFactory(moss_voice_providers::local::CommandKeyword, usize);

impl VadFactory for KeywordFactory {
    fn create(&self, _rate: u32, _config: &VadConfig) -> Option<Box<dyn Vad>> {
        Some(Box::new(KeywordVad {
            inner: VoicedOnlyVad::default(),
            keyword: self.0,
            reported: false,
            delay: self.1,
            since: None,
        }))
    }
}

fn start_with_keyword_vad(
    keyword: moss_voice_providers::local::CommandKeyword,
    llm: FakeLlm,
    asr: &[&str],
) -> (VoiceServer, Arc<FakeRegistry>) {
    start_with_keyword_vad_delayed(keyword, 0, llm, asr)
}

fn start_with_keyword_vad_delayed(
    keyword: moss_voice_providers::local::CommandKeyword,
    delay: usize,
    llm: FakeLlm,
    asr: &[&str],
) -> (VoiceServer, Arc<FakeRegistry>) {
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(asr.iter().map(|text| Ok((*text).to_owned())).collect()),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        Arc::new(llm),
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
        ConversationSettings {
            command_keywords: true,
            ..ConversationSettings::default()
        },
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            vad_factory: Some(Arc::new(KeywordFactory(keyword, delay))),
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry)
}

/// 一段说话：浊音（判停认作人声）里夹一阵宽带噪声（命令词模型在这里“听到”命令词）。
fn command_in_noise() -> Vec<i16> {
    let mut pcm = tone(UP, 300.0, 400, 8000.0);
    pcm.extend(bang(200));
    pcm.extend(tone(UP, 300.0, 200, 8000.0));
    pcm.extend(silence(UP, 700));
    pcm
}

/// n101：噪声里识别把“退下”听成了“问一下”，命令词模型听到了退出词——按退出处理，道别后回到待命。
#[test]
fn a_misheard_exit_word_still_exits_when_the_keyword_model_heard_it() {
    use moss_voice_providers::local::CommandKeyword;
    let (server, registry) =
        start_with_keyword_vad(CommandKeyword::Exit, FakeLlm::default(), &["问一下"]);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&command_in_noise()));
    let goodbye = ConversationSettings::default().goodbye;
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:退下".to_owned(),
            "tts:start".into(),
            format!("句:{goodbye}"),
            "tts:stop".into()
        ]
    );
    expect_closed(&mut device);
}

/// 长句一律以识别为准：命令词模型听到了“退下”，但识别出的是一整句话，照常交给大模型。
#[test]
fn the_keyword_model_never_overrides_a_full_sentence() {
    use moss_voice_providers::local::CommandKeyword;
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["好的。"])])),
        ..FakeLlm::default()
    };
    let (server, registry) =
        start_with_keyword_vad(CommandKeyword::Exit, llm, &["帮我问一下今天天气怎么样"]);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&command_in_noise()));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:帮我问一下今天天气怎么样",
            "tts:start",
            "句:好的。",
            "tts:stop"
        ]
    );
}

/// 思考中的一阵声音里命令词模型听到了“停”：不用等识别，立即打断准备好的回答、执行停止。
#[test]
fn a_stop_word_heard_by_the_keyword_model_while_thinking_stops_at_once() {
    use moss_voice_providers::local::CommandKeyword;
    let (server, registry) =
        start_with_keyword_vad(CommandKeyword::Stop, thinking_llm(), &["帮我看看状态"]);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "帮我看看状态");
    send_audio(&mut device, 1, &uplink(&bang(300)));
    let items = texts(&collect_items(&mut device));
    assert!(!items.contains(&"句:第一轮的回答。".into()), "{items:?}");
    assert!(
        items
            .iter()
            .any(|text| text == &format!("句:{STOP_OK_SPEECH}")
                || text == &format!("句:{STOP_ERR_SPEECH}")),
        "{items:?}"
    );
}

/// 命令词纠正的条件：实测的听错说法都纠正，常见的短回答（“好的”“挺好”“嗯”）在命令词模型误报时也不改成命令。
#[test]
fn only_misheard_commands_are_corrected_by_the_keyword_hint() {
    use crate::conversation::command_hint_fits;
    use moss_voice_providers::local::CommandKeyword::{Exit, Stop};
    for misheard in [
        "跪下",
        "跪一下",
        "会一下",
        "不理下",
        "看一下",
        "问一下",
        "待见",
        "",
    ] {
        assert!(command_hint_fits(Exit, misheard), "{misheard}");
    }
    for misheard in ["天下", "听", "挺", ""] {
        assert!(command_hint_fits(Stop, misheard), "{misheard}");
    }
    for normal in ["好的", "挺好", "可以", "对对", "谢谢你", "好", "嗯", "对"] {
        assert!(!command_hint_fits(Stop, normal), "{normal}");
        assert!(!command_hint_fits(Exit, normal), "{normal}");
    }
    // 长句一律以识别为准
    assert!(!command_hint_fits(Exit, "帮我问一下今天天气"));
}

/// 命令词结果晚于“这段话说完”才到（真实命令词模型要在词后再听一小段）：说完后尾音接着送进同一路判停流，
/// 识别结果像被听错的命令时稍等它，照样按退出处理。
#[test]
fn a_keyword_that_arrives_after_the_utterance_ended_still_counts() {
    use moss_voice_providers::local::CommandKeyword;
    // 噪声后约 1.1 秒才报：已经过了判停说完（约 0.6 秒）
    let (server, registry) = start_with_keyword_vad_delayed(
        CommandKeyword::Exit,
        UP as usize * 11 / 10,
        FakeLlm::default(),
        &["问一下"],
    );
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&command_in_noise()));
    // 设备按实时节奏继续上传尾音
    for _ in 0..12 {
        std::thread::sleep(Duration::from_millis(60));
        send_audio(&mut device, 1, &uplink(&silence(UP, 60)));
    }
    let items = texts(&collect_items(&mut device));
    assert_eq!(items[0], "听:退下", "{items:?}");
    expect_closed(&mut device);
}
