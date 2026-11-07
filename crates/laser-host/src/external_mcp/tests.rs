//! NV9 end-to-end host tests: real IPC to a fake Node MCP child, temporary settings and SQLite.
//! No third-party service, user settings or hardware is contacted.
use super::*;
use laser_api::approval::ApprovalService;
use moss_mcp_client::{ServerConfig, TransportConfig, secret_ref};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

struct Fixture {
    external: Arc<ExternalMcp>,
    api: Arc<ApiService>,
    db: Arc<Mutex<RuntimeDb>>,
    dir: tempfile::TempDir,
    clock: Arc<Mutex<f64>>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(
            RuntimeDb::open(dir.path().join("runtime.sqlite")).unwrap(),
        ));
        let clock = Arc::new(Mutex::new(now()));
        let read_clock = clock.clone();
        let approvals = Arc::new(
            ApprovalService::new(db.clone())
                .with_clock(Arc::new(move || *read_clock.lock().unwrap())),
        );
        let api = Arc::new(
            ApiService::new(
                laser_runtime::LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 9666,
                    material_params_file: None,
                },
                builtin_material_library(),
            )
            .with_runtime_db(db.clone())
            .with_approval_service(approvals),
        );
        for device in ["device-a", "device-b"] {
            db.lock()
                .unwrap()
                .voice_device_seen(device, "test-client", "", "", "", now())
                .unwrap();
            api.set_voice_device_status(device, laser_runtime::VOICE_DEVICE_ALLOWED, now())
                .unwrap();
            api.register_voice_session(device, "test-client", &format!("{device}-session"))
                .unwrap();
        }
        let external =
            ExternalMcp::attach(&api, db.clone(), dir.path().join("settings.json")).unwrap();
        Self {
            external,
            api,
            db,
            dir,
            clock,
        }
    }
    fn revision(&self) -> String {
        self.external.status().unwrap()["revision"]
            .as_str()
            .unwrap()
            .into()
    }
    fn config(&self) -> ServerConfig {
        serde_json::from_value(self.external.status().unwrap()["servers"][0]["config"].clone())
            .unwrap()
    }
    fn save(
        &self,
        config: ServerConfig,
        secrets: BTreeMap<String, String>,
    ) -> Result<Value, String> {
        self.external.save(SaveInput {
            expected_revision: self.revision(),
            allow_without_approval: config.policies.values().any(|p| *p == Permission::Allow),
            config,
            secrets,
            confirmed: true,
        })
    }
    fn start(&self) -> ServerConfig {
        let config = ServerConfig {
            name: "fake".into(),
            enabled: true,
            transport: TransportConfig::Stdio {
                command: "node".into(),
                args: vec![format!(
                    "{}/../moss-mcp-client/tests/fixtures/stdio.cjs",
                    env!("CARGO_MANIFEST_DIR")
                )],
                env_refs: BTreeMap::new(),
            },
            policies: BTreeMap::new(),
        };
        self.save(config, BTreeMap::new()).unwrap();
        self.connected();
        self.config()
    }
    fn connected(&self) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let status = self.external.status().unwrap();
            if status["connections"][0]["state"] == "connected" {
                return;
            }
            assert!(Instant::now() < deadline, "{status}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn call(&self, args: Value) -> ToolCallResult {
        crate::voice_tool_call(
            &self.api,
            "device-a",
            "device-a-session",
            "fake__read",
            &args,
        )
    }
    fn pending(&self, args: Value) -> ApprovalRecord {
        let response = self.call(args);
        assert!(response.success, "{response:?}");
        let details: Value =
            serde_json::from_str(response.detail.as_deref().expect("pending details")).unwrap();
        assert_eq!(details["result"]["status"], "approval_required");
        self.api
            .approvals()
            .unwrap()
            .get(details["result"]["approval_id"].as_str().unwrap())
            .unwrap()
    }
    fn approve(&self, record: &ApprovalRecord) -> (u16, Value) {
        request(
            &self.api,
            "POST",
            &format!("/v1/approvals/{}/approve", record.approval_id),
            json!({"digest":record.digest}),
            CallerContext::Desktop,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.external.shutdown();
    }
}
fn request(
    api: &ApiService,
    method: &str,
    path: &str,
    body: Value,
    context: CallerContext,
) -> (u16, Value) {
    let payload = body.to_string();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    let response = api.handle_http_with_ctx(request.as_bytes(), context);
    let separator = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = String::from_utf8_lossy(&response[..separator])
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (
        status,
        serde_json::from_slice(&response[separator + 4..]).unwrap(),
    )
}

