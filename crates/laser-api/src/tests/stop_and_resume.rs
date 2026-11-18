use super::*;

#[test]
fn unknown_stop_state_keeps_exact_transport_for_retry() {
    let fake = laser_device::FakeGrbl::new(Vec::<String>::new());
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(|_| {
        panic!("retry must not open another device")
    }));
    service.uncertain_jobs.lock().unwrap().insert(
        "lost-job".into(),
        UncertainJob::from_active(
            &ActiveJob {
                workflow_id: "lost-job".into(),
                mode: "network",
                target: "fake-a".into(),
                cancel: Default::default(),
                progress: Default::default(),
                transport: std::sync::Arc::new(fake.clone()),
                started_at: std::time::Instant::now(),
                spec: None,
            },
            None,
        ),
    );
    assert!(service.active_jobs.lock().unwrap().is_empty());
    let unknown = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"other"}),
    );
    assert_eq!(unknown["success"], false);
    assert!(fake.realtime().is_empty());
    let result = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"lost-job"}),
    );
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(fake.realtime(), vec![0x18]);
    let res =
        service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let text = String::from_utf8_lossy(&res);
    assert!(
        text.contains("lost-job") && text.contains("unknown"),
        "{text}"
    );
}

#[test]
fn emergency_stop_targets_only_requested_job_and_dry_run_never_writes() {
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(|_| {
        panic!("must reuse exact job transport")
    }));
    let devices: Vec<_> = (0..2)
        .map(|_| laser_device::FakeGrbl::new(Vec::<String>::new()))
        .collect();
    for (i, device) in devices.iter().enumerate() {
        service.active_jobs.lock().unwrap().insert(
            format!("job-{i}"),
            std::sync::Arc::new(ActiveJob {
                workflow_id: format!("job-{i}"),
                mode: "network",
                target: format!("127.0.0.1:{}", 2300 + i),
                cancel: Default::default(),
                progress: Default::default(),
                transport: std::sync::Arc::new(device.clone()),
                started_at: std::time::Instant::now(),
                spec: None,
            }),
        );
    }
    for args in [
        serde_json::json!({"action":"emergency_stop","confirmed":true}),
        serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"unknown"}),
        serde_json::json!({"action":"power_plus_10","job_id":"job-0"}),
        serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"job-0","host":"127.0.0.1","telnet_port":2301}),
    ] {
        assert_eq!(
            post_json(&service, "/api/safe-action", &args)["success"],
            false
        );
    }
    let dry = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action":"emergency_stop","job_id":"job-1","dry_run":true}),
    );
    assert_eq!(dry["success"], true);
    assert!(devices.iter().all(|d| d.realtime().is_empty()));
    let result = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action":"emergency_stop","job_id":"job-1","confirmed":true}),
    );
    assert_eq!(result["success"], true);
    assert!(devices[0].realtime().is_empty());
    assert_eq!(devices[1].realtime(), vec![0x18]);
}

#[test]
fn stop_targets_exposes_read_only_progress_for_the_global_panel() {
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(|_| panic!("只读查询不得打开设备连接")));
    let device = laser_device::FakeGrbl::new(Vec::<String>::new());
    let progress = std::sync::Arc::new(laser_device::JobProgress::default());
    progress
        .total_lines
        .store(100, std::sync::atomic::Ordering::Relaxed);
    progress
        .lines_sent
        .store(37, std::sync::atomic::Ordering::Relaxed);
    service.active_jobs.lock().unwrap().insert(
        "job-9".into(),
        std::sync::Arc::new(ActiveJob {
            workflow_id: "wf-9".into(),
            mode: "network",
            target: "127.0.0.1:2301".into(),
            cancel: Default::default(),
            progress: progress.clone(),
            transport: std::sync::Arc::new(device.clone()),
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );
    let res =
        service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let text = String::from_utf8_lossy(&res);
    assert!(
        text.contains("\"workflow_id\":\"wf-9\""),
        "浮层需要 workflow_id 才能取消或续雕: {text}"
    );
    assert!(
        text.contains("\"lines_sent\":37") && text.contains("\"total_lines\":100"),
        "浮层需要真实进度: {text}"
    );
    assert!(
        text.contains("\"phase\":\"streaming\"") && text.contains("\"state\":\"running\""),
        "{text}"
    );
    assert!(
        text.contains("\"elapsed_ms\"") && text.contains("\"mode\":\"network\""),
        "{text}"
    );
    assert!(device.realtime().is_empty(), "只读查询不得写实时通道");
}

#[test]
fn safe_action_route_overrides_running_job_without_reopening_transport() {
    let fake = laser_device::FakeGrbl::new(vec!["ok".into()]);
    let fake_clone = fake.clone();
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
    .with_transport_factory(std::sync::Arc::new(move |_| {
        Ok(Box::new(fake_clone.clone()))
    }));

    // 1. 模拟正在加工任务（注册进 active_jobs）
    let cancel = laser_device::CancellationToken::default();
    let progress = std::sync::Arc::new(laser_device::JobProgress::default());
    progress
        .total_lines
        .store(100, std::sync::atomic::Ordering::Relaxed);
    let active_job = std::sync::Arc::new(ActiveJob {
        workflow_id: "test-job-1".into(),
        started_at: std::time::Instant::now(),
        spec: None,
        target: "127.0.0.1:8080".into(),
        mode: "network",
        cancel: cancel.clone(),
        progress: progress.clone(),
        transport: std::sync::Arc::new(fake.clone()),
    });
    service
        .active_jobs
        .lock()
        .unwrap()
        .insert("test-job-1".into(), active_job);

    // 2. 边刻边调：下发进给速度增加 10%
    let feed_res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "feed_plus_10",
        }),
    );
    assert_eq!(feed_res["success"], true, "{feed_res}");
    assert!(feed_res["result"].as_str().unwrap().contains("不停机"));
    assert!(fake.realtime().contains(&0x91));

    // 3. 边刻边调：下发功率降低 10%
    let power_res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "power_minus_10",
        }),
    );
    assert_eq!(power_res["success"], true, "{power_res}");
    assert!(power_res["result"].as_str().unwrap().contains("不停机"));
    assert!(fake.realtime().contains(&0x9B));

    // 4. 正在加工时下发移动指令应当被安全拦截
    let move_res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "relative_move",
            "x": 10.0,
            "y": 0.0,
            "confirmed": true,
        }),
    );
    assert_eq!(move_res["success"], false);
    assert!(move_res["result"].as_str().unwrap().contains("禁止下发"));

    // 5. 正在加工时下发急停：任务被取消且收到 0x18
    let stop_res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "emergency_stop",
        }),
    );
    assert_eq!(stop_res["success"], true);
    assert!(cancel.is_cancelled());
    assert!(fake.realtime().contains(&0x18));
}

