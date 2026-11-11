use std::collections::BTreeMap;

use serde_json::Value;

use crate::db::{RuntimeDb, SchemaError};
use laser_contracts::{CalibrationRecord, JobRecord, TextTaskRecord, WorkflowRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecordKind {
    Workflow,
    TextTask,
    Job,
    Calibration,
}

impl RecordKind {
    pub(crate) fn table(self) -> (&'static str, &'static str) {
        match self {
            Self::Workflow => ("workflows", "workflow_id"),
            Self::TextTask => ("text_tasks", "task_id"),
            Self::Job => ("jobs", "job_id"),
            Self::Calibration => ("calibrations", "calibration_id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    pub kind: RecordKind,
    pub id: String,
    pub status: Option<String>,
    pub payload: Value,
}

pub trait RuntimeStore {
    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError>;
    fn put(&self, record: &StoredRecord) -> Result<(), SchemaError>;
    fn set_status(
        &self,
        kind: RecordKind,
        id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, SchemaError>;
    fn list_recent(&self, kind: RecordKind, limit: usize)
    -> Result<Vec<StoredRecord>, SchemaError>;
    /// Atomically append or replace only feedback history, without overwriting job state.
    fn save_feedback(&self, workflow_id: &str, feedback: &Value) -> Result<bool, SchemaError>;
    fn adopt_feedback(
        &self,
        workflow_id: &str,
        feedback: &Value,
        preview: &StoredRecord,
    ) -> Result<bool, SchemaError>;
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    records: std::sync::Mutex<BTreeMap<(RecordKind, String), StoredRecord>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RuntimeStore for MemoryStore {
    fn adopt_feedback(
        &self,
        workflow_id: &str,
        feedback: &Value,
        preview: &StoredRecord,
    ) -> Result<bool, SchemaError> {
        let mut records = self.records.lock().map_err(|_| SchemaError::Busy)?;
        let Some(record) = records.get_mut(&(RecordKind::Workflow, workflow_id.to_owned())) else {
            return Ok(false);
        };
        if !feedback_adoptable(&record.payload, feedback) {
            return Ok(false);
        }
        merge_feedback(&mut record.payload, feedback);
        records.insert((preview.kind, preview.id.clone()), preview.clone());
        Ok(true)
    }
    fn save_feedback(&self, workflow_id: &str, feedback: &Value) -> Result<bool, SchemaError> {
        let mut records = self.records.lock().map_err(|_| SchemaError::Busy)?;
        let Some(record) = records.get_mut(&(RecordKind::Workflow, workflow_id.to_owned())) else {
            return Ok(false);
        };
        merge_feedback(&mut record.payload, feedback);
        Ok(true)
    }
    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError> {
        Ok(self
            .records
            .lock()
            .map_err(|_| SchemaError::Busy)?
            .get(&(kind, id.to_owned()))
            .cloned())
    }

    fn put(&self, record: &StoredRecord) -> Result<(), SchemaError> {
        let mut records = self.records.lock().map_err(|_| SchemaError::Busy)?;
        let mut record = record.clone();
        if record.kind == RecordKind::Workflow
            && let Some(history) = records
                .get(&(record.kind, record.id.clone()))
                .and_then(|previous| previous.payload.get("feedback_history"))
                .filter(|v| v.is_array())
        {
            record.payload["feedback_history"] = history.clone();
        }
        records.insert((record.kind, record.id.clone()), record);
        Ok(())
    }

    fn set_status(
        &self,
        kind: RecordKind,
        id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        let mut records = self.records.lock().map_err(|_| SchemaError::Busy)?;
        let Some(record) = records.get_mut(&(kind, id.to_owned())) else {
            return Ok(false);
        };
        record.status = Some(status.to_owned());
        if let Some(object) = record.payload.as_object_mut() {
            object.insert("status".into(), Value::String(status.to_owned()));
            object.insert("updated_at".into(), serde_json::json!(now));
        }
        Ok(true)
    }

    fn list_recent(
        &self,
        kind: RecordKind,
        limit: usize,
    ) -> Result<Vec<StoredRecord>, SchemaError> {
        let mut records: Vec<_> = self
            .records
            .lock()
            .map_err(|_| SchemaError::Busy)?
            .values()
            .filter(|record| record.kind == kind)
            .cloned()
            .collect();
        records.sort_by(|a, b| {
            updated_at(&b.payload)
                .total_cmp(&updated_at(&a.payload))
                .then_with(|| a.id.cmp(&b.id))
        });
        records.truncate(limit);
        Ok(records)
    }
}

pub struct SqliteStore<'a> {
    pub db: &'a RuntimeDb,
}

impl<'a> SqliteStore<'a> {
    pub fn new(db: &'a RuntimeDb) -> Self {
        Self { db }
    }

    fn table(kind: RecordKind) -> (&'static str, &'static str) {
        kind.table()
    }
}

impl RuntimeStore for SqliteStore<'_> {
    fn adopt_feedback(
        &self,
        workflow_id: &str,
        feedback: &Value,
        preview: &StoredRecord,
    ) -> Result<bool, SchemaError> {
        let tx = self.db.connection.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE workflows SET payload=payload WHERE workflow_id=?1",
            [workflow_id],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        let raw: String = tx.query_row(
            "SELECT payload FROM workflows WHERE workflow_id=?1",
            [workflow_id],
            |row| row.get(0),
        )?;
        let mut payload: Value = serde_json::from_str(&raw)?;
        if !feedback_adoptable(&payload, feedback) {
            return Ok(false);
        }
        merge_feedback(&mut payload, feedback);
        put_record(&tx, preview)?;
        tx.execute("UPDATE workflows SET payload=json_set(payload,'$.feedback_history',json(?2)) WHERE workflow_id=?1",
            rusqlite::params![workflow_id, payload["feedback_history"].to_string()])?;
        tx.commit()?;
        Ok(true)
    }
    fn save_feedback(&self, workflow_id: &str, feedback: &Value) -> Result<bool, SchemaError> {
        let tx = self.db.connection.unchecked_transaction()?;
        // An UPDATE acquires the write lock before reading; only feedback_history changes.
        let changed = tx.execute(
            "UPDATE workflows SET payload=json_set(payload,'$.feedback_history',
             COALESCE(json_extract(payload,'$.feedback_history'),json('[]')))
             WHERE workflow_id=?1",
            [workflow_id],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        let raw: String = tx.query_row(
            "SELECT payload FROM workflows WHERE workflow_id=?1",
            [workflow_id],
            |row| row.get(0),
        )?;
        let mut payload: Value = serde_json::from_str(&raw)?;
        merge_feedback(&mut payload, feedback);
        tx.execute(
            "UPDATE workflows SET payload=json_set(payload,'$.feedback_history',json(?2))
             WHERE workflow_id=?1",
            rusqlite::params![workflow_id, payload["feedback_history"].to_string()],
        )?;
        tx.commit()?;
        Ok(true)
    }
    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError> {
        use rusqlite::OptionalExtension;
        let (table, key) = Self::table(kind);
        let sql = format!("SELECT payload, status FROM {table} WHERE {key}=?1");
        let mut statement = self.db.connection.prepare_cached(&sql)?;
        let row: Option<(String, Option<String>)> = statement
            .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let Some((raw_payload, status)) = row else {
            return Ok(None);
        };
        let payload: Value = serde_json::from_str(&raw_payload)?;
        Ok(Some(StoredRecord {
            kind,
            id: id.to_owned(),
            status,
            payload,
        }))
    }

    fn put(&self, record: &StoredRecord) -> Result<(), SchemaError> {
        put_record(&self.db.connection, record)
    }

    fn set_status(
        &self,
        kind: RecordKind,
        id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        let (table, key) = Self::table(kind);
        let sql = format!(
            "UPDATE {table} SET status=?1, updated_at=?2,
            payload=json_set(payload,'$.status',?1,'$.updated_at',?2) WHERE {key}=?3"
        );
        let mut statement = self.db.connection.prepare_cached(&sql)?;
        let changed = statement.execute(rusqlite::params![status, now, id])?;
        Ok(changed == 1)
    }

    fn list_recent(
        &self,
        kind: RecordKind,
        limit: usize,
    ) -> Result<Vec<StoredRecord>, SchemaError> {
        let (table, key) = Self::table(kind);
        let sql = format!(
            "SELECT {key}, payload, status FROM {table}
            ORDER BY updated_at DESC, {key} ASC LIMIT ?1"
        );
        let mut statement = self.db.connection.prepare_cached(&sql)?;
        let rows = statement.query_map([limit as i64], |row| {
            let id: String = row.get(0)?;
            let payload: String = row.get(1)?;
            let status: Option<String> = row.get(2)?;
            Ok((id, payload, status))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (id, payload, status) = row?;
            let payload: Value = serde_json::from_str(&payload)?;
            records.push(StoredRecord {
                kind,
                id,
                status,
                payload,
            });
        }
        Ok(records)
    }
}

/// 读写分离 SQLite 存储：写操作走主连接锁，纯只读操作（`get`、`list_recent`）优先走独立 WAL 只读连接，
/// 避免后台重建知识库或批量写入期间阻塞前端高频状态轮询；内存库自动回退到主连接。
pub struct SplitSqliteStore {
    write_db: std::sync::Arc<std::sync::Mutex<RuntimeDb>>,
    read_db: Option<std::sync::Arc<std::sync::Mutex<RuntimeDb>>>,
}

impl SplitSqliteStore {
    pub fn new(
        write_db: std::sync::Arc<std::sync::Mutex<RuntimeDb>>,
        read_db: Option<std::sync::Arc<std::sync::Mutex<RuntimeDb>>>,
    ) -> Self {
        Self { write_db, read_db }
    }

    fn lock_read(&self) -> Result<std::sync::MutexGuard<'_, RuntimeDb>, SchemaError> {
        match self.read_db.as_ref() {
            Some(read) => read.lock().map_err(|_| SchemaError::Busy),
            None => self.write_db.lock().map_err(|_| SchemaError::Busy),
        }
    }

    fn lock_write(&self) -> Result<std::sync::MutexGuard<'_, RuntimeDb>, SchemaError> {
        self.write_db.lock().map_err(|_| SchemaError::Busy)
    }
}

impl RuntimeStore for SplitSqliteStore {
    fn adopt_feedback(
        &self,
        workflow_id: &str,
        feedback: &Value,
        preview: &StoredRecord,
    ) -> Result<bool, SchemaError> {
        let db = self.lock_write()?;
        SqliteStore::new(&db).adopt_feedback(workflow_id, feedback, preview)
    }

    fn save_feedback(&self, workflow_id: &str, feedback: &Value) -> Result<bool, SchemaError> {
        let db = self.lock_write()?;
        SqliteStore::new(&db).save_feedback(workflow_id, feedback)
    }

    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError> {
        let db = self.lock_read()?;
        SqliteStore::new(&db).get(kind, id)
    }

    fn put(&self, record: &StoredRecord) -> Result<(), SchemaError> {
        let db = self.lock_write()?;
        SqliteStore::new(&db).put(record)
    }

    fn set_status(
        &self,
        kind: RecordKind,
        id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        let db = self.lock_write()?;
        SqliteStore::new(&db).set_status(kind, id, status, now)
    }

    fn list_recent(
        &self,
        kind: RecordKind,
        limit: usize,
    ) -> Result<Vec<StoredRecord>, SchemaError> {
        let db = self.lock_read()?;
        SqliteStore::new(&db).list_recent(kind, limit)
    }
}

impl RuntimeStore for std::sync::Mutex<RuntimeDb> {
    fn adopt_feedback(
        &self,
        workflow_id: &str,
        feedback: &Value,
        preview: &StoredRecord,
    ) -> Result<bool, SchemaError> {
        let db = self.lock().map_err(|_| SchemaError::Busy)?;
        SqliteStore::new(&db).adopt_feedback(workflow_id, feedback, preview)
    }
    fn save_feedback(&self, workflow_id: &str, feedback: &Value) -> Result<bool, SchemaError> {
        let db = self.lock().map_err(|_| SchemaError::Busy)?;
        SqliteStore::new(&db).save_feedback(workflow_id, feedback)
    }
    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError> {
        let db = self.lock().map_err(|_| SchemaError::Busy)?;
        SqliteStore::new(&db).get(kind, id)
    }

    fn put(&self, record: &StoredRecord) -> Result<(), SchemaError> {
        let db = self.lock().map_err(|_| SchemaError::Busy)?;
        SqliteStore::new(&db).put(record)
    }

    fn set_status(
        &self,
        kind: RecordKind,
        id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        let db = self.lock().map_err(|_| SchemaError::Busy)?;
        SqliteStore::new(&db).set_status(kind, id, status, now)
    }

    fn list_recent(
        &self,
        kind: RecordKind,
        limit: usize,
    ) -> Result<Vec<StoredRecord>, SchemaError> {
        let db = self.lock().map_err(|_| SchemaError::Busy)?;
        SqliteStore::new(&db).list_recent(kind, limit)
    }
}

pub fn feedback_adoptable(payload: &Value, feedback: &Value) -> bool {
    payload["stale"] != true
        && !matches!(
            payload["status"].as_str(),
            Some("sending" | "running" | "preparing")
        )
        && payload["feedback_history"]
            .as_array()
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry["feedback_id"] == feedback["feedback_id"]
                        && entry["adopted_params"].is_null()
                })
            })
}

