//! 流式识别与流式合成：边说边识别、边合成边播、下一句预取、主合成失败回退本地。
use super::*;

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
            Ok(Pcm {
                samples: tone(24_000, 300.0, self.chunk_ms * self.chunks as u32, 6000.0),
                sample_rate: 24_000,
            })
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
                let _ = chunks.send(Pcm {
                    samples: tone(24_000, 300.0, self.chunk_ms, 6000.0),
                    sample_rate: 24_000,
                });
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
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("整段识别".to_owned())])),
        heard: Mutex::default(),
    });
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
    let (server, registry, asr) =
        start_streaming_chat(streaming.clone(), Arc::new(FakeTts::default()), 600);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"manual"}"#,
    );
    send_audio(&mut device, 1, &uplink(&tone(UP, 300.0, 1000, 8000.0)));
    // 还没松开按键：音频已经送到识别
    let deadline = Instant::now() + Duration::from_secs(3);
    while streaming.received().first().copied().unwrap_or(0) < 17 * 960 && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(streaming.received(), vec![17 * 960], "说话期间收到全部音频");
    send_json(&mut device, r#"{"type":"listen","state":"stop"}"#);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:打开激光", "tts:start", "句:好的。", "tts:stop"]
    );
    assert!(
        asr.heard.lock().unwrap().is_empty(),
        "流式成功时不再整段识别"
    );
}

/// NV7：自动模式只在 VAD 确认开始说话后才建立识别（环境静音不建连接），并补送说话前保留的音频。
#[test]
fn auto_mode_opens_streaming_recognition_only_when_speech_starts() {
    let streaming = FakeStreamingAsr::new(Ok("开灯"));
    let (server, registry, _asr) =
        start_streaming_chat(streaming.clone(), Arc::new(FakeTts::default()), 600);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    send_audio(&mut device, 1, &uplink(&silence(UP, 900)));
    std::thread::sleep(Duration::from_millis(300));
    assert!(streaming.received().is_empty(), "只有静音时不建立识别");
    let mut pcm = tone(UP, 300.0, 600, 8000.0);
    pcm.extend(silence(UP, 600));
    send_audio(&mut device, 1, &uplink(&pcm));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:开灯", "tts:start", "句:好的。", "tts:stop"]
    );
    let received = streaming.received();
    assert_eq!(received.len(), 1, "一段话只建立一次识别");
    assert!(
        received[0] >= UP as usize * 600 / 1000 + UP as usize * 200 / 1000,
        "含说话前保留的音频：{received:?}"
    );
}

/// NV7：流式识别失败时用整段音频再识别一次，并记录原因。
#[test]
fn failed_streaming_recognition_falls_back_to_whole_utterance() {
    let streaming = FakeStreamingAsr::new(Err("豆包识别连接被拒绝（HTTP 401）"));
    let (server, registry, asr) =
        start_streaming_chat(streaming, Arc::new(FakeTts::default()), 600);
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:整段识别", "tts:start", "句:好的。", "tts:stop"]
    );
    assert_eq!(asr.heard.lock().unwrap().len(), 1);
    assert!(
        server.status().events.iter().any(|event| event
            .text
            .contains("流式识别失败，改用整段识别：豆包识别连接被拒绝")
            && event.level == crate::EventLevel::Warn),
        "已自动补救的失败记为警告：{:?}",
        server.status().events
    );
}

/// NV7：流式合成的第一段一到就开始播放（不等整句合成完），句子文字只发一次，分段之间连续不中断。
#[test]
fn streamed_synthesis_starts_playing_before_the_sentence_is_finished() {
    let tts = Arc::new(ChunkedTts {
        chunks: 4,
        chunk_ms: 240,
        gap_ms: 150,
        finished_at: Mutex::new(None),
    });
    let (server, registry, _asr) =
        start_streaming_chat(FakeStreamingAsr::new(Ok("你好")), tts.clone(), 300);
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
    assert_eq!(
        texts(&items),
        vec!["听:你好", "tts:start", "句:好的。", "tts:stop"]
    );
    let finished = tts.finished_at.lock().unwrap().unwrap();
    assert!(
        first_frame.unwrap() < finished,
        "第一帧在整句合成完之前发出"
    );
    // 4 段 × 240 ms = 960 ms → 16 帧；合成快于实时，不应出现供流中断
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(item, Item::Frame))
            .count(),
        16
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|event| event.text.contains("供流中断 0 次")),
        "{:?}",
        server.status().events
    );
}

