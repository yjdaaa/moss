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
