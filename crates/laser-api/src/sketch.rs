//! 创意画板直接转矢量刀路（2026-11-16 用户决定：画板默认用矢量规划路线）。
//!
//! 画板是 Excalidraw，元素本身就是矢量：这里把场景里的元素换成刀路几何（画板坐标，y 向下），
//! 再由调用方按成品尺寸换算到毫米、交给 [`laser_core::vector_plan`] 规划：
//! - 手绘笔画（`freedraw`）按中心线一笔刻，不闭合（原先导出成图片再描边，一笔会被描成两条边）；
//! - 直线 / 箭头（`line` / `arrow`）按点连线，首尾相接的多边形算闭合；箭头补两笔箭头头部；
//! - 矩形 / 菱形 / 椭圆按形状描边（圆角按直角处理）；元素的旋转角一并换算；
//! - 有填充色（`backgroundColor` 不透明）的闭合图形用矢量排线填满；
//! - 文字元素用系统字体转成字形轮廓，填满并描边（画板手写体换成系统字体，字形会不同）；
//! - 插入的图片矢量刻不了：返回 `has_image`，由调用方改用光栅。

use laser_contracts::geometry::Point;
use serde_json::Value;

/// 场景转出的矢量几何（画板坐标）。
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct SketchGeometry {
    /// 要描的闭合轮廓
    pub closed: Vec<Vec<Point>>,
    /// 要描的开放线段
    pub open: Vec<Vec<Point>>,
    /// 要填满的闭合图形（每个图形单独按奇偶规则填）
    pub filled_shapes: Vec<Vec<Point>>,
    /// 要填满的文字（同一段文字的字形轮廓一起按非零规则填，字里的孔不填）
    pub filled_text: Vec<Vec<Vec<Point>>>,
    /// 场景里有插入的图片（矢量刻不了）
    pub has_image: bool,
}

impl SketchGeometry {
    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
            && self.open.is_empty()
            && self.filled_shapes.is_empty()
            && self.filled_text.is_empty()
    }

    /// 有要扫描填充的图形或文字（过扫描要让出位置）
    pub fn has_fills(&self) -> bool {
        !self.filled_shapes.is_empty() || !self.filled_text.is_empty()
    }

    /// 所有几何点的范围（min_x, min_y, max_x, max_y）。
    pub fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut points = self
            .closed
            .iter()
            .chain(&self.open)
            .chain(&self.filled_shapes)
            .flatten()
            .chain(self.filled_text.iter().flatten().flatten())
            .peekable();
        points.peek()?;
        Some(points.fold(
            (
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ),
            |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)),
        ))
    }

    /// 对每个点做同一个变换。
    pub fn map(&self, f: impl Fn(Point) -> Point) -> Self {
        let each = |paths: &Vec<Vec<Point>>| -> Vec<Vec<Point>> {
            paths
                .iter()
                .map(|path| path.iter().map(|p| f(*p)).collect())
                .collect()
        };
        Self {
            closed: each(&self.closed),
            open: each(&self.open),
            filled_shapes: each(&self.filled_shapes),
            filled_text: self.filled_text.iter().map(each).collect(),
            has_image: self.has_image,
        }
    }
}

/// 颜色是否可见（`transparent`、空串或 alpha 为 0 都不可见）。
fn visible_color(value: Option<&Value>) -> bool {
    match value.and_then(Value::as_str).map(str::trim) {
        None | Some("") => false,
        Some(color) if color.eq_ignore_ascii_case("transparent") => false,
        Some(color) => {
            let hex = color.trim_start_matches('#');
            // #rrggbbaa / #rgba 的透明度为 0 时不可见
            !((hex.len() == 8 && hex.ends_with("00")) || (hex.len() == 4 && hex.ends_with('0')))
        }
    }
}

fn number(element: &Value, key: &str) -> f64 {
    element.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// 绕中心旋转（弧度，与 Excalidraw 一致：y 向下时为顺时针）。
fn rotate(points: Vec<Point>, center: Point, angle: f64) -> Vec<Point> {
    if angle.abs() < 1e-12 {
        return points;
    }
    let (sin, cos) = angle.sin_cos();
    points
        .into_iter()
        .map(|(x, y)| {
            let (dx, dy) = (x - center.0, y - center.1);
            (
                center.0 + dx * cos - dy * sin,
                center.1 + dx * sin + dy * cos,
            )
        })
        .collect()
}

fn bbox_center(points: &[Point]) -> Point {
    let (min_x, min_y, max_x, max_y) = points.iter().fold(
        (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ),
        |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)),
    );
    ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0)
}

fn closed_polyline(points: &[Point]) -> bool {
    points.len() > 2 && {
        let (a, b) = (points[0], points[points.len() - 1]);
        (a.0 - b.0).hypot(a.1 - b.1) < 1e-6
    }
}

