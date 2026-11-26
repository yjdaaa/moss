use super::*;

#[test]
fn jev_key_is_masked_without_leaking_the_middle() {
    assert_eq!(mask_secret("sk-1234567890"), "sk-12••••••••7890");
    assert_eq!(mask_secret("short"), "••••••••");
    assert_eq!(
        mask_secret("测试密钥一二三四五六七八九"),
        "测试密钥一••••••••六七八九"
    );
    assert_eq!(mask_secret("中文短密钥"), "••••••••");
}

/// 设置写不进 settings.json 时返回错误，不显示“已保存”；能写时一次写入并保留其他键（2026-10-14）。
#[test]
fn settings_save_reports_write_failure_instead_of_success() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("settings.json"),
        r#"{"voice":{"mode":"official"}}"#,
    )
    .unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    let saved = post_json(
        &service,
        "/api/settings",
        &serde_json::json!({"web_port": 9888, "lan_access": true, "jev_enabled": false}),
    );
    assert_eq!(saved["success"], true, "{saved}");
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
    assert_eq!(file["web_port"], 9888);
    assert_eq!(file["lan_access"], true);
    assert_eq!(file["voice"]["mode"], "official", "其他键必须保留");
    assert!(!temp.path().join("settings.json.tmp").exists());

    // 运行目录指向一个文件：无法写入 settings.json
    let blocker = temp.path().join("not-a-dir");
    std::fs::write(&blocker, b"x").unwrap();
    let broken = cors_test_service().with_runtime_path(blocker);
    let failed = post_json(
        &broken,
        "/api/settings",
        &serde_json::json!({"lan_access": false}),
    );
    assert_eq!(failed["success"], false, "{failed}");
    assert_eq!(failed["detail"]["error_code"], "settings_write_failed");

    let no_runtime = post_json(
        &cors_test_service(),
        "/api/settings",
        &serde_json::json!({"web_port": 9888}),
    );
    assert_eq!(
        no_runtime["detail"]["error_code"], "settings_write_failed",
        "{no_runtime}"
    );
}

#[test]
fn settings_route_never_accepts_export_dir() {
    // 技术书 §9.7.3：导出目录只能由桌面壳经系统选择框修改，HTTP 接口不接受
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("settings.json"),
        br#"{"export":{"dir":"D:\\exports","ask_each_time":false}}"#,
    )
    .unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    let body = r#"{"export":{"dir":"C:\\Windows\\System32","ask_each_time":true},"export.dir":"C:\\evil"}"#;
    let request = format!(
        "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = service.handle_http(request.as_bytes());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["export"]["dir"], "D:\\exports");
    assert_eq!(saved["export"]["ask_each_time"], false);
    assert!(saved.get("export.dir").is_none());

    // 后端写其他设置时保留桌面壳写入的 export 键
    let port = r#"{"web_port":9777}"#;
    let request = format!(
        "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: {}\r\n\r\n{port}",
        port.len()
    );
    let _ = service.handle_http(request.as_bytes());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["web_port"], 9777);
    assert_eq!(saved["export"]["dir"], "D:\\exports");
}

#[test]
fn mcp_status_reports_only_real_telemetry() {
    // A-72：已配置但从未连接——不得出现任何编造值
    let empty = laser_contracts::McpTelemetrySnapshot::default();
    let status = mcp_status_json(
        Some("wss://example.test/mcp/?token=secret"),
        &empty,
        19,
        true,
    );
    assert_eq!(status["configured"], true);
    assert_eq!(status["connected"], false);
    assert_eq!(status["endpoint"], "wss://example.test/mcp/?token=***");
    assert_eq!(status["client_info"], "");
    assert_eq!(status["last_request"], "");
    assert_eq!(status["messages_count"], 0);
    assert_eq!(status["recent_events"], serde_json::json!([]));
    assert_eq!(status["tools_count"], 19);
    assert_eq!(status["protocol_version"], "", "未握手时不写死协议版本");
    assert!(!status.to_string().contains("xz-mcp-broker"));
    assert!(!status.to_string().contains("16:00:02"));

    // 未配置
    let unconfigured = mcp_status_json(None, &empty, 20, true);
    assert_eq!(unconfigured["configured"], false);
    assert_eq!(unconfigured["endpoint"], "");
    assert_eq!(unconfigured["tools_count"], 20);

    // 已连接：原样反映真实遥测
    let live = laser_contracts::McpTelemetrySnapshot {
        connected: true,
        client_name: "client-a".into(),
        client_version: "1.2".into(),
        last_request: "tools/list (查询工具列表)".into(),
        last_request_time: "10:00:00".into(),
        messages_count: 4,
        recent_events: vec!["10:00:00 [RECV] initialize (client: client-a v1.2)".into()],
        protocol_version: "2024-11-05".into(),
        endpoints: vec![laser_contracts::EndpointLink {
            id: "primary".into(),
            endpoint: "wss://example.test/mcp/".into(),
            connected: true,
            connections: 2,
            ..Default::default()
        }],
        ..Default::default()
    };
    let status = mcp_status_json(Some("wss://example.test/mcp/"), &live, 19, true);
    assert_eq!(status["connected"], true);
    assert_eq!(status["client_info"], "client-a v1.2");
    assert_eq!(
        status["protocol_version"], "2024-11-05",
        "握手后显示实际应答的版本"
    );
    assert_eq!(status["messages_count"], 4);
    assert_eq!(status["recent_events"].as_array().unwrap().len(), 1);
    // NV8：各接入点的状态
    assert_eq!(status["endpoints"][0]["id"], "primary");
    assert_eq!(status["endpoints"][0]["connections"], 2);
    assert_eq!(empty_endpoints(&mcp_status_json(None, &empty, 19, true)), 0);
}

