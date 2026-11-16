//! 文字雕刻任务（2026-11-06 修订：填充方式、多行排版、按木牌尺寸自动适配、激光头起点约定）。
//!
//! - 刻法 [`TextFill`]：`Outline` 只描字的轮廓（默认，用户决定）；`Fill` 用矢量密排扫描线填满再描边
//!   （2026-11-16 起不再转位图；扫描线距取材料像素间距）；`Hatch` 线距放宽到 3 倍（线条可见、更快）。
//!   填充按非零环绕规则计算，“口”“O”这类字的内孔不会被填上；扫描设置（过扫描、分组、角度、交叉、
//!   双向、偏移）与走刀顺序见 [`crate::vector_plan`]。
//! - 排版：文字里的换行即分行，按 [`TextAlignment`] 对齐。给了木牌尺寸（宽 / 高，可只给一边）时，
//!   未指定字号就自动取能放下的最大字号（四周各留 `margin_mm`）；指定了字号但放不下直接报错，不悄悄缩小。
//! - 起点 [`TextOrigin`]：任务以开始时激光头所在位置为参照。`Center`（默认，用户决定）= 激光头对准
//!   木牌 / 图案中心；`LowerLeft` = 对准木牌左下角（没给木牌时为文字左下角）。实现上只改开头的
//!   `G92`（声明激光头在图案坐标中的位置），刀路本身不变；结束时激光头关光回到起点。
//! - 切割质量 [`TextTaskSpec::cut`]（2026-11-16，对齐 LightBurn）：切缝补偿在规划前改轮廓，桥位、
//!   引入 / 引出线、过切在规划（及其出光长度自检）之后改写闭合轮廓，见 [`crate::cut`]；默认全关。

use crate::{cut, font, gcode, vector_plan};
use image::RgbaImage;
use laser_contracts::geometry::{PathOptimizationStats, Paths};

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
            Some(other) => Err(format!(
                "text_fill 只能是 outline / fill / hatch，收到 {other}"
            )),
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
        Some(other) => Err(format!(
            "text_align 只能是 left / center / right，收到 {other}"
        )),
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
    /// 实心填充的线距（mm，取材料的像素间距）；排线填充为它的 [`HATCH_SPACING_FACTOR`] 倍
    pub pixel_size_mm: f64,
    /// 填充的扫描设置（分组、角度按机器 / 预览视角逆时针、交叉排线、双向、过扫描、扫描偏移）；
    /// 其中 `spacing` 与 `rule` 不用，由刻法决定（线距见上，字体一律非零环绕）
    pub scan: vector_plan::FillOptions,
    /// 走刀顺序选项；切割时 `inside_first` 须为 true（外框先切断，里面的孔会移位）
    pub plan: vector_plan::PlanOptions,
    /// 切割质量设置（切缝补偿、桥位、引线、过切），默认全关；只在切割时由调用方填写
    pub cut: cut::CutOptions,
}

/// 排线填充的线距是实心填充的几倍（线条可见、更快）。
pub const HATCH_SPACING_FACTOR: f64 = 3.0;

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
    /// 文字笔画相对激光头起点的范围（不含过扫描）
    pub frame: HeadRelativeBox,
    /// 实际使用的填充设置（只描轮廓时为 `None`）
    pub fill_options: Option<vector_plan::FillOptions>,
    /// 去掉的重复轮廓条数
    pub removed_duplicates: usize,
    /// 切割质量设置生效时的统计（全关时为 `None`）
    pub cut_report: Option<cut::CutReport>,
}

pub struct TextTaskEngine;

const LINE_SPACING_RATIO: f64 = 0.2;
const MIN_FONT_SIZE_MM: f64 = 1.0;

fn ink_bounds(paths: &Paths) -> Option<(f64, f64, f64, f64)> {
    let mut points = paths.iter().flatten();
    let first = points.next()?;
    Some(points.fold(
        (first.0, first.1, first.0, first.1),
        |(x0, y0, x1, y1), p| (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1)),
    ))
}

