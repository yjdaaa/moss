//! NV15 one-shot approved control transition. No real transport is registered.
//! The approved plan and caller permit must be derived from a trusted session.
use super::{HomeAction, HomeCatalog, HomeControlPlan};
use laser_api::approval::{ApprovalError, ApprovalService};
use laser_contracts::{ApprovalOutcome, ApprovalPhase};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeDispatchResult {
    Confirmed,
    AcceptedUnconfirmed,
    ResultUnknown,
    CancelledBeforeSend,
}

pub(crate) trait HomeControlTransport {
    /// Any transport error must be treated as possibly sent.
    fn submit_once(&mut self, path: &str, body: &serde_json::Value) -> Result<(), ()>;
    /// An observation must honor the remaining deadline; blocking network
    /// reads must never overrun the caller's total observation budget.
    fn read_target_state(&mut self, entity_id: &str, budget: Duration) -> Result<String, ()>;
    fn read_target_snapshot(
        &mut self,
        entity_id: &str,
        budget: Duration,
    ) -> Result<(String, serde_json::Value), ()> {
        self.read_target_state(entity_id, budget)
            .map(|state| (state, serde_json::Value::Null))
    }
}

enum ExpectedObservation {
    State(&'static str),
    Temperature(f64),
    PlayingMedia(Option<String>),
}

impl ExpectedObservation {
    fn matches(&self, state: &str, attributes: &serde_json::Value) -> bool {
        match self {
            Self::State(expected) => state == *expected,
            Self::Temperature(target) => {
                state != "unavailable"
                    && state != "unknown"
                    && attributes
                        .get("temperature")
                        .and_then(serde_json::Value::as_f64)
                        .is_some_and(|actual| {
                            actual.is_finite() && (actual - *target).abs() <= 0.05
                        })
            }
            Self::PlayingMedia(expected_id) => {
                state == "playing"
                    && expected_id.as_deref().is_some_and(|expected| {
                        attributes
                            .get("media_content_id")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|actual| actual == expected)
                    })
            }
        }
    }
}

fn wait_before_retry(deadline: Instant, permit: &AtomicBool) -> bool {
    let pause_until = (Instant::now() + Duration::from_millis(150)).min(deadline);
    while Instant::now() < pause_until {
        if !permit.load(Ordering::Acquire) || Instant::now() >= deadline {
            return false;
        }
        let step = pause_until
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(25));
        if step.is_zero() {
            break;
        }
        std::thread::sleep(step);
    }
    permit.load(Ordering::Acquire) && Instant::now() < deadline
}

