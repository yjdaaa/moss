use super::*;

/// 非回环来源最多占 `MAX - RESERVED` 个连接名额，回环可用满全部：局域网慢连接占满时本机急停仍能接入（2026-11-05）。
#[test]
fn http_admission_reserves_slots_for_loopback() {
    let active = std::sync::atomic::AtomicUsize::new(0);
    let remote_limit = MAX_CONCURRENT_HTTP - LOOPBACK_RESERVED_HTTP;
    for _ in 0..remote_limit {
        assert!(try_admit_http_connection(&active, false));
    }
    assert!(
        !try_admit_http_connection(&active, false),
        "非回环不能占用回环预留名额"
    );
    assert_eq!(
        active.load(std::sync::atomic::Ordering::SeqCst),
        remote_limit
    );
    for _ in 0..LOOPBACK_RESERVED_HTTP {
        assert!(try_admit_http_connection(&active, true), "回环必须还能接入");
    }
    assert!(!try_admit_http_connection(&active, true), "总上限仍然生效");
    assert!(!try_admit_http_connection(&active, false));
    assert_eq!(
        active.load(std::sync::atomic::Ordering::SeqCst),
        MAX_CONCURRENT_HTTP
    );
    // 释放一个名额后，非回环在总数仍高于其上限时依旧被拒，回环可以补上
    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    assert!(!try_admit_http_connection(&active, false));
    assert!(try_admit_http_connection(&active, true));
}

#[test]
fn auth_challenge_is_loopback_only_and_issued_code_pairs_remote_client() {
    let auth = std::sync::Arc::new(AuthManager::new());
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "0.0.0.0".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
    .with_auth(auth.clone());
    let challenge_request =
        b"POST /v1/auth/challenge HTTP/1.1\r\nHost: remote\r\nContent-Length: 0\r\n\r\n";
    let denied = service.handle_http_from(challenge_request, false, "10.0.0.8", None);
    assert!(String::from_utf8_lossy(&denied).starts_with("HTTP/1.1 403"));

    // 本机客户端以 127.0.0.1 访问（回环连接的 Host 须通过 DNS 重绑定校验）。
    let local_challenge =
        b"POST /v1/auth/challenge HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: 0\r\n\r\n";
    let issued = service.handle_http_from(local_challenge, true, "127.0.0.1", None);
    assert!(String::from_utf8_lossy(&issued).starts_with("HTTP/1.1 200 OK"));
    let payload = parse_json_response(&issued);
    let code = payload["result"]["pairing_code"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(payload["result"]["valid_seconds"], 300);

    let body = format!(r#"{{"pairing_code":"{code}"}}"#);
    let pair = format!(
        "POST /v1/auth/pair HTTP/1.1\r\nHost: remote\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let paired = service.handle_http_from(pair.as_bytes(), false, "10.0.0.8", None);
    assert!(String::from_utf8_lossy(&paired).starts_with("HTTP/1.1 200 OK"));
    assert!(
        parse_json_response(&paired)["result"]["token"]
            .as_str()
            .is_some()
    );
}

#[test]
fn http_request_reader_rejects_oversized_headers_and_bodies() {
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut results = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            results.push(
                read_http_request(&mut stream)
                    .map(|_| ())
                    .map_err(|e| e.kind()),
            );
        }
        results
    });
    // 1. 70KB 没有终止符的畸形请求头
    let mut oversized_header = TcpStream::connect(address).unwrap();
    let _ = oversized_header.write_all(&vec![b'A'; 70 * 1024]);
    let _ = oversized_header.shutdown(std::net::Shutdown::Write);
    // 2. 声明 21MB 请求体
    let mut oversized_body = TcpStream::connect(address).unwrap();
    let _ = oversized_body.write_all(
        b"POST /api/upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: 22020096\r\n\r\nxx",
    );
    let _ = oversized_body.shutdown(std::net::Shutdown::Write);
    let results = server.join().unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| matches!(r, Err(std::io::ErrorKind::InvalidData))),
        "{results:?}"
    );
    drop(oversized_header);
    drop(oversized_body);
}

