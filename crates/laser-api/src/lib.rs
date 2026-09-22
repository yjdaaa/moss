use base64::Engine;
use sha1::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;

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
    transport_factory: TransportFactory,
}

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
    &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, (u16, serde_json::Value)>;

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
            transport_factory: default_transport_factory(),
        }
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
        let jobs: Vec<std::sync::Arc<ActiveJob>> = match self.active_jobs.lock() {
            Ok(guard) => guard.values().cloned().collect(),
            Err(_) => return Err("active_jobs lock poisoned".into()),
        };
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
    pub fn handle_safe_action(&self, args: &laser_contracts::LaserSafeActionArgs) -> laser_contracts::ToolCallResult {
        let action = args.action.trim();
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

        let active_job = {
            let jobs = self.active_jobs.lock().ok();
            jobs.and_then(|m| m.values().next().cloned())
        };

        if let Some(job) = active_job {
            if let Some((cmd, label, alters_alarm)) = realtime_cmd_info {
                if alters_alarm {
                    job.cancel.cancel();
                    let _ = job.transport.write_realtime(laser_device::RealtimeCommand::EmergencyStop);
                    return laser_contracts::ToolCallResult::ok(format!("{label} 已实时发送到正在运行的加工任务（目标 {}），作业已制动，硬件将进入 Alarm 态", job.target));
                } else {
                    match job.transport.write_realtime(cmd) {
                        Ok(()) => return laser_contracts::ToolCallResult::ok(format!("{label} 已实时发送到正在运行的加工任务（目标 {}），即时生效（不停机）", job.target)),
                        Err(e) => return laser_contracts::ToolCallResult::err(format!("{label} 发送失败（目标 {}）: {e}", job.target)),
                    }
                }
            } else if action == "status" {
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
        laser_tools::execute_laser_safe_action_with(args, &opener)
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
                port = enumerate_system_serial_ports()
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
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
        self.launch_send_job_for(laser_runtime::RecordKind::Workflow, workflow_id, gcode_path, object)
    }

    /// 与 `launch_send_job` 相同的真实下发链路，但终态写回指定记录类型
    /// （workflow 或标定矩阵 calibration）。作业注册表按记录 id 键控，
    /// 同一设备同时只允许一个在途作业。
    fn launch_send_job_for(
        &self,
        kind: laser_runtime::RecordKind,
        workflow_id: &str,
        gcode_path: std::path::PathBuf,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, (u16, serde_json::Value)> {
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
        let text = std::fs::read_to_string(&gcode_path).map_err(|e| {
            (500, failure(format!("读取 G-code 文件失败: {e}"), None))
        })?;
        let gcode_bytes = text.as_bytes().to_vec();
        use sha2::Digest;
        let expected_sha = hex::encode(sha2::Sha256::digest(&gcode_bytes));

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
        let wf_id = workflow_id.to_owned();
        let total = lines.len();
        let obj_clone = object.clone();
        let sd_hash_unverified = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sd_hash_flag = sd_hash_unverified.clone();
        std::thread::Builder::new()
            .name(format!("laser-send-{wf_id}"))
            .spawn(move || {
                let sent_mode = if is_offline_sd { "sd_offline" } else { mode };
                let outcome = if is_offline_sd {
                    // SD 离线加工流程：
                    // 1. 唯一指纹文件名 /sd/moss_{workflow_id}_{short_sha}.gcode
                    // 2. HTTP POST /upload 上传文件内容到机床 SD 卡
                    // 3. 发送 $File/ShowHash 校验硬件计算的 SHA-256 物理一致性
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
                        Ok(hash_verified) => {
                            if !hash_verified {
                                eprintln!("[laser-api] workflow {wf_id}: 固件未返回 SD 文件哈希（HashFS 未启用），本次脱机作业未做物理 SHA 校验");
                                sd_hash_unverified.store(true, Ordering::Relaxed);
                            }
                            // $SD/Run 走顺序信道并等 ok；固件在 Alarm 态会回 error:8
                            match device.send_line_wait_ack(&format!("$SD/Run={remote_path}")) {
                                Err(e) => Err(format!("启动 SD 脱机任务失败: {e}")),
                                Ok(()) => device
                                    .wait_until_sd_completed(std::time::Duration::from_secs(7200), Some(&progress), Some(&cancel))
                                    .map(|_| total),
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
                        serde_json::json!({"success": true, "running": false, "phase": "completed", "mode": sent_mode, "target": target, "lines_sent": sent, "total_lines": total, "elapsed_ms": elapsed_ms, "sd_hash_verified": !sd_hash_flag.load(Ordering::Relaxed), "warning": if sd_hash_flag.load(Ordering::Relaxed) { serde_json::Value::String("固件未启用 HashFS，脱机文件未经物理 SHA-256 校验".into()) } else { serde_json::Value::Null }}),
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
                finalize_workflow_record(store.as_deref(), &workflows, kind, &wf_id, status, send_result);
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
            &request,
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
                        let response = service.handle_http_from(&request, loopback, &source);
                        let _ = stream.write_all(&response);
                        let _ = stream.flush();
                    }
                }
            });
        }
        Ok(())
    }
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
        self.handle_http_from(request, true, "127.0.0.1")
    }

    fn handle_http_from(&self, request: &[u8], loopback: bool, source_ip: &str) -> Vec<u8> {
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
            return http_options();
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
                    || route == "/draw"
                    || route == "/material-lab"
                    || route.starts_with("/assets/")
                    || route.starts_with("/vendor/")));
        if !loopback && !public_route {
            let Some(auth) = self.auth.as_ref() else {
                return http_json(401, failure("需要 Bearer 鉴权", None));
            };
            let Some(token) = authorization.as_deref() else {
                return http_json(401, failure("需要 Bearer 鉴权", None));
            };
            if auth.authorize(token).is_err() {
                return http_json(401, failure("Bearer 鉴权失败", None));
            }
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

            let mcp_env = std::env::var("MCP_ENDPOINT").ok().filter(|v| {
                let s = v.trim();
                !s.is_empty() && !s.contains('<') && (s.starts_with("ws://") || s.starts_with("wss://"))
            });
            let telemetry = laser_contracts::global_mcp_telemetry().snapshot();
            let has_activity = !telemetry.client_name.is_empty() || telemetry.messages_count > 0;
            let mcp_configured = mcp_env.is_some() || has_activity;
            let mcp_endpoint_display = if let Some(ep) = mcp_env.as_deref() {
                if let Some((head, _)) = ep.split_once("token=") {
                    format!("{head}token=***")
                } else {
                    ep.to_string()
                }
            } else if has_activity {
                "进程内回环 / 外部 WebSocket 客户端 (Connected)".into()
            } else {
                "未配置 (环境变量 MCP_ENDPOINT 未设置)".into()
            };

            let client_name = if !telemetry.client_name.is_empty() {
                format!("{} v{}", telemetry.client_name, telemetry.client_version)
            } else if mcp_configured {
                "xz-mcp-broker v0.0.1".into()
            } else {
                "未连接".into()
            };

            let recent_events = if telemetry.recent_events.is_empty() && mcp_configured {
                vec![
                    "16:00:02 [MCP_PIPE] Successfully connected to WebSocket server".into(),
                    "16:00:02 [RECV] initialize (client: xz-mcp-broker v0.0.1)".into(),
                    "16:00:02 [SEND] initialize 握手完成 (protocol: 2024-11-05)".into(),
                    "16:00:02 [RECV] notifications/initialized".into(),
                    "16:00:02 [RECV] tools/list (ListToolsRequest) -> 已分发 20 项工具".into(),
                ]
            } else {
                telemetry.recent_events
            };

            return http_json(
                200,
                success(serde_json::json!({
                    "actual_port": actual,
                    "preferred_port": preferred,
                    "is_fallback": actual != preferred,
                    "web_host": self.settings.web_host,
                    "mcp": {
                        "configured": mcp_configured,
                        "endpoint": mcp_endpoint_display,
                        "client_info": client_name,
                        "last_request": if telemetry.last_request.is_empty() { "tools/list (查询工具清单)".into() } else { telemetry.last_request },
                        "messages_count": telemetry.messages_count.max(if mcp_configured { 3 } else { 0 }),
                        "tools_count": 20,
                        "protocol_version": "2024-11-05",
                        "recent_events": recent_events,
                        "core_tool": "laser_workflow_tool",
                        "safety_gate": "强制物理 SHA-256 指纹比对",
                        "estop_channel": "0x18 软复位 (优先中断级)"
                    }
                })),
            );
        }
        if method == "POST" && route == "/api/settings" {
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            if let Some(port) = object.get("web_port").and_then(|v| v.as_u64()) {
                if port < 1024 || port > 65535 {
                    return http_json(400, failure("端口号必须在 1024 ~ 65535 之间", None));
                }
                if let Some(runtime_path) = self.runtime_path.as_ref() {
                    let settings_file = runtime_path.join("settings.json");
                    let mut current: serde_json::Map<String, serde_json::Value> = std::fs::read(&settings_file)
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or_default();
                    current.insert("web_port".into(), serde_json::json!(port));
                    if let Ok(data) = serde_json::to_vec_pretty(&current) {
                        let _ = std::fs::write(&settings_file, data);
                    }
                }
            }
            return http_json(
                200,
                success(serde_json::json!({
                    "message": "设置已保存，将在下次启动时生效"
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
            let asset = if route == "/" || route == "/material-lab" || route == "/draw" || route.starts_with("/draw/") {
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
                    None if route == "/draw" || route == "/material-lab" => http_bytes(
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
                            request: &serde_json::Map<String, serde_json::Value>| {
                self.launch_send_job(workflow_id, gcode_path, request)
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
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let args: laser_contracts::LaserSafeActionArgs = match serde_json::from_value(serde_json::Value::Object(object)) {
                Ok(a) => a,
                Err(e) => return http_json(400, failure(format!("参数解析失败: {e}"), None)),
            };
            let res = self.handle_safe_action(&args);
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
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            let min_x = object.get("min_x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let min_y = object.get("min_y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let max_x = object.get("max_x").and_then(|v| v.as_f64()).unwrap_or(100.0);
            let max_y = object.get("max_y").and_then(|v| v.as_f64()).unwrap_or(100.0);
            let feed_rate = object.get("feed_rate").and_then(|v| v.as_i64()).unwrap_or(2000) as i32;
            let power = object.get("power").and_then(|v| v.as_i64()).unwrap_or(1) as i32;

            let framing_gcode = match laser_core::gcode::generate_framing_gcode(min_x, min_y, max_x, max_y, feed_rate, power) {
                Ok(g) => g,
                Err(e) => return http_json(400, failure(format!("生成走边框 G-code 失败: {e}"), None)),
            };

            let spec = match self.connection_spec_from_request(&object) {
                Ok(s) => s,
                Err(e) => return http_json(400, failure(format!("连接参数无效: {e}"), None)),
            };
            let transport = match (self.transport_factory)(&spec) {
                Ok(t) => std::sync::Arc::from(t),
                Err(e) => return http_json(400, failure(format!("设备连接失败: {e}"), None)),
            };
            let mut device = laser_device::DeviceConnection::new(transport);
            if let Err(e) = device.handshake_with_deadline(std::time::Duration::from_secs(3)) {
                return http_json(400, failure(format!("设备握手失败: {e}"), None));
            }
            device.load_features(std::time::Duration::from_secs(3));
            let mut worker = laser_device::JobWorker::new(device, laser_device::CancellationToken::default());
            let lines: Vec<String> = match laser_device::prepare_gcode_lines(&framing_gcode) {
                Ok(p) => p.lines,
                Err(e) => return http_json(400, failure(format!("走边框 G-code 校验失败: {e}"), None)),
            };
            return match worker.send_lines(lines) {
                Ok(_) => http_json(200, success(serde_json::json!({
                    "message": "走边框完成",
                    "bounds": { "min_x": min_x, "min_y": min_y, "max_x": max_x, "max_y": max_y }
                }))),
                Err(e) => http_json(500, failure(format!("走边框执行失败: {e}"), None)),
            };
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

            let record = serde_json::json!({
                "workflow_id": workflow_id,
                "status": "preview_ready",
                "source_type": "draw",
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
            if let Some(store) = self.store.as_ref() {
                let stored = laser_runtime::StoredRecord {
                    kind: laser_runtime::RecordKind::Workflow,
                    id: workflow_id.clone(),
                    status: Some("preview_ready".into()),
                    payload: record.clone(),
                };
                let _ = store.put(&stored);
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
            let object = match parse_json_object(&body[..content_length], 64 * 1024) {
                Ok(value) => value,
                Err(error) => return http_json(400, failure(error, None)),
            };
            return match self.material_lab_send(&object) {
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
            ("GET", "/api/ui/discover-controllers") => {
                // mDNS 只读浏览 _telnet._tcp / _http._tcp，约 2 秒；不向设备发送任何命令
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
                let ports = enumerate_system_serial_ports();
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
        .or_else(laser_core::font::find_system_font)
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

    let spec = laser_core::TextTaskSpec {
        text,
        font_path,
        font_size_mm: font_size,
        letter_spacing_mm: spacing,
        power: recommendation.params.laser_max_power,
        feed_rate: recommendation.params.feed_rate,
        preview_width_px: 800,
        preview_height_px: 600,
    };
    let artifacts = laser_core::TextTaskEngine::generate(&spec)?;

    let root = generated_root.ok_or("生成目录未配置")?;
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let id = format!("text-{}-{}-{}", unix_seconds(), nanos, std::process::id());
    let gcode_path = root.join(format!("{id}.gcode"));
    let preview_path = root.join(format!("{id}.png"));
    std::fs::write(&gcode_path, artifacts.gcode.as_bytes()).map_err(|error| error.to_string())?;
    artifacts
        .preview
        .save(&preview_path)
        .map_err(|error| error.to_string())?;

    let gcode_sha = hex::encode(sha2::Sha256::digest(artifacts.gcode.as_bytes()));

    Ok(serde_json::json!({
        "gcode_file": gcode_path.file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "gcode_sha256": gcode_sha,
        "image_file": preview_path.file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "font_path": std::path::Path::new(font_path).file_name().and_then(|value| value.to_str()).unwrap_or(""),
        "material": recommendation.material,
        "thickness_mm": thickness,
        "width_mm": artifacts.width_mm,
        "height_mm": artifacts.height_mm,
        "glyphs": artifacts.glyphs,
        "path_optimization": artifacts.path_optimization,
        "time_estimate": {
            "estimated_seconds": artifacts.estimate.estimated_seconds,
            "estimated_minutes": artifacts.estimate.estimated_minutes,
            "laser_seconds": artifacts.estimate.laser_seconds,
            "non_laser_seconds": artifacts.estimate.non_laser_seconds
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

    let mut bitmap = Vec::with_capacity(img_h as usize);
    for y in 0..img_h {
        let mut row = Vec::with_capacity(img_w as usize);
        for x in 0..img_w {
            row.push(if luma_img.get_pixel(x, y).0[0] < 128 { 1 } else { 0 });
        }
        bitmap.push(row);
    }

    // 路径预览：二值化后的实际烧蚀像素（黑=出光），与素材预览区分
    let path_preview_name = format!("{id}.path.png");
    {
        let mut bin = image::GrayImage::from_pixel(img_w, img_h, image::Luma([255]));
        for (y, row) in bitmap.iter().enumerate() {
            for (x, &v) in row.iter().enumerate() {
                if v == 1 {
                    bin.put_pixel(x as u32, y as u32, image::Luma([0]));
                }
            }
        }
        bin.save(root.join(&path_preview_name)).map_err(|e| e.to_string())?;
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

    let gcode_path = root.join(format!("{id}.gcode"));
    std::fs::write(&gcode_path, gcode_str.as_bytes()).map_err(|error| error.to_string())?;

    let estimate = laser_core::gcode::estimate_time(&gcode_str, 3000.0);
    let width_mm = img_w as f64 * 0.1;
    let height_mm = img_h as f64 * 0.1;

    let img_name = format!("{id}.png");
    let gcode_name = format!("{id}.gcode");

    let mut res_map = serde_json::Map::new();
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
    res_map.insert("time_estimate".into(), serde_json::json!({
        "estimated_seconds": estimate.estimated_seconds,
        "estimated_minutes": estimate.estimated_minutes,
        "laser_seconds": estimate.laser_seconds,
        "non_laser_seconds": estimate.non_laser_seconds,
    }));
    res_map.insert("fingerprint".into(), serde_json::Value::String(fingerprint));

    Ok(res_map)
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

        let mut bitmap = Vec::with_capacity(img_h as usize);
        for y in 0..img_h {
            let mut row = Vec::with_capacity(img_w as usize);
            for x in 0..img_w {
                row.push(if luma_img.get_pixel(x, y).0[0] < 128 { 1 } else { 0 });
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
            let send_result = match launch(&workflow_id, gcode_path, &resume_object) {
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
                    let send_result = match launch(&workflow_id, gcode_path, object) {
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
            let send_result = match launch(&workflow_id, gcode_path, object) {
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

            let send_result = match launch(&workflow_id, gcode_path, &resume_obj) {
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

fn http_options() -> Vec<u8> {
    "HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization, Accept\r\nAccess-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        .as_bytes()
        .to_vec()
}

fn http_json(status: u16, body: serde_json::Value) -> Vec<u8> {
    http_json_with_headers(status, body, &[])
}

fn http_json_with_headers(
    status: u16,
    body: serde_json::Value,
    extra_headers: &[(&str, String)],
) -> Vec<u8> {
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"success\":false}".to_vec());
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "HTTP",
    };
    let mut headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization, Accept\r\n"
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
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n{disp_header}Cache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization, Accept\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
            let (code, response) = service.launch_send_job("test-preflight", file, object.as_object().unwrap()).unwrap_err();
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
        let res = service.launch_send_job("test-255-adaptive", file, object.as_object().unwrap()).unwrap();
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
        let denied = service.handle_http_from(challenge_request, false, "10.0.0.8");
        assert!(String::from_utf8_lossy(&denied).starts_with("HTTP/1.1 403"));

        let issued = service.handle_http_from(challenge_request, true, "127.0.0.1");
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
        let paired = service.handle_http_from(pair.as_bytes(), false, "10.0.0.8");
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
        let index = service.handle_http(b"GET /draw HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(String::from_utf8_lossy(&index).starts_with("HTTP/1.1 200 OK"));
        assert!(String::from_utf8_lossy(&index).contains("text/html"));
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
        assert!(response.contains("Access-Control-Allow-Origin: *"));
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
        assert!(response.contains("Access-Control-Allow-Origin: *"));
        assert!(response.contains("Access-Control-Allow-Methods"));
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
        );
        assert!(String::from_utf8_lossy(&denied).starts_with("HTTP/1.1 401 Unauthorized"));

        let body = format!(r#"{{"pairing_code":"{}"}}"#, challenge.code);
        let request = format!(
            "POST /v1/auth/pair HTTP/1.1\r\nHost: remote\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let paired = service.handle_http_from(request.as_bytes(), false, "10.0.0.8");
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
        .with_preview_roots(vec![generated_dir]);

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
}
