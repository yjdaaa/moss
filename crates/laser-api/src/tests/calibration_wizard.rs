//! r40（2026-11-16）：首次标定向导的测试图案、随材料保存的填充 / 切割设置与工时自校准。
use super::*;
use crate::time_calibration::{
    JobTiming, MAX_SAMPLES, MAX_TARGETS, SETTINGS_KEY, apply_to_estimate, factor_from,
    record_job_timing, record_ratio, sample_ratio,
};

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

fn get(service: &ApiService, path: &str) -> serde_json::Value {
    parse_json_response(
        &service.handle_http(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()),
    )
}

fn wizard_service(temp: &tempfile::TempDir) -> ApiService {
    let gen_dir = temp.path().join("gen");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(&gen_dir).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    let mut settings = test_loopback_settings();
    settings.material_params_file = Some(temp.path().join("materials.json").display().to_string());
    ApiService::new(settings, laser_contracts::builtin_material_library())
        .with_runtime_path(runtime)
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir])
}

const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/fonts/geometry.ttf"
);

fn text_body(extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({"text": "AB", "font_path": FONT, "material": "椴木",
        "thickness_mm": 3, "font_size_mm": 10, "text_fill": "fill"});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

const RASTER_REC: &str = "/api/ui/material-recommendation?material=%E6%A4%B4%E6%9C%A8&thickness_mm=3&laser_mode=engrave&engraving_mode=raster";
const CUT_REC: &str =
    "/api/ui/material-recommendation?material=%E6%A4%B4%E6%9C%A8&thickness_mm=3&laser_mode=cut";

#[test]
fn layer_options_are_saved_with_the_material_entry_validated_and_kept_on_param_edits() {
    let temp = tempfile::tempdir().unwrap();
    let service = wizard_service(&temp);
    // 旧条目没有图层设置：推荐里不带这两个键
    let before = get(&service, RASTER_REC);
    assert!(before["result"].get("fill_options").is_none(), "{before}");

    let saved = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "save_layer_options", "material": "椴木", "thickness_mm": 3,
            "laser_mode": "engrave", "engraving_mode": "raster",
            "fill_options": {"angle_deg": 45, "overscan_mm": 3, "bidirectional": false, "cross_hatch": null}}),
    );
    assert_eq!(saved["success"], true, "{saved}");
    assert_eq!(saved["result"]["thickness_key"], "3");
    assert_eq!(
        saved["result"]["fill_options"],
        serde_json::json!({"angle_deg": 45, "overscan_mm": 3, "bidirectional": false})
    );
    let rec = get(&service, RASTER_REC);
    assert_eq!(rec["result"]["fill_options"]["angle_deg"], 45, "{rec}");
    // 材料库文件里存在参数格的 extra 里（旧格式兼容，无需迁移）
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(temp.path().join("materials.json")).unwrap())
            .unwrap();
    assert_eq!(
        file["materials"]["椴木"]["thicknesses"]["3"]["engrave"]["raster"]["fill_options"]["overscan_mm"],
        3
    );

    // 改功率速度（不带 fill_options）不会丢掉图层设置；显式 null 才清除
    let edited = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "save", "material": "椴木", "thickness_mm": 3,
            "laser_mode": "engrave", "engraving_mode": "raster", "laser_max_power": 333, "feed_rate": 1600}),
    );
    assert_eq!(edited["success"], true, "{edited}");
    let rec = get(&service, RASTER_REC);
    assert_eq!(rec["result"]["laser_max_power"], 333);
    assert_eq!(rec["result"]["fill_options"]["angle_deg"], 45, "{rec}");

    // 机器级设置、错放的切割设置、超范围的值都拒绝
    for (body, why) in [
        (
            serde_json::json!({"fill_options": {"scan_offset_mm": 0.1}}),
            "scan offset is per machine",
        ),
        (
            serde_json::json!({"fill_options": {"acceleration_mm_s2": 800}}),
            "acceleration is per machine",
        ),
        (
            serde_json::json!({"fill_options": {"overscan_mm": 50}}),
            "overscan range",
        ),
        (
            serde_json::json!({"cut_options": {"kerf_mm": 0.1}}),
            "cut options on an engrave entry",
        ),
        (serde_json::json!({"fill_options": "fast"}), "not an object"),
    ] {
        let mut request = serde_json::json!({"action": "save_layer_options", "material": "椴木",
            "thickness_mm": 3, "laser_mode": "engrave", "engraving_mode": "raster"});
        request
            .as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        let (status, response) = post_status(&service, "/api/material-lab/manage", &request);
        assert_eq!(status, 400, "{why}: {response}");
    }

    // 切割条目：merge=true 只并入给出的键
    for patch in [
        serde_json::json!({"kerf_mm": 0.1}),
        serde_json::json!({"tab_count": 2}),
    ] {
        let response = post_json(
            &service,
            "/api/material-lab/manage",
            &serde_json::json!({"action": "save_layer_options", "material": "basswood", "thickness_mm": 3,
                "laser_mode": "cut", "cut_options": patch, "merge": true}),
        );
        assert_eq!(response["success"], true, "{response}");
    }
    let cut = get(&service, CUT_REC);
    assert_eq!(
        cut["result"]["cut_options"],
        serde_json::json!({"kerf_mm": 0.1, "tab_count": 2}),
        "{cut}"
    );

    // 条目不存在时提示先保存参数
    let (status, missing) = post_status(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "save_layer_options", "material": "椴木", "thickness_mm": 99,
            "laser_mode": "cut", "cut_options": {"kerf_mm": 0.1}}),
    );
    assert_eq!(status, 400);
    assert_eq!(
        missing["detail"]["error_code"], "material_entry_missing",
        "{missing}"
    );

    // 导入的库同样校验图层设置
    let (status, _) = post_status(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({"action": "import", "import_mode": "merge", "library_json": {
            "version": 1, "materials": {"新板": {"thicknesses": {"2": {"engrave": {"raster": {
                "laser_max_power": 300, "feed_rate": 1500, "travel_rate": 3000, "threshold": -1,
                "passes": 1, "pixel_size_mm": 0.1, "laser_min_power": 0,
                "fill_options": {"scan_offset_table": [[1000, 0.1]]}}}}}}}}}),
    );
    assert_eq!(status, 400);
}

