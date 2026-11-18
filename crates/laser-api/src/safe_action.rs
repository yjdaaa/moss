use super::*;

impl ApiService {
    /// 处理安全动作请求：优先复用当前正在运行的任务信道（边刻边调/急停），
    /// 避免正在雕刻时重新打开串口/网络引发的设备冲突。
    pub fn handle_safe_action(
        &self,
        args: &laser_contracts::LaserSafeActionArgs,
        _ctx: &laser_contracts::CallerContext,
    ) -> laser_contracts::ToolCallResult {
        self.handle_safe_action_for(args, _ctx, None)
    }

    pub(crate) fn handle_safe_action_for(
        &self,
        args: &laser_contracts::LaserSafeActionArgs,
        _ctx: &laser_contracts::CallerContext,
        job_id: Option<&str>,
    ) -> laser_contracts::ToolCallResult {
        if !self.voice_caller_allowed(_ctx) {
            return laser_contracts::ToolCallResult::with_detail(
                false,
                "语音设备授权已失效，请重新连接",
                "voice_session_revoked",
            );
        }
        // 与 laser-tools 一致：动作名大小写不敏感，避免 "Emergency_Stop" 落空而不发 0x18（2026-11-05）。
        let action_owned = args.action.trim().to_ascii_lowercase();
        let action = action_owned.as_str();
        if matches!(action, "pause" | "resume" | "stop_framing") {
            return self.control_framing(action, args, job_id);
        }
        if job_id.is_some() && !matches!(action, "emergency_stop" | "soft_reset" | "status") {
            return laser_contracts::ToolCallResult::err(
                "job_id 只用于查询或停止任务，不能改变其他动作的目标",
            );
        }
        let realtime_cmd_info = match action {
            "emergency_stop" | "soft_reset" => Some((
                laser_device::RealtimeCommand::EmergencyStop,
                "急停 0x18",
                true,
            )),
            "feed_override_reset" | "feed_reset" | "feed_100" => Some((
                laser_device::RealtimeCommand::FeedOvrReset,
                "进给速度恢复100% (0x90)",
                false,
            )),
            "feed_override_plus_10" | "feed_plus_10" | "feed_+10%" => Some((
                laser_device::RealtimeCommand::FeedOvrCoarsePlus,
                "进给速度增加10% (0x91)",
                false,
            )),
            "feed_override_minus_10" | "feed_minus_10" | "feed_-10%" => Some((
                laser_device::RealtimeCommand::FeedOvrCoarseMinus,
                "进给速度降低10% (0x92)",
                false,
            )),
            "feed_override_plus_1" | "feed_plus_1" | "feed_+1%" => Some((
                laser_device::RealtimeCommand::FeedOvrFinePlus,
                "进给速度增加1% (0x93)",
                false,
            )),
            "feed_override_minus_1" | "feed_minus_1" | "feed_-1%" => Some((
                laser_device::RealtimeCommand::FeedOvrFineMinus,
                "进给速度降低1% (0x94)",
                false,
            )),
            "spindle_override_reset" | "power_override_reset" | "power_reset" | "power_100" => {
                Some((
                    laser_device::RealtimeCommand::SpindleOvrReset,
                    "激光功率恢复100% (0x99)",
                    false,
                ))
            }
            "spindle_override_plus_10"
            | "power_override_plus_10"
            | "power_plus_10"
            | "power_+10%" => Some((
                laser_device::RealtimeCommand::SpindleOvrCoarsePlus,
                "激光功率增加10% (0x9A)",
                false,
            )),
            "spindle_override_minus_10"
            | "power_override_minus_10"
            | "power_minus_10"
            | "power_-10%" => Some((
                laser_device::RealtimeCommand::SpindleOvrCoarseMinus,
                "激光功率降低10% (0x9B)",
                false,
            )),
            "spindle_override_plus_1" | "power_override_plus_1" | "power_plus_1" | "power_+1%" => {
                Some((
                    laser_device::RealtimeCommand::SpindleOvrFinePlus,
                    "激光功率增加1% (0x9C)",
                    false,
                ))
            }
            "spindle_override_minus_1"
            | "power_override_minus_1"
            | "power_minus_1"
            | "power_-1%" => Some((
                laser_device::RealtimeCommand::SpindleOvrFineMinus,
                "激光功率降低1% (0x9D)",
                false,
            )),
            _ => None,
        };

        let alters_alarm = realtime_cmd_info.is_some_and(|(_, _, alters)| alters);
        let has_host = args.host.as_deref().is_some_and(|s| !s.trim().is_empty());
        let has_port = args.port.as_deref().is_some_and(|s| !s.trim().is_empty());
        let mode_hint = args
            .connection_mode
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase);
        let active_mode_field_present = match mode_hint.as_deref() {
            Some("network") => has_host,
            Some("serial") => has_port,
            _ => has_host || has_port,
        };
        let explicit_target = if active_mode_field_present {
            match laser_tools::resolve_connection_spec(
                args.port.as_deref(),
                args.host.as_deref(),
                args.baudrate,
                args.telnet_port,
                args.connection_mode.as_deref(),
            ) {
                Ok(spec) => Some(spec.describe()),
                Err(_) if job_id.is_some() => None,
                Err(error) => return laser_contracts::ToolCallResult::err(error),
            }
        } else {
            None
        };
        enum StopTarget {
            Running(std::sync::Arc<ActiveJob>),
            Uncertain(std::sync::Arc<UncertainJob>),
        }
        let matched_target = {
            let Ok(jobs) = self.active_jobs.lock() else {
                return laser_contracts::ToolCallResult::err("无法读取设备任务，请使用实体急停");
            };
            let Ok(uncertain) = self.uncertain_jobs.lock() else {
                return laser_contracts::ToolCallResult::err(
                    "无法读取待确认停止的设备任务，请使用实体急停",
                );
            };
            if let Some(id) = job_id {
                if let Some(job) = jobs.values().find(|j| j.workflow_id == id).cloned() {
                    if explicit_target.as_ref().is_some_and(|t| t != &job.target) {
                        return laser_contracts::ToolCallResult::err(
                            "任务编号与指定设备不一致，停止未发送，请检查目标或使用实体急停",
                        );
                    }
                    Some(StopTarget::Running(job))
                } else if let Some(u) = uncertain.values().find(|j| j.workflow_id == id).cloned() {
                    if explicit_target.as_ref().is_some_and(|t| t != &u.target) {
                        return laser_contracts::ToolCallResult::err(
                            "任务编号与指定设备不一致，停止未发送，请检查目标或使用实体急停",
                        );
                    }
                    Some(StopTarget::Uncertain(u))
                } else {
                    return laser_contracts::ToolCallResult::err(
                        "找不到此任务的设备连接；没有向其他设备发送停止，请使用实体急停",
                    );
                }
            } else if let Some(target) = explicit_target.as_ref() {
                if let Some(job) = jobs.values().find(|j| &j.target == target).cloned() {
                    Some(StopTarget::Running(job))
                } else if alters_alarm {
                    uncertain
                        .values()
                        .find(|j| &j.target == target)
                        .cloned()
                        .map(StopTarget::Uncertain)
                } else {
                    None
                }
            } else if alters_alarm {
                let distinct_targets: std::collections::BTreeSet<&str> = jobs
                    .values()
                    .map(|j| j.target.as_str())
                    .chain(uncertain.values().map(|u| u.target.as_str()))
                    .collect();
                if distinct_targets.len() > 1 {
                    return laser_contracts::ToolCallResult::err(
                        "存在多个设备任务，请指定停止目标；紧急情况请使用实体急停",
                    );
                }
                if let Some(job) = jobs.values().next().cloned() {
                    Some(StopTarget::Running(job))
                } else {
                    uncertain
                        .values()
                        .next()
                        .cloned()
                        .map(StopTarget::Uncertain)
                }
            } else if jobs.len() > 1 {
                return laser_contracts::ToolCallResult::err(
                    "存在多个设备任务，请指定停止目标；紧急情况请使用实体急停",
                );
            } else {
                jobs.values().next().cloned().map(StopTarget::Running)
            }
        };
        match matched_target {
            Some(StopTarget::Running(job)) => {
                if let Some((cmd, label, alters_alarm)) = realtime_cmd_info {
                    if args.dry_run == Some(true) {
                        return laser_contracts::ToolCallResult::ok(format!(
                            "仅预览：{label}；未向设备发送"
                        ));
                    }
                    if alters_alarm {
                        job.cancel.cancel();
                        if let Err(error) = job
                            .transport
                            .write_realtime(laser_device::RealtimeCommand::EmergencyStop)
                        {
                            return laser_contracts::ToolCallResult::err(format!(
                                "作业取消已请求，但急停 0x18 发送失败：{error}"
                            ));
                        }
                        return laser_contracts::ToolCallResult::ok(format!(
                            "{label} 已发送到任务的原设备通道（目标 {}），不代表实际停机，请确认硬件状态",
                            job.target
                        ));
                    }
                    match job.transport.write_realtime(cmd) {
                        Ok(()) => {
                            return laser_contracts::ToolCallResult::ok(format!(
                                "{label} 已实时发送到正在运行的加工任务（目标 {}），即时生效（不停机）",
                                job.target
                            ));
                        }
                        Err(e) => {
                            return laser_contracts::ToolCallResult::err(format!(
                                "{label} 发送失败（目标 {}）: {e}",
                                job.target
                            ));
                        }
                    }
                } else if action == "status" {
                    let _ = job
                        .transport
                        .write_realtime(laser_device::RealtimeCommand::StatusQuery);
                    let sent = job
                        .progress
                        .lines_sent
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let total = job
                        .progress
                        .total_lines
                        .load(std::sync::atomic::Ordering::Relaxed);
                    return laser_contracts::ToolCallResult::with_detail(
                        true,
                        format!(
                            "设备正在加工中（目标 {}），已确认 {}/{} 行",
                            job.target, sent, total
                        ),
                        "active_job_streaming",
                    );
                } else {
                    return laser_contracts::ToolCallResult::err(format!(
                        "设备正在执行加工任务（目标 {}），禁止下发 {action} 等顺序指令（请先等待加工完成或执行急停）",
                        job.target
                    ));
                }
            }
            Some(StopTarget::Uncertain(job)) => {
                if let Some((_cmd, label, true)) = realtime_cmd_info {
                    if args.dry_run == Some(true) {
                        return laser_contracts::ToolCallResult::ok(format!(
                            "仅预览：{label}；未向设备发送"
                        ));
                    }
                    job.cancel.cancel();
                    if let Err(error) =
                        job.write_emergency_stop(&self.transport_factory, &self.target_open_locks)
                    {
                        return laser_contracts::ToolCallResult::err(format!(
                            "作业取消已请求，但急停 0x18 发送失败：{error}"
                        ));
                    }
                    return laser_contracts::ToolCallResult::ok(format!(
                        "{label} 已发送到任务的原设备通道（目标 {}，此前任务结果未知），不代表实际停机，请确认硬件状态",
                        job.target
                    ));
                } else if action == "status" {
                    return laser_contracts::ToolCallResult::with_detail(
                        false,
                        "任务执行结果和停机状态未知，请使用急停重试并到现场确认",
                        "job_result_unknown",
                    );
                } else {
                    return laser_contracts::ToolCallResult::err(format!(
                        "设备此前任务停机状态未确认（目标 {}），请先到现场确认或执行急停",
                        job.target
                    ));
                }
            }
            None => {}
        }

