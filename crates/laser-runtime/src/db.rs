use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use laser_contracts::{
    EngraveModes, MaterialEntry, MaterialLibrary, ParamCell, ThicknessEntry,
    builtin_material_library,
};

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

#[derive(Debug)]
pub enum SchemaError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    InvalidRecord(String),
    Busy,
}
impl From<rusqlite::Error> for SchemaError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<serde_json::Error> for SchemaError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

pub struct RuntimeDb {
    pub connection: Connection,
}

impl RuntimeDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SchemaError> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Self { connection };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self, SchemaError> {
        let connection = Connection::open_in_memory()?;
        let db = Self { connection };
        db.migrate()?;
        Ok(db)
    }

    pub fn migrate(&self) -> Result<(), SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version(version INTEGER PRIMARY KEY, applied_at REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS workflows(
               workflow_id TEXT PRIMARY KEY, status TEXT NOT NULL, source_type TEXT,
               material TEXT, thickness_mm REAL, laser_mode TEXT, connection_mode TEXT,
               gcode_path TEXT, gcode_sha256 TEXT, has_snapshot INTEGER NOT NULL DEFAULT 0,
               stale INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, updated_at REAL NOT NULL,
               payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS text_tasks(
               task_id TEXT PRIMARY KEY, status TEXT, text TEXT, material TEXT, thickness_mm REAL,
               laser_mode TEXT, attempt_count INTEGER NOT NULL DEFAULT 0, task_dir TEXT,
               stale INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, updated_at REAL NOT NULL,
               payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS jobs(
               job_id TEXT PRIMARY KEY, workflow_id TEXT, transport TEXT NOT NULL DEFAULT '',
               status TEXT NOT NULL, port TEXT, baudrate INTEGER, host TEXT,
               http_port INTEGER, telnet_port INTEGER, timeout REAL, gcode_path TEXT,
               expected_gcode_sha256 TEXT, process_id INTEGER, calibration_id TEXT,
               created_at REAL NOT NULL, started_at REAL, finished_at REAL,
               progress_lines INTEGER, progress_updated_at REAL, payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS calibrations(
               calibration_id TEXT PRIMARY KEY, status TEXT, material TEXT, thickness_mm REAL,
               laser_mode TEXT, engraving_mode TEXT, matrix_kind TEXT, gcode_path TEXT,
               job_id TEXT, selected_cell_number INTEGER, created_at REAL NOT NULL,
               updated_at REAL NOT NULL, payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS materials(name TEXT PRIMARY KEY, created_at REAL NOT NULL, updated_at REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS material_aliases(alias TEXT PRIMARY KEY, material_name TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS material_params(
               id INTEGER PRIMARY KEY, material_name TEXT NOT NULL, thickness_key TEXT NOT NULL,
               laser_mode TEXT NOT NULL, engraving_mode TEXT NOT NULL DEFAULT '',
               laser_min_power INTEGER, laser_max_power INTEGER, feed_rate INTEGER,
               travel_rate INTEGER, threshold INTEGER, passes INTEGER, pixel_size_mm REAL,
               source TEXT, updated_at REAL, notes TEXT, extra TEXT NOT NULL DEFAULT '{}',
               UNIQUE(material_name, thickness_key, laser_mode, engraving_mode)
             );
             CREATE TABLE IF NOT EXISTS material_params_history(
               id INTEGER NOT NULL, material_name TEXT NOT NULL, thickness_key TEXT NOT NULL,
               laser_mode TEXT NOT NULL, engraving_mode TEXT NOT NULL DEFAULT '',
               laser_min_power INTEGER, laser_max_power INTEGER, feed_rate INTEGER,
               travel_rate INTEGER, threshold INTEGER, passes INTEGER, pixel_size_mm REAL,
               source TEXT, updated_at REAL, notes TEXT, extra TEXT NOT NULL DEFAULT '{}',
               replaced_at REAL NOT NULL, reason TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS ai_state(id INTEGER PRIMARY KEY CHECK(id=1), payload TEXT NOT NULL, updated_at REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS gui_calibration(target TEXT PRIMARY KEY, payload TEXT NOT NULL, saved_at REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS uploads(upload_id TEXT PRIMARY KEY, path TEXT NOT NULL, sha256 TEXT NOT NULL, size_bytes INTEGER NOT NULL, mime TEXT, created_at REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS auth_pairings(token_hash TEXT PRIMARY KEY, pairing_id TEXT UNIQUE NOT NULL, device_name TEXT, created_at REAL NOT NULL, last_seen_at REAL, revoked INTEGER NOT NULL DEFAULT 0, revoked_at REAL);
             CREATE TABLE IF NOT EXISTS audit_log(id INTEGER PRIMARY KEY, at REAL NOT NULL, client TEXT NOT NULL, action TEXT NOT NULL, target TEXT NOT NULL, result TEXT NOT NULL, detail TEXT NOT NULL);"
        )?;
        let current: Option<i64> =
            tx.query_row("SELECT MAX(version) FROM schema_version", [], |r| {
                r.get::<_, Option<i64>>(0)
            })?;
        if current.unwrap_or(0) < 1 {
            tx.execute(
                "INSERT INTO schema_version(version, applied_at) VALUES(1, unixepoch())",
                [],
            )?;
            self.seed_builtin_materials(&tx)?;
        }
        if current.unwrap_or(0) < 2 {
            tx.execute_batch(
                "ALTER TABLE jobs ADD COLUMN updated_at REAL NOT NULL DEFAULT 0;
                 UPDATE jobs SET updated_at=COALESCE(finished_at,started_at,created_at);
                 CREATE INDEX workflows_status_updated ON workflows(status,updated_at DESC);
                 CREATE INDEX workflows_source ON workflows(source_type);
                 CREATE INDEX workflows_material ON workflows(material,thickness_mm);
                 CREATE INDEX jobs_updated ON jobs(updated_at DESC);
                 CREATE INDEX text_tasks_updated ON text_tasks(updated_at DESC);
                 CREATE INDEX calibrations_updated ON calibrations(updated_at DESC);",
            )?;
            for table in ["workflows", "text_tasks", "jobs", "calibrations"] {
                tx.execute_batch(&format!(
                    "CREATE TABLE {table}_history(
                       history_id INTEGER PRIMARY KEY, record_id TEXT NOT NULL,
                       payload TEXT NOT NULL, updated_at REAL NOT NULL,
                       replaced_at REAL NOT NULL, reason TEXT NOT NULL,
                       UNIQUE(record_id,payload)
                     );"
                ))?;
            }
            tx.execute(
                "INSERT INTO schema_version(version,applied_at) VALUES(2,unixepoch())", [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn seed_builtin_materials(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), SchemaError> {
        let now = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
        let library = builtin_material_library();
        for (name, entry) in library.materials {
            tx.execute(
                "INSERT OR IGNORE INTO materials(name,created_at,updated_at) VALUES(?1,?2,?2)",
                params![name, now],
            )?;
            for alias in entry.aliases {
                tx.execute(
                    "INSERT OR IGNORE INTO material_aliases(alias,material_name) VALUES(?1,?2)",
                    params![alias, name],
                )?;
            }
            for (thickness, value) in entry.thicknesses {
                if let Some(engrave) = value.engrave {
                    for (mode, cell) in [("raster", engrave.raster), ("outline", engrave.outline)] {
                        if let Some(cell) = cell {
                            self.insert_cell(tx, &name, &thickness, "engrave", mode, &cell, now)?;
                        }
                    }
                }
                if let Some(cell) = value.cut {
                    self.insert_cell(tx, &name, &thickness, "cut", "", &cell, now)?;
                }
            }
        }
        Ok(())
    }

    fn insert_cell(
        &self,
        tx: &rusqlite::Transaction<'_>,
        material: &str,
        thickness: &str,
        mode: &str,
        engraving: &str,
        cell: &laser_contracts::ParamCell,
        now: f64,
    ) -> Result<(), SchemaError> {
        tx.execute("INSERT OR IGNORE INTO material_params(material_name,thickness_key,laser_mode,engraving_mode,laser_min_power,laser_max_power,feed_rate,travel_rate,threshold,passes,pixel_size_mm,extra) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'{}')", params![material, thickness, mode, engraving, cell.laser_min_power, cell.laser_max_power, cell.feed_rate, cell.travel_rate, cell.threshold, cell.passes, cell.pixel_size_mm])?;
        tx.execute(
            "UPDATE materials SET updated_at=?2 WHERE name=?1",
            params![material, now],
        )?;
        Ok(())
    }

    pub fn recover_after_restart(&self) -> Result<usize, SchemaError> {
        let count = self.connection.execute(
            "UPDATE jobs SET status='failed', finished_at=unixepoch(),
             payload=json_set(payload,'$.result.detail','host 重启中断，任务安全终止')
             WHERE status IN ('running', 'pending', 'queued')",
            [],
        )?;
        self.connection.execute(
            "UPDATE workflows SET status='failed', updated_at=unixepoch(),
             payload=json_set(payload,'$.detail','host 重启中断，任务安全终止')
             WHERE status IN ('sending', 'processing')",
            [],
        )?;
        Ok(count)
    }

    pub fn export_material_library(&self) -> Result<MaterialLibrary, SchemaError> {
        let mut library = MaterialLibrary {
            version: 1,
            power_scale: 1000,
            materials: std::collections::BTreeMap::new(),
        };
        let mut materials = self
            .connection
            .prepare("SELECT name FROM materials ORDER BY rowid")?;
        let names = materials
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for name in names {
            let aliases = {
                let mut statement = self.connection.prepare(
                    "SELECT alias FROM material_aliases WHERE material_name=?1 ORDER BY rowid",
                )?;
                statement
                    .query_map([&name], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            let mut thicknesses = std::collections::BTreeMap::new();
            let mut cells = self.connection.prepare(
                "SELECT thickness_key, laser_mode, engraving_mode, laser_min_power,
                 laser_max_power, feed_rate, travel_rate, threshold, passes, pixel_size_mm,
                 source, updated_at, notes, extra
                 FROM material_params WHERE material_name=?1 ORDER BY id",
            )?;
            let rows = cells.query_map([&name], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    ParamCell {
                        laser_min_power: row.get(3)?,
                        laser_max_power: row.get(4)?,
                        feed_rate: row.get(5)?,
                        travel_rate: row.get(6)?,
                        threshold: row.get(7)?,
                        passes: row.get(8)?,
                        pixel_size_mm: row.get(9)?,
                        power_scale: Some(1000),
                        source: row.get(10)?,
                        updated_at: row.get(11)?,
                        notes: row.get(12)?,
                        extra: serde_json::from_str(&row.get::<_, String>(13)?).unwrap_or_default(),
                    },
                ))
            })?;
            for row in rows {
                let (thickness, mode, engraving, cell) = row?;
                let entry = thicknesses
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
            library.materials.insert(
                name,
                MaterialEntry {
                    aliases,
                    thicknesses,
                },
            );
        }
        Ok(library)
    }

    pub fn export_material_json(&self) -> Result<Value, SchemaError> {
        Ok(serde_json::to_value(self.export_material_library()?)?)
    }

    pub fn import_material_json(
        &self,
        value: &Value,
        policy: MaterialConflictPolicy,
        now: f64,
    ) -> Result<MaterialImportCounts, SchemaError> {
        let mut counts = MaterialImportCounts::default();
        let Some(materials) = value.get("materials").and_then(Value::as_object) else {
            return Ok(counts);
        };

        let tx = self.connection.unchecked_transaction()?;
        for (name, mat_val) in materials {
            tx.execute(
                "INSERT OR IGNORE INTO materials(name, created_at, updated_at) VALUES(?1, ?2, ?2)",
                params![name, now],
            )?;

            if let Some(aliases) = mat_val.get("aliases").and_then(Value::as_array) {
                for alias in aliases {
                    if let Some(alias_str) = alias.as_str() {
                        let trimmed = alias_str.trim();
                        if !trimmed.is_empty() {
                            tx.execute(
                                "INSERT OR IGNORE INTO material_aliases(alias, material_name) VALUES(?1, ?2)",
                                params![trimmed, name],
                            )?;
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
                            if engrave_val.is_null() || engrave_val.as_object().is_some_and(|o| o.is_empty()) {
                                let deleted = tx.execute(
                                    "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode='engrave'",
                                    params![name, thickness],
                                )?;
                                if deleted > 0 {
                                    counts.tombstones += deleted as u64;
                                }
                            } else if let Some(engrave_obj) = engrave_val.as_object() {
                                if engrave_obj.contains_key("raster") || engrave_obj.contains_key("outline") {
                                    for mode in ["raster", "outline"] {
                                        if let Some(cell_val) = engrave_obj.get(mode) {
                                            if cell_val.is_null() || cell_val.as_object().is_some_and(|o| o.is_empty()) {
                                                let deleted = tx.execute(
                                                    "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode='engrave' AND engraving_mode=?3",
                                                    params![name, thickness, mode],
                                                )?;
                                                if deleted > 0 {
                                                    counts.tombstones += deleted as u64;
                                                }
                                            } else if let Some(cell) = parse_param_cell_from_value(cell_val) {
                                                self.upsert_cell_with_conflict(&tx, name, thickness, "engrave", mode, &cell, policy, now, &mut counts)?;
                                            }
                                        }
                                    }
                                } else if let Some(cell) = parse_param_cell_from_value(engrave_val) {
                                    self.upsert_cell_with_conflict(&tx, name, thickness, "engrave", "raster", &cell, policy, now, &mut counts)?;
                                }
                            }
                        }

                        // Handle cut
                        if let Some(cut_val) = thick_obj.get("cut") {
                            if cut_val.is_null() || cut_val.as_object().is_some_and(|o| o.is_empty()) {
                                let deleted = tx.execute(
                                    "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode='cut'",
                                    params![name, thickness],
                                )?;
                                if deleted > 0 {
                                    counts.tombstones += deleted as u64;
                                }
                            } else if let Some(cell) = parse_param_cell_from_value(cut_val) {
                                self.upsert_cell_with_conflict(&tx, name, thickness, "cut", "", &cell, policy, now, &mut counts)?;
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
            .query_row(
                "SELECT id, laser_min_power, laser_max_power, feed_rate, travel_rate, threshold, passes, pixel_size_mm, source, notes
                 FROM material_params WHERE material_name=?1 AND thickness_key=?2 AND laser_mode=?3 AND engraving_mode=?4",
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
                tx.execute(
                    "INSERT INTO material_params(
                        material_name, thickness_key, laser_mode, engraving_mode,
                        laser_min_power, laser_max_power, feed_rate, travel_rate,
                        threshold, passes, pixel_size_mm, source, updated_at, notes, extra
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        material, thickness, mode, engraving,
                        cell.laser_min_power, cell.laser_max_power, cell.feed_rate, cell.travel_rate,
                        cell.threshold, cell.passes, cell.pixel_size_mm, cell.source, cell.updated_at.or(Some(now)),
                        cell.notes, extra_json
                    ],
                )?;
                tx.execute("UPDATE materials SET updated_at=?2 WHERE name=?1", params![material, now])?;
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

                        let extra_json = serde_json::to_string(&cell.extra).unwrap_or_else(|_| "{}".into());
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
                        tx.execute("UPDATE materials SET updated_at=?2 WHERE name=?1", params![material, now])?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn read_payload(
        &self,
        table: &str,
        id_column: &str,
        id: &str,
    ) -> Result<Option<Value>, SchemaError> {
        if !matches!(table, "workflows" | "text_tasks" | "jobs" | "calibrations")
            || !matches!(
                id_column,
                "workflow_id" | "task_id" | "job_id" | "calibration_id"
            )
        {
            return Ok(None);
        }
        let sql = format!("SELECT payload FROM {table} WHERE {id_column}=?1");
        let raw: Option<String> = self
            .connection
            .query_row(&sql, [id], |r| r.get(0))
            .optional()?;
        raw.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(SchemaError::from)
    }
}
