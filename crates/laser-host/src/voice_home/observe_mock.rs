//! NV15-4 control lifecycle state-machine, mock-only: a plan can be submitted
//! at most ONCE and observed, but this file has no HTTP POST implementation.
//! Only the eventual dedicated HomeControl executor may invoke this logic.
use super::{HomeCatalog, HomeControlPlan, HomeDeny};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservationResult {
    Confirmed,
    AcceptedStateUnconfirmed,
    ResultUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MockTransportFailure {
    BeforeSend,
    AfterSend,
}

/// The transport is a test seam. Its implementation in this stage cannot
/// reach real HA and is NEVER exposed as a production control interface.
pub(crate) trait SyntheticControlTransport {
    fn submit_once(
        &mut self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(), MockTransportFailure>;
    fn read_target_state(&mut self, entity_id: &str) -> Result<String, ()>;
}

/// No retry after submit begins: an error may mean the remote already acted.
/// State reads are independent observations and cannot trigger another POST.
pub(crate) fn run_synthetic_control(
    catalog: &HomeCatalog,
    plan: &HomeControlPlan,
    permission: &AtomicBool,
    executor: &mut impl SyntheticControlTransport,
) -> Result<ObservationResult, HomeDeny> {
    catalog.revalidate_plan(plan, permission)?;
    if !plan.requires_dedicated_approval {
        return Err(HomeDeny::ApprovalRequired);
    }
    if !permission.load(Ordering::Acquire) {
        return Err(HomeDeny::Cancelled);
    }
    match executor.submit_once(&plan.service_path, &plan.body) {
        Ok(()) => {}
        Err(MockTransportFailure::BeforeSend) => return Err(HomeDeny::ServiceUnavailable),
        Err(MockTransportFailure::AfterSend) => return Ok(ObservationResult::ResultUnknown),
    }
    if !permission.load(Ordering::Acquire) {
        return Ok(ObservationResult::ResultUnknown);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    for _ in 0..3 {
        if Instant::now() >= deadline || !permission.load(Ordering::Acquire) {
            return Ok(ObservationResult::ResultUnknown);
        }
        let Ok(observed) = executor.read_target_state(&plan.entity_id) else {
            return Ok(ObservationResult::ResultUnknown);
        };
        let expected = match plan.action {
            super::HomeAction::TurnOn => "on",
            super::HomeAction::TurnOff => "off",
            super::HomeAction::Lock => "locked",
            super::HomeAction::Unlock => "unlocked",
            super::HomeAction::OpenCover => "open",
            super::HomeAction::CloseCover => "closed",
            super::HomeAction::StopCover => "stopped",
            super::HomeAction::ArmHome => "armed_home",
            super::HomeAction::ArmAway => "armed_away",
            super::HomeAction::Disarm => "disarmed",
            // Climate and playback service effects cannot be inferred from
            // a generic state string; wait for dedicated attribute verification.
            super::HomeAction::SetTemperature | super::HomeAction::PlayMedia => "",
        };
        if !expected.is_empty() && observed == expected {
            return Ok(ObservationResult::Confirmed);
        }
    }
    Ok(ObservationResult::AcceptedStateUnconfirmed)
}

#[cfg(test)]
mod tests {
    use super::super::{AllowedEntity, HomeAction, HomePolicy};
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicBool;

    struct FakeHome {
        sent: usize,
        reads: usize,
        failure: Option<MockTransportFailure>,
        readings: Vec<String>,
        revoke_on_send: Option<std::sync::Arc<AtomicBool>>,
    }
    impl SyntheticControlTransport for FakeHome {
        fn submit_once(
            &mut self,
            path: &str,
            body: &serde_json::Value,
        ) -> Result<(), MockTransportFailure> {
            assert_eq!(path, "/api/services/light/turn_on");
            assert_eq!(body["entity_id"], "light.lab");
            self.sent += 1;
            if let Some(flag) = &self.revoke_on_send {
                flag.store(false, Ordering::Release);
            }
            if let Some(error) = self.failure {
                return Err(error);
            }
            Ok(())
        }
        fn read_target_state(&mut self, id: &str) -> Result<String, ()> {
            assert_eq!(id, "light.lab");
            self.reads += 1;
            Ok(self
                .readings
                .get(self.reads - 1)
                .cloned()
                .unwrap_or("off".into()))
        }
    }
    fn catalog() -> (HomeCatalog, HomeControlPlan, std::sync::Arc<AtomicBool>) {
        let permit = std::sync::Arc::new(AtomicBool::new(true));
        let list = vec![AllowedEntity {
            entity_id: "light.lab".into(),
            alias: "测试灯".into(),
            enabled: true,
            policy: HomePolicy::Ask,
            media: vec![],
        }];
        let states =
            br#"[{"entity_id":"light.lab","state":"off","last_updated":"2026","attributes":{}}]"#;
        let services = br#"[{"domain":"light","services":{"turn_on":{}}}]"#;
        let cat = HomeCatalog::from_snapshots(states, services, list, 3, 2).unwrap();
        let plan = cat
            .plan("light.lab", HomeAction::TurnOn, &json!({}), &permit)
            .unwrap();
        (cat, plan, permit)
    }
    fn fake() -> FakeHome {
        FakeHome {
            sent: 0,
            reads: 0,
            failure: None,
            readings: vec![],
            revoke_on_send: None,
        }
    }
    #[test]
    fn confirms_state_without_duplicate_submit() {
        let (cat, plan, permit) = catalog();
        let mut test = fake();
        test.readings = vec!["off".into(), "on".into()];
        assert_eq!(
            run_synthetic_control(&cat, &plan, &permit, &mut test),
            Ok(ObservationResult::Confirmed)
        );
        assert_eq!((test.sent, test.reads), (1, 2));
    }
    #[test]
    fn accepted_but_not_observed_is_not_reported_as_success() {
        let (cat, plan, permit) = catalog();
        let mut test = fake();
        assert_eq!(
            run_synthetic_control(&cat, &plan, &permit, &mut test),
            Ok(ObservationResult::AcceptedStateUnconfirmed)
        );
        assert_eq!((test.sent, test.reads), (1, 3));
    }
    #[test]
    fn uncertain_after_send_never_retries_or_reads() {
        let (cat, plan, permit) = catalog();
        let mut test = fake();
        test.failure = Some(MockTransportFailure::AfterSend);
        assert_eq!(
            run_synthetic_control(&cat, &plan, &permit, &mut test),
            Ok(ObservationResult::ResultUnknown)
        );
        assert_eq!((test.sent, test.reads), (1, 0));
    }
    #[test]
    fn revoked_before_send_zero_posts_or_after_send_unknown() {
        let (cat, plan, permit) = catalog();
        permit.store(false, Ordering::Release);
        let mut test = fake();
        assert_eq!(
            run_synthetic_control(&cat, &plan, &permit, &mut test),
            Err(HomeDeny::Cancelled)
        );
        assert_eq!(test.sent, 0);
        permit.store(true, Ordering::Release);
        let mut after = fake();
        after.revoke_on_send = Some(permit.clone());
        assert_eq!(
            run_synthetic_control(&cat, &plan, &permit, &mut after),
            Ok(ObservationResult::ResultUnknown)
        );
        assert_eq!((after.sent, after.reads), (1, 0));
    }
}
