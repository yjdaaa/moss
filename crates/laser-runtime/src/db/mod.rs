//! 运行时 SQLite 数据库 `RuntimeDb`：共享类型、打开连接、事务与只读引用查询。
//! 各业务方法按领域拆分到子模块，各自追加 `impl RuntimeDb`：
//! - `migrate`：schema 建表、逐版本迁移与内置材料种子；
//! - `materials`：材料库删除、导出与 JSON 导入；
//! - `approvals`：桌面审批与审计日志；
//! - `voice`：语音设备、快照、声纹、记忆、对话记录与固件交付记录；
//! - `secrets`：本机密钥与 Home Assistant 令牌；
//! - `maintenance`：重启恢复、退出收尾与历史记录清理。

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod approvals;
mod maintenance;
mod materials;
mod migrate;
mod secrets;
mod voice;

pub use materials::{MaterialConflictPolicy, MaterialImportCounts, parse_param_cell_from_value};
pub use voice::DeleteVoiceDeviceOutcome;

/// 当前 schema 版本（v4：桌面审批 `approvals` 表与审计索引；v5：`approvals.request` 执行参数列，技术书 §7；
/// v6：内置语音服务的设备登记表 `voice_devices`，内置语音技术书 NV1）。
/// v7：按设备存储用户可编辑的语音摘要（NV10），带 revision 防后台旧总结覆盖。
/// v8：按 AgentRun 审批记录可恢复的有界只读 CLI 历史（NV13）。
/// v9：NV16 声纹登记与按人摘要、NV17 固件交付记录。
/// v10：NV19 结构化长期记忆 `context_memories` 与提取变更清单 `context_extraction_runs`。
/// v11：NV19-04 记忆正文的 FTS5 trigram 全文索引 `context_memories_fts`（触发器同步）。
/// v12：NV19-05 分层知识库：资料根目录、目录 L0/L1 摘要与文件指纹，目录摘要 FTS5 索引。
/// v14：小智设备信息快照（`voice_devices` 的 info/hello/status 原始 JSON 与时间）。
/// v15：设备实际使用的拾音模式（`voice_devices.listen_json` / `listen_at`）。
/// v16：资料目录单独启用 / 停用（`context_knowledge_roots.enabled`）。
/// v17：NV19 记忆取代链与复现计数（`context_memories.supersedes/seen_count/last_seen_at`），
/// 资料段落级索引（`context_knowledge_chunks` + 无正文 FTS5 `context_knowledge_chunks_fts`）。
/// v18：删除 7 张日常无读写废弃表（`ai_state`、`gui_calibration`、`auth_pairings` 及 4 张未用 `*_history`），
/// 补齐 `workflows`、`approvals`、`material_aliases` 高频查询索引。
/// v19（2026-11-12 n81）：新增有界语音对话记录表 `voice_chat_logs`（说了什么、工具调用、回了什么，上限 500 条）。
/// v20（2026-11-18）：`voice_devices` 增加 `status_revision` 递增修订号，用于 USB 配网并发撤销 CAS 校验。
/// v21（2026-11-27 VA）：`voice_chat_logs` 增加 `simulated` 模拟轮次标记，区分真实语音与文字直通联调轮次。
/// v22（2026-11-27 VC1）：`voice_chat_logs` 增加 `bench_window` 测试时间窗标记，区分“模拟（整体测试）”与文字直通“模拟”。
pub const SCHEMA_VERSION: i64 = 22;

const AUDIT_LOG_MAX_ROWS: i64 = 2000;
pub const VOICE_CHAT_LOGS_MAX_ROWS: i64 = 500;

