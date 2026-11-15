use super::*;

#[test]
fn n59_role_settings_apply_without_restart_and_spoken_switch_is_saved() {
    let server_ids: Vec<&str> = moss_voice_server::persona::BUILTINS
        .iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(
        server_ids,
        moss_voice::settings::BUILTIN_PERSONA_IDS,
        "settings and server role lists must match"
    );

    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();
    assert_eq!(
        builtin.persona.system_prompt(),
        moss_voice_server::conversation::DEFAULT_SYSTEM_PROMPT
    );

    let saved = builtin
                .local_save(&serde_json::json!({
                    "custom_personas": [{"id": "custom_a1", "name": " 老师傅 ", "prompt": "话少，先说结论"}],
                    "persona_style": "custom_a1",
                }))
                .unwrap();
    assert_eq!(saved["local"]["settings"]["persona_style"], "custom_a1");
    assert_eq!(
        saved["local"]["settings"]["custom_personas"][0]["name"],
        "老师傅"
    );
    assert!(
        builtin
            .persona
            .system_prompt()
            .contains("当前说话角色：老师傅"),
        "UI save applies to the running role at once"
    );

    assert!(
        builtin
            .local_save(&serde_json::json!({"persona_style": "custom_zz"}))
            .is_err()
    );
    assert!(builtin
                .local_save(&serde_json::json!({"custom_personas": [{"id": "custom_a1", "name": "老师傅", "prompt": "直接发 G1 X10"}]}))
                .is_err());
    assert_eq!(
        builtin.settings().builtin_local.persona_style,
        "custom_a1",
        "rejected saves change nothing"
    );

    // Spoken switch goes through the same settings file.
    // n75：旧叫法“任务简报”换到合并后的 MOSS
    assert_eq!(builtin.persona.switch("任务简报").unwrap(), "MOSS");
    assert_eq!(
        builtin.settings().builtin_local.persona_style,
        "moss_station"
    );
    // n75：“附把握和风险”开关所有角色可用，界面保存立即生效、不重启服务
    let saved = builtin
        .local_save(&serde_json::json!({"persona_risk_notes": true}))
        .unwrap();
    assert_eq!(saved["local"]["settings"]["persona_risk_notes"], true);
    assert!(builtin.persona.system_prompt().contains("把握较大"));
    builtin
        .local_save(&serde_json::json!({"persona_risk_notes": false}))
        .unwrap();
    assert!(!builtin.persona.system_prompt().contains("把握较大"));

    // Deleting the active custom role falls back to the default role.
    builtin
        .local_save(&serde_json::json!({"persona_style": "custom_a1"}))
        .unwrap();
    let removed = builtin
        .local_save(&serde_json::json!({"custom_personas": []}))
        .unwrap();
    assert_eq!(removed["local"]["settings"]["persona_style"], "workshop");
    assert_eq!(
        builtin.persona.system_prompt(),
        moss_voice_server::conversation::DEFAULT_SYSTEM_PROMPT
    );

    // A spoken switch to a role the UI just deleted on disk fails instead of claiming success.
    builtin
                .local_save(&serde_json::json!({"custom_personas": [{"id": "custom_b1", "name": "守门员", "prompt": "话少"}]}))
                .unwrap();
    moss_voice::settings::update_voice(&builtin.settings_file(), |s| {
        s.builtin_local.custom_personas.clear();
        Ok(())
    })
    .unwrap();
    assert!(builtin.persona.switch("守门员").is_err());
    assert_eq!(
        builtin.persona.config(),
        persona_config(&builtin.settings().builtin_local)
    );

    // UI saves and spoken switches racing each other leave the running role equal to the file.
    std::thread::scope(|scope| {
        let ui = &builtin;
        scope.spawn(move || {
                    for (i, style) in ["moss_station", "moss_risk", "beginner_guide"].iter().cycle().take(15).enumerate() {
                        ui.local_save(&serde_json::json!({"persona_style": style, "persona_prompt": format!("背景{i}")})).unwrap();
                    }
                });
        let voice = &builtin;
        scope.spawn(move || {
            for name in ["守夜", "任务简报", "冷面幽默"].iter().cycle().take(15) {
                voice.persona.switch(name).unwrap();
            }
        });
    });
    assert_eq!(
        builtin.persona.config(),
        persona_config(&builtin.settings().builtin_local)
    );
}

