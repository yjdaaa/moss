//! r45（2026-11-17）：雕刻路线全局开关（矢量 / 转图片）与图片路线优化。
use super::*;

const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/fonts/geometry.ttf"
);

fn service(temp: &tempfile::TempDir) -> ApiService {
    let gen_dir = temp.path().join("gen");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(&gen_dir).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_runtime_path(runtime)
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir])
}

fn get(service: &ApiService, path: &str) -> serde_json::Value {
    parse_json_response(
        &service.handle_http(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()),
    )
}

fn text(service: &ApiService, extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({"text": "AB", "font_path": FONT, "material": "椴木",
        "thickness_mm": 3, "width_mm": 60, "height_mm": 30});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    post_json(service, "/api/text-task/generate", &body)
}

#[test]
fn engrave_route_is_one_shared_setting_followed_by_text_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let service = service(&temp);
    assert_eq!(
        get(&service, "/api/engrave-route")["result"]["route"],
        "vector"
    );
    let vector = text(&service, serde_json::json!({}));
    assert_eq!(
        vector["result"]["artifacts"]["engrave_route"], "vector",
        "{vector}"
    );

    // 改成转图片：之后不带 engrave_route 的文字任务（工作台、语音同一条）都转图片
    let set = post_json(
        &service,
        "/api/engrave-route",
        &serde_json::json!({"route": "image"}),
    );
    assert_eq!(set["result"]["route"], "image", "{set}");
    assert_eq!(
        get(&service, "/api/engrave-route")["result"]["route"],
        "image"
    );
    let image = text(&service, serde_json::json!({"text_fill": "outline"}));
    let artifacts = &image["result"]["artifacts"];
    assert_eq!(artifacts["engrave_route"], "image", "{image}");
    assert_eq!(artifacts["text_fill"], "fill", "转图片时文字一律实心");
    assert!(
        artifacts["route_note"]
            .as_str()
            .unwrap()
            .contains("一律实心")
    );
    assert_eq!(artifacts["raster"]["pixel_size_mm"], 0.1);
    let gcode = std::fs::read_to_string(
        temp.path()
            .join("gen")
            .join(artifacts["gcode_file"].as_str().unwrap()),
    )
    .unwrap();
    assert!(gcode.contains("; Raster mode"));
    // 转图片用光栅参数（与实心填充相同）
    let fill = {
        post_json(
            &service,
            "/api/engrave-route",
            &serde_json::json!({"route": "vector"}),
        );
        let fill = text(&service, serde_json::json!({"text_fill": "fill"}));
        post_json(
            &service,
            "/api/engrave-route",
            &serde_json::json!({"route": "image"}),
        );
        fill
    };
    assert_eq!(
        artifacts["actual_params"],
        fill["result"]["artifacts"]["actual_params"]
    );
    // 同一版面：起点与走边框范围相同
    assert_eq!(
        artifacts["frame_mm"],
        fill["result"]["artifacts"]["frame_mm"]
    );

    // 请求写了就按请求；外框切割始终走矢量
    let explicit = text(&service, serde_json::json!({"engrave_route": "vector"}));
    assert_eq!(explicit["result"]["artifacts"]["engrave_route"], "vector");
    let cut = text(&service, serde_json::json!({"laser_mode": "cut"}));
    assert_eq!(
        cut["result"]["artifacts"]["engrave_route"], "vector",
        "{cut}"
    );
    assert!(
        cut["result"]["artifacts"]["route_note"]
            .as_str()
            .unwrap()
            .contains("外框切割只能走矢量")
    );
    let bad = text(&service, serde_json::json!({"engrave_route": "bitmap"}));
    assert_eq!(bad["success"], false);

    // 设置的写入：值校验、局域网只读
    let (status, _) = post_with_ctx(
        &service,
        "/api/engrave-route",
        &serde_json::json!({"route": "raster"}),
        laser_contracts::CallerContext::Loopback,
    );
    assert_eq!(status, 400);
    let (status, _) = post_with_ctx(
        &service,
        "/api/engrave-route",
        &serde_json::json!({"route": "vector"}),
        laser_contracts::CallerContext::Lan {
            pairing_id: "pair-1".into(),
        },
    );
    assert_eq!(status, 403, "局域网只能看不能改");
    assert_eq!(
        get(&service, "/api/engrave-route")["result"]["route"],
        "image"
    );
}

#[test]
fn draw_board_follows_the_switch_and_images_use_the_workbench_engine() {
    let temp = tempfile::tempdir().unwrap();
    let service = service(&temp);
    let png = super::preview::draw_test_png();
    let draw = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"image_data_url": png, "material": "椴木",
            "thickness_mm": 3.0, "width_mm": 40});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/draw/preview", &body)
    };
    // 没写 mode：跟开关走（没带场景数据时矢量会退回光栅并说明）
    let default = draw(serde_json::json!({}));
    assert_eq!(
        default["result"]["artifacts"]["mode"], "raster",
        "{default}"
    );
    assert!(default["result"]["artifacts"]["vector_note"].is_string());
    post_json(
        &service,
        "/api/engrave-route",
        &serde_json::json!({"route": "image"}),
    );
    let image = draw(serde_json::json!({}));
    let artifacts = &image["result"]["artifacts"];
    assert_eq!(artifacts["mode"], "raster", "{image}");
    assert!(
        artifacts["vector_note"].is_null(),
        "转图片是用户选的，不是退回"
    );
    assert_eq!(artifacts["engrave_route"], "image");
    // 与工作台图片同一套：材料线距、自动识别、自动过扫描
    assert_eq!(artifacts["pixel_size_mm"], 0.1);
    assert!(artifacts["image_mode"].is_string());
    assert_eq!(artifacts["scan_options"]["overscan_auto"], true);
    let overscan = artifacts["scan_options"]["overscan_mm"].as_f64().unwrap();
    assert!(overscan >= 1.0);
    assert_eq!(
        artifacts["frame_mm"]["max_x"].as_f64().unwrap(),
        artifacts["width_mm"].as_f64().unwrap() + 2.0 * overscan
    );
    // 图片模式、行间距倍数与扫描偏移
    let jarvis = draw(serde_json::json!({"mode": "raster", "image_mode": "jarvis",
        "line_interval_scale": 1.5, "fill_options": {"scan_offset_mm": 0.1}}));
    let artifacts = &jarvis["result"]["artifacts"];
    assert_eq!(artifacts["image_mode"], "jarvis", "{jarvis}");
    assert!((artifacts["pixel_size_mm"].as_f64().unwrap() - 0.15).abs() < 1e-9);
    assert_eq!(artifacts["scan_options"]["scan_offset_mm"], 0.1);
    for (extra, expected) in [
        (
            serde_json::json!({"mode": "raster", "pass_through": true}),
            "直通",
        ),
        (
            serde_json::json!({"mode": "raster", "line_interval_scale": 5}),
            "line_interval_scale",
        ),
        (
            serde_json::json!({"mode": "raster", "fill_options": {"overscan_mm": 0.5, "scan_offset_mm": 1.5}}),
            "扫描偏移的一半",
        ),
        (serde_json::json!({"mode": "bitmap"}), "mode 只能是"),
    ] {
        let refused = draw(extra.clone());
        assert_eq!(refused["success"], false, "{extra}");
        assert!(
            refused["result"]
                .as_str()
                .unwrap_or_default()
                .contains(expected),
            "{extra}: {refused}"
        );
    }
}
