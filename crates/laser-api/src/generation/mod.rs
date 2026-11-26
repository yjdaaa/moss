//! 预览生成：文字、画板、图片 / G-code 上传三条链路，共用的请求选项解析与幅面换算。

use super::*;

mod draw;
mod image_upload;
mod options;
mod shapes;
mod text;

pub(crate) use draw::generate_draw_artifacts;
pub(crate) use image_upload::generate_image_or_gcode_artifacts;
use options::{MachineMotion, ParsedFill, cut_options_json, fill_options_json, parse_path_options};
pub(crate) use options::{
    feed_exceeds_max_rate_warning, frame_hull_json, merge_layer_options, parse_cut_options,
    parse_fill_options, parse_machine_motion, time_estimate_json,
};
pub(crate) use text::generate_text_artifacts;

/// 画板光栅每像素对应的物理尺寸与工作区上限（见 safety.md §4）。
const DRAW_PIXEL_MM: f64 = 0.1;

/// 图片任务默认机器幅面（翼宿 V1.0，`docs/laser/hardware-bom.md`）；请求可用 `work_area_width_mm` /
/// `work_area_height_mm` 传入连接检查读到的实际行程（上限 `MAX_WORK_AREA_MM`）。发送前另按固件行程复核。
pub(crate) const MACHINE_WORK_AREA_MM: f64 = 100.0;
const MAX_WORK_AREA_MM: f64 = 2000.0;