#[test]
fn second_sentence_tts_prefetches_concurrently_while_first_sentence_is_streaming() {
    struct OverlapTts {
        events: Mutex<Vec<String>>,
    }
    impl Tts for OverlapTts {
        fn synthesize<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
            Box::pin(async move {
                Ok(Pcm {
                    samples: tone(16_000, 300.0, 200, 6000.0),
                    sample_rate: 16_000,
                })
            })
        }
        fn synthesize_stream<'a>(
            &'a self,
            text: &'a str,
            chunks: tokio::sync::mpsc::UnboundedSender<Pcm>,
        ) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async move {
                self.events.lock().unwrap().push(format!("start:{text}"));
                let _ = chunks.send(Pcm {
                    samples: tone(16_000, 300.0, 120, 6000.0),
                    sample_rate: 16_000,
                });
                tokio::time::sleep(Duration::from_millis(80)).await;
                let mut second_chunk = tone(16_000, 300.0, 120, 6000.0);
                // 附带 250ms 句尾静音，验证自动裁剪不会破坏播放
                second_chunk.extend(silence(16_000, 250));
                let _ = chunks.send(Pcm {
                    samples: second_chunk,
                    sample_rate: 16_000,
                });
                self.events.lock().unwrap().push(format!("end:{text}"));
                Ok(())
            })
        }
    }

    let tts = Arc::new(OverlapTts {
        events: Mutex::new(Vec::new()),
    });
    let llm = Arc::new(FakeLlm {
        // 首句满 6 字切出保证连续缓冲；后续 1 字碎句 "嗯。" 自动与下一段合并为 "嗯。设备状态一切正常。"
        rounds: Mutex::new(VecDeque::from([say(&[
            "收到，正在检查，",
            "嗯。",
            "设备状态一切正常。",
        ])])),
        ..FakeLlm::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("检查状态".to_owned())])),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        llm,
        tts.clone(),
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
            playback_start_ms: 120,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let items = collect_items(&mut device);
    assert_eq!(
        texts(&items),
        vec![
            "听:检查状态",
            "tts:start",
            "句:收到，正在检查，",
            "句:嗯。设备状态一切正常。",
            "tts:stop",
        ]
    );
    let log = tts.events.lock().unwrap().clone();
    let start_second = log
        .iter()
        .position(|e| e == "start:嗯。设备状态一切正常。")
        .expect("第二句应启动 TTS");
    let end_first = log
        .iter()
        .position(|e| e == "end:收到，正在检查，")
        .expect("第一句应完成 TTS");
    assert!(
        start_second < end_first,
        "第二句 TTS 必须在第一句流式合成结束前已并发预启动，实际顺序: {log:?}"
    );
}

#[test]
fn primary_tts_failure_automatically_falls_back_to_local_tts_without_dropping_sentence() {
    // FakeTts 在句子含“失败”时返回 Err("语音合成返回 HTTP 500")；
    // 挂载 fallback_tts 后，含“失败”的句子自动由本地降级 TTS 合成出音频并正常播报，不会退化为纯文本静音。
    struct LocalFallbackFakeTts {
        fallback_said: Mutex<Vec<String>>,
    }
    impl Tts for LocalFallbackFakeTts {
        fn synthesize<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
            Box::pin(async move {
                self.fallback_said.lock().unwrap().push(text.to_owned());
                Ok(Pcm {
                    samples: tone(24_000, 300.0, 240, 6000.0),
                    sample_rate: 24_000,
                })
            })
        }
    }

    let primary_tts = Arc::new(FakeTts::default());
    let fallback_tts = Arc::new(LocalFallbackFakeTts {
        fallback_said: Mutex::new(Vec::new()),
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&[
            "云端本句合成失败，",
            "第二句恢复正常。",
        ])])),
        ..FakeLlm::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("测试降级".to_owned())])),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        llm,
        primary_tts.clone(),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    )
    .with_fallback_tts(fallback_tts.clone());
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            playback_start_ms: 120,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let items = collect_items(&mut device);
    assert_eq!(
        texts(&items),
        vec![
            "听:测试降级",
            "tts:start",
            "句:云端本句合成失败，",
            "句:第二句恢复正常。",
            "tts:stop",
        ]
    );
    assert_eq!(
        *fallback_tts.fallback_said.lock().unwrap(),
        vec!["云端本句合成失败，"],
        "只有主 TTS 失败的那一句走本地降级 TTS，恢复正常的后续句子继续走主 TTS"
    );
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|e| e.text.contains("本句已自动降级为本地语音合成")),
        "事件记录应明确提示发生了本地 TTS 自动降级"
    );
}

