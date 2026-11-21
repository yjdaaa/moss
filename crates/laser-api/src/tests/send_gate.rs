use super::*;

/// 回完 `ok_lines` 个 ok 后保持静默：没有数据时按真实串口那样报读超时，而不是“连接断开”，
/// 发送线程会一直停在发送中（每 500ms 心跳一次，设备回状态报告），直到被取消。
/// 2026-11-08：此前用 `fake_transport_factory` 模拟“设备不再回 ok”，读空即断开，任务立即失败；
/// 断言“仍在发送中”是否成立全看测试线程先跑还是发送线程先跑，满负载时会误报。
fn stalling_transport_factory(ok_lines: usize) -> TransportFactory {
    struct Stalling(Box<dyn laser_device::DeviceTransport>);
    impl laser_device::DeviceTransport for Stalling {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.0.write_line(line)
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            self.0.write_realtime(command)
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            match self.0.read_line()? {
                Some(line) => Ok(Some(line)),
                None => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    Err("read timeout".into())
                }
            }
        }
    }
    let inner = fake_transport_factory(ok_lines);
    std::sync::Arc::new(move |spec: &laser_device::ConnectionSpec| {
        let device = inner(spec)?;
        Ok(Box::new(Stalling(device)) as Box<dyn laser_device::DeviceTransport>)
    })
}

#[test]
fn job_launcher_rejects_power_or_busy_state_before_writing_gcode() {
    for (max, status) in [("0", "<Idle>"), ("1000", "<Run>"), ("1000", "<Check>")] {
        let fake = laser_device::FakeGrbl::new([
            format!("$30={max}"),
            "$32=1".into(),
            "ok".into(),
            "error:3".into(),
            status.into(),
        ]);
        let observed = fake.clone();
        let service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake.clone()))));
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("job.gcode");
        std::fs::write(&file, "M4 S100\nG1 X1 F100\nM5\n").unwrap();
        let object = serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23});
        let sha = hex::encode(Sha256::digest(std::fs::read(&file).unwrap()));
        let (code, response) = service
            .launch_send_job(
                "test-preflight",
                file,
                &sha,
                object.as_object().unwrap(),
                &laser_contracts::CallerContext::Loopback,
            )
            .unwrap_err();
        assert_eq!(code, 400);
        assert_eq!(response["detail"]["error_code"], "device_preflight_failed");
        assert_eq!(observed.lines(), ["$$", "$CD"]);
        assert!(service.active_jobs.lock().unwrap().is_empty());
    }
}

#[test]
fn job_launcher_accepts_255_scale_and_adapts_dynamically() {
    let fake = laser_device::FakeGrbl::new([
        "$30=255".into(),
        "$32=1".into(),
        "ok".into(),
        "<Idle>".into(),
        "ok".into(),
        "ok".into(),
        "ok".into(),
        "ok".into(),
        "<Idle>".into(),
    ]);
    let _observed = fake.clone();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake.clone()))));
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("job.gcode");
    std::fs::write(&file, "M4 S1000\nG1 X1 F100\nM5\n").unwrap();
    let object = serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23});
    let sha = hex::encode(Sha256::digest(std::fs::read(&file).unwrap()));
    let res = service
        .launch_send_job(
            "test-255-adaptive",
            file,
            &sha,
            object.as_object().unwrap(),
            &laser_contracts::CallerContext::Loopback,
        )
        .unwrap();
    assert_eq!(res["success"], true);
}

