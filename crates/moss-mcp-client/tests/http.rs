//! Only loopback fake servers. No third-party account or hardware is contacted.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use moss_mcp_client::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU16, AtomicUsize, Ordering},
};
use std::time::Duration;

#[path = "http/manager.rs"]
mod manager;

#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
    pings: AtomicUsize,
    fail_call: AtomicU16,
    fail_ping: AtomicU16,
    headers: Mutex<Vec<String>>,
    tools: Mutex<Vec<Value>>,
}
impl Fake {
    fn one_tool() -> Arc<Self> {
        Arc::new(Self {
            tools: Mutex::new(vec![
                json!({"name":"read","description":"中文".repeat(300),"inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}),
            ]),
            ..Default::default()
        })
    }
}
async fn handle(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    fake.headers.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_owned(),
    );
    let id = request.get("id").cloned();
    let method = request["method"].as_str().unwrap_or("");
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = match method {
        // Exercise compatibility with a legacy MCP implementation.
        "server/discover" => return Json(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
        )
        .into_response(),
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"loopback-fake","version":"1"}})
        }
        "tools/list" => json!({"tools":fake.tools.lock().unwrap().clone()}),
        "ping" => {
            fake.pings.fetch_add(1, Ordering::SeqCst);
            let status = fake.fail_ping.load(Ordering::SeqCst);
            if status != 0 {
                return StatusCode::from_u16(status).unwrap().into_response();
            }
            json!({})
        }
        "tools/call" => {
            fake.calls.fetch_add(1, Ordering::SeqCst);
            let status = fake.fail_call.load(Ordering::SeqCst);
            if status != 0 {
                return StatusCode::from_u16(status).unwrap().into_response();
            }
            json!({"content":[{"type":"text","text":"fake result"}],"isError":false})
        }
        _ => {
            return Json(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unknown"}}),
            )
            .into_response();
        }
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}
struct Fixture {
    fake: Arc<Fake>,
    config: ServerConfig,
    secrets: BTreeMap<String, String>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(fake: Arc<Fake>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/mcp", post(handle))
            .with_state(fake.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let reference = secret_ref("notes", "header", "authorization");
        let config = ServerConfig {
            name: "notes".into(),
            enabled: true,
            transport: TransportConfig::StreamableHttp {
                url,
                header_refs: BTreeMap::from([("authorization".into(), reference.clone())]),
            },
            policies: BTreeMap::new(),
        };
        Self {
            fake,
            config,
            secrets: BTreeMap::from([(reference, "Bearer fake-secret".into())]),
            server,
        }
    }
    async fn client(&self) -> Client {
        tokio::time::timeout(
            Duration::from_secs(10),
            Client::connect(
                self.config.clone(),
                &self.secrets,
                Arc::new(RequireApproval),
            ),
        )
        .await
        .unwrap()
        .unwrap()
    }
}

#[tokio::test]
async fn default_ask_never_calls_even_with_read_only_annotation_and_confirmed_argument() {
    let fixture = Fixture::new(Fake::one_tool()).await;
    let mut client = fixture.client().await;
    let tools = client.tools(&HashSet::new(), 64);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "notes__read");
    assert_eq!(tools[0].description.chars().count(), 200);
    assert!(
        client
            .tools(&HashSet::from(["notes__read".into()]), 64)
            .is_empty()
    );
    assert!(client.tools(&HashSet::new(), 0).is_empty());
    assert!(matches!(
        client
            .call(
                "notes__read",
                json!({"confirmed":true,"approval_id":"model-invented"})
            )
            .await
            .unwrap(),
        CallOutcome::ApprovalRequired(_)
    ));
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    client.set_permission("read", Permission::Allow).unwrap();
    assert!(matches!(
        client.call("notes__read", json!({})).await.unwrap(),
        CallOutcome::Completed(_)
    ));
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 1);
    client.set_permission("read", Permission::Disabled).unwrap();
    assert!(client.tools(&HashSet::new(), 64).is_empty());
    assert_eq!(
        client.call("notes__read", json!({})).await.unwrap_err(),
        ClientError::Disabled
    );
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 1);
    assert_eq!(client.stats()["notes__read"].calls, 1);
    assert_eq!(client.stats()["notes__read"].approval_requests, 1);
    assert!(
        !serde_json::to_string(client.stats())
            .unwrap()
            .contains("secret")
    );
    assert!(
        fixture
            .fake
            .headers
            .lock()
            .unwrap()
            .iter()
            .all(|header| header == "Bearer fake-secret")
    );
    client.heartbeat().await.unwrap();
    assert_eq!(fixture.fake.pings.load(Ordering::SeqCst), 1);
    client.disconnect().await;
    assert!(!client.is_connected());
}

