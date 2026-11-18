use super::*;

struct StrictFixture {
    service: ApiService,
    _temp: tempfile::TempDir,
    written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    targets: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    db: Option<std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
}

fn strict_fixture(strict: bool, with_approvals: bool) -> StrictFixture {
    let temp = tempfile::tempdir().unwrap();
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let targets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let base = recording_transport_factory(200_000, written.clone());
    let targets_for_factory = targets.clone();
    let factory: TransportFactory =
        std::sync::Arc::new(move |spec: &laser_device::ConnectionSpec| {
            targets_for_factory.lock().unwrap().push(spec.describe());
            base(spec)
        });
    let mut service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(factory)
    .with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()))
    .with_strict_mode(strict);
    let mut approvals_db = None;
    if with_approvals {
        let db = std::sync::Arc::new(std::sync::Mutex::new(
            laser_runtime::RuntimeDb::open_memory().unwrap(),
        ));
        approvals_db = Some(db.clone());
        service = service
            .with_runtime_db(db.clone())
            .with_approval_service(std::sync::Arc::new(
                approval::ApprovalService::new(db.clone()).with_ttl_seconds(120),
            ));
        {
            let db = db.lock().unwrap();
            db.voice_device_seen("aa:bb:cc:dd:ee:ff", "test-client", "", "", "", 0.0)
                .unwrap();
            db.set_voice_device_status(
                "aa:bb:cc:dd:ee:ff",
                laser_runtime::VOICE_DEVICE_ALLOWED,
                0.0,
            )
            .unwrap();
        }
        service
            .register_voice_session("aa:bb:cc:dd:ee:ff", "test-client", "sess-1")
            .unwrap();
    }
    StrictFixture {
        service,
        _temp: temp,
        written,
        targets,
        db: approvals_db,
    }
}

fn preview_workflow(service: &ApiService) -> String {
    let preview = post_json(
        service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "STRICT", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
        }),
    );
    assert_eq!(preview["success"], true, "{preview}");
    preview["result"]["workflow_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn confirm_send_body(workflow_id: &str, host: &str) -> serde_json::Value {
    serde_json::json!({
        "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
        "connection_mode": "network", "network_host": host, "network_telnet_port": 23
    })
}

/// A-24 / A-26：严格模式下本机回环（无桌面会话）的发送、走边框、回原点一律拒绝，且不连接设备。
#[test]
fn strict_mode_blocks_loopback_r3_without_touching_device() {
    let fixture = strict_fixture(true, true);
    let workflow_id = preview_workflow(&fixture.service);
    let send = post_json(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
    );
    assert_eq!(
        send["detail"]["error_code"], "strict_mode_blocked",
        "{send}"
    );
    let framing = post_json(
        &fixture.service,
        "/api/device/framing",
        &serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
        }),
    );
    assert_eq!(
        framing["detail"]["error_code"], "strict_mode_blocked",
        "{framing}"
    );
    let home = post_json(
        &fixture.service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "home", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"
        }),
    );
    assert_eq!(
        home["detail"]["error_code"], "strict_mode_blocked",
        "{home}"
    );
    assert!(
        fixture.targets.lock().unwrap().is_empty(),
        "未授权的 R3 不得连接设备"
    );
    assert!(fixture.written.lock().unwrap().is_empty());
    let status = post_json(
        &fixture.service,
        "/api/workflow",
        &serde_json::json!({"action": "status", "workflow_id": workflow_id}),
    );
    assert_eq!(status["result"]["status"], "preview_ready", "{status}");
    assert!(
        fixture
            .service
            .approvals()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .is_empty(),
        "回环不创建审批"
    );
}