/// 解析画板 `work_area_width_mm` / `work_area_height_mm`：缺省或 null 用默认 100 mm；
/// 显式给出时必须是 `(0, 2000]` 内的有限数字，非数字、非正数、非有限数或超上限直接报错（HTTP 400），不静默回退。
pub(crate) fn parse_work_area_mm(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<f64, String> {
    let number = match value {
        None | Some(serde_json::Value::Null) => return Ok(MACHINE_WORK_AREA_MM),
        Some(serde_json::Value::Number(num)) => num.as_f64().ok_or_else(|| {
            format!("{field} 必须是大于 0 且不超过 {MAX_WORK_AREA_MM:.0} mm 的有限数字")
        })?,
        Some(serde_json::Value::String(text)) => text.trim().parse::<f64>().map_err(|_| {
            format!("{field} 必须是大于 0 且不超过 {MAX_WORK_AREA_MM:.0} mm 的有限数字")
        })?,
        Some(_) => {
            return Err(format!(
                "{field} 必须是大于 0 且不超过 {MAX_WORK_AREA_MM:.0} mm 的有限数字"
            ));
        }
    };
    if !number.is_finite() || number <= 0.0 || number > MAX_WORK_AREA_MM {
        return Err(format!(
            "{field} 必须是大于 0 且不超过 {MAX_WORK_AREA_MM:.0} mm 的有限数字"
        ));
    }
    Ok(number)
}

/// 请求省略 `work_area_*` 时优先取握手记忆的固件行程（同样经过 `parse_work_area_mm` 上限校验），无记录时回退默认 100 mm。
pub(crate) fn parse_work_area_mm_or(
    value: Option<&serde_json::Value>,
    field: &str,
    remembered_mm: Option<f64>,
) -> Result<f64, String> {
    match value {
        None | Some(serde_json::Value::Null) => match remembered_mm {
            Some(rem) => parse_work_area_mm(Some(&serde_json::json!(rem)), field),
            None => Ok(MACHINE_WORK_AREA_MM),
        },
        other => parse_work_area_mm(other, field),
    }
}

/// 图片横向扫描每侧过扫描（mm），尺寸换算会为它留出余量
const IMAGE_OVERSCAN_MM: f64 = 1.0;

/// 文字放进木牌时四周默认留边（mm）
const TEXT_BOARD_MARGIN_MM: f64 = 2.0;

/// 发送前按固件报告的机器行程复核任务范围（2026-11-06）。
/// 任务以开始时激光头所在位置为原点（生成的 G-code 以 `G92 X0 Y0` 开头），因此只比较刀路走过的
/// 宽、高是否超过 X/Y 行程——超过时无论从哪里开始都会撞到行程尽头，直接拒绝、不连接后续发送。
/// 固件没报告行程（`$130/$131` 与 `$CD` 都没有）或文件含机床坐标 / 回参考点移动、无法从文件得知
/// 范围时跳过本项，交给固件软限位。
pub(crate) fn check_job_fits_machine(
    gcode: &str,
    limits: &laser_device::MachineLimits,
) -> Result<(), String> {
    if limits.x_travel_mm.is_none() && limits.y_travel_mm.is_none() {
        return Ok(());
    }
    let Ok(Some(extents)) = laser_core::gcode::motion_extents(gcode) else {
        return Ok(());
    };
    const TOLERANCE_MM: f64 = 0.01;
    let over = |size: f64, travel: Option<f64>| travel.is_some_and(|t| size > t + TOLERANCE_MM);
    if over(extents.width(), limits.x_travel_mm) || over(extents.height(), limits.y_travel_mm) {
        let fmt = |travel: Option<f64>| travel.map_or("未知".to_owned(), |t| format!("{t:.0}"));
        return Err(format!(
            "任务范围 {:.1} × {:.1} mm 超出机器行程 {} × {} mm，已拒绝发送；请缩小尺寸后重新生成预览",
            extents.width(),
            extents.height(),
            fmt(limits.x_travel_mm),
            fmt(limits.y_travel_mm),
        ));
    }
    Ok(())
}

/// r66（I1）：固件未完整报告 X/Y 行程时 `check_job_fits_machine` 照旧放行，但若软限位确认未开启或状态未知，
/// 生成风险提示带进发送结果（不阻断发送）。
pub(crate) fn soft_limit_preflight_warning(limits: &laser_device::MachineLimits) -> Option<String> {
    if limits.x_travel_mm.is_some() && limits.y_travel_mm.is_some() {
        return None;
    }
    match limits.soft_limits_enabled {
        Some(true) => None,
        Some(false) => Some("固件未报告行程且软限位未开启，越界不会被拦截".to_owned()),
        None => Some("固件未报告行程且软限位状态未知，越界可能不会被拦截".to_owned()),
    }
}

/// r66（I2）：仅在回零已开启、轴行程已知、机械坐标范围方向明确且拿到了起点 `MPos` 时，
/// 按 `起点 MPos + 刀路相对范围` 估算是否会撞到行程尽头；只返回警告，绝不拒绝发送。
pub(crate) fn estimate_mpos_travel_warning(
    gcode: &str,
    origin_mpos: Option<(f64, f64, f64)>,
    limits: &laser_device::MachineLimits,
    features: Option<&laser_device::MachineFeatures>,
) -> Option<String> {
    let (ox, oy, _) = origin_mpos?;
    let Ok(Some(extents)) = laser_core::gcode::motion_extents(gcode) else {
        return None;
    };
    const TOLERANCE_MM: f64 = 0.05;

    let axis_bounds = |axis: &'static str, origin: f64| -> Option<(f64, f64)> {
        let travel = match axis {
            "X" => limits.x_travel_mm,
            "Y" => limits.y_travel_mm,
            _ => None,
        }
        .filter(|t| t.is_finite() && *t > 0.0)?;
        if let Some(feat) = features {
            let homing = match axis {
                "X" => feat.x_homing.as_ref(),
                "Y" => feat.y_homing.as_ref(),
                _ => None,
            }?;
            if homing.cycle.unwrap_or(1) <= 0 {
                return None;
            }
            let mpos_mm = homing.mpos_mm?;
            let pos_dir = homing.positive_direction?;
            Some(if pos_dir {
                (mpos_mm - travel, mpos_mm)
            } else {
                (mpos_mm, mpos_mm + travel)
            })
        } else {
            if limits.homing_enabled != Some(true) {
                return None;
            }
            let mask = limits.homing_dir_mask?;
            let bit = if axis == "X" { 1u8 } else { 2u8 };
            let negative_home = (mask & bit) != 0;
            // 标准 GRBL 回零后机械坐标在 [-travel, 0] 内；仅当 $23 设为负向回零且当前 MPos 已处于正区间（HOMING_FORCE_SET_ORIGIN）时取 [0, travel]
            if negative_home && origin > TOLERANCE_MM {
                Some((0.0, travel))
            } else {
                Some((-travel, 0.0))
            }
        }
    };

    let mut overflow_parts = Vec::new();
    for (axis, origin, rel_min, rel_max) in [
        ("X", ox, extents.min_x, extents.max_x),
        ("Y", oy, extents.min_y, extents.max_y),
    ] {
        let Some((bound_min, bound_max)) = axis_bounds(axis, origin) else {
            continue;
        };
        let reach_min = origin + rel_min;
        let reach_max = origin + rel_max;
        if reach_min < bound_min - TOLERANCE_MM || reach_max > bound_max + TOLERANCE_MM {
            overflow_parts.push(format!(
                "{axis} 轴预计走到 {reach_min:.1} ~ {reach_max:.1} mm（机械坐标范围 {bound_min:.0} ~ {bound_max:.0} mm）"
            ));
        }
    }
    (!overflow_parts.is_empty()).then(|| {
        format!(
            "按当前机械坐标估算，{}，可能撞到行程尽头，请核对起点位置",
            overflow_parts.join("；")
        )
    })
}