#[test]
fn mcp_status_hides_endpoint_and_telemetry_when_details_hidden() {
    let endpoint = "wss://fakeuser:fakepass@example.invalid/mcp?k=fake1&token=fake2#frag";
    let telemetry = laser_contracts::McpTelemetrySnapshot {
        connected: true,
        client_name: "client-a".into(),
        client_version: "1.2".into(),
        last_request: "tools/list".into(),
        last_request_time: "10:00:00".into(),
        messages_count: 7,
        recent_events: vec!["10:00:00 [RECV] fake1".into()],
        protocol_version: "2024-11-05".into(),
        endpoints: vec![laser_contracts::EndpointLink {
            id: "primary".into(),
            endpoint: endpoint.into(),
            connected: true,
            connected_at: None,
            connections: 1,
            last_error: Some("dial fakeuser".into()),
            client: String::new(),
        }],
        ..Default::default()
    };

    let public_view = mcp_status_json(Some(endpoint), &telemetry, 19, false);
    assert_eq!(public_view["configured"], true);
    assert_eq!(public_view["connected"], true);
    assert_eq!(public_view["tools_count"], 19);
    assert_eq!(public_view["endpoint"], "");
    assert_eq!(public_view["client_info"], "");
    assert_eq!(public_view["last_request"], "");
    assert_eq!(public_view["last_request_time"], "");
    assert_eq!(public_view["messages_count"], 0);
    assert_eq!(public_view["protocol_version"], "");
    assert_eq!(public_view["recent_events"], serde_json::json!([]));
    assert_eq!(public_view["endpoints"], serde_json::json!([]));
    let raw_public = public_view.to_string();
    for secret in [
        "example.invalid",
        "fakeuser",
        "fakepass",
        "fake1",
        "fake2",
        "frag",
    ] {
        assert!(
            !raw_public.contains(secret),
            "{secret} leaked in {raw_public}"
        );
    }

    let authed_view = mcp_status_json(Some(endpoint), &telemetry, 19, true);
    assert_eq!(
        authed_view["endpoint"],
        "wss://***@example.invalid/mcp?k=***&token=***#***"
    );
    assert_eq!(
        authed_view["endpoints"][0]["endpoint"],
        "wss://***@example.invalid/mcp?k=***&token=***#***"
    );
    assert_eq!(authed_view["messages_count"], 7);
    let raw_authed = authed_view.to_string();
    for secret in ["fakeuser", "fakepass", "fake1", "fake2", "frag"] {
        assert!(
            !raw_authed.contains(secret),
            "{secret} leaked in {raw_authed}"
        );
    }
}

