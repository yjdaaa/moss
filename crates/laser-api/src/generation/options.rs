//! 请求选项解析与回显：机器运动参数、填充 / 路线 / 切割选项、工时估算与走边框轮廓。

/// 请求里的机器运动参数（`machine_motion`，键名与 `/api/connection/check` 的 `machine_limits` 相同，
/// 前端可原样转发握手读到的值，或用用户填的加速度覆盖）：`x_accel_mm_s2`、`y_accel_mm_s2`、
/// `x_max_rate_mm_per_min`、`y_max_rate_mm_per_min`、`junction_deviation_mm`，全部可省略。
pub(crate) struct MachineMotion {
    pub(crate) profile: laser_core::gcode::MotionProfile,
    /// `machine`：带了机器加速度（请求、后端手填档案或固件）；`default`：按默认 1000 mm/s² 估算
    pub(crate) source: &'static str,
    /// 细粒度来源（r78）：`manual`（后端手填档案）/ `firmware`（固件读回）/ `request`（请求显式传入）/ `default`
    pub(crate) origin: &'static str,
}

impl MachineMotion {
    /// 自动过扫描用的加速度：两轴取小（扫描方向不定时保守）。
    pub(crate) fn overscan_accel(&self) -> f64 {
        self.profile.accel_x_mm_s2.min(self.profile.accel_y_mm_s2)
    }
}

/// r66：有效雕刻进给速度超过固件两轴最大速度中的较小值时，返回非阻断警告文案（不修改、不夹紧原速度）。
pub(crate) fn feed_exceeds_max_rate_warning(
    feed_rate_mm_per_min: f64,
    motion: &MachineMotion,
) -> Option<String> {
    if !feed_rate_mm_per_min.is_finite() || feed_rate_mm_per_min <= 0.0 {
        return None;
    }
    let max_rate = match (
        motion.profile.max_rate_x_mm_per_min,
        motion.profile.max_rate_y_mm_per_min,
    ) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }?;
    if feed_rate_mm_per_min <= max_rate + 1e-6 {
        return None;
    }
    let fmt = |v: f64| {
        if (v - v.round()).abs() < 1e-6 {
            format!("{v:.0}")
        } else {
            format!("{v}")
        }
    };
    Some(format!(
        "雕刻速度 {} mm/min 超过固件最大速度 {} mm/min，固件会自动降速，工时与出光效果会偏离设定",
        fmt(feed_rate_mm_per_min),
        fmt(max_rate)
    ))
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
    let has_accel = ax.is_some() || ay.is_some();
    let origin = if has_accel {
        match object
            .get("_acceleration_origin")
            .and_then(serde_json::Value::as_str)
        {
            Some("manual") => "manual",
            Some("firmware") => "firmware",
            Some("request") => "request",
            _ => "request",
        }
    } else {
        "default"
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
        source: if has_accel { "machine" } else { "default" },
        origin,
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
        "acceleration_origin": motion.origin,
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

/// 请求里的 `fill_options` 解析结果；`auto` 表示过扫描按速度与加速度自动算；
/// `offset_table` 为排好序的“速度 → 偏移”表（给了表时 `options.scan_offset_mm` 是在加工速度处插值的结果）。
pub(crate) struct ParsedFill {
    pub(crate) options: laser_core::vector_plan::FillOptions,
    pub(crate) auto: bool,
    pub(crate) offset_table: Option<Vec<(f64, f64)>>,
    /// 扫描偏移来源（r78）：`machine_profile`（后端机器档案）/ `request`（请求显式给出）/ `default`（默认 0）
    pub(crate) scan_offset_origin: &'static str,
    /// 取自材料库条目的键（请求没写、由材料库补上的，r40）
    pub(crate) from_material: Vec<String>,
}

/// 请求里的图层设置与材料库条目随材料保存的值合并（r40，2026-11-16）：请求写了（非 null、非空串）的键优先，
/// 其余键取材料库条目（只取 `allowed` 里的键）。返回合并后的值与取自材料库的键（排好序）。
/// 请求的值不是对象时原样返回，交给解析函数报错。桥位个数 / 间距二选一：请求写了任一个，
/// 材料库里的两个都不用，避免拼出“两个都填”。
pub(crate) fn merge_layer_options(
    request: Option<&serde_json::Value>,
    material: Option<&serde_json::Map<String, serde_json::Value>>,
    allowed: &[&str],
) -> (Option<serde_json::Value>, Vec<String>) {
    let request_map = match request {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Object(map)) => Some(map),
        Some(other) => return (Some(other.clone()), Vec::new()),
    };
    let Some(material) = material else {
        return (request.cloned(), Vec::new());
    };
    let given = |key: &str| {
        request_map
            .and_then(|map| map.get(key))
            .is_some_and(|value| match value {
                serde_json::Value::Null => false,
                serde_json::Value::String(text) => !text.trim().is_empty(),
                _ => true,
            })
    };
    let tabs_given = given("tab_count") || given("tab_spacing_mm");
    let mut merged = request_map.cloned().unwrap_or_default();
    let mut from_material = Vec::new();
    for (key, value) in material {
        if !allowed.contains(&key.as_str()) || value.is_null() || given(key) {
            continue;
        }
        if tabs_given && matches!(key.as_str(), "tab_count" | "tab_spacing_mm") {
            continue;
        }
        merged.insert(key.clone(), value.clone());
        from_material.push(key.clone());
    }
    from_material.sort();
    if merged.is_empty() && request_map.is_none() {
        return (None, from_material);
    }
    (Some(serde_json::Value::Object(merged)), from_material)
}