#[test]
fn static_routes_and_sse_have_safe_wire_contracts() {
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    );
    // A release EXE must serve the embedded React page from /. Prior
    // builds returned a 404 JSON error because the embedded dist was stale.
    let home = service.handle_http(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let page = String::from_utf8_lossy(&home);
    assert!(page.starts_with("HTTP/1.1 200 OK"));
    assert!(page.contains("<div id=\"root\"></div>"));
    assert!(page.contains("/assets/"));
    for route in ["/draw", "/material-lab", "/voice", "/camera", "/twin"] {
        // 在任一页面按 F5 刷新都应回到前端页面，而不是 404
        let index = service
            .handle_http(format!("GET {route} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());
        assert!(
            String::from_utf8_lossy(&index).starts_with("HTTP/1.1 200 OK"),
            "{route}"
        );
        assert!(
            String::from_utf8_lossy(&index).contains("text/html"),
            "{route}"
        );
    }
    let traversal =
        service.handle_http(b"GET /assets/../index.html HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(String::from_utf8_lossy(&traversal).starts_with("HTTP/1.1 404 Not Found"));
    let events =
        service.handle_http(b"GET /v1/events?workflow_id=wf-1 HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let event_text = String::from_utf8_lossy(&events);
    assert!(event_text.starts_with("HTTP/1.1 200 OK"));
    assert!(event_text.contains("text/event-stream"));
    assert!(event_text.contains("\"workflow_id\":\"wf-1\""));
}

#[test]
fn http_adapter_answers_real_loopback_request() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    let service = std::sync::Arc::new(ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).unwrap();
        stream.write_all(&service.handle_http(&request)).unwrap();
    });
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    server.join().unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("\"status\":\"ok\""));
    // 不带 Origin（非浏览器客户端）：不输出任何 CORS 头，绝不出现通配符。
    assert!(!response.contains("Access-Control-Allow-"));
    assert!(!response.contains("Allow-Origin: *"));
}

#[test]
fn http_adapter_handles_cors_options_preflight() {
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    );
    let request = b"OPTIONS /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nOrigin: http://tauri.localhost\r\n\r\n";
    let response = String::from_utf8(service.handle_http(request)).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains("Access-Control-Allow-Origin: http://tauri.localhost\r\n"));
    assert!(response.contains("Vary: Origin"));
    assert!(response.contains("Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS"));
    assert!(response.contains(
        "Access-Control-Allow-Headers: Content-Type, Authorization, Accept, X-Moss-Desktop-Session"
    ));
    assert!(response.contains("Access-Control-Max-Age: 86400"));
    assert!(!response.contains("Allow-Origin: *"));

    // 不允许的来源：预检 403，且不带任何 CORS 头。
    let denied = b"OPTIONS /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nOrigin: http://evil.example\r\n\r\n";
    let denied = String::from_utf8(service.handle_http(denied)).unwrap();
    assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");
    assert!(denied.contains("origin_not_allowed"));
    assert!(!denied.contains("Access-Control-Allow-"));
}

#[test]
fn allowed_origin_rules_are_exact() {
    let none: Vec<String> = Vec::new();
    // 允许项
    assert!(is_allowed_origin(
        "http://tauri.localhost",
        Some("127.0.0.1:9666"),
        true,
        &none
    ));
    assert!(is_allowed_origin(
        "tauri://localhost",
        Some("127.0.0.1:9666"),
        false,
        &none
    ));
    assert!(is_allowed_origin(
        "http://127.0.0.1:9666",
        Some("127.0.0.1:9666"),
        true,
        &none
    ));
    assert!(is_allowed_origin(
        "http://192.0.2.10:9666",
        Some("192.0.2.10:9666"),
        false,
        &none
    ));
    for dev in [
        "http://127.0.0.1:5173",
        "http://localhost:5173",
        "http://127.0.0.1:5177",
        "http://localhost:5177",
        "http://127.0.0.1:5178",
        "http://localhost:5178",
    ] {
        assert!(
            is_allowed_origin(dev, Some("127.0.0.1:9666"), true, &none),
            "{dev}"
        );
        // 非回环请求下开发端口不放行
        assert!(
            !is_allowed_origin(dev, Some("192.0.2.10:9666"), false, &none),
            "{dev}"
        );
    }
    let extra = vec!["http://lab.example:8080".to_owned()];
    assert!(is_allowed_origin(
        "http://lab.example:8080",
        Some("127.0.0.1:9666"),
        false,
        &extra
    ));

    // 拒绝项
    for bad in [
        "http://tauri.localhost.evil.com",
        "http://evil.com",
        "null",
        "",
        " http://tauri.localhost",
        "HTTP://TAURI.LOCALHOST",
        "http://Tauri.localhost",
        "http://tauri.localhost:8080",
        "https://tauri.localhost",
        "http://127.0.0.1:9667",
        "http://lab.example:8081",
        "http://lab.example",
    ] {
        assert!(
            !is_allowed_origin(bad, Some("127.0.0.1:9666"), true, &extra),
            "{bad:?}"
        );
    }
    // 没有 Host 时不能靠同源放行
    assert!(!is_allowed_origin(
        "http://127.0.0.1:9666",
        None,
        true,
        &none
    ));
    assert!(!is_allowed_origin(
        "http://127.0.0.1:9666",
        Some(""),
        true,
        &none
    ));
}

