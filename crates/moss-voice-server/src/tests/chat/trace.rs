//! n105 完整过程记录：开关打开时一轮对话依次写下录音、识别、每轮大模型请求与回复、工具完整结果、
//! 轮次结束（含合成语音 WAV）与耗时；开关关着时什么都不写。

use super::*;

fn trace_lines(dir: &std::path::Path) -> Vec<Value> {
    let Ok(days) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for day in days.flatten() {
        if let Ok(text) = std::fs::read_to_string(
            day.path()
                .join(laser_runtime::process_trace::TIMELINE_JSONL),
        ) {
            lines.extend(
                text.lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok()),
            );
        }
    }
    lines
}

fn start_traced(runtime: &std::path::Path) -> (VoiceServer, Arc<FakeRegistry>) {
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("帮我查一下时间".into())])),
        heard: Mutex::default(),
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([call("get_time"), say(&["现在十二点。"])])),
        repeat: None,
        seen: Mutex::default(),
    });
    let trace = laser_runtime::ProcessTrace::new(runtime);
    let mut conversation = Conversation::new(
        asr,
        llm,
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
        ConversationSettings {
            read_only_shortcuts: false,
            ..ConversationSettings::default()
        },
    );
    conversation.process_trace = Some(trace.clone());
    let diagnostics = crate::DiagnosticsLog::new();
    diagnostics.set_process_trace(trace);
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            diagnostics,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry)
}

