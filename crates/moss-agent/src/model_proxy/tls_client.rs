//! NV13 HTTPS-only outbound adapter, NOT wired to production AgentRunner.
//! Destination is a validated, DNS-address-pinned plan. No untrusted code may
//! supply URLs, host headers or a credential. Tests use a local fake TLS server.
use super::egress::{
    EgressDeny, FIXED_URL, MAX_UPSTREAM_BYTES, TLS_HOST, TLS_PORT, VerifiedDestination,
    accept_upstream_result,
};
use reqwest::blocking::{Client, ClientBuilder};
use std::{
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OUTBOUND_BODY: usize = 4 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TlsDeny {
    Invalid,
    PermissionRevoked,
    TransportFailure,
    UpstreamRejected,
    TooLarge,
}

/// Build a client only for the immutable model origin. Critically, the caller
/// MUST use the addresses returned by VerifiedDestination::check, not a new
/// DNS lookup. TLS is authenticated against the ORIGINAL hostname, not IP.
fn client_builder(dest: &VerifiedDestination) -> Result<ClientBuilder, TlsDeny> {
    if dest.sni != TLS_HOST || dest.port != TLS_PORT || dest.dial.is_empty() {
        return Err(TlsDeny::Invalid);
    }
    Ok(Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .resolve_to_addrs(TLS_HOST, &dest.dial))
}

/// Even the trusted transport repeats the fixed test-body validation so a
/// future adapter cannot bypass the ingress policy and forward tool/URL fields.
fn validate_synthetic_body(
    body: &[u8],
    trusted_key: &str,
    alive: &AtomicBool,
) -> Result<(), TlsDeny> {
    if !alive.load(Ordering::Acquire) {
        return Err(TlsDeny::PermissionRevoked);
    }
    if body.is_empty() || body.len() > MAX_OUTBOUND_BODY {
        return Err(TlsDeny::TooLarge);
    }
    if trusted_key.is_empty()
        || trusted_key.len() > 256
        || !trusted_key.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(TlsDeny::Invalid);
    }
    let request: super::SyntheticBody =
        serde_json::from_slice(body).map_err(|_| TlsDeny::Invalid)?;
    if request.model != super::ROUTE_MODEL
        || request.stream
        || request.input.is_empty()
        || request.input.chars().count() > 500
        || request.input.chars().any(char::is_control)
    {
        return Err(TlsDeny::Invalid);
    }
    Ok(())
}

