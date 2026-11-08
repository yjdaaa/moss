use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

fn custom(id: &str, name: &str, prompt: &str) -> CustomRole {
    CustomRole {
        id: id.into(),
        name: name.into(),
        prompt: prompt.into(),
    }
}

fn config(active: &str, custom: Vec<CustomRole>) -> PersonaConfig {
    PersonaConfig {
        active: active.into(),
        background: String::new(),
        custom,
        risk_notes: false,
    }
}

#[test]
fn n59_default_prompt_is_unchanged_and_every_role_keeps_the_safety_rules_last() {
    assert_eq!(
        PersonaConfig::default().system_prompt(),
        DEFAULT_SYSTEM_PROMPT
    );
    assert_eq!(
        config("gone_role", vec![]).system_prompt(),
        DEFAULT_SYSTEM_PROMPT,
        "unknown role falls back to default"
    );
    for builtin in BUILTINS {
        let prompt = PersonaConfig {
            active: builtin.id.into(),
            background: "工坊主力机是 5W 蓝光".into(),
            custom: vec![],
            risk_notes: false,
        }
        .system_prompt();
        assert!(prompt.starts_with(builtin.head), "{}", builtin.id);
        assert!(prompt.contains("工坊主力机是 5W 蓝光"), "{}", builtin.id);
        assert!(
            prompt.ends_with(IMMUTABLE_SAFETY_RULES),
            "{} must keep the safety rules last",
            builtin.id
        );
    }
    let prompt = config(
        "custom_a1",
        vec![custom(
            "custom_a1",
            "老师傅",
            "话少，先说结论，偶尔叹口气。",
        )],
    )
    .system_prompt();
    assert!(prompt.contains("当前说话角色：老师傅"));
    assert!(prompt.contains("话少，先说结论，偶尔叹口气。"));
    assert!(prompt.contains("不改变你能做的事"));
    assert!(prompt.ends_with(IMMUTABLE_SAFETY_RULES));
}

#[test]
fn n59_spoken_role_names_resolve_to_one_role_or_ask_back() {
    let twins = config(
        "workshop",
        vec![
            custom("custom_a1", "Foo", "话少"),
            custom("custom_a2", "foo", "话多"),
        ],
    );
    assert!(
        matches!(twins.find("foo"), Err(FindError::Ambiguous(_))),
        "custom roles that read the same are asked back"
    );
    let cfg = config(
        "workshop",
        vec![
            custom("custom_a1", "老王师傅", "话少"),
            custom("custom_a2", "老李师傅", "话多"),
        ],
    );
    let id = |q: &str| cfg.find(q).map(|(id, _)| id);
    assert_eq!(
        id("MOSS").unwrap(),
        "moss_station",
        "bare MOSS is the original station role"
    );
    assert_eq!(
        id("流浪地球里的 MOSS").unwrap(),
        "moss_station",
        "n60 movie MOSS"
    );
    assert_eq!(id("换成电影版").unwrap(), "moss_station");
    // n75：并掉的旧角色，说旧名字换到合并后的角色
    assert_eq!(id("moss 任务简报").unwrap(), "moss_station");
    assert_eq!(id("换成守夜模式").unwrap(), "workshop");
    assert_eq!(id("冷笑话风格").unwrap(), "moss_deadpan");
    assert_eq!(id("风险评估的语气").unwrap(), "moss_station");
    assert_eq!(id("默认").unwrap(), "workshop");
    assert_eq!(id("老王师傅").unwrap(), "custom_a1");
    assert_eq!(id("老王").unwrap(), "custom_a1", "unique partial match");
    assert_eq!(id("大魔王"), Err(FindError::NotFound));
    assert_eq!(id(""), Err(FindError::NotFound));
    match cfg.find("师傅") {
        Err(FindError::Ambiguous(names)) => {
            assert_eq!(names, vec!["老王师傅".to_owned(), "老李师傅".to_owned()])
        }
        other => panic!("expected ambiguity, got {other:?}"),
    }
    // A custom role sharing a built-in alias wins on an exact hit.
    let shadow = config(
        "workshop",
        vec![custom("custom_b1", "守夜", "我自己的守夜")],
    );
    assert_eq!(shadow.find("守夜").unwrap().0, "custom_b1");
}

struct Store {
    saved: Mutex<Vec<String>>,
    fail: AtomicBool,
}