fn set_switch(runtime: &std::path::Path, on: bool) {
    laser_runtime::settings_store::update(
        &laser_runtime::settings_store::settings_path(runtime),
        |map| {
            map.insert(
                laser_runtime::process_trace::SETTING_KEY.into(),
                Value::Bool(on),
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn process_trace_records_full_turn_with_audio_when_enabled() {
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let trace_dir = runtime.path().join(laser_runtime::process_trace::TRACE_DIR);
    let (server, registry) = start_traced(runtime.path());
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let items = collect_items(&mut device);
    assert!(texts(&items).contains(&"句:现在十二点。".to_owned()));

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut lines = trace_lines(&trace_dir);
    while !lines.iter().any(|l| l["kind"] == "timing") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        lines = trace_lines(&trace_dir);
    }
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["kind"].as_str()).collect();
    let of = |kind: &str| -> Vec<&Value> { lines.iter().filter(|l| l["kind"] == kind).collect() };

    // 用户录音（上行采样率 WAV）
    let audio = of("user_audio");
    assert_eq!(audio.len(), 1, "{kinds:?}");
    assert_eq!(audio[0]["sample_rate"], UP);
    assert_eq!(audio[0]["turn"], 1);
    let user_wav = trace_dir.join(audio[0]["file"].as_str().unwrap());
    assert!(std::fs::read(&user_wav).unwrap().starts_with(b"RIFF"));

    // 识别原文
    assert_eq!(of("asr")[0]["raw"], "帮我查一下时间");

    // 两轮大模型：请求发出时先记完整消息与工具定义，回复一完整就记回复与纯大模型耗时（n110）
    let requests = of("llm_request");
    let rounds = of("llm_round");
    assert_eq!(requests.len(), 2, "{kinds:?}");
    assert_eq!(rounds.len(), 2, "{kinds:?}");
    assert!(
        requests[0]["messages"]
            .to_string()
            .contains("帮我查一下时间")
    );
    assert!(requests[0]["tools"].to_string().contains("get_time"));
    assert_eq!(requests[0]["round"], 1);
    assert_eq!(requests[1]["round"], 2);
    assert!(requests[1]["messages"].to_string().contains("12:00"));
    let first = rounds[0];
    assert_eq!(first["reply_tool_calls"][0]["function"]["name"], "get_time");
    assert_eq!(rounds[1]["reply_text"], "现在十二点。");
    // 逐句合成：开始合成、合成出音频都有记录
    let tts_start = of("tts_start");
    assert_eq!(tts_start[0]["text"], "现在十二点。");
    assert!(!of("tts_ready").is_empty(), "{kinds:?}");

    // 工具完整结果
    let tool = of("tool_call");
    assert_eq!(tool[0]["name"], "get_time");
    assert_eq!(tool[0]["result"], "12:00");

    // 轮次结束：播出的句子、合成语音 WAV、耗时
    let end = of("turn_end");
    assert_eq!(end.len(), 1);
    assert_eq!(end[0]["spoken"][0], "现在十二点。");
    assert_eq!(end[0]["device_id"], audio[0]["device_id"]);
    let tts_wav = trace_dir.join(end[0]["tts_file"].as_str().unwrap());
    assert!(
        std::fs::read(&tts_wav).unwrap().len() > 44,
        "合成语音不是空文件"
    );
    assert!(!of("timing").is_empty(), "耗时也写进记录：{kinds:?}");

    // 时间线：按发生先后编号，每一步都有一句中文说明，属于同一台设备的第 1 轮
    let seq_of = |kind: &str, nth: usize| -> u64 { of(kind)[nth]["seq"].as_u64().unwrap() };
    let order = [
        seq_of("listen_start", 0),
        seq_of("listen_stop", 0),
        seq_of("user_audio", 0),
        seq_of("asr", 0),
        seq_of("llm_request", 0),
        seq_of("llm_round", 0),
        seq_of("tool_call", 0),
        seq_of("llm_request", 1),
        seq_of("llm_round", 1),
        seq_of("tts_start", 0),
        seq_of("tts_ready", 0),
        seq_of("sentence_play", 0),
        seq_of("turn_end", 0),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} {kinds:?}");
    assert_eq!(rounds[0]["round"], 1);
    assert_eq!(rounds[1]["round"], 2);
    assert!(
        audio[0]["flow"].as_str().unwrap().ends_with("·第1轮"),
        "用户录音步骤须归入第1轮：{}",
        audio[0]
    );
    assert!(end[0]["flow"].as_str().unwrap().ends_with("·第1轮"));
    let step = |l: &Value| l["step"].as_str().unwrap().to_owned();
    assert!(
        step(audio[0]).starts_with("用户说完一句话（"),
        "{}",
        step(audio[0])
    );
    assert_eq!(step(of("asr")[0]), "识别结果：「帮我查一下时间」");
    assert!(step(rounds[0]).contains("要求调用工具 get_time"));
    assert!(step(rounds[1]).contains("回复「现在十二点。」"));
    assert!(step(tool[0]).contains("结果「12:00」"));
    assert_eq!(tool[0]["description"], "查询时间", "工具说明随调用记下");
    assert_eq!(
        step(of("sentence_play")[0]),
        "开始播放第1句：「现在十二点。」"
    );
    assert!(step(end[0]).starts_with("本轮结束（正常播完）：播出「现在十二点。」，合成语音："));
    let day = std::fs::read_dir(&trace_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let readable =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_TXT)).unwrap();
    for text in [
        "设备开始拾音",
        "用户说完一句话",
        "向大模型发出第1轮请求",
        "大模型第1轮回复完",
        "大模型第2轮回复完",
        "工具 get_time（查询时间）执行完",
        "开始合成第1句",
        "本轮各阶段耗时",
        "识别出最终文字",
        "本轮结束（正常播完）",
    ] {
        assert!(
            readable.contains(text),
            "可读时间线缺少「{text}」：\n{readable}"
        );
    }
    drop(server);
}

#[test]
fn process_trace_writes_nothing_when_switch_is_off() {
    let runtime = tempfile::tempdir().unwrap();
    let trace_dir = runtime.path().join(laser_runtime::process_trace::TRACE_DIR);
    let (server, registry) = start_traced(runtime.path());
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let items = collect_items(&mut device);
    assert!(texts(&items).contains(&"句:现在十二点。".to_owned()));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!trace_dir.exists(), "开关关着时不创建记录目录");
    drop(server);
}

fn traced_conversation(
    runtime: &std::path::Path,
    asr: &[&str],
    llm: FakeLlm,
    settings: ConversationSettings,
) -> (Conversation, laser_runtime::ProcessTrace) {
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(asr.iter().map(|text| Ok((*text).to_owned())).collect()),
        heard: Mutex::default(),
    });
    let trace = laser_runtime::ProcessTrace::new(runtime);
    let mut conversation = Conversation::new(
        asr,
        Arc::new(llm),
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
        settings,
    );
    conversation.process_trace = Some(trace.clone());
    (conversation, trace)
}

fn start_traced_server(
    conversation: Conversation,
    trace: laser_runtime::ProcessTrace,
    vad_factory: Option<Arc<dyn crate::audio::VadFactory>>,
) -> (VoiceServer, Arc<FakeRegistry>) {
    let diagnostics = crate::DiagnosticsLog::new();
    diagnostics.set_process_trace(trace);
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            diagnostics,
            vad_factory,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry)
}

/// 等时间线里出现某一步（最多 3 秒）。
fn wait_for_kind(trace_dir: &std::path::Path, kind: &str) -> Vec<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let lines = trace_lines(trace_dir);
        if lines.iter().any(|l| l["kind"] == kind) || std::time::Instant::now() > deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 判停用能量检测，另外像本地模型那样报出自己的名字、这一路的降噪情况，并在开口时“听到”退出命令词。