        let permit = match self.voice_permit(_ctx) {
            Ok(permit) => permit,
            Err(_) => {
                return laser_contracts::ToolCallResult::err("语音设备授权已失效，请重新连接");
            }
        };
        let factory = self.transport_factory.clone();
        let uncertain_for_opener = self.uncertain_jobs.clone();
        let open_locks_for_opener = self.target_open_locks.clone();
        let opener: laser_tools::TransportOpener = std::sync::Arc::new(
            move |spec: &laser_device::ConnectionSpec, _timeout: std::time::Duration| {
                let target_desc = spec.describe();
                let mutex = target_open_mutex(&open_locks_for_opener, &target_desc);
                let _open_guard = mutex
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                release_uncertain_target_handle(&uncertain_for_opener, &target_desc);
                (factory)(spec)
            },
        );
        laser_tools::execute_laser_safe_action_with_gate(args, &opener, &|| {
            permit
                .as_ref()
                .is_none_or(|p| p.load(std::sync::atomic::Ordering::Acquire))
        })
    }

    /// 走边框途中的暂停 / 继续 / 停止（2026-11-18 用户决定，技术书 r52）。只作用于正在走的边框
    /// （活动作业 `framing-…`；不带 `job_id` 时取唯一的活动作业），正式加工不支持：
    /// - `pause`：进给保持 `!`，激光头原地停住（走边框本来就不出光），不需要确认；
    /// - `resume`：恢复 `~`，机器会再动，必须 `confirmed=true`（严格模式下路由层已按 R3 授权）；
    /// - `stop_framing`：取消这次走边框并立即进给保持，作业线程等停稳后再发 0x18（比直接急停不易丢步），
    ///   激光头停在半路、不回起点，与急停一样转为待确认停止。
    pub(crate) fn control_framing(
        &self,
        action: &str,
        args: &laser_contracts::LaserSafeActionArgs,
        job_id: Option<&str>,
    ) -> laser_contracts::ToolCallResult {
        use std::sync::atomic::Ordering;
        let confirmed = args.confirmed == Some(true);
        let job = {
            let Ok(jobs) = self.active_jobs.lock() else {
                return laser_contracts::ToolCallResult::err("无法读取设备任务，请使用实体急停");
            };
            match job_id {
                Some(id) => jobs.values().find(|job| job.workflow_id == id).cloned(),
                None if jobs.len() > 1 => {
                    return laser_contracts::ToolCallResult::err(
                        "存在多个设备任务，请指定 job_id；紧急情况请使用急停",
                    );
                }
                None => jobs.values().next().cloned(),
            }
        };
        let Some(job) = job else {
            return laser_contracts::ToolCallResult::with_detail(
                false,
                "现在没有正在走的边框",
                "no_active_framing",
            );
        };
        if !job.workflow_id.starts_with("framing-") {
            return laser_contracts::ToolCallResult::with_detail(
                false,
                "暂停、继续、停止走边框只用于走边框；正式加工要停下请用急停，或取消（记断点）",
                "framing_control_not_supported",
            );
        }
        // 显式连接目标（含审批保存的目标）必须就是这次走边框的设备，不能把 `~` 发给别的机器
        if let Some(target) = explicit_safe_action_target(args)
            && target != job.target
        {
            return laser_contracts::ToolCallResult::with_detail(
                false,
                "指定的设备不是正在走边框的那台，没有发送",
                "framing_target_mismatch",
            );
        }
        // 已在停止收尾的走边框不能再暂停或继续（减速停稳期间写 `~` 会让电机重新加速）
        if job.cancel.is_cancelled() && action != "stop_framing" {
            return laser_contracts::ToolCallResult::with_detail(
                false,
                "走边框正在停止，不能再暂停或继续",
                "no_active_framing",
            );
        }
        if args.dry_run == Some(true) {
            return laser_contracts::ToolCallResult::ok(format!(
                "仅预览：{action}（{}）；未向设备发送",
                job.workflow_id
            ));
        }
        match action {
            "pause" => match job
                .transport
                .write_realtime(laser_device::RealtimeCommand::FeedHold)
            {
                Ok(()) => {
                    job.progress.paused.store(true, Ordering::Release);
                    laser_contracts::ToolCallResult::ok(
                        "走边框已暂停，激光头停在原地（不出光）。要继续先问用户“确认继续走边框吗”；不走了用 stop_framing",
                    )
                }
                Err(error) => laser_contracts::ToolCallResult::err(format!(
                    "暂停没有发出去：{error}；要停下请用急停"
                )),
            },
            "resume" => {
                if !confirmed {
                    return laser_contracts::ToolCallResult::with_detail(
                        false,
                        "继续走边框会让机器运动：先问用户“确认继续走边框吗”，同意后再带 confirmed=true",
                        "confirmation_required",
                    );
                }
                match job
                    .transport
                    .write_realtime(laser_device::RealtimeCommand::Resume)
                {
                    Ok(()) => {
                        job.progress.paused.store(false, Ordering::Release);
                        laser_contracts::ToolCallResult::ok("走边框继续，走完回到起点（不出光）")
                    }
                    Err(error) => {
                        laser_contracts::ToolCallResult::err(format!("继续没有发出去：{error}"))
                    }
                }
            }
            _ => {
                // 先取消再进给保持：作业线程据取消令牌收尾（等停稳后 0x18），`!` 让机器立即开始减速
                job.cancel.cancel();
                job.progress.paused.store(false, Ordering::Release);
                if let Err(error) = job
                    .transport
                    .write_realtime(laser_device::RealtimeCommand::FeedHold)
                {
                    return laser_contracts::ToolCallResult::err(format!(
                        "停止走边框的减速指令没发出去（{error}），请立即用急停或机器上的急停按钮"
                    ));
                }
                laser_contracts::ToolCallResult::ok(
                    "已停止走边框：激光头先减速停稳再复位，停在半路、不回起点。开始加工前请把激光头挪回起点，或重新走一遍边框",
                )
            }
        }
    }

    /// AI 在走边框途中对 workflow 调 cancel 时，应当停下的走边框作业（见 `/api/workflow` 路由）：
    /// 唯一的活动作业是还没在停止的走边框，且这份 workflow 存在、在等待发送、不在启动或发送中。
    pub(crate) fn framing_to_stop_for_cancel(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<String> {
        let requested = object
            .get("workflow_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())?;
        let framing_id = {
            let jobs = self.active_jobs.lock().ok()?;
            if jobs.len() != 1 {
                return None;
            }
            let job = jobs.values().next()?;
            if !job.workflow_id.starts_with("framing-") || job.cancel.is_cancelled() {
                return None;
            }
            job.workflow_id.clone()
        };
        let launching = self
            .launching_workflows
            .lock()
            .map(|state| state.active.contains_key(requested))
            .unwrap_or(true);
        if launching {
            return None;
        }
        let stored = self
            .with_effective_store(|store| store.get(laser_runtime::RecordKind::Workflow, requested))
            .ok()
            .flatten()?;
        let status = stored
            .status
            .as_deref()
            .or_else(|| stored.payload.get("status").and_then(|v| v.as_str()));
        crate::workflow::workflow_send_blocked_reason(status)
            .is_none()
            .then_some(framing_id)
    }

    /// `/api/safe-action resume` 在授权之前的绑定：必须 `confirmed=true`；找到正在走（未在停止）的走边框作业，
    /// 把它的 `job_id` 写进请求；请求没带连接时补上这次作业的连接，严格模式审批目标因此就是那台设备。
    pub(crate) fn bind_framing_resume(
        &self,
        object: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), Vec<u8>> {
        let refuse = |message: &str, code: &str| {
            http_json(
                400,
                failure(message, Some(serde_json::json!({"error_code": code}))),
            )
        };
        if object.get("confirmed").and_then(|v| v.as_bool()) != Some(true) {
            return Err(refuse(
                "继续走边框会让机器运动：先问用户“确认继续走边框吗”，同意后再带 confirmed=true",
                "confirmation_required",
            ));
        }
        let requested = object
            .get("job_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let job = {
            let Ok(jobs) = self.active_jobs.lock() else {
                return Err(refuse("无法读取设备任务", "no_active_framing"));
            };
            match requested.as_deref() {
                Some(id) => jobs.values().find(|job| job.workflow_id == id).cloned(),
                None if jobs.len() > 1 => {
                    return Err(refuse(
                        "存在多个设备任务，请指定 job_id",
                        "no_active_framing",
                    ));
                }
                None => jobs.values().next().cloned(),
            }
        };
        let Some(job) =
            job.filter(|job| job.workflow_id.starts_with("framing-") && !job.cancel.is_cancelled())
        else {
            return Err(refuse("现在没有可以继续的走边框", "no_active_framing"));
        };
        object.insert("job_id".into(), serde_json::json!(job.workflow_id));
        let has_target = ["port", "serial_port", "network_host", "host"]
            .iter()
            .any(|key| {
                object
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| !v.trim().is_empty())
            });
        if !has_target {
            match &job.spec {
                Some(laser_device::ConnectionSpec::Tcp { host, port }) => {
                    object.insert("connection_mode".into(), serde_json::json!("network"));
                    object.insert("network_host".into(), serde_json::json!(host));
                    object.insert("network_telnet_port".into(), serde_json::json!(port));
                }
                Some(laser_device::ConnectionSpec::Serial { port, baud_rate }) => {
                    object.insert("connection_mode".into(), serde_json::json!("serial"));
                    object.insert("port".into(), serde_json::json!(port));
                    object.insert("baudrate".into(), serde_json::json!(baud_rate));
                }
                None => {}
            }
        }
        Ok(())
    }

    /// `/api/device/framing` 的完整处理（参数校验 → 严格模式授权 → 执行 → 诊断事件 → 结束授权），
    /// 工作台按钮与语音“按预览走边框”（`/api/workflow` action=frame）共用。
    pub(crate) fn framing_with_authority(
        &self,
        mut object: serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        // 先校验整数参数，审批摘要与实际下发使用同一份已校验的值（2026-11-05）。
        if let Err(error) = framing_feed_and_power(&object) {
            return framing_params_invalid(error);
        }
        // 橡皮筋轮廓同样先校验，非法时不创建审批（与 run_framing 用同一套默认范围）
        let number =
            |key: &str, default: f64| object.get(key).and_then(|v| v.as_f64()).unwrap_or(default);
        let bounds = (
            number("min_x", 0.0),
            number("min_y", 0.0),
            number("max_x", 100.0),
            number("max_y", 100.0),
        );
        if let Err(error) = crate::safe_action::framing_outline(&object, bounds) {
            return framing_params_invalid(error);
        }
        // 严格模式检查点（技术书 §6.2）：走边框真实驱动机器
        let framing_facts = approval::framing_digest_facts(&object);
        let framing_summary = framing_approval_summary(&framing_facts);
        let framing_authority = match self.require_r3_authority(
            ctx,
            approval::R3Action {
                kind: laser_contracts::ApprovalKind::Framing,
                target_record: "framing",
                facts: framing_facts,
                request: &object,
                summary: framing_summary,
            },
        ) {
            Ok(authority) => authority,
            Err((status, body)) => return http_json(status, body),
        };
        if let approval::R3Authority::Approval(record) = &framing_authority {
            match record.connection.as_ref() {
                Some(connection) => object = approval::object_with_connection(&object, connection),
                None => {
                    let (status, body) =
                        approval::error_response(&approval::ApprovalError::Mismatch);
                    return http_json(status, body);
                }
            }
        }
        let response = self.run_framing(&object, ctx);
        let outcome: Result<serde_json::Value, (u16, serde_json::Value)> =
            if response.starts_with(b"HTTP/1.1 200") {
                Ok(serde_json::Value::Null)
            } else {
                Err((400, failure("走边框未完成", None)))
            };
        if !response.starts_with(b"HTTP/1.1 200") {
            let body = http_response_json(&response);
            let message = body["result"].as_str().unwrap_or("走边框未完成").to_owned();
            self.emit_event(LaserEvent {
                level: LaserEventLevel::Error,
                text: format!("走边框没有完成：{message}"),
                hint: Some(
                    laser_failure_hint(&message)
                        .unwrap_or("到设备旁确认机器状态，排除原因后再走边框")
                        .to_owned(),
                ),
                detail: Some(body.to_string()),
            });
        }
        self.finish_r3_authority(&framing_authority, &outcome, None);
        response
    }

    /// 语音 / 小智按预览走边框（2026-11-18 用户决定，技术书 r51）：`/api/workflow` action=frame。
    /// 范围只取这份预览记录的 `artifacts.frame_mm`，与预览窗“先走边框”同一参照（以激光头当前位置为原点、
    /// 走完回到起点）；功率固定 0 只空走不出光、速度 2000 mm/min。请求里的坐标 / 轮廓 / 速度一概不用，
    /// 带了非 0 功率直接拒绝。先过与 confirm_send 相同的状态与刀路 SHA-256 门禁，保证走的就是之后要刻的那份；
    /// 必须 `confirmed=true`；严格模式审批、语音授权、设备占用与急停沿用 `/api/device/framing`。
    /// 走完不改 workflow 状态：挪好材料后可再走一次，或在用户确认开始后 confirm_send。
    pub(crate) fn workflow_frame(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let Some(workflow_id) = object
            .get("workflow_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
        else {
            return http_json(400, failure("workflow_id 不能为空", None));
        };
        if object.get("confirmed").and_then(|v| v.as_bool()) != Some(true) {
            return http_json(
                400,
                failure(
                    "走边框会让机器运动：先向用户说明并得到明确确认，再带 confirmed=true",
                    Some(serde_json::json!({"error_code": "confirmation_required"})),
                ),
            );
        }
        if object
            .get("power")
            .filter(|power| !power.is_null())
            .is_some_and(|power| power.as_f64() != Some(0.0))
        {
            return http_json(
                400,
                failure(
                    "按预览走边框只空走、不出光；需要弱光指示请在电脑工作台操作",
                    Some(serde_json::json!({"error_code": "framing_light_not_allowed"})),
                ),
            );
        }
        let stored = match self.with_effective_store(|store| {
            store.get(laser_runtime::RecordKind::Workflow, &workflow_id)
        }) {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                return http_json(
                    404,
                    failure(
                        "找不到这份预览，请先生成预览",
                        Some(serde_json::json!({"error_code": "workflow_not_found"})),
                    ),
                );
            }
            Err(_) => return http_json(500, failure("读取 workflow 失败", None)),
        };
        let status = stored
            .status
            .as_deref()
            .or_else(|| stored.payload.get("status").and_then(|v| v.as_str()));
        if let Some(reason) = crate::workflow::workflow_send_blocked_reason(status) {
            return http_json(
                400,
                failure(
                    reason,
                    Some(serde_json::json!({"error_code": "workflow_not_sendable"})),
                ),
            );
        }
        // 与 confirm_send / resume 相同的材料安全复核（PVC 等直接拒绝）
        if let Err(error) = crate::materials::validate_material_safety_in_payload(&stored.payload) {
            return http_json(
                400,
                failure(
                    error,
                    Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                ),
            );
        }
        let gate = crate::workflow::build_send_preflight(&stored.payload, &self.send_gcode_roots())
            .and_then(|(preflight, _, _)| {
                laser_device::authorize_send(&preflight).map_err(|error| error.to_string())
            });
        if let Err(error) = gate {
            return http_json(
                400,
                failure(
                    format!("走边框安全门禁未通过: {error}"),
                    Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                ),
            );
        }
        let frame = &stored.payload["artifacts"]["frame_mm"];
        let value = |key: &str| {
            frame
                .get(key)
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite())
        };
        let (Some(min_x), Some(max_x), Some(min_y), Some(max_y)) = (
            value("min_x"),
            value("max_x"),
            value("min_y"),
            value("max_y"),
        ) else {
            return http_json(
                400,
                failure(
                    "这份预览没有记录加工范围（例如导入的 G-code 起点由文件决定），不能按预览走边框",
                    Some(serde_json::json!({"error_code": "framing_not_available"})),
                ),
            );
        };
        if max_x <= min_x || max_y <= min_y {
            return http_json(
                400,
                failure(
                    "这份预览的加工范围为空，不能走边框",
                    Some(serde_json::json!({"error_code": "framing_not_available"})),
                ),
            );
        }
        let round = |v: f64| (v * 1000.0).round() / 1000.0;
        let mut framing = object.clone();
        for key in ["action", "workflow_id", "outline", "feed_rate", "power"] {
            framing.remove(key);
        }
        framing.insert("relative_to_head".into(), serde_json::Value::Bool(true));
        framing.insert("min_x".into(), serde_json::json!(round(min_x)));
        framing.insert("max_x".into(), serde_json::json!(round(max_x)));
        framing.insert("min_y".into(), serde_json::json!(round(min_y)));
        framing.insert("max_y".into(), serde_json::json!(round(max_y)));
        framing.insert("feed_rate".into(), serde_json::json!(2000));
        framing.insert("power".into(), serde_json::json!(0));
        let response = self.framing_with_authority(framing, ctx);
        if !response.starts_with(b"HTTP/1.1 200") {
            return response;
        }
        let mut body = http_response_json(&response);
        body["result"]["workflow_id"] = serde_json::Value::String(workflow_id);
        body["result"]["next_actions"] = serde_json::json!(["frame", "confirm_send", "cancel"]);
        body["result"]["hint"] = serde_json::Value::String(
            "边框已空走一圈（未出光），激光头已回到起点。请用户核对材料位置；挪好后可再走一次，用户明确说确认开始后再 confirm_send".into(),
        );
        http_json(200, body)
    }

    /// 走边框：真实驱动机器沿外接矩形运动，按发送链路对待（2026-10-14）。
    /// 与完整发送相同的设备占用检查、握手与 Idle 预检；运行期间登记为活动作业（`framing-…`），
    /// 顶栏 / 页面急停与取消都能找到它。默认不出光（`power` 缺省 0），弱光指示需显式传 `power`。
    pub(crate) fn run_framing(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        let permit = match self.voice_permit(ctx) {
            Ok(permit) => permit,
            Err((status, body)) => return http_json(status, body),
        };
        let min_x = object.get("min_x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let min_y = object.get("min_y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let max_x = object
            .get("max_x")
            .and_then(|v| v.as_f64())
            .unwrap_or(100.0);
        let max_y = object
            .get("max_y")
            .and_then(|v| v.as_f64())
            .unwrap_or(100.0);
        let (feed_rate, power) = match framing_feed_and_power(object) {
            Ok(values) => values,
            Err(error) => return framing_params_invalid(error),
        };

        // 2026-11-16 橡皮筋走边框（对齐 LightBurn）：带 `outline` 时沿多边形走，否则走外接矩形
        let outline = match framing_outline(object, (min_x, min_y, max_x, max_y)) {
            Ok(outline) => outline,
            Err(error) => return framing_params_invalid(error),
        };
        let generated = match &outline {
            Some(points) => {
                laser_core::gcode::generate_outline_framing_gcode(points, feed_rate, power)
            }
            None => laser_core::gcode::generate_framing_gcode(
                min_x, min_y, max_x, max_y, feed_rate, power,
            ),
        };
        let framing_gcode = match generated {
            Ok(g) => g,
            Err(e) => return http_json(400, failure(format!("生成走边框 G-code 失败: {e}"), None)),
        };
        // relative_to_head（2026-11-06，AI 预览窗“先走边框”）：以激光头当前位置为 (0,0)，范围取预览返回的
        // frame_mm（可为负），与即将开刻的任务同一参照；生成器结尾的 G0 X0 Y0 即回到起点
        let framing_gcode =
            if object.get("relative_to_head").and_then(|v| v.as_bool()) == Some(true) {
                format!("G21\nG92 X0 Y0 Z0\n{framing_gcode}")
            } else {
                framing_gcode
            };
        let lines: Vec<String> = match laser_device::prepare_gcode_lines(&framing_gcode) {
            Ok(p) => p.lines,
            Err(e) => return http_json(400, failure(format!("走边框 G-code 校验失败: {e}"), None)),
        };
        let job = ShortDeviceJob {
            label: "走边框",
            id_prefix: "framing-",
            failed_code: "framing_failed",
            cancelled_code: "framing_cancelled",
            travel_check: Some(&framing_gcode),
        };
        let job_id = match self.run_short_device_job(object, permit, lines, &job) {
            Ok(job_id) => job_id,
            Err(response) => return response,
        };
        let bounds =
            serde_json::json!({ "min_x": min_x, "min_y": min_y, "max_x": max_x, "max_y": max_y });
        http_json(
            200,
            success(serde_json::json!({
                "message": "走边框完成",
                "job_id": job_id,
                "laser_on": power > 0,
                "bounds": bounds,
            })),
        )
    }

    /// 低功率点射（2026-11-16，对齐 LightBurn “Fire”）：没有红光指示的机器在两点对位时，
    /// 以极低功率原地打一个点看清激光头位置。只给工作台 / 画板界面的人手动用：
    /// 语音、小智 MCP、网关与审批重放一律 403；启用了桌面会话时不带会话的本机回环进程 403；
    /// 严格模式下只有桌面会话可用（点射不进审批队列）。
    /// 功率 S1..=10（0~1000 标尺，下发时按 `$30` 换算）、时长 10..=200 ms，越界拒绝不截断；
    /// 必须 `confirmed=true`。走与走边框相同的设备占用、握手、Idle 预检与活动作业登记（`pointer-…`），
    /// 关光行 `M5 S0` 与开光行一起进入控制器缓冲区；失败或取消时补发实时 0x18。
    pub(crate) fn run_pointer_fire(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Vec<u8> {
        if !matches!(
            ctx,
            laser_contracts::CallerContext::Desktop
                | laser_contracts::CallerContext::Loopback
                | laser_contracts::CallerContext::Lan { .. }
        ) {
            return http_json(
                403,
                failure(
                    "点射只能在工作台或画板界面由人手动操作，语音与 AI 工具不能出光",
                    Some(serde_json::json!({"error_code": "pointer_fire_not_allowed"})),
                ),
            );
        }
        // 桌面版里工作台 / 画板总以桌面会话来访：不带会话的本机进程不能出光（与确认停机、设置一致）
        if self.desktop_session.get().is_some()
            && matches!(ctx, laser_contracts::CallerContext::Loopback)
        {
            return http_json(
                403,
                failure(
                    "点射只能在 MOSS 桌面端的工作台或画板里操作",
                    Some(serde_json::json!({"error_code": "desktop_session_required"})),
                ),
            );
        }
        let (power, duration_ms) = match pointer_fire_params(object) {
            Ok(values) => values,
            Err(error) => {
                return http_json(
                    400,
                    failure(
                        format!("点射参数无效：{error}"),
                        Some(serde_json::json!({"error_code": "pointer_fire_params_invalid"})),
                    ),
                );
            }
        };
        if object.get("confirmed").and_then(|v| v.as_bool()) != Some(true) {
            return http_json(
                400,
                failure(
                    "点射会出光，请先确认（confirmed=true）",
                    Some(serde_json::json!({"error_code": "confirmation_required"})),
                ),
            );
        }
        if let Err((status, body)) = self.require_desktop_when_strict(ctx, "pointer_fire") {
            return http_json(status, body);
        }
        let lines = pointer_fire_lines(power, duration_ms);
        let job = ShortDeviceJob {
            label: "点射",
            id_prefix: "pointer-",
            failed_code: "pointer_fire_failed",
            cancelled_code: "pointer_fire_cancelled",
            travel_check: None,
        };
        match self.run_short_device_job(object, None, lines, &job) {
            Ok(job_id) => http_json(
                200,
                success(serde_json::json!({
                    "message": "点射完成，激光已关闭",
                    "job_id": job_id,
                    "power": power,
                    "duration_ms": duration_ms,
                })),
            ),
            Err(response) => response,
        }
    }

    /// 走边框与点射共用的短作业执行：同设备占用检查、打开传输、有界握手、Idle 预检、
    /// （可选）行程复核，登记为活动作业后流式发送；完成后解除占用，失败或取消时补发 0x18
    /// 并原子转为“待确认停止”。成功返回作业编号，失败返回完整 HTTP 响应。
    fn run_short_device_job(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        permit: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        lines: Vec<String>,
        job_spec: &ShortDeviceJob<'_>,
    ) -> Result<String, Vec<u8>> {
        use std::sync::atomic::Ordering;
        let label = job_spec.label;
        let failed = |status: u16, message: String, code: &str| {
            Err(http_json(
                status,
                failure(message, Some(serde_json::json!({"error_code": code}))),
            ))
        };
        let spec = match self.connection_spec_from_request(object) {
            Ok(s) => s,
            Err(e) => {
                return failed(
                    400,
                    format!("连接参数无效: {e}"),
                    "connection_params_invalid",
                );
            }
        };
        let target = spec.describe();
        if let Ok(jobs) = self.active_jobs.lock()
            && let Some(job) = jobs.values().find(|job| job.target == target)
        {
            return failed(
                409,
                format!(
                    "设备 {target} 正在执行 {}，请等待完成或先取消",
                    job.workflow_id
                ),
                "device_busy",
            );
        }
        let transport: std::sync::Arc<dyn laser_device::DeviceTransport> = {
            let open_mutex = target_open_mutex(&self.target_open_locks, &target);
            let _open_guard = open_mutex
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            release_uncertain_target_handle(&self.uncertain_jobs, &target);
            match (self.transport_factory)(&spec) {
                Ok(t) => std::sync::Arc::from(t),
                Err(e) => {
                    return failed(
                        400,
                        format!("设备连接失败 ({target}): {e}"),
                        "device_unreachable",
                    );
                }
            }
        };
        let mut device = laser_device::DeviceConnection::new(transport.clone());
        if let Err(e) = device.handshake_with_deadline(std::time::Duration::from_secs(3)) {
            return failed(
                400,
                format!("设备握手失败 ({target}): {e}"),
                "device_not_ready",
            );
        }
        device.load_features(std::time::Duration::from_secs(3));
        let device_scale = match device.verify_job_ready_adaptive(std::time::Duration::from_secs(3))
        {
            Ok(scale) => scale,
            Err(e) => {
                return failed(
                    400,
                    format!("{label}前检查未通过 ({target}): {e}"),
                    "device_preflight_failed",
                );
            }
        };
        if let Some(gcode) = job_spec.travel_check
            && let Err(error) = check_job_fits_machine(gcode, &device.travel_limits)
        {
            return failed(400, format!("{error}（{target}）"), "job_exceeds_travel");
        }
        if permit.as_ref().is_some_and(|p| !p.load(Ordering::Acquire)) {
            return failed(
                403,
                format!("预检期间设备授权已撤销，未开始{label}"),
                "voice_session_revoked",
            );
        }

        let job_id = next_workflow_id().replacen("wf-", job_spec.id_prefix, 1);
        let progress = std::sync::Arc::new(laser_device::JobProgress::default());
        progress.total_lines.store(lines.len(), Ordering::Relaxed);
        let cancel = laser_device::CancellationToken::default();
        let job = std::sync::Arc::new(ActiveJob {
            workflow_id: job_id.clone(),
            mode: spec.mode(),
            target: target.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
            transport: transport.clone(),
            started_at: std::time::Instant::now(),
            spec: Some(spec.clone()),
        });
        let cleared_uncertain = {
            let mut jobs = self
                .active_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(busy) = jobs.values().find(|j| j.target == target) {
                return failed(
                    409,
                    format!(
                        "设备 {target} 正在执行 {}，请等待完成或先取消",
                        busy.workflow_id
                    ),
                    "device_busy",
                );
            }
            let mut uncertain = self
                .uncertain_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut dropped = Vec::new();
            // 发起方（内置语音）已放弃等待：这次不再开始；否则在同一把锁内登记并回执作业编号
            let watch = crate::jobs::current_short_job_watch();
            let mut watch_state = watch.as_ref().map(|watch| {
                watch
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            if watch_state
                .as_deref()
                .is_some_and(|state| *state == crate::jobs::ShortJobStart::Abandoned)
            {
                return failed(
                    409,
                    format!("语音那边已不再等待，未开始{label}"),
                    "short_job_abandoned",
                );
            }
            uncertain.retain(|_, old| {
                if old.target == target {
                    dropped.push(old.clone());
                    false
                } else {
                    true
                }
            });
            jobs.insert(job_id.clone(), job.clone());
            if let Some(state) = watch_state.as_deref_mut() {
                *state = crate::jobs::ShortJobStart::Started(job_id.clone());
            }
            drop(watch_state);
            dropped
        };
        drop(cleared_uncertain);
        let outcome = {
            let mut worker =
                laser_device::JobWorker::with_progress(device, cancel.clone(), progress)
                    .with_scale_factor(device_scale / 1000.0);
            worker.send_lines(lines)
        };
        match outcome {
            Ok(_) if !cancel.is_cancelled() => {
                if let Ok(mut jobs) = self.active_jobs.lock() {
                    jobs.remove(&job_id);
                }
                Ok(job_id)
            }
            result => {
                // 取消或失败都不能证明机器已停：在同锁对内原子完成 active -> uncertain 交接，杜绝急停空窗。
                // 失败，或取消后收尾也出错（停止指令未必送达）：都补发一次 0x18
                if !cancel.is_cancelled() || result.is_err() {
                    let _ = transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
                }
                let uncertain_entry = UncertainJob::from_active(&job, Some(spec));
                let replaced_uncertain = {
                    let mut jobs = self
                        .active_jobs
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let mut uncertain = self
                        .uncertain_jobs
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    jobs.remove(&job_id);
                    let mut old_entries = Vec::new();
                    uncertain.retain(|_, old| {
                        if old.target == uncertain_entry.target {
                            old_entries.push(old.clone());
                            false
                        } else {
                            true
                        }
                    });
                    uncertain.insert(job_id.clone(), uncertain_entry);
                    old_entries
                };
                drop(replaced_uncertain);
                drop(job);
                drop(transport);
                match result {
                    Err(e) if !cancel.is_cancelled() => Err(http_json(
                        500,
                        failure(
                            format!("{label}执行失败，已发送急停 0x18，请到设备旁确认: {e}"),
                            Some(
                                serde_json::json!({"error_code": job_spec.failed_code, "job_id": job_id}),
                            ),
                        ),
                    )),
                    Err(e) => Err(http_json(
                        409,
                        failure(
                            format!(
                                "{label}已取消，但收尾时出错（{e}），已补发急停 0x18，请到设备旁确认机器已停"
                            ),
                            Some(
                                serde_json::json!({"error_code": job_spec.cancelled_code, "job_id": job_id}),
                            ),
                        ),
                    )),
                    _ => Err(http_json(
                        409,
                        failure(
                            format!("{label}已取消，急停 0x18 已发送，请到设备旁确认机器已停"),
                            Some(
                                serde_json::json!({"error_code": job_spec.cancelled_code, "job_id": job_id}),
                            ),
                        ),
                    )),
                }
            }
        }
    }
}

/// 走边框 / 点射这类短作业的标签与错误码。
pub(crate) struct ShortDeviceJob<'a> {
    /// 出现在错误文案里的动作名
    pub label: &'a str,
    /// 活动作业编号前缀（`framing-` / `pointer-`），急停与停止目标列表据此显示
    pub id_prefix: &'a str,
    pub failed_code: &'a str,
    pub cancelled_code: &'a str,
    /// 需要按固件行程复核范围的 G-code（点射不运动，不复核）
    pub travel_check: Option<&'a str>,
}

/// 点射功率上限：S10，即 0~1000 标尺的 1%（下发时按设备 `$30` 等比换算）。
pub(crate) const POINTER_FIRE_MAX_POWER: i32 = 10;
/// 点射时长上限（毫秒）。
pub(crate) const POINTER_FIRE_MAX_DURATION_MS: i32 = 200;
const POINTER_FIRE_MIN_DURATION_MS: i32 = 10;
const POINTER_FIRE_DEFAULT_DURATION_MS: i32 = 100;

/// 读取点射的 `power`（缺省上限 S10）与 `duration_ms`（缺省 100）。必须是 JSON 整数，
/// `power` 1..=10、`duration_ms` 10..=200；越界、小数、字符串一律拒绝，不截断。
pub(crate) fn pointer_fire_params(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(i32, i32), String> {
    let read = |key: &str, default: i32, min: i32, max: i32| {
        let Some(value) = object.get(key).filter(|value| !value.is_null()) else {
            return Ok(default);
        };
        value
            .as_i64()
            .and_then(|raw| i32::try_from(raw).ok())
            .filter(|parsed| (min..=max).contains(parsed))
            .ok_or_else(|| format!("{key} 必须是 {min}..={max} 的整数，收到 {value}"))
    };
    let power = read("power", POINTER_FIRE_MAX_POWER, 1, POINTER_FIRE_MAX_POWER)?;
    let duration_ms = read(
        "duration_ms",
        POINTER_FIRE_DEFAULT_DURATION_MS,
        POINTER_FIRE_MIN_DURATION_MS,
        POINTER_FIRE_MAX_DURATION_MS,
    )?;
    Ok((power, duration_ms))
}

/// 点射的 G-code：先确保关光；`G1 F100`（不带坐标，不运动）切到直线模态——GRBL / FluidNC 激光模式
/// （`$32=1`）只在 G1/G2/G3 模态下让激光输出，G0 模态下 M3 不出光；用 `M3` 恒定功率而不是 `M4`：
/// M4 动态功率按实际速度折算，原地不动时输出为 0，打不出点。`G4` 停留指定毫秒后 `M5 S0` 关光，
/// 最后 `G0` 把模态切回快速移动。这几行一共不到 50 字节，字符计数流控会在 `M3` 生效前把
/// 关光行一并写进控制器接收缓冲区。
pub(crate) fn pointer_fire_lines(power: i32, duration_ms: i32) -> Vec<String> {
    vec![
        "M5 S0".to_owned(),
        "G1 F100".to_owned(),
        format!("M3 S{power}"),
        format!("G4 P{:.3}", f64::from(duration_ms) / 1000.0),
        "M5 S0".to_owned(),
        "G0".to_owned(),
    ]
}

pub(crate) fn safe_action_approval_summary(
    action: &str,
    facts: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let label = match action {
        "unlock" => "解锁 $X",
        "home" => "回原点 $H",
        "set_origin" => "设置工作原点",
        "absolute_move" => "移动到绝对坐标",
        "relative_move" => "相对移动",
        "resume" => "继续走边框",
        other if other.contains("plus") || other.contains('+') => "提高进给或功率倍率",
        _ => "设备动作",
    };
    let coords: Vec<String> = ["x", "y", "z", "feed_rate", "power"]
        .iter()
        .filter_map(|key| facts.get(*key).map(|value| format!("{key}={value}")))
        .collect();
    if coords.is_empty() {
        format!("安全动作：{label}（{action}）")
    } else {
        format!("安全动作：{label}（{action}）；{}", coords.join("，"))
    }
}

/// 走边框进给上限（mm/min）。只挡明显错误的数值；实际速度仍受固件 `$110/$111` 限制。
const FRAMING_MAX_FEED_RATE: i32 = 20_000;

/// 读取走边框的 `feed_rate`（缺省 2000）与 `power`（缺省 0，不出光）。
/// 必须是 JSON 整数且可无损转为 `i32`；`power` 在 G-code S 标尺 0..=1000（下发时再按设备 `$30` 换算），
/// `feed_rate` 在 1..=20000。不截断、不回绕，越界一律拒绝（2026-11-05）。
pub(crate) fn framing_feed_and_power(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<(i32, i32), String> {
    fn read_int(
        object: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        default: i32,
        min: i32,
        max: i32,
    ) -> Result<i32, String> {
        let Some(value) = object.get(key).filter(|value| !value.is_null()) else {
            return Ok(default);
        };
        let parsed = value
            .as_i64()
            .and_then(|raw| i32::try_from(raw).ok())
            .filter(|parsed| (min..=max).contains(parsed));
        parsed.ok_or_else(|| format!("{key} 必须是 {min}..={max} 的整数，收到 {value}"))
    }
    let feed_rate = read_int(object, "feed_rate", 2000, 1, FRAMING_MAX_FEED_RATE)?;
    let power = read_int(object, "power", 0, 0, laser_core::safety::MAX_POWER)?;
    Ok((feed_rate, power))
}

/// 校验走边框多边形：3 ~ 256 个 `[x, y]` 有限数字，且都在给出的矩形范围内（容差 0.01 mm），
/// 这样审批摘要里的范围与实际运动一致。没带或为 null 时走矩形。
pub(crate) fn framing_outline(
    object: &serde_json::Map<String, serde_json::Value>,
    (min_x, min_y, max_x, max_y): (f64, f64, f64, f64),
) -> Result<Option<Vec<(f64, f64)>>, String> {
    let Some(value) = object.get("outline").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let items = value.as_array().ok_or("outline 必须是 [[x, y], …] 数组")?;
    let max = laser_core::gcode::MAX_FRAMING_OUTLINE_POINTS;
    if items.len() < 3 || items.len() > max {
        return Err(format!("outline 须有 3 ~ {max} 个点"));
    }
    const TOLERANCE: f64 = 0.01;
    items
        .iter()
        .map(|item| {
            let pair = item.as_array().filter(|pair| pair.len() == 2);
            let point = pair.and_then(|pair| Some((pair[0].as_f64()?, pair[1].as_f64()?)));
            match point {
                Some((x, y))
                    if x.is_finite()
                        && y.is_finite()
                        && x >= min_x - TOLERANCE
                        && x <= max_x + TOLERANCE
                        && y >= min_y - TOLERANCE
                        && y <= max_y + TOLERANCE =>
                {
                    Ok((x, y))
                }
                Some(_) => Err("outline 的点超出了 min_x..max_x / min_y..max_y 范围".to_owned()),
                None => Err("outline 的每个点必须是 [x, y] 数字".to_owned()),
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub(crate) fn framing_params_invalid(error: String) -> Vec<u8> {
    http_json(
        400,
        failure(
            format!("走边框参数无效：{error}"),
            Some(serde_json::json!({"error_code": "framing_params_invalid"})),
        ),
    )
}

pub(crate) fn framing_approval_summary(
    facts: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let value = |key: &str, default: &str| {
        facts
            .get(key)
            .map(|v| v.to_string())
            .unwrap_or_else(|| default.to_owned())
    };
    let shape = facts
        .get("outline")
        .and_then(|v| v.as_array())
        .map_or("外接矩形".to_owned(), |points| {
            format!("沿图形外轮廓 {} 个点", points.len())
        });
    format!(
        "走边框（{shape}）：X {}..{}，Y {}..{} mm；速度 {}，功率 {}",
        value("min_x", "0"),
        value("max_x", "100"),
        value("min_y", "0"),
        value("max_y", "100"),
        value("feed_rate", "2000"),
        value("power", "0（不出光）"),
    )
}

/// 经审批执行安全动作时，用审批保存的连接参数覆盖请求中的连接字段。
/// 安全动作请求里显式给出的连接目标（与活动作业的 `target` 同一写法）；没给或解析不了为 `None`。
fn explicit_safe_action_target(args: &laser_contracts::LaserSafeActionArgs) -> Option<String> {
    let has_host = args.host.as_deref().is_some_and(|s| !s.trim().is_empty());
    let has_port = args.port.as_deref().is_some_and(|s| !s.trim().is_empty());
    if !has_host && !has_port {
        return None;
    }
    laser_tools::resolve_connection_spec(
        args.port.as_deref(),
        args.host.as_deref(),
        args.baudrate,
        args.telnet_port,
        args.connection_mode.as_deref(),
    )
    .ok()
    .map(|spec| spec.describe())
}

pub(crate) fn apply_connection_to_safe_action(
    args: &mut laser_contracts::LaserSafeActionArgs,
    connection: &laser_contracts::ApprovalConnection,
) {
    args.connection_mode = Some(connection.mode().to_owned());
    match connection {
        laser_contracts::ApprovalConnection::Home { .. } => {
            args.host = None;
            args.port = None;
            args.telnet_port = None;
        }
        laser_contracts::ApprovalConnection::Agent { .. } => {
            args.host = None;
            args.port = None;
            args.telnet_port = None;
        }
        laser_contracts::ApprovalConnection::External { .. } => {
            args.host = None;
            args.port = None;
        }
        laser_contracts::ApprovalConnection::Network { host, port } => {
            args.host = Some(host.clone());
            args.telnet_port = Some(i32::from(*port));
            args.port = None;
        }
        laser_contracts::ApprovalConnection::Serial { port, baud_rate } => {
            args.port = Some(port.clone());
            args.baudrate = i32::try_from(*baud_rate).ok();
            args.host = None;
        }
    }
}
