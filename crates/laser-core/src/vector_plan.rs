//! 矢量刀路规划（2026-11-16 用户决定：文字与画板一律“转矢量 → 规划路线 → G-code”；
//! 同日修订：填充与走刀顺序对齐 LightBurn 的 Fill 模式与优化设置）。
//!
//! - **矢量扫描填充** [`scan_fill`]：按非零环绕 / 奇偶规则，逐行求扫描线与轮廓的精确交段（不经过位图，
//!   边缘没有像素锯齿；“口”“回”这类字的内孔不填）。选项对齐 LightBurn Fill 模式：
//!   - 线距 `spacing`；扫描角度 `angle_deg`（0 = 横扫）；交叉排线 `cross_hatch`（再转 90° 扫一遍）；
//!   - 双向 / 单向扫描 `bidirectional`（单向时回程不出光）；
//!   - **过扫描** `overscan_mm`：每行两头各多走一段、全程 `S0` 关光，让激光头在出光前已到速、
//!     出光结束后才减速，避免线头线尾烧深（GRBL / FluidNC 不会自动做，DSP 控制器才在硬件里做）；
//!   - 分组 [`FillGrouping`]：`AllAtOnce`（默认，同 LightBurn“一次填完所有形状”）每行横穿所有图形，
//!     图形之间关光匀速滑过；`Individually`（同“逐个形状填充”）按区块一块块扫完，少走空白；
//!   - 双向扫描偏移 `scan_offset_mm`：双向时来回两行出光错位的补偿（每行逆着自己的行进方向挪一半）；
//!     错位随速度变化时可给“速度 → 偏移”表 [`scan_offset_at`]（同 LightBurn Scanning Offset Adjustment，
//!     按加工速度线性插值、两端取端点值）。
//! - **路线规划** [`plan`] / [`plan_layers`]：把轮廓（闭合，可从任一顶点起刀）、开放线段（可正反走）与
//!   扫描区块（可从首行或末行开始）排成空走最短的顺序：最近邻贪心 + 2-opt，最后按新顺序重挑起刀点。
//!   选项对齐 LightBurn 优化设置：内部形状先切 `inside_first`（硬约束，不会被“减少空程”打乱）、
//!   起刀点尽量放在尖角 `prefer_corners`（起刀焦点不落在平滑曲线中间）、去掉重复线 `remove_overlaps`。
//!   规划只改顺序与方向，结束前自检出光总长度与输入一致，不一致直接拒绝。
//!
//! 输出 [`Planned::steps`] 交给 [`crate::gcode::write_steps`]：`Travel` = `G0` 空走，`Coast` = `G1 … S0`
//! 匀速关光滑行（过扫描、图形之间），`Burn` = `G1 … S功率` 出光。

use crate::gcode::Step;
use crate::safety::SafetyError;
use laser_contracts::geometry::{PathOptimizationStats, Paths, Point};

/// 单次填充的出光线段上限：再多说明尺寸或线距不合理（100 mm 见方、0.05 mm 线距约 2000 行）。
pub const MAX_FILL_SEGMENTS: usize = 400_000;
/// 自动过扫描默认假设的加速度（mm/s²）；请求可传机器实际值（GRBL `$120/$121`）。
pub use crate::gcode::DEFAULT_ACCELERATION_MM_S2;
/// 自动过扫描下限与上限（mm）。
pub const MIN_AUTO_OVERSCAN_MM: f64 = 1.0;
pub const MAX_OVERSCAN_MM: f64 = 20.0;
/// 去重复线的距离容差（mm）。
pub const OVERLAP_TOLERANCE_MM: f64 = 0.01;
/// 2-opt 只在单元数不太多时做（O(n²) 每轮），再多只用贪心结果。
const TWO_OPT_MAX_UNITS: usize = 2_500;
const TWO_OPT_MAX_PASSES: usize = 40;
/// 拐角判定：前后两条边方向变化超过这个角度（度）算尖角。
const CORNER_TURN_DEG: f64 = 45.0;
const EPS: f64 = 1e-9;