#[test]
fn local_laya_device_selection_lifecycle_and_jev_save_preserve_local_routing() {
    assert_eq!(
        BuiltinVoice::select_laya_device(true, false, None),
        "cuda:0"
    );
    assert_eq!(BuiltinVoice::select_laya_device(true, true, None), "cuda:0");
    assert_eq!(BuiltinVoice::select_laya_device(false, true, None), "mps");
    assert_eq!(BuiltinVoice::select_laya_device(false, false, None), "cpu");
    assert_eq!(
        BuiltinVoice::select_laya_device(false, true, Some(" cuda:1 ")),
        "cuda:1"
    );
    assert_eq!(
        BuiltinVoice::select_laya_device(true, false, Some("   ")),
        "cuda:0"
    );
    assert!(!BuiltinVoice::probe_laya_cuda_available(
        std::path::Path::new("nonexistent-python"),
        true
    ));
    assert!(!BuiltinVoice::probe_laya_cuda_available(
        std::path::Path::new("nonexistent-python"),
        false
    ));
    assert!(!BuiltinVoice::probe_laya_mps_available(
        std::path::Path::new("nonexistent-python"),
        true
    ));
    assert!(!BuiltinVoice::probe_laya_mps_available(
        std::path::Path::new("nonexistent-python"),
        false
    ));

    let temp = tempfile::tempdir().unwrap();
    let fake_install = |model_id: &str| {
        let spec = local_models::spec(model_id).unwrap();
        let dir = temp.path().join("models").join(model_id);
        for (rel, len) in std::iter::once((spec.path, spec.size))
            .chain(spec.extras.iter().map(|e| (e.path, e.size)))
        {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::File::create(&p).unwrap().set_len(len).unwrap();
        }
    };
    fake_install("laya_multilingual");
    fake_install("onejev_1_6_0_8b");
    fake_install("onejev_1_6_4b");

    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let mut voice_settings = moss_voice::VoiceSettings {
        mode: VoiceMode::Official,
        ..Default::default()
    };
    voice_settings.builtin_local.jev_model = "laya_multilingual".into();
    voice_settings.builtin_local.jev_filter_enabled = true;
    moss_voice::settings::save_with_jev_feedback(
        &temp.path().join("settings.json"),
        &voice_settings,
        Some(false),
    )
    .unwrap();

    let builtin = init(Some(host.clone())).0.unwrap();
    assert!(builtin.should_run_local_laya());
    assert!(!builtin.should_run_local_laya_for_feedback());
    assert!(builtin.status()["local"]["onejev_env"]["accelerator"].is_string());
    assert!(builtin.status()["local"]["onejev_env"]["accelerator_label"].is_string());
    assert!(builtin.status()["local"]["laya_env"]["accelerator"].is_string());

    // 可在本地 OneJev 0.8B / 4B / 9B 与 Laya 之间自由切换（未安装的 9B 在启用时会给出明确下载提示）
    assert!(
        builtin
            .precheck_jev_save_input(
                Some(&serde_json::json!({
                    "model": "onejev_1_6_9b",
                    "voice_enabled": true,
                })),
                false,
                &ModelsInput::default(),
                &[],
            )
            .is_err(),
        "未下载的 OneJev 9B 不允许直接作为启用态保存"
    );
    let switched_onejev = builtin
        .local_save(&serde_json::json!({
            "jev_filter_enabled": true,
            "jev_model": "onejev_1_6_4b",
        }))
        .unwrap();
    assert_eq!(switched_onejev["jev"]["model"], "onejev_1_6_4b");
    assert_eq!(
        switched_onejev["jev"]["endpoint"],
        moss_voice::models::DEFAULT_LOCAL_LAYA_DECISIONS_URL
    );
    assert_eq!(switched_onejev["jev"]["local_installed"], true);
    assert_eq!(switched_onejev["local"]["jev_key_configured"], true);
    assert!(builtin.should_run_local_laya());

    // 本地 Jev 实际运行情况：已下载已启用但未拉起 → stopped；在途启动 → starting；
    // 后台启动失败 → failed 并带原因；切到云端模型 → cloud（2026-11-07）
    let runtime = |b: &BuiltinVoice| b.status()["local"]["jev_runtime"].clone();
    assert_eq!(runtime(&builtin)["model"], "onejev_1_6_4b");
    assert_eq!(runtime(&builtin)["state"], "stopped");
    {
        let mut runner = builtin.laya.lock().unwrap();
        runner.starting_model = Some("onejev_1_6_4b".to_owned());
        runner.starting_cancel = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    }
    assert_eq!(runtime(&builtin)["state"], "starting");
    {
        let mut runner = builtin.laya.lock().unwrap();
        runner.starting_model = None;
        runner.starting_cancel = None;
        runner.last_failure = Some(("onejev_1_6_4b".to_owned(), "OneJev 模型加载超时".to_owned()));
    }
    assert_eq!(runtime(&builtin)["state"], "failed");
    assert_eq!(runtime(&builtin)["error"], "OneJev 模型加载超时");
    builtin.laya.lock().unwrap().last_failure =
        Some(("onejev_1_6_0_8b".to_owned(), "别的档位的旧失败".to_owned()));
    assert_eq!(
        runtime(&builtin)["state"],
        "stopped",
        "其他档位的失败不串到当前模型"
    );
    builtin.laya.lock().unwrap().last_failure = None;
    let cpu_gpu = moss_voice::onejev::expected_gpu(builtin.models.root_for("onejev_1_6_4b"));
    assert_eq!(
        runtime(&builtin)["notice"].as_str(),
        moss_voice::onejev::local_jev_budget("onejev_1_6_4b", cpu_gpu)
            .unwrap()
            .notice
    );

    // 切回 laya_multilingual 继续验证免密钥与备用密钥共存
    builtin
        .local_save(&serde_json::json!({
            "jev_filter_enabled": true,
            "jev_model": "laya_multilingual",
        }))
        .unwrap();

    // 省略 model 且无 OpenRouter API Key：保留 laya_multilingual，并允许单独开启加工反馈、关闭语音筛选
    builtin
        .precheck_jev_save_input(
            Some(&serde_json::json!({
                "voice_enabled": false,
                "feedback_enabled": true,
            })),
            false,
            &ModelsInput::default(),
            &[],
        )
        .unwrap();

    let saved_feedback_only = builtin
        .jev_save(
            Some(&serde_json::json!({
                "voice_enabled": false,
                "feedback_enabled": true,
            })),
            false,
        )
        .unwrap();
    assert_eq!(saved_feedback_only["jev"]["model"], "laya_multilingual");
    assert_eq!(saved_feedback_only["jev"]["voice_enabled"], false);
    assert_eq!(saved_feedback_only["jev"]["feedback_enabled"], true);
    assert!(
        builtin.should_run_local_laya_for_feedback(),
        "仅开启加工反馈且非内置语音模式时，应用启动也应拉起本地 Jev 服务"
    );

    // 录入备用 OpenRouter 密钥但不传/不改本地模型：写入密钥且不覆盖本地路由
    let saved_backup_key = builtin
        .jev_save(
            Some(&serde_json::json!({
                "api_key": "sk-or-backup-only",
                "model": "",
            })),
            false,
        )
        .unwrap();
    assert_eq!(saved_backup_key["jev"]["model"], "laya_multilingual");
    assert_eq!(saved_backup_key["jev"]["feedback_enabled"], true);
    assert_eq!(
        host.db
            .lock()
            .unwrap()
            .get_secret("jev_api_key")
            .unwrap()
            .as_deref(),
        Some("sk-or-backup-only")
    );

    // 删除未激活的 onejev_1_6_0_8b 不递增当前运行代次；切换到 onejev_1_6_4b 后删除闲置 laya_multilingual 也不误停当前网关
    let gen_before = builtin.laya.lock().unwrap().generation;
    builtin.model_delete("onejev_1_6_0_8b").unwrap();
    assert_eq!(builtin.laya.lock().unwrap().generation, gen_before);
    builtin
        .local_save(&serde_json::json!({
            "jev_filter_enabled": true,
            "jev_model": "onejev_1_6_4b",
        }))
        .unwrap();
    let gen_on_onejev = builtin.laya.lock().unwrap().generation;
    // 2026-11-07：删除 Laya 时连同 MOSS 创建（带标记）的虚拟环境与 tmp 一起删
    let laya_root = temp.path().join("laya-env");
    std::fs::create_dir_all(laya_root.join(".venv").join("bin")).unwrap();
    std::fs::write(
        laya_root
            .join(".venv")
            .join(moss_voice::laya_env::MANAGED_MARKER),
        b"",
    )
    .unwrap();
    std::fs::create_dir_all(laya_root.join("tmp")).unwrap();
    *builtin.laya_root_for_test.lock().unwrap() = Some(laya_root.clone());
    assert_eq!(builtin.status()["local"]["laya_setup"]["state"], "idle");
    builtin.model_delete("laya_multilingual").unwrap();
    assert!(
        !laya_root.exists(),
        "MOSS 创建的 Laya 虚拟环境随模型一起删除"
    );
    assert_eq!(
        builtin.laya.lock().unwrap().generation,
        gen_on_onejev,
        "运行 onejev_1_6_4b 时删除闲置 laya_multilingual 不得误停当前 Jev 进程"
    );
    // 当反馈关闭、仅语音筛选开启时：原地重启（keep_local_jev=true）不打断在途预热，完整停止（false）才回收
    builtin
        .jev_save(
            Some(&serde_json::json!({
                "voice_enabled": true,
                "feedback_enabled": false,
                "model": "onejev_1_6_4b",
            })),
            false,
        )
        .unwrap();
    let in_flight_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut runner = builtin.laya.lock().unwrap();
        runner.starting_model = Some("onejev_1_6_4b".to_owned());
        runner.starting_cancel = Some(in_flight_cancel.clone());
    }
    let gen_keep = builtin.laya.lock().unwrap().generation;
    builtin.stop_locked_inner(true);
    assert!(!in_flight_cancel.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(builtin.laya.lock().unwrap().generation, gen_keep);
    builtin.stop_locked_inner(false);
    assert!(in_flight_cancel.load(std::sync::atomic::Ordering::SeqCst));
    assert!(builtin.laya.lock().unwrap().generation > gen_keep);

    // 删除正在后台加载权重的模型时：必须先置位取消并等待 jev_start_lock 释放，才允许删除磁盘目录
    let bg_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bg_cleaned_before_delete = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut runner = builtin.laya.lock().unwrap();
        runner.starting_model = Some("onejev_1_6_4b".to_owned());
        runner.starting_cancel = Some(bg_cancel.clone());
    }
    let start_lock_clone = builtin.jev_start_lock.clone();
    let model_dir_check = builtin.models.path_if_installed("onejev_1_6_4b").unwrap();
    let cancel_for_bg = bg_cancel.clone();
    let cleaned_flag = bg_cleaned_before_delete.clone();
    let lock_acquired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let lock_acquired_bg = lock_acquired.clone();
    let bg_worker = std::thread::spawn(move || {
        let _g = start_lock_clone.lock().unwrap();
        lock_acquired_bg.store(true, std::sync::atomic::Ordering::SeqCst);
        while !cancel_for_bg.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        // 在释放 jev_start_lock 前，模型权重目录必须尚未被删除（Windows 下否则触发共享冲突）
        if model_dir_check.exists() {
            cleaned_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    while !lock_acquired.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let gen_before_del = builtin.laya.lock().unwrap().generation;
    builtin.model_delete("onejev_1_6_4b").unwrap();
    bg_worker.join().unwrap();
    assert!(bg_cleaned_before_delete.load(std::sync::atomic::Ordering::SeqCst));
    assert!(builtin.laya.lock().unwrap().generation > gen_before_del);
    assert!(builtin.models.path_if_installed("onejev_1_6_4b").is_none());

    // 先切回 off 触发非阻塞取消（starting_model -> cancelling_model），紧接着删 onejev_1_6_9b 也必须等 jev_start_lock 释放
    fake_install("onejev_1_6_9b");
    let cancel_9b = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut runner = builtin.laya.lock().unwrap();
        runner.starting_model = Some("onejev_1_6_9b".to_owned());
        runner.starting_cancel = Some(cancel_9b.clone());
    }
    let lock_9b = builtin.jev_start_lock.clone();
    let dir_9b_check = builtin.models.path_if_installed("onejev_1_6_9b").unwrap();
    let survived_until_unlock = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let survived_bg = survived_until_unlock.clone();
    let locked_9b = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let locked_9b_bg = locked_9b.clone();
    let cancel_9b_bg = cancel_9b.clone();
    let worker_9b = std::thread::spawn(move || {
        let _g = lock_9b.lock().unwrap();
        locked_9b_bg.store(true, std::sync::atomic::Ordering::SeqCst);
        while !cancel_9b_bg.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        if dir_9b_check.exists() {
            survived_bg.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    while !locked_9b.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    builtin
        .local_save(&serde_json::json!({
            "jev_filter_enabled": false,
            "jev_model": "typesafe/jev-latest",
        }))
        .unwrap();
    assert_eq!(
        builtin.laya.lock().unwrap().cancelling_model.as_deref(),
        Some("onejev_1_6_9b")
    );
    builtin.model_delete("onejev_1_6_9b").unwrap();
    worker_9b.join().unwrap();
    assert!(survived_until_unlock.load(std::sync::atomic::Ordering::SeqCst));
    assert!(builtin.laya.lock().unwrap().cancelling_model.is_none());
    assert!(builtin.models.path_if_installed("onejev_1_6_9b").is_none());

    assert!(parse_abs_env_path(None).is_none());
    assert!(parse_abs_env_path(Some(std::ffi::OsStr::new(""))).is_none());
    assert!(parse_abs_env_path(Some(std::ffi::OsStr::new("   "))).is_none());
    assert!(parse_abs_env_path(Some(std::ffi::OsStr::new("relative/laya"))).is_none());
    let abs_sample = std::env::temp_dir().join("moss-laya-sample");
    let abs_padded = format!("  {}  ", abs_sample.display());
    assert_eq!(
        parse_abs_env_path(Some(std::ffi::OsStr::new(&abs_padded))),
        Some(abs_sample.clone())
    );
    assert!(parse_non_empty_env_path(None).is_none());
    assert!(parse_non_empty_env_path(Some(std::ffi::OsStr::new("  "))).is_none());
    assert_eq!(
        parse_non_empty_env_path(Some(std::ffi::OsStr::new("  python3  "))),
        Some(std::path::PathBuf::from("python3"))
    );

    if moss_laya_root_env().is_none() && moss_laya_python_env().is_none() {
        let (laya_root, laya_py) = BuiltinVoice::resolve_laya_python();
        let tmp = std::env::temp_dir();
        assert!(
            !laya_root.starts_with(&tmp) && !laya_py.starts_with(&tmp),
            "resolve_laya_python must never probe world-writable temp_dir"
        );
    }
    let saved_speed = builtin
        .local_save(&serde_json::json!({ "tts_speed": 1.25 }))
        .unwrap();
    assert_eq!(saved_speed["local"]["settings"]["tts_speed"], 1.25);

    builtin
        .precheck_jev_save_input(
            Some(&serde_json::json!({
                "voice_enabled": false,
                "feedback_enabled": false,
            })),
            false,
            &ModelsInput::default(),
            &[],
        )
        .unwrap();
    let turned_off = builtin
        .jev_save(
            Some(&serde_json::json!({
                "voice_enabled": false,
                "feedback_enabled": false,
            })),
            false,
        )
        .unwrap();
    assert_eq!(turned_off["jev"]["voice_enabled"], false);
    assert_eq!(turned_off["jev"]["feedback_enabled"], false);
    assert!(builtin.laya.lock().unwrap().generation > gen_before);
}

#[test]
fn probe_laya_http_rejects_non_http_tcp_listener_and_accepts_http() {
    use std::io::{Read, Write};
    let non_http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let non_http_addr = non_http.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = non_http.accept() {
            let mut buf = [0u8; 128];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"NOT_HTTP_BANNER\r\n");
        }
    });
    assert!(
        probe_local_jev_health_model(non_http_addr, std::time::Duration::from_millis(200))
            .is_none()
    );

    for bad_http in [
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nnot found".as_slice(),
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\n\r\nerror".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\n\r\n<html></html>".as_slice(),
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"status\":\"error\"}".as_slice(),
            ] {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let addr = listener.local_addr().unwrap();
                let reply = bad_http.to_vec();
                std::thread::spawn(move || {
                    if let Ok((mut stream, _)) = listener.accept() {
                        let mut buf = [0u8; 256];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(&reply);
                    }
                });
                assert!(probe_local_jev_health_model(addr, std::time::Duration::from_millis(200)).is_none());
            }

    let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let http_addr = http.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = http.accept() {
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}",
                    );
        }
    });
    assert_eq!(
        probe_local_jev_health_model(http_addr, std::time::Duration::from_millis(200)).as_deref(),
        Some("laya_multilingual")
    );

    let onejev_http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let onejev_addr = onejev_http.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = onejev_http.accept() {
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf);
            let body = serde_json::to_vec(&moss_voice::onejev::health_payload(
                "onejev_1_6_4b",
                "Apple Metal GPU",
            ))
            .unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    assert_eq!(
        probe_local_jev_health_model(onejev_addr, std::time::Duration::from_millis(200)).as_deref(),
        Some("onejev_1_6_4b")
    );

    // 外部上游 qev / OneJev 服务（不含 engine 字段，但含 fork_mode 与自定义 model）不得被误判为 laya_multilingual
    let ext_http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ext_addr = ext_http.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = ext_http.accept() {
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf);
            let body = br#"{"status":"ok","fork_mode":"llama.cpp","model":"custom-qev"}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    assert_eq!(
        probe_local_jev_health_model(ext_addr, std::time::Duration::from_millis(200)).as_deref(),
        Some("external_onejev:custom-qev")
    );
}

