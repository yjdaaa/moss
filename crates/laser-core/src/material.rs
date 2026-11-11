use laser_contracts::{ParamCell, builtin_material_library};

#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    pub material: String,
    pub requested_thickness_mm: f64,
    pub matched_thickness_mm: f64,
    pub laser_mode: String,
    pub engraving_mode: Option<String>,
    pub params: ParamCell,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationCell {
    pub id: String,
    pub row: usize,
    pub column: usize,
    pub x_mm: f64,
    pub y_mm: f64,
    pub size_mm: f64,
    pub power: i32,
    pub speed: i32,
    pub passes: i32,
    pub pixel_size_mm: Option<f64>,
    pub line_interval_mm: Option<f64>,
}

pub fn recommend(
    material: &str,
    thickness_mm: f64,
    laser_mode: &str,
    engraving_mode: Option<&str>,
) -> Option<Recommendation> {
    recommend_with_scale(material, thickness_mm, laser_mode, engraving_mode, 1000.0)
}

pub fn recommend_with_scale(
    material: &str,
    thickness_mm: f64,
    laser_mode: &str,
    engraving_mode: Option<&str>,
    power_scale: f64,
) -> Option<Recommendation> {
    recommend_in_library(
        &builtin_material_library(),
        material,
        thickness_mm,
        laser_mode,
        engraving_mode,
        power_scale,
    )
}

pub fn recommend_in_library(
    library: &laser_contracts::MaterialLibrary,
    material: &str,
    thickness_mm: f64,
    laser_mode: &str,
    engraving_mode: Option<&str>,
    power_scale: f64,
) -> Option<Recommendation> {
    if !thickness_mm.is_finite() || thickness_mm <= 0.0 {
        return None;
    }
    let (name, entry) = library.materials.iter().find(|(name, entry)| {
        name.eq_ignore_ascii_case(material)
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(material))
    })?;
    if laser_mode.eq_ignore_ascii_case("cut")
        && !entry.thicknesses.keys().any(|value| {
            value
                .parse::<f64>()
                .is_ok_and(|candidate| (candidate - thickness_mm).abs() <= 1e-9)
        })
    {
        return None;
    }
    let (thickness, modes) = entry.thicknesses.iter().min_by(|(a, _), (b, _)| {
        let da = (a.parse::<f64>().unwrap_or(f64::MAX) - thickness_mm).abs();
        let db = (b.parse::<f64>().unwrap_or(f64::MAX) - thickness_mm).abs();
        da.total_cmp(&db)
    })?;
    let normalized_mode = laser_mode.to_ascii_lowercase();
    let (params, engraving) = if normalized_mode == "cut" {
        (modes.cut.as_ref()?, None)
    } else if normalized_mode == "engrave" {
        let mode = engraving_mode.unwrap_or("raster");
        let params = match mode {
            "raster" => modes.engrave.as_ref()?.raster.as_ref()?,
            "outline" => modes.engrave.as_ref()?.outline.as_ref()?,
            _ => return None,
        };
        (params, Some(mode.to_owned()))
    } else {
        return None;
    };
    Some(Recommendation {
        material: name.to_owned(),
        requested_thickness_mm: thickness_mm,
        matched_thickness_mm: thickness.parse().ok()?,
        laser_mode: normalized_mode,
        engraving_mode: engraving,
        params: if (power_scale - 1000.0).abs() > 1e-6 && power_scale > 0.0 {
            params.rescaled_to(power_scale)
        } else {
            params.clone()
        },
    })
}

pub fn calibration_matrix(
    rows: usize,
    columns: usize,
    power_min: i32,
    power_max: i32,
    speed_min: i32,
    speed_max: i32,
    passes: i32,
) -> Result<Vec<CalibrationCell>, &'static str> {
    if rows == 0 || columns == 0 || rows > 25 || columns > 25 || passes < 1 {
        return Err("invalid calibration matrix dimensions");
    }
    let interpolate = |min: i32, max: i32, index: usize, count: usize| {
        if count == 1 {
            min
        } else {
            min + ((max - min) as f64 * index as f64 / (count - 1) as f64).round() as i32
        }
    };
    Ok((0..rows)
        .flat_map(|row| {
            (0..columns).map(move |column| CalibrationCell {
                id: format!("{}{}", (b'A' + row as u8) as char, column + 1),
                row: row + 1,
                column: column + 1,
                x_mm: column as f64 * 16.0,
                y_mm: row as f64 * 16.0,
                size_mm: 12.0,
                power: interpolate(power_min, power_max, column, columns),
                speed: interpolate(speed_min, speed_max, row, rows),
                passes,
                pixel_size_mm: None,
                line_interval_mm: None,
            })
        })
        .collect())
}

pub fn build_calibration_cells(
    task_type: &str,
    power: i32,
    speed: i32,
    passes: i32,
) -> Result<Vec<CalibrationCell>, &'static str> {
    if !matches!(task_type, "cut_contour" | "engrave_photo" | "engrave_logo") {
        return Err("unsupported calibration task type");
    }
    let mut cells = Vec::with_capacity(9);
    for row in 0..3 {
        for column in 0..3 {
            let pixel = if task_type == "cut_contour" {
                None
            } else {
                Some([0.16, 0.20, 0.24][column])
            };
            cells.push(CalibrationCell {
                id: format!("{}{}", (b'A' + row as u8) as char, column + 1),
                row: row + 1,
                column: column + 1,
                x_mm: column as f64 * 16.0,
                y_mm: row as f64 * 16.0,
                size_mm: 12.0,
                power: (power + [-40, 0, 40][column]).clamp(1, 500),
                speed: (speed + [200, 0, -200][row]).clamp(300, 3000),
                passes: if task_type == "cut_contour" {
                    (passes + column as i32 - 1).clamp(1, 5)
                } else {
                    passes
                },
                pixel_size_mm: pixel,
                line_interval_mm: pixel,
            });
        }
    }
    Ok(cells)
}

