//! schema 建表与逐版本迁移、内置材料种子数据。

use rusqlite::params;

use laser_contracts::builtin_material_library;

use super::{RuntimeDb, SchemaError};

impl RuntimeDb {
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
             CREATE TABLE IF NOT EXISTS uploads(upload_id TEXT PRIMARY KEY, path TEXT NOT NULL, sha256 TEXT NOT NULL, size_bytes INTEGER NOT NULL, mime TEXT, created_at REAL NOT NULL);
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
                 CREATE INDEX IF NOT EXISTS workflows_status_updated ON workflows(status,updated_at DESC);
                 CREATE INDEX IF NOT EXISTS workflows_source ON workflows(source_type);
                 CREATE INDEX IF NOT EXISTS workflows_material ON workflows(material,thickness_mm);
                 CREATE INDEX IF NOT EXISTS jobs_updated ON jobs(updated_at DESC);
                 CREATE INDEX IF NOT EXISTS text_tasks_updated ON text_tasks(updated_at DESC);
                 CREATE INDEX IF NOT EXISTS calibrations_updated ON calibrations(updated_at DESC);",
            )?;
            tx.execute(
                "INSERT INTO schema_version(version,applied_at) VALUES(2,unixepoch())",
                [],
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
                 INSERT INTO schema_version(version,applied_at) VALUES(8,unixepoch());",
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
                let mut statement =
                    tx.prepare("SELECT name FROM pragma_table_info('voice_firmware_delivery')")?;

                statement
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for (column, definition) in [
                ("firmware_version", "TEXT NOT NULL DEFAULT ''"),
                ("token", "TEXT"),
                ("expires_at", "REAL NOT NULL DEFAULT 0"),
            ] {
                if !existing.iter().any(|name| name == column) {
                    tx.execute_batch(&format!(
                        "ALTER TABLE voice_firmware_delivery ADD COLUMN {column} {definition};"
                    ))?;
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
        if current.unwrap_or(0) < 14 {
            // 2026-11-02：小智设备信息快照（开机 OTA 上报 / 连接 hello / 实时状态查询），
            // 只存原始 JSON，字段随固件版本变化时不改表结构。加列前先查表结构，重跑迁移不会重复加列。
            let existing = {
                let mut statement =
                    tx.prepare("SELECT name FROM pragma_table_info('voice_devices')")?;

                statement
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for (column, definition) in [
                ("info_json", "TEXT"),
                ("info_at", "REAL"),
                ("hello_json", "TEXT"),
                ("hello_at", "REAL"),
                ("status_json", "TEXT"),
                ("status_at", "REAL"),
            ] {
                if !existing.iter().any(|name| name == column) {
                    tx.execute_batch(&format!(
                        "ALTER TABLE voice_devices ADD COLUMN {column} {definition};"
                    ))?;
                }
            }
            tx.execute_batch(
                "INSERT INTO schema_version(version,applied_at) VALUES(14,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 15 {
            // 2026-11-02：设备实际使用的拾音模式（auto / manual / realtime）。它是判断设备端 AEC
            // 是否开启的唯一行为证据——固件只在 AEC 开启时才选 realtime。加列前先查表结构。
            let existing = {
                let mut statement =
                    tx.prepare("SELECT name FROM pragma_table_info('voice_devices')")?;

                statement
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for (column, definition) in [("listen_json", "TEXT"), ("listen_at", "REAL")] {
                if !existing.iter().any(|name| name == column) {
                    tx.execute_batch(&format!(
                        "ALTER TABLE voice_devices ADD COLUMN {column} {definition};"
                    ))?;
                }
            }
            tx.execute_batch(
                "INSERT INTO schema_version(version,applied_at) VALUES(15,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 16 {
            // 2026-11-04（技术书 n50）：本地知识库独立化，支持按每个资料目录单独勾选启用/停用。
            // 加列前先查表结构，重跑迁移不会重复加列。
            let existing = {
                let mut statement =
                    tx.prepare("SELECT name FROM pragma_table_info('context_knowledge_roots')")?;

                statement
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            if !existing.iter().any(|name| name == "enabled") {
                tx.execute_batch(
                    "ALTER TABLE context_knowledge_roots ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1));",
                )?;
            }
            tx.execute_batch(
                "INSERT INTO schema_version(version,applied_at) VALUES(16,unixepoch());",
            )?;
        }
        if current.unwrap_or(0) < 17 {
            // 2026-11-06（技术书 n57）：记忆新增“取代哪条旧记录”与被再次提到的次数；资料新增段落级
            // 索引。段落 FTS5 为无正文（content=''）表，只存三字片段的倒排索引（detail=column，
            // 不存位置），原文仍只在磁盘上，检索命中后按指纹核对再读。加列前先查表结构，可重跑。
            let existing = {
                let mut statement =
                    tx.prepare("SELECT name FROM pragma_table_info('context_memories')")?;

                statement
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for (column, definition) in [
                (
                    "supersedes",
                    "TEXT CHECK(supersedes IS NULL OR length(supersedes) BETWEEN 1 AND 64)",
                ),
                (
                    "seen_count",
                    "INTEGER NOT NULL DEFAULT 1 CHECK(seen_count >= 1)",
                ),
                ("last_seen_at", "REAL"),
            ] {
                if !existing.iter().any(|name| name == column) {
                    tx.execute_batch(&format!(
                        "ALTER TABLE context_memories ADD COLUMN {column} {definition};"
                    ))?;
                }
            }
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS context_knowledge_chunks(
                   id INTEGER PRIMARY KEY,
                   root_id TEXT NOT NULL,
                   dir_id TEXT NOT NULL,
                   file_id TEXT NOT NULL,
                   ord INTEGER NOT NULL CHECK(ord >= 0),
                   UNIQUE(file_id, ord)
                 );
                 CREATE INDEX IF NOT EXISTS context_knowledge_chunks_root ON context_knowledge_chunks(root_id);
                 CREATE VIRTUAL TABLE IF NOT EXISTS context_knowledge_chunks_fts USING fts5(
                   text, content='', contentless_delete=1, detail=column, tokenize='trigram'
                 );
                 INSERT INTO schema_version(version,applied_at) VALUES(17,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 18 {
            // 2026-11-11：清理日常完全不读写的 7 张历史废弃表，并补齐高频轮询与按材料查询索引。
            tx.execute_batch(
                "DROP TABLE IF EXISTS ai_state;
                 DROP TABLE IF EXISTS gui_calibration;
                 DROP TABLE IF EXISTS auth_pairings;
                 DROP TABLE IF EXISTS workflows_history;
                 DROP TABLE IF EXISTS text_tasks_history;
                 DROP TABLE IF EXISTS jobs_history;
                 DROP TABLE IF EXISTS calibrations_history;
                 CREATE INDEX IF NOT EXISTS workflows_updated ON workflows(updated_at DESC, workflow_id ASC);
                 CREATE INDEX IF NOT EXISTS approvals_created ON approvals(created_at DESC, approval_id DESC);
                 CREATE INDEX IF NOT EXISTS approvals_status_created ON approvals(status, created_at DESC, approval_id DESC);
                 CREATE INDEX IF NOT EXISTS material_aliases_material ON material_aliases(material_name);
                 INSERT INTO schema_version(version,applied_at) VALUES(18,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 19 {
            // 2026-11-12（技术书 n81）：持久化有界语音对话记录（说了什么、调了什么工具、回了什么，方便随时排障）。
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS voice_chat_logs(
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   at_ms INTEGER NOT NULL,
                   device_id TEXT NOT NULL,
                   session_id TEXT NOT NULL,
                   turn INTEGER NOT NULL,
                   raw_asr TEXT NOT NULL,
                   corrected_asr TEXT NOT NULL,
                   reply TEXT NOT NULL,
                   spoken TEXT NOT NULL DEFAULT '',
                   tools_json TEXT NOT NULL DEFAULT '[]',
                   outcome TEXT NOT NULL,
                   truncated INTEGER NOT NULL DEFAULT 0 CHECK(truncated IN (0, 1)),
                   tools_dropped INTEGER NOT NULL DEFAULT 0,
                   created_at REAL NOT NULL,
                   updated_at REAL NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS voice_chat_logs_at ON voice_chat_logs(at_ms DESC, id DESC);
                 INSERT INTO schema_version(version,applied_at) VALUES(19,unixepoch());"
            )?;
        }
        if current.unwrap_or(0) < 20 {
            // 2026-11-18：voice_devices 增加状态修订号 status_revision，绑定 USB 配网起始登记修订并阻止并发撤销覆盖。
            let existing = {
                let mut statement =
                    tx.prepare("SELECT name FROM pragma_table_info('voice_devices')")?;
                statement
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            if !existing.iter().any(|name| name == "status_revision") {
                tx.execute_batch(
                    "ALTER TABLE voice_devices ADD COLUMN status_revision INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            tx.execute_batch(
                "INSERT INTO schema_version(version,applied_at) VALUES(20,unixepoch());",
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
}