/// 加工中查询状态：走原作业通道写实时状态字节，并直接回报已下发行数（2026-11-02 补测）。
#[test]
fn status_during_active_job_reports_progress_without_ending_the_job() {
    use std::sync::atomic::Ordering;
    let fake = laser_device::FakeGrbl::new(vec!["ok".into()]);
    let fake_clone = fake.clone();
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
    .with_transport_factory(std::sync::Arc::new(move |_| {
        Ok(Box::new(fake_clone.clone()))
    }));
    let progress = std::sync::Arc::new(laser_device::JobProgress::default());
    progress.total_lines.store(342, Ordering::Relaxed);
    progress.lines_sent.store(120, Ordering::Relaxed);
    let job = std::sync::Arc::new(ActiveJob {
        workflow_id: "wf-status".into(),
        started_at: std::time::Instant::now(),
        spec: None,
        target: "127.0.0.1:8080".into(),
        mode: "network",
        cancel: laser_device::CancellationToken::default(),
        progress: progress.clone(),
        transport: std::sync::Arc::new(fake.clone()),
    });
    service
        .active_jobs
        .lock()
        .unwrap()
        .insert("wf-status".into(), job);

    let res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "status"}),
    );
    assert_eq!(res["success"], true, "{res}");
    let text = res["result"].as_str().unwrap_or_default();
    assert!(
        text.contains("设备正在加工中") && text.contains("120/342"),
        "加工中状态必须回报真实行数: {text}"
    );
    assert_eq!(res["detail"], "active_job_streaming");
    assert!(
        fake.realtime().contains(&0x3F),
        "加工中的状态查询必须经原作业通道写实时状态字节"
    );
    assert_eq!(service.active_job_count(), 1, "查询状态不能移除在途作业");
}

#[test]
fn job_worker_outcome_translates_device_errors_to_chinese_diagnostics() {
    let diag = laser_device::diagnose_device_message("error:20");
    assert!(diag.is_some());
    assert!(diag.unwrap().contains("不支持或无效的 G-code 指令"));

    let diag_alarm = laser_device::diagnose_device_message("ALARM:1");
    assert!(diag_alarm.is_some());
    assert!(diag_alarm.unwrap().contains("硬限位开关触发"));
}

#[test]
fn workflow_resume_action_safely_resumes_remaining_code() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();

    // 写入一个 5 行 G-code
    let gcode_content = "G21\nG90\nG0 X0 Y0\nG1 X10 Y10 F1000\nM5\n";
    let gcode_path = gen_dir.join("resume_test.gcode");
    std::fs::write(&gcode_path, gcode_content).unwrap();
    let expected_sha = hex::encode(sha2::Sha256::digest(gcode_content.as_bytes()));

    let written_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir.clone()])
    .with_transport_factory(recording_transport_factory(100, written_lines.clone()));

    // 1. 构造一个已取消且断点为 2/5 的 workflow 记录
    let wf_id = "test-resume-wf-001";
    let record = serde_json::json!({
        "workflow_id": wf_id,
        "status": "cancelled",
        "artifacts": {
            "gcode_file": "resume_test.gcode",
            "gcode_sha256": expected_sha,
        },
        "confirmation_snapshot": {
            "gcode_file": "resume_test.gcode",
            "gcode_sha256": expected_sha,
        },
        "send_result": {
            "success": false,
            "lines_sent": 2,
            "total_lines": 5,
            "can_resume": true
        }
    });
    service
        .workflows
        .lock()
        .unwrap()
        .insert(wf_id.into(), record);

    // 2. 调用 resume action
    let resume_req = serde_json::json!({
        "action": "resume",
        "workflow_id": wf_id,
        "confirmed": true,
        "connection_mode": "network",
        "network_host": "127.0.0.1",
        "network_telnet_port": 23
    });
    let res = post_json(&service, "/api/workflow", &resume_req);
    assert_eq!(res["success"], true, "{res}");
    assert_eq!(res["result"]["status"], "sending");
    assert_eq!(res["result"]["send_result"]["lines_sent"], 2);
    assert_eq!(res["result"]["send_result"]["total_lines"], 5);

    // 3. 等待后台续雕线程完成，并验证下发流中包含 $X 解锁命令以及剩余行
    let finished = wait_for_status(&service, wf_id, "completed");
    assert_eq!(finished["result"]["status"], "completed", "{finished}");
    let lines = written_lines.lock().unwrap().clone();
    assert!(
        lines.contains(&"$X".to_string()),
        "恢复首个命令必须下发 $X 解除锁定: {lines:?}"
    );
    assert_eq!(finished["result"]["send_result"]["lines_sent"], 5);
    assert_eq!(finished["result"]["send_result"]["total_lines"], 5);

    // 4. 反向边界：若任务已完成，必须 400 拦截
    let invalid_wf = "test-invalid-resume";
    service.workflows.lock().unwrap().insert(
        invalid_wf.into(),
        serde_json::json!({
            "workflow_id": invalid_wf,
            "status": "completed",
            "send_result": { "lines_sent": 5, "total_lines": 5 }
        }),
    );
    let bad_res = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume", "workflow_id": invalid_wf, "confirmed": true
        }),
    );
    assert_eq!(bad_res["success"], false);
    assert_eq!(bad_res["detail"]["error_code"], "workflow_completed");
}

