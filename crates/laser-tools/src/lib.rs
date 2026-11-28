use laser_contracts::*;
use laser_device::{
    ConnectionSpec, DeviceConnection, DeviceTransport, RealtimeCommand, SendPreflight,
    authorize_send,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 核心激光工具（顺序与 `tests/golden/mcp_tools.json` 一致，常规与严格模式均注册）。
const LASER_TOOLS: [&str; 8] = [
    "check_laser_connection_tool",
    "laser_safe_action_tool",
    "laser_workflow_tool",
    "material_params_tool",
    "refine_laser_params_from_feedback_tool",
    "regenerate_text_laser_task_tool",
    "run_calibration_grid_tool",
    "select_calibration_cell_tool",
];

/// 本机桌面控制工具（R4，严格模式与内置语音不开放；`command_execution_tool` 仅开发者模式注册）。
const DESKTOP_TOOLS: [&str; 2] = ["open_app_tool", "command_execution_tool"];

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
        let mut specs = LASER_TOOLS
            .iter()
            .map(|name| ToolSpec {
                name,
                developer_only: false,
            })
            .collect::<Vec<_>>();
        specs.extend(
            DESKTOP_TOOLS
                .iter()
                .filter(|name| **name != "command_execution_tool")
                .map(|name| ToolSpec {
                    name,
                    developer_only: false,
                }),
        );
        Self { specs }
    }

    pub fn developer_mode() -> Self {
        let mut catalog = Self::safe_default();
        catalog.specs.push(ToolSpec {
            name: "command_execution_tool",
            developer_only: true,
        });
        catalog
    }

    pub fn from_environment() -> Self {
        if developer_mode_enabled() {
            Self::developer_mode()
        } else {
            Self::safe_default()
        }
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }
    pub fn names(&self) -> Vec<&'static str> {
        self.specs.iter().map(|spec| spec.name).collect()
    }
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
pub type TransportOpener = Arc<
    dyn Fn(&ConnectionSpec, Duration) -> Result<Box<dyn DeviceTransport>, String> + Send + Sync,
