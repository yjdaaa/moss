//! G-code 写出、解析、预览、走边框与工时估算。按职责分成子模块，对外路径仍是 `laser_core::gcode::*`。

mod estimate;
mod framing;
mod parse;
mod preview;
mod raster;
mod writer;

pub use estimate::{
    DEFAULT_ACCELERATION_MM_S2, MotionProfile, estimate_time, estimate_time_seconds,
    estimate_time_with,
};
pub use framing::{
    MAX_FRAMING_OUTLINE_POINTS, burn_hull, generate_framing_gcode, generate_outline_framing_gcode,
    generate_paths_framing_gcode,
};
pub use parse::{
    MotionExtents, detect_gcode_power_scale, motion_extents, parse_moves, rescale_gcode_power,
    write_move,
};
pub use preview::{
    MAX_PREVIEW_ARC_POINTS, MAX_PREVIEW_ARC_SEGMENTS, PREVIEW_ARC_TOLERANCE_MM, PreviewToolpath,
    preview_outline, preview_outline_rgba, preview_outline_rgba_with_options, preview_toolpath,
};
pub use raster::{RasterWriterOptions, write_grayscale_raster, write_raster};
pub use writer::{Step, write_outline, write_steps};

#[derive(Debug, Clone, PartialEq)]
pub struct Move {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub feed_rate: Option<f64>,
    pub power: Option<f64>,
    pub rapid: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeEstimate {
    pub estimated_seconds: f64,
    pub estimated_minutes: f64,
    pub laser_seconds: f64,
    pub non_laser_seconds: f64,
    pub laser_distance_mm: f64,
    pub non_laser_distance_mm: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriterOptions {
    pub width_mm: f64,
    pub height_mm: f64,
    pub power: i32,
    pub feed_rate: i32,
    pub auto_close: bool,
    pub laser_mode: &'static str,
    pub power_scale: Option<i32>,
}

#[cfg(test)]
mod tests;
