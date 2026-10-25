use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use laser_contracts::{
    ApprovalConnection, ApprovalKind, ApprovalOutcome, ApprovalRecord, ApprovalSource, ApprovalStatus,
    AuditAction, AuditDetail, AuditEntry, AuditResult, EngraveModes, MaterialEntry, MaterialLibrary,
    ParamCell, ThicknessEntry, builtin_material_library,
};

const APPROVAL_COLUMNS: &str = "approval_id, status, kind, target_record, digest, summary, source, \
     session_id, request_id, connection, created_at, decided_at, expires_at, updated_at, outcome, request";

/// `approvals` 表的一行原始值，转换为 `ApprovalRecord` 时校验枚举与 JSON 列。
struct ApprovalRow {
    approval_id: String,
    status: String,
    kind: String,
    target_record: String,
    digest: String,
    summary: String,
    source: String,
    session_id: Option<String>,
    request_id: Option<String>,
    connection: Option<String>,
    created_at: f64,
    decided_at: Option<f64>,
    expires_at: f64,
    updated_at: f64,
    outcome: Option<String>,
    request: Option<String>,
}

impl ApprovalRow {
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            approval_id: r.get(0)?,
            status: r.get(1)?,
            kind: r.get(2)?,
            target_record: r.get(3)?,
            digest: r.get(4)?,
            summary: r.get(5)?,
            source: r.get(6)?,
            session_id: r.get(7)?,
            request_id: r.get(8)?,
            connection: r.get(9)?,
            created_at: r.get(10)?,
            decided_at: r.get(11)?,
            expires_at: r.get(12)?,
            updated_at: r.get(13)?,
            outcome: r.get(14)?,
            request: r.get(15)?,
        })
    }

    fn into_record(self) -> Result<ApprovalRecord, SchemaError> {
        let invalid = |what: &str, value: &str| SchemaError::InvalidRecord(format!("审批记录{what}无效: {value}"));
        let status = ApprovalStatus::parse(&self.status).ok_or_else(|| invalid("状态", &self.status))?;
        let kind = ApprovalKind::parse(&self.kind).ok_or_else(|| invalid("类别", &self.kind))?;
        let source = ApprovalSource::parse(&self.source).ok_or_else(|| invalid("来源", &self.source))?;
        let connection = match self.connection.as_deref() {
            Some(text) => Some(serde_json::from_str::<ApprovalConnection>(text)?),
            None => None,
        };
        let outcome = match self.outcome.as_deref() {
            Some(text) => Some(serde_json::from_str::<ApprovalOutcome>(text)?),
            None => None,
        };
        let request = match self.request.as_deref() {
            Some(text) => Some(serde_json::from_str::<serde_json::Map<String, Value>>(text)?),
            None => None,
        };
        Ok(ApprovalRecord {
            approval_id: self.approval_id,
            status,
            kind,
            target_record: self.target_record,
            digest: self.digest,
            summary: self.summary,
            source,
            session_id: self.session_id,
            request_id: self.request_id,
            connection,
            created_at: self.created_at,
            decided_at: self.decided_at,
            expires_at: self.expires_at,
            updated_at: self.updated_at,
            outcome,
            request,
        })
    }
}

/// 当前 schema 版本（v4：桌面审批 `approvals` 表与审计索引；v5：`approvals.request` 执行参数列，技术书 §7；
/// v6：内置语音服务的设备登记表 `voice_devices`，内置语音技术书 NV1）。
/// v7：按设备存储用户可编辑的语音摘要（NV10），带 revision 防后台旧总结覆盖。
/// v8：按 AgentRun 审批记录可恢复的有界只读 CLI 历史（NV13）。
/// v9：NV16 声纹登记与按人摘要、NV17 固件交付记录。
/// v10：NV19 结构化长期记忆 `context_memories` 与提取变更清单 `context_extraction_runs`。
/// v11：NV19-04 记忆正文的 FTS5 trigram 全文索引 `context_memories_fts`（触发器同步）。
/// v12：NV19-05 分层知识库：资料根目录、目录 L0/L1 摘要与文件指纹，目录摘要 FTS5 索引。
pub const SCHEMA_VERSION: i64 = 13;

/// NV10 语音记忆摘要；仅受用户开启开关后由上层服务读写，不含原始语音。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceMemoryRecord {
    pub device_id: String,
    pub summary: String,
    pub revision: i64,
    pub updated_at: f64,
}

/// Private desktop metadata. Embeddings are deliberately never serialized.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VoiceSpeakerRecord {
    pub id:String, pub name:String, pub model_id:String,
    pub dimension:u32, pub revision:i64, pub created_at:f64,
}
/// Internal-only biometric material; intentionally not Serialize/Deserialize.
pub struct VoiceSpeakerTemplate {pub id:String,pub revision:i64,pub embedding:Vec<f32>}

#[derive(Debug,Clone,Serialize)]
pub struct VoiceFirmwareDeliveryRecord {
    pub delivery_id:String,pub device_id:String,pub artifact_sha256:String,
    pub download_status:String,pub device_report_status:String,pub revision:i64,pub updated_at:f64,
    /// 推送的固件版本与推送到期时间（schema 13；旧记录为空串 / 0）
    pub firmware_version:String,pub expires_at:f64,
}

/// 内置语音服务登记的小智设备（内置语音技术书 §6：新设备首次连接需在桌面端允许）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceDeviceRecord {
    /// 设备 MAC（固件请求头 `Device-Id`）
    pub device_id: String,
    /// 设备 UUID（固件请求头 `Client-Id`）
    pub client_id: String,
    /// `pending` 待允许 / `allowed` 已允许 / `revoked` 已撤销
    pub status: String,
    pub name: String,
    pub model: String,
    pub firmware_version: String,
    pub last_ip: String,
    pub first_seen: f64,
    pub last_seen: f64,
    pub decided_at: Option<f64>,
}

impl VoiceDeviceRecord {
    pub fn is_allowed(&self) -> bool {
        self.status == VOICE_DEVICE_ALLOWED
    }
}

pub const VOICE_DEVICE_PENDING: &str = "pending";
pub const VOICE_DEVICE_ALLOWED: &str = "allowed";
pub const VOICE_DEVICE_REVOKED: &str = "revoked";

const VOICE_DEVICE_COLUMNS: &str =
    "device_id, client_id, status, name, model, firmware_version, last_ip, first_seen, last_seen, decided_at";

