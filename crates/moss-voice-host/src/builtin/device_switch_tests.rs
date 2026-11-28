use super::*;
use moss_agent::Provider;
use moss_voice::models::ProviderKind;
use moss_voice::settings::{BuiltinAgents, BuiltinModels, BuiltinMusic, BuiltinProvider};

#[test]
fn agent_save_allows_disabling_with_stale_paths_and_clears_inherited_dead_cli() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("runtime");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let sensitive = [runtime];

    let old = BuiltinAgents {
        enabled: true,
        workspace: "C:\\old-device\\agent-work".into(),
        codex_executable: "C:\\old-device\\codex.exe".into(),
        claude_executable: "/Users/old-user/.local/bin/claude".into(),
    };

    // 1. 换电脑后直接关掉开关点保存：不被旧工作目录或旧命令路径拦住
    let mut turn_off = old.clone();
    turn_off.enabled = false;
    let saved_off = normalize_agent_save(turn_off.clone(), &old, &sensitive, |_| None).unwrap();
    assert!(!saved_off.enabled);
    assert_eq!(saved_off.workspace, turn_off.workspace);

    // 2. 换电脑后保持开启、只改新电脑的工作目录：沿用的旧失效 CLI 路径自动清空并走自动读取
    let mut migrated = old.clone();
    migrated.workspace = work.display().to_string();
    let detected_claude = temp.path().join("claude-auto");
    let saved_on = normalize_agent_save(migrated, &old, &sensitive, |kind| {
        (kind == Provider::ClaudeCode).then(|| detected_claude.clone())
    })
    .unwrap();
    assert!(saved_on.enabled);
    assert!(
        saved_on.codex_executable.is_empty(),
        "旧设备的失效 Codex 路径应清空回退自动读取"
    );
    assert!(
        saved_on.claude_executable.is_empty(),
        "旧账户的失效 Claude 路径应清空回退自动读取"
    );

    // 3. 开启状态下若用户新填了一个不存在的手动路径，照常报错拦截
    let freshly_typed_bad = BuiltinAgents {
        enabled: true,
        workspace: work.display().to_string(),
        codex_executable: temp
            .path()
            .join("nonexistent-codex.exe")
            .display()
            .to_string(),
        claude_executable: String::new(),
    };
    let err = normalize_agent_save(
        freshly_typed_bad,
        &BuiltinAgents::default(),
        &sensitive,
        |_| Some(detected_claude.clone()),
    )
    .unwrap_err();
    assert_eq!(err.code, "agent_config_invalid");

    // 4. 开启状态下若工作目录仍是旧电脑不存在的路径，照常报错拦截
    let stale_work = BuiltinAgents {
        enabled: true,
        workspace: temp.path().join("missing-work").display().to_string(),
        codex_executable: String::new(),
        claude_executable: String::new(),
    };
    assert!(
        normalize_agent_save(stale_work, &old, &sensitive, |_| Some(
            detected_claude.clone()
        ))
        .is_err()
    );
}