/// 按速度与加速度算过扫描距离：加速到 `feed` 需要 v²/(2a)，限制在 [1, 20] mm，取 0.1 mm 整。
pub fn auto_overscan_mm(feed_mm_per_min: f64, acceleration_mm_s2: f64) -> f64 {
    let accel = if acceleration_mm_s2.is_finite() && acceleration_mm_s2 > 0.0 {
        acceleration_mm_s2
    } else {
        DEFAULT_ACCELERATION_MM_S2
    };
    let v = feed_mm_per_min.max(0.0) / 60.0;
    let d = v * v / (2.0 * accel);
    ((d * 10.0).ceil() / 10.0).clamp(MIN_AUTO_OVERSCAN_MM, MAX_OVERSCAN_MM)
}

/// 速度 → 偏移表最多几行。
pub const MAX_SCAN_OFFSET_ROWS: usize = 32;
/// 扫描偏移上限（mm）。
pub const MAX_SCAN_OFFSET_MM: f64 = 2.0;

/// 按“速度（mm/min）→ 双向扫描偏移（mm）”表在 `feed_mm_per_min` 处线性插值（2026-11-16，对齐 LightBurn
/// Scanning Offset Adjustment）：速度须严格递增（1 ~ 32 行），偏移 ±2 mm；低于首行或高于末行取端点值。
pub fn scan_offset_at(table: &[(f64, f64)], feed_mm_per_min: f64) -> Result<f64, SafetyError> {
    if table.is_empty() || table.len() > MAX_SCAN_OFFSET_ROWS {
        return Err(SafetyError("扫描偏移表须有 1 ~ 32 行"));
    }
    let valid_rows = table.iter().all(|&(speed, offset)| {
        speed.is_finite() && speed > 0.0 && offset.is_finite() && offset.abs() <= MAX_SCAN_OFFSET_MM
    });
    if !valid_rows || table.windows(2).any(|w| w[1].0 <= w[0].0) {
        return Err(SafetyError(
            "扫描偏移表的速度须为正数且从小到大不重复，偏移须在 -2 ~ 2 mm",
        ));
    }
    if !feed_mm_per_min.is_finite() {
        return Err(SafetyError("加工速度无效"));
    }
    let (first, last) = (table[0], table[table.len() - 1]);
    if feed_mm_per_min <= first.0 {
        return Ok(first.1);
    }
    if feed_mm_per_min >= last.0 {
        return Ok(last.1);
    }
    let k = table.partition_point(|row| row.0 <= feed_mm_per_min);
    let ((s0, o0), (s1, o1)) = (table[k - 1], table[k]);
    Ok(o0 + (o1 - o0) * (feed_mm_per_min - s0) / (s1 - s0))
}

/// 填充时怎么判断“在图形里面”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    /// 非零环绕（字体：内孔方向相反，所以不填）
    NonZero,
    /// 奇偶（画板图形各自独立填充时用）
    EvenOdd,
}

/// 填充分组（对齐 LightBurn Fill Grouping）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillGrouping {
    /// 一次填完：每行横穿所有图形，图形之间关光匀速滑过（LightBurn 默认，速度均匀）
    AllAtOnce,
    /// 逐块填充：每一笔 / 每一块单独蛇形扫完再去下一块（空白多、机器加速快时更省时）
    Individually,
}

impl FillGrouping {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None | Some("all") => Ok(Self::AllAtOnce),
            Some("individual") => Ok(Self::Individually),
            Some(other) => Err(format!(
                "fill_grouping 只能是 all / individual，收到 {other}"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllAtOnce => "all",
            Self::Individually => "individual",
        }
    }
}

/// 扫描填充选项（见模块说明）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FillOptions {
    pub spacing: f64,
    pub rule: FillRule,
    pub grouping: FillGrouping,
    /// 扫描方向与 +x 轴的夹角（度，按调用方坐标系逆时针）
    pub angle_deg: f64,
    pub cross_hatch: bool,
    pub bidirectional: bool,
    pub overscan_mm: f64,
    pub scan_offset_mm: f64,
}

impl FillOptions {
    /// 默认：横扫、一次填完、双向、不过扫描、无偏移。
    pub fn new(spacing: f64, rule: FillRule) -> Self {
        Self {
            spacing,
            rule,
            grouping: FillGrouping::AllAtOnce,
            angle_deg: 0.0,
            cross_hatch: false,
            bidirectional: true,
            overscan_mm: 0.0,
            scan_offset_mm: 0.0,
        }
    }