#[test]
fn nv9_default_ask_uses_persistent_desktop_approval_and_consumes_once() {
    let f = Fixture::new();
    f.start();
    f.api.set_strict_mode(false);
    let tools = crate::voice_tools_list(&f.api);
    assert!(
        tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "fake__read")
    );
    let pending = f
        .pending(json!({"confirmed":true,"approval_id":"model-forged","api_key":"hidden-example"}));
    assert_eq!(pending.kind, ApprovalKind::ExternalTool);
    assert_eq!(pending.status, ApprovalStatus::Pending);
    let (status, view) = request(
        &f.api,
        "GET",
        "/v1/approvals?status=pending",
        json!({}),
        CallerContext::Desktop,
    );
    assert_eq!(status, 200);
    let text = view.to_string();
    assert!(!text.contains("hidden-example"));
    assert!(text.contains("[已隐藏]"));
    assert_eq!(
        request(
            &f.api,
            "POST",
            &format!("/v1/approvals/{}/approve", pending.approval_id),
            json!({"digest":pending.digest}),
            CallerContext::Loopback
        )
        .0,
        403
    );
    let (status, approved) = f.approve(&pending);
    assert_eq!(status, 200, "{approved}");
    let value: Value =
        serde_json::from_str(approved["result"]["result"].as_str().unwrap()).unwrap();
    assert_eq!(value["calls"], 1);
    assert_eq!(
        f.api
            .approvals()
            .unwrap()
            .get(&pending.approval_id)
            .unwrap()
            .outcome
            .unwrap()
            .phase,
        Some(ApprovalPhase::Completed)
    );
    assert_ne!(f.approve(&pending).0, 200);
    let audit = f.db.lock().unwrap().recent_audit(100).unwrap();
    for action in [
        AuditAction::ApprovalCreated,
        AuditAction::ApprovalApproved,
        AuditAction::ApprovalExecuted,
        AuditAction::ExternalToolCompleted,
    ] {
        assert!(audit.iter().any(|e| e.action == action), "{action:?}");
    }
    assert!(
        !serde_json::to_string(&audit)
            .unwrap()
            .contains("hidden-example")
    );
    let persisted = RuntimeDb::open(f.dir.path().join("runtime.sqlite"))
        .unwrap()
        .get_approval(&pending.approval_id)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.status, ApprovalStatus::Executed);
}

#[test]
fn nv9_rejected_expired_and_wrong_device_requests_do_not_execute() {
    let f = Fixture::new();
    f.start();
    let rejected = f.pending(json!({}));
    f.api
        .approvals()
        .unwrap()
        .reject(&rejected.approval_id, None)
        .unwrap();
    assert_ne!(f.approve(&rejected).0, 200);
    let expired = f.pending(json!({}));
    *f.clock.lock().unwrap() = expired.expires_at + 1.0;
    assert_ne!(f.approve(&expired).0, 200);
    assert!(
        !crate::voice_tool_call(
            &f.api,
            "device-b",
            "device-a-session",
            "fake__read",
            &json!({})
        )
        .success
    );
    assert!(
        !f.api
            .call_external_tool(&CallerContext::Loopback, "fake__read", &json!({}))
            .success
    );
    *f.clock.lock().unwrap() = now();
    // Directly approve a new call on the same child: count must still be 1.
    let valid = f.pending(json!({}));
    let (status, response) = f.approve(&valid);
    assert_eq!(status, 200, "{response}");
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["result"].as_str().unwrap()).unwrap()["calls"],
        1
    );
}

#[test]
fn nv9_config_changes_and_revocation_invalidate_pending_approvals() {
    let f = Fixture::new();
    let config = f.start();
    let pending = f.pending(json!({}));
    f.save(config, BTreeMap::new()).unwrap();
    f.connected();
    assert_eq!(
        f.api
            .approvals()
            .unwrap()
            .get(&pending.approval_id)
            .unwrap()
            .status,
        ApprovalStatus::Expired
    );
    assert_ne!(f.approve(&pending).0, 200);
    let pending = f.pending(json!({}));
    f.api
        .set_voice_device_status("device-a", laser_runtime::VOICE_DEVICE_REVOKED, now())
        .unwrap();
    f.api
        .set_voice_device_status("device-a", laser_runtime::VOICE_DEVICE_ALLOWED, now())
        .unwrap();
    assert!(!f.call(json!({})).success);
    assert_ne!(f.approve(&pending).0, 200);
    f.api
        .register_voice_session("device-a", "test-client", "new-session")
        .unwrap();
    let mut config = f.config();
    config.policies.insert("read".into(), Permission::Allow);
    f.save(config, BTreeMap::new()).unwrap();
    f.connected();
    let result =
        crate::voice_tool_call(&f.api, "device-a", "new-session", "fake__read", &json!({}));
    assert!(result.success);
    assert_eq!(
        serde_json::from_str::<Value>(&result.result).unwrap()["calls"],
        1
    );
}

