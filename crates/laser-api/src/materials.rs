use super::*;

impl ApiService {
    pub(crate) fn effective_material_library(
        &self,
    ) -> Result<laser_contracts::MaterialLibrary, String> {
        load_effective_material_library(
            &self.settings,
            self.read_db.as_ref().or(self.runtime_db.as_ref()),
            &self.materials,
        )
    }
}

fn material_library_path(settings: &laser_runtime::LaserSettings) -> Option<std::path::PathBuf> {
    settings
        .material_params_file
        .as_deref()
        .map(std::path::PathBuf::from)
}

pub(crate) fn load_effective_material_library(
    settings: &laser_runtime::LaserSettings,
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    fallback: &laser_contracts::MaterialLibrary,
) -> Result<laser_contracts::MaterialLibrary, String> {
    if let Some(path) = material_library_path(settings) {
        match std::fs::metadata(&path) {
            Ok(_) => {
                let raw = std::fs::read_to_string(&path)
                    .map_err(|error| format!("读取材料库失败: {error}"))?;
                return serde_json::from_str(&raw)
                    .map_err(|error| format!("材料库解析失败: {error}"));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("检查材料库路径失败: {err}")),
        }
    }
    if let Some(db_arc) = db {
        let guard = db_arc
            .lock()
            .map_err(|_| "材料库数据库锁失败".to_string())?;
        return guard
            .export_material_library()
            .map_err(|error| format!("读取材料库失败: {error:?}"));
    }
    Ok(fallback.clone())
}

pub(crate) fn validate_material_safety(material: &str) -> Result<(), String> {
    match laser_core::safety::evaluate_material(material) {
        laser_core::safety::MaterialSafety::Allow => Ok(()),
        laser_core::safety::MaterialSafety::Block { .. } => {
            Err("材料安全门禁拒绝：PVC/聚氯乙烯及明确危险材料禁止激光加工".into())
        }
        laser_core::safety::MaterialSafety::Clarify { .. } => {
            Err("材料成分不明确：请先确认材料不是塑料或其他危险材料".into())
        }
    }
}

pub(crate) fn material_from_payload(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("material")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            payload
                .get("input")
                .and_then(|v| v.get("material"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            payload
                .get("confirmation_snapshot")
                .and_then(|v| v.get("material"))
                .and_then(serde_json::Value::as_str)
        })
        .filter(|value| !value.trim().is_empty())
}

pub(crate) fn validate_material_safety_in_payload(
    payload: &serde_json::Value,
) -> Result<(), String> {
    if let Some(material) = material_from_payload(payload) {
        validate_material_safety(material)?;
    }
    Ok(())
}

pub(crate) fn material_library_export_value(
    library: &laser_contracts::MaterialLibrary,
) -> serde_json::Value {
    fn strip_default_power_scale(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("power_scale") == Some(&serde_json::json!(1000)) {
                    object.remove("power_scale");
                }
                for child in object.values_mut() {
                    strip_default_power_scale(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    strip_default_power_scale(item);
                }
            }
            _ => {}
        }
    }

    let mut value = serde_json::to_value(library).expect("material library must serialize");
    strip_default_power_scale(&mut value);
    value
}

fn material_export_response(
    settings: &laser_runtime::LaserSettings,
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    fallback: &laser_contracts::MaterialLibrary,
) -> Vec<u8> {
    match load_effective_material_library(settings, db, fallback) {
        Ok(library) => http_json(
            200,
            success(serde_json::json!({
                "params_file": settings.material_params_file.as_deref().unwrap_or(""),
                "library": material_library_export_value(&library),
            })),
        ),
        Err(error) => http_json(
            500,
            failure(
                "读取材料库失败",
                Some(serde_json::json!({ "error": error })),
            ),
        ),
    }
}

fn normalize_and_validate_cell(
    cell: &mut laser_contracts::ParamCell,
    default_scale: u32,
    material: &str,
    thickness_key: &str,
    is_cut: bool,
) -> Result<(), String> {
    let cell_scale = cell.power_scale.unwrap_or(default_scale as i32);
    if !(1..=100_000).contains(&cell_scale) {
        return Err(format!(
            "材料 {material} ({thickness_key}mm) 的 power_scale 无效"
        ));
    }
    cell.power_scale = Some(cell_scale);
    if cell_scale != 1000 {
        *cell = cell.rescaled_to(1000.0);
    }
    validate_entry_title(cell, material, thickness_key)?;
    validate_layer_options(cell, is_cut, material, thickness_key)?;
    cell.validate(1000, 100)
        .map_err(|err| format!("材料 {material} ({thickness_key}mm) 参数超出允许范围: {err}"))
}

