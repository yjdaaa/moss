//! 对话模式（NV3）端到端测试：假的识别 / 大模型 / 合成 / 工具，tungstenite 扮演设备。

use std::collections::VecDeque;

use moss_voice_providers::{
    Asr, AsrStream, BoxFuture, ChatMessage, Llm, LlmReply, Pcm, ProviderError, Role, StreamingAsr, ToolCall, ToolSpec, Tts,
};

use super::*;
use crate::conversation::{Conversation, ConversationSettings, ToolContext, ToolExecutor, ToolOutcome};

mod barge;
mod music;
mod person;

#[derive(Default)]
struct FakeAsr {
    replies: Mutex<VecDeque<Result<String, ProviderError>>>,
    heard: Mutex<Vec<usize>>,
}

impl Asr for FakeAsr {
    fn transcribe<'a>(&'a self, pcm: &'a [i16], sample_rate: u32) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            assert_eq!(sample_rate, UP);
            self.heard.lock().unwrap().push(pcm.len());
            self.replies.lock().unwrap().pop_front().unwrap_or_else(|| Ok("你好".into()))
        })
    }
}

#[derive(Clone)]
enum Round {
    Reply { deltas: Vec<&'static str>, calls: Vec<ToolCall>, delay_ms: u64 },
    Fail(&'static str),
}

fn say(deltas: &[&'static str]) -> Round {
    Round::Reply { deltas: deltas.to_vec(), calls: Vec::new(), delay_ms: 0 }
}

fn call(name: &str) -> Round {
    Round::Reply { deltas: Vec::new(), calls: vec![ToolCall::new(format!("call-{name}"), name, "{}")], delay_ms: 0 }
}

#[derive(Default)]
struct FakeLlm {
    rounds: Mutex<VecDeque<Round>>,
    /// 脚本用完后一直返回这一轮
    repeat: Option<Round>,
    seen: Mutex<Vec<(Vec<ChatMessage>, usize)>>,
}

impl Llm for FakeLlm {
    fn chat<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [ToolSpec],
        deltas: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((messages.to_vec(), tools.len()));
            let round = self.rounds.lock().unwrap().pop_front().or_else(|| self.repeat.clone()).unwrap_or_else(|| say(&["好的。"]));
            match round {
                Round::Fail(message) => Err(ProviderError::new(message)),
                Round::Reply { deltas: parts, calls, delay_ms } => {
                    let mut text = String::new();
                    for part in parts {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        text.push_str(part);
                        let _ = deltas.send(part.to_owned());
                    }
                    Ok(LlmReply { text, tool_calls: calls })
                }
            }
        })
    }
}

/// 合成 16 kHz 音调（与下行 24 kHz 不同，覆盖重采样）；文字含“失败”时报错。
#[derive(Default)]
struct FakeTts {
    said: Mutex<Vec<String>>,
}

impl Tts for FakeTts {
    fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            self.said.lock().unwrap().push(text.to_owned());
            if text.contains("失败") {
                return Err(ProviderError::new("语音合成返回 HTTP 500"));
            }
            Ok(Pcm { samples: tone(16_000, 300.0, 300, 6000.0), sample_rate: 16_000 })
        })
    }
}

#[derive(Default)]
struct FakeTools {
    calls: Mutex<Vec<(String, String)>>,
}

#[derive(Default)]
struct FastTools { calls: Mutex<Vec<(String, Value, ToolContext)>>, fail: bool }
impl ToolExecutor for FastTools {
    fn tools(&self) -> Vec<ToolSpec> { vec![] }
    fn call<'a>(&'a self, _: &'a ToolContext, _: &'a str, _: &'a str) -> BoxFuture<'a, ToolOutcome> { Box::pin(async { panic!("fixed routes must use full results") }) }
    fn call_raw<'a>(&'a self, context: &'a ToolContext, name: &'a str, arguments: &'a str) -> BoxFuture<'a, crate::ToolHostResult> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments).unwrap();
            self.calls.lock().unwrap().push((name.into(), args.clone(), context.clone()));
            tokio::time::sleep(Duration::from_millis(15)).await;
            let body = match (name, args["action"].as_str()) {
                ("laser_workflow_tool", Some("recent")) => serde_json::json!({"workflows":[{"workflow_id":"wf-fast","status":"sending"}]}),
                ("laser_workflow_tool", Some("status")) => serde_json::json!({"workflow_id":"wf-fast","status":"sending","input":{"material":"椴木","thickness_mm":3},"send_result":{"lines_sent":40,"total_lines":100},"unused":"x".repeat(2000)}),
                ("material_params_tool", _) => serde_json::json!({"laser_max_power":400,"feed_rate":1200,"passes":2}),
                _ => Value::Null,
            };
            crate::ToolHostResult { success: !self.fail, result: if self.fail { "设备未连接".into() } else { "已连接".into() }, detail: Some(serde_json::json!({"success":!self.fail,"result":body}).to_string()) }
        })
    }
}