>;

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
        std::env::var("GRBL_BAUDRATE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(115200)
    }) as u32;
    let telnet = telnet_port
        .filter(|p| (1..=65535).contains(p))
        .unwrap_or_else(|| {
            std::env::var("LASER_NETWORK_TELNET_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(23)
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
            .or_else(|| {
                std::env::var("GRBL_DEFAULT_PORT")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .ok_or("串口模式需要提供 port（或配置 GRBL_DEFAULT_PORT）")?;
        Ok(ConnectionSpec::Serial {
            port,
            baud_rate: baud,
        })
    } else {
        let host = host
            .map(str::to_owned)
            .or_else(|| {
                std::env::var("LASER_NETWORK_HOST")
                    .ok()
                    .filter(|v| !v.trim().is_empty() && !v.starts_with('<'))
            })
            .ok_or("网络模式需要提供 host（或配置 LASER_NETWORK_HOST）")?;
        Ok(ConnectionSpec::Tcp { host, port: telnet })
    }
}

fn timeout_from(value: Option<f64>) -> Duration {
    Duration::from_secs_f64(
        value
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(5.0)
            .clamp(0.5, 30.0),
    )
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
    let label = if spec.mode() == "serial" {
        "串口"
    } else {
        "网络"
    };
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
                format!(
                    "{label} 握手成功 ({target})，$32=1 激光模式就绪；固件设置 {} 行: {}",
                    settings.len(),
                    settings.join(" ")
                )
            } else {
                format!("{label} 握手成功 ({target})，固件 $32=1 激光模式就绪")
            };
            ToolCallResult::with_detail(true, format!("{label}已连通 ({target})"), detail)
        }
        Err(error) => ToolCallResult::with_detail(
            false,
            format!("{label}已连接但未就绪 ({target})"),
            error.to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// laser_safe_action_tool：单命令安全动作，真实写设备
// ---------------------------------------------------------------------------

pub fn execute_laser_safe_action(args: &LaserSafeActionArgs) -> ToolCallResult {
    execute_laser_safe_action_with(args, &default_transport_opener())
}

fn wait_status_report<T: DeviceTransport>(
    device: &DeviceConnection<T>,
    timeout: Duration,
) -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        match device.transport.read_line() {
            Ok(Some(line)) if line.starts_with('<') => return Some(line),
            Ok(Some(_)) => continue,
            Ok(None) => return None,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    None
}

fn send_gcode_lines_cancellable<T: DeviceTransport>(
    device: &mut DeviceConnection<T>,
    lines: &[String],
    cancel: Option<&laser_device::CancellationToken>,
) -> Result<(), String> {
    if let Some(policy) = device.transport.dry_run_policy() {
        let preamble = policy.preamble_line();
        device.send_line_wait_ack_cancellable(&preamble, cancel)?;
    }
    for line in lines {
        device.send_line_wait_ack_cancellable(line, cancel)?;
    }
    Ok(())
}

pub fn execute_laser_safe_action_with(
    args: &LaserSafeActionArgs,
    opener: &TransportOpener,
) -> ToolCallResult {
    execute_laser_safe_action_with_gate(args, opener, &|| true)
}

/// 安全动作里直接写 GRBL 实时字节的那一类：急停 / 软复位（0x18）与进给、激光功率倍率（0x90–0x9D）。
pub struct RealtimeAction {
    pub command: RealtimeCommand,
    /// 回执与审计里的说明文字（含实际字节）
    pub label: &'static str,
    /// 是否会改变报警状态（急停 / 软复位）；倍率调节为假，可免确认执行
    pub alters_alarm_state: bool,
}

/// 安全动作名（已去空白并转小写）→ 实时字节命令；不是实时类动作时返回 `None`。
/// `laser-api` 的后台安全动作与本包的直连执行共用这一张表（2026-11-21 前两边各抄一份）。
pub fn realtime_action(action: &str) -> Option<RealtimeAction> {
    Some(match action {
        "emergency_stop" => RealtimeAction {
            command: RealtimeCommand::EmergencyStop,
            label: "急停 0x18",
            alters_alarm_state: true,
        },
        "soft_reset" => RealtimeAction {
            command: RealtimeCommand::EmergencyStop,
            label: "软复位 0x18",
            alters_alarm_state: true,
        },
        // 进给速度倍率调节 (Feed Override)
        "feed_override_reset" | "feed_reset" | "feed_100" => RealtimeAction {
            command: RealtimeCommand::FeedOvrReset,
            label: "进给速度恢复100% (0x90)",
            alters_alarm_state: false,
        },
        "feed_override_plus_10" | "feed_plus_10" | "feed_+10%" => RealtimeAction {
            command: RealtimeCommand::FeedOvrCoarsePlus,
            label: "进给速度增加10% (0x91)",
            alters_alarm_state: false,
        },
        "feed_override_minus_10" | "feed_minus_10" | "feed_-10%" => RealtimeAction {
            command: RealtimeCommand::FeedOvrCoarseMinus,
            label: "进给速度降低10% (0x92)",
            alters_alarm_state: false,
        },
        "feed_override_plus_1" | "feed_plus_1" | "feed_+1%" => RealtimeAction {
            command: RealtimeCommand::FeedOvrFinePlus,
            label: "进给速度增加1% (0x93)",
            alters_alarm_state: false,
        },
        "feed_override_minus_1" | "feed_minus_1" | "feed_-1%" => RealtimeAction {
            command: RealtimeCommand::FeedOvrFineMinus,
            label: "进给速度降低1% (0x94)",
            alters_alarm_state: false,
        },
        // 激光/主轴功率倍率调节 (Spindle/Laser Power Override)
        "spindle_override_reset" | "power_override_reset" | "power_reset" | "power_100" => {
            RealtimeAction {
                command: RealtimeCommand::SpindleOvrReset,
                label: "激光功率恢复100% (0x99)",
                alters_alarm_state: false,
            }
        }
        "spindle_override_plus_10" | "power_override_plus_10" | "power_plus_10" | "power_+10%" => {
            RealtimeAction {
                command: RealtimeCommand::SpindleOvrCoarsePlus,
                label: "激光功率增加10% (0x9A)",
                alters_alarm_state: false,
            }
        }
        "spindle_override_minus_10"
        | "power_override_minus_10"
        | "power_minus_10"
        | "power_-10%" => RealtimeAction {
            command: RealtimeCommand::SpindleOvrCoarseMinus,
            label: "激光功率降低10% (0x9B)",
            alters_alarm_state: false,
        },
        "spindle_override_plus_1" | "power_override_plus_1" | "power_plus_1" | "power_+1%" => {
            RealtimeAction {
                command: RealtimeCommand::SpindleOvrFinePlus,
                label: "激光功率增加1% (0x9C)",
                alters_alarm_state: false,
            }
        }
        "spindle_override_minus_1" | "power_override_minus_1" | "power_minus_1" | "power_-1%" => {
            RealtimeAction {
                command: RealtimeCommand::SpindleOvrFineMinus,
                label: "激光功率降低1% (0x9D)",
                alters_alarm_state: false,
            }
        }
        _ => return None,
    })
}

/// The host can invalidate a queued request during device opening or read-only preflight.
/// The gate does not replace confirmation, strict-mode approval or device safety checks.
pub fn execute_laser_safe_action_with_gate(
    args: &LaserSafeActionArgs,
    opener: &TransportOpener,
    allowed: &dyn Fn() -> bool,
) -> ToolCallResult {
    execute_laser_safe_action_with_gate_and_cancel(args, opener, allowed, None)
}

/// 在授权门禁基础上绑定 `CancellationToken`，确保独立 `home`/`unlock`/点动等动作在握手、自动宏、等 Idle 与行发送全阶段可被急停/退出实时中止。
pub fn execute_laser_safe_action_with_gate_and_cancel(
    args: &LaserSafeActionArgs,
    opener: &TransportOpener,
    allowed: &dyn Fn() -> bool,
    cancel: Option<&laser_device::CancellationToken>,
) -> ToolCallResult {
    execute_laser_safe_action_with_gate_cancel_and_dispatch(args, opener, allowed, cancel, None)
}

/// 同 [`execute_laser_safe_action_with_gate_and_cancel`]，额外在越过预检正式向控制器发令前置位 `dispatched`，
/// 供调用方区分“预检未发令即拒绝”与“已发令后失败/停机状态未知”。
pub fn execute_laser_safe_action_with_gate_cancel_and_dispatch(
    args: &LaserSafeActionArgs,
    opener: &TransportOpener,
    allowed: &dyn Fn() -> bool,
    cancel: Option<&laser_device::CancellationToken>,
    dispatched: Option<&std::sync::atomic::AtomicBool>,
) -> ToolCallResult {
    let action = args.action.trim().to_ascii_lowercase();
    let confirmed = args.confirmed.unwrap_or(false);
    let dry_run = args.dry_run.unwrap_or(false);
    let feed = args
        .feed_rate
        .filter(|f| f.is_finite() && *f > 0.0)
        .unwrap_or(1200.0);
    let x = args.x.unwrap_or(0.0);
    let y = args.y.unwrap_or(0.0);
    if !x.is_finite() || !y.is_finite() {
        return ToolCallResult::err("坐标 x / y 必须是有限数字");
    }

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
        "unlock" => Plan::Lines(vec!["$X".into()], "解锁 $X".into()),
        "home" => Plan::Lines(vec!["$H".into()], "回原点 $H".into()),
        "set_origin" => Plan::Lines(vec!["G92 X0 Y0".into()], "设置工作原点 G92 X0 Y0".into()),
        "laser_off" => Plan::Lines(vec!["M5".into(), "S0".into()], "关闭激光 M5 S0".into()),
        "absolute_move" => Plan::Lines(
            vec![
                "M5".into(),
                "G90".into(),
                format!("G1 X{x:.3} Y{y:.3} S0 F{feed:.0}"),
            ],
            format!("绝对安全移动到 ({x:.3}, {y:.3}) F{feed:.0}（激光关闭）"),
        ),
        "relative_move" => Plan::Lines(
            vec![
                "M5".into(),
                "G91".into(),
                format!("G0 X{x:.3} Y{y:.3} F{feed:.0}"),
                "G90".into(),
            ],
            format!("相对安全移动 ({x:+.3}, {y:+.3}) F{feed:.0}（激光关闭）"),
        ),
        "pause" | "resume" | "stop_framing" => {
            return ToolCallResult::err(
                "走边框的暂停、继续、停止要作用在 MOSS 后台（moss-host）正在走的边框上，当前没有连上后台",
            );
        }
        unknown => match realtime_action(unknown) {
            Some(realtime) => Plan::Realtime {
                command: realtime.command,
                label: realtime.label,
                alters_alarm_state: realtime.alters_alarm_state,
            },
            None => return ToolCallResult::err(format!("不支持的安全动作: {unknown}")),
        },
    };
    let is_read_or_override = matches!(
        plan,
        Plan::Status
            | Plan::Realtime {
                alters_alarm_state: false,
                ..
            }
    );
    if !is_read_or_override && !confirmed && !dry_run {
        return ToolCallResult::err(format!(
            "动作 {action} 必须确认后方可执行 (confirmed=false)"
        ));
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
        return ToolCallResult::with_detail(
            true,
            format!("dry_run: 将向 {target} 发送 {description}"),
            "未连接设备，未发送任何字节",
        );
    }
    if !allowed() {
        return ToolCallResult::err("语音设备授权已失效，操作未执行");
    }
    if cancel.is_some_and(laser_device::CancellationToken::is_cancelled) {
        return ToolCallResult::err("操作在建立设备连接前已取消，未连接设备");
    }
    let timeout = timeout_from(args.timeout);
    let transport = match opener(&spec, timeout) {
        Ok(transport) => transport,
        Err(error) => return ToolCallResult::err(format!("设备连接失败 ({target}): {error}")),
    };
    if !allowed() {
        return ToolCallResult::err("连接期间设备授权已失效，操作未执行");
    }
    if cancel.is_some_and(laser_device::CancellationToken::is_cancelled) {
        let _ = transport.write_realtime(RealtimeCommand::EmergencyStop);
        return ToolCallResult::err("连接期间操作已取消，已发送急停 0x18 兜底");
    }
    let mut device = DeviceConnection::new(transport);
    match plan {
        Plan::Status => {
            if let Err(error) = device
                .transport
                .write_realtime(RealtimeCommand::StatusQuery)
            {
                return ToolCallResult::err(format!("状态查询发送失败 ({target}): {error}"));
            }
            match wait_status_report(&device, timeout) {
                Some(report) => ToolCallResult::with_detail(
                    true,
                    format!("设备状态: {}", summarize_status(&report)),
                    report,
                ),
                None => ToolCallResult::err(format!(
                    "{target} 在 {}ms 内未返回状态报告",
                    timeout.as_millis()
                )),
            }
        }
        Plan::Realtime {
            command,
            label,
            alters_alarm_state,
        } => match device.transport.write_realtime(command) {
            Ok(()) => {
                if alters_alarm_state {
                    ToolCallResult::ok(format!(
                        "{label} 已发送到 {target}，控制器将进入 Alarm/复位态"
                    ))
                } else {
                    ToolCallResult::ok(format!("{label} 已实时发送到 {target}，即时生效（不停机）"))
                }
            }
            Err(error) => ToolCallResult::err(format!("{label} 发送失败 ({target}): {error}")),
        },
        Plan::Lines(lines, label) => {
            // 运动/解锁类顺序命令必须先握手：$32=1 门禁 + 设备可响应
            let needs_laser_mode = !matches!(action.as_str(), "unlock" | "laser_off");
            match device.handshake_with_deadline_cancellable(timeout, cancel) {
                Ok(_) => {}
                Err(error) if cancel.is_some_and(laser_device::CancellationToken::is_cancelled) => {
                    let _ = device
                        .transport
                        .write_realtime(RealtimeCommand::EmergencyStop);
                    return ToolCallResult::err(format!(
                        "{label} 在握手阶段已取消，已发送急停 0x18 兜底 ({target}): {error}"
                    ));
                }
                Err(error) if needs_laser_mode => {
                    return ToolCallResult::err(format!("{label} 拒绝执行：{error}"));
                }
                Err(_) => {
                    // 解锁/关光允许在 Alarm 态下发；设备须至少有响应
                    if device.state() == laser_device::DeviceState::Disconnected {
                        return ToolCallResult::err(format!(
                            "{label} 拒绝执行：{target} 无 GRBL 响应"
                        ));
                    }
                    device.state = laser_device::DeviceState::Ready;
                }
            }
            if cancel.is_some_and(laser_device::CancellationToken::is_cancelled) {
                let _ = device
                    .transport
                    .write_realtime(RealtimeCommand::EmergencyStop);
                return ToolCallResult::err(format!(
                    "{label} 在预检阶段已取消，已发送急停 0x18 兜底 ({target})"
                ));
            }
            // FluidNC：读取 $CD 以感知 after_unlock / after_homing 自动宏；仅旧 GRBL error:3 静默跳过。
            // R2 关光（laser_off）不依赖 $CD；解锁（unlock）允许在 Alarm 态（如 error:9/ALARM:）继续发 $X，
            // 但断线/复位/未排空超时仍必须中止。
            if action != "laser_off"
                && let Err(error) = device.load_features(timeout)
            {
                if needs_laser_mode
                    || matches!(
                        device.state(),
                        laser_device::DeviceState::Disconnected | laser_device::DeviceState::Lost
                    )
                {
                    return ToolCallResult::err(format!("{label} 拒绝执行：{target} {error}"));
                }
                device.state = laser_device::DeviceState::Ready;
            }
            if !allowed() {
                return ToolCallResult::err("预检期间设备授权已失效，操作未执行");
            }
            if cancel.is_some_and(laser_device::CancellationToken::is_cancelled) {
                let _ = device
                    .transport
                    .write_realtime(RealtimeCommand::EmergencyStop);
                return ToolCallResult::err(format!(
                    "{label} 已取消，已发送急停 0x18 兜底 ({target})"
                ));
            }
            // LB1（2026-11-28）：固件已明确上报 $22=0（关闭机械回零）时，在发送 $H 前拦截，
            // 不发送 $H、不触发 0x18 急停兜底、不留下待确认停机记录；未知（None）时保持原有行为。
            if action == "home" && device.travel_limits.homing_enabled == Some(false) {
                return ToolCallResult::with_detail(
                    false,
                    format!("固件未开启回零（$22=0），已拒绝机械回零 ({target})"),
                    "homing_disabled",
                );
            }
            if let Some(flag) = dispatched {
                flag.store(true, std::sync::atomic::Ordering::Release);
            }
            let outcome = match action.as_str() {
                // 解锁 / 回零：等 ok 后继续等到 Idle（FluidNC 会在其后执行自动宏，Home 状态视为忙碌）
                "unlock" => device.unlock_cancellable(cancel),
                "home" => device.home_cancellable(cancel),
                _ => send_gcode_lines_cancellable(&mut device, &lines, cancel),
            };
            match outcome {
                Ok(()) if cancel.is_some_and(laser_device::CancellationToken::is_cancelled) => {
                    let _ = device
                        .transport
                        .write_realtime(RealtimeCommand::EmergencyStop);
                    ToolCallResult::err(format!(
                        "{label} 执行期间收到取消/急停，已发送急停 0x18 兜底 ({target})"
                    ))
                }
                Ok(()) => ToolCallResult::with_detail(
                    true,
                    format!("{label} 已执行 ({target})"),
                    lines.join(" | "),
                ),
                Err(error) => {
                    let estop_res = device
                        .transport
                        .write_realtime(RealtimeCommand::EmergencyStop);
                    let diagnostic = laser_device::diagnose_device_message(&error)
                        .map(|d| format!(" ({d})"))
                        .unwrap_or_default();
                    let estop_note = match estop_res {
                        Ok(()) => "已发送急停 0x18 兜底",
                        Err(ref estop_err) => {
                            return ToolCallResult::err(format!(
                                "{label} 执行失败 ({target})，且急停 0x18 兜底失败（{estop_err}）: {error}{diagnostic}"
                            ));
                        }
                    };
                    ToolCallResult::err(format!(
                        "{label} 执行失败 ({target})，{estop_note}: {error}{diagnostic}"
                    ))
                }
            }
        }
    }
}

fn summarize_status(report: &str) -> String {
    let inner = report.trim().trim_start_matches('<').trim_end_matches('>');
    let mut parts = inner.split('|');
    let state = parts.next().unwrap_or("Unknown");
    let pos = parts
        .find(|p| p.starts_with("MPos:") || p.starts_with("WPos:"))
        .unwrap_or("");
    if pos.is_empty() {
        state.to_owned()
    } else {
        format!("{state} ({pos})")
    }
}

// ---------------------------------------------------------------------------
// 校准选格（本地校验；持久化由 host 后端承接）
// ---------------------------------------------------------------------------

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

#[derive(Debug)]
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
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("[stderr] ");
                text.push_str(output.stderr.trim_end());
            }
            const LIMIT: usize = 8000;
            if text.len() > LIMIT {
                let mut cut = LIMIT;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                text.push_str("\n…(输出已截断)");
            }
            ToolCallResult::with_detail(
                output.success,
                format!("命令退出码 {}: {cmd}", output.code),
                text,
            )
        }
        Err(error) => ToolCallResult::err(format!("命令启动失败: {error}")),
    }
}

