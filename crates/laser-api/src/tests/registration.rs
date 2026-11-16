//! 两点对位（2026-11-16）：读位置只发状态查询，对位预览是新 workflow、新 SHA，发送照常确认。
use super::*;

/// 记录写出的每一行与每个实时字节；状态查询回固定报告。
fn status_only_factory(
    report: &'static str,
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    realtime: std::sync::Arc<std::sync::Mutex<Vec<laser_device::RealtimeCommand>>>,
) -> TransportFactory {
    struct StatusGrbl {
        report: &'static str,
        pending: std::sync::Mutex<std::collections::VecDeque<String>>,
        lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        realtime: std::sync::Arc<std::sync::Mutex<Vec<laser_device::RealtimeCommand>>>,
    }
    impl laser_device::DeviceTransport for StatusGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.lines.lock().unwrap().push(line.to_owned());
            self.pending.lock().unwrap().push_back("ok".into());
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            self.realtime.lock().unwrap().push(command);
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.pending
                    .lock()
                    .unwrap()
                    .push_back(self.report.to_owned());
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.pending.lock().unwrap().pop_front())
        }
    }
    std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
        Ok(Box::new(StatusGrbl {
            report,
            pending: Default::default(),
            lines: lines.clone(),
            realtime: realtime.clone(),
        }) as Box<dyn laser_device::DeviceTransport>)
    })
}

fn connection() -> serde_json::Value {
    serde_json::json!({
        "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23
    })
}

#[test]
fn read_position_sends_only_a_status_query_and_returns_machine_coordinates() {
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let realtime = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(status_only_factory(
        "<Idle|MPos:12.500,30.250,0.000|FS:0,0>",
        lines.clone(),
        realtime.clone(),
    ));
    let mut body = connection();
    // 客户端夹带的运动字段一律丢弃，不会变成移动
    body["x"] = serde_json::json!(50);
    body["confirmed"] = serde_json::json!(true);
    let response = post_json(&service, "/api/registration/read-position", &body);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["result"]["frame"], "mpos");
    assert_eq!(response["result"]["position"]["x"], 12.5);
    assert_eq!(response["result"]["position"]["y"], 30.25);
    assert_eq!(response["result"]["state"], "Idle");
    assert!(lines.lock().unwrap().is_empty(), "不得写任何 G-code 行");
    assert_eq!(
        *realtime.lock().unwrap(),
        vec![laser_device::RealtimeCommand::StatusQuery]
    );
}

#[test]
fn read_position_refuses_while_machine_is_moving_and_falls_back_to_wpos() {
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let realtime = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let jogging = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(status_only_factory(
        "<Jog|MPos:1,2,0>",
        lines.clone(),
        realtime.clone(),
    ));
    let response = post_json(&jogging, "/api/registration/read-position", &connection());
    assert_eq!(response["success"], false);
    assert_eq!(response["detail"]["error_code"], "registration_not_idle");

    let wpos_only = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(status_only_factory(
        "<Idle|WPos:3,4,0>",
        lines.clone(),
        realtime,
    ));
    let response = post_json(&wpos_only, "/api/registration/read-position", &connection());
    assert_eq!(response["result"]["frame"], "wpos", "{response}");
    assert_eq!(response["result"]["position"]["y"], 4.0);
    assert!(lines.lock().unwrap().is_empty());
}

fn text_preview(service: &ApiService) -> serde_json::Value {
    let preview = post_json(
        service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "MOSS", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12,
            "origin": "lower_left"
        }),
    );
    assert_eq!(preview["success"], true, "{preview}");
    preview["result"].clone()
}

fn sha256_file(path: &std::path::Path) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(std::fs::read(path).unwrap()))
}

