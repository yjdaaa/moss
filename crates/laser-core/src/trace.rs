use std::collections::{BTreeSet, HashSet, VecDeque};

use image::{GrayImage, Luma};
use imageproc::contours::{BorderType, find_contours};
pub use laser_contracts::geometry::{Contour, Point, TraceGeometry, TraceOptions};

use crate::safety::SafetyError;

const MAX_PIXELS: usize = 4_000_000;
pub const TRACE_OUTPUT_POINT_BUDGET: usize = 2500;

fn dimensions(bitmap: &[Vec<u8>]) -> Result<(usize, usize), SafetyError> {
    let width = bitmap.first().map_or(0, Vec::len);
    if bitmap.iter().any(|row| row.len() != width) {
        return Err(SafetyError("Trace bitmap rows must have equal width"));
    }
    if width.saturating_mul(bitmap.len()) > MAX_PIXELS {
        return Err(SafetyError("Trace image exceeds the working pixel limit"));
    }
    Ok((width, bitmap.len()))
}

/// Suzuki-Abe: nonzero foreground, 8-connectivity, pixel-center coordinates.
/// Padding ensures that foreground touching any image edge has an outer border.
pub fn trace_binary(bitmap: &[Vec<u8>], tolerance: f64) -> Result<Vec<Contour>, SafetyError> {
    validate_tolerance(tolerance)?;
    let (width, height) = dimensions(bitmap)?;
    if width == 0 || height == 0 {
        return Ok(Vec::new());
    }
    let mut padded = GrayImage::new(width as u32 + 2, height as u32 + 2);
    for (y, row) in bitmap.iter().enumerate() {
        for (x, value) in row.iter().enumerate() {
            padded.put_pixel(x as u32 + 1, y as u32 + 1, Luma([*value]));
        }
    }
    let mut result = Vec::new();
    for contour in find_contours::<i32>(&padded) {
        let mut points: Vec<_> = contour
            .points
            .iter()
            .map(|p| (f64::from(p.x - 1), f64::from(p.y - 1)))
            .collect();
        if points.first() != points.last() {
            points.push(points[0]);
        }
        let simplified = simplify_closed(&points, tolerance)?;
        // Do not delete a topological node if simplification collapses its geometry.
        let points = if simplified.len() >= 4 {
            simplified
        } else {
            points
        };
        result.push(Contour {
            area: signed_area(&points),
            points,
            parent: contour.parent,
            child: None,
            is_hole: contour.border_type == BorderType::Hole,
        });
    }
    for index in 0..result.len() {
        if let Some(parent) = result[index].parent {
            result[parent].child.get_or_insert(index);
        }
    }
    Ok(result)
}

/// Python AI compatibility: dark (<180) foreground, 4-connected components,
/// pixel-cell edges, physical scaling, simplification and per-component budget.
pub fn trace_grayscale(
    bitmap: &[Vec<u8>],
    options: &TraceOptions,
) -> Result<TraceGeometry, SafetyError> {
    let (width, height) = dimensions(bitmap)?;
    if width == 0 || height == 0 {
        return Err(SafetyError("source dimensions must be positive"));
    }
    let (target_w, target_h) = target_dimensions(width, height, options)?;
    let (scale_x, scale_y) = (target_w / width as f64, target_h / height as f64);
    let mut seen = HashSet::new();
    let mut components = Vec::new();
    for y in 0..height {
        for x in 0..width {
            if bitmap[y][x] >= 180 || seen.contains(&(x, y)) {
                continue;
            }
            let group = component(bitmap, x, y, &mut seen);
            if group.len() >= 4 {
                components.push(group);
            }
        }
    }
    // Equal-area components are ordered by scan discovery; Python set order is
    // not a contract, so the differential comparator normalizes only such ties.
    components.sort_by_key(|group| std::cmp::Reverse(group.len()));
    let budget = (TRACE_OUTPUT_POINT_BUDGET / components.len().max(1)).max(25);
    let tolerance = (scale_x.max(scale_y) * 0.35).max(target_w.max(target_h) * 0.001)
        * options.simplify_factor.max(0.25);
    let mut paths = Vec::new();
    let mut raw_contour_count = 0;
    let mut raw_point_count = 0;
    for group in components {
        let mut raw = edge_loops(&group)?;
        raw.sort_by(|a, b| signed_area(b).abs().total_cmp(&signed_area(a).abs()));
        for pixel_path in raw {
            let path: Vec<_> = pixel_path
                .iter()
                .map(|p| (p.0 * scale_x, p.1 * scale_y))
                .collect();
            raw_contour_count += 1;
            raw_point_count += path.len();
            let simplified = simplify_closed(&path, tolerance)?;
            let sampled = downsample_closed_path(&simplified, budget)?;
            if sampled.len() >= 4 {
                paths.push(sampled);
            }
        }
    }
    Ok(TraceGeometry {
        width_mm: target_w,
        height_mm: target_h,
        contour_count: paths.len(),
        point_count: paths.iter().map(Vec::len).sum(),
        paths,
        raw_contour_count,
        raw_point_count,
    })
}