struct NamedVad {
    inner: crate::audio::EnergyVad,
    keyword_sent: bool,
    denoise: Option<moss_voice_providers::local::DenoiseReport>,
}

impl crate::audio::Vad for NamedVad {
    fn accept(&mut self, pcm: &[i16]) -> Vec<crate::audio::VadEvent> {
        let mut events = self.inner.accept(pcm);
        if !self.keyword_sent && events.contains(&crate::audio::VadEvent::SpeechStart) {
            self.keyword_sent = true;
            events.push(crate::audio::VadEvent::Keyword(
                moss_voice_providers::local::CommandKeyword::Exit,
            ));
        }
        events
    }
    fn in_speech(&self) -> bool {
        self.inner.in_speech()
    }
    fn reset(&mut self) {
        self.inner.reset();
        self.keyword_sent = false;
        self.denoise = Some(moss_voice_providers::local::DenoiseReport {
            applied: true,
            model: "gtcrn_simple.onnx".into(),
            engine: "gtcrn".into(),
            error: None,
        });
    }
    fn engine(&self) -> String {
        "测试判停模型".into()
    }
    fn take_denoise(&mut self) -> Option<moss_voice_providers::local::DenoiseReport> {
        self.denoise.take()
    }
}

struct NamedVadFactory;

impl crate::audio::VadFactory for NamedVadFactory {
    fn create(
        &self,
        rate: u32,
        config: &crate::audio::VadConfig,
    ) -> Option<Box<dyn crate::audio::Vad>> {
        Some(Box::new(NamedVad {
            inner: crate::audio::EnergyVad::new(rate, config.clone()),
            keyword_sent: false,
            denoise: Some(moss_voice_providers::local::DenoiseReport {
                applied: true,
                model: "gtcrn_simple.onnx".into(),
                engine: "gtcrn".into(),
                error: None,
            }),
        }))
    }
}

/// 判停模型建不起来（例如上行不是 16 kHz）：会话改用能量判停，并把原因记下来。
struct BrokenVadFactory;

impl crate::audio::VadFactory for BrokenVadFactory {
    fn create(&self, _: u32, _: &crate::audio::VadConfig) -> Option<Box<dyn crate::audio::Vad>> {
        None
    }
    fn label(&self) -> String {
        "本地判停 测试模型".into()
    }
}

/// n110：自动拾音时记下判停方式、这一路的降噪情况、谁判定的开口与说完、命令词命中，
/// 识别结果里原文 → 同音纠正 → 命令词改判分开写，抢跑轮次记下开口前要不要多等及依据。
#[test]
fn process_trace_records_vad_denoise_keyword_and_asr_steps_separately() {
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let trace_dir = runtime.path().join(laser_runtime::process_trace::TRACE_DIR);
    let (conversation, trace) = traced_conversation(
        runtime.path(),
        &["问一夏"],
        FakeLlm::default(),
        ConversationSettings {
            command_keywords: true,
            asr_corrections: vec![("问一夏".into(), "问一下".into())],
            ..ConversationSettings::default()
        },
    );
    let (server, registry) =
        start_traced_server(conversation, trace, Some(Arc::new(NamedVadFactory)));
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&pcm));
    let goodbye = ConversationSettings::default().goodbye;
    assert!(texts(&collect_items(&mut device)).contains(&format!("句:{goodbye}")));
    let lines = wait_for_kind(&trace_dir, "turn_end");
    let of = |kind: &str| -> Vec<&Value> { lines.iter().filter(|l| l["kind"] == kind).collect() };
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["kind"].as_str()).collect();

    assert_eq!(of("vad_engine")[0]["engine"], "测试判停模型", "{kinds:?}");
    let denoise = of("denoise");
    assert_eq!(
        denoise.len(),
        1,
        "首轮自动拾音前 worker.reset() 不重复打开或双记降噪：{kinds:?}"
    );
    assert_eq!(denoise[0]["stage"], "判停", "{kinds:?}");
    assert_eq!(denoise[0]["applied"], true);
    assert!(
        denoise[0]["step"]
            .as_str()
            .unwrap()
            .contains("先经 GTCRN 降噪")
    );
    assert_eq!(of("speech_began")[0]["engine"], "测试判停模型", "{kinds:?}");
    assert_eq!(of("speech_end")[0]["engine"], "测试判停模型", "{kinds:?}");
    let hit = of("kws_hit");
    assert_eq!(hit[0]["keyword"], "退出", "{kinds:?}");
    assert!(hit[0]["flow"].as_str().unwrap().starts_with("语音·"));
    let asr = of("asr");
    assert_eq!(asr[0]["raw"], "问一夏");
    assert_eq!(asr[0]["homophone"], "问一下", "同音纠正单独记");
    assert_eq!(asr[0]["corrected"], "退下", "命令词改判单独记");
    assert_eq!(asr[0]["kws_fused"], true);
    assert_eq!(
        asr[0]["step"],
        "识别结果：「问一夏」，同音纠正为「问一下」，命令词模型听到退出命令、改判为「退下」"
    );
    assert!(
        !of("hold_decision").is_empty(),
        "抢跑轮次记下开口前等待：{kinds:?}"
    );
    // 发生先后：判停方式 → 开口 → 说完 → 用户录音 → 识别
    let seq = |kind: &str| of(kind)[0]["seq"].as_u64().unwrap();
    let order = [
        seq("vad_engine"),
        seq("speech_began"),
        seq("speech_end"),
        seq("user_audio"),
        seq("asr"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} {kinds:?}");
    drop(server);
}

