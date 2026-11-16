use super::*;

pub(crate) fn generate_text_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    generated_root: Option<&std::path::Path>,
    library: &laser_contracts::MaterialLibrary,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    // 2026-11-16：工时按机器加速度估算（请求 `machine_motion`，缺省按默认值）
    let motion = parse_machine_motion(object)?;
    let text = object
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
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
    let font_path_buf = object
        .get("font_path")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| laser_core::font::find_system_font_for_text(text))
        .ok_or("font_path 必须指向可读的 TTF/TTC 字体")?;
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
    // 文字刻法（2026-11-06）：默认只描轮廓（用户决定）；fill 实心填充用材料库 raster 参数
    // （2026-11-16 起改为矢量密排线，行距、速度与扫描填充同档），outline / hatch 用 outline 参数
    let text_fill =
        laser_core::engine::TextFill::parse(object.get("text_fill").and_then(|v| v.as_str()))?;
    let (rec_laser_mode, rec_engraving_mode) = if is_cut {
        ("cut", None)
    } else if text_fill == laser_core::engine::TextFill::Fill {
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
            match object.get(*key) {
                None | Some(serde_json::Value::Null) => continue,
                Some(serde_json::Value::String(text)) if text.trim().is_empty() => continue,
                Some(value) => {
                    return value
                        .as_f64()
                        .or_else(|| {
                            value
                                .as_str()
                                .and_then(|text| text.trim().parse::<f64>().ok())
                        })
                        .filter(|v| v.is_finite())
                        .map(Some)
                        .ok_or_else(|| format!("{key} 必须是数字"));
                }
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
    // 填充扫描设置（2026-11-16 对齐 LightBurn）：过扫描默认按速度自动算
    let fill = parse_fill_options(object, params.feed, motion.overscan_accel())?;
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
        origin: laser_core::engine::TextOrigin::parse(
            object.get("origin").and_then(|v| v.as_str()),
        )?,
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
    };
    let artifacts = laser_core::TextTaskEngine::generate(&spec)?;
    // 文字成品超出机器幅面时在预览阶段就拒绝，AI / 界面能立即提示改小字号（2026-11-06）
    let area = |key: &str| {
        parse_float(object.get(key), key, MACHINE_WORK_AREA_MM)
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(MACHINE_WORK_AREA_MM)
    };
    let (area_w, area_h) = (area("work_area_width_mm"), area("work_area_height_mm"));
    if let Ok(Some(extents)) = laser_core::gcode::motion_extents(&artifacts.gcode)
        && (extents.width() > area_w + 0.01 || extents.height() > area_h + 0.01)
    {
        return Err(format!(
            "文字成品约 {:.1} × {:.1} mm，超出机器幅面 {area_w:.0} × {area_h:.0} mm，请减小字号（当前 {:.1} mm）或减少字数",
            extents.width(),
            extents.height(),
            artifacts.font_size_mm
        ));
    }
    let gcode = feedback::repeat_generated(&artifacts.gcode, params.passes);
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

    Ok(serde_json::json!({
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
        "fill_options": artifacts.fill_options.map(|options| fill_options_json(&options, fill.auto)),
        "removed_duplicates": artifacts.removed_duplicates,
        "time_estimate": time_estimate_json(&estimate, &motion)
    }).as_object().cloned().unwrap_or_default())
}