#[test]
fn generation_falls_back_to_material_layer_defaults_and_echoes_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let service = wizard_service(&temp);
    for (mode, extra) in [
        (
            serde_json::json!({"laser_mode": "engrave", "engraving_mode": "raster"}),
            serde_json::json!({"fill_options": {"angle_deg": 90, "overscan_mm": 0, "bidirectional": false}}),
        ),
        (
            serde_json::json!({"laser_mode": "cut"}),
            serde_json::json!({"cut_options": {"kerf_mm": 0.1, "tab_count": 2}}),
        ),
    ] {
        let mut body = serde_json::json!({"action": "save_layer_options", "material": "椴木", "thickness_mm": 3});
        let map = body.as_object_mut().unwrap();
        map.extend(mode.as_object().unwrap().clone());
        map.extend(extra.as_object().unwrap().clone());
        let saved = post_json(&service, "/api/material-lab/manage", &body);
        assert_eq!(saved["success"], true, "{saved}");
    }

    // 请求没带 fill_options：全部取材料库
    let plain = post_json(
        &service,
        "/api/text-task/generate",
        &text_body(serde_json::json!({})),
    );
    assert_eq!(plain["success"], true, "{plain}");
    let fill = &plain["result"]["artifacts"]["fill_options"];
    assert_eq!(fill["angle_deg"], 90.0, "{fill}");
    assert_eq!(fill["overscan_mm"], 0.0);
    assert_eq!(fill["overscan_auto"], false);
    assert_eq!(fill["bidirectional"], false);
    assert_eq!(
        fill["from_material"],
        serde_json::json!(["angle_deg", "bidirectional", "overscan_mm"])
    );

    // 请求写了的键优先，其余仍取材料库
    let mixed = post_json(
        &service,
        "/api/text-task/generate",
        &text_body(serde_json::json!({"fill_options": {"angle_deg": 0, "overscan_mm": "auto"}})),
    );
    let fill = &mixed["result"]["artifacts"]["fill_options"];
    assert_eq!(fill["angle_deg"], 0.0, "{fill}");
    assert_eq!(fill["overscan_auto"], true);
    assert_eq!(fill["bidirectional"], false);
    assert_eq!(fill["from_material"], serde_json::json!(["bidirectional"]));

    // 外框切割：切缝与桥位取材料库；请求改用桥位间距时材料库的桥位个数不再拼进来
    let cut_body = |extra: serde_json::Value| {
        text_body(
            serde_json::json!({"laser_mode": "cut", "engraving_mode": "cut", "text_fill": null}),
        )
        .as_object()
        .cloned()
        .map(|mut map| {
            map.remove("text_fill");
            map.extend(extra.as_object().unwrap().clone());
            serde_json::Value::Object(map)
        })
        .unwrap()
    };
    let cut = post_json(
        &service,
        "/api/text-task/generate",
        &cut_body(serde_json::json!({})),
    );
    assert_eq!(cut["success"], true, "{cut}");
    let options = &cut["result"]["artifacts"]["cut_options"];
    assert_eq!(options["kerf_mm"], 0.1, "{options}");
    assert_eq!(options["tab_mode"], "count");
    assert_eq!(options["tab_count"], 2);
    assert_eq!(
        options["from_material"],
        serde_json::json!(["kerf_mm", "tab_count"])
    );
    let spaced = post_json(
        &service,
        "/api/text-task/generate",
        &cut_body(serde_json::json!({"cut_options": {"tab_spacing_mm": 20}})),
    );
    assert_eq!(spaced["success"], true, "{spaced}");
    let options = &spaced["result"]["artifacts"]["cut_options"];
    assert_eq!(options["tab_mode"], "spacing", "{options}");
    assert_eq!(options["kerf_mm"], 0.1);
    assert_eq!(options["from_material"], serde_json::json!(["kerf_mm"]));

    // 描边雕刻不会拿到切割条目的设置
    let outline = post_json(
        &service,
        "/api/text-task/generate",
        &text_body(serde_json::json!({"text_fill": "outline"})),
    );
    assert_eq!(outline["success"], true, "{outline}");
    assert!(outline["result"]["artifacts"]["cut_options"].is_null());
}

