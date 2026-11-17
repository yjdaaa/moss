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

/// r46：画板矢量刻法——轮廓不生成填充、填充把闭合图形填满并取光栅参数，回显 `vector_style`。
#[test]
fn draw_vector_style_chooses_outline_or_fill() {
    let temp = tempfile::tempdir().unwrap();
    // 内置库里椴木的光栅与描边参数相同：把光栅功率单独调低，才能看出用的是哪一档
    let mut library = laser_contracts::builtin_material_library();
    for modes in library
        .materials
        .get_mut("椴木")
        .unwrap()
        .thicknesses
        .values_mut()
    {
        if let Some(raster) = modes.engrave.as_mut().and_then(|e| e.raster.as_mut()) {
            raster.laser_max_power = 300;
        }
    }
    let gen_dir = temp.path().join("gen");
    std::fs::create_dir_all(&gen_dir).unwrap();
    let service = ApiService::new(test_loopback_settings(), library)
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir]);
    let scene = serde_json::json!({"elements": [
        {"type": "rectangle", "x": 0, "y": 0, "width": 200, "height": 100, "strokeColor": "#1e1e1e",
         "backgroundColor": "transparent", "strokeWidth": 2},
    ]});
    let draw = |style: &str| {
        post_json(
            &service,
            "/api/draw/preview",
            &serde_json::json!({"image_data_url": super::preview::draw_test_png(), "material": "椴木",
                "thickness_mm": 3.0, "width_mm": 40, "mode": "vector", "scene_json": scene,
                "vector_style": style}),
        )
    };
    let outline = draw("outline");
    let artifacts = &outline["result"]["artifacts"];
    assert_eq!(artifacts["vector_style"], "outline", "{outline}");
    assert!(artifacts["fill_options"].is_null(), "轮廓不填");
    let fill = draw("fill");
    let artifacts = &fill["result"]["artifacts"];
    assert_eq!(artifacts["vector_style"], "fill", "{fill}");
    assert!(
        artifacts["fill_options"].is_object(),
        "没设背景色的矩形也填满"
    );
    assert_ne!(
        artifacts["actual_params"], outline["result"]["artifacts"]["actual_params"],
        "填充用光栅参数，轮廓用描边参数"
    );
    let canvas = draw("");
    assert_eq!(canvas["result"]["artifacts"]["vector_style"], "canvas");
    assert!(
        canvas["result"]["artifacts"]["fill_options"].is_null(),
        "按画布颜色：没背景色不填"
    );
    assert_eq!(draw("hatch")["success"], false);
}

