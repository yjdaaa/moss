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
    /// 语音、小智 MCP、网关与审批重放一律 403；严格模式下只有桌面会话可用（点射不进审批队列）。
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
            uncertain.retain(|_, old| {
                if old.target == target {
                    dropped.push(old.clone());
                    false
                } else {
                    true
                }
            });
            jobs.insert(job_id.clone(), job.clone());
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
                if !cancel.is_cancelled() {
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