fn target_dimensions(w: usize, h: usize, o: &TraceOptions) -> Result<(f64, f64), SafetyError> {
    if [
        o.size_mm,
        o.work_area_width_mm,
        o.work_area_height_mm,
        o.simplify_factor,
    ]
    .iter()
    .any(|v| !v.is_finite() || *v <= 0.0)
        || o.width_mm.is_some_and(|v| !v.is_finite() || v < 0.0)
        || o.height_mm.is_some_and(|v| !v.is_finite() || v < 0.0)
    {
        return Err(SafetyError(
            "Trace dimensions and simplify factor must be finite and positive",
        ));
    }
    let (w, h) = (w as f64, h as f64);
    let width = o.width_mm.filter(|v| *v > 0.0);
    let height = o.height_mm.filter(|v| *v > 0.0);
    let (tw, th) = match (width, height) {
        (Some(tw), Some(th)) if o.lock_aspect_ratio => {
            let scale = (tw / w).min(th / h);
            (w * scale, h * scale)
        }
        (Some(tw), Some(th)) => (tw, th),
        (Some(tw), None) => (tw, tw / (w / h)),
        (None, Some(th)) => (th * (w / h), th),
        (None, None) => {
            let scale = o.size_mm / w.max(h);
            (w * scale, h * scale)
        }
    };
    let fit = 1.0_f64
        .min(o.work_area_width_mm / tw)
        .min(o.work_area_height_mm / th);
    let result = (tw * fit, th * fit);
    if !result.0.is_finite() || !result.1.is_finite() || result.0 <= 0.0 || result.1 <= 0.0 {
        return Err(SafetyError("Trace dimensions overflowed"));
    }
    Ok(result)
}

fn component(
    bitmap: &[Vec<u8>],
    sx: usize,
    sy: usize,
    seen: &mut HashSet<(usize, usize)>,
) -> Vec<(usize, usize)> {
    let mut queue = VecDeque::from([(sx, sy)]);
    let mut out = Vec::new();
    seen.insert((sx, sy));
    while let Some((x, y)) = queue.pop_front() {
        out.push((x, y));
        for (nx, ny) in [
            (x.wrapping_sub(1), y),
            (x + 1, y),
            (x, y.wrapping_sub(1)),
            (x, y + 1),
        ] {
            if ny < bitmap.len()
                && nx < bitmap[ny].len()
                && bitmap[ny][nx] < 180
                && seen.insert((nx, ny))
            {
                queue.push_back((nx, ny));
            }
        }
    }
    out
}

// Store vertices as (y,x) to reproduce Python's deterministic edge ordering.
fn edge_loops(group: &[(usize, usize)]) -> Result<Vec<Vec<Point>>, SafetyError> {
    let members: HashSet<_> = group.iter().copied().collect();
    let mut edges = BTreeSet::new();
    for &(x, y) in group {
        if !members.contains(&(x, y.wrapping_sub(1))) {
            edges.insert(((y, x), (y, x + 1)));
        }
        if !members.contains(&(x + 1, y)) {
            edges.insert(((y, x + 1), (y + 1, x + 1)));
        }
        if !members.contains(&(x, y + 1)) {
            edges.insert(((y + 1, x + 1), (y + 1, x)));
        }
        if !members.contains(&(x.wrapping_sub(1), y)) {
            edges.insert(((y + 1, x), (y, x)));
        }
    }
    let mut loops = Vec::new();
    while let Some((start, end)) = edges.pop_first() {
        let mut path = vec![
            (start.1 as f64, start.0 as f64),
            (end.1 as f64, end.0 as f64),
        ];
        let mut current = end;
        while current != start {
            let next = edges
                .range((current, (0, 0))..)
                .next()
                .copied()
                .filter(|(from, _)| *from == current)
                .ok_or(SafetyError("Trace boundary is not closed"))?;
            edges.remove(&next);
            current = next.1;
            path.push((current.1 as f64, current.0 as f64));
        }
        if path.len() >= 4 {
            loops.push(path);
        }
    }
    Ok(loops)
}

fn validate_tolerance(value: f64) -> Result<(), SafetyError> {
    if !value.is_finite() || value < 0.0 {
        Err(SafetyError(
            "Simplification tolerance must be finite and nonnegative",
        ))
    } else {
        Ok(())
    }
}

pub fn validate_points(points: &[Point]) -> Result<(), SafetyError> {
    if points.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        Err(SafetyError(
            "Geometry cannot contain non-finite coordinates",
        ))
    } else {
        Ok(())
    }
}

