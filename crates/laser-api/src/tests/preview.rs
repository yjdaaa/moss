use super::*;

#[test]
fn frontend_fixture_is_embedded() {
    assert!(embedded_fixture_is_valid());
}

#[test]
fn api_field_conversion_matches_python_edges() {
    let body = br#"{"truth":"false","whole":"3.7","positive":0}"#;
    let object = parse_json_object(body, 64).unwrap();
    assert!(!parse_bool(object.get("truth"), false));
    assert_eq!(parse_int(object.get("whole"), "whole", 0).unwrap(), 3);
    assert_eq!(
        parse_optional_positive(object.get("positive"), "positive").unwrap(),
        None
    );
}

#[test]
fn api_body_and_path_boundaries_are_enforced() {
    assert_eq!(
        parse_json_object(br#"[]"#, 64).unwrap_err(),
        "JSON 顶层必须是对象"
    );
    assert_eq!(
        parse_json_object(&[b'a'; 65], 64).unwrap_err(),
        "请求体过大"
    );
    let root = std::path::PathBuf::from(r"C:\runtime");
    let path = root.join("uploads").join("x.png");
    assert_eq!(exposed_path(&path, &root, true), "uploads/x.png");
    assert_eq!(exposed_path(&path, &root, false), "x.png");
}

#[test]
fn api_service_dispatches_health_config_materials_and_404() {
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
    assert_eq!(service.handle("GET", "/api/health", b"").status, 200);
    assert_eq!(
        service.handle("GET", "/api/ui/material-options", b"").body["success"],
        true
    );
    assert_eq!(service.handle("GET", "/missing", b"").status, 404);
    // 局域网扫描只接受 POST：不带 Origin 的跨站 GET 不能触发 mDNS
    assert_eq!(
        service
            .handle("GET", "/api/ui/discover-controllers", b"")
            .status,
        404
    );
    assert_eq!(
        service.handle("GET", "/api/health", b"").headers[0].0,
        "Cache-Control"
    );
    let config = service.handle("GET", "/api/ui/runtime-config", b"");
    assert_eq!(
        config.body["result"]["connection"]["serial"]["baudrate"],
        115200
    );
    assert!(service.handle("GET", "/api/ui/serial-ports", b"").body["result"]["ports"].is_array());
}

#[test]
fn gcode_source_preview_is_rendered_upright_without_false_closing_lines() {
    let temp = tempfile::tempdir().unwrap();
    let uploads = temp.path().join("uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    // 一个"Γ"形开放折线：竖线在左，横杠在机器坐标 Y=10（顶部）。
    std::fs::write(
        uploads.join("gamma.gcode"),
        "G21\nG90\nM4 S0\nG0 X0 Y0\nG1 X0 Y10 S300 F1000\nG1 X20 Y10 S300 F1000\nM5\n",
    )
    .unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_upload_root(uploads.clone())
    .with_generated_root(temp.path().join("gen"))
    .with_preview_roots(vec![uploads.clone(), temp.path().join("gen")]);
    let body = serde_json::json!({
        "action": "preview", "source_type": "gcode", "gcode_file": "gamma.gcode",
        "material": "椴木", "thickness_mm": 3
    })
    .to_string();
    let request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let value = parse_json_response(&service.handle_http(request.as_bytes()));
    assert_eq!(value["success"], true, "{value}");
    let image_file = value["result"]["artifacts"]["image_file"].as_str().unwrap();
    let img = image::open(temp.path().join("gen").join(image_file))
        .unwrap()
        .to_rgba8();
    let (w, h) = (img.width(), img.height());
    let row_hits = |y: u32| (0..w).filter(|&x| img.get_pixel(x, y).0[3] > 0).count();
    let top: usize = (0..h / 2).map(row_hits).sum();
    let bottom: usize = (h / 2..h).map(row_hits).sum();
    // 横杠在 Y=10（机器坐标顶部）→ 翻转后应出现在图像上半部，上半部像素明显多于下半部
    assert!(
        top > bottom * 2,
        "横杠必须渲染在图像上半部（top={top}, bottom={bottom}）"
    );
    // 开放折线不得被补画闭合斜线：右下角区域（对角线经过处）应基本无像素
    let diag_hits: usize = (h * 6 / 10..h * 8 / 10)
        .flat_map(|y| (w * 5 / 10..w * 7 / 10).map(move |x| (x, y)))
        .filter(|&(x, y)| img.get_pixel(x, y).0[3] > 0)
        .count();
    assert_eq!(diag_hits, 0, "开放折线不应出现伪闭合斜线");
}

/// 2026-11-16（技术书 r41）：上传的 G-code 里 G2/G3 圆弧在预览里按圆弧画出，尺寸按弧上的点计算。
#[test]
fn gcode_source_preview_draws_arcs() {
    let temp = tempfile::tempdir().unwrap();
    let uploads = temp.path().join("uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    // 半径 10 的整圆（I/J 写法），只写了起终点 (10,0)
    std::fs::write(
        uploads.join("circle.gcode"),
        "G21\nG90\nM4 S0\nG0 X10 Y0\nG2 X10 Y0 I-10 J0 S300 F1000\nM5\n",
    )
    .unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_upload_root(uploads.clone())
    .with_generated_root(temp.path().join("gen"))
    .with_preview_roots(vec![uploads.clone(), temp.path().join("gen")]);
    let value = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "preview", "source_type": "gcode", "gcode_file": "circle.gcode",
            "material": "椴木", "thickness_mm": 3
        }),
    );
    assert_eq!(value["success"], true, "{value}");
    let artifacts = &value["result"]["artifacts"];
    for key in ["width_mm", "height_mm"] {
        let size = artifacts[key].as_f64().unwrap();
        assert!(
            (size - 20.0).abs() < 0.1,
            "{key} = {size}（旧实现只看端点得到 1）"
        );
    }
    let img = image::open(
        temp.path()
            .join("gen")
            .join(artifacts["image_file"].as_str().unwrap()),
    )
    .unwrap()
    .to_rgba8();
    let (w, h) = (img.width(), img.height());
    let lit = |x: u32, y: u32| img.get_pixel(x, y).0[3] > 0;
    // 圆心空着，上下左右四个方向的圆周都有线
    assert!(!lit(w / 2, h / 2));
    let column_hits = (0..h).filter(|&y| lit(w / 2, y)).count();
    let row_hits = (0..w).filter(|&x| lit(x, h / 2)).count();
    assert!(
        column_hits >= 2 && row_hits >= 2,
        "{column_hits} / {row_hits}"
    );
    assert!((0..h / 2).any(|y| lit(w / 2, y)) && (h / 2..h).any(|y| lit(w / 2, y)));
}

