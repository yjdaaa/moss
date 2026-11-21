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
            "target": "127.0.0.1:23",
            "lines_sent": 2,
            "lines_completed": 2,
            "resumable_from_line": 2,
            "resume_boundary_source": "device_hold_ln",
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

#[test]
fn cancelling_a_resumed_job_without_new_execution_proof_cannot_resume_again() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    struct NoExecutionProofGrbl {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        resumed_laser_line_seen: Arc<AtomicBool>,
    }
    impl laser_device::DeviceTransport for NoExecutionProofGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            let mut responses = self.responses.lock().unwrap();
            if line == "$$" {
                responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else if !self.resumed_laser_line_seen.load(Ordering::Acquire) {
                if line.starts_with("G1 X20") {
                    self.resumed_laser_line_seen.store(true, Ordering::Release);
                } else {
                    responses.push_back("ok".into());
                }
            }
            Ok(())
        }

        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Idle|MPos:0,0,0>".into());
            }
            Ok(())
        }

        fn read_line(&self) -> Result<Option<String>, String> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .map(Some)
                .ok_or_else(|| "timed out".into())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let gcode = "G21\nG90\nG0 X0 Y0\nG1 X20 F1000\nM5\n";
    std::fs::write(dir.path().join("resume_twice.gcode"), gcode).unwrap();
    let sha = hex::encode(sha2::Sha256::digest(gcode.as_bytes()));
    let seen = Arc::new(AtomicBool::new(false));
    let factory_seen = seen.clone();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(dir.path().to_path_buf())
    .with_preview_roots(vec![dir.path().to_path_buf()])
    .with_transport_factory(Arc::new(move |_| {
        Ok(Box::new(NoExecutionProofGrbl {
            responses: Default::default(),
            resumed_laser_line_seen: factory_seen.clone(),
        }))
    }));
    let id = "resume-twice";
    service.workflows.lock().unwrap().insert(
        id.into(),
        serde_json::json!({
            "workflow_id": id,
            "status": "cancelled",
            "artifacts": { "gcode_file": "resume_twice.gcode", "gcode_sha256": sha },
            "confirmation_snapshot": { "gcode_file": "resume_twice.gcode", "gcode_sha256": sha },
            "send_result": {
                "target": "127.0.0.1:23", "lines_sent": 2, "lines_completed": 2,
                "resumable_from_line": 2, "resume_boundary_source": "device_hold_ln",
                "total_lines": 5, "can_resume": true
            }
        }),
    );
    let resume = serde_json::json!({
        "action": "resume", "workflow_id": id, "confirmed": true,
        "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23
    });
    let started = post_json(&service, "/api/workflow", &resume);
    assert_eq!(started["success"], true, "{started}");

    let deadline = Instant::now() + Duration::from_secs(2);
    while !seen.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "续雕应开始下发剩余刀路");
        std::thread::sleep(Duration::from_millis(5));
    }
    let cancelled = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "cancel", "workflow_id": id
        }),
    );
    assert_eq!(cancelled["success"], true, "{cancelled}");

    let deadline = Instant::now() + Duration::from_secs(2);
    while service.active_job_count() != 0 {
        assert!(Instant::now() < deadline, "取消后作业应收尾");
        std::thread::sleep(Duration::from_millis(5));
    }
    let terminal = wait_for_status(&service, id, "cancelled");
    let sent = &terminal["result"]["send_result"];
    assert_eq!(sent["can_resume"], false, "{terminal}");
    assert_eq!(sent["resumable_from_line"], 0, "{terminal}");
    assert_eq!(sent["resume_boundary_source"], "unproven", "{terminal}");
    let rejected = post_json(&service, "/api/workflow", &resume);
    assert_eq!(
        rejected["detail"]["error_code"], "workflow_not_resumable",
        "{rejected}"
    );
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
            Ok(())
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
                return Ok(());
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
            "send_result": { "success": false, "target": "tcp://127.0.0.1:23", "lines_sent": 2, "lines_completed": 2, "resumable_from_line": 2, "resume_boundary_source": "device_hold_ln", "total_lines": 5, "can_resume": true }
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
            "send_result": { "success": false, "target": "tcp://127.0.0.1:23", "lines_sent": 2, "lines_completed": 2, "resumable_from_line": 2, "resume_boundary_source": "device_hold_ln", "total_lines": 5, "can_resume": true }
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