#[test]
fn chain_test_runs_five_steps_end_to_end_and_reports_exact_failing_stage() {
    use std::io::{Read, Write};

    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();

    // 1. 尚未配置任何 TTS 且未提供麦克风录音时，明确报告 tts 阶段未配置
    let unconfigured = builtin.chain_test(&serde_json::json!({})).unwrap();
    assert_eq!(unconfigured["ok"], false);
    assert_eq!(unconfigured["failed_step"], "tts");
    assert_eq!(unconfigured["steps"]["tts"]["ok"], false);

    // 2. 启动本机回环假 OpenAI 语音 + 对话服务端（覆盖 TTS 种子语音 + ASR 识别 + LLM 流式回复 + TTS 最终合成）
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server_handle = std::thread::spawn(move || {
        // 共接收 3 个请求：第 0 棒 TTS 种子语音 -> 第 2 棒 ASR 识别 -> 第 4 棒 LLM 回复 -> 第 5 棒 TTS 回复合成（共 4 个连接）
        for _ in 0..4 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .ok();
            let mut req_buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            let mut content_len = 0usize;
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        req_buf.extend_from_slice(&chunk[..n]);
                        if header_end.is_none()
                            && let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            let head_str = String::from_utf8_lossy(&req_buf[..pos]);
                            for line in head_str.lines() {
                                if let Some(rest) =
                                    line.to_ascii_lowercase().strip_prefix("content-length:")
                                {
                                    content_len = rest.trim().parse().unwrap_or(0);
                                }
                            }
                            header_end = Some(pos + 4);
                        }
                        if let Some(h_end) = header_end
                            && req_buf.len() >= h_end + content_len
                        {
                            break;
                        }
                    }
                }
            }
            let req_head = String::from_utf8_lossy(&req_buf);
            let first_line = req_head.lines().next().unwrap_or("");
            if first_line.contains("/audio/speech") {
                // 返回 0.6 秒高幅值 16kHz 正弦人声模拟波，确保 EnergyVad 稳定触发 SpeechStart 与尾部静音 SpeechEnd
                let samples: Vec<i16> = (0..9_600)
                    .map(|i| {
                        ((i as f64 / 16_000.0 * 320.0 * std::f64::consts::TAU).sin() * 12_000.0)
                            as i16
                    })
                    .collect();
                let wav = moss_voice_providers::wav::encode_wav(&samples, 16_000);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    wav.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&wav);
            } else if first_line.contains("/audio/transcriptions") {
                // 返回含同音错词“段木”的识别结果，顺带验证链内同音纠偏生效
                let body = r#"{"text":"你好看看段木状态"}"#.as_bytes();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            } else if first_line.contains("/chat/completions") {
                let sse = concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"😊 我在线，东北椴木参数已就绪。\"}}]}\n\n",
                    "data: [DONE]\n\n"
                );
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    sse.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(sse.as_bytes());
            }
        }
    });

    let base_url = format!("http://{addr}/v1");
    builtin
                .models_save(
                    &serde_json::from_value(serde_json::json!({
                        "asr": { "kind": "openai", "url": format!("{base_url}/audio/transcriptions"), "api_key": "test-key", "model": "whisper-1" },
                        "llm": { "kind": "openai", "url": base_url, "api_key": "test-key", "model": "qwen-test" },
                        "tts": { "kind": "openai", "url": format!("http://{addr}/v1/audio/speech"), "api_key": "test-key", "model": "tts-1", "voice": "alloy", "speed": 1.0 }
                    }))
                    .unwrap(),
                    &[],
                )
                .unwrap();

    let out = builtin.chain_test(&serde_json::json!({})).unwrap();
    let _ = server_handle.join();

    assert_eq!(out["ok"], true, "five-step chain test failed: {out}");
    assert!(out["failed_step"].is_null());
    assert_eq!(
        out["recognized_text"], "你好看看椴木状态",
        "同音纠偏应在连贯识别环节生效"
    );
    assert!(
        out["reply_text"]
            .as_str()
            .unwrap_or("")
            .contains("东北椴木参数已就绪")
    );
    assert!(
        out["input_audio_data_url"]
            .as_str()
            .unwrap_or("")
            .starts_with("data:audio/wav;base64,")
    );
    assert!(
        out["reply_audio_data_url"]
            .as_str()
            .unwrap_or("")
            .starts_with("data:audio/wav;base64,")
    );
    for step in ["vad", "asr", "jev", "llm", "tts"] {
        assert_eq!(out["steps"][step]["ok"], true, "step {step} failed: {out}");
    }
    assert_eq!(out["steps"]["jev"]["skipped"], true);

    // 3. 单飞互斥锁生效：占住 chain_test_lock 期间再次调用立即返回 409 chain_test_busy
    {
        let _held = builtin.chain_test_lock.lock().unwrap();
        let busy_err = builtin.chain_test(&serde_json::json!({})).unwrap_err();
        assert_eq!(busy_err.code, "chain_test_busy");
        assert_eq!(busy_err.status, 409);
    }

    // 4. Colab CLI 边角回归：abort_colab_starting 将挂起的 starting 复位为 error，clear_ref_audio 不静默吞掉非 NotFound 错误
    {
        {
            let mut st = builtin.colab_cli.lock().unwrap();
            st.generation = 7;
            st.runner_state = "starting".into();
            st.runner_step = "正在通过 Colab CLI 自动拉起云端 T4...".into();
        }
        BuiltinVoice::abort_colab_starting(
            &builtin.colab_cli,
            7,
            "未找到 Colab CLI 可执行文件".into(),
        );
        let st = builtin.colab_cli.lock().unwrap();
        assert_eq!(st.runner_state, "error");
        assert!(
            st.runner_error
                .as_deref()
                .unwrap_or("")
                .contains("Colab CLI")
        );
    }
    {
        let ref_wav = builtin.colab_ref_audio_path();
        std::fs::create_dir_all(&ref_wav).unwrap();
        let gen_before = builtin.colab_cli.lock().unwrap().ref_sync_gen;
        let err = builtin
            .handle_colab_cli_action(&serde_json::json!({ "action": "clear_ref_audio" }))
            .unwrap_err();
        assert_eq!(err.code, "colab_ref_clear_failed");
        assert_eq!(builtin.colab_cli.lock().unwrap().ref_sync_gen, gen_before);
        std::fs::remove_dir_all(&ref_wav).unwrap();
        builtin
            .handle_colab_cli_action(&serde_json::json!({ "action": "clear_ref_audio" }))
            .unwrap();
        let st = builtin.colab_cli.lock().unwrap();
        assert_eq!(st.ref_sync_gen, gen_before + 1);
        assert_eq!(st.synced_ref_gen, 0);
    }
}

