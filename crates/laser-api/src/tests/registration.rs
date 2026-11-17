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

/// 画板任务也能对位（2026-11-16）：结果带画板视图（作业摘要、新内容指纹、指回原画板任务），
/// 画板页据此沿用自己的发送确认与重复发送防呆；发送照常要求 confirmed=true。
#[test]
fn registered_draw_preview_keeps_a_draw_view_linked_to_the_source_job() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf())
    .with_transport_factory(fake_transport_factory(200_000));
    let scene = serde_json::json!({"elements": [
        {"type": "rectangle", "x": 0, "y": 0, "width": 60, "height": 30, "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 2},
    ]});
    let draw = post_json(
        &service,
        "/api/draw/preview",
        &serde_json::json!({
            "image_data_url": super::preview::draw_test_png(), "material": "椴木", "thickness_mm": 3.0,
            "mode": "vector", "width_mm": 40, "scene_json": scene
        }),
    );
    assert_eq!(draw["success"], true, "{draw}");
    let source = &draw["result"];
    let source_id = source["workflow_id"].as_str().unwrap();
    assert!(source_id.starts_with("draw-"), "{source_id}");
    let frame = &source["artifacts"]["frame_mm"];
    let (min_x, max_x, min_y) = (
        frame["min_x"].as_f64().unwrap(),
        frame["max_x"].as_f64().unwrap(),
        frame["min_y"].as_f64().unwrap(),
    );
    let width = max_x - min_x;
    let response = post_json(
        &service,
        "/api/registration/preview",
        &serde_json::json!({
            "workflow_id": source_id,
            "design_points": [{"x": min_x, "y": min_y}, {"x": max_x, "y": min_y}],
            "measured_points": [{"x": 20.0, "y": 20.0}, {"x": 20.0 + width, "y": 20.0}],
        }),
    );
    assert_eq!(response["success"], true, "{response}");
    let record = &response["result"];
    assert_eq!(record["registered_from"], source_id);
    let lab = &record["draw_lab"];
    assert_eq!(lab["registered_from"], source_id, "{lab}");
    assert_eq!(
        lab["content_fingerprint"], record["confirmation_snapshot"]["gcode_sha256"],
        "新刀路的指纹与原画板图案不同"
    );
    assert_ne!(
        lab["content_fingerprint"],
        source["draw_lab"]["content_fingerprint"]
    );
    let summary = &lab["job_summary"];
    assert_eq!(summary["can_send"], true, "{summary}");
    assert_eq!(summary["registered"], true);
    assert_eq!(
        summary["material"],
        source["draw_lab"]["job_summary"]["material"]
    );
    assert!(
        summary["actual_width_mm"].as_f64().unwrap() > 0.0,
        "{summary}"
    );
    assert_eq!(
        record["web"]["material_preview_url"],
        source["web"]["material_preview_url"]
    );
    assert_eq!(record["artifacts"]["origin"], "registration_mark_2");

    // 非画板来源不带画板视图
    let text = text_preview(&service);
    let frame = &text["artifacts"]["frame_mm"];
    let (min_x, max_x) = (
        frame["min_x"].as_f64().unwrap(),
        frame["max_x"].as_f64().unwrap(),
    );
    let plain = post_json(
        &service,
        "/api/registration/preview",
        &serde_json::json!({
            "workflow_id": text["workflow_id"],
            "design_points": [{"x": min_x, "y": 0}, {"x": max_x, "y": 0}],
            "measured_points": [{"x": 0.0, "y": 0.0}, {"x": max_x - min_x, "y": 0.0}],
        }),
    );
    assert_eq!(plain["success"], true, "{plain}");
    assert!(plain["result"].get("draw_lab").is_none(), "{plain}");

    // 发送仍要 confirmed=true
    let new_id = record["workflow_id"].as_str().unwrap();
    let refused = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({"action": "confirm_send", "workflow_id": new_id}),
    );
    assert_eq!(refused["success"], false, "{refused}");
}

type Recorded<T> = std::sync::Arc<std::sync::Mutex<Vec<T>>>;

