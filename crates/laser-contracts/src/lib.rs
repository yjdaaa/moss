use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod geometry;

pub const MATERIAL_LIBRARY_VERSION: u32 = 1;
pub const DEFAULT_POWER_SCALE: u32 = 1000;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct McpTelemetrySnapshot {
    pub client_name: String,
    pub client_version: String,
    pub last_request: String,
    pub last_request_time: String,
    pub messages_count: u64,
    pub recent_events: Vec<String>,
}

pub struct McpTelemetry {
    data: Mutex<McpTelemetrySnapshot>,
}

static MCP_TELEMETRY: OnceLock<McpTelemetry> = OnceLock::new();

pub fn global_mcp_telemetry() -> &'static McpTelemetry {
    MCP_TELEMETRY.get_or_init(|| McpTelemetry {
        data: Mutex::new(McpTelemetrySnapshot::default()),
    })
}

fn current_time_str() -> String {
    let now = std::time::SystemTime::now();
    let duration = now.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    let h = (secs / 3600 % 24 + 8) % 24;
    let m = secs / 60 % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

impl McpTelemetry {
    pub fn record_event(&self, event: &str) {
        if let Ok(mut lock) = self.data.lock() {
            lock.messages_count += 1;
            let now = current_time_str();
            lock.recent_events.push(format!("{now} {event}"));
            if lock.recent_events.len() > 30 {
                lock.recent_events.remove(0);
            }
        }
    }

    pub fn record_client_info(&self, name: &str, version: &str) {
        if let Ok(mut lock) = self.data.lock() {
            lock.client_name = name.to_string();
            lock.client_version = version.to_string();
        }
    }

    pub fn record_request(&self, req: &str) {
        if let Ok(mut lock) = self.data.lock() {
            lock.last_request = req.to_string();
            lock.last_request_time = current_time_str();
        }
    }

    pub fn snapshot(&self) -> McpTelemetrySnapshot {
        self.data.lock().map(|d| d.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaterialLibrary {
    #[serde(default = "material_library_version")]
    pub version: u32,
    #[serde(default = "default_power_scale")]
    pub power_scale: u32,
    #[serde(default)]
    pub materials: BTreeMap<String, MaterialEntry>,
}

fn material_library_version() -> u32 {
    MATERIAL_LIBRARY_VERSION
}

fn default_power_scale() -> u32 {
    DEFAULT_POWER_SCALE
}

impl MaterialLibrary {
    /// 按目标功率标尺（如 255 或 1000）等比例重算整个材料库所有配方参数
    pub fn rescaled_to(&self, target_scale: u32) -> Self {
        if self.power_scale == target_scale || self.power_scale == 0 || target_scale == 0 {
            return self.clone();
        }
        let mut clone = self.clone();
        clone.power_scale = target_scale;
        for entry in clone.materials.values_mut() {
            for thickness in entry.thicknesses.values_mut() {
                if let Some(engrave) = &mut thickness.engrave {
                    if let Some(raster) = &mut engrave.raster {
                        *raster = raster.rescaled_to(target_scale as f64);
                    }
                    if let Some(outline) = &mut engrave.outline {
                        *outline = outline.rescaled_to(target_scale as f64);
                    }
                }
                if let Some(cut) = &mut thickness.cut {
                    *cut = cut.rescaled_to(target_scale as f64);
                }
            }
        }
        clone
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct MaterialEntry {
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub thicknesses: BTreeMap<String, ThicknessEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ThicknessEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engrave: Option<EngraveModes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<ParamCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct EngraveModes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raster: Option<ParamCell>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<ParamCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParamCell {
    pub laser_min_power: i32,
    pub laser_max_power: i32,
    pub feed_rate: i32,
    pub travel_rate: i32,
    pub threshold: i32,
    pub passes: i32,
    pub pixel_size_mm: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_scale: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl ParamCell {
    pub fn new(laser_max_power: i32, feed_rate: i32, threshold: i32) -> Self {
        Self {
            laser_min_power: 0,
            laser_max_power,
            feed_rate,
            travel_rate: 3000,
            threshold,
            passes: 1,
            pixel_size_mm: 0.1,
            power_scale: Some(1000),
            source: None,
            notes: None,
            updated_at: None,
            extra: BTreeMap::new(),
        }
    }

    /// 将当前参数单元按目标功率标尺（如 255 或 1000）等比例重算
    pub fn rescaled_to(&self, target_scale: f64) -> Self {
        let current_scale = self.power_scale.unwrap_or(1000) as f64;
        if (current_scale - target_scale).abs() < 1e-6 || current_scale <= 0.0 || target_scale <= 0.0 {
            return self.clone();
        }
        let ratio = target_scale / current_scale;
        let rescaled_min = (self.laser_min_power as f64 * ratio).round().clamp(0.0, target_scale) as i32;
        let rescaled_max = (self.laser_max_power as f64 * ratio).round().clamp(0.0, target_scale) as i32;
        let mut clone = self.clone();
        clone.laser_min_power = rescaled_min;
        clone.laser_max_power = rescaled_max;
        clone.power_scale = Some(target_scale.round() as i32);
        clone
    }

    pub fn validate(&self, s_max: i32, max_passes: i32) -> Result<(), String> {
        if !(0..=s_max).contains(&self.laser_min_power)
            || !(0..=s_max).contains(&self.laser_max_power)
            || self.laser_min_power > self.laser_max_power
        {
            return Err("power must be within 0..S_MAX".into());
        }
        if self.feed_rate < 1 || self.travel_rate < 1 {
            return Err("feed_rate and travel_rate must be positive".into());
        }
        if !(-1..=255).contains(&self.threshold) {
            return Err("threshold must be -1..255".into());
        }
        if !(1..=max_passes).contains(&self.passes) {
            return Err("passes are outside the configured range".into());
        }
        if !self.pixel_size_mm.is_finite() || self.pixel_size_mm <= 0.0 {
            return Err("pixel_size_mm must be positive".into());
        }
        Ok(())
    }
}

pub fn material_library_schema() -> schemars::schema::RootSchema {
    schemars::schema_for!(MaterialLibrary)
}

pub fn material_library_json_schema() -> serde_json::Value {
    serde_json::to_value(material_library_schema()).expect("material library schema must serialize")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowRecord {
    pub workflow_id: String,
    pub status: String,
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub input: WorkflowInput,
    #[serde(default)]
    pub artifacts: WorkflowArtifacts,
    #[serde(default)]
    pub confirmation_snapshot: Option<WorkflowSnapshot>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub updated_at: f64,
    #[serde(default)]
    pub stale: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowInput {
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub connection_mode: Option<String>,
    #[serde(default)]
    pub font_size: Option<i32>,
    #[serde(default)]
    pub network_telnet_port: Option<i32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowArtifacts {
    #[serde(default)]
    pub gcode_file: Option<String>,
    #[serde(default)]
    pub gcode_sha256: Option<String>,
    #[serde(default)]
    pub image_file: Option<String>,
    #[serde(default)]
    pub preview_file: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct WorkflowSnapshot {
    #[serde(default)]
    pub power: Option<i32>,
    #[serde(default)]
    pub feed_rate: Option<i32>,
    #[serde(default)]
    pub passes: Option<i32>,
    #[serde(default)]
    pub pixel_size_mm: Option<f64>,
    #[serde(default)]
    pub overscan_mm: Option<f64>,
    #[serde(default)]
    pub threshold: Option<i32>,
    #[serde(default)]
    pub gcode_sha256: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct JobRecord {
    pub job_id: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
    #[serde(default)]
    pub calibration_id: Option<String>,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub baudrate: Option<i32>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub http_port: Option<i32>,
    #[serde(default)]
    pub telnet_port: Option<i32>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub expected_gcode_sha256: Option<String>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub started_at: Option<f64>,
    #[serde(default)]
    pub finished_at: Option<f64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct TextTaskRecord {
    pub task_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub task_dir: Option<String>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub updated_at: f64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CalibrationRecord {
    pub calibration_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub engraving_mode: Option<String>,
    #[serde(default)]
    pub matrix_kind: Option<String>,
    #[serde(default)]
    pub gcode_file: Option<String>,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub selected_cell_number: Option<i32>,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub updated_at: f64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ToolCallResult {
    pub success: bool,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ToolCallResult {
    pub fn ok(result: impl Into<String>) -> Self {
        Self {
            success: true,
            result: result.into(),
            detail: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            result: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(success: bool, result: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            success,
            result: result.into(),
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CheckLaserConnectionArgs {
    #[serde(default)]
    pub baudrate: Option<i32>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub http_port: Option<i32>,
    #[serde(default)]
    pub include_detail: Option<bool>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub telnet_port: Option<i32>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub transport: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct LaserSafeActionArgs {
    pub action: String,
    #[serde(default)]
    pub baudrate: Option<i32>,
    #[serde(default)]
    pub confirmed: Option<bool>,
    #[serde(default)]
    pub connection_mode: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
    #[serde(default)]
    pub feed_rate: Option<f64>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub http_port: Option<i32>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub power: Option<f64>,
    #[serde(default)]
    pub telnet_port: Option<i32>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub z: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RecommendLaserParamsArgs {
    pub material: String,
    pub thickness_mm: f64,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub engraving_mode: Option<String>,
    #[serde(default)]
    pub power_scale: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SelectCalibrationCellArgs {
    pub calibration_id: String,
    pub cell_number: i32,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub thickness_mm: Option<f64>,
    #[serde(default)]
    pub laser_mode: Option<String>,
    #[serde(default)]
    pub engraving_mode: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CommandExecutionArgs {
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct OpenAppArgs {
    pub argument: String,
    #[serde(default)]
    pub payload: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ShortcutKeyExecutionArgs {
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct GenerateTextImageArgs {
    pub text: String,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
    #[serde(default)]
    pub font_size: Option<i32>,
    #[serde(default)]
    pub line_spacing: Option<f64>,
    #[serde(default)]
    pub auto_wrap: Option<bool>,
    #[serde(default)]
    pub max_lines: Option<i32>,
    #[serde(default)]
    pub layout_mode: Option<String>,
    #[serde(default)]
    pub output_file: Option<String>,
    #[serde(default)]
    pub font_path: Option<String>,
}

pub fn builtin_material_library() -> MaterialLibrary {
    let presets = [
        (
            "椴木",
            vec![
                "basswood",
                "wood",
                "plywood",
                "木头",
                "木板",
                "木料",
                "木牌",
                "木片",
                "木质",
                "椴木板",
            ],
            "3",
            420,
            1800,
            900,
            360,
        ),
        (
            "亚克力",
            vec!["acrylic", "plexiglass", "有机玻璃"],
            "3",
            340,
            1600,
            850,
            260,
        ),
        ("皮革", vec!["leather", "皮料"], "1.5", 280, 1900, 650, 520),
        (
            "纸板",
            vec!["cardboard", "paperboard", "瓦楞纸", "卡纸"],
            "1",
            220,
            2500,
            520,
            760,
        ),
    ];
    let mut materials = BTreeMap::new();
    for (name, aliases, thickness, engrave_power, engrave_feed, cut_power, cut_feed) in presets {
        let mut thicknesses = BTreeMap::new();
        thicknesses.insert(
            thickness.to_owned(),
            ThicknessEntry {
                engrave: Some(EngraveModes {
                    raster: Some(ParamCell::new(engrave_power, engrave_feed, -1)),
                    outline: Some(ParamCell::new(engrave_power, engrave_feed, -1)),
                }),
                cut: Some(ParamCell::new(cut_power, cut_feed, 128)),
            },
        );
        materials.insert(
            name.to_owned(),
            MaterialEntry {
                aliases: aliases.into_iter().map(str::to_owned).collect(),
                thicknesses,
            },
        );
    }
    MaterialLibrary {
        version: MATERIAL_LIBRARY_VERSION,
        power_scale: DEFAULT_POWER_SCALE,
        materials,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_integer_and_schema_is_exportable() {
        let library = MaterialLibrary {
            version: MATERIAL_LIBRARY_VERSION,
            power_scale: DEFAULT_POWER_SCALE,
            materials: BTreeMap::new(),
        };
        assert_eq!(serde_json::to_value(library).unwrap()["version"], 1);
        assert_eq!(material_library_json_schema()["title"], "MaterialLibrary");
    }

    #[test]
    fn unknown_cell_fields_round_trip_through_extra() {
        let input = serde_json::json!({
            "laser_min_power": 0, "laser_max_power": 420,
            "feed_rate": 1800, "travel_rate": 3000,
            "threshold": -1, "passes": 1, "pixel_size_mm": 0.1,
            "confidence": "verified"
        });
        let cell: ParamCell = serde_json::from_value(input).unwrap();
        assert_eq!(cell.extra["confidence"], "verified");
        assert_eq!(
            serde_json::to_value(cell).unwrap()["confidence"],
            "verified"
        );
    }

    #[test]
    fn builtin_presets_have_four_materials_and_twelve_cells() {
        let library = builtin_material_library();
        assert_eq!(library.materials.len(), 4);
        let cells: usize = library
            .materials
            .values()
            .flat_map(|entry| entry.thicknesses.values())
            .map(|entry| {
                entry.engrave.as_ref().map_or(0, |m| {
                    usize::from(m.raster.is_some()) + usize::from(m.outline.is_some())
                }) + usize::from(entry.cut.is_some())
            })
            .sum();
        assert_eq!(cells, 12);
    }

    #[test]
    fn parameter_cell_range_validation_is_strict() {
        assert!(ParamCell::new(800, 1200, -1).validate(1000, 10).is_ok());
        let mut invalid = ParamCell::new(800, 1200, -1);
        invalid.threshold = 256;
        assert!(invalid.validate(1000, 10).is_err());
        invalid = ParamCell::new(1001, 1200, -1);
        assert!(invalid.validate(1000, 10).is_err());
    }

    #[test]
    fn param_cell_and_material_library_rescale_properly() {
        let cell = ParamCell::new(420, 1800, -1);
        assert_eq!(cell.power_scale, Some(1000));
        let rescaled = cell.rescaled_to(255.0);
        assert_eq!(rescaled.power_scale, Some(255));
        assert_eq!(rescaled.laser_max_power, 107); // 420 * 255 / 1000 = 107.1 -> 107

        let lib = builtin_material_library();
        assert_eq!(lib.power_scale, 1000);
        let lib_255 = lib.rescaled_to(255);
        assert_eq!(lib_255.power_scale, 255);
        let wood = &lib_255.materials["椴木"].thicknesses["3"];
        let raster = wood.engrave.as_ref().unwrap().raster.as_ref().unwrap();
        assert_eq!(raster.power_scale, Some(255));
        assert_eq!(raster.laser_max_power, 107);
    }
}