#[test]
fn builtin_voice_starts_cleanly_when_migrated_music_dir_and_speaker_model_are_missing() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let ws_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let http_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    // 模拟从旧电脑拷来的数据库与配置：配齐云端对话三件套、开启了不存在的旧音乐目录、开启了声纹按人记忆但新电脑还没装声纹模型
    {
        let db = host.db.lock().unwrap();
        db.set_secret("voice.llm.main", "sk-test-llm", 1.0).unwrap();
        db.set_secret("voice.asr.main", "sk-test-asr", 1.0).unwrap();
        db.set_secret("voice.tts.main", "sk-test-tts", 1.0).unwrap();

        let mut v1 = vec![0.0f32; 192];
        v1[0] = 1.0;
        let mut v2 = vec![0.0f32; 192];
        v2[1] = 1.0;
        db.enroll_voice_speaker(
            "sp-a",
            "甲",
            SPEAKER_MODEL,
            &[v1.clone(), v1.clone(), v1],
            1.0,
        )
        .unwrap();
        db.enroll_voice_speaker(
            "sp-b",
            "乙",
            SPEAKER_MODEL,
            &[v2.clone(), v2.clone(), v2],
            1.0,
        )
        .unwrap();
        let cal = Calibration {
            report: CalibrationReport {
                threshold: 0.72,
                margin: 0.05,
                people: 2,
                genuine_trials: 6,
                genuine_accepted: 6,
                impostor_trials: 6,
                false_accepts: 0,
                max_impostor_score: 0.2,
            },
            directory: speaker_directory([("sp-a", 1), ("sp-b", 1)].into_iter()),
            calibrated_at: 2.0,
        };
        db.set_secret(
            SPEAKER_CALIBRATION_KEY,
            &serde_json::to_string(&cal).unwrap(),
            2.0,
        )
        .unwrap();
    }

    let mut voice_settings = moss_voice::VoiceSettings {
        builtin_ws_port: ws_port,
        builtin_http_port: http_port,
        ..Default::default()
    };
    voice_settings.builtin_models = BuiltinModels {
        llm: Some(BuiltinProvider {
            kind: ProviderKind::Openai,
            url: "https://llm.example.test/v1".into(),
            model: "gpt-test".into(),
            voice: String::new(),
            speed: 1.0,
        }),
        asr: Some(BuiltinProvider {
            kind: ProviderKind::Openai,
            url: "https://asr.example.test/v1/audio/transcriptions".into(),
            model: "whisper-1".into(),
            voice: String::new(),
            speed: 1.0,
        }),
        tts: Some(BuiltinProvider {
            kind: ProviderKind::Openai,
            url: "https://tts.example.test/v1/audio/speech".into(),
            model: "tts-1".into(),
            voice: "alloy".into(),
            speed: 1.0,
        }),
    };
    voice_settings.builtin_music = BuiltinMusic {
        enabled: true,
        directory: temp
            .path()
            .join("unplugged-old-music-drive")
            .display()
            .to_string(),
    };
    voice_settings.builtin_person_memory_enabled = true;
    moss_voice::settings::save(&temp.path().join("settings.json"), &voice_settings).unwrap();

    let state = init(Some(host));
    let builtin = state.0.unwrap();

    let status = builtin
        .start()
        .expect("旧音乐目录缺失与声纹模型未安装不应阻断内置语音主服务启动");
    assert_eq!(status["running"], true);
    assert_eq!(status["mode"], "chat");

    let music_view = builtin.music_status();
    assert!(
        music_view["warning"]
            .as_str()
            .unwrap_or_default()
            .contains("本地音乐目录当前不可用"),
        "{music_view}"
    );

    let speaker_view = builtin
        .speaker_manage(&serde_json::json!({"action": "status"}))
        .unwrap();
    assert!(
        speaker_view["warning"]
            .as_str()
            .unwrap_or_default()
            .contains("声纹按人记忆暂未生效"),
        "{speaker_view}"
    );

    // n68：降级提示出现在状态里，也写进诊断记录（警告级别、带分类与下一步）
    let warnings = status["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2, "{status}");
    let events = builtin.status()["events"].as_array().unwrap().clone();
    let music_event = events
        .iter()
        .find(|e| {
            e["text"]
                .as_str()
                .unwrap()
                .contains("本地音乐目录当前不可用")
        })
        .expect("音乐降级写进记录");
    assert_eq!(music_event["level"], "warn");
    assert_eq!(music_event["category"], "config");
    assert!(music_event["hint"].is_string());
    assert!(
        events.iter().any(|e| e["level"] == "warn"
            && e["text"].as_str().unwrap().contains("声纹按人记忆暂未生效"))
    );
    assert!(events.iter().any(|e| {
        e["text"]
            .as_str()
            .unwrap()
            .starts_with("内置语音服务已启动")
    }));

    builtin.stop();
    assert!(builtin.music_status()["warning"].is_null());
    assert!(
        builtin
            .speaker_manage(&serde_json::json!({"action": "status"}))
            .unwrap()["warning"]
            .is_null()
    );
    let after_stop = builtin.status();
    assert_eq!(after_stop["running"], false);
    assert!(after_stop["warnings"].as_array().unwrap().is_empty());
    assert_eq!(
        after_stop["events"].as_array().unwrap().len(),
        events.len(),
        "停止服务不清空诊断记录"
    );

    // 诊断包：整份记录 + 不含任何密钥
    let bundle = builtin.diagnostics_bundle();
    assert_eq!(bundle["format"], "moss-voice-diagnostics");
    assert_eq!(bundle["app"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(bundle["events"].as_array().unwrap().len(), events.len());
    assert_eq!(bundle["events_dropped"]["info"], 0);
    assert_eq!(bundle["approvals"]["limit"], APPROVALS_IN_BUNDLE);
    assert!(bundle["approvals"]["items"].is_array() && bundle["approvals"]["error"].is_null());
    assert_eq!(bundle["models"]["llm"]["model"], "gpt-test");
    assert_eq!(bundle["models"]["llm"]["key_set"], true);
    let text = bundle.to_string();
    for secret in ["sk-test-llm", "sk-test-asr", "sk-test-tts"] {
        assert!(!text.contains(secret), "诊断包泄露了 {secret}");
    }
    assert!(
        bundle["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("密钥"))
    );
}

/// n68：诊断包脱敏覆盖每一个字符串字段（键值写法的密钥、Bearer、带凭据地址、设备 IP）。
#[test]
fn diagnostics_bundle_scrub_masks_every_string() {
    let mut bundle = serde_json::json!({
        "events": [{"text": "OTA 请求被拒绝（192.168.31.20）", "detail": "api_key: sk-abc123 Authorization: Bearer tok.en"}],
        "models": {"llm": {"url": "https://user:pw@llm.example.test/v1?key=zzz", "model": "gpt-test"}},
        "sessions": [{"ip": "10.1.2.3", "device_id": "aa:bb:cc:11:22:33"}],
        "nested": [["token=abcd"]],
        "number": 42,
    });
    scrub_bundle(&mut bundle);
    let text = bundle.to_string();
    for leak in [
        "192.168.31.20",
        "sk-abc123",
        "tok.en",
        "user:pw",
        "zzz",
        "10.1.2.3",
        "abcd",
    ] {
        assert!(!text.contains(leak), "{leak} 未脱敏：{text}");
    }
    assert_eq!(
        bundle["models"]["llm"]["url"],
        "https://[REDACTED]@llm.example.test/v1?[REDACTED]"
    );
    assert_eq!(bundle["models"]["llm"]["model"], "gpt-test");
    assert_eq!(bundle["sessions"][0]["ip"], "<ip>");
    assert_eq!(
        bundle["sessions"][0]["device_id"], "aa:bb:cc:11:22:33",
        "设备编号保留用于排障"
    );
    assert_eq!(bundle["number"], 42);
}

#[test]
fn models_list_uses_stored_section_secret_without_echoing_it() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = br#"{"object":"list","data":[{"id":"FunAudioLLM/SenseVoiceSmall"},{"id":"Qwen/Qwen2.5-7B-Instruct"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.write_all(body);
        let _ = tx.send(head);
    });

    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    host.db
        .lock()
        .unwrap()
        .set_secret("voice.asr.main", "sk-stored-asr-secret", 1.0)
        .unwrap();
    let builtin = init(Some(host)).0.unwrap();

    let input = SectionInput {
        url: format!("http://127.0.0.1:{port}/v1/audio/transcriptions"),
        ..Default::default()
    };
    let out = builtin.models_list(Section::Asr, &input).unwrap();
    assert_eq!(out["ok"], true);
    assert_eq!(
        out["models"],
        serde_json::json!(["FunAudioLLM/SenseVoiceSmall", "Qwen/Qwen2.5-7B-Instruct"])
    );
    assert!(!out.to_string().contains("sk-stored-asr-secret"));
    let head = rx.recv().unwrap();
    assert!(head.starts_with("GET /v1/models "), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer sk-stored-asr-secret"),
        "{head}"
    );

    // 同基础地址跨分区复用已保存密钥（例如 TTS 填同一个基础地址且留空密钥时，自动沿用 ASR 已保存的本机密钥）
    let mut settings = builtin.settings();
    settings.builtin_models.asr = Some(moss_voice::settings::BuiltinProvider {
        kind: ProviderKind::Openai,
        url: format!("http://127.0.0.1:{port}/v1/audio/transcriptions"),
        model: "FunAudioLLM/SenseVoiceSmall".into(),
        voice: String::new(),
        speed: 1.0,
    });
    moss_voice::settings::save(&builtin.settings_file(), &settings).unwrap();
    let tts_input = SectionInput {
        url: format!("http://127.0.0.1:{port}/v1"),
        ..Default::default()
    };
    assert_eq!(
        builtin
            .secret_or_same_origin(Section::Tts, &tts_input)
            .unwrap()
            .as_deref(),
        Some("sk-stored-asr-secret")
    );

    // 切换服务商或域名且密钥留空时：严禁将本分区旧服务商密钥发往新域名，若有新域名同源分区密钥则改用新同源密钥，无同源密钥则在保存时清掉旧密钥
    builtin
        .models_save(
            &moss_voice::models::ModelsInput {
                llm: Some(SectionInput {
                    url: "https://maas.qianwenaiapi.com/compatible-mode/v1".into(),
                    api_key: "sk-qianwen-llm-secret".into(),
                    model: "qwen3.8-flash".into(),
                    ..Default::default()
                }),
                tts: Some(SectionInput {
                    url: "https://old-tts.example.test/v1".into(),
                    api_key: "sk-old-tts-vendor-secret".into(),
                    model: "tts-1".into(),
                    voice: "alloy".into(),
                    speed: Some(1.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
    let other_vendor_tts = SectionInput {
        url: "https://another-vendor.example.test/v1".into(),
        model: "tts-1".into(),
        voice: "alloy".into(),
        ..Default::default()
    };
    assert_eq!(
        builtin
            .secret_or_same_origin(Section::Tts, &other_vendor_tts)
            .unwrap(),
        None,
        "切换第三方域名且留空密钥时不得外泄旧 TTS 密钥"
    );
    let dashscope_tts = SectionInput {
        kind: ProviderKind::Dashscope,
        url: "https://maas.qianwenaiapi.com/api/v1".into(),
        model: "qwen-audio-3.1-tts-flash".into(),
        voice: "longanhuan_v3.1".into(),
        speed: Some(1.0),
        ..Default::default()
    };
    assert_eq!(
        builtin
            .secret_or_same_origin(Section::Tts, &dashscope_tts)
            .unwrap()
            .as_deref(),
        Some("sk-qianwen-llm-secret"),
        "切到千问合成且留空密钥时应复用同源千问大模型密钥而非旧 TTS 密钥"
    );
    builtin
        .models_save(
            &moss_voice::models::ModelsInput {
                tts: Some(dashscope_tts),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(
        builtin
            .host
            .db
            .lock()
            .unwrap()
            .get_secret("voice.tts.main")
            .unwrap()
            .as_deref(),
        Some("sk-qianwen-llm-secret"),
        "保存新服务商且留空密钥时应覆盖为新同源密钥"
    );
    builtin
        .models_save(
            &moss_voice::models::ModelsInput {
                tts: Some(SectionInput {
                    url: "http://127.0.0.1:11434/v1".into(),
                    model: "local-tts".into(),
                    voice: "alloy".into(),
                    speed: Some(1.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(
        builtin
            .host
            .db
            .lock()
            .unwrap()
            .get_secret("voice.tts.main")
            .unwrap(),
        None,
        "切换到无同源密钥的本机服务且留空密钥时应清掉旧云端密钥"
    );
}

#[test]
fn jev_save_shares_system_secret_switches_model_and_clears_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();

    // 初始未配置
    let initial = builtin.status();
    assert_eq!(initial["jev"]["key_set"], false);
    assert_eq!(initial["jev"]["model"], "typesafe/jev-1.14");

    // 保存 Jev 配置：写入与系统设置共用的 app_secrets["jev_api_key"] 与顶层 jev_enabled，且返回状态不回显密钥
    let saved = builtin
        .jev_save(
            Some(&serde_json::json!({
                "api_key": "sk-or-shared-jev-secret",
                "model": "typesafe/jev-1.13",
                "voice_enabled": true,
                "feedback_enabled": true,
            })),
            false,
        )
        .unwrap();
    assert_eq!(saved["jev"]["key_set"], true);
    assert_eq!(saved["jev"]["model"], "typesafe/jev-1.13");
    assert_eq!(saved["jev"]["voice_enabled"], true);
    assert_eq!(saved["jev"]["feedback_enabled"], true);
    assert!(!saved.to_string().contains("sk-or-shared-jev-secret"));
    assert_eq!(
        host.db
            .lock()
            .unwrap()
            .get_secret("jev_api_key")
            .unwrap()
            .as_deref(),
        Some("sk-or-shared-jev-secret")
    );
    assert!(moss_voice::settings::jev_feedback_enabled(
        &builtin.settings_file()
    ));

    // 顶部第 5 栏下拉框通过 local_save 切换模型版本与开关
    let switched = builtin
        .local_save(&serde_json::json!({
            "jev_filter_enabled": true,
            "jev_model": "typesafe/jev-1.14",
        }))
        .unwrap();
    assert_eq!(switched["jev"]["model"], "typesafe/jev-1.14");
    assert!(
        builtin
            .local_save(&serde_json::json!({"jev_model": "gpt-4o"}))
            .is_err()
    );

    // 删除此项：清理 jev_api_key，并重置语音筛选与加工反馈开关
    let cleared = builtin.jev_save(None, true).unwrap();
    assert_eq!(cleared["jev"]["key_set"], false);
    assert_eq!(cleared["jev"]["voice_enabled"], false);
    assert_eq!(cleared["jev"]["feedback_enabled"], false);
    assert_eq!(
        host.db.lock().unwrap().get_secret("jev_api_key").unwrap(),
        None
    );
}

#[test]
fn screen_pack_auto_keep_is_opt_in_and_only_retries_once_a_day() {
    use moss_voice::settings::DeviceHardwarePrefs;
    use moss_voice_server::ScreenCardStyle;
    const NOW: u64 = 1_790_000_000;
    let hud = moss_voice_server::screen_pack::pack_marker(ScreenCardStyle::MossHud);
    let bot = moss_voice_server::screen_pack::pack_marker(ScreenCardStyle::GoggleBot);
    // 判定：没记录 / 没装 → 推；版本+画风+指纹完全一致 → 不推；无指纹旧两段式或指纹不符 → 推；等下载未过期 → 不推；过期 → 推；一天内自动推过 → 不推
    assert!(screen_pack_auto_due(None, &hud, NOW));
    assert!(screen_pack_auto_due(
        Some(&DeviceHardwarePrefs::default()),
        &hud,
        NOW
    ));
    let installed = DeviceHardwarePrefs {
        screen_pack: Some(hud.clone()),
        ..DeviceHardwarePrefs::default()
    };
    assert!(!screen_pack_auto_due(Some(&installed), &hud, NOW));
    let legacy_unfingerprinted = DeviceHardwarePrefs {
        screen_pack: Some("moss-screen-2:moss_hud".into()),
        ..DeviceHardwarePrefs::default()
    };
    assert!(
        screen_pack_auto_due(Some(&legacy_unfingerprinted), &hud, NOW),
        "旧两段式无指纹标记要自动同步重刷补齐指纹"
    );
    let stale_fp = DeviceHardwarePrefs {
        screen_pack: Some("moss-screen-2:moss_hud:0000000000000000".into()),
        ..DeviceHardwarePrefs::default()
    };
    assert!(
        screen_pack_auto_due(Some(&stale_fp), &hud, NOW),
        "底包或字库指纹变化后要自动更新"
    );
    let old_version = DeviceHardwarePrefs {
        screen_pack: Some("moss-screen-1:moss_hud".into()),
        ..DeviceHardwarePrefs::default()
    };
    let other_style = DeviceHardwarePrefs {
        screen_pack: Some(bot.clone()),
        ..DeviceHardwarePrefs::default()
    };
    assert!(
        screen_pack_auto_due(Some(&other_style), &hud, NOW),
        "换了画风要重刷，开机待命画面才跟着换"
    );
    assert!(
        screen_pack_auto_due(Some(&old_version), &hud, NOW),
        "旧无字库版 moss-screen-1 要自动升级重刷补回中文字库"
    );
    let pending = DeviceHardwarePrefs {
        screen_pack_pending: Some(hud.clone()),
        screen_pack_pending_at: Some(NOW - 60),
        ..DeviceHardwarePrefs::default()
    };
    assert!(
        !screen_pack_auto_due(Some(&pending), &hud, NOW),
        "等下载时不重复推"
    );
    let expired = DeviceHardwarePrefs {
        screen_pack_pending_at: Some(NOW - 31 * 60),
        ..pending.clone()
    };
    assert!(screen_pack_auto_due(Some(&expired), &hud, NOW));
    let tried = DeviceHardwarePrefs {
        screen_pack_auto_at: Some(NOW - 3600),
        ..DeviceHardwarePrefs::default()
    };
    assert!(
        !screen_pack_auto_due(Some(&tried), &hud, NOW),
        "失败也不在一天内反复重启设备"
    );
    assert!(screen_pack_auto_due(Some(&tried), &hud, NOW + 24 * 3600));

    // 开关默认关；关着或服务没运行时什么也不推
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();
    assert!(!builtin.settings().builtin_local.screen_pack_auto_keep);
    assert_eq!(builtin.screen_pack_auto_tick(NOW), None);
    let saved = builtin
        .local_save(&serde_json::json!({"screen_pack_auto_keep": true}))
        .unwrap();
    assert_eq!(saved["local"]["settings"]["screen_pack_auto_keep"], true);
    assert_eq!(saved["restarted"], false, "开关热生效，不重启服务");
    assert_eq!(builtin.screen_pack_auto_tick(NOW), None, "服务没运行时不推");
    assert!(
        builtin
            .local_save(&serde_json::json!({"screen_pack_auto_keep": "yes"}))
            .is_err()
    );
    // 换画风：清掉“一天内已自动推过”，开着自动保持时马上按新画风重刷；状态里旧画风标为 style_outdated
    moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
        let entry = s.builtin_device_prefs.entry("dev-1".into()).or_default();
        entry.screen_pack = Some(hud.clone());
        entry.screen_pack_auto_at = Some(NOW);
        Ok(())
    })
    .unwrap();
    builtin
        .local_save(&serde_json::json!({"screen_card_style": "moss_hud"}))
        .unwrap();
    assert_eq!(
        moss_voice::settings::load(&builtin.settings_file()).builtin_device_prefs["dev-1"]
            .screen_pack_auto_at,
        Some(NOW),
        "画风没变不清"
    );
    builtin
        .local_save(&serde_json::json!({"screen_card_style": "goggle_bot"}))
        .unwrap();
    let settings = moss_voice::settings::load(&builtin.settings_file());
    assert_eq!(
        settings.builtin_device_prefs["dev-1"].screen_pack_auto_at,
        None
    );
    assert_eq!(current_screen_pack_marker(&settings), bot);
    assert!(screen_pack_auto_due(
        settings.builtin_device_prefs.get("dev-1"),
        &current_screen_pack_marker(&settings),
        NOW
    ));

    // n84：导入 / 移除基础资源包会立即刷新指纹并清掉 24 小时自动节流；重复导入同一份底包不清节流
    let base_file = temp.path().join("custom-base.bin");
    let base_bytes = moss_voice_server::wake_assets::pack_assets(&[
        moss_voice_server::wake_assets::AssetEntry {
            name: "font.bin".into(),
            data: vec![1, 2, 3, 4],
            width: 0,
            height: 0,
        },
        moss_voice_server::wake_assets::AssetEntry {
            name: "index.json".into(),
            data: br#"{"version":1,"text_font":"font.bin"}"#.to_vec(),
            width: 0,
            height: 0,
        },
    ])
    .unwrap();
    std::fs::write(&base_file, &base_bytes).unwrap();
    moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
        let entry = s.builtin_device_prefs.entry("dev-1".into()).or_default();
        entry.screen_pack = Some(bot.clone());
        entry.screen_pack_auto_at = Some(NOW);
        Ok(())
    })
    .unwrap();
    builtin.wakeword_import_base(&base_file).unwrap();
    let after_import = moss_voice::settings::load(&builtin.settings_file());
    assert_eq!(
        after_import.builtin_device_prefs["dev-1"].screen_pack_auto_at, None,
        "导入新底包后清掉自动节流"
    );
    let expected_with_base = expected_device_screen_pack_marker(
        ScreenCardStyle::GoggleBot,
        Some((20, 4)),
        Some((320, 240)),
        false,
        None,
        builtin.base_pack_fingerprint(),
        Some(&bot),
    );
    assert_ne!(expected_with_base, bot, "导入底包后期望标记指纹已更新");
    assert_eq!(
        moss_voice_server::screen_pack::compare_pack_marker(Some(&bot), &expected_with_base),
        moss_voice_server::screen_pack::PackMatchState::AssetsOutdated
    );

    // 重复导入内容完全一致的底包：不清空自动节流
    moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
        s.builtin_device_prefs
            .get_mut("dev-1")
            .unwrap()
            .screen_pack_auto_at = Some(NOW);
        Ok(())
    })
    .unwrap();
    builtin.wakeword_import_base(&base_file).unwrap();
    assert_eq!(
        moss_voice::settings::load(&builtin.settings_file()).builtin_device_prefs["dev-1"]
            .screen_pack_auto_at,
        Some(NOW),
        "重复导入相同底包不清节流"
    );

    // 移除底包：恢复无底包指纹并清掉自动节流
    builtin
        .wakeword_manage(&serde_json::json!({"action": "clear_base", "confirmed": true}))
        .unwrap();
    assert_eq!(
        moss_voice::settings::load(&builtin.settings_file()).builtin_device_prefs["dev-1"]
            .screen_pack_auto_at,
        None,
        "移除底包后清掉自动节流"
    );
    let expected_after_clear = expected_device_screen_pack_marker(
        ScreenCardStyle::GoggleBot,
        Some((20, 4)),
        Some((320, 240)),
        false,
        None,
        builtin.base_pack_fingerprint(),
        Some(&bot),
    );
    assert_eq!(expected_after_clear, bot);

    // 超大屏（800×480）因 8 MB 分区限制回退装入 20_4 字库后，常连模式重启重连尚未发 hello
    // （hello_json 快照仍留刷前 30_4，或固件报非标准 24_4）：已装 20_4 指纹仍应判为最新，底包变了立即判过期
    {
        let db = builtin.host.db.lock().unwrap();
        db.voice_device_seen("dev-800", "cli-800", "board", "2.0.4", "192.0.2.8", 1.0)
            .unwrap();
        db.voice_device_snapshot(
            "dev-800",
            "hello",
            r#"{"text_font":{"size":30,"bpp":4}}"#,
            2.0,
        )
        .unwrap();
    }
    let dev_800 = builtin
        .host
        .db
        .lock()
        .unwrap()
        .voice_device("dev-800")
        .unwrap();
    let none_fp = builtin.base_pack_fingerprint();
    let fallback_800_20 = moss_voice_server::screen_pack::pack_marker_for(
        ScreenCardStyle::GoggleBot,
        (800, 480),
        Some(moss_voice_server::glyph_font::PROFILES[2]),
        none_fp,
    )
    .unwrap();
    assert_eq!(
        expected_device_screen_pack_marker(
            ScreenCardStyle::GoggleBot,
            None,
            Some((800, 480)),
            false,
            dev_800.as_ref(),
            none_fp,
            Some(&fallback_800_20),
        ),
        fallback_800_20,
        "常连重连未发 hello、快照仍为 30_4 时，已装 20_4 回退包应视为最新"
    );
    assert_eq!(
        expected_device_screen_pack_marker(
            ScreenCardStyle::GoggleBot,
            Some((24, 4)),
            Some((800, 480)),
            false,
            dev_800.as_ref(),
            none_fp,
            Some(&fallback_800_20),
        ),
        fallback_800_20,
        "超大屏上报非标准字号时，已装 20_4 回退包同样视为最新"
    );
    let custom_fp = moss_voice_server::wake_assets::fingerprint_base_pack(Some(&base_bytes));
    assert_ne!(
        expected_device_screen_pack_marker(
            ScreenCardStyle::GoggleBot,
            None,
            Some((800, 480)),
            false,
            dev_800.as_ref(),
            custom_fp,
            Some(&fallback_800_20),
        ),
        fallback_800_20,
        "底包变更后 20_4 回退包必须立即判为过期"
    );

    // base-assets.bin 存在但暂不可读（这里用同名目录模拟 metadata 成功、fs::read 失败）：不得把空指纹写进缓存
    let bad_base = builtin.wakeword_dir().join("base-assets.bin");
    std::fs::create_dir_all(&bad_base).unwrap();
    assert_eq!(builtin.base_pack_fingerprint(), none_fp);
    assert!(
        builtin.wakeword_base_fp_cache.lock().unwrap().is_none(),
        "读取失败时不得污染底包指纹缓存"
    );
    std::fs::remove_dir_all(&bad_base).unwrap();
}

#[test]
fn chat_logs_are_on_by_default_persist_across_restarts_and_clear_on_demand() {
    use moss_voice_server::DeviceRegistry;
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();
    let initial = builtin.status();
    assert_eq!(initial["debug_enabled"], true, "默认开启对话记录");
    assert_eq!(initial["debug_traces"].as_array().unwrap().len(), 0);

    let registry = DbRegistry {
        db: host.db.clone(),
        host: host.clone(),
        settings_file: builtin.settings_file(),
        chat_log_enabled: builtin.debug_trace_enabled.clone(),
        chat_log_epoch: builtin.chat_log_epoch.clone(),
    };
    registry.record_chat_turn(&moss_voice_server::TurnDebugTrace {
        id: 0,
        at_ms: 1_790_000_123,
        device_id: "dev-chat".into(),
        session_id: "s-chat".into(),
        turn: 1,
        at: 1_790_000,
        raw_asr: "今天天气怎么样".into(),
        corrected_asr: "今天天气怎么样".into(),
        tools: vec![moss_voice_server::DebugToolTrace {
            name: "get_weather".into(),
            arguments: "{}".into(),
            result: "晴，22度".into(),
            ..Default::default()
        }],
        reply: "今天天气晴朗，气温22度，非常适合出门。".into(),
        spoken: "今天天气晴朗，".into(),
        outcome: "interrupted".into(),
        truncated: false,
        tools_dropped: 0,
        simulated: false,
        bench_window: false,
        ..Default::default()
    });

    // 模拟重启 BuiltinVoice 实例：历史记录仍在
    let restarted = init(Some(host)).0.unwrap();
    let after = restarted.status();
    let traces = after["debug_traces"].as_array().unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0]["device_id"], "dev-chat");
    assert_eq!(traces[0]["raw_asr"], "今天天气怎么样");
    assert_eq!(traces[0]["reply"], "今天天气晴朗，气温22度，非常适合出门。");
    assert_eq!(traces[0]["spoken"], "今天天气晴朗，");
    assert_eq!(traces[0]["outcome"], "interrupted");

    // 关掉开关：停录且导出诊断包不含历史对话，但本机数据库里的旧记录不被误删
    let toggled = restarted
        .local_save(&serde_json::json!({"chat_log_enabled": false}))
        .unwrap();
    assert_eq!(toggled["hot_applied"], true);
    assert_eq!(toggled["restarted"], false);
    assert_eq!(toggled["debug_enabled"], false);
    assert_eq!(toggled["debug_traces"].as_array().unwrap().len(), 1);
    let bundle = restarted.diagnostics_bundle();
    assert_eq!(bundle["debug_traces"].as_array().unwrap().len(), 0);

    let restarted_registry = DbRegistry {
        db: restarted.host.db.clone(),
        host: restarted.host.clone(),
        settings_file: restarted.settings_file(),
        chat_log_enabled: restarted.debug_trace_enabled.clone(),
        chat_log_epoch: restarted.chat_log_epoch.clone(),
    };
    let secret_turn = || moss_voice_server::TurnDebugTrace {
        id: 0,
        at_ms: 1_790_000_200,
        device_id: "dev-chat".into(),
        session_id: "s-chat".into(),
        turn: 2,
        at: 1_790_000,
        raw_asr: "Authorization Bearer fake_val api_key fake_api_val".into(),
        corrected_asr: "password fake_pw_val".into(),
        tools: vec![moss_voice_server::DebugToolTrace {
            name: "fetch".into(),
            arguments: r#"{"token":"fake_json_tok"}"#.into(),
            result: "https://fakeuser:fakepass@example.invalid/x?token=fake2".into(),
            ..Default::default()
        }],
        reply: "Bearer fake_reply_tok".into(),
        spoken: "done".into(),
        outcome: "done".into(),
        truncated: false,
        tools_dropped: 0,
        simulated: false,
        bench_window: false,
        ..Default::default()
    };
    restarted_registry.record_chat_turn(&secret_turn());
    assert_eq!(
        restarted
            .host
            .db
            .lock()
            .unwrap()
            .list_voice_chat_logs(20)
            .unwrap()
            .len(),
        1,
        "关闭对话记录期间新轮次不得写入数据库"
    );

    // #11：重新开启后，含空格分隔和键值写法的合成凭据经 record_chat_turn -> DB -> 诊断导出整条链路都不得留存原文
    let reenabled = restarted
        .local_save(&serde_json::json!({"chat_log_enabled": true}))
        .unwrap();
    restarted_registry.record_chat_turn(&secret_turn());
    let reenabled_status = restarted.status();
    let exported = restarted.diagnostics_bundle();
    let db_rows = restarted
        .host
        .db
        .lock()
        .unwrap()
        .list_voice_chat_logs(20)
        .unwrap();
    assert_eq!(db_rows.len(), 2, "重新开启后应持久化第 2 轮脱敏记录");
    let combined = format!("{}{}{}{:?}", reenabled, reenabled_status, exported, db_rows);
    for leak in [
        "fake_val",
        "fake_api_val",
        "fake_pw_val",
        "fake_json_tok",
        "fakeuser",
        "fakepass",
        "fake2",
        "fake_reply_tok",
    ] {
        assert!(
            !combined.contains(leak),
            "{leak} leaked across DB/export: {combined}"
        );
    }

    // 显式清空：本机记录清零
    let cleared = restarted
        .local_save(&serde_json::json!({"clear_debug_traces": true}))
        .unwrap();
    assert_eq!(cleared["hot_applied"], true);
    assert_eq!(cleared["restarted"], false);
    assert_eq!(cleared["debug_traces"].as_array().unwrap().len(), 0);
}

#[test]
fn screen_pack_counts_as_installed_only_after_the_device_downloads_it() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();
    let registry = DbRegistry {
        db: host.db.clone(),
        host: host.clone(),
        settings_file: builtin.settings_file(),
        chat_log_enabled: builtin.debug_trace_enabled.clone(),
        chat_log_epoch: builtin.chat_log_epoch.clone(),
    };
    // 推送后先“等下载”：握手读到的偏好里还没有画风标记，不会按全屏发画面
    moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
        let entry = s.builtin_device_prefs.entry("dev-1".into()).or_default();
        entry.screen_pack_pending = Some("moss-screen-1:moss_hud".into());
        entry.screen_pack_pending_at = Some(1);
        entry.wake_target = Some("default".into());
        Ok(())
    })
    .unwrap();
    assert_eq!(
        registry.device_hardware_prefs("dev-1").unwrap().screen_pack,
        None
    );
    registry.assets_downloaded("someone-else");
    registry.assets_downloaded("dev-1");
    let prefs = moss_voice::settings::load(&builtin.settings_file()).builtin_device_prefs;
    let entry = &prefs["dev-1"];
    assert_eq!(entry.screen_pack.as_deref(), Some("moss-screen-1:moss_hud"));
    assert_eq!(entry.screen_pack_pending, None);
    assert_eq!(
        entry.wake_target.as_deref(),
        Some("default"),
        "一键刷入沿用上次的唤醒词"
    );
    assert!(
        !prefs.contains_key("someone-else"),
        "没推送过的设备不新增记录"
    );
    assert_eq!(
        registry
            .device_hardware_prefs("dev-1")
            .unwrap()
            .screen_pack
            .as_deref(),
        Some("moss-screen-1:moss_hud"),
        "下载完后重连握手按全屏画风显示"
    );
}

