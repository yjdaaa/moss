//! 上下文与人设：NV19 记忆检索注入、口头切换人设、人设风格与识别纠错端到端。
use super::*;

/// NV19-04: the optional retrieval hook injects labelled memory for real questions only;
/// small talk still reaches the model without a search, stop words never reach either.
#[tokio::test]
async fn nv19_context_retrieval_hook_injects_memory_only_for_questions() {
    use crate::context::fake_store::FakeStore;
    use crate::context::retrieve::{ContextAccessProvider, ContextRetrieval, ContextRetriever};
    use crate::context::service::ContextService;
    use crate::context::{
        AccessContext, Applicability, Category, MemoryDraft, ScopeKey, ScopeKind, Source,
        SourceKind, TrustedScope, Verification,
    };
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    use crate::person_context::PersonScope;

    struct Access;
    impl ContextAccessProvider for Access {
        fn access(
            &self,
            context: &ToolContext,
            _person: Option<&PersonScope>,
        ) -> Option<(AccessContext, Option<Applicability>)> {
            let device = TrustedScope::device_grant(&context.device_id, 1).ok()?;
            Some((
                AccessContext::voice(device, None, None).ok()?,
                Applicability::new("MOSS-550W", Some("1.2.0")).ok(),
            ))
        }
    }
    let service = ContextService::new(Arc::new(FakeStore::default()));
    let owner =
        AccessContext::desktop_private(vec![TrustedScope::device_grant("fake-device", 1).unwrap()]);
    service
        .create(
            &owner,
            MemoryDraft {
                scope: ScopeKey::new(ScopeKind::Device, "fake-device").unwrap(),
                category: Category::TroubleshootingExperience,
                content: "边缘发黑时降低功率后改善".into(),
                verification: Verification::Confirmed,
                source: Source::new(SourceKind::UserEdit, None).unwrap(),
                applicability: Some(Applicability::new("MOSS-550W", Some("1.2.0")).unwrap()),
                expires_at: None,
            },
            1.0,
        )
        .unwrap();
    let retriever = ContextRetriever::new(service);
    retriever.set_enabled(true);

    for (text, expect_llm, expect_memory) in [
        ("上次边缘发黑是怎么解决的", true, true),
        ("你好", true, false),
        ("急停", false, false),
    ] {
        let llm = Arc::new(FakeLlm::default());
        let asr = Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok(text.into())])),
            ..Default::default()
        });
        let conversation = Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            Arc::new(FastTools::default()),
            ConversationSettings::default(),
        )
        .with_context_retrieval(ContextRetrieval {
            retriever: retriever.clone(),
            access: Arc::new(Access),
        });
        let context = ToolContext {
            device_id: "fake-device".into(),
            session_id: "fake-session".into(),
        };
        let (handle, mut events) = spawn_turn(
            Arc::new(conversation),
            context,
            TurnInput::Utterance {
                pcm: vec![0; 960],
                sample_rate: UP,
                stream: None,
            },
            vec![],
            None,
        );
        while let Some(event) = events.recv().await {
            let _: TurnEvent = event;
        }
        handle.await.unwrap();
        let seen = llm.seen.lock().unwrap();
        assert_eq!(!seen.is_empty(), expect_llm, "{text}");
        let injected = seen
            .iter()
            .flat_map(|(messages, _)| messages.iter())
            .any(|m| {
                m.role == Role::System
                    && m.content
                        .as_deref()
                        .is_some_and(|c| c.contains("长期记忆检索结果") && c.contains("降低功率"))
            });
        assert_eq!(injected, expect_memory, "{text}");
    }
    assert_eq!(retriever.searches(), 1, "只有真正的问题会查库");
}