#[test]
fn local_tts_voices_list_and_preview_serve_cached_wav_and_clean_on_delete() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();

    // 1. 未安装时仍可读取完整语言包与全部音色清单，但试听返回 409 提示先下载
    let list_uninstalled = builtin
        .local_tts_voices_list(&serde_json::json!({ "model_id": "kokoro" }))
        .unwrap();
    assert_eq!(list_uninstalled["installed"], false);
    assert_eq!(list_uninstalled["total_count"], 54);
    assert_eq!(list_uninstalled["cached_count"], 0);
    assert_eq!(list_uninstalled["languages"][0]["code"], "zh");
    let err = builtin
        .local_tts_preview(&serde_json::json!({ "model_id": "kokoro", "speaker": 45 }))
        .unwrap_err();
    assert_eq!(err.code, "local_tts_not_installed");

    // 2. 模拟模型目录已安装并写入一条本机缓存 WAV：列表显示 cached=true，试听直接秒读缓存返回 data URL
    let kokoro_spec = local_models::spec_of("kokoro", ModelKind::Tts).unwrap();
    let kokoro_dir = local_models::install_path(builtin.models.root(), kokoro_spec);
    std::fs::create_dir_all(kokoro_dir.join("espeak-ng-data")).unwrap();
    std::fs::create_dir_all(kokoro_dir.join("dict")).unwrap();
    for req in [
        "model.onnx",
        "voices.bin",
        "tokens.txt",
        "lexicon-zh.txt",
        "lexicon-us-en.txt",
    ] {
        std::fs::write(kokoro_dir.join(req), b"stub").unwrap();
    }
    let samples = vec![0i16; 2400];
    let wav = moss_voice_providers::wav::encode_wav(&samples, 24_000);
    moss_voice::tts_voices::write_cached_preview_atomic(builtin.models.root(), "kokoro", 45, &wav)
        .unwrap();

    let list_installed = builtin
        .local_tts_voices_list(&serde_json::json!({ "model_id": "kokoro" }))
        .unwrap();
    assert_eq!(list_installed["installed"], true);
    assert_eq!(list_installed["cached_count"], 1);
    assert_eq!(list_installed["speakers"][0]["id"], 45);
    assert_eq!(list_installed["speakers"][0]["cached"], true);

    let preview = builtin
        .local_tts_preview(&serde_json::json!({ "model_id": "kokoro", "speaker": 45 }))
        .unwrap();
    assert_eq!(preview["ok"], true);
    assert_eq!(preview["from_cache"], true);
    assert!(
        preview["audio_data_url"]
            .as_str()
            .unwrap_or("")
            .starts_with("data:audio/wav;base64,")
    );

    // 3. 删除模型时：即使后台正在用临时 LocalHelper 预热该模型，也会主动终止子进程、等待工作线程退出再删目录与缓存
    let (reader_tx, reader_rx) = std::sync::mpsc::sync_channel::<u8>(1);
    struct BlockingPipe(std::sync::mpsc::Receiver<u8>);
    impl std::io::Read for BlockingPipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(b) => {
                    buf[0] = b;
                    Ok(1)
                }
                Err(_) => Ok(0),
            }
        }
    }
    let (preview_tx, preview_rx) = std::sync::mpsc::sync_channel::<u8>(1);
    let fake_helper =
        LocalHelper::from_streams(Box::new(BlockingPipe(reader_rx)), Box::new(std::io::sink()));
    let fake_in_flight_helper = LocalHelper::from_streams(
        Box::new(BlockingPipe(preview_rx)),
        Box::new(std::io::sink()),
    );
    assert!(fake_helper.is_alive());
    assert!(fake_in_flight_helper.is_alive());
    {
        let mut st = builtin.tts_preview_warmer.0.lock().unwrap();
        st.active = Some("kokoro");
        st.active_helper = Some(PreviewRunner::Helper(fake_helper.clone()));
        st.in_flight_previews.push((
            1,
            "kokoro",
            Some(PreviewRunner::Helper(fake_in_flight_helper.clone())),
            false,
        ));
        st.queue.push_back("kokoro");
        st.queue.push_back("matcha_zh_en");
    }
    let builtin_for_worker = builtin.clone();
    let worker = std::thread::spawn(move || {
        let (lock, cv) = &builtin_for_worker.tts_preview_warmer;
        {
            let mut st = lock.lock().unwrap();
            while !st.cancelled.contains("kokoro") {
                st = cv.wait(st).unwrap();
            }
            assert!(
                st.deleting.contains("kokoro"),
                "卸载期间应持有 deleting 门禁"
            );
            assert_eq!(
                st.in_flight_previews
                    .iter()
                    .find(|(seq, _, _, _)| *seq == 1)
                    .map(|(_, _, _, c)| *c),
                Some(true),
                "卸载期间应将并发试听条目标记为已取消"
            );
        }
        // 模拟弹窗 1.5s 轮询在卸载中途并发调用入队：必须被 deleting 门禁拦截且不得抹掉 cancelled
        assert_eq!(
            builtin_for_worker.enqueue_tts_preview_warmup_state("kokoro"),
            (false, false)
        );
        let mut st = lock.lock().unwrap();
        assert!(st.cancelled.contains("kokoro"));
        st.active = None;
        st.in_flight_previews.retain(|(seq, _, _, _)| *seq != 1);
        st.cancelled.remove("kokoro");
        cv.notify_all();
    });

    builtin.model_delete("kokoro").unwrap();
    worker.join().unwrap();
    drop(reader_tx);
    drop(preview_tx);
    assert!(
        !fake_helper.is_alive(),
        "卸载模型时应立即终止活跃预热 LocalHelper"
    );
    assert!(
        !fake_in_flight_helper.is_alive(),
        "卸载模型时应同步终止并发试听 LocalHelper"
    );
    {
        let st = builtin.tts_preview_warmer.0.lock().unwrap();
        assert_eq!(st.active, None);
        assert!(!st.deleting.contains("kokoro"));
        assert_eq!(
            st.queue.iter().copied().collect::<Vec<_>>(),
            vec!["matcha_zh_en"]
        );
    }
    assert!(!moss_voice::tts_voices::has_cached_preview(
        builtin.models.root(),
        "kokoro",
        45
    ));

    // 4. 宿主退出门禁置位后，禁止任何迟到轮询重新入队或拉起后台预热线程
    builtin.stop_all_tts_preview_warmups();
    assert_eq!(
        builtin.enqueue_tts_preview_warmup_state("matcha_zh_en"),
        (false, false)
    );
}