/// A-27：急停、软复位、降低倍率、查询状态与取消在严格模式下照常执行。
#[test]
fn strict_mode_never_blocks_stop_actions() {
    let fixture = strict_fixture(true, true);
    for action in [
        "emergency_stop",
        "soft_reset",
        "status",
        "feed_override_minus_10",
        "power_reset",
    ] {
        let (status, response) = post_with_ctx(
            &fixture.service,
            "/api/safe-action",
            &serde_json::json!({"action": action, "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_ne!(status, 202, "{action}: {response}");
        assert_ne!(status, 403, "{action}: {response}");
    }
    assert!(
        !fixture.targets.lock().unwrap().is_empty(),
        "停止类动作照常连接设备"
    );
    let workflow_id = preview_workflow(&fixture.service);
    let (status, cancel) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": workflow_id}),
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 200, "{cancel}");
    assert!(
        fixture
            .service
            .approvals()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .is_empty()
    );
}

fn voice_ctx() -> laser_contracts::CallerContext {
    laser_contracts::CallerContext::Voice {
        device_id: "aa:bb:cc:dd:ee:ff".into(),
        session_id: "sess-1".into(),
    }
}

#[test]
fn active_job_stop_write_failure_is_not_reported_as_success() {
    struct FailRealtime;
    impl laser_device::DeviceTransport for FailRealtime {
        fn write_line(&self, _: &str) -> Result<(), String> {
            panic!("stop must bypass G-code")
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            assert_eq!(command, laser_device::RealtimeCommand::EmergencyStop);
            Err("injected realtime failure".into())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
    }
    let fixture = strict_fixture(true, true);
    let cancel = laser_device::CancellationToken::default();
    fixture.service.active_jobs.lock().unwrap().insert(
        "wf-fail-stop".into(),
        std::sync::Arc::new(ActiveJob {
            workflow_id: "wf-fail-stop".into(),
            mode: "stream",
            target: "fake".into(),
            cancel: cancel.clone(),
            progress: std::sync::Arc::new(laser_device::JobProgress::default()),
            transport: std::sync::Arc::new(FailRealtime),
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );
    let args =
        serde_json::from_value(serde_json::json!({"action":"emergency_stop","confirmed":true}))
            .unwrap();
    let result = fixture.service.handle_safe_action(&args, &voice_ctx());
    assert!(
        !result.success && result.result.contains("0x18 发送失败"),
        "{result:?}"
    );
    assert!(cancel.is_cancelled());
}

/// 内置语音（NV4，A-20 / A-25 / A-26 在内置链路复测）：严格模式开启时，语音发起的发送与设备动作只生成审批，
/// 审批来源为 voice、记录发起设备，审计记为 `voice:<设备>`；批准后以审批身份执行。
#[test]
fn voice_r3_needs_desktop_approval_in_strict_mode() {
    let fixture = strict_fixture(true, true);
    let workflow_id = preview_workflow(&fixture.service);
    let mut request = confirm_send_body(&workflow_id, "127.0.0.1");
    request["confirmed"] = serde_json::json!(true);
    let (status, pending) = post_with_ctx(&fixture.service, "/api/workflow", &request, voice_ctx());
    assert_eq!(status, 202, "{pending}");
    assert_eq!(pending["result"]["status"], "approval_required");
    assert!(
        fixture.targets.lock().unwrap().is_empty() && fixture.written.lock().unwrap().is_empty(),
        "未批准不连接设备"
    );
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let approvals = fixture.service.approvals().unwrap();
    let record = approvals.get(&approval_id).unwrap();
    assert_eq!(record.source, laser_contracts::ApprovalSource::Voice);
    assert_eq!(record.session_id.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
    let audit = fixture
        .db
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .recent_audit(20)
        .unwrap();
    assert!(
        audit
            .iter()
            .any(|entry| entry.client == "voice:aa:bb:cc:dd:ee:ff"),
        "{audit:?}"
    );

    let (status, origin) = post_with_ctx(
        &fixture.service,
        "/api/safe-action",
        &serde_json::json!({"action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
        voice_ctx(),
    );
    assert_eq!(status, 202, "设定原点（R3）同样需要审批：{origin}");
    assert!(fixture.targets.lock().unwrap().is_empty());

    approvals
        .approve(&approval_id, &record.digest, &record.digest)
        .unwrap();
    let approval_ctx = laser_contracts::CallerContext::Approval {
        approval_id: approval_id.clone(),
    };
    let (status, sent) = post_with_ctx(&fixture.service, "/api/workflow", &request, approval_ctx);
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["result"]["status"], "sending");
}

/// 严格模式关闭时（2026-10-07 用户决定）：内置语音与官方接入点相同，模型带 confirmed=true 即直接发送，不生成审批。
#[test]
fn voice_follows_strict_mode_switch_when_off() {
    let fixture = strict_fixture(false, true);
    let workflow_id = preview_workflow(&fixture.service);
    let mut request = confirm_send_body(&workflow_id, "127.0.0.1");
    request["confirmed"] = serde_json::json!(true);
    let (status, sent) = post_with_ctx(&fixture.service, "/api/workflow", &request, voice_ctx());
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["result"]["status"], "sending");
    assert!(
        fixture
            .service
            .approvals()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .is_empty(),
        "严格模式关闭不生成审批"
    );
    let finished = wait_for_status(&fixture.service, &workflow_id, "completed");
    assert_eq!(finished["result"]["status"], "completed", "{finished}");
}

/// A-27（内置链路）：停止类动作与取消不受影响，不生成审批。
#[test]
fn revoke_during_preflight_prevents_queued_gcode_from_starting() {
    let fixture = strict_fixture(false, true);
    let slot = std::sync::Arc::new(std::sync::OnceLock::<std::sync::Weak<ApiService>>::new());
    let owner = slot.clone();
    let written = fixture.written.clone();
    let factory = recording_transport_factory(100, written.clone());
    let service = std::sync::Arc::new(fixture.service.with_transport_factory(std::sync::Arc::new(
        move |spec| {
            let api = owner.get().unwrap().upgrade().unwrap();
            api.set_voice_device_status(
                "aa:bb:cc:dd:ee:ff",
                laser_runtime::VOICE_DEVICE_REVOKED,
                0.0,
            )
            .unwrap();
            factory(spec)
        },
    )));
    slot.set(std::sync::Arc::downgrade(&service)).unwrap();
    let id = preview_workflow(&service);
    let (status, result) = post_with_ctx(
        &service,
        "/api/workflow",
        &confirm_send_body(&id, "127.0.0.1"),
        voice_ctx(),
    );
    assert_eq!(status, 403, "{result}");
    assert!(service.active_jobs.lock().unwrap().is_empty());
    assert!(
        written
            .lock()
            .unwrap()
            .iter()
            .all(|line| line == "$$" || line == "$CD" || line.starts_with("$RI=")),
        "revoked request must not send machining lines"
    );
}

#[test]
fn revoke_during_connect_prevents_safe_motion_and_framing() {
    for (route, status, body) in [
        (
            "/api/safe-action",
            400,
            serde_json::json!({"action":"home","confirmed":true,"connection_mode":"network","host":"127.0.0.1"}),
        ),
        (
            "/api/device/framing",
            403,
            serde_json::json!({"confirmed":true,"connection_mode":"network","host":"127.0.0.1","max_x":10,"max_y":10}),
        ),
    ] {
        let fixture = strict_fixture(false, true);
        let slot = std::sync::Arc::new(std::sync::OnceLock::<std::sync::Weak<ApiService>>::new());
        let owner = slot.clone();
        let written = fixture.written.clone();
        let factory = recording_transport_factory(100, written.clone());
        let service = std::sync::Arc::new(fixture.service.with_transport_factory(
            std::sync::Arc::new(move |spec| {
                owner
                    .get()
                    .unwrap()
                    .upgrade()
                    .unwrap()
                    .set_voice_device_status(
                        "aa:bb:cc:dd:ee:ff",
                        laser_runtime::VOICE_DEVICE_REVOKED,
                        0.0,
                    )
                    .unwrap();
                factory(spec)
            }),
        ));
        slot.set(std::sync::Arc::downgrade(&service)).unwrap();
        let (code, result) = post_with_ctx(&service, route, &body, voice_ctx());
        assert_eq!(code, status, "{route}: {result}");
        assert_eq!(result["success"], false);
        assert!(
            written
                .lock()
                .unwrap()
                .iter()
                .all(|line| line == "$$" || line == "$CD" || line.starts_with("$RI=")),
            "no motion after revoke"
        );
    }
}

#[test]
fn voice_stop_without_active_job_writes_reset_and_reports_connection_failure() {
    for strict in [false, true] {
        let fixture = strict_fixture(strict, true);
        let fake = laser_device::FakeGrbl::new(Vec::<String>::new());
        let observed = fake.clone();
        let service = fixture
            .service
            .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake.clone()))));
        assert!(service.active_jobs.lock().unwrap().is_empty());
        let body = serde_json::json!({"action":"emergency_stop","confirmed":true,"connection_mode":"network","host":"127.0.0.1"});
        let (status, result) = post_with_ctx(&service, "/api/safe-action", &body, voice_ctx());
        assert_eq!(status, 200, "{result}");
        assert_eq!(observed.realtime(), vec![0x18]);
        assert!(
            observed.lines().is_empty(),
            "reset must not wait behind handshake/G-code"
        );
        let service = service
            .with_transport_factory(std::sync::Arc::new(|_| Err("fake disconnected".into())));
        let (status, result) = post_with_ctx(&service, "/api/safe-action", &body, voice_ctx());
        assert_eq!(status, 400);
        assert_eq!(result["success"], false);
    }
}

#[test]
fn voice_stop_actions_run_immediately() {
    let fixture = strict_fixture(true, true);
    for action in ["emergency_stop", "soft_reset", "status"] {
        let (status, response) = post_with_ctx(
            &fixture.service,
            "/api/safe-action",
            &serde_json::json!({"action": action, "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
            voice_ctx(),
        );
        assert_ne!(status, 202, "{action}: {response}");
        assert_ne!(status, 403, "{action}: {response}");
    }
    assert!(
        !fixture.targets.lock().unwrap().is_empty(),
        "停止类动作照常连接设备"
    );
    let workflow_id = preview_workflow(&fixture.service);
    let (status, cancel) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": workflow_id}),
        voice_ctx(),
    );
    assert_eq!(status, 200, "{cancel}");
    assert!(
        fixture
            .service
            .approvals()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .is_empty()
    );
}

/// A-25 / A-21：严格模式下小智桥接的发送只生成审批；桌面批准后以审批身份执行，
/// 使用审批保存的连接目标而不是执行请求里的地址。
#[test]
fn strict_mode_turns_bridge_send_into_approval_and_executes_with_saved_target() {
    let fixture = strict_fixture(true, true);
    let workflow_id = preview_workflow(&fixture.service);
    let mut request = confirm_send_body(&workflow_id, "127.0.0.1");
    request["request_id"] = serde_json::json!("req-1");
    let (status, pending) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &request,
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 202, "{pending}");
    assert_eq!(pending["success"], true);
    assert_eq!(pending["result"]["status"], "approval_required");
    assert!(
        pending["result"]["speech"]
            .as_str()
            .is_some_and(|speech| !speech.is_empty())
    );
    assert!(
        !pending.to_string().contains("127.0.0.1"),
        "响应不得包含设备地址: {pending}"
    );
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        fixture.targets.lock().unwrap().is_empty() && fixture.written.lock().unwrap().is_empty()
    );
    let status_now = post_json(
        &fixture.service,
        "/api/workflow",
        &serde_json::json!({"action": "status", "workflow_id": workflow_id}),
    );
    assert_eq!(status_now["result"]["status"], "preview_ready");
    let (_, again) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &request,
        laser_contracts::CallerContext::McpBridge,
    );
    assert_ne!(
        again["result"]["approval_id"],
        approval_id.as_str(),
        "旧桥接身份无法区分接入点，不得按自报编号复用另一条审批"
    );
    assert!(
        fixture.targets.lock().unwrap().is_empty(),
        "两次请求仍都未获执行授权"
    );

    let approval_ctx = laser_contracts::CallerContext::Approval {
        approval_id: approval_id.clone(),
    };
    let execute = confirm_send_body(&workflow_id, "192.0.2.99");
    let (status, early) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &execute,
        approval_ctx.clone(),
    );
    assert_eq!(status, 409, "{early}");
    assert_eq!(early["detail"]["error_code"], "approval_not_approved");

    let approvals = fixture.service.approvals().unwrap();
    let record = approvals.get(&approval_id).unwrap();
    assert_eq!(
        record.kind,
        laser_contracts::ApprovalKind::WorkflowConfirmSend
    );
    assert_eq!(record.source, laser_contracts::ApprovalSource::McpBridge);
    assert_eq!(record.target_record, workflow_id);
    approvals
        .approve(&approval_id, &record.digest, &record.digest)
        .unwrap();

    let (status, sent) = post_with_ctx(&fixture.service, "/api/workflow", &execute, approval_ctx);
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["result"]["status"], "sending");
    assert_eq!(
        fixture.targets.lock().unwrap().first().map(String::as_str),
        Some("127.0.0.1:23"),
        "必须使用审批保存的目标"
    );
    let executed = approvals.get(&approval_id).unwrap();
    assert_eq!(executed.status, laser_contracts::ApprovalStatus::Executed);
    assert_eq!(
        executed.outcome.unwrap().job_id.as_deref(),
        Some(workflow_id.as_str())
    );
    let finished = wait_for_status(&fixture.service, &workflow_id, "completed");
    assert_eq!(finished["result"]["status"], "completed", "{finished}");
}