#[test]
fn registered_preview_is_a_new_rotated_workflow_that_still_needs_confirmation() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(fake_transport_factory(200_000));
    let source = text_preview(&service);
    let source_id = source["workflow_id"].as_str().unwrap();
    let source_file = temp
        .path()
        .join(source["artifacts"]["gcode_file"].as_str().unwrap());
    let source_text = std::fs::read_to_string(&source_file).unwrap();
    let before = laser_core::gcode::motion_extents(&source_text)
        .unwrap()
        .unwrap();
    let frame = &source["artifacts"]["frame_mm"];
    let (min_x, max_x, min_y) = (
        frame["min_x"].as_f64().unwrap(),
        frame["max_x"].as_f64().unwrap(),
        frame["min_y"].as_f64().unwrap(),
    );
    let width = max_x - min_x;
    assert!(width >= 10.0, "{frame}");
    // 材料转了 90°：设计下边两个角在机器上竖着排列
    let response = post_json(
        &service,
        "/api/registration/preview",
        &serde_json::json!({
            "workflow_id": source_id,
            "design_points": [{"x": min_x, "y": min_y}, {"x": max_x, "y": min_y}],
            "measured_points": [{"x": 100.0, "y": 50.0}, {"x": 100.0, "y": 50.0 + width}],
        }),
    );
    assert_eq!(response["success"], true, "{response}");
    let record = &response["result"];
    let new_id = record["workflow_id"].as_str().unwrap();
    assert_ne!(new_id, source_id);
    assert_eq!(record["status"], "preview_ready");
    assert_eq!(record["registered_from"], source_id);
    let registration = &record["registration"];
    assert!((registration["rotation_deg"].as_f64().unwrap() - 90.0).abs() < 1e-6);
    assert_eq!(registration["scale"], 1.0);
    assert!(registration["residual_mm"].as_f64().unwrap() < 1e-6);
    let new_file = temp.path().join(
        record["confirmation_snapshot"]["gcode_file"]
            .as_str()
            .unwrap(),
    );
    let new_sha = record["confirmation_snapshot"]["gcode_sha256"]
        .as_str()
        .unwrap();
    assert_eq!(new_sha, sha256_file(&new_file));
    assert_ne!(
        new_sha,
        source["artifacts"]["gcode_sha256"].as_str().unwrap()
    );
    // 原任务文件不被改写
    assert_eq!(std::fs::read_to_string(&source_file).unwrap(), source_text);
    // 刀路宽高互换，仍以 G92 X0 Y0 相对激光头（第 2 个标记点）
    let new_text = std::fs::read_to_string(&new_file).unwrap();
    assert!(new_text.contains("G92 X0 Y0"));
    assert!(!new_text.contains("G53") && !new_text.contains("G28"));
    let after = laser_core::gcode::motion_extents(&new_text)
        .unwrap()
        .unwrap();
    assert!(
        (after.width() - before.height()).abs() < 0.01,
        "{after:?} {before:?}"
    );
    assert!(
        (after.height() - before.width()).abs() < 0.01,
        "{after:?} {before:?}"
    );
    // 设计右下角正好落在新起点（第 2 个标记点）
    let frame = &record["artifacts"]["frame_mm"];
    assert!(frame["max_x"].as_f64().unwrap().abs() < 0.5, "{frame}");
    assert!(frame["max_y"].as_f64().unwrap() <= 0.01, "{frame}");

    // 发送仍然要求 confirmed=true，并走完整门禁
    let refused = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({"action": "confirm_send", "workflow_id": new_id}),
    );
    assert_eq!(refused["success"], false, "{refused}");
    let mut send = connection();
    send["action"] = serde_json::json!("confirm_send");
    send["workflow_id"] = serde_json::json!(new_id);
    send["confirmed"] = serde_json::json!(true);
    let sent = post_json(&service, "/api/workflow", &send);
    assert_eq!(sent["success"], true, "{sent}");
    let status = wait_for_status(&service, new_id, "completed");
    assert_eq!(status["result"]["status"], "completed", "{status}");
}

