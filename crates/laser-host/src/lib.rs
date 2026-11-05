// NV15 restricted mock Home Assistant policy; NOT registered for control.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod voice_home;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod home_control;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod voice_agent;

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

#[derive(Default)]
pub struct ShutdownCoordinator {
    requested: Arc<AtomicBool>,
    raw_handles: Arc<Mutex<Vec<RawAbortCallback>>>,
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
        let mut handles = self
            .raw_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        handles.push(Arc::new(handle));
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

    fn snapshot_raw_handles(&self) -> Vec<RawAbortCallback> {
        self.raw_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn invoke_handles(handles: &[RawAbortCallback]) {
        for handle in handles {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (handle)()));
        }
    }

    /// Panic Hook 专用紧急停机：先克隆并释放 `raw_handles` 锁，再遍历裸句柄刷出 `0x18`，
    /// 且**不置位** `self.requested`，既杜绝持锁回调 panic 重入自锁，又保证后续正常退出仍有效。
    pub fn trigger_panic_abort(&self) {
        let Some(_guard) = enter_raw_abort() else {
            return;
        };
        let handles = self.snapshot_raw_handles();
        Self::invoke_handles(&handles);
    }

    /// 非阻塞停机：先 `try_lock` 快照句柄并立即释放锁，再在锁外刷出 `0x18`，不阻塞等待 SQLite 锁。
    pub fn trigger_shutdown_from_signal(&self) -> bool {
        if self.requested.swap(true, Ordering::SeqCst) {
            return false;
        }
        let Some(_guard) = enter_raw_abort() else {
            return true;
        };
        let handles = match self.raw_handles.try_lock() {
            Ok(guard) => guard.clone(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
            Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
        };
        Self::invoke_handles(&handles);
        true
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
            let handles = self.snapshot_raw_handles();
            Self::invoke_handles(&handles);
        }

        // 步骤 2: 任务安全清零落库
        if let Ok(db_guard) = self.db.lock() {
            if let Some(db_arc) = db_guard.as_ref() {
                if let Ok(db) = db_arc.lock() {
                    let _ = db.connection.execute(
                        "UPDATE jobs SET status='failed', finished_at=unixepoch(),
                         payload=json_set(payload,'$.result.detail','host 退出中断，任务安全终止')
                         WHERE status IN ('running', 'pending', 'queued')",
                        [],
                    );
                    let _ = db.connection.execute(
                        "UPDATE workflows SET status='failed', updated_at=unixepoch(),
                         payload=json_set(payload,'$.detail','host 退出中断，任务安全终止')
                         WHERE status IN ('sending', 'processing')",
                        [],
                    );
                    let _ = db.connection.execute(
                        "UPDATE calibrations SET status='failed', updated_at=unixepoch(),
                         payload=json_set(payload,'$.detail','host 退出中断，任务安全终止')
                         WHERE status IN ('sending', 'processing')",
                        [],
                    );
                }
            }
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
        // 由预先启动的普通后台线程完成完整停机（刷出 0x18 + 落库 + 重新传递信号）。
        unsafe extern "C" fn signal_handler(signum: i32) {
            let fd = SIGNAL_PIPE_WR.load(Ordering::Relaxed);
            if fd >= 0 {
                let byte = signum as u8;
                unsafe {
                    let _ = write(fd, (&byte as *const u8).cast(), 1);
                }
            }
        }

        let mut fds = [-1i32; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        let (rd, wr) = (fds[0], fds[1]);
        let nonblock_ok = unsafe {
            let _ = fcntl(rd, F_SETFD, FD_CLOEXEC);
            let _ = fcntl(wr, F_SETFD, FD_CLOEXEC);
            let flags = fcntl(wr, F_GETFL);
            flags >= 0 && fcntl(wr, F_SETFL, flags | O_NONBLOCK) == 0
        };
        if !nonblock_ok {
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
                    let n = unsafe { read(rd, (&mut byte as *mut u8).cast(), 1) };
                    if n == 1 {
                        let signum = i32::from(byte);
                        global_shutdown_coordinator().trigger_shutdown();
                        unsafe {
                            signal(signum, SIG_DFL);
                            raise(signum);
                        }
                        break;
                    }
                    if n == 0 {
                        break;
                    }
                }
                unsafe {
                    let _ = close(rd);
                }
            });
        if spawned.is_err() {
            SIGNAL_PIPE_WR.store(-1, Ordering::Release);
            unsafe {
                let _ = close(rd);
                let _ = close(wr);
            }
            return;
        }
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

/// 以给定身份调用进程内 HTTP 处理器，返回（状态码, JSON 响应体）。
fn in_process_request(
    api: &ApiService,
    method: &str,
    route: &str,
    body: &serde_json::Value,
    caller: laser_contracts::CallerContext,
) -> Result<(u16, serde_json::Value), String> {
    let payload = if body.is_null() { String::new() } else { body.to_string() };
    let request = format!(
        "{method} {route} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        payload.len(),
        payload
    );
    let raw = api.handle_http_with_ctx(request.as_bytes(), caller);
    let separator = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("后端响应缺少头部分隔符")?;
    let head = String::from_utf8_lossy(&raw[..separator]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("后端响应状态行无法解析")?;
    let body = serde_json::from_slice(&raw[separator + 4..])
        .unwrap_or_else(|_| serde_json::json!({"success": status < 400, "result": String::from_utf8_lossy(&raw[separator + 4..])}));
    Ok((status, body))
}

impl laser_mcp::HostBackend for InProcessBackend {
    fn request(&self, method: &str, route: &str, body: &serde_json::Value) -> Result<(u16, serde_json::Value), String> {
        // 技术书 §6.4 规则 2：进程内 MCP 转发显式声明身份，不借用回环语义
        in_process_request(&self.api, method, route, body, laser_contracts::CallerContext::McpBridge)
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
}

/// 内置语音服务调用 MOSS 工具时的后端（内置语音技术书 §5.2，NV4）：身份为 `CallerContext::Voice`，审计记为 `voice:<设备>`。
/// R4 与未分级工具始终拒绝；运动与出光（R3）跟随全局严格模式——开启时只生成桌面审批，关闭时与官方接入点相同，
/// 模型带 `confirmed=true` 即执行（2026-10-07 用户决定）。
pub struct VoiceBackend {
    pub api: Arc<ApiService>,
    pub device_id: String,
    pub session_id: String,
}

impl VoiceBackend {
    fn caller(&self) -> laser_contracts::CallerContext {
        laser_contracts::CallerContext::Voice { device_id: self.device_id.clone(), session_id: self.session_id.clone() }
    }
}

impl laser_mcp::HostBackend for VoiceBackend {
    fn request(&self, method: &str, route: &str, body: &serde_json::Value) -> Result<(u16, serde_json::Value), String> {
        in_process_request(&self.api, method, route, body, self.caller())
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
}

/// 内置语音可用的 MOSS 工具清单（MCP `tools` 格式）：与严格模式下的接入点相同，不含永不开放的工具、开发者工具
/// 与用户关闭的工具（NV8）。
pub fn voice_tools_list(api: &ApiService) -> serde_json::Value {
    let mut list = laser_mcp::tools_list_for(false, true);
    if let Some(tools) = list["tools"].as_array_mut() {
        tools.retain(|tool| !api.tool_disabled(tool["name"].as_str().unwrap_or("")));
        if let Some(external) = api.external_tools_list()["tools"].as_array() { tools.extend(external.iter().filter(|tool| !api.tool_disabled(tool["name"].as_str().unwrap_or(""))).cloned()); }
    }
    list
}

/// 以内置语音身份调用一个 MOSS 工具（同步；可能访问设备或生成文件，调用方放到阻塞线程执行）。
pub fn voice_tool_call(
    api: &Arc<ApiService>,
    device_id: &str,
    session_id: &str,
    name: &str,
    arguments: &serde_json::Value,
) -> laser_contracts::ToolCallResult {
    if !api.voice_session_allowed(device_id, session_id) {
        return laser_contracts::ToolCallResult::with_detail(false, "语音设备授权已失效，请重新连接", "voice_session_revoked");
    }
    if name.contains("__") {
        if api.tool_disabled(name) { return laser_contracts::ToolCallResult::err("该工具已关闭"); }
        return api.call_external_tool(&laser_contracts::CallerContext::Voice { device_id:device_id.into(),session_id:session_id.into() }, name, arguments);
    }
    let backend = VoiceBackend { api: api.clone(), device_id: device_id.to_owned(), session_id: session_id.to_owned() };
    let result = laser_mcp::dispatch_tool_call_with(name, arguments, false, Some(&backend));
    if name == "laser_safe_action_tool" && arguments["action"] == "emergency_stop" {
        // 执行优先：审计存储失败不能阻止停止请求。不记录识别文本、设备地址或返回原文。
        let entry = laser_contracts::AuditEntry {
            at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            client: laser_contracts::audit_client(&backend.caller()),
            action: laser_contracts::AuditAction::VoiceStopRequested,
            target: session_id.to_owned(),
            result: if result.success { laser_contracts::AuditResult::Ok } else { laser_contracts::AuditResult::Failed },
            detail: laser_contracts::AuditDetail { tool: Some(name.into()), reason: Some("emergency_stop request; physical state unverified".into()), ..Default::default() },
        };
        if api.record_audit(&entry).is_err() { eprintln!("[moss-host] voice stop audit write failed"); }
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
    if !api.voice_session_allowed(device_id, session_id) || api.tool_disabled("laser_workflow_tool") {
        return None;
    }
    let make_caller = || laser_contracts::CallerContext::Voice {
        device_id: device_id.into(),
        session_id: session_id.into(),
    };
    let body = serde_json::json!({"action": "recent"});
    let (status, response) = in_process_request(api, "POST", "/api/workflow", &body, make_caller()).ok()?;
    if status >= 400 {
        return None;
    }
    let mut result = response.get("result").cloned()?;
    let has_sending = result["workflows"]
        .as_array()
        .is_some_and(|items| items.iter().any(|w| w["status"].as_str() == Some("sending")));
    if has_sending
        && let Ok((200, targets_res)) = in_process_request(
            api,
            "GET",
            "/api/jobs/stop-targets",
            &serde_json::Value::Null,
            make_caller(),
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
pub fn mcp_bridge_connected() -> bool {
    laser_contracts::global_mcp_telemetry().snapshot().connected
}

fn read_runtime_settings(runtime_dir: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(runtime_dir.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// 运行目录 `settings.json` 中配置的接入点（NV8）：`(编号, 地址, 是否连接)`。
///
/// - `primary`：顶层 `mcp_endpoint`，未配置时回退环境变量 `MCP_ENDPOINT`；
/// - `extra-N`：`mcp_extra_endpoints` 的第 N 项（最多 [`laser_mcp::MAX_EXTRA_ENDPOINTS`] 个）。附加接入点是官方接入点，
///   自建模式下（主接入点指向自建接入点）暂不连接；无效地址与重复地址跳过。
pub fn configured_mcp_endpoints(runtime_dir: &std::path::Path) -> Vec<(String, String, bool)> {
    let settings = read_runtime_settings(runtime_dir);
    let mut endpoints = Vec::new();
    let primary = settings
        .get("mcp_endpoint")
        .and_then(serde_json::Value::as_str)
        .and_then(laser_mcp::endpoint_from_value)
        .or_else(laser_mcp::endpoint_from_env);
    if let Some(primary) = primary {
        endpoints.push(("primary".to_owned(), primary, true));
    }
    let extras_active = !voice_mode_is_self_hosted(runtime_dir);
    let extras = settings.get("mcp_extra_endpoints").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    for (index, value) in extras.iter().take(laser_mcp::MAX_EXTRA_ENDPOINTS).enumerate() {
        let Some(endpoint) = value.as_str().and_then(laser_mcp::endpoint_from_value) else { continue };
        if endpoints.iter().any(|(_, existing, _)| existing == &endpoint) {
            continue;
        }
        endpoints.push((format!("extra-{}", index + 1), endpoint, extras_active));
    }
    endpoints
}

/// 语音中心写入的自建链路设备会话数（`/api/system/info` 的 `voice.device_sessions`，`None` 为未知）。
pub fn set_voice_device_sessions(sessions: Option<u32>) {
    laser_contracts::global_voice_telemetry().set_device_sessions(sessions);
}

/// 内置语音服务写入的实时会话主状态（`idle`/`listening`/`thinking`/`speaking`）与归一化音量（0..=100）。
pub fn set_voice_pulse(phase: Option<&str>, level: u8) {
    laser_contracts::global_voice_telemetry().set_pulse(phase, level);
}
/// 运行目录 `settings.json` 中语音链路是否为自建模式（自建模式必须开启严格模式，技术书 §9.4.2）。
pub fn voice_mode_is_self_hosted(runtime_dir: &std::path::Path) -> bool {
    std::fs::read(runtime_dir.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("voice")?.get("mode")?.as_str().map(|mode| mode == "self_hosted"))
        .unwrap_or(false)
}

/// 语音中心“接入点与工具”卡片的数据（NV8）：各接入点的状态与身份、每个工具的开关与调用统计。地址均已脱敏。
pub fn mcp_overview(runtime_dir: &std::path::Path, api: &ApiService) -> serde_json::Value {
    let telemetry = laser_contracts::global_mcp_telemetry().snapshot();
    let endpoints: Vec<serde_json::Value> = configured_mcp_endpoints(runtime_dir)
        .into_iter()
        .map(|(id, endpoint, active)| {
            let link = telemetry.endpoints.iter().find(|link| link.id == id).cloned().unwrap_or_default();
            serde_json::json!({
                "id": id,
                "endpoint": laser_mcp::redact_endpoint(&endpoint),
                "active": active,
                "connected": active && link.connected,
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
            let description: String = tool["description"].as_str().unwrap_or("").chars().take(80).collect();
            let stat = telemetry.tool_stats.get(&name).cloned().unwrap_or_default();
            Some(serde_json::json!({
                "name": name,
                "description": description,
                "enabled": !api.tool_disabled(&name),
                "can_disable": laser_mcp::tool_can_be_disabled(&name),
                "voice_hidden": laser_contracts::tool_never_exposed(&name),
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
        "extras_paused": voice_mode_is_self_hosted(runtime_dir),
        "max_extra_endpoints": laser_mcp::MAX_EXTRA_ENDPOINTS,
        "tools": tools,
    })
}

/// 校验关闭的工具名单：只接受工具清单里可关闭的工具，去重并排序。
pub fn validate_disabled_tools(names: &[String]) -> Result<Vec<String>, String> {
    let known = laser_mcp::tools_list_json(true);
    let known: Vec<&str> = known["tools"].as_array().map(|tools| tools.iter().filter_map(|tool| tool["name"].as_str()).collect()).unwrap_or_default();
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
        // 语音中心选择了自建链路时，严格模式必须开启（技术书 §9.4.2）
        let strict_mode = strict_from_env || voice_mode_is_self_hosted(&runtime_root.path);
        let db = RuntimeDb::open(&runtime_root.database)
            .map_err(|e| format!("数据库打开失败: {e:?}"))?;
        // 落实 D6 规范：启动时对未完成残留任务执行安全清零，注明"host 重启中断，任务安全终止"
        let _ = db.recover_after_restart();
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
                .with_auth(Arc::new(laser_api::AuthManager::with_runtime_path(runtime_root.path.clone())))
                .with_allowed_origins(allowed_origins)
                .with_strict_mode(strict_mode),
        );
        // 技术书 §5.5：重启后所有待批准 / 已批准未执行的审批一律过期（A-32）
        let expired = api_service.recover_approvals_after_restart();
        if expired > 0 {
            eprintln!("启动时已将 {expired} 个未完成的审批标记为过期");
        }
        if strict_mode {
            eprintln!("严格模式已开启：运动或出光动作需要桌面会话或桌面审批");
        }

        // 五条退出路径统一兜底：对所有正在下发的作业置取消并直接写 0x18（实时信道 + 裸句柄）
        let abort_service = api_service.clone();
        coordinator.register_raw_abort(move || abort_service.abort_all_active_jobs());

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
        let controls = self.bridge.lock().map(|mut guard| std::mem::take(&mut *guard)).unwrap_or_default();
        for control in &controls {
            control.shutdown.store(true, Ordering::SeqCst);
        }
        let all_done = || controls.iter().all(|control| control.done.load(Ordering::SeqCst));
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
        let expected = configured_mcp_endpoints(&self.runtime_root.path)
            .into_iter()
            .filter(|(_, _, active)| *active)
            .count();
        let spawned = self.spawn_mcp_bridge();
        (stopped_cleanly, expected, spawned)
    }

    /// 小智 MCP 桥接：把各接入点（wss://）上的 JSON-RPC 工具调用转发到进程内 ApiService，每个接入点一条线程。
    /// 接入点见 [`configured_mcp_endpoints`]。返回启动的桥数量（0 表示未配置）。
    pub fn spawn_mcp_bridge(&self) -> usize {
        let endpoints = configured_mcp_endpoints(&self.runtime_root.path);
        let redacted: Vec<(String, String)> =
            endpoints.iter().map(|(id, endpoint, _)| (id.clone(), laser_mcp::redact_endpoint(endpoint))).collect();
        laser_contracts::global_mcp_telemetry().register_endpoints(&redacted);
        let developer_mode = laser_tools::developer_mode_enabled();
        let mut controls = Vec::new();
        for (id, endpoint, active) in endpoints {
            if !active {
                continue;
            }
            let backend: laser_mcp::SharedBackend = Arc::new(InProcessBackend { api: self.api_service.clone() });
            let shutdown = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            let stats = Arc::new(laser_mcp::BridgeStats::default());
            let (thread_shutdown, thread_done) = (shutdown.clone(), done.clone());
            let spawned = std::thread::Builder::new().name(format!("mcp-bridge-{id}")).spawn(move || {
                laser_mcp::run_bridge(&id, &endpoint, Some(backend), developer_mode, thread_shutdown, stats);
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
                eprintln!("提示: 首选端口 {host}:{preferred} 无法绑定 ({e})，正在自动搜寻可用端口...");
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

    #[test]
    fn in_process_backend_routes_mcp_calls_through_api_service() {
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
        let (status, body) = laser_mcp::HostBackend::request(&backend, "GET", "/api/health", &serde_json::Value::Null).unwrap();
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
            .with_approval_service(Arc::new(laser_api::approval::ApprovalService::new(db.clone()))),
        );
        let names: Vec<String> = voice_tools_list(&service)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert!(names.iter().any(|name| name == "laser_workflow_tool"));
        for hidden in ["command_execution_tool", "open_app_tool", "shortcut_key_execution_tool", "open_jsjds_website_tool"] {
            assert!(!names.iter().any(|name| name == hidden), "{hidden} 不对内置语音开放");
        }
        allow_test_voice(&service, &db, "aa:bb:cc:dd:ee:ff", "s1");
        let denied = voice_tool_call(&service, "aa:bb:cc:dd:ee:ff", "s1", "open_app_tool", &serde_json::json!({"app_name": "notepad"}));
        assert!(!denied.success);
        assert!(denied.detail.as_deref().unwrap_or("").contains("tool_not_allowed"), "{denied:?}");
        let audit = db.lock().unwrap().recent_audit(10).unwrap();
        assert!(audit.iter().any(|entry| entry.client == "voice:aa:bb:cc:dd:ee:ff" && entry.target == "open_app_tool"), "{audit:?}");
        let status = voice_tool_call(&service, "aa:bb:cc:dd:ee:ff", "s1", "laser_workflow_tool", &serde_json::json!({"action": "status", "workflow_id": "wf-missing"}));
        assert!(!status.success && status.result.contains("未找到"), "只读工具照常执行: {status:?}");
        // 关闭连接的 fake factory；不能触及真实设备。停止失败仍必须留下有界审计。
        let service = Arc::new(plain_service().with_runtime_db(db.clone()).with_approval_service(Arc::new(laser_api::approval::ApprovalService::new(db.clone())))
            .with_transport_factory(Arc::new(|_| Err("injected connection failure".into()))));
        allow_test_voice(&service, &db, "fake-device", "s-stop");
        let stopped = voice_tool_call(&service, "fake-device", "s-stop", "laser_safe_action_tool", &serde_json::json!({"action":"emergency_stop","connection_mode":"network","host":"127.0.0.1"}));
        assert!(!stopped.success);
        let audit = db.lock().unwrap().recent_audit(10).unwrap();
        let entry = audit.iter().find(|entry| entry.action == laser_contracts::AuditAction::VoiceStopRequested).unwrap();
        assert_eq!(entry.client, "voice:fake-device");
        assert_eq!(entry.target, "s-stop");
        assert_eq!(entry.result, laser_contracts::AuditResult::Failed);
        assert!(!serde_json::to_string(entry).unwrap().contains("127.0.0.1"));
    }

    /// 加工结束播报的轮询通道：只读、不走工具分发（因此不涨调用统计），
    /// 授权失效或用户在语音中心关了 `laser_workflow_tool` 时静默返回 `None`。
    #[test]
    fn voice_recent_workflows_is_read_only_and_never_counts_as_a_tool_call() {
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

    fn allow_test_voice(service: &ApiService, db: &Arc<Mutex<RuntimeDb>>, device: &str, session: &str) {
        db.lock().unwrap().voice_device_seen(device, "test-client", "", "", "", 0.0).unwrap();
        service.set_voice_device_status(device, laser_runtime::VOICE_DEVICE_ALLOWED, 0.0).unwrap();
        service.register_voice_session(device, "test-client", session).unwrap();
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
            let dir = std::env::temp_dir().join(format!("laser-host-{name}-{}", std::process::id()));
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

    /// NV8：主接入点加附加接入点；无效、重复与超出上限的跳过；自建模式下附加接入点暂停连接。
    #[test]
    fn configured_endpoints_include_extras_and_pause_them_in_self_hosted_mode() {
        let primary = "wss://official.example/mcp/?token=primary";
        let runtime = TempRuntime::new("endpoints", serde_json::json!({
            "mcp_endpoint": primary,
            "mcp_extra_endpoints": [
                "wss://official.example/mcp/?token=a",
                "http://not-websocket.example/",
                primary,
                "wss://official.example/mcp/?token=b",
                "wss://official.example/mcp/?token=c",
                "wss://official.example/mcp/?token=d"
            ]
        }));
        let endpoints = configured_mcp_endpoints(&runtime.0);
        let ids: Vec<&str> = endpoints.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(ids, ["primary", "extra-1", "extra-4"], "无效与重复跳过，只看前 {MAX_EXTRA_ENDPOINTS} 项");
        assert!(endpoints.iter().all(|(_, _, active)| *active));

        let mut settings: serde_json::Value = serde_json::from_slice(&std::fs::read(runtime.0.join("settings.json")).unwrap()).unwrap();
        settings["voice"] = serde_json::json!({"mode": "self_hosted"});
        std::fs::write(runtime.0.join("settings.json"), settings.to_string()).unwrap();
        let endpoints = configured_mcp_endpoints(&runtime.0);
        assert!(endpoints[0].2, "主接入点照常连接");
        assert!(endpoints[1..].iter().all(|(_, _, active)| !active), "自建模式下附加接入点暂停");

        // 总览：地址脱敏、带身份字段；自建模式标记暂停
        let overview = mcp_overview(&runtime.0, &plain_service());
        assert_eq!(overview["extras_paused"], true);
        assert_eq!(overview["endpoints"][0]["endpoint"], "wss://official.example/mcp/?token=***");
        assert!(!overview.to_string().contains("token=primary"), "总览不得含 token");
        assert_eq!(overview["endpoints"][1]["connected"], false);
    }

    /// NV8：工具开关从 settings.json 读入；关闭的工具不出现在内置语音清单里，调用被拒绝；停止类工具不能关闭。
    #[test]
    fn disabled_tools_are_hidden_from_voice_and_refused() {
        let runtime = TempRuntime::new("switches", serde_json::json!({
            "mcp_disabled_tools": ["material_params_tool", "laser_safe_action_tool", ""]
        }));
        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        let service = Arc::new(plain_service().with_runtime_path(runtime.0.clone()).with_runtime_db(db.clone()));
        allow_test_voice(&service, &db, "aa:bb:cc:dd:ee:ff", "s1");
        assert_eq!(service.disabled_tools(), vec!["material_params_tool".to_string()], "停止类工具与空名被忽略");
        let names: Vec<String> = voice_tools_list(&service)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert!(!names.contains(&"material_params_tool".to_owned()));
        assert!(names.contains(&"laser_safe_action_tool".to_owned()));
        let refused = voice_tool_call(&service, "aa:bb:cc:dd:ee:ff", "s1", "material_params_tool", &serde_json::json!({"action": "list"}));
        assert!(!refused.success && refused.detail.as_deref().unwrap_or("").contains("tool_disabled"), "{refused:?}");
        let backend = InProcessBackend { api: service.clone() };
        assert!(laser_mcp::HostBackend::tool_disabled(&backend, "material_params_tool"));
        let listed = laser_mcp::tools_list_with(false, Some(&backend));
        assert!(!listed.to_string().contains("\"material_params_tool\""));
        let overview = mcp_overview(&runtime.0, &service);
        let tool = overview["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == "material_params_tool").unwrap();
        assert_eq!(tool["enabled"], false);
        let stop = overview["tools"].as_array().unwrap().iter().find(|tool| tool["name"] == "laser_safe_action_tool").unwrap();
        assert_eq!(stop["can_disable"], false);

        service.set_disabled_tools(Vec::new());
        assert!(!service.tool_disabled("material_params_tool"), "运行中可重新打开");

        assert_eq!(
            validate_disabled_tools(&["open_app_tool".into(), "material_params_tool".into(), "open_app_tool".into()]).unwrap(),
            vec!["material_params_tool".to_string(), "open_app_tool".to_string()]
        );
        assert!(validate_disabled_tools(&["laser_safe_action_tool".into()]).unwrap_err().contains("不能关闭"));
        assert!(validate_disabled_tools(&["no_such_tool".into()]).unwrap_err().contains("没有名为"));
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
        db.lock().unwrap().connection.execute(
            "INSERT INTO jobs(job_id, status, created_at, payload) VALUES
             ('active_job', 'running', unixepoch(), '{}')",
            [],
        ).unwrap();

        coordinator.register_db(db.clone());

        assert!(coordinator.trigger_shutdown());
        // 幂等性测试: 第二次调用不重复执行
        assert!(!coordinator.trigger_shutdown());

        assert!(aborted.load(Ordering::SeqCst), "必须触发裸写急停");

        let job_status: String = db.lock().unwrap().connection.query_row(
            "SELECT status FROM jobs WHERE job_id='active_job'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(job_status, "failed", "活动 job 必须在退出时标记为 failed");
    }

    #[test]
    fn fake_device_receives_0x18_on_host_shutdown() {
        use laser_device::{FakeGrbl, DeviceTransport};

        let fake = FakeGrbl::new(["$32=1".to_string()]);
        let abort_handle = fake.raw_abort_handle().expect("raw handle must exist");

        let coordinator = ShutdownCoordinator::new();
        coordinator.register_raw_abort(abort_handle);

        assert!(coordinator.trigger_shutdown());
        assert_eq!(fake.realtime(), vec![0x18], "假设备必须在关机时收到 0x18 单字节软复位急停");
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
                .connection
                .execute(
                    "INSERT INTO jobs(job_id, status, created_at, payload) VALUES
                     ('job_1', 'running', unixepoch(), '{}')",
                    [],
                )
                .unwrap();
            guard
                .connection
                .execute(
                    "INSERT INTO workflows(workflow_id, status, created_at, updated_at, payload) VALUES
                     ('wf_1', 'sending', unixepoch(), unixepoch(), '{}')",
                    [],
                )
                .unwrap();
            guard
                .connection
                .execute(
                    "INSERT INTO calibrations(calibration_id, status, created_at, updated_at, payload) VALUES
                     ('cal_1', 'processing', unixepoch(), unixepoch(), '{}')",
                    [],
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
                .connection
                .query_row(
                    &format!(
                        "SELECT status, json_extract(payload, '{path}') FROM {table} WHERE {pk_col}=?1"
                    ),
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "failed", "{table} must be marked failed on shutdown");
            assert_eq!(detail, "host 退出中断，任务安全终止");
        }
    }

    #[test]
    fn signal_shutdown_is_non_blocking_when_db_lock_is_already_held() {
        use laser_device::{DeviceTransport, FakeGrbl};

        let fake = FakeGrbl::new(["$32=1".to_string()]);
        let abort_handle = fake.raw_abort_handle().expect("raw handle must exist");

        let coordinator = ShutdownCoordinator::new();
        coordinator.register_raw_abort(abort_handle);

        let db = Arc::new(Mutex::new(RuntimeDb::open_memory().unwrap()));
        coordinator.register_db(db.clone());

        // Simulate an async signal arriving while the interrupted thread already holds `db` lock
        let _held_db_guard = db.lock().unwrap();
        assert!(coordinator.trigger_shutdown_from_signal());
        assert!(coordinator.is_shutdown_requested());
        assert_eq!(fake.realtime(), vec![0x18]);
        assert!(!coordinator.trigger_shutdown_from_signal());
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
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("trigger_shutdown must not self-deadlock on raw_handles mutex"),
            true
        );
        assert_eq!(fake.realtime(), vec![0x18]);
    }
}
