use laser_contracts::*;
use laser_device::{
    ConnectionSpec, DeviceConnection, DeviceTransport, RealtimeCommand, SendPreflight, authorize_send,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PRIORITY_TOOLS: [&str; 8] = [
    "check_laser_connection_tool",
    "laser_safe_action_tool",
    "laser_workflow_tool",
    "material_params_tool",
    "recommend_laser_params_tool",
    "run_calibration_grid_tool",
    "select_calibration_cell_tool",
    "start_tuned_job_tool",
];

const SECONDARY_TOOLS: [&str; 12] = [
    "ai_laser_gcode_tool",
    "command_execution_tool",
    "generate_text_image_gcode_tool",
    "generate_text_image_tool",
    "generate_text_laser_task_tool",
    "laser_grbl_tool",
    "laser_network_grbl_tool",
    "open_app_tool",
    "open_jsjds_website_tool",
    "refine_laser_params_from_feedback_tool",
    "regenerate_text_laser_task_tool",
    "shortcut_key_execution_tool",
];

pub const JSJDS_OFFICIAL_WEBSITE_URL: &str = "https://jsjds.blcu.edu.cn/index.htm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub developer_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCatalog {
    specs: Vec<ToolSpec>,
}

impl ToolCatalog {
    pub fn safe_default() -> Self {
        let mut specs = PRIORITY_TOOLS.iter()
            .map(|name| ToolSpec { name, developer_only: false })
            .collect::<Vec<_>>();
        specs.extend(SECONDARY_TOOLS.iter()
            .filter(|name| **name != "command_execution_tool")
            .map(|name| ToolSpec { name, developer_only: false }));
        Self { specs }
    }

    pub fn developer_mode() -> Self {
        let mut catalog = Self::safe_default();
        catalog.specs.push(ToolSpec { name: "command_execution_tool", developer_only: true });
        catalog.specs[8..].sort_by_key(|spec| spec.name);
        catalog
    }

    pub fn from_environment() -> Self {
        if developer_mode_enabled() { Self::developer_mode() } else { Self::safe_default() }
    }

    pub fn specs(&self) -> &[ToolSpec] { &self.specs }
    pub fn names(&self) -> Vec<&'static str> { self.specs.iter().map(|spec| spec.name).collect() }
}

