//! 创意画板直接转矢量刀路（2026-11-16 用户决定：画板默认用矢量规划路线）。
//!
//! 画板是 Excalidraw，元素本身就是矢量：这里把场景里的元素换成刀路几何（画板坐标，y 向下），
//! 再由调用方按成品尺寸换算到毫米、交给 [`laser_core::vector_plan`] 规划：
//! - 手绘笔画（`freedraw`）按中心线一笔刻，不闭合（原先导出成图片再描边，一笔会被描成两条边）；
//! - 直线 / 箭头（`line` / `arrow`）按点连线，首尾相接的多边形算闭合；箭头补两笔箭头头部；
//! - 矩形 / 菱形 / 椭圆按形状描边（圆角按直角处理）；元素的旋转角一并换算；
//! - 有填充色（`backgroundColor` 不透明）的闭合图形用矢量排线填满；
//! - 文字元素用系统字体转成字形轮廓，先焊接（交叠笔画合成一笔，2026-11-17 技术书 r44），再填满并描边
//!   （画板手写体换成系统字体，字形会不同）；
//! - 插入的图片矢量刻不了：返回 `has_image`，由调用方改用光栅。

use laser_contracts::geometry::Point;
use serde_json::Value;

/// 矢量模式的刻法（2026-11-17 用户决定，技术书 r46：“轮廓和填充可以选择”）。
/// 只影响闭合图形（矩形、菱形、椭圆、首尾相接的直线）与文字；手绘笔画、开放直线与箭头始终刻一笔。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum VectorStyle {
    /// 按画布颜色（默认，原行为）：设了背景色的图形填满，线条颜色可见的描边；文字填满并描边
    #[default]
    Canvas,
    /// 轮廓：闭合图形与文字只描边（只设了背景色、没有线条颜色的图形也描边）
    Outline,
    /// 填充：闭合图形与文字都填满并描边
    Fill,
    /// 切割（r47）：沿闭合图形与文字轮廓切穿（先内后外，可带切缝、桥位、引线、过切），开放线条沿线切
    Cut,
}

impl VectorStyle {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None | Some("canvas") => Ok(Self::Canvas),
            Some("outline") => Ok(Self::Outline),
            Some("fill") => Ok(Self::Fill),
            Some("cut") => Ok(Self::Cut),
            Some(other) => Err(format!(
                "vector_style 只能是 canvas / outline / fill / cut，收到 {other}"
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Outline => "outline",
            Self::Fill => "fill",
            Self::Cut => "cut",
        }
    }

    /// 闭合图形要不要填满、要不要描边（`stroke` / `fill`：线条颜色、背景色是否可见）。
    fn closed_shape(self, stroke: bool, fill: bool) -> (bool, bool) {
        let visible = stroke || fill;
        match self {
            Self::Canvas => (fill, stroke),
            Self::Outline | Self::Cut => (false, visible),
            Self::Fill => (visible, visible),
        }
    }
}

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
    /// 切割刻法下的文字（同一段文字的字形轮廓一组，切缝补偿按组整体做，字里的孔反向补偿）
    pub cut_text: Vec<Vec<Vec<Point>>>,
    /// 场景里有插入的图片（矢量刻不了）
    pub has_image: bool,
}

impl SketchGeometry {
    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
            && self.open.is_empty()
            && self.filled_shapes.is_empty()
            && self.filled_text.is_empty()
            && self.cut_text.is_empty()
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
            .chain(self.cut_text.iter().flatten().flatten())
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
            cut_text: self.cut_text.iter().map(each).collect(),
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

/// 导入 SVG 的矢量数据上限：防止异常文件让规划卡死（与前端 `svgImport.js` 的采样上限一致）。
const MAX_VECTOR_IMAGE_POINTS: usize = 400_000;
const MAX_VECTOR_IMAGE_PATHS: usize = 20_000;

/// 解析 `[[x, y], ...]`；坐标须是有限数字。
fn vector_points(value: &Value) -> Result<Vec<Point>, String> {
    let bad = || "导入的 SVG 矢量数据损坏（坐标不是数字），请重新导入".to_owned();
    value
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|p| {
            let p = p.as_array().ok_or_else(bad)?;
            let x = p.first().and_then(Value::as_f64).filter(|v| v.is_finite());
            let y = p.get(1).and_then(Value::as_f64).filter(|v| v.is_finite());
            match (x, y) {
                (Some(x), Some(y)) => Ok((x, y)),
                _ => Err(bad()),
            }
        })
        .collect()
}

