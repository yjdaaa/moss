use super::*;
use crate::person_context::{
    PersonContext,
    tests::{Resolver, Store},
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
struct DeviceMemory {
    reads: AtomicUsize,
}
impl crate::memory::VoiceMemoryStore for DeviceMemory {
    fn allowed(&self, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    fn load(&self, id: &str) -> Result<Option<crate::memory::MemoryRecord>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(Some(crate::memory::MemoryRecord {
            device_id: id.into(),
            summary: "禁止混入的设备共享摘要".into(),
            revision: 0,
            updated_at: 0.,
        }))
    }
    fn ensure(&self, id: &str) -> Result<crate::memory::MemoryRecord, String> {
        Ok(self.load(id)?.unwrap())
    }
    fn update(
        &self,
        _: &str,
        _: i64,
        _: &str,
    ) -> Result<Option<crate::memory::MemoryRecord>, String> {
        Ok(None)
    }
    fn list(&self) -> Result<Vec<crate::memory::MemoryRecord>, String> {
        Ok(vec![])
    }
}
#[test]
fn fake_device_turns_use_current_person_and_never_shared_device_context() {
    let resolver = Resolver::new(&[Some("a"), Some("b"), None, Some("a")]);
    let personal = PersonContext::new(resolver, Store::seeded());
    let device_memory = Arc::new(DeviceMemory {
        reads: AtomicUsize::new(0),
    });
    let memory = crate::memory::MemoryService::new(device_memory.clone());
    memory.configure(true, || Ok(())).unwrap();
    let llm = Arc::new(FakeLlm::default());
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(
            ["第一句", "第二句", "第三句", "第四句"]
                .iter()
                .map(|s| Ok((*s).into()))
                .collect(),
        ),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        llm.clone(),
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    )
    .with_memory(memory)
    .with_personal_context(personal);
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
    let mut device = chat_device(&server, &registry, 1);
    for _ in 0..4 {
        speak_manual(&mut device, 1, 2100);
        let _ = collect_items(&mut device);
    }
    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    let texts = seen
        .iter()
        .map(|(messages, _)| {
            messages
                .iter()
                .filter_map(|m| m.content.as_deref())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>();
    assert!(texts[0].contains("甲喜欢茶"));
    assert!(!texts[0].contains("乙"));
    assert!(texts[1].contains("乙喜欢咖啡"));
    assert!(!texts[1].contains("甲") && !texts[1].contains("第一句"));
    assert!(
        !texts[2].contains("历史摘要")
            && !texts[2].contains("第一句")
            && !texts[2].contains("第二句")
    );
    assert!(texts[3].contains("甲喜欢茶") && texts[3].contains("第一句"));
    assert!(!texts[3].contains("第二句") && !texts[3].contains("第三句"));
    assert_eq!(device_memory.reads.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn final_and_interim_fast_stop_never_wait_for_slow_identity() {
    let resolver = Arc::new(Resolver {
        delay: Duration::from_secs(5),
        allowed: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        replies: Mutex::new(VecDeque::from([Some("a".into())])),
    });
    let personal = PersonContext::new(resolver.clone(), Store::seeded());
    let tools = Arc::new(FastTools::default());
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("停止".into())])),
        heard: Mutex::default(),
    });
    let conversation = Arc::new(
        Conversation::new(
            asr,
            Arc::new(FakeLlm::default()),
            Arc::new(FakeTts::default()),
            tools.clone(),
            ConversationSettings::default(),
        )
        .with_personal_context(personal),
    );
    let ctx = ToolContext {
        device_id: "synthetic-device".into(),
        session_id: "synthetic-session".into(),
    };
    let began = Instant::now();
    let (handle, mut rx) = crate::conversation::spawn_turn(
        conversation.clone(),
        ctx.clone(),
        crate::conversation::TurnInput::Utterance {
            pcm: vec![200; 32000],
            sample_rate: 16000,
            stream: None,
        },
        vec![],
        None,
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        while rx.recv().await.is_some() {}
        handle.await.unwrap();
    })
    .await
    .unwrap();
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
    let calls = resolver.calls.load(Ordering::SeqCst);
    let (handle, mut rx) = crate::conversation::spawn_turn(
        conversation,
        ctx,
        crate::conversation::TurnInput::FastStop {
            text: "停止".into(),
        },
        vec![],
        None,
    );
    while rx.recv().await.is_some() {}
    handle.await.unwrap();
    assert_eq!(resolver.calls.load(Ordering::SeqCst), calls);
    assert_eq!(tools.calls.lock().unwrap().len(), 2);
}