/// 一句合成要 400 ms 的合成（模拟慢一点的本地 / 云端合成）
struct SlowTts;
impl Tts for SlowTts {
    fn synthesize<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            Ok(Pcm {
                samples: tone(24_000, 300.0, 400, 6000.0),
                sample_rate: 24_000,
            })
        })
    }
}

/// n115：大模型先说开场白、同时调工具——工具不等开场白合成完就开跑；开场白照样先于回答播出。
#[tokio::test]
async fn tools_start_without_waiting_for_the_preamble_to_be_synthesized() {
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    #[derive(Default)]
    struct StampTools {
        started: Mutex<Option<std::time::Instant>>,
    }
    impl ToolExecutor for StampTools {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec {
                name: "get_time".into(),
                description: "查询时间".into(),
                parameters: serde_json::json!({"type": "object"}),
            }]
        }
        fn call<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a str,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move {
                *self.started.lock().unwrap() = Some(std::time::Instant::now());
                ToolOutcome::Reply("12:00".into())
            })
        }
    }
    let tools = Arc::new(StampTools::default());
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: vec!["好，我查一下。"],
                calls: vec![ToolCall::new("call-time", "get_time", "{}")],
                delay_ms: 0,
            },
            say(&["现在十二点。"]),
        ])),
        ..FakeLlm::default()
    });
    let conversation = Arc::new(Conversation::new(
        Arc::new(FakeAsr::default()),
        llm,
        Arc::new(SlowTts),
        tools.clone(),
        ConversationSettings::default(),
    ));
    let (handle, mut rx) = spawn_turn(
        conversation,
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
    let mut spoken = Vec::new();
    let mut preamble_ready = None;
    let mut history = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            TurnEvent::Speech { text, .. } => {
                if spoken.is_empty() {
                    preamble_ready = Some(std::time::Instant::now());
                }
                spoken.push(text);
            }
            TurnEvent::Done { messages, .. } => history = messages,
            _ => {}
        }
    }
    handle.await.unwrap();
    assert_eq!(spoken, ["好，我查一下。", "现在十二点。"]);
    let started = tools.started.lock().unwrap().expect("工具执行了");
    assert!(
        started < preamble_ready.unwrap(),
        "工具应在开场白合成完之前就开跑"
    );
    // 历史照常：开场白随工具调用记下，工具结果配对，最后是回答
    let roles: Vec<_> = history.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [Role::User, Role::Assistant, Role::Tool, Role::Assistant],
        "{history:?}"
    );
}

/// n115：音频还没到就先发 `tts start`，固件切到说话状态的保护间隔和合成时间重叠——
/// 音频一合成好就能起播，不用再等 180 ms。
#[test]
fn tts_start_goes_out_while_the_first_sentence_is_still_synthesizing() {
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["好的。"])])),
        ..FakeLlm::default()
    });
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        llm,
        Arc::new(SlowTts),
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
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    let mut start_at = None;
    let mut first_frame_at = None;
    loop {
        match device.read().unwrap() {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["type"] == "tts" && value["state"] == "start" {
                    start_at.get_or_insert_with(Instant::now);
                }
                if value["type"] == "tts" && value["state"] == "stop" {
                    break;
                }
            }
            Message::Binary(_) => {
                first_frame_at.get_or_insert_with(Instant::now);
            }
            _ => {}
        }
    }
    let gap = first_frame_at.unwrap() - start_at.unwrap();
    // 合成要 400 ms：`tts start` 在合成开始时就发出，首帧约 400 ms 后到；原先音频到了才发 start，间隔只有 180 ms
    assert!(gap >= Duration::from_millis(300), "{gap:?}");
}

