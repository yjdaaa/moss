use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use laser_api::ApiService;
use laser_contracts::builtin_material_library;
use laser_runtime::{LaserSettings, RuntimeDb, RuntimeRoot};

pub type RawAbortCallback = Box<dyn Fn() -> Result<(), String> + Send + Sync + 'static>;

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
        let mut handles = self.raw_handles.lock().unwrap();
        handles.push(Box::new(handle));
    }

    pub fn register_db(&self, db: Arc<Mutex<RuntimeDb>>) {
        let mut target = self.db.lock().unwrap();
        *target = Some(db);
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// 核心退出激光安全兜底（规格 §16 & §22）:
    /// 1. 触发后第一步脱离 Tokio 运行时，向每个打开连接通过同步裸句柄直接写出 0x18；
    /// 2. 进行中任务标为 failed，注明 "host 退出中断"；
    /// 3. 幂等性保证：仅首个请求执行，后续调用直接返回 false。
    pub fn trigger_shutdown(&self) -> bool {
        if self.requested.swap(true, Ordering::SeqCst) {
            return false;
        }

        let start = Instant::now();

        // 步骤 1: 遍历所有活动设备连接，同步裸写 0x18
        if let Ok(handles) = self.raw_handles.lock() {
            for handle in handles.iter() {
                let _ = handle();
            }
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
                }
            }
        }

        let elapsed = start.elapsed();
        // 确保在 2s 超时约束内完成
        if elapsed > Duration::from_secs(2) {
            eprintln!("警告: 退出激光安全停机超时 (耗时 {:?})", elapsed);
        }

        true
    }
}

static GLOBAL_COORDINATOR: OnceLock<ShutdownCoordinator> = OnceLock::new();

pub fn global_shutdown_coordinator() -> &'static ShutdownCoordinator {
    GLOBAL_COORDINATOR.get_or_init(ShutdownCoordinator::new)
}

/// 路径 5 兜底: 注册异常 Panic Hook，在崩溃第一现场通过裸句柄同步刷出 0x18
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 第一时间脱离调度写入 0x18
        global_shutdown_coordinator().trigger_shutdown();
        default_hook(info);
    }));
}

/// 路径 4 兜底: 注册 Windows 控制台关闭事件 (SetConsoleCtrlHandler)
#[cfg(windows)]
pub fn install_console_ctrl_handler() {
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
}

/// 路径 3 兜底: Unix 平台 SIGHUP / SIGINT / SIGTERM 触发退出激光安全停机（规格 §16）
#[cfg(not(windows))]
pub fn install_console_ctrl_handler() {
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
        fn raise(signum: i32) -> i32;
    }
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const SIG_DFL: usize = 0;

    unsafe extern "C" fn signal_handler(signum: i32) {
        // 与 Windows 控制台回调一致：先经协调器同步裸写 0x18 并清零任务，
        // 再恢复默认处置并重新抛出信号，保留常规的信号退出码。
        global_shutdown_coordinator().trigger_shutdown();
        unsafe {
            signal(signum, SIG_DFL);
            raise(signum);
        }
    }

    unsafe {
        for signum in [SIGHUP, SIGINT, SIGTERM] {
            signal(signum, signal_handler as unsafe extern "C" fn(i32) as usize);
        }
    }
}

/// 进程内后端：MCP 工具调用直接走 ApiService 的 HTTP 处理器（回环语义，免认证），
/// 与网页/桌面端共享同一套门禁、存储与 JobWorker。
pub struct InProcessBackend {
    pub api: Arc<ApiService>,
}

impl laser_mcp::HostBackend for InProcessBackend {
    fn request(&self, method: &str, route: &str, body: &serde_json::Value) -> Result<(u16, serde_json::Value), String> {
        let payload = if body.is_null() { String::new() } else { body.to_string() };
        let request = format!(
            "{method} {route} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let raw = self.api.handle_http(request.as_bytes());
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
}

pub struct HostApplication {
    pub settings: LaserSettings,
    pub runtime_root: RuntimeRoot,
    pub db: Arc<Mutex<RuntimeDb>>,
    pub api_service: Arc<ApiService>,
    pub coordinator: &'static ShutdownCoordinator,
}

impl HostApplication {
    pub fn init() -> Result<Self, String> {
        let runtime_root = RuntimeRoot::resolve(None, None)
            .map_err(|e| format!("运行时目录初始化失败: {e:?}"))?;
        let settings = LaserSettings::from_env_with_runtime_dir(Some(&runtime_root.path))
            .map_err(|e| format!("配置读取失败: {e}"))?;
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
                .with_auth(Arc::new(laser_api::AuthManager::new())),
        );

        // 五条退出路径统一兜底：对所有正在下发的作业置取消并直接写 0x18（实时信道 + 裸句柄）
        let abort_service = api_service.clone();
        coordinator.register_raw_abort(move || abort_service.abort_all_active_jobs());

        Ok(Self {
            settings,
            runtime_root,
            db: db_arc,
            api_service,
            coordinator,
        })
    }

    /// 小智 MCP 桥接：把 `MCP_ENDPOINT`（wss://）上的 JSON-RPC 工具调用转发到进程内 ApiService。
    /// 未配置或仍为模板占位符时返回 None，不启动任何网络连接。
    pub fn spawn_mcp_bridge(&self) -> Option<std::thread::JoinHandle<()>> {
        let endpoint = laser_mcp::endpoint_from_env()?;
        let backend: laser_mcp::SharedBackend = Arc::new(InProcessBackend { api: self.api_service.clone() });
        let developer_mode = laser_tools::developer_mode_enabled();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(laser_mcp::BridgeStats::default());
        let handle = std::thread::Builder::new()
            .name("mcp-bridge".into())
            .spawn(move || laser_mcp::run_bridge(&endpoint, Some(backend), developer_mode, shutdown, stats))
            .ok()?;
        Some(handle)
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
}
