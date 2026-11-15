use super::*;

/// 作业结束回调：发送线程在写回终态前调用（状态 `completed` / `cancelled` / `failed`，以及 `send_result`）。
pub type JobTerminalHook = Box<dyn FnOnce(&str, &serde_json::Value) + Send + 'static>;

/// 传输工厂：默认按连接描述真实打开 TCP/串口；测试可注入 FakeGrbl。
pub type TransportFactory = std::sync::Arc<
    dyn Fn(&laser_device::ConnectionSpec) -> Result<Box<dyn laser_device::DeviceTransport>, String>
        + Send
        + Sync,
>;

/// 正在向设备下发的作业：注册表持有取消令牌、进度计数与传输句柄，供轮询/取消/急停使用。
pub struct ActiveJob {
    pub workflow_id: String,
    pub mode: &'static str,
    pub target: String,
    pub cancel: laser_device::CancellationToken,
    pub progress: std::sync::Arc<laser_device::JobProgress>,
    pub transport: std::sync::Arc<dyn laser_device::DeviceTransport>,
    pub started_at: std::time::Instant,
}

impl ActiveJob {
    pub fn live_send_result(&self) -> serde_json::Value {
        use std::sync::atomic::Ordering;
        let mut val = serde_json::json!({
            "success": true,
            "running": true,
            "phase": if self.progress.awaiting_completion.load(Ordering::Acquire) { "awaiting_completion" } else { "streaming" },
            "mode": self.mode,
            "target": self.target,
            "lines_sent": self.progress.lines_sent.load(Ordering::Relaxed),
            "total_lines": self.progress.total_lines.load(Ordering::Relaxed),
            "elapsed_ms": self.started_at.elapsed().as_millis() as u64,
        });
        if let (Some(raw), Some(map)) = (self.progress.status_report(), val.as_object_mut()) {
            map.insert("last_status_report".into(), serde_json::json!(raw));
        }
        val
    }
}

pub type ActiveJobs = std::sync::Arc<std::sync::Mutex<HashMap<String, std::sync::Arc<ActiveJob>>>>;

/// 已退出活动发送但尚未确认物理停稳的任务记录：允许为重连/探测先行释放底层串口句柄，
/// 同时持续保留 `job_id`、`target` 与 `spec`，直到新作业通过 Idle 预检启动或操作者显式确认停稳。
pub struct UncertainJob {
    pub workflow_id: String,
    pub mode: &'static str,
    pub target: String,
    pub spec: Option<laser_device::ConnectionSpec>,
    pub cancel: laser_device::CancellationToken,
    pub transport: std::sync::Mutex<Option<std::sync::Arc<dyn laser_device::DeviceTransport>>>,
}

impl UncertainJob {
    pub fn from_active(
        job: &ActiveJob,
        spec: Option<laser_device::ConnectionSpec>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            workflow_id: job.workflow_id.clone(),
            mode: job.mode,
            target: job.target.clone(),
            spec,
            cancel: job.cancel.clone(),
            transport: std::sync::Mutex::new(Some(job.transport.clone())),
        })
    }

    pub fn release_handle(&self) -> Option<std::sync::Arc<dyn laser_device::DeviceTransport>> {
        self.transport
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub fn write_emergency_stop(
        &self,
        factory: &TransportFactory,
        open_locks: &TargetOpenLocks,
    ) -> Result<(), String> {
        {
            let guard = self
                .transport
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(transport) = guard.as_ref() {
                return transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
            }
        }
        let Some(spec) = self.spec.clone() else {
            return Err("原设备句柄已释放且无重连参数，请使用实体急停".into());
        };
        let open_mutex = target_open_mutex(open_locks, &self.target);
        let _open_guard = open_mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut guard = self
            .transport
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(transport) = guard.as_ref() {
            return transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
        }
        let reopened: std::sync::Arc<dyn laser_device::DeviceTransport> =
            std::sync::Arc::from((factory)(&spec)?);
        let res = reopened.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
        if res.is_ok() {
            *guard = Some(reopened);
        }
        res
    }

    pub fn abort_with_factory(
        &self,
        factory: &TransportFactory,
        open_locks: &TargetOpenLocks,
    ) -> Result<(), String> {
        self.cancel.cancel();
        {
            let guard = self
                .transport
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(transport) = guard.as_ref() {
                let res = transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
                if let Some(raw) = transport.raw_abort_handle() {
                    let _ = raw();
                }
                return res;
            }
        }
        let Some(spec) = self.spec.clone() else {
            return Err("原设备句柄已释放且无重连参数，无法向未知状态设备发送退出急停".into());
        };
        let open_mutex = target_open_mutex(open_locks, &self.target);
        let _open_guard = open_mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut guard = self
            .transport
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(transport) = guard.as_ref() {
            let res = transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
            if let Some(raw) = transport.raw_abort_handle() {
                let _ = raw();
            }
            return res;
        }
        let reopened: std::sync::Arc<dyn laser_device::DeviceTransport> =
            std::sync::Arc::from((factory)(&spec)?);
        let res = reopened.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
        if let Some(raw) = reopened.raw_abort_handle() {
            let _ = raw();
        }
        if res.is_ok() {
            *guard = Some(reopened);
        }
        res
    }
}

