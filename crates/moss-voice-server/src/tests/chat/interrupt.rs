//! n114：工具开跑之后的打断与“停”——被打断时工具补记进历史，工具执行中自动拾音照样听“停”。
use super::*;

/// 慢工具：`slow_tool` 要跑一会儿（模拟生成、提交这类在后台线程里跑、打断停不下的工具），其余立即返回。
#[derive(Default)]
struct SlowTools {
    calls: Mutex<Vec<String>>,
}

impl ToolExecutor for SlowTools {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "slow_tool".into(),
            description: "慢工具".into(),
            parameters: serde_json::json!({"type": "object"}),
        }]
    }
    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        name: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(name.to_owned());
            if name == "slow_tool" {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            ToolOutcome::Reply(r#"{"success":true,"result":"ok"}"#.into())
        })
    }
}

fn start_with_slow_tools(
    llm: Arc<FakeLlm>,
    asr: &[&str],
) -> (VoiceServer, Arc<FakeRegistry>, Arc<SlowTools>) {
    let tools = Arc::new(SlowTools::default());
    let conversation = Conversation::new(
        Arc::new(FakeAsr {
            replies: Mutex::new(asr.iter().map(|text| Ok((*text).to_owned())).collect()),
            heard: Mutex::default(),
        }),
        llm,
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
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry, tools)
}

fn wait_for_tool(tools: &SlowTools) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while tools.calls.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!tools.calls.lock().unwrap().is_empty(), "慢工具应已开跑");
}

/// 工具跑到一半用户按键重新说话：这一轮被打断，但工具调用照样补记进历史（结果还没回来就写“结果未知、别重复执行”），
/// 下一轮大模型知道它已经开始执行过。
#[test]
fn a_turn_interrupted_while_its_tool_runs_still_records_the_tool_call() {
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([call("slow_tool"), say(&["已经在做了。"])])),
        ..FakeLlm::default()
    });
    let (server, registry, tools) =
        start_with_slow_tools(llm.clone(), &["帮我做那件慢的事", "开始了吗"]);
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 600);
    wait_for_tool(&tools);
    // 工具还在跑：按键重新说话
    speak_manual(&mut device, 1, 600);
    let items = texts(&collect_items(&mut device));
    assert!(items.contains(&"句:已经在做了。".to_owned()), "{items:?}");
    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let second = &seen[1].0;
    let at = second
        .iter()
        .position(|m| m.role == Role::User && m.content.as_deref() == Some("帮我做那件慢的事"))
        .expect("被打断那一轮的原话记在历史里");
    assert_eq!(second[at + 1].tool_calls.len(), 1, "{second:?}");
    assert_eq!(second[at + 1].tool_calls[0].function.name, "slow_tool");
    assert_eq!(second[at + 2].role, Role::Tool);
    assert_eq!(
        second[at + 2].content.as_deref(),
        Some(crate::conversation::INTERRUPTED_TOOL_RESULT)
    );
    assert_eq!(
        tools.calls.lock().unwrap().len(),
        1,
        "被打断的工具不会因为补记历史而再执行一遍"
    );
}

/// 自动拾音、没开实时插话：工具执行中（还没开口）用户说“停”——原先这段音频整段丢掉，现在听出来就急停。
#[test]
fn saying_stop_while_a_tool_runs_stops_even_without_realtime_barge_in() {
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([call("slow_tool")])),
        ..FakeLlm::default()
    });
    let (server, registry, tools) = start_with_slow_tools(llm, &["帮我做那件慢的事", "停"]);
    let mut device = chat_device(&server, &registry, 1);
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let mut first = tone(UP, 300.0, 600, 8000.0);
    first.extend(silence(UP, 700));
    send_audio(&mut device, 1, &uplink(&first));
    wait_for_tool(&tools);
    // 设备按实时节奏继续上传：一声“停”，之后是安静
    let mut second = tone(UP, 300.0, 400, 8000.0);
    second.extend(silence(UP, 900));
    for chunk in uplink(&second).chunks(3) {
        send_audio(&mut device, 1, chunk);
        std::thread::sleep(Duration::from_millis(60));
    }
    let items = texts(&collect_items(&mut device));
    assert!(items.contains(&"听:停".to_owned()), "{items:?}");
    // 急停播报（成功或失败都算：测试工具不是真激光机）
    assert!(
        items
            .iter()
            .any(|text| text.starts_with("句:已发送停止请求")
                || text.starts_with("句:停止请求发送失败")),
        "{items:?}"
    );
}