pub fn merge_feedback(payload: &mut Value, feedback: &Value) {
    if !payload["feedback_history"].is_array() {
        payload["feedback_history"] = serde_json::json!([]);
    }
    let history = payload["feedback_history"].as_array_mut().unwrap();
    if let Some(existing) = history
        .iter_mut()
        .find(|item| item["feedback_id"] == feedback["feedback_id"])
    {
        // Adoption is monotonic: concurrent stale responses cannot erase a completed adoption.
        if existing["adopted_params"].is_null() && !feedback["adopted_params"].is_null() {
            *existing = feedback.clone();
        }
    } else {
        history.push(feedback.clone());
    }
}

pub fn coerce_numeric_to_f64(v: &mut Value) {
    if let Value::String(s) = v
        && let Ok(num) = s.trim().parse::<f64>()
    {
        *v = serde_json::json!(num);
    }
}

pub fn coerce_numeric_to_i64(v: &mut Value) {
    if let Value::String(s) = v {
        if let Ok(num) = s.trim().parse::<f64>() {
            *v = serde_json::json!(num.trunc() as i64);
        }
    } else if let Value::Number(n) = v
        && let Some(f) = n.as_f64()
    {
        *v = serde_json::json!(f.trunc() as i64);
    }
}

pub fn coerce_to_bool(v: &mut Value) {
    if let Value::String(s) = v {
        let trimmed = s.trim().to_ascii_lowercase();
        if matches!(trimmed.as_str(), "true" | "1" | "yes") {
            *v = Value::Bool(true);
        } else if matches!(trimmed.as_str(), "false" | "0" | "no") {
            *v = Value::Bool(false);
        }
    } else if let Value::Number(n) = v {
        if n.as_i64() == Some(1) {
            *v = Value::Bool(true);
        } else if n.as_i64() == Some(0) {
            *v = Value::Bool(false);
        }
    }
}