#[test]
fn draw_preview_generates_real_gcode_and_confirm_send_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(laser_runtime::MemoryStore::new());
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
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
    .with_store(store.clone())
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir.clone()])
    .with_transport_factory(fake_transport_factory(200_000));

    let dummy_png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    let preview_body = serde_json::json!({
        "image_data_url": dummy_png,
        "material": "椴木",
        "thickness_mm": 3.0,
        "mode": "raster",
        "scene_json": {"type": "excalidraw", "version": 2}
    })
    .to_string();

    let request = format!(
        "POST /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        preview_body.len(),
        preview_body
    );
    let response = service.handle_http(request.as_bytes());
    let body_start = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let json: serde_json::Value = serde_json::from_slice(&response[body_start..]).unwrap();
    assert_eq!(json["success"], true);
    let wf_id = json["result"]["workflow_id"].as_str().unwrap();
    let gcode_file = json["result"]["artifacts"]["gcode_file"].as_str().unwrap();
    let gcode_url = json["result"]["web"]["gcode_download_url"]
        .as_str()
        .unwrap();
    assert!(gen_dir.join(gcode_file).exists(), "物理 G-code 必须落盘");
    assert!(gcode_url.contains("download=1"));

    // 测试下载 Content-Disposition
    let download_req = format!("GET {gcode_url} HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let dl_res = service.handle_http(download_req.as_bytes());
    let dl_str = String::from_utf8_lossy(&dl_res);
    assert!(dl_str.starts_with("HTTP/1.1 200 OK"));
    assert!(dl_str.contains("Content-Disposition: attachment; filename="));

    // 测试 confirm_send 闭环
    let send_body = serde_json::json!({
        "action": "confirm_send",
        "workflow_id": wf_id,
        "confirmed": true,
        "connection_mode": "network",
        "network_host": "127.0.0.1",
        "network_telnet_port": 23
    })
    .to_string();
    let send_req = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        send_body.len(),
        send_body
    );
    let send_res = service.handle_http(send_req.as_bytes());
    let send_start = send_res.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let send_json: serde_json::Value = serde_json::from_slice(&send_res[send_start..]).unwrap();
    assert_eq!(send_json["success"], true);
    assert_eq!(send_json["result"]["status"], "sending");
}

/// 200×100 像素、中间一块黑色矩形的 PNG data URL。
fn draw_test_png() -> String {
    use base64::Engine;
    let mut img = image::GrayImage::from_pixel(200, 100, image::Luma([255]));
    for y in 25..75 {
        for x in 50..150 {
            img.put_pixel(x, y, image::Luma([0]));
        }
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn gcode_max_xy(gcode: &str) -> (f64, f64) {
    let mut max = (0.0_f64, 0.0_f64);
    for line in gcode
        .lines()
        .filter(|line| line.starts_with("G0") || line.starts_with("G1"))
    {
        for word in line.split_whitespace() {
            if let Some(value) = word.strip_prefix('X').and_then(|v| v.parse::<f64>().ok()) {
                max.0 = max.0.max(value);
            }
            if let Some(value) = word.strip_prefix('Y').and_then(|v| v.parse::<f64>().ok()) {
                max.1 = max.1.max(value);
            }
        }
    }
    max
}

#[test]
fn draw_natural_size_is_fitted_into_the_machine_and_explicit_size_is_not() {
    // 1500 × 100 像素按 0.1 mm 是 150 × 10 mm，可放范围 98 × 100 → 等比缩到 98 × 6.53
    let (w, h, fitted) =
        draw_target_size_mm(1500, 100, None, None, (98.0, 100.0), (100.0, 100.0)).unwrap();
    assert!(
        fitted && (w - 98.0).abs() < 1e-9 && (h - 98.0 / 15.0).abs() < 1e-9,
        "{w} {h}"
    );
    let (w, _, fitted) =
        draw_target_size_mm(200, 100, None, None, (98.0, 100.0), (100.0, 100.0)).unwrap();
    assert!(!fitted && (w - 20.0).abs() < 1e-9);
    assert!(
        draw_target_size_mm(200, 100, Some(99.0), None, (98.0, 100.0), (100.0, 100.0)).is_err()
    );
}

/// 画板按填写的宽 / 高出成品（锁定比例）；不填按 0.1 mm/像素；超出工作区拒绝；轮廓模式真的描边（2026-10-14）。
#[test]
fn draw_preview_honors_target_size_and_outline_mode() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let png = draw_test_png();
    let preview = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"image_data_url": png, "material": "椴木", "thickness_mm": 3.0, "mode": "raster"});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        post_json(&service, "/api/draw/preview", &body)
    };
    let gcode_of = |response: &serde_json::Value| {
        std::fs::read_to_string(
            temp.path().join(
                response["result"]["artifacts"]["gcode_file"]
                    .as_str()
                    .unwrap(),
            ),
        )
        .unwrap()
    };

    let natural = preview(serde_json::json!({}));
    assert_eq!(natural["success"], true, "{natural}");
    assert_eq!(natural["result"]["artifacts"]["width_mm"], 20.0);
    assert_eq!(natural["result"]["artifacts"]["height_mm"], 10.0);

    let sized = preview(serde_json::json!({"width_mm": 50, "height_mm": ""}));
    assert_eq!(sized["result"]["artifacts"]["width_mm"], 50.0, "{sized}");
    assert_eq!(sized["result"]["artifacts"]["height_mm"], 25.0);
    let (max_x, max_y) = gcode_max_xy(&gcode_of(&sized));
    // 黑块占中间一半：出光段约在 12.5..37.5 × 6.25..18.75 mm 内，X 另有过扫
    assert!(
        (36.0..=40.0).contains(&max_x) && (17.5..=20.0).contains(&max_y),
        "光栅没有按 50 mm 缩放（含 1 mm 过扫）: {max_x} {max_y}"
    );

    let boxed = preview(serde_json::json!({"width_mm": 30, "height_mm": 30}));
    assert_eq!(
        boxed["result"]["artifacts"]["width_mm"], 30.0,
        "锁定比例放进框内: {boxed}"
    );
    assert_eq!(boxed["result"]["artifacts"]["height_mm"], 15.0);

    // 2026-11-06 起按机器幅面（默认 100 mm，光栅宽度另留两侧 1 mm 过扫描）而不是 400 mm 画布
    let too_big = preview(serde_json::json!({"width_mm": 120}));
    assert_eq!(too_big["success"], false);
    assert!(too_big.to_string().contains("超出工作区"), "{too_big}");
    assert!(too_big.to_string().contains("100 × 100"), "{too_big}");
    assert_eq!(
        preview(serde_json::json!({"width_mm": 99}))["success"],
        false,
        "光栅宽度须留出过扫描"
    );
    assert_eq!(
        preview(serde_json::json!({"width_mm": 99, "mode": "outline"}))["success"],
        true,
        "轮廓模式无过扫描"
    );
    assert_eq!(natural["result"]["artifacts"]["auto_fitted"], false);

    let outline = preview(serde_json::json!({"mode": "outline", "width_mm": 40}));
    assert_eq!(outline["success"], true, "{outline}");
    assert_eq!(outline["result"]["artifacts"]["mode"], "outline");
    let gcode = gcode_of(&outline);
    let cuts = gcode.lines().filter(|line| line.starts_with("G1")).count();
    assert!(
        (4..=40).contains(&cuts),
        "轮廓应是少量矢量线段，不是逐行扫描: {cuts} 行"
    );
    let (max_x, max_y) = gcode_max_xy(&gcode);
    assert!(
        (29.0..=31.0).contains(&max_x) && (14.0..=16.0).contains(&max_y),
        "轮廓没有按 40 mm 缩放: {max_x} {max_y}"
    );
}