/// n81 本机持久化的语音对话记录（单轮识别原话、纠偏文本、工具调用 JSON、完整回复与实际已播句子）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceChatLogRecord {
    pub id: i64,
    pub at_ms: i64,
    pub device_id: String,
    pub session_id: String,
    pub turn: i64,
    pub raw_asr: String,
    pub corrected_asr: String,
    pub reply: String,
    pub spoken: String,
    pub tools_json: String,
    pub outcome: String,
    pub truncated: bool,
    pub tools_dropped: i64,
    #[serde(default)]
    pub simulated: bool,
    #[serde(default)]
    pub bench_window: bool,
}

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
    pub id: String,
    pub name: String,
    pub model_id: String,
    pub dimension: u32,
    pub revision: i64,
    pub created_at: f64,
}
/// Internal-only biometric material; intentionally not Serialize/Deserialize.
pub struct VoiceSpeakerTemplate {
    pub id: String,
    pub revision: i64,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceFirmwareDeliveryRecord {
    pub delivery_id: String,
    pub device_id: String,
    pub artifact_sha256: String,
    pub download_status: String,
    pub device_report_status: String,
    pub revision: i64,
    pub updated_at: f64,
    /// 推送的固件版本与推送到期时间（schema 13；旧记录为空串 / 0）
    pub firmware_version: String,
    pub expires_at: f64,
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
    /// 设备快照（schema 14）：开机 OTA 上报的原始 JSON（固件版本、芯片、flash/psram、MAC 等）
    pub info_json: Option<String>,
    pub info_at: Option<f64>,
    /// 连接握手 `hello` 的快照（协议版本、传输方式、音频参数、`features.aec`）
    pub hello_json: Option<String>,
    pub hello_at: Option<f64>,
    /// 最近一次 `self.get_device_status` 的返回（音量、屏幕、电池、网络）
    pub status_json: Option<String>,
    pub status_at: Option<f64>,
    /// 最近一次实际拾音模式（schema 15）：`{"mode":"realtime"}`；固件在设备端 AEC 开启时才选 realtime
    pub listen_json: Option<String>,
    pub listen_at: Option<f64>,
    /// 设备状态与绑定修订号（schema 20）：每次登记、允许/撤销或换绑递增，用于拦截 USB 并发撤销覆盖
    #[serde(default)]
    pub status_revision: i64,
}

impl VoiceDeviceRecord {
    pub fn is_allowed(&self) -> bool {
        self.status == VOICE_DEVICE_ALLOWED
    }
}

pub const VOICE_DEVICE_PENDING: &str = "pending";
pub const VOICE_DEVICE_ALLOWED: &str = "allowed";
pub const VOICE_DEVICE_REVOKED: &str = "revoked";

/// 设备快照种类（schema 14）：`info` 开机 OTA 上报、`hello` 连接握手、`status` 实时状态查询。
pub const VOICE_SNAPSHOT_INFO: &str = "info";
pub const VOICE_SNAPSHOT_HELLO: &str = "hello";
pub const VOICE_SNAPSHOT_STATUS: &str = "status";

/// 快照 JSON 上限：开机上报含分区表，8 KB 足够；更长的内容不写库（避免把设备上报当存储用）。
/// 设备实际使用的拾音模式（`auto` / `manual` / `realtime`）：判断设备端 AEC 是否开启的行为证据。
pub const VOICE_SNAPSHOT_LISTEN: &str = "listen";

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
    pub(crate) connection: Connection,
    path: Option<std::path::PathBuf>,
}

impl RuntimeDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SchemaError> {
        let db_path = path.as_ref().to_path_buf();
        let connection = Connection::open(&db_path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        // Attempt to wipe superseded credentials from freed SQLite cells.
        // Backups and historical WAL copies require separate secure handling.
        connection.pragma_update(None, "secure_delete", "ON")?;
        let db = Self {
            connection,
            path: Some(db_path),
        };
        db.migrate()?;
        db.migrate_legacy_home_token(chrono::Utc::now().timestamp_millis() as f64 / 1000.0)?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self, SchemaError> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        let db = Self {
            connection,
            path: None,
        };
        db.migrate()?;
        Ok(db)
    }

