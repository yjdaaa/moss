//! Agent 运行（NV13）与家居控制（NV15）专用审批的单元测试。

use super::*;

#[test]
fn nv13_agent_run_approval_consumed_once_with_durable_record_and_revoke_gate() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new();
    {
        let db = fixture.db.lock().unwrap();
        db.voice_device_seen("device-nv13", "client-nv13", "", "", "", 995.0)
            .unwrap();
        db.set_voice_device_status("device-nv13", laser_runtime::VOICE_DEVICE_ALLOWED, 996.0)
            .unwrap();
    }
    let permit = Arc::new(AtomicBool::new(true));
    let connection = ApprovalConnection::Agent {
        provider: "codex".into(),
        fingerprint: "c".repeat(64),
    };
    let request = request(
        json!({"task":"读取说明文件并总结","provider":"codex","workspace":"fake-workspace","device_id":"device-nv13","voice_session_id":"session-nv13","config_revision":"b".repeat(64)}),
    );
    let created = fixture
        .service
        .create(NewApproval {
            owner: Some("nv13-test-device".into()),
            voice_permit: Some(permit.clone()),
            kind: ApprovalKind::AgentRun,
            target_record: "agent_codex".into(),
            source: ApprovalSource::Voice,
            session_id: Some("device-nv13".into()),
            request_id: None,
            summary: "Codex 只读任务，需要桌面批准".into(),
            facts: request.clone(),
            connection,
            request,
            client: "voice:device-nv13".into(),
            removed_fields: Vec::new(),
        })
        .unwrap();
    assert_eq!(created.status, ApprovalStatus::Pending);
    let base = laser_runtime::AgentRunRecord {
        id: "run-nv13".into(),
        approval_id: created.approval_id.clone(),
        provider: "codex".into(),
        workspace: "fake-workspace".into(),
        task_digest: "a".repeat(64),
        config_revision: "b".repeat(64),
        status: "running".into(),
        output: String::new(),
        truncated: false,
        started_at: 1000.0,
        finished_at: None,
        error_code: None,
    };
    assert_eq!(
        fixture
            .service
            .consume_agent_run(&created.approval_id, &created.digest, &base, &permit)
            .unwrap_err(),
        ApprovalError::NotApproved
    );
    fixture
        .service
        .approve(&created.approval_id, &created.digest, &created.digest)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .consume_agent_run(&created.approval_id, "wrong", &base, &permit)
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    let started = fixture
        .service
        .consume_agent_run(&created.approval_id, &created.digest, &base, &permit)
        .unwrap();
    assert_eq!(started.status, ApprovalStatus::Executed);
    assert_eq!(started.outcome.unwrap().phase, Some(ApprovalPhase::Running));
    assert_eq!(
        fixture
            .service
            .consume_agent_run(&created.approval_id, &created.digest, &base, &permit)
            .unwrap_err(),
        ApprovalError::Consumed
    );
    assert_eq!(fixture.db.lock().unwrap().agent_runs(50).unwrap().len(), 1);
    assert_eq!(
        approval_view(&created)["agent"]["permission"],
        "workspace_write"
    );
    assert!(
        fixture
            .service
            .finish_agent_run("run-nv13", "completed", "任务完成", false, None)
            .unwrap()
    );
    assert_eq!(
        fixture
            .service
            .get(&created.approval_id)
            .unwrap()
            .outcome
            .unwrap()
            .message,
        "Agent 任务执行完成"
    );
    assert_eq!(
        fixture
            .db
            .lock()
            .unwrap()
            .find_agent_run("run-nv13")
            .unwrap()
            .unwrap()
            .status,
        "completed"
    );
    assert!(
        fixture
            .service
            .finish_agent_run("run-nv13", "completed", "retry", false, None)
            .is_err()
    );

    let revoked = fixture
        .service
        .create(NewApproval {
            owner: Some("nv13-test-device".into()),
            voice_permit: Some(permit.clone()),
            kind: ApprovalKind::AgentRun,
            target_record: "agent_codex".into(),
            source: ApprovalSource::Voice,
            session_id: Some("device-nv13".into()),
            request_id: None,
            summary: "第二次只读任务".into(),
            facts: Map::new(),
            connection: ApprovalConnection::Agent {
                provider: "codex".into(),
                fingerprint: "d".repeat(64),
            },
            request: Map::new(),
            client: "voice:device-nv13".into(),
            removed_fields: Vec::new(),
        })
        .unwrap();
    fixture
        .service
        .approve(&revoked.approval_id, &revoked.digest, &revoked.digest)
        .unwrap();
    permit.store(false, Ordering::Release);
    let mut second = base.clone();
    second.id = "run-revoked".into();
    second.approval_id = revoked.approval_id.clone();
    assert_eq!(
        fixture
            .service
            .consume_agent_run(&revoked.approval_id, &revoked.digest, &second, &permit)
            .unwrap_err(),
        ApprovalError::Expired
    );
    assert!(
        fixture
            .db
            .lock()
            .unwrap()
            .find_agent_run("run-revoked")
            .unwrap()
            .is_none()
    );
}