fn empty_endpoints(status: &serde_json::Value) -> usize {
    status["endpoints"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(usize::MAX)
}

/// 回环假 GRBL：应答一次 `$$` 握手，`$32` 由参数决定。
fn fake_grbl_handshake(laser_mode: u8) -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() && line.trim() == "$$" {
                let _ = write!(writer, "$0=10\r\n$32={laser_mode}\r\nok\r\n");
                line.clear();
                if reader.read_line(&mut line).is_ok() && line.trim() == "$CD" {
                    let _ = write!(writer, "error:3\r\n");
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
    port
}

fn check_connection(service: &ApiService, port: u16, ctx: Option<laser_contracts::CallerContext>) {
    let body = format!(
        r#"{{"connection_mode":"network","network_host":"127.0.0.1","network_telnet_port":{port},"network_timeout":2}}"#
    );
    let request = format!(
        "POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = match ctx {
        Some(ctx) => service.handle_http_with_ctx(request.as_bytes(), ctx),
        None => service.handle_http(request.as_bytes()),
    };
}

/// n73：工作台握手成功的连接被记下并写进设置（重启后还在）；握手失败、或语音 / 接入点调用的检查不记。
#[test]
fn workbench_handshake_success_is_remembered_for_ai_tools() {
    let temp = tempfile::tempdir().unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    assert!(service.verified_connection().is_none());

    // 激光模式没开（$32=0）：握手不算成功，不记
    check_connection(&service, fake_grbl_handshake(0), None);
    assert!(service.verified_connection().is_none());

    // 语音身份做的检查不记（只认工作台）
    check_connection(
        &service,
        fake_grbl_handshake(1),
        Some(laser_contracts::CallerContext::Voice {
            device_id: "d".into(),
            session_id: "s".into(),
        }),
    );
    assert!(service.verified_connection().is_none());

    let port = fake_grbl_handshake(1);
    check_connection(&service, port, None);
    let record = service.verified_connection().expect("握手成功要记下");
    assert_eq!(record["mode"], "network");
    assert_eq!(record["fields"]["network_host"], "127.0.0.1");
    assert_eq!(record["fields"]["host"], "127.0.0.1");
    assert_eq!(record["fields"]["network_telnet_port"], port);
    assert_eq!(record["fields"]["connection_mode"], "network");

    let reloaded = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    assert_eq!(
        reloaded.verified_connection().unwrap()["fields"],
        record["fields"],
        "写进 settings.json，重启后还记得"
    );
    // 设置文件里被改坏的记录不采用
    std::fs::write(
        temp.path().join("settings.json"),
        r#"{"laser_verified_connection":{"fields":{"connection_mode":"serial","port":""}}}"#,
    )
    .unwrap();
    assert!(
        cors_test_service()
            .with_runtime_path(temp.path().to_path_buf())
            .verified_connection()
            .is_none()
    );
}

#[test]
fn system_info_tools_count_matches_actual_catalog() {
    let service = cors_test_service();
    let value = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    // n72：默认精简清单，只数精简名单里的工具
    let catalog = laser_tools::ToolCatalog::from_environment();
    let expected = catalog
        .specs()
        .iter()
        .filter(|spec| laser_mcp_compact_tool(spec.name))
        .count();
    assert_eq!(value["result"]["mcp"]["tools_count"], expected);
    service.set_full_tool_list(true);
    let full = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(
        full["result"]["mcp"]["tools_count"],
        catalog.specs().len(),
        "完整清单数全部"
    );
    service.set_full_tool_list(false);
    assert_eq!(value["result"]["voice"]["mode"], "official");
    assert!(value["result"]["voice"]["device_sessions"].is_null());
    // NV8：关闭的工具不计入
    service.set_disabled_tools([
        "material_params_tool".to_owned(),
        "laser_safe_action_tool".to_owned(),
    ]);
    let value = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(
        value["result"]["mcp"]["tools_count"],
        expected - 1,
        "停止类工具不能关闭，只少一个"
    );
}

/// 技术书 §9.4.6 / A-68：语音中心只以桌面端 Tauri 命令提供，HTTP 后端（含 `moss-host serve`
/// 与 Android）没有 `/v1/voice/*`；内置链路下顶栏读取内置服务写入的设备会话数（§9.5.3）。
/// 2026-11-17 删除的自建链路：遗留的 `self_hosted` 按内置链路报告。
#[test]
fn voice_routes_are_not_served_over_http_and_system_info_reads_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    for raw in [
        &b"GET /v1/voice/status HTTP/1.1\r\nHost: localhost\r\n\r\n"[..],
        &b"GET /v1/voice/server/log?tail=200 HTTP/1.1\r\nHost: localhost\r\n\r\n"[..],
    ] {
        let response = service.handle_http(raw);
        assert!(
            String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"),
            "{}",
            String::from_utf8_lossy(&response)
        );
    }
    std::fs::write(
        temp.path().join("settings.json"),
        br#"{"voice":{"mode":"self_hosted"}}"#,
    )
    .unwrap();
    laser_contracts::global_voice_telemetry().set_device_sessions(Some(0));
    let value = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    laser_contracts::global_voice_telemetry().set_device_sessions(None);
    assert_eq!(value["result"]["voice"]["mode"], "builtin");
    assert_eq!(value["result"]["voice"]["device_sessions"], 0);
    // 内置链路（NV5）：会话数来自内置语音服务
    std::fs::write(
        temp.path().join("settings.json"),
        br#"{"voice":{"mode":"builtin"}}"#,
    )
    .unwrap();
    laser_contracts::global_voice_telemetry().set_device_sessions(Some(2));
    laser_contracts::global_voice_telemetry().set_pulse(Some("speaking"), 76);
    let builtin = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    laser_contracts::global_voice_telemetry().set_device_sessions(None);
    assert_eq!(builtin["result"]["voice"]["mode"], "builtin");
    assert_eq!(builtin["result"]["voice"]["device_sessions"], 2);
    assert_eq!(builtin["result"]["voice"]["phase"], "speaking");
    assert_eq!(builtin["result"]["voice"]["level"], 76);
    // 未知（内置服务尚未上报）时为 null，不猜
    let value = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert!(value["result"]["voice"]["device_sessions"].is_null());
}

#[test]
fn serial_ports_route_reports_enumerated_ports_and_first_as_default() {
    let service = cors_test_service().with_serial_port_lister(std::sync::Arc::new(|| {
        vec!["COM3".to_owned(), "COM7".to_owned()]
    }));
    let value = parse_json_response(
        &service.handle_http(b"GET /api/ui/serial-ports HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(value["success"], true);
    assert_eq!(
        value["result"]["ports"],
        serde_json::json!(["COM3", "COM7"])
    );
    assert_eq!(value["result"]["default_port"], "COM3");
    assert_eq!(value["result"]["baudrate"], 115200);
}

#[test]
fn system_info_and_settings_routes_work() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 9666,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf());

    service.set_actual_port(9666);
    let resp_info = String::from_utf8(
        service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    )
    .unwrap();
    assert!(resp_info.contains("\"actual_port\":9666"));

    let req_set = b"POST /api/settings HTTP/1.1\r\nHost: localhost\r\nContent-Length: 17\r\n\r\n{\"web_port\":9888}";
    let resp_set = String::from_utf8(service.handle_http(req_set)).unwrap();
    assert!(resp_set.contains("\"success\":true"));

    let saved = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
    assert!(saved.contains("9888"));

    let endpoint = "wss://example.test/mcp/?token=secret";
    let body = format!(r#"{{"mcp_endpoint":"{endpoint}"}}"#);
    let request = format!(
        "POST /api/settings HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let resp_endpoint = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
    assert!(resp_endpoint.contains("\"success\":true"));
    let saved = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
    assert!(saved.contains(endpoint));

    let invalid = br#"POST /api/settings HTTP/1.1
Host: localhost
Content-Length: 30

{"mcp_endpoint":"http://invalid"}"#;
    let resp_invalid = String::from_utf8(service.handle_http(invalid)).unwrap();
    assert!(resp_invalid.contains("\"success\":false"));
}

#[test]
fn non_loopback_system_info_hides_mcp_details_until_authenticated() {
    let temp = tempfile::tempdir().unwrap();
    let endpoint = "wss://fakeuser:fakepass@example.invalid/mcp?k=fake1&token=fake2";
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::json!({ "mcp_endpoint": endpoint }).to_string(),
    )
    .unwrap();
    let auth = std::sync::Arc::new(AuthManager::new());
    let challenge = auth.issue_code(unix_seconds()).unwrap();
    let (token, _) = auth
        .pair("10.0.0.8", &challenge.code, unix_seconds())
        .unwrap();
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "0.0.0.0".into(),
            web_port: 9666,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf())
    .with_auth(auth);

    // 1. 非回环未鉴权：允许 /api/system/info，但隐藏 MCP endpoint 和遥测细节
    let anon = parse_json_response(&service.handle_http_from(
        b"GET /api/system/info HTTP/1.1\r\nHost: 192.168.1.10:9666\r\n\r\n",
        false,
        "10.0.0.8",
        None,
    ));
    assert_eq!(anon["success"], true);
    assert_eq!(anon["result"]["mcp"]["configured"], true);
    assert_eq!(anon["result"]["mcp"]["endpoint"], "");
    assert_eq!(anon["result"]["mcp"]["endpoints"], serde_json::json!([]));
    assert_eq!(
        anon["result"]["mcp"]["recent_events"],
        serde_json::json!([])
    );
    assert_eq!(anon["result"]["mcp"]["messages_count"], 0);
    let raw_anon = anon.to_string();
    for secret in ["example.invalid", "fakeuser", "fakepass", "fake1", "fake2"] {
        assert!(!raw_anon.contains(secret), "{secret} leaked in {raw_anon}");
    }

    // 2. 非回环携带无效 Bearer token：仍隐藏 MCP 敏感细节
    let bad_auth = parse_json_response(&service.handle_http_from(
        b"GET /api/system/info HTTP/1.1\r\nHost: 192.168.1.10:9666\r\nAuthorization: Bearer invalid-token\r\n\r\n",
        false,
        "10.0.0.8",
        None,
    ));
    assert_eq!(bad_auth["success"], true);
    assert_eq!(bad_auth["result"]["mcp"]["endpoint"], "");

    // 3. 非回环携带合法 Bearer token：返回脱敏后的 endpoint（userinfo 和全部 query 参数已遮蔽）
    let req_authed = format!(
        "GET /api/system/info HTTP/1.1\r\nHost: 192.168.1.10:9666\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    let authed = parse_json_response(&service.handle_http_from(
        req_authed.as_bytes(),
        false,
        "10.0.0.8",
        None,
    ));
    assert_eq!(authed["success"], true);
    assert_eq!(
        authed["result"]["mcp"]["endpoint"],
        "wss://***@example.invalid/mcp?k=***&token=***"
    );
    let raw_authed = authed.to_string();
    for secret in ["fakeuser", "fakepass", "fake1", "fake2"] {
        assert!(
            !raw_authed.contains(secret),
            "{secret} leaked in {raw_authed}"
        );
    }
}

