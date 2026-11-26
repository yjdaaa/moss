//! 材料测试（轴参数、风险顺序、标注）、线距测试与材料库表面条目 / 条目标题（2026-11-16，对齐 LightBurn）。
use super::*;

fn post_status(
    service: &ApiService,
    route: &str,
    body: &serde_json::Value,
) -> (u16, serde_json::Value) {
    let body = body.to_string();
    let request = format!(
        "POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let raw = service.handle_http(request.as_bytes());
    let status = String::from_utf8_lossy(&raw[..12])
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap();
    (status, parse_json_response(&raw))
}

fn lab_service(temp: &tempfile::TempDir) -> ApiService {
    let gen_dir = temp.path().join("gen");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let mut settings = test_loopback_settings();
    settings.material_params_file = Some(temp.path().join("materials.json").display().to_string());
    ApiService::new(settings, laser_contracts::builtin_material_library())
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir])
}

fn gcode_of(temp: &tempfile::TempDir, record: &serde_json::Value) -> String {
    let name = record["gcode_file"].as_str().unwrap();
    std::fs::read_to_string(temp.path().join("gen").join(name)).unwrap()
}

#[test]
fn default_preview_keeps_power_by_speed_axes_and_runs_lowest_risk_first() {
    let temp = tempfile::tempdir().unwrap();
    let service = lab_service(&temp);
    let res = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave", "rows": 3, "columns": 3,
            "power_min": 100, "power_max": 500, "speed_min": 600, "speed_max": 1800
        }),
    );
    assert_eq!(res["success"], true, "{res}");
    let record = &res["result"];
    assert_eq!(record["test_type"], "matrix");
    assert_eq!(
        (record["x_axis"].as_str(), record["y_axis"].as_str()),
        (Some("power"), Some("speed"))
    );
    let legacy = laser_core::material::calibration_matrix(3, 3, 100, 500, 600, 1800, 1).unwrap();
    let cells = record["cells"].as_array().unwrap();
    for (cell, old) in cells.iter().zip(&legacy) {
        assert_eq!(cell["id"], old.id.as_str());
        assert_eq!(cell["power"], old.power);
        assert_eq!(cell["speed"], old.speed);
    }
    assert_eq!(record["power_min"], 100);
    assert_eq!(record["speed_max"], 1800);
    // 风险最低：最快的一行（C）里功率最低的那格先刻
    assert_eq!(record["run_order"], "risk");
    assert_eq!(record["run_sequence"][0], 7, "{record}");
    assert_eq!(cells[6]["run_index"], 1);
    assert_eq!(cells[2]["run_index"], 9, "A3 = slowest + strongest last");
    // 填充线距默认取材料推荐（与实际雕刻一致），选格时随之写回
    assert_eq!(cells[0]["line_interval_mm"], 0.1);
    assert_eq!(record["labels"]["enabled"], true);
    assert_eq!(cells[0]["x_label"], "10");
    assert!(
        record["width_mm"].as_f64().unwrap() > 44.0,
        "labels widen the job"
    );
    let gcode = gcode_of(&temp, record);
    assert!(gcode.contains("; Labels: S100 F1800"), "{}", &gcode[..400]);
    assert!(gcode.find("; Cell C1:").unwrap() < gcode.find("; Cell A3:").unwrap());
}

#[test]
fn grid_order_without_labels_reproduces_the_legacy_cut_matrix_byte_for_byte() {
    let temp = tempfile::tempdir().unwrap();
    let service = lab_service(&temp);
    let res = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木", "thickness_mm": 3, "laser_mode": "cut", "rows": 2, "columns": 3,
            "power_min": 400, "power_max": 800, "speed_min": 200, "speed_max": 400, "passes": 2,
            "run_order": "grid", "labels": false
        }),
    );
    assert_eq!(res["success"], true, "{res}");
    let record = &res["result"];
    let legacy = laser_core::material::calibration_matrix(2, 3, 400, 800, 200, 400, 2).unwrap();
    let expected =
        laser_core::material::build_calibration_gcode("椴木", "cut_contour", &legacy).unwrap();
    assert_eq!(gcode_of(&temp, record), expected);
    assert_eq!(
        (record["width_mm"].as_f64(), record["height_mm"].as_f64()),
        (Some(44.0), Some(28.0))
    );
    assert_eq!(record["cells"][1]["x_mm"], 16.0);
    assert!(
        record["cells"][0]["line_interval_mm"].is_null(),
        "cut has no scan interval"
    );
}