#[test]
fn resume_target_equivalence_blocks_cross_machine_and_tampered_resume_skips_unlock() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let gcode = "G21\nG90\nG0 X0 Y0\nG1 X10 Y10 F1000\nM5\n";
    let gcode_path = gen_dir.join("dual_target.gcode");
    std::fs::write(&gcode_path, gcode).unwrap();
    let expected_sha = hex::encode(sha2::Sha256::digest(gcode.as_bytes()));

    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir.clone()])
    .with_transport_factory(recording_transport_factory(100, written.clone()));

    let insert_wf = |id: &str, target: &str| {
        service.workflows.lock().unwrap().insert(
            id.into(),
            serde_json::json!({
                "workflow_id": id,
                "status": "cancelled",
                "artifacts": { "gcode_file": "dual_target.gcode", "gcode_sha256": expected_sha },
                "confirmation_snapshot": { "gcode_file": "dual_target.gcode", "gcode_sha256": expected_sha },
                "send_result": {
                    "success": false,
                    "target": target,
                    "lines_sent": 4,
                    "lines_completed": 2,
                    "resumable_from_line": 2,
                    "resume_boundary_source": "device_hold_ln",
                    "total_lines": 5,
                    "can_resume": true
                }
            }),
        );
    };

    // 1. TCP 等价历史串 tcp://127.0.0.1:23 与规范串 127.0.0.1:23 互通，且仅从 resumable_from_line=2 续刻（不用 ACK=4）
    insert_wf("wf-tcp-legacy", "tcp://127.0.0.1:23");
    let ok_tcp = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume",
            "workflow_id": "wf-tcp-legacy",
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": 23
        }),
    );
    assert_eq!(ok_tcp["success"], true, "{ok_tcp}");
    assert_eq!(ok_tcp["result"]["send_result"]["lines_sent"], 2);
    wait_for_status(&service, "wf-tcp-legacy", "completed");

    // 2. TCP 换主机、换端口或无意义 target 必须被拦截
    insert_wf("wf-tcp-mismatch", "127.0.0.1:23");
    for (host, port) in [("127.0.0.2", 23), ("127.0.0.1", 24)] {
        let bad = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "resume",
                "workflow_id": "wf-tcp-mismatch",
                "confirmed": true,
                "connection_mode": "network",
                "network_host": host,
                "network_telnet_port": port
            }),
        );
        assert_eq!(bad["success"], false, "{host}:{port}");
        assert_eq!(bad["detail"]["error_code"], "workflow_not_resumable");
    }

    // 3. 串口双目标兼容：serial:COM3:115200 匹配 com3@115200，但拒绝 COM4 或 9600 波特率
    insert_wf("wf-serial-equiv", "serial:COM3:115200");
    let ok_serial = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume",
            "workflow_id": "wf-serial-equiv",
            "confirmed": true,
            "connection_mode": "serial",
            "port": "com3",
            "baudrate": 115200
        }),
    );
    assert_eq!(ok_serial["success"], true, "{ok_serial}");
    wait_for_status(&service, "wf-serial-equiv", "completed");

    insert_wf("wf-serial-bad", "COM3@115200");
    for (port, baud) in [("COM4", 115200), ("COM3", 9600)] {
        let bad = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "resume",
                "workflow_id": "wf-serial-bad",
                "confirmed": true,
                "connection_mode": "serial",
                "port": port,
                "baudrate": baud
            }),
        );
        assert_eq!(bad["success"], false, "{port}@{baud}");
        assert_eq!(bad["detail"]["error_code"], "workflow_not_resumable");
    }

    insert_wf("wf-garbage-target", "invalid://garbage");
    let bad_garbage = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume",
            "workflow_id": "wf-garbage-target",
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1"
        }),
    );
    assert_eq!(
        bad_garbage["detail"]["error_code"],
        "workflow_not_resumable"
    );

    // 3b. 缺失安全来源标记 resume_boundary_source=device_hold_ln 的旧版 ACK-only 记录必须拒绝续雕
    service.workflows.lock().unwrap().insert(
        "wf-legacy-ack-only".into(),
        serde_json::json!({
            "workflow_id": "wf-legacy-ack-only",
            "status": "cancelled",
            "artifacts": { "gcode_file": "dual_target.gcode", "gcode_sha256": expected_sha },
            "confirmation_snapshot": { "gcode_file": "dual_target.gcode", "gcode_sha256": expected_sha },
            "send_result": {
                "success": false,
                "target": "127.0.0.1:23",
                "lines_sent": 3,
                "resumable_from_line": 3,
                "total_lines": 5,
                "can_resume": true
            }
        }),
    );
    let legacy_rejected = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume",
            "workflow_id": "wf-legacy-ack-only",
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1"
        }),
    );
    assert_eq!(legacy_rejected["success"], false);
    assert_eq!(
        legacy_rejected["detail"]["error_code"],
        "workflow_not_resumable"
    );

    // 4. 建连期内磁盘 G-code 被篡改（TOCTOU）必须在发送 $X 解锁前被拒
    let toctou_written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let base_toctou = recording_transport_factory(100, toctou_written.clone());
    let gcode_path_clone = gcode_path.clone();
    let toctou_service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(gen_dir)
    .with_preview_roots(vec![temp.path().join("generated")])
    .with_transport_factory(std::sync::Arc::new(move |spec| {
        std::fs::write(&gcode_path_clone, "G21\nG90\nG0 X99 Y99\n").unwrap();
        base_toctou(spec)
    }));
    toctou_service.workflows.lock().unwrap().insert(
        "wf-resume-toctou".into(),
        serde_json::json!({
            "workflow_id": "wf-resume-toctou",
            "status": "cancelled",
            "artifacts": { "gcode_file": "dual_target.gcode", "gcode_sha256": expected_sha },
            "confirmation_snapshot": { "gcode_file": "dual_target.gcode", "gcode_sha256": expected_sha },
            "send_result": {
                "success": false,
                "target": "127.0.0.1:23",
                "lines_sent": 2,
                "lines_completed": 2,
                "resumable_from_line": 2,
                "resume_boundary_source": "device_hold_ln",
                "total_lines": 5,
                "can_resume": true
            }
        }),
    );
    let toctou_res = post_json(
        &toctou_service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume",
            "workflow_id": "wf-resume-toctou",
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1"
        }),
    );
    assert_eq!(toctou_res["detail"]["error_code"], "send_gate_rejected");
    assert!(
        !toctou_written.lock().unwrap().iter().any(|l| l == "$X"),
        "篡改刀路绝不能提前触发 $X 解锁: {:?}",
        *toctou_written.lock().unwrap()
    );
}