pub fn decode_workflow(payload: &Value) -> Result<WorkflowRecord, (bool, String)> {
    let mut normalized = payload.clone();
    if let Some(input) = normalized.get_mut("input").and_then(Value::as_object_mut) {
        if let Some(t) = input.get_mut("thickness_mm") {
            coerce_numeric_to_f64(t);
        }
        if let Some(fs) = input.get_mut("font_size") {
            coerce_numeric_to_i64(fs);
        }
        if let Some(port) = input.get_mut("network_telnet_port") {
            coerce_numeric_to_i64(port);
        }
    }
    if let Some(snapshot) = normalized
        .get_mut("confirmation_snapshot")
        .and_then(Value::as_object_mut)
    {
        for key in ["pixel_size_mm", "overscan_mm", "power_percent"] {
            if let Some(v) = snapshot.get_mut(key) {
                coerce_numeric_to_f64(v);
            }
        }
        for key in [
            "threshold",
            "laser_min_power",
            "laser_max_power",
            "travel_rate",
            "s_max",
            "feed_rate",
            "passes",
            "power",
        ] {
            if let Some(v) = snapshot.get_mut(key) {
                coerce_numeric_to_i64(v);
            }
        }
        for key in ["invert", "bidirectional"] {
            if let Some(v) = snapshot.get_mut(key) {
                coerce_to_bool(v);
            }
        }
    }
    match serde_json::from_value::<WorkflowRecord>(normalized) {
        Ok(record) => Ok(record),
        Err(err) => Err((true, format!("workflow deserialization error: {err}"))),
    }
}

