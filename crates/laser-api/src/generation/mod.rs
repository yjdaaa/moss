//! 预览生成：文字、画板、图片 / G-code 上传三条链路，共用的请求选项解析与幅面换算。

use super::*;

mod draw;
mod image_upload;
mod options;
mod shapes;
mod text;

pub(crate) use draw::generate_draw_artifacts;
pub(crate) use image_upload::generate_image_or_gcode_artifacts;
use options::{cut_options_json, fill_options_json, parse_path_options};
pub(crate) use options::{
    frame_hull_json, merge_layer_options, parse_cut_options, parse_fill_options,
    parse_machine_motion, time_estimate_json,
};
pub(crate) use text::generate_text_artifacts;

/// 画板光栅每像素对应的物理尺寸与工作区上限（见 safety.md §4）。
const DRAW_PIXEL_MM: f64 = 0.1;

/// 图片任务默认机器幅面（翼宿 V1.0，`docs/laser/hardware-bom.md`）；请求可用 `work_area_width_mm` /
/// `work_area_height_mm` 传入连接检查读到的实际行程。发送前另按固件行程复核。
const MACHINE_WORK_AREA_MM: f64 = 100.0;

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