pub(crate) fn generate_draw_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    generated_root: Option<&std::path::Path>,
    library: &laser_contracts::MaterialLibrary,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    // 2026-11-16：工时按机器加速度估算（请求 `machine_motion`，缺省按默认值）
    let motion = parse_machine_motion(object)?;
    let image_data_url = object
        .get("image_data_url")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if image_data_url.is_empty() {
        return Err("image_data_url 不能为空".into());
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

    let requested_mode = object
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("raster");
    // 2026-11-16：矢量模式直接用画板里的图形规划路线；场景里有插入的图片（矢量刻不了）或没带场景时改用光栅
    // 同日：`selected_element_ids` 只刻选中的图形（对齐 LightBurn“只加工选中图形”，不用选区原点）：
    // 排版仍按整幅画计算，选中的部分刻在它在整幅画里的位置，补刻 / 重刻能对上
    let selection: Option<std::collections::HashSet<String>> =
        match object.get("selected_element_ids") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Array(ids)) => {
                let ids: Option<std::collections::HashSet<String>> = ids
                    .iter()
                    .map(|id| {
                        id.as_str()
                            .filter(|s| !s.is_empty() && s.len() <= 128)
                            .map(str::to_owned)
                    })
                    .collect();
                match ids {
                    Some(ids) if !ids.is_empty() && ids.len() <= 10_000 => Some(ids),
                    _ => return Err("selected_element_ids 必须是 1 ~ 10000 个非空字符串".into()),
                }
            }
            Some(_) => return Err("selected_element_ids 必须是字符串数组".into()),
        };
    if selection.is_some() && requested_mode != "vector" {
        return Err("只刻选中的图形需要“矢量”生成策略".into());
    }
    let (sketch, layout_sketch) = if requested_mode == "vector" {
        match object.get("scene_json") {
            Some(scene) => {
                let font = &laser_core::font::find_system_font_for_text;
                let full = crate::sketch::sketch_from_scene(scene, font)?;
                match &selection {
                    Some(ids) => {
                        let picked = crate::sketch::sketch_from_scene(
                            &crate::sketch::select_elements(scene, ids),
                            font,
                        )?;
                        if picked.has_image {
                            return Err("选中的图形里有插入的图片，矢量刻不了；请取消选中图片或改用光栅刻整幅".into());
                        }
                        if picked.is_empty() {
                            return Err("选中的图形里没有可刻的线条或填充".into());
                        }
                        (Some(picked), Some(full))
                    }
                    None => (Some(full), None),
                }
            }
            None if selection.is_some() => {
                return Err("只刻选中的图形需要画板场景数据".into());
            }
            None => (None, None),
        }
    } else {
        (None, None)
    };
    let vector_fallback = match (&sketch, requested_mode) {
        (_, mode) if mode != "vector" => None,
        (None, _) => Some("没有收到画板的矢量数据，已按光栅生成"),
        (Some(sketch), _) if sketch.has_image => {
            Some("画板里有插入的图片，矢量模式刻不了图片，已按光栅生成")
        }
        (Some(sketch), _) if sketch.is_empty() => {
            return Err("画板里没有可刻的图形（线条颜色是透明的？）".into());
        }
        _ => None,
    };
    let mode = if requested_mode == "vector" && vector_fallback.is_some() {
        "raster"
    } else {
        requested_mode
    };
    // 矢量走线与轮廓描边用同一档材料参数
    let recommend_mode = if mode == "vector" { "outline" } else { mode };

    let data_part = if let Some((_, b64)) = image_data_url.split_once(',') {
        b64
    } else {
        image_data_url
    };
    let image_bytes = base64::engine::general_purpose::STANDARD
        .decode(data_part.trim())
        .map_err(|e| format!("Base64 解码失败: {e}"))?;

    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(&image_bytes);
    let fingerprint = hex::encode(hasher.finalize());

    let root = generated_root.ok_or("生成目录未配置")?;
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = format!("draw-{}-{}-{}", unix_seconds(), nanos, std::process::id());

    let image_path = root.join(format!("{id}.png"));
    std::fs::write(&image_path, &image_bytes).map_err(|error| error.to_string())?;

    let scene_name = if let Some(scene) = object.get("scene_json") {
        let scene_path = root.join(format!("{id}.scene.json"));
        let scene_bytes = serde_json::to_vec_pretty(scene).map_err(|e| e.to_string())?;
        std::fs::write(&scene_path, scene_bytes).map_err(|error| error.to_string())?;
        format!("{id}.scene.json")
    } else {
        String::new()
    };

    let luma_img = image::load_from_memory(&image_bytes)
        .map_err(|e| format!("图像加载失败: {e}"))?
        .to_luma8();
    let img_w = luma_img.width();
    let img_h = luma_img.height();
    if img_w == 0 || img_h == 0 {
        return Err("图像尺寸为 0".into());
    }

    let recommendation = laser_core::material::recommend_in_library(
        library,
        material,
        thickness,
        if mode == "cut" { "cut" } else { "engrave" },
        if mode == "cut" {
            None
        } else {
            Some(recommend_mode)
        },
        1000.0,
    );
    let power = recommendation
        .as_ref()
        .map(|r| r.params.laser_max_power)
        .unwrap_or(400);
    let feed_rate = recommendation
        .as_ref()
        .map(|r| r.params.feed_rate)
        .unwrap_or(1500);
    let rec_passes = recommendation
        .as_ref()
        .map(|r| r.params.passes.clamp(1, 5) as u32)
        .unwrap_or(1);
    let params = feedback::generation_params(object, power as f64, feed_rate as f64, rec_passes)?;
    let power = params.power as i32;
    let feed_rate = params.feed as i32;

    // 成品尺寸：填了宽 / 高按填的尺寸（锁定比例，取能放进去的最大尺寸）；都没填按导出图 0.1 mm/像素。
    // 2026-11-06 起按机器幅面（默认 100 mm，请求可传 work_area_*）而不是 400 mm 画布限制；光栅模式宽度另留两侧过扫描
    let area = |key: &str| {
        parse_float(object.get(key), key, MACHINE_WORK_AREA_MM)
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(MACHINE_WORK_AREA_MM)
    };
    let (area_w, area_h) = (area("work_area_width_mm"), area("work_area_height_mm"));
    let usable_w = if mode == "outline" || mode == "vector" {
        area_w
    } else {
        area_w - 2.0 * IMAGE_OVERSCAN_MM
    };
    let (width_mm, height_mm, draw_auto_fitted) = draw_target_size_mm(
        img_w,
        img_h,
        optional_positive_mm(object.get("width_mm"), "width_mm")?,
        optional_positive_mm(object.get("height_mm"), "height_mm")?,
        (usable_w, area_h),
        (area_w, area_h),
    )?;
    let path_preview_name = format!("{id}.path.png");
    let fill_spacing = recommendation
        .as_ref()
        .map(|r| r.params.pixel_size_mm)
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(DRAW_PIXEL_MM);
    let mut path_optimization = None;
    let mut vector_details = None;
    let gcode_str = if let (Some(sketch), "vector") = (&sketch, mode) {
        let fill = parse_fill_options(object, feed_rate as f64, motion.overscan_accel())?;
        let planned = plan_sketch(
            sketch,
            layout_sketch.as_ref().unwrap_or(sketch),
            width_mm,
            height_mm,
            laser_core::vector_plan::FillOptions {
                spacing: fill_spacing,
                ..fill.options
            },
            &parse_path_options(object)?,
        )?;
        path_optimization = Some(planned.stats.clone());
        vector_details = Some((
            sketch.has_fills().then(|| {
                fill_options_json(
                    &laser_core::vector_plan::FillOptions {
                        spacing: fill_spacing,
                        ..fill.options
                    },
                    fill.auto,
                )
            }),
            planned.removed_duplicates,
        ));
        let preview_scale = (1024.0 / width_mm.max(height_mm)).max(1.0);
        laser_core::gcode::preview_outline_rgba_with_options(
            &planned.paths,
            (width_mm * preview_scale).ceil() as u32,
            (height_mm * preview_scale).ceil() as u32,
            width_mm,
            height_mm,
            false,
        )?
        .save(root.join(&path_preview_name))
        .map_err(|e| e.to_string())?;
        laser_core::gcode::write_steps(
            &planned.steps,
            &laser_core::gcode::WriterOptions {
                width_mm,
                height_mm,
                power,
                feed_rate,
                auto_close: false,
                laser_mode: "M4",
                power_scale: None,
            },
        )?
    } else if mode == "outline" {
        // 轮廓雕刻：深色区域描边（与 Python 版相同的灰度描摹、简化与点数预算），按成品尺寸缩放
        let gray: Vec<Vec<u8>> = (0..img_h)
            .map(|y| (0..img_w).map(|x| luma_img.get_pixel(x, y).0[0]).collect())
            .collect();
        let simplify_factor = parse_float(
            object.get("vector_simplify_factor"),
            "vector_simplify_factor",
            3.0,
        )?;
        if !(0.25..=8.0).contains(&simplify_factor) {
            return Err("vector_simplify_factor 必须在 0.25 ~ 8 之间".into());
        }
        let traced = laser_core::trace::trace_grayscale(
            &gray,
            &laser_core::trace::TraceOptions {
                size_mm: width_mm.max(height_mm),
                width_mm: Some(width_mm),
                height_mm: Some(height_mm),
                lock_aspect_ratio: true,
                work_area_width_mm: area_w,
                work_area_height_mm: area_h,
                simplify_factor,
            },
        )
        .map_err(|e| e.to_string())?;
        if traced.paths.is_empty() {
            return Err("画板里没有可描边的深色图形，请改用光栅雕刻或加深线条".into());
        }
        let optimized =
            laser_core::path_opt::optimize_with_threshold(&traced.paths, width_mm.max(height_mm))
                .map_err(|e| e.to_string())?;
        let preview_scale = (1024.0 / width_mm.max(height_mm)).max(1.0);
        laser_core::gcode::preview_outline_rgba(
            &optimized.paths,
            (width_mm * preview_scale).ceil() as u32,
            (height_mm * preview_scale).ceil() as u32,
            width_mm,
            height_mm,
        )?
        .save(root.join(&path_preview_name))
        .map_err(|e| e.to_string())?;
        laser_core::gcode::write_outline(
            &optimized.paths,
            &laser_core::gcode::WriterOptions {
                width_mm,
                height_mm,
                power,
                feed_rate,
                auto_close: true,
                laser_mode: "M4",
                power_scale: None,
            },
        )?
    } else {
        // 光栅：先把导出图缩放到成品尺寸（每像素 0.1 mm），再二值化逐行扫描
        let raster_w = ((width_mm / DRAW_PIXEL_MM).round() as u32).max(1);
        let raster_h = ((height_mm / DRAW_PIXEL_MM).round() as u32).max(1);
        let scaled = if (raster_w, raster_h) == (img_w, img_h) {
            luma_img.clone()
        } else {
            image::imageops::resize(
                &luma_img,
                raster_w,
                raster_h,
                image::imageops::FilterType::Triangle,
            )
        };
        let mut bitmap = Vec::with_capacity(raster_h as usize);
        for y in 0..raster_h {
            let mut row = Vec::with_capacity(raster_w as usize);
            for x in 0..raster_w {
                row.push(if scaled.get_pixel(x, y).0[0] < 128 {
                    0
                } else {
                    255
                });
            }
            bitmap.push(row);
        }

        // 路径预览：二值化后的实际烧蚀像素（黑=出光），与素材预览区分
        let mut bin = image::GrayImage::from_pixel(raster_w, raster_h, image::Luma([255]));
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &v) in row.iter().enumerate() {
                if v == 0 {
                    bin.put_pixel(x as u32, y as u32, image::Luma([0]));
                }
            }
        }
        bin.save(root.join(&path_preview_name))
            .map_err(|e| e.to_string())?;

        let plan =
            laser_core::raster::raster_plan_with_direction(&bitmap, "horizontal", "scanline")
                .map_err(|e| e.to_string())?;
        laser_core::gcode::write_raster(
            &plan,
            raster_w as usize,
            raster_h as usize,
            &laser_core::gcode::RasterWriterOptions {
                pixel_size_mm: DRAW_PIXEL_MM,
                overscan_mm: IMAGE_OVERSCAN_MM,
                work_area_width_mm: area_w,
                work_area_height_mm: area_h,
                power,
                feed_rate,
                laser_mode: "M4",
                snake_scan: true,
                material: Some(material.to_string()),
                safety_note: None,
                power_scale: None,
                dot_width_mm: 0.0,
            },
        )
        .map_err(|e| e.to_string())?
    };

    let gcode_str = feedback::repeat_generated(&gcode_str, params.passes);
    let gcode_path = root.join(format!("{id}.gcode"));
    std::fs::write(&gcode_path, gcode_str.as_bytes()).map_err(|error| error.to_string())?;

    let estimate = laser_core::gcode::estimate_time_with(&gcode_str, &motion.profile);

    let img_name = format!("{id}.png");
    let gcode_name = format!("{id}.gcode");

    let mut res_map = serde_json::Map::new();
    res_map.insert("actual_params".into(), params.json());
    res_map.insert("image_file".into(), serde_json::Value::String(img_name));
    res_map.insert("gcode_file".into(), serde_json::Value::String(gcode_name));
    res_map.insert(
        "gcode_sha256".into(),
        serde_json::Value::String(hex::encode(sha2::Sha256::digest(gcode_str.as_bytes()))),
    );
    res_map.insert("scene_file".into(), serde_json::Value::String(scene_name));
    res_map.insert(
        "path_preview_file".into(),
        serde_json::Value::String(path_preview_name),
    );
    res_map.insert(
        "material".into(),
        serde_json::Value::String(material.to_string()),
    );
    res_map.insert("thickness_mm".into(), serde_json::json!(thickness));
    res_map.insert("width_mm".into(), serde_json::json!(width_mm));
    res_map.insert("height_mm".into(), serde_json::json!(height_mm));
    res_map.insert("auto_fitted".into(), serde_json::json!(draw_auto_fitted));
    if draw_auto_fitted {
        res_map.insert(
            "fit_note".into(),
            serde_json::json!(format!(
                "画板按 0.1 mm/像素会超出机器幅面，已等比缩小到 {width_mm:.1} × {height_mm:.1} mm"
            )),
        );
    }
    res_map.insert("mode".into(), serde_json::Value::String(mode.to_string()));
    if let Some(note) = vector_fallback {
        res_map.insert("vector_note".into(), serde_json::json!(note));
    }
    if let (Some(ids), "vector") = (&selection, mode) {
        res_map.insert("selected_only".into(), serde_json::json!(true));
        res_map.insert(
            "selection_note".into(),
            serde_json::json!(format!("只刻选中的 {} 个图形，位置与整幅画一致", ids.len())),
        );
    }
    if let Some(stats) = path_optimization {
        res_map.insert("path_optimization".into(), serde_json::json!(stats));
    }
    if let Some((fill, removed)) = vector_details {
        res_map.insert("fill_options".into(), serde_json::json!(fill));
        res_map.insert("removed_duplicates".into(), serde_json::json!(removed));
    }
    res_map.insert("origin".into(), serde_json::json!("lower_left"));
    res_map.insert(
        "origin_note".into(),
        serde_json::json!("开刻前把激光头移到图案左下角"),
    );
    let frame_max_x = if mode == "outline" || mode == "vector" {
        width_mm
    } else {
        width_mm + 2.0 * IMAGE_OVERSCAN_MM
    };
    res_map.insert(
        "frame_mm".into(),
        serde_json::json!({
            "min_x": 0.0,
            "max_x": frame_max_x,
            "min_y": 0.0,
            "max_y": height_mm,
        }),
    );
    res_map.insert("frame_hull_mm".into(), frame_hull_json(&gcode_str));
    res_map.insert(
        "time_estimate".into(),
        time_estimate_json(&estimate, &motion),
    );
    res_map.insert("fingerprint".into(), serde_json::Value::String(fingerprint));

    Ok(res_map)
}