fn hide_console_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

const COMMAND_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(target_os = "windows"))]
const SYSTEM_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_COMMAND_CAPTURE_BYTES: usize = 64 * 1024;

fn wait_child_until(
    child: &mut std::process::Child,
    deadline: Instant,
    timeout_secs: u64,
) -> Result<std::process::ExitStatus, String> {
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => return Ok(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("外部命令执行超过 {timeout_secs} 秒已中止"));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn run_status_with_timeout(
    cmd: &mut std::process::Command,
    timeout: Duration,
) -> Result<std::process::ExitStatus, String> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    wait_child_until(&mut child, Instant::now() + timeout, timeout.as_secs())
}

fn spawn_bounded_pipe_reader<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    use std::sync::atomic::Ordering;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut captured = Vec::new();
        if let Some(mut reader) = pipe {
            let mut scratch = [0u8; 4096];
            while !stop.load(Ordering::Acquire) {
                match reader.read(&mut scratch) {
                    Ok(0) => break,
                    Ok(n) => {
                        let remain = MAX_COMMAND_CAPTURE_BYTES.saturating_sub(captured.len());
                        if remain > 0 {
                            captured.extend_from_slice(&scratch[..n.min(remain)]);
                        }
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            drop(reader);
        }
        let _ = tx.send(captured);
    });
    rx
}