/// A-23 / A-26：安全动作经局域网发起只生成审批；批准后执行一次，重放得到 approval_consumed。
#[test]
fn strict_mode_safe_action_approval_is_single_use() {
    let fixture = strict_fixture(true, true);
    let body = serde_json::json!({
        "action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"
    });
    let lan = laser_contracts::CallerContext::Lan {
        pairing_id: "pair_test".into(),
    };
    let (status, pending) = post_with_ctx(&fixture.service, "/api/safe-action", &body, lan);
    assert_eq!(status, 202, "{pending}");
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(fixture.targets.lock().unwrap().is_empty());
    let approvals = fixture.service.approvals().unwrap();
    let record = approvals.get(&approval_id).unwrap();
    assert_eq!(record.kind, laser_contracts::ApprovalKind::SafeAction);
    assert_eq!(record.source, laser_contracts::ApprovalSource::Lan);
    approvals
        .approve(&approval_id, &record.digest, &record.digest)
        .unwrap();

    let approval_ctx = laser_contracts::CallerContext::Approval {
        approval_id: approval_id.clone(),
    };
    let (_, done) = post_with_ctx(
        &fixture.service,
        "/api/safe-action",
        &body,
        approval_ctx.clone(),
    );
    assert!(
        fixture
            .written
            .lock()
            .unwrap()
            .iter()
            .any(|line| line == "G92 X0 Y0"),
        "批准后应执行一次: {done}"
    );
    let (status, replay) = post_with_ctx(&fixture.service, "/api/safe-action", &body, approval_ctx);
    assert_eq!(status, 409, "{replay}");
    assert_eq!(replay["detail"]["error_code"], "approval_consumed");

    // 授权不能挪用到别的动作：要素不同，摘要不一致
    let (_, second) = post_with_ctx(
        &fixture.service,
        "/api/safe-action",
        &body,
        laser_contracts::CallerContext::Lan {
            pairing_id: "pair_test".into(),
        },
    );
    let second_id = second["result"]["approval_id"].as_str().unwrap().to_owned();
    let second_record = approvals.get(&second_id).unwrap();
    approvals
        .approve(&second_id, &second_record.digest, &second_record.digest)
        .unwrap();
    let (status, mismatch) = post_with_ctx(
        &fixture.service,
        "/api/safe-action",
        &serde_json::json!({"action": "home", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
        laser_contracts::CallerContext::Approval {
            approval_id: second_id.clone(),
        },
    );
    assert_eq!(status, 409, "{mismatch}");
    assert_eq!(mismatch["detail"]["error_code"], "approval_mismatch");
    assert_eq!(
        approvals.get(&second_id).unwrap().status,
        laser_contracts::ApprovalStatus::Failed
    );
}

#[test]
fn strict_mode_allows_desktop_session_and_fails_closed_without_database() {
    let fixture = strict_fixture(true, true);
    let workflow_id = preview_workflow(&fixture.service);
    let (status, sent) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["result"]["status"], "sending");
    wait_for_status(&fixture.service, &workflow_id, "completed");

    let no_db = strict_fixture(true, false);
    let workflow_id = preview_workflow(&no_db.service);
    let (status, unavailable) = post_with_ctx(
        &no_db.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 503, "{unavailable}");
    assert_eq!(unavailable["detail"]["error_code"], "approval_unavailable");
    assert!(no_db.targets.lock().unwrap().is_empty());
}

/// A-40：严格模式关闭时，小智桥接与本机回环的发送行为与之前一致，不创建审批。
#[test]
fn strict_mode_off_keeps_existing_behavior() {
    let fixture = strict_fixture(false, true);
    let workflow_id = preview_workflow(&fixture.service);
    let (status, sent) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["result"]["status"], "sending");
    wait_for_status(&fixture.service, &workflow_id, "completed");
    let home = post_json(
        &fixture.service,
        "/api/safe-action",
        &serde_json::json!({
            "action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"
        }),
    );
    assert_ne!(
        home["detail"]["error_code"], "strict_mode_blocked",
        "{home}"
    );
    assert!(
        fixture
            .service
            .approvals()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.service.strict_mode());
}

// ----- 桌面会话与桌面审批接口（技术书 §5.3，实施清单 P1-07，验收 A-21、A-22、A-75、A-76） -----

