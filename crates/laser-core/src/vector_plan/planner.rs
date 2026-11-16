//! 路线规划：最近邻贪心 + 2-opt 排序、内部优先、尖角起刀与去重复线。

use super::{Unit, close_contour, distance, finite};
use crate::gcode::Step;
use crate::safety::SafetyError;
use laser_contracts::geometry::{PathOptimizationStats, Paths, Point};

/// 去重复线的距离容差（mm）。
pub const OVERLAP_TOLERANCE_MM: f64 = 0.01;

/// 2-opt 只在单元数不太多时做（O(n²) 每轮），再多只用贪心结果。
const TWO_OPT_MAX_UNITS: usize = 2_500;

const TWO_OPT_MAX_PASSES: usize = 40;

/// 拐角判定：前后两条边方向变化超过这个角度（度）算尖角。
const CORNER_TURN_DEG: f64 = 45.0;

/// 规划选项（对齐 LightBurn 优化设置，见模块说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanOptions {
    pub inside_first: bool,
    pub prefer_corners: bool,
    pub remove_overlaps: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            inside_first: false,
            prefer_corners: true,
            remove_overlaps: true,
        }
    }
}

/// 一个单元排进顺序后的走法：开放线段与扫描区块可反向，闭合轮廓选起刀顶点。
#[derive(Debug, Clone, Copy)]
struct Placed {
    unit: usize,
    reverse: bool,
    start: usize,
}

/// 闭合轮廓去掉重复终点后的顶点。
fn ring(points: &[Point]) -> &[Point] {
    &points[..points.len() - 1]
}

/// 预先算好的每个单元的进出点与可选起刀点。
struct Prepared {
    units: Vec<Unit>,
    /// 开放线段 / 扫描区块：[正走, 倒走] 的 (进点, 出点)
    ends: Vec<[(Point, Point); 2]>,
    /// 闭合轮廓可选的起刀顶点下标（优先尖角）
    starts: Vec<Vec<usize>>,
    /// 倒着走正好是原路反过来（2-opt 的前提）
    symmetric: bool,
}

fn corner_vertices(points: &[Point]) -> Vec<usize> {
    let ring = ring(points);
    let n = ring.len();
    let threshold = CORNER_TURN_DEG.to_radians();
    (0..n)
        .filter(|&i| {
            let (a, b, c) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
            let (d1, d2) = ((b.0 - a.0, b.1 - a.1), (c.0 - b.0, c.1 - b.1));
            let turn = (d1.0 * d2.1 - d1.1 * d2.0)
                .atan2(d1.0 * d2.0 + d1.1 * d2.1)
                .abs();
            turn > threshold
        })
        .collect()
}

fn prepare(units: Vec<Unit>, prefer_corners: bool) -> Prepared {
    let mut ends = Vec::with_capacity(units.len());
    let mut starts = Vec::with_capacity(units.len());
    let mut symmetric = true;
    for unit in &units {
        match unit {
            Unit::Closed(points) => {
                let corners = if prefer_corners {
                    corner_vertices(points)
                } else {
                    Vec::new()
                };
                starts.push(if corners.is_empty() {
                    (0..ring(points).len()).collect()
                } else {
                    corners
                });
                ends.push([(points[0], points[0]); 2]);
            }
            Unit::Open(points) => {
                let (first, last) = (points[0], points[points.len() - 1]);
                starts.push(Vec::new());
                ends.push([(first, last), (last, first)]);
            }
            Unit::Scan(block) => {
                let span = |reverse| {
                    let mut steps = Vec::new();
                    block.emit(reverse, &mut steps);
                    let point = |step: &Step| match *step {
                        Step::Travel(p) | Step::Coast(p) | Step::Burn(p) => p,
                    };
                    (point(&steps[0]), point(&steps[steps.len() - 1]))
                };
                let (forward, backward) = (span(false), span(true));
                if distance(forward.0, backward.1) > 1e-6 || distance(forward.1, backward.0) > 1e-6
                {
                    symmetric = false;
                }
                starts.push(Vec::new());
                ends.push([forward, backward]);
            }
        }
    }
    Prepared {
        units,
        ends,
        starts,
        symmetric,
    }
}

impl Prepared {
    fn entry(&self, placed: Placed) -> Point {
        match &self.units[placed.unit] {
            Unit::Closed(points) => ring(points)[placed.start],
            _ => self.ends[placed.unit][usize::from(placed.reverse)].0,
        }
    }

    fn exit(&self, placed: Placed) -> Point {
        match &self.units[placed.unit] {
            Unit::Closed(points) => ring(points)[placed.start],
            _ => self.ends[placed.unit][usize::from(placed.reverse)].1,
        }
    }

