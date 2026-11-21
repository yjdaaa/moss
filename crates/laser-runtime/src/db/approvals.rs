//! 桌面审批记录与审计日志。

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use laser_contracts::{
    ApprovalConnection, ApprovalKind, ApprovalOutcome, ApprovalRecord, ApprovalSource,
    ApprovalStatus, AuditAction, AuditDetail, AuditEntry, AuditResult,
};

use super::{AUDIT_LOG_MAX_ROWS, RuntimeDb, SchemaError};

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
        let invalid = |what: &str, value: &str| {
            SchemaError::InvalidRecord(format!("审批记录{what}无效: {value}"))
        };
        let status =
            ApprovalStatus::parse(&self.status).ok_or_else(|| invalid("状态", &self.status))?;
        let kind = ApprovalKind::parse(&self.kind).ok_or_else(|| invalid("类别", &self.kind))?;
        let source =
            ApprovalSource::parse(&self.source).ok_or_else(|| invalid("来源", &self.source))?;
        let connection = match self.connection.as_deref() {
            Some(text) => Some(serde_json::from_str::<ApprovalConnection>(text)?),
            None => None,
        };
        let outcome = match self.outcome.as_deref() {
            Some(text) => Some(serde_json::from_str::<ApprovalOutcome>(text)?),
            None => None,
        };
        let request = match self.request.as_deref() {
            Some(text) => Some(serde_json::from_str::<serde_json::Map<String, Value>>(
                text,
            )?),
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

impl RuntimeDb {
    /// 追加一条审计记录（技术书 §7.3）。`detail` 由 `AuditDetail` 序列化，不拼裸 JSON。
    /// 返回新行 id，并按 `AUDIT_LOG_MAX_ROWS` 定额淘汰最早的旧记录，防止长期运行膨胀。
    /// 调用方决定失败后的处理：R2 停止类动作不因审计失败而阻断，R3 阻断。
    pub fn append_audit(&self, entry: &AuditEntry) -> Result<i64, SchemaError> {
        let detail = serde_json::to_string(&entry.detail)?;
        let mut stmt = self.connection.prepare_cached(
            "INSERT INTO audit_log(at, client, action, target, result, detail) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        stmt.execute(params![
            entry.at,
            entry.client,
            entry.action.as_str(),
            entry.target,
            entry.result.as_str(),
            detail
        ])?;
        let new_id = self.connection.last_insert_rowid();
        if new_id > AUDIT_LOG_MAX_ROWS {
            let mut prune = self
                .connection
                .prepare_cached("DELETE FROM audit_log WHERE id <= ?1")?;
            let _ = prune.execute(params![new_id - AUDIT_LOG_MAX_ROWS]);
        }
        Ok(new_id)
    }

    /// 最近的审计记录，新的在前。
    pub fn recent_audit(&self, limit: usize) -> Result<Vec<AuditEntry>, SchemaError> {
        let mut statement = self.connection.prepare_cached(
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
            entries.push(AuditEntry {
                at,
                client,
                action,
                target,
                result,
                detail,
            });
        }
        Ok(entries)
    }

    /// 写入一条新审批（技术书 §7.1）。`connection` 与 `outcome` 以 JSON 存入各自的列。
    pub fn insert_approval(&self, record: &ApprovalRecord) -> Result<(), SchemaError> {
        let connection = record
            .connection
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let outcome = record
            .outcome
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let request = record
            .request
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
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
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {APPROVAL_COLUMNS} FROM approvals WHERE approval_id=?1"
        ))?;
        let row = statement
            .query_row(params![approval_id], ApprovalRow::read)
            .optional()?;
        row.map(ApprovalRow::into_record).transpose()
    }

    /// 按幂等键 `(source, request_id)` 查找审批。
    pub fn find_approval_by_request(
        &self,
        source: ApprovalSource,
        request_id: &str,
    ) -> Result<Option<ApprovalRecord>, SchemaError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {APPROVAL_COLUMNS} FROM approvals WHERE source=?1 AND request_id=?2"
        ))?;
        let row = statement
            .query_row(params![source.as_str(), request_id], ApprovalRow::read)
            .optional()?;
        row.map(ApprovalRow::into_record).transpose()
    }

    /// 列出审批，新的在前；`status` 为空时列出全部。
    pub fn list_approvals(
        &self,
        status: Option<ApprovalStatus>,
        limit: usize,
    ) -> Result<Vec<ApprovalRecord>, SchemaError> {
        let mut statement = self.connection.prepare_cached(&format!(
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

    /// 启动恢复专用：筛出已消费但缺少终态或仍停留在 `running` 的中断审批，避免全表搬进内存。
    pub fn list_running_executed_approvals(&self) -> Result<Vec<ApprovalRecord>, SchemaError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {APPROVAL_COLUMNS} FROM approvals \
             WHERE status='executed' \
               AND (outcome IS NULL \
                    OR json_extract(outcome, '$.phase') IS NULL \
                    OR json_extract(outcome, '$.phase') NOT IN ('completed', 'cancelled', 'failed', 'unknown')) \
             ORDER BY created_at ASC, approval_id ASC"
        ))?;
        let rows = statement.query_map([], ApprovalRow::read)?;
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
            params![
                to.as_str(),
                now,
                decided,
                outcome,
                approval_id,
                from.as_str()
            ],
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
    pub fn approvals_due_for_expiry(
        &self,
        now: f64,
        all_open: bool,
    ) -> Result<Vec<ApprovalRecord>, SchemaError> {
        let mut statement = self.connection.prepare_cached(&format!(
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
}