pub fn decode_job(payload: &Value) -> Result<JobRecord, (bool, String)> {
    let mut normalized = payload.clone();
    for key in [
        "baudrate",
        "http_port",
        "telnet_port",
        "process_id",
        "progress_lines",
    ] {
        if let Some(v) = normalized.get_mut(key) {
            coerce_numeric_to_i64(v);
        }
    }
    for key in [
        "timeout",
        "started_at",
        "finished_at",
        "created_at",
        "progress_updated_at",
    ] {
        if let Some(v) = normalized.get_mut(key) {
            coerce_numeric_to_f64(v);
        }
    }
    match serde_json::from_value::<JobRecord>(normalized) {
        Ok(record) => Ok(record),
        Err(err) => Err((true, format!("job deserialization error: {err}"))),
    }
}

pub fn decode_text_task(payload: &Value) -> Result<TextTaskRecord, (bool, String)> {
    let mut normalized = payload.clone();
    if let Some(t) = normalized.get_mut("thickness_mm") {
        coerce_numeric_to_f64(t);
    }
    if let Some(opts) = normalized
        .get_mut("generation_options")
        .and_then(Value::as_object_mut)
        && let Some(tt) = opts.get_mut("trim_tolerance")
    {
        coerce_numeric_to_f64(tt);
    }
    match serde_json::from_value::<TextTaskRecord>(normalized) {
        Ok(record) => Ok(record),
        Err(err) => Err((true, format!("text_task deserialization error: {err}"))),
    }
}

