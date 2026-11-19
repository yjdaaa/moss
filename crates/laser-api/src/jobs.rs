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
    /// 打开这次作业用的连接（走边框“继续”在严格模式下据此建审批目标，2026-11-18）；测试桩可为空
    pub spec: Option<laser_device::ConnectionSpec>,
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
            "paused": self.progress.paused.load(Ordering::Acquire),
        });
        if let (Some(raw), Some(map)) = (self.progress.status_report(), val.as_object_mut()) {
            map.insert("last_status_report".into(), serde_json::json!(raw));
        }
        val
    }
}

/// 短作业（走边框）的开始回执（2026-11-18，技术书 r52）：内置语音在后台线程里发起走边框，
/// 等到“这一次”真正开始就先回话；等不及放弃后，同一次请求不得再让机器动。
/// 经 `ApiService::dispatch_json_with_watch` 显式传入，通过 `Condvar` 立即唤醒（不再依赖 `thread_local!` 或轮询睡眠）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ShortJobStart {
    #[default]
    Waiting,
    Started(String),
    Finished,
    Abandoned,
}

#[derive(Default)]
pub struct ShortJobWatch {
    state: std::sync::Mutex<ShortJobStart>,
    notify: std::sync::Condvar,
}

impl ShortJobWatch {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    pub fn new_abandoned() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            state: std::sync::Mutex::new(ShortJobStart::Abandoned),
            notify: std::sync::Condvar::new(),
        })
    }

    pub fn state(&self) -> ShortJobStart {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn lock_state(&self) -> std::sync::MutexGuard<'_, ShortJobStart> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn notify_all(&self) {
        self.notify.notify_all();
    }

    /// 后台调用提前结束（未进入开始阶段或已直接返回/panic）：仅当仍处于 Waiting 时标记为 Finished 并唤醒等待者。
    pub fn mark_finished(&self) {
        let mut guard = self.lock_state();
        if *guard == ShortJobStart::Waiting {
            *guard = ShortJobStart::Finished;
        }
        drop(guard);
        self.notify.notify_all();
    }

    /// 阻塞等待短作业开始或调用提前结束，直到截止时刻；超时仍未开始则原子置为 Abandoned，阻止后续起动。
    pub fn wait_until_settled(&self, deadline: std::time::Instant) -> ShortJobStart {
        let mut guard = self.lock_state();
        loop {
            if *guard != ShortJobStart::Waiting {
                return guard.clone();
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                *guard = ShortJobStart::Abandoned;
                self.notify.notify_all();
                return ShortJobStart::Abandoned;
            }
            let wait_for = deadline.saturating_duration_since(now);
            let (next_guard, _) = self
                .notify
                .wait_timeout(guard, wait_for)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard = next_guard;
        }
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

/// 优先通过无锁/独立裸句柄发送 0x18（不被 `realtime_writer` 上的 `?` 心跳阻塞），
/// 无裸句柄或裸句柄失败时再回退到 `write_realtime(EmergencyStop)`。
pub(crate) fn send_emergency_stop_transport(
    transport: &dyn laser_device::DeviceTransport,
) -> Result<(), String> {
    if let Some(raw) = transport.raw_abort_handle() {
        match raw() {
            Ok(()) => return Ok(()),
            Err(raw_err) => {
                return transport
                    .write_realtime(laser_device::RealtimeCommand::EmergencyStop)
                    .map_err(|rt_err| format!("{raw_err}; {rt_err}"));
            }
        }
    }
    transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop)
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

    fn current_handle(&self) -> Option<std::sync::Arc<dyn laser_device::DeviceTransport>> {
        self.transport
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn try_current_handle(&self) -> Option<std::sync::Arc<dyn laser_device::DeviceTransport>> {
        match self.transport.try_lock() {
            Ok(guard) => guard.clone(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    pub fn write_emergency_stop(
        &self,
        factory: &TransportFactory,
        open_locks: &TargetOpenLocks,
    ) -> Result<(), String> {
        if let Some(transport) = self.current_handle() {
            return send_emergency_stop_transport(transport.as_ref());
        }
        let Some(spec) = self.spec.clone() else {
            return Err("原设备句柄已释放且无重连参数，请使用实体急停".into());
        };
        let open_mutex = target_open_mutex(open_locks, &self.target);
        let _open_guard = open_mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(transport) = self.current_handle() {
            return send_emergency_stop_transport(transport.as_ref());
        }
        let reopened: std::sync::Arc<dyn laser_device::DeviceTransport> =
            std::sync::Arc::from((factory)(&spec)?);
        let res = send_emergency_stop_transport(reopened.as_ref());
        if res.is_ok() {
            *self
                .transport
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reopened);
        }
        res
    }

    pub fn abort_with_factory(
        &self,
        factory: &TransportFactory,
        open_locks: &TargetOpenLocks,
    ) -> Result<(), String> {
        self.cancel.cancel();
        if let Some(transport) = self.current_handle() {
            return send_emergency_stop_transport(transport.as_ref());
        }
        let Some(spec) = self.spec.clone() else {
            return Err("原设备句柄已释放且无重连参数，无法向未知状态设备发送退出急停".into());
        };
        let open_mutex = target_open_mutex(open_locks, &self.target);
        let _open_guard = open_mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(transport) = self.current_handle() {
            return send_emergency_stop_transport(transport.as_ref());
        }
        let reopened: std::sync::Arc<dyn laser_device::DeviceTransport> =
            std::sync::Arc::from((factory)(&spec)?);
        let res = send_emergency_stop_transport(reopened.as_ref());
        if res.is_ok() {
            *self
                .transport
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reopened);
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
    /// 已登记的走边框作业编号（`framing-…`），含仍在握手或预检的急停目标。
    pub fn active_framing_job_ids(&self) -> Vec<String> {
        self.active_jobs
            .lock()
            .map(|jobs| {
                jobs.values()
                    .filter(|job| job.workflow_id.starts_with("framing-"))
                    .map(|job| job.workflow_id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

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

    /// 退出/崩溃兜底：对所有活动作业置取消并优先通过裸句柄直接写 0x18（锁中毒时恢复读取，绝不因锁中毒跳过急停）。
    pub fn abort_all_active_jobs(&self) -> Result<(), String> {
        let (jobs, uncertain) = {
            let jobs_guard = self
                .active_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let uncertain_guard = self
                .uncertain_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let launching = self
                .launching_workflows
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for token in launching.active.values() {
                token.cancel();
            }
            (
                jobs_guard.values().cloned().collect::<Vec<_>>(),
                uncertain_guard.values().cloned().collect::<Vec<_>>(),
            )
        };
        let mut first_error = None;
        for job in jobs {
            job.cancel.cancel();
            if let Err(e) = send_emergency_stop_transport(job.transport.as_ref()) {
                first_error.get_or_insert(e);
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

    /// Panic Hook 专用非阻塞急停：使用 `try_lock` 避免持锁线程 panic 时自死锁，
    /// 锁中毒时恢复读取并优先走无锁裸句柄写 0x18，不在崩溃现场阻塞重连。
    pub fn abort_all_active_jobs_nonblocking(&self) -> Result<(), String> {
        let jobs = match self.active_jobs.try_lock() {
            Ok(guard) => guard.values().cloned().collect::<Vec<_>>(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().values().cloned().collect::<Vec<_>>()
            }
            Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
        };
        let uncertain = match self.uncertain_jobs.try_lock() {
            Ok(guard) => guard.values().cloned().collect::<Vec<_>>(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().values().cloned().collect::<Vec<_>>()
            }
            Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
        };
        match self.launching_workflows.try_lock() {
            Ok(launching) => {
                for token in launching.active.values() {
                    token.cancel();
                }
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                for token in poisoned.into_inner().active.values() {
                    token.cancel();
                }
            }
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        let mut first_error = None;
        for job in jobs {
            job.cancel.cancel();
            // Panic 现场绝不回退阻塞的 write_realtime，只走非阻塞裸句柄
            if let Some(raw) = job.transport.raw_abort_handle()
                && let Err(e) = raw()
            {
                first_error.get_or_insert(e);
            }
        }
        for job in uncertain {
            job.cancel.cancel();
            if let Some(transport) = job.try_current_handle()
                && let Some(raw) = transport.raw_abort_handle()
                && let Err(e) = raw()
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

#[cfg(test)]
mod short_job_watch_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn short_job_watch_wakes_immediately_on_started() {
        let watch = ShortJobWatch::new();
        let bg = watch.clone();
        let started_at = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            {
                let mut state = bg.lock_state();
                *state = ShortJobStart::Started("framing-1".to_owned());
            }
            bg.notify_all();
        });
        let settled = watch.wait_until_settled(Instant::now() + Duration::from_secs(2));
        assert!(matches!(settled, ShortJobStart::Started(ref id) if id == "framing-1"));
        assert!(started_at.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn short_job_watch_wakes_on_mark_finished() {
        let watch = ShortJobWatch::new();
        let bg = watch.clone();
        let started_at = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            bg.mark_finished();
        });
        let settled = watch.wait_until_settled(Instant::now() + Duration::from_secs(2));
        assert!(matches!(settled, ShortJobStart::Finished));
        assert!(started_at.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn short_job_watch_abandons_on_timeout() {
        let watch = ShortJobWatch::new();
        let settled = watch.wait_until_settled(Instant::now() + Duration::from_millis(30));
        assert!(matches!(settled, ShortJobStart::Abandoned));
        assert!(matches!(watch.state(), ShortJobStart::Abandoned));
    }
}
