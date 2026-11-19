use super::*;

#[test]
fn material_manage_writes_through_to_runtime_db_when_attached() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("materials.json");
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    let mut settings = test_loopback_settings();
    settings.material_params_file = Some(file.display().to_string());
    let service = ApiService::new(settings, laser_contracts::builtin_material_library())
        .with_runtime_db(db.clone());

    let save = serde_json::json!({
        "action": "save", "material": "测试胶合板", "thickness_mm": 4, "laser_mode": "cut",
        "laser_min_power": 0, "laser_max_power": 900, "feed_rate": 300, "travel_rate": 3000,
        "passes": 2, "pixel_size_mm": 0.1, "threshold": -1
    })
    .to_string();
    let request = format!(
        "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        save.len(),
        save
    );
    let saved = parse_json_response(&service.handle_http(request.as_bytes()));
    assert_eq!(saved["success"], true, "{saved}");
    let count_cells = |sql: &str| -> i64 { db.lock().unwrap().test_query_i64(sql).unwrap() };
    assert_eq!(
        count_cells(
            "SELECT COUNT(*) FROM material_params WHERE material_name='测试胶合板' AND thickness_key='4' AND laser_mode='cut'"
        ),
        1,
        "保存必须写入 SQLite material_params"
    );
    assert!(
        db.lock()
            .unwrap()
            .export_material_library()
            .unwrap()
            .materials
            .contains_key("测试胶合板")
    );

    let delete = serde_json::json!({
        "action": "delete", "material": "测试胶合板", "thickness_mm": 4, "confirmed": true
    })
    .to_string();
    let request = format!(
        "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        delete.len(),
        delete
    );
    let deleted = parse_json_response(&service.handle_http(request.as_bytes()));
    assert_eq!(deleted["success"], true, "{deleted}");
    assert_eq!(
        count_cells("SELECT COUNT(*) FROM material_params WHERE material_name='测试胶合板'"),
        0
    );
}

