//! NV15 local HTTP mock fixture, compiled ONLY under Rust tests.
//! Not a production network adapter; no real Home Assistant endpoints.
use super::{AllowedEntity, HomeCatalog, HomeDeny, MAX_SCHEMA_BYTES};
use reqwest::{Url, blocking::Client, redirect::Policy};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const FAKE_BEARER: &str = "nv15-synthetic-test-only";

fn get_synthetic(origin: &str, path: &str) -> Result<Vec<u8>, HomeDeny> {
    let base = Url::parse(origin).map_err(|_| HomeDeny::InvalidConfig)?;
    if base.scheme() != "http"
        || base.host_str() != Some("127.0.0.1")
        || !base.username().is_empty()
        || base.password().is_some()
        || base.path() != "/"
        || base.query().is_some()
        || base.fragment().is_some()
        || !matches!(path, "/api/states" | "/api/services")
    {
        return Err(HomeDeny::InvalidConfig);
    }
    let endpoint = base.join(path).map_err(|_| HomeDeny::InvalidConfig)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| HomeDeny::InvalidConfig)?;
    let response = client
        .get(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .bearer_auth(FAKE_BEARER)
        .send()
        .map_err(|_| HomeDeny::InvalidSchema)?;
    if response.status() != reqwest::StatusCode::OK
        || response.headers().contains_key(reqwest::header::LOCATION)
        || response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some("application/json")
    {
        return Err(HomeDeny::InvalidSchema);
    }
    let mut bytes = Vec::new();
    response
        .take((MAX_SCHEMA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| HomeDeny::InvalidSchema)?;
    if bytes.len() > MAX_SCHEMA_BYTES {
        return Err(HomeDeny::InvalidSchema);
    }
    Ok(bytes)
}
fn sync_synthetic(origin: &str, allowlist: Vec<AllowedEntity>) -> Result<HomeCatalog, HomeDeny> {
    let states = get_synthetic(origin, "/api/states")?;
    let services = get_synthetic(origin, "/api/services")?;
    HomeCatalog::from_snapshots(&states, &services, allowlist, 11, 12)
}
struct FakeReply {
    code: u16,
    mime: &'static str,
    bytes: Vec<u8>,
    redirect: bool,
}
fn fake_server(
    replies: Vec<FakeReply>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let handle = thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            for _ in 0..5 {
                let count = stream.read(&mut buf).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..count]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(request.len() <= 8192);
            let message = String::from_utf8(request).unwrap();
            assert!(message.starts_with("GET /api/"));
            captured.lock().unwrap().push(message);
            let location = if reply.redirect {
                "Location: http://127.0.0.1:9/private\r\n"
            } else {
                ""
            };
            let header = format!(
                "HTTP/1.1 {} Response\r\nContent-Type: {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                reply.code,
                reply.mime,
                location,
                reply.bytes.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&reply.bytes);
        }
    });
    (addr, seen, handle)
}
#[test]
fn fake_ha_discovery_uses_exactly_two_trusted_get_routes() {
    let replies = vec![
        FakeReply {
            code: 200,
            mime: "application/json",
            bytes: super::tests::sample_states(),
            redirect: false,
        },
        FakeReply {
            code: 200,
            mime: "application/json",
            bytes: super::tests::sample_services(),
            redirect: false,
        },
    ];
    let (addr, seen, worker) = fake_server(replies);
    let catalog = sync_synthetic(&format!("http://{addr}/"), super::tests::selected()).unwrap();
    assert_eq!(catalog.selected_count(), 8);
    worker.join().unwrap();
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/states HTTP/1.1"));
    assert!(requests[1].starts_with("GET /api/services HTTP/1.1"));
    for request in requests.iter() {
        assert!(request.contains(FAKE_BEARER));
        assert!(!request.contains("POST "));
    }
}
#[test]
fn mock_http_denies_redirect_auth_error_html_and_oversized_reply() {
    for reply in [
        FakeReply {
            code: 302,
            mime: "application/json",
            bytes: b"{}".to_vec(),
            redirect: true,
        },
        FakeReply {
            code: 401,
            mime: "application/json",
            bytes: b"{}".to_vec(),
            redirect: false,
        },
        FakeReply {
            code: 200,
            mime: "text/html",
            bytes: b"<html>error</html>".to_vec(),
            redirect: false,
        },
        FakeReply {
            code: 200,
            mime: "application/json",
            bytes: vec![b'x'; MAX_SCHEMA_BYTES + 1],
            redirect: false,
        },
    ] {
        let (addr, seen, worker) = fake_server(vec![reply]);
        assert_eq!(
            get_synthetic(&format!("http://{addr}/"), "/api/states"),
            Err(HomeDeny::InvalidSchema)
        );
        worker.join().unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
    for origin in [
        "https://api.openai.com/",
        "http://localhost:8888/",
        "http://127.0.0.1:8000/v1/",
        "http://user:pass@127.0.0.1:8000/",
        "http://169.254.169.254/",
        "http://192.168.1.5/",
        "http://127.0.0.1:8000/?target=secret",
        "file:///C:/secrets",
        "http://[::1]:1234/",
    ] {
        assert_eq!(
            get_synthetic(origin, "/api/states"),
            Err(HomeDeny::InvalidConfig)
        );
    }
    assert_eq!(
        get_synthetic("http://127.0.0.1:9/", "/api/template"),
        Err(HomeDeny::InvalidConfig)
    );
}
#[test]
fn disconnected_mock_ha_cannot_infer_state_or_issue_controls() {
    assert!(matches!(
        sync_synthetic("http://127.0.0.1:9/", super::tests::selected()),
        Err(HomeDeny::InvalidSchema)
    ));
}
