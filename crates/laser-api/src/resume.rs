pub(crate) struct ResumePlan {
    pub(crate) preamble: Vec<String>,
    pub(crate) modal_spindle_s: Option<f64>,
    pub(crate) resume_in_cut_motion: bool,
    pub(crate) requires_physical_origin: bool,
}

pub(crate) fn parse_mpos_value(val: Option<&serde_json::Value>) -> Option<(f64, f64, f64)> {
    let arr = val?.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    let x = arr.first()?.as_f64()?;
    let y = arr.get(1)?.as_f64()?;
    let z = arr.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
    (x.is_finite() && y.is_finite() && z.is_finite()).then_some((x, y, z))
}

fn parse_gcode_words(line: &str) -> Vec<(char, f64)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('$') || trimmed.starts_with(';') {
        return Vec::new();
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut words = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b.is_ascii_alphabetic() {
            let letter = (b as char).to_ascii_uppercase();
            i += 1;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            let start = i;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i > start
                && let Ok(num_str) = std::str::from_utf8(&bytes[start..i])
                && let Ok(val) = num_str.parse::<f64>()
                && val.is_finite()
            {
                words.push((letter, val));
            }
        } else {
            i += 1;
        }
    }
    words
}

/// 从断点前已确认行 `prefix` 重建 `0x18` 软复位丢失的控制器模态头与精确 `G92` 偏移。
/// 所有预热恢复行均使用 `S0`（零功率），绝不在静止或快速归位期间出光。
pub(crate) fn build_resume_plan(
    prefix: &[String],
    origin_mpos: Option<(f64, f64, f64)>,
    current_mpos: Option<(f64, f64, f64)>,
) -> ResumePlan {
    let mut units: Option<&'static str> = None;
    let mut plane: Option<&'static str> = None;
    let mut wcs: Option<&'static str> = None;
    let mut feed_mode: Option<&'static str> = None;
    let mut distance_mode: &'static str = "G90";
    let mut motion_mode: &'static str = "G0";
    let mut spindle_mode: &'static str = "M5";
    let mut feed_rate: Option<f64> = None;
    let mut last_s: f64 = 0.0;

    let mut phys_x: f64 = 0.0;
    let mut phys_y: f64 = 0.0;
    let mut work_x: f64 = 0.0;
    let mut work_y: f64 = 0.0;
    let mut offset_x: f64 = 0.0;
    let mut offset_y: f64 = 0.0;
    let mut has_g92_x = false;
    let mut has_g92_y = false;
    let mut known_work_x = false;
    let mut known_work_y = false;

    for line in prefix {
        let words = parse_gcode_words(line);
        if words.is_empty() {
            continue;
        }
        let mut is_g92 = false;
        let mut is_g92_reset = false;
        let mut non_modal_no_track = false;
        let mut x_word: Option<f64> = None;
        let mut y_word: Option<f64> = None;

        for &(letter, val) in &words {
            match letter {
                'G' => {
                    if (val - 92.1).abs() < 1e-4 || (val - 92.2).abs() < 1e-4 {
                        is_g92_reset = true;
                    } else if (val - 92.0).abs() < 1e-4 {
                        is_g92 = true;
                    } else {
                        let code = val.round() as i32;
                        if (val - f64::from(code)).abs() < 1e-4 {
                            match code {
                                0 => motion_mode = "G0",
                                1 => motion_mode = "G1",
                                2 => motion_mode = "G2",
                                3 => motion_mode = "G3",
                                4 | 10 | 28 | 30 | 53 => non_modal_no_track = true,
                                17 => plane = Some("G17"),
                                18 => plane = Some("G18"),
                                19 => plane = Some("G19"),
                                20 => units = Some("G20"),
                                21 => units = Some("G21"),
                                54 => wcs = Some("G54"),
                                55 => wcs = Some("G55"),
                                56 => wcs = Some("G56"),
                                57 => wcs = Some("G57"),
                                58 => wcs = Some("G58"),
                                59 => wcs = Some("G59"),
                                90 => distance_mode = "G90",
                                91 => distance_mode = "G91",
                                93 => feed_mode = Some("G93"),
                                94 => feed_mode = Some("G94"),
                                _ => {}
                            }
                        }
                    }
                }
                'M' => match val.round() as i32 {
                    3 => spindle_mode = "M3",
                    4 => spindle_mode = "M4",
                    5 => {
                        spindle_mode = "M5";
                        last_s = 0.0;
                    }
                    _ => {}
                },
                'F' if val > 0.0 => feed_rate = Some(val),
                'S' if val >= 0.0 => last_s = val,
                'X' => x_word = Some(val),
                'Y' => y_word = Some(val),
                _ => {}
            }
        }

        if is_g92_reset {
            has_g92_x = false;
            has_g92_y = false;
            offset_x = 0.0;
            offset_y = 0.0;
            work_x = phys_x;
            work_y = phys_y;
        } else if is_g92 {
            if let Some(gx) = x_word {
                has_g92_x = true;
                known_work_x = true;
                work_x = gx;
                offset_x = gx - phys_x;
            }
            if let Some(gy) = y_word {
                has_g92_y = true;
                known_work_y = true;
                work_y = gy;
                offset_y = gy - phys_y;
            }
        } else if !non_modal_no_track {
            if let Some(tx) = x_word {
                if distance_mode == "G90" {
                    let dx = tx - work_x;
                    phys_x += dx;
                    work_x = tx;
                    known_work_x = true;
                } else {
                    phys_x += tx;
                    work_x += tx;
                }
            }
            if let Some(ty) = y_word {
                if distance_mode == "G90" {
                    let dy = ty - work_y;
                    phys_y += dy;
                    work_y = ty;
                    known_work_y = true;
                } else {
                    phys_y += ty;
                    work_y += ty;
                }
            }
        }
    }

    let mut preamble = vec!["M5 S0".to_owned()];
    if let Some(u) = units {
        preamble.push(u.to_owned());
    }
    if let Some(p) = plane {
        preamble.push(p.to_owned());
    }
    if let Some(w) = wcs {
        preamble.push(w.to_owned());
    }
    if let Some(fm) = feed_mode {
        preamble.push(fm.to_owned());
    }
    preamble.push("G90".to_owned());

    if has_g92_x || has_g92_y {
        let (cur_wx, cur_wy) = match (origin_mpos, current_mpos) {
            (Some((ox, oy, _)), Some((cx, cy, _))) if units != Some("G20") => {
                (offset_x + (cx - ox), offset_y + (cy - oy))
            }
            _ => (work_x, work_y),
        };
        let mut g92 = String::from("G92");
        if has_g92_x {
            g92.push_str(&format!(" X{cur_wx:.3}"));
        }
        if has_g92_y {
            g92.push_str(&format!(" Y{cur_wy:.3}"));
        }
        preamble.push(g92);
    }

    if known_work_x || known_work_y {
        let mut g0 = String::from("G0");
        if known_work_x {
            g0.push_str(&format!(" X{work_x:.3}"));
        }
        if known_work_y {
            g0.push_str(&format!(" Y{work_y:.3}"));
        }
        preamble.push(g0);
    }
    if (!known_work_x || !known_work_y)
        && units != Some("G20")
        && let (Some((ox, oy, _)), Some((cx, cy, _))) = (origin_mpos, current_mpos)
    {
        let dx = (ox + phys_x) - cx;
        let dy = (oy + phys_y) - cy;
        let need_x = !known_work_x && dx.abs() > 1e-4;
        let need_y = !known_work_y && dy.abs() > 1e-4;
        if need_x || need_y {
            let mut rel_g0 = String::from("G0");
            if need_x {
                rel_g0.push_str(&format!(" X{dx:.3}"));
            }
            if need_y {
                rel_g0.push_str(&format!(" Y{dy:.3}"));
            }
            preamble.push("G91".to_owned());
            preamble.push(rel_g0);
            preamble.push("G90".to_owned());
        }
    }

    if let Some(f) = feed_rate {
        preamble.push(format!("G1 F{f:.3} S0"));
        if motion_mode == "G0" {
            preamble.push("G0".to_owned());
        }
    } else if motion_mode != "G0" {
        preamble.push("G1 S0".to_owned());
    }

    if spindle_mode != "M5" {
        preamble.push(format!("{spindle_mode} S0"));
    }
    if distance_mode == "G91" {
        preamble.push("G91".to_owned());
    }

    let modal_spindle_s = (spindle_mode != "M5" && last_s > 0.0).then_some(last_s);
    let requires_physical_origin = has_g92_x
        || has_g92_y
        || (!known_work_x && phys_x.abs() > 1e-4)
        || (!known_work_y && phys_y.abs() > 1e-4);
    ResumePlan {
        preamble,
        modal_spindle_s,
        resume_in_cut_motion: motion_mode != "G0",
        requires_physical_origin,
    }
}