#[test]
fn missing_frozen_sha_and_preview_id_collision_are_blocked() {
    for use_db in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let gen_dir = temp.path().join("generated");
        std::fs::create_dir_all(&gen_dir).unwrap();
        std::fs::write(gen_dir.join("old.gcode"), "G21\nG0 X1 Y1\nM5\n").unwrap();

        let mut service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir]);
        if use_db {
            let db = std::sync::Arc::new(std::sync::Mutex::new(
                laser_runtime::RuntimeDb::open_memory().unwrap(),
            ));
            service = service.with_runtime_db(db);
        }

        // 1. 缺冻结 SHA 的老记录不得在 confirm_send 当刻现场补算放行
        let legacy = serde_json::json!({
            "workflow_id": "wf-legacy-no-sha",
            "status": "preview_ready",
            "artifacts": { "gcode_file": "old.gcode", "gcode_sha256": "" },
            "confirmation_snapshot": { "gcode_file": "old.gcode", "gcode_sha256": "" }
        });
        service
            .with_effective_store(|store| {
                store.put(&laser_runtime::StoredRecord {
                    kind: laser_runtime::RecordKind::Workflow,
                    id: "wf-legacy-no-sha".into(),
                    status: Some("preview_ready".into()),
                    payload: legacy.clone(),
                })
            })
            .unwrap();
        let rejected_send = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "confirm_send",
                "workflow_id": "wf-legacy-no-sha",
                "confirmed": true
            }),
        );
        assert_eq!(rejected_send["success"], false, "use_db={use_db}");
        assert_eq!(
            rejected_send["detail"]["error_code"], "confirmation_snapshot_missing",
            "use_db={use_db}: {rejected_send}"
        );

        // 2. preview 传冲突的 workflow_id 绝不能覆盖现有/在途记录
        let collision = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "preview",
                "workflow_id": "wf-legacy-no-sha",
                "source_type": "gcode",
                "gcode_file": "old.gcode",
                "material": "椴木",
                "thickness_mm": 3
            }),
        );
        assert_eq!(collision["success"], false, "use_db={use_db}: {collision}");
        assert_eq!(
            collision["detail"]["error_code"], "workflow_id_conflict",
            "use_db={use_db}: {collision}"
        );
    }
}

#[test]
fn preflight_registers_framing_for_abort_and_shutdown_rejects_new_requests() {
    // 1. framing 在 handshake 阶段即进入 active_jobs，期间触发关停清点与急停可实时中止并拒接后续请求
    let handshake_entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_seen_by_device = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let resume_seen_by_device = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct BlockingHandshakeTransport {
        entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
        estop_seen: std::sync::Arc<std::sync::atomic::AtomicBool>,
        resume_seen: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl laser_device::DeviceTransport for BlockingHandshakeTransport {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            self.entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::EmergencyStop {
                self.estop_seen
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            if command == laser_device::RealtimeCommand::Resume {
                self.resume_seen
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            std::thread::sleep(std::time::Duration::from_millis(20));
            Err("timed out".into())
        }
    }
    let he = handshake_entered.clone();
    let es = cancel_seen_by_device.clone();
    let rs = resume_seen_by_device.clone();
    let service = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(std::sync::Arc::new(move |_| {
            Ok(Box::new(BlockingHandshakeTransport {
                entered: he.clone(),
                estop_seen: es.clone(),
                resume_seen: rs.clone(),
            }))
        })),
    );
    let svc_for_thread = service.clone();
    let framing_thread = std::thread::spawn(move || {
        post_json(
            &svc_for_thread,
            "/api/device/framing",
            &serde_json::json!({
                "connection_mode": "network",
                "network_host": "127.0.0.1",
                "max_x": 10,
                "max_y": 10
            }),
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !handshake_entered.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        service.active_job_count(),
        1,
        "走边框在 $$ 握手未结束前就必须进入 active_jobs"
    );
    let resume = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "resume", "confirmed": true}),
    );
    assert_eq!(resume["success"], false, "{resume}");
    assert!(!resume_seen_by_device.load(std::sync::atomic::Ordering::SeqCst));

    service.request_shutdown();
    service.abort_all_active_jobs().unwrap();
    let framing_res = framing_thread.join().unwrap();
    assert_eq!(framing_res["success"], false);
    assert!(cancel_seen_by_device.load(std::sync::atomic::Ordering::SeqCst));

    // 2. 置位关停门禁后，新的走边框与安全动作必须返回 host_shutting_down 且不得再调用 transport_factory
    let denied = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "home", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
    );
    assert_eq!(denied["success"], false);
    assert_eq!(denied["detail"]["error_code"], "host_shutting_down");

    let denied_framing = post_json(
        &service,
        "/api/device/framing",
        &serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10}),
    );
    assert_eq!(denied_framing["success"], false);
    assert_eq!(denied_framing["detail"]["error_code"], "host_shutting_down");
}

