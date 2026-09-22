use crate::safety::SafetyError;
pub use laser_contracts::geometry::{PathOptimizationResult, PathOptimizationStats, Paths, Point};

pub const MINIMUM_TRAVEL_GAIN: f64 = 0.05;

fn distance(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}
fn closed(path: &[Point]) -> bool {
    path.len() >= 3 && distance(path[0], path[path.len() - 1]) <= 1e-9
}

// Python writer closes every path longer than two points, including zigzags.
fn writer_end(path: &[Point]) -> Point {
    if path.len() > 2 {
        path[0]
    } else {
        path[path.len() - 1]
    }
}

fn validate(paths: &[Vec<Point>]) -> Result<(), SafetyError> {
    if paths.iter().any(Vec::is_empty) {
        return Err(SafetyError("Path optimization cannot process empty paths"));
    }
    if paths
        .iter()
        .flatten()
        .any(|p| !p.0.is_finite() || !p.1.is_finite())
    {
        return Err(SafetyError(
            "Path optimization cannot process non-finite coordinates",
        ));
    }
    Ok(())
}

pub fn travel_distance(paths: &[Vec<Point>]) -> Result<f64, SafetyError> {
    validate(paths)?;
    let (mut current, mut total) = ((0.0, 0.0), 0.0);
    for path in paths {
        total += distance(current, path[0]);
        current = writer_end(path);
    }
    if !total.is_finite() {
        return Err(SafetyError(
            "Path optimization produced an invalid travel distance",
        ));
    }
    Ok(total)
}

pub fn optimize_with_threshold(
    paths: &[Vec<Point>],
    size_mm: f64,
) -> Result<PathOptimizationResult, SafetyError> {
    if paths.is_empty() {
        return Err(SafetyError(
            "Path optimization requires at least one usable path",
        ));
    }
    validate(paths)?;
    if !size_mm.is_finite() || size_mm <= 0.0 {
        return Err(SafetyError(
            "Path optimization size must be finite and positive",
        ));
    }
    let before = travel_distance(paths)?;
    let direction = direction_order(paths, size_mm);
    let nearest = nearest_neighbor(paths);
    let direction_distance = travel_distance(&direction)?;
    let nearest_distance = travel_distance(&nearest)?;
    let use_nearest = nearest_is_better(direction_distance, nearest_distance);
    let (selected, after, strategy) = if use_nearest {
        (nearest, nearest_distance, "nearest_neighbor")
    } else {
        (direction, direction_distance, "direction_order")
    };
    if geometry_points(paths) != geometry_points(&selected) {
        return Err(SafetyError(
            "Path optimization changed path geometry; refusing to generate G-code",
        ));
    }
    Ok(PathOptimizationResult {
        paths: selected,
        stats: PathOptimizationStats {
            strategy: strategy.to_owned(),
            travel_distance_before: before,
            travel_distance_after: after,
            travel_reduction_ratio: if before <= 0.0 {
                0.0
            } else {
                (before - after) / before
            },
            path_count: paths.len(),
        },
    })
}

fn nearest_is_better(direction: f64, nearest: f64) -> bool {
    direction > 0.0 && (direction - nearest) / direction >= MINIMUM_TRAVEL_GAIN
}

fn geometry_points(paths: &[Vec<Point>]) -> Vec<Point> {
    let mut points: Vec<_> = paths
        .iter()
        .flat_map(|p| if closed(p) { &p[..p.len() - 1] } else { &p[..] })
        .copied()
        .collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    points
}

