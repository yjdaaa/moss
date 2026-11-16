#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherAlgorithm {
    Threshold,
    FloydSteinberg,
    Atkinson,
    SierraLite,
    /// Jarvis–Judice–Ninke（LightBurn 对照片的推荐算法，2026-11-16）
    JarvisJudiceNinke,
    Stucki,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RasterSegment {
    pub axis: &'static str,
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RasterPlan {
    pub horizontal: Vec<RasterSegment>,
    pub vertical: Vec<RasterSegment>,
    pub selected_axis: &'static str,
    pub output_strategy: &'static str,
}

pub fn raster_plan(bitmap: &[Vec<u8>], output_strategy: &str) -> Result<RasterPlan, &'static str> {
    raster_plan_with_direction(bitmap, "auto", output_strategy)
}

pub fn raster_plan_with_direction(
    bitmap: &[Vec<u8>],
    scan_direction: &str,
    output_strategy: &str,
) -> Result<RasterPlan, &'static str> {
    let width = bitmap.first().map_or(0, Vec::len);
    if bitmap.iter().any(|row| row.len() != width) {
        return Err("raster bitmap is ragged");
    }
    let horizontal = runs_horizontal(bitmap);
    let vertical = runs_vertical(bitmap);
    let selected_axis = match scan_direction {
        "horizontal" => "horizontal",
        "vertical" => "vertical",
        "auto" => {
            if !vertical.is_empty() && vertical.len() < horizontal.len() {
                "vertical"
            } else {
                "horizontal"
            }
        }
        _ => return Err("unknown raster scan direction"),
    };
    let output_strategy = match output_strategy {
        "segment" => "segment",
        "scanline" => "scanline",
        "auto" => {
            let scanlines = if selected_axis == "vertical" {
                vertical
                    .iter()
                    .map(|s| s.line)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            } else {
                horizontal
                    .iter()
                    .map(|s| s.line)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
            };
            let count = if selected_axis == "vertical" {
                vertical.len()
            } else {
                horizontal.len()
            };
            if scanlines > 0 && count >= scanlines * 2 {
                "scanline"
            } else {
                "segment"
            }
        }
        _ => return Err("unknown raster output strategy"),
    };
    Ok(RasterPlan {
        horizontal,
        vertical,
        selected_axis,
        output_strategy,
    })
}

fn runs_horizontal(bitmap: &[Vec<u8>]) -> Vec<RasterSegment> {
    let mut result = Vec::new();
    for (line, row) in bitmap.iter().enumerate() {
        let mut start = None;
        for (index, value) in row.iter().copied().chain(std::iter::once(255)).enumerate() {
            if value == 0 && start.is_none() {
                start = Some(index);
            }
            if value != 0
                && let Some(begin) = start.take()
            {
                result.push(RasterSegment {
                    axis: "horizontal",
                    line,
                    start: begin,
                    end: index,
                });
            }
        }
    }
    result
}

fn runs_vertical(bitmap: &[Vec<u8>]) -> Vec<RasterSegment> {
    let width = bitmap.first().map_or(0, Vec::len);
    let mut result = Vec::new();
    for line in 0..width {
        let mut start = None;
        for (index, value) in bitmap
            .iter()
            .map(|row| row[line])
            .chain(std::iter::once(255))
            .enumerate()
        {
            if value == 0 && start.is_none() {
                start = Some(index);
            }
            if value != 0
                && let Some(begin) = start.take()
            {
                result.push(RasterSegment {
                    axis: "vertical",
                    line,
                    start: begin,
                    end: index,
                });
            }
        }
    }
    result
}

pub fn dither(input: &[Vec<f32>], algorithm: DitherAlgorithm, threshold: f32) -> Vec<Vec<u8>> {
    let input: Vec<Vec<f64>> = input
        .iter()
        .map(|r| r.iter().map(|v| *v as f64).collect())
        .collect();
    dither_f64(&input, algorithm, threshold as f64)
}