#[test]
fn disallowed_origin_post_is_rejected_before_routing() {
    let temp = tempfile::tempdir().unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    let body = r#"{"web_port":9888}"#;
    for content_type in ["application/json", "text/plain"] {
        let request = format!(
            "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://evil.example\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"), "{response}");
        assert!(response.contains("\"error_code\":\"origin_not_allowed\""));
        assert!(!response.contains("Access-Control-Allow-"));
        // 业务未执行：settings.json 未被写入
        assert!(!temp.path().join("settings.json").exists());
    }

    let workflow = r#"{"action":"status","workflow_id":""}"#;
    let request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://evil.example\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{workflow}",
        workflow.len()
    );
    let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    assert!(service.workflows.lock().unwrap().is_empty());

    // 不带 Origin 的同一请求行为不变
    let request = format!(
        "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
    assert!(response.contains("\"success\":true"), "{response}");
    assert!(!response.contains("Access-Control-Allow-"));
    assert!(
        std::fs::read_to_string(temp.path().join("settings.json"))
            .unwrap()
            .contains("9888")
    );
}

#[test]
fn caller_context_is_built_by_entry_never_from_body() {
    use laser_contracts::CallerContext;
    assert_eq!(
        caller_context_for(None, true, None),
        CallerContext::Loopback
    );
    assert_eq!(
        caller_context_for(None, false, Some("pair_1".into())),
        CallerContext::Lan {
            pairing_id: "pair_1".into()
        }
    );
    assert_eq!(
        caller_context_for(Some(CallerContext::McpBridge), true, None),
        CallerContext::McpBridge
    );
    // A-36：请求体伪造的身份字段被删除，其余业务字段保留
    let mut object = serde_json::json!({
        "action": "status",
        "workflow_id": "wf-1",
        "caller_context": "Desktop",
        "caller": "desktop",
        "_caller": {"kind": "approval"},
        "desktop_session": "forged",
        "approval_id": "apr_forged",
        "gateway_session": "forged"
    })
    .as_object()
    .cloned()
    .unwrap();
    strip_identity_fields(&mut object);
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["action", "workflow_id"]);
}

#[test]
fn loopback_host_rules_block_dns_rebinding_names() {
    let none: Vec<String> = Vec::new();
    for ok in [
        "127.0.0.1",
        "127.0.0.1:9666",
        "localhost",
        "LOCALHOST:9666",
        "192.0.2.10:9666",
        "[::1]",
        "[::1]:9666",
    ] {
        assert!(is_allowed_loopback_host(ok, &none), "{ok}");
    }
    for bad in [
        "",
        "evil.example",
        "evil.example:9666",
        "127.0.0.1.evil.example:9666",
        "localhost.evil.example",
        "127.0.0.1:abc",
        "127.0.0.1:",
        "::1",
        "[::1",
        "[]:9666",
        "[evil]:9666",
        "[::1]x",
    ] {
        assert!(!is_allowed_loopback_host(bad, &none), "{bad:?}");
    }
    let extra = vec!["http://moss-lab.example:8080".to_owned()];
    assert!(is_allowed_loopback_host("moss-lab.example:9666", &extra));
    assert!(is_allowed_loopback_host("MOSS-LAB.example", &extra));
    assert!(!is_allowed_loopback_host("other.example", &extra));
}