fn run_confirm_send_gate_scenario(service: &ApiService, root: &std::path::Path) {
    use std::io::Write;
    let body = serde_json::json!({
        "text": "SHA", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
    })
    .to_string();
    let request = format!(
        "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let generated = parse_json_response(&service.handle_http(request.as_bytes()));
    assert_eq!(generated["success"], true, "{generated}");
    let workflow_id = generated["result"]["workflow_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let gcode_file = generated["result"]["artifacts"]["gcode_file"]
        .as_str()
        .unwrap()
        .to_owned();
    let expected_sha = generated["result"]["artifacts"]["gcode_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let gcode_path = root.join(&gcode_file);
    let original = std::fs::read(&gcode_path).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&original)),
        expected_sha,
        "冻结的 SHA 必须来自物理刀路文件"
    );

    let send_body = format!(
        r#"{{"action":"confirm_send","workflow_id":"{workflow_id}","confirmed":true,"connection_mode":"network","network_host":"127.0.0.1","network_telnet_port":23}}"#
    );
    let send_request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        send_body.len(),
        send_body
    );

    // 1. 篡改落盘刀路：重算 SHA 与快照不一致，必须拒绝
    std::fs::OpenOptions::new()
        .append(true)
        .open(&gcode_path)
        .unwrap()
        .write_all(b"\nG1 X999 Y999 S1000\n")
        .unwrap();
    let rejected_text =
        String::from_utf8_lossy(&service.handle_http(send_request.as_bytes())).into_owned();
    assert!(rejected_text.starts_with("HTTP/1.1 400"), "{rejected_text}");
    assert!(
        rejected_text.contains("send_gate_rejected"),
        "{rejected_text}"
    );
    assert!(rejected_text.contains("SHA-256"), "{rejected_text}");

    // 2. 恢复原文件：门禁通过并推进到 sending，快照冻结物理 SHA
    std::fs::write(&gcode_path, &original).unwrap();
    let accepted = parse_json_response(&service.handle_http(send_request.as_bytes()));
    assert_eq!(accepted["success"], true, "{accepted}");
    assert_eq!(accepted["result"]["status"], "sending");
    assert_eq!(
        accepted["result"]["confirmation_snapshot"]["gcode_sha256"],
        expected_sha
    );

    assert_eq!(
        accepted["result"]["send_result"]["success"], true,
        "{accepted}"
    );

    // 3. 发送中/已完成重复下发必须拦截（防二次烧蚀）
    let duplicate_text =
        String::from_utf8_lossy(&service.handle_http(send_request.as_bytes())).into_owned();
    assert!(
        duplicate_text.starts_with("HTTP/1.1 400"),
        "{duplicate_text}"
    );
    assert!(
        duplicate_text.contains("workflow_not_sendable"),
        "{duplicate_text}"
    );

    // 4. 真实 JobWorker 逐行发送并等待 ok，轮询推进到终态；取消后不可再发送
    let finished = wait_for_status(service, &workflow_id, "completed");
    assert_eq!(finished["result"]["status"], "completed", "{finished}");
    let sent = finished["result"]["send_result"]["lines_sent"]
        .as_u64()
        .unwrap_or(0);
    let total = finished["result"]["send_result"]["total_lines"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        total > 0 && sent == total,
        "全部刀路行必须收到设备 ok: {sent}/{total}"
    );
    assert_eq!(service.active_job_count(), 0, "作业完成后必须从注册表移除");
    assert!(
        !service.device_work_in_progress(),
        "作业完成后应用内更新不再被加工任务挡住"
    );
    let cancel_body = format!(r#"{{"action":"cancel","workflow_id":"{workflow_id}"}}"#);
    let cancel_request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        cancel_body.len(),
        cancel_body
    );
    // 5. 已完成任务不得被取消改写为 cancelled：拒绝并保留 completed 与逐行确认记录
    let cancel_text =
        String::from_utf8_lossy(&service.handle_http(cancel_request.as_bytes())).into_owned();
    assert!(cancel_text.starts_with("HTTP/1.1 400"), "{cancel_text}");
    assert!(cancel_text.contains("workflow_completed"), "{cancel_text}");
    let after = wait_for_status(service, &workflow_id, "completed");
    assert_eq!(after["result"]["status"], "completed", "{after}");
    assert_eq!(
        after["result"]["send_result"]["lines_sent"].as_u64(),
        Some(sent),
        "{after}"
    );
    let after_cancel =
        String::from_utf8_lossy(&service.handle_http(send_request.as_bytes())).into_owned();
    assert!(
        after_cancel.contains("workflow_not_sendable"),
        "{after_cancel}"
    );
}

#[test]
fn confirm_send_recomputes_physical_gcode_sha_for_store_and_memory_backends() {
    for use_store in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let mut service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(temp.path().to_path_buf())
        .with_transport_factory(fake_transport_factory(200_000));
        if use_store {
            service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
        }
        run_confirm_send_gate_scenario(&service, temp.path());
    }
}