/// 走边框用的假设备：实时状态固定为 `status`，记录写入的每一行。
fn status_transport_factory(
    status: &'static str,
    written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> TransportFactory {
    struct StatusGrbl {
        status: &'static str,
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl laser_device::DeviceTransport for StatusGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.written.lock().unwrap().push(line.to_owned());
            let mut responses = self.responses.lock().unwrap();
            if line == "$$" {
                responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else {
                responses.push_back("ok".into());
            }
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back(format!("<{}|MPos:0,0,0|FS:0,0>", self.status));
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }
    std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
        Ok(Box::new(StatusGrbl {
            status,
            responses: Default::default(),
            written: written.clone(),
        }) as Box<dyn laser_device::DeviceTransport>)
    })
}

/// 走边框默认不出光，勾选弱光才写 M4 S1；设备不在 Idle 时不运动（2026-10-14）。
#[test]
fn framing_defaults_to_laser_off_and_requires_idle_preflight() {
    let framing = |service: &ApiService, power: Option<i64>| {
        let mut body = serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "max_x": 20, "max_y": 10});
        if let Some(power) = power {
            body["power"] = power.into();
        }
        post_json(service, "/api/device/framing", &body)
    };
    let motion = |lines: &[String]| {
        lines
            .iter()
            .filter(|line| {
                line.starts_with("G0") || line.starts_with("G1") || line.starts_with("M4")
            })
            .cloned()
            .collect::<Vec<_>>()
    };

    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(status_transport_factory("Idle", written.clone()));
    let off = framing(&service, None);
    assert_eq!(off["success"], true, "{off}");
    assert_eq!(off["result"]["laser_on"], false);
    let lines = written.lock().unwrap().clone();
    assert!(
        !lines.iter().any(|line| line.starts_with("M4")),
        "默认不能出光: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("G1 X20")),
        "{lines:?}"
    );
    assert_eq!(service.active_job_count(), 0, "完成后不再占用设备");

    // 以激光头当前位置为原点（AI 预览窗“先走边框”）：先 G92 置零，范围可为负，最后回到起点
    written.lock().unwrap().clear();
    let relative = post_json(
        &service,
        "/api/device/framing",
        &serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "relative_to_head": true,
            "min_x": -15, "max_x": 15, "min_y": -5, "max_y": 5
        }),
    );
    assert_eq!(relative["success"], true, "{relative}");
    let lines = written.lock().unwrap().clone();
    let g92 = lines
        .iter()
        .position(|line| line.starts_with("G92 X0 Y0"))
        .expect("必须先把当前位置设为原点");
    let first_move = lines
        .iter()
        .position(|line| line.starts_with("G0") || line.starts_with("G1"))
        .unwrap();
    assert!(g92 < first_move, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("G0 X-15.000 Y-5.000")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .rfind(|line| line.starts_with("G0") || line.starts_with("G1"))
            .unwrap()
            .starts_with("G0 X0 Y0"),
        "{lines:?}"
    );

    written.lock().unwrap().clear();
    let weak = framing(&service, Some(1));
    assert_eq!(weak["result"]["laser_on"], true, "{weak}");
    assert!(
        written.lock().unwrap().iter().any(|line| line == "M4 S1"),
        "{:?}",
        written.lock().unwrap()
    );

    let alarm_written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let alarm = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(status_transport_factory("Alarm", alarm_written.clone()));
    let refused = framing(&alarm, Some(1));
    assert_eq!(
        refused["detail"]["error_code"], "device_preflight_failed",
        "{refused}"
    );
    assert!(
        motion(&alarm_written.lock().unwrap()).is_empty(),
        "预检失败不能运动: {:?}",
        alarm_written.lock().unwrap()
    );
}

/// 2026-11-16 橡皮筋走边框：带 `outline` 时沿多边形走（默认仍不出光），非法轮廓在打开设备前 400 拒绝。
#[test]
fn outline_framing_traces_the_polygon_and_rejects_bad_outlines_before_touching_the_device() {
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(status_transport_factory("Idle", written.clone()));
    let body = |outline: serde_json::Value| {
        serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "relative_to_head": true,
            "min_x": -10, "max_x": 10, "min_y": -10, "max_y": 5, "outline": outline
        })
    };
    let ok = post_json(
        &service,
        "/api/device/framing",
        &body(serde_json::json!([[-10, -10], [10, -10], [0, 5]])),
    );
    assert_eq!(ok["success"], true, "{ok}");
    let lines = written.lock().unwrap().clone();
    assert!(
        lines.iter().any(|l| l.starts_with("G0 X-10.000 Y-10.000")),
        "{lines:?}"
    );
    let lit_moves: Vec<&String> = lines.iter().filter(|l| l.starts_with("G1 ")).collect();
    assert_eq!(lit_moves.len(), 3, "三角形三条边：{lines:?}");
    assert!(lit_moves[1].starts_with("G1 X0.000 Y5.000"), "{lines:?}");
    assert!(!lines.iter().any(|l| l.starts_with("M4")), "默认不出光");

    for bad in [
        serde_json::json!([[0, 0], [1, 1]]),
        serde_json::json!([[0, 0], [50, 0], [0, 1]]),
        serde_json::json!([[0, 0], ["a", 0], [0, 1]]),
        serde_json::json!("triangle"),
    ] {
        written.lock().unwrap().clear();
        let refused = post_json(&service, "/api/device/framing", &body(bad.clone()));
        assert_eq!(
            refused["detail"]["error_code"], "framing_params_invalid",
            "{bad} → {refused}"
        );
        assert!(written.lock().unwrap().is_empty(), "非法轮廓不能碰设备");
    }
    assert!(
        crate::safe_action::framing_approval_summary(
            body(serde_json::json!([[-10, -10], [10, -10], [0, 5]]))
                .as_object()
                .unwrap()
        )
        .contains("沿图形外轮廓 3 个点")
    );
}