/// 画板矢量几何换算到成品尺寸（毫米，y 向下、左下角为激光头起点）并规划路线：
/// 先把有填充色的图形与文字扫描填满，再从填充结束处描所有轮廓与笔画（同 LightBurn 先 Fill 层后 Line 层）。
/// 留边与光栅导出一致（四周 16 个画板单位），等比放进成品尺寸并居中；有填充时四周再让出过扫描的距离，
/// 关光滑行也不越出成品范围（激光头起点在左下角，不往负方向走）。
fn plan_sketch(
    sketch: &crate::sketch::SketchGeometry,
    // 排版（缩放、居中、过扫描留边）按它算：只刻选中图形时是整幅画，否则就是 `sketch`
    layout: &crate::sketch::SketchGeometry,
    width_mm: f64,
    height_mm: f64,
    fill: laser_core::vector_plan::FillOptions,
    path_options: &laser_core::vector_plan::PlanOptions,
) -> Result<laser_core::vector_plan::Planned, String> {
    use laser_core::vector_plan::{FillOptions, FillRule, Unit, plan_layers, scan_fill};
    const PADDING: f64 = 16.0;
    let (x0, y0, x1, y1) = layout.bounds().ok_or("画板里没有可刻的图形")?;
    let (reach_x, reach_y) = if layout.has_fills() {
        fill.extension_mm()
    } else {
        (0.0, 0.0)
    };
    let (inner_w, inner_h) = (width_mm - 2.0 * reach_x, height_mm - 2.0 * reach_y);
    if inner_w <= 0.0 || inner_h <= 0.0 {
        return Err("成品尺寸太小，放不下过扫描，请加大尺寸或调小过扫描".into());
    }
    let (frame_w, frame_h) = ((x1 - x0) + 2.0 * PADDING, (y1 - y0) + 2.0 * PADDING);
    let scale = (inner_w / frame_w).min(inner_h / frame_h);
    if !scale.is_finite() || scale <= 0.0 {
        return Err("画板图形尺寸无效".into());
    }
    let (ox, oy) = (
        (width_mm - frame_w * scale) / 2.0,
        (height_mm - frame_h * scale) / 2.0,
    );
    let mm = sketch.map(|(x, y)| {
        (
            (x - x0 + PADDING) * scale + ox,
            (y - y0 + PADDING) * scale + oy,
        )
    });
    // 扫描角度按机器视角逆时针，画板坐标 y 向下时取反
    let options = |rule| FillOptions {
        rule,
        angle_deg: -fill.angle_deg,
        ..fill
    };
    let mut fills = Vec::new();
    for shape in &mm.filled_shapes {
        fills.extend(
            scan_fill(std::slice::from_ref(shape), &options(FillRule::EvenOdd))
                .map_err(|e| e.to_string())?,
        );
    }
    for glyphs in &mm.filled_text {
        fills.extend(scan_fill(glyphs, &options(FillRule::NonZero)).map_err(|e| e.to_string())?);
    }
    let strokes: Vec<Unit> = mm
        .closed
        .iter()
        .cloned()
        .map(Unit::Closed)
        .chain(mm.open.iter().cloned().map(Unit::Open))
        .collect();
    // 激光头起点在左下角：y 向下坐标里是 (0, height)；规划内部自检出光总长度不变
    plan_layers(vec![fills, strokes], (0.0, height_mm), path_options).map_err(|e| e.to_string())
}

