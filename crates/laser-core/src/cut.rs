//! 矢量切割质量（2026-11-16 用户决定：对齐 LightBurn Line 模式 / Cut 设置，“直接一起修改完”）。
//!
//! 全部默认关闭（0），不传时切割输出与以前完全相同。
//!
//! - **切缝补偿** [`offset_contours`]（LightBurn Kerf Offset）：只作用于闭合图形。正值往外、负值往里；
//!   内孔自动反向（零件变大时孔变小），所以切下来的零件保持设计尺寸。实现同 Clipper 的做法：
//!   先按非零环绕把轮廓（字体的外框 / 内孔、相互重叠的笔画）整理成“区域在左侧”的干净边界，
//!   每条边往外平移、凸角补圆弧、凹角经过原顶点，最后按正环绕数求并集——内缩时变没的小图形
//!   自然消失（不会输出翻折的垃圾路径），全部消失直接报错。
//! - **后处理** [`apply_cut_options`]：在路线规划（含出光总长度自检）之后，对规划出的每个闭合轮廓：
//!   - 桥位 / 断点（Tabs）：按个数或间距沿周长均匀分布、宽度 mm，可跳过内部图形；桥位处 `Coast`
//!     （`G1 … S0`）关光匀速滑过，零件不会掉落；
//!   - 引入线 / 引出线（Lead-in / Lead-out）：长度 mm + 角度（90° 垂直于起刀边），总在废料一侧
//!     （外轮廓在外面、内孔在孔里），会碰到任何轮廓或落到零件一侧时这一条不加；
//!   - 过切（Overcut）：回到起点后沿轮廓多切一段，跨过桥位时同样关光。
//!
//!   后处理单独自检：轮廓上走过的长度等于周长 + 过切，出光总长度 = 规划出光长度 − 桥位 + 引线 + 过切。

use crate::gcode::Step;
use crate::safety::SafetyError;
use crate::vector_plan::{burn_length, burn_paths, close_contour, point_in_polygon};
use laser_contracts::geometry::Point;
use std::collections::{HashMap, HashSet};

/// 切缝补偿上限（mm）；激光切缝一般 0.1 ~ 0.3 mm，补偿取一半。
pub const MAX_KERF_MM: f64 = 2.0;
/// 引入 / 引出线与过切上限（mm）。
pub const MAX_LEAD_MM: f64 = 10.0;
pub const MAX_OVERCUT_MM: f64 = 10.0;
/// 桥位宽度范围（mm）、个数与间距上限。
pub const MIN_TAB_WIDTH_MM: f64 = 0.05;
pub const MAX_TAB_WIDTH_MM: f64 = 10.0;
pub const MAX_TAB_COUNT: u32 = 100;
pub const MIN_TAB_SPACING_MM: f64 = 1.0;
pub const MAX_TAB_SPACING_MM: f64 = 1000.0;
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

/// 桥位怎么分布。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TabMode {
    Off,
    /// 每个闭合图形固定几个
    Count(u32),
    /// 每隔多少 mm 一个（至少 1 个）
    Spacing(f64),
}

/// 切割设置（见模块说明），[`Default`] 全部关闭。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutOptions {
    /// 切缝补偿（mm）：正值往外，负值往里
    pub kerf_mm: f64,
    pub tabs: TabMode,
    pub tab_width_mm: f64,
    /// 内部图形（被别的图形包着的，如“口”的内孔）不加桥位
    pub tabs_skip_inner: bool,
    pub lead_in_mm: f64,
    pub lead_out_mm: f64,
    /// 引线与起刀边的夹角（度，1 ~ 90，90 = 垂直）
    pub lead_angle_deg: f64,
    pub overcut_mm: f64,
}

impl Default for CutOptions {
    fn default() -> Self {
        Self {
            kerf_mm: 0.0,
            tabs: TabMode::Off,
            tab_width_mm: 0.5,
            tabs_skip_inner: true,
            lead_in_mm: 0.0,
            lead_out_mm: 0.0,
            lead_angle_deg: 90.0,
            overcut_mm: 0.0,
        }
    }
}

impl CutOptions {
    /// 有任何一项会改变刀路。
    pub fn is_active(&self) -> bool {
        self.kerf_mm != 0.0 || self.needs_post_processing()
    }

    /// 需要在规划之后改写闭合轮廓（桥位、引线、过切）。
    pub fn needs_post_processing(&self) -> bool {
        self.tabs != TabMode::Off
            || self.lead_in_mm > 0.0
            || self.lead_out_mm > 0.0
            || self.overcut_mm > 0.0
    }