#[test]
fn version_comparison_and_loopback_update_check_work() {
    assert!(is_newer_version("0.3.0", "0.2.0"));
    assert!(is_newer_version("v1.0.0", "0.9.9"));
    assert!(!is_newer_version("0.2.0", "0.2.0"));
    assert!(!is_newer_version("0.1.9", "0.2.0"));
    assert!(is_newer_version("0.2.0", "0.2.0-beta.1"));
    assert!(is_newer_version("0.2.0-beta.10", "0.2.0-beta.2"));
    assert!(!is_newer_version("0.2.0-beta.1", "0.2.0"));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let body = r#"[{"tag_name":"v0.3.1","prerelease":false,"draft":false,"published_at":"2026-11-04T08:00:00Z","html_url":"https://github.com/yjdaaa/moss/releases/tag/v0.3.1","body":"新增持久配对与自动检查更新"}]"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = std::io::Write::write_all(&mut stream, resp.as_bytes());
        }
    });
    let report =
        check_github_release_update("0.2.0", false, &format!("http://{addr}/releases")).unwrap();
    let _ = handle.join();
    assert_eq!(report["has_update"], true);
    assert_eq!(report["latest_version"], "0.3.1");
    assert_eq!(report["current_version"], "0.2.0");

    let temp_dir = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 9666,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp_dir.path().to_path_buf());
    let info_default = service.handle_http_from(
        b"GET /api/system/info HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        true,
        "127.0.0.1",
        None,
    );
    let default_json: serde_json::Value = serde_json::from_slice(
        &info_default[info_default
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4..],
    )
    .unwrap();
    assert_eq!(
        default_json["result"]["auto_check_update"], true,
        "新安装默认开启启动自动检查更新"
    );

    let body = br#"{"auto_check_update":false}"#;
    let save_req = format!(
        "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let mut raw_post = save_req.into_bytes();
    raw_post.extend_from_slice(body);
    let save_resp = service.handle_http_from(&raw_post, true, "127.0.0.1", None);
    assert!(String::from_utf8_lossy(&save_resp).starts_with("HTTP/1.1 200"));
    let info_after = service.handle_http_from(
        b"GET /api/system/info HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        true,
        "127.0.0.1",
        None,
    );
    let after_json: serde_json::Value = serde_json::from_slice(
        &info_after[info_after
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .unwrap()
            + 4..],
    )
    .unwrap();
    assert_eq!(
        after_json["result"]["auto_check_update"], false,
        "用户关闭后持久化生效"
    );
}

#[test]
fn desktop_session_guards_settings_and_jev_reveal_against_unauthenticated_loopback() {
    let temp = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    db.lock()
        .unwrap()
        .set_secret(JEV_API_KEY_SECRET, "sk-test-desktop-only", 1.0)
        .unwrap();
    let service = cors_test_service()
        .with_runtime_path(temp.path().to_path_buf())
        .with_runtime_db(db);
    let session = service.enable_desktop_session().unwrap();

    // 未携带桌面会话的本机回环进程禁止读取密钥或篡改设置
    let (code, reveal_denied) = desktop_request(
        &service,
        "POST",
        "/api/jev/reveal",
        Some(&serde_json::json!({})),
        None,
    );
    assert_eq!(code, 403, "{reveal_denied}");
    assert_eq!(
        reveal_denied["detail"]["error_code"],
        "desktop_session_required"
    );
    assert!(!reveal_denied.to_string().contains("sk-test-desktop-only"));

    let (code, settings_denied) = desktop_request(
        &service,
        "POST",
        "/api/settings",
        Some(&serde_json::json!({"lan_access": true})),
        None,
    );
    assert_eq!(code, 403, "{settings_denied}");
    assert_eq!(
        settings_denied["detail"]["error_code"],
        "desktop_session_required"
    );
    assert!(!temp.path().join("settings.json").exists());

    // 携带有效桌面会话时正常放行
    let (code, reveal_ok) = desktop_request(
        &service,
        "POST",
        "/api/jev/reveal",
        Some(&serde_json::json!({})),
        Some(&session),
    );
    assert_eq!(code, 200, "{reveal_ok}");
    assert_eq!(reveal_ok["result"]["api_key"], "sk-test-desktop-only");

    let (code, settings_ok) = desktop_request(
        &service,
        "POST",
        "/api/settings",
        Some(&serde_json::json!({"lan_access": true})),
        Some(&session),
    );
    assert_eq!(code, 200, "{settings_ok}");
    assert!(temp.path().join("settings.json").exists());
}

/// 本机假 TCP 设备：`grbl` 时对 `?` 回状态报告、对 `$I` 回固件信息；否则一连上就回一行别的协议横幅。
fn spawn_fake_tcp_device(grbl: bool) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let Ok(mut stream) = stream else { continue };
            if !grbl {
                let _ = stream.write_all(b"SSH-2.0-OpenSSH_9.6\r\n");
                std::thread::sleep(std::time::Duration::from_millis(300));
                continue;
            }
            // 满负载下客户端两次写入之间可能被挂起较久：读超时放宽，避免假设备提前断开
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
            let mut seen = Vec::new();
            let mut buf = [0u8; 256];
            let (mut status_sent, mut version_sent) = (false, false);
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
                if !status_sent && seen.contains(&b'?') {
                    let _ = stream.write_all(b"<Idle|MPos:0.000,0.000,0.000|FS:0,0>\r\n");
                    status_sent = true;
                }
                if !version_sent && seen.windows(2).any(|w| w == b"$I") {
                    let _ = stream.write_all(b"[VER:4.1 FluidNC v4.1.1 (esp32-wifi) :yisu\r\n[OPT:PHS\r\n[MSG:Machine: Yisu V1.0]\r\nok\r\n");
                    version_sent = true;
                }
                // 回完也不主动断开：像真实 FluidNC 一样等客户端关闭。先关会让晚到的字节触发 RST，
                // 客户端缓冲里还没读的回复随之被丢弃
            }
        }
    });
    port
}

