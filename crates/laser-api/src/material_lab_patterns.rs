//! 首次标定向导的测试图案预览（r40，2026-11-16）：`/api/material-lab/preview` 的
//! `test_type = scan_offset | overscan | kerf`。与标定矩阵一样只生成、落盘、冻结 SHA-256 并存为标定记录，
//! 发送仍走 `/api/material-lab/send` 的确认与完整门禁；这几种测试不按格选参数（select-cell 拒绝），
//! 结果由用户量好后在向导里填写保存。
use super::*;

/// 不能按格写回材料库的向导测试类型。
pub(crate) const WIZARD_PATTERN_TEST_TYPES: &[&str] = &["scan_offset", "overscan", "kerf"];

impl ApiService {
    pub(crate) fn material_lab_pattern_preview(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        material: &str,
        thickness_mm: f64,
        test_type: &str,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        use laser_core::wizard_patterns::{ScanSquareSpec, kerf_square_gcode, scan_squares_gcode};
        let invalid = |message: String| (400, failure(message, None));
        let library = self.effective_material_library().map_err(|error| {
            (
                500,
                failure(
                    "读取材料库失败",
                    Some(serde_json::json!({ "error": error })),
                ),
            )
        })?;
        let motion =
            crate::generation::parse_machine_motion(object).map_err(|e| (400, failure(e, None)))?;
        let is_kerf = test_type == "kerf";
        let recommendation = laser_core::material::recommend_in_library(
            &library,
            material,
            thickness_mm,
            if is_kerf { "cut" } else { "engrave" },
            if is_kerf { None } else { Some("raster") },
            1000.0,
        );
        let Some(recommendation) = recommendation else {
            return Err(invalid(if is_kerf {
                format!(
                    "材料库里还没有「{material} {thickness_mm}mm」的切割参数，请先做切割标定并保存后再测切缝"
                )
            } else {
                format!("材料库里还没有「{material}」的光栅雕刻参数，请先做线距与功率速度测试")
            }));
        };
        let params = &recommendation.params;
        let int_field = |key: &str, fallback: i64| {
            parse_int(object.get(key), key, fallback).map_err(|e| invalid(e.to_string()))
        };
        let float_field = |key: &str, fallback: f64| {
            parse_float(object.get(key), key, fallback).map_err(|e| invalid(e.to_string()))
        };
        let power = int_field("power", params.laser_max_power as i64)? as i32;
        let speed = int_field("speed", params.feed_rate as i64)? as i32;
        if !(0..=1000).contains(&power) || speed <= 0 {
            return Err(invalid("power 必须在 0..1000，speed 必须为正数".to_owned()));
        }
        let (gcode, cells, width_mm, height_mm, size_mm, pattern_note, x_axis) = if is_kerf {
            let size = float_field("square_mm", 20.0)?;
            let passes = int_field("passes", params.passes.max(1) as i64)? as i32;
            let gcode = kerf_square_gcode(material, size, power, speed, passes)
                .map_err(|e| invalid(format!("切缝测试生成失败: {e}")))?;
            let cells = vec![serde_json::json!({
                "cell_number": 1, "id": "K1", "row": 0, "column": 0, "x_mm": 0.0, "y_mm": 0.0,
                "size_mm": size, "power": power, "speed": speed, "passes": passes,
                "x_label": format!("{size}mm"), "y_label": "",
            })];
            let note = serde_json::json!({
                "square_mm": size,
                "formula": "kerf_mm = (square_mm − measured_mm) / 2",
                "hint": format!("量切下来的方块边长；切缝补偿 = ({size} − 实测) ÷ 2"),
            });
            (gcode, cells, size, size, size, note, "kerf")
        } else {
            let interval = match object.get("line_interval_mm") {
                None | Some(serde_json::Value::Null) => {
                    if params.pixel_size_mm.is_finite() && params.pixel_size_mm > 0.0 {
                        params.pixel_size_mm
                    } else {
                        0.1
                    }
                }
                Some(_) => float_field("line_interval_mm", 0.1)?,
            };
            let accel = motion.overscan_accel();
            let mut overscan_auto = true;
            let specs: Vec<ScanSquareSpec> = if test_type == "scan_offset" {
                // 默认三档速度：推荐速度的一半、本身、一倍半（取 100 整），偏移 0，过扫描按速度自动
                let speeds: Vec<i32> = match object.get("speeds") {
                    None | Some(serde_json::Value::Null) => {
                        let mut list: Vec<i32> = [0.5, 1.0, 1.5]
                            .iter()
                            .map(|k| {
                                (((speed as f64 * k) / 100.0).round() * 100.0).max(100.0) as i32
                            })
                            .collect();
                        list.dedup();
                        list
                    }
                    Some(serde_json::Value::Array(items)) => items
                        .iter()
                        .map(|v| {
                            v.as_f64()
                                .filter(|s| s.is_finite() && *s >= 1.0 && *s <= 100_000.0)
                                .map(|s| s.round() as i32)
                        })
                        .collect::<Option<Vec<_>>>()
                        .filter(|list| (1..=6).contains(&list.len()))
                        .ok_or_else(|| {
                            invalid("speeds 必须是 1 ~ 6 个 1 ~ 100000 的速度（mm/min）".to_owned())
                        })?,
                    Some(_) => return Err(invalid("speeds 必须是数组".to_owned())),
                };
                speeds
                    .into_iter()
                    .map(|s| ScanSquareSpec {
                        power,
                        speed: s,
                        overscan_mm: laser_core::vector_plan::auto_overscan_mm(s as f64, accel),
                        scan_offset_mm: 0.0,
                    })
                    .collect()
            } else {
                // 过扫描检查：左块不过扫描，右块按当前设置（请求的 fill_options，没写的取材料库条目）
                let (fill_request, _) = crate::generation::merge_layer_options(
                    object.get("fill_options"),
                    params.layer_options(laser_contracts::MATERIAL_FILL_OPTIONS_KEY),
                    laser_contracts::MATERIAL_FILL_OPTION_KEYS,
                );
                let fill = crate::generation::parse_fill_options(
                    fill_request.as_ref(),
                    speed as f64,
                    accel,
                )
                .map_err(invalid)?;
                let offset = fill.options.scan_offset_mm;
                overscan_auto = fill.auto;
                vec![
                    ScanSquareSpec {
                        power,
                        speed,
                        overscan_mm: 0.0,
                        scan_offset_mm: offset,
                    },
                    ScanSquareSpec {
                        power,
                        speed,
                        overscan_mm: fill.options.overscan_mm,
                        scan_offset_mm: offset,
                    },
                ]
            };
            let size = float_field(
                "square_mm",
                if test_type == "scan_offset" {
                    10.0
                } else {
                    15.0
                },
            )?;
            let note_text = if test_type == "scan_offset" {
                "Bidirectional scan offset test: offset 0, one speed per square"
            } else {
                "Overscan check: left square no overscan, right square current overscan"
            };
            let pattern = scan_squares_gcode(material, note_text, &specs, size, interval)
                .map_err(|e| invalid(format!("测试图案生成失败: {e}")))?;
            let cells: Vec<serde_json::Value> = pattern
                .squares
                .iter()
                .enumerate()
                .map(|(index, square)| {
                    let label = if test_type == "scan_offset" {
                        square.spec.speed.to_string()
                    } else if square.spec.overscan_mm > 0.0 {
                        format!("{:.1}mm", square.spec.overscan_mm)
                    } else {
                        "0".to_owned()
                    };
                    serde_json::json!({
                        "cell_number": index + 1,
                        "id": format!("P{}", index + 1),
                        "row": 0,
                        "column": index,
                        "x_mm": (square.x_mm * 1000.0).round() / 1000.0,
                        "y_mm": square.y_mm,
                        "size_mm": size,
                        "power": square.spec.power,
                        "speed": square.spec.speed,
                        "passes": 1,
                        "line_interval_mm": interval,
                        "overscan_mm": square.spec.overscan_mm,
                        "scan_offset_mm": square.spec.scan_offset_mm,
                        "x_label": label,
                        "y_label": "",
                    })
                })
                .collect();
            let note = if test_type == "scan_offset" {
                serde_json::json!({
                    "line_interval_mm": interval,
                    "hint": "看每块左右两条边：来回两行出光错开的距离就是这个速度的偏移（行行都往行进方向拖后填正数）",
                })
            } else {
                serde_json::json!({
                    "line_interval_mm": interval,
                    "overscan_mm": specs[1].overscan_mm,
                    "overscan_auto": overscan_auto,
                    "scan_offset_mm": specs[0].scan_offset_mm,
                    "hint": "左块不过扫描、右块按当前设置；右块两头仍发黑就加大过扫描或填写机器加速度",
                })
            };
            (
                pattern.gcode,
                cells,
                pattern.width_mm,
                pattern.height_mm,
                size,
                note,
                if test_type == "scan_offset" {
                    "speed"
                } else {
                    "overscan"
                },
            )
        };

        let (rem_w, rem_h) = self.remembered_work_area(object);
        let max_w = crate::generation::parse_work_area_mm_or(
            object.get("work_area_width_mm"),
            "work_area_width_mm",
            rem_w,
        )
        .map_err(|e| (400, failure(e, None)))?;
        let max_h = crate::generation::parse_work_area_mm_or(
            object.get("work_area_height_mm"),
            "work_area_height_mm",
            rem_h,
        )
        .map_err(|e| (400, failure(e, None)))?;
        if width_mm > max_w + 0.01 || height_mm > max_h + 0.01 {
            return Err(invalid(format!(
                "测试图案约 {:.1}×{:.1} mm，超出当前行程 {:.0}×{:.0} mm",
                width_mm, height_mm, max_w, max_h
            )));
        }

        let root = self
            .generated_root
            .as_deref()
            .or(self.upload_root.as_deref())
            .ok_or_else(|| (500, failure("生成目录未配置", None)))?;
        std::fs::create_dir_all(root)
            .map_err(|e| (500, failure(format!("创建生成目录失败: {e}"), None)))?;
        let calibration_id = format!("cal-{}", next_workflow_id().trim_start_matches("wf-"));
        let gcode_name = format!("{calibration_id}.gcode");
        std::fs::write(root.join(&gcode_name), gcode.as_bytes())
            .map_err(|e| (500, failure(format!("写入测试 G-code 失败: {e}"), None)))?;
        let gcode_sha = hex::encode(Sha256::digest(gcode.as_bytes()));
        let estimate = laser_core::gcode::estimate_time_with(&gcode, &motion.profile);
        let round_mm = |v: f64| (v * 1000.0).round() / 1000.0;
        let now = unix_seconds();
        let (laser_mode, engraving_mode, task_type) = if is_kerf {
            ("cut", "", "cut_contour")
        } else {
            ("engrave", "raster", "engrave_logo")
        };
        let cell_count = cells.len();
        let mut record = serde_json::json!({
            "calibration_id": calibration_id,
            "status": "prepared",
            "material": material,
            "thickness_mm": thickness_mm,
            "laser_mode": laser_mode,
            "engraving_mode": engraving_mode,
            "matrix_kind": laser_mode,
            "task_type": task_type,
            "test_type": test_type,
            "wizard_pattern": true,
            "x_axis": x_axis,
            "rows": 1,
            "columns": cell_count,
            "power_min": power,
            "power_max": power,
            "speed_min": cells.iter().filter_map(|c| c["speed"].as_i64()).min().unwrap_or(speed as i64),
            "speed_max": cells.iter().filter_map(|c| c["speed"].as_i64()).max().unwrap_or(speed as i64),
            "passes": cells.first().and_then(|c| c["passes"].as_i64()).unwrap_or(1),
            "cells": cells,
            "cell_count": cell_count,
            "square_mm": size_mm,
            "width_mm": round_mm(width_mm),
            "height_mm": round_mm(height_mm),
            "pattern": pattern_note,
            "labels": {"enabled": false},
            // 测试参数取自哪个材料库条目（可能回退到“表面通用”或最近厚度）：向导保存结果写回同一条目
            "surface_entry": recommendation.surface,
            "matched_thickness_mm": recommendation.matched_thickness_mm,
            "gcode_file": gcode_name,
            "gcode_sha256": gcode_sha,
        });
        if let Some(map) = record.as_object_mut() {
            map.insert(
                "artifacts".into(),
                serde_json::json!({"gcode_file": gcode_name, "gcode_sha256": gcode_sha}),
            );
            map.insert(
                "confirmation_snapshot".into(),
                serde_json::json!({"gcode_file": gcode_name, "gcode_sha256": gcode_sha, "material": material, "thickness_mm": thickness_mm}),
            );
            map.insert(
                "time_estimate".into(),
                crate::generation::time_estimate_json(&estimate, &motion),
            );
            map.insert(
                "web".into(),
                serde_json::json!({"gcode_download_url": format!("/preview?path={gcode_name}&download=1")}),
            );
            map.insert("next_actions".into(), serde_json::json!(["send"]));
            map.insert("created_at".into(), serde_json::json!(now));
            map.insert("updated_at".into(), serde_json::json!(now));
            let max_pattern_speed = map
                .get("speed_max")
                .and_then(|v| v.as_f64())
                .unwrap_or(speed as f64);
            if let Some(warning) =
                crate::generation::feed_exceeds_max_rate_warning(max_pattern_speed, &motion)
            {
                map.insert("warnings".into(), serde_json::json!([warning.clone()]));
                if let Some(art) = map.get_mut("artifacts").and_then(|v| v.as_object_mut()) {
                    art.insert("warnings".into(), serde_json::json!([warning]));
                }
            }
        }
        self.apply_time_calibration(record.get_mut("time_estimate"), object);
        self.save_calibration(&record)
            .map_err(|e| (500, failure(format!("保存测试图案失败: {e}"), None)))?;
        Ok(record)
    }
}