#[test]
fn wizard_patterns_are_generated_as_sendable_calibration_records_but_not_selectable() {
    let temp = tempfile::tempdir().unwrap();
    let service = wizard_service(&temp);
    let gcode_of = |record: &serde_json::Value| {
        std::fs::read_to_string(
            temp.path()
                .join("gen")
                .join(record["gcode_file"].as_str().unwrap()),
        )
        .unwrap()
    };

    let scan = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({"material": "椴木", "thickness_mm": 3, "test_type": "scan_offset",
            "speeds": [1000, 2000, 3000]}),
    );
    assert_eq!(scan["success"], true, "{scan}");
    let record = &scan["result"];
    assert_eq!(record["test_type"], "scan_offset");
    assert_eq!(record["wizard_pattern"], true);
    assert_eq!(record["next_actions"], serde_json::json!(["send"]));
    let cells = record["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 3);
    assert_eq!(cells[2]["speed"], 3000);
    assert_eq!(cells[0]["scan_offset_mm"], 0.0);
    // 速度越快自动过扫描越长
    assert!(cells[2]["overscan_mm"].as_f64() > cells[0]["overscan_mm"].as_f64());
    let gcode = gcode_of(record);
    use sha2::Digest;
    assert_eq!(
        record["gcode_sha256"],
        hex::encode(sha2::Sha256::digest(gcode.as_bytes()))
    );
    assert!(gcode.contains("F3000"));
    // 这种测试不能按格写回材料库
    let (status, rejected) = post_status(
        &service,
        "/api/material-lab/select-cell",
        &serde_json::json!({"calibration_id": record["calibration_id"], "cell_number": 1}),
    );
    assert_eq!(status, 400);
    assert_eq!(rejected["detail"]["error_code"], "pattern_not_selectable");

    // 过扫描检查：左块不过扫描、右块按当前设置（请求的固定 2.5 mm）
    let over = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({"material": "椴木", "thickness_mm": 3, "test_type": "overscan",
            "fill_options": {"overscan_mm": 2.5, "scan_offset_mm": 0.05}}),
    );
    assert_eq!(over["success"], true, "{over}");
    let cells = over["result"]["cells"].as_array().unwrap();
    assert_eq!(cells[0]["overscan_mm"], 0.0);
    assert_eq!(cells[1]["overscan_mm"], 2.5);
    assert_eq!(cells[1]["scan_offset_mm"], 0.05);
    assert_eq!(over["result"]["pattern"]["overscan_auto"], false);

    // 切缝：按材料库切割参数切一个 20 mm 方块，不做补偿
    let kerf = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({"material": "椴木", "thickness_mm": 3, "test_type": "kerf"}),
    );
    assert_eq!(kerf["success"], true, "{kerf}");
    let record = &kerf["result"];
    assert_eq!(record["laser_mode"], "cut");
    assert_eq!(record["square_mm"], 20.0);
    assert!(gcode_of(record).contains("G1 X20.000 Y20.000"));
    let (status, _) = post_status(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({"material": "椴木", "thickness_mm": 99, "test_type": "kerf"}),
    );
    assert_eq!(status, 400, "no cut entry for that thickness");
    let (status, _) = post_status(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({"material": "椴木", "thickness_mm": 3, "test_type": "scan_offset",
            "speeds": [0]}),
    );
    assert_eq!(status, 400);
}