/// 开发者命令开关：`MOSS_ENABLE_DEVELOPER_COMMANDS=true` 才开放 `command_execution_tool`。
pub fn developer_mode_enabled() -> bool {
    std::env::var("MOSS_ENABLE_DEVELOPER_COMMANDS")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

// ---------------------------------------------------------------------------
// 设备传输注入：生产走真实 TCP/串口，测试注入 FakeGrbl
// ---------------------------------------------------------------------------

/// 传输打开器：按连接描述与超时返回设备传输。默认实现为 `laser_device::open_transport`。
pub type TransportOpener =
    Arc<dyn Fn(&ConnectionSpec, Duration) -> Result<Box<dyn DeviceTransport>, String> + Send + Sync>;

pub fn default_transport_opener() -> TransportOpener {
    Arc::new(|spec: &ConnectionSpec, timeout: Duration| laser_device::open_transport(spec, timeout))
}

/// 从工具参数解析连接描述：显式 port → 串口；显式 host → 网络；都缺省时按环境变量
/// （`LASER_DEFAULT_CONNECTION_MODE` / `LASER_NETWORK_HOST` / `GRBL_DEFAULT_PORT`）兜底。
pub fn resolve_connection_spec(
    port: Option<&str>,
    host: Option<&str>,
    baudrate: Option<i32>,
    telnet_port: Option<i32>,
    connection_mode: Option<&str>,
) -> Result<ConnectionSpec, String> {
    let port = port.map(str::trim).filter(|v| !v.is_empty());
    let host = host.map(str::trim).filter(|v| !v.is_empty());
    let baud = baudrate.filter(|b| *b > 0).unwrap_or_else(|| {
        std::env::var("GRBL_BAUDRATE").ok().and_then(|v| v.parse().ok()).unwrap_or(115200)
    }) as u32;
    let telnet = telnet_port.filter(|p| (1..=65535).contains(p)).unwrap_or_else(|| {
        std::env::var("LASER_NETWORK_TELNET_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(23)
    }) as u16;
    let mode = connection_mode
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| m == "serial" || m == "network");
    let prefer_serial = match mode.as_deref() {
        Some("serial") => true,
        Some("network") => false,
        _ => {
            if port.is_some() && host.is_none() {
                true
            } else if host.is_some() {
                false
            } else {
                std::env::var("LASER_DEFAULT_CONNECTION_MODE")
                    .map(|v| v.eq_ignore_ascii_case("serial"))
                    .unwrap_or(false)
            }
        }
    };
    if prefer_serial {
        let port = port
            .map(str::to_owned)
            .or_else(|| std::env::var("GRBL_DEFAULT_PORT").ok().filter(|v| !v.trim().is_empty()))
            .ok_or("串口模式需要提供 port（或配置 GRBL_DEFAULT_PORT）")?;
        Ok(ConnectionSpec::Serial { port, baud_rate: baud })
    } else {
        let host = host
            .map(str::to_owned)
            .or_else(|| std::env::var("LASER_NETWORK_HOST").ok().filter(|v| !v.trim().is_empty() && !v.starts_with('<')))
            .ok_or("网络模式需要提供 host（或配置 LASER_NETWORK_HOST）")?;
        Ok(ConnectionSpec::Tcp { host, port: telnet })
    }
}

fn timeout_from(value: Option<f64>) -> Duration {
    Duration::from_secs_f64(value.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(5.0).clamp(0.5, 30.0))
}

// ---------------------------------------------------------------------------
// check_laser_connection_tool：只读握手探测（发 `$$`，不移动轴、不出光）
// ---------------------------------------------------------------------------

pub fn execute_check_laser_connection(args: &CheckLaserConnectionArgs) -> ToolCallResult {
    execute_check_laser_connection_with(args, &default_transport_opener())
}

pub fn execute_check_laser_connection_with(
    args: &CheckLaserConnectionArgs,
    opener: &TransportOpener,
) -> ToolCallResult {
    let spec = match resolve_connection_spec(
        args.port.as_deref(),
        args.host.as_deref(),
        args.baudrate,
        args.telnet_port,
        None,
    ) {
        Ok(spec) => spec,
        Err(error) => return ToolCallResult::err(format!("无法确定探测目标: {error}")),
    };
    let timeout = timeout_from(args.timeout);
    let label = if spec.mode() == "serial" { "串口" } else { "网络" };
    let target = spec.describe();
    let transport = match opener(&spec, timeout) {
        Ok(transport) => transport,
        Err(error) => {
            return ToolCallResult::with_detail(
                false,
                format!("{label}未连通 ({target})"),
                format!("{label} 连接失败: {error}"),
            );
        }
    };
    let mut device = DeviceConnection::new(transport);
    match device.handshake_with_deadline(timeout) {
        Ok(settings) => {
            let detail = if args.include_detail.unwrap_or(false) {
                format!("{label} 握手成功 ({target})，$32=1 激光模式就绪；固件设置 {} 行: {}", settings.len(), settings.join(" "))
            } else {
                format!("{label} 握手成功 ({target})，固件 $32=1 激光模式就绪")
            };
            ToolCallResult::with_detail(true, format!("{label}已连通 ({target})"), detail)
        }
        Err(error) => ToolCallResult::with_detail(
            false,
            format!("{label}已连接但未就绪 ({target})"),
            format!("{error}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// laser_safe_action_tool：单命令安全动作，真实写设备
// ---------------------------------------------------------------------------

pub fn execute_laser_safe_action(args: &LaserSafeActionArgs) -> ToolCallResult {
    execute_laser_safe_action_with(args, &default_transport_opener())
}

fn wait_status_report<T: DeviceTransport>(device: &DeviceConnection<T>, timeout: Duration) -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        match device.transport.read_line() {
            Ok(Some(line)) if line.starts_with('<') => return Some(line),
            Ok(Some(_)) => continue,
            Ok(None) => return None,
            Err(_) => continue,
        }
    }
    None
}

fn send_gcode_lines<T: DeviceTransport>(device: &mut DeviceConnection<T>, lines: &[String]) -> Result<(), String> {
    for line in lines {
        device.send_line_wait_ack(line)?;
    }
    Ok(())
}

pub fn execute_laser_safe_action_with(args: &LaserSafeActionArgs, opener: &TransportOpener) -> ToolCallResult {
    execute_laser_safe_action_with_gate(args, opener, &|| true)
}

/// The host can invalidate a queued request during device opening or read-only preflight.
/// The gate does not replace confirmation, strict-mode approval or device safety checks.
pub fn execute_laser_safe_action_with_gate(args: &LaserSafeActionArgs, opener: &TransportOpener, allowed: &dyn Fn() -> bool) -> ToolCallResult {
    let action = args.action.trim().to_ascii_lowercase();
    let confirmed = args.confirmed.unwrap_or(false);
    let dry_run = args.dry_run.unwrap_or(false);
    let feed = args.feed_rate.filter(|f| f.is_finite() && *f > 0.0).unwrap_or(1200.0);
    let x = args.x.unwrap_or(0.0);
    let y = args.y.unwrap_or(0.0);

    // 规划要写的顺序 G-code / 实时字节，先做门禁再碰设备
    enum Plan {
        Status,
        Realtime {
            command: RealtimeCommand,
            label: &'static str,
            alters_alarm_state: bool,
        },
        Lines(Vec<String>, String),
    }
    let plan = match action.as_str() {
        "status" => Plan::Status,
        "emergency_stop" => Plan::Realtime {
            command: RealtimeCommand::EmergencyStop,
            label: "急停 0x18",
            alters_alarm_state: true,
        },
        "soft_reset" => Plan::Realtime {
            command: RealtimeCommand::EmergencyStop,
            label: "软复位 0x18",
            alters_alarm_state: true,
        },
        // 进给速度倍率调节 (Feed Override)
        "feed_override_reset" | "feed_reset" | "feed_100" => Plan::Realtime {
            command: RealtimeCommand::FeedOvrReset,
            label: "进给速度恢复100% (0x90)",
            alters_alarm_state: false,
        },
        "feed_override_plus_10" | "feed_plus_10" | "feed_+10%" => Plan::Realtime {
            command: RealtimeCommand::FeedOvrCoarsePlus,
            label: "进给速度增加10% (0x91)",
            alters_alarm_state: false,
        },
        "feed_override_minus_10" | "feed_minus_10" | "feed_-10%" => Plan::Realtime {
            command: RealtimeCommand::FeedOvrCoarseMinus,
            label: "进给速度降低10% (0x92)",
            alters_alarm_state: false,
        },
        "feed_override_plus_1" | "feed_plus_1" | "feed_+1%" => Plan::Realtime {
            command: RealtimeCommand::FeedOvrFinePlus,
            label: "进给速度增加1% (0x93)",
            alters_alarm_state: false,
        },
        "feed_override_minus_1" | "feed_minus_1" | "feed_-1%" => Plan::Realtime {
            command: RealtimeCommand::FeedOvrFineMinus,
            label: "进给速度降低1% (0x94)",
            alters_alarm_state: false,
        },
        // 激光/主轴功率倍率调节 (Spindle/Laser Power Override)
        "spindle_override_reset" | "power_override_reset" | "power_reset" | "power_100" => Plan::Realtime {
            command: RealtimeCommand::SpindleOvrReset,
            label: "激光功率恢复100% (0x99)",
            alters_alarm_state: false,
        },
        "spindle_override_plus_10" | "power_override_plus_10" | "power_plus_10" | "power_+10%" => Plan::Realtime {
            command: RealtimeCommand::SpindleOvrCoarsePlus,
            label: "激光功率增加10% (0x9A)",
            alters_alarm_state: false,
        },
        "spindle_override_minus_10" | "power_override_minus_10" | "power_minus_10" | "power_-10%" => Plan::Realtime {
            command: RealtimeCommand::SpindleOvrCoarseMinus,
            label: "激光功率降低10% (0x9B)",
            alters_alarm_state: false,
        },
        "spindle_override_plus_1" | "power_override_plus_1" | "power_plus_1" | "power_+1%" => Plan::Realtime {
            command: RealtimeCommand::SpindleOvrFinePlus,
            label: "激光功率增加1% (0x9C)",
            alters_alarm_state: false,
        },
        "spindle_override_minus_1" | "power_override_minus_1" | "power_minus_1" | "power_-1%" => Plan::Realtime {
            command: RealtimeCommand::SpindleOvrFineMinus,
            label: "激光功率降低1% (0x9D)",
            alters_alarm_state: false,
        },
        "unlock" => Plan::Lines(vec!["$X".into()], "解锁 $X".into()),
        "home" => Plan::Lines(vec!["$H".into()], "回原点 $H".into()),
        "set_origin" => Plan::Lines(vec!["G92 X0 Y0".into()], "设置工作原点 G92 X0 Y0".into()),
        "laser_off" => Plan::Lines(vec!["M5".into(), "S0".into()], "关闭激光 M5 S0".into()),
        "absolute_move" => Plan::Lines(
            vec!["M5".into(), "G90".into(), format!("G0 X{x:.3} Y{y:.3} F{feed:.0}")],
            format!("绝对安全移动到 ({x:.3}, {y:.3}) F{feed:.0}（激光关闭）"),
        ),
        "relative_move" => Plan::Lines(
            vec!["M5".into(), "G91".into(), format!("G0 X{x:.3} Y{y:.3} F{feed:.0}"), "G90".into()],
            format!("相对安全移动 ({x:+.3}, {y:+.3}) F{feed:.0}（激光关闭）"),
        ),
        unknown => return ToolCallResult::err(format!("不支持的安全动作: {unknown}")),
    };
    let is_read_or_override = matches!(plan, Plan::Status | Plan::Realtime { alters_alarm_state: false, .. });
    if !is_read_or_override && !confirmed && !dry_run {
        return ToolCallResult::err(format!("动作 {action} 必须确认后方可执行 (confirmed=false)"));
    }
    let spec = match resolve_connection_spec(
        args.port.as_deref(),
        args.host.as_deref(),
        args.baudrate,
        args.telnet_port,
        args.connection_mode.as_deref(),
    ) {
        Ok(spec) => spec,
        Err(error) => return ToolCallResult::err(format!("无法确定设备: {error}")),
    };
    let target = spec.describe();
    if dry_run && !matches!(plan, Plan::Status) {
        let description = match &plan {
            Plan::Realtime { label, .. } => (*label).to_owned(),
            Plan::Lines(lines, label) => format!("{label}: {}", lines.join(" | ")),
            Plan::Status => String::new(),
        };
        return ToolCallResult::with_detail(true, format!("dry_run: 将向 {target} 发送 {description}"), "未连接设备，未发送任何字节");
    }
    if !allowed() { return ToolCallResult::err("语音设备授权已失效，操作未执行"); }
    let timeout = timeout_from(args.timeout);
    let transport = match opener(&spec, timeout) {
        Ok(transport) => transport,
        Err(error) => return ToolCallResult::err(format!("设备连接失败 ({target}): {error}")),
    };
    if !allowed() { return ToolCallResult::err("连接期间设备授权已失效，操作未执行"); }
    let mut device = DeviceConnection::new(transport);
    match plan {
        Plan::Status => {
            if let Err(error) = device.transport.write_realtime(RealtimeCommand::StatusQuery) {
                return ToolCallResult::err(format!("状态查询发送失败 ({target}): {error}"));
            }
            match wait_status_report(&device, timeout) {
                Some(report) => ToolCallResult::with_detail(true, format!("设备状态: {}", summarize_status(&report)), report),
                None => ToolCallResult::err(format!("{target} 在 {}ms 内未返回状态报告", timeout.as_millis())),
            }
        }
        Plan::Realtime { command, label, alters_alarm_state } => match device.transport.write_realtime(command) {
            Ok(()) => {
                if alters_alarm_state {
                    ToolCallResult::ok(format!("{label} 已发送到 {target}，控制器将进入 Alarm/复位态"))
                } else {
                    ToolCallResult::ok(format!("{label} 已实时发送到 {target}，即时生效（不停机）"))
                }
            }
            Err(error) => ToolCallResult::err(format!("{label} 发送失败 ({target}): {error}")),
        },
        Plan::Lines(lines, label) => {
            // 运动/解锁类顺序命令必须先握手：$32=1 门禁 + 设备可响应
            let needs_laser_mode = !matches!(action.as_str(), "unlock" | "laser_off");
            match device.handshake_with_deadline(timeout) {
                Ok(_) => {}
                Err(error) if needs_laser_mode => {
                    return ToolCallResult::err(format!("{label} 拒绝执行：{error}"));
                }
                Err(_) => {
                    // 解锁/关光允许在 Alarm 态下发；设备须至少有响应
                    if device.state() == laser_device::DeviceState::Disconnected {
                        return ToolCallResult::err(format!("{label} 拒绝执行：{target} 无 GRBL 响应"));
                    }
                    device.state = laser_device::DeviceState::Ready;
                }
            }
            // FluidNC：读取 $CD 以感知 after_unlock / after_homing 自动宏；旧 GRBL 静默跳过
            device.load_features(timeout);
            if !allowed() { return ToolCallResult::err("预检期间设备授权已失效，操作未执行"); }
            let outcome = match action.as_str() {
                // 解锁 / 回零：等 ok 后继续等到 Idle（FluidNC 会在其后执行自动宏，Home 状态视为忙碌）
                "unlock" => device.unlock(),
                "home" => device.home(),
                _ => send_gcode_lines(&mut device, &lines),
            };
            match outcome {
                Ok(()) => ToolCallResult::with_detail(true, format!("{label} 已执行 ({target})"), lines.join(" | ")),
                Err(error) => {
                    let _ = device.transport.write_realtime(RealtimeCommand::EmergencyStop);
                    let diagnostic = laser_device::diagnose_device_message(&error)
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default();
                    ToolCallResult::err(format!("{label} 执行失败 ({target})，已发送急停兜底: {error}{diagnostic}"))
                }
            }
        }
    }
}

fn summarize_status(report: &str) -> String {
    let inner = report.trim().trim_start_matches('<').trim_end_matches('>');
    let mut parts = inner.split('|');
    let state = parts.next().unwrap_or("Unknown");
    let pos = parts.find(|p| p.starts_with("MPos:") || p.starts_with("WPos:")).unwrap_or("");
    if pos.is_empty() { state.to_owned() } else { format!("{state} ({pos})") }
}

// ---------------------------------------------------------------------------
// 参数推荐 / 校准选格（本地校验；持久化由 host 后端承接）
// ---------------------------------------------------------------------------

pub fn execute_recommend_laser_params(args: &RecommendLaserParamsArgs) -> ToolCallResult {
    let scale = args.power_scale.unwrap_or(1000.0);
    match laser_core::material::recommend_with_scale(
        &args.material,
        args.thickness_mm,
        args.laser_mode.as_deref().unwrap_or("engrave"),
        args.engraving_mode.as_deref(),
        scale,
    ) {
        Some(rec) => {
            let msg = format!(
                "推荐参数: 材料={}, 匹配厚度={}mm, 模式={}, 功率={} (标尺={}), 速度={}",
                rec.material, rec.matched_thickness_mm, rec.laser_mode, rec.params.laser_max_power, scale as i64, rec.params.feed_rate
            );
            ToolCallResult::with_detail(
                true,
                msg,
                format!(
                    "laser_min_power={} laser_max_power={} feed_rate={} travel_rate={} passes={} pixel_size_mm={} engraving_mode={}",
                    rec.params.laser_min_power,
                    rec.params.laser_max_power,
                    rec.params.feed_rate,
                    rec.params.travel_rate,
                    rec.params.passes,
                    rec.params.pixel_size_mm,
                    rec.engraving_mode.as_deref().unwrap_or("")
                ),
            )
        }
        None => ToolCallResult::err(format!("未找到材料 {} (厚度 {}mm) 的推荐参数", args.material, args.thickness_mm)),
    }
}

/// 参数校验；无 host 后端时明确告知未持久化，绝不伪装成功。
pub fn execute_select_calibration_cell(args: &SelectCalibrationCellArgs) -> ToolCallResult {
    if args.calibration_id.trim().is_empty() {
        return ToolCallResult::err("calibration_id 不能为空");
    }
    if args.cell_number < 1 {
        return ToolCallResult::err("cell_number 必须大于等于 1");
    }
    ToolCallResult::err(format!(
        "select_calibration_cell 需要 moss-host 后端持久化（calibration {} 单元格 #{}），当前 MCP 会话未连接 host",
        args.calibration_id, args.cell_number
    ))
}

// ---------------------------------------------------------------------------
// 桌面/系统工具：真实执行（原 Python 行为的 Rust 移植）
// ---------------------------------------------------------------------------

struct CommandOutput {
    success: bool,
    code: i32,
    stdout: String,
    stderr: String,
}

fn execute_command_with_runner(
    args: &CommandExecutionArgs,
    developer_mode: bool,
    runner: impl FnOnce(&str) -> Result<CommandOutput, String>,
) -> ToolCallResult {
    if !developer_mode {
        return ToolCallResult::err("command_execution_tool 仅在本地开发者调试模式下可用");
    }
    let cmd = args.command.trim();
    if cmd.is_empty() {
        return ToolCallResult::err("command 不能为空");
    }
    match runner(cmd) {
        Ok(output) => {
            let mut text = output.stdout.trim_end().to_owned();
            if !output.stderr.trim().is_empty() {
                if !text.is_empty() { text.push('\n'); }
                text.push_str("[stderr] ");
                text.push_str(output.stderr.trim_end());
            }
            const LIMIT: usize = 8000;
            if text.len() > LIMIT {
                let mut cut = LIMIT;
                while !text.is_char_boundary(cut) { cut -= 1; }
                text.truncate(cut);
                text.push_str("\n…(输出已截断)");
            }
            ToolCallResult::with_detail(output.success, format!("命令退出码 {}: {cmd}", output.code), text)
        }
        Err(error) => ToolCallResult::err(format!("命令启动失败: {error}")),
    }
}

pub fn execute_command(args: &CommandExecutionArgs, developer_mode: bool) -> ToolCallResult {
    execute_command_with_runner(args, developer_mode, |cmd| {
        let output = if cfg!(windows) {
            std::process::Command::new("cmd").args(["/C", cmd]).output()
        } else {
            std::process::Command::new("sh").args(["-c", cmd]).output()
        }
        .map_err(|error| error.to_string())?;
        Ok(CommandOutput {
            success: output.status.success(),
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    })
}

fn open_with_system(target: &str) -> Result<(), String> {
    let status = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", "", target]).status()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(target).status()
    } else {
        std::process::Command::new("xdg-open").arg(target).status()
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("系统打开命令退出码 {}", status.code().unwrap_or(-1))),
        Err(error) => Err(error.to_string()),
    }
}

fn resolve_app_alias(argument: &str) -> String {
    resolve_app_alias_for(std::env::consts::OS, argument)
}

fn resolve_app_alias_for(os: &str, argument: &str) -> String {
    let normalized = argument.replace(' ', "").to_lowercase();
    if os == "macos" {
        return match normalized.as_str() {
            "terminal" | "终端" => "/System/Applications/Utilities/Terminal.app",
            "微信" | "wechat" => "/Applications/WeChat.app",
            "企业微信" => "/Applications/企业微信.app",
            "浏览器" | "safari" => "/Applications/Safari.app",
            "chrome" => "/Applications/Google Chrome.app",
            "edge" => "/Applications/Microsoft Edge.app",
            "vscode" => "/Applications/Visual Studio Code.app",
            _ => argument,
        }
        .to_owned();
    }
    if os != "windows" {
        return match normalized.as_str() {
            "terminal" | "终端" => "x-terminal-emulator",
            "浏览器" | "chrome" => "google-chrome",
            "vscode" => "code",
            _ => argument,
        }
        .to_owned();
    }
    match normalized.as_str() {
        "terminal" | "cmd" | "终端" => "cmd.exe",
        "powershell" => "powershell.exe",
        "微信" | "wechat" => r"C:\Program Files (x86)\Tencent\WeChat\WeChat.exe",
        "企业微信" => r"C:\Program Files (x86)\WXWork\WXWork.exe",
        "浏览器" | "chrome" => r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        "edge" => r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        "vscode" => "code",
        "lasergrbl" => "LaserGRBL",
        _ => argument,
    }
    .to_owned()
}

pub fn execute_open_app(args: &OpenAppArgs) -> ToolCallResult {
    let app = args.argument.trim();
    if app.is_empty() {
        return ToolCallResult::err("argument 不能为空");
    }
    let normalized = app.replace(' ', "");
    if normalized.contains("计算机设计大赛") || normalized.eq_ignore_ascii_case("4c") || normalized == "打开4c" {
        return execute_open_jsjds_website();
    }
    let target = resolve_app_alias(app);
    let payload = args.payload.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let result = match payload {
        Some(payload) => {
            let status = if cfg!(target_os = "windows") {
                std::process::Command::new("cmd").args(["/C", "start", "", &target, payload]).status()
            } else if cfg!(target_os = "macos") {
                std::process::Command::new("open").args(["-a", &target, payload]).status()
            } else {
                std::process::Command::new(&target).arg(payload).status()
            };
            status.map_err(|e| e.to_string()).and_then(|s| if s.success() { Ok(()) } else { Err(format!("退出码 {}", s.code().unwrap_or(-1))) })
        }
        None => open_with_system(&target),
    };
    match result {
        Ok(()) => ToolCallResult::ok(format!("已启动应用: {app}")),
        Err(error) => ToolCallResult::err(format!("启动应用 {app} 失败: {error}")),
    }
}

pub fn execute_open_jsjds_website() -> ToolCallResult {
    match open_with_system(JSJDS_OFFICIAL_WEBSITE_URL) {
        Ok(()) => ToolCallResult::with_detail(true, "已打开中国大学生计算机设计大赛官网", JSJDS_OFFICIAL_WEBSITE_URL),
        Err(error) => ToolCallResult::err(format!("打开官网失败: {error}")),
    }
}

const SHORTCUT_ACTIONS: [(&str, &str); 6] = [
    ("播放音乐", "^ {ENTER}"),
    ("暂停音乐", "^ {ENTER}"),
    ("切换下一首", "^ {RIGHT}"),
    ("切换上一首", "^ {LEFT}"),
    ("音乐音量加", "^ {UP}"),
    ("音乐音量减", "^ {DOWN}"),
];

fn macos_shortcut_script(action: &str) -> Option<&'static str> {
    match action {
        "播放音乐" | "暂停音乐" => Some(r#"tell application "Music" to playpause"#),
        "切换下一首" => Some(r#"tell application "Music" to next track"#),
        "切换上一首" => Some(r#"tell application "Music" to previous track"#),
        "音乐音量加" => Some(
            "set v to output volume of (get volume settings)\nif v > 90 then\nset volume output volume 100\nelse\nset volume output volume (v + 10)\nend if",
        ),
        "音乐音量减" => Some(
            "set v to output volume of (get volume settings)\nif v < 10 then\nset volume output volume 0\nelse\nset volume output volume (v - 10)\nend if",
        ),
        _ => None,
    }
}

pub fn execute_shortcut_key(args: &ShortcutKeyExecutionArgs) -> ToolCallResult {
    let action = args.action.trim();
    if action.is_empty() {
        return ToolCallResult::err("action 不能为空");
    }
    let Some((_, keys)) = SHORTCUT_ACTIONS.iter().find(|(name, _)| *name == action) else {
        let supported = SHORTCUT_ACTIONS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ");
        return ToolCallResult::err(format!("不支持的快捷键操作: {action}，支持的操作: {supported}"));
    };
    if cfg!(target_os = "windows") {
        let script = format!("(New-Object -COM WScript.Shell).SendKeys(\"{keys}\")");
        return match std::process::Command::new("powershell").args(["-NoProfile", "-Command", &script]).status() {
            Ok(status) if status.success() => ToolCallResult::ok(format!("快捷键动作已执行: {action}")),
            Ok(status) => ToolCallResult::err(format!("快捷键 {action} 执行失败，退出码 {}", status.code().unwrap_or(-1))),
            Err(error) => ToolCallResult::err(format!("快捷键 {action} 启动 PowerShell 失败: {error}")),
        };
    }
    if cfg!(target_os = "macos") {
        let Some(script) = macos_shortcut_script(action) else {
            return ToolCallResult::err(format!("不支持的快捷键操作: {action}"));
        };
        return match std::process::Command::new("osascript").args(["-e", script]).status() {
            Ok(status) if status.success() => ToolCallResult::ok(format!("快捷键动作已执行: {action}")),
            Ok(status) => ToolCallResult::err(format!("快捷键 {action} 执行失败，退出码 {}", status.code().unwrap_or(-1))),
            Err(error) => ToolCallResult::err(format!("快捷键 {action} 启动 osascript 失败: {error}")),
        };
    }
    ToolCallResult::err("快捷键执行当前仅支持 Windows 与 macOS")
}

// ---------------------------------------------------------------------------
// generate_text_image_tool：真实渲染并落盘 PNG
// ---------------------------------------------------------------------------

pub fn execute_generate_text_image(args: &GenerateTextImageArgs) -> ToolCallResult {
    let text = args.text.trim();
    if text.is_empty() {
        return ToolCallResult::err("text 不能为空");
    }
    let width = args.width.filter(|v| *v > 0).unwrap_or(1024) as u32;
    let height = args.height.filter(|v| *v > 0).unwrap_or(1024) as u32;
    let font_size = args.font_size.filter(|v| *v > 0).unwrap_or(320) as f64;
    let font_path = args
        .font_path
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| laser_core::font::find_system_font_for_text(text));
    let font_data = match font_path.as_ref() {
        Some(path) => match std::fs::read(path) {
            Ok(data) => data,
            Err(error) => return ToolCallResult::err(format!("字体读取失败 {}: {error}", path.display())),
        },
        None => include_bytes!("../../../tests/fixtures/fonts/geometry.ttf").to_vec(),
    };
    // 以像素为单位排版：font_size 视为字高像素
    let layout = match laser_core::font::layout_text(&font_data, 0, text, font_size, font_size * 0.05) {
        Ok(layout) => layout,
        Err(error) => return ToolCallResult::err(format!("字体排版失败: {error}")),
    };
    let image = match laser_core::gcode::preview_outline_rgba(&layout.paths, width, height, layout.width.max(1.0), layout.height.max(1.0)) {
        Ok(image) => image,
        Err(error) => return ToolCallResult::err(format!("文字图片渲染失败: {error}")),
    };
    let output = args
        .output_file
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("generated_images").join("text_image.png"));
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                return ToolCallResult::err(format!("创建输出目录失败: {error}"));
            }
        }
    }
    match image.save(&output) {
        Ok(()) => ToolCallResult::with_detail(
            true,
            format!("文字图片已生成: {}", output.display()),
            format!("尺寸={}x{}px, 字数={}, 字体={}", width, height, layout.glyphs, font_path.map(|p| p.display().to_string()).unwrap_or_else(|| "内置几何字体".into())),
        ),
        Err(error) => ToolCallResult::err(format!("保存文字图片失败: {error}")),
    }
}