#[tokio::test]
async fn fixed_routes_skip_llm_keep_full_progress_and_report_failure() {
    use crate::conversation::{TurnInput, TurnEvent, spawn_turn};
    for (text, expect, success) in [("急停", "已发送停止请求", true), ("停止", "发送失败", false), ("当前进度", "百分之40", true), ("还要多久", "没有可信", true), ("当前材料参数", "材料库建议功率400", true)] {
        let tools = Arc::new(FastTools { fail: !success, ..Default::default() });
        let llm = Arc::new(FakeLlm::default());
        let asr = Arc::new(FakeAsr { replies: Mutex::new(VecDeque::from([Ok(text.into())])), ..Default::default() });
        let conversation = Arc::new(Conversation::new(asr, llm.clone(), Arc::new(FakeTts::default()), tools.clone(), ConversationSettings::default()));
        let context = ToolContext { device_id:"fake-device".into(), session_id:"fake-session".into() };
        let (handle, mut events) = spawn_turn(conversation, context.clone(), TurnInput::Utterance { pcm:vec![0;960], sample_rate:UP, stream:None }, vec![], None);
        let mut reply = String::new(); let mut stages = Vec::new(); let mut durations = Vec::new();
        while let Some(event) = events.recv().await {
            match event { TurnEvent::Speech { text, .. } | TurnEvent::TextReply(text) => reply = text,
                TurnEvent::Timing { stage, .. } => stages.push(stage), TurnEvent::ToolTiming { ms, .. } => durations.push(ms), _=>{} }
        }
        handle.await.unwrap();
        assert!(reply.contains(expect), "{text}: {reply}");
        assert!(llm.seen.lock().unwrap().is_empty());
        assert!(stages.contains(&"final_asr") && stages.contains(&"fast_route"));
        assert!(!stages.contains(&"first_llm_delta"));
        assert!(durations.iter().all(|ms| *ms >= 10));
        let calls = tools.calls.lock().unwrap();
        assert!(calls.iter().all(|(_,_,ctx)| *ctx == context));
        assert!(calls.iter().all(|(_,args,_)| [None,Some("status"),Some("recent"),Some("get"),Some("emergency_stop")].contains(&args["action"].as_str())));
        if crate::fast::is_stop(text) { assert_eq!(calls[0].1, serde_json::json!({"action":"emergency_stop","confirmed":true})); assert_eq!(calls.len(),1); }
    }
    // 关闭只读路由只影响只读词，停止始终保留；否定/嵌入词转正常 LLM。
    for text in ["当前进度", "别停", "停止之后怎么办"] {
        let llm = Arc::new(FakeLlm::default());
        let tools = Arc::new(FastTools::default());
        let asr = Arc::new(FakeAsr { replies:Mutex::new(VecDeque::from([Ok(text.into())])), ..Default::default() });
        let c = Arc::new(Conversation::new(asr,llm.clone(),Arc::new(FakeTts::default()),tools.clone(),ConversationSettings { read_only_shortcuts:false,..Default::default() }));
        let (handle,mut rx)=spawn_turn(c,ToolContext{device_id:"d".into(),session_id:"s".into()},TurnInput::Utterance{pcm:vec![0;960],sample_rate:UP,stream:None},vec![],None);
        while rx.recv().await.is_some() {} handle.await.unwrap();
        assert_eq!(llm.seen.lock().unwrap().len(),1);
        assert!(tools.calls.lock().unwrap().is_empty());
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
        let conversation = Conversation::new(Arc::new(FakeAsr::default()), llm.clone(), Arc::new(FakeTts::default()), tools.clone(), ConversationSettings::default())
            .with_streaming_asr(Arc::new(InterimStopAsr { final_text: text, sent: sent.clone() }));
        let registry = Arc::new(FakeRegistry::default());
        let server = VoiceServer::start(ServerConfig { bind:IpAddr::V4(Ipv4Addr::LOCALHOST), ws_port:0, http_port:0, mode:ServerMode::Chat, conversation:Some(Arc::new(conversation)), ..Default::default() }, registry.clone()).unwrap();
        let mut device = chat_device(&server, &registry, 1);
        send_json(&mut device, r#"{"type":"listen","state":"start","mode":"manual"}"#);
        send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 300, 8000.0)));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !sent.load(Ordering::Acquire) && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(5)); }
        assert!(sent.load(Ordering::Acquire));
        send_json(&mut device, r#"{"type":"ping"}"#);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let message = read_json(&mut device);
            if message["type"] == "pong" { break; }
            assert!(Instant::now() < deadline, "ping reply must not be lost during early stop");
        }
        // 即使最终识别稍后会给出否定句，完整中间识别为“停止”也应立即触发；
        // 这是 NV8a-1 用户选定的低延迟策略，误触发风险须在 UI 和实机验收说明。
        let deadline = Instant::now() + Duration::from_secs(3);
        while tools.calls.lock().unwrap().is_empty() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(5)); }
        assert_eq!(tools.calls.lock().unwrap().len(), 1, "whole interim stop must act before listen stop");
        send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
        let _items = collect_items(&mut device);
        assert_eq!(tools.calls.lock().unwrap().len(), 1, "interim and final must not trigger twice");
        assert!(llm.seen.lock().unwrap().is_empty());
    }
}
impl ToolExecutor for FakeTools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec { name: "get_time".into(), description: "查询时间".into(), parameters: serde_json::json!({"type":"object"}) }]
    }

    fn call<'a>(&'a self, context: &'a ToolContext, name: &'a str, _: &'a str) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((name.to_owned(), context.device_id.clone()));
            match name {
                "announce" => ToolOutcome::Speak("已经播报".into()),
                _ => ToolOutcome::Reply("12:00".into()),
            }
        })
    }
}

struct Fakes {
    asr: Arc<FakeAsr>,
    llm: Arc<FakeLlm>,
    tts: Arc<FakeTts>,
    tools: Arc<FakeTools>,
}

fn start_chat(llm: FakeLlm, asr_replies: &[&str], idle: Duration) -> (VoiceServer, Arc<FakeRegistry>, Fakes) {
    start_chat_with(llm,asr_replies,idle,None)
}
fn start_chat_with(llm: FakeLlm, asr_replies: &[&str], idle: Duration, vision:Option<Arc<dyn crate::vision::VisionFactory>>) -> (VoiceServer, Arc<FakeRegistry>, Fakes) {
    let fakes = Fakes {
        asr: Arc::new(FakeAsr {
            replies: Mutex::new(asr_replies.iter().map(|text| Ok((*text).to_owned())).collect()),
            heard: Mutex::default(),
        }),
        llm: Arc::new(llm),
        tts: Arc::new(FakeTts::default()),
        tools: Arc::new(FakeTools::default()),
    };
    let conversation = Conversation::new(
        fakes.asr.clone(),
        fakes.llm.clone(),
        fakes.tts.clone(),
        fakes.tools.clone(),
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        mode: ServerMode::Chat,
        vision,
        conversation: Some(Arc::new(conversation)),
        idle_timeout: idle,
        ..ServerConfig::default()
    };
    let server = VoiceServer::start(config, registry.clone()).unwrap();
    (server, registry, fakes)
}