/// 走边框的 `power` / `feed_rate` 必须是范围内整数：溢出不能回绕（4294968096 曾变成 S800），
/// 越界与非整数在打开设备前 400 拒绝；合法值原样写进 G-code（2026-11-05）。
#[test]
fn framing_rejects_overflowing_or_out_of_range_power_and_feed() {
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(status_transport_factory("Idle", written.clone()));
    let framing = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "max_x": 20, "max_y": 10});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        post_json(&service, "/api/device/framing", &body)
    };
    let rejected = [
        serde_json::json!({"power": 4_294_968_096_u64}),
        serde_json::json!({"power": 1001}),
        serde_json::json!({"power": -1}),
        serde_json::json!({"power": 12.5}),
        serde_json::json!({"power": "800"}),
        serde_json::json!({"feed_rate": 4_294_969_296_u64}),
        serde_json::json!({"feed_rate": 0}),
        serde_json::json!({"feed_rate": 20_001}),
    ];
    for extra in rejected {
        let result = framing(extra.clone());
        assert_eq!(result["success"], false, "{extra}: {result}");
        assert_eq!(
            result["detail"]["error_code"], "framing_params_invalid",
            "{extra}: {result}"
        );
        assert!(
            written.lock().unwrap().is_empty(),
            "参数无效时不能连接或写设备: {extra}"
        );
    }

    let accepted = framing(serde_json::json!({"power": 1000, "feed_rate": 2500}));
    assert_eq!(accepted["success"], true, "{accepted}");
    let lines = written.lock().unwrap().clone();
    assert!(lines.iter().any(|line| line == "M4 S1000"), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("G1 X20") && line.ends_with("F2500")),
        "{lines:?}"
    );
}

/// 动作名大小写不敏感（与 laser-tools 一致）：运行中的作业收到 `Emergency_Stop` / `EMERGENCY_STOP`
/// 必须走原任务通道写出实时 0x18，不能落空或另开新连接（2026-11-05）。
#[test]
fn mixed_case_emergency_stop_during_running_job_writes_realtime_estop() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct GatedRecordingGrbl {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        realtime: std::sync::Arc<std::sync::Mutex<Vec<laser_device::RealtimeCommand>>>,
        gate: std::sync::Arc<AtomicBool>,
    }
    impl laser_device::DeviceTransport for GatedRecordingGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            if line.starts_with('$') {
                let mut responses = self.responses.lock().unwrap();
                if line == "$$" {
                    responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                } else {
                    responses.push_back("ok".into());
                }
                return Ok(());
            }
            while !self.gate.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err("测试结束：模拟连接断开".into())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
            } else {
                self.realtime.lock().unwrap().push(command);
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }
    let gate = std::sync::Arc::new(AtomicBool::new(false));
    let realtime = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let opens = std::sync::Arc::new(AtomicUsize::new(0));
    let factory: TransportFactory = {
        let (gate, realtime, opens) = (gate.clone(), realtime.clone(), opens.clone());
        std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
            opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(GatedRecordingGrbl {
                responses: Default::default(),
                realtime: realtime.clone(),
                gate: gate.clone(),
            }) as Box<dyn laser_device::DeviceTransport>)
        })
    };
    let service = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(factory),
    );
    let runner = {
        let service = service.clone();
        std::thread::spawn(move || {
            post_json(
                &service,
                "/api/device/framing",
                &serde_json::json!({
                    "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
                }),
            )
        })
    };
    let targets_request = b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let start = std::time::Instant::now();
    let job_id = loop {
        let targets = parse_json_response(&service.handle_http(targets_request));
        if let Some(id) = targets["result"]["jobs"][0]["job_id"].as_str() {
            break id.to_owned();
        }
        if runner.is_finished() || start.elapsed() > std::time::Duration::from_secs(10) {
            gate.store(true, Ordering::SeqCst);
            panic!("走边框没有登记为活动作业: {}", runner.join().unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(opens.load(Ordering::SeqCst), 1);

    let stop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "Emergency_Stop", "confirmed": true}),
    );
    assert_eq!(stop["success"], true, "{stop}");
    assert!(
        stop["result"].as_str().unwrap_or("").contains("原设备通道"),
        "{stop}"
    );
    assert_eq!(
        *realtime.lock().unwrap(),
        vec![laser_device::RealtimeCommand::EmergencyStop]
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "急停必须复用运行中任务的通道，不能另开连接"
    );

    let by_id = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": " EMERGENCY_STOP ", "confirmed": true, "job_id": job_id}),
    );
    assert_eq!(by_id["success"], true, "{by_id}");
    assert_eq!(
        realtime.lock().unwrap().len(),
        2,
        "按任务编号的大写急停也要写出 0x18"
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);

    gate.store(true, Ordering::SeqCst);
    let result = runner.join().unwrap();
    assert_eq!(
        result["detail"]["error_code"], "framing_cancelled",
        "{result}"
    );
}