// ---------------------------------------------------------------------------
// laser_workflow_tool：本地门禁；真实 workflow 状态/发送由 host 后端承接
// ---------------------------------------------------------------------------

pub fn execute_laser_workflow(
    action: Option<&str>,
    workflow_id: Option<&str>,
    confirmed: bool,
) -> ToolCallResult {
    execute_laser_workflow_with_preflight(action, workflow_id, confirmed, None)
}

/// 无 host 后端时的兜底：只做参数与门禁校验，任何“已发送/已入队”文案都不再伪造。
pub fn execute_laser_workflow_with_preflight(
    action: Option<&str>,
    workflow_id: Option<&str>,
    confirmed: bool,
    preflight: Option<&SendPreflight>,
) -> ToolCallResult {
    let act = action.unwrap_or("status");
    match act {
        "status" | "preview" | "cancel" | "recent" | "feedback" | "regenerate" => ToolCallResult::err(format!(
            "workflow {act} 需要 moss-host 后端（workflow_id: {}），当前 MCP 会话未连接 host",
            workflow_id.unwrap_or("none")
        )),
        "confirm_send" => {
            if !confirmed {
                return ToolCallResult::err("发送动作未确认 (confirmed=false)");
            }
            let Some(id) = workflow_id.filter(|s| !s.trim().is_empty()) else {
                return ToolCallResult::err("confirm_send 缺少 workflow_id");
            };
            if let Some(pf) = preflight {
                if let Err(e) = validate_confirm_send(pf) {
                    return ToolCallResult::err(format!("发送安全门禁未通过: {e}"));
                }
                return ToolCallResult::err(format!("workflow {id} 门禁通过，但当前 MCP 会话未连接 host，未发送"));
            }
            ToolCallResult::err(format!("workflow {id} confirm_send 需要 moss-host 后端执行物理 SHA 门禁与真实下发"))
        }
        other => ToolCallResult::err(format!("未知工作流动作: {other}")),
    }
}