#[test]
fn cancel_path_error_reports_failed_with_fallback_0x18_and_safe_home_registers_active() {
    // 1. 取消期间如果 device.cancel() 自身失败，send 绝不能伪报安全取消，必须标 failed/can_resume=false 并补发 0x18
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let gcode = "G21\nG90\nG1 X10 F1000\nG1 X20\nM5\n";
    std::fs::write(gen_dir.join("err_cancel.gcode"), gcode).unwrap();
    let sha = hex::encode(sha2::Sha256::digest(gcode.as_bytes()));

    #[derive(Clone)]
    struct CancelHoldFailsTransport {
        responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
        fallback_estop_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl laser_device::DeviceTransport for CancelHoldFailsTransport {
        fn write_line(&self, line: &str) -> Result<(), String> {
            let mut r = self.responses.lock().unwrap();
            if line == "$$" {
                r.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else if line == "$CD" {
                r.push_back("ok".into());
            } else {
                // 让发送行在阻塞读中等待外部 cancel
                std::thread::sleep(std::time::Duration::from_millis(80));
                r.push_back("ok".into());
            }
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Idle|MPos:0,0,0>".into());
            } else if command == laser_device::RealtimeCommand::FeedHold {
                return Err("feedhold write broken".into());
            } else if command == laser_device::RealtimeCommand::EmergencyStop {
                self.fallback_estop_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }

    let estops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let estops_clone = estops.clone();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir])
    .with_transport_factory(std::sync::Arc::new(move |_| {
        Ok(Box::new(CancelHoldFailsTransport {
            responses: Default::default(),
            fallback_estop_count: estops_clone.clone(),
        }))
    }));
    service.workflows.lock().unwrap().insert(
        "wf-cancel-err".into(),
        serde_json::json!({
            "workflow_id": "wf-cancel-err",
            "status": "preview_ready",
            "artifacts": { "gcode_file": "err_cancel.gcode", "gcode_sha256": sha },
            "confirmation_snapshot": { "gcode_file": "err_cancel.gcode", "gcode_sha256": sha },
        }),
    );
    let started = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "confirm_send",
            "workflow_id": "wf-cancel-err",
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1"
        }),
    );
    assert_eq!(started["success"], true, "{started}");
    let _ = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": "wf-cancel-err"}),
    );
    let failed = wait_for_status(&service, "wf-cancel-err", "failed");
    assert_eq!(failed["result"]["status"], "failed", "{failed}");
    assert_eq!(
        failed["result"]["send_result"]["can_resume"], false,
        "{failed}"
    );
    assert!(
        failed["result"]["send_result"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("feedhold write broken"),
        "{failed}"
    );
    assert!(
        estops.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "取消路径报错分支也必须补发急停 0x18 兜底"
    );

    // 2. 独立 /api/safe-action home 执行期间必须登记进 active_jobs 并可被 abort_all_active_jobs 急停中止
    let home_entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let home_estop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct BlockingHomeTransport {
        entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
        estop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    }
    impl laser_device::DeviceTransport for BlockingHomeTransport {
        fn write_line(&self, line: &str) -> Result<(), String> {
            let mut r = self.responses.lock().unwrap();
            if line == "$$" {
                r.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else if line == "$CD" {
                r.push_back("ok".into());
            } else if line == "$H" {
                r.push_back("ok".into());
                self.entered
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::EmergencyStop {
                self.estop.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            if let Some(line) = self.responses.lock().unwrap().pop_front() {
                return Ok(Some(line));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            if self.estop.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(None)
            } else {
                Ok(Some("<Home|MPos:0,0,0>".into()))
            }
        }
    }
    let he = home_entered.clone();
    let hes = home_estop.clone();
    let home_svc = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(std::sync::Arc::new(move |_| {
            Ok(Box::new(BlockingHomeTransport {
                entered: he.clone(),
                estop: hes.clone(),
                responses: Default::default(),
            }))
        })),
    );
    let svc2 = home_svc.clone();
    let home_thread = std::thread::spawn(move || {
        post_json(
            &svc2,
            "/api/safe-action",
            &serde_json::json!({
                "action": "home",
                "confirmed": true,
                "connection_mode": "network",
                "host": "127.0.0.1"
            }),
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !home_entered.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        home_svc.active_job_count(),
        1,
        "独立 home 期间必须登记在 active_jobs"
    );
    home_svc.abort_all_active_jobs().unwrap();
    let home_res = home_thread.join().unwrap();
    assert_eq!(home_res["success"], false, "{home_res}");
    assert!(home_estop.load(std::sync::atomic::Ordering::SeqCst));
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

/// 在首条 G21 已写出、正要读应答时注入取消。可同时给出控制器报警。
fn cancel_at_first_g21(
    service: ApiService,
    alarm: bool,
    written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    events: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
) -> ApiService {
    use laser_device::{DeviceTransport, RealtimeCommand};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct CancelAtAck {
        transport: Box<dyn DeviceTransport>,
        jobs: ActiveJobs,
        alarm: bool,
        pending: AtomicBool,
        injected: AtomicBool,
        held: AtomicBool,
        events: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }
    impl DeviceTransport for CancelAtAck {
        fn write_line(&self, line: &str) -> Result<(), String> {
            if line == "G21" && !self.injected.swap(true, Ordering::SeqCst) {
                self.pending.store(true, Ordering::SeqCst);
            }
            self.transport.write_line(line)
        }
        fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
            match command {
                RealtimeCommand::FeedHold => {
                    self.held.store(true, Ordering::SeqCst);
                    self.events.lock().unwrap().push(b'!');
                }
                RealtimeCommand::StatusQuery => self.events.lock().unwrap().push(b'?'),
                RealtimeCommand::EmergencyStop => self.events.lock().unwrap().push(0x18),
                _ => {}
            }
            self.transport.write_realtime(command)
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            if self.pending.swap(false, Ordering::SeqCst) {
                self.jobs
                    .lock()
                    .unwrap()
                    .values()
                    .next()
                    .unwrap()
                    .cancel
                    .cancel();
                return if self.alarm {
                    Ok(Some("ALARM:1".into()))
                } else {
                    Err("timed out".into())
                };
            }
            if self.held.swap(false, Ordering::SeqCst) {
                self.events.lock().unwrap().push(b'H');
                return Ok(Some("<Hold:0|MPos:0,0,0>".into()));
            }
            self.transport.read_line()
        }
    }
    let jobs = service.active_jobs.clone();
    let factory = recording_transport_factory(100, written);
    service.with_transport_factory(std::sync::Arc::new(move |spec| {
        Ok(Box::new(CancelAtAck {
            transport: factory(spec)?,
            jobs: jobs.clone(),
            alarm,
            pending: Default::default(),
            injected: Default::default(),
            held: Default::default(),
            events: events.clone(),
        }))
    }))
}

fn seed_review_send(service: &ApiService, root: &std::path::Path, workflow_id: &str, resume: bool) {
    let gcode = "G21\nG90\nG0 X0 Y0\nG1 X20 F1000\nM5\n";
    std::fs::write(root.join("review.gcode"), gcode).unwrap();
    let sha = hex::encode(sha2::Sha256::digest(gcode.as_bytes()));
    service.workflows.lock().unwrap().insert(
        workflow_id.into(),
        serde_json::json!({
            "workflow_id": workflow_id,
            "status": if resume { "cancelled" } else { "preview_ready" },
            "artifacts": { "gcode_file": "review.gcode", "gcode_sha256": sha },
            "confirmation_snapshot": { "gcode_file": "review.gcode", "gcode_sha256": sha },
            "send_result": {
                "target": "127.0.0.1:23", "lines_sent": 2, "lines_completed": 2,
                "resumable_from_line": 2, "resume_boundary_source": "device_hold_ln",
                "total_lines": 5, "can_resume": true
            }
        }),
    );
}

#[test]
fn alarm_during_cancel_stays_failed_for_send_and_framing() {
    let root = tempfile::tempdir().unwrap();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = cancel_at_first_g21(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(root.path().to_path_buf())
        .with_preview_roots(vec![root.path().to_path_buf()]),
        true,
        Default::default(),
        events.clone(),
    );
    seed_review_send(&service, root.path(), "alarm-send", false);
    let started = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "confirm_send", "workflow_id": "alarm-send", "confirmed": true,
            "connection_mode": "network", "host": "127.0.0.1",
        }),
    );
    assert_eq!(started["success"], true, "{started}");
    let failed = wait_for_status(&service, "alarm-send", "failed");
    let send = &failed["result"]["send_result"];
    assert_eq!(send["cancelled"], false, "{failed}");
    assert!(
        send["error"].as_str().unwrap().contains("ALARM:1"),
        "{failed}"
    );
    assert!(
        events
            .lock()
            .unwrap()
            .windows(4)
            .any(|w| w == [b'!', b'?', b'H', 0x18])
    );

    events.lock().unwrap().clear();
    let (code, frame) = post_with_ctx(
        &service,
        "/api/device/framing",
        &serde_json::json!({
            "connection_mode": "network", "host": "127.0.0.1", "max_x": 10, "max_y": 10,
        }),
        laser_contracts::CallerContext::Loopback,
    );
    assert_eq!(code, 500, "{frame}");
    assert_eq!(frame["detail"]["error_code"], "framing_failed");
    assert_eq!(frame["detail"]["cancelled"], false);
    assert!(
        frame["result"].as_str().unwrap().contains("ALARM:1"),
        "{frame}"
    );
    assert!(
        events
            .lock()
            .unwrap()
            .windows(4)
            .any(|w| w == [b'!', b'?', b'H', 0x18])
    );
}

#[test]
fn resume_modal_header_cancel_waits_for_hold_before_reset() {
    let root = tempfile::tempdir().unwrap();
    let written: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = cancel_at_first_g21(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(root.path().to_path_buf())
        .with_preview_roots(vec![root.path().to_path_buf()]),
        false,
        written.clone(),
        events.clone(),
    );
    seed_review_send(&service, root.path(), "modal-cancel", true);
    let started = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "resume", "workflow_id": "modal-cancel", "confirmed": true,
            "connection_mode": "network", "host": "127.0.0.1",
        }),
    );
    assert_eq!(started["success"], true, "{started}");
    let terminal = wait_for_status(&service, "modal-cancel", "cancelled");
    let send = &terminal["result"]["send_result"];
    assert_eq!(send["cancelled"], true, "{terminal}");
    assert_eq!(send["can_resume"], false, "{terminal}");
    assert_eq!(send["lines_sent"], 2);
    let written = written.lock().unwrap();
    assert_eq!(
        written.last().map(String::as_str),
        Some("G21"),
        "{written:?}"
    );
    let events = events.lock().unwrap();
    let first_hold = events.iter().position(|byte| *byte == b'!').unwrap();
    assert_eq!(&events[first_hold..], &[b'!', b'?', b'H', 0x18]);
}