/// 请求里的机器运动参数（`machine_motion`，键名与 `/api/connection/check` 的 `machine_limits` 相同，
/// 前端可原样转发握手读到的值，或用用户填的加速度覆盖）：`x_accel_mm_s2`、`y_accel_mm_s2`、
/// `x_max_rate_mm_per_min`、`y_max_rate_mm_per_min`、`junction_deviation_mm`，全部可省略。
pub(crate) struct MachineMotion {
    pub(crate) profile: laser_core::gcode::MotionProfile,
    /// `machine`：请求带了加速度；`default`：按默认 1000 mm/s² 估算
    pub(crate) source: &'static str,
}

impl MachineMotion {
    /// 自动过扫描用的加速度：两轴取小（扫描方向不定时保守）。
    pub(crate) fn overscan_accel(&self) -> f64 {
        self.profile.accel_x_mm_s2.min(self.profile.accel_y_mm_s2)
    }
}

pub(crate) fn parse_machine_motion(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<MachineMotion, String> {
    use laser_core::gcode::MotionProfile;
    let empty = serde_json::Map::new();
    let motion = match object.get("machine_motion") {
        None | Some(serde_json::Value::Null) => &empty,
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("machine_motion 必须是对象".into()),
    };
    let number = |key: &str, max: f64| -> Result<Option<f64>, String> {
        match motion.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(value) => value
                .as_f64()
                .filter(|v| v.is_finite() && *v > 0.0 && *v <= max)
                .map(Some)
                .ok_or_else(|| format!("machine_motion.{key} 必须是 0 ~ {max} 之间的数字")),
        }
    };
    let ax = number("x_accel_mm_s2", 100_000.0)?;
    let ay = number("y_accel_mm_s2", 100_000.0)?;
    let rx = number("x_max_rate_mm_per_min", 1_000_000.0)?;
    let ry = number("y_max_rate_mm_per_min", 1_000_000.0)?;
    let jd = number("junction_deviation_mm", 10.0)?;
    let defaults = MotionProfile::default();
    // GRBL 的 G0 按各轴最大速度走；知道最大速度时快移取两轴中较大的，再按方向逐轴封顶
    let rapid = match (rx, ry) {
        (Some(x), Some(y)) => x.max(y),
        (Some(v), None) | (None, Some(v)) => v,
        (None, None) => defaults.rapid_mm_per_min,
    };
    // 只给了一个轴的加速度时另一个轴用同一个值
    let (accel_x, accel_y) = match (ax, ay) {
        (Some(x), Some(y)) => (x, y),
        (Some(v), None) | (None, Some(v)) => (v, v),
        (None, None) => (defaults.accel_x_mm_s2, defaults.accel_y_mm_s2),
    };
    Ok(MachineMotion {
        profile: MotionProfile {
            rapid_mm_per_min: rapid,
            accel_x_mm_s2: accel_x,
            accel_y_mm_s2: accel_y,
            max_rate_x_mm_per_min: rx,
            max_rate_y_mm_per_min: ry,
            junction_deviation_mm: jd.unwrap_or(defaults.junction_deviation_mm),
        },
        source: if ax.is_some() || ay.is_some() {
            "machine"
        } else {
            "default"
        },
    })
}

/// 结果里的工时估算：按加速度的规划器模型，并写明用了多大的加速度、来自哪里。
pub(crate) fn time_estimate_json(
    estimate: &laser_core::gcode::TimeEstimate,
    motion: &MachineMotion,
) -> serde_json::Value {
    serde_json::json!({
        "estimated_seconds": estimate.estimated_seconds,
        "estimated_minutes": estimate.estimated_minutes,
        "laser_seconds": estimate.laser_seconds,
        "non_laser_seconds": estimate.non_laser_seconds,
        "model": "acceleration",
        "acceleration_mm_s2": {"x": motion.profile.accel_x_mm_s2, "y": motion.profile.accel_y_mm_s2},
        "acceleration_source": motion.source,
    })
}

/// 走边框“橡皮筋”轮廓（2026-11-16）：出光点的凸包，以激光头起点为原点，`[[x, y], …]`；
/// 含圆弧或出光点不足时为 null，前端退回外接矩形。
pub(crate) fn frame_hull_json(gcode: &str) -> serde_json::Value {
    laser_core::gcode::burn_hull(gcode).map_or(serde_json::Value::Null, |hull| {
        let round = |v: f64| (v * 1000.0).round() / 1000.0;
        serde_json::json!(
            hull.iter()
                .map(|p| [round(p.0), round(p.1)])
                .collect::<Vec<_>>()
        )
    })
}

/// 请求里的 `fill_options` 解析结果；`auto` 表示过扫描按速度与加速度自动算。
struct ParsedFill {
    options: laser_core::vector_plan::FillOptions,
    auto: bool,
}

