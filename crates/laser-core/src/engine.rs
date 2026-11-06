//! 文字雕刻任务（2026-11-06 修订：填充方式、多行排版、按木牌尺寸自动适配、激光头起点约定）。
//!
//! - 刻法 [`TextFill`]：`Outline` 只描字的轮廓（默认，用户决定）；`Fill` 把字形转成黑白位图逐行扫描填满
//!   （小字、中文在木头上最清楚）；`Hatch` 用矢量平行线填满并描边（大字更快）。填充按非零环绕规则计算，
//!   “口”“O”这类字的内孔不会被填上。
//! - 排版：文字里的换行即分行，按 [`TextAlignment`] 对齐。给了木牌尺寸（宽 / 高，可只给一边）时，
//!   未指定字号就自动取能放下的最大字号（四周各留 `margin_mm`）；指定了字号但放不下直接报错，不悄悄缩小。
//! - 起点 [`TextOrigin`]：任务以开始时激光头所在位置为参照。`Center`（默认，用户决定）= 激光头对准
//!   木牌 / 图案中心；`LowerLeft` = 对准木牌左下角（没给木牌时为文字左下角）。实现上只改开头的
//!   `G92`（声明激光头在图案坐标中的位置），刀路本身不变；结束时激光头关光回到起点。

use crate::{font, gcode, path_opt, raster};
use image::{GrayImage, Luma, RgbaImage};
use laser_contracts::geometry::{Paths, PathOptimizationStats};

pub use font::TextAlignment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFill {
    Outline,
    Fill,
    Hatch,
}

impl TextFill {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None | Some("outline") => Ok(Self::Outline),
            Some("fill") => Ok(Self::Fill),
            Some("hatch") => Ok(Self::Hatch),
            Some(other) => Err(format!("text_fill 只能是 outline / fill / hatch，收到 {other}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Fill => "fill",
            Self::Hatch => "hatch",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOrigin {
    Center,
    LowerLeft,
}

impl TextOrigin {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None | Some("center") => Ok(Self::Center),
            Some("lower_left") => Ok(Self::LowerLeft),
            Some(other) => Err(format!("origin 只能是 center / lower_left，收到 {other}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::LowerLeft => "lower_left",
        }
    }
}

pub fn parse_alignment(value: Option<&str>) -> Result<TextAlignment, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None | Some("center") => Ok(TextAlignment::Center),
        Some("left") => Ok(TextAlignment::Left),
        Some("right") => Ok(TextAlignment::Right),
        Some(other) => Err(format!("text_align 只能是 left / center / right，收到 {other}")),
    }
}

#[derive(Debug, Clone)]
pub struct TextTaskSpec<'a> {
    pub text: &'a str,
    pub font_path: &'a str,
    /// 字高（mm）；`auto_size` 时只作起始值
    pub font_size_mm: f64,
    pub letter_spacing_mm: f64,
    pub power: i32,
    pub feed_rate: i32,
    pub preview_width_px: u32,
    pub preview_height_px: u32,
    pub fill: TextFill,
    pub alignment: TextAlignment,
    pub origin: TextOrigin,
    pub board_width_mm: Option<f64>,
    pub board_height_mm: Option<f64>,
    /// 木牌四周留边（mm）
    pub margin_mm: f64,
    /// 有木牌尺寸且未指定字号：自动取能放下的最大字号
    pub auto_size: bool,
    /// 扫描填充的像素间距 / 排线间距（mm）
    pub pixel_size_mm: f64,
    /// 扫描填充每侧过扫描（mm）
    pub overscan_mm: f64,
}

/// 以开始时激光头位置为原点的范围（mm），走边框用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadRelativeBox {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

#[derive(Debug, Clone)]
pub struct TextTaskArtifacts {
    pub gcode: String,
    pub preview: RgbaImage,
    /// 文字笔画实际范围（不含过扫描）
    pub width_mm: f64,
    pub height_mm: f64,
    pub glyphs: usize,
    pub path_optimization: PathOptimizationStats,
    pub estimate: gcode::TimeEstimate,
    /// 实际字高（自动适配后）
    pub font_size_mm: f64,
    pub fill: TextFill,
    pub origin: TextOrigin,
    /// 文字笔画相对激光头起点的范围
    pub frame: HeadRelativeBox,
}

pub struct TextTaskEngine;

const LINE_SPACING_RATIO: f64 = 0.2;
const MIN_FONT_SIZE_MM: f64 = 1.0;