#[test]
fn shutdown_allows_emergency_stop_and_soft_reset_for_unregistered_device() {
    let fake = laser_device::FakeGrbl::new(Vec::<String>::new());
    let opened = fake.clone();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(opened.clone()))));
    service.request_shutdown();
    assert_eq!(service.active_job_count(), 0);
    for action in ["emergency_stop", "soft_reset"] {
        let result = post_json(
            &service,
            "/api/safe-action",
            &serde_json::json!({
                "action": action, "confirmed": true, "connection_mode": "network", "host": "127.0.0.1",
            }),
        );
        assert_eq!(result["success"], true, "{result}");
    }
    assert!(fake.lines().is_empty());
    assert_eq!(fake.realtime(), [0x18, 0x18]);
}

#[test]
fn shutdown_during_open_drops_unwritten_transport_for_send_framing_and_home() {
    for (route, body) in [
        (
            "/api/safe-action",
            serde_json::json!({"action":"home","confirmed":true}),
        ),
        (
            "/api/workflow",
            serde_json::json!({"action":"confirm_send","confirmed":true,"workflow_id":"shutdown-open"}),
        ),
        (
            "/api/device/framing",
            serde_json::json!({"max_x":10,"max_y":10}),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let fake = laser_device::FakeGrbl::new(Vec::<String>::new());
        let opened = fake.clone();
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown = flag.clone();
        let service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(root.path().to_path_buf())
        .with_preview_roots(vec![root.path().to_path_buf()])
        .with_shutdown_flag(flag)
        .with_transport_factory(std::sync::Arc::new(move |_| {
            shutdown.store(true, std::sync::atomic::Ordering::Release);
            Ok(Box::new(opened.clone()))
        }));
        seed_review_send(&service, root.path(), "shutdown-open", false);
        let mut body = body;
        body["connection_mode"] = "network".into();
        body["host"] = "127.0.0.1".into();
        let result = post_json(&service, route, &body);
        assert_eq!(result["success"], false, "{route}: {result}");
        assert!(
            service.is_shutdown_requested(),
            "{route} 必须实际进入建连工厂"
        );
        assert!(
            fake.lines().is_empty() && fake.realtime().is_empty(),
            "{route} 不得对新连接写任何字节"
        );
        assert_eq!(service.active_job_count(), 0);
    }
}

#[test]
fn emergency_stop_prefers_raw_abort_recovers_poisoned_lock_and_bounds_lock_wait() {
    struct RawFirstTransport {
        fake: laser_device::FakeGrbl,
        rt_hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl laser_device::DeviceTransport for RawFirstTransport {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn write_realtime(&self, _command: laser_device::RealtimeCommand) -> Result<(), String> {
            self.rt_hits
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
        fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
            self.fake.raw_abort_handle()
        }
    }

    let fake = laser_device::FakeGrbl::new([]);
    let rt_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    );
    let transport: std::sync::Arc<dyn laser_device::DeviceTransport> =
        std::sync::Arc::new(RawFirstTransport {
            fake: fake.clone(),
            rt_hits: rt_hits.clone(),
        });
    service.active_jobs.lock().unwrap().insert(
        "wf-estop-raw".into(),
        std::sync::Arc::new(ActiveJob {
            workflow_id: "wf-estop-raw".into(),
            mode: "network",
            target: "127.0.0.1:23".into(),
            cancel: laser_device::CancellationToken::default(),
            progress: Default::default(),
            transport,
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );

    // 1. UI/API 急停必须优先走无锁 raw_abort_handle，不等待 write_realtime
    let res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true}),
    );
    assert_eq!(res["success"], true, "{res}");
    assert_eq!(fake.realtime(), vec![0x18]);
    assert_eq!(rt_hits.load(std::sync::atomic::Ordering::SeqCst), 0);

    // 2. 持锁线程触发 panic hook 非阻塞急停时不得自死锁
    {
        let _held = service.active_jobs.lock().unwrap();
        let _uncertain = service.uncertain_jobs.lock().unwrap();
        assert!(service.abort_all_active_jobs_nonblocking().is_ok());
        assert_eq!(fake.realtime(), vec![0x18]);
    }

    // 3. 另一线程短暂持锁时，重试能拿到作业并写出 0x18。
    std::thread::scope(|scope| {
        let (ready, held) = std::sync::mpsc::sync_channel(0);
        let api = &service;
        scope.spawn(move || {
            let _guard = api.active_jobs.lock().unwrap();
            ready.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        });
        held.recv().unwrap();
        service.abort_all_active_jobs_nonblocking().unwrap();
    });
    assert_eq!(fake.realtime(), vec![0x18; 2]);

    // 4. 等不到活动作业表时要明确失败，仍向可读取的待确认目标写出 0x18。
    std::thread::scope(|scope| {
        let guard = service.active_jobs.lock().unwrap();
        service.uncertain_jobs.lock().unwrap().insert(
            "wf-estop-raw".into(),
            UncertainJob::from_active(&guard["wf-estop-raw"], None),
        );
        let (result, done) = std::sync::mpsc::channel();
        let api = &service;
        scope.spawn(move || {
            result
                .send(api.abort_all_active_jobs_nonblocking())
                .unwrap();
        });
        let reply = done.recv_timeout(std::time::Duration::from_secs(2));
        drop(guard);
        assert!(reply.unwrap().unwrap_err().contains("活动作业表超时"));
    });
    assert_eq!(fake.realtime(), vec![0x18; 3]);
    service.uncertain_jobs.lock().unwrap().clear();

    // 5. 即使 active_jobs 锁中毒，两条退出路径仍须恢复锁并写出 0x18。
    let jobs_arc = service.active_jobs.clone();
    let _ = std::thread::spawn(move || {
        let _guard = jobs_arc.lock().unwrap();
        panic!("simulate mutex poison");
    })
    .join();
    assert!(service.active_jobs.is_poisoned());
    assert!(service.abort_all_active_jobs().is_ok());
    assert_eq!(fake.realtime(), vec![0x18; 4]);
    assert!(service.abort_all_active_jobs_nonblocking().is_ok());
    assert_eq!(fake.realtime(), vec![0x18; 5]);
}

