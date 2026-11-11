//! 桌面审批服务（技术书 §5.3–§5.5；实施清单 P1-03）。
//!
//! 负责：解析显式连接目标（拿不到就 `target_required`，绝不自动选串口）、计算动作摘要、
//! 创建 / 批准 / 拒绝 / 过期、单次消费与执行结果记录。这里只管审批状态，不碰设备；
//! 严格模式下由执行入口调用（P1-04），桌面审批接口在 P1-07 接入。

use std::sync::{Arc, Mutex};

use laser_contracts::{
    APPROVAL_ID_PREFIX, ApprovalConnection, ApprovalKind, ApprovalOutcome, ApprovalPhase,
    ApprovalRecord, ApprovalSource, ApprovalStatus, AuditAction, AuditDetail, AuditEntry,
    AuditResult, CallerContext, approval_ttl_seconds,
};
use laser_runtime::{RuntimeDb, SchemaError};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// 审批服务错误，`code()` 即接口中的 `error_code`（技术书 §5.1 错误码表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalError {
    /// 无法得到显式连接目标（串口留空且无 `.env` 默认值等）
    TargetRequired(String),
    InvalidRequest(String),
    NotFound,
    Expired,
    /// 已被使用或已失败
    Consumed,
    Rejected,
    /// 尚未批准就尝试执行
    NotApproved,
    /// 摘要与批准时 / 当前状态不一致
    Mismatch,
    RequestConflict,
    /// 数据库不可用
    Unavailable(String),
}

impl ApprovalError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TargetRequired(_) => "target_required",
            Self::InvalidRequest(_) => "approval_invalid_request",
            Self::NotFound => "approval_not_found",
            Self::Expired => "approval_expired",
            Self::Consumed => "approval_consumed",
            Self::Rejected => "approval_rejected",
            Self::NotApproved => "approval_not_approved",
            Self::Mismatch => "approval_mismatch",
            Self::RequestConflict => "approval_request_conflict",
            Self::Unavailable(_) => "approval_unavailable",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            Self::TargetRequired(_) | Self::InvalidRequest(_) => 400,
            Self::NotFound => 404,
            Self::Expired => 410,
            Self::Consumed
            | Self::Rejected
            | Self::NotApproved
            | Self::Mismatch
            | Self::RequestConflict => 409,
            Self::Unavailable(_) => 503,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::TargetRequired(detail) => format!("需要明确的目标设备：{detail}"),
            Self::InvalidRequest(detail) => format!("审批请求无效：{detail}"),
            Self::NotFound => "审批请求不存在".into(),
            Self::Expired => "授权已过期，请重新发起".into(),
            Self::Consumed => "授权已被使用或已失败，不能重复执行".into(),
            Self::Rejected => "该请求已被拒绝".into(),
            Self::NotApproved => "该请求尚未在桌面批准".into(),
            Self::Mismatch => "加工内容或目标设备已变化，与批准时不一致，请重新发起".into(),
            Self::RequestConflict => {
                "同一请求编号对应的内容或目标已改变，请使用新编号重新发起".into()
            }
            Self::Unavailable(detail) => format!("审批服务不可用：{detail}"),
        }
    }
}

impl From<SchemaError> for ApprovalError {
    fn from(error: SchemaError) -> Self {
        Self::Unavailable(format!("{error:?}"))
    }
}

/// 创建审批所需的信息。`facts` 是参与摘要的动作要素（G-code SHA、材料、参数、发送方式等），
/// 由各执行入口按类别整理；连接目标单独给出，摘要中只放它的哈希。
#[derive(Debug, Clone)]
pub struct NewApproval {
    /// Trusted identity derived at entry, never from request JSON. None disables request-id reuse.
    pub owner: Option<String>,
    /// Live voice permission checked under the database lock to fence revoke/re-allow races.
    pub voice_permit: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub kind: ApprovalKind,
    pub target_record: String,
    pub source: ApprovalSource,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub summary: String,
    pub facts: Map<String, Value>,
    pub connection: ApprovalConnection,
    /// 原始执行参数（调用方已去掉连接字段与身份字段）；批准后据此重放执行
    pub request: Map<String, Value>,
    /// 审计 `client`：发起方（`audit_client(&CallerContext)`）
    pub client: String,
    /// 从请求中删除的字段（如模型自行填写的 `confirmed`），单独记一条审计
    pub removed_fields: Vec<String>,
}

/// 时钟（Unix 秒）。测试注入固定时间以验证过期。
pub type ApprovalClock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn system_clock() -> ApprovalClock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0)
    })
}

pub struct ApprovalService {
    db: Arc<Mutex<RuntimeDb>>,
    read_db: Option<Arc<Mutex<RuntimeDb>>>,
    ttl_seconds: u64,
    clock: ApprovalClock,
}

/// 审计写入失败：R3 路径据此拒绝（技术书 §7.3，验收 A-34）。
fn audit_failed(error: SchemaError) -> ApprovalError {
    ApprovalError::Unavailable(format!("审计记录写入失败，已拒绝执行：{error:?}"))
}

fn sql_failed(error: rusqlite::Error) -> ApprovalError {
    ApprovalError::from(SchemaError::from(error))
}

fn audit_entry(
    now: f64,
    client: &str,
    action: AuditAction,
    result: AuditResult,
    record: &ApprovalRecord,
    detail: AuditDetail,
) -> AuditEntry {
    AuditEntry {
        at: now,
        client: client.to_owned(),
        action,
        target: record.target_record.clone(),
        result,
        detail: AuditDetail {
            approval_id: Some(record.approval_id.clone()),
            kind: Some(record.kind),
            digest: Some(record.digest.clone()),
            ..detail
        },
    }
}

/// 降低风险的状态变化（拒绝、过期、失败）不因审计写入失败而回退，只记录到错误输出。
fn audit_best_effort(db: &RuntimeDb, entry: &AuditEntry) {
    if let Err(error) = db.append_audit(entry) {
        eprintln!(
            "[laser-api] 审计记录写入失败（{}）: {error:?}",
            entry.action.as_str()
        );
    }
}

/// 审计 `client`：审批过期等由服务自身触发的变化。
const SYSTEM_CLIENT: &str = "system";
/// 审计 `client`：桌面端的批准、拒绝与经批准的执行。
const DESKTOP_CLIENT: &str = "desktop";