pub fn build_calibration_gcode(
    material: &str,
    task_type: &str,
    cells: &[CalibrationCell],
) -> Result<String, &'static str> {
    if material.is_empty() || cells.is_empty() {
        return Err("calibration requires material and cells");
    }
    let mut lines = vec![
        "; Generated by ai-laser-gcode-nc".to_owned(),
        "; Calibration matrix only; observe sample results before saving verified parameters"
            .to_owned(),
        format!("; Material: {material}"),
        format!("; Task type: {task_type}"),
        "G21".into(),
        "G90".into(),
        "G94".into(),
        "G17".into(),
        "G92 X0 Y0 Z0".into(),
        "M4 S0".into(),
        "G0 F3000".into(),
    ];
    for cell in cells {
        lines.push(format!(
            "; Cell {}: S{} F{} passes={}",
            cell.id, cell.power, cell.speed, cell.passes
        ));
        for _ in 0..cell.passes {
            let (x0, y0) = (cell.x_mm, cell.y_mm);
            let (x1, y1) = (x0 + cell.size_mm, y0 + cell.size_mm);
            lines.extend(["M5".into(), format!("G0 X{x0:.3} Y{y0:.3}"), "M4 S0".into()]);
            if matches!(task_type, "cut_contour" | "engrave_outline") {
                lines.extend([
                    format!("G1 X{x1:.3} Y{y0:.3} S{} F{}", cell.power, cell.speed),
                    format!("G1 X{x1:.3} Y{y1:.3} S{} F{}", cell.power, cell.speed),
                    format!("G1 X{x0:.3} Y{y1:.3} S{} F{}", cell.power, cell.speed),
                    format!("G1 X{x0:.3} Y{y0:.3} S{} F{}", cell.power, cell.speed),
                ]);
            } else {
                let step = cell.line_interval_mm.unwrap_or(0.2);
                let mut y = y0;
                while y <= y1 + 1e-9 {
                    lines.push(format!("G0 X{x0:.3} Y{y:.3}"));
                    lines.push(format!(
                        "G1 X{x1:.3} Y{y:.3} S{} F{}",
                        cell.power, cell.speed
                    ));
                    y += step;
                }
            }
        }
        lines.push("S0".into());
    }
    lines.extend(["M5".into(), "M2".into()]);
    Ok(lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recommends_builtin_materials_and_aliases() {
        let result = recommend("basswood", 3.0, "engrave", Some("outline")).unwrap();
        assert_eq!(result.material, "椴木");
        assert_eq!(result.params.laser_max_power, 420);
    }
    #[test]
    fn builds_deterministic_calibration_grid() {
        let grid = calibration_matrix(3, 3, 80, 160, 1000, 1400, 1).unwrap();
        assert_eq!(grid.len(), 9);
        assert_eq!(grid[0].power, 80);
        assert_eq!(grid[8].speed, 1400);
    }

    #[test]
    fn cut_does_not_silently_select_a_nearest_thickness() {
        assert!(recommend("椴木", 2.0, "cut", None).is_none());
        assert!(recommend("椴木", f64::NAN, "engrave", None).is_none());
    }

    #[test]
    fn calibration_cells_match_python_offsets_and_safe_clamps() {
        let cells = build_calibration_cells("cut_contour", 120, 1200, 1).unwrap();
        assert_eq!(cells[0].id, "A1");
        assert_eq!((cells[0].power, cells[0].speed), (80, 1400));
        assert_eq!(cells[2].passes, 2);
        assert_eq!(cells[8].y_mm, 32.0);
        assert!(
            build_calibration_gcode("椴木", "cut_contour", &cells)
                .unwrap()
                .ends_with("M5\nM2\n")
        );
    }

    #[test]
    fn recommends_custom_materials_from_supplied_library() {
        let mut lib = builtin_material_library();
        let mut entry = laser_contracts::MaterialEntry::default();
        entry.aliases.push("custom-bamboo".into());
        let th = laser_contracts::ThicknessEntry {
            cut: Some(ParamCell {
                laser_min_power: 0,
                laser_max_power: 750,
                feed_rate: 320,
                travel_rate: 3000,
                threshold: -1,
                passes: 2,
                pixel_size_mm: 0.1,
                power_scale: Some(1000),
                source: None,
                notes: None,
                updated_at: None,
                extra: Default::default(),
            }),
            ..Default::default()
        };
        entry.thicknesses.insert("5".into(), th);
        lib.materials.insert("特制楠竹".into(), entry);

        let res = recommend_in_library(&lib, "custom-bamboo", 5.0, "cut", None, 255.0).unwrap();
        assert_eq!(res.material, "特制楠竹");
        assert_eq!(res.laser_mode, "cut");
        assert_eq!(res.params.feed_rate, 320);
        assert_eq!(res.params.passes, 2);
        assert!(res.params.laser_max_power < 255);
    }
}
