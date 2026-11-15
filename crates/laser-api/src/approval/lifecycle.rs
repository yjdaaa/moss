//! 审批服务本体：构造、审计、过期与重启恢复、创建、批准、拒绝、单次消费与执行结果记录。

use super::*;

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

    pub(super) fn now(&self) -> f64 {
        (self.clock)()
    }

    pub(super) fn lock(&self) -> Result<std::sync::MutexGuard<'_, RuntimeDb>, ApprovalError> {
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
    pub(super) fn expire_one(
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
