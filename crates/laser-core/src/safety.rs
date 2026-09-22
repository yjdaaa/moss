use std::fmt;

pub const MAX_POWER: i32 = 1000;
pub const MAX_FEED_RATE: i32 = 3000;
pub const MIN_FEED_RATE: i32 = 50;
pub const MAX_PASSES: i32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialSafety {
    Allow,
    Clarify { reason: &'static str },
    Block { reason: &'static str },
}

pub fn evaluate_material(material: &str) -> MaterialSafety {
    let normalized = material
        .trim()
        .to_lowercase()
        .replace(['_', '-', '/', '\\'], " ");
    let paper_card = normalized.contains("纸质")
        || normalized.contains("纸制")
        || normalized.contains("纸做")
        || normalized.contains("paper");
    if normalized.contains("pvc")
        || normalized.contains("聚氯乙烯")
        || normalized.contains("聚氯已烯")
        || normalized.contains("polyvinyl chloride")
        || normalized.contains("polyvinylchloride")
        || normalized.contains("vinyl chloride")
        || normalized.contains("氯乙烯")
    {
        return MaterialSafety::Block {
            reason: "PVC may release hazardous chlorine gas when lasered",
        };
    }
    if paper_card && (normalized.contains("软卡") || normalized.contains("soft card")) {
        return MaterialSafety::Allow;
    }
    if normalized.contains("soft card")
        || normalized.contains("软卡")
        || normalized.contains("软卡片")
        || normalized.contains("软性卡")
        || normalized.contains("软质卡")
        || normalized.contains("plastic")
        || normalized.contains("塑料")
        || normalized.contains("塑胶")
        || normalized.contains("unknown plastic")
        || normalized.contains("不明塑料")
        || normalized.contains("成分不明")
        || normalized.contains("未知塑料")
        || normalized.contains("普通塑料")
    {
        return MaterialSafety::Clarify {
            reason: "material composition must be confirmed before laser use",
        };
    }
    MaterialSafety::Allow
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobSafetyParams {
    pub size_mm: f64,
    pub power: i32,
    pub feed_rate: i32,
    pub passes: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoreJobSafetyParams {
    pub output_format: String,
    pub mode: String,
    pub size_mm: f64,
    pub power: i32,
    pub feed_rate: i32,
    pub passes: i32,
    pub pixel_size_mm: f64,
    pub overscan_mm: f64,
    pub arc_tolerance_mm: f64,
    pub vector_simplify_factor: f64,
    pub fill_strategy: String,
    pub fill_spacing_mm: f64,
    pub raster_output_strategy: String,
    pub work_area_width_mm: f64,
    pub work_area_height_mm: f64,
    pub threshold: i32,
    pub manual_threshold: bool,
    pub manual_dither: bool,
    pub dither_algorithm: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyError(pub &'static str);

impl fmt::Display for SafetyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for SafetyError {}

pub fn validate_job(params: &JobSafetyParams) -> Result<(), SafetyError> {
    if !(5.0..=120.0).contains(&params.size_mm) {
        return Err(SafetyError("size must be between 5mm and 120mm"));
    }
    if !(1..=MAX_POWER).contains(&params.power) {
        return Err(SafetyError("power must stay within S1-S1000"));
    }
    if !(MIN_FEED_RATE..=MAX_FEED_RATE).contains(&params.feed_rate) {
        return Err(SafetyError("feed rate is outside the safe range"));
    }
    if !(1..=MAX_PASSES).contains(&params.passes) {
        return Err(SafetyError("pass count must be between 1 and 5"));
    }
    Ok(())
}

pub fn validate_core_job(params: &CoreJobSafetyParams) -> Result<(), SafetyError> {
    if !matches!(&*params.output_format, "gcode" | "nc") {
        return Err(SafetyError("Output format must be gcode or nc"));
    }
    if !matches!(&*params.mode, "outline" | "raster") {
        return Err(SafetyError("Mode must be outline or raster"));
    }
    if !params.size_mm.is_finite() || !(5.0..=120.0).contains(&params.size_mm) {
        return Err(SafetyError(
            "Size must be between 5mm and 120mm for MVP safety",
        ));
    }
    if !(1..=MAX_POWER).contains(&params.power) {
        return Err(SafetyError(
            "Power S value must stay within configured GRBL range S1-S1000",
        ));
    }
    if !(MIN_FEED_RATE..=MAX_FEED_RATE).contains(&params.feed_rate) {
        return Err(SafetyError("Feed rate is outside the safe range"));
    }
    if !(1..=MAX_PASSES).contains(&params.passes) {
        return Err(SafetyError("Pass count must be between 1 and 5"));
    }
    if !params.pixel_size_mm.is_finite() || !(0.05..=1.0).contains(&params.pixel_size_mm) {
        return Err(SafetyError(
            "Raster pixel size must be between 0.05mm and 1.0mm",
        ));
    }
    if !params.overscan_mm.is_finite() || !(0.0..=10.0).contains(&params.overscan_mm) {
        return Err(SafetyError("Raster overscan must be between 0mm and 10mm"));
    }
    if !params.arc_tolerance_mm.is_finite()
        || !(0.0..=1.0).contains(&params.arc_tolerance_mm)
        || params.arc_tolerance_mm == 0.0
    {
        return Err(SafetyError(
            "Arc fitting tolerance must be between 0mm and 1.0mm",
        ));
    }
    if !params.vector_simplify_factor.is_finite()
        || !(0.25..=8.0).contains(&params.vector_simplify_factor)
    {
        return Err(SafetyError(
            "Vector simplify factor must be between 0.25 and 8.0",
        ));
    }
    if !matches!(&*params.fill_strategy, "auto" | "none" | "hatch" | "zigzag") {
        return Err(SafetyError(
            "Fill strategy must be auto, none, hatch, or zigzag",
        ));
    }
    if !params.fill_spacing_mm.is_finite() || !(0.1..=10.0).contains(&params.fill_spacing_mm) {
        return Err(SafetyError("Fill spacing must be between 0.1mm and 10.0mm"));
    }
    if !matches!(
        &*params.raster_output_strategy,
        "auto" | "segment" | "scanline"
    ) {
        return Err(SafetyError(
            "Raster output strategy must be auto, segment, or scanline",
        ));
    }
    if !params.work_area_width_mm.is_finite()
        || !params.work_area_height_mm.is_finite()
        || params.work_area_width_mm <= 0.0
        || params.work_area_height_mm <= 0.0
    {
        return Err(SafetyError("Work area dimensions must be positive"));
    }
    if !(-1..=255).contains(&params.threshold) {
        return Err(SafetyError(
            "threshold must be an integer in 0..255 or -1 for auto",
        ));
    }
    if params.manual_threshold
        && params.manual_dither
        && matches!(
            &*params.dither_algorithm,
            "floyd_steinberg" | "atkinson" | "sierra_lite"
        )
    {
        return Err(SafetyError(
            "threshold conflicts with error-diffusion dither_algorithm",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_pvc_and_clarifies_unknown_plastic() {
        assert!(matches!(
            evaluate_material("PVC"),
            MaterialSafety::Block { .. }
        ));
        assert!(matches!(
            evaluate_material("塑料"),
            MaterialSafety::Clarify { .. }
        ));
        assert_eq!(evaluate_material("椴木"), MaterialSafety::Allow);
    }

    #[test]
    fn validates_safety_limits() {
        let valid = JobSafetyParams {
            size_mm: 20.0,
            power: 100,
            feed_rate: 800,
            passes: 1,
        };
        assert!(validate_job(&valid).is_ok());
        assert!(
            validate_job(&JobSafetyParams {
                size_mm: 4.0,
                ..valid.clone()
            })
            .is_err()
        );
        assert!(
            validate_job(&JobSafetyParams {
                power: 1001,
                ..valid.clone()
            })
            .is_err()
        );
        assert!(validate_job(&JobSafetyParams { passes: 6, ..valid }).is_err());
    }

    #[test]
    fn validates_full_core_contract_and_material_aliases() {
        assert!(matches!(
            evaluate_material("vinyl-chloride"),
            MaterialSafety::Block { .. }
        ));
        assert_eq!(evaluate_material("纸质软卡"), MaterialSafety::Allow);
        assert!(matches!(
            evaluate_material("塑胶"),
            MaterialSafety::Clarify { .. }
        ));
        let valid = CoreJobSafetyParams {
            output_format: "gcode".into(),
            mode: "raster".into(),
            size_mm: 20.0,
            power: 100,
            feed_rate: 800,
            passes: 1,
            pixel_size_mm: 0.2,
            overscan_mm: 1.0,
            arc_tolerance_mm: 0.1,
            vector_simplify_factor: 1.0,
            fill_strategy: "none".into(),
            fill_spacing_mm: 1.0,
            raster_output_strategy: "auto".into(),
            work_area_width_mm: 100.0,
            work_area_height_mm: 100.0,
            threshold: -1,
            manual_threshold: false,
            manual_dither: false,
            dither_algorithm: "threshold".into(),
        };
        assert!(validate_core_job(&valid).is_ok());
        assert!(
            validate_core_job(&CoreJobSafetyParams {
                manual_threshold: true,
                manual_dither: true,
                dither_algorithm: "atkinson".into(),
                ..valid
            })
            .is_err()
        );
    }
}