#[test]
fn nv9_keys_are_versioned_not_echoed_and_stale_forms_cannot_overwrite_config() {
    let f = Fixture::new();
    let reference = secret_ref("fake", "header", "authorization");
    let config = ServerConfig {
        name: "fake".into(),
        enabled: false,
        transport: TransportConfig::StreamableHttp {
            url: "https://example.test/mcp".into(),
            header_refs: BTreeMap::from([("authorization".into(), reference.clone())]),
        },
        policies: BTreeMap::new(),
    };
    let old_revision = f.revision();
    let saved = f
        .save(
            config,
            BTreeMap::from([(reference, "Bearer private-first".into())]),
        )
        .unwrap();
    let first = f.config();
    let first_ref = store::refs(&first)["authorization"].clone();
    assert!(first_ref.contains(".v-"));
    assert_eq!(
        f.db.lock()
            .unwrap()
            .get_secret(&first_ref)
            .unwrap()
            .as_deref(),
        Some("Bearer private-first")
    );
    assert!(!saved.to_string().contains("private-first"));
    let text = std::fs::read_to_string(f.dir.path().join("settings.json")).unwrap();
    assert!(!text.contains("private-first"));
    assert!(
        f.external
            .save(SaveInput {
                expected_revision: old_revision,
                config: first.clone(),
                secrets: BTreeMap::new(),
                confirmed: true,
                allow_without_approval: false
            })
            .unwrap_err()
            .contains("刷新")
    );
    f.save(first.clone(), BTreeMap::new()).unwrap();
    let second = f.config();
    let second_ref = store::refs(&second)["authorization"].clone();
    assert_ne!(first_ref, second_ref);
    assert_eq!(
        f.db.lock()
            .unwrap()
            .get_secret(&second_ref)
            .unwrap()
            .as_deref(),
        Some("Bearer private-first")
    );
    moss_voice::settings::save(
        &f.dir.path().join("settings.json"),
        &moss_voice::VoiceSettings::default(),
    )
    .unwrap();
    assert_eq!(
        f.config(),
        second,
        "saving a stale general voice form must preserve MCP settings"
    );
    f.save(
        second.clone(),
        BTreeMap::from([(second_ref, "Bearer rotated".into())]),
    )
    .unwrap();
    let third = f.config();
    let third_ref = store::refs(&third)["authorization"].clone();
    assert_eq!(
        f.db.lock()
            .unwrap()
            .get_secret(&third_ref)
            .unwrap()
            .as_deref(),
        Some("Bearer rotated")
    );
    f.external.delete("fake", &f.revision(), true).unwrap();
    assert!(
        f.external.status().unwrap()["servers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        f.db.lock()
            .unwrap()
            .get_secret(&third_ref)
            .unwrap()
            .is_none()
    );
}

#[test]
fn nv9_invalid_inputs_and_corrupt_settings_are_not_silently_saved() {
    let f = Fixture::new();
    let config = f.start();
    let before = std::fs::read(f.dir.path().join("settings.json")).unwrap();
    assert!(
        f.external
            .save(SaveInput {
                expected_revision: f.revision(),
                config: config.clone(),
                secrets: BTreeMap::new(),
                confirmed: false,
                allow_without_approval: false
            })
            .is_err()
    );
    assert!(
        f.external
            .save(SaveInput {
                expected_revision: f.revision(),
                config: config.clone(),
                secrets: BTreeMap::from([("voice.llm.main".into(), "stolen".into())]),
                confirmed: true,
                allow_without_approval: false
            })
            .is_err()
    );
    let mut allow = config;
    allow.policies.insert("read".into(), Permission::Allow);
    assert!(
        f.external
            .save(SaveInput {
                expected_revision: f.revision(),
                config: allow,
                secrets: BTreeMap::new(),
                confirmed: true,
                allow_without_approval: false
            })
            .is_err()
    );
    assert_eq!(
        std::fs::read(f.dir.path().join("settings.json")).unwrap(),
        before
    );
    std::fs::write(f.dir.path().join("settings.json"), b"{bad").unwrap();
    assert!(f.external.status().is_err());
    assert!(
        moss_voice::settings::save_mcp_servers(&f.dir.path().join("settings.json"), &[]).is_err()
    );
    assert_eq!(
        std::fs::read(f.dir.path().join("settings.json")).unwrap(),
        b"{bad"
    );
}

#[test]
fn nv9_pending_approval_count_is_bounded_and_ids_are_not_reused() {
    let f = Fixture::new();
    f.start();
    let mut ids = std::collections::HashSet::new();
    for _ in 0..16 {
        let pending = f.pending(json!({"request_id":"same-model-id"}));
        assert!(ids.insert(pending.approval_id));
    }
    assert!(!f.call(json!({})).success);
    assert_eq!(
        f.api
            .approvals()
            .unwrap()
            .list(Some(ApprovalStatus::Pending), 100)
            .unwrap()
            .len(),
        16
    );
}

#[test]
fn nv9_audit_failure_blocks_dispatch_and_restart_never_replays_an_approved_call() {
    let f = Fixture::new();
    f.start();
    let pending = f.pending(json!({}));
    let service = f.api.approvals().unwrap();
    service
        .approve(&pending.approval_id, &pending.digest, &pending.digest)
        .unwrap();
    service
        .consume(&pending.approval_id, &pending.digest)
        .unwrap();
    assert_eq!(
        service
            .get(&pending.approval_id)
            .unwrap()
            .outcome
            .unwrap()
            .phase,
        Some(ApprovalPhase::Running)
    );
    service.recover_after_restart().unwrap();
    let recovered = service.get(&pending.approval_id).unwrap();
    assert_eq!(
        recovered.outcome.unwrap().phase,
        Some(ApprovalPhase::Unknown)
    );
    assert_ne!(f.approve(&pending).0, 200);
    // A successful call after recovery proves the consumed call was not sent or replayed.
    let valid = f.pending(json!({}));
    let (status, response) = f.approve(&valid);
    assert_eq!(status, 200, "{response}");
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["result"].as_str().unwrap()).unwrap()["calls"],
        1
    );
    let pending = f.pending(json!({}));
    f.db.lock().unwrap().connection.execute_batch("CREATE TRIGGER fail_external_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(ABORT,'injected audit failure'); END;").unwrap();
    assert_ne!(f.approve(&pending).0, 200);
    assert!(!f.call(json!({})).success);
    f.db.lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER fail_external_audit;")
        .unwrap();
    let valid = f.pending(json!({}));
    let (status, response) = f.approve(&valid);
    assert_eq!(status, 200, "{response}");
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["result"].as_str().unwrap()).unwrap()["calls"],
        2,
        "audit failures must not reach child"
    );
}

