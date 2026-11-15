//! NV15 OFFLINE end-to-end HA control and read-back simulation.
//! Compiled only under tests. Only ephemeral loopback servers and synthetic
//! JSON/entities/approvals. Never reaches a real Home Assistant or device.
use super::tests::{sample_services, sample_states, selected};
use super::{HomeAction, HomeCatalog, HomeControlPlan, HomeDeny};
use laser_api::approval::{ApprovalService, NewApproval};
use laser_contracts::{
    ApprovalConnection, ApprovalKind, ApprovalOutcome, ApprovalPhase, ApprovalSource,
    ApprovalStatus,
};
use laser_runtime::{RuntimeDb, VOICE_DEVICE_ALLOWED};
use reqwest::{blocking::Client, redirect::Policy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const DEVICE: &str = "nv15-fake-device";
const SYNTHETIC_CREDENTIAL: &str = "nv15-synthetic-test-only";
const CONTROL_PATH: &str = "/api/services/light/turn_on";
const STATE_PATH: &str = "/api/states/light.desk";
const MAX_REPLY: usize = 4096;

fn fingerprint(plan: &HomeControlPlan) -> String {
    // Only a synthetic policy/credential revision fingerprint, no real URL
    // or long-lived secret. Production fingerprint must use trusted vault ID.
    let bytes = serde_json::to_vec(&json!({
        "path":plan.service_path, "body":plan.body,
        "policy_revision":plan.policy_revision,
        "credential_revision":plan.credential_revision,
    }))
    .unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

fn fake_setup() -> (
    ApprovalService,
    HomeCatalog,
    HomeControlPlan,
    String,
    Arc<AtomicBool>,
) {
    let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
    {
        let locked = db.lock().unwrap();
        locked
            .voice_device_seen(DEVICE, "nv15-mock-client", "", "", "", 995.0)
            .unwrap();
        locked
            .set_voice_device_status(DEVICE, VOICE_DEVICE_ALLOWED, 996.0)
            .unwrap();
    }
    let service = ApprovalService::new(db).with_clock(Arc::new(|| 1000.0));
    let catalog =
        HomeCatalog::from_snapshots(&sample_states(), &sample_services(), selected(), 7, 9)
            .unwrap();
    let permit = Arc::new(AtomicBool::new(true));
    let plan = catalog
        .plan(
            "台灯",
            HomeAction::TurnOn,
            &json!({"brightness":30}),
            &permit,
        )
        .unwrap();
    let fp = fingerprint(&plan);
    (service, catalog, plan, fp, permit)
}

fn approved(
    service: &ApprovalService,
    plan: &HomeControlPlan,
    fp: &str,
    permit: Arc<AtomicBool>,
) -> laser_contracts::ApprovalRecord {
    let facts = json!({
        "service_path":plan.service_path,
        "body":plan.body,
        "action":"turn_on",
        "policy_revision":plan.policy_revision,
        "credential_revision":plan.credential_revision,
    })
    .as_object()
    .unwrap()
    .clone();
    let created = service
        .create(NewApproval {
            owner: Some("nv15-offline-authenticated-mock".into()),
            voice_permit: Some(permit),
            kind: ApprovalKind::HomeControl,
            target_record: plan.entity_id.clone(),
            source: ApprovalSource::Voice,
            session_id: Some(DEVICE.into()),
            request_id: None,
            summary: "模拟审批：台灯开启到亮度 30（无真实家居）".into(),
            facts: facts.clone(),
            connection: ApprovalConnection::Home {
                fingerprint: fp.to_string(),
            },
            request: facts,
            client: "voice:nv15-fake-device".into(),
            removed_fields: Vec::new(),
        })
        .unwrap();
    assert_eq!(created.status, ApprovalStatus::Pending);
    service
        .approve(&created.approval_id, &created.digest, &created.digest)
        .unwrap()
}

/// A mock-only HA server with one POST followed by at most three state
/// observations. It NEVER listens on any non-loopback interface.
fn mock_hass_response(
    confirmed: bool,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    assert!(addr.ip().is_loopback());
    let captured = Arc::new(Mutex::new(Vec::new()));
    let seen = captured.clone();
    let worker = thread::spawn(move || {
        for step in 0..if confirmed { 2 } else { 4 } {
            let path = if step == 0 { CONTROL_PATH } else { STATE_PATH };
            let (mut tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                let count = tcp.read(&mut buf).unwrap();
                assert_ne!(count, 0, "mock request incomplete");
                raw.extend_from_slice(&buf[..count]);
                assert!(raw.len() <= 8192);
                let Some(split) = raw.windows(4).position(|v| v == b"\r\n\r\n") else {
                    continue;
                };
                let header = String::from_utf8_lossy(&raw[..split]);
                let expected = if step == 0 { "POST" } else { "GET" };
                assert!(header.starts_with(&format!("{expected} {path} HTTP/1.1")));
                assert!(
                    header
                        .to_ascii_lowercase()
                        .contains("authorization: bearer nv15-synthetic-test-only")
                );
                let body_size = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if raw.len() < split + 4 + body_size {
                    continue;
                }
                assert_eq!(raw.len(), split + 4 + body_size);
                if step == 0 {
                    let body: Value = serde_json::from_slice(&raw[split + 4..]).unwrap();
                    assert_eq!(body["entity_id"], "light.desk");
                    assert_eq!(body["brightness"], 30);
                }
                seen.lock().unwrap().push(expected.into());
                break;
            }
            let payload = if step == 0 {
                // Changed-states arrays never constitute confirmation.
                json!([{"entity_id":"light.other","state":"on"}])
            } else {
                json!({"entity_id":"light.desk","state":if confirmed{"on"}else{"off"}})
            };
            let bytes = payload.to_string();
            write!(tcp,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                bytes.len(),bytes).unwrap();
        }
    });
    (addr, captured, worker)
}

/// A test-only trusted action adapter with a single irreversible POST.
/// Once .send() starts, ANY ambiguous error means result_unknown; never retry.
fn fake_send_and_readback(
    catalog: &HomeCatalog,
    plan: &HomeControlPlan,
    addr: SocketAddr,
    permit: &AtomicBool,
) -> Result<super::observe_mock::ObservationResult, HomeDeny> {
    use super::observe_mock::ObservationResult;
    use std::time::Instant;
    if !addr.ip().is_loopback() || plan.service_path != CONTROL_PATH {
        return Err(HomeDeny::InvalidConfig);
    }
    catalog.revalidate_plan(plan, permit)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| HomeDeny::InvalidConfig)?;
    let origin = format!("http://127.0.0.1:{}", addr.port());
    if !permit.load(Ordering::Acquire) {
        return Err(HomeDeny::Cancelled);
    }
    // The approved plan is revalidated just before sending. The mock cannot
    // accept custom URLs, unapproved entities, credentials, or HA actions.
    catalog.revalidate_plan(plan, permit)?;
    let response = client
        .post(format!("{origin}{CONTROL_PATH}"))
        .bearer_auth(SYNTHETIC_CREDENTIAL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&plan.body)
        .send();
    let Ok(response) = response else {
        return Ok(ObservationResult::ResultUnknown);
    };
    if response.status() != reqwest::StatusCode::OK
        || response.headers().contains_key(reqwest::header::LOCATION)
        || response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some("application/json")
    {
        return Ok(ObservationResult::ResultUnknown);
    }
    let mut response_bytes = Vec::new();
    if response
        .take((MAX_REPLY + 1) as u64)
        .read_to_end(&mut response_bytes)
        .is_err()
        || response_bytes.len() > MAX_REPLY
        || serde_json::from_slice::<Value>(&response_bytes).is_err()
    {
        return Ok(ObservationResult::ResultUnknown);
    }
    if !permit.load(Ordering::Acquire) {
        return Ok(ObservationResult::ResultUnknown);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    for _ in 0..3 {
        if !permit.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Ok(ObservationResult::ResultUnknown);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let response = client
            .get(format!("{origin}{STATE_PATH}"))
            .bearer_auth(SYNTHETIC_CREDENTIAL)
            .timeout(remaining.min(Duration::from_millis(900)))
            .send();
        let Ok(response) = response else {
            return Ok(ObservationResult::ResultUnknown);
        };
        if response.status() != reqwest::StatusCode::OK
            || response.headers().contains_key(reqwest::header::LOCATION)
            || response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                != Some("application/json")
        {
            return Ok(ObservationResult::ResultUnknown);
        }
        let mut bytes = Vec::new();
        if response
            .take((MAX_REPLY + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > MAX_REPLY
        {
            return Ok(ObservationResult::ResultUnknown);
        }
        if !permit.load(Ordering::Acquire) {
            return Ok(ObservationResult::ResultUnknown);
        }
        let Ok(state) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(ObservationResult::ResultUnknown);
        };
        // Do not confuse an unrelated changed state with a confirmation.
        if state["entity_id"] != plan.entity_id {
            return Ok(ObservationResult::ResultUnknown);
        }
        if state["state"] == "on" {
            return Ok(ObservationResult::Confirmed);
        }
    }
    Ok(ObservationResult::AcceptedStateUnconfirmed)
}

fn fake_outcome(
    service: &ApprovalService,
    approval_id: &str,
    result: super::observe_mock::ObservationResult,
) -> laser_contracts::ApprovalRecord {
    use super::observe_mock::ObservationResult;
    let confirmed = result == ObservationResult::Confirmed;
    let phase = if confirmed {
        ApprovalPhase::Completed
    } else {
        ApprovalPhase::Unknown
    };
    let message = match result {
        ObservationResult::Confirmed => "模拟 HA 状态已回读确认",
        ObservationResult::AcceptedStateUnconfirmed => {
            "模拟 HA 服务已接受，但三次回查未确认目标状态；严禁重发"
        }
        ObservationResult::ResultUnknown => {
            "模拟 HA 服务响应中断或权限撤销，控制结果未知；严禁重发"
        }
    };
    service
        .record_outcome(
            approval_id,
            ApprovalOutcome {
                success: confirmed,
                message: message.into(),
                job_id: None,
                error_code: match result {
                    ObservationResult::Confirmed => None,
                    ObservationResult::AcceptedStateUnconfirmed => Some("state_unconfirmed".into()),
                    ObservationResult::ResultUnknown => Some("result_unknown".into()),
                },
                phase: Some(phase),
            },
        )
        .unwrap()
}

#[test]
fn nv15_fake_home_approval_single_post_followed_by_state_confirmation() {
    let (service, catalog, plan, fp, permit) = fake_setup();
    let approval = approved(&service, &plan, &fp, permit.clone());
    catalog.revalidate_plan(&plan, &permit).unwrap();
    assert!(
        service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fp,
                DEVICE,
                &permit
            )
            .is_ok()
    );
    let (addr, captured, worker) = mock_hass_response(true);
    let confirmed = fake_send_and_readback(&catalog, &plan, addr, &permit).unwrap();
    let outcome = fake_outcome(&service, &approval.approval_id, confirmed);
    assert_eq!(outcome.status, ApprovalStatus::Executed);
    assert_eq!(
        outcome.outcome.unwrap().phase,
        Some(ApprovalPhase::Completed)
    ); // Completed and Unknown terminal outcomes are immutable. A delayed or
    // reordered response must never claim that this control succeeded.
    assert_eq!(
        service
            .record_outcome(
                &approval.approval_id,
                ApprovalOutcome {
                    success: true,
                    message: "late fake success".into(),
                    phase: Some(ApprovalPhase::Completed),
                    ..ApprovalOutcome::default()
                }
            )
            .unwrap_err(),
        laser_api::approval::ApprovalError::Consumed
    );

    worker.join().unwrap();
    assert_eq!(*captured.lock().unwrap(), ["POST", "GET"]);
    assert!(
        service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fp,
                DEVICE,
                &permit
            )
            .is_err()
    );
}

#[test]
fn nv15_fake_home_state_mismatch_becomes_unknown_and_never_retries() {
    let (service, catalog, plan, fp, permit) = fake_setup();
    let approval = approved(&service, &plan, &fp, permit.clone());
    service
        .consume_home_control(
            &approval.approval_id,
            &approval.digest,
            &fp,
            DEVICE,
            &permit,
        )
        .unwrap();
    let (addr, captured, worker) = mock_hass_response(false);
    let confirmed = fake_send_and_readback(&catalog, &plan, addr, &permit).unwrap();
    assert_eq!(
        confirmed,
        super::observe_mock::ObservationResult::AcceptedStateUnconfirmed
    );
    let unknown = fake_outcome(&service, &approval.approval_id, confirmed);
    assert_eq!(unknown.status, ApprovalStatus::Failed);
    assert_eq!(unknown.outcome.unwrap().phase, Some(ApprovalPhase::Unknown)); // Completed and Unknown terminal outcomes are immutable. A delayed or
    // reordered response must never claim that this control succeeded.
    assert_eq!(
        service
            .record_outcome(
                &approval.approval_id,
                ApprovalOutcome {
                    success: true,
                    message: "late fake success".into(),
                    phase: Some(ApprovalPhase::Completed),
                    ..ApprovalOutcome::default()
                }
            )
            .unwrap_err(),
        laser_api::approval::ApprovalError::Consumed
    );

    worker.join().unwrap();
    assert_eq!(*captured.lock().unwrap(), ["POST", "GET", "GET", "GET"]);
    assert!(
        service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fp,
                DEVICE,
                &permit
            )
            .is_err()
    );
}

