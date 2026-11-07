use std::io::{self, Read};

use laser_core::{gcode, material, pycompat, raster, safety};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "op")]
enum Request {
    #[serde(rename = "material")]
    Material {
        material: String,
        thickness_mm: f64,
        laser_mode: String,
        engraving_mode: Option<String>,
    },
    #[serde(rename = "safety")]
    Safety { material: String },
    #[serde(rename = "pycompat")]
    Pycompat {
        values: Vec<f64>,
        digits: Vec<i32>,
        times: Vec<i64>,
    },
    #[serde(rename = "dither")]
    Dither {
        algorithm: String,
        threshold: f64,
        pixels: Vec<Vec<f64>>,
    },
    #[serde(rename = "otsu")]
    Otsu { pixels: Vec<Vec<u8>> },
    #[serde(rename = "time")]
    Time { gcode: String, rapid_rate: f64 },
    #[serde(rename = "raster_gcode")]
    RasterGcode {
        bitmap: Vec<Vec<u8>>,
        scan_direction: String,
        output_strategy: String,
        pixel_size_mm: f64,
        overscan_mm: f64,
        work_area_width_mm: f64,
        work_area_height_mm: f64,
        power: i32,
        feed_rate: i32,
        laser_mode: String,
        snake_scan: bool,
    },
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let request: Request = serde_json::from_str(&input).unwrap();
    let output = match request {
        Request::Material {
            material: name,
            thickness_mm,
            laser_mode,
            engraving_mode,
        } => {
            match material::recommend(&name, thickness_mm, &laser_mode, engraving_mode.as_deref()) {
                Some(value) => {
                    serde_json::json!({"status":"ok","material":value.material,"matched_thickness_mm":value.matched_thickness_mm,"laser_mode":value.laser_mode,"engraving_mode":value.engraving_mode,"max_power":value.params.laser_max_power,"feed_rate":value.params.feed_rate})
                }
                None => serde_json::json!({"status":"none"}),
            }
        }
        Request::Safety { material: name } => {
            let status = match safety::evaluate_material(&name) {
                safety::MaterialSafety::Allow => "allow",
                safety::MaterialSafety::Clarify { .. } => "clarify",
                safety::MaterialSafety::Block { .. } => "block",
            };
            serde_json::json!({"status":status})
        }
        Request::Pycompat {
            values,
            digits,
            times,
        } => {
            serde_json::json!({
                "formatted": values.iter().map(|v| pycompat::format_float(*v)).collect::<Vec<_>>(),
                "rounds": values.iter().map(|v| digits.iter().map(|n| pycompat::py_round(*v, *n).to_bits().to_string()).collect::<Vec<_>>()).collect::<Vec<_>>(),
                "times": times.iter().map(|v| pycompat::iso8601_from_micros(*v)).collect::<Vec<_>>()
            })
        }
        Request::Dither {
            algorithm,
            threshold,
            pixels,
        } => {
            let algorithm = match algorithm.as_str() {
                "threshold" => raster::DitherAlgorithm::Threshold,
                "floyd_steinberg" => raster::DitherAlgorithm::FloydSteinberg,
                "atkinson" => raster::DitherAlgorithm::Atkinson,
                "sierra_lite" => raster::DitherAlgorithm::SierraLite,
                other => panic!("unknown dither algorithm: {other}"),
            };
            serde_json::json!({ "pixels": raster::dither_f64(&pixels, algorithm, threshold) })
        }
        Request::Otsu { pixels } => {
            serde_json::json!({ "threshold": raster::otsu_threshold(&pixels) })
        }
        Request::Time { gcode, rapid_rate } => {
            let estimate = gcode::estimate_time(&gcode, rapid_rate);
            serde_json::to_value(serde_json::json!({
                "estimated_seconds": estimate.estimated_seconds,
                "estimated_minutes": estimate.estimated_minutes,
                "laser_seconds": estimate.laser_seconds,
                "non_laser_seconds": estimate.non_laser_seconds,
                "laser_distance_mm": estimate.laser_distance_mm,
                "non_laser_distance_mm": estimate.non_laser_distance_mm,
            }))
            .unwrap()
        }
        Request::RasterGcode {
            bitmap,
            scan_direction,
            output_strategy,
            pixel_size_mm,
            overscan_mm,
            work_area_width_mm,
            work_area_height_mm,
            power,
            feed_rate,
            laser_mode,
            snake_scan,
        } => {
            let plan =
                raster::raster_plan_with_direction(&bitmap, &scan_direction, &output_strategy)
                    .unwrap();
            let width = bitmap.first().map_or(0, Vec::len);
            let height = bitmap.len();
            let gcode_str = gcode::write_raster(
                &plan,
                width,
                height,
                &gcode::RasterWriterOptions {
                    pixel_size_mm,
                    overscan_mm,
                    work_area_width_mm,
                    work_area_height_mm,
                    power,
                    feed_rate,
                    laser_mode: if laser_mode == "M3" { "M3" } else { "M4" },
                    snake_scan,
                    material: Some("basswood".into()),
                    safety_note: Some("test".into()),
                    power_scale: None,
                },
            )
            .unwrap();
            serde_json::json!({
                "gcode": gcode_str,
                "selected_axis": plan.selected_axis,
                "output_strategy": plan.output_strategy,
            })
        }
    };
    println!("{}", serde_json::to_string(&output).unwrap());
}
