use super::*;
use crate::person_context::{
    PersonContext,
    tests::{Resolver, Store},
};
use crate::vision::Photo;
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
            source: crate::conversation::FastStopSource::Interim,
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
                AccessContext::voice(dev, p, None)
                    .ok()?
                    .with_personal_mode(true),
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
        // 会话断开后的后台摘要与 NV19 提取统一返回带原话引用的候选 JSON（a、b 各只命中自己说过的话）
        repeat: Some(say(&[
            r#"{"operations":[{"op":"add","category":"preference","content":"回答保持精炼直接","quote":"我喜欢精炼直接的回答"},{"op":"add","category":"preference","content":"日常闲聊记录","quote":"我是乙，随便聊聊"}]}"#,
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
        assert!(
            flatten(0).contains("木板纹理怎么样"),
            "第 1 轮说话人 a 应看到视觉照片"
        );
        assert!(
            !flatten(1).contains("木板纹理怎么样"),
            "第 2 轮说话人 b 严禁看到 a 的照片"
        );
        assert!(
            flatten(2).contains("木板纹理怎么样"),
            "第 3 轮说话人 a 追问时照片仍在其个人历史中"
        );
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

#[test]
fn short_and_overlong_utterances_fail_closed_without_leaking_or_wiping_verified_speaker_history() {
    // 第 1 轮（a，2.1s）与第 3 轮（陌生人 None，2.1s）及第 5 轮（a，2.1s）进入声纹提取器；
    // 第 2 轮（1s < 2s）与第 4 轮（10.5s > 10s）均未经过本轮声纹核验，严禁读取或污染说话人 a 的私密历史与摘要；
    // 第 5 轮说话人 a 重新说满 >=2s 完成验声后，PersonContext 中保留的第 1 轮历史与摘要仍完整恢复。
    let resolver = Resolver::new(&[Some("a"), None, Some("a")]);
    let personal = PersonContext::new(resolver.clone(), Store::seeded());
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["请问厚度是多少？"]),
            say(&["当前未识别说话人身份，请补充完整需求。"]),
            say(&["我不清楚之前的对话。"]),
            say(&["超长语句未识别身份。"]),
            say(&["好的，按五毫米继续刚才的椴木板任务。"]),
        ])),
        repeat: None,
        seen: Mutex::default(),
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("帮我切割一块椴木板".into()),
            Ok("刚才说的厚度是多少".into()),
            Ok("继续刚才的任务".into()),
            Ok("这是一段超过十秒的陌生人长句测试".into()),
            Ok("厚度按五毫米来".into()),
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

    // 第 1 轮：Alice 说满 >=2s（2100ms），声纹识别为 a
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);
    // 第 2 轮：未达验声下限的 1s 短问句（<2s），不验证身份，绝不能泄露说话人 a 的历史或摘要
    speak_manual(&mut device, 1, 1000);
    let _ = collect_items(&mut device);
    // 第 3 轮：陌生人说满 >=2s（2100ms），声纹识别返回 None，绝不能看到说话人 a 的历史或摘要
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);
    // 第 4 轮：陌生人说超过 10s 的超长句（10500ms > 10s），按 Denied 拒绝，绝不能继承说话人 a
    speak_manual(&mut device, 1, 10500);
    let _ = collect_items(&mut device);
    // 第 5 轮：Alice 再次说满 >=2s（2100ms）通过验声，第 1 轮持久个人历史与摘要完好无损且不含中间未知轮次
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);

    assert_eq!(
        resolver.calls.load(Ordering::SeqCst),
        3,
        "<2s 与 >10s 语句不应调用声纹提取器"
    );
    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 5);
    let flatten = |idx: usize| -> String {
        seen[idx]
            .0
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for unverified_turn in [1, 2, 3] {
        let prompt = flatten(unverified_turn);
        assert!(
            !prompt.contains("甲喜欢茶")
                && !prompt.contains("帮我切割一块椴木板")
                && !prompt.contains("请问厚度是多少"),
            "第 {} 轮未验声说话人严禁看到说话人 a 的任何个人历史或摘要，实际：\n{prompt}",
            unverified_turn + 1
        );
    }
    let alice_resumed = flatten(4);
    assert!(
        alice_resumed.contains("甲喜欢茶")
            && alice_resumed.contains("帮我切割一块椴木板")
            && alice_resumed.contains("请问厚度是多少")
            && !alice_resumed.contains("刚才说的厚度是多少")
            && !alice_resumed.contains("超过十秒的陌生人"),
        "第 5 轮说话人 a 重新验声后应完整恢复本人历史且未混入未知轮次，实际：\n{alice_resumed}"
    );
}