#[test]
fn nv15_post_reached_fake_ha_but_disconnected_is_unknown_and_never_replayed() {
    use super::observe_mock::ObservationResult;
    let (service, catalog, plan, fingerprint, permit) = fake_setup();
    let approval = approved(&service, &plan, &fingerprint, permit.clone());
    service
        .consume_home_control(
            &approval.approval_id,
            &approval.digest,
            &fingerprint,
            DEVICE,
            &permit,
        )
        .unwrap();

    // The fake peer reads the POST and closes before returning any HTTP
    // acknowledgement. The action may already have occurred: never retry.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = captured.clone();
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut first = [0u8; 4096];
        let received = socket.read(&mut first).unwrap();
        let header = String::from_utf8_lossy(&first[..received]);
        assert!(header.starts_with("POST /api/services/light/turn_on HTTP/1.1"));
        seen.lock().unwrap().push("POST".into());
        // Drop socket without any response: after-send result is uncertain.
    });
    assert_eq!(
        fake_send_and_readback(&catalog, &plan, addr, &permit),
        Ok(ObservationResult::ResultUnknown)
    );
    let unknown = fake_outcome(
        &service,
        &approval.approval_id,
        ObservationResult::ResultUnknown,
    );
    assert_eq!(unknown.status, ApprovalStatus::Failed);
    let outcome = unknown.outcome.unwrap();
    assert_eq!(outcome.phase, Some(ApprovalPhase::Unknown));
    assert_eq!(outcome.error_code.as_deref(), Some("result_unknown"));
    worker.join().unwrap();
    assert_eq!(*captured.lock().unwrap(), ["POST"]);
    assert!(
        service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fingerprint,
                DEVICE,
                &permit,
            )
            .is_err(),
        "uncertain sent commands must never be re-consumed"
    );
}

