//! 文字任务里的基础图形、边框与文字区域（2026-11-17 用户决定，技术书 r44）：请求解析与回显。
//!
//! 请求字段（全部可省略，坐标以木牌中心为原点、x 向右、y 向上，单位 mm）：
//! - `shapes`：`[{kind, x_mm, y_mm, size_mm | width_mm + height_mm | length_mm, rotation_deg, fill, …}]`，
//!   `kind` = `circle` / `ellipse` / `rect` / `polygon` / `star` / `heart` / `line`；
//!   `rect` 可带 `corner_radius_mm`，`polygon` 带 `sides`（默认 6），`star` 带 `points`（默认 5）与 `inner_ratio`；
//!   `fill` 为 `true` / `"fill"` 填满，`false` / `"outline"`（默认）只描边。
//! - `border`：`{shape: rect|rounded|circle|ellipse, style: single|double, inset_mm, gap_mm, corner_radius_mm}`。
//! - `text_box`：`{x_mm, y_mm, width_mm, height_mm}`，文字自动适配进这个矩形。
//! - `text_weld`：`false` 关闭字形焊接（默认开）。

use laser_core::shapes::{self, Border, BorderShape, Shape, ShapeKind};

fn number(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    what: &str,
) -> Result<Option<f64>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{what}的 {key} 必须是数字")),
    }
}

fn integer(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    what: &str,
    default: u32,
) -> Result<u32, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("{what}的 {key} 必须是正整数")),
    }
}

fn fill_flag(value: Option<&serde_json::Value>, what: &str) -> Result<bool, String> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(serde_json::Value::Bool(b)) => Ok(*b),
        Some(serde_json::Value::String(s)) => match s.trim() {
            "" | "outline" => Ok(false),
            "fill" => Ok(true),
            other => Err(format!("{what}的 fill 只能是 fill / outline，收到 {other}")),
        },
        Some(_) => Err(format!("{what}的 fill 只能是 fill / outline")),
    }
}

/// 解析 `shapes`。
pub(crate) fn parse_shapes(value: Option<&serde_json::Value>) -> Result<Vec<Shape>, String> {
    let items = match value {
        None | Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items,
        Some(_) => return Err("shapes 必须是数组".into()),
    };
    if items.len() > shapes::MAX_SHAPES {
        return Err(format!("图形最多 {} 个", shapes::MAX_SHAPES));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let what = format!("第 {} 个图形", index + 1);
            let object = item
                .as_object()
                .ok_or_else(|| format!("{what}必须是对象"))?;
            let kind_name = object
                .get("kind")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            let kind = match kind_name {
                "circle" => ShapeKind::Circle,
                "ellipse" => ShapeKind::Ellipse,
                "rect" => ShapeKind::Rect {
                    corner_radius_mm: number(object, "corner_radius_mm", &what)?.unwrap_or(0.0),
                },
                "polygon" => ShapeKind::Polygon {
                    sides: integer(object, "sides", &what, 6)?,
                },
                "star" => {
                    let points = integer(object, "points", &what, 5)?;
                    ShapeKind::Star {
                        points,
                        inner_ratio: number(object, "inner_ratio", &what)?
                            .unwrap_or_else(|| shapes::regular_star_ratio(points)),
                    }
                }
                "heart" => ShapeKind::Heart,
                "line" => ShapeKind::Line,
                other => {
                    return Err(format!(
                        "{what}的 kind 只能是 circle / ellipse / rect / polygon / star / heart / line，收到「{other}」"
                    ));
                }
            };
            let size = number(object, "size_mm", &what)?;
            let width = number(object, "width_mm", &what)?
                .or(number(object, "length_mm", &what)?)
                .or(size);
            let height = number(object, "height_mm", &what)?.or(size);
            let (width, height) = match (kind, width, height) {
                // 圆只看直径
                (ShapeKind::Circle, Some(d), _) | (ShapeKind::Circle, None, Some(d)) => (d, d),
                (ShapeKind::Line, Some(l), _) => (l, 0.0),
                (_, Some(w), Some(h)) => (w, h),
                // 只给一边：正多边形、星形、爱心按等比
                (ShapeKind::Polygon { .. } | ShapeKind::Star { .. }, Some(v), None)
                | (ShapeKind::Polygon { .. } | ShapeKind::Star { .. }, None, Some(v)) => (v, v),
                (ShapeKind::Heart, Some(w), None) => (w, w * 0.9),
                (ShapeKind::Heart, None, Some(h)) => (h / 0.9, h),
                _ => {
                    return Err(format!(
                        "{what}（{}）缺少尺寸：给 size_mm，或 width_mm 与 height_mm{}",
                        kind.label(),
                        if kind == ShapeKind::Line {
                            "（直线给 length_mm）"
                        } else {
                            ""
                        }
                    ));
                }
            };
            let shape = Shape {
                kind,
                x_mm: number(object, "x_mm", &what)?.unwrap_or(0.0),
                y_mm: number(object, "y_mm", &what)?.unwrap_or(0.0),
                width_mm: width,
                height_mm: height,
                rotation_deg: number(object, "rotation_deg", &what)?.unwrap_or(0.0),
                fill: fill_flag(object.get("fill"), &what)?,
            };
            shape.validate().map_err(|e| format!("{what}：{e}"))?;
            Ok(shape)
        })
        .collect()
}