#[test]
fn personal_mode_preserves_uncommitted_vision_photo_after_ignored_turn_until_real_speaker_commits()
{
    let resolver = Resolver::new(&[Some("a"), Some("b")]);
    let personal = PersonContext::new(resolver, Store::seeded());
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            say(&["这是刚拍好的椴木板。"]),
            say(&["好的乙。"]),
        ])),
        repeat: None,
        seen: Mutex::default(),
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok(String::new()),
            Ok("帮我看看刚刚拍的木板".into()),
            Ok("我是乙".into()),
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

    server.shared.vision_photos.put(
        DEVICE,
        Photo {
            question: "这张木板纹理如何".into(),
            text: "一块干净的椴木板".into(),
            data_url: "data:image/png;base64,iVBORw0KGgo=".into(),
        },
    );

    // 第 1 轮 ASR 返回空（TurnEvent::Ignored），未提交且未交给大模型：照片不能丢失
    speak_manual(&mut device, 1, 260);
    let ignored_deadline = Instant::now() + Duration::from_secs(3);
    while !server
        .status()
        .events
        .iter()
        .any(|e| e.text.contains("没有识别出文字"))
    {
        assert!(
            Instant::now() < ignored_deadline,
            "第 1 轮空识别应记录“没有识别出文字”事件"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
    // 第 2 轮（说话人 a >= 2s）：必须继承上一轮尚未提交的视觉照片
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);
    // 第 3 轮（说话人 b >= 2s）：照片已提交进说话人 a，严禁再带给 b
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);

    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let flatten = |idx: usize| -> String {
        seen[idx]
            .0
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        flatten(0).contains("这张木板纹理如何"),
        "空识别 Ignored 之后的第 2 轮必须仍看得到未提交照片"
    );
    assert!(
        !flatten(1).contains("这张木板纹理如何"),
        "照片已在第 2 轮提交给说话人 a，第 3 轮说话人 b 严禁看到"
    );
}

fn write_test_wav(dir: &std::path::Path, seconds: u32) {
    let rate = 24_000u32;
    let frames = rate * seconds;
    let mut file = std::fs::File::create(dir.join("测试音乐.wav")).unwrap();
    let mut header = Vec::new();
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&rate.to_le_bytes());
    header.extend_from_slice(&(rate * 2).to_le_bytes());
    header.extend_from_slice(&2u16.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(frames * 2).to_le_bytes());
    std::io::Write::write_all(&mut file, &header).unwrap();
    let pcm = vec![2000i16; frames as usize];
    for sample in pcm {
        std::io::Write::write_all(&mut file, &sample.to_le_bytes()).unwrap();
    }
}

#[test]
fn personal_mode_preserves_uncommitted_photo_across_asr_failure_music_and_unverified_turn() {
    let dir = tempfile::tempdir().unwrap();
    write_test_wav(dir.path(), 1);
    let music_index = Arc::new(crate::music::MusicIndex::scan(dir.path()).unwrap());
    let tools = Arc::new(crate::music::MusicTools::new(
        Arc::new(FakeTools::default()),
        music_index.clone(),
    ));
    let resolver = Resolver::new(&[Some("a"), Some("a"), Some("b")]);
    let personal = PersonContext::new(resolver, Store::seeded());
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([
            Round::Reply {
                deltas: vec![],
                calls: vec![ToolCall::new(
                    "music-call-1",
                    "play_music",
                    r#"{"query":"测试音乐"}"#,
                )],
                delay_ms: 0,
            },
            say(&["短句收到。"]),
            say(&["甲的问题我看到了。"]),
            say(&["乙的问题收到。"]),
        ])),
        ..Default::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Err(ProviderError::new("识别超时")),
            Ok("播放测试音乐".into()),
            Ok("在吗".into()),
            Ok("看看木板".into()),
            Ok("今天天气".into()),
        ])),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        llm.clone(),
        Arc::new(FakeTts::default()),
        tools,
        ConversationSettings::default(),
    )
    .with_personal_context(personal);
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            music: Some(music_index),
            playback_start_ms: 100,
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    let mut device = chat_device(&server, &registry, 1);

    server.shared.vision_photos.put(
        DEVICE,
        Photo {
            question: "这张木板纹理如何".into(),
            text: "一块干净的椴木板".into(),
            data_url: "data:image/png;base64,iVBORw0KGgo=".into(),
        },
    );

    // 第 1 轮：ASR 报错（TurnEvent::Failed -> 固定报错播报 + Done{messages:[]}），照片不得丢失
    speak_manual(&mut device, 1, 260);
    let _ = collect_items(&mut device);

    // 第 2 轮：PlayMusic 接管并在 music.active() 时收到 Done{messages:[]}，照片不得丢失
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);

    // 第 3 轮：<2s 短句未核验身份（commit_scope = None），end_turn 未提交给任何说话人，照片继续保留
    speak_manual(&mut device, 1, 260);
    let _ = collect_items(&mut device);

    // 第 4 轮：说话人 a >= 2s 正常回答并提交，必须看到未提交照片
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);

    // 第 5 轮：说话人 b >= 2s，照片已提交进 a，严禁泄露给 b
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);

    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    let flatten = |idx: usize| -> String {
        seen[idx]
            .0
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        flatten(1).contains("这张木板纹理如何"),
        "ASR 报错与 PlayMusic 接管之后，第 3 轮短句仍应带着未提交照片"
    );
    assert!(
        flatten(2).contains("甲喜欢茶") && flatten(2).contains("这张木板纹理如何"),
        "未核验短句第 3 轮未提交历史后，第 4 轮已核验说话人 a 必须仍看到该照片"
    );
    assert!(
        flatten(3).contains("乙喜欢咖啡") && !flatten(3).contains("这张木板纹理如何"),
        "照片已在第 4 轮提交给说话人 a，第 5 轮说话人 b 严禁看到"
    );
}