#[test]
fn http_framing_and_safe_asset_path_reject_smuggling_chunked_and_backslash_traversal() {
    assert_eq!(safe_asset_path("app.js").as_deref(), Some("app.js"));
    assert_eq!(
        safe_asset_path("..\\..\\etc\\passwd"),
        None,
        "Unix 上也必须先归一化反斜杠再拦截 ParentDir"
    );
    assert_eq!(safe_asset_path("%5c..%5csecret.txt"), None);
    assert_eq!(safe_asset_path("a\0b.js"), None);
    // 非法百分比编码与多字节 UTF-8 不得触发 panic（防止经公开 /assets 路由触发全局 Panic Hook 急停）
    assert_eq!(percent_decode("%中"), "%中");
    assert_eq!(percent_decode("%\u{FFFD}x"), "%\u{FFFD}x");
    assert_eq!(percent_decode("%2e%2e/a"), "../a");

    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    );
    let utf8_percent =
        service.handle_http("GET /assets/%中 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".as_bytes());
    let utf8_text = String::from_utf8_lossy(&utf8_percent);
    assert!(
        utf8_text.starts_with("HTTP/1.1 404"),
        "公开 /assets 收到 %中 应安全返 404 而非 panic：{utf8_text}"
    );

    // 冲突的重复 Content-Length → 400 invalid_content_length
    let conflict = service.handle_http(
        b"POST /api/workflow HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\nContent-Length: 5\r\n\r\n{}",
    );
    let conflict_text = String::from_utf8_lossy(&conflict);
    assert!(conflict_text.starts_with("HTTP/1.1 400"), "{conflict_text}");
    assert!(
        conflict_text.contains("invalid_content_length"),
        "{conflict_text}"
    );

    // 非数字 Content-Length → 400 invalid_content_length
    let invalid_cl = service.handle_http(
        b"POST /api/workflow HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: abc\r\n\r\n{}",
    );
    let invalid_text = String::from_utf8_lossy(&invalid_cl);
    assert!(invalid_text.starts_with("HTTP/1.1 400"), "{invalid_text}");
    assert!(
        invalid_text.contains("invalid_content_length"),
        "{invalid_text}"
    );

    // Transfer-Encoding: chunked → 501 transfer_encoding_unsupported
    let chunked = service.handle_http(
        b"POST /api/workflow HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
    );
    let chunked_text = String::from_utf8_lossy(&chunked);
    assert!(chunked_text.starts_with("HTTP/1.1 501"), "{chunked_text}");
    assert!(
        chunked_text.contains("transfer_encoding_unsupported"),
        "{chunked_text}"
    );
}