/// 已完成任务拒绝取消的门禁不得挡住发送中的取消：设备停止回 ok 时取消必须放行并停在中途。
#[test]
fn cancel_mid_send_is_still_accepted_for_store_and_memory_backends() {
    for use_store in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let mut service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(temp.path().to_path_buf())
        .with_transport_factory(stalling_transport_factory(5));
        if use_store {
            service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
        }
        let preview = post_json(
            &service,
            "/api/text-task/generate",
            &serde_json::json!({
                "text": "MOSS-CANCEL", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 8
            }),
        );
        assert_eq!(
            preview["success"], true,
            "文字须在 100 mm 幅面内：{preview}"
        );
        let workflow_id = preview["result"]["workflow_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let send = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
                "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23,
            }),
        );
        assert_eq!(send["success"], true, "{send}");
        assert_eq!(
            service.active_job_count(),
            1,
            "设备只回 5 个 ok，任务必须仍在发送中"
        );
        assert!(
            service.device_work_in_progress(),
            "发送中必须拒绝应用内更新重启"
        );

        let cancel = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({"action": "cancel", "workflow_id": workflow_id}),
        );
        assert_eq!(cancel["success"], true, "use_store={use_store}: {cancel}");
        assert_eq!(cancel["result"]["status"], "cancelled", "{cancel}");

        let finished = wait_for_status(&service, &workflow_id, "cancelled");
        assert_eq!(finished["result"]["status"], "cancelled", "{finished}");
        let sent = finished["result"]["send_result"]["lines_sent"]
            .as_u64()
            .unwrap_or(0);
        let total = finished["result"]["send_result"]["total_lines"]
            .as_u64()
            .unwrap_or(0);
        assert!(sent < total, "取消必须发生在发送中途: {sent}/{total}");
    }
}

/// SD 脱机：上传成功但固件不返回文件哈希（未启用 HashFS）时不得发送 `$SD/Run`（2026-10-14）。
#[test]
fn sd_offline_refuses_to_run_when_firmware_returns_no_file_hash() {
    use std::io::{Read, Write};
    let upload = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = upload.local_addr().unwrap().port();
    let uploads = std::thread::spawn(move || {
        let (mut stream, _) = upload.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let mut buffer = [0u8; 4096];
        while let Ok(n) = stream.read(&mut buffer) {
            if n == 0 {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(recording_transport_factory(200_000, written.clone()));
    let preview = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "SD", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
        }),
    );
    let workflow_id = preview["result"]["workflow_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let send = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
            "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23,
            "network_http_port": http_port, "offline_sd": true
        }),
    );
    assert_eq!(send["success"], true, "{send}");
    let status = wait_for_status(&service, &workflow_id, "failed");
    uploads.join().unwrap();
    assert_eq!(status["result"]["status"], "failed", "{status}");
    assert!(
        status["result"]["send_result"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("HashFS"),
        "{status}"
    );
    let lines = written.lock().unwrap().clone();
    assert!(
        lines.iter().any(|line| line.starts_with("$File/ShowHash=")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("$SD/Run")),
        "没有固件哈希不能启动 SD 作业: {lines:?}"
    );
}

/// SD 脱机上传端口不能截断：65616 曾被 `as u16` 变成 80 并上传到错误端口；
/// 无效端口必须在连接设备前 400 拒绝（2026-11-05）。
#[test]
fn sd_offline_rejects_out_of_range_http_port_before_opening_device() {
    let temp = tempfile::tempdir().unwrap();
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(recording_transport_factory(200_000, written.clone()));
    let preview = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "SD", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
        }),
    );
    let workflow_id = preview["result"]["workflow_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (key, port) in [
        ("network_http_port", serde_json::json!(65_616)),
        ("network_http_port", serde_json::json!(0)),
        ("http_port", serde_json::json!(-1)),
        ("network_http_port", serde_json::json!("8080")),
    ] {
        let mut body = serde_json::json!({
            "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
            "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23,
            "offline_sd": true
        });
        body[key] = port.clone();
        let send = post_json(&service, "/api/workflow", &body);
        assert_eq!(send["success"], false, "{key}={port}: {send}");
        assert_eq!(
            send["detail"]["error_code"], "sd_http_port_invalid",
            "{key}={port}: {send}"
        );
        assert!(
            written.lock().unwrap().is_empty(),
            "端口无效时不能连接设备: {key}={port}"
        );
        assert_eq!(service.active_job_count(), 0);
    }
    let mut object = serde_json::Map::new();
    assert_eq!(sd_offline_http_port(&object), Ok(80));
    object.insert("http_port".into(), serde_json::json!(8080));
    assert_eq!(sd_offline_http_port(&object), Ok(8080));
    object.insert("network_http_port".into(), serde_json::json!(65_535));
    assert_eq!(sd_offline_http_port(&object), Ok(65_535));
}