#[test]
fn dns_rebinding_request_is_rejected_before_routing() {
    let temp = tempfile::tempdir().unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    let body = r#"{"web_port":9888}"#;
    // 重绑定后浏览器认为同源：Origin 与 Host 都是攻击者域名，且对端是回环地址。
    for origin_header in ["Origin: http://evil.example:9666\r\n", ""] {
        let request = format!(
            "POST /api/settings HTTP/1.1\r\nHost: evil.example:9666\r\n{origin_header}Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"), "{response}");
        assert!(response.contains("\"error_code\":\"host_not_allowed\""));
        assert!(!response.contains("Access-Control-Allow-"));
        assert!(!temp.path().join("settings.json").exists());
    }
    // 读接口同样拦截（重绑定页面同源，可读响应）
    let read = String::from_utf8(
        service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: evil.example:9666\r\n\r\n"),
    )
    .unwrap();
    assert!(read.starts_with("HTTP/1.1 403"), "{read}");
    assert!(!read.contains("actual_port"));

    // 真实 TCP 入口：回环请求缺少 Host 同样拒绝；带 Host 或非回环的请求交给正常入口
    let missing = missing_host_rejection(
        b"POST /api/settings HTTP/1.0
Content-Length: 0

",
        true,
    )
    .map(|bytes| String::from_utf8(bytes).unwrap())
    .expect("missing Host must be rejected on loopback");
    assert!(
        missing.starts_with("HTTP/1.1 403") && missing.contains("host_not_allowed"),
        "{missing}"
    );
    assert!(
        missing_host_rejection(
            b"GET / HTTP/1.1
Host: 127.0.0.1:9666

",
            true
        )
        .is_none()
    );
    assert!(
        missing_host_rejection(
            b"GET / HTTP/1.0

",
            false
        )
        .is_none()
    );

    // 非回环连接不做 Host 校验（仍由 Bearer 鉴权保护），局域网主机名访问公开页面不受影响。
    let lan = String::from_utf8(service.handle_http_from(
        b"GET /api/health HTTP/1.1\r\nHost: moss-pc.lan:9666\r\n\r\n",
        false,
        "192.0.2.20",
        None,
    ))
    .unwrap();
    assert!(lan.starts_with("HTTP/1.1 200 OK"), "{lan}");

    // 不带 Host 的本机非浏览器请求照常
    let no_host =
        String::from_utf8(service.handle_http(b"GET /api/health HTTP/1.1\r\n\r\n")).unwrap();
    assert!(no_host.starts_with("HTTP/1.1 200 OK"), "{no_host}");
}

#[test]
fn disallowed_origin_get_is_served_without_cors_headers() {
    let service = cors_test_service();
    let response = String::from_utf8(service.handle_http(
        b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://evil.example\r\n\r\n",
    ))
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(!response.contains("Access-Control-Allow-"));
}

#[test]
fn dev_origin_is_rejected_for_non_loopback_peer() {
    let service = cors_test_service();
    let request = b"POST /api/workflow HTTP/1.1\r\nHost: 192.0.2.10:9666\r\nOrigin: http://127.0.0.1:5177\r\nContent-Length: 2\r\n\r\n{}";
    let response =
        String::from_utf8(service.handle_http_from(request, false, "192.0.2.20", None)).unwrap();
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    assert!(!response.contains("Access-Control-Allow-"));
}

