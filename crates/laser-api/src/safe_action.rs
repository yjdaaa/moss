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
        use std::sync::atomic::Ordering;
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

        let framing_gcode = match laser_core::gcode::generate_framing_gcode(
            min_x, min_y, max_x, max_y, feed_rate, power,
        ) {
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

        let spec = match self.connection_spec_from_request(object) {
            Ok(s) => s,
            Err(e) => {
                return http_json(
                    400,
                    failure(
                        format!("连接参数无效: {e}"),
                        Some(serde_json::json!({"error_code": "connection_params_invalid"})),
                    ),
                );
            }
        };
        let target = spec.describe();
        if let Ok(jobs) = self.active_jobs.lock()
            && let Some(job) = jobs.values().find(|job| job.target == target)
        {
            return http_json(
                409,
                failure(
                    format!(
                        "设备 {target} 正在执行 {}，请等待完成或先取消",
                        job.workflow_id
                    ),
                    Some(serde_json::json!({"error_code": "device_busy"})),
                ),
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
                    return http_json(
                        400,
                        failure(
                            format!("设备连接失败 ({target}): {e}"),
                            Some(serde_json::json!({"error_code": "device_unreachable"})),
                        ),
                    );
                }
            }
        };
        let mut device = laser_device::DeviceConnection::new(transport.clone());
        if let Err(e) = device.handshake_with_deadline(std::time::Duration::from_secs(3)) {
            return http_json(
                400,
                failure(
                    format!("设备握手失败 ({target}): {e}"),
                    Some(serde_json::json!({"error_code": "device_not_ready"})),
                ),
            );
        }
        device.load_features(std::time::Duration::from_secs(3));
        let device_scale = match device.verify_job_ready_adaptive(std::time::Duration::from_secs(3))
        {
            Ok(scale) => scale,
            Err(e) => {
                return http_json(
                    400,
                    failure(
                        format!("走边框前检查未通过 ({target}): {e}"),
                        Some(serde_json::json!({"error_code": "device_preflight_failed"})),
                    ),
                );
            }
        };
        if let Err(error) = check_job_fits_machine(&framing_gcode, &device.travel_limits) {
            return http_json(
                400,
                failure(
                    format!("{error}（{target}）"),
                    Some(serde_json::json!({"error_code": "job_exceeds_travel"})),
                ),
            );
        }
        if permit.as_ref().is_some_and(|p| !p.load(Ordering::Acquire)) {
            return http_json(
                403,
                failure(
                    "预检期间设备授权已撤销，未开始走边框",
                    Some(serde_json::json!({"error_code":"voice_session_revoked"})),
                ),
            );
        }

        let job_id = next_workflow_id().replacen("wf-", "framing-", 1);
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
                return http_json(
                    409,
                    failure(
                        format!(
                            "设备 {target} 正在执行 {}，请等待完成或先取消",
                            busy.workflow_id
                        ),
                        Some(serde_json::json!({"error_code": "device_busy"})),
                    ),
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
        let bounds =
            serde_json::json!({ "min_x": min_x, "min_y": min_y, "max_x": max_x, "max_y": max_y });
        match outcome {
            Ok(_) if !cancel.is_cancelled() => {
                if let Ok(mut jobs) = self.active_jobs.lock() {
                    jobs.remove(&job_id);
                }
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
                    Err(e) if !cancel.is_cancelled() => http_json(
                        500,
                        failure(
                            format!("走边框执行失败，已发送急停 0x18，请到设备旁确认: {e}"),
                            Some(
                                serde_json::json!({"error_code": "framing_failed", "job_id": job_id}),
                            ),
                        ),
                    ),
                    _ => http_json(
                        409,
                        failure(
                            "走边框已取消，急停 0x18 已发送，请到设备旁确认机器已停",
                            Some(
                                serde_json::json!({"error_code": "framing_cancelled", "job_id": job_id}),
                            ),
                        ),
                    ),
                }
            }
        }
    }
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
    format!(
        "走边框：X {}..{}，Y {}..{} mm；速度 {}，功率 {}",
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