/// The approval is consumed and audited *before* the one possible request.
/// Any uncertainty stays terminal, never triggering another send.
pub(crate) fn dispatch_approved(
    approval: &ApprovalService,
    approval_id: &str,
    device_id: &str,
    fingerprint: &str,
    catalog: &HomeCatalog,
    plan: &HomeControlPlan,
    permit: &Arc<AtomicBool>,
    transport: &mut impl HomeControlTransport,
) -> Result<HomeDispatchResult, ApprovalError> {
    if !plan.requires_dedicated_approval {
        return Err(ApprovalError::Mismatch);
    }
    catalog
        .revalidate_plan(plan, permit)
        .map_err(|_| ApprovalError::Mismatch)?;
    let record = approval.get(approval_id)?;
    // Check approved, frozen action bytes, not only the target. The digest
    // alone may be valid for a different service/brightness under the same ID.
    let expected = serde_json::json!({
        "path":plan.service_path,"body":plan.body,
        "policy":plan.policy_revision,"credential":plan.credential_revision,
    })
    .as_object()
    .cloned()
    .ok_or(ApprovalError::Mismatch)?;
    if record.request.as_ref() != Some(&expected) || record.target_record != plan.entity_id {
        return Err(ApprovalError::Mismatch);
    }
    let consumed = approval.consume_home_control(
        approval_id,
        &record.digest,
        fingerprint,
        device_id,
        permit,
    )?;
    if consumed.target_record != plan.entity_id {
        let _ = approval.record_outcome(
            approval_id,
            ApprovalOutcome {
                success: false,
                message: "家居控制目标不匹配，禁止发送".into(),
                error_code: Some("home_target_mismatch".into()),
                phase: Some(ApprovalPhase::Cancelled),
                ..Default::default()
            },
        );
        return Err(ApprovalError::Mismatch);
    }
    if !permit.load(Ordering::Acquire) || catalog.revalidate_plan(plan, permit).is_err() {
        approval.record_outcome(
            approval_id,
            ApprovalOutcome {
                success: false,
                message: "权限已撤销，未发送家居命令".into(),
                error_code: Some("home_cancelled".into()),
                phase: Some(ApprovalPhase::Cancelled),
                ..Default::default()
            },
        )?;
        return Ok(HomeDispatchResult::CancelledBeforeSend);
    }
    let result = if transport
        .submit_once(&plan.service_path, &plan.body)
        .is_err()
        || !permit.load(Ordering::Acquire)
    {
        HomeDispatchResult::ResultUnknown
    } else {
        let expected = match plan.action {
            HomeAction::TurnOn => Some(ExpectedObservation::State("on")),
            HomeAction::TurnOff => Some(ExpectedObservation::State("off")),
            HomeAction::Lock => Some(ExpectedObservation::State("locked")),
            HomeAction::Unlock => Some(ExpectedObservation::State("unlocked")),
            HomeAction::ArmHome => Some(ExpectedObservation::State("armed_home")),
            HomeAction::ArmAway => Some(ExpectedObservation::State("armed_away")),
            HomeAction::Disarm => Some(ExpectedObservation::State("disarmed")),
            HomeAction::OpenCover => Some(ExpectedObservation::State("open")),
            HomeAction::CloseCover => Some(ExpectedObservation::State("closed")),
            HomeAction::StopCover => Some(ExpectedObservation::State("stopped")),
            HomeAction::PlayMedia => Some(ExpectedObservation::PlayingMedia(
                plan.body
                    .get("media_content_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            )),
            HomeAction::SetTemperature => plan
                .body
                .get("temperature")
                .and_then(serde_json::Value::as_f64)
                .filter(|t| t.is_finite())
                .map(ExpectedObservation::Temperature),
        };
        let mut result = HomeDispatchResult::AcceptedUnconfirmed;
        let deadline = Instant::now() + Duration::from_secs(3);
        if let Some(expected) = expected {
            for attempt in 0..3 {
                if Instant::now() >= deadline || !permit.load(Ordering::Acquire) {
                    result = HomeDispatchResult::ResultUnknown;
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                let budget = remaining.min(Duration::from_millis(900));
                if budget.is_zero() {
                    result = HomeDispatchResult::ResultUnknown;
                    break;
                }
                let observed = transport.read_target_snapshot(&plan.entity_id, budget);
                // Revocation can happen while HTTPS observation is pending.
                // An old read success must not turn revoked work into Completed.
                if !permit.load(Ordering::Acquire) || Instant::now() >= deadline {
                    result = HomeDispatchResult::ResultUnknown;
                    break;
                }
                match observed {
                    Ok((state, attributes)) if expected.matches(&state, &attributes) => {
                        result = HomeDispatchResult::Confirmed;
                        break;
                    }
                    Ok(_) => {
                        if attempt < 2 && !wait_before_retry(deadline, permit) {
                            result = HomeDispatchResult::ResultUnknown;
                            break;
                        }
                    }
                    Err(_) => {
                        result = HomeDispatchResult::ResultUnknown;
                        break;
                    }
                }
            }
        }
        result
    };
    let (success, phase, reason, message) = match result {
        HomeDispatchResult::Confirmed => {
            (true, ApprovalPhase::Completed, None, "目标实体状态已确认")
        }
        HomeDispatchResult::AcceptedUnconfirmed => (
            false,
            ApprovalPhase::Unknown,
            Some("home_state_unconfirmed".into()),
            "服务已接受，但目标状态未确认",
        ),
        HomeDispatchResult::ResultUnknown => (
            false,
            ApprovalPhase::Unknown,
            Some("result_unknown".into()),
            "家居命令执行结果未知，不可重发",
        ),
        HomeDispatchResult::CancelledBeforeSend => unreachable!(),
    };
    approval.record_outcome(
        approval_id,
        ApprovalOutcome {
            success,
            message: message.into(),
            phase: Some(phase),
            error_code: reason,
            ..Default::default()
        },
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::super::{AllowedEntity, HomePolicy};
    use super::*;
    use laser_api::approval::NewApproval;
    use laser_contracts::{ApprovalConnection, ApprovalKind, ApprovalSource, ApprovalStatus};
    use laser_runtime::{RuntimeDb, VOICE_DEVICE_ALLOWED};
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};
    use std::sync::Mutex;
    struct Fake {
        sends: u32,
        reads: u32,
        fail: bool,
    }
    impl HomeControlTransport for Fake {
        fn submit_once(&mut self, path: &str, body: &Value) -> Result<(), ()> {
            assert_eq!(path, "/api/services/light/turn_on");
            assert_eq!(body["entity_id"], "light.desk");
            self.sends += 1;
            if self.fail { Err(()) } else { Ok(()) }
        }
        fn read_target_state(&mut self, id: &str, budget: Duration) -> Result<String, ()> {
            assert_eq!(id, "light.desk");
            assert!(!budget.is_zero() && budget <= Duration::from_millis(900));
            self.reads += 1;
            Ok("on".into())
        }
    }
    fn fixture() -> (
        ApprovalService,
        HomeCatalog,
        HomeControlPlan,
        String,
        String,
        Arc<AtomicBool>,
    ) {
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        {
            let lock = db.lock().unwrap();
            lock.voice_device_seen("nv15-device", "nv15-client", "", "", "", 1.)
                .unwrap();
            lock.set_voice_device_status("nv15-device", VOICE_DEVICE_ALLOWED, 2.)
                .unwrap();
        }
        let service = ApprovalService::new(db).with_clock(Arc::new(|| 10.));
        let catalog = HomeCatalog::from_snapshots(
            br#"[{"entity_id":"light.desk","state":"off","last_updated":"now","attributes":{}}]"#,
            br#"[{"domain":"light","services":{"turn_on":{}}}]"#,
            vec![AllowedEntity {
                entity_id: "light.desk".into(),
                alias: "台灯".into(),
                enabled: true,
                policy: HomePolicy::Ask,
                media: vec![],
            }],
            4,
            5,
        )
        .unwrap();
        let permit = Arc::new(AtomicBool::new(true));
        let plan = catalog
            .plan("light.desk", HomeAction::TurnOn, &json!({}), &permit)
            .unwrap();
        let facts: Map<String, Value> =
            json!({"path":plan.service_path,"body":plan.body,"policy":4,"credential":5})
                .as_object()
                .unwrap()
                .clone();
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&facts).unwrap()));
        let created = service
            .create(NewApproval {
                owner: Some("trusted-device".into()),
                voice_permit: Some(permit.clone()),
                kind: ApprovalKind::HomeControl,
                target_record: "light.desk".into(),
                source: ApprovalSource::Voice,
                session_id: Some("nv15-device".into()),
                request_id: None,
                summary: "合成家居审批".into(),
                facts: facts.clone(),
                connection: ApprovalConnection::Home {
                    fingerprint: fingerprint.clone(),
                },
                request: facts,
                client: "voice:nv15-device".into(),
                removed_fields: vec![],
            })
            .unwrap();
        (
            service,
            catalog,
            plan,
            fingerprint,
            created.approval_id,
            permit,
        )
    }
    #[test]
    fn approved_exactly_once_and_durable_result() {
        let (service, catalog, plan, fp, id, permit) = fixture();
        let mut fake = Fake {
            sends: 0,
            reads: 0,
            fail: false,
        };
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap_err(),
            ApprovalError::NotApproved
        );
        assert_eq!(fake.sends, 0);
        let digest = service.get(&id).unwrap().digest;
        service.approve(&id, &digest, &digest).unwrap();
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap(),
            HomeDispatchResult::Confirmed
        );
        assert_eq!((fake.sends, fake.reads), (1, 1));
        assert_eq!(service.get(&id).unwrap().status, ApprovalStatus::Executed);
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap_err(),
            ApprovalError::Consumed
        );
    }
    #[test]
    fn edited_plan_or_wrong_connection_cannot_consume_approval() {
        let (service, catalog, plan, fp, id, permit) = fixture();
        let digest = service.get(&id).unwrap().digest;
        service.approve(&id, &digest, &digest).unwrap();
        let mut forged = plan.clone();
        forged.body["brightness"] = json!(5);
        let mut fake = Fake {
            sends: 0,
            reads: 0,
            fail: false,
        };
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &forged,
                &permit,
                &mut fake
            )
            .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &"f".repeat(64),
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(fake.sends, 0);
        assert_eq!(service.get(&id).unwrap().status, ApprovalStatus::Approved);
    }
    struct RevokeWhileReading {
        inner: Fake,
        permit: Arc<AtomicBool>,
    }
    impl HomeControlTransport for RevokeWhileReading {
        fn submit_once(&mut self, path: &str, body: &Value) -> Result<(), ()> {
            self.inner.submit_once(path, body)
        }
        fn read_target_state(&mut self, entity_id: &str, budget: Duration) -> Result<String, ()> {
            let state = self.inner.read_target_state(entity_id, budget)?;
            // Simulate a concurrent user revocation while the HTTPS GET was
            // pending. Its stale "on" reply must NEVER mean success.
            self.permit.store(false, Ordering::Release);
            Ok(state)
        }
    }
    #[test]
    fn revoked_during_observation_never_reports_confirmed_or_resends() {
        let (service, catalog, plan, fp, id, permit) = fixture();
        let digest = service.get(&id).unwrap().digest;
        service.approve(&id, &digest, &digest).unwrap();
        let mut fake = RevokeWhileReading {
            inner: Fake {
                sends: 0,
                reads: 0,
                fail: false,
            },
            permit: permit.clone(),
        };
        let result = dispatch_approved(
            &service,
            &id,
            "nv15-device",
            &fp,
            &catalog,
            &plan,
            &permit,
            &mut fake,
        )
        .unwrap();
        assert_eq!(result, HomeDispatchResult::ResultUnknown);
        assert_eq!((fake.inner.sends, fake.inner.reads), (1, 1));
        let recorded = service.get(&id).unwrap();
        assert_eq!(recorded.status, ApprovalStatus::Failed);
        let outcome = recorded.outcome.unwrap();
        assert_eq!(outcome.phase, Some(ApprovalPhase::Unknown));
        assert_eq!(outcome.error_code.as_deref(), Some("result_unknown"));
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(fake.inner.sends, 1, "a revoked request is never repeated");
    }

    struct RevokeAtSimulatedSubmit {
        sends: u32,
        read_count: u32,
        permit: Arc<AtomicBool>,
    }
    impl HomeControlTransport for RevokeAtSimulatedSubmit {
        fn submit_once(&mut self, _: &str, _: &Value) -> Result<(), ()> {
            self.sends += 1;
            // A real remote endpoint might already have received the POST;
            // this cancellation cannot claim it never happened.
            self.permit.store(false, Ordering::Release);
            Ok(())
        }
        fn read_target_state(&mut self, _: &str, _: Duration) -> Result<String, ()> {
            self.read_count += 1;
            Ok("on".into())
        }
    }

    #[test]
    fn nv15_independent_home_approval_simulated_send_revoke_is_unknown_and_not_replayed() {
        let (service, catalog, plan, fp, id, permit) = fixture();
        let digest = service.get(&id).unwrap().digest;
        service.approve(&id, &digest, &digest).unwrap();
        assert_eq!(
            service.consume(&id, &digest).unwrap_err(),
            ApprovalError::Mismatch,
            "general laser approval executor cannot consume HomeControl"
        );
        let mut fake = RevokeAtSimulatedSubmit {
            sends: 0,
            read_count: 0,
            permit: permit.clone(),
        };
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap(),
            HomeDispatchResult::ResultUnknown
        );
        assert_eq!(fake.sends, 1);
        assert_eq!(
            fake.read_count, 0,
            "do not trust observation after revocation"
        );
        let saved = service.get(&id).unwrap();
        assert_eq!(saved.status, ApprovalStatus::Failed);
        let outcome = saved.outcome.unwrap();
        assert_eq!(outcome.phase, Some(ApprovalPhase::Unknown));
        assert_eq!(outcome.error_code.as_deref(), Some("result_unknown"));
        assert!(
            service
                .record_outcome(
                    &id,
                    ApprovalOutcome {
                        success: true,
                        phase: Some(ApprovalPhase::Completed),
                        message: "forged stale confirmation".into(),
                        ..Default::default()
                    }
                )
                .is_err(),
            "late success may not overwrite unknown"
        );
        assert_eq!(
            service
                .consume_home_control(&id, &digest, &fp, "nv15-device", &permit)
                .unwrap_err(),
            ApprovalError::Consumed
        );
    }

    struct StaleStateReplies {
        sends: u32,
        reads: u32,
    }
    impl HomeControlTransport for StaleStateReplies {
        fn submit_once(&mut self, _: &str, _: &Value) -> Result<(), ()> {
            self.sends += 1;
            Ok(())
        }
        fn read_target_state(&mut self, _: &str, budget: Duration) -> Result<String, ()> {
            assert!(!budget.is_zero() && budget <= Duration::from_millis(900));
            self.reads += 1;
            Ok("off".into())
        }
    }
    #[test]
    fn nv15_independent_approval_mock_accepted_not_confirmed_durable_unknown() {
        let (service, catalog, plan, fp, id, permit) = fixture();
        let digest = service.get(&id).unwrap().digest;
        service.approve(&id, &digest, &digest).unwrap();
        let mut fake = StaleStateReplies { sends: 0, reads: 0 };
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap(),
            HomeDispatchResult::AcceptedUnconfirmed
        );
        assert_eq!((fake.sends, fake.reads), (1, 3));
        let saved = service.get(&id).unwrap();
        assert_eq!(saved.status, ApprovalStatus::Failed);
        let outcome = saved.outcome.unwrap();
        assert_eq!(outcome.phase, Some(ApprovalPhase::Unknown));
        assert_eq!(
            outcome.error_code.as_deref(),
            Some("home_state_unconfirmed")
        );
        assert_eq!(
            service.consume(&id, &digest).unwrap_err(),
            ApprovalError::Mismatch
        );
        assert!(
            service
                .record_outcome(
                    &id,
                    ApprovalOutcome {
                        success: true,
                        phase: Some(ApprovalPhase::Completed),
                        message: "fabricated late GET".into(),
                        ..Default::default()
                    }
                )
                .is_err()
        );
        assert_eq!(fake.sends, 1, "no repeated control POST");
    }

    #[test]
    fn failed_transport_unknown_not_replayed_or_reauthorized() {
        let (service, catalog, plan, fp, id, permit) = fixture();
        let digest = service.get(&id).unwrap().digest;
        service.approve(&id, &digest, &digest).unwrap();
        let mut fake = Fake {
            sends: 0,
            reads: 0,
            fail: true,
        };
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap(),
            HomeDispatchResult::ResultUnknown
        );
        assert_eq!(service.get(&id).unwrap().status, ApprovalStatus::Failed);
        assert_eq!(
            service.get(&id).unwrap().outcome.unwrap().phase,
            Some(ApprovalPhase::Unknown)
        );
        assert_eq!(
            dispatch_approved(
                &service,
                &id,
                "nv15-device",
                &fp,
                &catalog,
                &plan,
                &permit,
                &mut fake
            )
            .unwrap_err(),
            ApprovalError::Consumed
        );
        assert_eq!(fake.sends, 1);
    }

    struct PollSequenceTransport {
        expected_path: &'static str,
        expected_entity: &'static str,
        replies: Vec<(String, Value)>,
        sends: usize,
        reads: usize,
    }
    impl HomeControlTransport for PollSequenceTransport {
        fn submit_once(&mut self, path: &str, body: &Value) -> Result<(), ()> {
            assert_eq!(path, self.expected_path);
            assert_eq!(body["entity_id"], self.expected_entity);
            self.sends += 1;
            Ok(())
        }
        fn read_target_state(&mut self, entity_id: &str, budget: Duration) -> Result<String, ()> {
            self.read_target_snapshot(entity_id, budget).map(|(s, _)| s)
        }
        fn read_target_snapshot(
            &mut self,
            entity_id: &str,
            budget: Duration,
        ) -> Result<(String, Value), ()> {
            assert_eq!(entity_id, self.expected_entity);
            assert!(!budget.is_zero() && budget <= Duration::from_millis(900));
            let idx = self.reads.min(self.replies.len().saturating_sub(1));
            self.reads += 1;
            Ok(self.replies[idx].clone())
        }
    }

    fn custom_plan_fixture(
        states_json: &[u8],
        services_json: &[u8],
        allowlist: Vec<AllowedEntity>,
        target: &str,
        action: HomeAction,
        params: Value,
    ) -> (
        ApprovalService,
        HomeCatalog,
        HomeControlPlan,
        String,
        String,
        Arc<AtomicBool>,
    ) {
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        {
            let lock = db.lock().unwrap();
            lock.voice_device_seen("nv15-device", "nv15-client", "", "", "", 1.)
                .unwrap();
            lock.set_voice_device_status("nv15-device", VOICE_DEVICE_ALLOWED, 2.)
                .unwrap();
        }
        let service = ApprovalService::new(db).with_clock(Arc::new(|| 10.));
        let catalog =
            HomeCatalog::from_snapshots(states_json, services_json, allowlist, 4, 5).unwrap();
        let permit = Arc::new(AtomicBool::new(true));
        let plan = catalog.plan(target, action, &params, &permit).unwrap();
        let facts: Map<String, Value> =
            json!({"path":plan.service_path,"body":plan.body,"policy":4,"credential":5})
                .as_object()
                .unwrap()
                .clone();
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&facts).unwrap()));
        let created = service
            .create(NewApproval {
                owner: Some("trusted-device".into()),
                voice_permit: Some(permit.clone()),
                kind: ApprovalKind::HomeControl,
                target_record: plan.entity_id.clone(),
                source: ApprovalSource::Voice,
                session_id: Some("nv15-device".into()),
                request_id: None,
                summary: "合成家居审批".into(),
                facts: facts.clone(),
                connection: ApprovalConnection::Home {
                    fingerprint: fingerprint.clone(),
                },
                request: facts,
                client: "voice:nv15-device".into(),
                removed_fields: vec![],
            })
            .unwrap();
        let digest = created.digest;
        service
            .approve(&created.approval_id, &digest, &digest)
            .unwrap();
        (
            service,
            catalog,
            plan,
            fingerprint,
            created.approval_id,
            permit,
        )
    }

    #[test]
    fn set_temperature_and_play_media_confirm_on_second_poll_with_backoff() {
        use super::super::AllowedMedia;

        // 1. SetTemperature confirms on poll #2 when attributes["temperature"] matches within 0.05
        let (service, catalog, plan, fp, id, permit) = custom_plan_fixture(
            br#"[{"entity_id":"climate.room","state":"heat","last_updated":"now","attributes":{"min_temp":17.0,"max_temp":28.0,"temperature":20.0}}]"#,
            br#"[{"domain":"climate","services":{"set_temperature":{}}}]"#,
            vec![AllowedEntity {
                entity_id: "climate.room".into(),
                alias: "空调".into(),
                enabled: true,
                policy: HomePolicy::Ask,
                media: vec![],
            }],
            "空调",
            HomeAction::SetTemperature,
            json!({"temperature": 23.5}),
        );
        let mut climate_transport = PollSequenceTransport {
            expected_path: "/api/services/climate/set_temperature",
            expected_entity: "climate.room",
            replies: vec![
                ("heat".into(), json!({"temperature": 20.0})),
                ("heat".into(), json!({"temperature": 23.53})),
            ],
            sends: 0,
            reads: 0,
        };
        let started = Instant::now();
        let res = dispatch_approved(
            &service,
            &id,
            "nv15-device",
            &fp,
            &catalog,
            &plan,
            &permit,
            &mut climate_transport,
        )
        .unwrap();
        assert_eq!(res, HomeDispatchResult::Confirmed);
        assert_eq!((climate_transport.sends, climate_transport.reads), (1, 2));
        assert!(
            started.elapsed() >= Duration::from_millis(120),
            "poll #1 未命中后应退避等待再执行 poll #2"
        );
        let saved = service.get(&id).unwrap();
        assert_eq!(saved.status, ApprovalStatus::Executed);
        let outcome = saved.outcome.unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.phase, Some(ApprovalPhase::Completed));
        assert!(outcome.error_code.is_none());

        // 2. PlayMedia confirms ONLY when state == "playing" AND media_content_id matches approved media
        let (service, catalog, plan, fp, id, permit) = custom_plan_fixture(
            br#"[{"entity_id":"media_player.room","state":"idle","last_updated":"now","attributes":{}}]"#,
            br#"[{"domain":"media_player","services":{"play_media":{}}}]"#,
            vec![AllowedEntity {
                entity_id: "media_player.room".into(),
                alias: "音箱".into(),
                enabled: true,
                policy: HomePolicy::Ask,
                media: vec![AllowedMedia {
                    id: "library_demo_song".into(),
                    media_type: "music".into(),
                }],
            }],
            "音箱",
            HomeAction::PlayMedia,
            json!({"library_id": "library_demo_song"}),
        );
        let mut media_transport = PollSequenceTransport {
            expected_path: "/api/services/media_player/play_media",
            expected_entity: "media_player.room",
            replies: vec![
                (
                    "buffering".into(),
                    json!({"media_content_id": "library_demo_song"}),
                ),
                (
                    "playing".into(),
                    json!({"media_content_id": "library_demo_song"}),
                ),
            ],
            sends: 0,
            reads: 0,
        };
        let res = dispatch_approved(
            &service,
            &id,
            "nv15-device",
            &fp,
            &catalog,
            &plan,
            &permit,
            &mut media_transport,
        )
        .unwrap();
        assert_eq!(res, HomeDispatchResult::Confirmed);
        assert_eq!((media_transport.sends, media_transport.reads), (1, 2));
        let saved = service.get(&id).unwrap();
        assert_eq!(saved.status, ApprovalStatus::Executed);
        let outcome = saved.outcome.unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.phase, Some(ApprovalPhase::Completed));
        assert!(outcome.error_code.is_none());

        // 2b. PlayMedia stays AcceptedUnconfirmed if speaker is playing a different track or omits media_content_id
        let (service2, catalog2, plan2, fp2, id2, permit2) = custom_plan_fixture(
            br#"[{"entity_id":"media_player.room","state":"playing","last_updated":"now","attributes":{"media_content_id":"other_song"}}]"#,
            br#"[{"domain":"media_player","services":{"play_media":{}}}]"#,
            vec![AllowedEntity {
                entity_id: "media_player.room".into(),
                alias: "音箱".into(),
                enabled: true,
                policy: HomePolicy::Ask,
                media: vec![AllowedMedia {
                    id: "library_demo_song".into(),
                    media_type: "music".into(),
                }],
            }],
            "音箱",
            HomeAction::PlayMedia,
            json!({"library_id": "library_demo_song"}),
        );
        let mut wrong_media_transport = PollSequenceTransport {
            expected_path: "/api/services/media_player/play_media",
            expected_entity: "media_player.room",
            replies: vec![
                ("playing".into(), json!({"media_content_id": "other_song"})),
                ("playing".into(), Value::Null),
                ("playing".into(), json!({"media_content_id": "other_song"})),
            ],
            sends: 0,
            reads: 0,
        };
        let unconfirmed = dispatch_approved(
            &service2,
            &id2,
            "nv15-device",
            &fp2,
            &catalog2,
            &plan2,
            &permit2,
            &mut wrong_media_transport,
        )
        .unwrap();
        assert_eq!(unconfirmed, HomeDispatchResult::AcceptedUnconfirmed);
        // 3. Temperature matcher rejects unavailable/unknown states and out-of-tolerance values
        let matcher = ExpectedObservation::Temperature(23.5);
        assert!(matcher.matches("heat", &json!({"temperature": 23.54})));
        assert!(!matcher.matches("unavailable", &json!({"temperature": 23.5})));
        assert!(!matcher.matches("unknown", &json!({"temperature": 23.5})));
        assert!(!matcher.matches("heat", &json!({"temperature": 23.6})));
    }
}
