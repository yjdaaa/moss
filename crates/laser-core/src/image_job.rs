//! 图片素材 → 雕刻位图（2026-11-06 修复：成品尺寸、机器幅面、透明背景与灰度）。
//!
//! 旧实现固定 0.1 mm/像素且用 128 硬阈值：2000 像素宽的照片会生成 200 mm 宽的任务（翼宿 V1 只有
//! 100 × 100 mm），照片糊成色块，透明 PNG 的背景被当成黑色整片出光。这里统一处理：
//! 1. 透明像素按白色（不出光）合成；
//! 2. 成品尺寸按用户给的宽 / 高（mm）确定，同时给宽高时按比例放进这个框；都没给时按材料像素间距
//!    换算原图尺寸，超出机器幅面就按比例缩到幅面内并标记 `auto_fitted`；显式尺寸超出幅面直接拒绝；
//! 3. 按材料像素间距重采样到雕刻网格；
//! 4. 照片类（中间灰度多）用 Floyd–Steinberg 抖动，线稿类用 Otsu 阈值；也可显式指定。

use image::{DynamicImage, GrayImage, Luma};

use crate::image::{otsu_threshold, resize, resize_area};
use crate::raster::{DitherAlgorithm, dither_f64};

/// 像素间距允许范围（mm）。小于 0.05 mm 超出 5 W 二极管光斑能力，大于 1 mm 已无图像意义。
pub const MIN_PIXEL_SIZE_MM: f64 = 0.05;
pub const MAX_PIXEL_SIZE_MM: f64 = 1.0;
/// 自动判定照片的阈值：中间灰度（40..=215）像素占比超过此值按照片抖动。
const PHOTO_MIDTONE_RATIO: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneMode {
    Auto,
    Dither,
    Threshold,
}