/// 解析 `fill_options`（2026-11-16，对齐 LightBurn Fill 模式；全部可省略）：
/// `grouping`（all 默认 / individual）、`angle_deg`（0 横扫）、`cross_hatch`、`bidirectional`（默认 true）、
/// `overscan_mm`（省略或 "auto" 按 v²/2a 自动；0 关闭；上限 20）、`acceleration_mm_s2`（自动过扫描用，默认 1000）、
/// `scan_offset_mm`（双向扫描错位补偿，±2）、`scan_offset_table`（`[[速度 mm/min, 偏移 mm], …]`，≤32 行，
/// 按加工速度线性插值、两端取端点值，给了就覆盖 `scan_offset_mm`）。线距与填充规则由调用方按刻法填。
pub(crate) fn parse_fill_options(
    value: Option<&serde_json::Value>,
    feed_mm_per_min: f64,
    fallback_accel_mm_s2: f64,
) -> Result<ParsedFill, String> {
    use laser_core::vector_plan::{
        FillGrouping, FillOptions, FillRule, MAX_OVERSCAN_MM, auto_overscan_mm,
    };
    let empty = serde_json::Map::new();
    let fill = match value {
        None | Some(serde_json::Value::Null) => &empty,
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("fill_options 必须是对象".into()),
    };
    let number = |key: &str| {
        crate::http::parse_optional_float(fill.get(key), &format!("fill_options.{key}"))
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
    let fixed_offset = number("scan_offset_mm")?.unwrap_or(0.0);
    if fixed_offset.abs() > 2.0 {
        return Err("fill_options.scan_offset_mm 须在 -2 ~ 2 mm".into());
    }
    let offset_table = parse_scan_offset_table(fill.get("scan_offset_table"))?;
    let offset = match &offset_table {
        Some(table) => laser_core::vector_plan::scan_offset_at(table, feed_mm_per_min)
            .map_err(|e| format!("fill_options.scan_offset_table：{e}"))?,
        None => fixed_offset,
    };
    let scan_offset_origin = if fill
        .get("_scan_offset_origin")
        .and_then(serde_json::Value::as_str)
        == Some("machine_profile")
    {
        "machine_profile"
    } else if offset_table.is_some()
        || fill.get("scan_offset_mm").is_some_and(|v| !v.is_null())
        || fill.get("scan_offset_table").is_some_and(|v| !v.is_null())
    {
        "request"
    } else {
        "default"
    };
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
        offset_table,
        scan_offset_origin,
        from_material: Vec::new(),
    })
}

/// `fill_options.scan_offset_table`：`[[速度 mm/min, 偏移 mm], …]`，省略、null 或空数组表示不用表。
pub(crate) fn parse_scan_offset_table(
    value: Option<&serde_json::Value>,
) -> Result<Option<Vec<(f64, f64)>>, String> {
    use laser_core::vector_plan::{MAX_SCAN_OFFSET_MM, MAX_SCAN_OFFSET_ROWS};
    let rows = match value {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Array(rows)) if rows.is_empty() => return Ok(None),
        Some(serde_json::Value::Array(rows)) => rows,
        Some(_) => {
            return Err("fill_options.scan_offset_table 必须是 [[速度, 偏移], …] 数组".into());
        }
    };
    if rows.len() > MAX_SCAN_OFFSET_ROWS {
        return Err(format!(
            "fill_options.scan_offset_table 最多 {MAX_SCAN_OFFSET_ROWS} 行"
        ));
    }
    let mut table = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let pair = row
            .as_array()
            .filter(|pair| pair.len() == 2)
            .and_then(|pair| Some((pair[0].as_f64()?, pair[1].as_f64()?)));
        match pair {
            Some((speed, offset))
                if speed.is_finite()
                    && speed > 0.0
                    && speed <= 1_000_000.0
                    && offset.is_finite()
                    && offset.abs() <= MAX_SCAN_OFFSET_MM =>
            {
                table.push((speed, offset));
            }
            _ => {
                return Err(format!(
                    "fill_options.scan_offset_table 第 {} 行须为 [速度 mm/min（大于 0）, 偏移 mm（-2 ~ 2）]",
                    index + 1
                ));
            }
        }
    }
    table.sort_by(|a, b| a.0.total_cmp(&b.0));
    if table.windows(2).any(|w| w[1].0 == w[0].0) {
        return Err("fill_options.scan_offset_table 里有重复的速度".into());
    }
    Ok(Some(table))
}