#[test]
fn recommendation_route_projects_core_result_and_rejects_bad_input() {
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
    let response = service.handle_http(
            b"GET /api/ui/material-recommendation?material=%E7%BA%B8%E6%9D%BF&thickness_mm=3&laser_mode=engrave&engraving_mode=outline HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
    let body_start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let payload: serde_json::Value = serde_json::from_slice(&response[body_start..]).unwrap();
    assert_eq!(payload["result"]["match"], "nearest_thickness");
    assert_eq!(payload["result"]["laser_max_power"], 220);
    let bad = service.handle_http(
            b"GET /api/ui/material-recommendation?material=x&thickness_mm=0&laser_mode=engrave HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
    assert!(String::from_utf8_lossy(&bad).starts_with("HTTP/1.1 400 Bad Request"));
}

#[test]
fn material_lab_preview_send_and_select_cell_run_real_pipeline() {
    for use_store in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let gen_dir = temp.path().join("generated");
        std::fs::create_dir_all(&gen_dir).unwrap();
        let params_file = temp.path().join("materials.json");
        let settings = laser_runtime::LaserSettings {
            material_params_file: Some(params_file.to_string_lossy().into_owned()),
            ..test_loopback_settings()
        };
        let mut service = ApiService::new(settings, laser_contracts::builtin_material_library())
            .with_generated_root(gen_dir.clone())
            .with_preview_roots(vec![gen_dir.clone()])
            .with_transport_factory(fake_transport_factory(200_000));
        if use_store {
            service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
        }

        // 1. 预览：真实生成 4x3 矩阵、落盘 G-code、返回 calibration_id 与 cells
        let preview = post_json(
            &service,
            "/api/material-lab/preview",
            &serde_json::json!({
                "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave", "matrix_kind": "engrave",
                "rows": 4, "columns": 3, "power_min": 100, "power_max": 500, "speed_min": 600, "speed_max": 2400
            }),
        );
        assert_eq!(preview["success"], true, "{preview}");
        let cal_id = preview["result"]["calibration_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(cal_id.starts_with("cal-"), "{cal_id}");
        assert_eq!(preview["result"]["status"], "prepared");
        let cells = preview["result"]["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 12);
        assert_eq!(cells[0]["power"], 100);
        assert_eq!(cells[2]["power"], 500);
        assert_eq!(cells[0]["speed"], 600);
        assert_eq!(cells[11]["speed"], 2400);
        let gcode_file = preview["result"]["gcode_file"].as_str().unwrap().to_owned();
        let gcode_path = gen_dir.join(&gcode_file);
        let original = std::fs::read(&gcode_path).unwrap();
        assert!(original.len() > 200, "标定 G-code 必须真实落盘");
        assert_eq!(
            hex::encode(Sha256::digest(&original)),
            preview["result"]["gcode_sha256"].as_str().unwrap()
        );

        // 2. 未确认 / 缺 host 必须拒绝
        let unconfirmed = post_json(
            &service,
            "/api/material-lab/send",
            &serde_json::json!({"calibration_id": cal_id}),
        );
        assert_eq!(unconfirmed["success"], false);
        let no_host = post_json(
            &service,
            "/api/material-lab/send",
            &serde_json::json!({"calibration_id": cal_id, "confirmed": true}),
        );
        assert_eq!(no_host["success"], false);

        // 3. 篡改落盘刀路：物理 SHA 门禁拒绝
        std::fs::write(&gcode_path, b"G1 X999 Y999 S1000\n").unwrap();
        let tampered = post_json(
            &service,
            "/api/material-lab/send",
            &serde_json::json!({
                "calibration_id": cal_id, "confirmed": true, "network_host": "127.0.0.1", "network_telnet_port": 23
            }),
        );
        assert_eq!(tampered["success"], false, "{tampered}");
        assert_eq!(
            tampered["detail"]["error_code"], "send_gate_rejected",
            "{tampered}"
        );
        std::fs::write(&gcode_path, &original).unwrap();

        // 4. 真实 JobWorker 下发并推进到 completed
        let sent = post_json(
            &service,
            "/api/material-lab/send",
            &serde_json::json!({
                "calibration_id": cal_id, "confirmed": true, "network_host": "127.0.0.1", "network_telnet_port": 23
            }),
        );
        assert_eq!(sent["success"], true, "{sent}");
        assert_eq!(sent["result"]["status"], "sending");
        assert_eq!(sent["result"]["probe"]["connected"], true);
        assert_eq!(sent["result"]["send_result"]["running"], true);
        let total = sent["result"]["send_result"]["total_lines"]
            .as_u64()
            .unwrap();
        assert!(total > 0);
        let start = std::time::Instant::now();
        let finished = loop {
            let status = post_json(
                &service,
                "/api/material-lab/status",
                &serde_json::json!({"calibration_id": cal_id}),
            );
            if status["result"]["status"] == "completed"
                || start.elapsed() > std::time::Duration::from_secs(5)
            {
                break status;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(finished["result"]["status"], "completed", "{finished}");
        assert_eq!(
            finished["result"]["send_result"]["lines_sent"]
                .as_u64()
                .unwrap(),
            total
        );
        assert_eq!(service.active_job_count(), 0);

        // 5. 完成后禁止重复下发
        let again = post_json(
            &service,
            "/api/material-lab/send",
            &serde_json::json!({
                "calibration_id": cal_id, "confirmed": true, "network_host": "127.0.0.1", "network_telnet_port": 23
            }),
        );
        assert_eq!(
            again["detail"]["error_code"], "calibration_not_sendable",
            "{again}"
        );

        // 6. 选格：单元格 #6（第 2 行第 3 列）功率 500、速度 = 600+(2400-600)/3 = 1200 写回材料库
        let selected = post_json(
            &service,
            "/api/material-lab/select-cell",
            &serde_json::json!({
                "calibration_id": cal_id, "cell_number": 6, "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave"
            }),
        );
        assert_eq!(selected["success"], true, "{selected}");
        assert_eq!(selected["result"]["selected_cell"]["power"], 500);
        assert_eq!(selected["result"]["selected_cell"]["speed"], 1200);
        let saved_cell = &selected["result"]["materials"]["materials"]["椴木"]["thicknesses"]["3"]
            ["engrave"]["raster"];
        assert_eq!(saved_cell["laser_max_power"], 500, "{selected}");
        assert_eq!(saved_cell["feed_rate"], 1200, "{selected}");
        assert_eq!(saved_cell["source"], "calibration");
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&params_file).unwrap()).unwrap();
        assert_eq!(
            on_disk["materials"]["椴木"]["thicknesses"]["3"]["engrave"]["raster"]["feed_rate"],
            1200
        );
        let out_of_range = post_json(
            &service,
            "/api/material-lab/select-cell",
            &serde_json::json!({"calibration_id": cal_id, "cell_number": 13}),
        );
        assert_eq!(out_of_range["success"], false);
        let missing = post_json(
            &service,
            "/api/material-lab/select-cell",
            &serde_json::json!({"calibration_id": "cal-nope", "cell_number": 1}),
        );
        assert_eq!(missing["success"], false);
    }
}

#[test]
fn material_manage_persists_export_import_save_and_delete() {
    let temp = tempfile::tempdir().unwrap();
    let material_file = temp.path().join("materials.json");
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: Some(material_file.to_string_lossy().into_owned()),
        },
        laser_contracts::builtin_material_library(),
    );
    let request = |body: serde_json::Value| {
        let raw = body.to_string();
        let request = format!(
            "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            raw.len(),
            raw
        );
        service.handle_http(request.as_bytes())
    };
    let saved = request(serde_json::json!({
        "action": "save",
        "material": "测试材料",
        "thickness_mm": 2,
        "laser_mode": "engrave",
        "engraving_mode": "raster",
        "laser_max_power": "500",
        "feed_rate": "1000",
        "aliases": "test, 测试"
    }));
    assert!(String::from_utf8_lossy(&saved).starts_with("HTTP/1.1 200 OK"));
    let exported =
        service.handle_http(b"GET /api/material-lab/export HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let start = exported
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let exported: serde_json::Value = serde_json::from_slice(&exported[start..]).unwrap();
    assert_eq!(
        exported["result"]["library"]["materials"]["测试材料"]["thicknesses"]["2"]["engrave"]["raster"]
            ["laser_max_power"],
        500
    );
    let invalid =
        request(serde_json::json!({"action": "import", "library_json": {"version": "1"}}));
    assert!(String::from_utf8_lossy(&invalid).starts_with("HTTP/1.1 400 Bad Request"));
    let deleted = request(serde_json::json!({
        "action": "delete",
        "material": "测试材料",
        "thickness_mm": 2,
        "confirmed": true
    }));
    assert!(String::from_utf8_lossy(&deleted).starts_with("HTTP/1.1 200 OK"));
}

#[test]
fn material_manage_supports_d9_atomic_batch_thicknesses() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("materials.json");
    let service = ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: Some(file.display().to_string()),
        },
        laser_contracts::builtin_material_library(),
    );
    let batch_body = serde_json::json!({
        "action": "save",
        "material": "椴木测试板",
        "thicknesses": {
            "2.0": {
                "cut": {
                    "laser_min_power": 0,
                    "laser_max_power": 900,
                    "feed_rate": 300,
                    "travel_rate": 3000,
                    "threshold": -1,
                    "passes": 2,
                    "pixel_size_mm": 0.1
                }
            },
            "3.0": {
                "cut": {
                    "laser_min_power": 0,
                    "laser_max_power": 1000,
                    "feed_rate": 200,
                    "travel_rate": 3000,
                    "threshold": -1,
                    "passes": 3,
                    "pixel_size_mm": 0.1
                }
            }
        }
    });
    let raw = serde_json::to_vec(&batch_body).unwrap();
    let request = format!(
        "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        raw.len(),
        String::from_utf8_lossy(&raw)
    );
    let response = service.handle_http(request.as_bytes());
    let start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let parsed: serde_json::Value = serde_json::from_slice(&response[start..]).unwrap();
    assert_eq!(parsed["success"], true);
    assert!(
        parsed["result"]["materials"]["materials"]["椴木测试板"]["thicknesses"]["2.0"]["cut"]
            .is_object()
    );
    assert!(
        parsed["result"]["materials"]["materials"]["椴木测试板"]["thicknesses"]["3.0"]["cut"]
            .is_object()
    );
}