/// 2026-11-09：局域网扫描只返回按 GRBL / FluidNC 回应的设备（不再把广播 _http 的打印机、
/// 路由器当成雕刻机）；正在加工的目标不另开连接，单独列出；连不上或不是 GRBL 的只计数。
#[test]
fn lan_discovery_lists_only_devices_that_answer_like_grbl() {
    let grbl_port = spawn_fake_tcp_device(true);
    let ssh_port = spawn_fake_tcp_device(false);
    let closed_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let busy_port = 2399u16;
    let candidate = |port: u16, hostname: &str, source: &'static str| ControllerCandidate {
        ip: "127.0.0.1".into(),
        telnet_port: port,
        hostname: hostname.into(),
        http_port: Some(80),
        source,
    };
    let candidates = vec![
        candidate(grbl_port, "fluidnc", "mdns"),
        candidate(ssh_port, "printer", "mdns"),
        candidate(closed_port, "", "scan"),
        candidate(busy_port, "", "scan"),
    ];
    let service = cors_test_service()
        .with_controller_candidates(std::sync::Arc::new(
            move |emit: &(dyn Fn(ControllerCandidate) + Sync)| {
                for candidate in &candidates {
                    emit(candidate.clone());
                }
            },
        ))
        .with_discovery_reply_timeout(std::time::Duration::from_secs(5));
    service.active_jobs.lock().unwrap().insert(
        "wf-busy".into(),
        std::sync::Arc::new(ActiveJob {
            workflow_id: "wf-busy".into(),
            mode: "network",
            target: format!("127.0.0.1:{busy_port}"),
            cancel: Default::default(),
            progress: Default::default(),
            transport: std::sync::Arc::new(laser_device::FakeGrbl::new(Vec::<String>::new())),
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );
    let response = service.handle("POST", "/api/ui/discover-controllers", b"{}");
    assert_eq!(response.status, 200);
    let result = &response.body["result"];
    let controllers = result["controllers"].as_array().unwrap();
    assert_eq!(controllers.len(), 1, "{result}");
    assert_eq!(controllers[0]["telnet_port"], grbl_port);
    assert_eq!(controllers[0]["hostname"], "fluidnc");
    assert_eq!(controllers[0]["firmware"], "FluidNC v4.1.1");
    assert_eq!(controllers[0]["machine"], "Yisu V1.0");
    assert_eq!(controllers[0]["state"], "Idle");
    assert_eq!(
        result["busy"][0]["workflow_id"], "wf-busy",
        "正在加工的目标不另开连接"
    );
    assert_eq!(result["checked"], 4);
    assert_eq!(result["not_controllers"], 2, "SSH 横幅与连不上的都不算");
    assert_eq!(result["hint"], "");

    let none = cors_test_service().with_controller_candidates(std::sync::Arc::new(
        |_: &(dyn Fn(ControllerCandidate) + Sync)| {},
    ));
    let empty = none.handle("POST", "/api/ui/discover-controllers", b"{}");
    assert!(
        empty.body["result"]["controllers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let hint = empty.body["result"]["hint"].as_str().unwrap();
    assert!(
        hint.contains("没找到可能的控制器") || hint.contains("没有找到可用的局域网网卡"),
        "{hint}"
    );
}

/// 2026-11-09：激光侧事件——加工结束（完成 / 停止 / 失败带原因与行号）、握手失败与急停都会发出，
/// 桌面端接到语音中心“记录与诊断”并标为“激光雕刻”。
#[test]
fn laser_events_cover_job_outcomes_handshake_failure_and_estop() {
    let done = job_terminal_event(
        "wf-1",
        "completed",
        &serde_json::json!({"lines_sent": 400, "total_lines": 400, "target": "192.0.2.20:23"}),
    );
    assert_eq!(done.level, LaserEventLevel::Info);
    assert_eq!(done.text, "加工完成：wf-1，共 400 行（192.0.2.20:23）");
    let stopped = job_terminal_event(
        "wf-2",
        "cancelled",
        &serde_json::json!({"lines_sent": 120, "total_lines": 400, "can_resume": true}),
    );
    assert_eq!(stopped.level, LaserEventLevel::Warn);
    assert!(
        stopped.text.contains("停在第 120/400 行"),
        "{}",
        stopped.text
    );
    assert!(stopped.hint.unwrap().contains("断点继续"));
    let failed = job_terminal_event(
        "wf-3",
        "failed",
        &serde_json::json!({"lines_sent": 88, "total_lines": 400, "error": "ALARM:2 (软限位超程（目标运动坐标超出了设置的最大行程，已安全拦截，可执行 $X 解锁）)"}),
    );
    assert_eq!(failed.level, LaserEventLevel::Error);
    assert!(
        failed.text.contains("停在第 88/400 行") && failed.text.contains("ALARM:2"),
        "{}",
        failed.text
    );
    assert!(failed.hint.as_deref().unwrap().contains("超出了机器行程"));
    assert!(failed.detail.unwrap().contains("软限位"));
    assert!(
        laser_failure_hint("控制器连续 500ms 无响应，已判定失联")
            .unwrap()
            .contains("握手探测")
    );
    assert!(laser_failure_hint("一切正常").is_none());

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<LaserEvent>::new()));
    let service = cors_test_service().with_transport_factory(fake_transport_factory(10));
    let sink_events = events.clone();
    service.set_event_sink(std::sync::Arc::new(move |event| {
        sink_events.lock().unwrap().push(event)
    }));
    // 握手失败：连一个没人监听的本机端口
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    post_json(
        &service,
        "/api/connection/check",
        &serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": closed, "network_timeout": 0.5}),
    );
    // 急停
    post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1", "telnet_port": 2302}),
    );
    // 状态查询不记
    post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "status", "connection_mode": "network", "host": "127.0.0.1", "telnet_port": 2302}),
    );
    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.level == LaserEventLevel::Warn && e.text.starts_with("连接雕刻机失败")),
        "{events:?}"
    );
    assert!(events.iter().any(|e| e.text.contains("急停")), "{events:?}");
    assert!(
        !events.iter().any(|e| e.text.contains("status")),
        "状态查询不写事件: {events:?}"
    );
}

#[test]
fn jev_test_requires_operator_session_and_status_redacts_masked_key() {
    let temp = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    db.lock()
        .unwrap()
        .set_secret(JEV_API_KEY_SECRET, "sk-or-v1-secret-9876", 1.0)
        .unwrap();
    let service = cors_test_service()
        .with_runtime_path(temp.path().to_path_buf())
        .with_runtime_db(db);
    let session = service.enable_desktop_session().unwrap();

    let (code, test_denied) = desktop_request(
        &service,
        "POST",
        "/api/jev/test",
        Some(&serde_json::json!({})),
        None,
    );
    assert_eq!(code, 403, "{test_denied}");
    assert_eq!(
        test_denied["detail"]["error_code"],
        "desktop_session_required"
    );

    let bridge_denied = parse_json_response(&service.handle_http_with_ctx(
        b"POST /api/jev/test HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: 2\r\n\r\n{}",
        laser_contracts::CallerContext::McpBridge,
    ));
    assert_eq!(
        bridge_denied["detail"]["error_code"],
        "desktop_session_required"
    );

    let (code, unauth_status) = desktop_request(&service, "GET", "/api/jev/status", None, None);
    assert_eq!(code, 200, "{unauth_status}");
    assert_eq!(unauth_status["result"]["masked_key"], "");

    let (code, auth_status) =
        desktop_request(&service, "GET", "/api/jev/status", None, Some(&session));
    assert_eq!(code, 200, "{auth_status}");
    assert!(
        !auth_status["result"]["masked_key"]
            .as_str()
            .unwrap_or("")
            .is_empty()
    );

    // 无桌面会话（独立 moss-host 模式）时，局域网配对会话读取 /api/jev/status 同样隐藏 masked_key，
    // 且 MCP 桥 / 语音调用方不得触发 /api/system/check-update 外联检查。
    let host_db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    host_db
        .lock()
        .unwrap()
        .set_secret(JEV_API_KEY_SECRET, "sk-or-v1-secret-9876", 1.0)
        .unwrap();
    let host_service = cors_test_service()
        .with_runtime_path(temp.path().to_path_buf())
        .with_runtime_db(host_db);
    let lan_status = parse_json_response(&host_service.handle_http_with_ctx(
        b"GET /api/jev/status HTTP/1.1\r\nHost: 127.0.0.1:9666\r\n\r\n",
        laser_contracts::CallerContext::Lan {
            pairing_id: "phone-1".into(),
        },
    ));
    assert_eq!(lan_status["result"]["masked_key"], "", "{lan_status}");

    let bridge_update = parse_json_response(&host_service.handle_http_with_ctx(
            b"POST /api/system/check-update HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: 2\r\n\r\n{}",
            laser_contracts::CallerContext::McpBridge,
        ));
    assert_eq!(
        bridge_update["detail"]["error_code"], "desktop_session_required",
        "{bridge_update}"
    );
}