fn run_output_with_timeout(
    cmd: &mut std::process::Command,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let deadline = Instant::now() + timeout;
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_guard = StopOnDrop(stop.clone());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let out_rx = spawn_bounded_pipe_reader(child.stdout.take(), stop.clone());
    let err_rx = spawn_bounded_pipe_reader(child.stderr.take(), stop);
    let status = wait_child_until(&mut child, deadline, timeout.as_secs())?;
    let recv_bounded = |rx: std::sync::mpsc::Receiver<Vec<u8>>| -> Result<Vec<u8>, String> {
        let remain = deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(remain)
            .map_err(|_| format!("外部命令输出管道超过 {} 秒未关闭已中止", timeout.as_secs()))
    };
    let stdout = recv_bounded(out_rx)?;
    let stderr = recv_bounded(err_rx)?;
    Ok(CommandOutput {
        success: status.success(),
        code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

pub fn execute_command(args: &CommandExecutionArgs, developer_mode: bool) -> ToolCallResult {
    execute_command_with_runner(args, developer_mode, |cmd| {
        if cfg!(windows) {
            let mut command = std::process::Command::new("cmd");
            command.args(["/C", cmd]);
            hide_console_window(&mut command);
            run_output_with_timeout(&mut command, COMMAND_EXEC_TIMEOUT)
        } else {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", cmd]);
            run_output_with_timeout(&mut command, COMMAND_EXEC_TIMEOUT)
        }
    })
}

fn validate_launch_text(value: &str, label: &str) -> Result<(), String> {
    if value
        .chars()
        .any(|c| c == '\0' || c == '\r' || c == '\n' || c.is_control())
    {
        return Err(format!("{label} 包含非法控制字符"));
    }
    Ok(())
}

/// Windows 侧针对批处理脚本与未显式指定 `.exe` 目标的参数注入防线：
/// `ShellExecuteW` 启动 `.cmd`/`.bat` 或经 `PATHEXT` 解析到脚本时仍由 `cmd.exe` 解析命令行，
/// 因此禁止向 `.cmd`/`.bat` 传参，并对非 `.exe` 目标拒绝 `cmd.exe` 元字符。
#[cfg(any(target_os = "windows", test))]
fn validate_windows_shell_launch(target: &str, payload: Option<&str>) -> Result<(), String> {
    let lower_target = target.trim().to_ascii_lowercase();
    let is_batch = lower_target.ends_with(".cmd") || lower_target.ends_with(".bat");
    if let Some(arg) = payload {
        if is_batch {
            return Err("安全策略禁止在 Windows 上向批处理脚本传递参数".into());
        }
        let is_explicit_exe = lower_target.ends_with(".exe");
        if !is_explicit_exe
            && arg
                .chars()
                .any(|c| matches!(c, '&' | '|' | '<' | '>' | '^' | '%' | '!' | '"'))
        {
            return Err("非 .exe 目标的参数包含 cmd.exe 元字符，已拒绝执行".into());
        }
    }
    Ok(())
}

/// 按 Windows `CommandLineToArgvW` 规则将单个参数安全包裹，避免参数分裂。
#[cfg(any(target_os = "windows", test))]
fn quote_windows_shell_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        if ch == '\\' {
            backslashes += 1;
        } else if ch == '"' {
            for _ in 0..backslashes * 2 + 1 {
                out.push('\\');
            }
            backslashes = 0;
            out.push('"');
        } else {
            for _ in 0..backslashes {
                out.push('\\');
            }
            backslashes = 0;
            out.push(ch);
        }
    }
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

#[cfg(target_os = "windows")]
fn windows_shell_execute(target: &str, payload: Option<&str>) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    validate_windows_shell_launch(target, payload)?;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }

    const SW_SHOWNORMAL: i32 = 1;
    let to_wide = |s: &str| -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let verb = to_wide("open");
    let file = to_wide(target);
    let params = payload.map(|p| to_wide(&quote_windows_shell_arg(p)));
    let params_ptr = params.as_ref().map_or(std::ptr::null(), |buf| buf.as_ptr());
    // SAFETY: verb/file/params 均为有效的 NUL 结尾 UTF-16 缓冲区，调用期间保持存活；
    // ShellExecuteW 已在入口拦截 .cmd/.bat 与元字符，直接调用系统关联打开目标。
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params_ptr,
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if rc > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW 返回错误码 {rc}"))
    }
}