#[test]
fn n61_wake_word_settings_base_package_and_push_guards() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();
    let status = builtin.wakeword_status().unwrap();
    assert_eq!(
        status["word"]["text"], "你好 MOSS",
        "default suggestion is MOSS"
    );
    assert_eq!(status["word"]["command"], "ni hao mo si");
    assert!(status["base"].is_null() && status["devices"].as_array().unwrap().is_empty());

    let saved = builtin.wakeword_manage(&serde_json::json!({"action": "save", "text": " MOSS MOSS ", "command": "Mo Si  mo si", "threshold": 0.3})).unwrap();
    assert_eq!(
        saved["word"],
        serde_json::json!({"text": "MOSS MOSS", "command": "mo si mo si", "threshold": 0.3})
    );
    for bad in [
        serde_json::json!({"action": "save", "text": "MOSS", "command": "moss", "threshold": 0.2}),
        serde_json::json!({"action": "save", "text": "", "command": "mo si", "threshold": 0.2}),
        serde_json::json!({"action": "save", "text": "MOSS", "command": "mo si", "threshold": 2.0}),
        serde_json::json!({"action": "save", "text": "MOSS", "command": "mo si", "threshold": 0.2, "extra": 1}),
        serde_json::json!({"action": "push", "device_id": "dev", "target": "custom"}),
        serde_json::json!({"action": "push", "device_id": "dev", "target": "custom", "confirmed": false}),
        serde_json::json!({"action": "clear_base", "confirmed": false}),
        serde_json::json!({"action": "flash"}),
        // n70：screen_pack 只能是布尔值，且不能夹带别的字段
        serde_json::json!({"action": "push", "device_id": "dev", "target": "custom", "confirmed": true, "screen_pack": "yes"}),
        serde_json::json!({"action": "push", "device_id": "dev", "target": "custom", "confirmed": true, "screen_pack": true, "extra": 1}),
    ] {
        assert!(builtin.wakeword_manage(&bad).is_err(), "{bad}");
    }
    assert_eq!(
        builtin.wakeword_status().unwrap()["word"]["command"],
        "mo si mo si",
        "rejected saves change nothing"
    );
    let push = builtin.wakeword_manage(&serde_json::json!({"action": "push", "device_id": "never-allowed", "target": "custom", "confirmed": true}));
    assert_eq!(
        push.unwrap_err().code,
        "wakeword_device_invalid",
        "only allowed devices"
    );
    let push = builtin.wakeword_manage(&serde_json::json!({"action": "push", "device_id": "never-allowed", "target": "custom", "confirmed": true, "screen_pack": true}));
    assert_eq!(
        push.unwrap_err().code,
        "wakeword_device_invalid",
        "the full-screen pack follows the same per-device guard"
    );

    let base_path = temp.path().join("picked-assets.bin");
    let base = moss_voice_server::wake_assets::pack_assets(&[
        moss_voice_server::wake_assets::AssetEntry {
            name: "index.json".into(),
            data: br#"{"version":1,"text_font":"f.bin"}"#.to_vec(),
            width: 0,
            height: 0,
        },
        moss_voice_server::wake_assets::AssetEntry {
            name: "f.bin".into(),
            data: vec![1; 10],
            width: 0,
            height: 0,
        },
    ])
    .unwrap();
    std::fs::write(&base_path, &base).unwrap();
    assert_eq!(
        builtin.wakeword_import_base(&base_path).unwrap()["text_font"],
        "f.bin"
    );
    assert_eq!(builtin.wakeword_status().unwrap()["base"]["files"], 2);
    std::fs::write(&base_path, b"not a package").unwrap();
    assert!(builtin.wakeword_import_base(&base_path).is_err());
    assert_eq!(
        builtin.wakeword_status().unwrap()["base"]["files"],
        2,
        "a bad import keeps the old base"
    );
    builtin
        .wakeword_manage(&serde_json::json!({"action": "clear_base", "confirmed": true}))
        .unwrap();
    assert!(builtin.wakeword_status().unwrap()["base"].is_null());
}