#[test]
fn draw_state_persists_across_port_changes_and_enforces_caller_and_validation_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let runtime_path = temp.path().to_path_buf();
    let service = cors_test_service().with_runtime_path(runtime_path.clone());

    let (code, initial) = desktop_request(&service, "GET", "/api/draw/state", None, None);
    assert_eq!(code, 200, "{initial}");
    assert_eq!(initial["result"]["exists"], false);
    assert!(initial["result"]["scene"].is_null());

    let fp = "a".repeat(64);
    let (code, saved) = desktop_request(
        &service,
        "POST",
        "/api/draw/state",
        Some(&serde_json::json!({
            "scene": "{\"version\":1,\"elements\":[{\"id\":\"r1\"}]}",
            "last_preview": "{\"workflowId\":\"wf-1\",\"sceneRev\":\"r1:1:1\",\"formSig\":\"{}\"}",
            "form": { "material": "椴木", "width_mm": "80" },
            "last_accepted_fingerprint": fp.to_ascii_uppercase(),
        })),
        None,
    );
    assert_eq!(code, 200, "{saved}");
    assert_eq!(saved["result"]["exists"], true);
    assert_eq!(saved["result"]["last_accepted_fingerprint"], fp);

    // 部分字段补丁不得清掉已保存的画布和预览指针
    let (code, patched) = desktop_request(
        &service,
        "POST",
        "/api/draw/state",
        Some(&serde_json::json!({
            "form": { "width_mm": "120" }
        })),
        None,
    );
    assert_eq!(code, 200, "{patched}");
    assert_eq!(
        patched["result"]["scene"],
        "{\"version\":1,\"elements\":[{\"id\":\"r1\"}]}"
    );
    assert_eq!(patched["result"]["form"]["material"], "椴木");
    assert_eq!(patched["result"]["form"]["width_mm"], "120");

    // 模拟桌面端端口回退重启后的新 ApiService 实例（仅共享同一 RuntimeRoot）
    let restarted = cors_test_service().with_runtime_path(runtime_path);
    let (code, reloaded) = desktop_request(&restarted, "GET", "/api/draw/state", None, None);
    assert_eq!(code, 200, "{reloaded}");
    assert_eq!(reloaded["result"]["exists"], true);
    assert_eq!(
        reloaded["result"]["last_preview"],
        "{\"workflowId\":\"wf-1\",\"sceneRev\":\"r1:1:1\",\"formSig\":\"{}\"}"
    );
    assert_eq!(reloaded["result"]["form"]["material"], "椴木");
    assert_eq!(reloaded["result"]["last_accepted_fingerprint"], fp);

    // 失败路径：非法参数返回 400；局域网（LAN）读写与语音/MCP 桥全部 403
    let (code, bad_form) = desktop_request(
        &restarted,
        "POST",
        "/api/draw/state",
        Some(&serde_json::json!({ "form": { "bad-key!": "x" } })),
        None,
    );
    assert_eq!(code, 400, "{bad_form}");
    let (code, bad_fp) = desktop_request(
        &restarted,
        "POST",
        "/api/draw/state",
        Some(&serde_json::json!({ "last_accepted_fingerprint": "not-hex" })),
        None,
    );
    assert_eq!(code, 400, "{bad_fp}");

    let parse_with_status = |raw: Vec<u8>| -> (u16, serde_json::Value) {
        let status = String::from_utf8_lossy(&raw[9..12]).parse::<u16>().unwrap();
        (status, parse_json_response(&raw))
    };

    let (lan_read_status, lan_read) = parse_with_status(restarted.handle_http_with_ctx(
        b"GET /api/draw/state HTTP/1.1\r\nHost: 127.0.0.1:9666\r\n\r\n",
        laser_contracts::CallerContext::Lan {
            pairing_id: "phone-1".into(),
        },
    ));
    assert_eq!(lan_read_status, 403, "{lan_read}");
    assert_eq!(lan_read["success"], false, "{lan_read}");
    assert!(
        lan_read["result"].as_str().unwrap_or("").contains("本机"),
        "{lan_read}"
    );

    let (lan_write_status, lan_write) = parse_with_status(restarted.handle_http_with_ctx(
        b"POST /api/draw/state HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: 2\r\n\r\n{}",
        laser_contracts::CallerContext::Lan {
            pairing_id: "phone-1".into(),
        },
    ));
    assert_eq!(lan_write_status, 403, "{lan_write}");
    assert_eq!(lan_write["success"], false, "{lan_write}");
    assert!(
        lan_write["result"]
            .as_str()
            .unwrap_or("")
            .contains("局域网")
            || lan_write["result"].as_str().unwrap_or("").contains("只读"),
        "{lan_write}"
    );

    let (bridge_read_status, bridge_read) = parse_with_status(restarted.handle_http_with_ctx(
        b"GET /api/draw/state HTTP/1.1\r\nHost: 127.0.0.1:9666\r\n\r\n",
        laser_contracts::CallerContext::McpBridge,
    ));
    assert_eq!(bridge_read_status, 403, "{bridge_read}");
    assert_eq!(bridge_read["success"], false, "{bridge_read}");
}