#[test]
fn sqlite_only_custom_material_supports_save_recommend_and_text_cut_or_engrave_generation() {
    let temp = tempfile::tempdir().unwrap();
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    let service = ApiService::new(
        test_loopback_settings(), // material_params_file: None
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_db(db)
    .with_generated_root(temp.path().to_path_buf());

    // 1. 使用数组 thicknesses ([3, 5]) 保存切割参数（MaterialModal 发送形态）
    let save_cut = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "save",
            "material": "特制胡桃木",
            "aliases": ["walnut-pro"],
            "thicknesses": [3, 5],
            "laser_mode": "cut",
            "laser_max_power": 820,
            "feed_rate": 280,
            "passes": 2
        }),
    );
    assert_eq!(save_cut["success"], true, "{save_cut}");

    // 2. 再次保存第二种自定义材料，验证多次编辑保留此前 SQLite 中的材料
    let save_second = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "save",
            "material": "特制楠竹",
            "thickness_mm": 4.0,
            "laser_mode": "engrave",
            "engraving_mode": "raster",
            "laser_max_power": 460,
            "feed_rate": 1350,
            "passes": 1
        }),
    );
    assert_eq!(save_second["success"], true, "{save_second}");

    // 3. bootstrap / material-options / export / recommendation 均可读到两个材料
    let bootstrap = parse_json_response(
        &service.handle_http(b"GET /v1/material-lab/bootstrap HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert!(
        bootstrap["result"]["materials"]["materials"]["特制胡桃木"].is_object(),
        "{bootstrap}"
    );
    assert!(
        bootstrap["result"]["materials"]["materials"]["特制楠竹"].is_object(),
        "{bootstrap}"
    );

    let options = service.handle("GET", "/api/ui/material-options", b"");
    let names: Vec<&str> = options.body["result"]["materials"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();
    assert!(
        names.contains(&"特制胡桃木") && names.contains(&"特制楠竹"),
        "{names:?}"
    );

    let exported = service.handle("GET", "/api/material-lab/export", b"");
    assert_eq!(exported.status, 200);
    assert!(
        exported.body["result"]["library"]["materials"]["特制胡桃木"]["thicknesses"]["5"]["cut"]
            .is_object()
    );

    let rec = parse_json_response(&service.handle_http(
            b"GET /api/ui/material-recommendation?material=walnut-pro&thickness_mm=5&laser_mode=cut HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
    assert_eq!(rec["success"], true, "{rec}");
    assert_eq!(rec["result"]["material"], "特制胡桃木");
    assert_eq!(rec["result"]["laser_max_power"], 820);
    assert_eq!(rec["result"]["feed_rate"], 280);

    // 4. 文字生成支持自定义材料 + cut 模式
    let font_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let generated = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "AB",
            "font_path": font_path,
            "material": "特制胡桃木",
            "thickness_mm": 5,
            "engraving_mode": "cut"
        }),
    );
    assert_eq!(generated["success"], true, "{generated}");
    let gcode_file = generated["result"]["artifacts"]["gcode_file"]
        .as_str()
        .unwrap();
    let gcode = std::fs::read_to_string(temp.path().join(gcode_file)).unwrap();
    assert!(gcode.contains("S820") && gcode.contains("F280"), "{gcode}");
}