/// n114：推理模型把思考过程用 `<think>` 夹在正文里——不念、不进历史；标签被拆在两段流式文字里也一样。
#[tokio::test]
async fn think_blocks_in_the_reply_are_neither_spoken_nor_kept_in_history() {
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&[
            "<thi",
            "nk>用户想知道机器状态</think>",
            "好，",
            "现在空闲。",
        ])])),
        ..FakeLlm::default()
    });
    let conversation = Arc::new(Conversation::new(
        Arc::new(FakeAsr::default()),
        llm,
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    ));
    let (handle, mut rx) = spawn_turn(
        conversation,
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
    let (mut spoken, mut history) = (Vec::new(), Vec::new());
    while let Some(event) = rx.recv().await {
        match event {
            TurnEvent::Speech { text, .. } => spoken.push(text),
            TurnEvent::Done { messages, .. } => history = messages,
            _ => {}
        }
    }
    handle.await.unwrap();
    assert_eq!(spoken.concat(), "好，现在空闲。");
    let answer = history.iter().rfind(|m| m.role == Role::Assistant).unwrap();
    assert_eq!(answer.content.as_deref(), Some("好，现在空闲。"));
}

/// n114：大模型说到一半出错——已经说出去的半句照样记进历史，下一轮它知道自己说过什么。
#[tokio::test]
async fn a_reply_cut_by_an_llm_error_keeps_the_spoken_half_in_history() {
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    struct BreaksMidway;
    impl Llm for BreaksMidway {
        fn chat<'a>(
            &'a self,
            _: &'a [ChatMessage],
            _: &'a [ToolSpec],
            deltas: tokio::sync::mpsc::UnboundedSender<String>,
        ) -> BoxFuture<'a, Result<LlmReply, ProviderError>> {
            Box::pin(async move {
                let _ = deltas.send("好，激光机现在空闲。".into());
                let _ = deltas.send("材料".into());
                Err(ProviderError::new("连接中断"))
            })
        }
    }
    let conversation = Arc::new(Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(BreaksMidway),
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    ));
    let (handle, mut rx) = spawn_turn(
        conversation,
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
    let mut history = Vec::new();
    while let Some(event) = rx.recv().await {
        if let TurnEvent::Done { messages, .. } = event {
            history = messages;
        }
    }
    handle.await.unwrap();
    let answer = history.iter().rfind(|m| m.role == Role::Assistant);
    assert_eq!(
        answer.and_then(|m| m.content.as_deref()),
        Some("好，激光机现在空闲。材料"),
        "{history:?}"
    );
}

/// n114：合成失败又没有备用合成——这句至少把文字发给设备（原先设备什么都收不到，历史里却记着说过）；
/// 前面的句子已经在播时不发 `tts stop`，不让设备丢掉排着的音频。
#[test]
fn a_sentence_whose_synthesis_fails_is_still_shown_as_text() {
    for (deltas, expect) in [
        (
            vec!["这句合成失败了。"],
            vec!["听:你好", "tts:start", "句:这句合成失败了。", "tts:stop"],
        ),
        (
            vec!["第一句正常。", "第二句合成失败。"],
            vec!["听:你好", "tts:start", "句:第一句正常。", "tts:stop"],
        ),
    ] {
        let llm = FakeLlm {
            rounds: Mutex::new(VecDeque::from([say(&deltas)])),
            ..FakeLlm::default()
        };
        let (server, registry, _fakes) = start_chat(llm, &[], Duration::from_secs(30));
        let mut device = chat_device(&server, &registry, 1);
        speak_manual(&mut device, 1, 600);
        let items = collect_items(&mut device);
        assert_eq!(texts(&items), expect);
        if deltas.len() == 2 {
            assert!(
                items.iter().any(|item| matches!(item, Item::Frame)),
                "第一句的音频照常播完"
            );
        }
    }
}