fn fake_home_request(permit: Arc<std::sync::atomic::AtomicBool>, fingerprint: &str) -> NewApproval {
    let mut facts = Map::new();
    facts.insert("action".into(), json!("turn_on"));
    facts.insert("entity_id".into(), json!("light.desk"));
    facts.insert("policy_revision".into(), json!(7));
    facts.insert("credential_revision".into(), json!(9));
    facts.insert("service_path".into(), json!("/api/services/light/turn_on"));
    NewApproval {
        owner: Some("nv15-mock-desktop".into()),
        voice_permit: Some(permit),
        kind: ApprovalKind::HomeControl,
        target_record: "light.desk".into(),
        source: ApprovalSource::Voice,
        session_id: Some("nv15-device".into()),
        request_id: None,
        summary: "Home Assistant：打开书桌台灯，需要独立桌面授权".into(),
        facts: facts.clone(),
        connection: ApprovalConnection::Home {
            fingerprint: fingerprint.into(),
        },
        request: facts,
        client: "voice:nv15-device".into(),
        removed_fields: Vec::new(),
    }
}

fn fake_home_fixture() -> (Fixture, Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::AtomicBool;
    let fixture = Fixture::new();
    {
        let db = fixture.db.lock().unwrap();
        db.voice_device_seen("nv15-device", "nv15-test-client", "", "", "", 995.0)
            .unwrap();
        db.set_voice_device_status("nv15-device", laser_runtime::VOICE_DEVICE_ALLOWED, 996.0)
            .unwrap();
    }
    (fixture, Arc::new(AtomicBool::new(true)))
}