#[test]
fn material_object_thickness_merge_preserves_other_modes_empty_db_stays_empty_and_corrupt_file_fails()
 {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("materials.json");
    let mut settings = test_loopback_settings();
    settings.material_params_file = Some(file.display().to_string());
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    let service = ApiService::new(settings, laser_contracts::builtin_material_library())
        .with_runtime_db(db.clone());

    // 1. 椴木 3mm 原本同时含 cut / engrave.raster / engrave.outline；前端按对象提交仅含 raster 时不得丢失 cut 与 outline
    let save_raster = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "save",
            "material": "椴木",
            "thicknesses": {
                "3": {
                    "engrave": {
                        "raster": {
                            "laser_min_power": 0,
                            "laser_max_power": 555,
                            "feed_rate": 1666,
                            "travel_rate": 3000,
                            "threshold": -1,
                            "passes": 1,
                            "pixel_size_mm": 0.1
                        }
                    }
                }
            }
        }),
    );
    assert_eq!(save_raster["success"], true, "{save_raster}");
    let basswood_3 = &save_raster["result"]["materials"]["materials"]["椴木"]["thicknesses"]["3"];
    assert_eq!(
        basswood_3["engrave"]["raster"]["laser_max_power"], 555,
        "{basswood_3}"
    );
    assert!(
        basswood_3["cut"].is_object(),
        "修改 raster 后不得擦除同厚度 cut: {basswood_3}"
    );
    assert!(
        basswood_3["engrave"]["outline"].is_object(),
        "修改 raster 后不得擦除同厚度 outline: {basswood_3}"
    );

    // 2. 损坏材料镜像文件时推荐与标定必须显式返回 500，绝不静默降级为内置参数
    std::fs::write(&file, "{corrupt-material-json").unwrap();
    let rec_raw = service.handle_http(
            b"GET /api/ui/material-recommendation?material=%E6%A4%B4%E6%9C%A8&thickness_mm=3&laser_mode=cut HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
    assert!(String::from_utf8_lossy(&rec_raw).starts_with("HTTP/1.1 500"));
    let cal_err = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({"material": "椴木", "thickness_mm": 3, "laser_mode": "cut"}),
    );
    assert_eq!(cal_err["success"], false, "{cal_err}");

    // 3. 纯 SQLite 模式下清空全部材料后，再次读取必须保持空库，不得复活内置预设
    let db_only_service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_db(std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    )));
    let import_empty = post_json(
        &db_only_service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "import",
            "library_json": { "version": 1, "power_scale": 1000, "materials": {} }
        }),
    );
    assert_eq!(import_empty["success"], true, "{import_empty}");
    let bootstrap = parse_json_response(
        &db_only_service
            .handle_http(b"GET /v1/material-lab/bootstrap HTTP/1.1\r\nHost: localhost\r\n\r\n"),
    );
    assert_eq!(
        bootstrap["result"]["materials"]["materials"],
        serde_json::json!({}),
        "清空后再次加载不得复活内置预设: {bootstrap}"
    );
}