fn open_with_system(target: &str) -> Result<(), String> {
    validate_launch_text(target, "目标")?;
    #[cfg(target_os = "windows")]
    {
        windows_shell_execute(target, None)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let status = if cfg!(target_os = "macos") {
            let mut cmd = std::process::Command::new("open");
            cmd.args(["--", target]);
            run_status_with_timeout(&mut cmd, SYSTEM_OPEN_TIMEOUT)
        } else {
            if target.starts_with('-') {
                return Err("目标不能以 '-' 开头".into());
            }
            let mut cmd = std::process::Command::new("xdg-open");
            cmd.arg(target);
            run_status_with_timeout(&mut cmd, SYSTEM_OPEN_TIMEOUT)
        };
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!(
                "系统打开命令退出码 {}",
                status.code().unwrap_or(-1)
            )),
            Err(error) => Err(error),
        }
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
        "vscode" => "Code.exe",
        "lasergrbl" => "LaserGRBL.exe",
        _ => argument,
    }
    .to_owned()
}

pub fn execute_open_app(args: &OpenAppArgs) -> ToolCallResult {
    let app = args.argument.trim();
    if app.is_empty() {
        return ToolCallResult::err("argument 不能为空");
    }
    let target = resolve_app_alias(app);
    let payload = args
        .payload
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let result = match payload {
        Some(payload) => {
            if let Err(err) = validate_launch_text(&target, "应用目标")
                .and_then(|()| validate_launch_text(payload, "应用参数"))
            {
                Err(err)
            } else {
                #[cfg(target_os = "windows")]
                {
                    windows_shell_execute(&target, Some(payload))
                }
                #[cfg(not(target_os = "windows"))]
                {
                    if cfg!(target_os = "macos") {
                        let mut cmd = std::process::Command::new("open");
                        cmd.args(["-a", &target, "--", payload]);
                        run_status_with_timeout(&mut cmd, SYSTEM_OPEN_TIMEOUT).and_then(|s| {
                            if s.success() {
                                Ok(())
                            } else {
                                Err(format!("退出码 {}", s.code().unwrap_or(-1)))
                            }
                        })
                    } else {
                        // Linux GUI 应用启动后直接返回，避免 `.status()` 无限阻塞直到目标窗口关闭
                        std::process::Command::new(&target)
                            .arg(payload)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    }
                }
            }
        }
        None => open_with_system(&target),
    };
    match result {
        Ok(()) => ToolCallResult::ok(format!("已启动应用: {app}")),
        Err(error) => ToolCallResult::err(format!("启动应用 {app} 失败: {error}")),
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
        "status" | "preview" | "frame" | "cancel" | "recent" | "feedback" | "regenerate" => {
            ToolCallResult::err(format!(
                "workflow {act} 需要 moss-host 后端（workflow_id: {}），当前 MCP 会话未连接 host",
                workflow_id.unwrap_or("none")
            ))
        }
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
                return ToolCallResult::err(format!(
                    "workflow {id} 门禁通过，但当前 MCP 会话未连接 host，未发送"
                ));
            }
            ToolCallResult::err(format!(
                "workflow {id} confirm_send 需要 moss-host 后端执行物理 SHA 门禁与真实下发"
            ))
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

    /// 没有 host 后端时，走边框的暂停 / 继续 / 停止明确说明需要后台，不当成未知动作（2026-11-18）
    #[test]
    fn framing_controls_without_host_explain_backend_is_needed() {
        for action in ["pause", "resume", "stop_framing"] {
            let args = laser_contracts::LaserSafeActionArgs {
                action: action.into(),
                confirmed: Some(true),
                ..Default::default()
            };
            let result = execute_laser_safe_action(&args);
            assert!(!result.success, "{action}");
            assert!(
                result.result.contains("moss-host"),
                "{action}: {}",
                result.result
            );
        }
    }
    use laser_device::FakeGrbl;
    use std::sync::Mutex;

    #[test]
    fn safe_catalog_keeps_laser_order_and_hides_commands() {
        let catalog = ToolCatalog::safe_default();
        assert_eq!(&catalog.names()[..8], &LASER_TOOLS);
        assert_eq!(catalog.names()[8], "open_app_tool");
        assert!(!catalog.names().contains(&"command_execution_tool"));
        assert_eq!(catalog.specs().len(), 9);
    }

    #[test]
    fn developer_mode_adds_command_only_after_laser_and_desktop_tools() {
        let catalog = ToolCatalog::developer_mode();
        assert_eq!(&catalog.names()[..8], &LASER_TOOLS);
        assert_eq!(catalog.specs().len(), 10);
        assert!(
            catalog
                .specs()
                .iter()
                .any(|spec| spec.name == "command_execution_tool" && spec.developer_only)
        );
    }

    /// 共享句柄：让同一个 FakeGrbl 既交给工具使用，又留在测试里做断言。
    struct SharedFake(Arc<FakeGrbl>);
    impl DeviceTransport for SharedFake {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.0.write_line(line)
        }
        fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
            self.0.write_realtime(command)
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            self.0.read_line()
        }
        fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
            self.0.raw_abort_handle()
        }
    }

    /// 记录每次打开的连接描述，并把同一个 FakeGrbl 句柄交给工具，便于断言写入的字节。
    fn fake_opener(
        responses: Vec<&str>,
    ) -> (TransportOpener, Arc<Mutex<Vec<String>>>, Arc<FakeGrbl>) {
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
        let args = CheckLaserConnectionArgs {
            host: Some("192.0.2.10".into()),
            telnet_port: Some(23),
            ..Default::default()
        };
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
        assert!(
            refused
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("connection refused")
        );

        // 串口参数
        let (opener, targets, _) = fake_opener(vec!["$32=1", "ok"]);
        let serial = execute_check_laser_connection_with(
            &CheckLaserConnectionArgs {
                port: Some("COM7".into()),
                baudrate: Some(250000),
                ..Default::default()
            },
            &opener,
        );
        assert!(serial.success, "{serial:?}");
        assert_eq!(targets.lock().unwrap().as_slice(), ["COM7@250000"]);
    }

    #[test]
    fn home_rejects_when_dollar22_is_zero_without_sending_home_or_estop() {
        // $22=0：握手后拦截，不发 $H、不触发急停 0x18 兜底、返回 homing_disabled
        let (opener, _, fake) = fake_opener(vec!["$22=0", "$32=1", "ok", "error:3"]);
        let args = LaserSafeActionArgs {
            action: "home".into(),
            confirmed: Some(true),
            host: Some("192.0.2.10".into()),
            ..Default::default()
        };
        let res = execute_laser_safe_action_with(&args, &opener);
        assert!(!res.success, "expected rejection: {res:?}");
        assert_eq!(res.detail.as_deref(), Some("homing_disabled"));
        assert!(res.result.contains("$22=0"), "{}", res.result);
        assert!(
            !fake.lines().iter().any(|l| l == "$H"),
            "must not send $H when $22=0: {:?}",
            fake.lines()
        );
        assert!(
            fake.realtime().is_empty(),
            "must not trigger fallback 0x18 when rejected before dispatch: {:?}",
            fake.realtime()
        );

        // $22=1：正常放行并发送 $H
        let (opener_ok, _, fake_ok) = fake_opener(vec![
            "$22=1",
            "$32=1",
            "ok",
            "error:3",
            "ok",
            "<Idle|MPos:0.000,0.000,0.000>",
        ]);
        let res_ok = execute_laser_safe_action_with(&args, &opener_ok);
        assert!(res_ok.success, "{res_ok:?}");
        assert!(fake_ok.lines().iter().any(|l| l == "$H"));

        // 固件未报告 $22（homing_enabled == None）：保持原有行为，照常发送 $H
        let (opener_unknown, _, fake_unknown) = fake_opener(vec![
            "$32=1",
            "ok",
            "error:3",
            "ok",
            "<Idle|MPos:0.000,0.000,0.000>",
        ]);
        let res_unknown = execute_laser_safe_action_with(&args, &opener_unknown);
        assert!(res_unknown.success, "{res_unknown:?}");
        assert!(fake_unknown.lines().iter().any(|l| l == "$H"));
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
        let unconfirmed =
            execute_laser_safe_action_with(&host_args("emergency_stop", false), &opener);
        assert!(!unconfirmed.success);
        assert!(targets.lock().unwrap().is_empty(), "未确认动作不得打开设备");

        // 急停：实时信道写 0x18
        let (opener, _, fake) = fake_opener(vec![]);
        let stop = execute_laser_safe_action_with(&host_args("emergency_stop", true), &opener);
        assert!(stop.success, "{stop:?}");
        assert_eq!(fake.realtime(), vec![0x18]);
        assert!(fake.lines().is_empty());

        // 绝对移动：握手后写 M5/G90/G1 S0 F，按指定速度可控移动且激光关闭（GRBL 的 G0 会忽略 F）
        // 响应：握手($32=1, ok) → $CD(ok, 非 FluidNC 视为空配置) → M5/G90/G1 各一个 ok
        let (opener, _, fake) = fake_opener(vec!["$32=1", "ok", "ok", "ok", "ok", "ok"]);
        let mut mv = host_args("absolute_move", true);
        mv.x = Some(10.0);
        mv.y = Some(5.5);
        mv.feed_rate = Some(800.0);
        let moved = execute_laser_safe_action_with(&mv, &opener);
        assert!(moved.success, "{moved:?}");
        assert_eq!(
            fake.lines(),
            vec!["$$", "$CD", "M5", "G90", "G1 X10.000 Y5.500 S0 F800"]
        );

        // 回工作原点（N4）：必须用 G1 X0.000 Y0.000 S0 F1000，确保实际按 1000 mm/min 移回且关光
        let (opener, _, fake) = fake_opener(vec!["$32=1", "ok", "ok", "ok", "ok", "ok"]);
        let mut ret = host_args("absolute_move", true);
        ret.x = Some(0.0);
        ret.y = Some(0.0);
        ret.feed_rate = Some(1000.0);
        let returned = execute_laser_safe_action_with(&ret, &opener);
        assert!(returned.success, "{returned:?}");
        assert_eq!(
            fake.lines(),
            vec!["$$", "$CD", "M5", "G90", "G1 X0.000 Y0.000 S0 F1000"]
        );

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

        // Alarm 态下：laser_off 不依赖 $CD 直接关光，unlock 容忍 $CD 的 error:9/ALARM 继续发 $X 恢复
        let (opener, _, fake) = fake_opener(vec!["ALARM:1", "ok", "ok"]);
        let off = execute_laser_safe_action_with(&host_args("laser_off", true), &opener);
        assert!(off.success, "{off:?}");
        assert_eq!(fake.lines(), vec!["$$", "M5", "S0"]);

        let (opener, _, fake) = fake_opener(vec!["ALARM:1", "error:9", "ok", "<Idle|MPos:0,0,0>"]);
        let unlocked = execute_laser_safe_action_with(&host_args("unlock", true), &opener);
        assert!(unlocked.success, "{unlocked:?}");
        assert_eq!(fake.lines(), vec!["$$", "$CD", "$X"]);

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
        let power_minus =
            execute_laser_safe_action_with(&host_args("power_minus_10", false), &opener);
        assert!(power_minus.success, "{power_minus:?}");
        assert_eq!(fake.realtime(), vec![0x9B]);
        assert!(power_minus.result.contains("不停机"));
    }

    #[test]
    fn local_fallbacks_do_not_fake_success() {
        let cmd = CommandExecutionArgs {
            command: "echo test".into(),
        };
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
        assert!(
            !execute_select_calibration_cell(&SelectCalibrationCellArgs {
                calibration_id: "cal".into(),
                cell_number: 2,
                ..Default::default()
            })
            .success
        );

        let bad_preflight = SendPreflight {
            status: "running".into(),
            expected_sha256: "0".repeat(64),
            actual_sha256: Some("0".repeat(64)),
            gcode_path: None,
            workflow_snapshot_valid: true,
            calibration_sha_valid: false,
        };
        let wf_bad_gate = execute_laser_workflow_with_preflight(
            Some("confirm_send"),
            Some("wf_1"),
            true,
            Some(&bad_preflight),
        );
        assert!(!wf_bad_gate.success);
        assert!(wf_bad_gate.result.contains("门禁"));

        assert_eq!(
            resolve_app_alias_for("macos", "微信"),
            "/Applications/WeChat.app"
        );
        assert_eq!(
            resolve_app_alias_for("macos", "终端"),
            "/System/Applications/Utilities/Terminal.app"
        );
        assert_eq!(resolve_app_alias_for("windows", "终端"), "cmd.exe");
    }

    #[test]
    fn safe_action_rejects_non_finite_coords_and_launch_guards_control_chars() {
        let (opener, targets, _) = fake_opener(vec![]);
        let bad = execute_laser_safe_action_with(
            &LaserSafeActionArgs {
                action: "absolute_move".into(),
                confirmed: Some(true),
                x: Some(f64::NAN),
                y: Some(10.0),
                host: Some("127.0.0.1".into()),
                ..Default::default()
            },
            &opener,
        );
        assert!(!bad.success);
        assert!(bad.result.contains("有限数字"), "{bad:?}");
        assert!(targets.lock().unwrap().is_empty());

        assert!(validate_launch_text("hello\nwhoami", "target").is_err());
        assert!(validate_launch_text("hello\0world", "target").is_err());
        assert_eq!(
            quote_windows_shell_arg(r#"a "b" & calc.exe"#),
            r#""a \"b\" & calc.exe""#
        );
        assert_eq!(quote_windows_shell_arg("plain.txt"), "plain.txt");
        assert!(validate_windows_shell_launch("code.cmd", Some("foo&calc")).is_err());
        assert!(validate_windows_shell_launch("tool.BAT", Some("a")).is_err());
        assert!(validate_windows_shell_launch("code", Some("foo&calc.exe")).is_err());
        assert!(validate_windows_shell_launch("Code.exe", Some("foo&bar")).is_ok());
        assert_eq!(resolve_app_alias_for("windows", "vscode"), "Code.exe");

        #[cfg(not(windows))]
        {
            let mut sleeper = std::process::Command::new("sh");
            sleeper.args(["-c", "sleep 5"]);
            let err = run_status_with_timeout(&mut sleeper, Duration::from_millis(80)).unwrap_err();
            assert!(err.contains("已中止"), "{err}");

            // 后台孙进程继承 stdout/stderr 管道时，父 sh 立即退出也不得绕过总超时
            let mut bg = std::process::Command::new("sh");
            bg.args(["-c", "sleep 2 &"]);
            let started = Instant::now();
            let err = run_output_with_timeout(&mut bg, Duration::from_millis(120)).unwrap_err();
            assert!(err.contains("已中止"), "{err}");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "继承管道的孙进程不应拖住 run_output_with_timeout: {:?}",
                started.elapsed()
            );

            // 超 64 KiB 正常输出按上限截断但不因管道写满死锁
            let mut big = std::process::Command::new("sh");
            big.args([
                "-c",
                "dd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\\0' 'A'",
            ]);
            let out = run_output_with_timeout(&mut big, Duration::from_secs(5)).unwrap();
            assert!(out.success);
            assert_eq!(out.stdout.len(), MAX_COMMAND_CAPTURE_BYTES);

            // 持续输出的孤儿孙进程在超时返回后因读端关闭而收到 SIGPIPE 退出，不无限空转
            let mut spam = std::process::Command::new("sh");
            spam.args(["-c", "yes >/dev/stdout &"]);
            let spam_started = Instant::now();
            let spam_err =
                run_output_with_timeout(&mut spam, Duration::from_millis(120)).unwrap_err();
            assert!(spam_err.contains("已中止"), "{spam_err}");
            assert!(spam_started.elapsed() < Duration::from_secs(1));
        }
    }
}