fn voice_device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VoiceDeviceRecord> {
    Ok(VoiceDeviceRecord {
        device_id: row.get(0)?,
        client_id: row.get(1)?,
        status: row.get(2)?,
        name: row.get(3)?,
        model: row.get(4)?,
        firmware_version: row.get(5)?,
        last_ip: row.get(6)?,
        first_seen: row.get(7)?,
        last_seen: row.get(8)?,
        decided_at: row.get(9)?,
    })
}

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
        // Attempt to wipe superseded credentials from freed SQLite cells.
        // Backups and historical WAL copies require separate secure handling.
        connection.pragma_update(None, "secure_delete", "ON")?;
        let db = Self { connection };
        db.migrate()?;
        db.migrate_legacy_home_token(chrono::Utc::now().timestamp_millis() as f64 / 1000.0)?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self, SchemaError> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "secure_delete", "ON")?;
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
        if current.unwrap_or(0) < 3 {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS app_secrets(
                   name TEXT PRIMARY KEY,
                   value TEXT NOT NULL,
                   updated_at REAL NOT NULL
                 );
                 INSERT INTO schema_version(version,applied_at) VALUES(3,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 4 {
            // 技术书 §7.1：桌面审批。`connection` 只在本地库保存创建审批时解析出的连接参数，
            // 接口与摘要不返回。`gateway_requests` 随可选网关（P2b）再加。
            // 表用不带 IF NOT EXISTS 的 CREATE：若已有同名但结构未知的表，迁移失败并整体回滚，
            // 不把半完成的 schema 当作可用。
            tx.execute_batch(
                "CREATE TABLE approvals(
                   approval_id TEXT PRIMARY KEY,
                   status TEXT NOT NULL,
                   kind TEXT NOT NULL,
                   target_record TEXT NOT NULL,
                   digest TEXT NOT NULL,
                   summary TEXT NOT NULL,
                   source TEXT NOT NULL,
                   session_id TEXT,
                   request_id TEXT,
                   connection TEXT,
                   created_at REAL NOT NULL,
                   decided_at REAL,
                   expires_at REAL NOT NULL,
                   updated_at REAL NOT NULL,
                   outcome TEXT,
                   UNIQUE(source, request_id)
                 );
                 CREATE INDEX IF NOT EXISTS approvals_status_expires ON approvals(status, expires_at);
                 CREATE INDEX IF NOT EXISTS audit_log_at ON audit_log(at DESC);
                 INSERT INTO schema_version(version,applied_at) VALUES(4,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 5 {
            // 审批批准后按原始参数重放执行并重算摘要（P1-07）；参数只在本地库保存，接口不返回。
            tx.execute_batch(
                "ALTER TABLE approvals ADD COLUMN request TEXT;
                 INSERT INTO schema_version(version,applied_at) VALUES(5,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 6 {
            // 内置语音服务（NV1）：设备登记与允许状态。设备 token 的签名密钥在 app_secrets。
            tx.execute_batch(
                "CREATE TABLE voice_devices(
                   device_id TEXT PRIMARY KEY,
                   client_id TEXT NOT NULL,
                   status TEXT NOT NULL,
                   name TEXT NOT NULL DEFAULT '',
                   model TEXT NOT NULL DEFAULT '',
                   firmware_version TEXT NOT NULL DEFAULT '',
                   last_ip TEXT NOT NULL DEFAULT '',
                   first_seen REAL NOT NULL,
                   last_seen REAL NOT NULL,
                   decided_at REAL
                 );
                 INSERT INTO schema_version(version,applied_at) VALUES(6,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 7 {
            tx.execute_batch(
                "CREATE TABLE voice_memory(
                    device_id TEXT PRIMARY KEY NOT NULL,
                    summary TEXT NOT NULL DEFAULT '',
                    revision INTEGER NOT NULL DEFAULT 0,
                    updated_at REAL NOT NULL
                 );
                 INSERT INTO schema_version(version,applied_at) VALUES(7,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 8 {
            tx.execute_batch(
                "CREATE TABLE voice_agent_runs(
                   id TEXT PRIMARY KEY,
                   approval_id TEXT NOT NULL UNIQUE,
                   provider TEXT NOT NULL,
                   workspace TEXT NOT NULL,
                   task_digest TEXT NOT NULL,
                   config_revision TEXT NOT NULL,
                   status TEXT NOT NULL,
                   output TEXT NOT NULL DEFAULT '',
                   truncated INTEGER NOT NULL DEFAULT 0,
                   started_at REAL NOT NULL,
                   finished_at REAL,
                   error_code TEXT
                 );
                 CREATE INDEX voice_agent_runs_started ON voice_agent_runs(started_at DESC);
                 INSERT INTO schema_version(version,applied_at) VALUES(8,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 9 {
            // NV16/NV17 durable metadata only. No real speaker recognizer or
            // OTA transport is enabled by the presence of these tables.
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS voice_speakers(
                    id TEXT PRIMARY KEY, name TEXT NOT NULL, model_id TEXT NOT NULL,
                    dimension INTEGER NOT NULL CHECK(dimension BETWEEN 64 AND 1024),
                    embedding BLOB NOT NULL, revision INTEGER NOT NULL DEFAULT 1,
                    created_at REAL NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS voice_person_memory(
                    speaker_id TEXT PRIMARY KEY REFERENCES voice_speakers(id) ON DELETE CASCADE,
                    summary TEXT NOT NULL DEFAULT '', revision INTEGER NOT NULL DEFAULT 0,
                    updated_at REAL NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS voice_firmware_delivery(
                    delivery_id TEXT PRIMARY KEY, device_id TEXT NOT NULL,
                    artifact_sha256 TEXT NOT NULL CHECK(length(artifact_sha256)=64),
                    download_status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(download_status IN ('pending','downloading','downloaded','unknown')),
                    device_report_status TEXT NOT NULL DEFAULT 'not_reported'
                        CHECK(device_report_status IN ('not_reported','reported')),
                    revision INTEGER NOT NULL DEFAULT 0, updated_at REAL NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS voice_firmware_device ON voice_firmware_delivery(device_id);
                 INSERT INTO schema_version(version,applied_at) VALUES(9,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 10 {
            // NV19-02：独立的长期记忆条目，不搬迁、不修改 NV10 的 voice_memory。
            // 取值约束与 moss-voice-server::context 的名称一致；删除只留无内容的墓碑，
            // 让迟到的后台任务因 revision 不符而失败，不能复活已删内容。
            // 不带 IF NOT EXISTS：同名但结构未知的表会让迁移整体回滚。
            tx.execute_batch(
                "CREATE TABLE context_memories(
                   id TEXT PRIMARY KEY NOT NULL CHECK(length(id) BETWEEN 1 AND 64),
                   scope_kind TEXT NOT NULL CHECK(scope_kind IN ('device','person','project','workspace')),
                   scope_id TEXT NOT NULL CHECK(length(scope_id) BETWEEN 1 AND 64),
                   category TEXT NOT NULL CHECK(category IN ('preference','device_fact','technical_note','task_context','troubleshooting_experience')),
                   content TEXT NOT NULL CHECK(length(content) <= 1000),
                   verification TEXT NOT NULL CHECK(verification IN ('candidate','confirmed','invalidated')),
                   source_kind TEXT NOT NULL CHECK(source_kind IN ('user_edit','conversation_extraction','legacy_nv10_import','document_index','trusted_test','agent_run_record')),
                   source_ref TEXT CHECK(source_ref IS NULL OR length(source_ref) BETWEEN 1 AND 128),
                   device_model TEXT CHECK(device_model IS NULL OR length(device_model) BETWEEN 1 AND 64),
                   firmware TEXT CHECK(firmware IS NULL OR (device_model IS NOT NULL AND length(firmware) BETWEEN 1 AND 64)),
                   revision INTEGER NOT NULL CHECK(revision >= 1),
                   created_at REAL NOT NULL,
                   updated_at REAL NOT NULL,
                   expires_at REAL,
                   deleted_at REAL,
                   CHECK((deleted_at IS NULL AND length(trim(content)) > 0) OR (deleted_at IS NOT NULL AND content = ''))
                 );
                 CREATE INDEX context_memories_scope ON context_memories(scope_kind,scope_id,category,verification,deleted_at);
                 CREATE INDEX context_memories_source ON context_memories(scope_kind,scope_id,source_ref);
                 CREATE TABLE context_extraction_runs(
                   id TEXT PRIMARY KEY NOT NULL,
                   scope_kind TEXT NOT NULL CHECK(scope_kind IN ('device','person','project','workspace')),
                   scope_id TEXT NOT NULL CHECK(length(scope_id) BETWEEN 1 AND 64),
                   started_at REAL NOT NULL,
                   finished_at REAL,
                   adds_json TEXT NOT NULL DEFAULT '[]',
                   updates_json TEXT NOT NULL DEFAULT '[]',
                   deletes_json TEXT NOT NULL DEFAULT '[]',
                   skipped_json TEXT NOT NULL DEFAULT '[]',
                   reverted_at REAL
                 );
                 CREATE INDEX context_extraction_runs_scope ON context_extraction_runs(scope_kind,scope_id,started_at DESC);
                 INSERT INTO schema_version(version,applied_at) VALUES(10,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 11 {
            // NV19-04：外部内容 FTS5 索引，trigram 分词支持中文子串检索（内置 SQLite 3.46.0）。
            // 触发器随插入 / 修改 / 删除同步；删除留下的墓碑正文为空，索引条目随之清掉。
            tx.execute_batch(
                "CREATE VIRTUAL TABLE context_memories_fts USING fts5(
                   content, content='context_memories', content_rowid='rowid', tokenize='trigram'
                 );
                 CREATE TRIGGER context_memories_fts_ai AFTER INSERT ON context_memories BEGIN
                   INSERT INTO context_memories_fts(rowid, content) VALUES (new.rowid, new.content);
                 END;
                 CREATE TRIGGER context_memories_fts_ad AFTER DELETE ON context_memories BEGIN
                   INSERT INTO context_memories_fts(context_memories_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
                 END;
                 CREATE TRIGGER context_memories_fts_au AFTER UPDATE OF content ON context_memories BEGIN
                   INSERT INTO context_memories_fts(context_memories_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
                   INSERT INTO context_memories_fts(rowid, content) VALUES (new.rowid, new.content);
                 END;
                 INSERT INTO context_memories_fts(context_memories_fts) VALUES ('rebuild');
                 INSERT INTO schema_version(version,applied_at) VALUES(11,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 12 {
            // NV19-05：用户明确选择的资料目录。只存目录级摘要（L0 ≤256 字、L1 ≤4000 字）与
            // 文件指纹；原文（L2）不入库，检索时按需从磁盘读并核对指纹。外键未开启，
            // 删除由 context_store 在同一事务内显式清理。
            tx.execute_batch(
                "CREATE TABLE context_knowledge_roots(
                   id TEXT PRIMARY KEY NOT NULL,
                   scope_kind TEXT NOT NULL CHECK(scope_kind IN ('project','workspace')),
                   scope_id TEXT NOT NULL CHECK(length(scope_id) BETWEEN 1 AND 64),
                   label TEXT NOT NULL CHECK(length(label) BETWEEN 1 AND 128),
                   root_path TEXT NOT NULL CHECK(length(root_path) BETWEEN 1 AND 1024),
                   revision INTEGER NOT NULL CHECK(revision >= 1),
                   created_at REAL NOT NULL,
                   indexed_at REAL,
                   UNIQUE(scope_kind, scope_id, root_path)
                 );
                 CREATE TABLE context_knowledge_dirs(
                   id TEXT PRIMARY KEY NOT NULL,
                   root_id TEXT NOT NULL,
                   rel_dir TEXT NOT NULL CHECK(length(rel_dir) <= 1024),
                   l0 TEXT NOT NULL CHECK(length(l0) BETWEEN 1 AND 256),
                   l1 TEXT NOT NULL CHECK(length(l1) BETWEEN 1 AND 4000),
                   file_count INTEGER NOT NULL CHECK(file_count >= 1),
                   UNIQUE(root_id, rel_dir)
                 );
                 CREATE TABLE context_knowledge_files(
                   id TEXT PRIMARY KEY NOT NULL,
                   root_id TEXT NOT NULL,
                   dir_id TEXT NOT NULL,
                   rel_path TEXT NOT NULL CHECK(length(rel_path) BETWEEN 1 AND 1024),
                   sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
                   size INTEGER NOT NULL CHECK(size > 0),
                   mtime REAL NOT NULL,
                   UNIQUE(root_id, rel_path)
                 );
                 CREATE INDEX context_knowledge_files_dir ON context_knowledge_files(dir_id);
                 CREATE VIRTUAL TABLE context_knowledge_fts USING fts5(
                   l0, l1, content='context_knowledge_dirs', content_rowid='rowid', tokenize='trigram'
                 );
                 CREATE TRIGGER context_knowledge_fts_ai AFTER INSERT ON context_knowledge_dirs BEGIN
                   INSERT INTO context_knowledge_fts(rowid, l0, l1) VALUES (new.rowid, new.l0, new.l1);
                 END;
                 CREATE TRIGGER context_knowledge_fts_ad AFTER DELETE ON context_knowledge_dirs BEGIN
                   INSERT INTO context_knowledge_fts(context_knowledge_fts, rowid, l0, l1) VALUES ('delete', old.rowid, old.l0, old.l1);
                 END;
                 CREATE TRIGGER context_knowledge_fts_au AFTER UPDATE ON context_knowledge_dirs BEGIN
                   INSERT INTO context_knowledge_fts(context_knowledge_fts, rowid, l0, l1) VALUES ('delete', old.rowid, old.l0, old.l1);
                   INSERT INTO context_knowledge_fts(rowid, l0, l1) VALUES (new.rowid, new.l0, new.l1);
                 END;
                 INSERT INTO schema_version(version,applied_at) VALUES(12,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 13 {
            // NV17 固件 OTA（2026-10-15）：导入的固件与逐台推送。令牌只用于这一次推送的下载地址。
            // 加列前先查表结构，重跑迁移（失败回滚后重试）时不会重复加列。
            let existing = {
                let mut statement = tx.prepare("SELECT name FROM pragma_table_info('voice_firmware_delivery')")?;
                let names = statement.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
                names
            };
            for (column, definition) in [
                ("firmware_version", "TEXT NOT NULL DEFAULT ''"),
                ("token", "TEXT"),
                ("expires_at", "REAL NOT NULL DEFAULT 0"),
            ] {
                if !existing.iter().any(|name| name == column) {
                    tx.execute_batch(&format!("ALTER TABLE voice_firmware_delivery ADD COLUMN {column} {definition};"))?;
                }
            }
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS voice_firmware_artifacts(
                   sha256 TEXT PRIMARY KEY NOT NULL CHECK(length(sha256)=64),
                   version TEXT NOT NULL CHECK(length(version) BETWEEN 1 AND 32),
                   project TEXT NOT NULL DEFAULT '' CHECK(length(project) <= 32),
                   size INTEGER NOT NULL CHECK(size > 0),
                   created_at REAL NOT NULL
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS voice_firmware_token ON voice_firmware_delivery(token) WHERE token IS NOT NULL;
                 INSERT INTO schema_version(version,applied_at) VALUES(13,unixepoch());"
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 已应用的最高 schema 版本。
    pub fn schema_version(&self) -> Result<i64, SchemaError> {
        let version: Option<i64> =
            self.connection
                .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))?;
        Ok(version.unwrap_or(0))
    }

    /// NV17 durable download journal. Startup recovery never treats an
    /// interrupted transfer as safely retryable or installed.
    pub fn recover_firmware_downloads(&self, now: f64) -> Result<usize, SchemaError> {
        let changed = self.connection.execute(
            "UPDATE voice_firmware_delivery SET download_status='unknown',revision=revision+1,updated_at=?1 WHERE download_status='downloading'",
            params![now],
        )?;
        Ok(changed)
    }

    /// Private desktop diagnostic only. A finished download is never a flash claim.
    pub fn list_firmware_deliveries(&self)->Result<Vec<VoiceFirmwareDeliveryRecord>,SchemaError>{
        let mut statement=self.connection.prepare("SELECT delivery_id,device_id,artifact_sha256,download_status,device_report_status,revision,updated_at,firmware_version,expires_at FROM voice_firmware_delivery ORDER BY updated_at DESC,delivery_id LIMIT 200")?;
        let rows=statement.query_map([],|r|Ok(VoiceFirmwareDeliveryRecord{delivery_id:r.get(0)?,device_id:r.get(1)?,artifact_sha256:r.get(2)?,download_status:r.get(3)?,device_report_status:r.get(4)?,revision:r.get(5)?,updated_at:r.get(6)?,firmware_version:r.get(7)?,expires_at:r.get(8)?}))?;
        rows.collect::<Result<Vec<_>,_>>().map_err(Into::into)
    }

    /// NV17 fail-closed conditional transition: callers cannot use completion
    /// of a download to assert that firmware was flashed. The device report
    /// must be separately authenticated by the upper layer.
    pub fn update_firmware_download_if_revision(
        &self, delivery_id: &str, expected_revision: i64,
        next: &str, now: f64,
    ) -> Result<bool, SchemaError> {
        if delivery_id.is_empty() || expected_revision < 0 || !now.is_finite() {
            return Err(SchemaError::InvalidRecord("固件记录或版本无效".into()));
        }
        let predecessor = match next {
            "downloading" => "pending",
            "downloaded" | "unknown" => "downloading",
            _ => return Err(SchemaError::InvalidRecord("不允许的下载状态迁移".into())),
        };
        Ok(self.connection.execute(
            "UPDATE voice_firmware_delivery SET download_status=?1, revision=revision+1, updated_at=?2 \
             WHERE delivery_id=?3 AND revision=?4 AND download_status=?5 AND device_report_status='not_reported'",
            params![next, now, delivery_id, expected_revision, predecessor],
        )? == 1)
    }

    /// NV16 internal enrollment from three independently extracted clips.
    /// IDs come from the host, never an LLM or a device's identity claim.
    pub fn enroll_voice_speaker(&self,id:&str,name:&str,model_id:&str,clips:&[Vec<f32>],now:f64)
        ->Result<VoiceSpeakerRecord,SchemaError>{
        let invalid=||SchemaError::InvalidRecord("声纹登记数据无效".into());
        if id.is_empty() || id.len()>64 || !id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_')
            || name.trim()!=name || name.is_empty() || name.chars().count()>64 || name.chars().any(char::is_control)
            || model_id!="campplus_sv_zh_cn_16k_common" || clips.len()!=3 || !now.is_finite(){return Err(invalid());}
        let normalize=|v:&[f32]|->Result<Vec<f32>,SchemaError>{
            if v.len()!=192 || v.iter().any(|n|!n.is_finite()){return Err(invalid());}
            let norm=v.iter().map(|n|f64::from(*n).powi(2)).sum::<f64>().sqrt();
            if !norm.is_finite() || norm<=1e-8{return Err(invalid());}
            Ok(v.iter().map(|n|(f64::from(*n)/norm) as f32).collect())
        };
        let vectors=clips.iter().map(|v|normalize(v)).collect::<Result<Vec<_>,_>>()?;
        for i in 0..3 {for j in i+1..3 {
            if vectors[i].iter().zip(&vectors[j]).map(|(a,b)|a*b).sum::<f32>()<0.70{return Err(invalid());}
        }}
        let mean=(0..192).map(|i|vectors.iter().map(|v|v[i]).sum::<f32>()/3.).collect::<Vec<_>>();
        let embedding=normalize(&mean)?.iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>();
        let tx=self.connection.unchecked_transaction()?;
        let (count,duplicate):(i64,i64)=tx.query_row("SELECT count(*),coalesce(sum(name=?1),0) FROM voice_speakers",[name],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if count>=64 || duplicate>0{return Err(invalid());}
        tx.execute("INSERT INTO voice_speakers(id,name,model_id,dimension,embedding,revision,created_at) VALUES(?1,?2,?3,192,?4,1,?5)",params![id,name,model_id,embedding,now])?;
        tx.execute("INSERT INTO voice_person_memory(speaker_id,summary,revision,updated_at) VALUES(?1,'',0,?2)",params![id,now])?;
        tx.commit()?;
        Ok(VoiceSpeakerRecord{id:id.into(),name:name.into(),model_id:model_id.into(),dimension:192,revision:1,created_at:now})
    }

    pub fn list_voice_speakers(&self)->Result<Vec<VoiceSpeakerRecord>,SchemaError>{
        let mut statement=self.connection.prepare("SELECT id,name,model_id,dimension,revision,created_at FROM voice_speakers ORDER BY created_at,id LIMIT 64")?;
        let rows=statement.query_map([],|r|Ok(VoiceSpeakerRecord{id:r.get(0)?,name:r.get(1)?,model_id:r.get(2)?,dimension:r.get(3)?,revision:r.get(4)?,created_at:r.get(5)?}))?;
        rows.collect::<Result<Vec<_>,_>>().map_err(Into::into)
    }
    pub fn native_voice_speaker_templates(&self)->Result<Vec<VoiceSpeakerTemplate>,SchemaError>{
        let mut statement=self.connection.prepare("SELECT id,revision,embedding FROM voice_speakers WHERE model_id='campplus_sv_zh_cn_16k_common' AND dimension=192 ORDER BY id LIMIT 65")?;
        let rows=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Vec<u8>>(2)?)))?;
        let mut result=Vec::new();
        for row in rows {
            let (id,revision,bytes)=row?;
            if bytes.len()!=768 || revision<1 || id.is_empty() || id.len()>64 || !id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {return Err(SchemaError::InvalidRecord("声纹记录损坏".into()));}
            let embedding=bytes.chunks_exact(4).map(|v|f32::from_le_bytes(v.try_into().expect("four bytes"))).collect::<Vec<_>>();
            let norm=embedding.iter().map(|v|f64::from(*v).powi(2)).sum::<f64>();
            if !norm.is_finite() || norm<=1e-16 || embedding.iter().any(|v|!v.is_finite()){return Err(SchemaError::InvalidRecord("声纹向量损坏".into()));}
            result.push(VoiceSpeakerTemplate{id,revision,embedding});
        }
        if result.len()>64{return Err(SchemaError::InvalidRecord("声纹库超过限额".into()));}
        Ok(result)
    }

    /// NV16 internal-only person memory storage. This API accepts a server-
    /// validated speaker ID, never a model-supplied identity claim.
    pub fn voice_person_summary(&self, speaker_id: &str) -> Result<Option<(String,i64)>, SchemaError> {
        self.connection.query_row(
            "SELECT m.summary,m.revision FROM voice_person_memory m JOIN voice_speakers s ON s.id=m.speaker_id WHERE s.id=?1",
            [speaker_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional().map_err(Into::into)
    }

    pub fn update_voice_person_summary_if_revision(&self, speaker_id: &str,
        expected_revision: i64, summary: &str, now: f64) -> Result<bool, SchemaError> {
        if speaker_id.is_empty() || expected_revision < 0 || !now.is_finite()
            || summary.chars().count() > 1500 || summary.chars().any(char::is_control) {
            return Err(SchemaError::InvalidRecord("个人摘要或版本无效".into()));
        }
        // Do not create an identity/memory row on reads or failed writes.
        Ok(self.connection.execute(
            "UPDATE voice_person_memory SET summary=?3, revision=revision+1, updated_at=?4 \
             WHERE speaker_id=?1 AND revision=?2 AND EXISTS(SELECT 1 FROM voice_speakers WHERE id=?1)",
            params![speaker_id, expected_revision, summary, now],
        )? == 1)
    }

    /// Explicit internal delete removes the entire identity and its personal
    /// summary in the same transaction, even when FK enforcement is disabled.
    pub fn delete_voice_speaker_if_revision(&self, speaker_id: &str,
        expected_revision: i64) -> Result<bool, SchemaError> {
        if speaker_id.is_empty() || expected_revision < 1 {
            return Err(SchemaError::InvalidRecord("说话人版本无效".into()));
        }
        let tx = self.connection.unchecked_transaction()?;
        let removed = tx.execute(
            "DELETE FROM voice_speakers WHERE id=?1 AND revision=?2",
            params![speaker_id, expected_revision],
        )? == 1;
        if removed {
            tx.execute("DELETE FROM voice_person_memory WHERE speaker_id=?1",[speaker_id])?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// 首次开启记忆时创建空摘要；重复调用不覆盖已有内容或 revision。
    pub fn ensure_voice_memory(&self, device_id: &str, now: f64) -> Result<VoiceMemoryRecord, SchemaError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO voice_memory(device_id,summary,revision,updated_at) VALUES(?1,'',0,?2)",
            params![device_id, now],
        )?;
        self.voice_memory(device_id)?.ok_or_else(|| SchemaError::InvalidRecord("记忆行创建失败".into()))
    }

    /// 不存在时返回 None；不能因为一次后台读就替设备悄悄开启记忆。
    pub fn voice_memory(&self, device_id: &str) -> Result<Option<VoiceMemoryRecord>, SchemaError> {
        self.connection.query_row(
            "SELECT device_id,summary,revision,updated_at FROM voice_memory WHERE device_id=?1",
            params![device_id],
            |row| Ok(VoiceMemoryRecord { device_id: row.get(0)?, summary: row.get(1)?, revision: row.get(2)?, updated_at: row.get(3)? }),
        ).optional().map_err(Into::into)
    }

    /// 所有记忆只读列表，不含原会话。
    pub fn voice_memories(&self) -> Result<Vec<VoiceMemoryRecord>, SchemaError> {
        let mut query = self.connection.prepare("SELECT device_id,summary,revision,updated_at FROM voice_memory ORDER BY updated_at DESC")?;
        let rows = query.query_map([], |row| Ok(VoiceMemoryRecord { device_id: row.get(0)?, summary: row.get(1)?, revision: row.get(2)?, updated_at: row.get(3)? }))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 用于后台总结和用户编辑/清空：只有旧 revision 一致才改变内容。
    /// 返回 None 说明用户或另一会话已编辑，调用方不能无条件重试覆盖。
    pub fn update_voice_memory_if_revision(&self, device_id: &str, expected_revision: i64, summary: &str, now: f64) -> Result<Option<VoiceMemoryRecord>, SchemaError> {
        if summary.chars().count() > 1500 { return Err(SchemaError::InvalidRecord("语音记忆最多 1500 字符".into())); }
        let count = self.connection.execute(
            "UPDATE voice_memory SET summary=?3,revision=revision+1,updated_at=?4 WHERE device_id=?1 AND revision=?2",
            params![device_id, expected_revision, summary, now],
        )?;
        if count == 0 { return Ok(None); }
        self.voice_memory(device_id)
    }

    /// 追加一条审计记录（技术书 §7.3）。`detail` 由 `AuditDetail` 序列化，不拼裸 JSON。
    /// 返回新行 id。调用方决定失败后的处理：R2 停止类动作不因审计失败而阻断，R3 阻断。
    pub fn append_audit(&self, entry: &AuditEntry) -> Result<i64, SchemaError> {
        let detail = serde_json::to_string(&entry.detail)?;
        self.connection.execute(
            "INSERT INTO audit_log(at, client, action, target, result, detail) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.at,
                entry.client,
                entry.action.as_str(),
                entry.target,
                entry.result.as_str(),
                detail
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// 最近的审计记录，新的在前。
    pub fn recent_audit(&self, limit: usize) -> Result<Vec<AuditEntry>, SchemaError> {
        let mut statement = self.connection.prepare(
            "SELECT at, client, action, target, result, detail FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |r| {
            Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let (at, client, action, target, result, detail) = row?;
            let action = AuditAction::parse(&action)
                .ok_or_else(|| SchemaError::InvalidRecord(format!("未知审计动作: {action}")))?;
            let result = AuditResult::parse(&result)
                .ok_or_else(|| SchemaError::InvalidRecord(format!("未知审计结果: {result}")))?;
            let detail: AuditDetail = serde_json::from_str(&detail)?;
            entries.push(AuditEntry { at, client, action, target, result, detail });
        }
        Ok(entries)
    }

    /// 写入一条新审批（技术书 §7.1）。`connection` 与 `outcome` 以 JSON 存入各自的列。
    pub fn insert_approval(&self, record: &ApprovalRecord) -> Result<(), SchemaError> {
        let connection = record.connection.as_ref().map(serde_json::to_string).transpose()?;
        let outcome = record.outcome.as_ref().map(serde_json::to_string).transpose()?;
        let request = record.request.as_ref().map(serde_json::to_string).transpose()?;
        self.connection.execute(
            &format!(
                "INSERT INTO approvals({APPROVAL_COLUMNS}) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
            ),
            params![
                record.approval_id,
                record.status.as_str(),
                record.kind.as_str(),
                record.target_record,
                record.digest,
                record.summary,
                record.source.as_str(),
                record.session_id,
                record.request_id,
                connection,
                record.created_at,
                record.decided_at,
                record.expires_at,
                record.updated_at,
                outcome,
                request
            ],
        )?;
        Ok(())
    }

    /// 按 ID 读取审批（含本地保存的连接参数）。
    pub fn get_approval(&self, approval_id: &str) -> Result<Option<ApprovalRecord>, SchemaError> {
        let row = self
            .connection
            .query_row(
                &format!("SELECT {APPROVAL_COLUMNS} FROM approvals WHERE approval_id=?1"),
                params![approval_id],
                ApprovalRow::read,
            )
            .optional()?;
        row.map(ApprovalRow::into_record).transpose()
    }

    /// 按幂等键 `(source, request_id)` 查找审批。
    pub fn find_approval_by_request(
        &self,
        source: ApprovalSource,
        request_id: &str,
    ) -> Result<Option<ApprovalRecord>, SchemaError> {
        let row = self
            .connection
            .query_row(
                &format!("SELECT {APPROVAL_COLUMNS} FROM approvals WHERE source=?1 AND request_id=?2"),
                params![source.as_str(), request_id],
                ApprovalRow::read,
            )
            .optional()?;
        row.map(ApprovalRow::into_record).transpose()
    }

    /// 列出审批，新的在前；`status` 为空时列出全部。
    pub fn list_approvals(
        &self,
        status: Option<ApprovalStatus>,
        limit: usize,
    ) -> Result<Vec<ApprovalRecord>, SchemaError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {APPROVAL_COLUMNS} FROM approvals \
             WHERE (?1 IS NULL OR status=?1) ORDER BY created_at DESC, approval_id DESC LIMIT ?2"
        ))?;
        let rows = statement.query_map(
            params![status.map(ApprovalStatus::as_str), limit as i64],
            ApprovalRow::read,
        )?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?.into_record()?);
        }
        Ok(records)
    }

    /// 条件迁移：只有当前状态仍为 `from` 时才改为 `to`（单条 UPDATE，原子）。
    /// 返回是否实际改动；不允许的迁移直接报错。进入 approved / rejected 时记录 `decided_at`；
    /// 给出 `outcome` 时一并写入。
    pub fn transition_approval(
        &self,
        approval_id: &str,
        from: ApprovalStatus,
        to: ApprovalStatus,
        now: f64,
        outcome: Option<&ApprovalOutcome>,
    ) -> Result<bool, SchemaError> {
        if !from.can_transition_to(to) {
            return Err(SchemaError::InvalidRecord(format!(
                "审批状态不允许从 {} 改为 {}",
                from.as_str(),
                to.as_str()
            )));
        }
        let decided = matches!(to, ApprovalStatus::Approved | ApprovalStatus::Rejected);
        let outcome = outcome.map(serde_json::to_string).transpose()?;
        let changed = self.connection.execute(
            "UPDATE approvals SET status=?1, updated_at=?2, \
               decided_at=CASE WHEN ?3 THEN ?2 ELSE decided_at END, \
               outcome=COALESCE(?4, outcome) \
             WHERE approval_id=?5 AND status=?6",
            params![to.as_str(), now, decided, outcome, approval_id, from.as_str()],
        )?;
        Ok(changed == 1)
    }

    /// 单次消费（技术书 §5.5）：仅当状态为 approved 且未过期时改为 executed。
    /// 单条 UPDATE 在 SQLite 中是原子的，两个并发执行最多一个成功。
    pub fn consume_approval(&self, approval_id: &str, now: f64) -> Result<bool, SchemaError> {
        let changed = self.connection.execute(
            "UPDATE approvals SET status='executed', updated_at=?1 \
             WHERE approval_id=?2 AND status='approved' AND expires_at > ?1",
            params![now, approval_id],
        )?;
        Ok(changed == 1)
    }

    /// 为已执行（或已失败）的审批写入执行结果摘要。
    pub fn set_approval_outcome(
        &self,
        approval_id: &str,
        outcome: &ApprovalOutcome,
        now: f64,
    ) -> Result<bool, SchemaError> {
        let outcome = serde_json::to_string(outcome)?;
        let changed = self.connection.execute(
            "UPDATE approvals SET outcome=?1, updated_at=?2 \
             WHERE approval_id=?3 AND status IN ('executed','failed')",
            params![outcome, now, approval_id],
        )?;
        Ok(changed == 1)
    }

    /// 将被 [`Self::expire_approvals`] 标为过期的审批（同样条件），供逐条记审计。
    pub fn approvals_due_for_expiry(&self, now: f64, all_open: bool) -> Result<Vec<ApprovalRecord>, SchemaError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {APPROVAL_COLUMNS} FROM approvals \
             WHERE status IN ('pending','approved') AND (?2 OR expires_at <= ?1) ORDER BY created_at"
        ))?;
        let rows = statement.query_map(params![now, all_open], ApprovalRow::read)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?.into_record()?);
        }
        Ok(records)
    }

    /// 把仍可执行的审批（pending / approved）标为 expired。`all_open=true` 用于进程重启
    /// （技术书 §5.5：重启后全部过期）；否则只处理已到期的。返回改动行数。
    pub fn expire_approvals(&self, now: f64, all_open: bool) -> Result<usize, SchemaError> {
        let changed = self.connection.execute(
            "UPDATE approvals SET status='expired', updated_at=?1 \
             WHERE status IN ('pending','approved') AND (?2 OR expires_at <= ?1)",
            params![now, all_open],
        )?;
        Ok(changed)
    }

    /// 记录一次设备来访（OTA 或 WebSocket）。新设备登记为待允许；
    /// **`client_id` 与已登记的不同**时，已允许的设备退回待允许（防止他人冒用 MAC 取得 token），已撤销的保持撤销。
    pub fn voice_device_seen(
        &self,
        device_id: &str,
        client_id: &str,
        model: &str,
        firmware_version: &str,
        last_ip: &str,
        now: f64,
    ) -> Result<VoiceDeviceRecord, SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        let existing = tx
            .query_row(
                &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
                [device_id],
                voice_device_from_row,
            )
            .optional()?;
        match existing {
            None => {
                tx.execute(
                    "INSERT INTO voice_devices(device_id, client_id, status, model, firmware_version, last_ip, first_seen, last_seen)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![device_id, client_id, VOICE_DEVICE_PENDING, model, firmware_version, last_ip, now],
                )?;
            }
            // 同名 Device-Id 的未认证来访不能替换已绑定的 Client-Id，
            // 更不能把已允许设备降级并撤销其正在执行的合法会话。
            Some(record) if record.client_id != client_id => {
                tx.commit()?;
                return Ok(record);
            }
            Some(record) => {
                let keep = |new: &str, old: String| if new.is_empty() { old } else { new.to_owned() };
                tx.execute(
                    "UPDATE voice_devices SET client_id=?2, status=?3, model=?4, firmware_version=?5, last_ip=?6, last_seen=?7
                     WHERE device_id=?1",
                    params![
                        device_id,
                        client_id,
                        record.status,
                        keep(model, record.model),
                        keep(firmware_version, record.firmware_version),
                        keep(last_ip, record.last_ip),
                        now
                    ],
                )?;
            }
        }
        tx.execute("UPDATE approvals SET status='expired', updated_at=?2 WHERE source='voice' AND session_id=?1 AND status IN ('pending','approved') AND EXISTS (SELECT 1 FROM voice_devices WHERE device_id=?1 AND status<>'allowed')", params![device_id, now])?;
        let record = tx.query_row(
            &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
            [device_id],
            voice_device_from_row,
        )?;
        tx.commit()?;
        Ok(record)
    }

    pub fn voice_device(&self, device_id: &str) -> Result<Option<VoiceDeviceRecord>, SchemaError> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
                [device_id],
                voice_device_from_row,
            )
            .optional()?)
    }

    /// 全部登记设备，最近来访的在前。
    pub fn voice_devices(&self) -> Result<Vec<VoiceDeviceRecord>, SchemaError> {
        let mut statement = self
            .connection
            .prepare(&format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices ORDER BY last_seen DESC"))?;
        let rows = statement.query_map([], voice_device_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 桌面端允许或撤销设备；返回是否找到该设备。
    pub fn set_voice_device_status(&self, device_id: &str, status: &str, now: f64) -> Result<bool, SchemaError> {
        if ![VOICE_DEVICE_PENDING, VOICE_DEVICE_ALLOWED, VOICE_DEVICE_REVOKED].contains(&status) {
            return Err(SchemaError::InvalidRecord(format!("未知的设备状态: {status}")));
        }
        let tx = self.connection.unchecked_transaction()?;
        let found = tx.execute(
            "UPDATE voice_devices SET status=?2, decided_at=?3 WHERE device_id=?1",
            params![device_id, status, now],
        )? > 0;
        if found && status != VOICE_DEVICE_ALLOWED {
            tx.execute("UPDATE approvals SET status='expired', updated_at=?2 WHERE source='voice' AND session_id=?1 AND status IN ('pending','approved')", params![device_id, now])?;
        }
        tx.commit()?;
        Ok(found)
    }

    /// 修改设备显示名（最长 32 字）。
    pub fn rename_voice_device(&self, device_id: &str, name: &str) -> Result<bool, SchemaError> {
        let name: String = name.trim().chars().take(32).collect();
        Ok(self.connection.execute("UPDATE voice_devices SET name=?2 WHERE device_id=?1", params![device_id, name])? > 0)
    }

    /// NV15 desktop-owned HA token store, separate from voice JSON settings.
    /// The caller never receives the bearer token, only its presence and
    /// monotonic revision. Token changes atomically invalidate HA approvals.
    /// Read ONLY the optimistic revision for confirmed safe-template recovery.
    /// Unlike home_token_status, this does not decrypt an old DPAPI blob,
    /// so a foreign Windows-user copy can be safely revoked and replaced.
    /// Never use this as proof a token is configured or valid.
    pub fn home_token_recovery_revision(&self) -> Result<u64, SchemaError> {
        self.get_secret("voice.homeassistant.token_revision")?
            .map(|raw| raw.parse::<u64>().ok().filter(|v| *v <= 9_007_199_254_740_991)
                 .ok_or_else(|| SchemaError::InvalidRecord("HA 令牌版本损坏".into())))
            .transpose().map(|revision| revision.unwrap_or(0))
    }
    pub fn home_token_status(&self) -> Result<(bool, u64), SchemaError> {
        let revision = self.home_token_recovery_revision()?;
        let configured = match self.get_secret("voice.homeassistant.token")? {
            None => false,
            Some(value) if crate::home_secret::is_sealed(&value) => {
                // A malformed envelope fails closed, not a configured usable token.
                crate::home_secret::open(&value).map_err(|_| SchemaError::InvalidRecord("HA 令牌加密数据无效".into()))?;
                true
            }
            Some(_) => return Err(SchemaError::InvalidRecord("检测到旧版明文 HA 凭据，需安全迁移".into())),
        };
        Ok((configured,revision))
    }

    pub fn update_home_token_if_revision(&self, expected_revision: u64,
        replacement: Option<&str>, now: f64) -> Result<Option<u64>, SchemaError> {
        if expected_revision > 9_007_199_254_740_990 || !now.is_finite() || now < 0.0 {
            return Err(SchemaError::InvalidRecord("HA 令牌版本或时间无效".into()));
        }
        if let Some(token) = replacement {
            if token.len() < 20 || token.len() > 8192 || token.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
                return Err(SchemaError::InvalidRecord("HA 访问令牌格式无效".into()));
            }
        }
        let sealed = replacement.map(|token|crate::home_secret::seal(token)
            .map_err(|_|SchemaError::InvalidRecord("无法使用系统安全存储保护 HA 令牌".into())))
            .transpose()?;
        // BEGIN IMMEDIATE acquires the SQLite write reservation *before* the
        // revision read. With multiple connections, the loser waits then sees
        // the incremented revision (conflict) instead of SQLITE_BUSY_SNAPSHOT.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection, rusqlite::TransactionBehavior::Immediate)?;
        let raw: Option<String> = tx.query_row(
            "SELECT value FROM app_secrets WHERE name='voice.homeassistant.token_revision'",[],|r|r.get(0)
        ).optional()?;
        let current = raw.map(|raw|raw.parse::<u64>().ok()
            .filter(|v| *v<=9_007_199_254_740_991)
            .ok_or_else(||SchemaError::InvalidRecord("HA 令牌版本损坏".into())))
            .transpose()?.unwrap_or(0);
        if current != expected_revision { return Ok(None); }
        let next=current.checked_add(1)
            .filter(|v| *v<=9_007_199_254_740_991)
            .ok_or_else(||SchemaError::InvalidRecord("HA 令牌版本上限".into()))?;
        match replacement {
            Some(_) => {
                let sealed = sealed.as_ref().ok_or_else(||SchemaError::InvalidRecord("HA 令牌密封失败".into()))?;
                tx.execute("INSERT INTO app_secrets(name,value,updated_at) VALUES('voice.homeassistant.token',?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![sealed,now])?;
            }
            None => {tx.execute("DELETE FROM app_secrets WHERE name='voice.homeassistant.token'",[])?;}
        }
        tx.execute("INSERT INTO app_secrets(name,value,updated_at) VALUES('voice.homeassistant.token_revision',?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![next.to_string(),now])?;
        // Both voice-read and HomeControl grants were bound to the old
        // credential revision. Delete them in the same transaction as the
        // token rotation; re-entering the old value never restores authority.
        tx.execute("DELETE FROM app_secrets WHERE name IN ('voice.homeassistant.voice_read_consent','voice.homeassistant.voice_control_consent')", [])?;
        // A token can be replaced without changing the entity policy, but all
        // outstanding HomeControl approvals must be made stale immediately.
        tx.execute("UPDATE approvals SET status='expired',updated_at=?1 WHERE kind='home_control' AND status IN ('pending','approved')",params![now])?;
        tx.commit()?;
        Ok(Some(next))
    }

    /// Only the trusted internal HA connector may request decrypted credentials.
    /// Do not expose this through the Tauri command or general settings APIs.
    pub fn home_token_for_connection(&self) -> Result<Option<String>, SchemaError> {
        let Some(sealed) = self.get_secret("voice.homeassistant.token")? else { return Ok(None); };
        crate::home_secret::open(&sealed)
            .map(Some)
            .map_err(|_|SchemaError::InvalidRecord("Home Assistant 令牌无法解密或已损坏".into()))
    }

    /// One-time upgrade for an existing plaintext HA token saved by the older
    /// prototype. Runs in one SQLite transaction and bumps the credential
    /// revision, expiring pending/approved HomeControl approvals. Existing
    /// external/old backups must be protected or deleted separately by owner.
    pub fn migrate_legacy_home_token(&self, now: f64) -> Result<bool, SchemaError> {
        if !now.is_finite() || now < 0.0 {
            return Err(SchemaError::InvalidRecord("令牌迁移时间无效".into()));
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection, rusqlite::TransactionBehavior::Immediate)?;
        let original: Option<String> = tx.query_row(
            "SELECT value FROM app_secrets WHERE name='voice.homeassistant.token'",[],|row|row.get(0)
        ).optional()?;
        let Some(original) = original else { return Ok(false); };
        if crate::home_secret::is_sealed(&original) {
            // A sealed blob copied from another Windows principal may be
            // impossible to decrypt. Never fail the *entire database startup*:
            // home_token_status/for_connection must still reject the blob,
            // and ONLY explicitly confirmed credential-revoking migration may
            // clear it with an expected revision under BEGIN IMMEDIATE.
            // Do not unseal, reencrypt or grant any HomeControl permission.
            return Ok(false);
        }
        if original.len()<20 || original.len()>8192 ||
            original.bytes().any(|b|b.is_ascii_whitespace() || b.is_ascii_control()) {
            return Err(SchemaError::InvalidRecord("旧版明文令牌格式不合法，拒绝迁移".into()));
        }
        let cipher=crate::home_secret::seal(&original)
            .map_err(|_|SchemaError::InvalidRecord("DPAPI 旧凭据迁移失败".into()))?;
        let old:Option<String>=tx.query_row(
            "SELECT value FROM app_secrets WHERE name='voice.homeassistant.token_revision'",[],|row|row.get(0)
        ).optional()?;
        let current=old.as_deref().unwrap_or("0").parse::<u64>()
            .ok().filter(|v|*v<9_007_199_254_740_991)
            .ok_or_else(||SchemaError::InvalidRecord("旧 HA 凭据版本损坏".into()))?;
        tx.execute("UPDATE app_secrets SET value=?1,updated_at=?2 WHERE name='voice.homeassistant.token'",params![cipher,now])?;
        tx.execute("INSERT INTO app_secrets(name,value,updated_at) VALUES('voice.homeassistant.token_revision',?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![(current+1).to_string(),now])?;
        tx.execute("DELETE FROM app_secrets WHERE name IN ('voice.homeassistant.voice_read_consent','voice.homeassistant.voice_control_consent')", [])?;
        tx.execute("UPDATE approvals SET status='expired',updated_at=?1 WHERE kind='home_control' AND status IN ('pending','approved')",params![now])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn get_secret(&self, name: &str) -> Result<Option<String>, SchemaError> {
        self.connection
            .query_row(
                "SELECT value FROM app_secrets WHERE name=?1",
                [name],
                |row| row.get(0),
            )
            .optional()
            .map_err(SchemaError::from)
    }

    pub fn set_secret(&self, name: &str, value: &str, now: f64) -> Result<(), SchemaError> {
        self.connection.execute(
            "INSERT INTO app_secrets(name,value,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(name) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![name, value, now],
        )?;
        Ok(())
    }

    pub fn delete_secret(&self, name: &str) -> Result<bool, SchemaError> {
        Ok(self
            .connection
            .execute("DELETE FROM app_secrets WHERE name=?1", [name])?
            > 0)
    }

    /// 在一个事务内写入（`Some`）或删除（`None`）多项密钥，任一项失败整批不生效。
    /// 返回每项修改前的值，调用方后续步骤（如写 `settings.json`）失败时交给 [`Self::restore_secrets`] 还原。
    pub fn apply_secret_changes(
        &self,
        changes: &[(String, Option<String>)],
        now: f64,
    ) -> Result<Vec<(String, Option<String>)>, SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        let mut previous = Vec::with_capacity(changes.len());
        for (name, value) in changes {
            let old: Option<String> = tx
                .query_row("SELECT value FROM app_secrets WHERE name=?1", [name], |row| row.get(0))
                .optional()?;
            match value {
                Some(value) => tx.execute(
                    "INSERT INTO app_secrets(name,value,updated_at) VALUES(?1,?2,?3)
                     ON CONFLICT(name) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                    params![name, value, now],
                )?,
                None => tx.execute("DELETE FROM app_secrets WHERE name=?1", [name])?,
            };
            previous.push((name.clone(), old));
        }
        tx.commit()?;
        Ok(previous)
    }

    /// 按 [`Self::apply_secret_changes`] 返回的原值还原（逆序应用，同名多次修改也回到批次前的值）。
    pub fn restore_secrets(&self, previous: &[(String, Option<String>)], now: f64) -> Result<(), SchemaError> {
        let reversed: Vec<_> = previous.iter().rev().cloned().collect();
        self.apply_secret_changes(&reversed, now).map(|_| ())
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
        // 记录正文里的 status 与列保持一致：状态查询返回的是正文（2026-10-06 修复：此前只改列，
        // 重启后查询仍显示“发送中”）
        self.connection.execute(
            "UPDATE workflows SET status='failed', updated_at=unixepoch(),
             payload=json_set(payload,'$.status','failed','$.detail','host 重启中断，任务安全终止')
             WHERE status IN ('sending', 'processing')",
            [],
        )?;
        // 标定矩阵与 workflow 走同一条发送链路，发送中被中断同样记为失败、结果未知，不自动重发
        self.connection.execute(
            "UPDATE calibrations SET status='failed', updated_at=unixepoch(),
             payload=json_set(payload,'$.status','failed','$.detail','host 重启中断，任务安全终止')
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
