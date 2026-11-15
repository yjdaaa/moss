//! 真实本地模型端到端（默认忽略，需本机模型）。
use super::*;

/// 真实本地模型的端到端测试（不在常规测试中运行）：用合成的一句话当作设备上传的语音，
/// 自动模式下由本地判停、本地识别，假大模型回答，再由本地合成回复。
/// `MOSS_VOICE_LOCAL_EXE=<moss-voice-local.exe> MOSS_LOCAL_MODELS=<模型目录> cargo test -p moss-voice-server real_local -- --ignored --nocapture`
struct LocalCombo {
    vad: (&'static str, moss_voice_providers::local::VadEngine),
    asr: (&'static str, moss_voice_providers::local::AsrEngine),
    tts: (&'static str, moss_voice_providers::local::TtsEngine, i32),
}

fn run_local_end_to_end(combo: LocalCombo) {
    use moss_voice_providers::local::{
        LocalAsr, LocalHelper, LocalStreamingAsr, LocalTts, VadParams,
    };
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let models =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").expect("MOSS_LOCAL_MODELS"));
    let helper = LocalHelper::spawn(std::path::Path::new(&exe)).unwrap();
    helper
        .load_vad(
            &models.join(combo.vad.0),
            VadParams::for_engine(combo.vad.1),
            combo.vad.1,
        )
        .unwrap();
    helper
        .load_asr(&models.join(combo.asr.0), combo.asr.1)
        .unwrap();
    helper
        .load_tts(&models.join(combo.tts.0), combo.tts.1)
        .unwrap();
    let speaker = combo.tts.2;

    // 设备端“说话”：合成后重采样到 16 kHz，前后加静音
    let speech = helper
        .synthesize_blocking("帮我看看激光机的状态。", speaker, 1.0)
        .unwrap();
    let mut resampler = crate::audio::Resampler::new(speech.sample_rate, UP);
    let mut pcm = silence(UP, 500);
    pcm.extend(resampler.process(&speech.samples));
    pcm.extend(silence(UP, 1200));

    let llm = FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["激光机现在空闲。"])])),
        ..FakeLlm::default()
    };
    let llm = Arc::new(llm);
    let mut conversation = Conversation::new(
        Arc::new(LocalAsr {
            helper: helper.clone(),
        }),
        llm.clone(),
        Arc::new(LocalTts {
            helper: helper.clone(),
            speaker,
            speed: 1.0,
        }),
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    );
    if combo.asr.1.streaming() {
        conversation = conversation.with_streaming_asr(Arc::new(LocalStreamingAsr {
            helper: helper.clone(),
        }));
    }
    let registry = Arc::new(FakeRegistry::default());
    let config = ServerConfig {
        bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        ws_port: 0,
        http_port: 0,
        mode: ServerMode::Chat,
        conversation: Some(Arc::new(conversation)),
        vad_factory: Some(Arc::new(crate::LocalVadFactory {
            helper: helper.clone(),
        })),
        ..ServerConfig::default()
    };
    let server = VoiceServer::start(config, registry.clone()).unwrap();
    let mut device = chat_device(&server, &registry, 1);
    device
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    send_json(
        &mut device,
        r#"{"type":"listen","state":"start","mode":"auto"}"#,
    );
    let started = Instant::now();
    send_audio(&mut device, 1, &uplink(&pcm));
    let items = collect_items(&mut device);
    let spoken = texts(&items);
    println!("用时 {:?}：{spoken:?}", started.elapsed());
    for event in server.status().events.iter().rev() {
        println!("  事件：{}", event.text);
    }
    assert!(
        spoken[0].starts_with("听:")
            && spoken
                .iter()
                .any(|line| line.starts_with("听:") && line.contains("激光")),
        "{spoken:?}"
    );
    assert!(spoken.contains(&"句:激光机现在空闲。".to_owned()));
    let frames = items
        .iter()
        .filter(|item| matches!(item, Item::Frame))
        .count();
    assert!(frames > 10, "回复有音频：{frames} 帧");
    let events = server.status().events;
    assert!(
        !events
            .iter()
            .any(|event| event.text.contains("改用能量判停")),
        "本地判停正常工作"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.text.contains("改用整段识别")),
        "流式识别没有失败"
    );
    assert_eq!(
        llm.seen.lock().unwrap()[0].0.last().unwrap().role,
        Role::User
    );
}

/// NV6 默认组合：Silero + SenseVoice 2024 + Kokoro v1.0。
#[test]
#[ignore]
fn real_local_models_end_to_end() {
    use moss_voice_providers::local::{AsrEngine, TtsEngine, VadEngine};
    run_local_end_to_end(LocalCombo {
        vad: ("silero_vad.onnx", VadEngine::Silero),
        asr: (
            "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17",
            AsrEngine::SenseVoice,
        ),
        tts: ("kokoro-multi-lang-v1_0", TtsEngine::Kokoro, 50),
    });
}

/// 可选模型中最快的组合：TEN-VAD + X-ASR 流式（边说边识别）+ Matcha。
#[test]
#[ignore]
fn real_local_fast_models_end_to_end() {
    use moss_voice_providers::local::{AsrEngine, TtsEngine, VadEngine};
    run_local_end_to_end(LocalCombo {
        vad: ("ten-vad.onnx", VadEngine::Ten),
        asr: (
            "sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05",
            AsrEngine::StreamingTransducer,
        ),
        tts: ("matcha-icefall-zh-en", TtsEngine::Matcha, 0),
    });
}

#[test]
#[ignore]
fn real_local_kokoro_fallback_when_primary_cloud_tts_fails() {
    use moss_voice_providers::local::{LocalHelper, LocalTts, TtsEngine};
    let exe = std::env::var("MOSS_VOICE_LOCAL_EXE").expect("MOSS_VOICE_LOCAL_EXE");
    let models =
        std::path::PathBuf::from(std::env::var("MOSS_LOCAL_MODELS").expect("MOSS_LOCAL_MODELS"));
    let helper = LocalHelper::spawn(std::path::Path::new(&exe)).unwrap();
    helper
        .load_tts(&models.join("kokoro-multi-lang-v1_0"), TtsEngine::Kokoro)
        .unwrap();
    let local_fallback = Arc::new(LocalTts {
        helper,
        speaker: 50,
        speed: 1.0,
    });

    // 主 TTS 始终失败（文本含“失败”）
    let primary_tts = Arc::new(FakeTts::default());
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([say(&["云端合成失败，已切换本地播报。"])])),
        ..FakeLlm::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("测试真实本地兜底".to_owned())])),
        heard: Mutex::default(),
    });
    let conversation = Conversation::new(
        asr,
        llm,
        primary_tts,
        Arc::new(FakeTools::default()),
        ConversationSettings::default(),
    )
    .with_fallback_tts(local_fallback);
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
    device
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    speak_manual(&mut device, 1, 600);
    let items = collect_items(&mut device);
    let frame_count = items
        .iter()
        .filter(|item| matches!(item, Item::Frame))
        .count();
    assert_eq!(
        texts(&items),
        vec![
            "听:测试真实本地兜底",
            "tts:start",
            "句:云端合成失败，",
            "句:已切换本地播报。",
            "tts:stop",
        ]
    );
    assert!(
        frame_count >= 15,
        "真实 Kokoro 本地降级必须产出有效 Opus 音频帧，实际帧数={frame_count}"
    );
}