/// Strict TLS adapter for a TRUSTED proxy with credentials held OUTSIDE
/// the Agent container. There is no route to this function from live MOSS.
/// In-flight cancellation is checked before and after synchronous IO, but
/// cannot yet abort a remote request immediately: production stays blocked.
fn send_one(
    client: &Client,
    body: &[u8],
    trusted_key: &str,
    alive: &AtomicBool,
) -> Result<Vec<u8>, TlsDeny> {
    validate_synthetic_body(body, trusted_key, alive)?;
    let mut response = client
        .post(FIXED_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .bearer_auth(trusted_key)
        .body(body.to_vec())
        .send()
        .map_err(|_| TlsDeny::TransportFailure)?;
    if !alive.load(Ordering::Acquire) {
        return Err(TlsDeny::PermissionRevoked);
    }
    // Even 200 responses with a Location header are denied to avoid future
    // accidental redirection through a downstream caller.
    if response.headers().contains_key(reqwest::header::LOCATION) {
        return Err(TlsDeny::UpstreamRejected);
    }
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .ok_or(TlsDeny::UpstreamRejected)?
        .to_string();
    if status != 200 || content_type != "application/json" {
        return Err(TlsDeny::UpstreamRejected);
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut response)
        .take((MAX_UPSTREAM_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| TlsDeny::TransportFailure)?;
    if !alive.load(Ordering::Acquire) {
        return Err(TlsDeny::PermissionRevoked);
    }
    let body = accept_upstream_result(status, &content_type, &bytes).map_err(|e| match e {
        EgressDeny::TooLarge => TlsDeny::TooLarge,
        _ => TlsDeny::UpstreamRejected,
    })?;
    Ok(body.to_vec())
}

/// Asynchronous attempt with prompt cancellation while a trusted upstream is
/// pending. Dropping the in-flight future stops waiting and closes the request
/// stream; bytes already delivered to an upstream cannot be retracted.
pub(super) async fn send_abortable(
    client: &reqwest::Client,
    body: &[u8],
    trusted_key: &str,
    alive: std::sync::Arc<AtomicBool>,
) -> Result<Vec<u8>, TlsDeny> {
    validate_synthetic_body(body, trusted_key, &alive)?;
    // A trusted side must already have consumed one scoped lease before
    // providing the fake credential to this transport (never the container).
    let attempt = async {
        let mut response = client
            .post(FIXED_URL)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .bearer_auth(trusted_key)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| TlsDeny::TransportFailure)?;
        if response.headers().contains_key(reqwest::header::LOCATION) {
            return Err(TlsDeny::UpstreamRejected);
        }
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .ok_or(TlsDeny::UpstreamRejected)?
            .to_owned();
        if status != 200 || content_type != "application/json" {
            return Err(TlsDeny::UpstreamRejected);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TlsDeny::TransportFailure)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_UPSTREAM_BYTES {
                return Err(TlsDeny::TooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        accept_upstream_result(status, &content_type, &bytes)
            .map_err(|_| TlsDeny::UpstreamRejected)?;
        Ok(bytes)
    };
    tokio::pin!(attempt);
    loop {
        if !alive.load(Ordering::Acquire) {
            return Err(TlsDeny::PermissionRevoked);
        }
        tokio::select! {
            biased;
            _ = tokio::time::sleep(Duration::from_millis(25)) => {},
            result = &mut attempt => {
                if !alive.load(Ordering::Acquire) {
                    return Err(TlsDeny::PermissionRevoked);
                }
                return result;
            }
        }
    }
}

fn async_client_builder(dest: &VerifiedDestination) -> Result<reqwest::ClientBuilder, TlsDeny> {
    if dest.sni != TLS_HOST || dest.port != TLS_PORT || dest.dial.is_empty() {
        return Err(TlsDeny::Invalid);
    }
    Ok(reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .resolve_to_addrs(TLS_HOST, &dest.dial))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{
        ServerConfig, ServerConnection, StreamOwned,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    };
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        sync::{Arc, Mutex},
        thread,
    };

    const SYNTHETIC_REQUEST: &[u8] =
        br#"{"model":"nv13-fixture-model","input":"hello","stream":false}"#;
    const TEST_KEY: &str = "NV13_ONLY_SYNTHETIC_TEST_KEY";
    fn fake_tls(
        dns_name: &str,
        status: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> (
        SocketAddr,
        reqwest::Certificate,
        Arc<Mutex<Vec<u8>>>,
        thread::JoinHandle<()>,
    ) {
        fake_tls_with_delay(dns_name, status, content_type, body, Duration::ZERO)
    }
    fn fake_tls_with_delay(
        dns_name: &str,
        status: &str,
        content_type: &str,
        body: Vec<u8>,
        delay: Duration,
    ) -> (
        SocketAddr,
        reqwest::Certificate,
        Arc<Mutex<Vec<u8>>>,
        thread::JoinHandle<()>,
    ) {
        let generated = rcgen::generate_simple_self_signed(vec![dns_name.to_string()]).unwrap();
        let der = generated.cert.der().to_vec();
        let key = generated.key_pair.serialize_der();
        // This workspace compiles rustls with more than one crypto provider.
        // Explicitly select the test provider instead of relying on auto-pick.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let status = status.to_string();
        let content_type = content_type.to_string();
        let task = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let tls = ServerConnection::new(Arc::new(config)).unwrap();
            let mut stream = StreamOwned::new(tls, socket);
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            for _ in 0..8 {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(&buf[..n]);
                        if let Some(cut) = raw.windows(4).position(|p| p == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&raw[..cut]);
                            let length = header.lines().find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .or_else(|| line.strip_prefix("Content-Length:"))
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            });
                            if length.is_some_and(|n| raw.len() >= cut + 4 + n) {
                                break;
                            }
                        }
                    }
                }
            }
            *sink.lock().unwrap() = raw;
            if !sink.lock().unwrap().is_empty() {
                thread::sleep(delay);
                let redirect = if status.starts_with("302") {
                    "Location: http://127.0.0.1:9/not-allowed\r\n"
                } else {
                    ""
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\n{redirect}Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                );
                let _ = stream.write_all(wire.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (
            addr,
            reqwest::Certificate::from_der(&der).unwrap(),
            captured,
            task,
        )
    }
    fn synthetic_destination(addr: SocketAddr) -> VerifiedDestination {
        // Loopback fixture exists only in this cfg(test) module. Public
        // VerifiedDestination::check NEVER permits this address.
        assert!(addr.ip().is_loopback());
        VerifiedDestination {
            sni: TLS_HOST,
            port: TLS_PORT,
            dial: vec![addr],
        }
    }
    fn test_client(addr: SocketAddr, ca: reqwest::Certificate) -> Client {
        client_builder(&synthetic_destination(addr))
            .unwrap()
            .add_root_certificate(ca)
            .build()
            .unwrap()
    }
    #[test]
    fn valid_fake_tls_cert_and_pinned_sni_roundtrip_without_real_model() {
        let body = br#"{"id":"nv13-fake-tls","output":[]}"#.to_vec();
        let (addr, ca, captured, server) =
            fake_tls("api.openai.com", "200 OK", "application/json", body.clone());
        let client = test_client(addr, ca);
        let alive = AtomicBool::new(true);
        let fake_request = br#"{"model":"nv13-fixture-model","input":"hello","stream":false}"#;
        assert_eq!(send_one(&client, fake_request, TEST_KEY, &alive), Ok(body));
        server.join().unwrap();
        let raw = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(raw.contains("POST /v1/responses HTTP/1.1"));
        assert!(raw.to_ascii_lowercase().contains("host: api.openai.com"));
        assert!(raw.to_ascii_lowercase().contains("authorization: bearer "));
        assert!(!raw.contains("MOSS_SESSION_TOKEN"));
    }
    #[test]
    fn self_signed_wrong_server_hostname_is_rejected() {
        let (addr, ca, _, server) = fake_tls(
            "evil.invalid",
            "200 OK",
            "application/json",
            br#"{"id":"wrong-server"}"#.to_vec(),
        );
        let client = test_client(addr, ca);
        let alive = AtomicBool::new(true);
        assert_eq!(
            send_one(&client, SYNTHETIC_REQUEST, TEST_KEY, &alive),
            Err(TlsDeny::TransportFailure)
        );
        server.join().unwrap();
    }
    #[test]
    fn tls_redirect_is_never_followed_and_bad_content_type_denied() {
        for (status, typ) in [("302 Found", "application/json"), ("200 OK", "text/html")] {
            let (addr, ca, _, server) =
                fake_tls("api.openai.com", status, typ, br#"{"id":"fake"}"#.to_vec());
            let client = test_client(addr, ca);
            let alive = AtomicBool::new(true);
            assert_eq!(
                send_one(&client, SYNTHETIC_REQUEST, TEST_KEY, &alive),
                Err(TlsDeny::UpstreamRejected)
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn revoke_while_fake_tls_response_is_pending_never_delivers_result() {
        let body = br#"{"id":"NV13_PENDING_FAKE_RESULT","output":[]}"#.to_vec();
        let (addr, ca, seen, server) = fake_tls_with_delay(
            "api.openai.com",
            "200 OK",
            "application/json",
            body,
            Duration::from_millis(450),
        );
        let client = test_client(addr, ca);
        let live = Arc::new(AtomicBool::new(true));
        let active = live.clone();
        let requester =
            thread::spawn(move || send_one(&client, SYNTHETIC_REQUEST, TEST_KEY, &active));
        let started = std::time::Instant::now();
        while seen.lock().unwrap().is_empty() && started.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !seen.lock().unwrap().is_empty(),
            "fake TLS server never received request"
        );
        live.store(false, Ordering::Release);
        assert_eq!(requester.join().unwrap(), Err(TlsDeny::PermissionRevoked));
        server.join().unwrap();
    }

    #[test]
    fn revoke_during_async_fake_tls_wait_aborts_without_delivering_output() {
        let pending_body = br#"{"id":"NV13_PENDING_MOCK_ONLY"}"#.to_vec();
        let (addr, ca, seen, server) = fake_tls_with_delay(
            "api.openai.com",
            "200 OK",
            "application/json",
            pending_body,
            Duration::from_millis(650),
        );
        let client = async_client_builder(&synthetic_destination(addr))
            .unwrap()
            .add_root_certificate(ca)
            .build()
            .unwrap();
        let live = Arc::new(AtomicBool::new(true));
        let active = live.clone();
        let worker = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                send_abortable(&client, SYNTHETIC_REQUEST, TEST_KEY, active).await
            })
        });
        let wait_started = std::time::Instant::now();
        while seen.lock().unwrap().is_empty() && wait_started.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !seen.lock().unwrap().is_empty(),
            "fake TLS request did not start"
        );
        let cancel_started = std::time::Instant::now();
        live.store(false, Ordering::Release);
        assert_eq!(worker.join().unwrap(), Err(TlsDeny::PermissionRevoked));
        assert!(
            cancel_started.elapsed() < Duration::from_millis(350),
            "async TLS cancellation did not promptly abort"
        );
        server.join().unwrap();
    }

    fn scoped_http_wire(token: &str) -> Vec<u8> {
        format!(
            "POST /v1/responses HTTP/1.1\r\nHost: nv13-model-mock\r\n\
             Authorization: Bearer {token}\r\nX-NV13-Run-ID: tls-run\r\n\
             X-NV13-Device-ID: tls-device\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            SYNTHETIC_REQUEST.len(),
            std::str::from_utf8(SYNTHETIC_REQUEST).unwrap(),
        )
        .into_bytes()
    }

    #[test]
    fn full_tcp_authorization_to_fake_https_and_single_use_response() {
        use std::net::{Shutdown, TcpStream};
        let (upstream, ca, seen, server) = fake_tls(
            "api.openai.com",
            "200 OK",
            "application/json",
            br#"{"id":"NV13_END_TO_END_FAKE_TLS","output":[]}"#.to_vec(),
        );
        let tls = async_client_builder(&synthetic_destination(upstream))
            .unwrap()
            .add_root_certificate(ca)
            .build()
            .unwrap();
        let gate = Arc::new(super::super::OfflineModelProxy::new());
        let permit = Arc::new(AtomicBool::new(true));
        let token = gate
            .issue(
                "tls-run",
                "tls-device",
                permit.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        let request = scoped_http_wire(&token);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ingress = listener.local_addr().unwrap();
        let validator = gate.clone();
        let trusted = permit.clone();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let frame = super::super::transport::read_frame(&mut stream, &trusted).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let reply = runtime
                .block_on(validator.evaluate_through_fake_https(&frame, &tls, TEST_KEY))
                .unwrap();
            stream.write_all(&reply).unwrap();
        });
        let mut stream = TcpStream::connect(ingress).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(&request).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
        let result = String::from_utf8(response).unwrap();
        assert!(result.starts_with("HTTP/1.1 200 OK"));
        assert!(result.contains("NV13_END_TO_END_FAKE_TLS"));
        assert!(!result.contains(&token) && !result.contains(TEST_KEY));
        let upstream_request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert!(upstream_request.contains(TEST_KEY));
        assert!(!upstream_request.contains(&token));
        assert!(!upstream_request.contains("X-NV13-Device-ID"));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let invalid = runtime.block_on(
            gate.evaluate_through_fake_https(
                &request,
                &async_client_builder(&synthetic_destination(upstream))
                    .unwrap()
                    .build()
                    .unwrap(),
                TEST_KEY,
            ),
        );
        assert_eq!(
            invalid,
            Err(super::super::Deny::Unauthenticated),
            "a consumed token must not reach TLS again"
        );
        assert_eq!(
            gate.audit.lock().unwrap().back().unwrap().outcome,
            "synthetic_https_ok"
        );
    }

    #[test]
    fn full_tcp_tls_chain_revoked_midflight_never_returns_success() {
        use std::net::{Shutdown, TcpStream};
        let (upstream, ca, seen, server) = fake_tls_with_delay(
            "api.openai.com",
            "200 OK",
            "application/json",
            br#"{"id":"NV13_MUST_NOT_RETURN"}"#.to_vec(),
            Duration::from_millis(650),
        );
        let tls = async_client_builder(&synthetic_destination(upstream))
            .unwrap()
            .add_root_certificate(ca)
            .build()
            .unwrap();
        let gate = Arc::new(super::super::OfflineModelProxy::new());
        let permit = Arc::new(AtomicBool::new(true));
        let token = gate
            .issue(
                "tls-run",
                "tls-device",
                permit.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        let request = scoped_http_wire(&token);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ingress = listener.local_addr().unwrap();
        let validator = gate.clone();
        let trusted = permit.clone();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let frame = super::super::transport::read_frame(&mut stream, &trusted).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result =
                runtime.block_on(validator.evaluate_through_fake_https(&frame, &tls, TEST_KEY));
            if let Ok(reply) = &result {
                stream.write_all(reply).unwrap();
            }
            result
        });
        let mut stream = TcpStream::connect(ingress).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream.write_all(&request).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let started = std::time::Instant::now();
        while seen.lock().unwrap().is_empty() && started.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !seen.lock().unwrap().is_empty(),
            "fake TLS request not received"
        );
        permit.store(false, Ordering::Release);
        assert_eq!(worker.join().unwrap(), Err(super::super::Deny::Gone));
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        assert!(response.is_empty(), "revoked work leaked TLS result");
        server.join().unwrap();
        assert_ne!(
            gate.audit.lock().unwrap().back().map(|entry| entry.outcome),
            Some("synthetic_https_ok")
        );
    }
    #[test]
    fn tls_response_bound_and_preflight_revocation() {
        let (addr, ca, _, server) = fake_tls(
            "api.openai.com",
            "200 OK",
            "application/json",
            vec![b'a'; MAX_UPSTREAM_BYTES + 1],
        );
        let client = test_client(addr, ca);
        let alive = AtomicBool::new(true);
        assert_eq!(
            send_one(&client, SYNTHETIC_REQUEST, TEST_KEY, &alive),
            Err(TlsDeny::TooLarge)
        );
        server.join().unwrap();

        // A revoked task is rejected before any network socket is opened.
        let generated = rcgen::generate_simple_self_signed(vec![TLS_HOST.to_owned()]).unwrap();
        let ca = reqwest::Certificate::from_der(generated.cert.der().as_ref()).unwrap();
        let client = test_client("127.0.0.1:9".parse().unwrap(), ca);
        let revoked = AtomicBool::new(false);
        assert_eq!(
            send_one(&client, SYNTHETIC_REQUEST, TEST_KEY, &revoked),
            Err(TlsDeny::PermissionRevoked)
        );
    }
}