fn valid_home_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 64
        && fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_home_target(target: &str) -> bool {
    let Some((domain, name)) = target.split_once('.') else {
        return false;
    };
    matches!(
        domain,
        "light" | "switch" | "climate" | "lock" | "alarm_control_panel" | "cover" | "media_player"
    ) && !name.is_empty()
        && target.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

impl ApprovalService {
    /// 有效期取 `MOSS_APPROVAL_TTL_SECONDS`（30..600，默认 120）。
    pub fn new(db: Arc<Mutex<RuntimeDb>>) -> Self {
        let raw = std::env::var("MOSS_APPROVAL_TTL_SECONDS").ok();
        let read_db = db
            .lock()
            .ok()
            .and_then(|guard| guard.open_readonly().ok().flatten())
            .map(|ro| Arc::new(Mutex::new(ro)));
        Self {
            db,
            read_db,
            ttl_seconds: approval_ttl_seconds(raw.as_deref()),
            clock: system_clock(),
        }
    }

    pub fn with_read_db(mut self, read_db: Option<Arc<Mutex<RuntimeDb>>>) -> Self {
        self.read_db = read_db;
        self
    }

    pub fn with_ttl_seconds(mut self, seconds: u64) -> Self {
        self.ttl_seconds = approval_ttl_seconds(Some(&seconds.to_string()));
        self
    }

    pub fn with_clock(mut self, clock: ApprovalClock) -> Self {
        self.clock = clock;
        self
    }

    pub fn ttl_seconds(&self) -> u64 {
        self.ttl_seconds
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, RuntimeDb>, ApprovalError> {
        self.db
            .lock()
            .map_err(|_| ApprovalError::Unavailable("数据库锁已损坏".into()))
    }

    fn lock_read(&self) -> Result<std::sync::MutexGuard<'_, RuntimeDb>, ApprovalError> {
        match self.read_db.as_ref() {
            Some(read) => read
                .lock()
                .map_err(|_| ApprovalError::Unavailable("只读数据库锁已损坏".into())),
            None => self.lock(),
        }
    }

    /// 写一条与审批生命周期无关的审计（R4 拒绝、严格模式拦截等）。调用方决定失败后的处理。
    pub fn audit(&self, entry: &AuditEntry) -> Result<(), ApprovalError> {
        self.lock()?
            .append_audit(entry)
            .map(|_| ())
            .map_err(audit_failed)
    }

    /// 把到期（或 `all_open` 时全部）pending / approved 标为 expired，并逐条记审计。
    fn expire_with_audit(
        &self,
        db: &RuntimeDb,
        now: f64,
        all_open: bool,
    ) -> Result<usize, ApprovalError> {
        let due = db.approvals_due_for_expiry(now, all_open)?;
        let count = db.expire_approvals(now, all_open)?;
        for record in &due {
            let reason = if all_open {
                "进程重启，未完成的审批一律过期"
            } else {
                "超过有效期"
            };
            let detail = AuditDetail {
                reason: Some(reason.into()),
                ..AuditDetail::default()
            };
            audit_best_effort(
                db,
                &audit_entry(
                    now,
                    SYSTEM_CLIENT,
                    AuditAction::ApprovalExpired,
                    AuditResult::Ok,
                    record,
                    detail,
                ),
            );
        }
        Ok(count)
    }

    /// 单条审批到期：标为 expired 并记审计。
    fn expire_one(
        &self,
        db: &RuntimeDb,
        record: &ApprovalRecord,
        now: f64,
    ) -> Result<(), ApprovalError> {
        if db.transition_approval(
            &record.approval_id,
            record.status,
            ApprovalStatus::Expired,
            now,
            None,
        )? {
            let detail = AuditDetail {
                reason: Some("超过有效期".into()),
                ..AuditDetail::default()
            };
            audit_best_effort(
                db,
                &audit_entry(
                    now,
                    SYSTEM_CLIENT,
                    AuditAction::ApprovalExpired,
                    AuditResult::Ok,
                    record,
                    detail,
                ),
            );
        }
        Ok(())
    }

    /// 进程启动时调用（技术书 §5.5）：所有 pending / approved 一律过期（A-32）；
    /// 发送过程中被中断的已执行审批（进展仍为 `running`）记为失败、结果未知，不重放（A-33）。
    /// 返回改动的审批条数。
    pub fn recover_after_restart(&self) -> Result<usize, ApprovalError> {
        let now = self.now();
        let db = self.lock()?;
        let mut changed = self.expire_with_audit(&db, now, true)?;
        for record in db.list_running_executed_approvals()? {
            let outcome = ApprovalOutcome {
                success: false,
                message: if record.kind == ApprovalKind::AgentRun {
                    "MOSS 在 Agent 任务运行期间中断，结果未知且不会自动重跑".into()
                } else if record.kind == ApprovalKind::HomeControl {
                    "Home Assistant 控制可能已接受但观测中断，结果未知且不会自动重发".into()
                } else if record.kind == ApprovalKind::ExternalTool {
                    "MOSS 在外部工具调用期间中断，执行结果未知；不会自动重试，请先核实第三方结果"
                        .into()
                } else {
                    "MOSS 在发送过程中中断，设备实际执行到哪里无法确认；不会自动重发，请到机器旁确认后重新发起".into()
                },
                job_id: record
                    .outcome
                    .as_ref()
                    .and_then(|outcome| outcome.job_id.clone()),
                error_code: Some("result_unknown".into()),
                phase: Some(ApprovalPhase::Unknown),
            };
            if db.transition_approval(
                &record.approval_id,
                ApprovalStatus::Executed,
                ApprovalStatus::Failed,
                now,
                Some(&outcome),
            )? {
                changed += 1;
                audit_best_effort(
                    &db,
                    &audit_entry(
                        now,
                        SYSTEM_CLIENT,
                        AuditAction::ApprovalFailed,
                        AuditResult::Failed,
                        &record,
                        AuditDetail {
                            error_code: outcome.error_code.clone(),
                            reason: Some("进程重启时作业仍在发送，结果未知".into()),
                            ..AuditDetail::default()
                        },
                    ),
                );
            }
        }
        // An Agent run cannot be resumed or automatically replayed after a crash.
        db.recover_agent_runs(now)?;
        Ok(changed)
    }

    /// 经审批的发送作业已开始：记进展为 `running`，供重启时识别“发送中被中断”（A-33）。
    pub fn mark_running(&self, approval_id: &str, job_id: &str) -> Result<(), ApprovalError> {
        let db = self.lock()?;
        let outcome = ApprovalOutcome {
            success: true,
            message: "已开始执行，等待加工完成".into(),
            job_id: Some(job_id.to_owned()),
            error_code: None,
            phase: Some(ApprovalPhase::Running),
        };
        db.set_approval_outcome(approval_id, &outcome, self.now())?;
        Ok(())
    }

    /// 作业结束时写回结果（由发送线程调用）。只更新进展仍为 `running` 的审批，
    /// 不覆盖重启时记下的“结果未知”等其他结论。
    pub fn record_job_result(&self, approval_id: &str, status: &str, send_result: &Value) {
        let record = match self.get(approval_id) {
            Ok(record) => record,
            Err(error) => {
                eprintln!(
                    "[laser-api] 审批 {approval_id} 写回作业结果失败: {}",
                    error.message()
                );
                return;
            }
        };
        let current = record.outcome.clone().unwrap_or_default();
        if record.status != ApprovalStatus::Executed
            || current.phase != Some(ApprovalPhase::Running)
        {
            return;
        }
        let sent = send_result
            .get("lines_sent")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total = send_result
            .get("total_lines")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let error_text = send_result
            .get("error")
            .and_then(Value::as_str)
            .map(|text| text.chars().take(200).collect::<String>());
        let outcome = match status {
            "completed" => ApprovalOutcome {
                success: true,
                message: format!("加工完成（{sent}/{total} 行已确认）"),
                phase: Some(ApprovalPhase::Completed),
                ..current
            },
            "cancelled" => ApprovalOutcome {
                success: false,
                message: error_text.unwrap_or_else(|| format!("已取消（{sent}/{total} 行）")),
                error_code: Some("cancelled".into()),
                phase: Some(ApprovalPhase::Cancelled),
                ..current
            },
            _ => ApprovalOutcome {
                success: false,
                message: error_text.unwrap_or_else(|| format!("加工失败（{sent}/{total} 行）")),
                error_code: Some("job_failed".into()),
                phase: Some(ApprovalPhase::Failed),
                ..current
            },
        };
        if let Err(error) = self.record_outcome(approval_id, outcome) {
            eprintln!(
                "[laser-api] 审批 {approval_id} 写回作业结果失败: {}",
                error.message()
            );
        }
    }

    /// 把已到期的 pending / approved 标为 expired。
    pub fn expire_due(&self) -> Result<usize, ApprovalError> {
        let now = self.now();
        let db = self.lock()?;
        self.expire_with_audit(&db, now, false)
    }

    /// 创建审批。可信调用方 + 原请求编号生成作用域哈希；相同编号但不同内容返回冲突。
    /// 审批与“创建”审计（以及删除 `confirmed` 的审计）在同一事务中写入，审计写不进去就不创建。
    pub fn create(&self, request: NewApproval) -> Result<ApprovalRecord, ApprovalError> {
        if request.target_record.trim().is_empty() {
            return Err(ApprovalError::InvalidRequest("缺少目标记录".into()));
        }
        if request.summary.trim().is_empty() {
            return Err(ApprovalError::InvalidRequest("缺少人类可读摘要".into()));
        }
        // Dedicated HA controls can only be created by a trusted voice caller
        // with an explicit live permit and a fingerprinted Home connection.
        // Never accept a model-selected arbitrary URL/command or public route.
        if request.kind == ApprovalKind::HomeControl
            && (request.source != ApprovalSource::Voice
                || request.session_id.as_deref().is_none_or(str::is_empty)
                || request.voice_permit.is_none()
                || !valid_home_target(&request.target_record)
                || request.summary.chars().count() > 200
                || !matches!(&request.connection,
                    ApprovalConnection::Home { fingerprint }
                    if valid_home_fingerprint(fingerprint)))
        {
            return Err(ApprovalError::InvalidRequest(
                "HomeControl 必须经独立语音身份、设备授权与可信配置指纹创建".into(),
            ));
        }
        let raw_id = request
            .request_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        if raw_id.is_some_and(|id| id.chars().count() > 128) {
            return Err(ApprovalError::InvalidRequest(
                "请求编号不能超过 128 个字符".into(),
            ));
        }
        // Keep the existing UNIQUE(source, request_id) schema without reusing legacy unscoped keys.
        // Unidentified MCP bridge callers must never share somebody else's approval by guessing an id.
        let request_id = request
            .owner
            .as_deref()
            .filter(|owner| !owner.is_empty())
            .zip(raw_id)
            .map(|(owner, id)| {
                let bytes = serde_json::to_vec(&(owner, id)).expect("strings serialize");
                format!("scope-v1:{}", hex::encode(Sha256::digest(bytes)))
            });
        let digest = approval_digest(
            request.kind,
            &request.target_record,
            &request.facts,
            &request.connection,
        );
        let db = self.lock()?;
        let now = self.now();
        if request.source == ApprovalSource::Voice {
            if !request
                .voice_permit
                .as_ref()
                .is_some_and(|permit| permit.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(ApprovalError::Expired);
            }
            let device = request
                .session_id
                .as_deref()
                .ok_or(ApprovalError::Expired)?;
            if !db
                .voice_device(device)?
                .is_some_and(|record| record.is_allowed())
            {
                return Err(ApprovalError::Expired);
            }
        }
        if let Some(request_id) = request_id.as_deref()
            && let Some(existing) = db.find_approval_by_request(request.source, request_id)?
        {
            if existing.digest != digest || existing.request.as_ref() != Some(&request.request) {
                return Err(ApprovalError::RequestConflict);
            }
            if existing.is_expired_at(now) {
                self.expire_one(&db, &existing, now)?;
                return db
                    .get_approval(&existing.approval_id)?
                    .ok_or(ApprovalError::NotFound);
            }
            return Ok(existing);
        }
        let record = ApprovalRecord {
            approval_id: new_approval_id()?,
            status: ApprovalStatus::Pending,
            kind: request.kind,
            target_record: request.target_record,
            digest,
            summary: request.summary,
            source: request.source,
            session_id: request.session_id,
            request_id,
            connection: Some(request.connection),
            request: Some(request.request),
            created_at: now,
            decided_at: None,
            expires_at: now + self.ttl_seconds as f64,
            updated_at: now,
            outcome: None,
        };
        let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
        db.insert_approval(&record)?;
        db.append_audit(&audit_entry(
            now,
            &request.client,
            AuditAction::ApprovalCreated,
            AuditResult::Ok,
            &record,
            AuditDetail::default(),
        ))
        .map_err(audit_failed)?;
        if !request.removed_fields.is_empty() {
            db.append_audit(&audit_entry(
                now,
                &request.client,
                AuditAction::ConfirmedFieldStripped,
                AuditResult::Ok,
                &record,
                AuditDetail {
                    removed_fields: request.removed_fields.clone(),
                    reason: Some("模型提供的确认不构成授权，需在桌面批准".into()),
                    ..AuditDetail::default()
                },
            ))
            .map_err(audit_failed)?;
        }
        tx.commit().map_err(sql_failed)?;
        Ok(record)
    }

    /// 读取审批；未到期时仅走只读连接，已到期的才升级写锁标为 expired 再返回。
    pub fn get(&self, approval_id: &str) -> Result<ApprovalRecord, ApprovalError> {
        let now = self.now();
        {
            let read_db = self.lock_read()?;
            let record = read_db
                .get_approval(approval_id)?
                .ok_or(ApprovalError::NotFound)?;
            if !record.is_expired_at(now) {
                return Ok(record);
            }
        }
        let db = self.lock()?;
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        if record.is_expired_at(now) {
            self.expire_one(&db, &record, now)?;
            return db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound);
        }
        Ok(record)
    }

    /// 列出审批（先检查是否有到期项；无到期项时纯走只读连接，不抢主写锁），新的在前。
    pub fn list(
        &self,
        status: Option<ApprovalStatus>,
        limit: usize,
    ) -> Result<Vec<ApprovalRecord>, ApprovalError> {
        let now = self.now();
        {
            let read_db = self.lock_read()?;
            if read_db.approvals_due_for_expiry(now, false)?.is_empty() {
                return Ok(read_db.list_approvals(status, limit)?);
            }
        }
        let db = self.lock()?;
        self.expire_with_audit(&db, now, false)?;
        Ok(db.list_approvals(status, limit)?)
    }

    /// 桌面批准。`submitted_digest` 是审批卡提交的摘要，`current_digest` 是调用方按当前
    /// 记录与连接重算的摘要；两者都必须等于创建时的摘要，否则 `approval_mismatch`，
    /// 状态不变（请求在有效期后自然过期）。批准与审计在同一事务中，审计写不进去就不批准。
    pub fn approve(
        &self,
        approval_id: &str,
        submitted_digest: &str,
        current_digest: &str,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let db = self.lock()?;
        let now = self.now();
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        match record.status {
            ApprovalStatus::Pending => {}
            ApprovalStatus::Expired => return Err(ApprovalError::Expired),
            ApprovalStatus::Rejected => return Err(ApprovalError::Rejected),
            ApprovalStatus::Approved | ApprovalStatus::Executed | ApprovalStatus::Failed => {
                return Err(ApprovalError::Consumed);
            }
        }
        if record.is_expired_at(now) {
            self.expire_one(&db, &record, now)?;
            return Err(ApprovalError::Expired);
        }
        if submitted_digest != record.digest || current_digest != record.digest {
            return Err(ApprovalError::Mismatch);
        }
        let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
        if !db.transition_approval(
            approval_id,
            ApprovalStatus::Pending,
            ApprovalStatus::Approved,
            now,
            None,
        )? {
            drop(tx);
            return Err(status_error(&db, approval_id)?);
        }
        db.append_audit(&audit_entry(
            now,
            DESKTOP_CLIENT,
            AuditAction::ApprovalApproved,
            AuditResult::Ok,
            &record,
            AuditDetail::default(),
        ))
        .map_err(audit_failed)?;
        tx.commit().map_err(sql_failed)?;
        db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound)
    }

    /// 桌面拒绝，只对 pending 有效。`reason` 只记入结果摘要，不含路径与凭据。
    /// 拒绝是降低风险的动作，审计写入失败不影响拒绝生效。
    pub fn reject(
        &self,
        approval_id: &str,
        reason: Option<&str>,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let db = self.lock()?;
        let now = self.now();
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        match record.status {
            ApprovalStatus::Pending => {}
            ApprovalStatus::Expired => return Err(ApprovalError::Expired),
            ApprovalStatus::Rejected => return Err(ApprovalError::Rejected),
            ApprovalStatus::Approved | ApprovalStatus::Executed | ApprovalStatus::Failed => {
                return Err(ApprovalError::Consumed);
            }
        }
        let outcome = ApprovalOutcome {
            success: false,
            message: reason
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(|reason| reason.chars().take(200).collect())
                .unwrap_or_else(|| "已在桌面拒绝".into()),
            job_id: None,
            error_code: None,
            ..ApprovalOutcome::default()
        };
        if !db.transition_approval(
            approval_id,
            ApprovalStatus::Pending,
            ApprovalStatus::Rejected,
            now,
            Some(&outcome),
        )? {
            return Err(status_error(&db, approval_id)?);
        }
        audit_best_effort(
            &db,
            &audit_entry(
                now,
                DESKTOP_CLIENT,
                AuditAction::ApprovalRejected,
                AuditResult::Ok,
                &record,
                AuditDetail {
                    reason: Some(outcome.message.clone()),
                    ..AuditDetail::default()
                },
            ),
        );
        db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound)
    }

    /// 执行前消费授权（技术书 §5.5）：必须已批准、未过期、`current_digest` 与批准时一致；
    /// 成功后状态为 executed，返回含连接参数的记录供执行使用。摘要不一致时授权作废（failed）。
    /// 消费与“已执行”审计在同一事务中：审计写不进去则不消费、不执行（A-34）。
    pub fn consume(
        &self,
        approval_id: &str,
        current_digest: &str,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let db = self.lock()?;
        let now = self.now();
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        if matches!(
            record.kind,
            ApprovalKind::AgentRun | ApprovalKind::HomeControl
        ) {
            return Err(ApprovalError::Mismatch);
        }
        match record.status {
            ApprovalStatus::Approved => {}
            ApprovalStatus::Pending => return Err(ApprovalError::NotApproved),
            ApprovalStatus::Expired => return Err(ApprovalError::Expired),
            ApprovalStatus::Rejected => return Err(ApprovalError::Rejected),
            ApprovalStatus::Executed | ApprovalStatus::Failed => {
                return Err(ApprovalError::Consumed);
            }
        }
        if record.is_expired_at(now) {
            self.expire_one(&db, &record, now)?;
            return Err(ApprovalError::Expired);
        }
        if current_digest != record.digest {
            let outcome = ApprovalOutcome {
                success: false,
                message: "执行时摘要与批准时不一致，未发送".into(),
                job_id: None,
                error_code: Some(ApprovalError::Mismatch.code().into()),
                ..ApprovalOutcome::default()
            };
            if db.transition_approval(
                approval_id,
                ApprovalStatus::Approved,
                ApprovalStatus::Failed,
                now,
                Some(&outcome),
            )? {
                audit_best_effort(
                    &db,
                    &audit_entry(
                        now,
                        SYSTEM_CLIENT,
                        AuditAction::ApprovalFailed,
                        AuditResult::Failed,
                        &record,
                        AuditDetail {
                            error_code: outcome.error_code.clone(),
                            reason: Some(outcome.message.clone()),
                            ..AuditDetail::default()
                        },
                    ),
                );
            }
            return Err(ApprovalError::Mismatch);
        }
        let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
        if !db.consume_approval(approval_id, now)? {
            drop(tx);
            return Err(status_error(&db, approval_id)?);
        }
        if record.kind == ApprovalKind::ExternalTool {
            db.set_approval_outcome(
                approval_id,
                &ApprovalOutcome {
                    success: false,
                    message: "外部调用正在执行，尚未收到结果".into(),
                    phase: Some(ApprovalPhase::Running),
                    ..Default::default()
                },
                now,
            )?;
        }
        db.append_audit(&audit_entry(
            now,
            DESKTOP_CLIENT,
            AuditAction::ApprovalExecuted,
            AuditResult::Ok,
            &record,
            AuditDetail::default(),
        ))
        .map_err(audit_failed)?;
        tx.commit().map_err(sql_failed)?;
        db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound)
    }

    /// NV15 only: atomic consumption of a dedicated HA approval.
    /// This method is NOT registered to any production voice tool or POST
    /// sender. The caller must revalidate live permit immediately before
    /// any future network send, and may never retry an uncertain send.
    pub fn consume_home_control(
        &self,
        approval_id: &str,
        current_digest: &str,
        current_fingerprint: &str,
        device_id: &str,
        live_permit: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ApprovalRecord, ApprovalError> {
        use std::sync::atomic::Ordering;
        let db = self.lock()?;
        let now = self.now();
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        let Some(ApprovalConnection::Home { fingerprint }) = &record.connection else {
            return Err(ApprovalError::Mismatch);
        };
        if record.kind != ApprovalKind::HomeControl
            || record.source != ApprovalSource::Voice
            || record.session_id.as_deref() != Some(device_id)
            || !valid_home_fingerprint(current_fingerprint)
            || fingerprint != current_fingerprint
        {
            return Err(ApprovalError::Mismatch);
        }
        match record.status {
            ApprovalStatus::Approved => {}
            ApprovalStatus::Pending => return Err(ApprovalError::NotApproved),
            ApprovalStatus::Expired => return Err(ApprovalError::Expired),
            ApprovalStatus::Rejected => return Err(ApprovalError::Rejected),
            ApprovalStatus::Executed | ApprovalStatus::Failed => {
                return Err(ApprovalError::Consumed);
            }
        }
        if record.is_expired_at(now) {
            self.expire_one(&db, &record, now)?;
            return Err(ApprovalError::Expired);
        }
        if current_digest != record.digest {
            return Err(ApprovalError::Mismatch);
        }
        if !live_permit.load(Ordering::Acquire)
            || !db
                .voice_device(device_id)?
                .is_some_and(|row| row.is_allowed())
        {
            return Err(ApprovalError::Expired);
        }
        let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
        if !live_permit.load(Ordering::Acquire) {
            return Err(ApprovalError::Expired);
        }
        if !db.consume_approval(approval_id, now)? {
            return Err(ApprovalError::Consumed);
        }
        // Running is persistently recorded BEFORE any potential future send.
        // recover_after_restart moves it to result_unknown and never retries.
        db.set_approval_outcome(
            approval_id,
            &ApprovalOutcome {
                success: false,
                message: "Home Assistant 服务请求结果待确认；不得自动重发".into(),
                job_id: None,
                error_code: None,
                phase: Some(ApprovalPhase::Running),
            },
            now,
        )?;
        db.append_audit(&audit_entry(
            now,
            DESKTOP_CLIENT,
            AuditAction::ApprovalExecuted,
            AuditResult::Ok,
            &record,
            AuditDetail::default(),
        ))
        .map_err(audit_failed)?;
        tx.commit().map_err(sql_failed)?;
        db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound)
    }

    /// NV15 dedicated private-desktop revocation. Pending approvals become
    /// rejected; already approved-but-not-consumed approvals become expired.
    /// Never touches an executed, unknown or failed HomeControl request.
    /// The caller must authenticate as the packaged local private window.
    pub fn cancel_home_control(
        &self,
        approval_id: &str,
        submitted_digest: &str,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let db = self.lock()?;
        let now = self.now();
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        if record.kind != ApprovalKind::HomeControl
            || submitted_digest != record.digest
            || !matches!(record.connection, Some(ApprovalConnection::Home { .. }))
        {
            return Err(ApprovalError::Mismatch);
        }
        let target = match record.status {
            ApprovalStatus::Pending => ApprovalStatus::Rejected,
            ApprovalStatus::Approved => ApprovalStatus::Expired,
            ApprovalStatus::Expired => return Err(ApprovalError::Expired),
            ApprovalStatus::Rejected => return Err(ApprovalError::Rejected),
            ApprovalStatus::Executed | ApprovalStatus::Failed => {
                return Err(ApprovalError::Consumed);
            }
        };
        if record.is_expired_at(now) {
            self.expire_one(&db, &record, now)?;
            return Err(ApprovalError::Expired);
        }
        let outcome = ApprovalOutcome {
            success: false,
            message: "家居控制已在桌面撤销，未授权新的发送".into(),
            error_code: Some("home_cancelled".into()),
            phase: Some(ApprovalPhase::Cancelled),
            ..Default::default()
        };
        // SQLite serializes this state change with consume_home_control.
        // Revocation must persist even when the best-effort audit fails.
        let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
        if !db.transition_approval(approval_id, record.status, target, now, Some(&outcome))? {
            return Err(status_error(&db, approval_id)?);
        }
        audit_best_effort(
            &db,
            &audit_entry(
                now,
                DESKTOP_CLIENT,
                if target == ApprovalStatus::Rejected {
                    AuditAction::ApprovalRejected
                } else {
                    AuditAction::ApprovalExpired
                },
                AuditResult::Ok,
                &record,
                AuditDetail {
                    reason: Some("HomeControl 桌面主动撤销".into()),
                    ..AuditDetail::default()
                },
            ),
        );
        tx.commit().map_err(sql_failed)?;
        db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound)
    }

    /// NV13: consume a single AgentRun approval AND create the durable running
    /// record within one SQLite transaction. Does not launch a process.
    pub fn consume_agent_run(
        &self,
        approval_id: &str,
        current_digest: &str,
        run: &laser_runtime::AgentRunRecord,
        live_permit: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ApprovalRecord, ApprovalError> {
        use std::sync::atomic::Ordering;
        let db = self.lock()?;
        let now = self.now();
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        if record.kind != ApprovalKind::AgentRun
            || record.source != ApprovalSource::Voice
            || !matches!(record.connection, Some(ApprovalConnection::Agent { .. }))
            || record.approval_id != run.approval_id
            || run.status != "running"
        {
            return Err(ApprovalError::Mismatch);
        }
        match record.status {
            ApprovalStatus::Approved => {}
            ApprovalStatus::Pending => return Err(ApprovalError::NotApproved),
            ApprovalStatus::Expired => return Err(ApprovalError::Expired),
            ApprovalStatus::Rejected => return Err(ApprovalError::Rejected),
            ApprovalStatus::Executed | ApprovalStatus::Failed => {
                return Err(ApprovalError::Consumed);
            }
        }
        if record.is_expired_at(now) {
            self.expire_one(&db, &record, now)?;
            return Err(ApprovalError::Expired);
        }
        if record.digest != current_digest {
            return Err(ApprovalError::Mismatch);
        }
        if !live_permit.load(Ordering::Acquire) {
            return Err(ApprovalError::Expired);
        }
        let device = record.session_id.as_deref().ok_or(ApprovalError::Expired)?;
        if !db.voice_device(device)?.is_some_and(|row| row.is_allowed()) {
            return Err(ApprovalError::Expired);
        }
        let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
        if !live_permit.load(Ordering::Acquire) {
            return Err(ApprovalError::Expired);
        }
        if !db.consume_approval(approval_id, now)? {
            return Err(ApprovalError::Consumed);
        }
        db.insert_agent_run(run)?;
        db.set_approval_outcome(
            approval_id,
            &ApprovalOutcome {
                success: false,
                message: "Agent 任务正在运行".into(),
                job_id: Some(run.id.clone()),
                error_code: None,
                phase: Some(ApprovalPhase::Running),
            },
            now,
        )?;
        db.append_audit(&audit_entry(
            now,
            DESKTOP_CLIENT,
            AuditAction::ApprovalExecuted,
            AuditResult::Ok,
            &record,
            AuditDetail::default(),
        ))
        .map_err(audit_failed)?;
        tx.commit().map_err(sql_failed)?;
        db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound)
    }

    /// NV13: finish one consumed AgentRun and its approval together.
    pub fn finish_agent_run(
        &self,
        run_id: &str,
        status: &str,
        output: &str,
        truncated: bool,
        error_code: Option<&str>,
    ) -> Result<bool, ApprovalError> {
        let db = self.lock()?;
        let now = self.now();
        let Some(run) = db.find_agent_run(run_id)? else {
            return Err(ApprovalError::NotFound);
        };
        let record = db
            .get_approval(&run.approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        if record.kind != ApprovalKind::AgentRun || record.status != ApprovalStatus::Executed {
            return Err(ApprovalError::Consumed);
        }
        let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
        if !db.finish_agent_run(run_id, status, output, truncated, error_code, now)? {
            return Err(ApprovalError::Consumed);
        }
        let success = status == "completed";
        let phase = match status {
            "completed" => ApprovalPhase::Completed,
            "cancelled" => ApprovalPhase::Cancelled,
            _ => ApprovalPhase::Failed,
        };
        let outcome = ApprovalOutcome {
            success,
            message: if success {
                "Agent 任务执行完成".into()
            } else {
                "Agent 任务未成功完成".into()
            },
            job_id: Some(run.id.clone()),
            error_code: error_code.map(str::to_owned),
            phase: Some(phase),
        };
        if success {
            db.set_approval_outcome(&run.approval_id, &outcome, now)?;
        } else {
            if !db.transition_approval(
                &run.approval_id,
                ApprovalStatus::Executed,
                ApprovalStatus::Failed,
                now,
                Some(&outcome),
            )? {
                return Err(ApprovalError::Consumed);
            }
            db.append_audit(&audit_entry(
                now,
                SYSTEM_CLIENT,
                AuditAction::ApprovalFailed,
                AuditResult::Failed,
                &record,
                AuditDetail {
                    error_code: outcome.error_code.clone(),
                    reason: Some("Agent 执行未完成".into()),
                    ..AuditDetail::default()
                },
            ))
            .map_err(audit_failed)?;
        }
        tx.commit().map_err(sql_failed)?;
        Ok(true)
    }

    /// 记录执行结果：失败时 executed → failed（不可重用）并记审计；成功时只写结果摘要。
    pub fn record_outcome(
        &self,
        approval_id: &str,
        outcome: ApprovalOutcome,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let db = self.lock()?;
        let now = self.now();
        let record = db
            .get_approval(approval_id)?
            .ok_or(ApprovalError::NotFound)?;
        // HA controls are strictly one-shot, including terminal outcomes.
        // Never allow a later success to overwrite an unknown result.
        if record.kind == ApprovalKind::HomeControl {
            if record.status != ApprovalStatus::Executed
                || record.outcome.as_ref().and_then(|value| value.phase)
                    != Some(ApprovalPhase::Running)
            {
                return Err(ApprovalError::Consumed);
            }
            // A fake "running"/"completed but failed" outcome must never
            // extend a one-shot HA approval's lifetime or mask uncertainty.
            let terminal = matches!(
                (outcome.success, outcome.phase),
                (true, Some(ApprovalPhase::Completed))
                    | (
                        false,
                        Some(
                            ApprovalPhase::Unknown
                                | ApprovalPhase::Failed
                                | ApprovalPhase::Cancelled
                        ),
                    )
            );
            if !terminal {
                return Err(ApprovalError::Mismatch);
            }
            // Record the HA terminal outcome and its fixed-code audit in ONE
            // SQLite transaction. If audit insertion fails, the terminal
            // outcome rolls back: an already-consumed control remains Running,
            // and restart recovery makes it result_unknown (never resends).
            let tx = db.connection.unchecked_transaction().map_err(sql_failed)?;
            let recorded = if outcome.success {
                db.set_approval_outcome(approval_id, &outcome, now)?
            } else {
                db.transition_approval(
                    approval_id,
                    ApprovalStatus::Executed,
                    ApprovalStatus::Failed,
                    now,
                    Some(&outcome),
                )?
            };
            if !recorded {
                return Err(ApprovalError::Consumed);
            }
            let (action, result, reason) = if outcome.success {
                (
                    AuditAction::ApprovalExecuted,
                    AuditResult::Ok,
                    "home_confirmed",
                )
            } else {
                (
                    AuditAction::ApprovalFailed,
                    AuditResult::Failed,
                    "home_result_unknown",
                )
            };
            db.append_audit(&audit_entry(
                now,
                SYSTEM_CLIENT,
                action,
                result,
                &record,
                AuditDetail {
                    error_code: outcome.error_code.clone(),
                    reason: Some(reason.into()),
                    ..AuditDetail::default()
                },
            ))
            .map_err(audit_failed)?;
            tx.commit().map_err(sql_failed)?;
            return db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound);
        }
        let written = match (record.status, outcome.success) {
            (ApprovalStatus::Executed, false) => {
                let changed = db.transition_approval(
                    approval_id,
                    ApprovalStatus::Executed,
                    ApprovalStatus::Failed,
                    now,
                    Some(&outcome),
                )?;
                if changed {
                    audit_best_effort(
                        &db,
                        &audit_entry(
                            now,
                            SYSTEM_CLIENT,
                            AuditAction::ApprovalFailed,
                            AuditResult::Failed,
                            &record,
                            AuditDetail {
                                error_code: outcome.error_code.clone(),
                                reason: Some(outcome.message.chars().take(200).collect()),
                                ..AuditDetail::default()
                            },
                        ),
                    );
                }
                changed
            }
            (ApprovalStatus::Executed | ApprovalStatus::Failed, _) => {
                db.set_approval_outcome(approval_id, &outcome, now)?
            }
            _ => return Err(ApprovalError::NotApproved),
        };
        if !written {
            return Err(status_error(&db, approval_id)?);
        }
        db.get_approval(approval_id)?.ok_or(ApprovalError::NotFound)
    }
}