/// 条目标题（`extra.title`）只能是不超过 60 字的字符串；空串视为没有标题。
fn validate_entry_title(
    cell: &mut laser_contracts::ParamCell,
    material: &str,
    thickness_key: &str,
) -> Result<(), String> {
    match cell.extra.get("title") {
        None | Some(serde_json::Value::Null) => {
            cell.extra.remove("title");
            Ok(())
        }
        Some(serde_json::Value::String(raw)) => {
            let trimmed = raw.trim().to_owned();
            if trimmed.chars().count() > laser_contracts::MATERIAL_ENTRY_TITLE_MAX_CHARS {
                return Err(format!(
                    "材料 {material} ({thickness_key}) 的条目标题不能超过 {} 个字",
                    laser_contracts::MATERIAL_ENTRY_TITLE_MAX_CHARS
                ));
            }
            cell.set_title(Some(&trimmed));
            Ok(())
        }
        Some(_) => Err(format!(
            "材料 {material} ({thickness_key}) 的条目标题必须是文字"
        )),
    }
}

/// 随条目保存的图层设置（r40，2026-11-16）：雕刻条目只能带 `fill_options`、切割条目只能带 `cut_options`；
/// 只收 `MATERIAL_FILL_OPTION_KEYS` / `MATERIAL_CUT_OPTION_KEYS` 里的键（扫描偏移与加速度跟机器走，
/// 不存进材料库），值的范围与生成请求相同（复用同一个解析函数）。null 值去掉，空对象等于没有。
pub(crate) fn validate_layer_options(
    cell: &mut laser_contracts::ParamCell,
    is_cut: bool,
    material: &str,
    thickness_key: &str,
) -> Result<(), String> {
    use laser_contracts::{
        MATERIAL_CUT_OPTION_KEYS, MATERIAL_CUT_OPTIONS_KEY, MATERIAL_FILL_OPTION_KEYS,
        MATERIAL_FILL_OPTIONS_KEY,
    };
    for (key, allowed, label, allowed_here) in [
        (
            MATERIAL_FILL_OPTIONS_KEY,
            MATERIAL_FILL_OPTION_KEYS,
            "填充设置",
            !is_cut,
        ),
        (
            MATERIAL_CUT_OPTIONS_KEY,
            MATERIAL_CUT_OPTION_KEYS,
            "切割设置",
            is_cut,
        ),
    ] {
        let map = match cell.extra.remove(key) {
            None | Some(serde_json::Value::Null) => continue,
            Some(serde_json::Value::Object(map)) => map,
            Some(_) => {
                return Err(format!(
                    "材料 {material} ({thickness_key}) 的{label}（{key}）必须是对象"
                ));
            }
        };
        let map: serde_json::Map<String, serde_json::Value> =
            map.into_iter().filter(|(_, v)| !v.is_null()).collect();
        if map.is_empty() {
            continue;
        }
        if !allowed_here {
            return Err(format!(
                "材料 {material} ({thickness_key}) 的{label}只能存在{}条目里",
                if is_cut { "雕刻" } else { "切割" }
            ));
        }
        if let Some(bad) = map.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!(
                "材料 {material} ({thickness_key}) 的{label}不能包含 `{bad}`（可存：{}；双向扫描偏移与机器加速度跟机器走，不存进材料库）",
                allowed.join("、")
            ));
        }
        let value = serde_json::Value::Object(map);
        let checked = if is_cut {
            parse_cut_options(Some(&value)).map(|_| ())
        } else {
            parse_fill_options(Some(&value), 1000.0, 1000.0).map(|_| ())
        };
        checked.map_err(|e| format!("材料 {material} ({thickness_key}) 的{label}无效：{e}"))?;
        if let serde_json::Value::Object(map) = value {
            cell.set_layer_options(key, Some(map));
        }
    }
    Ok(())
}

/// 请求是否指向“表面通用”条目：`surface=true`，或 `thickness_mm` 直接写 `"surface"`。
pub(crate) fn is_surface_request(object: &serde_json::Map<String, serde_json::Value>) -> bool {
    parse_bool(object.get("surface"), false)
        || object
            .get("thickness_mm")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|v| v.trim() == laser_contracts::SURFACE_THICKNESS_KEY)
}

/// 厚度键：大于 0 的数字，或“表面通用”键 `surface`（只能存雕刻参数）。
fn validate_thickness_key(
    material: &str,
    key: &str,
    entry: &laser_contracts::ThicknessEntry,
) -> Result<Option<f64>, String> {
    if key == laser_contracts::SURFACE_THICKNESS_KEY {
        if entry.cut.is_some() {
            return Err(format!(
                "材料 {material} 的表面通用条目只能存雕刻参数，切割必须按真实厚度保存"
            ));
        }
        return Ok(None);
    }
    match key.trim().parse::<f64>() {
        Ok(value) if value.is_finite() && value > 0.0 => Ok(Some(value)),
        _ => Err(format!(
            "材料 {material} 的厚度 `{key}` 必须是大于 0 的数字（或 surface 表示表面通用条目）"
        )),
    }
}