/// n110：完整过程记录打开时，服务启动写一步“启动了什么”（端口、对话模型、每类本地模型选了什么 / 装没装 /
/// 加载上没有），停止时写一步；本地模型程序的启动、输出与意外退出由观察者写进“语音·本地模型程序”。
#[test]
fn service_start_and_stop_are_traced_with_every_local_model_kind() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let port = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    {
        let db = host.db.lock().unwrap();
        db.set_secret("voice.llm.main", "sk-test-llm", 1.0).unwrap();
        db.set_secret("voice.asr.main", "sk-test-asr", 1.0).unwrap();
        db.set_secret("voice.tts.main", "sk-test-tts", 1.0).unwrap();
    }
    let cloud = |url: &str, model: &str| BuiltinProvider {
        kind: ProviderKind::Openai,
        url: url.into(),
        model: model.into(),
        voice: "alloy".into(),
        speed: 1.0,
    };
    let mut voice_settings = moss_voice::VoiceSettings {
        builtin_ws_port: port(),
        builtin_http_port: port(),
        ..Default::default()
    };
    voice_settings.builtin_models = BuiltinModels {
        llm: Some(cloud("https://llm.example.test/v1", "gpt-test")),
        asr: Some(cloud(
            "https://asr.example.test/v1/audio/transcriptions",
            "whisper-1",
        )),
        tts: Some(cloud("https://tts.example.test/v1/audio/speech", "tts-1")),
    };
    // 选了本地判停但这台电脑没装：如实写“选了但没安装”
    voice_settings.builtin_local.vad = true;
    voice_settings.builtin_local.vad_model = "silero_vad_v6".into();
    moss_voice::settings::save(&temp.path().join("settings.json"), &voice_settings).unwrap();
    laser_runtime::settings_store::update(
        &laser_runtime::settings_store::settings_path(temp.path()),
        |map| {
            map.insert(
                laser_runtime::process_trace::SETTING_KEY.into(),
                serde_json::json!(true),
            );
            Ok(())
        },
    )
    .unwrap();

    let builtin = init(Some(host.clone())).0.unwrap();
    builtin.start().unwrap();
    builtin.stop();
    let trace = host.api_service.process_trace().unwrap().clone();
    trace.flush();
    let mut lines = Vec::new();
    for day in std::fs::read_dir(trace.dir()).unwrap().flatten() {
        let text = std::fs::read_to_string(
            day.path()
                .join(laser_runtime::process_trace::TIMELINE_JSONL),
        )
        .unwrap_or_default();
        lines.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok()),
        );
    }
    let start = lines
        .iter()
        .find(|line| line["kind"] == "service_start")
        .expect("服务启动写进记录");
    assert_eq!(start["flow"], "语音·服务");
    assert_eq!(start["mode"], "chat");
    assert_eq!(
        start["ws_port"].as_u64(),
        Some(u64::from(voice_settings.builtin_ws_port))
    );
    let kinds: Vec<&str> = start["local_models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        vec!["判停", "降噪", "命令词抗噪", "语义判停", "识别", "合成"]
    );
    let vad = &start["local_models"][0];
    assert_eq!(vad["model_id"], "silero_vad_v6");
    assert_eq!(vad["installed"], false);
    assert_eq!(vad["loaded"], false);
    assert_eq!(vad["status"], "选了但没安装，这次没用上");
    let step = start["step"].as_str().unwrap();
    assert!(
        step.starts_with("内置语音服务启动：设备连接端口 "),
        "{step}"
    );
    assert!(step.contains("LLM:gpt-test"), "{step}");
    assert!(step.contains("判停 "), "{step}");
    assert!(
        !start.to_string().contains("sk-test"),
        "记录里不出现密钥：{start}"
    );
    // 降噪、命令词、语义判停没开：写“未开启”，不冒充云端（n110 复审 7）
    for item in &start["local_models"].as_array().unwrap()[1..4] {
        assert_eq!(item["status"], "未开启", "{item}");
    }
    assert_eq!(start["local_models"][4]["status"], "未开启（用云端）");
    let stop = lines
        .iter()
        .find(|line| line["kind"] == "service_stop")
        .expect("服务停止写进记录");
    assert!(stop["seq"].as_u64() > start["seq"].as_u64());
    // 纯云端：没有本地模型程序，不说“结束了本地模型程序”；手动停止不是重启
    assert_eq!(stop["step"], "内置语音服务停止");
    assert_eq!(stop["restarting"], false);
    assert_eq!(stop["ended"], serde_json::json!([]));
}

/// 本地模型程序的输出与意外退出写进“语音·本地模型程序”，写之前去掉疑似密钥；记录关着时什么都不写。
#[test]
fn helper_observer_traces_output_and_exit_with_secrets_scrubbed() {
    use moss_voice_providers::local::HelperObserver;
    let temp = tempfile::tempdir().unwrap();
    let trace = laser_runtime::ProcessTrace::new(temp.path());
    let observer = TraceHelperObserver {
        trace: trace.clone(),
    };
    observer.stderr_line("本地模型程序", "关着时不写");
    trace.flush();
    assert!(!trace.dir().exists(), "开关关着不建记录目录");
    laser_runtime::settings_store::update(
        &laser_runtime::settings_store::settings_path(temp.path()),
        |map| {
            map.insert(
                laser_runtime::process_trace::SETTING_KEY.into(),
                serde_json::json!(true),
            );
            Ok(())
        },
    )
    .unwrap();
    // 开关每 2 秒最多读一次设置文件：换一个新记录器，免得测试干等缓存过期
    let trace = laser_runtime::ProcessTrace::new(temp.path());
    let observer = TraceHelperObserver {
        trace: trace.clone(),
    };
    observer.started("本地模型程序", Some(42));
    observer.stderr_line(
        "本地模型程序",
        "onnxruntime warning token=sk-abcdefghijklmnopqrstuv",
    );
    observer.exited("语义判停程序", "意外退出（退出码 3）");
    trace.flush();
    let day = std::fs::read_dir(trace.dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL)).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let kinds: Vec<&str> = lines.iter().filter_map(|l| l["kind"].as_str()).collect();
    assert_eq!(kinds, vec!["helper_start", "helper_stderr", "helper_exit"]);
    assert!(lines.iter().all(|l| l["flow"] == "语音·本地模型程序"));
    assert!(!text.contains("sk-abcdefghijklmnopqrstuv"), "{text}");
    assert!(
        lines[1]["step"]
            .as_str()
            .unwrap()
            .starts_with("本地模型程序输出：onnxruntime warning")
    );
    assert_eq!(lines[2]["step"], "语义判停程序停了：意外退出（退出码 3）");
}