/// SD 脱机必须和流式发送使用同一功率标尺：设备 `$30=255` 时上传换算后的行，并用上传字节的 SHA 做固件哈希门禁。
#[test]
fn sd_offline_upload_payload_rescales_power_to_device_scale() {
    let original = b"M4 S1000\nG1X10S500 F600\nM5\n".to_vec();
    let original_sha = hex::encode(Sha256::digest(&original));
    let lines: Vec<String> = ["M4 S1000", "G1X10S500 F600", "M5"]
        .map(str::to_owned)
        .to_vec();

    let (same, same_sha) = sd_offline_upload_payload(original.clone(), &original_sha, &lines, 1.0);
    assert_eq!(same, original, "标尺 1000 时原样上传已门禁校验的文件");
    assert_eq!(same_sha, original_sha);

    let (scaled, scaled_sha) =
        sd_offline_upload_payload(original.clone(), &original_sha, &lines, 255.0 / 1000.0);
    assert_eq!(
        String::from_utf8(scaled.clone()).unwrap(),
        "M4 S255\nG1X10S128 F600\nM5\n"
    );
    assert_eq!(
        scaled_sha,
        hex::encode(Sha256::digest(&scaled)),
        "固件哈希门禁必须比对实际上传的字节"
    );
    assert_ne!(scaled_sha, original_sha);
}

/// `confirm_send` / `send` 不得采用客户端自带的 `start_from_line`（只允许 `resume` 从服务端记录写入），
/// 否则会先发 `$X` 解锁再跳过开头 G21/G90/M5 设置行。
#[test]
fn confirm_send_rejects_client_supplied_start_from_line_for_store_and_memory_backends() {
    for use_store in [true, false] {
        for action in ["confirm_send", "send"] {
            let temp = tempfile::tempdir().unwrap();
            let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut service = ApiService::new(
                test_loopback_settings(),
                laser_contracts::builtin_material_library(),
            )
            .with_generated_root(temp.path().to_path_buf())
            .with_transport_factory(recording_transport_factory(200_000, written.clone()));
            if use_store {
                service =
                    service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
            }
            let preview = post_json(
                &service,
                "/api/text-task/generate",
                &serde_json::json!({
                    "text": "SKIP", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
                }),
            );
            let workflow_id = preview["result"]["workflow_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let send = post_json(
                &service,
                "/api/workflow",
                &serde_json::json!({
                    "action": action, "workflow_id": workflow_id, "confirmed": true, "start_from_line": 5,
                    "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23,
                }),
            );
            assert_eq!(
                send["success"], false,
                "use_store={use_store} action={action}: {send}"
            );
            assert_eq!(
                send["detail"]["error_code"], "resume_line_not_allowed",
                "{send}"
            );
            assert_eq!(service.active_job_count(), 0);
            assert!(
                written.lock().unwrap().is_empty(),
                "被拒绝的发送不得打开设备或写任何行"
            );
        }
    }
}