    /// 刀路可能越出原图形的最大距离（mm）：排版留边与幅面检查用。
    pub fn extension_mm(&self) -> f64 {
        self.kerf_mm.max(0.0) + self.lead_in_mm.max(self.lead_out_mm).max(0.0)
    }

    pub fn validate(&self) -> Result<(), SafetyError> {
        let within = |v: f64, lo: f64, hi: f64| v.is_finite() && (lo..=hi).contains(&v);
        if !within(self.kerf_mm, -MAX_KERF_MM, MAX_KERF_MM) {
            return Err(SafetyError("切缝补偿须在 -2 ~ 2 mm"));
        }
        if !within(self.lead_in_mm, 0.0, MAX_LEAD_MM)
            || !within(self.lead_out_mm, 0.0, MAX_LEAD_MM)
            || !within(self.lead_angle_deg, 1.0, 90.0)
        {
            return Err(SafetyError("引入 / 引出线须在 0 ~ 10 mm，角度 1 ~ 90 度"));
        }
        if !within(self.overcut_mm, 0.0, MAX_OVERCUT_MM) {
            return Err(SafetyError("过切须在 0 ~ 10 mm"));
        }
        let tabs_ok = match self.tabs {
            TabMode::Off => true,
            TabMode::Count(n) => (1..=MAX_TAB_COUNT).contains(&n),
            TabMode::Spacing(s) => within(s, MIN_TAB_SPACING_MM, MAX_TAB_SPACING_MM),
        };
        if !tabs_ok
            || (self.tabs != TabMode::Off
                && !within(self.tab_width_mm, MIN_TAB_WIDTH_MM, MAX_TAB_WIDTH_MM))
        {
            return Err(SafetyError(
                "桥位个数须在 1 ~ 100、间距 1 ~ 1000 mm、宽度 0.05 ~ 10 mm",
            ));
        }
        Ok(())
    }
}

/// 切割后处理与补偿的统计（结果里回显）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CutReport {
    /// 处理的闭合轮廓数
    pub contours: usize,
    /// 切缝补偿前后的轮廓数（整理重叠后 → 补偿后；变少说明有图形消失或相连）
    pub kerf_contours: Option<(usize, usize)>,
    pub tabs: usize,
    /// 桥位关光总长度（mm）
    pub tab_gap_mm: f64,
    /// 太小放不下桥位的轮廓数
    pub contours_without_tabs: usize,
    pub lead_ins: usize,
    pub lead_outs: usize,
    /// 会碰到别的轮廓或落到零件一侧、因而没加的引线条数
    pub leads_skipped: usize,
    /// 过切总长度（mm）
    pub overcut_mm: f64,
}

// ---------- 小工具 ----------

fn sub(a: Point, b: Point) -> Point {
    (a.0 - b.0, a.1 - b.1)
}

fn add(a: Point, b: Point) -> Point {
    (a.0 + b.0, a.1 + b.1)
}

fn scale(a: Point, k: f64) -> Point {
    (a.0 * k, a.1 * k)
}

fn dot(a: Point, b: Point) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

fn cross(a: Point, b: Point) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn distance(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn unit(a: Point) -> Point {
    let len = a.0.hypot(a.1);
    if len <= 0.0 {
        (0.0, 0.0)
    } else {
        (a.0 / len, a.1 / len)
    }
}

fn rotate(a: Point, angle: f64) -> Point {
    let (sin, cos) = angle.sin_cos();
    (a.0 * cos - a.1 * sin, a.0 * sin + a.1 * cos)
}

/// 有向面积（标准鞋带公式，环不重复首点）：区域在左侧的外轮廓为正。
fn signed_area(ring: &[Point]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| cross(ring[i], ring[(i + 1) % n]))
        .sum::<f64>()
        / 2.0
}

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

// ---------- 后处理：桥位、引线、过切 ----------

fn step_point(step: &Step) -> Point {
    match *step {
        Step::Travel(p) | Step::Coast(p) | Step::Burn(p) => p,
    }
}

