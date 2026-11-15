use super::*;

/// SD 脱机上传内容与对应 SHA-256。标尺为 1000（`scale_factor == 1`）时原样上传已门禁校验的文件字节；
/// 否则上传与流式发送完全相同的已校验并按设备 `$30` 换算 S 值后的行，避免固件按原值钳位成满功率。
pub(crate) fn sd_offline_upload_payload(
    original_bytes: Vec<u8>,
    original_sha: &str,
    prepared_lines: &[String],
    scale_factor: f64,
) -> (Vec<u8>, String) {
    if !scale_factor.is_finite() || scale_factor <= 0.0 || (scale_factor - 1.0).abs() < 1e-6 {
        return (original_bytes, original_sha.to_owned());
    }
    let mut text = String::new();
    for line in prepared_lines {
        text.push_str(&laser_device::rescale_gcode_line_s(line, scale_factor));
        text.push('\n');
    }
    use sha2::Digest;
    let sha = hex::encode(sha2::Sha256::digest(text.as_bytes()));
    (text.into_bytes(), sha)
}

/// 发送启动器：门禁通过后由 workflow 处理函数回调，负责真实连接、握手并启动后台作业。
/// 返回 `send_result` JSON；失败返回 (HTTP 状态码, 失败响应体)。
pub(crate) type SendLauncher<'a> =
    &'a dyn Fn(
        &str,
        std::path::PathBuf,
        &str,
        &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)>;

/// SD 脱机：固件没有返回文件哈希时拒绝 `$SD/Run`（无法确认机器端文件就是确认过的刀路）。
const SD_HASH_UNAVAILABLE: &str = "固件没有返回 SD 文件的哈希（未启用 HashFS），无法确认机器端文件与确认的刀路一致，已拒绝启动；请在固件中启用 HashFS，或改用逐行发送";

impl ApiService {
    /// 真实发送：打开连接 → 有界握手（$32=1 门禁）→ 后台 JobWorker 逐行下发并等待 ok。
    /// 连接或握手失败时同步返回 400，workflow 保持可再次发送。
    pub(crate) fn launch_send_job(
        &self,
        workflow_id: &str,
        gcode_path: std::path::PathBuf,
        expected_sha: &str,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        self.launch_send_job_for(
            laser_runtime::RecordKind::Workflow,
            workflow_id,
            gcode_path,
            expected_sha,
            object,
            ctx,
        )
    }

    /// 与 `launch_send_job` 相同的真实下发链路，但终态写回指定记录类型
    /// （workflow 或标定矩阵 calibration）。作业注册表按记录 id 键控，
    /// 同一设备同时只允许一个在途作业。
    pub(crate) fn launch_send_job_for(
        &self,
        kind: laser_runtime::RecordKind,
        workflow_id: &str,
        gcode_path: std::path::PathBuf,
        expected_sha: &str,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let voice_permit = self.voice_permit(ctx)?;
        // 严格模式检查点（技术书 §6.2）：workflow confirm_send / send / resume、文字任务发送、
        // 标定矩阵发送都经过这里。严格模式关闭且不是审批执行时直接走原逻辑，不读取任何额外数据（A-40）。
        if !self.strict_mode() && !matches!(ctx, laser_contracts::CallerContext::Approval { .. }) {
            return self.launch_send_job_unchecked(
                kind,
                workflow_id,
                gcode_path,
                expected_sha,
                object,
                None,
                voice_permit,
            );
        }
        let (approval_kind, label) = match kind {
            laser_runtime::RecordKind::Calibration => (
                laser_contracts::ApprovalKind::CalibrationSend,
                "发送标定矩阵",
            ),
            _ => (
                laser_contracts::ApprovalKind::WorkflowConfirmSend,
                "发送加工任务",
            ),
        };
        let facts = self.send_digest_facts(kind, expected_sha, workflow_id, object);
        let summary = send_approval_summary(label, workflow_id, &facts);
        let authority = self.require_r3_authority(
            ctx,
            approval::R3Action {
                kind: approval_kind,
                target_record: workflow_id,
                facts,
                request: object,
                summary,
            },
        )?;
        let result = match &authority {
            approval::R3Authority::Approval(record) => match record.connection.as_ref() {
                Some(connection) => {
                    let effective = approval::object_with_connection(object, connection);
                    // 先记“发送中”，作业结束时由发送线程写回结果；进程若在此期间中断，
                    // 重启时据此把审批记为“结果未知”，不重放（技术书 §5.5，A-33）
                    let hook: Option<JobTerminalHook> = self.approvals.clone().map(|service| {
                        if let Err(error) = service.mark_running(&record.approval_id, workflow_id) {
                            eprintln!(
                                "[laser-api] 审批 {} 记录发送中状态失败: {}",
                                record.approval_id,
                                error.message()
                            );
                        }
                        let approval_id = record.approval_id.clone();
                        Box::new(move |status: &str, send_result: &serde_json::Value| {
                            service.record_job_result(&approval_id, status, send_result);
                        }) as JobTerminalHook
                    });
                    self.launch_send_job_unchecked(
                        kind,
                        workflow_id,
                        gcode_path,
                        expected_sha,
                        &effective,
                        hook,
                        voice_permit,
                    )
                }
                None => Err(approval::error_response(&approval::ApprovalError::Mismatch)),
            },
            _ => self.launch_send_job_unchecked(
                kind,
                workflow_id,
                gcode_path,
                expected_sha,
                object,
                None,
                voice_permit,
            ),
        };
        // 成功时的结果由发送线程在作业结束时写回；这里只记录没能启动的情况
        if result.is_err() {
            self.finish_r3_authority(&authority, &result, Some(workflow_id));
        }
        result
    }

