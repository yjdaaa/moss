//! 工时估算：匀速估算与按机器加速度的规划估算。

use super::TimeEstimate;
use super::parse::motion_segments;

pub fn estimate_time_seconds(text: &str, rapid_mm_per_minute: f64) -> f64 {
    estimate_time(text, rapid_mm_per_minute).estimated_seconds
}

fn summarize(
    laser_seconds: f64,
    non_laser_seconds: f64,
    laser_distance: f64,
    non_laser_distance: f64,
) -> TimeEstimate {
    let total = laser_seconds + non_laser_seconds;
    TimeEstimate {
        estimated_seconds: (total * 10.0).round() / 10.0,
        estimated_minutes: (total / 60.0 * 100.0).round() / 100.0,
        laser_seconds: (laser_seconds * 10.0).round() / 10.0,
        non_laser_seconds: (non_laser_seconds * 10.0).round() / 10.0,
        laser_distance_mm: (laser_distance * 1000.0).round() / 1000.0,
        non_laser_distance_mm: (non_laser_distance * 1000.0).round() / 1000.0,
    }
}

/// 匀速估算：每段按指令速度走满全程（不计加减速，与 Python 版一致）。
pub fn estimate_time(text: &str, rapid_rate: f64) -> TimeEstimate {
    let (mut laser_s, mut other_s, mut laser_d, mut other_d) = (0.0, 0.0, 0.0, 0.0);
    for seg in motion_segments(text, rapid_rate) {
        if seg.length == 0.0 {
            other_s += seg.dwell;
            continue;
        }
        let seconds = seg.length / seg.rate * 60.0;
        if seg.laser {
            laser_s += seconds;
            laser_d += seg.length;
        } else {
            other_s += seconds;
            other_d += seg.length;
        }
    }
    summarize(laser_s, other_s, laser_d, other_d)
}

/// 机器运动参数（2026-11-16，对齐 LightBurn 按加速度估算工时）：GRBL `$110/$111`（最大速度）、
/// `$120/$121`（加速度）、`$11`（转角偏差），或 FluidNC `max_rate_mm_per_min` / `acceleration_mm_per_sec2`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionProfile {
    /// `G0` 快移速度（mm/min）
    pub rapid_mm_per_min: f64,
    pub accel_x_mm_s2: f64,
    pub accel_y_mm_s2: f64,
    /// 各轴最大速度（mm/min），没有时不限
    pub max_rate_x_mm_per_min: Option<f64>,
    pub max_rate_y_mm_per_min: Option<f64>,
    /// 转角偏差（mm，GRBL `$11`，默认 0.01）
    pub junction_deviation_mm: f64,
}

/// 没读到机器参数时假设的加速度（mm/s²），与自动过扫描一致。
pub const DEFAULT_ACCELERATION_MM_S2: f64 = 1000.0;

impl Default for MotionProfile {
    fn default() -> Self {
        Self {
            rapid_mm_per_min: 3000.0,
            accel_x_mm_s2: DEFAULT_ACCELERATION_MM_S2,
            accel_y_mm_s2: DEFAULT_ACCELERATION_MM_S2,
            max_rate_x_mm_per_min: None,
            max_rate_y_mm_per_min: None,
            junction_deviation_mm: 0.01,
        }
    }
}

