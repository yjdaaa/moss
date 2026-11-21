//! 工具 schema：完整 / 精简清单的构建与按严格模式、工具开关裁剪。

use crate::HostBackend;

/// 图片转换方式（与 `laser_core::image_job::TONE_MODES` 一致，测试核对）。
const IMAGE_MODES: [&str; 11] = [
    "auto",
    "dither",
    "threshold",
    "ordered",
    "atkinson",
    "jarvis",
    "stucki",
    "newsprint",
    "halftone",
    "sketch",
    "grayscale",
];

fn image_mode_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string", "enum": IMAGE_MODES, "default": "auto",
        "description": "source_type=image：转换方式。auto 自动识别照片 / 线稿；jarvis 照片推荐；threshold 黑白线条；halftone 网点；grayscale 灰度变功率（二极管）"
    })
}

/// 材料测试（`/api/material-lab/preview`）对齐 LightBurn 的字段：完整清单与精简清单共用，说明保持简短。
/// 文字任务的基础图形、边框与文字区域（2026-11-17 技术书 r44）：两种清单的 laser_workflow_tool 与
/// regenerate_text_laser_task_tool 共用。坐标以木牌中心为原点、y 向上，轮廓由后端按公式生成。
fn design_props(regenerate: bool) -> Vec<(&'static str, serde_json::Value)> {
    let note = if regenerate {
        "（不改就不填，给了就整组替换）"
    } else {
        ""
    };
    vec![
        (
            "shapes",
            serde_json::json!({
                "type": "array", "maxItems": 32,
                "description": format!("preview：基础图形，坐标 mm、木牌中心为原点、y 正数在上，别和文字重叠{note}"),
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
                "description": format!("preview：沿木牌边缘加边框（要给木牌尺寸），文字自动放进边框里{note}"),
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
                "description": format!("preview：文字放进这个矩形（给图形留位置时用），坐标同 shapes{note}"),
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

pub fn tools_list_json(developer_mode: bool) -> serde_json::Value {
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/golden/mcp_tools.json"))
            .expect("golden mcp_tools.json must parse");
    for tool in raw["tools"].as_array_mut().unwrap() {
        match tool["name"].as_str().unwrap_or("") {
            "refine_laser_params_from_feedback_tool" => {
                tool["description"] = serde_json::json!(
                    "关联原 task_id 判断加工反馈并保存有界建议；不发送。原任务缺少实际参数时必须显式提供功率、速度、次数和 power_scale。"
                );
                tool["inputSchema"]["properties"]["power_scale"] = serde_json::json!({
                    "type":"number", "minimum":1, "maximum":100000,
                    "description":"原任务功率标尺；只有旧任务缺少实际参数时才需要显式提供"
                });
                for key in ["laser_max_power", "feed_rate", "passes"] {
                    tool["inputSchema"]["properties"][key]
                        .as_object_mut()
                        .unwrap()
                        .remove("default");
                }
                tool["inputSchema"]["required"] = serde_json::json!(["task_id", "feedback_text"]);
            }
            "regenerate_text_laser_task_tool" => {
                tool["description"] = serde_json::json!(
                    "重新生成任务预览，不发送。采用已保存调参建议时传 task_id、feedback_id、confirmed=true；返回 adopted_workflow_id，新任务发送仍须独立确认。"
                );
                tool["inputSchema"]["properties"]["feedback_id"] = serde_json::json!({
                    "type":"string", "description":"待采用的已保存反馈 ID"
                });
                tool["inputSchema"]["oneOf"] = serde_json::json!([
                    {"not":{"required":["feedback_id"]}},
                    {"required":["task_id","feedback_id","confirmed"],"properties":{"confirmed":{"const":true},"feedback_id":{"minLength":1}}}
                ]);
                for (key, value) in design_props(true) {
                    tool["inputSchema"]["properties"][key] = value;
                }
            }
            "generate_text_laser_task_tool"
            | "generate_text_image_gcode_tool"
            | "ai_laser_gcode_tool"
            | "laser_workflow_tool" => {
                // 2026-11-06 文字刻法、排版与起点（用户决定：默认描边、激光头对准木牌中心）
                let props = &mut tool["inputSchema"]["properties"];
                props["text_fill"] = serde_json::json!({
                    "type": "string", "enum": ["outline", "fill", "hatch"], "default": "outline",
                    "description": "文字刻法：outline 只描轮廓（默认）；fill 矢量扫描线密排填满（小字、中文更清楚）；hatch 线距放宽的排线填满并描边（大字更快）"
                });
                // 2026-11-16 填充扫描与走刀选项（对齐 LightBurn），全部可省略
                props["fill_options"] = serde_json::json!({
                    "type": "object",
                    "description": "填充扫描设置（只对 fill / hatch 有效，一般不用传）",
                    "properties": {
                        "grouping": {"type": "string", "enum": ["all", "individual"], "default": "all", "description": "all 每行横穿所有字（默认，速度均匀）；individual 一块块扫完（空白多时更快）"},
                        "angle_deg": {"type": "number", "default": 0, "description": "扫描角度，0 横扫，90 竖扫"},
                        "cross_hatch": {"type": "boolean", "default": false, "description": "再转 90° 扫一遍，更实更深"},
                        "bidirectional": {"type": "boolean", "default": true, "description": "来回都出光；false 只单向出光（更齐、更慢）"},
                        "overscan_mm": {"type": ["number", "string"], "description": "每行两头关光多走的距离 mm；省略或 auto 按速度自动算，0 关闭"},
                        "scan_offset_mm": {"type": "number", "default": 0, "description": "双向扫描错位补偿 mm（-2 ~ 2）"},
                        "scan_offset_table": {"type": "array", "items": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}, "maxItems": 32, "description": "按速度的错位补偿表 [[速度 mm/min, 偏移 mm], …]，按加工速度插值，给了就覆盖 scan_offset_mm"}
                    }
                });
                // 2026-11-16 外框切割质量（对齐 LightBurn Cut 设置），只用于 laser_mode=cut，默认全关
                props["cut_options"] = serde_json::json!({
                    "type": "object",
                    "description": "外框切割设置（只用于切割，一般不用传）",
                    "properties": {
                        "kerf_mm": {"type": "number", "default": 0, "description": "切缝补偿 mm（-2 ~ 2）：正值往外、负值往里，孔自动反向，零件保持尺寸"},
                        "tab_count": {"type": "integer", "default": 0, "description": "每个闭合图形的桥位个数（防零件掉落），与 tab_spacing_mm 二选一"},
                        "tab_spacing_mm": {"type": "number", "description": "每隔多少 mm 放一个桥位"},
                        "tab_width_mm": {"type": "number", "default": 0.5, "description": "桥位宽度 mm"},
                        "tabs_skip_inner": {"type": "boolean", "default": true, "description": "内部图形（孔）不加桥位"},
                        "lead_in_mm": {"type": "number", "default": 0, "description": "引入线长度 mm（从废料一侧切入）"},
                        "lead_out_mm": {"type": "number", "default": 0, "description": "引出线长度 mm"},
                        "lead_angle_deg": {"type": "number", "default": 90, "description": "引线角度（1 ~ 90，90 垂直）"},
                        "overcut_mm": {"type": "number", "default": 0, "description": "回到起点后多切的距离 mm"}
                    }
                });
                props["path_options"] = serde_json::json!({
                    "type": "object",
                    "description": "走刀顺序选项（一般不用传）",
                    "properties": {
                        "prefer_corners": {"type": "boolean", "default": true, "description": "轮廓起刀点尽量放在尖角"},
                        "remove_overlaps": {"type": "boolean", "default": true, "description": "重复的线只刻一遍"}
                    }
                });
                props["text_align"] = serde_json::json!({
                    "type": "string", "enum": ["left", "center", "right"], "default": "center",
                    "description": "多行文字（用换行分行）的对齐方式"
                });
                // r48：不填按电脑工作台选的对刀起点（全局设置，默认木牌中心），用户明确说了才填
                props["origin"] = serde_json::json!({
                    "type": "string", "enum": ["center", "lower_left"],
                    "description": "开刻前激光头对准的位置：center 木牌（图案）中心，lower_left 木牌左下角；用户没说就不填，按电脑上工作台选的对刀起点"
                });
                props["width_mm"] = serde_json::json!({
                    "type": "number", "description": "文字任务：木牌宽度 mm；不给字号时自动取能放下的最大字号并居中"
                });
                props["height_mm"] = serde_json::json!({
                    "type": "number", "description": "文字任务：木牌高度 mm（可与 width_mm 一起或单独给）"
                });
                props["safe_margin_mm"] = serde_json::json!({
                    "type": "number", "default": 2, "description": "文字放进木牌时四周留边 mm"
                });
                if tool["name"] == "laser_workflow_tool" {
                    // 2026-11-16（技术书 r41）图片转换方式与行间距倍数；图片只能是已上传到 MOSS 的文件
                    let props = &mut tool["inputSchema"]["properties"];
                    props["image_mode"] = image_mode_schema();
                    props["line_interval_scale"] = serde_json::json!({
                        "type": "number", "minimum": 0.5, "maximum": 3,
                        "description": "source_type=image：行间距 = 材料推荐线距 × 倍数；1.5 快速、1 均衡（默认）、0.8 精细"
                    });
                    for (key, value) in design_props(false) {
                        props[key] = value;
                    }
                }
            }
            "run_calibration_grid_tool" => {
                // 2026-11-16（技术书 r41）材料测试对齐 LightBurn 的测试类型、轴与区间
                let props = &mut tool["inputSchema"]["properties"];
                for (key, value) in calibration_test_props() {
                    props[key] = value;
                }
            }
            _ => {}
        }
    }
    if developer_mode {
        raw
    } else {
        let tools = raw["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["name"].as_str() != Some("command_execution_tool"))
            .cloned()
            .collect::<Vec<_>>();
        serde_json::json!({ "tools": tools })
    }
}

/// n72（2026-11-08 用户决定）：精简清单里的激光工具。其余激光工具（重复的生成 / 发送入口、
/// 底层 GRBL 收发）只在“完整工具（高级）”打开时列出；执行逻辑与安全门禁不变，按名字仍可调用。
pub const COMPACT_LASER_TOOLS: [&str; 8] = [
    "check_laser_connection_tool",
    "laser_safe_action_tool",
    "laser_workflow_tool",
    "material_params_tool",
    "refine_laser_params_from_feedback_tool",
    "regenerate_text_laser_task_tool",
    "run_calibration_grid_tool",
    "select_calibration_cell_tool",
];

/// 不属于激光的电脑控制类工具：两种清单都原样列出（内置语音与严格模式下另由 `tool_never_exposed` 去掉）。
fn is_desktop_control_tool(name: &str) -> bool {
    matches!(
        name,
        "command_execution_tool"
            | "open_app_tool"
            | "shortcut_key_execution_tool"
            | "open_jsjds_website_tool"
    )
}

/// n72 精简清单：8 个激光工具用面向用户意图重写的说明与参数（`tests/golden/mcp_tools_compact.json`，
/// 每个参数都有中文说明，连接参数只留可不填的串口号与 IP），再加上电脑控制类工具。
pub fn compact_tools_list_json(developer_mode: bool) -> serde_json::Value {
    let compact: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/golden/mcp_tools_compact.json"))
            .expect("golden mcp_tools_compact.json must parse");
    let mut tools = compact["tools"].as_array().cloned().unwrap_or_default();
    for tool in &mut tools {
        if tool["name"] == "run_calibration_grid_tool" {
            for (key, value) in calibration_test_props() {
                tool["inputSchema"]["properties"][key] = value;
            }
        }
        // 精简清单只在 laser_workflow_tool 上列图形参数（每轮工具说明有字数预算）：改图形就重新 preview
        if tool["name"] == "laser_workflow_tool" {
            for (key, value) in design_props(false) {
                tool["inputSchema"]["properties"][key] = value;
            }
        }
    }
    if let Some(full) = tools_list_json(developer_mode)["tools"].as_array() {
        tools.extend(
            full.iter()
                .filter(|tool| is_desktop_control_tool(tool["name"].as_str().unwrap_or("")))
                .cloned(),
        );
    }
    serde_json::json!({ "tools": tools })
}

/// 严格模式下接入点实际列出的工具：去掉永不对语音开放的 R4 工具（技术书 §6.1）。
pub fn tools_list_for(developer_mode: bool, strict: bool) -> serde_json::Value {
    tools_list_profile(developer_mode, strict, true)
}

/// 按清单档位列出：`full` 为完整清单，否则为精简清单；严格模式再去掉永不开放的工具。
pub fn tools_list_profile(developer_mode: bool, strict: bool, full: bool) -> serde_json::Value {
    let raw = if full {
        tools_list_json(developer_mode)
    } else {
        compact_tools_list_json(developer_mode)
    };
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
    let full = backend.is_none_or(|backend| backend.full_tool_list());
    let mut list = tools_list_profile(developer_mode, strict, full);
    if let (Some(backend), Some(tools)) = (backend, list["tools"].as_array_mut()) {
        tools.retain(|tool| !backend.tool_disabled(tool["name"].as_str().unwrap_or("")));
    }
    list
}