#[test]
fn auto_allow_devices_and_tokenless_grace_period_for_bound_allowed_device() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();
    assert!(!builtin.settings().builtin_local.auto_allow_devices);

    let registry = DbRegistry {
        db: host.db.clone(),
        host: host.clone(),
        settings_file: builtin.settings_file(),
        chat_log_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        chat_log_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };

    // 1. 默认关闭自动允许：新设备首次连入为 Pending，不允许无 token 建会话
    let visit_a = DeviceVisit {
        device_id: "aa:bb:cc:00:00:01".into(),
        client_id: "client-uuid-a".into(),
        model: "lichuang-dev".into(),
        firmware_version: "2.2.0".into(),
        ip: "192.0.2.10".into(),
    };
    assert_eq!(registry.seen(&visit_a).unwrap(), DeviceStatus::Pending);
    assert!(!registry.allow_tokenless_session("aa:bb:cc:00:00:01", "client-uuid-a"));

    // 2. 允许后、OTA 快照写入前：放行；冒用或空 Client-Id 仍拒绝
    builtin
        .set_device_status("aa:bb:cc:00:00:01", "allowed")
        .unwrap();
    assert!(registry.allow_tokenless_session("aa:bb:cc:00:00:01", "client-uuid-a"));
    assert!(!registry.allow_tokenless_session("aa:bb:cc:00:00:01", "spoofed-client"));
    assert!(!registry.allow_tokenless_session("aa:bb:cc:00:00:01", ""));

    // 3. 设备重启完成已绑定 OTA（写入 SNAPSHOT_INFO 快照并获签 token）后：宽限期自动关闭，无 token 拒绝
    registry.device_snapshot(
        "aa:bb:cc:00:00:01",
        moss_voice_server::SNAPSHOT_INFO,
        r#"{"application":{"version":"2.2.0"}}"#,
    );
    assert!(
        !registry.allow_tokenless_session("aa:bb:cc:00:00:01", "client-uuid-a"),
        "已完成绑定 OTA 的设备须携带有效 token，不能继续免 token 直连"
    );

    // 4. 撤销后再重新允许：宽限期重新打开，直到下一次绑定 OTA
    std::thread::sleep(std::time::Duration::from_millis(2));
    builtin
        .set_device_status("aa:bb:cc:00:00:01", "revoked")
        .unwrap();
    assert!(!registry.allow_tokenless_session("aa:bb:cc:00:00:01", "client-uuid-a"));
    builtin
        .set_device_status("aa:bb:cc:00:00:01", "allowed")
        .unwrap();
    assert!(
        registry.allow_tokenless_session("aa:bb:cc:00:00:01", "client-uuid-a"),
        "撤销后再允许应重新打开免 token 宽限期"
    );
    registry.device_snapshot(
        "aa:bb:cc:00:00:01",
        moss_voice_server::SNAPSHOT_INFO,
        r#"{"application":{"version":"2.2.1"}}"#,
    );
    assert!(
        !registry.allow_tokenless_session("aa:bb:cc:00:00:01", "client-uuid-a"),
        "重新允许后一旦完成下次 OTA，宽限期再次关闭"
    );

    // 5. decided_at 超过 7 天（即使从未写过 SNAPSHOT_INFO）：拒绝无 token 连接
    let visit_expired = DeviceVisit {
        device_id: "aa:bb:cc:00:00:09".into(),
        client_id: "client-uuid-expired".into(),
        model: "lichuang-dev".into(),
        firmware_version: "2.2.0".into(),
        ip: "192.0.2.19".into(),
    };
    assert_eq!(
        registry.seen(&visit_expired).unwrap(),
        DeviceStatus::Pending
    );
    host.db
        .lock()
        .unwrap()
        .set_voice_device_status(
            "aa:bb:cc:00:00:09",
            laser_runtime::VOICE_DEVICE_ALLOWED,
            now() - TOKENLESS_GRACE_SECS - 1.0,
        )
        .unwrap();
    assert!(
        !registry.allow_tokenless_session("aa:bb:cc:00:00:09", "client-uuid-expired"),
        "decided_at 超过 7 天宽限期后须拒绝无 token 连接"
    );

    // 6. 开启 auto_allow_devices 后：不批量放行已在排队的存量 Pending 设备（仍需逐台允许），
    //    仅对之后新连入的陌生设备自动设为 Allowed，且已手动 Revoked 的设备仍保持 Revoked
    builtin
        .set_device_status("aa:bb:cc:00:00:01", "revoked")
        .unwrap();
    let visit_b = DeviceVisit {
        device_id: "aa:bb:cc:00:00:02".into(),
        client_id: "client-uuid-b".into(),
        model: "lichuang-dev".into(),
        firmware_version: "2.2.0".into(),
        ip: "192.0.2.11".into(),
    };
    assert_eq!(registry.seen(&visit_b).unwrap(), DeviceStatus::Pending);

    let saved = builtin
        .local_save(&serde_json::json!({"auto_allow_devices": true}))
        .unwrap();
    assert_eq!(saved["local"]["settings"]["auto_allow_devices"], true);
    assert_eq!(saved["restarted"], false, "自动允许开关热生效，不重启服务");
    assert!(
        !registry.allow_tokenless_session("aa:bb:cc:00:00:02", "client-uuid-b"),
        "开启自动允许时不得批量放行已在排队的存量 Pending 设备"
    );
    assert_eq!(
        registry.seen(&visit_b).unwrap(),
        DeviceStatus::Pending,
        "存量 Pending 设备重连时仍保持 Pending，需由用户逐台确认"
    );

    let visit_c = DeviceVisit {
        device_id: "aa:bb:cc:00:00:03".into(),
        client_id: "client-uuid-c".into(),
        model: "box-3".into(),
        firmware_version: "2.2.0".into(),
        ip: "192.0.2.12".into(),
    };
    assert_eq!(
        registry.seen(&visit_c).unwrap(),
        DeviceStatus::Allowed,
        "开启自动允许后新连入设备直接记为 Allowed"
    );
    assert_eq!(
        registry.seen(&visit_a).unwrap(),
        DeviceStatus::Revoked,
        "已被手动撤销的设备不因自动允许开关而放行"
    );
}

