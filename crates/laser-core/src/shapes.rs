//! 基础图形与边框（2026-11-17 用户决定，技术书 r44：“基础图形帮我加上”“直接用文字矢量然后转 gcode 来雕刻最好，
//! 这样雕刻好坏就跟雕刻机和材料库参数有关了”）。
//!
//! 大模型只说“要什么图形、多大、放哪”，轮廓一律由这里按公式算出来：圆是正圆、正多边形各边等长、
//! 五角星五个角一样长，曲线按 [`TOLERANCE_MM`] 弦高误差自适应拆成折线。坐标以木牌中心为原点，
//! x 向右、y 向上，单位 mm；旋转按逆时针。闭合图形一律输出逆时针（y 向上）、首尾点相同的轮廓。
//!
//! - [`ShapeKind`]：圆、椭圆、矩形（可圆角）、正多边形、星形、爱心、直线；除直线外都可选填满。
//! - [`Border`]：按木牌尺寸生成单线 / 双线边框（矩形、圆角矩形、圆、椭圆），并给出边框里面的文字区域。
//! - [`TextRegion`]：文字自动排版时要放进去的区域（矩形或椭圆），见 [`crate::engine`]。

use laser_contracts::geometry::Point;

/// 一次任务最多几个图形（含边框生成的线）。
pub const MAX_SHAPES: usize = 32;
/// 图形尺寸上下限（mm）。
pub const MIN_SHAPE_SIZE_MM: f64 = 0.5;
pub const MAX_SHAPE_SIZE_MM: f64 = 2000.0;
/// 曲线拆折线的弦高误差（mm）：远小于激光光斑（约 0.08 ~ 0.2 mm），刻出来看不出折线。
pub const TOLERANCE_MM: f64 = 0.01;
/// 正多边形边数、星形角数范围。
pub const MIN_SIDES: u32 = 3;
pub const MAX_SIDES: u32 = 24;
/// 边框默认离木牌边缘、双线间距（mm）。
pub const DEFAULT_BORDER_INSET_MM: f64 = 3.0;
pub const DEFAULT_BORDER_GAP_MM: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShapeKind {
    /// 圆：直径 = `width_mm`（`height_mm` 与之相同）
    Circle,
    Ellipse,
    /// 矩形，`corner_radius_mm` > 0 为圆角（不超过短边一半）
    Rect {
        corner_radius_mm: f64,
    },
    /// 正多边形，内接于 `width_mm × height_mm` 的椭圆，第一个顶点朝上
    Polygon {
        sides: u32,
    },
    /// 星形：外角内接于 `width_mm × height_mm` 的椭圆，内角半径为外角的 `inner_ratio` 倍，第一个角朝上
    Star {
        points: u32,
        inner_ratio: f64,
    },
    /// 爱心：正好放进 `width_mm × height_mm` 的框
    Heart,
    /// 直线：长 `width_mm`，默认水平，按 `rotation_deg` 转；不能填满
    Line,
}

impl ShapeKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Circle => "circle",
            Self::Ellipse => "ellipse",
            Self::Rect { .. } => "rect",
            Self::Polygon { .. } => "polygon",
            Self::Star { .. } => "star",
            Self::Heart => "heart",
            Self::Line => "line",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Circle => "圆",
            Self::Ellipse => "椭圆",
            Self::Rect { .. } => "矩形",
            Self::Polygon { .. } => "正多边形",
            Self::Star { .. } => "星形",
            Self::Heart => "爱心",
            Self::Line => "直线",
        }
    }
}

/// 正 n 角星（{n/2} 星形多边形）的内外半径比；n < 5 时取 0.4。
pub fn regular_star_ratio(points: u32) -> f64 {
    if points < 5 {
        return 0.4;
    }
    let n = f64::from(points);
    (2.0 * std::f64::consts::PI / n).cos() / (std::f64::consts::PI / n).cos()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub kind: ShapeKind,
    /// 中心（木牌中心为原点，x 向右、y 向上，mm）
    pub x_mm: f64,
    pub y_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
    /// 逆时针旋转角度
    pub rotation_deg: f64,
    /// 填满（矢量扫描线）；否则只描轮廓
    pub fill: bool,
}