/// n57: once structured retrieval is on, the old NV10 per-device summary is not injected.
#[tokio::test]
async fn nv19_retrieval_on_replaces_the_nv10_device_summary() {
    use crate::context::fake_store::FakeStore;
    use crate::context::retrieve::{ContextAccessProvider, ContextRetrieval, ContextRetriever};
    use crate::context::service::ContextService;
    use crate::context::{AccessContext, Applicability, TrustedScope};
    use crate::conversation::{TurnEvent, TurnInput, spawn_turn};
    use crate::memory::{MemoryRecord, MemoryService, VoiceMemoryStore};
    use crate::person_context::PersonScope;

    struct Summary;
    impl VoiceMemoryStore for Summary {
        fn allowed(&self, _: &str) -> Result<bool, String> {
            Ok(true)
        }
        fn load(&self, id: &str) -> Result<Option<MemoryRecord>, String> {
            Ok(Some(MemoryRecord {
                device_id: id.into(),
                summary: "旧设备摘要：喜欢绿茶".into(),
                revision: 0,
                updated_at: 0.0,
            }))
        }
        fn ensure(&self, id: &str) -> Result<MemoryRecord, String> {
            Ok(self.load(id)?.unwrap())
        }
        fn update(&self, _: &str, _: i64, _: &str) -> Result<Option<MemoryRecord>, String> {
            Ok(None)
        }
        fn list(&self) -> Result<Vec<MemoryRecord>, String> {
            Ok(vec![])
        }
    }
    struct Access;
    impl ContextAccessProvider for Access {
        fn access(
            &self,
            context: &ToolContext,
            _person: Option<&PersonScope>,
        ) -> Option<(AccessContext, Option<Applicability>)> {
            Some((
                AccessContext::voice(
                    TrustedScope::device_grant(&context.device_id, 1).ok()?,
                    None,
                    None,
                )
                .ok()?,
                None,
            ))
        }
    }
    let memory = MemoryService::new(Arc::new(Summary));
    memory.configure(true, || Ok(())).unwrap();
    let retriever = ContextRetriever::new(ContextService::new(Arc::new(FakeStore::default())));
    for (retrieval_on, expect_summary) in [(false, true), (true, false)] {
        retriever.set_enabled(retrieval_on);
        let llm = Arc::new(FakeLlm::default());
        let asr = Arc::new(FakeAsr {
            replies: Mutex::new(VecDeque::from([Ok("我喜欢喝什么茶".into())])),
            ..Default::default()
        });
        let conversation = Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            Arc::new(FastTools::default()),
            ConversationSettings::default(),
        )
        .with_memory(memory.clone())
        .with_context_retrieval(ContextRetrieval {
            retriever: retriever.clone(),
            access: Arc::new(Access),
        });
        let context = ToolContext {
            device_id: "fake-device".into(),
            session_id: "fake-session".into(),
        };
        let (handle, mut events) = spawn_turn(
            Arc::new(conversation),
            context,
            TurnInput::Utterance {
                pcm: vec![0; 960],
                sample_rate: UP,
                stream: None,
            },
            vec![],
            None,
        );
        while let Some(event) = events.recv().await {
            let _: TurnEvent = event;
        }
        handle.await.unwrap();
        let seen = llm.seen.lock().unwrap();
        let injected = seen
            .iter()
            .flat_map(|(messages, _)| messages.iter())
            .any(|m| {
                m.content
                    .as_deref()
                    .is_some_and(|c| c.contains("旧设备摘要"))
            });
        assert_eq!(injected, expect_summary, "检索开关 {retrieval_on}");
    }
}

/// n59：对小智说“换成 XX”时模型调用 `voice_persona_tool`，切换话术直接朗读，下一轮起系统提示换成新角色，
/// 安全铁律仍在末尾。
#[tokio::test]
async fn n59_spoken_persona_switch_changes_the_next_turn_prompt_and_keeps_safety_rules() {
    use crate::conversation::{IMMUTABLE_SAFETY_RULES, TurnEvent, TurnInput, spawn_turn};
    use crate::persona::{PERSONA_TOOL, PersonaConfig, PersonaSwitch, PersonaTools};

    let persona = Arc::new(PersonaSwitch::new(PersonaConfig::default(), None));
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: Vec::new(),
                calls: vec![ToolCall::new(
                    "call-persona",
                    PERSONA_TOOL,
                    r#"{"action":"switch","name":"冷面幽默"}"#,
                )],
                delay_ms: 0,
            },
            say(&["收到。"]),
        ])),
        ..FakeLlm::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("换成冷面幽默".into()),
            Ok("现在几点".into()),
        ])),
        ..Default::default()
    });
    let tools: Arc<dyn ToolExecutor> = Arc::new(PersonaTools {
        original: Arc::new(FastTools::default()),
        persona: persona.clone(),
    });
    let conversation = Arc::new(
        Conversation::new(
            asr,
            llm.clone(),
            Arc::new(FakeTts::default()),
            tools,
            ConversationSettings {
                read_only_shortcuts: false,
                ..ConversationSettings::default()
            },
        )
        .with_persona(persona.clone()),
    );
    let context = ToolContext {
        device_id: "fake-device".into(),
        session_id: "fake-session".into(),
    };
    let mut spoken = Vec::new();
    for _ in 0..2 {
        let (handle, mut events) = spawn_turn(
            conversation.clone(),
            context.clone(),
            TurnInput::Utterance {
                pcm: vec![0; 960],
                sample_rate: UP,
                stream: None,
            },
            vec![],
            None,
        );
        while let Some(event) = events.recv().await {
            if let TurnEvent::Speech { text, .. } = event {
                spoken.push(text);
            }
        }
        handle.await.unwrap();
    }
    assert_eq!(
        spoken,
        vec![
            "好的，已切换为MOSS 冷面幽默。".to_owned(),
            "收到。".to_owned()
        ]
    );
    let seen = llm.seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "the switch reply is spoken without asking the model again"
    );
    let first = seen[0].0[0].content.clone().unwrap();
    let second = seen[1].0[0].content.clone().unwrap();
    assert!(!first.contains("冷面幽默风格"));
    assert!(
        second.contains("冷面幽默风格"),
        "next turn uses the new role"
    );
    let base = second.split("\n当前本机时间").next().unwrap();
    assert!(
        base.ends_with(IMMUTABLE_SAFETY_RULES),
        "safety rules stay last in the role prompt"
    );
}

