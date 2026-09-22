use crate::safety::SafetyError;
use crate::trace::validate_points;
pub use laser_contracts::geometry::{FillResult, FillStrategy, Paths, Point};

const EPS: f64 = 1e-9;
const MAX_FILL_SEGMENTS: usize = 20_000;

pub fn apply_vector_fill(
    paths: &[Vec<Point>],
    spacing: f64,
    strategy: FillStrategy,
) -> Result<FillResult, SafetyError> {
    if !spacing.is_finite() || spacing <= 0.0 {
        return Err(SafetyError("Fill spacing must be finite and positive"));
    }
    for path in paths {
        validate_points(path)?;
    }
    let mut result = FillResult {
        paths: paths.to_vec(),
        fill_strategy: if strategy == FillStrategy::Auto {
            FillStrategy::None
        } else {
            strategy
        },
        fill_spacing_mm: spacing,
        fill_segment_count: 0,
        fill_warnings: Vec::new(),
    };
    if strategy == FillStrategy::Auto {
        result
            .fill_warnings
            .push("fill_strategy=auto resolved to none for MVP safety.".to_owned());
    }
    if matches!(strategy, FillStrategy::None | FillStrategy::Auto) {
        return Ok(result);
    }
    for (index, path) in paths.iter().enumerate() {
        if !simple_closed(path) {
            result.fill_warnings.push(format!(
                "Path {index} is not a supported simple closed polygon; skipped fill."
            ));
            continue;
        }
        let Some(segments) = hatch(path, spacing)? else {
            result.fill_warnings.push(format!(
                "Path {index} has unsupported scanline intersections; skipped fill."
            ));
            continue;
        };
        let filled = if strategy == FillStrategy::Hatch {
            segments
                .into_iter()
                .map(|(a, b)| vec![a, b])
                .collect::<Paths>()
        } else {
            let mut joined = Vec::new();
            for (index, (a, b)) in segments.into_iter().enumerate() {
                if index % 2 == 0 {
                    joined.extend([a, b]);
                } else {
                    joined.extend([b, a]);
                }
            }
            if joined.is_empty() {
                Vec::new()
            } else {
                vec![joined]
            }
        };
        result.fill_segment_count += filled
            .iter()
            .map(|p| p.len().saturating_sub(1))
            .sum::<usize>();
        if result.fill_segment_count > MAX_FILL_SEGMENTS {
            return Err(SafetyError("Fill output exceeds the segment limit"));
        }
        result.paths.extend(filled);
    }
    Ok(result)
}

pub fn fill_polygon(
    path: &[Point],
    spacing: f64,
    strategy: FillStrategy,
) -> Result<Paths, SafetyError> {
    let result = apply_vector_fill(&[path.to_vec()], spacing, strategy)?;
    Ok(result.paths.into_iter().skip(1).collect())
}

fn hatch(path: &[Point], spacing: f64) -> Result<Option<Vec<(Point, Point)>>, SafetyError> {
    let polygon = &path[..path.len() - 1];
    let low = polygon.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let high = polygon
        .iter()
        .map(|p| p.1)
        .fold(f64::NEG_INFINITY, f64::max);
    if !((high - low) / spacing).is_finite()
        || (high - low) / spacing > MAX_FILL_SEGMENTS as f64 + 1.0
    {
        return Err(SafetyError("Fill output exceeds the segment limit"));
    }
    let mut segments = Vec::new();
    let mut y = low + spacing;
    while y < high - EPS {
        let mut intersections = Vec::new();
        for (a, b) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
            if (a.1 - b.1).abs() <= EPS {
                continue;
            }
            if a.1.min(b.1) <= y && y < a.1.max(b.1) {
                let x = a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0);
                if !x.is_finite() {
                    return Err(SafetyError("Fill intersection overflowed"));
                }
                intersections.push(x);
            }
        }
        intersections.sort_by(f64::total_cmp);
        if intersections.len() != 2 {
            return Ok(None);
        }
        if intersections[1] - intersections[0] > EPS {
            segments.push(((intersections[0], y), (intersections[1], y)));
        }
        let next = y + spacing;
        if next <= y {
            return Err(SafetyError(
                "Fill spacing is too small for these coordinates",
            ));
        }
        y = next;
    }
    Ok(Some(segments))
}

fn simple_closed(path: &[Point]) -> bool {
    if path.len() < 4 || squared(path[0], path[path.len() - 1]) > EPS {
        return false;
    }
    let polygon = &path[..path.len() - 1];
    let mut unique = Vec::new();
    for point in polygon {
        if !unique.contains(point) {
            unique.push(*point);
        }
        if unique.len() == 3 {
            break;
        }
    }
    if unique.len() < 3 {
        return false;
    }
    for i in 0..polygon.len() {
        for j in i + 1..polygon.len() {
            if j == i + 1 || (i == 0 && j == polygon.len() - 1) {
                continue;
            }
            let (a, b, c, d) = (
                polygon[i],
                polygon[(i + 1) % polygon.len()],
                polygon[j],
                polygon[(j + 1) % polygon.len()],
            );
            if orientation(a, b, c) * orientation(a, b, d) < 0.0
                && orientation(c, d, a) * orientation(c, d, b) < 0.0
            {
                return false;
            }
        }
    }
    true
}
fn squared(a: Point, b: Point) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}
fn orientation(a: Point, b: Point, c: Point) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn square() -> Vec<Point> {
        vec![
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ]
    }
    #[test]
    fn hatch_zigzag_and_auto_match_python_rules() {
        let hatch = apply_vector_fill(&[square()], 2.0, FillStrategy::Hatch).unwrap();
        assert_eq!(hatch.fill_segment_count, 4);
        assert_eq!(hatch.paths[1], vec![(0.0, 2.0), (10.0, 2.0)]);
        let zigzag = apply_vector_fill(&[square()], 2.0, FillStrategy::Zigzag).unwrap();
        assert_eq!(zigzag.fill_segment_count, 7);
        assert_eq!(&zigzag.paths[1][2..4], &[(10.0, 4.0), (0.0, 4.0)]);
        let auto = apply_vector_fill(&[square()], 2.0, FillStrategy::Auto).unwrap();
        assert_eq!(auto.paths, vec![square()]);
        assert_eq!(auto.fill_strategy, FillStrategy::None);
        assert_eq!(auto.fill_warnings.len(), 1);
    }
    #[test]
    fn rejects_invalid_numeric_input_and_skips_self_intersections() {
        for spacing in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-20] {
            assert!(apply_vector_fill(&[square()], spacing, FillStrategy::Hatch).is_err());
        }
        assert!(apply_vector_fill(&[vec![(f64::NAN, 0.0)]], 1.0, FillStrategy::None).is_err());
        let bow = vec![
            (0.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (10.0, 0.0),
            (0.0, 0.0),
        ];
        let result = apply_vector_fill(&[bow.clone()], 2.0, FillStrategy::Hatch).unwrap();
        assert_eq!(result.paths, vec![bow]);
        assert_eq!(result.fill_warnings.len(), 1);
    }
    #[test]
    fn closure_uses_squared_distance_tolerance() {
        let mut path = square();
        *path.last_mut().unwrap() = (1e-5, 0.0);
        assert!(simple_closed(&path));
        *path.last_mut().unwrap() = (1e-3, 0.0);
        assert!(!simple_closed(&path));
    }
}