#[test]
fn personal_context_bridges_vision_photos_per_speaker_and_runs_nv19_extraction_on_disconnect() {
    use crate::context::extract::{ContextExtraction, ContextExtractor};
    use crate::context::fake_store::FakeStore;
    use crate::context::retrieve::ContextAccessProvider;
    use crate::context::service::ContextService;
    use crate::context::{AccessContext, Applicability, ScopeKey, ScopeKind, TrustedScope};
    use crate::person_context::PersonScope;
    use crate::vision::Photo;

    struct BridgeAccess;
    impl ContextAccessProvider for BridgeAccess {
        fn access(
            &self,
            context: &ToolContext,
            person: Option<&PersonScope>,
        ) -> Option<(AccessContext, Option<Applicability>)> {
            let dev = TrustedScope::device_grant(&context.device_id, 1).ok()?;
            let p = person.and_then(|s| TrustedScope::calibrated_person(s).ok());
            Some((
                AccessContext::voice(dev, p, None).ok()?,
                Applicability::new("MOSS-550W", Some("1.2.0")).ok(),
            ))
        }
    }

    let store = Arc::new(FakeStore::default());
    let service = ContextService::new(store.clone());
    let extractor = ContextExtractor::new(service.clone());
    extractor.configure(true, || Ok(())).unwrap();
    let extraction = ContextExtraction {
        extractor,
        access: Arc::new(BridgeAccess),
    };

    // 3 轮语音对话（a → b → a）+ 断开后 2 个说话人的摘要与 2 个说话人的 NV19 提取
    let resolver = Resolver::new(&[Some("a"), Some("b"), Some("a")]);
    let personal = PersonContext::new(resolver, Store::seeded());
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["看到的是一块椴木板。"]),
            say(&["好的乙。"]),
            say(&["图案左边是圆角。"]),
        ])),
        // 会话断开后的后台摘要与 NV19 提取统一返回一条合法候选 JSON
        repeat: Some(say(&[
            r#"{"operations":[{"op":"add","category":"preference","content":"回答保持精炼直接"}]}"#,
        ])),
        seen: Mutex::default(),
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("看看刚拍的照片，另外我喜欢精炼直接的回答".into()),
            Ok("我是乙，随便聊聊".into()),
            Ok("刚才照片左边是什么".into()),
        ])),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        llm.clone(),
        Arc::new(FakeTts::default()),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    )
    .with_personal_context(personal)
    .with_context_extraction(extraction);

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

    let mut device = chat_device(&server, &registry, 1);
    // 模拟设备刚通过视觉接口拍了一张保留上下文的照片
    server.shared.vision_photos.put(
        DEVICE,
        Photo {
            question: "木板纹理怎么样".into(),
            text: "一块 3mm 椴木板，左上角有圆角图案".into(),
            data_url: "data:image/png;base64,iVBORw0KGgo=".into(),
        },
    );

    // 第 1 轮（说话人 a，时长 >= 2000ms 以触发声纹识别）：必须看到刚拍的照片
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);
    // 第 2 轮（说话人 b）：严禁看到说话人 a 的照片与对话
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);
    // 第 3 轮（说话人 a 追问）：照片已随第 1 轮归入说话人 a 的个人上下文，追问仍可见
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);

    {
        let seen = llm.seen.lock().unwrap();
        assert!(seen.len() >= 3);
        let flatten = |idx: usize| -> String {
            seen[idx]
                .0
                .iter()
                .filter_map(|m| m.content.as_deref())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(flatten(0).contains("木板纹理怎么样"), "第 1 轮说话人 a 应看到视觉照片");
        assert!(!flatten(1).contains("木板纹理怎么样"), "第 2 轮说话人 b 严禁看到 a 的照片");
        assert!(flatten(2).contains("木板纹理怎么样"), "第 3 轮说话人 a 追问时照片仍在其个人历史中");
    }

    // 正常说退出词关闭会话，触发按人摘要与 NV19 结构化自动提取
    let _ = device.close(None);
    drop(device);

    let owner = AccessContext::desktop_private(vec![
        TrustedScope::calibrated_person(&PersonScope::candidate("a", 1, 1).unwrap()).unwrap(),
        TrustedScope::calibrated_person(&PersonScope::candidate("b", 1, 1).unwrap()).unwrap(),
    ]);
    let key_a = ScopeKey::new(ScopeKind::Person, "a").unwrap();
    let key_b = ScopeKey::new(ScopeKind::Person, "b").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let rows_a = service.list(&owner, &key_a, 100.0).unwrap_or_default();
        let rows_b = service.list(&owner, &key_b, 100.0).unwrap_or_default();
        if !rows_a.is_empty() && !rows_b.is_empty() {
            assert!(rows_a.iter().any(|r| r.content.contains("精炼直接")));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "会话断开后应为已标定说话人 a 与 b 完成 NV19 自动提取，实际 a={}, b={}",
            rows_a.len(),
            rows_b.len()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