#[test]
fn denoise_turn_and_new_models_save_validate_and_persist() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();

    let defaults = &builtin.settings().builtin_local;
    assert!(!defaults.denoise);
    assert!(!defaults.turn);
    assert_eq!(defaults.denoise_model, "gtcrn_simple");
    assert_eq!(defaults.turn_model, "smart_turn_v3");

    // 类型不匹配或不存在的模型 id 必须拒绝
    assert!(
        builtin
            .local_save(&serde_json::json!({ "denoise_model": "smart_turn_v3" }))
            .is_err()
    );
    assert!(
        builtin
            .local_save(&serde_json::json!({ "turn_model": "gtcrn_simple" }))
            .is_err()
    );

    let saved = builtin
        .local_save(&serde_json::json!({
            "denoise": true,
            "turn": true,
            "denoise_model": "gtcrn_simple",
            "turn_model": "smart_turn_v3",
            "asr_model": "qwen3_asr_0_6b",
            "tts_model": "qwen3_tts_0_6b",
        }))
        .unwrap();
    let local = &saved["local"]["settings"];
    assert_eq!(local["denoise"], true);
    assert_eq!(local["turn"], true);
    assert_eq!(local["denoise_model"], "gtcrn_simple");
    assert_eq!(local["turn_model"], "smart_turn_v3");
    assert_eq!(local["asr_model"], "qwen3_asr_0_6b");
    assert_eq!(local["tts_model"], "qwen3_tts_0_6b");
    assert_eq!(local["tts_speaker"], 0, "切到新 TTS 模型应切到默认中文音色");

    let saved2 = builtin
        .local_save(&serde_json::json!({
            "asr_model": "firered_asr2_ctc",
            "tts_model": "zipvoice_distill",
            "tts_speaker": 1,
        }))
        .unwrap();
    assert_eq!(saved2["local"]["settings"]["asr_model"], "firered_asr2_ctc");
    assert_eq!(saved2["local"]["settings"]["tts_model"], "zipvoice_distill");
    assert_eq!(saved2["local"]["settings"]["tts_speaker"], 1);
}