pub fn dither_f64(input: &[Vec<f64>], algorithm: DitherAlgorithm, threshold: f64) -> Vec<Vec<u8>> {
    if algorithm == DitherAlgorithm::Threshold {
        return input
            .iter()
            .map(|row| {
                row.iter()
                    .map(|v| if *v <= threshold { 0 } else { 255 })
                    .collect()
            })
            .collect();
    }
    let mut work = input.to_vec();
    let h = work.len();
    let w = work.first().map_or(0, Vec::len);
    let kernel = diffusion_kernel(algorithm);
    let mut output = vec![vec![0; w]; h];
    for y in 0..h {
        for x in 0..w {
            let old = work[y][x];
            let new = if old < 128.0 { 0 } else { 255 };
            output[y][x] = new;
            let error = old - new as f64;
            for (dx, dy, weight) in kernel {
                let nx = x as isize + dx;
                let ny = y as isize + dy;
                if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                    let value = &mut work[ny as usize][nx as usize];
                    *value = (*value + error * *weight).clamp(0.0, 255.0);
                }
            }
        }
    }
    output
}

/// 误差扩散核 `(dx, dy, 权重)`；`Threshold` 没有扩散核。
/// Floyd–Steinberg、Sierra Lite、Jarvis、Stucki 的权重和为 1；Atkinson 按原算法只扩散 6/8 的误差。
pub fn diffusion_kernel(algorithm: DitherAlgorithm) -> &'static [(isize, isize, f64)] {
    match algorithm {
        DitherAlgorithm::Threshold => &[],
        DitherAlgorithm::FloydSteinberg => &[
            (1, 0, 7.0 / 16.0),
            (-1, 1, 3.0 / 16.0),
            (0, 1, 5.0 / 16.0),
            (1, 1, 1.0 / 16.0),
        ],
        DitherAlgorithm::Atkinson => &[
            (1, 0, 1.0 / 8.0),
            (2, 0, 1.0 / 8.0),
            (-1, 1, 1.0 / 8.0),
            (0, 1, 1.0 / 8.0),
            (1, 1, 1.0 / 8.0),
            (0, 2, 1.0 / 8.0),
        ],
        DitherAlgorithm::SierraLite => &[(1, 0, 2.0 / 4.0), (-1, 1, 1.0 / 4.0), (0, 1, 1.0 / 4.0)],
        DitherAlgorithm::JarvisJudiceNinke => &[
            (1, 0, 7.0 / 48.0),
            (2, 0, 5.0 / 48.0),
            (-2, 1, 3.0 / 48.0),
            (-1, 1, 5.0 / 48.0),
            (0, 1, 7.0 / 48.0),
            (1, 1, 5.0 / 48.0),
            (2, 1, 3.0 / 48.0),
            (-2, 2, 1.0 / 48.0),
            (-1, 2, 3.0 / 48.0),
            (0, 2, 5.0 / 48.0),
            (1, 2, 3.0 / 48.0),
            (2, 2, 1.0 / 48.0),
        ],
        DitherAlgorithm::Stucki => &[
            (1, 0, 8.0 / 42.0),
            (2, 0, 4.0 / 42.0),
            (-2, 1, 2.0 / 42.0),
            (-1, 1, 4.0 / 42.0),
            (0, 1, 8.0 / 42.0),
            (1, 1, 4.0 / 42.0),
            (2, 1, 2.0 / 42.0),
            (-2, 2, 1.0 / 42.0),
            (-1, 2, 2.0 / 42.0),
            (0, 2, 4.0 / 42.0),
            (1, 2, 2.0 / 42.0),
            (2, 2, 1.0 / 42.0),
        ],
    }
}