/// 走边框运行中登记为活动作业：顶栏急停能找到并停止它，结束后保留为“待确认停止”（2026-10-14）。
#[test]
fn framing_is_listed_in_stop_targets_and_can_be_emergency_stopped() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let service = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(gated_transport_factory(gate.clone())),
    );
    let runner = {
        let service = service.clone();
        std::thread::spawn(move || {
            post_json(
                &service,
                "/api/device/framing",
                &serde_json::json!({
                    "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
                }),
            )
        })
    };
    let targets_request = b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let start = std::time::Instant::now();
    let job_id = loop {
        let targets = parse_json_response(&service.handle_http(targets_request));
        if let Some(id) = targets["result"]["jobs"][0]["job_id"].as_str() {
            assert_eq!(targets["result"]["jobs"][0]["state"], "running");
            break id.to_owned();
        }
        if runner.is_finished() || start.elapsed() > std::time::Duration::from_secs(5) {
            gate.store(true, std::sync::atomic::Ordering::SeqCst);
            panic!("走边框没有登记为活动作业: {}", runner.join().unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(job_id.starts_with("framing-"), "{job_id}");
    let busy = post_json(
        &service,
        "/api/device/framing",
        &serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
        }),
    );
    assert_eq!(busy["detail"]["error_code"], "device_busy", "{busy}");
    let stop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "emergency_stop", "confirmed": true, "job_id": job_id
        }),
    );
    assert_eq!(stop["success"], true, "{stop}");
    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = runner.join().unwrap();
    assert_eq!(
        result["detail"]["error_code"], "framing_cancelled",
        "{result}"
    );
    assert_eq!(service.active_job_count(), 0);
    let targets = parse_json_response(&service.handle_http(targets_request));
    assert_eq!(
        targets["result"]["jobs"][0]["state"], "unknown",
        "停止未确认，保留原通道供重试急停: {targets}"
    );
}

