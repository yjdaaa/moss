//! NV13 offline model API proxy policy fixture.
//!
//! This component intentionally contains NO live HTTP upstream or real credentials.
//! It is not installed into the production AgentCoordinator. The only admitted
//! route is a hardcoded synthetic /v1/responses responder for negative tests.
//! The future egress service requires an independent TLS/SSRF review.
#![allow(dead_code)]

mod egress;
mod tls_client;
mod transport;
use getrandom::fill;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const ROUTE_HOST: &str = "nv13-model-mock";
const ROUTE_PATH: &str = "/v1/responses";
const ROUTE_MODEL: &str = "nv13-fixture-model";
const MAX_WIRE_BYTES: usize = 16 * 1024;
const MAX_HEADERS_BYTES: usize = 8 * 1024;
const MAX_BODY_BYTES: usize = 4 * 1024;
const MAX_REPLY_BYTES: usize = 8 * 1024;
const MAX_SESSIONS: usize = 16;
const MAX_AUDIT: usize = 32;
const MAX_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deny {
    Malformed,
    Unauthenticated,
    Mismatch,
    Gone,
    Deadline,
    TooLarge,
    RateLimited,
    Internal,
}
impl Deny {
    fn status(self) -> u16 {
        match self {
            Self::Malformed => 400,
            Self::Unauthenticated => 401,
            Self::Mismatch => 403,
            Self::Gone => 403,
            Self::Deadline => 408,
            Self::TooLarge => 413,
            Self::RateLimited => 429,
            Self::Internal => 503,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Malformed => "invalid_request",
            Self::Unauthenticated => "not_authorized",
            Self::Mismatch => "scope_denied",
            Self::Gone => "permission_expired",
            Self::Deadline => "request_timeout",
            Self::TooLarge => "payload_too_large",
            Self::RateLimited => "rate_limited",
            Self::Internal => "proxy_unavailable",
        }
    }
}

#[derive(Debug, Clone)]
struct Permit {
    run_id: String,
    device_id: String,
    expires: Instant,
    alive: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Audit {
    /// SHA-256 digest only: no prompt, credentials, device ID or token.
    run_hash_prefix: String,
    outcome: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyntheticBody {
    model: String,
    input: String,
    stream: bool,
}
struct Parsed<'a> {
    token: &'a str,
    run_id: &'a str,
    device_id: &'a str,
    body: SyntheticBody,
}

fn parse<'a>(wire: &'a [u8]) -> Result<Parsed<'a>, Deny> {
    if wire.len() > MAX_WIRE_BYTES {
        return Err(Deny::TooLarge);
    }
    let split = wire
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(Deny::Malformed)?;
    if split > MAX_HEADERS_BYTES {
        return Err(Deny::TooLarge);
    }
    let head = std::str::from_utf8(&wire[..split]).map_err(|_| Deny::Malformed)?;
    if head.as_bytes().contains(&0) {
        return Err(Deny::Malformed);
    }
    let mut lines = head.split("\r\n");
    // Exact origin-form target: absolute proxy URLs, CONNECT and alternate
    // paths must never be sent to any upstream.
    if lines.next() != Some("POST /v1/responses HTTP/1.1") {
        return Err(Deny::Mismatch);
    }
    let mut host = None;
    let mut auth = None;
    let mut run = None;
    let mut device = None;
    let mut length = None;
    let mut content_type = None;
    let mut connection = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Deny::Malformed)?;
        if name.is_empty()
            || name.trim() != name
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Deny::Malformed);
        }
        let value = value.trim();
        if value.contains(['\r', '\n', '\0']) {
            return Err(Deny::Malformed);
        }
        // Reject every unrecognized header (including forwarded, proxy auth,
        // transfer-encoding, x-http-method-override, websocket upgrades).
        let target = match name.to_ascii_lowercase().as_str() {
            "host" => &mut host,
            "authorization" => &mut auth,
            "x-nv13-run-id" => &mut run,
            "x-nv13-device-id" => &mut device,
            "content-length" => &mut length,
            "content-type" => &mut content_type,
            "connection" => &mut connection,
            _ => return Err(Deny::Mismatch),
        };
        if target.replace(value).is_some() {
            return Err(Deny::Malformed);
        }
    }
    if host != Some(ROUTE_HOST)
        || content_type != Some("application/json")
        || !matches!(connection, None | Some("close"))
    {
        return Err(Deny::Mismatch);
    }
    let token = auth
        .and_then(|header| header.strip_prefix("Bearer "))
        .ok_or(Deny::Unauthenticated)?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Deny::Unauthenticated);
    }
    let run_id = run.ok_or(Deny::Mismatch)?;
    let device_id = device.ok_or(Deny::Mismatch)?;
    if !valid_label(run_id) || !valid_label(device_id) {
        return Err(Deny::Mismatch);
    }
    let size_text = length.ok_or(Deny::Malformed)?;
    if size_text.is_empty() || !size_text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Deny::Malformed);
    }
    let size: usize = size_text.parse().map_err(|_| Deny::TooLarge)?;
    if size > MAX_BODY_BYTES {
        return Err(Deny::TooLarge);
    }
    let body = &wire[split + 4..];
    if body.len() != size {
        return Err(Deny::Malformed);
    }
    let body: SyntheticBody = serde_json::from_slice(body).map_err(|_| Deny::Malformed)?;
    if body.model != ROUTE_MODEL
        || body.stream
        || body.input.is_empty()
        || body.input.chars().count() > 500
        || body.input.chars().any(char::is_control)
    {
        return Err(Deny::Mismatch);
    }
    Ok(Parsed {
        token,
        run_id,
        device_id,
        body,
    })
}