/// n114：主合成失败时备用合成出的固定话术只用这一次、不进缓存——否则启动预热时云端还连不上，
/// 固定话术会一直是备用音色；主合成恢复后照常缓存。
#[tokio::test]
async fn fixed_phrases_from_the_fallback_voice_are_not_cached() {
    struct Fallback;
    impl Tts for Fallback {
        fn synthesize<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
            Box::pin(async {
                Ok(Pcm {
                    samples: tone(24_000, 300.0, 240, 6000.0),
                    sample_rate: 24_000,
                })
            })
        }
    }
    let primary = Arc::new(FakeTts::default());
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(FakeLlm::default()),
        primary.clone(),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    )
    .with_fallback_tts(Arc::new(Fallback));
    // FakeTts 对含“失败”的句子报错：这句由备用合成说出，但不缓存
    conversation
        .prewarm_or_speak_for_test("合成失败的固定话术")
        .await;
    assert_eq!(conversation.speech_cache_len_for_test(), 0);
    conversation
        .prewarm_or_speak_for_test("合成失败的固定话术")
        .await;
    assert_eq!(primary.said.lock().unwrap().len(), 2, "下次照样先试主合成");
    conversation.prewarm_or_speak_for_test("我在").await;
    assert_eq!(conversation.speech_cache_len_for_test(), 1);
}

#[tokio::test]
async fn streaming_tts_preserves_mid_sentence_pause_and_trims_only_true_sentence_tail() {
    use crate::audio::test_signal::{silence, tone};
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};

    struct ChunkedPauseTts;
    impl Tts for ChunkedPauseTts {
        fn synthesize<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Pcm, ProviderError>> {
            Box::pin(async {
                Ok(Pcm {
                    samples: tone(DOWN, 440.0, 100, 6000.0),
                    sample_rate: DOWN,
                })
            })
        }

        fn synthesize_stream<'a>(
            &'a self,
            _text: &'a str,
            chunks: tokio::sync::mpsc::UnboundedSender<Pcm>,
        ) -> BoxFuture<'a, Result<(), ProviderError>> {
            Box::pin(async move {
                // 分块 1：100ms 语音 + 300ms 句中停顿（旧代码会在此误触发句尾裁静音把 300ms 砍成 140ms）
                let mut c1 = tone(DOWN, 440.0, 100, 6000.0);
                c1.extend(silence(DOWN, 300));
                let _ = chunks.send(Pcm {
                    samples: c1,
                    sample_rate: DOWN,
                });
                // 分块 2：100ms 后续语音 + 180ms 句尾静音（与分块 3 拼成 360ms 句尾死寂）
                let mut c2 = tone(DOWN, 440.0, 100, 6000.0);
                c2.extend(silence(DOWN, 180));
                let _ = chunks.send(Pcm {
                    samples: c2,
                    sample_rate: DOWN,
                });
                // 分块 3..=11：9 个 80ms 细碎纯静音包（与分块 2 合计 900ms > 512ms 超长句尾死寂，验证跨多包累计不漏放）
                for _ in 0..9 {
                    let _ = chunks.send(Pcm {
                        samples: silence(DOWN, 80),
                        sample_rate: DOWN,
                    });
                }
                Ok(())
            })
        }
    }

    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["这是一句带停顿的话。"])])),
        ..FakeLlm::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("测试流式停顿".into())])),
        ..Default::default()
    });
    let conversation = Arc::new(Conversation::new(
        asr,
        llm,
        Arc::new(ChunkedPauseTts),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    ));
    let ctx = crate::ToolContext {
        device_id: "dev-pause".into(),
        session_id: "sess-pause".into(),
    };
    let (handle, mut rx) = spawn_turn(
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
    let mut total_samples = 0usize;
    let mut sentence_ends = 0usize;
    while let Some(ev) = rx.recv().await {
        match ev {
            TurnEvent::Speech { pcm, .. } | TurnEvent::SpeechMore(pcm) => {
                total_samples += pcm.samples.len();
            }
            TurnEvent::SentenceEnd => sentence_ends += 1,
            _ => {}
        }
    }
    handle.await.unwrap();
    assert_eq!(
        sentence_ends, 1,
        "句子结束应发出 SentenceEnd 供节奏泵冲刷句尾残帧"
    );
    // 期望总长：100ms 语音 + 300ms 句中停顿（完整保留！）+ 100ms 语音 + 140ms 裁剪后句尾气口 = 640ms（15360 样本）
    let expected_ms = (total_samples * 1000) / DOWN as usize;
    assert!(
        (630..=650).contains(&expected_ms),
        "句中 300ms 停顿必须完整保留且句尾 900ms 跨块死寂裁剪至 140ms（期望约 640ms，实际 {expected_ms}ms / {total_samples} 样本）"
    );
}