fn ink_bounds(paths: &Paths) -> Option<(f64, f64, f64, f64)> {
    let mut points = paths.iter().flatten();
    let first = points.next()?;
    Some(points.fold((first.0, first.1, first.0, first.1), |(x0, y0, x1, y1), p| {
        (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1))
    }))
}

/// 排版并把笔画平移到以墨迹左上角为 (0,0)（y 向下）；返回 (笔画, 宽, 高, 字数)。
fn layout(data: &[u8], spec: &TextTaskSpec<'_>, size: f64) -> Result<(Paths, f64, f64, usize), String> {
    let layout = font::layout_text_advanced(
        data,
        0,
        spec.text,
        &font::TextLayoutOptions {
            height_mm: size,
            spacing_mm: spec.letter_spacing_mm,
            line_spacing_ratio: LINE_SPACING_RATIO,
            max_width_mm: None,
            alignment: spec.alignment,
        },
    )
    .map_err(|e| match e {
        "glyph is missing" => "所选字体缺少其中某个字，请换一种字体".to_owned(),
        other => other.to_owned(),
    })?;
    let (x0, y0, x1, y1) = ink_bounds(&layout.paths).ok_or("文字没有可雕刻的笔画")?;
    let mut paths = layout.paths;
    for point in paths.iter_mut().flatten() {
        point.0 -= x0;
        point.1 -= y0;
    }
    Ok((paths, x1 - x0, y1 - y0, layout.glyphs))
}

/// 一条水平扫描线（y 向下坐标）与全部闭合笔画的交点，按非零环绕规则返回需要填充的区间。
fn scanline_spans(paths: &Paths, y: f64) -> Vec<(f64, f64)> {
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for path in paths {
        if path.len() < 3 {
            continue;
        }
        for i in 0..path.len() {
            let (a, b) = (path[i], path[(i + 1) % path.len()]);
            if (a.1 <= y && b.1 > y) || (b.1 <= y && a.1 > y) {
                let t = (y - a.1) / (b.1 - a.1);
                crossings.push((a.0 + t * (b.0 - a.0), if b.1 > a.1 { 1 } else { -1 }));
            }
        }
    }
    crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut spans = Vec::new();
    let mut winding = 0;
    let mut start = 0.0;
    for (x, dir) in crossings {
        let was_inside = winding != 0;
        winding += dir;
        if !was_inside && winding != 0 {
            start = x;
        } else if was_inside && winding == 0 && x > start {
            spans.push((start, x));
        }
    }
    spans
}

/// 字形转黑白位图（0 = 出光），行 0 在上。
fn rasterize(paths: &Paths, width: f64, height: f64, pixel: f64) -> Vec<Vec<u8>> {
    let cols = ((width / pixel).ceil() as usize).max(1);
    let rows = ((height / pixel).ceil() as usize).max(1);
    (0..rows)
        .map(|row| {
            let mut line = vec![255_u8; cols];
            for (a, b) in scanline_spans(paths, (row as f64 + 0.5) * pixel) {
                let from = (a / pixel - 0.5).ceil().max(0.0) as usize;
                let to = ((b / pixel - 0.5).floor() as isize).min(cols as isize - 1);
                for col in from as isize..=to {
                    line[col as usize] = 0;
                }
            }
            line
        })
        .collect()
}

/// 矢量排线：每隔 `spacing` 一条水平线段，逐行换向，减少空走。
fn hatch_lines(paths: &Paths, height: f64, spacing: f64) -> Paths {
    let mut out = Vec::new();
    let mut y = spacing / 2.0;
    let mut forward = true;
    while y < height {
        let mut spans = scanline_spans(paths, y);
        if !forward {
            spans.reverse();
        }
        for (a, b) in spans {
            out.push(if forward { vec![(a, y), (b, y)] } else { vec![(b, y), (a, y)] });
        }
        forward = !forward;
        y += spacing;
    }
    out
}

/// 把开头的 `G92 X0 Y0 Z0` 改成激光头在图案坐标中的位置，并在结束前关光回到起点。
fn place_at_head(gcode: &str, start_x: f64, start_y: f64) -> Result<String, String> {
    const ORIGIN: &str = "G92 X0 Y0 Z0";
    // 避免把 0 写成 "-0.000"
    let clean = |v: f64| if v.abs() < 5e-4 { 0.0 } else { v };
    let (start_x, start_y) = (clean(start_x), clean(start_y));
    if !gcode.contains(ORIGIN) {
        return Err("生成的 G-code 缺少原点设定".into());
    }
    let placed = gcode.replacen(ORIGIN, &format!("G92 X{start_x:.3} Y{start_y:.3} Z0"), 1);
    let tail = "M5\nM2\n";
    let body = placed.strip_suffix(tail).ok_or("生成的 G-code 结尾格式异常")?;
    Ok(format!("{body}M5\nG0 X{start_x:.3} Y{start_y:.3}\nM2\n"))
}

