//! n100：前台陪聊、后台干活——只读慢查询超过 1 秒转到后台，用户可以接着聊，查好了小智主动播报。
use super::*;

/// 联网搜索与两个第三方工具都要 1.5 秒；其他工具立即返回。
/// `notes__search` 是用户设为“允许”（可信只读）的第三方工具，`notes__write` 不是。
struct SlowSearch {
    calls: Mutex<Vec<String>>,
}

impl ToolExecutor for SlowSearch {
    fn tools(&self) -> Vec<ToolSpec> {
        ["web_search", "notes__search", "notes__write"]
            .into_iter()
            .map(|name| ToolSpec {
                name: name.into(),
                description: "测试工具".into(),
                parameters: serde_json::json!({"type": "object"}),
            })
            .collect()
    }

    fn trusted_read_only(&self, name: &str) -> bool {
        name == "notes__search"
    }

    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        name: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(name.to_owned());
            if matches!(name, "web_search" | "notes__search" | "notes__write") {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                return ToolOutcome::Reply(if name == "web_search" {
                    "晴，25度".into()
                } else {
                    "笔记里写着：椴木 3 毫米用 80% 功率".into()
                });
            }
            ToolOutcome::Reply("ok".into())
        })
    }
}

fn start_background(
    heard: &[&str],
    rounds: Vec<Round>,
) -> (
    VoiceServer,
    Arc<FakeRegistry>,
    Arc<FakeLlm>,
    Arc<SlowSearch>,
) {
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(heard.iter().map(|text| Ok((*text).to_owned())).collect()),
        ..Default::default()
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(rounds.into()),
        ..Default::default()
    });
    let tools = Arc::new(SlowSearch {
        calls: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        llm.clone(),
        Arc::new(FakeTts::default()),
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
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry, llm, tools)
}

#[test]
fn a_slow_search_runs_in_the_background_while_the_user_keeps_chatting() {
    let (server, registry, llm, tools) = start_background(
        &["帮我查一下今天天气", "讲个笑话"],
        vec![
            call("web_search"),
            say(&["好的笑话。"]),
            say(&["刚才查的天气有结果了，晴，25度。"]),
        ],
    );
    let mut device = chat_device(&server, &registry, 1);
    let listen = r#"{"type":"listen","state":"start","mode":"auto"}"#;
    let mut speech = tone(UP, 300.0, 600, 8000.0);
    speech.extend(silence(UP, 700));
    send_json(&mut device, listen);
    send_audio(&mut device, 1, &uplink(&speech));
    // 查询 1 秒没回：先说一句就结束这一轮，不等结果
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "听:帮我查一下今天天气",
            "tts:start",
            "句:我在后台查着，查好了告诉你。",
            "tts:stop"
        ]
    );
    // 后台还在查，用户接着聊别的：照常回答
    send_json(&mut device, listen);
    send_audio(&mut device, 1, &uplink(&speech));
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["听:讲个笑话", "tts:start", "句:好的笑话。", "tts:stop"]
    );
    // 查好了：小智主动开口播报（不需要用户再说话）
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec![
            "tts:start",
            "句:刚才查的天气有结果了，",
            "句:晴，25度。",
            "tts:stop"
        ]
    );
    assert_eq!(
        *tools.calls.lock().unwrap(),
        vec!["web_search"],
        "工具只执行一次"
    );
    let seen = llm.seen.lock().unwrap();
    let (report, tool_count) = &seen[2];
    assert_eq!(*tool_count, 0, "播报这一轮不给工具");
    assert!(
        report.iter().any(|message| message
            .content
            .as_deref()
            .is_some_and(|text| text.contains("用户之前问：帮我查一下今天天气")
                && text.contains("[web_search] 晴，25度"))),
        "{report:?}"
    );
    // 第二轮的历史里，转后台的那次调用有配对的工具结果
    let (second, _) = &seen[1];
    assert!(second.iter().any(|message| message.role == Role::Tool
        && message.content.as_deref() == Some("已转到后台查询，结果出来后会主动告诉用户")));
}

/// 审查修正：播报正在请求大模型（还没开口）时用户开口说话——这句话不能丢，播报让路，之后重新播报。
#[test]
fn a_report_talked_over_before_it_speaks_yields_and_is_delivered_later() {
    let (server, registry, _llm, _) = start_background(
        &["帮我查一下今天天气", "讲个笑话"],
        vec![
            call("web_search"),
            // 第一次播报：大模型慢，用户在它开口前就说话了（这一轮被放弃）
            Round::Reply {
                deltas: vec!["刚才查的天气有结果了。"],
                calls: Vec::new(),
                delay_ms: 1500,
            },
            say(&["好的笑话。"]),
            say(&["晴，25度。"]),
        ],
    );
    let mut device = chat_device(&server, &registry, 1);
    let listen = r#"{"type":"listen","state":"start","mode":"auto"}"#;
    let mut speech = tone(UP, 300.0, 600, 8000.0);
    speech.extend(silence(UP, 700));
    send_json(&mut device, listen);
    send_audio(&mut device, 1, &uplink(&speech));
    collect_items(&mut device);
    // 固件播完重新拾音；等播报开始请求大模型
    send_json(&mut device, listen);
    let deadline = Instant::now() + Duration::from_secs(6);
    while !server
        .status()
        .events
        .iter()
        .any(|event| event.text == "播报后台查询结果")
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    send_audio(&mut device, 1, &uplink(&speech));
    let mut items = texts(&collect_items(&mut device));
    if !items.contains(&"句:好的笑话。".into()) {
        // 播报让路时可能先发一个 tts stop（设备不在说话状态时固件忽略它）
        items = texts(&collect_items(&mut device));
    }
    assert!(
        items.contains(&"听:讲个笑话".into()),
        "用户的话没丢：{items:?}"
    );
    assert!(items.contains(&"句:好的笑话。".into()), "{items:?}");
    assert!(!items.iter().any(|text| text.contains("刚才查的天气")));
    // 之后重新播报
    assert_eq!(
        texts(&collect_items(&mut device)),
        vec!["tts:start", "句:晴，25度。", "tts:stop"]
    );
}

/// n101：用户设为“允许”（可信只读）的第三方工具慢了也转后台；没设“允许”的第三方工具照旧在这一轮里等结果。
#[test]
fn only_user_trusted_third_party_tools_go_to_the_background() {
    for (tool, background) in [("notes__search", true), ("notes__write", false)] {
        let (server, registry, _, tools) = start_background(
            &["查一下笔记里椴木用多大功率"],
            vec![call(tool), say(&["查到了，八十的功率。"])],
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
        assert_eq!(
            items.contains(&"句:我在后台查着，查好了告诉你。".into()),
            background,
            "{tool}: {items:?}"
        );
        if !background {
            // 不可信的工具：这一轮等它返回再回答（期间放的是原来的慢查询垫音之外的正常回答）
            assert!(
                items.iter().any(|text| text.starts_with("句:查到了")),
                "{items:?}"
            );
        }
        assert_eq!(*tools.calls.lock().unwrap(), vec![tool.to_owned()]);
    }
}