    /// 从 `from` 进入这个单元的最近走法与距离。
    fn best_entry(&self, unit: usize, from: Point) -> (Placed, f64) {
        match &self.units[unit] {
            Unit::Closed(points) => {
                let ring = ring(points);
                let (start, d) = self.starts[unit]
                    .iter()
                    .map(|&index| (index, distance(from, ring[index])))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .unwrap_or((0, 0.0));
                (
                    Placed {
                        unit,
                        reverse: false,
                        start,
                    },
                    d,
                )
            }
            _ => {
                let forward = Placed {
                    unit,
                    reverse: false,
                    start: 0,
                };
                let backward = Placed {
                    reverse: true,
                    ..forward
                };
                let (df, db) = (
                    distance(from, self.entry(forward)),
                    distance(from, self.entry(backward)),
                );
                if db < df {
                    (backward, db)
                } else {
                    (forward, df)
                }
            }
        }
    }

    fn travel(&self, order: &[Placed], start: Point) -> f64 {
        let mut current = start;
        let mut total = 0.0;
        for placed in order {
            total += distance(current, self.entry(*placed));
            current = self.exit(*placed);
        }
        total
    }
}

pub(crate) fn point_in_polygon(p: Point, polygon: &[Point]) -> bool {
    let mut inside = false;
    for pair in polygon.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

fn polygon_area(polygon: &[Point]) -> f64 {
    polygon
        .windows(2)
        .map(|pair| pair[0].0 * pair[1].1 - pair[1].0 * pair[0].1)
        .sum::<f64>()
        .abs()
        / 2.0
}

/// 每个闭合轮廓里面包着哪些闭合轮廓（只用于切割先内后外）。
fn containment(units: &[Unit]) -> Vec<Vec<usize>> {
    let areas: Vec<f64> = units
        .iter()
        .map(|unit| match unit {
            Unit::Closed(points) => polygon_area(points),
            _ => 0.0,
        })
        .collect();
    units
        .iter()
        .enumerate()
        .map(|(i, outer)| {
            let Unit::Closed(polygon) = outer else {
                return Vec::new();
            };
            units
                .iter()
                .enumerate()
                .filter(|(j, inner)| {
                    *j != i
                        && areas[*j] < areas[i]
                        && matches!(inner, Unit::Closed(points) if point_in_polygon(points[0], polygon))
                })
                .map(|(j, _)| j)
                .collect()
        })
        .collect()
}

fn same_points(a: &[Point], b: impl Iterator<Item = Point>) -> bool {
    let mut count = 0;
    for (p, q) in a.iter().zip(b) {
        if distance(*p, q) > OVERLAP_TOLERANCE_MM {
            return false;
        }
        count += 1;
    }
    count == a.len()
}

/// 两个单元是同一条线（开放线段可反向；闭合轮廓可换起点、可反向）。
fn duplicates(a: &Unit, b: &Unit) -> bool {
    match (a, b) {
        (Unit::Open(p), Unit::Open(q)) if p.len() == q.len() => {
            same_points(p, q.iter().copied()) || same_points(p, q.iter().rev().copied())
        }
        (Unit::Closed(p), Unit::Closed(q)) if p.len() == q.len() => {
            let (rp, rq) = (ring(p), ring(q));
            let n = rq.len();
            let Some(offset) = rq
                .iter()
                .position(|v| distance(*v, rp[0]) <= OVERLAP_TOLERANCE_MM)
            else {
                return false;
            };
            same_points(rp, (0..n).map(|k| rq[(offset + k) % n]))
                || same_points(rp, (0..n).map(|k| rq[(offset + n - k) % n]))
        }
        _ => false,
    }
}

/// 去掉重复的轮廓与线段（同 LightBurn“Remove Overlapping Lines”的整条重复部分），返回去掉几条。
fn remove_duplicates(units: &mut Vec<Unit>) -> usize {
    let key = |unit: &Unit| match unit {
        Unit::Closed(points) | Unit::Open(points) => {
            let length = burn_length(&vec![points.clone()]);
            Some(((length / 0.1).round() as i64, points.len()))
        }
        Unit::Scan(_) => None,
    };
    let keys: Vec<Option<(i64, usize)>> = units.iter().map(key).collect();
    let mut keep = vec![true; units.len()];
    for i in 0..units.len() {
        if !keep[i] || keys[i].is_none() {
            continue;
        }
        for j in i + 1..units.len() {
            if keep[j]
                && keys[j].is_some_and(|k| {
                    keys[i].is_some_and(|ki| (ki.0 - k.0).abs() <= 1 && ki.1 == k.1)
                })
                && duplicates(&units[i], &units[j])
            {
                keep[j] = false;
            }
        }
    }
    let removed = keep.iter().filter(|k| !**k).count();
    let mut index = 0;
    units.retain(|_| {
        let kept = keep[index];
        index += 1;
        kept
    });
    removed
}

/// 规划结果：按顺序的加工步骤、其中的出光路径（预览用）与空走统计。
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub steps: Vec<Step>,
    pub paths: Paths,
    pub stats: PathOptimizationStats,
    /// 去掉的重复轮廓 / 线段条数
    pub removed_duplicates: usize,
}

