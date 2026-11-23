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

type SharedDeviceConnection =
    laser_device::DeviceConnection<std::sync::Arc<dyn laser_device::DeviceTransport>>;

struct VerifiedGcode {
    text: String,
    bytes: Vec<u8>,
    sha256: String,
    lines: Vec<String>,
}

struct SendLaunchPlan {
    spec: laser_device::ConnectionSpec,
    mode: &'static str,
    target: String,
    is_offline_sd: bool,
    sd_http_port: u16,
    start_from_line: usize,
}

struct SendPreflightGuard {
    active_jobs: ActiveJobs,
    uncertain_jobs: UncertainJobs,
    job: std::sync::Arc<ActiveJob>,
    spec: laser_device::ConnectionSpec,
    armed: bool,
}

impl Drop for SendPreflightGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.job.cancel.is_cancelled() {
            let _ = send_emergency_stop_transport(self.job.transport.as_ref());
            let uncertain_entry = UncertainJob::from_active(&self.job, Some(self.spec.clone()));
            let mut jobs = self
                .active_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut uncertain = self
                .uncertain_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            jobs.remove(&self.job.workflow_id);
            uncertain.retain(|_, old| old.target != uncertain_entry.target);
            uncertain.insert(self.job.workflow_id.clone(), uncertain_entry);
        } else {
            let mut jobs = self
                .active_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            jobs.remove(&self.job.workflow_id);
        }
    }
}

struct SendPreflightOutcome {
    verified: VerifiedGcode,
    scale_factor: f64,
    origin_mpos: Option<(f64, f64, f64)>,
    current_mpos: Option<(f64, f64, f64)>,
}

struct SendWorkerParams {
    kind: laser_runtime::RecordKind,
    workflow_id: String,
    mode: &'static str,
    target: String,
    spec: laser_device::ConnectionSpec,
    is_offline_sd: bool,
    sd_http_port: u16,
    sd_upload_bytes: Vec<u8>,
    sd_upload_sha: String,
    lines: Vec<String>,
    scale_factor: f64,
    start_from_line: usize,
    origin_mpos: Option<(f64, f64, f64)>,
    current_mpos: Option<(f64, f64, f64)>,
    raw_estimate: Option<f64>,
    runtime_path: Option<std::path::PathBuf>,
    object: serde_json::Map<String, serde_json::Value>,
    cancel: laser_device::CancellationToken,
    progress: std::sync::Arc<laser_device::JobProgress>,
    job: std::sync::Arc<ActiveJob>,
    voice_permit: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    on_terminal: Option<JobTerminalHook>,
    store: Option<std::sync::Arc<dyn laser_runtime::RuntimeStore + Send + Sync>>,
    /// 完整过程记录（n105，默认关）：加工开始、每 10% 进度与结束结果写进时间线
    process_trace: Option<laser_runtime::ProcessTrace>,
    workflows:
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    calibrations:
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    active_jobs: ActiveJobs,
    uncertain_jobs: UncertainJobs,
    shutdown_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    event_sink: SharedLaserEventSink,
}