#[test]
fn image_workflow_preview_rejects_out_of_sandbox_path_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let uploads = temp.path().join("uploads");
    let generated = temp.path().join("generated");
    std::fs::create_dir_all(&uploads).unwrap();
    std::fs::create_dir_all(&generated).unwrap();
    // 在沙箱外写入一张合法 PNG，验证绝对路径与 ../ 穿越均被拦截
    let outside_png = temp.path().join("secret.png");
    let img = image::GrayImage::from_pixel(4, 4, image::Luma([128]));
    img.save(&outside_png).unwrap();

    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_upload_root(uploads.clone())
    .with_generated_root(generated)
    .with_preview_roots(vec![uploads]);

    for evil_path in [
        outside_png.to_string_lossy().into_owned(),
        "../secret.png".to_owned(),
    ] {
        let resp = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "preview",
                "source_type": "image",
                "image_file": evil_path,
                "material": "3mm椴木板",
                "thickness_mm": 3.0
            }),
        );
        assert_eq!(
            resp["success"], false,
            "沙箱外路径 {evil_path} 必须被拒绝: {resp}"
        );
        assert!(
            resp["result"].as_str().unwrap_or("").contains("找不到"),
            "{resp}"
        );
    }
}

#[test]
fn probe_connections_preserves_uncertain_job_handle_and_does_not_probe() {
    let realtime = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    struct SpyTransport(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl laser_device::DeviceTransport for SpyTransport {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn write_realtime(&self, cmd: laser_device::RealtimeCommand) -> Result<(), String> {
            self.0.lock().unwrap().push(cmd.byte());
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
    }
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    );
    let target = "127.0.0.1:2399";
    let active = ActiveJob {
        workflow_id: "wf-uncertain-1".into(),
        mode: "network",
        target: target.into(),
        cancel: laser_device::CancellationToken::default(),
        progress: std::sync::Arc::new(laser_device::JobProgress::default()),
        transport: std::sync::Arc::new(SpyTransport(realtime.clone())),
        started_at: std::time::Instant::now(),
        spec: Some(laser_device::ConnectionSpec::Tcp {
            host: "127.0.0.1".into(),
            port: 2399,
        }),
    };
    service.uncertain_jobs.lock().unwrap().insert(
        "wf-uncertain-1".into(),
        UncertainJob::from_active(&active, active.spec.clone()),
    );
    let probe = post_json(
        &service,
        "/api/connection/check",
        &serde_json::json!({
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": 2399
        }),
    );
    assert_eq!(probe["result"]["connected"], false, "{probe}");
    assert!(
        probe["result"]["summary"]
            .as_str()
            .unwrap_or("")
            .contains("待确认停止"),
        "{probe}"
    );
    // 急停仍直接命中原活句柄写出 0x18，未被只读探测剥走
    let estop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true, "job_id": "wf-uncertain-1"}),
    );
    assert_eq!(estop["success"], true, "{estop}");
    assert_eq!(*realtime.lock().unwrap(), vec![0x18]);
}

#[test]
fn failed_mutating_safe_action_migrates_to_uncertain_jobs() {
    struct FailAfterMoveTransport {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    }
    impl laser_device::DeviceTransport for FailAfterMoveTransport {
        fn write_line(&self, line: &str) -> Result<(), String> {
            let mut q = self.responses.lock().unwrap();
            if line == "$$" {
                q.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else if line == "$CD" {
                q.push_back("error:3".into());
            } else {
                // 运动命令下发后返回报警，触发 0x18 兜底并失败（无用户取消）
                q.push_back("ALARM:2".into());
            }
            Ok(())
        }
        fn write_realtime(&self, _cmd: laser_device::RealtimeCommand) -> Result<(), String> {
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(|_| {
        Ok(Box::new(FailAfterMoveTransport {
            responses: Default::default(),
        }))
    }));
    let res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "relative_move",
            "confirmed": true,
            "x": 5.0,
            "y": 0.0,
            "connection_mode": "network",
            "host": "127.0.0.1",
            "telnet_port": 2345
        }),
    );
    assert_eq!(res["success"], false, "{res}");
    assert_eq!(service.active_job_count(), 0);
    let targets = parse_json_response(
        &service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    let jobs = targets["result"]["jobs"].as_array().unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "发令后失败的安全动作必须迁入 uncertain_jobs: {targets}"
    );
    assert_eq!(jobs[0]["state"], "unknown");
    assert_eq!(jobs[0]["target"], "127.0.0.1:2345");
}

#[test]
fn finalize_workflow_record_preserves_shutdown_result_unknown() {
    use laser_runtime::RuntimeStore;
    let store = std::sync::Arc::new(laser_runtime::MemoryStore::default());
    let record = laser_runtime::StoredRecord {
        id: "wf-shutdown-1".into(),
        kind: laser_runtime::RecordKind::Workflow,
        status: Some("failed".into()),
        payload: serde_json::json!({
            "workflow_id": "wf-shutdown-1",
            "status": "failed",
            "send_result": {
                "success": false,
                "running": false,
                "phase": "result_unknown",
                "can_resume": false,
                "error": "host 退出中断，任务安全终止"
            }
        }),
    };
    store.put(&record).unwrap();
    let workflows = Default::default();
    let calibrations = Default::default();
    let sink = Default::default();
    crate::workflow::finalize_workflow_record(
        Some(store.as_ref()),
        &workflows,
        &calibrations,
        &sink,
        laser_runtime::RecordKind::Workflow,
        "wf-shutdown-1",
        "cancelled",
        serde_json::json!({"can_resume": true, "resumable_from_line": 5}),
    );
    let after = store
        .get(laser_runtime::RecordKind::Workflow, "wf-shutdown-1")
        .unwrap()
        .unwrap();
    assert_eq!(after.status.as_deref(), Some("failed"));
    assert_eq!(after.payload["send_result"]["phase"], "result_unknown");
    assert_eq!(after.payload["send_result"]["can_resume"], false);
}

