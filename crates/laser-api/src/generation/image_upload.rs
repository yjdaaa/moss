//! 图片与 G-code 上传预览：图片模式（对齐 LightBurn Image Mode）与 G-code 文件解析出的轨迹预览。

use super::*;

/// 图片预览 `line_interval_scale`（行间距 = 材料推荐线距 × 倍数）的取值范围，前端质量预设用 1.5 / 1 / 0.8。
const IMAGE_INTERVAL_SCALE_RANGE: (f64, f64) = (0.5, 3.0);

/// 图片预览里对齐 LightBurn Image Mode 的可选字段（2026-11-16，技术书 r38）。
/// 值域在这里先校验；与像素间距相关的（网点网格、点宽补偿）在得到实际行间距后再查。
/// 2026-11-17（技术书 r45）创作画板转图片同样用它。
pub(super) struct ImageModeExtras {
    pub(super) negative: bool,
    pub(super) pass_through: bool,
    pub(super) halftone_cell_mm: Option<f64>,
    pub(super) halftone_angle_deg: f64,
    pub(super) gray_levels: u8,
    pub(super) gray_min_power_percent: f64,
    pub(super) dot_width_mm: f64,
}

impl ImageModeExtras {
    pub(super) fn parse(
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

/// 位图扫描的过扫描、双向与扫描偏移（2026-11-17 技术书 r45，对齐 LightBurn 图片层）：读请求 `fill_options`
/// （没写的键取材料库光栅条目随材料保存的值），过扫描省略或 `auto` 时按速度与加速度自动算（下限 1 mm），
/// 扫描偏移可按速度查表；偏移的一半不能超过过扫描。
pub(super) fn raster_scan_options(
    object: &serde_json::Map<String, serde_json::Value>,
    feed_rate: f64,
    motion: &MachineMotion,
    recommendation: Option<&laser_core::material::Recommendation>,
) -> Result<ParsedFill, String> {
    let (request, from_material) = merge_layer_options(
        object.get("fill_options"),
        recommendation.and_then(|r| {
            r.params
                .layer_options(laser_contracts::MATERIAL_FILL_OPTIONS_KEY)
        }),
        laser_contracts::MATERIAL_FILL_OPTION_KEYS,
    );
    let mut parsed = parse_fill_options(request.as_ref(), feed_rate, motion.overscan_accel())?;
    parsed.from_material = from_material;
    let options = &parsed.options;
    if options.bidirectional && options.scan_offset_mm.abs() / 2.0 > options.overscan_mm + 1e-9 {
        return Err(format!(
            "扫描偏移的一半（{:.2} mm）不能超过过扫描（{:.2} mm），请加大过扫描或改成自动",
            options.scan_offset_mm.abs() / 2.0,
            options.overscan_mm
        ));
    }
    Ok(parsed)
}

/// 按 [`raster_scan_options`] 的结果写位图 G-code（灰度模式变功率）；返回 G-code 与灰度的最小 / 最大 S 值。
pub(super) fn write_prepared_raster(
    prepared: &laser_core::image_job::PreparedRaster,
    extras: &ImageModeExtras,
    scan: &ParsedFill,
    material: &str,
    power: i32,
    feed_rate: i32,
    work_area: (f64, f64),
) -> Result<(String, Option<(i32, i32)>), String> {
    let pixel_size_mm = prepared.pixel_size_mm;
    if extras.dot_width_mm >= pixel_size_mm {
        return Err(format!(
            "dot_width_mm（点宽补偿）必须小于行间距 {pixel_size_mm:.3} mm，收到 {:.3} mm",
            extras.dot_width_mm
        ));
    }
    let bidirectional = scan.options.bidirectional;
    let writer_options = laser_core::gcode::RasterWriterOptions {
        pixel_size_mm,
        overscan_mm: scan.options.overscan_mm,
        work_area_width_mm: work_area.0,
        work_area_height_mm: work_area.1,
        power,
        feed_rate,
        laser_mode: "M4",
        snake_scan: bidirectional,
        material: Some(material.to_string()),
        safety_note: None,
        power_scale: None,
        dot_width_mm: extras.dot_width_mm,
        scan_offset_mm: if bidirectional {
            scan.options.scan_offset_mm
        } else {
            0.0
        },
    };
    if let (Some(levels), Some(count)) = (&prepared.levels, prepared.gray_levels) {
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
        let gcode = laser_core::gcode::write_grayscale_raster(levels, &table, &writer_options)
            .map_err(|e| e.to_string())?;
        Ok((gcode, Some((min_power, power))))
    } else {
        let plan = laser_core::raster::raster_plan_with_direction(
            &prepared.bitmap,
            "horizontal",
            "scanline",
        )
        .map_err(|e| e.to_string())?;
        let gcode = laser_core::gcode::write_raster(
            &plan,
            prepared.grid_width as usize,
            prepared.grid_height as usize,
            &writer_options,
        )
        .map_err(|e| e.to_string())?;
        Ok((gcode, None))
    }
}

/// 回显位图的转换方式与扫描设置（工作台图片与画板转图片共用）。
pub(super) fn insert_raster_fields(
    res: &mut serde_json::Map<String, serde_json::Value>,
    prepared: &laser_core::image_job::PreparedRaster,
    extras: &ImageModeExtras,
    scan: &ParsedFill,
    gray_power: Option<(i32, i32)>,
) {
    res.insert(
        "pixel_size_mm".into(),
        serde_json::json!(prepared.pixel_size_mm),
    );
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
    if let (Some(cell), Some(angle)) = (prepared.halftone_cell_mm, prepared.halftone_angle_deg) {
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
        "scan_options".into(),
        serde_json::json!({
            "overscan_mm": scan.options.overscan_mm,
            "overscan_auto": scan.auto,
            "bidirectional": scan.options.bidirectional,
            "scan_offset_mm": if scan.options.bidirectional { scan.options.scan_offset_mm } else { 0.0 },
            "scan_offset_table": scan.offset_table,
            "from_material": scan.from_material,
        }),
    );
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
        // 过扫描按速度自动、双向扫描偏移（r45）
        let scan = raster_scan_options(object, feed_rate as f64, &motion, recommendation.as_ref())?;
        let overscan_mm = scan.options.overscan_mm;

        // 成品尺寸、机器幅面、材料像素间距与灰度处理（laser_core::image_job）
        let optional_number = |key: &str| crate::http::parse_optional_float(object.get(key), key);
        let threshold = match optional_number("threshold")? {
            None => None,
            Some(v) if (0.0..=255.0).contains(&v) => Some(v.round() as u8),
            Some(_) => return Err("threshold 必须在 0 ~ 255 之间".into()),
        };
        // 2026-11-16（技术书 r38）对齐 LightBurn Image Mode 的可选项，全部缺省时与旧行为一致
        let extras = ImageModeExtras::parse(object, &optional_number)?;
        // 2026-11-16（技术书 r41）质量预设“快速 / 均衡 / 精细”：行间距 = 材料推荐线距 × line_interval_scale
        let interval_scale = optional_number("line_interval_scale")?;
        let base_pixel_mm = match (optional_number("pixel_size_mm")?, interval_scale) {
            (Some(_), Some(_)) => {
                return Err("pixel_size_mm 与 line_interval_scale 只能给一个".into());
            }
            (Some(pixel), None) => pixel,
            (None, Some(scale))
                if (IMAGE_INTERVAL_SCALE_RANGE.0..=IMAGE_INTERVAL_SCALE_RANGE.1)
                    .contains(&scale) =>
            {
                material_pixel * scale
            }
            (None, Some(_)) => {
                return Err(format!(
                    "line_interval_scale（行间距倍数）须在 {} ~ {} 之间",
                    IMAGE_INTERVAL_SCALE_RANGE.0, IMAGE_INTERVAL_SCALE_RANGE.1
                ));
            }
            (None, None) => material_pixel,
        };
        let raster_options = laser_core::image_job::ImageRasterOptions {
            pixel_size_mm: base_pixel_mm,
            width_mm: optional_number("width_mm")?,
            height_mm: optional_number("height_mm")?,
            work_area_width_mm: optional_number("work_area_width_mm")?
                .unwrap_or(MACHINE_WORK_AREA_MM),
            work_area_height_mm: optional_number("work_area_height_mm")?
                .unwrap_or(MACHINE_WORK_AREA_MM),
            scan_overscan_mm: overscan_mm,
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
        let (gcode_str, gray_power) = write_prepared_raster(
            &prepared,
            &extras,
            &scan,
            material,
            power,
            feed_rate,
            (
                raster_options.work_area_width_mm,
                raster_options.work_area_height_mm,
            ),
        )?;

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
        use sha2::Digest;
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
        if let Some(scale) = interval_scale {
            res.insert("line_interval_scale".into(), serde_json::json!(scale));
        }
        insert_raster_fields(&mut res, &prepared, &extras, &scan, gray_power);
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
            "min_x": 0.0, "max_x": width_mm + 2.0 * overscan_mm, "min_y": 0.0, "max_y": height_mm
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

        let estimate = laser_core::gcode::estimate_time_with(&gcode_str, &motion.profile);

        // 2026-11-16（技术书 r41）：G2/G3 圆弧按弦高容差拆成折线画出，并跟踪 G90/G91、G20/G21、G92 与模态移动
        let toolpath = laser_core::gcode::preview_toolpath(&gcode_str);
        let mut paths = toolpath.paths;
        let (min_x, min_y, max_x, max_y) = toolpath.bounds.unwrap_or((0.0, 0.0, 50.0, 50.0));

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