/// Iterative RDP, preserving Python's first maximum tie and exact threshold.
pub fn douglas_peucker(points: &[Point], tolerance: f64) -> Result<Vec<Point>, SafetyError> {
    validate_tolerance(tolerance)?;
    validate_points(points)?;
    if points.len() <= 2 {
        return Ok(points.to_vec());
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        if end - start < 2 {
            continue;
        }
        let (mut split, mut maximum) = (start + 1, -1.0);
        for index in start + 1..end {
            let distance = perpendicular(points[index], points[start], points[end]);
            if !distance.is_finite() {
                return Err(SafetyError("Geometry distance overflowed"));
            }
            if distance > maximum {
                maximum = distance;
                split = index;
            }
        }
        if maximum > tolerance {
            keep[split] = true;
            stack.push((split, end));
            stack.push((start, split));
        }
    }
    Ok(points
        .iter()
        .zip(keep)
        .filter_map(|(p, keep)| keep.then_some(*p))
        .collect())
}

pub fn simplify_closed(points: &[Point], tolerance: f64) -> Result<Vec<Point>, SafetyError> {
    validate_tolerance(tolerance)?;
    validate_points(points)?;
    if points.len() <= 4 {
        return Ok(points.to_vec());
    }
    let closed = points_equal(points[0], points[points.len() - 1]);
    let input = if closed {
        &points[..points.len() - 1]
    } else {
        points
    };
    let mut simplified = douglas_peucker(input, tolerance)?;
    if closed && simplified.first() != simplified.last() {
        simplified.push(simplified[0]);
    }
    Ok(simplified)
}

pub fn downsample_closed_path(points: &[Point], budget: usize) -> Result<Vec<Point>, SafetyError> {
    if budget < 4 {
        return Err(SafetyError(
            "Closed path point budget must be at least four",
        ));
    }
    if points.len() <= budget {
        return Ok(points.to_vec());
    }
    let closed = points.len() >= 4 && points_equal(points[0], points[points.len() - 1]);
    let source = if closed {
        &points[..points.len() - 1]
    } else {
        points
    };
    let count = if closed { budget - 1 } else { budget };
    let step = source.len() as f64 / count as f64;
    let mut sampled: Vec<_> = (0..count)
        .map(|i| source[(i as f64 * step) as usize])
        .collect();
    if closed {
        sampled.push(sampled[0]);
    }
    Ok(sampled)
}

fn points_equal(a: Point, b: Point) -> bool {
    (a.0 - b.0).abs() <= 1e-9 && (a.1 - b.1).abs() <= 1e-9
}

fn perpendicular(p: Point, a: Point, b: Point) -> f64 {
    if points_equal(a, b) {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    ((b.1 - a.1) * p.0 - (b.0 - a.0) * p.1 + b.0 * a.1 - b.1 * a.0).abs()
        / ((b.1 - a.1).powi(2) + (b.0 - a.0).powi(2)).sqrt()
}

fn signed_area(points: &[Point]) -> f64 {
    points
        .windows(2)
        .map(|p| p[0].0 * p[1].1 - p[1].0 * p[0].1)
        .sum::<f64>()
        / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suzuki_has_hole_and_island_hierarchy_even_at_edges() {
        let mut image = vec![vec![255; 13]; 13];
        for row in &mut image[2..11] {
            row[2..11].fill(0);
        }
        for row in &mut image[4..9] {
            row[4..9].fill(255);
        }
        let contours = trace_binary(&image, 0.1).unwrap();
        assert_eq!(contours.len(), 3);
        assert_eq!(
            contours.iter().map(|c| c.parent).collect::<Vec<_>>(),
            vec![None, Some(0), Some(1)]
        );
        assert_eq!(contours[0].child, Some(1));
        assert!(contours[1].is_hole);
        assert!(!contours[2].is_hole);
    }

    #[test]
    fn invalid_inputs_are_errors_not_panics() {
        assert!(trace_binary(&[vec![1], vec![]], 0.1).is_err());
        assert!(trace_binary(&[], f64::NAN).is_err());
        assert!(douglas_peucker(&[(f64::NAN, 0.0)], 0.1).is_err());
        assert!(douglas_peucker(&[], -0.1).is_err());
        assert!(trace_binary(&[], 0.0).unwrap().is_empty());
    }

    #[test]
    fn grayscale_preserves_cell_extents_and_filters_small_components() {
        let options = TraceOptions {
            size_mm: 4.0,
            ..Default::default()
        };
        let result = trace_grayscale(&[vec![0, 0], vec![0, 0]], &options).unwrap();
        assert_eq!(result.contour_count, 1);
        assert!(result.paths[0].contains(&(4.0, 4.0)));
        assert_eq!(result.raw_point_count, 9);
        assert_eq!(
            trace_grayscale(&[vec![0, 0], vec![0, 255]], &options)
                .unwrap()
                .contour_count,
            0
        );
    }

    #[test]
    fn rdp_threshold_and_budget_are_exact() {
        assert_eq!(
            douglas_peucker(&[(0.0, 0.0), (1.0, 0.1), (2.0, 0.0)], 0.1)
                .unwrap()
                .len(),
            2
        );
        let mut points: Vec<_> = (0..80).map(|i| (i as f64, (i % 2) as f64)).collect();
        points.push(points[0]);
        let sampled = downsample_closed_path(&points, 25).unwrap();
        assert_eq!(sampled.len(), 25);
        assert_eq!(sampled.first(), sampled.last());
    }
}