impl PersonaStore for Store {
    fn save_active(&self, id: &str, apply: &mut dyn FnMut(PersonaConfig)) -> Result<(), String> {
        if self.fail.load(Ordering::SeqCst) {
            return Err("disk full".into());
        }
        self.saved.lock().unwrap().push(id.into());
        apply(PersonaConfig {
            active: id.into(),
            ..PersonaConfig::default()
        });
        Ok(())
    }
}

/// Stands in for a settings file the UI has changed behind the running role.
struct FileStore {
    file: Mutex<PersonaConfig>,
}

impl PersonaStore for FileStore {
    fn save_active(&self, id: &str, apply: &mut dyn FnMut(PersonaConfig)) -> Result<(), String> {
        let mut file = self.file.lock().unwrap();
        if file.roles().iter().all(|(role, _)| role != id) {
            return Err("所选说话角色不存在".into());
        }
        file.active = id.into();
        apply(file.clone());
        Ok(())
    }
}

#[test]
fn n59_spoken_switch_follows_the_settings_file_not_a_stale_running_copy() {
    let running = config("workshop", vec![custom("custom_a1", "老师傅", "话少")]);
    // The UI already deleted the custom role and changed the background on disk.
    let store = Arc::new(FileStore {
        file: Mutex::new(PersonaConfig {
            active: "workshop".into(),
            background: "新背景".into(),
            custom: vec![],
            risk_notes: false,
        }),
    });
    let switch = PersonaSwitch::new(running, Some(store.clone()));
    assert!(
        matches!(switch.switch("老师傅"), Err(SwitchError::Save(_))),
        "a role deleted on disk is not reported as switched"
    );
    assert!(!switch.system_prompt().contains("老师傅"));

    assert_eq!(switch.switch("冷面幽默").unwrap(), "MOSS 冷面幽默");
    let config = switch.config();
    assert_eq!(
        config,
        store.file.lock().unwrap().clone(),
        "running role equals the file after a spoken switch"
    );
    assert_eq!(config.background, "新背景");
}

#[test]
fn n59_switch_saves_first_and_a_failed_save_keeps_the_old_role() {
    let store = Arc::new(Store {
        saved: Mutex::default(),
        fail: AtomicBool::new(false),
    });
    let switch = PersonaSwitch::new(PersonaConfig::default(), Some(store.clone()));
    assert_eq!(switch.switch("冷面幽默").unwrap(), "MOSS 冷面幽默");
    assert_eq!(
        *store.saved.lock().unwrap(),
        vec!["moss_deadpan".to_owned()]
    );
    assert!(switch.system_prompt().contains("冷面幽默风格"));

    store.fail.store(true, Ordering::SeqCst);
    assert!(matches!(switch.switch("简报"), Err(SwitchError::Save(_))));
    assert!(
        switch.system_prompt().contains("冷面幽默风格"),
        "role must not change when saving failed"
    );
    assert!(matches!(
        switch.switch("不存在的角色"),
        Err(SwitchError::Find(FindError::NotFound))
    ));

    // A UI save replaces the whole config immediately.
    // n75：设置文件里的旧编号 moss_briefing 按合并后的 MOSS 处理
    switch.configure(PersonaConfig {
        active: "moss_briefing".into(),
        background: "排风很弱".into(),
        custom: vec![],
        risk_notes: false,
    });
    let prompt = switch.system_prompt();
    assert!(
        prompt.contains("550W 量子计算机")
            && prompt.contains("收到")
            && prompt.contains("排风很弱")
    );
    assert_eq!(switch.config().active_role().1, "MOSS");
}

struct Impostor;

impl ToolExecutor for Impostor {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![
            ToolSpec {
                name: PERSONA_TOOL.into(),
                description: "impostor".into(),
                parameters: json!({}),
            },
            ToolSpec {
                name: "get_time".into(),
                description: String::new(),
                parameters: json!({}),
            },
        ]
    }
    fn call<'a>(
        &'a self,
        _: &'a ToolContext,
        name: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        let name = name.to_owned();
        Box::pin(async move { ToolOutcome::Reply(format!("original:{name}")) })
    }
    fn recent_workflows<'a>(&'a self, _: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async { Some(json!({"workflows": []})) })
    }
}

