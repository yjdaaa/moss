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
//! - 焊接与基础图形（2026-11-17 用户决定，技术书 r44）：字形轮廓在规划前按非零环绕取并集（对齐 LightBurn
//!   “Weld”），笔画交叠、字与字相连的字体不会在交叠处多刻一道线；可同时放 [`crate::shapes`] 的基础图形
//!   与边框，文字自动适配进边框里面或 `text_box`。版面以木牌中心为原点，激光头起点仍是木牌中心 /
//!   左下角；图形超出木牌、与文字重叠直接报错，不悄悄挪动。刻得好坏只取决于机器与材料库参数。
//! - 转图片（2026-11-17 用户决定，技术书 r45）：[`TextTaskSpec::image`] 为 true 时，版面（焊接后的文字一律
//!   实心、填满的图形、描边图形与直线按 [`IMAGE_STROKE_MIN_MM`] 线宽）按材料线距渲染成黑白位图，网格与版面
//!   左下角对齐，再逐行扫描（过扫描、双向与扫描偏移取 `scan`）；起点、走边框范围与矢量相同。切割不能转图片。

use crate::{cut, font, gcode, shapes, vector_plan};
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
    /// 基础图形（木牌中心为原点、y 向上，mm）；文字可以为空
    pub shapes: &'a [shapes::Shape],
    /// 沿木牌边缘的边框；文字自动放进边框里面
    pub border: Option<shapes::Border>,
    /// 文字放进这个矩形区域（木牌坐标）；给了就不按边框 / 整块木牌排版
    pub text_box: Option<TextBox>,
    /// 规划前合并重叠的字形轮廓（默认开）
    pub weld: bool,
    /// 转图片：版面渲染成位图后逐行扫描（文字一律实心）；false 为矢量规划
    pub image: bool,
}

/// 转图片时描边图形与直线的最小线宽（mm）；实际取它与两倍线距中较大的。
pub const IMAGE_STROKE_MIN_MM: f64 = 0.3;

/// 转图片的位图最多多少个像素。
pub const MAX_IMAGE_PIXELS: usize = 16_000_000;

/// 转图片的位图信息。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageRaster {
    pub grid_width: usize,
    pub grid_height: usize,
    pub pixel_size_mm: f64,
}

/// 文字区域（木牌中心为原点、y 向上，mm）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextBox {
    pub x_mm: f64,
    pub y_mm: f64,
    pub width_mm: f64,
    pub height_mm: f64,
}

/// 焊接统计：合并前后的字形轮廓数（`None` = 没有焊接）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeldReport {
    pub before: usize,
    pub after: usize,
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
    /// 焊接统计（没有文字或关闭焊接时为 `None`）
    pub weld: Option<WeldReport>,
    /// 图形个数（含边框线）
    pub shapes: usize,
    /// 转图片时的位图（矢量规划时为 `None`）
    pub raster: Option<ImageRaster>,
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

/// 焊接：一组闭合轮廓按非零环绕取并集（对齐 LightBurn “Weld”），返回首尾点相同的闭合轮廓。
pub fn weld_contours(paths: &Paths) -> Result<(Paths, WeldReport), String> {
    let welded = cut::offset_contours(paths, 0.0).map_err(|e| match e.0 {
        "没有可切割的闭合图形" => "文字没有可雕刻的闭合笔画".to_owned(),
        "切割图形太复杂，无法做切缝补偿" => {
            "文字笔画太复杂，无法合并重叠笔画；请减少字数或关闭焊接".to_owned()
        }
        other => other.to_owned(),
    })?;
    let report = WeldReport {
        before: paths.len(),
        after: welded.contours.len(),
    };
    Ok((welded.contours, report))
}

/// 一个图形在版面里的几何（木牌坐标转成 y 向下后）。
struct PlacedShape {
    shape: shapes::Shape,
    geometry: shapes::ShapeGeometry,
}

/// 图形与文字墨迹框 `ink`（y 向上，min_x, min_y, max_x, max_y）是否重叠。
fn overlaps_text(placed: &PlacedShape, ink: (f64, f64, f64, f64)) -> bool {
    const GAP_MM: f64 = 0.2;
    let rect = (
        ink.0 - GAP_MM,
        ink.1 - GAP_MM,
        ink.2 + GAP_MM,
        ink.3 + GAP_MM,
    );
    let points = placed.geometry.points();
    if points
        .windows(2)
        .any(|w| shapes::segment_hits_rect(w[0], w[1], rect))
    {
        return true;
    }
    // 填满的图形把整段文字包在里面也算重叠（只描边的外框不算）
    placed.shape.fill
        && matches!(placed.geometry, shapes::ShapeGeometry::Closed(_))
        && vector_plan::point_in_polygon(((ink.0 + ink.2) / 2.0, (ink.1 + ink.3) / 2.0), points)
}

