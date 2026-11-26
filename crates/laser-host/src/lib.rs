// 本机 Agent（NV13）在 `moss-agent`，家居控制（NV15）在 `moss-home`，由桌面壳直接组装。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod external_mcp;

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use laser_api::ApiService;
use laser_contracts::builtin_material_library;
use laser_runtime::{LaserSettings, RuntimeDb, RuntimeRoot};

/// 附加接入点数量上限（语音中心据此限制输入，NV8）。
pub use laser_mcp::MAX_EXTRA_ENDPOINTS;

pub type RawAbortCallback = Arc<dyn Fn() -> Result<(), String> + Send + Sync + 'static>;

std::thread_local! {
    static IN_RAW_ABORT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct RawAbortReentrancyGuard;
impl Drop for RawAbortReentrancyGuard {
    fn drop(&mut self) {
        IN_RAW_ABORT.with(|flag| flag.set(false));
    }
}

fn enter_raw_abort() -> Option<RawAbortReentrancyGuard> {
    let already = IN_RAW_ABORT.with(|flag| flag.replace(true));
    if already {
        None
    } else {
        Some(RawAbortReentrancyGuard)
    }
}

#[derive(Clone)]
struct RegisteredAbort {
    on_shutdown: RawAbortCallback,
    on_panic: RawAbortCallback,
}

#[derive(Default)]
pub struct ShutdownCoordinator {
    requested: Arc<AtomicBool>,
    raw_handles: Arc<Mutex<Vec<RegisteredAbort>>>,
    db: Arc<Mutex<Option<Arc<Mutex<RuntimeDb>>>>>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_raw_abort<F>(&self, handle: F)
    where
        F: Fn() -> Result<(), String> + Send + Sync + 'static,
    {
        let cb: RawAbortCallback = Arc::new(handle);
        self.register_shutdown_and_panic_abort(cb.clone(), cb);
    }

    /// 分别登记正常停机与 Panic Hook 非阻塞急停回调，避免持锁线程 panic 时在 hook 内自死锁。
    pub fn register_shutdown_and_panic_abort(
        &self,
        on_shutdown: RawAbortCallback,
        on_panic: RawAbortCallback,
    ) {
        let mut handles = self
            .raw_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        handles.push(RegisteredAbort {
            on_shutdown,
            on_panic,
        });
    }

    pub fn register_db(&self, db: Arc<Mutex<RuntimeDb>>) {
        let mut target = self
            .db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *target = Some(db);
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        self.requested.clone()
    }

    fn snapshot_shutdown_handles(&self) -> Vec<RawAbortCallback> {
        self.raw_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|entry| entry.on_shutdown.clone())
            .collect()
    }

    fn try_snapshot_panic_handles(&self) -> Vec<RawAbortCallback> {
        match self.raw_handles.try_lock() {
            Ok(guard) => guard.iter().map(|entry| entry.on_panic.clone()).collect(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned
                .into_inner()
                .iter()
                .map(|entry| entry.on_panic.clone())
                .collect(),
            Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
        }
    }

    fn invoke_handles(handles: &[RawAbortCallback]) {
        for handle in handles {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (handle)()));
        }
    }

    /// Panic Hook 专用紧急停机：先 `try_lock` 快照并释放 `raw_handles` 锁，再遍历非阻塞裸句柄刷出 `0x18`，
    /// 且**不置位** `self.requested`，既杜绝持锁线程 panic 自死锁，又保证后续正常退出仍有效。
    pub fn trigger_panic_abort(&self) {
        let Some(_guard) = enter_raw_abort() else {
            return;
        };
        let handles = self.try_snapshot_panic_handles();
        Self::invoke_handles(&handles);
    }

    /// 核心退出激光安全兜底（规格 §16 & §22）:
    /// 1. 触发后第一步快照并释放 `raw_handles` 锁，向每个打开连接通过同步裸句柄写出 0x18；
    /// 2. 进行中任务标为 failed，注明 "host 退出中断"；
    /// 3. 幂等性保证：仅首个请求执行，后续调用直接返回 false。
    pub fn trigger_shutdown(&self) -> bool {
        if self.requested.swap(true, Ordering::SeqCst) {
            return false;
        }

        let start = Instant::now();

        // 步骤 1: 快照后释放注册表锁，在锁外执行 0x18 回调（即使回调触发全局 panic hook 也不自锁）
        if let Some(_guard) = enter_raw_abort() {
            let handles = self.snapshot_shutdown_handles();
            Self::invoke_handles(&handles);
        }

        // 步骤 2: 任务安全清零落库与 WAL 整理收尾
        if let Ok(db_guard) = self.db.lock()
            && let Some(db_arc) = db_guard.as_ref()
            && let Ok(db) = db_arc.lock()
        {
            let _ = db.abort_active_records_on_shutdown();
            db.maintenance_on_shutdown();
        }

        let elapsed = start.elapsed();
        if elapsed > Duration::from_secs(2) {
            eprintln!("警告: 退出激光安全停机超时 (耗时 {:?})", elapsed);
        }

        true
    }
}

static GLOBAL_COORDINATOR: OnceLock<ShutdownCoordinator> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();
static CTRL_HANDLER_INSTALLED: OnceLock<()> = OnceLock::new();

pub fn global_shutdown_coordinator() -> &'static ShutdownCoordinator {
    GLOBAL_COORDINATOR.get_or_init(ShutdownCoordinator::new)
}

/// 路径 5 兜底: 注册异常 Panic Hook（幂等单次安装），在崩溃第一现场通过裸句柄同步刷出 0x18，
/// 且不置位 `requested`，避免被捕获的后台任务 panic 让后续真正的退出停机失效。
pub fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // 第一时间脱离调度写入 0x18
            global_shutdown_coordinator().trigger_panic_abort();
            default_hook(info);
        }));
    });
}

/// 路径 4 兜底: 注册 Windows 控制台关闭事件 (SetConsoleCtrlHandler)
#[cfg(windows)]
pub fn install_console_ctrl_handler() {
    CTRL_HANDLER_INSTALLED.get_or_init(|| {
        unsafe extern "system" fn console_handler(_ctrl_type: u32) -> i32 {
            global_shutdown_coordinator().trigger_shutdown();
            0 // 传递给后续处理或允许正常终止
        }

        unsafe extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<unsafe extern "system" fn(ctrl_type: u32) -> i32>,
                add: i32,
            ) -> i32;
        }

        unsafe {
            SetConsoleCtrlHandler(Some(console_handler), 1);
        }
    });
}