#[test]
fn interval_and_passes_axes_write_the_chosen_interval_and_passes_back() {
    let temp = tempfile::tempdir().unwrap();
    let service = lab_service(&temp);
    let res = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave", "rows": 3, "columns": 3,
            "x_axis": "interval", "y_axis": "passes", "interval_min_mm": 0.06, "interval_max_mm": 0.12,
            "passes_min": 1, "passes_max": 3, "power": 350, "speed": 1600
        }),
    );
    assert_eq!(res["success"], true, "{res}");
    let record = &res["result"];
    assert_eq!(record["axes"]["x"]["param"], "interval");
    assert_eq!(record["axes"]["y"]["unit"], "passes");
    let cells = record["cells"].as_array().unwrap();
    assert_eq!(cells[2]["line_interval_mm"], 0.12);
    assert_eq!(cells[6]["passes"], 3);
    assert!(
        cells
            .iter()
            .all(|c| c["power"] == 350 && c["speed"] == 1600)
    );
    assert_eq!(
        (record["power_min"].as_i64(), record["power_max"].as_i64()),
        (Some(350), Some(350))
    );
    // 风险最低：最宽线距 + 1 遍（A3）先刻
    assert_eq!(record["run_sequence"][0], 3);
    assert!(gcode_of(&temp, record).contains("interval=0.120"));

    let selected = post_json(
        &service,
        "/api/material-lab/select-cell",
        &serde_json::json!({
            "calibration_id": record["calibration_id"], "cell_number": 8, "title": "细线深雕"
        }),
    );
    assert_eq!(selected["success"], true, "{selected}");
    assert_eq!(
        selected["result"]["selected_cell"]["line_interval_mm"],
        0.09
    );
    assert_eq!(selected["result"]["selected_cell"]["passes"], 3);
    let raster = &selected["result"]["materials"]["materials"]["椴木"]["thicknesses"]["3"]["engrave"]
        ["raster"];
    assert_eq!(raster["pixel_size_mm"], 0.09, "{raster}");
    assert_eq!(raster["passes"], 3);
    assert_eq!(raster["title"], "细线深雕");
    assert_eq!(raster["source"], "calibration");
}

#[test]
fn interval_test_generates_one_labelled_row_and_saves_the_interval() {
    let temp = tempfile::tempdir().unwrap();
    let service = lab_service(&temp);
    // 默认 9 格线距测试宽 104 mm：未握手且未传 work_area_* 时按 100 mm 默认行程拦截；传入 ≥104 mm 幅面可正常生成
    let default_blocked = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave", "engraving_mode": "raster",
            "test_type": "interval"
        }),
    );
    assert_eq!(default_blocked["success"], false, "{default_blocked}");
    let res = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave", "engraving_mode": "raster",
            "test_type": "interval", "work_area_width_mm": 120
        }),
    );
    assert_eq!(res["success"], true, "{res}");
    let record = &res["result"];
    assert_eq!(record["test_type"], "interval");
    assert_eq!(
        (record["rows"].as_i64(), record["columns"].as_i64()),
        (Some(1), Some(9))
    );
    let cells = record["cells"].as_array().unwrap();
    assert_eq!(cells[0]["line_interval_mm"], 0.08);
    assert_eq!(cells[8]["line_interval_mm"], 0.16);
    assert_eq!(cells[0]["size_mm"], 8.0);
    assert_eq!(cells[2]["x_label"], "0.10");
    assert_eq!(record["run_sequence"][0], 9, "widest interval first");
    assert!(gcode_of(&temp, record).contains("; Labels:"));

    let selected = post_json(
        &service,
        "/api/material-lab/select-cell",
        &serde_json::json!({"calibration_id": record["calibration_id"], "cell_number": 4}),
    );
    assert_eq!(selected["success"], true, "{selected}");
    let raster = &selected["result"]["materials"]["materials"]["椴木"]["thicknesses"]["3"]["engrave"]
        ["raster"];
    assert_eq!(raster["pixel_size_mm"], 0.11, "{raster}");
    assert_eq!(raster["source"], "interval_test");
    assert!(raster["notes"].as_str().unwrap().contains("线距测试"));
}