impl TextTaskEngine {
    pub fn generate(spec: &TextTaskSpec<'_>) -> Result<TextTaskArtifacts, String> {
        if !(spec.pixel_size_mm.is_finite() && (0.05..=1.0).contains(&spec.pixel_size_mm)) {
            return Err("pixel_size_mm 必须在 0.05 ~ 1 mm 之间".into());
        }
        if !(spec.margin_mm.is_finite() && spec.margin_mm >= 0.0) {
            return Err("留边必须是不小于 0 的数字".into());
        }
        let has_text = !spec.text.trim().is_empty();
        if !has_text && spec.shapes.is_empty() && spec.border.is_none() {
            return Err("要刻的文字和图形不能都为空".into());
        }
        let data = if has_text {
            std::fs::read(spec.font_path).map_err(|_| "font file cannot be read".to_owned())?
        } else {
            Vec::new()
        };
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
        // 转图片：文字一律实心，位图横向逐行扫描，过扫描只往 x 两侧伸
        let image = spec.image;
        if image && spec.cut.is_active() {
            return Err("切割不能转图片，请用矢量".into());
        }
        let fills = has_text && (image || matches!(spec.fill, TextFill::Fill | TextFill::Hatch));
        let shape_fills = spec.shapes.iter().any(|s| s.fill);
        let (over_x, over_y) = if image {
            (spec.scan.overscan_mm, 0.0)
        } else if fills || shape_fills {
            spec.scan.extension_mm()
        } else {
            (0.0, 0.0)
        };
        let (text_over_x, text_over_y) = if fills { (over_x, over_y) } else { (0.0, 0.0) };
        // 切缝外扩与引线同样会越出笔画，各方向都要留出来
        spec.cut.validate().map_err(|e| e.to_string())?;
        let cut_reach = if spec.cut.is_active() {
            spec.cut.extension_mm()
        } else {
            0.0
        };
        let (text_over_x, text_over_y) = (text_over_x.max(cut_reach), text_over_y.max(cut_reach));
        let (over_x, over_y) = (over_x.max(cut_reach), over_y.max(cut_reach));
        let margin_x = spec.margin_mm.max(text_over_x);
        let margin_y = spec.margin_mm.max(text_over_y);

        // 图形：用户给的 + 边框生成的线
        let mut all_shapes: Vec<shapes::Shape> = spec.shapes.to_vec();
        let mut region = None;
        if let Some(border) = spec.border {
            let (ring, inner) = border.build(board_w, board_h, spec.margin_mm)?;
            all_shapes.extend(ring);
            region = Some(inner);
        }
        if all_shapes.len() > shapes::MAX_SHAPES {
            return Err(format!("图形最多 {} 个（含边框线）", shapes::MAX_SHAPES));
        }
        if let Some(text_box) = spec.text_box {
            let w = positive(Some(text_box.width_mm), "text_box 宽度")?;
            let h = positive(Some(text_box.height_mm), "text_box 高度")?;
            if !(text_box.x_mm.is_finite() && text_box.y_mm.is_finite()) {
                return Err("text_box 的 x_mm / y_mm 必须是数字".into());
            }
            let (w, h) = (w.unwrap_or_default(), h.unwrap_or_default());
            let outside = board_w.is_some_and(|b| text_box.x_mm.abs() + w / 2.0 > b / 2.0 + 1e-6)
                || board_h.is_some_and(|b| text_box.y_mm.abs() + h / 2.0 > b / 2.0 + 1e-6);
            if outside {
                return Err("text_box 超出了木牌范围".into());
            }
            region = Some(shapes::TextRegion::Rect {
                cx: text_box.x_mm,
                cy: text_box.y_mm,
                width: Some(w),
                height: Some(h),
            });
        }
        let default_region = region.is_none();
        let region = match region {
            Some(region) => region,
            None => {
                let avail_w = board_w.map(|b| b - 2.0 * margin_x);
                let avail_h = board_h.map(|b| b - 2.0 * margin_y);
                if has_text
                    && (avail_w.is_some_and(|v| v <= 0.0) || avail_h.is_some_and(|v| v <= 0.0))
                {
                    return Err("木牌尺寸小于两侧留边，放不下任何文字".into());
                }
                shapes::TextRegion::Rect {
                    cx: 0.0,
                    cy: 0.0,
                    width: avail_w,
                    height: avail_h,
                }
            }
        };

        // 文字：按区域自动适配字号，墨迹框以区域中心为中心
        let mut size = spec.font_size_mm;
        let (mut paths, mut ink_w, mut ink_h, mut glyphs) = if has_text {
            layout(&data, spec, size)?
        } else {
            (Vec::new(), 0.0, 0.0, 0)
        };
        if has_text && region.is_bounded() {
            if spec.auto_size {
                // 字号与笔画尺寸近似成正比，字距是固定 mm，迭代几次收敛到能放下的最大字号
                for _ in 0..5 {
                    let ratio = region.ratio(ink_w, ink_h);
                    let fits = region.fits(ink_w, ink_h);
                    if fits && ratio < 1.001 {
                        break;
                    }
                    let step = if !fits && ratio >= 0.998 {
                        0.995
                    } else {
                        ratio * 0.999
                    };
                    size *= step;
                    if size < MIN_FONT_SIZE_MM {
                        return Err(format!(
                            "文字太长，放进{}后字高不到 {MIN_FONT_SIZE_MM} mm，请减少字数或分行",
                            if default_region {
                                "这块木牌"
                            } else {
                                "文字区域"
                            }
                        ));
                    }
                    (paths, ink_w, ink_h, glyphs) = layout(&data, spec, size)?;
                }
            }
            if !region.fits(ink_w, ink_h) {
                return Err(if default_region {
                    format!(
                        "按 {size:.1} mm 字号文字约 {ink_w:.1} × {ink_h:.1} mm，放不进 {} × {} mm 的木牌（四周各留 {:.1} mm）；请减小字号，或不指定字号让 MOSS 自动适配",
                        board_w.map_or("—".to_owned(), |v| format!("{v:.0}")),
                        board_h.map_or("—".to_owned(), |v| format!("{v:.0}")),
                        margin_x
                    )
                } else {
                    format!(
                        "按 {size:.1} mm 字号文字约 {ink_w:.1} × {ink_h:.1} mm，放不进{}；请减小字号，或不指定字号让 MOSS 自动适配",
                        if spec.text_box.is_some() {
                            " text_box"
                        } else {
                            "边框里面"
                        }
                    )
                });
            }
        }
        let (rcx, rcy) = region.center();
        // 墨迹框（木牌坐标，y 向上）
        let ink = (
            rcx - ink_w / 2.0,
            rcy - ink_h / 2.0,
            rcx + ink_w / 2.0,
            rcy + ink_h / 2.0,
        );

        // 图形几何（木牌坐标，y 向上），检查是否超出木牌、是否与文字重叠
        let mut placed = Vec::with_capacity(all_shapes.len());
        for shape in &all_shapes {
            let geometry = shape.geometry()?;
            let (x0, y0, x1, y1) =
                shapes::bounds(geometry.points()).ok_or("图形没有可雕刻的线条")?;
            let (ox, oy) = if shape.fill || image {
                (over_x, over_y)
            } else {
                (0.0, 0.0)
            };
            let outside = board_w
                .is_some_and(|b| x0 - ox < -b / 2.0 - 1e-6 || x1 + ox > b / 2.0 + 1e-6)
                || board_h.is_some_and(|b| y0 - oy < -b / 2.0 - 1e-6 || y1 + oy > b / 2.0 + 1e-6);
            if outside {
                return Err(format!(
                    "{}（中心 {:.1}, {:.1}，{:.1} × {:.1} mm）超出了木牌{}；请缩小或往里挪",
                    shape.kind.label(),
                    shape.x_mm,
                    shape.y_mm,
                    x1 - x0,
                    y1 - y0,
                    if shape.fill && (ox > 0.0 || oy > 0.0) {
                        "（填满时两头还要留出过扫描）"
                    } else {
                        ""
                    }
                ));
            }
            let item = PlacedShape {
                shape: *shape,
                geometry,
            };
            if has_text && overlaps_text(&item, ink) {
                return Err(format!(
                    "{}（中心 {:.1}, {:.1}）和文字重叠了；请挪开图形，或用 text_box 给文字留出位置",
                    shape.kind.label(),
                    shape.x_mm,
                    shape.y_mm
                ));
            }
            placed.push(item);
        }

        // 版面范围（y 向下）：文字墨迹框与全部图形；平移到左上角为 (0,0)
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        let mut grow = |x: f64, y_down: f64| {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y_down);
            max_y = max_y.max(y_down);
        };
        if has_text {
            grow(ink.0, -ink.3);
            grow(ink.2, -ink.1);
        }
        for item in &placed {
            for p in item.geometry.points() {
                grow(p.0, -p.1);
            }
        }
        let (width, height) = (max_x - min_x, max_y - min_y);
        // 文字笔画（墨迹左上角为 0,0）在版面里的平移；只有文字时为 0
        let (ink_dx, ink_dy) = ((rcx - ink_w / 2.0) - min_x, (-rcy - ink_h / 2.0) - min_y);
        if ink_dx != 0.0 || ink_dy != 0.0 {
            for point in paths.iter_mut().flatten() {
                point.0 += ink_dx;
                point.1 += ink_dy;
            }
        }
        let to_layout =
            |points: &[laser_contracts::geometry::Point]| -> Vec<laser_contracts::geometry::Point> {
                points.iter().map(|p| (p.0 - min_x, -p.1 - min_y)).collect()
            };

