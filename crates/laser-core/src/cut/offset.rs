//! 切缝补偿：按环绕数整理出干净边界、逐边平移补圆弧，再求并集。

use super::{MAX_KERF_MM, add, cross, distance, dot, rotate, scale, signed_area, sub, unit};
use crate::safety::SafetyError;
use crate::vector_plan::close_contour;
use laser_contracts::geometry::Point;
use std::collections::{HashMap, HashSet};

/// 补偿运算的边数上限：再多说明图形不适合切割。
const MAX_OFFSET_EDGES: usize = 200_000;

/// 凸角圆弧的弦高误差（mm）。
const ARC_TOLERANCE_MM: f64 = 0.005;

/// 交点合并距离（mm）。
const SNAP_MM: f64 = 1e-6;

/// 判断边两侧环绕数时的取样距离（mm）。
const SIDE_MM: f64 = 1e-7;

/// 补偿后面积小于它的环当作数值碎屑丢掉（mm²）。
const MIN_AREA_MM2: f64 = 1e-6;

// ---------- 并集（按环绕数取边界） ----------

#[derive(Clone, Copy)]
enum Keep {
    NonZero,
    Positive,
}

#[derive(Default)]
struct Nodes {
    points: Vec<Point>,
    grid: HashMap<(i64, i64), Vec<usize>>,
}

impl Nodes {
    fn cell(p: Point) -> (i64, i64) {
        (
            (p.0 / SNAP_MM).floor() as i64,
            (p.1 / SNAP_MM).floor() as i64,
        )
    }

    /// 相距不到 [`SNAP_MM`] 的点算同一个节点。
    fn id(&mut self, p: Point) -> usize {
        let (cx, cy) = Self::cell(p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(list) = self.grid.get(&(cx + dx, cy + dy)) {
                    for &i in list {
                        if distance(self.points[i], p) <= SNAP_MM {
                            return i;
                        }
                    }
                }
            }
        }
        let i = self.points.len();
        self.points.push(p);
        self.grid.entry((cx, cy)).or_default().push(i);
        i
    }
}

/// 按水平条带索引的环绕数查询。
struct Winding<'a> {
    edges: &'a [(Point, Point)],
    y0: f64,
    y1: f64,
    h: f64,
    strips: Vec<Vec<usize>>,
}

impl<'a> Winding<'a> {
    fn new(edges: &'a [(Point, Point)]) -> Self {
        let (y0, y1) = edges
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), (a, b)| {
                (lo.min(a.1).min(b.1), hi.max(a.1).max(b.1))
            });
        let count = ((edges.len() as f64).sqrt().ceil() as usize).clamp(1, 1024);
        let h = ((y1 - y0) / count as f64).max(1e-9);
        let mut strips = vec![Vec::new(); count];
        let index = |y: f64| (((y - y0) / h).floor().max(0.0) as usize).min(count - 1);
        for (i, (a, b)) in edges.iter().enumerate() {
            for strip in &mut strips[index(a.1.min(b.1))..=index(a.1.max(b.1))] {
                strip.push(i);
            }
        }
        Self {
            edges,
            y0,
            y1,
            h,
            strips,
        }
    }

    fn at(&self, q: Point) -> i32 {
        if q.1 < self.y0 || q.1 > self.y1 {
            return 0;
        }
        let strip =
            (((q.1 - self.y0) / self.h).floor().max(0.0) as usize).min(self.strips.len() - 1);
        let mut winding = 0;
        for &i in &self.strips[strip] {
            let (a, b) = self.edges[i];
            let side = cross(sub(b, a), sub(q, a));
            if a.1 <= q.1 {
                if b.1 > q.1 && side > 0.0 {
                    winding += 1;
                }
            } else if b.1 <= q.1 && side < 0.0 {
                winding -= 1;
            }
        }
        winding
    }
}

/// 两条线段的交点参数（各自 0..1），共线重叠时取对方端点的投影。
fn split_params(p: (Point, Point), q: (Point, Point), tp: &mut Vec<f64>, tq: &mut Vec<f64>) {
    let (r, s) = (sub(p.1, p.0), sub(q.1, q.0));
    let (rr, ss) = (dot(r, r), dot(s, s));
    if rr <= 0.0 || ss <= 0.0 {
        return;
    }
    let qp = sub(q.0, p.0);
    let denom = cross(r, s);
    if denom.abs() > 1e-12 * (rr * ss).sqrt() {
        let t = cross(qp, s) / denom;
        let u = cross(qp, r) / denom;
        let (et, eu) = (SNAP_MM / rr.sqrt(), SNAP_MM / ss.sqrt());
        if (-et..=1.0 + et).contains(&t) && (-eu..=1.0 + eu).contains(&u) {
            tp.push(t.clamp(0.0, 1.0));
            tq.push(u.clamp(0.0, 1.0));
        }
        return;
    }
    // 平行：共线时互相在对方的端点处切开
    if cross(qp, r).abs() / rr.sqrt() > SNAP_MM {
        return;
    }
    for end in [q.0, q.1] {
        let t = dot(sub(end, p.0), r) / rr;
        if t > 0.0 && t < 1.0 {
            tp.push(t);
        }
    }
    for end in [p.0, p.1] {
        let u = dot(sub(end, q.0), s) / ss;
        if u > 0.0 && u < 1.0 {
            tq.push(u);
        }
    }
}