pub fn decode_calibration(payload: &Value) -> Result<CalibrationRecord, (bool, String)> {
    let mut normalized = payload.clone();
    if let Some(t) = normalized.get_mut("thickness_mm") {
        coerce_numeric_to_f64(t);
    }
    if let Some(conn) = normalized
        .get_mut("connection")
        .and_then(Value::as_object_mut)
        && let Some(timeout) = conn.get_mut("network_timeout")
    {
        coerce_numeric_to_f64(timeout);
    }
    if let Some(cell) = normalized
        .get_mut("selected_cell")
        .and_then(Value::as_object_mut)
        && let Some(cell_num) = cell.get_mut("cell_number")
    {
        coerce_numeric_to_i64(cell_num);
    }
    if normalized.get("selected_cell_number").is_none()
        && let Some(cell_num) = normalized.pointer("/selected_cell/cell_number").cloned()
    {
        normalized["selected_cell_number"] = cell_num;
    }
    match serde_json::from_value::<CalibrationRecord>(normalized) {
        Ok(record) => Ok(record),
        Err(err) => Err((true, format!("calibration deserialization error: {err}"))),
    }
}

pub(crate) fn numeric(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|n| n.is_finite())
}

pub(crate) fn updated_at(payload: &Value) -> f64 {
    numeric(payload.get("updated_at"))
        .or_else(|| numeric(payload.get("created_at")))
        .unwrap_or(0.0)
}

