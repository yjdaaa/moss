#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherAlgorithm {
    Threshold,
    FloydSteinberg,
    Atkinson,
    SierraLite,
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
    let kernel: &[(isize, isize, f64)] = match algorithm {
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
        DitherAlgorithm::Threshold => unreachable!(),
    };
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
}