    /// 过扫描与偏移让扫描线越出图形的最大距离（x, y 分量，mm）：排版留边与幅面检查用。
    pub fn extension_mm(&self) -> (f64, f64) {
        let reach = self.overscan_mm + self.scan_offset_mm.abs() / 2.0;
        let mut angles = vec![self.angle_deg];
        if self.cross_hatch {
            angles.push(self.angle_deg + 90.0);
        }
        angles.iter().fold((0.0_f64, 0.0_f64), |(x, y), angle| {
            let (sin, cos) = angle.to_radians().sin_cos();
            (x.max(reach * cos.abs()), y.max(reach * sin.abs()))
        })
    }
}

/// 一行扫描：扫描线在局部坐标 `v` 处，与图形的交段（局部 `u`，从小到大）。
#[derive(Debug, Clone, PartialEq)]
struct ScanRow {
    v: f64,
    spans: Vec<(f64, f64)>,
}

/// 一个扫描区块：若干相邻行，按蛇形（双向）或同向（单向）走完。
#[derive(Debug, Clone, PartialEq)]
pub struct ScanBlock {
    rows: Vec<ScanRow>,
    cos: f64,
    sin: f64,
    bidirectional: bool,
    overscan: f64,
    offset: f64,
}

impl ScanBlock {
    fn world(&self, u: f64, v: f64) -> Point {
        (u * self.cos - v * self.sin, u * self.sin + v * self.cos)
    }

    /// 行数与出光段数。
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    pub fn spans(&self) -> usize {
        self.rows.iter().map(|row| row.spans.len()).sum()
    }

    fn burn_length(&self) -> f64 {
        self.rows
            .iter()
            .flat_map(|row| row.spans.iter())
            .map(|(a, b)| b - a)
            .sum()
    }

    /// 按 `reverse` 走完整块的步骤（整块倒着走时，双向扫描是原路完全反过来）。
    fn emit(&self, reverse: bool, out: &mut Vec<Step>) {
        let n = self.rows.len();
        for k in 0..n {
            let i = if reverse { n - 1 - k } else { k };
            let row = &self.rows[i];
            let natural = !self.bidirectional || i % 2 == 0;
            let forward = if reverse && self.bidirectional {
                !natural
            } else {
                natural
            };
            // 双向时每行逆着自己的行进方向挪半个偏移量：来回两行的出光错位正好抵消
            let shift = if self.bidirectional {
                if forward {
                    -self.offset / 2.0
                } else {
                    self.offset / 2.0
                }
            } else {
                0.0
            };
            let at = |u: f64| self.world(u + shift, row.v);
            let spans: Vec<(f64, f64)> = if forward {
                row.spans.clone()
            } else {
                row.spans.iter().rev().map(|&(a, b)| (b, a)).collect()
            };
            let sign = if forward { 1.0 } else { -1.0 };
            let (first, last) = (spans[0].0, spans[spans.len() - 1].1);
            if self.overscan > 0.0 {
                out.push(Step::Travel(at(first - sign * self.overscan)));
                out.push(Step::Coast(at(first)));
            } else {
                out.push(Step::Travel(at(first)));
            }
            for (index, &(a, b)) in spans.iter().enumerate() {
                if index > 0 {
                    out.push(Step::Coast(at(a)));
                }
                out.push(Step::Burn(at(b)));
            }
            if self.overscan > 0.0 {
                out.push(Step::Coast(at(last + sign * self.overscan)));
            }
        }
    }
}

/// 一个可整体排序的加工单元。
#[derive(Debug, Clone, PartialEq)]
pub enum Unit {
    /// 闭合轮廓（首尾点相同，或会被补成相同）：可从任一顶点起刀
    Closed(Vec<Point>),
    /// 开放线段（手绘笔画、直线）：可正走或反走，不闭合
    Open(Vec<Point>),
    /// 扫描填充区块：可从首行或末行开始
    Scan(ScanBlock),
}

fn distance(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn finite(points: &[Point]) -> bool {
    points.iter().all(|p| p.0.is_finite() && p.1.is_finite())
}

/// 补成首尾相同的闭合轮廓；少于 3 个不同顶点返回 `None`。
pub fn close_contour(points: &[Point]) -> Option<Vec<Point>> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len() + 1);
    for &p in points {
        if out.last().is_none_or(|last| distance(*last, p) > EPS) {
            out.push(p);
        }
    }
    if out.len() >= 2 && distance(out[0], out[out.len() - 1]) <= EPS {
        out.pop();
    }
    if out.len() < 3 {
        return None;
    }
    out.push(out[0]);
    Some(out)
}