#[test]
fn desktop_session_header_is_checked_and_maps_to_desktop_context() {
    let fixture = strict_fixture(true, true);
    let workflow_id = preview_workflow(&fixture.service);
    let send = confirm_send_body(&workflow_id, "127.0.0.1");
    // 尚未开启桌面会话（如独立 moss-host）：带头即拒绝
    let (status, rejected) = desktop_request(
        &fixture.service,
        "POST",
        "/api/workflow",
        Some(&send),
        Some(&"a".repeat(64)),
    );
    assert_eq!(status, 403, "{rejected}");
    assert_eq!(rejected["detail"]["error_code"], "desktop_session_invalid");

    let session = fixture.service.enable_desktop_session().unwrap();
    assert_eq!(session.len(), 64);
    assert_eq!(
        fixture.service.enable_desktop_session().unwrap(),
        session,
        "同一进程只有一个会话"
    );
    let (status, wrong) = desktop_request(
        &fixture.service,
        "POST",
        "/api/workflow",
        Some(&send),
        Some(&"b".repeat(64)),
    );
    assert_eq!(status, 403, "{wrong}");
    assert_eq!(wrong["detail"]["error_code"], "desktop_session_invalid");
    assert!(fixture.targets.lock().unwrap().is_empty());
    // 非回环连接即使带对了值也不算桌面会话
    assert!(
        fixture
            .service
            .check_desktop_session(
                &format!("GET / HTTP/1.1\r\nX-Moss-Desktop-Session: {session}"),
                false
            )
            .is_err()
    );

    // 严格模式下带有效会话的本机发送 = 桌面端，直接执行
    let (status, sent) = desktop_request(
        &fixture.service,
        "POST",
        "/api/workflow",
        Some(&send),
        Some(&session),
    );
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["result"]["status"], "sending");
    wait_for_status(&fixture.service, &workflow_id, "completed");
}

/// A-75：审批接口只接受桌面会话。
#[test]
fn approval_routes_require_desktop_session() {
    let fixture = strict_fixture(true, true);
    let session = fixture.service.enable_desktop_session().unwrap();
    let (status, response) = desktop_request(
        &fixture.service,
        "GET",
        "/v1/approvals?status=pending",
        None,
        None,
    );
    assert_eq!(status, 403, "{response}");
    assert_eq!(response["detail"]["error_code"], "desktop_session_required");
    let (status, _) = post_with_ctx(
        &fixture.service,
        "/v1/approvals/apr_x/approve",
        &serde_json::json!({"digest": "x"}),
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 403);
    let (status, response) = desktop_request(
        &fixture.service,
        "GET",
        "/v1/approvals?status=bogus",
        None,
        Some(&session),
    );
    assert_eq!(status, 400, "{response}");
    let (status, response) = desktop_request(
        &fixture.service,
        "GET",
        "/v1/approvals?status=pending",
        None,
        Some(&session),
    );
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["result"]["approvals"].as_array().unwrap().len(), 0);
    assert_eq!(
        response["result"]["limit"], 50,
        "列表上限随结果返回，界面不把它当总数"
    );
    assert_eq!(response["result"]["ttl_seconds"], 120);
    let (status, response) = desktop_request(
        &fixture.service,
        "POST",
        "/v1/approvals/apr_missing/approve",
        Some(&serde_json::json!({"digest": "x"})),
        Some(&session),
    );
    assert_eq!(status, 404, "{response}");
    assert_eq!(response["detail"]["error_code"], "approval_not_found");
}

/// A-21：小智发起 → 桌面列表（不含地址）→ 桌面批准 → 以审批身份按保存的目标执行。
#[test]
fn desktop_approval_executes_bridge_request_with_saved_target() {
    let fixture = strict_fixture(true, true);
    let session = fixture.service.enable_desktop_session().unwrap();
    let workflow_id = preview_workflow(&fixture.service);
    let (status, pending) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 202, "{pending}");
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, listed) = desktop_request(
        &fixture.service,
        "GET",
        "/v1/approvals?status=pending",
        None,
        Some(&session),
    );
    assert_eq!(status, 200, "{listed}");
    let card = &listed["result"]["approvals"][0];
    assert_eq!(card["approval_id"], approval_id.as_str());
    assert_eq!(card["target"]["mode"], "network");
    assert_eq!(card["target"]["label"], "网络设备");
    assert!(
        !listed.to_string().contains("127.0.0.1"),
        "列表不得包含设备地址: {listed}"
    );
    let digest = card["digest"].as_str().unwrap().to_owned();

    let approve_path = format!("/v1/approvals/{approval_id}/approve");
    let (status, wrong) = desktop_request(
        &fixture.service,
        "POST",
        &approve_path,
        Some(&serde_json::json!({"digest": "0".repeat(64)})),
        Some(&session),
    );
    assert_eq!(status, 409, "{wrong}");
    assert_eq!(wrong["detail"]["error_code"], "approval_mismatch");
    assert!(fixture.targets.lock().unwrap().is_empty());

    let (status, approved) = desktop_request(
        &fixture.service,
        "POST",
        &approve_path,
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert_eq!(status, 200, "{approved}");
    assert_eq!(approved["result"]["result"]["status"], "sending");
    assert_eq!(approved["result"]["approval"]["status"], "executed");
    assert_eq!(
        fixture.targets.lock().unwrap().first().map(String::as_str),
        Some("127.0.0.1:23")
    );
    let finished = wait_for_status(&fixture.service, &workflow_id, "completed");
    assert_eq!(finished["result"]["status"], "completed", "{finished}");

    let (status, again) = desktop_request(
        &fixture.service,
        "POST",
        &approve_path,
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["detail"]["error_code"], "approval_consumed");
}

/// A-22：创建审批后加工内容变化（重新预览），桌面批准得到 approval_mismatch，不执行。
#[test]
fn desktop_approval_rejects_changed_content() {
    let fixture = strict_fixture(true, true);
    let session = fixture.service.enable_desktop_session().unwrap();
    let workflow_id = preview_workflow(&fixture.service);
    let (_, pending) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::McpBridge,
    );
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let digest = fixture
        .service
        .approvals()
        .unwrap()
        .get(&approval_id)
        .unwrap()
        .digest;

    let store = fixture.service.store.as_ref().unwrap();
    let mut stored = store
        .get(laser_runtime::RecordKind::Workflow, &workflow_id)
        .unwrap()
        .unwrap();
    stored.payload["artifacts"]["gcode_sha256"] = serde_json::json!("f".repeat(64));
    store.put(&stored).unwrap();

    let (status, response) = desktop_request(
        &fixture.service,
        "POST",
        &format!("/v1/approvals/{approval_id}/approve"),
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert_eq!(status, 409, "{response}");
    assert_eq!(response["detail"]["error_code"], "approval_mismatch");
    assert_eq!(response["detail"]["approval"]["status"], "pending");
    assert!(
        fixture.targets.lock().unwrap().is_empty(),
        "摘要不一致不得连接设备"
    );
}

