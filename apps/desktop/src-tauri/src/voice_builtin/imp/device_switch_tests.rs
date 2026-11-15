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
        }],
        reply: "今天天气晴朗，气温22度，非常适合出门。".into(),
        spoken: "今天天气晴朗，".into(),
        outcome: "interrupted".into(),
        truncated: false,
        tools_dropped: 0,
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
    assert_eq!(toggled["debug_enabled"], false);
    assert_eq!(toggled["debug_traces"].as_array().unwrap().len(), 1);
    let bundle = restarted.diagnostics_bundle();
    assert_eq!(bundle["debug_traces"].as_array().unwrap().len(), 0);

    // 显式清空：本机记录清零
    let cleared = restarted
        .local_save(&serde_json::json!({"clear_debug_traces": true}))
        .unwrap();
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