#[derive(Debug)]
enum Item {
    Text(Value),
    Frame,
}

/// 收到 tts stop 为止的全部消息（保持顺序）。
fn collect_items(device: &mut Device) -> Vec<Item> {
    let mut items = Vec::new();
    loop {
        match device.read().unwrap() {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                let stop = value["type"] == "tts" && value["state"] == "stop";
                items.push(Item::Text(value));
                if stop {
                    return items;
                }
            }
            Message::Binary(_) => items.push(Item::Frame),
            _ => {}
        }
    }
}

fn texts(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Text(value) => Some(match (value["type"].as_str(), value["state"].as_str()) {
                (Some("tts"), Some("sentence_start")) => format!("句:{}", value["text"].as_str().unwrap()),
                (Some("tts"), Some(state)) => format!("tts:{state}"),
                (Some("stt"), _) => format!("听:{}", value["text"].as_str().unwrap()),
                (Some(kind), _) => kind.to_owned(),
                _ => "?".into(),
            }),
            Item::Frame => None,
        })
        .collect()
}

/// 对话测试用的设备：不声明自带 MCP（设备 MCP 另有专门测试）。
fn chat_device(server: &VoiceServer, registry: &FakeRegistry, version: u32) -> Device {
    allowed_device_with(server, registry, version, false)
}

fn speak_manual(device: &mut Device, version: u32, ms: u32) {
    send_json(device, r#"{"type":"listen","state":"start","mode":"manual"}"#);
    send_audio(device, version, &uplink(&tone(UP, 300.0, ms, 8000.0)));
    send_json(device, r#"{"type":"listen","state":"stop"}"#);
}

#[test]
fn chat_mode_requires_conversation() {
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        mode: ServerMode::Chat,
        ..ServerConfig::default()
    };
    assert!(VoiceServer::start(config, Arc::new(FakeRegistry::default())).err().unwrap().contains("对话模式"));
}

#[test]
fn answer_is_spoken_sentence_by_sentence_with_synced_text_and_history() {
    let llm = FakeLlm { rounds: Mutex::new(VecDeque::from([say(&["你好，", "我是 MOSS。", "很高兴见到你。"]), say(&["记得。"])])), ..FakeLlm::default() };
    let (server, registry, fakes) = start_chat(llm, &["今天天气怎么样", "你还记得吗"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 1000);
    let items = collect_items(&mut device);
    assert_eq!(
        texts(&items),
        vec!["听:今天天气怎么样", "tts:start", "句:你好，", "句:我是 MOSS。", "句:很高兴见到你。", "tts:stop"]
    );
    // 每句的文字紧挨着这句的第一帧：两句文字之间有音频帧，第一帧在第一句文字之后
    let positions: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item, Item::Text(value) if value["state"] == "sentence_start"))
        .map(|(index, _)| index)
        .collect();
    let first_frame = items.iter().position(|item| matches!(item, Item::Frame)).unwrap();
    assert!(first_frame == positions[0] + 1);
    for pair in positions.windows(2) {
        assert!(items[pair[0]..pair[1]].iter().filter(|item| matches!(item, Item::Frame)).count() >= 4, "{pair:?}");
    }
    // 3 句各 300 ms → 24 kHz 下 15 帧
    assert_eq!(items.iter().filter(|item| matches!(item, Item::Frame)).count(), 15);
    assert_eq!(*fakes.tts.said.lock().unwrap(), vec!["你好，", "我是 MOSS。", "很高兴见到你。"]);
    // 1000 ms 在设备端编成 17 个 60 ms 帧（最后一帧补零），识别收到全部 17 × 960 个样本
    assert_eq!(fakes.asr.heard.lock().unwrap()[0], 17 * 960, "识别收到完整的手动录音");

    speak_manual(&mut device, 1, 600);
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:你还记得吗", "tts:start", "句:记得。", "tts:stop"]);
    let seen = fakes.llm.seen.lock().unwrap();
    let (second, tools) = &seen[1];
    assert_eq!(*tools, 1, "工具定义随请求发送");
    let roles: Vec<Role> = second.iter().map(|message| message.role).collect();
    assert_eq!(roles, vec![Role::System, Role::User, Role::Assistant, Role::User]);
    assert_eq!(second[2].content.as_deref(), Some("你好，我是 MOSS。很高兴见到你。"));
    let events = server.status().events;
    assert!(events.iter().any(|event| event.text.contains("识别完成：7 字")));
    assert!(events.iter().any(|event| event.text.starts_with("首句延迟：")));
    assert!(!events.iter().any(|event| event.text.contains("天气")), "识别文字不写入事件");
}