#[test]
fn remove_device_clears_records_prefs_approvals_deliveries_rejects_revoked_and_reenters_pending() {
    use laser_contracts::{ApprovalKind, ApprovalRecord, ApprovalSource, ApprovalStatus};

    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();
    let registry = DbRegistry {
        db: host.db.clone(),
        host: host.clone(),
        settings_file: builtin.settings_file(),
        chat_log_enabled: builtin.debug_trace_enabled.clone(),
        chat_log_epoch: builtin.chat_log_epoch.clone(),
    };

    let visit_allowed = DeviceVisit {
        device_id: "aa:bb:cc:00:01:01".into(),
        client_id: "client-allowed-1".into(),
        model: "lichuang-dev".into(),
        firmware_version: "2.2.0".into(),
        ip: "192.0.2.21".into(),
    };
    let visit_pending = DeviceVisit {
        device_id: "aa:bb:cc:00:01:02".into(),
        client_id: "client-pending-2".into(),
        model: "box-3".into(),
        firmware_version: "2.2.0".into(),
        ip: "192.0.2.22".into(),
    };
    let visit_revoked = DeviceVisit {
        device_id: "aa:bb:cc:00:01:03".into(),
        client_id: "client-revoked-3".into(),
        model: "lichuang-dev".into(),
        firmware_version: "2.1.0".into(),
        ip: "192.0.2.23".into(),
    };
    let visit_keep = DeviceVisit {
        device_id: "aa:bb:cc:00:01:04".into(),
        client_id: "client-keep-4".into(),
        model: "lichuang-dev".into(),
        firmware_version: "2.2.0".into(),
        ip: "192.0.2.24".into(),
    };
    for v in [&visit_allowed, &visit_pending, &visit_revoked, &visit_keep] {
        assert_eq!(registry.seen(v).unwrap(), DeviceStatus::Pending);
    }
    builtin
        .set_device_status(&visit_allowed.device_id, "allowed")
        .unwrap();
    builtin
        .set_device_status(&visit_revoked.device_id, "revoked")
        .unwrap();
    builtin
        .set_device_status(&visit_keep.device_id, "allowed")
        .unwrap();

    // 写入四类快照、在线会话租约、有效设备 HMAC token、快捷控制偏好（含唤醒词/画风待下载标记）、
    // 待推送固件记录、未处理语音审批，以及不应被删除的对话记录与设备记忆摘要
    for (kind, payload) in [
        (
            moss_voice_server::SNAPSHOT_INFO,
            r#"{"application":{"version":"2.2.0"}}"#,
        ),
        (
            moss_voice_server::SNAPSHOT_HELLO,
            r#"{"protocol_version":1,"mcp":true}"#,
        ),
        (
            moss_voice_server::SNAPSHOT_STATUS,
            r#"{"audio_speaker":{"volume":55}}"#,
        ),
        (moss_voice_server::SNAPSHOT_LISTEN, r#"{"mode":"realtime"}"#),
    ] {
        registry.device_snapshot(&visit_allowed.device_id, kind, payload);
    }
    let live_lease = registry
        .open_session(
            &visit_allowed.device_id,
            &visit_allowed.client_id,
            "sess-live-1",
        )
        .unwrap();
    let keep_lease = registry
        .open_session(&visit_keep.device_id, &visit_keep.client_id, "sess-keep-4")
        .unwrap();
    let secret = registry.token_secret().unwrap();
    let signer = moss_xiaozhi_proto::TokenSigner::new(secret);
    let issued_at = (now() as u64).max(1);
    let old_valid_token = signer.issue(
        &visit_allowed.client_id,
        &visit_allowed.device_id,
        issued_at,
    );
    assert!(signer.verify(
        &old_valid_token,
        &visit_allowed.client_id,
        &visit_allowed.device_id,
        issued_at + 10,
    ));

    moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
        let p1 = s
            .builtin_device_prefs
            .entry(visit_allowed.device_id.clone())
            .or_default();
        p1.volume = Some(35);
        p1.brightness = Some(70);
        p1.theme = Some("dark".into());
        p1.talk_mode = Some("press_to_talk".into());
        p1.wake_target = Some("default".into());
        p1.screen_pack_pending = Some("moss-screen-2:moss_hud".into());
        p1.screen_pack_pending_at = Some(1_790_000_000);
        let p4 = s
            .builtin_device_prefs
            .entry(visit_keep.device_id.clone())
            .or_default();
        p4.volume = Some(80);
        Ok(())
    })
    .unwrap();

    let make_approval = |id: &str, dev_id: &str, status: ApprovalStatus| ApprovalRecord {
        approval_id: id.into(),
        status,
        kind: ApprovalKind::WorkflowConfirmSend,
        target_record: "wf-test".into(),
        digest: "sha256:test".into(),
        summary: "测试语音审批".into(),
        source: ApprovalSource::Voice,
        session_id: Some(dev_id.into()),
        request_id: None,
        connection: None,
        created_at: 1000.0,
        decided_at: None,
        expires_at: 2000.0,
        updated_at: 1000.0,
        outcome: None,
        request: None,
    };
    {
        let db = host.db.lock().unwrap();
        db.insert_approval(&make_approval(
            "appr-pending-1",
            &visit_allowed.device_id,
            ApprovalStatus::Pending,
        ))
        .unwrap();
        db.insert_approval(&make_approval(
            "appr-approved-1",
            &visit_allowed.device_id,
            ApprovalStatus::Approved,
        ))
        .unwrap();
        db.insert_approval(&make_approval(
            "appr-keep-4",
            &visit_keep.device_id,
            ApprovalStatus::Pending,
        ))
        .unwrap();

        let sha_a = "a".repeat(64);
        let sha_b = "b".repeat(64);
        db.add_firmware_artifact(&sha_a, "2.3.0", "", 1024, 1000.0)
            .unwrap();
        db.add_firmware_artifact(&sha_b, "2.3.1", "", 1024, 1000.0)
            .unwrap();
        db.create_firmware_offer(
            "deliv-inflight-1",
            &visit_allowed.device_id,
            &sha_a,
            "2.3.0",
            &"2".repeat(32),
            9_999_999_999.0,
            1000.0,
        )
        .unwrap();
        assert!(
            db.update_firmware_download_if_revision("deliv-inflight-1", 0, "downloading", 1000.5)
                .unwrap()
        );
        db.create_firmware_offer(
            "deliv-pending-1",
            &visit_allowed.device_id,
            &sha_b,
            "2.3.1",
            &"1".repeat(32),
            9_999_999_999.0,
            1001.0,
        )
        .unwrap();
        db.create_firmware_offer(
            "deliv-keep-4",
            &visit_keep.device_id,
            &sha_a,
            "2.3.0",
            &"4".repeat(32),
            9_999_999_999.0,
            1001.0,
        )
        .unwrap();

        db.ensure_voice_memory(&visit_allowed.device_id, 1000.0)
            .unwrap();
        db.update_voice_memory_if_revision(&visit_allowed.device_id, 0, "偏好 3mm 椴木", 1001.0)
            .unwrap();
        db.append_voice_chat_log(&laser_runtime::VoiceChatLogRecord {
            id: 0,
            at_ms: 1_790_000_000_000,
            device_id: visit_allowed.device_id.clone(),
            session_id: "sess-live-1".into(),
            turn: 1,
            raw_asr: "你好".into(),
            corrected_asr: "你好".into(),
            reply: "我在".into(),
            spoken: "我在".into(),
            tools_json: "[]".into(),
            outcome: "done".into(),
            truncated: false,
            tools_dropped: 0,
            simulated: false,
            bench_window: false,
        })
        .unwrap();
    }

    // 1. 移除 allowed 设备成功：会话断开、审批过期、设备行与快照删除、偏好与待推送固件清理、对话记录与记忆保留
    let after_allowed = builtin.remove_device(&visit_allowed.device_id).unwrap();
    assert!(
        !live_lease.load(std::sync::atomic::Ordering::Acquire),
        "移除设备须立即断开其在线会话租约"
    );
    assert!(
        !host
            .api_service
            .voice_session_allowed(&visit_allowed.device_id, "sess-live-1")
    );
    assert!(
        keep_lease.load(std::sync::atomic::Ordering::Acquire),
        "其他设备的在线会话不受影响"
    );
    assert!(
        !after_allowed["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["device_id"] == visit_allowed.device_id)
    );
    let prefs_after = moss_voice::settings::load(&builtin.settings_file()).builtin_device_prefs;
    assert!(
        !prefs_after.contains_key(&visit_allowed.device_id),
        "移除后须删掉该设备的快捷控制与唤醒词/画风待推送偏好"
    );
    assert_eq!(prefs_after[&visit_keep.device_id].volume, Some(80));
    {
        let db = host.db.lock().unwrap();
        assert!(
            db.voice_device(&visit_allowed.device_id).unwrap().is_none(),
            "voice_devices 记录及同行快照须已删除"
        );
        assert_eq!(
            db.get_approval("appr-pending-1").unwrap().unwrap().status,
            ApprovalStatus::Expired
        );
        assert_eq!(
            db.get_approval("appr-approved-1").unwrap().unwrap().status,
            ApprovalStatus::Expired
        );
        assert_eq!(
            db.get_approval("appr-keep-4").unwrap().unwrap().status,
            ApprovalStatus::Pending
        );
        let deliveries = db.list_firmware_deliveries().unwrap();
        assert!(
            !deliveries
                .iter()
                .any(|d| d.delivery_id == "deliv-pending-1"),
            "仅针对该设备的待推送固件记录须已删除"
        );
        assert!(
            deliveries.iter().any(|d| d.delivery_id == "deliv-keep-4"),
            "其他设备的待推送固件记录须保留"
        );
        assert!(
            db.firmware_offer_by_token(&"2".repeat(32), 1002.0)
                .unwrap()
                .is_none(),
            "该设备未上报的下载中固件令牌须同步作废"
        );
        assert_eq!(
            db.voice_memory(&visit_allowed.device_id)
                .unwrap()
                .unwrap()
                .summary,
            "偏好 3mm 椴木",
            "设备记忆等用户数据不删除"
        );
        assert_eq!(
            db.list_voice_chat_logs(10).unwrap().len(),
            1,
            "对话记录不删除"
        );
    }

    // 2. 移除后设备持有的旧有效 HMAC token 不能建立会话
    assert!(
        registry
            .open_session(
                &visit_allowed.device_id,
                &visit_allowed.client_id,
                "sess-replay-1"
            )
            .is_err(),
        "设备记录被移除后，即使旧 HMAC token 签名未过期也不能建立会话"
    );
    assert_eq!(
        registry
            .authorize_http(&visit_allowed.device_id, &visit_allowed.client_id)
            .unwrap(),
        None
    );

    // 3. 移除 pending 设备同样成功
    builtin.remove_device(&visit_pending.device_id).unwrap();
    assert!(
        host.db
            .lock()
            .unwrap()
            .voice_device(&visit_pending.device_id)
            .unwrap()
            .is_none()
    );

    // 4. 移除 revoked 设备被后端 409 拒绝，记录保持 revoked 不变（防止删记录绕过撤销）
    let err = builtin.remove_device(&visit_revoked.device_id).unwrap_err();
    assert_eq!(err.status, 409);
    assert_eq!(err.code, "voice_device_revoked");
    assert_eq!(
        host.db
            .lock()
            .unwrap()
            .voice_device(&visit_revoked.device_id)
            .unwrap()
            .unwrap()
            .status,
        laser_runtime::VOICE_DEVICE_REVOKED
    );

    // 不存在的设备返回 404 voice_device_not_found
    let not_found = builtin.remove_device("aa:bb:cc:99:99:99").unwrap_err();
    assert_eq!(not_found.status, 404);
    assert_eq!(not_found.code, "voice_device_not_found");

    // 5. 移除后的设备再次来访：关闭自动允许时重新登记为 Pending（旧有效 token 仍建不了会话）；开启自动允许时直接放行为 Allowed
    assert_eq!(
        registry.seen(&visit_allowed).unwrap(),
        DeviceStatus::Pending,
        "移除后再来访视为新设备，重新进入 Pending"
    );
    assert!(
        registry
            .open_session(
                &visit_allowed.device_id,
                &visit_allowed.client_id,
                "sess-replay-2"
            )
            .is_err(),
        "重新进入 Pending 后未允许前，旧 token 依然不能建立会话"
    );
    builtin
        .local_save(&serde_json::json!({"auto_allow_devices": true}))
        .unwrap();
    assert_eq!(
        registry.seen(&visit_pending).unwrap(),
        DeviceStatus::Allowed,
        "已开启自动允许时，移除后的设备再次连入按新设备直接放行"
    );
}

#[test]
fn debug_text_inject_switch_creates_0600_token_hot_applies_and_revokes_on_disable_or_expiry() {
    let temp = tempfile::tempdir().unwrap();
    let host = std::sync::Arc::new(
        laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
    );
    let builtin = init(Some(host)).0.unwrap();

    // 1. 默认关闭，且直接注入返回 403 debug_inject_disabled
    let initial = builtin.status();
    assert_eq!(initial["debug_inject"]["enabled"], false);
    assert_eq!(
        initial["local"]["settings"]["debug_text_inject_enabled"],
        false
    );
    let err = builtin
        .device_request(
            "aa:bb:cc:11:22:33",
            "inject_text",
            Some(&serde_json::json!({"text": "激光机现在什么状态"})),
        )
        .unwrap_err();
    assert_eq!(err.status, 403);
    assert_eq!(err.code, "debug_inject_disabled");

    // 2. 开启开关：热生效不重启，生成 0600 令牌文件与无密钥元数据，status() 绝不泄露令牌原文
    let enabled = builtin
        .local_save(&serde_json::json!({"debug_text_inject_enabled": true}))
        .unwrap();
    assert_eq!(enabled["hot_applied"], true);
    assert_eq!(enabled["restarted"], false);
    assert_eq!(enabled["debug_inject"]["enabled"], true);
    assert_eq!(
        enabled["debug_inject"]["token_file_rel"],
        "debug/voice-debug.token"
    );
    let token_path = temp.path().join("debug/voice-debug.token");
    let meta_path = temp.path().join("debug/voice-debug.json");
    assert!(token_path.is_file(), "开启后必须生成令牌文件");
    assert!(meta_path.is_file(), "开启后必须生成元数据文件");
    let token = std::fs::read_to_string(&token_path).unwrap();
    let token = token.trim().to_owned();
    assert!(token.len() >= 64);
    let meta_text = std::fs::read_to_string(&meta_path).unwrap();
    assert!(!meta_text.contains(&token), "元数据文件绝不能包含令牌原文");
    let status_text = serde_json::to_string(&enabled).unwrap();
    assert!(
        !status_text.contains(&token),
        "status 响应绝不能泄露令牌原文"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "令牌文件权限必须为 0600");
    }

    // 3. 关闭开关：立即作废并删除令牌与元数据文件
    let disabled = builtin
        .local_save(&serde_json::json!({"debug_text_inject_enabled": false}))
        .unwrap();
    assert_eq!(disabled["hot_applied"], true);
    assert_eq!(disabled["debug_inject"]["enabled"], false);
    assert!(!token_path.exists(), "关闭开关后必须删除令牌文件");
    assert!(!meta_path.exists(), "关闭开关后必须删除元数据文件");

    // 4. 开启满 24 小时后自动作废并清理文件，再次注入被拒绝
    builtin
        .local_save(&serde_json::json!({"debug_text_inject_enabled": true}))
        .unwrap();
    assert!(token_path.is_file());
    builtin.expire_debug_inject_for_test();
    let after_expire = builtin.status();
    assert_eq!(after_expire["debug_inject"]["enabled"], false);
    assert!(!token_path.exists(), "过期后应立即清理令牌文件");
    let err2 = builtin
        .device_request(
            "aa:bb:cc:11:22:33",
            "inject_text",
            Some(&serde_json::json!({"text": "测试"})),
        )
        .unwrap_err();
    assert_eq!(err2.status, 403);
    assert_eq!(err2.code, "debug_inject_disabled");
}

