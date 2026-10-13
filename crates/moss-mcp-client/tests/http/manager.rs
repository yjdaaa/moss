use super::*;
use std::sync::atomic::AtomicBool;

#[derive(Default)]
struct Gate {
    denied: AtomicBool,
    authorized: AtomicUsize,
    completed: AtomicUsize,
}
impl DispatchGuard for Gate {
    fn authorize(&self, _: &CallIntent, _: Permission) -> Result<(), ClientError> {
        self.authorized.fetch_add(1, Ordering::SeqCst);
        if self.denied.load(Ordering::SeqCst) {
            Err(ClientError::ApprovalFailed)
        } else {
            Ok(())
        }
    }
    fn is_valid(&self) -> bool {
        !self.denied.load(Ordering::SeqCst)
    }
    fn completed(&self, _: &CallIntent, _: &Result<Value, ClientError>) -> Result<(), ClientError> {
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
async fn until(condition: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition not reached"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn manager() -> Manager {
    Manager::new(tokio::runtime::Handle::current(), HashSet::new())
        .with_heartbeat(Duration::from_millis(50))
}

#[tokio::test]
async fn automatic_heartbeat_recovers_without_replaying_calls_and_preserves_stats() {
    let fixture = Fixture::new(Fake::one_tool()).await;
    let manager = manager();
    manager
        .configure(fixture.config.clone(), fixture.secrets.clone())
        .unwrap();
    until(|| manager.tools().len() == 1).await;
    let prepared = manager.prepare("notes__read", json!({})).await.unwrap();
    fixture.fake.fail_call.store(401, Ordering::SeqCst);
    let gate = Arc::new(Gate::default());
    assert_eq!(
        manager.execute(prepared, gate.clone()).await.unwrap_err(),
        ClientError::OutcomeUnknown
    );
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 1);
    fixture.fake.fail_call.store(0, Ordering::SeqCst);
    until(|| manager.snapshot()[0].connections >= 2 && manager.tools().len() == 1).await;
    assert_eq!(
        fixture.fake.calls.load(Ordering::SeqCst),
        1,
        "reconnection must never replay a call"
    );
    let prepared = manager.prepare("notes__read", json!({})).await.unwrap();
    manager.execute(prepared, gate.clone()).await.unwrap();
    until(|| fixture.fake.pings.load(Ordering::SeqCst) > 0).await;
    assert_eq!(manager.snapshot()[0].tools[0].stats.calls, 2);
    assert_eq!(manager.snapshot()[0].tools[0].stats.failures, 1);
    fixture.fake.fail_ping.store(500, Ordering::SeqCst);
    until(|| manager.tools().is_empty()).await;
    fixture.fake.fail_ping.store(0, Ordering::SeqCst);
    until(|| manager.snapshot()[0].connections >= 3 && manager.tools().len() == 1).await;
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 2);
    assert_eq!(gate.completed.load(Ordering::SeqCst), 2);
    manager.shutdown();
    assert!(manager.tools().is_empty());
}

#[tokio::test]
async fn catalog_refresh_invalidates_old_schema_and_new_tools_are_ask_by_default() {
    let fixture = Fixture::new(Fake::one_tool()).await;
    let manager = manager();
    manager
        .configure(fixture.config.clone(), fixture.secrets.clone())
        .unwrap();
    until(|| manager.tools().len() == 1).await;
    let prepared = manager.prepare("notes__read", json!({})).await.unwrap();
    fixture
        .fake
        .tools
        .lock()
        .unwrap()
        .push(json!({"name":"added","inputSchema":{"type":"object"}}));
    fixture.fake.tools.lock().unwrap()[0]["inputSchema"] =
        json!({"type":"object","properties":{"extra":{"type":"string"}}});
    until(|| manager.tools().len() == 2).await;
    assert_eq!(
        manager
            .execute(prepared, Arc::new(Gate::default()))
            .await
            .unwrap_err(),
        ClientError::ApprovalFailed
    );
    assert_eq!(
        manager
            .prepare("notes__added", json!({}))
            .await
            .unwrap()
            .permission,
        Permission::Ask
    );
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    manager.shutdown();
}