#[test]
fn model_delete_blocks_during_chain_test_and_active_turn_or_fallback_workers() {
    struct BlockingPipe(std::sync::mpsc::Receiver<u8>);
    impl std::io::Read for BlockingPipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(b) => {
                    buf[0] = b;
                    Ok(1)
                }
                Err(_) => Ok(0),
            }
        }
    }

    let install_stub_model = |root: &std::path::Path, id: &str| {
        let spec = local_models::spec(id).unwrap();
        let dir = local_models::install_path(root, spec);
        std::fs::create_dir_all(&dir).unwrap();
        for rel in spec.required {
            let p = dir.join(rel);
            if *rel == "espeak-ng-data" || *rel == "dict" {
                std::fs::create_dir_all(&p).unwrap();
            } else {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, b"stub").unwrap();
            }
        }
        dir
    };

    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();
    let root = builtin.models.root();
    let kokoro_dir = install_stub_model(root, "kokoro");
    let turn_dir = install_stub_model(root, "smart_turn_v3");
    let zip_dir = install_stub_model(root, "zipvoice_distill");

    // 1. chain_test_lock 持有期间禁止删除模型，释放后放行
    {
        let _chain_guard = builtin.chain_test_lock.lock().unwrap();
        let err = builtin.model_delete("kokoro").unwrap_err();
        assert_eq!(err.code, "local_model_in_use");
        assert_eq!(err.status, 409);
    }

    // 启动回环 Echo 服务，使 builtin.running() 为真
    let registry = Arc::new(DbRegistry {
        db: host.db.clone(),
        host: host.clone(),
        settings_file: builtin.settings_file(),
    });
    let server = VoiceServer::start(
        ServerConfig {
            bind: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            ..Default::default()
        },
        registry,
    )
    .unwrap();
    *builtin.server.lock().unwrap() = Some(server);

    // 2. 运行态持有存活 TurnHelper 时拒绝删除 smart_turn_v3
    let (turn_tx, turn_rx) = std::sync::mpsc::sync_channel::<u8>(1);
    let turn_raw =
        LocalHelper::from_streams(Box::new(BlockingPipe(turn_rx)), Box::new(std::io::sink()));
    let turn_worker =
        TurnHelper::from_loaded_helper(turn_raw, Some(&turn_dir.join("smart-turn-v3.2-cpu.onnx")));
    *builtin.turn_worker.lock().unwrap() = Some(turn_worker.clone());
    let err = builtin.model_delete("smart_turn_v3").unwrap_err();
    assert_eq!(err.code, "local_model_in_use");
    assert_eq!(err.status, 409);
    assert!(turn_worker.is_alive());

    // 3. 运行态持有延迟加载本地兜底 TTS（zipvoice_distill）时拒绝删除该模型；未加载的候选模型（kokoro）可持 deleting 守卫安全删除
    let (fb_tx, fb_rx) = std::sync::mpsc::sync_channel::<u8>(1);
    let fb_helper =
        LocalHelper::from_streams(Box::new(BlockingPipe(fb_rx)), Box::new(std::io::sink()));
    let fb = LazyLocalFallbackTts::for_test_loaded(
        "zipvoice_distill",
        zip_dir,
        "kokoro",
        kokoro_dir,
        fb_helper.clone(),
    );
    *builtin.fallback_tts.lock().unwrap() = Some(fb.clone());

    let err = builtin.model_delete("zipvoice_distill").unwrap_err();
    assert_eq!(err.code, "local_model_in_use");
    assert_eq!(err.status, 409);
    assert!(fb_helper.is_alive());

    {
        let _deleting_guard = builtin.begin_fallback_tts_delete("kokoro").unwrap();
        assert!(fb.is_deleting("kokoro"));
    }
    assert!(!fb.is_deleting("kokoro"));
    builtin.model_delete("kokoro").unwrap();
    assert!(builtin.models.path_if_installed("kokoro").is_none());

    // 4. 停止服务后，turn_worker 与 fallback_tts 均被回收终止，此后可正常删除两模型
    builtin.stop_locked_inner(false);
    drop(turn_tx);
    drop(fb_tx);
    assert!(!turn_worker.is_alive());
    assert!(!fb_helper.is_alive());
    builtin.model_delete("smart_turn_v3").unwrap();
    builtin.model_delete("zipvoice_distill").unwrap();
    assert!(builtin.models.path_if_installed("smart_turn_v3").is_none());
    assert!(
        builtin
            .models
            .path_if_installed("zipvoice_distill")
            .is_none()
    );
}

