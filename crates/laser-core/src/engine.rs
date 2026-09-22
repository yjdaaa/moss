use crate::{font, gcode, path_opt};
use image::RgbaImage;
use laser_contracts::geometry::PathOptimizationStats;

#[derive(Debug, Clone)]
pub struct TextTaskSpec<'a> {
    pub text: &'a str,
    pub font_path: &'a str,
    pub font_size_mm: f64,
    pub letter_spacing_mm: f64,
    pub power: i32,
    pub feed_rate: i32,
    pub preview_width_px: u32,
    pub preview_height_px: u32,
}

#[derive(Debug, Clone)]
pub struct TextTaskArtifacts {
    pub gcode: String,
    pub preview: RgbaImage,
    pub width_mm: f64,
    pub height_mm: f64,
    pub glyphs: usize,
    pub path_optimization: PathOptimizationStats,
    pub estimate: gcode::TimeEstimate,
}

pub struct TextTaskEngine;

impl TextTaskEngine {
    pub fn generate(spec: &TextTaskSpec<'_>) -> Result<TextTaskArtifacts, String> {
        let layout = font::layout_text_file(
            spec.font_path,
            spec.text,
            spec.font_size_mm,
            spec.letter_spacing_mm,
        )?;
        let max_dim = layout.width.max(layout.height).max(1.0);
        let optimized = path_opt::optimize_with_threshold(&layout.paths, max_dim)
            .map_err(|e| e.to_string())?;
        let writer_options = gcode::WriterOptions {
            width_mm: layout.width.max(1.0),
            height_mm: layout.height.max(1.0),
            power: spec.power,
            feed_rate: spec.feed_rate,
            auto_close: true,
            laser_mode: "M4",
            power_scale: None,
        };
        let gcode = gcode::write_outline(&optimized.paths, &writer_options)?;
        let preview = gcode::preview_outline_rgba(
            &optimized.paths,
            spec.preview_width_px,
            spec.preview_height_px,
            writer_options.width_mm,
            writer_options.height_mm,
        )?;
        let estimate = gcode::estimate_time(&gcode, 3000.0);
        Ok(TextTaskArtifacts {
            gcode,
            preview,
            width_mm: layout.width,
            height_mm: layout.height,
            glyphs: layout.glyphs,
            path_optimization: optimized.stats,
            estimate,
        })
    }
}
