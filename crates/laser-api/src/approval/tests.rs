//! 审批服务单元测试：目标解析、摘要、创建 / 批准 / 消费、过期、重启恢复与审计。

use super::desktop::approval_view;
use super::*;
use serde_json::json;

mod home_agent;

struct Fixture {
    service: ApprovalService,
    clock: Arc<Mutex<f64>>,
    db: Arc<Mutex<RuntimeDb>>,
}

impl Fixture {
    fn new() -> Self {
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let clock = Arc::new(Mutex::new(1000.0_f64));
        let reader = clock.clone();
        let service = ApprovalService::new(db.clone())
            .with_ttl_seconds(120)
            .with_clock(Arc::new(move || *reader.lock().unwrap()));
        Self { service, clock, db }
    }

    fn audit_actions(&self) -> Vec<AuditAction> {
        let mut entries = self.db.lock().unwrap().recent_audit(100).unwrap();
        entries.reverse();
        entries.into_iter().map(|entry| entry.action).collect()
    }

    fn break_audit_log(&self) {
        self.db
            .lock()
            .unwrap()
            .connection
            .execute_batch("DROP TABLE audit_log;")
            .unwrap();
    }

    fn advance(&self, seconds: f64) {
        *self.clock.lock().unwrap() += seconds;
    }
}

fn network() -> ApprovalConnection {
    ApprovalConnection::Network {
        host: "192.0.2.10".into(),
        port: 23,
    }
}

fn workflow_record(gcode_sha: &str) -> Value {
    json!({
        "workflow_id": "wf_test",
        "input": {"material": "椴木板", "thickness_mm": 3.0, "power": 300, "feed_rate": 1500, "passes": 1},
        "artifacts": {"gcode_file": "job.gcode", "gcode_sha256": gcode_sha},
    })
}

fn new_approval(request_id: Option<&str>) -> NewApproval {
    NewApproval {
        owner: Some("authenticated-test-peer".into()),
        voice_permit: None,
        kind: ApprovalKind::WorkflowConfirmSend,
        target_record: "wf_test".into(),
        source: ApprovalSource::McpBridge,
        session_id: None,
        request_id: request_id.map(str::to_owned),
        summary: "雕刻文字；椴木板 3 mm；功率 30%，速度 1500 mm/min，1 遍".into(),
        facts: record_digest_facts(&workflow_record(&"a".repeat(64)), &Map::new()),
        connection: network(),
        request: Map::new(),
        client: "mcp".into(),
        removed_fields: Vec::new(),
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn request(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

/// A-35：串口留空且没有 .env 默认值时返回 target_required，绝不自动选串口。
#[test]
fn blank_serial_port_without_default_requires_target() {
    let error = resolve_approval_target(
        &request(json!({"connection_mode": "serial", "port": "  "})),
        &no_env,
    )
    .unwrap_err();
    assert_eq!(error.code(), "target_required");
    assert_eq!(error.http_status(), 400);
    let error = resolve_approval_target(&request(json!({"connection_mode": "network"})), &no_env)
        .unwrap_err();
    assert_eq!(error.code(), "target_required");
    // 什么都没给、也没有默认模式：按网络模式要求地址
    let error = resolve_approval_target(&Map::new(), &no_env).unwrap_err();
    assert_eq!(error.code(), "target_required");
    // 占位符形式的默认值不算配置
    let placeholder = |name: &str| (name == "LASER_NETWORK_HOST").then(|| "<设备IP>".to_string());
    assert!(resolve_approval_target(&Map::new(), &placeholder).is_err());
}

#[test]
fn explicit_or_env_targets_are_resolved() {
    assert_eq!(
        resolve_approval_target(&request(json!({"network_host": "192.0.2.10"})), &no_env).unwrap(),
        network()
    );
    assert_eq!(
        resolve_approval_target(
            &request(json!({"port": "COMX", "baudrate": 230400})),
            &no_env
        )
        .unwrap(),
        ApprovalConnection::Serial {
            port: "COMX".into(),
            baud_rate: 230400
        }
    );
    let env = |name: &str| match name {
        "LASER_DEFAULT_CONNECTION_MODE" => Some("serial".to_string()),
        "GRBL_DEFAULT_PORT" => Some("COMX".to_string()),
        _ => None,
    };
    assert_eq!(
        resolve_approval_target(&Map::new(), &env).unwrap(),
        ApprovalConnection::Serial {
            port: "COMX".into(),
            baud_rate: 115200
        }
    );
    let error = resolve_approval_target(&request(json!({"port": "COMX", "baudrate": 10})), &no_env)
        .unwrap_err();
    assert_eq!(error.code(), "approval_invalid_request");
}

#[test]
fn canonical_json_sorts_keys_without_whitespace() {
    let value = json!({"b": 1, "a": {"d": [1, 2.5, "x"], "c": "引号\""}, "e": null});
    assert_eq!(
        canonical_json(&value),
        r#"{"a":{"c":"引号\"","d":[1,2.5,"x"]},"b":1,"e":null}"#
    );
}

/// 摘要覆盖 G-code、发送方式与解析后的目标；不受键顺序影响；facts 不能冒充保留键。
#[test]
fn digest_changes_with_gcode_send_mode_and_target() {
    let facts = record_digest_facts(&workflow_record(&"a".repeat(64)), &Map::new());
    let base = approval_digest(
        ApprovalKind::WorkflowConfirmSend,
        "wf_test",
        &facts,
        &network(),
    );
    assert_eq!(base.len(), 64);
    assert_eq!(
        base,
        approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_test",
            &facts.clone(),
            &network()
        )
    );

    let regenerated = record_digest_facts(&workflow_record(&"b".repeat(64)), &Map::new());
    assert_ne!(
        base,
        approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_test",
            &regenerated,
            &network()
        )
    );

    let offline = record_digest_facts(
        &workflow_record(&"a".repeat(64)),
        &request(json!({"offline_sd": true})),
    );
    assert_eq!(offline["send_mode"], "sd_offline");
    assert_ne!(
        base,
        approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_test",
            &offline,
            &network()
        )
    );

    let other_device = ApprovalConnection::Network {
        host: "192.0.2.11".into(),
        port: 23,
    };
    assert_ne!(
        base,
        approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_test",
            &facts,
            &other_device
        )
    );
    assert_ne!(
        base,
        approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_other",
            &facts,
            &network()
        )
    );

    let mut forged = facts.clone();
    forged.insert("workflow_id".into(), json!("wf_other"));
    forged.insert("kind".into(), json!("framing"));
    assert_eq!(
        base,
        approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_test",
            &forged,
            &network()
        )
    );

    assert_eq!(facts["gcode_sha256"], json!("a".repeat(64)));
    assert_eq!(facts["material"], "椴木板");
    assert_eq!(facts["params"]["power"], 300);
    assert_eq!(facts["send_mode"], "stream");
}

