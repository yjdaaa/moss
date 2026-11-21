//! 重启恢复、退出收尾、历史记录定额清理与孤儿生成文件清理。

use std::path::Path;

use rusqlite::params;
use serde_json::Value;

use super::{AUDIT_LOG_MAX_ROWS, RuntimeDb, SchemaError, VOICE_CHAT_LOGS_MAX_ROWS};

const APPROVALS_TERMINAL_MAX_ROWS: i64 = 500;
const WORKFLOWS_INACTIVE_MAX_ROWS: i64 = 1000;
const CALIBRATIONS_INACTIVE_MAX_ROWS: i64 = 200;
const MATERIAL_HISTORY_MAX_ROWS: i64 = 500;
const EXTRACTION_RUNS_MAX_ROWS: i64 = 500;
const FIRMWARE_DELIVERY_FINISHED_MAX_ROWS: i64 = 200;
const CONTEXT_TOMBSTONE_RETENTION_SECS: f64 = 30.0 * 86_400.0;

impl RuntimeDb {
    pub fn recover_after_restart(&self) -> Result<usize, SchemaError> {
        let count = self.connection.execute(
            "UPDATE jobs SET status='failed', finished_at=unixepoch(),
             payload=json_set(
               CASE WHEN json_type(payload,'$.result')='object' THEN payload ELSE json_set(payload,'$.result',json('{}')) END,
               '$.status','failed',
               '$.result.detail','host 重启中断，任务安全终止',
               '$.result.phase','result_unknown'
             )
             WHERE status IN ('running', 'pending', 'queued')
                OR (status='failed' AND json_extract(payload,'$.status') IN ('running', 'pending', 'queued'))",
            [],
        )?;
        // 记录正文里的 status 与列保持一致：状态查询返回的是正文（同时修补旧版退出路径曾漏改 $.status 的记录，并标记 send_result.phase='result_unknown'）
        self.connection.execute(
            "UPDATE workflows SET status='failed', updated_at=unixepoch(),
             payload=json_set(
               CASE WHEN json_type(payload,'$.send_result')='object' THEN payload ELSE json_set(payload,'$.send_result',json('{}')) END,
               '$.status','failed',
               '$.detail','host 重启中断，任务安全终止',
               '$.send_result.running',json('false'),
               '$.send_result.phase','result_unknown'
             )
             WHERE status IN ('sending', 'processing')
                OR (status='failed' AND json_extract(payload,'$.status') IN ('sending', 'processing'))",
            [],
        )?;
        // 标定矩阵与 workflow 走同一条发送链路，发送中被中断同样记为失败、结果未知，不自动重发
        self.connection.execute(
            "UPDATE calibrations SET status='failed', updated_at=unixepoch(),
             payload=json_set(
               CASE WHEN json_type(payload,'$.send_result')='object' THEN payload ELSE json_set(payload,'$.send_result',json('{}')) END,
               '$.status','failed',
               '$.detail','host 重启中断，任务安全终止',
               '$.send_result.running',json('false'),
               '$.send_result.phase','result_unknown'
             )
             WHERE status IN ('sending', 'processing')
                OR (status='failed' AND json_extract(payload,'$.status') IN ('sending', 'processing'))",
            [],
        )?;
        Ok(count)
    }