#[test]
fn calibration_workflow_status_cancel_and_txt_upload_rejection_work() {
    let temp = tempfile::tempdir().unwrap();
    let uploads = temp.path().join("uploads");
    let gen_dir = temp.path().join("gen");
    std::fs::create_dir_all(&uploads).unwrap();
    std::fs::create_dir_all(&gen_dir).unwrap();
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_upload_root(uploads.clone())
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![uploads, gen_dir])
    .with_runtime_db(db);

    let encoded = base64::engine::general_purpose::STANDARD
        .encode("G21\nG90\nG0 X0 Y0\nG1 X10 Y10 S200 F1000\nM5\n");
    let rejected_txt = post_json(
        &service,
        "/api/upload",
        &serde_json::json!({
            "filename": "part.txt",
            "content_base64": encoded,
        }),
    );
    assert_eq!(rejected_txt["success"], false, "{rejected_txt}");

    let up_nc = post_json(
        &service,
        "/api/upload",
        &serde_json::json!({
            "filename": "part.nc",
            "content_base64": encoded,
        }),
    );
    assert_eq!(up_nc["success"], true, "{up_nc}");
    let saved_name = up_nc["result"]["filename"].as_str().unwrap();

    let wf = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "preview",
            "source_type": "gcode",
            "gcode_file": saved_name,
            "material": "椴木",
            "thickness_mm": 3
        }),
    );
    assert_eq!(wf["success"], true, "{wf}");
    let cal = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave", "rows": 2, "columns": 2
        }),
    );
    assert_eq!(cal["success"], true, "{cal}");
    let cal_id = cal["result"]["calibration_id"].as_str().unwrap();
    let cal_status = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({"action": "status", "workflow_id": cal_id}),
    );
    assert_eq!(cal_status["success"], true, "{cal_status}");
    let cal_cancel = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({"action": "cancel", "workflow_id": cal_id}),
    );
    assert_eq!(cal_cancel["success"], true, "{cal_cancel}");
    assert_eq!(cal_cancel["result"]["status"], "cancelled");
}