#[test]
fn allowed_origin_is_echoed_on_every_response_kind() {
    let temp = tempfile::tempdir().unwrap();
    let generated = temp.path().join("generated");
    std::fs::create_dir_all(&generated).unwrap();
    std::fs::write(generated.join("probe.gcode"), "G0 X0\n").unwrap();
    let service = cors_test_service()
        .with_generated_root(generated.clone())
        .with_preview_roots(vec![generated.clone()])
        .with_allowed_origins(vec!["http://lab.example:8080".into()]);
    let origin = "http://tauri.localhost";
    let expect_cors = |response: &[u8], origin: &str| {
        let text = String::from_utf8_lossy(response).into_owned();
        let head = text.split("\r\n\r\n").next().unwrap().to_owned();
        assert!(head.starts_with("HTTP/1.1 "), "{head}");
        assert!(
            head.contains(&format!("\r\nAccess-Control-Allow-Origin: {origin}\r\n")),
            "{head}"
        );
        assert!(head.contains("\r\nVary: Origin\r\n"), "{head}");
        assert_eq!(
            head.matches("Access-Control-Allow-Origin").count(),
            1,
            "{head}"
        );
        assert!(!head.contains("Allow-Origin: *"));
    };

    // JSON
    let json = service.handle_http(
        format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n")
            .as_bytes(),
    );
    expect_cors(&json, origin);
    // 额外来源（MOSS_ALLOWED_ORIGINS）
    let extra = service.handle_http(
            b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://lab.example:8080\r\n\r\n",
        );
    expect_cors(&extra, "http://lab.example:8080");
    // 静态页面（同源）
    let page = service.handle_http(
        b"GET /draw HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://127.0.0.1:9666\r\n\r\n",
    );
    expect_cors(&page, "http://127.0.0.1:9666");
    // SSE
    let sse = service.handle_http(
            format!("GET /v1/events?workflow_id=wf-1 HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n").as_bytes(),
        );
    assert!(String::from_utf8_lossy(&sse).contains("text/event-stream"));
    expect_cors(&sse, origin);
    // 文件下载 / 预览（二进制响应）
    let path = generated.join("probe.gcode");
    let preview = service.handle_http(
        format!(
            "GET /preview?path={} HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n",
            percent_encode(path.to_string_lossy().as_ref())
        )
        .as_bytes(),
    );
    let preview_text = String::from_utf8_lossy(&preview).into_owned();
    assert!(
        preview_text.starts_with("HTTP/1.1 200 OK"),
        "{preview_text}"
    );
    assert!(preview_text.ends_with("G0 X0\n"), "{preview_text}");
    expect_cors(&preview, origin);
    // OPTIONS
    let options = service.handle_http(
        format!(
            "OPTIONS /api/workflow HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n"
        )
        .as_bytes(),
    );
    expect_cors(&options, origin);
}

#[test]
fn http_preview_and_bootstrap_routes_enforce_contract() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("preview.gcode");
    std::fs::write(&file, b"G0 X0").unwrap();
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
    .with_preview_roots(vec![temp.path().to_path_buf()]);
    let encoded = percent_encode(file.to_string_lossy().as_ref());
    let response = service.handle_http(
        format!("GET /preview?path={encoded} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
    );
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200 OK"));
    let bootstrap = service.handle("GET", "/v1/material-lab/bootstrap", b"");
    assert_eq!(bootstrap.body["result"]["version"], 1);
    assert_eq!(bootstrap.body["result"]["prototype"], false);
}

#[test]
fn auth_pairing_expires_locks_and_stores_only_hashes() {
    let auth = AuthManager::new();
    let challenge = auth.issue_code(100).unwrap();
    let (token, pairing_id) = auth.pair("127.0.0.1", &challenge.code, 101).unwrap();
    assert!(pairing_id.starts_with("pair-"));
    assert!(auth.authorize(&token).is_ok());
    assert_eq!(
        auth.pair("127.0.0.1", &challenge.code, 101),
        Err(AuthError::Invalid)
    );

    let auth = AuthManager::new();
    let challenge = auth.issue_code(1000).unwrap();
    assert_eq!(
        auth.pair("10.0.0.2", "000000", 1001),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        auth.pair("10.0.0.2", "000000", 1001),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        auth.pair("10.0.0.2", "000000", 1001),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        auth.pair("10.0.0.2", "000000", 1001),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        auth.pair("10.0.0.2", "000000", 1001),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        auth.pair("10.0.0.2", &challenge.code, 1001),
        Err(AuthError::Locked(900))
    );
    let new_challenge = auth.issue_code(2000).unwrap();
    assert_eq!(
        auth.pair("10.0.0.2", &new_challenge.code, 2001)
            .unwrap()
            .1
            .len(),
        17
    );

    let auth = AuthManager::new();
    let challenge = auth.issue_code(100).unwrap();
    assert_eq!(
        auth.pair("127.0.0.1", &challenge.code, 400),
        Err(AuthError::Invalid)
    );
}