/// 判停模型建不起来：记下原因与改用的能量判停，开口也写明是能量判停判的。
#[test]
fn process_trace_records_vad_fallback_reason() {
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let trace_dir = runtime.path().join(laser_runtime::process_trace::TRACE_DIR);
    let (conversation, trace) = traced_conversation(
        runtime.path(),
        &["你好"],
        FakeLlm::default(),
        ConversationSettings::default(),
    );
    let (server, registry) =
        start_traced_server(conversation, trace, Some(Arc::new(BrokenVadFactory)));
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&pcm));
    collect_items(&mut device);
    let lines = wait_for_kind(&trace_dir, "turn_end");
    let engine = lines
        .iter()
        .find(|l| l["kind"] == "vad_engine")
        .expect("判停方式");
    assert_eq!(engine["engine"], crate::audio::ENERGY_VAD_LABEL);
    assert_eq!(engine["fallback_reason"], "本地判停 测试模型没建起来");
    assert!(
        engine["step"]
            .as_str()
            .unwrap()
            .starts_with("本地判停模型没能用上（本地判停 测试模型没建起来）")
    );
    let began = lines.iter().find(|l| l["kind"] == "speech_began").unwrap();
    assert_eq!(began["engine"], crate::audio::ENERGY_VAD_LABEL);
    drop(server);
}

/// 边说边识别的每个中间文字都记一步（手动模式按下即开流式识别）。
#[test]
fn process_trace_records_each_interim_text() {
    struct InterimAsr;
    impl StreamingAsr for InterimAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let (interim_tx, interim_rx) = tokio::sync::watch::channel(String::new());
            // 像本地识别那样，打开流时报告这一路的降噪情况
            let slot = moss_voice_providers::DenoiseSlot::default();
            *slot.lock().unwrap() = Some(moss_voice_providers::local::DenoiseReport {
                applied: true,
                model: "gtcrn_simple.onnx".into(),
                engine: "gtcrn".into(),
                error: None,
            });
            let handle = tokio::spawn(async move {
                let mut chunks = 0;
                while rx.recv().await.is_some() {
                    chunks += 1;
                    if chunks == 1 {
                        interim_tx.send_replace("帮我".into());
                    } else if chunks == 3 {
                        // 空的中间结果不记
                        interim_tx.send_replace(String::new());
                    } else if chunks == 5 {
                        interim_tx.send_replace("帮我刻字".into());
                    }
                }
                let _ = done_tx.send(Ok("帮我刻字".into()));
            });
            AsrStream::new(tx, done_rx, handle)
                .with_interim(interim_rx)
                .with_denoise(slot)
        }
    }
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let trace_dir = runtime.path().join(laser_runtime::process_trace::TRACE_DIR);
    let (conversation, trace) = traced_conversation(
        runtime.path(),
        &[],
        FakeLlm::default(),
        ConversationSettings::default(),
    );
    let conversation = conversation.with_streaming_asr(Arc::new(InterimAsr));
    let (server, registry) = start_traced_server(conversation, trace, None);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    for _ in 0..8 {
        send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 60, 8000.0)));
        std::thread::sleep(Duration::from_millis(30));
    }
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    collect_items(&mut device);
    let lines = wait_for_kind(&trace_dir, "turn_end");
    let interims: Vec<&str> = lines
        .iter()
        .filter(|l| l["kind"] == "interim")
        .filter_map(|l| l["text"].as_str())
        .collect();
    assert_eq!(interims, vec!["帮我", "帮我刻字"], "{lines:#?}");
    // n110 复审 5③：识别流的降噪情况只记一次，在第一个中间结果时（排在中间文字与用户录音之前）
    let seq = |kind: &str| -> Vec<u64> {
        lines
            .iter()
            .filter(|l| l["kind"] == kind)
            .filter_map(|l| l["seq"].as_u64())
            .collect()
    };
    let denoise = seq("denoise");
    assert_eq!(denoise.len(), 1, "{lines:#?}");
    assert!(denoise[0] < seq("interim")[0]);
    assert!(denoise[0] < seq("user_audio")[0]);
    // 按键说话不跑判停，不应虚报能量判停；随后切自动拾音时写一次判停方式
    assert!(
        lines.iter().all(|l| l["kind"] != "vad_engine"),
        "按键说话期间不应写判停方式：{lines:#?}"
    );
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let lines = wait_for_kind(&trace_dir, "vad_engine");
    let engines: Vec<&Value> = lines.iter().filter(|l| l["kind"] == "vad_engine").collect();
    assert_eq!(engines.len(), 1, "{lines:#?}");
    assert_eq!(engines[0]["engine"], crate::audio::ENERGY_VAD_LABEL);
    drop(server);
}

