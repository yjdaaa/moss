use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub type Point = (f64, f64);
pub type Paths = Vec<Vec<Point>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Contour {
    pub points: Vec<Point>,
    pub parent: Option<usize>,
    /// First immediate child; all children can be found through `parent`.
    pub child: Option<usize>,
    pub area: f64,
    pub is_hole: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceGeometry {
    pub paths: Paths,
    pub width_mm: f64,
    pub height_mm: f64,
    pub contour_count: usize,
    pub point_count: usize,
    pub raw_contour_count: usize,
    pub raw_point_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TraceOptions {
    pub size_mm: f64,
    pub width_mm: Option<f64>,
    pub height_mm: Option<f64>,
    pub lock_aspect_ratio: bool,
    pub work_area_width_mm: f64,
    pub work_area_height_mm: f64,
    pub simplify_factor: f64,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            size_mm: 50.0,
            width_mm: None,
            height_mm: None,
            lock_aspect_ratio: true,
            work_area_width_mm: 100.0,
            work_area_height_mm: 100.0,
            simplify_factor: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FillStrategy {
    None,
    Auto,
    Hatch,
    Zigzag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FillResult {
    pub paths: Paths,
    pub fill_strategy: FillStrategy,
    pub fill_spacing_mm: f64,
    pub fill_segment_count: usize,
    pub fill_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PathOptimizationStats {
    pub strategy: String,
    pub travel_distance_before: f64,
    pub travel_distance_after: f64,
    pub travel_reduction_ratio: f64,
    pub path_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PathOptimizationResult {
    pub paths: Paths,
    pub stats: PathOptimizationStats,
}