fn normalize_and_validate_library(
    library: &mut laser_contracts::MaterialLibrary,
) -> Result<(), String> {
    if !(1..=100_000).contains(&library.power_scale) {
        return Err("材料库 power_scale 必须是 1..100000 的正整数".into());
    }
    let lib_scale = library.power_scale;
    for (name, entry) in &mut library.materials {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("材料名称不能为空".into());
        }
        validate_material_safety(trimmed)?;
        entry.aliases.retain(|alias| !alias.trim().is_empty());
        for alias in &mut entry.aliases {
            *alias = alias.trim().to_owned();
            validate_material_safety(alias)?;
        }
        for (thick_key, thick_entry) in &mut entry.thicknesses {
            validate_thickness_key(trimmed, thick_key, thick_entry)?;
            if let Some(cut) = &mut thick_entry.cut {
                normalize_and_validate_cell(cut, lib_scale, trimmed, thick_key, true)?;
            }
            if let Some(engrave) = &mut thick_entry.engrave {
                if let Some(raster) = &mut engrave.raster {
                    normalize_and_validate_cell(raster, lib_scale, trimmed, thick_key, false)?;
                }
                if let Some(outline) = &mut engrave.outline {
                    normalize_and_validate_cell(outline, lib_scale, trimmed, thick_key, false)?;
                }
            }
        }
    }
    library.power_scale = 1000;
    Ok(())
}

fn merge_material_library(
    target: &mut laser_contracts::MaterialLibrary,
    incoming: laser_contracts::MaterialLibrary,
) {
    for (name, src_entry) in incoming.materials {
        let dst_entry = target.materials.entry(name).or_default();
        for alias in src_entry.aliases {
            if !dst_entry
                .aliases
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&alias))
            {
                dst_entry.aliases.push(alias);
            }
        }
        for (thick_key, src_thick) in src_entry.thicknesses {
            let dst_thick = dst_entry.thicknesses.entry(thick_key).or_default();
            if let Some(cut) = src_thick.cut {
                dst_thick.cut = Some(cut);
            }
            if let Some(engrave) = src_thick.engrave {
                let dst_engrave = dst_thick.engrave.get_or_insert_with(Default::default);
                if let Some(raster) = engrave.raster {
                    dst_engrave.raster = Some(raster);
                }
                if let Some(outline) = engrave.outline {
                    dst_engrave.outline = Some(outline);
                }
            }
        }
    }
}

fn strict_import_library(
    value: Option<&serde_json::Value>,
) -> Result<laser_contracts::MaterialLibrary, String> {
    let Some(raw_value) = value else {
        return Err("导入材料库格式不支持：需要 version=1 的 JSON 对象".into());
    };
    let value = if raw_value.get("version").is_none()
        && let Some(inner) = raw_value.get("library").filter(|v| v.is_object())
    {
        inner
    } else {
        raw_value
    };
    let valid_version = value
        .get("version")
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|version| version == 1.0);
    if !valid_version || !value.is_object() {
        return Err("导入材料库格式不支持：需要 version=1 的 JSON 对象".into());
    }
    let mut library: laser_contracts::MaterialLibrary = serde_json::from_value(value.clone())
        .map_err(|_| "导入材料库格式不支持：需要 version=1 的 JSON 对象".to_owned())?;
    normalize_and_validate_library(&mut library)?;
    Ok(library)
}

fn write_material_library_temp(
    settings: &laser_runtime::LaserSettings,
    library: &laser_contracts::MaterialLibrary,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let path = material_library_path(settings).ok_or("材料库文件路径未配置")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(library).map_err(|error| error.to_string())?;
    std::fs::write(&temp, bytes).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        error.to_string()
    })?;
    Ok((path, temp))
}

fn finalize_material_library_mirror(
    path: &std::path::Path,
    temp: &std::path::Path,
) -> Result<String, String> {
    let backup = path.with_extension("json.bak");
    let had_existing = match std::fs::metadata(path) {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            let _ = std::fs::remove_file(temp);
            return Err(err.to_string());
        }
    };
    if had_existing && let Err(error) = std::fs::copy(path, &backup) {
        let _ = std::fs::remove_file(temp);
        return Err(error.to_string());
    }
    if let Err(error) = std::fs::rename(temp, path) {
        let _ = std::fs::remove_file(temp);
        return Err(error.to_string());
    }
    Ok(backup
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_owned())
}

fn number_i32(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: i32,
) -> Result<i32, String> {
    parse_int(object.get(key), key, default as i64)
        .map(|value| value as i32)
        .map_err(|error| error.to_string())
}

