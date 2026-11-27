//! 对话模式（NV3）端到端测试：假的识别 / 大模型 / 合成 / 工具，tungstenite 扮演设备。

use std::collections::VecDeque;

use moss_voice_providers::{
    Asr, AsrStream, BoxFuture, ChatMessage, Llm, LlmReply, Pcm, ProviderError, Role, StreamingAsr,
    ToolCall, ToolSpec, Tts,
};

use super::*;
use crate::conversation::{
    Conversation, ConversationSettings, ToolContext, ToolExecutor, ToolOutcome,
};

mod addressee_eval;
mod background;
mod barge;
mod chain;
mod context;
mod device_mcp;
mod follow;
mod guide;
mod interrupt;
mod jev;
mod local;
mod mqtt;
mod music;
mod notice;
mod person;
mod playback;
mod preview;
mod screen;
mod shortcut;
mod simulated_inject;
mod streaming;
mod trace;
mod turns;

#[derive(Default)]
struct FakeAsr {
    replies: Mutex<VecDeque<Result<String, ProviderError>>>,
    heard: Mutex<Vec<usize>>,
}

impl Asr for FakeAsr {
    fn transcribe<'a>(
        &'a self,
        pcm: &'a [i16],
        sample_rate: u32,
    ) -> BoxFuture<'a, Result<String, ProviderError>> {
        Box::pin(async move {
            assert_eq!(sample_rate, UP);
            self.heard.lock().unwrap().push(pcm.len());
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok("你好".into()))
        })
    }
}

#[derive(Clone)]
enum Round {
    Reply {
        deltas: Vec<&'static str>,
        calls: Vec<ToolCall>,
        delay_ms: u64,
    },
    Fail(&'static str),
}

fn say(deltas: &[&'static str]) -> Round {
    Round::Reply {
        deltas: deltas.to_vec(),
        calls: Vec::new(),
        delay_ms: 0,
    }
}

fn call(name: &str) -> Round {
    Round::Reply {
        deltas: Vec::new(),
        calls: vec![ToolCall::new(format!("call-{name}"), name, "{}")],
        delay_ms: 0,
    }
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
            self.seen
                .lock()
                .unwrap()
                .push((messages.to_vec(), tools.len()));
            let round = self
                .rounds
                .lock()
                .unwrap()
                .pop_front()
                .or_else(|| self.repeat.clone())
                .unwrap_or_else(|| say(&["好的。"]));
            match round {
                Round::Fail(message) => Err(ProviderError::new(message)),
                Round::Reply {
                    deltas: parts,
                    calls,
                    delay_ms,
                } => {
                    let mut text = String::new();
                    for part in parts {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        text.push_str(part);
                        let _ = deltas.send(part.to_owned());
                    }
                    Ok(LlmReply {
                        text,
                        tool_calls: calls,
                    })
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
            Ok(Pcm {
                samples: tone(16_000, 300.0, 300, 6000.0),
                sample_rate: 16_000,
            })
        })
    }
}

#[derive(Default)]
struct FakeTools {
    calls: Mutex<Vec<(String, String)>>,
}

#[derive(Default)]
struct FastTools {
    calls: Mutex<Vec<(String, Value, ToolContext)>>,
    fail: bool,
}

impl ToolExecutor for FastTools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![]
    }
    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { panic!("fixed routes must use full results") })
    }
    fn call_raw<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, crate::ToolHostResult> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(arguments).unwrap();
            self.calls
                .lock()
                .unwrap()
                .push((name.into(), args.clone(), context.clone()));
            tokio::time::sleep(Duration::from_millis(15)).await;
            let body = match (name, args["action"].as_str()) {
                ("laser_workflow_tool", Some("recent")) => {
                    serde_json::json!({"workflows":[{"workflow_id":"wf-fast","status":"sending"}]})
                }
                ("laser_workflow_tool", Some("status")) => {
                    serde_json::json!({"workflow_id":"wf-fast","status":"sending","input":{"material":"椴木","thickness_mm":3},"send_result":{"lines_sent":40,"total_lines":100},"unused":"x".repeat(2000)})
                }
                ("material_params_tool", _) => {
                    serde_json::json!({"laser_max_power":400,"feed_rate":1200,"passes":2})
                }
                _ => Value::Null,
            };
            crate::ToolHostResult {
                success: !self.fail,
                result: if self.fail {
                    "设备未连接".into()
                } else {
                    "已连接".into()
                },
                detail: Some(serde_json::json!({"success":!self.fail,"result":body}).to_string()),
            }
        })
    }
}

impl ToolExecutor for FakeTools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "get_time".into(),
            description: "查询时间".into(),
            parameters: serde_json::json!({"type":"object"}),
        }]
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), context.device_id.clone()));
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

fn start_chat(
    llm: FakeLlm,
    asr_replies: &[&str],
    idle: Duration,
) -> (VoiceServer, Arc<FakeRegistry>, Fakes) {
    start_chat_with(llm, asr_replies, idle, None)
}

fn start_chat_with(
    llm: FakeLlm,
    asr_replies: &[&str],
    idle: Duration,
    vision: Option<Arc<dyn crate::vision::VisionFactory>>,
) -> (VoiceServer, Arc<FakeRegistry>, Fakes) {
    let fakes = Fakes {
        asr: Arc::new(FakeAsr {
            replies: Mutex::new(
                asr_replies
                    .iter()
                    .map(|text| Ok((*text).to_owned()))
                    .collect(),
            ),
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
                (Some("tts"), Some("sentence_start")) => {
                    format!("句:{}", value["text"].as_str().unwrap())
                }
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
    send_json(
        device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    send_audio(device, version, &uplink(&tone(UP, 300.0, ms, 8000.0)));
    send_json(device, r#"{"type":"listen","state":"stop"}"#);
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

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ok() {
        assert!(Instant::now() < deadline, "等待超时：{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