#[test]
fn tool_results_go_back_to_the_model_and_speak_outcome_skips_it() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([call("get_time"), say(&["现在十二点。"]), call("announce")])),
        ..FakeLlm::default()
    };
    let (server, registry, fakes) = start_chat(llm, &["几点了", "播报一下"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:几点了", "tts:start", "句:现在十二点。", "tts:stop"]);
    {
        let seen = fakes.llm.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let second = &seen[1].0;
        let call_message = &second[second.len() - 2];
        assert_eq!(call_message.tool_calls[0].function.name, "get_time");
        let result = second.last().unwrap();
        assert_eq!((result.role, result.content.as_deref(), result.tool_call_id.as_deref()), (Role::Tool, Some("12:00"), Some("call-get_time")));
    }
    assert_eq!(fakes.tools.calls.lock().unwrap()[0], ("get_time".to_owned(), DEVICE.to_owned()), "工具收到设备身份");

    // “直接播报”的结果不再询问模型
    speak_manual(&mut device, 1, 600);
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:播报一下", "tts:start", "句:已经播报", "tts:stop"]);
    assert_eq!(fakes.llm.seen.lock().unwrap().len(), 3);
}

#[test]
fn endless_tool_calls_stop_after_five_rounds() {
    let llm = FakeLlm { repeat: Some(call("get_time")), ..FakeLlm::default() };
    let (server, registry, fakes) = start_chat(llm, &["一直查"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let spoken = texts(&collect_items(&mut device));
    assert_eq!(spoken[2], format!("句:{}", ConversationSettings::default().error_reply));
    assert_eq!(fakes.llm.seen.lock().unwrap().len(), 6, "5 轮工具 + 第 6 次回复仍要调用工具时停止");
    assert_eq!(fakes.tools.calls.lock().unwrap().len(), 5);
    assert!(server.status().events.iter().any(|event| event.text.contains("超过 5 轮")));
}

#[test]
fn model_errors_and_failed_sentences_are_handled() {
    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([Round::Fail("大模型返回 HTTP 401，密钥无效或没有权限"), say(&["第一句。", "这句失败。", "第三句。"])])),
        ..FakeLlm::default()
    };
    let (server, registry, _fakes) = start_chat(llm, &["你好", "再试一次"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let reply = ConversationSettings::default().error_reply;
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:你好".to_owned(), "tts:start".into(), format!("句:{reply}"), "tts:stop".into()]);
    assert!(server.status().events.iter().any(|event| event.text.contains("对话出错：大模型返回 HTTP 401")));
    // 合成失败的句子跳过，其余照常
    speak_manual(&mut device, 1, 600);
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:再试一次", "tts:start", "句:第一句。", "句:第三句。", "tts:stop"]);
}

#[test]
fn exit_word_says_goodbye_and_closes() {
    let (server, registry, fakes) = start_chat(FakeLlm::default(), &["再见！"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let goodbye = ConversationSettings::default().goodbye;
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:再见！".to_owned(), "tts:start".into(), format!("句:{goodbye}"), "tts:stop".into()]);
    expect_closed(&mut device);
    assert!(fakes.llm.seen.lock().unwrap().is_empty(), "退出词不经过大模型");
    assert!(server.status().events.iter().any(|event| event.text.contains("用户说了退出词")));
}

#[test]
fn abort_cancels_turn_and_keeps_partial_history() {
    let slow = Round::Reply {
        deltas: vec!["第一句。", "第二句。", "第三句。", "第四句。", "第五句。"],
        calls: Vec::new(),
        delay_ms: 400,
    };
    let llm = FakeLlm { rounds: Mutex::new(VecDeque::from([slow, say(&["好。"])])), ..FakeLlm::default() };
    let (server, registry, fakes) = start_chat(llm, &["讲个长故事", "算了"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 2);
    speak_manual(&mut device, 2, 600);
    // 等到第一帧音频后打断
    loop {
        if let Message::Binary(_) = device.read().unwrap() {
            break;
        }
    }
    send_json(&mut device, r#"{"type":"abort","reason":"wake_word_detected"}"#);
    loop {
        if let Message::Text(text) = device.read().unwrap()
            && text.as_str().contains(r#""state":"stop""#)
        {
            break;
        }
    }
    device.get_mut().set_read_timeout(Some(Duration::from_millis(1500))).unwrap();
    match device.read() {
        Ok(Message::Binary(_)) => panic!("打断后仍在发送音频"),
        Ok(Message::Text(text)) => panic!("打断后仍有消息：{text}"),
        _ => {}
    }
    device.get_mut().set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert!(fakes.tts.said.lock().unwrap().len() < 5, "打断后不再合成后面的句子");
    speak_manual(&mut device, 2, 600);
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:算了", "tts:start", "句:好。", "tts:stop"]);
    let seen = fakes.llm.seen.lock().unwrap();
    let history = &seen[1].0;
    assert_eq!(history[1].content.as_deref(), Some("讲个长故事"));
    assert_eq!(history[2].role, Role::Assistant);
    assert!(history[2].content.as_deref().unwrap().starts_with("第一句。"), "已播的句子记入历史");
}

#[test]
fn empty_transcript_keeps_listening_in_auto_mode() {
    let (server, registry, fakes) = start_chat(FakeLlm::default(), &["", "开始"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    send_json(&mut device, r#"{"type":"listen","state":"start","mode":"auto"}"#);
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&pcm));
    // 第一次识别为空：不回复，继续拾音；不发 listen start 再说一次
    std::thread::sleep(Duration::from_millis(300));
    send_audio(&mut device, 1, &uplink(&pcm));
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:开始", "tts:start", "句:好的。", "tts:stop"]);
    assert_eq!(fakes.asr.heard.lock().unwrap().len(), 2);
    assert!(server.status().events.iter().any(|event| event.text.contains("没有识别出文字")));
}

#[test]
fn wake_word_gets_cached_greeting_then_listens() {
    let (server, registry, fakes) = start_chat(FakeLlm::default(), &["开灯"], Duration::from_secs(120));
    let mut device = chat_device(&server, &registry, 1);
    for round in 0..2 {
        // 固件：检测到唤醒词后先发 detect，紧接着发 listen start（自动模式）
        send_json(&mut device, r#"{"type":"listen","state":"detect","text":"你好小智"}"#);
        send_json(&mut device, r#"{"type":"listen","state":"start","mode":"auto"}"#);
        assert_eq!(texts(&collect_items(&mut device)), vec!["tts:start", "句:我在", "tts:stop"], "第 {round} 次唤醒");
    }
    assert_eq!(fakes.tts.said.lock().unwrap().iter().filter(|text| *text == "我在").count(), 1, "问候语只合成一次");
    // 问候播完后已在拾音：直接说话即可
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&pcm));
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:开灯", "tts:start", "句:好的。", "tts:stop"]);
}

#[test]
fn idle_session_says_goodbye_then_closes() {
    let (server, registry, _fakes) = start_chat(FakeLlm::default(), &[], Duration::from_millis(800));
    let mut device = chat_device(&server, &registry, 1);
    let started = Instant::now();
    let goodbye = ConversationSettings::default().goodbye;
    assert_eq!(texts(&collect_items(&mut device)), vec!["tts:start".to_owned(), format!("句:{goodbye}"), "tts:stop".into()]);
    assert!(started.elapsed() >= Duration::from_millis(700));
    expect_closed(&mut device);
    assert!(server.status().events.iter().any(|event| event.text.contains("没有对话，已说再见")));
}

/// 读一条设备 MCP 消息的负载（跳过其他文本消息）。
fn read_mcp(device: &mut Device) -> Value {
    loop {
        let value = read_json(device);
        if value["type"] == "mcp" {
            return value["payload"].clone();
        }
    }
}

fn reply_mcp(device: &mut Device, id: &Value, result: Value) {
    let message = serde_json::json!({"type": "mcp", "payload": {"jsonrpc": "2.0", "id": id, "result": result}});
    send_json(device, &message.to_string());
}

/// NV12: advertise the actual 8003 HTTP port and a token only to allowed,
/// MCP-capable devices when the configured provider is available.
#[test]
fn vision_capability_is_conditional_and_uses_bound_http_port(){
    use std::sync::atomic::AtomicBool;
    use moss_voice_providers::vision::{VisionProvider,VisionFailure};
    use crate::vision::{VisionFactory,VisionSession};
    struct ReadyProvider;
    impl VisionProvider for ReadyProvider {
        fn explain<'a>(&'a self,_:&'a str,_:&'a str,_:&'a [u8])->BoxFuture<'a,Result<String,VisionFailure>>{
            Box::pin(async {Ok("图片解释".into())})
        }
    }
    struct ReadyFactory {enabled:Arc<AtomicBool>}
    impl VisionFactory for ReadyFactory {
        fn ready(&self)->bool{self.enabled.load(std::sync::atomic::Ordering::Acquire)}
        fn open(&self)->Option<VisionSession>{
            self.ready().then(||VisionSession{provider:Arc::new(ReadyProvider),generation:1})
        }
        fn still_current(&self,_:u64)->bool{self.ready()}
    }
    let on=Arc::new(AtomicBool::new(true));
    let factory=Arc::new(ReadyFactory{enabled:on});
    let (server,registry,_fakes)=start_chat_with(FakeLlm::default(),&[],Duration::from_secs(120),Some(factory));
    let mut device=allowed_device_with(&server,&registry,1,true);
    let initial=read_mcp(&mut device);
    assert_eq!(initial["method"],"initialize");
    let cap=&initial["params"]["capabilities"]["vision"];
    assert_eq!(cap["url"],format!("http://127.0.0.1:{}/mcp/vision/explain",server.http_port()));
    let token=cap["token"].as_str().unwrap();
    assert!(TokenSigner::new(SECRET.to_vec()).verify(token,CLIENT,DEVICE,unix_now()));
    let off=Arc::new(AtomicBool::new(false));
    let (disabled,registered,_)=start_chat_with(FakeLlm::default(),&[],Duration::from_secs(120),
        Some(Arc::new(ReadyFactory{enabled:off})));
    let mut device_off=allowed_device_with(&disabled,&registered,1,true);
    let init_off=read_mcp(&mut device_off);
    assert!(init_off["params"]["capabilities"]["vision"].is_null());
}

/// NV4 设备自带 MCP：握手、分页取工具、模型以清理后的名字调用，设备收到原名，结果回填给模型。
#[test]
fn device_mcp_tools_are_listed_and_relayed() {
    let call = Round::Reply {
        deltas: Vec::new(),
        calls: vec![ToolCall::new("c1", "device__self_audio_speaker_set_volume", r#"{"volume":50}"#)],
        delay_ms: 0,
    };
    let llm = FakeLlm { rounds: Mutex::new(VecDeque::from([call, say(&["好了。"])])), ..FakeLlm::default() };
    let (server, registry, fakes) = start_chat(llm, &["音量调到一半"], Duration::from_secs(120));
    let mut device = allowed_device_with(&server, &registry, 1, true);

    let initialize = read_mcp(&mut device);
    assert_eq!(initialize["method"], "initialize");
    assert_eq!(initialize["params"]["clientInfo"]["name"], "MOSS");
    reply_mcp(&mut device, &initialize["id"], serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}));
    assert_eq!(read_mcp(&mut device)["method"], "notifications/initialized");
    let page1 = read_mcp(&mut device);
    assert_eq!(page1["method"], "tools/list");
    reply_mcp(&mut device, &page1["id"], serde_json::json!({
        "tools": [{"name": "self.get_device_status", "description": "设备状态", "inputSchema": {"type": "object", "properties": {}}}],
        "nextCursor": "p2"
    }));
    let page2 = read_mcp(&mut device);
    assert_eq!(page2["params"]["cursor"], "p2");
    reply_mcp(&mut device, &page2["id"], serde_json::json!({
        "tools": [{"name": "self.audio_speaker.set_volume", "description": "设置音量", "inputSchema": {"type": "object", "properties": {"volume": {"type": "integer"}}}}]
    }));

    speak_manual(&mut device, 1, 600);
    assert_eq!(read_json(&mut device)["text"], "音量调到一半");
    let call = read_mcp(&mut device);
    assert_eq!(call["method"], "tools/call");
    assert_eq!(call["params"]["name"], "self.audio_speaker.set_volume", "设备收到原始工具名");
    assert_eq!(call["params"]["arguments"]["volume"], 50);
    reply_mcp(&mut device, &call["id"], serde_json::json!({"content": [{"type": "text", "text": "音量已设为 50"}], "isError": false}));
    assert_eq!(texts(&collect_items(&mut device)), vec!["tts:start", "句:好了。", "tts:stop"]);

    let seen = fakes.llm.seen.lock().unwrap();
    assert_eq!(seen[0].1, 3, "模型看到 MOSS 工具 1 个 + 设备工具 2 个");
    let result = seen[1].0.last().unwrap();
    assert_eq!((result.role, result.content.as_deref()), (Role::Tool, Some("音量已设为 50")));
    assert!(server.status().events.iter().any(|event| event.text.contains("设备提供 2 个自带工具")));
}

#[test]
fn device_mcp_results_are_turned_into_text() {
    use crate::session::device_result_text;
    let error = serde_json::json!({"id": 3, "error": {"code": -32601, "message": "Unknown tool"}});
    assert_eq!(device_result_text(&error), "设备工具出错：Unknown tool");
    let failed = serde_json::json!({"id": 3, "result": {"content": [{"type": "text", "text": "摄像头不可用"}], "isError": true}});
    assert_eq!(device_result_text(&failed), "设备工具执行失败：摄像头不可用");
    let raw = serde_json::json!({"id": 3, "result": {"battery": 80}});
    assert_eq!(device_result_text(&raw), r#"{"battery":80}"#);
}

/// 假流式识别（NV7）：记录每段识别实时收到的样本数，说完后返回固定结果。
struct FakeStreamingAsr {
    reply: Result<String, ProviderError>,
    received: Arc<Mutex<Vec<usize>>>,
}

impl FakeStreamingAsr {
    fn new(reply: Result<&str, &str>) -> Arc<Self> {
        Arc::new(Self {
            reply: reply.map(str::to_owned).map_err(ProviderError::new),
            received: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn received(&self) -> Vec<usize> {
        self.received.lock().unwrap().clone()
    }
}

impl StreamingAsr for FakeStreamingAsr {
    fn start(&self) -> AsrStream {
        let index = {
            let mut received = self.received.lock().unwrap();
            received.push(0);
            received.len() - 1
        };
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let (received, reply) = (self.received.clone(), self.reply.clone());
        let task = tokio::spawn(async move {
            while let Some(chunk) = audio_rx.recv().await {
                received.lock().unwrap()[index] += chunk.len();
            }
            let _ = result_tx.send(reply);
        });
        AsrStream::new(audio_tx, result_rx, task)
    }
}

/// 假流式合成：每句分 `chunks` 段送出，段间隔 `gap_ms`（模拟边合成边返回）；记录最后一段送出的时刻。
struct ChunkedTts {
    chunks: usize,
    chunk_ms: u32,
    gap_ms: u64,
    finished_at: Mutex<Option<Instant>>,
}

impl Tts for ChunkedTts {
    fn synthesize<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async move {
            Ok(Pcm { samples: tone(24_000, 300.0, self.chunk_ms * self.chunks as u32, 6000.0), sample_rate: 24_000 })
        })
    }

    fn synthesize_stream<'a>(
        &'a self,
        _: &'a str,
        chunks: tokio::sync::mpsc::UnboundedSender<Pcm>,
    ) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            for _ in 0..self.chunks {
                tokio::time::sleep(Duration::from_millis(self.gap_ms)).await;
                let _ = chunks.send(Pcm { samples: tone(24_000, 300.0, self.chunk_ms, 6000.0), sample_rate: 24_000 });
            }
            *self.finished_at.lock().unwrap() = Some(Instant::now());
            Ok(())
        })
    }
}

fn start_streaming_chat(
    streaming: Arc<FakeStreamingAsr>,
    tts: Arc<dyn Tts>,
    playback_start_ms: u32,
) -> (VoiceServer, Arc<FakeRegistry>, Arc<FakeAsr>) {
    let asr = Arc::new(FakeAsr { replies: Mutex::new(VecDeque::from([Ok("整段识别".to_owned())])), heard: Mutex::default() });
    let conversation = Conversation::new(
        asr.clone(),
        Arc::new(FakeLlm::default()),
        tts,
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    )
    .with_streaming_asr(streaming);
    let registry = Arc::new(FakeRegistry::default());
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        mode: ServerMode::Chat,
        conversation: Some(Arc::new(conversation)),
        playback_start_ms,
        ..ServerConfig::default()
    };
    let server = VoiceServer::start(config, registry.clone()).unwrap();
    (server, registry, asr)
}

/// NV7：手动模式按下即建立流式识别，说话期间音频已经送出；说完直接用流式结果，不再整段识别。
#[test]
fn streaming_recognition_receives_audio_while_the_user_speaks() {
    let streaming = FakeStreamingAsr::new(Ok("打开激光"));
    let (server, registry, asr) = start_streaming_chat(streaming.clone(), Arc::new(FakeTts::default()), 600);
    let mut device = chat_device(&server, &registry, 1);
    send_json(&mut device, r#"{"type":"listen","state":"start","mode":"manual"}"#);
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 1000, 8000.0)));
    // 还没松开按键：音频已经送到识别
    let deadline = Instant::now() + Duration::from_secs(3);
    while streaming.received().first().copied().unwrap_or(0) < 17 * 960 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(streaming.received(), vec![17 * 960], "说话期间收到全部音频");
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:打开激光", "tts:start", "句:好的。", "tts:stop"]);
    assert!(asr.heard.lock().unwrap().is_empty(), "流式成功时不再整段识别");
}

/// NV7：自动模式只在 VAD 确认开始说话后才建立识别（环境静音不建连接），并补送说话前保留的音频。
#[test]
fn auto_mode_opens_streaming_recognition_only_when_speech_starts() {
    let streaming = FakeStreamingAsr::new(Ok("开灯"));
    let (server, registry, _asr) = start_streaming_chat(streaming.clone(), Arc::new(FakeTts::default()), 600);
    let mut device = chat_device(&server, &registry, 1);
    send_json(&mut device, r#"{"type":"listen","state":"start","mode":"auto"}"#);
    send_audio(&mut device, 1, &uplink(&silence(UP, 900)));
    std::thread::sleep(Duration::from_millis(300));
    assert!(streaming.received().is_empty(), "只有静音时不建立识别");
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&pcm));
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:开灯", "tts:start", "句:好的。", "tts:stop"]);
    let received = streaming.received();
    assert_eq!(received.len(), 1, "一段话只建立一次识别");
    assert!(received[0] >= UP as usize * 600 / 1000 + UP as usize * 200 / 1000, "含说话前保留的音频：{received:?}");
}

/// NV7：流式识别失败时用整段音频再识别一次，并记录原因。
#[test]
fn failed_streaming_recognition_falls_back_to_whole_utterance() {
    let streaming = FakeStreamingAsr::new(Err("豆包识别连接被拒绝（HTTP 401）"));
    let (server, registry, asr) = start_streaming_chat(streaming, Arc::new(FakeTts::default()), 600);
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    assert_eq!(texts(&collect_items(&mut device)), vec!["听:整段识别", "tts:start", "句:好的。", "tts:stop"]);
    assert_eq!(asr.heard.lock().unwrap().len(), 1);
    assert!(server.status().events.iter().any(|event| event.text.contains("流式识别失败，改用整段识别：豆包识别连接被拒绝")));
}

/// NV7：流式合成的第一段一到就开始播放（不等整句合成完），句子文字只发一次，分段之间连续不补静音。
#[test]
fn streamed_synthesis_starts_playing_before_the_sentence_is_finished() {
    let tts = Arc::new(ChunkedTts { chunks: 4, chunk_ms: 240, gap_ms: 150, finished_at: Mutex::new(None) });
    let (server, registry, _asr) = start_streaming_chat(FakeStreamingAsr::new(Ok("你好")), tts.clone(), 300);
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let mut first_frame = None;
    let mut items = Vec::new();
    loop {
        match device.read().unwrap() {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                let stop = value["type"] == "tts" && value["state"] == "stop";
                items.push(Item::Text(value));
                if stop {
                    break;
                }
            }
            Message::Binary(_) => {
                first_frame.get_or_insert_with(Instant::now);
                items.push(Item::Frame);
            }
            _ => {}
        }
    }
    assert_eq!(texts(&items), vec!["听:你好", "tts:start", "句:好的。", "tts:stop"]);
    let finished = tts.finished_at.lock().unwrap().unwrap();
    assert!(first_frame.unwrap() < finished, "第一帧在整句合成完之前发出");
    // 4 段 × 240 ms = 960 ms → 16 帧；合成快于实时，不应补静音
    assert_eq!(items.iter().filter(|item| matches!(item, Item::Frame)).count(), 16);
    assert!(server.status().events.iter().any(|event| event.text.contains("补静音 0 帧")), "{:?}", server.status().events);
}

/// 真实本地模型的端到端测试（不在常规测试中运行）：用合成的一句话当作设备上传的语音，
/// 自动模式下由本地判停、本地识别，假大模型回答，再由本地合成回复。
/// `MOSS_VOICE_LOCAL_EXE=<moss-voice-local.exe> MOSS_LOCAL_MODELS=<模型目录> cargo test -p moss-voice-server real_local -- --ignored --nocapture`
struct LocalCombo {
    vad: (&'static str, moss_voice_providers::local::VadEngine),
    asr: (&'static str, moss_voice_providers::local::AsrEngine),
    tts: (&'static str, moss_voice_providers::local::TtsEngine, i32),
}

fn run_local_end_to_end(combo: LocalCombo) {
    use moss_voice_providers::local::{LocalAsr, LocalHelper, LocalStreamingAsr, LocalTts, VadParams};
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let models = std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").expect("MOSS_LOCAL_MODELS"));
    let helper = LocalHelper::spawn(std::path::Path::new(&exe)).unwrap();
    helper.load_vad(&models.join(combo.vad.0), VadParams::for_engine(combo.vad.1), combo.vad.1).unwrap();
    helper.load_asr(&models.join(combo.asr.0), combo.asr.1).unwrap();
    helper.load_tts(&models.join(combo.tts.0), combo.tts.1).unwrap();
    let speaker = combo.tts.2;

    // 设备端“说话”：合成后重采样到 16 kHz，前后加静音
    let speech = helper.synthesize_blocking("帮我看看激光机的状态。", speaker, 1.0).unwrap();
    let mut resampler = crate::audio::Resampler::new(speech.sample_rate, UP);
    let mut pcm = silence(UP, 500);
    pcm.extend(resampler.process(&speech.samples));
    pcm.extend(silence(UP, 1200));

    let llm = FakeLlm { rounds: Mutex::new(VecDeque::from([say(&["激光机现在空闲。"])])), ..FakeLlm::default() };
    let llm = Arc::new(llm);
    let mut conversation = Conversation::new(
        Arc::new(LocalAsr { helper: helper.clone() }),
        llm.clone(),
        Arc::new(LocalTts { helper: helper.clone(), speaker, speed: 1.0 }),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    );
    if combo.asr.1.streaming() {
        conversation = conversation.with_streaming_asr(Arc::new(LocalStreamingAsr { helper: helper.clone() }));
    }
    let registry = Arc::new(FakeRegistry::default());
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        mode: ServerMode::Chat,
        conversation: Some(Arc::new(conversation)),
        vad_factory: Some(Arc::new(crate::LocalVadFactory { helper: helper.clone() })),
        ..ServerConfig::default()
    };
    let server = VoiceServer::start(config, registry.clone()).unwrap();
    let mut device = chat_device(&server, &registry, 1);
    device.get_mut().set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    send_json(&mut device, r#"{"type":"listen","state":"start","mode":"auto"}"#);
    let started = Instant::now();
    send_audio(&mut device, 1, &uplink(&pcm));
    let items = collect_items(&mut device);
    let spoken = texts(&items);
    println!("用时 {:?}：{spoken:?}", started.elapsed());
    for event in server.status().events.iter().rev() {
        println!("  事件：{}", event.text);
    }
    assert!(spoken[0].starts_with("听:") && spoken[0].contains("激光"), "{spoken:?}");
    assert!(spoken.contains(&"句:激光机现在空闲。".to_owned()));
    let frames = items.iter().filter(|item| matches!(item, Item::Frame)).count();
    assert!(frames > 10, "回复有音频：{frames} 帧");
    let events = server.status().events;
    assert!(!events.iter().any(|event| event.text.contains("改用能量判停")), "本地判停正常工作");
    assert!(!events.iter().any(|event| event.text.contains("改用整段识别")), "流式识别没有失败");
    assert_eq!(llm.seen.lock().unwrap()[0].0.last().unwrap().role, Role::User);
}

/// NV6 默认组合：Silero + SenseVoice 2024 + Kokoro v1.0。
#[test]
#[ignore]
fn real_local_models_end_to_end() {
    use moss_voice_providers::local::{AsrEngine, TtsEngine, VadEngine};
    run_local_end_to_end(LocalCombo {
        vad: ("silero_vad.onnx", VadEngine::Silero),
        asr: ("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17", AsrEngine::SenseVoice),
        tts: ("kokoro-multi-lang-v1_0", TtsEngine::Kokoro, 50),
    });
}

/// 可选模型中最快的组合：TEN-VAD + X-ASR 流式（边说边识别）+ Matcha。
#[test]
#[ignore]
fn real_local_fast_models_end_to_end() {
    use moss_voice_providers::local::{AsrEngine, TtsEngine, VadEngine};
    run_local_end_to_end(LocalCombo {
        vad: ("ten-vad.onnx", VadEngine::Ten),
        asr: ("sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05", AsrEngine::StreamingTransducer),
        tts: ("matcha-icefall-zh-en", TtsEngine::Matcha, 0),
    });
}

/// NV19-04: the optional retrieval hook injects labelled memory for real questions only;
/// small talk still reaches the model without a search, stop words never reach either.
#[tokio::test]
async fn nv19_context_retrieval_hook_injects_memory_only_for_questions() {
    use crate::context::fake_store::FakeStore;
    use crate::context::retrieve::{ContextAccessProvider, ContextRetrieval, ContextRetriever};
    use crate::context::service::ContextService;
    use crate::context::{AccessContext, Applicability, Category, MemoryDraft, ScopeKey, ScopeKind, Source, SourceKind, TrustedScope, Verification};
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    use crate::person_context::PersonScope;

    struct Access;
    impl ContextAccessProvider for Access {
        fn access(&self, context: &ToolContext, _person: Option<&PersonScope>) -> Option<(AccessContext, Option<Applicability>)> {
            let device = TrustedScope::device_grant(&context.device_id, 1).ok()?;
            Some((AccessContext::voice(device, None, None).ok()?, Applicability::new("MOSS-550W", Some("1.2.0")).ok()))
        }
    }
    let service = ContextService::new(Arc::new(FakeStore::default()));
    let owner = AccessContext::desktop_private(vec![TrustedScope::device_grant("fake-device", 1).unwrap()]);
    service
        .create(&owner, MemoryDraft {
            scope: ScopeKey::new(ScopeKind::Device, "fake-device").unwrap(),
            category: Category::TroubleshootingExperience,
            content: "边缘发黑时降低功率后改善".into(),
            verification: Verification::Confirmed,
            source: Source::new(SourceKind::UserEdit, None).unwrap(),
            applicability: Some(Applicability::new("MOSS-550W", Some("1.2.0")).unwrap()),
            expires_at: None,
        }, 1.0)
        .unwrap();
    let retriever = ContextRetriever::new(service);
    retriever.set_enabled(true);

    for (text, expect_llm, expect_memory) in [("上次边缘发黑是怎么解决的", true, true), ("你好", true, false), ("急停", false, false)] {
        let llm = Arc::new(FakeLlm::default());
        let asr = Arc::new(FakeAsr { replies: Mutex::new(VecDeque::from([Ok(text.into())])), ..Default::default() });
        let conversation = Conversation::new(asr, llm.clone(), Arc::new(FakeTts::default()), Arc::new(FastTools::default()), ConversationSettings::default())
            .with_context_retrieval(ContextRetrieval { retriever: retriever.clone(), access: Arc::new(Access) });
        let context = ToolContext { device_id: "fake-device".into(), session_id: "fake-session".into() };
        let (handle, mut events) = spawn_turn(Arc::new(conversation), context, TurnInput::Utterance { pcm: vec![0; 960], sample_rate: UP, stream: None }, vec![], None);
        while let Some(event) = events.recv().await {
            let _: TurnEvent = event;
        }
        handle.await.unwrap();
        let seen = llm.seen.lock().unwrap();
        assert_eq!(!seen.is_empty(), expect_llm, "{text}");
        let injected = seen.iter().flat_map(|(messages, _)| messages.iter()).any(|m| m.role == Role::System && m.content.as_deref().is_some_and(|c| c.contains("长期记忆检索结果") && c.contains("降低功率")));
        assert_eq!(injected, expect_memory, "{text}");
    }
    assert_eq!(retriever.searches(), 1, "只有真正的问题会查库");
}