impl ToneMode {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None | Some("auto") => Ok(Self::Auto),
            Some("dither") => Ok(Self::Dither),
            Some("threshold") => Ok(Self::Threshold),
            Some(other) => Err(format!(
                "image_mode 只能是 auto / dither / threshold，收到 {other}"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dither => "dither",
            Self::Threshold => "threshold",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageRasterOptions {
    pub pixel_size_mm: f64,
    pub width_mm: Option<f64>,
    pub height_mm: Option<f64>,
    pub work_area_width_mm: f64,
    pub work_area_height_mm: f64,
    /// 横向扫描每侧过扫描距离（mm）：栅格写出器把图像右移这么多、两端再各多走这么多，
    /// 所以图像宽度最多只能是 `幅面宽 - 2 × 过扫描`。
    pub scan_overscan_mm: f64,
    pub mode: ToneMode,
    /// 仅阈值模式使用；`None` 时用 Otsu 自动阈值
    pub threshold: Option<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRaster {
    /// 0 = 出光，255 = 空白；行优先
    pub bitmap: Vec<Vec<u8>>,
    pub grid_width: u32,
    pub grid_height: u32,
    pub width_mm: f64,
    pub height_mm: f64,
    /// 未指定尺寸、原图换算后超出机器幅面而被等比缩小
    pub auto_fitted: bool,
    /// 实际使用的灰度处理（`auto` 已解析为 dither / threshold）
    pub mode: ToneMode,
    pub threshold: Option<u8>,
}

impl PreparedRaster {
    /// 实际将雕刻的黑白图，用于预览（黑 = 出光）。
    pub fn preview(&self) -> GrayImage {
        GrayImage::from_fn(self.grid_width, self.grid_height, |x, y| {
            Luma([self.bitmap[y as usize][x as usize]])
        })
    }
}

/// 透明像素按白色（不出光）合成后转灰度。
pub fn flatten_on_white(image: &DynamicImage) -> GrayImage {
    let rgba = image.to_rgba8();
    GrayImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let alpha = f64::from(a) / 255.0;
        let luma = 0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b);
        Luma([(luma * alpha + 255.0 * (1.0 - alpha))
            .round()
            .clamp(0.0, 255.0) as u8])
    })
}

fn positive(value: Option<f64>, name: &str) -> Result<Option<f64>, String> {
    match value {
        None => Ok(None),
        Some(v) if v.is_finite() && v > 0.0 => Ok(Some(v)),
        Some(_) => Err(format!("{name} 必须是大于 0 的数字")),
    }
}

/// 计算成品尺寸（mm）与是否自动缩放；显式尺寸超出幅面返回错误。
fn target_size(
    source_w: u32,
    source_h: u32,
    options: &ImageRasterOptions,
) -> Result<(f64, f64, bool), String> {
    let overscan = options.scan_overscan_mm;
    if !(overscan.is_finite() && overscan >= 0.0) {
        return Err("过扫描距离必须是不小于 0 的数字".into());
    }
    let (area_w, area_h) = (
        options.work_area_width_mm - 2.0 * overscan,
        options.work_area_height_mm,
    );
    if !(area_w.is_finite() && area_w > 0.0 && area_h.is_finite() && area_h > 0.0) {
        return Err("机器幅面必须大于两侧过扫描余量".into());
    }
    let aspect = f64::from(source_h) / f64::from(source_w);
    let contain = |box_w: f64, box_h: f64| {
        if box_h / box_w >= aspect {
            (box_w, box_w * aspect)
        } else {
            (box_h / aspect, box_h)
        }
    };
    let explicit = match (
        positive(options.width_mm, "width_mm")?,
        positive(options.height_mm, "height_mm")?,
    ) {
        (Some(w), Some(h)) => Some(contain(w, h)),
        (Some(w), None) => Some((w, w * aspect)),
        (None, Some(h)) => Some((h / aspect, h)),
        (None, None) => None,
    };
    if let Some((w, h)) = explicit {
        if w > area_w + 1e-6 || h > area_h + 1e-6 {
            return Err(format!(
                "成品尺寸 {w:.1} × {h:.1} mm 超出机器幅面 {:.0} × {:.0} mm 可雕刻范围（宽度需留出两侧各 {overscan:.1} mm 过扫描，最大 {area_w:.1} × {area_h:.1} mm），请减小宽或高",
                options.work_area_width_mm, options.work_area_height_mm
            ));
        }
        return Ok((w, h, false));
    }
    let natural = (
        f64::from(source_w) * options.pixel_size_mm,
        f64::from(source_h) * options.pixel_size_mm,
    );
    if natural.0 <= area_w + 1e-6 && natural.1 <= area_h + 1e-6 {
        Ok((natural.0, natural.1, false))
    } else {
        let (w, h) = contain(area_w, area_h);
        Ok((w, h, true))
    }
}

fn midtone_ratio(image: &GrayImage) -> f64 {
    let total = u64::from(image.width()) * u64::from(image.height());
    if total == 0 {
        return 0.0;
    }
    let mid = image
        .pixels()
        .filter(|p| (40..=215).contains(&p.0[0]))
        .count() as f64;
    mid / total as f64
}

pub fn prepare_raster(
    source: &GrayImage,
    options: &ImageRasterOptions,
) -> Result<PreparedRaster, String> {
    let (source_w, source_h) = source.dimensions();
    if source_w == 0 || source_h == 0 {
        return Err("图片尺寸为 0".into());
    }
    let pixel = options.pixel_size_mm;
    if !(pixel.is_finite() && (MIN_PIXEL_SIZE_MM..=MAX_PIXEL_SIZE_MM).contains(&pixel)) {
        return Err(format!(
            "pixel_size_mm 必须在 {MIN_PIXEL_SIZE_MM} ~ {MAX_PIXEL_SIZE_MM} mm 之间"
        ));
    }
    let (width_mm, height_mm, auto_fitted) = target_size(source_w, source_h, options)?;
    let grid_w = ((width_mm / pixel).round() as u32).max(1);
    let grid_h = ((height_mm / pixel).round() as u32).max(1);
    let resampled = if grid_w <= source_w && grid_h <= source_h {
        resize_area(source, grid_w, grid_h)
    } else {
        resize(source, grid_w, grid_h)
    };
    let mode = match options.mode {
        ToneMode::Auto if midtone_ratio(&resampled) > PHOTO_MIDTONE_RATIO => ToneMode::Dither,
        ToneMode::Auto => ToneMode::Threshold,
        explicit => explicit,
    };
    let rows: Vec<Vec<f64>> = (0..grid_h)
        .map(|y| {
            (0..grid_w)
                .map(|x| f64::from(resampled.get_pixel(x, y).0[0]))
                .collect()
        })
        .collect();
    let (bitmap, threshold) = if mode == ToneMode::Dither {
        (
            dither_f64(&rows, DitherAlgorithm::FloydSteinberg, 128.0),
            None,
        )
    } else {
        let value = options
            .threshold
            .unwrap_or_else(|| otsu_threshold(&resampled));
        let bitmap = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|v| if *v <= f64::from(value) { 0 } else { 255 })
                    .collect()
            })
            .collect();
        (bitmap, Some(value))
    };
    Ok(PreparedRaster {
        bitmap,
        grid_width: grid_w,
        grid_height: grid_h,
        width_mm: f64::from(grid_w) * pixel,
        height_mm: f64::from(grid_h) * pixel,
        auto_fitted,
        mode,
        threshold,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn options() -> ImageRasterOptions {
        ImageRasterOptions {
            pixel_size_mm: 0.1,
            width_mm: None,
            height_mm: None,
            work_area_width_mm: 100.0,
            work_area_height_mm: 100.0,
            scan_overscan_mm: 0.0,
            mode: ToneMode::Auto,
            threshold: None,
        }
    }

    fn black_square_on_white(size: u32) -> GrayImage {
        GrayImage::from_fn(size, size, |x, y| {
            let inside =
                (size / 4..size * 3 / 4).contains(&x) && (size / 4..size * 3 / 4).contains(&y);
            Luma([if inside { 0 } else { 255 }])
        })
    }

    #[test]
    fn oversized_source_without_size_is_fitted_into_the_machine() {
        // 2000 × 1000 px 按 0.1 mm 是 200 × 100 mm，必须缩到 100 × 50 mm
        let source = GrayImage::from_pixel(2000, 1000, Luma([255]));
        let raster = prepare_raster(&source, &options()).unwrap();
        assert!(raster.auto_fitted);
        assert_eq!((raster.grid_width, raster.grid_height), (1000, 500));
        assert!((raster.width_mm - 100.0).abs() < 1e-9 && (raster.height_mm - 50.0).abs() < 1e-9);
    }

    #[test]
    fn small_source_keeps_its_natural_size() {
        let raster = prepare_raster(&black_square_on_white(200), &options()).unwrap();
        assert!(!raster.auto_fitted);
        assert_eq!((raster.grid_width, raster.grid_height), (200, 200));
    }

    #[test]
    fn explicit_size_scales_with_aspect_and_rejects_oversize() {
        let source = GrayImage::from_pixel(400, 200, Luma([255]));
        let one_side = prepare_raster(
            &source,
            &ImageRasterOptions {
                width_mm: Some(60.0),
                ..options()
            },
        )
        .unwrap();
        assert_eq!((one_side.grid_width, one_side.grid_height), (600, 300));
        // 宽高都给：按比例放进 80 × 30 的框 → 60 × 30
        let boxed = prepare_raster(
            &source,
            &ImageRasterOptions {
                width_mm: Some(80.0),
                height_mm: Some(30.0),
                ..options()
            },
        )
        .unwrap();
        assert_eq!((boxed.grid_width, boxed.grid_height), (600, 300));
        let err = prepare_raster(
            &source,
            &ImageRasterOptions {
                width_mm: Some(150.0),
                ..options()
            },
        )
        .unwrap_err();
        assert!(err.contains("超出机器幅面"), "{err}");
        assert!(
            prepare_raster(
                &source,
                &ImageRasterOptions {
                    height_mm: Some(-1.0),
                    ..options()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn overscan_margin_shrinks_the_usable_width() {
        let source = GrayImage::from_pixel(2000, 500, Luma([255]));
        let fitted = prepare_raster(
            &source,
            &ImageRasterOptions {
                scan_overscan_mm: 1.0,
                ..options()
            },
        )
        .unwrap();
        assert!(fitted.auto_fitted);
        assert_eq!(
            fitted.grid_width, 980,
            "100 mm 幅面减两侧 1 mm 过扫描 → 98 mm"
        );
        let err = prepare_raster(
            &source,
            &ImageRasterOptions {
                scan_overscan_mm: 1.0,
                width_mm: Some(99.0),
                ..options()
            },
        )
        .unwrap_err();
        assert!(err.contains("过扫描"), "{err}");
        assert!(
            prepare_raster(
                &source,
                &ImageRasterOptions {
                    scan_overscan_mm: 1.0,
                    width_mm: Some(98.0),
                    ..options()
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn material_pixel_size_sets_the_grid() {
        let source = GrayImage::from_pixel(500, 500, Luma([255]));
        let raster = prepare_raster(
            &source,
            &ImageRasterOptions {
                pixel_size_mm: 0.2,
                width_mm: Some(50.0),
                ..options()
            },
        )
        .unwrap();
        assert_eq!((raster.grid_width, raster.grid_height), (250, 250));
        assert!(
            prepare_raster(
                &source,
                &ImageRasterOptions {
                    pixel_size_mm: 0.01,
                    ..options()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn transparent_background_does_not_burn() {
        let mut rgba = RgbaImage::from_pixel(50, 50, Rgba([0, 0, 0, 0]));
        for x in 20..30 {
            for y in 20..30 {
                rgba.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let gray = flatten_on_white(&DynamicImage::ImageRgba8(rgba));
        assert_eq!(gray.get_pixel(0, 0).0[0], 255);
        assert_eq!(gray.get_pixel(25, 25).0[0], 0);
        let raster = prepare_raster(&gray, &options()).unwrap();
        let burned: usize = raster.bitmap.iter().flatten().filter(|v| **v == 0).count();
        assert_eq!(burned, 100);
    }

    #[test]
    fn photos_are_dithered_and_line_art_is_thresholded() {
        let gradient = GrayImage::from_fn(200, 100, |x, _| Luma([(x * 255 / 199) as u8]));
        let photo = prepare_raster(&gradient, &options()).unwrap();
        assert_eq!(photo.mode, ToneMode::Dither);
        // 抖动保留灰度：左半明显比右半黑
        let dark = |range: std::ops::Range<usize>| {
            photo
                .bitmap
                .iter()
                .map(|row| row[range.clone()].iter().filter(|v| **v == 0).count())
                .sum::<usize>()
        };
        assert!(dark(0..100) > dark(100..200) * 3);
        assert!(dark(100..200) > 0, "右半应仍有少量点，而不是硬阈值的全白");

        let line_art = prepare_raster(&black_square_on_white(200), &options()).unwrap();
        assert_eq!(line_art.mode, ToneMode::Threshold);
        assert!(line_art.threshold.is_some());

        let forced = prepare_raster(
            &gradient,
            &ImageRasterOptions {
                mode: ToneMode::Threshold,
                threshold: Some(100),
                ..options()
            },
        )
        .unwrap();
        assert_eq!(forced.threshold, Some(100));
        assert!(ToneMode::parse(Some("sketch")).is_err());
    }
}