/// 排版并把笔画平移到以墨迹左上角为 (0,0)（y 向下）；返回 (笔画, 宽, 高, 字数)。
fn layout(
    data: &[u8],
    spec: &TextTaskSpec<'_>,
    size: f64,
) -> Result<(Paths, f64, f64, usize), String> {
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
    let body = placed
        .strip_suffix(tail)
        .ok_or("生成的 G-code 结尾格式异常")?;
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
        let data =
            std::fs::read(spec.font_path).map_err(|_| "font file cannot be read".to_owned())?;
        let positive = |v: Option<f64>, name: &str| -> Result<Option<f64>, String> {
            match v {
                None => Ok(None),
                Some(v) if v.is_finite() && v > 0.0 => Ok(Some(v)),
                Some(_) => Err(format!("{name} 必须是大于 0 的数字")),
            }
        };
        let board_w = positive(spec.board_width_mm, "木牌宽度")?;
        let board_h = positive(spec.board_height_mm, "木牌高度")?;
        // 填充的过扫描让扫描线越出笔画：放进木牌时两侧留边至少容得下它（与旧位图扫描一致）
        let fills = matches!(spec.fill, TextFill::Fill | TextFill::Hatch);
        let (over_x, over_y) = if fills {
            spec.scan.extension_mm()
        } else {
            (0.0, 0.0)
        };
        // 切缝外扩与引线同样会越出笔画，各方向都要留出来
        spec.cut.validate().map_err(|e| e.to_string())?;
        let cut_reach = if spec.cut.is_active() {
            spec.cut.extension_mm()
        } else {
            0.0
        };
        let (over_x, over_y) = (over_x.max(cut_reach), over_y.max(cut_reach));
        let margin_x = spec.margin_mm.max(over_x);
        let margin_y = spec.margin_mm.max(over_y);
        let avail_w = board_w.map(|b| b - 2.0 * margin_x);
        let avail_h = board_h.map(|b| b - 2.0 * margin_y);
        if avail_w.is_some_and(|v| v <= 0.0) || avail_h.is_some_and(|v| v <= 0.0) {
            return Err("木牌尺寸小于两侧留边，放不下任何文字".into());
        }

        let mut size = spec.font_size_mm;
        let (mut paths, mut width, mut height, mut glyphs) = layout(&data, spec, size)?;
        let effective_w = |w: f64| w;
        let fits = |w: f64, h: f64| {
            avail_w.is_none_or(|a| effective_w(w) <= a + 1e-6)
                && avail_h.is_none_or(|a| h <= a + 1e-6)
        };
        if board_w.is_some() || board_h.is_some() {
            if spec.auto_size {
                // 字号与笔画尺寸近似成正比，字距是固定 mm，迭代几次收敛到能放下的最大字号
                for _ in 0..5 {
                    let cur_w = effective_w(width);
                    let ratio_w = avail_w.map_or(f64::INFINITY, |a| a / cur_w);
                    let ratio_h = avail_h.map_or(f64::INFINITY, |a| a / height);
                    let ratio = ratio_w.min(ratio_h);
                    if fits(width, height) && ratio < 1.001 {
                        break;
                    }
                    let step = if !fits(width, height) && ratio >= 0.998 {
                        0.995
                    } else {
                        ratio * 0.999
                    };
                    size *= step;
                    if size < MIN_FONT_SIZE_MM {
                        return Err(format!(
                            "文字太长，放进这块木牌后字高不到 {MIN_FONT_SIZE_MM} mm，请减少字数或分行"
                        ));
                    }
                    (paths, width, height, glyphs) = layout(&data, spec, size)?;
                }
            }
            if !fits(width, height) {
                return Err(format!(
                    "按 {size:.1} mm 字号文字约 {width:.1} × {height:.1} mm，放不进 {} × {} mm 的木牌（四周各留 {:.1} mm）；请减小字号，或不指定字号让 MOSS 自动适配",
                    board_w.map_or("—".to_owned(), |v| format!("{v:.0}")),
                    board_h.map_or("—".to_owned(), |v| format!("{v:.0}")),
                    margin_x
                ));
            }
        }

        // 激光头起点在图案坐标（y 向上，笔画范围 0..width × 0..height）中的位置
        let (start_x, start_y) = match spec.origin {
            TextOrigin::Center => (width / 2.0, height / 2.0),
            // 没给木牌时激光头在过扫描范围的左下角，扫描线不会往起点的负方向越界
            TextOrigin::LowerLeft => (
                -board_w.map_or(over_x, |b| (b - width) / 2.0),
                -board_h.map_or(over_y, |b| (b - height) / 2.0),
            ),
        };
        let frame = HeadRelativeBox {
            min_x: -start_x,
            max_x: width - start_x,
            min_y: -start_y,
            max_y: height - start_y,
        };

        // 矢量规划（2026-11-16 用户决定：文字一律转矢量再规划路线；同日对齐 LightBurn：先扫描填充、再描轮廓）。
        // 笔画坐标 y 向下，激光头在 (start_x, height - start_y)；扫描角度按机器视角逆时针，y 向下时取反
        let head = (start_x, height - start_y);
        // 切缝补偿：规划前把轮廓整体外扩 / 内缩（孔反向），规划自检的是补偿后的轮廓
        let mut kerf_contours = None;
        let contours: Vec<vector_plan::Unit> = if spec.cut.kerf_mm != 0.0 {
            let offset =
                cut::offset_contours(&paths, spec.cut.kerf_mm).map_err(|e| e.to_string())?;
            kerf_contours = Some((offset.before, offset.after));
            offset
                .contours
                .into_iter()
                .map(vector_plan::Unit::Closed)
                .collect()
        } else {
            paths
                .iter()
                .cloned()
                .map(vector_plan::Unit::Closed)
                .collect()
        };
        let fill_options = fills.then(|| vector_plan::FillOptions {
            spacing: if spec.fill == TextFill::Fill {
                spec.pixel_size_mm
            } else {
                spec.pixel_size_mm * HATCH_SPACING_FACTOR
            },
            rule: vector_plan::FillRule::NonZero,
            ..spec.scan
        });
        let layers = match fill_options {
            None => vec![contours],
            Some(options) => {
                let fill = vector_plan::scan_fill(
                    &paths,
                    &vector_plan::FillOptions {
                        angle_deg: -options.angle_deg,
                        ..options
                    },
                )
                .map_err(|e| e.to_string())?;
                vec![fill, contours]
            }
        };
        let mut planned =
            vector_plan::plan_layers(layers, head, &spec.plan).map_err(|e| e.to_string())?;
        // 桥位、引线、过切：在规划（含出光长度自检）之后改写闭合轮廓，后处理另有自检
        let cut_report = if spec.cut.is_active() {
            let mut report = if spec.cut.needs_post_processing() {
                let (steps, report) =
                    cut::apply_cut_options(&planned.steps, &spec.cut).map_err(|e| e.to_string())?;
                planned.paths = vector_plan::burn_paths(&steps);
                planned.steps = steps;
                report
            } else {
                cut::CutReport::default()
            };
            report.kerf_contours = kerf_contours;
            if report.contours == 0 {
                report.contours = kerf_contours.map_or(0, |(_, after)| after);
            }
            Some(report)
        } else {
            None
        };
        let options = gcode::WriterOptions {
            width_mm: width.max(1e-3),
            height_mm: height.max(1e-3),
            power: spec.power,
            feed_rate: spec.feed_rate,
            auto_close: false,
            laser_mode: "M4",
            power_scale: None,
        };
        let written = gcode::write_steps(&planned.steps, &options)?;
        let preview = gcode::preview_outline_rgba_with_options(
            &planned.paths,
            spec.preview_width_px,
            spec.preview_height_px,
            options.width_mm,
            options.height_mm,
            false,
        )?;
        let gcode = place_at_head(&written, start_x, start_y)?;
        let estimate = gcode::estimate_time(&gcode, 3000.0);
        Ok(TextTaskArtifacts {
            gcode,
            preview,
            width_mm: width,
            height_mm: height,
            glyphs,
            path_optimization: planned.stats,
            estimate,
            font_size_mm: size,
            fill: spec.fill,
            origin: spec.origin,
            frame,
            fill_options,
            removed_duplicates: planned.removed_duplicates,
            cut_report,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/fonts/geometry.ttf"
    );

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
            scan: vector_plan::FillOptions {
                overscan_mm: 1.0,
                ..vector_plan::FillOptions::new(0.1, vector_plan::FillRule::NonZero)
            },
            plan: vector_plan::PlanOptions::default(),
            cut: cut::CutOptions::default(),
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
        assert!(
            (e.min_x + e.max_x).abs() < 0.01 && (e.min_y + e.max_y).abs() < 0.01,
            "{e:?}"
        );
        assert!((job.frame.min_x + job.width_mm / 2.0).abs() < 1e-9);
        assert!(
            job.gcode.trim_end().ends_with(&format!(
                "G0 X{:.3} Y{:.3}\nM2",
                job.width_mm / 2.0,
                job.height_mm / 2.0
            )),
            "{}",
            job.gcode
        );

        let lower_left = TextTaskEngine::generate(&TextTaskSpec {
            origin: TextOrigin::LowerLeft,
            ..spec("AB")
        })
        .unwrap();
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
        assert!(
            fitted.width_mm <= 76.0 + 1e-6 && fitted.height_mm <= 26.0 + 1e-6,
            "{} × {}",
            fitted.width_mm,
            fitted.height_mm
        );
        assert!(
            fitted.width_mm > 70.0 || fitted.height_mm > 24.0,
            "应接近能放下的最大字号：{} × {}",
            fitted.width_mm,
            fitted.height_mm
        );
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
        assert!(
            corner.gcode.contains(&format!("G92 X{:.3}", -left)),
            "{}",
            &corner.gcode[..300]
        );
        assert!(
            ((80.0 - e.max_x) - left).abs() < 0.05,
            "左右留白应相等：{e:?}"
        );

        let err = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(30.0),
            font_size_mm: 20.0,
            ..spec("ABAB")
        })
        .unwrap_err();
        assert!(err.contains("放不进"), "{err}");
        assert!(
            TextTaskEngine::generate(&TextTaskSpec {
                board_width_mm: Some(3.0),
                auto_size: true,
                ..spec("A")
            })
            .is_err()
        );
    }

    #[test]
    fn multiline_text_is_aligned_and_stacked() {
        let one = TextTaskEngine::generate(&spec("AB")).unwrap();
        let two = TextTaskEngine::generate(&spec("AB\nA")).unwrap();
        assert!(two.height_mm > one.height_mm * 1.8);
        assert_eq!(two.glyphs, 3);
    }

    #[test]
    fn fill_is_vector_scanning_with_overscan_outside_the_strokes() {
        // 2026-11-16 起实心填充是矢量扫描线；对齐 LightBurn，每行两头各多走 1 mm 关光过扫描
        let filled = TextTaskEngine::generate(&TextTaskSpec {
            fill: TextFill::Fill,
            ..spec("AB")
        })
        .unwrap();
        let outline = TextTaskEngine::generate(&spec("AB")).unwrap();
        let burns = filled.gcode.lines().filter(|l| l.contains(" S420")).count();
        assert!(burns > 100, "实心填充应有大量出光段：{burns}");
        assert!(
            filled
                .gcode
                .lines()
                .any(|l| l.starts_with("G1 ") && l.contains(" S0")),
            "过扫描是 G1 S0 匀速滑行，不是 G0"
        );
        let (ef, eo) = (extents(&filled.gcode), extents(&outline.gcode));
        assert!(
            (eo.min_x - ef.min_x - 1.0).abs() < 0.01 && (ef.max_x - eo.max_x - 1.0).abs() < 0.01,
            "横扫的过扫描只往 x 两侧各伸 1 mm：{ef:?} vs {eo:?}"
        );
        assert!((ef.min_y - eo.min_y).abs() < 0.01 && (ef.max_y - eo.max_y).abs() < 0.01);
        assert_eq!(filled.frame, outline.frame);
        assert!(filled.width_mm == outline.width_mm);
        assert!(!filled.gcode.contains("; raster"), "不再生成位图扫描");
        assert_eq!(filled.fill_options.unwrap().overscan_mm, 1.0);

        // 不过扫描时范围与轮廓完全相同
        let tight = TextTaskEngine::generate(&TextTaskSpec {
            fill: TextFill::Fill,
            scan: vector_plan::FillOptions {
                overscan_mm: 0.0,
                ..spec("AB").scan
            },
            ..spec("AB")
        })
        .unwrap();
        let et = extents(&tight.gcode);
        assert!((et.min_x - eo.min_x).abs() < 0.01 && (et.max_x - eo.max_x).abs() < 0.01);

        // 左下角起点：激光头在过扫描范围左下角，不往负方向越界
        let filled_ll = TextTaskEngine::generate(&TextTaskSpec {
            fill: TextFill::Fill,
            origin: TextOrigin::LowerLeft,
            ..spec("AB")
        })
        .unwrap();
        let e_ll = extents(&filled_ll.gcode);
        assert!(e_ll.min_x >= -1e-6 && e_ll.min_y >= -1e-6, "{e_ll:?}");
        assert!((filled_ll.frame.min_x - 1.0).abs() < 1e-9);

        // 木牌留边为 0、自动适配字号时，过扫描也留在木牌内
        let filled_board = TextTaskEngine::generate(&TextTaskSpec {
            fill: TextFill::Fill,
            origin: TextOrigin::LowerLeft,
            board_width_mm: Some(50.0),
            board_height_mm: Some(30.0),
            margin_mm: 0.0,
            auto_size: true,
            ..spec("ABAB")
        })
        .unwrap();
        let e_board = extents(&filled_board.gcode);
        assert!(
            e_board.min_x >= -1e-6 && e_board.max_x <= 50.0 + 1e-6,
            "{e_board:?}"
        );
        // 规划减少了空走
        assert!(
            filled.path_optimization.travel_distance_after
                <= filled.path_optimization.travel_distance_before,
            "{:?}",
            filled.path_optimization
        );

        // 竖扫（90°）：过扫描改往 y 两侧伸
        let vertical = TextTaskEngine::generate(&TextTaskSpec {
            fill: TextFill::Fill,
            scan: vector_plan::FillOptions {
                angle_deg: 90.0,
                ..spec("AB").scan
            },
            ..spec("AB")
        })
        .unwrap();
        let ev = extents(&vertical.gcode);
        assert!((eo.min_y - ev.min_y - 1.0).abs() < 0.01, "{ev:?} vs {eo:?}");
        assert!((ev.min_x - eo.min_x).abs() < 0.01);
    }

    #[test]
    fn cut_text_cuts_inner_contours_first() {
        // “A”的三角形内孔要先于外轮廓切
        let job = TextTaskEngine::generate(&TextTaskSpec {
            plan: vector_plan::PlanOptions {
                inside_first: true,
                ..vector_plan::PlanOptions::default()
            },
            ..spec("A")
        })
        .unwrap();
        assert!(
            job.path_optimization
                .strategy
                .starts_with("nearest_neighbor")
        );
        assert!(job.gcode.contains(" S420"));
    }

    #[test]
    fn cut_options_offset_tab_and_lead_the_text_contours() {
        let cut_plan = vector_plan::PlanOptions {
            inside_first: true,
            ..vector_plan::PlanOptions::default()
        };
        let plain = TextTaskEngine::generate(&TextTaskSpec {
            plan: cut_plan,
            ..spec("A")
        })
        .unwrap();
        assert!(plain.cut_report.is_none(), "默认全关，输出不变");
        let kerf = TextTaskEngine::generate(&TextTaskSpec {
            plan: cut_plan,
            cut: cut::CutOptions {
                kerf_mm: 0.2,
                ..cut::CutOptions::default()
            },
            ..spec("A")
        })
        .unwrap();
        let report = kerf.cut_report.clone().unwrap();
        assert_eq!(report.kerf_contours, Some((2, 2)), "{report:?}");
        // 外框往外 0.2 mm：刀路范围每边大 0.2
        let (ep, ek) = (extents(&plain.gcode), extents(&kerf.gcode));
        assert!(
            ((ek.max_x - ek.min_x) - (ep.max_x - ep.min_x) - 0.4).abs() < 0.01,
            "{ep:?} {ek:?}"
        );
        assert!(((ek.max_y - ek.min_y) - (ep.max_y - ep.min_y) - 0.4).abs() < 0.01);

        let tabbed = TextTaskEngine::generate(&TextTaskSpec {
            plan: cut_plan,
            origin: TextOrigin::LowerLeft,
            cut: cut::CutOptions {
                tabs: cut::TabMode::Count(3),
                tab_width_mm: 0.5,
                lead_in_mm: 1.0,
                overcut_mm: 0.5,
                ..cut::CutOptions::default()
            },
            ..spec("A")
        })
        .unwrap();
        let report = tabbed.cut_report.clone().unwrap();
        assert_eq!(report.contours, 2);
        assert_eq!(report.tabs, 3, "默认跳过内部图形（A 的内孔）：{report:?}");
        assert!(
            tabbed
                .gcode
                .lines()
                .filter(|l| l.starts_with("G1 ") && l.contains(" S0"))
                .count()
                >= 3,
            "桥位是 G1 S0"
        );
        // 左下角起点留出了引线的距离：刀路不往负方向越界
        let e = extents(&tabbed.gcode);
        assert!(e.min_x >= -1e-6 && e.min_y >= -1e-6, "{e:?}");

        let too_small = TextTaskEngine::generate(&TextTaskSpec {
            plan: cut_plan,
            cut: cut::CutOptions {
                kerf_mm: -2.0,
                ..cut::CutOptions::default()
            },
            font_size_mm: 3.0,
            ..spec("A")
        });
        assert!(too_small.is_err(), "全部内缩没了要报错");
    }

    #[test]
    fn hatch_adds_parallel_lines_to_the_outline() {
        let outline = TextTaskEngine::generate(&spec("A")).unwrap();
        let hatch = TextTaskEngine::generate(&TextTaskSpec {
            fill: TextFill::Hatch,
            pixel_size_mm: 0.2,
            ..spec("A")
        })
        .unwrap();
        assert!(hatch.gcode.lines().count() > outline.gcode.lines().count() * 2);
        // 排线线距是实心的 3 倍：出光段少得多
        let filled = TextTaskEngine::generate(&TextTaskSpec {
            fill: TextFill::Fill,
            pixel_size_mm: 0.2,
            ..spec("A")
        })
        .unwrap();
        assert!(filled.gcode.lines().count() > hatch.gcode.lines().count() * 2);
        assert!(TextFill::parse(Some("solid")).is_err());
        assert!(TextOrigin::parse(Some("top")).is_err());
        assert!(parse_alignment(Some("justify")).is_err());
        assert_eq!(TextFill::parse(None).unwrap(), TextFill::Outline);
        assert_eq!(TextOrigin::parse(None).unwrap(), TextOrigin::Center);
    }
}