/// 一个图形算出来的刀路几何。
#[derive(Debug, Clone, PartialEq)]
pub enum ShapeGeometry {
    /// 闭合轮廓（逆时针，首尾点相同）
    Closed(Vec<Point>),
    /// 开放线段（直线）
    Open(Vec<Point>),
}

impl ShapeGeometry {
    pub fn points(&self) -> &[Point] {
        match self {
            Self::Closed(points) | Self::Open(points) => points,
        }
    }
}

fn finite(v: f64) -> bool {
    v.is_finite()
}

impl Shape {
    /// 检查参数（尺寸、边数、圆角、比例、直线不能填满）。
    pub fn validate(&self) -> Result<(), String> {
        let label = self.kind.label();
        for (value, name) in [
            (self.x_mm, "x_mm"),
            (self.y_mm, "y_mm"),
            (self.rotation_deg, "rotation_deg"),
        ] {
            if !finite(value) {
                return Err(format!("{label}的 {name} 必须是数字"));
            }
        }
        let size_ok = |v: f64| finite(v) && (MIN_SHAPE_SIZE_MM..=MAX_SHAPE_SIZE_MM).contains(&v);
        if !size_ok(self.width_mm) {
            return Err(format!(
                "{label}的宽度（直径 / 长度）须在 {MIN_SHAPE_SIZE_MM} ~ {MAX_SHAPE_SIZE_MM} mm"
            ));
        }
        if self.kind != ShapeKind::Line && !size_ok(self.height_mm) {
            return Err(format!(
                "{label}的高度须在 {MIN_SHAPE_SIZE_MM} ~ {MAX_SHAPE_SIZE_MM} mm"
            ));
        }
        match self.kind {
            ShapeKind::Rect { corner_radius_mm } => {
                if !finite(corner_radius_mm) || corner_radius_mm < 0.0 {
                    return Err("矩形的 corner_radius_mm 不能为负数".into());
                }
            }
            ShapeKind::Polygon { sides } => {
                if !(MIN_SIDES..=MAX_SIDES).contains(&sides) {
                    return Err(format!("正多边形的边数须在 {MIN_SIDES} ~ {MAX_SIDES} 之间"));
                }
            }
            ShapeKind::Star {
                points,
                inner_ratio,
            } => {
                if !(MIN_SIDES..=MAX_SIDES).contains(&points) {
                    return Err(format!("星形的角数须在 {MIN_SIDES} ~ {MAX_SIDES} 之间"));
                }
                if !finite(inner_ratio) || !(0.1..=0.95).contains(&inner_ratio) {
                    return Err("星形的 inner_ratio 须在 0.1 ~ 0.95 之间".into());
                }
            }
            ShapeKind::Line if self.fill => {
                return Err("直线不能填满，请去掉 fill".into());
            }
            _ => {}
        }
        Ok(())
    }

