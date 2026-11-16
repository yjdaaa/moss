//! 预览：轮廓栅格化成图片，G-code 文件解析成可画的轨迹（圆弧拆成折线）。

use super::parse::words;
use laser_contracts::geometry::Paths;
use std::f64::consts::TAU;

/// 把像素坐标系中的线段裁剪到 `[min, max]` 矩形内（Liang–Barsky），返回可见部分的参数区间 `(t0, t1)`；
/// 完全不可见时返回 `None`。预览只栅格化可见部分，单段工作量因此以画布对角线为上限，
/// 不会因 `X1e12` 这类超大坐标把步数推到 `u32::MAX` 而卡死（2026-11-05 修订）。
fn clip_segment(
    p0: (f64, f64),
    p1: (f64, f64),
    min: (f64, f64),
    max: (f64, f64),
) -> Option<(f64, f64)> {
    let dx = p1.0 - p0.0;
    let dy = p1.1 - p0.1;
    let mut t0 = 0.0_f64;
    let mut t1 = 1.0_f64;
    for (p, q) in [
        (-dx, p0.0 - min.0),
        (dx, max.0 - p0.0),
        (-dy, p0.1 - min.1),
        (dy, max.1 - p0.1),
    ] {
        if !p.is_finite() || !q.is_finite() {
            return None;
        }
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            t0 = t0.max(r);
        } else {
            t1 = t1.min(r);
        }
        if t0 > t1 {
            return None;
        }
    }
    Some((t0, t1))
}

pub fn preview_outline(
    paths: &Paths,
    width: u32,
    height: u32,
    width_mm: f64,
    height_mm: f64,
) -> Result<image::GrayImage, &'static str> {
    if width == 0
        || height == 0
        || !width_mm.is_finite()
        || !height_mm.is_finite()
        || width_mm <= 0.0
        || height_mm <= 0.0
    {
        return Err("invalid preview dimensions");
    }
    let mut image = image::GrayImage::from_pixel(width, height, image::Luma([255]));
    let sx = f64::from(width.saturating_sub(1)) / width_mm;
    let sy = f64::from(height.saturating_sub(1)) / height_mm;
    for path in paths {
        if path.len() < 2 {
            continue;
        }
        let mut pairs: Vec<(
            laser_contracts::geometry::Point,
            laser_contracts::geometry::Point,
        )> = path.windows(2).map(|w| (w[0], w[1])).collect();
        if path.len() > 2 {
            let first = path[0];
            let last = *path.last().unwrap();
            if (first.0 - last.0).hypot(first.1 - last.1) > 0.01 {
                pairs.push((last, first));
            }
        }
        for (p0, p1) in pairs {
            let (x0, y0) = p0;
            let (x1, y1) = p1;
            if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
                return Err("path contains non-finite coordinates");
            }
            let (px0, py0) = (x0 * sx, y0 * sy);
            let (px1, py1) = (x1 * sx, y1 * sy);
            let Some((t0, t1)) = clip_segment(
                (px0, py0),
                (px1, py1),
                (0.0, 0.0),
                (f64::from(width), f64::from(height)),
            ) else {
                continue;
            };
            let (cx0, cy0) = (px0 + (px1 - px0) * t0, py0 + (py1 - py0) * t0);
            let (cx1, cy1) = (px0 + (px1 - px0) * t1, py0 + (py1 - py0) * t1);
            // 可见部分长度不超过画布对角线：每步不超过 1 像素，步数有界
            let steps = ((cx1 - cx0).hypot(cy1 - cy0).ceil() as u32).max(1);
            for step in 0..=steps {
                let t = f64::from(step) / f64::from(steps);
                let px = cx0 + (cx1 - cx0) * t;
                let py = cy0 + (cy1 - cy0) * t;
                if px >= 0.0 && py >= 0.0 && px < f64::from(width) && py < f64::from(height) {
                    image.put_pixel(px as u32, py as u32, image::Luma([0]));
                }
            }
        }
    }
    Ok(image)
}

pub fn preview_outline_rgba(
    paths: &Paths,
    width: u32,
    height: u32,
    width_mm: f64,
    height_mm: f64,
) -> Result<image::RgbaImage, &'static str> {
    preview_outline_rgba_with_options(paths, width, height, width_mm, height_mm, true)
}

