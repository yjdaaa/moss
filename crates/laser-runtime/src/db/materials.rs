//! 材料库：删除、导出、整库替换与 JSON 导入（含冲突策略）。

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use laser_contracts::{EngraveModes, MaterialEntry, MaterialLibrary, ParamCell, ThicknessEntry};

use super::{RuntimeDb, SchemaError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialConflictPolicy {
    Keep,
    Overwrite,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterialImportCounts {
    pub added: u64,
    pub skipped: u64,
    pub conflicts: u64,
    pub tombstones: u64,
}

pub fn parse_param_cell_from_value(val: &Value) -> Option<ParamCell> {
    let obj = val.as_object()?;
    let get_i32 = |key: &str| -> Option<i32> {
        match obj.get(key)? {
            Value::Number(n) => n.as_i64().map(|v| v as i32),
            Value::String(s) => s.trim().parse::<f64>().ok().map(|v| v.trunc() as i32),
            _ => None,
        }
    };
    let get_f64 = |key: &str| -> Option<f64> {
        match obj.get(key)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    };

    let laser_min_power = get_i32("laser_min_power").unwrap_or(0);
    let laser_max_power = get_i32("laser_max_power")?;
    let feed_rate = get_i32("feed_rate")?;
    let travel_rate = get_i32("travel_rate").unwrap_or(3000);
    let threshold = get_i32("threshold").unwrap_or(-1);
    let passes = get_i32("passes").unwrap_or(1);
    let pixel_size_mm = get_f64("pixel_size_mm").unwrap_or(0.1);

    let source = obj.get("source").and_then(Value::as_str).map(str::to_owned);
    let notes = obj.get("notes").and_then(Value::as_str).map(str::to_owned);
    let updated_at = get_f64("updated_at");

    let mut extra = BTreeMap::new();
    for (k, v) in obj {
        if !matches!(
            k.as_str(),
            "laser_min_power"
                | "laser_max_power"
                | "feed_rate"
                | "travel_rate"
                | "threshold"
                | "passes"
                | "pixel_size_mm"
                | "source"
                | "notes"
                | "updated_at"
        ) {
            extra.insert(k.clone(), v.clone());
        }
    }

    Some(ParamCell {
        laser_min_power,
        laser_max_power,
        feed_rate,
        travel_rate,
        threshold,
        passes,
        pixel_size_mm,
        power_scale: Some(1000),
        source,
        notes,
        updated_at,
        extra,
    })
}

impl RuntimeDb {
    /// 删除指定材料及其全部厚度参数与别名（单事务原子提交）。
    pub fn delete_material(&self, material_name: &str) -> Result<(), SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM material_params WHERE material_name=?1",
            params![material_name],
        )?;
        tx.execute(
            "DELETE FROM material_aliases WHERE material_name=?1",
            params![material_name],
        )?;
        tx.execute(
            "DELETE FROM materials WHERE name=?1",
            params![material_name],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 删除指定材料下的一组厚度参数（单事务原子提交）。
    pub fn delete_material_thicknesses(
        &self,
        material_name: &str,
        thickness_keys: &[String],
    ) -> Result<(), SchemaError> {
        if thickness_keys.is_empty() {
            return Ok(());
        }
        let tx = self.connection.unchecked_transaction()?;
        for key in thickness_keys {
            tx.execute(
                "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2",
                params![material_name, key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn export_material_library(&self) -> Result<MaterialLibrary, SchemaError> {
        let mut library = MaterialLibrary {
            version: 1,
            power_scale: 1000,
            materials: std::collections::BTreeMap::new(),
        };
        let names = {
            let mut stmt = self
                .connection
                .prepare_cached("SELECT name FROM materials ORDER BY rowid")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for name in names {
            library.materials.insert(
                name,
                MaterialEntry {
                    aliases: Vec::new(),
                    thicknesses: std::collections::BTreeMap::new(),
                },
            );
        }
        if library.materials.is_empty() {
            return Ok(library);
        }
        {
            let mut stmt = self.connection.prepare_cached(
                "SELECT material_name, alias FROM material_aliases ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (material_name, alias) = row?;
                if let Some(entry) = library.materials.get_mut(&material_name) {
                    entry.aliases.push(alias);
                }
            }
        }
        {
            let mut stmt = self.connection.prepare_cached(
                "SELECT material_name, thickness_key, laser_mode, engraving_mode, laser_min_power,
                 laser_max_power, feed_rate, travel_rate, threshold, passes, pixel_size_mm,
                 source, updated_at, notes, extra
                 FROM material_params ORDER BY id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    ParamCell {
                        laser_min_power: row.get(4)?,
                        laser_max_power: row.get(5)?,
                        feed_rate: row.get(6)?,
                        travel_rate: row.get(7)?,
                        threshold: row.get(8)?,
                        passes: row.get(9)?,
                        pixel_size_mm: row.get(10)?,
                        power_scale: Some(1000),
                        source: row.get(11)?,
                        updated_at: row.get(12)?,
                        notes: row.get(13)?,
                        extra: serde_json::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
                    },
                ))
            })?;
            for row in rows {
                let (material_name, thickness, mode, engraving, cell) = row?;
                let Some(mat_entry) = library.materials.get_mut(&material_name) else {
                    continue;
                };
                let entry = mat_entry
                    .thicknesses
                    .entry(thickness)
                    .or_insert_with(ThicknessEntry::default);
                if mode == "cut" {
                    entry.cut = Some(cell);
                } else {
                    let engrave = entry.engrave.get_or_insert_with(EngraveModes::default);
                    if engraving == "outline" {
                        engrave.outline = Some(cell);
                    } else {
                        engrave.raster = Some(cell);
                    }
                }
            }
        }
        Ok(library)
    }

    pub fn export_material_json(&self) -> Result<Value, SchemaError> {
        Ok(serde_json::to_value(self.export_material_library()?)?)
    }

    pub fn replace_material_json(
        &self,
        value: &Value,
        now: f64,
    ) -> Result<MaterialImportCounts, SchemaError> {
        self.import_material_json_inner(value, MaterialConflictPolicy::Overwrite, now, true)
    }

    pub fn import_material_json(
        &self,
        value: &Value,
        policy: MaterialConflictPolicy,
        now: f64,
    ) -> Result<MaterialImportCounts, SchemaError> {
        self.import_material_json_inner(value, policy, now, false)
    }

    fn import_material_json_inner(
        &self,
        value: &Value,
        policy: MaterialConflictPolicy,
        now: f64,
        clear_existing: bool,
    ) -> Result<MaterialImportCounts, SchemaError> {
        let mut counts = MaterialImportCounts::default();
        let Some(materials) = value.get("materials").and_then(Value::as_object) else {
            return Ok(counts);
        };

        let tx = self.connection.unchecked_transaction()?;
        if clear_existing {
            tx.execute_batch(
                "DELETE FROM material_params; DELETE FROM material_aliases; DELETE FROM materials;",
            )?;
        }
        for (name, mat_val) in materials {
            tx.prepare_cached(
                "INSERT OR IGNORE INTO materials(name, created_at, updated_at) VALUES(?1, ?2, ?2)",
            )?
            .execute(params![name, now])?;

            if let Some(aliases) = mat_val.get("aliases").and_then(Value::as_array) {
                if policy == MaterialConflictPolicy::Overwrite {
                    tx.prepare_cached("DELETE FROM material_aliases WHERE material_name=?1")?
                        .execute(params![name])?;
                }
                let mut ins_alias = tx.prepare_cached(
                    "INSERT OR IGNORE INTO material_aliases(alias, material_name) VALUES(?1, ?2)",
                )?;
                for alias in aliases {
                    if let Some(alias_str) = alias.as_str() {
                        let trimmed = alias_str.trim();
                        if !trimmed.is_empty() {
                            ins_alias.execute(params![trimmed, name])?;
                        }
                    }
                }
            }

            if let Some(thicknesses) = mat_val.get("thicknesses").and_then(Value::as_object) {
                for (thickness, thick_val) in thicknesses {
                    if thick_val.is_null() || thick_val.as_object().is_some_and(|o| o.is_empty()) {
                        let deleted = tx.execute(
                            "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2",
                            params![name, thickness],
                        )?;
                        if deleted > 0 {
                            counts.tombstones += deleted as u64;
                        }
                        continue;
                    }

                    if let Some(thick_obj) = thick_val.as_object() {
                        // Handle engrave
                        if let Some(engrave_val) = thick_obj.get("engrave") {
                            if engrave_val.is_null()
                                || engrave_val.as_object().is_some_and(|o| o.is_empty())
                            {
                                let deleted = tx.execute(
                                    "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode='engrave'",
                                    params![name, thickness],
                                )?;
                                if deleted > 0 {
                                    counts.tombstones += deleted as u64;
                                }
                            } else if let Some(engrave_obj) = engrave_val.as_object() {
                                if engrave_obj.contains_key("raster")
                                    || engrave_obj.contains_key("outline")
                                {
                                    for mode in ["raster", "outline"] {
                                        if let Some(cell_val) = engrave_obj.get(mode) {
                                            if cell_val.is_null()
                                                || cell_val
                                                    .as_object()
                                                    .is_some_and(|o| o.is_empty())
                                            {
                                                let deleted = tx.execute(
                                                    "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode='engrave' AND engraving_mode=?3",
                                                    params![name, thickness, mode],
                                                )?;
                                                if deleted > 0 {
                                                    counts.tombstones += deleted as u64;
                                                }
                                            } else if let Some(cell) =
                                                parse_param_cell_from_value(cell_val)
                                            {
                                                self.upsert_cell_with_conflict(
                                                    &tx,
                                                    name,
                                                    thickness,
                                                    "engrave",
                                                    mode,
                                                    &cell,
                                                    policy,
                                                    now,
                                                    &mut counts,
                                                )?;
                                            }
                                        }
                                    }
                                } else if let Some(cell) = parse_param_cell_from_value(engrave_val)
                                {
                                    self.upsert_cell_with_conflict(
                                        &tx,
                                        name,
                                        thickness,
                                        "engrave",
                                        "raster",
                                        &cell,
                                        policy,
                                        now,
                                        &mut counts,
                                    )?;
                                }
                            }
                        }

                        // Handle cut
                        if let Some(cut_val) = thick_obj.get("cut") {
                            if cut_val.is_null()
                                || cut_val.as_object().is_some_and(|o| o.is_empty())
                            {
                                let deleted = tx.execute(
                                    "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode='cut'",
                                    params![name, thickness],
                                )?;
                                if deleted > 0 {
                                    counts.tombstones += deleted as u64;
                                }
                            } else if let Some(cell) = parse_param_cell_from_value(cut_val) {
                                self.upsert_cell_with_conflict(
                                    &tx,
                                    name,
                                    thickness,
                                    "cut",
                                    "",
                                    &cell,
                                    policy,
                                    now,
                                    &mut counts,
                                )?;
                            }
                        }
                    }
                }
            }
        }
        tx.commit()?;
        Ok(counts)
    }

    fn upsert_cell_with_conflict(
        &self,
        tx: &rusqlite::Transaction<'_>,
        material: &str,
        thickness: &str,
        mode: &str,
        engraving: &str,
        cell: &ParamCell,
        policy: MaterialConflictPolicy,
        now: f64,
        counts: &mut MaterialImportCounts,
    ) -> Result<(), SchemaError> {
        let existing: Option<(i64, i32, i32, i32, i32, i32, i32, f64, Option<String>, Option<String>)> = tx
            .prepare_cached(
                "SELECT id, laser_min_power, laser_max_power, feed_rate, travel_rate, threshold, passes, pixel_size_mm, source, notes
                 FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode=?3 AND engraving_mode=?4",
            )?
            .query_row(
                params![material, thickness, mode, engraving],
                |row| {
                    Ok((
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                        row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                    ))
                },
            )
            .optional()?;

        match existing {
            None => {
                let extra_json = serde_json::to_string(&cell.extra).unwrap_or_else(|_| "{}".into());
                tx.prepare_cached(
                    "INSERT INTO material_params(
                        material_name, thickness_key, laser_mode, engraving_mode,
                        laser_min_power, laser_max_power, feed_rate, travel_rate,
                        threshold, passes, pixel_size_mm, source, updated_at, notes, extra
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                )?
                .execute(params![
                    material,
                    thickness,
                    mode,
                    engraving,
                    cell.laser_min_power,
                    cell.laser_max_power,
                    cell.feed_rate,
                    cell.travel_rate,
                    cell.threshold,
                    cell.passes,
                    cell.pixel_size_mm,
                    cell.source,
                    cell.updated_at.or(Some(now)),
                    cell.notes,
                    extra_json
                ])?;
                tx.prepare_cached("UPDATE materials SET updated_at=?2 WHERE name=?1")?
                    .execute(params![material, now])?;
                counts.added += 1;
            }
            Some((id, min_p, max_p, feed, travel, thresh, passes, px_size, src, nts)) => {
                let matches = min_p == cell.laser_min_power
                    && max_p == cell.laser_max_power
                    && feed == cell.feed_rate
                    && travel == cell.travel_rate
                    && thresh == cell.threshold
                    && passes == cell.passes
                    && (px_size - cell.pixel_size_mm).abs() < 1e-6
                    && src.as_deref() == cell.source.as_deref()
                    && nts.as_deref() == cell.notes.as_deref();

                if matches {
                    counts.skipped += 1;
                } else {
                    counts.conflicts += 1;
                    if policy == MaterialConflictPolicy::Overwrite {
                        tx.execute(
                            "INSERT INTO material_params_history(
                                id, material_name, thickness_key, laser_mode, engraving_mode,
                                laser_min_power, laser_max_power, feed_rate, travel_rate,
                                threshold, passes, pixel_size_mm, source, updated_at, notes, extra,
                                replaced_at, reason
                             ) SELECT id, material_name, thickness_key, laser_mode, engraving_mode,
                                      laser_min_power, laser_max_power, feed_rate, travel_rate,
                                      threshold, passes, pixel_size_mm, source, updated_at, notes, extra,
                                      ?1, 'import_overwrite'
                               FROM material_params WHERE id=?2",
                            params![now, id],
                        )?;

                        let extra_json =
                            serde_json::to_string(&cell.extra).unwrap_or_else(|_| "{}".into());
                        tx.execute(
                            "UPDATE material_params SET
                                laser_min_power=?1, laser_max_power=?2, feed_rate=?3, travel_rate=?4,
                                threshold=?5, passes=?6, pixel_size_mm=?7, source=?8, updated_at=?9,
                                notes=?10, extra=?11
                             WHERE id=?12",
                            params![
                                cell.laser_min_power, cell.laser_max_power, cell.feed_rate, cell.travel_rate,
                                cell.threshold, cell.passes, cell.pixel_size_mm, cell.source, cell.updated_at.or(Some(now)),
                                cell.notes, extra_json, id
                            ],
                        )?;
                        tx.execute(
                            "UPDATE materials SET updated_at=?2 WHERE name=?1",
                            params![material, now],
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}