#[test]
fn desktop_reject_and_safe_action_approval() {
    let fixture = strict_fixture(true, true);
    let session = fixture.service.enable_desktop_session().unwrap();
    let body = serde_json::json!({"action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"});
    let lan = || laser_contracts::CallerContext::Lan {
        pairing_id: "pair_test".into(),
    };

    let (_, first) = post_with_ctx(&fixture.service, "/api/safe-action", &body, lan());
    let first_id = first["result"]["approval_id"].as_str().unwrap().to_owned();
    let (status, rejected) = desktop_request(
        &fixture.service,
        "POST",
        &format!("/v1/approvals/{first_id}/reject"),
        Some(&serde_json::json!({"reason": "材料没放好"})),
        Some(&session),
    );
    assert_eq!(status, 200, "{rejected}");
    assert_eq!(rejected["result"]["status"], "rejected");
    let digest = fixture
        .service
        .approvals()
        .unwrap()
        .get(&first_id)
        .unwrap()
        .digest;
    let (status, after_reject) = desktop_request(
        &fixture.service,
        "POST",
        &format!("/v1/approvals/{first_id}/approve"),
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert_eq!(status, 409, "{after_reject}");
    assert_eq!(after_reject["detail"]["error_code"], "approval_rejected");
    assert!(fixture.targets.lock().unwrap().is_empty());

    let (_, second) = post_with_ctx(&fixture.service, "/api/safe-action", &body, lan());
    let second_id = second["result"]["approval_id"].as_str().unwrap().to_owned();
    let digest = fixture
        .service
        .approvals()
        .unwrap()
        .get(&second_id)
        .unwrap()
        .digest;
    let (_, approved) = desktop_request(
        &fixture.service,
        "POST",
        &format!("/v1/approvals/{second_id}/approve"),
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert!(
        fixture
            .written
            .lock()
            .unwrap()
            .iter()
            .any(|line| line == "G92 X0 Y0"),
        "批准后应执行设原点: {approved}"
    );
    let state = fixture
        .service
        .approvals()
        .unwrap()
        .get(&second_id)
        .unwrap()
        .status;
    assert!(
        matches!(
            state,
            laser_contracts::ApprovalStatus::Executed | laser_contracts::ApprovalStatus::Failed
        ),
        "{state:?}"
    );
}

// ----- 审计（技术书 §7.3，实施清单 P1-08，验收 A-20、A-34） -----

fn audit_trail(fixture: &StrictFixture) -> Vec<laser_contracts::AuditEntry> {
    let mut entries = fixture
        .db
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .recent_audit(100)
        .unwrap();
    entries.reverse();
    entries
}

/// A-20：模型带 confirmed=true 发起发送 → 只生成审批，审计记录“删除确认”；
/// 桌面批准与执行、回环被拦截都留下审计，且审计不含设备地址。
#[test]
fn strict_mode_actions_are_audited() {
    use laser_contracts::{AuditAction, AuditResult};
    let fixture = strict_fixture(true, true);
    let session = fixture.service.enable_desktop_session().unwrap();
    let workflow_id = preview_workflow(&fixture.service);

    let blocked = post_json(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
    );
    assert_eq!(blocked["detail"]["error_code"], "strict_mode_blocked");

    let (status, pending) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 202, "{pending}");
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let digest = fixture
        .service
        .approvals()
        .unwrap()
        .get(&approval_id)
        .unwrap()
        .digest;
    let (status, approved) = desktop_request(
        &fixture.service,
        "POST",
        &format!("/v1/approvals/{approval_id}/approve"),
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert_eq!(status, 200, "{approved}");
    wait_for_status(&fixture.service, &workflow_id, "completed");

    let trail = audit_trail(&fixture);
    let actions: Vec<AuditAction> = trail.iter().map(|entry| entry.action).collect();
    assert_eq!(
        actions,
        vec![
            AuditAction::StrictModeBlocked,
            AuditAction::ApprovalCreated,
            AuditAction::ConfirmedFieldStripped,
            AuditAction::ApprovalApproved,
            AuditAction::ApprovalExecuted,
        ],
        "{trail:?}"
    );
    assert_eq!(trail[0].client, "local");
    assert_eq!(trail[0].result, AuditResult::Denied);
    assert_eq!(trail[0].target, workflow_id);
    assert_eq!(trail[1].client, "mcp");
    assert_eq!(
        trail[2].detail.removed_fields,
        vec!["confirmed".to_string()]
    );
    assert_eq!(trail[3].client, "desktop");
    assert!(
        trail[1..]
            .iter()
            .all(|entry| entry.detail.approval_id.as_deref() == Some(approval_id.as_str()))
    );
    let text = serde_json::to_string(&trail).unwrap();
    assert!(!text.contains("127.0.0.1"), "审计不得包含设备地址: {text}");
    // 审批保存的执行参数里也不保留模型自填的 confirmed
    let stored = fixture
        .service
        .approvals()
        .unwrap()
        .get(&approval_id)
        .unwrap();
    assert!(!stored.request.unwrap().contains_key("confirmed"));
}

/// A-34：审计写入失败时 R3 拒绝（不创建审批、不连接设备），R2 停止类照常执行。
#[test]
fn audit_failure_refuses_r3_and_keeps_r2() {
    let fixture = strict_fixture(true, true);
    fixture
        .db
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TABLE audit_log;")
        .unwrap();
    let lan = || laser_contracts::CallerContext::Lan {
        pairing_id: "pair_test".into(),
    };

    let (status, refused) = post_with_ctx(
        &fixture.service,
        "/api/safe-action",
        &serde_json::json!({"action": "home", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
        lan(),
    );
    assert_eq!(status, 503, "{refused}");
    assert_eq!(refused["detail"]["error_code"], "approval_unavailable");
    assert!(
        fixture
            .service
            .approvals()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .is_empty()
    );
    assert!(fixture.targets.lock().unwrap().is_empty());

    let (status, stopped) = post_with_ctx(
        &fixture.service,
        "/api/safe-action",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
        lan(),
    );
    assert!(status != 503 && status != 403 && status != 202, "{stopped}");
    assert!(
        !fixture.targets.lock().unwrap().is_empty(),
        "急停照常连接设备"
    );

    // 回环被拦截时审计也写不进去，但拦截照常生效
    let blocked = post_json(
        &fixture.service,
        "/api/device/framing",
        &serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
        }),
    );
    assert_eq!(blocked["detail"]["error_code"], "strict_mode_blocked");
}

// ----- 故障场景（技术书 §8，实施清单 P1-10，验收 A-31、A-33） -----

/// A-33：经审批的发送进行中进程崩溃 → 新进程启动恢复后 workflow 为 failed、审批结果未知，
/// 不连接设备、不重放，同一授权不能再用。
#[test]
fn restart_during_approved_send_marks_result_unknown_and_never_replays() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("moss.sqlite");
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let db1 = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open(&db_path).unwrap(),
    ));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(gated_transport_factory(gate.clone()))
    .with_runtime_db(db1)
    .with_strict_mode(true);
    let session = service.enable_desktop_session().unwrap();
    let workflow_id = preview_workflow(&service);
    let (status, pending) = post_with_ctx(
        &service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::McpBridge,
    );
    assert_eq!(status, 202, "{pending}");
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let digest = service
        .approvals()
        .unwrap()
        .get(&approval_id)
        .unwrap()
        .digest;
    let (status, approved) = desktop_request(
        &service,
        "POST",
        &format!("/v1/approvals/{approval_id}/approve"),
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert_eq!(status, 200, "{approved}");
    assert_eq!(approved["result"]["result"]["status"], "sending");
    let running = service.approvals().unwrap().get(&approval_id).unwrap();
    assert_eq!(
        running.outcome.unwrap().phase,
        Some(laser_contracts::ApprovalPhase::Running)
    );

    // “崩溃后重启”：新进程打开同一个数据库，按 laser-host 的启动顺序恢复
    let db2 = laser_runtime::RuntimeDb::open(&db_path).unwrap();
    db2.recover_after_restart().unwrap();
    let written_after = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let restarted = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(recording_transport_factory(200_000, written_after.clone()))
    .with_runtime_db(std::sync::Arc::new(std::sync::Mutex::new(db2)))
    .with_strict_mode(true);
    assert_eq!(restarted.recover_approvals_after_restart(), 1);

    let record = restarted.approvals().unwrap().get(&approval_id).unwrap();
    assert_eq!(record.status, laser_contracts::ApprovalStatus::Failed);
    let outcome = record.outcome.unwrap();
    assert_eq!(outcome.error_code.as_deref(), Some("result_unknown"));
    assert_eq!(outcome.phase, Some(laser_contracts::ApprovalPhase::Unknown));
    let status = post_json(
        &restarted,
        "/api/workflow",
        &serde_json::json!({"action": "status", "workflow_id": workflow_id}),
    );
    assert_eq!(status["result"]["status"], "failed", "{status}");

    // 不重放：重启后的服务没有连接设备；同一授权执行被拒
    let (status, replay) = post_with_ctx(
        &restarted,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::Approval {
            approval_id: approval_id.clone(),
        },
    );
    assert_ne!(status, 200, "{replay}");
    assert_ne!(replay["result"]["status"], "sending", "{replay}");
    assert!(
        written_after.lock().unwrap().is_empty(),
        "重启后不得向设备写入任何内容"
    );
    assert_eq!(
        restarted
            .approvals()
            .unwrap()
            .consume(&approval_id, &digest)
            .unwrap_err(),
        approval::ApprovalError::Consumed
    );

    // 清理：停掉“崩溃前进程”里仍在等待的作业；迟到的回写不覆盖“结果未知”
    let _ = service.abort_all_active_jobs();
    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(
        restarted
            .approvals()
            .unwrap()
            .get(&approval_id)
            .unwrap()
            .outcome
            .unwrap()
            .phase,
        Some(laser_contracts::ApprovalPhase::Unknown)
    );
}

