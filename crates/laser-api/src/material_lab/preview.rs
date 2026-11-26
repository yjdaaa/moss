//! 材料实验室标定矩阵预览：真实生成单元格与 G-code，落盘、冻结 SHA 并入库（只生成不出光）。

use super::*;

impl ApiService {
    /// 标定矩阵预览：按行列/功率/速度区间真实生成单元格与 G-code，落盘、冻结 SHA 并入库。
    /// 只生成不出光；返回 `calibration_id`、cells 与 `gcode_file` 供发送与选格复用。
    pub(crate) fn material_lab_preview(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let material = object
            .get("material")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        if material.is_empty() {
            return Err((400, failure("material 不能为空", None)));
        }
        if let Err(error) = validate_material_safety(material) {
            return Err((
                400,
                failure(
                    error,
                    Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                ),
            ));
        }
        let thickness_mm =
            parse_float(object.get("thickness_mm"), "thickness_mm", 0.0).unwrap_or(0.0);
        if !thickness_mm.is_finite() || thickness_mm <= 0.0 {
            return Err((400, failure("thickness_mm 必须大于 0", None)));
        }
        let laser_mode = object
            .get("laser_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("engrave")
            .to_ascii_lowercase();
        if !matches!(laser_mode.as_str(), "engrave" | "cut") {
            return Err((400, failure("laser_mode 必须是 engrave 或 cut", None)));
        }
        let matrix_kind = object
            .get("matrix_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_else(|| laser_mode.clone());
        let engraving_mode = if laser_mode == "cut" {
            String::new()
        } else {
            object
                .get("engraving_mode")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or("raster")
                .to_ascii_lowercase()
        };
        // 测试类型（2026-11-16 对齐 LightBurn）：matrix = Material Test 参数网格（默认），
        // interval = Interval Test 一排不同线距的小填充方块
        let test_type = object
            .get("test_type")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("matrix")
            .to_ascii_lowercase();
        // 首次标定向导的测试图案（r40）：双向扫描偏移、过扫描检查、切缝，另行生成
        if crate::material_lab_patterns::WIZARD_PATTERN_TEST_TYPES.contains(&test_type.as_str()) {
            return self.material_lab_pattern_preview(object, material, thickness_mm, &test_type);
        }
        let is_interval_test = match test_type.as_str() {
            "matrix" => false,
            "interval" => true,
            _ => {
                return Err((
                    400,
                    failure(
                        "test_type 只能是 matrix、interval、scan_offset、overscan 或 kerf",
                        Some(serde_json::json!({"error_code": "invalid_test_type"})),
                    ),
                ));
            }
        };
        if is_interval_test && (laser_mode != "engrave" || engraving_mode != "raster") {
            return Err((
                400,
                failure(
                    "线距测试只用于光栅 / 填充雕刻（laser_mode=engrave、engraving_mode=raster）",
                    Some(serde_json::json!({"error_code": "axis_not_supported"})),
                ),
            ));
        }
        let surface_entry = crate::materials::is_surface_request(object);
        if surface_entry && laser_mode == "cut" {
            return Err((
                400,
                failure("表面通用条目只能存雕刻参数，切割标定必须按真实厚度", None),
            ));
        }
        let (rem_w, rem_h) = self.remembered_work_area();
        let max_work_area_w = crate::generation::parse_work_area_mm_or(
            object.get("work_area_width_mm"),
            "work_area_width_mm",
            rem_w,
        )
        .map_err(|e| (400, failure(e, None)))?;
        let max_work_area_h = crate::generation::parse_work_area_mm_or(
            object.get("work_area_height_mm"),
            "work_area_height_mm",
            rem_h,
        )
        .map_err(|e| (400, failure(e, None)))?;
        let rows = parse_int(
            object.get("rows"),
            "rows",
            if is_interval_test { 1 } else { 5 },
        )
        .map_err(|e| (400, failure(e.to_string(), None)))?;
        let columns = parse_int(
            object.get("columns"),
            "columns",
            if is_interval_test { 9 } else { 5 },
        )
        .map_err(|e| (400, failure(e.to_string(), None)))?;
        if is_interval_test {
            if rows != 1 || !(2..=20).contains(&columns) {
                return Err((
                    400,
                    failure(
                        "线距测试固定 1 行，columns（方块数）必须在 2..20 之间",
                        None,
                    ),
                ));
            }
        } else if !(1..=25).contains(&rows) || !(1..=25).contains(&columns) {
            return Err((400, failure("rows/columns 必须在 1..25 之间", None)));
        }
        // 区间缺省（-1 或缺失）时以材料推荐参数为中心取 ±40% 区间
        let effective_library = self.effective_material_library().map_err(|error| {
            (
                500,
                failure(
                    "读取材料库失败",
                    Some(serde_json::json!({ "error": error })),
                ),
            )
        })?;
        let recommendation = laser_core::material::recommend_in_library(
            &effective_library,
            material,
            thickness_mm,
            &laser_mode,
            if laser_mode == "cut" {
                None
            } else {
                Some(engraving_mode.as_str())
            },
            1000.0,
        );
        let (base_power, base_speed, base_passes) = recommendation
            .as_ref()
            .map(|r| {
                (
                    r.params.laser_max_power,
                    r.params.feed_rate,
                    r.params.passes.max(1),
                )
            })
            .unwrap_or((400, 1500, 1));
        let bound = |key: &str, fallback: i32| -> Result<i32, (u16, serde_json::Value)> {
            let value = parse_int(object.get(key), key, -1)
                .map_err(|e| (400, failure(e.to_string(), None)))?;
            Ok(if value < 0 { fallback } else { value as i32 })
        };
        let power_min = bound("power_min", (base_power as f64 * 0.6).round() as i32)?;
        let power_max = bound(
            "power_max",
            (base_power as f64 * 1.4).round().min(1000.0) as i32,
        )?;
        let speed_min = bound(
            "speed_min",
            (base_speed as f64 * 0.6).round().max(100.0) as i32,
        )?;
        let speed_max = bound("speed_max", (base_speed as f64 * 1.4).round() as i32)?;
        let passes = {
            let value = parse_int(object.get("passes"), "passes", 0)
                .map_err(|e| (400, failure(e.to_string(), None)))?;
            if value <= 0 {
                base_passes
            } else {
                value as i32
            }
        };
        if power_min > power_max || speed_min > speed_max || power_min < 0 || speed_min <= 0 {
            return Err((
                400,
                failure("功率/速度区间无效：min 必须 ≤ max 且为正数", None),
            ));
        }
        let task_type = if laser_mode == "cut" {
            "cut_contour"
        } else if engraving_mode == "outline" {
            "engrave_outline"
        } else {
            "engrave_logo"
        };
        let invalid = |message: String| (400, failure(message, None));
        let float_field = |key: &str, fallback: f64| {
            parse_float(object.get(key), key, fallback).map_err(|e| invalid(e.to_string()))
        };
        let int_field = |key: &str, fallback: i64| {
            parse_int(object.get(key), key, fallback).map_err(|e| invalid(e.to_string()))
        };
        // 两个轴各选一个参数（对齐 LightBurn Material Test）；默认列 = 功率、行 = 速度
        let axis_field = |key: &str, fallback: TestAxis| -> Result<TestAxis, _> {
            match object.get(key).and_then(|v| v.as_str()).map(str::trim) {
                None | Some("") => Ok(fallback),
                Some(raw) => TestAxis::parse(raw).ok_or_else(|| {
                    (
                        400,
                        failure(
                            format!("{key} 只能是 power / speed / interval / passes"),
                            Some(serde_json::json!({"error_code": "invalid_axis"})),
                        ),
                    )
                }),
            }
        };
        use laser_core::material::TestAxis;
        let (x_axis, y_axis) = if is_interval_test {
            (TestAxis::Interval, TestAxis::Speed)
        } else {
            (
                axis_field("x_axis", TestAxis::Power)?,
                axis_field("y_axis", TestAxis::Speed)?,
            )
        };
        if !is_interval_test && x_axis == y_axis {
            return Err((
                400,
                failure(
                    "x_axis 与 y_axis 必须是两个不同的参数",
                    Some(serde_json::json!({"error_code": "invalid_axis"})),
                ),
            ));
        }
        let uses = |axis: TestAxis| x_axis == axis || y_axis == axis;
        if uses(TestAxis::Interval) && task_type != "engrave_logo" {
            return Err((
                400,
                failure(
                    "线距轴只用于光栅 / 填充雕刻；轮廓与切割没有扫描线距",
                    Some(serde_json::json!({"error_code": "axis_not_supported"})),
                ),
            ));
        }
        let recommended_interval = recommendation
            .as_ref()
            .map(|r| r.params.pixel_size_mm)
            .filter(|v| {
                v.is_finite()
                    && (laser_core::material::TEST_INTERVAL_RANGE_MM.0
                        ..=laser_core::material::TEST_INTERVAL_RANGE_MM.1)
                        .contains(v)
            });
        // 不在轴上的线距：填充类测试默认用材料推荐线距（与实际雕刻一致），可用 line_interval_mm 覆盖
        let fixed_interval_mm = if task_type == "engrave_logo" {
            match object.get("line_interval_mm") {
                None | Some(serde_json::Value::Null) => recommended_interval,
                Some(_) => Some(float_field("line_interval_mm", 0.0)?),
            }
        } else {
            None
        };
        let fixed_power = int_field("power", base_power as i64)? as i32;
        let fixed_speed = int_field("speed", base_speed as i64)? as i32;
        if !(0..=1000).contains(&fixed_power) || fixed_speed <= 0 {
            return Err(invalid("power 必须在 0..1000，speed 必须为正数".to_owned()));
        }
        let interval_min_mm = float_field("interval_min_mm", 0.08)?;
        let interval_max_mm = float_field("interval_max_mm", 0.16)?;
        let passes_min = int_field("passes_min", passes as i64)? as i32;
        let passes_max = int_field(
            "passes_max",
            (passes_min as i64 + 2).min(laser_core::material::TEST_PASSES_RANGE.1 as i64),
        )? as i32;
        let defaults = if is_interval_test {
            laser_core::material::MaterialTestSpec::interval_test(
                columns as usize,
                (interval_min_mm, interval_max_mm),
                fixed_power,
                fixed_speed,
                passes,
            )
        } else {
            laser_core::material::MaterialTestSpec::power_speed(
                rows as usize,
                columns as usize,
                (power_min, power_max),
                (speed_min, speed_max),
                passes,
            )
        };
        let spec = laser_core::material::MaterialTestSpec {
            x_axis,
            y_axis,
            power: if is_interval_test {
                defaults.power
            } else {
                (power_min, power_max)
            },
            speed: if is_interval_test {
                defaults.speed
            } else {
                (speed_min, speed_max)
            },
            interval_mm: (interval_min_mm, interval_max_mm),
            passes: if uses(TestAxis::Passes) {
                (passes_min, passes_max)
            } else {
                (passes, passes)
            },
            fixed_power,
            fixed_speed,
            fixed_interval_mm,
            fixed_passes: passes,
            cell_size_mm: float_field("cell_size_mm", defaults.cell_size_mm)?,
            gap_mm: float_field("cell_gap_mm", defaults.gap_mm)?,
            ..defaults
        };
        let cells = laser_core::material::material_test_cells(&spec)
            .map_err(|e| invalid(format!("标定矩阵参数无效: {e}")))?;
        // 加工顺序：默认从烧蚀风险最低到最高（高速、低功率、宽线距、少遍数先刻）
        let run_order = object
            .get("run_order")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("risk")
            .to_ascii_lowercase();
        let order: Vec<usize> = match run_order.as_str() {
            "risk" => laser_core::material::risk_order(&cells),
            "grid" => (0..cells.len()).collect(),
            _ => return Err(invalid("run_order 只能是 risk 或 grid".to_owned())),
        };
        // 行列标注（默认开）：单线字体，默认用风险最低那格的功率（不超过 30%）与不低于 1500 mm/min 的速度
        let labels_enabled = parse_bool(object.get("labels"), true);
        let lowest = &cells[order[0]];
        let label_power = int_field("label_power", lowest.power.min(300) as i64)? as i32;
        let label_speed = int_field("label_speed", lowest.speed.max(1500) as i64)? as i32;
        let label_height_mm =
            float_field("label_height_mm", if is_interval_test { 2.5 } else { 3.0 })?;
        if labels_enabled && (!(0..=1000).contains(&label_power) || label_speed <= 0) {
            return Err(invalid(
                "label_power 必须在 0..1000，label_speed 必须为正数".to_owned(),
            ));
        }
        let label_spec = laser_core::material::TestLabels {
            height_mm: label_height_mm,
            power: label_power,
            speed: label_speed,
        };
        let layout = laser_core::material::layout_material_test(
            cells,
            x_axis,
            y_axis,
            labels_enabled.then_some(&label_spec),
        )
        .map_err(|e| invalid(format!("标定标注参数无效: {e}")))?;
        if layout.width_mm > max_work_area_w + 0.01 || layout.height_mm > max_work_area_h + 0.01 {
            return Err(invalid(format!(
                "矩阵约 {:.1}×{:.1} mm，超出当前行程 {:.0}×{:.0} mm，请减少行列或缩小单元",
                layout.width_mm, layout.height_mm, max_work_area_w, max_work_area_h
            )));
        }
        let cells = layout.cells;
        let gcode = laser_core::material::build_material_test_gcode(
            material,
            task_type,
            &cells,
            &order,
            labels_enabled.then_some(laser_core::material::LabelBurn {
                paths: &layout.labels,
                power: label_power,
                speed: label_speed,
            }),
        )
        .map_err(|e| invalid(format!("标定 G-code 生成失败: {e}")))?;
        let mut run_index = vec![0usize; cells.len()];
        for (position, &index) in order.iter().enumerate() {
            run_index[index] = position + 1;
        }
        // 记录里的功率 / 速度区间按实际单元格取值（参数不在轴上时 min = max = 固定值）
        let extent = |values: Vec<i32>| {
            (
                values.iter().copied().min().unwrap_or(0),
                values.iter().copied().max().unwrap_or(0),
            )
        };
        let (power_min, power_max) = if uses(TestAxis::Power) && !is_interval_test {
            (power_min, power_max)
        } else {
            extent(cells.iter().map(|c| c.power).collect())
        };
        let (speed_min, speed_max) = if uses(TestAxis::Speed) && !is_interval_test {
            (speed_min, speed_max)
        } else {
            extent(cells.iter().map(|c| c.speed).collect())
        };
        let axis_json = |axis: TestAxis, count: usize| {
            let (min, max, unit) = match axis {
                TestAxis::Power => (
                    serde_json::json!(spec.power.0),
                    serde_json::json!(spec.power.1),
                    "S (0-1000)",
                ),
                TestAxis::Speed => (
                    serde_json::json!(spec.speed.0),
                    serde_json::json!(spec.speed.1),
                    "mm/min",
                ),
                TestAxis::Interval => (
                    serde_json::json!(spec.interval_mm.0),
                    serde_json::json!(spec.interval_mm.1),
                    "mm",
                ),
                TestAxis::Passes => (
                    serde_json::json!(spec.passes.0),
                    serde_json::json!(spec.passes.1),
                    "passes",
                ),
            };
            serde_json::json!({"param": axis.as_str(), "count": count, "min": min, "max": max, "unit": unit})
        };
        let axes = serde_json::json!({
            "x": axis_json(x_axis, columns as usize),
            "y": axis_json(y_axis, rows as usize),
        });

        let root = self
            .generated_root
            .as_deref()
            .or(self.upload_root.as_deref())
            .ok_or_else(|| (500, failure("生成目录未配置", None)))?;
        std::fs::create_dir_all(root)
            .map_err(|e| (500, failure(format!("创建生成目录失败: {e}"), None)))?;
        let calibration_id = format!("cal-{}", next_workflow_id().trim_start_matches("wf-"));
        let gcode_name = format!("{calibration_id}.gcode");
        let gcode_path = root.join(&gcode_name);
        std::fs::write(&gcode_path, gcode.as_bytes())
            .map_err(|e| (500, failure(format!("写入标定 G-code 失败: {e}"), None)))?;
        let gcode_sha = hex::encode(Sha256::digest(gcode.as_bytes()));
        let motion =
            crate::generation::parse_machine_motion(object).map_err(|e| (400, failure(e, None)))?;
        let estimate = laser_core::gcode::estimate_time_with(&gcode, &motion.profile);
        let cells_json = cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                serde_json::json!({
                    "cell_number": index + 1,
                    "id": cell.id,
                    "row": cell.row,
                    "column": cell.column,
                    "x_mm": cell.x_mm,
                    "y_mm": cell.y_mm,
                    "size_mm": cell.size_mm,
                    "power": cell.power,
                    "speed": cell.speed,
                    "passes": cell.passes,
                    "pixel_size_mm": cell.pixel_size_mm,
                    "line_interval_mm": cell.line_interval_mm,
                    "run_index": run_index[index],
                    "x_label": x_axis.value_label(cell),
                    "y_label": y_axis.value_label(cell),
                })
            })
            .collect::<Vec<_>>();
        let run_sequence: Vec<usize> = order.iter().map(|&index| index + 1).collect();
        let round_mm = |v: f64| (v * 1000.0).round() / 1000.0;
        let width_mm = round_mm(layout.width_mm);
        let height_mm = round_mm(layout.height_mm);
        let now = unix_seconds();
        // 2026-11-16 新增的测试描述字段（分开构造，避免单个 json! 宏递归过深）
        let test_meta = serde_json::json!({
            "test_type": test_type,
            "x_axis": x_axis.as_str(),
            "y_axis": y_axis.as_str(),
            "axes": axes,
            "fixed": {
                "power": fixed_power,
                "speed": fixed_speed,
                "passes": passes,
                "line_interval_mm": fixed_interval_mm,
            },
            "interval_min_mm": spec.interval_mm.0,
            "interval_max_mm": spec.interval_mm.1,
            "passes_min": spec.passes.0,
            "passes_max": spec.passes.1,
            "cell_size_mm": spec.cell_size_mm,
            "cell_gap_mm": spec.gap_mm,
            "run_order": run_order,
            "run_sequence": run_sequence,
            "labels": {
                "enabled": labels_enabled && !layout.labels.is_empty(),
                "height_mm": label_height_mm,
                "power": label_power,
                "speed": label_speed,
            },
            "surface_entry": surface_entry,
        });
        let mut record = serde_json::json!({
            "calibration_id": calibration_id,
            "status": "prepared",
            "material": material,
            "thickness_mm": thickness_mm,
            "laser_mode": laser_mode,
            "engraving_mode": engraving_mode,
            "matrix_kind": matrix_kind,
            "task_type": task_type,
            "rows": rows,
            "columns": columns,
            "power_min": power_min,
            "power_max": power_max,
            "speed_min": speed_min,
            "speed_max": speed_max,
            "passes": passes,
            "cells": cells_json,
            "cell_count": cells.len(),
            "width_mm": width_mm,
            "height_mm": height_mm,
            "gcode_file": gcode_name,
            "gcode_sha256": gcode_sha,
            "artifacts": {"gcode_file": gcode_name, "gcode_sha256": gcode_sha},
            "confirmation_snapshot": {"gcode_file": gcode_name, "gcode_sha256": gcode_sha, "material": material, "thickness_mm": thickness_mm},
            "time_estimate": crate::generation::time_estimate_json(&estimate, &motion),
            "web": {"gcode_download_url": format!("/preview?path={gcode_name}&download=1")},
            "next_actions": ["send", "select_cell"],
            "created_at": now,
            "updated_at": now,
        });
        if let (Some(map), serde_json::Value::Object(meta)) = (record.as_object_mut(), test_meta) {
            map.extend(meta);
            if let Some(warning) =
                crate::generation::feed_exceeds_max_rate_warning(speed_max as f64, &motion)
            {
                map.insert("warnings".into(), serde_json::json!([warning.clone()]));
                if let Some(art) = map.get_mut("artifacts").and_then(|v| v.as_object_mut()) {
                    art.insert("warnings".into(), serde_json::json!([warning]));
                }
            }
        }
        self.apply_time_calibration(record.get_mut("time_estimate"), object);
        self.save_calibration(&record)
            .map_err(|e| (500, failure(format!("保存标定矩阵失败: {e}"), None)))?;
        Ok(record)
    }
}