#[test]
fn auth_persistent_tokens_and_static_pin_survive_restart_and_support_revoke() {
    let temp = tempfile::tempdir().unwrap();
    let auth1 = AuthManager::with_runtime_path(temp.path().to_path_buf());
    assert!(!auth1.has_static_pin());
    assert!(auth1.set_static_pin(Some("12345")).is_err());
    auth1.set_static_pin(Some("886699")).unwrap();
    assert!(auth1.has_static_pin());

    // 使用自定义固定配对码可多次配对（不随 5 分钟过期）
    let (token_a, _) = auth1.pair("192.168.1.50", "886699", 50_000).unwrap();
    let (token_b, _) = auth1.pair("192.168.1.51", "886699", 99_999).unwrap();
    assert_eq!(auth1.paired_count(), 2);

    // 模拟软件重启或电脑重启：从同一 runtime_path 重新创建 AuthManager
    let auth2 = AuthManager::with_runtime_path(temp.path().to_path_buf());
    assert!(auth2.has_static_pin());
    assert_eq!(auth2.paired_count(), 2);
    assert!(auth2.authorize(&token_a).is_ok());
    assert!(auth2.authorize(&token_b).is_ok());

    // 撤销已配对设备后旧 token 立即失效，且重启后不再恢复
    assert_eq!(auth2.revoke_all_tokens().unwrap(), 2);
    assert!(auth2.authorize(&token_a).is_err());
    let auth3 = AuthManager::with_runtime_path(temp.path().to_path_buf());
    assert_eq!(auth3.paired_count(), 0);
}

#[test]
fn auth_persist_failure_leaves_memory_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let auth = AuthManager::with_runtime_path(temp.path().to_path_buf());
    auth.set_static_pin(Some("886699")).unwrap();
    let challenge = auth.issue_code(1_000).unwrap();
    let (kept_token, _) = auth.pair("192.168.1.50", "886699", 1_010).unwrap();

    // 让 settings.json 写入失败：持久化失败时内存状态不得提前变更，且一次性配对码不得被消费
    std::fs::write(temp.path().join("settings.json"), b"{corrupt").unwrap();
    assert_eq!(
        auth.pair("192.168.1.51", &challenge.code, 1_020),
        Err(AuthError::Internal)
    );
    assert!(auth.set_static_pin(Some("123456")).is_err());
    assert!(auth.revoke_all_tokens().is_err());
    assert_eq!(auth.paired_count(), 1);
    assert!(auth.authorize(&kept_token).is_ok());

    // 恢复正常配置文件后，原先未提交的那次动态配对码仍可正常使用
    std::fs::write(temp.path().join("settings.json"), b"{}").unwrap();
    let (new_token, _) = auth.pair("192.168.1.51", &challenge.code, 1_030).unwrap();
    assert!(auth.authorize(&new_token).is_ok());
}

#[test]
fn auth_concurrent_pair_and_revoke_all_stay_consistent_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let auth = std::sync::Arc::new(AuthManager::with_runtime_path(temp.path().to_path_buf()));
    auth.set_static_pin(Some("886699")).unwrap();

    let pair_auth = auth.clone();
    let t_pair = std::thread::spawn(move || {
        for i in 0..40u64 {
            let _ = pair_auth.pair("192.168.1.60", "886699", 10_000 + i);
        }
    });
    let revoke_auth = auth.clone();
    let t_revoke = std::thread::spawn(move || {
        for _ in 0..40 {
            revoke_auth.revoke_all_tokens().unwrap();
        }
    });
    t_pair.join().unwrap();
    t_revoke.join().unwrap();

    // 任意并发交叉后重启加载的数量必须与内存一致
    let reloaded = AuthManager::with_runtime_path(temp.path().to_path_buf());
    assert_eq!(reloaded.paired_count(), auth.paired_count());

    // 再执行一次全量撤销，重启后绝不能因旧线程快照回写而复活
    auth.revoke_all_tokens().unwrap();
    let after_revoke = AuthManager::with_runtime_path(temp.path().to_path_buf());
    assert_eq!(after_revoke.paired_count(), 0);
}