/// 路径 3 兜底: Unix 平台 SIGHUP / SIGINT / SIGTERM 触发退出激光安全停机（规格 §16）
#[cfg(not(windows))]
pub fn install_console_ctrl_handler() {
    CTRL_HANDLER_INSTALLED.get_or_init(|| {
        use std::sync::atomic::AtomicI32;
        static SIGNAL_PIPE_WR: AtomicI32 = AtomicI32::new(-1);

        unsafe extern "C" {
            fn pipe(fds: *mut i32) -> i32;
            fn close(fd: i32) -> i32;
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
            fn read(fd: i32, buf: *mut std::ffi::c_void, count: usize) -> isize;
            fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
            fn signal(signum: i32, handler: usize) -> usize;
            fn raise(signum: i32) -> i32;
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            fn __error() -> *mut i32;
            #[cfg(target_os = "android")]
            fn __errno() -> *mut i32;
            #[cfg(target_os = "linux")]
            fn __errno_location() -> *mut i32;
        }
        #[inline]
        unsafe fn errno_ptr() -> *mut i32 {
            // SAFETY: 平台 C 运行库保证线程局部 errno 地址在信号处理器内可安全读取与恢复。
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                unsafe { __error() }
            }
            #[cfg(target_os = "android")]
            {
                unsafe { __errno() }
            }
            #[cfg(target_os = "linux")]
            {
                unsafe { __errno_location() }
            }
            #[cfg(not(any(
                target_os = "macos",
                target_os = "ios",
                target_os = "android",
                target_os = "linux"
            )))]
            {
                std::ptr::null_mut()
            }
        }
        const SIGHUP: i32 = 1;
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        const SIG_DFL: usize = 0;
        const F_GETFL: i32 = 3;
        const F_SETFL: i32 = 4;
        const F_SETFD: i32 = 2;
        const FD_CLOEXEC: i32 = 1;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        const O_NONBLOCK: i32 = 0x0004;
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        const O_NONBLOCK: i32 = 0x0800;

        // 信号处理器内仅调用异步信号安全的 write() 写入 1 字节信号编号，
        // 同时保存并恢复 errno，避免破坏被中断线程的错误码；
        // 由预先启动的普通后台线程完成完整停机（刷出 0x18 + 落库 + 重新传递信号）。
        unsafe extern "C" fn signal_handler(signum: i32) {
            let err_p = unsafe { errno_ptr() };
            let saved_errno = if err_p.is_null() {
                0
            } else {
                // SAFETY: 非空 errno 指针指向当前线程的 errno 槽位。
                unsafe { *err_p }
            };
            let fd = SIGNAL_PIPE_WR.load(Ordering::Relaxed);
            if fd >= 0 {
                let byte = signum as u8;
                // SAFETY: write 为 POSIX async-signal-safe，fd 已设 O_NONBLOCK，缓冲区有效 1 字节。
                unsafe {
                    let _ = write(fd, (&byte as *const u8).cast(), 1);
                }
            }
            if !err_p.is_null() {
                // SAFETY: 恢复进入信号处理器前的 errno 值。
                unsafe {
                    *err_p = saved_errno;
                }
            }
        }

        let mut fds = [-1i32; 2];
        // SAFETY: fds 为长度为 2 的可写栈数组。
        if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        let (rd, wr) = (fds[0], fds[1]);
        // SAFETY: rd/wr 为刚创建的有效文件描述符。
        let nonblock_ok = unsafe {
            let _ = fcntl(rd, F_SETFD, FD_CLOEXEC);
            let _ = fcntl(wr, F_SETFD, FD_CLOEXEC);
            let flags = fcntl(wr, F_GETFL);
            flags >= 0 && fcntl(wr, F_SETFL, flags | O_NONBLOCK) == 0
        };
        if !nonblock_ok {
            // SAFETY: 关闭初始化失败的管道两端。
            unsafe {
                let _ = close(rd);
                let _ = close(wr);
            }
            return;
        }
        SIGNAL_PIPE_WR.store(wr, Ordering::Release);
        let spawned = std::thread::Builder::new()
            .name("moss-unix-signal-shutdown".into())
            .spawn(move || {
                let mut byte = 0u8;
                loop {
                    // SAFETY: rd 由本线程独占，byte 为有效 1 字节可写缓冲区。
                    let n = unsafe { read(rd, (&mut byte as *mut u8).cast(), 1) };
                    if n == 1 {
                        let signum = i32::from(byte);
                        global_shutdown_coordinator().trigger_shutdown();
                        // SAFETY: 恢复默认信号处理并重新向本进程投递原信号。
                        unsafe {
                            signal(signum, SIG_DFL);
                            raise(signum);
                        }
                        break;
                    }
                    if n == 0 {
                        break;
                    }
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                // SAFETY: 线程退出前关闭独占读端。
                unsafe {
                    let _ = close(rd);
                }
            });
        if spawned.is_err() {
            SIGNAL_PIPE_WR.store(-1, Ordering::Release);
            // SAFETY: 后台线程未启动，关闭管道两端。
            unsafe {
                let _ = close(rd);
                let _ = close(wr);
            }
            return;
        }
        // SAFETY: signal_handler 为 extern "C" 异步信号安全函数，且自管道与后台线程已就绪。
        unsafe {
            for signum in [SIGHUP, SIGINT, SIGTERM] {
                signal(signum, signal_handler as unsafe extern "C" fn(i32) as usize);
            }
        }
    });
}

/// 进程内后端：MCP 工具调用直接走 ApiService 的 HTTP 处理器（回环语义，免认证），
/// 与网页/桌面端共享同一套门禁、存储与 JobWorker。
pub struct InProcessBackend {
    pub api: Arc<ApiService>,
}

/// 以给定身份直接调用进程内类型化分发入口（不再拼装原始 HTTP/1.1 报文），返回（状态码, JSON 响应体）。
fn in_process_request(
    api: &ApiService,
    method: &str,
    route: &str,
    body: &serde_json::Value,
    caller: laser_contracts::CallerContext,
    short_job_watch: Option<&Arc<laser_api::ShortJobWatch>>,
) -> Result<(u16, serde_json::Value), String> {
    Ok(api.dispatch_json_with_watch(method, route, body, caller, short_job_watch))
}

impl laser_mcp::HostBackend for InProcessBackend {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String> {
        // 技术书 §6.4 规则 2：进程内 MCP 转发显式声明身份，不借用回环语义
        in_process_request(
            &self.api,
            method,
            route,
            body,
            laser_contracts::CallerContext::McpBridge,
            None,
        )
    }

    fn strict_mode(&self) -> bool {
        self.api.strict_mode()
    }

    fn audit(&self, entry: &laser_contracts::AuditEntry) -> Result<(), String> {
        self.api.record_audit(entry)
    }

    fn tool_disabled(&self, name: &str) -> bool {
        self.api.tool_disabled(name)
    }

    fn full_tool_list(&self) -> bool {
        self.api.full_tool_list()
    }

    fn default_connection(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.api
            .verified_connection()
            .and_then(|record| record["fields"].as_object().cloned())
    }

    fn mcp_telemetry(&self) -> &laser_contracts::McpTelemetry {
        self.api.mcp_telemetry().as_ref()
    }
}

/// 内置语音服务调用 MOSS 工具时的后端（内置语音技术书 §5.2，NV4）：身份为 `CallerContext::Voice`，审计记为 `voice:<设备>`。
/// R4 与未分级工具始终拒绝；运动与出光（R3）跟随全局严格模式——开启时只生成桌面审批，关闭时与官方接入点相同，
/// 模型带 `confirmed=true` 即执行（2026-10-07 用户决定）。
pub struct VoiceBackend {
    pub api: Arc<ApiService>,
    pub device_id: String,
    pub session_id: String,
    pub short_job_watch: Option<Arc<laser_api::ShortJobWatch>>,
}