pub type UncertainJobs =
    std::sync::Arc<std::sync::Mutex<HashMap<String, std::sync::Arc<UncertainJob>>>>;
pub(crate) type TargetOpenLocks =
    std::sync::Arc<std::sync::Mutex<HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>>;

#[derive(Default)]
pub(crate) struct LaunchReservationState {
    pub(crate) active: HashMap<String, laser_device::CancellationToken>,
    cancel_epochs: HashMap<String, u64>,
}

pub(crate) type LaunchReservations = std::sync::Arc<std::sync::Mutex<LaunchReservationState>>;

pub(crate) enum ReserveLaunchError {
    Busy,
    Cancelled,
}

pub(crate) struct LaunchReservationGuard {
    pub(crate) reservations: LaunchReservations,
    pub(crate) workflow_id: String,
}

impl Drop for LaunchReservationGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.reservations.lock() {
            state.active.remove(&self.workflow_id);
        }
    }
}

pub(crate) fn current_cancel_epoch(reservations: &LaunchReservations, workflow_id: &str) -> u64 {
    reservations
        .lock()
        .ok()
        .and_then(|s| s.cancel_epochs.get(workflow_id).copied())
        .unwrap_or(0)
}

pub(crate) fn reserve_workflow_launch(
    jobs: &ActiveJobs,
    reservations: &LaunchReservations,
    workflow_id: &str,
    expected_cancel_epoch: Option<u64>,
) -> Result<LaunchReservationGuard, ReserveLaunchError> {
    let jobs_guard = jobs.lock().map_err(|_| ReserveLaunchError::Busy)?;
    if jobs_guard.contains_key(workflow_id) {
        return Err(ReserveLaunchError::Busy);
    }
    let mut state = reservations.lock().map_err(|_| ReserveLaunchError::Busy)?;
    if let Some(expected) = expected_cancel_epoch {
        let actual = state.cancel_epochs.get(workflow_id).copied().unwrap_or(0);
        if actual != expected {
            return Err(ReserveLaunchError::Cancelled);
        }
    }
    if state.active.contains_key(workflow_id) {
        return Err(ReserveLaunchError::Busy);
    }
    let token = laser_device::CancellationToken::default();
    state.active.insert(workflow_id.to_owned(), token);
    drop(state);
    drop(jobs_guard);
    Ok(LaunchReservationGuard {
        reservations: reservations.clone(),
        workflow_id: workflow_id.to_owned(),
    })
}

pub(crate) fn target_open_mutex(
    locks: &TargetOpenLocks,
    target: &str,
) -> std::sync::Arc<std::sync::Mutex<()>> {
    let mut map = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.entry(target.to_owned())
        .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
        .clone()
}

pub(crate) fn default_transport_factory() -> TransportFactory {
    std::sync::Arc::new(|spec: &laser_device::ConnectionSpec| {
        laser_device::open_transport(spec, std::time::Duration::from_secs(3))
    })
}

impl ApiService {
    /// 当前正在下发的作业数量。
    pub fn active_job_count(&self) -> usize {
        self.active_jobs.lock().map(|jobs| jobs.len()).unwrap_or(0)
    }