pub fn validate_confirm_send(preflight: &SendPreflight) -> Result<(), String> {
    authorize_send(preflight)
}

#[cfg(test)]
mod tests {
    use super::*;
    use laser_device::FakeGrbl;
    use std::sync::Mutex;

    #[test]
    fn safe_catalog_keeps_priority_order_and_hides_commands() {
        let catalog = ToolCatalog::safe_default();
        assert_eq!(&catalog.names()[..8], &PRIORITY_TOOLS);
        assert!(!catalog.names().contains(&"command_execution_tool"));
        assert_eq!(catalog.specs().len(), 19);
    }

    #[test]
    fn developer_mode_adds_command_only_after_priority_tools() {
        let catalog = ToolCatalog::developer_mode();
        assert_eq!(&catalog.names()[..8], &PRIORITY_TOOLS);
        assert_eq!(catalog.specs().len(), 20);
        assert!(catalog.specs().iter().any(|spec| spec.name == "command_execution_tool" && spec.developer_only));
    }

    /// 共享句柄：让同一个 FakeGrbl 既交给工具使用，又留在测试里做断言。
    struct SharedFake(Arc<FakeGrbl>);
    impl DeviceTransport for SharedFake {
        fn write_line(&self, line: &str) -> Result<(), String> { self.0.write_line(line) }
        fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> { self.0.write_realtime(command) }
        fn read_line(&self) -> Result<Option<String>, String> { self.0.read_line() }
        fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> { self.0.raw_abort_handle() }
    }