/// 点射用的假设备：`$$` 回报给定 `$30`，实时状态固定为 `status`；记录每一行与每个实时字节，
/// 写到 `fail_on` 那一行时模拟连接断开。
fn pointer_factory(
    status: &'static str,
    max_spindle: &'static str,
    fail_on: Option<&'static str>,
    lines: Recorded<String>,
    realtime: Recorded<laser_device::RealtimeCommand>,
) -> TransportFactory {
    struct PointerGrbl {
        status: &'static str,
        max_spindle: &'static str,
        fail_on: Option<&'static str>,
        pending: std::sync::Mutex<std::collections::VecDeque<String>>,
        lines: Recorded<String>,
        realtime: Recorded<laser_device::RealtimeCommand>,
    }
    impl laser_device::DeviceTransport for PointerGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            if self.fail_on == Some(line) {
                return Err("模拟连接断开".into());
            }
            self.lines.lock().unwrap().push(line.to_owned());
            let mut pending = self.pending.lock().unwrap();
            if line == "$$" {
                pending.extend([
                    format!("$30={}", self.max_spindle),
                    "$32=1".into(),
                    "ok".into(),
                ]);
            } else {
                pending.push_back("ok".into());
            }
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.pending
                    .lock()
                    .unwrap()
                    .push_back(format!("<{}|MPos:0,0,0|FS:0,0>", self.status));
            } else {
                self.realtime.lock().unwrap().push(command);
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.pending.lock().unwrap().pop_front())
        }
    }
    std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
        Ok(Box::new(PointerGrbl {
            status,
            max_spindle,
            fail_on,
            pending: Default::default(),
            lines: lines.clone(),
            realtime: realtime.clone(),
        }) as Box<dyn laser_device::DeviceTransport>)
    })
}

fn pointer_service(
    status: &'static str,
    max_spindle: &'static str,
    fail_on: Option<&'static str>,
) -> (
    ApiService,
    Recorded<String>,
    Recorded<laser_device::RealtimeCommand>,
) {
    let lines: Recorded<String> = Default::default();
    let realtime: Recorded<laser_device::RealtimeCommand> = Default::default();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_transport_factory(pointer_factory(
        status,
        max_spindle,
        fail_on,
        lines.clone(),
        realtime.clone(),
    ));
    (service, lines, realtime)
}