/// 规划结果里的闭合轮廓：`Travel(p0)` 后接至少 3 个 `Burn`、最后回到 p0（[`crate::vector_plan::plan`] 的写法）。
fn find_loops(steps: &[Step]) -> Vec<(usize, usize)> {
    let mut loops = Vec::new();
    let mut i = 0;
    while i < steps.len() {
        if let Step::Travel(p0) = steps[i] {
            let mut k = i + 1;
            while k < steps.len() && matches!(steps[k], Step::Burn(_)) {
                k += 1;
            }
            if k - i > 3 && distance(step_point(&steps[k - 1]), p0) <= 1e-9 {
                loops.push((i, k));
                i = k;
                continue;
            }
        }
        i += 1;
    }
    loops
}

/// 线段 a→b 碰到任何轮廓的边（交点离 `ignore` 超过 1 µm）。
fn crosses_any(a: Point, b: Point, rings: &[Vec<Point>], ignore: Point) -> bool {
    let r = sub(b, a);
    rings.iter().any(|ring| {
        ring.windows(2).any(|edge| {
            let (c, d) = (edge[0], edge[1]);
            let s = sub(d, c);
            let denom = cross(r, s);
            let qp = sub(c, a);
            if denom.abs() < 1e-15 {
                // 平行：共线重叠且不止在 ignore 这一点相碰就算碰到
                if cross(qp, r).abs() > 1e-9 * r.0.hypot(r.1).max(1.0) {
                    return false;
                }
                let rr = dot(r, r).max(1e-30);
                let (t0, t1) = (dot(sub(c, a), r) / rr, dot(sub(d, a), r) / rr);
                let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(1.0));
                return lo <= hi
                    && (distance(add(a, scale(r, lo)), ignore) > 1e-6
                        || distance(add(a, scale(r, hi)), ignore) > 1e-6);
            }
            let t = cross(qp, s) / denom;
            let u = cross(qp, r) / denom;
            (0.0..=1.0).contains(&t)
                && (0.0..=1.0).contains(&u)
                && distance(add(a, scale(r, t)), ignore) > 1e-6
        })
    })
}

struct LoopGeometry<'a> {
    ring: &'a [Point],
    cum: Vec<f64>,
    perimeter: f64,
    /// 外轮廓（废料在外面）；内孔（废料在孔里）为 false
    outer: bool,
    ccw: bool,
}

impl LoopGeometry<'_> {
    /// 周长位置 `pos`（0 ~ 2 圈）处的点。
    fn point_at(&self, pos: f64) -> Point {
        let lap = if pos >= self.perimeter {
            pos - self.perimeter
        } else {
            pos
        };
        let k = self
            .cum
            .partition_point(|&c| c <= lap)
            .clamp(1, self.cum.len() - 1);
        let (a, b) = (self.ring[k - 1], self.ring[k]);
        let len = self.cum[k] - self.cum[k - 1];
        if len <= 0.0 {
            return b;
        }
        add(
            a,
            scale(sub(b, a), ((lap - self.cum[k - 1]) / len).clamp(0.0, 1.0)),
        )
    }

    /// 沿行进方向 `tangent` 时指向废料一侧的单位法线。
    fn scrap_normal(&self, tangent: Point) -> Point {
        let left = (-tangent.1, tangent.0);
        let interior = if self.ccw { left } else { scale(left, -1.0) };
        if self.outer {
            scale(interior, -1.0)
        } else {
            interior
        }
    }

    /// 周长位置 `pos` 处进来与出去的方向（在顶点上两者不同）。
    fn directions_at(&self, pos: f64) -> (Point, Point) {
        let lap = pos.rem_euclid(self.perimeter);
        let n = self.ring.len() - 1;
        let edge = |k: usize| unit(sub(self.ring[k % n + 1], self.ring[k % n]));
        let k = self.cum.partition_point(|&c| c <= lap + 1e-9).clamp(1, n) - 1;
        if (lap - self.cum[k]).abs() <= 1e-9 {
            (edge(k + n - 1), edge(k))
        } else {
            (edge(k), edge(k))
        }
    }

    /// 引线的基准方向：进出两条边废料一侧法线的角平分线（尖角上不贴着前一条边走）。
    fn lead_normal(&self, incoming: Point, outgoing: Point) -> Point {
        let sum = add(self.scrap_normal(incoming), self.scrap_normal(outgoing));
        if sum.0.hypot(sum.1) < 1e-9 {
            self.scrap_normal(outgoing)
        } else {
            unit(sum)
        }
    }

    /// 点在废料一侧（外轮廓的外面 / 内孔的里面）。
    fn on_scrap_side(&self, p: Point) -> bool {
        point_in_polygon(p, self.ring) != self.outer
    }
}

