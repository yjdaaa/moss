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
    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError> {
        Ok(self
            .records
            .lock()
            .expect("memory store lock")
            .get(&(kind, id.to_owned()))
            .cloned())
    }

    fn put(&self, record: &StoredRecord) -> Result<(), SchemaError> {
        self.records
            .lock()
            .expect("memory store lock")
            .insert((record.kind, record.id.clone()), record.clone());
        Ok(())
    }

    fn set_status(
        &self,
        kind: RecordKind,
        id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        let mut records = self.records.lock().expect("memory store lock");
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
            .expect("memory store lock")
            .values()
            .filter(|record| record.kind == kind)
            .cloned()
            .collect();
        records.sort_by(|a, b| {
            updated_at(&b.payload).total_cmp(&updated_at(&a.payload))
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
    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError> {
        let (table, key) = Self::table(kind);
        let payload = self.db.read_payload(table, key, id)?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let status: Option<String> = self.db.connection.query_row(
            &format!("SELECT status FROM {table} WHERE {key}=?1"), [id], |row| row.get(0)
        )?;
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
        let changed = self.db.connection.execute(
            &format!("UPDATE {table} SET status=?1, updated_at=?2,
                payload=json_set(payload,'$.status',?1,'$.updated_at',?2) WHERE {key}=?3"),
            rusqlite::params![status, now, id],
        )?;
        Ok(changed == 1)
    }

    fn list_recent(
        &self,
        kind: RecordKind,
        limit: usize,
    ) -> Result<Vec<StoredRecord>, SchemaError> {
        let (table, key) = Self::table(kind);
        let sql = format!("SELECT {key}, payload, status FROM {table}
            ORDER BY updated_at DESC, {key} ASC LIMIT ?1");
        let mut statement = self.db.connection.prepare(&sql)?;
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

impl RuntimeStore for std::sync::Mutex<RuntimeDb> {
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

pub fn coerce_numeric_to_f64(v: &mut Value) {
    if let Value::String(s) = v {
        if let Ok(num) = s.trim().parse::<f64>() {
            *v = serde_json::json!(num);
        }
    }
}

pub fn coerce_numeric_to_i64(v: &mut Value) {
    if let Value::String(s) = v {
        if let Ok(num) = s.trim().parse::<f64>() {
            *v = serde_json::json!(num.trunc() as i64);
        }
    } else if let Value::Number(n) = v {
        if let Some(f) = n.as_f64() {
            *v = serde_json::json!(f.trunc() as i64);
        }
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
    {
        if let Some(tt) = opts.get_mut("trim_tolerance") {
            coerce_numeric_to_f64(tt);
        }
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
    if let Some(conn) = normalized.get_mut("connection").and_then(Value::as_object_mut) {
        if let Some(timeout) = conn.get_mut("network_timeout") {
            coerce_numeric_to_f64(timeout);
        }
    }
    if let Some(cell) = normalized.get_mut("selected_cell").and_then(Value::as_object_mut) {
        if let Some(cell_num) = cell.get_mut("cell_number") {
            coerce_numeric_to_i64(cell_num);
        }
    }
    if normalized.get("selected_cell_number").is_none() {
        if let Some(cell_num) = normalized.pointer("/selected_cell/cell_number").cloned() {
            normalized["selected_cell_number"] = cell_num;
        }
    }
    match serde_json::from_value::<CalibrationRecord>(normalized) {
        Ok(record) => Ok(record),
        Err(err) => Err((true, format!("calibration deserialization error: {err}"))),
    }
}

pub(crate) fn numeric(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value.as_f64().or_else(|| value.as_str()?.trim().parse().ok()).filter(|n| n.is_finite())
}

pub(crate) fn updated_at(payload: &Value) -> f64 {
    numeric(payload.get("updated_at")).or_else(|| numeric(payload.get("created_at"))).unwrap_or(0.0)
}

pub(crate) fn put_record(connection: &rusqlite::Connection, record: &StoredRecord) -> Result<(), SchemaError> {
    use rusqlite::types::Value as SqlValue;
    if !record.payload.is_object() || record.id.is_empty() {
        return Err(SchemaError::InvalidRecord("record must have an id and object payload".into()));
    }
    let payload = &record.payload;
    let (table, key) = record.kind.table();
    let text = |pointer: &str| payload.pointer(pointer).and_then(Value::as_str)
        .map(|s| SqlValue::Text(s.into())).unwrap_or(SqlValue::Null);
    let real = |pointer: &str| numeric(payload.pointer(pointer))
        .map(SqlValue::Real).unwrap_or(SqlValue::Null);
    let integer = |pointer: &str| numeric(payload.pointer(pointer))
        .filter(|n| *n >= i64::MIN as f64 && *n < -(i64::MIN as f64))
        .map(|n| SqlValue::Integer(n.trunc() as i64)).unwrap_or(SqlValue::Null);
    let stale = payload.get("stale").is_some_and(|v| v == true || v == 1);
    let mut columns = vec![key, "status", "created_at", "updated_at", "payload"];
    let mut values = vec![
        SqlValue::Text(record.id.clone()), SqlValue::Text(record.status.clone().unwrap_or_default()),
        SqlValue::Real(numeric(payload.get("created_at")).unwrap_or(0.0)),
        SqlValue::Real(updated_at(payload)), SqlValue::Text(serde_json::to_string(payload)?),
    ];
    let mut push = |column, value| { columns.push(column); values.push(value); };
    match record.kind {
        RecordKind::Workflow => {
            for (column, pointer) in [
                ("source_type","/source_type"), ("material","/input/material"),
                ("laser_mode","/input/laser_mode"), ("connection_mode","/input/connection_mode"),
                ("gcode_path","/artifacts/gcode_file"), ("gcode_sha256","/artifacts/gcode_sha256"),
            ] { push(column, text(pointer)); }
            push("thickness_mm", real("/input/thickness_mm"));
            push("has_snapshot", SqlValue::Integer(i64::from(
                payload.get("confirmation_snapshot").and_then(Value::as_object).is_some_and(|v| !v.is_empty())
            )));
            push("stale", SqlValue::Integer(i64::from(stale)));
        }
        RecordKind::TextTask => {
            for name in ["text", "material", "laser_mode", "task_dir"] {
                push(name, text(&format!("/{name}")));
            }
            push("thickness_mm", real("/thickness_mm"));
            push("attempt_count", SqlValue::Integer(payload.get("attempts").and_then(Value::as_array)
                .map_or(0, |v| v.len()) as i64));
            push("stale", SqlValue::Integer(i64::from(stale)));
        }
        RecordKind::Job => {
            for name in ["workflow_id", "port", "host", "expected_gcode_sha256", "calibration_id"] {
                push(name, text(&format!("/{name}")));
            }
            push("transport", SqlValue::Text(payload.get("transport").and_then(Value::as_str).unwrap_or("").into()));
            push("gcode_path", text("/prepared/gcode_file"));
            for name in ["baudrate", "http_port", "telnet_port", "process_id", "progress_lines"] {
                push(name, integer(&format!("/{name}")));
            }
            for name in ["timeout", "started_at", "finished_at", "progress_updated_at"] {
                push(name, real(&format!("/{name}")));
            }
        }
        RecordKind::Calibration => {
            for name in ["material", "laser_mode", "engraving_mode", "matrix_kind", "job_id"] {
                push(name, text(&format!("/{name}")));
            }
            push("thickness_mm", real("/thickness_mm"));
            push("gcode_path", text("/gcode_file"));
            push("selected_cell_number", integer("/selected_cell/cell_number"));
        }
    }
    let placeholders = (1..=values.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let updates = columns.iter().skip(1).map(|c| format!("{c}=excluded.{c}")).collect::<Vec<_>>().join(",");
    connection.execute(
        &format!("INSERT INTO {table}({}) VALUES({placeholders}) ON CONFLICT({key}) DO UPDATE SET {updates}", columns.join(",")),
        rusqlite::params_from_iter(values),
    )?;
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
        for record in &records { store.put(record).unwrap(); }
        let task: (String, f64, i64) = db.connection.query_row(
            "SELECT material, thickness_mm, attempt_count FROM text_tasks WHERE task_id='task_1'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).unwrap();
        assert_eq!(task, ("椴木".into(), 3.0, 1));
        let job: (String, String, i64) = db.connection.query_row(
            "SELECT transport, gcode_path, baudrate FROM jobs WHERE job_id='job_1'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).unwrap();
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
}