#[test]
fn personal_mode_preserves_uncommitted_photo_on_abort_before_person_resolved() {
    let resolver = Arc::new(Resolver {
        allowed: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
        delay: Duration::from_millis(500),
        replies: Mutex::new(VecDeque::from([Some("a".into()), Some("b".into())])),
    });
    let personal = PersonContext::new(resolver, Store::seeded());
    let llm = Arc::new(FakeLlm::default());
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([
            Ok("先停一下别查".into()),
            Ok("现在看刚才的照片".into()),
            Ok("换个人问".into()),
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

    server.shared.vision_photos.put(
        DEVICE,
        Photo {
            question: "这张木板纹理如何".into(),
            text: "一块干净的椴木板".into(),
            data_url: "data:image/png;base64,iVBORw0KGgo=".into(),
        },
    );

    // 第 1 轮：刚收到 Heard（stt 已下发，turn.user = Some(...)）且慢声纹仍在 500ms 等待期间立刻 abort
    speak_manual(&mut device, 1, 2100);
    let stt_msg = read_json(&mut device);
    assert_eq!(stt_msg["type"], "stt");
    send_json(&mut device, r#"{"type":"abort","reason":"user_cancel"}"#);
    let abort_deadline = Instant::now() + Duration::from_secs(3);
    while !server
        .status()
        .events
        .iter()
        .any(|e| e.text.contains("对话被打断"))
    {
        assert!(Instant::now() < abort_deadline, "应记录对话被打断事件");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        llm.seen.lock().unwrap().is_empty(),
        "声纹未完成前被打断，第 1 轮不应请求大模型"
    );
    // 短暂让出调度确保已 abort 的第 1 轮声纹任务 Drop 释放单设备锁（不需要等满 500ms）
    std::thread::sleep(Duration::from_millis(30));

    // 第 2 轮：说话人 a >= 2s 正常提交，必须继承被打断轮尚未提交的视觉照片
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);
    // 第 3 轮：说话人 b >= 2s，严禁看到已提交给 a 的照片
    speak_manual(&mut device, 1, 2100);
    let _ = collect_items(&mut device);

    let seen = llm.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let flatten = |idx: usize| -> String {
        seen[idx]
            .0
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        flatten(0).contains("甲喜欢茶") && flatten(0).contains("这张木板纹理如何"),
        "Heard 后、PersonResolved 前被打断，第 2 轮说话人 a 必须继承未提交照片"
    );
    assert!(
        flatten(1).contains("乙喜欢咖啡") && !flatten(1).contains("这张木板纹理如何"),
        "照片已在第 2 轮提交给说话人 a，第 3 轮说话人 b 严禁看到"
    );
}
#[tokio::test]
async fn speech_cache_deduplicates_and_caps_at_thirty_two_entries_preserving_fixed_phrases() {
    let tts = Arc::new(FakeTts::default());
    let conversation = Conversation::new(
        Arc::new(FakeAsr::default()),
        Arc::new(FakeLlm::default()),
        tts.clone(),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    );
    let fixed: Vec<String> = conversation
        .fixed_phrases()
        .into_iter()
        .filter(|p| !p.contains("失败"))
        .collect();
    assert!(!fixed.is_empty() && fixed.len() < 32);
    assert_eq!(conversation.prewarm_cache().await, fixed.len());
    let calls_after_prewarm = tts.said.lock().unwrap().len();

    // 重复合成同一句动态话术只调用一次底层 TTS，且超过 32 条后淘汰最早的动态条目并保留固定话术
    for i in 0..40 {
        let phrase = format!("动态播报第{i}条");
        conversation.prewarm_or_speak_for_test(&phrase).await;
        conversation.prewarm_or_speak_for_test(&phrase).await;
    }
    assert_eq!(tts.said.lock().unwrap().len(), calls_after_prewarm + 40);
    assert_eq!(conversation.speech_cache_len_for_test(), 32);

    // 预合成成功的固定话术仍在缓存中（不会再次触发底层 TTS）
    let before_fixed_check = tts.said.lock().unwrap().len();
    for phrase in &fixed {
        conversation.prewarm_or_speak_for_test(phrase).await;
    }
    assert_eq!(tts.said.lock().unwrap().len(), before_fixed_check);
}