/// 条件更新没有生效时（并发改了状态），按最新状态给出错误。
fn status_error(db: &RuntimeDb, approval_id: &str) -> Result<ApprovalError, ApprovalError> {
    let record = db
        .get_approval(approval_id)?
        .ok_or(ApprovalError::NotFound)?;
    Ok(match record.status {
        ApprovalStatus::Pending => ApprovalError::NotApproved,
        ApprovalStatus::Expired => ApprovalError::Expired,
        ApprovalStatus::Rejected => ApprovalError::Rejected,
        ApprovalStatus::Approved | ApprovalStatus::Executed | ApprovalStatus::Failed => {
            ApprovalError::Consumed
        }
    })
}

fn new_approval_id() -> Result<String, ApprovalError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| ApprovalError::Unavailable(error.to_string()))?;
    Ok(format!("{APPROVAL_ID_PREFIX}{}", hex::encode(random)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 规范 JSON（技术书 §5.4）：键名按字节序排序、无多余空白、UTF-8。
/// 不依赖 `serde_json::Map` 的内部顺序（启用 `preserve_order` 时会保留插入顺序）。
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

fn target_key(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::WorkflowConfirmSend => "workflow_id",
        ApprovalKind::CalibrationSend => "calibration_id",
        ApprovalKind::SafeAction
        | ApprovalKind::Framing
        | ApprovalKind::ExternalTool
        | ApprovalKind::AgentRun
        | ApprovalKind::HomeControl => "target_record",
    }
}

/// 动作摘要：`SHA-256(canonical_json(payload))`（技术书 §5.4）。payload = 动作要素 + 类别 + 目标记录
/// 加 `target{mode, host_fingerprint}`；`host_fingerprint` 是解析后连接描述的 SHA-256，摘要里不出现地址明文。
/// 保留键（kind、目标记录键、target）以服务端的值为准，`facts` 不能覆盖。
pub fn approval_digest(
    kind: ApprovalKind,
    target_record: &str,
    facts: &Map<String, Value>,
    connection: &ApprovalConnection,
) -> String {
    let mut payload = facts.clone();
    payload.insert("kind".into(), Value::String(kind.as_str().into()));
    payload.insert(target_key(kind).into(), Value::String(target_record.into()));
    payload.insert(
        "target".into(),
        serde_json::json!({
            "mode": connection.mode(),
            "host_fingerprint": sha256_hex(connection.describe().as_bytes()),
        }),
    );
    sha256_hex(canonical_json(&Value::Object(payload)).as_bytes())
}

/// 从工作流 / 标定记录与执行请求整理摘要要素（§5.4）：G-code SHA、材料、厚度、参数、发送方式。
/// 重新预览会改变 G-code SHA，改发送方式会改变 `send_mode`，都会让旧授权失效。
pub fn record_digest_facts(record: &Value, request: &Map<String, Value>) -> Map<String, Value> {
    let lookup = |section: &str, key: &str| {
        record
            .get(section)
            .and_then(|value| value.get(key))
            .cloned()
    };
    let first = |key: &str| {
        lookup("artifacts", key)
            .or_else(|| lookup("confirmation_snapshot", key))
            .or_else(|| lookup("input", key))
            .or_else(|| record.get(key).cloned())
            .filter(|value| !value.is_null())
    };
    let mut facts = Map::new();
    for key in ["gcode_sha256", "material", "thickness_mm"] {
        if let Some(value) = first(key) {
            facts.insert(key.into(), value);
        }
    }
    let mut params = Map::new();
    for key in [
        "power",
        "laser_max_power",
        "laser_min_power",
        "power_scale",
        "feed_rate",
        "passes",
    ] {
        if let Some(value) = lookup("input", key)
            .or_else(|| lookup("confirmation_snapshot", key))
            .or_else(|| lookup("params", key))
            .filter(|value| !value.is_null())
        {
            params.insert(key.into(), value);
        }
    }
    if !params.is_empty() {
        facts.insert("params".into(), Value::Object(params));
    }
    let offline = request
        .get("offline_sd")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let send_mode = request
        .get("send_mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if offline {
                "sd_offline".into()
            } else {
                "stream".into()
            }
        });
    facts.insert("send_mode".into(), Value::String(send_mode));
    if let Some(line) = request
        .get("start_from_line")
        .and_then(Value::as_u64)
        .filter(|line| *line > 0)
    {
        facts.insert("start_from_line".into(), Value::from(line));
    }
    facts
}

/// 按请求中的显式参数解析审批目标；缺省时只用环境变量（`.env`）默认值：
/// `LASER_DEFAULT_CONNECTION_MODE`、`LASER_NETWORK_HOST`、`LASER_NETWORK_TELNET_PORT`、
/// `GRBL_DEFAULT_PORT`、`GRBL_BAUDRATE`。**不枚举系统串口自动选择**（验收 A-35）。
pub fn resolve_approval_target(
    request: &Map<String, Value>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ApprovalConnection, ApprovalError> {
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| request.get(*key).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let number = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            request.get(*key).and_then(|value| {
                value.as_u64().or_else(|| {
                    value
                        .as_str()
                        .and_then(|text| text.trim().parse::<u64>().ok())
                })
            })
        })
    };
    let env_text = |name: &str| {
        env(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty() && !value.starts_with('<'))
    };
    let port = text(&["port", "serial_port"][..]);
    let host = text(&["network_host", "host"][..]);
    let mode = text(&["connection_mode", "preferred_mode"][..])
        .map(|mode| mode.to_ascii_lowercase())
        .filter(|mode| mode == "serial" || mode == "network");
    let serial = match mode.as_deref() {
        Some("serial") => true,
        Some(_) => false,
        None if port.is_some() && host.is_none() => true,
        None if host.is_some() => false,
        None => env_text("LASER_DEFAULT_CONNECTION_MODE")
            .is_some_and(|mode| mode.eq_ignore_ascii_case("serial")),
    };
    if serial {
        let port = port
            .or_else(|| env_text("GRBL_DEFAULT_PORT"))
            .ok_or_else(|| {
                ApprovalError::TargetRequired(
                    "串口模式需要指定串口（或在 .env 配置 GRBL_DEFAULT_PORT），不会自动选择".into(),
                )
            })?;
        let baud = number(&["baudrate", "baud"][..])
            .or_else(|| env_text("GRBL_BAUDRATE").and_then(|value| value.parse().ok()))
            .unwrap_or(115_200);
        if !(1200..=4_000_000).contains(&baud) {
            return Err(ApprovalError::InvalidRequest(
                "baudrate 超出合法范围 (1200 ~ 4000000)".into(),
            ));
        }
        Ok(ApprovalConnection::Serial {
            port,
            baud_rate: baud as u32,
        })
    } else {
        let host = host
            .or_else(|| env_text("LASER_NETWORK_HOST"))
            .ok_or_else(|| {
                ApprovalError::TargetRequired(
                    "网络模式需要指定设备地址（或在 .env 配置 LASER_NETWORK_HOST）".into(),
                )
            })?;
        let port = number(&["network_telnet_port", "telnet_port"][..])
            .or_else(|| env_text("LASER_NETWORK_TELNET_PORT").and_then(|value| value.parse().ok()))
            .unwrap_or(23);
        if !(1..=65535).contains(&port) {
            return Err(ApprovalError::InvalidRequest(
                "network_telnet_port 超出合法范围".into(),
            ));
        }
        Ok(ApprovalConnection::Network {
            host,
            port: port as u16,
        })
    }
}

