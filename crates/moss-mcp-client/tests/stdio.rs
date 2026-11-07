//! Test requires Node.js, also required by the project's frontend toolchain.
use moss_mcp_client::*;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn stdio_handshake_env_refs_policy_and_shutdown() {
    let reference = secret_ref("local", "env", "FAKE_KEY");
    let config = ServerConfig {
        name: "local".into(),
        enabled: true,
        transport: TransportConfig::Stdio {
            command: "node".into(),
            args: vec![format!(
                "{}/tests/fixtures/stdio.cjs",
                env!("CARGO_MANIFEST_DIR")
            )],
            env_refs: BTreeMap::from([("FAKE_KEY".into(), reference.clone())]),
        },
        policies: BTreeMap::new(),
    };
    let secrets = BTreeMap::from([(reference, "test-value".into())]);
    let mut client = Client::connect(config, &secrets, Arc::new(RequireApproval))
        .await
        .unwrap();
    assert_eq!(client.tools(&HashSet::new(), 64)[0].name, "local__read");
    assert!(matches!(
        client.call("local__read", json!({})).await.unwrap(),
        CallOutcome::ApprovalRequired(_)
    ));
    client.set_permission("read", Permission::Allow).unwrap();
    let CallOutcome::Completed(result) = client.call("local__read", json!({})).await.unwrap()
    else {
        panic!("explicit allow should call the fixture")
    };
    let text = result["content"][0]["text"].as_str().unwrap();
    let report: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        report,
        json!({"calls":1,"secret":true,"cleanEnvironment":true})
    );
    client.heartbeat().await.unwrap();
    client.disconnect().await;
    assert!(!client.is_connected());
    assert!(client.tools(&HashSet::new(), 64).is_empty());
}