#[test]
fn material_lab_recent_actual_params_import_merge_validation_and_serial_send() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let params_file = temp.path().join("materials.json");
    let db = std::sync::Arc::new(std::sync::Mutex::new(
        laser_runtime::RuntimeDb::open_memory().unwrap(),
    ));
    let settings = laser_runtime::LaserSettings {
        material_params_file: Some(params_file.to_string_lossy().into_owned()),
        ..test_loopback_settings()
    };
    let service = ApiService::new(settings, laser_contracts::builtin_material_library())
        .with_runtime_db(db.clone())
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir])
        .with_transport_factory(fake_transport_factory(200_000));

    // 1. 工作台生成带 manual_params 的文字预览后，/api/material-lab/recent 提取真实的 actual_params 与刻法
    let generated = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "测试",
            "material": "椴木",
            "thickness_mm": 3.0,
            "text_fill": "hatch",
            "manual_params": { "power_percent": 68, "feed_rate": 950, "passes": 3 }
        }),
    );
    assert_eq!(generated["success"], true, "{generated}");
    let recent = post_json(&service, "/api/material-lab/recent", &serde_json::json!({}));
    assert_eq!(recent["success"], true, "{recent}");
    assert_eq!(recent["result"]["material"], "椴木");
    assert_eq!(recent["result"]["engraving_mode"], "outline");
    assert_eq!(recent["result"]["params"]["laser_max_power"], 680);
    assert_eq!(recent["result"]["params"]["feed_rate"], 950);
    assert_eq!(recent["result"]["params"]["passes"], 3);

    // 2. 危险材料与越界参数在 save / import 时均被拦截
    let pvc_save = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "save",
            "material": "PVC板",
            "thickness_mm": 3,
            "laser_mode": "cut"
        }),
    );
    assert_eq!(pvc_save["success"], false, "{pvc_save}");

    let bad_import = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "import",
            "import_mode": "merge",
            "library_json": {
                "version": 1,
                "power_scale": 1000,
                "materials": {
                    "坏参数木": {
                        "thicknesses": {
                            "3": {
                                "cut": {
                                    "laser_min_power": 0,
                                    "laser_max_power": 5000,
                                    "feed_rate": 0,
                                    "travel_rate": 3000,
                                    "threshold": -1,
                                    "passes": 1,
                                    "pixel_size_mm": 0.1
                                }
                            }
                        }
                    }
                }
            }
        }),
    );
    assert_eq!(bad_import["success"], false, "{bad_import}");

    // 3. 合并导入（import_mode=merge）+ 255 标尺自动折算为 1000 标尺，保留原有椴木且导出不失真
    let merge_255 = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "import",
            "import_mode": "merge",
            "library_json": {
                "library": {
                    "version": 1,
                    "power_scale": 255,
                    "materials": {
                        "进口樱桃木": {
                            "aliases": ["cherry-255"],
                            "thicknesses": {
                                "4.0": {
                                    "cut": {
                                        "laser_min_power": 0,
                                        "laser_max_power": 255,
                                        "feed_rate": 320,
                                        "travel_rate": 3000,
                                        "threshold": 128,
                                        "passes": 2,
                                        "pixel_size_mm": 0.1
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }),
    );
    assert_eq!(merge_255["success"], true, "{merge_255}");
    let mats = &merge_255["result"]["materials"]["materials"];
    assert!(mats["椴木"].is_object(), "merge 模式必须保留原有材料");
    assert_eq!(
        mats["进口樱桃木"]["thicknesses"]["4.0"]["cut"]["laser_max_power"], 1000,
        "255 标尺满功率导入后必须折算为 1000"
    );
    let db_exported = db.lock().unwrap().export_material_library().unwrap();
    assert_eq!(
        db_exported.materials["进口樱桃木"].thicknesses["4.0"]
            .cut
            .as_ref()
            .unwrap()
            .laser_max_power,
        1000
    );

    // 4. prune_missing_thicknesses 同步裁剪编辑弹窗移除的旧厚度，单厚度删除兼容 "4.0" 与 4
    let add_thick = post_json(
        &service,
        "/api/material-lab/manage",
        &serde_json::json!({
            "action": "save",
            "material": "进口樱桃木",
            "prune_missing_thicknesses": true,
            "thicknesses": {
                "2": {
                    "cut": {
                        "laser_min_power": 0,
                        "laser_max_power": 600,
                        "feed_rate": 450,
                        "travel_rate": 3000,
                        "threshold": 128,
                        "passes": 1,
                        "pixel_size_mm": 0.1
                    }
                },
                "6": {
                    "cut": {
                        "laser_min_power": 0,
                        "laser_max_power": 950,
                        "feed_rate": 220,
                        "travel_rate": 3000,
                        "threshold": 128,
                        "passes": 3,
                        "pixel_size_mm": 0.1
                    }
                }
            }
        }),
    );
    assert_eq!(add_thick["success"], true, "{add_thick}");
    let cherry_thicks = &add_thick["result"]["materials"]["materials"]["进口樱桃木"]["thicknesses"];
    assert!(
        cherry_thicks["4.0"].is_null(),
        "已被移除的 4.0mm 厚度应当被清理"
    );
    assert!(cherry_thicks["2"].is_object() && cherry_thicks["6"].is_object());

    // 5. 轮廓标定矩阵预览生成 engrave_outline + 串口模式空端口拦截
    let cal = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木",
            "thickness_mm": 3,
            "laser_mode": "engrave",
            "engraving_mode": "outline",
            "rows": 2,
            "columns": 2
        }),
    );
    assert_eq!(cal["success"], true, "{cal}");
    assert_eq!(cal["result"]["task_type"], "engrave_outline");
    let cal_id = cal["result"]["calibration_id"].as_str().unwrap();
    let missing_port = post_json(
        &service,
        "/api/material-lab/send",
        &serde_json::json!({
            "calibration_id": cal_id,
            "confirmed": true,
            "connection_mode": "serial",
            "port": ""
        }),
    );
    assert_eq!(missing_port["success"], false, "{missing_port}");
}