/// 用进程环境变量（启动时已加载 `.env`）解析审批目标。
pub fn resolve_approval_target_from_env(
    request: &Map<String, Value>,
) -> Result<ApprovalConnection, ApprovalError> {
    resolve_approval_target(request, &|name: &str| std::env::var(name).ok())
}

// ---------------------------------------------------------------------------
// 严格模式（技术书 §6.2 / §6.4 规则 3；实施清单 P1-04）
// ---------------------------------------------------------------------------

/// 接口失败值：HTTP 状态 + `{success, result, detail}` 包装体。
pub(crate) type HttpFailure = (u16, Value);

/// 一个 R3 动作（运动或出光）的授权检查输入。
pub(crate) struct R3Action<'a> {
    pub kind: ApprovalKind,
    pub target_record: &'a str,
    /// 参与摘要的动作要素（不含连接目标）
    pub facts: Map<String, Value>,
    /// 原始请求：非桌面来源创建审批时从中解析显式目标、读取 `request_id`
    pub request: &'a Map<String, Value>,
    /// 审批卡上显示的人类可读摘要，不含路径与凭据
    pub summary: String,
}

/// 授权检查结果。
#[derive(Debug)]
pub(crate) enum R3Authority {
    /// 严格模式关闭：不追加任何条件，行为与之前一致
    Unchecked,
    /// 桌面会话
    Desktop,
    /// 已消费的审批授权；执行必须使用其中保存的连接参数
    Approval(ApprovalRecord),
}

pub(crate) fn error_response(error: &ApprovalError) -> HttpFailure {
    (
        error.http_status(),
        crate::failure(
            error.message(),
            Some(serde_json::json!({"error_code": error.code()})),
        ),
    )
}

fn strict_blocked() -> HttpFailure {
    (
        403,
        crate::failure(
            "严格模式下，运动或出光动作需要在 MOSS 桌面端批准后才能执行",
            Some(serde_json::json!({"error_code": "strict_mode_blocked"})),
        ),
    )
}

/// 已创建审批、等待桌面批准（技术书 §5.1 响应示例）。`success=true` 让小智播报 `speech`，
/// HTTP 202 让调用方知道动作尚未执行；调用方不得据此改动记录状态。
pub(crate) fn approval_required_response(record: &ApprovalRecord) -> HttpFailure {
    (
        202,
        crate::success(serde_json::json!({
            "status": "approval_required",
            "approval_id": record.approval_id,
            "expires_at": record.expires_at,
            "summary": record.summary,
            "speech": "已提交，请在电脑上的 MOSS 里确认后开始。",
        })),
    )
}

/// 请求里的连接字段：审批单独保存解析后的连接，执行时再用它覆盖。
const CONNECTION_FIELDS: [&str; 10] = [
    "connection_mode",
    "preferred_mode",
    "port",
    "serial_port",
    "baudrate",
    "baud",
    "network_host",
    "host",
    "network_telnet_port",
    "telnet_port",
];

/// 审批保存的执行参数：去掉连接字段（单独保存）与幂等键。身份字段在 HTTP 入口已删除。
fn stored_request(request: &Map<String, Value>) -> Map<String, Value> {
    let mut stored = request.clone();
    for key in CONNECTION_FIELDS {
        stored.remove(key);
    }
    stored.remove("request_id");
    // 调用方自报的确认不构成授权（技术书 §4.3 r5 说明）；批准后执行时由桌面批准提供 confirmed=true
    stored.remove("confirmed");
    stored
}