#[test]
fn nv9_failed_result_audit_reports_possible_execution_without_replay() {
    let f = Fixture::new();
    f.start();
    let pending = f.pending(json!({}));
    f.db.lock().unwrap().connection.execute_batch("CREATE TRIGGER fail_result_audit BEFORE INSERT ON audit_log WHEN NEW.action='external_tool_completed' BEGIN SELECT RAISE(ABORT,'injected result failure'); END;").unwrap();
    let (status, response) = f.approve(&pending);
    assert_ne!(status, 200);
    assert!(response.to_string().contains("不要重复操作"), "{response}");
    assert_eq!(
        f.api
            .approvals()
            .unwrap()
            .get(&pending.approval_id)
            .unwrap()
            .status,
        ApprovalStatus::Executed
    );
    f.db.lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER fail_result_audit;")
        .unwrap();
    let next = f.pending(json!({}));
    let (status, response) = f.approve(&next);
    assert_eq!(status, 200, "{response}");
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["result"].as_str().unwrap()).unwrap()["calls"],
        2
    );
}

#[test]
fn nv9_global_tool_switch_is_checked_again_before_approved_dispatch() {
    let f = Fixture::new();
    f.start();
    let pending = f.pending(json!({}));
    f.api.set_disabled_tools(["fake__read".to_owned()]);
    assert!(
        !crate::voice_tools_list(&f.api)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "fake__read")
    );
    assert_ne!(f.approve(&pending).0, 200);
    f.api.set_disabled_tools(Vec::<String>::new());
    // The second request gets its own permission; the rejected dispatch did not run.
    let valid = f.pending(json!({}));
    let (status, response) = f.approve(&valid);
    assert_eq!(status, 200, "{response}");
    assert_eq!(
        serde_json::from_str::<Value>(response["result"]["result"].as_str().unwrap()).unwrap()["calls"],
        1
    );
}