#[test]
fn asr_corpus_capture_defaults_off_excludes_diagnostics_and_allows_delete_after_disable() {
    let temp = tempfile::tempdir().unwrap();
    let host = std::sync::Arc::new(
        laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
    );
    let builtin = init(Some(host)).0.unwrap();

    // 1. 默认关闭，不带 confirmed 拒绝开启，且关闭态下读取录音与保存标注返回 403
    let initial = builtin.status();
    assert_eq!(initial["asr_corpus"]["enabled"], false);
    assert_eq!(
        initial["local"]["settings"]["asr_corpus_capture_enabled"],
        false
    );
    let list_off = builtin
        .asr_corpus_manage(&serde_json::json!({"action": "list"}))
        .unwrap();
    assert_eq!(list_off["enabled"], false);
    assert_eq!(list_off["items"].as_array().unwrap().len(), 0);

    let need_confirm = builtin
        .asr_corpus_manage(&serde_json::json!({"action": "set_config", "enabled": true}))
        .unwrap_err();
    assert_eq!(need_confirm.status, 400);
    assert_eq!(need_confirm.code, "confirmation_required");

    // 2. 带 confirmed 开启后写入一条真实语料并保存标注
    let enabled = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "set_config",
            "enabled": true,
            "max_items": 50,
            "max_days": 5,
            "confirmed": true
        }))
        .unwrap();
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["max_items"], 50);
    assert_eq!(enabled["max_days"], 5);

    let canary = "绝密测试语料CANARY_98765_椴木三毫米";
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let saved_meta = builtin
        .asr_corpus
        .record(moss_voice_server::CapturedUtterance {
            session_id: "sess-host".into(),
            device_id: "aa:bb:cc:11:22:33".into(),
            turn: 1,
            captured_at_ms: now_ms,
            simulated: false,
            bench_window: false,
            pcm: vec![900; 4_000],
            sample_rate: 16_000,
            raw_text: canary.into(),
            corrected_text: canary.into(),
            interims: Vec::new(),
            asr_model: "本地 sense_voice".into(),
            vad_engine: "本地判停 Silero v6.2".into(),
            endpoint: Default::default(),
            outcome: "done".into(),
        })
        .unwrap()
        .expect("开启后真实语音应落盘");

    let label_res = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "save_label",
            "date": saved_meta.date,
            "turn_id": saved_meta.turn_id,
            "reference_text": canary,
            "verified_correct": true,
            "tags": ["材料名", "数字"]
        }))
        .unwrap();
    assert_eq!(label_res["labeled_count"], 1);

    let audio_res = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "audio",
            "date": saved_meta.date,
            "turn_id": saved_meta.turn_id,
        }))
        .unwrap();
    assert!(
        audio_res["audio_data_url"]
            .as_str()
            .is_some_and(|u| u.starts_with("data:audio/wav;base64,"))
    );

    // 3. 诊断包严禁包含任何语料文本、标注或录音数据
    let bundle = builtin.diagnostics_bundle();
    let bundle_json = serde_json::to_string(&bundle).unwrap();
    assert!(
        !bundle_json.contains(canary),
        "诊断包绝不能包含识别语料原文或标注"
    );
    assert!(
        !bundle_json.contains("asr-corpus"),
        "诊断包绝不能包含识别语料路径或目录"
    );
    assert!(
        bundle["excluded"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|v| v == "识别测评录音语料与标注")),
        "诊断包 excluded 应显式注明排除识别语料"
    );

    // 4. 关闭开关但不立即删除：列表隐藏明细、audio/save_label 返回 403，但关闭后仍允许 delete/clear 删除已有语料
    let disabled_keep = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "set_config",
            "enabled": false,
            "delete_existing": false
        }))
        .unwrap();
    assert_eq!(disabled_keep["enabled"], false);
    assert_eq!(disabled_keep["total_count"], 1);
    assert_eq!(disabled_keep["items"].as_array().unwrap().len(), 0);
    let retained = disabled_keep["retained_items"].as_array().unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0]["turn_id"], saved_meta.turn_id);
    assert!(retained[0].get("raw_text").is_none());
    assert!(retained[0].get("corrected_text").is_none());
    assert!(retained[0].get("label").is_none());

    let blocked_audio = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "audio",
            "date": saved_meta.date,
            "turn_id": saved_meta.turn_id,
        }))
        .unwrap_err();
    assert_eq!(blocked_audio.status, 403);
    assert_eq!(blocked_audio.code, "asr_corpus_disabled");

    let deleted_after_off = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "delete",
            "date": saved_meta.date,
            "turn_id": saved_meta.turn_id,
        }))
        .unwrap();
    assert_eq!(deleted_after_off["deleted"], true);
    assert_eq!(deleted_after_off["total_count"], 0);

    // 5. 关闭开关时勾选同时删除（delete_existing: true）应立即清空全部语料
    builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "set_config",
            "enabled": true,
            "confirmed": true
        }))
        .unwrap();
    builtin
        .asr_corpus
        .record(moss_voice_server::CapturedUtterance {
            session_id: "sess-host".into(),
            device_id: "aa:bb:cc:11:22:33".into(),
            turn: 2,
            captured_at_ms: now_ms + 5_000,
            simulated: false,
            bench_window: false,
            pcm: vec![700; 3_200],
            sample_rate: 16_000,
            raw_text: "停下".into(),
            corrected_text: "停下".into(),
            interims: Vec::new(),
            asr_model: "本地 sense_voice".into(),
            vad_engine: "本地判停 Silero v6.2".into(),
            endpoint: Default::default(),
            outcome: "done".into(),
        })
        .unwrap()
        .unwrap();
    let disabled_wipe = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "set_config",
            "enabled": false,
            "delete_existing": true
        }))
        .unwrap();
    assert_eq!(disabled_wipe["enabled"], false);
    assert_eq!(disabled_wipe["deleted_count"], 1);
    assert_eq!(disabled_wipe["total_count"], 0);
}

#[test]
fn bench_recordings_validates_quality_stores_per_speaker_excludes_diagnostics_and_requires_confirm_to_clear()
 {
    use base64::Engine as _;

    let temp = tempfile::tempdir().unwrap();
    let host = std::sync::Arc::new(
        laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
    );
    let builtin = init(Some(host)).0.unwrap();

    let make_wav_b64 = |sample_rate: u32, duration_ms: u64, amplitude: f32| -> String {
        let count = ((u64::from(sample_rate) * duration_ms) / 1000) as usize;
        let samples: Vec<i16> = (0..count)
            .map(|i| {
                let phase = 2.0 * std::f32::consts::PI * 440.0 * (i as f32) / (sample_rate as f32);
                (phase.sin() * amplitude * 32767.0)
                    .round()
                    .clamp(-32768.0, 32767.0) as i16
            })
            .collect();
        let wav = moss_voice_providers::wav::encode_wav(&samples, sample_rate);
        format!(
            "data:audio/wav;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&wav)
        )
    };

    // 1. 初始列表包含默认语料且无录音
    let initial = builtin
        .bench_recordings_manage(&serde_json::json!({"action": "list"}))
        .unwrap();
    assert_eq!(initial["recordings_dir_rel"], "voice-bench/recordings");
    assert_eq!(initial["total_recordings"], 0);
    assert!(initial["corpus_count"].as_u64().unwrap() >= 12);

    // 2. 质量校验：过短(<0.5s)、过长(>15s)、过轻、削顶、路径穿越均必须拒绝
    let too_short = builtin
        .bench_recordings_manage(&serde_json::json!({
            "action": "save",
            "speaker": "我",
            "corpus_id": "status_fast_read",
            "audio_wav_base64": make_wav_b64(16_000, 200, 0.35),
        }))
        .unwrap_err();
    assert_eq!(too_short.code, "bench_recording_quality_failed");

    let too_quiet = builtin
        .bench_recordings_manage(&serde_json::json!({
            "action": "save",
            "speaker": "我",
            "corpus_id": "status_fast_read",
            "audio_wav_base64": make_wav_b64(16_000, 1200, 0.002),
        }))
        .unwrap_err();
    assert_eq!(too_quiet.code, "bench_recording_quality_failed");

    let clipped_samples: Vec<i16> = vec![32767; 16_000];
    let clipped_b64 = base64::engine::general_purpose::STANDARD.encode(
        moss_voice_providers::wav::encode_wav(&clipped_samples, 16_000),
    );
    let clipped = builtin
        .bench_recordings_manage(&serde_json::json!({
            "action": "save",
            "speaker": "我",
            "corpus_id": "status_fast_read",
            "audio_wav_base64": clipped_b64,
        }))
        .unwrap_err();
    assert_eq!(clipped.code, "bench_recording_quality_failed");

    let bad_speaker = builtin
        .bench_recordings_manage(&serde_json::json!({
            "action": "save",
            "speaker": "../escape",
            "corpus_id": "status_fast_read",
            "audio_wav_base64": make_wav_b64(48_000, 1500, 0.4),
        }))
        .unwrap_err();
    assert_eq!(bad_speaker.code, "bench_recordings_invalid");

    // 3. 合法录音（48 kHz 自动重采样至 16 kHz）落盘到 <runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}
    let speaker_canary = "说话人CANARY_家人A";
    let saved = builtin
        .bench_recordings_manage(&serde_json::json!({
            "action": "save",
            "speaker": speaker_canary,
            "corpus_id": "status_fast_read",
            "audio_wav_base64": make_wav_b64(48_000, 1500, 0.4),
        }))
        .unwrap();
    assert_eq!(saved["total_recordings"], 1);
    assert_eq!(saved["saved"]["sample_rate"], 16_000);
    assert_eq!(saved["saved"]["speaker"], speaker_canary);
    let wav_path = temp
        .path()
        .join("voice-bench")
        .join("recordings")
        .join(speaker_canary)
        .join("status_fast_read.wav");
    let json_path = temp
        .path()
        .join("voice-bench")
        .join("recordings")
        .join(speaker_canary)
        .join("status_fast_read.json");
    assert!(wav_path.is_file(), "应写入 16 kHz WAV 文件");
    assert!(json_path.is_file(), "应写入元数据 JSON 文件");
    let decoded =
        moss_voice_providers::wav::decode_wav(&std::fs::read(&wav_path).unwrap()).unwrap();
    assert_eq!(decoded.sample_rate, 16_000);

    // 读取试听音频 Data URL（不暴露绝对路径）
    let audio = builtin
        .bench_recordings_manage(&serde_json::json!({
            "action": "audio",
            "speaker": speaker_canary,
            "corpus_id": "status_fast_read",
        }))
        .unwrap();
    assert!(
        audio["audio_data_url"]
            .as_str()
            .is_some_and(|u| u.starts_with("data:audio/wav;base64,"))
    );
    let audio_json = serde_json::to_string(&audio).unwrap();
    assert!(!audio_json.contains(&temp.path().to_string_lossy().to_string()));

    // 4. 诊断包绝不能包含测试录音内容、说话人标签或目录路径
    let bundle = builtin.diagnostics_bundle();
    let bundle_json = serde_json::to_string(&bundle).unwrap();
    assert!(
        !bundle_json.contains(speaker_canary),
        "诊断包绝不能包含测试录音说话人标签"
    );
    assert!(
        !bundle_json.contains("voice-bench/recordings"),
        "诊断包绝不能包含测试录音路径"
    );
    assert!(
        bundle["excluded"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|v| v == "本机整体测试真人录音与元数据")),
        "诊断包 excluded 应显式注明排除本机测试真人录音"
    );

    // 5. 清空必须显式 confirmed: true，确认后删除全部录音
    let clear_unconfirmed = builtin
        .bench_recordings_manage(&serde_json::json!({"action": "clear"}))
        .unwrap_err();
    assert_eq!(clear_unconfirmed.code, "confirmation_required");

    let cleared = builtin
        .bench_recordings_manage(&serde_json::json!({"action": "clear", "confirmed": true}))
        .unwrap();
    assert_eq!(cleared["deleted_count"], 1);
    assert_eq!(cleared["total_recordings"], 0);
    assert!(!wav_path.exists());
}