    /// 按公式算出轮廓（坐标同 [`Shape`]：木牌中心为原点、y 向上）。
    pub fn geometry(&self) -> Result<ShapeGeometry, String> {
        self.validate()?;
        let (a, b) = (self.width_mm / 2.0, self.height_mm / 2.0);
        let local: ShapeGeometry = match self.kind {
            ShapeKind::Line => ShapeGeometry::Open(vec![(-a, 0.0), (a, 0.0)]),
            ShapeKind::Circle | ShapeKind::Ellipse => {
                ShapeGeometry::Closed(flatten_closed(|t| (a * t.cos(), b * t.sin()), 32))
            }
            ShapeKind::Rect { corner_radius_mm } => {
                ShapeGeometry::Closed(rounded_rect(a, b, corner_radius_mm.min(a.min(b))))
            }
            ShapeKind::Polygon { sides } => ShapeGeometry::Closed(
                (0..sides)
                    .map(|k| {
                        let t = std::f64::consts::FRAC_PI_2
                            + 2.0 * std::f64::consts::PI * f64::from(k) / f64::from(sides);
                        (a * t.cos(), b * t.sin())
                    })
                    .collect(),
            ),
            ShapeKind::Star {
                points,
                inner_ratio,
            } => ShapeGeometry::Closed(
                (0..2 * points)
                    .map(|k| {
                        let t = std::f64::consts::FRAC_PI_2
                            + std::f64::consts::PI * f64::from(k) / f64::from(points);
                        let r = if k % 2 == 0 { 1.0 } else { inner_ratio };
                        (r * a * t.cos(), r * b * t.sin())
                    })
                    .collect(),
            ),
            ShapeKind::Heart => ShapeGeometry::Closed(heart(a, b)),
        };
        let (sin, cos) = self.rotation_deg.to_radians().sin_cos();
        let place = |p: &Point| {
            (
                self.x_mm + p.0 * cos - p.1 * sin,
                self.y_mm + p.0 * sin + p.1 * cos,
            )
        };
        Ok(match local {
            ShapeGeometry::Open(points) => ShapeGeometry::Open(points.iter().map(place).collect()),
            ShapeGeometry::Closed(points) => {
                let mut ring: Vec<Point> = points.iter().map(place).collect();
                if signed_area(&ring) < 0.0 {
                    ring.reverse();
                }
                ring.push(ring[0]);
                ShapeGeometry::Closed(ring)
            }
        })
    }
}

/// 多边形有向面积（y 向上时逆时针为正；首尾点不重复）。
pub fn signed_area(ring: &[Point]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            p.0 * q.1 - q.0 * p.1
        })
        .sum::<f64>()
        / 2.0
}

fn midpoint_deviation(a: Point, m: Point, b: Point) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    if len < 1e-12 {
        return (m.0 - a.0).hypot(m.1 - a.1);
    }
    ((m.0 - a.0) * dy - (m.1 - a.1) * dx).abs() / len
}

/// 参数曲线 `f(t)`（`t` 从 `t0` 到 `t1`）按弦高误差自适应拆折线，追加到 `out`（不含起点）。
fn flatten_into(f: &impl Fn(f64) -> Point, t0: f64, t1: f64, depth: u32, out: &mut Vec<Point>) {
    let (a, b) = (f(t0), f(t1));
    let tm = (t0 + t1) / 2.0;
    let m = f(tm);
    // 再看两个四分点，避免对称曲线的中点恰好落在弦上
    let q1 = f((t0 + tm) / 2.0);
    let q3 = f((tm + t1) / 2.0);
    let deviation = midpoint_deviation(a, m, b)
        .max(midpoint_deviation(a, q1, b))
        .max(midpoint_deviation(a, q3, b));
    if deviation <= TOLERANCE_MM || depth >= 14 {
        out.push(b);
    } else {
        flatten_into(f, t0, tm, depth + 1, out);
        flatten_into(f, tm, t1, depth + 1, out);
    }
}

/// 闭合参数曲线（`t` ∈ [0, 2π)）拆成折线，先均分 `initial` 段再自适应细分；不重复首点。
fn flatten_closed(f: impl Fn(f64) -> Point, initial: u32) -> Vec<Point> {
    let tau = 2.0 * std::f64::consts::PI;
    let mut out = vec![f(0.0)];
    for k in 0..initial {
        let t0 = tau * f64::from(k) / f64::from(initial);
        let t1 = tau * f64::from(k + 1) / f64::from(initial);
        flatten_into(&f, t0, t1, 0, &mut out);
    }
    out.pop(); // 最后一点就是起点
    out
}