/// 请求里是否带了 `confirmed=true`（小智模型自行填写）。
fn model_confirmed(request: &Map<String, Value>) -> bool {
    match request.get("confirmed") {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => value.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// 用审批记录里保存的连接参数覆盖请求中的连接字段（“批准的就是执行的”，技术书 §5.4）。
pub(crate) fn object_with_connection(
    object: &Map<String, Value>,
    connection: &ApprovalConnection,
) -> Map<String, Value> {
    let mut effective = object.clone();
    for key in CONNECTION_FIELDS {
        effective.remove(key);
    }
    effective.insert(
        "connection_mode".into(),
        Value::String(connection.mode().into()),
    );
    match connection {
        ApprovalConnection::External { .. }
        | ApprovalConnection::Agent { .. }
        | ApprovalConnection::Home { .. } => {}
        ApprovalConnection::Network { host, port } => {
            effective.insert("network_host".into(), Value::String(host.clone()));
            effective.insert("network_telnet_port".into(), Value::from(*port));
        }
        ApprovalConnection::Serial { port, baud_rate } => {
            effective.insert("port".into(), Value::String(port.clone()));
            effective.insert("baudrate".into(), Value::from(*baud_rate));
        }
    }
    effective
}

/// Source identity for replay protection. The legacy bridge has no authenticated per-peer id;
/// it deliberately gets no deduplication rather than sharing authorization across endpoints.
fn request_owner(ctx: &CallerContext) -> Option<String> {
    let identity = match ctx {
        CallerContext::Voice {
            device_id,
            session_id,
        } if !device_id.is_empty() && !session_id.is_empty() => {
            serde_json::json!(["voice", device_id, session_id])
        }
        CallerContext::Lan { pairing_id } if !pairing_id.is_empty() => {
            serde_json::json!(["lan", pairing_id])
        }
        CallerContext::Gateway { session_id } if !session_id.is_empty() => {
            serde_json::json!(["gateway", session_id])
        }
        _ => return None,
    };
    Some(identity.to_string())
}

impl crate::ApiService {
    /// 唯一的 R3 授权检查点（技术书 §6.4 规则 3）。
    ///
    /// - `CallerContext::Approval`：按当前要素与审批保存的连接重算摘要并单次消费，不论严格模式开关；
    /// - 严格模式关闭：`Unchecked`，不改变任何现有判定（A-40），内置语音同样如此（2026-10-07 用户决定，技术书 §5.2）；
    /// - 桌面会话：放行；
    /// - 小智桥接 / 局域网配对 / 网关 / 内置语音：解析显式目标（拿不到即 `target_required`）并创建审批，
    ///   返回 `approval_required`，动作不执行；
    /// - 本机回环无凭据：`strict_mode_blocked`。
    pub(crate) fn require_r3_authority(
        &self,
        ctx: &CallerContext,
        action: R3Action<'_>,
    ) -> Result<R3Authority, HttpFailure> {
        if let CallerContext::Approval { approval_id } = ctx {
            let service = self.approval_service()?;
            let stored = service
                .get(approval_id)
                .map_err(|error| error_response(&error))?;
            let connection = stored
                .connection
                .ok_or_else(|| error_response(&ApprovalError::Mismatch))?;
            let digest = approval_digest(
                action.kind,
                action.target_record,
                &action.facts,
                &connection,
            );
            let record = service
                .consume(approval_id, &digest)
                .map_err(|error| error_response(&error))?;
            return Ok(R3Authority::Approval(record));
        }
        if !self.strict_mode() {
            return Ok(R3Authority::Unchecked);
        }
        if matches!(ctx, CallerContext::Desktop) {
            return Ok(R3Authority::Desktop);
        }
        let Some(source) = ApprovalSource::from_caller(ctx) else {
            // 本机回环无凭据：拒绝并尽力记审计（拒绝本身不依赖审计）
            if let Some(service) = self.approvals.as_deref() {
                let entry = AuditEntry {
                    at: (service.clock)(),
                    client: laser_contracts::audit_client(ctx),
                    action: AuditAction::StrictModeBlocked,
                    target: action.target_record.to_owned(),
                    result: AuditResult::Denied,
                    detail: AuditDetail {
                        kind: Some(action.kind),
                        error_code: Some("strict_mode_blocked".into()),
                        ..AuditDetail::default()
                    },
                };
                if let Err(error) = service.audit(&entry) {
                    eprintln!("[laser-api] {}", error.message());
                }
            }
            return Err(strict_blocked());
        };
        let service = self.approval_service()?;
        let connection = resolve_approval_target_from_env(action.request)
            .map_err(|error| error_response(&error))?;
        let session_id = match ctx {
            CallerContext::Gateway { session_id } => Some(session_id.clone()),
            // 内置语音：记录发起的设备，审批卡据此显示来源设备
            CallerContext::Voice { device_id, .. } => Some(device_id.chars().take(128).collect()),
            _ => None,
        };
        let request_id = action
            .request
            .get("request_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let record = service
            .create(NewApproval {
                owner: request_owner(ctx),
                voice_permit: self.voice_permit(ctx)?,
                kind: action.kind,
                target_record: action.target_record.to_owned(),
                source,
                session_id,
                request_id,
                summary: action.summary,
                facts: action.facts,
                connection,
                request: stored_request(action.request),
                client: laser_contracts::audit_client(ctx),
                removed_fields: if model_confirmed(action.request) {
                    vec!["confirmed".into()]
                } else {
                    Vec::new()
                },
            })
            .map_err(|error| error_response(&error))?;
        Err(approval_required_response(&record))
    }

    /// 记一条审计（R4 拒绝等由接入点发生的事件）。没有本地数据库时返回错误，调用方自行决定是否忽略。
    pub fn record_audit(&self, entry: &AuditEntry) -> Result<(), String> {
        match self.approvals.as_deref() {
            Some(service) => service.audit(entry).map_err(|error| error.message()),
            None => Err("未启用本地数据库，无法写审计".into()),
        }
    }

    fn approval_service(&self) -> Result<&ApprovalService, HttpFailure> {
        self.approvals.as_deref().ok_or_else(|| {
            error_response(&ApprovalError::Unavailable(
                "未启用本地数据库，无法使用桌面审批".into(),
            ))
        })
    }

    /// 经审批执行后记录结果：成功写入作业 ID，失败记 failed（不可重用）。审批记录写入失败不影响已发生的执行结果。
    pub(crate) fn finish_r3_authority(
        &self,
        authority: &R3Authority,
        result: &Result<Value, HttpFailure>,
        job_id: Option<&str>,
    ) {
        let R3Authority::Approval(record) = authority else {
            return;
        };
        let Some(service) = self.approvals.as_deref() else {
            return;
        };
        let outcome = match result {
            Ok(_) => ApprovalOutcome {
                success: true,
                message: "已执行".into(),
                job_id: job_id.map(str::to_owned),
                error_code: None,
                phase: Some(ApprovalPhase::Completed),
            },
            Err((status, body)) => ApprovalOutcome {
                success: false,
                message: body
                    .get("result")
                    .and_then(Value::as_str)
                    .map(|text| text.chars().take(200).collect())
                    .unwrap_or_else(|| format!("执行失败 (HTTP {status})")),
                job_id: None,
                error_code: body
                    .get("detail")
                    .and_then(|detail| detail.get("error_code"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                phase: Some(ApprovalPhase::Failed),
            },
        };
        if let Err(error) = service.record_outcome(&record.approval_id, outcome) {
            eprintln!(
                "[laser-api] 审批 {} 记录执行结果失败: {}",
                record.approval_id,
                error.message()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 桌面会话与桌面审批接口（技术书 §5.3；实施清单 P1-07）
// ---------------------------------------------------------------------------

/// 桌面会话请求头。只在回环连接上有效；值由 Tauri 命令 `get_desktop_session` 交给桌面页，只存在内存。
pub const DESKTOP_SESSION_HEADER: &str = "x-moss-desktop-session";

/// 长度无关的逐字节比较，避免按前缀逐位猜测会话值。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn desktop_session_invalid() -> Vec<u8> {
    crate::http_json(
        403,
        crate::failure(
            "桌面会话无效，请重新打开 MOSS 桌面端",
            Some(serde_json::json!({"error_code": "desktop_session_invalid"})),
        ),
    )
}

/// 审批卡上显示的目标设备：只给连接方式与串口名，网络设备不显示地址（技术书 §9.5.2）。
fn target_view(connection: Option<&ApprovalConnection>) -> Value {
    match connection {
        Some(ApprovalConnection::Home { .. }) => {
            serde_json::json!({"mode":"home","label":"Home Assistant · 需独立桌面审批"})
        }
        Some(ApprovalConnection::Agent { provider, .. }) => {
            serde_json::json!({"mode":"agent","label":provider})
        }
        Some(ApprovalConnection::External { server, .. }) => {
            serde_json::json!({"mode":"external","label":server})
        }
        Some(ApprovalConnection::Serial { port, .. }) => {
            serde_json::json!({"mode": "serial", "label": port})
        }
        Some(ApprovalConnection::Network { .. }) => {
            serde_json::json!({"mode": "network", "label": "网络设备"})
        }
        None => serde_json::json!({"mode": "unknown", "label": "未知设备"}),
    }
}

pub(crate) fn approval_view(record: &ApprovalRecord) -> Value {
    let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
    if let Some(map) = value.as_object_mut() {
        map.insert("target".into(), target_view(record.connection.as_ref()));
        if record.kind == ApprovalKind::AgentRun
            && let Some(request) = &record.request
        {
            let workspace = request
                .get("workspace")
                .and_then(Value::as_str)
                .unwrap_or("");
            let folder = std::path::Path::new(workspace)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("未提供");
            map.insert(
                "agent".into(),
                serde_json::json!({
                    "provider":request.get("provider").and_then(Value::as_str).unwrap_or(""),
                    "task":request.get("task").and_then(Value::as_str).unwrap_or(""),
                    "workspace_label":folder,
                    "version":request.get("version").and_then(Value::as_str).unwrap_or(""),
                    "permission":"workspace_write",
                    "uses_installed_cli_account":true,
                    "may_incur_cost":true,
                }),
            );
        }
        if record.kind == ApprovalKind::ExternalTool
            && let Some(request) = &record.request
        {
            map.insert("external".into(), serde_json::json!({"tool":record.target_record,"arguments":crate::external_tools::parameter_preview(request.get("arguments").unwrap_or(&Value::Null))}));
        }
    }
    value
}

/// 安全动作的摘要要素（路由创建审批与批准时重算共用）。
pub(crate) fn safe_action_digest_facts(object: &Map<String, Value>) -> Map<String, Value> {
    let mut facts = Map::new();
    let action = object
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    facts.insert("action".into(), Value::String(action));
    for key in ["x", "y", "z", "feed_rate", "power"] {
        if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
            facts.insert(key.into(), value.clone());
        }
    }
    facts
}

/// 走边框的摘要要素（路由创建审批与批准时重算共用）。
pub(crate) fn framing_digest_facts(object: &Map<String, Value>) -> Map<String, Value> {
    let mut facts = Map::new();
    // relative_to_head（2026-11-06）改变走边框的参照点，必须进入摘要；未带该字段的请求摘要不变
    for key in [
        "min_x",
        "min_y",
        "max_x",
        "max_y",
        "feed_rate",
        "power",
        "relative_to_head",
    ] {
        if let Some(value) = object.get(key).filter(|value| !value.is_null()) {
            facts.insert(key.into(), value.clone());
        }
    }
    facts
}

impl crate::ApiService {
    /// 开启桌面会话并返回会话值（Tauri 桌面壳启动时调用一次；重复调用返回同一个值）。
    /// 独立 `moss-host serve` 不调用，因此没有桌面会话，严格模式下无法批准，只能拒绝或过期。
    pub fn enable_desktop_session(&self) -> Result<String, String> {
        if let Some(existing) = self.desktop_session.get() {
            return Ok(existing.clone());
        }
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random).map_err(|error| error.to_string())?;
        let _ = self.desktop_session.set(hex::encode(random));
        self.desktop_session
            .get()
            .cloned()
            .ok_or_else(|| "桌面会话初始化失败".to_string())
    }

    /// 校验请求头中的桌面会话：没有该头返回 `Ok(false)`；有且有效（仅回环）返回 `Ok(true)`；
    /// 有但无效返回 403 `desktop_session_invalid`，前端据此重新获取会话并只重试一次。
    pub(crate) fn check_desktop_session(
        &self,
        head: &str,
        loopback: bool,
    ) -> Result<bool, Vec<u8>> {
        let Some(value) = crate::header_value(head, DESKTOP_SESSION_HEADER) else {
            return Ok(false);
        };
        let Some(expected) = self.desktop_session.get() else {
            return Err(desktop_session_invalid());
        };
        if loopback && constant_time_eq(value.trim().as_bytes(), expected.as_bytes()) {
            Ok(true)
        } else {
            Err(desktop_session_invalid())
        }
    }

    /// 仅供内置语音副屏展示的只读待电脑审批无敏摘要（不含 approval_id、digest、路径或凭据）。
    pub fn pending_approval_display_summary(&self) -> Option<serde_json::Value> {
        let service = self.approval_service().ok()?;
        let now = service.now();
        let records = service.list(Some(ApprovalStatus::Pending), 5).ok()?;
        let rec = records.into_iter().find(|r| !r.is_expired_at(now))?;
        let rem = (rec.expires_at - now).ceil().max(1.0) as u64;
        let total = (rec.expires_at - rec.created_at).round().clamp(30.0, 600.0) as u64;
        Some(serde_json::json!({
            "kind": rec.kind.as_str(),
            "summary": rec.summary,
            "remaining_secs": rem,
            "total_secs": total,
        }))
    }

    /// `/v1/approvals`（列表）、`/v1/approvals/{id}/approve`、`/v1/approvals/{id}/reject`，仅桌面会话。
    pub(crate) fn handle_approval_route(
        &self,
        method: &str,
        route: &str,
        query: &str,
        body: &[u8],
        ctx: &CallerContext,
    ) -> Vec<u8> {
        if !matches!(ctx, CallerContext::Desktop) {
            return crate::http_json(
                403,
                crate::failure(
                    "审批只能在 MOSS 桌面端操作",
                    Some(serde_json::json!({"error_code": "desktop_session_required"})),
                ),
            );
        }
        let service = match self.approval_service() {
            Ok(service) => service,
            Err((status, body)) => return crate::http_json(status, body),
        };
        if method == "GET" && route == "/v1/approvals" {
            let status = match crate::query_param(query, "status") {
                None => None,
                Some(raw) if raw.trim().is_empty() || raw == "all" => None,
                Some(raw) => match ApprovalStatus::parse(raw.trim()) {
                    Some(status) => Some(status),
                    None => {
                        return crate::http_json(
                            400,
                            crate::failure(format!("未知的审批状态: {raw}"), None),
                        );
                    }
                },
            };
            const LIST_LIMIT: usize = 50;
            return match service.list(status, LIST_LIMIT) {
                Ok(records) => crate::http_json(
                    200,
                    crate::success(serde_json::json!({
                        "approvals": records.iter().map(approval_view).collect::<Vec<_>>(),
                        // 最多返回最近 50 条；界面据此说“最近 N 条”，不把它当总数（2026-11-07 n68）
                        "limit": LIST_LIMIT,
                        "server_time": (service.clock)(),
                        "ttl_seconds": service.ttl_seconds(),
                    })),
                ),
                Err(error) => {
                    let (status, body) = error_response(&error);
                    crate::http_json(status, body)
                }
            };
        }
        let Some(rest) = route.strip_prefix("/v1/approvals/") else {
            return crate::http_json(404, crate::failure("未找到路径", None));
        };
        let (approval_id, action) = match rest.split_once('/') {
            Some((id, action)) if !id.is_empty() => (id, action),
            _ => return crate::http_json(404, crate::failure("未找到路径", None)),
        };
        if method != "POST" {
            return crate::http_json(404, crate::failure("未找到路径", None));
        }
        let object = if body.is_empty() {
            Map::new()
        } else {
            match crate::parse_json_object(body, 16 * 1024) {
                Ok(object) => object,
                Err(error) => return crate::http_json(400, crate::failure(error, None)),
            }
        };
        match action {
            "reject" => {
                let reason = object.get("reason").and_then(Value::as_str);
                match service.reject(approval_id, reason) {
                    Ok(record) => crate::http_json(200, crate::success(approval_view(&record))),
                    Err(error) => {
                        let (status, body) = error_response(&error);
                        crate::http_json(status, body)
                    }
                }
            }
            "approve" => {
                let Some(submitted) = object
                    .get("digest")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                else {
                    return crate::http_json(
                        400,
                        crate::failure("批准需要提交审批卡上的摘要 digest", None),
                    );
                };
                self.approve_and_execute(service, approval_id, submitted)
            }
            _ => crate::http_json(404, crate::failure("未找到路径", None)),
        }
    }

    /// 批准并立即以审批身份执行（技术书 §4.3 第 6 步）。批准前按当前记录重算摘要；
    /// 执行走现有路由与全部门禁，由执行入口单次消费授权并写回结果。
    fn approve_and_execute(
        &self,
        service: &ApprovalService,
        approval_id: &str,
        submitted: &str,
    ) -> Vec<u8> {
        let stored = match service.get(approval_id) {
            Ok(record) => record,
            Err(error) => {
                let (status, body) = error_response(&error);
                return crate::http_json(status, body);
            }
        };
        // 记录已删除、刀路不可用等都会让摘要算不出来，按“不一致”处理
        let current = self.current_approval_digest(&stored).unwrap_or_default();
        let approved = match service.approve(approval_id, submitted, &current) {
            Ok(record) => record,
            Err(error) => {
                let (status, mut body) = error_response(&error);
                if let Ok(latest) = service.get(approval_id) {
                    body["detail"]["approval"] = approval_view(&latest);
                }
                return crate::http_json(status, body);
            }
        };
        let (status, response) = self.execute_approval(&approved);
        let latest = service
            .get(approval_id)
            .map(|record| approval_view(&record))
            .unwrap_or(Value::Null);
        let succeeded =
            status < 300 && response.get("success").and_then(Value::as_bool) == Some(true);
        if succeeded {
            crate::http_json(
                200,
                crate::success(serde_json::json!({
                    "approval": latest,
                    "result": response.get("result").cloned().unwrap_or(Value::Null),
                })),
            )
        } else {
            let message = response
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("执行失败 (HTTP {status})"));
            let error_code = response
                .get("detail")
                .and_then(|detail| detail.get("error_code"))
                .cloned()
                .unwrap_or(Value::Null);
            let status = if status < 400 { 500 } else { status };
            crate::http_json(
                status,
                crate::failure(
                    message,
                    Some(serde_json::json!({"error_code": error_code, "approval": latest})),
                ),
            )
        }
    }

    /// 按当前记录、审批保存的执行参数与连接重算摘要（批准时，技术书 §5.4）。
    pub(crate) fn current_approval_digest(
        &self,
        record: &ApprovalRecord,
    ) -> Result<String, ApprovalError> {
        // No general HTTP replay path can authorize HomeControl. Only a
        // dedicated trusted HA module may re-evaluate its policy revision.
        if record.kind == ApprovalKind::HomeControl {
            return Err(ApprovalError::Mismatch);
        }
        if record.kind == ApprovalKind::AgentRun {
            return self
                .agent_runs
                .get()
                .ok_or(ApprovalError::Mismatch)?
                .current_digest(record);
        }
        if record.kind == ApprovalKind::ExternalTool {
            return self
                .external_tools
                .get()
                .ok_or(ApprovalError::Mismatch)?
                .current_digest(record);
        }
        if matches!(record.connection, Some(ApprovalConnection::External { .. })) {
            return Err(ApprovalError::Mismatch);
        }
        let connection = record.connection.as_ref().ok_or(ApprovalError::Mismatch)?;
        let empty = Map::new();
        let request = record.request.as_ref().unwrap_or(&empty);
        let facts = match record.kind {
            ApprovalKind::WorkflowConfirmSend | ApprovalKind::CalibrationSend => {
                let kind = if record.kind == ApprovalKind::CalibrationSend {
                    laser_runtime::RecordKind::Calibration
                } else {
                    laser_runtime::RecordKind::Workflow
                };
                let payload = self
                    .load_record_payload(kind, &record.target_record)
                    .ok_or(ApprovalError::Mismatch)?;
                let roots = self.send_gcode_roots();
                let (_, gcode_sha, _) = crate::build_send_preflight(&payload, &roots)
                    .map_err(|_| ApprovalError::Mismatch)?;
                self.send_digest_facts(kind, &gcode_sha, &record.target_record, request)
            }
            ApprovalKind::SafeAction => safe_action_digest_facts(request),
            ApprovalKind::Framing => framing_digest_facts(request),
            ApprovalKind::ExternalTool | ApprovalKind::AgentRun | ApprovalKind::HomeControl => {
                return Err(ApprovalError::Mismatch);
            }
        };
        Ok(approval_digest(
            record.kind,
            &record.target_record,
            &facts,
            connection,
        ))
    }

    /// 以 `CallerContext::Approval` 重放原始请求：路由由类别决定，连接参数用审批保存的值，
    /// `confirmed=true` 由桌面批准提供。返回（HTTP 状态，响应体）。
    fn execute_approval(&self, record: &ApprovalRecord) -> (u16, Value) {
        if record.kind == ApprovalKind::AgentRun {
            return self
                .agent_runs
                .get()
                .map(|runner| runner.execute(record))
                .unwrap_or_else(|| error_response(&ApprovalError::Mismatch));
        }
        if record.kind == ApprovalKind::ExternalTool {
            return self
                .external_tools
                .get()
                .map(|tools| tools.execute(record))
                .unwrap_or_else(|| error_response(&ApprovalError::Mismatch));
        }
        if matches!(record.connection, Some(ApprovalConnection::External { .. })) {
            return error_response(&ApprovalError::Mismatch);
        }
        let Some(connection) = record.connection.as_ref() else {
            return error_response(&ApprovalError::Mismatch);
        };
        let mut body =
            object_with_connection(&record.request.clone().unwrap_or_default(), connection);
        body.insert("confirmed".into(), Value::Bool(true));
        let route = match record.kind {
            ApprovalKind::WorkflowConfirmSend => {
                let action = body.get("action").and_then(Value::as_str).unwrap_or("");
                if !matches!(action, "confirm_send" | "send" | "resume") {
                    body.insert("action".into(), Value::String("confirm_send".into()));
                }
                body.insert(
                    "workflow_id".into(),
                    Value::String(record.target_record.clone()),
                );
                "/api/workflow"
            }
            ApprovalKind::CalibrationSend => {
                body.insert(
                    "calibration_id".into(),
                    Value::String(record.target_record.clone()),
                );
                "/api/material-lab/send"
            }
            ApprovalKind::SafeAction => "/api/safe-action",
            ApprovalKind::Framing => "/api/device/framing",
            ApprovalKind::ExternalTool | ApprovalKind::AgentRun | ApprovalKind::HomeControl => {
                return error_response(&ApprovalError::Mismatch);
            }
        };
        let payload = Value::Object(body).to_string();
        let request = format!(
            "POST {route} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let raw = self.handle_http_with_ctx(
            request.as_bytes(),
            CallerContext::Approval {
                approval_id: record.approval_id.clone(),
            },
        );
        let Some(separator) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
            return (500, crate::failure("执行响应格式错误", None));
        };
        let status = String::from_utf8_lossy(&raw[..separator])
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(500);
        let body = serde_json::from_slice(&raw[separator + 4..])
            .unwrap_or_else(|_| crate::failure("执行响应无法解析", None));
        (status, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fixture {
        service: ApprovalService,
        clock: Arc<Mutex<f64>>,
        db: Arc<Mutex<RuntimeDb>>,
    }

    impl Fixture {
        fn new() -> Self {
            let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
            let clock = Arc::new(Mutex::new(1000.0_f64));
            let reader = clock.clone();
            let service = ApprovalService::new(db.clone())
                .with_ttl_seconds(120)
                .with_clock(Arc::new(move || *reader.lock().unwrap()));
            Self { service, clock, db }
        }

        fn audit_actions(&self) -> Vec<AuditAction> {
            let mut entries = self.db.lock().unwrap().recent_audit(100).unwrap();
            entries.reverse();
            entries.into_iter().map(|entry| entry.action).collect()
        }

        fn break_audit_log(&self) {
            self.db
                .lock()
                .unwrap()
                .connection
                .execute_batch("DROP TABLE audit_log;")
                .unwrap();
        }

        fn advance(&self, seconds: f64) {
            *self.clock.lock().unwrap() += seconds;
        }
    }

    fn network() -> ApprovalConnection {
        ApprovalConnection::Network {
            host: "192.0.2.10".into(),
            port: 23,
        }
    }

    fn workflow_record(gcode_sha: &str) -> Value {
        json!({
            "workflow_id": "wf_test",
            "input": {"material": "椴木板", "thickness_mm": 3.0, "power": 300, "feed_rate": 1500, "passes": 1},
            "artifacts": {"gcode_file": "job.gcode", "gcode_sha256": gcode_sha},
        })
    }

    fn new_approval(request_id: Option<&str>) -> NewApproval {
        NewApproval {
            owner: Some("authenticated-test-peer".into()),
            voice_permit: None,
            kind: ApprovalKind::WorkflowConfirmSend,
            target_record: "wf_test".into(),
            source: ApprovalSource::McpBridge,
            session_id: None,
            request_id: request_id.map(str::to_owned),
            summary: "雕刻文字；椴木板 3 mm；功率 30%，速度 1500 mm/min，1 遍".into(),
            facts: record_digest_facts(&workflow_record(&"a".repeat(64)), &Map::new()),
            connection: network(),
            request: Map::new(),
            client: "mcp".into(),
            removed_fields: Vec::new(),
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn request(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    /// A-35：串口留空且没有 .env 默认值时返回 target_required，绝不自动选串口。
    #[test]
    fn blank_serial_port_without_default_requires_target() {
        let error = resolve_approval_target(
            &request(json!({"connection_mode": "serial", "port": "  "})),
            &no_env,
        )
        .unwrap_err();
        assert_eq!(error.code(), "target_required");
        assert_eq!(error.http_status(), 400);
        let error =
            resolve_approval_target(&request(json!({"connection_mode": "network"})), &no_env)
                .unwrap_err();
        assert_eq!(error.code(), "target_required");
        // 什么都没给、也没有默认模式：按网络模式要求地址
        let error = resolve_approval_target(&Map::new(), &no_env).unwrap_err();
        assert_eq!(error.code(), "target_required");
        // 占位符形式的默认值不算配置
        let placeholder =
            |name: &str| (name == "LASER_NETWORK_HOST").then(|| "<设备IP>".to_string());
        assert!(resolve_approval_target(&Map::new(), &placeholder).is_err());
    }

    #[test]
    fn explicit_or_env_targets_are_resolved() {
        assert_eq!(
            resolve_approval_target(&request(json!({"network_host": "192.0.2.10"})), &no_env)
                .unwrap(),
            network()
        );
        assert_eq!(
            resolve_approval_target(
                &request(json!({"port": "COMX", "baudrate": 230400})),
                &no_env
            )
            .unwrap(),
            ApprovalConnection::Serial {
                port: "COMX".into(),
                baud_rate: 230400
            }
        );
        let env = |name: &str| match name {
            "LASER_DEFAULT_CONNECTION_MODE" => Some("serial".to_string()),
            "GRBL_DEFAULT_PORT" => Some("COMX".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_approval_target(&Map::new(), &env).unwrap(),
            ApprovalConnection::Serial {
                port: "COMX".into(),
                baud_rate: 115200
            }
        );
        let error =
            resolve_approval_target(&request(json!({"port": "COMX", "baudrate": 10})), &no_env)
                .unwrap_err();
        assert_eq!(error.code(), "approval_invalid_request");
    }

    #[test]
    fn canonical_json_sorts_keys_without_whitespace() {
        let value = json!({"b": 1, "a": {"d": [1, 2.5, "x"], "c": "引号\""}, "e": null});
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"c":"引号\"","d":[1,2.5,"x"]},"b":1,"e":null}"#
        );
    }

    /// 摘要覆盖 G-code、发送方式与解析后的目标；不受键顺序影响；facts 不能冒充保留键。
    #[test]
    fn digest_changes_with_gcode_send_mode_and_target() {
        let facts = record_digest_facts(&workflow_record(&"a".repeat(64)), &Map::new());
        let base = approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_test",
            &facts,
            &network(),
        );
        assert_eq!(base.len(), 64);
        assert_eq!(
            base,
            approval_digest(
                ApprovalKind::WorkflowConfirmSend,
                "wf_test",
                &facts.clone(),
                &network()
            )
        );

        let regenerated = record_digest_facts(&workflow_record(&"b".repeat(64)), &Map::new());
        assert_ne!(
            base,
            approval_digest(
                ApprovalKind::WorkflowConfirmSend,
                "wf_test",
                &regenerated,
                &network()
            )
        );

        let offline = record_digest_facts(
            &workflow_record(&"a".repeat(64)),
            &request(json!({"offline_sd": true})),
        );
        assert_eq!(offline["send_mode"], "sd_offline");
        assert_ne!(
            base,
            approval_digest(
                ApprovalKind::WorkflowConfirmSend,
                "wf_test",
                &offline,
                &network()
            )
        );

        let other_device = ApprovalConnection::Network {
            host: "192.0.2.11".into(),
            port: 23,
        };
        assert_ne!(
            base,
            approval_digest(
                ApprovalKind::WorkflowConfirmSend,
                "wf_test",
                &facts,
                &other_device
            )
        );
        assert_ne!(
            base,
            approval_digest(
                ApprovalKind::WorkflowConfirmSend,
                "wf_other",
                &facts,
                &network()
            )
        );

        let mut forged = facts.clone();
        forged.insert("workflow_id".into(), json!("wf_other"));
        forged.insert("kind".into(), json!("framing"));
        assert_eq!(
            base,
            approval_digest(
                ApprovalKind::WorkflowConfirmSend,
                "wf_test",
                &forged,
                &network()
            )
        );

        assert_eq!(facts["gcode_sha256"], json!("a".repeat(64)));
        assert_eq!(facts["material"], "椴木板");
        assert_eq!(facts["params"]["power"], 300);
        assert_eq!(facts["send_mode"], "stream");
    }

    #[test]
    fn nv13_agent_run_approval_consumed_once_with_durable_record_and_revoke_gate() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let fixture = Fixture::new();
        {
            let db = fixture.db.lock().unwrap();
            db.voice_device_seen("device-nv13", "client-nv13", "", "", "", 995.0)
                .unwrap();
            db.set_voice_device_status("device-nv13", laser_runtime::VOICE_DEVICE_ALLOWED, 996.0)
                .unwrap();
        }
        let permit = Arc::new(AtomicBool::new(true));
        let connection = ApprovalConnection::Agent {
            provider: "codex".into(),
            fingerprint: "c".repeat(64),
        };
        let request = request(
            json!({"task":"读取说明文件并总结","provider":"codex","workspace":"fake-workspace","device_id":"device-nv13","voice_session_id":"session-nv13","config_revision":"b".repeat(64)}),
        );
        let created = fixture
            .service
            .create(NewApproval {
                owner: Some("nv13-test-device".into()),
                voice_permit: Some(permit.clone()),
                kind: ApprovalKind::AgentRun,
                target_record: "agent_codex".into(),
                source: ApprovalSource::Voice,
                session_id: Some("device-nv13".into()),
                request_id: None,
                summary: "Codex 只读任务，需要桌面批准".into(),
                facts: request.clone(),
                connection,
                request,
                client: "voice:device-nv13".into(),
                removed_fields: Vec::new(),
            })
            .unwrap();
        assert_eq!(created.status, ApprovalStatus::Pending);
        let base = laser_runtime::AgentRunRecord {
            id: "run-nv13".into(),
            approval_id: created.approval_id.clone(),
            provider: "codex".into(),
            workspace: "fake-workspace".into(),
            task_digest: "a".repeat(64),
            config_revision: "b".repeat(64),
            status: "running".into(),
            output: String::new(),
            truncated: false,
            started_at: 1000.0,
            finished_at: None,
            error_code: None,
        };
        assert_eq!(
            fixture
                .service
                .consume_agent_run(&created.approval_id, &created.digest, &base, &permit)
                .unwrap_err(),
            ApprovalError::NotApproved
        );
        fixture
            .service
            .approve(&created.approval_id, &created.digest, &created.digest)
            .unwrap();
        assert_eq!(
            fixture
                .service
                .consume_agent_run(&created.approval_id, "wrong", &base, &permit)
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        let started = fixture
            .service
            .consume_agent_run(&created.approval_id, &created.digest, &base, &permit)
            .unwrap();
        assert_eq!(started.status, ApprovalStatus::Executed);
        assert_eq!(started.outcome.unwrap().phase, Some(ApprovalPhase::Running));
        assert_eq!(
            fixture
                .service
                .consume_agent_run(&created.approval_id, &created.digest, &base, &permit)
                .unwrap_err(),
            ApprovalError::Consumed
        );
        assert_eq!(fixture.db.lock().unwrap().agent_runs(50).unwrap().len(), 1);
        assert_eq!(
            approval_view(&created)["agent"]["permission"],
            "workspace_write"
        );
        assert!(
            fixture
                .service
                .finish_agent_run("run-nv13", "completed", "任务完成", false, None)
                .unwrap()
        );
        assert_eq!(
            fixture
                .service
                .get(&created.approval_id)
                .unwrap()
                .outcome
                .unwrap()
                .message,
            "Agent 任务执行完成"
        );
        assert_eq!(
            fixture
                .db
                .lock()
                .unwrap()
                .find_agent_run("run-nv13")
                .unwrap()
                .unwrap()
                .status,
            "completed"
        );
        assert!(
            fixture
                .service
                .finish_agent_run("run-nv13", "completed", "retry", false, None)
                .is_err()
        );

        let revoked = fixture
            .service
            .create(NewApproval {
                owner: Some("nv13-test-device".into()),
                voice_permit: Some(permit.clone()),
                kind: ApprovalKind::AgentRun,
                target_record: "agent_codex".into(),
                source: ApprovalSource::Voice,
                session_id: Some("device-nv13".into()),
                request_id: None,
                summary: "第二次只读任务".into(),
                facts: Map::new(),
                connection: ApprovalConnection::Agent {
                    provider: "codex".into(),
                    fingerprint: "d".repeat(64),
                },
                request: Map::new(),
                client: "voice:device-nv13".into(),
                removed_fields: Vec::new(),
            })
            .unwrap();
        fixture
            .service
            .approve(&revoked.approval_id, &revoked.digest, &revoked.digest)
            .unwrap();
        permit.store(false, Ordering::Release);
        let mut second = base.clone();
        second.id = "run-revoked".into();
        second.approval_id = revoked.approval_id.clone();
        assert_eq!(
            fixture
                .service
                .consume_agent_run(&revoked.approval_id, &revoked.digest, &second, &permit)
                .unwrap_err(),
            ApprovalError::Expired
        );
        assert!(
            fixture
                .db
                .lock()
                .unwrap()
                .find_agent_run("run-revoked")
                .unwrap()
                .is_none()
        );
    }

    fn fake_home_request(
        permit: Arc<std::sync::atomic::AtomicBool>,
        fingerprint: &str,
    ) -> NewApproval {
        let mut facts = Map::new();
        facts.insert("action".into(), json!("turn_on"));
        facts.insert("entity_id".into(), json!("light.desk"));
        facts.insert("policy_revision".into(), json!(7));
        facts.insert("credential_revision".into(), json!(9));
        facts.insert("service_path".into(), json!("/api/services/light/turn_on"));
        NewApproval {
            owner: Some("nv15-mock-desktop".into()),
            voice_permit: Some(permit),
            kind: ApprovalKind::HomeControl,
            target_record: "light.desk".into(),
            source: ApprovalSource::Voice,
            session_id: Some("nv15-device".into()),
            request_id: None,
            summary: "Home Assistant：打开书桌台灯，需要独立桌面授权".into(),
            facts: facts.clone(),
            connection: ApprovalConnection::Home {
                fingerprint: fingerprint.into(),
            },
            request: facts,
            client: "voice:nv15-device".into(),
            removed_fields: Vec::new(),
        }
    }

    fn fake_home_fixture() -> (Fixture, Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::atomic::AtomicBool;
        let fixture = Fixture::new();
        {
            let db = fixture.db.lock().unwrap();
            db.voice_device_seen("nv15-device", "nv15-test-client", "", "", "", 995.0)
                .unwrap();
            db.set_voice_device_status("nv15-device", laser_runtime::VOICE_DEVICE_ALLOWED, 996.0)
                .unwrap();
        }
        (fixture, Arc::new(AtomicBool::new(true)))
    }

    #[test]
    fn nv15_independent_home_approvals_and_terminal_result_audits_are_atomic() {
        let (fixture, permit) = fake_home_fixture();
        let fp = "a".repeat(64);
        let first = fixture
            .service
            .create(fake_home_request(permit.clone(), &fp))
            .unwrap();
        let second_fp = "b".repeat(64);
        let second = fixture
            .service
            .create(fake_home_request(permit.clone(), &second_fp))
            .unwrap();
        assert_ne!(first.digest, second.digest);
        assert_ne!(first.approval_id, second.approval_id);
        fixture
            .service
            .approve(&first.approval_id, &first.digest, &first.digest)
            .unwrap();
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &second.approval_id,
                    &second.digest,
                    &second_fp,
                    "nv15-device",
                    &permit
                )
                .unwrap_err(),
            ApprovalError::NotApproved
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &first.approval_id,
                    &second.digest,
                    &fp,
                    "nv15-device",
                    &permit
                )
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &first.approval_id,
                    &first.digest,
                    &fp,
                    "nv15-other-device",
                    &permit
                )
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        fixture
            .service
            .consume_home_control(
                &first.approval_id,
                &first.digest,
                &fp,
                "nv15-device",
                &permit,
            )
            .unwrap();
        let before = fixture.db.lock().unwrap().recent_audit(80).unwrap().len();
        let confirmed = fixture
            .service
            .record_outcome(
                &first.approval_id,
                ApprovalOutcome {
                    success: true,
                    message: "已确认合成台灯状态".into(),
                    phase: Some(ApprovalPhase::Completed),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(confirmed.status, ApprovalStatus::Executed);
        assert_eq!(
            confirmed.outcome.unwrap().phase,
            Some(ApprovalPhase::Completed)
        );
        let audits = fixture.db.lock().unwrap().recent_audit(80).unwrap();
        assert_eq!(audits.len(), before + 1);
        assert_eq!(audits[0].action, AuditAction::ApprovalExecuted);
        assert_eq!(
            audits[0].detail.approval_id.as_deref(),
            Some(first.approval_id.as_str())
        );
        assert_eq!(audits[0].detail.reason.as_deref(), Some("home_confirmed"));
        assert_eq!(audits[0].result, AuditResult::Ok);
        assert_eq!(
            fixture.service.get(&second.approval_id).unwrap().status,
            ApprovalStatus::Pending
        );
        assert_eq!(
            fixture
                .service
                .record_outcome(
                    &first.approval_id,
                    ApprovalOutcome {
                        success: false,
                        phase: Some(ApprovalPhase::Unknown),
                        message: "late unknown".into(),
                        ..Default::default()
                    }
                )
                .unwrap_err(),
            ApprovalError::Consumed
        );

        fixture
            .service
            .approve(&second.approval_id, &second.digest, &second.digest)
            .unwrap();
        fixture
            .service
            .consume_home_control(
                &second.approval_id,
                &second.digest,
                &second_fp,
                "nv15-device",
                &permit,
            )
            .unwrap();
        let unknown = fixture
            .service
            .record_outcome(
                &second.approval_id,
                ApprovalOutcome {
                    success: false,
                    message: "模拟网络确认失败，不重发".into(),
                    error_code: Some("result_unknown".into()),
                    phase: Some(ApprovalPhase::Unknown),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(unknown.status, ApprovalStatus::Failed);
        assert_eq!(unknown.outcome.unwrap().phase, Some(ApprovalPhase::Unknown));
        let audits = fixture.db.lock().unwrap().recent_audit(80).unwrap();
        assert_eq!(audits[0].action, AuditAction::ApprovalFailed);
        assert_eq!(
            audits[0].detail.reason.as_deref(),
            Some("home_result_unknown")
        );
        assert_eq!(
            audits[0].detail.approval_id.as_deref(),
            Some(second.approval_id.as_str())
        );
        assert_eq!(
            fixture
                .service
                .record_outcome(
                    &second.approval_id,
                    ApprovalOutcome {
                        success: true,
                        phase: Some(ApprovalPhase::Completed),
                        message: "伪造的迟到成功".into(),
                        ..Default::default()
                    }
                )
                .unwrap_err(),
            ApprovalError::Consumed
        );
    }

    #[test]
    fn nv15_audit_failure_rolls_back_terminal_outcome_and_never_allows_replay() {
        let (fixture, permit) = fake_home_fixture();
        let fp = "f".repeat(64);
        let approval = fixture
            .service
            .create(fake_home_request(permit.clone(), &fp))
            .unwrap();
        fixture
            .service
            .approve(&approval.approval_id, &approval.digest, &approval.digest)
            .unwrap();
        fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fp,
                "nv15-device",
                &permit,
            )
            .unwrap();
        {
            let db = fixture.db.lock().unwrap();
            db.connection
                .execute_batch(
                    "CREATE TRIGGER deny_home_terminal_audit BEFORE INSERT ON audit_log
                 WHEN NEW.action='approval_failed'
                 BEGIN SELECT RAISE(ABORT,'synthetic audit failure'); END;",
                )
                .unwrap();
        }
        assert!(
            fixture
                .service
                .record_outcome(
                    &approval.approval_id,
                    ApprovalOutcome {
                        success: false,
                        message: "模拟设备状态未知".into(),
                        error_code: Some("result_unknown".into()),
                        phase: Some(ApprovalPhase::Unknown),
                        ..Default::default()
                    }
                )
                .is_err()
        );
        let unchanged = fixture.service.get(&approval.approval_id).unwrap();
        assert_eq!(unchanged.status, ApprovalStatus::Executed);
        assert_eq!(
            unchanged.outcome.unwrap().phase,
            Some(ApprovalPhase::Running)
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &approval.approval_id,
                    &approval.digest,
                    &fp,
                    "nv15-device",
                    &permit
                )
                .unwrap_err(),
            ApprovalError::Consumed
        );
        {
            let db = fixture.db.lock().unwrap();
            db.connection
                .execute_batch("DROP TRIGGER deny_home_terminal_audit")
                .unwrap();
        }
        fixture.service.recover_after_restart().unwrap();
        let recovered = fixture.service.get(&approval.approval_id).unwrap();
        assert_eq!(recovered.status, ApprovalStatus::Failed);
        assert_eq!(
            recovered.outcome.unwrap().phase,
            Some(ApprovalPhase::Unknown)
        );
    }
    #[test]
    fn nv15_home_control_private_cancel_cannot_cross_approval_types_or_replay() {
        let (fixture, permit) = fake_home_fixture();
        let fp = "a".repeat(64);
        let pending = fixture
            .service
            .create(fake_home_request(permit.clone(), &fp))
            .unwrap();
        assert_eq!(
            fixture
                .service
                .cancel_home_control(&pending.approval_id, "wrong")
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            fixture.service.get(&pending.approval_id).unwrap().status,
            ApprovalStatus::Pending
        );
        let cancelled = fixture
            .service
            .cancel_home_control(&pending.approval_id, &pending.digest)
            .unwrap();
        assert_eq!(cancelled.status, ApprovalStatus::Rejected);
        assert_eq!(
            cancelled.outcome.unwrap().phase,
            Some(ApprovalPhase::Cancelled)
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &pending.approval_id,
                    &pending.digest,
                    &fp,
                    "nv15-device",
                    &permit
                )
                .unwrap_err(),
            ApprovalError::Rejected
        );
        assert_eq!(
            fixture
                .service
                .cancel_home_control(&pending.approval_id, &pending.digest)
                .unwrap_err(),
            ApprovalError::Rejected
        );
        let next = fixture
            .service
            .create(fake_home_request(permit.clone(), &fp))
            .unwrap();
        let approved = fixture
            .service
            .approve(&next.approval_id, &next.digest, &next.digest)
            .unwrap();
        assert_eq!(approved.status, ApprovalStatus::Approved);
        let cancelled = fixture
            .service
            .cancel_home_control(&next.approval_id, &next.digest)
            .unwrap();
        assert_eq!(cancelled.status, ApprovalStatus::Expired);
        assert_eq!(
            fixture
                .service
                .consume_home_control(&next.approval_id, &next.digest, &fp, "nv15-device", &permit)
                .unwrap_err(),
            ApprovalError::Expired
        );
        let third = fixture
            .service
            .create(fake_home_request(permit.clone(), &fp))
            .unwrap();
        fixture
            .service
            .approve(&third.approval_id, &third.digest, &third.digest)
            .unwrap();
        fixture
            .service
            .consume_home_control(
                &third.approval_id,
                &third.digest,
                &fp,
                "nv15-device",
                &permit,
            )
            .unwrap();
        assert_eq!(
            fixture
                .service
                .cancel_home_control(&third.approval_id, &third.digest)
                .unwrap_err(),
            ApprovalError::Consumed
        );
    }

    #[test]
    fn nv15_home_control_persistent_single_use_never_executes_via_general_approval() {
        let (fixture, permit) = fake_home_fixture();
        let fingerprint = "a".repeat(64);
        let approval = fixture
            .service
            .create(fake_home_request(permit.clone(), &fingerprint))
            .unwrap();
        assert_eq!(approval.kind, ApprovalKind::HomeControl);
        assert_eq!(approval.status, ApprovalStatus::Pending);
        let output = serde_json::to_string(&approval_view(&approval)).unwrap();
        assert!(!output.contains(&fingerprint));
        assert!(!output.contains("nv15-fake-token"));
        assert_eq!(
            fixture
                .service
                .consume(&approval.approval_id, &approval.digest)
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &approval.approval_id,
                    &approval.digest,
                    &fingerprint,
                    "nv15-device",
                    &permit,
                )
                .unwrap_err(),
            ApprovalError::NotApproved
        );
        fixture
            .service
            .approve(&approval.approval_id, &approval.digest, &approval.digest)
            .unwrap();
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &approval.approval_id,
                    &approval.digest,
                    &"b".repeat(64),
                    "nv15-device",
                    &permit,
                )
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &approval.approval_id,
                    &approval.digest,
                    &fingerprint,
                    "nv15-other-device",
                    &permit,
                )
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &approval.approval_id,
                    "invalid-digest",
                    &fingerprint,
                    "nv15-device",
                    &permit,
                )
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        let consumed = fixture
            .service
            .consume_home_control(
                &approval.approval_id,
                &approval.digest,
                &fingerprint,
                "nv15-device",
                &permit,
            )
            .unwrap();
        assert_eq!(consumed.status, ApprovalStatus::Executed);
        assert_eq!(
            consumed.outcome.as_ref().unwrap().phase,
            Some(ApprovalPhase::Running)
        );
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &approval.approval_id,
                    &approval.digest,
                    &fingerprint,
                    "nv15-device",
                    &permit,
                )
                .unwrap_err(),
            ApprovalError::Consumed
        );
        assert_eq!(
            fixture
                .service
                .consume(&approval.approval_id, &approval.digest)
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        let recovered = fixture.service.recover_after_restart().unwrap();
        assert!(recovered >= 1);
        let after = fixture.service.get(&approval.approval_id).unwrap();
        assert_eq!(after.status, ApprovalStatus::Failed);
        assert_eq!(
            after.outcome.as_ref().unwrap().phase,
            Some(ApprovalPhase::Unknown)
        );
        assert!(after.outcome.unwrap().message.contains("Home Assistant"));
        // The fake plan never has a network executor, and no POST exists.
    }

    #[test]
    fn nv15_home_control_fails_closed_on_revocation_expiry_wrong_source_and_identity() {
        use std::sync::atomic::Ordering;
        let (fixture, permit) = fake_home_fixture();
        let fp = "c".repeat(64);
        for modify in 0..5 {
            let mut request = fake_home_request(permit.clone(), &fp);
            match modify {
                0 => request.source = ApprovalSource::McpBridge,
                1 => {
                    request.connection = ApprovalConnection::Network {
                        host: "169.254.169.254".into(),
                        port: 80,
                    }
                }
                2 => request.target_record = "../../../api/services/lock/unlock".into(),
                3 => request.session_id = None,
                4 => {
                    request.connection = ApprovalConnection::Home {
                        fingerprint: "not-a-fingerprint".into(),
                    }
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                fixture.service.create(request),
                Err(ApprovalError::InvalidRequest(_))
            ));
        }
        let first = fixture
            .service
            .create(fake_home_request(permit.clone(), &fp))
            .unwrap();
        fixture
            .service
            .approve(&first.approval_id, &first.digest, &first.digest)
            .unwrap();
        permit.store(false, Ordering::Release);
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &first.approval_id,
                    &first.digest,
                    &fp,
                    "nv15-device",
                    &permit,
                )
                .unwrap_err(),
            ApprovalError::Expired
        );
        permit.store(true, Ordering::Release);
        let second = fixture
            .service
            .create(fake_home_request(permit.clone(), &fp))
            .unwrap();
        fixture
            .service
            .approve(&second.approval_id, &second.digest, &second.digest)
            .unwrap();
        fixture.advance(121.0);
        assert_eq!(
            fixture
                .service
                .consume_home_control(
                    &second.approval_id,
                    &second.digest,
                    &fp,
                    "nv15-device",
                    &permit,
                )
                .unwrap_err(),
            ApprovalError::Expired
        );
    }

    #[test]
    fn create_stores_connection_and_is_idempotent_per_request_id() {
        let fixture = Fixture::new();
        let created = fixture.service.create(new_approval(Some("req-1"))).unwrap();
        assert!(created.approval_id.starts_with(APPROVAL_ID_PREFIX));
        assert_eq!(created.status, ApprovalStatus::Pending);
        assert_eq!(created.expires_at, 1120.0);
        assert_eq!(created.connection, Some(network()));
        let text = serde_json::to_string(&created).unwrap();
        assert!(!text.contains("192.0.2.10"), "{text}");

        let again = fixture.service.create(new_approval(Some("req-1"))).unwrap();
        assert_eq!(again.approval_id, created.approval_id);
        let other = fixture.service.create(new_approval(Some("req-2"))).unwrap();
        assert_ne!(other.approval_id, created.approval_id);
        let without_key = fixture.service.create(new_approval(None)).unwrap();
        assert_ne!(without_key.approval_id, created.approval_id);

        let stored = fixture.service.get(&created.approval_id).unwrap();
        assert_eq!(stored.connection, Some(network()));
        assert_eq!(
            fixture
                .service
                .list(Some(ApprovalStatus::Pending), 10)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            fixture.service.get("apr_missing").unwrap_err(),
            ApprovalError::NotFound
        );
    }

    #[test]
    fn request_ids_are_isolated_by_trusted_owner_and_changed_content_conflicts() {
        let fixture = Fixture::new();
        let first = fixture.service.create(new_approval(Some("same"))).unwrap();
        let mut other = new_approval(Some("same"));
        other.owner = Some("other-device".into());
        let second = fixture.service.create(other).unwrap();
        assert_ne!(first.approval_id, second.approval_id);
        let mut changed = new_approval(Some("same"));
        changed.target_record = "wf_other".into();
        assert_eq!(
            fixture.service.create(changed).unwrap_err(),
            ApprovalError::RequestConflict
        );
        let mut changed = new_approval(Some("same"));
        changed.request.insert("power".into(), json!(999));
        assert_eq!(
            fixture.service.create(changed).unwrap_err(),
            ApprovalError::RequestConflict
        );
        let mut unknown = new_approval(Some("same"));
        unknown.owner = None;
        let a = fixture.service.create(unknown.clone()).unwrap();
        let b = fixture.service.create(unknown).unwrap();
        assert_ne!(
            a.approval_id, b.approval_id,
            "unidentified bridge peers never share approvals"
        );
        fixture.advance(121.0);
        assert_eq!(
            fixture
                .service
                .create(new_approval(Some("same")))
                .unwrap()
                .status,
            ApprovalStatus::Expired
        );
        assert!(
            fixture
                .service
                .create(new_approval(Some(&"x".repeat(129))))
                .is_err()
        );
    }

    #[test]
    fn revoking_voice_expires_only_its_open_approvals_and_blocks_new_ones() {
        let fixture = Fixture::new();
        for d in ["a", "b"] {
            let db = fixture.db.lock().unwrap();
            db.voice_device_seen(d, "client", "", "", "", 1000.0)
                .unwrap();
            db.set_voice_device_status(d, laser_runtime::VOICE_DEVICE_ALLOWED, 1000.0)
                .unwrap();
        }
        let make = |d: &str, id: &str| {
            let mut n = new_approval(Some(id));
            n.source = ApprovalSource::Voice;
            n.voice_permit = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
            n.session_id = Some(d.into());
            n.owner = Some(d.into());
            n
        };
        let pending = fixture.service.create(make("a", "1")).unwrap();
        let approved = fixture.service.create(make("a", "2")).unwrap();
        fixture
            .service
            .approve(&approved.approval_id, &approved.digest, &approved.digest)
            .unwrap();
        let other = fixture.service.create(make("b", "1")).unwrap();
        fixture
            .db
            .lock()
            .unwrap()
            .set_voice_device_status("a", laser_runtime::VOICE_DEVICE_REVOKED, 1001.0)
            .unwrap();
        for a in [&pending, &approved] {
            assert_eq!(
                fixture.service.get(&a.approval_id).unwrap().status,
                ApprovalStatus::Expired
            );
        }
        assert_eq!(
            fixture.service.get(&other.approval_id).unwrap().status,
            ApprovalStatus::Pending
        );
        assert!(fixture.service.create(make("a", "3")).is_err());
        fixture
            .db
            .lock()
            .unwrap()
            .set_voice_device_status("a", laser_runtime::VOICE_DEVICE_ALLOWED, 1002.0)
            .unwrap();
        let mut stale = make("a", "queued");
        stale.voice_permit = Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        assert_eq!(
            fixture.service.create(stale).unwrap_err(),
            ApprovalError::Expired,
            "re-allow cannot revive work from a revoked session"
        );
        assert_eq!(
            fixture
                .service
                .consume(&approved.approval_id, &approved.digest)
                .unwrap_err(),
            ApprovalError::Expired
        );
    }

    #[test]
    fn approve_then_consume_once() {
        let fixture = Fixture::new();
        let created = fixture.service.create(new_approval(None)).unwrap();
        let digest = created.digest.clone();
        assert_eq!(
            fixture
                .service
                .consume(&created.approval_id, &digest)
                .unwrap_err(),
            ApprovalError::NotApproved
        );

        fixture.advance(5.0);
        let approved = fixture
            .service
            .approve(&created.approval_id, &digest, &digest)
            .unwrap();
        assert_eq!(approved.status, ApprovalStatus::Approved);
        assert_eq!(approved.decided_at, Some(1005.0));
        assert_eq!(
            fixture
                .service
                .approve(&created.approval_id, &digest, &digest)
                .unwrap_err(),
            ApprovalError::Consumed
        );

        let executed = fixture
            .service
            .consume(&created.approval_id, &digest)
            .unwrap();
        assert_eq!(executed.status, ApprovalStatus::Executed);
        assert_eq!(executed.connection, Some(network()));
        // A-23：重复使用
        let error = fixture
            .service
            .consume(&created.approval_id, &digest)
            .unwrap_err();
        assert_eq!(error, ApprovalError::Consumed);
        assert_eq!(error.code(), "approval_consumed");
        assert_eq!(error.http_status(), 409);

        let done = fixture
            .service
            .record_outcome(
                &created.approval_id,
                ApprovalOutcome {
                    success: true,
                    message: "已开始发送".into(),
                    job_id: Some("wf_test".into()),
                    error_code: None,
                    ..ApprovalOutcome::default()
                },
            )
            .unwrap();
        assert_eq!(done.status, ApprovalStatus::Executed);
        assert_eq!(done.outcome.unwrap().job_id.as_deref(), Some("wf_test"));
    }

    /// A-22：批准后重新预览（摘要变化）再执行 → approval_mismatch，授权作废，不可再用。
    #[test]
    fn changed_content_after_approval_is_a_mismatch() {
        let fixture = Fixture::new();
        let created = fixture.service.create(new_approval(None)).unwrap();
        let digest = created.digest.clone();
        let regenerated = approval_digest(
            ApprovalKind::WorkflowConfirmSend,
            "wf_test",
            &record_digest_facts(&workflow_record(&"b".repeat(64)), &Map::new()),
            &network(),
        );
        // 批准时卡片摘要或当前状态不一致：不批准，状态不变
        assert_eq!(
            fixture
                .service
                .approve(&created.approval_id, "0", &digest)
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            fixture
                .service
                .approve(&created.approval_id, &digest, &regenerated)
                .unwrap_err(),
            ApprovalError::Mismatch
        );
        assert_eq!(
            fixture.service.get(&created.approval_id).unwrap().status,
            ApprovalStatus::Pending
        );

        fixture
            .service
            .approve(&created.approval_id, &digest, &digest)
            .unwrap();
        let error = fixture
            .service
            .consume(&created.approval_id, &regenerated)
            .unwrap_err();
        assert_eq!(error.code(), "approval_mismatch");
        let failed = fixture.service.get(&created.approval_id).unwrap();
        assert_eq!(failed.status, ApprovalStatus::Failed);
        assert_eq!(
            failed.outcome.unwrap().error_code.as_deref(),
            Some("approval_mismatch")
        );
        assert_eq!(
            fixture
                .service
                .consume(&created.approval_id, &digest)
                .unwrap_err(),
            ApprovalError::Consumed
        );
    }

    /// A-23：授权过期。
    #[test]
    fn expired_approvals_cannot_be_approved_or_executed() {
        let fixture = Fixture::new();
        let pending = fixture
            .service
            .create(new_approval(Some("late-approve")))
            .unwrap();
        let approved = fixture
            .service
            .create(new_approval(Some("late-execute")))
            .unwrap();
        fixture
            .service
            .approve(&approved.approval_id, &approved.digest, &approved.digest)
            .unwrap();

        fixture.advance(120.0);
        let error = fixture
            .service
            .approve(&pending.approval_id, &pending.digest, &pending.digest)
            .unwrap_err();
        assert_eq!(error, ApprovalError::Expired);
        assert_eq!(error.http_status(), 410);
        assert_eq!(
            fixture
                .service
                .consume(&approved.approval_id, &approved.digest)
                .unwrap_err(),
            ApprovalError::Expired
        );
        assert_eq!(
            fixture.service.get(&pending.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
        assert_eq!(
            fixture.service.get(&approved.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
        assert!(
            fixture
                .service
                .list(Some(ApprovalStatus::Pending), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn list_and_get_expire_due_requests() {
        let fixture = Fixture::new();
        let created = fixture.service.create(new_approval(None)).unwrap();
        fixture.advance(119.0);
        assert_eq!(
            fixture
                .service
                .list(Some(ApprovalStatus::Pending), 10)
                .unwrap()
                .len(),
            1
        );
        fixture.advance(1.0);
        assert_eq!(fixture.service.expire_due().unwrap(), 1);
        assert_eq!(
            fixture.service.get(&created.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
    }

    #[test]
    fn rejected_requests_stay_rejected() {
        let fixture = Fixture::new();
        let created = fixture.service.create(new_approval(None)).unwrap();
        let rejected = fixture
            .service
            .reject(&created.approval_id, Some("材料没放好"))
            .unwrap();
        assert_eq!(rejected.status, ApprovalStatus::Rejected);
        assert_eq!(rejected.decided_at, Some(1000.0));
        assert_eq!(rejected.outcome.unwrap().message, "材料没放好");
        assert_eq!(
            fixture
                .service
                .approve(&created.approval_id, &created.digest, &created.digest)
                .unwrap_err(),
            ApprovalError::Rejected
        );
        assert_eq!(
            fixture
                .service
                .consume(&created.approval_id, &created.digest)
                .unwrap_err(),
            ApprovalError::Rejected
        );
        assert_eq!(
            fixture
                .service
                .reject(&created.approval_id, None)
                .unwrap_err(),
            ApprovalError::Rejected
        );
    }

    /// A-32：批准后、设备阶段前进程崩溃 → 重启后过期；同一工作流可以重新发起并批准。
    #[test]
    fn restart_expires_open_approvals_and_workflow_can_be_reapproved() {
        let fixture = Fixture::new();
        let approved = fixture
            .service
            .create(new_approval(Some("before-crash")))
            .unwrap();
        fixture
            .service
            .approve(&approved.approval_id, &approved.digest, &approved.digest)
            .unwrap();
        let pending = fixture
            .service
            .create(new_approval(Some("pending")))
            .unwrap();
        let executed = fixture
            .service
            .create(new_approval(Some("executed")))
            .unwrap();
        fixture
            .service
            .approve(&executed.approval_id, &executed.digest, &executed.digest)
            .unwrap();
        fixture
            .service
            .consume(&executed.approval_id, &executed.digest)
            .unwrap();

        assert_eq!(fixture.service.recover_after_restart().unwrap(), 2);
        assert_eq!(
            fixture.service.get(&approved.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
        assert_eq!(
            fixture.service.get(&pending.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
        assert_eq!(
            fixture.service.get(&executed.approval_id).unwrap().status,
            ApprovalStatus::Executed
        );
        assert_eq!(
            fixture
                .service
                .consume(&approved.approval_id, &approved.digest)
                .unwrap_err(),
            ApprovalError::Expired
        );

        let again = fixture
            .service
            .create(new_approval(Some("after-restart")))
            .unwrap();
        assert_eq!(again.digest, approved.digest);
        fixture
            .service
            .approve(&again.approval_id, &again.digest, &again.digest)
            .unwrap();
        assert_eq!(
            fixture
                .service
                .consume(&again.approval_id, &again.digest)
                .unwrap()
                .status,
            ApprovalStatus::Executed
        );
    }

    #[test]
    fn failed_execution_is_not_reusable() {
        let fixture = Fixture::new();
        let created = fixture.service.create(new_approval(None)).unwrap();
        fixture
            .service
            .approve(&created.approval_id, &created.digest, &created.digest)
            .unwrap();
        fixture
            .service
            .consume(&created.approval_id, &created.digest)
            .unwrap();
        let failed = fixture
            .service
            .record_outcome(
                &created.approval_id,
                ApprovalOutcome {
                    success: false,
                    message: "加工前检查未通过".into(),
                    job_id: None,
                    error_code: Some("device_preflight_failed".into()),
                    ..ApprovalOutcome::default()
                },
            )
            .unwrap();
        assert_eq!(failed.status, ApprovalStatus::Failed);
        assert_eq!(
            fixture
                .service
                .consume(&created.approval_id, &created.digest)
                .unwrap_err(),
            ApprovalError::Consumed
        );
        let pending = fixture.service.create(new_approval(None)).unwrap();
        assert_eq!(
            fixture
                .service
                .record_outcome(
                    &pending.approval_id,
                    ApprovalOutcome {
                        success: true,
                        message: String::new(),
                        job_id: None,
                        error_code: None,
                        ..ApprovalOutcome::default()
                    },
                )
                .unwrap_err(),
            ApprovalError::NotApproved
        );
    }

    /// P1-08：审批每个状态变化都留审计；模型自填的 confirmed 单独记一条。
    #[test]
    fn approval_lifecycle_is_audited() {
        let fixture = Fixture::new();
        let mut request = new_approval(Some("audit-1"));
        request.removed_fields = vec!["confirmed".into()];
        let created = fixture.service.create(request).unwrap();
        fixture
            .service
            .approve(&created.approval_id, &created.digest, &created.digest)
            .unwrap();
        fixture
            .service
            .consume(&created.approval_id, &created.digest)
            .unwrap();
        fixture
            .service
            .record_outcome(
                &created.approval_id,
                ApprovalOutcome {
                    success: false,
                    message: "设备未就绪".into(),
                    job_id: None,
                    error_code: Some("device_not_ready".into()),
                    ..ApprovalOutcome::default()
                },
            )
            .unwrap();
        let rejected = fixture
            .service
            .create(new_approval(Some("audit-2")))
            .unwrap();
        fixture
            .service
            .reject(&rejected.approval_id, Some("不做了"))
            .unwrap();
        let expiring = fixture
            .service
            .create(new_approval(Some("audit-3")))
            .unwrap();
        fixture.advance(120.0);
        fixture.service.expire_due().unwrap();
        assert_eq!(
            fixture.audit_actions(),
            vec![
                AuditAction::ApprovalCreated,
                AuditAction::ConfirmedFieldStripped,
                AuditAction::ApprovalApproved,
                AuditAction::ApprovalExecuted,
                AuditAction::ApprovalFailed,
                AuditAction::ApprovalCreated,
                AuditAction::ApprovalRejected,
                AuditAction::ApprovalCreated,
                AuditAction::ApprovalExpired,
            ]
        );
        let entries = fixture.db.lock().unwrap().recent_audit(100).unwrap();
        let expired = &entries[0];
        assert_eq!(expired.client, "system");
        assert_eq!(
            expired.detail.approval_id.as_deref(),
            Some(expiring.approval_id.as_str())
        );
        let stripped = entries
            .iter()
            .find(|entry| entry.action == AuditAction::ConfirmedFieldStripped)
            .unwrap();
        assert_eq!(stripped.client, "mcp");
        assert_eq!(
            stripped.detail.removed_fields,
            vec!["confirmed".to_string()]
        );
        let approved = entries
            .iter()
            .find(|entry| entry.action == AuditAction::ApprovalApproved)
            .unwrap();
        assert_eq!(approved.client, "desktop");
        assert_eq!(approved.target, "wf_test");
        let text = serde_json::to_string(&entries).unwrap();
        assert!(!text.contains("192.0.2.10"), "审计不得包含设备地址: {text}");
    }

    /// A-34：审计写不进去时，创建、批准、执行一律拒绝且状态不变；拒绝与过期照常生效。
    #[test]
    fn audit_failure_blocks_r3_but_not_risk_reduction() {
        let fixture = Fixture::new();
        let approved = fixture
            .service
            .create(new_approval(Some("before-1")))
            .unwrap();
        fixture
            .service
            .approve(&approved.approval_id, &approved.digest, &approved.digest)
            .unwrap();
        let pending = fixture
            .service
            .create(new_approval(Some("before-2")))
            .unwrap();
        let to_reject = fixture
            .service
            .create(new_approval(Some("before-3")))
            .unwrap();
        fixture.break_audit_log();

        let error = fixture
            .service
            .create(new_approval(Some("after-1")))
            .unwrap_err();
        assert_eq!(error.code(), "approval_unavailable");
        assert!(error.message().contains("审计"), "{}", error.message());
        assert!(
            fixture
                .service
                .list(None, 10)
                .unwrap()
                .iter()
                .all(|record| record.request_id.as_deref() != Some("after-1"))
        );

        let error = fixture
            .service
            .approve(&pending.approval_id, &pending.digest, &pending.digest)
            .unwrap_err();
        assert_eq!(error.code(), "approval_unavailable");
        assert_eq!(
            fixture.service.get(&pending.approval_id).unwrap().status,
            ApprovalStatus::Pending
        );

        let error = fixture
            .service
            .consume(&approved.approval_id, &approved.digest)
            .unwrap_err();
        assert_eq!(error.code(), "approval_unavailable");
        assert_eq!(
            fixture.service.get(&approved.approval_id).unwrap().status,
            ApprovalStatus::Approved,
            "未执行就不能算已用"
        );

        assert_eq!(
            fixture
                .service
                .reject(&to_reject.approval_id, None)
                .unwrap()
                .status,
            ApprovalStatus::Rejected
        );
        fixture.advance(120.0);
        assert_eq!(fixture.service.expire_due().unwrap(), 2);
        assert_eq!(
            fixture.service.get(&approved.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
    }

    /// A-31：服务端宕机期间无人处理的待批准请求到期即过期，之后不能再批准。
    #[test]
    fn pending_request_expires_while_nobody_answers() {
        let fixture = Fixture::new();
        let created = fixture
            .service
            .create(new_approval(Some("server-down")))
            .unwrap();
        fixture.advance(121.0);
        assert!(
            fixture
                .service
                .list(Some(ApprovalStatus::Pending), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture.service.get(&created.approval_id).unwrap().status,
            ApprovalStatus::Expired
        );
        assert_eq!(
            fixture
                .service
                .approve(&created.approval_id, &created.digest, &created.digest)
                .unwrap_err(),
            ApprovalError::Expired
        );
        assert_eq!(
            fixture.audit_actions().last(),
            Some(&AuditAction::ApprovalExpired)
        );
    }

    /// A-33：发送中进程中断 → 重启时审批记为失败、结果未知；授权不能再用，之后的作业回写也不覆盖该结论。
    #[test]
    fn interrupted_send_is_marked_unknown_on_restart() {
        let fixture = Fixture::new();
        let created = fixture.service.create(new_approval(Some("crash"))).unwrap();
        fixture
            .service
            .approve(&created.approval_id, &created.digest, &created.digest)
            .unwrap();
        fixture
            .service
            .consume(&created.approval_id, &created.digest)
            .unwrap();
        fixture
            .service
            .mark_running(&created.approval_id, "wf_test")
            .unwrap();
        let running = fixture.service.get(&created.approval_id).unwrap();
        assert_eq!(running.outcome.unwrap().phase, Some(ApprovalPhase::Running));

        assert_eq!(fixture.service.recover_after_restart().unwrap(), 1);
        let failed = fixture.service.get(&created.approval_id).unwrap();
        assert_eq!(failed.status, ApprovalStatus::Failed);
        let outcome = failed.outcome.unwrap();
        assert_eq!(outcome.error_code.as_deref(), Some("result_unknown"));
        assert_eq!(outcome.phase, Some(ApprovalPhase::Unknown));
        assert_eq!(outcome.job_id.as_deref(), Some("wf_test"));
        assert!(!outcome.success);
        assert_eq!(
            fixture
                .service
                .consume(&created.approval_id, &created.digest)
                .unwrap_err(),
            ApprovalError::Consumed
        );
        assert_eq!(
            fixture.audit_actions().last(),
            Some(&AuditAction::ApprovalFailed)
        );

        // 迟到的作业回写不覆盖“结果未知”
        fixture.service.record_job_result(
            &created.approval_id,
            "completed",
            &serde_json::json!({"lines_sent": 3, "total_lines": 3}),
        );
        assert_eq!(
            fixture
                .service
                .get(&created.approval_id)
                .unwrap()
                .outcome
                .unwrap()
                .phase,
            Some(ApprovalPhase::Unknown)
        );
        // 再次重启不重复处理
        assert_eq!(fixture.service.recover_after_restart().unwrap(), 0);
    }

    #[test]
    fn job_results_are_written_back_once() {
        let fixture = Fixture::new();
        let mut ids = Vec::new();
        for key in ["done", "cancel"] {
            let created = fixture.service.create(new_approval(Some(key))).unwrap();
            fixture
                .service
                .approve(&created.approval_id, &created.digest, &created.digest)
                .unwrap();
            fixture
                .service
                .consume(&created.approval_id, &created.digest)
                .unwrap();
            fixture
                .service
                .mark_running(&created.approval_id, "wf_test")
                .unwrap();
            ids.push(created.approval_id);
        }
        fixture.service.record_job_result(
            &ids[0],
            "completed",
            &serde_json::json!({"lines_sent": 120, "total_lines": 120}),
        );
        let done = fixture.service.get(&ids[0]).unwrap();
        assert_eq!(done.status, ApprovalStatus::Executed);
        let outcome = done.outcome.unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.phase, Some(ApprovalPhase::Completed));
        assert!(outcome.message.contains("120/120"), "{}", outcome.message);
        assert_eq!(outcome.job_id.as_deref(), Some("wf_test"));
        // 已结束的不再改动
        fixture
            .service
            .record_job_result(&ids[0], "failed", &serde_json::json!({"error": "late"}));
        assert_eq!(
            fixture.service.get(&ids[0]).unwrap().outcome.unwrap().phase,
            Some(ApprovalPhase::Completed)
        );

        fixture.service.record_job_result(
            &ids[1],
            "cancelled",
            &serde_json::json!({"lines_sent": 10, "total_lines": 120, "error": "用户急停"}),
        );
        let cancelled = fixture.service.get(&ids[1]).unwrap();
        assert_eq!(cancelled.status, ApprovalStatus::Failed);
        let outcome = cancelled.outcome.unwrap();
        assert_eq!(outcome.phase, Some(ApprovalPhase::Cancelled));
        assert_eq!(outcome.error_code.as_deref(), Some("cancelled"));
        assert_eq!(outcome.message, "用户急停");
        // 已结束的作业不会在重启时被误判为“结果未知”
        assert_eq!(fixture.service.recover_after_restart().unwrap(), 0);
    }

    #[test]
    fn ttl_and_input_validation() {
        let fixture = Fixture::new();
        assert_eq!(fixture.service.ttl_seconds(), 120);
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        assert_eq!(
            ApprovalService::new(db.clone())
                .with_ttl_seconds(5)
                .ttl_seconds(),
            30
        );
        assert_eq!(
            ApprovalService::new(db)
                .with_ttl_seconds(5000)
                .ttl_seconds(),
            600
        );
        let mut missing = new_approval(None);
        missing.target_record = " ".into();
        assert_eq!(
            fixture.service.create(missing).unwrap_err().code(),
            "approval_invalid_request"
        );
    }
}