/// 经审批的作业正常结束后，审批结果写回“加工完成”。
#[test]
fn approved_job_completion_is_written_back_to_approval() {
    let fixture = strict_fixture(true, true);
    let session = fixture.service.enable_desktop_session().unwrap();
    let workflow_id = preview_workflow(&fixture.service);
    let (_, pending) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        laser_contracts::CallerContext::McpBridge,
    );
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let digest = fixture
        .service
        .approvals()
        .unwrap()
        .get(&approval_id)
        .unwrap()
        .digest;
    let (status, approved) = desktop_request(
        &fixture.service,
        "POST",
        &format!("/v1/approvals/{approval_id}/approve"),
        Some(&serde_json::json!({"digest": digest})),
        Some(&session),
    );
    assert_eq!(status, 200, "{approved}");
    wait_for_status(&fixture.service, &workflow_id, "completed");
    let record = fixture
        .service
        .approvals()
        .unwrap()
        .get(&approval_id)
        .unwrap();
    assert_eq!(record.status, laser_contracts::ApprovalStatus::Executed);
    let outcome = record.outcome.unwrap();
    assert!(outcome.success);
    assert_eq!(
        outcome.phase,
        Some(laser_contracts::ApprovalPhase::Completed)
    );
    assert_eq!(outcome.job_id.as_deref(), Some(workflow_id.as_str()));
}

// ----- 语音按预览走边框（2026-11-18 用户决定，技术书 r51） -----