fn fire_body(extra: serde_json::Value) -> serde_json::Value {
    let mut body = connection();
    body["confirmed"] = serde_json::json!(true);
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

/// 写出的 G-code 行（去掉握手 / 配置读取的 `$` 指令）
fn gcode_lines(lines: &Recorded<String>) -> Vec<String> {
    lines
        .lock()
        .unwrap()
        .iter()
        .filter(|line| !line.starts_with('$'))
        .cloned()
        .collect()
}

/// 点射：恒定功率 M3（M4 原地不出光）、功率不超过 S10、停留不超过 200 ms，关光行紧跟其后，
/// 最后切回 G0；`$30` 非 1000 时 S 值按比例换算；完成后不再占用设备。
#[test]
fn pointer_fire_sends_a_capped_m3_dot_and_always_turns_the_laser_off() {
    let (service, lines, realtime) = pointer_service("Idle", "1000", None);
    let fired = post_json(
        &service,
        "/api/device/pointer-fire",
        &fire_body(serde_json::json!({})),
    );
    assert_eq!(fired["success"], true, "{fired}");
    assert!(
        fired["result"]["job_id"]
            .as_str()
            .unwrap()
            .starts_with("pointer-"),
        "{fired}"
    );
    assert_eq!(fired["result"]["power"], 10);
    assert_eq!(fired["result"]["duration_ms"], 100);
    assert_eq!(
        gcode_lines(&lines),
        [
            "M5 S0",
            "G1 F100",
            "M3 S10",
            "G4 P0.100",
            "M5 S0",
            "G0",
            "G4 P0"
        ],
        "默认取上限 S10（1%）、停留 100 ms"
    );
    assert!(realtime.lock().unwrap().is_empty(), "正常完成不发急停");
    assert_eq!(service.active_job_count(), 0);

    lines.lock().unwrap().clear();
    let low = post_json(
        &service,
        "/api/device/pointer-fire",
        &fire_body(serde_json::json!({"power": 3, "duration_ms": 200})),
    );
    assert_eq!(low["success"], true, "{low}");
    assert_eq!(
        gcode_lines(&lines),
        [
            "M5 S0",
            "G1 F100",
            "M3 S3",
            "G4 P0.200",
            "M5 S0",
            "G0",
            "G4 P0"
        ]
    );
    // 运动字段被忽略：点射不移动激光头
    lines.lock().unwrap().clear();
    let with_motion = post_json(
        &service,
        "/api/device/pointer-fire",
        &fire_body(serde_json::json!({"x": 50, "y": 50, "feed_rate": 3000})),
    );
    assert_eq!(with_motion["success"], true, "{with_motion}");
    assert!(
        gcode_lines(&lines)
            .iter()
            .all(|line| !line.contains('X') && !line.contains('Y')),
        "{:?}",
        lines.lock().unwrap()
    );

    // $30=255：S10 换算成 S3（round(2.55)），仍是 1%
    let (scaled, scaled_lines, _) = pointer_service("Idle", "255", None);
    let fired = post_json(
        &scaled,
        "/api/device/pointer-fire",
        &fire_body(serde_json::json!({})),
    );
    assert_eq!(fired["success"], true, "{fired}");
    let sent = gcode_lines(&scaled_lines);
    assert!(sent.contains(&"M3 S3".to_owned()), "{sent:?}");
    assert!(!sent.iter().any(|line| line.starts_with("M4")), "{sent:?}");
}

/// 越界、非整数、未确认都在打开设备前 400 拒绝；机器不在 Idle 时预检拒绝、不出光。
#[test]
fn pointer_fire_rejects_bad_params_unconfirmed_requests_and_busy_machines() {
    let (service, lines, _) = pointer_service("Idle", "1000", None);
    for extra in [
        serde_json::json!({"power": 11}),
        serde_json::json!({"power": 0}),
        serde_json::json!({"power": -1}),
        serde_json::json!({"power": 5.5}),
        serde_json::json!({"power": "10"}),
        serde_json::json!({"power": 4_294_967_306_u64}),
        serde_json::json!({"power": 1000}),
        serde_json::json!({"duration_ms": 201}),
        serde_json::json!({"duration_ms": 9}),
        serde_json::json!({"duration_ms": 100.5}),
        serde_json::json!({"duration_ms": 5000}),
    ] {
        let refused = post_json(
            &service,
            "/api/device/pointer-fire",
            &fire_body(extra.clone()),
        );
        assert_eq!(
            refused["detail"]["error_code"], "pointer_fire_params_invalid",
            "{extra}: {refused}"
        );
    }
    for confirmed in [
        serde_json::Value::Null,
        serde_json::json!(false),
        serde_json::json!("true"),
    ] {
        let mut body = fire_body(serde_json::json!({}));
        body["confirmed"] = confirmed.clone();
        let refused = post_json(&service, "/api/device/pointer-fire", &body);
        assert_eq!(
            refused["detail"]["error_code"], "confirmation_required",
            "{confirmed}: {refused}"
        );
    }
    assert!(lines.lock().unwrap().is_empty(), "拒绝时不得连接设备");

    for status in ["Alarm", "Jog", "Run", "Hold:0", "Door:0"] {
        let (busy, busy_lines, _) = pointer_service(status, "1000", None);
        let refused = post_json(
            &busy,
            "/api/device/pointer-fire",
            &fire_body(serde_json::json!({})),
        );
        assert_eq!(
            refused["detail"]["error_code"], "device_preflight_failed",
            "{status}: {refused}"
        );
        assert!(
            !busy_lines
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.starts_with("M3")),
            "{status}: 不在 Idle 不能出光"
        );
    }
}