/// 审查回归：① 抢跑等待（`turn.hold`）期间被打断，已发生的大模型请求与回复仍在发生当刻落盘、不随通道丢弃；
/// ② 轮次中途切换保存目录时，同轮的用户录音、大模型步骤、打断结束与合成 WAV 全留在旧目录；
/// ③ 连接建立后中途开启过程记录，下一次自动拾音补写本地判停方式；非抢跑轮次记下不用语义判停的原因。
#[test]
fn speculative_hold_barge_and_mid_turn_dir_change_preserve_real_time_traces() {
    struct Predictor;
    impl crate::conversation::TurnEndPredictor for Predictor {
        fn predict_turn_end<'a>(&'a self, _: &'a [i16], _: u32) -> BoxFuture<'a, Option<f32>> {
            // 返回低概率让会话进入最长 1.3s 的开口前等待（`turn.hold`）
            Box::pin(async { Some(0.05) })
        }
    }
    let runtime = tempfile::tempdir().unwrap();
    let next_parent = tempfile::tempdir().unwrap();
    // 先关着过程记录建连并跑一轮拾音，让本地判停线程在未开启记录时完成初始化上报
    set_switch(runtime.path(), false);
    let (conversation, trace) = traced_conversation(
        runtime.path(),
        &["先不记", "第一句没说完", "按键补一句"],
        FakeLlm {
            rounds: Mutex::new(VecDeque::from([
                say(&["这是抢跑备好的回答。"]),
                say(&["收到按键补句。"]),
            ])),
            ..FakeLlm::default()
        },
        ConversationSettings::default(),
    );
    let conversation = conversation.with_turn_predictor(Arc::new(Predictor));
    let (server, registry) =
        start_traced_server(conversation, trace.clone(), Some(Arc::new(NamedVadFactory)));
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut warm_pcm = tone(UP, 300.0, 600, 8000.0);
    warm_pcm.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&warm_pcm));
    collect_items(&mut device);

    // 连接中途打开过程记录，进入下一轮自动拾音（静音判停触发抢跑 + `hold` 等待）
    set_switch(runtime.path(), true);
    trace.invalidate();
    let old_dir = trace.dir();
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first_pcm = tone(UP, 300.0, 600, 8000.0);
    first_pcm.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first_pcm));

    // 在 `turn.hold` 尚未到点放行（设备尚未收到 TTS）期间，大模型请求与回复已经实时写入旧目录！
    let hold_lines = wait_for_kind(&old_dir, "tts_ready");
    assert!(
        hold_lines
            .iter()
            .any(|l| l["kind"] == "vad_engine" && l["engine"] == "测试判停模型"),
        "中途开启记录后下一次自动拾音补写本地判停模型：{hold_lines:#?}"
    );
    assert!(
        hold_lines.iter().any(|l| l["kind"] == "llm_request")
            && hold_lines.iter().any(|l| l["kind"] == "llm_round"),
        "抢跑等待期间大模型步骤不卡在 turn.hold 后面：{hold_lines:#?}"
    );

    // 轮次进行中途切换过程记录目录，随后立刻发 abort 打断处于 hold 中的这一轮
    trace.set_directory(Some(next_parent.path())).unwrap();
    let new_dir = trace.dir();
    assert_ne!(old_dir, new_dir);
    send_json(&mut device, r#"{"type":"abort","reason":"barge"}"#);
    let old_lines = wait_for_kind(&old_dir, "turn_end");
    let turn2_no = old_lines
        .iter()
        .find(|l| l["kind"] == "user_audio")
        .and_then(|l| l["turn"].as_u64())
        .unwrap();
    assert!(
        old_lines
            .iter()
            .filter(|l| l["turn"] == turn2_no)
            .any(|l| l["kind"] == "llm_round"),
        "被打断的抢跑轮次保留已发生的大模型记录"
    );
    assert!(
        trace_lines(&new_dir).iter().all(|l| l["turn"] != turn2_no),
        "切换前已开始的轮次不拆散到新目录"
    );

    // 新目录里发起按键说话轮次：非抢跑轮次如实记下不用语义判停的原因
    speak_manual(&mut device, 1, 600);
    collect_items(&mut device);
    let new_lines = wait_for_kind(&new_dir, "turn_end");
    let smart = new_lines
        .iter()
        .find(|l| l["kind"] == "smart_turn")
        .expect("非抢跑轮次也说明为何没跑语义判停");
    assert!(
        smart["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("不用语义判停"),
        "{smart}"
    );
    drop(server);
}

/// 对话任务这一层：收集一轮的全部事件。
async fn turn_events(
    conversation: Conversation,
    input: crate::conversation::TurnInput,
    gate: Option<crate::conversation::TurnGate>,
) -> Vec<crate::conversation::TurnEvent> {
    let (handle, mut rx) = crate::conversation::spawn_turn_gated(
        Arc::new(conversation),
        ToolContext {
            device_id: "dev".into(),
            session_id: "s".into(),
        },
        input,
        Vec::new(),
        None,
        None,
        gate,
    );
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    handle.await.unwrap();
    events
}

fn traces(events: &[crate::conversation::TurnEvent]) -> Vec<(&'static str, &Value)> {
    events
        .iter()
        .filter_map(|event| match event {
            crate::conversation::TurnEvent::Trace { kind, data } => Some((*kind, data)),
            _ => None,
        })
        .collect()
}

fn utterance(stream: Option<AsrStream>) -> crate::conversation::TurnInput {
    crate::conversation::TurnInput::Utterance {
        pcm: vec![0; 960],
        sample_rate: UP,
        stream,
    }
}

/// 快路径不经大模型直接调用的工具，与大模型发起的一样记下名字、参数、完整结果与耗时。
#[tokio::test]
async fn fast_route_tool_calls_are_traced_like_model_tool_calls() {
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let trace = laser_runtime::ProcessTrace::new(runtime.path());
    let mut conversation = Conversation::new(
        Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("当前进度".into())])),
            ..Default::default()
        }),
        Arc::new(FakeLlm::default()),
        Arc::new(FakeTts::default()),
        Arc::new(FastTools::default()),
        ConversationSettings::default(),
    );
    conversation.process_trace = Some(trace);
    let events = turn_events(conversation, utterance(None), None).await;
    let tools: Vec<&Value> = traces(&events)
        .into_iter()
        .filter(|(kind, _)| *kind == "tool_call")
        .map(|(_, data)| data)
        .collect();
    assert_eq!(tools.len(), 2, "recent + status");
    assert_eq!(tools[0]["name"], "laser_workflow_tool");
    assert_eq!(tools[0]["fast_route"], true);
    assert_eq!(tools[0]["arguments"], r#"{"action":"recent"}"#);
    assert_eq!(tools[0]["result"], "已连接");
    assert!(tools[0]["elapsed_ms"].as_u64().unwrap() >= 10);
    assert!(
        crate::session::voice_step_text("tool_call", tools[0]).starts_with(
            "快路径直接调用工具 laser_workflow_tool（加工任务：查询、生成、发送）执行完"
        )
    );
}