impl Planned {
    /// 最后停在哪里（下一层从这里接着规划）。
    pub fn exit(&self) -> Option<Point> {
        self.steps.last().map(|step| match *step {
            Step::Travel(p) | Step::Coast(p) | Step::Burn(p) => p,
        })
    }
}

/// 步骤里的出光路径：连续的出光步骤连成一条折线。
pub fn burn_paths(steps: &[Step]) -> Paths {
    let mut paths: Paths = Vec::new();
    let mut current: Option<Point> = None;
    let mut open = false;
    for step in steps {
        match *step {
            Step::Travel(p) | Step::Coast(p) => {
                open = false;
                current = Some(p);
            }
            Step::Burn(p) => {
                if !open {
                    paths.push(vec![current.unwrap_or(p)]);
                    open = true;
                }
                if let Some(path) = paths.last_mut() {
                    path.push(p);
                }
                current = Some(p);
            }
        }
    }
    paths
}

fn unit_burn(unit: &Unit) -> f64 {
    match unit {
        Unit::Closed(points) | Unit::Open(points) => burn_length(&vec![points.clone()]),
        Unit::Scan(block) => block.burn_length(),
    }
}

/// 把加工单元排成空走最短的顺序（见模块说明）。`start` 为激光头开始时在图案坐标里的位置。
pub fn plan(units: Vec<Unit>, start: Point, options: &PlanOptions) -> Result<Planned, SafetyError> {
    let mut units: Vec<Unit> = units
        .into_iter()
        .filter_map(|unit| match unit {
            Unit::Closed(points) => close_contour(&points).map(Unit::Closed),
            Unit::Open(points) if points.len() >= 2 => Some(Unit::Open(points)),
            Unit::Scan(block) if !block.rows.is_empty() => Some(Unit::Scan(block)),
            _ => None,
        })
        .collect();
    if units.is_empty() {
        return Err(SafetyError("没有可加工的矢量图形"));
    }
    let valid = units.iter().all(|unit| match unit {
        Unit::Closed(points) | Unit::Open(points) => finite(points),
        Unit::Scan(block) => block.rows.iter().all(|row| {
            row.v.is_finite() && row.spans.iter().all(|s| s.0.is_finite() && s.1.is_finite())
        }),
    });
    if !valid || !start.0.is_finite() || !start.1.is_finite() {
        return Err(SafetyError("矢量图形含有无效坐标"));
    }
    let removed_duplicates = if options.remove_overlaps {
        remove_duplicates(&mut units)
    } else {
        0
    };
    let expected_burn: f64 = units.iter().map(unit_burn).sum();
    let inner = if options.inside_first {
        containment(&units)
    } else {
        vec![Vec::new(); units.len()]
    };
    let prepared = prepare(units, options.prefer_corners);
    let n = prepared.units.len();
    let naive: Vec<Placed> = (0..n)
        .map(|unit| Placed {
            unit,
            reverse: false,
            start: 0,
        })
        .collect();
    let before = prepared.travel(&naive, start);

    // 1. 贪心：每次挑离当前位置最近、且（切割时）里面的轮廓都已切完的单元
    let mut done = vec![false; n];
    let mut order: Vec<Placed> = Vec::with_capacity(n);
    let mut current = start;
    for _ in 0..n {
        let mut best: Option<(Placed, f64)> = None;
        for unit in 0..n {
            if done[unit] || inner[unit].iter().any(|&j| !done[j]) {
                continue;
            }
            let candidate = prepared.best_entry(unit, current);
            if best.is_none_or(|(_, d)| candidate.1 < d) {
                best = Some(candidate);
            }
        }
        // 包含关系成环（数值退化）时放开约束，保证每个单元都排进去
        let (placed, _) = best.unwrap_or_else(|| {
            let unit = (0..n).find(|&u| !done[u]).unwrap_or(0);
            prepared.best_entry(unit, current)
        });
        done[placed.unit] = true;
        current = prepared.exit(placed);
        order.push(placed);
    }

    // 2. 2-opt：整段反向能缩短空走就保留（切割先内后外、或有单向扫描区块时不做）
    let mut strategy = "nearest_neighbor";
    if !options.inside_first && prepared.symmetric && n > 2 && n <= TWO_OPT_MAX_UNITS {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
        let flip = |placed: Placed| match prepared.units[placed.unit] {
            Unit::Closed(_) => placed,
            _ => Placed {
                reverse: !placed.reverse,
                ..placed
            },
        };
        'passes: for _ in 0..TWO_OPT_MAX_PASSES {
            let mut improved = false;
            for a in 0..n - 1 {
                let previous = if a == 0 {
                    start
                } else {
                    prepared.exit(order[a - 1])
                };
                for b in a + 1..n {
                    let following = order.get(b + 1).map(|p| prepared.entry(*p));
                    let old = distance(previous, prepared.entry(order[a]))
                        + following.map_or(0.0, |f| distance(prepared.exit(order[b]), f));
                    let new = distance(previous, prepared.exit(order[b]))
                        + following.map_or(0.0, |f| distance(prepared.entry(order[a]), f));
                    if new < old - 1e-9 {
                        order[a..=b].reverse();
                        for placed in &mut order[a..=b] {
                            *placed = flip(*placed);
                        }
                        improved = true;
                        strategy = "nearest_neighbor+2opt";
                    }
                }
                if std::time::Instant::now() > deadline {
                    break 'passes;
                }
            }
            if !improved {
                break;
            }
        }
    }

    // 3. 按最终顺序重新给每个闭合轮廓挑离上一个出口最近的起刀点
    let mut current = start;
    for placed in &mut order {
        if matches!(prepared.units[placed.unit], Unit::Closed(_)) {
            *placed = prepared.best_entry(placed.unit, current).0;
        }
        current = prepared.exit(*placed);
    }
    let after = prepared.travel(&order, start);

    let mut steps: Vec<Step> = Vec::new();
    for placed in &order {
        match &prepared.units[placed.unit] {
            Unit::Closed(points) => {
                let ring = ring(points);
                let n = ring.len();
                steps.push(Step::Travel(ring[placed.start]));
                steps.extend((1..=n).map(|k| Step::Burn(ring[(placed.start + k) % n])));
            }
            Unit::Open(points) => {
                let ordered: Vec<Point> = if placed.reverse {
                    points.iter().rev().copied().collect()
                } else {
                    points.clone()
                };
                steps.push(Step::Travel(ordered[0]));
                steps.extend(ordered[1..].iter().map(|p| Step::Burn(*p)));
            }
            Unit::Scan(block) => block.emit(placed.reverse, &mut steps),
        }
    }
    let paths = burn_paths(&steps);
    // 自检：规划只改顺序与方向，出光总长度必须与（去重后的）输入完全一致
    if (burn_length(&paths) - expected_burn).abs() > 1e-6 * expected_burn.max(1.0) {
        return Err(SafetyError("路径规划改变了出光路径，拒绝生成 G-code"));
    }
    Ok(Planned {
        steps,
        paths,
        stats: PathOptimizationStats {
            strategy: strategy.to_owned(),
            travel_distance_before: before,
            travel_distance_after: after,
            travel_reduction_ratio: if before <= 0.0 {
                0.0
            } else {
                (before - after) / before
            },
            path_count: n,
        },
        removed_duplicates,
    })
}

