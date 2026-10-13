//! xiaozhi-server 子进程管理（技术书 §9.4.3，实施清单 P4-02 / P4-04）。
//!
//! - 同一时刻最多一个由 MOSS 启动的实例；重复启动返回 `already_running`。
//! - 启动前探测端口：已被占用且不是本进程启动的，记为“外部运行”，不能停止（`not_owned`）。
//! - 停止只针对自己持有句柄的子进程：先请求正常退出，超时后强制结束；Windows 用 Job Object
//!   （随句柄关闭结束整个进程树），MOSS 异常退出时也不会留下孤儿进程。
//! - 子进程意外退出时记录退出码，**不自动重启**；stdout / stderr 逐行脱敏写入日志。

use std::io::{BufRead, BufReader, Read};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::logfile::RotatingLog;
use crate::settings::VoiceSettings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceError {
    /// 已有 MOSS 启动的实例在运行
    AlreadyRunning,
    /// 端口已被其他程序占用（外部运行的服务端）
    ExternalRunning,
    /// 没有正在运行的服务端
    NotRunning,
    /// 正在运行的不是 MOSS 启动的实例，不能停止
    NotOwned,
    /// 缺少目录、Python 路径或服务端配置
    Config(String),
    /// 服务端配置文件缺失，需先完成配置向导
    ConfigMissing(String),
    Spawn(String),
}