fn valid_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn token_digest(token: &str) -> [u8; 32] {
    let hash = Sha256::digest(token.as_bytes());
    let mut array = [0u8; 32];
    array.copy_from_slice(hash.as_slice());
    array
}
fn hash_prefix(run_id: &str) -> String {
    let hash = Sha256::digest(run_id.as_bytes());
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Only the trusted host can allocate run-bound leases. No user-controlled
/// route, upstream URL, HTTP client, API key, DNS resolver or auth file exists.
struct OfflineModelProxy {
    sessions: Mutex<HashMap<[u8; 32], Permit>>,
    audit: Mutex<VecDeque<Audit>>,
}
impl OfflineModelProxy {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            audit: Mutex::new(VecDeque::new()),
        }
    }

    fn issue(
        &self,
        run_id: &str,
        device_id: &str,
        alive: Arc<AtomicBool>,
        ttl: Duration,
    ) -> Result<String, Deny> {
        if !valid_label(run_id)
            || !valid_label(device_id)
            || ttl.is_zero()
            || ttl > MAX_TTL
            || !alive.load(Ordering::Acquire)
        {
            return Err(Deny::Mismatch);
        }
        let mut leases = self.sessions.lock().map_err(|_| Deny::Internal)?;
        leases.retain(|_, lease| {
            lease.expires > Instant::now() && lease.alive.load(Ordering::Acquire)
        });
        if leases.len() >= MAX_SESSIONS || leases.values().any(|lease| lease.run_id == run_id) {
            return Err(Deny::RateLimited);
        }
        // OS CSPRNG, never timestamp- or counter-derived. Digest only is retained.
        let mut random = [0u8; 32];
        fill(&mut random).map_err(|_| Deny::Internal)?;
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let digest = token_digest(&token);
        // Never replace a previous lease, even in the extremely unlikely
        // case of a secure random collision.
        if leases.contains_key(&digest) {
            return Err(Deny::Internal);
        }
        leases.insert(
            digest,
            Permit {
                run_id: run_id.into(),
                device_id: device_id.into(),
                expires: Instant::now() + ttl,
                alive,
            },
        );
        Ok(token)
    }

    fn revoke(&self, run_id: &str) -> Result<(), Deny> {
        let mut leases = self.sessions.lock().map_err(|_| Deny::Internal)?;
        leases.retain(|_, lease| lease.run_id != run_id);
        Ok(())
    }

    /// End-to-end policy evaluation using a *synthetic* no-network responder.
    /// A valid token is consumed atomically before any fake work takes place.
    fn handle(&self, wire: &[u8]) -> Vec<u8> {
        match self.evaluate(wire) {
            Ok(reply) => http_response(200, &reply),
            Err(error) => {
                // Denied wire payload is never logged; only a bounded fixed
                // reason code is recorded, without identifying data.
                self.record_denial(error.label());
                http_response(
                    error.status(),
                    &json!({"error":{"code":error.label()}}).to_string(),
                )
            }
        }
    }

    /// Atomically authorize one scoped HTTP request and return ONLY its
    /// validated JSON body to a trusted transport. Never forward client token
    /// or device headers to any upstream.
    fn authorize_wire(&self, wire: &[u8]) -> Result<(Permit, Vec<u8>), Deny> {
        let request = parse(wire)?;
        let key = token_digest(request.token);
        let mut leases = self.sessions.lock().map_err(|_| Deny::Internal)?;
        let lease = leases.get(&key).ok_or(Deny::Unauthenticated)?;
        if lease.run_id != request.run_id || lease.device_id != request.device_id {
            return Err(Deny::Mismatch);
        }
        if lease.expires <= Instant::now() || !lease.alive.load(Ordering::Acquire) {
            leases.remove(&key);
            return Err(Deny::Gone);
        }
        // Consume the capability before a potentially slow TLS operation.
        let lease = leases.remove(&key).ok_or(Deny::Unauthenticated)?;
        drop(leases);
        if !lease.alive.load(Ordering::Acquire) {
            return Err(Deny::Gone);
        }
        let split = wire
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .ok_or(Deny::Malformed)?;
        let body = wire[split + 4..].to_vec();
        // Parsing the synthetic request guarantees a small bounded body.
        let _ = request.body.input.len();
        Ok((lease, body))
    }

    fn evaluate(&self, wire: &[u8]) -> Result<String, Deny> {
        let (lease, _body) = self.authorize_wire(wire)?;
        let fake_reply = json!({
            "id":"nv13-synthetic-response",
            "object":"response",
            "model":ROUTE_MODEL,
            "output":[{"type":"message","role":"assistant",
                "content":[{"type":"output_text","text":"NV13_FAKE_MODEL_REPLY_ONLY"}]}],
            "usage":{"input_tokens":0,"output_tokens":0}
        })
        .to_string();
        if fake_reply.len() > MAX_REPLY_BYTES {
            return Err(Deny::Internal);
        }
        if !lease.alive.load(Ordering::Acquire) {
            return Err(Deny::Gone);
        }
        self.record_audit(&lease.run_id, "synthetic_ok")?;
        Ok(fake_reply)
    }

    /// Test-only integrated path: untrusted TCP HTTP framing -> one-time
    /// approved lease -> pinned TLS fake server -> bounded response. This
    /// cannot reach a real model and is not exported to AgentCoordinator.
    #[cfg(test)]
    async fn evaluate_through_fake_https(
        &self,
        wire: &[u8],
        client: &reqwest::Client,
        fake_trusted_key: &str,
    ) -> Result<Vec<u8>, Deny> {
        let (lease, body) = self.authorize_wire(wire)?;
        let answer =
            tls_client::send_abortable(client, &body, fake_trusted_key, lease.alive.clone())
                .await
                .map_err(|error| match error {
                    tls_client::TlsDeny::PermissionRevoked => Deny::Gone,
                    tls_client::TlsDeny::TooLarge => Deny::TooLarge,
                    _ => Deny::Internal,
                })?;
        if !lease.alive.load(Ordering::Acquire) {
            return Err(Deny::Gone);
        }
        if answer.len() > MAX_REPLY_BYTES {
            return Err(Deny::TooLarge);
        }
        self.record_audit(&lease.run_id, "synthetic_https_ok")?;
        Ok(http_response(
            200,
            std::str::from_utf8(&answer).map_err(|_| Deny::Internal)?,
        ))
    }

    fn record_denial(&self, outcome: &'static str) {
        if let Ok(mut audit) = self.audit.lock() {
            if audit.len() == MAX_AUDIT {
                audit.pop_front();
            }
            audit.push_back(Audit {
                run_hash_prefix: "unattributed".into(),
                outcome,
            });
        }
    }

    fn record_audit(&self, run_id: &str, outcome: &'static str) -> Result<(), Deny> {
        let mut audit = self.audit.lock().map_err(|_| Deny::Internal)?;
        if audit.len() == MAX_AUDIT {
            audit.pop_front();
        }
        audit.push_back(Audit {
            run_hash_prefix: hash_prefix(run_id),
            outcome,
        });
        Ok(())
    }
}
fn http_response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        _ => "Service Unavailable",
    };
    let mut reply = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body.as_bytes());
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpListener, TcpStream},
        thread,
    };

    fn wire(token: &str, run: &str, device: &str, data: &str) -> Vec<u8> {
        format!(
            "POST {ROUTE_PATH} HTTP/1.1\r\nHost: {ROUTE_HOST}\r\n\
             Authorization: Bearer {token}\r\nX-NV13-Run-ID: {run}\r\n\
             X-NV13-Device-ID: {device}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{data}",
            data.len()
        )
        .into_bytes()
    }
    fn payload() -> &'static str {
        r#"{"model":"nv13-fixture-model","input":"test synthetic input","stream":false}"#
    }
    fn allowed() -> (OfflineModelProxy, String, Arc<AtomicBool>) {
        let proxy = OfflineModelProxy::new();
        let permit = Arc::new(AtomicBool::new(true));
        let token = proxy
            .issue(
                "run-ok",
                "device-ok",
                permit.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        (proxy, token, permit)
    }
    fn status(reply: &[u8]) -> u16 {
        let line = std::str::from_utf8(reply).unwrap().lines().next().unwrap();
        line.split_whitespace().nth(1).unwrap().parse().unwrap()
    }
    #[test]
    fn one_time_token_scoped_to_run_device_and_expiry() {
        let (proxy, token, permit) = allowed();
        let base = wire(&token, "run-ok", "device-ok", payload());
        assert_eq!(
            status(&proxy.handle(&wire(&token, "other", "device-ok", payload()))),
            403
        );
        assert_eq!(
            status(&proxy.handle(&wire(&token, "run-ok", "other", payload()))),
            403
        );
        let response = proxy.handle(&base);
        assert_eq!(status(&response), 200);
        assert!(String::from_utf8_lossy(&response).contains("NV13_FAKE_MODEL_REPLY_ONLY"));
        assert!(!String::from_utf8_lossy(&response).contains("test synthetic input"));
        assert!(!String::from_utf8_lossy(&response).contains(&token));
        assert_eq!(status(&proxy.handle(&base)), 401);
        assert_eq!(proxy.audit.lock().unwrap().len(), 4);
        let audit = proxy.audit.lock().unwrap()[2].clone();
        assert_eq!(
            proxy.audit.lock().unwrap()[0].run_hash_prefix,
            "unattributed"
        );
        assert!(proxy.audit.lock().unwrap().iter().all(|entry| {
            !entry.run_hash_prefix.contains(&token) && !entry.outcome.contains(&token)
        }));
        assert_eq!(audit.outcome, "synthetic_ok");
        assert!(!audit.run_hash_prefix.contains("run-ok"));
        assert!(permit.load(Ordering::Acquire));

        let token2 = proxy
            .issue(
                "run-cancel",
                "device-ok",
                permit.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        permit.store(false, Ordering::Release);
        assert_eq!(
            status(&proxy.handle(&wire(&token2, "run-cancel", "device-ok", payload()))),
            403
        );
        assert!(
            proxy
                .issue(
                    "run-new",
                    "device-ok",
                    permit.clone(),
                    Duration::from_secs(5)
                )
                .is_err()
        );
    }

    #[test]
    fn token_revoke_timeout_and_non_replay() {
        let (proxy, token, permit) = allowed();
        proxy.revoke("run-ok").unwrap();
        assert_eq!(
            status(&proxy.handle(&wire(&token, "run-ok", "device-ok", payload()))),
            401
        );
        let token = proxy
            .issue("run-short", "device-ok", permit, Duration::from_millis(12))
            .unwrap();
        thread::sleep(Duration::from_millis(30));
        assert_eq!(
            status(&proxy.handle(&wire(&token, "run-short", "device-ok", payload()))),
            403
        );
    }

    #[test]
    fn never_redirect_proxy_connect_other_headers_methods_or_paths() {
        let (proxy, token, _) = allowed();
        let valid = String::from_utf8(wire(&token, "run-ok", "device-ok", payload())).unwrap();
        let replacements = [
            ("POST /v1/responses HTTP/1.1", "GET /v1/responses HTTP/1.1"),
            (
                "POST /v1/responses HTTP/1.1",
                "CONNECT api.openai.com:443 HTTP/1.1",
            ),
            (
                "POST /v1/responses HTTP/1.1",
                "POST https://api.openai.com/v1/responses HTTP/1.1",
            ),
            (
                "POST /v1/responses HTTP/1.1",
                "POST /v1/responses/ HTTP/1.1",
            ),
            (
                "POST /v1/responses HTTP/1.1",
                "POST /v1/%72esponses HTTP/1.1",
            ),
            ("Host: nv13-model-mock", "Host: 169.254.169.254"),
            ("Host: nv13-model-mock", "Host: localhost"),
            ("Content-Type: application/json", "Content-Type: text/plain"),
            ("Connection: close", "Connection: keep-alive"),
        ];
        for (old, new) in replacements {
            assert_eq!(
                status(&proxy.handle(valid.replace(old, new).as_bytes())),
                403,
                "attack replacement should fail: {new}"
            );
        }
        for injected in [
            "Transfer-Encoding: chunked",
            "Proxy-Authorization: Basic secret",
            "X-Forwarded-Host: api.openai.com",
            "X-HTTP-Method-Override: GET",
            "Upgrade: websocket",
            "Authorization: Bearer should-not-repeat",
            "Content-Length: 0",
            "Host: localhost",
            "Cookie: private",
            "Expect: 100-continue",
            "X-Target-URL: http://127.0.0.1",
        ] {
            let bad = valid.replace("\r\n\r\n", &format!("\r\n{injected}\r\n\r\n"));
            assert_ne!(status(&proxy.handle(bad.as_bytes())), 200, "{injected}");
        }
        assert_eq!(
            status(&proxy.handle(valid.as_bytes())),
            200,
            "denied requests may not consume the one-time token"
        );
    }

    #[test]
    fn body_must_match_exact_synthetic_schema_and_budget() {
        let (proxy, token, _) = allowed();
        for body in [
            r#"{"model":"gpt-6","input":"hi","stream":false}"#,
            r#"{"model":"nv13-fixture-model","input":"hi","stream":true}"#,
            r#"{"model":"nv13-fixture-model","input":"hi","stream":false,"tools":[]}"#,
            r#"{"model":"nv13-fixture-model","input":"hi","stream":false,"url":"http://127.0.0.1"}"#,
            r#"{"model":"nv13-fixture-model","model":"nv13-fixture-model","input":"hi","stream":false}"#,
            r#"{"model":"nv13-fixture-model","input":"","stream":false}"#,
            r#"{"model":"nv13-fixture-model","input":"x\nx","stream":false}"#,
            "not-json",
        ] {
            assert_ne!(
                status(&proxy.handle(&wire(&token, "run-ok", "device-ok", body))),
                200
            );
        }
        let too_long = format!(
            r#"{{"model":"nv13-fixture-model","input":"{}","stream":false}}"#,
            "x".repeat(600)
        );
        assert_ne!(
            status(&proxy.handle(&wire(&token, "run-ok", "device-ok", &too_long))),
            200
        );
        let over_limit = "X".repeat(MAX_BODY_BYTES + 1);
        assert_eq!(
            status(&proxy.handle(&wire(&token, "run-ok", "device-ok", &over_limit))),
            413
        );
        assert_eq!(
            status(&proxy.handle(&wire(&token, "run-ok", "device-ok", payload()))),
            200
        );
    }

    #[test]
    fn bounded_wire_rejects_request_smuggling_and_missing_body() {
        let (proxy, token, _) = allowed();
        let valid = String::from_utf8(wire(&token, "run-ok", "device-ok", payload())).unwrap();
        for bad in [
            valid.replace("\r\n\r\n", "\r\nTransfer-Encoding: chunked\r\n\r\n"),
            valid.replace("\r\n\r\n", "\r\nContent-Length: 0\r\n\r\n"),
            valid.replace("Content-Length: ", "Content-Length: +"),
            valid.replace("\r\n\r\n", "\r\n\r\nEXTRA"),
            valid.replace("\r\n\r\n", "\r\nFolded: x\r\n continuation\r\n\r\n"),
        ] {
            assert_ne!(status(&proxy.handle(bad.as_bytes())), 200);
        }
        assert_eq!(status(&proxy.handle(&vec![b'A'; MAX_WIRE_BYTES + 1])), 413);
        assert_eq!(status(&proxy.handle(valid.as_bytes())), 200);
    }

    #[test]
    fn only_one_concurrent_request_consumes_a_lease() {
        let (proxy, token, _) = allowed();
        let proxy = Arc::new(proxy);
        let mut threads = Vec::new();
        for _ in 0..12 {
            let proxy = proxy.clone();
            let request = wire(&token, "run-ok", "device-ok", payload());
            threads.push(thread::spawn(move || status(&proxy.handle(&request))));
        }
        let statuses = threads
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(statuses.iter().filter(|code| **code == 200).count(), 1);
        assert!(statuses.iter().all(|code| matches!(code, 200 | 401)));
    }

    #[test]
    fn a_real_loopback_socket_receives_only_synthetic_response() {
        let (proxy, token, _) = allowed();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut client, _) = listener.accept().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut raw = Vec::new();
            std::io::Read::by_ref(&mut client)
                .take((MAX_WIRE_BYTES + 1) as u64)
                .read_to_end(&mut raw)
                .unwrap();
            let response = proxy.handle(&raw);
            client.write_all(&response).unwrap();
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let request = wire(&token, "run-ok", "device-ok", payload());
        client.write_all(&request).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        handle.join().unwrap();
        assert_eq!(status(&response), 200);
        let text = String::from_utf8_lossy(&response);
        assert!(text.contains("Cache-Control: no-store"));
        assert!(text.contains("NV13_FAKE_MODEL_REPLY_ONLY"));
        assert!(!text.contains(&token));
    }

    #[test]
    fn budget_capacity_and_untrusted_issuance_rejected() {
        let proxy = OfflineModelProxy::new();
        let alive = Arc::new(AtomicBool::new(true));
        assert_eq!(
            proxy
                .issue("run", "device", alive.clone(), Duration::ZERO)
                .unwrap_err(),
            Deny::Mismatch
        );
        assert_eq!(
            proxy
                .issue(
                    "run",
                    "device",
                    alive.clone(),
                    MAX_TTL + Duration::from_secs(1)
                )
                .unwrap_err(),
            Deny::Mismatch
        );
        assert_eq!(
            proxy
                .issue("../../../", "device", alive.clone(), MAX_TTL)
                .unwrap_err(),
            Deny::Mismatch
        );
        let mut secrets = Vec::new();
        for i in 0..MAX_SESSIONS {
            secrets.push(
                proxy
                    .issue(&format!("run-{i}"), "device", alive.clone(), MAX_TTL)
                    .unwrap(),
            );
        }
        assert_eq!(
            proxy
                .issue("run-extra", "device", alive.clone(), MAX_TTL)
                .unwrap_err(),
            Deny::RateLimited
        );
        assert_eq!(
            proxy
                .issue("run-0", "device", alive.clone(), MAX_TTL)
                .unwrap_err(),
            Deny::RateLimited
        );
        assert!(secrets.windows(2).all(|pair| pair[0] != pair[1]));
    }
}