pub(crate) fn put_record(
    connection: &rusqlite::Connection,
    record: &StoredRecord,
) -> Result<(), SchemaError> {
    use rusqlite::types::Value as SqlValue;
    if !record.payload.is_object() || record.id.is_empty() {
        return Err(SchemaError::InvalidRecord(
            "record must have an id and object payload".into(),
        ));
    }
    let payload = &record.payload;
    let (table, key) = record.kind.table();
    let text = |pointer: &str| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(|s| SqlValue::Text(s.into()))
            .unwrap_or(SqlValue::Null)
    };
    let real = |pointer: &str| {
        numeric(payload.pointer(pointer))
            .map(SqlValue::Real)
            .unwrap_or(SqlValue::Null)
    };
    let integer = |pointer: &str| {
        numeric(payload.pointer(pointer))
            .filter(|n| *n >= i64::MIN as f64 && *n < -(i64::MIN as f64))
            .map(|n| SqlValue::Integer(n.trunc() as i64))
            .unwrap_or(SqlValue::Null)
    };
    let stale = payload.get("stale").is_some_and(|v| v == true || v == 1);
    let mut columns = vec![key, "status", "created_at", "updated_at", "payload"];
    let mut values = vec![
        SqlValue::Text(record.id.clone()),
        SqlValue::Text(record.status.clone().unwrap_or_default()),
        SqlValue::Real(numeric(payload.get("created_at")).unwrap_or(0.0)),
        SqlValue::Real(updated_at(payload)),
        SqlValue::Text(serde_json::to_string(payload)?),
    ];
    let mut push = |column, value| {
        columns.push(column);
        values.push(value);
    };
    match record.kind {
        RecordKind::Workflow => {
            for (column, pointer) in [
                ("source_type", "/source_type"),
                ("material", "/input/material"),
                ("laser_mode", "/input/laser_mode"),
                ("connection_mode", "/input/connection_mode"),
                ("gcode_path", "/artifacts/gcode_file"),
                ("gcode_sha256", "/artifacts/gcode_sha256"),
            ] {
                push(column, text(pointer));
            }
            push("thickness_mm", real("/input/thickness_mm"));
            push(
                "has_snapshot",
                SqlValue::Integer(i64::from(
                    payload
                        .get("confirmation_snapshot")
                        .and_then(Value::as_object)
                        .is_some_and(|v| !v.is_empty()),
                )),
            );
            push("stale", SqlValue::Integer(i64::from(stale)));
        }
        RecordKind::TextTask => {
            for name in ["text", "material", "laser_mode", "task_dir"] {
                push(name, text(&format!("/{name}")));
            }
            push("thickness_mm", real("/thickness_mm"));
            push(
                "attempt_count",
                SqlValue::Integer(
                    payload
                        .get("attempts")
                        .and_then(Value::as_array)
                        .map_or(0, |v| v.len()) as i64,
                ),
            );
            push("stale", SqlValue::Integer(i64::from(stale)));
        }
        RecordKind::Job => {
            for name in [
                "workflow_id",
                "port",
                "host",
                "expected_gcode_sha256",
                "calibration_id",
            ] {
                push(name, text(&format!("/{name}")));
            }
            push(
                "transport",
                SqlValue::Text(
                    payload
                        .get("transport")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                ),
            );
            push("gcode_path", text("/prepared/gcode_file"));
            for name in [
                "baudrate",
                "http_port",
                "telnet_port",
                "process_id",
                "progress_lines",
            ] {
                push(name, integer(&format!("/{name}")));
            }
            for name in [
                "timeout",
                "started_at",
                "finished_at",
                "progress_updated_at",
            ] {
                push(name, real(&format!("/{name}")));
            }
        }
        RecordKind::Calibration => {
            for name in [
                "material",
                "laser_mode",
                "engraving_mode",
                "matrix_kind",
                "job_id",
            ] {
                push(name, text(&format!("/{name}")));
            }
            push("thickness_mm", real("/thickness_mm"));
            push("gcode_path", text("/gcode_file"));
            push(
                "selected_cell_number",
                integer("/selected_cell/cell_number"),
            );
        }
    }
    let placeholders = (1..=values.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let updates = columns.iter().skip(1).map(|c| {
        if record.kind == RecordKind::Workflow && *c == "payload" {
            "payload=CASE WHEN json_type(workflows.payload,'$.feedback_history')='array'
             THEN json_set(excluded.payload,'$.feedback_history',json_extract(workflows.payload,'$.feedback_history'))
             ELSE excluded.payload END".to_owned()
        } else { format!("{c}=excluded.{c}") }
    }).collect::<Vec<_>>().join(",");
    let sql = format!(
        "INSERT INTO {table}({}) VALUES({placeholders}) ON CONFLICT({key}) DO UPDATE SET {updates}",
        columns.join(",")
    );
    let mut stmt = connection.prepare_cached(&sql)?;
    stmt.execute(rusqlite::params_from_iter(values))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, status: &str) -> StoredRecord {
        StoredRecord {
            kind: RecordKind::Workflow,
            id: id.into(),
            status: Some(status.into()),
            payload: serde_json::json!({"status": status, "value": 1}),
        }
    }

    #[test]
    fn memory_store_put_get_and_status_are_atomic_at_record_level() {
        let store = MemoryStore::new();
        store.put(&record("wf_1", "preview_ready")).unwrap();
        assert!(
            store
                .set_status(RecordKind::Workflow, "wf_1", "failed", 2.0)
                .unwrap()
        );
        let stored = store.get(RecordKind::Workflow, "wf_1").unwrap().unwrap();
        assert_eq!(stored.status.as_deref(), Some("failed"));
        assert_eq!(stored.payload["status"], "failed");
    }

    #[test]
    fn sqlite_store_round_trips_payload() {
        let db = RuntimeDb::open_memory().unwrap();
        let store = SqliteStore::new(&db);
        store.put(&record("wf_1", "preview_ready")).unwrap();
        assert_eq!(
            store
                .get(RecordKind::Workflow, "wf_1")
                .unwrap()
                .unwrap()
                .payload["value"],
            1
        );
        assert!(
            store
                .set_status(RecordKind::Workflow, "wf_1", "completed", 3.0)
                .unwrap()
        );
    }

    #[test]
    fn feedback_merge_preserves_status_and_first_adoption_in_all_stores() {
        let db = RuntimeDb::open_memory().unwrap();
        let stores: Vec<Box<dyn RuntimeStore + '_>> = vec![
            Box::new(MemoryStore::new()),
            Box::new(SqliteStore::new(&db)),
        ];
        for store in stores {
            store.put(&record("wf-feedback", "preview_ready")).unwrap();
            let entry = serde_json::json!({"feedback_id":"fb-1","adopted_params":null});
            assert!(!store.save_feedback("missing", &entry).unwrap());
            store.save_feedback("wf-feedback", &entry).unwrap();
            store
                .set_status(RecordKind::Workflow, "wf-feedback", "completed", 42.0)
                .unwrap();
            let adopted = serde_json::json!({"feedback_id":"fb-1","adopted_params":{"passes":2}});
            store.save_feedback("wf-feedback", &adopted).unwrap();
            store.save_feedback("wf-feedback", &entry).unwrap();
            store
                .save_feedback(
                    "wf-feedback",
                    &serde_json::json!({"feedback_id":"fb-2","adopted_params":null}),
                )
                .unwrap();
            let mut stale = record("wf-feedback", "completed");
            stale.payload["updated_at"] = serde_json::json!(42.0);
            store.put(&stale).unwrap();
            let saved = store
                .get(RecordKind::Workflow, "wf-feedback")
                .unwrap()
                .unwrap();
            assert_eq!(saved.status.as_deref(), Some("completed"));
            assert_eq!(saved.payload["updated_at"], 42.0);
            assert_eq!(
                saved.payload["feedback_history"].as_array().unwrap().len(),
                2
            );
            assert_eq!(saved.payload["feedback_history"][0], adopted);
        }
    }

    #[test]
    fn feedback_adoption_is_transactional_and_rejects_busy_workflows() {
        let db = RuntimeDb::open_memory().unwrap();
        let store = SqliteStore::new(&db);
        store.put(&record("original", "completed")).unwrap();
        store
            .save_feedback(
                "original",
                &serde_json::json!({"feedback_id":"fb-1","adopted_params":null}),
            )
            .unwrap();
        let adopted = serde_json::json!({"feedback_id":"fb-1","adopted_params":{"passes":2}});
        db.connection
            .execute_batch(
                "CREATE TRIGGER fail_feedback BEFORE UPDATE ON workflows
            WHEN json_extract(NEW.payload,'$.feedback_history[0].adopted_params') IS NOT NULL
            BEGIN SELECT RAISE(ABORT,'test failure'); END;",
            )
            .unwrap();
        assert!(
            store
                .adopt_feedback("original", &adopted, &record("preview", "preview_ready"))
                .is_err()
        );
        assert!(
            store
                .get(RecordKind::Workflow, "preview")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get(RecordKind::Workflow, "original")
                .unwrap()
                .unwrap()
                .payload["feedback_history"][0]["adopted_params"]
                .is_null()
        );
        db.connection
            .execute_batch("DROP TRIGGER fail_feedback")
            .unwrap();
        store
            .set_status(RecordKind::Workflow, "original", "sending", 1.0)
            .unwrap();
        assert!(
            !store
                .adopt_feedback("original", &adopted, &record("preview", "preview_ready"))
                .unwrap()
        );
        store
            .set_status(RecordKind::Workflow, "original", "completed", 2.0)
            .unwrap();
        assert!(
            store
                .adopt_feedback("original", &adopted, &record("preview", "preview_ready"))
                .unwrap()
        );
        assert!(
            !store
                .adopt_feedback("original", &adopted, &record("duplicate", "preview_ready"))
                .unwrap()
        );
        assert!(
            store
                .get(RecordKind::Workflow, "duplicate")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn sqlite_store_extracts_indexes_for_all_record_kinds() {
        let db = RuntimeDb::open_memory().unwrap();
        let store = SqliteStore::new(&db);
        let records = [
            StoredRecord {
                kind: RecordKind::TextTask,
                id: "task_1".into(),
                status: Some("prepared".into()),
                payload: serde_json::json!({
                    "status":"prepared","text":"Hi","material":"椴木",
                    "thickness_mm":"3","laser_mode":"engrave","attempts":[{}],
                    "created_at":1,"updated_at":2
                }),
            },
            StoredRecord {
                kind: RecordKind::Job,
                id: "job_1".into(),
                status: Some("pending".into()),
                payload: serde_json::json!({
                    "status":"pending","transport":"serial","workflow_id":"wf_1",
                    "port":"COM1","baudrate":"115200","prepared":{"gcode_file":"x.gcode"},
                    "created_at":1,"updated_at":3
                }),
            },
            StoredRecord {
                kind: RecordKind::Calibration,
                id: "cal_1".into(),
                status: Some("prepared".into()),
                payload: serde_json::json!({
                    "status":"prepared","material":"椴木","thickness_mm":"3",
                    "laser_mode":"engrave","engraving_mode":"raster","matrix_kind":"engrave",
                    "gcode_file":"cal.gcode","selected_cell":{"cell_number":4},
                    "created_at":1,"updated_at":4
                }),
            },
        ];
        for record in &records {
            store.put(record).unwrap();
        }
        let task: (String, f64, i64) = db.connection.query_row(
            "SELECT material, thickness_mm, attempt_count FROM text_tasks WHERE task_id='task_1'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).unwrap();
        assert_eq!(task, ("椴木".into(), 3.0, 1));
        let job: (String, String, i64) = db
            .connection
            .query_row(
                "SELECT transport, gcode_path, baudrate FROM jobs WHERE job_id='job_1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(job, ("serial".into(), "x.gcode".into(), 115200));
        let calibration: (String, f64, i64) = db.connection.query_row(
            "SELECT material, thickness_mm, selected_cell_number FROM calibrations WHERE calibration_id='cal_1'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).unwrap();
        assert_eq!(calibration, ("椴木".into(), 3.0, 4));
    }

    #[test]
    fn lenient_payload_mapping_covers_schema_section_8() {
        let wf_payload = serde_json::json!({
            "workflow_id": "wf_test",
            "status": "preview_ready",
            "input": {
                "material": "椴木",
                "thickness_mm": "3.5",
                "font_size": "240",
                "network_telnet_port": "23"
            },
            "confirmation_snapshot": {
                "power": "250",
                "feed_rate": "1200",
                "pixel_size_mm": "0.15",
                "overscan_mm": "1.0",
                "threshold": "128",
                "invert": "yes",
                "bidirectional": "1"
            },
            "created_at": 100.0,
            "updated_at": 200.0
        });

        let wf = decode_workflow(&wf_payload).expect("workflow decoding succeeds");
        assert_eq!(wf.input.thickness_mm, Some(3.5));
        assert_eq!(wf.input.font_size, Some(240));
        assert_eq!(wf.input.network_telnet_port, Some(23));
        let snap = wf.confirmation_snapshot.expect("snapshot exists");
        assert_eq!(snap.power, Some(250));
        assert_eq!(snap.feed_rate, Some(1200));
        assert_eq!(snap.pixel_size_mm, Some(0.15));
        assert_eq!(snap.overscan_mm, Some(1.0));
        assert_eq!(snap.threshold, Some(128));

        let job_payload = serde_json::json!({
            "job_id": "job_test",
            "transport": "serial",
            "status": "pending",
            "baudrate": "115200",
            "http_port": "8766",
            "timeout": "10.5",
            "created_at": 10.0
        });
        let job = decode_job(&job_payload).expect("job decoding succeeds");
        assert_eq!(job.baudrate, Some(115200));
        assert_eq!(job.http_port, Some(8766));
        assert_eq!(job.timeout, Some(10.5));

        let task_payload = serde_json::json!({
            "task_id": "task_test",
            "thickness_mm": "2",
            "generation_options": {
                "trim_tolerance": "0.05"
            },
            "created_at": 1.0,
            "updated_at": 2.0
        });
        let task = decode_text_task(&task_payload).expect("text_task decoding succeeds");
        assert_eq!(task.thickness_mm, Some(2.0));

        let cal_payload = serde_json::json!({
            "calibration_id": "cal_test",
            "thickness_mm": 1,
            "connection": {
                "network_timeout": "25.0"
            },
            "selected_cell": {
                "cell_number": "5"
            },
            "created_at": 1.0,
            "updated_at": 2.0
        });
        let cal = decode_calibration(&cal_payload).expect("calibration decoding succeeds");
        assert_eq!(cal.thickness_mm, Some(1.0));
        assert_eq!(cal.selected_cell_number, Some(5));

        // Test invalid/broken record returning error
        let bad_payload = serde_json::json!({
            "not_a_workflow": 123
        });
        let bad_res = decode_workflow(&bad_payload);
        assert!(bad_res.is_err());
        assert!(bad_res.unwrap_err().0); // stale is true
    }

    #[test]
    fn memory_store_returns_busy_instead_of_panicking_when_poisoned() {
        let store = std::sync::Arc::new(MemoryStore::new());
        let poisoned = store.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = poisoned.records.lock().unwrap();
            panic!("poison memory store");
        }));
        let dummy = StoredRecord {
            kind: RecordKind::Workflow,
            id: "wf-1".into(),
            status: Some("preview_ready".into()),
            payload: serde_json::json!({"workflow_id": "wf-1"}),
        };
        assert!(matches!(
            store.get(RecordKind::Workflow, "wf-1"),
            Err(SchemaError::Busy)
        ));
        assert!(matches!(store.put(&dummy), Err(SchemaError::Busy)));
        assert!(matches!(
            store.set_status(RecordKind::Workflow, "wf-1", "cancelled", 1.0),
            Err(SchemaError::Busy)
        ));
        assert!(matches!(
            store.list_recent(RecordKind::Workflow, 10),
            Err(SchemaError::Busy)
        ));
    }
}