#[test]
fn draw_state_recovers_corrupt_files_and_discards_connection_fields() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("draw_state.json");
    let service = cors_test_service().with_runtime_path(root.path().to_path_buf());
    // GET 和 POST 都应自行处理损坏文件，逐个保留原始字节。
    std::fs::write(&file, "{broken").unwrap();
    let (code, empty) = desktop_request(&service, "GET", "/api/draw/state", None, None);
    assert_eq!(code, 200, "{empty}");
    assert_eq!(empty["result"]["exists"], false);
    std::fs::write(&file, r#"{"scene":42}"#).unwrap();
    let (code, saved) = desktop_request(
        &service,
        "POST",
        "/api/draw/state",
        Some(&serde_json::json!({ "form": {
            "material": "椴木", "network_host": "example.invalid", "port": "COM9",
            "host": "example.invalid", "network_telnet_port": "23",
            "connection_mode": "network", "token": {"test_value": true}
        }})),
        None,
    );
    assert_eq!(code, 200, "{saved}");
    assert_eq!(
        saved["result"]["form"],
        serde_json::json!({"material":"椴木"})
    );
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(stored["form"], serde_json::json!({"material":"椴木"}));
    let mut backups = std::fs::read_dir(root.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("draw_state.json.corrupt-")
        })
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect::<Vec<_>>();
    backups.sort_unstable();
    assert_eq!(backups, [r#"{"scene":42}"#, "{broken"]);
}

/// n105 完整过程记录：默认关；只有桌面（或无桌面会话时的本机回环）能开关、查看与清空，语音 / 局域网一律拒绝。
#[test]
fn process_trace_switch_status_and_clear_are_desktop_only() {
    let temp = tempfile::tempdir().unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    let info = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(info["result"]["process_trace_enabled"], false, "默认关闭");
    assert!(
        info["result"].get("process_trace_dir").is_none(),
        "公开的系统信息不带本机目录"
    );

    let status = post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "status"}),
    );
    assert_eq!(status["success"], true, "{status}");
    assert_eq!(status["result"]["enabled"], false);
    assert_eq!(status["result"]["custom_dir"], serde_json::Value::Null);
    assert_eq!(status["result"]["available"], true);
    assert_eq!(status["result"]["error"], serde_json::Value::Null);
    assert_eq!(status["result"]["bytes"], 0);
    assert_eq!(status["result"]["retention_days"], 30);
    assert!(
        status["result"].get("max_bytes").is_none(),
        "n110 起不按大小限制"
    );
    assert!(
        status["result"]["dir"]
            .as_str()
            .unwrap()
            .ends_with("process-trace")
    );

    let bad = post_json(
        &service,
        "/api/settings",
        &serde_json::json!({"process_trace_enabled": "yes"}),
    );
    assert_eq!(bad["success"], false, "{bad}");

    let saved = post_json(
        &service,
        "/api/settings",
        &serde_json::json!({"process_trace_enabled": true}),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(
        parse_json_response(
            &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n")
        )["result"]["process_trace_enabled"],
        true
    );
    // 保存后立即生效（不用等 2 秒缓存）
    let trace = service.process_trace().unwrap();
    assert!(trace.enabled());
    trace.record(
        "laser",
        "激光",
        "generate",
        "生成预览",
        serde_json::json!({"text": "你好"}),
    );
    let status = post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "status"}),
    );
    assert!(status["result"]["bytes"].as_u64().unwrap() > 0, "{status}");
    assert_eq!(status["result"]["days"], 1);

    // 语音身份与局域网不能开关、查看或清空
    for ctx in [
        laser_contracts::CallerContext::Voice {
            device_id: "d".into(),
            session_id: "s".into(),
        },
        laser_contracts::CallerContext::Lan {
            pairing_id: "p".into(),
        },
    ] {
        for (path, body) in [
            ("/api/process-trace", r#"{"action":"clear"}"#),
            ("/api/settings", r#"{"process_trace_enabled":false}"#),
        ] {
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let denied =
                parse_json_response(&service.handle_http_with_ctx(request.as_bytes(), ctx.clone()));
            assert_eq!(denied["success"], false, "{path} {denied}");
        }
    }
    assert!(trace.enabled(), "被拒绝的请求不改开关");
    assert!(trace.usage().bytes > 0, "被拒绝的请求不清空");
    assert!(
        is_reference_data_write("POST", "/api/process-trace", b"{}"),
        "手机连接不允许操作"
    );

    let cleared = post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "clear"}),
    );
    assert_eq!(cleared["result"]["bytes"], 0, "{cleared}");
    let unknown = post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "delete_all"}),
    );
    assert_eq!(unknown["success"], false);
}

#[test]
fn process_trace_directory_is_reported_but_http_cannot_change_it() {
    let temp = tempfile::tempdir().unwrap();
    let selected = tempfile::tempdir().unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    let trace = service.process_trace().unwrap();
    trace.set_directory(Some(selected.path())).unwrap();
    let chosen = trace.dir();
    assert!(chosen.ends_with("moss-process-trace"));
    assert_eq!(
        chosen.parent().unwrap().canonicalize().unwrap(),
        selected.path().canonicalize().unwrap()
    );
    let status = post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "status"}),
    );
    assert_eq!(status["result"]["dir"], serde_json::json!(chosen));
    assert_eq!(status["result"]["custom_dir"], serde_json::json!(chosen));
    assert_eq!(status["result"]["available"], true);
    for value in [serde_json::json!(temp.path()), serde_json::Value::Null] {
        let rejected = post_json(
            &service,
            "/api/settings",
            &serde_json::json!({"process_trace_dir": value, "process_trace_enabled": true}),
        );
        assert_eq!(rejected["success"], false, "{rejected}");
        assert_eq!(
            rejected["result"],
            "过程记录保存位置只能通过桌面文件夹选择框修改"
        );
        assert_eq!(trace.dir(), chosen);
        assert!(!trace.enabled(), "拒绝整个请求，不能部分保存其他设置");
    }
    let body = serde_json::json!({"process_trace_dir": temp.path()}).to_string();
    let request = format!(
        "PATCH /api/settings HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let rejected = parse_json_response(&service.handle_http(request.as_bytes()));
    assert_eq!(rejected["success"], false, "PATCH 也没有设置路径入口");
    assert_eq!(trace.dir(), chosen);

    let info = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert!(info["result"].get("process_trace_dir").is_none());
    assert!(info["result"].get("custom_dir").is_none());
    assert!(
        !info
            .to_string()
            .contains(&chosen.to_string_lossy().to_string())
    );

    let unplugged = chosen.with_file_name("unplugged");
    std::fs::rename(&chosen, &unplugged).unwrap();
    let unavailable = post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "status"}),
    );
    assert_eq!(unavailable["result"]["available"], false);
    assert!(unavailable["result"]["error"].is_string());
    assert_eq!(unavailable["result"]["dir"], serde_json::json!(chosen));
    assert!(!chosen.exists(), "状态查询不能重建缺失的自选目录");
    std::fs::rename(&unplugged, &chosen).unwrap();
    let restored = post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "status"}),
    );
    assert_eq!(restored["result"]["available"], true);
}