#[test]
fn create_stores_connection_and_is_idempotent_per_request_id() {
    let fixture = Fixture::new();
    let created = fixture.service.create(new_approval(Some("req-1"))).unwrap();
    assert!(created.approval_id.starts_with(APPROVAL_ID_PREFIX));
    assert_eq!(created.status, ApprovalStatus::Pending);
    assert_eq!(created.expires_at, 1120.0);
    assert_eq!(created.connection, Some(network()));
    let text = serde_json::to_string(&created).unwrap();
    assert!(!text.contains("192.0.2.10"), "{text}");

    let again = fixture.service.create(new_approval(Some("req-1"))).unwrap();
    assert_eq!(again.approval_id, created.approval_id);
    let other = fixture.service.create(new_approval(Some("req-2"))).unwrap();
    assert_ne!(other.approval_id, created.approval_id);
    let without_key = fixture.service.create(new_approval(None)).unwrap();
    assert_ne!(without_key.approval_id, created.approval_id);

    let stored = fixture.service.get(&created.approval_id).unwrap();
    assert_eq!(stored.connection, Some(network()));
    assert_eq!(
        fixture
            .service
            .list(Some(ApprovalStatus::Pending), 10)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        fixture.service.get("apr_missing").unwrap_err(),
        ApprovalError::NotFound
    );
}