    /// 打开指向同一 WAL 数据库文件的独立只读连接（内存库返回 `Ok(None)`），
    /// 供高频轮询查询分流，避免后台大事务写入时阻塞前端只读状态刷新。
    pub fn open_readonly(&self) -> Result<Option<Self>, SchemaError> {
        let Some(ref db_path) = self.path else {
            return Ok(None);
        };
        let connection = Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "query_only", "ON")?;
        Ok(Some(Self {
            connection,
            path: Some(db_path.clone()),
        }))
    }

    /// 在单个 SQLite 事务内执行闭包；闭包返回 `Ok` 提交，返回 `Err` 自动回滚。
    pub fn with_transaction<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Self) -> Result<T, E>,
        E: From<SchemaError>,
    {
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(SchemaError::from)?;
        let value = f(self)?;
        tx.commit().map_err(SchemaError::from)?;
        Ok(value)
    }

    /// 只读查询任务引用的实时状态与阶段（优先 workflows → calibrations → 旧 jobs），
    /// 退出/重启中断的在途任务统一映射为 `result_unknown`。
    pub fn lookup_job_ref_state(&self, id: &str) -> Option<(String, Option<String>)> {
        let row = |sql: &str| -> Option<(String, Option<String>)> {
            self.connection
                .query_row(sql, [id], |r| {
                    let status: String = r.get(0)?;
                    let phase: Option<String> = r.get(1)?;
                    let detail: Option<String> = r.get(2)?;
                    let running: Option<i64> = r.get(3).ok().flatten();
                    let effective_phase = phase.or_else(|| {
                        let interrupted = detail
                            .as_deref()
                            .is_some_and(|d| d.contains("重启中断") || d.contains("退出中断"))
                            || running == Some(1);
                        (status == "failed" && interrupted).then(|| "result_unknown".to_owned())
                    });
                    Ok((status, effective_phase))
                })
                .ok()
        };
        row(
            "SELECT status, json_extract(payload,'$.send_result.phase'), json_extract(payload,'$.detail'), json_extract(payload,'$.send_result.running') FROM workflows WHERE workflow_id=?1",
        )
        .or_else(|| {
            row(
                "SELECT status, json_extract(payload,'$.send_result.phase'), json_extract(payload,'$.detail'), json_extract(payload,'$.send_result.running') FROM calibrations WHERE calibration_id=?1",
            )
        })
        .or_else(|| {
            row(
                "SELECT status, COALESCE(json_extract(payload,'$.send_result.phase'), json_extract(payload,'$.result.phase')), COALESCE(json_extract(payload,'$.detail'), json_extract(payload,'$.result.detail')), NULL FROM jobs WHERE job_id=?1",
            )
        })
    }

    /// 只读查询审批引用的状态与阶段。
    pub fn lookup_approval_ref_state(&self, id: &str) -> Option<(String, Option<String>)> {
        self.connection
            .query_row(
                "SELECT status, json_extract(outcome,'$.phase') FROM approvals WHERE approval_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }

    /// 只读查询 AgentRun 引用的状态。
    pub fn lookup_agent_run_ref_state(&self, id: &str) -> Option<(String, Option<String>)> {
        self.connection
            .query_row(
                "SELECT status, NULL FROM voice_agent_runs WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
    }

    /// 仅供跨 crate 与本 crate 集成测试访问底层 SQLite 连接；严禁出现在正式依赖中。
    #[cfg(any(test, feature = "test-sql-access"))]
    pub fn test_connection(&self) -> &Connection {
        &self.connection
    }

    /// 仅供跨 crate 测试：执行 SQL 批处理（如故障注入触发器/构造历史行）。
    #[cfg(any(test, feature = "test-sql-access"))]
    pub fn test_execute_batch(&self, sql: &str) -> Result<(), SchemaError> {
        self.connection.execute_batch(sql)?;
        Ok(())
    }

    /// 仅供跨 crate 测试：查询单值整数。
    #[cfg(any(test, feature = "test-sql-access"))]
    pub fn test_query_i64(&self, sql: &str) -> Result<i64, SchemaError> {
        Ok(self.connection.query_row(sql, [], |r| r.get(0))?)
    }

    /// 仅供跨 crate 测试：查询单值文本。
    #[cfg(any(test, feature = "test-sql-access"))]
    pub fn test_query_string(&self, sql: &str) -> Result<String, SchemaError> {
        Ok(self.connection.query_row(sql, [], |r| r.get(0))?)
    }

    /// 仅供跨 crate 测试：查询多行文本列。
    #[cfg(any(test, feature = "test-sql-access"))]
    pub fn test_query_strings(&self, sql: &str) -> Result<Vec<String>, SchemaError> {
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 仅供跨 crate 测试：按单参数查询两列文本。
    #[cfg(any(test, feature = "test-sql-access"))]
    pub fn test_query_string_pair(
        &self,
        sql: &str,
        param: &str,
    ) -> Result<(String, String), SchemaError> {
        Ok(self
            .connection
            .query_row(sql, [param], |r| Ok((r.get(0)?, r.get(1)?)))?)
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
        let mut stmt = self.connection.prepare_cached(&sql)?;
        let raw: Option<String> = stmt.query_row([id], |r| r.get(0)).optional()?;
        raw.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(SchemaError::from)
    }
}