impl VoiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AlreadyRunning => "server_already_running",
            Self::ExternalRunning => "server_external",
            Self::NotRunning => "server_not_running",
            Self::NotOwned => "server_not_owned",
            Self::Config(_) => "voice_config_invalid",
            Self::ConfigMissing(_) => "server_config_missing",
            Self::Spawn(_) => "server_spawn_failed",
        }
    }

    pub fn http_status(&self) -> u16 {
        match self {
            Self::AlreadyRunning | Self::ExternalRunning | Self::NotOwned => 409,
            Self::NotRunning => 409,
            Self::Config(_) | Self::ConfigMissing(_) => 400,
            Self::Spawn(_) => 500,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::AlreadyRunning => "服务端已由 MOSS 启动并在运行".into(),
            Self::ExternalRunning => "服务端端口已被其他程序占用（外部运行的服务端），MOSS 不会再启动一个".into(),
            Self::NotRunning => "服务端没有在运行".into(),
            Self::NotOwned => "正在运行的服务端不是 MOSS 启动的，请在启动它的地方关闭".into(),
            Self::Config(detail) => format!("语音中心设置不完整：{detail}"),
            Self::ConfigMissing(detail) => format!("服务端配置缺失：{detail}。请先在“模型配置”中完成并保存"),
            Self::Spawn(detail) => format!("启动服务端失败：{detail}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    Stopped,
    Running,
    /// 端口被占用但不是 MOSS 启动的
    External,
    /// 意外退出（崩溃），等待用户手动启动
    Exited,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServerStatus {
    pub state: ServerState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exited_at: Option<f64>,
    /// 是否由 MOSS 启动（只有这种才显示停止按钮）
    pub owned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 一次启动所需的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub port: u16,
}

impl ServerSpec {
    /// 由语音设置得到启动参数；目录、Python 或服务端配置缺失时给出明确原因。
    pub fn from_settings(voice: &VoiceSettings) -> Result<Self, VoiceError> {
        let dir = voice
            .server_dir
            .as_deref()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
            .ok_or_else(|| VoiceError::Config("未设置 xiaozhi-server 目录".into()))?;
        let python = voice
            .python
            .as_deref()
            .map(str::trim)
            .filter(|python| !python.is_empty())
            .ok_or_else(|| VoiceError::Config("未设置 Python 解释器路径".into()))?;
        let cwd = PathBuf::from(dir);
        if !cwd.is_dir() {
            return Err(VoiceError::Config("xiaozhi-server 目录不存在".into()));
        }
        let config = voice.config_path().ok_or_else(|| VoiceError::Config("未设置 xiaozhi-server 目录".into()))?;
        if !config.is_file() {
            return Err(VoiceError::ConfigMissing("data/.config.yaml 不存在".into()));
        }
        let args = if voice.args.is_empty() { vec!["app.py".into()] } else { voice.args.clone() };
        Ok(Self { program: PathBuf::from(python), args, cwd, port: voice.server_port })
    }
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// 本机端口是否有程序在监听。
pub fn port_in_use(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&address, Duration::from_millis(300)).is_ok()
}

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };

    /// 子进程所在的 Job Object；句柄关闭（含 MOSS 异常退出）时结束整个进程树。
    pub struct JobHandle(HANDLE);

    // 句柄只在持有 Mutex 时使用
    unsafe impl Send for JobHandle {}

    impl JobHandle {
        pub fn for_child(child: &std::process::Child) -> Option<Self> {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job == 0 as HANDLE {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if configured == 0 || AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }

        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }

        /// 取消“句柄关闭即结束进程树”（用户退出 MOSS 时选择保留服务端运行）。
        pub fn release_on_close(&self) -> bool {
            unsafe {
                let info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) != 0
            }
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod job {
    /// 非 Windows 平台没有 Job Object；停止时直接结束子进程。
    pub struct JobHandle;

    impl JobHandle {
        pub fn for_child(_child: &std::process::Child) -> Option<Self> {
            None
        }

        pub fn terminate(&self) {}

        pub fn release_on_close(&self) -> bool {
            true
        }
    }
}

struct Running {
    child: Child,
    pid: u32,
    started_at: f64,
    job: Option<job::JobHandle>,
    stopping: Arc<AtomicBool>,
}

struct LastExit {
    code: Option<i32>,
    at: f64,
    by_user: bool,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    last_exit: Option<LastExit>,
}

/// 服务端进程管理器（每个应用一个）。
pub struct Supervisor {
    inner: Arc<Mutex<Inner>>,
    log: Arc<RotatingLog>,
    graceful_timeout: Duration,
}

impl Supervisor {
    pub fn new(log: RotatingLog) -> Self {
        Self { inner: Arc::new(Mutex::new(Inner::default())), log: Arc::new(log), graceful_timeout: Duration::from_secs(5) }
    }

    /// 正常退出的等待时间（默认 5 秒，测试可缩短）。
    pub fn with_graceful_timeout(mut self, timeout: Duration) -> Self {
        self.graceful_timeout = timeout;
        self
    }

    pub fn log(&self) -> &RotatingLog {
        &self.log
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 当前状态。`port` 用于识别外部运行的实例。
    pub fn status(&self, port: u16) -> ServerStatus {
        let inner = self.lock();
        if let Some(running) = &inner.running {
            return ServerStatus {
                state: ServerState::Running,
                pid: Some(running.pid),
                started_at: Some(running.started_at),
                exit_code: None,
                exited_at: None,
                owned: true,
                message: None,
            };
        }
        let (exit_code, exited_at, crashed) = match &inner.last_exit {
            Some(exit) => (exit.code, Some(exit.at), !exit.by_user),
            None => (None, None, false),
        };
        drop(inner);
        if port_in_use(port) {
            return ServerStatus {
                state: ServerState::External,
                pid: None,
                started_at: None,
                exit_code: None,
                exited_at: None,
                owned: false,
                message: Some("端口已被其他程序占用：服务端在 MOSS 之外运行".into()),
            };
        }
        if crashed {
            let code = exit_code.map(|code| code.to_string()).unwrap_or_else(|| "未知".into());
            return ServerStatus {
                state: ServerState::Exited,
                pid: None,
                started_at: None,
                exit_code,
                exited_at,
                owned: false,
                message: Some(format!("服务端意外退出（退出码 {code}），不会自动重启，请查看日志后手动启动")),
            };
        }
        ServerStatus {
            state: ServerState::Stopped,
            pid: None,
            started_at: None,
            exit_code,
            exited_at,
            owned: false,
            message: None,
        }
    }

    /// 启动服务端。
    pub fn start(&self, spec: &ServerSpec) -> Result<ServerStatus, VoiceError> {
        if spec.program.as_os_str().is_empty() {
            return Err(VoiceError::Config("未设置启动程序".into()));
        }
        let mut inner = self.lock();
        if inner.running.is_some() {
            return Err(VoiceError::AlreadyRunning);
        }
        if port_in_use(spec.port) {
            return Err(VoiceError::ExternalRunning);
        }
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|error| VoiceError::Spawn(error.to_string()))?;
        let job = job::JobHandle::for_child(&child);
        let pid = child.id();
        for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)]
            .into_iter()
            .flatten()
        {
            let log = self.log.clone();
            let _ = std::thread::Builder::new().name("voice-server-log".into()).spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut buffer = Vec::new();
                loop {
                    buffer.clear();
                    match reader.read_until(b'\n', &mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let _ = log.write_line(&String::from_utf8_lossy(&buffer));
                        }
                    }
                }
            });
        }
        let started_at = unix_now();
        let stopping = Arc::new(AtomicBool::new(false));
        inner.running = Some(Running { child, pid, started_at, job, stopping });
        inner.last_exit = None;
        drop(inner);
        let _ = self.log.write_line(&format!("[MOSS] 已启动服务端，进程 {pid}"));
        self.spawn_monitor(pid);
        Ok(self.status(spec.port))
    }

    /// 后台检查子进程是否退出：意外退出记录退出码，不自动重启。
    fn spawn_monitor(&self, pid: u32) {
        let inner = self.inner.clone();
        let log = self.log.clone();
        let _ = std::thread::Builder::new().name("voice-server-monitor".into()).spawn(move || loop {
            std::thread::sleep(Duration::from_millis(200));
            let mut guard = inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(running) = guard.running.as_mut().filter(|running| running.pid == pid) else {
                break;
            };
            match running.child.try_wait() {
                Ok(Some(status)) => {
                    let by_user = running.stopping.load(Ordering::SeqCst);
                    guard.last_exit = Some(LastExit { code: status.code(), at: unix_now(), by_user });
                    guard.running = None;
                    drop(guard);
                    let code = status.code().map(|code| code.to_string()).unwrap_or_else(|| "未知".into());
                    let text = if by_user {
                        format!("[MOSS] 服务端已停止（退出码 {code}）")
                    } else {
                        format!("[MOSS] 服务端意外退出（退出码 {code}），不会自动重启")
                    };
                    let _ = log.write_line(&text);
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
        });
    }

    /// 停止 MOSS 启动的服务端：先请求正常退出，超时后强制结束（连同子进程树）。
    pub fn stop(&self, port: u16) -> Result<ServerStatus, VoiceError> {
        let pid = {
            let inner = self.lock();
            match &inner.running {
                Some(running) => {
                    running.stopping.store(true, Ordering::SeqCst);
                    running.pid
                }
                None => {
                    drop(inner);
                    return Err(if port_in_use(port) { VoiceError::NotOwned } else { VoiceError::NotRunning });
                }
            }
        };
        request_graceful_exit(pid);
        let deadline = Instant::now() + self.graceful_timeout;
        loop {
            {
                let mut inner = self.lock();
                let Some(running) = inner.running.as_mut().filter(|running| running.pid == pid) else {
                    break; // 监控线程已记录退出
                };
                if let Ok(Some(status)) = running.child.try_wait() {
                    inner.last_exit = Some(LastExit { code: status.code(), at: unix_now(), by_user: true });
                    inner.running = None;
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = running.child.kill();
                    if let Some(job) = running.job.as_ref() {
                        job.terminate();
                    }
                    let code = running.child.wait().ok().and_then(|status| status.code());
                    inner.last_exit = Some(LastExit { code, at: unix_now(), by_user: true });
                    inner.running = None;
                    drop(inner);
                    let _ = self.log.write_line("[MOSS] 服务端未在限定时间内退出，已强制结束");
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(self.status(port))
    }

    /// 是否有 MOSS 启动的实例在运行（退出时决定是否询问）。
    pub fn owns_running_server(&self) -> bool {
        self.lock().running.is_some()
    }

    /// MOSS 退出且用户选择保留服务端时调用：解除 Job Object 的“随 MOSS 关闭”，之后不再管理该进程。
    /// 返回被放开的进程号。之后服务端的输出不再写入 MOSS 的日志。
    pub fn detach(&self) -> Option<u32> {
        let running = self.lock().running.take()?;
        let pid = running.pid;
        if let Some(job) = running.job.as_ref() {
            if !job.release_on_close() {
                let _ = self.log.write_line("[MOSS] 无法解除 Job Object 关联，MOSS 退出时服务端仍会结束");
            }
        }
        let _ = self.log.write_line(&format!("[MOSS] 退出 MOSS 时保留服务端运行（进程 {pid}），之后其输出不再写入本日志"));
        // 丢弃 Child 句柄不会结束进程；监控线程发现 running 为空后自行退出
        drop(running);
        Some(pid)
    }
}

/// 请求子进程正常退出。Unix 发 SIGTERM；Windows 用 `taskkill`（不带 /F）通知进程树。
fn request_graceful_exit(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// 假服务端（不启动真实 xiaozhi-server）：Windows 用 cmd，其他平台用 sh。
    fn fake_spec(dir: &std::path::Path, unix: &str, windows: &str, port: u16) -> ServerSpec {
        if cfg!(windows) {
            ServerSpec { program: "cmd".into(), args: vec!["/C".into(), windows.into()], cwd: dir.into(), port }
        } else {
            ServerSpec { program: "sh".into(), args: vec!["-c".into(), unix.into()], cwd: dir.into(), port }
        }
    }

    fn supervisor(dir: &std::path::Path) -> Supervisor {
        Supervisor::new(RotatingLog::new(dir.join("logs").join("xiaozhi-server.log")))
            .with_graceful_timeout(Duration::from_millis(800))
    }

    fn wait_for(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// A-62：启动、拒绝重复启动、只停止自己启动的进程；日志脱敏。
    #[test]
    fn start_stop_and_single_instance() {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let supervisor = supervisor(dir.path());
        assert_eq!(supervisor.status(port).state, ServerState::Stopped);
        assert_eq!(supervisor.stop(port).unwrap_err(), VoiceError::NotRunning);

        let spec = fake_spec(
            dir.path(),
            "echo 'api_key: sk-test-secret'; sleep 30",
            "echo api_key: sk-test-secret & ping -n 30 127.0.0.1 >nul",
            port,
        );
        let status = supervisor.start(&spec).unwrap();
        assert_eq!(status.state, ServerState::Running);
        assert!(status.owned && status.pid.is_some());
        assert!(supervisor.owns_running_server());
        assert_eq!(supervisor.start(&spec).unwrap_err(), VoiceError::AlreadyRunning);
        assert!(wait_for(|| supervisor.log().tail(20).iter().any(|line| line.contains("api_key: ***"))));

        let stopped = supervisor.stop(port).unwrap();
        assert_eq!(stopped.state, ServerState::Stopped, "{stopped:?}");
        assert!(!supervisor.owns_running_server());
        let text = std::fs::read_to_string(dir.path().join("logs").join("xiaozhi-server.log")).unwrap();
        assert!(!text.contains("sk-test-secret"), "{text}");
        // 用户停止不算崩溃，也不会自动重启
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(supervisor.status(port).state, ServerState::Stopped);
    }

    /// A-63：外部已运行的服务端标为“外部运行”，不能停止，也不再启动一个。
    #[test]
    fn external_instance_is_not_started_or_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let supervisor = supervisor(dir.path());
        let status = supervisor.status(port);
        assert_eq!(status.state, ServerState::External);
        assert!(!status.owned);
        let spec = fake_spec(dir.path(), "sleep 30", "ping -n 30 127.0.0.1 >nul", port);
        let error = supervisor.start(&spec).unwrap_err();
        assert_eq!(error, VoiceError::ExternalRunning);
        assert_eq!(error.http_status(), 409);
        let error = supervisor.stop(port).unwrap_err();
        assert_eq!(error, VoiceError::NotOwned);
        assert_eq!(error.code(), "server_not_owned");
        assert!(!supervisor.owns_running_server());
        drop(listener);
    }

    /// A-65：意外退出时记录退出码与日志，不自动重启。
    #[test]
    fn crash_is_recorded_and_not_restarted() {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let supervisor = supervisor(dir.path());
        let spec = fake_spec(dir.path(), "echo boom; exit 3", "echo boom & exit 3", port);
        supervisor.start(&spec).unwrap();
        assert!(wait_for(|| supervisor.status(port).state == ServerState::Exited));
        let status = supervisor.status(port);
        assert_eq!(status.exit_code, Some(3));
        assert!(status.message.unwrap().contains("不会自动重启"));
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(supervisor.status(port).state, ServerState::Exited, "不得自动重启");
        assert!(supervisor.log().tail(20).iter().any(|line| line.contains("boom")));
        assert!(supervisor.log().tail(20).iter().any(|line| line.contains("意外退出")));
        // 手动再次启动可以
        let again = fake_spec(dir.path(), "sleep 30", "ping -n 30 127.0.0.1 >nul", port);
        assert_eq!(supervisor.start(&again).unwrap().state, ServerState::Running);
        supervisor.stop(port).unwrap();
    }

    /// A-64：退出时选择保留服务端：进程继续运行且不再归 MOSS 管理。
    #[test]
    fn detach_leaves_server_running_and_unmanaged() {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let supervisor = supervisor(dir.path());
        assert_eq!(supervisor.detach(), None);
        let spec = fake_spec(dir.path(), "sleep 30", "ping -n 30 127.0.0.1 >nul", port);
        supervisor.start(&spec).unwrap();
        let pid = supervisor.detach().expect("有运行中的实例");
        assert!(!supervisor.owns_running_server());
        assert_eq!(supervisor.stop(port).unwrap_err(), VoiceError::NotRunning);
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(supervisor.status(port).state, ServerState::Stopped, "放开后不算崩溃");
        assert!(supervisor.log().tail(10).iter().any(|line| line.contains("保留服务端运行")));
        // 清理：测试自己结束这个假进程
        if cfg!(windows) {
            let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status();
        } else {
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
        }
    }

    #[test]
    fn spec_from_settings_reports_missing_parts() {
        let dir = tempfile::tempdir().unwrap();
        let mut voice = VoiceSettings::default();
        assert!(matches!(ServerSpec::from_settings(&voice), Err(VoiceError::Config(_))));
        voice.server_dir = Some(dir.path().to_string_lossy().into_owned());
        assert!(matches!(ServerSpec::from_settings(&voice), Err(VoiceError::Config(_))));
        voice.python = Some("python".into());
        let error = ServerSpec::from_settings(&voice).unwrap_err();
        assert_eq!(error.code(), "server_config_missing");
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(dir.path().join("data").join(".config.yaml"), "selected_module: {}\n").unwrap();
        let spec = ServerSpec::from_settings(&voice).unwrap();
        assert_eq!(spec.args, vec!["app.py".to_string()]);
        assert_eq!(spec.port, 8000);
        assert_eq!(spec.cwd, dir.path());
    }
}
