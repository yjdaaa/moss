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

mod offset;
mod postprocess;

pub use offset::{KerfOffset, offset_contours};
pub use postprocess::apply_cut_options;

use crate::gcode::Step;
use crate::safety::SafetyError;
use laser_contracts::geometry::Point;

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

fn step_point(step: &Step) -> Point {
    match *step {
        Step::Travel(p) | Step::Coast(p) | Step::Burn(p) => p,
    }
}

#[cfg(test)]
mod tests;