/// 解析 `border`。
pub(crate) fn parse_border(value: Option<&serde_json::Value>) -> Result<Option<Border>, String> {
    let object = match value {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Object(object)) => object,
        Some(_) => return Err("border 必须是对象".into()),
    };
    let what = "边框";
    let shape = BorderShape::parse(
        object
            .get("shape")
            .and_then(|v| v.as_str())
            .unwrap_or("rect"),
    )?;
    let double = match object.get("style").and_then(|v| v.as_str()).map(str::trim) {
        None | Some("") | Some("single") => false,
        Some("double") => true,
        Some(other) => {
            return Err(format!("border.style 只能是 single / double，收到 {other}"));
        }
    };
    Ok(Some(Border {
        shape,
        double,
        inset_mm: number(object, "inset_mm", what)?.unwrap_or(shapes::DEFAULT_BORDER_INSET_MM),
        gap_mm: number(object, "gap_mm", what)?.unwrap_or(shapes::DEFAULT_BORDER_GAP_MM),
        corner_radius_mm: number(object, "corner_radius_mm", what)?,
    }))
}

/// 解析 `text_box`。
pub(crate) fn parse_text_box(
    value: Option<&serde_json::Value>,
) -> Result<Option<laser_core::engine::TextBox>, String> {
    let object = match value {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Object(object)) => object,
        Some(_) => return Err("text_box 必须是对象".into()),
    };
    let what = "text_box ";
    let (Some(width_mm), Some(height_mm)) = (
        number(object, "width_mm", what)?,
        number(object, "height_mm", what)?,
    ) else {
        return Err("text_box 要给 width_mm 和 height_mm".into());
    };
    Ok(Some(laser_core::engine::TextBox {
        x_mm: number(object, "x_mm", what)?.unwrap_or(0.0),
        y_mm: number(object, "y_mm", what)?.unwrap_or(0.0),
        width_mm,
        height_mm,
    }))
}

/// 回显实际使用的图形（尺寸已按默认值补齐）。
pub(crate) fn shapes_json(shapes: &[Shape]) -> serde_json::Value {
    serde_json::Value::Array(
        shapes
            .iter()
            .map(|shape| {
                let mut item = serde_json::json!({
                    "kind": shape.kind.name(),
                    "x_mm": shape.x_mm,
                    "y_mm": shape.y_mm,
                    "width_mm": shape.width_mm,
                    "height_mm": shape.height_mm,
                    "rotation_deg": shape.rotation_deg,
                    "fill": if shape.fill { "fill" } else { "outline" },
                });
                match shape.kind {
                    ShapeKind::Rect { corner_radius_mm } => {
                        item["corner_radius_mm"] = serde_json::json!(corner_radius_mm);
                    }
                    ShapeKind::Polygon { sides } => item["sides"] = serde_json::json!(sides),
                    ShapeKind::Star {
                        points,
                        inner_ratio,
                    } => {
                        item["points"] = serde_json::json!(points);
                        item["inner_ratio"] = serde_json::json!(inner_ratio);
                    }
                    _ => {}
                }
                item
            })
            .collect(),
    )
}

pub(crate) fn border_json(border: &Border) -> serde_json::Value {
    serde_json::json!({
        "shape": border.shape.as_str(),
        "style": if border.double { "double" } else { "single" },
        "inset_mm": border.inset_mm,
        "gap_mm": border.gap_mm,
        "corner_radius_mm": border.corner_radius_mm,
    })
}