#[test]
fn discover_controllers_is_single_flight() {
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered_cb = entered.clone();
    let release_cb = release.clone();
    let service = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_controller_candidates(std::sync::Arc::new(move |_emit| {
            entered_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            while !release_cb.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        })),
    );
    let srv = service.clone();
    let first = std::thread::spawn(move || {
        post_json(&srv, "/api/ui/discover-controllers", &serde_json::json!({}))
    });
    let start = std::time::Instant::now();
    while !entered.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let second = post_json(
        &service,
        "/api/ui/discover-controllers",
        &serde_json::json!({}),
    );
    assert!(
        second["result"]["hint"]
            .as_str()
            .unwrap_or("")
            .contains("正在进行"),
        "并发扫描应被单飞闸门拦截: {second}"
    );
    release.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = first.join().unwrap();
}

#[test]
fn resume_rejects_non_cancelled_status_and_estop_bumps_cancel_epoch() {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_path_buf();
    let gcode_path = dir.join("job.gcode");
    let gcode = "G21\nG90\nG1 X5 F1000 S200\nG1 X10 F1000 S200\nM5\n";
    std::fs::write(&gcode_path, gcode).unwrap();
    let sha = hex::encode(Sha256::digest(gcode.as_bytes()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(dir.clone())
    .with_preview_roots(vec![dir]);

    for bad_status in ["failed", "preview_ready"] {
        let wf_id = format!("wf-resume-{bad_status}");
        service.workflows.lock().unwrap().insert(
            wf_id.clone(),
            serde_json::json!({
                "workflow_id": wf_id,
                "status": bad_status,
                "artifacts": { "gcode_file": "job.gcode", "gcode_sha256": sha },
                "confirmation_snapshot": { "gcode_file": "job.gcode", "gcode_sha256": sha },
                "send_result": {
                    "success": false,
                    "target": "tcp://127.0.0.1:23",
                    "lines_sent": 2,
                    "lines_completed": 2,
                    "resumable_from_line": 2,
                    "resume_boundary_source": "device_hold_ln",
                    "total_lines": 5,
                    "can_resume": true
                }
            }),
        );
        let resp = post_json(
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
        assert_eq!(resp["success"], false, "{bad_status}: {resp}");
        assert_eq!(
            resp["detail"]["error_code"], "workflow_not_resumable",
            "{bad_status}: {resp}"
        );
    }

    // 急停命中在途作业时须同步递增 cancel_epoch，阻止并发续雕复用急停前快照
    let fake = laser_device::FakeGrbl::new(Vec::<String>::new());
    let job_id = "wf-estop-epoch";
    let before_epoch = current_cancel_epoch(&service.launching_workflows, job_id);
    service.active_jobs.lock().unwrap().insert(
        job_id.into(),
        std::sync::Arc::new(ActiveJob {
            workflow_id: job_id.into(),
            mode: "network",
            target: "127.0.0.1:23".into(),
            cancel: laser_device::CancellationToken::default(),
            progress: std::sync::Arc::new(laser_device::JobProgress::default()),
            transport: std::sync::Arc::new(fake.clone()),
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );
    let stop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "emergency_stop",
            "confirmed": true,
            "job_id": job_id
        }),
    );
    assert_eq!(stop["success"], true, "{stop}");
    assert!(fake.realtime().contains(&0x18));
    assert!(
        current_cancel_epoch(&service.launching_workflows, job_id) > before_epoch,
        "emergency_stop on active job must increment cancel_epoch"
    );
}

#[test]
fn short_io_blocks_concurrent_double_open_but_allows_emergency_stop_reuse() {
    let factory_opens = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let opens_cb = factory_opens.clone();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(std::sync::Arc::new(move |_spec| {
        opens_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(laser_device::FakeGrbl::new(
            ["<Idle|MPos:0,0,0>"].map(str::to_owned),
        )))
    }));

    let held_transport = laser_device::FakeGrbl::new(Vec::<String>::new());
    let held_arc: std::sync::Arc<dyn laser_device::DeviceTransport> =
        std::sync::Arc::new(held_transport.clone());
    let target = "127.0.0.1:23";
    let _guard = register_short_io_target(&service.short_io_targets, target, held_arc);

    // 非急停的顺序安全动作（如 status）必须被 short_io_targets 拦截，不得并发双开
    let status_res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "status",
            "connection_mode": "network",
            "host": "127.0.0.1",
            "telnet_port": 23
        }),
    );
    assert_eq!(status_res["success"], false, "{status_res}");
    assert_eq!(
        factory_opens.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "short_io 占位期间不得打开第二个传输通道"
    );

    // 急停必须直接复用 short_io_targets 中在途连接写 0x18，绝不被阻塞也不必双开
    let estop_res = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "emergency_stop",
            "confirmed": true,
            "connection_mode": "network",
            "host": "127.0.0.1",
            "telnet_port": 23
        }),
    );
    assert_eq!(estop_res["success"], true, "{estop_res}");
    assert!(
        held_transport.realtime().contains(&0x18),
        "急停必须经由在途 short_io 通道送达 0x18"
    );
    assert_eq!(
        factory_opens.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "复用在途通道写急停时不应重复打开新连接"
    );
}