#[test]
fn request_ids_are_isolated_by_trusted_owner_and_changed_content_conflicts() {
    let fixture = Fixture::new();
    let first = fixture.service.create(new_approval(Some("same"))).unwrap();
    let mut other = new_approval(Some("same"));
    other.owner = Some("other-device".into());
    let second = fixture.service.create(other).unwrap();
    assert_ne!(first.approval_id, second.approval_id);
    let mut changed = new_approval(Some("same"));
    changed.target_record = "wf_other".into();
    assert_eq!(
        fixture.service.create(changed).unwrap_err(),
        ApprovalError::RequestConflict
    );
    let mut changed = new_approval(Some("same"));
    changed.request.insert("power".into(), json!(999));
    assert_eq!(
        fixture.service.create(changed).unwrap_err(),
        ApprovalError::RequestConflict
    );
    let mut unknown = new_approval(Some("same"));
    unknown.owner = None;
    let a = fixture.service.create(unknown.clone()).unwrap();
    let b = fixture.service.create(unknown).unwrap();
    assert_ne!(
        a.approval_id, b.approval_id,
        "unidentified bridge peers never share approvals"
    );
    fixture.advance(121.0);
    assert_eq!(
        fixture
            .service
            .create(new_approval(Some("same")))
            .unwrap()
            .status,
        ApprovalStatus::Expired
    );
    assert!(
        fixture
            .service
            .create(new_approval(Some(&"x".repeat(129))))
            .is_err()
    );
}