/// 被丢掉的识别结果（噪声词）记下原文与原因；开关关着时对话任务不发任何记录。
#[tokio::test]
async fn ignored_asr_results_are_traced_with_reason() {
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let (conversation, _trace) = traced_conversation(
        runtime.path(),
        &["呃"],
        FakeLlm::default(),
        Default::default(),
    );
    let events = turn_events(conversation, utterance(None), None).await;
    let traced = traces(&events);
    let ignored = traced
        .iter()
        .find(|(kind, _)| *kind == "asr_ignored")
        .expect("记下被忽略的识别结果");
    assert_eq!(ignored.1["raw"], "呃");
    assert_eq!(ignored.1["reason"], "只是语气词或噪声词");
    assert!(matches!(
        events.last(),
        Some(crate::conversation::TurnEvent::Ignored)
    ));

    let off = tempfile::tempdir().unwrap();
    let (conversation, _trace) = traced_conversation(
        off.path(),
        &["你好"],
        FakeLlm::default(),
        Default::default(),
    );
    let events = turn_events(conversation, utterance(None), None).await;
    assert!(traces(&events).is_empty(), "开关关着不发记录");
}

/// 语义判停的概率与耗时如实记下；大模型请求先于回复，回复带首字与完整耗时、在合成出音频之前；
/// 流式识别报告的降噪情况在识别结果之前记下。
#[tokio::test]
async fn smart_turn_llm_request_reply_and_denoise_are_traced_in_order() {
    use crate::conversation::{TurnEndPredictor, TurnEvent, TurnGate};
    struct Predictor;
    impl TurnEndPredictor for Predictor {
        fn predict_turn_end<'a>(&'a self, _: &'a [i16], _: u32) -> BoxFuture<'a, Option<f32>> {
            // 越界的概率：记录里要夹到 0~1 之间（n110 复审 8）
            Box::pin(async { Some(1.5) })
        }
    }
    struct DenoisedAsr;
    impl StreamingAsr for DenoisedAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let slot = moss_voice_providers::DenoiseSlot::default();
            *slot.lock().unwrap() = Some(moss_voice_providers::local::DenoiseReport {
                applied: false,
                model: "gtcrn_simple.onnx".into(),
                engine: "gtcrn".into(),
                error: Some("降噪器没建起来".into()),
            });
            let handle = tokio::spawn(async move {
                while rx.recv().await.is_some() {}
                // 识别要 200 ms：语义判停的耗时不能把它算进去
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = done_tx.send(Ok("今天几号".into()));
            });
            AsrStream::new(tx, done_rx, handle).with_denoise(slot)
        }
    }
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let (conversation, _trace) = traced_conversation(
        runtime.path(),
        &[],
        FakeLlm {
            rounds: Mutex::new(VecDeque::from([Round::Reply {
                deltas: vec!["今天", "十二号。"],
                calls: Vec::new(),
                delay_ms: 20,
            }])),
            ..FakeLlm::default()
        },
        Default::default(),
    );
    let conversation = conversation.with_turn_predictor(Arc::new(Predictor));
    let (_release_tx, release) = tokio::sync::watch::channel(true);
    let gate = TurnGate {
        release,
        tool_latch: Default::default(),
        speculative: true,
        hold_llm: false,
        fresh_history: Default::default(),
        allow_silent: false,
        allow_background: false,
        command_hint: Default::default(),
        command_hint_until: None,
    };
    let stream = DenoisedAsr.start();
    let events = turn_events(conversation, utterance(Some(stream)), Some(gate)).await;
    let index_of = |wanted: &str| {
        events
            .iter()
            .position(|event| matches!(event, TurnEvent::Trace { kind, .. } if *kind == wanted))
            .unwrap_or_else(|| panic!("缺少 {wanted}"))
    };
    let first_speech = events
        .iter()
        .position(|event| matches!(event, TurnEvent::Speech { .. }))
        .unwrap();
    let raw_heard = events
        .iter()
        .position(|event| matches!(event, TurnEvent::RawHeard { .. }))
        .unwrap();
    assert!(index_of("denoise") < raw_heard);
    assert!(index_of("smart_turn") < raw_heard);
    assert!(index_of("llm_request") < index_of("llm_round"));
    assert!(index_of("llm_round") < first_speech, "回复完就记，不等合成");
    assert!(index_of("tts_start") < index_of("tts_ready"));
    assert!(index_of("tts_ready") < first_speech);
    let traced = traces(&events);
    let get = |kind: &str| traced.iter().find(|(k, _)| *k == kind).unwrap().1;
    assert_eq!(
        get("smart_turn")["probability"].as_f64().unwrap(),
        1.0,
        "先夹到 0~1 再记"
    );
    assert!(
        get("smart_turn")["elapsed_ms"].as_u64().unwrap() < 150,
        "只算推理本身，不含识别：{}",
        get("smart_turn")
    );
    assert_eq!(get("denoise")["stage"], "识别");
    assert_eq!(get("denoise")["error"], "降噪器没建起来");
    assert_eq!(get("llm_request")["round"], 1);
    assert_eq!(get("llm_round")["round"], 1);
    assert_eq!(get("llm_round")["reply_text"], "今天十二号。");
    assert!(get("llm_round")["first_token_ms"].is_u64());
    assert_eq!(get("tts_start")["index"], 1);
}

