//! 画板预览：光栅、描边与矢量（场景图形规划路线）三种模式。

use super::*;

pub(crate) fn generate_draw_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    generated_root: Option<&std::path::Path>,
    library: &laser_contracts::MaterialLibrary,
    // 全局雕刻路线（r45）：请求没写 `mode` 时，矢量 → vector，转图片 → raster
    default_route: crate::engrave_route::EngraveRoute,
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

    let requested_mode =
        object
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or(match default_route {
                crate::engrave_route::EngraveRoute::Vector => "vector",
                crate::engrave_route::EngraveRoute::Image => "raster",
            });
    if !matches!(requested_mode, "vector" | "raster" | "outline") {
        return Err(format!(
            "mode 只能是 vector / raster / outline，收到 {requested_mode}"
        ));
    }
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
    // 矢量刻法（r46）：按画布颜色（默认）/ 轮廓 / 填充
    let vector_style =
        crate::sketch::VectorStyle::parse(object.get("vector_style").and_then(|v| v.as_str()))?;
    // 切割（r47）：只在矢量刻法选“切割”时；其余画板任务都是雕刻，带切割设置直接拒绝
    let is_cut = requested_mode == "vector" && vector_style == crate::sketch::VectorStyle::Cut;
    if !is_cut
        && !matches!(
            object.get("cut_options"),
            None | Some(serde_json::Value::Null)
        )
    {
        return Err("cut_options（切割设置）只用于画板矢量刻法“切割”或工作台文字的外框切割".into());
    }
    let (sketch, layout_sketch) = if requested_mode == "vector" {
        match object.get("scene_json") {
            Some(scene) => {
                let font = &laser_core::font::find_system_font_for_text;
                let full = crate::sketch::sketch_from_scene(scene, font, vector_style)?;
                match &selection {
                    Some(ids) => {
                        let picked = crate::sketch::sketch_from_scene(
                            &crate::sketch::select_elements(scene, ids),
                            font,
                            vector_style,
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
    // 矢量走线与轮廓描边用同一档材料参数；矢量“填充”刻法用填充（光栅）参数，同工作台文字实心填充（r46）
    let recommend_mode = match mode {
        "vector" if vector_style == crate::sketch::VectorStyle::Fill => "raster",
        "vector" => "outline",
        other => other,
    };

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

    // 透明背景按白色（不出光）合成，与工作台图片一致
    let luma_img = laser_core::image_job::flatten_on_white(
        &image::load_from_memory(&image_bytes).map_err(|e| format!("图像加载失败: {e}"))?,
    );
    let img_w = luma_img.width();
    let img_h = luma_img.height();
    if img_w == 0 || img_h == 0 {
        return Err("图像尺寸为 0".into());
    }

    let recommendation = laser_core::material::recommend_in_library(
        library,
        material,
        thickness,
        if is_cut { "cut" } else { "engrave" },
        if is_cut { None } else { Some(recommend_mode) },
        1000.0,
    );
    // 切割必须有该厚度的切割参数，不能拿默认值去切
    if is_cut && recommendation.is_none() {
        return Err(format!(
            "材料库里还没有「{material} {thickness}mm」的切割参数，请先在材料实验室做切割标定并保存"
        ));
    }
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
    // 转图片（光栅）与工作台图片同一套：过扫描按速度自动、双向扫描偏移（r45）
    let raster_scan = if mode == "raster" {
        Some(super::image_upload::raster_scan_options(
            object,
            feed_rate as f64,
            &motion,
            recommendation.as_ref(),
        )?)
    } else {
        None
    };
    let raster_overscan = raster_scan
        .as_ref()
        .map_or(IMAGE_OVERSCAN_MM, |scan| scan.options.overscan_mm);

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
        area_w - 2.0 * raster_overscan
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
    let mut raster_details = None;
    let mut cut_json = None;
    let gcode_str = if let (Some(sketch), "vector") = (&sketch, mode) {
        let (fill_request, fill_from_material) = merge_layer_options(
            object.get("fill_options"),
            recommendation.as_ref().and_then(|r| {
                r.params
                    .layer_options(laser_contracts::MATERIAL_FILL_OPTIONS_KEY)
            }),
            laser_contracts::MATERIAL_FILL_OPTION_KEYS,
        );
        let mut fill = parse_fill_options(
            fill_request.as_ref(),
            feed_rate as f64,
            motion.overscan_accel(),
        )?;
        fill.from_material = fill_from_material;
        // 切割设置（r47）：请求没写的键取材料库切割条目随材料保存的值，同工作台文字外框切割
        let cut_setup = if is_cut {
            let (request, from_material) = merge_layer_options(
                object.get("cut_options"),
                recommendation.as_ref().and_then(|r| {
                    r.params
                        .layer_options(laser_contracts::MATERIAL_CUT_OPTIONS_KEY)
                }),
                laser_contracts::MATERIAL_CUT_OPTION_KEYS,
            );
            Some((parse_cut_options(request.as_ref())?, from_material))
        } else {
            None
        };
        let (planned, cut_report) = plan_sketch(
            sketch,
            layout_sketch.as_ref().unwrap_or(sketch),
            width_mm,
            height_mm,
            laser_core::vector_plan::FillOptions {
                spacing: fill_spacing,
                ..fill.options
            },
            &parse_path_options(object)?,
            cut_setup.as_ref().map(|(cut, _)| cut),
        )?;
        if let Some((cut, from_material)) = &cut_setup {
            cut_json = Some(cut_options_json(cut, cut_report.as_ref(), from_material));
        }
        path_optimization = Some(planned.stats.clone());
        vector_details = Some((
            sketch.has_fills().then(|| {
                fill_options_json(
                    &laser_core::vector_plan::FillOptions {
                        spacing: fill_spacing,
                        ..fill.options
                    },
                    &fill,
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
        // 转图片（2026-11-17 技术书 r45，与工作台图片同一套引擎）：按材料线距（可乘 line_interval_scale）重采样到
        // 成品尺寸，图片模式默认自动识别（画板线稿多为 Otsu 阈值，也可选 Jarvis 等抖动、灰度变功率），
        // 过扫描按速度自动、双向扫描偏移
        let scan = raster_scan.as_ref().expect("光栅模式已解析扫描设置");
        let number = |key: &str| crate::http::parse_optional_float(object.get(key), key);
        let extras = super::image_upload::ImageModeExtras::parse(object, &number)?;
        if extras.pass_through {
            return Err("画板转图片不支持直通（pass_through），请去掉".into());
        }
        let interval_scale = number("line_interval_scale")?;
        if let Some(scale) = interval_scale
            && !(0.5..=3.0).contains(&scale)
        {
            return Err("line_interval_scale（行间距倍数）须在 0.5 ~ 3 之间".into());
        }
        let threshold = match number("threshold")? {
            None => None,
            Some(v) if (0.0..=255.0).contains(&v) => Some(v.round() as u8),
            Some(_) => return Err("threshold 必须在 0 ~ 255 之间".into()),
        };
        let raster_options = laser_core::image_job::ImageRasterOptions {
            pixel_size_mm: fill_spacing * interval_scale.unwrap_or(1.0),
            width_mm: Some(width_mm),
            height_mm: Some(height_mm),
            work_area_width_mm: area_w,
            work_area_height_mm: area_h,
            scan_overscan_mm: scan.options.overscan_mm,
            mode: laser_core::image_job::ToneMode::parse(
                object.get("image_mode").and_then(|v| v.as_str()),
            )?,
            threshold,
            negative: extras.negative,
            halftone_cell_mm: extras.halftone_cell_mm,
            halftone_angle_deg: extras.halftone_angle_deg,
            gray_levels: extras.gray_levels,
            pass_through: false,
        };
        let prepared = laser_core::image_job::prepare_raster(&luma_img, &raster_options)?;
        prepared
            .preview()
            .save(root.join(&path_preview_name))
            .map_err(|e| e.to_string())?;
        let (gcode, gray_power) = super::image_upload::write_prepared_raster(
            &prepared,
            &extras,
            scan,
            material,
            power,
            feed_rate,
            (area_w, area_h),
        )?;
        raster_details = Some((prepared, extras, gray_power, interval_scale));
        gcode
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
    if let (Some((prepared, extras, gray_power, interval_scale)), Some(scan)) =
        (&raster_details, &raster_scan)
    {
        if let Some(scale) = interval_scale {
            res_map.insert("line_interval_scale".into(), serde_json::json!(scale));
        }
        super::image_upload::insert_raster_fields(
            &mut res_map,
            prepared,
            extras,
            scan,
            *gray_power,
        );
    }
    if mode == "vector" {
        res_map.insert(
            "vector_style".into(),
            serde_json::json!(vector_style.as_str()),
        );
    }
    res_map.insert(
        "laser_mode".into(),
        serde_json::json!(if is_cut { "cut" } else { "engrave" }),
    );
    if let Some(cut) = cut_json {
        res_map.insert("cut_options".into(), cut);
    }
    res_map.insert(
        "engrave_route".into(),
        serde_json::json!(if mode == "vector" { "vector" } else { "image" }),
    );
    res_map.insert("origin".into(), serde_json::json!("lower_left"));
    res_map.insert(
        "origin_note".into(),
        serde_json::json!("开刻前把激光头移到图案左下角"),
    );
    let frame_max_x = if mode == "outline" || mode == "vector" {
        width_mm
    } else {
        width_mm + 2.0 * raster_overscan
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
    // 切割刻法（r47）：先内后外切穿，切缝补偿按嵌套层数决定方向，规划后再加桥位、引线、过切
    cut: Option<&laser_core::cut::CutOptions>,
) -> Result<
    (
        laser_core::vector_plan::Planned,
        Option<laser_core::cut::CutReport>,
    ),
    String,
> {
    use laser_core::vector_plan::{FillOptions, FillRule, Unit, plan_layers, scan_fill};
    const PADDING: f64 = 16.0;
    let (x0, y0, x1, y1) = layout.bounds().ok_or("画板里没有可刻的图形")?;
    let (reach_x, reach_y) = if let Some(cut) = cut {
        cut.validate().map_err(|e| e.to_string())?;
        let reach = if cut.is_active() {
            cut.extension_mm()
        } else {
            0.0
        };
        (reach, reach)
    } else if layout.has_fills() {
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
    if let Some(cut) = cut {
        return plan_cut(&mm, height_mm, path_options, cut);
    }
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
    let planned = plan_layers(vec![fills, strokes], (0.0, height_mm), path_options)
        .map_err(|e| e.to_string())?;
    Ok((planned, None))
}

/// 点在多边形里（射线法，首尾点可重复）。
fn inside(p: laser_contracts::geometry::Point, ring: &[laser_contracts::geometry::Point]) -> bool {
    let n = ring.len();
    let mut hit = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            hit = !hit;
        }
    }
    hit
}

/// 画板切割（r47）：闭合图形各自一组、同一段文字一组；每组被别的组的轮廓包了几层决定是零件外框（偶数层，
/// 切缝往外）还是孔（奇数层，切缝往孔里），切缝补偿按组做（文字组内按非零环绕合并、字里的孔自动反向）；
/// 开放线条沿线切。先内后外规划，出光长度自检后再加桥位、引线、过切（同工作台文字外框切割）。
fn plan_cut(
    mm: &crate::sketch::SketchGeometry,
    height_mm: f64,
    path_options: &laser_core::vector_plan::PlanOptions,
    cut: &laser_core::cut::CutOptions,
) -> Result<
    (
        laser_core::vector_plan::Planned,
        Option<laser_core::cut::CutReport>,
    ),
    String,
> {
    use laser_core::vector_plan::{PlanOptions, Unit, burn_paths, plan_layers};
    let groups: Vec<Vec<Vec<laser_contracts::geometry::Point>>> = mm
        .closed
        .iter()
        .map(|ring| vec![ring.clone()])
        .chain(mm.cut_text.iter().cloned())
        .filter(|group| !group.is_empty())
        .collect();
    let mut units: Vec<Unit> = Vec::new();
    let (mut before, mut after) = (0, 0);
    for (index, group) in groups.iter().enumerate() {
        if cut.kerf_mm == 0.0 {
            units.extend(group.iter().cloned().map(Unit::Closed));
            continue;
        }
        let first = &group[0];
        let probe = if first.len() >= 2 {
            (
                (first[0].0 + first[1].0) / 2.0,
                (first[0].1 + first[1].1) / 2.0,
            )
        } else {
            first[0]
        };
        let depth: usize = groups
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, other)| other.iter().filter(|ring| inside(probe, ring)).count())
            .sum();
        let kerf = if depth.is_multiple_of(2) {
            cut.kerf_mm
        } else {
            -cut.kerf_mm
        };
        let offset = laser_core::cut::offset_contours(group, kerf).map_err(|e| e.to_string())?;
        before += offset.before;
        after += offset.after;
        units.extend(offset.contours.into_iter().map(Unit::Closed));
    }
    units.extend(mm.open.iter().cloned().map(Unit::Open));
    let mut planned = plan_layers(
        vec![units],
        (0.0, height_mm),
        &PlanOptions {
            inside_first: true,
            ..*path_options
        },
    )
    .map_err(|e| e.to_string())?;
    let mut report = if cut.needs_post_processing() {
        let (steps, report) =
            laser_core::cut::apply_cut_options(&planned.steps, cut).map_err(|e| e.to_string())?;
        planned.paths = burn_paths(&steps);
        planned.steps = steps;
        report
    } else {
        laser_core::cut::CutReport::default()
    };
    if cut.kerf_mm != 0.0 {
        report.kerf_contours = Some((before, after));
    }
    if report.contours == 0 {
        report.contours = groups.iter().map(Vec::len).sum();
    }
    Ok((planned, cut.is_active().then_some(report)))
}