/// 解析 `fill_options`（2026-11-16，对齐 LightBurn Fill 模式；全部可省略）：
/// `grouping`（all 默认 / individual）、`angle_deg`（0 横扫）、`cross_hatch`、`bidirectional`（默认 true）、
/// `overscan_mm`（省略或 "auto" 按 v²/2a 自动；0 关闭；上限 20）、`acceleration_mm_s2`（自动过扫描用，默认 1000）、
/// `scan_offset_mm`（双向扫描错位补偿，±2）。线距与填充规则由调用方按刻法填。
fn parse_fill_options(
    object: &serde_json::Map<String, serde_json::Value>,
    feed_mm_per_min: f64,
    fallback_accel_mm_s2: f64,
) -> Result<ParsedFill, String> {
    use laser_core::vector_plan::{
        FillGrouping, FillOptions, FillRule, MAX_OVERSCAN_MM, auto_overscan_mm,
    };
    let empty = serde_json::Map::new();
    let fill = match object.get("fill_options") {
        None | Some(serde_json::Value::Null) => &empty,
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("fill_options 必须是对象".into()),
    };
    let number = |key: &str| -> Result<Option<f64>, String> {
        match fill.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(text)) if text.trim().is_empty() => Ok(None),
            Some(value) => value
                .as_f64()
                .or_else(|| value.as_str().and_then(|t| t.trim().parse().ok()))
                .filter(|v: &f64| v.is_finite())
                .map(Some)
                .ok_or_else(|| format!("fill_options.{key} 必须是数字")),
        }
    };
    let flag = |key: &str, default: bool| -> Result<bool, String> {
        match fill.get(key) {
            None | Some(serde_json::Value::Null) => Ok(default),
            Some(serde_json::Value::Bool(value)) => Ok(*value),
            Some(_) => Err(format!("fill_options.{key} 必须是 true / false")),
        }
    };
    let auto = matches!(
        fill.get("overscan_mm"),
        None | Some(serde_json::Value::Null)
    ) || fill
        .get("overscan_mm")
        .and_then(|v| v.as_str())
        .is_some_and(|t| t.trim().is_empty() || t.trim().eq_ignore_ascii_case("auto"));
    let overscan = if auto {
        // 加速度：fill_options 里填的 > 机器读到的（两轴取小）> 默认 1000
        let accel = number("acceleration_mm_s2")?.unwrap_or(fallback_accel_mm_s2);
        auto_overscan_mm(feed_mm_per_min, accel)
    } else {
        let value = number("overscan_mm")?.unwrap_or(0.0);
        if !(0.0..=MAX_OVERSCAN_MM).contains(&value) {
            return Err(format!(
                "fill_options.overscan_mm 须在 0 ~ {MAX_OVERSCAN_MM} mm"
            ));
        }
        value
    };
    let angle = number("angle_deg")?.unwrap_or(0.0);
    if !(-360.0..=360.0).contains(&angle) {
        return Err("fill_options.angle_deg 须在 -360 ~ 360 度".into());
    }
    let offset = number("scan_offset_mm")?.unwrap_or(0.0);
    if offset.abs() > 2.0 {
        return Err("fill_options.scan_offset_mm 须在 -2 ~ 2 mm".into());
    }
    Ok(ParsedFill {
        options: FillOptions {
            grouping: FillGrouping::parse(fill.get("grouping").and_then(|v| v.as_str()))?,
            angle_deg: angle,
            cross_hatch: flag("cross_hatch", false)?,
            bidirectional: flag("bidirectional", true)?,
            overscan_mm: overscan,
            scan_offset_mm: offset,
            ..FillOptions::new(0.1, FillRule::NonZero)
        },
        auto,
    })
}

/// 解析 `path_options`（对齐 LightBurn 优化设置）：`prefer_corners`（起刀点放尖角，默认 true）、
/// `remove_overlaps`（去重复线，默认 true）。切割先内后外由刻法决定，不在这里开关。
fn parse_path_options(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<laser_core::vector_plan::PlanOptions, String> {
    let empty = serde_json::Map::new();
    let path = match object.get("path_options") {
        None | Some(serde_json::Value::Null) => &empty,
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("path_options 必须是对象".into()),
    };
    let flag = |key: &str| -> Result<bool, String> {
        match path.get(key) {
            None | Some(serde_json::Value::Null) => Ok(true),
            Some(serde_json::Value::Bool(value)) => Ok(*value),
            Some(_) => Err(format!("path_options.{key} 必须是 true / false")),
        }
    };
    Ok(laser_core::vector_plan::PlanOptions {
        inside_first: false,
        prefer_corners: flag("prefer_corners")?,
        remove_overlaps: flag("remove_overlaps")?,
    })
}

/// 结果里回显实际使用的填充设置。
fn fill_options_json(
    options: &laser_core::vector_plan::FillOptions,
    overscan_auto: bool,
) -> serde_json::Value {
    serde_json::json!({
        "spacing_mm": options.spacing,
        "grouping": options.grouping.as_str(),
        "angle_deg": options.angle_deg,
        "cross_hatch": options.cross_hatch,
        "bidirectional": options.bidirectional,
        "overscan_mm": options.overscan_mm,
        "overscan_auto": overscan_auto,
        "scan_offset_mm": options.scan_offset_mm,
    })
}

/// 画板光栅每像素对应的物理尺寸与工作区上限（见 safety.md §4）。
const DRAW_PIXEL_MM: f64 = 0.1;
/// 图片任务默认机器幅面（翼宿 V1.0，`docs/laser/hardware-bom.md`）；请求可用 `work_area_width_mm` /
/// `work_area_height_mm` 传入连接检查读到的实际行程。发送前另按固件行程复核。
const MACHINE_WORK_AREA_MM: f64 = 100.0;
/// 图片横向扫描每侧过扫描（mm），尺寸换算会为它留出余量
const IMAGE_OVERSCAN_MM: f64 = 1.0;
/// 文字放进木牌时四周默认留边（mm）
const TEXT_BOARD_MARGIN_MM: f64 = 2.0;

/// 发送前按固件报告的机器行程复核任务范围（2026-11-06）。
/// 任务以开始时激光头所在位置为原点（生成的 G-code 以 `G92 X0 Y0` 开头），因此只比较刀路走过的
/// 宽、高是否超过 X/Y 行程——超过时无论从哪里开始都会撞到行程尽头，直接拒绝、不连接后续发送。
/// 固件没报告行程（`$130/$131` 与 `$CD` 都没有）或文件含机床坐标 / 回参考点移动、无法从文件得知
/// 范围时跳过本项，交给固件软限位。
pub(crate) fn check_job_fits_machine(
    gcode: &str,
    limits: &laser_device::MachineLimits,
) -> Result<(), String> {
    if limits.x_travel_mm.is_none() && limits.y_travel_mm.is_none() {
        return Ok(());
    }
    let Ok(Some(extents)) = laser_core::gcode::motion_extents(gcode) else {
        return Ok(());
    };
    const TOLERANCE_MM: f64 = 0.01;
    let over = |size: f64, travel: Option<f64>| travel.is_some_and(|t| size > t + TOLERANCE_MM);
    if over(extents.width(), limits.x_travel_mm) || over(extents.height(), limits.y_travel_mm) {
        let fmt = |travel: Option<f64>| travel.map_or("未知".to_owned(), |t| format!("{t:.0}"));
        return Err(format!(
            "任务范围 {:.1} × {:.1} mm 超出机器行程 {} × {} mm，已拒绝发送；请缩小尺寸后重新生成预览",
            extents.width(),
            extents.height(),
            fmt(limits.x_travel_mm),
            fmt(limits.y_travel_mm),
        ));
    }
    Ok(())
}

/// 可选的正数尺寸：缺省、null、空串或 0 表示“自动”。
fn optional_positive_mm(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<Option<f64>, String> {
    let number = match value {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::String(text)) if text.trim().is_empty() => return Ok(None),
        Some(serde_json::Value::String(text)) => text
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("{field} 必须是数字"))?,
        Some(other) => other
            .as_f64()
            .ok_or_else(|| format!("{field} 必须是数字"))?,
    };
    if !number.is_finite() || number < 0.0 {
        return Err(format!("{field} 必须是不小于 0 的数字"));
    }
    Ok((number > 0.0).then_some(number))
}