/// 以原点为中心、半宽 `a`、半高 `b`、圆角 `r` 的矩形（逆时针，从右下角圆角终点起）。
fn rounded_rect(a: f64, b: f64, r: f64) -> Vec<Point> {
    if r <= 1e-9 {
        return vec![(a, -b), (a, b), (-a, b), (-a, -b)];
    }
    let mut out = Vec::new();
    // 四个角的圆心与起始角：右上、左上、左下、右下
    let corners = [
        ((a - r, b - r), 0.0),
        ((-a + r, b - r), std::f64::consts::FRAC_PI_2),
        ((-a + r, -b + r), std::f64::consts::PI),
        ((a - r, -b + r), 3.0 * std::f64::consts::FRAC_PI_2),
    ];
    for ((cx, cy), start) in corners {
        let arc = |t: f64| (cx + r * t.cos(), cy + r * t.sin());
        out.push(arc(start));
        for k in 0..4 {
            let t0 = start + std::f64::consts::FRAC_PI_2 * f64::from(k) / 4.0;
            let t1 = start + std::f64::consts::FRAC_PI_2 * f64::from(k + 1) / 4.0;
            flatten_into(&arc, t0, t1, 0, &mut out);
        }
    }
    // 圆角恰好等于半边长时，相邻两段弧在边中点相接：去掉重复点
    out.dedup_by(|p, q| (p.0 - q.0).hypot(p.1 - q.1) < 1e-9);
    if out.len() > 1
        && (out[0].0 - out[out.len() - 1].0).hypot(out[0].1 - out[out.len() - 1].1) < 1e-9
    {
        out.pop();
    }
    out
}

/// 经典爱心曲线 x = 16 sin³t，y = 13 cos t − 5 cos 2t − 2 cos 3t − cos 4t，缩放到正好放进 2a × 2b 的框。
fn heart(a: f64, b: f64) -> Vec<Point> {
    let raw = |t: f64| {
        (
            16.0 * t.sin().powi(3),
            13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos(),
        )
    };
    // 原曲线的范围：x ∈ [-16, 16]，y 最低 -17（t = π），最高按采样求
    let (y_min, y_max) = (0..=4000).fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), k| {
        let y = raw(2.0 * std::f64::consts::PI * f64::from(k) / 4000.0).1;
        (lo.min(y), hi.max(y))
    });
    let (sx, sy) = (a / 16.0, 2.0 * b / (y_max - y_min));
    let cy = (y_max + y_min) / 2.0;
    flatten_closed(
        move |t| {
            let (x, y) = raw(t);
            (x * sx, (y - cy) * sy)
        },
        64,
    )
}

/// 边框形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderShape {
    Rect,
    Rounded,
    Circle,
    Ellipse,
}

impl BorderShape {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "rect" => Ok(Self::Rect),
            "rounded" => Ok(Self::Rounded),
            "circle" => Ok(Self::Circle),
            "ellipse" => Ok(Self::Ellipse),
            other => Err(format!(
                "border.shape 只能是 rect / rounded / circle / ellipse，收到 {other}"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rect => "rect",
            Self::Rounded => "rounded",
            Self::Circle => "circle",
            Self::Ellipse => "ellipse",
        }
    }
}

/// 沿木牌边缘的边框。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub shape: BorderShape,
    /// 双线
    pub double: bool,
    /// 外圈离木牌边缘（mm）
    pub inset_mm: f64,
    /// 双线两圈的间距（mm）
    pub gap_mm: f64,
    /// 圆角矩形的圆角半径（mm）；`None` 取短边的 12%
    pub corner_radius_mm: Option<f64>,
}

/// 文字自动排版要放进去的区域（坐标同 [`Shape`]）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextRegion {
    /// 矩形；`None` 的方向不限
    Rect {
        cx: f64,
        cy: f64,
        width: Option<f64>,
        height: Option<f64>,
    },
    /// 椭圆（半轴 `a`、`b`）
    Ellipse { cx: f64, cy: f64, a: f64, b: f64 },
}

impl TextRegion {
    pub fn center(&self) -> (f64, f64) {
        match *self {
            Self::Rect { cx, cy, .. } | Self::Ellipse { cx, cy, .. } => (cx, cy),
        }
    }

    /// 至少一个方向有限制。
    pub fn is_bounded(&self) -> bool {
        match *self {
            Self::Rect { width, height, .. } => width.is_some() || height.is_some(),
            Self::Ellipse { .. } => true,
        }
    }