fn nearest_neighbor(paths: &[Vec<Point>]) -> Paths {
    let mut remaining: Paths = paths.to_vec();
    let mut selected = Vec::with_capacity(paths.len());
    let mut current = (0.0, 0.0);
    while !remaining.is_empty() {
        let (mut best_index, mut best_reverse, mut best_start) = (0, false, 0);
        let mut best_distance = f64::INFINITY;
        for (index, path) in remaining.iter().enumerate() {
            let is_closed = closed(path);
            // Preserve Python's order: all forward rotations, then reversed ones.
            for reverse in [false, true] {
                let count = if is_closed { path.len() - 1 } else { 1 };
                for start in 0..count {
                    let candidate = if reverse {
                        path[path.len() - 1 - start]
                    } else {
                        path[start]
                    };
                    let candidate_distance = distance(current, candidate);
                    if candidate_distance < best_distance {
                        best_distance = candidate_distance;
                        best_index = index;
                        best_reverse = reverse;
                        best_start = start;
                    }
                }
            }
        }
        let mut path = remaining.remove(best_index);
        if best_reverse {
            path.reverse();
        }
        if closed(&path) {
            path.pop();
            path.rotate_left(best_start);
            path.push(path[0]);
        }
        current = writer_end(&path);
        selected.push(path);
    }
    selected
}

type Bounds = (f64, f64, f64, f64);
fn bounds(path: &[Point]) -> Bounds {
    path.iter().fold(
        (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ),
        |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)),
    )
}
fn cx(b: Bounds) -> f64 {
    (b.0 + b.2) / 2.0
}
fn cy(b: Bounds) -> f64 {
    (b.1 + b.3) / 2.0
}

fn direction_order(paths: &[Vec<Point>], size: f64) -> Paths {
    let mut indexed: Vec<_> = paths.iter().map(|p| (bounds(p), p)).collect();
    let average_height = indexed.iter().map(|(b, _)| b.3 - b.1).sum::<f64>() / paths.len() as f64;
    let tolerance = (size * 0.05).max(average_height).max(0.001);
    indexed.sort_by(|a, b| {
        cy(a.0)
            .total_cmp(&cy(b.0))
            .then(cx(a.0).total_cmp(&cx(b.0)))
    });
    let mut result = Vec::with_capacity(paths.len());
    let mut start = 0;
    for end in 1..=indexed.len() {
        if end == indexed.len() || (cy(indexed[end].0) - cy(indexed[end - 1].0)).abs() > tolerance {
            let mut row = indexed[start..end].to_vec();
            row.sort_by(|a, b| cx(a.0).total_cmp(&cx(b.0)));
            result.extend(row.into_iter().map(|(_, p)| p.clone()));
            start = end;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn travel_is_only_rapid_and_writer_closes_long_open_paths() {
        assert_eq!(
            travel_distance(&[vec![(3.0, 4.0), (300.0, 400.0)]]).unwrap(),
            5.0
        );
        assert_eq!(
            travel_distance(&[vec![(0.0, 0.0), (9.0, 0.0), (9.0, 9.0)], vec![(0.0, 0.0)]]).unwrap(),
            0.0
        );
    }
    #[test]
    fn closed_path_can_rotate_to_nearest_vertex() {
        let paths = vec![vec![(10.0, 10.0), (1.0, 0.0), (10.0, 0.0), (10.0, 10.0)]];
        let result = optimize_with_threshold(&paths, 20.0).unwrap();
        assert_eq!(result.paths[0][0], (1.0, 0.0));
        assert_eq!(geometry_points(&paths), geometry_points(&result.paths));
        assert_eq!(result.stats.strategy, "nearest_neighbor");
    }
    #[test]
    fn gain_boundary_is_five_percent_and_ties_keep_first() {
        assert!(nearest_is_better(100.0, 95.0));
        assert!(!nearest_is_better(100.0, 95.0001));
        let input = vec![vec![(1.0, 0.0), (2.0, 0.0)], vec![(-1.0, 0.0), (-2.0, 0.0)]];
        assert_eq!(nearest_neighbor(&input)[0], input[0]);
    }
    #[test]
    fn invalid_inputs_do_not_disappear() {
        assert!(optimize_with_threshold(&[], 10.0).is_err());
        assert!(optimize_with_threshold(&[vec![]], 10.0).is_err());
        assert!(optimize_with_threshold(&[vec![(f64::NAN, 0.0)]], 10.0).is_err());
        assert!(travel_distance(&[vec![(f64::MAX, f64::MAX)]]).is_err());
    }
}