struct WrongDigest;
impl ApprovalGate for WrongDigest {
    fn authorize<'a>(
        &'a self,
        _: &'a CallIntent,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Authorization, ClientError>> + Send + 'a>,
    > {
        Box::pin(async {
            Ok(Authorization::Approved {
                digest: "forged".into(),
            })
        })
    }
}
#[tokio::test]
async fn mismatched_approval_never_reaches_server() {
    let fixture = Fixture::new(Fake::one_tool()).await;
    let mut client = Client::connect(
        fixture.config.clone(),
        &fixture.secrets,
        Arc::new(WrongDigest),
    )
    .await
    .unwrap();
    assert_eq!(
        client.call("notes__read", json!({})).await.unwrap_err(),
        ClientError::ApprovalFailed
    );
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    client.disconnect().await;
}

#[tokio::test]
async fn failures_are_not_replayed_and_explicit_reconnect_preserves_statistics() {
    for status in [401, 404, 500] {
        let fixture = Fixture::new(Fake::one_tool()).await;
        let mut client = fixture.client().await;
        client.set_permission("read", Permission::Allow).unwrap();
        fixture.fake.fail_call.store(status, Ordering::SeqCst);
        assert_eq!(
            client.call("notes__read", json!({})).await.unwrap_err(),
            ClientError::OutcomeUnknown
        );
        assert_eq!(
            fixture.fake.calls.load(Ordering::SeqCst),
            1,
            "status {status} must not replay tools/call"
        );
        assert!(!client.is_connected());
        assert!(client.tools(&HashSet::new(), 64).is_empty());
        fixture.fake.fail_call.store(0, Ordering::SeqCst);
        client.reconnect(&fixture.secrets).await.unwrap();
        assert_eq!(
            fixture.fake.calls.load(Ordering::SeqCst),
            1,
            "reconnect must not replay the previous call"
        );
        client.call("notes__read", json!({})).await.unwrap();
        assert_eq!(client.stats()["notes__read"].calls, 2);
        assert_eq!(client.stats()["notes__read"].failures, 1);
        client.disconnect().await;
    }
}

#[tokio::test]
async fn invalid_catalogs_are_rejected_without_tool_calls() {
    for kind in ["duplicate", "too_many", "invalid_name"] {
        let fake = Fake::one_tool();
        let tool = fake.tools.lock().unwrap()[0].clone();
        *fake.tools.lock().unwrap() = match kind {
            "duplicate" => vec![tool.clone(), tool],
            "too_many" => (0..257)
                .map(|index| json!({"name":format!("tool{index}"),"inputSchema":{"type":"object"}}))
                .collect(),
            _ => vec![json!({"name":"bad.name","inputSchema":{"type":"object"}})],
        };
        let fixture = Fixture::new(fake).await;
        let result = Client::connect(
            fixture.config.clone(),
            &fixture.secrets,
            Arc::new(RequireApproval),
        )
        .await;
        assert!(result.is_err(), "{kind}");
        assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn malformed_arguments_and_failed_heartbeat_fail_closed() {
    let fixture = Fixture::new(Fake::one_tool()).await;
    let mut client = fixture.client().await;
    client.set_permission("read", Permission::Allow).unwrap();
    for arguments in [
        json!(null),
        json!([]),
        json!({"payload":"x".repeat(40_000)}),
    ] {
        assert_eq!(
            client.call("notes__read", arguments).await.unwrap_err(),
            ClientError::InvalidArguments
        );
    }
    assert_eq!(fixture.fake.calls.load(Ordering::SeqCst), 0);
    fixture.fake.fail_ping.store(500, Ordering::SeqCst);
    assert!(client.heartbeat().await.is_err());
    assert!(!client.is_connected());
    assert_eq!(
        client.call("notes__read", json!({})).await.unwrap_err(),
        ClientError::Unavailable
    );
    client.disconnect().await;
}