#[test]
fn revoking_voice_expires_only_its_open_approvals_and_blocks_new_ones() {
    let fixture = Fixture::new();
    for d in ["a", "b"] {
        let db = fixture.db.lock().unwrap();
        db.voice_device_seen(d, "client", "", "", "", 1000.0)
            .unwrap();
        db.set_voice_device_status(d, laser_runtime::VOICE_DEVICE_ALLOWED, 1000.0)
            .unwrap();
    }
    let make = |d: &str, id: &str| {
        let mut n = new_approval(Some(id));
        n.source = ApprovalSource::Voice;
        n.voice_permit = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
        n.session_id = Some(d.into());
        n.owner = Some(d.into());
        n
    };
    let pending = fixture.service.create(make("a", "1")).unwrap();
    let approved = fixture.service.create(make("a", "2")).unwrap();
    fixture
        .service
        .approve(&approved.approval_id, &approved.digest, &approved.digest)
        .unwrap();
    let other = fixture.service.create(make("b", "1")).unwrap();
    fixture
        .db
        .lock()
        .unwrap()
        .set_voice_device_status("a", laser_runtime::VOICE_DEVICE_REVOKED, 1001.0)
        .unwrap();
    for a in [&pending, &approved] {
        assert_eq!(
            fixture.service.get(&a.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
    }
    assert_eq!(
        fixture.service.get(&other.approval_id).unwrap().status,
        ApprovalStatus::Pending
    );
    assert!(fixture.service.create(make("a", "3")).is_err());
    fixture
        .db
        .lock()
        .unwrap()
        .set_voice_device_status("a", laser_runtime::VOICE_DEVICE_ALLOWED, 1002.0)
        .unwrap();
    let mut stale = make("a", "queued");
    stale.voice_permit = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    assert_eq!(
        fixture.service.create(stale).unwrap_err(),
        ApprovalError::Expired,
        "re-allow cannot revive work from a revoked session"
    );
    assert_eq!(
        fixture
            .service
            .consume(&approved.approval_id, &approved.digest)
            .unwrap_err(),
        ApprovalError::Expired
    );
}

#[test]
fn approve_then_consume_once() {
    let fixture = Fixture::new();
    let created = fixture.service.create(new_approval(None)).unwrap();
    let digest = created.digest.clone();
    assert_eq!(
        fixture
            .service
            .consume(&created.approval_id, &digest)
            .unwrap_err(),
        ApprovalError::NotApproved
    );

    fixture.advance(5.0);
    let approved = fixture
        .service
        .approve(&created.approval_id, &digest, &digest)
        .unwrap();
    assert_eq!(approved.status, ApprovalStatus::Approved);
    assert_eq!(approved.decided_at, Some(1005.0));
    assert_eq!(
        fixture
            .service
            .approve(&created.approval_id, &digest, &digest)
            .unwrap_err(),
        ApprovalError::Consumed
    );

    let executed = fixture
        .service
        .consume(&created.approval_id, &digest)
        .unwrap();
    assert_eq!(executed.status, ApprovalStatus::Executed);
    assert_eq!(executed.connection, Some(network()));
    // A-23：重复使用
    let error = fixture
        .service
        .consume(&created.approval_id, &digest)
        .unwrap_err();
    assert_eq!(error, ApprovalError::Consumed);
    assert_eq!(error.code(), "approval_consumed");
    assert_eq!(error.http_status(), 409);

    let done = fixture
        .service
        .record_outcome(
            &created.approval_id,
            ApprovalOutcome {
                success: true,
                message: "已开始发送".into(),
                job_id: Some("wf_test".into()),
                error_code: None,
                ..ApprovalOutcome::default()
            },
        )
        .unwrap();
    assert_eq!(done.status, ApprovalStatus::Executed);
    assert_eq!(done.outcome.unwrap().job_id.as_deref(), Some("wf_test"));
}

/// A-22：批准后重新预览（摘要变化）再执行 → approval_mismatch，授权作废，不可再用。
#[test]
fn changed_content_after_approval_is_a_mismatch() {
    let fixture = Fixture::new();
    let created = fixture.service.create(new_approval(None)).unwrap();
    let digest = created.digest.clone();
    let regenerated = approval_digest(
        ApprovalKind::WorkflowConfirmSend,
        "wf_test",
        &record_digest_facts(&workflow_record(&"b".repeat(64)), &Map::new()),
        &network(),
    );
    // 批准时卡片摘要或当前状态不一致：不批准，状态不变
    assert_eq!(
        fixture
            .service
            .approve(&created.approval_id, "0", &digest)
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    assert_eq!(
        fixture
            .service
            .approve(&created.approval_id, &digest, &regenerated)
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    assert_eq!(
        fixture.service.get(&created.approval_id).unwrap().status,
        ApprovalStatus::Pending
    );

    fixture
        .service
        .approve(&created.approval_id, &digest, &digest)
        .unwrap();
    let error = fixture
        .service
        .consume(&created.approval_id, &regenerated)
        .unwrap_err();
    assert_eq!(error.code(), "approval_mismatch");
    let failed = fixture.service.get(&created.approval_id).unwrap();
    assert_eq!(failed.status, ApprovalStatus::Failed);
    assert_eq!(
        failed.outcome.unwrap().error_code.as_deref(),
        Some("approval_mismatch")
    );
    assert_eq!(
        fixture
            .service
            .consume(&created.approval_id, &digest)
            .unwrap_err(),
        ApprovalError::Consumed
    );
}

/// A-23：授权过期。
#[test]
fn expired_approvals_cannot_be_approved_or_executed() {
    let fixture = Fixture::new();
    let pending = fixture
        .service
        .create(new_approval(Some("late-approve")))
        .unwrap();
    let approved = fixture
        .service
        .create(new_approval(Some("late-execute")))
        .unwrap();
    fixture
        .service
        .approve(&approved.approval_id, &approved.digest, &approved.digest)
        .unwrap();

    fixture.advance(120.0);
    let error = fixture
        .service
        .approve(&pending.approval_id, &pending.digest, &pending.digest)
        .unwrap_err();
    assert_eq!(error, ApprovalError::Expired);
    assert_eq!(error.http_status(), 410);
    assert_eq!(
        fixture
            .service
            .consume(&approved.approval_id, &approved.digest)
            .unwrap_err(),
        ApprovalError::Expired
    );
    assert_eq!(
        fixture.service.get(&pending.approval_id).unwrap().status,
        ApprovalStatus::Expired
    );
    assert_eq!(
        fixture.service.get(&approved.approval_id).unwrap().status,
        ApprovalStatus::Expired
    );
    assert!(
        fixture
            .service
            .list(Some(ApprovalStatus::Pending), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn list_and_get_expire_due_requests() {
    let fixture = Fixture::new();
    let created = fixture.service.create(new_approval(None)).unwrap();
    fixture.advance(119.0);
    assert_eq!(
        fixture
            .service
            .list(Some(ApprovalStatus::Pending), 10)
            .unwrap()
            .len(),
        1
    );
    fixture.advance(1.0);
    assert_eq!(fixture.service.expire_due().unwrap(), 1);
    assert_eq!(
        fixture.service.get(&created.approval_id).unwrap().status,
        ApprovalStatus::Expired
    );
}

#[test]
fn rejected_requests_stay_rejected() {
    let fixture = Fixture::new();
    let created = fixture.service.create(new_approval(None)).unwrap();
    let rejected = fixture
        .service
        .reject(&created.approval_id, Some("材料没放好"))
        .unwrap();
    assert_eq!(rejected.status, ApprovalStatus::Rejected);
    assert_eq!(rejected.decided_at, Some(1000.0));
    assert_eq!(rejected.outcome.unwrap().message, "材料没放好");
    assert_eq!(
        fixture
            .service
            .approve(&created.approval_id, &created.digest, &created.digest)
            .unwrap_err(),
        ApprovalError::Rejected
    );
    assert_eq!(
        fixture
            .service
            .consume(&created.approval_id, &created.digest)
            .unwrap_err(),
        ApprovalError::Rejected
    );
    assert_eq!(
        fixture
            .service
            .reject(&created.approval_id, None)
            .unwrap_err(),
        ApprovalError::Rejected
    );
}

/// A-32：批准后、设备阶段前进程崩溃 → 重启后过期；同一工作流可以重新发起并批准。
#[test]
fn restart_expires_open_approvals_and_workflow_can_be_reapproved() {
    let fixture = Fixture::new();
    let approved = fixture
        .service
        .create(new_approval(Some("before-crash")))
        .unwrap();
    fixture
        .service
        .approve(&approved.approval_id, &approved.digest, &approved.digest)
        .unwrap();
    let pending = fixture
        .service
        .create(new_approval(Some("pending")))
        .unwrap();
    let executed = fixture
        .service
        .create(new_approval(Some("executed")))
        .unwrap();
    fixture
        .service
        .approve(&executed.approval_id, &executed.digest, &executed.digest)
        .unwrap();
    let consumed_executed = fixture
        .service
        .consume(&executed.approval_id, &executed.digest)
        .unwrap();
    assert_eq!(
        consumed_executed.outcome.as_ref().and_then(|o| o.phase),
        Some(ApprovalPhase::Running),
        "物理审批一旦消费必须立即原子写入 running 阶段"
    );
    // 已记录终态 Completed 的已执行审批在重启后保持 Executed；尚处于 running 的另行恢复为 Failed(Unknown)
    fixture
        .service
        .record_outcome(
            &executed.approval_id,
            ApprovalOutcome {
                success: true,
                message: "已完成".into(),
                phase: Some(ApprovalPhase::Completed),
                ..ApprovalOutcome::default()
            },
        )
        .unwrap();
    let crashed_mid_action = fixture
        .service
        .create(new_approval(Some("crashed-mid-action")))
        .unwrap();
    fixture
        .service
        .approve(
            &crashed_mid_action.approval_id,
            &crashed_mid_action.digest,
            &crashed_mid_action.digest,
        )
        .unwrap();
    fixture
        .service
        .consume(&crashed_mid_action.approval_id, &crashed_mid_action.digest)
        .unwrap();

    assert_eq!(fixture.service.recover_after_restart().unwrap(), 3);
    assert_eq!(
        fixture.service.get(&approved.approval_id).unwrap().status,
        ApprovalStatus::Expired
    );
    assert_eq!(
        fixture.service.get(&pending.approval_id).unwrap().status,
        ApprovalStatus::Expired
    );
    assert_eq!(
        fixture.service.get(&executed.approval_id).unwrap().status,
        ApprovalStatus::Executed
    );
    let crashed = fixture
        .service
        .get(&crashed_mid_action.approval_id)
        .unwrap();
    assert_eq!(crashed.status, ApprovalStatus::Failed);
    assert_eq!(
        crashed.outcome.as_ref().and_then(|o| o.phase),
        Some(ApprovalPhase::Unknown)
    );
    assert_eq!(
        fixture
            .service
            .consume(&approved.approval_id, &approved.digest)
            .unwrap_err(),
        ApprovalError::Expired
    );

    let again = fixture
        .service
        .create(new_approval(Some("after-restart")))
        .unwrap();
    assert_eq!(again.digest, approved.digest);
    fixture
        .service
        .approve(&again.approval_id, &again.digest, &again.digest)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .consume(&again.approval_id, &again.digest)
            .unwrap()
            .status,
        ApprovalStatus::Executed
    );
}

#[test]
fn failed_execution_is_not_reusable() {
    let fixture = Fixture::new();
    let created = fixture.service.create(new_approval(None)).unwrap();
    fixture
        .service
        .approve(&created.approval_id, &created.digest, &created.digest)
        .unwrap();
    fixture
        .service
        .consume(&created.approval_id, &created.digest)
        .unwrap();
    let failed = fixture
        .service
        .record_outcome(
            &created.approval_id,
            ApprovalOutcome {
                success: false,
                message: "加工前检查未通过".into(),
                job_id: None,
                error_code: Some("device_preflight_failed".into()),
                ..ApprovalOutcome::default()
            },
        )
        .unwrap();
    assert_eq!(failed.status, ApprovalStatus::Failed);
    assert_eq!(
        fixture
            .service
            .consume(&created.approval_id, &created.digest)
            .unwrap_err(),
        ApprovalError::Consumed
    );
    let pending = fixture.service.create(new_approval(None)).unwrap();
    assert_eq!(
        fixture
            .service
            .record_outcome(
                &pending.approval_id,
                ApprovalOutcome {
                    success: true,
                    message: String::new(),
                    job_id: None,
                    error_code: None,
                    ..ApprovalOutcome::default()
                },
            )
            .unwrap_err(),
        ApprovalError::NotApproved
    );
}

/// P1-08：审批每个状态变化都留审计；模型自填的 confirmed 单独记一条。
#[test]
fn approval_lifecycle_is_audited() {
    let fixture = Fixture::new();
    let mut request = new_approval(Some("audit-1"));
    request.removed_fields = vec!["confirmed".into()];
    let created = fixture.service.create(request).unwrap();
    fixture
        .service
        .approve(&created.approval_id, &created.digest, &created.digest)
        .unwrap();
    fixture
        .service
        .consume(&created.approval_id, &created.digest)
        .unwrap();
    fixture
        .service
        .record_outcome(
            &created.approval_id,
            ApprovalOutcome {
                success: false,
                message: "设备未就绪".into(),
                job_id: None,
                error_code: Some("device_not_ready".into()),
                ..ApprovalOutcome::default()
            },
        )
        .unwrap();
    let rejected = fixture
        .service
        .create(new_approval(Some("audit-2")))
        .unwrap();
    fixture
        .service
        .reject(&rejected.approval_id, Some("不做了"))
        .unwrap();
    let expiring = fixture
        .service
        .create(new_approval(Some("audit-3")))
        .unwrap();
    fixture.advance(120.0);
    fixture.service.expire_due().unwrap();
    assert_eq!(
        fixture.audit_actions(),
        vec![
            AuditAction::ApprovalCreated,
            AuditAction::ConfirmedFieldStripped,
            AuditAction::ApprovalApproved,
            AuditAction::ApprovalExecuted,
            AuditAction::ApprovalFailed,
            AuditAction::ApprovalCreated,
            AuditAction::ApprovalRejected,
            AuditAction::ApprovalCreated,
            AuditAction::ApprovalExpired,
        ]
    );
    let entries = fixture.db.lock().unwrap().recent_audit(100).unwrap();
    let expired = &entries[0];
    assert_eq!(expired.client, "system");
    assert_eq!(
        expired.detail.approval_id.as_deref(),
        Some(expiring.approval_id.as_str())
    );
    let stripped = entries
        .iter()
        .find(|entry| entry.action == AuditAction::ConfirmedFieldStripped)
        .unwrap();
    assert_eq!(stripped.client, "mcp");
    assert_eq!(
        stripped.detail.removed_fields,
        vec!["confirmed".to_string()]
    );
    let approved = entries
        .iter()
        .find(|entry| entry.action == AuditAction::ApprovalApproved)
        .unwrap();
    assert_eq!(approved.client, "desktop");
    assert_eq!(approved.target, "wf_test");
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains("192.0.2.10"), "审计不得包含设备地址: {text}");
}

/// A-34：审计写不进去时，创建、批准、执行一律拒绝且状态不变；拒绝与过期照常生效。
#[test]
fn audit_failure_blocks_r3_but_not_risk_reduction() {
    let fixture = Fixture::new();
    let approved = fixture
        .service
        .create(new_approval(Some("before-1")))
        .unwrap();
    fixture
        .service
        .approve(&approved.approval_id, &approved.digest, &approved.digest)
        .unwrap();
    let pending = fixture
        .service
        .create(new_approval(Some("before-2")))
        .unwrap();
    let to_reject = fixture
        .service
        .create(new_approval(Some("before-3")))
        .unwrap();
    fixture.break_audit_log();

    let error = fixture
        .service
        .create(new_approval(Some("after-1")))
        .unwrap_err();
    assert_eq!(error.code(), "approval_unavailable");
    assert!(error.message().contains("审计"), "{}", error.message());
    assert!(
        fixture
            .service
            .list(None, 10)
            .unwrap()
            .iter()
            .all(|record| record.request_id.as_deref() != Some("after-1"))
    );

    let error = fixture
        .service
        .approve(&pending.approval_id, &pending.digest, &pending.digest)
        .unwrap_err();
    assert_eq!(error.code(), "approval_unavailable");
    assert_eq!(
        fixture.service.get(&pending.approval_id).unwrap().status,
        ApprovalStatus::Pending
    );

    let error = fixture
        .service
        .consume(&approved.approval_id, &approved.digest)
        .unwrap_err();
    assert_eq!(error.code(), "approval_unavailable");
    assert_eq!(
        fixture.service.get(&approved.approval_id).unwrap().status,
        ApprovalStatus::Approved,
        "未执行就不能算已用"
    );

    assert_eq!(
        fixture
            .service
            .reject(&to_reject.approval_id, None)
            .unwrap()
            .status,
        ApprovalStatus::Rejected
    );
    fixture.advance(120.0);
    assert_eq!(fixture.service.expire_due().unwrap(), 2);
    assert_eq!(
        fixture.service.get(&approved.approval_id).unwrap().status,
        ApprovalStatus::Expired
    );
}

/// A-31：服务端宕机期间无人处理的待批准请求到期即过期，之后不能再批准。
#[test]
fn pending_request_expires_while_nobody_answers() {
    let fixture = Fixture::new();
    let created = fixture
        .service
        .create(new_approval(Some("server-down")))
        .unwrap();
    fixture.advance(121.0);
    assert!(
        fixture
            .service
            .list(Some(ApprovalStatus::Pending), 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture.service.get(&created.approval_id).unwrap().status,
        ApprovalStatus::Expired
    );
    assert_eq!(
        fixture
            .service
            .approve(&created.approval_id, &created.digest, &created.digest)
            .unwrap_err(),
        ApprovalError::Expired
    );
    assert_eq!(
        fixture.audit_actions().last(),
        Some(&AuditAction::ApprovalExpired)
    );
}

/// A-33：发送中进程中断 → 重启时审批记为失败、结果未知；授权不能再用，之后的作业回写也不覆盖该结论。
#[test]
fn interrupted_send_is_marked_unknown_on_restart() {
    let fixture = Fixture::new();
    let created = fixture.service.create(new_approval(Some("crash"))).unwrap();
    fixture
        .service
        .approve(&created.approval_id, &created.digest, &created.digest)
        .unwrap();
    fixture
        .service
        .consume(&created.approval_id, &created.digest)
        .unwrap();
    fixture
        .service
        .mark_running(&created.approval_id, "wf_test")
        .unwrap();
    let running = fixture.service.get(&created.approval_id).unwrap();
    assert_eq!(running.outcome.unwrap().phase, Some(ApprovalPhase::Running));

    assert_eq!(fixture.service.recover_after_restart().unwrap(), 1);
    let failed = fixture.service.get(&created.approval_id).unwrap();
    assert_eq!(failed.status, ApprovalStatus::Failed);
    let outcome = failed.outcome.unwrap();
    assert_eq!(outcome.error_code.as_deref(), Some("result_unknown"));
    assert_eq!(outcome.phase, Some(ApprovalPhase::Unknown));
    assert_eq!(outcome.job_id.as_deref(), Some("wf_test"));
    assert!(!outcome.success);
    assert_eq!(
        fixture
            .service
            .consume(&created.approval_id, &created.digest)
            .unwrap_err(),
        ApprovalError::Consumed
    );
    assert_eq!(
        fixture.audit_actions().last(),
        Some(&AuditAction::ApprovalFailed)
    );

    // 迟到的作业回写不覆盖“结果未知”
    fixture.service.record_job_result(
        &created.approval_id,
        "completed",
        &serde_json::json!({"lines_sent": 3, "total_lines": 3}),
    );
    assert_eq!(
        fixture
            .service
            .get(&created.approval_id)
            .unwrap()
            .outcome
            .unwrap()
            .phase,
        Some(ApprovalPhase::Unknown)
    );
    // 再次重启不重复处理
    assert_eq!(fixture.service.recover_after_restart().unwrap(), 0);
}

#[test]
fn job_results_are_written_back_once() {
    let fixture = Fixture::new();
    let mut ids = Vec::new();
    for key in ["done", "cancel"] {
        let created = fixture.service.create(new_approval(Some(key))).unwrap();
        fixture
            .service
            .approve(&created.approval_id, &created.digest, &created.digest)
            .unwrap();
        fixture
            .service
            .consume(&created.approval_id, &created.digest)
            .unwrap();
        fixture
            .service
            .mark_running(&created.approval_id, "wf_test")
            .unwrap();
        ids.push(created.approval_id);
    }
    fixture.service.record_job_result(
        &ids[0],
        "completed",
        &serde_json::json!({"lines_sent": 120, "total_lines": 120}),
    );
    let done = fixture.service.get(&ids[0]).unwrap();
    assert_eq!(done.status, ApprovalStatus::Executed);
    let outcome = done.outcome.unwrap();
    assert!(outcome.success);
    assert_eq!(outcome.phase, Some(ApprovalPhase::Completed));
    assert!(outcome.message.contains("120/120"), "{}", outcome.message);
    assert_eq!(outcome.job_id.as_deref(), Some("wf_test"));
    // 已结束的不再改动
    fixture
        .service
        .record_job_result(&ids[0], "failed", &serde_json::json!({"error": "late"}));
    assert_eq!(
        fixture.service.get(&ids[0]).unwrap().outcome.unwrap().phase,
        Some(ApprovalPhase::Completed)
    );

    fixture.service.record_job_result(
        &ids[1],
        "cancelled",
        &serde_json::json!({"lines_sent": 10, "total_lines": 120, "error": "用户急停"}),
    );
    let cancelled = fixture.service.get(&ids[1]).unwrap();
    assert_eq!(cancelled.status, ApprovalStatus::Failed);
    let outcome = cancelled.outcome.unwrap();
    assert_eq!(outcome.phase, Some(ApprovalPhase::Cancelled));
    assert_eq!(outcome.error_code.as_deref(), Some("cancelled"));
    assert_eq!(outcome.message, "用户急停");
    // 已结束的作业不会在重启时被误判为“结果未知”
    assert_eq!(fixture.service.recover_after_restart().unwrap(), 0);
}

#[test]
fn ttl_and_input_validation() {
    let fixture = Fixture::new();
    assert_eq!(fixture.service.ttl_seconds(), 120);
    let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
    assert_eq!(
        ApprovalService::new(db.clone())
            .with_ttl_seconds(5)
            .ttl_seconds(),
        30
    );
    assert_eq!(
        ApprovalService::new(db)
            .with_ttl_seconds(5000)
            .ttl_seconds(),
        600
    );
    let mut missing = new_approval(None);
    missing.target_record = " ".into();
    assert_eq!(
        fixture.service.create(missing).unwrap_err().code(),
        "approval_invalid_request"
    );
}

#[test]
fn consume_sets_running_and_restart_recovers_missing_outcome_executed_records() {
    let fixture = Fixture::new();
    // 1. SafeAction / Framing 等所有物理类型在 consume 当刻原子写入 phase=running
    let mut safe_req = new_approval(Some("safe-home"));
    safe_req.kind = ApprovalKind::SafeAction;
    let safe_rec = fixture.service.create(safe_req).unwrap();
    fixture
        .service
        .approve(&safe_rec.approval_id, &safe_rec.digest, &safe_rec.digest)
        .unwrap();
    let consumed_safe = fixture
        .service
        .consume(&safe_rec.approval_id, &safe_rec.digest)
        .unwrap();
    assert_eq!(
        consumed_safe.outcome.as_ref().and_then(|o| o.phase),
        Some(ApprovalPhase::Running)
    );

    // 2. mark_running 遇到不存在的 ID（影响 0 行）必须报错而非静默返回 Ok(())
    let mark_err = fixture
        .service
        .mark_running("ap_nonexistent", "wf-x")
        .unwrap_err();
    assert_eq!(mark_err, ApprovalError::NotFound);

    // 3. 遗留/异常导致 Executed 但 outcome=NULL 的记录在重启恢复时也必须转为 Failed(Unknown)
    let raw_executed = fixture
        .service
        .create(new_approval(Some("null-outcome")))
        .unwrap();
    fixture
        .service
        .approve(
            &raw_executed.approval_id,
            &raw_executed.digest,
            &raw_executed.digest,
        )
        .unwrap();
    {
        let now = *fixture.clock.lock().unwrap();
        let db = fixture.db.lock().unwrap();
        assert!(db.consume_approval(&raw_executed.approval_id, now).unwrap());
    }
    assert_eq!(
        fixture
            .service
            .get(&raw_executed.approval_id)
            .unwrap()
            .status,
        ApprovalStatus::Executed
    );
    assert_eq!(fixture.service.recover_after_restart().unwrap(), 2);
    for id in [&safe_rec.approval_id, &raw_executed.approval_id] {
        let rec = fixture.service.get(id).unwrap();
        assert_eq!(rec.status, ApprovalStatus::Failed, "{id}");
        assert_eq!(
            rec.outcome.as_ref().and_then(|o| o.phase),
            Some(ApprovalPhase::Unknown),
            "{id}"
        );
    }
}