/// 一条扫描线 `v` 与（已转到局部坐标的）轮廓的交段，从小到大。
fn scanline_spans(contours: &[Vec<Point>], v: f64, rule: FillRule) -> Vec<(f64, f64)> {
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for path in contours {
        for pair in path.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if (a.1 <= v && b.1 > v) || (b.1 <= v && a.1 > v) {
                let t = (v - a.1) / (b.1 - a.1);
                crossings.push((a.0 + t * (b.0 - a.0), if b.1 > a.1 { 1 } else { -1 }));
            }
        }
    }
    crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
    let inside = |winding: i32| match rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding % 2 != 0,
    };
    let mut spans = Vec::new();
    let mut winding = 0;
    let mut start = 0.0;
    for (u, dir) in crossings {
        let was_inside = inside(winding);
        winding += match rule {
            FillRule::NonZero => dir,
            FillRule::EvenOdd => 1,
        };
        if !was_inside && inside(winding) {
            start = u;
        } else if was_inside && !inside(winding) && u > start + EPS {
            spans.push((start, u));
        }
    }
    spans
}

/// 一个扫描角度下的所有行（局部坐标）。
fn scan_rows(
    contours: &[Vec<Point>],
    options: &FillOptions,
    cos: f64,
    sin: f64,
) -> Result<Vec<ScanRow>, SafetyError> {
    // 转到扫描方向为 +u 的局部坐标：u = x·cos + y·sin，v = −x·sin + y·cos
    let local: Vec<Vec<Point>> = contours
        .iter()
        .map(|path| {
            path.iter()
                .map(|&(x, y)| (x * cos + y * sin, -x * sin + y * cos))
                .collect()
        })
        .collect();
    let (low, high) = local
        .iter()
        .flatten()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.1), hi.max(p.1))
        });
    if !low.is_finite() || !high.is_finite() || high - low <= EPS {
        return Ok(Vec::new());
    }
    let count = ((high - low) / options.spacing).ceil() as usize;
    if count > MAX_FILL_SEGMENTS {
        return Err(SafetyError("填充行数过多，请加大线距或缩小尺寸"));
    }
    let mut rows = Vec::new();
    let mut total = 0usize;
    for row in 0..count {
        let v = low + options.spacing * (row as f64 + 0.5);
        if v >= high {
            break;
        }
        let spans = scanline_spans(&local, v, options.rule);
        total += spans.len();
        if total > MAX_FILL_SEGMENTS {
            return Err(SafetyError("填充线段过多，请加大线距或缩小尺寸"));
        }
        rows.push(ScanRow { v, spans });
    }
    Ok(rows)
}

/// 把逐行交段连成一块块竖向区块：上下相邻、彼此只与对方重叠的交段接成一块（字的每一笔各成一块）。
fn split_blocks(rows: Vec<ScanRow>) -> Vec<Vec<ScanRow>> {
    let overlaps = |a: (f64, f64), b: (f64, f64)| a.0 < b.1 - EPS && b.0 < a.1 - EPS;
    let mut finished: Vec<Vec<ScanRow>> = Vec::new();
    // 进行中的区块与它最后一行的交段
    let mut active: Vec<(Vec<ScanRow>, (f64, f64))> = Vec::new();
    for row in rows {
        let spans = row.spans;
        let down: Vec<Vec<usize>> = active
            .iter()
            .map(|(_, last)| {
                (0..spans.len())
                    .filter(|&j| overlaps(*last, spans[j]))
                    .collect()
            })
            .collect();
        let up: Vec<usize> = (0..spans.len())
            .map(|j| down.iter().filter(|targets| targets.contains(&j)).count())
            .collect();
        let mut claimed = vec![false; spans.len()];
        let mut next: Vec<(Vec<ScanRow>, (f64, f64))> = Vec::new();
        for ((mut block, _), targets) in active.into_iter().zip(down) {
            match targets.as_slice() {
                [j] if up[*j] == 1 => {
                    claimed[*j] = true;
                    block.push(ScanRow {
                        v: row.v,
                        spans: vec![spans[*j]],
                    });
                    next.push((block, spans[*j]));
                }
                _ => finished.push(block),
            }
        }
        for (j, span) in spans.iter().enumerate() {
            if !claimed[j] {
                next.push((
                    vec![ScanRow {
                        v: row.v,
                        spans: vec![*span],
                    }],
                    *span,
                ));
            }
        }
        active = next;
    }
    finished.extend(active.into_iter().map(|(block, _)| block));
    finished
}