/// n146 唤醒词条目与动态唤醒词：
/// 1. `e2e-corpus.json` 含 `wake_word`（`purpose: "wake"`），不计入 13 句标准语料覆盖率；
/// 2. 唤醒词文本动态跟随 MOSS 当前生效唤醒词（默认“你好小智”，推送自定义后跟随自定义唤醒词）。
#[test]
fn bench_recordings_wake_item_follows_effective_wake_word_and_excludes_from_standard_count() {
    use base64::Engine as _;

    let temp = tempfile::tempdir().unwrap();
    let host = std::sync::Arc::new(
        laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
    );
    let builtin = init(Some(host)).0.unwrap();

    let samples: Vec<i16> = (0..16_000)
        .map(|i| {
            let phase = 2.0 * std::f32::consts::PI * 440.0 * (i as f32) / 16_000.0;
            (phase.sin() * 0.4 * 32767.0).round() as i16
        })
        .collect();
    let wav_b64 = base64::engine::general_purpose::STANDARD
        .encode(moss_voice_providers::wav::encode_wav(&samples, 16_000));

    // 默认未推自定义唤醒词：生效唤醒词为“你好小智”，标准语料数恒为 13，列表含 14 条（含 1 条 wake）
    let list = builtin
        .bench_recordings_manage(&serde_json::json!({"action": "list"}))
        .unwrap();
    assert_eq!(list["wake_word"], "你好小智");
    assert_eq!(list["wake_word_recorded"], false);
    assert_eq!(list["corpus_count"], 13);
    let items = list["corpus_items"].as_array().unwrap();
    assert_eq!(items.len(), 14);
    assert_eq!(items[0]["id"], "wake_word");
    assert_eq!(items[0]["purpose"], "wake");
    assert_eq!(items[0]["text"], "你好小智");

    // 仅录唤醒词：不计入 13 句标准语料覆盖率，但 wake_word_recorded 变为 true
    let saved_wake = builtin
        .bench_recordings_manage(&serde_json::json!({
            "action": "save",
            "speaker": "我",
            "corpus_id": "wake_word",
            "audio_wav_base64": wav_b64,
        }))
        .unwrap();
    assert_eq!(saved_wake["saved"]["text"], "你好小智");
    assert_eq!(saved_wake["corpus_count"], 13);
    assert_eq!(saved_wake["wake_word_recorded"], true);
    assert_eq!(saved_wake["speakers"][0]["recorded_count"], 0);
    assert_eq!(saved_wake["speakers"][0]["corpus_count"], 13);
    assert_eq!(saved_wake["speakers"][0]["complete"], false);

    // 保存自定义唤醒词并标记设备 dev-custom 已推送自定义唤醒词：按设备与全局动态跟随新唤醒词
    builtin
        .wakeword_manage(&serde_json::json!({
            "action": "save",
            "text": "你好大白",
            "command": "ni hao da bai",
            "threshold": 0.25,
        }))
        .unwrap();
    moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
        s.builtin_device_prefs
            .entry("dev-custom".into())
            .or_default()
            .wake_target = Some("custom".into());
        s.builtin_device_prefs
            .entry("dev-default".into())
            .or_default()
            .wake_target = Some("default".into());
        Ok(())
    })
    .unwrap();
    assert_eq!(builtin.effective_wake_word(Some("dev-custom")), "你好大白");
    assert_eq!(builtin.effective_wake_word(Some("dev-default")), "你好小智");
    assert_eq!(builtin.effective_wake_word(Some("dev-unknown")), "你好小智");
    assert_eq!(builtin.effective_wake_word(None), "你好大白");

    let list_custom = builtin
        .bench_recordings_manage(&serde_json::json!({"action": "list"}))
        .unwrap();
    assert_eq!(list_custom["wake_word"], "你好大白");
    assert_eq!(list_custom["corpus_items"][0]["text"], "你好大白");
    assert_eq!(
        list_custom["wake_word_recorded"], false,
        "旧录音文本是“你好小智”，切换为“你好大白”后不再视作当前唤醒词已录"
    );
}

#[cfg(unix)]
#[test]
fn bench_recordings_rejects_symlinked_parent_dir_on_read_and_delete() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_speaker = outside.path().join("我");
    std::fs::create_dir_all(&outside_speaker).unwrap();
    let outside_wav = outside_speaker.join("status_fast_read.wav");
    std::fs::write(&outside_wav, b"outside-secret-wav").unwrap();

    let vb_dir = temp.path().join("voice-bench");
    std::fs::create_dir_all(&vb_dir).unwrap();
    std::os::unix::fs::symlink(outside.path(), vb_dir.join("recordings")).unwrap();

    let host = std::sync::Arc::new(
        laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
    );
    let builtin = init(Some(host)).0.unwrap();

    for req in [
        serde_json::json!({"action": "list"}),
        serde_json::json!({"action": "audio", "speaker": "我", "corpus_id": "status_fast_read"}),
        serde_json::json!({"action": "delete", "speaker": "我", "corpus_id": "status_fast_read"}),
        serde_json::json!({"action": "clear", "confirmed": true}),
    ] {
        let err = builtin
            .bench_recordings_manage(&req)
            .expect_err("软链录音目录必须在读写删前全部拒绝");
        assert_eq!(err.status, 500);
    }
    assert!(outside_wav.is_file(), "绝不能穿透软链删除外部目录文件");
}

#[test]
fn process_trace_prefixed_wav_pairs_with_trace_audio_label_and_respects_disabled_asr_corpus_gate() {
    let temp = tempfile::tempdir().unwrap();
    let host = std::sync::Arc::new(
        laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
    );
    let builtin = init(Some(host.clone())).0.unwrap();

    // 1. 构造一条开启时采集、随后关闭开关并保留在磁盘的独立 asr-corpus 条目
    builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "set_config",
            "enabled": true,
            "confirmed": true
        }))
        .unwrap();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let disabled_meta = builtin
        .asr_corpus
        .record(moss_voice_server::CapturedUtterance {
            session_id: "sess-disabled".into(),
            device_id: "aa:bb:cc:11:22:33".into(),
            turn: 9,
            captured_at_ms: now_ms,
            simulated: false,
            bench_window: false,
            pcm: vec![700; 3_200],
            sample_rate: 16_000,
            raw_text: "停用后绝不能暴露".into(),
            corrected_text: "停用后绝不能暴露".into(),
            interims: Vec::new(),
            asr_model: "本地 sense_voice".into(),
            vad_engine: "能量判停".into(),
            endpoint: Default::default(),
            outcome: "done".into(),
        })
        .unwrap()
        .unwrap();
    builtin
        .asr_corpus
        .save_label(
            &disabled_meta.date,
            &disabled_meta.turn_id,
            "停用语料隐私真值CANARY",
            true,
            vec!["材料名".into()],
        )
        .unwrap();
    builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "set_config",
            "enabled": false,
            "delete_existing": false
        }))
        .unwrap();

    // 2. 写入两条对话日志：一条只有禁用 asr-corpus 录音，一条有 ProcessTrace 带前缀的真实 WAV
    let date = &disabled_meta.date;
    let voice_files_dir = temp
        .path()
        .join(laser_runtime::process_trace::TRACE_DIR)
        .join(date)
        .join("voice-files");
    std::fs::create_dir_all(&voice_files_dir).unwrap();
    let prefixed_stem = "2233-t1-user";
    let prefixed_name = format!("154730123-000001-{prefixed_stem}.wav");
    let wav_bytes = moss_voice_providers::wav::encode_wav(&vec![500; 3_200], 16_000);
    std::fs::write(voice_files_dir.join(&prefixed_name), &wav_bytes).unwrap();
    {
        let db = host.db.lock().unwrap();
        db.append_voice_chat_log(&laser_runtime::db::VoiceChatLogRecord {
            id: 0,
            at_ms: now_ms as i64,
            device_id: "aa:bb:cc:11:22:33".into(),
            session_id: "sess-disabled".into(),
            turn: 9,
            raw_asr: "停用后绝不能暴露".into(),
            corrected_asr: "停用后绝不能暴露".into(),
            tools_json: "[]".into(),
            reply: "收到".into(),
            spoken: "收到".into(),
            outcome: "done".into(),
            truncated: false,
            tools_dropped: 0,
            simulated: false,
            bench_window: false,
        })
        .unwrap();
        db.append_voice_chat_log(&laser_runtime::db::VoiceChatLogRecord {
            id: 0,
            at_ms: (now_ms + 1_000) as i64,
            device_id: "aa:bb:cc:11:22:33".into(),
            session_id: "sess-pt".into(),
            turn: 1,
            raw_asr: "三毫米段木".into(),
            corrected_asr: "三毫米椴木".into(),
            tools_json: "[]".into(),
            reply: "已查参数".into(),
            spoken: "已查参数".into(),
            outcome: "done".into(),
            truncated: false,
            tools_dropped: 0,
            simulated: false,
            bench_window: false,
        })
        .unwrap();
    }

    // 3. 禁用态的 asr-corpus 录音与标注绝不能经 trace_audio / trace_label / voice_builtin_status 泄漏
    let blocked = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "trace_audio",
            "date": date,
            "device_id": "aa:bb:cc:11:22:33",
            "session_id": "sess-disabled",
            "turn": 9
        }))
        .unwrap_err();
    assert_eq!(blocked.status, 403);
    assert_eq!(blocked.code, "asr_corpus_disabled");

    // 4. ProcessTrace 带时间戳序号前缀的 WAV 必须被 trace_audio / trace_label 与 status 精准配对
    let pt_audio = builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "trace_audio",
            "date": date,
            "device_id": "aa:bb:cc:11:22:33",
            "session_id": "sess-pt",
            "turn": 1,
            "at_ms": now_ms + 1_000
        }))
        .unwrap();
    assert_eq!(
        pt_audio["trace_audio_file"],
        format!("{date}/voice-files/{prefixed_name}")
    );
    assert!(
        pt_audio["audio_data_url"]
            .as_str()
            .is_some_and(|u| u.starts_with("data:audio/wav;base64,"))
    );

    builtin
        .asr_corpus_manage(&serde_json::json!({
            "action": "trace_label",
            "date": date,
            "device_id": "aa:bb:cc:11:22:33",
            "session_id": "sess-pt",
            "turn": 1,
            "at_ms": now_ms + 1_000,
            "raw_text": "三毫米段木",
            "reference_text": "三毫米椴木",
            "input_source": "live_mic",
            "verified_correct": false
        }))
        .unwrap();
    let expected_label =
        voice_files_dir.join(format!("154730123-000001-{prefixed_stem}.label.json"));
    assert!(
        expected_label.is_file(),
        "trace_label 必须与带前缀的过程记录 WAV 同名配对写出"
    );

    let st = builtin.status();
    let traces = st["debug_traces"].as_array().unwrap();
    let disabled_trace = traces
        .iter()
        .find(|t| t["session_id"] == "sess-disabled")
        .unwrap();
    assert_eq!(disabled_trace["has_audio"], false);
    assert!(disabled_trace["reference_text"].is_null());

    let pt_trace = traces
        .iter()
        .find(|t| t["session_id"] == "sess-pt")
        .unwrap();
    assert_eq!(pt_trace["has_audio"], true);
    assert_eq!(pt_trace["reference_text"], "三毫米椴木");
    assert_eq!(
        pt_trace["trace_audio_file"],
        format!("{date}/voice-files/{prefixed_name}")
    );

    let bundle_json = serde_json::to_string(&builtin.diagnostics_bundle()).unwrap();
    assert!(!bundle_json.contains("停用语料隐私真值CANARY"));
    assert!(!bundle_json.contains(&prefixed_name));
}