impl VoiceBackend {
    fn caller(&self) -> laser_contracts::CallerContext {
        laser_contracts::CallerContext::Voice {
            device_id: self.device_id.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

impl laser_mcp::HostBackend for VoiceBackend {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String> {
        in_process_request(
            &self.api,
            method,
            route,
            body,
            self.caller(),
            self.short_job_watch.as_ref(),
        )
    }

    fn strict_mode(&self) -> bool {
        self.api.strict_mode()
    }

    fn always_deny_r4(&self) -> bool {
        true
    }

    fn audit(&self, entry: &laser_contracts::AuditEntry) -> Result<(), String> {
        self.api.record_audit(entry)
    }

    fn audit_client(&self) -> String {
        laser_contracts::audit_client(&self.caller())
    }

    fn tool_disabled(&self, name: &str) -> bool {
        self.api.tool_disabled(name)
    }

    fn full_tool_list(&self) -> bool {
        self.api.full_tool_list()
    }

    fn default_connection(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.api
            .verified_connection()
            .and_then(|record| record["fields"].as_object().cloned())
    }

    fn mcp_telemetry(&self) -> &laser_contracts::McpTelemetry {
        self.api.mcp_telemetry().as_ref()
    }
}

/// 内置语音可用的 MOSS 工具清单（MCP `tools` 格式）：与严格模式下的接入点相同，不含永不开放的工具、开发者工具
/// 与用户关闭的工具（NV8）。
pub fn voice_tools_list(api: &ApiService) -> serde_json::Value {
    // n72：默认精简清单，“完整工具（高级）”打开时给完整清单
    let mut list = laser_mcp::tools_list_profile(false, true, api.full_tool_list());
    if let Some(tools) = list["tools"].as_array_mut() {
        tools.retain(|tool| !api.tool_disabled(tool["name"].as_str().unwrap_or("")));
        if let Some(external) = api.external_tools_list()["tools"].as_array() {
            tools.extend(
                external
                    .iter()
                    .filter(|tool| !api.tool_disabled(tool["name"].as_str().unwrap_or("")))
                    .cloned(),
            );
        }
    }
    list
}

/// 内置语音按预览走边框（2026-11-18，技术书 r52）：边框一开始走就先回话，不等走完，
/// 让用户走到一半能说“暂停 / 继续 / 不走了”。没开始就结束的（未确认、门禁拒绝、严格模式待批准、
/// 连接或预检失败）照常返回结果；走边框本身仍在后台按原流程执行，急停与停止目标不变。
fn voice_frame_call(
    mut backend: VoiceBackend,
    arguments: &serde_json::Value,
) -> laser_contracts::ToolCallResult {
    use laser_api::ShortJobStart;
    // 语音工具 30 秒超时（moss-voice-server `tool_timeout`），这里更早放弃，放弃后同一次请求不会再让机器动
    const START_WAIT: Duration = Duration::from_secs(20);
    let deadline = Instant::now() + START_WAIT;
    let watch = laser_api::ShortJobWatch::new();
    backend.short_job_watch = Some(watch.clone());
    let arguments = arguments.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    {
        struct FinishWatchOnDrop(Arc<laser_api::ShortJobWatch>);
        impl Drop for FinishWatchOnDrop {
            fn drop(&mut self) {
                self.0.mark_finished();
            }
        }
        let watch_guard = FinishWatchOnDrop(watch.clone());
        std::thread::spawn(move || {
            let _guard = watch_guard;
            let result = laser_mcp::dispatch_tool_call_with(
                "laser_workflow_tool",
                &arguments,
                false,
                Some(&backend),
            );
            let _ = tx.send(result);
        });
    }
    let started_reply = |job_id: &str| {
        laser_contracts::ToolCallResult::ok(format!(
            "已开始走边框（{job_id}）：按预览范围空走一圈、不出光，走完回到起点。\
             途中用户说暂停用 laser_safe_action_tool 的 pause；说继续先问“确认继续走边框吗”，同意后 resume 带 confirmed=true；\
             说不走了、取消、别走了用 stop_framing；说停或急停用 emergency_stop"
        ))
    };
    match watch.wait_until_settled(deadline) {
        ShortJobStart::Started(job_id) => started_reply(&job_id),
        ShortJobStart::Abandoned => laser_contracts::ToolCallResult::err(
            "连接或检查机器太慢，这次走边框已放弃，机器不会再动；请看电脑上的 MOSS，确认机器正常后再说一次",
        ),
        ShortJobStart::Finished | ShortJobStart::Waiting => {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(result) => result,
                Err(_) => laser_contracts::ToolCallResult::err(
                    "走边框没有执行完，结果未知，请看电脑上的 MOSS",
                ),
            }
        }
    }
}

/// 以内置语音身份调用一个 MOSS 工具（同步；可能访问设备或生成文件，调用方放到阻塞线程执行）。
pub fn voice_tool_call(
    api: &Arc<ApiService>,
    device_id: &str,
    session_id: &str,
    name: &str,
    arguments: &serde_json::Value,
) -> laser_contracts::ToolCallResult {
    if api.is_shutdown_requested() {
        let is_stop = name == "laser_safe_action_tool"
            && arguments
                .get("action")
                .and_then(|v| v.as_str())
                .is_some_and(laser_contracts::is_safe_action_stop_or_halt);
        if !is_stop {
            return laser_contracts::ToolCallResult::with_detail(
                false,
                "服务正在关闭，已拒绝新的工具调用",
                "host_shutting_down",
            );
        }
    }
    if !api.voice_session_allowed(device_id, session_id) {
        return laser_contracts::ToolCallResult::with_detail(
            false,
            "语音设备授权已失效，请重新连接",
            "voice_session_revoked",
        );
    }
    if name.contains("__") {
        if api.tool_disabled(name) {
            return laser_contracts::ToolCallResult::err("该工具已关闭");
        }
        return api.call_external_tool(
            &laser_contracts::CallerContext::Voice {
                device_id: device_id.into(),
                session_id: session_id.into(),
            },
            name,
            arguments,
        );
    }
    let backend = VoiceBackend {
        api: api.clone(),
        device_id: device_id.to_owned(),
        session_id: session_id.to_owned(),
        short_job_watch: None,
    };
    if name == "laser_workflow_tool" && arguments["action"].as_str().map(str::trim) == Some("frame")
    {
        return voice_frame_call(backend, arguments);
    }
    let result = laser_mcp::dispatch_tool_call_with(name, arguments, false, Some(&backend));
    if name == "laser_safe_action_tool" && arguments["action"] == "emergency_stop" {
        // 执行优先：审计存储失败不能阻止停止请求。不记录识别文本、设备地址或返回原文。
        let entry = laser_contracts::AuditEntry {
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            client: laser_contracts::audit_client(&backend.caller()),
            action: laser_contracts::AuditAction::VoiceStopRequested,
            target: session_id.to_owned(),
            result: if result.success {
                laser_contracts::AuditResult::Ok
            } else {
                laser_contracts::AuditResult::Failed
            },
            detail: laser_contracts::AuditDetail {
                tool: Some(name.into()),
                reason: Some("emergency_stop request; physical state unverified".into()),
                ..Default::default()
            },
        };
        if api.record_audit(&entry).is_err() {
            eprintln!("[moss-host] voice stop audit write failed");
        }
    }
    result
}

/// 加工结束播报用的只读轮询：直接读进程内后端 `/api/workflow {action:"recent"}`。
/// **不经过工具分发**，因此不计入工具调用统计、也不会给模型多出一个工具；
/// 设备授权失效、用户在语音中心关闭了这个工具、或后端出错时返回 `None`（调用方保持静默）。
pub fn voice_recent_workflows(
    api: &Arc<ApiService>,
    device_id: &str,
    session_id: &str,
) -> Option<serde_json::Value> {
    if !api.voice_session_allowed(device_id, session_id) || api.tool_disabled("laser_workflow_tool")
    {
        return None;
    }
    let make_caller = || laser_contracts::CallerContext::Voice {
        device_id: device_id.into(),
        session_id: session_id.into(),
    };
    let body = serde_json::json!({"action": "recent"});
    let (status, response) =
        in_process_request(api, "POST", "/api/workflow", &body, make_caller(), None).ok()?;
    if status >= 400 {
        return None;
    }
    let mut result = response.get("result").cloned()?;
    let has_sending = result["workflows"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|w| w["status"].as_str() == Some("sending"))
    });
    if has_sending
        && let Ok((200, targets_res)) = in_process_request(
            api,
            "GET",
            "/api/jobs/stop-targets",
            &serde_json::Value::Null,
            make_caller(),
            None,
        )
        && let Some(jobs) = targets_res["result"]["jobs"].as_array()
        && let Some(items) = result["workflows"].as_array_mut()
    {
        for item in items.iter_mut() {
            if item["status"].as_str() != Some("sending") {
                continue;
            }
            let Some(wf_id) = item["workflow_id"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            if let Some(live) = jobs.iter().find(|j| {
                j["workflow_id"].as_str() == Some(wf_id) && j["state"].as_str() == Some("running")
            }) && let Some(obj) = item.as_object_mut()
            {
                obj.insert(
                    "live_progress".into(),
                    serde_json::json!({
                        "lines_sent": live["lines_sent"].as_u64().unwrap_or(0),
                        "total_lines": live["total_lines"].as_u64().unwrap_or(0),
                        "elapsed_ms": live["elapsed_ms"].as_u64().unwrap_or(0),
                    }),
                );
            }
        }
    }
    if let Some(pending) = api.pending_approval_display_summary()
        && let Some(root) = result.as_object_mut()
    {
        root.insert("pending_approval".into(), pending);
    }
    if let Some(agent_runs) = api.agent_voice_digest()
        && let Some(root) = result.as_object_mut()
    {
        root.insert("agent_runs".into(), agent_runs);
    }
    Some(result)
}

pub struct HostApplication {
    pub settings: LaserSettings,
    pub runtime_root: RuntimeRoot,
    pub db: Arc<Mutex<RuntimeDb>>,
    pub api_service: Arc<ApiService>,
    pub coordinator: &'static ShutdownCoordinator,
    /// `.env` 中配置的严格模式（`MOSS_FUSION_STRICT`）。语音中心切回官方链路时恢复为该值；
    /// n27 起语音中心的关闭开关会把它一起置为 false（否则 `enabled || from_env` 会把热关挡回去）。
    pub strict_from_env: std::sync::atomic::AtomicBool,
    /// 当前各条小智桥（每个接入点一条，NV8）的停止开关与结束标记（语音中心切换链路时先断旧桥再接新桥，技术书 §9.4.2）。
    bridge: Mutex<Vec<BridgeControl>>,
}

impl HostApplication {
    /// 启动时 `.env` 快照的严格模式。
    pub fn strict_from_env(&self) -> bool {
        self.strict_from_env
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 语音中心热关/热开时同步这份快照，避免启动值把运行时的开关挡回去。
    pub fn set_strict_from_env(&self, value: bool) {
        self.strict_from_env
            .store(value, std::sync::atomic::Ordering::Relaxed);
    }
}

struct BridgeControl {
    shutdown: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

/// 小智桥当前是否已与接入点握手（语音中心状态卡用；多接入点时任一已连接即为真）。
pub fn mcp_bridge_connected(api: &ApiService) -> bool {
    api.mcp_telemetry().snapshot().connected
}

fn read_runtime_settings(
    runtime_dir: &std::path::Path,
) -> serde_json::Map<String, serde_json::Value> {
    laser_runtime::settings_store::read(&laser_runtime::settings_store::settings_path(runtime_dir))
}

/// 运行目录 `settings.json` 中配置的接入点（NV8）：`(编号, 地址)`。
///
/// - `primary`：顶层 `mcp_endpoint`，未配置时回退环境变量 `MCP_ENDPOINT`；
/// - `extra-N`：`mcp_extra_endpoints` 的第 N 项（最多 [`laser_mcp::MAX_EXTRA_ENDPOINTS`] 个），无效地址与重复地址跳过。
pub fn configured_mcp_endpoints(runtime_dir: &std::path::Path) -> Vec<(String, String)> {
    let settings = read_runtime_settings(runtime_dir);
    let mut endpoints = Vec::new();
    let primary = settings
        .get("mcp_endpoint")
        .and_then(serde_json::Value::as_str)
        .and_then(laser_mcp::endpoint_from_value)
        .or_else(laser_mcp::endpoint_from_env);
    if let Some(primary) = primary {
        endpoints.push(("primary".to_owned(), primary));
    }
    let extras = settings
        .get("mcp_extra_endpoints")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (index, value) in extras
        .iter()
        .take(laser_mcp::MAX_EXTRA_ENDPOINTS)
        .enumerate()
    {
        let Some(endpoint) = value.as_str().and_then(laser_mcp::endpoint_from_value) else {
            continue;
        };
        if endpoints.iter().any(|(_, existing)| existing == &endpoint) {
            continue;
        }
        endpoints.push((format!("extra-{}", index + 1), endpoint));
    }
    endpoints
}

/// 内置语音服务写入的设备会话数（`/api/system/info` 的 `voice.device_sessions`，`None` 为未知）。
pub fn set_voice_device_sessions(api: &ApiService, sessions: Option<u32>) {
    api.voice_telemetry().set_device_sessions(sessions);
}

/// 内置语音服务写入的实时会话主状态（`idle`/`listening`/`thinking`/`speaking`）与归一化音量（0..=100）。
pub fn set_voice_pulse(api: &ApiService, phase: Option<&str>, level: u8) {
    api.voice_telemetry().set_pulse(phase, level);
}
/// 语音中心“接入点与工具”卡片的数据（NV8）：各接入点的状态与身份、每个工具的开关与调用统计。地址均已脱敏。
pub fn mcp_overview(runtime_dir: &std::path::Path, api: &ApiService) -> serde_json::Value {
    let telemetry = api.mcp_telemetry().snapshot();
    let endpoints: Vec<serde_json::Value> = configured_mcp_endpoints(runtime_dir)
        .into_iter()
        .map(|(id, endpoint)| {
            let link = telemetry
                .endpoints
                .iter()
                .find(|link| link.id == id)
                .cloned()
                .unwrap_or_default();
            serde_json::json!({
                "id": id,
                "endpoint": laser_mcp::redact_endpoint(&endpoint),
                "connected": link.connected,
                "connected_at": link.connected_at,
                "connections": link.connections,
                "last_error": link.last_error,
                "client": link.client,
                "identity": laser_mcp::endpoint_identity(&endpoint),
            })
        })
        .collect();
    let developer_mode = laser_tools::developer_mode_enabled();
    let tools: Vec<serde_json::Value> = laser_mcp::tools_list_json(developer_mode)["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tool| {
            let name = tool["name"].as_str()?.to_owned();
            let description: String = tool["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            let stat = telemetry.tool_stats.get(&name).cloned().unwrap_or_default();
            Some(serde_json::json!({
                "name": name,
                "description": description,
                "enabled": !api.tool_disabled(&name),
                "can_disable": laser_mcp::tool_can_be_disabled(&name),
                "voice_hidden": laser_contracts::tool_never_exposed(&name),
                // n72：是否在精简清单里（精简清单下不在的只在“完整工具（高级）”打开时提供给 AI）
                "in_compact": laser_api::laser_mcp_compact_tool(&name),
                "calls": stat.calls,
                "failures": stat.failures,
                "denied": stat.denied,
                "last_at": (stat.calls > 0).then_some(stat.last_at),
                "last_caller": stat.last_caller,
            }))
        })
        .collect();
    serde_json::json!({
        "endpoints": endpoints,
        "max_extra_endpoints": laser_mcp::MAX_EXTRA_ENDPOINTS,
        "tools": tools,
        "full_tool_list": api.full_tool_list(),
    })
}

/// 校验关闭的工具名单：只接受工具清单里可关闭的工具，去重并排序。
pub fn validate_disabled_tools(names: &[String]) -> Result<Vec<String>, String> {
    let known = laser_mcp::tools_list_json(true);
    let known: Vec<&str> = known["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut accepted = Vec::new();
    for name in names {
        let name = name.trim();
        if !known.contains(&name) {
            return Err(format!("没有名为 {name} 的工具"));
        }
        if !laser_mcp::tool_can_be_disabled(name) {
            return Err(format!("{name} 负责停止与急停，不能关闭"));
        }
        if !accepted.iter().any(|existing: &String| existing == name) {
            accepted.push(name.to_owned());
        }
    }
    accepted.sort();
    Ok(accepted)
}

impl HostApplication {
    pub fn init() -> Result<Self, String> {
        Self::init_with_runtime_dir(None)
    }

    /// 与 [`init`](Self::init) 相同，但可显式指定运行时数据目录。
    ///
    /// Android 进程的当前目录是只读的 `/`，也没有 `APPDATA`，默认规则会落到 `/.runtime`
    /// 而初始化失败（Read-only file system）；移动端必须传入应用私有数据目录。
    pub fn init_with_runtime_dir(runtime_dir: Option<&std::path::Path>) -> Result<Self, String> {
        let runtime_root = RuntimeRoot::resolve(runtime_dir, None).map_err(|e| match e {
            laser_runtime::RuntimeRootError::AlreadyLocked(lock) => {
                let stop_hint = if cfg!(target_os = "windows") {
                    "请从托盘图标右键“退出程序”，或在任务管理器结束 moss-desktop.exe / moss-host.exe 后重试"
                } else if cfg!(target_os = "macos") {
                    "请从菜单栏图标选择“退出程序”，或在“活动监视器”结束 moss-desktop / moss-host 后重试"
                } else {
                    "请先退出正在运行的 MOSS，或结束 moss-desktop / moss-host 进程后重试"
                };
                format!("已有 MOSS 实例正在运行（数据目录锁 {} 被占用）。{stop_hint}", lock.display())
            }
            other => format!("运行时目录初始化失败: {other:?}"),
        })?;
        let settings = LaserSettings::from_env_with_runtime_dir(Some(&runtime_root.path))
            .map_err(|e| format!("配置读取失败: {e}"))?;
        // 浏览器额外允许来源（技术书 §6.5）；写了 `*` 时启动失败，不静默放开。
        let allowed_origins = laser_runtime::config::allowed_origins_from_env()
            .map_err(|e| format!("配置读取失败: {e}"))?;
        // 严格模式（技术书 §6.2）；开启网关却关闭严格模式、或写错取值时启动失败。
        let strict_from_env = laser_runtime::config::fusion_strict_from_env()
            .map_err(|e| format!("配置读取失败: {e}"))?;
        let strict_mode = strict_from_env;
        let db = RuntimeDb::open(&runtime_root.database)
            .map_err(|e| format!("数据库打开失败: {e:?}"))?;
        // 落实 D6 规范：启动时对未完成残留任务执行安全清零，注明"host 重启中断，任务安全终止"，并定额清理陈旧历史记录
        let _ = db.recover_after_restart();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let _ = db.prune_historical_records(now_secs);
        let _ = db.prune_orphan_generated_files(
            &[
                runtime_root.generated_assets.as_path(),
                runtime_root.previews.as_path(),
            ],
            now_secs,
        );
        let db_arc = Arc::new(Mutex::new(db));

        let coordinator = global_shutdown_coordinator();
        coordinator.register_db(db_arc.clone());

        // 安装退出钩子
        install_panic_hook();
        install_console_ctrl_handler();

        let materials = builtin_material_library();
        let api_service = Arc::new(
            ApiService::new(settings.clone(), materials)
                .with_upload_root(runtime_root.uploads.clone())
                .with_generated_root(runtime_root.generated_assets.clone())
                .with_runtime_path(runtime_root.path.clone())
                .with_preview_roots(vec![
                    runtime_root.previews.clone(),
                    runtime_root.uploads.clone(),
                    runtime_root.generated_assets.clone(),
                ])
                .with_runtime_db(db_arc.clone())
                .with_auth(Arc::new(laser_api::AuthManager::with_runtime_path(
                    runtime_root.path.clone(),
                )))
                .with_allowed_origins(allowed_origins)
                .with_strict_mode(strict_mode)
                .with_shutdown_flag(coordinator.shutdown_flag()),
        );
        // 技术书 §5.5：重启后所有待批准 / 已批准未执行的审批一律过期（A-32）
        // 完整过程记录到期清理（不看开关：关掉后旧记录照样按 30 天删除；n110 起不按大小删）
        if let Some(trace) = api_service.process_trace() {
            trace.sweep();
        }
        let expired = api_service.recover_approvals_after_restart();
        if expired > 0 {
            eprintln!("启动时已将 {expired} 个未完成的审批标记为过期");
        }
        if strict_mode {
            eprintln!("严格模式已开启：运动或出光动作需要桌面会话或桌面审批");
        }

        // 五条退出路径统一兜底：正常退出走完整急停，Panic Hook 走 try_lock 非阻塞裸句柄急停
        let abort_service = api_service.clone();
        let panic_service = api_service.clone();
        coordinator.register_shutdown_and_panic_abort(
            Arc::new(move || abort_service.abort_all_active_jobs()),
            Arc::new(move || panic_service.abort_all_active_jobs_nonblocking()),
        );

        Ok(Self {
            settings,
            runtime_root,
            db: db_arc,
            api_service,
            coordinator,
            strict_from_env: std::sync::atomic::AtomicBool::new(strict_from_env),
            bridge: Mutex::new(Vec::new()),
        })
    }

    /// 停止当前所有小智桥并等待其退出（最多 `wait`）。返回旧桥是否都已确认退出。
    pub fn stop_mcp_bridge(&self, wait: Duration) -> bool {
        let controls = self
            .bridge
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        for control in &controls {
            control.shutdown.store(true, Ordering::SeqCst);
        }
        let all_done = || {
            controls
                .iter()
                .all(|control| control.done.load(Ordering::SeqCst))
        };
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if all_done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        all_done()
    }

    /// 先断旧桥再按当前 settings.json 的地址接新桥（语音中心切换链路、改接入点或工具开关后用）。
    /// 返回是否启动了至少一条新桥。
    pub fn restart_mcp_bridge(&self) -> bool {
        if !self.stop_mcp_bridge(Duration::from_secs(5)) {
            eprintln!("提示：旧的小智桥 5 秒内未退出，将在其下次检查时停止");
        }
        self.spawn_mcp_bridge() > 0
    }

    /// 重启小智 MCP 桥并返回 `(旧桥已在时限内退出, 应启动的活跃接入点数, 实际成功启动的桥线程数)`。
    pub fn restart_mcp_bridge_checked(&self) -> (bool, usize, usize) {
        let stopped_cleanly = self.stop_mcp_bridge(Duration::from_secs(5));
        if !stopped_cleanly {
            eprintln!("提示：旧的小智桥 5 秒内未退出，将在其下次检查时停止");
        }
        let expected = configured_mcp_endpoints(&self.runtime_root.path).len();
        let spawned = self.spawn_mcp_bridge();
        (stopped_cleanly, expected, spawned)
    }

    /// 小智 MCP 桥接：把各接入点（wss://）上的 JSON-RPC 工具调用转发到进程内 ApiService，每个接入点一条线程。
    /// 接入点见 [`configured_mcp_endpoints`]。返回启动的桥数量（0 表示未配置）。
    pub fn spawn_mcp_bridge(&self) -> usize {
        let endpoints = configured_mcp_endpoints(&self.runtime_root.path);
        let redacted: Vec<(String, String)> = endpoints
            .iter()
            .map(|(id, endpoint)| (id.clone(), laser_mcp::redact_endpoint(endpoint)))
            .collect();
        self.api_service
            .mcp_telemetry()
            .register_endpoints(&redacted);
        let developer_mode = laser_tools::developer_mode_enabled();
        let mut controls = Vec::new();
        for (id, endpoint) in endpoints {
            let backend: laser_mcp::SharedBackend = Arc::new(InProcessBackend {
                api: self.api_service.clone(),
            });
            let shutdown = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            let stats = Arc::new(laser_mcp::BridgeStats::default());
            let (thread_shutdown, thread_done) = (shutdown.clone(), done.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("mcp-bridge-{id}"))
                .spawn(move || {
                    laser_mcp::run_bridge(
                        &id,
                        &endpoint,
                        Some(backend),
                        developer_mode,
                        thread_shutdown,
                        stats,
                    );
                    thread_done.store(true, Ordering::SeqCst);
                });
            if spawned.is_ok() {
                controls.push(BridgeControl { shutdown, done });
            }
        }
        let count = controls.len();
        if let Ok(mut guard) = self.bridge.lock() {
            // 调用方未先停止旧桥时，通知旧桥退出，只保留这一批
            for previous in guard.drain(..) {
                previous.shutdown.store(true, Ordering::SeqCst);
            }
            *guard = controls;
        }
        count
    }

    /// 语音中心“接入点与工具”卡片的数据（NV8），见 [`mcp_overview`]。
    pub fn mcp_overview(&self) -> serde_json::Value {
        mcp_overview(&self.runtime_root.path, &self.api_service)
    }

    /// 校验关闭的工具名单（语音中心保存 `mcp_disabled_tools` 前调用），见 [`validate_disabled_tools`]。
    pub fn validate_disabled_tools(&self, names: &[String]) -> Result<Vec<String>, String> {
        validate_disabled_tools(names)
    }

    pub fn spawn_http_server(&self) -> Result<(std::thread::JoinHandle<()>, u16), String> {
        let host = &self.settings.web_host;
        let preferred = self.settings.web_port;
        let (listener, actual_port) = match TcpListener::bind(format!("{host}:{preferred}")) {
            Ok(l) => (l, preferred),
            Err(e) => {
                eprintln!(
                    "提示: 首选端口 {host}:{preferred} 无法绑定 ({e})，正在自动搜寻可用端口..."
                );
                let mut found = None;
                for p in (preferred + 1)..=(preferred + 50) {
                    if let Ok(l) = TcpListener::bind(format!("{host}:{p}")) {
                        found = Some((l, p));
                        break;
                    }
                }
                if let Some((l, p)) = found {
                    (l, p)
                } else {
                    let l = TcpListener::bind(format!("{host}:0"))
                        .map_err(|e| format!("无法绑定任何可用端口: {e}"))?;
                    let p = l.local_addr().map_err(|e| e.to_string())?.port();
                    (l, p)
                }
            }
        };

        self.api_service.set_actual_port(actual_port);
        let service = self.api_service.clone();
        let handle = std::thread::spawn(move || {
            let _ = ApiService::serve(listener, service);
        });
        Ok((handle, actual_port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `laser_workflow_tool` 的调用计数全进程共享（`global_mcp_telemetry`）。断言“不计入调用统计”的测试
    /// 与会经工具分发调用它的测试互斥，免得并行时被别的测试带动计数而误报（2026-11-08）。
    /// 新增经分发调用 `laser_workflow_tool` 的测试也要先取这把锁。
    static WORKFLOW_TOOL_STATS: Mutex<()> = Mutex::new(());

    fn workflow_tool_stats_guard() -> std::sync::MutexGuard<'static, ()> {
        WORKFLOW_TOOL_STATS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn in_process_backend_routes_mcp_calls_through_api_service() {
        let _stats = workflow_tool_stats_guard();
        let service = Arc::new(ApiService::new(
            LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            builtin_material_library(),
        ));
        let backend = InProcessBackend { api: service };
        let (status, body) = laser_mcp::HostBackend::request(
            &backend,
            "GET",
            "/api/health",
            &serde_json::Value::Null,
        )
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["result"]["status"], "ok");
        let result = laser_mcp::dispatch_tool_call_with(
            "laser_workflow_tool",
            &serde_json::json!({"action": "status", "workflow_id": "wf-missing"}),
            false,
            Some(&backend),
        );
        assert!(!result.success, "不存在的 workflow 必须失败: {result:?}");
        assert!(result.result.contains("未找到"), "{result:?}");
    }

    /// NV4：内置语音后端——永不开放的工具不列出，即使全局严格模式关闭，调用也被拒并记审计（`voice:<设备>`）；
    /// 只读工具照常执行。
    #[test]
    fn voice_backend_denies_desktop_control_tools_and_audits() {
        let _stats = workflow_tool_stats_guard();
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let service = Arc::new(
            ApiService::new(
                LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 8766,
                    material_params_file: None,
                },
                builtin_material_library(),
            )
            .with_strict_mode(false)
            .with_runtime_db(db.clone())
            .with_approval_service(Arc::new(
                laser_api::approval::ApprovalService::new(db.clone()),
            )),
        );
        let names: Vec<String> = voice_tools_list(&service)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert!(names.iter().any(|name| name == "laser_workflow_tool"));
        for hidden in [
            "command_execution_tool",
            "open_app_tool",
            "shortcut_key_execution_tool",
        ] {
            assert!(
                !names.iter().any(|name| name == hidden),
                "{hidden} 不对内置语音开放"
            );
        }
        allow_test_voice(&service, &db, "aa:bb:cc:dd:ee:ff", "s1");
        let denied = voice_tool_call(
            &service,
            "aa:bb:cc:dd:ee:ff",
            "s1",
            "open_app_tool",
            &serde_json::json!({"app_name": "notepad"}),
        );
        assert!(!denied.success);
        assert!(
            denied
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("tool_not_allowed"),
            "{denied:?}"
        );
        let audit = db.lock().unwrap().recent_audit(10).unwrap();
        assert!(
            audit
                .iter()
                .any(|entry| entry.client == "voice:aa:bb:cc:dd:ee:ff"
                    && entry.target == "open_app_tool"),
            "{audit:?}"
        );
        let status = voice_tool_call(
            &service,
            "aa:bb:cc:dd:ee:ff",
            "s1",
            "laser_workflow_tool",
            &serde_json::json!({"action": "status", "workflow_id": "wf-missing"}),
        );
        assert!(
            !status.success && status.result.contains("未找到"),
            "只读工具照常执行: {status:?}"
        );
        // 关闭连接的 fake factory；不能触及真实设备。停止失败仍必须留下有界审计。
        let service = Arc::new(
            plain_service()
                .with_runtime_db(db.clone())
                .with_approval_service(Arc::new(laser_api::approval::ApprovalService::new(
                    db.clone(),
                )))
                .with_transport_factory(Arc::new(|_| Err("injected connection failure".into()))),
        );
        allow_test_voice(&service, &db, "fake-device", "s-stop");
        let stopped = voice_tool_call(
            &service,
            "fake-device",
            "s-stop",
            "laser_safe_action_tool",
            &serde_json::json!({"action":"emergency_stop","connection_mode":"network","host":"127.0.0.1"}),
        );
        assert!(!stopped.success);
        let audit = db.lock().unwrap().recent_audit(10).unwrap();
        let entry = audit
            .iter()
            .find(|entry| entry.action == laser_contracts::AuditAction::VoiceStopRequested)
            .unwrap();
        assert_eq!(entry.client, "voice:fake-device");
        assert_eq!(entry.target, "s-stop");
        assert_eq!(entry.result, laser_contracts::AuditResult::Failed);
        assert!(!serde_json::to_string(entry).unwrap().contains("127.0.0.1"));
    }

    /// 加工结束播报的轮询通道：只读、不走工具分发（因此不涨调用统计），
    /// 授权失效或用户在语音中心关了 `laser_workflow_tool` 时静默返回 `None`。
    #[test]
    fn voice_recent_workflows_is_read_only_and_never_counts_as_a_tool_call() {
        let _stats = workflow_tool_stats_guard();
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let service = Arc::new(plain_service().with_runtime_db(db.clone()));
        allow_test_voice(&service, &db, "watch-device", "watch-session");
        let stat = || {
            laser_mcp::global_mcp_telemetry()
                .snapshot()
                .tool_stats
                .get("laser_workflow_tool")
                .map(|stat| stat.calls)
                .unwrap_or(0)
        };
        let before = stat();
        let body = voice_recent_workflows(&service, "watch-device", "watch-session")
            .expect("授权有效的语音会话必须能读到最近任务");
        assert!(body.get("workflows").is_some(), "{body}");
        assert_eq!(stat(), before, "自动轮询不能计入工具调用统计");

        assert!(
            voice_recent_workflows(&service, "watch-device", "other-session").is_none(),
            "未授权的会话必须静默"
        );
        service.set_disabled_tools(vec!["laser_workflow_tool".into()]);
        assert!(
            voice_recent_workflows(&service, "watch-device", "watch-session").is_none(),
            "用户关掉这个工具后不再轮询"
        );
    }

    /// 内置语音按预览走边框（2026-11-18，技术书 r52）：边框一开始走就回话，不等走完；
    /// 回话后照样能“停止走边框”，后台作业按取消收尾；未确认的不连接设备、照常返回错误。
    #[test]
    fn voice_frame_replies_once_framing_starts_and_can_be_stopped() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct HeldGrbl {
            responses: Mutex<std::collections::VecDeque<String>>,
            gate: Arc<AtomicBool>,
        }
        impl laser_device::DeviceTransport for HeldGrbl {
            fn write_line(&self, line: &str) -> Result<(), String> {
                if line.starts_with('$') {
                    let mut responses = self.responses.lock().unwrap();
                    if line == "$$" {
                        responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                    } else {
                        responses.push_back("ok".into());
                    }
                    return Ok(());
                }
                // 走边框的 G-code：一直“在走”，直到测试放行
                while !self.gate.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err("测试结束：模拟连接断开".into())
            }
            fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
                if command == laser_device::RealtimeCommand::StatusQuery {
                    self.responses
                        .lock()
                        .unwrap()
                        .push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
                }
                Ok(())
            }
            fn read_line(&self) -> Result<Option<String>, String> {
                Ok(self.responses.lock().unwrap().pop_front())
            }
        }
        let gate = Arc::new(AtomicBool::new(false));
        let factory_gate = gate.clone();
        let runtime = TempRuntime::new("voice-frame", serde_json::json!({}));
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let service = Arc::new(
            plain_service()
                .with_generated_root(runtime.0.clone())
                .with_runtime_db(db.clone())
                .with_approval_service(Arc::new(laser_api::approval::ApprovalService::new(
                    db.clone(),
                )))
                .with_transport_factory(Arc::new(move |_| {
                    Ok(Box::new(HeldGrbl {
                        responses: Mutex::default(),
                        gate: factory_gate.clone(),
                    })
                        as Box<dyn laser_device::DeviceTransport>)
                })),
        );
        allow_test_voice(&service, &db, "aa:bb:cc:dd:ee:01", "s-frame");
        let body = r#"{"text":"FRAME","material":"椴木","thickness_mm":3,"font_size_mm":10}"#;
        let raw = service.handle_http(
            format!(
                "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        let text = String::from_utf8_lossy(&raw);
        let json: serde_json::Value =
            serde_json::from_str(&text[text.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        let workflow_id = json["result"]["workflow_id"].as_str().unwrap().to_owned();
        let call = |args: serde_json::Value| {
            voice_tool_call(
                &service,
                "aa:bb:cc:dd:ee:01",
                "s-frame",
                "laser_workflow_tool",
                &args,
            )
        };
        let unconfirmed = call(serde_json::json!({
            "action": "frame", "workflow_id": workflow_id, "network_host": "127.0.0.1"
        }));
        assert!(!unconfirmed.success, "{unconfirmed:?}");
        assert!(service.active_framing_job_ids().is_empty());

        let started = std::time::Instant::now();
        let framing = call(serde_json::json!({
            "action": "frame", "workflow_id": workflow_id, "confirmed": true,
            "connection_mode": "network", "network_host": "127.0.0.1"
        }));
        assert!(
            framing.success && framing.result.contains("已开始走边框"),
            "{framing:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(service.active_framing_job_ids().len(), 1, "边框还在走");
        service.request_shutdown();
        let new_framing = call(serde_json::json!({
            "action": "frame", "workflow_id": workflow_id, "confirmed": true,
            "connection_mode": "network", "network_host": "127.0.0.1"
        }));
        assert_eq!(new_framing.detail.as_deref(), Some("host_shutting_down"));
        let stopped = voice_tool_call(
            &service,
            "aa:bb:cc:dd:ee:01",
            "s-frame",
            "laser_safe_action_tool",
            &serde_json::json!({"action": "stop_framing"}),
        );
        assert!(stopped.success, "{stopped:?}");
        gate.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while service.active_job_count() > 0 {
            assert!(std::time::Instant::now() < deadline, "停止后作业应收尾");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn allow_test_voice(
        service: &ApiService,
        db: &Arc<Mutex<RuntimeDb>>,
        device: &str,
        session: &str,
    ) {
        db.lock()
            .unwrap()
            .voice_device_seen(device, "test-client", "", "", "", 0.0)
            .unwrap();
        service
            .set_voice_device_status(device, laser_runtime::VOICE_DEVICE_ALLOWED, 0.0)
            .unwrap();
        service
            .register_voice_session(device, "test-client", session)
            .unwrap();
    }

    fn plain_service() -> ApiService {
        ApiService::new(
            LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            builtin_material_library(),
        )
    }

    /// 测试专用的临时运行目录（本 crate 没有 tempfile 依赖）。
    struct TempRuntime(std::path::PathBuf);

    impl TempRuntime {
        fn new(name: &str, settings: serde_json::Value) -> Self {
            let dir =
                std::env::temp_dir().join(format!("laser-host-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("settings.json"), settings.to_string()).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempRuntime {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// NV8：主接入点加附加接入点；无效、重复与超出上限的跳过。
    /// 2026-11-17 删除自建链路后，遗留的 `"mode": "self_hosted"` 不再让附加接入点暂停。
    #[test]
    fn configured_endpoints_include_extras() {
        let primary = "wss://official.example/mcp/?token=primary";
        let runtime = TempRuntime::new(
            "endpoints",
            serde_json::json!({
                "mcp_endpoint": primary,
                "mcp_extra_endpoints": [
                    "wss://official.example/mcp/?token=a",
                    "http://not-websocket.example/",
                    primary,
                    "wss://official.example/mcp/?token=b",
                    "wss://official.example/mcp/?token=c",
                    "wss://official.example/mcp/?token=d"
                ]
            }),
        );
        let endpoints = configured_mcp_endpoints(&runtime.0);
        let ids: Vec<&str> = endpoints.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            ["primary", "extra-1", "extra-4"],
            "无效与重复跳过，只看前 {MAX_EXTRA_ENDPOINTS} 项"
        );

        let mut settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(runtime.0.join("settings.json")).unwrap())
                .unwrap();
        settings["voice"] = serde_json::json!({"mode": "self_hosted"});
        std::fs::write(runtime.0.join("settings.json"), settings.to_string()).unwrap();
        assert_eq!(
            configured_mcp_endpoints(&runtime.0).len(),
            3,
            "遗留的自建模式不再暂停附加接入点"
        );

        // 总览：地址脱敏、带身份字段；不再有暂停标记
        let overview = mcp_overview(&runtime.0, &plain_service());
        assert!(overview.get("extras_paused").is_none(), "{overview}");
        assert!(overview["endpoints"][1].get("active").is_none());
        assert_eq!(
            overview["endpoints"][0]["endpoint"],
            "wss://official.example/mcp/?token=***"
        );
        assert!(
            !overview.to_string().contains("token=primary"),
            "总览不得含 token"
        );
        assert_eq!(overview["endpoints"][1]["connected"], false);
    }

    /// NV8：工具开关从 settings.json 读入；关闭的工具不出现在内置语音清单里，调用被拒绝；停止类工具不能关闭。
    #[test]
    fn disabled_tools_are_hidden_from_voice_and_refused() {
        let runtime = TempRuntime::new(
            "switches",
            serde_json::json!({
                "mcp_disabled_tools": ["material_params_tool", "laser_safe_action_tool", ""]
            }),
        );
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let service = Arc::new(
            plain_service()
                .with_runtime_path(runtime.0.clone())
                .with_runtime_db(db.clone()),
        );
        allow_test_voice(&service, &db, "aa:bb:cc:dd:ee:ff", "s1");
        assert_eq!(
            service.disabled_tools(),
            vec!["material_params_tool".to_string()],
            "停止类工具与空名被忽略"
        );
        let names: Vec<String> = voice_tools_list(&service)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert!(!names.contains(&"material_params_tool".to_owned()));
        assert!(names.contains(&"laser_safe_action_tool".to_owned()));
        let refused = voice_tool_call(
            &service,
            "aa:bb:cc:dd:ee:ff",
            "s1",
            "material_params_tool",
            &serde_json::json!({"action": "list"}),
        );
        assert!(
            !refused.success
                && refused
                    .detail
                    .as_deref()
                    .unwrap_or("")
                    .contains("tool_disabled"),
            "{refused:?}"
        );
        let backend = InProcessBackend {
            api: service.clone(),
        };
        assert!(laser_mcp::HostBackend::tool_disabled(
            &backend,
            "material_params_tool"
        ));
        let listed = laser_mcp::tools_list_with(false, Some(&backend));
        assert!(!listed.to_string().contains("\"material_params_tool\""));
        let overview = mcp_overview(&runtime.0, &service);
        let tool = overview["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "material_params_tool")
            .unwrap();
        assert_eq!(tool["enabled"], false);
        let stop = overview["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "laser_safe_action_tool")
            .unwrap();
        assert_eq!(stop["can_disable"], false);

        service.set_disabled_tools(Vec::new());
        assert!(
            !service.tool_disabled("material_params_tool"),
            "运行中可重新打开"
        );

        // n72：默认精简清单；“完整工具（高级）”打开后接入点与内置语音拿到完整清单
        let names = |list: serde_json::Value| -> Vec<String> {
            list["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect()
        };
        assert!(!service.full_tool_list(), "默认精简");
        let compact = names(laser_mcp::tools_list_with(true, Some(&backend)));
        assert!(compact.contains(&"laser_workflow_tool".to_owned()));
        assert!(!compact.contains(&"laser_grbl_tool".to_owned()));
        for name in &compact {
            assert!(
                laser_api::laser_mcp_compact_tool(name),
                "laser-api 的精简名单要与 laser-mcp 一致：{name}"
            );
        }
        assert!(!names(voice_tools_list(&service)).contains(&"start_tuned_job_tool".to_owned()));
        service.set_full_tool_list(true);
        assert!(
            names(laser_mcp::tools_list_with(true, Some(&backend)))
                .contains(&"laser_grbl_tool".to_owned())
        );
        assert!(names(voice_tools_list(&service)).contains(&"start_tuned_job_tool".to_owned()));
        service.set_full_tool_list(false);

        assert_eq!(
            validate_disabled_tools(&[
                "open_app_tool".into(),
                "material_params_tool".into(),
                "open_app_tool".into()
            ])
            .unwrap(),
            vec![
                "material_params_tool".to_string(),
                "open_app_tool".to_string()
            ]
        );
        assert!(
            validate_disabled_tools(&["laser_safe_action_tool".into()])
                .unwrap_err()
                .contains("不能关闭")
        );
        assert!(
            validate_disabled_tools(&["no_such_tool".into()])
                .unwrap_err()
                .contains("没有名为")
        );
    }

    #[test]
    fn shutdown_coordinator_triggers_raw_aborts_and_fails_jobs() {
        let coordinator = ShutdownCoordinator::new();
        let aborted = Arc::new(AtomicBool::new(false));
        let aborted_clone = aborted.clone();

        coordinator.register_raw_abort(move || {
            aborted_clone.store(true, Ordering::SeqCst);
            Ok(())
        });

        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        db.lock()
            .unwrap()
            .test_execute_batch(
                "INSERT INTO jobs(job_id, status, created_at, payload) VALUES
             ('active_job', 'running', unixepoch(), '{}')",
            )
            .unwrap();

        coordinator.register_db(db.clone());

        assert!(coordinator.trigger_shutdown());
        // 幂等性测试: 第二次调用不重复执行
        assert!(!coordinator.trigger_shutdown());

        assert!(aborted.load(Ordering::SeqCst), "必须触发裸写急停");

        let job_status: String = db
            .lock()
            .unwrap()
            .test_query_string("SELECT status FROM jobs WHERE job_id='active_job'")
            .unwrap();
        assert_eq!(job_status, "failed", "活动 job 必须在退出时标记为 failed");
    }

    #[test]
    fn fake_device_receives_0x18_on_host_shutdown() {
        use laser_device::{DeviceTransport, FakeGrbl};

        let fake = FakeGrbl::new(["$32=1".to_string()]);
        let abort_handle = fake.raw_abort_handle().expect("raw handle must exist");

        let coordinator = ShutdownCoordinator::new();
        coordinator.register_raw_abort(abort_handle);

        assert!(coordinator.trigger_shutdown());
        assert_eq!(
            fake.realtime(),
            vec![0x18],
            "假设备必须在关机时收到 0x18 单字节软复位急停"
        );
    }

    #[test]
    fn shutdown_coordinator_state_changes_once() {
        let coordinator = ShutdownCoordinator::new();
        assert!(!coordinator.is_shutdown_requested());
        assert!(coordinator.trigger_shutdown());
        assert!(coordinator.is_shutdown_requested());
        assert!(!coordinator.trigger_shutdown());
    }

    #[test]
    fn panic_abort_sends_0x18_without_disarming_subsequent_shutdown_and_cleans_calibrations() {
        use laser_device::{DeviceTransport, FakeGrbl};

        let fake = FakeGrbl::new(["$32=1".to_string()]);
        let abort_handle = fake.raw_abort_handle().expect("raw handle must exist");

        let coordinator = ShutdownCoordinator::new();
        coordinator.register_raw_abort(abort_handle);

        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        {
            let guard = db.lock().unwrap();
            guard
                .test_execute_batch(
                    "INSERT INTO jobs(job_id, status, created_at, payload) VALUES
                     ('job_1', 'running', unixepoch(), '{}');
                     INSERT INTO workflows(workflow_id, status, created_at, updated_at, payload) VALUES
                     ('wf_1', 'sending', unixepoch(), unixepoch(), '{}');
                     INSERT INTO calibrations(calibration_id, status, created_at, updated_at, payload) VALUES
                     ('cal_1', 'processing', unixepoch(), unixepoch(), '{}');",
                )
                .unwrap();
        }
        coordinator.register_db(db.clone());

        // Simulate a caught background worker panic: must send 0x18 immediately without flipping `requested`
        coordinator.trigger_panic_abort();
        assert_eq!(fake.realtime(), vec![0x18]);
        assert!(
            !coordinator.is_shutdown_requested(),
            "caught panic must not permanently disarm future real shutdown"
        );

        // Subsequent real shutdown still runs, sends 0x18 again, and fails jobs/workflows/calibrations
        assert!(coordinator.trigger_shutdown());
        assert!(coordinator.is_shutdown_requested());
        assert_eq!(fake.realtime(), vec![0x18, 0x18]);
        assert!(!coordinator.trigger_shutdown());

        let guard = db.lock().unwrap();
        for (table, pk_col, id, path) in [
            ("jobs", "job_id", "job_1", "$.result.detail"),
            ("workflows", "workflow_id", "wf_1", "$.detail"),
            ("calibrations", "calibration_id", "cal_1", "$.detail"),
        ] {
            let (status, detail): (String, String) = guard
                .test_query_string_pair(
                    &format!(
                        "SELECT status, json_extract(payload, '{path}') FROM {table} WHERE {pk_col}=?1"
                    ),
                    id,
                )
                .unwrap();
            assert_eq!(
                status, "failed",
                "{table} must be marked failed on shutdown"
            );
            assert_eq!(detail, "host 退出中断，任务安全终止");
        }
    }

    #[test]
    fn panicking_raw_abort_callback_never_self_deadlocks_and_runs_remaining_handles() {
        use laser_device::{DeviceTransport, FakeGrbl};

        let fake = FakeGrbl::new(["$32=1".to_string()]);
        let abort_handle = fake.raw_abort_handle().expect("raw handle must exist");

        let coordinator = Arc::new(ShutdownCoordinator::new());
        let coord_in_panic = coordinator.clone();
        // 模拟安装了全局 panic hook：回调自身发生 panic 时在栈展开前重入 trigger_panic_abort()
        coordinator.register_raw_abort(move || {
            coord_in_panic.trigger_panic_abort();
            panic!("simulated raw abort panic");
        });
        coordinator.register_raw_abort(abort_handle);

        let (tx, rx) = std::sync::mpsc::channel();
        let coord_runner = coordinator.clone();
        std::thread::spawn(move || {
            let ok = coord_runner.trigger_shutdown();
            let _ = tx.send(ok);
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("trigger_shutdown must not self-deadlock on raw_handles mutex")
        );
        assert_eq!(fake.realtime(), vec![0x18]);
    }

    #[test]
    fn injected_mcp_telemetry_receives_tool_calls_without_mutating_global() {
        let isolated = Arc::new(laser_contracts::McpTelemetry::new());
        let api = Arc::new(
            ApiService::new(
                LaserSettings {
                    default_connection_mode: "network".into(),
                    engraving_mode: "raster".into(),
                    web_host: "127.0.0.1".into(),
                    web_port: 8766,
                    material_params_file: None,
                },
                laser_contracts::builtin_material_library(),
            )
            .with_mcp_telemetry(isolated.clone()),
        );
        let backend = InProcessBackend { api: api.clone() };
        let res = laser_mcp::dispatch_tool_call_with(
            "material_params_tool",
            &serde_json::json!({
                "material": "wood",
                "thickness_mm": 3.0,
                "laser_mode": "engrave"
            }),
            false,
            Some(&backend),
        );
        assert!(res.success, "{res:?}");
        let snap = isolated.snapshot();
        assert_eq!(
            snap.tool_stats
                .get("material_params_tool")
                .map(|stat| stat.calls),
            Some(1)
        );
        let overview = mcp_overview(std::path::Path::new("."), &api);
        let mat_tool = overview["tools"]
            .as_array()
            .and_then(|arr| arr.iter().find(|t| t["name"] == "material_params_tool"))
            .expect("material_params_tool in overview");
        assert_eq!(mat_tool["calls"], 1);
    }

    #[test]
    fn bridge_and_voice_status_use_injected_telemetry() {
        let make_api = || {
            plain_service()
                .with_mcp_telemetry(Arc::new(laser_contracts::McpTelemetry::new()))
                .with_voice_telemetry(Arc::new(laser_contracts::VoiceTelemetry::new()))
        };
        let api = make_api();
        let other = make_api();

        api.mcp_telemetry().set_connected(true);
        assert!(mcp_bridge_connected(&api));
        assert!(!mcp_bridge_connected(&other));

        set_voice_device_sessions(&api, Some(2));
        set_voice_pulse(&api, Some("speaking"), 42);
        assert_eq!(api.voice_telemetry().device_sessions(), Some(2));
        assert_eq!(api.voice_telemetry().pulse(), (Some("speaking"), 42));
        assert_eq!(other.voice_telemetry().device_sessions(), None);
        assert_eq!(other.voice_telemetry().pulse(), (None, 0));
    }
}