/// 2026-11-16：画板矢量模式直接用场景里的图形规划路线——手绘笔画按中心线一笔、不闭合，填充色用矢量排线填满；
/// 场景里有插入的图片时改用光栅并说明原因。
#[test]
fn draw_vector_mode_plans_scene_shapes_and_falls_back_for_images() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let png = draw_test_png();
    let scene = serde_json::json!({"elements": [
        {"type": "freedraw", "x": 0, "y": 0, "points": [[0, 0], [40, 20], [80, 0]], "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 2},
        {"type": "rectangle", "x": 100, "y": 0, "width": 40, "height": 40, "strokeColor": "#1e1e1e", "backgroundColor": "#ffc9c9", "strokeWidth": 2},
    ]});
    let body = serde_json::json!({"image_data_url": png, "material": "椴木", "thickness_mm": 3.0, "mode": "vector", "width_mm": 40, "scene_json": scene});
    let vector = post_json(&service, "/api/draw/preview", &body);
    assert_eq!(vector["success"], true, "{vector}");
    let artifacts = &vector["result"]["artifacts"];
    assert_eq!(artifacts["mode"], "vector", "{artifacts}");
    assert!(artifacts.get("vector_note").is_none());
    assert!(
        artifacts["path_optimization"]["strategy"].is_string(),
        "{artifacts}"
    );
    let gcode =
        std::fs::read_to_string(temp.path().join(artifacts["gcode_file"].as_str().unwrap()))
            .unwrap();
    let burns = gcode.lines().filter(|line| line.starts_with("G1")).count();
    assert!(burns > 20, "矩形填充应有大量排线：{burns}");
    let (max_x, max_y) = gcode_max_xy(&gcode);
    assert!(
        max_x <= 40.0 + 1e-6 && max_y <= 40.0 + 1e-6,
        "{max_x} {max_y}"
    );
    // 填充有过扫描（G1 S0 关光滑行），且整段运动不往激光头起点的负方向走、不越出成品范围
    assert!(
        gcode
            .lines()
            .any(|l| l.starts_with("G1 ") && l.ends_with(" S0")),
        "矩形填充应有过扫描"
    );
    let extents = laser_core::gcode::motion_extents(&gcode).unwrap().unwrap();
    assert!(
        extents.min_x >= -1e-6 && extents.min_y >= -1e-6,
        "{extents:?}"
    );
    assert_eq!(
        artifacts["fill_options"]["overscan_auto"], true,
        "{artifacts}"
    );
    // 笔画是开放线段：手绘那一笔的两个端点之间不会补一条闭合线（笔画只有 2 段）
    let stroke_moves = gcode
        .split("S0\n")
        .filter(|chunk| chunk.matches("G1 ").count() == 2)
        .count();
    assert!(stroke_moves >= 1, "手绘笔画按中心线两段刻、不闭合");

    // 场景里有图片：改用光栅并写明原因
    let mut with_image = body.clone();
    with_image["scene_json"]["elements"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"type": "image", "x": 0, "y": 0, "width": 10, "height": 10}));
    let fallback = post_json(&service, "/api/draw/preview", &with_image);
    assert_eq!(fallback["success"], true, "{fallback}");
    assert_eq!(fallback["result"]["artifacts"]["mode"], "raster");
    assert!(
        fallback["result"]["artifacts"]["vector_note"]
            .as_str()
            .unwrap()
            .contains("图片")
    );
}

/// 2026-11-16 只刻选中的图形（对齐 LightBurn“只加工选中图形”，不用选区原点）：
/// 选中的部分刻在它在整幅画里的位置；只刻矢量；选中图片或空选区报错。
#[test]
fn draw_vector_can_burn_only_the_selected_shapes_at_their_place_in_the_whole_drawing() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let scene = serde_json::json!({"elements": [
        {"id": "left", "type": "rectangle", "x": 0, "y": 0, "width": 40, "height": 40, "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 2},
        {"id": "right", "type": "rectangle", "x": 160, "y": 0, "width": 40, "height": 40, "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 2},
        {"id": "photo", "type": "image", "x": 300, "y": 300, "width": 10, "height": 10},
    ]});
    let post = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"image_data_url": draw_test_png(), "material": "椴木",
            "thickness_mm": 3.0, "mode": "vector", "width_mm": 60, "height_mm": 60, "scene_json": scene});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/draw/preview", &body)
    };
    // 出光范围（不含从起点出发的空走）：取出光点凸包的 x 范围
    let burn_x = |response: &serde_json::Value| {
        let hull = response["result"]["artifacts"]["frame_hull_mm"]
            .as_array()
            .unwrap()
            .clone();
        let xs: Vec<f64> = hull.iter().map(|p| p[0].as_f64().unwrap()).collect();
        (
            xs.iter().cloned().fold(f64::INFINITY, f64::min),
            xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        )
    };
    // 不选图片：只刻右边那个方块，位置在整幅（两个方块）排版里的右侧
    let right = post(serde_json::json!({"selected_element_ids": ["right"]}));
    assert_eq!(right["success"], true, "{right}");
    let artifacts = &right["result"]["artifacts"];
    assert_eq!(artifacts["mode"], "vector", "{artifacts}");
    assert_eq!(artifacts["selected_only"], true);
    assert!(
        artifacts["selection_note"]
            .as_str()
            .unwrap()
            .contains("1 个")
    );
    let (min_x, _) = burn_x(&right);
    assert!(
        min_x > 30.0,
        "选中的右侧方块应在整幅画的右半边，而不是被挪到左下角：{min_x}"
    );
    let left = post(serde_json::json!({"selected_element_ids": ["left"]}));
    assert!(burn_x(&left).1 < 30.0);

    for (bad, why) in [
        (
            serde_json::json!({"selected_element_ids": ["photo"]}),
            "图片",
        ),
        (
            serde_json::json!({"selected_element_ids": ["nothing"]}),
            "没有可刻",
        ),
        (
            serde_json::json!({"selected_element_ids": []}),
            "selected_element_ids",
        ),
        (
            serde_json::json!({"selected_element_ids": ["right"], "mode": "raster"}),
            "矢量",
        ),
    ] {
        let response = post(bad.clone());
        assert_eq!(response["success"], false, "{bad} → {response}");
        assert!(
            response["result"].as_str().unwrap_or("").contains(why),
            "{bad} → {response}"
        );
    }
}

#[test]
fn material_safety_gate_rejects_pvc_and_unknown_plastic() {
    assert!(validate_material_safety("PVC").unwrap_err().contains("PVC"));
    assert!(
        validate_material_safety("塑料")
            .unwrap_err()
            .contains("成分不明确")
    );
    assert!(validate_material_safety("椴木").is_ok());
}

#[test]
fn workflow_preview_applies_material_safety_gate_with_and_without_store() {
    for use_store in [true, false] {
        let mut service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        );
        if use_store {
            service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
        }

        let blocked = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "preview",
                "source_type": "text",
                "text": "MOSS",
                "material": "PVC",
                "thickness_mm": 3
            }),
        );
        assert_eq!(blocked["success"], false, "{blocked}");
        assert_eq!(blocked["detail"]["error_code"], "material_safety_blocked");
    }
}

#[test]
fn job_travel_check_compares_extents_with_firmware_travel() {
    let limits = laser_device::MachineLimits {
        x_travel_mm: Some(100.0),
        y_travel_mm: Some(100.0),
        ..Default::default()
    };
    assert!(check_job_fits_machine("G92 X0 Y0\nG1 X100 Y60 S300", &limits).is_ok());
    let err = check_job_fits_machine("G92 X0 Y0\nG1 X200 Y60 S300", &limits).unwrap_err();
    assert!(
        err.contains("200.0 × 60.0") && err.contains("100 × 100"),
        "{err}"
    );
    // 固件没报告行程、或文件无法计算范围时交给固件软限位
    assert!(check_job_fits_machine("G1 X500", &laser_device::MachineLimits::default()).is_ok());
    assert!(check_job_fits_machine("G28\nG1 X500", &limits).is_ok());
    // 只知道 X 行程时仍检查 X
    let x_only = laser_device::MachineLimits {
        x_travel_mm: Some(50.0),
        ..Default::default()
    };
    assert!(check_job_fits_machine("G1 X60 Y500", &x_only).is_err());
}