fn sync_library_to_db(
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    library: &laser_contracts::MaterialLibrary,
) -> Result<(), String> {
    let Some(db_arc) = db else {
        return Ok(());
    };
    let val = serde_json::to_value(library).map_err(|error| error.to_string())?;
    let db_guard = db_arc
        .lock()
        .map_err(|_| "材料库数据库锁失败".to_string())?;
    db_guard
        .import_material_json(
            &val,
            laser_runtime::MaterialConflictPolicy::Overwrite,
            unix_seconds() as f64,
        )
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

fn replace_library_in_db(
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    library: &laser_contracts::MaterialLibrary,
) -> Result<(), String> {
    let Some(db_arc) = db else {
        return Ok(());
    };
    let val = serde_json::to_value(library).map_err(|error| error.to_string())?;
    let db_guard = db_arc
        .lock()
        .map_err(|_| "材料库数据库锁失败".to_string())?;
    db_guard
        .replace_material_json(&val, unix_seconds() as f64)
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

/// 先将新材料库写入同目录临时文件验证可写且不破坏正式镜像，随后提交 SQLite 单事务，最后原子改名替换正式镜像。
fn persist_material_library(
    settings: &laser_runtime::LaserSettings,
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    library: &laser_contracts::MaterialLibrary,
    commit_db: impl FnOnce() -> Result<(), String>,
) -> Result<String, String> {
    let staged = if material_library_path(settings).is_some() || db.is_none() {
        Some(write_material_library_temp(settings, library)?)
    } else {
        None
    };
    if let Err(error) = commit_db() {
        if let Some((_, temp)) = staged {
            let _ = std::fs::remove_file(temp);
        }
        return Err(error);
    }
    match staged {
        Some((path, temp)) => finalize_material_library_mirror(&path, &temp),
        None => Ok(String::new()),
    }
}

pub(crate) fn handle_material_manage(
    settings: &laser_runtime::LaserSettings,
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    fallback: &laser_contracts::MaterialLibrary,
    write_lock: &std::sync::Mutex<()>,
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<u8> {
    let action = object
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let _serial_guard = write_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut library = match load_effective_material_library(settings, db, fallback) {
        Ok(library) => library,
        Err(error) => {
            return http_json(
                500,
                failure(
                    "读取材料库失败",
                    Some(serde_json::json!({ "error": error })),
                ),
            );
        }
    };
    match action {
        "export" => material_export_response(settings, db, fallback),
        "import" => {
            let imported = object
                .get("library_json")
                .or_else(|| object.get("params_json"));
            let value = match imported {
                Some(serde_json::Value::String(raw)) => match serde_json::from_str(raw) {
                    Ok(value) => Some(value),
                    Err(_) => {
                        return http_json(
                            200,
                            failure(
                                "library_json 不是合法 JSON: Expecting value: line 1 column 1 (char 0)",
                                None,
                            ),
                        );
                    }
                },
                Some(serde_json::Value::Object(map)) if map.is_empty() => {
                    return http_json(
                        200,
                        failure(
                            "library_json 不是合法 JSON: Expecting value: line 1 column 1 (char 0)",
                            None,
                        ),
                    );
                }
                Some(value) => Some(value.clone()),
                None => None,
            };
            let import_mode = object
                .get("import_mode")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("replace")
                .to_ascii_lowercase();
            if !matches!(import_mode.as_str(), "merge" | "replace") {
                return http_json(400, failure("import_mode 必须是 merge 或 replace", None));
            }
            let imported = match strict_import_library(value.as_ref()) {
                Ok(library) => library,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let is_merge = import_mode == "merge";
            if is_merge {
                merge_material_library(&mut library, imported);
            } else {
                library = imported;
            }
            let backup_path = match persist_material_library(settings, db, &library, || {
                if is_merge {
                    sync_library_to_db(db, &library)
                } else {
                    replace_library_in_db(db, &library)
                }
            }) {
                Ok(path) => path,
                Err(error) => {
                    return http_json(
                        500,
                        failure(
                            "保存材料库失败",
                            Some(serde_json::json!({ "error": error })),
                        ),
                    );
                }
            };
            http_json(
                200,
                success(serde_json::json!({
                    "library": library,
                    "materials": library,
                    "backup_path": backup_path,
                    "import_mode": import_mode,
                })),
            )
        }
        "delete" => {
            if !parse_bool(object.get("confirmed"), false) {
                return http_json(400, failure("删除当前参数需要 confirmed=true", None));
            }
            let material = object
                .get("material")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let surface = is_surface_request(object);
            let thickness = object.get("thickness_mm").and_then(|value| value.as_f64());
            let thickness_key = thickness.map(|value| value.to_string()).unwrap_or_default();
            let Some(entry) = library.materials.get_mut(material) else {
                return http_json(400, failure("材料不存在", None));
            };
            let mut removed_keys = Vec::new();
            let remove_whole_material = if surface {
                // 只删表面通用条目；不能因为没带数字厚度就误删整个材料
                if entry
                    .thicknesses
                    .remove(laser_contracts::SURFACE_THICKNESS_KEY)
                    .is_none()
                {
                    return http_json(400, failure("该材料没有表面通用条目", None));
                }
                removed_keys.push(laser_contracts::SURFACE_THICKNESS_KEY.to_owned());
                if entry.thicknesses.is_empty() {
                    library.materials.remove(material);
                    true
                } else {
                    false
                }
            } else if thickness.is_none() || thickness == Some(0.0) {
                library.materials.remove(material);
                true
            } else {
                let target_num = thickness.unwrap_or(0.0);
                let matched: Vec<String> = entry
                    .thicknesses
                    .keys()
                    .filter(|k| {
                        *k == &thickness_key
                            || k.trim()
                                .parse::<f64>()
                                .is_ok_and(|v| (v - target_num).abs() <= 1e-9)
                    })
                    .cloned()
                    .collect();
                if matched.is_empty() {
                    removed_keys.push(thickness_key);
                } else {
                    for k in matched {
                        entry.thicknesses.remove(&k);
                        removed_keys.push(k);
                    }
                }
                if entry.thicknesses.is_empty() {
                    library.materials.remove(material);
                    true
                } else {
                    false
                }
            };
            match persist_material_library(settings, db, &library, || {
                let Some(db_arc) = db else {
                    return Ok(());
                };
                let db_guard = db_arc
                    .lock()
                    .map_err(|_| "材料库数据库锁失败".to_string())?;
                let tx = db_guard
                    .connection
                    .unchecked_transaction()
                    .map_err(|e| format!("{e:?}"))?;
                if remove_whole_material {
                    tx.execute(
                        "DELETE FROM material_params WHERE material_name=?1",
                        rusqlite::params![material],
                    )
                    .map_err(|e| format!("{e:?}"))?;
                    tx.execute(
                        "DELETE FROM material_aliases WHERE material_name=?1",
                        rusqlite::params![material],
                    )
                    .map_err(|e| format!("{e:?}"))?;
                    tx.execute(
                        "DELETE FROM materials WHERE name=?1",
                        rusqlite::params![material],
                    )
                    .map_err(|e| format!("{e:?}"))?;
                } else {
                    for key in &removed_keys {
                        tx.execute(
                            "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2",
                            rusqlite::params![material, key],
                        )
                        .map_err(|e| format!("{e:?}"))?;
                    }
                }
                tx.commit().map_err(|e| format!("{e:?}"))?;
                Ok(())
            }) {
                Ok(_) => http_json(200, success(serde_json::json!({ "materials": library }))),
                Err(error) => http_json(
                    500,
                    failure(
                        "保存材料库失败",
                        Some(serde_json::json!({ "error": error })),
                    ),
                ),
            }
        }
        "save" => {
            let material = object
                .get("material")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            if material.is_empty() {
                return http_json(400, failure("material 不能为空", None));
            }
            if let Err(error) = validate_material_safety(material) {
                return http_json(400, failure(error, None));
            }
            let parsed_aliases: Option<Vec<String>> = if let Some(value) = object.get("aliases") {
                let list: Vec<String> = if let Some(values) = value.as_array() {
                    values
                        .iter()
                        .filter_map(|item| item.as_str().map(str::trim))
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                } else {
                    value
                        .as_str()
                        .unwrap_or("")
                        .split([',', '，'])
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect()
                };
                for alias in &list {
                    if let Err(error) = validate_material_safety(alias) {
                        return http_json(400, failure(error, None));
                    }
                }
                Some(list)
            } else {
                None
            };
            let prune_missing = parse_bool(object.get("prune_missing_thicknesses"), false);
            if let Some(thicknesses_val) = object.get("thicknesses").filter(|v| v.is_object()) {
                let mut parsed_thicknesses = match serde_json::from_value::<
                    std::collections::BTreeMap<String, laser_contracts::ThicknessEntry>,
                >(thicknesses_val.clone())
                {
                    Ok(map) if !map.is_empty() => map,
                    _ => return http_json(400, failure("thicknesses 格式无效或为空", None)),
                };
                let mut incoming_nums = Vec::with_capacity(parsed_thicknesses.len());
                for (k, v) in &mut parsed_thicknesses {
                    match validate_thickness_key(material, k, v) {
                        Ok(Some(num)) => incoming_nums.push(num),
                        Ok(None) => {}
                        Err(error) => return http_json(400, failure(error, None)),
                    }
                    if let Some(cut) = &mut v.cut
                        && let Err(err) = normalize_and_validate_cell(cut, 1000, material, k, true)
                    {
                        return http_json(400, failure(err, None));
                    }
                    if let Some(engrave) = &mut v.engrave {
                        if let Some(raster) = &mut engrave.raster
                            && let Err(err) =
                                normalize_and_validate_cell(raster, 1000, material, k, false)
                        {
                            return http_json(400, failure(err, None));
                        }
                        if let Some(outline) = &mut engrave.outline
                            && let Err(err) =
                                normalize_and_validate_cell(outline, 1000, material, k, false)
                        {
                            return http_json(400, failure(err, None));
                        }
                    }
                }
                let entry = library.materials.entry(material.to_owned()).or_default();
                if let Some(aliases) = parsed_aliases {
                    entry.aliases = aliases;
                }
                let mut pruned_keys = Vec::new();
                if prune_missing {
                    let stale: Vec<String> = entry
                        .thicknesses
                        .keys()
                        .filter(|existing_k| {
                            // 表面通用条目不属于厚度列表，编辑厚度规格时不随之删除
                            existing_k.as_str() != laser_contracts::SURFACE_THICKNESS_KEY
                                && !parsed_thicknesses.contains_key(*existing_k)
                                && !existing_k.trim().parse::<f64>().is_ok_and(|ev| {
                                    incoming_nums.iter().any(|nv| (ev - nv).abs() <= 1e-9)
                                })
                        })
                        .cloned()
                        .collect();
                    for k in stale {
                        entry.thicknesses.remove(&k);
                        pruned_keys.push(k);
                    }
                }
                for (k, v) in parsed_thicknesses {
                    let canonical_k = if entry.thicknesses.contains_key(&k) {
                        k
                    } else if let Ok(nv) = k.trim().parse::<f64>() {
                        entry
                            .thicknesses
                            .keys()
                            .find(|existing_k| {
                                existing_k
                                    .trim()
                                    .parse::<f64>()
                                    .is_ok_and(|ev| (ev - nv).abs() <= 1e-9)
                            })
                            .cloned()
                            .unwrap_or(k)
                    } else {
                        k
                    };
                    let target_thick = entry.thicknesses.entry(canonical_k).or_default();
                    if let Some(cut) = v.cut {
                        target_thick.cut = Some(cut);
                    }
                    if let Some(engrave) = v.engrave {
                        let target_engrave =
                            target_thick.engrave.get_or_insert_with(Default::default);
                        if let Some(raster) = engrave.raster {
                            target_engrave.raster = Some(raster);
                        }
                        if let Some(outline) = engrave.outline {
                            target_engrave.outline = Some(outline);
                        }
                    }
                }
                return match persist_material_library(settings, db, &library, || {
                    sync_library_to_db(db, &library)?;
                    if !pruned_keys.is_empty()
                        && let Some(db_arc) = db
                    {
                        let db_guard = db_arc
                            .lock()
                            .map_err(|_| "材料库数据库锁失败".to_string())?;
                        let tx = db_guard
                            .connection
                            .unchecked_transaction()
                            .map_err(|e| format!("{e:?}"))?;
                        for key in &pruned_keys {
                            tx.execute(
                                "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2",
                                rusqlite::params![material, key],
                            )
                            .map_err(|e| format!("{e:?}"))?;
                        }
                        tx.commit().map_err(|e| format!("{e:?}"))?;
                    }
                    Ok(())
                }) {
                    Ok(_) => http_json(200, success(serde_json::json!({ "materials": library }))),
                    Err(error) => http_json(
                        500,
                        failure(
                            "保存材料库失败",
                            Some(serde_json::json!({ "error": error })),
                        ),
                    ),
                };
            }
            let entry = library.materials.entry(material.to_owned()).or_default();
            if let Some(aliases) = parsed_aliases {
                entry.aliases = aliases;
            }
            let array_thicknesses: Vec<f64> = object
                .get("thicknesses")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            item.as_f64().or_else(|| {
                                item.as_str().and_then(|s| s.trim().parse::<f64>().ok())
                            })
                        })
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .collect()
                })
                .unwrap_or_default();
            let to_surface = is_surface_request(object);
            let target_thicknesses: Vec<String> = if to_surface {
                vec![laser_contracts::SURFACE_THICKNESS_KEY.to_owned()]
            } else if !array_thicknesses.is_empty() {
                array_thicknesses.iter().map(f64::to_string).collect()
            } else {
                let thickness = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0).ok();
                let Some(thickness) = thickness.filter(|value| value.is_finite() && *value > 0.0)
                else {
                    return http_json(400, failure("thickness_mm 必须是大于 0 的数字", None));
                };
                vec![thickness.to_string()]
            };
            let mode = object
                .get("laser_mode")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("engrave");
            let engraving = object
                .get("engraving_mode")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("raster");
            let cell = laser_contracts::ParamCell {
                laser_min_power: match number_i32(object, "laser_min_power", 0) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                laser_max_power: match number_i32(object, "laser_max_power", 800) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                feed_rate: match number_i32(object, "feed_rate", 1200) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                travel_rate: match number_i32(object, "travel_rate", 3000) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                threshold: match number_i32(object, "threshold", -1) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                passes: match number_i32(object, "passes", 1) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                pixel_size_mm: match parse_float(object.get("pixel_size_mm"), "pixel_size_mm", 0.1)
                {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                power_scale: object
                    .get("power_scale")
                    .and_then(|value| value.as_i64())
                    .map(|value| value as i32)
                    .or(Some(1000)),
                source: object
                    .get("source")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                notes: object
                    .get("notes")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                updated_at: Some(unix_seconds() as f64),
                extra: std::collections::BTreeMap::new(),
            };
            let mut cell = cell;
            match object.get("title") {
                None | Some(serde_json::Value::Null) => {}
                Some(serde_json::Value::String(title)) => cell.set_title(Some(title)),
                Some(_) => return http_json(400, failure("title 必须是文字", None)),
            }
            if let Err(error) = validate_entry_title(&mut cell, material, "-") {
                return http_json(400, failure(error, None));
            }
            if cell.validate(1000, 100).is_err() {
                return http_json(400, failure("材料参数超出允许范围", None));
            }
            if !(mode == "cut"
                || (mode == "engrave" && (engraving == "outline" || engraving == "raster")))
            {
                return http_json(400, failure("laser_mode 或 engraving_mode 无效", None));
            }
            if to_surface && mode == "cut" {
                return http_json(
                    400,
                    failure("表面通用条目只能存雕刻参数，切割必须按真实厚度保存", None),
                );
            }
            // 随条目保存的图层设置（r40）：请求带了 fill_options / cut_options 就用请求的（null 清除）；
            // 没带就沿用该位置原有条目里的，改功率、速度或选格写回不会把它们丢掉
            let layer_key = if mode == "cut" {
                laser_contracts::MATERIAL_CUT_OPTIONS_KEY
            } else {
                laser_contracts::MATERIAL_FILL_OPTIONS_KEY
            };
            for key in [
                laser_contracts::MATERIAL_FILL_OPTIONS_KEY,
                laser_contracts::MATERIAL_CUT_OPTIONS_KEY,
            ] {
                if let Some(value) = object.get(key).filter(|v| !v.is_null()) {
                    cell.extra.insert(key.to_owned(), value.clone());
                }
            }
            if let Err(error) = validate_layer_options(&mut cell, mode == "cut", material, "-") {
                return http_json(400, failure(error, None));
            }
            let keep_existing_layer = !object.contains_key(layer_key);
            for th in target_thicknesses {
                let thickness_entry = entry.thicknesses.entry(th).or_default();
                let slot = if mode == "cut" {
                    &mut thickness_entry.cut
                } else if engraving == "outline" {
                    &mut thickness_entry
                        .engrave
                        .get_or_insert_with(Default::default)
                        .outline
                } else {
                    &mut thickness_entry
                        .engrave
                        .get_or_insert_with(Default::default)
                        .raster
                };
                let mut next = cell.clone();
                if keep_existing_layer
                    && let Some(existing) = slot.as_ref().and_then(|c| c.layer_options(layer_key))
                {
                    next.set_layer_options(layer_key, Some(existing.clone()));
                }
                *slot = Some(next);
            }
            match persist_material_library(settings, db, &library, || {
                sync_library_to_db(db, &library)
            }) {
                Ok(_) => http_json(200, success(serde_json::json!({ "materials": library }))),
                Err(error) => http_json(
                    500,
                    failure(
                        "保存材料库失败",
                        Some(serde_json::json!({ "error": error })),
                    ),
                ),
            }
        }
        "save_layer_options" => save_layer_options(settings, db, &mut library, object),
        _ => http_json(
            400,
            failure(
                "action 必须是 save、save_layer_options、delete、import 或 export",
                None,
            ),
        ),
    }
}