#[test]
fn time_calibration_math_and_exclusion_rules() {
    let ok = JobTiming {
        status: "completed",
        resumed: false,
        sd_offline: false,
        actual_seconds: 120.0,
        estimated_seconds: Some(100.0),
    };
    assert!((sample_ratio(&ok).unwrap() - 1.2).abs() < 1e-12);
    for (timing, reason) in [
        (
            JobTiming {
                status: "cancelled",
                ..ok.clone()
            },
            "not_completed",
        ),
        (
            JobTiming {
                status: "failed",
                ..ok.clone()
            },
            "not_completed",
        ),
        (
            JobTiming {
                resumed: true,
                ..ok.clone()
            },
            "resumed",
        ),
        (
            JobTiming {
                sd_offline: true,
                ..ok.clone()
            },
            "sd_offline",
        ),
        (
            JobTiming {
                actual_seconds: 19.9,
                ..ok.clone()
            },
            "too_short",
        ),
        (
            JobTiming {
                estimated_seconds: None,
                ..ok.clone()
            },
            "no_estimate",
        ),
        (
            JobTiming {
                actual_seconds: 1000.0,
                ..ok.clone()
            },
            "outlier",
        ),
    ] {
        assert_eq!(sample_ratio(&timing), Err(reason));
    }

    // 中位数、样本不足、限幅
    assert_eq!(factor_from(&[1.2]), None);
    assert!((factor_from(&[1.1, 1.3]).unwrap() - 1.2).abs() < 1e-12);
    assert_eq!(
        factor_from(&[1.0, 4.0, 1.2]),
        Some(1.2),
        "median ignores one outlier"
    );
    assert_eq!(factor_from(&[0.3, 0.3, 0.3]), Some(0.5));
    assert_eq!(factor_from(&[4.0, 4.5]), Some(3.0));

    // 只留最近 N 次；目标数有上限，去掉最久没更新的
    let mut settings = serde_json::Map::new();
    for i in 0..(MAX_SAMPLES + 3) {
        record_ratio(&mut settings, "m1", 1.0 + i as f64 / 100.0, 100 + i as u64);
    }
    let ratios = settings[SETTINGS_KEY]["m1"]["ratios"].as_array().unwrap();
    assert_eq!(ratios.len(), MAX_SAMPLES);
    assert_eq!(ratios[0], 1.03);
    for t in 0..MAX_TARGETS {
        record_ratio(&mut settings, &format!("t{t}"), 1.0, 1000 + t as u64);
    }
    let store = settings[SETTINGS_KEY].as_object().unwrap();
    assert_eq!(store.len(), MAX_TARGETS);
    assert!(!store.contains_key("m1"), "oldest target dropped");

    // 套用校准保留原始值，重复套用不叠乘
    let mut estimate = serde_json::json!({"estimated_seconds": 100.0, "estimated_minutes": 1.67,
        "laser_seconds": 60.0, "model": "acceleration"});
    apply_to_estimate(&mut estimate, 1.2, 4);
    apply_to_estimate(&mut estimate, 1.2, 4);
    assert_eq!(estimate["raw_estimated_seconds"], 100.0);
    assert_eq!(estimate["estimated_seconds"], 120.0);
    assert_eq!(estimate["estimated_minutes"], 2.0);
    assert_eq!(estimate["laser_seconds"], 60.0);
    assert_eq!(
        estimate["calibration"],
        serde_json::json!({"factor": 1.2, "samples": 4, "source": "recent_jobs"})
    );

    // 局域网只读：清除校准算写操作
    assert!(is_reference_data_write(
        "POST",
        "/api/time-calibration",
        b"{\"action\":\"reset\"}"
    ));
    assert!(!is_reference_data_write(
        "GET",
        "/api/time-calibration",
        b""
    ));
}

