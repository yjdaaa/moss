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

mod planner;
mod scan;

pub(crate) use planner::point_in_polygon;
pub use planner::{
    OVERLAP_TOLERANCE_MM, PlanOptions, Planned, burn_length, burn_paths, plan, plan_layers,
};
pub use scan::{
    FillGrouping, FillOptions, FillRule, MAX_FILL_SEGMENTS, MAX_OVERSCAN_MM, MAX_SCAN_OFFSET_MM,
    MAX_SCAN_OFFSET_ROWS, MIN_AUTO_OVERSCAN_MM, auto_overscan_mm, scan_fill, scan_offset_at,
};

use crate::gcode::Step;
use laser_contracts::geometry::Point;

/// 自动过扫描默认假设的加速度（mm/s²）；请求可传机器实际值（GRBL `$120/$121`）。
pub use crate::gcode::DEFAULT_ACCELERATION_MM_S2;

const EPS: f64 = 1e-9;

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

#[cfg(test)]
mod tests;
