//! NV15 fake TLS Home Assistant: actual HTTPS bytes, no external devices.
//! This fixture is TEST-ONLY and never sends anything to a real HA instance.
use super::*;
use reqwest::redirect::Policy;
use rustls::{
    ServerConfig, ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SYNTHETIC_BEARER: &str = "NV15_ONLY_SYNTHETIC_FAKE_HOME_TOKEN_012345";

struct FakeHttps {
    transport: HttpsHomeTransport,
    seen: Arc<Mutex<Vec<u8>>>,
    worker: JoinHandle<()>,
}
impl FakeHttps {
    fn finish(self) -> Vec<u8> {
        self.worker.join().expect("synthetic TLS worker panicked");
        self.seen.lock().unwrap().clone()
    }
}
fn fake_ha_https(
    cert_name: &str,
    status: &str,
    mime: &str,
    body: Vec<u8>,
    wait: Duration,
    location: Option<&str>,
) -> FakeHttps {
    let cert = rcgen::generate_simple_self_signed(vec![cert_name.to_owned()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.key_pair.serialize_der();
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let tls_config = ServerConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        )
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let status = status.to_owned();
    let mime = mime.to_owned();
    let location = location.map(str::to_owned);
    let worker = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let conn = ServerConnection::new(Arc::new(tls_config)).unwrap();
        let mut stream = StreamOwned::new(conn, socket);
        let mut raw = Vec::new();
        let mut packet = [0u8; 2048];
        for _ in 0..12 {
            match stream.read(&mut packet) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    raw.extend_from_slice(&packet[..count]);
                    if let Some(offset) = raw.windows(4).position(|p| p == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&raw[..offset]);
                        let declared = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                if key.eq_ignore_ascii_case("content-length") {
                                    value.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if raw.len() >= offset + 4 + declared {
                            break;
                        }
                    }
                }
            }
        }
        *recorded.lock().unwrap() = raw;
        if recorded.lock().unwrap().is_empty() {
            return;
        }
        thread::sleep(wait);
        let mut wire = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        if let Some(url) = location {
            wire.push_str(&format!("Location: {url}\r\n"));
        }
        wire.push_str("\r\n");
        let _ = stream.write_all(wire.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    });
    let client = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(Policy::none())
        .resolve("ha.example.test", addr)
        .add_root_certificate(reqwest::Certificate::from_der(&cert_der).unwrap())
        .connect_timeout(Duration::from_millis(900))
        .timeout(Duration::from_secs(7))
        .build()
        .unwrap();
    let origin = Url::parse(&format!("https://ha.example.test:{}/", addr.port())).unwrap();
    FakeHttps {
        transport: HttpsHomeTransport {
            origin,
            bearer: SYNTHETIC_BEARER.to_owned(),
            client,
        },
        seen,
        worker,
    }
}

#[test]
fn nv15_synthetic_tls_service_post_exactly_once_and_targeted_state_get() {
    let mut fake = fake_ha_https(
        "ha.example.test",
        "200 OK",
        "application/json",
        br#"[{"entity_id":"light.other","state":"on"}]"#.to_vec(),
        Duration::ZERO,
        None,
    );
    assert!(
        fake.transport
            .submit_once(
                "/api/services/light/turn_on",
                &serde_json::json!({"entity_id":"light.desk"})
            )
            .is_ok()
    );
    let record = String::from_utf8(fake.finish()).unwrap();
    assert!(record.starts_with("POST /api/services/light/turn_on HTTP/1.1"));
    assert!(record.contains("light.desk"));
    assert!(
        record
            .to_ascii_lowercase()
            .contains("authorization: bearer ")
    );
    // The service's changed-states list contained only light.other. It is
    // NOT evidence for the target. Only a separate exact GET may confirm.
    let mut fake = fake_ha_https(
        "ha.example.test",
        "200 OK",
        "application/json",
        br#"{"entity_id":"light.desk","state":"on","attributes":{"token":"redacted"}}"#.to_vec(),
        Duration::ZERO,
        None,
    );
    assert_eq!(
        fake.transport
            .read_target_state("light.desk", Duration::from_millis(900)),
        Ok("on".to_owned())
    );
    let record = String::from_utf8(fake.finish()).unwrap();
    assert!(record.starts_with("GET /api/states/light.desk HTTP/1.1"));
    assert!(!record.contains("attributes"));
}

#[test]
fn nv15_synthetic_tls_wrong_certificate_hostname_must_fail() {
    let mut fake = fake_ha_https(
        "different-host.invalid",
        "200 OK",
        "application/json",
        br#"{"entity_id":"light.desk","state":"on"}"#.to_vec(),
        Duration::ZERO,
        None,
    );
    assert!(
        fake.transport
            .read_target_state("light.desk", Duration::from_millis(900))
            .is_err()
    );
    let captured = fake.finish();
    assert!(
        captured.is_empty(),
        "failed TLS verification must not send HTTP data"
    );
}

#[test]
fn nv15_synthetic_tls_redirect_wrong_entity_html_and_large_json_are_rejected() {
    let scenarios = vec![
        (
            "302 Found",
            "application/json",
            br#"[]"#.to_vec(),
            Some("http://127.0.0.1:9/not-allowed"),
        ),
        ("200 OK", "text/html", b"<html>not HA</html>".to_vec(), None),
        (
            "200 OK",
            "application/json",
            br#"{"entity_id":"light.another","state":"on"}"#.to_vec(),
            None,
        ),
        (
            "200 OK",
            "application/json",
            vec![b'X'; MAX_REPLY + 1],
            None,
        ),
    ];
    for (status, mime, payload, redirect) in scenarios {
        let mut fake = fake_ha_https(
            "ha.example.test",
            status,
            mime,
            payload,
            Duration::ZERO,
            redirect,
        );
        assert!(
            fake.transport
                .read_target_state("light.desk", Duration::from_millis(900))
                .is_err()
        );
        let captured = fake.finish();
        assert!(!captured.is_empty());
    }
}

#[test]
fn nv15_synthetic_tls_response_deadline_is_enforced() {
    let mut fake = fake_ha_https(
        "ha.example.test",
        "200 OK",
        "application/json",
        br#"{"entity_id":"light.desk","state":"on"}"#.to_vec(),
        Duration::from_millis(650),
        None,
    );
    let started = Instant::now();
    assert!(
        fake.transport
            .read_target_state("light.desk", Duration::from_millis(120))
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_millis(600));
    fake.finish();
    let mut h = HttpsHomeTransport::new("https://ha.example.test/", SYNTHETIC_BEARER).unwrap();
    assert!(h.read_target_state("light.desk", Duration::ZERO).is_err());
    assert!(
        h.read_target_state("light.desk", Duration::from_secs(1))
            .is_err()
    );
}
