use super::*;

/// 用同样大小的稀疏文件模拟“已下载”的本地视觉模型（不真的启动推理进程）。
fn fake_install(builtin: &BuiltinVoice, id: &str) -> std::path::PathBuf {
    let spec = moss_voice::local_models::spec(id).unwrap();
    let dir = builtin.models.root_for(id).join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::File::create(dir.join(spec.path))
        .unwrap()
        .set_len(spec.size)
        .unwrap();
    for extra in spec.extras {
        std::fs::File::create(dir.join(extra.path))
            .unwrap()
            .set_len(extra.size)
            .unwrap();
    }
    assert!(
        builtin.models.path_if_installed(id).is_some(),
        "模拟安装应被识别为已下载"
    );
    dir
}

/// 2026-11-09：激光后端的事件写进同一本记录，语音中心事件列表里来源标为“激光雕刻”。
#[test]
fn laser_events_reach_voice_center_with_laser_source() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host.clone())).0.unwrap();
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let body = serde_json::json!({
        "connection_mode": "network", "network_host": "127.0.0.1",
        "network_telnet_port": closed, "network_timeout": 0.5,
    })
    .to_string();
    let request = format!(
        "POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    host.api_service.handle_http(request.as_bytes());
    let events = builtin.status()["events"].as_array().unwrap().clone();
    let laser = events
        .iter()
        .find(|event| event["source"] == "laser")
        .expect("握手失败应写进语音中心事件列表");
    assert_eq!(laser["level"], "warn");
    assert_eq!(laser["category"], "device");
    assert!(
        laser["text"]
            .as_str()
            .unwrap()
            .starts_with("连接雕刻机失败"),
        "{laser}"
    );
    assert!(laser["hint"].as_str().is_some());
}

/// n74：没选本地模型时不启动；模型文件不全时启动失败并记下原因；停用后回到空闲。
#[test]
fn local_vision_runtime_reports_missing_launch_and_files() {
    let runtime = LocalVision::default();
    assert!(runtime.ensure().unwrap_err().contains("没有选用"));
    assert_eq!(runtime.status()["state"], "idle");
    let temp = tempfile::tempdir().unwrap();
    runtime.configure(Some(LocalVisionLaunch {
        model: "qwen3_vl_2b".into(),
        dir: temp.path().join("qwen3_vl_2b"),
        root: temp.path().to_path_buf(),
        mirror: None,
    }));
    let error = runtime.ensure().unwrap_err();
    assert!(error.contains("还没下载完整"), "{error}");
    assert_eq!(runtime.status()["state"], "failed");
    assert_eq!(runtime.status()["model"], "qwen3_vl_2b");
    runtime.configure(None);
    assert_eq!(runtime.status()["state"], "idle");
}

/// 本地视觉只要求模型已下载、不要密钥；未下载或未知模型拒绝保存；保存后视觉工厂就绪且不需要地址。
#[test]
fn vision_save_accepts_installed_local_model_without_key() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();
    let save = |model: &str, enabled: bool| {
        builtin.vision_save(&serde_json::json!({
            "settings": {"enabled": enabled, "url": "", "model": "", "local_model": model},
            "key": {"action": "keep"},
        }))
    };
    let unknown = save("qwen3_vl_99b", true).unwrap_err();
    assert_eq!(unknown.code, "vision_invalid");
    let missing = save("qwen3_vl_2b", true).unwrap_err();
    assert!(
        missing.message.contains("先在“本地模型”里下载"),
        "{}",
        missing.message
    );
    let status = builtin.vision_status();
    assert_eq!(
        status["local_models"].as_array().unwrap().len(),
        4,
        "Qwen3-VL 与 Qwen3.5 各 2B / 4B 共四档"
    );
    assert_eq!(status["local_models"][0]["installed"], false);

    fake_install(&builtin, "qwen3_vl_2b");
    let status = save("qwen3_vl_2b", true).expect("已下载的本地视觉模型不需要地址与密钥");
    assert_eq!(status["settings"]["local_model"], "qwen3_vl_2b");
    assert_eq!(
        status["ready"], true,
        "选好已下载的本地模型即视为就绪（推理进程按需启动）"
    );
    assert_eq!(
        status["local_runtime"]["state"], "idle",
        "服务没运行时不预加载"
    );
    assert_eq!(status["local_runtime"]["model"], "qwen3_vl_2b");
    assert_eq!(
        builtin.local_vision.launch().map(|launch| launch.model),
        Some("qwen3_vl_2b".to_owned())
    );

    // 改回云端（未填地址、没有密钥）且关闭：不再保留本地启动方式
    builtin
        .vision_save(&serde_json::json!({
            "settings": {"enabled": false, "url": "", "model": ""},
            "key": {"action": "keep"},
        }))
        .unwrap();
    assert!(builtin.local_vision.launch().is_none());
    // 删除模型前先停掉（此处未运行，确认不会报错并能删掉）
    fake_install(&builtin, "qwen3_vl_2b");
    builtin.model_delete("qwen3_vl_2b").unwrap();
    assert!(builtin.models.path_if_installed("qwen3_vl_2b").is_none());
}