/// 2026-11-06 严重缺陷修复：大图不再按 0.1 mm/像素生成超出幅面的任务；
/// 显式超大尺寸拒绝；超出固件行程的 G-code 在发送前被拒、不写入任何 G-code 行。
#[test]
fn oversized_image_is_fitted_and_oversized_gcode_is_refused_before_sending() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    // 2000 × 500 像素的渐变照片：按 0.1 mm/像素是 200 × 50 mm
    let photo = image::GrayImage::from_fn(2000, 500, |x, _| image::Luma([(x * 255 / 1999) as u8]));
    photo.save(gen_dir.join("wide.png")).unwrap();
    std::fs::write(
        gen_dir.join("wide.gcode"),
        "G21\nG90\nG92 X0 Y0\nM4 S0\nG1 X150 Y10 F1000 S200\nM5\n",
    )
    .unwrap();
    let settings = laser_runtime::LaserSettings {
        default_connection_mode: "network".into(),
        engraving_mode: "raster".into(),
        web_host: "127.0.0.1".into(),
        web_port: 8766,
        material_params_file: None,
    };
    let service = ApiService::new(settings, laser_contracts::builtin_material_library())
        .with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()))
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir.clone()])
        .with_transport_factory(fake_transport_factory_with_settings(
            10_000,
            &["$130=100.000", "$131=100.000"],
        ));

    let fitted = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "preview", "source_type": "image", "image_file": "wide.png", "material": "椴木", "thickness_mm": 3
        }),
    );
    assert_eq!(fitted["success"], true, "{fitted}");
    let artifacts = &fitted["result"]["artifacts"];
    assert_eq!(artifacts["auto_fitted"], true);
    // 100 mm 幅面减两侧各 1 mm 过扫描 → 98 × 24.5 mm
    assert_eq!(artifacts["width_mm"].as_f64().unwrap(), 98.0);
    assert_eq!(artifacts["height_mm"].as_f64().unwrap(), 24.5);
    assert_eq!(artifacts["image_mode"], "dither");
    let gcode =
        std::fs::read_to_string(gen_dir.join(artifacts["gcode_file"].as_str().unwrap())).unwrap();
    let extents = laser_core::gcode::motion_extents(&gcode).unwrap().unwrap();
    assert!(
        extents.width() <= 100.0 + 1e-6,
        "生成的刀路必须在幅面内：{}",
        extents.width()
    );

    let sized = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "preview", "source_type": "image", "image_file": "wide.png", "material": "椴木", "thickness_mm": 3, "width_mm": 40
        }),
    );
    assert_eq!(
        sized["result"]["artifacts"]["width_mm"]
            .as_f64()
            .unwrap()
            .round(),
        40.0,
        "{sized}"
    );
    let refused = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "preview", "source_type": "image", "image_file": "wide.png", "material": "椴木", "thickness_mm": 3, "width_mm": 150
        }),
    );
    assert_eq!(refused["success"], false, "{refused}");

    let preview = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "preview", "source_type": "gcode", "gcode_file": "wide.gcode", "material": "椴木", "thickness_mm": 3
        }),
    );
    assert_eq!(preview["success"], true, "{preview}");
    let send = post_json(
        &service,
        "/api/workflow",
        &serde_json::json!({
            "action": "confirm_send", "workflow_id": preview["result"]["workflow_id"], "confirmed": true,
            "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23
        }),
    );
    assert_eq!(send["success"], false, "{send}");
    assert_eq!(send["detail"]["error_code"], "job_exceeds_travel", "{send}");
}

/// 2026-11-16（技术书 r38）：图片模式对齐 LightBurn Image Mode——新字段生效、在产物中回显、非法值中文 400。
#[test]
fn image_preview_supports_lightburn_image_modes() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    image::GrayImage::from_fn(200, 100, |x, _| image::Luma([(x * 255 / 199) as u8]))
        .save(gen_dir.join("ramp.png"))
        .unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()))
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir.clone()]);
    let preview = |extra: serde_json::Value| {
        let mut body = serde_json::json!({
            "action": "preview", "source_type": "image", "image_file": "ramp.png",
            "material": "椴木", "thickness_mm": 3,
            "manual_params": {"power_percent": 60, "feed_rate": 1500, "passes": 1}
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/workflow", &body)
    };
    let gcode_of = |res: &serde_json::Value| {
        std::fs::read_to_string(
            gen_dir.join(res["result"]["artifacts"]["gcode_file"].as_str().unwrap()),
        )
        .unwrap()
    };
    let s_values = |gcode: &str| -> std::collections::BTreeSet<i64> {
        gcode
            .lines()
            .filter(|l| l.starts_with("G1 "))
            .filter_map(|l| l.split_whitespace().find_map(|t| t.strip_prefix('S')))
            .filter_map(|s| s.parse().ok())
            .collect()
    };

    // 默认请求：旧行为不变，只多回显三个关闭的开关
    let plain = preview(serde_json::json!({}));
    assert_eq!(plain["success"], true, "{plain}");
    let artifacts = &plain["result"]["artifacts"];
    assert_eq!(artifacts["image_mode"], "dither");
    assert_eq!(artifacts["negative"], false);
    assert_eq!(artifacts["pass_through"], false);
    assert_eq!(artifacts["dot_width_mm"], 0.0);
    assert!(artifacts.get("gray_levels").is_none() && artifacts.get("halftone_cell_mm").is_none());
    assert_eq!(s_values(&gcode_of(&plain)), [0, 600].into());

    // 灰度：S 值有界、级数受限、最大功率 = 本次功率
    let gray = preview(serde_json::json!({
        "image_mode": "grayscale", "gray_levels": 8, "gray_min_power_percent": 20
    }));
    assert_eq!(gray["success"], true, "{gray}");
    let artifacts = &gray["result"]["artifacts"];
    assert_eq!(artifacts["image_mode"], "grayscale");
    assert_eq!(artifacts["gray_levels"], 8);
    assert_eq!(artifacts["gray_min_power"], 200);
    assert_eq!(artifacts["gray_max_power"], 600);
    let gcode = gcode_of(&gray);
    assert!(gcode.contains("; Raster mode: grayscale variable power (8 levels)"));
    let powers = s_values(&gcode);
    assert!(powers.len() <= 8 && powers.len() > 3, "{powers:?}");
    assert!(powers.iter().all(|p| *p == 0 || (200..=600).contains(p)));
    assert!(powers.contains(&600));

    // 网点：LPI 换算成网格并回显；反相、点宽补偿、直通同时回显
    let halftone = preview(serde_json::json!({
        "image_mode": "halftone", "halftone_lpi": 25.4, "halftone_angle_deg": 30,
        "negative": true, "dot_width_mm": 0.04
    }));
    assert_eq!(halftone["success"], true, "{halftone}");
    let artifacts = &halftone["result"]["artifacts"];
    assert_eq!(artifacts["halftone_cell_mm"], 1.0);
    assert_eq!(artifacts["halftone_angle_deg"], 30.0);
    assert_eq!(artifacts["negative"], true);
    assert_eq!(artifacts["dot_width_mm"], 0.04);
    let through = preview(serde_json::json!({
        "image_mode": "jarvis", "pass_through": true, "width_mm": 50
    }));
    assert_eq!(through["success"], true, "{through}");
    assert_eq!(through["result"]["artifacts"]["pass_through"], true);
    assert_eq!(through["result"]["artifacts"]["pixel_size_mm"], 0.25);
    // 质量预设（r41）：行间距 = 材料推荐线距 × line_interval_scale，并回显倍数；不给时不回显
    assert!(
        plain["result"]["artifacts"]
            .get("line_interval_scale")
            .is_none()
    );
    let base_pixel = plain["result"]["artifacts"]["pixel_size_mm"]
        .as_f64()
        .unwrap();
    let fast = preview(serde_json::json!({ "line_interval_scale": 1.5 }));
    assert_eq!(fast["success"], true, "{fast}");
    assert_eq!(fast["result"]["artifacts"]["line_interval_scale"], 1.5);
    let fast_pixel = fast["result"]["artifacts"]["pixel_size_mm"]
        .as_f64()
        .unwrap();
    assert!(
        (fast_pixel - base_pixel * 1.5).abs() < 1e-9,
        "{base_pixel} → {fast_pixel}"
    );
    for mode in ["stucki", "atkinson", "ordered", "newsprint", "sketch"] {
        let res = preview(serde_json::json!({ "image_mode": mode }));
        assert_eq!(res["success"], true, "{mode}: {res}");
        assert_eq!(res["result"]["artifacts"]["image_mode"], mode);
    }

    // 非法输入：中文 400，不生成产物
    for (extra, needle) in [
        (
            serde_json::json!({"image_mode": "pencil"}),
            "image_mode 只能是",
        ),
        (
            serde_json::json!({"image_mode": "halftone", "halftone_lpi": 50, "halftone_cell_mm": 1}),
            "只能给一个",
        ),
        (
            serde_json::json!({"image_mode": "halftone", "halftone_cell_mm": 0.1}),
            "网点网格",
        ),
        (serde_json::json!({"dot_width_mm": 0.1}), "必须小于行间距"),
        (serde_json::json!({"dot_width_mm": -1}), "不能为负数"),
        (serde_json::json!({"negative": "yes"}), "true 或 false"),
        (
            serde_json::json!({"image_mode": "grayscale", "gray_min_power_percent": 60}),
            "必须小于本次最大功率",
        ),
        (
            serde_json::json!({"image_mode": "grayscale", "gray_levels": 100}),
            "gray_levels",
        ),
        (
            serde_json::json!({"pass_through": true, "width_mm": 5}),
            "直通模式",
        ),
        (
            serde_json::json!({"line_interval_scale": 5}),
            "line_interval_scale",
        ),
        (
            serde_json::json!({"line_interval_scale": 1, "pixel_size_mm": 0.1}),
            "只能给一个",
        ),
    ] {
        let res = preview(extra.clone());
        assert_eq!(res["success"], false, "{extra}: {res}");
        let message = res["result"].as_str().unwrap_or_default();
        assert!(message.contains(needle), "{extra}: {res}");
    }
}

#[test]
fn text_larger_than_the_machine_is_refused_at_preview() {
    let temp = tempfile::tempdir().unwrap();
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
    .with_generated_root(temp.path().to_path_buf());
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let small = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "AB", "font_path": font, "font_size_mm": 10, "material": "椴木", "thickness_mm": 3
        }),
    );
    assert_eq!(small["success"], true, "{small}");
    let huge = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "ABABABAB", "font_path": font, "font_size_mm": 40, "material": "椴木", "thickness_mm": 3
        }),
    );
    assert_eq!(huge["success"], false, "{huge}");
    assert!(
        huge["result"]
            .as_str()
            .unwrap_or_default()
            .contains("超出机器幅面"),
        "{huge}"
    );
}