fn frame_body(workflow_id: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "action": "frame", "workflow_id": workflow_id,
        "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

/// 语音 action=frame：必须 confirmed=true、不许出光；范围只取预览记录的 frame_mm（请求里的坐标不算），
/// 以激光头当前位置为原点空走一圈回到起点；走完 workflow 仍可 confirm_send；
/// 刀路文件被改、任务已取消、没有这份预览都在连接设备前拒绝。
#[test]
fn voice_frame_follows_the_preview_range_without_light() {
    let fixture = strict_fixture(false, true);
    let service = &fixture.service;
    let nothing_touched = |fixture: &StrictFixture| {
        fixture.targets.lock().unwrap().is_empty() && fixture.written.lock().unwrap().is_empty()
    };
    let workflow_id = preview_workflow(service);
    let frame = service
        .load_record_payload(laser_runtime::RecordKind::Workflow, &workflow_id)
        .unwrap()["artifacts"]["frame_mm"]
        .clone();
    let min_x = frame["min_x"].as_f64().unwrap();
    let min_y = frame["min_y"].as_f64().unwrap();
    assert!(min_x < 0.0 && min_y < 0.0, "文字默认以中心为原点：{frame}");

    let (status, unconfirmed) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body(&workflow_id, serde_json::json!({})),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{unconfirmed}");
    assert_eq!(unconfirmed["detail"]["error_code"], "confirmation_required");
    let (status, lit) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body(
            &workflow_id,
            serde_json::json!({"confirmed": true, "power": 1}),
        ),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{lit}");
    assert_eq!(lit["detail"]["error_code"], "framing_light_not_allowed");
    let (status, missing) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body("wf-missing", serde_json::json!({"confirmed": true})),
        voice_ctx(),
    );
    assert_eq!(status, 404, "{missing}");
    assert!(nothing_touched(&fixture), "拒绝时不能连接设备");

    // 请求里自带的坐标与轮廓一概不用，只走预览记录的范围
    let (status, framed) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body(
            &workflow_id,
            serde_json::json!({"confirmed": true, "min_x": -200, "max_x": 200, "outline": [[0, 0], [1, 0], [0, 1]]}),
        ),
        voice_ctx(),
    );
    assert_eq!(status, 200, "{framed}");
    assert_eq!(framed["result"]["laser_on"], false);
    assert_eq!(framed["result"]["workflow_id"], workflow_id.as_str());
    let lines = fixture.written.lock().unwrap().clone();
    assert!(
        lines.iter().any(|l| l.starts_with("G92 X0 Y0")),
        "{lines:?}"
    );
    let start = format!("G0 X{min_x:.3} Y{min_y:.3}");
    assert!(
        lines.iter().any(|l| l.starts_with(&start)),
        "{start}: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("M3") || l.starts_with("M4")),
        "不能出光：{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.contains("X-200")), "{lines:?}");
    assert!(
        lines
            .iter()
            .rfind(|l| l.starts_with("G0") || l.starts_with("G1"))
            .unwrap()
            .starts_with("G0 X0 Y0"),
        "走完回到起点：{lines:?}"
    );

    // 走边框不改 workflow 状态：之后照常确认发送
    let (status, sent) = post_with_ctx(
        service,
        "/api/workflow",
        &confirm_send_body(&workflow_id, "127.0.0.1"),
        voice_ctx(),
    );
    assert_eq!(status, 200, "{sent}");

    // 等后台发送结束，后面的“拒绝时不碰设备”才有意义
    let start = std::time::Instant::now();
    while service.active_job_count() > 0 {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "后台发送没有结束"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // 刀路文件被改：与发送相同的 SHA-256 门禁拒绝
    fixture.written.lock().unwrap().clear();
    fixture.targets.lock().unwrap().clear();
    let tampered = preview_workflow(service);
    let gcode_file = service
        .load_record_payload(laser_runtime::RecordKind::Workflow, &tampered)
        .unwrap()["artifacts"]["gcode_file"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = fixture
        ._temp
        .path()
        .join(std::path::Path::new(&gcode_file).file_name().unwrap());
    std::fs::write(&path, "G21\nG0 X500 Y500\n").unwrap();
    let (status, rejected) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body(&tampered, serde_json::json!({"confirmed": true})),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{rejected}");
    assert_eq!(rejected["detail"]["error_code"], "send_gate_rejected");

    // 已取消的任务不能再走边框
    let cancelled = preview_workflow(service);
    let cancel = post_json(
        service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": cancelled}),
    );
    assert_eq!(cancel["success"], true, "{cancel}");
    let (status, blocked) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body(&cancelled, serde_json::json!({"confirmed": true})),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{blocked}");
    assert_eq!(blocked["detail"]["error_code"], "workflow_not_sendable");
    assert!(nothing_touched(&fixture), "拒绝时不能连接设备");
}

/// 严格模式：语音按预览走边框只生成走边框审批、不碰设备；桌面批准后按冻结的范围空走（不出光）。
#[test]
fn strict_mode_voice_frame_needs_desktop_approval() {
    let fixture = strict_fixture(true, true);
    let session = fixture.service.enable_desktop_session().unwrap();
    let workflow_id = preview_workflow(&fixture.service);
    let (status, pending) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &frame_body(&workflow_id, serde_json::json!({"confirmed": true})),
        voice_ctx(),
    );
    assert_eq!(status, 202, "{pending}");
    assert_eq!(pending["result"]["status"], "approval_required");
    assert!(
        fixture.targets.lock().unwrap().is_empty() && fixture.written.lock().unwrap().is_empty()
    );
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let record = fixture
        .service
        .approvals()
        .unwrap()
        .get(&approval_id)
        .unwrap();
    assert_eq!(record.kind, laser_contracts::ApprovalKind::Framing);
    assert_eq!(record.source, laser_contracts::ApprovalSource::Voice);
    let (status, approved) = desktop_request(
        &fixture.service,
        "POST",
        &format!("/v1/approvals/{approval_id}/approve"),
        Some(&serde_json::json!({"digest": record.digest})),
        Some(&session),
    );
    assert_eq!(status, 200, "{approved}");
    let lines = fixture.written.lock().unwrap().clone();
    assert!(
        lines.iter().any(|l| l.starts_with("G92 X0 Y0")),
        "{lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("M3") || l.starts_with("M4")),
        "{lines:?}"
    );
}

/// 走边框途中模型调了 workflow cancel（2026-11-18）：停的是走边框，预览保留、之后照样能走边框。
#[test]
fn workflow_cancel_during_framing_stops_framing_and_keeps_the_preview() {
    let fixture = strict_fixture(false, true);
    let workflow_id = preview_workflow(&fixture.service);
    let device = laser_device::FakeGrbl::new(Vec::<String>::new());
    fixture.service.active_jobs.lock().unwrap().insert(
        "framing-9".into(),
        std::sync::Arc::new(ActiveJob {
            workflow_id: "framing-9".into(),
            mode: "network",
            target: "127.0.0.1:23".into(),
            cancel: Default::default(),
            progress: Default::default(),
            transport: std::sync::Arc::new(device.clone()),
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );
    let (status, cancelled) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": workflow_id}),
        voice_ctx(),
    );
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(cancelled["detail"]["framing_stopped"], "framing-9");
    let job = fixture.service.active_jobs.lock().unwrap()["framing-9"].clone();
    assert!(job.cancel.is_cancelled());
    assert_eq!(device.realtime(), vec![b'!']);
    let record = fixture
        .service
        .load_record_payload(laser_runtime::RecordKind::Workflow, &workflow_id)
        .unwrap();
    assert_eq!(
        record["status"], "preview_ready",
        "预览不能被作废：{record}"
    );
}

fn insert_framing_job(
    service: &ApiService,
    id: &str,
    device: &laser_device::FakeGrbl,
) -> std::sync::Arc<ActiveJob> {
    let job = std::sync::Arc::new(ActiveJob {
        workflow_id: id.into(),
        mode: "network",
        target: "127.0.0.1:23".into(),
        cancel: Default::default(),
        progress: Default::default(),
        transport: std::sync::Arc::new(device.clone()),
        started_at: std::time::Instant::now(),
        spec: Some(laser_device::ConnectionSpec::Tcp {
            host: "127.0.0.1".into(),
            port: 23,
        }),
    });
    service
        .active_jobs
        .lock()
        .unwrap()
        .insert(id.into(), job.clone());
    job
}

/// 审查修正（2026-11-18）：走边框途中只有 AI 发来、且预览确实在等待发送时，cancel 才改为停边框；
/// 界面的“取消这个任务”、空编号、正在启动的任务照常走原来的取消，不碰走边框。
#[test]
fn only_ai_cancel_of_a_waiting_preview_is_redirected_to_stop_framing() {
    let fixture = strict_fixture(false, true);
    let service = &fixture.service;
    let device = laser_device::FakeGrbl::new(Vec::<String>::new());
    let job = insert_framing_job(service, "framing-5", &device);

    // 界面（本机回环）取消：照常作废预览，走边框不受影响
    let desk = preview_workflow(service);
    let cancelled = post_json(
        service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": desk}),
    );
    assert_eq!(cancelled["success"], true, "{cancelled}");
    let record = service
        .load_record_payload(laser_runtime::RecordKind::Workflow, &desk)
        .unwrap();
    assert_eq!(record["status"], "cancelled", "{record}");
    assert!(!job.cancel.is_cancelled() && device.realtime().is_empty());

    // 语音取消空编号：照常报错
    let (status, empty) = post_with_ctx(
        service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": ""}),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{empty}");
    assert!(!job.cancel.is_cancelled());

    // 语音取消一份正在启动的任务：照常取消那个任务（预约被取消），不去停走边框
    let launching = preview_workflow(service);
    service
        .launching_workflows
        .lock()
        .unwrap()
        .active
        .insert(launching.clone(), Default::default());
    let (_, launch_cancel) = post_with_ctx(
        service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": launching}),
        voice_ctx(),
    );
    assert!(
        launch_cancel["detail"]["framing_stopped"].is_null(),
        "{launch_cancel}"
    );
    assert!(!job.cancel.is_cancelled());
    service
        .launching_workflows
        .lock()
        .unwrap()
        .active
        .remove(&launching);

    // 语音取消一份在等待发送的预览：停的是走边框
    let waiting = preview_workflow(service);
    let (status, stopped) = post_with_ctx(
        service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": waiting}),
        voice_ctx(),
    );
    assert_eq!(status, 200, "{stopped}");
    assert_eq!(stopped["detail"]["framing_stopped"], "framing-5");
    assert!(job.cancel.is_cancelled());
}

/// 审查修正（2026-11-18）：继续走边框先确认、先找到正在走的那次边框；严格模式下审批目标与 job_id
/// 取自这次作业（请求没带连接也能建审批），批准后 `~` 只发到那次作业；指定别的设备、已在停止都拒绝。
#[test]
fn framing_resume_is_bound_to_its_job_and_device() {
    let fixture = strict_fixture(true, true);
    let service = &fixture.service;
    let session = service.enable_desktop_session().unwrap();
    let approvals = service.approvals().unwrap();
    let pending_count = || {
        approvals
            .list(Some(laser_contracts::ApprovalStatus::Pending), 100)
            .unwrap()
            .len()
    };

    // 没有边框在走、未确认：都不建审批
    let (status, none) = post_with_ctx(
        service,
        "/api/safe-action",
        &serde_json::json!({"action": "resume", "confirmed": true}),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{none}");
    assert_eq!(none["detail"]["error_code"], "no_active_framing");
    let device = laser_device::FakeGrbl::new(Vec::<String>::new());
    let job = insert_framing_job(service, "framing-8", &device);
    let (status, unconfirmed) = post_with_ctx(
        service,
        "/api/safe-action",
        &serde_json::json!({"action": "resume"}),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{unconfirmed}");
    assert_eq!(pending_count(), 0, "不建废审批");

    // 请求没带连接：审批目标取这次走边框的设备，摘要写“继续走边框”
    let (status, pending) = post_with_ctx(
        service,
        "/api/safe-action",
        &serde_json::json!({"action": "resume", "confirmed": true}),
        voice_ctx(),
    );
    assert_eq!(status, 202, "{pending}");
    let approval_id = pending["result"]["approval_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let record = approvals.get(&approval_id).unwrap();
    assert!(record.summary.contains("继续走边框"), "{}", record.summary);
    assert_eq!(
        record.connection,
        Some(laser_contracts::ApprovalConnection::Network {
            host: "127.0.0.1".into(),
            port: 23
        })
    );
    assert!(device.realtime().is_empty(), "批准前不发送");
    let (status, approved) = desktop_request(
        service,
        "POST",
        &format!("/v1/approvals/{approval_id}/approve"),
        Some(&serde_json::json!({"digest": record.digest})),
        Some(&session),
    );
    assert_eq!(status, 200, "{approved}");
    assert_eq!(device.realtime(), vec![b'~']);

    // 指定了别的设备：不发
    let (_, mismatch) = post_with_ctx(
        service,
        "/api/safe-action",
        &serde_json::json!({"action": "pause", "host": "10.0.0.9"}),
        voice_ctx(),
    );
    assert_eq!(mismatch["detail"], "framing_target_mismatch", "{mismatch}");
    // 预览 dry_run 不写字节
    let (_, dry) = post_with_ctx(
        service,
        "/api/safe-action",
        &serde_json::json!({"action": "pause", "dry_run": true}),
        voice_ctx(),
    );
    assert_eq!(dry["success"], true, "{dry}");
    assert_eq!(device.realtime(), vec![b'~']);
    // 已在停止：不能再继续
    job.cancel.cancel();
    let (status, stopping) = post_with_ctx(
        service,
        "/api/safe-action",
        &serde_json::json!({"action": "resume", "confirmed": true}),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{stopping}");
    assert_eq!(device.realtime(), vec![b'~']);
}

/// 审查修正（2026-11-18）：停止走边框的进给保持写不出去时如实报错、提示急停。
#[test]
fn stop_framing_reports_a_failed_feed_hold() {
    struct Broken;
    impl laser_device::DeviceTransport for Broken {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Err("断开".into())
        }
        fn write_realtime(&self, _command: laser_device::RealtimeCommand) -> Result<(), String> {
            Err("断开".into())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(None)
        }
    }
    let fixture = strict_fixture(false, false);
    fixture.service.active_jobs.lock().unwrap().insert(
        "framing-3".into(),
        std::sync::Arc::new(ActiveJob {
            workflow_id: "framing-3".into(),
            mode: "network",
            target: "127.0.0.1:23".into(),
            cancel: Default::default(),
            progress: Default::default(),
            transport: std::sync::Arc::new(Broken),
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );
    let result = post_json(
        &fixture.service,
        "/api/safe-action",
        &serde_json::json!({"action": "stop_framing"}),
    );
    assert_eq!(result["success"], false, "{result}");
    assert!(
        result["result"].as_str().unwrap().contains("急停"),
        "{result}"
    );
}

/// 审查修正（2026-11-18）：按预览走边框与发送一样复核材料安全；`power: null` 视为没给（不出光）。
#[test]
fn voice_frame_checks_material_safety_and_accepts_null_power() {
    let fixture = strict_fixture(false, true);
    let service = &fixture.service;
    let workflow_id = preview_workflow(service);
    let (status, null_power) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body(
            &workflow_id,
            serde_json::json!({"confirmed": true, "power": null}),
        ),
        voice_ctx(),
    );
    assert_eq!(status, 200, "{null_power}");

    // 记录里的材料被改成 PVC（例如旧版本生成的记录）：拒绝，不连接设备
    let store = service.store.as_deref().unwrap();
    let mut record = store
        .get(laser_runtime::RecordKind::Workflow, &workflow_id)
        .unwrap()
        .unwrap();
    record.payload["material"] = serde_json::json!("PVC");
    store.put(&record).unwrap();
    fixture.targets.lock().unwrap().clear();
    let (status, blocked) = post_with_ctx(
        service,
        "/api/workflow",
        &frame_body(&workflow_id, serde_json::json!({"confirmed": true})),
        voice_ctx(),
    );
    assert_eq!(status, 400, "{blocked}");
    assert_eq!(blocked["detail"]["error_code"], "material_safety_blocked");
    assert!(fixture.targets.lock().unwrap().is_empty());
}

/// 审查修正（2026-11-18）：发起方（内置语音）已放弃等待后，同一次请求不再开始走边框。
#[test]
fn abandoned_voice_frame_never_starts_moving() {
    let fixture = strict_fixture(false, true);
    let workflow_id = preview_workflow(&fixture.service);
    let watch: crate::ShortJobWatch =
        std::sync::Arc::new(std::sync::Mutex::new(crate::ShortJobStart::Abandoned));
    crate::watch_short_jobs_on_this_thread(Some(watch));
    let (status, refused) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &frame_body(&workflow_id, serde_json::json!({"confirmed": true})),
        voice_ctx(),
    );
    crate::watch_short_jobs_on_this_thread(None);
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["detail"]["error_code"], "short_job_abandoned");
    assert_eq!(fixture.service.active_job_count(), 0);
    assert!(
        !fixture
            .written
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("G0") || l.starts_with("G1")),
        "放弃后不能运动"
    );
    // 登记了回执、没放弃：开始时回执写入作业编号
    let watch: crate::ShortJobWatch = std::sync::Arc::default();
    crate::watch_short_jobs_on_this_thread(Some(watch.clone()));
    let (status, framed) = post_with_ctx(
        &fixture.service,
        "/api/workflow",
        &frame_body(&workflow_id, serde_json::json!({"confirmed": true})),
        voice_ctx(),
    );
    crate::watch_short_jobs_on_this_thread(None);
    assert_eq!(status, 200, "{framed}");
    assert!(
        matches!(&*watch.lock().unwrap(), crate::ShortJobStart::Started(id) if id.starts_with("framing-"))
    );
}