    /// 是否有设备作业在进行：正在下发、正在启动或停机后待操作者确认的任务都算（锁中毒时按“忙”处理）。
    /// 应用内更新（2026-11-07）据此拒绝在加工途中重启 MOSS。
    pub fn device_work_in_progress(&self) -> bool {
        let busy = |held: Option<bool>| held.unwrap_or(true);
        busy(self.active_jobs.lock().ok().map(|jobs| !jobs.is_empty()))
            || busy(self.uncertain_jobs.lock().ok().map(|jobs| !jobs.is_empty()))
            || busy(
                self.launching_workflows
                    .lock()
                    .ok()
                    .map(|state| !state.active.is_empty()),
            )
    }

    /// 退出/崩溃兜底：对所有活动作业置取消并直接写 0x18（实时信道 + 裸句柄双路）。
    pub fn abort_all_active_jobs(&self) -> Result<(), String> {
        let (jobs, uncertain) = {
            let jobs_guard = self
                .active_jobs
                .lock()
                .map_err(|_| "active_jobs lock poisoned".to_owned())?;
            let uncertain_guard = self
                .uncertain_jobs
                .lock()
                .map_err(|_| "uncertain_jobs lock poisoned".to_owned())?;
            if let Ok(launching) = self.launching_workflows.lock() {
                for token in launching.active.values() {
                    token.cancel();
                }
            }
            (
                jobs_guard.values().cloned().collect::<Vec<_>>(),
                uncertain_guard.values().cloned().collect::<Vec<_>>(),
            )
        };
        let mut first_error = None;
        for job in jobs {
            job.cancel.cancel();
            if let Err(e) = job
                .transport
                .write_realtime(laser_device::RealtimeCommand::EmergencyStop)
            {
                first_error.get_or_insert(e);
            }
            if let Some(raw) = job.transport.raw_abort_handle() {
                let _ = raw();
            }
        }
        for job in uncertain {
            if let Err(e) = job.abort_with_factory(&self.transport_factory, &self.target_open_locks)
            {
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

pub(crate) fn release_uncertain_target_handle(uncertain_jobs: &UncertainJobs, target: &str) {
    let handles: Vec<std::sync::Arc<dyn laser_device::DeviceTransport>> =
        match uncertain_jobs.lock() {
            Ok(uncertain) => uncertain
                .values()
                .filter(|old| old.target == target)
                .filter_map(|old| old.release_handle())
                .collect(),
            Err(_) => Vec::new(),
        };
    drop(handles);
}

pub(crate) fn overlay_live_progress(
    payload: &mut serde_json::Value,
    jobs: &ActiveJobs,
    workflow_id: &str,
) {
    if let Ok(jobs) = jobs.lock()
        && let Some(job) = jobs.get(workflow_id)
        && let Some(map) = payload.as_object_mut()
    {
        map.insert("send_result".into(), job.live_send_result());
        map.insert("status".into(), serde_json::Value::String("sending".into()));
    }
}

/// 仅在内存中推进取消世代并置位取消令牌（零硬件/磁盘 I/O，可安全在 `record_transition_guard` 内执行）。
pub(crate) fn mark_job_cancelled_in_memory(
    jobs: &ActiveJobs,
    reservations: &LaunchReservations,
    workflow_id: &str,
) -> (bool, Option<std::sync::Arc<ActiveJob>>) {
    let job = match jobs.lock() {
        Ok(guard) => guard.get(workflow_id).cloned(),
        Err(_) => None,
    };
    let mut signalled = false;
    if let Ok(mut state) = reservations.lock() {
        let next = state
            .cancel_epochs
            .get(workflow_id)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        state.cancel_epochs.insert(workflow_id.to_owned(), next);
        if let Some(token) = state.active.get(workflow_id) {
            token.cancel();
            signalled = true;
        }
    }
    if let Some(ref job) = job {
        job.cancel.cancel();
        signalled = true;
    }
    (signalled, job)
}

pub(crate) fn flush_cancel_feed_hold(job: Option<std::sync::Arc<ActiveJob>>) {
    if let Some(job) = job {
        let _ = job
            .transport
            .write_realtime(laser_device::RealtimeCommand::FeedHold);
    }
}
