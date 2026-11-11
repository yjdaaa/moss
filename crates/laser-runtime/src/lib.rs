pub mod agent_store;
pub mod config;
pub mod context_store;
pub mod db;
pub mod firmware_store;
mod home_secret;
pub mod knowledge_store;
pub mod migration;
pub mod root;
pub mod store;

pub use agent_store::AgentRunRecord;
pub use config::LaserSettings;
pub use context_store::{ContextExtractionRunRow, ContextMemoryPatch, ContextMemoryRow};
pub use db::{
    MaterialConflictPolicy, RuntimeDb, SCHEMA_VERSION, SchemaError, VOICE_DEVICE_ALLOWED,
    VOICE_DEVICE_PENDING, VOICE_DEVICE_REVOKED, VoiceDeviceRecord, VoiceMemoryRecord,
};
pub use firmware_store::{FirmwareArtifactRow, FirmwareOfferRow};
pub use knowledge_store::{
    KnowledgeChunkHitRow, KnowledgeChunkRow, KnowledgeDirRow, KnowledgeFileRow, KnowledgeRootRow,
};
pub use migration::{
    ConflictPolicy, MigrationCounts, MigrationError, MigrationOptions, MigrationReport,
    normalize_material_version, remap_path, remap_path_to, run_migration, scan_uploads,
    sha256_file,
};
pub use root::{RuntimeRoot, RuntimeRootError};
pub use store::{
    MemoryStore, RecordKind, RuntimeStore, SplitSqliteStore, SqliteStore, StoredRecord,
};

