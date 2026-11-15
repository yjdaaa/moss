use super::*;

fn mask_golden_dynamic_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if matches!(
                    key.as_str(),
                    "workflow_id"
                        | "task_id"
                        | "calibration_id"
                        | "job_id"
                        | "created_at"
                        | "updated_at"
                        | "source_path"
                        | "backup_path"
                        | "params_file"
                        | "content_fingerprint"
                ) {
                    *child = serde_json::Value::String("<dynamic>".into());
                } else {
                    mask_golden_dynamic_fields(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mask_golden_dynamic_fields(item);
            }
        }
        _ => {}
    }
}

fn normalize_golden_numbers(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Number(number) => {
            if let Some(float) = number.as_f64()
                && float.is_finite()
                && float.fract() == 0.0
            {
                *value = serde_json::json!(float as i64);
            }
        }
        serde_json::Value::Object(object) => {
            for child in object.values_mut() {
                normalize_golden_numbers(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_golden_numbers(item);
            }
        }
        _ => {}
    }
}

fn mask_default_power_scales(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            if object.get("power_scale") == Some(&serde_json::json!(1000)) {
                object.remove("power_scale");
            }
            for child in object.values_mut() {
                mask_default_power_scales(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                mask_default_power_scales(item);
            }
        }
        _ => {}
    }
}

fn assert_golden_api_response(raw: &[u8], file_name: &str) {
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response must contain a header separator");
    let headers = String::from_utf8_lossy(&raw[..separator]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("HTTP response must contain a status code")
        .parse::<u16>()
        .expect("HTTP status code must be numeric");
    let mut actual = serde_json::json!({
        "status": status,
        "body": serde_json::from_slice::<serde_json::Value>(&raw[separator + 4..])
            .expect("HTTP response body must be valid JSON"),
    });
    let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/api")
        .join(file_name);
    let mut expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&golden_path).expect("golden response must exist"))
            .expect("golden response must be valid JSON");
    mask_golden_dynamic_fields(&mut actual);
    mask_golden_dynamic_fields(&mut expected);
    mask_default_power_scales(&mut actual);
    mask_default_power_scales(&mut expected);
    normalize_golden_numbers(&mut actual);
    normalize_golden_numbers(&mut expected);
    assert_eq!(
        actual,
        expected,
        "API response differs from {}",
        golden_path.display()
    );
}

fn assert_golden_bootstrap_response(raw: &[u8], file_name: &str) {
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response must contain a header separator");
    let headers = String::from_utf8_lossy(&raw[..separator]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("HTTP response must contain a status code")
        .parse::<u16>()
        .expect("HTTP status code must be numeric");
    let body = serde_json::from_slice::<serde_json::Value>(&raw[separator + 4..])
        .expect("HTTP response body must be valid JSON");
    let actual_result = body
        .get("result")
        .cloned()
        .expect("bootstrap response must contain result");
    let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/api")
        .join(file_name);
    let mut expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&golden_path).expect("golden response must exist"))
            .expect("golden response must be valid JSON");
    if let Some(materials) = expected.get_mut("materials")
        && materials.get("version").is_none()
    {
        let legacy_materials = materials.take();
        *materials = serde_json::json!({
            "version": 1,
            "materials": legacy_materials,
            "power_scale": 1000
        });
    }
    let mut actual = serde_json::json!({
        "status": status,
        "result": actual_result,
    });
    mask_golden_dynamic_fields(&mut actual);
    mask_golden_dynamic_fields(&mut expected);
    mask_default_power_scales(&mut actual);
    mask_default_power_scales(&mut expected);
    normalize_golden_numbers(&mut actual);
    normalize_golden_numbers(&mut expected);
    assert_eq!(actual["status"], 200);
    assert_eq!(
        actual["result"],
        expected,
        "bootstrap response differs from {}",
        golden_path.display()
    );
}