    /// `w × h` 的文字框（以区域中心为中心）放得进去。
    pub fn fits(&self, w: f64, h: f64) -> bool {
        match *self {
            Self::Rect { width, height, .. } => {
                width.is_none_or(|a| w <= a + 1e-6) && height.is_none_or(|a| h <= a + 1e-6)
            }
            Self::Ellipse { .. } => self.ratio(w, h) >= 1.0 - 1e-9,
        }
    }

    /// 文字框最多还能放大的倍数（< 1 表示放不下）。
    pub fn ratio(&self, w: f64, h: f64) -> f64 {
        if !w.is_finite() || !h.is_finite() || w < 0.0 || h < 0.0 {
            return 0.0;
        }
        match *self {
            Self::Rect { width, height, .. } => {
                if width.is_some_and(|a| !a.is_finite() || a <= 0.0)
                    || height.is_some_and(|a| !a.is_finite() || a <= 0.0)
                {
                    return 0.0;
                }
                let rw = match width {
                    Some(a) if w > 1e-9 => a / w,
                    _ => f64::INFINITY,
                };
                let rh = match height {
                    Some(a) if h > 1e-9 => a / h,
                    _ => f64::INFINITY,
                };
                rw.min(rh)
            }
            Self::Ellipse { a, b, .. } => {
                if !a.is_finite() || !b.is_finite() || a <= 0.0 || b <= 0.0 {
                    return 0.0;
                }
                let s = ((w / 2.0 / a).powi(2) + (h / 2.0 / b).powi(2)).sqrt();
                if s <= 1e-9 { f64::INFINITY } else { 1.0 / s }
            }
        }
    }
}

impl Border {
    /// 按木牌尺寸生成边框线（只描轮廓），并返回边框里面留出 `margin_mm` 后的文字区域。
    pub fn build(
        &self,
        board_w: Option<f64>,
        board_h: Option<f64>,
        margin_mm: f64,
    ) -> Result<(Vec<Shape>, TextRegion), String> {
        if !finite(self.inset_mm) || self.inset_mm < 0.0 {
            return Err("border.inset_mm 不能为负数".into());
        }
        if !finite(self.gap_mm) || self.gap_mm <= 0.0 {
            return Err("border.gap_mm 必须大于 0".into());
        }
        let (w, h) = match (self.shape, board_w, board_h) {
            (BorderShape::Circle, Some(w), Some(h)) => (w.min(h), w.min(h)),
            (BorderShape::Circle, Some(d), None) | (BorderShape::Circle, None, Some(d)) => (d, d),
            (_, Some(w), Some(h)) => (w, h),
            _ => {
                return Err(if self.shape == BorderShape::Circle {
                    "加圆形边框要先给木牌尺寸（width_mm 或 height_mm）".into()
                } else {
                    "加边框要同时给木牌宽度和高度（width_mm、height_mm）".into()
                });
            }
        };
        let outer = (w - 2.0 * self.inset_mm, h - 2.0 * self.inset_mm);
        let rings = if self.double { 2 } else { 1 };
        let inner = (
            outer.0 - 2.0 * self.gap_mm * f64::from(rings - 1),
            outer.1 - 2.0 * self.gap_mm * f64::from(rings - 1),
        );
        let text = (inner.0 - 2.0 * margin_mm, inner.1 - 2.0 * margin_mm);
        if inner.0.min(inner.1) < MIN_SHAPE_SIZE_MM || text.0 <= 0.0 || text.1 <= 0.0 {
            return Err(
                "木牌太小，放不下这个边框和文字，请减小 inset_mm / gap_mm 或换大一点的木牌".into(),
            );
        }
        let corner = match self.shape {
            BorderShape::Rounded => {
                let r = self.corner_radius_mm.unwrap_or(0.12 * outer.0.min(outer.1));
                if !finite(r) || r < 0.0 {
                    return Err("border.corner_radius_mm 不能为负数".into());
                }
                r
            }
            _ => 0.0,
        };
        let shapes = (0..rings)
            .map(|k| {
                let shrink = 2.0 * self.gap_mm * f64::from(k);
                let (sw, sh) = (outer.0 - shrink, outer.1 - shrink);
                Shape {
                    kind: match self.shape {
                        BorderShape::Rect => ShapeKind::Rect {
                            corner_radius_mm: 0.0,
                        },
                        BorderShape::Rounded => ShapeKind::Rect {
                            corner_radius_mm: (corner - self.gap_mm * f64::from(k)).max(0.0),
                        },
                        BorderShape::Circle => ShapeKind::Circle,
                        BorderShape::Ellipse => ShapeKind::Ellipse,
                    },
                    x_mm: 0.0,
                    y_mm: 0.0,
                    width_mm: sw,
                    height_mm: sh,
                    rotation_deg: 0.0,
                    fill: false,
                }
            })
            .collect();
        let region = match self.shape {
            BorderShape::Rect => TextRegion::Rect {
                cx: 0.0,
                cy: 0.0,
                width: Some(text.0),
                height: Some(text.1),
            },
            // 圆角处让出 r(1 − 1/√2)，文字框的角不会碰到圆角
            BorderShape::Rounded => {
                let inner_r = (corner - self.gap_mm * f64::from(rings - 1)).max(0.0);
                let cut = inner_r * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
                TextRegion::Rect {
                    cx: 0.0,
                    cy: 0.0,
                    width: Some(text.0 - 2.0 * cut),
                    height: Some(text.1 - 2.0 * cut),
                }
            }
            BorderShape::Circle | BorderShape::Ellipse => TextRegion::Ellipse {
                cx: 0.0,
                cy: 0.0,
                a: text.0 / 2.0,
                b: text.1 / 2.0,
            },
        };
        Ok((shapes, region))
    }
}