/// n73：文字预览带这次临时指定的功率 / 速度 / 遍数，刀路就按它生成并在 actual_params 里如实显示；越界拒绝。
#[test]
fn text_preview_applies_manual_params_to_the_gcode() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let body = serde_json::json!({
        "text": "AB", "font_path": font, "material": "椴木", "thickness_mm": 3,
        "manual_params": {"power_percent": 37, "feed_rate": 900, "passes": 2}
    });
    let res = post_json(&service, "/api/text-task/generate", &body);
    assert_eq!(res["success"], true, "{res}");
    let params = &res["result"]["artifacts"]["actual_params"];
    assert_eq!(params["laser_max_power"].as_f64(), Some(370.0));
    assert_eq!(params["feed_rate"].as_f64(), Some(900.0));
    assert_eq!(params["passes"], 2);
    let gcode = std::fs::read_to_string(
        temp.path()
            .join(res["result"]["artifacts"]["gcode_file"].as_str().unwrap()),
    )
    .unwrap();
    assert!(gcode.contains("S370"), "刀路用指定功率");
    assert!(gcode.contains("F900"), "刀路用指定速度");
    assert_eq!(
        res["result"]["input"]["manual_params"]["power_percent"], 37,
        "任务记录里留着这次的手动参数"
    );
    let bad = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({"text": "AB", "font_path": font, "material": "椴木", "thickness_mm": 3,
                "manual_params": {"power_percent": 150}}),
    );
    assert_eq!(bad["success"], false, "{bad}");
}

/// 2026-11-06：文字默认描边、激光头对准中心；给木牌尺寸自动适配字号；填充用材料库 raster 参数。
#[test]
fn text_board_fill_and_origin_options_reach_the_engine() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let base = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"text": "ABAB", "font_path": font, "material": "椴木", "thickness_mm": 3});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/text-task/generate", &body)
    };
    let default = base(serde_json::json!({}));
    let artifacts = &default["result"]["artifacts"];
    assert_eq!(artifacts["text_fill"], "outline", "{default}");
    assert_eq!(artifacts["origin"], "center");
    assert_eq!(artifacts["origin_note"], "开刻前把激光头移到要刻的位置中心");
    let frame = &artifacts["frame_mm"];
    assert!((frame["min_x"].as_f64().unwrap() + frame["max_x"].as_f64().unwrap()).abs() < 1e-6);

    let board = base(serde_json::json!({"width_mm": 80, "height_mm": 30, "text_fill": "fill"}));
    let artifacts = &board["result"]["artifacts"];
    assert_eq!(artifacts["text_fill"], "fill", "{board}");
    assert!(artifacts["width_mm"].as_f64().unwrap() <= 76.0 + 1e-6);
    assert!(
        artifacts["font_size_mm"].as_f64().unwrap() > 10.0,
        "未指定字号时应自动放大到木牌"
    );
    assert_eq!(artifacts["origin_note"], "开刻前把激光头移到木牌中心");
    assert_eq!(
        artifacts["actual_params"]["laser_max_power"].as_f64(),
        Some(420.0)
    );

    let fixed = base(serde_json::json!({"width_mm": 30, "font_size_mm": 20}));
    assert_eq!(fixed["success"], false, "{fixed}");
    assert!(
        fixed["result"].as_str().unwrap().contains("放不进"),
        "{fixed}"
    );
    let bad = base(serde_json::json!({"text_fill": "solid"}));
    assert_eq!(bad["success"], false, "{bad}");
    let corner = base(serde_json::json!({"origin": "lower_left"}));
    assert_eq!(
        corner["result"]["artifacts"]["frame_mm"]["min_x"]
            .as_f64()
            .unwrap(),
        0.0,
        "{corner}"
    );
}