    /// 发送摘要要素：记录中的材料、厚度、参数 + 门禁核对过的 G-code SHA + 发送方式（技术书 §5.4）。
    /// 只读持久化存储，不碰内存表（调用方可能正持有内存表的锁）。
    pub(crate) fn send_digest_facts(
        &self,
        kind: laser_runtime::RecordKind,
        expected_sha: &str,
        record_id: &str,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let payload = self
            .store
            .as_ref()
            .and_then(|store| store.get(kind, record_id).ok().flatten())
            .map(|stored| stored.payload)
            .unwrap_or(serde_json::Value::Null);
        let mut facts = approval::record_digest_facts(&payload, object);
        facts.insert(
            "gcode_sha256".into(),
            serde_json::Value::String(expected_sha.trim().to_ascii_lowercase()),
        );
        facts
    }

    /// 原有的真实下发链路（不含授权检查，只能经 [`Self::launch_send_job_for`] 调用）。
    fn launch_send_job_unchecked(
        &self,
        kind: laser_runtime::RecordKind,
        workflow_id: &str,
        gcode_path: std::path::PathBuf,
        expected_sha: &str,
        object: &serde_json::Map<String, serde_json::Value>,
        on_terminal: Option<JobTerminalHook>,
        voice_permit: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        if let Some(material) = object
            .get("material")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            && let Err(error) = validate_material_safety(material)
        {
            return Err((
                400,
                failure(
                    error,
                    Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                ),
            ));
        }
        use std::sync::atomic::Ordering;
        let spec = self.connection_spec_from_request(object).map_err(|e| {
            (
                400,
                failure(
                    format!("连接参数无效: {e}"),
                    Some(serde_json::json!({"error_code": "connection_params_invalid"})),
                ),
            )
        })?;
        let mode = spec.mode();
        let target = spec.describe();
        let is_offline_sd = object
            .get("offline_sd")
            .and_then(|v| v.as_bool())
            .or_else(|| {
                object
                    .get("send_mode")
                    .and_then(|v| v.as_str())
                    .map(|s| s == "sd_offline")
            })
            .unwrap_or(false);
        // 网络脱机上传端口在打开设备前校验：不截断（65616 不能变成 80），无效直接 400（2026-11-05）。
        let sd_http_port = if is_offline_sd && mode == "network" {
            sd_offline_http_port(object).map_err(|error| {
                (
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "sd_http_port_invalid"})),
                    ),
                )
            })?
        } else {
            80
        };
        let (cancel, _owned_reservation) = {
            let jobs = self
                .active_jobs
                .lock()
                .map_err(|_| (500, failure("活动任务注册表不可用", None)))?;
            if let Some(job) = jobs
                .get(workflow_id)
                .or_else(|| jobs.values().find(|job| job.target == target))
            {
                return Err((
                    409,
                    failure(
                        format!(
                            "设备 {target} 正在执行 workflow {}，请等待完成或先取消",
                            job.workflow_id
                        ),
                        Some(serde_json::json!({"error_code": "device_busy"})),
                    ),
                ));
            }
            let mut reservations = self
                .launching_workflows
                .lock()
                .map_err(|_| (500, failure("发送预约表不可用", None)))?;
            if let Some(existing) = reservations.active.get(workflow_id).cloned() {
                (existing, None)
            } else {
                let token = laser_device::CancellationToken::default();
                reservations
                    .active
                    .insert(workflow_id.to_owned(), token.clone());
                drop(reservations);
                drop(jobs);
                (
                    token,
                    Some(LaunchReservationGuard {
                        reservations: self.launching_workflows.clone(),
                        workflow_id: workflow_id.to_owned(),
                    }),
                )
            }
        };
        if voice_permit
            .as_ref()
            .is_some_and(|permit| !permit.load(Ordering::Acquire))
        {
            return Err((
                403,
                failure(
                    "语音设备授权已撤销，未开始加工",
                    Some(serde_json::json!({"error_code":"voice_session_revoked"})),
                ),
            ));
        }
        if cancel.is_cancelled() {
            return Err((
                409,
                failure(
                    "任务已在启动前取消，未开始发送",
                    Some(serde_json::json!({"error_code": "workflow_cancelled"})),
                ),
            ));
        }
        // 按设备目标串行化“释放旧待确认停止句柄 -> 打开独占新句柄”，杜绝同串口并发关闭/打开竞争。
        let transport: std::sync::Arc<dyn laser_device::DeviceTransport> = {
            let open_mutex = target_open_mutex(&self.target_open_locks, &target);
            let _open_guard = open_mutex
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            release_uncertain_target_handle(&self.uncertain_jobs, &target);
            let opened = (self.transport_factory)(&spec).map_err(|e| {
                (
                    400,
                    failure(
                        format!("设备连接失败 ({target}): {e}"),
                        Some(serde_json::json!({"error_code": "device_unreachable"})),
                    ),
                )
            })?;
            std::sync::Arc::from(opened)
        };
        let mut device = laser_device::DeviceConnection::new(transport.clone());
        device
            .handshake_with_deadline(std::time::Duration::from_secs(3))
            .map_err(|e| {
                (
                    400,
                    failure(
                        format!("设备握手未通过 ({target}): {e}"),
                        Some(serde_json::json!({"error_code": "device_not_ready"})),
                    ),
                )
            })?;
        let start_from_line = object
            .get("start_from_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        if start_from_line > 0 && is_offline_sd {
            return Err((
                400,
                failure(
                    "SD 脱机作业由控制器整文件执行，不支持按行断点续雕，请重新发起确认发送",
                    Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                ),
            ));
        }

        // FluidNC：读取 $CD 配置以感知自动宏 / 急停引脚 / planner；旧 GRBL 静默跳过
        device.load_features(std::time::Duration::from_secs(3));
        let preflight_res = if start_from_line > 0 {
            device.verify_job_ready_for_resume(std::time::Duration::from_secs(3))
        } else {
            device.verify_job_ready_adaptive(std::time::Duration::from_secs(3))
        };
        let device_scale = preflight_res.map_err(|e| {
            (
                400,
                failure(
                    format!("加工前检查未通过 ({target}): {e}"),
                    Some(serde_json::json!({"error_code": "device_preflight_failed"})),
                ),
            )
        })?;
        let current_mpos = device.last_mpos;
        let origin_mpos = if start_from_line == 0 {
            current_mpos
        } else {
            parse_mpos_value(object.get("_origin_mpos"))
        };
        let scale_factor = device_scale / 1000.0;
        let text = std::fs::read_to_string(&gcode_path)
            .map_err(|e| (500, failure(format!("读取 G-code 文件失败: {e}"), None)))?;
        let gcode_bytes = text.as_bytes().to_vec();
        use sha2::Digest;
        let actual_sha = hex::encode(sha2::Sha256::digest(&gcode_bytes));
        if !actual_sha.eq_ignore_ascii_case(expected_sha) {
            return Err((
                400,
                failure(
                    "G-code SHA-256 与确认快照不一致，拒绝发送",
                    Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                ),
            ));
        }
        let expected_sha = actual_sha;
        if let Err(error) = check_job_fits_machine(&text, &device.travel_limits) {
            return Err((
                400,
                failure(
                    format!("{error}（{target}）"),
                    Some(serde_json::json!({"error_code": "job_exceeds_travel"})),
                ),
            ));
        }

        let prepared = laser_device::prepare_gcode_lines(&text).map_err(|e| {
            (
                400,
                failure(
                    format!("G-code 发送前校验未通过: {e}"),
                    Some(serde_json::json!({"error_code": "gcode_line_invalid"})),
                ),
            )
        })?;
        let lines: Vec<String> = prepared.lines;
        if prepared.comment_stripped_lines > 0 {
            eprintln!(
                "[laser-api] workflow {workflow_id}: 已剥离 {} 行注释、跳过 {} 行空行/纯注释后发送（SHA 门禁仍校验原文件）",
                prepared.comment_stripped_lines, prepared.skipped_lines
            );
        }
        if lines.is_empty() {
            return Err((
                400,
                failure(
                    "G-code 文件为空，拒绝发送",
                    Some(serde_json::json!({"error_code": "gcode_empty"})),
                ),
            ));
        }
        if start_from_line >= lines.len() {
            return Err((
                400,
                failure(
                    format!("断点行号 {start_from_line} 超出总行数 {}", lines.len()),
                    Some(serde_json::json!({"error_code": "invalid_resume_line"})),
                ),
            ));
        }
        if voice_permit
            .as_ref()
            .is_some_and(|permit| !permit.load(Ordering::Acquire))
        {
            return Err((
                403,
                failure(
                    "预检期间设备授权已撤销，未开始加工",
                    Some(serde_json::json!({"error_code":"voice_session_revoked"})),
                ),
            ));
        }
        // SD 脱机文件与流式发送使用同一功率标尺：设备 $30 不是 1000 时，上传按标尺换算后的已校验行，
        // 并以实际上传字节的 SHA-256 做固件侧哈希门禁（原文件 SHA 已在上面与确认快照比对）。
        let (sd_upload_bytes, sd_upload_sha) =
            sd_offline_upload_payload(gcode_bytes, &expected_sha, &lines, scale_factor);
        let progress = std::sync::Arc::new(laser_device::JobProgress::default());
        progress.total_lines.store(lines.len(), Ordering::Relaxed);
        progress
            .lines_sent
            .store(start_from_line, Ordering::Relaxed);
        let job = std::sync::Arc::new(ActiveJob {
            workflow_id: workflow_id.to_owned(),
            mode: if is_offline_sd { "sd_offline" } else { mode },
            target: target.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
            transport: transport.clone(),
            started_at: std::time::Instant::now(),
        });
        let target_for_thread = target.clone();
        let spec_for_thread = spec.clone();
        let cleared_uncertain = {
            let mut jobs = self
                .active_jobs
                .lock()
                .map_err(|_| (500, failure("活动任务注册表不可用", None)))?;
            if cancel.is_cancelled() {
                return Err((
                    409,
                    failure(
                        "预检期间任务已被取消，未下发刀路",
                        Some(serde_json::json!({"error_code": "workflow_cancelled"})),
                    ),
                ));
            }
            if let Some(busy) = jobs
                .get(workflow_id)
                .or_else(|| jobs.values().find(|j| j.target == target))
            {
                return Err((
                    409,
                    failure(
                        format!(
                            "设备 {target} 正在执行 workflow {}，请等待完成或先取消",
                            busy.workflow_id
                        ),
                        Some(serde_json::json!({"error_code": "device_busy"})),
                    ),
                ));
            }
            let mut uncertain = self
                .uncertain_jobs
                .lock()
                .map_err(|_| (500, failure("待确认停止任务表不可用", None)))?;
            let mut dropped = Vec::new();
            uncertain.retain(|_, old| {
                if old.target == target {
                    dropped.push(old.clone());
                    false
                } else {
                    true
                }
            });
            jobs.insert(workflow_id.to_owned(), job.clone());
            dropped
        };
        drop(cleared_uncertain);
        drop(transport);
        let store = self.store.clone();
        let workflows = self.workflows.clone();
        let calibrations = self.calibrations.clone();
        let jobs_registry = self.active_jobs.clone();
        let event_sink = self.event_sink.clone();
        let uncertain_registry = self.uncertain_jobs.clone();
        let wf_id = workflow_id.to_owned();
        let total = lines.len();
        let obj_clone = object.clone();
        std::thread::Builder::new()
            .name(format!("laser-send-{wf_id}"))
            .spawn(move || {
                let sent_mode = if is_offline_sd { "sd_offline" } else { mode };
                let outcome = {
                    let mut device = device;
                    if voice_permit.as_ref().is_some_and(|permit| !permit.load(Ordering::Acquire)) {
                    Err("语音授权已撤销，排队中的加工未执行".to_owned())
                } else if is_offline_sd {
                    // SD 离线加工流程：
                    // 1. 唯一指纹文件名 /sd/moss_{workflow_id}_{short_sha}.gcode
                    // 2. HTTP POST /upload 上传文件内容到机床 SD 卡
                    // 3. 发送 $File/ShowHash 校验硬件计算的 SHA-256 物理一致性；固件不返回哈希（未启用 HashFS）时拒绝启动（2026-10-14 用户决定）
                    // 4. 发送 $SD/Run 并进入 wait_until_sd_completed 监控
                    let short_sha = if sd_upload_sha.len() >= 8 { &sd_upload_sha[..8] } else { &sd_upload_sha };
                    let safe_wf_id = wf_id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_");
                    let remote_filename = format!("moss_{safe_wf_id}_{short_sha}.gcode");
                    let remote_path = format!("/sd/{remote_filename}");

                    let upload_res = if mode == "network" {
                        let http_port = sd_http_port;
                        let host = obj_clone
                            .get("network_host")
                            .or_else(|| obj_clone.get("host"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim();
                        if host.is_empty() {
                            Err("脱机作业需要提供有效的主机 IP 地址".to_string())
                        } else {
                            laser_device::upload_gcode_to_fluidnc_sd(
                                host,
                                http_port,
                                &remote_filename,
                                &sd_upload_bytes,
                                std::time::Duration::from_secs(15),
                            )
                        }
                    } else {
                        Err("串口模式暂不支持自动网络传卡，请通过网络连接或手动拷贝至 SD 卡".to_string())
                    };

                    let verified = match upload_res {
                        Err(e) => Err(format!("上传脱机文件失败: {e}")),
                        Ok(()) => device
                            .verify_sd_file_hash(&remote_path, &sd_upload_sha, std::time::Duration::from_secs(10))
                            .map_err(|e| format!("脱机文件硬件安全门禁校验失败: {e}")),
                    };
                    match verified {
                        Err(e) => Err(e),
                        Ok(false) => Err(SD_HASH_UNAVAILABLE.to_owned()),
                        Ok(true) => {
                            // 上传和哈希校验可能耗时很久；真正启动机器前必须再次检查撤销和取消。
                            // 不论结果是否已经上传到 SD，都不能在授权失效后发送 $SD/Run。
                            if cancel.is_cancelled() || voice_permit.as_ref().is_some_and(|permit| !permit.load(Ordering::Acquire)) {
                                Err("SD 启动前加工已取消或语音授权已撤销，未发送 $SD/Run".to_owned())
                            } else {
                                // 2026-11-09：$SD/Run 的 ok 要等整个文件跑完才回（FluidNC 推迟应答），
                                // 发出后直接监控进度、取消与结束，不再“发一行等 ok”
                                device
                                    .run_sd_file_and_wait(&remote_path, std::time::Duration::from_secs(7200), Some(&progress), Some(&cancel))
                                    .map(|_| total)
                                    .map_err(|e| format!("SD 脱机任务未完成: {e}"))
                            }
                        }
                    }
                } else {
                    let mut worker = laser_device::JobWorker::with_progress(device, cancel.clone(), progress.clone())
                        .with_scale_factor(scale_factor);
                    if start_from_line > 0 {
                        // 断点续雕：先同步下发从前缀刀路重建的模态头（G21/G90/G92 偏移/无光定位 G0/进给率 F/M4 S0），
                        // 避免 0x18 软复位清空模态后首条 G1 报 error:22、丢 M4 不出光或丢 G92 偏移撞错位；不计入刀路进度行。
                        let plan = build_resume_plan(
                            &lines[..start_from_line],
                            origin_mpos,
                            current_mpos,
                        );
                        let mut preamble_res = Ok(());
                        for cmd in plan.preamble {
                            if let Err(e) = worker
                                .device
                                .send_line_wait_ack_cancellable(&cmd, Some(&cancel))
                            {
                                preamble_res = Err(format!("断点续雕恢复模态指令 `{cmd}` 失败: {e}"));
                                break;
                            }
                        }
                        preamble_res.and_then(|()| {
                            let mut remaining = lines[start_from_line..].to_vec();
                            apply_modal_spindle_to_remaining(
                                &mut remaining,
                                plan.modal_spindle_s,
                                plan.resume_in_cut_motion,
                            );
                            worker.send_lines(remaining)
                        })
                    } else {
                        worker.send_lines(lines)
                    }
                    }
                };
                let sent = progress.lines_sent.load(Ordering::Relaxed);
                let last_status_report = progress.status_report();
                let elapsed_ms = job.started_at.elapsed().as_millis() as u64;
                let target = target_for_thread;
                let cancelled_result = || {
                    let can_resume = !is_offline_sd && sent > 0 && sent < total;
                    serde_json::json!({
                        "success": false,
                        "running": false,
                        "cancelled": true,
                        "can_resume": can_resume,
                        "resumable_from_line": if can_resume { sent } else { 0 },
                        "mode": sent_mode,
                        "target": target,
                        "lines_sent": sent,
                        "total_lines": total,
                        "elapsed_ms": elapsed_ms,
                        "error": if can_resume {
                            format!("用户急停/取消：已在第 {sent}/{total} 行安全停机（可从第 {} 行继续加工）", sent + 1)
                        } else {
                            "用户取消：已执行 ! → ? → 0x18 安全停机序列".into()
                        }
                    })
                };
                let (status, mut send_result) = match outcome {
                    Ok(_) if cancel.is_cancelled() => ("cancelled", cancelled_result()),
                    Ok(_) => (
                        "completed",
                        serde_json::json!({"success": true, "running": false, "phase": "completed", "mode": sent_mode, "target": target, "lines_sent": sent, "total_lines": total, "elapsed_ms": elapsed_ms}),
                    ),
                    Err(_error) if cancel.is_cancelled() => ("cancelled", cancelled_result()),
                    Err(error) => {
                        let _ = job.transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
                        let diagnostic = laser_device::diagnose_device_message(&error)
                            .map(|d| format!(" ({d})"))
                            .unwrap_or_default();
                        let error_msg = format!("{error}{diagnostic}");
                        (
                            "failed",
                            serde_json::json!({"success": false, "running": false, "mode": sent_mode, "target": target, "lines_sent": sent, "total_lines": total, "elapsed_ms": elapsed_ms, "error": error_msg}),
                        )
                    }
                };
                if let (Some((mx, my, mz)), Some(map)) = (origin_mpos, send_result.as_object_mut())
                    && status != "completed"
                {
                    map.insert("origin_mpos".into(), serde_json::json!([mx, my, mz]));
                }
                if let (Some(raw), Some(map)) = (last_status_report, send_result.as_object_mut()) {
                    map.insert("last_status_report".into(), serde_json::json!(raw));
                }
                if let Some(hook) = on_terminal {
                    hook(status, &send_result);
                }
                emit_laser_event(&event_sink, job_terminal_event(&wf_id, status, &send_result));
                finalize_workflow_record(
                    store.as_deref(),
                    &workflows,
                    &calibrations,
                    &event_sink,
                    kind,
                    &wf_id,
                    status,
                    send_result,
                );
                if status == "completed" {
                    if let Ok(mut jobs) = jobs_registry.lock() {
                        jobs.remove(&wf_id);
                    }
                    drop(job);
                } else {
                    // cancelled/failed 都不能证明激光已物理停止；在同锁对内原子完成 active -> uncertain 迁移。
                    let uncertain_entry = UncertainJob::from_active(&job, Some(spec_for_thread));
                    let replaced_uncertain = {
                        let mut jobs = jobs_registry
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let mut uncertain = uncertain_registry
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        jobs.remove(&wf_id);
                        let mut old_entries = Vec::new();
                        uncertain.retain(|_, old| {
                            if old.target == uncertain_entry.target {
                                old_entries.push(old.clone());
                                false
                            } else {
                                true
                            }
                        });
                        uncertain.insert(wf_id.clone(), uncertain_entry);
                        old_entries
                    };
                    drop(replaced_uncertain);
                    drop(job);
                }
            })
            .map_err(|e| {
                if let Ok(mut jobs) = self.active_jobs.lock() {
                    jobs.remove(workflow_id);
                }
                (500, failure(format!("无法启动发送线程: {e}"), None))
            })?;
        Ok(serde_json::json!({
            "success": true,
            "running": true,
            "mode": mode,
            "target": target,
            "lines_sent": start_from_line,
            "total_lines": total,
            "resumed": start_from_line > 0,
            "elapsed_ms": 0,
        }))
    }

    /// 发送门禁允许解析 G-code 刀路的目录集合：预览根 + 生成目录 + 上传目录
    pub(crate) fn send_gcode_roots(&self) -> Vec<std::path::PathBuf> {
        let mut roots = self.preview_roots.clone();
        for extra in [self.generated_root.as_ref(), self.upload_root.as_ref()]
            .into_iter()
            .flatten()
        {
            if !roots.contains(extra) {
                roots.push(extra.clone());
            }
        }
        roots
    }
}