/// n105：完整过程记录打开后，改动类接口调用按先后记进时间线（发起方、做什么、结果），密钥隐藏，只读轮询不记。
#[test]
fn process_trace_timeline_records_api_calls_in_order_and_hides_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
    let enabled = post_json(
        &service,
        "/api/settings",
        &serde_json::json!({"process_trace_enabled": true}),
    );
    assert_eq!(enabled["success"], true, "{enabled}");
    post_json(
        &service,
        "/api/settings",
        &serde_json::json!({"auto_check_update": false}),
    );
    post_json(
        &service,
        "/api/settings",
        &serde_json::json!({"jev_api_key": "fake-jev-value-for-trace-test"}),
    );
    post_json(
        &service,
        "/api/settings",
        &serde_json::json!({"mcp_endpoint": "wss://mcp.example.com/ws?TOKEN=secret-ep-token-123"}),
    );
    post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({"action": "recent"}),
    );
    // 缺省 action 的 /api/workflow 等价只读 status，以及无 action 的材料实验室只读 POST，均不记入时间线
    post_json(&service, "/api/workflow", &serde_json::json!({}));
    post_json(
        &service,
        "/api/material-lab/status",
        &serde_json::json!({"calibration_id": "cal-1"}),
    );
    post_json(&service, "/api/material-lab/recent", &serde_json::json!({}));
    post_json(
        &service,
        "/api/process-trace",
        &serde_json::json!({"action": "status"}),
    );
    service.process_trace().unwrap().flush();
    let day = std::fs::read_dir(temp.path().join(laser_runtime::process_trace::TRACE_DIR))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let timeline =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL)).unwrap();
    assert!(
        !timeline.contains("fake-jev-value-for-trace-test"),
        "密钥不进记录：{timeline}"
    );
    assert!(
        !timeline.contains("secret-ep-token-123"),
        "端点 URL 中的大小写 token 也不进记录：{timeline}"
    );
    let lines: Vec<serde_json::Value> = timeline
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let steps: Vec<&str> = lines.iter().filter_map(|l| l["step"].as_str()).collect();
    // n110：每次改动类调用记开始与结束两步，共 4 次调用 8 条（只读轮询全部过滤）；
    // 打开开关那次调用开始时开关还关着，处理完补记开始这一步
    assert_eq!(lines.len(), 8, "{steps:?}");
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["api_begin", "api_call"].repeat(4), "{steps:?}");
    for pair in lines.chunks(2) {
        assert_eq!(pair[0]["call"], pair[1]["call"], "开始与结束编号对得上");
    }
    assert!(
        steps[0].contains("修改设置：process_trace_enabled（开始处理"),
        "{steps:?}"
    );
    assert!(
        steps[1].contains("修改设置：process_trace_enabled → 成功"),
        "{steps:?}"
    );
    assert!(
        steps[3].contains("修改设置：auto_check_update → 成功"),
        "{steps:?}"
    );
    assert!(steps[4].contains("修改设置：jev_api_key"), "{steps:?}");
    assert_eq!(lines[4]["request"]["jev_api_key"], "[已隐藏]");
    assert_eq!(
        lines[6]["request"]["mcp_endpoint"], "wss://mcp.example.com/ws?TOKEN=[已隐藏]",
        "只隐藏令牌本身，地址其余部分保留"
    );
    assert_eq!(lines[0]["flow"], "激光");
    let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "按先后编号：{seqs:?}");
    assert!(!timeline.contains("\"recent\""), "只读轮询不进时间线");
    assert!(
        !timeline.contains("/api/material-lab/status")
            && !timeline.contains("/api/material-lab/recent"),
        "材料实验室只读 POST 不进时间线：{timeline}"
    );
    let readable =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_TXT)).unwrap();
    assert_eq!(readable.lines().count(), 8);
    assert!(
        !readable.contains("secret-ep-token-123")
            && !readable.contains("fake-jev-value-for-trace-test"),
        "可读时间线不得泄露凭据：{readable}"
    );
    assert!(
        readable.contains("[激光] 本机发起的修改设置：auto_check_update → 成功"),
        "{readable}"
    );
}

#[test]
fn system_info_and_stop_targets_expose_caller_capability_flags() {
    let service = cors_test_service();
    let token = service.enable_desktop_session().unwrap();

    // 1. 携带桌面会话的本机请求：拥有操作员设置、材料/路线写入与确认停稳三项权限
    let desktop_info_req = format!(
        "GET /api/system/info HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nX-Moss-Desktop-Session: {token}\r\n\r\n"
    );
    let desktop_info = parse_json_response(&service.handle_http(desktop_info_req.as_bytes()));
    assert_eq!(desktop_info["result"]["can_edit_operator_settings"], true);
    assert_eq!(desktop_info["result"]["can_write_reference_data"], true);
    assert_eq!(desktop_info["result"]["can_acknowledge_stop"], true);

    let desktop_targets_req = format!(
        "GET /api/jobs/stop-targets HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nX-Moss-Desktop-Session: {token}\r\n\r\n"
    );
    let desktop_targets = parse_json_response(&service.handle_http(desktop_targets_req.as_bytes()));
    assert_eq!(desktop_targets["result"]["can_acknowledge_stop"], true);

    // 2. 桌面版运行时同机外部浏览器（无桌面会话）：不能改操作员设置、不能确认停稳，回环仍可读写材料与路线
    let loopback_browser_info = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: 127.0.0.1:9666\r\n\r\n"),
    );
    assert_eq!(
        loopback_browser_info["result"]["can_edit_operator_settings"],
        false
    );
    assert_eq!(
        loopback_browser_info["result"]["can_write_reference_data"],
        true
    );
    assert_eq!(
        loopback_browser_info["result"]["can_acknowledge_stop"],
        false
    );

    let loopback_browser_targets = parse_json_response(
        &service
            .handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: 127.0.0.1:9666\r\n\r\n"),
    );
    assert_eq!(
        loopback_browser_targets["result"]["can_acknowledge_stop"],
        false
    );

    // 3. 局域网非回环访问：不能改设置、不能写材料/路线
    let lan_info = parse_json_response(&service.handle_http_from(
        b"GET /api/system/info HTTP/1.1\r\nHost: 192.0.2.10:9666\r\n\r\n",
        false,
        "192.0.2.50",
        None,
    ));
    assert_eq!(lan_info["result"]["can_edit_operator_settings"], false);
    assert_eq!(lan_info["result"]["can_write_reference_data"], false);
    assert_eq!(lan_info["result"]["can_acknowledge_stop"], false);
}