    /// 记录每次打开的连接描述，并把同一个 FakeGrbl 句柄交给工具，便于断言写入的字节。
    fn fake_opener(responses: Vec<&str>) -> (TransportOpener, Arc<Mutex<Vec<String>>>, Arc<FakeGrbl>) {
        let fake = Arc::new(FakeGrbl::new(responses.into_iter().map(str::to_owned)));
        let targets = Arc::new(Mutex::new(Vec::new()));
        let fake_for_opener = fake.clone();
        let targets_for_opener = targets.clone();
        let opener: TransportOpener = Arc::new(move |spec: &ConnectionSpec, _timeout: Duration| {
            targets_for_opener.lock().unwrap().push(spec.describe());
            Ok(Box::new(SharedFake(fake_for_opener.clone())) as Box<dyn DeviceTransport>)
        });
        (opener, targets, fake)
    }

    fn failing_opener() -> TransportOpener {
        Arc::new(|_spec: &ConnectionSpec, _timeout: Duration| Err("connection refused".to_owned()))
    }

    #[test]
    fn check_connection_really_handshakes_and_reports_failures() {
        let (opener, targets, fake) = fake_opener(vec!["$0=10", "$32=1", "ok"]);
        let args = CheckLaserConnectionArgs { host: Some("192.0.2.10".into()), telnet_port: Some(23), ..Default::default() };
        let ok = execute_check_laser_connection_with(&args, &opener);
        assert!(ok.success, "{ok:?}");
        assert!(ok.result.contains("192.0.2.10:23"));
        assert_eq!(fake.lines(), vec!["$$".to_string()], "探测只允许发送 $$");
        assert_eq!(targets.lock().unwrap().as_slice(), ["192.0.2.10:23"]);

        // $32=0：已连接但未就绪
        let (opener, _, _) = fake_opener(vec!["$32=0", "ok"]);
        let not_ready = execute_check_laser_connection_with(&args, &opener);
        assert!(!not_ready.success);
        assert!(not_ready.result.contains("未就绪"), "{not_ready:?}");

        // 打不开连接
        let refused = execute_check_laser_connection_with(&args, &failing_opener());
        assert!(!refused.success);
        assert!(refused.detail.as_deref().unwrap_or("").contains("connection refused"));

        // 串口参数
        let (opener, targets, _) = fake_opener(vec!["$32=1", "ok"]);
        let serial = execute_check_laser_connection_with(
            &CheckLaserConnectionArgs { port: Some("COM7".into()), baudrate: Some(250000), ..Default::default() },
            &opener,
        );
        assert!(serial.success, "{serial:?}");
        assert_eq!(targets.lock().unwrap().as_slice(), ["COM7@250000"]);
    }

