//! 家居控制与本机 Agent 运行的专用审批：原子消费、取消与终态记录。

use super::*;

impl ApprovalService {
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
}