/// 在规划好的步骤上给闭合轮廓加桥位、引线与过切（见模块说明），并单独自检。
pub fn apply_cut_options(
    steps: &[Step],
    options: &CutOptions,
) -> Result<(Vec<Step>, CutReport), SafetyError> {
    options.validate()?;
    let loops = find_loops(steps);
    let rings: Vec<Vec<Point>> = loops
        .iter()
        .map(|&(start, end)| steps[start..end].iter().map(step_point).collect())
        .collect();
    let areas: Vec<f64> = rings
        .iter()
        .map(|ring| signed_area(&ring[..ring.len() - 1]))
        .collect();
    let mut report = CutReport {
        contours: loops.len(),
        ..CutReport::default()
    };
    let mut out: Vec<Step> = Vec::with_capacity(steps.len() + loops.len() * 8);
    let mut added = 0.0;
    let mut cursor = 0;
    let (sin, cos) = options.lead_angle_deg.to_radians().sin_cos();
    for (index, &(start, end)) in loops.iter().enumerate() {
        out.extend_from_slice(&steps[cursor..start]);
        cursor = end;
        let ring = &rings[index];
        // 被几个（面积更大的）轮廓包着：偶数层是零件外框，奇数层是孔
        let probe = scale(add(ring[0], ring[1]), 0.5);
        let depth = rings
            .iter()
            .enumerate()
            .filter(|(j, other)| {
                *j != index
                    && areas[*j].abs() > areas[index].abs()
                    && point_in_polygon(probe, other)
            })
            .count();
        let mut cum = vec![0.0];
        for pair in ring.windows(2) {
            cum.push(cum[cum.len() - 1] + distance(pair[0], pair[1]));
        }
        let perimeter = cum[cum.len() - 1];
        let geometry = LoopGeometry {
            ring,
            perimeter,
            cum,
            outer: depth % 2 == 0,
            ccw: areas[index] > 0.0,
        };

        // 桥位：均匀分布，第一个离起刀点半个间距；总宽不超过周长一半
        let requested = match options.tabs {
            TabMode::Off => 0,
            _ if options.tabs_skip_inner && depth > 0 => 0,
            TabMode::Count(n) => n as usize,
            TabMode::Spacing(spacing) => ((perimeter / spacing).round() as usize).max(1),
        };
        let fit = (0.5 * perimeter / options.tab_width_mm).floor() as usize;
        let count = requested.min(fit);
        if requested > 0 && count == 0 {
            report.contours_without_tabs += 1;
        }
        let half = options.tab_width_mm / 2.0;
        let gaps: Vec<(f64, f64)> = (0..count)
            .flat_map(|j| {
                let center = (j as f64 + 0.5) * perimeter / count as f64;
                [
                    (center - half, center + half),
                    (center + perimeter - half, center + perimeter + half),
                ]
            })
            .collect();
        // 过切：最多一圈；结束点落在桥位里时停在桥位前
        let mut finish = perimeter + options.overcut_mm.min(perimeter);
        if let Some(&(g0, _)) = gaps.iter().find(|&&(g0, g1)| g0 < finish && finish < g1) {
            finish = g0;
        }
        let overcut = finish - perimeter;

        // 引入线：从废料一侧切进起刀点
        let p0 = ring[0];
        let lead_in_from = (options.lead_in_mm > 0.0).then(|| {
            let (incoming, tangent) = geometry.directions_at(0.0);
            let normal = geometry.lead_normal(incoming, tangent);
            add(
                p0,
                scale(
                    unit(sub(scale(normal, sin), scale(tangent, cos))),
                    options.lead_in_mm,
                ),
            )
        });
        match lead_in_from {
            Some(from) if geometry.on_scrap_side(from) && !crosses_any(from, p0, &rings, p0) => {
                out.push(Step::Travel(from));
                out.push(Step::Burn(p0));
                report.lead_ins += 1;
                added += options.lead_in_mm;
            }
            Some(_) => {
                out.push(Step::Travel(p0));
                report.leads_skipped += 1;
            }
            None => out.push(Step::Travel(p0)),
        }

        // 沿轮廓走：顶点、桥位边界与结束点为断点，桥位里关光滑行
        let n = ring.len() - 1;
        let mut marks: Vec<(f64, Point)> = Vec::with_capacity(2 * n + gaps.len() * 2 + 1);
        for lap in 0..2 {
            for (&at, &vertex) in geometry.cum[1..].iter().zip(&ring[1..]) {
                let pos = lap as f64 * perimeter + at;
                if pos < finish - 1e-9 {
                    marks.push((pos, vertex));
                }
            }
        }
        for &(g0, g1) in &gaps {
            for g in [g0, g1] {
                if g > 0.0 && g < finish {
                    marks.push((g, geometry.point_at(g)));
                }
            }
        }
        marks.push((finish, geometry.point_at(finish)));
        marks.sort_by(|a, b| a.0.total_cmp(&b.0));
        marks.dedup_by(|later, earlier| (later.0 - earlier.0).abs() < 1e-9);
        let (mut previous, mut walked, mut here) = (0.0, 0.0, p0);
        for &(pos, point) in &marks {
            let middle = (previous + pos) / 2.0;
            if gaps.iter().any(|&(g0, g1)| g0 <= middle && middle <= g1) {
                out.push(Step::Coast(point));
            } else {
                out.push(Step::Burn(point));
            }
            walked += distance(here, point);
            here = point;
            previous = pos;
        }
        if (walked - finish).abs() > 1e-6 * finish.max(1.0) {
            return Err(SafetyError("切割后处理改变了轮廓路径，拒绝生成 G-code"));
        }
        let gap_total: f64 = gaps
            .iter()
            .map(|&(g0, g1)| (g1.min(finish) - g0.max(0.0)).max(0.0))
            .sum();
        report.tabs += count;
        report.tab_gap_mm += gap_total;
        report.overcut_mm += overcut;
        added += overcut - gap_total;

        // 引出线：从结束点切向废料一侧
        if options.lead_out_mm > 0.0 {
            let (incoming, outgoing) = geometry.directions_at(finish);
            let normal = geometry.lead_normal(incoming, outgoing);
            let to = add(
                here,
                scale(
                    unit(add(scale(normal, sin), scale(incoming, cos))),
                    options.lead_out_mm,
                ),
            );
            if geometry.on_scrap_side(to) && !crosses_any(here, to, &rings, here) {
                out.push(Step::Burn(to));
                report.lead_outs += 1;
                added += options.lead_out_mm;
            } else {
                report.leads_skipped += 1;
            }
        }
    }
    out.extend_from_slice(&steps[cursor..]);

    // 自检：出光总长度 = 原来的 − 桥位 + 引线 + 过切
    let before = burn_length(&burn_paths(steps));
    let after = burn_length(&burn_paths(&out));
    if (after - (before + added)).abs() > 1e-6 * (before + added).max(1.0) {
        eprintln!("DEBUG before={before} after={after} added={added} out={out:?}");
        return Err(SafetyError(
            "切割后处理的出光长度与预期不符，拒绝生成 G-code",
        ));
    }
    Ok((out, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_plan::{PlanOptions, Unit, plan};

    fn square(x: f64, y: f64, size: f64, ccw: bool) -> Vec<Point> {
        let mut points = vec![
            (x, y),
            (x + size, y),
            (x + size, y + size),
            (x, y + size),
            (x, y),
        ];
        if !ccw {
            points.reverse();
        }
        points
    }

    fn bbox(path: &[Point]) -> (f64, f64, f64, f64) {
        path.iter().fold(
            (
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ),
            |(a, b, c, d), p| (a.min(p.0), b.min(p.1), c.max(p.0), d.max(p.1)),
        )
    }

    fn area(path: &[Point]) -> f64 {
        signed_area(&path[..path.len() - 1]).abs()
    }

    #[test]
    fn kerf_grows_and_shrinks_a_square() {
        let grown = offset_contours(&[square(0.0, 0.0, 10.0, true)], 0.5).unwrap();
        assert_eq!((grown.before, grown.after), (1, 1));
        let (x0, y0, x1, y1) = bbox(&grown.contours[0]);
        assert!(
            (x0 + 0.5).abs() < 1e-9 && (y0 + 0.5).abs() < 1e-9,
            "{x0} {y0}"
        );
        assert!((x1 - 10.5).abs() < 1e-9 && (y1 - 10.5).abs() < 1e-9);
        // 凸角补圆弧：面积 = 11² − 4 个角的 (1 − π/4)·0.25
        let expected = 11.0 * 11.0 - (1.0 - std::f64::consts::PI / 4.0);
        assert!((area(&grown.contours[0]) - expected).abs() < 0.01);
        // 每个点到原正方形的距离都正好是 0.5
        for p in &grown.contours[0] {
            let dx = (p.0 - 5.0).abs() - 5.0;
            let dy = (p.1 - 5.0).abs() - 5.0;
            let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0);
            assert!((d - 0.5).abs() < 0.006, "{p:?} → {d}");
        }

        // 方向反了（顺时针）也一样往外
        let cw = offset_contours(&[square(0.0, 0.0, 10.0, false)], 0.5).unwrap();
        assert!((area(&cw.contours[0]) - expected).abs() < 0.01);

        let shrunk = offset_contours(&[square(0.0, 0.0, 10.0, true)], -0.5).unwrap();
        let (x0, y0, x1, y1) = bbox(&shrunk.contours[0]);
        assert!((x0 - 0.5).abs() < 1e-9 && (y0 - 0.5).abs() < 1e-9);
        assert!((x1 - 9.5).abs() < 1e-9 && (y1 - 9.5).abs() < 1e-9);
        assert!(
            (area(&shrunk.contours[0]) - 81.0).abs() < 1e-6,
            "内缩保持尖角"
        );
        assert_eq!(shrunk.contours[0].first(), shrunk.contours[0].last());
    }

    #[test]
    fn holes_shrink_when_the_part_grows() {
        // “口”：外框 10 mm，孔 4 mm（字体里孔与外框方向相反）
        let part = vec![square(0.0, 0.0, 10.0, true), square(3.0, 3.0, 4.0, false)];
        let grown = offset_contours(&part, 0.25).unwrap();
        assert_eq!((grown.before, grown.after), (2, 2));
        let mut sizes: Vec<f64> = grown
            .contours
            .iter()
            .map(|c| {
                let (x0, _, x1, _) = bbox(c);
                x1 - x0
            })
            .collect();
        sizes.sort_by(f64::total_cmp);
        assert!((sizes[0] - 3.5).abs() < 1e-9, "孔变小：{sizes:?}");
        assert!((sizes[1] - 10.5).abs() < 1e-9, "外框变大：{sizes:?}");
        // 内缩：孔变大
        let shrunk = offset_contours(&part, -0.25).unwrap();
        let hole = shrunk
            .contours
            .iter()
            .map(|c| bbox(c))
            .map(|(x0, _, x1, _)| x1 - x0)
            .fold(f64::INFINITY, f64::min);
        assert!((hole - 4.5).abs() < 0.01, "{hole}");
    }

    #[test]
    fn vanishing_and_overlapping_shapes_are_handled() {
        // 1 mm 的小方块内缩 0.6 mm 会消失：丢掉，不输出翻折的路径
        let shapes = vec![square(0.0, 0.0, 10.0, true), square(20.0, 0.0, 1.0, true)];
        let result = offset_contours(&shapes, -0.6).unwrap();
        assert_eq!((result.before, result.after), (2, 1));
        assert!(bbox(&result.contours[0]).2 < 10.0);
        // 全部消失：明确报错
        assert!(offset_contours(&[square(0.0, 0.0, 1.0, true)], -0.6).is_err());
        // 孔被外扩吃掉：只剩外框
        let thin_hole = vec![square(0.0, 0.0, 10.0, true), square(4.0, 4.0, 0.4, false)];
        assert_eq!(offset_contours(&thin_hole, 0.3).unwrap().after, 1);
        // 两个重叠的笔画先合成一个外轮廓再补偿
        let overlap = vec![square(0.0, 0.0, 4.0, true), square(2.0, 0.0, 4.0, true)];
        let merged = offset_contours(&overlap, 0.1).unwrap();
        assert_eq!((merged.before, merged.after), (1, 1));
        let (x0, _, x1, _) = bbox(&merged.contours[0]);
        assert!((x1 - x0 - 6.2).abs() < 1e-9);
        // 两个靠得很近的方块外扩后相连成一个（轮廓数变少会回显给用户）
        let close = vec![square(0.0, 0.0, 4.0, true), square(4.3, 0.0, 4.0, true)];
        assert_eq!(offset_contours(&close, 0.2).unwrap().after, 1);
        // 自交的“8”字（领结）按非零环绕补偿成两块
        let bowtie = vec![vec![(0.0, 0.0), (4.0, 4.0), (4.0, 0.0), (0.0, 4.0)]];
        let tie = offset_contours(&bowtie, -0.1).unwrap();
        assert_eq!(tie.after, 2);
        // 无效输入
        assert!(offset_contours(&[square(0.0, 0.0, 4.0, true)], 3.0).is_err());
        assert!(offset_contours(&[vec![(0.0, 0.0), (1.0, 0.0)]], 0.1).is_err());
        assert!(offset_contours(&[vec![(f64::NAN, 0.0), (1.0, 0.0), (1.0, 1.0)]], 0.1).is_err());
    }

    fn planned(units: Vec<Unit>) -> Vec<Step> {
        plan(
            units,
            (0.0, 0.0),
            &PlanOptions {
                inside_first: true,
                prefer_corners: true,
                remove_overlaps: true,
            },
        )
        .unwrap()
        .steps
    }

    #[test]
    fn tabs_leave_laser_off_gaps_of_the_right_width() {
        let steps = planned(vec![Unit::Closed(square(0.0, 0.0, 10.0, true))]);
        let options = CutOptions {
            tabs: TabMode::Count(4),
            tab_width_mm: 1.0,
            ..CutOptions::default()
        };
        let (out, report) = apply_cut_options(&steps, &options).unwrap();
        assert_eq!(report.tabs, 4);
        assert!((report.tab_gap_mm - 4.0).abs() < 1e-9);
        // 4 段关光滑行，每段 1 mm，都在轮廓上
        let mut gaps = Vec::new();
        for pair in out.windows(2) {
            if let Step::Coast(b) = pair[1] {
                gaps.push(distance(step_point(&pair[0]), b));
            }
        }
        assert_eq!(gaps.len(), 4, "{out:?}");
        assert!(gaps.iter().all(|g| (g - 1.0).abs() < 1e-9), "{gaps:?}");
        // 桥位居中在每条边中点（周长 40，起点在角上，间距 10）
        let paths = burn_paths(&out);
        assert!((burn_length(&paths) - 36.0).abs() < 1e-9);
        // G-code 里桥位是 G1 … S0
        let gcode = crate::gcode::write_steps(
            &out,
            &crate::gcode::WriterOptions {
                width_mm: 10.0,
                height_mm: 10.0,
                power: 500,
                feed_rate: 600,
                auto_close: false,
                laser_mode: "M4",
                power_scale: None,
            },
        )
        .unwrap();
        assert_eq!(
            gcode
                .lines()
                .filter(|l| l.starts_with("G1 ") && l.contains(" S0"))
                .count(),
            4
        );

        // 按间距：周长 40、间距 15 → 3 个
        let spaced = CutOptions {
            tabs: TabMode::Spacing(15.0),
            ..options
        };
        assert_eq!(apply_cut_options(&steps, &spaced).unwrap().1.tabs, 3);
        // 桥位太宽放不下：这个轮廓不加
        let wide = CutOptions {
            tabs: TabMode::Count(30),
            tab_width_mm: 10.0,
            ..options
        };
        let (_, r) = apply_cut_options(&steps, &wide).unwrap();
        assert_eq!(r.tabs, 2, "总宽不超过周长一半：{r:?}");
    }

    #[test]
    fn tabs_skip_inner_shapes_when_asked() {
        let steps = planned(vec![
            Unit::Closed(square(0.0, 0.0, 20.0, true)),
            Unit::Closed(square(5.0, 5.0, 5.0, false)),
        ]);
        let options = CutOptions {
            tabs: TabMode::Count(2),
            tab_width_mm: 0.5,
            ..CutOptions::default()
        };
        assert_eq!(apply_cut_options(&steps, &options).unwrap().1.tabs, 2);
        let all = CutOptions {
            tabs_skip_inner: false,
            ..options
        };
        assert_eq!(apply_cut_options(&steps, &all).unwrap().1.tabs, 4);
    }

    #[test]
    fn lead_in_lead_out_and_overcut_geometry() {
        let steps = planned(vec![
            Unit::Closed(square(0.0, 0.0, 20.0, true)),
            Unit::Closed(square(5.0, 5.0, 6.0, true)),
        ]);
        let options = CutOptions {
            lead_in_mm: 2.0,
            lead_out_mm: 1.0,
            overcut_mm: 3.0,
            ..CutOptions::default()
        };
        let (out, report) = apply_cut_options(&steps, &options).unwrap();
        assert_eq!(
            (report.lead_ins, report.lead_outs, report.leads_skipped),
            (2, 2, 0)
        );
        assert!((report.overcut_mm - 6.0).abs() < 1e-9);
        let loops: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, Step::Travel(_)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(loops.len(), 2);
        // 先切的是内孔（先内后外）：引入线从孔里垂直切进起刀点
        let (from, start) = (step_point(&out[loops[0]]), step_point(&out[loops[0] + 1]));
        assert!((distance(from, start) - 2.0).abs() < 1e-9);
        assert!(
            point_in_polygon(from, &square(5.0, 5.0, 6.0, true)),
            "{from:?}"
        );
        // 外框的引入线在外面
        let (from, start) = (step_point(&out[loops[1]]), step_point(&out[loops[1] + 1]));
        assert!(
            !point_in_polygon(from, &square(0.0, 0.0, 20.0, true)),
            "{from:?}"
        );
        assert!((distance(from, start) - 2.0).abs() < 1e-9);
        // 过切：回到起点后又沿第一条边切了 3 mm，再切出 1 mm 引出线
        let last = step_point(&out[out.len() - 1]);
        let before_last = step_point(&out[out.len() - 2]);
        assert!(
            (distance(before_last, start) - 3.0).abs() < 1e-9,
            "{before_last:?} {start:?}"
        );
        assert!((distance(last, before_last) - 1.0).abs() < 1e-9);
        assert!(!point_in_polygon(last, &square(0.0, 0.0, 20.0, true)));
        let length = burn_length(&burn_paths(&out));
        assert!((length - (80.0 + 24.0 + 2.0 * (2.0 + 1.0 + 3.0))).abs() < 1e-9);

        // 斜着引入（30°）：长度不变，从起刀边后方斜着切进来；孔的角上斜线会落到零件一侧，这一条不加
        let angled = CutOptions {
            lead_angle_deg: 30.0,
            ..options
        };
        let (out, report) = apply_cut_options(&steps, &angled).unwrap();
        assert_eq!(
            (report.lead_ins, report.leads_skipped),
            (1, 1),
            "{report:?}"
        );
        let t = out
            .iter()
            .rposition(|s| matches!(s, Step::Travel(_)))
            .unwrap();
        let (from, start, next) = (
            step_point(&out[t]),
            step_point(&out[t + 1]),
            step_point(&out[t + 2]),
        );
        let dir = unit(sub(next, start));
        assert!(dot(unit(sub(start, from)), dir) > 0.9, "{from:?}");
        assert!((distance(from, start) - 2.0).abs() < 1e-9);
        assert!(!point_in_polygon(from, &square(0.0, 0.0, 20.0, true)));
    }

    #[test]
    fn leads_that_would_hit_another_contour_are_skipped() {
        // 孔只有 1 mm 宽，3 mm 的引入线会切到对面的孔壁：不加
        let steps = planned(vec![
            Unit::Closed(square(0.0, 0.0, 20.0, true)),
            Unit::Closed(square(5.0, 5.0, 1.0, true)),
        ]);
        let options = CutOptions {
            lead_in_mm: 3.0,
            ..CutOptions::default()
        };
        let (_, report) = apply_cut_options(&steps, &options).unwrap();
        assert_eq!((report.lead_ins, report.leads_skipped), (1, 1));
    }

    #[test]
    fn overcut_does_not_burn_through_a_tab_and_other_steps_are_untouched() {
        let steps = planned(vec![
            Unit::Closed(square(0.0, 0.0, 4.0, true)),
            Unit::Open(vec![(10.0, 0.0), (15.0, 0.0)]),
        ]);
        // 周长 16、1 个桥位在 7.75 ~ 8.25：过切 8 mm 会落进第二圈的桥位（23.75 起），停在桥位前
        let options = CutOptions {
            tabs: TabMode::Count(1),
            tab_width_mm: 0.5,
            overcut_mm: 8.0,
            ..CutOptions::default()
        };
        let (out, report) = apply_cut_options(&steps, &options).unwrap();
        assert!((report.overcut_mm - 7.75).abs() < 1e-9, "{report:?}");
        // 开放线段照原样保留
        assert!(out.contains(&Step::Burn((15.0, 0.0))) || out.contains(&Step::Burn((10.0, 0.0))));
        assert!(CutOptions::default().validate().is_ok());
        assert!(!CutOptions::default().is_active());
        assert!(
            CutOptions {
                lead_angle_deg: 0.0,
                lead_in_mm: 1.0,
                ..CutOptions::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            CutOptions {
                tabs: TabMode::Count(0),
                ..CutOptions::default()
            }
            .validate()
            .is_err()
        );
    }
}