#[test]
fn material_lab_send_reserves_launch_exclusively_and_cancel_never_overwrites_completed_while_active()
 {
    use laser_runtime::RuntimeStore as _;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // 1. 并发点击同一 calibration_id 发送：慢打开窗口内第二个请求被预约门禁以 409 device_busy 拒绝，传输只打开 1 次
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let open_calls = std::sync::Arc::new(AtomicUsize::new(0));
    let open_calls_for_factory = open_calls.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Arc::new(std::sync::Mutex::new(Some(release_rx)));
    let entered_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(entered_tx)));
    let slow_factory: TransportFactory = std::sync::Arc::new(move |_spec| {
        open_calls_for_factory.fetch_add(1, Ordering::SeqCst);
        if let Some(tx) = entered_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(rx) = release_rx.lock().unwrap().take() {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
        }
        Err("mock_unreachable".into())
    });

    let service = std::sync::Arc::new(
        ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()))
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir.clone()])
        .with_transport_factory(slow_factory),
    );

    let preview = post_json(
        &service,
        "/api/material-lab/preview",
        &serde_json::json!({
            "material": "椴木",
            "thickness_mm": 3.0,
            "mode": "raster",
            "powers": [20, 40],
            "speeds": [600, 1200]
        }),
    );
    assert_eq!(preview["success"], true, "{preview}");
    let cal_id = preview["result"]["calibration_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let rejected_resume = post_json(
        &service,
        "/api/material-lab/send",
        &serde_json::json!({
            "calibration_id": cal_id,
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "start_from_line": 5
        }),
    );
    assert_eq!(
        rejected_resume["detail"]["error_code"], "resume_line_not_allowed",
        "{rejected_resume}"
    );

    let svc_first = service.clone();
    let cal_first = cal_id.clone();
    let first_handle = std::thread::spawn(move || {
        post_json(
            &svc_first,
            "/api/material-lab/send",
            &serde_json::json!({
                "calibration_id": cal_first,
                "confirmed": true,
                "connection_mode": "network",
                "network_host": "127.0.0.1"
            }),
        )
    });

    entered_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("首个标定发送应进入传输打开阶段");

    let second_body = serde_json::json!({
        "calibration_id": cal_id,
        "confirmed": true,
        "connection_mode": "network",
        "network_host": "127.0.0.1"
    })
    .to_string();
    let second_raw = service.handle_http(
            format!(
                "POST /api/material-lab/send HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                second_body.len(),
                second_body
            )
            .as_bytes(),
        );
    assert!(
        String::from_utf8_lossy(&second_raw).starts_with("HTTP/1.1 409"),
        "{}",
        String::from_utf8_lossy(&second_raw)
    );
    let second_resp = parse_json_response(&second_raw);
    assert_eq!(second_resp["detail"]["error_code"], "device_busy");

    let _ = release_tx.send(());
    let _ = first_handle.join().unwrap();
    assert_eq!(
        open_calls.load(Ordering::SeqCst),
        1,
        "并发请求不得穿透预约表重复打开设备传输"
    );

    // 2. 发送线程已把 workflow 落库为 completed、但尚未从 active_jobs 移除时，cancel 必须拒绝篡改 completed
    let store = std::sync::Arc::new(laser_runtime::MemoryStore::new());
    store
        .put(&laser_runtime::StoredRecord {
            kind: laser_runtime::RecordKind::Workflow,
            id: "wf-done-race".into(),
            status: Some("completed".into()),
            payload: serde_json::json!({
                "workflow_id": "wf-done-race",
                "status": "completed",
                "send_result": { "success": true, "running": false, "phase": "completed" }
            }),
        })
        .unwrap();
    let fake_transport: std::sync::Arc<dyn laser_device::DeviceTransport> =
        std::sync::Arc::new(laser_device::FakeGrbl::new(std::iter::empty::<String>()));
    let dummy_job = std::sync::Arc::new(ActiveJob {
        workflow_id: "wf-done-race".into(),
        mode: "network",
        target: "127.0.0.1:23".into(),
        progress: std::sync::Arc::new(laser_device::JobProgress::default()),
        cancel: laser_device::CancellationToken::default(),
        transport: fake_transport,
        started_at: std::time::Instant::now(),
        spec: None,
    });
    let jobs = ActiveJobs::default();
    jobs.lock()
        .unwrap()
        .insert("wf-done-race".into(), dummy_job);
    let reservations: LaunchReservations =
        std::sync::Arc::new(std::sync::Mutex::new(LaunchReservationState::default()));
    let cancel_bytes = handle_workflow_store_request(
        store.as_ref(),
        "/api/workflow",
        serde_json::json!({ "action": "cancel", "workflow_id": "wf-done-race" })
            .as_object()
            .unwrap(),
        &[],
        &jobs,
        &reservations,
        &Ok(laser_device::ConnectionSpec::Tcp {
            host: "127.0.0.1".into(),
            port: 23,
        }),
        &|_, _, _, _| unreachable!(),
    );
    let cancel_str = String::from_utf8_lossy(&cancel_bytes);
    assert!(
        cancel_str.starts_with("HTTP/1.1 400"),
        "已落库 completed 的任务即使尚未从 active_jobs 移除也须拒绝取消: {cancel_str}"
    );
    assert!(cancel_str.contains("workflow_completed"), "{cancel_str}");
    let after = store
        .get(laser_runtime::RecordKind::Workflow, "wf-done-race")
        .unwrap()
        .unwrap();
    assert_eq!(after.status.as_deref(), Some("completed"));
    assert_eq!(after.payload["status"], "completed");

    // 3. 存储落库报错时 cancel 仍必须无条件停止活动任务并写出 FeedHold（停止动作永不依赖落盘）
    struct SetStatusFailStore;
    impl laser_runtime::RuntimeStore for SetStatusFailStore {
        fn get(
            &self,
            _kind: laser_runtime::RecordKind,
            _id: &str,
        ) -> Result<Option<laser_runtime::StoredRecord>, laser_runtime::SchemaError> {
            Err(laser_runtime::SchemaError::Busy)
        }
        fn put(
            &self,
            _record: &laser_runtime::StoredRecord,
        ) -> Result<(), laser_runtime::SchemaError> {
            Err(laser_runtime::SchemaError::Busy)
        }
        fn set_status(
            &self,
            _kind: laser_runtime::RecordKind,
            _id: &str,
            _status: &str,
            _now: f64,
        ) -> Result<bool, laser_runtime::SchemaError> {
            Err(laser_runtime::SchemaError::Busy)
        }
        fn list_recent(
            &self,
            _kind: laser_runtime::RecordKind,
            _limit: usize,
        ) -> Result<Vec<laser_runtime::StoredRecord>, laser_runtime::SchemaError> {
            Err(laser_runtime::SchemaError::Busy)
        }
        fn save_feedback(
            &self,
            _workflow_id: &str,
            _feedback: &serde_json::Value,
        ) -> Result<bool, laser_runtime::SchemaError> {
            Err(laser_runtime::SchemaError::Busy)
        }
        fn adopt_feedback(
            &self,
            _workflow_id: &str,
            _feedback: &serde_json::Value,
            _preview: &laser_runtime::StoredRecord,
        ) -> Result<bool, laser_runtime::SchemaError> {
            Err(laser_runtime::SchemaError::Busy)
        }
    }
    let grbl = std::sync::Arc::new(laser_device::FakeGrbl::new(std::iter::empty::<String>()));
    let active_cancel = laser_device::CancellationToken::default();
    let err_job = std::sync::Arc::new(ActiveJob {
        workflow_id: "wf-store-err".into(),
        mode: "network",
        target: "127.0.0.1:23".into(),
        progress: std::sync::Arc::new(laser_device::JobProgress::default()),
        cancel: active_cancel.clone(),
        transport: grbl.clone(),
        started_at: std::time::Instant::now(),
        spec: None,
    });
    let err_jobs = ActiveJobs::default();
    err_jobs
        .lock()
        .unwrap()
        .insert("wf-store-err".into(), err_job);
    let err_reservations: LaunchReservations =
        std::sync::Arc::new(std::sync::Mutex::new(LaunchReservationState::default()));
    let err_bytes = handle_workflow_store_request(
        &SetStatusFailStore,
        "/api/workflow",
        serde_json::json!({ "action": "cancel", "workflow_id": "wf-store-err" })
            .as_object()
            .unwrap(),
        &[],
        &err_jobs,
        &err_reservations,
        &Ok(laser_device::ConnectionSpec::Tcp {
            host: "127.0.0.1".into(),
            port: 23,
        }),
        &|_, _, _, _| unreachable!(),
    );
    assert!(String::from_utf8_lossy(&err_bytes).starts_with("HTTP/1.1 500"));
    assert!(
        active_cancel.is_cancelled(),
        "存储报错时仍必须置位活动任务取消令牌"
    );
    assert!(
        grbl.realtime().contains(&b'!'),
        "存储报错时仍必须向硬件写出实时 FeedHold (!)"
    );
}