#[test]
fn send_rejects_gcode_changed_after_first_sha_check_without_writing_job_lines() {
    for use_store in [true, false] {
        for offline_sd in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let base_factory = recording_transport_factory(200_000, written.clone());
            let gcode_path = std::sync::Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));
            let path_for_factory = gcode_path.clone();
            let factory: TransportFactory = std::sync::Arc::new(move |spec| {
                let path = path_for_factory.lock().unwrap().clone().unwrap();
                std::fs::write(path, b"M4 S1000\nG1 X999 Y999 F100\nM5\n").unwrap();
                base_factory(spec)
            });
            let mut service = ApiService::new(
                test_loopback_settings(),
                laser_contracts::builtin_material_library(),
            )
            .with_generated_root(temp.path().to_path_buf())
            .with_transport_factory(factory);
            if use_store {
                service =
                    service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
            }
            let preview = post_json(
                &service,
                "/api/text-task/generate",
                &serde_json::json!({
                    "text": "SHA", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
                }),
            );
            assert_eq!(preview["success"], true, "{preview}");
            let workflow_id = preview["result"]["workflow_id"].as_str().unwrap();
            *gcode_path.lock().unwrap() = Some(
                temp.path().join(
                    preview["result"]["artifacts"]["gcode_file"]
                        .as_str()
                        .unwrap(),
                ),
            );
            let send = post_json(
                &service,
                "/api/workflow",
                &serde_json::json!({
                    "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
                    "connection_mode": "network", "network_host": "127.0.0.1",
                    "network_telnet_port": 23, "offline_sd": offline_sd
                }),
            );
            assert_eq!(send["detail"]["error_code"], "send_gate_rejected", "{send}");
            assert!(
                written
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|line| line == "$$" || line == "$CD" || line.starts_with("$RI=")),
                "校验失败不能写入 G-code、SD 启动或执行同步命令: {:?}",
                written.lock().unwrap()
            );
            assert_eq!(service.active_job_count(), 0);
        }
    }
}

#[test]
fn workflow_routes_keep_preview_status_cancel_and_send_gate_stateful() {
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    );
    let body = br#"{"action":"preview","source_type":"text","text":"hello"}"#;
    let request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let preview = service.handle_http(request.as_bytes());
    let start = preview
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let preview: serde_json::Value = serde_json::from_slice(&preview[start..]).unwrap();
    let workflow_id = preview["result"]["workflow_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(preview["result"]["status"], "preview_ready");

    let status_body = format!(r#"{{"action":"status","workflow_id":"{workflow_id}"}}"#);
    let status_request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        status_body.len(),
        status_body
    );
    let status = service.handle_http(status_request.as_bytes());
    assert!(String::from_utf8_lossy(&status).starts_with("HTTP/1.1 200 OK"));

    let send_body =
        format!(r#"{{"action":"confirm_send","workflow_id":"{workflow_id}","confirmed":true}}"#);
    let send_request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        send_body.len(),
        send_body
    );
    let send = service.handle_http(send_request.as_bytes());
    assert!(String::from_utf8_lossy(&send).contains("confirmation_snapshot_missing"));
}

#[test]
fn workflow_routes_persist_when_runtime_db_is_attached() {
    let store = std::sync::Arc::new(laser_runtime::MemoryStore::new());
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
    .with_store(store.clone());
    let body = br#"{"action":"preview","source_type":"text","text":"persist"}"#;
    let request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let response = service.handle_http(request.as_bytes());
    let start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let response: serde_json::Value = serde_json::from_slice(&response[start..]).unwrap();
    let workflow_id = response["result"]["workflow_id"].as_str().unwrap();
    use laser_runtime::RuntimeStore;
    let stored = store
        .get(laser_runtime::RecordKind::Workflow, workflow_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.status.as_deref(), Some("preview_ready"));
}