/// `close_paths=true`：把每条路径视为闭合轮廓（字形），补画末点→首点；
/// `close_paths=false`：按原样绘制开放折线（解析自 G-code 的轨迹已包含所有真实线段）。
pub fn preview_outline_rgba_with_options(
    paths: &Paths,
    width: u32,
    height: u32,
    width_mm: f64,
    height_mm: f64,
    close_paths: bool,
) -> Result<image::RgbaImage, &'static str> {
    if width == 0
        || height == 0
        || !width_mm.is_finite()
        || !height_mm.is_finite()
        || width_mm <= 0.0
        || height_mm <= 0.0
    {
        return Err("invalid preview dimensions");
    }

    let mut image = image::RgbaImage::from_pixel(width, height, image::Rgba([0, 0, 0, 0]));

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for (x, y) in paths.iter().flatten() {
        if x.is_finite() && y.is_finite() {
            min_x = min_x.min(*x);
            min_y = min_y.min(*y);
            max_x = max_x.max(*x);
            max_y = max_y.max(*y);
        }
    }

    if !min_x.is_finite() {
        min_x = 0.0;
        max_x = width_mm;
        min_y = 0.0;
        max_y = height_mm;
    }

    let content_w = (max_x - min_x).max(0.1);
    let content_h = (max_y - min_y).max(0.1);

    // 预留 15% 边距，居中且严格锁定几何长宽比，杜绝字符变形与切边
    let margin_x = width as f64 * 0.15;
    let margin_y = height as f64 * 0.15;
    let draw_w = (width as f64) - margin_x * 2.0;
    let draw_h = (height as f64) - margin_y * 2.0;

    let scale = (draw_w / content_w).min(draw_h / content_h);

    let offset_x = margin_x + (draw_w - content_w * scale) * 0.5 - min_x * scale;
    let offset_y = margin_y + (draw_h - content_h * scale) * 0.5 - min_y * scale;

    let w_i32 = width as i32;
    let h_i32 = height as i32;

    for path in paths {
        if path.len() < 2 {
            continue;
        }
        let mut pairs: Vec<(
            laser_contracts::geometry::Point,
            laser_contracts::geometry::Point,
        )> = path.windows(2).map(|w| (w[0], w[1])).collect();
        if close_paths && path.len() > 2 {
            let first = path[0];
            let last = *path.last().unwrap();
            if (first.0 - last.0).hypot(first.1 - last.1) > 0.01 {
                pairs.push((last, first));
            }
        }
        for (p0, p1) in pairs {
            let (x0, y0) = p0;
            let (x1, y1) = p1;
            if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
                return Err("path contains non-finite coordinates");
            }
            let px0 = x0 * scale + offset_x;
            let py0 = y0 * scale + offset_y;
            let px1 = x1 * scale + offset_x;
            let py1 = y1 * scale + offset_y;

            // 自动缩放通常已把轨迹装进画布；仍按画布（含 2px 光晕）裁剪，保证单段步数有界
            let Some((t0, t1)) = clip_segment(
                (px0, py0),
                (px1, py1),
                (-3.0, -3.0),
                (f64::from(width) + 3.0, f64::from(height) + 3.0),
            ) else {
                continue;
            };
            let (sx0, sy0) = (px0 + (px1 - px0) * t0, py0 + (py1 - py0) * t0);
            let (sx1, sy1) = (px0 + (px1 - px0) * t1, py0 + (py1 - py0) * t1);
            let dist_px = (sx1 - sx0).hypot(sy1 - sy0);
            let steps = (dist_px * 2.0).ceil() as u32;
            let steps = steps.max(1);

            for step in 0..=steps {
                let t = f64::from(step) / f64::from(steps);
                let cx = (sx0 + (sx1 - sx0) * t).round() as i32;
                let cy = (sy0 + (sy1 - sy0) * t).round() as i32;

                // 绘制高对比度多层激光等离子发光线条 (3px 线宽 + 高光白芯)
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        let nx = cx + dx;
                        let ny = cy + dy;
                        if nx >= 0 && ny >= 0 && nx < w_i32 && ny < h_i32 {
                            let d2 = dx * dx + dy * dy;
                            let target_pixel = image.get_pixel_mut(nx as u32, ny as u32);
                            if d2 == 0 {
                                // 白炽线芯
                                *target_pixel = image::Rgba([240, 252, 255, 255]);
                            } else if d2 <= 2 {
                                // 激光青高能主光束
                                if target_pixel.0[3] < 220 {
                                    *target_pixel = image::Rgba([0, 229, 255, 220]);
                                }
                            } else if d2 <= 4 {
                                // 柔和外围等离子光晕
                                if target_pixel.0[3] < 90 {
                                    *target_pixel = image::Rgba([0, 229, 255, 90]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(image)
}

/// 预览把圆弧拆成折线时的弦高容差（mm）：弧线看起来是光滑的，又不会为大圆生成过多点。
pub const PREVIEW_ARC_TOLERANCE_MM: f64 = 0.02;

/// 单段圆弧最多拆成多少段（整圆也够画圆滑；防止超大半径的弧拖慢预览）。
pub const MAX_PREVIEW_ARC_SEGMENTS: usize = 360;

/// 整份文件里圆弧拆出的点数上限；超过后余下的圆弧只画首尾连线，预览工作量有界。
pub const MAX_PREVIEW_ARC_POINTS: usize = 400_000;

/// G-code 文件预览用的轨迹（2026-11-16，技术书 r41）：`paths` 为每段连续非快速移动（从上一个位置开始的折线），
/// G2/G3 圆弧已拆成折线；`bounds` 为所有移动终点与圆弧拆分点的范围 `(min_x, min_y, max_x, max_y)`，没有移动时为 `None`。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PreviewToolpath {
    pub paths: Vec<Vec<laser_contracts::geometry::Point>>,
    pub bounds: Option<(f64, f64, f64, f64)>,
    /// 因超出 [`MAX_PREVIEW_ARC_POINTS`] 或参数无效而只画了首尾连线的圆弧个数。
    pub arcs_simplified: usize,
}

/// 圆弧拆成折线（不含起点、含终点）。`ccw=true` 为 G3；`r` 为 `R` 写法的半径（负值取大于 180° 的弧），
/// 否则 `ij` 为圆心相对起点的偏移。半径无效或拆分预算用完时返回 `None`，由调用方画首尾连线。
fn tessellate_arc(
    start: (f64, f64),
    end: (f64, f64),
    ccw: bool,
    r: Option<f64>,
    ij: (f64, f64),
    budget: &mut usize,
) -> Option<Vec<(f64, f64)>> {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let same_point = dx.abs() < 1e-9 && dy.abs() < 1e-9;
    let center = match r {
        Some(r) => {
            // R 写法与 GRBL 一致：R>0 取小于 180° 的弧，R<0 取大于 180° 的弧；起终点重合无法定圆
            let chord = dx.hypot(dy);
            if same_point || !r.is_finite() || r == 0.0 || chord > 2.0 * r.abs() + 1e-6 {
                return None;
            }
            let h = (4.0 * r * r - chord * chord).max(0.0).sqrt() / chord;
            let mut h = -h;
            if ccw {
                h = -h;
            }
            if r < 0.0 {
                h = -h;
            }
            (start.0 + 0.5 * (dx - dy * h), start.1 + 0.5 * (dy + dx * h))
        }
        None => (start.0 + ij.0, start.1 + ij.1),
    };
    let radius = (start.0 - center.0).hypot(start.1 - center.1);
    if !radius.is_finite() || radius < 1e-9 || radius > 1e7 {
        return None;
    }
    let a0 = (start.1 - center.1).atan2(start.0 - center.0);
    let a1 = (end.1 - center.1).atan2(end.0 - center.0);
    let sweep = if same_point {
        TAU
    } else if ccw {
        (a1 - a0).rem_euclid(TAU)
    } else {
        (a0 - a1).rem_euclid(TAU)
    };
    // 每段转角不超过 10°，且弦高不超过容差
    let max_step = if radius > PREVIEW_ARC_TOLERANCE_MM {
        (2.0 * (1.0 - PREVIEW_ARC_TOLERANCE_MM / radius).acos()).min(10f64.to_radians())
    } else {
        10f64.to_radians()
    };
    let segments = ((sweep / max_step).ceil() as usize).clamp(1, MAX_PREVIEW_ARC_SEGMENTS);
    if segments > *budget {
        return None;
    }
    *budget -= segments;
    let direction = if ccw { 1.0 } else { -1.0 };
    let mut points = Vec::with_capacity(segments);
    for step in 1..segments {
        let angle = a0 + direction * sweep * step as f64 / segments as f64;
        points.push((
            center.0 + radius * angle.cos(),
            center.1 + radius * angle.sin(),
        ));
    }
    points.push(end);
    Some(points)
}

/// 解析 G-code 文件得到预览轨迹（上传的 G-code 与两点对位生成的新刀路都用它出预览图）：
/// 跟踪模态 G0/G1/G2/G3、G90/G91、G20/G21、`G92`（只改原点不移动），G2/G3 在 XY 平面（G17）按
/// [`PREVIEW_ARC_TOLERANCE_MM`] 拆成折线，`I/J`、`R`（含负值）、顺 / 逆时针与整圆都支持；G18/G19 平面的圆弧、
/// 无效半径或超出点数预算时只画首尾连线。只用于画图，不做安全判断（范围复核仍用 [`motion_extents`]）。
pub fn preview_toolpath(text: &str) -> PreviewToolpath {
    let (mut px, mut py) = (0.0_f64, 0.0_f64);
    let (mut ox, mut oy) = (0.0_f64, 0.0_f64);
    let mut absolute = true;
    let mut unit = 1.0;
    let mut motion = 0;
    let mut xy_plane = true;
    let mut budget = MAX_PREVIEW_ARC_POINTS;
    let mut out = PreviewToolpath::default();
    let mut current: Vec<(f64, f64)> = Vec::new();
    let mut bounds = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    let mut include = |x: f64, y: f64| {
        bounds.0 = bounds.0.min(x);
        bounds.1 = bounds.1.min(y);
        bounds.2 = bounds.2.max(x);
        bounds.3 = bounds.3.max(y);
    };
    for line in text.lines() {
        let ws = words(line);
        if ws.is_empty() {
            continue;
        }
        let has = |code: f64| ws.iter().any(|&(l, v)| l == 'G' && (v - code).abs() < 1e-9);
        let word = |letter: char| ws.iter().rev().find(|(l, _)| *l == letter).map(|(_, v)| *v);
        if has(20.0) {
            unit = 25.4;
        }
        if has(21.0) {
            unit = 1.0;
        }
        if has(90.0) {
            absolute = true;
        }
        if has(91.0) {
            absolute = false;
        }
        if has(17.0) {
            xy_plane = true;
        }
        if has(18.0) || has(19.0) {
            xy_plane = false;
        }
        let (x, y) = (word('X'), word('Y'));
        if has(92.0) {
            if let Some(v) = x {
                ox = px - v * unit;
            }
            if let Some(v) = y {
                oy = py - v * unit;
            }
            if x.is_none() && y.is_none() {
                ox = px;
                oy = py;
            }
            continue;
        }
        for &(letter, value) in &ws {
            if letter == 'G' {
                for code in [0, 1, 2, 3] {
                    if (value - f64::from(code)).abs() < 1e-9 {
                        motion = code;
                    }
                }
            }
        }
        // 机床坐标 / 回参考点之类的行无法知道终点：预览里跳过（发送前的范围复核会拒绝这类文件）
        if [53.0, 28.0, 30.0, 10.0].iter().any(|code| has(*code)) {
            continue;
        }
        if x.is_none() && y.is_none() {
            continue;
        }
        let target = |current: f64, origin: f64, value: Option<f64>| match value {
            None => current,
            Some(v) if absolute => origin + v * unit,
            Some(v) => current + v * unit,
        };
        let (tx, ty) = (target(px, ox, x), target(py, oy, y));
        if !(tx.is_finite() && ty.is_finite()) {
            continue;
        }
        if motion == 0 {
            if current.len() > 1 {
                out.paths.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            current.push((tx, ty));
            include(tx, ty);
        } else {
            if current.is_empty() {
                current.push((px, py));
                include(px, py);
            }
            let arc = if (motion == 2 || motion == 3) && xy_plane {
                let r = word('R').map(|r| r * unit);
                let ij = (
                    word('I').unwrap_or(0.0) * unit,
                    word('J').unwrap_or(0.0) * unit,
                );
                let points = tessellate_arc((px, py), (tx, ty), motion == 3, r, ij, &mut budget);
                if points.is_none() {
                    out.arcs_simplified += 1;
                }
                points
            } else {
                if motion == 2 || motion == 3 {
                    out.arcs_simplified += 1;
                }
                None
            };
            match arc {
                Some(points) => {
                    for &(x, y) in &points {
                        include(x, y);
                    }
                    current.extend(points);
                }
                None => {
                    include(tx, ty);
                    current.push((tx, ty));
                }
            }
        }
        px = tx;
        py = ty;
    }
    if current.len() > 1 {
        out.paths.push(current);
    }
    out.bounds = bounds.0.is_finite().then_some(bounds);
    out
}