        // 激光头起点在图案坐标（y 向上，版面范围 0..width × 0..height）中的位置：木牌中心在版面里是 (-min_x, -min_y)
        let (board_cx, board_cy_up) = (-min_x, height + min_y);
        let (start_x, start_y) = match spec.origin {
            TextOrigin::Center => (board_cx, board_cy_up),
            // 没给木牌时激光头在过扫描范围的左下角，扫描线不会往起点的负方向越界
            TextOrigin::LowerLeft => (
                board_w.map_or(-over_x, |b| board_cx - b / 2.0),
                board_h.map_or(-over_y, |b| board_cy_up - b / 2.0),
            ),
        };
        let frame = HeadRelativeBox {
            min_x: -start_x,
            max_x: width - start_x,
            min_y: -start_y,
            max_y: height - start_y,
        };

        // 焊接（2026-11-17 技术书 r44）：字形轮廓按非零环绕取并集，交叠处不再多一道线；
        // 填充本来就按非零环绕算，焊接前后填满的区域相同
        let mut weld = None;
        if has_text && spec.weld && !paths.is_empty() {
            let (welded, report) = weld_contours(&paths)?;
            weld = Some(report);
            paths = welded;
        }

        if image {
            return rasterize_layout(&RasterLayout {
                spec,
                text: &paths,
                placed: &placed,
                to_layout: &to_layout,
                width,
                height,
                start: (start_x, start_y),
                frame,
                glyphs,
                font_size_mm: if has_text { size } else { 0.0 },
                weld,
            });
        }