/// 按 GRBL 规划器估算工时：每段梯形加减速，相邻两段的衔接速度按转角偏差公式限速，
/// 前后两遍扫描保证每段都能在下一段的限速前减下来；M3/M4/M5 切换与 G4 处停住。
/// 扫描填充这种大量短行、来回掉头的刀路，匀速估算会明显偏短。
pub fn estimate_time_with(text: &str, profile: &MotionProfile) -> TimeEstimate {
    let positive = |v: f64, fallback: f64| {
        if v.is_finite() && v > 0.0 {
            v
        } else {
            fallback
        }
    };
    let ax = positive(profile.accel_x_mm_s2, DEFAULT_ACCELERATION_MM_S2);
    let ay = positive(profile.accel_y_mm_s2, DEFAULT_ACCELERATION_MM_S2);
    let jd = positive(profile.junction_deviation_mm, 0.01);
    let rapid = positive(profile.rapid_mm_per_min, 3000.0);
    let segments = motion_segments(text, rapid);
    let n = segments.len();
    // 每段：单位方向、加速度、巡航速度（mm/s）
    let mut unit = Vec::with_capacity(n);
    let mut accel = Vec::with_capacity(n);
    let mut cruise = Vec::with_capacity(n);
    for seg in &segments {
        if seg.length == 0.0 {
            unit.push((0.0, 0.0));
            accel.push(ax.min(ay));
            cruise.push(0.0);
            continue;
        }
        let chord = seg.dx.hypot(seg.dy);
        let u = if chord > 0.0 {
            (seg.dx / chord, seg.dy / chord)
        } else {
            (1.0, 0.0)
        };
        let limit = |a_axis: f64, component: f64| {
            if component.abs() > 1e-9 {
                a_axis / component.abs()
            } else {
                f64::INFINITY
            }
        };
        let a = limit(ax, u.0).min(limit(ay, u.1));
        let mut v = seg.rate / 60.0;
        if let Some(max) = profile.max_rate_x_mm_per_min.filter(|m| *m > 0.0) {
            v = v.min(limit(max / 60.0, u.0));
        }
        if let Some(max) = profile.max_rate_y_mm_per_min.filter(|m| *m > 0.0) {
            v = v.min(limit(max / 60.0, u.1));
        }
        unit.push(u);
        accel.push(if a.is_finite() { a } else { ax.min(ay) });
        cruise.push(v);
    }
    // 每段入口的最大速度：衔接转角限速、两段巡航速度都不能超
    let mut entry = vec![0.0_f64; n + 1];
    for i in 1..n {
        if segments[i].stop_before || segments[i].length == 0.0 || segments[i - 1].length == 0.0 {
            continue;
        }
        let (u0, u1) = (unit[i - 1], unit[i]);
        let cos_theta = -(u0.0 * u1.0 + u0.1 * u1.1);
        let junction = if cos_theta > 0.999_999 {
            0.0
        } else if cos_theta < -0.999_999 {
            f64::INFINITY
        } else {
            let sin_half = (0.5 * (1.0 - cos_theta)).sqrt();
            (accel[i].min(accel[i - 1]) * jd * sin_half / (1.0 - sin_half)).sqrt()
        };
        entry[i] = junction.min(cruise[i]).min(cruise[i - 1]);
    }
    if n > 0 && segments[0].stop_before {
        entry[0] = 0.0;
    }
    // 倒着扫：每段入口速度不能高到在本段内减不到出口速度（最后停住）
    for i in (0..n).rev() {
        let reachable = (entry[i + 1].powi(2) + 2.0 * accel[i] * segments[i].length).sqrt();
        entry[i] = entry[i].min(reachable);
    }
    // 正着扫：每段出口速度不能高过从入口加速所能达到的
    for i in 0..n {
        let reachable = (entry[i].powi(2) + 2.0 * accel[i] * segments[i].length).sqrt();
        entry[i + 1] = entry[i + 1].min(reachable);
    }
    let (mut laser_s, mut other_s, mut laser_d, mut other_d) = (0.0, 0.0, 0.0, 0.0);
    for (i, seg) in segments.iter().enumerate() {
        if seg.length == 0.0 {
            other_s += seg.dwell;
            continue;
        }
        let (v0, v1, vc, a, len) = (entry[i], entry[i + 1], cruise[i], accel[i], seg.length);
        let accel_d = (vc * vc - v0 * v0) / (2.0 * a);
        let decel_d = (vc * vc - v1 * v1) / (2.0 * a);
        let seconds = if accel_d + decel_d <= len {
            (vc - v0) / a + (vc - v1) / a + (len - accel_d - decel_d) / vc
        } else {
            // 到不了巡航速度：三角形速度曲线
            let peak = ((2.0 * a * len + v0 * v0 + v1 * v1) / 2.0).sqrt();
            (peak - v0) / a + (peak - v1) / a
        };
        if seg.laser {
            laser_s += seconds;
            laser_d += len;
        } else {
            other_s += seconds;
            other_d += len;
        }
    }
    summarize(laser_s, other_s, laser_d, other_d)
}