/// 画板成品尺寸（毫米）：宽高都填时锁定比例放进该框内；只填一边按比例算另一边；
/// 都没填按导出图每像素 0.1 mm，超出可用范围时等比缩小并返回 `true`；填写的尺寸超出直接拒绝，不悄悄缩小。
/// `usable` 为实际可放图案的范围（光栅模式宽度已扣除两侧过扫描），`machine` 为机器幅面（只用于提示）。
pub(crate) fn draw_target_size_mm(
    img_w: u32,
    img_h: u32,
    width: Option<f64>,
    height: Option<f64>,
    usable: (f64, f64),
    machine: (f64, f64),
) -> Result<(f64, f64, bool), String> {
    let (w, h) = (f64::from(img_w), f64::from(img_h));
    let (usable_w, usable_h) = usable;
    let mut auto_fitted = false;
    let (target_w, target_h) = match (width, height) {
        (Some(tw), Some(th)) => {
            let scale = (tw / w).min(th / h);
            (w * scale, h * scale)
        }
        (Some(tw), None) => (tw, tw * h / w),
        (None, Some(th)) => (th * w / h, th),
        (None, None) => {
            let (nw, nh) = (w * DRAW_PIXEL_MM, h * DRAW_PIXEL_MM);
            if nw > usable_w + 1e-9 || nh > usable_h + 1e-9 {
                auto_fitted = true;
                let scale = (usable_w / nw).min(usable_h / nh);
                (nw * scale, nh * scale)
            } else {
                (nw, nh)
            }
        }
    };
    if target_w > usable_w + 1e-9 || target_h > usable_h + 1e-9 {
        return Err(format!(
            "成品尺寸 {target_w:.1} × {target_h:.1} mm 超出工作区（机器幅面 {:.0} × {:.0} mm，可放图案最大 {usable_w:.1} × {usable_h:.1} mm），请减小宽或高",
            machine.0, machine.1
        ));
    }
    if target_w < DRAW_PIXEL_MM || target_h < DRAW_PIXEL_MM {
        return Err("成品尺寸太小，宽和高都至少 0.1 mm".into());
    }
    Ok((target_w, target_h, auto_fitted))
}

/// 图片预览里对齐 LightBurn Image Mode 的可选字段（2026-11-16，技术书 r38）。
/// 值域在这里先校验；与像素间距相关的（网点网格、点宽补偿）在得到实际行间距后再查。
struct ImageModeExtras {
    negative: bool,
    pass_through: bool,
    halftone_cell_mm: Option<f64>,
    halftone_angle_deg: f64,
    gray_levels: u8,
    gray_min_power_percent: f64,
    dot_width_mm: f64,
}

impl ImageModeExtras {
    fn parse(
        object: &serde_json::Map<String, serde_json::Value>,
        number: &dyn Fn(&str) -> Result<Option<f64>, String>,
    ) -> Result<Self, String> {
        let flag = |key: &str| match object.get(key) {
            None | Some(serde_json::Value::Null) => Ok(false),
            Some(serde_json::Value::Bool(value)) => Ok(*value),
            Some(_) => Err(format!("{key} 必须是 true 或 false")),
        };
        let halftone_cell_mm = match (number("halftone_cell_mm")?, number("halftone_lpi")?) {
            (Some(_), Some(_)) => {
                return Err("halftone_cell_mm 与 halftone_lpi 只能给一个".into());
            }
            (Some(cell), None) if cell > 0.0 => Some(cell),
            (Some(_), None) => return Err("halftone_cell_mm 必须大于 0".into()),
            (None, Some(lpi)) if (2.54..=508.0).contains(&lpi) => Some(25.4 / lpi),
            (None, Some(_)) => return Err("halftone_lpi 必须在 2.54 ~ 508 之间".into()),
            (None, None) => None,
        };
        let gray_levels = match number("gray_levels")? {
            None => laser_core::image_job::DEFAULT_GRAY_LEVELS,
            Some(value)
                if value.fract() == 0.0
                    && (f64::from(laser_core::image_job::MIN_GRAY_LEVELS)
                        ..=f64::from(laser_core::image_job::MAX_GRAY_LEVELS))
                        .contains(&value) =>
            {
                value as u8
            }
            Some(_) => {
                return Err(format!(
                    "gray_levels 必须是 {} ~ {} 的整数",
                    laser_core::image_job::MIN_GRAY_LEVELS,
                    laser_core::image_job::MAX_GRAY_LEVELS
                ));
            }
        };
        let gray_min_power_percent = match number("gray_min_power_percent")? {
            None => 0.0,
            Some(value) if (0.0..100.0).contains(&value) => value,
            Some(_) => return Err("gray_min_power_percent 必须在 0 ~ 100 之间（不含 100）".into()),
        };
        let dot_width_mm = match number("dot_width_mm")? {
            None => 0.0,
            Some(value) if value >= 0.0 => value,
            Some(_) => return Err("dot_width_mm 不能为负数".into()),
        };
        Ok(Self {
            negative: flag("negative")?,
            pass_through: flag("pass_through")?,
            halftone_cell_mm,
            halftone_angle_deg: number("halftone_angle_deg")?
                .unwrap_or(laser_core::image_job::DEFAULT_HALFTONE_ANGLE_DEG),
            gray_levels,
            gray_min_power_percent,
            dot_width_mm,
        })
    }
}