/// 写出过程中断开：补发实时 0x18，任务转为“待确认停止”，急停仍能找到它。
#[test]
fn pointer_fire_failure_sends_realtime_estop_and_keeps_a_stop_target() {
    let (service, lines, realtime) = pointer_service("Idle", "1000", Some("G4 P0.100"));
    let failed = post_json(
        &service,
        "/api/device/pointer-fire",
        &fire_body(serde_json::json!({})),
    );
    assert_eq!(
        failed["detail"]["error_code"], "pointer_fire_failed",
        "{failed}"
    );
    assert!(
        gcode_lines(&lines).contains(&"M3 S10".to_owned()),
        "{:?}",
        lines.lock().unwrap()
    );
    assert_eq!(
        *realtime.lock().unwrap(),
        vec![laser_device::RealtimeCommand::EmergencyStop],
        "出错后必须补发 0x18 关光"
    );
    assert_eq!(service.active_job_count(), 0);
    let targets = parse_json_response(
        &service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(
        targets["result"]["jobs"][0]["state"], "unknown",
        "{targets}"
    );
    assert_eq!(
        targets["result"]["jobs"][0]["job_id"], failed["detail"]["job_id"],
        "{targets}"
    );
}

/// 点射运行中登记为活动作业：顶栏急停按任务编号能停止它（与走边框一致）。
#[test]
fn pointer_fire_is_an_active_job_that_emergency_stop_can_find() {
    let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let service = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(gated_transport_factory(gate.clone())),
    );
    let runner = {
        let service = service.clone();
        std::thread::spawn(move || {
            post_json(
                &service,
                "/api/device/pointer-fire",
                &fire_body(serde_json::json!({})),
            )
        })
    };
    let targets_request = b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let start = std::time::Instant::now();
    let job_id = loop {
        let targets = parse_json_response(&service.handle_http(targets_request));
        if let Some(id) = targets["result"]["jobs"][0]["job_id"].as_str() {
            break id.to_owned();
        }
        if runner.is_finished() || start.elapsed() > std::time::Duration::from_secs(5) {
            gate.store(true, std::sync::atomic::Ordering::SeqCst);
            panic!("点射没有登记为活动作业: {}", runner.join().unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(job_id.starts_with("pointer-"), "{job_id}");
    let busy = post_json(
        &service,
        "/api/device/pointer-fire",
        &fire_body(serde_json::json!({})),
    );
    assert_eq!(busy["detail"]["error_code"], "device_busy", "{busy}");
    let stop = post_json(
        &service,
        "/api/safe-action",
        &serde_json::json!({"action": "emergency_stop", "confirmed": true, "job_id": job_id}),
    );
    assert_eq!(stop["success"], true, "{stop}");
    gate.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = runner.join().unwrap();
    assert_eq!(
        result["detail"]["error_code"], "pointer_fire_cancelled",
        "{result}"
    );
    assert_eq!(service.active_job_count(), 0);
}

/// 桌面版（启用了桌面会话）里不带会话的本机回环进程不能点射，哪怕严格模式关着；桌面会话照常可用。
#[test]
fn pointer_fire_rejects_sessionless_loopback_once_a_desktop_session_exists() {
    let (service, lines, _) = pointer_service("Idle", "1000", None);
    service.enable_desktop_session().unwrap();
    let body = fire_body(serde_json::json!({}));
    let (status, refused) = post_with_ctx(
        &service,
        "/api/device/pointer-fire",
        &body,
        laser_contracts::CallerContext::Loopback,
    );
    assert_eq!(status, 403, "{refused}");
    assert_eq!(
        refused["detail"]["error_code"], "desktop_session_required",
        "{refused}"
    );
    assert!(lines.lock().unwrap().is_empty(), "拒绝时不得连接设备");
    let (status, fired) = post_with_ctx(
        &service,
        "/api/device/pointer-fire",
        &body,
        laser_contracts::CallerContext::Desktop,
    );
    assert_eq!(status, 200, "{fired}");
    assert!(gcode_lines(&lines).contains(&"M3 S10".to_owned()));
}

/// 严格模式下只有桌面会话能点射（不创建审批）；语音、小智 MCP、网关、审批重放不论开关一律拒绝；
/// `laser_safe_action_tool` 与 MCP 工具清单里都没有点射。
#[test]
fn pointer_fire_is_desktop_only_in_strict_mode_and_unreachable_from_voice_or_mcp() {
    for strict in [false, true] {
        let lines: Recorded<String> = Default::default();
        let realtime: Recorded<laser_device::RealtimeCommand> = Default::default();
        let db = std::sync::Arc::new(std::sync::Mutex::new(
            laser_runtime::RuntimeDb::open_memory().unwrap(),
        ));
        let service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_strict_mode(strict)
        .with_runtime_db(db.clone())
        .with_approval_service(std::sync::Arc::new(approval::ApprovalService::new(
            db.clone(),
        )))
        .with_transport_factory(pointer_factory(
            "Idle",
            "1000",
            None,
            lines.clone(),
            realtime,
        ));
        // 已允许、会话有效的语音设备也不行（不是因为会话失效才被拒）
        {
            let db = db.lock().unwrap();
            db.voice_device_seen("aa:bb:cc:dd:ee:ff", "test-client", "", "", "", 0.0)
                .unwrap();
            db.set_voice_device_status(
                "aa:bb:cc:dd:ee:ff",
                laser_runtime::VOICE_DEVICE_ALLOWED,
                0.0,
            )
            .unwrap();
        }
        service
            .register_voice_session("aa:bb:cc:dd:ee:ff", "test-client", "sess-1")
            .unwrap();
        let body = fire_body(serde_json::json!({}));
        for ctx in [
            laser_contracts::CallerContext::McpBridge,
            laser_contracts::CallerContext::Voice {
                device_id: "aa:bb:cc:dd:ee:ff".into(),
                session_id: "sess-1".into(),
            },
            laser_contracts::CallerContext::Gateway {
                session_id: "gw-1".into(),
            },
            laser_contracts::CallerContext::Approval {
                approval_id: "apr-1".into(),
            },
        ] {
            let (status, refused) =
                post_with_ctx(&service, "/api/device/pointer-fire", &body, ctx.clone());
            assert_eq!(status, 403, "{ctx:?}: {refused}");
            assert_eq!(
                refused["detail"]["error_code"], "pointer_fire_not_allowed",
                "{ctx:?}: {refused}"
            );
        }
        let lan = laser_contracts::CallerContext::Lan {
            pairing_id: "pair-1".into(),
        };
        for ctx in [laser_contracts::CallerContext::Loopback, lan] {
            let (status, result) =
                post_with_ctx(&service, "/api/device/pointer-fire", &body, ctx.clone());
            if strict {
                assert_eq!(status, 403, "{ctx:?}: {result}");
                assert_eq!(
                    result["detail"]["error_code"], "strict_mode_blocked",
                    "{ctx:?}: {result}"
                );
            } else {
                assert_eq!(status, 200, "{ctx:?}: {result}");
            }
        }
        if strict {
            assert!(
                !gcode_lines(&lines)
                    .iter()
                    .any(|line| line.starts_with("M3")),
                "严格模式下非桌面来源不得出光"
            );
        }
        let (status, desktop) = post_with_ctx(
            &service,
            "/api/device/pointer-fire",
            &body,
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(status, 200, "strict={strict}: {desktop}");
        assert!(
            service
                .approvals()
                .unwrap()
                .list(None, 10)
                .unwrap()
                .is_empty(),
            "点射不进审批队列"
        );

        // laser_safe_action_tool 的动作里没有点射
        lines.lock().unwrap().clear();
        let mut via_safe_action = connection();
        via_safe_action["action"] = serde_json::json!("pointer_fire");
        via_safe_action["confirmed"] = serde_json::json!(true);
        via_safe_action["host"] = serde_json::json!("127.0.0.1");
        let (_, refused) = post_with_ctx(
            &service,
            "/api/safe-action",
            &via_safe_action,
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(refused["success"], false, "{refused}");
        assert!(
            !gcode_lines(&lines)
                .iter()
                .any(|line| line.starts_with("M3")),
            "{:?}",
            lines.lock().unwrap()
        );
    }
    let tools = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/mcp_tools.json"),
    )
    .unwrap();
    assert!(!tools.contains("pointer"), "MCP 工具清单不得出现点射");
}