#[tokio::test]
async fn reconfiguration_and_revocation_block_queued_prepared_calls() {
    let fixture = Fixture::new(Fake::one_tool()).await;
    let manager = manager();
    manager
        .configure(fixture.config.clone(), fixture.secrets.clone())
        .unwrap();
    until(|| manager.tools().len() == 1).await;
    let old = manager.prepare("notes__read", json!({})).await.unwrap();
    manager.suspend("notes");
    assert!(manager.tools().is_empty());
    manager
        .configure(fixture.config.clone(), fixture.secrets.clone())
        .unwrap();
    until(|| manager.tools().len() == 1).await;
    assert_eq!(
        manager
            .execute(old, Arc::new(Gate::default()))
            .await
            .unwrap_err(),
        ClientError::ApprovalFailed
    );
    let prepared = manager.prepare("notes__read", json!({})).await.unwrap();
    let gate = Arc::new(Gate::default());
    gate.denied.store(true, Ordering::SeqCst);
    assert_eq!(
        manager.execute(prepared, gate).await.unwrap_err(),
        ClientError::ApprovalFailed
    );
    let mut tampered = manager.prepare("notes__read", json!({})).await.unwrap();
    tampered
        .intent
        .arguments
        .insert("unapproved".into(), json!(true));
    assert_eq!(
        manager
            .execute(tampered, Arc::new(Gate::default()))
            .await
            .unwrap_err(),
        ClientError::ApprovalFailed
    );
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    manager.shutdown();
}

#[tokio::test]
async fn global_catalog_budget_and_reserved_names_are_enforced_on_list_and_call() {
    let fake = Fake::one_tool();
    *fake.tools.lock().unwrap() = (0..70)
        .map(|index| json!({"name":format!("t{index:02}"),"inputSchema":{"type":"object"}}))
        .collect();
    let fixture = Fixture::new(fake).await;
    let manager = Manager::new(
        tokio::runtime::Handle::current(),
        HashSet::from(["notes__t00".into()]),
    );
    manager
        .configure(fixture.config.clone(), fixture.secrets.clone())
        .unwrap();
    until(|| manager.snapshot()[0].state == "connected").await;
    assert_eq!(manager.tools().len(), 64);
    assert_eq!(manager.snapshot()[0].tools.len(), 70);
    assert!(manager.prepare("notes__t00", json!({})).await.is_err());
    assert!(manager.prepare("notes__t69", json!({})).await.is_err());
    let mut config = fixture.config.clone();
    for i in 1..10 {
        config
            .policies
            .insert(format!("t{i:02}"), Permission::Disabled);
    }
    manager.configure(config, fixture.secrets.clone()).unwrap();
    until(|| manager.snapshot()[0].state == "connected").await;
    assert_eq!(manager.tools().len(), 60);
    assert!(manager.prepare("notes__t69", json!({})).await.is_ok());
    assert!(manager.prepare("notes__t01", json!({})).await.is_err());
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    manager.shutdown();
}

#[tokio::test]
async fn denied_or_cancelled_before_authorization_never_reaches_transport() {
    struct SlowGate {
        began: AtomicBool,
        release: AtomicBool,
        completed: AtomicUsize,
    }
    impl DispatchGuard for SlowGate {
        fn authorize(&self, _: &CallIntent, _: Permission) -> Result<(), ClientError> {
            self.began.store(true, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !self.release.load(Ordering::SeqCst) {
                if std::time::Instant::now() > deadline {
                    return Err(ClientError::ApprovalFailed);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }
        fn is_valid(&self) -> bool {
            true
        }
        fn completed(
            &self,
            _: &CallIntent,
            _: &Result<Value, ClientError>,
        ) -> Result<(), ClientError> {
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let fixture = Fixture::new(Fake::one_tool()).await;
    let manager = Arc::new(manager());
    manager
        .configure(fixture.config.clone(), fixture.secrets.clone())
        .unwrap();
    until(|| manager.tools().len() == 1).await;
    let prepared = manager.prepare("notes__read", json!({})).await.unwrap();
    let gate = Arc::new(SlowGate {
        began: AtomicBool::new(false),
        release: AtomicBool::new(false),
        completed: AtomicUsize::new(0),
    });
    let (m, g) = (manager.clone(), gate.clone());
    let task = tokio::spawn(async move { m.execute(prepared, g).await });
    until(|| gate.began.load(Ordering::SeqCst)).await;
    task.abort();
    let _ = task.await;
    gate.release.store(true, Ordering::SeqCst);
    until(|| gate.completed.load(Ordering::SeqCst) == 1).await;
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    manager.shutdown();
}
