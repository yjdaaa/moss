//! 文字预览：字形转矢量后按填充 / 描边 / 光栅 / 切割生成 G-code；可带基础图形、边框与文字区域
//! （2026-11-17 技术书 r44，见 [`super::shapes`]），文字可以为空。

use super::*;

fn parse_g92_origin(program: &str) -> (f64, f64) {
    program
        .lines()
        .find_map(|line| {
            let rest = line.trim().strip_prefix("G92 ")?;
            let mut x = 0.0;
            let mut y = 0.0;
            for token in rest.split_whitespace() {
                if let Some(v) = token.strip_prefix('X').or_else(|| token.strip_prefix('x'))
                    && let Ok(parsed) = v.parse::<f64>()
                {
                    x = parsed;
                } else if let Some(v) = token.strip_prefix('Y').or_else(|| token.strip_prefix('y'))
                    && let Ok(parsed) = v.parse::<f64>()
                {
                    y = parsed;
                }
            }
            Some((x, y))
        })
        .unwrap_or((0.0, 0.0))
}

fn shift_program_origin(program: &str, target_origin: (f64, f64)) -> String {
    let source_origin = parse_g92_origin(program);
    let dx = target_origin.0 - source_origin.0;
    let dy = target_origin.1 - source_origin.1;
    let clean = |v: f64| if v.abs() < 5e-4 { 0.0 } else { v };
    let (tx, ty) = (clean(target_origin.0), clean(target_origin.1));
    let mut out = String::with_capacity(program.len() + 64);
    for line in program.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("G92 ") {
            out.push_str(&format!("G92 X{tx:.3} Y{ty:.3} Z0\n"));
        } else if trimmed.starts_with("G0 ") || trimmed.starts_with("G1 ") {
            let mut tokens = Vec::new();
            for token in trimmed.split_whitespace() {
                if let Some(v) = token.strip_prefix('X')
                    && let Ok(x) = v.parse::<f64>()
                {
                    tokens.push(format!("X{:.3}", clean(x + dx)));
                } else if let Some(v) = token.strip_prefix('Y')
                    && let Ok(y) = v.parse::<f64>()
                {
                    tokens.push(format!("Y{:.3}", clean(y + dy)));
                } else {
                    tokens.push(token.to_owned());
                }
            }
            out.push_str(&tokens.join(" "));
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn append_stage_program(first: &str, second: &str) -> String {
    let mut out = String::with_capacity(first.len() + second.len() + 64);
    for line in first.lines() {
        if line.trim() == "M2" {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("; --- Stage 2: outer border cut ---\n");
    for line in second.lines() {
        if line.trim().starts_with("G92 ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub(crate) fn generate_text_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    generated_root: Option<&std::path::Path>,
    library: &laser_contracts::MaterialLibrary,
    // 全局雕刻路线（r45）：请求没写 `engrave_route` 时按它
    default_route: crate::engrave_route::EngraveRoute,
    // 全局对刀起点（r48）：请求没写 `origin` 时按它
    default_origin: laser_core::engine::TextOrigin,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    // 2026-11-16：工时按机器加速度估算（请求 `machine_motion`，缺省按默认值）
    let motion = parse_machine_motion(object)?;
    let text = object
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    let shapes = super::shapes::parse_shapes(object.get("shapes"))?;
    let border = super::shapes::parse_border(object.get("border"))?;
    let text_box = super::shapes::parse_text_box(object.get("text_box"))?;
    if text.is_empty() && shapes.is_empty() && border.is_none() {
        // 报错原文是兼容快照（tests/golden/api/text-generate.response.json），只刻图形时给 shapes 或 border
        return Err("text 不能为空".into());
    }
    let material = object
        .get("material")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    validate_material_safety(material)?;
    let thickness = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0)
        .map_err(|_| "thickness_mm 必须是大于 0 的数字")?;
    if material.is_empty() || !thickness.is_finite() || thickness <= 0.0 {
        return Err("thickness_mm 必须是大于 0 的数字".into());
    }
    // 只刻图形时不需要字体
    let font_path_buf = if text.is_empty() {
        std::path::PathBuf::new()
    } else {
        object
            .get("font_path")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| laser_core::font::find_system_font_for_text(text))
            .ok_or("font_path 必须指向可读的 TTF/TTC 字体")?
    };
    let font_path = font_path_buf.to_str().ok_or("非法字体路径")?;
    let font_size = parse_float(object.get("font_size_mm"), "font_size_mm", 10.0)
        .map_err(|error| error.to_string())?;
    let spacing = parse_float(object.get("letter_spacing_mm"), "letter_spacing_mm", 0.5)
        .map_err(|error| error.to_string())?;
    let raw_engraving_mode = object
        .get("engraving_mode")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let raw_laser_mode = object
        .get("laser_mode")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let is_cut = raw_engraving_mode.is_some_and(|m| m.eq_ignore_ascii_case("cut"))
        || raw_laser_mode.is_some_and(|m| m.eq_ignore_ascii_case("cut"));
    // 2026-11-27：铭牌“先雕内文、后切外框”（border_cut=true 或 border.cut=true）
    let border_cut_requested = parse_bool(object.get("border_cut"), false)
        || object
            .get("border")
            .and_then(|b| b.get("cut"))
            .is_some_and(|v| parse_bool(Some(v), false));
    if border_cut_requested && border.is_none() {
        return Err("开启“切下外框”需要先选择外框样式（矩形框、圆角矩形框、圆牌或椭圆牌）".into());
    }
    let border_cut = border_cut_requested && !is_cut;
    let border_cut_rec = if border_cut {
        Some(
            laser_core::material::recommend_in_library(
                library,
                material,
                thickness,
                "cut",
                None,
                1000.0,
            )
            .ok_or_else(|| {
                format!(
                    "材料库里还没有「{material} {thickness}mm」的切割参数，无法切下外框；请换用支持切割的厚度或先在材料实验室保存切割参数"
                )
            })?,
        )
    } else {
        None
    };
    // 文字刻法（2026-11-06）：默认只描轮廓（用户决定）；fill 实心填充用材料库 raster 参数
    // （2026-11-16 起改为矢量密排线，行距、速度与扫描填充同档），outline / hatch 用 outline 参数；
    // 有填满的图形时整件同样用 raster 参数（r44）
    let text_fill =
        laser_core::engine::TextFill::parse(object.get("text_fill").and_then(|v| v.as_str()))?;
    if is_cut && shapes.iter().any(|shape| shape.fill) {
        return Err("切割时图形不能填满，请去掉 fill".into());
    }
    // 雕刻路线（2026-11-17 技术书 r45）：转图片时文字一律实心、整件取光栅参数；外框切割始终走矢量
    let requested_route = crate::engrave_route::EngraveRoute::from_request(object, default_route)?;
    let image = requested_route == crate::engrave_route::EngraveRoute::Image && !is_cut;
    let route_note = if requested_route == crate::engrave_route::EngraveRoute::Image && is_cut {
        Some("外框切割只能走矢量，本次按矢量生成")
    } else if image && !text.is_empty() && text_fill != laser_core::engine::TextFill::Fill {
        Some("转图片时文字一律实心扫描（描边、排线只在矢量下可选）")
    } else {
        None
    };
    let solid_fill = image
        || (!text.is_empty() && text_fill == laser_core::engine::TextFill::Fill)
        || shapes.iter().any(|shape| shape.fill);
    let (rec_laser_mode, rec_engraving_mode) = if is_cut {
        ("cut", None)
    } else if solid_fill {
        ("engrave", Some("raster"))
    } else {
        ("engrave", Some("outline"))
    };
    let recommendation = laser_core::material::recommend_in_library(
        library,
        material,
        thickness,
        rec_laser_mode,
        rec_engraving_mode,
        1000.0,
    )
    .ok_or("读取材料推荐失败")?;

    let params = feedback::generation_params(
        object,
        recommendation.params.laser_max_power as f64,
        recommendation.params.feed_rate as f64,
        recommendation.params.passes.clamp(1, 5) as u32,
    )?;
    // 排版与起点（2026-11-06）：width_mm / height_mm 为木牌尺寸；没指定字号时自动取能放下的最大字号；
    // 默认激光头对准木牌（图案）中心（用户决定）
    let optional_number = |keys: &[&str]| -> Result<Option<f64>, String> {
        for key in keys {
            if let Some(value) = crate::http::parse_optional_float(object.get(*key), key)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    };
    let explicit_font_size = optional_number(&["font_size_mm"])?.is_some();
    let pixel_size = optional_number(&["pixel_size_mm"])?.unwrap_or(
        if recommendation.params.pixel_size_mm.is_finite()
            && recommendation.params.pixel_size_mm > 0.0
        {
            recommendation.params.pixel_size_mm
        } else {
            0.1
        },
    );
    // 填充扫描设置（2026-11-16 对齐 LightBurn）：过扫描默认按速度自动算；请求没写的键取材料库条目
    // 随材料保存的值（r40，对齐 LightBurn 库条目保存整套图层设置）
    let (fill_request, fill_from_material) = merge_layer_options(
        object.get("fill_options"),
        recommendation
            .params
            .layer_options(laser_contracts::MATERIAL_FILL_OPTIONS_KEY),
        laser_contracts::MATERIAL_FILL_OPTION_KEYS,
    );
    let mut fill = parse_fill_options(fill_request.as_ref(), params.feed, motion.overscan_accel())?;
    fill.from_material = fill_from_material;
    // 切割质量（2026-11-16 对齐 LightBurn）：只用于外框切割，默认全关；切割时同样回落到材料库条目
    let (cut_request, cut_from_material) = if is_cut {
        merge_layer_options(
            object.get("cut_options"),
            recommendation
                .params
                .layer_options(laser_contracts::MATERIAL_CUT_OPTIONS_KEY),
            laser_contracts::MATERIAL_CUT_OPTION_KEYS,
        )
    } else if let Some(ref cut_rec) = border_cut_rec {
        merge_layer_options(
            object.get("cut_options"),
            cut_rec
                .params
                .layer_options(laser_contracts::MATERIAL_CUT_OPTIONS_KEY),
            laser_contracts::MATERIAL_CUT_OPTION_KEYS,
        )
    } else {
        (object.get("cut_options").cloned(), Vec::new())
    };
    let cut = parse_cut_options(cut_request.as_ref())?;
    if !is_cut && !border_cut && cut.is_active() {
        return Err("cut_options（切缝补偿、桥位、引线、过切）只用于外框切割（laser_mode=cut），雕刻任务请去掉".into());
    }
    let spec = laser_core::TextTaskSpec {
        text,
        font_path,
        font_size_mm: font_size,
        letter_spacing_mm: spacing,
        power: params.power as i32,
        feed_rate: params.feed as i32,
        preview_width_px: 800,
        preview_height_px: 600,
        fill: text_fill,
        alignment: laser_core::engine::parse_alignment(
            object.get("text_align").and_then(|v| v.as_str()),
        )?,
        origin: match object
            .get("origin")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            Some(origin) => laser_core::engine::TextOrigin::parse(Some(origin))?,
            None => default_origin,
        },
        board_width_mm: optional_number(&["board_width_mm", "width_mm"])?,
        board_height_mm: optional_number(&["board_height_mm", "height_mm"])?,
        margin_mm: optional_number(&["margin_mm", "safe_margin_mm"])?
            .unwrap_or(TEXT_BOARD_MARGIN_MM),
        auto_size: !explicit_font_size,
        pixel_size_mm: pixel_size,
        scan: fill.options,
        // 切割时闭合轮廓先内后外
        plan: laser_core::vector_plan::PlanOptions {
            inside_first: is_cut,
            ..parse_path_options(object)?
        },
        cut: if is_cut { cut } else { Default::default() },
        shapes: &shapes,
        border,
        text_box,
        weld: parse_bool(object.get("text_weld"), true),
        image,
    };
    // 文字成品超出机器幅面时在预览阶段就拒绝，AI / 界面能立即提示改小字号（2026-11-06）；
    // 2026-11-26（r65）：work_area_* 校验 (0, 2000] 有限数字，非法值直接返回 400
    let area_w = parse_work_area_mm(object.get("work_area_width_mm"), "work_area_width_mm")?;
    let area_h = parse_work_area_mm(object.get("work_area_height_mm"), "work_area_height_mm")?;
    let artifacts = laser_core::TextTaskEngine::generate(&spec)?;
    if let Ok(Some(extents)) = laser_core::gcode::motion_extents(&artifacts.gcode)
        && (extents.width() > area_w + 0.01 || extents.height() > area_h + 0.01)
    {
        return Err(format!(
            "成品约 {:.1} × {:.1} mm，超出机器幅面 {area_w:.0} × {area_h:.0} mm，请减小字号（当前 {:.1} mm）、减少字数或缩小图形",
            extents.width(),
            extents.height(),
            artifacts.font_size_mm
        ));
    }
    let (gcode, border_cut_params, effective_cut_report) = if let Some(ref cut_rec) = border_cut_rec
    {
        let border_val = border.expect("border_cut requires border");
        let mut inner_shapes = shapes.clone();
        if border_val.double {
            let (rings, _) =
                border_val.build(spec.board_width_mm, spec.board_height_mm, spec.margin_mm)?;
            if rings.len() > 1 {
                inner_shapes.extend(rings.into_iter().skip(1));
            }
        }
        let outer_border = laser_core::shapes::Border {
            double: false,
            ..border_val
        };
        let cut_power = cut_rec.params.laser_max_power;
        let cut_feed = cut_rec.params.feed_rate;
        let cut_passes = cut_rec.params.passes.clamp(1, 5) as u32;
        let outer_spec = laser_core::TextTaskSpec {
            text: "",
            shapes: &[],
            border: Some(outer_border),
            power: cut_power,
            feed_rate: cut_feed,
            fill: laser_core::engine::TextFill::Outline,
            plan: laser_core::vector_plan::PlanOptions {
                inside_first: true,
                ..spec.plan
            },
            cut,
            image: false,
            ..spec
        };
        let outer_artifacts = laser_core::TextTaskEngine::generate(&outer_spec)?;
        let outer_origin = parse_g92_origin(&outer_artifacts.gcode);
        let stage2 = feedback::repeat_generated(&outer_artifacts.gcode, cut_passes);
        let combined = if !text.is_empty() || !inner_shapes.is_empty() {
            let inner_spec = laser_core::TextTaskSpec {
                font_size_mm: artifacts.font_size_mm,
                auto_size: false,
                shapes: &inner_shapes,
                border: None,
                cut: Default::default(),
                ..spec
            };
            let inner_artifacts = laser_core::TextTaskEngine::generate(&inner_spec)?;
            let shifted_inner = shift_program_origin(&inner_artifacts.gcode, outer_origin);
            let stage1 = feedback::repeat_generated(&shifted_inner, params.passes);
            append_stage_program(&stage1, &stage2)
        } else {
            stage2
        };
        (
            combined,
            Some(serde_json::json!({
                "laser_max_power": cut_power,
                "feed_rate": cut_feed,
                "passes": cut_passes,
                "power_scale": 1000.0
            })),
            outer_artifacts.cut_report,
        )
    } else {
        (
            feedback::repeat_generated(&artifacts.gcode, params.passes),
            None,
            artifacts.cut_report,
        )
    };
    let estimate = laser_core::gcode::estimate_time_with(&gcode, &motion.profile);

    let root = generated_root.ok_or("生成目录未配置")?;
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = format!("text-{}-{}-{}", unix_seconds(), nanos, std::process::id());
    let gcode_path = root.join(format!("{id}.gcode"));
    let preview_path = root.join(format!("{id}.png"));
    std::fs::write(&gcode_path, gcode.as_bytes()).map_err(|error| error.to_string())?;
    artifacts
        .preview
        .save(&preview_path)
        .map_err(|error| error.to_string())?;

    let gcode_sha = hex::encode(sha2::Sha256::digest(gcode.as_bytes()));
    let speed_warning = feed_exceeds_max_rate_warning(params.feed, &motion);

    let mut map = serde_json::json!({
        "gcode_file": gcode_path.file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "gcode_sha256": gcode_sha,
        "actual_params": params.json(),
        "image_file": preview_path.file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "font_path": std::path::Path::new(font_path).file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "material": recommendation.material,
        "thickness_mm": thickness,
        "width_mm": artifacts.width_mm,
        "height_mm": artifacts.height_mm,
        "font_size_mm": artifacts.font_size_mm,
        "text_fill": artifacts.fill.as_str(),
        "origin": artifacts.origin.as_str(),
        "origin_note": match (artifacts.origin, spec.board_width_mm.is_some() || spec.board_height_mm.is_some()) {
            (laser_core::engine::TextOrigin::Center, true) => "开刻前把激光头移到木牌中心",
            (laser_core::engine::TextOrigin::Center, false) => "开刻前把激光头移到要刻的位置中心",
            (laser_core::engine::TextOrigin::LowerLeft, true) => "开刻前把激光头移到木牌左下角",
            (laser_core::engine::TextOrigin::LowerLeft, false) => "开刻前把激光头移到文字左下角",
        },
        "board_width_mm": spec.board_width_mm,
        "board_height_mm": spec.board_height_mm,
        "frame_mm": {
            "min_x": artifacts.frame.min_x,
            "max_x": artifacts.frame.max_x,
            "min_y": artifacts.frame.min_y,
            "max_y": artifacts.frame.max_y
        },
        "frame_hull_mm": frame_hull_json(&gcode),
        "glyphs": artifacts.glyphs,
        "path_optimization": artifacts.path_optimization,
        "fill_options": artifacts.fill_options.map(|options| fill_options_json(&options, &fill)),
        "cut_options": (is_cut || border_cut).then(|| cut_options_json(&cut, effective_cut_report.as_ref(), &cut_from_material)),
        "removed_duplicates": artifacts.removed_duplicates,
        "engrave_route": if image { "image" } else { "vector" },
        "route_note": route_note,
        "raster": artifacts.raster.map(|r| serde_json::json!({"grid_width": r.grid_width, "grid_height": r.grid_height, "pixel_size_mm": r.pixel_size_mm})),
        "shapes": super::shapes::shapes_json(&shapes),
        "shape_count": artifacts.shapes,
        "border": border.as_ref().map(|b| {
            let mut val = super::shapes::border_json(b);
            if border_cut {
                val["cut"] = serde_json::json!(true);
            }
            val
        }),
        "border_cut": border_cut,
        "border_cut_params": border_cut_params,
        "text_box": text_box.map(|b| serde_json::json!({"x_mm": b.x_mm, "y_mm": b.y_mm, "width_mm": b.width_mm, "height_mm": b.height_mm})),
        "weld": artifacts.weld.map(|w| serde_json::json!({"contours_before": w.before, "contours_after": w.after})),
        "time_estimate": time_estimate_json(&estimate, &motion)
    }).as_object().cloned().unwrap_or_default();
    if let Some(warning) = speed_warning {
        map.insert("warnings".into(), serde_json::json!([warning]));
    }
    Ok(map)
}