    #[test]
    fn safe_actions_gate_confirmation_and_write_real_bytes() {
        let host_args = |action: &str, confirmed: bool| LaserSafeActionArgs {
            action: action.into(),
            confirmed: Some(confirmed),
            host: Some("127.0.0.1".into()),
            telnet_port: Some(23),
            ..Default::default()
        };
        // 未确认：不碰设备
        let (opener, targets, _) = fake_opener(vec![]);
        let unconfirmed = execute_laser_safe_action_with(&host_args("emergency_stop", false), &opener);
        assert!(!unconfirmed.success);
        assert!(targets.lock().unwrap().is_empty(), "未确认动作不得打开设备");

        // 急停：实时信道写 0x18
        let (opener, _, fake) = fake_opener(vec![]);
        let stop = execute_laser_safe_action_with(&host_args("emergency_stop", true), &opener);
        assert!(stop.success, "{stop:?}");
        assert_eq!(fake.realtime(), vec![0x18]);
        assert!(fake.lines().is_empty());

        // 绝对移动：握手后写 M5/G90/G0，激光关闭
        // 响应：握手($32=1, ok) → $CD(ok, 非 FluidNC 视为空配置) → M5/G90/G0 各一个 ok
        let (opener, _, fake) = fake_opener(vec!["$32=1", "ok", "ok", "ok", "ok", "ok"]);
        let mut mv = host_args("absolute_move", true);
        mv.x = Some(10.0);
        mv.y = Some(5.5);
        mv.feed_rate = Some(800.0);
        let moved = execute_laser_safe_action_with(&mv, &opener);
        assert!(moved.success, "{moved:?}");
        assert_eq!(fake.lines(), vec!["$$", "$CD", "M5", "G90", "G0 X10.000 Y5.500 F800"]);

        // $32=0 时运动动作拒绝，且不写运动指令
        let (opener, _, fake) = fake_opener(vec!["$32=0", "ok"]);
        let refused = execute_laser_safe_action_with(&host_args("home", true), &opener);
        assert!(!refused.success);
        assert_eq!(fake.lines(), vec!["$$"]);

        // 设备返回 error：失败并急停兜底
        let (opener, _, fake) = fake_opener(vec!["$32=1", "ok", "ok", "error:9"]);
        let failed = execute_laser_safe_action_with(&host_args("set_origin", true), &opener);
        assert!(!failed.success);
        assert_eq!(fake.realtime(), vec![0x18]);

        // dry_run：描述将发送的内容，不打开设备
        let (opener, targets, _) = fake_opener(vec![]);
        let mut dry = host_args("relative_move", false);
        dry.dry_run = Some(true);
        let described = execute_laser_safe_action_with(&dry, &opener);
        assert!(described.success);
        assert!(described.result.contains("G91"), "{described:?}");
        assert!(targets.lock().unwrap().is_empty());

        // status：写 `?` 并解析状态报告
        let (opener, _, fake) = fake_opener(vec!["<Idle|MPos:1.000,2.000,0.000|FS:0,0>"]);
        let status = execute_laser_safe_action_with(&host_args("status", false), &opener);
        assert!(status.success, "{status:?}");
        assert!(status.result.contains("Idle"));
        assert_eq!(fake.realtime(), vec![b'?']);

        // 进给与功率倍率调节：实时单字节直接下发
        let (opener, _, fake) = fake_opener(vec![]);
        let feed_plus = execute_laser_safe_action_with(&host_args("feed_plus_10", false), &opener);
        assert!(feed_plus.success, "{feed_plus:?}");
        assert_eq!(fake.realtime(), vec![0x91]);
        assert!(feed_plus.result.contains("不停机"));

        let (opener, _, fake) = fake_opener(vec![]);
        let power_minus = execute_laser_safe_action_with(&host_args("power_minus_10", false), &opener);
        assert!(power_minus.success, "{power_minus:?}");
        assert_eq!(fake.realtime(), vec![0x9B]);
        assert!(power_minus.result.contains("不停机"));
    }