pub fn packaging_sqlite_probe() -> rusqlite::Result<i64> {
    let connection = rusqlite::Connection::open_in_memory()?;
    connection.execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])?;
    connection.execute("INSERT INTO probe (value) VALUES (42)", [])?;
    connection.query_row("SELECT value FROM probe", [], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_sqlite_reads_and_writes() {
        assert_eq!(packaging_sqlite_probe().unwrap(), 42);
    }

    #[test]
    fn runtime_schema_seeds_twelve_builtin_cells() {
        let db = RuntimeDb::open_memory().unwrap();
        let materials: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM materials", [], |row| row.get(0))
            .unwrap();
        let cells: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM material_params", [], |row| row.get(0))
            .unwrap();
        assert_eq!(materials, 4);
        assert_eq!(cells, 12);
        db.migrate().unwrap();
        assert_eq!(
            db.connection
                .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            // v4（2026-10-06，技术书 §7）新增 approvals 表后为 4 行
            SCHEMA_VERSION
        );
    }

    #[test]
    fn app_secrets_are_created_and_updated_by_migration() {
        let db = RuntimeDb::open_memory().unwrap();
        assert_eq!(db.get_secret("jev_api_key").unwrap(), None);
        db.set_secret("jev_api_key", "sk-test-123", 10.0).unwrap();
        assert_eq!(
            db.get_secret("jev_api_key").unwrap().as_deref(),
            Some("sk-test-123")
        );
        db.set_secret("jev_api_key", "sk-test-456", 11.0).unwrap();
        assert_eq!(
            db.get_secret("jev_api_key").unwrap().as_deref(),
            Some("sk-test-456")
        );
        assert!(db.delete_secret("jev_api_key").unwrap());
        assert_eq!(db.get_secret("jev_api_key").unwrap(), None);
        assert!(!db.delete_secret("jev_api_key").unwrap());
    }

    #[test]
    fn history_schema_keeps_replacement_metadata() {
        let db = RuntimeDb::open_memory().unwrap();
        let columns: Vec<String> = db
            .connection
            .prepare("PRAGMA table_info(material_params_history)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "replaced_at"));
        assert!(columns.iter().any(|column| column == "reason"));
    }

    #[test]
    fn restart_recovery_fails_running_and_pending_jobs() {
        let db = RuntimeDb::open_memory().unwrap();
        db.connection
            .execute(
                "INSERT INTO jobs(job_id,status,created_at,payload) VALUES
             ('running-job','running',unixepoch(),'{}'),
             ('pending-job','pending',unixepoch(),'{}')",
                [],
            )
            .unwrap();
        assert_eq!(db.recover_after_restart().unwrap(), 2);
        let running: String = db
            .connection
            .query_row(
                "SELECT status FROM jobs WHERE job_id='running-job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let pending: String = db
            .connection
            .query_row(
                "SELECT status FROM jobs WHERE job_id='pending-job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(running, "failed");
        assert_eq!(pending, "failed");
    }

    /// 发送中被中断的 workflow 与标定矩阵：列与记录正文的状态都改为 failed（重启后查询不再显示“发送中”）。
    #[test]
    fn restart_recovery_fails_sending_workflows_and_calibrations_in_payload_too() {
        let db = RuntimeDb::open_memory().unwrap();
        db.connection
            .execute_batch(
                "INSERT INTO workflows(workflow_id,status,created_at,updated_at,payload) VALUES
                   ('wf-sending','sending',1,1,'{\"workflow_id\":\"wf-sending\",\"status\":\"sending\"}'),
                   ('wf-done','completed',1,1,'{\"workflow_id\":\"wf-done\",\"status\":\"completed\"}');
                 INSERT INTO calibrations(calibration_id,status,created_at,updated_at,payload) VALUES
                   ('cal-sending','sending',1,1,'{\"calibration_id\":\"cal-sending\",\"status\":\"sending\"}'),
                   ('cal-prepared','prepared',1,1,'{\"calibration_id\":\"cal-prepared\",\"status\":\"prepared\"}');",
            )
            .unwrap();
        db.recover_after_restart().unwrap();
        let read = |table: &str, key: &str, id: &str| -> (String, serde_json::Value) {
            let status: String = db
                .connection
                .query_row(
                    &format!("SELECT status FROM {table} WHERE {key}=?1"),
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            (status, db.read_payload(table, key, id).unwrap().unwrap())
        };
        for (table, key, id) in [
            ("workflows", "workflow_id", "wf-sending"),
            ("calibrations", "calibration_id", "cal-sending"),
        ] {
            let (status, payload) = read(table, key, id);
            assert_eq!(status, "failed", "{id}");
            assert_eq!(payload["status"], "failed", "{id}");
            assert_eq!(payload["detail"], "host 重启中断，任务安全终止", "{id}");
        }
        assert_eq!(
            read("workflows", "workflow_id", "wf-done").1["status"],
            "completed"
        );
        assert_eq!(
            read("calibrations", "calibration_id", "cal-prepared").1["status"],
            "prepared"
        );
    }

    #[test]
    fn material_import_handles_conflicts_history_and_tombstones() {
        let db = RuntimeDb::open_memory().unwrap();
        let cell_count: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM material_params", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cell_count, 12);

        let modified_json = serde_json::json!({
            "version": 1,
            "materials": {
                "椴木": {
                    "aliases": ["椴木新别名"],
                    "thicknesses": {
                        "3": {
                            "engrave": {
                                "raster": {
                                    "laser_min_power": 10,
                                    "laser_max_power": 999,
                                    "feed_rate": 2000,
                                    "travel_rate": 3000,
                                    "threshold": -1,
                                    "passes": 1,
                                    "pixel_size_mm": 0.1
                                }
                            }
                        }
                    }
                }
            }
        });

        let counts_keep = db
            .import_material_json(
                &modified_json,
                crate::db::MaterialConflictPolicy::Keep,
                1000.0,
            )
            .unwrap();
        assert_eq!(counts_keep.conflicts, 1);
        let power_keep: i32 = db
            .connection
            .query_row(
                "SELECT laser_max_power FROM material_params WHERE material_name='椴木' AND thickness_key='3' AND engraving_mode='raster'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(power_keep, 420);

        let counts_overwrite = db
            .import_material_json(
                &modified_json,
                crate::db::MaterialConflictPolicy::Overwrite,
                2000.0,
            )
            .unwrap();
        assert_eq!(counts_overwrite.conflicts, 1);
        let power_overwritten: i32 = db
            .connection
            .query_row(
                "SELECT laser_max_power FROM material_params WHERE material_name='椴木' AND thickness_key='3' AND engraving_mode='raster'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(power_overwritten, 999);

        let history_count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM material_params_history WHERE material_name='椴木' AND reason='import_overwrite'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_count, 1);

        let alias_exists: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM material_aliases WHERE alias='椴木新别名'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias_exists, 1);

        let tombstone_json = serde_json::json!({
            "version": 1,
            "materials": {
                "椴木": {
                    "thicknesses": {
                        "3": {
                            "engrave": {
                                "raster": null
                            }
                        }
                    }
                }
            }
        });
        let counts_tomb = db
            .import_material_json(
                &tombstone_json,
                crate::db::MaterialConflictPolicy::Keep,
                3000.0,
            )
            .unwrap();
        assert_eq!(counts_tomb.tombstones, 1);
        let raster_remaining: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM material_params WHERE material_name='椴木' AND thickness_key='3' AND engraving_mode='raster'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raster_remaining, 0);
    }

    fn table_columns(db: &RuntimeDb, table: &str) -> Vec<String> {
        db.connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    }

    /// A-41：内存库迁移到 v4，重复执行幂等。
    #[test]
    fn schema_v4_creates_approvals_table_and_is_idempotent() {
        let db = RuntimeDb::open_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let columns = table_columns(&db, "approvals");
        for column in [
            "approval_id",
            "status",
            "kind",
            "target_record",
            "digest",
            "summary",
            "source",
            "session_id",
            "request_id",
            "connection",
            "created_at",
            "decided_at",
            "expires_at",
            "updated_at",
            "outcome",
        ] {
            assert!(
                columns.iter().any(|c| c == column),
                "approvals 缺少列 {column}: {columns:?}"
            );
        }
        db.migrate().unwrap();
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let rows: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM schema_version WHERE version=4",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
        let materials: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM materials", [], |row| row.get(0))
            .unwrap();
        assert_eq!(materials, 4, "重复迁移不得重复播种");
    }

    /// A-41：v4 步骤失败时整体回滚、版本停在 3；排除原因后重试成功。
    #[test]
    fn schema_v4_failure_rolls_back_and_retry_succeeds() {
        let db = RuntimeDb::open_memory().unwrap();
        // 退回 v3 状态，并放一张同名但结构未知的表让 v4 步骤失败
        db.connection
            .execute_batch(
                "DROP TABLE context_knowledge_fts;
                 DROP TABLE context_knowledge_roots;
                 DROP TABLE context_knowledge_dirs;
                 DROP TABLE context_knowledge_files;
                 DROP TABLE context_memories_fts;
                 DROP TABLE context_memories;
                 DROP TABLE context_extraction_runs;
                 DROP TABLE approvals;
                 DROP TABLE voice_devices;
                 DROP TABLE voice_memory;
                 DROP TABLE voice_agent_runs;
                 DELETE FROM schema_version WHERE version>=4;
                 CREATE TABLE approvals(unexpected TEXT);",
            )
            .unwrap();
        assert_eq!(db.schema_version().unwrap(), 3);
        assert!(db.migrate().is_err());
        assert_eq!(db.schema_version().unwrap(), 3, "失败的迁移不得写入版本号");
        assert_eq!(
            table_columns(&db, "approvals"),
            vec!["unexpected".to_string()]
        );

        db.connection
            .execute_batch("DROP TABLE approvals;")
            .unwrap();
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(
            table_columns(&db, "approvals")
                .iter()
                .any(|c| c == "connection")
        );
    }

    /// A-41：迁移只执行 `current < N` 的步骤、不拒绝更高版本，
    /// 因此只认识 v3 的旧程序打开 v4 库时不会出错，也不会动新表。
    #[test]
    fn newer_schema_is_opened_without_changes() {
        let db = RuntimeDb::open_memory().unwrap();
        db.connection
            .execute(
                "INSERT INTO schema_version(version, applied_at) VALUES(?1, unixepoch())",
                [SCHEMA_VERSION + 1],
            )
            .unwrap();
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION + 1);
        let cells: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM material_params", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cells, 12);
    }

    /// 设备身份绑定：未经认证的 Client-Id 冲突不得降级已允许设备或篡改绑定；撤销保持撤销。
    #[test]
    fn voice_devices_registration_and_client_change() {
        let db = RuntimeDb::open_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        let record = db
            .voice_device_seen(
                "aa:bb",
                "uuid-1",
                "lichuang-dev",
                "1.9.2",
                "192.0.2.10",
                10.0,
            )
            .unwrap();
        assert_eq!(record.status, VOICE_DEVICE_PENDING);
        assert!(!record.is_allowed());
        assert!(
            db.set_voice_device_status("aa:bb", VOICE_DEVICE_ALLOWED, 11.0)
                .unwrap()
        );
        let again = db
            .voice_device_seen("aa:bb", "uuid-1", "", "", "", 12.0)
            .unwrap();
        assert!(again.is_allowed());
        assert_eq!(again.model, "lichuang-dev", "空值不覆盖已记录的信息");
        assert_eq!(again.first_seen, 10.0);
        assert_eq!(again.last_seen, 12.0);
        // 同一 Device-Id 被其他 Client-Id 冒用：不修改绑定、授权或最近访问时间。
        let changed = db
            .voice_device_seen("aa:bb", "uuid-other", "", "", "203.0.113.12", 13.0)
            .unwrap();
        assert_eq!(changed.status, VOICE_DEVICE_ALLOWED);
        assert_eq!(changed.client_id, "uuid-1");
        assert_eq!(changed.last_seen, 12.0);
        assert_eq!(changed.last_ip, "192.0.2.10");
        // 撤销后再次来访仍是撤销
        db.set_voice_device_status("aa:bb", VOICE_DEVICE_REVOKED, 14.0)
            .unwrap();
        let revoked = db
            .voice_device_seen("aa:bb", "uuid-x", "", "", "", 15.0)
            .unwrap();
        assert_eq!(revoked.status, VOICE_DEVICE_REVOKED);
        assert_eq!(revoked.client_id, "uuid-1");
        assert!(db.rename_voice_device("aa:bb", "  工作台小智  ").unwrap());
        assert_eq!(
            db.voice_device("aa:bb").unwrap().unwrap().name,
            "工作台小智"
        );
        assert!(
            db.set_voice_device_status("aa:bb", "unknown", 16.0)
                .is_err()
        );
        assert!(
            !db.set_voice_device_status("zz", VOICE_DEVICE_ALLOWED, 16.0)
                .unwrap()
        );
        db.voice_device_seen("cc:dd", "uuid-2", "m", "1", "", 20.0)
            .unwrap();
        let list = db.voice_devices().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].device_id, "cc:dd", "最近来访的在前");
    }

    /// v5：已是 v4 的库升级后 approvals 增加 request 列，旧行的 request 为空、仍能读出。
    #[test]
    fn schema_v5_adds_request_column_to_v4_database() {
        let db = RuntimeDb::open_memory().unwrap();
        db.connection
            .execute_batch(
                "ALTER TABLE approvals DROP COLUMN request;
                 DELETE FROM schema_version WHERE version>=5;
                 DROP TABLE context_knowledge_fts;
                 DROP TABLE context_knowledge_roots;
                 DROP TABLE context_knowledge_dirs;
                 DROP TABLE context_knowledge_files;
                 DROP TABLE context_memories_fts;
                 DROP TABLE context_memories;
                 DROP TABLE context_extraction_runs;
                 DROP TABLE voice_devices;
                 DROP TABLE voice_memory;
                 DROP TABLE voice_agent_runs;
                 INSERT INTO approvals(approval_id, status, kind, target_record, digest, summary, source,
                   created_at, expires_at, updated_at)
                 VALUES('apr_v4', 'pending', 'framing', 'framing', 'd', '走边框', 'lan', 1.0, 121.0, 1.0);",
            )
            .unwrap();
        assert_eq!(db.schema_version().unwrap(), 4);
        assert!(
            !table_columns(&db, "approvals")
                .iter()
                .any(|c| c == "request")
        );
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(
            table_columns(&db, "approvals")
                .iter()
                .any(|c| c == "request")
        );
        let record = db.get_approval("apr_v4").unwrap().unwrap();
        assert_eq!(record.request, None);
        assert_eq!(record.kind, laser_contracts::ApprovalKind::Framing);
    }

    #[test]
    fn nv10_voice_memory_revision_and_clear_prevent_stale_save() {
        let db = RuntimeDb::open_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(db.voice_memory("device-a").unwrap().is_none());
        let original = db.ensure_voice_memory("device-a", 10.0).unwrap();
        assert_eq!((original.revision, original.summary.as_str()), (0, ""));
        let same = db.ensure_voice_memory("device-a", 11.0).unwrap();
        assert_eq!(same, original, "重复初始化不能清除旧记忆");
        let updated = db
            .update_voice_memory_if_revision("device-a", 0, "喜欢喝茶", 12.0)
            .unwrap()
            .unwrap();
        assert_eq!(updated.revision, 1);
        assert!(
            db.update_voice_memory_if_revision("device-a", 0, "过期总结", 13.0)
                .unwrap()
                .is_none()
        );
        let cleared = db
            .update_voice_memory_if_revision("device-a", 1, "", 14.0)
            .unwrap()
            .unwrap();
        assert_eq!((cleared.revision, cleared.summary.as_str()), (2, ""));
        assert!(
            db.update_voice_memory_if_revision("device-a", 1, "应被拒绝", 15.0)
                .unwrap()
                .is_none()
        );
        assert!(db.voice_memory("device-b").unwrap().is_none());
        assert_eq!(db.voice_memories().unwrap().len(), 1);
        assert!(
            db.update_voice_memory_if_revision("device-a", 2, &"记".repeat(1501), 16.0)
                .is_err()
        );
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(db.voice_memory("device-a").unwrap().unwrap(), cleared);
    }

    #[test]
    fn nv10_voice_memory_v7_migration_failure_is_atomic() {
        let db = RuntimeDb::open_memory().unwrap();
        db.connection.execute_batch("DROP TABLE context_knowledge_fts; DROP TABLE context_knowledge_roots; DROP TABLE context_knowledge_dirs; DROP TABLE context_knowledge_files; DROP TABLE context_memories_fts; DROP TABLE context_memories; DROP TABLE context_extraction_runs; DROP TABLE voice_memory; DROP TABLE voice_agent_runs; DELETE FROM schema_version WHERE version>=7; CREATE TABLE voice_memory(unexpected TEXT);").unwrap();
        assert_eq!(db.schema_version().unwrap(), 6);
        assert!(db.migrate().is_err());
        assert_eq!(db.schema_version().unwrap(), 6);
        assert_eq!(
            table_columns(&db, "voice_memory"),
            vec!["unexpected".to_string()]
        );
        db.connection
            .execute_batch("DROP TABLE voice_memory;")
            .unwrap();
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn audit_entries_are_written_and_read_back_typed() {
        use laser_contracts::{ApprovalKind, AuditAction, AuditDetail, AuditEntry, AuditResult};
        let db = RuntimeDb::open_memory().unwrap();
        assert!(db.recent_audit(10).unwrap().is_empty());
        let created = AuditEntry {
            at: 100.0,
            client: "mcp".into(),
            action: AuditAction::ApprovalCreated,
            target: "wf_placeholder".into(),
            result: AuditResult::Ok,
            detail: AuditDetail {
                approval_id: Some("apr_test1".into()),
                kind: Some(ApprovalKind::WorkflowConfirmSend),
                ..AuditDetail::default()
            },
        };
        let blocked = AuditEntry {
            at: 101.0,
            client: "local".into(),
            action: AuditAction::StrictModeBlocked,
            target: "wf_placeholder".into(),
            result: AuditResult::Denied,
            detail: AuditDetail::default(),
        };
        let first = db.append_audit(&created).unwrap();
        let second = db.append_audit(&blocked).unwrap();
        assert!(second > first);
        assert_eq!(db.recent_audit(10).unwrap(), vec![blocked.clone(), created]);
        assert_eq!(db.recent_audit(1).unwrap(), vec![blocked]);
        let raw: String = db
            .connection
            .query_row(
                "SELECT detail FROM audit_log WHERE id=?1",
                [second],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw, "{}");
        let action: String = db
            .connection
            .query_row("SELECT action FROM audit_log WHERE id=?1", [first], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(action, "approval_created");
    }

    #[test]
    fn schema_v18_drops_dead_tables_and_indexes_hot_poll_queries() {
        let db = RuntimeDb::open_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        // 模拟从旧版升级：塞入 7 张废弃表并回退到 v17，重跑迁移后它们必须被清掉
        db.connection
            .execute_batch(
                "CREATE TABLE ai_state(id INTEGER PRIMARY KEY);
                 CREATE TABLE gui_calibration(target TEXT PRIMARY KEY);
                 CREATE TABLE auth_pairings(token_hash TEXT PRIMARY KEY);
                 CREATE TABLE workflows_history(history_id INTEGER PRIMARY KEY);
                 CREATE TABLE text_tasks_history(history_id INTEGER PRIMARY KEY);
                 CREATE TABLE jobs_history(history_id INTEGER PRIMARY KEY);
                 CREATE TABLE calibrations_history(history_id INTEGER PRIMARY KEY);
                 DELETE FROM schema_version WHERE version>=18;",
            )
            .unwrap();
        assert_eq!(db.schema_version().unwrap(), 17);
        db.migrate().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);

        for dead in [
            "ai_state",
            "gui_calibration",
            "auth_pairings",
            "workflows_history",
            "text_tasks_history",
            "jobs_history",
            "calibrations_history",
        ] {
            let exists: i64 = db
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [dead],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(exists, 0, "废弃表 {dead} 应在 v18 被删除");
        }

        let explain = |sql: &str| -> String {
            db.connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .join(" | ")
        };
        let wf_plan = explain(
            "SELECT workflow_id, payload, status FROM workflows ORDER BY updated_at DESC, workflow_id ASC LIMIT 50",
        );
        assert!(
            wf_plan.contains("workflows_updated"),
            "workflow recent 应命中 workflows_updated 索引，实际: {wf_plan}"
        );
        assert!(
            !wf_plan.contains("TEMP B-TREE"),
            "workflow recent 不得产生临时排序，实际: {wf_plan}"
        );

        let apr_all_plan = explain(
            "SELECT approval_id FROM approvals ORDER BY created_at DESC, approval_id DESC LIMIT 50",
        );
        assert!(
            apr_all_plan.contains("approvals_created"),
            "approvals 列表应命中 approvals_created 索引，实际: {apr_all_plan}"
        );
        let apr_status_plan = explain(
            "SELECT approval_id FROM approvals WHERE status='pending' ORDER BY created_at DESC, approval_id DESC LIMIT 50",
        );
        assert!(
            apr_status_plan.contains("approvals_status_created"),
            "按状态查 approvals 应命中 approvals_status_created 索引，实际: {apr_status_plan}"
        );
    }

    #[test]
    fn shutdown_updates_payload_status_and_startup_heals_legacy_split_records() {
        let db = RuntimeDb::open_memory().unwrap();
        db.connection
            .execute_batch(
                "INSERT INTO workflows(workflow_id,status,created_at,updated_at,payload) VALUES
                   ('wf-active','sending',1,1,'{\"workflow_id\":\"wf-active\",\"status\":\"sending\"}'),
                   ('wf-split','failed',1,1,'{\"workflow_id\":\"wf-split\",\"status\":\"sending\"}');
                 INSERT INTO calibrations(calibration_id,status,created_at,updated_at,payload) VALUES
                   ('cal-active','processing',1,1,'{\"calibration_id\":\"cal-active\",\"status\":\"processing\"}');",
            )
            .unwrap();

        db.abort_active_records_on_shutdown().unwrap();
        let wf_active = db
            .read_payload("workflows", "workflow_id", "wf-active")
            .unwrap()
            .unwrap();
        assert_eq!(wf_active["status"], "failed");
        assert_eq!(wf_active["detail"], "host 退出中断，任务安全终止");

        let cal_active = db
            .read_payload("calibrations", "calibration_id", "cal-active")
            .unwrap()
            .unwrap();
        assert_eq!(cal_active["status"], "failed");

        // 旧版遗留的“外层已是 failed、内层 payload 仍卡在 sending”的脏记录，启动恢复能自动修好
        db.recover_after_restart().unwrap();
        let wf_split = db
            .read_payload("workflows", "workflow_id", "wf-split")
            .unwrap()
            .unwrap();
        assert_eq!(wf_split["status"], "failed");
    }

    #[test]
    fn wal_readonly_split_store_reads_while_write_tx_is_open_and_checkpoints_cleanly() {
        use std::sync::{Arc, Mutex};
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("moss.sqlite");
        let write_db = RuntimeDb::open(&db_path).unwrap();
        SqliteStore::new(&write_db)
            .put(&StoredRecord {
                kind: RecordKind::Workflow,
                id: "wf-1".into(),
                status: Some("preview_ready".into()),
                payload: serde_json::json!({"workflow_id":"wf-1","status":"preview_ready","updated_at":10.0}),
            })
            .unwrap();
        let read_db = write_db
            .open_readonly()
            .unwrap()
            .expect("file db opens readonly");
        let write_arc = Arc::new(Mutex::new(write_db));
        let read_arc = Arc::new(Mutex::new(read_db));
        let store = SplitSqliteStore::new(write_arc.clone(), Some(read_arc));

        // 模拟后台线程一直占着写锁并开启了未提交的 SQLite 写事务
        let write_guard = write_arc.lock().unwrap();
        write_guard
            .connection
            .execute_batch(
                "BEGIN IMMEDIATE; INSERT INTO app_secrets(name,value,updated_at) VALUES('k','v',1.0);",
            )
            .unwrap();

        // 只读轮询完全不等待 write_arc 锁，也不被未提交写事务阻塞，直接读到最新已提交快照
        let recent = store.list_recent(RecordKind::Workflow, 10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, "wf-1");
        let single = store.get(RecordKind::Workflow, "wf-1").unwrap().unwrap();
        assert_eq!(single.status.as_deref(), Some("preview_ready"));

        write_guard.connection.execute_batch("COMMIT;").unwrap();
        write_guard.maintenance_on_shutdown();
        let wal_path = temp.path().join("moss.sqlite-wal");
        if wal_path.exists() {
            assert_eq!(
                std::fs::metadata(&wal_path).unwrap().len(),
                0,
                "正常退出维护后 WAL 文件应被截断归零"
            );
        }
    }

    #[test]
    fn audit_cap_and_startup_pruning_bound_growth_while_preserving_active_rows() {
        use laser_contracts::{AuditAction, AuditDetail, AuditEntry, AuditResult};
        let db = RuntimeDb::open_memory().unwrap();
        for idx in 0..2005 {
            db.append_audit(&AuditEntry {
                at: idx as f64,
                client: "desktop".into(),
                action: AuditAction::ApprovalCreated,
                target: "wf".into(),
                result: AuditResult::Ok,
                detail: AuditDetail::default(),
            })
            .unwrap();
        }
        let audit_count: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(audit_count, 2000, "append_audit 应实时限制最多保留 2000 条");

        // 插入一条无 source_ref 的陈旧墓碑（40 天前）、一条带 source_ref 的防重导入墓碑（40 天前）和一条正常记忆
        db.connection
            .execute_batch(
                "INSERT INTO context_memories(id,scope_kind,scope_id,category,content,verification,source_kind,source_ref,revision,created_at,updated_at,deleted_at,seen_count)
                 VALUES
                   ('ctx-old-empty','device','d1','preference','','candidate','user_edit',NULL,2,10.0,100.0,100.0,1),
                   ('ctx-keep-ref','device','d1','preference','','candidate','legacy_nv10_import','nv10:d1',2,10.0,100.0,100.0,1),
                   ('ctx-live','device','d1','preference','保留正常记忆','confirmed','user_edit',NULL,1,10.0,10.0,NULL,1);",
            )
            .unwrap();

        let removed = db
            .prune_historical_records(40.0 * 86_400.0 + 200.0)
            .unwrap();
        assert!(removed >= 1);
        assert!(db.context_memory("ctx-old-empty").unwrap().is_none());
        assert!(
            db.context_source_seen("device", "d1", "nv10:d1").unwrap(),
            "带 source_ref 的防重导入墓碑不得被清理"
        );
        assert!(db.context_memory("ctx-live").unwrap().is_some());
    }
}
