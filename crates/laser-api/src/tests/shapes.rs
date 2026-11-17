//! r44（2026-11-17）：文字任务里的基础图形、边框、文字区域与字形焊接。
use super::*;

const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/fonts/geometry.ttf"
);

fn service(temp: &tempfile::TempDir) -> ApiService {
    let gen_dir = temp.path().join("gen");
    std::fs::create_dir_all(&gen_dir).unwrap();
    ApiService::new(
        test_loopback_settings(),
        laser_contracts::builtin_material_library(),
    )
    .with_generated_root(gen_dir.clone())
    .with_preview_roots(vec![gen_dir])
}

fn generate(service: &ApiService, body: serde_json::Value) -> serde_json::Value {
    post_json(service, "/api/text-task/generate", &body)
}

fn coaster(extra: serde_json::Value) -> serde_json::Value {
    let mut body = serde_json::json!({
        "text": "AB", "font_path": FONT, "material": "椴木", "thickness_mm": 3,
        "width_mm": 90, "height_mm": 90, "text_fill": "fill",
        "border": {"shape": "circle", "style": "double"},
        "text_box": {"x_mm": 0, "y_mm": -5, "width_mm": 40, "height_mm": 30},
        "shapes": [{"kind": "star", "size_mm": 10, "x_mm": 0, "y_mm": 30, "fill": "fill"}]
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

#[test]
fn text_with_border_and_shapes_generates_one_job_centered_on_the_board() {
    let temp = tempfile::tempdir().unwrap();
    let service = service(&temp);
    let response = generate(&service, coaster(serde_json::json!({})));
    assert_eq!(response["success"], true, "{response}");
    let artifacts = &response["result"]["artifacts"];
    assert_eq!(artifacts["shape_count"], 3, "{artifacts}");
    let star = &artifacts["shapes"][0];
    assert_eq!(star["kind"], "star");
    assert_eq!(star["points"], 5);
    assert!((star["inner_ratio"].as_f64().unwrap() - 0.382).abs() < 1e-3);
    assert_eq!(star["width_mm"], 10.0);
    assert_eq!(star["fill"], "fill");
    assert_eq!(artifacts["border"]["shape"], "circle");
    assert_eq!(artifacts["border"]["style"], "double");
    assert_eq!(artifacts["text_box"]["y_mm"], -5.0);
    assert!(artifacts["weld"]["contours_after"].as_u64().unwrap() > 0);
    // 激光头对准木牌中心：外圈直径 84，范围关于起点对称
    let frame = &artifacts["frame_mm"];
    assert!(
        (frame["min_x"].as_f64().unwrap() + 42.0).abs() < 0.05,
        "{frame}"
    );
    assert!(
        (frame["max_y"].as_f64().unwrap() - 42.0).abs() < 0.05,
        "{frame}"
    );
    assert_eq!(artifacts["origin_note"], "开刻前把激光头移到木牌中心");

    // 只刻填满的图形：不需要字体，参数与实心填充文字一样取材料库光栅参数
    let shapes_only = generate(
        &service,
        serde_json::json!({"material": "椴木", "thickness_mm": 3, "width_mm": 40, "height_mm": 40,
            "shapes": [{"kind": "heart", "size_mm": 30, "fill": "fill"}]}),
    );
    assert_eq!(shapes_only["success"], true, "{shapes_only}");
    let only = &shapes_only["result"]["artifacts"];
    assert_eq!(only["glyphs"], 0);
    assert!(only["weld"].is_null());
    assert_eq!(
        only["actual_params"], artifacts["actual_params"],
        "填满的图形用光栅参数"
    );
    let outline = generate(
        &service,
        serde_json::json!({"material": "椴木", "thickness_mm": 3, "width_mm": 40, "height_mm": 40,
            "shapes": [{"kind": "polygon", "sides": 6, "size_mm": 30}]}),
    );
    assert_eq!(outline["success"], true, "{outline}");
    assert_eq!(outline["result"]["artifacts"]["shapes"][0]["sides"], 6);

    // 关闭焊接
    let raw = generate(&service, coaster(serde_json::json!({"text_weld": false})));
    assert!(raw["result"]["artifacts"]["weld"].is_null(), "{raw}");
}

#[test]
fn shape_requests_are_validated_before_generating() {
    let temp = tempfile::tempdir().unwrap();
    let service = service(&temp);
    for (extra, expected) in [
        (
            serde_json::json!({"shapes": [{"kind": "triangle", "size_mm": 10}]}),
            "kind 只能是",
        ),
        (
            serde_json::json!({"shapes": [{"kind": "rect", "width_mm": 10}]}),
            "缺少尺寸",
        ),
        (
            serde_json::json!({"shapes": [{"kind": "line", "length_mm": 10, "fill": "fill"}]}),
            "直线不能填满",
        ),
        (
            serde_json::json!({"shapes": [{"kind": "star", "size_mm": 10, "y_mm": 60}]}),
            "超出了木牌",
        ),
        (
            serde_json::json!({"shapes": [{"kind": "circle", "size_mm": 20, "y_mm": -5}]}),
            "重叠",
        ),
        (
            serde_json::json!({"shapes": {"kind": "circle"}}),
            "shapes 必须是数组",
        ),
        (
            serde_json::json!({"border": {"shape": "hexagon"}}),
            "border.shape",
        ),
        (
            serde_json::json!({"border": {"shape": "rect", "style": "triple"}}),
            "border.style",
        ),
        (
            serde_json::json!({"text_box": {"width_mm": 20}}),
            "text_box 要给 width_mm 和 height_mm",
        ),
        (
            serde_json::json!({"text_box": {"x_mm": 40, "width_mm": 30, "height_mm": 10}}),
            "text_box 超出了木牌范围",
        ),
        (
            serde_json::json!({"laser_mode": "cut", "text_fill": "outline"}),
            "切割时图形不能填满",
        ),
    ] {
        let response = generate(&service, coaster(extra.clone()));
        assert_eq!(response["success"], false, "{extra}: {response}");
        let message = response["result"].as_str().unwrap_or_default();
        assert!(message.contains(expected), "{extra}: {message}");
    }
    // 文字和图形都没有：报错原文不变（兼容快照）
    let empty = generate(
        &service,
        serde_json::json!({"text": " ", "material": "椴木", "thickness_mm": 3}),
    );
    assert_eq!(empty["result"], "text 不能为空");
}