    /// 进程退出停机落库：外层 `status` 列与 JSON 正文的 `$.status` 同步改为 `failed`，并写入 `result_unknown` 阶段标记，避免内外状态分裂或被误判为普通失败。
    pub fn abort_active_records_on_shutdown(&self) -> Result<(), SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "UPDATE jobs SET status='failed', finished_at=unixepoch(),
             payload=json_set(
               CASE WHEN json_type(payload,'$.result')='object' THEN payload ELSE json_set(payload,'$.result',json('{}')) END,
               '$.status','failed',
               '$.result.detail','host 退出中断，任务安全终止',
               '$.result.phase','result_unknown'
             )
             WHERE status IN ('running', 'pending', 'queued')",
            [],
        )?;
        tx.execute(
            "UPDATE workflows SET status='failed', updated_at=unixepoch(),
             payload=json_set(
               CASE WHEN json_type(payload,'$.send_result')='object' THEN payload ELSE json_set(payload,'$.send_result',json('{}')) END,
               '$.status','failed',
               '$.detail','host 退出中断，任务安全终止',
               '$.send_result.running',json('false'),
               '$.send_result.phase','result_unknown'
             )
             WHERE status IN ('sending', 'processing')",
            [],
        )?;
        tx.execute(
            "UPDATE calibrations SET status='failed', updated_at=unixepoch(),
             payload=json_set(
               CASE WHEN json_type(payload,'$.send_result')='object' THEN payload ELSE json_set(payload,'$.send_result',json('{}')) END,
               '$.status','failed',
               '$.detail','host 退出中断，任务安全终止',
               '$.send_result.running',json('false'),
               '$.send_result.phase','result_unknown'
             )
             WHERE status IN ('sending', 'processing')",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 启动时定额清理超额陈旧历史记录，不触碰任何进行中任务、待处理审批、正常记忆或防重导入标记。
    pub fn prune_historical_records(&self, now: f64) -> Result<usize, SchemaError> {
        if !now.is_finite() {
            return Ok(0);
        }
        let tx = self.connection.unchecked_transaction()?;
        let mut removed = 0usize;
        removed += tx.execute(
            "DELETE FROM audit_log WHERE id NOT IN (SELECT id FROM audit_log ORDER BY id DESC LIMIT ?1)",
            params![AUDIT_LOG_MAX_ROWS],
        )?;
        removed += tx.execute(
            "DELETE FROM approvals
             WHERE status IN ('rejected','expired','executed','failed')
               AND COALESCE(json_extract(outcome,'$.phase'), '') <> 'running'
               AND approval_id NOT IN (
                   SELECT approval_id FROM approvals
                   WHERE status IN ('rejected','expired','executed','failed')
                     AND COALESCE(json_extract(outcome,'$.phase'), '') <> 'running'
                   ORDER BY created_at DESC, approval_id DESC LIMIT ?1
               )",
            params![APPROVALS_TERMINAL_MAX_ROWS],
        )?;
        removed += tx.execute(
            "DELETE FROM workflows
             WHERE status NOT IN ('sending','processing','running','preparing')
               AND workflow_id NOT IN (
                   SELECT workflow_id FROM workflows
                   WHERE status NOT IN ('sending','processing','running','preparing')
                   ORDER BY updated_at DESC, workflow_id ASC LIMIT ?1
               )",
            params![WORKFLOWS_INACTIVE_MAX_ROWS],
        )?;
        removed += tx.execute(
            "DELETE FROM calibrations
             WHERE status NOT IN ('sending','processing','running','preparing')
               AND calibration_id NOT IN (
                   SELECT calibration_id FROM calibrations
                   WHERE status NOT IN ('sending','processing','running','preparing')
                   ORDER BY updated_at DESC, calibration_id ASC LIMIT ?1
               )",
            params![CALIBRATIONS_INACTIVE_MAX_ROWS],
        )?;
        removed += tx.execute(
            "DELETE FROM material_params_history
             WHERE rowid NOT IN (SELECT rowid FROM material_params_history ORDER BY rowid DESC LIMIT ?1)",
            params![MATERIAL_HISTORY_MAX_ROWS],
        )?;
        removed += tx.execute(
            "DELETE FROM context_memories
             WHERE deleted_at IS NOT NULL AND source_ref IS NULL AND deleted_at < ?1",
            params![now - CONTEXT_TOMBSTONE_RETENTION_SECS],
        )?;
        removed += tx.execute(
            "DELETE FROM context_extraction_runs
             WHERE id NOT IN (SELECT id FROM context_extraction_runs ORDER BY started_at DESC, id DESC LIMIT ?1)",
            params![EXTRACTION_RUNS_MAX_ROWS],
        )?;
        removed += tx.execute(
            "DELETE FROM voice_firmware_delivery
             WHERE token IS NULL AND download_status <> 'downloading'
               AND delivery_id NOT IN (
                   SELECT delivery_id FROM voice_firmware_delivery
                   WHERE token IS NULL AND download_status <> 'downloading'
                   ORDER BY updated_at DESC, delivery_id DESC LIMIT ?1
               )",
            params![FIRMWARE_DELIVERY_FINISHED_MAX_ROWS],
        )?;
        removed += tx.execute(
            "DELETE FROM voice_chat_logs
             WHERE id NOT IN (SELECT id FROM voice_chat_logs ORDER BY at_ms DESC, id DESC LIMIT ?1)",
            params![VOICE_CHAT_LOGS_MAX_ROWS],
        )?;
        tx.commit()?;
        Ok(removed)
    }

    /// 启动时回收 `generated_assets/` 与 `previews/` 中已被历史定额清理淘汰且超过 7 天无人引用的自动生成产物。
    /// 绝不触碰 `uploads/` 用户上传目录、符号链接、7 天内新文件或仍在任何表记录中出现过文件名的产物。
    pub fn prune_orphan_generated_files(
        &self,
        generated_dirs: &[&Path],
        now: f64,
    ) -> Result<usize, SchemaError> {
        const ORPHAN_GENERATED_MIN_AGE_SECS: f64 = 7.0 * 86_400.0;
        if !now.is_finite() || now <= ORPHAN_GENERATED_MIN_AGE_SECS {
            return Ok(0);
        }
        let cutoff = now - ORPHAN_GENERATED_MIN_AGE_SECS;
        let mut referenced = std::collections::HashSet::new();

        fn record_str_ref(raw: &str, set: &mut std::collections::HashSet<String>) {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.len() > 1024 {
                return;
            }
            if let Some(base) = trimmed.rsplit(['/', '\\']).next()
                && !base.is_empty()
            {
                set.insert(base.to_owned());
            }
        }

        fn collect_json_refs(val: &Value, set: &mut std::collections::HashSet<String>) {
            match val {
                Value::String(s) => record_str_ref(s, set),
                Value::Array(items) => {
                    for item in items {
                        collect_json_refs(item, set);
                    }
                }
                Value::Object(map) => {
                    for v in map.values() {
                        collect_json_refs(v, set);
                    }
                }
                _ => {}
            }
        }

        for sql in [
            "SELECT gcode_path, payload FROM workflows",
            "SELECT gcode_path, payload FROM calibrations",
            "SELECT task_dir, payload FROM text_tasks",
            "SELECT gcode_path, payload FROM jobs",
        ] {
            let mut stmt = self.connection.prepare(sql)?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                if let Some(path) = row.get::<_, Option<String>>(0)? {
                    record_str_ref(&path, &mut referenced);
                }
                let payload: String = row.get(1)?;
                if let Ok(parsed) = serde_json::from_str::<Value>(&payload) {
                    collect_json_refs(&parsed, &mut referenced);
                } else {
                    record_str_ref(&payload, &mut referenced);
                }
            }
        }
        {
            let mut stmt = self.connection.prepare("SELECT path FROM uploads")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                record_str_ref(&path, &mut referenced);
            }
        }

        fn is_generated_artifact_name(name: &str) -> bool {
            if name.starts_with('.') {
                return false;
            }
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".gcode")
                || lower.ends_with(".png")
                || lower.ends_with(".jpg")
                || lower.ends_with(".jpeg")
                || lower.ends_with(".svg")
                || lower.ends_with(".scene.json")
        }

        let mut removed = 0usize;
        for dir in generated_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                    continue;
                }
                let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !is_generated_artifact_name(file_name) || referenced.contains(file_name) {
                    continue;
                }
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                let Ok(mtime_dur) = modified.duration_since(std::time::UNIX_EPOCH) else {
                    continue;
                };
                if mtime_dur.as_secs_f64() < cutoff && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// 退出前执行统计优化与限时 WAL 截断归并，避免阻塞停机路径的同时收回 `.sqlite-wal` 体积。
    pub fn maintenance_on_shutdown(&self) {
        let _ = self
            .connection
            .busy_timeout(std::time::Duration::from_millis(100));
        let _ = self
            .connection
            .execute_batch("PRAGMA optimize; PRAGMA wal_checkpoint(TRUNCATE);");
        let _ = self
            .connection
            .busy_timeout(std::time::Duration::from_secs(5));
    }
}
