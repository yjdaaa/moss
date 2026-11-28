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
fn connection_check_exposes_fluidnc_has_estop_or_fault_pin_and_null_features_for_plain_grbl() {
    let grbl_port = fake_grbl_handshake(1);
    let service = cors_test_service();
    let grbl_body = format!(
        r#"{{"connection_mode":"network","network_host":"127.0.0.1","network_telnet_port":{grbl_port},"network_timeout":2}}"#
    );
    let grbl_resp = parse_json_response(
        &service.handle_http(
            format!(
                "POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                grbl_body.len(),
                grbl_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(grbl_resp["result"]["connected"], true, "{grbl_resp}");
    assert!(
        grbl_resp["result"]["features"].is_null(),
        "普通 GRBL 不返回 $CD YAML，features 应保持 null: {grbl_resp}"
    );

    let fluid_port = {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() && line.trim() == "$$" {
                    let _ = write!(writer, "$0=10\r\n$32=1\r\nok\r\n");
                    line.clear();
                    if reader.read_line(&mut line).is_ok() && line.trim() == "$CD" {
                        let _ = write!(
                            writer,
                            "axes:\r\n  x:\r\n    max_travel_mm: 220\r\n  y:\r\n    max_travel_mm: 180\r\ncontrol:\r\n  estop_pin: gpio.4:low\r\n  safety_door_pin: NO_PIN\r\nLaser:\r\n  speed_map: 0=0% 1000=100%\r\n  off_on_alarm: true\r\nok\r\n"
                        );
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        port
    };
    let fluid_body = format!(
        r#"{{"connection_mode":"network","network_host":"127.0.0.1","network_telnet_port":{fluid_port},"network_timeout":2}}"#
    );
    let fluid_resp = parse_json_response(
        &service.handle_http(
            format!(
                "POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                fluid_body.len(),
                fluid_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(fluid_resp["result"]["connected"], true, "{fluid_resp}");
    assert_eq!(
        fluid_resp["result"]["features"]["axes"],
        serde_json::json!(["x", "y"]),
        "{fluid_resp}"
    );
    assert_eq!(
        fluid_resp["result"]["features"]["has_estop_or_fault_pin"], true,
        "{fluid_resp}"
    );
    assert_eq!(
        fluid_resp["result"]["features"]["has_door"], false,
        "{fluid_resp}"
    );
    assert_eq!(
        fluid_resp["result"]["features"]["speed_map"], "0=0% 1000=100%",
        "{fluid_resp}"
    );
}

#[test]
fn system_info_tools_count_matches_actual_catalog() {
    let service = cors_test_service();
    let value = parse_json_response(
        &service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    let catalog = laser_tools::ToolCatalog::from_environment();
    let expected = catalog.specs().len();
    assert_eq!(value["result"]["mcp"]["tools_count"], expected);
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
            "form": {
                "material": "椴木",
                "width_mm": "80",
                "halftone_cell_mm": "0.8",
                "halftone_angle_deg": "45",
                "gray_min_power_percent": "12",
            },
            "last_accepted_fingerprint": fp.to_ascii_uppercase(),
        })),
        None,
    );
    assert_eq!(code, 200, "{saved}");
    assert_eq!(saved["result"]["exists"], true);
    assert_eq!(saved["result"]["last_accepted_fingerprint"], fp);
    assert_eq!(saved["result"]["form"]["halftone_cell_mm"], "0.8");
    assert_eq!(saved["result"]["form"]["halftone_angle_deg"], "45");
    assert_eq!(saved["result"]["form"]["gray_min_power_percent"], "12");

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
    assert_eq!(patched["result"]["form"]["halftone_cell_mm"], "0.8");
    assert_eq!(patched["result"]["form"]["halftone_angle_deg"], "45");
    assert_eq!(patched["result"]["form"]["gray_min_power_percent"], "12");

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
    assert_eq!(reloaded["result"]["form"]["halftone_cell_mm"], "0.8");
    assert_eq!(reloaded["result"]["form"]["halftone_angle_deg"], "45");
    assert_eq!(reloaded["result"]["form"]["gray_min_power_percent"], "12");
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

#[test]
fn connection_check_reports_soft_limits_homing_identity_and_remembers_firmware_work_area() {
    use std::io::{BufRead, BufReader, Write};
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let service = cors_test_service()
        .with_runtime_path(temp.path().to_path_buf())
        .with_generated_root(gen_dir);

    // 1) 未握手记下固件行程时，省略 work_area_* 回退默认 100 mm：180 mm 宽矩阵被拦截，而显式带 work_area 时按请求值放行
    let matrix_150 = serde_json::json!({
        "material": "椴木",
        "thickness_mm": 3,
        "laser_mode": "engrave",
        "engraving_mode": "raster",
        "rows": 2,
        "columns": 12,
        "cell_size_mm": 10.0,
        "cell_gap_mm": 2.0
    });
    let unprobed_body = matrix_150.to_string();
    let unprobed_resp = parse_json_response(
        &service.handle_http(
            format!(
                "POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                unprobed_body.len(),
                unprobed_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(
        unprobed_resp["success"], false,
        "未记下固件行程且未传 work_area 时应回退 100 mm 拦截: {unprobed_resp}"
    );
    assert!(
        unprobed_resp["result"]
            .as_str()
            .unwrap_or("")
            .contains("100×100"),
        "{unprobed_resp}"
    );

    let mut explicit_matrix = matrix_150.clone();
    explicit_matrix["work_area_width_mm"] = serde_json::json!(220);
    explicit_matrix["work_area_height_mm"] = serde_json::json!(150);
    let explicit_body = explicit_matrix.to_string();
    let explicit_resp = parse_json_response(
        &service.handle_http(
            format!(
                "POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                explicit_body.len(),
                explicit_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(
        explicit_resp["success"], true,
        "显式传 work_area_* 时优先按请求值校验: {explicit_resp}"
    );

    // 2) 工作台握手成功后，返回 soft_limits_enabled、homing_enabled、identity，并将固件 X/Y 行程持久化到 laser_verified_connection
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok() && !line.is_empty() {
                match line.trim() {
                    "$$" => {
                        let _ = write!(
                            writer,
                            "$20=0\r\n$22=1\r\n$23=0\r\n$32=1\r\n$110=5000.0\r\n$111=3000.0\r\n$120=900.0\r\n$121=800.0\r\n$130=320.0\r\n$131=220.0\r\nok\r\n"
                        );
                    }
                    "$CD" => {
                        let _ = write!(writer, "error:3\r\n");
                    }
                    "$I" => {
                        let _ = write!(writer, "[VER:1.1h.20190825:]\r\n[OPT:V,15,128]\r\nok\r\n");
                    }
                    _ => {
                        let _ = write!(writer, "ok\r\n");
                    }
                }
                line.clear();
            }
        }
    });
    let check_body = format!(
        r#"{{"connection_mode":"network","network_host":"127.0.0.1","network_telnet_port":{port},"network_timeout":2}}"#
    );
    let check_resp = parse_json_response(
        &service.handle_http(
            format!(
                "POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                check_body.len(),
                check_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(check_resp["result"]["connected"], true, "{check_resp}");
    assert_eq!(check_resp["result"]["soft_limits_enabled"], false);
    assert_eq!(check_resp["result"]["homing_enabled"], true);
    assert_eq!(
        check_resp["result"]["identity"],
        serde_json::json!({
            "firmware": "Grbl",
            "version": "1.1h.20190825",
            "machine_name": null
        })
    );
    let remembered = service.verified_connection().expect("verified_connection");
    assert_eq!(remembered["machine_limits"]["x_travel_mm"], 320.0);
    assert_eq!(remembered["machine_limits"]["y_travel_mm"], 220.0);

    // 3) 之后省略 work_area_* 的生成路由（矩阵、文字等）自动采用记下的 320×220 mm 固件行程；重启后同样生效
    let reloaded = cors_test_service()
        .with_runtime_path(temp.path().to_path_buf())
        .with_generated_root(temp.path().join("generated"));
    let remembered_resp = parse_json_response(
        &reloaded.handle_http(
            format!(
                "POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                unprobed_body.len(),
                unprobed_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(
        remembered_resp["success"], true,
        "省略 work_area_* 且已握手时应采用记忆的 320×220 mm 固件行程: {remembered_resp}"
    );

    // 显式传更小的 work_area_width_mm=120 时仍以请求参数为准，不拿 320 mm 覆盖请求值
    let mut smaller_explicit = matrix_150;
    smaller_explicit["work_area_width_mm"] = serde_json::json!(120);
    smaller_explicit["work_area_height_mm"] = serde_json::json!(120);
    let smaller_body = smaller_explicit.to_string();
    let smaller_resp = parse_json_response(
        &reloaded.handle_http(
            format!(
                "POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                smaller_body.len(),
                smaller_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(
        smaller_resp["success"], false,
        "显式传 work_area 时不得被记忆的固件行程覆盖: {smaller_resp}"
    );

    // 4) 省略 machine_motion 的 AI/MCP 生成请求自动沿用握手记忆的固件最大速度与加速度，触发超速提示
    let ai_fast_text = serde_json::json!({
        "text": "MOSS",
        "material": "椴木",
        "thickness_mm": 3,
        "manual_params": {"power_percent": 40, "feed_rate": 4500, "passes": 1}
    })
    .to_string();
    let ai_fast_resp = parse_json_response(
        &reloaded.handle_http(
            format!(
                "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                ai_fast_text.len(),
                ai_fast_text
            )
            .as_bytes(),
        ),
    );
    assert_eq!(ai_fast_resp["success"], true, "{ai_fast_resp}");
    assert_eq!(
        ai_fast_resp["result"]["artifacts"]["warnings"][0].as_str(),
        Some(
            "雕刻速度 4500 mm/min 超过固件最大速度 3000 mm/min，固件会自动降速，工时与出光效果会偏离设定"
        ),
        "省略 machine_motion 时应沿用握手记下的固件最大速度告警: {ai_fast_resp}"
    );

    // 5) 请求显式带了另一台未握手的连接目标时，不得套用旧机器记忆的 320×220 mm 行程
    let mut other_target_req = serde_json::json!({
        "material": "椴木",
        "thickness_mm": 3,
        "rows": 8,
        "columns": 8,
        "connection_mode": "network",
        "network_host": "192.0.2.99",
        "network_telnet_port": 23
    });
    let other_body = other_target_req.to_string();
    let other_resp = parse_json_response(
        &reloaded.handle_http(
            format!(
                "POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                other_body.len(),
                other_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(
        other_resp["success"], false,
        "请求指定了不同于 verified_connection 的设备目标时应回退 100×100 mm: {other_resp}"
    );
    let unconfigured_serial_req = serde_json::json!({
        "material": "椴木",
        "thickness_mm": 3,
        "rows": 8,
        "columns": 8,
        "connection_mode": "serial",
        "port": ""
    })
    .to_string();
    let unconfigured_serial_resp = parse_json_response(
        &reloaded.handle_http(
            format!(
                "POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                unconfigured_serial_req.len(),
                unconfigured_serial_req
            )
            .as_bytes(),
        ),
    );
    assert_eq!(
        unconfigured_serial_resp["success"], false,
        "显式切到未选串口模式时不得沿用旧网络机器记忆行程: {unconfigured_serial_resp}"
    );
    other_target_req["network_host"] = serde_json::json!("127.0.0.1");
    other_target_req["network_telnet_port"] = serde_json::json!(port);
    let same_target_body = other_target_req.to_string();
    let same_target_resp = parse_json_response(
        &reloaded.handle_http(
            format!(
                "POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                same_target_body.len(),
                same_target_body
            )
            .as_bytes(),
        ),
    );
    assert_eq!(
        same_target_resp["success"], true,
        "请求目标与 verified_connection 一致时沿用 320×220 mm 行程: {same_target_resp}"
    );
}

#[test]
fn preview_warns_when_feed_exceeds_firmware_max_rate_without_clamping() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let service = cors_test_service().with_generated_root(gen_dir);

    let text_req = serde_json::json!({
        "text": "MOSS",
        "material": "椴木",
        "thickness_mm": 3,
        "manual_params": {"power_percent": 40, "feed_rate": 4500, "passes": 1},
        "machine_motion": {
            "x_max_rate_mm_per_min": 5000,
            "y_max_rate_mm_per_min": 3000
        }
    })
    .to_string();
    let text_resp = parse_json_response(
        &service.handle_http(
            format!(
                "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                text_req.len(),
                text_req
            )
            .as_bytes(),
        ),
    );
    assert_eq!(text_resp["success"], true, "{text_resp}");
    assert_eq!(
        text_resp["result"]["artifacts"]["actual_params"]["feed_rate"].as_f64(),
        Some(4500.0),
        "速度不得被夹紧改动"
    );
    let warnings = text_resp["result"]["artifacts"]["warnings"]
        .as_array()
        .expect("应返回超速警告数组");
    assert_eq!(
        warnings[0].as_str(),
        Some(
            "雕刻速度 4500 mm/min 超过固件最大速度 3000 mm/min，固件会自动降速，工时与出光效果会偏离设定"
        )
    );

    // 未超过两轴较小最大速度时不返回超速警告
    let ok_req = serde_json::json!({
        "text": "MOSS",
        "material": "椴木",
        "thickness_mm": 3,
        "manual_params": {"power_percent": 40, "feed_rate": 2800, "passes": 1},
        "machine_motion": {
            "x_max_rate_mm_per_min": 5000,
            "y_max_rate_mm_per_min": 3000
        }
    })
    .to_string();
    let ok_resp = parse_json_response(
        &service.handle_http(
            format!(
                "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                ok_req.len(),
                ok_req
            )
            .as_bytes(),
        ),
    );
    assert_eq!(ok_resp["success"], true, "{ok_resp}");
    assert!(ok_resp["result"]["artifacts"]["warnings"].is_null());
}

/// LB1：HTTP / MCP / 语音三条入口调用 home 时，固件 `$22=0` 必须在握手预检拦截，不发 `$H`、不触发急停兜底也不留下待确认任务。
#[test]
fn lb1_home_rejected_across_http_mcp_and_voice_when_dollar22_is_zero() {
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let realtime = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let w_clone = written.clone();
    let r_clone = realtime.clone();
    let factory: TransportFactory = std::sync::Arc::new(move |_| {
        struct RecTransport {
            inner: laser_device::FakeGrbl,
            lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
            rt: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        }
        impl laser_device::DeviceTransport for RecTransport {
            fn write_line(&self, line: &str) -> Result<(), String> {
                self.lines.lock().unwrap().push(line.to_owned());
                self.inner.write_line(line)
            }
            fn write_realtime(&self, cmd: laser_device::RealtimeCommand) -> Result<(), String> {
                self.rt.lock().unwrap().push(cmd.byte());
                self.inner.write_realtime(cmd)
            }
            fn read_line(&self) -> Result<Option<String>, String> {
                self.inner.read_line()
            }
        }
        Ok(Box::new(RecTransport {
            inner: laser_device::FakeGrbl::new([
                "$22=0".to_owned(),
                "$32=1".to_owned(),
                "ok".to_owned(),
                "error:3".to_owned(),
            ]),
            lines: w_clone.clone(),
            rt: r_clone.clone(),
        }))
    });
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    {
        let guard = db.lock().unwrap();
        guard
            .voice_device_seen("dev-lb1", "client-lb1", "", "", "", 0.0)
            .unwrap();
        guard
            .set_voice_device_status("dev-lb1", laser_runtime::VOICE_DEVICE_ALLOWED, 0.0)
            .unwrap();
    }
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_db(db)
    .with_transport_factory(factory);
    let voice_session_id = "voice-lb1-test";
    service
        .register_voice_session("dev-lb1", "client-lb1", voice_session_id)
        .unwrap();

    for ctx in [
        laser_contracts::CallerContext::Desktop,
        laser_contracts::CallerContext::McpBridge,
        laser_contracts::CallerContext::Voice {
            device_id: "dev-lb1".into(),
            session_id: voice_session_id.into(),
        },
    ] {
        written.lock().unwrap().clear();
        realtime.lock().unwrap().clear();
        let (status, body) = service.dispatch_json(
            "POST",
            "/api/safe-action",
            &serde_json::json!({
                "action": "home",
                "confirmed": true,
                "connection_mode": "network",
                "host": "127.0.0.1",
                "telnet_port": 23
            }),
            ctx.clone(),
        );
        assert_eq!(status, 400, "{ctx:?}: {body}");
        assert_eq!(body["success"], false);
        assert_eq!(body["detail"], "homing_disabled", "{body}");
        assert!(
            !written.lock().unwrap().iter().any(|l| l == "$H"),
            "{ctx:?} must not send $H: {:?}",
            written.lock().unwrap()
        );
        assert!(
            realtime.lock().unwrap().is_empty(),
            "{ctx:?} must not send 0x18 fallback: {:?}",
            realtime.lock().unwrap()
        );
        // 也不留下待确认停止任务
        let (_, targets) = service.dispatch_json(
            "GET",
            "/api/jobs/stop-targets",
            &serde_json::Value::Null,
            laser_contracts::CallerContext::Loopback,
        );
        assert_eq!(targets["result"]["jobs"].as_array().unwrap().len(), 0);
    }
}

/// LB2：空跑开关门禁（仅桌面主窗口可开关，局域网/语音/MCP/无桌面会话的回环/带测试头脚本一律 403）+ 限速校验 + 全出口强制改写与关闭恢复。
#[test]
fn lb2_dry_run_switch_gate_and_backend_forced_rewriting() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    {
        let guard = db.lock().unwrap();
        guard
            .voice_device_seen("dev-lb2", "client-lb2", "", "", "", 0.0)
            .unwrap();
        guard
            .set_voice_device_status("dev-lb2", laser_runtime::VOICE_DEVICE_ALLOWED, 0.0)
            .unwrap();
    }
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf())
    .with_runtime_db(db)
    .with_generated_root(gen_dir)
    .with_transport_factory(recording_transport_factory(20_000, written.clone()));
    let desktop_token = service.enable_desktop_session().unwrap();
    let voice_sid = "voice-lb2-gate";
    service
        .register_voice_session("dev-lb2", "client-lb2", voice_sid)
        .unwrap();

    // 默认关闭
    let (_, info0) = service.dispatch_json(
        "GET",
        "/api/system/info",
        &serde_json::Value::Null,
        laser_contracts::CallerContext::Loopback,
    );
    assert_eq!(info0["result"]["laser_dry_run_enabled"], false);
    assert_eq!(info0["result"]["dry_run_max_feed_mm_min"], 2000);

    // 局域网 / 语音 / MCP / 网关 / 无桌面会话回环 均禁止开关
    for ctx in [
        laser_contracts::CallerContext::Lan {
            pairing_id: "p1".into(),
        },
        laser_contracts::CallerContext::Voice {
            device_id: "dev-lb2".into(),
            session_id: voice_sid.into(),
        },
        laser_contracts::CallerContext::McpBridge,
        laser_contracts::CallerContext::Gateway {
            session_id: "gw".into(),
        },
        laser_contracts::CallerContext::Loopback,
    ] {
        let (st, body) = service.dispatch_json(
            "POST",
            "/api/settings",
            &serde_json::json!({"laser_dry_run_enabled": true}),
            ctx.clone(),
        );
        assert_eq!(st, 403, "{ctx:?} must be denied: {body}");
    }

    // 带 X-Moss-Bench 头的请求即使带桌面会话也禁止开关空跑
    let bench_payload = r#"{"laser_dry_run_enabled":true}"#;
    let bench_req = format!(
        "POST /api/settings HTTP/1.1\r\nHost: localhost\r\nX-Moss-Desktop-Session: {desktop_token}\r\nX-Moss-Bench: dryrun\r\nContent-Length: {}\r\n\r\n{bench_payload}",
        bench_payload.len()
    );
    let bench_resp = parse_json_response(&service.handle_http(bench_req.as_bytes()));
    assert_eq!(bench_resp["success"], false);
    assert_eq!(
        bench_resp["detail"]["error_code"],
        "bench_cannot_toggle_dry_run"
    );

    // 速度上限越界拒绝
    let (bad_st, _) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({"laser_dry_run_enabled": true, "dry_run_max_feed_mm_min": 200}),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(bad_st, 400);
    let (bad_st2, _) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({"laser_dry_run_enabled": true, "dry_run_max_feed_mm_min": 6000}),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(bad_st2, 400);

    // 桌面端开启空跑（限速 1600 -> 折算 200% 倍率后下发上限 F800）
    let (ok_st, ok_body) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({"laser_dry_run_enabled": true, "dry_run_max_feed_mm_min": 1600}),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(ok_st, 200, "{ok_body}");
    assert_eq!(ok_body["result"]["laser_dry_run_enabled"], true);
    assert_eq!(ok_body["result"]["dry_run_max_feed_mm_min"], 1600);
    assert_eq!(
        ok_body["result"]["laser_dry_run_effective_max_feed_mm_min"],
        800
    );
    assert!(
        ok_body["result"]["laser_dry_run_remaining_seconds"]
            .as_u64()
            .unwrap()
            > 3500
    );

    // 1) 点射（原本带 M3 S10）在空跑期也强制变成 S0
    written.lock().unwrap().clear();
    let (pf_st, pf_body) = service.dispatch_json(
        "POST",
        "/api/device/pointer-fire",
        &serde_json::json!({
            "confirmed": true,
            "power": 10,
            "duration_ms": 50,
            "connection_mode": "network",
            "host": "127.0.0.1",
            "telnet_port": 23
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(pf_st, 200, "{pf_body}");
    let pf_lines = written.lock().unwrap().clone();
    assert!(
        pf_lines.iter().any(|l| l == "G21 G94 G1 M5 S0 F800"),
        "{pf_lines:?}"
    );
    assert!(pf_lines.iter().any(|l| l == "M3 S0"), "{pf_lines:?}");
    assert!(
        !pf_lines.iter().any(|l| l.contains("S10")),
        "点射不得出光: {pf_lines:?}"
    );

    // 2) 相对移动（原本发 G0 X5 Y0 F3000）被改写为 G1 X5.000 Y0.000 F800 S0
    written.lock().unwrap().clear();
    let (mv_st, mv_body) = service.dispatch_json(
        "POST",
        "/api/safe-action",
        &serde_json::json!({
            "action": "relative_move",
            "x": 5.0,
            "y": 0.0,
            "feed_rate": 3000.0,
            "confirmed": true,
            "connection_mode": "network",
            "host": "127.0.0.1",
            "telnet_port": 23
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(mv_st, 200, "{mv_body}");
    let mv_lines = written.lock().unwrap().clone();
    assert!(
        mv_lines.iter().any(|l| l == "G21 G94 G1 M5 S0 F800"),
        "{mv_lines:?}"
    );
    assert!(
        mv_lines.iter().any(|l| l == "G1 X5.000 Y0.000 F800 S0"),
        "{mv_lines:?}"
    );
    assert!(
        !mv_lines.iter().any(|l| l.starts_with("G0")),
        "G0 须全部改写为限速 G1: {mv_lines:?}"
    );

    // 3) 关闭开关后恢复原样（点射恢复 M3 S10，相对移动恢复 G0 F3000）
    let (off_st, _) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({"laser_dry_run_enabled": false}),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(off_st, 200);
    written.lock().unwrap().clear();
    let (pf2_st, _) = service.dispatch_json(
        "POST",
        "/api/device/pointer-fire",
        &serde_json::json!({
            "confirmed": true,
            "power": 10,
            "duration_ms": 50,
            "connection_mode": "network",
            "host": "127.0.0.1",
            "telnet_port": 23
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(pf2_st, 200);
    assert!(
        written.lock().unwrap().iter().any(|l| l == "M3 S10"),
        "关闭空跑后须恢复原样: {:?}",
        written.lock().unwrap()
    );
}

/// LB4：X-Moss-Bench 请求头只作 ProcessTrace 来源标签（不授任何权限），空跑开启时手动操作也标“空跑模式”。
#[test]
fn lb4_process_trace_tags_bench_sources_and_dry_run_without_granting_privileges() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf())
    .with_generated_root(gen_dir);
    let desktop_token = service.enable_desktop_session().unwrap();

    // 打开完整过程记录
    let (st, _) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({"process_trace_enabled": true}),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st, 200);

    // 1) X-Moss-Bench: readonly 绝不授予桌面权限（尝试改设置仍 403）
    let denied_body = r#"{"auto_check_update":false}"#;
    let denied_req = format!(
        "POST /api/settings HTTP/1.1\r\nHost: localhost\r\nX-Moss-Bench: readonly\r\nContent-Length: {}\r\n\r\n{denied_body}",
        denied_body.len()
    );
    let denied_raw = service.handle_http(denied_req.as_bytes());
    assert!(
        denied_raw.starts_with(b"HTTP/1.1 403"),
        "X-Moss-Bench 不得授予权限"
    );

    // 2) 带 X-Moss-Bench: readonly 的生成/检查请求记为 ai_laser_bench_readonly + AI 激光测试 · 只读核对
    let ro_body = r#"{"text":"RO","material":"椴木","thickness_mm":3}"#;
    let ro_req = format!(
        "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nX-Moss-Bench: readonly\r\nContent-Length: {}\r\n\r\n{ro_body}",
        ro_body.len()
    );
    let ro_resp = parse_json_response(&service.handle_http(ro_req.as_bytes()));
    assert_eq!(ro_resp["success"], true, "{ro_resp}");

    // 3) 开启空跑模式后：
    //    - 普通桌面手动请求（无 X-Moss-Bench 头）标上 dry_run:true 与“空跑模式”；
    //    - 带 X-Moss-Bench: dryrun 的请求同时标上 ai_laser_bench_dryrun、“AI 激光测试 · 空跑”与“空跑模式”。
    let (on_st, _) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({"laser_dry_run_enabled": true}),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(on_st, 200);

    let manual_body = r#"{"text":"MAN","material":"椴木","thickness_mm":3}"#;
    let manual_req = format!(
        "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nX-Moss-Desktop-Session: {desktop_token}\r\nContent-Length: {}\r\n\r\n{manual_body}",
        manual_body.len()
    );
    let manual_resp = parse_json_response(&service.handle_http(manual_req.as_bytes()));
    assert_eq!(manual_resp["success"], true);

    let dry_body = r#"{"text":"DRY","material":"椴木","thickness_mm":3}"#;
    let dry_req = format!(
        "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nX-Moss-Bench: dryrun\r\nContent-Length: {}\r\n\r\n{dry_body}",
        dry_body.len()
    );
    let dry_resp = parse_json_response(&service.handle_http(dry_req.as_bytes()));
    assert_eq!(dry_resp["success"], true);

    service.process_trace().unwrap().flush();
    let day = std::fs::read_dir(temp.path().join(laser_runtime::process_trace::TRACE_DIR))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("trace day dir");
    let jsonl =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL)).unwrap();
    let txt =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_TXT)).unwrap();
    let records: Vec<serde_json::Value> = jsonl
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert!(
        records
            .iter()
            .any(|r| r["source"] == "ai_laser_bench_readonly"),
        "缺少 ai_laser_bench_readonly 来源字段: {jsonl}"
    );
    assert!(
        records
            .iter()
            .any(|r| r["source"] == "ai_laser_bench_dryrun" && r["dry_run"] == true),
        "缺少 ai_laser_bench_dryrun + dry_run 标记: {jsonl}"
    );
    assert!(
        records
            .iter()
            .any(|r| r["origin"] == "desktop" && r.get("source").is_none() && r["dry_run"] == true),
        "空跑期手动操作也必须标上 dry_run:true: {jsonl}"
    );
    assert!(txt.contains("AI 激光测试 · 只读核对"), "{txt}");
    assert!(txt.contains("AI 激光测试 · 空跑"), "{txt}");
    assert!(txt.contains("空跑模式"), "{txt}");
}

/// LC：空跑模式发送预检拒绝含 G28 / G30 / G93 的文件（G28.1 放行），传输层兜底替换为无运动注释并写带来源标签的过程告警。
#[test]
fn lc_dry_run_blocks_g28_g30_in_send_preflight_and_transport_fallback_logs_warning() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    let upload_dir = temp.path().join("uploads");
    std::fs::create_dir_all(&gen_dir).unwrap();
    std::fs::create_dir_all(&upload_dir).unwrap();
    let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf())
    .with_generated_root(gen_dir)
    .with_upload_root(upload_dir.clone())
    .with_transport_factory(recording_transport_factory(20_000, written.clone()));

    // 开启完整过程记录与空跑模式
    let (st, _) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({
            "process_trace_enabled": true,
            "laser_dry_run_enabled": true,
            "dry_run_max_feed_mm_min": 1600
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st, 200);

    // 1) 含 G28 / G30 / G93 的外部 G-code 在发送预检阶段直接被拒，零设备写入
    for (filename, content, expected_code) in [
        (
            "has-g28.gcode",
            "G21\nG90\nG28 X0 Y0\nG1 X5 Y0 F600 S200\nM5\n",
            "dry_run_rapid_home_blocked",
        ),
        (
            "has-g30.gcode",
            "G21\nG90\ng30\nG1 X5 Y0 F600 S200\nM5\n",
            "dry_run_rapid_home_blocked",
        ),
        (
            "has-g93.gcode",
            "G21\nG93\nG1 X5 Y0 F60 S200\nM5\n",
            "dry_run_speed_mode_blocked",
        ),
    ] {
        std::fs::write(upload_dir.join(filename), content).unwrap();
        let (prev_st, prev_body) = service.dispatch_json(
            "POST",
            "/api/workflow",
            &serde_json::json!({
                "action": "preview",
                "source_type": "gcode",
                "gcode_file": filename,
                "material": "椴木",
                "thickness_mm": 3
            }),
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(prev_st, 200, "{filename}: {prev_body}");
        let wf_id = prev_body["result"]["workflow_id"].as_str().unwrap();

        written.lock().unwrap().clear();
        let (send_st, send_body) = service.dispatch_json(
            "POST",
            "/api/workflow",
            &serde_json::json!({
                "action": "confirm_send",
                "workflow_id": wf_id,
                "confirmed": true,
                "connection_mode": "network",
                "host": "127.0.0.1",
                "telnet_port": 23
            }),
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(send_st, 400, "{filename} 必须被空跑预检拒绝: {send_body}");
        assert_eq!(
            send_body["detail"]["error_code"], expected_code,
            "{filename}: {send_body}"
        );
        assert!(
            written.lock().unwrap().is_empty(),
            "{filename} 被拒后绝不能向设备写入任何字节: {:?}",
            written.lock().unwrap()
        );
    }

    // 2) 仅含 G28.1（存储位置、不运动）的外部 G-code 在空跑模式下放行
    std::fs::write(
        upload_dir.join("store-g28-1.gcode"),
        "G21\nG90\nG28.1\nG1 X5 Y0 F600 S200\nM5\n",
    )
    .unwrap();
    let (prev_ok_st, prev_ok_body) = service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({
            "action": "preview",
            "source_type": "gcode",
            "gcode_file": "store-g28-1.gcode",
            "material": "椴木",
            "thickness_mm": 3
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(prev_ok_st, 200, "{prev_ok_body}");
    let wf_ok_id = prev_ok_body["result"]["workflow_id"].as_str().unwrap();
    let (send_ok_st, send_ok_body) = service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({
            "action": "confirm_send",
            "workflow_id": wf_ok_id,
            "confirmed": true,
            "connection_mode": "network",
            "host": "127.0.0.1",
            "telnet_port": 23
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(send_ok_st, 200, "G28.1 应放行发送: {send_ok_body}");
    // 等后台发送线程结束，避免与其他断言竞争同一目标锁
    for _ in 0..80 {
        let (_, status_body) = service.dispatch_json(
            "POST",
            "/api/workflow",
            &serde_json::json!({"action": "status", "workflow_id": wf_ok_id}),
            laser_contracts::CallerContext::Desktop,
        );
        if matches!(
            status_body["result"]["status"].as_str(),
            Some("completed" | "failed" | "cancelled")
        ) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // 3) 传输层兜底：即使绕过预检直接通过 open_device_transport 写入 G28 X0 Y0，
    //    也不会发运动指令，而是替换为 `; dry-run blocked G28` 并向 ProcessTrace 记告警（带来源标签）
    written.lock().unwrap().clear();
    {
        let _scope = crate::process_trace::enter_bench_trace_scope(Some(
            crate::process_trace::BenchTraceSource::Dryrun,
        ));
        let transport = service
            .open_device_transport(&laser_device::ConnectionSpec::Tcp {
                host: "127.0.0.1".into(),
                port: 23,
            })
            .unwrap();
        transport.write_line("G28 X0 Y0").unwrap();
    }
    assert_eq!(
        written.lock().unwrap().as_slice(),
        &["; dry-run blocked G28".to_string()]
    );

    service.process_trace().unwrap().flush();
    let day = std::fs::read_dir(temp.path().join(laser_runtime::process_trace::TRACE_DIR))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("trace day dir");
    let jsonl =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_JSONL)).unwrap();
    let txt =
        std::fs::read_to_string(day.join(laser_runtime::process_trace::TIMELINE_TXT)).unwrap();
    let records: Vec<serde_json::Value> = jsonl
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        records.iter().any(|r| {
            r["kind"] == "dry_run_blocked"
                && r["level"] == "warn"
                && r["code"] == "G28"
                && r["source"] == "ai_laser_bench_dryrun"
                && r["dry_run"] == true
        }),
        "传输层兜底必须写带来源与空跑标签的告警事件: {jsonl}"
    );
    assert!(
        txt.contains("[AI 激光测试 · 空跑 · 空跑模式] 空跑传输层兜底拦截 G28"),
        "{txt}"
    );
}

/// r77（2026-11-28）：`/api/connection/check` 连上时返回整理好的 `capabilities` 与中文摘要；
/// 有效行程按“手填 → 固件 → 默认 100 mm”逐轴取值并标来源，固件未报字段一律写“未知”。
#[test]
fn connection_check_reports_structured_capabilities_and_unknown_for_missing_fields() {
    use std::io::{BufRead, BufReader, Write};

    let spawn_grbl =
        |dollar_dollar: &'static str, dollar_cd: &'static str, dollar_i: &'static str| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok() && !line.is_empty() {
                        match line.trim() {
                            "$$" => {
                                let _ = write!(writer, "{dollar_dollar}");
                            }
                            "$CD" => {
                                let _ = write!(writer, "{dollar_cd}");
                            }
                            "$I" => {
                                let _ = write!(writer, "{dollar_i}");
                            }
                            _ => {
                                let _ = write!(writer, "ok\r\n");
                            }
                        }
                        line.clear();
                    }
                }
            });
            port
        };

    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    );

    // 1. 完整 FluidNC 响应 + X 轴手填覆盖
    let full_port = spawn_grbl(
        "$20=1\r\n$22=1\r\n$30=1000\r\n$32=1\r\n$130=300.000\r\n$131=200.000\r\nok\r\n",
        "axes:\r\n  x:\r\n    max_travel_mm: 300\r\n    soft_limits: true\r\n    homing:\r\n      cycle: 1\r\n  y:\r\n    max_travel_mm: 200\r\n    soft_limits: true\r\nok\r\n",
        "[VER:4.1 FluidNC v4.1.1 (esp32-wifi) :g1234]\r\n[MSG:Machine: Yisu-3020]\r\nok\r\n",
    );
    let (st, body) = service.dispatch_json(
        "POST",
        "/api/connection/check",
        &serde_json::json!({
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": full_port,
            "auto_fallback": false,
            "work_area_width_mm": 260
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st, 200, "{body}");
    assert_eq!(body["result"]["connected"], true, "{body}");
    let caps = &body["result"]["capabilities"];
    assert_eq!(caps["effective_travel_mm"]["x"], 260.0);
    assert_eq!(caps["effective_travel_mm"]["x_source"], "manual");
    assert_eq!(caps["effective_travel_mm"]["y"], 200.0);
    assert_eq!(caps["effective_travel_mm"]["y_source"], "firmware");
    assert_eq!(caps["homing_enabled"], true);
    assert_eq!(caps["soft_limits_enabled"], true);
    assert_eq!(caps["spindle_max_s"], 1000.0);
    assert_eq!(caps["laser_mode_enabled"], true);
    assert_eq!(caps["firmware_type"], "FluidNC v4.1.1");
    let cap_summary = caps["summary"].as_str().unwrap();
    assert!(cap_summary.contains("X 260 mm（手填）"), "{cap_summary}");
    assert!(cap_summary.contains("Y 200 mm（固件）"), "{cap_summary}");
    assert!(cap_summary.contains("回零已开启"), "{cap_summary}");
    assert!(cap_summary.contains("S 上限1000"), "{cap_summary}");

    // 2. 极简响应（缺 $130/$131/$20/$22/$30/$CD/$I）：行程默认 100 mm，其余全标“未知”
    let minimal_port = spawn_grbl("$32=1\r\nok\r\n", "error:3\r\n", "error:3\r\n");
    let (st2, body2) = service.dispatch_json(
        "POST",
        "/api/connection/check",
        &serde_json::json!({
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": minimal_port,
            "auto_fallback": false
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st2, 200, "{body2}");
    let caps2 = &body2["result"]["capabilities"];
    assert_eq!(caps2["effective_travel_mm"]["x"], 100.0);
    assert_eq!(caps2["effective_travel_mm"]["x_source"], "default");
    assert_eq!(caps2["homing_label"], "未知");
    assert_eq!(caps2["soft_limits_label"], "未知");
    assert_eq!(caps2["spindle_max_s_label"], "未知");
    assert_eq!(caps2["firmware_type"], "未知");
    assert_eq!(caps2["laser_mode_label"], "已开启");
}

/// r77（2026-11-28）：`laser_workflow_tool(action=status)`（带或不带 workflow_id）基于工时估算与实时进度计算 `remaining_seconds`，
/// 缺少估算或总行数时返回 `null` 与“未知”，不编造。
#[test]
fn workflow_status_computes_remaining_seconds_and_reports_unknown_when_missing() {
    use std::sync::atomic::Ordering;

    // 1. 纯计算单元测试：流式、awaiting_completion、SD 脱机与缺失估算
    let streaming = serde_json::json!({
        "phase": "streaming",
        "mode": "stream",
        "lines_sent": 50,
        "total_lines": 200,
        "elapsed_ms": 25_000,
        "paused": false
    });
    assert_eq!(
        crate::jobs::compute_remaining_seconds(Some(120.0), &streaming),
        Some(90.0)
    );
    assert_eq!(
        crate::jobs::compute_remaining_seconds(None, &streaming),
        None,
        "缺少 time_estimate 时不得编造剩余秒数"
    );

    let awaiting = serde_json::json!({
        "phase": "awaiting_completion",
        "mode": "stream",
        "lines_sent": 200,
        "total_lines": 200,
        "elapsed_ms": 100_000
    });
    assert_eq!(
        crate::jobs::compute_remaining_seconds(Some(120.0), &awaiting),
        Some(20.0)
    );

    let sd_running = serde_json::json!({
        "phase": "streaming",
        "mode": "sd_offline",
        "lines_sent": 0,
        "total_lines": 0,
        "last_status_report": "<Run|MPos:0,0,0|SD:25.00,/sd/job.nc>"
    });
    assert_eq!(
        crate::jobs::compute_remaining_seconds(Some(200.0), &sd_running),
        Some(150.0)
    );

    // 2. 端到端 `/api/workflow {action:"status"}`（带 workflow_id 与不带 workflow_id）
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    {
        let guard = db.lock().unwrap();
        guard
            .voice_device_seen("dev-1", "client-1", "", "", "", 0.0)
            .unwrap();
        guard
            .set_voice_device_status("dev-1", laser_runtime::VOICE_DEVICE_ALLOWED, 0.0)
            .unwrap();
    }
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_db(db.clone());
    service
        .register_voice_session("dev-1", "client-1", "s-1")
        .unwrap();
    let wf_id = "wf-rem-test";
    laser_runtime::RuntimeStore::put(
        db.as_ref(),
        &laser_runtime::StoredRecord {
            kind: laser_runtime::RecordKind::Workflow,
            id: wf_id.into(),
            status: Some("sending".into()),
            payload: serde_json::json!({
                "workflow_id": wf_id,
                "status": "sending",
                "artifacts": {
                    "time_estimate": {
                        "estimated_seconds": 120.0,
                        "raw_estimated_seconds": 100.0
                    }
                },
                "updated_at": unix_seconds()
            }),
        },
    )
    .unwrap();
    let progress = std::sync::Arc::new(laser_device::JobProgress::default());
    progress.lines_sent.store(60, Ordering::Relaxed);
    progress.total_lines.store(120, Ordering::Relaxed);
    let fake_transport: std::sync::Arc<dyn laser_device::DeviceTransport> =
        std::sync::Arc::new(laser_device::FakeGrbl::new(vec!["ok".to_string()]));
    service.active_jobs.lock().unwrap().insert(
        wf_id.into(),
        std::sync::Arc::new(crate::jobs::ActiveJob {
            workflow_id: wf_id.into(),
            mode: "stream",
            target: "127.0.0.1:23".into(),
            cancel: laser_device::CancellationToken::default(),
            progress,
            transport: fake_transport,
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );

    let (st_by_id, body_by_id) = service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({"action": "status", "workflow_id": wf_id}),
        laser_contracts::CallerContext::Voice {
            device_id: "dev-1".into(),
            session_id: "s-1".into(),
        },
    );
    assert_eq!(st_by_id, 200, "{body_by_id}");
    assert_eq!(body_by_id["result"]["remaining_seconds"], 60.0);
    assert_eq!(
        body_by_id["result"]["send_result"]["remaining_seconds"],
        60.0
    );
    assert_eq!(body_by_id["result"]["remaining_text"], "约 1 分钟");
    assert_eq!(
        body_by_id["result"]["send_result"]["progress_percent"],
        50.0
    );

    // 不带 workflow_id 的 status 同样叠加实时进度与中文剩余时间播报
    let (st_no_id, body_no_id) = service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({"action": "status"}),
        laser_contracts::CallerContext::Voice {
            device_id: "dev-1".into(),
            session_id: "s-1".into(),
        },
    );
    assert_eq!(st_no_id, 200, "{body_no_id}");
    let speech = body_no_id["result"]["speech"].as_str().unwrap();
    assert!(
        speech.contains("60/120") && speech.contains("约 1 分钟"),
        "{speech}"
    );
    assert_eq!(
        body_no_id["result"]["workflows"][0]["remaining_seconds"],
        60.0
    );

    // 缺少 time_estimate 的活动任务：remaining_seconds=null、remaining_text="未知"
    let wf_no_est = "wf-rem-unknown";
    laser_runtime::RuntimeStore::put(
        db.as_ref(),
        &laser_runtime::StoredRecord {
            kind: laser_runtime::RecordKind::Workflow,
            id: wf_no_est.into(),
            status: Some("sending".into()),
            payload: serde_json::json!({
                "workflow_id": wf_no_est,
                "status": "sending",
                "artifacts": {},
                "updated_at": unix_seconds()
            }),
        },
    )
    .unwrap();
    let progress2 = std::sync::Arc::new(laser_device::JobProgress::default());
    progress2.lines_sent.store(10, Ordering::Relaxed);
    progress2.total_lines.store(100, Ordering::Relaxed);
    service.active_jobs.lock().unwrap().insert(
        wf_no_est.into(),
        std::sync::Arc::new(crate::jobs::ActiveJob {
            workflow_id: wf_no_est.into(),
            mode: "stream",
            target: "127.0.0.1:24".into(),
            cancel: laser_device::CancellationToken::default(),
            progress: progress2,
            transport: std::sync::Arc::new(laser_device::FakeGrbl::new(vec!["ok".to_string()])),
            started_at: std::time::Instant::now(),
            spec: None,
        }),
    );
    let (st_unk, body_unk) = service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({"action": "status", "workflow_id": wf_no_est}),
        laser_contracts::CallerContext::Voice {
            device_id: "dev-1".into(),
            session_id: "s-1".into(),
        },
    );
    assert_eq!(st_unk, 200, "{body_unk}");
    assert!(body_unk["result"]["remaining_seconds"].is_null());
    assert_eq!(body_unk["result"]["remaining_text"], "未知");
}

/// r78（2026-11-28）：后端机器档案（按 `target_key` 分桶，上限 16）与全局 `laser_framing_shape`：
/// 读写持久化、旧设置文件兼容、操作者设置门禁（只读客户端与非桌面调用方拒绝写）、
/// 未鉴权局域网脱敏（不泄露串口号 / 控制器 IP）与一次性迁移不覆盖后端已有值。
#[test]
fn r78_machine_profiles_crud_cap_legacy_compat_and_access_gate() {
    let temp = tempfile::tempdir().unwrap();

    // 1. 旧设置文件不含新字段（以及损坏格式）均按空处理，不报错不丢旧设置
    std::fs::write(
        temp.path().join("settings.json"),
        serde_json::json!({
            "web_port": 9666,
            "laser_machine_profiles": "corrupt-legacy-value"
        })
        .to_string(),
    )
    .unwrap();

    let auth = std::sync::Arc::new(AuthManager::new());
    let challenge = auth.issue_code(unix_seconds()).unwrap();
    let (lan_token, _) = auth
        .pair("10.0.0.12", &challenge.code, unix_seconds())
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
    .with_auth(auth.clone());
    let session = service.enable_desktop_session().unwrap();

    let (st, info0) = desktop_request(&service, "GET", "/api/system/info", None, Some(&session));
    assert_eq!(st, 200, "{info0}");
    assert_eq!(
        info0["result"]["machine_profiles"],
        serde_json::json!({ "targets": {} })
    );
    assert_eq!(info0["result"]["laser_framing_shape"], "box");
    assert_eq!(info0["result"]["can_edit_operator_settings"], true);

    // 2. 门禁：无桌面会话回环、局域网只读、语音、MCP 桥均拒绝写入 machine_profile_patch / laser_framing_shape
    let patch_body = serde_json::json!({
        "machine_profile_patch": {
            "target_key": "network:10.0.0.20:23",
            "manual_travel": { "x_travel_mm": 320, "y_travel_mm": 220 },
            "manual_motion": { "acceleration_mm_s2": 600 },
            "scan_offset_mm": 0.15,
            "scan_offset_table": [[1000, 0.1], [2500, 0.25]]
        },
        "laser_framing_shape": "hull"
    });
    let (st_no_sess, denied_no_sess) =
        desktop_request(&service, "POST", "/api/settings", Some(&patch_body), None);
    assert_eq!(st_no_sess, 403, "{denied_no_sess}");

    for caller in [
        laser_contracts::CallerContext::Lan {
            pairing_id: "pair-1".into(),
        },
        laser_contracts::CallerContext::Voice {
            device_id: "dev-1".into(),
            session_id: "s-1".into(),
        },
        laser_contracts::CallerContext::McpBridge,
    ] {
        let (st_ro, denied_ro) =
            service.dispatch_json("POST", "/api/settings", &patch_body, caller);
        assert_eq!(st_ro, 403, "{denied_ro}");
    }

    // 3. 桌面主窗口带有效会话写入成功并持久化到 settings.json，重启后读回一致
    let (st_ok, saved) = desktop_request(
        &service,
        "POST",
        "/api/settings",
        Some(&patch_body),
        Some(&session),
    );
    assert_eq!(st_ok, 200, "{saved}");
    assert_eq!(saved["result"]["laser_framing_shape"], "hull");
    let target_prof = &saved["result"]["machine_profiles"]["targets"]["network:10.0.0.20:23"];
    assert_eq!(target_prof["manual_travel"]["x_travel_mm"], 320.0);
    assert_eq!(target_prof["manual_travel"]["y_travel_mm"], 220.0);
    assert_eq!(target_prof["manual_motion"]["x_accel_mm_s2"], 600.0);
    assert_eq!(target_prof["manual_motion"]["y_accel_mm_s2"], 600.0);
    assert_eq!(target_prof["scan_offset_mm"], 0.15);
    assert_eq!(
        target_prof["scan_offset_table"],
        serde_json::json!([[1000.0, 0.1], [2500.0, 0.25]])
    );

    // 4. 未鉴权局域网 GET /api/system/info 隐藏含设备地址的 machine_profiles；已配对局域网可读但不可写
    let anon_info = parse_json_response(&service.handle_http_from(
        b"GET /api/system/info HTTP/1.1\r\nHost: 192.168.1.10:9666\r\n\r\n",
        false,
        "10.0.0.12",
        None,
    ));
    assert_eq!(anon_info["success"], true);
    assert_eq!(
        anon_info["result"]["machine_profiles"],
        serde_json::json!({ "targets": {} }),
        "未鉴权局域网不得暴露含设备 IP/串口号的档案键"
    );
    assert!(!anon_info.to_string().contains("10.0.0.20"));

    let authed_req = format!(
        "GET /api/system/info HTTP/1.1\r\nHost: 192.168.1.10:9666\r\nAuthorization: Bearer {lan_token}\r\n\r\n"
    );
    let lan_info = parse_json_response(&service.handle_http_from(
        authed_req.as_bytes(),
        false,
        "10.0.0.12",
        None,
    ));
    assert_eq!(lan_info["result"]["can_edit_operator_settings"], false);
    assert_eq!(lan_info["result"]["laser_framing_shape"], "hull");
    assert_eq!(
        lan_info["result"]["machine_profiles"]["targets"]["network:10.0.0.20:23"]["manual_travel"]
            ["x_travel_mm"],
        320.0
    );

    // 5. 一次性迁移：后端已有值不覆盖，只补齐缺失字段与新目标
    let migrate_body = serde_json::json!({
        "laser_framing_shape_migrate": "box",
        "machine_profiles_migrate": {
            "targets": {
                "network:10.0.0.20:23": {
                    "manual_travel": { "x_travel_mm": 999, "z_travel_mm": 45 },
                    "scan_offset_mm": 0.99
                },
                "serial:/dev/ttyUSB0:115200": {
                    "manual_travel": { "x_travel_mm": 180, "y_travel_mm": 160 }
                }
            }
        }
    });
    let (st_mig, migrated) = desktop_request(
        &service,
        "POST",
        "/api/settings",
        Some(&migrate_body),
        Some(&session),
    );
    assert_eq!(st_mig, 200, "{migrated}");
    assert_eq!(
        migrated["result"]["laser_framing_shape"], "hull",
        "后端已设置的 framing_shape 不得被迁移覆盖"
    );
    let existing = &migrated["result"]["machine_profiles"]["targets"]["network:10.0.0.20:23"];
    assert_eq!(existing["manual_travel"]["x_travel_mm"], 320.0);
    assert_eq!(existing["manual_travel"]["z_travel_mm"], 45.0);
    assert_eq!(existing["scan_offset_mm"], 0.15);
    assert_eq!(
        migrated["result"]["machine_profiles"]["targets"]["serial:/dev/ttyUSB0:115200"]["manual_travel"]
            ["x_travel_mm"],
        180.0
    );

    // 6. 最多保留 16 个目标（写入第 17 个淘汰最旧项），并验证重启后重新从 settings.json 读回一致
    for idx in 1..=17 {
        let key = format!("network:10.0.1.{idx}:23");
        let (st_i, body_i) = desktop_request(
            &service,
            "POST",
            "/api/settings",
            Some(&serde_json::json!({
                "machine_profile_patch": {
                    "target_key": key,
                    "manual_travel": { "x_travel_mm": 100 + idx, "y_travel_mm": 100 + idx }
                }
            })),
            Some(&session),
        );
        assert_eq!(st_i, 200, "{body_i}");
    }
    let reloaded = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf());
    let profiles = reloaded.machine_profiles_json();
    let map = profiles["targets"].as_object().unwrap();
    assert_eq!(map.len(), 16, "机器档案最多保留 16 个连接目标");
    assert!(
        !map.contains_key("network:10.0.0.20:23") && !map.contains_key("network:10.0.1.1:23"),
        "最旧目标应被淘汰: {:?}",
        map.keys().collect::<Vec<_>>()
    );
    assert!(map.contains_key("network:10.0.1.17:23"));
    assert_eq!(reloaded.framing_shape().as_str(), "hull");

    // 7. 清除单个目标后立即从档案移除
    let (st_clr, cleared) = reloaded.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({
            "machine_profile_patch": {
                "target_key": "network:10.0.1.17:23",
                "clear": true
            }
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st_clr, 200, "{cleared}");
    assert!(
        cleared["result"]["machine_profiles"]["targets"]
            .get("network:10.0.1.17:23")
            .is_none()
    );
}

/// r78（2026-11-28）：取值优先级（请求显式值 > 后端手填档案 > 固件 > 默认），
/// 语音 / MCP 无前端参与也能拿到手填行程、手填加速度、双向扫描偏移表与全局走边框形状，
/// 且发送前仍按当场读到的固件行程拦截超行程刀路（r65 / r68 不变）。
#[test]
fn r78_backend_profile_priority_voice_mcp_framing_and_firmware_send_gate() {
    use std::io::{BufRead, BufReader, Write};

    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    image::GrayImage::from_fn(200, 100, |x, _| image::Luma([(x * 255 / 199) as u8]))
        .save(gen_dir.join("wide_r78.png"))
        .unwrap();

    // 模拟真实 TCP GRBL 控制器（响应 $$ / $CD / $I），固件报行程 100×100、加速度 800 mm/s²
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let probe_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(4) {
            let Ok(stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok() && !line.is_empty() {
                match line.trim() {
                    "$$" => {
                        let _ = write!(
                            writer,
                            "$20=1\r\n$22=1\r\n$30=1000\r\n$32=1\r\n$120=800.000\r\n$121=800.000\r\n$130=100.000\r\n$131=100.000\r\nok\r\n"
                        );
                    }
                    "$CD" | "$I" => {
                        let _ = write!(writer, "error:3\r\n");
                    }
                    _ => {
                        let _ = write!(writer, "ok\r\n");
                    }
                }
                line.clear();
            }
        }
    });

    let written_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    {
        let guard = db.lock().unwrap();
        guard
            .voice_device_seen("dev-r78", "cli-r78", "", "", "", 0.0)
            .unwrap();
        guard
            .set_voice_device_status("dev-r78", laser_runtime::VOICE_DEVICE_ALLOWED, 0.0)
            .unwrap();
    }

    let firmware_100_factory: TransportFactory = std::sync::Arc::new(move |_spec| {
        // 发送前探查仍读到固件 100×100（绝不被手填 260×200 覆盖）
        let mut replies = vec![
            "$20=1".to_string(),
            "$30=1000".to_string(),
            "$32=1".to_string(),
            "$120=800.000".to_string(),
            "$121=800.000".to_string(),
            "$130=100.000".to_string(),
            "$131=100.000".to_string(),
            "ok".to_string(),
        ];
        replies.extend(std::iter::repeat_n("ok".to_string(), 20_000));
        let fake = laser_device::FakeGrbl::new(replies);
        Ok(Box::new(fake))
    });

    let target_key = format!("network:127.0.0.1:{probe_port}");
    let voice_ctx = laser_contracts::CallerContext::Voice {
        device_id: "dev-r78".into(),
        session_id: "sess-r78".into(),
    };
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf())
    .with_runtime_db(db)
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir.clone()])
    .with_transport_factory(firmware_100_factory);
    service
        .register_voice_session("dev-r78", "cli-r78", "sess-r78")
        .unwrap();

    // 1. 先由桌面端握手探测，建立已验证连接与固件行程（100×100）+ 固件加速度（800）
    let (st_chk0, chk0) = service.dispatch_json(
        "POST",
        "/api/connection/check",
        &serde_json::json!({
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": probe_port,
            "auto_fallback": false
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st_chk0, 200, "{chk0}");
    assert_eq!(chk0["result"]["connected"], true, "{chk0}");
    assert_eq!(
        chk0["result"]["capabilities"]["effective_travel_mm"]["x_source"],
        "firmware"
    );

    // 2. 桌面主窗口在后端保存该机器的手填行程（260×200）、手填加速度（200 mm/s²）与双向扫描偏移表
    let (st_set, set_body) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({
            "machine_profile_patch": {
                "target_key": target_key,
                "manual_travel": { "x_travel_mm": 260, "y_travel_mm": 200 },
                "manual_motion": { "acceleration_mm_s2": 200 },
                "scan_offset_mm": 0.15,
                "scan_offset_table": [[1000, 0.12], [3000, 0.36]]
            }
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st_set, 200, "{set_body}");

    // 3. 语音不传 work_area_* 调用 /api/connection/check：自动按“手填 → 固件 → 默认”拿到 260×200（手填）
    let (st_chk1, chk1) = service.dispatch_json(
        "POST",
        "/api/connection/check",
        &serde_json::json!({
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": probe_port,
            "auto_fallback": false
        }),
        voice_ctx.clone(),
    );
    assert_eq!(st_chk1, 200, "{chk1}");
    assert_eq!(
        chk1["result"]["capabilities"]["effective_travel_mm"]["x"],
        260.0
    );
    assert_eq!(
        chk1["result"]["capabilities"]["effective_travel_mm"]["x_source"],
        "manual"
    );
    assert!(
        chk1["result"]["capabilities"]["summary"]
            .as_str()
            .unwrap()
            .contains("有效行程 260×200 mm（手填）"),
        "{chk1}"
    );
    // 请求显式传 work_area_width_mm=180 时仍以请求为准
    let (st_chk_exp, chk_exp) = service.dispatch_json(
        "POST",
        "/api/connection/check",
        &serde_json::json!({
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": probe_port,
            "auto_fallback": false,
            "work_area_width_mm": 180
        }),
        voice_ctx.clone(),
    );
    assert_eq!(st_chk_exp, 200, "{chk_exp}");
    assert_eq!(
        chk_exp["result"]["capabilities"]["effective_travel_mm"]["x"],
        180.0
    );

    // 4. 语音 / MCP（laser_workflow_tool preview 转发到 /api/text-task/generate）不传 work_area_*、
    //    machine_motion、fill_options 生成填充文字预览：
    //    自动套用后端手填行程（允许超过固件 100mm）、手填加速度（200 mm/s²）与双向扫描偏移表
    let (st_pv, pv) = service.dispatch_json(
        "POST",
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "MOSS LASER TEST",
            "font_size_mm": 18,
            "text_fill": "fill",
            "material": "椴木",
            "thickness_mm": 3
        }),
        voice_ctx.clone(),
    );
    assert_eq!(st_pv, 200, "{pv}");
    let art = &pv["result"]["artifacts"];
    assert_eq!(
        art["time_estimate"]["acceleration_origin"], "manual",
        "{art}"
    );
    assert_eq!(art["time_estimate"]["acceleration_mm_s2"]["x"], 200.0);
    assert_eq!(
        art["fill_options"]["scan_offset_origin"], "machine_profile",
        "{art}"
    );
    assert_eq!(
        art["fill_options"]["scan_offset_table"],
        serde_json::json!([[1000.0, 0.12], [3000.0, 0.36]])
    );
    let est_manual = art["time_estimate"]["estimated_seconds"].as_f64().unwrap();

    // 请求显式传 machine_motion 与 fill_options 时以请求为准
    let (st_exp, exp) = service.dispatch_json(
        "POST",
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "MOSS LASER TEST",
            "font_size_mm": 18,
            "text_fill": "fill",
            "material": "椴木",
            "thickness_mm": 3,
            "machine_motion": { "x_accel_mm_s2": 4000, "y_accel_mm_s2": 4000 },
            "fill_options": { "bidirectional": true, "scan_offset_mm": 0.04 }
        }),
        voice_ctx.clone(),
    );
    assert_eq!(st_exp, 200, "{exp}");
    let art_exp = &exp["result"]["artifacts"];
    assert_eq!(
        art_exp["time_estimate"]["acceleration_origin"], "request",
        "{art_exp}"
    );
    assert_eq!(art_exp["time_estimate"]["acceleration_mm_s2"]["x"], 4000.0);
    assert_eq!(art_exp["fill_options"]["scan_offset_origin"], "request");
    assert_eq!(art_exp["fill_options"]["scan_offset_mm"], 0.04);
    assert!(
        est_manual
            > art_exp["time_estimate"]["estimated_seconds"]
                .as_f64()
                .unwrap(),
        "更低的手填加速度应算出更长的工时"
    );

    // 5. 语音图片预览不传 work_area_* / fill_options：用后端手填行程（260×200，可刻 150 mm 宽）和扫描偏移表
    let (st_img, img) = service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({
            "action": "preview",
            "source_type": "image",
            "image_file": "wide_r78.png",
            "width_mm": 150,
            "material": "椴木",
            "thickness_mm": 3
        }),
        voice_ctx.clone(),
    );
    assert_eq!(st_img, 200, "{img}");
    let img_art = &img["result"]["artifacts"];
    assert_eq!(img_art["width_mm"].as_f64().unwrap().round(), 150.0);
    assert_eq!(
        img_art["scan_options"]["scan_offset_origin"], "machine_profile",
        "{img_art}"
    );
    assert_eq!(
        img_art["scan_options"]["scan_offset_table"],
        serde_json::json!([[1000.0, 0.12], [3000.0, 0.36]])
    );
    assert_eq!(img_art["time_estimate"]["acceleration_origin"], "manual");

    // 6. 安全不变量（r65 / r68 / r78）：即使后端手填行程允许生成 150 mm 宽预览，
    //    发送前仍按当场读到的固件行程（100×100）拦截！
    let wide_wf_id = img["result"]["workflow_id"].as_str().unwrap();
    let (st_send, send_blocked) = service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({
            "action": "confirm_send",
            "workflow_id": wide_wf_id,
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": probe_port
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st_send, 400, "{send_blocked}");
    assert_eq!(
        send_blocked["detail"]["error_code"], "job_exceeds_travel",
        "手填行程绝不能绕过发送前固件复核: {send_blocked}"
    );

    // 7. 清掉手填运动参数后回落到固件加速度 800（acceleration_origin="firmware"）
    let (st_clr_m, _) = service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({
            "machine_profile_patch": {
                "target_key": target_key,
                "manual_motion": null
            }
        }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st_clr_m, 200);
    let (st_fw, pv_fw) = service.dispatch_json(
        "POST",
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "HI",
            "font_size_mm": 12,
            "text_fill": "fill",
            "material": "椴木",
            "thickness_mm": 3
        }),
        voice_ctx.clone(),
    );
    assert_eq!(st_fw, 200, "{pv_fw}");
    assert_eq!(
        pv_fw["result"]["artifacts"]["time_estimate"]["acceleration_origin"],
        "firmware"
    );
    assert_eq!(
        pv_fw["result"]["artifacts"]["time_estimate"]["acceleration_mm_s2"]["x"],
        800.0
    );

    // 8. 全局走边框形状（laser_framing_shape）：语音 workflow(action=frame) 跟随全局设置，
    //    hull 时只取预览冻结的 frame_hull_mm（忽略请求自带 outline，仍强制 power=0）
    let small_wf_id = pv_fw["result"]["workflow_id"].as_str().unwrap();
    let frame_service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(temp.path().to_path_buf())
    .with_runtime_db(service.runtime_db.clone().unwrap())
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir])
    .with_transport_factory(recording_transport_factory(20_000, written_lines.clone()));
    frame_service
        .register_voice_session("dev-r78", "cli-r78", "sess-r78")
        .unwrap();

    // 默认 box：走外接矩形
    let (st_box, res_box) = frame_service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({
            "action": "frame",
            "workflow_id": small_wf_id,
            "confirmed": true,
            "outline": [[99, 99], [99, 98], [98, 98]],
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": 23
        }),
        voice_ctx.clone(),
    );
    assert_eq!(st_box, 200, "{res_box}");
    assert_eq!(res_box["result"]["framing_shape"], "box");
    let box_lines = std::mem::take(&mut *written_lines.lock().unwrap());
    assert!(
        !box_lines.iter().any(|l| l.contains("99.000")),
        "请求自带 outline 必须被忽略: {box_lines:?}"
    );

    // 切换全局 laser_framing_shape = "hull"：语音走边框改沿服务器冻结的 frame_hull_mm 走，仍忽略请求 outline 且不出光
    let (st_sh, _) = frame_service.dispatch_json(
        "POST",
        "/api/settings",
        &serde_json::json!({ "laser_framing_shape": "hull" }),
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(st_sh, 200);
    let (st_hull, res_hull) = frame_service.dispatch_json(
        "POST",
        "/api/workflow",
        &serde_json::json!({
            "action": "frame",
            "workflow_id": small_wf_id,
            "confirmed": true,
            "outline": [[99, 99], [99, 98], [98, 98]],
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": 23
        }),
        voice_ctx,
    );
    assert_eq!(st_hull, 200, "{res_hull}");
    assert_eq!(res_hull["result"]["framing_shape"], "hull");
    let hull_lines = std::mem::take(&mut *written_lines.lock().unwrap());
    assert!(
        !hull_lines.iter().any(|l| l.contains("99.000")),
        "hull 模式下仍不得信任请求自带 outline: {hull_lines:?}"
    );
    assert!(
        !hull_lines
            .iter()
            .any(|l| l.contains("M3") || l.contains("M4")),
        "语音走边框永不出光: {hull_lines:?}"
    );
    assert_ne!(
        box_lines, hull_lines,
        "box 与 hull 的实际走边框轨迹应当不同"
    );
}