fn read_and_verify_gcode(
    gcode_path: &std::path::Path,
    expected_sha: &str,
    start_from_line: usize,
) -> Result<VerifiedGcode, (u16, serde_json::Value)> {
    let text = std::fs::read_to_string(gcode_path)
        .map_err(|e| (500, failure(format!("读取 G-code 文件失败: {e}"), None)))?;
    let bytes = text.as_bytes().to_vec();
    use sha2::Digest;
    let sha256 = hex::encode(sha2::Sha256::digest(&bytes));
    if !sha256.eq_ignore_ascii_case(expected_sha) {
        return Err((
            400,
            failure(
                "G-code SHA-256 与确认快照不一致，拒绝发送",
                Some(serde_json::json!({"error_code": "send_gate_rejected"})),
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
    let lines = prepared.lines;
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
    Ok(VerifiedGcode {
        text,
        bytes,
        sha256,
        lines,
    })
}

fn execute_sd_offline_job(
    device: &mut SharedDeviceConnection,
    params: &SendWorkerParams,
    total: usize,
) -> Result<usize, String> {
    use std::sync::atomic::Ordering;
    // SD 离线加工流程：
    // 1. 唯一指纹文件名 /sd/moss_{workflow_id}_{short_sha}.gcode
    // 2. HTTP POST /upload 上传文件内容到机床 SD 卡
    // 3. 发送 $File/ShowHash 校验硬件计算的 SHA-256 物理一致性；固件不返回哈希（未启用 HashFS）时拒绝启动（2026-10-14 用户决定）
    // 4. 发送 $SD/Run 并进入 wait_until_sd_completed 监控
    let short_sha = if params.sd_upload_sha.len() >= 8 {
        &params.sd_upload_sha[..8]
    } else {
        &params.sd_upload_sha
    };
    let safe_wf_id = params
        .workflow_id
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_");
    let remote_filename = format!("moss_{safe_wf_id}_{short_sha}.gcode");
    let remote_path = format!("/sd/{remote_filename}");

    let upload_res = if params.mode == "network" {
        let host = params
            .object
            .get("network_host")
            .or_else(|| params.object.get("host"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if host.is_empty() {
            Err("脱机作业需要提供有效的主机 IP 地址".to_string())
        } else {
            laser_device::upload_gcode_to_fluidnc_sd(
                host,
                params.sd_http_port,
                &remote_filename,
                &params.sd_upload_bytes,
                std::time::Duration::from_secs(15),
            )
        }
    } else {
        Err("串口模式暂不支持自动网络传卡，请通过网络连接或手动拷贝至 SD 卡".to_string())
    };

    let verified = match upload_res {
        Err(e) => Err(format!("上传脱机文件失败: {e}")),
        Ok(()) => device
            .verify_sd_file_hash(
                &remote_path,
                &params.sd_upload_sha,
                std::time::Duration::from_secs(10),
            )
            .map_err(|e| format!("脱机文件硬件安全门禁校验失败: {e}")),
    };
    match verified {
        Err(e) => Err(e),
        Ok(false) => Err(SD_HASH_UNAVAILABLE.to_owned()),
        Ok(true) => {
            // 上传和哈希校验可能耗时很久；真正启动机器前必须再次检查撤销和取消。
            // 不论结果是否已经上传到 SD，都不能在授权失效后发送 $SD/Run。
            if params.cancel.is_cancelled() {
                Ok(0)
            } else if params
                .voice_permit
                .as_ref()
                .is_some_and(|permit| !permit.load(Ordering::Acquire))
            {
                Err("SD 启动前语音授权已撤销，未发送 $SD/Run".to_owned())
            } else {
                // 2026-11-09：$SD/Run 的 ok 要等整个文件跑完才回（FluidNC 推迟应答），
                // 发出后直接监控进度、取消与结束，不再“发一行等 ok”
                device
                    .run_sd_file_or_cancel(
                        &remote_path,
                        std::time::Duration::from_secs(7200),
                        Some(&params.progress),
                        Some(&params.cancel),
                    )
                    .map(|completed| {
                        if completed {
                            total
                        } else {
                            params.progress.lines_sent.load(Ordering::Relaxed)
                        }
                    })
                    .map_err(|e| format!("SD 脱机任务未完成: {e}"))
            }
        }
    }
}

fn execute_streaming_job(
    device: SharedDeviceConnection,
    params: &SendWorkerParams,
    lines: Vec<String>,
) -> Result<usize, String> {
    let mut worker = laser_device::JobWorker::with_progress(
        device,
        params.cancel.clone(),
        params.progress.clone(),
    )
    .with_scale_factor(params.scale_factor);
    if params.start_from_line > 0 {
        // 断点续雕：先同步下发从前缀刀路重建的模态头（G21/G90/G92 偏移/无光定位 G0/进给率 F/M4 S0），
        // 避免 0x18 软复位清空模态后首条 G1 报 error:22、丢 M4 不出光或丢 G92 偏移撞错位；不计入刀路进度行。
        let plan = build_resume_plan(
            &lines[..params.start_from_line],
            params.origin_mpos,
            params.current_mpos,
        );
        for cmd in plan.preamble {
            let acked = worker
                .device
                .send_line_wait_ack_or_cancel(&cmd, Some(&params.cancel))
                .map_err(|error| format!("断点续雕恢复模态指令 `{cmd}` 失败: {error}"))?;
            if !acked {
                // 模态头也可能已经移动激光头，先进给保持并等停稳，再复位。
                worker.device.cancel()?;
                return Ok(0);
            }
        }
        let mut remaining = lines[params.start_from_line..].to_vec();
        apply_modal_spindle_to_remaining(
            &mut remaining,
            plan.modal_spindle_s,
            plan.resume_in_cut_motion,
        );
        worker.send_lines(remaining)
    } else {
        worker.send_lines(lines)
    }
}

fn build_terminal_send_outcome(
    params: &SendWorkerParams,
    sent_mode: &str,
    outcome: Result<usize, String>,
    total: usize,
) -> (&'static str, serde_json::Value) {
    use std::sync::atomic::Ordering;
    let confirmed_acked = match (&outcome, params.is_offline_sd) {
        (Ok(acked), false) => params.start_from_line.saturating_add(*acked).min(total),
        (Ok(acked), true) => (*acked).min(total),
        (Err(_), _) => params
            .progress
            .lines_sent
            .load(Ordering::Relaxed)
            .min(total),
    };
    let round_completed = params.progress.lines_completed.load(Ordering::Relaxed);
    let confirmed_completed = match (&outcome, params.is_offline_sd) {
        // 仅当本轮 JobWorker 确认了新完成行时才可加回 start_from_line；
        // 断点续雕后再次取消且无新完成行证明时，不得把上轮断点当成本轮已确认进度。
        (Ok(_), false) if round_completed > 0 => params
            .start_from_line
            .saturating_add(round_completed)
            .min(confirmed_acked),
        (Ok(acked), true) if !params.cancel.is_cancelled() => (*acked).min(total),
        _ => 0,
    };
    let last_status_report = params.progress.status_report();
    let elapsed_ms = params.job.started_at.elapsed().as_millis() as u64;
    let target = &params.target;

    let shutdown_interrupted = params.shutdown_requested.load(Ordering::Acquire);
    let (status, mut send_result) = match outcome {
        Ok(_) if shutdown_interrupted => (
            "failed",
            serde_json::json!({
                "success": false,
                "running": false,
                "cancelled": true,
                "phase": "result_unknown",
                "can_resume": false,
                "resumable_from_line": 0,
                "resume_boundary_source": "shutdown",
                "mode": sent_mode,
                "target": target,
                "lines_sent": confirmed_acked,
                "lines_completed": 0,
                "total_lines": total,
                "elapsed_ms": elapsed_ms,
                "error": "服务正在关闭，已发急停中止在途任务，执行结果未知；请核实设备状态",
            }),
        ),
        Ok(_) if params.cancel.is_cancelled() => {
            let can_resume =
                !params.is_offline_sd && confirmed_completed > 0 && confirmed_completed < total;
            let error_msg = if can_resume {
                format!(
                    "用户急停/取消：已确认完成 {confirmed_completed}/{total} 行（已传输 ACK {confirmed_acked}/{total} 行，可从第 {} 行继续加工）",
                    confirmed_completed + 1
                )
            } else if confirmed_acked > 0 && !params.is_offline_sd {
                format!(
                    "用户急停/取消：已传输 {confirmed_acked}/{total} 行，但规划器/状态未能安全确认已完成行数，禁止盲目断点续雕，请重新发起确认发送"
                )
            } else if params.is_offline_sd {
                "SD 脱机任务已取消，请到设备旁确认".into()
            } else {
                "用户取消：已执行 ! → ? → 0x18 安全停机序列".into()
            };
            (
                "cancelled",
                serde_json::json!({
                    "success": false,
                    "running": false,
                    "cancelled": true,
                    "can_resume": can_resume,
                    "resumable_from_line": if can_resume { confirmed_completed } else { 0 },
                    "resume_boundary_source": if can_resume { "device_hold_ln" } else { "unproven" },
                    "mode": sent_mode,
                    "target": target,
                    "lines_sent": confirmed_acked,
                    "lines_completed": confirmed_completed,
                    "total_lines": total,
                    "elapsed_ms": elapsed_ms,
                    "error": error_msg,
                }),
            )
        }
        Ok(_) => (
            "completed",
            serde_json::json!({
                "success": true,
                "running": false,
                "phase": "completed",
                "mode": sent_mode,
                "target": target,
                "lines_sent": confirmed_acked,
                "lines_completed": confirmed_completed,
                "total_lines": total,
                "elapsed_ms": elapsed_ms,
            }),
        ),
        Err(error) => {
            let fallback_estop = send_emergency_stop_transport(params.job.transport.as_ref());
            let diagnostic = laser_device::diagnose_device_message(&error)
                .map(|d| format!(" ({d})"))
                .unwrap_or_default();
            let fallback_note = match fallback_estop {
                Ok(()) => "",
                Err(ref estop_err) => {
                    eprintln!(
                        "[laser-api] workflow {} 急停 0x18 兜底失败: {estop_err}",
                        params.workflow_id
                    );
                    "；且急停 0x18 兜底发送失败，请立刻按物理急停"
                }
            };
            let error_msg = format!("{error}{diagnostic}{fallback_note}");
            (
                "failed",
                serde_json::json!({
                    "success": false,
                    "running": false,
                    "cancelled": false,
                    "can_resume": false,
                    "resumable_from_line": 0,
                    "resume_boundary_source": "failed",
                    "mode": sent_mode,
                    "target": target,
                    "lines_sent": confirmed_acked,
                    "lines_completed": confirmed_completed,
                    "total_lines": total,
                    "elapsed_ms": elapsed_ms,
                    "error": error_msg,
                }),
            )
        }
    };
    if let (Some((mx, my, mz)), Some(map)) = (params.origin_mpos, send_result.as_object_mut())
        && status != "completed"
    {
        map.insert("origin_mpos".into(), serde_json::json!([mx, my, mz]));
    }
    if let (Some(raw), Some(map)) = (last_status_report, send_result.as_object_mut()) {
        map.insert("last_status_report".into(), serde_json::json!(raw));
    }
    if status == "completed" {
        let note = crate::time_calibration::record_job_timing(
            params.runtime_path.as_deref(),
            target,
            &crate::time_calibration::JobTiming {
                status,
                resumed: params.start_from_line > 0,
                sd_offline: params.is_offline_sd,
                actual_seconds: elapsed_ms as f64 / 1000.0,
                estimated_seconds: params.raw_estimate,
            },
        );
        if let Some(map) = send_result.as_object_mut() {
            map.insert("time_calibration".into(), note);
        }
    }
    (status, send_result)
}

fn finish_active_send_job(params: SendWorkerParams, status: &str) {
    if status == "completed" {
        if let Ok(mut jobs) = params.active_jobs.lock() {
            jobs.remove(&params.workflow_id);
        }
        drop(params.job);
    } else {
        // cancelled/failed 都不能证明激光已物理停止；在同锁对内原子完成 active -> uncertain 迁移。
        let uncertain_entry = UncertainJob::from_active(&params.job, Some(params.spec));
        let replaced_uncertain = {
            let mut jobs = params
                .active_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut uncertain = params
                .uncertain_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            jobs.remove(&params.workflow_id);
            let mut old_entries = Vec::new();
            uncertain.retain(|_, old| {
                if old.target == uncertain_entry.target {
                    old_entries.push(old.clone());
                    false
                } else {
                    true
                }
            });
            uncertain.insert(params.workflow_id.clone(), uncertain_entry);
            old_entries
        };
        drop(replaced_uncertain);
        drop(params.job);
    }
}

/// 加工进度记录线程守卫：离开作用域或显式 drop 时立刻唤醒并等待线程退出，
/// 避免异常退出漏线程，也保证 `job_progress` 绝不会排在 `job_end` 之后。
pub(crate) struct ProgressTraceGuard {
    stop: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ProgressTraceGuard {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.stop;
        {
            let mut done = lock.lock().unwrap_or_else(|e| e.into_inner());
            *done = true;
            cvar.notify_one();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// 加工进度每过 1% 记一步（只在完整过程记录打开时起这个线程；n110 由 10% 改细）。
fn spawn_progress_trace(params: &SendWorkerParams, total: usize) -> Option<ProgressTraceGuard> {
    let trace = params.process_trace.clone().filter(|t| t.enabled())?;
    spawn_progress_trace_with(
        trace,
        params.workflow_id.clone(),
        params.progress.clone(),
        total,
        params.start_from_line,
        std::time::Duration::from_millis(500),
    )
}

pub(crate) fn spawn_progress_trace_with(
    trace: laser_runtime::ProcessTrace,
    workflow_id: String,
    progress: std::sync::Arc<laser_device::JobProgress>,
    total: usize,
    start_from: usize,
    interval: std::time::Duration,
) -> Option<ProgressTraceGuard> {
    if total == 0 {
        return None;
    }
    let stop = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let stop_worker = stop.clone();
    let handle = std::thread::Builder::new()
        .name(format!("laser-trace-{workflow_id}"))
        .spawn(move || {
            use std::sync::atomic::Ordering;
            let began = std::time::Instant::now();
            // 续雕任务从已完成的档位起算，只记后续前进的档位；每 1% 记一步（n110 由 10% 改细）
            let mut reported = (start_from.min(total) * 100 / total).min(99);
            let (lock, cvar) = &*stop_worker;
            loop {
                {
                    let done = lock.lock().unwrap_or_else(|e| e.into_inner());
                    if *done {
                        break;
                    }
                    let (done, _) = cvar
                        .wait_timeout(done, interval)
                        .unwrap_or_else(|e| e.into_inner());
                    if *done {
                        break;
                    }
                }
                // 加工过程中只有 lines_sent 实时增长（lines_completed 在收尾/取消时才写回），
                // 两者取大值按总行数换算 1%~99% 档位（100% 由结束那一步记）。
                let sent = progress.lines_sent.load(Ordering::Relaxed);
                let completed = progress.lines_completed.load(Ordering::Relaxed);
                let current = sent.max(completed).min(total);
                let percent = (current * 100 / total).min(99);
                if percent > reported {
                    reported = percent;
                    crate::process_trace::trace_job_step(
                        Some(&trace),
                        &workflow_id,
                        "job_progress",
                        &format!(
                            "加工进度 {percent}%：已发送 {sent} / {total} 行（用时 {} 秒）",
                            began.elapsed().as_secs()
                        ),
                        serde_json::json!({
                            "percent": percent,
                            "lines_completed": completed,
                            "lines_sent": sent,
                            "total_lines": total,
                            "start_from_line": start_from,
                            "elapsed_ms": began.elapsed().as_millis() as u64,
                            "status_report": progress.status_report(),
                        }),
                    );
                }
            }
        })
        .ok()?;
    Some(ProgressTraceGuard {
        stop,
        handle: Some(handle),
    })
}

fn job_end_step(status: &str, send_result: &serde_json::Value) -> String {
    let lines = format!(
        "{} / {} 行",
        send_result["lines_completed"].as_u64().unwrap_or(0),
        send_result["total_lines"].as_u64().unwrap_or(0)
    );
    let seconds = send_result["elapsed_ms"].as_u64().unwrap_or(0) / 1000;
    if send_result["phase"].as_str() == Some("result_unknown") {
        return format!(
            "加工结果未知（停机状态待确认）：已发送 {} / {} 行，{}",
            send_result["lines_sent"].as_u64().unwrap_or(0),
            send_result["total_lines"].as_u64().unwrap_or(0),
            send_result["error"].as_str().unwrap_or("请核实设备状态")
        );
    }
    match status {
        "completed" => format!("加工完成：{lines}，用时 {seconds} 秒"),
        "cancelled" => format!("加工已取消：完成 {lines}，用时 {seconds} 秒"),
        _ => format!(
            "加工失败：完成 {lines}，{}",
            send_result["error"].as_str().unwrap_or("原因未知")
        ),
    }
}

fn run_send_worker(mut device: SharedDeviceConnection, mut params: SendWorkerParams) {
    use std::sync::atomic::Ordering;
    let total = params.lines.len();
    let sent_mode = if params.is_offline_sd {
        "sd_offline"
    } else {
        params.mode
    };
    crate::process_trace::trace_job_step(
        params.process_trace.as_ref(),
        &params.workflow_id,
        "job_start",
        &format!(
            "开始加工：{} 行 G-code（{}，目标 {}{}）",
            total,
            if params.is_offline_sd {
                "脱机 SD 卡"
            } else {
                "逐行下发"
            },
            params.target,
            if params.start_from_line > 0 {
                format!("，从第 {} 行续雕", params.start_from_line + 1)
            } else {
                String::new()
            }
        ),
        serde_json::json!({
            "mode": sent_mode,
            "target": params.target,
            "total_lines": total,
            "start_from_line": params.start_from_line,
            "estimated_seconds": params.raw_estimate,
            "scale_factor": params.scale_factor,
        }),
    );
    let progress_trace = spawn_progress_trace(&params, total);
    let outcome = if params
        .voice_permit
        .as_ref()
        .is_some_and(|permit| !permit.load(Ordering::Acquire))
    {
        Err("语音授权已撤销，排队中的加工未执行".to_owned())
    } else if params.is_offline_sd && params.cancel.is_cancelled() {
        Ok(0)
    } else if params.is_offline_sd {
        let result = execute_sd_offline_job(&mut device, &params, total);
        drop(device);
        result
    } else {
        let lines = std::mem::take(&mut params.lines);
        execute_streaming_job(device, &params, lines)
    };

    let (status, send_result) = build_terminal_send_outcome(&params, sent_mode, outcome, total);
    // 立即唤醒并回收进度线程（微秒级），保证 job_progress 不会晚到排在 job_end 之后
    drop(progress_trace);
    crate::process_trace::trace_job_step(
        params.process_trace.as_ref(),
        &params.workflow_id,
        "job_end",
        &job_end_step(status, &send_result),
        serde_json::json!({"status": status, "send_result": send_result}),
    );
    if let Some(hook) = params.on_terminal.take() {
        hook(status, &send_result);
    }
    emit_laser_event(
        &params.event_sink,
        job_terminal_event(&params.workflow_id, status, &send_result),
    );
    finalize_workflow_record(
        params.store.as_deref(),
        &params.workflows,
        &params.calibrations,
        &params.event_sink,
        params.kind,
        &params.workflow_id,
        status,
        send_result,
    );
    finish_active_send_job(params, status);
}

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
        if self.is_shutdown_requested() {
            return Err((
                503,
                failure(
                    "服务正在关闭，已拒绝新的加工发送请求",
                    Some(serde_json::json!({"error_code": "host_shutting_down"})),
                ),
            ));
        }
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
        let result = (|| match &authority {
            approval::R3Authority::Approval(record) => match record.connection.as_ref() {
                Some(connection) => {
                    let effective = approval::object_with_connection(object, connection);
                    // 先记“发送中”，写库失败时立即拒发，杜绝未落库盲发（#6）
                    let service = match self.approvals.clone() {
                        Some(s) => s,
                        None => {
                            return Err(approval::error_response(
                                &approval::ApprovalError::Unavailable("审批服务不可用".into()),
                            ));
                        }
                    };
                    if let Err(error) = service.mark_running(&record.approval_id, workflow_id) {
                        return Err(approval::error_response(&error));
                    }
                    let approval_id = record.approval_id.clone();
                    let hook: Option<JobTerminalHook> = Some(Box::new(
                        move |status: &str, send_result: &serde_json::Value| {
                            service.record_job_result(&approval_id, status, send_result);
                        },
                    )
                        as JobTerminalHook);
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
        })();
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

    fn validate_send_launch_plan(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<SendLaunchPlan, (u16, serde_json::Value)> {
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
        let start_from_line = object
            .get("start_from_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        Ok(SendLaunchPlan {
            spec,
            mode,
            target,
            is_offline_sd,
            sd_http_port,
            start_from_line,
        })
    }

    fn reserve_send_launch(
        &self,
        workflow_id: &str,
        target: &str,
    ) -> Result<
        (
            laser_device::CancellationToken,
            Option<LaunchReservationGuard>,
        ),
        (u16, serde_json::Value),
    > {
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
            Ok((existing, None))
        } else {
            let token = laser_device::CancellationToken::default();
            reservations
                .active
                .insert(workflow_id.to_owned(), token.clone());
            drop(reservations);
            drop(jobs);
            Ok((
                token,
                Some(LaunchReservationGuard {
                    reservations: self.launching_workflows.clone(),
                    workflow_id: workflow_id.to_owned(),
                }),
            ))
        }
    }

    fn open_and_register_send_job(
        &self,
        workflow_id: &str,
        plan: &SendLaunchPlan,
        initial_line_count: usize,
        cancel: &laser_device::CancellationToken,
    ) -> Result<
        (
            std::sync::Arc<dyn laser_device::DeviceTransport>,
            std::sync::Arc<laser_device::JobProgress>,
            std::sync::Arc<ActiveJob>,
            SendPreflightGuard,
        ),
        (u16, serde_json::Value),
    > {
        use std::sync::atomic::Ordering;
        // 按设备目标串行化“复查活动任务 -> 释放旧待确认停止句柄 -> 打开独占新句柄 -> 登记 ActiveJob”，
        // 杜绝同设备并发打开双句柄（串口 DTR 复位 / Telnet 顶号），统一锁序 target_open_mutex -> active_jobs -> uncertain_jobs。
        let open_mutex = target_open_mutex(&self.target_open_locks, &plan.target);
        let _open_guard = open_mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_shutdown_requested() {
            return Err((
                503,
                failure(
                    "服务正在关闭，已拒绝建立新的设备连接",
                    Some(serde_json::json!({"error_code": "host_shutting_down"})),
                ),
            ));
        }
        if cancel.is_cancelled() {
            return Err((
                409,
                failure(
                    "预检前任务已被取消，未下发刀路",
                    Some(serde_json::json!({"error_code": "workflow_cancelled"})),
                ),
            ));
        }
        {
            let jobs = self
                .active_jobs
                .lock()
                .map_err(|_| (500, failure("活动任务注册表不可用", None)))?;
            if let Some(busy) = jobs
                .get(workflow_id)
                .or_else(|| jobs.values().find(|j| j.target == plan.target))
            {
                return Err((
                    409,
                    failure(
                        format!(
                            "设备 {} 正在执行 workflow {}，请等待完成或先取消",
                            plan.target, busy.workflow_id
                        ),
                        Some(serde_json::json!({"error_code": "device_busy"})),
                    ),
                ));
            }
        }
        release_uncertain_target_handle(&self.uncertain_jobs, &plan.target);
        if self
            .short_io_targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&plan.target)
        {
            return Err((
                409,
                failure(
                    format!(
                        "设备 {} 正在执行连接探测或状态查询，请稍后重试",
                        plan.target
                    ),
                    Some(serde_json::json!({"error_code": "device_busy"})),
                ),
            ));
        }
        let opened = self.open_device_transport(&plan.spec).map_err(|e| {
            let status = if self.is_shutdown_requested() {
                503
            } else {
                400
            };
            let code = if self.is_shutdown_requested() {
                "host_shutting_down"
            } else {
                "device_unreachable"
            };
            (
                status,
                failure(
                    format!("设备连接失败 ({}): {e}", plan.target),
                    Some(serde_json::json!({"error_code": code})),
                ),
            )
        })?;
        let transport: std::sync::Arc<dyn laser_device::DeviceTransport> =
            std::sync::Arc::from(opened);
        // 开连接后立刻登记 ActiveJob，覆盖握手 / $CD / $X / 二次哈希复核全预检窗口，使急停与退出可实时中止
        let progress = std::sync::Arc::new(laser_device::JobProgress::default());
        progress
            .total_lines
            .store(initial_line_count, Ordering::Relaxed);
        progress
            .lines_sent
            .store(plan.start_from_line, Ordering::Relaxed);
        let job = std::sync::Arc::new(ActiveJob {
            workflow_id: workflow_id.to_owned(),
            mode: if plan.is_offline_sd {
                "sd_offline"
            } else {
                plan.mode
            },
            target: plan.target.clone(),
            cancel: cancel.clone(),
            progress: progress.clone(),
            transport: transport.clone(),
            started_at: std::time::Instant::now(),
            spec: Some(plan.spec.clone()),
        });
        {
            let mut jobs = self
                .active_jobs
                .lock()
                .map_err(|_| (500, failure("活动任务注册表不可用", None)))?;
            if self.is_shutdown_requested() {
                return Err((
                    503,
                    failure(
                        "服务正在关闭，已中止发码预检",
                        Some(serde_json::json!({"error_code": "host_shutting_down"})),
                    ),
                ));
            }
            if cancel.is_cancelled() {
                return Err((
                    409,
                    failure(
                        "预检前任务已被取消，未下发刀路",
                        Some(serde_json::json!({"error_code": "workflow_cancelled"})),
                    ),
                ));
            }
            if let Some(busy) = jobs
                .get(workflow_id)
                .or_else(|| jobs.values().find(|j| j.target == plan.target))
            {
                return Err((
                    409,
                    failure(
                        format!(
                            "设备 {} 正在执行 workflow {}，请等待完成或先取消",
                            plan.target, busy.workflow_id
                        ),
                        Some(serde_json::json!({"error_code": "device_busy"})),
                    ),
                ));
            }
            jobs.insert(workflow_id.to_owned(), job.clone());
        }
        drop(_open_guard);
        let preflight_guard = SendPreflightGuard {
            active_jobs: self.active_jobs.clone(),
            uncertain_jobs: self.uncertain_jobs.clone(),
            job: job.clone(),
            spec: plan.spec.clone(),
            armed: true,
        };
        Ok((transport, progress, job, preflight_guard))
    }

    fn run_send_preflight(
        device: &mut SharedDeviceConnection,
        gcode_path: &std::path::Path,
        expected_sha: &str,
        object: &serde_json::Map<String, serde_json::Value>,
        plan: &SendLaunchPlan,
        cancel: &laser_device::CancellationToken,
        progress: &laser_device::JobProgress,
    ) -> Result<SendPreflightOutcome, (u16, serde_json::Value)> {
        use std::sync::atomic::Ordering;
        device
            .handshake_with_deadline_cancellable(std::time::Duration::from_secs(3), Some(cancel))
            .map_err(|e| {
                let status = if cancel.is_cancelled() { 409 } else { 400 };
                let code = if cancel.is_cancelled() {
                    "workflow_cancelled"
                } else {
                    "device_not_ready"
                };
                (
                    status,
                    failure(
                        format!("设备握手未通过 ({}): {e}", plan.target),
                        Some(serde_json::json!({"error_code": code})),
                    ),
                )
            })?;

        // FluidNC：读取 $CD 配置以感知自动宏 / 急停引脚 / planner；仅旧 GRBL error:3 静默跳过，复位/报警/断线一律中止预检
        if let Err(e) = device.load_features(std::time::Duration::from_secs(3)) {
            let status = if cancel.is_cancelled() { 409 } else { 400 };
            let code = if cancel.is_cancelled() {
                "workflow_cancelled"
            } else {
                "device_not_ready"
            };
            return Err((
                status,
                failure(
                    format!("设备 $CD 配置读取异常 ({}): {e}", plan.target),
                    Some(serde_json::json!({"error_code": code})),
                ),
            ));
        }
        // 先于 $X 解锁执行二次物理 SHA-256 与行程复核，杜绝建连期间 TOCTOU 篡改刀路却提前触发 $X 移动（#4）
        let verified = read_and_verify_gcode(gcode_path, expected_sha, plan.start_from_line)?;
        progress
            .total_lines
            .store(verified.lines.len(), Ordering::Relaxed);
        if let Err(error) = check_job_fits_machine(&verified.text, &device.travel_limits) {
            return Err((
                400,
                failure(
                    format!("{error}（{}）", plan.target),
                    Some(serde_json::json!({"error_code": "job_exceeds_travel"})),
                ),
            ));
        }

        let preflight_res = if plan.start_from_line > 0 {
            device.verify_job_ready_for_resume_cancellable(
                std::time::Duration::from_secs(3),
                Some(cancel),
            )
        } else {
            device.verify_job_ready_adaptive_cancellable(
                std::time::Duration::from_secs(3),
                Some(cancel),
            )
        };
        let device_scale = preflight_res.map_err(|e| {
            let status = if cancel.is_cancelled() { 409 } else { 400 };
            let code = if cancel.is_cancelled() {
                "workflow_cancelled"
            } else {
                "device_preflight_failed"
            };
            (
                status,
                failure(
                    format!("加工前检查未通过 ({}): {e}", plan.target),
                    Some(serde_json::json!({"error_code": code})),
                ),
            )
        })?;
        let current_mpos = device.last_mpos;
        let origin_mpos = if plan.start_from_line == 0 {
            current_mpos
        } else {
            let recorded_origin = parse_mpos_value(object.get("_origin_mpos"));
            if recorded_origin.is_some() && current_mpos.is_none() {
                return Err((
                    400,
                    failure(
                        "原任务锁定了开工坐标 MPos，但当前设备未上报 MPos（请检查 $10 状态掩码），禁止盲目续雕",
                        Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                    ),
                ));
            }
            let plan_preview = build_resume_plan(
                &verified.lines[..plan.start_from_line],
                recorded_origin,
                current_mpos,
            );
            if plan_preview.requires_physical_origin
                && (recorded_origin.is_none() || current_mpos.is_none())
            {
                return Err((
                    400,
                    failure(
                        "续雕断点前包含 G92 偏移或 G91 增量定位，但缺少原任务开工 MPos 或当前 MPos，禁止盲目续雕",
                        Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                    ),
                ));
            }
            recorded_origin
        };
        Ok(SendPreflightOutcome {
            verified,
            scale_factor: device_scale / 1000.0,
            origin_mpos,
            current_mpos,
        })
    }

    fn clear_uncertain_for_launch(
        &self,
        target: &str,
        cancel: &laser_device::CancellationToken,
    ) -> Result<Vec<std::sync::Arc<UncertainJob>>, (u16, serde_json::Value)> {
        let jobs = self
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
        drop(jobs);
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
        Ok(dropped)
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
        use std::sync::atomic::Ordering;
        let plan = self.validate_send_launch_plan(object)?;
        let (cancel, _owned_reservation) = self.reserve_send_launch(workflow_id, &plan.target)?;
        if self.is_shutdown_requested() {
            return Err((
                503,
                failure(
                    "服务正在关闭，已拒绝建立新的发送任务",
                    Some(serde_json::json!({"error_code": "host_shutting_down"})),
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
        if plan.start_from_line > 0 && plan.is_offline_sd {
            return Err((
                400,
                failure(
                    "SD 脱机作业由控制器整文件执行，不支持按行断点续雕，请重新发起确认发送",
                    Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                ),
            ));
        }
        // 开连接前先做一轮 G-code 读取、SHA-256 校验与行准备，避免不合格/已篡改任务触发设备打开与解锁
        let initial_verified =
            read_and_verify_gcode(&gcode_path, expected_sha, plan.start_from_line)?;
        let (transport, progress, job, mut preflight_guard) = self.open_and_register_send_job(
            workflow_id,
            &plan,
            initial_verified.lines.len(),
            &cancel,
        )?;
        let mut device = laser_device::DeviceConnection::new(transport.clone());
        let preflight = Self::run_send_preflight(
            &mut device,
            &gcode_path,
            expected_sha,
            object,
            &plan,
            &cancel,
            &progress,
        )?;
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
        if self.is_shutdown_requested() {
            return Err((
                503,
                failure(
                    "服务正在关闭，已终止启动",
                    Some(serde_json::json!({"error_code": "host_shutting_down"})),
                ),
            ));
        }
        // SD 脱机文件与流式发送使用同一功率标尺：设备 $30 不是 1000 时，上传按标尺换算后的已校验行，
        // 并以实际上传字节的 SHA-256 做固件侧哈希门禁（原文件 SHA 已在上面与确认快照比对）。
        let (sd_upload_bytes, sd_upload_sha) = sd_offline_upload_payload(
            preflight.verified.bytes,
            &preflight.verified.sha256,
            &preflight.verified.lines,
            preflight.scale_factor,
        );
        let cleared_uncertain = self.clear_uncertain_for_launch(&plan.target, &cancel)?;
        preflight_guard.armed = false;
        drop(cleared_uncertain);
        drop(transport);
        // 工时自校准（r40）：预览时的原始估算，作业正常完成后与实际用时比较（只读持久化存储，不碰内存表）
        let raw_estimate = self
            .store
            .as_ref()
            .and_then(|store| store.get(kind, workflow_id).ok().flatten())
            .and_then(|record| crate::time_calibration::record_raw_estimate(&record.payload));
        let total = preflight.verified.lines.len();
        let worker_params = SendWorkerParams {
            kind,
            workflow_id: workflow_id.to_owned(),
            mode: plan.mode,
            target: plan.target.clone(),
            spec: plan.spec.clone(),
            is_offline_sd: plan.is_offline_sd,
            sd_http_port: plan.sd_http_port,
            sd_upload_bytes,
            sd_upload_sha,
            lines: preflight.verified.lines,
            scale_factor: preflight.scale_factor,
            start_from_line: plan.start_from_line,
            origin_mpos: preflight.origin_mpos,
            current_mpos: preflight.current_mpos,
            raw_estimate,
            runtime_path: self.runtime_path.clone(),
            object: object.clone(),
            cancel,
            progress,
            job,
            voice_permit,
            on_terminal,
            store: self.store.clone(),
            process_trace: self.process_trace().cloned(),
            workflows: self.workflows.clone(),
            calibrations: self.calibrations.clone(),
            active_jobs: self.active_jobs.clone(),
            uncertain_jobs: self.uncertain_jobs.clone(),
            shutdown_requested: self.shutdown_requested.clone(),
            event_sink: self.event_sink.clone(),
        };
        std::thread::Builder::new()
            .name(format!("laser-send-{workflow_id}"))
            .spawn(move || run_send_worker(device, worker_params))
            .map_err(|e| {
                if let Ok(mut jobs) = self.active_jobs.lock() {
                    jobs.remove(workflow_id);
                }
                (500, failure(format!("无法启动发送线程: {e}"), None))
            })?;
        Ok(serde_json::json!({
            "success": true,
            "running": true,
            "mode": plan.mode,
            "target": plan.target,
            "lines_sent": plan.start_from_line,
            "total_lines": total,
            "resumed": plan.start_from_line > 0,
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
