//! 工具 schema：单一黄金清单（`tests/golden/mcp_tools.json`）的构建与按严格模式、工具开关裁剪。

use crate::HostBackend;

/// 文字任务的基础图形、边框与文字区域（2026-11-17 技术书 r44）：坐标以木牌中心为原点、y 向上，
/// 轮廓由后端按公式生成。只在 `laser_workflow_tool` 上列（每轮工具说明有字数预算），改图形就重新 preview。
fn design_props() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "shapes",
            serde_json::json!({
                "type": "array", "maxItems": 32,
                "description": "preview：基础图形，坐标 mm、木牌中心为原点、y 正数在上，别和文字重叠",
                "items": {
                    "type": "object", "required": ["kind"],
                    "properties": {
                        "kind": {"type": "string", "enum": ["circle", "ellipse", "rect", "polygon", "star", "heart", "line"], "description": "circle 圆 / ellipse 椭圆 / rect 矩形 / polygon 正多边形 / star 星形 / heart 爱心 / line 直线"},
                        "x_mm": {"type": "number", "default": 0, "description": "中心 x"},
                        "y_mm": {"type": "number", "default": 0, "description": "中心 y"},
                        "size_mm": {"type": "number", "description": "直径或外接尺寸；直线是长度"},
                        "width_mm": {"type": "number", "description": "宽（椭圆、矩形）"},
                        "height_mm": {"type": "number", "description": "高（椭圆、矩形）"},
                        "sides": {"type": "integer", "default": 6, "description": "正多边形边数 3~24"},
                        "points": {"type": "integer", "default": 5, "description": "星形角数 3~24"},
                        "corner_radius_mm": {"type": "number", "default": 0, "description": "矩形圆角半径"},
                        "rotation_deg": {"type": "number", "default": 0, "description": "逆时针旋转角度"},
                        "fill": {"type": "string", "enum": ["outline", "fill"], "default": "outline", "description": "outline 描边，fill 填满（直线不能填）"}
                    }
                }
            }),
        ),
        (
            "border",
            serde_json::json!({
                "type": "object",
                "description": "preview：沿木牌边缘加边框（要给木牌尺寸），文字自动放进边框里",
                "properties": {
                    "shape": {"type": "string", "enum": ["rect", "rounded", "circle", "ellipse"], "default": "rect", "description": "rect 直角 / rounded 圆角 / circle 圆 / ellipse 椭圆"},
                    "style": {"type": "string", "enum": ["single", "double"], "default": "single", "description": "single 单线 / double 双线"}
                }
            }),
        ),
        (
            "text_box",
            serde_json::json!({
                "type": "object",
                "description": "preview：文字放进这个矩形（给图形留位置时用），坐标同 shapes",
                "required": ["width_mm", "height_mm"],
                "properties": {
                    "x_mm": {"type": "number", "default": 0, "description": "中心 x"},
                    "y_mm": {"type": "number", "default": 0, "description": "中心 y"},
                    "width_mm": {"type": "number", "description": "宽"},
                    "height_mm": {"type": "number", "description": "高"}
                }
            }),
        ),
    ]
}

/// 材料测试（`/api/material-lab/preview`）对齐 LightBurn 的字段（r41）。
fn calibration_test_props() -> Vec<(&'static str, serde_json::Value)> {
    let axis = |default: &str, label: &str| {
        serde_json::json!({
            "type": "string", "enum": ["power", "speed", "interval", "passes"], "default": default,
            "description": format!("{label}：power 功率 / speed 速度 / interval 线距（只用于填充雕刻）/ passes 遍数；两轴须不同")
        })
    };
    vec![
        (
            "test_type",
            serde_json::json!({
                "type": "string", "enum": ["matrix", "interval"], "default": "matrix",
                "description": "matrix 参数网格（默认）；interval 线距测试：一排不同线距的填充小方块（用户说“线距测试”）"
            }),
        ),
        ("x_axis", axis("power", "matrix 的列参数")),
        ("y_axis", axis("speed", "matrix 的行参数")),
        (
            "interval_min_mm",
            serde_json::json!({"type": "number", "default": 0.08, "description": "线距区间下限 mm"}),
        ),
        (
            "interval_max_mm",
            serde_json::json!({"type": "number", "default": 0.16, "description": "线距区间上限 mm"}),
        ),
        (
            "passes_min",
            serde_json::json!({"type": "integer", "description": "遍数区间下限（轴为 passes 时）"}),
        ),
        (
            "passes_max",
            serde_json::json!({"type": "integer", "description": "遍数区间上限"}),
        ),
        (
            "labels",
            serde_json::json!({"type": "boolean", "default": true, "description": "在矩阵旁刻行列参数标注"}),
        ),
    ]
}

/// 单一工具清单：8 个核心激光工具（面向用户意图重写的说明与参数）+ 桌面控制工具。
/// `developer_mode` 为 false 时隐藏 `command_execution_tool`。
pub fn tools_list_json(developer_mode: bool) -> serde_json::Value {
    let raw: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/golden/mcp_tools.json"))
            .expect("golden mcp_tools.json must parse");
    let mut tools = raw["tools"].as_array().cloned().unwrap_or_default();
    if !developer_mode {
        tools.retain(|t| t["name"].as_str() != Some("command_execution_tool"));
    }
    for tool in &mut tools {
        if tool["name"] == "run_calibration_grid_tool" {
            for (key, value) in calibration_test_props() {
                tool["inputSchema"]["properties"][key] = value;
            }
        }
        if tool["name"] == "laser_workflow_tool" {
            for (key, value) in design_props() {
                tool["inputSchema"]["properties"][key] = value;
            }
        }
    }
    serde_json::json!({ "tools": tools })
}

/// 按严格模式列出工具：严格模式下掉永不对语音开放的 R4 工具（技术书 §6.1）。
pub fn tools_list_for(developer_mode: bool, strict: bool) -> serde_json::Value {
    let raw = tools_list_json(developer_mode);
    if !strict {
        return raw;
    }
    let tools = raw["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| {
                    !laser_contracts::tool_never_exposed(tool["name"].as_str().unwrap_or(""))
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    serde_json::json!({ "tools": tools })
}

/// 按后端的状态列出工具：严格模式去掉永不开放的工具，再去掉用户关闭的工具（NV8）。
pub fn tools_list_with(
    developer_mode: bool,
    backend: Option<&dyn HostBackend>,
) -> serde_json::Value {
    let strict = backend.is_some_and(|backend| backend.strict_mode());
    let mut list = tools_list_for(developer_mode, strict);
    if let (Some(backend), Some(tools)) = (backend, list["tools"].as_array_mut()) {
        tools.retain(|tool| !backend.tool_disabled(tool["name"].as_str().unwrap_or("")));
    }
    list
}