/// 当断点前处于 M3/M4 且有非零模态 `S`，而续雕起始切割行依赖模态 `S`（未显式写 `S`）时，
/// 在首条切割运动行补上模态 `S`，既不增加行数也不在静止期出光。
pub(crate) fn apply_modal_spindle_to_remaining(
    remaining: &mut [String],
    modal_s: Option<f64>,
    mut in_cut_motion: bool,
) {
    let Some(s_val) = modal_s else {
        return;
    };
    for line in remaining.iter_mut() {
        let words = parse_gcode_words(line);
        if words.is_empty() {
            continue;
        }
        let mut has_s = false;
        let mut has_m5 = false;
        let mut has_xy = false;
        for &(letter, val) in &words {
            match letter {
                'S' => has_s = true,
                'M' if val.round() as i32 == 5 => has_m5 = true,
                'G' => match val.round() as i32 {
                    0 if (val - 0.0).abs() < 1e-4 => in_cut_motion = false,
                    1..=3 if (val - val.round()).abs() < 1e-4 => in_cut_motion = true,
                    _ => {}
                },
                'X' | 'Y' => has_xy = true,
                _ => {}
            }
        }
        if has_m5 || has_s {
            break;
        }
        if in_cut_motion && has_xy {
            let formatted_s = if (s_val - s_val.round()).abs() < 1e-6 {
                format!("{}", s_val.round() as i64)
            } else {
                format!("{s_val:.3}")
            };
            line.push_str(&format!(" S{formatted_s}"));
            break;
        }
    }
}