    #[test]
    fn recommend_and_local_fallbacks_do_not_fake_success() {
        let rec = execute_recommend_laser_params(&RecommendLaserParamsArgs {
            material: "椴木".into(),
            thickness_mm: 3.0,
            laser_mode: Some("engrave".into()),
            engraving_mode: Some("raster".into()),
            power_scale: None,
        });
        assert!(rec.success);

        let rec_255 = execute_recommend_laser_params(&RecommendLaserParamsArgs {
            material: "椴木".into(),
            thickness_mm: 3.0,
            laser_mode: Some("engrave".into()),
            engraving_mode: Some("raster".into()),
            power_scale: Some(255.0),
        });
        assert!(rec_255.success);
        assert!(rec_255.result.contains("标尺=255"));

        let cmd = CommandExecutionArgs { command: "echo test".into() };
        assert!(!execute_command(&cmd, false).success);
        let ran = execute_command_with_runner(&cmd, true, |_command| {
            Ok(CommandOutput {
                success: true,
                code: 0,
                stdout: "test\n".into(),
                stderr: String::new(),
            })
        });
        assert!(ran.success, "{ran:?}");
        assert!(ran.detail.as_deref().unwrap_or("").contains("test"));

        // 无后端：workflow 与选格都明确失败而不是伪装入队
        assert!(!execute_laser_workflow(Some("confirm_send"), Some("wf_1"), false).success);
        let no_backend = execute_laser_workflow(Some("confirm_send"), Some("wf_1"), true);
        assert!(!no_backend.success);
        assert!(no_backend.result.contains("moss-host"));
        assert!(!execute_laser_workflow(Some("confirm_send"), None, true).success);
        assert!(!execute_select_calibration_cell(&SelectCalibrationCellArgs { calibration_id: "cal".into(), cell_number: 2, ..Default::default() }).success);

        let bad_preflight = SendPreflight {
            status: "running".into(),
            expected_sha256: "0".repeat(64),
            actual_sha256: Some("0".repeat(64)),
            gcode_path: None,
            workflow_snapshot_valid: true,
            calibration_sha_valid: false,
        };
        let wf_bad_gate = execute_laser_workflow_with_preflight(Some("confirm_send"), Some("wf_1"), true, Some(&bad_preflight));
        assert!(!wf_bad_gate.success);
        assert!(wf_bad_gate.result.contains("门禁"));

        let unsupported = execute_shortcut_key(&ShortcutKeyExecutionArgs { action: "打开灯".into() });
        assert!(!unsupported.success);

        assert_eq!(resolve_app_alias_for("macos", "微信"), "/Applications/WeChat.app");
        assert_eq!(resolve_app_alias_for("macos", "终端"), "/System/Applications/Utilities/Terminal.app");
        assert_eq!(resolve_app_alias_for("windows", "终端"), "cmd.exe");
        for (name, _) in SHORTCUT_ACTIONS {
            assert!(macos_shortcut_script(name).is_some(), "missing macOS script for {name}");
        }
    }

    #[test]
    fn text_image_tool_writes_png() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("nested").join("hello.png");
        let result = execute_generate_text_image(&GenerateTextImageArgs {
            text: "MOSS".into(),
            width: Some(256),
            height: Some(128),
            font_size: Some(48),
            output_file: Some(output.to_string_lossy().into_owned()),
            ..Default::default()
        });
        assert!(result.success, "{result:?}");
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }
}