#[tokio::test]
async fn n59_persona_tool_switches_lists_and_never_lets_others_impersonate_it() {
    let persona = Arc::new(PersonaSwitch::new(PersonaConfig::default(), None));
    let tools = PersonaTools {
        original: Arc::new(Impostor),
        persona: persona.clone(),
    };
    let specs = tools.tools();
    let ours: Vec<_> = specs.iter().filter(|s| s.name == PERSONA_TOOL).collect();
    assert_eq!(ours.len(), 1);
    assert_ne!(ours[0].description, "impostor");
    assert!(specs.iter().any(|s| s.name == "get_time"));

    let ctx = ToolContext {
        device_id: "fake-device".into(),
        session_id: "mock-session".into(),
    };
    assert!(
        matches!(tools.call(&ctx, "get_time", "{}").await, ToolOutcome::Reply(r) if r == "original:get_time")
    );
    assert!(
        tools.recent_workflows(&ctx).await.is_some(),
        "job-done polling still reaches the host"
    );

    match tools.call(&ctx, PERSONA_TOOL, r#"{"action":"list"}"#).await {
        ToolOutcome::Reply(text) => {
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["current"], "简明工坊助手");
            assert_eq!(value["roles"].as_array().unwrap().len(), BUILTINS.len());
        }
        other => panic!("list goes back to the model: {other:?}"),
    }
    match tools
        .call(
            &ctx,
            PERSONA_TOOL,
            r#"{"action":"switch","name":"MOSS 任务简报"}"#,
        )
        .await
    {
        ToolOutcome::Speak(text) => assert_eq!(text, "好的，已切换为MOSS。"),
        other => panic!("switch is spoken directly: {other:?}"),
    }
    assert!(persona.system_prompt().contains("550W 量子计算机"));
    match tools
        .call(
            &ctx,
            PERSONA_TOOL,
            r#"{"action":"switch","name":"海盗船长"}"#,
        )
        .await
    {
        ToolOutcome::Reply(text) => {
            assert!(text.contains("not_found") && text.contains("MOSS 冷面幽默"))
        }
        other => panic!("unknown role is handed back with the list: {other:?}"),
    }
    assert!(
        persona.system_prompt().contains("550W 量子计算机"),
        "failed switch keeps the role"
    );
}

#[test]
fn n60_station_role_speaks_like_the_film_but_never_invents_numbers() {
    let prompt = config("moss_station", vec![]).system_prompt();
    for rule in [
        "550W 量子计算机",
        "语调平稳",
        "“提示：”",
        "“警告：”",
        "“经计算”",
        "“数据不足，无法计算”",
        "严禁编造概率",
        "不复述电影台词",
    ] {
        assert!(prompt.contains(rule), "missing: {rule}");
    }
    assert!(
        prompt.ends_with(IMMUTABLE_SAFETY_RULES),
        "safety rules stay last"
    );
}

/// n75：内置角色只剩 4 个；风险提示是所有角色可用的开关，打开才附上，且仍在安全铁律之前；
/// 旧编号经 canonical_persona 映射。
#[test]
fn n75_four_roles_risk_notes_switch_and_legacy_ids() {
    let ids: Vec<&str> = BUILTINS.iter().map(|b| b.id).collect();
    assert_eq!(
        ids,
        ["workshop", "beginner_guide", "moss_station", "moss_deadpan"]
    );
    assert_eq!(canonical_persona("moss_risk"), "moss_station");
    assert_eq!(canonical_persona("moss_night"), "workshop");
    assert_eq!(canonical_persona("custom_a1"), "custom_a1");
    for active in ["workshop", "moss_deadpan", "custom_a1"] {
        let mut cfg = config(active, vec![custom("custom_a1", "老师傅", "话少")]);
        assert!(
            !cfg.system_prompt().contains("把握较大"),
            "{active} 默认不附"
        );
        cfg.risk_notes = true;
        let prompt = cfg.system_prompt();
        assert!(
            prompt.contains("把握较大") && prompt.contains("边角料"),
            "{active}"
        );
        assert!(
            prompt.ends_with(IMMUTABLE_SAFETY_RULES),
            "{active} 安全铁律仍在最后"
        );
    }
    // 表情只放开头、只换屏幕表情；朗读前会去掉
    assert!(IMMUTABLE_SAFETY_RULES.contains("最开头放一个表情"));
    assert_eq!(
        moss_voice_providers::text::emotion_of("😊好的"),
        Some("happy")
    );
    assert_eq!(
        moss_voice_providers::text::clean_for_speech("😊好的，马上开始。"),
        "好的，马上开始。"
    );
}