#[test]
fn invalid_material_test_inputs_are_rejected_with_400() {
    let temp = tempfile::tempdir().unwrap();
    let service = lab_service(&temp);
    let base = serde_json::json!({"material": "椴木", "thickness_mm": 3, "laser_mode": "engrave"});
    let with = |extra: serde_json::Value| {
        let mut body = base.clone();
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    for (extra, code) in [
        (serde_json::json!({"x_axis": "focus"}), Some("invalid_axis")),
        (
            serde_json::json!({"x_axis": "speed", "y_axis": "speed"}),
            Some("invalid_axis"),
        ),
        (
            serde_json::json!({"x_axis": "interval", "laser_mode": "cut"}),
            Some("axis_not_supported"),
        ),
        (
            serde_json::json!({"y_axis": "interval", "engraving_mode": "outline"}),
            Some("axis_not_supported"),
        ),
        (
            serde_json::json!({"test_type": "interval", "laser_mode": "cut"}),
            Some("axis_not_supported"),
        ),
        (
            serde_json::json!({"test_type": "focus"}),
            Some("invalid_test_type"),
        ),
        (
            serde_json::json!({"test_type": "interval", "rows": 2}),
            None,
        ),
        (
            serde_json::json!({"test_type": "interval", "columns": 1}),
            None,
        ),
        (
            serde_json::json!({"x_axis": "interval", "interval_min_mm": 0.2, "interval_max_mm": 0.1}),
            None,
        ),
        (
            serde_json::json!({"x_axis": "interval", "interval_min_mm": 0.001}),
            None,
        ),
        (
            serde_json::json!({"x_axis": "passes", "passes_min": 1, "passes_max": 30}),
            None,
        ),
        (serde_json::json!({"run_order": "random"}), None),
        (serde_json::json!({"label_height_mm": 50}), None),
        (serde_json::json!({"label_power": 5000}), None),
        (serde_json::json!({"line_interval_mm": 0}), None),
        (serde_json::json!({"cell_size_mm": 0.2}), None),
        (
            serde_json::json!({"surface": true, "laser_mode": "cut"}),
            None,
        ),
    ] {
        let (status, body) =
            post_status(&service, "/api/material-lab/preview", &with(extra.clone()));
        assert_eq!(status, 400, "{extra} -> {body}");
        assert_eq!(body["success"], false);
        if let Some(code) = code {
            assert_eq!(body["detail"]["error_code"], code, "{extra} -> {body}");
        }
    }
}

#[test]
fn surface_entries_and_titles_round_trip_through_save_recommend_and_delete() {
    let temp = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    let service = lab_service(&temp).with_runtime_db(db.clone());
    let save = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "save", "material": "椴木", "surface": true, "laser_mode": "engrave",
            "engraving_mode": "raster", "laser_max_power": 260, "feed_rate": 2400,
            "pixel_size_mm": 0.08, "title": "  浅色照片  "
        }),
    );
    assert_eq!(save["success"], true, "{save}");
    let surface = &save["result"]["materials"]["materials"]["椴木"]["thicknesses"]["surface"];
    assert_eq!(
        surface["engrave"]["raster"]["title"], "浅色照片",
        "{surface}"
    );
    // SQLite 镜像也保留标题（存在 extra 列，无需迁移）
    let exported = db.lock().unwrap().export_material_library().unwrap();
    let cell = exported.materials["椴木"].thicknesses["surface"]
        .engrave
        .as_ref()
        .unwrap()
        .raster
        .clone()
        .unwrap();
    assert_eq!(cell.title(), Some("浅色照片"));

    // 椴木 3 mm 已有光栅参数 → 仍用同厚度；8 mm 没有 → 表面条目，而不是最近厚度
    let rec = |thickness: &str| {
        parse_json_response(&service.handle_http(
            format!("GET /api/ui/material-recommendation?material=%E6%A4%B4%E6%9C%A8&thickness_mm={thickness}&laser_mode=engrave&engraving_mode=raster HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
        ))
    };
    assert_eq!(rec("3")["result"]["match"], "exact");
    let at_8 = rec("8");
    assert_eq!(at_8["result"]["match"], "surface", "{at_8}");
    assert_eq!(at_8["result"]["laser_max_power"], 260);
    assert_eq!(at_8["result"]["title"], "浅色照片");

    let options = parse_json_response(
        &service.handle_http(b"GET /api/ui/material-options HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    let basswood = options["result"]["materials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "椴木")
        .unwrap()
        .clone();
    assert_eq!(basswood["surface"], true);
    assert!(
        basswood["thicknesses"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t.is_number())
    );

    // 切割不能存进表面条目；导入时表面条目带 cut 同样拒绝；标题超长拒绝
    let (status, _) = post_status(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "save", "material": "椴木", "surface": true, "laser_mode": "cut",
            "laser_max_power": 800, "feed_rate": 300}),
    );
    assert_eq!(status, 400);
    let (status, _) = post_status(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "import", "import_mode": "merge", "library_json": {
            "version": 1, "materials": {"新板": {"thicknesses": {"surface": {"cut": {
                "laser_max_power": 800, "feed_rate": 300, "travel_rate": 3000, "threshold": -1,
                "passes": 1, "pixel_size_mm": 0.1, "laser_min_power": 0}}}}}}}),
    );
    assert_eq!(status, 400);
    let (status, _) = post_status(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "save", "material": "椴木", "thickness_mm": 3,
            "laser_max_power": 300, "feed_rate": 1500, "title": "长".repeat(61)}),
    );
    assert_eq!(status, 400);

    // 旧库 JSON（无 surface / title）照常导入
    let legacy = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "import", "import_mode": "merge", "library_json": {
            "version": 1, "materials": {"旧板": {"thicknesses": {"2": {"engrave": {"raster": {
                "laser_max_power": 300, "feed_rate": 1500, "travel_rate": 3000, "threshold": -1,
                "passes": 1, "pixel_size_mm": 0.1, "laser_min_power": 0}}}}}}}}),
    );
    assert_eq!(legacy["success"], true, "{legacy}");

    // 删表面条目只删它，不误删整个材料
    let deleted = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "delete", "material": "椴木", "thickness_mm": "surface", "confirmed": true}),
    );
    assert_eq!(deleted["success"], true, "{deleted}");
    let basswood = &deleted["result"]["materials"]["materials"]["椴木"];
    assert!(basswood["thicknesses"]["surface"].is_null());
    assert!(basswood["thicknesses"]["3"].is_object());
}