/// 矢量扫描填充（见模块说明）。轮廓会先补成闭合折线。
pub fn scan_fill(contours: &[Vec<Point>], options: &FillOptions) -> Result<Vec<Unit>, SafetyError> {
    if !options.spacing.is_finite() || options.spacing <= 0.0 {
        return Err(SafetyError("填充线距必须是大于 0 的数字"));
    }
    if !options.angle_deg.is_finite()
        || !options.overscan_mm.is_finite()
        || !(0.0..=MAX_OVERSCAN_MM).contains(&options.overscan_mm)
        || !options.scan_offset_mm.is_finite()
        || options.scan_offset_mm.abs() > 2.0
    {
        return Err(SafetyError(
            "扫描角度、过扫描（0 ~ 20 mm）或扫描偏移（±2 mm）无效",
        ));
    }
    if contours.iter().any(|path| !finite(path)) {
        return Err(SafetyError("填充轮廓含有无效坐标"));
    }
    // 扫描只看相邻两点组成的边：没有首尾相同的轮廓先补上闭合边，否则交点成单、整片漏填
    let closed: Vec<Vec<Point>> = contours
        .iter()
        .filter_map(|path| close_contour(path))
        .collect();
    let mut angles = vec![options.angle_deg];
    if options.cross_hatch {
        angles.push(options.angle_deg + 90.0);
    }
    let mut units = Vec::new();
    let mut total = 0usize;
    for angle in angles {
        let (sin, cos) = angle.to_radians().sin_cos();
        let rows: Vec<ScanRow> = scan_rows(&closed, options, cos, sin)?
            .into_iter()
            .filter(|row| !row.spans.is_empty())
            .collect();
        total += rows.iter().map(|row| row.spans.len()).sum::<usize>();
        if total > MAX_FILL_SEGMENTS {
            return Err(SafetyError("填充线段过多，请加大线距或缩小尺寸"));
        }
        let blocks = match options.grouping {
            FillGrouping::AllAtOnce if !rows.is_empty() => vec![rows],
            FillGrouping::AllAtOnce => Vec::new(),
            FillGrouping::Individually => split_blocks(rows),
        };
        units.extend(blocks.into_iter().map(|rows| {
            Unit::Scan(ScanBlock {
                rows,
                cos,
                sin,
                bidirectional: options.bidirectional,
                overscan: options.overscan_mm,
                offset: options.scan_offset_mm,
            })
        }));
    }
    Ok(units)
}

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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn fill(spacing: f64) -> FillOptions {
        FillOptions::new(spacing, FillRule::NonZero)
    }

    fn individually(spacing: f64) -> FillOptions {
        FillOptions {
            grouping: FillGrouping::Individually,
            ..fill(spacing)
        }
    }

    fn block(unit: &Unit) -> &ScanBlock {
        match unit {
            Unit::Scan(block) => block,
            other => panic!("{other:?}"),
        }
    }

    fn point(step: &Step) -> Point {
        match *step {
            Step::Travel(p) | Step::Coast(p) | Step::Burn(p) => p,
        }
    }

    #[test]
    fn holes_are_not_filled_under_the_nonzero_rule() {
        // “口”：外框逆时针、内孔顺时针
        let contours = vec![square(0.0, 0.0, 10.0, true), square(3.0, 3.0, 4.0, false)];
        let units = scan_fill(&contours, &individually(1.0)).unwrap();
        let planned = plan(units.clone(), (0.0, 0.0), &PlanOptions::default()).unwrap();
        for path in &planned.paths {
            let (a, b) = (path[0], path[path.len() - 1]);
            if a.1 > 3.0 && a.1 < 7.0 {
                let (lo, hi) = (a.0.min(b.0), a.0.max(b.0));
                assert!(hi <= 3.0 + 1e-9 || lo >= 7.0 - 1e-9, "{a:?} {b:?}");
            }
        }
        // 孔把中间几行分成左右两块，上下各一块：共 4 块
        assert_eq!(units.len(), 4, "{units:?}");
        assert_eq!(
            units.iter().map(|u| block(u).rows()).sum::<usize>(),
            3 + 3 + 4 + 4
        );
        // 一次填完：一整块，孔所在的行每行两段
        let all = scan_fill(&contours, &fill(1.0)).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(block(&all[0]).rows(), 10);
        assert_eq!(block(&all[0]).spans(), 14);
    }

    #[test]
    fn contours_without_a_repeated_end_point_are_still_filled() {
        // 字体轮廓常不重复首点：漏掉闭合边会交点成单、整片漏填
        let open_square = vec![vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)]];
        let units = scan_fill(&open_square, &fill(1.0)).unwrap();
        assert_eq!(block(&units[0]).spans(), 5);
    }

    #[test]
    fn individual_blocks_are_finished_before_moving_to_the_next() {
        // 两个分开的方块（两个字）：各成一块，两块之间只空走一次
        let contours = vec![square(0.0, 0.0, 5.0, true), square(20.0, 0.0, 5.0, true)];
        let units = scan_fill(&contours, &individually(0.5)).unwrap();
        assert_eq!(units.len(), 2);
        let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
        let long_travels = planned
            .steps
            .windows(2)
            .filter(|w| {
                matches!(w[1], Step::Travel(_)) && distance(point(&w[0]), point(&w[1])) > 1.0
            })
            .count();
        assert_eq!(long_travels, 1, "{:?}", planned.steps);
    }

    #[test]
    fn fill_all_at_once_sweeps_each_row_across_every_shape_with_overscan() {
        // LightBurn 默认：每行横穿两个方块，中间关光匀速滑过；两头各多走 2 mm 关光
        let contours = vec![square(0.0, 0.0, 5.0, true), square(20.0, 0.0, 5.0, true)];
        let options = FillOptions {
            overscan_mm: 2.0,
            ..fill(1.0)
        };
        let units = scan_fill(&contours, &options).unwrap();
        assert_eq!(units.len(), 1);
        let planned = plan(units, (-2.0, 0.0), &PlanOptions::default()).unwrap();
        // 每行：空走到 -2 → 滑到 0 → 出光到 5 → 滑到 20 → 出光到 25 → 滑到 27
        let first_row: Vec<Step> = planned.steps[..6].to_vec();
        assert_eq!(
            first_row,
            vec![
                Step::Travel((-2.0, 0.5)),
                Step::Coast((0.0, 0.5)),
                Step::Burn((5.0, 0.5)),
                Step::Coast((20.0, 0.5)),
                Step::Burn((25.0, 0.5)),
                Step::Coast((27.0, 0.5)),
            ]
        );
        // 下一行反向（双向蛇形），从右侧过扫描处开始
        assert_eq!(planned.steps[6], Step::Travel((27.0, 1.5)));
        assert_eq!(planned.steps[7], Step::Coast((25.0, 1.5)));
        // 出光长度不受过扫描影响
        assert!((burn_length(&planned.paths) - 5.0 * 10.0).abs() < 1e-9);
        assert_eq!(options.extension_mm(), (2.0, 0.0));
    }

    #[test]
    fn unidirectional_scans_every_row_the_same_way() {
        let options = FillOptions {
            bidirectional: false,
            ..fill(1.0)
        };
        let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &options).unwrap();
        let planned = plan(units, (10.0, 10.0), &PlanOptions::default()).unwrap();
        for pair in planned.steps.chunks(2) {
            let (Step::Travel(a), Step::Burn(b)) = (pair[0], pair[1]) else {
                panic!("{pair:?}")
            };
            assert!(b.0 > a.0, "每行都从左往右出光：{a:?} → {b:?}");
        }
    }

    #[test]
    fn scan_offset_shifts_forward_and_return_rows_against_their_travel() {
        let options = FillOptions {
            scan_offset_mm: 0.2,
            ..fill(1.0)
        };
        let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &options).unwrap();
        let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
        // 第一行往右：整体左挪 0.1；第二行往左：整体右挪 0.1
        let expected = [(-0.1, 0.5), (3.9, 0.5), (4.1, 1.5), (0.1, 1.5)];
        for (step, want) in planned.steps.iter().zip(expected) {
            assert!(distance(point(step), want) < 1e-9, "{step:?} vs {want:?}");
        }
        assert!(matches!(planned.steps[2], Step::Travel(_)));
        assert!(matches!(planned.steps[3], Step::Burn(_)));
    }

    #[test]
    fn scan_offset_table_interpolates_at_the_job_speed() {
        let table = [(1000.0, 0.05), (3000.0, 0.15), (6000.0, 0.3)];
        assert!((scan_offset_at(&table, 2000.0).unwrap() - 0.1).abs() < 1e-12);
        assert!((scan_offset_at(&table, 4500.0).unwrap() - 0.225).abs() < 1e-12);
        assert_eq!(scan_offset_at(&table, 3000.0).unwrap(), 0.15);
        // 两端取端点值
        assert_eq!(scan_offset_at(&table, 500.0).unwrap(), 0.05);
        assert_eq!(scan_offset_at(&table, 9000.0).unwrap(), 0.3);
        assert_eq!(scan_offset_at(&[(1500.0, -0.1)], 99.0).unwrap(), -0.1);
        // 无效表
        assert!(scan_offset_at(&[], 1000.0).is_err());
        assert!(scan_offset_at(&[(2000.0, 0.1), (1000.0, 0.2)], 1500.0).is_err());
        assert!(scan_offset_at(&[(1000.0, 0.1), (1000.0, 0.2)], 1500.0).is_err());
        assert!(scan_offset_at(&[(1000.0, 3.0)], 1500.0).is_err());
        assert!(scan_offset_at(&[(0.0, 0.1)], 1500.0).is_err());
        assert!(scan_offset_at(&vec![(1.0, 0.0); 33], 1.0).is_err());
    }

    #[test]
    fn scan_angle_and_cross_hatch_rotate_the_lines() {
        let vertical = FillOptions {
            angle_deg: 90.0,
            ..fill(1.0)
        };
        let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &vertical).unwrap();
        let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
        for path in &planned.paths {
            assert!((path[0].0 - path[1].0).abs() < 1e-9, "90° 竖扫：{path:?}");
        }
        let cross = FillOptions {
            cross_hatch: true,
            ..fill(1.0)
        };
        let units = scan_fill(&[square(0.0, 0.0, 4.0, true)], &cross).unwrap();
        assert_eq!(units.len(), 2, "交叉排线多一遍转 90° 的扫描");
        let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
        assert!((burn_length(&planned.paths) - 32.0).abs() < 1e-6);
    }

    #[test]
    fn planning_keeps_every_burn_and_shortens_travel() {
        // 一排打乱顺序的短线：规划后出光长度不变、空走明显变短
        let mut units = Vec::new();
        for i in [7, 2, 9, 0, 5, 3, 8, 1, 6, 4] {
            let x = f64::from(i) * 10.0;
            units.push(Unit::Open(vec![(x, 0.0), (x + 5.0, 0.0)]));
        }
        let planned = plan(units, (0.0, 0.0), &PlanOptions::default()).unwrap();
        assert!((burn_length(&planned.paths) - 50.0).abs() < 1e-9);
        assert!(
            planned.stats.travel_distance_after < planned.stats.travel_distance_before * 0.5,
            "{:?}",
            planned.stats
        );
        // 开放线段不被闭合
        assert!(planned.paths.iter().all(|path| path.len() == 2));
    }

    #[test]
    fn closed_contours_start_at_the_nearest_corner() {
        let planned = plan(
            vec![Unit::Closed(square(10.0, 10.0, 5.0, true))],
            (16.0, 16.0),
            &PlanOptions::default(),
        )
        .unwrap();
        assert_eq!(planned.paths[0][0], (15.0, 15.0));
        assert_eq!(planned.paths[0].first(), planned.paths[0].last());

        // 一条边上加了个共线的中间点：它离得最近，但不是尖角，起刀点仍放在角上
        let mut with_mid = square(0.0, 0.0, 10.0, true);
        with_mid.insert(1, (5.0, 0.0));
        let corner = plan(
            vec![Unit::Closed(with_mid.clone())],
            (5.0, -1.0),
            &PlanOptions::default(),
        )
        .unwrap();
        assert!(
            [(0.0, 0.0), (10.0, 0.0)].contains(&corner.paths[0][0]),
            "{:?}",
            corner.paths[0]
        );
        let nearest = plan(
            vec![Unit::Closed(with_mid)],
            (5.0, -1.0),
            &PlanOptions {
                prefer_corners: false,
                ..PlanOptions::default()
            },
        )
        .unwrap();
        assert_eq!(nearest.paths[0][0], (5.0, 0.0));
    }

    #[test]
    fn duplicate_lines_are_burned_once() {
        // 同一个方块画了两遍（一遍换了起点、反了方向），一条线段正反各一遍
        let mut rotated = square(0.0, 0.0, 5.0, false);
        rotated.pop();
        rotated.rotate_left(2);
        rotated.push(rotated[0]);
        let units = vec![
            Unit::Closed(square(0.0, 0.0, 5.0, true)),
            Unit::Closed(rotated),
            Unit::Open(vec![(10.0, 0.0), (20.0, 0.0)]),
            Unit::Open(vec![(20.0, 0.0), (10.0, 0.0)]),
        ];
        let planned = plan(units.clone(), (0.0, 0.0), &PlanOptions::default()).unwrap();
        assert_eq!(planned.removed_duplicates, 2);
        assert!((burn_length(&planned.paths) - 30.0).abs() < 1e-9);
        let kept = plan(
            units,
            (0.0, 0.0),
            &PlanOptions {
                remove_overlaps: false,
                ..PlanOptions::default()
            },
        )
        .unwrap();
        assert_eq!(kept.removed_duplicates, 0);
        assert!((burn_length(&kept.paths) - 60.0).abs() < 1e-9);
    }

    #[test]
    fn cutting_finishes_inner_contours_before_the_outer_one() {
        // 外框离起点最近，但里面两个孔必须先切
        let units = vec![
            Unit::Closed(square(0.0, 0.0, 30.0, true)),
            Unit::Closed(square(5.0, 5.0, 5.0, true)),
            Unit::Closed(square(18.0, 18.0, 5.0, true)),
        ];
        let planned = plan(
            units,
            (0.0, 0.0),
            &PlanOptions {
                inside_first: true,
                ..PlanOptions::default()
            },
        )
        .unwrap();
        let outer_at = planned
            .paths
            .iter()
            .position(|path| path.iter().any(|p| p.0 == 30.0))
            .unwrap();
        assert_eq!(outer_at, 2, "{:?}", planned.paths);
    }

    #[test]
    fn layers_continue_from_where_the_previous_layer_ended() {
        let contours = vec![square(0.0, 0.0, 4.0, true)];
        let fills = scan_fill(&contours, &fill(1.0)).unwrap();
        let outline = vec![Unit::Closed(contours[0].clone())];
        let planned =
            plan_layers(vec![fills, outline], (0.0, 0.0), &PlanOptions::default()).unwrap();
        // 填充 4 行 + 轮廓 1 圈
        assert_eq!(planned.paths.len(), 5);
        assert!((burn_length(&planned.paths) - (16.0 + 16.0)).abs() < 1e-9);
        assert!(plan_layers(vec![Vec::new()], (0.0, 0.0), &PlanOptions::default()).is_err());
    }

    #[test]
    fn auto_overscan_follows_speed_and_acceleration() {
        // 3000 mm/min = 50 mm/s，a = 1000 → 1.25 mm → 取整 1.3
        assert_eq!(auto_overscan_mm(3000.0, 1000.0), 1.3);
        // 慢速取下限 1 mm，极快取上限 20 mm
        assert_eq!(auto_overscan_mm(600.0, 1000.0), 1.0);
        assert_eq!(auto_overscan_mm(60_000.0, 100.0), 20.0);
        // 加速度无效时用默认 1000
        assert_eq!(auto_overscan_mm(3000.0, f64::NAN), 1.3);
    }

    #[test]
    fn invalid_input_is_rejected() {
        assert!(plan(Vec::new(), (0.0, 0.0), &PlanOptions::default()).is_err());
        assert!(
            plan(
                vec![Unit::Open(vec![(f64::NAN, 0.0), (1.0, 0.0)])],
                (0.0, 0.0),
                &PlanOptions::default()
            )
            .is_err()
        );
        assert!(scan_fill(&[square(0.0, 0.0, 1.0, true)], &fill(0.0)).is_err());
        let too_far = FillOptions {
            overscan_mm: 50.0,
            ..fill(0.1)
        };
        assert!(scan_fill(&[square(0.0, 0.0, 1.0, true)], &too_far).is_err());
    }
}