/// 按层依次规划（同 LightBurn“按图层排序”：先扫描填充、再描轮廓），每层从上一层结束处接着走。
pub fn plan_layers(
    layers: Vec<Vec<Unit>>,
    start: Point,
    options: &PlanOptions,
) -> Result<Planned, SafetyError> {
    let mut merged = Planned {
        steps: Vec::new(),
        paths: Vec::new(),
        stats: PathOptimizationStats {
            strategy: String::new(),
            travel_distance_before: 0.0,
            travel_distance_after: 0.0,
            travel_reduction_ratio: 0.0,
            path_count: 0,
        },
        removed_duplicates: 0,
    };
    let mut current = start;
    for units in layers.into_iter().filter(|units| !units.is_empty()) {
        let planned = plan(units, current, options)?;
        current = planned.exit().unwrap_or(current);
        if merged.stats.strategy.is_empty() || planned.stats.strategy.ends_with("2opt") {
            merged.stats.strategy = planned.stats.strategy;
        }
        merged.stats.travel_distance_before += planned.stats.travel_distance_before;
        merged.stats.travel_distance_after += planned.stats.travel_distance_after;
        merged.stats.path_count += planned.stats.path_count;
        merged.removed_duplicates += planned.removed_duplicates;
        merged.steps.extend(planned.steps);
        merged.paths.extend(planned.paths);
    }
    if merged.steps.is_empty() {
        return Err(SafetyError("没有可加工的矢量图形"));
    }
    let before = merged.stats.travel_distance_before;
    if before > 0.0 {
        merged.stats.travel_reduction_ratio =
            (before - merged.stats.travel_distance_after) / before;
    }
    Ok(merged)
}

/// 出光走过的总长度。
pub fn burn_length(paths: &Paths) -> f64 {
    paths
        .iter()
        .map(|path| path.windows(2).map(|w| distance(w[0], w[1])).sum::<f64>())
        .sum()
}