/// 2026-11-16：文字填充的扫描设置对齐 LightBurn，经 `fill_options` / `path_options` 传入并在结果里回显。
#[test]
fn text_fill_options_follow_lightburn_and_are_echoed() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let base = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"text": "AB", "font_path": font, "material": "椴木",
            "thickness_mm": 3, "font_size_mm": 10, "text_fill": "fill"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/text-task/generate", &body)
    };
    let gcode_of = |response: &serde_json::Value| {
        std::fs::read_to_string(
            temp.path().join(
                response["result"]["artifacts"]["gcode_file"]
                    .as_str()
                    .unwrap(),
            ),
        )
        .unwrap()
    };

    // 默认：一次填完、横扫、双向、过扫描按速度自动算（至少 1 mm），以 G1 S0 匀速滑行
    let default = base(serde_json::json!({}));
    assert_eq!(default["success"], true, "{default}");
    let fill = &default["result"]["artifacts"]["fill_options"];
    assert_eq!(fill["grouping"], "all", "{fill}");
    assert_eq!(fill["bidirectional"], true);
    assert_eq!(fill["overscan_auto"], true);
    assert!(fill["overscan_mm"].as_f64().unwrap() >= 1.0, "{fill}");
    assert!(
        gcode_of(&default)
            .lines()
            .any(|l| l.starts_with("G1 ") && l.ends_with(" S0")),
        "过扫描用 G1 S0"
    );

    // 手动：逐块、关过扫描、竖扫、交叉、单向、偏移，不去重
    let custom = base(serde_json::json!({
        "fill_options": {"grouping": "individual", "overscan_mm": 0, "angle_deg": 90,
            "cross_hatch": true, "bidirectional": false, "scan_offset_mm": 0.1},
        "path_options": {"prefer_corners": false, "remove_overlaps": false}
    }));
    assert_eq!(custom["success"], true, "{custom}");
    let fill = &custom["result"]["artifacts"]["fill_options"];
    assert_eq!(fill["grouping"], "individual", "{fill}");
    assert_eq!(fill["overscan_mm"], 0.0);
    assert_eq!(fill["overscan_auto"], false);
    assert_eq!(fill["cross_hatch"], true);
    assert_eq!(fill["bidirectional"], false);
    assert_eq!(custom["result"]["artifacts"]["removed_duplicates"], 0);
    assert!(
        !gcode_of(&custom)
            .lines()
            .any(|l| l.starts_with("G1 ") && l.ends_with(" S0")),
        "关掉过扫描后逐块填充没有关光滑行"
    );

    // 描边没有填充设置
    let outline = base(serde_json::json!({"text_fill": "outline"}));
    assert!(outline["result"]["artifacts"]["fill_options"].is_null());

    for bad in [
        serde_json::json!({"fill_options": {"grouping": "zigzag"}}),
        serde_json::json!({"fill_options": {"overscan_mm": 50}}),
        serde_json::json!({"fill_options": {"scan_offset_mm": 5}}),
        serde_json::json!({"fill_options": {"bidirectional": "yes"}}),
        serde_json::json!({"fill_options": "fast"}),
        serde_json::json!({"path_options": {"prefer_corners": 1}}),
    ] {
        let response = base(bad.clone());
        assert_eq!(response["success"], false, "{bad} → {response}");
    }
}

/// 2026-11-16：外框切割对齐 LightBurn Cut 设置：`cut_options`（切缝补偿、桥位、引线、过切）默认全关，
/// 生效时回显实际设置与统计；雕刻任务与画板拒绝 `cut_options`，非法值返回中文错误。
#[test]
fn text_cut_options_follow_lightburn_and_are_echoed() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let base = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"text": "AB", "font_path": font, "material": "椴木",
            "thickness_mm": 3, "font_size_mm": 10, "laser_mode": "cut", "engraving_mode": "cut"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/text-task/generate", &body)
    };
    let gcode_of = |response: &serde_json::Value| {
        std::fs::read_to_string(
            temp.path().join(
                response["result"]["artifacts"]["gcode_file"]
                    .as_str()
                    .unwrap(),
            ),
        )
        .unwrap()
    };
    let coasts = |gcode: &str| {
        gcode
            .lines()
            .filter(|l| l.starts_with("G1 ") && l.contains(" S0"))
            .count()
    };

    // 默认全关：回显 active=false，刀路里没有关光滑行
    let plain = base(serde_json::json!({}));
    assert_eq!(plain["success"], true, "{plain}");
    let echo = &plain["result"]["artifacts"]["cut_options"];
    assert_eq!(echo["active"], false, "{echo}");
    assert_eq!(echo["kerf_mm"], 0.0);
    assert_eq!(echo["tab_mode"], "off");
    assert_eq!(coasts(&gcode_of(&plain)), 0);
    let plain_again = base(serde_json::json!({"cut_options": {}}));
    assert_eq!(
        gcode_of(&plain),
        gcode_of(&plain_again),
        "空 cut_options 与不传完全相同"
    );

    // 切缝补偿 + 桥位 + 引入线 + 过切
    let tuned = base(serde_json::json!({"cut_options": {
        "kerf_mm": 0.1, "tab_count": 2, "tab_width_mm": 0.4,
        "lead_in_mm": 1, "lead_angle_deg": 90, "overcut_mm": 0.5
    }}));
    assert_eq!(tuned["success"], true, "{tuned}");
    let echo = &tuned["result"]["artifacts"]["cut_options"];
    assert_eq!(echo["active"], true, "{echo}");
    assert_eq!(echo["kerf_mm"], 0.1);
    assert_eq!(echo["tab_mode"], "count");
    assert_eq!(echo["tab_count"], 2);
    assert_eq!(echo["tabs_skip_inner"], true);
    let report = &echo["report"];
    assert!(
        report["kerf_contours_after"].as_u64().unwrap() >= 2,
        "{report}"
    );
    // 只有外框加桥位（A、B 各一个外框，内孔跳过）
    assert_eq!(report["tabs"], 4, "{report}");
    assert!(report["lead_ins"].as_u64().unwrap() >= 1, "{report}");
    assert!(report["overcut_mm"].as_f64().unwrap() > 0.0);
    assert!(coasts(&gcode_of(&tuned)) >= 4, "桥位是 G1 S0 关光滑行");

    // 按间距放桥位、不跳过内部图形
    let spaced = base(serde_json::json!({"cut_options": {
        "tab_spacing_mm": 20, "tabs_skip_inner": false
    }}));
    assert_eq!(spaced["success"], true, "{spaced}");
    let echo = &spaced["result"]["artifacts"]["cut_options"];
    assert_eq!(echo["tab_mode"], "spacing");
    assert_eq!(echo["tab_spacing_mm"], 20.0);
    // 每个轮廓（含内孔）至少一个
    assert!(
        echo["report"]["tabs"].as_u64().unwrap() >= echo["report"]["contours"].as_u64().unwrap(),
        "{echo}"
    );

    // 雕刻任务（非切割）不接受切割设置；切割但没有设置时照常
    let engrave = base(
        serde_json::json!({"laser_mode": "engrave", "engraving_mode": "outline",
        "cut_options": {"kerf_mm": 0.1}}),
    );
    assert_eq!(engrave["success"], false, "{engrave}");
    assert!(
        engrave["error"].to_string().contains("外框切割")
            || engrave.to_string().contains("外框切割"),
        "{engrave}"
    );
    let engrave_plain =
        base(serde_json::json!({"laser_mode": "engrave", "engraving_mode": "outline"}));
    assert!(engrave_plain["result"]["artifacts"]["cut_options"].is_null());

    for bad in [
        serde_json::json!({"cut_options": "tight"}),
        serde_json::json!({"cut_options": {"kerf_mm": 3}}),
        serde_json::json!({"cut_options": {"kerf_mm": "abc"}}),
        serde_json::json!({"cut_options": {"tab_count": 2.5}}),
        serde_json::json!({"cut_options": {"tab_count": 200}}),
        serde_json::json!({"cut_options": {"tab_count": 2, "tab_spacing_mm": 10}}),
        serde_json::json!({"cut_options": {"tab_spacing_mm": 0.5}}),
        serde_json::json!({"cut_options": {"tab_count": 2, "tab_width_mm": 20}}),
        serde_json::json!({"cut_options": {"tabs_skip_inner": "yes"}}),
        serde_json::json!({"cut_options": {"lead_in_mm": 11}}),
        serde_json::json!({"cut_options": {"lead_in_mm": 1, "lead_angle_deg": 0}}),
        serde_json::json!({"cut_options": {"overcut_mm": -1}}),
        // 内缩 2 mm 后 3 mm 字的笔画全部消失
        serde_json::json!({"font_size_mm": 3, "cut_options": {"kerf_mm": -2}}),
    ] {
        let response = base(bad.clone());
        assert_eq!(response["success"], false, "{bad} → {response}");
    }
}