#[test]
fn exclusive_serial_transport_keeps_uncertain_for_voice_stop_and_releases_before_reopen() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct ExclusiveGrbl {
        port_in_use: std::sync::Arc<AtomicBool>,
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        realtime: std::sync::Arc<std::sync::Mutex<Vec<laser_device::RealtimeCommand>>>,
        block_lines: bool,
        gate: std::sync::Arc<AtomicBool>,
    }
    impl Drop for ExclusiveGrbl {
        fn drop(&mut self) {
            self.port_in_use.store(false, Ordering::SeqCst);
        }
    }
    impl laser_device::DeviceTransport for ExclusiveGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            if line.starts_with('$') {
                let mut responses = self.responses.lock().unwrap();
                if line == "$$" {
                    responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                } else {
                    responses.push_back("ok".into());
                }
                return Ok(());
            }
            if self.block_lines {
                while !self.gate.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                return Err("模拟传输中断".into());
            }
            self.responses.lock().unwrap().push_back("ok".into());
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            self.realtime.lock().unwrap().push(command);
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }

    let port_in_use = std::sync::Arc::new(AtomicBool::new(false));
    let gate = std::sync::Arc::new(AtomicBool::new(false));
    let first_run = std::sync::Arc::new(AtomicBool::new(true));
    let fail_open = std::sync::Arc::new(AtomicBool::new(false));
    let realtime_log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<
        laser_device::RealtimeCommand,
    >::new()));

    let port_flag = port_in_use.clone();
    let gate_flag = gate.clone();
    let first_flag = first_run.clone();
    let fail_flag = fail_open.clone();
    let rt_flag = realtime_log.clone();
    let factory: TransportFactory = std::sync::Arc::new(move |_spec| {
        if port_flag.load(Ordering::SeqCst) {
            return Err("Access is denied: 独占串口仍被旧句柄占用".into());
        }
        if fail_flag.load(Ordering::SeqCst) {
            return Err("串口设备暂时不可达".into());
        }
        port_flag.store(true, Ordering::SeqCst);
        let block_lines = first_flag.swap(false, Ordering::SeqCst);
        Ok(Box::new(ExclusiveGrbl {
            port_in_use: port_flag.clone(),
            responses: Default::default(),
            realtime: rt_flag.clone(),
            block_lines,
            gate: gate_flag.clone(),
        }))
    });

    let mut settings = test_loopback_settings();
    settings.default_connection_mode = "serial".into();
    let service = std::sync::Arc::new(
        ApiService::new(settings, laser_contracts::builtin_material_library())
            .with_serial_port_lister(std::sync::Arc::new(|| vec!["COM3".into()]))
            .with_transport_factory(factory),
    );

    // 1. 启动串口走边框并通过 /api/workflow cancel 取消，结束后进入 uncertain_jobs
    let srv = service.clone();
    let framing_thread = std::thread::spawn(move || {
        post_json(
            &srv,
            "/api/device/framing",
            &serde_json::json!({"connection_mode": "serial", "port": "COM3", "max_x": 10, "max_y": 10}),
        )
    });
    let start = std::time::Instant::now();
    let framing_id = loop {
        let targets = parse_json_response(
            &service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n"),
        );
        if let Some(id) = targets["result"]["jobs"][0]["job_id"].as_str() {
            break id.to_owned();
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "framing did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    // 验证加工期间 probe_connections 不打开冲突串口而直接回报连接正常
    let probe_busy = post_json(
        &service,
        "/api/connection/check",
        &serde_json::json!({"connection_mode": "serial", "port": "COM3"}),
    );
    assert_eq!(probe_busy["result"]["connected"], true, "{probe_busy}");

    // /api/workflow cancel 对走边框返回 200 而不是 404
    let cancelled = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": framing_id}),
    );
    assert_eq!(cancelled["success"], true, "{cancelled}");
    assert_eq!(cancelled["result"]["status"], "cancelled");

    gate.store(true, Ordering::SeqCst);
    let framing_res = framing_thread.join().unwrap();
    assert_eq!(
        framing_res["detail"]["error_code"], "framing_cancelled",
        "{framing_res}"
    );
    assert!(
        port_in_use.load(Ordering::SeqCst),
        "待确认停止期间保留原句柄供急停重试"
    );

    // 2. 语音快速急停（不传 job_id、不传串口）直接命中 uncertain_jobs 原句柄写出 0x18
    realtime_log.lock().unwrap().clear();
    let fast_stop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true}),
    );
    assert_eq!(fast_stop["success"], true, "{fast_stop}");
    assert!(
        realtime_log
            .lock()
            .unwrap()
            .contains(&laser_device::RealtimeCommand::EmergencyStop)
    );

    // 3. 重连或探测失败不得弄丢 uncertain_jobs：模拟端口打不开时先走重连，记录必须仍在 stop-targets
    fail_open.store(true, Ordering::SeqCst);
    let failed_framing = post_json(
        &service,
        "/api/device/framing",
        &serde_json::json!({"connection_mode": "serial", "port": "COM3", "max_x": 10, "max_y": 10}),
    );
    assert_eq!(failed_framing["success"], false, "{failed_framing}");
    assert!(
        !port_in_use.load(Ordering::SeqCst),
        "重连前旧串口句柄已被彻底释放（无残留 Arc）"
    );
    let targets_after_fail = parse_json_response(
        &service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(
        targets_after_fail["result"]["jobs"][0]["job_id"], framing_id,
        "重连失败时绝不能删除待确认停止记录: {targets_after_fail}"
    );
    assert_eq!(targets_after_fail["result"]["jobs"][0]["state"], "unknown");
    // 恢复串口可用后：无编号急停仍可通过保留的 spec 重开端口并发出 0x18
    fail_open.store(false, Ordering::SeqCst);
    realtime_log.lock().unwrap().clear();
    let retry_stop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true}),
    );
    assert_eq!(retry_stop["success"], true, "{retry_stop}");
    assert!(
        realtime_log
            .lock()
            .unwrap()
            .contains(&laser_device::RealtimeCommand::EmergencyStop)
    );

    // 4. 在同一串口成功启动并通过预检完成新任务后，旧 uncertain 记录自动清除且句柄正常释放
    let second = post_json(
        &service,
        "/api/device/framing",
        &serde_json::json!({"connection_mode": "serial", "port": "COM3", "max_x": 10, "max_y": 10}),
    );
    assert_eq!(second["success"], true, "{second}");
    assert!(!port_in_use.load(Ordering::SeqCst), "正常完成后句柄已释放");
    assert!(service.uncertain_jobs.lock().unwrap().is_empty());

    // 5. acknowledge-stop 必须同时携带非空 job_id 与 confirmed=true，拒绝缺省/false/无编号全量清空与语音调用方
    let dummy = laser_device::FakeGrbl::new(Vec::<String>::new());
    service.uncertain_jobs.lock().unwrap().insert(
        "uncertain-1".into(),
        UncertainJob::from_active(
            &ActiveJob {
                workflow_id: "uncertain-1".into(),
                mode: "serial",
                target: "COM3@115200".into(),
                cancel: Default::default(),
                progress: Default::default(),
                transport: std::sync::Arc::new(dummy),
                started_at: std::time::Instant::now(),
                spec: None,
            },
            None,
        ),
    );
    for invalid_body in [
        serde_json::json!({"job_id": "uncertain-1"}),
        serde_json::json!({"job_id": "uncertain-1", "confirmed": false}),
        serde_json::json!({"confirmed": true}),
    ] {
        let rejected = post_json(&service, "/api/jobs/acknowledge-stop", &invalid_body);
        assert_eq!(
            rejected["success"], false,
            "invalid body must be rejected: {invalid_body}"
        );
        assert!(!service.uncertain_jobs.lock().unwrap().is_empty());
    }
    for denied_ctx in [
        laser_contracts::CallerContext::McpBridge,
        laser_contracts::CallerContext::Voice {
            device_id: "dev-1".into(),
            session_id: "sess-1".into(),
        },
    ] {
        let ack_body = r#"{"job_id":"uncertain-1","confirmed":true}"#;
        let raw_req = format!(
            "POST /api/jobs/acknowledge-stop HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{ack_body}",
            ack_body.len()
        );
        let raw_denied = service.handle_http_with_ctx(raw_req.as_bytes(), denied_ctx);
        assert!(String::from_utf8_lossy(&raw_denied).starts_with("HTTP/1.1 403"));
        assert!(!service.uncertain_jobs.lock().unwrap().is_empty());
    }

    let ack = post_json(
        &service,
        "/api/jobs/acknowledge-stop",
        &serde_json::json!({"job_id": "uncertain-1", "confirmed": true}),
    );
    assert_eq!(ack["success"], true, "{ack}");
    assert_eq!(ack["result"]["acknowledged"], true);
    assert!(service.uncertain_jobs.lock().unwrap().is_empty());
}