#[test]
fn nv15_independent_home_approvals_and_terminal_result_audits_are_atomic() {
    let (fixture, permit) = fake_home_fixture();
    let fp = "a".repeat(64);
    let first = fixture
        .service
        .create(fake_home_request(permit.clone(), &fp))
        .unwrap();
    let second_fp = "b".repeat(64);
    let second = fixture
        .service
        .create(fake_home_request(permit.clone(), &second_fp))
        .unwrap();
    assert_ne!(first.digest, second.digest);
    assert_ne!(first.approval_id, second.approval_id);
    fixture
        .service
        .approve(&first.approval_id, &first.digest, &first.digest)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &second.approval_id,
                &second.digest,
                &second_fp,
                "nv15-device",
                &permit
            )
            .unwrap_err(),
        ApprovalError::NotApproved
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &first.approval_id,
                &second.digest,
                &fp,
                "nv15-device",
                &permit
            )
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &first.approval_id,
                &first.digest,
                &fp,
                "nv15-other-device",
                &permit
            )
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    fixture
        .service
        .consume_home_control(
            &first.approval_id,
            &first.digest,
            &fp,
            "nv15-device",
            &permit,
        )
        .unwrap();
    let before = fixture.db.lock().unwrap().recent_audit(80).unwrap().len();
    let confirmed = fixture
        .service
        .record_outcome(
            &first.approval_id,
            ApprovalOutcome {
                success: true,
                message: "已确认合成台灯状态".into(),
                phase: Some(ApprovalPhase::Completed),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(confirmed.status, ApprovalStatus::Executed);
    assert_eq!(
        confirmed.outcome.unwrap().phase,
        Some(ApprovalPhase::Completed)
    );
    let audits = fixture.db.lock().unwrap().recent_audit(80).unwrap();
    assert_eq!(audits.len(), before + 1);
    assert_eq!(audits[0].action, AuditAction::ApprovalExecuted);
    assert_eq!(
        audits[0].detail.approval_id.as_deref(),
        Some(first.approval_id.as_str())
    );
    assert_eq!(audits[0].detail.reason.as_deref(), Some("home_confirmed"));
    assert_eq!(audits[0].result, AuditResult::Ok);
    assert_eq!(
        fixture.service.get(&second.approval_id).unwrap().status,
        ApprovalStatus::Pending
    );
    assert_eq!(
        fixture
            .service
            .record_outcome(
                &first.approval_id,
                ApprovalOutcome {
                    success: false,
                    phase: Some(ApprovalPhase::Unknown),
                    message: "late unknown".into(),
                    ..Default::default()
                }
            )
            .unwrap_err(),
        ApprovalError::Consumed
    );

    fixture
        .service
        .approve(&second.approval_id, &second.digest, &second.digest)
        .unwrap();
    fixture
        .service
        .consume_home_control(
            &second.approval_id,
            &second.digest,
            &second_fp,
            "nv15-device",
            &permit,
        )
        .unwrap();
    let unknown = fixture
        .service
        .record_outcome(
            &second.approval_id,
            ApprovalOutcome {
                success: false,
                message: "模拟网络确认失败，不重发".into(),
                error_code: Some("result_unknown".into()),
                phase: Some(ApprovalPhase::Unknown),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(unknown.status, ApprovalStatus::Failed);
    assert_eq!(unknown.outcome.unwrap().phase, Some(ApprovalPhase::Unknown));
    let audits = fixture.db.lock().unwrap().recent_audit(80).unwrap();
    assert_eq!(audits[0].action, AuditAction::ApprovalFailed);
    assert_eq!(
        audits[0].detail.reason.as_deref(),
        Some("home_result_unknown")
    );
    assert_eq!(
        audits[0].detail.approval_id.as_deref(),
        Some(second.approval_id.as_str())
    );
    assert_eq!(
        fixture
            .service
            .record_outcome(
                &second.approval_id,
                ApprovalOutcome {
                    success: true,
                    phase: Some(ApprovalPhase::Completed),
                    message: "伪造的迟到成功".into(),
                    ..Default::default()
                }
            )
            .unwrap_err(),
        ApprovalError::Consumed
    );
}

#[test]
fn nv15_audit_failure_rolls_back_terminal_outcome_and_never_allows_replay() {
    let (fixture, permit) = fake_home_fixture();
    let fp = "f".repeat(64);
    let approval = fixture
        .service
        .create(fake_home_request(permit.clone(), &fp))
        .unwrap();
    fixture
        .service
        .approve(&approval.approval_id, &approval.digest, &approval.digest)
        .unwrap();
    fixture
        .service
        .consume_home_control(
            &approval.approval_id,
            &approval.digest,
            &fp,
            "nv15-device",
            &permit,
        )
        .unwrap();
    {
        let db = fixture.db.lock().unwrap();
        db.test_execute_batch(
            "CREATE TRIGGER deny_home_terminal_audit BEFORE INSERT ON audit_log
             WHEN NEW.action='approval_failed'
             BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END;",
        )
        .unwrap();
    }
    assert!(
        fixture
            .service
            .record_outcome(
                &approval.approval_id,
                ApprovalOutcome {
                    success: false,
                    message: "模拟设备状态未知".into(),
                    error_code: Some("result_unknown".into()),
                    phase: Some(ApprovalPhase::Unknown),
                    ..Default::default()
                }
            )
            .is_err()
    );
    let unchanged = fixture.service.get(&approval.approval_id).unwrap();
    assert_eq!(unchanged.status, ApprovalStatus::Executed);
    assert_eq!(
        unchanged.outcome.unwrap().phase,
        Some(ApprovalPhase::Running)
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fp,
                "nv15-device",
                &permit
            )
            .unwrap_err(),
        ApprovalError::Consumed
    );
    {
        let db = fixture.db.lock().unwrap();
        db.test_execute_batch("DROP TRIGGER deny_home_terminal_audit")
            .unwrap();
    }
    fixture.service.recover_after_restart().unwrap();
    let recovered = fixture.service.get(&approval.approval_id).unwrap();
    assert_eq!(recovered.status, ApprovalStatus::Failed);
    assert_eq!(
        recovered.outcome.unwrap().phase,
        Some(ApprovalPhase::Unknown)
    );
}
#[test]
fn nv15_home_control_private_cancel_cannot_cross_approval_types_or_replay() {
    let (fixture, permit) = fake_home_fixture();
    let fp = "a".repeat(64);
    let pending = fixture
        .service
        .create(fake_home_request(permit.clone(), &fp))
        .unwrap();
    assert_eq!(
        fixture
            .service
            .cancel_home_control(&pending.approval_id, "wrong")
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    assert_eq!(
        fixture.service.get(&pending.approval_id).unwrap().status,
        ApprovalStatus::Pending
    );
    let cancelled = fixture
        .service
        .cancel_home_control(&pending.approval_id, &pending.digest)
        .unwrap();
    assert_eq!(cancelled.status, ApprovalStatus::Rejected);
    assert_eq!(
        cancelled.outcome.unwrap().phase,
        Some(ApprovalPhase::Cancelled)
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &pending.approval_id,
                &pending.digest,
                &fp,
                "nv15-device",
                &permit
            )
            .unwrap_err(),
        ApprovalError::Rejected
    );
    assert_eq!(
        fixture
            .service
            .cancel_home_control(&pending.approval_id, &pending.digest)
            .unwrap_err(),
        ApprovalError::Rejected
    );
    let next = fixture
        .service
        .create(fake_home_request(permit.clone(), &fp))
        .unwrap();
    let approved = fixture
        .service
        .approve(&next.approval_id, &next.digest, &next.digest)
        .unwrap();
    assert_eq!(approved.status, ApprovalStatus::Approved);
    let cancelled = fixture
        .service
        .cancel_home_control(&next.approval_id, &next.digest)
        .unwrap();
    assert_eq!(cancelled.status, ApprovalStatus::Expired);
    assert_eq!(
        fixture
            .service
            .consume_home_control(&next.approval_id, &next.digest, &fp, "nv15-device", &permit)
            .unwrap_err(),
        ApprovalError::Expired
    );
    let third = fixture
        .service
        .create(fake_home_request(permit.clone(), &fp))
        .unwrap();
    fixture
        .service
        .approve(&third.approval_id, &third.digest, &third.digest)
        .unwrap();
    fixture
        .service
        .consume_home_control(
            &third.approval_id,
            &third.digest,
            &fp,
            "nv15-device",
            &permit,
        )
        .unwrap();
    assert_eq!(
        fixture
            .service
            .cancel_home_control(&third.approval_id, &third.digest)
            .unwrap_err(),
        ApprovalError::Consumed
    );
}

#[test]
fn nv15_home_control_persistent_single_use_never_executes_via_general_approval() {
    let (fixture, permit) = fake_home_fixture();
    let fingerprint = "a".repeat(64);
    let approval = fixture
        .service
        .create(fake_home_request(permit.clone(), &fingerprint))
        .unwrap();
    assert_eq!(approval.kind, ApprovalKind::HomeControl);
    assert_eq!(approval.status, ApprovalStatus::Pending);
    let output = serde_json::to_string(&approval_view(&approval)).unwrap();
    assert!(!output.contains(&fingerprint));
    assert!(!output.contains("nv15-fake-token"));
    assert_eq!(
        fixture
            .service
            .consume(&approval.approval_id, &approval.digest)
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fingerprint,
                "nv15-device",
                &permit,
            )
            .unwrap_err(),
        ApprovalError::NotApproved
    );
    fixture
        .service
        .approve(&approval.approval_id, &approval.digest, &approval.digest)
        .unwrap();
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &"b".repeat(64),
                "nv15-device",
                &permit,
            )
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fingerprint,
                "nv15-other-device",
                &permit,
            )
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                "invalid-digest",
                &fingerprint,
                "nv15-device",
                &permit,
            )
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    let consumed = fixture
        .service
        .consume_home_control(
            &approval.approval_id,
            &approval.digest,
            &fingerprint,
            "nv15-device",
            &permit,
        )
        .unwrap();
    assert_eq!(consumed.status, ApprovalStatus::Executed);
    assert_eq!(
        consumed.outcome.as_ref().unwrap().phase,
        Some(ApprovalPhase::Running)
    );
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fingerprint,
                "nv15-device",
                &permit,
            )
            .unwrap_err(),
        ApprovalError::Consumed
    );
    assert_eq!(
        fixture
            .service
            .consume(&approval.approval_id, &approval.digest)
            .unwrap_err(),
        ApprovalError::Mismatch
    );
    let recovered = fixture.service.recover_after_restart().unwrap();
    assert!(recovered >= 1);
    let after = fixture.service.get(&approval.approval_id).unwrap();
    assert_eq!(after.status, ApprovalStatus::Failed);
    assert_eq!(
        after.outcome.as_ref().unwrap().phase,
        Some(ApprovalPhase::Unknown)
    );
    assert!(after.outcome.unwrap().message.contains("Home Assistant"));
    // The fake plan never has a network executor, and no POST exists.
}

