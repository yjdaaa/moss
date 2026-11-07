//! Offline-only, bounded one-request HTTP/1.1 TCP transport.
//! No production bind address and no live upstream client.
use super::{
    Deny, MAX_BODY_BYTES, MAX_HEADERS_BYTES, MAX_WIRE_BYTES, OfflineModelProxy, http_response,
};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const READ_POLL: Duration = Duration::from_millis(80);
const FRAME_DEADLINE: Duration = Duration::from_secs(2);
const MAX_CONNECTIONS: usize = 8;

/// Read one length-delimited frame. A second request on the same TCP socket is
/// never executed. Read polls enforce cancellation and a total slowloris cap.
pub(super) fn read_frame(stream: &mut TcpStream, alive: &AtomicBool) -> Result<Vec<u8>, Deny> {
    stream
        .set_read_timeout(Some(READ_POLL))
        .map_err(|_| Deny::Internal)?;
    let started = Instant::now();
    let mut frame = Vec::with_capacity(1024);
    let mut buf = [0u8; 2048];
    let mut required: Option<usize> = None;
    loop {
        if !alive.load(Ordering::Acquire) {
            return Err(Deny::Gone);
        }
        if started.elapsed() >= FRAME_DEADLINE {
            return Err(Deny::Deadline);
        }
        match stream.read(&mut buf) {
            Ok(0) => return Err(Deny::Malformed),
            Ok(count) => {
                if frame.len().saturating_add(count) > MAX_WIRE_BYTES {
                    return Err(Deny::TooLarge);
                }
                frame.extend_from_slice(&buf[..count]);
                if required.is_none() {
                    if let Some(split) = frame.windows(4).position(|part| part == b"\r\n\r\n") {
                        if split > MAX_HEADERS_BYTES {
                            return Err(Deny::TooLarge);
                        }
                        let text =
                            std::str::from_utf8(&frame[..split]).map_err(|_| Deny::Malformed)?;
                        let mut content_length = None;
                        for line in text.split("\r\n").skip(1) {
                            if let Some((name, value)) = line.split_once(':')
                                && name.eq_ignore_ascii_case("Content-Length")
                            {
                                if content_length.is_some() {
                                    return Err(Deny::Malformed);
                                }
                                let value = value.trim();
                                if value.is_empty()
                                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                                {
                                    return Err(Deny::Malformed);
                                }
                                let count: usize = value.parse().map_err(|_| Deny::TooLarge)?;
                                if count > MAX_BODY_BYTES {
                                    return Err(Deny::TooLarge);
                                }
                                content_length = Some(count);
                            }
                        }
                        required = Some(
                            (split + 4)
                                .checked_add(content_length.ok_or(Deny::Malformed)?)
                                .ok_or(Deny::TooLarge)?,
                        );
                    } else if frame.len() > MAX_HEADERS_BYTES + 4 {
                        return Err(Deny::TooLarge);
                    }
                }
                if let Some(required) = required {
                    if frame.len() > required {
                        return Err(Deny::Malformed);
                    }
                    if frame.len() == required {
                        return Ok(frame);
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(_) => return Err(Deny::Malformed),
        }
    }
}

/// Actual TCP I/O, but the only output is a fixed synthetic responder.
/// The per-run validity flag can be revoked while the socket is waiting.
fn serve_one(mut stream: TcpStream, proxy: &OfflineModelProxy, alive: &AtomicBool) {
    let outcome = match read_frame(&mut stream, alive) {
        Ok(frame) if alive.load(Ordering::Acquire) => proxy.handle(&frame),
        Ok(_) => http_response(
            Deny::Gone.status(),
            r#"{"error":{"code":"permission_expired"}}"#,
        ),
        Err(error) => {
            proxy.record_denial(error.label());
            http_response(
                error.status(),
                &format!(r#"{{"error":{{"code":"{}"}}}}"#, error.label()),
            )
        }
    };
    if alive.load(Ordering::Acquire) {
        let _ = stream.set_write_timeout(Some(READ_POLL));
        let _ = stream.write_all(&outcome);
        let _ = stream.flush();
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// A test-owned listener, not a production service or a public proxy.
/// All child socket workers are joined before it returns on cancellation.
fn serve_offline_listener(
    listener: TcpListener,
    proxy: Arc<OfflineModelProxy>,
    alive: Arc<AtomicBool>,
) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    let mut threads: Vec<thread::JoinHandle<()>> = Vec::new();
    while alive.load(Ordering::Acquire) {
        let mut index = 0;
        while index < threads.len() {
            if threads[index].is_finished() {
                let child = threads.swap_remove(index);
                let _ = child.join();
            } else {
                index += 1;
            }
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                if threads.len() >= MAX_CONNECTIONS {
                    let _ = stream.set_write_timeout(Some(READ_POLL));
                    let _ = stream
                        .write_all(&http_response(429, r#"{"error":{"code":"rate_limited"}}"#));
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let cloned = proxy.clone();
                let guard = alive.clone();
                threads.push(thread::spawn(move || serve_one(stream, &cloned, &guard)));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error),
        }
    }
    for child in threads {
        let _ = child.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn connect(addr: SocketAddr, bytes: &[u8], close_write: bool) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        stream.write_all(bytes).unwrap();
        if close_write {
            stream.shutdown(Shutdown::Write).unwrap();
        }
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }
    fn status(raw: &[u8]) -> u16 {
        String::from_utf8_lossy(raw)
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }
    fn wire(token: &str, length: usize) -> Vec<u8> {
        let body = r#"{"model":"nv13-fixture-model","input":"hello","stream":false}"#;
        format!(
            "POST /v1/responses HTTP/1.1\r\nHost: nv13-model-mock\r\n\
             Authorization: Bearer {token}\r\nX-NV13-Run-ID: run-socket\r\n\
             X-NV13-Device-ID: device-socket\r\nContent-Type: application/json\r\n\
             Content-Length: {length}\r\nConnection: close\r\n\r\n{body}"
        )
        .into_bytes()
    }
    fn fixture() -> (
        SocketAddr,
        Arc<OfflineModelProxy>,
        Arc<AtomicBool>,
        thread::JoinHandle<io::Result<()>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(addr.ip().is_loopback());
        let proxy = Arc::new(OfflineModelProxy::new());
        let guard = Arc::new(AtomicBool::new(true));
        let a = proxy.clone();
        let b = guard.clone();
        let worker = thread::spawn(move || serve_offline_listener(listener, a, b));
        (addr, proxy, guard, worker)
    }
    #[test]
    fn fragmented_valid_request_returns_only_synthetic_payload() {
        let (addr, proxy, live, worker) = fixture();
        let token = proxy
            .issue(
                "run-socket",
                "device-socket",
                live.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        let body = r#"{"model":"nv13-fixture-model","input":"hello","stream":false}"#;
        let bytes = wire(&token, body.len());
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        for chunk in bytes.chunks(23) {
            stream.write_all(chunk).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        assert_eq!(status(&response), 200);
        assert!(String::from_utf8_lossy(&response).contains("NV13_FAKE_MODEL_REPLY_ONLY"));
        assert!(!String::from_utf8_lossy(&response).contains(&token));
        assert_eq!(status(&connect(addr, &bytes, true)), 401);
        live.store(false, Ordering::Release);
        worker.join().unwrap().unwrap();
    }
    #[test]
    fn partial_frame_timeout_and_oversized_body_do_not_consume_token() {
        let (addr, proxy, live, worker) = fixture();
        let token = proxy
            .issue(
                "run-socket",
                "device-socket",
                live.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        let body = r#"{"model":"nv13-fixture-model","input":"hello","stream":false}"#;
        let valid = wire(&token, body.len());
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        stream.write_all(&valid[..valid.len() / 2]).unwrap();
        let mut timed_out = Vec::new();
        stream.read_to_end(&mut timed_out).unwrap();
        assert_eq!(status(&timed_out), 408);
        assert_eq!(
            status(&connect(addr, &wire(&token, MAX_BODY_BYTES + 1), true)),
            413
        );
        assert_eq!(status(&connect(addr, &valid, true)), 200);
        live.store(false, Ordering::Release);
        worker.join().unwrap().unwrap();
    }
    #[test]
    fn cancel_during_incomplete_frame_stops_socket_and_listener() {
        let (addr, proxy, live, worker) = fixture();
        let token = proxy
            .issue(
                "run-socket",
                "device-socket",
                live.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(b"POST /v1/responses HTTP/1.1\r\nHost: nv13-model-mock\r\n")
            .unwrap();
        live.store(false, Ordering::Release);
        let started = Instant::now();
        worker.join().unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response);
        assert!(response.is_empty() || status(&response) != 200);
        assert!(proxy.audit.lock().unwrap().len() <= super::super::MAX_AUDIT);
        assert_eq!(status(&proxy.handle(&wire(&token, 61))), 403);
    }
    #[test]
    fn pipelining_is_rejected_and_cannot_replay() {
        let (addr, proxy, live, worker) = fixture();
        let token = proxy
            .issue(
                "run-socket",
                "device-socket",
                live.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
        let body = r#"{"model":"nv13-fixture-model","input":"hello","stream":false}"#;
        let valid = wire(&token, body.len());
        let mut pipelined = valid.clone();
        pipelined.extend_from_slice(&valid);
        assert_eq!(status(&connect(addr, &pipelined, true)), 400);
        assert_eq!(status(&connect(addr, &valid, true)), 200);
        live.store(false, Ordering::Release);
        worker.join().unwrap().unwrap();
    }
}
