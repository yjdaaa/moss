//! 矢量扫描填充：扫描线与轮廓求交、分行分块，以及过扫描与双向扫描偏移。

use super::{DEFAULT_ACCELERATION_MM_S2, EPS, ScanBlock, ScanRow, Unit, close_contour, finite};
use crate::safety::SafetyError;
use laser_contracts::geometry::Point;

/// 单次填充的出光线段上限：再多说明尺寸或线距不合理（100 mm 见方、0.05 mm 线距约 2000 行）。
pub const MAX_FILL_SEGMENTS: usize = 400_000;

/// 自动过扫描下限与上限（mm）。
pub const MIN_AUTO_OVERSCAN_MM: f64 = 1.0;

pub const MAX_OVERSCAN_MM: f64 = 20.0;

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