#[test]
fn nv15_home_control_fails_closed_on_revocation_expiry_wrong_source_and_identity() {
    use std::sync::atomic::Ordering;
    let (fixture, permit) = fake_home_fixture();
    let fp = "c".repeat(64);
    for modify in 0..5 {
        let mut request = fake_home_request(permit.clone(), &fp);
        match modify {
            0 => request.source = ApprovalSource::McpBridge,
            1 => {
                request.connection = ApprovalConnection::Network {
                    host: "169.254.169.254".into(),
                    port: 80,
                }
            }
            2 => request.target_record = "../../../api/services/lock/unlock".into(),
            3 => request.session_id = None,
            4 => {
                request.connection = ApprovalConnection::Home {
                    fingerprint: "not-a-fingerprint".into(),
                }
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            fixture.service.create(request),
            Err(ApprovalError::InvalidRequest(_))
        ));
    }
    let first = fixture
        .service
        .create(fake_home_request(permit.clone(), &fp))
        .unwrap();
    fixture
        .service
        .approve(&first.approval_id, &first.digest, &first.digest)
        .unwrap();
    permit.store(false, Ordering::Release);
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &first.approval_id,
                &first.digest,
                &fp,
                "nv15-device",
                &permit,
            )
            .unwrap_err(),
        ApprovalError::Expired
    );
    permit.store(true, Ordering::Release);
    let second = fixture
        .service
        .create(fake_home_request(permit.clone(), &fp))
        .unwrap();
    fixture
        .service
        .approve(&second.approval_id, &second.digest, &second.digest)
        .unwrap();
    fixture.advance(121.0);
    assert_eq!(
        fixture
            .service
            .consume_home_control(
                &second.approval_id,
                &second.digest,
                &fp,
                "nv15-device",
                &permit,
            )
            .unwrap_err(),
        ApprovalError::Expired
    );
}