/// 图片元素上的导入 SVG 矢量（`customData.moss_vector`，版本 1）放进元素框，返回是否处理了。
///
/// 数据格式（前端 `svgImport.js::buildVectorData` 生成）：`size` 为轮廓原始尺寸（mm），
/// `paths[]` 每项 `rings`（闭合环，同一项内按非零规则一起填，孔已按方向区分）、`open`（开放线）、
/// `fill` / `stroke`（SVG 原样式是否可见）。坐标为相对轮廓左上角的 mm，按元素框等比拉伸；
/// 元素的翻转（`scale` 为负）与旋转角一并换算。
fn push_vector_image(
    element: &Value,
    (x, y, w, h): (f64, f64, f64, f64),
    angle: f64,
    style: VectorStyle,
    out: &mut SketchGeometry,
) -> Result<bool, String> {
    let Some(data) = element.get("customData").and_then(|c| c.get("moss_vector")) else {
        return Ok(false);
    };
    if data.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("导入的 SVG 矢量数据版本不认识，请重新导入".into());
    }
    let (src_w, src_h) = match data.get("size").and_then(Value::as_array) {
        Some(pair) if pair.len() == 2 => (
            pair[0].as_f64().filter(|v| v.is_finite() && *v >= 0.0),
            pair[1].as_f64().filter(|v| v.is_finite() && *v >= 0.0),
        ),
        _ => (None, None),
    };
    let (Some(src_w), Some(src_h)) = (src_w, src_h) else {
        return Err("导入的 SVG 矢量数据缺少尺寸，请重新导入".into());
    };
    let paths = data
        .get("paths")
        .and_then(Value::as_array)
        .ok_or("导入的 SVG 矢量数据缺少轮廓，请重新导入")?;
    if paths.len() > MAX_VECTOR_IMAGE_PATHS {
        return Err(format!(
            "导入的 SVG 图形太多（{} 个，上限 {MAX_VECTOR_IMAGE_PATHS}），请先在设计软件里合并或简化",
            paths.len()
        ));
    }
    // 等比拉伸到元素框；某一边为 0（一条横线）时用另一边的比例
    let (w, h) = (w.abs(), h.abs());
    let sx = if src_w > 1e-9 {
        w / src_w
    } else {
        h / src_h.max(1e-9)
    };
    let sy = if src_h > 1e-9 { h / src_h } else { sx };
    let flip = |i: usize| {
        element
            .get("scale")
            .and_then(Value::as_array)
            .and_then(|s| s.get(i))
            .and_then(Value::as_f64)
            .is_some_and(|v| v < 0.0)
    };
    let (flip_x, flip_y) = (flip(0), flip(1));
    let center = (x + w / 2.0, y + h / 2.0);
    let place = |points: Vec<Point>| -> Vec<Point> {
        let placed = points
            .into_iter()
            .map(|(px, py)| {
                let u = if flip_x { src_w - px } else { px };
                let v = if flip_y { src_h - py } else { py };
                (x + u * sx, y + v * sy)
            })
            .collect();
        rotate(placed, center, angle)
    };
    let mut total = 0usize;
    let mut count = |points: &[Point]| -> Result<(), String> {
        total += points.len();
        if total > MAX_VECTOR_IMAGE_POINTS {
            return Err(format!(
                "导入的 SVG 轮廓点数超过 {MAX_VECTOR_IMAGE_POINTS}，请先在设计软件里简化路径"
            ));
        }
        Ok(())
    };
    for path in paths {
        let flag = |key: &str| path.get(key).and_then(Value::as_bool).unwrap_or(false);
        let (fill, stroke) = (flag("fill"), flag("stroke"));
        let mut rings = Vec::new();
        for ring in path
            .get("rings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let mut ring = vector_points(ring)?;
            count(&ring)?;
            if ring.len() < 3 {
                continue;
            }
            if !closed_polyline(&ring) {
                ring.push(ring[0]);
            }
            rings.push(place(ring));
        }
        let mut open = Vec::new();
        for line in path
            .get("open")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let line = vector_points(line)?;
            count(&line)?;
            if line.len() >= 2 {
                open.push(place(line));
            }
        }
        if !rings.is_empty() {
            let (fill_it, stroke_it) = style.closed_shape(stroke, fill);
            match style {
                // 切割：同一图形的外轮廓与孔整组切（切缝补偿按组做，孔反向补偿），与文字相同
                VectorStyle::Cut => {
                    if stroke_it {
                        out.cut_text.push(rings);
                    }
                }
                _ => {
                    if fill_it {
                        out.filled_text.push(rings.clone());
                    }
                    if stroke_it {
                        out.closed.extend(rings);
                    }
                }
            }
        }
        // 开放线只看线条是否可见（与画板直线一致；填充图形的子路径前端已按闭合处理）
        if stroke {
            out.open.extend(open);
        }
    }
    Ok(true)
}