/// 8×8 Bayer 有序抖动（LightBurn Ordered）：规则网格点阵，适合大面积纯色。输入 0 = 黑。
pub fn ordered_dither(input: &[Vec<f64>]) -> Vec<Vec<u8>> {
    const BAYER: [[u8; 8]; 8] = [
        [0, 32, 8, 40, 2, 34, 10, 42],
        [48, 16, 56, 24, 50, 18, 58, 26],
        [12, 44, 4, 36, 14, 46, 6, 38],
        [60, 28, 52, 20, 62, 30, 54, 22],
        [3, 35, 11, 43, 1, 33, 9, 41],
        [51, 19, 59, 27, 49, 17, 57, 25],
        [15, 47, 7, 39, 13, 45, 5, 37],
        [63, 31, 55, 23, 61, 29, 53, 21],
    ];
    input
        .iter()
        .enumerate()
        .map(|(y, row)| {
            row.iter()
                .enumerate()
                .map(|(x, v)| {
                    let darkness = 1.0 - (v / 255.0).clamp(0.0, 1.0);
                    let level = (f64::from(BAYER[y % 8][x % 8]) + 0.5) / 64.0;
                    if darkness > level { 0 } else { 255 }
                })
                .collect()
        })
        .collect()
}

/// 半径 `r` 的圆与边长 `cell`、同中心的正方形交集占正方形面积的比例（0..=1，随 r 单调增）。
/// 网点从圆点长到铺满整格，面积比例与灰度线性对应。
fn spot_area_fraction(r: f64, cell: f64) -> f64 {
    let s = cell / 2.0;
    if r <= 0.0 {
        return 0.0;
    }
    if r <= s {
        return std::f64::consts::PI * r * r / (cell * cell);
    }
    if r >= s * std::f64::consts::SQRT_2 {
        return 1.0;
    }
    let segment = r * r * (s / r).acos() - s * (r * r - s * s).sqrt();
    ((std::f64::consts::PI * r * r - 4.0 * segment) / (cell * cell)).clamp(0.0, 1.0)
}

/// 网点加网。`cell_px` 为网格边长（像素），`angle_deg` 为网格角度。
/// `average_cells = true`（Halftone）：每格先求平均灰度，再画一个面积与之成比例的圆点；
/// `false`（Newsprint）：逐像素与网点阈值比较，网点随局部明暗变大变小。输入 0 = 黑，输出 0 = 出光。
pub fn halftone_screen(
    input: &[Vec<f64>],
    cell_px: f64,
    angle_deg: f64,
    average_cells: bool,
) -> Vec<Vec<u8>> {
    let h = input.len();
    let w = input.first().map_or(0, Vec::len);
    let mut output = vec![vec![255_u8; w]; h];
    if !(cell_px.is_finite() && cell_px > 0.0 && angle_deg.is_finite()) {
        return output;
    }
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    let locate = |x: usize, y: usize| {
        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
        let u = px * cos + py * sin;
        let v = -px * sin + py * cos;
        let (i, j) = ((u / cell_px).floor(), (v / cell_px).floor());
        let du = u - (i + 0.5) * cell_px;
        let dv = v - (j + 0.5) * cell_px;
        ((i as i64, j as i64), (du * du + dv * dv).sqrt())
    };
    let darkness = |value: f64| 1.0 - (value / 255.0).clamp(0.0, 1.0);
    let mut cells: std::collections::HashMap<(i64, i64), (f64, u32)> =
        std::collections::HashMap::new();
    if average_cells {
        for (y, row) in input.iter().enumerate() {
            for (x, value) in row.iter().enumerate() {
                let entry = cells.entry(locate(x, y).0).or_insert((0.0, 0));
                entry.0 += darkness(*value);
                entry.1 += 1;
            }
        }
    }
    for (y, row) in input.iter().enumerate() {
        for (x, value) in row.iter().enumerate() {
            let (key, distance) = locate(x, y);
            let d = if average_cells {
                cells.get(&key).map_or(0.0, |(sum, n)| sum / f64::from(*n))
            } else {
                darkness(*value)
            };
            if d >= 1.0 - 1e-9 || spot_area_fraction(distance, cell_px) < d {
                output[y][x] = 0;
            }
        }
    }
    output
}

