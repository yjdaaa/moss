//! n72：激光工具在场时，每轮系统提示后附“说法 → 工具”用法表；没有激光工具时不附。
use super::*;
use moss_voice_providers::BoxFuture;

struct NamedTools(Vec<&'static str>);

impl ToolExecutor for NamedTools {
    fn tools(&self) -> Vec<ToolSpec> {
        self.0
            .iter()
            .map(|name| ToolSpec {
                name: (*name).into(),
                description: "测试".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            })
            .collect()
    }
    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Reply("{}".into()) })
    }
}

fn system_prompt_with(tools: Vec<&'static str>) -> String {
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["好的。"])])),
        ..Default::default()
    });
    let conversation = Conversation::new(
        Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("你好".to_owned())])),
            ..Default::default()
        }),
        llm.clone(),
        Arc::new(FakeTts::default()),
        Arc::new(NamedTools(tools)),
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
            playback_start_ms: 80,
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = allowed_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 260);
    let _ = collect_playback(&mut device, 1);
    let seen = llm.seen.lock().unwrap();
    seen[0].0[0].content.clone().unwrap_or_default()
}

#[test]
fn laser_tool_guide_is_added_only_when_laser_tools_are_offered() {
    let with = system_prompt_with(vec!["laser_workflow_tool", "laser_safe_action_tool"]);
    assert!(with.contains("激光工具怎么选"), "{with}");
    assert!(with.contains("confirm_send") && with.contains("emergency_stop"));
    let without = system_prompt_with(vec!["get_weather"]);
    assert!(!without.contains("激光工具怎么选"));
}