/// 场景（`{"elements": [...]}` 或直接是元素数组）转成矢量几何。`font_for` 为文字找可用字体。
/// 只留选中的元素（2026-11-16，对齐 LightBurn“只加工选中图形”）：`id` 在选中集合里，
/// 或绑在选中容器上的文字（`containerId`）。其余字段原样保留。
pub(crate) fn select_elements(scene: &Value, ids: &std::collections::HashSet<String>) -> Value {
    let keep = |element: &Value| {
        let field = |key: &str| element.get(key).and_then(Value::as_str);
        field("id").is_some_and(|id| ids.contains(id))
            || field("containerId").is_some_and(|id| ids.contains(id))
    };
    let filter = |elements: &Vec<Value>| -> Vec<Value> {
        elements.iter().filter(|e| keep(e)).cloned().collect()
    };
    match scene {
        Value::Array(elements) => Value::Array(filter(elements)),
        Value::Object(map) => {
            let mut out = map.clone();
            if let Some(Value::Array(elements)) = map.get("elements") {
                out.insert("elements".into(), Value::Array(filter(elements)));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

pub(crate) fn sketch_from_scene(
    scene: &Value,
    font_for: &dyn Fn(&str) -> Option<std::path::PathBuf>,
    style: VectorStyle,
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
                // 首尾相接的直线是闭合图形，按刻法决定填满 / 描边；其余只看线条颜色
                let (fill_it, stroke_it) = if closed {
                    style.closed_shape(stroke, fill)
                } else {
                    (false, stroke)
                };
                if fill_it {
                    out.filled_shapes.push(points.clone());
                }
                if stroke_it {
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
                let (fill_it, stroke_it) = style.closed_shape(stroke, fill);
                if fill_it {
                    out.filled_shapes.push(shape.clone());
                }
                if stroke_it {
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
                // 焊接（2026-11-17 技术书 r44，与文字任务相同）：交叠的笔画合成一笔，描边时交叠处不多一道线；
                // 并集与缩放、旋转无关，在字形坐标里做即可
                let (welded, _) = laser_core::engine::weld_contours(&layout.paths)
                    .map_err(|error| format!("画板文字合并笔画失败：{error}"))?;
                // 字形块放进文字元素的框：等比缩放、左上对齐
                let scale = if layout.width > 0.0 && layout.height > 0.0 && w > 0.0 && h > 0.0 {
                    (w / layout.width).min(h / layout.height)
                } else {
                    1.0
                };
                let center = (x + w / 2.0, y + h / 2.0);
                let glyphs: Vec<Vec<Point>> = welded
                    .iter()
                    .map(|path| {
                        let placed = path
                            .iter()
                            .map(|(px, py)| (x + px * scale, y + py * scale))
                            .collect();
                        rotate(placed, center, angle)
                    })
                    .collect();
                match style {
                    // 切割：文字整组切（切缝补偿要按组做，字里的孔反向）
                    VectorStyle::Cut => out.cut_text.push(glyphs),
                    // 轮廓：文字只描边
                    VectorStyle::Outline => out.closed.extend(glyphs),
                    _ => {
                        out.closed.extend(glyphs.iter().cloned());
                        out.filled_text.push(glyphs);
                    }
                }
            }
            // 导入的 SVG（2026-11-26）：图片元素带着 `customData.moss_vector` 矢量轮廓时按矢量刻，
            // 画布上看到的是 SVG 原图（孔洞、颜色都对），刀路用的是同一份轮廓
            // 摄像头参考底图（2026-11-26，`customData.moss_reference`）只作摆位参考，永远不雕刻
            "image"
                if element
                    .get("customData")
                    .and_then(|c| c.get("moss_reference"))
                    .and_then(Value::as_bool)
                    == Some(true) => {}
            "image" => {
                let vector = push_vector_image(element, (x, y, w, h), angle, style, &mut out)?;
                out.has_image |= !vector;
            }
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
        let sketch = sketch_from_scene(&scene, &no_font, VectorStyle::Canvas).unwrap();
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
        let sketch = sketch_from_scene(&scene, &no_font, VectorStyle::Canvas).unwrap();
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

    /// 画板文字转轮廓后先焊接（r44）：输出首尾点相同的闭合轮廓，填满与描边用同一组焊接后的轮廓。
    #[test]
    fn text_elements_become_welded_glyph_contours() {
        let font = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/fonts/geometry.ttf"
        ));
        let scene = json!([
            {"type": "text", "x": 0, "y": 0, "width": 40, "height": 20, "text": "AB", "fontSize": 20,
             "strokeColor": "#1e1e1e", "strokeWidth": 1},
        ]);
        let sketch =
            sketch_from_scene(&scene, &|_| Some(font.clone()), VectorStyle::Canvas).unwrap();
        assert!(!sketch.closed.is_empty());
        for ring in &sketch.closed {
            assert_eq!(ring.first(), ring.last(), "焊接输出闭合轮廓");
        }
        assert_eq!(sketch.filled_text.len(), 1);
        assert_eq!(sketch.filled_text[0], sketch.closed);
    }

    /// r46：刻法选择——轮廓只描边（只设背景色的图形也描边、文字不填），填充把闭合图形与文字都填满，
    /// 开放线条始终刻一笔；按画布颜色保持原行为。
    #[test]
    fn vector_style_switches_closed_shapes_between_outline_and_fill() {
        let scene = json!([
            {"type": "rectangle", "x": 0, "y": 0, "width": 20, "height": 10, "strokeColor": "#1e1e1e", "backgroundColor": "transparent", "strokeWidth": 1},
            {"type": "ellipse", "x": 30, "y": 0, "width": 10, "height": 10, "strokeColor": "transparent", "backgroundColor": "#ffc9c9"},
            {"type": "line", "x": 0, "y": 20, "points": [[0, 0], [10, 0]], "strokeColor": "#000", "strokeWidth": 1},
            {"type": "line", "x": 0, "y": 30, "points": [[0, 0], [10, 0], [5, 8], [0, 0]], "strokeColor": "#000", "strokeWidth": 1},
        ]);
        let canvas = sketch_from_scene(&scene, &no_font, VectorStyle::Canvas).unwrap();
        assert_eq!(
            (
                canvas.filled_shapes.len(),
                canvas.closed.len(),
                canvas.open.len()
            ),
            (1, 2, 1)
        );
        let outline = sketch_from_scene(&scene, &no_font, VectorStyle::Outline).unwrap();
        assert_eq!(
            (
                outline.filled_shapes.len(),
                outline.closed.len(),
                outline.open.len()
            ),
            (0, 3, 1)
        );
        let fill = sketch_from_scene(&scene, &no_font, VectorStyle::Fill).unwrap();
        assert_eq!(
            (fill.filled_shapes.len(), fill.closed.len(), fill.open.len()),
            (3, 3, 1)
        );

        let font = std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/fonts/geometry.ttf"
        ));
        let text = json!([{"type": "text", "x": 0, "y": 0, "width": 40, "height": 20, "text": "AB",
            "fontSize": 20, "strokeColor": "#1e1e1e", "strokeWidth": 1}]);
        let with_font = |style| sketch_from_scene(&text, &|_| Some(font.clone()), style).unwrap();
        assert!(
            with_font(VectorStyle::Outline).filled_text.is_empty(),
            "轮廓：文字只描边"
        );
        assert_eq!(with_font(VectorStyle::Fill).filled_text.len(), 1);
        assert_eq!(with_font(VectorStyle::Canvas).filled_text.len(), 1);
        // r47 切割：闭合图形都描（切），不填；文字整组放进 cut_text，不进 closed / filled_text
        let cut = sketch_from_scene(&scene, &no_font, VectorStyle::Cut).unwrap();
        assert_eq!(
            (cut.filled_shapes.len(), cut.closed.len(), cut.open.len()),
            (0, 3, 1)
        );
        let cut_text = with_font(VectorStyle::Cut);
        assert_eq!(cut_text.cut_text.len(), 1);
        assert!(cut_text.closed.is_empty() && cut_text.filled_text.is_empty());
        assert!(!cut_text.has_fills());
        assert!(VectorStyle::parse(Some("hatch")).is_err());
        assert_eq!(VectorStyle::parse(None).unwrap(), VectorStyle::Canvas);
    }

    fn svg_image(custom: serde_json::Value) -> serde_json::Value {
        json!({"elements": [
            {"type": "image", "x": 5, "y": 0, "width": 20, "height": 20, "customData": {"moss_vector": custom}},
        ]})
    }

    fn square_with_hole() -> serde_json::Value {
        json!({
            "version": 1,
            "size": [10, 10],
            "paths": [{
                "rings": [
                    [[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]],
                    [[3, 3], [3, 7], [7, 7], [7, 3], [3, 3]]
                ],
                "open": [],
                "fill": true,
                "stroke": false
            }, {
                "rings": [],
                "open": [[[0, 12], [10, 12]]],
                "fill": false,
                "stroke": true
            }]
        })
    }

    #[test]
    fn imported_svg_images_carve_as_vectors_with_holes_grouped() {
        let sketch = sketch_from_scene(
            &svg_image(square_with_hole()),
            &no_font,
            VectorStyle::Canvas,
        )
        .unwrap();
        assert!(!sketch.has_image, "带矢量数据的导入 SVG 不算位图");
        assert_eq!(sketch.filled_text.len(), 1, "外轮廓与孔是同一组非零填充");
        assert_eq!(sketch.filled_text[0].len(), 2);
        assert!(sketch.closed.is_empty(), "原样式只填不描");
        // 10 mm 的轮廓拉伸到 20 的元素框、左移 5
        assert_eq!(sketch.filled_text[0][0][1], (25.0, 0.0));
        assert_eq!(sketch.filled_text[0][1][2], (19.0, 14.0));
        assert_eq!(
            sketch.open,
            vec![vec![(5.0, 24.0), (25.0, 24.0)]],
            "描边的开放线照刻一笔"
        );

        let outline = sketch_from_scene(
            &svg_image(square_with_hole()),
            &no_font,
            VectorStyle::Outline,
        )
        .unwrap();
        assert!(outline.filled_text.is_empty());
        assert_eq!(outline.closed.len(), 2, "轮廓刻法：外轮廓与孔都描边");

        let cut =
            sketch_from_scene(&svg_image(square_with_hole()), &no_font, VectorStyle::Cut).unwrap();
        assert_eq!(cut.cut_text.len(), 1, "切割：外轮廓与孔整组切，孔反向补偿");
        assert_eq!(cut.cut_text[0].len(), 2);
        assert!(!cut.has_fills());
    }

    #[test]
    fn imported_svg_follows_element_flip_and_rotation() {
        let mut scene = svg_image(square_with_hole());
        scene["elements"][0]["scale"] = json!([-1, 1]);
        let flipped = sketch_from_scene(&scene, &no_font, VectorStyle::Canvas).unwrap();
        // 水平翻转：原 (10, 0) 落到框左边
        assert_eq!(flipped.filled_text[0][0][1], (5.0, 0.0));
        scene["elements"][0]["scale"] = json!([1, 1]);
        scene["elements"][0]["angle"] = json!(std::f64::consts::PI);
        let turned = sketch_from_scene(&scene, &no_font, VectorStyle::Canvas).unwrap();
        let (px, py) = turned.filled_text[0][0][0];
        assert!(
            (px - 25.0).abs() < 1e-9 && (py - 20.0).abs() < 1e-9,
            "转 180° 后左上角到右下角：{px},{py}"
        );
    }

    #[test]
    fn imported_svg_rejects_broken_or_oversized_data() {
        let mut bad = square_with_hole();
        bad["version"] = json!(2);
        assert!(
            sketch_from_scene(&svg_image(bad), &no_font, VectorStyle::Canvas)
                .unwrap_err()
                .contains("版本")
        );
        let mut bad = square_with_hole();
        bad["paths"][0]["rings"][0][1] = json!(["a", 0]);
        assert!(
            sketch_from_scene(&svg_image(bad), &no_font, VectorStyle::Canvas)
                .unwrap_err()
                .contains("损坏")
        );
        let mut bad = square_with_hole();
        bad.as_object_mut().unwrap().remove("size");
        assert!(
            sketch_from_scene(&svg_image(bad), &no_font, VectorStyle::Canvas)
                .unwrap_err()
                .contains("尺寸")
        );
        let huge: Vec<_> = (0..=MAX_VECTOR_IMAGE_POINTS)
            .map(|i| json!([i % 10, i % 7]))
            .collect();
        let big =
            json!({"version": 1, "size": [10, 10], "paths": [{"rings": [huge], "fill": true}]});
        assert!(
            sketch_from_scene(&svg_image(big), &no_font, VectorStyle::Canvas)
                .unwrap_err()
                .contains("点数")
        );
    }

    #[test]
    fn camera_reference_images_are_never_carved() {
        let scene = json!({"elements": [
            {"type": "image", "x": 0, "y": 0, "width": 480, "height": 360, "customData": {"moss_reference": true}},
            {"type": "rectangle", "x": 10, "y": 10, "width": 20, "height": 10, "strokeColor": "#000", "backgroundColor": "transparent"},
        ]});
        let sketch = sketch_from_scene(&scene, &no_font, VectorStyle::Canvas).unwrap();
        assert!(!sketch.has_image, "参考底图不算位图，矢量模式照常");
        assert_eq!(sketch.closed.len(), 1, "只刻矩形");
        assert_eq!(
            sketch.bounds(),
            Some((10.0, 10.0, 30.0, 20.0)),
            "排版范围不含参考底图"
        );
        let only_reference = json!({"elements": [
            {"type": "image", "x": 0, "y": 0, "width": 480, "height": 360, "customData": {"moss_reference": true}},
        ]});
        assert!(
            sketch_from_scene(&only_reference, &no_font, VectorStyle::Canvas)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn images_are_flagged_and_invisible_strokes_skipped() {
        let scene = json!({"elements": [
            {"type": "image", "x": 0, "y": 0, "width": 10, "height": 10},
            {"type": "line", "x": 0, "y": 0, "points": [[0, 0], [5, 5]], "strokeColor": "transparent"},
        ]});
        let sketch = sketch_from_scene(&scene, &no_font, VectorStyle::Canvas).unwrap();
        assert!(sketch.has_image);
        assert!(sketch.is_empty());
        assert!(sketch_from_scene(&json!({"nothing": 1}), &no_font, VectorStyle::Canvas).is_err());
    }
}