/// 素描（LightBurn Sketch）：Sobel 高通，只在明暗交界处出光；平坦区域不出光。
/// 门限取梯度幅值的 Otsu 阈值，且不低于 `SKETCH_MIN_EDGE`（避免噪声满版出光）。
pub fn sketch_edges(input: &[Vec<f64>]) -> Vec<Vec<u8>> {
    const SKETCH_MIN_EDGE: u8 = 32;
    let h = input.len();
    let w = input.first().map_or(0, Vec::len);
    if w == 0 {
        return vec![Vec::new(); h];
    }
    let at = |x: isize, y: isize| -> f64 {
        let cx = x.clamp(0, w as isize - 1) as usize;
        let cy = y.clamp(0, h as isize - 1) as usize;
        input[cy][cx]
    };
    let magnitude: Vec<Vec<u8>> = (0..h as isize)
        .map(|y| {
            (0..w as isize)
                .map(|x| {
                    let gx = at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1)
                        - at(x - 1, y - 1)
                        - 2.0 * at(x - 1, y)
                        - at(x - 1, y + 1);
                    let gy = at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1)
                        - at(x - 1, y - 1)
                        - 2.0 * at(x, y - 1)
                        - at(x + 1, y - 1);
                    // 单边阶跃 0→255 的 Sobel 幅值为 4 × 255，归一化到 0..=255
                    ((gx * gx + gy * gy).sqrt() / 4.0).round().clamp(0.0, 255.0) as u8
                })
                .collect()
        })
        .collect();
    if magnitude.iter().flatten().all(|m| *m < SKETCH_MIN_EDGE) {
        return vec![vec![255; w]; h];
    }
    let threshold = otsu_threshold(&magnitude).max(SKETCH_MIN_EDGE);
    magnitude
        .iter()
        .map(|row| {
            row.iter()
                .map(|m| if *m > threshold { 0 } else { 255 })
                .collect()
        })
        .collect()
}

/// 灰度量化：0 = 纯白（不出光），`1..levels` 越大越黑。`levels` 至少为 2。
pub fn quantize_levels(input: &[Vec<f64>], levels: u8) -> Vec<Vec<u8>> {
    let top = f64::from(levels.max(2) - 1);
    input
        .iter()
        .map(|row| {
            row.iter()
                .map(|v| ((1.0 - (v / 255.0).clamp(0.0, 1.0)) * top).round() as u8)
                .collect()
        })
        .collect()
}

/// 每个灰度级对应的 S 值：级 0 为 0（不出光），级 `1..levels` 在 `min_power..=max_power` 间线性分布，
/// 最黑一级恰为 `max_power`。返回长度为 `levels` 的单调不减序列（要求 `min_power <= max_power`）。
pub fn grayscale_power_table(levels: u8, min_power: i32, max_power: i32) -> Vec<i32> {
    let levels = levels.max(2);
    let top = f64::from(levels - 1);
    (0..levels)
        .map(|k| {
            if k == 0 {
                0
            } else {
                let t = f64::from(k) / top;
                (f64::from(min_power) + f64::from(max_power - min_power) * t).round() as i32
            }
        })
        .collect()
}