/// 2026-11-16：画板只生成雕刻任务，不接受切割设置。
#[test]
fn draw_rejects_cut_options() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let body = serde_json::json!({"image_data_url": draw_test_png(), "material": "椴木",
        "thickness_mm": 3.0, "mode": "raster", "cut_options": {"kerf_mm": 0.1}});
    let response = post_json(&service, "/api/draw/preview", &body);
    assert_eq!(response["success"], false, "{response}");
    assert!(response.to_string().contains("cut_options"), "{response}");
}

/// 2026-11-16：双向扫描偏移按“速度 → 偏移”表（LightBurn Scanning Offset Adjustment）在加工速度处插值。
#[test]
fn scan_offset_table_is_interpolated_at_the_job_speed_and_echoed() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let base = |fill: serde_json::Value| {
        let body = serde_json::json!({"text": "AB", "font_path": font, "material": "椴木",
            "thickness_mm": 3, "font_size_mm": 10, "text_fill": "fill",
            "manual_params": {"feed_rate": 2000}, "fill_options": fill});
        post_json(&service, "/api/text-task/generate", &body)
    };
    // 乱序给表：排好序回显，2000 mm/min 处插值 0.1 + (0.3 − 0.1) × 0.5 = 0.2，覆盖固定偏移
    let table = base(serde_json::json!({"scan_offset_mm": 0.05,
        "scan_offset_table": [[3000, 0.3], [1000, 0.1]]}));
    assert_eq!(table["success"], true, "{table}");
    let fill = &table["result"]["artifacts"]["fill_options"];
    assert!(
        (fill["scan_offset_mm"].as_f64().unwrap() - 0.2).abs() < 1e-9,
        "{fill}"
    );
    assert_eq!(fill["scan_offset_source"], "table");
    assert_eq!(
        fill["scan_offset_table"],
        serde_json::json!([[1000.0, 0.1], [3000.0, 0.3]])
    );
    // 速度超出表的范围取端点值
    let clamped = base(serde_json::json!({"scan_offset_table": [[500, 0.02], [1500, 0.08]]}));
    assert_eq!(
        clamped["result"]["artifacts"]["fill_options"]["scan_offset_mm"],
        0.08
    );
    // 不给表：固定偏移
    let fixed = base(serde_json::json!({"scan_offset_mm": 0.05}));
    let fill = &fixed["result"]["artifacts"]["fill_options"];
    assert_eq!(fill["scan_offset_source"], "fixed");
    assert_eq!(fill["scan_offset_mm"], 0.05);
    assert!(fill["scan_offset_table"].is_null());

    let mut too_many = Vec::new();
    for i in 0..33 {
        too_many.push(serde_json::json!([1000 + i * 10, 0.1]));
    }
    for bad in [
        serde_json::json!({"scan_offset_table": "fast"}),
        serde_json::json!({"scan_offset_table": [[1000, 3]]}),
        serde_json::json!({"scan_offset_table": [[0, 0.1]]}),
        serde_json::json!({"scan_offset_table": [[1000]]}),
        serde_json::json!({"scan_offset_table": [[1000, 0.1], [1000, 0.2]]}),
        serde_json::json!({"scan_offset_table": too_many}),
    ] {
        let response = base(bad.clone());
        assert_eq!(response["success"], false, "{bad} → {response}");
    }
}

/// 2026-11-16：工时按机器加速度估算（`machine_motion` 与握手 `machine_limits` 同键名），
/// 自动过扫描也跟着机器加速度走。
#[test]
fn time_estimate_and_auto_overscan_follow_machine_motion() {
    let temp = tempfile::tempdir().unwrap();
    let service = ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(temp.path().to_path_buf());
    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let base = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"text": "AB", "font_path": font, "material": "椴木",
            "thickness_mm": 3, "font_size_mm": 10, "text_fill": "fill"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/text-task/generate", &body)
    };
    let default = base(serde_json::json!({}));
    let estimate = &default["result"]["artifacts"]["time_estimate"];
    assert_eq!(estimate["model"], "acceleration", "{estimate}");
    assert_eq!(estimate["acceleration_source"], "default");
    assert_eq!(estimate["acceleration_mm_s2"]["x"], 1000.0);
    // 橡皮筋走边框轮廓：出光点凸包，都在 frame_mm 范围内
    let artifacts = &default["result"]["artifacts"];
    let hull = artifacts["frame_hull_mm"]
        .as_array()
        .expect("文字应有外轮廓");
    assert!(hull.len() >= 3, "{hull:?}");
    let frame = &artifacts["frame_mm"];
    for point in hull {
        let (x, y) = (point[0].as_f64().unwrap(), point[1].as_f64().unwrap());
        assert!(
            x >= frame["min_x"].as_f64().unwrap() - 0.01
                && x <= frame["max_x"].as_f64().unwrap() + 0.01
        );
        assert!(
            y >= frame["min_y"].as_f64().unwrap() - 0.01
                && y <= frame["max_y"].as_f64().unwrap() + 0.01
        );
    }

    // 加速度低的机器：工时更长，自动过扫描更长
    let slow =
        base(serde_json::json!({"machine_motion": {"x_accel_mm_s2": 200, "y_accel_mm_s2": 200}}));
    assert_eq!(slow["success"], true, "{slow}");
    let slow_artifacts = &slow["result"]["artifacts"];
    assert_eq!(
        slow_artifacts["time_estimate"]["acceleration_source"],
        "machine"
    );
    assert!(
        slow_artifacts["time_estimate"]["estimated_seconds"]
            .as_f64()
            .unwrap()
            > estimate["estimated_seconds"].as_f64().unwrap(),
        "{slow_artifacts}"
    );
    assert!(
        slow_artifacts["fill_options"]["overscan_mm"]
            .as_f64()
            .unwrap()
            >= default["result"]["artifacts"]["fill_options"]["overscan_mm"]
                .as_f64()
                .unwrap(),
        "{slow_artifacts}"
    );
    // 填充设置里显式填的加速度优先于机器读到的
    let explicit = base(serde_json::json!({
        "machine_motion": {"x_accel_mm_s2": 200},
        "fill_options": {"acceleration_mm_s2": 5000}
    }));
    assert_eq!(
        explicit["result"]["artifacts"]["fill_options"]["overscan_mm"], 1.0,
        "{explicit}"
    );

    for bad in [
        serde_json::json!({"machine_motion": "fast"}),
        serde_json::json!({"machine_motion": {"x_accel_mm_s2": -5}}),
        serde_json::json!({"machine_motion": {"junction_deviation_mm": "a"}}),
    ] {
        let response = base(bad.clone());
        assert_eq!(response["success"], false, "{bad} → {response}");
    }
}

