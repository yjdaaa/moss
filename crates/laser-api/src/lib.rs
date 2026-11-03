use base64::Engine;
use sha1::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;

const JEV_API_KEY_SECRET: &str = "jev_api_key";
const DEFAULT_JEV_ENDPOINT: &str = "https://openrouter.ai/api/alpha/decisions";
const JEV_MODEL: &str = "typesafe/jev-1.13";
pub mod approval;
pub mod agent_runs;
pub mod external_tools;
mod feedback;
mod voice_authority;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../frontend/dist/"]
pub struct FrontendFixture;

pub struct ApiService {
    settings: laser_runtime::LaserSettings,
    materials: laser_contracts::MaterialLibrary,
    upload_root: Option<std::path::PathBuf>,
    preview_roots: Vec<std::path::PathBuf>,
    auth: Option<std::sync::Arc<AuthManager>>,
    workflows:
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    /// 无 SQLite 时的标定矩阵内存表（与 workflows 同构，按 calibration_id 键控）。
    calibrations:
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    store: Option<std::sync::Arc<dyn laser_runtime::RuntimeStore + Send + Sync>>,
    runtime_db: Option<std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    generated_root: Option<std::path::PathBuf>,
    actual_port: std::sync::Arc<std::sync::atomic::AtomicU16>,
    runtime_path: Option<std::path::PathBuf>,
    active_jobs: ActiveJobs,
    /// 已结束但物理停机未获确认的任务：保留原设备实时通道供 0x18 重试，绝不猜测其他目标。
    uncertain_jobs: ActiveJobs,
    feedback_adoption: std::sync::Mutex<()>,
    transport_factory: TransportFactory,
    /// `MOSS_ALLOWED_ORIGINS` 额外允许的浏览器来源（精确匹配，已拒绝 `*`）。
    extra_allowed_origins: Vec<String>,
    /// 系统串口枚举：默认查询本机真实串口；测试注入固定列表，避免结果随插拔的设备变化。
    serial_port_lister: SerialPortLister,
    /// 严格模式（`MOSS_FUSION_STRICT`，技术书 §6.2）：开启后 R3 动作需要桌面会话或审批授权。
    /// 语音中心切换链路时可在运行中改变（自建模式必须开启），因此用原子量。
    strict_mode: std::sync::atomic::AtomicBool,
    /// 桌面审批服务（有本地数据库时可用）。
    approvals: Option<std::sync::Arc<approval::ApprovalService>>,
    /// 桌面会话（`X-Moss-Desktop-Session`，技术书 §5.3）：只有桌面壳调用 `enable_desktop_session` 后才存在。
    desktop_session: std::sync::OnceLock<String>,
    agent_runs: std::sync::OnceLock<std::sync::Arc<dyn agent_runs::AgentRuns>>,
    external_tools: std::sync::OnceLock<std::sync::Arc<dyn external_tools::ExternalTools>>,
    /// 用户关闭的工具（`settings.json` 的 `mcp_disabled_tools`，NV8）：接入点与内置语音都不列出、不可调用。
    disabled_tools: std::sync::RwLock<std::collections::BTreeSet<String>>,
    voice_sessions: voice_authority::VoiceSessions,
}

/// 作业结束回调：发送线程在写回终态前调用（状态 `completed` / `cancelled` / `failed`，以及 `send_result`）。
pub type JobTerminalHook = Box<dyn FnOnce(&str, &serde_json::Value) + Send + 'static>;

/// 串口枚举函数类型（见 [`ApiService::with_serial_port_lister`]）。
pub type SerialPortLister = std::sync::Arc<dyn Fn() -> Vec<String> + Send + Sync>;

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
        serde_json::json!({
            "success": true,
            "running": true,
            "phase": if self.progress.awaiting_completion.load(Ordering::Acquire) { "awaiting_completion" } else { "streaming" },
            "mode": self.mode,
            "target": self.target,
            "lines_sent": self.progress.lines_sent.load(Ordering::Relaxed),
            "total_lines": self.progress.total_lines.load(Ordering::Relaxed),
            "elapsed_ms": self.started_at.elapsed().as_millis() as u64,
        })
    }
}

pub type ActiveJobs = std::sync::Arc<std::sync::Mutex<HashMap<String, std::sync::Arc<ActiveJob>>>>;

/// 发送启动器：门禁通过后由 workflow 处理函数回调，负责真实连接、握手并启动后台作业。
/// 返回 `send_result` JSON；失败返回 (HTTP 状态码, 失败响应体)。
type SendLauncher<'a> = &'a dyn Fn(
    &str,
    std::path::PathBuf,
    &str,
    &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, (u16, serde_json::Value)>;

/// 前端页面路由：刷新这些地址时返回 index.html，由前端决定显示哪一页（语音中心只在桌面端显示）。
fn is_spa_route(route: &str) -> bool {
    matches!(route, "/draw" | "/material-lab" | "/voice" | "/twin") || route.starts_with("/draw/")
}

/// SD 脱机：固件没有返回文件哈希时拒绝 `$SD/Run`（无法确认机器端文件就是确认过的刀路）。
const SD_HASH_UNAVAILABLE: &str = "固件没有返回 SD 文件的哈希（未启用 HashFS），无法确认机器端文件与确认的刀路一致，已拒绝启动；请在固件中启用 HashFS，或改用逐行发送";

fn default_transport_factory() -> TransportFactory {
    std::sync::Arc::new(|spec: &laser_device::ConnectionSpec| {
        laser_device::open_transport(spec, std::time::Duration::from_secs(3))
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct PairChallenge {
    pub code: String,
    pub expires_at: u64,
}

#[derive(Debug)]
struct PairState {
    code: String,
    expires_at: u64,
    used: bool,
    failures: HashMap<String, (u32, u64)>,
    locked_until: HashMap<String, u64>,
    tokens: HashMap<String, String>,
}

#[derive(Debug)]
pub struct AuthManager {
    state: std::sync::Mutex<PairState>,
}

impl AuthManager {
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(PairState {
                code: String::new(),
                expires_at: 0,
                used: false,
                failures: HashMap::new(),
                locked_until: HashMap::new(),
                tokens: HashMap::new(),
            }),
        }
    }

    pub fn issue_code(&self, now: u64) -> Result<PairChallenge, String> {
        let mut random = [0u8; 4];
        getrandom::fill(&mut random).map_err(|error| error.to_string())?;
        let number = u32::from_le_bytes(random) % 1_000_000;
        let code = format!("{number:06}");
        let challenge = PairChallenge {
            code: code.clone(),
            expires_at: now + 300,
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "auth state lock poisoned".to_string())?;
        state.code = code;
        state.expires_at = challenge.expires_at;
        state.used = false;
        state.failures.clear();
        state.locked_until.clear();
        Ok(challenge)
    }

    pub fn pair(
        &self,
        source_ip: &str,
        code: &str,
        now: u64,
    ) -> Result<(String, String), AuthError> {
        let mut state = self.state.lock().map_err(|_| AuthError::Internal)?;
        if state
            .locked_until
            .get(source_ip)
            .is_some_and(|until| *until > now)
        {
            return Err(AuthError::Locked(state.locked_until[source_ip] - now));
        }
        let valid = !state.used && now < state.expires_at && code == state.code;
        if !valid {
            let entry = state
                .failures
                .entry(source_ip.to_string())
                .or_insert((0, now));
            entry.0 += 1;
            if entry.0 >= 5 {
                state.locked_until.insert(source_ip.to_string(), now + 900);
            }
            return Err(AuthError::Invalid);
        }
        state.used = true;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| AuthError::Internal)?;
        let token = base64::engine::general_purpose::STANDARD.encode(bytes);
        let token_hash = hex::encode(Sha256::digest(bytes));
        let pairing_id = format!("pair-{}", &token_hash[..12]);
        state.tokens.insert(token_hash, pairing_id.clone());
        Ok((token, pairing_id))
    }

    pub fn authorize(&self, bearer: &str) -> Result<String, AuthError> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(bearer)
            .map_err(|_| AuthError::Unauthorized)?;
        let hash = hex::encode(Sha256::digest(raw));
        self.state
            .lock()
            .map_err(|_| AuthError::Internal)?
            .tokens
            .get(&hash)
            .cloned()
            .ok_or(AuthError::Unauthorized)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Invalid,
    Locked(u64),
    Unauthorized,
    Internal,
}

const PREVIEW_SUFFIXES: [&str; 9] = [
    ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp", ".gcode", ".nc", ".json",
];
const UPLOAD_SUFFIXES: [&str; 8] = [
    ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp", ".gcode", ".nc",
];

pub fn resolve_preview_file(
    requested: &std::path::Path,
    allowed_roots: &[std::path::PathBuf],
) -> Result<std::path::PathBuf, String> {
    let suffix = requested
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    if !PREVIEW_SUFFIXES.contains(&suffix.as_str()) {
        return Err("不支持预览该文件类型".into());
    }

    // 1. 如果请求的是相对文件名或相对子路径，优先在允许的根目录列表中拼接查找
    for root in allowed_roots {
        let candidate = root.join(requested);
        if let Ok(resolved) = std::fs::canonicalize(&candidate) {
            if let Ok(canonical_root) = std::fs::canonicalize(root) {
                if resolved.starts_with(canonical_root) {
                    return Ok(resolved);
                }
            }
        }
    }

    // 2. 如果请求的是完整路径，校验是否位于任一允许的根目录内
    if let Ok(resolved) = std::fs::canonicalize(requested) {
        if allowed_roots.iter().any(|root| {
            std::fs::canonicalize(root)
                .ok()
                .is_some_and(|canonical_root| resolved.starts_with(canonical_root))
        }) {
            return Ok(resolved);
        }
    }

    Err("文件不存在或不在允许的预览目录内".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub path: std::path::PathBuf,
    pub filename: String,
    pub size: usize,
    pub suffix: String,
}

pub fn save_upload(
    filename: &str,
    encoded: &str,
    upload_root: &std::path::Path,
    unix_seconds: u64,
) -> Result<UploadResult, String> {
    let basename = filename.rsplit(['\\', '/']).next().unwrap_or(filename);
    let cleaned = basename
        .replace(['<', '>', ':', '"', '|', '?', '*'], "_")
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let path = std::path::Path::new(&cleaned);
    let suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    if !UPLOAD_SUFFIXES.contains(&suffix.as_str()) {
        return Err("不支持上传该文件类型".into());
    }
    let raw = encoded
        .split_once(',')
        .map(|(_, value)| value)
        .unwrap_or(encoded);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| "上传内容不是合法 base64".to_string())?;
    if bytes.is_empty() {
        return Err("上传文件为空".into());
    }
    let digest = sha1::Sha1::digest(&bytes);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("upload")
        .chars()
        .take(32)
        .collect::<String>();
    let filename = format!(
        "{unix_seconds}_{}_{stem}{suffix}",
        hex::encode(&digest)[..10].to_string()
    );
    std::fs::create_dir_all(upload_root).map_err(|error| error.to_string())?;
    let destination = upload_root.join(&filename);
    std::fs::write(&destination, &bytes).map_err(|error| error.to_string())?;
    Ok(UploadResult {
        path: destination,
        filename,
        size: bytes.len(),
        suffix,
    })
}

impl ApiService {
    pub fn new(
        settings: laser_runtime::LaserSettings,
        materials: laser_contracts::MaterialLibrary,
    ) -> Self {
        Self {
            settings,
            materials,
            upload_root: None,
            preview_roots: Vec::new(),
            auth: None,
            workflows: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::BTreeMap::new()),
            ),
            calibrations: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::BTreeMap::new()),
            ),
            store: None,
            runtime_db: None,
            generated_root: None,
            actual_port: std::sync::Arc::new(std::sync::atomic::AtomicU16::new(0)),
            runtime_path: None,
            active_jobs: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            uncertain_jobs: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            feedback_adoption: std::sync::Mutex::new(()),
            transport_factory: default_transport_factory(),
            extra_allowed_origins: Vec::new(),
            serial_port_lister: std::sync::Arc::new(enumerate_system_serial_ports),
            strict_mode: std::sync::atomic::AtomicBool::new(false),
            approvals: None,
            desktop_session: std::sync::OnceLock::new(),
            agent_runs: std::sync::OnceLock::new(),            external_tools: std::sync::OnceLock::new(),
            disabled_tools: std::sync::RwLock::new(std::collections::BTreeSet::new()),
            voice_sessions: Default::default(),
        }
    }

    /// 严格模式开关（来自 `MOSS_FUSION_STRICT`，见 [`laser_runtime::config::fusion_strict_from_env`]）。
    pub fn with_strict_mode(self, strict: bool) -> Self {
        self.strict_mode.store(strict, std::sync::atomic::Ordering::SeqCst);
        self
    }

    pub fn strict_mode(&self) -> bool {
        self.strict_mode.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 运行中切换严格模式（语音中心切到自建链路时开启，切回官方时恢复 `.env` 的设置，技术书 §9.4.2）。
    pub fn set_strict_mode(&self, strict: bool) {
        self.strict_mode.store(strict, std::sync::atomic::Ordering::SeqCst);
    }

    /// 用户是否关闭了工具 `name`（NV8）。
    pub fn tool_disabled(&self, name: &str) -> bool {
        self.disabled_tools.read().map(|set| set.contains(name)).unwrap_or(false)
    }

    /// 当前关闭的工具（按名称排序）。
    pub fn disabled_tools(&self) -> Vec<String> {
        self.disabled_tools.read().map(|set| set.iter().cloned().collect()).unwrap_or_default()
    }

    /// 运行中替换关闭的工具列表（语音中心写入 `settings.json` 后调用）。不能关闭的工具与空名会被忽略。
    pub fn set_disabled_tools<I: IntoIterator<Item = String>>(&self, names: I) {
        let names = names
            .into_iter()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty() && laser_contracts::tool_can_be_disabled(name))
            .collect();
        if let Ok(mut set) = self.disabled_tools.write() {
            *set = names;
        }
    }

    /// 替换审批服务（测试注入固定时钟用）；生产环境由 [`Self::with_runtime_db`] 自动创建。
    pub fn with_approval_service(mut self, service: std::sync::Arc<approval::ApprovalService>) -> Self {
        self.approvals = Some(service);
        self
    }

    pub fn approvals(&self) -> Option<&std::sync::Arc<approval::ApprovalService>> {
        self.approvals.as_ref()
    }

    /// 进程启动时调用：所有 pending / approved 审批一律过期（技术书 §5.5、A-32）。
    pub fn recover_approvals_after_restart(&self) -> usize {
        match self.approvals.as_ref().map(|service| service.recover_after_restart()) {
            Some(Ok(count)) => count,
            Some(Err(error)) => {
                eprintln!("[laser-api] 启动时处理遗留审批失败: {}", error.message());
                0
            }
            None => 0,
        }
    }

    /// 替换系统串口枚举（测试用；生产环境使用默认的真实枚举）。
    pub fn with_serial_port_lister(mut self, lister: SerialPortLister) -> Self {
        self.serial_port_lister = lister;
        self
    }

    /// 额外允许的浏览器来源（来自 `MOSS_ALLOWED_ORIGINS`，见 [`laser_runtime::config::parse_allowed_origins`]）。
    pub fn with_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.extra_allowed_origins = origins;
        self
    }

    pub fn with_transport_factory(mut self, factory: TransportFactory) -> Self {
        self.transport_factory = factory;
        self
    }

    /// 当前正在下发的作业数量。
    pub fn active_job_count(&self) -> usize {
        self.active_jobs.lock().map(|jobs| jobs.len()).unwrap_or(0)
    }

    /// 退出/崩溃兜底：对所有活动作业置取消并直接写 0x18（实时信道 + 裸句柄双路）。
    pub fn abort_all_active_jobs(&self) -> Result<(), String> {
        let mut jobs: Vec<std::sync::Arc<ActiveJob>> = match self.active_jobs.lock() {
            Ok(guard) => guard.values().cloned().collect(),
            Err(_) => return Err("active_jobs lock poisoned".into()),
        };
        // 任务已报错并不代表机床停机；退出时也应尝试停止仍不确定的原目标。
        match self.uncertain_jobs.lock() {
            Ok(guard) => jobs.extend(guard.values().cloned()),
            Err(_) => return Err("uncertain_jobs lock poisoned".into()),
        }
        let mut first_error = None;
        for job in jobs {
            job.cancel.cancel();
            if let Err(e) = job.transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop) {
                first_error.get_or_insert(e);
            }
            if let Some(raw) = job.transport.raw_abort_handle() {
                let _ = raw();
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// 处理安全动作请求：优先复用当前正在运行的任务信道（边刻边调/急停），
    /// 避免正在雕刻时重新打开串口/网络引发的设备冲突。
    pub fn handle_safe_action(
        &self,
        args: &laser_contracts::LaserSafeActionArgs,
        _ctx: &laser_contracts::CallerContext,
    ) -> laser_contracts::ToolCallResult {
        self.handle_safe_action_for(args, _ctx, None)
    }

    fn handle_safe_action_for(&self, args: &laser_contracts::LaserSafeActionArgs, _ctx: &laser_contracts::CallerContext, job_id: Option<&str>) -> laser_contracts::ToolCallResult {
        if !self.voice_caller_allowed(_ctx) {
            return laser_contracts::ToolCallResult::with_detail(false, "语音设备授权已失效，请重新连接", "voice_session_revoked");
        }
        let action = args.action.trim();
        if job_id.is_some() && !matches!(action, "emergency_stop" | "soft_reset" | "status") {
            return laser_contracts::ToolCallResult::err("job_id 只用于查询或停止任务，不能改变其他动作的目标");
        }
        let realtime_cmd_info = match action {
            "emergency_stop" | "soft_reset" => Some((laser_device::RealtimeCommand::EmergencyStop, "急停 0x18", true)),
            "feed_override_reset" | "feed_reset" | "feed_100" => Some((laser_device::RealtimeCommand::FeedOvrReset, "进给速度恢复100% (0x90)", false)),
            "feed_override_plus_10" | "feed_plus_10" | "feed_+10%" => Some((laser_device::RealtimeCommand::FeedOvrCoarsePlus, "进给速度增加10% (0x91)", false)),
            "feed_override_minus_10" | "feed_minus_10" | "feed_-10%" => Some((laser_device::RealtimeCommand::FeedOvrCoarseMinus, "进给速度降低10% (0x92)", false)),
            "feed_override_plus_1" | "feed_plus_1" | "feed_+1%" => Some((laser_device::RealtimeCommand::FeedOvrFinePlus, "进给速度增加1% (0x93)", false)),
            "feed_override_minus_1" | "feed_minus_1" | "feed_-1%" => Some((laser_device::RealtimeCommand::FeedOvrFineMinus, "进给速度降低1% (0x94)", false)),
            "spindle_override_reset" | "power_override_reset" | "power_reset" | "power_100" => Some((laser_device::RealtimeCommand::SpindleOvrReset, "激光功率恢复100% (0x99)", false)),
            "spindle_override_plus_10" | "power_override_plus_10" | "power_plus_10" | "power_+10%" => Some((laser_device::RealtimeCommand::SpindleOvrCoarsePlus, "激光功率增加10% (0x9A)", false)),
            "spindle_override_minus_10" | "power_override_minus_10" | "power_minus_10" | "power_-10%" => Some((laser_device::RealtimeCommand::SpindleOvrCoarseMinus, "激光功率降低10% (0x9B)", false)),
            "spindle_override_plus_1" | "power_override_plus_1" | "power_plus_1" | "power_+1%" => Some((laser_device::RealtimeCommand::SpindleOvrFinePlus, "激光功率增加1% (0x9C)", false)),
            "spindle_override_minus_1" | "power_override_minus_1" | "power_minus_1" | "power_-1%" => Some((laser_device::RealtimeCommand::SpindleOvrFineMinus, "激光功率降低1% (0x9D)", false)),
            _ => None,
        };

        let explicit_target = if args.host.as_deref().is_some_and(|s| !s.trim().is_empty()) || args.port.as_deref().is_some_and(|s| !s.trim().is_empty()) {
            match laser_tools::resolve_connection_spec(args.port.as_deref(), args.host.as_deref(), args.baudrate, args.telnet_port, args.connection_mode.as_deref()) {
                Ok(spec) => Some(spec.describe()),
                Err(error) => return laser_contracts::ToolCallResult::err(error),
            }
        } else { None };
        let (active_job, uncertain_stop) = {
            let Ok(jobs) = self.active_jobs.lock() else { return laser_contracts::ToolCallResult::err("无法读取设备任务，请使用实体急停"); };
            let Ok(uncertain) = self.uncertain_jobs.lock() else { return laser_contracts::ToolCallResult::err("无法读取待确认停止的设备任务，请使用实体急停"); };
            if let Some(id) = job_id {
                let running = jobs.values().find(|j| j.workflow_id == id).cloned();
                let unknown = running.is_none();
                match running.or_else(|| uncertain.values().find(|j| j.workflow_id == id).cloned()) {
                    Some(job) if explicit_target.as_ref().is_none_or(|target| target == &job.target) => (Some(job), unknown),
                    Some(_) => return laser_contracts::ToolCallResult::err("任务编号与指定设备不一致，停止未发送，请检查目标或使用实体急停"),
                    None => return laser_contracts::ToolCallResult::err("找不到此任务的设备连接；没有向其他设备发送停止，请使用实体急停"),
                }
            } else if let Some(target) = explicit_target.as_ref() {
                (jobs.values().find(|j| &j.target == target).cloned(), false)
            } else if jobs.len() > 1 {
                return laser_contracts::ToolCallResult::err("存在多个设备任务，请指定停止目标；紧急情况请使用实体急停");
            } else { (jobs.values().next().cloned(), false) }
        };
        if let Some(job) = active_job {
            if let Some((cmd, label, alters_alarm)) = realtime_cmd_info {
                if args.dry_run == Some(true) {
                    return laser_contracts::ToolCallResult::ok(format!("仅预览：{label}；未向设备发送"));
                }
                if alters_alarm {
                    job.cancel.cancel();
                    if let Err(error) = job.transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop) {
                        return laser_contracts::ToolCallResult::err(format!("作业取消已请求，但急停 0x18 发送失败：{error}"));
                    }
                    return laser_contracts::ToolCallResult::ok(format!("{label} 已发送到任务的原设备通道（目标 {}{}），不代表实际停机，请确认硬件状态", job.target, if uncertain_stop { "，此前任务结果未知" } else { "" }));
                } else {
                    match job.transport.write_realtime(cmd) {
                        Ok(()) => return laser_contracts::ToolCallResult::ok(format!("{label} 已实时发送到正在运行的加工任务（目标 {}），即时生效（不停机）", job.target)),
                        Err(e) => return laser_contracts::ToolCallResult::err(format!("{label} 发送失败（目标 {}）: {e}", job.target)),
                    }
                }
            } else if action == "status" {
                if uncertain_stop {
                    return laser_contracts::ToolCallResult::with_detail(false, "任务执行结果和停机状态未知，请使用急停重试并到现场确认", "job_result_unknown");
                }
                let _ = job.transport.write_realtime(laser_device::RealtimeCommand::StatusQuery);
                let sent = job.progress.lines_sent.load(std::sync::atomic::Ordering::Relaxed);
                let total = job.progress.total_lines.load(std::sync::atomic::Ordering::Relaxed);
                return laser_contracts::ToolCallResult::with_detail(
                    true,
                    format!("设备正在加工中（目标 {}），已确认 {}/{} 行", job.target, sent, total),
                    "active_job_streaming",
                );
            } else {
                return laser_contracts::ToolCallResult::err(format!("设备正在执行加工任务（目标 {}），禁止下发 {action} 等顺序指令（请先等待加工完成或执行急停）", job.target));
            }
        }

        let factory = self.transport_factory.clone();
        let opener: laser_tools::TransportOpener = std::sync::Arc::new(move |spec: &laser_device::ConnectionSpec, _timeout: std::time::Duration| {
            (factory)(spec)
        });
        let permit = match self.voice_permit(_ctx) {
            Ok(permit) => permit,
            Err(_) => return laser_contracts::ToolCallResult::err("语音设备授权已失效，请重新连接"),
        };
        laser_tools::execute_laser_safe_action_with_gate(args, &opener, &|| permit.as_ref().is_none_or(|p| p.load(std::sync::atomic::Ordering::Acquire)))
    }

    /// 走边框：真实驱动机器沿外接矩形运动，按发送链路对待（2026-10-14）。
    /// 与完整发送相同的设备占用检查、握手与 Idle 预检；运行期间登记为活动作业（`framing-…`），
    /// 顶栏 / 页面急停与取消都能找到它。默认不出光（`power` 缺省 0），弱光指示需显式传 `power`。
    fn run_framing(&self, object: &serde_json::Map<String, serde_json::Value>, ctx: &laser_contracts::CallerContext) -> Vec<u8> {
        use std::sync::atomic::Ordering;
        let permit = match self.voice_permit(ctx) { Ok(permit) => permit, Err((status,body)) => return http_json(status,body) };
        let min_x = object.get("min_x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let min_y = object.get("min_y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let max_x = object.get("max_x").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let max_y = object.get("max_y").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let feed_rate = object.get("feed_rate").and_then(|v| v.as_i64()).unwrap_or(2000) as i32;
        let power = object.get("power").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

        let framing_gcode = match laser_core::gcode::generate_framing_gcode(min_x, min_y, max_x, max_y, feed_rate, power) {
            Ok(g) => g,
            Err(e) => return http_json(400, failure(format!("生成走边框 G-code 失败: {e}"), None)),
        };
        let lines: Vec<String> = match laser_device::prepare_gcode_lines(&framing_gcode) {
            Ok(p) => p.lines,
            Err(e) => return http_json(400, failure(format!("走边框 G-code 校验失败: {e}"), None)),
        };

        let spec = match self.connection_spec_from_request(object) {
            Ok(s) => s,
            Err(e) => return http_json(400, failure(format!("连接参数无效: {e}"), Some(serde_json::json!({"error_code": "connection_params_invalid"})))),
        };
        let target = spec.describe();
        if let Ok(jobs) = self.active_jobs.lock() {
            if let Some(job) = jobs.values().find(|job| job.target == target) {
                return http_json(409, failure(
                    format!("设备 {target} 正在执行 {}，请等待完成或先取消", job.workflow_id),
                    Some(serde_json::json!({"error_code": "device_busy"})),
                ));
            }
        }
        let transport: std::sync::Arc<dyn laser_device::DeviceTransport> = match (self.transport_factory)(&spec) {
            Ok(t) => std::sync::Arc::from(t),
            Err(e) => return http_json(400, failure(format!("设备连接失败 ({target}): {e}"), Some(serde_json::json!({"error_code": "device_unreachable"})))),
        };
        let mut device = laser_device::DeviceConnection::new(transport.clone());
        if let Err(e) = device.handshake_with_deadline(std::time::Duration::from_secs(3)) {
            return http_json(400, failure(format!("设备握手失败 ({target}): {e}"), Some(serde_json::json!({"error_code": "device_not_ready"}))));
        }
        device.load_features(std::time::Duration::from_secs(3));
        let device_scale = match device.verify_job_ready_adaptive(std::time::Duration::from_secs(3)) {
            Ok(scale) => scale,
            Err(e) => return http_json(400, failure(format!("走边框前检查未通过 ({target}): {e}"), Some(serde_json::json!({"error_code": "device_preflight_failed"})))),
        };
        if let Ok(mut uncertain) = self.uncertain_jobs.lock() {
            uncertain.retain(|_, old| old.target != target);
        }
        if permit.as_ref().is_some_and(|p| !p.load(Ordering::Acquire)) {
            return http_json(403, failure("预检期间设备授权已撤销，未开始走边框", Some(serde_json::json!({"error_code":"voice_session_revoked"}))));
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
        if let Ok(mut jobs) = self.active_jobs.lock() {
            jobs.insert(job_id.clone(), job.clone());
        }
        let mut worker = laser_device::JobWorker::with_progress(device, cancel.clone(), progress)
            .with_scale_factor(device_scale / 1000.0);
        let outcome = worker.send_lines(lines);
        if let Ok(mut jobs) = self.active_jobs.lock() {
            jobs.remove(&job_id);
        }
        let bounds = serde_json::json!({ "min_x": min_x, "min_y": min_y, "max_x": max_x, "max_y": max_y });
        match outcome {
            Ok(_) if !cancel.is_cancelled() => http_json(200, success(serde_json::json!({
                "message": "走边框完成",
                "job_id": job_id,
                "laser_on": power > 0,
                "bounds": bounds,
            }))),
            result => {
                // 取消或失败都不能证明机器已停：保留原设备通道，供顶栏急停重试 0x18。
                if !cancel.is_cancelled() {
                    let _ = transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
                }
                if let Ok(mut uncertain) = self.uncertain_jobs.lock() {
                    uncertain.insert(job_id.clone(), job);
                }
                match result {
                    Err(e) if !cancel.is_cancelled() => http_json(500, failure(
                        format!("走边框执行失败，已发送急停 0x18，请到设备旁确认: {e}"),
                        Some(serde_json::json!({"error_code": "framing_failed", "job_id": job_id})),
                    )),
                    _ => http_json(409, failure(
                        "走边框已取消，急停 0x18 已发送，请到设备旁确认机器已停",
                        Some(serde_json::json!({"error_code": "framing_cancelled", "job_id": job_id})),
                    )),
                }
            }
        }
    }

    /// 从请求参数构造连接描述（串口留空时自动选择首个枚举到的系统串口）。
    fn connection_spec_from_request(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<laser_device::ConnectionSpec, String> {
        let mode = object
            .get("connection_mode")
            .or_else(|| object.get("preferred_mode"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(self.settings.default_connection_mode.as_str())
            .to_ascii_lowercase();
        if mode == "serial" {
            let mut port = object
                .get("port")
                .or_else(|| object.get("serial_port"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            if port.is_empty() {
                port = (self.serial_port_lister)()
                    .into_iter()
                    .next()
                    .ok_or("未选择串口且未发现任何系统串口，请插入 USB 线并点击刷新")?;
            }
            let baud = parse_int(object.get("baudrate").or_else(|| object.get("baud")), "baudrate", 115_200)
                .map_err(|e| e.to_string())?;
            if !(1200..=4_000_000).contains(&baud) {
                return Err("baudrate 超出合法范围 (1200 ~ 4000000)".into());
            }
            Ok(laser_device::ConnectionSpec::Serial { port, baud_rate: baud as u32 })
        } else {
            let host = object
                .get("network_host")
                .or_else(|| object.get("host"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            if host.is_empty() {
                return Err("请填写设备 host/IP".into());
            }
            let port = parse_int(
                object.get("network_telnet_port").or_else(|| object.get("telnet_port")),
                "network_telnet_port",
                23,
            )
            .map_err(|e| e.to_string())?;
            if !(1..=65535).contains(&port) {
                return Err("network_telnet_port 超出合法范围".into());
            }
            Ok(laser_device::ConnectionSpec::Tcp { host, port: port as u16 })
        }
    }

    /// 真实发送：打开连接 → 有界握手（$32=1 门禁）→ 后台 JobWorker 逐行下发并等待 ok。
    /// 连接或握手失败时同步返回 400，workflow 保持可再次发送。
    fn launch_send_job(
        &self,
        workflow_id: &str,
        gcode_path: std::path::PathBuf,
        expected_sha: &str,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        self.launch_send_job_for(laser_runtime::RecordKind::Workflow, workflow_id, gcode_path, expected_sha, object, ctx)
    }

    /// 与 `launch_send_job` 相同的真实下发链路，但终态写回指定记录类型
    /// （workflow 或标定矩阵 calibration）。作业注册表按记录 id 键控，
    /// 同一设备同时只允许一个在途作业。
    fn launch_send_job_for(
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
            return self.launch_send_job_unchecked(kind, workflow_id, gcode_path, expected_sha, object, None, voice_permit);
        }
        let (approval_kind, label) = match kind {
            laser_runtime::RecordKind::Calibration => (laser_contracts::ApprovalKind::CalibrationSend, "发送标定矩阵"),
            _ => (laser_contracts::ApprovalKind::WorkflowConfirmSend, "发送加工任务"),
        };
        let facts = self.send_digest_facts(kind, expected_sha, workflow_id, object);
        let summary = send_approval_summary(label, workflow_id, &facts);
        let authority = self.require_r3_authority(
            ctx,
            approval::R3Action { kind: approval_kind, target_record: workflow_id, facts, request: object, summary },
        )?;
        let result = match &authority {
            approval::R3Authority::Approval(record) => match record.connection.as_ref() {
                Some(connection) => {
                    let effective = approval::object_with_connection(object, connection);
                    // 先记“发送中”，作业结束时由发送线程写回结果；进程若在此期间中断，
                    // 重启时据此把审批记为“结果未知”，不重放（技术书 §5.5，A-33）
                    let hook: Option<JobTerminalHook> = self.approvals.clone().map(|service| {
                        if let Err(error) = service.mark_running(&record.approval_id, workflow_id) {
                            eprintln!("[laser-api] 审批 {} 记录发送中状态失败: {}", record.approval_id, error.message());
                        }
                        let approval_id = record.approval_id.clone();
                        Box::new(move |status: &str, send_result: &serde_json::Value| {
                            service.record_job_result(&approval_id, status, send_result);
                        }) as JobTerminalHook
                    });
                    self.launch_send_job_unchecked(kind, workflow_id, gcode_path, expected_sha, &effective, hook, voice_permit)
                }
                None => Err(approval::error_response(&approval::ApprovalError::Mismatch)),
            },
            _ => self.launch_send_job_unchecked(kind, workflow_id, gcode_path, expected_sha, object, None, voice_permit),
        };
        // 成功时的结果由发送线程在作业结束时写回；这里只记录没能启动的情况
        if result.is_err() {
            self.finish_r3_authority(&authority, &result, Some(workflow_id));
        }
        result
    }

    /// 发送摘要要素：记录中的材料、厚度、参数 + 门禁核对过的 G-code SHA + 发送方式（技术书 §5.4）。
    /// 只读持久化存储，不碰内存表（调用方可能正持有内存表的锁）。
    fn send_digest_facts(
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
        {
            if let Err(error) = validate_material_safety(material) {
                return Err((
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                    ),
                ));
            }
        }
        use std::sync::atomic::Ordering;
        let spec = self.connection_spec_from_request(object).map_err(|e| {
            (400, failure(format!("连接参数无效: {e}"), Some(serde_json::json!({"error_code": "connection_params_invalid"}))))
        })?;
        let mode = spec.mode();
        let target = spec.describe();
        if let Ok(jobs) = self.active_jobs.lock() {
            if let Some(job) = jobs.values().find(|job| job.target == target) {
                return Err((
                    409,
                    failure(
                        format!("设备 {target} 正在执行 workflow {}，请等待完成或先取消", job.workflow_id),
                        Some(serde_json::json!({"error_code": "device_busy"})),
                    ),
                ));
            }
        }
        if voice_permit.as_ref().is_some_and(|permit| !permit.load(Ordering::Acquire)) {
            return Err((403, failure("语音设备授权已撤销，未开始加工", Some(serde_json::json!({"error_code":"voice_session_revoked"})))));
        }
        let transport = (self.transport_factory)(&spec).map_err(|e| {
            (400, failure(format!("设备连接失败 ({target}): {e}"), Some(serde_json::json!({"error_code": "device_unreachable"}))))
        })?;
        let transport: std::sync::Arc<dyn laser_device::DeviceTransport> = std::sync::Arc::from(transport);
        let mut device = laser_device::DeviceConnection::new(transport.clone());
        device
            .handshake_with_deadline(std::time::Duration::from_secs(3))
            .map_err(|e| {
                (400, failure(format!("设备握手未通过 ({target}): {e}"), Some(serde_json::json!({"error_code": "device_not_ready"}))))
            })?;
        // FluidNC：读取 $CD 配置以感知自动宏 / 急停引脚 / planner；旧 GRBL 静默跳过
        device.load_features(std::time::Duration::from_secs(3));
        let device_scale = device.verify_job_ready_adaptive(std::time::Duration::from_secs(3)).map_err(|e| {
            (400, failure(format!("加工前检查未通过 ({target}): {e}"), Some(serde_json::json!({"error_code": "device_preflight_failed"}))))
        })?;
        let scale_factor = device_scale / 1000.0;
        // 设备重新通过实时 Idle 预检后，旧的不确定停止记录不再占用此目标的设备通道。
        if let Ok(mut uncertain) = self.uncertain_jobs.lock() {
            uncertain.retain(|_, old| old.target != target);
        }
        let text = std::fs::read_to_string(&gcode_path).map_err(|e| {
            (500, failure(format!("读取 G-code 文件失败: {e}"), None))
        })?;
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

        let is_offline_sd = object
            .get("offline_sd")
            .and_then(|v| v.as_bool())
            .or_else(|| object.get("send_mode").and_then(|v| v.as_str()).map(|s| s == "sd_offline"))
            .unwrap_or(false);

        let start_from_line = object
            .get("start_from_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let prepared = laser_device::prepare_gcode_lines(&text).map_err(|e| {
            (400, failure(format!("G-code 发送前校验未通过: {e}"), Some(serde_json::json!({"error_code": "gcode_line_invalid"}))))
        })?;
        let lines: Vec<String> = prepared.lines;
        if prepared.comment_stripped_lines > 0 {
            eprintln!(
                "[laser-api] workflow {workflow_id}: 已剥离 {} 行注释、跳过 {} 行空行/纯注释后发送（SHA 门禁仍校验原文件）",
                prepared.comment_stripped_lines, prepared.skipped_lines
            );
        }
        if lines.is_empty() {
            return Err((400, failure("G-code 文件为空，拒绝发送", Some(serde_json::json!({"error_code": "gcode_empty"})))));
        }
        if start_from_line >= lines.len() {
            return Err((400, failure(format!("断点行号 {start_from_line} 超出总行数 {}", lines.len()), Some(serde_json::json!({"error_code": "invalid_resume_line"})))));
        }
        if voice_permit.as_ref().is_some_and(|permit| !permit.load(Ordering::Acquire)) {
            return Err((403, failure("预检期间设备授权已撤销，未开始加工", Some(serde_json::json!({"error_code":"voice_session_revoked"})))));
        }
        let progress = std::sync::Arc::new(laser_device::JobProgress::default());
        progress.total_lines.store(lines.len(), Ordering::Relaxed);
        progress.lines_sent.store(start_from_line, Ordering::Relaxed);
        let cancel = laser_device::CancellationToken::default();
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
        if let Ok(mut jobs) = self.active_jobs.lock() {
            jobs.insert(workflow_id.to_owned(), job.clone());
        }
        let store = self.store.clone();
        let workflows = match kind {
            laser_runtime::RecordKind::Calibration => self.calibrations.clone(),
            _ => self.workflows.clone(),
        };
        let jobs_registry = self.active_jobs.clone();
        let uncertain_registry = self.uncertain_jobs.clone();
        let wf_id = workflow_id.to_owned();
        let total = lines.len();
        let obj_clone = object.clone();
        std::thread::Builder::new()
            .name(format!("laser-send-{wf_id}"))
            .spawn(move || {
                let sent_mode = if is_offline_sd { "sd_offline" } else { mode };
                let outcome = if voice_permit.as_ref().is_some_and(|permit| !permit.load(Ordering::Acquire)) {
                    Err("语音授权已撤销，排队中的加工未执行".to_owned())
                } else if is_offline_sd {
                    // SD 离线加工流程：
                    // 1. 唯一指纹文件名 /sd/moss_{workflow_id}_{short_sha}.gcode
                    // 2. HTTP POST /upload 上传文件内容到机床 SD 卡
                    // 3. 发送 $File/ShowHash 校验硬件计算的 SHA-256 物理一致性；固件不返回哈希（未启用 HashFS）时拒绝启动（2026-10-14 用户决定）
                    // 4. 发送 $SD/Run 并进入 wait_until_sd_completed 监控
                    let short_sha = if expected_sha.len() >= 8 { &expected_sha[..8] } else { &expected_sha };
                    let safe_wf_id = wf_id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_");
                    let remote_filename = format!("moss_{safe_wf_id}_{short_sha}.gcode");
                    let remote_path = format!("/sd/{remote_filename}");

                    let upload_res = if mode == "network" {
                        let http_port = obj_clone
                            .get("network_http_port")
                            .or_else(|| obj_clone.get("http_port"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(80) as u16;
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
                                &gcode_bytes,
                                std::time::Duration::from_secs(15),
                            )
                        }
                    } else {
                        Err("串口模式暂不支持自动网络传卡，请通过网络连接或手动拷贝至 SD 卡".to_string())
                    };

                    let verified = match upload_res {
                        Err(e) => Err(format!("上传脱机文件失败: {e}")),
                        Ok(()) => device
                            .verify_sd_file_hash(&remote_path, &expected_sha, std::time::Duration::from_secs(10))
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
                                // $SD/Run 走顺序信道并等 ok；固件在 Alarm 态会回 error:8
                                match device.send_line_wait_ack(&format!("$SD/Run={remote_path}")) {
                                Err(e) => Err(format!("启动 SD 脱机任务失败: {e}")),
                                Ok(()) => device
                                    .wait_until_sd_completed(std::time::Duration::from_secs(7200), Some(&progress), Some(&cancel))
                                    .map(|_| total),
                                }
                            }
                        }
                    }
                } else {
                    let mut worker = laser_device::JobWorker::with_progress(device, cancel.clone(), progress.clone())
                        .with_scale_factor(scale_factor);
                    if start_from_line > 0 {
                        // 断点续雕：下发 $X 解除可能存在的急停 Alarm 锁定，并只下发剩余未完成代码
                        let _ = worker.device.send_line_wait_ack("$X");
                        let remaining = lines[start_from_line..].to_vec();
                        worker.send_lines(remaining)
                    } else {
                        worker.send_lines(lines)
                    }
                };
                let sent = progress.lines_sent.load(Ordering::Relaxed);
                let elapsed_ms = job.started_at.elapsed().as_millis() as u64;
                let target = target_for_thread;
                let cancelled_result = || {
                    let can_resume = sent > 0 && sent < total;
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
                let (status, send_result) = match outcome {
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
                if let Some(hook) = on_terminal {
                    hook(status, &send_result);
                }
                finalize_workflow_record(store.as_deref(), &workflows, kind, &wf_id, status, send_result);
                if status != "completed" {
                    // cancelled/failed 都不能证明激光已物理停止；保留原设备以供重试 0x18。
                    if let Ok(mut uncertain) = uncertain_registry.lock() {
                        uncertain.insert(wf_id.clone(), job.clone());
                    }
                }
                if let Ok(mut jobs) = jobs_registry.lock() {
                    jobs.remove(&wf_id);
                }
            })
            .map_err(|e| (500, failure(format!("无法启动发送线程: {e}"), None)))?;
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

    /// 读取标定矩阵记录（SQLite 优先，否则内存表）。
    /// 读取 workflow / 标定记录（持久化存储优先，否则内存表）。调用方不得持有内存表的锁。
    fn load_record_payload(&self, kind: laser_runtime::RecordKind, id: &str) -> Option<serde_json::Value> {
        if kind == laser_runtime::RecordKind::Calibration {
            return self.load_calibration(id);
        }
        if let Some(store) = self.store.as_ref() {
            return store.get(kind, id).ok().flatten().map(|record| record.payload);
        }
        self.workflows.lock().ok()?.get(id).cloned()
    }

    fn load_calibration(&self, calibration_id: &str) -> Option<serde_json::Value> {
        if let Some(store) = self.store.as_ref() {
            return store
                .get(laser_runtime::RecordKind::Calibration, calibration_id)
                .ok()
                .flatten()
                .map(|record| record.payload);
        }
        self.calibrations.lock().ok()?.get(calibration_id).cloned()
    }

    /// 写入标定矩阵记录（SQLite 优先，否则内存表）。
    fn save_calibration(&self, record: &serde_json::Value) -> Result<(), String> {
        let calibration_id = record
            .get("calibration_id")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .ok_or("calibration_id 缺失")?;
        let status = record.get("status").and_then(|v| v.as_str()).map(str::to_owned);
        if let Some(store) = self.store.as_ref() {
            let stored = laser_runtime::StoredRecord {
                kind: laser_runtime::RecordKind::Calibration,
                id: calibration_id.to_owned(),
                status,
                payload: record.clone(),
            };
            return store.put(&stored).map_err(|e| format!("{e:?}"));
        }
        self.calibrations
            .lock()
            .map_err(|_| "calibration 内存表不可用".to_string())?
            .insert(calibration_id.to_owned(), record.clone());
        Ok(())
    }

    /// 标定矩阵预览：按行列/功率/速度区间真实生成单元格与 G-code，落盘、冻结 SHA 并入库。
    /// 只生成不出光；返回 `calibration_id`、cells 与 `gcode_file` 供发送与选格复用。
    fn material_lab_preview(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let material = object
            .get("material")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        if material.is_empty() {
            return Err((400, failure("material 不能为空", None)));
        }
        if let Err(error) = validate_material_safety(material) {
            return Err((
                400,
                failure(
                    error,
                    Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                ),
            ));
        }
        let thickness_mm = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0).unwrap_or(0.0);
        if !thickness_mm.is_finite() || thickness_mm <= 0.0 {
            return Err((400, failure("thickness_mm 必须大于 0", None)));
        }
        let laser_mode = object
            .get("laser_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("engrave")
            .to_ascii_lowercase();
        if !matches!(laser_mode.as_str(), "engrave" | "cut") {
            return Err((400, failure("laser_mode 必须是 engrave 或 cut", None)));
        }
        let matrix_kind = object
            .get("matrix_kind")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
            .unwrap_or_else(|| laser_mode.clone());
        let engraving_mode = if laser_mode == "cut" {
            String::new()
        } else {
            object
                .get("engraving_mode")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or("raster")
                .to_ascii_lowercase()
        };
        let rows = parse_int(object.get("rows"), "rows", 5).map_err(|e| (400, failure(e.to_string(), None)))?;
        let columns = parse_int(object.get("columns"), "columns", 5).map_err(|e| (400, failure(e.to_string(), None)))?;
        if !(1..=25).contains(&rows) || !(1..=25).contains(&columns) {
            return Err((400, failure("rows/columns 必须在 1..25 之间", None)));
        }
        // 区间缺省（-1 或缺失）时以材料推荐参数为中心取 ±40% 区间
        let recommendation = laser_core::material::recommend(
            material,
            thickness_mm,
            &laser_mode,
            if laser_mode == "cut" { None } else { Some(engraving_mode.as_str()) },
        );
        let (base_power, base_speed, base_passes) = recommendation
            .as_ref()
            .map(|r| (r.params.laser_max_power, r.params.feed_rate, r.params.passes.max(1)))
            .unwrap_or((400, 1500, 1));
        let bound = |key: &str, fallback: i32| -> Result<i32, (u16, serde_json::Value)> {
            let value = parse_int(object.get(key), key, -1).map_err(|e| (400, failure(e.to_string(), None)))?;
            Ok(if value < 0 { fallback } else { value as i32 })
        };
        let power_min = bound("power_min", (base_power as f64 * 0.6).round() as i32)?;
        let power_max = bound("power_max", (base_power as f64 * 1.4).round().min(1000.0) as i32)?;
        let speed_min = bound("speed_min", (base_speed as f64 * 0.6).round().max(100.0) as i32)?;
        let speed_max = bound("speed_max", (base_speed as f64 * 1.4).round() as i32)?;
        let passes = {
            let value = parse_int(object.get("passes"), "passes", 0).map_err(|e| (400, failure(e.to_string(), None)))?;
            if value <= 0 { base_passes } else { value as i32 }
        };
        if power_min > power_max || speed_min > speed_max || power_min < 0 || speed_min <= 0 {
            return Err((400, failure("功率/速度区间无效：min 必须 ≤ max 且为正数", None)));
        }
        let cells = laser_core::material::calibration_matrix(
            rows as usize,
            columns as usize,
            power_min,
            power_max,
            speed_min,
            speed_max,
            passes,
        )
        .map_err(|e| (400, failure(format!("标定矩阵参数无效: {e}"), None)))?;
        let task_type = if laser_mode == "cut" { "cut_contour" } else { "engrave_logo" };
        let gcode = laser_core::material::build_calibration_gcode(material, task_type, &cells)
            .map_err(|e| (500, failure(format!("标定 G-code 生成失败: {e}"), None)))?;

        let root = self
            .generated_root
            .as_deref()
            .or(self.upload_root.as_deref())
            .ok_or_else(|| (500, failure("生成目录未配置", None)))?;
        std::fs::create_dir_all(root).map_err(|e| (500, failure(format!("创建生成目录失败: {e}"), None)))?;
        let calibration_id = format!("cal-{}", next_workflow_id().trim_start_matches("wf-"));
        let gcode_name = format!("{calibration_id}.gcode");
        let gcode_path = root.join(&gcode_name);
        std::fs::write(&gcode_path, gcode.as_bytes())
            .map_err(|e| (500, failure(format!("写入标定 G-code 失败: {e}"), None)))?;
        let gcode_sha = hex::encode(Sha256::digest(gcode.as_bytes()));
        let estimate = laser_core::gcode::estimate_time(&gcode, 3000.0);
        let cells_json = cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                serde_json::json!({
                    "cell_number": index + 1,
                    "id": cell.id,
                    "row": cell.row,
                    "column": cell.column,
                    "x_mm": cell.x_mm,
                    "y_mm": cell.y_mm,
                    "size_mm": cell.size_mm,
                    "power": cell.power,
                    "speed": cell.speed,
                    "passes": cell.passes,
                    "pixel_size_mm": cell.pixel_size_mm,
                    "line_interval_mm": cell.line_interval_mm,
                })
            })
            .collect::<Vec<_>>();
        let width_mm = (columns as f64 - 1.0) * 16.0 + 12.0;
        let height_mm = (rows as f64 - 1.0) * 16.0 + 12.0;
        let now = unix_seconds();
        let record = serde_json::json!({
            "calibration_id": calibration_id,
            "status": "prepared",
            "material": material,
            "thickness_mm": thickness_mm,
            "laser_mode": laser_mode,
            "engraving_mode": engraving_mode,
            "matrix_kind": matrix_kind,
            "task_type": task_type,
            "rows": rows,
            "columns": columns,
            "power_min": power_min,
            "power_max": power_max,
            "speed_min": speed_min,
            "speed_max": speed_max,
            "passes": passes,
            "cells": cells_json,
            "cell_count": cells.len(),
            "width_mm": width_mm,
            "height_mm": height_mm,
            "gcode_file": gcode_name,
            "gcode_sha256": gcode_sha,
            "artifacts": {"gcode_file": gcode_name, "gcode_sha256": gcode_sha},
            "confirmation_snapshot": {"gcode_file": gcode_name, "gcode_sha256": gcode_sha, "material": material, "thickness_mm": thickness_mm},
            "time_estimate": {
                "estimated_seconds": estimate.estimated_seconds,
                "estimated_minutes": estimate.estimated_minutes,
                "laser_seconds": estimate.laser_seconds,
                "non_laser_seconds": estimate.non_laser_seconds,
            },
            "web": {"gcode_download_url": format!("/preview?path={gcode_name}&download=1")},
            "next_actions": ["send", "select_cell"],
            "created_at": now,
            "updated_at": now,
        });
        self.save_calibration(&record)
            .map_err(|e| (500, failure(format!("保存标定矩阵失败: {e}"), None)))?;
        Ok(record)
    }

    /// 标定矩阵发送：与 workflow confirm_send 同一套门禁（confirmed、终态拦截、物理 SHA 重算）
    /// 后，复用 JobWorker 真实逐行下发；终态由后台线程写回 calibration 记录。
    fn material_lab_send(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
        ctx: &laser_contracts::CallerContext,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let calibration_id = object
            .get("calibration_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if calibration_id.is_empty() {
            return Err((400, failure("calibration_id 不能为空", None)));
        }
        if !parse_bool(object.get("confirmed"), false) {
            return Err((400, failure("发送测试矩阵需要 confirmed=true", None)));
        }
        let mut request = object.clone();
        request
            .entry("connection_mode")
            .or_insert(serde_json::Value::String("network".into()));
        let mode = request
            .get("connection_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("network")
            .to_ascii_lowercase();
        if mode == "network" {
            let host = request.get("network_host").and_then(serde_json::Value::as_str).unwrap_or("");
            if host.trim().is_empty() {
                return Err((400, failure("请填写设备 host/IP", None)));
            }
        }
        let mut record = self
            .load_calibration(&calibration_id)
            .ok_or_else(|| (404, failure("未找到标定矩阵，请先生成", Some(serde_json::json!({"error_code": "calibration_missing"})))))?;
        if let Err(error) = validate_material_safety_in_payload(&record) {
            return Err((
                400,
                failure(
                    error,
                    Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                ),
            ));
        }
        let current_status = record.get("status").and_then(|v| v.as_str());
        if let Some(reason) = workflow_send_blocked_reason(current_status) {
            return Err((400, failure(reason, Some(serde_json::json!({"error_code": "calibration_not_sendable"})))));
        }
        let send_roots = self.send_gcode_roots();
        let (preflight, gcode_sha, gcode_file) = build_send_preflight(&record, &send_roots)
            .map_err(|e| (400, failure(format!("发送安全门禁未通过: {e}"), Some(serde_json::json!({"error_code": "send_gate_rejected"})))))?;
        laser_device::authorize_send(&preflight)
            .map_err(|e| (400, failure(format!("发送安全门禁未通过: {e}"), Some(serde_json::json!({"error_code": "send_gate_rejected"})))))?;
        let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
        let send_result = self.launch_send_job_for(
            laser_runtime::RecordKind::Calibration,
            &calibration_id,
            gcode_path,
            &gcode_sha,
            &request,
            ctx,
        )?;
        if let Some(map) = record.as_object_mut() {
            map.insert("status".into(), serde_json::Value::String("sending".into()));
            map.insert("updated_at".into(), serde_json::json!(unix_seconds()));
            map.insert("send_result".into(), send_result);
            map.insert("job_id".into(), serde_json::Value::String(calibration_id.clone()));
            map.insert(
                "probe".into(),
                serde_json::json!({"connected": true, "mode": mode, "summary": "握手通过（$32=1），已开始逐行下发"}),
            );
            freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
        }
        self.save_calibration(&record)
            .map_err(|e| (500, failure(format!("保存标定矩阵失败: {e}"), None)))?;
        Ok(record)
    }

    /// 选格回填：把标定矩阵中指定单元格的功率/速度/次数写入材料库（文件 + SQLite），
    /// 并在 calibration 记录上标注 selected_cell。
    fn material_lab_select_cell(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        let calibration_id = object
            .get("calibration_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        if calibration_id.is_empty() {
            return Err((400, failure("calibration_id 不能为空", None)));
        }
        let cell_number = parse_int(object.get("cell_number"), "cell_number", 0)
            .map_err(|e| (400, failure(e.to_string(), None)))?;
        if cell_number < 1 {
            return Err((400, failure("cell_number 必须大于等于 1", None)));
        }
        let mut record = self
            .load_calibration(&calibration_id)
            .ok_or_else(|| (404, failure("未找到标定矩阵，请先生成", Some(serde_json::json!({"error_code": "calibration_missing"})))))?;
        let cell = record
            .get("cells")
            .and_then(|v| v.as_array())
            .and_then(|cells| cells.get((cell_number - 1) as usize))
            .cloned()
            .ok_or_else(|| (400, failure(format!("cell_number {cell_number} 超出矩阵范围"), None)))?;
        let material = object
            .get("material")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or_else(|| record.get("material").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_owned();
        let thickness_mm = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0)
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| record.get("thickness_mm").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        let laser_mode = object
            .get("laser_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or_else(|| record.get("laser_mode").and_then(|v| v.as_str()))
            .unwrap_or("engrave")
            .to_ascii_lowercase();
        let engraving_mode = object
            .get("engraving_mode")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .or_else(|| record.get("engraving_mode").and_then(|v| v.as_str()).filter(|v| !v.is_empty()))
            .unwrap_or("raster")
            .to_ascii_lowercase();
        if material.is_empty() || thickness_mm <= 0.0 {
            return Err((400, failure("material 与 thickness_mm 必须有效", None)));
        }
        let power = cell.get("power").and_then(|v| v.as_i64()).unwrap_or(0);
        let speed = cell.get("speed").and_then(|v| v.as_i64()).unwrap_or(0);
        let passes = cell.get("passes").and_then(|v| v.as_i64()).unwrap_or(1);
        let notes = object
            .get("notes")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("标定矩阵 {calibration_id} 单元格 #{cell_number}"));
        let mut save = serde_json::Map::new();
        save.insert("action".into(), serde_json::Value::String("save".into()));
        save.insert("material".into(), serde_json::Value::String(material.clone()));
        save.insert("thickness_mm".into(), serde_json::json!(thickness_mm));
        save.insert("laser_mode".into(), serde_json::Value::String(laser_mode.clone()));
        save.insert("engraving_mode".into(), serde_json::Value::String(engraving_mode.clone()));
        save.insert("laser_max_power".into(), serde_json::json!(power));
        save.insert("feed_rate".into(), serde_json::json!(speed));
        save.insert("passes".into(), serde_json::json!(passes.max(1)));
        if let Some(pixel) = cell.get("pixel_size_mm").and_then(|v| v.as_f64()) {
            save.insert("pixel_size_mm".into(), serde_json::json!(pixel));
        }
        save.insert("source".into(), serde_json::Value::String("calibration".into()));
        save.insert("notes".into(), serde_json::Value::String(notes));
        let response = handle_material_manage(&self.settings, self.runtime_db.as_ref(), &save);
        let separator = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(0);
        let saved: serde_json::Value = serde_json::from_slice(&response[separator..]).unwrap_or_default();
        if saved.get("success") != Some(&serde_json::Value::Bool(true)) {
            let message = saved
                .get("result")
                .and_then(|v| v.as_str())
                .unwrap_or("材料参数保存失败")
                .to_owned();
            return Err((400, failure(format!("单元格参数写回材料库失败: {message}"), None)));
        }
        let now = unix_seconds();
        if let Some(map) = record.as_object_mut() {
            map.insert(
                "selected_cell".into(),
                serde_json::json!({
                    "cell_number": cell_number,
                    "id": cell.get("id").cloned().unwrap_or(serde_json::Value::Null),
                    "power": power,
                    "speed": speed,
                    "passes": passes,
                    "material": material,
                    "thickness_mm": thickness_mm,
                    "laser_mode": laser_mode,
                    "engraving_mode": engraving_mode,
                    "selected_at": now,
                }),
            );
            map.insert("selected_cell_number".into(), serde_json::json!(cell_number));
            map.insert("updated_at".into(), serde_json::json!(now));
        }
        self.save_calibration(&record)
            .map_err(|e| (500, failure(format!("保存标定矩阵失败: {e}"), None)))?;
        Ok(serde_json::json!({
            "calibration_id": calibration_id,
            "selected_cell": record.get("selected_cell").cloned().unwrap_or(serde_json::Value::Null),
            "cell_number": cell_number,
            "materials": saved.get("result").and_then(|r| r.get("materials")).cloned().unwrap_or(serde_json::Value::Null),
            "speech": format!("已将单元格 #{cell_number} 的功率 {power} / 速度 {speed} 保存为推荐参数"),
        }))
    }

    /// 握手探测：只发 `$$` 读取固件设置（不移动轴、不出光）。先探首选模式，
    /// 失败且另一模式参数可用时回退探测，输出与前端契约一致的 attempts/summary 结构。
    fn probe_connections(&self, object: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
        let preferred = object
            .get("connection_mode")
            .or_else(|| object.get("preferred_mode"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(self.settings.default_connection_mode.as_str())
            .to_ascii_lowercase();
        let preferred = if preferred == "serial" { "serial" } else { "network" };
        let other = if preferred == "network" { "serial" } else { "network" };
        let timeout_secs = parse_float(object.get("network_timeout"), "network_timeout", 3.0)
            .unwrap_or(3.0)
            .clamp(0.5, 10.0);
        let timeout = std::time::Duration::from_secs_f64(timeout_secs);

        let mut attempts = Vec::new();
        let mut selected: Option<&str> = None;
        let mut serial_connected = false;
        let mut network_connected = false;
        let mut firmware_settings: Vec<String> = Vec::new();
        let mut machine_limits = None;
        let mut machine_features = None;
        let mut summary = String::new();

        for (index, mode) in [preferred, other].into_iter().enumerate() {
            let mut request = object.clone();
            request.insert("connection_mode".into(), serde_json::Value::String(mode.to_owned()));
            let spec = match self.connection_spec_from_request(&request) {
                Ok(spec) => spec,
                Err(error) => {
                    // 回退模式参数缺失不算错误，只记录跳过原因
                    attempts.push(serde_json::json!({
                        "mode": mode, "target": "", "connected": false, "laser_mode_ok": false,
                        "summary": format!("{}: {error}", if index == 0 { "参数无效" } else { "未尝试" })
                    }));
                    if index == 0 { summary = format!("{} 参数无效: {error}", if mode == "network" { "网络" } else { "串口" }); }
                    continue;
                }
            };
            let report = laser_device::probe_device(&spec, timeout);
            attempts.push(serde_json::json!({
                "mode": mode,
                "target": report.target,
                "connected": report.connected,
                "laser_mode_ok": report.laser_mode_ok,
                "summary": report.summary,
            }));
            if index == 0 || summary.is_empty() {
                summary = report.summary.clone();
            }
            if report.connected && report.laser_mode_ok {
                if mode == "serial" { serial_connected = true; } else { network_connected = true; }
                selected = Some(mode);
                firmware_settings = report.firmware_settings.clone();
                machine_limits = report.machine_limits.clone();
                machine_features = report.features.clone();
                summary = report.summary.clone();
                break;
            }
        }
        serde_json::json!({
            "preferred_mode": preferred,
            "selected_mode": selected,
            "fallback_used": selected.is_some_and(|m| m != preferred),
            "attempts": attempts,
            "serial_connected": serial_connected,
            "network_connected": network_connected,
            "connected": serial_connected || network_connected,
            "firmware_settings": firmware_settings,
            "machine_limits": machine_limits.map(|lim| serde_json::json!({
                "x_travel_mm": lim.x_travel_mm,
                "y_travel_mm": lim.y_travel_mm,
                "z_travel_mm": lim.z_travel_mm,
            })),
            "features": machine_features.map(|feat| serde_json::json!({
                "has_air_assist": feat.has_air_assist,
                "has_door": feat.has_door,
                "speed_map": feat.speed_map,
            })),
            "timeout_seconds": timeout_secs,
            "summary": summary,
        })
    }

    pub fn with_runtime_path(mut self, runtime_path: std::path::PathBuf) -> Self {
        let disabled = read_runtime_settings(Some(&runtime_path))
            .get("mcp_disabled_tools")
            .and_then(serde_json::Value::as_array)
            .map(|names| names.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect::<Vec<_>>())
            .unwrap_or_default();
        self.set_disabled_tools(disabled);
        self.runtime_path = Some(runtime_path);
        self
    }

    pub fn set_actual_port(&self, port: u16) {
        self.actual_port.store(port, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn get_actual_port(&self) -> u16 {
        let port = self.actual_port.load(std::sync::atomic::Ordering::SeqCst);
        if port == 0 {
            self.settings.web_port
        } else {
            port
        }
    }

    pub fn with_upload_root(mut self, upload_root: std::path::PathBuf) -> Self {
        self.upload_root = Some(upload_root);
        self
    }

    pub fn with_preview_roots(mut self, preview_roots: Vec<std::path::PathBuf>) -> Self {
        self.preview_roots = preview_roots;
        self
    }

    pub fn with_auth(mut self, auth: std::sync::Arc<AuthManager>) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn with_store(
        mut self,
        store: std::sync::Arc<dyn laser_runtime::RuntimeStore + Send + Sync>,
    ) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_runtime_db(
        mut self,
        runtime_db: std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>,
    ) -> Self {
        self.runtime_db = Some(runtime_db.clone());
        if self.approvals.is_none() {
            self.approvals = Some(std::sync::Arc::new(approval::ApprovalService::new(runtime_db.clone())));
        }
        self.with_store(runtime_db)
    }

    pub fn with_generated_root(mut self, generated_root: std::path::PathBuf) -> Self {
        self.generated_root = Some(generated_root);
        self
    }

    /// 发送门禁允许解析 G-code 刀路的目录集合：预览根 + 生成目录 + 上传目录
    fn send_gcode_roots(&self) -> Vec<std::path::PathBuf> {
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

    pub fn find_recent_material_workflow(&self) -> Option<serde_json::Value> {
        let workflows = if let Some(store) = self.store.as_ref() {
            store
                .list_recent(laser_runtime::RecordKind::Workflow, 50)
                .ok()
                .map(|records| records.into_iter().map(|r| r.payload).collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            self.workflows.lock().unwrap().values().cloned().collect()
        };
        for wf in workflows {
            let input = wf.get("input").and_then(serde_json::Value::as_object);
            let artifacts = wf.get("artifacts").and_then(serde_json::Value::as_object);
            let material = input
                .and_then(|i| i.get("material"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    artifacts
                        .and_then(|a| a.get("material"))
                        .and_then(serde_json::Value::as_str)
                });
            let thickness = input
                .and_then(|i| i.get("thickness_mm"))
                .and_then(serde_json::Value::as_f64)
                .or_else(|| {
                    artifacts
                        .and_then(|a| a.get("thickness_mm"))
                        .and_then(serde_json::Value::as_f64)
                });
            if let (Some(mat), Some(thick)) = (material, thickness) {
                if !mat.is_empty() && thick > 0.0 {
                    let laser_mode = input
                        .and_then(|i| i.get("laser_mode"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("engrave");
                    let engraving_mode = input
                        .and_then(|i| i.get("engraving_mode"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("raster");
                    let rec = laser_core::material::recommend(
                        mat,
                        thick,
                        laser_mode,
                        Some(engraving_mode),
                    );
                    let params = if let Some(rec) = rec {
                        serde_json::json!({
                            "laser_min_power": rec.params.laser_min_power,
                            "laser_max_power": rec.params.laser_max_power,
                            "feed_rate": rec.params.feed_rate,
                            "travel_rate": rec.params.travel_rate,
                            "threshold": rec.params.threshold,
                            "passes": rec.params.passes,
                            "pixel_size_mm": rec.params.pixel_size_mm
                        })
                    } else {
                        serde_json::json!({
                            "laser_min_power": 0,
                            "laser_max_power": 340,
                            "feed_rate": 1600,
                            "travel_rate": 3000,
                            "threshold": -1,
                            "passes": 1,
                            "pixel_size_mm": 0.1
                        })
                    };
                    return Some(serde_json::json!({
                        "workflow_id": wf.get("workflow_id").and_then(serde_json::Value::as_str).unwrap_or(""),
                        "material": mat,
                        "thickness_mm": thick,
                        "laser_mode": laser_mode,
                        "engraving_mode": engraving_mode,
                        "params": params
                    }));
                }
            }
        }
        None
    }

    pub fn serve(listener: TcpListener, service: std::sync::Arc<Self>) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let service = service.clone();
            std::thread::spawn(move || {
                let Ok(peer) = stream.peer_addr() else { return; };
                let source = peer.ip().to_string();
                let loopback = peer.ip().is_loopback();
                if let Ok(request) = read_http_request(&mut stream) {
                    if !request.is_empty() {
                        let response = missing_host_rejection(&request, loopback)
                            .unwrap_or_else(|| service.handle_http_from(&request, loopback, &source, None));
                        let _ = stream.write_all(&response);
                        let _ = stream.flush();
                    }
                }
            });
        }
        Ok(())
    }
}

/// 真实 TCP 入口上的回环请求必须带 `Host`：否则 `is_allowed_loopback_host` 的防 DNS 重绑定检查无从执行。
/// 进程内调用（`handle_http` / `handle_http_with_ctx`）不经过这里。
fn missing_host_rejection(request: &[u8], loopback: bool) -> Option<Vec<u8>> {
    if !loopback || header_value(&request_head(request), "host").is_some() {
        return None;
    }
    Some(http_json(
        403,
        failure(
            "请求缺少 Host 头，请用 127.0.0.1 或 localhost 访问",
            Some(serde_json::json!({"error_code": "host_not_allowed"})),
        ),
    ))
}

fn read_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<Vec<u8>> {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let mut content_length: Option<usize> = None;
    let mut header_end: Option<usize> = None;
    const MAX_HEADER_SIZE: usize = 64 * 1024;
    const MAX_BODY_SIZE: usize = 20 * 1024 * 1024;

    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);

        if header_end.is_none() {
            if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let head = String::from_utf8_lossy(&buffer[..pos]);
                for line in head.lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_length = v.trim().parse::<usize>().ok();
                        }
                    }
                }
            } else if buffer.len() > MAX_HEADER_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP 请求头超出 64KB 限制",
                ));
            }
        }

        if let Some(end) = header_end {
            let body_len = content_length.unwrap_or(0);
            if body_len > MAX_BODY_SIZE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP 请求体超出 20MB 限制",
                ));
            }
            let needed = end + body_len;
            if buffer.len() >= needed {
                break;
            }
        }
    }
    Ok(buffer)
}

impl ApiService {

    pub fn handle_http(&self, request: &[u8]) -> Vec<u8> {
        self.handle_http_from(request, true, "127.0.0.1", None)
    }

    /// 进程内调用方显式声明身份（技术书 §6.4 规则 2）：`InProcessBackend` 传 `McpBridge`，
    /// 网关分发传 `Gateway`。按本机回环处理鉴权，但上下文用调用方给的值，不借用 `Loopback`。
    pub fn handle_http_with_ctx(&self, request: &[u8], ctx: laser_contracts::CallerContext) -> Vec<u8> {
        self.handle_http_from(request, true, "127.0.0.1", Some(ctx))
    }

    /// HTTP 入口：先做 Host 校验与浏览器来源校验（技术书 §6.5 / D11），再进入路由。
    ///
    /// - 回环连接的 `Host` 必须是 IP 字面量、`localhost` 或 `MOSS_ALLOWED_ORIGINS` 中的主机名，
    ///   否则 403 `host_not_allowed`（防 DNS 重绑定：攻击者域名解析到 127.0.0.1 时 `Host` 仍是其域名）；
    /// - 不带 `Origin` 的请求（插件、MCP 桥接、curl、局域网配对客户端、进程内调用）行为不变，响应不带 CORS 头；
    /// - 带 `Origin` 且允许：正常路由，响应回显该来源并加 `Vary: Origin`；
    /// - 带 `Origin` 且不允许：`GET`/`HEAD` 正常路由但不带任何 CORS 头（跨站页面读不到响应），
    ///   其他方法（含 `OPTIONS` 预检与 `text/plain` 简单请求）一律 403 `origin_not_allowed`，不进入路由。
    fn handle_http_from(
        &self,
        request: &[u8],
        loopback: bool,
        source_ip: &str,
        ctx_override: Option<laser_contracts::CallerContext>,
    ) -> Vec<u8> {
        let head = request_head(request);
        let method = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or("")
            .to_owned();
        let host = header_value(&head, "host");
        if loopback
            && host
                .as_deref()
                .is_some_and(|host| !is_allowed_loopback_host(host, &self.extra_allowed_origins))
        {
            return http_json(
                403,
                failure(
                    "请求的主机名不在允许范围内，请用 127.0.0.1 或 localhost 访问",
                    Some(serde_json::json!({"error_code": "host_not_allowed"})),
                ),
            );
        }
        let Some(origin) = header_value(&head, "origin") else {
            return self.route_http(request, loopback, source_ip, ctx_override.clone());
        };
        if !is_allowed_origin(&origin, host.as_deref(), loopback, &self.extra_allowed_origins) {
            if method == "GET" || method == "HEAD" {
                return self.route_http(request, loopback, source_ip, ctx_override.clone());
            }
            return http_json(
                403,
                failure(
                    "请求来源不在允许范围内",
                    Some(serde_json::json!({"error_code": "origin_not_allowed"})),
                ),
            );
        }
        if method == "OPTIONS" {
            return http_options(Some(&origin));
        }
        with_cors_headers(self.route_http(request, loopback, source_ip, ctx_override.clone()), &origin)
    }

    fn route_http(
        &self,
        request: &[u8],
        loopback: bool,
        source_ip: &str,
        ctx_override: Option<laser_contracts::CallerContext>,
    ) -> Vec<u8> {
        let Some(separator) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return http_json(400, failure("请求格式错误", None));
        };
        let head = String::from_utf8_lossy(&request[..separator]);
        let mut lines = head.lines();
        let Some(request_line) = lines.next() else {
            return http_json(400, failure("请求格式错误", None));
        };
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/");
        let authorization = head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().strip_prefix("Bearer ").map(str::to_string))
                .flatten()
        });
        let content_length = lines
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0usize);
        let body = &request[separator + 4..];
        if body.len() < content_length {
            return http_json(400, failure("请求体不完整", None));
        }
        let route = path.split('?').next().unwrap_or(path);
        if method == "OPTIONS" {
            return http_options(None);
        }
        if method == "POST" && route == "/v1/auth/challenge" {
            if !loopback {
                return http_json(403, failure("仅允许本地回环客户端生成配对挑战码", None));
            }
            let Some(auth) = self.auth.as_ref() else {
                return http_json(500, failure("认证服务未配置", None));
            };
            return match auth.issue_code(unix_seconds()) {
                Ok(challenge) => http_json(
                    200,
                    success(serde_json::json!({
                        "pairing_code": challenge.code,
                        "expires_at": challenge.expires_at,
                        "valid_seconds": 300
                    })),
                ),
                Err(error) => http_json(500, failure(error, None)),
            };
        }
        if method == "POST" && route == "/v1/auth/pair" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let code = object
                .get("pairing_code")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let Some(auth) = self.auth.as_ref() else {
                return http_json(404, failure("未找到路径", None));
            };
            return match auth.pair(source_ip, code, unix_seconds()) {
                Ok((token, pairing_id)) => http_json(
                    200,
                    success(serde_json::json!({
                        "token": token, "pairing_id": pairing_id
                    })),
                ),
                Err(AuthError::Locked(seconds)) => http_json_with_headers(
                    429,
                    failure("配对请求过于频繁，请稍后重试", None),
                    &[("Retry-After", seconds.to_string())],
                ),
                Err(AuthError::Invalid) => http_json(400, failure("配对码无效或已过期", None)),
                Err(AuthError::Internal) => http_json(500, failure("认证服务不可用", None)),
                Err(AuthError::Unauthorized) => http_json(400, failure("配对码无效或已过期", None)),
            };
        }
        let public_route = (method == "GET"
            && (route == "/api/health" || route == "/v1/capabilities" || route == "/api/system/info"))
            || (method == "GET"
                && (route == "/"
                    || is_spa_route(route)
                    || route.starts_with("/assets/")
                    || route.starts_with("/vendor/")));
        let mut pairing_id = None;
        if !loopback && !public_route {
            let Some(auth) = self.auth.as_ref() else {
                return http_json(401, failure("需要 Bearer 鉴权", None));
            };
            let Some(token) = authorization.as_deref() else {
                return http_json(401, failure("需要 Bearer 鉴权", None));
            };
            match auth.authorize(token) {
                Ok(id) => pairing_id = Some(id),
                Err(_) => return http_json(401, failure("Bearer 鉴权失败", None)),
            }
        }
        // 桌面会话头（技术书 §5.3）：只在回环连接上有效，值不对时 403，前端重新取会话后只重试一次
        let desktop = match self.check_desktop_session(&head, loopback) {
            Ok(desktop) => desktop,
            Err(response) => return response,
        };
        // 调用方上下文只由入口构造（技术书 §6.4）：进程内调用方显式声明优先，其次桌面会话
        let ctx = if desktop && ctx_override.is_none() {
            laser_contracts::CallerContext::Desktop
        } else {
            caller_context_for(ctx_override, loopback, pairing_id)
        };
        if !self.voice_caller_allowed(&ctx) {
            return http_json(403, failure("语音设备授权已失效，请重新连接", Some(serde_json::json!({"error_code":"voice_session_revoked"}))));
        }
        // 手机 / 局域网连接（技术书 §9.8）：可以操作雕刻机，但材料、校准参数与系统设置只读，只能在电脑上修改
        if matches!(ctx, laser_contracts::CallerContext::Lan { .. })
            && is_reference_data_write(method, route, &body[..content_length])
        {
            return http_json(
                403,
                failure(
                    "手机 / 局域网连接为只读：材料、校准参数和系统设置请在电脑上修改",
                    Some(serde_json::json!({"error_code": "lan_read_only"})),
                ),
            );
        }
        if route == "/v1/approvals" || route.starts_with("/v1/approvals/") {
            let query = path.split_once('?').map(|(_, query)| query).unwrap_or("");
            return self.handle_approval_route(method, route, query, &body[..content_length], &ctx);
        }
        if method == "POST" && matches!(route, "/api/text-task/generate" | "/api/draw/preview" | "/api/workflow")
            && serde_json::from_slice::<serde_json::Value>(&body[..content_length])
                .ok().is_some_and(|value| value.get("feedback_params").is_some())
        {
            return http_json(400, failure("feedback_params 为内部字段，请通过已保存反馈确认采用", None));
        }
        if method == "GET" && route == "/api/system/lan-info" {
            // 只给电脑本机看：偏好设置“手机连接”分区显示本机局域网地址与端口
            if !loopback {
                return http_json(403, failure("仅允许在电脑本机查看", None));
            }
            let saved = read_runtime_settings(self.runtime_path.as_deref())
                .get("lan_access")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let listening_all = matches!(self.settings.web_host.as_str(), "0.0.0.0" | "::" | "[::]");
            let port = self.get_actual_port();
            let addresses: Vec<String> = primary_lan_ipv4().into_iter().map(|ip| ip.to_string()).collect();
            return http_json(
                200,
                success(serde_json::json!({
                    "lan_access_saved": saved,
                    "lan_access_active": listening_all,
                    "web_host": self.settings.web_host,
                    "port": port,
                    "addresses": addresses,
                    "restart_required": saved != listening_all,
                })),
            );
        }
        if method == "GET" && route == "/api/jobs/stop-targets" {
            // 顶栏急停不依赖 React 页面的生命周期，从后端读取精确任务和原连接目标。
            let Ok(running) = self.active_jobs.lock() else { return http_json(500, failure("设备任务表不可用", None)); };
            let Ok(uncertain) = self.uncertain_jobs.lock() else { return http_json(500, failure("待确认停止任务表不可用", None)); };
            let mut targets = std::collections::BTreeMap::<String, serde_json::Value>::new();
            for (id, job) in uncertain.iter() {
                targets.insert(id.clone(), serde_json::json!({"job_id": id, "target": job.target, "state": "unknown"}));
            }
            for (id, job) in running.iter() {
                // 进度浮层（2026-11-02）：顶栏急停只用 job_id / state，这里额外带上只读进度字段
                // （workflow_id、phase、lines_sent、total_lines、elapsed_ms、mode），供全局浮层显示；
                // 只读查询，不改变门禁、不触发任何设备写入。
                let mut live = job.live_send_result();
                if let Some(map) = live.as_object_mut() {
                    map.insert("job_id".into(), serde_json::json!(id));
                    map.insert("workflow_id".into(), serde_json::json!(job.workflow_id));
                    map.insert("state".into(), serde_json::json!("running"));
                }
                targets.insert(id.clone(), live);
            }
            return http_json(200, success(serde_json::json!({"jobs": targets.into_values().collect::<Vec<_>>() })));
        }
        if method == "GET" && route == "/api/system/info" {
            let actual = self.get_actual_port();
            let preferred_from_file = self.runtime_path.as_ref()
                .map(|d| d.join("settings.json"))
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|v| v.get("web_port").and_then(|p| p.as_u64()))
                .map(|p| p as u16);
            let preferred = preferred_from_file.unwrap_or(self.settings.web_port);

            let mcp_endpoint = configured_mcp_endpoint(self.runtime_path.as_deref());
            let telemetry = laser_contracts::global_mcp_telemetry().snapshot();
            // 严格模式下接入点不列出永不开放的 R4 工具（与 laser_mcp::tools_list_for 一致）
            // 用户关闭的工具同样不列出（NV8）
            let tools_count = laser_tools::ToolCatalog::from_environment()
                .specs()
                .iter()
                .filter(|spec| !(self.strict_mode() && laser_contracts::tool_never_exposed(spec.name)))
                .filter(|spec| !self.tool_disabled(spec.name))
                .count();
            return http_json(
                200,
                success(serde_json::json!({
                    "actual_port": actual,
                    "preferred_port": preferred,
                    "is_fallback": actual != preferred,
                    "web_host": self.settings.web_host,
                    "mcp": mcp_status_json(mcp_endpoint.as_deref(), &telemetry, tools_count),
                    // 链路模式来自 settings.json 的 voice.mode（语音中心写入）；设备会话数由桌面端语音中心
                    // 轮询自建接入点 health 后提供，官方链路或未知时为 null（技术书 §9.4.5）
                    "voice": voice_status_json(self.runtime_path.as_deref())
                })),
            );
        }
        if method == "GET" && route == "/api/jev/status" {
            let settings = read_runtime_settings(self.runtime_path.as_deref());
            let api_key = match self.runtime_db.as_ref() {
                Some(db) => match db.lock().map_err(|_| "运行时数据库锁失败".to_owned()).and_then(|db| {
                    db.get_secret(JEV_API_KEY_SECRET).map_err(|error| format!("{error:?}"))
                }) {
                    Ok(value) => value,
                    Err(error) => return http_json(500, failure(error, None)),
                },
                None => None,
            };
            return http_json(
                200,
                success(serde_json::json!({
                    "enabled": settings.get("jev_enabled").and_then(|value| value.as_bool()).unwrap_or(false),
                    "endpoint": DEFAULT_JEV_ENDPOINT,
                    "model": JEV_MODEL,
                    "provider": "OpenRouter",
                    "configured": api_key.as_deref().is_some_and(|value| !value.trim().is_empty()),
                    "masked_key": api_key.as_deref().map(mask_secret).unwrap_or_default(),
                })),
            );
        }
        if method == "POST" && route == "/api/jev/test" {
            let Some(db) = self.runtime_db.as_ref() else {
                return http_json(503, failure("运行时数据库未启用", None));
            };
            let api_key = match db.lock().map_err(|_| "运行时数据库锁失败".to_owned()).and_then(|db| {
                db.get_secret(JEV_API_KEY_SECRET).map_err(|error| format!("{error:?}"))
            }) {
                Ok(Some(value)) if !value.trim().is_empty() => value,
                Ok(_) => return http_json(404, failure("请先保存 Jev API Key", None)),
                Err(error) => return http_json(500, failure(error, None)),
            };
            return match feedback::call_jev(DEFAULT_JEV_ENDPOINT, &api_key, &serde_json::json!({
                "feedback_text": "效果正常，无需调整"
            })) {
                Ok(_) => http_json(200, success(serde_json::json!({"message": "Jev 连接正常"}))),
                Err(error) => http_json(502, failure(format!("Jev 连接失败: {error}"), None)),
            };
        }
        if (method == "POST" && matches!(route, "/api/feedback" | "/api/jev/feedback" | "/api/feedback/accept"))
            || (method == "GET" && route == "/api/feedback/history")
        {
            return self.handle_feedback_route(route, path, &body[..content_length]);
        }
        if method == "POST" && route == "/api/jev/reveal" {
            let Some(db) = self.runtime_db.as_ref() else {
                return http_json(503, failure("运行时数据库未启用", None));
            };
            let api_key = match db.lock().map_err(|_| "运行时数据库锁失败".to_owned()).and_then(|db| {
                db.get_secret(JEV_API_KEY_SECRET).map_err(|error| format!("{error:?}"))
            }) {
                Ok(Some(value)) if !value.trim().is_empty() => value,
                Ok(_) => return http_json(404, failure("Jev API Key 尚未配置", None)),
                Err(error) => return http_json(500, failure(error, None)),
            };
            return http_json(200, success(serde_json::json!({ "api_key": api_key })));
        }
        if method == "POST" && route == "/api/settings" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            if let Some(endpoint) = object.get("jev_endpoint") {
                if !endpoint.as_str().is_some_and(|s| s.trim().is_empty() || s.trim() == DEFAULT_JEV_ENDPOINT) {
                    return http_json(400, failure("Jev 仅支持固定 OpenRouter Decisions 地址", None));
                }
            }
            if object.get("jev_enabled").is_some_and(|v| !v.is_boolean())
                || object.get("jev_api_key").is_some_and(|v| !v.is_string())
            {
                return http_json(400, failure("jev_enabled 必须是布尔值，jev_api_key 必须是字符串", None));
            }
            let mcp_endpoint = match object.get("mcp_endpoint") {
                None => None,
                Some(serde_json::Value::String(value)) if value.trim().is_empty() => Some(None),
                Some(serde_json::Value::String(value)) => {
                    let Some(normalized) = normalize_mcp_endpoint(value) else {
                        return http_json(
                            400,
                            failure("小智 MCP 端点必须是 ws:// 或 wss:// 地址，且不能包含模板占位符", None),
                        );
                    };
                    Some(Some(normalized))
                }
                Some(_) => return http_json(400, failure("mcp_endpoint 必须是字符串", None)),
            };
            // 先校验全部字段，再一次写入 settings.json；写不进去就报错，不能显示“已保存”
            let mut file_changes: Vec<(&str, Option<serde_json::Value>)> = Vec::new();
            if let Some(port) = object.get("web_port").and_then(|v| v.as_u64()) {
                if port < 1024 || port > 65535 {
                    return http_json(400, failure("端口号必须在 1024 ~ 65535 之间", None));
                }
                file_changes.push(("web_port", Some(serde_json::json!(port))));
            }
            if let Some(enabled) = object.get("jev_enabled").and_then(|value| value.as_bool()) {
                file_changes.push(("jev_enabled", Some(serde_json::json!(enabled))));
            }
            if let Some(value) = object.get("lan_access") {
                let Some(enabled) = value.as_bool() else {
                    return http_json(400, failure("lan_access 必须是布尔值", None));
                };
                file_changes.push(("lan_access", Some(serde_json::json!(enabled))));
            }
            if let Some(endpoint) = object.get("jev_endpoint") {
                let Some(endpoint) = endpoint.as_str() else {
                    return http_json(400, failure("jev_endpoint 必须是字符串", None));
                };
                let endpoint = endpoint.trim();
                if !endpoint.is_empty() && endpoint != DEFAULT_JEV_ENDPOINT {
                    return http_json(400, failure("Jev 仅支持固定 OpenRouter Decisions 地址", None));
                }
                file_changes.push(("jev_endpoint", (!endpoint.is_empty()).then(|| serde_json::json!(endpoint))));
            }
            let has_mcp_setting = mcp_endpoint.is_some();
            if let Some(endpoint) = mcp_endpoint {
                file_changes.push(("mcp_endpoint", endpoint.map(serde_json::Value::String)));
            }
            if !file_changes.is_empty() {
                let written = update_runtime_settings(self.runtime_path.as_deref(), |current| {
                    for (key, value) in file_changes {
                        match value {
                            Some(value) => current.insert(key.to_owned(), value),
                            None => current.remove(key),
                        };
                    }
                });
                if let Err(error) = written {
                    return http_json(500, failure(format!("设置没有保存：{error}"), Some(serde_json::json!({"error_code": "settings_write_failed"}))));
                }
            }
            if let Some(api_key) = object.get("jev_api_key") {
                let Some(api_key) = api_key.as_str() else {
                    return http_json(400, failure("jev_api_key 必须是字符串", None));
                };
                if !api_key.trim().is_empty() {
                    let Some(db) = self.runtime_db.as_ref() else {
                        return http_json(503, failure("运行时数据库未启用，无法保存 Jev API Key", None));
                    };
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_secs_f64())
                        .unwrap_or(0.0);
                    if let Err(error) = db
                        .lock()
                        .map_err(|_| "运行时数据库锁失败".to_owned())
                        .and_then(|db| db.set_secret(JEV_API_KEY_SECRET, api_key.trim(), now).map_err(|error| format!("{error:?}")))
                    {
                        return http_json(500, failure(error, None));
                    }
                }
            }
            if object.get("jev_delete_api_key").and_then(|value| value.as_bool()).unwrap_or(false) {
                if let Some(db) = self.runtime_db.as_ref() {
                    if let Err(error) = db
                        .lock()
                        .map_err(|_| "运行时数据库锁失败".to_owned())
                        .and_then(|db| db.delete_secret(JEV_API_KEY_SECRET).map_err(|error| format!("{error:?}")))
                    {
                        return http_json(500, failure(error, None));
                    }
                }
            }
            return http_json(
                200,
                success(serde_json::json!({
                    "message": if has_mcp_setting {
                        "设置已保存，小智 MCP 端点将在下次启动时生效"
                    } else {
                        "设置已保存，将在下次启动时生效"
                    }
                })),
            );
        }
        if method == "GET" && route == "/v1/events" {
            let workflow_id = path
                .split_once('?')
                .and_then(|(_, query)| query_param(query, "workflow_id"))
                .unwrap_or_default();
            let status = self
                .store
                .as_ref()
                .and_then(|store| {
                    store
                        .get(laser_runtime::RecordKind::Workflow, &workflow_id)
                        .ok()
                        .flatten()
                })
                .and_then(|record| {
                    record.status.or_else(|| {
                        record
                            .payload
                            .get("status")
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                    })
                })
                .unwrap_or_else(|| "connected".to_owned());
            return http_sse(format!(
                "event: ready\ndata: {}\n\n",
                serde_json::json!({"workflow_id": workflow_id, "status": status})
            ));
        }
        if method == "GET" {
            let asset = if route == "/" || is_spa_route(route) {
                Some("index.html".to_owned())
            } else if let Some(path) = route.strip_prefix("/assets/") {
                safe_asset_path(path).map(|value| format!("assets/{value}"))
            } else if let Some(path) = route.strip_prefix("/vendor/") {
                safe_asset_path(path).map(|value| format!("vendor/{value}"))
            } else {
                None
            };
            if let Some(asset) = asset {
                return match FrontendFixture::get(&asset) {
                    Some(file) => {
                        http_bytes(200, embedded_content_type(&asset), file.data.to_vec())
                    }
                    None if is_spa_route(route) => http_bytes(
                        200,
                        "text/html; charset=utf-8",
                        FrontendFixture::get("index.html")
                            .map(|file| file.data.to_vec())
                            .unwrap_or_default(),
                    ),
                    None => http_json(404, failure("未找到路径", None)),
                };
            }
        }
        if method == "GET" && route == "/preview" {
            let query = path.split_once('?').map(|(_, value)| value).unwrap_or("");
            let requested = query_param(query, "path").map(|value| std::path::PathBuf::from(value));
            let Some(requested) = requested else {
                return http_json(404, failure("缺少 path", None));
            };
            let resolved = match resolve_preview_file(&requested, &self.preview_roots) {
                Ok(path) => path,
                Err(error) => return http_json(404, failure(error, None)),
            };
            let bytes = match std::fs::read(&resolved) {
                Ok(bytes) => bytes,
                Err(_) => return http_json(404, failure("文件不存在", None)),
            };
            let is_download = query_param(query, "download").is_some_and(|v| v == "1" || v == "true");
            let disposition = if is_download {
                let fname = requested.file_name().and_then(|v| v.to_str()).unwrap_or("download");
                Some(format!("attachment; filename=\"{fname}\""))
            } else {
                None
            };
            return http_bytes_with_disposition(200, content_type(&resolved), disposition.as_deref(), bytes);
        }
        if method == "GET" && route == "/v1/material-lab/bootstrap" {
            let source_path = self
                .settings
                .material_params_file
                .as_deref()
                .and_then(|value| std::path::Path::new(value).file_name())
                .and_then(|value| value.to_str())
                .unwrap_or("");
            let materials = load_material_library(&self.settings).unwrap_or_else(|_| self.materials.clone());
            return http_json(
                200,
                success(serde_json::json!({
                    "version": 1,
                    "materials": materials,
                    "error": serde_json::Value::Null,
                    "prototype": false,
                    "defaults": {
                        "network_host": "",
                        "network_telnet_port": 23
                    },
                    "source_path": source_path,
                    "backup_path": ""
                })),
            );
        }
        if method == "GET" && route == "/api/ui/material-recommendation" {
            let query = path.split_once('?').map(|(_, value)| value).unwrap_or("");
            let material = query_param(query, "material").unwrap_or_default();
            let thickness =
                query_param(query, "thickness_mm").and_then(|value| value.parse::<f64>().ok());
            let laser_mode = query_param(query, "laser_mode").unwrap_or_default();
            let engraving_mode = query_param(query, "engraving_mode");
            let Some(thickness_mm) = thickness.filter(|value| value.is_finite() && *value > 0.0)
            else {
                return http_json(400, failure("thickness_mm 必须是大于 0 的数字", None));
            };
            if material.trim().is_empty() || laser_mode.trim().is_empty() {
                return http_json(400, failure("material 和 laser_mode 不能为空", None));
            }
            let power_scale = query_param(query, "power_scale").and_then(|value| value.parse::<f64>().ok()).unwrap_or(1000.0);
            let recommendation = laser_core::material::recommend_with_scale(
                &material,
                thickness_mm,
                &laser_mode,
                engraving_mode.as_deref(),
                power_scale,
            );
            let Some(recommendation) = recommendation else {
                return http_json(400, failure("读取材料推荐失败", None));
            };
            let match_name = if (recommendation.requested_thickness_mm
                - recommendation.matched_thickness_mm)
                .abs()
                < 1e-9
            {
                "exact"
            } else {
                "nearest_thickness"
            };
            let mut result = serde_json::json!({
                "material": recommendation.material,
                "thickness_mm": recommendation.requested_thickness_mm,
                "matched_thickness_mm": recommendation.matched_thickness_mm,
                "match": match_name,
                "laser_mode": recommendation.laser_mode,
                "laser_max_power": recommendation.params.laser_max_power,
                "feed_rate": recommendation.params.feed_rate,
                "passes": recommendation.params.passes,
            });
            if let Some(mode) = recommendation.engraving_mode {
                result["engraving_mode"] = serde_json::Value::String(mode);
            }
            return http_json(200, success(result));
        }
        if method == "POST"
            && matches!(
                route,
                "/api/workflow" | "/api/text-task/generate" | "/api/text-task/send"
            )
        {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let mut object = object;
            strip_identity_fields(&mut object);
            if route == "/api/text-task/generate" {
                match generate_text_artifacts(
                    &object,
                    self.generated_root
                        .as_deref()
                        .or(self.upload_root.as_deref()),
                ) {
                    Ok(artifacts) => {
                        object.insert("_artifacts".into(), serde_json::Value::Object(artifacts));
                    }
                    Err(error) => return http_json(400, failure(error, None)),
                }
            } else if route == "/api/workflow" {
                let action = object.get("action").and_then(|v| v.as_str()).unwrap_or("");
                if action == "preview" || action == "create" || action == "plan" {
                    let source_type = object.get("source_type").and_then(|v| v.as_str()).unwrap_or("");
                    if source_type == "image" || source_type == "gcode" {
                        match generate_image_or_gcode_artifacts(
                            &object,
                            self.upload_root.as_deref(),
                            self.generated_root.as_deref(),
                            &self.preview_roots,
                        ) {
                            Ok(Some(artifacts)) => {
                                object.insert("_artifacts".into(), serde_json::Value::Object(artifacts));
                            }
                            Ok(None) => {}
                            Err(error) => return http_json(400, failure(error, None)),
                        }
                    }
                }
            }
            let send_roots = self.send_gcode_roots();
            let launcher = |workflow_id: &str,
                            gcode_path: std::path::PathBuf,
                            expected_sha: &str,
                            request: &serde_json::Map<String, serde_json::Value>| {
                self.launch_send_job(workflow_id, gcode_path, expected_sha, request, &ctx)
            };
            if let Some(store) = self.store.as_ref() {
                return handle_workflow_store_request(
                    store.as_ref(),
                    route,
                    &object,
                    &send_roots,
                    &self.active_jobs,
                    &launcher,
                );
            }
            return handle_workflow_request(
                &self.workflows,
                route,
                &object,
                &send_roots,
                &self.active_jobs,
                &launcher,
            );
        }
        if method == "POST" && route == "/api/safe-action" {
            let mut object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            strip_identity_fields(&mut object);
            let mut args: laser_contracts::LaserSafeActionArgs = match serde_json::from_value(serde_json::Value::Object(object.clone())) {
                Ok(a) => a,
                Err(e) => return http_json(400, failure(format!("参数解析失败: {e}"), None)),
            };
            // 严格模式检查点（技术书 §6.2）：只对 R3 动作；急停、软复位、关激光、降低倍率永不追加条件（A-27）
            let mut authority = approval::R3Authority::Unchecked;
            if laser_contracts::safe_action_requires_approval(&args.action) {
                let action = args.action.trim().to_owned();
                let facts = approval::safe_action_digest_facts(&object);
                let summary = safe_action_approval_summary(&action, &facts);
                authority = match self.require_r3_authority(
                    &ctx,
                    approval::R3Action {
                        kind: laser_contracts::ApprovalKind::SafeAction,
                        target_record: &action,
                        facts,
                        request: &object,
                        summary,
                    },
                ) {
                    Ok(authority) => authority,
                    Err((status, body)) => return http_json(status, body),
                };
                if let approval::R3Authority::Approval(record) = &authority {
                    if let Some(connection) = record.connection.as_ref() {
                        apply_connection_to_safe_action(&mut args, connection);
                    }
                }
            }
            let job_id = object.get("job_id").and_then(serde_json::Value::as_str);
            let res = self.handle_safe_action_for(&args, &ctx, job_id);
            let outcome: Result<serde_json::Value, (u16, serde_json::Value)> = if res.success {
                Ok(serde_json::Value::Null)
            } else {
                Err((400, failure(res.result.clone(), None)))
            };
            self.finish_r3_authority(&authority, &outcome, None);
            return http_json(
                if res.success { 200 } else { 400 },
                serde_json::json!({
                    "success": res.success,
                    "result": res.result,
                    "detail": res.detail,
                }),
            );
        }
        if method == "POST" && route == "/api/connection/check" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return http_json(200, success(self.probe_connections(&object)));
        }
        if method == "POST" && route == "/api/device/framing" {
            let mut object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            strip_identity_fields(&mut object);
            // 严格模式检查点（技术书 §6.2）：走边框真实驱动机器
            let framing_facts = approval::framing_digest_facts(&object);
            let framing_summary = framing_approval_summary(&framing_facts);
            let framing_authority = match self.require_r3_authority(
                &ctx,
                approval::R3Action {
                    kind: laser_contracts::ApprovalKind::Framing,
                    target_record: "framing",
                    facts: framing_facts,
                    request: &object,
                    summary: framing_summary,
                },
            ) {
                Ok(authority) => authority,
                Err((status, body)) => return http_json(status, body),
            };
            if let approval::R3Authority::Approval(record) = &framing_authority {
                match record.connection.as_ref() {
                    Some(connection) => object = approval::object_with_connection(&object, connection),
                    None => {
                        let (status, body) = approval::error_response(&approval::ApprovalError::Mismatch);
                        return http_json(status, body);
                    }
                }
            }
            let response = self.run_framing(&object, &ctx);
            let outcome: Result<serde_json::Value, (u16, serde_json::Value)> =
                if response.starts_with(b"HTTP/1.1 200") { Ok(serde_json::Value::Null) } else { Err((400, failure("走边框未完成", None))) };
            self.finish_r3_authority(&framing_authority, &outcome, None);
            return response;
        }
        if method == "POST" && route == "/api/draw/preview" {
            let object = match parse_json_object(&body[..content_length], 10 * 1024 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let artifacts = match generate_draw_artifacts(
                &object,
                self.generated_root
                    .as_deref()
                    .or(self.upload_root.as_deref()),
            ) {
                Ok(a) => a,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let workflow_id = format!("draw-{}", next_workflow_id());
            let material = artifacts.get("material").and_then(|v| v.as_str()).unwrap_or("");
            let thickness_mm = artifacts.get("thickness_mm").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let img_name = artifacts.get("image_file").and_then(|v| v.as_str()).unwrap_or("");
            let gcode_name = artifacts.get("gcode_file").and_then(|v| v.as_str()).unwrap_or("");
            let scene_name = artifacts.get("scene_file").and_then(|v| v.as_str()).unwrap_or("");
            let path_preview_name = artifacts.get("path_preview_file").and_then(|v| v.as_str()).unwrap_or(img_name);
            let fingerprint = artifacts.get("fingerprint").and_then(|v| v.as_str()).unwrap_or("");
            let gcode_sha = artifacts.get("gcode_sha256").and_then(|v| v.as_str()).unwrap_or("");
            let est_secs = artifacts
                .get("time_estimate")
                .and_then(|t| t.get("estimated_seconds"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);

            let mut input = object.clone();
            input.remove("image_data_url");
            let mut record = serde_json::json!({
                "workflow_id": workflow_id,
                "status": "preview_ready",
                "source_type": "draw",
                "input": input,
                "material": material,
                "thickness_mm": thickness_mm,
                "artifacts": artifacts,
                "confirmation_snapshot": {
                    "gcode_file": gcode_name,
                    "gcode_sha256": gcode_sha,
                    "material": material,
                    "thickness_mm": thickness_mm,
                },
                "draw_lab": {
                    "job_summary": format!("画板任务就绪 · 预估 {:.1} 秒", est_secs),
                    "content_fingerprint": fingerprint,
                    "uploaded_image_file": img_name,
                    "scene_file": scene_name,
                },
                "web": {
                    "material_preview_url": format!("/preview?path={img_name}"),
                    "uploaded_image_url": format!("/preview?path={img_name}"),
                    "gcode_preview_url": format!("/preview?path={path_preview_name}"),
                    "image_preview_url": format!("/preview?path={path_preview_name}"),
                    "gcode_download_url": format!("/preview?path={gcode_name}&download=1"),
                    "scene_download_url": if scene_name.is_empty() { "".into() } else { format!("/preview?path={scene_name}&download=1") }
                },
                "next_actions": ["status", "cancel", "confirm_send"],
                "created_at": unix_seconds(),
                "updated_at": unix_seconds()
            });
            feedback::add_draw_view(&mut record);
            if let Some(store) = self.store.as_ref() {
                let stored = laser_runtime::StoredRecord {
                    kind: laser_runtime::RecordKind::Workflow,
                    id: workflow_id.clone(),
                    status: Some("preview_ready".into()),
                    payload: record.clone(),
                };
                if store.put(&stored).is_err() {
                    return http_json(500, failure("画板预览保存失败", None));
                }
            } else {
                let mut map = self.workflows.lock().unwrap();
                map.insert(workflow_id.clone(), record.clone());
            }
            return http_json(200, success(record));
        }
        if method == "POST" && route == "/api/material-lab/preview" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return match self.material_lab_preview(&object) {
                Ok(record) => http_json(200, success(record)),
                Err((status, body)) => http_json(status, body),
            };
        }
        if method == "POST" && route == "/api/material-lab/send" {
            let mut object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            strip_identity_fields(&mut object);
            return match self.material_lab_send(&object, &ctx) {
                Ok(record) => http_json(200, success(record)),
                Err((status, body)) => http_json(status, body),
            };
        }
        if method == "POST" && route == "/api/material-lab/select-cell" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return match self.material_lab_select_cell(&object) {
                Ok(record) => http_json(200, success(record)),
                Err((status, body)) => http_json(status, body),
            };
        }
        if method == "POST" && route == "/api/material-lab/status" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let calibration_id = object.get("calibration_id").and_then(serde_json::Value::as_str).unwrap_or("").trim();
            if calibration_id.is_empty() {
                return http_json(400, failure("calibration_id 不能为空", None));
            }
            return match self.load_calibration(calibration_id) {
                Some(mut record) => {
                    overlay_live_progress(&mut record, &self.active_jobs, calibration_id);
                    http_json(200, success(record))
                }
                None => http_json(404, failure("未找到标定矩阵", None)),
            };
        }
        if method == "POST" && route == "/api/upload" {
            let object = match parse_json_object(&body[..content_length], 20 * 1024 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let Some(root) = self.upload_root.as_ref() else {
                return http_json(500, failure("上传目录未配置", None));
            };
            let filename = object
                .get("filename")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let encoded = object
                .get("content_base64")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            return match save_upload(filename, encoded, root, unix_seconds()) {
                Ok(upload) => http_json(
                    200,
                    success(serde_json::json!({
                        "file_path": upload.filename.clone(),
                        "preview_url": format!("/preview?path={}", upload.filename),
                        "filename": upload.filename,
                        "size": upload.size,
                        "suffix": upload.suffix,
                    })),
                ),
                Err(error) => {
                    let detail = if error == "不支持上传该文件类型" {
                        let basename = filename.rsplit(['\\', '/']).next().unwrap_or(filename);
                        let cleaned = basename
                            .replace(['<', '>', ':', '"', '|', '?', '*'], "_")
                            .chars()
                            .filter(|c| !c.is_control())
                            .collect::<String>();
                        Some(serde_json::json!({
                            "filename": if cleaned.is_empty() { "upload" } else { &cleaned },
                            "allowed": [
                                ".bmp", ".gcode", ".gif", ".jpeg", ".jpg", ".nc", ".png", ".webp"
                            ]
                        }))
                    } else {
                        None
                    };
                    http_json(400, failure(error, detail))
                }
            };
        }
        if method == "GET" && route == "/api/material-lab/export" {
            return material_export_response(&self.settings);
        }
        if (method == "GET" || method == "POST") && route == "/api/material-lab/recent" {
            if let Some(recent) = self.find_recent_material_workflow() {
                return http_json(200, success(recent));
            }
            return http_json(
                400,
                failure("最近的激光 workflow 中没有可保存的完整材料参数", None),
            );
        }
        if method == "POST" && route == "/api/material-lab/manage" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return handle_material_manage(&self.settings, self.runtime_db.as_ref(), &object);
        }
        let response = self.handle(method, route, &body[..content_length]);
        http_json(response.status, response.body)
    }

    pub fn handle(&self, method: &str, path: &str, body: &[u8]) -> ApiResponse {
        let response = match (method, path) {
            ("GET", "/api/health") => (200, success(serde_json::json!({"status": "ok"}))),
            ("GET", "/v1/capabilities") => (
                200,
                success(serde_json::json!({
                    "http": true,
                    "serial": true,
                    "network": true,
                    "shell": false,
                    "tray": cfg!(any(target_os = "windows", target_os = "macos", target_os = "linux")),
                    "command_execution": std::env::var("MOSS_ENABLE_DEVELOPER_COMMANDS")
                        .ok()
                        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                })),
            ),
            ("POST", "/api/ui/discover-controllers") => {
                // mDNS 只读浏览 _telnet._tcp / _http._tcp，约 2 秒；不向设备发送任何命令。
                // 用 POST：跨站页面无法用不带 Origin 的 GET（如 <img>）反复触发局域网扫描
                let controllers = laser_device::discover_controllers(std::time::Duration::from_millis(2000));
                let list: Vec<serde_json::Value> = controllers
                    .iter()
                    .map(|c| serde_json::json!({
                        "hostname": c.hostname,
                        "ip": c.ip,
                        "telnet_port": c.telnet_port,
                        "http_port": c.http_port,
                    }))
                    .collect();
                (
                    200,
                    success(serde_json::json!({
                        "controllers": list,
                        "hint": if list.is_empty() { "未发现广播 mDNS 的控制器；FluidNC 需开启 $MDNS/Enable=ON，且电脑与机器在同一网段" } else { "" },
                    })),
                )
            }
            ("GET", "/api/ui/serial-ports") => {
                let ports = (self.serial_port_lister)();
                let default_port = ports.first().cloned().unwrap_or_default();
                (
                    200,
                    success(serde_json::json!({
                        "ports": ports,
                        "default_port": default_port,
                        "baudrate": 115200
                    })),
                )
            }
            ("GET", "/api/ui/runtime-config") => (
                200,
                success(serde_json::json!({
                    "connection": {
                        "default_mode": self.settings.default_connection_mode,
                        "preferred_modes": [self.settings.default_connection_mode, if self.settings.default_connection_mode == "network" {"serial"} else {"network"}],
                        "serial": {"default_port": "", "baudrate": 115200},
                        "network": {
                            "default_host": "",
                            "transport": "telnet",
                            "http_port": 80,
                            "telnet_port": 23,
                            "timeout": 5.0
                        }
                    },
                    "upload": {"gcode_suffixes": [".gcode", ".nc"]}
                })),
            ),
            ("GET", "/api/ui/material-options") => {
                let library = load_material_library(&self.settings).unwrap_or_else(|_| self.materials.clone());
                let materials = library
                    .materials
                    .iter()
                    .map(|(name, entry)| {
                        let mut thicknesses = entry
                            .thicknesses
                            .keys()
                            .filter_map(|value| value.parse::<f64>().ok())
                            .collect::<Vec<_>>();
                        thicknesses.sort_by(f64::total_cmp);
                        serde_json::json!({"name": name, "thicknesses": thicknesses})
                    })
                    .collect::<Vec<_>>();
                (200, success(serde_json::json!({"materials": materials})))
            }
            ("GET", "/api/material-lab/export") => (
                200,
                success(serde_json::json!({
                    "params_file": self.settings.material_params_file.as_deref().unwrap_or(""),
                    "library": self.materials,
                })),
            ),
            ("GET", "/api/material-lab/recent") => (
                400,
                failure("最近的激光 workflow 中没有可保存的完整材料参数", None),
            ),
            ("GET", "/v1/material-lab/bootstrap") => {
                let source_path = self
                    .settings
                    .material_params_file
                    .as_deref()
                    .and_then(|value| std::path::Path::new(value).file_name())
                    .and_then(|value| value.to_str())
                    .unwrap_or("");
                (
                    200,
                    success(serde_json::json!({
                        "version": 1,
                        "materials": self.materials,
                        "error": serde_json::Value::Null,
                        "prototype": false,
                        "defaults": {"network_host": "", "network_telnet_port": 23},
                        "source_path": source_path,
                        "backup_path": ""
                    })),
                )
            }
            ("POST", _) => match parse_json_object(body, 64 * 1024) {
                Ok(_) => (404, failure("未找到路径", None)),
                Err(error) => (400, failure(error, None)),
            },
            _ => (404, failure("未找到路径", None)),
        };
        ApiResponse {
            status: response.0,
            headers: vec![
                ("Cache-Control", "no-store".into()),
                ("Content-Type", "application/json; charset=utf-8".into()),
            ],
            body: response.1,
        }
    }
}

fn material_library_path(settings: &laser_runtime::LaserSettings) -> Option<std::path::PathBuf> {
    settings
        .material_params_file
        .as_deref()
        .map(std::path::PathBuf::from)
}

fn generate_text_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    generated_root: Option<&std::path::Path>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let text = object
        .get("text")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err("text 不能为空".into());
    }
    let material = object
        .get("material")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    validate_material_safety(material)?;
    let thickness = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0)
        .map_err(|_| "thickness_mm 必须是大于 0 的数字")?;
    if material.is_empty() || !thickness.is_finite() || thickness <= 0.0 {
        return Err("thickness_mm 必须是大于 0 的数字".into());
    }
    let font_path_buf = object
        .get("font_path")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| laser_core::font::find_system_font_for_text(text))
        .ok_or("font_path 必须指向可读的 TTF/TTC 字体")?;
    let font_path = font_path_buf.to_str().ok_or("非法字体路径")?;
    let font_size = parse_float(object.get("font_size_mm"), "font_size_mm", 10.0)
        .map_err(|error| error.to_string())?;
    let spacing = parse_float(object.get("letter_spacing_mm"), "letter_spacing_mm", 0.5)
        .map_err(|error| error.to_string())?;
    let recommendation = laser_core::material::recommend(
        material,
        thickness,
        "engrave",
        object
            .get("engraving_mode")
            .and_then(|value| value.as_str()),
    )
    .ok_or("读取材料推荐失败")?;

    let params = feedback::generation_params(object, recommendation.params.laser_max_power as f64, recommendation.params.feed_rate as f64)?;
    let spec = laser_core::TextTaskSpec {
        text,
        font_path,
        font_size_mm: font_size,
        letter_spacing_mm: spacing,
        power: params.power as i32,
        feed_rate: params.feed as i32,
        preview_width_px: 800,
        preview_height_px: 600,
    };
    let artifacts = laser_core::TextTaskEngine::generate(&spec)?;
    let gcode = feedback::repeat_generated(&artifacts.gcode, params.passes);
    let estimate = laser_core::gcode::estimate_time(&gcode, 3000.0);

    let root = generated_root.ok_or("生成目录未配置")?;
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = format!("text-{}-{}-{}", unix_seconds(), nanos, std::process::id());
    let gcode_path = root.join(format!("{id}.gcode"));
    let preview_path = root.join(format!("{id}.png"));
    std::fs::write(&gcode_path, gcode.as_bytes()).map_err(|error| error.to_string())?;
    artifacts
        .preview
        .save(&preview_path)
        .map_err(|error| error.to_string())?;

    let gcode_sha = hex::encode(sha2::Sha256::digest(gcode.as_bytes()));

    Ok(serde_json::json!({
        "gcode_file": gcode_path.file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "gcode_sha256": gcode_sha,
        "actual_params": params.json(),
        "image_file": preview_path.file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "font_path": std::path::Path::new(font_path).file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "material": recommendation.material,
        "thickness_mm": thickness,
        "width_mm": artifacts.width_mm,
        "height_mm": artifacts.height_mm,
        "glyphs": artifacts.glyphs,
        "path_optimization": artifacts.path_optimization,
        "time_estimate": {
            "estimated_seconds": estimate.estimated_seconds,
            "estimated_minutes": estimate.estimated_minutes,
            "laser_seconds": estimate.laser_seconds,
            "non_laser_seconds": estimate.non_laser_seconds
        }
    }).as_object().cloned().unwrap_or_default())
}

fn generate_draw_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    generated_root: Option<&std::path::Path>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let image_data_url = object
        .get("image_data_url")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if image_data_url.is_empty() {
        return Err("image_data_url 不能为空".into());
    }
    let material = object
        .get("material")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    validate_material_safety(material)?;
    let thickness = parse_float(object.get("thickness_mm"), "thickness_mm", 0.0)
        .map_err(|_| "thickness_mm 必须是大于 0 的数字")?;
    if material.is_empty() || !thickness.is_finite() || thickness <= 0.0 {
        return Err("thickness_mm 必须是大于 0 的数字".into());
    }

    let mode = object.get("mode").and_then(|v| v.as_str()).unwrap_or("raster");

    let data_part = if let Some((_, b64)) = image_data_url.split_once(',') {
        b64
    } else {
        image_data_url
    };
    let image_bytes = base64::engine::general_purpose::STANDARD
        .decode(data_part.trim())
        .map_err(|e| format!("Base64 解码失败: {e}"))?;

    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(&image_bytes);
    let fingerprint = hex::encode(hasher.finalize());

    let root = generated_root.ok_or("生成目录未配置")?;
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = format!("draw-{}-{}-{}", unix_seconds(), nanos, std::process::id());

    let image_path = root.join(format!("{id}.png"));
    std::fs::write(&image_path, &image_bytes).map_err(|error| error.to_string())?;

    let scene_name = if let Some(scene) = object.get("scene_json") {
        let scene_path = root.join(format!("{id}.scene.json"));
        let scene_bytes = serde_json::to_vec_pretty(scene).map_err(|e| e.to_string())?;
        std::fs::write(&scene_path, scene_bytes).map_err(|error| error.to_string())?;
        format!("{id}.scene.json")
    } else {
        String::new()
    };

    let luma_img = image::load_from_memory(&image_bytes)
        .map_err(|e| format!("图像加载失败: {e}"))?
        .to_luma8();
    let img_w = luma_img.width();
    let img_h = luma_img.height();
    if img_w == 0 || img_h == 0 {
        return Err("图像尺寸为 0".into());
    }

    let recommendation = laser_core::material::recommend(
        material,
        thickness,
        if mode == "cut" { "cut" } else { "engrave" },
        Some(mode),
    );
    let power = recommendation.as_ref().map(|r| r.params.laser_max_power).unwrap_or(400);
    let feed_rate = recommendation.as_ref().map(|r| r.params.feed_rate).unwrap_or(1500);
    let params = feedback::generation_params(object, power as f64, feed_rate as f64)?;
    let power = params.power as i32;
    let feed_rate = params.feed as i32;

    // 成品尺寸：填了宽 / 高按填的尺寸（锁定比例，取能放进去的最大尺寸）；都没填按导出图 0.1 mm/像素
    let (width_mm, height_mm) = draw_target_size_mm(
        img_w,
        img_h,
        optional_positive_mm(object.get("width_mm"), "width_mm")?,
        optional_positive_mm(object.get("height_mm"), "height_mm")?,
    )?;
    let path_preview_name = format!("{id}.path.png");
    let gcode_str = if mode == "outline" {
        // 轮廓雕刻：深色区域描边（与 Python 版相同的灰度描摹、简化与点数预算），按成品尺寸缩放
        let gray: Vec<Vec<u8>> = (0..img_h)
            .map(|y| (0..img_w).map(|x| luma_img.get_pixel(x, y).0[0]).collect())
            .collect();
        let simplify_factor = parse_float(object.get("vector_simplify_factor"), "vector_simplify_factor", 3.0)?;
        if !(0.25..=8.0).contains(&simplify_factor) {
            return Err("vector_simplify_factor 必须在 0.25 ~ 8 之间".into());
        }
        let traced = laser_core::trace::trace_grayscale(&gray, &laser_core::trace::TraceOptions {
            size_mm: width_mm.max(height_mm),
            width_mm: Some(width_mm),
            height_mm: Some(height_mm),
            lock_aspect_ratio: true,
            work_area_width_mm: DRAW_WORK_AREA_MM,
            work_area_height_mm: DRAW_WORK_AREA_MM,
            simplify_factor,
        })
        .map_err(|e| e.to_string())?;
        if traced.paths.is_empty() {
            return Err("画板里没有可描边的深色图形，请改用光栅雕刻或加深线条".into());
        }
        let optimized = laser_core::path_opt::optimize_with_threshold(&traced.paths, width_mm.max(height_mm))
            .map_err(|e| e.to_string())?;
        let preview_scale = (1024.0 / width_mm.max(height_mm)).max(1.0);
        laser_core::gcode::preview_outline_rgba(
            &optimized.paths,
            (width_mm * preview_scale).ceil() as u32,
            (height_mm * preview_scale).ceil() as u32,
            width_mm,
            height_mm,
        )?
        .save(root.join(&path_preview_name))
        .map_err(|e| e.to_string())?;
        laser_core::gcode::write_outline(&optimized.paths, &laser_core::gcode::WriterOptions {
            width_mm,
            height_mm,
            power,
            feed_rate,
            auto_close: true,
            laser_mode: "M4",
            power_scale: None,
        })?
    } else {
        // 光栅：先把导出图缩放到成品尺寸（每像素 0.1 mm），再二值化逐行扫描
        let raster_w = ((width_mm / DRAW_PIXEL_MM).round() as u32).max(1);
        let raster_h = ((height_mm / DRAW_PIXEL_MM).round() as u32).max(1);
        let scaled = if (raster_w, raster_h) == (img_w, img_h) {
            luma_img.clone()
        } else {
            image::imageops::resize(&luma_img, raster_w, raster_h, image::imageops::FilterType::Triangle)
        };
        let mut bitmap = Vec::with_capacity(raster_h as usize);
        for y in 0..raster_h {
            let mut row = Vec::with_capacity(raster_w as usize);
            for x in 0..raster_w {
                row.push(if scaled.get_pixel(x, y).0[0] < 128 { 0 } else { 255 });
            }
            bitmap.push(row);
        }

        // 路径预览：二值化后的实际烧蚀像素（黑=出光），与素材预览区分
        let mut bin = image::GrayImage::from_pixel(raster_w, raster_h, image::Luma([255]));
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &v) in row.iter().enumerate() {
                if v == 0 {
                    bin.put_pixel(x as u32, y as u32, image::Luma([0]));
                }
            }
        }
        bin.save(root.join(&path_preview_name)).map_err(|e| e.to_string())?;

        let plan = laser_core::raster::raster_plan_with_direction(&bitmap, "horizontal", "scanline")
            .map_err(|e| e.to_string())?;
        laser_core::gcode::write_raster(
            &plan,
            raster_w as usize,
            raster_h as usize,
            &laser_core::gcode::RasterWriterOptions {
                pixel_size_mm: DRAW_PIXEL_MM,
                overscan_mm: 1.0,
                work_area_width_mm: DRAW_WORK_AREA_MM,
                work_area_height_mm: DRAW_WORK_AREA_MM,
                power,
                feed_rate,
                laser_mode: "M4",
                snake_scan: true,
                material: Some(material.to_string()),
                safety_note: None,
                power_scale: None,
            },
        ).map_err(|e| e.to_string())?
    };

    let gcode_str = feedback::repeat_generated(&gcode_str, params.passes);
    let gcode_path = root.join(format!("{id}.gcode"));
    std::fs::write(&gcode_path, gcode_str.as_bytes()).map_err(|error| error.to_string())?;

    let estimate = laser_core::gcode::estimate_time(&gcode_str, 3000.0);

    let img_name = format!("{id}.png");
    let gcode_name = format!("{id}.gcode");

    let mut res_map = serde_json::Map::new();
    res_map.insert("actual_params".into(), params.json());
    res_map.insert("image_file".into(), serde_json::Value::String(img_name));
    res_map.insert("gcode_file".into(), serde_json::Value::String(gcode_name));
    res_map.insert(
        "gcode_sha256".into(),
        serde_json::Value::String(hex::encode(sha2::Sha256::digest(gcode_str.as_bytes()))),
    );
    res_map.insert("scene_file".into(), serde_json::Value::String(scene_name));
    res_map.insert("path_preview_file".into(), serde_json::Value::String(path_preview_name));
    res_map.insert("material".into(), serde_json::Value::String(material.to_string()));
    res_map.insert("thickness_mm".into(), serde_json::json!(thickness));
    res_map.insert("width_mm".into(), serde_json::json!(width_mm));
    res_map.insert("height_mm".into(), serde_json::json!(height_mm));
    res_map.insert("mode".into(), serde_json::Value::String(mode.to_string()));
    res_map.insert("time_estimate".into(), serde_json::json!({
        "estimated_seconds": estimate.estimated_seconds,
        "estimated_minutes": estimate.estimated_minutes,
        "laser_seconds": estimate.laser_seconds,
        "non_laser_seconds": estimate.non_laser_seconds,
    }));
    res_map.insert("fingerprint".into(), serde_json::Value::String(fingerprint));

    Ok(res_map)
}

/// 画板光栅每像素对应的物理尺寸与工作区上限（见 safety.md §4）。
const DRAW_PIXEL_MM: f64 = 0.1;
const DRAW_WORK_AREA_MM: f64 = 400.0;

/// 可选的正数尺寸：缺省、null、空串或 0 表示“自动”。
fn optional_positive_mm(value: Option<&serde_json::Value>, field: &str) -> Result<Option<f64>, String> {
    let number = match value {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::String(text)) if text.trim().is_empty() => return Ok(None),
        Some(serde_json::Value::String(text)) => text.trim().parse::<f64>().map_err(|_| format!("{field} 必须是数字"))?,
        Some(other) => other.as_f64().ok_or_else(|| format!("{field} 必须是数字"))?,
    };
    if !number.is_finite() || number < 0.0 {
        return Err(format!("{field} 必须是不小于 0 的数字"));
    }
    Ok((number > 0.0).then_some(number))
}

/// 画板成品尺寸（毫米）：宽高都填时锁定比例放进该框内；只填一边按比例算另一边；
/// 都没填按导出图每像素 0.1 mm。超出工作区直接拒绝，不悄悄缩小。
fn draw_target_size_mm(img_w: u32, img_h: u32, width: Option<f64>, height: Option<f64>) -> Result<(f64, f64), String> {
    let (w, h) = (f64::from(img_w), f64::from(img_h));
    let (target_w, target_h) = match (width, height) {
        (Some(tw), Some(th)) => {
            let scale = (tw / w).min(th / h);
            (w * scale, h * scale)
        }
        (Some(tw), None) => (tw, tw * h / w),
        (None, Some(th)) => (th * w / h, th),
        (None, None) => (w * DRAW_PIXEL_MM, h * DRAW_PIXEL_MM),
    };
    if target_w > DRAW_WORK_AREA_MM + 1e-9 || target_h > DRAW_WORK_AREA_MM + 1e-9 {
        return Err(format!(
            "成品尺寸 {target_w:.1} × {target_h:.1} mm 超出工作区 {DRAW_WORK_AREA_MM:.0} × {DRAW_WORK_AREA_MM:.0} mm，请减小宽或高"
        ));
    }
    if target_w < DRAW_PIXEL_MM || target_h < DRAW_PIXEL_MM {
        return Err("成品尺寸太小，宽和高都至少 0.1 mm".into());
    }
    Ok((target_w, target_h))
}

fn generate_image_or_gcode_artifacts(
    object: &serde_json::Map<String, serde_json::Value>,
    upload_root: Option<&std::path::Path>,
    generated_root: Option<&std::path::Path>,
    preview_roots: &[std::path::PathBuf],
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    let source_type = object
        .get("source_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let material = object
        .get("material")
        .and_then(|v| v.as_str())
        .unwrap_or("椴木");
    let thickness = parse_float(object.get("thickness_mm"), "thickness_mm", 3.0).unwrap_or(3.0);

    let root = generated_root
        .or(upload_root)
        .ok_or("生成与上传目录未配置")?;
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);

    if source_type == "image" {
        let image_file = object
            .get("image_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if image_file.is_empty() {
            return Err("未指定图片素材路径".into());
        }

        let image_path = resolve_preview_file(std::path::Path::new(image_file), preview_roots)
            .or_else(|_| {
                if let Some(up) = upload_root {
                    let candidate = up.join(image_file);
                    if candidate.exists() {
                        return Ok(candidate);
                    }
                }
                Err("找不到指定的素材图片文件")
            })
            .map_err(|e| e.to_string())?;

        let image_bytes = std::fs::read(&image_path).map_err(|e| format!("读取素材失败: {e}"))?;
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(&image_bytes);
        let _fingerprint = hex::encode(hasher.finalize());

        let luma_img = image::load_from_memory(&image_bytes)
            .map_err(|e| format!("解析图片格式失败: {e}"))?
            .to_luma8();
        let img_w = luma_img.width();
        let img_h = luma_img.height();
        if img_w == 0 || img_h == 0 {
            return Err("图片尺寸为 0".into());
        }

        let recommendation = laser_core::material::recommend(material, thickness, "engrave", Some("raster"));
        let power = recommendation.as_ref().map(|r| r.params.laser_max_power).unwrap_or(400);
        let feed_rate = recommendation.as_ref().map(|r| r.params.feed_rate).unwrap_or(1500);
        let params = feedback::generation_params(object, power as f64, feed_rate as f64)?;
        let power = params.power as i32;
        let feed_rate = params.feed as i32;

        let mut bitmap = Vec::with_capacity(img_h as usize);
        for y in 0..img_h {
            let mut row = Vec::with_capacity(img_w as usize);
            for x in 0..img_w {
                row.push(if luma_img.get_pixel(x, y).0[0] < 128 { 0 } else { 255 });
            }
            bitmap.push(row);
        }

        let plan = laser_core::raster::raster_plan_with_direction(&bitmap, "horizontal", "scanline")
            .map_err(|e| e.to_string())?;
        let gcode_str = laser_core::gcode::write_raster(
            &plan,
            img_w as usize,
            img_h as usize,
            &laser_core::gcode::RasterWriterOptions {
                pixel_size_mm: 0.1,
                overscan_mm: 1.0,
                work_area_width_mm: 400.0,
                work_area_height_mm: 400.0,
                power,
                feed_rate,
                laser_mode: "M4",
                snake_scan: true,
                material: Some(material.to_string()),
                safety_note: None,
                power_scale: None,
            },
        ).map_err(|e| e.to_string())?;

        let id = format!("img-{}-{}-{}", unix_seconds(), nanos, std::process::id());
        let gcode_str = feedback::repeat_generated(&gcode_str, params.passes);
        let gcode_path = root.join(format!("{id}.gcode"));
        std::fs::write(&gcode_path, gcode_str.as_bytes()).map_err(|e| e.to_string())?;

        let preview_path = root.join(format!("{id}.png"));
        luma_img.save(&preview_path).map_err(|e| e.to_string())?;

        let estimate = laser_core::gcode::estimate_time(&gcode_str, 3000.0);
        let width_mm = img_w as f64 * 0.1;
        let height_mm = img_h as f64 * 0.1;

        let img_name = format!("{id}.png");
        let gcode_name = format!("{id}.gcode");
        let gcode_sha = hex::encode(sha2::Sha256::digest(gcode_str.as_bytes()));

        let mut res = serde_json::Map::new();
        res.insert("actual_params".into(), params.json());
        res.insert("image_file".into(), serde_json::Value::String(img_name));
        res.insert("gcode_file".into(), serde_json::Value::String(gcode_name));
        res.insert("gcode_sha256".into(), serde_json::Value::String(gcode_sha));
        res.insert("material".into(), serde_json::Value::String(material.to_string()));
        res.insert("thickness_mm".into(), serde_json::json!(thickness));
        res.insert("width_mm".into(), serde_json::json!(width_mm));
        res.insert("height_mm".into(), serde_json::json!(height_mm));
        res.insert("time_estimate".into(), serde_json::json!({
            "estimated_seconds": estimate.estimated_seconds,
            "estimated_minutes": estimate.estimated_minutes,
            "laser_seconds": estimate.laser_seconds,
            "non_laser_seconds": estimate.non_laser_seconds,
        }));
        return Ok(Some(res));
    } else if source_type == "gcode" {
        let gcode_file = object
            .get("gcode_file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if gcode_file.is_empty() {
            return Err("未指定 G-code 刀路路径".into());
        }

        let gcode_path = resolve_preview_file(std::path::Path::new(gcode_file), preview_roots)
            .or_else(|_| {
                if let Some(up) = upload_root {
                    let candidate = up.join(gcode_file);
                    if candidate.exists() {
                        return Ok(candidate);
                    }
                }
                Err("找不到指定的 G-code 刀路文件")
            })
            .map_err(|e| e.to_string())?;

        let gcode_bytes = std::fs::read(&gcode_path).map_err(|e| format!("读取刀路文件失败: {e}"))?;
        let gcode_str = String::from_utf8_lossy(&gcode_bytes);

        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(&gcode_bytes);
        let fingerprint = hex::encode(hasher.finalize());

        let moves = laser_core::gcode::parse_moves(&gcode_str);
        let estimate = laser_core::gcode::estimate_time(&gcode_str, 3000.0);

        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        let mut paths: Vec<Vec<laser_contracts::geometry::Point>> = Vec::new();
        let mut current_path: Vec<laser_contracts::geometry::Point> = Vec::new();
        let mut cur_pos = (0.0, 0.0);

        for m in moves {
            if let Some(x) = m.x {
                cur_pos.0 = x;
            }
            if let Some(y) = m.y {
                cur_pos.1 = y;
            }
            if cur_pos.0.is_finite() && cur_pos.1.is_finite() {
                min_x = min_x.min(cur_pos.0);
                max_x = max_x.max(cur_pos.0);
                min_y = min_y.min(cur_pos.1);
                max_y = max_y.max(cur_pos.1);
            }
            if m.rapid {
                if current_path.len() > 1 {
                    paths.push(std::mem::take(&mut current_path));
                } else {
                    current_path.clear();
                }
                current_path.push(cur_pos);
            } else {
                current_path.push(cur_pos);
            }
        }
        if current_path.len() > 1 {
            paths.push(current_path);
        }

        if !min_x.is_finite() {
            min_x = 0.0;
            max_x = 50.0;
            min_y = 0.0;
            max_y = 50.0;
        }

        let width_mm = (max_x - min_x).max(1.0);
        let height_mm = (max_y - min_y).max(1.0);

        // G-code 机器坐标 Y 轴向上，图像坐标 Y 轴向下：翻转后用户看到的即工件正面视角。
        // 解析出的轨迹已包含刀路里显式写出的闭合段，因此不再自动补画首尾闭合线。
        for path in &mut paths {
            for point in path.iter_mut() {
                point.0 -= min_x;
                point.1 = max_y - point.1;
            }
        }

        let id = format!("gc-{}-{}-{}", unix_seconds(), nanos, std::process::id());
        let preview_path = root.join(format!("{id}.png"));
        let preview_img = laser_core::gcode::preview_outline_rgba_with_options(&paths, 800, 600, width_mm, height_mm, false)
            .map_err(|e| e.to_string())?;
        preview_img.save(&preview_path).map_err(|e| e.to_string())?;

        let img_name = format!("{id}.png");
        let gcode_name = gcode_path.file_name().and_then(|v| v.to_str()).unwrap_or(gcode_file).to_string();

        let mut res = serde_json::Map::new();
        res.insert("image_file".into(), serde_json::Value::String(img_name));
        res.insert("gcode_file".into(), serde_json::Value::String(gcode_name));
        res.insert("gcode_sha256".into(), serde_json::Value::String(fingerprint));
        res.insert("material".into(), serde_json::Value::String(material.to_string()));
        res.insert("thickness_mm".into(), serde_json::json!(thickness));
        res.insert("width_mm".into(), serde_json::json!(width_mm));
        res.insert("height_mm".into(), serde_json::json!(height_mm));
        res.insert("time_estimate".into(), serde_json::json!({
            "estimated_seconds": estimate.estimated_seconds,
            "estimated_minutes": estimate.estimated_minutes,
            "laser_seconds": estimate.laser_seconds,
            "non_laser_seconds": estimate.non_laser_seconds,
        }));
        return Ok(Some(res));
    }

    Ok(None)
}

fn next_workflow_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("wf-{}-{seq}", unix_seconds())
}

/// 终态或发送中的 workflow 不允许再次下发（规格 §8：未确认/已终止任务绝不自动重发）。
#[cfg(windows)]
fn enumerate_system_serial_ports() -> Vec<String> {
    let mut ports = Vec::new();
    let ps = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[System.IO.Ports.SerialPort]::GetPortNames()",
        ])
        .output();
    if let Ok(output) = ps {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("COM") && trimmed.len() >= 4 {
                if !ports.contains(&trimmed.to_string()) {
                    ports.push(trimmed.to_string());
                }
            }
        }
    }
    if ports.is_empty() {
        for n in 1..=32 {
            let com = format!("\\\\.\\COM{n}");
            if std::fs::OpenOptions::new().read(true).write(true).open(&com).is_ok() {
                let name = format!("COM{n}");
                if !ports.contains(&name) {
                    ports.push(name);
                }
            }
        }
    }
    ports.sort();
    ports
}

#[cfg(not(windows))]
fn enumerate_system_serial_ports() -> Vec<String> {
    let mut ports = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("ttyUSB") || name.starts_with("ttyACM") || name.starts_with("cu.") {
                ports.push(format!("/dev/{name}"));
            }
        }
    }
    ports.sort();
    ports
}

fn workflow_send_blocked_reason(status: Option<&str>) -> Option<&'static str> {
    match status {
        Some("cancelled") => Some("该 workflow 已取消，不能再发送，请重新生成预览"),
        Some("failed") => Some("该 workflow 已失败，不能再发送，请重新生成预览"),
        Some("sending") => Some("该 workflow 正在发送中，禁止重复下发"),
        Some("completed") => Some("该 workflow 已完成加工；重复加工同一图案请重新生成预览（防止二次烧蚀）"),
        _ => None,
    }
}

/// 发送准入预检（规格 §8/§22）：把 workflow 记录中的刀路文件名限定在 RuntimeRoot 允许目录内解析为物理文件，
/// 读取（或首次冻结）快照 SHA，交给 `laser_device::authorize_send` 以物理文件重算比对。
/// 返回 (preflight, 冻结的 SHA-256, 记录中的刀路文件名)。
fn build_send_preflight(
    payload: &serde_json::Value,
    send_roots: &[std::path::PathBuf],
) -> Result<(laser_device::SendPreflight, String, String), String> {
    let gcode_file = payload
        .get("artifacts")
        .and_then(|a| a.get("gcode_file"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("confirmation_snapshot")
                .and_then(|c| c.get("gcode_file"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| payload.get("input").and_then(|i| i.get("gcode_file")).and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim();
    if gcode_file.is_empty() {
        return Err("当前 workflow 没有 G-code 刀路文件，拒绝发送".into());
    }
    let gcode_path = resolve_preview_file(std::path::Path::new(gcode_file), send_roots)
        .map_err(|_| "G-code 刀路文件不存在或不在允许目录内，拒绝发送".to_string())?;
    let stored_sha = payload
        .get("artifacts")
        .and_then(|a| a.get("gcode_sha256"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("confirmation_snapshot")
                .and_then(|c| c.get("gcode_sha256"))
                .and_then(|v| v.as_str())
        })
        .map(str::trim)
        .filter(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned);
    let expected_sha256 = match stored_sha {
        Some(sha) => sha,
        None => {
            // 快照尚未冻结 SHA：以当前物理文件首次冻结，之后每次发送都必须与之一致
            let bytes = std::fs::read(&gcode_path).map_err(|e| format!("读取 G-code 文件失败: {e}"))?;
            hex::encode(Sha256::digest(&bytes))
        }
    };
    let preflight = laser_device::SendPreflight {
        status: "pending".into(),
        expected_sha256: expected_sha256.clone(),
        actual_sha256: None,
        gcode_path: Some(gcode_path),
        workflow_snapshot_valid: true,
        calibration_sha_valid: false,
    };
    Ok((preflight, expected_sha256, gcode_file.to_owned()))
}

/// 通过门禁后把刀路文件名与 SHA 冻结进 confirmation_snapshot（保留画板等已有字段）。
fn freeze_confirmation_snapshot(
    map: &mut serde_json::Map<String, serde_json::Value>,
    gcode_file: &str,
    gcode_sha: &str,
) {
    let now = unix_seconds();
    match map
        .get_mut("confirmation_snapshot")
        .and_then(|value| value.as_object_mut())
    {
        Some(snapshot) => {
            snapshot.insert("gcode_file".into(), serde_json::Value::String(gcode_file.to_owned()));
            snapshot.insert("gcode_sha256".into(), serde_json::Value::String(gcode_sha.to_owned()));
            snapshot.insert("confirmed_at".into(), serde_json::json!(now));
        }
        None => {
            map.insert(
                "confirmation_snapshot".into(),
                serde_json::json!({
                    "gcode_file": gcode_file,
                    "gcode_sha256": gcode_sha,
                    "confirmed_at": now
                }),
            );
        }
    }
}

/// 后台发送线程收尾：把终态与 send_result 写回存储（SQLite/内存）。
fn finalize_workflow_record(
    store: Option<&(dyn laser_runtime::RuntimeStore + Send + Sync)>,
    workflows: &std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>>,
    kind: laser_runtime::RecordKind,
    workflow_id: &str,
    status: &str,
    send_result: serde_json::Value,
) {
    let now = unix_seconds();
    if let Some(store) = store {
        if let Ok(Some(mut stored)) = store.get(kind, workflow_id) {
            // 取消请求可能已由 HTTP 层写入 cancelled；不要用 completed 覆盖用户取消
            let already_cancelled = stored.status.as_deref() == Some("cancelled");
            let final_status = if already_cancelled && status == "completed" { "cancelled" } else { status };
            stored.status = Some(final_status.into());
            if let Some(map) = stored.payload.as_object_mut() {
                map.insert("status".into(), serde_json::Value::String(final_status.into()));
                map.insert("updated_at".into(), serde_json::json!(now));
                map.insert("send_result".into(), send_result);
            }
            let _ = store.put(&stored);
        }
        return;
    }
    if let Ok(mut records) = workflows.lock() {
        if let Some(record) = records.get_mut(workflow_id) {
            let already_cancelled = record.get("status").and_then(|v| v.as_str()) == Some("cancelled");
            let final_status = if already_cancelled && status == "completed" { "cancelled" } else { status };
            record["status"] = serde_json::Value::String(final_status.into());
            record["updated_at"] = serde_json::json!(now);
            record["send_result"] = send_result;
        }
    }
}

fn overlay_live_progress(payload: &mut serde_json::Value, jobs: &ActiveJobs, workflow_id: &str) {
    if let Ok(jobs) = jobs.lock() {
        if let Some(job) = jobs.get(workflow_id) {
            if let Some(map) = payload.as_object_mut() {
                map.insert("send_result".into(), job.live_send_result());
                map.insert("status".into(), serde_json::Value::String("sending".into()));
            }
        }
    }
}

/// 取消：置令牌（worker 在下一行前执行 ! → ? → 0x18），并立即通过实时信道写 `!` 进给保持。
fn signal_job_cancel(jobs: &ActiveJobs, workflow_id: &str) -> bool {
    let job = match jobs.lock() {
        Ok(guard) => guard.get(workflow_id).cloned(),
        Err(_) => None,
    };
    match job {
        Some(job) => {
            job.cancel.cancel();
            let _ = job.transport.write_realtime(laser_device::RealtimeCommand::FeedHold);
            true
        }
        None => false,
    }
}

fn handle_workflow_store_request(
    store: &dyn laser_runtime::RuntimeStore,
    route: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    send_roots: &[std::path::PathBuf],
    jobs: &ActiveJobs,
    launch: SendLauncher<'_>,
) -> Vec<u8> {
    let action = if route == "/api/text-task/generate" {
        "preview"
    } else if route == "/api/text-task/send" {
        "confirm_send"
    } else {
        object
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("status")
    };
    let requested_id = object
        .get("workflow_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    if action == "recent" || (action == "status" && requested_id.is_none()) {
        let records = match store.list_recent(laser_runtime::RecordKind::Workflow, 50) {
            Ok(records) => records,
            Err(_) => return http_json(500, failure("读取 workflow 失败", None)),
        };
        let workflows = records
            .into_iter()
            .map(|record| record.payload)
            .collect::<Vec<_>>();
        return http_json(
            200,
            success(serde_json::json!({
                "workflows": workflows,
                "speech": "已列出最近的激光 workflow；可以用 workflow_id 继续查询、反馈或重生成。"
            })),
        );
    }
    match action {
        "preview" | "plan" | "create" => {
            let workflow_id = requested_id.unwrap_or_else(next_workflow_id);
            let artifacts = object
                .get("_artifacts")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let mut input = object.clone();
            input.remove("_artifacts");
            if let Err(error) =
                validate_material_safety_in_payload(&serde_json::Value::Object(input.clone()))
            {
                return http_json(
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                    ),
                );
            }

            let gcode_name = artifacts.get("gcode_file").and_then(|v| v.as_str()).unwrap_or("");
            let gcode_sha = artifacts.get("gcode_sha256").and_then(|v| v.as_str()).unwrap_or("");
            let material = input.get("material").and_then(|v| v.as_str()).unwrap_or("");
            let thickness = parse_float(input.get("thickness_mm"), "thickness_mm", 3.0).unwrap_or(3.0);
            let confirmation_snapshot = if !gcode_name.is_empty() {
                serde_json::json!({
                    "gcode_file": gcode_name,
                    "gcode_sha256": gcode_sha,
                    "material": material,
                    "thickness_mm": thickness,
                })
            } else {
                serde_json::Value::Null
            };

            let record = serde_json::json!({
                "workflow_id": workflow_id,
                "status": "preview_ready",
                "source_type": object.get("source_type").and_then(|value| value.as_str()).unwrap_or(
                    if route == "/api/text-task/generate" { "text" } else { "image" }
                ),
                "input": input,
                "artifacts": artifacts,
                "confirmation_snapshot": confirmation_snapshot,
                "next_actions": ["status", "cancel", "confirm_send"],
                "created_at": unix_seconds(),
                "updated_at": unix_seconds()
            });
            let stored = laser_runtime::StoredRecord {
                kind: laser_runtime::RecordKind::Workflow,
                id: workflow_id,
                status: Some("preview_ready".into()),
                payload: record.clone(),
            };
            if store.put(&stored).is_err() {
                return http_json(500, failure("保存 workflow 失败", None));
            }
            http_json(200, success(record))
        }
        "status" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                Ok(Some(mut stored)) => {
                    overlay_live_progress(&mut stored.payload, jobs, &workflow_id);
                    http_json(200, success(stored.payload))
                }
                Ok(None) => http_json(404, failure("未找到 workflow", None)),
                Err(_) => http_json(500, failure("读取 workflow 失败", None)),
            }
        }
        "cancel" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            signal_job_cancel(jobs, &workflow_id);
            match store.set_status(
                laser_runtime::RecordKind::Workflow,
                &workflow_id,
                "cancelled",
                unix_seconds() as f64,
            ) {
                Ok(true) => match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                    Ok(Some(stored)) => http_json(200, success(stored.payload)),
                    _ => http_json(500, failure("读取 workflow 失败", None)),
                },
                Ok(false) => http_json(404, failure("未找到 workflow", None)),
                Err(_) => http_json(500, failure("取消 workflow 失败", None)),
            }
        }
        "resume" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            let confirmed = object.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(true);
            if !confirmed {
                return http_json(400, failure("请确认断点恢复安全选项 (confirmed=true)", None));
            }
            let Some(mut stored) = (match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                Ok(value) => value,
                Err(_) => return http_json(500, failure("读取 workflow 失败", None)),
            }) else {
                return http_json(404, failure("未找到 workflow", None));
            };
            if let Err(error) = validate_material_safety_in_payload(&stored.payload) {
                return http_json(
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                    ),
                );
            }

            let status = stored
                .status
                .as_deref()
                .or_else(|| stored.payload.get("status").and_then(|v| v.as_str()));
            if status == Some("sending") {
                return http_json(409, failure("该 workflow 正在发送中，禁止重复操作", Some(serde_json::json!({"error_code": "device_busy"}))));
            }
            if status == Some("completed") {
                return http_json(400, failure("该 workflow 已全量完成，无法断点继续", Some(serde_json::json!({"error_code": "workflow_completed"}))));
            }

            let send_result = stored.payload.get("send_result").cloned().unwrap_or_default();
            let lines_sent = send_result.get("lines_sent").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let total_lines = send_result.get("total_lines").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if lines_sent == 0 || (total_lines > 0 && lines_sent >= total_lines) {
                return http_json(
                    400,
                    failure(
                        format!("该任务尚未开始或已完成（断点: {lines_sent}/{total_lines}），无法断点续雕"),
                        Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                    ),
                );
            }

            let (preflight, gcode_sha, gcode_file) = match build_send_preflight(&stored.payload, send_roots) {
                Ok(value) => value,
                Err(error) => {
                    return http_json(
                        400,
                        failure(
                            format!("发送安全门禁未通过: {error}"),
                            Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                        ),
                    );
                }
            };
            let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
            let mut resume_object = object.clone();
            resume_object.insert("start_from_line".into(), serde_json::json!(lines_sent));
            let send_result = match launch(&workflow_id, gcode_path, &gcode_sha, &resume_object) {
                Ok(value) => value,
                Err((status, body)) => return http_json(status, body),
            };
            stored.status = Some("sending".into());
            if let Some(map) = stored.payload.as_object_mut() {
                map.insert("status".into(), serde_json::Value::String("sending".into()));
                map.insert("updated_at".into(), serde_json::json!(unix_seconds()));
                map.insert("send_result".into(), send_result);
                freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
            }
            if store.put(&stored).is_err() {
                return http_json(500, failure("保存 workflow 失败", None));
            }
            http_json(200, success(stored.payload))
        }
        "confirm_send" | "send" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            let confirmed = object.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false);
            if !confirmed {
                return http_json(400, failure("请确认发送安全选项 (confirmed=true)", None));
            }
            match store.get(laser_runtime::RecordKind::Workflow, &workflow_id) {
                Ok(Some(mut stored)) => {
                    let has_gcode = stored.payload.get("artifacts")
                        .and_then(|a| a.get("gcode_file").or_else(|| a.get("gcode_sha256")))
                        .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                        || stored.payload.get("confirmation_snapshot").is_some_and(|v| !v.is_null())
                        || stored.payload.get("input").and_then(|i| i.get("gcode_file")).is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()));
                    if !has_gcode {
                        return http_json(
                            400,
                            failure(
                                "当前 workflow 没有可确认发送的 G-code 快照",
                                Some(serde_json::json!({"error_code": "confirmation_snapshot_missing"})),
                            ),
                        );
                    }
                    if let Err(error) = validate_material_safety_in_payload(&stored.payload) {
                        return http_json(
                            400,
                            failure(
                                error,
                                Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                            ),
                        );
                    }

                    let current_status = stored
                        .status
                        .as_deref()
                        .or_else(|| stored.payload.get("status").and_then(|v| v.as_str()));
                    if let Some(reason) = workflow_send_blocked_reason(current_status) {
                        return http_json(
                            400,
                            failure(reason, Some(serde_json::json!({"error_code": "workflow_not_sendable"}))),
                        );
                    }
                    let (preflight, gcode_sha, gcode_file) =
                        match build_send_preflight(&stored.payload, send_roots) {
                            Ok(value) => value,
                            Err(error) => {
                                return http_json(
                                    400,
                                    failure(
                                        format!("发送安全门禁未通过: {error}"),
                                        Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                                    ),
                                );
                            }
                        };
                    if let Err(error) = laser_device::authorize_send(&preflight) {
                        return http_json(
                            400,
                            failure(
                                format!("发送安全门禁未通过: {error}"),
                                Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                            ),
                        );
                    }

                    let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
                    let send_result = match launch(&workflow_id, gcode_path, &gcode_sha, object) {
                        Ok(value) => value,
                        Err((status, body)) => return http_json(status, body),
                    };
                    stored.status = Some("sending".into());
                    if let Some(map) = stored.payload.as_object_mut() {
                        map.insert("status".into(), serde_json::Value::String("sending".into()));
                        map.insert("updated_at".into(), serde_json::json!(unix_seconds()));
                        map.insert("send_result".into(), send_result);
                        freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
                    }
                    let _ = store.put(&stored);
                    http_json(200, success(stored.payload))
                }
                Ok(None) => http_json(404, failure("未找到 workflow", None)),
                Err(_) => http_json(500, failure("读取 workflow 失败", None)),
            }
        }
        _ => http_json(400, failure("未知 workflow action", None)),
    }
}

fn handle_workflow_request(
    workflows: &std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
    >,
    route: &str,
    object: &serde_json::Map<String, serde_json::Value>,
    send_roots: &[std::path::PathBuf],
    jobs: &ActiveJobs,
    launch: SendLauncher<'_>,
) -> Vec<u8> {
    let mut records = match workflows.lock() {
        Ok(records) => records,
        Err(_) => return http_json(500, failure("workflow 状态存储不可用", None)),
    };
    let action = if route == "/api/text-task/generate" {
        "preview"
    } else if route == "/api/text-task/send" {
        "confirm_send"
    } else {
        object
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("status")
    };
    let requested_id = object
        .get("workflow_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    if action == "recent" || (action == "status" && requested_id.is_none()) {
        let workflows = records.values().cloned().collect::<Vec<_>>();
        return http_json(
            200,
            success(serde_json::json!({
                "workflows": workflows,
                "speech": "已列出最近的激光 workflow；可以用 workflow_id 继续查询、反馈或重生成。"
            })),
        );
    }
    match action {
        "preview" | "plan" | "create" => {
            let workflow_id = requested_id.unwrap_or_else(next_workflow_id);
            let artifacts = object
                .get("_artifacts")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let mut input = object.clone();
            input.remove("_artifacts");
            if let Err(error) =
                validate_material_safety_in_payload(&serde_json::Value::Object(input.clone()))
            {
                return http_json(
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                    ),
                );
            }

            let gcode_name = artifacts.get("gcode_file").and_then(|v| v.as_str()).unwrap_or("");
            let gcode_sha = artifacts.get("gcode_sha256").and_then(|v| v.as_str()).unwrap_or("");
            let material = input.get("material").and_then(|v| v.as_str()).unwrap_or("");
            let thickness = parse_float(input.get("thickness_mm"), "thickness_mm", 3.0).unwrap_or(3.0);
            let confirmation_snapshot = if !gcode_name.is_empty() {
                serde_json::json!({
                    "gcode_file": gcode_name,
                    "gcode_sha256": gcode_sha,
                    "material": material,
                    "thickness_mm": thickness,
                })
            } else {
                serde_json::Value::Null
            };

            let record = serde_json::json!({
                "workflow_id": workflow_id,
                "status": "preview_ready",
                "source_type": object.get("source_type").and_then(|value| value.as_str()).unwrap_or(
                    if route == "/api/text-task/generate" { "text" } else { "image" }
                ),
                "input": input,
                "artifacts": artifacts,
                "confirmation_snapshot": confirmation_snapshot,
                "next_actions": ["status", "cancel", "confirm_send"],
                "created_at": unix_seconds(),
                "updated_at": unix_seconds()
            });
            records.insert(workflow_id.clone(), record.clone());
            http_json(200, success(record))
        }
        "status" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            match records.get_mut(&workflow_id) {
                Some(record) => {
                    let mut payload = record.clone();
                    overlay_live_progress(&mut payload, jobs, &workflow_id);
                    http_json(200, success(payload))
                }
                None => http_json(404, failure("未找到 workflow", None)),
            }
        }
        "cancel" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            let Some(record) = records.get_mut(&workflow_id) else {
                return http_json(404, failure("未找到 workflow", None));
            };
            signal_job_cancel(jobs, &workflow_id);
            record["status"] = serde_json::Value::String("cancelled".into());
            record["updated_at"] = serde_json::json!(unix_seconds());
            http_json(200, success(record.clone()))
        }
        "confirm_send" | "send" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            let confirmed = object.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false);
            if !confirmed {
                return http_json(400, failure("请确认发送安全选项 (confirmed=true)", None));
            }
            let Some(record) = records.get_mut(&workflow_id) else {
                return http_json(404, failure("未找到 workflow", None));
            };
            if let Err(error) = validate_material_safety_in_payload(record) {
                return http_json(
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                    ),
                );
            }
            let has_snapshot = record.get("confirmation_snapshot").is_some_and(|v| !v.is_null())
                || record.get("artifacts").and_then(|a| a.get("gcode_file")).and_then(|f| f.as_str()).map(|s| !s.is_empty()).unwrap_or(false)
                || record.get("input").and_then(|i| i.get("gcode_file")).and_then(|f| f.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
            if !has_snapshot {
                return http_json(
                    400,
                    failure(
                        "当前 workflow 没有可确认发送的 G-code 快照",
                        Some(serde_json::json!({"error_code": "confirmation_snapshot_missing"})),
                    ),
                );
            }

            let current_status = record.get("status").and_then(|v| v.as_str());
            if let Some(reason) = workflow_send_blocked_reason(current_status) {
                return http_json(
                    400,
                    failure(reason, Some(serde_json::json!({"error_code": "workflow_not_sendable"}))),
                );
            }
            let (preflight, gcode_sha, gcode_file) = match build_send_preflight(&*record, send_roots) {
                Ok(value) => value,
                Err(error) => {
                    return http_json(
                        400,
                        failure(
                            format!("发送安全门禁未通过: {error}"),
                            Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                        ),
                    );
                }
            };
            if let Err(error) = laser_device::authorize_send(&preflight) {
                return http_json(
                    400,
                    failure(
                        format!("发送安全门禁未通过: {error}"),
                        Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                    ),
                );
            }

            let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
            let send_result = match launch(&workflow_id, gcode_path, &gcode_sha, object) {
                Ok(value) => value,
                Err((status, body)) => return http_json(status, body),
            };
            record["status"] = serde_json::Value::String("sending".into());
            record["updated_at"] = serde_json::json!(unix_seconds());
            if let Some(map) = record.as_object_mut() {
                map.insert("send_result".into(), send_result);
                freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
            }
            http_json(200, success(record.clone()))
        }
        "resume" => {
            let Some(workflow_id) = requested_id else {
                return http_json(400, failure("workflow_id 不能为空", None));
            };
            let confirmed = object.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(true);
            if !confirmed {
                return http_json(400, failure("请确认断点恢复安全选项 (confirmed=true)", None));
            }
            let Some(record) = records.get_mut(&workflow_id) else {
                return http_json(404, failure("未找到 workflow", None));
            };
            if let Err(error) = validate_material_safety_in_payload(record) {
                return http_json(
                    400,
                    failure(
                        error,
                        Some(serde_json::json!({"error_code": "material_safety_blocked"})),
                    ),
                );
            }

            let status = record.get("status").and_then(|v| v.as_str());
            if status == Some("sending") {
                return http_json(409, failure("该 workflow 正在发送中，禁止重复操作", Some(serde_json::json!({"error_code": "device_busy"}))));
            }
            if status == Some("completed") {
                return http_json(400, failure("该 workflow 已全量完成，无法断点继续", Some(serde_json::json!({"error_code": "workflow_completed"}))));
            }

            let send_result = record.get("send_result").cloned().unwrap_or_default();
            let lines_sent = send_result.get("lines_sent").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let total_lines = send_result.get("total_lines").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            if lines_sent == 0 || (total_lines > 0 && lines_sent >= total_lines) {
                return http_json(
                    400,
                    failure(
                        format!("该任务尚未开始或已完成（断点: {lines_sent}/{total_lines}），无法断点续雕"),
                        Some(serde_json::json!({"error_code": "workflow_not_resumable"})),
                    ),
                );
            }

            let (preflight, gcode_sha, gcode_file) = match build_send_preflight(&*record, send_roots) {
                Ok(value) => value,
                Err(error) => {
                    return http_json(
                        400,
                        failure(
                            format!("发送安全门禁未通过: {error}"),
                            Some(serde_json::json!({"error_code": "send_gate_rejected"})),
                        ),
                    );
                }
            };

            let gcode_path = preflight.gcode_path.clone().unwrap_or_default();
            let mut resume_obj = object.clone();
            resume_obj.insert("start_from_line".into(), serde_json::json!(lines_sent));

            let send_result = match launch(&workflow_id, gcode_path, &gcode_sha, &resume_obj) {
                Ok(value) => value,
                Err((status, body)) => return http_json(status, body),
            };
            record["status"] = serde_json::Value::String("sending".into());
            record["updated_at"] = serde_json::json!(unix_seconds());
            if let Some(map) = record.as_object_mut() {
                map.insert("send_result".into(), send_result);
                freeze_confirmation_snapshot(map, &gcode_file, &gcode_sha);
            }
            http_json(200, success(record.clone()))
        }
        _ => http_json(400, failure("未知 workflow action", None)),
    }
}

fn load_material_library(
    settings: &laser_runtime::LaserSettings,
) -> Result<laser_contracts::MaterialLibrary, String> {
    let Some(path) = material_library_path(settings) else {
        return Ok(laser_contracts::builtin_material_library());
    };
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|error| format!("材料库解析失败: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(laser_contracts::builtin_material_library())
        }
        Err(error) => Err(format!("读取材料库失败: {error}")),
    }
}

fn validate_material_safety(material: &str) -> Result<(), String> {
    match laser_core::safety::evaluate_material(material) {
        laser_core::safety::MaterialSafety::Allow => Ok(()),
        laser_core::safety::MaterialSafety::Block { .. } => Err(
            "材料安全门禁拒绝：PVC/聚氯乙烯及明确危险材料禁止激光加工".into(),
        ),
        laser_core::safety::MaterialSafety::Clarify { .. } => Err(
            "材料成分不明确：请先确认材料不是塑料或其他危险材料".into(),
        ),
    }
}

fn material_from_payload(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("material")
        .and_then(serde_json::Value::as_str)
        .or_else(|| payload.get("input").and_then(|v| v.get("material")).and_then(serde_json::Value::as_str))
        .or_else(|| {
            payload
                .get("confirmation_snapshot")
                .and_then(|v| v.get("material"))
                .and_then(serde_json::Value::as_str)
        })
        .filter(|value| !value.trim().is_empty())
}

fn validate_material_safety_in_payload(payload: &serde_json::Value) -> Result<(), String> {
    if let Some(material) = material_from_payload(payload) {
        validate_material_safety(material)?;
    }
    Ok(())
}

fn material_library_export_value(library: &laser_contracts::MaterialLibrary) -> serde_json::Value {
    fn strip_default_power_scale(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("power_scale") == Some(&serde_json::json!(1000)) {
                    object.remove("power_scale");
                }
                for child in object.values_mut() {
                    strip_default_power_scale(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    strip_default_power_scale(item);
                }
            }
            _ => {}
        }
    }

    let mut value = serde_json::to_value(library).expect("material library must serialize");
    strip_default_power_scale(&mut value);
    value
}

fn material_export_response(settings: &laser_runtime::LaserSettings) -> Vec<u8> {
    match load_material_library(settings) {
        Ok(library) => http_json(
            200,
            success(serde_json::json!({
                "params_file": settings.material_params_file.as_deref().unwrap_or(""),
                "library": material_library_export_value(&library),
            })),
        ),
        Err(error) => http_json(
            500,
            failure(
                "读取材料库失败",
                Some(serde_json::json!({ "error": error })),
            ),
        ),
    }
}

fn strict_import_library(
    value: Option<&serde_json::Value>,
) -> Result<laser_contracts::MaterialLibrary, String> {
    let Some(value) = value else {
        return Err("导入材料库格式不支持：需要 version=1 的 JSON 对象".into());
    };
    let valid_version = value
        .get("version")
        .and_then(serde_json::Value::as_f64)
        .is_some_and(|version| version == 1.0);
    if !valid_version || !value.is_object() {
        return Err("导入材料库格式不支持：需要 version=1 的 JSON 对象".into());
    }
    serde_json::from_value(value.clone())
        .map_err(|_| "导入材料库格式不支持：需要 version=1 的 JSON 对象".into())
}

fn write_material_library(
    settings: &laser_runtime::LaserSettings,
    library: &laser_contracts::MaterialLibrary,
) -> Result<String, String> {
    let path = material_library_path(settings).ok_or("材料库文件路径未配置")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let backup = path.with_extension("json.bak");
    if path.exists() {
        std::fs::copy(&path, &backup).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(library).map_err(|error| error.to_string())?;
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    Ok(backup
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_owned())
}

fn number_i32(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: i32,
) -> Result<i32, String> {
    parse_int(object.get(key), key, default as i64)
        .map(|value| value as i32)
        .map_err(|error| error.to_string())
}

fn handle_material_manage(
    settings: &laser_runtime::LaserSettings,
    db: Option<&std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<u8> {
    let action = object
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let mut library = match load_material_library(settings) {
        Ok(library) => library,
        Err(error) => {
            return http_json(
                500,
                failure(
                    "读取材料库失败",
                    Some(serde_json::json!({ "error": error })),
                ),
            );
        }
    };
    match action {
        "export" => material_export_response(settings),
        "import" => {
            let imported = object
                .get("library_json")
                .or_else(|| object.get("params_json"));
            let value = match imported {
                Some(serde_json::Value::String(raw)) => match serde_json::from_str(raw) {
                    Ok(value) => Some(value),
                    Err(_) => {
                        return http_json(
                            200,
                            failure(
                                "library_json 不是合法 JSON: Expecting value: line 1 column 1 (char 0)",
                                None,
                            ),
                        );
                    }
                },
                Some(serde_json::Value::Object(map)) if map.is_empty() => {
                    return http_json(
                        200,
                        failure(
                            "library_json 不是合法 JSON: Expecting value: line 1 column 1 (char 0)",
                            None,
                        ),
                    );
                }
                Some(value) => Some(value.clone()),
                None => None,
            };
            let imported = match strict_import_library(value.as_ref()) {
                Ok(library) => library,
                Err(error) => return http_json(400, failure(error, None)),
            };
            library = imported;
            if let Some(db_arc) = db {
                if let Ok(val) = serde_json::to_value(&library) {
                    if let Ok(db_guard) = db_arc.lock() {
                        let _ = db_guard.import_material_json(
                            &val,
                            laser_runtime::MaterialConflictPolicy::Overwrite,
                            unix_seconds() as f64,
                        );
                    }
                }
            }
            let backup_path = match write_material_library(settings, &library) {
                Ok(path) => path,
                Err(error) => {
                    return http_json(
                        500,
                        failure(
                            "保存材料库失败",
                            Some(serde_json::json!({ "error": error })),
                        ),
                    );
                }
            };
            http_json(
                200,
                success(serde_json::json!({
                    "library": library,
                    "materials": library,
                    "backup_path": backup_path,
                })),
            )
        }
        "delete" => {
            if !parse_bool(object.get("confirmed"), false) {
                return http_json(400, failure("删除当前参数需要 confirmed=true", None));
            }
            let material = object
                .get("material")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let thickness = object.get("thickness_mm").and_then(|value| value.as_f64());
            let thickness_key = thickness.map(|value| value.to_string()).unwrap_or_default();
            let Some(entry) = library.materials.get_mut(material) else {
                return http_json(400, failure("材料不存在", None));
            };
            if thickness.is_none() || thickness == Some(0.0) {
                library.materials.remove(material);
            } else {
                entry.thicknesses.remove(&thickness_key);
                if entry.thicknesses.is_empty() {
                    library.materials.remove(material);
                }
            }
            if let Some(db_arc) = db {
                if let Ok(db_guard) = db_arc.lock() {
                    if thickness.is_none() || thickness == Some(0.0) {
                        let _ = db_guard.connection.execute(
                            "DELETE FROM material_params WHERE material_name=?1",
                            rusqlite::params![material],
                        );
                        let _ = db_guard.connection.execute(
                            "DELETE FROM material_aliases WHERE material_name=?1",
                            rusqlite::params![material],
                        );
                        let _ = db_guard.connection.execute(
                            "DELETE FROM materials WHERE name=?1",
                            rusqlite::params![material],
                        );
                    } else {
                        let _ = db_guard.connection.execute(
                            "DELETE FROM material_params WHERE material_name=?1 AND thickness_key=?2",
                            rusqlite::params![material, thickness_key],
                        );
                    }
                }
            }
            match write_material_library(settings, &library) {
                Ok(_) => http_json(200, success(serde_json::json!({ "materials": library }))),
                Err(error) => http_json(
                    500,
                    failure(
                        "保存材料库失败",
                        Some(serde_json::json!({ "error": error })),
                    ),
                ),
            }
        }
        "save" => {
            let material = object
                .get("material")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .trim();
            if material.is_empty() {
                return http_json(400, failure("material 不能为空", None));
            }
            let entry = library.materials.entry(material.to_owned()).or_default();
            if let Some(value) = object.get("aliases") {
                entry.aliases = if let Some(values) = value.as_array() {
                    values
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                } else {
                    value
                        .as_str()
                        .unwrap_or("")
                        .split([',', '，'])
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect()
                };
            }
            if let Some(thicknesses_val) = object.get("thicknesses") {
                if let Ok(parsed_thicknesses) = serde_json::from_value::<
                    std::collections::BTreeMap<String, laser_contracts::ThicknessEntry>,
                >(thicknesses_val.clone())
                {
                    for (k, v) in parsed_thicknesses {
                        entry.thicknesses.insert(k, v);
                    }
                    if let Some(db_arc) = db {
                        if let Ok(val) = serde_json::to_value(&library) {
                            if let Ok(db_guard) = db_arc.lock() {
                                let _ = db_guard.import_material_json(
                                    &val,
                                    laser_runtime::MaterialConflictPolicy::Overwrite,
                                    unix_seconds() as f64,
                                );
                            }
                        }
                    }
                    return match write_material_library(settings, &library) {
                        Ok(_) => {
                            http_json(200, success(serde_json::json!({ "materials": library })))
                        }
                        Err(error) => http_json(
                            500,
                            failure(
                                "保存材料库失败",
                                Some(serde_json::json!({ "error": error })),
                            ),
                        ),
                    };
                }
            }
            let thickness = object.get("thickness_mm").and_then(|value| value.as_f64());
            let Some(thickness) = thickness.filter(|value| value.is_finite() && *value > 0.0)
            else {
                return http_json(400, failure("thickness_mm 必须是大于 0 的数字", None));
            };
            let mode = object
                .get("laser_mode")
                .and_then(|value| value.as_str())
                .unwrap_or("engrave");
            let engraving = object
                .get("engraving_mode")
                .and_then(|value| value.as_str())
                .unwrap_or("raster");
            let thickness_key = thickness.to_string();
            let cell = laser_contracts::ParamCell {
                laser_min_power: match number_i32(object, "laser_min_power", 0) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                laser_max_power: match number_i32(object, "laser_max_power", 800) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                feed_rate: match number_i32(object, "feed_rate", 1200) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                travel_rate: match number_i32(object, "travel_rate", 3000) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                threshold: match number_i32(object, "threshold", -1) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                passes: match number_i32(object, "passes", 1) {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                pixel_size_mm: match parse_float(object.get("pixel_size_mm"), "pixel_size_mm", 0.1)
                {
                    Ok(value) => value,
                    Err(error) => return http_json(400, failure(error, None)),
                },
                power_scale: object
                    .get("power_scale")
                    .and_then(|value| value.as_i64())
                    .map(|value| value as i32)
                    .or(Some(1000)),
                source: object
                    .get("source")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                notes: object
                    .get("notes")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                updated_at: Some(unix_seconds() as f64),
                extra: std::collections::BTreeMap::new(),
            };
            if cell.validate(1000, 100).is_err() {
                return http_json(400, failure("材料参数超出允许范围", None));
            }
            let thickness_entry = entry.thicknesses.entry(thickness_key).or_default();
            if mode == "cut" {
                thickness_entry.cut = Some(cell);
            } else if mode == "engrave" && engraving == "outline" {
                thickness_entry
                    .engrave
                    .get_or_insert_with(Default::default)
                    .outline = Some(cell);
            } else if mode == "engrave" && engraving == "raster" {
                thickness_entry
                    .engrave
                    .get_or_insert_with(Default::default)
                    .raster = Some(cell);
            } else {
                return http_json(400, failure("laser_mode 或 engraving_mode 无效", None));
            }
            if let Some(db_arc) = db {
                if let Ok(val) = serde_json::to_value(&library) {
                    if let Ok(db_guard) = db_arc.lock() {
                        let _ = db_guard.import_material_json(
                            &val,
                            laser_runtime::MaterialConflictPolicy::Overwrite,
                            unix_seconds() as f64,
                        );
                    }
                }
            }
            match write_material_library(settings, &library) {
                Ok(_) => http_json(200, success(serde_json::json!({ "materials": library }))),
                Err(error) => http_json(
                    500,
                    failure(
                        "保存材料库失败",
                        Some(serde_json::json!({ "error": error })),
                    ),
                ),
            }
        }
        _ => http_json(
            400,
            failure("action 必须是 save、delete、import 或 export", None),
        ),
    }
}

pub fn success(result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"success": true, "result": result})
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 浏览器来源白名单（技术书 §6.5）：Tauri 2 内嵌页。
const TAURI_ORIGINS: [&str; 2] = ["http://tauri.localhost", "tauri://localhost"];
/// 前端开发 / 预览端口（`frontend/package.json` 的 5177、5178，Vite 默认 5173）；仅回环请求时允许。
const DEV_LOOPBACK_ORIGINS: [&str; 6] = [
    "http://127.0.0.1:5173",
    "http://localhost:5173",
    "http://127.0.0.1:5177",
    "http://localhost:5177",
    "http://127.0.0.1:5178",
    "http://localhost:5178",
];
const CORS_ALLOW_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
const CORS_ALLOW_HEADERS: &str = "Content-Type, Authorization, Accept, X-Moss-Desktop-Session";

/// 判断浏览器 `Origin` 是否允许跨源访问。精确匹配 scheme + host + port，不做前缀或大小写归一。
///
/// 允许：Tauri 内嵌页；与请求 `Host` 同源（MOSS 自己提供的页面，含局域网访问）；
/// 回环请求下的前端开发端口；`extra`（`MOSS_ALLOWED_ORIGINS`）。
pub fn is_allowed_origin(
    origin: &str,
    host_header: Option<&str>,
    loopback: bool,
    extra: &[String],
) -> bool {
    if origin.is_empty() || origin != origin.trim() || origin == "null" {
        return false;
    }
    if TAURI_ORIGINS.contains(&origin) {
        return true;
    }
    if loopback && DEV_LOOPBACK_ORIGINS.contains(&origin) {
        return true;
    }
    if extra.iter().any(|item| item == origin) {
        return true;
    }
    if let Some(host) = host_header.map(str::trim).filter(|host| !host.is_empty()) {
        if origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .is_some_and(|rest| rest == host)
        {
            return true;
        }
    }
    false
}

/// 回环连接的 `Host` 白名单（防 DNS 重绑定，技术书 §6.5）。
///
/// 允许：IPv4 / 方括号 IPv6 字面量、`localhost`（不区分大小写），以及 `extra_origins`
/// （`MOSS_ALLOWED_ORIGINS`）里出现的主机名。端口可有可无，但必须是数字。
/// 只对回环连接生效：非回环连接本来就要 Bearer 鉴权，局域网主机名访问不受影响。
pub fn is_allowed_loopback_host(host: &str, extra_origins: &[String]) -> bool {
    let Some(name) = host_name_without_port(host.trim()) else {
        return false;
    };
    if name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    if let Some(inner) = name.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    extra_origins.iter().any(|origin| {
        origin
            .split_once("://")
            .and_then(|(_, rest)| host_name_without_port(rest))
            .is_some_and(|extra| extra.eq_ignore_ascii_case(name))
    })
}

/// 去掉 `Host` 中的端口；格式不合法（空、端口非数字、未加方括号的 IPv6）时返回 `None`。
fn host_name_without_port(host: &str) -> Option<&str> {
    if host.is_empty() {
        return None;
    }
    let (name, port) = if host.starts_with('[') {
        let end = host.find(']')?;
        let rest = &host[end + 1..];
        let port = match rest {
            "" => None,
            _ => Some(rest.strip_prefix(':')?),
        };
        (&host[..=end], port)
    } else {
        match host.split_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host, None),
        }
    };
    if name.is_empty() || name == "[]" {
        return None;
    }
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
    }
    Some(name)
}

/// 局域网（手机）连接不允许的“参考数据”写操作（技术书 §9.8）：材料库增删改与导入、校准单元格存为推荐参数、系统设置、
/// Jev 密钥测试与查看。雕刻机操作（连接、预览、发送、走边框、急停、安全动作）与只读查询不受影响。
pub fn is_reference_data_write(method: &str, route: &str, body: &[u8]) -> bool {
    if method != "POST" {
        return false;
    }
    match route {
        // select-cell 会把校准单元格保存为材料推荐参数
        "/api/settings" | "/api/jev/test" | "/api/jev/reveal" | "/api/material-lab/select-cell" => true,
        "/api/material-lab/manage" => {
            let action = serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .and_then(|value| value.get("action").and_then(|a| a.as_str()).map(str::to_owned));
            // 只有导出是只读；解析失败或缺 action 一律按写入拒绝
            action.as_deref() != Some("export")
        }
        _ => false,
    }
}

/// 本机用于局域网通信的 IPv4 地址：对外“连接”一个 UDP 套接字（不发送任何数据包）后读本地地址。
/// 没有网络或只有回环时返回空。
fn primary_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// 构造调用方上下文：进程内调用方显式给出的优先；否则回环 → `Loopback`，局域网配对 → `Lan`。
fn caller_context_for(
    ctx_override: Option<laser_contracts::CallerContext>,
    loopback: bool,
    pairing_id: Option<String>,
) -> laser_contracts::CallerContext {
    if let Some(ctx) = ctx_override {
        return ctx;
    }
    match (loopback, pairing_id) {
        (false, Some(pairing_id)) => laser_contracts::CallerContext::Lan { pairing_id },
        // 非回环且未鉴权只可能访问公开 GET 路由，不触发任何需要身份的动作
        (false, None) => laser_contracts::CallerContext::Lan { pairing_id: String::new() },
        (true, _) => laser_contracts::CallerContext::Loopback,
    }
}

/// 请求体里的身份类字段一律删除（技术书 §6.4 规则 1，验收 A-36）：身份只由入口构造，不能自报。
const IDENTITY_FIELDS: [&str; 6] = [
    "caller_context",
    "caller",
    "_caller",
    "desktop_session",
    "approval_id",
    "gateway_session",
];

/// 审批卡上的发送摘要（技术书 §9.5.2）：内容、材料、参数与发送方式；不含文件路径与设备地址。
fn send_approval_summary(
    label: &str,
    record_id: &str,
    facts: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut parts = vec![format!("{label} {record_id}")];
    if let Some(material) = facts.get("material").and_then(|v| v.as_str()).filter(|v| !v.trim().is_empty()) {
        match facts.get("thickness_mm").and_then(|v| v.as_f64()) {
            Some(thickness) => parts.push(format!("{material} {thickness} mm")),
            None => parts.push(material.to_owned()),
        }
    }
    if let Some(params) = facts.get("params").and_then(|v| v.as_object()) {
        let mut items = Vec::new();
        for (key, label) in [("power", "功率"), ("laser_max_power", "最大功率"), ("feed_rate", "速度"), ("passes", "遍数")] {
            if let Some(value) = params.get(key).filter(|v| !v.is_null()) {
                items.push(format!("{label} {value}"));
            }
        }
        if !items.is_empty() {
            parts.push(items.join("，"));
        }
    }
    if let Some(mode) = facts.get("send_mode").and_then(|v| v.as_str()) {
        parts.push(if mode == "sd_offline" { "脱机 SD 卡加工".into() } else { "逐行发送".into() });
    }
    if let Some(line) = facts.get("start_from_line").and_then(|v| v.as_u64()) {
        parts.push(format!("从第 {line} 行继续"));
    }
    parts.join("；")
}

fn safe_action_approval_summary(action: &str, facts: &serde_json::Map<String, serde_json::Value>) -> String {
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

fn framing_approval_summary(facts: &serde_json::Map<String, serde_json::Value>) -> String {
    let value = |key: &str, default: &str| facts.get(key).map(|v| v.to_string()).unwrap_or_else(|| default.to_owned());
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
fn apply_connection_to_safe_action(
    args: &mut laser_contracts::LaserSafeActionArgs,
    connection: &laser_contracts::ApprovalConnection,
) {
    args.connection_mode = Some(connection.mode().to_owned());
    match connection {
        laser_contracts::ApprovalConnection::Home { .. } => { args.host = None; args.port = None; args.telnet_port = None; },
        laser_contracts::ApprovalConnection::Agent { .. } => { args.host = None; args.port = None; args.telnet_port = None; },
        laser_contracts::ApprovalConnection::External { .. } => { args.host = None; args.port = None; }
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

fn strip_identity_fields(object: &mut serde_json::Map<String, serde_json::Value>) {
    for key in IDENTITY_FIELDS {
        object.remove(key);
    }
}

fn request_head(request: &[u8]) -> std::borrow::Cow<'_, str> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(request.len());
    String::from_utf8_lossy(&request[..end])
}

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

fn cors_header_block(origin: &str) -> String {
    format!(
        "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: {CORS_ALLOW_METHODS}\r\nAccess-Control-Allow-Headers: {CORS_ALLOW_HEADERS}\r\n"
    )
}

/// 在状态行之后插入允许来源的 CORS 头。响应构造函数本身不再输出任何 CORS 头。
fn with_cors_headers(response: Vec<u8>, origin: &str) -> Vec<u8> {
    let Some(line_end) = response.windows(2).position(|window| window == b"\r\n") else {
        return response;
    };
    let mut out = Vec::with_capacity(response.len() + 256);
    out.extend_from_slice(&response[..line_end + 2]);
    out.extend_from_slice(cors_header_block(origin).as_bytes());
    out.extend_from_slice(&response[line_end + 2..]);
    out
}

/// `OPTIONS` 响应：允许来源时带 CORS 头与缓存时长；无 `Origin` 时只返回空 200。
fn http_options(origin: Option<&str>) -> Vec<u8> {
    let cors = origin
        .map(|origin| format!("{}Access-Control-Max-Age: 86400\r\n", cors_header_block(origin)))
        .unwrap_or_default();
    format!("HTTP/1.1 200 OK\r\n{cors}Content-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
}

fn http_json(status: u16, body: serde_json::Value) -> Vec<u8> {
    http_json_with_headers(status, body, &[])
}

fn normalize_mcp_endpoint(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.contains('<')
        || !(value.starts_with("ws://") || value.starts_with("wss://"))
    {
        return None;
    }
    Some(value.to_owned())
}

/// `/api/system/info` 的 `voice` 字段：`{mode, device_sessions}`。设备会话数只在自建与内置链路下有意义
/// （自建来自接入点健康检查，内置来自内置语音服务）；未知时为 null。
fn voice_status_json(runtime_path: Option<&std::path::Path>) -> serde_json::Value {
    let settings = read_runtime_settings(runtime_path);
    let mode = match settings.get("voice").and_then(|voice| voice.get("mode")).and_then(serde_json::Value::as_str) {
        Some("self_hosted") => "self_hosted",
        Some("builtin") => "builtin",
        _ => "official",
    };
    let telemetry = laser_contracts::global_voice_telemetry();
    let sessions = if mode != "official" {
        telemetry
            .device_sessions()
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    let (phase, level) = if mode == "builtin" {
        telemetry.pulse()
    } else {
        (None, 0)
    };
    serde_json::json!({
        "mode": mode,
        "device_sessions": sessions,
        "phase": phase,
        "level": level,
    })
}

fn read_runtime_settings(runtime_path: Option<&std::path::Path>) -> serde_json::Map<String, serde_json::Value> {
    runtime_path
        .map(|path| path.join("settings.json"))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// 读-改-写运行目录的 settings.json（先写临时文件再改名，保留其他键）。
/// 没有运行目录或写入失败时返回错误，调用方不能报告“已保存”。
fn update_runtime_settings(
    runtime_path: Option<&std::path::Path>,
    change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<(), String> {
    let runtime_path = runtime_path.ok_or("没有配置运行目录")?;
    let mut current = read_runtime_settings(Some(runtime_path));
    change(&mut current);
    let data = serde_json::to_vec_pretty(&current).map_err(|error| error.to_string())?;
    let temp = runtime_path.join("settings.json.tmp");
    std::fs::write(&temp, data)
        .and_then(|()| std::fs::rename(&temp, runtime_path.join("settings.json")))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("写入 settings.json 失败：{error}")
        })
}

fn mask_secret(value: &str) -> String {
    let value = value.trim();
    let char_count = value.chars().count();
    if char_count <= 8 {
        return "••••••••".to_owned();
    }
    let prefix: String = value.chars().take(5).collect();
    let suffix: String = value.chars().skip(char_count - 4).collect();
    format!("{prefix}••••••••{suffix}")
}

/// `/api/system/info` 的 `mcp` 段：只反映真实配置与真实遥测（技术书 §9.5.3，验收 A-72）。
///
/// - `configured`：是否配置了接入点地址；
/// - `connected`：桥当前是否与接入点保持连接；
/// - `client_info` / `last_request` / `recent_events` / `messages_count`：没有真实数据时为空 / 0；
/// - `tools_count`：当前开关下实际暴露的工具数（安全模式 19，开发者模式 20）。
fn mcp_status_json(
    endpoint: Option<&str>,
    telemetry: &laser_contracts::McpTelemetrySnapshot,
    tools_count: usize,
) -> serde_json::Value {
    let endpoint_display = endpoint
        .map(|ep| match ep.split_once("token=") {
            Some((head, _)) => format!("{head}token=***"),
            None => ep.to_owned(),
        })
        .unwrap_or_default();
    let client_info = match (telemetry.client_name.is_empty(), telemetry.client_version.is_empty()) {
        (true, _) => String::new(),
        (false, true) => telemetry.client_name.clone(),
        (false, false) => format!("{} v{}", telemetry.client_name, telemetry.client_version),
    };
    serde_json::json!({
        "configured": endpoint.is_some(),
        "connected": telemetry.connected,
        "endpoint": endpoint_display,
        "client_info": client_info,
        "last_request": telemetry.last_request,
        "last_request_time": telemetry.last_request_time,
        "messages_count": telemetry.messages_count,
        "tools_count": tools_count,
        "protocol_version": "2024-11-05",
        "recent_events": telemetry.recent_events,
        // 各接入点的连接状态（NV8 多接入点；地址已脱敏）
        "endpoints": telemetry.endpoints.iter().map(|link| serde_json::json!({
            "id": link.id,
            "endpoint": link.endpoint,
            "connected": link.connected,
            "connections": link.connections,
            "last_error": link.last_error,
        })).collect::<Vec<_>>(),
        // 以下三项是固定的设计说明，不是检测结果
        "core_tool": "laser_workflow_tool",
        "safety_gate": "发送前重新计算 G-code 文件 SHA-256，与确认时的快照比对",
        "estop_channel": "0x18 软复位走实时通道、不排队；写出成功不代表已停机"
    })
}

fn configured_mcp_endpoint(runtime_path: Option<&std::path::Path>) -> Option<String> {
    let saved = runtime_path
        .map(|path| path.join("settings.json"))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get("mcp_endpoint").and_then(|item| item.as_str()).map(str::to_owned));
    saved
        .as_deref()
        .and_then(normalize_mcp_endpoint)
        .or_else(|| std::env::var("MCP_ENDPOINT").ok().and_then(|value| normalize_mcp_endpoint(&value)))
}

fn http_json_with_headers(
    status: u16,
    body: serde_json::Value,
    extra_headers: &[(&str, String)],
) -> Vec<u8> {
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"success\":false}".to_vec());
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "HTTP",
    };
    let mut headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nCache-Control: no-store\r\n"
    );
    for (key, value) in extra_headers {
        headers.push_str(&format!("{key}: {value}\r\n"));
    }
    headers.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    ));
    headers.into_bytes().into_iter().chain(bytes).collect()
}

fn http_bytes_with_disposition(
    status: u16,
    content_type: &str,
    disposition: Option<&str>,
    bytes: Vec<u8>,
) -> Vec<u8> {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let disp_header = match disposition {
        Some(d) => format!("Content-Disposition: {d}\r\n"),
        None => String::new(),
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n{disp_header}Cache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    ).into_bytes().into_iter().chain(bytes).collect()
}

fn http_bytes(status: u16, content_type: &str, bytes: Vec<u8>) -> Vec<u8> {
    http_bytes_with_disposition(status, content_type, None, bytes)
}

fn http_sse(body: String) -> Vec<u8> {
    let bytes = body.into_bytes();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nContent-Length: {}\r\n\r\n",
        bytes.len()
    ).into_bytes().into_iter().chain(bytes).collect()
}

fn safe_asset_path(path: &str) -> Option<String> {
    let decoded = percent_decode(path);
    let candidate = std::path::Path::new(&decoded);
    if decoded.is_empty()
        || candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return None;
    }
    Some(decoded.replace('\\', "/"))
}

fn embedded_content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "woff" | "woff2" => "font/woff2",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn content_type(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "json" => "application/json",
        "gcode" | "nc" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn query_param(query: &str, wanted: &str) -> Option<String> {
    query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key != wanted {
            return None;
        }
        Some(percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                output.push(byte);
                index += 3;
                continue;
            }
        }
        output.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

pub fn failure(message: impl Into<String>, detail: Option<serde_json::Value>) -> serde_json::Value {
    let mut response = serde_json::json!({"success": false, "result": message.into()});
    if let Some(detail) = detail {
        response["detail"] = detail;
    }
    response
}

pub fn parse_json_object(
    body: &[u8],
    limit: usize,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if body.len() > limit {
        return Err("请求体过大".into());
    }
    if body.is_empty() {
        return Ok(serde_json::Map::new());
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| format!("JSON 解析失败: {error}"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "JSON 顶层必须是对象".into())
}

pub fn parse_bool(value: Option<&serde_json::Value>, default: bool) -> bool {
    match value {
        None | Some(serde_json::Value::Null) => default,
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) if value.trim().is_empty() => default,
        Some(value) => matches!(
            value
                .as_str()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "y" | "on" | "确认" | "confirmed"
        ),
    }
}

pub fn parse_float(
    value: Option<&serde_json::Value>,
    key: &str,
    default: f64,
) -> Result<f64, String> {
    let Some(value) = value else {
        return Ok(default);
    };
    if value.is_null() || value.as_str().is_some_and(|v| v.trim().is_empty()) {
        return Ok(default);
    }
    let parsed = value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok());
    parsed
        .filter(|v: &f64| v.is_finite())
        .ok_or_else(|| format!("{key} 必须是数字"))
}

pub fn parse_int(
    value: Option<&serde_json::Value>,
    key: &str,
    default: i64,
) -> Result<i64, String> {
    let number = parse_float(value, key, default as f64)?;
    if number < i64::MIN as f64 || number > i64::MAX as f64 {
        return Err(format!("{key} 必须是整数"));
    }
    Ok(number.trunc() as i64)
}

pub fn parse_optional_positive(
    value: Option<&serde_json::Value>,
    key: &str,
) -> Result<Option<f64>, String> {
    let number = parse_float(value, key, 0.0)?;
    if number < 0.0 {
        return Err(format!("{key} 不能小于 0"));
    }
    if number == 0.0 {
        Ok(None)
    } else {
        Ok(Some(number))
    }
}

pub fn exposed_path(
    path: &std::path::Path,
    runtime_root: &std::path::Path,
    loopback: bool,
) -> String {
    if loopback {
        path.strip_prefix(runtime_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    } else {
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string()
    }
}

pub fn embedded_fixture_is_valid() -> bool {
    FrontendFixture::get("index.html")
        .map(|file| {
            file.data
                .windows(b"<div id=\"root\"></div>".len())
                .any(|window| window == b"<div id=\"root\"></div>")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_fixture_is_embedded() {
        assert!(embedded_fixture_is_valid());
    }

    #[test]
    fn api_field_conversion_matches_python_edges() {
        let body = br#"{"truth":"false","whole":"3.7","positive":0}"#;
        let object = parse_json_object(body, 64).unwrap();
        assert!(!parse_bool(object.get("truth"), false));
        assert_eq!(parse_int(object.get("whole"), "whole", 0).unwrap(), 3);
        assert_eq!(
            parse_optional_positive(object.get("positive"), "positive").unwrap(),
            None
        );
    }

    #[test]
    fn api_body_and_path_boundaries_are_enforced() {
        assert_eq!(
            parse_json_object(br#"[]"#, 64).unwrap_err(),
            "JSON 顶层必须是对象"
        );
        assert_eq!(
            parse_json_object(&[b'a'; 65], 64).unwrap_err(),
            "请求体过大"
        );
        let root = std::path::PathBuf::from(r"C:\runtime");
        let path = root.join("uploads").join("x.png");
        assert_eq!(exposed_path(&path, &root, true), "uploads/x.png");
        assert_eq!(exposed_path(&path, &root, false), "x.png");
    }

    #[test]
    fn jev_key_is_masked_without_leaking_the_middle() {
        assert_eq!(mask_secret("sk-1234567890"), "sk-12••••••••7890");
        assert_eq!(mask_secret("short"), "••••••••");
        assert_eq!(mask_secret("测试密钥一二三四五六七八九"), "测试密钥一••••••••六七八九");
        assert_eq!(mask_secret("中文短密钥"), "••••••••");
    }

    #[test]
    fn api_service_dispatches_health_config_materials_and_404() {
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        );
        assert_eq!(service.handle("GET", "/api/health", b"").status, 200);
        assert_eq!(
            service.handle("GET", "/api/ui/material-options", b"").body["success"],
            true
        );
        assert_eq!(service.handle("GET", "/missing", b"").status, 404);
        // 局域网扫描只接受 POST：不带 Origin 的跨站 GET 不能触发 mDNS
        assert_eq!(service.handle("GET", "/api/ui/discover-controllers", b"").status, 404);
        assert_eq!(
            service.handle("GET", "/api/health", b"").headers[0].0,
            "Cache-Control"
        );
        let config = service.handle("GET", "/api/ui/runtime-config", b"");
        assert_eq!(
            config.body["result"]["connection"]["serial"]["baudrate"],
            115200
        );
        assert!(
            service.handle("GET", "/api/ui/serial-ports", b"").body["result"]["ports"].is_array()
        );
    }

    #[test]
    fn gcode_source_preview_is_rendered_upright_without_false_closing_lines() {
        let temp = tempfile::tempdir().unwrap();
        let uploads = temp.path().join("uploads");
        std::fs::create_dir_all(&uploads).unwrap();
        // 一个"Γ"形开放折线：竖线在左，横杠在机器坐标 Y=10（顶部）。
        std::fs::write(uploads.join("gamma.gcode"), "G21\nG90\nM4 S0\nG0 X0 Y0\nG1 X0 Y10 S300 F1000\nG1 X20 Y10 S300 F1000\nM5\n").unwrap();
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_upload_root(uploads.clone())
            .with_generated_root(temp.path().join("gen"))
            .with_preview_roots(vec![uploads.clone(), temp.path().join("gen")]);
        let body = serde_json::json!({
            "action": "preview", "source_type": "gcode", "gcode_file": "gamma.gcode",
            "material": "椴木", "thickness_mm": 3
        }).to_string();
        let request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        let value = parse_json_response(&service.handle_http(request.as_bytes()));
        assert_eq!(value["success"], true, "{value}");
        let image_file = value["result"]["artifacts"]["image_file"].as_str().unwrap();
        let img = image::open(temp.path().join("gen").join(image_file)).unwrap().to_rgba8();
        let (w, h) = (img.width(), img.height());
        let row_hits = |y: u32| (0..w).filter(|&x| img.get_pixel(x, y).0[3] > 0).count();
        let top: usize = (0..h / 2).map(row_hits).sum();
        let bottom: usize = (h / 2..h).map(row_hits).sum();
        // 横杠在 Y=10（机器坐标顶部）→ 翻转后应出现在图像上半部，上半部像素明显多于下半部
        assert!(top > bottom * 2, "横杠必须渲染在图像上半部（top={top}, bottom={bottom}）");
        // 开放折线不得被补画闭合斜线：右下角区域（对角线经过处）应基本无像素
        let diag_hits: usize = (h * 6 / 10..h * 8 / 10)
            .flat_map(|y| (w * 5 / 10..w * 7 / 10).map(move |x| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y).0[3] > 0)
            .count();
        assert_eq!(diag_hits, 0, "开放折线不应出现伪闭合斜线");
    }

    #[test]
    fn draw_preview_generates_real_gcode_and_confirm_send_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(laser_runtime::MemoryStore::new());
        let gen_dir = temp.path().join("generated");
        std::fs::create_dir_all(&gen_dir).unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_store(store.clone())
        .with_generated_root(gen_dir.clone())
        .with_preview_roots(vec![gen_dir.clone()])
        .with_transport_factory(fake_transport_factory(200_000));

        let dummy_png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let preview_body = serde_json::json!({
            "image_data_url": dummy_png,
            "material": "椴木",
            "thickness_mm": 3.0,
            "mode": "raster",
            "scene_json": {"type": "excalidraw", "version": 2}
        }).to_string();

        let request = format!(
            "POST /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            preview_body.len(),
            preview_body
        );
        let response = service.handle_http(request.as_bytes());
        let body_start = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let json: serde_json::Value = serde_json::from_slice(&response[body_start..]).unwrap();
        assert_eq!(json["success"], true);
        let wf_id = json["result"]["workflow_id"].as_str().unwrap();
        let gcode_file = json["result"]["artifacts"]["gcode_file"].as_str().unwrap();
        let gcode_url = json["result"]["web"]["gcode_download_url"].as_str().unwrap();
        assert!(gen_dir.join(gcode_file).exists(), "物理 G-code 必须落盘");
        assert!(gcode_url.contains("download=1"));

        // 测试下载 Content-Disposition
        let download_req = format!("GET {gcode_url} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let dl_res = service.handle_http(download_req.as_bytes());
        let dl_str = String::from_utf8_lossy(&dl_res);
        assert!(dl_str.starts_with("HTTP/1.1 200 OK"));
        assert!(dl_str.contains("Content-Disposition: attachment; filename="));

        // 测试 confirm_send 闭环
        let send_body = serde_json::json!({
            "action": "confirm_send",
            "workflow_id": wf_id,
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": 23
        }).to_string();
        let send_req = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            send_body.len(),
            send_body
        );
        let send_res = service.handle_http(send_req.as_bytes());
        let send_start = send_res.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let send_json: serde_json::Value = serde_json::from_slice(&send_res[send_start..]).unwrap();
        assert_eq!(send_json["success"], true);
        assert_eq!(send_json["result"]["status"], "sending");
    }

    /// 200×100 像素、中间一块黑色矩形的 PNG data URL。
    fn draw_test_png() -> String {
        use base64::Engine;
        let mut img = image::GrayImage::from_pixel(200, 100, image::Luma([255]));
        for y in 25..75 {
            for x in 50..150 {
                img.put_pixel(x, y, image::Luma([0]));
            }
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    fn gcode_max_xy(gcode: &str) -> (f64, f64) {
        let mut max = (0.0_f64, 0.0_f64);
        for line in gcode.lines().filter(|line| line.starts_with("G0") || line.starts_with("G1")) {
            for word in line.split_whitespace() {
                if let Some(value) = word.strip_prefix('X').and_then(|v| v.parse::<f64>().ok()) { max.0 = max.0.max(value); }
                if let Some(value) = word.strip_prefix('Y').and_then(|v| v.parse::<f64>().ok()) { max.1 = max.1.max(value); }
            }
        }
        max
    }

    /// 画板按填写的宽 / 高出成品（锁定比例）；不填按 0.1 mm/像素；超出工作区拒绝；轮廓模式真的描边（2026-10-14）。
    #[test]
    fn draw_preview_honors_target_size_and_outline_mode() {
        let temp = tempfile::tempdir().unwrap();
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_generated_root(temp.path().to_path_buf());
        let png = draw_test_png();
        let preview = |extra: serde_json::Value| {
            let mut body = serde_json::json!({"image_data_url": png, "material": "椴木", "thickness_mm": 3.0, "mode": "raster"});
            for (key, value) in extra.as_object().unwrap() { body[key] = value.clone(); }
            post_json(&service, "/api/draw/preview", &body)
        };
        let gcode_of = |response: &serde_json::Value| {
            std::fs::read_to_string(temp.path().join(response["result"]["artifacts"]["gcode_file"].as_str().unwrap())).unwrap()
        };

        let natural = preview(serde_json::json!({}));
        assert_eq!(natural["success"], true, "{natural}");
        assert_eq!(natural["result"]["artifacts"]["width_mm"], 20.0);
        assert_eq!(natural["result"]["artifacts"]["height_mm"], 10.0);

        let sized = preview(serde_json::json!({"width_mm": 50, "height_mm": ""}));
        assert_eq!(sized["result"]["artifacts"]["width_mm"], 50.0, "{sized}");
        assert_eq!(sized["result"]["artifacts"]["height_mm"], 25.0);
        let (max_x, max_y) = gcode_max_xy(&gcode_of(&sized));
        // 黑块占中间一半：出光段约在 12.5..37.5 × 6.25..18.75 mm 内，X 另有过扫
        assert!((36.0..=40.0).contains(&max_x) && (17.5..=20.0).contains(&max_y), "光栅没有按 50 mm 缩放（含 1 mm 过扫）: {max_x} {max_y}");

        let boxed = preview(serde_json::json!({"width_mm": 30, "height_mm": 30}));
        assert_eq!(boxed["result"]["artifacts"]["width_mm"], 30.0, "锁定比例放进框内: {boxed}");
        assert_eq!(boxed["result"]["artifacts"]["height_mm"], 15.0);

        let too_big = preview(serde_json::json!({"width_mm": 500}));
        assert_eq!(too_big["success"], false);
        assert!(too_big.to_string().contains("超出工作区"), "{too_big}");

        let outline = preview(serde_json::json!({"mode": "outline", "width_mm": 40}));
        assert_eq!(outline["success"], true, "{outline}");
        assert_eq!(outline["result"]["artifacts"]["mode"], "outline");
        let gcode = gcode_of(&outline);
        let cuts = gcode.lines().filter(|line| line.starts_with("G1")).count();
        assert!((4..=40).contains(&cuts), "轮廓应是少量矢量线段，不是逐行扫描: {cuts} 行");
        let (max_x, max_y) = gcode_max_xy(&gcode);
        assert!((29.0..=31.0).contains(&max_x) && (14.0..=16.0).contains(&max_y), "轮廓没有按 40 mm 缩放: {max_x} {max_y}");
    }

    fn parse_json_response(response: &[u8]) -> serde_json::Value {
        let start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        serde_json::from_slice(&response[start..]).unwrap()
    }

    fn test_loopback_settings() -> laser_runtime::LaserSettings {
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 8766,
            material_params_file: None,
        }
    }

    #[test]
    fn material_safety_gate_rejects_pvc_and_unknown_plastic() {
        assert!(validate_material_safety("PVC")
            .unwrap_err()
            .contains("PVC"));
        assert!(validate_material_safety("塑料")
            .unwrap_err()
            .contains("成分不明确"));
        assert!(validate_material_safety("椴木").is_ok());
    }

    #[test]
    fn workflow_preview_applies_material_safety_gate_with_and_without_store() {
        for use_store in [true, false] {
            let mut service = ApiService::new(
                test_loopback_settings(),
                laser_contracts::builtin_material_library(),
            );
            if use_store {
                service = service.with_store(std::sync::Arc::new(
                    laser_runtime::MemoryStore::new(),
                ));
            }

            let blocked = post_json(
                &service,
                "/api/workflow",
                &serde_json::json!({
                    "action": "preview",
                    "source_type": "text",
                    "text": "MOSS",
                    "material": "PVC",
                    "thickness_mm": 3
                }),
            );
            assert_eq!(blocked["success"], false, "{blocked}");
            assert_eq!(blocked["detail"]["error_code"], "material_safety_blocked");
        }
    }

    fn fake_transport_factory(ok_lines: usize) -> TransportFactory {
        struct ResponsiveGrbl {
            responses: std::sync::Mutex<std::collections::VecDeque<String>>,
            remaining: std::sync::atomic::AtomicUsize,
        }
        impl laser_device::DeviceTransport for ResponsiveGrbl {
            fn write_line(&self, line: &str) -> Result<(), String> {
                let mut responses = self.responses.lock().unwrap();
                if line == "$$" {
                    responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                } else if line == "$CD" || line.starts_with("$RI=") {
                    // 模拟 FluidNC：配置读取与自动上报设置都只回 ok，不占用 G-code 行的 ack 配额
                    responses.push_back("ok".into());
                } else if line == "G4 P0" || self.remaining.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) > 0 {
                    responses.push_back("ok".into());
                }
                Ok(())
            }
            fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
                if command == laser_device::RealtimeCommand::StatusQuery {
                    self.responses.lock().unwrap().push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
                }
                Ok(())
            }
            fn read_line(&self) -> Result<Option<String>, String> {
                Ok(self.responses.lock().unwrap().pop_front())
            }
        }
        std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
            Ok(Box::new(ResponsiveGrbl {
                responses: Default::default(),
                remaining: std::sync::atomic::AtomicUsize::new(ok_lines),
            }) as Box<dyn laser_device::DeviceTransport>)
        })
    }

    fn recording_transport_factory(
        ok_lines: usize,
        written_lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> TransportFactory {
        struct RecordingGrbl {
            responses: std::sync::Mutex<std::collections::VecDeque<String>>,
            remaining: std::sync::atomic::AtomicUsize,
            written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }
        impl laser_device::DeviceTransport for RecordingGrbl {
            fn write_line(&self, line: &str) -> Result<(), String> {
                self.written.lock().unwrap().push(line.to_string());
                let mut responses = self.responses.lock().unwrap();
                if line == "$$" {
                    responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                } else if line == "$CD" || line.starts_with("$RI=") || line == "$X" {
                    responses.push_back("ok".into());
                } else if line == "G4 P0" || self.remaining.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) > 0 {
                    responses.push_back("ok".into());
                }
                Ok(())
            }
            fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
                if command == laser_device::RealtimeCommand::StatusQuery {
                    self.responses.lock().unwrap().push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
                }
                Ok(())
            }
            fn read_line(&self) -> Result<Option<String>, String> {
                Ok(self.responses.lock().unwrap().pop_front())
            }
        }
        std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
            Ok(Box::new(RecordingGrbl {
                responses: Default::default(),
                remaining: std::sync::atomic::AtomicUsize::new(ok_lines),
                written: written_lines.clone(),
            }) as Box<dyn laser_device::DeviceTransport>)
        })
    }

    #[test]
    fn job_launcher_rejects_power_or_busy_state_before_writing_gcode() {
        for (max, status) in [("0", "<Idle>"), ("1000", "<Run>"), ("1000", "<Check>")] {
            let fake = laser_device::FakeGrbl::new([
                format!("$30={max}"), "$32=1".into(), "ok".into(), status.into(),
            ]);
            let observed = fake.clone();
            let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
                .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake.clone()))));
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("job.gcode");
            std::fs::write(&file, "M4 S100\nG1 X1 F100\nM5\n").unwrap();
            let object = serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23});
            let sha = hex::encode(Sha256::digest(std::fs::read(&file).unwrap()));
            let (code, response) = service.launch_send_job("test-preflight", file, &sha, object.as_object().unwrap(), &laser_contracts::CallerContext::Loopback).unwrap_err();
            assert_eq!(code, 400);
            assert_eq!(response["detail"]["error_code"], "device_preflight_failed");
            assert_eq!(observed.lines(), ["$$", "$CD"]);
            assert!(service.active_jobs.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn job_launcher_accepts_255_scale_and_adapts_dynamically() {
        let fake = laser_device::FakeGrbl::new([
            "$30=255".into(), "$32=1".into(), "ok".into(), "<Idle>".into(), "ok".into(), "ok".into(), "ok".into(), "ok".into(), "<Idle>".into(),
        ]);
        let _observed = fake.clone();
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake.clone()))));
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("job.gcode");
        std::fs::write(&file, "M4 S1000\nG1 X1 F100\nM5\n").unwrap();
        let object = serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23});
        let sha = hex::encode(Sha256::digest(std::fs::read(&file).unwrap()));
        let res = service.launch_send_job("test-255-adaptive", file, &sha, object.as_object().unwrap(), &laser_contracts::CallerContext::Loopback).unwrap();
        assert_eq!(res["success"], true);
    }

    fn wait_for_status(service: &ApiService, workflow_id: &str, wanted: &str) -> serde_json::Value {
        let status_body = format!(r#"{{"action":"status","workflow_id":"{workflow_id}"}}"#);
        let status_request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            status_body.len(),
            status_body
        );
        let start = std::time::Instant::now();
        loop {
            let value = parse_json_response(&service.handle_http(status_request.as_bytes()));
            if value["result"]["status"] == wanted || start.elapsed() > std::time::Duration::from_secs(5) {
                return value;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn run_confirm_send_gate_scenario(service: &ApiService, root: &std::path::Path) {
        use std::io::Write;
        let body = serde_json::json!({
            "text": "SHA", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
        })
        .to_string();
        let request = format!(
            "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let generated = parse_json_response(&service.handle_http(request.as_bytes()));
        assert_eq!(generated["success"], true, "{generated}");
        let workflow_id = generated["result"]["workflow_id"].as_str().unwrap().to_owned();
        let gcode_file = generated["result"]["artifacts"]["gcode_file"].as_str().unwrap().to_owned();
        let expected_sha = generated["result"]["artifacts"]["gcode_sha256"].as_str().unwrap().to_owned();
        let gcode_path = root.join(&gcode_file);
        let original = std::fs::read(&gcode_path).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&original)),
            expected_sha,
            "冻结的 SHA 必须来自物理刀路文件"
        );

        let send_body = format!(r#"{{"action":"confirm_send","workflow_id":"{workflow_id}","confirmed":true,"connection_mode":"network","network_host":"127.0.0.1","network_telnet_port":23}}"#);
        let send_request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            send_body.len(),
            send_body
        );

        // 1. 篡改落盘刀路：重算 SHA 与快照不一致，必须拒绝
        std::fs::OpenOptions::new()
            .append(true)
            .open(&gcode_path)
            .unwrap()
            .write_all(b"\nG1 X999 Y999 S1000\n")
            .unwrap();
        let rejected_text = String::from_utf8_lossy(&service.handle_http(send_request.as_bytes())).into_owned();
        assert!(rejected_text.starts_with("HTTP/1.1 400"), "{rejected_text}");
        assert!(rejected_text.contains("send_gate_rejected"), "{rejected_text}");
        assert!(rejected_text.contains("SHA-256"), "{rejected_text}");

        // 2. 恢复原文件：门禁通过并推进到 sending，快照冻结物理 SHA
        std::fs::write(&gcode_path, &original).unwrap();
        let accepted = parse_json_response(&service.handle_http(send_request.as_bytes()));
        assert_eq!(accepted["success"], true, "{accepted}");
        assert_eq!(accepted["result"]["status"], "sending");
        assert_eq!(accepted["result"]["confirmation_snapshot"]["gcode_sha256"], expected_sha);

        assert_eq!(accepted["result"]["send_result"]["success"], true, "{accepted}");

        // 3. 发送中/已完成重复下发必须拦截（防二次烧蚀）
        let duplicate_text = String::from_utf8_lossy(&service.handle_http(send_request.as_bytes())).into_owned();
        assert!(duplicate_text.starts_with("HTTP/1.1 400"), "{duplicate_text}");
        assert!(duplicate_text.contains("workflow_not_sendable"), "{duplicate_text}");

        // 4. 真实 JobWorker 逐行发送并等待 ok，轮询推进到终态；取消后不可再发送
        let finished = wait_for_status(service, &workflow_id, "completed");
        assert_eq!(finished["result"]["status"], "completed", "{finished}");
        let sent = finished["result"]["send_result"]["lines_sent"].as_u64().unwrap_or(0);
        let total = finished["result"]["send_result"]["total_lines"].as_u64().unwrap_or(0);
        assert!(total > 0 && sent == total, "全部刀路行必须收到设备 ok: {sent}/{total}");
        assert_eq!(service.active_job_count(), 0, "作业完成后必须从注册表移除");
        let cancel_body = format!(r#"{{"action":"cancel","workflow_id":"{workflow_id}"}}"#);
        let cancel_request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            cancel_body.len(),
            cancel_body
        );
        assert_eq!(
            parse_json_response(&service.handle_http(cancel_request.as_bytes()))["result"]["status"],
            "cancelled"
        );
        let after_cancel = String::from_utf8_lossy(&service.handle_http(send_request.as_bytes())).into_owned();
        assert!(after_cancel.contains("workflow_not_sendable"), "{after_cancel}");
    }

    #[test]
    fn confirm_send_recomputes_physical_gcode_sha_for_store_and_memory_backends() {
        for use_store in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let mut service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
                .with_generated_root(temp.path().to_path_buf())
                .with_transport_factory(fake_transport_factory(200_000));
            if use_store {
                service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
            }
            run_confirm_send_gate_scenario(&service, temp.path());
        }
    }

    /// SD 脱机：上传成功但固件不返回文件哈希（未启用 HashFS）时不得发送 `$SD/Run`（2026-10-14）。
    #[test]
    fn sd_offline_refuses_to_run_when_firmware_returns_no_file_hash() {
        use std::io::{Read, Write};
        let upload = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let http_port = upload.local_addr().unwrap().port();
        let uploads = std::thread::spawn(move || {
            let (mut stream, _) = upload.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
            let mut buffer = [0u8; 4096];
            while let Ok(n) = stream.read(&mut buffer) {
                if n == 0 { break; }
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_generated_root(temp.path().to_path_buf())
            .with_transport_factory(recording_transport_factory(200_000, written.clone()));
        let preview = post_json(&service, "/api/text-task/generate", &serde_json::json!({
            "text": "SD", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
        }));
        let workflow_id = preview["result"]["workflow_id"].as_str().unwrap().to_owned();
        let send = post_json(&service, "/api/workflow", &serde_json::json!({
            "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
            "connection_mode": "network", "network_host": "127.0.0.1", "network_telnet_port": 23,
            "network_http_port": http_port, "offline_sd": true
        }));
        assert_eq!(send["success"], true, "{send}");
        let status = wait_for_status(&service, &workflow_id, "failed");
        uploads.join().unwrap();
        assert_eq!(status["result"]["status"], "failed", "{status}");
        assert!(status["result"]["send_result"]["error"].as_str().unwrap_or("").contains("HashFS"), "{status}");
        let lines = written.lock().unwrap().clone();
        assert!(lines.iter().any(|line| line.starts_with("$File/ShowHash=")), "{lines:?}");
        assert!(!lines.iter().any(|line| line.starts_with("$SD/Run")), "没有固件哈希不能启动 SD 作业: {lines:?}");
    }

    #[test]
    fn send_rejects_gcode_changed_after_first_sha_check_without_writing_job_lines() {
        for use_store in [true, false] {
            for offline_sd in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let base_factory = recording_transport_factory(200_000, written.clone());
                let gcode_path = std::sync::Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));
                let path_for_factory = gcode_path.clone();
                let factory: TransportFactory = std::sync::Arc::new(move |spec| {
                    let path = path_for_factory.lock().unwrap().clone().unwrap();
                    std::fs::write(path, b"M4 S1000\nG1 X999 Y999 F100\nM5\n").unwrap();
                    base_factory(spec)
                });
                let mut service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
                    .with_generated_root(temp.path().to_path_buf())
                    .with_transport_factory(factory);
                if use_store {
                    service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
                }
                let preview = post_json(&service, "/api/text-task/generate", &serde_json::json!({
                    "text": "SHA", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
                }));
                assert_eq!(preview["success"], true, "{preview}");
                let workflow_id = preview["result"]["workflow_id"].as_str().unwrap();
                *gcode_path.lock().unwrap() = Some(temp.path().join(preview["result"]["artifacts"]["gcode_file"].as_str().unwrap()));
                let send = post_json(&service, "/api/workflow", &serde_json::json!({
                    "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
                    "connection_mode": "network", "network_host": "127.0.0.1",
                    "network_telnet_port": 23, "offline_sd": offline_sd
                }));
                assert_eq!(send["detail"]["error_code"], "send_gate_rejected", "{send}");
                assert!(written.lock().unwrap().iter().all(|line| line == "$$" || line == "$CD" || line.starts_with("$RI=")),
                    "校验失败不能写入 G-code、SD 启动或执行同步命令: {:?}", written.lock().unwrap());
                assert_eq!(service.active_job_count(), 0);
            }
        }
    }

    #[test]
    fn auth_challenge_is_loopback_only_and_issued_code_pairs_remote_client() {
        let auth = std::sync::Arc::new(AuthManager::new());
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "0.0.0.0".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_auth(auth.clone());
        let challenge_request =
            b"POST /v1/auth/challenge HTTP/1.1\r\nHost: remote\r\nContent-Length: 0\r\n\r\n";
        let denied = service.handle_http_from(challenge_request, false, "10.0.0.8", None);
        assert!(String::from_utf8_lossy(&denied).starts_with("HTTP/1.1 403"));

        // 本机客户端以 127.0.0.1 访问（回环连接的 Host 须通过 DNS 重绑定校验）。
        let local_challenge =
            b"POST /v1/auth/challenge HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: 0\r\n\r\n";
        let issued = service.handle_http_from(local_challenge, true, "127.0.0.1", None);
        assert!(String::from_utf8_lossy(&issued).starts_with("HTTP/1.1 200 OK"));
        let payload = parse_json_response(&issued);
        let code = payload["result"]["pairing_code"].as_str().unwrap().to_owned();
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(payload["result"]["valid_seconds"], 300);

        let body = format!(r#"{{"pairing_code":"{code}"}}"#);
        let pair = format!(
            "POST /v1/auth/pair HTTP/1.1\r\nHost: remote\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let paired = service.handle_http_from(pair.as_bytes(), false, "10.0.0.8", None);
        assert!(String::from_utf8_lossy(&paired).starts_with("HTTP/1.1 200 OK"));
        assert!(parse_json_response(&paired)["result"]["token"].as_str().is_some());
    }

    #[test]
    fn http_request_reader_rejects_oversized_headers_and_bodies() {
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut results = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                results.push(read_http_request(&mut stream).map(|_| ()).map_err(|e| e.kind()));
            }
            results
        });
        // 1. 70KB 没有终止符的畸形请求头
        let mut oversized_header = TcpStream::connect(address).unwrap();
        let _ = oversized_header.write_all(&vec![b'A'; 70 * 1024]);
        let _ = oversized_header.shutdown(std::net::Shutdown::Write);
        // 2. 声明 21MB 请求体
        let mut oversized_body = TcpStream::connect(address).unwrap();
        let _ = oversized_body.write_all(
            b"POST /api/upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: 22020096\r\n\r\nxx",
        );
        let _ = oversized_body.shutdown(std::net::Shutdown::Write);
        let results = server.join().unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|r| matches!(r, Err(std::io::ErrorKind::InvalidData))),
            "{results:?}"
        );
        drop(oversized_header);
        drop(oversized_body);
    }

    #[test]
    fn material_manage_writes_through_to_runtime_db_when_attached() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("materials.json");
        let db = std::sync::Arc::new(std::sync::Mutex::new(
            laser_runtime::RuntimeDb::open_memory().unwrap(),
        ));
        let mut settings = test_loopback_settings();
        settings.material_params_file = Some(file.display().to_string());
        let service = ApiService::new(settings, laser_contracts::builtin_material_library())
            .with_runtime_db(db.clone());

        let save = serde_json::json!({
            "action": "save", "material": "测试胶合板", "thickness_mm": 4, "laser_mode": "cut",
            "laser_min_power": 0, "laser_max_power": 900, "feed_rate": 300, "travel_rate": 3000,
            "passes": 2, "pixel_size_mm": 0.1, "threshold": -1
        })
        .to_string();
        let request = format!(
            "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            save.len(),
            save
        );
        let saved = parse_json_response(&service.handle_http(request.as_bytes()));
        assert_eq!(saved["success"], true, "{saved}");
        let count_cells = |sql: &str| -> i64 {
            db.lock().unwrap().connection.query_row(sql, [], |row| row.get(0)).unwrap()
        };
        assert_eq!(
            count_cells("SELECT COUNT(*) FROM material_params WHERE material_name='测试胶合板' AND thickness_key='4' AND laser_mode='cut'"),
            1,
            "保存必须写入 SQLite material_params"
        );
        assert!(db.lock().unwrap().export_material_library().unwrap().materials.contains_key("测试胶合板"));

        let delete = serde_json::json!({
            "action": "delete", "material": "测试胶合板", "thickness_mm": 4, "confirmed": true
        })
        .to_string();
        let request = format!(
            "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            delete.len(),
            delete
        );
        let deleted = parse_json_response(&service.handle_http(request.as_bytes()));
        assert_eq!(deleted["success"], true, "{deleted}");
        assert_eq!(count_cells("SELECT COUNT(*) FROM material_params WHERE material_name='测试胶合板'"), 0);
    }

    #[test]
    fn recommendation_route_projects_core_result_and_rejects_bad_input() {
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        );
        let response = service.handle_http(
            b"GET /api/ui/material-recommendation?material=%E7%BA%B8%E6%9D%BF&thickness_mm=3&laser_mode=engrave&engraving_mode=outline HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        let body_start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let payload: serde_json::Value = serde_json::from_slice(&response[body_start..]).unwrap();
        assert_eq!(payload["result"]["match"], "nearest_thickness");
        assert_eq!(payload["result"]["laser_max_power"], 220);
        let bad = service.handle_http(
            b"GET /api/ui/material-recommendation?material=x&thickness_mm=0&laser_mode=engrave HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(String::from_utf8_lossy(&bad).starts_with("HTTP/1.1 400 Bad Request"));
    }

    #[test]
    fn static_routes_and_sse_have_safe_wire_contracts() {
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        );
        // A release EXE must serve the embedded React page from /. Prior
        // builds returned a 404 JSON error because the embedded dist was stale.
        let home = service.handle_http(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let page = String::from_utf8_lossy(&home);
        assert!(page.starts_with("HTTP/1.1 200 OK"));
        assert!(page.contains("<div id=\"root\"></div>"));
        assert!(page.contains("/assets/"));
        for route in ["/draw", "/material-lab", "/voice", "/twin"] {
            // 在任一页面按 F5 刷新都应回到前端页面，而不是 404
            let index = service.handle_http(format!("GET {route} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());
            assert!(String::from_utf8_lossy(&index).starts_with("HTTP/1.1 200 OK"), "{route}");
            assert!(String::from_utf8_lossy(&index).contains("text/html"), "{route}");
        }
        let traversal =
            service.handle_http(b"GET /assets/../index.html HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(String::from_utf8_lossy(&traversal).starts_with("HTTP/1.1 404 Not Found"));
        let events = service
            .handle_http(b"GET /v1/events?workflow_id=wf-1 HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let event_text = String::from_utf8_lossy(&events);
        assert!(event_text.starts_with("HTTP/1.1 200 OK"));
        assert!(event_text.contains("text/event-stream"));
        assert!(event_text.contains("\"workflow_id\":\"wf-1\""));
    }

    fn post_json(service: &ApiService, route: &str, body: &serde_json::Value) -> serde_json::Value {
        let body = body.to_string();
        let request = format!(
            "POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        parse_json_response(&service.handle_http(request.as_bytes()))
    }

    #[test]
    fn material_lab_preview_send_and_select_cell_run_real_pipeline() {
        for use_store in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let gen_dir = temp.path().join("generated");
            std::fs::create_dir_all(&gen_dir).unwrap();
            let params_file = temp.path().join("materials.json");
            let settings = laser_runtime::LaserSettings {
                material_params_file: Some(params_file.to_string_lossy().into_owned()),
                ..test_loopback_settings()
            };
            let mut service = ApiService::new(settings, laser_contracts::builtin_material_library())
                .with_generated_root(gen_dir.clone())
                .with_preview_roots(vec![gen_dir.clone()])
                .with_transport_factory(fake_transport_factory(200_000));
            if use_store {
                service = service.with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()));
            }

            // 1. 预览：真实生成 4x3 矩阵、落盘 G-code、返回 calibration_id 与 cells
            let preview = post_json(&service, "/api/material-lab/preview", &serde_json::json!({
                "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave", "matrix_kind": "engrave",
                "rows": 4, "columns": 3, "power_min": 100, "power_max": 500, "speed_min": 600, "speed_max": 2400
            }));
            assert_eq!(preview["success"], true, "{preview}");
            let cal_id = preview["result"]["calibration_id"].as_str().unwrap().to_owned();
            assert!(cal_id.starts_with("cal-"), "{cal_id}");
            assert_eq!(preview["result"]["status"], "prepared");
            let cells = preview["result"]["cells"].as_array().unwrap();
            assert_eq!(cells.len(), 12);
            assert_eq!(cells[0]["power"], 100);
            assert_eq!(cells[2]["power"], 500);
            assert_eq!(cells[0]["speed"], 600);
            assert_eq!(cells[11]["speed"], 2400);
            let gcode_file = preview["result"]["gcode_file"].as_str().unwrap().to_owned();
            let gcode_path = gen_dir.join(&gcode_file);
            let original = std::fs::read(&gcode_path).unwrap();
            assert!(original.len() > 200, "标定 G-code 必须真实落盘");
            assert_eq!(
                hex::encode(Sha256::digest(&original)),
                preview["result"]["gcode_sha256"].as_str().unwrap()
            );

            // 2. 未确认 / 缺 host 必须拒绝
            let unconfirmed = post_json(&service, "/api/material-lab/send", &serde_json::json!({"calibration_id": cal_id}));
            assert_eq!(unconfirmed["success"], false);
            let no_host = post_json(&service, "/api/material-lab/send", &serde_json::json!({"calibration_id": cal_id, "confirmed": true}));
            assert_eq!(no_host["success"], false);

            // 3. 篡改落盘刀路：物理 SHA 门禁拒绝
            std::fs::write(&gcode_path, b"G1 X999 Y999 S1000\n").unwrap();
            let tampered = post_json(&service, "/api/material-lab/send", &serde_json::json!({
                "calibration_id": cal_id, "confirmed": true, "network_host": "127.0.0.1", "network_telnet_port": 23
            }));
            assert_eq!(tampered["success"], false, "{tampered}");
            assert_eq!(tampered["detail"]["error_code"], "send_gate_rejected", "{tampered}");
            std::fs::write(&gcode_path, &original).unwrap();

            // 4. 真实 JobWorker 下发并推进到 completed
            let sent = post_json(&service, "/api/material-lab/send", &serde_json::json!({
                "calibration_id": cal_id, "confirmed": true, "network_host": "127.0.0.1", "network_telnet_port": 23
            }));
            assert_eq!(sent["success"], true, "{sent}");
            assert_eq!(sent["result"]["status"], "sending");
            assert_eq!(sent["result"]["probe"]["connected"], true);
            assert_eq!(sent["result"]["send_result"]["running"], true);
            let total = sent["result"]["send_result"]["total_lines"].as_u64().unwrap();
            assert!(total > 0);
            let start = std::time::Instant::now();
            let finished = loop {
                let status = post_json(&service, "/api/material-lab/status", &serde_json::json!({"calibration_id": cal_id}));
                if status["result"]["status"] == "completed" || start.elapsed() > std::time::Duration::from_secs(5) {
                    break status;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            assert_eq!(finished["result"]["status"], "completed", "{finished}");
            assert_eq!(finished["result"]["send_result"]["lines_sent"].as_u64().unwrap(), total);
            assert_eq!(service.active_job_count(), 0);

            // 5. 完成后禁止重复下发
            let again = post_json(&service, "/api/material-lab/send", &serde_json::json!({
                "calibration_id": cal_id, "confirmed": true, "network_host": "127.0.0.1", "network_telnet_port": 23
            }));
            assert_eq!(again["detail"]["error_code"], "calibration_not_sendable", "{again}");

            // 6. 选格：单元格 #6（第 2 行第 3 列）功率 500、速度 = 600+(2400-600)/3 = 1200 写回材料库
            let selected = post_json(&service, "/api/material-lab/select-cell", &serde_json::json!({
                "calibration_id": cal_id, "cell_number": 6, "material": "椴木", "thickness_mm": 3, "laser_mode": "engrave"
            }));
            assert_eq!(selected["success"], true, "{selected}");
            assert_eq!(selected["result"]["selected_cell"]["power"], 500);
            assert_eq!(selected["result"]["selected_cell"]["speed"], 1200);
            let saved_cell = &selected["result"]["materials"]["materials"]["椴木"]["thicknesses"]["3"]["engrave"]["raster"];
            assert_eq!(saved_cell["laser_max_power"], 500, "{selected}");
            assert_eq!(saved_cell["feed_rate"], 1200, "{selected}");
            assert_eq!(saved_cell["source"], "calibration");
            let on_disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&params_file).unwrap()).unwrap();
            assert_eq!(on_disk["materials"]["椴木"]["thicknesses"]["3"]["engrave"]["raster"]["feed_rate"], 1200);
            let out_of_range = post_json(&service, "/api/material-lab/select-cell", &serde_json::json!({"calibration_id": cal_id, "cell_number": 13}));
            assert_eq!(out_of_range["success"], false);
            let missing = post_json(&service, "/api/material-lab/select-cell", &serde_json::json!({"calibration_id": "cal-nope", "cell_number": 1}));
            assert_eq!(missing["success"], false);
        }
    }

    #[test]
    fn material_manage_persists_export_import_save_and_delete() {
        let temp = tempfile::tempdir().unwrap();
        let material_file = temp.path().join("materials.json");
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: Some(material_file.to_string_lossy().into_owned()),
            },
            laser_contracts::builtin_material_library(),
        );
        let request = |body: serde_json::Value| {
            let raw = body.to_string();
            let request = format!(
                "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
                raw.len(),
                raw
            );
            service.handle_http(request.as_bytes())
        };
        let saved = request(serde_json::json!({
            "action": "save",
            "material": "测试材料",
            "thickness_mm": 2,
            "laser_mode": "engrave",
            "engraving_mode": "raster",
            "laser_max_power": "500",
            "feed_rate": "1000",
            "aliases": "test, 测试"
        }));
        assert!(String::from_utf8_lossy(&saved).starts_with("HTTP/1.1 200 OK"));
        let exported = service
            .handle_http(b"GET /api/material-lab/export HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let start = exported
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let exported: serde_json::Value = serde_json::from_slice(&exported[start..]).unwrap();
        assert_eq!(
            exported["result"]["library"]["materials"]["测试材料"]["thicknesses"]["2"]["engrave"]["raster"]
                ["laser_max_power"],
            500
        );
        let invalid =
            request(serde_json::json!({"action": "import", "library_json": {"version": "1"}}));
        assert!(String::from_utf8_lossy(&invalid).starts_with("HTTP/1.1 400 Bad Request"));
        let deleted = request(serde_json::json!({
            "action": "delete",
            "material": "测试材料",
            "thickness_mm": 2,
            "confirmed": true
        }));
        assert!(String::from_utf8_lossy(&deleted).starts_with("HTTP/1.1 200 OK"));
    }

    #[test]
    fn workflow_routes_keep_preview_status_cancel_and_send_gate_stateful() {
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        );
        let body = br#"{"action":"preview","source_type":"text","text":"hello"}"#;
        let request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        let preview = service.handle_http(request.as_bytes());
        let start = preview
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let preview: serde_json::Value = serde_json::from_slice(&preview[start..]).unwrap();
        let workflow_id = preview["result"]["workflow_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(preview["result"]["status"], "preview_ready");

        let status_body = format!(r#"{{"action":"status","workflow_id":"{workflow_id}"}}"#);
        let status_request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            status_body.len(),
            status_body
        );
        let status = service.handle_http(status_request.as_bytes());
        assert!(String::from_utf8_lossy(&status).starts_with("HTTP/1.1 200 OK"));

        let send_body = format!(
            r#"{{"action":"confirm_send","workflow_id":"{workflow_id}","confirmed":true}}"#
        );
        let send_request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            send_body.len(),
            send_body
        );
        let send = service.handle_http(send_request.as_bytes());
        assert!(String::from_utf8_lossy(&send).contains("confirmation_snapshot_missing"));
    }

    #[test]
    fn workflow_routes_persist_when_runtime_db_is_attached() {
        let store = std::sync::Arc::new(laser_runtime::MemoryStore::new());
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_store(store.clone());
        let body = br#"{"action":"preview","source_type":"text","text":"persist"}"#;
        let request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        let response = service.handle_http(request.as_bytes());
        let start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let response: serde_json::Value = serde_json::from_slice(&response[start..]).unwrap();
        let workflow_id = response["result"]["workflow_id"].as_str().unwrap();
        use laser_runtime::RuntimeStore;
        let stored = store
            .get(laser_runtime::RecordKind::Workflow, workflow_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status.as_deref(), Some("preview_ready"));
    }

    #[test]
    fn material_manage_supports_d9_atomic_batch_thicknesses() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("materials.json");
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: Some(file.display().to_string()),
            },
            laser_contracts::builtin_material_library(),
        );
        let batch_body = serde_json::json!({
            "action": "save",
            "material": "椴木测试板",
            "thicknesses": {
                "2.0": {
                    "cut": {
                        "laser_min_power": 0,
                        "laser_max_power": 900,
                        "feed_rate": 300,
                        "travel_rate": 3000,
                        "threshold": -1,
                        "passes": 2,
                        "pixel_size_mm": 0.1
                    }
                },
                "3.0": {
                    "cut": {
                        "laser_min_power": 0,
                        "laser_max_power": 1000,
                        "feed_rate": 200,
                        "travel_rate": 3000,
                        "threshold": -1,
                        "passes": 3,
                        "pixel_size_mm": 0.1
                    }
                }
            }
        });
        let raw = serde_json::to_vec(&batch_body).unwrap();
        let request = format!(
            "POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            raw.len(),
            String::from_utf8_lossy(&raw)
        );
        let response = service.handle_http(request.as_bytes());
        let start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let parsed: serde_json::Value = serde_json::from_slice(&response[start..]).unwrap();
        assert_eq!(parsed["success"], true);
        assert!(
            parsed["result"]["materials"]["materials"]["椴木测试板"]["thicknesses"]["2.0"]["cut"]
                .is_object()
        );
        assert!(
            parsed["result"]["materials"]["materials"]["椴木测试板"]["thicknesses"]["3.0"]["cut"]
                .is_object()
        );
    }

    #[test]
    fn text_generate_writes_real_gcode_and_preview_artifacts() {
        let font = std::path::PathBuf::from(r"C:\Windows\Fonts\arial.ttf");
        if !font.exists() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(temp.path().to_path_buf());
        let body = serde_json::json!({
            "text": "A",
            "material": "椴木",
            "thickness_mm": 3,
            "font_path": font,
            "font_size_mm": 8
        })
        .to_string();
        let request = format!(
            "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = service.handle_http(request.as_bytes());
        let start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let response: serde_json::Value = serde_json::from_slice(&response[start..]).unwrap();
        assert_eq!(response["success"], true);
        let gcode = response["result"]["artifacts"]["gcode_file"]
            .as_str()
            .unwrap();
        let preview = response["result"]["artifacts"]["image_file"]
            .as_str()
            .unwrap();
        assert!(!std::path::Path::new(gcode).is_absolute());
        assert!(std::fs::metadata(temp.path().join(gcode)).unwrap().len() > 0);
        assert!(std::fs::metadata(temp.path().join(preview)).unwrap().len() > 0);
        assert!(
            std::fs::read_to_string(temp.path().join(gcode))
                .unwrap()
                .contains("G1")
        );
    }

    #[test]
    fn text_generate_falls_back_to_system_font_when_omitted() {
        if laser_core::font::find_system_font().is_none() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_generated_root(temp.path().to_path_buf());
        let body = serde_json::json!({
            "text": "MOSS",
            "material": "椴木",
            "thickness_mm": 3,
            "font_size_mm": 8
        })
        .to_string();
        let request = format!(
            "POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let response = service.handle_http(request.as_bytes());
        assert!(String::from_utf8_lossy(&response).contains("\"success\":true"));
    }

    #[test]
    fn preview_requires_suffix_and_allowed_root() {
        let temp = tempfile::tempdir().unwrap();
        let allowed = temp.path().join("generated_assets");
        std::fs::create_dir_all(&allowed).unwrap();
        let file = allowed.join("x.gcode");
        std::fs::write(&file, b"G0 X0").unwrap();
        assert!(resolve_preview_file(&file, std::slice::from_ref(&allowed)).is_ok());
        assert!(resolve_preview_file(
            std::path::Path::new("x.gcode"),
            std::slice::from_ref(&allowed)
        )
        .is_ok());
        let secret = temp.path().join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();
        assert_eq!(
            resolve_preview_file(&secret, std::slice::from_ref(&allowed)).unwrap_err(),
            "不支持预览该文件类型"
        );
    }

    #[test]
    fn upload_sanitizes_name_and_uses_sha1_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let result = save_upload(
            "../hello?.gcode",
            "data:text/plain;base64,RzAgWDA=",
            temp.path(),
            123,
        )
        .unwrap();
        assert_eq!(result.filename, "123_ccb5aeb7c2_hello_.gcode");
        assert_eq!(std::fs::read(&result.path).unwrap(), b"G0 X0");
        assert!(save_upload("x.exe", "eA==", temp.path(), 123).is_err());
    }

    #[test]
    fn http_adapter_answers_real_loopback_request() {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        let service = std::sync::Arc::new(ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap();
            stream.write_all(&service.handle_http(&request)).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("\"status\":\"ok\""));
        // 不带 Origin（非浏览器客户端）：不输出任何 CORS 头，绝不出现通配符。
        assert!(!response.contains("Access-Control-Allow-"));
        assert!(!response.contains("Allow-Origin: *"));
    }

    #[test]
    fn http_adapter_handles_cors_options_preflight() {
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        );
        let request = b"OPTIONS /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nOrigin: http://tauri.localhost\r\n\r\n";
        let response = String::from_utf8(service.handle_http(request)).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Access-Control-Allow-Origin: http://tauri.localhost\r\n"));
        assert!(response.contains("Vary: Origin"));
        assert!(response.contains("Access-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS"));
        assert!(response.contains(
            "Access-Control-Allow-Headers: Content-Type, Authorization, Accept, X-Moss-Desktop-Session"
        ));
        assert!(response.contains("Access-Control-Max-Age: 86400"));
        assert!(!response.contains("Allow-Origin: *"));

        // 不允许的来源：预检 403，且不带任何 CORS 头。
        let denied = b"OPTIONS /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nOrigin: http://evil.example\r\n\r\n";
        let denied = String::from_utf8(service.handle_http(denied)).unwrap();
        assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");
        assert!(denied.contains("origin_not_allowed"));
        assert!(!denied.contains("Access-Control-Allow-"));
    }

    fn cors_test_service() -> ApiService {
        ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 9666,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
    }

    #[test]
    fn allowed_origin_rules_are_exact() {
        let none: Vec<String> = Vec::new();
        // 允许项
        assert!(is_allowed_origin("http://tauri.localhost", Some("127.0.0.1:9666"), true, &none));
        assert!(is_allowed_origin("tauri://localhost", Some("127.0.0.1:9666"), false, &none));
        assert!(is_allowed_origin("http://127.0.0.1:9666", Some("127.0.0.1:9666"), true, &none));
        assert!(is_allowed_origin("http://192.0.2.10:9666", Some("192.0.2.10:9666"), false, &none));
        for dev in [
            "http://127.0.0.1:5173",
            "http://localhost:5173",
            "http://127.0.0.1:5177",
            "http://localhost:5177",
            "http://127.0.0.1:5178",
            "http://localhost:5178",
        ] {
            assert!(is_allowed_origin(dev, Some("127.0.0.1:9666"), true, &none), "{dev}");
            // 非回环请求下开发端口不放行
            assert!(!is_allowed_origin(dev, Some("192.0.2.10:9666"), false, &none), "{dev}");
        }
        let extra = vec!["http://lab.example:8080".to_owned()];
        assert!(is_allowed_origin("http://lab.example:8080", Some("127.0.0.1:9666"), false, &extra));

        // 拒绝项
        for bad in [
            "http://tauri.localhost.evil.com",
            "http://evil.com",
            "null",
            "",
            " http://tauri.localhost",
            "HTTP://TAURI.LOCALHOST",
            "http://Tauri.localhost",
            "http://tauri.localhost:8080",
            "https://tauri.localhost",
            "http://127.0.0.1:9667",
            "http://lab.example:8081",
            "http://lab.example",
        ] {
            assert!(!is_allowed_origin(bad, Some("127.0.0.1:9666"), true, &extra), "{bad:?}");
        }
        // 没有 Host 时不能靠同源放行
        assert!(!is_allowed_origin("http://127.0.0.1:9666", None, true, &none));
        assert!(!is_allowed_origin("http://127.0.0.1:9666", Some(""), true, &none));
    }

    /// 设置写不进 settings.json 时返回错误，不显示“已保存”；能写时一次写入并保留其他键（2026-10-14）。
    #[test]
    fn settings_save_reports_write_failure_instead_of_success() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("settings.json"), r#"{"voice":{"mode":"official"}}"#).unwrap();
        let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
        let saved = post_json(&service, "/api/settings", &serde_json::json!({"web_port": 9888, "lan_access": true, "jev_enabled": false}));
        assert_eq!(saved["success"], true, "{saved}");
        let file: serde_json::Value = serde_json::from_slice(&std::fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(file["web_port"], 9888);
        assert_eq!(file["lan_access"], true);
        assert_eq!(file["voice"]["mode"], "official", "其他键必须保留");
        assert!(!temp.path().join("settings.json.tmp").exists());

        // 运行目录指向一个文件：无法写入 settings.json
        let blocker = temp.path().join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let broken = cors_test_service().with_runtime_path(blocker);
        let failed = post_json(&broken, "/api/settings", &serde_json::json!({"lan_access": false}));
        assert_eq!(failed["success"], false, "{failed}");
        assert_eq!(failed["detail"]["error_code"], "settings_write_failed");

        let no_runtime = post_json(&cors_test_service(), "/api/settings", &serde_json::json!({"web_port": 9888}));
        assert_eq!(no_runtime["detail"]["error_code"], "settings_write_failed", "{no_runtime}");
    }

    #[test]
    fn disallowed_origin_post_is_rejected_before_routing() {
        let temp = tempfile::tempdir().unwrap();
        let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
        let body = r#"{"web_port":9888}"#;
        for content_type in ["application/json", "text/plain"] {
            let request = format!(
                "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://evil.example\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
            assert!(response.starts_with("HTTP/1.1 403 Forbidden"), "{response}");
            assert!(response.contains("\"error_code\":\"origin_not_allowed\""));
            assert!(!response.contains("Access-Control-Allow-"));
            // 业务未执行：settings.json 未被写入
            assert!(!temp.path().join("settings.json").exists());
        }

        let workflow = r#"{"action":"status","workflow_id":""}"#;
        let request = format!(
            "POST /api/workflow HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://evil.example\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{workflow}",
            workflow.len()
        );
        let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(service.workflows.lock().unwrap().is_empty());

        // 不带 Origin 的同一请求行为不变
        let request = format!(
            "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
        assert!(response.contains("\"success\":true"), "{response}");
        assert!(!response.contains("Access-Control-Allow-"));
        assert!(std::fs::read_to_string(temp.path().join("settings.json")).unwrap().contains("9888"));
    }

    #[test]
    fn caller_context_is_built_by_entry_never_from_body() {
        use laser_contracts::CallerContext;
        assert_eq!(caller_context_for(None, true, None), CallerContext::Loopback);
        assert_eq!(
            caller_context_for(None, false, Some("pair_1".into())),
            CallerContext::Lan { pairing_id: "pair_1".into() }
        );
        assert_eq!(
            caller_context_for(Some(CallerContext::McpBridge), true, None),
            CallerContext::McpBridge
        );
        // A-36：请求体伪造的身份字段被删除，其余业务字段保留
        let mut object = serde_json::json!({
            "action": "status",
            "workflow_id": "wf-1",
            "caller_context": "Desktop",
            "caller": "desktop",
            "_caller": {"kind": "approval"},
            "desktop_session": "forged",
            "approval_id": "apr_forged",
            "gateway_session": "forged"
        })
        .as_object()
        .cloned()
        .unwrap();
        strip_identity_fields(&mut object);
        let keys: Vec<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["action", "workflow_id"]);
    }

    #[test]
    fn settings_route_never_accepts_export_dir() {
        // 技术书 §9.7.3：导出目录只能由桌面壳经系统选择框修改，HTTP 接口不接受
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("settings.json"),
            br#"{"export":{"dir":"D:\\exports","ask_each_time":false}}"#,
        )
        .unwrap();
        let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
        let body = r#"{"export":{"dir":"C:\\Windows\\System32","ask_each_time":true},"export.dir":"C:\\evil"}"#;
        let request = format!(
            "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = service.handle_http(request.as_bytes());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["export"]["dir"], "D:\\exports");
        assert_eq!(saved["export"]["ask_each_time"], false);
        assert!(saved.get("export.dir").is_none());

        // 后端写其他设置时保留桌面壳写入的 export 键
        let port = r#"{"web_port":9777}"#;
        let request = format!(
            "POST /api/settings HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nContent-Length: {}\r\n\r\n{port}",
            port.len()
        );
        let _ = service.handle_http(request.as_bytes());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.path().join("settings.json")).unwrap()).unwrap();
        assert_eq!(saved["web_port"], 9777);
        assert_eq!(saved["export"]["dir"], "D:\\exports");
    }

    #[test]
    fn mcp_status_reports_only_real_telemetry() {
        // A-72：已配置但从未连接——不得出现任何编造值
        let empty = laser_contracts::McpTelemetrySnapshot::default();
        let status = mcp_status_json(Some("wss://example.test/mcp/?token=secret"), &empty, 19);
        assert_eq!(status["configured"], true);
        assert_eq!(status["connected"], false);
        assert_eq!(status["endpoint"], "wss://example.test/mcp/?token=***");
        assert_eq!(status["client_info"], "");
        assert_eq!(status["last_request"], "");
        assert_eq!(status["messages_count"], 0);
        assert_eq!(status["recent_events"], serde_json::json!([]));
        assert_eq!(status["tools_count"], 19);
        assert!(!status.to_string().contains("xz-mcp-broker"));
        assert!(!status.to_string().contains("16:00:02"));

        // 未配置
        let unconfigured = mcp_status_json(None, &empty, 20);
        assert_eq!(unconfigured["configured"], false);
        assert_eq!(unconfigured["endpoint"], "");
        assert_eq!(unconfigured["tools_count"], 20);

        // 已连接：原样反映真实遥测
        let live = laser_contracts::McpTelemetrySnapshot {
            connected: true,
            client_name: "client-a".into(),
            client_version: "1.2".into(),
            last_request: "tools/list (查询工具列表)".into(),
            last_request_time: "10:00:00".into(),
            messages_count: 4,
            recent_events: vec!["10:00:00 [RECV] initialize (client: client-a v1.2)".into()],
            endpoints: vec![laser_contracts::EndpointLink {
                id: "primary".into(),
                endpoint: "wss://example.test/mcp/".into(),
                connected: true,
                connections: 2,
                ..Default::default()
            }],
            ..Default::default()
        };
        let status = mcp_status_json(Some("wss://example.test/mcp/"), &live, 19);
        assert_eq!(status["connected"], true);
        assert_eq!(status["client_info"], "client-a v1.2");
        assert_eq!(status["messages_count"], 4);
        assert_eq!(status["recent_events"].as_array().unwrap().len(), 1);
        // NV8：各接入点的状态
        assert_eq!(status["endpoints"][0]["id"], "primary");
        assert_eq!(status["endpoints"][0]["connections"], 2);
        assert_eq!(empty_endpoints(&mcp_status_json(None, &empty, 19)), 0);
    }

    fn empty_endpoints(status: &serde_json::Value) -> usize {
        status["endpoints"].as_array().map(Vec::len).unwrap_or(usize::MAX)
    }

    #[test]
    fn system_info_tools_count_matches_actual_catalog() {
        let service = cors_test_service();
        let value = parse_json_response(&service.handle_http(
            b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let expected = laser_tools::ToolCatalog::from_environment().specs().len();
        assert_eq!(value["result"]["mcp"]["tools_count"], expected);
        assert_eq!(value["result"]["voice"]["mode"], "official");
        assert!(value["result"]["voice"]["device_sessions"].is_null());
        // NV8：关闭的工具不计入
        service.set_disabled_tools(["material_params_tool".to_owned(), "laser_safe_action_tool".to_owned()]);
        let value = parse_json_response(&service.handle_http(
            b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        assert_eq!(value["result"]["mcp"]["tools_count"], expected - 1, "停止类工具不能关闭，只少一个");
    }

    /// 技术书 §9.4.6 / A-68：语音中心只以桌面端 Tauri 命令提供，HTTP 后端（含 `moss-host serve`
    /// 与 Android）没有 `/v1/voice/*`；自建模式下顶栏读取语音中心写入的设备会话数（§9.5.3）。
    #[test]
    fn voice_routes_are_not_served_over_http_and_system_info_reads_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
        for raw in [
            &b"GET /v1/voice/status HTTP/1.1\r\nHost: localhost\r\n\r\n"[..],
            &b"GET /v1/voice/server/log?tail=200 HTTP/1.1\r\nHost: localhost\r\n\r\n"[..],
        ] {
            let response = service.handle_http(raw);
            assert!(
                String::from_utf8_lossy(&response).starts_with("HTTP/1.1 404"),
                "{}",
                String::from_utf8_lossy(&response)
            );
        }
        std::fs::write(temp.path().join("settings.json"), br#"{"voice":{"mode":"self_hosted"}}"#).unwrap();
        laser_contracts::global_voice_telemetry().set_device_sessions(Some(0));
        let value = parse_json_response(&service.handle_http(
            b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        laser_contracts::global_voice_telemetry().set_device_sessions(None);
        assert_eq!(value["result"]["voice"]["mode"], "self_hosted");
        assert_eq!(value["result"]["voice"]["device_sessions"], 0);
        // 内置链路（NV5）：会话数来自内置语音服务
        std::fs::write(temp.path().join("settings.json"), br#"{"voice":{"mode":"builtin"}}"#).unwrap();
        laser_contracts::global_voice_telemetry().set_device_sessions(Some(2));
        laser_contracts::global_voice_telemetry().set_pulse(Some("speaking"), 76);
        let builtin = parse_json_response(&service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"));
        laser_contracts::global_voice_telemetry().set_device_sessions(None);
        assert_eq!(builtin["result"]["voice"]["mode"], "builtin");
        assert_eq!(builtin["result"]["voice"]["device_sessions"], 2);
        assert_eq!(builtin["result"]["voice"]["phase"], "speaking");
        assert_eq!(builtin["result"]["voice"]["level"], 76);
        // 未知（未配置健康检查或查询失败）时为 null，不猜
        let value = parse_json_response(&service.handle_http(
            b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        assert!(value["result"]["voice"]["device_sessions"].is_null());
    }

    #[test]
    fn serial_ports_route_reports_enumerated_ports_and_first_as_default() {
        let service = cors_test_service().with_serial_port_lister(std::sync::Arc::new(|| {
            vec!["COM3".to_owned(), "COM7".to_owned()]
        }));
        let value = parse_json_response(&service.handle_http(
            b"GET /api/ui/serial-ports HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        assert_eq!(value["success"], true);
        assert_eq!(value["result"]["ports"], serde_json::json!(["COM3", "COM7"]));
        assert_eq!(value["result"]["default_port"], "COM3");
        assert_eq!(value["result"]["baudrate"], 115200);
    }

    #[test]
    fn loopback_host_rules_block_dns_rebinding_names() {
        let none: Vec<String> = Vec::new();
        for ok in [
            "127.0.0.1",
            "127.0.0.1:9666",
            "localhost",
            "LOCALHOST:9666",
            "192.0.2.10:9666",
            "[::1]",
            "[::1]:9666",
        ] {
            assert!(is_allowed_loopback_host(ok, &none), "{ok}");
        }
        for bad in [
            "",
            "evil.example",
            "evil.example:9666",
            "127.0.0.1.evil.example:9666",
            "localhost.evil.example",
            "127.0.0.1:abc",
            "127.0.0.1:",
            "::1",
            "[::1",
            "[]:9666",
            "[evil]:9666",
            "[::1]x",
        ] {
            assert!(!is_allowed_loopback_host(bad, &none), "{bad:?}");
        }
        let extra = vec!["http://moss-lab.example:8080".to_owned()];
        assert!(is_allowed_loopback_host("moss-lab.example:9666", &extra));
        assert!(is_allowed_loopback_host("MOSS-LAB.example", &extra));
        assert!(!is_allowed_loopback_host("other.example", &extra));
    }

    #[test]
    fn dns_rebinding_request_is_rejected_before_routing() {
        let temp = tempfile::tempdir().unwrap();
        let service = cors_test_service().with_runtime_path(temp.path().to_path_buf());
        let body = r#"{"web_port":9888}"#;
        // 重绑定后浏览器认为同源：Origin 与 Host 都是攻击者域名，且对端是回环地址。
        for origin_header in ["Origin: http://evil.example:9666\r\n", ""] {
            let request = format!(
                "POST /api/settings HTTP/1.1\r\nHost: evil.example:9666\r\n{origin_header}Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let response = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
            assert!(response.starts_with("HTTP/1.1 403 Forbidden"), "{response}");
            assert!(response.contains("\"error_code\":\"host_not_allowed\""));
            assert!(!response.contains("Access-Control-Allow-"));
            assert!(!temp.path().join("settings.json").exists());
        }
        // 读接口同样拦截（重绑定页面同源，可读响应）
        let read = String::from_utf8(service.handle_http(
            b"GET /api/system/info HTTP/1.1\r\nHost: evil.example:9666\r\n\r\n",
        ))
        .unwrap();
        assert!(read.starts_with("HTTP/1.1 403"), "{read}");
        assert!(!read.contains("actual_port"));

        // 真实 TCP 入口：回环请求缺少 Host 同样拒绝；带 Host 或非回环的请求交给正常入口
        let missing = missing_host_rejection(b"POST /api/settings HTTP/1.0
Content-Length: 0

", true)
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .expect("missing Host must be rejected on loopback");
        assert!(missing.starts_with("HTTP/1.1 403") && missing.contains("host_not_allowed"), "{missing}");
        assert!(missing_host_rejection(b"GET / HTTP/1.1
Host: 127.0.0.1:9666

", true).is_none());
        assert!(missing_host_rejection(b"GET / HTTP/1.0

", false).is_none());

        // 非回环连接不做 Host 校验（仍由 Bearer 鉴权保护），局域网主机名访问公开页面不受影响。
        let lan = String::from_utf8(service.handle_http_from(
            b"GET /api/health HTTP/1.1\r\nHost: moss-pc.lan:9666\r\n\r\n",
            false,
            "192.0.2.20",
            None,
        ))
        .unwrap();
        assert!(lan.starts_with("HTTP/1.1 200 OK"), "{lan}");

        // 不带 Host 的本机非浏览器请求照常
        let no_host = String::from_utf8(service.handle_http(b"GET /api/health HTTP/1.1\r\n\r\n")).unwrap();
        assert!(no_host.starts_with("HTTP/1.1 200 OK"), "{no_host}");
    }

    #[test]
    fn disallowed_origin_get_is_served_without_cors_headers() {
        let service = cors_test_service();
        let response = String::from_utf8(service.handle_http(
            b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://evil.example\r\n\r\n",
        ))
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(!response.contains("Access-Control-Allow-"));
    }

    #[test]
    fn dev_origin_is_rejected_for_non_loopback_peer() {
        let service = cors_test_service();
        let request = b"POST /api/workflow HTTP/1.1\r\nHost: 192.0.2.10:9666\r\nOrigin: http://127.0.0.1:5177\r\nContent-Length: 2\r\n\r\n{}";
        let response = String::from_utf8(service.handle_http_from(request, false, "192.0.2.20", None)).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(!response.contains("Access-Control-Allow-"));
    }

    #[test]
    fn allowed_origin_is_echoed_on_every_response_kind() {
        let temp = tempfile::tempdir().unwrap();
        let generated = temp.path().join("generated");
        std::fs::create_dir_all(&generated).unwrap();
        std::fs::write(generated.join("probe.gcode"), "G0 X0\n").unwrap();
        let service = cors_test_service()
            .with_generated_root(generated.clone())
            .with_preview_roots(vec![generated.clone()])
            .with_allowed_origins(vec!["http://lab.example:8080".into()]);
        let origin = "http://tauri.localhost";
        let expect_cors = |response: &[u8], origin: &str| {
            let text = String::from_utf8_lossy(response).into_owned();
            let head = text.split("\r\n\r\n").next().unwrap().to_owned();
            assert!(head.starts_with("HTTP/1.1 "), "{head}");
            assert!(head.contains(&format!("\r\nAccess-Control-Allow-Origin: {origin}\r\n")), "{head}");
            assert!(head.contains("\r\nVary: Origin\r\n"), "{head}");
            assert_eq!(head.matches("Access-Control-Allow-Origin").count(), 1, "{head}");
            assert!(!head.contains("Allow-Origin: *"));
        };

        // JSON
        let json = service.handle_http(
            format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n").as_bytes(),
        );
        expect_cors(&json, origin);
        // 额外来源（MOSS_ALLOWED_ORIGINS）
        let extra = service.handle_http(
            b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://lab.example:8080\r\n\r\n",
        );
        expect_cors(&extra, "http://lab.example:8080");
        // 静态页面（同源）
        let page = service.handle_http(
            b"GET /draw HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: http://127.0.0.1:9666\r\n\r\n",
        );
        expect_cors(&page, "http://127.0.0.1:9666");
        // SSE
        let sse = service.handle_http(
            format!("GET /v1/events?workflow_id=wf-1 HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n").as_bytes(),
        );
        assert!(String::from_utf8_lossy(&sse).contains("text/event-stream"));
        expect_cors(&sse, origin);
        // 文件下载 / 预览（二进制响应）
        let path = generated.join("probe.gcode");
        let preview = service.handle_http(
            format!(
                "GET /preview?path={} HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n",
                percent_encode(path.to_string_lossy().as_ref())
            )
            .as_bytes(),
        );
        let preview_text = String::from_utf8_lossy(&preview).into_owned();
        assert!(preview_text.starts_with("HTTP/1.1 200 OK"), "{preview_text}");
        assert!(preview_text.ends_with("G0 X0\n"), "{preview_text}");
        expect_cors(&preview, origin);
        // OPTIONS
        let options = service.handle_http(
            format!("OPTIONS /api/workflow HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nOrigin: {origin}\r\n\r\n").as_bytes(),
        );
        expect_cors(&options, origin);
    }


    #[test]
    fn system_info_and_settings_routes_work() {
        let temp = tempfile::tempdir().unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 9666,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_runtime_path(temp.path().to_path_buf());

        service.set_actual_port(9666);
        let resp_info = String::from_utf8(
            service.handle_http(b"GET /api/system/info HTTP/1.1\r\nHost: localhost\r\n\r\n"),
        )
        .unwrap();
        assert!(resp_info.contains("\"actual_port\":9666"));

        let req_set = b"POST /api/settings HTTP/1.1\r\nHost: localhost\r\nContent-Length: 17\r\n\r\n{\"web_port\":9888}";
        let resp_set = String::from_utf8(service.handle_http(req_set)).unwrap();
        assert!(resp_set.contains("\"success\":true"));

        let saved = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        assert!(saved.contains("9888"));

        let endpoint = "wss://example.test/mcp/?token=secret";
        let body = format!(r#"{{"mcp_endpoint":"{endpoint}"}}"#);
        let request = format!(
            "POST /api/settings HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let resp_endpoint = String::from_utf8(service.handle_http(request.as_bytes())).unwrap();
        assert!(resp_endpoint.contains("\"success\":true"));
        let saved = std::fs::read_to_string(temp.path().join("settings.json")).unwrap();
        assert!(saved.contains(endpoint));

        let invalid = br#"POST /api/settings HTTP/1.1
Host: localhost
Content-Length: 30

{"mcp_endpoint":"http://invalid"}"#;
        let resp_invalid = String::from_utf8(service.handle_http(invalid)).unwrap();
        assert!(resp_invalid.contains("\"success\":false"));
    }

    #[test]
    fn http_preview_and_bootstrap_routes_enforce_contract() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("preview.gcode");
        std::fs::write(&file, b"G0 X0").unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_preview_roots(vec![temp.path().to_path_buf()]);
        let encoded = percent_encode(file.to_string_lossy().as_ref());
        let response = service.handle_http(
            format!("GET /preview?path={encoded} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
        );
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200 OK"));
        let bootstrap = service.handle("GET", "/v1/material-lab/bootstrap", b"");
        assert_eq!(bootstrap.body["result"]["version"], 1);
        assert_eq!(bootstrap.body["result"]["prototype"], false);
    }

    #[test]
    fn auth_pairing_expires_locks_and_stores_only_hashes() {
        let auth = AuthManager::new();
        let challenge = auth.issue_code(100).unwrap();
        let (token, pairing_id) = auth.pair("127.0.0.1", &challenge.code, 101).unwrap();
        assert!(pairing_id.starts_with("pair-"));
        assert!(auth.authorize(&token).is_ok());
        assert_eq!(
            auth.pair("127.0.0.1", &challenge.code, 101),
            Err(AuthError::Invalid)
        );

        let auth = AuthManager::new();
        let challenge = auth.issue_code(1000).unwrap();
        assert_eq!(
            auth.pair("10.0.0.2", "000000", 1001),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.pair("10.0.0.2", "000000", 1001),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.pair("10.0.0.2", "000000", 1001),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.pair("10.0.0.2", "000000", 1001),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.pair("10.0.0.2", "000000", 1001),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.pair("10.0.0.2", &challenge.code, 1001),
            Err(AuthError::Locked(900))
        );
        let new_challenge = auth.issue_code(2000).unwrap();
        assert_eq!(
            auth.pair("10.0.0.2", &new_challenge.code, 2001)
                .unwrap()
                .1
                .len(),
            17
        );

        let auth = AuthManager::new();
        let challenge = auth.issue_code(100).unwrap();
        assert_eq!(
            auth.pair("127.0.0.1", &challenge.code, 400),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn non_loopback_requests_require_bearer_and_pairing_returns_token() {
        let auth = std::sync::Arc::new(AuthManager::new());
        let challenge = auth.issue_code(unix_seconds()).unwrap();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "0.0.0.0".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_auth(auth.clone());
        let denied = service.handle_http_from(
            b"GET /api/ui/material-options HTTP/1.1\r\nHost: remote\r\n\r\n",
            false,
            "10.0.0.8",
            None,
        );
        assert!(String::from_utf8_lossy(&denied).starts_with("HTTP/1.1 401 Unauthorized"));

        let body = format!(r#"{{"pairing_code":"{}"}}"#, challenge.code);
        let request = format!(
            "POST /v1/auth/pair HTTP/1.1\r\nHost: remote\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let paired = service.handle_http_from(request.as_bytes(), false, "10.0.0.8", None);
        let paired_text = String::from_utf8_lossy(&paired);
        assert!(paired_text.starts_with("HTTP/1.1 200 OK"));
        let payload_start = paired
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let payload =
            serde_json::from_slice::<serde_json::Value>(&paired[payload_start..]).unwrap();
        assert!(payload["result"]["token"].as_str().is_some());
        assert!(payload["result"]["pairing_id"].as_str().is_some());
    }

    #[test]
    fn reference_data_writes_are_classified() {
        assert!(is_reference_data_write("POST", "/api/settings", b"{}"));
        assert!(is_reference_data_write("POST", "/api/jev/reveal", b""));
        assert!(is_reference_data_write("POST", "/api/material-lab/select-cell", b"{}"));
        assert!(is_reference_data_write("POST", "/api/material-lab/manage", br#"{"action":"save"}"#));
        assert!(is_reference_data_write("POST", "/api/material-lab/manage", br#"{"action":"import"}"#));
        assert!(is_reference_data_write("POST", "/api/material-lab/manage", br#"{"action":"delete"}"#));
        assert!(is_reference_data_write("POST", "/api/material-lab/manage", b"not json"));
        assert!(!is_reference_data_write("POST", "/api/material-lab/manage", br#"{"action":"export"}"#));
        for route in [
            "/api/text-task/generate",
            "/api/draw/preview",
            "/api/workflow",
            "/api/safe-action",
            "/api/device/framing",
            "/api/connection/check",
            "/api/material-lab/preview",
            "/api/material-lab/send",
            "/api/upload",
        ] {
            assert!(!is_reference_data_write("POST", route, b"{}"), "{route}");
        }
        assert!(!is_reference_data_write("GET", "/api/settings", b""));
    }

    #[test]
    fn paired_lan_client_cannot_change_materials_or_settings_but_loopback_can() {
        let auth = std::sync::Arc::new(AuthManager::new());
        let challenge = auth.issue_code(unix_seconds()).unwrap();
        let (token, _) = auth.pair("10.0.0.8", &challenge.code, unix_seconds()).unwrap();
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_auth(auth.clone());
        let post = |route: &str, body: &str, loopback: bool| {
            let request = format!(
                "POST {route} HTTP/1.1\r\nHost: 127.0.0.1:9666\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            String::from_utf8_lossy(&service.handle_http_from(request.as_bytes(), loopback, "10.0.0.8", None)).into_owned()
        };
        for (route, body) in [
            ("/api/material-lab/manage", r#"{"action":"save","material":"x"}"#),
            ("/api/material-lab/manage", r#"{"action":"delete","material":"x","confirmed":true}"#),
            ("/api/settings", r#"{"lan_access":true}"#),
            ("/api/jev/reveal", "{}"),
            ("/api/material-lab/select-cell", r#"{"calibration_id":"c","cell_number":1}"#),
        ] {
            let response = post(route, body, false);
            assert!(response.starts_with("HTTP/1.1 403"), "{route}: {response}");
            assert!(response.contains("lan_read_only"), "{route}: {response}");
        }
        // 导出与雕刻机操作不被只读规则拦截（是否成功由各自逻辑决定，这里只断言不是 lan_read_only）
        for (route, body) in [
            ("/api/material-lab/manage", r#"{"action":"export"}"#),
            ("/api/material-lab/preview", "{}"),
        ] {
            let response = post(route, body, false);
            assert!(!response.contains("lan_read_only"), "{route}: {response}");
        }
        // 电脑本机不受影响
        let response = post("/api/settings", r#"{"lan_access":"yes"}"#, true);
        assert!(!response.contains("lan_read_only"), "{response}");
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }

    #[test]
    fn lan_info_is_loopback_only() {
        let auth = std::sync::Arc::new(AuthManager::new());
        let challenge = auth.issue_code(unix_seconds()).unwrap();
        let (token, _) = auth.pair("10.0.0.8", &challenge.code, unix_seconds()).unwrap();
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_auth(auth.clone());
        let request = format!("GET /api/system/lan-info HTTP/1.1\r\nHost: 10.0.0.2:9666\r\nAuthorization: Bearer {token}\r\n\r\n");
        let remote = String::from_utf8_lossy(&service.handle_http_from(request.as_bytes(), false, "10.0.0.8", None)).into_owned();
        assert!(remote.starts_with("HTTP/1.1 403"), "{remote}");
        let local = String::from_utf8_lossy(&service.handle_http_from(
            b"GET /api/system/lan-info HTTP/1.1\r\nHost: 127.0.0.1:9666\r\n\r\n",
            true,
            "127.0.0.1",
            None,
        ))
        .into_owned();
        assert!(local.starts_with("HTTP/1.1 200"), "{local}");
        assert!(local.contains("\"lan_access_active\":false"), "{local}");
    }

    fn mask_golden_dynamic_fields(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                for (key, child) in object.iter_mut() {
                    if matches!(
                        key.as_str(),
                        "workflow_id"
                            | "task_id"
                            | "calibration_id"
                            | "job_id"
                            | "created_at"
                            | "updated_at"
                            | "source_path"
                            | "backup_path"
                            | "params_file"
                            | "content_fingerprint"
                    ) {
                        *child = serde_json::Value::String("<dynamic>".into());
                    } else {
                        mask_golden_dynamic_fields(child);
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    mask_golden_dynamic_fields(item);
                }
            }
            _ => {}
        }
    }

    fn normalize_golden_numbers(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Number(number) => {
                if let Some(float) = number.as_f64() {
                    if float.is_finite() && float.fract() == 0.0 {
                        *value = serde_json::json!(float as i64);
                    }
                }
            }
            serde_json::Value::Object(object) => {
                for child in object.values_mut() {
                    normalize_golden_numbers(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    normalize_golden_numbers(item);
                }
            }
            _ => {}
        }
    }

    fn mask_default_power_scales(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("power_scale") == Some(&serde_json::json!(1000)) {
                    object.remove("power_scale");
                }
                for child in object.values_mut() {
                    mask_default_power_scales(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    mask_default_power_scales(item);
                }
            }
            _ => {}
        }
    }

    fn assert_golden_api_response(raw: &[u8], file_name: &str) {
        let separator = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response must contain a header separator");
        let headers = String::from_utf8_lossy(&raw[..separator]);
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("HTTP response must contain a status code")
            .parse::<u16>()
            .expect("HTTP status code must be numeric");
        let mut actual = serde_json::json!({
            "status": status,
            "body": serde_json::from_slice::<serde_json::Value>(&raw[separator + 4..])
                .expect("HTTP response body must be valid JSON"),
        });
        let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/api")
            .join(file_name);
        let mut expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&golden_path).expect("golden response must exist"))
                .expect("golden response must be valid JSON");
        mask_golden_dynamic_fields(&mut actual);
        mask_golden_dynamic_fields(&mut expected);
        mask_default_power_scales(&mut actual);
        mask_default_power_scales(&mut expected);
        normalize_golden_numbers(&mut actual);
        normalize_golden_numbers(&mut expected);
        assert_eq!(actual, expected, "API response differs from {}", golden_path.display());
    }

    fn assert_golden_bootstrap_response(raw: &[u8], file_name: &str) {
        let separator = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP response must contain a header separator");
        let headers = String::from_utf8_lossy(&raw[..separator]);
        let status = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("HTTP response must contain a status code")
            .parse::<u16>()
            .expect("HTTP status code must be numeric");
        let body = serde_json::from_slice::<serde_json::Value>(&raw[separator + 4..])
            .expect("HTTP response body must be valid JSON");
        let actual_result = body
            .get("result")
            .cloned()
            .expect("bootstrap response must contain result");
        let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/api")
            .join(file_name);
        let mut expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&golden_path).expect("golden response must exist"))
                .expect("golden response must be valid JSON");
        if let Some(materials) = expected.get_mut("materials") {
            if materials.get("version").is_none() {
                let legacy_materials = materials.take();
                *materials = serde_json::json!({
                    "version": 1,
                    "materials": legacy_materials,
                    "power_scale": 1000
                });
            }
        }
        let mut actual = serde_json::json!({
            "status": status,
            "result": actual_result,
        });
        mask_golden_dynamic_fields(&mut actual);
        mask_golden_dynamic_fields(&mut expected);
        mask_default_power_scales(&mut actual);
        mask_default_power_scales(&mut expected);
        normalize_golden_numbers(&mut actual);
        normalize_golden_numbers(&mut expected);
        assert_eq!(actual["status"], 200);
        assert_eq!(
            actual["result"], expected,
            "bootstrap response differs from {}",
            golden_path.display()
        );
    }

    #[test]
    fn golden_api_snapshot_parity_test() {
        let temp = tempfile::tempdir().unwrap();
        let upload_dir = temp.path().join("uploads");
        let generated_dir = temp.path().join("generated");
        std::fs::create_dir_all(&upload_dir).unwrap();
        std::fs::create_dir_all(&generated_dir).unwrap();

        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_upload_root(upload_dir)
        .with_generated_root(generated_dir.clone())
        .with_preview_roots(vec![generated_dir])
        // 黄金样本描述“本机没有串口”的基线；真实串口枚举随插入的设备变化，不能进快照。
        .with_serial_port_lister(std::sync::Arc::new(Vec::<String>::new));

        // 1. health
        let res = service.handle_http(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_api_response(&res, "health.response.json");

        // 2. runtime-config
        let res = service.handle_http(b"GET /api/ui/runtime-config HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_api_response(&res, "runtime-config.response.json");

        // 3. serial-ports
        let res = service.handle_http(b"GET /api/ui/serial-ports HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_api_response(&res, "serial-ports.response.json");

        // 4. material-options
        let res = service.handle_http(b"GET /api/ui/material-options HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_api_response(&res, "material-options.response.json");

        // 5. material-recommendation
        let res = service.handle_http(b"GET /api/ui/material-recommendation?material=%E7%BA%B8%E6%9D%BF&thickness_mm=3&laser_mode=engrave&engraving_mode=outline HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_api_response(&res, "material-recommendation.response.json");

        // 6. preview-missing
        let res = service.handle_http(b"GET /preview HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_api_response(&res, "preview-missing.response.json");

        // 7. material-lab-export
        let res = service.handle_http(b"GET /api/material-lab/export HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_api_response(&res, "material-lab-export.response.json");

        // 8. upload (empty payload)
        let res = service.handle_http(b"POST /api/upload HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}");
        assert_golden_api_response(&res, "upload.response.json");

        // 9. draw-preview (empty payload)
        let res = service.handle_http(b"POST /api/draw/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}");
        assert_golden_api_response(&res, "draw-preview.response.json");

        // 10. material-preview (empty payload)
        let res = service.handle_http(b"POST /api/material-lab/preview HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}");
        assert_golden_api_response(&res, "material-preview.response.json");

        // 11. material-manage ({"action":"import", "library_json":{}})
        let b = r#"{"action":"import","library_json":{}}"#;
        let res = service.handle_http(format!("POST /api/material-lab/manage HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
        assert_golden_api_response(&res, "material-manage.response.json");

        // 12. material-send
        let b = r#"{"calibration_id":"","confirmed":false}"#;
        let res = service.handle_http(format!("POST /api/material-lab/send HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
        assert_golden_api_response(&res, "material-send.response.json");

        // 13. material-select-cell
        let res = service.handle_http(b"POST /api/material-lab/select-cell HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}");
        assert_golden_api_response(&res, "material-select-cell.response.json");

        // 14. connection-check
        let b = r#"{"connection_mode":"network","network_host":""}"#;
        let res = service.handle_http(format!("POST /api/connection/check HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
        assert_golden_api_response(&res, "connection-check.response.json");

        // 15. text-generate (empty)
        let res = service.handle_http(b"POST /api/text-task/generate HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}");
        assert_golden_api_response(&res, "text-generate.response.json");

        // 16. text-send (empty)
        let b = r#"{"workflow_id":"","confirmed":false}"#;
        let res = service.handle_http(format!("POST /api/text-task/send HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
        assert_golden_api_response(&res, "text-send.response.json");

        // 17. workflow (status empty id)
        let b = r#"{"action":"status","workflow_id":""}"#;
        let res = service.handle_http(format!("POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}", b.len(), b).as_bytes());
        assert_golden_api_response(&res, "workflow.response.json");

        // 18. bootstrap
        let res = service.handle_http(b"GET /v1/material-lab/bootstrap HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert_golden_bootstrap_response(&res, "get-v1-material-lab-bootstrap.json");
    }

    #[test]
    fn unknown_stop_state_keeps_exact_transport_for_retry() {
        let fake = laser_device::FakeGrbl::new(Vec::<String>::new());
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_transport_factory(std::sync::Arc::new(|_| panic!("retry must not open another device")));
        service.uncertain_jobs.lock().unwrap().insert("lost-job".into(), std::sync::Arc::new(ActiveJob {
            workflow_id: "lost-job".into(), mode: "network", target: "fake-a".into(),
            cancel: Default::default(), progress: Default::default(), transport: std::sync::Arc::new(fake.clone()),
            started_at: std::time::Instant::now(),
        }));
        assert!(service.active_jobs.lock().unwrap().is_empty());
        let unknown = post_json(&service,"/api/safe-action",&serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"other"}));
        assert_eq!(unknown["success"], false);
        assert!(fake.realtime().is_empty());
        let result = post_json(&service,"/api/safe-action",&serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"lost-job"}));
        assert_eq!(result["success"], true, "{result}");
        assert_eq!(fake.realtime(), vec![0x18]);
        let res = service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let text = String::from_utf8_lossy(&res);
        assert!(text.contains("lost-job") && text.contains("unknown"), "{text}");
    }

    #[test]
    fn emergency_stop_targets_only_requested_job_and_dry_run_never_writes() {
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_transport_factory(std::sync::Arc::new(|_| panic!("must reuse exact job transport")));
        let devices: Vec<_> = (0..2).map(|_| laser_device::FakeGrbl::new(Vec::<String>::new())).collect();
        for (i, device) in devices.iter().enumerate() {
            service.active_jobs.lock().unwrap().insert(format!("job-{i}"),std::sync::Arc::new(ActiveJob {
                workflow_id:format!("job-{i}"), mode:"network", target:format!("127.0.0.1:{}",2300+i),
                cancel:Default::default(), progress:Default::default(), transport:std::sync::Arc::new(device.clone()), started_at:std::time::Instant::now(),
            }));
        }
        for args in [
            serde_json::json!({"action":"emergency_stop","confirmed":true}),
            serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"unknown"}),
            serde_json::json!({"action":"power_plus_10","job_id":"job-0"}),
            serde_json::json!({"action":"emergency_stop","confirmed":true,"job_id":"job-0","host":"127.0.0.1","telnet_port":2301}),
        ] { assert_eq!(post_json(&service,"/api/safe-action",&args)["success"],false); }
        let dry = post_json(&service,"/api/safe-action",&serde_json::json!({"action":"emergency_stop","job_id":"job-1","dry_run":true}));
        assert_eq!(dry["success"],true);
        assert!(devices.iter().all(|d| d.realtime().is_empty()));
        let result = post_json(&service,"/api/safe-action",&serde_json::json!({"action":"emergency_stop","job_id":"job-1","confirmed":true}));
        assert_eq!(result["success"],true);
        assert!(devices[0].realtime().is_empty());
        assert_eq!(devices[1].realtime(),vec![0x18]);
    }

    #[test]
    fn stop_targets_exposes_read_only_progress_for_the_global_panel() {
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_transport_factory(std::sync::Arc::new(|_| panic!("只读查询不得打开设备连接")));
        let device = laser_device::FakeGrbl::new(Vec::<String>::new());
        let progress = std::sync::Arc::new(laser_device::JobProgress::default());
        progress.total_lines.store(100, std::sync::atomic::Ordering::Relaxed);
        progress.lines_sent.store(37, std::sync::atomic::Ordering::Relaxed);
        service.active_jobs.lock().unwrap().insert("job-9".into(), std::sync::Arc::new(ActiveJob {
            workflow_id: "wf-9".into(), mode: "network", target: "127.0.0.1:2301".into(),
            cancel: Default::default(), progress: progress.clone(),
            transport: std::sync::Arc::new(device.clone()), started_at: std::time::Instant::now(),
        }));
        let res = service.handle_http(b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let text = String::from_utf8_lossy(&res);
        assert!(text.contains("\"workflow_id\":\"wf-9\""), "浮层需要 workflow_id 才能取消或续雕: {text}");
        assert!(text.contains("\"lines_sent\":37") && text.contains("\"total_lines\":100"), "浮层需要真实进度: {text}");
        assert!(text.contains("\"phase\":\"streaming\"") && text.contains("\"state\":\"running\""), "{text}");
        assert!(text.contains("\"elapsed_ms\"") && text.contains("\"mode\":\"network\""), "{text}");
        assert!(device.realtime().is_empty(), "只读查询不得写实时通道");
    }

    #[test]
    fn safe_action_route_overrides_running_job_without_reopening_transport() {
        let fake = laser_device::FakeGrbl::new(vec!["ok".into()]);
        let fake_clone = fake.clone();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake_clone.clone()))));

        // 1. 模拟正在加工任务（注册进 active_jobs）
        let cancel = laser_device::CancellationToken::default();
        let progress = std::sync::Arc::new(laser_device::JobProgress::default());
        progress.total_lines.store(100, std::sync::atomic::Ordering::Relaxed);
        let active_job = std::sync::Arc::new(ActiveJob {
            workflow_id: "test-job-1".into(),
            started_at: std::time::Instant::now(),
            target: "127.0.0.1:8080".into(),
            mode: "network",
            cancel: cancel.clone(),
            progress: progress.clone(),
            transport: std::sync::Arc::new(fake.clone()),
        });
        service.active_jobs.lock().unwrap().insert("test-job-1".into(), active_job);

        // 2. 边刻边调：下发进给速度增加 10%
        let feed_res = post_json(&service, "/api/safe-action", &serde_json::json!({
            "action": "feed_plus_10",
        }));
        assert_eq!(feed_res["success"], true, "{feed_res}");
        assert!(feed_res["result"].as_str().unwrap().contains("不停机"));
        assert!(fake.realtime().contains(&0x91));

        // 3. 边刻边调：下发功率降低 10%
        let power_res = post_json(&service, "/api/safe-action", &serde_json::json!({
            "action": "power_minus_10",
        }));
        assert_eq!(power_res["success"], true, "{power_res}");
        assert!(power_res["result"].as_str().unwrap().contains("不停机"));
        assert!(fake.realtime().contains(&0x9B));

        // 4. 正在加工时下发移动指令应当被安全拦截
        let move_res = post_json(&service, "/api/safe-action", &serde_json::json!({
            "action": "relative_move",
            "x": 10.0,
            "y": 0.0,
            "confirmed": true,
        }));
        assert_eq!(move_res["success"], false);
        assert!(move_res["result"].as_str().unwrap().contains("禁止下发"));

        // 5. 正在加工时下发急停：任务被取消且收到 0x18
        let stop_res = post_json(&service, "/api/safe-action", &serde_json::json!({
            "action": "emergency_stop",
        }));
        assert_eq!(stop_res["success"], true);
        assert!(cancel.is_cancelled());
        assert!(fake.realtime().contains(&0x18));
    }

    /// 加工中查询状态：走原作业通道写实时状态字节，并直接回报已下发行数（2026-11-02 补测）。
    #[test]
    fn status_during_active_job_reports_progress_without_ending_the_job() {
        use std::sync::atomic::Ordering;
        let fake = laser_device::FakeGrbl::new(vec!["ok".into()]);
        let fake_clone = fake.clone();
        let service = ApiService::new(
            laser_runtime::LaserSettings {
                default_connection_mode: "network".into(),
                engraving_mode: "raster".into(),
                web_host: "127.0.0.1".into(),
                web_port: 8766,
                material_params_file: None,
            },
            laser_contracts::builtin_material_library(),
        )
        .with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake_clone.clone()))));
        let progress = std::sync::Arc::new(laser_device::JobProgress::default());
        progress.total_lines.store(342, Ordering::Relaxed);
        progress.lines_sent.store(120, Ordering::Relaxed);
        let job = std::sync::Arc::new(ActiveJob {
            workflow_id: "wf-status".into(),
            started_at: std::time::Instant::now(),
            target: "127.0.0.1:8080".into(),
            mode: "network",
            cancel: laser_device::CancellationToken::default(),
            progress: progress.clone(),
            transport: std::sync::Arc::new(fake.clone()),
        });
        service
            .active_jobs
            .lock()
            .unwrap()
            .insert("wf-status".into(), job);

        let res = post_json(&service, "/api/safe-action", &serde_json::json!({"action": "status"}));
        assert_eq!(res["success"], true, "{res}");
        let text = res["result"].as_str().unwrap_or_default();
        assert!(
            text.contains("设备正在加工中") && text.contains("120/342"),
            "加工中状态必须回报真实行数: {text}"
        );
        assert_eq!(res["detail"], "active_job_streaming");
        assert!(
            fake.realtime().contains(&0x3F),
            "加工中的状态查询必须经原作业通道写实时状态字节"
        );
        assert_eq!(service.active_job_count(), 1, "查询状态不能移除在途作业");
    }

    #[test]
    fn job_worker_outcome_translates_device_errors_to_chinese_diagnostics() {
        let diag = laser_device::diagnose_device_message("error:20");
        assert!(diag.is_some());
        assert!(diag.unwrap().contains("不支持或无效的 G-code 指令"));

        let diag_alarm = laser_device::diagnose_device_message("ALARM:1");
        assert!(diag_alarm.is_some());
        assert!(diag_alarm.unwrap().contains("硬限位开关触发"));
    }

    #[test]
    fn workflow_resume_action_safely_resumes_remaining_code() {
        let temp = tempfile::tempdir().unwrap();
        let gen_dir = temp.path().join("generated");
        std::fs::create_dir_all(&gen_dir).unwrap();

        // 写入一个 5 行 G-code
        let gcode_content = "G21\nG90\nG0 X0 Y0\nG1 X10 Y10 F1000\nM5\n";
        let gcode_path = gen_dir.join("resume_test.gcode");
        std::fs::write(&gcode_path, gcode_content).unwrap();
        let expected_sha = hex::encode(sha2::Sha256::digest(gcode_content.as_bytes()));

        let written_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_generated_root(gen_dir.clone())
            .with_preview_roots(vec![gen_dir.clone()])
            .with_transport_factory(recording_transport_factory(100, written_lines.clone()));

        // 1. 构造一个已取消且断点为 2/5 的 workflow 记录
        let wf_id = "test-resume-wf-001";
        let record = serde_json::json!({
            "workflow_id": wf_id,
            "status": "cancelled",
            "artifacts": {
                "gcode_file": "resume_test.gcode",
                "gcode_sha256": expected_sha,
            },
            "confirmation_snapshot": {
                "gcode_file": "resume_test.gcode",
                "gcode_sha256": expected_sha,
            },
            "send_result": {
                "success": false,
                "lines_sent": 2,
                "total_lines": 5,
                "can_resume": true
            }
        });
        service.workflows.lock().unwrap().insert(wf_id.into(), record);

        // 2. 调用 resume action
        let resume_req = serde_json::json!({
            "action": "resume",
            "workflow_id": wf_id,
            "confirmed": true,
            "connection_mode": "network",
            "network_host": "127.0.0.1",
            "network_telnet_port": 23
        });
        let res = post_json(&service, "/api/workflow", &resume_req);
        assert_eq!(res["success"], true, "{res}");
        assert_eq!(res["result"]["status"], "sending");
        assert_eq!(res["result"]["send_result"]["lines_sent"], 2);
        assert_eq!(res["result"]["send_result"]["total_lines"], 5);

        // 3. 等待后台续雕线程完成，并验证下发流中包含 $X 解锁命令以及剩余行
        let finished = wait_for_status(&service, wf_id, "completed");
        assert_eq!(finished["result"]["status"], "completed", "{finished}");
        let lines = written_lines.lock().unwrap().clone();
        assert!(lines.contains(&"$X".to_string()), "恢复首个命令必须下发 $X 解除锁定: {lines:?}");
        assert_eq!(finished["result"]["send_result"]["lines_sent"], 5);
        assert_eq!(finished["result"]["send_result"]["total_lines"], 5);

        // 4. 反向边界：若任务已完成，必须 400 拦截
        let invalid_wf = "test-invalid-resume";
        service.workflows.lock().unwrap().insert(invalid_wf.into(), serde_json::json!({
            "workflow_id": invalid_wf,
            "status": "completed",
            "send_result": { "lines_sent": 5, "total_lines": 5 }
        }));
        let bad_res = post_json(&service, "/api/workflow", &serde_json::json!({
            "action": "resume", "workflow_id": invalid_wf, "confirmed": true
        }));
        assert_eq!(bad_res["success"], false);
        assert_eq!(bad_res["detail"]["error_code"], "workflow_completed");
    }

    fn percent_encode(value: &str) -> String {
        value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || b"/._-:".contains(&byte) {
                    (byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect()
    }

    // ----- 严格模式（技术书 §6.2，实施清单 P1-04，验收 A-24..A-27、A-40） -----

    struct StrictFixture {
        service: ApiService,
        _temp: tempfile::TempDir,
        written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        targets: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        db: Option<std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>>,
    }

    fn strict_fixture(strict: bool, with_approvals: bool) -> StrictFixture {
        let temp = tempfile::tempdir().unwrap();
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let targets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let base = recording_transport_factory(200_000, written.clone());
        let targets_for_factory = targets.clone();
        let factory: TransportFactory = std::sync::Arc::new(move |spec: &laser_device::ConnectionSpec| {
            targets_for_factory.lock().unwrap().push(spec.describe());
            base(spec)
        });
        let mut service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_generated_root(temp.path().to_path_buf())
            .with_transport_factory(factory)
            .with_store(std::sync::Arc::new(laser_runtime::MemoryStore::new()))
            .with_strict_mode(strict);
        let mut approvals_db = None;
        if with_approvals {
            let db = std::sync::Arc::new(std::sync::Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap()));
            approvals_db = Some(db.clone());
            service = service.with_runtime_db(db.clone())
                .with_approval_service(std::sync::Arc::new(approval::ApprovalService::new(db.clone()).with_ttl_seconds(120)));
            {
                let db = db.lock().unwrap();
                db.voice_device_seen("aa:bb:cc:dd:ee:ff", "test-client", "", "", "", 0.0).unwrap();
                db.set_voice_device_status("aa:bb:cc:dd:ee:ff", laser_runtime::VOICE_DEVICE_ALLOWED, 0.0).unwrap();
            }
            service.register_voice_session("aa:bb:cc:dd:ee:ff", "test-client", "sess-1").unwrap();
        }
        StrictFixture { service, _temp: temp, written, targets, db: approvals_db }
    }

    fn preview_workflow(service: &ApiService) -> String {
        let preview = post_json(service, "/api/text-task/generate", &serde_json::json!({
            "text": "STRICT", "material": "椴木", "thickness_mm": 3.0, "font_size_mm": 12
        }));
        assert_eq!(preview["success"], true, "{preview}");
        preview["result"]["workflow_id"].as_str().unwrap().to_owned()
    }

    fn post_with_ctx(
        service: &ApiService,
        route: &str,
        body: &serde_json::Value,
        ctx: laser_contracts::CallerContext,
    ) -> (u16, serde_json::Value) {
        let body = body.to_string();
        let request = format!(
            "POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let raw = service.handle_http_with_ctx(request.as_bytes(), ctx);
        let status = String::from_utf8_lossy(&raw[9..12]).parse::<u16>().unwrap();
        (status, parse_json_response(&raw))
    }

    fn confirm_send_body(workflow_id: &str, host: &str) -> serde_json::Value {
        serde_json::json!({
            "action": "confirm_send", "workflow_id": workflow_id, "confirmed": true,
            "connection_mode": "network", "network_host": host, "network_telnet_port": 23
        })
    }

    /// A-24 / A-26：严格模式下本机回环（无桌面会话）的发送、走边框、回原点一律拒绝，且不连接设备。
    #[test]
    fn strict_mode_blocks_loopback_r3_without_touching_device() {
        let fixture = strict_fixture(true, true);
        let workflow_id = preview_workflow(&fixture.service);
        let send = post_json(&fixture.service, "/api/workflow", &confirm_send_body(&workflow_id, "127.0.0.1"));
        assert_eq!(send["detail"]["error_code"], "strict_mode_blocked", "{send}");
        let framing = post_json(&fixture.service, "/api/device/framing", &serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
        }));
        assert_eq!(framing["detail"]["error_code"], "strict_mode_blocked", "{framing}");
        let home = post_json(&fixture.service, "/api/safe-action", &serde_json::json!({
            "action": "home", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"
        }));
        assert_eq!(home["detail"]["error_code"], "strict_mode_blocked", "{home}");
        assert!(fixture.targets.lock().unwrap().is_empty(), "未授权的 R3 不得连接设备");
        assert!(fixture.written.lock().unwrap().is_empty());
        let status = post_json(&fixture.service, "/api/workflow", &serde_json::json!({"action": "status", "workflow_id": workflow_id}));
        assert_eq!(status["result"]["status"], "preview_ready", "{status}");
        assert!(fixture.service.approvals().unwrap().list(None, 10).unwrap().is_empty(), "回环不创建审批");
    }

    /// A-27：急停、软复位、降低倍率、查询状态与取消在严格模式下照常执行。
    #[test]
    fn strict_mode_never_blocks_stop_actions() {
        let fixture = strict_fixture(true, true);
        for action in ["emergency_stop", "soft_reset", "status", "feed_override_minus_10", "power_reset"] {
            let (status, response) = post_with_ctx(
                &fixture.service,
                "/api/safe-action",
                &serde_json::json!({"action": action, "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
                laser_contracts::CallerContext::McpBridge,
            );
            assert_ne!(status, 202, "{action}: {response}");
            assert_ne!(status, 403, "{action}: {response}");
        }
        assert!(!fixture.targets.lock().unwrap().is_empty(), "停止类动作照常连接设备");
        let workflow_id = preview_workflow(&fixture.service);
        let (status, cancel) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &serde_json::json!({"action": "cancel", "workflow_id": workflow_id}),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(status, 200, "{cancel}");
        assert!(fixture.service.approvals().unwrap().list(None, 10).unwrap().is_empty());
    }

    fn voice_ctx() -> laser_contracts::CallerContext {
        laser_contracts::CallerContext::Voice { device_id: "aa:bb:cc:dd:ee:ff".into(), session_id: "sess-1".into() }
    }

    #[test]
    fn active_job_stop_write_failure_is_not_reported_as_success() {
        struct FailRealtime;
        impl laser_device::DeviceTransport for FailRealtime {
            fn write_line(&self, _: &str) -> Result<(), String> { panic!("stop must bypass G-code") }
            fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
                assert_eq!(command, laser_device::RealtimeCommand::EmergencyStop);
                Err("injected realtime failure".into())
            }
            fn read_line(&self) -> Result<Option<String>, String> { Ok(None) }
        }
        let fixture = strict_fixture(true, true);
        let cancel = laser_device::CancellationToken::default();
        fixture.service.active_jobs.lock().unwrap().insert("wf-fail-stop".into(), std::sync::Arc::new(ActiveJob {
            workflow_id:"wf-fail-stop".into(),mode:"stream",target:"fake".into(),cancel:cancel.clone(),
            progress:std::sync::Arc::new(laser_device::JobProgress::default()),transport:std::sync::Arc::new(FailRealtime),started_at:std::time::Instant::now(),
        }));
        let args = serde_json::from_value(serde_json::json!({"action":"emergency_stop","confirmed":true})).unwrap();
        let result = fixture.service.handle_safe_action(&args, &voice_ctx());
        assert!(!result.success && result.result.contains("0x18 发送失败"), "{result:?}");
        assert!(cancel.is_cancelled());
    }

    /// 内置语音（NV4，A-20 / A-25 / A-26 在内置链路复测）：严格模式开启时，语音发起的发送与设备动作只生成审批，
    /// 审批来源为 voice、记录发起设备，审计记为 `voice:<设备>`；批准后以审批身份执行。
    #[test]
    fn voice_r3_needs_desktop_approval_in_strict_mode() {
        let fixture = strict_fixture(true, true);
        let workflow_id = preview_workflow(&fixture.service);
        let mut request = confirm_send_body(&workflow_id, "127.0.0.1");
        request["confirmed"] = serde_json::json!(true);
        let (status, pending) = post_with_ctx(&fixture.service, "/api/workflow", &request, voice_ctx());
        assert_eq!(status, 202, "{pending}");
        assert_eq!(pending["result"]["status"], "approval_required");
        assert!(fixture.targets.lock().unwrap().is_empty() && fixture.written.lock().unwrap().is_empty(), "未批准不连接设备");
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();
        let approvals = fixture.service.approvals().unwrap();
        let record = approvals.get(&approval_id).unwrap();
        assert_eq!(record.source, laser_contracts::ApprovalSource::Voice);
        assert_eq!(record.session_id.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        let audit = fixture.db.as_ref().unwrap().lock().unwrap().recent_audit(20).unwrap();
        assert!(audit.iter().any(|entry| entry.client == "voice:aa:bb:cc:dd:ee:ff"), "{audit:?}");

        let (status, origin) = post_with_ctx(
            &fixture.service,
            "/api/safe-action",
            &serde_json::json!({"action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
            voice_ctx(),
        );
        assert_eq!(status, 202, "设定原点（R3）同样需要审批：{origin}");
        assert!(fixture.targets.lock().unwrap().is_empty());

        approvals.approve(&approval_id, &record.digest, &record.digest).unwrap();
        let approval_ctx = laser_contracts::CallerContext::Approval { approval_id: approval_id.clone() };
        let (status, sent) = post_with_ctx(&fixture.service, "/api/workflow", &request, approval_ctx);
        assert_eq!(status, 200, "{sent}");
        assert_eq!(sent["result"]["status"], "sending");
    }

    /// 严格模式关闭时（2026-10-07 用户决定）：内置语音与官方接入点相同，模型带 confirmed=true 即直接发送，不生成审批。
    #[test]
    fn voice_follows_strict_mode_switch_when_off() {
        let fixture = strict_fixture(false, true);
        let workflow_id = preview_workflow(&fixture.service);
        let mut request = confirm_send_body(&workflow_id, "127.0.0.1");
        request["confirmed"] = serde_json::json!(true);
        let (status, sent) = post_with_ctx(&fixture.service, "/api/workflow", &request, voice_ctx());
        assert_eq!(status, 200, "{sent}");
        assert_eq!(sent["result"]["status"], "sending");
        assert!(fixture.service.approvals().unwrap().list(None, 10).unwrap().is_empty(), "严格模式关闭不生成审批");
        let finished = wait_for_status(&fixture.service, &workflow_id, "completed");
        assert_eq!(finished["result"]["status"], "completed", "{finished}");
    }

    /// A-27（内置链路）：停止类动作与取消不受影响，不生成审批。
    #[test]
    fn revoke_during_preflight_prevents_queued_gcode_from_starting() {
        let fixture = strict_fixture(false, true);
        let slot = std::sync::Arc::new(std::sync::OnceLock::<std::sync::Weak<ApiService>>::new());
        let owner = slot.clone();
        let written = fixture.written.clone();
        let factory = recording_transport_factory(100, written.clone());
        let service = std::sync::Arc::new(fixture.service.with_transport_factory(std::sync::Arc::new(move |spec| {
            let api = owner.get().unwrap().upgrade().unwrap();
            api.set_voice_device_status("aa:bb:cc:dd:ee:ff", laser_runtime::VOICE_DEVICE_REVOKED, 0.0).unwrap();
            factory(spec)
        })));
        slot.set(std::sync::Arc::downgrade(&service)).unwrap();
        let id = preview_workflow(&service);
        let (status, result) = post_with_ctx(&service, "/api/workflow", &confirm_send_body(&id,"127.0.0.1"), voice_ctx());
        assert_eq!(status,403,"{result}");
        assert!(service.active_jobs.lock().unwrap().is_empty());
        assert!(written.lock().unwrap().iter().all(|line| line == "$$" || line == "$CD" || line.starts_with("$RI=")), "revoked request must not send machining lines");
    }

    #[test]
    fn revoke_during_connect_prevents_safe_motion_and_framing() {
        for (route, status, body) in [
            ("/api/safe-action",400,serde_json::json!({"action":"home","confirmed":true,"connection_mode":"network","host":"127.0.0.1"})),
            ("/api/device/framing",403,serde_json::json!({"confirmed":true,"connection_mode":"network","host":"127.0.0.1","max_x":10,"max_y":10})),
        ] {
            let fixture = strict_fixture(false,true);
            let slot = std::sync::Arc::new(std::sync::OnceLock::<std::sync::Weak<ApiService>>::new());
            let owner = slot.clone();
            let written = fixture.written.clone();
            let factory = recording_transport_factory(100,written.clone());
            let service = std::sync::Arc::new(fixture.service.with_transport_factory(std::sync::Arc::new(move |spec| {
                owner.get().unwrap().upgrade().unwrap().set_voice_device_status("aa:bb:cc:dd:ee:ff",laser_runtime::VOICE_DEVICE_REVOKED,0.0).unwrap();
                factory(spec)
            })));
            slot.set(std::sync::Arc::downgrade(&service)).unwrap();
            let (code,result) = post_with_ctx(&service,route,&body,voice_ctx());
            assert_eq!(code,status,"{route}: {result}");
            assert_eq!(result["success"],false);
            assert!(written.lock().unwrap().iter().all(|line| line == "$$" || line == "$CD" || line.starts_with("$RI=")),"no motion after revoke");
        }
    }

    #[test]
    fn voice_stop_without_active_job_writes_reset_and_reports_connection_failure() {
        for strict in [false, true] {
            let fixture = strict_fixture(strict, true);
            let fake = laser_device::FakeGrbl::new(Vec::<String>::new());
            let observed = fake.clone();
            let service = fixture.service.with_transport_factory(std::sync::Arc::new(move |_| Ok(Box::new(fake.clone()))));
            assert!(service.active_jobs.lock().unwrap().is_empty());
            let body = serde_json::json!({"action":"emergency_stop","confirmed":true,"connection_mode":"network","host":"127.0.0.1"});
            let (status, result) = post_with_ctx(&service, "/api/safe-action", &body, voice_ctx());
            assert_eq!(status, 200, "{result}");
            assert_eq!(observed.realtime(), vec![0x18]);
            assert!(observed.lines().is_empty(), "reset must not wait behind handshake/G-code");
            let service = service.with_transport_factory(std::sync::Arc::new(|_| Err("fake disconnected".into())));
            let (status, result) = post_with_ctx(&service, "/api/safe-action", &body, voice_ctx());
            assert_eq!(status, 400);
            assert_eq!(result["success"], false);
        }
    }

    #[test]
    fn voice_stop_actions_run_immediately() {
        let fixture = strict_fixture(true, true);
        for action in ["emergency_stop", "soft_reset", "status"] {
            let (status, response) = post_with_ctx(
                &fixture.service,
                "/api/safe-action",
                &serde_json::json!({"action": action, "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
                voice_ctx(),
            );
            assert_ne!(status, 202, "{action}: {response}");
            assert_ne!(status, 403, "{action}: {response}");
        }
        assert!(!fixture.targets.lock().unwrap().is_empty(), "停止类动作照常连接设备");
        let workflow_id = preview_workflow(&fixture.service);
        let (status, cancel) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &serde_json::json!({"action": "cancel", "workflow_id": workflow_id}),
            voice_ctx(),
        );
        assert_eq!(status, 200, "{cancel}");
        assert!(fixture.service.approvals().unwrap().list(None, 10).unwrap().is_empty());
    }

    /// A-25 / A-21：严格模式下小智桥接的发送只生成审批；桌面批准后以审批身份执行，
    /// 使用审批保存的连接目标而不是执行请求里的地址。
    #[test]
    fn strict_mode_turns_bridge_send_into_approval_and_executes_with_saved_target() {
        let fixture = strict_fixture(true, true);
        let workflow_id = preview_workflow(&fixture.service);
        let mut request = confirm_send_body(&workflow_id, "127.0.0.1");
        request["request_id"] = serde_json::json!("req-1");
        let (status, pending) =
            post_with_ctx(&fixture.service, "/api/workflow", &request, laser_contracts::CallerContext::McpBridge);
        assert_eq!(status, 202, "{pending}");
        assert_eq!(pending["success"], true);
        assert_eq!(pending["result"]["status"], "approval_required");
        assert!(pending["result"]["speech"].as_str().is_some_and(|speech| !speech.is_empty()));
        assert!(!pending.to_string().contains("127.0.0.1"), "响应不得包含设备地址: {pending}");
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();
        assert!(fixture.targets.lock().unwrap().is_empty() && fixture.written.lock().unwrap().is_empty());
        let status_now = post_json(&fixture.service, "/api/workflow", &serde_json::json!({"action": "status", "workflow_id": workflow_id}));
        assert_eq!(status_now["result"]["status"], "preview_ready");
        let (_, again) =
            post_with_ctx(&fixture.service, "/api/workflow", &request, laser_contracts::CallerContext::McpBridge);
        assert_ne!(again["result"]["approval_id"], approval_id.as_str(), "旧桥接身份无法区分接入点，不得按自报编号复用另一条审批");
        assert!(fixture.targets.lock().unwrap().is_empty(), "两次请求仍都未获执行授权");

        let approval_ctx = laser_contracts::CallerContext::Approval { approval_id: approval_id.clone() };
        let execute = confirm_send_body(&workflow_id, "192.0.2.99");
        let (status, early) = post_with_ctx(&fixture.service, "/api/workflow", &execute, approval_ctx.clone());
        assert_eq!(status, 409, "{early}");
        assert_eq!(early["detail"]["error_code"], "approval_not_approved");

        let approvals = fixture.service.approvals().unwrap();
        let record = approvals.get(&approval_id).unwrap();
        assert_eq!(record.kind, laser_contracts::ApprovalKind::WorkflowConfirmSend);
        assert_eq!(record.source, laser_contracts::ApprovalSource::McpBridge);
        assert_eq!(record.target_record, workflow_id);
        approvals.approve(&approval_id, &record.digest, &record.digest).unwrap();

        let (status, sent) = post_with_ctx(&fixture.service, "/api/workflow", &execute, approval_ctx);
        assert_eq!(status, 200, "{sent}");
        assert_eq!(sent["result"]["status"], "sending");
        assert_eq!(
            fixture.targets.lock().unwrap().first().map(String::as_str),
            Some("127.0.0.1:23"),
            "必须使用审批保存的目标"
        );
        let executed = approvals.get(&approval_id).unwrap();
        assert_eq!(executed.status, laser_contracts::ApprovalStatus::Executed);
        assert_eq!(executed.outcome.unwrap().job_id.as_deref(), Some(workflow_id.as_str()));
        let finished = wait_for_status(&fixture.service, &workflow_id, "completed");
        assert_eq!(finished["result"]["status"], "completed", "{finished}");
    }

    /// A-23 / A-26：安全动作经局域网发起只生成审批；批准后执行一次，重放得到 approval_consumed。
    #[test]
    fn strict_mode_safe_action_approval_is_single_use() {
        let fixture = strict_fixture(true, true);
        let body = serde_json::json!({
            "action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"
        });
        let lan = laser_contracts::CallerContext::Lan { pairing_id: "pair_test".into() };
        let (status, pending) = post_with_ctx(&fixture.service, "/api/safe-action", &body, lan);
        assert_eq!(status, 202, "{pending}");
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();
        assert!(fixture.targets.lock().unwrap().is_empty());
        let approvals = fixture.service.approvals().unwrap();
        let record = approvals.get(&approval_id).unwrap();
        assert_eq!(record.kind, laser_contracts::ApprovalKind::SafeAction);
        assert_eq!(record.source, laser_contracts::ApprovalSource::Lan);
        approvals.approve(&approval_id, &record.digest, &record.digest).unwrap();

        let approval_ctx = laser_contracts::CallerContext::Approval { approval_id: approval_id.clone() };
        let (_, done) = post_with_ctx(&fixture.service, "/api/safe-action", &body, approval_ctx.clone());
        assert!(
            fixture.written.lock().unwrap().iter().any(|line| line == "G92 X0 Y0"),
            "批准后应执行一次: {done}"
        );
        let (status, replay) = post_with_ctx(&fixture.service, "/api/safe-action", &body, approval_ctx);
        assert_eq!(status, 409, "{replay}");
        assert_eq!(replay["detail"]["error_code"], "approval_consumed");

        // 授权不能挪用到别的动作：要素不同，摘要不一致
        let (_, second) = post_with_ctx(
            &fixture.service,
            "/api/safe-action",
            &body,
            laser_contracts::CallerContext::Lan { pairing_id: "pair_test".into() },
        );
        let second_id = second["result"]["approval_id"].as_str().unwrap().to_owned();
        let second_record = approvals.get(&second_id).unwrap();
        approvals.approve(&second_id, &second_record.digest, &second_record.digest).unwrap();
        let (status, mismatch) = post_with_ctx(
            &fixture.service,
            "/api/safe-action",
            &serde_json::json!({"action": "home", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
            laser_contracts::CallerContext::Approval { approval_id: second_id.clone() },
        );
        assert_eq!(status, 409, "{mismatch}");
        assert_eq!(mismatch["detail"]["error_code"], "approval_mismatch");
        assert_eq!(approvals.get(&second_id).unwrap().status, laser_contracts::ApprovalStatus::Failed);
    }

    #[test]
    fn strict_mode_allows_desktop_session_and_fails_closed_without_database() {
        let fixture = strict_fixture(true, true);
        let workflow_id = preview_workflow(&fixture.service);
        let (status, sent) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::Desktop,
        );
        assert_eq!(status, 200, "{sent}");
        assert_eq!(sent["result"]["status"], "sending");
        wait_for_status(&fixture.service, &workflow_id, "completed");

        let no_db = strict_fixture(true, false);
        let workflow_id = preview_workflow(&no_db.service);
        let (status, unavailable) = post_with_ctx(
            &no_db.service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(status, 503, "{unavailable}");
        assert_eq!(unavailable["detail"]["error_code"], "approval_unavailable");
        assert!(no_db.targets.lock().unwrap().is_empty());
    }

    /// A-40：严格模式关闭时，小智桥接与本机回环的发送行为与之前一致，不创建审批。
    #[test]
    fn strict_mode_off_keeps_existing_behavior() {
        let fixture = strict_fixture(false, true);
        let workflow_id = preview_workflow(&fixture.service);
        let (status, sent) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(status, 200, "{sent}");
        assert_eq!(sent["result"]["status"], "sending");
        wait_for_status(&fixture.service, &workflow_id, "completed");
        let home = post_json(&fixture.service, "/api/safe-action", &serde_json::json!({
            "action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"
        }));
        assert_ne!(home["detail"]["error_code"], "strict_mode_blocked", "{home}");
        assert!(fixture.service.approvals().unwrap().list(None, 10).unwrap().is_empty());
        assert!(!fixture.service.strict_mode());
    }

    // ----- 桌面会话与桌面审批接口（技术书 §5.3，实施清单 P1-07，验收 A-21、A-22、A-75、A-76） -----

    fn desktop_request(
        service: &ApiService,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        session: Option<&str>,
    ) -> (u16, serde_json::Value) {
        let body = body.map(|value| value.to_string()).unwrap_or_default();
        let header = session.map(|value| format!("X-Moss-Desktop-Session: {value}\r\n")).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{header}Content-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let raw = service.handle_http(request.as_bytes());
        let status = String::from_utf8_lossy(&raw[9..12]).parse::<u16>().unwrap();
        (status, parse_json_response(&raw))
    }

    #[test]
    fn desktop_session_header_is_checked_and_maps_to_desktop_context() {
        let fixture = strict_fixture(true, true);
        let workflow_id = preview_workflow(&fixture.service);
        let send = confirm_send_body(&workflow_id, "127.0.0.1");
        // 尚未开启桌面会话（如独立 moss-host）：带头即拒绝
        let (status, rejected) = desktop_request(&fixture.service, "POST", "/api/workflow", Some(&send), Some(&"a".repeat(64)));
        assert_eq!(status, 403, "{rejected}");
        assert_eq!(rejected["detail"]["error_code"], "desktop_session_invalid");

        let session = fixture.service.enable_desktop_session().unwrap();
        assert_eq!(session.len(), 64);
        assert_eq!(fixture.service.enable_desktop_session().unwrap(), session, "同一进程只有一个会话");
        let (status, wrong) = desktop_request(&fixture.service, "POST", "/api/workflow", Some(&send), Some(&"b".repeat(64)));
        assert_eq!(status, 403, "{wrong}");
        assert_eq!(wrong["detail"]["error_code"], "desktop_session_invalid");
        assert!(fixture.targets.lock().unwrap().is_empty());
        // 非回环连接即使带对了值也不算桌面会话
        assert!(fixture.service.check_desktop_session(&format!("GET / HTTP/1.1\r\nX-Moss-Desktop-Session: {session}"), false).is_err());

        // 严格模式下带有效会话的本机发送 = 桌面端，直接执行
        let (status, sent) = desktop_request(&fixture.service, "POST", "/api/workflow", Some(&send), Some(&session));
        assert_eq!(status, 200, "{sent}");
        assert_eq!(sent["result"]["status"], "sending");
        wait_for_status(&fixture.service, &workflow_id, "completed");
    }

    /// A-75：审批接口只接受桌面会话。
    #[test]
    fn approval_routes_require_desktop_session() {
        let fixture = strict_fixture(true, true);
        let session = fixture.service.enable_desktop_session().unwrap();
        let (status, response) = desktop_request(&fixture.service, "GET", "/v1/approvals?status=pending", None, None);
        assert_eq!(status, 403, "{response}");
        assert_eq!(response["detail"]["error_code"], "desktop_session_required");
        let (status, _) = post_with_ctx(
            &fixture.service,
            "/v1/approvals/apr_x/approve",
            &serde_json::json!({"digest": "x"}),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(status, 403);
        let (status, response) = desktop_request(&fixture.service, "GET", "/v1/approvals?status=bogus", None, Some(&session));
        assert_eq!(status, 400, "{response}");
        let (status, response) = desktop_request(&fixture.service, "GET", "/v1/approvals?status=pending", None, Some(&session));
        assert_eq!(status, 200, "{response}");
        assert_eq!(response["result"]["approvals"].as_array().unwrap().len(), 0);
        assert_eq!(response["result"]["ttl_seconds"], 120);
        let (status, response) = desktop_request(
            &fixture.service, "POST", "/v1/approvals/apr_missing/approve", Some(&serde_json::json!({"digest": "x"})), Some(&session),
        );
        assert_eq!(status, 404, "{response}");
        assert_eq!(response["detail"]["error_code"], "approval_not_found");
    }

    /// A-21：小智发起 → 桌面列表（不含地址）→ 桌面批准 → 以审批身份按保存的目标执行。
    #[test]
    fn desktop_approval_executes_bridge_request_with_saved_target() {
        let fixture = strict_fixture(true, true);
        let session = fixture.service.enable_desktop_session().unwrap();
        let workflow_id = preview_workflow(&fixture.service);
        let (status, pending) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(status, 202, "{pending}");
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();

        let (status, listed) = desktop_request(&fixture.service, "GET", "/v1/approvals?status=pending", None, Some(&session));
        assert_eq!(status, 200, "{listed}");
        let card = &listed["result"]["approvals"][0];
        assert_eq!(card["approval_id"], approval_id.as_str());
        assert_eq!(card["target"]["mode"], "network");
        assert_eq!(card["target"]["label"], "网络设备");
        assert!(!listed.to_string().contains("127.0.0.1"), "列表不得包含设备地址: {listed}");
        let digest = card["digest"].as_str().unwrap().to_owned();

        let approve_path = format!("/v1/approvals/{approval_id}/approve");
        let (status, wrong) = desktop_request(
            &fixture.service, "POST", &approve_path, Some(&serde_json::json!({"digest": "0".repeat(64)})), Some(&session),
        );
        assert_eq!(status, 409, "{wrong}");
        assert_eq!(wrong["detail"]["error_code"], "approval_mismatch");
        assert!(fixture.targets.lock().unwrap().is_empty());

        let (status, approved) =
            desktop_request(&fixture.service, "POST", &approve_path, Some(&serde_json::json!({"digest": digest})), Some(&session));
        assert_eq!(status, 200, "{approved}");
        assert_eq!(approved["result"]["result"]["status"], "sending");
        assert_eq!(approved["result"]["approval"]["status"], "executed");
        assert_eq!(fixture.targets.lock().unwrap().first().map(String::as_str), Some("127.0.0.1:23"));
        let finished = wait_for_status(&fixture.service, &workflow_id, "completed");
        assert_eq!(finished["result"]["status"], "completed", "{finished}");

        let (status, again) =
            desktop_request(&fixture.service, "POST", &approve_path, Some(&serde_json::json!({"digest": digest})), Some(&session));
        assert_eq!(status, 409, "{again}");
        assert_eq!(again["detail"]["error_code"], "approval_consumed");
    }

    /// A-22：创建审批后加工内容变化（重新预览），桌面批准得到 approval_mismatch，不执行。
    #[test]
    fn desktop_approval_rejects_changed_content() {
        let fixture = strict_fixture(true, true);
        let session = fixture.service.enable_desktop_session().unwrap();
        let workflow_id = preview_workflow(&fixture.service);
        let (_, pending) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::McpBridge,
        );
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();
        let digest = fixture.service.approvals().unwrap().get(&approval_id).unwrap().digest;

        let store = fixture.service.store.as_ref().unwrap();
        let mut stored = store.get(laser_runtime::RecordKind::Workflow, &workflow_id).unwrap().unwrap();
        stored.payload["artifacts"]["gcode_sha256"] = serde_json::json!("f".repeat(64));
        store.put(&stored).unwrap();

        let (status, response) = desktop_request(
            &fixture.service,
            "POST",
            &format!("/v1/approvals/{approval_id}/approve"),
            Some(&serde_json::json!({"digest": digest})),
            Some(&session),
        );
        assert_eq!(status, 409, "{response}");
        assert_eq!(response["detail"]["error_code"], "approval_mismatch");
        assert_eq!(response["detail"]["approval"]["status"], "pending");
        assert!(fixture.targets.lock().unwrap().is_empty(), "摘要不一致不得连接设备");
    }

    #[test]
    fn desktop_reject_and_safe_action_approval() {
        let fixture = strict_fixture(true, true);
        let session = fixture.service.enable_desktop_session().unwrap();
        let body = serde_json::json!({"action": "set_origin", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"});
        let lan = || laser_contracts::CallerContext::Lan { pairing_id: "pair_test".into() };

        let (_, first) = post_with_ctx(&fixture.service, "/api/safe-action", &body, lan());
        let first_id = first["result"]["approval_id"].as_str().unwrap().to_owned();
        let (status, rejected) = desktop_request(
            &fixture.service,
            "POST",
            &format!("/v1/approvals/{first_id}/reject"),
            Some(&serde_json::json!({"reason": "材料没放好"})),
            Some(&session),
        );
        assert_eq!(status, 200, "{rejected}");
        assert_eq!(rejected["result"]["status"], "rejected");
        let digest = fixture.service.approvals().unwrap().get(&first_id).unwrap().digest;
        let (status, after_reject) = desktop_request(
            &fixture.service,
            "POST",
            &format!("/v1/approvals/{first_id}/approve"),
            Some(&serde_json::json!({"digest": digest})),
            Some(&session),
        );
        assert_eq!(status, 409, "{after_reject}");
        assert_eq!(after_reject["detail"]["error_code"], "approval_rejected");
        assert!(fixture.targets.lock().unwrap().is_empty());

        let (_, second) = post_with_ctx(&fixture.service, "/api/safe-action", &body, lan());
        let second_id = second["result"]["approval_id"].as_str().unwrap().to_owned();
        let digest = fixture.service.approvals().unwrap().get(&second_id).unwrap().digest;
        let (_, approved) = desktop_request(
            &fixture.service,
            "POST",
            &format!("/v1/approvals/{second_id}/approve"),
            Some(&serde_json::json!({"digest": digest})),
            Some(&session),
        );
        assert!(
            fixture.written.lock().unwrap().iter().any(|line| line == "G92 X0 Y0"),
            "批准后应执行设原点: {approved}"
        );
        let state = fixture.service.approvals().unwrap().get(&second_id).unwrap().status;
        assert!(
            matches!(state, laser_contracts::ApprovalStatus::Executed | laser_contracts::ApprovalStatus::Failed),
            "{state:?}"
        );
    }

    // ----- 审计（技术书 §7.3，实施清单 P1-08，验收 A-20、A-34） -----

    fn audit_trail(fixture: &StrictFixture) -> Vec<laser_contracts::AuditEntry> {
        let mut entries = fixture.db.as_ref().unwrap().lock().unwrap().recent_audit(100).unwrap();
        entries.reverse();
        entries
    }

    /// A-20：模型带 confirmed=true 发起发送 → 只生成审批，审计记录“删除确认”；
    /// 桌面批准与执行、回环被拦截都留下审计，且审计不含设备地址。
    #[test]
    fn strict_mode_actions_are_audited() {
        use laser_contracts::{AuditAction, AuditResult};
        let fixture = strict_fixture(true, true);
        let session = fixture.service.enable_desktop_session().unwrap();
        let workflow_id = preview_workflow(&fixture.service);

        let blocked = post_json(&fixture.service, "/api/workflow", &confirm_send_body(&workflow_id, "127.0.0.1"));
        assert_eq!(blocked["detail"]["error_code"], "strict_mode_blocked");

        let (status, pending) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(status, 202, "{pending}");
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();
        let digest = fixture.service.approvals().unwrap().get(&approval_id).unwrap().digest;
        let (status, approved) = desktop_request(
            &fixture.service,
            "POST",
            &format!("/v1/approvals/{approval_id}/approve"),
            Some(&serde_json::json!({"digest": digest})),
            Some(&session),
        );
        assert_eq!(status, 200, "{approved}");
        wait_for_status(&fixture.service, &workflow_id, "completed");

        let trail = audit_trail(&fixture);
        let actions: Vec<AuditAction> = trail.iter().map(|entry| entry.action).collect();
        assert_eq!(
            actions,
            vec![
                AuditAction::StrictModeBlocked,
                AuditAction::ApprovalCreated,
                AuditAction::ConfirmedFieldStripped,
                AuditAction::ApprovalApproved,
                AuditAction::ApprovalExecuted,
            ],
            "{trail:?}"
        );
        assert_eq!(trail[0].client, "local");
        assert_eq!(trail[0].result, AuditResult::Denied);
        assert_eq!(trail[0].target, workflow_id);
        assert_eq!(trail[1].client, "mcp");
        assert_eq!(trail[2].detail.removed_fields, vec!["confirmed".to_string()]);
        assert_eq!(trail[3].client, "desktop");
        assert!(trail[1..].iter().all(|entry| entry.detail.approval_id.as_deref() == Some(approval_id.as_str())));
        let text = serde_json::to_string(&trail).unwrap();
        assert!(!text.contains("127.0.0.1"), "审计不得包含设备地址: {text}");
        // 审批保存的执行参数里也不保留模型自填的 confirmed
        let stored = fixture.service.approvals().unwrap().get(&approval_id).unwrap();
        assert!(!stored.request.unwrap().contains_key("confirmed"));
    }

    /// A-34：审计写入失败时 R3 拒绝（不创建审批、不连接设备），R2 停止类照常执行。
    #[test]
    fn audit_failure_refuses_r3_and_keeps_r2() {
        let fixture = strict_fixture(true, true);
        fixture.db.as_ref().unwrap().lock().unwrap().connection.execute_batch("DROP TABLE audit_log;").unwrap();
        let lan = || laser_contracts::CallerContext::Lan { pairing_id: "pair_test".into() };

        let (status, refused) = post_with_ctx(
            &fixture.service,
            "/api/safe-action",
            &serde_json::json!({"action": "home", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
            lan(),
        );
        assert_eq!(status, 503, "{refused}");
        assert_eq!(refused["detail"]["error_code"], "approval_unavailable");
        assert!(fixture.service.approvals().unwrap().list(None, 10).unwrap().is_empty());
        assert!(fixture.targets.lock().unwrap().is_empty());

        let (status, stopped) = post_with_ctx(
            &fixture.service,
            "/api/safe-action",
            &serde_json::json!({"action": "emergency_stop", "confirmed": true, "connection_mode": "network", "host": "127.0.0.1"}),
            lan(),
        );
        assert!(status != 503 && status != 403 && status != 202, "{stopped}");
        assert!(!fixture.targets.lock().unwrap().is_empty(), "急停照常连接设备");

        // 回环被拦截时审计也写不进去，但拦截照常生效
        let blocked = post_json(&fixture.service, "/api/device/framing", &serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
        }));
        assert_eq!(blocked["detail"]["error_code"], "strict_mode_blocked");
    }

    // ----- 故障场景（技术书 §8，实施清单 P1-10，验收 A-31、A-33） -----

    /// 握手与预检正常，第一条加工行写入时一直阻塞到测试放行：作业确定停在“发送中”，
    /// 模拟进程崩溃时的在途作业（不依赖心跳或超时的时序）。
    fn gated_transport_factory(gate: std::sync::Arc<std::sync::atomic::AtomicBool>) -> TransportFactory {
        struct GatedGrbl {
            responses: std::sync::Mutex<std::collections::VecDeque<String>>,
            gate: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }
        impl laser_device::DeviceTransport for GatedGrbl {
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
                while !self.gate.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err("测试结束：模拟连接断开".into())
            }
            fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
                if command == laser_device::RealtimeCommand::StatusQuery {
                    self.responses.lock().unwrap().push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
                }
                Ok(())
            }
            fn read_line(&self) -> Result<Option<String>, String> {
                Ok(self.responses.lock().unwrap().pop_front())
            }
        }
        std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
            Ok(Box::new(GatedGrbl { responses: Default::default(), gate: gate.clone() }) as Box<dyn laser_device::DeviceTransport>)
        })
    }

    /// 走边框用的假设备：实时状态固定为 `status`，记录写入的每一行。
    fn status_transport_factory(status: &'static str, written: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> TransportFactory {
        struct StatusGrbl {
            status: &'static str,
            responses: std::sync::Mutex<std::collections::VecDeque<String>>,
            written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }
        impl laser_device::DeviceTransport for StatusGrbl {
            fn write_line(&self, line: &str) -> Result<(), String> {
                self.written.lock().unwrap().push(line.to_owned());
                let mut responses = self.responses.lock().unwrap();
                if line == "$$" {
                    responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                } else {
                    responses.push_back("ok".into());
                }
                Ok(())
            }
            fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
                if command == laser_device::RealtimeCommand::StatusQuery {
                    self.responses.lock().unwrap().push_back(format!("<{}|MPos:0,0,0|FS:0,0>", self.status));
                }
                Ok(())
            }
            fn read_line(&self) -> Result<Option<String>, String> {
                Ok(self.responses.lock().unwrap().pop_front())
            }
        }
        std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
            Ok(Box::new(StatusGrbl { status, responses: Default::default(), written: written.clone() }) as Box<dyn laser_device::DeviceTransport>)
        })
    }

    /// 走边框默认不出光，勾选弱光才写 M4 S1；设备不在 Idle 时不运动（2026-10-14）。
    #[test]
    fn framing_defaults_to_laser_off_and_requires_idle_preflight() {
        let framing = |service: &ApiService, power: Option<i64>| {
            let mut body = serde_json::json!({"connection_mode": "network", "network_host": "127.0.0.1", "max_x": 20, "max_y": 10});
            if let Some(power) = power { body["power"] = power.into(); }
            post_json(service, "/api/device/framing", &body)
        };
        let motion = |lines: &[String]| lines.iter().filter(|line| line.starts_with("G0") || line.starts_with("G1") || line.starts_with("M4")).cloned().collect::<Vec<_>>();

        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_transport_factory(status_transport_factory("Idle", written.clone()));
        let off = framing(&service, None);
        assert_eq!(off["success"], true, "{off}");
        assert_eq!(off["result"]["laser_on"], false);
        let lines = written.lock().unwrap().clone();
        assert!(!lines.iter().any(|line| line.starts_with("M4")), "默认不能出光: {lines:?}");
        assert!(lines.iter().any(|line| line.starts_with("G1 X20")), "{lines:?}");
        assert_eq!(service.active_job_count(), 0, "完成后不再占用设备");

        written.lock().unwrap().clear();
        let weak = framing(&service, Some(1));
        assert_eq!(weak["result"]["laser_on"], true, "{weak}");
        assert!(written.lock().unwrap().iter().any(|line| line == "M4 S1"), "{:?}", written.lock().unwrap());

        let alarm_written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let alarm = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_transport_factory(status_transport_factory("Alarm", alarm_written.clone()));
        let refused = framing(&alarm, Some(1));
        assert_eq!(refused["detail"]["error_code"], "device_preflight_failed", "{refused}");
        assert!(motion(&alarm_written.lock().unwrap()).is_empty(), "预检失败不能运动: {:?}", alarm_written.lock().unwrap());
    }

    /// 走边框运行中登记为活动作业：顶栏急停能找到并停止它，结束后保留为“待确认停止”（2026-10-14）。
    #[test]
    fn framing_is_listed_in_stop_targets_and_can_be_emergency_stopped() {
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let service = std::sync::Arc::new(
            ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
                .with_transport_factory(gated_transport_factory(gate.clone())),
        );
        let runner = {
            let service = service.clone();
            std::thread::spawn(move || post_json(&service, "/api/device/framing", &serde_json::json!({
                "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
            })))
        };
        let targets_request = b"GET /api/jobs/stop-targets HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let start = std::time::Instant::now();
        let job_id = loop {
            let targets = parse_json_response(&service.handle_http(targets_request));
            if let Some(id) = targets["result"]["jobs"][0]["job_id"].as_str() {
                assert_eq!(targets["result"]["jobs"][0]["state"], "running");
                break id.to_owned();
            }
            if runner.is_finished() || start.elapsed() > std::time::Duration::from_secs(5) {
                gate.store(true, std::sync::atomic::Ordering::SeqCst);
                panic!("走边框没有登记为活动作业: {}", runner.join().unwrap());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(job_id.starts_with("framing-"), "{job_id}");
        let busy = post_json(&service, "/api/device/framing", &serde_json::json!({
            "connection_mode": "network", "network_host": "127.0.0.1", "max_x": 10, "max_y": 10
        }));
        assert_eq!(busy["detail"]["error_code"], "device_busy", "{busy}");
        let stop = post_json(&service, "/api/safe-action", &serde_json::json!({
            "action": "emergency_stop", "confirmed": true, "job_id": job_id
        }));
        assert_eq!(stop["success"], true, "{stop}");
        gate.store(true, std::sync::atomic::Ordering::SeqCst);
        let result = runner.join().unwrap();
        assert_eq!(result["detail"]["error_code"], "framing_cancelled", "{result}");
        assert_eq!(service.active_job_count(), 0);
        let targets = parse_json_response(&service.handle_http(targets_request));
        assert_eq!(targets["result"]["jobs"][0]["state"], "unknown", "停止未确认，保留原通道供重试急停: {targets}");
    }

    /// A-33：经审批的发送进行中进程崩溃 → 新进程启动恢复后 workflow 为 failed、审批结果未知，
    /// 不连接设备、不重放，同一授权不能再用。
    #[test]
    fn restart_during_approved_send_marks_result_unknown_and_never_replays() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("moss.sqlite");
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let db1 = std::sync::Arc::new(std::sync::Mutex::new(laser_runtime::RuntimeDb::open(&db_path).unwrap()));
        let service = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_generated_root(temp.path().to_path_buf())
            .with_transport_factory(gated_transport_factory(gate.clone()))
            .with_runtime_db(db1)
            .with_strict_mode(true);
        let session = service.enable_desktop_session().unwrap();
        let workflow_id = preview_workflow(&service);
        let (status, pending) = post_with_ctx(
            &service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::McpBridge,
        );
        assert_eq!(status, 202, "{pending}");
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();
        let digest = service.approvals().unwrap().get(&approval_id).unwrap().digest;
        let (status, approved) = desktop_request(
            &service,
            "POST",
            &format!("/v1/approvals/{approval_id}/approve"),
            Some(&serde_json::json!({"digest": digest})),
            Some(&session),
        );
        assert_eq!(status, 200, "{approved}");
        assert_eq!(approved["result"]["result"]["status"], "sending");
        let running = service.approvals().unwrap().get(&approval_id).unwrap();
        assert_eq!(running.outcome.unwrap().phase, Some(laser_contracts::ApprovalPhase::Running));

        // “崩溃后重启”：新进程打开同一个数据库，按 laser-host 的启动顺序恢复
        let db2 = laser_runtime::RuntimeDb::open(&db_path).unwrap();
        db2.recover_after_restart().unwrap();
        let written_after = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let restarted = ApiService::new(test_loopback_settings(), laser_contracts::builtin_material_library())
            .with_generated_root(temp.path().to_path_buf())
            .with_transport_factory(recording_transport_factory(200_000, written_after.clone()))
            .with_runtime_db(std::sync::Arc::new(std::sync::Mutex::new(db2)))
            .with_strict_mode(true);
        assert_eq!(restarted.recover_approvals_after_restart(), 1);

        let record = restarted.approvals().unwrap().get(&approval_id).unwrap();
        assert_eq!(record.status, laser_contracts::ApprovalStatus::Failed);
        let outcome = record.outcome.unwrap();
        assert_eq!(outcome.error_code.as_deref(), Some("result_unknown"));
        assert_eq!(outcome.phase, Some(laser_contracts::ApprovalPhase::Unknown));
        let status = post_json(&restarted, "/api/workflow", &serde_json::json!({"action": "status", "workflow_id": workflow_id}));
        assert_eq!(status["result"]["status"], "failed", "{status}");

        // 不重放：重启后的服务没有连接设备；同一授权执行被拒
        let (status, replay) = post_with_ctx(
            &restarted,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::Approval { approval_id: approval_id.clone() },
        );
        assert_ne!(status, 200, "{replay}");
        assert_ne!(replay["result"]["status"], "sending", "{replay}");
        assert!(written_after.lock().unwrap().is_empty(), "重启后不得向设备写入任何内容");
        assert_eq!(
            restarted.approvals().unwrap().consume(&approval_id, &digest).unwrap_err(),
            approval::ApprovalError::Consumed
        );

        // 清理：停掉“崩溃前进程”里仍在等待的作业；迟到的回写不覆盖“结果未知”
        let _ = service.abort_all_active_jobs();
        gate.store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(
            restarted.approvals().unwrap().get(&approval_id).unwrap().outcome.unwrap().phase,
            Some(laser_contracts::ApprovalPhase::Unknown)
        );
    }

    /// 经审批的作业正常结束后，审批结果写回“加工完成”。
    #[test]
    fn approved_job_completion_is_written_back_to_approval() {
        let fixture = strict_fixture(true, true);
        let session = fixture.service.enable_desktop_session().unwrap();
        let workflow_id = preview_workflow(&fixture.service);
        let (_, pending) = post_with_ctx(
            &fixture.service,
            "/api/workflow",
            &confirm_send_body(&workflow_id, "127.0.0.1"),
            laser_contracts::CallerContext::McpBridge,
        );
        let approval_id = pending["result"]["approval_id"].as_str().unwrap().to_owned();
        let digest = fixture.service.approvals().unwrap().get(&approval_id).unwrap().digest;
        let (status, approved) = desktop_request(
            &fixture.service,
            "POST",
            &format!("/v1/approvals/{approval_id}/approve"),
            Some(&serde_json::json!({"digest": digest})),
            Some(&session),
        );
        assert_eq!(status, 200, "{approved}");
        wait_for_status(&fixture.service, &workflow_id, "completed");
        let record = fixture.service.approvals().unwrap().get(&approval_id).unwrap();
        assert_eq!(record.status, laser_contracts::ApprovalStatus::Executed);
        let outcome = record.outcome.unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.phase, Some(laser_contracts::ApprovalPhase::Completed));
        assert_eq!(outcome.job_id.as_deref(), Some(workflow_id.as_str()));
    }
}