/// 包围盒 (min_x, min_y, max_x, max_y)。
pub fn bounds(points: &[Point]) -> Option<(f64, f64, f64, f64)> {
    let first = *points.first()?;
    Some(points.iter().fold(
        (first.0, first.1, first.0, first.1),
        |(x0, y0, x1, y1), p| (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1)),
    ))
}

/// 线段 a→b 与矩形 (x0, y0, x1, y1) 相交（含端点落在矩形里）。
pub fn segment_hits_rect(a: Point, b: Point, rect: (f64, f64, f64, f64)) -> bool {
    let (x0, y0, x1, y1) = rect;
    // Liang–Barsky 裁剪
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let mut t0: f64 = 0.0;
    let mut t1: f64 = 1.0;
    for (p, q) in [
        (-dx, a.0 - x0),
        (dx, x1 - a.0),
        (-dy, a.1 - y0),
        (dy, y1 - a.1),
    ] {
        if p.abs() < 1e-15 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(kind: ShapeKind, w: f64, h: f64) -> Shape {
        Shape {
            kind,
            x_mm: 0.0,
            y_mm: 0.0,
            width_mm: w,
            height_mm: h,
            rotation_deg: 0.0,
            fill: false,
        }
    }

    fn closed(geometry: ShapeGeometry) -> Vec<Point> {
        match geometry {
            ShapeGeometry::Closed(points) => points,
            ShapeGeometry::Open(_) => panic!("expected closed"),
        }
    }

    #[test]
    fn circle_is_round_within_tolerance_and_counter_clockwise() {
        let ring = closed(shape(ShapeKind::Circle, 80.0, 80.0).geometry().unwrap());
        assert_eq!(ring.first(), ring.last());
        for p in &ring {
            assert!((p.0.hypot(p.1) - 40.0).abs() < 1e-9, "顶点在圆上：{p:?}");
        }
        // 相邻两点中点离圆的距离（弦高）不超过容差
        for pair in ring.windows(2) {
            let m = ((pair[0].0 + pair[1].0) / 2.0, (pair[0].1 + pair[1].1) / 2.0);
            assert!(40.0 - m.0.hypot(m.1) <= TOLERANCE_MM + 1e-9);
        }
        assert!(signed_area(&ring[..ring.len() - 1]) > 0.0);
        let area = signed_area(&ring[..ring.len() - 1]);
        assert!(
            (area - std::f64::consts::PI * 1600.0).abs() / (std::f64::consts::PI * 1600.0) < 1e-3
        );
        // 小圆点数少、大圆点数多
        let small = closed(shape(ShapeKind::Circle, 4.0, 4.0).geometry().unwrap());
        assert!(
            small.len() < ring.len(),
            "{} vs {}",
            small.len(),
            ring.len()
        );
    }

    #[test]
    fn polygon_and_star_are_regular_and_point_up() {
        let hex = closed(
            shape(ShapeKind::Polygon { sides: 6 }, 40.0, 40.0)
                .geometry()
                .unwrap(),
        );
        assert_eq!(hex.len(), 7);
        let sides: Vec<f64> = hex
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
            .collect();
        assert!(sides.iter().all(|s| (s - 20.0).abs() < 1e-9), "{sides:?}");
        assert!(
            hex.iter()
                .any(|p| (p.0).abs() < 1e-9 && (p.1 - 20.0).abs() < 1e-9),
            "顶点朝上"
        );

        let star = closed(
            shape(
                ShapeKind::Star {
                    points: 5,
                    inner_ratio: regular_star_ratio(5),
                },
                50.0,
                50.0,
            )
            .geometry()
            .unwrap(),
        );
        assert_eq!(star.len(), 11);
        let radii: Vec<f64> = star[..10].iter().map(|p| p.0.hypot(p.1)).collect();
        let outer: Vec<f64> = radii.iter().copied().filter(|r| *r > 20.0).collect();
        assert_eq!(outer.len(), 5);
        assert!(outer.iter().all(|r| (r - 25.0).abs() < 1e-9));
        // 正五角星：内角半径 ≈ 0.382 × 外角
        assert!((regular_star_ratio(5) - 0.381_966).abs() < 1e-5);
        assert!(
            star.iter()
                .any(|p| p.0.abs() < 1e-9 && (p.1 - 25.0).abs() < 1e-9)
        );
    }

    #[test]
    fn heart_fits_its_box_and_rect_corners_are_rounded() {
        let ring = closed(shape(ShapeKind::Heart, 30.0, 26.0).geometry().unwrap());
        let (x0, y0, x1, y1) = bounds(&ring).unwrap();
        assert!(
            (x1 - x0 - 30.0).abs() < 0.05 && (y1 - y0 - 26.0).abs() < 0.05,
            "{x0} {y0} {x1} {y1}"
        );
        assert!(((x0 + x1) / 2.0).abs() < 0.05 && ((y0 + y1) / 2.0).abs() < 0.05);
        // 下面的尖朝下：最低点在中线上
        let bottom = ring.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert!(bottom.0.abs() < 0.05, "{bottom:?}");

        let square = closed(
            shape(
                ShapeKind::Rect {
                    corner_radius_mm: 0.0,
                },
                20.0,
                10.0,
            )
            .geometry()
            .unwrap(),
        );
        assert_eq!(square.len(), 5);
        let rounded = closed(
            shape(
                ShapeKind::Rect {
                    corner_radius_mm: 3.0,
                },
                20.0,
                10.0,
            )
            .geometry()
            .unwrap(),
        );
        assert!(rounded.len() > 20);
        let (x0, y0, x1, y1) = bounds(&rounded).unwrap();
        assert!((x1 - x0 - 20.0).abs() < 1e-9 && (y1 - y0 - 10.0).abs() < 1e-9);
        // 角上没有点（被圆角切掉了）
        assert!(
            rounded
                .iter()
                .all(|p| (p.0.abs() - 10.0).hypot(p.1.abs() - 5.0) > 0.8)
        );
        // 圆角超过短边一半时按一半算（变成跑道形）
        let pill = closed(
            shape(
                ShapeKind::Rect {
                    corner_radius_mm: 50.0,
                },
                20.0,
                10.0,
            )
            .geometry()
            .unwrap(),
        );
        assert!(pill.iter().all(|p| p.0.is_finite()));
    }

    #[test]
    fn placement_rotation_and_validation() {
        let line = Shape {
            x_mm: 5.0,
            y_mm: -3.0,
            rotation_deg: 90.0,
            ..shape(ShapeKind::Line, 10.0, 0.0)
        };
        let ShapeGeometry::Open(points) = line.geometry().unwrap() else {
            panic!()
        };
        assert!(
            (points[0].0 - 5.0).abs() < 1e-9 && (points[0].1 + 8.0).abs() < 1e-9,
            "{points:?}"
        );
        assert!((points[1].1 - 2.0).abs() < 1e-9);

        for bad in [
            Shape {
                fill: true,
                ..shape(ShapeKind::Line, 10.0, 0.0)
            },
            shape(ShapeKind::Circle, 0.1, 0.1),
            shape(ShapeKind::Circle, f64::NAN, 1.0),
            shape(ShapeKind::Polygon { sides: 2 }, 10.0, 10.0),
            shape(
                ShapeKind::Star {
                    points: 5,
                    inner_ratio: 1.5,
                },
                10.0,
                10.0,
            ),
            shape(
                ShapeKind::Rect {
                    corner_radius_mm: -1.0,
                },
                10.0,
                10.0,
            ),
            shape(ShapeKind::Ellipse, 10.0, 5000.0),
        ] {
            assert!(bad.geometry().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn borders_follow_the_board_and_leave_a_text_region() {
        let border = Border {
            shape: BorderShape::Circle,
            double: true,
            inset_mm: 3.0,
            gap_mm: 1.5,
            corner_radius_mm: None,
        };
        let (shapes, region) = border.build(Some(90.0), Some(90.0), 2.0).unwrap();
        assert_eq!(shapes.len(), 2);
        assert_eq!(shapes[0].width_mm, 84.0);
        assert_eq!(shapes[1].width_mm, 81.0);
        assert_eq!(
            region,
            TextRegion::Ellipse {
                cx: 0.0,
                cy: 0.0,
                a: 38.5,
                b: 38.5
            }
        );
        // 椭圆区域：正方形文字框的对角线不超过直径
        assert!(region.fits(54.0, 54.0) && !region.fits(56.0, 56.0));
        assert!((region.ratio(54.0, 54.0) - 77.0 / (54.0 * std::f64::consts::SQRT_2)).abs() < 1e-9);

        let rect = Border {
            shape: BorderShape::Rect,
            double: false,
            ..border
        };
        let (shapes, region) = rect.build(Some(100.0), Some(40.0), 2.0).unwrap();
        assert_eq!(shapes.len(), 1);
        assert_eq!(
            region,
            TextRegion::Rect {
                cx: 0.0,
                cy: 0.0,
                width: Some(90.0),
                height: Some(30.0)
            }
        );
        let rounded = Border {
            shape: BorderShape::Rounded,
            ..rect
        };
        let (shapes, region) = rounded.build(Some(100.0), Some(40.0), 2.0).unwrap();
        assert!(
            matches!(shapes[0].kind, ShapeKind::Rect { corner_radius_mm } if corner_radius_mm > 0.0)
        );
        let TextRegion::Rect { width: Some(w), .. } = region else {
            panic!()
        };
        assert!(w < 90.0);

        assert!(
            rect.build(Some(100.0), None, 2.0).is_err(),
            "矩形边框要宽高都给"
        );
        assert!(border.build(None, None, 2.0).is_err());
        assert!(border.build(Some(12.0), Some(12.0), 2.0).is_err(), "太小");
    }

    #[test]
    fn segment_rect_intersection() {
        let rect = (0.0, 0.0, 10.0, 10.0);
        assert!(segment_hits_rect((-5.0, 5.0), (15.0, 5.0), rect));
        assert!(
            segment_hits_rect((2.0, 2.0), (3.0, 3.0), rect),
            "整段在里面"
        );
        assert!(!segment_hits_rect((-5.0, -5.0), (-1.0, 20.0), rect));
        assert!(!segment_hits_rect((11.0, 0.0), (20.0, 10.0), rect));
    }
}