#[test]
fn workflow_resume_requires_confirmed_true_and_verifies_physical_gcode_sha() {
    for use_store in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let gen_dir = temp.path().join("generated");
        std::fs::create_dir_all(&gen_dir).unwrap();
        let gcode_content = "G21\nG90\nG0 X0 Y0\nG1 X10 Y10 F1000\nM5\n";
        let gcode_path = gen_dir.join("resume_gate.gcode");
        std::fs::write(&gcode_path, gcode_content).unwrap();
        let expected_sha = hex::encode(Sha256::digest(gcode_content.as_bytes()));

        let mut service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir.clone()])
        .with_transport_factory(fake_transport_factory(100));
        let wf_id = "wf-resume-gate";
        let payload = serde_json::json!({
            "workflow_id": wf_id,
            "status": "cancelled",
            "artifacts": { "gcode_file": "resume_gate.gcode", "gcode_sha256": expected_sha },
            "confirmation_snapshot": { "gcode_file": "resume_gate.gcode", "gcode_sha256": expected_sha },
            "send_result": { "success": false, "lines_sent": 2, "total_lines": 5, "can_resume": true }
        });
        if use_store {
            let db = std::sync::Arc::new(std::sync::Mutex::new(
                laser_runtime::RuntimeDb::open_memory().unwrap(),
            ));
            use laser_runtime::RuntimeStore;
            db.put(&laser_runtime::StoredRecord {
                kind: laser_runtime::RecordKind::Workflow,
                id: wf_id.into(),
                status: Some("cancelled".into()),
                payload,
            })
            .unwrap();
            service = service.with_runtime_db(db);
        } else {
            service
                .workflows
                .lock()
                .unwrap()
                .insert(wf_id.into(), payload);
        }

        // 未传 confirmed（缺省 false）必须 400 拒绝
        let raw_unconfirmed = service.handle_http(
            format!(
                "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                r#"{"action":"resume","workflow_id":"wf-resume-gate"}"#.len(),
                r#"{"action":"resume","workflow_id":"wf-resume-gate"}"#
            )
            .as_bytes(),
        );
        assert!(
            String::from_utf8_lossy(&raw_unconfirmed).starts_with("HTTP/1.1 400"),
            "{use_store}"
        );
        assert_eq!(parse_json_response(&raw_unconfirmed)["success"], false);

        // 合法续雕（文件未改 + confirmed=true）：必须通过门禁并从断点行启动
        let valid_resume = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "resume",
                "workflow_id": wf_id,
                "confirmed": true,
                "connection_mode": "network",
                "network_host": "127.0.0.1"
            }),
        );
        assert_eq!(valid_resume["success"], true, "{valid_resume}");
        assert_eq!(
            valid_resume["result"]["send_result"]["resumed"], true,
            "{valid_resume}"
        );
        let wait_start = std::time::Instant::now();
        while !service.active_jobs.lock().unwrap().is_empty() {
            assert!(wait_start.elapsed() < std::time::Duration::from_secs(3));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // 重新置回 cancelled 后篡改落盘刀路：即使 confirmed=true 也必须被 authorize_send 拦截
        let payload_again = serde_json::json!({
            "workflow_id": wf_id,
            "status": "cancelled",
            "artifacts": { "gcode_file": "resume_gate.gcode", "gcode_sha256": expected_sha },
            "confirmation_snapshot": { "gcode_file": "resume_gate.gcode", "gcode_sha256": expected_sha },
            "send_result": { "success": false, "lines_sent": 2, "total_lines": 5, "can_resume": true }
        });
        if let Some(store) = service.store.as_ref() {
            store
                .put(&laser_runtime::StoredRecord {
                    kind: laser_runtime::RecordKind::Workflow,
                    id: wf_id.into(),
                    status: Some("cancelled".into()),
                    payload: payload_again,
                })
                .unwrap();
        } else {
            service
                .workflows
                .lock()
                .unwrap()
                .insert(wf_id.into(), payload_again);
        }
        std::fs::write(
            &gcode_path,
            "G21\nG90\nG0 X999 Y999\nG1 X10 Y10 F1000\nM5\n",
        )
        .unwrap();
        let tampered = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "resume",
                "workflow_id": wf_id,
                "confirmed": true,
                "connection_mode": "network",
                "network_host": "127.0.0.1"
            }),
        );
        assert_eq!(tampered["success"], false, "{tampered}");
        assert_eq!(tampered["detail"]["error_code"], "send_gate_rejected");
    }
}

#[test]
fn resume_reconstructs_g92_offset_modals_and_rejects_sd_offline() {
    let prefix = vec![
        "G21".to_owned(),
        "G90".to_owned(),
        "M5".to_owned(),
        "G92 X-15.000 Y-5.000".to_owned(),
        "M4 S0".to_owned(),
        "G0 X-15.000 Y-5.000 F3000".to_owned(),
        "G1 X15.000 Y-5.000 F1200 S450".to_owned(),
    ];
    // 开工时 MPos=(100,50,0) 处执行了 G92 X-15 Y-5；急停减速后停在 MPos=(132,53,0)，0x18 清空了 G92/F/M4
    let plan = build_resume_plan(&prefix, Some((100.0, 50.0, 0.0)), Some((132.0, 53.0, 0.0)));
    assert_eq!(plan.preamble[0], "M5 S0");
    assert!(
        plan.preamble.contains(&"G92 X17.000 Y-2.000".to_owned()),
        "必须根据 origin_mpos 与 current_mpos 精确还原 G92 偏移: {:?}",
        plan.preamble
    );
    assert!(
        plan.preamble.contains(&"G0 X15.000 Y-5.000".to_owned()),
        "必须在激光关闭态快速回到断点期望坐标: {:?}",
        plan.preamble
    );
    assert!(
        plan.preamble.contains(&"G1 F1200.000 S0".to_owned()),
        "必须恢复切割进给率 F1200 以避免 error:22: {:?}",
        plan.preamble
    );
    assert!(
        plan.preamble.contains(&"M4 S0".to_owned()),
        "必须以零功率恢复 M4 动态激光模式: {:?}",
        plan.preamble
    );

    let mut remaining = vec![
        "G1 X15.000 Y5.000".to_owned(),
        "G1 X-15.000 Y5.000 S300".to_owned(),
    ];
    apply_modal_spindle_to_remaining(
        &mut remaining,
        plan.modal_spindle_s,
        plan.resume_in_cut_motion,
    );
    assert_eq!(remaining[0], "G1 X15.000 Y5.000 S450");
    assert_eq!(remaining[1], "G1 X-15.000 Y5.000 S300");

    // 纯 G91 增量刀路（无 G90/G92）：急停减速滑行后必须按物理位移差 G91 G0 纠偏再恢复 G91 模态
    let g91_prefix = vec![
        "G21".to_owned(),
        "G91".to_owned(),
        "M4 S0".to_owned(),
        "G1 X10.000 Y5.000 F1200 S500".to_owned(),
    ];
    let g91_plan = build_resume_plan(
        &g91_prefix,
        Some((100.0, 200.0, 0.0)),
        Some((112.5, 204.0, 0.0)),
    );
    assert!(
        g91_plan.preamble.contains(&"G0 X-2.500 Y1.000".to_owned()),
        "纯 G91 刀路应根据 MPos 物理差输出增量预定位: {:?}",
        g91_plan.preamble
    );
    assert_eq!(g91_plan.preamble.last().map(String::as_str), Some("G91"));

    // SD 脱机作业记录必须拒绝按行断点续雕
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_preview_roots(vec![temp.path().to_path_buf()]);
    service.workflows.lock().unwrap().insert(
        "wf-sd-no-resume".into(),
        serde_json::json!({
            "workflow_id": "wf-sd-no-resume",
            "status": "cancelled",
            "send_result": {
                "mode": "sd_offline",
                "can_resume": false,
                "lines_sent": 10,
                "total_lines": 50
            }
        }),
    );
    let rejected = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume",
            "workflow_id": "wf-sd-no-resume",
            "confirmed": true
        }),
    );
    assert_eq!(rejected["success"], false);
    assert_eq!(rejected["detail"]["error_code"], "workflow_not_resumable");
}