impl TextTaskEngine {
    pub fn generate(spec: &TextTaskSpec<'_>) -> Result<TextTaskArtifacts, String> {
        if !(spec.pixel_size_mm.is_finite() && (0.05..=1.0).contains(&spec.pixel_size_mm)) {
            return Err("pixel_size_mm 必须在 0.05 ~ 1 mm 之间".into());
        }
        if !(spec.margin_mm.is_finite() && spec.margin_mm >= 0.0) {
            return Err("留边必须是不小于 0 的数字".into());
        }
        let data = std::fs::read(spec.font_path).map_err(|_| "font file cannot be read".to_owned())?;
        let positive = |v: Option<f64>, name: &str| -> Result<Option<f64>, String> {
            match v {
                None => Ok(None),
                Some(v) if v.is_finite() && v > 0.0 => Ok(Some(v)),
                Some(_) => Err(format!("{name} 必须是大于 0 的数字")),
            }
        };
        let board_w = positive(spec.board_width_mm, "木牌宽度")?;
        let board_h = positive(spec.board_height_mm, "木牌高度")?;
        let avail = |board: Option<f64>| board.map(|b| b - 2.0 * spec.margin_mm);
        let (avail_w, avail_h) = (avail(board_w), avail(board_h));
        if avail_w.is_some_and(|v| v <= 0.0) || avail_h.is_some_and(|v| v <= 0.0) {
            return Err("木牌尺寸小于两侧留边，放不下任何文字".into());
        }

        let mut size = spec.font_size_mm;
        let (mut paths, mut width, mut height, mut glyphs) = layout(&data, spec, size)?;
        let fits = |w: f64, h: f64| avail_w.is_none_or(|a| w <= a + 1e-6) && avail_h.is_none_or(|a| h <= a + 1e-6);
        if board_w.is_some() || board_h.is_some() {
            if spec.auto_size {
                // 字号与笔画尺寸近似成正比，字距是固定 mm，迭代几次收敛到能放下的最大字号
                for _ in 0..4 {
                    let ratio_w = avail_w.map_or(f64::INFINITY, |a| a / width);
                    let ratio_h = avail_h.map_or(f64::INFINITY, |a| a / height);
                    let ratio = ratio_w.min(ratio_h);
                    if fits(width, height) && ratio < 1.001 {
                        break;
                    }
                    size *= ratio * 0.999;
                    if size < MIN_FONT_SIZE_MM {
                        return Err(format!("文字太长，放进这块木牌后字高不到 {MIN_FONT_SIZE_MM} mm，请减少字数或分行"));
                    }
                    (paths, width, height, glyphs) = layout(&data, spec, size)?;
                }
            }
            if !fits(width, height) {
                return Err(format!(
                    "按 {size:.1} mm 字号文字约 {width:.1} × {height:.1} mm，放不进 {} × {} mm 的木牌（四周各留 {:.1} mm）；请减小字号，或不指定字号让 MOSS 自动适配",
                    board_w.map_or("—".to_owned(), |v| format!("{v:.0}")),
                    board_h.map_or("—".to_owned(), |v| format!("{v:.0}")),
                    spec.margin_mm
                ));
            }
        }

        // 激光头起点在图案坐标（y 向上，笔画范围 0..width × 0..height）中的位置
        let (start_x, start_y) = match spec.origin {
            TextOrigin::Center => (width / 2.0, height / 2.0),
            TextOrigin::LowerLeft => (
                -board_w.map_or(0.0, |b| (b - width) / 2.0),
                -board_h.map_or(0.0, |b| (b - height) / 2.0),
            ),
        };
        let frame = HeadRelativeBox { min_x: -start_x, max_x: width - start_x, min_y: -start_y, max_y: height - start_y };

        let optimized = path_opt::optimize_with_threshold(&paths, width.max(height).max(1.0)).map_err(|e| e.to_string())?;
        let stats = optimized.stats;
        let (gcode, preview) = match spec.fill {
            TextFill::Outline | TextFill::Hatch => {
                let mut all = optimized.paths;
                if spec.fill == TextFill::Hatch {
                    all.extend(hatch_lines(&paths, height, spec.pixel_size_mm));
                }
                let options = gcode::WriterOptions {
                    width_mm: width.max(1e-3),
                    height_mm: height.max(1e-3),
                    power: spec.power,
                    feed_rate: spec.feed_rate,
                    auto_close: true,
                    laser_mode: "M4",
                    power_scale: None,
                };
                let written = gcode::write_outline(&all, &options)?;
                let preview = gcode::preview_outline_rgba(&all, spec.preview_width_px, spec.preview_height_px, options.width_mm, options.height_mm)?;
                (place_at_head(&written, start_x, start_y)?, preview)
            }
            TextFill::Fill => {
                let bitmap = rasterize(&paths, width, height, spec.pixel_size_mm);
                let rows = bitmap.len();
                let cols = bitmap[0].len();
                let plan = raster::raster_plan_with_direction(&bitmap, "horizontal", "scanline").map_err(|e| e.to_string())?;
                let written = gcode::write_raster(
                    &plan,
                    cols,
                    rows,
                    &gcode::RasterWriterOptions {
                        pixel_size_mm: spec.pixel_size_mm,
                        overscan_mm: spec.overscan_mm,
                        work_area_width_mm: cols as f64 * spec.pixel_size_mm + 2.0 * spec.overscan_mm + 1.0,
                        work_area_height_mm: rows as f64 * spec.pixel_size_mm + 1.0,
                        power: spec.power,
                        feed_rate: spec.feed_rate,
                        laser_mode: "M4",
                        snake_scan: true,
                        material: None,
                        safety_note: None,
                        power_scale: None,
                    },
                )
                .map_err(|e| e.to_string())?;
                let gray = GrayImage::from_fn(cols as u32, rows as u32, |x, y| Luma([bitmap[y as usize][x as usize]]));
                // 栅格写出器把图案右移一个过扫描距离
                (place_at_head(&written, start_x + spec.overscan_mm, start_y)?, image::DynamicImage::ImageLuma8(gray).to_rgba8())
            }
        };
        let estimate = gcode::estimate_time(&gcode, 3000.0);
        Ok(TextTaskArtifacts {
            gcode,
            preview,
            width_mm: width,
            height_mm: height,
            glyphs,
            path_optimization: stats,
            estimate,
            font_size_mm: size,
            fill: spec.fill,
            origin: spec.origin,
            frame,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/fonts/geometry.ttf");

    fn spec(text: &str) -> TextTaskSpec<'_> {
        TextTaskSpec {
            text,
            font_path: FONT,
            font_size_mm: 10.0,
            letter_spacing_mm: 0.5,
            power: 420,
            feed_rate: 1800,
            preview_width_px: 200,
            preview_height_px: 100,
            fill: TextFill::Outline,
            alignment: TextAlignment::Center,
            origin: TextOrigin::Center,
            board_width_mm: None,
            board_height_mm: None,
            margin_mm: 2.0,
            auto_size: false,
            pixel_size_mm: 0.1,
            overscan_mm: 1.0,
        }
    }

    fn extents(gcode: &str) -> gcode::MotionExtents {
        gcode::motion_extents(gcode).unwrap().unwrap()
    }

    #[test]
    fn center_origin_puts_the_head_in_the_middle_and_returns_there() {
        let job = TextTaskEngine::generate(&spec("AB")).unwrap();
        let e = extents(&job.gcode);
        // 激光头起点（0,0）位于笔画范围正中
        assert!((e.min_x + e.max_x).abs() < 0.01 && (e.min_y + e.max_y).abs() < 0.01, "{e:?}");
        assert!((job.frame.min_x + job.width_mm / 2.0).abs() < 1e-9);
        assert!(job.gcode.trim_end().ends_with(&format!("G0 X{:.3} Y{:.3}\nM2", job.width_mm / 2.0, job.height_mm / 2.0)), "{}", job.gcode);

        let lower_left = TextTaskEngine::generate(&TextTaskSpec { origin: TextOrigin::LowerLeft, ..spec("AB") }).unwrap();
        let e = extents(&lower_left.gcode);
        assert!(e.min_x.abs() < 0.01 && e.min_y.abs() < 0.01, "{e:?}");
        assert!(lower_left.gcode.contains("G92 X0.000 Y0.000 Z0"));
    }

    #[test]
    fn auto_size_fits_the_board_with_margins_and_explicit_size_must_fit() {
        let fitted = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(80.0),
            board_height_mm: Some(30.0),
            auto_size: true,
            ..spec("ABAB")
        })
        .unwrap();
        assert!(fitted.width_mm <= 76.0 + 1e-6 && fitted.height_mm <= 26.0 + 1e-6, "{} × {}", fitted.width_mm, fitted.height_mm);
        assert!(fitted.width_mm > 70.0 || fitted.height_mm > 24.0, "应接近能放下的最大字号：{} × {}", fitted.width_mm, fitted.height_mm);
        assert!(fitted.font_size_mm > 10.0);

        // 木牌左下角为起点：笔画在木牌中居中
        let corner = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(80.0),
            board_height_mm: Some(30.0),
            auto_size: true,
            origin: TextOrigin::LowerLeft,
            ..spec("ABAB")
        })
        .unwrap();
        // 激光头在木牌左下角（范围起点 0），笔画右侧到木牌右边的留白等于左侧留白
        let e = extents(&corner.gcode);
        let left = (80.0 - corner.width_mm) / 2.0;
        assert!(corner.gcode.contains(&format!("G92 X{:.3}", -left)), "{}", &corner.gcode[..300]);
        assert!(((80.0 - e.max_x) - left).abs() < 0.05, "左右留白应相等：{e:?}");

        let err = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(30.0),
            font_size_mm: 20.0,
            ..spec("ABAB")
        })
        .unwrap_err();
        assert!(err.contains("放不进"), "{err}");
        assert!(TextTaskEngine::generate(&TextTaskSpec { board_width_mm: Some(3.0), auto_size: true, ..spec("A") }).is_err());
    }

    #[test]
    fn multiline_text_is_aligned_and_stacked() {
        let one = TextTaskEngine::generate(&spec("AB")).unwrap();
        let two = TextTaskEngine::generate(&spec("AB\nA")).unwrap();
        assert!(two.height_mm > one.height_mm * 1.8);
        assert_eq!(two.glyphs, 3);
    }

    #[test]
    fn fill_scans_inside_the_strokes_and_keeps_holes_empty() {
        // 一个带方孔的方块（外圈逆时针、内圈顺时针，同 TrueType 约定）
        let square = vec![
            vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
            vec![(3.0, 3.0), (3.0, 7.0), (7.0, 7.0), (7.0, 3.0)],
        ];
        let spans = scanline_spans(&square, 5.0);
        assert_eq!(spans, vec![(0.0, 3.0), (7.0, 10.0)]);
        let bitmap = rasterize(&square, 10.0, 10.0, 1.0);
        assert_eq!(bitmap[5], vec![0, 0, 0, 255, 255, 255, 255, 0, 0, 0]);
        assert_eq!(bitmap[0], vec![0; 10]);
        // 两个重叠笔画按非零规则合并，不会在重叠处留白
        let overlap = vec![
            vec![(0.0, 0.0), (6.0, 0.0), (6.0, 2.0), (0.0, 2.0)],
            vec![(4.0, 0.0), (10.0, 0.0), (10.0, 2.0), (4.0, 2.0)],
        ];
        assert_eq!(scanline_spans(&overlap, 1.0), vec![(0.0, 10.0)]);

        let filled = TextTaskEngine::generate(&TextTaskSpec { fill: TextFill::Fill, ..spec("AB") }).unwrap();
        let burns = filled.gcode.lines().filter(|l| l.contains(" S420")).count();
        let outline = TextTaskEngine::generate(&spec("AB")).unwrap();
        assert!(burns > 20, "扫描填充应有大量出光段：{burns}");
        // 过扫描不改变激光头相对图案中心的位置
        let e = extents(&filled.gcode);
        assert!(((e.min_x + e.max_x) / 2.0).abs() < 0.2, "{e:?}");
        assert!(filled.width_mm == outline.width_mm);
    }

    #[test]
    fn hatch_adds_parallel_lines_to_the_outline() {
        let outline = TextTaskEngine::generate(&spec("A")).unwrap();
        let hatch = TextTaskEngine::generate(&TextTaskSpec { fill: TextFill::Hatch, pixel_size_mm: 0.2, ..spec("A") }).unwrap();
        assert!(hatch.gcode.lines().count() > outline.gcode.lines().count() * 2);
        assert!(TextFill::parse(Some("solid")).is_err());
        assert!(TextOrigin::parse(Some("top")).is_err());
        assert!(parse_alignment(Some("justify")).is_err());
        assert_eq!(TextFill::parse(None).unwrap(), TextFill::Outline);
        assert_eq!(TextOrigin::parse(None).unwrap(), TextOrigin::Center);
    }
}
