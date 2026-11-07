//! NV15 synthetic one-time approval ledger for negative tests ONLY.
//! This is not database ApprovalService, not a desktop UI, and has NO POST.
use super::{HomeCatalog, HomeControlPlan, HomeDeny, execute_without_home_control_approval};
use getrandom::fill;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 16;
const MAX_TTL: Duration = Duration::from_secs(30);

struct Pending {
    plan: HomeControlPlan,
    device: String,
    permit: Arc<AtomicBool>,
    expires: Instant,
    approved: bool,
}

struct MockHomeApprovals {
    pending: Mutex<HashMap<[u8; 32], Pending>>,
}
fn digest(ticket: &str) -> [u8; 32] {
    let hash = Sha256::digest(ticket.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&hash);
    key
}
fn valid_device(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
}
impl MockHomeApprovals {
    fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
    fn request(
        &self,
        catalog: &HomeCatalog,
        plan: HomeControlPlan,
        device: &str,
        permit: Arc<AtomicBool>,
        ttl: Duration,
    ) -> Result<String, HomeDeny> {
        if !valid_device(device) || ttl.is_zero() || ttl > MAX_TTL {
            return Err(HomeDeny::InvalidConfig);
        }
        catalog.revalidate_plan(&plan, &permit)?;
        let mut pending = self.pending.lock().map_err(|_| HomeDeny::InvalidConfig)?;
        pending.retain(|_, p| p.expires > Instant::now() && p.permit.load(Ordering::Acquire));
        if pending.len() >= MAX_PENDING {
            return Err(HomeDeny::InvalidConfig);
        }
        let mut bytes = [0u8; 16];
        fill(&mut bytes).map_err(|_| HomeDeny::InvalidConfig)?;
        let ticket: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let key = digest(&ticket);
        if pending.contains_key(&key) {
            return Err(HomeDeny::InvalidConfig);
        }
        pending.insert(
            key,
            Pending {
                plan,
                device: device.to_owned(),
                permit,
                expires: Instant::now() + ttl,
                approved: false,
            },
        );
        Ok(ticket)
    }
    fn desktop_approve(&self, ticket: &str, trusted_desktop: bool) -> Result<(), HomeDeny> {
        if !trusted_desktop {
            return Err(HomeDeny::Unauthorized);
        }
        let mut pending = self.pending.lock().map_err(|_| HomeDeny::InvalidConfig)?;
        let entry = pending
            .get_mut(&digest(ticket))
            .ok_or(HomeDeny::Unauthorized)?;
        if !entry.permit.load(Ordering::Acquire) || entry.expires <= Instant::now() {
            return Err(HomeDeny::Cancelled);
        }
        entry.approved = true;
        Ok(())
    }
    fn consume(
        &self,
        catalog: &HomeCatalog,
        ticket: &str,
        device: &str,
    ) -> Result<HomeControlPlan, HomeDeny> {
        let mut pending = self.pending.lock().map_err(|_| HomeDeny::InvalidConfig)?;
        let key = digest(ticket);
        let entry = pending.get(&key).ok_or(HomeDeny::Unauthorized)?;
        if entry.device != device {
            return Err(HomeDeny::Unauthorized);
        }
        if !entry.approved {
            return Err(HomeDeny::ApprovalRequired);
        }
        if !entry.permit.load(Ordering::Acquire) || entry.expires <= Instant::now() {
            pending.remove(&key);
            return Err(HomeDeny::Cancelled);
        }
        if let Err(error) = catalog.revalidate_plan(&entry.plan, &entry.permit) {
            pending.remove(&key);
            return Err(error);
        }
        // Exactly one consumer can receive the inert plan. Actual control
        // is still blocked by execute_without_home_control_approval.
        let entry = pending.remove(&key).ok_or(HomeDeny::Unauthorized)?;
        Ok(entry.plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_home::{
        HomeAction,
        tests::{sample_services, sample_states, selected},
    };
    use serde_json::json;

    fn catalog(policy: u64, credential: u64) -> HomeCatalog {
        HomeCatalog::from_snapshots(
            &sample_states(),
            &sample_services(),
            selected(),
            policy,
            credential,
        )
        .unwrap()
    }
    fn fake_ticket(
        ledger: &MockHomeApprovals,
        catalog: &HomeCatalog,
        permit: Arc<AtomicBool>,
    ) -> String {
        let plan = catalog
            .plan(
                "台灯",
                HomeAction::TurnOn,
                &json!({"brightness":30}),
                &permit,
            )
            .unwrap();
        ledger
            .request(
                catalog,
                plan,
                "nv15-test-device",
                permit,
                Duration::from_secs(5),
            )
            .unwrap()
    }
    #[test]
    fn no_approval_no_control_and_one_time_consumption() {
        let index = catalog(7, 9);
        let ledger = MockHomeApprovals::new();
        let permit = Arc::new(AtomicBool::new(true));
        let ticket = fake_ticket(&ledger, &index, permit);
        assert_eq!(
            ledger.consume(&index, &ticket, "nv15-test-device"),
            Err(HomeDeny::ApprovalRequired)
        );
        assert_eq!(
            ledger.desktop_approve(&ticket, false),
            Err(HomeDeny::Unauthorized)
        );
        assert_eq!(
            ledger.consume(&index, &ticket, "not-the-device"),
            Err(HomeDeny::Unauthorized)
        );
        ledger.desktop_approve(&ticket, true).unwrap();
        let intent = ledger.consume(&index, &ticket, "nv15-test-device").unwrap();
        assert_eq!(intent.body["brightness"], 30);
        assert_eq!(
            execute_without_home_control_approval(&intent),
            Err(HomeDeny::ApprovalRequired)
        );
        assert_eq!(
            ledger.consume(&index, &ticket, "nv15-test-device"),
            Err(HomeDeny::Unauthorized)
        );
    }
    #[test]
    fn policy_revision_and_credentials_changed_after_approval_invalidate_ticket() {
        for next in [catalog(8, 9), catalog(7, 10)] {
            let original = catalog(7, 9);
            let ledger = MockHomeApprovals::new();
            let permit = Arc::new(AtomicBool::new(true));
            let ticket = fake_ticket(&ledger, &original, permit);
            ledger.desktop_approve(&ticket, true).unwrap();
            assert_eq!(
                ledger.consume(&next, &ticket, "nv15-test-device"),
                Err(HomeDeny::InvalidConfig)
            );
            assert_eq!(
                ledger.consume(&original, &ticket, "nv15-test-device"),
                Err(HomeDeny::Unauthorized)
            );
        }
    }
    #[test]
    fn revoke_expire_and_removed_services_never_release_a_control() {
        let original = catalog(7, 9);
        let ledger = MockHomeApprovals::new();
        let permit = Arc::new(AtomicBool::new(true));
        let ticket = fake_ticket(&ledger, &original, permit.clone());
        ledger.desktop_approve(&ticket, true).unwrap();
        permit.store(false, Ordering::Release);
        assert_eq!(
            ledger.consume(&original, &ticket, "nv15-test-device"),
            Err(HomeDeny::Cancelled)
        );

        let ledger = MockHomeApprovals::new();
        let permit = Arc::new(AtomicBool::new(true));
        let plan = original
            .plan("台灯", HomeAction::TurnOn, &json!({}), &permit)
            .unwrap();
        let ticket = ledger
            .request(
                &original,
                plan,
                "nv15-test-device",
                permit,
                Duration::from_millis(5),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            ledger.desktop_approve(&ticket, true),
            Err(HomeDeny::Cancelled)
        );

        let ledger = MockHomeApprovals::new();
        let permit = Arc::new(AtomicBool::new(true));
        let ticket = fake_ticket(&ledger, &original, permit);
        ledger.desktop_approve(&ticket, true).unwrap();
        let empty = HomeCatalog::from_snapshots(&sample_states(), b"[]", selected(), 7, 9).unwrap();
        assert_eq!(
            ledger.consume(&empty, &ticket, "nv15-test-device"),
            Err(HomeDeny::ServiceUnavailable)
        );
    }
    #[test]
    fn malformed_identity_and_injected_plan_are_rejected() {
        let index = catalog(7, 9);
        let ledger = MockHomeApprovals::new();
        let permit = Arc::new(AtomicBool::new(true));
        let mut plan = index
            .plan("台灯", HomeAction::TurnOn, &json!({}), &permit)
            .unwrap();
        plan.service_path = "/api/states/light.desk".into();
        assert_eq!(
            ledger.request(&index, plan, "nv15-test-device", permit.clone(), MAX_TTL),
            Err(HomeDeny::InvalidConfig)
        );
        let safe = index
            .plan("台灯", HomeAction::TurnOn, &json!({}), &permit)
            .unwrap();
        assert_eq!(
            ledger.request(&index, safe, "../../secret", permit, MAX_TTL),
            Err(HomeDeny::InvalidConfig)
        );
    }
}