#[test]
fn nv15_modified_plan_or_revoked_permission_performs_zero_http_posts() {
    let (_service, catalog, plan, _fingerprint, permit) = fake_setup();
    let mut forged = plan.clone();
    forged.service_path = "/api/services/lock/unlock".into();
    assert_eq!(
        fake_send_and_readback(&catalog, &forged, "127.0.0.1:9".parse().unwrap(), &permit),
        Err(HomeDeny::InvalidConfig)
    );
    let mut changed = plan.clone();
    changed.body["brightness"] = json!(255);
    assert_eq!(
        fake_send_and_readback(&catalog, &changed, "127.0.0.1:9".parse().unwrap(), &permit),
        Err(HomeDeny::InvalidConfig)
    );
    permit.store(false, Ordering::Release);
    assert_eq!(
        fake_send_and_readback(&catalog, &plan, "127.0.0.1:9".parse().unwrap(), &permit),
        Err(HomeDeny::Cancelled)
    );
}
#[test]
fn nv15_revoked_voice_permission_blocks_fake_network_send() {
    let (service, catalog, plan, fp, permit) = fake_setup();
    let approval = approved(&service, &plan, &fp, permit.clone());
    permit.store(false, Ordering::Release);
    assert!(
        service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fp,
                DEVICE,
                &permit
            )
            .is_err()
    );
    assert_eq!(
        fake_send_and_readback(&catalog, &plan, "127.0.0.1:9".parse().unwrap(), &permit),
        Err(HomeDeny::Cancelled)
    );
}
