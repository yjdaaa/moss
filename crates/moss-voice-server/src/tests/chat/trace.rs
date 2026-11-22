//! n105 完整过程记录：开关打开时一轮对话依次写下录音、识别、每轮大模型请求与回复、工具完整结果、
//! 轮次结束（含合成语音 WAV）与耗时；开关关着时什么都不写。

use super::*;

fn trace_lines(dir: &std::path::Path) -> Vec<Value> {
    let Ok(days) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for day in days.flatten() {
        if let Ok(text) = std::fs::read_to_string(day.path().join("voice.jsonl")) {
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
    while !lines.iter().any(|l| l["kind"] == "turn_end") && std::time::Instant::now() < deadline {
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

    // 两轮大模型：第一轮带完整消息与工具定义并要求调工具，第二轮带着工具结果回答
    let rounds = of("llm_round");
    assert_eq!(rounds.len(), 2, "{kinds:?}");
    let first = rounds[0];
    assert!(first["messages"].to_string().contains("帮我查一下时间"));
    assert!(first["tools"].to_string().contains("get_time"));
    assert_eq!(first["reply_tool_calls"][0]["function"]["name"], "get_time");
    assert!(rounds[1]["messages"].to_string().contains("12:00"));
    assert_eq!(rounds[1]["reply_text"], "现在十二点。");

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