#[test]
fn confirm_send_persist_failure_cancels_launched_worker_and_clears_active_jobs() {
    use std::sync::atomic::{AtomicU8, Ordering};

    struct FaultyStore {
        inner: laser_runtime::MemoryStore,
        // 0 = normal, 1 = fail put, 2 = fail get after preflight (count-gated)
        mode: AtomicU8,
        remaining_ok_gets: AtomicU8,
    }

    impl laser_runtime::RuntimeStore for FaultyStore {
        fn get(
            &self,
            kind: laser_runtime::RecordKind,
            id: &str,
        ) -> Result<Option<laser_runtime::StoredRecord>, laser_runtime::SchemaError> {
            if self.mode.load(Ordering::SeqCst) == 2 {
                let prev = self.remaining_ok_gets.load(Ordering::SeqCst);
                if prev == 0 {
                    return Err(laser_runtime::SchemaError::Busy);
                }
                self.remaining_ok_gets.store(prev - 1, Ordering::SeqCst);
            }
            self.inner.get(kind, id)
        }

        fn put(
            &self,
            record: &laser_runtime::StoredRecord,
        ) -> Result<(), laser_runtime::SchemaError> {
            if self.mode.load(Ordering::SeqCst) == 1 {
                return Err(laser_runtime::SchemaError::Busy);
            }
            self.inner.put(record)
        }

        fn set_status(
            &self,
            kind: laser_runtime::RecordKind,
            id: &str,
            status: &str,
            now: f64,
        ) -> Result<bool, laser_runtime::SchemaError> {
            self.inner.set_status(kind, id, status, now)
        }

        fn list_recent(
            &self,
            kind: laser_runtime::RecordKind,
            limit: usize,
        ) -> Result<Vec<laser_runtime::StoredRecord>, laser_runtime::SchemaError> {
            self.inner.list_recent(kind, limit)
        }

        fn save_feedback(
            &self,
            workflow_id: &str,
            feedback: &serde_json::Value,
        ) -> Result<bool, laser_runtime::SchemaError> {
            self.inner.save_feedback(workflow_id, feedback)
        }

        fn adopt_feedback(
            &self,
            workflow_id: &str,
            feedback: &serde_json::Value,
            preview: &laser_runtime::StoredRecord,
        ) -> Result<bool, laser_runtime::SchemaError> {
            self.inner.adopt_feedback(workflow_id, feedback, preview)
        }
    }

    for fail_mode in [1u8, 2u8] {
        let temp = tempfile::tempdir().unwrap();
        let gen_dir = temp.path().join("generated");
        std::fs::create_dir_all(&gen_dir).unwrap();
        let faulty = std::sync::Arc::new(FaultyStore {
            inner: laser_runtime::MemoryStore::new(),
            mode: AtomicU8::new(0),
            remaining_ok_gets: AtomicU8::new(0),
        });
        let service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_store(faulty.clone())
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir])
        .with_transport_factory(stalling_transport_factory(0));

        let generated = post_json(
            &service,
            "/api/text-task/generate",
            &serde_json::json!({
                "text": "SAFE",
                "material": "椴木",
                "thickness_mm": 3.0
            }),
        );
        assert_eq!(generated["success"], true, "{generated}");
        let wf_id = generated["result"]["workflow_id"].as_str().unwrap();

        // confirm_send 在 launch() 前调用两次 store.get（初读 + 预约后二次复核），launch() 后调用第 3 次 store.get
        faulty.remaining_ok_gets.store(2, Ordering::SeqCst);
        faulty.mode.store(fail_mode, Ordering::SeqCst);

        let resp = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "confirm_send",
                "workflow_id": wf_id,
                "confirmed": true,
                "connection_mode": "network",
                "network_host": "127.0.0.1"
            }),
        );
        assert_eq!(resp["success"], false, "fail_mode={fail_mode}: {resp}");
        assert_eq!(
            resp["detail"]["error_code"], "workflow_persist_failed",
            "fail_mode={fail_mode}: {resp}"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !service.active_jobs.lock().unwrap().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "fail_mode={fail_mode}: active_jobs 未在取消后清空"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            service.uncertain_jobs.lock().unwrap().contains_key(wf_id),
            "fail_mode={fail_mode}: 被紧急中止的后台任务须记入待确认停机表"
        );
    }

    // auto_fallback=false 时仅探测指定连接方式，绝不回退打开另一类物理端口（防误触串口 DTR 复位）
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    );
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let no_fallback = post_json(
        &service,
        "/api/connection/check",
        &serde_json::json!({
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": closed,
            "port": "COM99",
            "network_timeout": 0.5,
            "auto_fallback": false
        }),
    );
    let attempts = no_fallback["result"]["attempts"].as_array().unwrap();
    assert_eq!(
        attempts.len(),
        1,
        "auto_fallback=false 不得额外探测串口: {no_fallback}"
    );
    assert_eq!(attempts[0]["mode"], "network");
}