#[test]
fn persona_style_clock_anti_parrot_asr_corrections_and_debug_traces_work_end_to_end() {
    use crate::conversation::{IMMUTABLE_SAFETY_RULES, compose_persona_system_prompt};

    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("帮我切 3 毫米段木".into()),
            Ok("再看一下一宿状态".into()),
        ])),
        heard: Mutex::default(),
    });
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["已按椴木准备。"]),
            say(&["翼宿正常。"]),
        ])),
        repeat: None,
        seen: Mutex::default(),
    });
    let tts = Arc::new(FakeTts::default());
    let prompt = compose_persona_system_prompt("moss_station", "工坊常用 5W 蓝光模组");
    assert!(prompt.contains("领航员空间站"));
    assert!(prompt.contains("工坊常用 5W 蓝光模组"));
    assert!(
        prompt.ends_with(IMMUTABLE_SAFETY_RULES),
        "底层安全铁律必须锁死在系统提示末尾"
    );

    let conversation = Conversation::new(
        asr,
        llm.clone(),
        tts,
        Arc::new(FakeTools::default()),
        ConversationSettings {
            system_prompt: prompt,
            include_local_clock: true,
            asr_corrections: vec![
                ("段木".into(), "椴木".into()),
                ("一宿".into(), "翼宿".into()),
            ],
            read_only_shortcuts: false,
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
            ..ServerConfig::default()
        },
        registry.clone(),
    )
    .unwrap();
    assert!(!server.status().debug_enabled);
    server.set_debug_enabled(true);

    let mut device = chat_device(&server, &registry, 1);
    // 第 1 轮：同音纠偏生效、系统提示含本机时间、首轮不加“后续轮次禁重复称呼”
    speak_manual(&mut device, 1, 600);
    let first_items = collect_items(&mut device);
    assert!(texts(&first_items).contains(&"听:帮我切 3 毫米椴木".to_owned()));

    // 第 2 轮：自动附带同一会话防复读机称呼约束
    speak_manual(&mut device, 1, 600);
    let second_items = collect_items(&mut device);
    assert!(texts(&second_items).contains(&"听:再看一下翼宿状态".to_owned()));

    let seen = llm.seen.lock().unwrap();
    let sys1 = seen[0].0[0].content.as_deref().unwrap();
    let sys2 = seen[1].0[0].content.as_deref().unwrap();
    assert!(sys1.contains("当前本机时间：") && !sys1.contains("对话连贯约束："));
    assert!(sys2.contains("当前本机时间：") && sys2.contains("对话连贯约束："));
    assert_eq!(
        seen[0].0.last().unwrap().content.as_deref(),
        Some("帮我切 3 毫米椴木")
    );
    assert_eq!(
        seen[1].0.last().unwrap().content.as_deref(),
        Some("再看一下翼宿状态")
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while server.status().debug_traces.len() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let traces = server.status().debug_traces;
    assert_eq!(traces.len(), 2);
    assert_eq!(traces[1].raw_asr, "帮我切 3 毫米段木");
    assert_eq!(traces[1].corrected_asr, "帮我切 3 毫米椴木");
    assert_eq!(traces[0].raw_asr, "再看一下一宿状态");
    assert_eq!(traces[0].corrected_asr, "再看一下翼宿状态");

    server.set_debug_enabled(false);
    assert!(server.status().debug_traces.is_empty());
}