/// n110 复审 5②：流式识别失败、改用整段识别时，不记识别流的降噪情况（最终文字不是这一路给的）。
#[tokio::test]
async fn failed_stream_does_not_claim_denoise() {
    struct FailingAsr;
    impl StreamingAsr for FailingAsr {
        fn start(&self) -> AsrStream {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let slot = moss_voice_providers::DenoiseSlot::default();
            *slot.lock().unwrap() = Some(moss_voice_providers::local::DenoiseReport {
                applied: true,
                model: "gtcrn_simple.onnx".into(),
                engine: "gtcrn".into(),
                error: None,
            });
            let handle = tokio::spawn(async move {
                while rx.recv().await.is_some() {}
                let _ = done_tx.send(Err(ProviderError::new("连接断了")));
            });
            AsrStream::new(tx, done_rx, handle).with_denoise(slot)
        }
    }
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let (conversation, _trace) = traced_conversation(
        runtime.path(),
        &["整段识别"],
        FakeLlm::default(),
        Default::default(),
    );
    let events = turn_events(conversation, utterance(Some(FailingAsr.start())), None).await;
    let traced = traces(&events);
    assert!(
        !traced.iter().any(|(kind, _)| *kind == "denoise"),
        "{:?}",
        traced.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
    assert!(
        traced.iter().any(|(kind, _)| *kind == "llm_request"),
        "整段识别后照常对话"
    );
}

/// n112 判停复盘：话说一半被判停、还没开口用户就接着说，过程记录写下当时的判停结论、是否合并和用户停顿长短，
/// 供 `scripts/voice-bench/endpoint-review.mjs` 统计抢话与白等。
#[test]
fn process_trace_records_user_continuing_after_an_endpoint_decision() {
    let runtime = tempfile::tempdir().unwrap();
    set_switch(runtime.path(), true);
    let trace_dir = runtime.path().join(laser_runtime::process_trace::TRACE_DIR);
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: vec!["只听到半句的回答。"],
                calls: Vec::new(),
                delay_ms: 3000,
            },
            say(&["完整的回答。"]),
        ])),
        ..FakeLlm::default()
    };
    let (conversation, trace) = traced_conversation(
        runtime.path(),
        &["帮我把功率", "帮我把功率调到八十"],
        llm,
        ConversationSettings::default(),
    );
    let (server, registry) = start_traced_server(conversation, trace, None);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&first));
    assert_eq!(read_json(&mut device)["text"], "帮我把功率");
    let mut second = tone(UP, 300.0, 600, 8000.0);
    second.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&second));
    assert!(texts(&collect_items(&mut device)).contains(&"句:完整的回答。".to_owned()));

    let lines = wait_for_kind(&trace_dir, "user_continued");
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["kind"].as_str()).collect();
    let decision = lines
        .iter()
        .find(|l| l["kind"] == "hold_decision")
        .unwrap_or_else(|| panic!("{kinds:?}"));
    let continued = lines
        .iter()
        .find(|l| l["kind"] == "user_continued")
        .unwrap();
    assert_eq!(
        continued["turn"], decision["turn"],
        "和判停结论记在同一轮，复盘脚本按轮次对上"
    );
    assert_eq!(continued["phase"], "thinking");
    assert_eq!(continued["merged"], true);
    assert_eq!(continued["too_late"], false);
    assert_eq!(continued["text"], "帮我把功率");
    assert_eq!(continued["text_wait_ms"], decision["text_wait_ms"]);
    // 句尾词表认不出“把功率”这种没说完的结构，按“拿不准”只多等 150 ms；测试里音频比实时快，
    // 接着说时是否已放行取决于时序，只核对两项一致
    assert_eq!(continued["text_wait_ms"], 400);
    let released = continued["released"].as_bool().unwrap();
    assert_eq!(continued["since_release_ms"].as_u64().is_some(), released);
    assert!(continued["since_playback_ms"].is_null(), "还没开播");
    // 停顿按真实时间估算；测试里音频比实时快，只核对开头里人声部分不超过整个开头
    assert!(continued["pause_estimate_ms"].as_u64().is_some());
    assert!(continued["voiced_head_ms"].as_u64() <= continued["head_ms"].as_u64());
    assert!(
        continued["step"]
            .as_str()
            .unwrap()
            .starts_with("判停复盘：还没开口用户就接着说，与前半句合并重新识别"),
        "{}",
        continued["step"]
    );
}