/// 去掉重合与共线的点；剩不到 3 个点或面积太小返回 `None`。
fn clean_ring(points: Vec<Point>) -> Option<Vec<Point>> {
    let mut ring: Vec<Point> = Vec::with_capacity(points.len());
    for p in points {
        if ring.last().is_none_or(|last| distance(*last, p) > SNAP_MM) {
            ring.push(p);
        }
    }
    while ring.len() >= 2 && distance(ring[0], ring[ring.len() - 1]) <= SNAP_MM {
        ring.pop();
    }
    // 反复去掉夹在一条直线中间的点
    let mut changed = true;
    while changed && ring.len() >= 3 {
        changed = false;
        let n = ring.len();
        let mut keep = vec![true; n];
        for i in 0..n {
            let (a, b, c) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
            let base = distance(a, c);
            let collinear =
                base > 0.0 && (cross(sub(b, a), sub(c, a)) / base).abs() < SNAP_MM * 0.1;
            if collinear && dot(sub(b, a), sub(c, b)) > 0.0 && keep[(i + n - 1) % n] {
                keep[i] = false;
                changed = true;
            }
        }
        let mut index = 0;
        ring.retain(|_| {
            let kept = keep[index];
            index += 1;
            kept
        });
    }
    (ring.len() >= 3 && signed_area(&ring).abs() >= MIN_AREA_MM2).then_some(ring)
}

/// 一组环（可自交、可重叠、方向任意）按环绕规则取区域边界：区域在每条输出边的左侧。
fn boundary(rings: &[Vec<Point>], keep: Keep) -> Result<Vec<Vec<Point>>, SafetyError> {
    let mut edges: Vec<(Point, Point)> = Vec::new();
    for ring in rings {
        let n = ring.len();
        if n < 3 {
            continue;
        }
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            if distance(a, b) > SNAP_MM {
                edges.push((a, b));
            }
        }
    }
    if edges.len() > MAX_OFFSET_EDGES {
        return Err(SafetyError("切割图形太复杂，无法做切缝补偿"));
    }
    if edges.is_empty() {
        return Ok(Vec::new());
    }

    // 1. 求所有交点，把每条边在交点处切开（按 x 排序扫一遍，只比较 x 范围重叠的边）
    let n = edges.len();
    let mut splits: Vec<Vec<f64>> = vec![vec![0.0, 1.0]; n];
    let bounds: Vec<(f64, f64, f64, f64)> = edges
        .iter()
        .map(|(a, b)| (a.0.min(b.0), a.0.max(b.0), a.1.min(b.1), a.1.max(b.1)))
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| bounds[a].0.total_cmp(&bounds[b].0));
    for (k, &i) in order.iter().enumerate() {
        let bi = bounds[i];
        for &j in &order[k + 1..] {
            let bj = bounds[j];
            if bj.0 > bi.1 + SNAP_MM {
                break;
            }
            if bj.2 > bi.3 + SNAP_MM || bi.2 > bj.3 + SNAP_MM {
                continue;
            }
            let (mut ti, mut tj) = (Vec::new(), Vec::new());
            split_params(edges[i], edges[j], &mut ti, &mut tj);
            splits[i].extend(ti);
            splits[j].extend(tj);
        }
    }

    // 2. 切开后的小段，端点合并成节点
    let mut nodes = Nodes::default();
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    for (i, &(a, b)) in edges.iter().enumerate() {
        let ts = &mut splits[i];
        ts.sort_by(f64::total_cmp);
        ts.dedup_by(|x, y| (*x - *y).abs() < 1e-12);
        let ids: Vec<usize> = ts
            .iter()
            .map(|&t| {
                nodes.id(if t <= 0.0 {
                    a
                } else if t >= 1.0 {
                    b
                } else {
                    add(a, scale(sub(b, a), t))
                })
            })
            .collect();
        pieces.extend(
            ids.windows(2)
                .filter(|w| w[0] != w[1])
                .map(|w| (w[0], w[1])),
        );
    }

    // 3. 每段看两侧的环绕数：一侧在区域里、一侧不在的才是边界，方向取区域在左
    let winding = Winding::new(&edges);
    let inside = |w: i32| match keep {
        Keep::NonZero => w != 0,
        Keep::Positive => w > 0,
    };
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut kept: Vec<(usize, usize)> = Vec::new();
    for (u, v) in pieces {
        let (pa, pb) = (nodes.points[u], nodes.points[v]);
        let mid = scale(add(pa, pb), 0.5);
        let dir = unit(sub(pb, pa));
        let normal = (-dir.1, dir.0);
        let left = inside(winding.at(add(mid, scale(normal, SIDE_MM))));
        let right = inside(winding.at(sub(mid, scale(normal, SIDE_MM))));
        let edge = match (left, right) {
            (true, false) => (u, v),
            (false, true) => (v, u),
            _ => continue,
        };
        if seen.insert(edge) {
            kept.push(edge);
        }
    }

    // 4. 连成环：在交汇点总是取最靠左的出边，相切的两个图形保持分开
    let mut outgoing: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, &(u, _)) in kept.iter().enumerate() {
        outgoing.entry(u).or_default().push(index);
    }
    let mut used = vec![false; kept.len()];
    let mut result = Vec::new();
    for first in 0..kept.len() {
        if used[first] {
            continue;
        }
        let origin = kept[first].0;
        let mut ring = vec![nodes.points[origin]];
        let mut current = first;
        let mut closed = false;
        for _ in 0..=kept.len() {
            used[current] = true;
            let (a, b) = kept[current];
            if b == origin {
                closed = true;
                break;
            }
            ring.push(nodes.points[b]);
            let incoming = sub(nodes.points[b], nodes.points[a]);
            let next = outgoing.get(&b).and_then(|candidates| {
                candidates
                    .iter()
                    .copied()
                    .filter(|&e| !used[e])
                    .map(|e| {
                        let out = sub(nodes.points[kept[e].1], nodes.points[b]);
                        let mut turn = cross(incoming, out).atan2(dot(incoming, out));
                        if turn >= std::f64::consts::PI - 1e-12 {
                            turn = -std::f64::consts::PI;
                        }
                        (e, turn)
                    })
                    .max_by(|x, y| x.1.total_cmp(&y.1))
                    .map(|(e, _)| e)
            });
            match next {
                Some(e) => current = e,
                None => break,
            }
        }
        if closed && let Some(ring) = clean_ring(ring) {
            result.push(ring);
        }
    }
    Ok(result)
}