/// 解析 `cut_options`（2026-11-16，对齐 LightBurn Line 模式 / Cut 设置；全部可省略，默认全关）：
/// `kerf_mm`（切缝补偿，正值往外、负值往里，±2）、`tab_count`（每个闭合图形几个桥位，0 ~ 100）或
/// `tab_spacing_mm`（每隔多少 mm 一个，1 ~ 1000，二选一）、`tab_width_mm`（默认 0.5，0.05 ~ 10）、
/// `tabs_skip_inner`（默认 true，内部图形不加桥位）、`lead_in_mm` / `lead_out_mm`（0 ~ 10）、
/// `lead_angle_deg`（1 ~ 90，默认 90 垂直）、`overcut_mm`（0 ~ 10）。
pub(crate) fn parse_cut_options(
    value: Option<&serde_json::Value>,
) -> Result<laser_core::cut::CutOptions, String> {
    use laser_core::cut::{
        CutOptions, MAX_KERF_MM, MAX_LEAD_MM, MAX_OVERCUT_MM, MAX_TAB_COUNT, MAX_TAB_SPACING_MM,
        MAX_TAB_WIDTH_MM, MIN_TAB_SPACING_MM, MIN_TAB_WIDTH_MM, TabMode,
    };
    let empty = serde_json::Map::new();
    let cut = match value {
        None | Some(serde_json::Value::Null) => &empty,
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("cut_options 必须是对象".into()),
    };
    let number = |key: &str, lo: f64, hi: f64| -> Result<Option<f64>, String> {
        let range_err = || format!("cut_options.{key} 须在 {lo} ~ {hi} 之间");
        let parsed =
            crate::http::parse_optional_float(cut.get(key), key).map_err(|_| range_err())?;
        match parsed {
            None => Ok(None),
            Some(v) if (lo..=hi).contains(&v) => Ok(Some(v)),
            Some(_) => Err(range_err()),
        }
    };
    let defaults = CutOptions::default();
    let tab_count = number("tab_count", 0.0, f64::from(MAX_TAB_COUNT))?;
    if tab_count.is_some_and(|n| n.fract() != 0.0) {
        return Err("cut_options.tab_count 必须是整数".into());
    }
    let tab_spacing = number("tab_spacing_mm", 0.0, MAX_TAB_SPACING_MM)?;
    let tabs = match (
        tab_count.filter(|n| *n > 0.0),
        tab_spacing.filter(|s| *s > 0.0),
    ) {
        (Some(_), Some(_)) => {
            return Err("cut_options.tab_count 与 tab_spacing_mm 只能填一个".into());
        }
        (Some(n), None) => TabMode::Count(n as u32),
        (None, Some(spacing)) if spacing < MIN_TAB_SPACING_MM => {
            return Err(format!(
                "cut_options.tab_spacing_mm 须在 {MIN_TAB_SPACING_MM} ~ {MAX_TAB_SPACING_MM} 之间"
            ));
        }
        (None, Some(spacing)) => TabMode::Spacing(spacing),
        (None, None) => TabMode::Off,
    };
    let skip_inner = match cut.get("tabs_skip_inner") {
        None | Some(serde_json::Value::Null) => defaults.tabs_skip_inner,
        Some(serde_json::Value::Bool(value)) => *value,
        Some(_) => return Err("cut_options.tabs_skip_inner 必须是 true / false".into()),
    };
    let options = CutOptions {
        kerf_mm: number("kerf_mm", -MAX_KERF_MM, MAX_KERF_MM)?.unwrap_or(0.0),
        tabs,
        tab_width_mm: number("tab_width_mm", MIN_TAB_WIDTH_MM, MAX_TAB_WIDTH_MM)?
            .unwrap_or(defaults.tab_width_mm),
        tabs_skip_inner: skip_inner,
        lead_in_mm: number("lead_in_mm", 0.0, MAX_LEAD_MM)?.unwrap_or(0.0),
        lead_out_mm: number("lead_out_mm", 0.0, MAX_LEAD_MM)?.unwrap_or(0.0),
        lead_angle_deg: number("lead_angle_deg", 1.0, 90.0)?.unwrap_or(defaults.lead_angle_deg),
        overcut_mm: number("overcut_mm", 0.0, MAX_OVERCUT_MM)?.unwrap_or(0.0),
    };
    options.validate().map_err(|e| e.to_string())?;
    Ok(options)
}