#[test]
fn time_calibration_is_persisted_per_machine_and_applied_to_new_previews() {
    let temp = tempfile::tempdir().unwrap();
    let service = wizard_service(&temp);
    let runtime = temp.path().join("runtime");
    let raw = post_json(
        &service,
        "/api/text-task/generate",
        &text_body(serde_json::json!({})),
    );
    let estimate = &raw["result"]["artifacts"]["time_estimate"];
    assert!(estimate.get("calibration").is_none(), "{estimate}");
    let raw_seconds = estimate["estimated_seconds"].as_f64().unwrap();

    // 不计的作业不写任何东西
    let skipped = record_job_timing(
        Some(&runtime),
        "laser.local:23",
        &JobTiming {
            status: "completed",
            resumed: true,
            sd_offline: false,
            actual_seconds: 300.0,
            estimated_seconds: Some(200.0),
        },
    );
    assert_eq!(
        skipped,
        serde_json::json!({"recorded": false, "reason": "resumed"})
    );
    assert!(!read_runtime_settings(Some(&runtime)).contains_key(SETTINGS_KEY));

    // 一次样本还不够；两次后开始校准
    for (actual, expected_samples) in [(300.0, 1), (260.0, 2)] {
        let note = record_job_timing(
            Some(&runtime),
            "laser.local:23",
            &JobTiming {
                status: "completed",
                resumed: false,
                sd_offline: false,
                actual_seconds: actual,
                estimated_seconds: Some(200.0),
            },
        );
        assert_eq!(note["recorded"], true, "{note}");
        assert_eq!(note["samples"], expected_samples);
    }
    let status = get(&service, "/api/time-calibration");
    assert_eq!(status["result"]["targets"][0]["target"], "laser.local:23");
    assert_eq!(status["result"]["targets"][0]["factor"], 1.4, "{status}");
    assert_eq!(status["result"]["targets"][0]["active"], true);

    // 指定这台机器的连接时套用它的系数；没给连接时用最近校准过的那台
    for extra in [
        serde_json::json!({"connection_mode": "network", "network_host": "laser.local", "network_telnet_port": 23}),
        serde_json::json!({}),
    ] {
        let calibrated = post_json(&service, "/api/text-task/generate", &text_body(extra));
        let estimate = &calibrated["result"]["artifacts"]["time_estimate"];
        assert_eq!(estimate["raw_estimated_seconds"], raw_seconds, "{estimate}");
        assert_eq!(estimate["calibration"]["factor"], 1.4);
        assert_eq!(estimate["calibration"]["samples"], 2);
        let seconds = estimate["estimated_seconds"].as_f64().unwrap();
        assert!((seconds - (raw_seconds * 1.4 * 10.0).round() / 10.0).abs() < 1e-9);
    }
    // 别的机器没有样本：不借用这台的系数
    let other = post_json(
        &service,
        "/api/text-task/generate",
        &text_body(
            serde_json::json!({"connection_mode": "network", "network_host": "other.local"}),
        ),
    );
    assert!(
        other["result"]["artifacts"]["time_estimate"]
            .get("calibration")
            .is_none()
    );
    // 标定矩阵预览同样套用
    let matrix = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({"material": "椴木", "thickness_mm": 3, "rows": 2, "columns": 2}),
    );
    assert_eq!(
        matrix["result"]["time_estimate"]["calibration"]["factor"],
        1.4
    );

    // 清除后不再校准
    let (code, reset) = post_status(
        &service,
        "/api/time-calibration",
        &serde_json::json!({"action": "reset", "target": "laser.local:23"}),
    );
    assert_eq!(code, 200, "{reset}");
    assert_eq!(reset["result"]["targets"], serde_json::json!([]));
    let after = post_json(
        &service,
        "/api/text-task/generate",
        &text_body(serde_json::json!({})),
    );
    assert!(
        after["result"]["artifacts"]["time_estimate"]
            .get("calibration")
            .is_none()
    );
    let (code, _) = post_status(
        &service,
        "/api/time-calibration",
        &serde_json::json!({"action": "wipe"}),
    );
    assert_eq!(code, 400);
}