/// 箭头头部：在 `tip` 处朝 `from → tip` 方向画两笔。
fn arrow_head(from: Point, tip: Point, length: f64) -> [Vec<Point>; 2] {
    let angle = (tip.1 - from.1).atan2(tip.0 - from.0);
    let wing = |offset: f64| {
        let a = angle + std::f64::consts::PI + offset;
        vec![(tip.0 + length * a.cos(), tip.1 + length * a.sin()), tip]
    };
    [wing(0.45), wing(-0.45)]
}

/// 场景（`{"elements": [...]}` 或直接是元素数组）转成矢量几何。`font_for` 为文字找可用字体。
pub(crate) fn sketch_from_scene(
    scene: &Value,
    font_for: &dyn Fn(&str) -> Option<std::path::PathBuf>,
) -> Result<SketchGeometry, String> {
    let elements = scene
        .get("elements")
        .and_then(Value::as_array)
        .or_else(|| scene.as_array())
        .ok_or("画板场景里没有图形")?;
    let mut out = SketchGeometry::default();
    for element in elements {
        if element.get("isDeleted").and_then(Value::as_bool) == Some(true)
            || element
                .get("opacity")
                .and_then(Value::as_f64)
                .is_some_and(|o| o <= 0.0)
        {
            continue;
        }
        let kind = element.get("type").and_then(Value::as_str).unwrap_or("");
        let (x, y) = (number(element, "x"), number(element, "y"));
        let (w, h) = (number(element, "width"), number(element, "height"));
        let angle = number(element, "angle");
        let stroke = visible_color(element.get("strokeColor"))
            && element
                .get("strokeWidth")
                .and_then(Value::as_f64)
                .is_none_or(|width| width > 0.0);
        let fill = visible_color(element.get("backgroundColor"));
        match kind {
            "freedraw" | "line" | "arrow" => {
                let points: Vec<Point> = element
                    .get("points")
                    .and_then(Value::as_array)
                    .map(|points| {
                        points
                            .iter()
                            .filter_map(|p| {
                                let p = p.as_array()?;
                                Some((x + p.first()?.as_f64()?, y + p.get(1)?.as_f64()?))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if points.len() < 2 {
                    // 只有一个点的手绘（点了一下）：画一个极短的点
                    if let Some(p) = points.first().copied().filter(|_| stroke) {
                        out.open.push(vec![p, (p.0 + 0.5, p.1)]);
                    }
                    continue;
                }
                let center = bbox_center(&points);
                let points = rotate(points, center, angle);
                let closed = kind != "freedraw" && closed_polyline(&points);
                if closed && fill {
                    out.filled_shapes.push(points.clone());
                }
                if stroke {
                    if kind == "arrow" {
                        let length = element
                            .get("strokeWidth")
                            .and_then(Value::as_f64)
                            .unwrap_or(2.0)
                            .mul_add(4.0, 8.0);
                        let head = |key: &str, default: bool| match element.get(key) {
                            Some(Value::Null) => false,
                            Some(_) => true,
                            None => default,
                        };
                        if head("endArrowhead", true) {
                            let n = points.len();
                            out.open
                                .extend(arrow_head(points[n - 2], points[n - 1], length));
                        }
                        if head("startArrowhead", false) {
                            out.open.extend(arrow_head(points[1], points[0], length));
                        }
                    }
                    if closed {
                        out.closed.push(points);
                    } else {
                        out.open.push(points);
                    }
                }
            }
            "rectangle" | "diamond" | "ellipse" => {
                if w.abs() < 1e-9 || h.abs() < 1e-9 {
                    continue;
                }
                let (x0, y0) = (x.min(x + w), y.min(y + h));
                let (w, h) = (w.abs(), h.abs());
                let shape: Vec<Point> = match kind {
                    "rectangle" => vec![
                        (x0, y0),
                        (x0 + w, y0),
                        (x0 + w, y0 + h),
                        (x0, y0 + h),
                        (x0, y0),
                    ],
                    "diamond" => vec![
                        (x0 + w / 2.0, y0),
                        (x0 + w, y0 + h / 2.0),
                        (x0 + w / 2.0, y0 + h),
                        (x0, y0 + h / 2.0),
                        (x0 + w / 2.0, y0),
                    ],
                    _ => {
                        // 椭圆按周长取点，至少 48 个，最多 360 个
                        let perimeter = std::f64::consts::PI * (w + h) / 2.0;
                        let steps = (perimeter / 2.0).clamp(48.0, 360.0) as usize;
                        let mut ring: Vec<Point> = (0..steps)
                            .map(|i| {
                                let t = i as f64 / steps as f64 * std::f64::consts::TAU;
                                (
                                    x0 + w / 2.0 * (1.0 + t.cos()),
                                    y0 + h / 2.0 * (1.0 + t.sin()),
                                )
                            })
                            .collect();
                        ring.push(ring[0]);
                        ring
                    }
                };
                let shape = rotate(shape, (x0 + w / 2.0, y0 + h / 2.0), angle);
                if fill {
                    out.filled_shapes.push(shape.clone());
                }
                if stroke {
                    out.closed.push(shape);
                }
            }
            "text" => {
                let text = element
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if text.is_empty() || !stroke {
                    continue;
                }
                let font_size = element
                    .get("fontSize")
                    .and_then(Value::as_f64)
                    .filter(|size| *size > 0.0)
                    .unwrap_or(20.0);
                let font = font_for(text).ok_or("找不到能显示画板文字的字体")?;
                let data = std::fs::read(&font).map_err(|_| "画板文字的字体读不出来")?;
                let layout = laser_core::font::layout_text_advanced(
                    &data,
                    0,
                    text,
                    &laser_core::font::TextLayoutOptions {
                        height_mm: font_size,
                        alignment: match element.get("textAlign").and_then(Value::as_str) {
                            Some("center") => laser_core::font::TextAlignment::Center,
                            Some("right") => laser_core::font::TextAlignment::Right,
                            _ => laser_core::font::TextAlignment::Left,
                        },
                        ..Default::default()
                    },
                )
                .map_err(|error| format!("画板文字转轮廓失败：{error}"))?;
                // 字形块放进文字元素的框：等比缩放、左上对齐
                let scale = if layout.width > 0.0 && layout.height > 0.0 && w > 0.0 && h > 0.0 {
                    (w / layout.width).min(h / layout.height)
                } else {
                    1.0
                };
                let center = (x + w / 2.0, y + h / 2.0);
                let glyphs: Vec<Vec<Point>> = layout
                    .paths
                    .iter()
                    .map(|path| {
                        let placed = path
                            .iter()
                            .map(|(px, py)| (x + px * scale, y + py * scale))
                            .collect();
                        rotate(placed, center, angle)
                    })
                    .collect();
                out.closed.extend(glyphs.iter().cloned());
                out.filled_text.push(glyphs);
            }
            "image" => out.has_image = true,
            // 框架、嵌入网页等不是要刻的图形
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn no_font(_: &str) -> Option<std::path::PathBuf> {
        None
    }

    #[test]
    fn freehand_strokes_stay_open_centerlines_and_shapes_are_closed() {
        let scene = json!({"elements": [
            {"type": "freedraw", "x": 10, "y": 10, "points": [[0, 0], [5, 2], [10, 0]], "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 2},
            {"type": "rectangle", "x": 0, "y": 0, "width": 20, "height": 10, "strokeColor": "#1e1e1e", "backgroundColor": "#ffc9c9", "strokeWidth": 2},
            {"type": "ellipse", "x": 30, "y": 0, "width": 10, "height": 10, "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 1},
            {"type": "rectangle", "x": 0, "y": 0, "width": 5, "height": 5, "isDeleted": true, "strokeColor": "#000"},
        ]});
        let sketch = sketch_from_scene(&scene, &no_font).unwrap();
        assert_eq!(
            sketch.open,
            vec![vec![(10.0, 10.0), (15.0, 12.0), (20.0, 10.0)]]
        );
        assert_eq!(sketch.closed.len(), 2, "矩形与椭圆描边");
        assert_eq!(sketch.filled_shapes.len(), 1, "只有矩形有填充色");
        assert!(!sketch.has_image);
    }

    #[test]
    fn arrows_get_heads_and_rotation_is_applied() {
        let scene = json!([
            {"type": "arrow", "x": 0, "y": 0, "points": [[0, 0], [100, 0]], "strokeColor": "#000", "strokeWidth": 1, "endArrowhead": "arrow", "startArrowhead": null},
            {"type": "rectangle", "x": 0, "y": 0, "width": 10, "height": 10, "angle": std::f64::consts::FRAC_PI_2, "strokeColor": "#000", "backgroundColor": "transparent"},
        ]);
        let sketch = sketch_from_scene(&scene, &no_font).unwrap();
        assert_eq!(sketch.open.len(), 3, "箭杆 + 两笔箭头");
        // 正方形绕中心转 90° 后仍占同一范围
        let (min_x, min_y, max_x, max_y) = SketchGeometry {
            closed: sketch.closed.clone(),
            ..Default::default()
        }
        .bounds()
        .unwrap();
        assert!((min_x - 0.0).abs() < 1e-9 && (max_x - 10.0).abs() < 1e-9);
        assert!((min_y - 0.0).abs() < 1e-9 && (max_y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn images_are_flagged_and_invisible_strokes_skipped() {
        let scene = json!({"elements": [
            {"type": "image", "x": 0, "y": 0, "width": 10, "height": 10},
            {"type": "line", "x": 0, "y": 0, "points": [[0, 0], [5, 5]], "strokeColor": "transparent"},
        ]});
        let sketch = sketch_from_scene(&scene, &no_font).unwrap();
        assert!(sketch.has_image);
        assert!(sketch.is_empty());
        assert!(sketch_from_scene(&json!({"nothing": 1}), &no_font).is_err());
    }
}