/// 结果里回显实际使用的切割设置与统计（`note` 提示图形消失 / 相连、放不下桥位、省略的引线）。
pub(super) fn cut_options_json(
    options: &laser_core::cut::CutOptions,
    report: Option<&laser_core::cut::CutReport>,
    from_material: &[String],
) -> serde_json::Value {
    use laser_core::cut::TabMode;
    let (tab_mode, tab_count, tab_spacing) = match options.tabs {
        TabMode::Off => ("off", None, None),
        TabMode::Count(n) => ("count", Some(n), None),
        TabMode::Spacing(s) => ("spacing", None, Some(s)),
    };
    let report = report.cloned().unwrap_or_default();
    let mut notes = Vec::new();
    if let Some((before, after)) = report.kerf_contours
        && after != before
    {
        notes.push(format!(
            "切缝补偿后轮廓数从 {before} 变为 {after}（有图形消失或相连），请检查预览"
        ));
    }
    if report.contours_without_tabs > 0 {
        notes.push(format!(
            "{} 个图形太小，放不下桥位",
            report.contours_without_tabs
        ));
    }
    if report.leads_skipped > 0 {
        notes.push(format!(
            "{} 条引线会碰到别的轮廓或落到零件一侧，已省略",
            report.leads_skipped
        ));
    }
    serde_json::json!({
        "active": options.is_active(),
        "kerf_mm": options.kerf_mm,
        "tab_mode": tab_mode,
        "tab_count": tab_count,
        "tab_spacing_mm": tab_spacing,
        "tab_width_mm": options.tab_width_mm,
        "tabs_skip_inner": options.tabs_skip_inner,
        "lead_in_mm": options.lead_in_mm,
        "lead_out_mm": options.lead_out_mm,
        "lead_angle_deg": options.lead_angle_deg,
        "overcut_mm": options.overcut_mm,
        "report": {
            "contours": report.contours,
            "kerf_contours_before": report.kerf_contours.map(|(before, _)| before),
            "kerf_contours_after": report.kerf_contours.map(|(_, after)| after),
            "tabs": report.tabs,
            "tab_gap_mm": report.tab_gap_mm,
            "contours_without_tabs": report.contours_without_tabs,
            "lead_ins": report.lead_ins,
            "lead_outs": report.lead_outs,
            "leads_skipped": report.leads_skipped,
            "overcut_mm": report.overcut_mm,
        },
        "note": notes.join("；"),
        "from_material": from_material,
    })
}

/// 解析 `path_options`（对齐 LightBurn 优化设置）：`prefer_corners`（起刀点放尖角，默认 true）、
/// `remove_overlaps`（去重复线，默认 true）。切割先内后外由刻法决定，不在这里开关。
pub(super) fn parse_path_options(
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
pub(super) fn fill_options_json(
    options: &laser_core::vector_plan::FillOptions,
    parsed: &ParsedFill,
) -> serde_json::Value {
    let table = parsed.offset_table.as_ref().map(|rows| {
        rows.iter()
            .map(|&(speed, offset)| [speed, offset])
            .collect::<Vec<_>>()
    });
    let overscan_auto = parsed.auto;
    serde_json::json!({
        "spacing_mm": options.spacing,
        "grouping": options.grouping.as_str(),
        "angle_deg": options.angle_deg,
        "cross_hatch": options.cross_hatch,
        "bidirectional": options.bidirectional,
        "overscan_mm": options.overscan_mm,
        "overscan_auto": overscan_auto,
        "scan_offset_mm": options.scan_offset_mm,
        "scan_offset_source": if table.is_some() { "table" } else { "fixed" },
        "scan_offset_origin": parsed.scan_offset_origin,
        "scan_offset_table": table,
        "from_material": parsed.from_material,
    })
}