/// 区域在左侧的环往右（外）平移 `d`（负值往里）：凸角补圆弧，凹角经过原顶点（留给并集去掉）。
fn raw_offset(ring: &[Point], d: f64) -> Vec<Point> {
    let n = ring.len();
    let ratio = (1.0 - ARC_TOLERANCE_MM / d.abs()).clamp(-1.0, 1.0);
    let step = (2.0 * ratio.acos()).clamp(0.02, std::f64::consts::FRAC_PI_4);
    let right = |u: Point| (u.1, -u.0);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let (prev, p, next) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
        let (u, v) = (unit(sub(p, prev)), unit(sub(next, p)));
        let (s, e) = (scale(right(u), d), scale(right(v), d));
        let (c, cos) = (cross(u, v), dot(u, v));
        if c.abs() < 1e-12 && cos > 0.0 {
            out.push(add(p, s));
        } else if c * d > 0.0 || c.abs() < 1e-12 {
            // 外凸（或原路折返）：补一段圆弧
            let sweep = if c.abs() < 1e-12 {
                std::f64::consts::PI * d.signum()
            } else {
                c.atan2(cos)
            };
            let k = ((sweep.abs() / step).ceil() as usize).max(1);
            out.extend((0..=k).map(|j| add(p, rotate(s, sweep * j as f64 / k as f64))));
        } else {
            out.extend([add(p, s), p, add(p, e)]);
        }
    }
    out
}

/// 切缝补偿结果：闭合轮廓（首尾点相同）与补偿前后的轮廓数。
#[derive(Debug, Clone, PartialEq)]
pub struct KerfOffset {
    pub contours: Vec<Vec<Point>>,
    pub before: usize,
    pub after: usize,
}

/// 切缝补偿（见模块说明）：`kerf_mm` 正值往外、负值往里，内孔反向；轮廓按非零环绕解释。
pub fn offset_contours(contours: &[Vec<Point>], kerf_mm: f64) -> Result<KerfOffset, SafetyError> {
    if !kerf_mm.is_finite() || kerf_mm.abs() > MAX_KERF_MM {
        return Err(SafetyError("切缝补偿须在 -2 ~ 2 mm"));
    }
    if contours
        .iter()
        .flatten()
        .any(|p| !p.0.is_finite() || !p.1.is_finite())
    {
        return Err(SafetyError("切割轮廓含有无效坐标"));
    }
    let rings: Vec<Vec<Point>> = contours
        .iter()
        .filter_map(|path| close_contour(path))
        .map(|mut ring| {
            ring.pop();
            ring
        })
        .collect();
    let normalized = boundary(&rings, Keep::NonZero)?;
    if normalized.is_empty() {
        return Err(SafetyError("没有可切割的闭合图形"));
    }
    let before = normalized.len();
    let result = if kerf_mm == 0.0 {
        normalized
    } else {
        let raw: Vec<Vec<Point>> = normalized
            .iter()
            .map(|ring| raw_offset(ring, kerf_mm))
            .collect();
        boundary(&raw, Keep::Positive)?
    };
    if result.is_empty() {
        return Err(SafetyError("切缝补偿后没有剩下可切割的图形，请减小内缩量"));
    }
    let after = result.len();
    Ok(KerfOffset {
        contours: result
            .into_iter()
            .map(|mut ring| {
                ring.push(ring[0]);
                ring
            })
            .collect(),
        before,
        after,
    })
}