pub fn otsu_threshold(input: &[Vec<u8>]) -> u8 {
    let mut histogram = [0_u64; 256];
    let total = input.iter().flatten().count() as f64;
    for value in input.iter().flatten() {
        histogram[*value as usize] += 1;
    }
    let sum: f64 = histogram
        .iter()
        .enumerate()
        .map(|(i, n)| i as f64 * *n as f64)
        .sum();
    let mut weight_background = 0.0;
    let mut sum_background = 0.0;
    let mut best = (-1.0, 128_u8);
    for (threshold, &bin) in histogram.iter().enumerate() {
        weight_background += bin as f64;
        if weight_background == 0.0 || weight_background == total {
            continue;
        }
        sum_background += threshold as f64 * bin as f64;
        let mean_background = sum_background / weight_background;
        let mean_foreground = (sum - sum_background) / (total - weight_background);
        let variance = weight_background
            * (total - weight_background)
            * (mean_background - mean_foreground).powi(2);
        if variance > best.0 {
            best = (variance, threshold as u8);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_dither_algorithms_are_binary_and_deterministic() {
        let input = vec![vec![0.0, 64.0, 128.0, 255.0]; 4];
        for algorithm in [
            DitherAlgorithm::Threshold,
            DitherAlgorithm::FloydSteinberg,
            DitherAlgorithm::Atkinson,
            DitherAlgorithm::SierraLite,
            DitherAlgorithm::JarvisJudiceNinke,
            DitherAlgorithm::Stucki,
        ] {
            let first = dither(&input, algorithm, 128.0);
            assert_eq!(first, dither(&input, algorithm, 128.0));
            assert!(first.iter().flatten().all(|v| *v == 0 || *v == 255));
        }
    }
    #[test]
    fn otsu_separates_two_clusters() {
        let input = vec![vec![10, 10, 20, 240, 250, 250]];
        assert!((20..=240).contains(&otsu_threshold(&input)));
    }

    #[test]
    fn raster_runs_are_exclusive_and_strategy_is_deterministic() {
        let bitmap = vec![vec![255, 0, 0, 255], vec![255, 0, 255, 255]];
        let plan = raster_plan(&bitmap, "auto").unwrap();
        assert_eq!(plan.horizontal[0].start..plan.horizontal[0].end, 1..3);
        assert_eq!(plan.vertical[0].line, 1);
        assert_eq!(plan.vertical[0].start..plan.vertical[0].end, 0..2);
        assert_eq!(plan.selected_axis, "horizontal");
    }

    fn gradient(w: usize, h: usize) -> Vec<Vec<f64>> {
        (0..h)
            .map(|_| (0..w).map(|x| 255.0 * x as f64 / (w - 1) as f64).collect())
            .collect()
    }

    fn burned(bitmap: &[Vec<u8>], cols: std::ops::Range<usize>) -> usize {
        bitmap
            .iter()
            .map(|row| row[cols.clone()].iter().filter(|v| **v == 0).count())
            .sum()
    }

    #[test]
    fn diffusion_kernels_are_normalized_and_forward_only() {
        for algorithm in [
            DitherAlgorithm::FloydSteinberg,
            DitherAlgorithm::SierraLite,
            DitherAlgorithm::JarvisJudiceNinke,
            DitherAlgorithm::Stucki,
        ] {
            let kernel = diffusion_kernel(algorithm);
            let sum: f64 = kernel.iter().map(|(_, _, w)| w).sum();
            assert!((sum - 1.0).abs() < 1e-12, "{algorithm:?} 权重和 {sum}");
            // 只能扩散到尚未处理的像素：同一行右侧或下面的行
            assert!(kernel.iter().all(|(dx, dy, _)| *dy > 0 || *dx > 0));
        }
        assert_eq!(
            diffusion_kernel(DitherAlgorithm::JarvisJudiceNinke).len(),
            12
        );
        assert_eq!(diffusion_kernel(DitherAlgorithm::Stucki).len(), 12);
        assert!(diffusion_kernel(DitherAlgorithm::Threshold).is_empty());
    }

    #[test]
    fn jarvis_and_stucki_keep_mean_tone_and_follow_the_gradient() {
        let flat = vec![vec![128.0; 64]; 64];
        for algorithm in [DitherAlgorithm::JarvisJudiceNinke, DitherAlgorithm::Stucki] {
            let out = dither_f64(&flat, algorithm, 128.0);
            let ratio = burned(&out, 0..64) as f64 / (64.0 * 64.0);
            assert!(
                (0.4..0.6).contains(&ratio),
                "{algorithm:?} 出光占比 {ratio}"
            );
            let dark = dither_f64(&gradient(64, 16), algorithm, 128.0);
            assert!(burned(&dark, 0..32) > burned(&dark, 32..64) * 2);
        }
    }

    #[test]
    fn ordered_halftone_newsprint_and_sketch_are_binary_and_deterministic() {
        let input = gradient(60, 40);
        let run = || {
            [
                ordered_dither(&input),
                halftone_screen(&input, 6.0, 45.0, true),
                halftone_screen(&input, 4.0, 45.0, false),
                sketch_edges(&input),
            ]
        };
        let outputs = run();
        assert_eq!(outputs, run());
        for out in &outputs {
            assert_eq!((out.len(), out[0].len()), (40, 60));
            assert!(out.iter().flatten().all(|v| *v == 0 || *v == 255));
        }
    }

    #[test]
    fn halftone_dot_density_increases_with_darkness() {
        let mut previous = 0;
        for gray in [250.0, 200.0, 150.0, 100.0, 50.0, 0.0] {
            let flat = vec![vec![gray; 80]; 80];
            let count = burned(&halftone_screen(&flat, 8.0, 22.5, true), 0..80);
            assert!(
                count >= previous,
                "灰度 {gray} 出光 {count} < 更亮时 {previous}"
            );
            previous = count;
        }
        assert_eq!(previous, 80 * 80, "纯黑铺满");
        let white = vec![vec![255.0; 40]; 40];
        assert_eq!(burned(&halftone_screen(&white, 8.0, 45.0, true), 0..40), 0);
        // 面积比例接近灰度：50% 灰约一半出光
        let half = vec![vec![127.5; 96]; 96];
        let ratio = burned(&halftone_screen(&half, 8.0, 0.0, true), 0..96) as f64 / (96.0 * 96.0);
        assert!((0.4..0.6).contains(&ratio), "{ratio}");
        // 逐像素网点（Newsprint）同样左黑右白
        let news = halftone_screen(&gradient(80, 40), 4.0, 45.0, false);
        assert!(burned(&news, 0..40) > burned(&news, 40..80) * 2);
    }

    #[test]
    fn spot_area_fraction_is_monotone_and_reaches_full_cell() {
        let mut last = 0.0;
        for step in 0..=100 {
            let f = spot_area_fraction(step as f64 * 0.08, 8.0);
            assert!(f >= last - 1e-12);
            last = f;
        }
        assert!((last - 1.0).abs() < 1e-12);
    }

    #[test]
    fn sketch_burns_only_edges() {
        let flat = vec![vec![90.0; 30]; 30];
        assert_eq!(burned(&sketch_edges(&flat), 0..30), 0, "平坦区域不出光");
        let step: Vec<Vec<f64>> = (0..30)
            .map(|_| (0..30).map(|x| if x < 15 { 0.0 } else { 255.0 }).collect())
            .collect();
        let out = sketch_edges(&step);
        assert!(burned(&out, 13..17) >= 30, "交界处出光");
        assert_eq!(
            burned(&out, 0..12) + burned(&out, 18..30),
            0,
            "两侧大块不出光"
        );
    }

    #[test]
    fn grayscale_levels_are_bounded_and_power_is_monotone() {
        let q = quantize_levels(&gradient(256, 1), 16);
        assert!(q.iter().flatten().all(|v| *v < 16));
        assert_eq!(q[0][0], 15, "最黑为最高级");
        assert_eq!(q[0][255], 0, "纯白不出光");
        assert!(q[0].windows(2).all(|p| p[0] >= p[1]), "越亮级数越低");
        let table = grayscale_power_table(16, 100, 800);
        assert_eq!(table.len(), 16);
        assert_eq!((table[0], table[1], table[15]), (0, 147, 800));
        assert!(table.windows(2).all(|p| p[0] <= p[1]));
        assert!(table[1..].iter().all(|p| (100..=800).contains(p)));
        assert_eq!(grayscale_power_table(2, 0, 500), vec![0, 500]);
    }
}