/// SD 脱机上传使用的 FluidNC HTTP 端口：`network_http_port`，其次 `http_port`，都缺省时为 80。
/// 给出的值必须是 1..=65535 的整数，否则拒绝（不截断、不回落到 80）。
pub(crate) fn sd_offline_http_port(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<u16, String> {
    let Some((key, value)) = ["network_http_port", "http_port"]
        .into_iter()
        .find_map(|key| {
            object
                .get(key)
                .filter(|value| !value.is_null())
                .map(|value| (key, value))
        })
    else {
        return Ok(80);
    };
    value
        .as_u64()
        .and_then(|raw| u16::try_from(raw).ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| {
            format!("脱机上传端口 {key} 无效：{value}，必须是 1..=65535 的整数，未连接设备")
        })
}

/// 审批卡上的发送摘要（技术书 §9.5.2）：内容、材料、参数与发送方式；不含文件路径与设备地址。
fn send_approval_summary(
    label: &str,
    record_id: &str,
    facts: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut parts = vec![format!("{label} {record_id}")];
    if let Some(material) = facts
        .get("material")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
    {
        match facts.get("thickness_mm").and_then(|v| v.as_f64()) {
            Some(thickness) => parts.push(format!("{material} {thickness} mm")),
            None => parts.push(material.to_owned()),
        }
    }
    if let Some(params) = facts.get("params").and_then(|v| v.as_object()) {
        let mut items = Vec::new();
        for (key, label) in [
            ("power", "功率"),
            ("laser_max_power", "最大功率"),
            ("feed_rate", "速度"),
            ("passes", "遍数"),
        ] {
            if let Some(value) = params.get(key).filter(|v| !v.is_null()) {
                items.push(format!("{label} {value}"));
            }
        }
        if !items.is_empty() {
            parts.push(items.join("，"));
        }
    }
    if let Some(mode) = facts.get("send_mode").and_then(|v| v.as_str()) {
        parts.push(if mode == "sd_offline" {
            "脱机 SD 卡加工".into()
        } else {
            "逐行发送".into()
        });
    }
    if let Some(line) = facts.get("start_from_line").and_then(|v| v.as_u64()) {
        parts.push(format!("从第 {line} 行继续"));
    }
    parts.join("；")
}