/// 走边框途中暂停 / 继续 / 停止（2026-11-18，技术书 r52）：暂停发 `!` 并在停止目标里显示 paused；
/// 继续必须 confirmed=true 才发 `~`；停止走边框取消作业并先进给保持；正式加工、没有边框时拒绝。
#[test]
fn framing_pause_resume_and_stop_only_touch_the_framing_job() {
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(|_| {
        panic!("必须复用走边框作业的原通道")
    }));
    let safe = |args: serde_json::Value| post_json(&service, "/api/safe-action", &args);
    let none = safe(serde_json::json!({"action": "pause"}));
    assert_eq!(none["detail"], "no_active_framing", "{none}");

    let device = laser_device::FakeGrbl::new(Vec::<String>::new());
    let insert = |id: &str| {
        service.active_jobs.lock().unwrap().insert(
            id.into(),
            std::sync::Arc::new(ActiveJob {
                workflow_id: id.into(),
                mode: "network",
                target: "127.0.0.1:23".into(),
                cancel: Default::default(),
                progress: Default::default(),
                transport: std::sync::Arc::new(device.clone()),
                started_at: std::time::Instant::now(),
                spec: None,
            }),
        )
    };
    insert("wf-engrave");
    let engraving = safe(serde_json::json!({"action": "pause"}));
    assert_eq!(
        engraving["detail"], "framing_control_not_supported",
        "{engraving}"
    );
    assert!(device.realtime().is_empty(), "正式加工不能被暂停");
    service.active_jobs.lock().unwrap().clear();

    insert("framing-1");
    let paused = safe(serde_json::json!({"action": "pause"}));
    assert_eq!(paused["success"], true, "{paused}");
    assert_eq!(device.realtime(), vec![b'!']);
    let targets = parse_json_response(
        &service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(targets["result"]["jobs"][0]["paused"], true, "{targets}");

    let unconfirmed = safe(serde_json::json!({"action": "resume"}));
    assert_eq!(
        unconfirmed["detail"]["error_code"], "confirmation_required",
        "{unconfirmed}"
    );
    assert_eq!(device.realtime(), vec![b'!'], "未确认不能继续");
    let resumed = safe(serde_json::json!({"action": "resume", "confirmed": true}));
    assert_eq!(resumed["success"], true, "{resumed}");
    assert_eq!(device.realtime(), vec![b'!', b'~']);

    let stopped = safe(serde_json::json!({"action": "stop_framing", "job_id": "framing-1"}));
    assert_eq!(stopped["success"], true, "{stopped}");
    let job = service.active_jobs.lock().unwrap()["framing-1"].clone();
    assert!(job.cancel.is_cancelled(), "停止走边框要取消作业");
    assert_eq!(
        device.realtime(),
        vec![b'!', b'~', b'!'],
        "先进给保持再由作业线程复位"
    );
    assert!(
        !laser_contracts::safe_action_requires_approval("pause")
            && !laser_contracts::safe_action_requires_approval("stop_framing")
            && laser_contracts::safe_action_requires_approval("resume")
    );
}

/// 真实走边框途中“停止走边框”：作业线程按取消收尾，返回 framing_cancelled，转为待确认停止。
#[test]
fn stop_framing_cancels_a_running_framing_job() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let service = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(gated_transport_factory(gate.clone())),
    );
    let runner = {
        let service = service.clone();
        std::thread::spawn(move || {
            post_json(
                &service,
                "/api/device/framing",
                &serde_json::json!({
                    "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
                }),
            )
        })
    };
    let start = std::time::Instant::now();
    while service.active_framing_job_ids().is_empty() {
        if runner.is_finished() || start.elapsed() > std::time::Duration::from_secs(5) {
            gate.store(true, std::sync::atomic::Ordering::SeqCst);
            panic!("走边框没有开始: {}", runner.join().unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let stop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "stop_framing"}),
    );
    assert_eq!(stop["success"], true, "{stop}");
    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = runner.join().unwrap();
    assert_eq!(
        result["detail"]["error_code"], "framing_cancelled",
        "{result}"
    );
    assert_eq!(service.active_job_count(), 0);
}