/// r47：画板矢量刻法“切割”——先内后外切穿，切缝按嵌套方向补偿，桥位默认只加在外框；必须有该厚度的切割参数。
#[test]
fn draw_vector_cut_cuts_inside_first_with_kerf_and_tabs() {
    let temp = tempfile::tempdir().unwrap();
    let service = service(&temp);
    // 外框 200×100，里面一个 60×40 的孔
    let scene = serde_json::json!({"elements": [
        {"type": "rectangle", "x": 0, "y": 0, "width": 200, "height": 100, "strokeColor": "#1e1e1e",
         "backgroundColor": "transparent", "strokeWidth": 2},
        {"type": "rectangle", "x": 70, "y": 30, "width": 60, "height": 40, "strokeColor": "#1e1e1e",
         "backgroundColor": "transparent", "strokeWidth": 2},
    ]});
    let draw = |extra: serde_json::Value| {
        let mut body = serde_json::json!({"image_data_url": super::preview::draw_test_png(),
            "material": "椴木", "thickness_mm": 3.0, "width_mm": 40, "mode": "vector",
            "scene_json": scene, "vector_style": "cut"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        post_json(&service, "/api/draw/preview", &body)
    };
    let plain = draw(serde_json::json!({}));
    let artifacts = &plain["result"]["artifacts"];
    assert_eq!(plain["success"], true, "{plain}");
    assert_eq!(artifacts["laser_mode"], "cut");
    assert_eq!(artifacts["vector_style"], "cut");
    assert!(artifacts["fill_options"].is_null(), "切割不填充");
    let gcode_of = |artifacts: &serde_json::Value| {
        std::fs::read_to_string(
            temp.path()
                .join("gen")
                .join(artifacts["gcode_file"].as_str().unwrap()),
        )
        .unwrap()
    };
    // 先内后外：第一段出光落在孔所在的中间区域
    let gcode = gcode_of(artifacts);
    let first_burn = gcode
        .lines()
        .find(|l| l.starts_with("G1 ") && !l.contains(" S0"))
        .unwrap();
    let x: f64 = first_burn
        .split_whitespace()
        .find(|w| w.starts_with('X'))
        .and_then(|w| w[1..].parse().ok())
        .unwrap();
    assert!((12.0..28.0).contains(&x), "先切中间的孔：{first_burn}");

    let tabbed = draw(serde_json::json!({"cut_options": {"kerf_mm": 0.1, "tab_count": 2}}));
    let artifacts = &tabbed["result"]["artifacts"];
    assert_eq!(tabbed["success"], true, "{tabbed}");
    let report = &artifacts["cut_options"]["report"];
    assert_eq!(report["tabs"], 2, "桥位默认跳过内孔：{report}");
    assert_eq!(report["kerf_contours_before"], 2, "{report}");
    assert_eq!(
        report["kerf_contours_after"], 2,
        "内孔反向补偿，没有消失或相连：{report}"
    );
    let e = laser_core::gcode::motion_extents(&gcode_of(artifacts))
        .unwrap()
        .unwrap();
    assert!(
        e.min_x >= -1e-6 && e.min_y >= -1e-6,
        "左下角起点不往负方向走：{e:?}"
    );

    // 没有该厚度的切割参数不能切；切割不能带填满
    let missing = post_json(
        &service,
        "/api/draw/preview",
        &serde_json::json!({"image_data_url": super::preview::draw_test_png(), "material": "椴木",
            "thickness_mm": 99.0, "mode": "vector", "scene_json": scene, "vector_style": "cut"}),
    );
    assert_eq!(missing["success"], false);
    assert!(
        missing["result"].as_str().unwrap().contains("切割参数"),
        "{missing}"
    );
}

/// r48：文字对刀起点全局设置——工作台与语音同一条，请求没写 `origin` 时按它，写了就按请求；局域网只读。
#[test]
fn text_origin_is_one_shared_setting_used_when_the_request_omits_it() {
    let temp = tempfile::tempdir().unwrap();
    let service = service(&temp);
    assert_eq!(
        get(&service, "/api/text-origin")["result"]["origin"],
        "center"
    );
    let default = text(&service, serde_json::json!({}));
    assert_eq!(
        default["result"]["artifacts"]["origin"], "center",
        "{default}"
    );

    let set = post_json(
        &service,
        "/api/text-origin",
        &serde_json::json!({"origin": "lower_left"}),
    );
    assert_eq!(set["result"]["origin"], "lower_left", "{set}");
    let follow = text(&service, serde_json::json!({}));
    assert_eq!(follow["result"]["artifacts"]["origin"], "lower_left");
    assert_eq!(
        follow["result"]["artifacts"]["origin_note"],
        "开刻前把激光头移到木牌左下角"
    );
    let explicit = text(&service, serde_json::json!({"origin": "center"}));
    assert_eq!(
        explicit["result"]["artifacts"]["origin"], "center",
        "说了就按说的"
    );

    let (status, _) = post_with_ctx(
        &service,
        "/api/text-origin",
        &serde_json::json!({"origin": "top"}),
        laser_contracts::CallerContext::Loopback,
    );
    assert_eq!(status, 400);
    let (status, _) = post_with_ctx(
        &service,
        "/api/text-origin",
        &serde_json::json!({"origin": "center"}),
        laser_contracts::CallerContext::Lan {
            pairing_id: "pair-1".into(),
        },
    );
    assert_eq!(status, 403, "局域网只能看不能改");
    assert_eq!(
        get(&service, "/api/text-origin")["result"]["origin"],
        "lower_left"
    );
}