        // 矢量规划（2026-11-16 用户决定：文字一律转矢量再规划路线；同日对齐 LightBurn：先扫描填充、再描轮廓）。
        // 笔画坐标 y 向下，激光头在 (start_x, height - start_y)；扫描角度按机器视角逆时针，y 向下时取反
        let head = (start_x, height - start_y);
        // 切缝补偿：规划前把轮廓整体外扩 / 内缩（孔反向），规划自检的是补偿后的轮廓；图形各自补偿
        let mut kerf_contours = None;
        let mut contours: Vec<vector_plan::Unit> = Vec::new();
        let mut filled_shapes: Vec<Vec<laser_contracts::geometry::Point>> = Vec::new();
        let kerf = |ring: &[Vec<laser_contracts::geometry::Point>]| {
            cut::offset_contours(ring, spec.cut.kerf_mm).map_err(|e| e.to_string())
        };
        let (mut kerf_before, mut kerf_after) = (0, 0);
        if spec.cut.kerf_mm != 0.0 && !paths.is_empty() {
            let offset = kerf(&paths)?;
            kerf_before += offset.before;
            kerf_after += offset.after;
            contours.extend(offset.contours.into_iter().map(vector_plan::Unit::Closed));
        } else {
            contours.extend(paths.iter().cloned().map(vector_plan::Unit::Closed));
        }
        for item in &placed {
            let points = to_layout(item.geometry.points());
            match item.geometry {
                shapes::ShapeGeometry::Open(_) => contours.push(vector_plan::Unit::Open(points)),
                shapes::ShapeGeometry::Closed(_) => {
                    if item.shape.fill {
                        filled_shapes.push(points.clone());
                    }
                    if spec.cut.kerf_mm != 0.0 {
                        let offset = kerf(std::slice::from_ref(&points))?;
                        kerf_before += offset.before;
                        kerf_after += offset.after;
                        contours.extend(offset.contours.into_iter().map(vector_plan::Unit::Closed));
                    } else {
                        contours.push(vector_plan::Unit::Closed(points));
                    }
                }
            }
        }
        if spec.cut.kerf_mm != 0.0 {
            kerf_contours = Some((kerf_before, kerf_after));
        }
        let fill_options = (fills || shape_fills).then(|| vector_plan::FillOptions {
            spacing: if !fills || spec.fill == TextFill::Fill {
                spec.pixel_size_mm
            } else {
                spec.pixel_size_mm * HATCH_SPACING_FACTOR
            },
            rule: vector_plan::FillRule::NonZero,
            ..spec.scan
        });
        let scan = |rings: &[Vec<laser_contracts::geometry::Point>], spacing: f64| {
            let options = fill_options.expect("有填充时才扫描");
            vector_plan::scan_fill(
                rings,
                &vector_plan::FillOptions {
                    angle_deg: -options.angle_deg,
                    spacing,
                    ..options
                },
            )
            .map_err(|e| e.to_string())
        };
        let mut fill_units = Vec::new();
        if fills {
            let spacing = fill_options.map_or(spec.pixel_size_mm, |o| o.spacing);
            fill_units.extend(scan(&paths, spacing)?);
        }
        if shape_fills {
            // 图形填满一律实心线距（材料像素间距）
            fill_units.extend(scan(&filled_shapes, spec.pixel_size_mm)?);
        }
        let layers = if fill_units.is_empty() {
            vec![contours]
        } else {
            vec![fill_units, contours]
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
            font_size_mm: if has_text { size } else { 0.0 },
            fill: spec.fill,
            origin: spec.origin,
            frame,
            fill_options,
            removed_duplicates: planned.removed_duplicates,
            cut_report,
            weld,
            shapes: placed.len(),
            raster: None,
        })
    }
}

/// 转图片需要的版面信息（坐标：版面左上角为 0、y 向下）。
struct RasterLayout<'a, 'b> {
    spec: &'a TextTaskSpec<'b>,
    text: &'a Paths,
    placed: &'a [PlacedShape],
    to_layout:
        &'a dyn Fn(&[laser_contracts::geometry::Point]) -> Vec<laser_contracts::geometry::Point>,
    width: f64,
    height: f64,
    start: (f64, f64),
    frame: HeadRelativeBox,
    glyphs: usize,
    font_size_mm: f64,
    weld: Option<WeldReport>,
}