pub(crate) fn generate_image_or_gcode_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    upload_root: Option<&std::path::Path>,
    generated_root: Option<&std::path::Path>,
    preview_roots: &[std::path::PathBuf],
    library: &laser_contracts::MaterialLibrary,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    // 2026-11-16：工时按机器加速度估算（请求 `machine_motion`，缺省按默认值）
    let motion = parse_machine_motion(object)?;
    let source_type = object
        .get("source_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let material = object
        .get("material")
        .and_then(|v| v.as_str())
        .unwrap_or("椴木");
    let thickness = parse_float(object.get("thickness_mm"), "thickness_mm", 3.0).unwrap_or(3.0);

    let root = generated_root
        .or(upload_root)
        .ok_or("生成与上传目录未配置")?;
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);

    if source_type == "image" {
        let image_file = object
            .get("image_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if image_file.is_empty() {
            return Err("未指定图片素材路径".into());
        }

        let image_path = resolve_preview_file(std::path::Path::new(image_file), preview_roots)
            .or_else(|_| {
                if let Some(up) = upload_root {
                    let candidate = up.join(image_file);
                    if candidate.exists() {
                        return Ok(candidate);
                    }
                }
                Err("找不到指定的素材图片文件")
            })
            .map_err(|e| e.to_string())?;

        let image_bytes = std::fs::read(&image_path).map_err(|e| format!("读取素材失败: {e}"))?;
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(&image_bytes);
        let _fingerprint = hex::encode(hasher.finalize());

        let decoded =
            image::load_from_memory(&image_bytes).map_err(|e| format!("解析图片格式失败: {e}"))?;
        // 透明背景按白色（不出光）处理，避免透明 PNG 整片出光
        let luma_img = laser_core::image_job::flatten_on_white(&decoded);
        if luma_img.width() == 0 || luma_img.height() == 0 {
            return Err("图片尺寸为 0".into());
        }

        let laser_mode = object
            .get("laser_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("engrave");
        let engraving_mode = object
            .get("engraving_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("raster");
        let is_cut =
            laser_mode.eq_ignore_ascii_case("cut") || engraving_mode.eq_ignore_ascii_case("cut");
        let recommendation = laser_core::material::recommend_in_library(
            library,
            material,
            thickness,
            if is_cut { "cut" } else { laser_mode },
            if is_cut { None } else { Some(engraving_mode) },
            1000.0,
        );
        let power = recommendation
            .as_ref()
            .map(|r| r.params.laser_max_power)
            .unwrap_or(400);
        let feed_rate = recommendation
            .as_ref()
            .map(|r| r.params.feed_rate)
            .unwrap_or(1500);
        let material_pixel = recommendation
            .as_ref()
            .map(|r| r.params.pixel_size_mm)
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(0.1);
        let rec_passes = recommendation
            .as_ref()
            .map(|r| r.params.passes.clamp(1, 5) as u32)
            .unwrap_or(1);
        let params =
            feedback::generation_params(object, power as f64, feed_rate as f64, rec_passes)?;
        let power = params.power as i32;
        let feed_rate = params.feed as i32;

        // 成品尺寸、机器幅面、材料像素间距与灰度处理（laser_core::image_job）
        let optional_number = |key: &str| -> Result<Option<f64>, String> {
            match object.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(serde_json::Value::String(text)) if text.trim().is_empty() => Ok(None),
                Some(value) => value
                    .as_f64()
                    .or_else(|| {
                        value
                            .as_str()
                            .and_then(|text| text.trim().parse::<f64>().ok())
                    })
                    .filter(|v| v.is_finite())
                    .map(Some)
                    .ok_or_else(|| format!("{key} 必须是数字")),
            }
        };
        let threshold = match optional_number("threshold")? {
            None => None,
            Some(v) if (0.0..=255.0).contains(&v) => Some(v.round() as u8),
            Some(_) => return Err("threshold 必须在 0 ~ 255 之间".into()),
        };
        // 2026-11-16（技术书 r38）对齐 LightBurn Image Mode 的可选项，全部缺省时与旧行为一致
        let extras = ImageModeExtras::parse(object, &optional_number)?;
        let raster_options = laser_core::image_job::ImageRasterOptions {
            pixel_size_mm: optional_number("pixel_size_mm")?.unwrap_or(material_pixel),
            width_mm: optional_number("width_mm")?,
            height_mm: optional_number("height_mm")?,
            work_area_width_mm: optional_number("work_area_width_mm")?
                .unwrap_or(MACHINE_WORK_AREA_MM),
            work_area_height_mm: optional_number("work_area_height_mm")?
                .unwrap_or(MACHINE_WORK_AREA_MM),
            scan_overscan_mm: IMAGE_OVERSCAN_MM,
            mode: laser_core::image_job::ToneMode::parse(
                object.get("image_mode").and_then(|v| v.as_str()),
            )?,
            threshold,
            negative: extras.negative,
            halftone_cell_mm: extras.halftone_cell_mm,
            halftone_angle_deg: extras.halftone_angle_deg,
            gray_levels: extras.gray_levels,
            pass_through: extras.pass_through,
        };
        let prepared = laser_core::image_job::prepare_raster(&luma_img, &raster_options)?;
        let (img_w, img_h) = (prepared.grid_width, prepared.grid_height);
        let pixel_size_mm = prepared.pixel_size_mm;
        if extras.dot_width_mm >= pixel_size_mm {
            return Err(format!(
                "dot_width_mm（点宽补偿）必须小于行间距 {pixel_size_mm:.3} mm，收到 {:.3} mm",
                extras.dot_width_mm
            ));
        }
        let writer_options = laser_core::gcode::RasterWriterOptions {
            pixel_size_mm,
            overscan_mm: IMAGE_OVERSCAN_MM,
            work_area_width_mm: raster_options.work_area_width_mm,
            work_area_height_mm: raster_options.work_area_height_mm,
            power,
            feed_rate,
            laser_mode: "M4",
            snake_scan: true,
            material: Some(material.to_string()),
            safety_note: None,
            power_scale: None,
            dot_width_mm: extras.dot_width_mm,
        };
        let mut gray_power = None;
        let gcode_str = if let (Some(levels), Some(count)) =
            (&prepared.levels, prepared.gray_levels)
        {
            // 灰度模式：最大功率 = 本次功率（材料库推荐 / 自定义 / 采用的调参建议），最小功率由请求给出
            let min_power = (extras.gray_min_power_percent * 10.0).round() as i32;
            if min_power >= power {
                return Err(format!(
                    "gray_min_power_percent（灰度最小功率）{}% 必须小于本次最大功率 {:.1}%",
                    extras.gray_min_power_percent,
                    f64::from(power) / 10.0
                ));
            }
            let table = laser_core::raster::grayscale_power_table(count, min_power, power);
            gray_power = Some((min_power, power));
            laser_core::gcode::write_grayscale_raster(levels, &table, &writer_options)
                .map_err(|e| e.to_string())?
        } else {
            let plan = laser_core::raster::raster_plan_with_direction(
                &prepared.bitmap,
                "horizontal",
                "scanline",
            )
            .map_err(|e| e.to_string())?;
            laser_core::gcode::write_raster(&plan, img_w as usize, img_h as usize, &writer_options)
                .map_err(|e| e.to_string())?
        };

        let id = format!("img-{}-{}-{}", unix_seconds(), nanos, std::process::id());
        let gcode_str = feedback::repeat_generated(&gcode_str, params.passes);
        let gcode_path = root.join(format!("{id}.gcode"));
        std::fs::write(&gcode_path, gcode_str.as_bytes()).map_err(|e| e.to_string())?;

        // 预览图就是实际要雕刻的黑白网格（黑 = 出光），而不是原图；原图（透明已按白色合成）另存一份，
        // 供“采用反馈建议重新生成”从原图重新处理，避免对已抖动的图再抖动一次
        let preview_path = root.join(format!("{id}.png"));
        prepared
            .preview()
            .save(&preview_path)
            .map_err(|e| e.to_string())?;
        let source_name = format!("{id}-source.png");
        luma_img
            .save(root.join(&source_name))
            .map_err(|e| e.to_string())?;

        let estimate = laser_core::gcode::estimate_time_with(&gcode_str, &motion.profile);
        let (width_mm, height_mm) = (prepared.width_mm, prepared.height_mm);

        let img_name = format!("{id}.png");
        let gcode_name = format!("{id}.gcode");
        let gcode_sha = hex::encode(sha2::Sha256::digest(gcode_str.as_bytes()));

        let mut res = serde_json::Map::new();
        res.insert("actual_params".into(), params.json());
        res.insert("image_file".into(), serde_json::Value::String(img_name));
        res.insert("gcode_file".into(), serde_json::Value::String(gcode_name));
        res.insert("gcode_sha256".into(), serde_json::Value::String(gcode_sha));
        res.insert(
            "material".into(),
            serde_json::Value::String(material.to_string()),
        );
        res.insert("thickness_mm".into(), serde_json::json!(thickness));
        res.insert("width_mm".into(), serde_json::json!(width_mm));
        res.insert("height_mm".into(), serde_json::json!(height_mm));
        res.insert(
            "source_image_file".into(),
            serde_json::Value::String(source_name),
        );
        res.insert("pixel_size_mm".into(), serde_json::json!(pixel_size_mm));
        res.insert(
            "image_mode".into(),
            serde_json::json!(prepared.mode.as_str()),
        );
        if let Some(value) = prepared.threshold {
            res.insert("threshold".into(), serde_json::json!(value));
        }
        res.insert("negative".into(), serde_json::json!(prepared.negative));
        res.insert(
            "pass_through".into(),
            serde_json::json!(prepared.pass_through),
        );
        res.insert(
            "dot_width_mm".into(),
            serde_json::json!(extras.dot_width_mm),
        );
        if let (Some(cell), Some(angle)) = (prepared.halftone_cell_mm, prepared.halftone_angle_deg)
        {
            res.insert("halftone_cell_mm".into(), serde_json::json!(cell));
            res.insert("halftone_lpi".into(), serde_json::json!(25.4 / cell));
            res.insert("halftone_angle_deg".into(), serde_json::json!(angle));
        }
        if let (Some(count), Some((min_power, max_power))) = (prepared.gray_levels, gray_power) {
            res.insert("gray_levels".into(), serde_json::json!(count));
            res.insert("gray_min_power".into(), serde_json::json!(min_power));
            res.insert("gray_max_power".into(), serde_json::json!(max_power));
            res.insert(
                "gray_min_power_percent".into(),
                serde_json::json!(extras.gray_min_power_percent),
            );
        }
        res.insert(
            "auto_fitted".into(),
            serde_json::json!(prepared.auto_fitted),
        );
        // 图片任务以开始时激光头位置为图片左下角（栅格写出器右移一个过扫描距离），走边框按激光头实际经过的范围
        res.insert("origin".into(), serde_json::json!("lower_left"));
        res.insert(
            "origin_note".into(),
            serde_json::json!("开刻前把激光头移到图片左下角"),
        );
        res.insert("frame_mm".into(), serde_json::json!({
            "min_x": 0.0, "max_x": width_mm + 2.0 * IMAGE_OVERSCAN_MM, "min_y": 0.0, "max_y": height_mm
        }));
        res.insert("frame_hull_mm".into(), frame_hull_json(&gcode_str));
        if prepared.auto_fitted {
            res.insert("fit_note".into(), serde_json::json!(format!(
                "原图按 {:.2} mm/像素会超出机器幅面，已等比缩小到 {width_mm:.1} × {height_mm:.1} mm",
                raster_options.pixel_size_mm
            )));
        }
        res.insert(
            "time_estimate".into(),
            time_estimate_json(&estimate, &motion),
        );
        return Ok(Some(res));
    } else if source_type == "gcode" {
        let gcode_file = object
            .get("gcode_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if gcode_file.is_empty() {
            return Err("未指定 G-code 刀路路径".into());
        }

        let mut gcode_roots = preview_roots.to_vec();
        for extra in [generated_root, upload_root].into_iter().flatten() {
            let buf = extra.to_path_buf();
            if !gcode_roots.contains(&buf) {
                gcode_roots.push(buf);
            }
        }
        let gcode_path = resolve_gcode_file(std::path::Path::new(gcode_file), &gcode_roots)
            .map_err(|_| "找不到指定的 G-code 刀路文件".to_string())?;

        let gcode_bytes =
            std::fs::read(&gcode_path).map_err(|e| format!("读取刀路文件失败: {e}"))?;
        let gcode_str = String::from_utf8_lossy(&gcode_bytes);

        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(&gcode_bytes);
        let fingerprint = hex::encode(hasher.finalize());

        let moves = laser_core::gcode::parse_moves(&gcode_str);
        let estimate = laser_core::gcode::estimate_time_with(&gcode_str, &motion.profile);

        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        let mut paths: Vec<Vec<laser_contracts::geometry::Point>> = Vec::new();
        let mut current_path: Vec<laser_contracts::geometry::Point> = Vec::new();
        let mut cur_pos = (0.0, 0.0);

        for m in moves {
            if let Some(x) = m.x {
                cur_pos.0 = x;
            }
            if let Some(y) = m.y {
                cur_pos.1 = y;
            }
            if cur_pos.0.is_finite() && cur_pos.1.is_finite() {
                min_x = min_x.min(cur_pos.0);
                max_x = max_x.max(cur_pos.0);
                min_y = min_y.min(cur_pos.1);
                max_y = max_y.max(cur_pos.1);
            }
            if m.rapid {
                if current_path.len() > 1 {
                    paths.push(std::mem::take(&mut current_path));
                } else {
                    current_path.clear();
                }
                current_path.push(cur_pos);
            } else {
                current_path.push(cur_pos);
            }
        }
        if current_path.len() > 1 {
            paths.push(current_path);
        }

        if !min_x.is_finite() {
            min_x = 0.0;
            max_x = 50.0;
            min_y = 0.0;
            max_y = 50.0;
        }

        let width_mm = (max_x - min_x).max(1.0);
        let height_mm = (max_y - min_y).max(1.0);

        // G-code 机器坐标 Y 轴向上，图像坐标 Y 轴向下：翻转后用户看到的即工件正面视角。
        // 解析出的轨迹已包含刀路里显式写出的闭合段，因此不再自动补画首尾闭合线。
        for path in &mut paths {
            for point in path.iter_mut() {
                point.0 -= min_x;
                point.1 = max_y - point.1;
            }
        }

        let id = format!("gc-{}-{}-{}", unix_seconds(), nanos, std::process::id());
        let preview_path = root.join(format!("{id}.png"));
        let preview_img = laser_core::gcode::preview_outline_rgba_with_options(
            &paths, 800, 600, width_mm, height_mm, false,
        )
        .map_err(|e| e.to_string())?;
        preview_img.save(&preview_path).map_err(|e| e.to_string())?;

        let img_name = format!("{id}.png");
        let gcode_name = gcode_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or(gcode_file)
            .to_string();

        let mut res = serde_json::Map::new();
        res.insert("image_file".into(), serde_json::Value::String(img_name));
        res.insert("gcode_file".into(), serde_json::Value::String(gcode_name));
        res.insert(
            "gcode_sha256".into(),
            serde_json::Value::String(fingerprint),
        );
        res.insert(
            "material".into(),
            serde_json::Value::String(material.to_string()),
        );
        res.insert("thickness_mm".into(), serde_json::json!(thickness));
        res.insert("width_mm".into(), serde_json::json!(width_mm));
        res.insert("height_mm".into(), serde_json::json!(height_mm));
        res.insert(
            "time_estimate".into(),
            time_estimate_json(&estimate, &motion),
        );
        return Ok(Some(res));
    }

    Ok(None)
}

pub(crate) fn next_workflow_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("wf-{}-{seq}", unix_seconds())
}