#[test]
fn golden_api_snapshot_parity_test() {
    let temp = tempfile::tempdir().unwrap();
    let upload_dir = temp.path().join("uploads");
    let generated_dir = temp.path().join("generated");
    std::fs::create_dir_all(&upload_dir).unwrap();
    std::fs::create_dir_all(&generated_dir).unwrap();

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
    .with_upload_root(upload_dir)
    .with_generated_root(generated_dir.clone())
    .with_preview_roots(vec![generated_dir])
    // 黄金样本描述“本机没有串口”的基线；真实串口枚举随插入的设备变化，不能进快照。
    .with_serial_port_lister(std::sync::Arc::new(Vec::<String>::new));

    // 1. health
    let res = service.handle_http(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_api_response(&res, "health.response.json");

    // 2. runtime-config
    let res =
        service.handle_http(b"GET /api/ui/runtime-config HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_api_response(&res, "runtime-config.response.json");

    // 3. serial-ports
    let res = service.handle_http(b"GET /api/ui/serial-ports HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_api_response(&res, "serial-ports.response.json");

    // 4. material-options
    let res =
        service.handle_http(b"GET /api/ui/material-options HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_api_response(&res, "material-options.response.json");

    // 5. material-recommendation
    let res = service.handle_http(b"GET /api/ui/material-recommendation?material=%E7%BA%B8%E6%9D%BF&thickness_mm=3&laser_mode=engrave&engraving_mode=outline HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_api_response(&res, "material-recommendation.response.json");

    // 6. preview-missing
    let res = service.handle_http(b"GET /preview HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_api_response(&res, "preview-missing.response.json");

    // 7. material-lab-export
    let res =
        service.handle_http(b"GET /api/material-lab/export HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_api_response(&res, "material-lab-export.response.json");

    // 8. upload (empty payload)
    let res = service.handle_http(
        b"POST /api/upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}",
    );
    assert_golden_api_response(&res, "upload.response.json");

    // 9. draw-preview (empty payload)
    let res = service.handle_http(
        b"POST /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}",
    );
    assert_golden_api_response(&res, "draw-preview.response.json");

    // 10. material-preview (empty payload)
    let res = service.handle_http(b"POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}");
    assert_golden_api_response(&res, "material-preview.response.json");

    // 11. material-manage ({"action":"import", "library_json":{}})
    let b = r#"{"action":"import","library_json":{}}"#;
    let res = service.handle_http(format!("POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
    assert_golden_api_response(&res, "material-manage.response.json");

    // 12. material-send
    let b = r#"{"calibration_id":"","confirmed":false}"#;
    let res = service.handle_http(format!("POST /api/material-lab/send HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
    assert_golden_api_response(&res, "material-send.response.json");

    // 13. material-select-cell
    let res = service.handle_http(b"POST /api/material-lab/select-cell HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}");
    assert_golden_api_response(&res, "material-select-cell.response.json");

    // 14. connection-check
    let b = r#"{"connection_mode":"network","network_host":""}"#;
    let res = service.handle_http(format!("POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
    assert_golden_api_response(&res, "connection-check.response.json");

    // 15. text-generate (empty)
    let res = service.handle_http(
        b"POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}",
    );
    assert_golden_api_response(&res, "text-generate.response.json");

    // 16. text-send (empty)
    let b = r#"{"workflow_id":"","confirmed":false}"#;
    let res = service.handle_http(
        format!(
            "POST /api/text-task/send HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            b.len(),
            b
        )
        .as_bytes(),
    );
    assert_golden_api_response(&res, "text-send.response.json");

    // 17. workflow (status empty id)
    let b = r#"{"action":"status","workflow_id":""}"#;
    let res = service.handle_http(
        format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            b.len(),
            b
        )
        .as_bytes(),
    );
    assert_golden_api_response(&res, "workflow.response.json");

    // 18. bootstrap
    let res =
        service.handle_http(b"GET /v1/material-lab/bootstrap HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_golden_bootstrap_response(&res, "get-v1-material-lab-bootstrap.json");
}