/// 可选的正数尺寸：缺省、null、空串或 0 表示“自动”。
fn optional_positive_mm(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<Option<f64>, String> {
    let number = match value {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::String(text)) if text.trim().is_empty() => return Ok(None),
        Some(serde_json::Value::String(text)) => text
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("{field} 必须是数字"))?,
        Some(other) => other
            .as_f64()
            .ok_or_else(|| format!("{field} 必须是数字"))?,
    };
    if !number.is_finite() || number < 0.0 {
        return Err(format!("{field} 必须是不小于 0 的数字"));
    }
    Ok((number > 0.0).then_some(number))
}

/// 画板成品尺寸（毫米）：宽高都填时锁定比例放进该框内；只填一边按比例算另一边；
/// 都没填按导出图每像素 0.1 mm，超出可用范围时等比缩小并返回 `true`；填写的尺寸超出直接拒绝，不悄悄缩小。
/// `usable` 为实际可放图案的范围（光栅模式宽度已扣除两侧过扫描），`machine` 为机器幅面（只用于提示）。
pub(crate) fn draw_target_size_mm(
    img_w: u32,
    img_h: u32,
    width: Option<f64>,
    height: Option<f64>,
    usable: (f64, f64),
    machine: (f64, f64),
) -> Result<(f64, f64, bool), String> {
    let (w, h) = (f64::from(img_w), f64::from(img_h));
    let (usable_w, usable_h) = usable;
    let mut auto_fitted = false;
    let (target_w, target_h) = match (width, height) {
        (Some(tw), Some(th)) => {
            let scale = (tw / w).min(th / h);
            (w * scale, h * scale)
        }
        (Some(tw), None) => (tw, tw * h / w),
        (None, Some(th)) => (th * w / h, th),
        (None, None) => {
            let (nw, nh) = (w * DRAW_PIXEL_MM, h * DRAW_PIXEL_MM);
            if nw > usable_w + 1e-9 || nh > usable_h + 1e-9 {
                auto_fitted = true;
                let scale = (usable_w / nw).min(usable_h / nh);
                (nw * scale, nh * scale)
            } else {
                (nw, nh)
            }
        }
    };
    if target_w > usable_w + 1e-9 || target_h > usable_h + 1e-9 {
        return Err(format!(
            "成品尺寸 {target_w:.1} × {target_h:.1} mm 超出工作区（机器幅面 {:.0} × {:.0} mm，可放图案最大 {usable_w:.1} × {usable_h:.1} mm），请减小宽或高",
            machine.0, machine.1
        ));
    }
    if target_w < DRAW_PIXEL_MM || target_h < DRAW_PIXEL_MM {
        return Err("成品尺寸太小，宽和高都至少 0.1 mm".into());
    }
    Ok((target_w, target_h, auto_fitted))
}

pub(crate) fn next_workflow_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("wf-{}-{seq}", unix_seconds())
}