#[test]
fn text_generate_writes_real_gcode_and_preview_artifacts() {
    let font = std::path::PathBuf::from(r"C:\Windows\Fonts\arial.ttf");
    if !font.exists() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
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
    .with_generated_root(temp.path().to_path_buf());
    let body = serde_json::json!({
        "text": "A",
        "material": "椴木",
        "thickness_mm": 3,
        "font_path": font,
        "font_size_mm": 8
    })
    .to_string();
    let request = format!(
        "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let response = service.handle_http(request.as_bytes());
    let start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let response: serde_json::Value = serde_json::from_slice(&response[start..]).unwrap();
    assert_eq!(response["success"], true);
    let gcode = response["result"]["artifacts"]["gcode_file"]
        .as_str()
        .unwrap();
    let preview = response["result"]["artifacts"]["image_file"]
        .as_str()
        .unwrap();
    assert!(!std::path::Path::new(gcode).is_absolute());
    assert!(std::fs::metadata(temp.path().join(gcode)).unwrap().len() > 0);
    assert!(std::fs::metadata(temp.path().join(preview)).unwrap().len() > 0);
    assert!(
        std::fs::read_to_string(temp.path().join(gcode))
            .unwrap()
            .contains("G1")
    );
}

#[test]
fn text_generate_falls_back_to_system_font_when_omitted() {
    if laser_core::font::find_system_font().is_none() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
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
    .with_generated_root(temp.path().to_path_buf());
    let body = serde_json::json!({
        "text": "MOSS",
        "material": "椴木",
        "thickness_mm": 3,
        "font_size_mm": 8
    })
    .to_string();
    let request = format!(
        "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let response = service.handle_http(request.as_bytes());
    assert!(String::from_utf8_lossy(&response).contains("\"success\":true"));
}

#[test]
fn preview_requires_suffix_and_allowed_root() {
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("generated_assets");
    std::fs::create_dir_all(&allowed).unwrap();
    let file = allowed.join("x.gcode");
    std::fs::write(&file, b"G0 X0").unwrap();
    assert!(resolve_preview_file(&file, std::slice::from_ref(&allowed)).is_ok());
    assert!(
        resolve_preview_file(
            std::path::Path::new("x.gcode"),
            std::slice::from_ref(&allowed)
        )
        .is_ok()
    );
    let secret = temp.path().join("secret.txt");
    std::fs::write(&secret, b"secret").unwrap();
    assert_eq!(
        resolve_preview_file(&secret, std::slice::from_ref(&allowed)).unwrap_err(),
        "不支持预览该文件类型"
    );
}

#[test]
fn upload_sanitizes_name_and_uses_sha1_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let result = save_upload(
        "../hello?.gcode",
        "data:text/plain;base64,RzAgWDA=",
        temp.path(),
        123,
    )
    .unwrap();
    assert_eq!(result.filename, "123_ccb5aeb7c2_hello_.gcode");
    assert_eq!(std::fs::read(&result.path).unwrap(), b"G0 X0");
    assert!(save_upload("x.exe", "eA==", temp.path(), 123).is_err());
}

/// 2026-11-06 统一 AI 预览：新预览记下发起方，客户端伪造的 `_origin` 被丢弃，
/// 原始入参里不残留该字段；图片任务另存原图供反馈重生成。
#[test]
fn previews_record_their_origin_and_ignore_client_supplied_origin() {
    let temp = tempfile::tempdir().unwrap();
    let gen_dir = temp.path().join("generated");
    std::fs::create_dir_all(&gen_dir).unwrap();
    image::GrayImage::from_fn(40, 20, |x, _| image::Luma([if x < 20 { 0 } else { 255 }]))
        .save(gen_dir.join("logo.png"))
        .unwrap();
    for use_store in [true, false] {
        let mut service = ApiService::new(
            test_loopback_settings(),
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir.clone()]);
        if use_store {
            service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
        }
        let font = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/fonts/geometry.ttf"
        );
        let (_, ai) = post_with_ctx(
            &service,
            "/api/text-task/generate",
            &serde_json::json!({
                "text": "AB", "font_path": font, "material": "椴木", "thickness_mm": 3, "_origin": "desktop"
            }),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(ai["result"]["origin"], "mcp_bridge", "{ai}");
        assert!(ai["result"]["input"].get("_origin").is_none(), "{ai}");
        let (_, voice) = post_with_ctx(
            &service,
            "/api/workflow",
            &serde_json::json!({
                "action": "preview", "source_type": "image", "image_file": "logo.png", "material": "椴木", "thickness_mm": 3
            }),
            laser_contracts::CallerContext::Gateway {
                session_id: "g".into(),
            },
        );
        assert_eq!(voice["result"]["origin"], "gateway", "{voice}");
        let source = voice["result"]["artifacts"]["source_image_file"]
            .as_str()
            .unwrap();
        assert!(gen_dir.join(source).exists());
        let (_, desk) = post_with_ctx(
            &service,
            "/api/text-task/generate",
            &serde_json::json!({
                "text": "AB", "font_path": font, "material": "椴木", "thickness_mm": 3, "_origin": "voice"
            }),
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(desk["result"]["origin"], "desktop", "{desk}");
        // 其他动作不改写已有记录的来源
        let id = ai["result"]["workflow_id"].as_str().unwrap();
        let status = post_json(
            &service,
            "/api/workflow",
            &serde_json::json!({"action": "status", "workflow_id": id, "_origin": "desktop"}),
        );
        assert_eq!(status["result"]["origin"], "mcp_bridge", "{status}");
    }
}

#[test]
fn material_preset_multipass_and_draw_framing_bounds_are_emitted() {
    let temp = tempfile::tempdir().unwrap();
    let mut lib = laser_contracts::builtin_material_library();
    lib.materials
        .get_mut("椴木")
        .unwrap()
        .thicknesses
        .get_mut("3")
        .unwrap()
        .engrave
        .as_mut()
        .unwrap()
        .outline
        .as_mut()
        .unwrap()
        .passes = 3;
    let service = ApiService::new(test_loopback_settings(), lib)
        .with_generated_root(temp.path().to_path_buf())
        .with_preview_roots(vec![temp.path().to_path_buf()]);

    let font = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );
    let text_res = post_json(
        &service,
        "/api/text-task/generate",
        &serde_json::json!({
            "text": "A",
            "font_path": font,
            "fill": "outline",
            "material": "椴木",
            "thickness_mm": 3
        }),
    );
    assert_eq!(text_res["success"], true, "{text_res}");
    assert_eq!(
        text_res["result"]["artifacts"]["actual_params"]["passes"],
        3
    );
    let gcode_name = text_res["result"]["artifacts"]["gcode_file"]
        .as_str()
        .unwrap();
    let gcode_text = std::fs::read_to_string(temp.path().join(gcode_name)).unwrap();
    assert_eq!(
        gcode_text.matches("; Generated by laser-core").count(),
        3,
        "材料预设 passes=3 必须生成 3 遍刀路: {gcode_text}"
    );
    assert!(
        gcode_text.contains("M5\nG90\nG0 X2.500 Y3.500\n"),
        "多遍刀路换遍回程必须回到 G92 中心原点: {gcode_text}"
    );

    // 画板预览必须输出 frame_mm 以支持红光走边框
    let img = image::RgbaImage::from_fn(36, 24, |x, y| {
        if (6..30).contains(&x) && (6..18).contains(&y) {
            image::Rgba([0, 0, 0, 255])
        } else {
            image::Rgba([255, 255, 255, 255])
        }
    });
    let mut png_bytes = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    use base64::Engine as _;
    let data_url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png_bytes)
    );
    let draw_res = post_json(
        &service,
        "/api/draw/preview",
        &serde_json::json!({
            "image_data_url": data_url,
            "mode": "outline",
            "width_mm": 30.0,
            "height_mm": 20.0,
            "material": "椴木",
            "thickness_mm": 3
        }),
    );
    assert_eq!(draw_res["success"], true, "{draw_res}");
    assert_eq!(
        draw_res["result"]["artifacts"]["actual_params"]["passes"],
        3
    );
    assert_eq!(draw_res["result"]["artifacts"]["frame_mm"]["max_x"], 30.0);
    assert_eq!(draw_res["result"]["artifacts"]["frame_mm"]["max_y"], 20.0);
}