#[test]
fn n80_colab_bridge_defaults_off_and_restores_on_disable() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();
    fake_install(&builtin, "qwen3_vl_2b");
    builtin
        .vision_save(&serde_json::json!({
            "settings": {"enabled": true, "url": "", "model": "", "local_model": "qwen3_vl_2b"},
            "key": {"action": "keep"},
        }))
        .unwrap();

    let initial = builtin.status();
    assert_eq!(initial["local"]["colab"]["enabled"], false);
    assert_eq!(initial["local"]["colab"]["active"], false);
    assert_eq!(initial["local"]["colab"]["cli"]["installed"], true);
    assert_eq!(initial["local"]["colab"]["cli"]["logged_in"], false);
    assert_eq!(
        builtin.local_vision.launch().map(|l| l.model),
        Some("qwen3_vl_2b".to_owned())
    );

    // 未登录 Colab CLI 且缺少手动地址或密钥时拒绝开启
    let err = builtin
        .local_save(&serde_json::json!({
            "colab": {"enabled": true, "url": "https://abc-123.trycloudflare.com"}
        }))
        .unwrap_err();
    assert!(
        err.message.contains("尚未登录 Colab CLI"),
        "{}",
        err.message
    );

    // 模拟新用户未安装 Colab CLI：cli.installed == false -> 执行 install -> cli.installed == true
    builtin
        .colab_cli_installed_for_test
        .store(false, std::sync::atomic::Ordering::Release);
    let uninstalled = builtin.status();
    assert_eq!(uninstalled["local"]["colab"]["cli"]["installed"], false);
    assert_eq!(uninstalled["local"]["colab"]["cli"]["logged_in"], false);
    let installed_after = builtin
        .local_save(&serde_json::json!({
            "colab_cli": {"action": "install"}
        }))
        .unwrap();
    assert_eq!(installed_after["local"]["colab"]["cli"]["installed"], true);
    assert_eq!(installed_after["local"]["colab"]["cli"]["logged_in"], false);

    // 本机保存与清除 550W 参考音频（仅存本机，云端启动时自动同步）
    use base64::Engine as _;
    let mut fake_wav = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
    fake_wav.resize(128, 0);
    let wav_b64 = base64::engine::general_purpose::STANDARD.encode(&fake_wav);
    let with_wav = builtin
        .local_save(&serde_json::json!({
            "colab_cli": {
                "action": "set_ref_audio",
                "file_name": "moss_550w.wav",
                "wav_base64": wav_b64
            }
        }))
        .unwrap();
    assert_eq!(
        with_wav["local"]["colab"]["cli"]["ref_audio_configured"],
        true
    );
    assert_eq!(
        with_wav["local"]["colab"]["cli"]["ref_audio_name"],
        "moss_550w.wav"
    );
    let cleared_wav = builtin
        .local_save(&serde_json::json!({
            "colab_cli": {"action": "clear_ref_audio"}
        }))
        .unwrap();
    assert_eq!(
        cleared_wav["local"]["colab"]["cli"]["ref_audio_configured"],
        false
    );

    // 通过 Colab CLI 登录：login_start -> login_complete 后显示已登录，此时即使不填地址和密钥也允许开启并自动拉起云端 T4
    let login_started = builtin
        .local_save(&serde_json::json!({
            "colab_cli": {"action": "login_start"}
        }))
        .unwrap();
    assert!(
        login_started["local"]["colab"]["cli"]["auth_url"]
            .as_str()
            .unwrap_or("")
            .starts_with("https://accounts.google.com/")
    );
    let logged_in_status = builtin
        .local_save(&serde_json::json!({
            "colab_cli": {"action": "login_complete", "code": "mock-valid-auth-code"}
        }))
        .unwrap();
    assert_eq!(logged_in_status["local"]["colab"]["cli"]["logged_in"], true);
    assert_eq!(
        logged_in_status["local"]["colab"]["cli"]["account_hint"],
        "t***r@gmail.com"
    );

    let auto_on = builtin
        .local_save(&serde_json::json!({
            "colab": {"enabled": true, "url": ""}
        }))
        .unwrap();
    assert_eq!(auto_on["local"]["colab"]["enabled"], true);
    assert_eq!(
        auto_on["local"]["colab"]["cli"]["runner_state"], "starting",
        "已登录 Colab CLI 时开启开关应自动在后台拉起云端 T4"
    );

    // 退出 Colab CLI 登录后回到未登录状态
    let logged_out = builtin
        .local_save(&serde_json::json!({
            "colab_cli": {"action": "logout"},
            "colab": {"enabled": false}
        }))
        .unwrap();
    assert_eq!(logged_out["local"]["colab"]["cli"]["logged_in"], false);

    // 填入隧道地址与密钥并开启：临时接管大模型、看图与合成，暂停本机视觉推理进程预热
    let on_status = builtin
        .local_save(&serde_json::json!({
            "colab": {
                "enabled": true,
                "url": "https://abc-123.trycloudflare.com",
                "api_key": "moss-test-colab-secret",
                "llm_model": "qwen3-vl-8b",
                "tts_model": "qwen3-tts",
                "tts_voice": "moss",
                "tts_speed": 1.0,
            }
        }))
        .unwrap();
    assert_eq!(on_status["local"]["colab"]["enabled"], true);
    assert_eq!(on_status["local"]["colab"]["active"], true);
    assert_eq!(on_status["local"]["colab"]["key_configured"], true);
    assert_eq!(
        on_status["local"]["colab"]["url"],
        "https://abc-123.trycloudflare.com/v1"
    );
    assert_eq!(on_status["local"]["ready"]["llm"], true);
    assert_eq!(on_status["local"]["ready"]["tts"], true);
    assert!(
        builtin.local_vision.launch().is_none(),
        "Colab 接管看图期间不保留本机视觉模型启动项"
    );
    let v_on = builtin.vision_status();
    assert_eq!(v_on["ready"], true);
    assert_eq!(v_on["colab_override"]["active"], true);
    assert_eq!(v_on["colab_override"]["model"], "qwen3-vl-8b");
    let raw_settings =
        std::fs::read_to_string(temp.path().join("settings.json")).unwrap_or_default();
    assert!(
        !raw_settings.contains("moss-test-colab-secret"),
        "Colab 密钥只存 app_secrets，绝不写入 settings.json"
    );

    // 关闭开关：立即恢复原有的本地视觉与大模型/合成状态，且保留已存的地址与密钥供下次直接开
    let off_status = builtin
        .local_save(&serde_json::json!({
            "colab": {"enabled": false}
        }))
        .unwrap();
    assert_eq!(off_status["local"]["colab"]["enabled"], false);
    assert_eq!(off_status["local"]["colab"]["active"], false);
    assert_eq!(off_status["local"]["colab"]["key_configured"], true);
    assert_eq!(off_status["local"]["ready"]["llm"], false);
    assert_eq!(
        builtin.local_vision.launch().map(|l| l.model),
        Some("qwen3_vl_2b".to_owned()),
        "关闭 Colab 联动后立即恢复原有的本地视觉模型"
    );
    let v_off = builtin.vision_status();
    assert_eq!(v_off["colab_override"]["active"], false);
    assert_eq!(v_off["settings"]["local_model"], "qwen3_vl_2b");

    // 真实子进程跟踪与强杀验证：挂起中的子进程记录 active_pid，取消时立即被强杀回收且不泄漏
    let start = std::time::Instant::now();
    let cur_gen = builtin.colab_cli.lock().unwrap().generation;
    let slot_for_worker = builtin.colab_cli.clone();
    let handle = std::thread::spawn(move || {
        #[cfg(windows)]
        let mut cmd = {
            use std::os::windows::process::CommandExt;
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"])
                .creation_flags(0x0800_0000);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        BuiltinVoice::run_tracked_colab_output(&slot_for_worker, cur_gen, &mut cmd)
    });
    let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while builtin.colab_cli.lock().unwrap().active_pid.is_none() {
        assert!(
            std::time::Instant::now() < wait_deadline,
            "run_tracked_colab_output 须记录子进程 active_pid"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    builtin.cancel_colab_cli_runner(Some("测试取消"));
    let outcome = handle.join().unwrap();
    assert!(
        matches!(outcome, Ok(None)),
        "取消后过期的子进程输出必须丢弃并返回 Ok(None): {outcome:?}"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "cancel_colab_cli_runner 必须立即终止卡住的子进程而非等待 30 秒自然退出"
    );
    assert!(builtin.colab_cli.lock().unwrap().active_pid.is_none());
}

#[test]
fn n83_full_font_pack_dismissal_and_delete_while_running() {
    let temp = tempfile::tempdir().unwrap();
    let host =
        Arc::new(laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap());
    let builtin = init(Some(host)).0.unwrap();
    let font_id = moss_voice::local_models::XIAOZHI_NOTO_V1_FULL_ID;
    let spec = moss_voice::local_models::spec(font_id).unwrap();
    assert_eq!(
        builtin.full_font_dir(),
        builtin.models.root_for(font_id).join("noto_v1_full")
    );

    // 模拟本地已安装全部必需文件：不发起自动下载
    let font_dir = builtin.full_font_dir();
    for rel in spec.required {
        let p = font_dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"stub").unwrap();
    }
    assert!(builtin.models.path_if_installed(font_id).is_some());
    assert!(
        !builtin.ensure_full_font_ready_with_base(Some("http://127.0.0.1:1")),
        "已安装时不重复触发后台下载"
    );

    // 用户手动删除后写入忽略标记：不会在重启后背着用户自动重下
    builtin.model_delete(font_id).unwrap();
    assert!(builtin.models.path_if_installed(font_id).is_none());
    assert!(builtin.full_font_dismissed_marker().exists());
    assert!(
        !builtin.ensure_full_font_ready_with_base(Some("http://127.0.0.1:1")),
        "用户手动删除后尊重选择，不再自动重下"
    );

    // 用户主动在界面点“下载”时清除忽略标记（对接本机假 HTTP 服务，不访问真实外网）
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mock_base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            use std::io::Write;
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    builtin
        .local_save(&serde_json::json!({ "mirror": mock_base }))
        .unwrap();
    builtin.model_download(font_id).unwrap();
    assert!(
        !builtin.full_font_dismissed_marker().exists(),
        "手动点击下载应清除忽略标记"
    );
    builtin.model_cancel(font_id);
    assert!(
        builtin.full_font_dismissed_marker().exists(),
        "手动取消下载同样写入忽略标记"
    );
}