#[test]
fn non_loopback_requests_require_bearer_and_pairing_returns_token() {
    let auth = std::sync::Arc::new(AuthManager::new());
    let challenge = auth.issue_code(unix_seconds()).unwrap();
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "0.0.0.0".into(),
            web_port: 8766,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
    .with_auth(auth.clone());
    let denied = service.handle_http_from(
        b"GET /api/ui/material-options HTTP/1.1\r\nHost: remote\r\n\r\n",
        false,
        "10.0.0.8",
        None,
    );
    assert!(String::from_utf8_lossy(&denied).starts_with("HTTP/1.1 401 Unauthorized"));

    let body = format!(r#"{{"pairing_code":"{}"}}"#, challenge.code);
    let request = format!(
        "POST /v1/auth/pair HTTP/1.1\r\nHost: remote\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let paired = service.handle_http_from(request.as_bytes(), false, "10.0.0.8", None);
    let paired_text = String::from_utf8_lossy(&paired);
    assert!(paired_text.starts_with("HTTP/1.1 200 OK"));
    let payload_start = paired
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let payload = serde_json::from_slice::<serde_json::Value>(&paired[payload_start..]).unwrap();
    assert!(payload["result"]["token"].as_str().is_some());
    assert!(payload["result"]["pairing_id"].as_str().is_some());
}

#[test]
fn reference_data_writes_are_classified() {
    assert!(is_reference_data_write("POST", "/api/settings", b"{}"));
    assert!(is_reference_data_write("POST", "/api/jev/reveal", b""));
    assert!(is_reference_data_write(
        "POST",
        "/api/material-lab/select-cell",
        b"{}"
    ));
    assert!(is_reference_data_write(
        "POST",
        "/api/material-lab/manage",
        br#"{"action":"save"}"#
    ));
    assert!(is_reference_data_write(
        "POST",
        "/api/material-lab/manage",
        br#"{"action":"import"}"#
    ));
    assert!(is_reference_data_write(
        "POST",
        "/api/material-lab/manage",
        br#"{"action":"delete"}"#
    ));
    assert!(is_reference_data_write(
        "POST",
        "/api/material-lab/manage",
        b"not json"
    ));
    assert!(!is_reference_data_write(
        "POST",
        "/api/material-lab/manage",
        br#"{"action":"export"}"#
    ));
    for route in [
        "/api/text-task/generate",
        "/api/draw/preview",
        "/api/workflow",
        "/api/safe-action",
        "/api/device/framing",
        "/api/connection/check",
        "/api/material-lab/preview",
        "/api/material-lab/send",
        "/api/upload",
    ] {
        assert!(!is_reference_data_write("POST", route, b"{}"), "{route}");
    }
    assert!(!is_reference_data_write("GET", "/api/settings", b""));
}

#[test]
fn paired_lan_client_cannot_change_materials_or_settings_but_loopback_can() {
    let auth = std::sync::Arc::new(AuthManager::new());
    let challenge = auth.issue_code(unix_seconds()).unwrap();
    let (token, _) = auth
        .pair("10.0.0.8", &challenge.code, unix_seconds())
        .unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_auth(auth.clone());
    let post = |route: &str, body: &str, loopback: bool| {
        let request = format!(
            "POST {route} HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        String::from_utf8_lossy(&service.handle_http_from(
            request.as_bytes(),
            loopback,
            "10.0.0.8",
            None,
        ))
        .into_owned()
    };
    for (route, body) in [
        (
            "/api/material-lab/manage",
            r#"{"action":"save","material":"x"}"#,
        ),
        (
            "/api/material-lab/manage",
            r#"{"action":"delete","material":"x","confirmed":true}"#,
        ),
        ("/api/settings", r#"{"lan_access":true}"#),
        ("/api/jev/reveal", "{}"),
        (
            "/api/material-lab/select-cell",
            r#"{"calibration_id":"c","cell_number":1}"#,
        ),
    ] {
        let response = post(route, body, false);
        assert!(response.starts_with("HTTP/1.1 403"), "{route}: {response}");
        assert!(response.contains("lan_read_only"), "{route}: {response}");
    }
    // 导出与雕刻机操作不被只读规则拦截（是否成功由各自逻辑决定，这里只断言不是 lan_read_only）
    for (route, body) in [
        ("/api/material-lab/manage", r#"{"action":"export"}"#),
        ("/api/material-lab/preview", "{}"),
    ] {
        let response = post(route, body, false);
        assert!(!response.contains("lan_read_only"), "{route}: {response}");
    }
    // 电脑本机不受影响
    let response = post("/api/settings", r#"{"lan_access":"yes"}"#, true);
    assert!(!response.contains("lan_read_only"), "{response}");
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
}

#[test]
fn lan_info_is_loopback_only() {
    let auth = std::sync::Arc::new(AuthManager::new());
    let challenge = auth.issue_code(unix_seconds()).unwrap();
    let (token, _) = auth
        .pair("10.0.0.8", &challenge.code, unix_seconds())
        .unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_auth(auth.clone());
    let request = format!(
        "GET /api/system/lan-info HTTP/1.1\r\nHost: 10.0.0.2:9666\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let remote = String::from_utf8_lossy(&service.handle_http_from(
        request.as_bytes(),
        false,
        "10.0.0.8",
        None,
    ))
    .into_owned();
    assert!(remote.starts_with("HTTP/1.1 403"), "{remote}");
    let local = String::from_utf8_lossy(&service.handle_http_from(
        b"GET /api/system/lan-info HTTP/1.1\r\nHost: 127.0.0.1:9666\r\n\r\n",
        true,
        "127.0.0.1",
        None,
    ))
    .into_owned();
    assert!(local.starts_with("HTTP/1.1 200"), "{local}");
    assert!(local.contains("\"lan_access_active\":false"), "{local}");
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"/._-:".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

// ----- 严格模式（技术书 §6.2，实施清单 P1-04，验收 A-24..A-27、A-40） -----

#[test]
fn serve_reuses_loopback_keep_alive_connection_across_requests() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    let service = std::sync::Arc::new(ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 0,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = ApiService::serve(listener, service);
    });

    let mut client = TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();

    // 同一 TCP 连接连续管线化发送 2 个 Keep-Alive 请求 + 1 个显式 close 请求
    let pipelined = concat!(
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        "GET /v1/capabilities HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    );
    client.write_all(pipelined.as_bytes()).unwrap();

    let mut raw = Vec::new();
    client.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    assert_eq!(
        text.matches("HTTP/1.1 200 OK").count(),
        3,
        "同一回环连接应连续返回 3 个 200 响应: {text}"
    );
    assert_eq!(
        text.matches("Connection: keep-alive").count(),
        2,
        "前两个请求应保持 keep-alive"
    );
    assert_eq!(
        text.matches("Connection: close").count(),
        1,
        "带 Connection: close 的末个请求应正常关闭连接"
    );
}

#[test]
fn release_origin_check_rejects_dev_ports_and_challenge_requires_desktop_session() {
    let none: Vec<String> = Vec::new();
    // release 口径（allow_dev_loopback=false）绝不放行 5173/5177/5178 开发端口
    for dev in [
        "http://127.0.0.1:5173",
        "http://localhost:5173",
        "http://127.0.0.1:5177",
        "http://localhost:5177",
        "http://127.0.0.1:5178",
        "http://localhost:5178",
    ] {
        assert!(
            !crate::access::is_allowed_origin_with_dev(
                dev,
                Some("127.0.0.1:9666"),
                true,
                &none,
                false
            ),
            "release 下开发来源 {dev} 必须被拒绝"
        );
    }
    // 同源与 Tauri 仍放行
    assert!(crate::access::is_allowed_origin_with_dev(
        "http://127.0.0.1:9666",
        Some("127.0.0.1:9666"),
        true,
        &none,
        false
    ));
    assert!(crate::access::is_allowed_origin_with_dev(
        "http://tauri.localhost",
        Some("127.0.0.1:9666"),
        true,
        &none,
        false
    ));

    // 桌面会话开启后，不带 X-Moss-Desktop-Session 的回环请求不能签发局域网配对挑战码
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_auth(std::sync::Arc::new(AuthManager::new()));
    let session = service.enable_desktop_session().unwrap();
    let req_without_session =
        b"POST /v1/auth/challenge HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: 0\r\n\r\n";
    let blocked = String::from_utf8(service.handle_http(req_without_session)).unwrap();
    assert!(blocked.starts_with("HTTP/1.1 403"), "{blocked}");
    assert!(blocked.contains("desktop_session_required"), "{blocked}");

    let req_with_session = format!(
        "POST /v1/auth/challenge HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nX-Moss-Desktop-Session: {session}\r\nContent-Length: 0\r\n\r\n"
    );
    let allowed = String::from_utf8(service.handle_http(req_with_session.as_bytes())).unwrap();
    assert!(allowed.starts_with("HTTP/1.1 200"), "{allowed}");
    assert!(allowed.contains("pairing_code"), "{allowed}");
}