/// `save_layer_options`（r40，2026-11-16）：只改已有条目随材料保存的填充 / 切割设置，不碰功率、速度等参数。
/// 雕刻条目收 `fill_options`，切割条目收 `cut_options`；`merge=true` 时把给出的键并进原有设置
/// （值为 null 的键删除），否则整体替换；对象为 null 或空等于清除。条目不存在时拒绝（先保存参数）。
fn save_layer_options(
    settings: &laser_runtime::LaserSettings,
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    library: &mut laser_contracts::MaterialLibrary,
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<u8> {
    let requested = object
        .get("material")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if requested.is_empty() {
        return http_json(400, failure("material 不能为空", None));
    }
    let laser_mode = object
        .get("laser_mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("engrave")
        .to_ascii_lowercase();
    let engraving = object
        .get("engraving_mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("raster")
        .to_ascii_lowercase();
    let is_cut = laser_mode == "cut";
    if !is_cut && !(laser_mode == "engrave" && matches!(engraving.as_str(), "raster" | "outline")) {
        return http_json(400, failure("laser_mode 或 engraving_mode 无效", None));
    }
    let (layer_key, other_key) = if is_cut {
        (
            laser_contracts::MATERIAL_CUT_OPTIONS_KEY,
            laser_contracts::MATERIAL_FILL_OPTIONS_KEY,
        )
    } else {
        (
            laser_contracts::MATERIAL_FILL_OPTIONS_KEY,
            laser_contracts::MATERIAL_CUT_OPTIONS_KEY,
        )
    };
    if object.get(other_key).is_some_and(|v| !v.is_null()) {
        return http_json(
            400,
            failure(
                if is_cut {
                    "切割条目只能存 cut_options"
                } else {
                    "雕刻条目只能存 fill_options"
                },
                None,
            ),
        );
    }
    let Some(incoming) = object.get(layer_key) else {
        return http_json(400, failure(format!("缺少 {layer_key}"), None));
    };
    let incoming = match incoming {
        serde_json::Value::Null => serde_json::Map::new(),
        serde_json::Value::Object(map) => map.clone(),
        _ => return http_json(400, failure(format!("{layer_key} 必须是对象"), None)),
    };
    let Some(material) = library
        .materials
        .iter()
        .find(|(name, entry)| {
            name.eq_ignore_ascii_case(requested)
                || entry
                    .aliases
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(requested))
        })
        .map(|(name, _)| name.clone())
    else {
        return http_json(400, failure("材料不存在", None));
    };
    let surface = is_surface_request(object);
    if surface && is_cut {
        return http_json(
            400,
            failure("表面通用条目只能存雕刻参数，切割必须按真实厚度保存", None),
        );
    }
    let entry = library
        .materials
        .get_mut(&material)
        .expect("material found above");
    let thickness_key = if surface {
        Some(laser_contracts::SURFACE_THICKNESS_KEY.to_owned())
    } else {
        let Some(target) = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0)
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
        else {
            return http_json(400, failure("thickness_mm 必须是大于 0 的数字", None));
        };
        entry
            .thicknesses
            .keys()
            .find(|k| {
                k.trim()
                    .parse::<f64>()
                    .is_ok_and(|v| (v - target).abs() <= 1e-9)
            })
            .cloned()
    };
    let slot_cell = thickness_key.as_ref().and_then(|key| {
        let modes = entry.thicknesses.get_mut(key)?;
        if is_cut {
            modes.cut.as_mut()
        } else if engraving == "outline" {
            modes.engrave.as_mut()?.outline.as_mut()
        } else {
            modes.engrave.as_mut()?.raster.as_mut()
        }
    });
    let Some(cell) = slot_cell else {
        return http_json(
            400,
            failure(
                "材料库里还没有这一档参数，请先在材料实验室保存功率、速度后再保存图层设置",
                Some(serde_json::json!({"error_code": "material_entry_missing"})),
            ),
        );
    };
    let thickness_key = thickness_key.unwrap_or_default();
    let mut next = cell.clone();
    let merged = if parse_bool(object.get("merge"), false) {
        let mut base = cell.layer_options(layer_key).cloned().unwrap_or_default();
        for (key, value) in incoming {
            if value.is_null() {
                base.remove(&key);
            } else {
                base.insert(key, value);
            }
        }
        base
    } else {
        incoming
    };
    next.extra
        .insert(layer_key.to_owned(), serde_json::Value::Object(merged));
    if let Err(error) = validate_layer_options(&mut next, is_cut, &material, &thickness_key) {
        return http_json(400, failure(error, None));
    }
    next.updated_at = Some(unix_seconds() as f64);
    let saved = next.layer_options(layer_key).cloned();
    *cell = next;
    match persist_material_library(settings, db, library, || sync_library_to_db(db, library)) {
        Ok(_) => http_json(
            200,
            success(serde_json::json!({
                "material": material,
                "thickness_key": thickness_key,
                "laser_mode": laser_mode,
                "engraving_mode": if is_cut { serde_json::Value::Null } else { serde_json::json!(engraving) },
                layer_key: saved,
                "materials": library,
            })),
        ),
        Err(error) => http_json(
            500,
            failure(
                "保存材料库失败",
                Some(serde_json::json!({ "error": error })),
            ),
        ),
    }
}