#[test]
fn model_delete_waits_for_lifecycle_lock_before_deleting_running_speech_worker() {
    use moss_voice_providers::speech_cpp::SpeechCppWorker;
    use std::io::Read;

    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();
    let spec = local_models::spec("qwen3_tts_0_6b").unwrap();
    let qwen_dir = local_models::install_path(builtin.models.root(), spec);
    std::fs::create_dir_all(&qwen_dir).unwrap();
    for rel in spec.required {
        let f = std::fs::File::create(qwen_dir.join(rel)).unwrap();
        if *rel == spec.path {
            f.set_len(spec.size).unwrap();
        }
    }

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let lifecycle_guard = builtin.lifecycle.lock().unwrap();
    let builtin_bg = builtin.clone();
    let deleter = std::thread::spawn(move || {
        let _ = started_tx.send(());
        builtin_bg.model_delete("qwen3_tts_0_6b")
    });
    started_rx.recv().unwrap();

    // 模拟 start_locked_inner 在持有 lifecycle 锁末尾才写入 server 与 speech_worker 槽位
    let registry = Arc::new(DbRegistry {
        db: host.db.clone(),
        host: host.clone(),
        settings_file: builtin.settings_file(),
    });
    let server = VoiceServer::start(
        ServerConfig {
            bind: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            ..Default::default()
        },
        registry,
    )
    .unwrap();
    *builtin.server.lock().unwrap() = Some(server);
    let (_tx, rx) = std::sync::mpsc::sync_channel::<u8>(1);
    struct HoldPipe(std::sync::mpsc::Receiver<u8>);
    impl std::io::Read for HoldPipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(b) => {
                    buf[0] = b;
                    Ok(1)
                }
                Err(_) => Ok(0),
            }
        }
    }
    let reader = std::io::Cursor::new(b"{\"type\":\"ready\"}\n".to_vec()).chain(HoldPipe(rx));
    let worker = SpeechCppWorker::from_streams(
        Box::new(reader),
        Box::new(std::io::sink()),
        qwen_dir.join(spec.required[0]),
    )
    .unwrap();
    *builtin.speech_worker.lock().unwrap() = Some(worker);
    drop(lifecycle_guard);

    let err = deleter.join().unwrap().unwrap_err();
    assert_eq!(err.code, "local_model_in_use");
    assert_eq!(err.status, 409);
    assert!(builtin.models.path_if_installed("qwen3_tts_0_6b").is_some());

    let env = builtin.status()["local"]["speech_env"].clone();
    assert!(env.get("summary").and_then(Value::as_str).is_some());
    assert!(env.get("binary_path").is_none());
    let bundle = builtin.diagnostics_bundle();
    assert!(bundle["local"]["speech_env"].is_object());
    assert!(bundle["local"]["speech_env"].get("binary_path").is_none());
    assert_eq!(bundle["local"]["denoise"], false);
    assert_eq!(bundle["local"]["denoise_model"], "gtcrn_simple");
    assert_eq!(bundle["local"]["turn"], false);
    assert_eq!(bundle["local"]["turn_model"], "smart_turn_v3");
    builtin.stop_locked_inner(false);
}