/// 非零环绕规则下，水平线 `y` 穿过的区间（x 从小到大）。
fn nonzero_spans(rings: &[Vec<laser_contracts::geometry::Point>], y: f64) -> Vec<(f64, f64)> {
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for ring in rings {
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            if (a.1 <= y && b.1 > y) || (b.1 <= y && a.1 > y) {
                let x = a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0);
                crossings.push((x, if b.1 > a.1 { 1 } else { -1 }));
            }
        }
    }
    crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut spans = Vec::new();
    let mut winding = 0;
    let mut open = 0.0;
    for (x, dir) in crossings {
        let was = winding != 0;
        winding += dir;
        match (was, winding != 0) {
            (false, true) => open = x,
            (true, false) => spans.push((open, x)),
            _ => {}
        }
    }
    spans
}

/// 版面渲染成位图并逐行扫描（见模块说明“转图片”）。
fn rasterize_layout(layout: &RasterLayout<'_, '_>) -> Result<TextTaskArtifacts, String> {
    let spec = layout.spec;
    let pixel = spec.pixel_size_mm;
    let grid_w = ((layout.width / pixel) - 1e-9).ceil().max(1.0) as usize;
    let grid_h = ((layout.height / pixel) - 1e-9).ceil().max(1.0) as usize;
    if grid_w.saturating_mul(grid_h) > MAX_IMAGE_PIXELS {
        return Err(format!(
            "转图片后约 {grid_w} × {grid_h} 像素，太大了；请缩小尺寸、加大线距或改用矢量"
        ));
    }
    // 网格与版面左下角对齐：第 r 行（自上而下）像素中心在版面 y = height − (grid_h − r − 0.5)·pixel
    let center_y = |r: usize| layout.height - (grid_h - r) as f64 * pixel + pixel / 2.0;
    let mut bitmap = vec![vec![255_u8; grid_w]; grid_h];
    let mut fill_rings: Vec<Vec<laser_contracts::geometry::Point>> = layout
        .text
        .iter()
        .map(|ring| {
            let mut ring = ring.clone();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            ring
        })
        .collect();
    let mut strokes: Vec<Vec<laser_contracts::geometry::Point>> = Vec::new();
    for item in layout.placed {
        let points = (layout.to_layout)(item.geometry.points());
        if item.shape.fill {
            let mut ring = points;
            ring.pop();
            fill_rings.push(ring);
        } else {
            strokes.push(points);
        }
    }
    for (r, row) in bitmap.iter_mut().enumerate() {
        for (x0, x1) in nonzero_spans(&fill_rings, center_y(r)) {
            // 像素中心落在区间里就出光
            let first = ((x0 / pixel) - 0.5).ceil().max(0.0) as usize;
            let last = ((x1 / pixel) - 0.5).floor();
            if last < 0.0 {
                continue;
            }
            for cell in row
                .iter_mut()
                .take((last as usize + 1).min(grid_w))
                .skip(first)
            {
                *cell = 0;
            }
        }
    }
    let half = IMAGE_STROKE_MIN_MM.max(2.0 * pixel) / 2.0;
    for line in &strokes {
        for seg in line.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len2 = dx * dx + dy * dy;
            let col = |x: f64| ((x / pixel) - 0.5).clamp(0.0, (grid_w - 1) as f64);
            let (c0, c1) = (
                col(a.0.min(b.0) - half).floor() as usize,
                col(a.0.max(b.0) + half).ceil() as usize,
            );
            for (r, row) in bitmap.iter_mut().enumerate() {
                let y = center_y(r);
                if y < a.1.min(b.1) - half || y > a.1.max(b.1) + half {
                    continue;
                }
                for (c, cell) in row.iter_mut().enumerate().take(c1 + 1).skip(c0) {
                    let x = (c as f64 + 0.5) * pixel;
                    let t = if len2 > 0.0 {
                        (((x - a.0) * dx + (y - a.1) * dy) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let (px, py) = (a.0 + t * dx, a.1 + t * dy);
                    if (x - px).hypot(y - py) <= half {
                        *cell = 0;
                    }
                }
            }
        }
    }
    if bitmap.iter().flatten().all(|v| *v != 0) {
        return Err("转图片后没有要出光的像素（图形太细？），请加粗或改用矢量".into());
    }
    let overscan = spec.scan.overscan_mm;
    let bidirectional = spec.scan.bidirectional;
    let scan_offset = if bidirectional {
        spec.scan.scan_offset_mm
    } else {
        0.0
    };
    if scan_offset.abs() / 2.0 > overscan + 1e-9 {
        return Err(format!(
            "转图片时扫描偏移的一半（{:.2} mm）不能超过过扫描（{overscan:.2} mm），请加大过扫描",
            scan_offset.abs() / 2.0
        ));
    }
    let plan = crate::raster::raster_plan_with_direction(&bitmap, "horizontal", "scanline")
        .map_err(|e| e.to_string())?;
    let written = gcode::write_raster(
        &plan,
        grid_w,
        grid_h,
        &gcode::RasterWriterOptions {
            pixel_size_mm: pixel,
            overscan_mm: overscan,
            work_area_width_mm: grid_w as f64 * pixel + 2.0 * overscan,
            work_area_height_mm: grid_h as f64 * pixel,
            power: spec.power,
            feed_rate: spec.feed_rate,
            laser_mode: "M4",
            snake_scan: bidirectional,
            material: None,
            safety_note: None,
            power_scale: None,
            dot_width_mm: 0.0,
            scan_offset_mm: scan_offset,
        },
    )?;
    // 位图坐标：x 右移一个过扫描，y 与版面（y 向上）相同
    let (start_x, start_y) = (layout.start.0 + overscan, layout.start.1);
    let gcode = place_at_head(&written, start_x, start_y)?;
    let mut preview = RgbaImage::from_pixel(
        grid_w as u32,
        grid_h as u32,
        image::Rgba([255, 255, 255, 255]),
    );
    for (r, row) in bitmap.iter().enumerate() {
        for (c, value) in row.iter().enumerate() {
            if *value == 0 {
                preview.put_pixel(c as u32, r as u32, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
    let estimate = gcode::estimate_time(&gcode, 3000.0);
    Ok(TextTaskArtifacts {
        gcode,
        preview,
        width_mm: layout.width,
        height_mm: layout.height,
        glyphs: layout.glyphs,
        path_optimization: PathOptimizationStats {
            strategy: "raster_scanline".into(),
            travel_distance_before: 0.0,
            travel_distance_after: 0.0,
            travel_reduction_ratio: 0.0,
            path_count: 0,
        },
        estimate,
        font_size_mm: layout.font_size_mm,
        fill: TextFill::Fill,
        origin: spec.origin,
        frame: layout.frame,
        fill_options: Some(vector_plan::FillOptions {
            spacing: pixel,
            rule: vector_plan::FillRule::NonZero,
            angle_deg: 0.0,
            cross_hatch: false,
            scan_offset_mm: scan_offset,
            ..spec.scan
        }),
        removed_duplicates: 0,
        cut_report: None,
        weld: layout.weld,
        shapes: layout.placed.len(),
        raster: Some(ImageRaster {
            grid_width: grid_w,
            grid_height: grid_h,
            pixel_size_mm: pixel,
        }),
    })
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
            shapes: &[],
            border: None,
            text_box: None,
            weld: true,
            image: false,
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

    // ---------- 2026-11-17 技术书 r44：焊接、基础图形与边框 ----------

    /// G-code 里出光（S > 0 的 G1）的总长度。
    fn burn_len(gcode: &str) -> f64 {
        let (mut x, mut y, mut s, mut total) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0);
        for line in gcode.lines() {
            let word = |c: char| {
                line.split_whitespace()
                    .find(|w| w.starts_with(c))
                    .and_then(|w| w[1..].parse::<f64>().ok())
            };
            if line.starts_with("G92") {
                (x, y) = (word('X').unwrap_or(x), word('Y').unwrap_or(y));
                continue;
            }
            if !(line.starts_with("G0") || line.starts_with("G1")) {
                continue;
            }
            s = word('S').unwrap_or(s);
            let (nx, ny) = (word('X').unwrap_or(x), word('Y').unwrap_or(y));
            if line.starts_with("G1") && s > 0.0 {
                total += (nx - x).hypot(ny - y);
            }
            (x, y) = (nx, ny);
        }
        total
    }

    #[test]
    fn weld_merges_overlapping_contours_and_keeps_separate_ones() {
        let square =
            |x: f64, y: f64| vec![(x, y), (x + 10.0, y), (x + 10.0, y + 10.0), (x, y + 10.0)];
        // 两个交叠的方块 + 一个分开的方块
        let paths = vec![square(0.0, 0.0), square(5.0, 5.0), square(30.0, 0.0)];
        let (welded, report) = weld_contours(&paths).unwrap();
        assert_eq!(
            report,
            WeldReport {
                before: 3,
                after: 2
            }
        );
        let perimeters: Vec<f64> = welded
            .iter()
            .map(|ring| {
                ring.windows(2)
                    .map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1))
                    .sum()
            })
            .collect();
        // 交叠两块合并后的外框周长 = 2 × 40（交叠处的内线没了），分开的那块不变
        assert!(
            perimeters.iter().any(|p| (p - 60.0).abs() < 1e-6),
            "{perimeters:?}"
        );
        assert!(
            perimeters.iter().any(|p| (p - 40.0).abs() < 1e-6),
            "{perimeters:?}"
        );
        // 内孔（反向轮廓）保留
        let hole = vec![(2.0, 2.0), (2.0, 8.0), (8.0, 8.0), (8.0, 2.0)];
        let (with_hole, report) = weld_contours(&vec![square(0.0, 0.0), hole]).unwrap();
        assert_eq!(report.after, 2);
        assert_eq!(with_hole.len(), 2);
    }

    #[test]
    fn welding_text_without_overlaps_keeps_the_same_strokes() {
        let welded = TextTaskEngine::generate(&spec("AB")).unwrap();
        let raw = TextTaskEngine::generate(&TextTaskSpec {
            weld: false,
            ..spec("AB")
        })
        .unwrap();
        assert!(welded.weld.is_some() && raw.weld.is_none());
        let (a, b) = (burn_len(&welded.gcode), burn_len(&raw.gcode));
        assert!(
            (a - b).abs() < 1e-3 * b,
            "不交叠的字焊接前后出光长度相同：{a} vs {b}"
        );
        assert_eq!(welded.frame, raw.frame);
    }

    fn coaster_shapes() -> Vec<shapes::Shape> {
        vec![shapes::Shape {
            kind: shapes::ShapeKind::Star {
                points: 5,
                inner_ratio: shapes::regular_star_ratio(5),
            },
            x_mm: 0.0,
            y_mm: 30.0,
            width_mm: 10.0,
            height_mm: 10.0,
            rotation_deg: 0.0,
            fill: true,
        }]
    }

    #[test]
    fn coaster_with_double_circle_border_text_and_a_star_stays_on_the_board() {
        let star = coaster_shapes();
        let job = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(90.0),
            board_height_mm: Some(90.0),
            auto_size: true,
            fill: TextFill::Fill,
            shapes: &star,
            border: Some(shapes::Border {
                shape: shapes::BorderShape::Circle,
                double: true,
                inset_mm: 3.0,
                gap_mm: 1.5,
                corner_radius_mm: None,
            }),
            text_box: Some(TextBox {
                x_mm: 0.0,
                y_mm: -5.0,
                width_mm: 40.0,
                height_mm: 30.0,
            }),
            ..spec("AB")
        })
        .unwrap();
        assert_eq!(job.shapes, 3, "星星 + 两圈边框");
        let e = extents(&job.gcode);
        // 激光头在木牌中心：外圈直径 84，范围关于起点对称（星星在上面、不超过外圈）
        assert!(
            (e.min_x + 42.0).abs() < 0.05 && (e.max_x - 42.0).abs() < 0.05,
            "{e:?}"
        );
        assert!(
            (e.min_y + 42.0).abs() < 0.05 && (e.max_y - 42.0).abs() < 0.05,
            "{e:?}"
        );
        assert!(
            job.font_size_mm > 10.0,
            "文字按 text_box 自动放大：{}",
            job.font_size_mm
        );
        // 外圈直径 84：版面宽高就是外圈
        assert!((job.width_mm - 84.0).abs() < 0.05 && (job.height_mm - 84.0).abs() < 0.05);
        // 星星填满：有大量扫描出光段
        assert!(job.gcode.lines().filter(|l| l.contains(" S420")).count() > 100);

        // 左下角起点：版面在木牌内、不往负方向越界
        let corner = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(90.0),
            board_height_mm: Some(90.0),
            auto_size: true,
            origin: TextOrigin::LowerLeft,
            shapes: &star,
            text_box: Some(TextBox {
                x_mm: 0.0,
                y_mm: -5.0,
                width_mm: 40.0,
                height_mm: 30.0,
            }),
            ..spec("AB")
        })
        .unwrap();
        let e = extents(&corner.gcode);
        assert!(
            e.min_x >= -1e-6 && e.min_y >= -1e-6 && e.max_x <= 90.0 && e.max_y <= 90.0,
            "{e:?}"
        );
    }

    #[test]
    fn shapes_only_jobs_and_layout_errors() {
        let heart = [shapes::Shape {
            kind: shapes::ShapeKind::Heart,
            x_mm: 0.0,
            y_mm: 0.0,
            width_mm: 30.0,
            height_mm: 26.0,
            rotation_deg: 0.0,
            fill: false,
        }];
        let job = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(40.0),
            board_height_mm: Some(40.0),
            shapes: &heart,
            ..spec("")
        })
        .unwrap();
        assert_eq!(job.glyphs, 0);
        assert!(job.weld.is_none());
        let e = extents(&job.gcode);
        assert!((e.max_x - e.min_x - 30.0).abs() < 0.05, "{e:?}");

        // 超出木牌
        let far = [shapes::Shape {
            x_mm: 15.0,
            ..heart[0]
        }];
        let err = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(40.0),
            board_height_mm: Some(40.0),
            shapes: &far,
            ..spec("")
        })
        .unwrap_err();
        assert!(err.contains("超出了木牌"), "{err}");
        // 与文字重叠
        let err = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(80.0),
            board_height_mm: Some(40.0),
            auto_size: true,
            shapes: &heart,
            ..spec("AB")
        })
        .unwrap_err();
        assert!(err.contains("重叠"), "{err}");
        // 只描边的外框把文字围在里面不算重叠
        let frame = [shapes::Shape {
            kind: shapes::ShapeKind::Rect {
                corner_radius_mm: 2.0,
            },
            x_mm: 0.0,
            y_mm: 0.0,
            width_mm: 76.0,
            height_mm: 36.0,
            rotation_deg: 0.0,
            fill: false,
        }];
        TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(80.0),
            board_height_mm: Some(40.0),
            font_size_mm: 10.0,
            shapes: &frame,
            ..spec("AB")
        })
        .unwrap();
        // 文字和图形都没有
        assert!(TextTaskEngine::generate(&spec("  ")).is_err());
        // 边框里放不下指定字号
        let err = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(40.0),
            board_height_mm: Some(40.0),
            font_size_mm: 30.0,
            border: Some(shapes::Border {
                shape: shapes::BorderShape::Rect,
                double: false,
                inset_mm: 3.0,
                gap_mm: 1.5,
                corner_radius_mm: None,
            }),
            ..spec("ABAB")
        })
        .unwrap_err();
        assert!(err.contains("边框里面"), "{err}");
    }

    // ---------- 2026-11-17 技术书 r45：转图片 ----------

    /// 出光（S > 0 的 G1）走过的 x / y 范围，相对激光头起点（`G92` 声明的位置）。
    fn burn_extents(gcode: &str) -> (f64, f64, f64, f64) {
        let (mut x, mut y, mut s) = (0.0_f64, 0.0_f64, 0.0_f64);
        let (mut hx, mut hy) = (0.0_f64, 0.0_f64);
        let mut e = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for line in gcode.lines() {
            let word = |c: char| {
                line.split_whitespace()
                    .find(|w| w.starts_with(c))
                    .and_then(|w| w[1..].parse::<f64>().ok())
            };
            if line.starts_with("G92") {
                (x, y) = (word('X').unwrap_or(x), word('Y').unwrap_or(y));
                (hx, hy) = (x, y);
                continue;
            }
            if !(line.starts_with("G0") || line.starts_with("G1")) {
                continue;
            }
            s = word('S').unwrap_or(s);
            let (nx, ny) = (word('X').unwrap_or(x), word('Y').unwrap_or(y));
            if line.starts_with("G1") && s > 0.0 {
                for (px, py) in [(x, y), (nx, ny)] {
                    e = (e.0.min(px), e.1.min(py), e.2.max(px), e.3.max(py));
                }
            }
            (x, y) = (nx, ny);
        }
        (e.0 - hx, e.1 - hy, e.2 - hx, e.3 - hy)
    }

    #[test]
    fn image_route_scans_the_same_layout_as_vector() {
        let board = |image: bool, origin: TextOrigin| TextTaskSpec {
            board_width_mm: Some(60.0),
            board_height_mm: Some(30.0),
            auto_size: true,
            origin,
            image,
            ..spec("AB")
        };
        let vector = TextTaskEngine::generate(&board(false, TextOrigin::Center)).unwrap();
        let image = TextTaskEngine::generate(&board(true, TextOrigin::Center)).unwrap();
        assert!(image.gcode.contains("; Raster mode"), "转图片走位图扫描");
        assert!(vector.raster.is_none());
        let raster = image.raster.unwrap();
        assert_eq!(raster.pixel_size_mm, 0.1);
        assert_eq!(image.fill, TextFill::Fill, "转图片时文字一律实心");
        // 同一版面：文字区域、字号与起点相同，出光范围相差不超过一个像素
        assert_eq!(image.frame, vector.frame);
        assert_eq!(image.font_size_mm, vector.font_size_mm);
        let (v, i) = (burn_extents(&vector.gcode), burn_extents(&image.gcode));
        for (a, b) in [(v.0, i.0), (v.1, i.1), (v.2, i.2), (v.3, i.3)] {
            assert!((a - b).abs() <= 0.11, "矢量 {v:?} 与位图 {i:?}");
        }
        // 左下角起点：不往负方向越界
        let corner = TextTaskEngine::generate(&board(true, TextOrigin::LowerLeft)).unwrap();
        let e = extents(&corner.gcode);
        assert!(
            e.min_x >= -1e-6 && e.min_y >= -1e-6 && e.max_x <= 60.0 + 1e-6,
            "{e:?}"
        );

        // 描边图形与直线按线宽渲染；切割不能转图片
        let ring = [shapes::Shape {
            kind: shapes::ShapeKind::Circle,
            x_mm: 0.0,
            y_mm: 0.0,
            width_mm: 20.0,
            height_mm: 20.0,
            rotation_deg: 0.0,
            fill: false,
        }];
        let outline = TextTaskEngine::generate(&TextTaskSpec {
            board_width_mm: Some(30.0),
            board_height_mm: Some(30.0),
            shapes: &ring,
            image: true,
            ..spec("")
        })
        .unwrap();
        let (x0, _, x1, _) = burn_extents(&outline.gcode);
        assert!((x1 - x0 - 20.0).abs() < 0.5, "{x0} {x1}");
        let err = TextTaskEngine::generate(&TextTaskSpec {
            image: true,
            cut: cut::CutOptions {
                kerf_mm: 0.1,
                ..cut::CutOptions::default()
            },
            ..spec("A")
        })
        .unwrap_err();
        assert!(err.contains("切割不能转图片"), "{err}");
    }

    #[test]
    fn image_route_applies_bidirectional_scan_offset() {
        let with_offset = |offset: f64| {
            TextTaskEngine::generate(&TextTaskSpec {
                image: true,
                scan: vector_plan::FillOptions {
                    overscan_mm: 1.0,
                    scan_offset_mm: offset,
                    ..spec("A").scan
                },
                ..spec("A")
            })
        };
        let plain = with_offset(0.0).unwrap();
        let shifted = with_offset(0.2).unwrap();
        // 第一行（正向）往回挪 0.1 mm：起点 x 小 0.1
        let first_burn = |gcode: &str| {
            gcode
                .lines()
                .find(|l| l.starts_with("G1 ") && !l.contains(" S0"))
                .map(str::to_owned)
                .unwrap()
        };
        assert_ne!(first_burn(&plain.gcode), first_burn(&shifted.gcode));
        assert!(with_offset(3.0).is_err(), "偏移的一半超过过扫描要拒绝");
    }
}