#[test]
fn registered_preview_rejects_bad_points_and_tampered_source() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let source = text_preview(&service);
    let source_id = source["workflow_id"].as_str().unwrap();
    let request = |design: serde_json::Value, measured: serde_json::Value| {
        post_json(
            &service,
            "/api/registration/preview",
            &serde_json::json!({
                "workflow_id": source_id, "design_points": design, "measured_points": measured,
            }),
        )
    };
    let too_close = request(
        serde_json::json!([{"x": 0, "y": 0}, {"x": 3, "y": 0}]),
        serde_json::json!([{"x": 0, "y": 0}, {"x": 3, "y": 0}]),
    );
    assert_eq!(
        too_close["detail"]["error_code"], "registration_points_too_close",
        "{too_close}"
    );
    let mismatch = request(
        serde_json::json!([{"x": 0, "y": 0}, {"x": 30, "y": 0}]),
        serde_json::json!([{"x": 0, "y": 0}, {"x": 36, "y": 0}]),
    );
    assert_eq!(
        mismatch["detail"]["error_code"],
        "registration_scale_mismatch"
    );
    let one_point = request(
        serde_json::json!([{"x": 0, "y": 0}]),
        serde_json::json!([{"x": 0, "y": 0}, {"x": 30, "y": 0}]),
    );
    assert_eq!(
        one_point["detail"]["error_code"],
        "registration_points_invalid"
    );
    let missing = post_json(
        &service,
        "/api/registration/preview",
        &serde_json::json!({
            "workflow_id": "wf-missing",
            "design_points": [{"x": 0, "y": 0}, {"x": 30, "y": 0}],
            "measured_points": [{"x": 0, "y": 0}, {"x": 30, "y": 0}],
        }),
    );
    assert_eq!(missing["success"], false);

    // 原刀路被改动：SHA-256 不一致，拒绝拿来对位
    let source_file = temp
        .path()
        .join(source["artifacts"]["gcode_file"].as_str().unwrap());
    let mut text = std::fs::read_to_string(&source_file).unwrap();
    text.push_str("G0 X1 Y1\n");
    std::fs::write(&source_file, text).unwrap();
    let tampered = request(
        serde_json::json!([{"x": 0, "y": 0}, {"x": 30, "y": 0}]),
        serde_json::json!([{"x": 0, "y": 0}, {"x": 30, "y": 0}]),
    );
    assert_eq!(
        tampered["detail"]["error_code"], "send_gate_rejected",
        "{tampered}"
    );
}

/// 严格模式下读位置（只查询）与生成对位预览（只生成）不需要审批；对位后的发送照常按 R3 拦截。
#[test]
fn strict_mode_keeps_registered_send_behind_authority() {
    let temp = tempfile::tempdir().unwrap();
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let realtime = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_strict_mode(true)
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(status_only_factory(
        "<Idle|MPos:0,0,0>",
        lines.clone(),
        realtime,
    ));
    let read = post_json(&service, "/api/registration/read-position", &connection());
    assert_eq!(read["success"], true, "{read}");
    let source = text_preview(&service);
    let frame = &source["artifacts"]["frame_mm"];
    let (min_x, max_x) = (
        frame["min_x"].as_f64().unwrap(),
        frame["max_x"].as_f64().unwrap(),
    );
    let registered = post_json(
        &service,
        "/api/registration/preview",
        &serde_json::json!({
            "workflow_id": source["workflow_id"],
            "design_points": [{"x": min_x, "y": 0}, {"x": max_x, "y": 0}],
            "measured_points": [{"x": 10.0, "y": 10.0}, {"x": 10.0 + (max_x - min_x), "y": 12.0}],
        }),
    );
    assert_eq!(registered["success"], true, "{registered}");
    let mut send = connection();
    send["action"] = serde_json::json!("confirm_send");
    send["workflow_id"] = registered["result"]["workflow_id"].clone();
    send["confirmed"] = serde_json::json!(true);
    let (status, body) = post_with_ctx(
        &service,
        "/api/workflow",
        &send,
        laser_contracts::CallerContext::Loopback,
    );
    assert_eq!(status, 403, "{body}");
    assert_eq!(
        body["detail"]["error_code"], "strict_mode_blocked",
        "{body}"
    );
    assert!(lines.lock().unwrap().is_empty(), "未授权不得写任何行");
}
