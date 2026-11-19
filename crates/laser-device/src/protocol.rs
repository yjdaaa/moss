#[derive(Debug, Clone, Default, PartialEq)]
pub struct MachineLimits {
    pub x_travel_mm: Option<f64>,
    pub y_travel_mm: Option<f64>,
    pub z_travel_mm: Option<f64>,
    /// X/Y 最大速度（mm/min）：GRBL `$110/$111`，FluidNC `max_rate_mm_per_min`（2026-11-16，工时估算用）
    pub x_max_rate_mm_per_min: Option<f64>,
    pub y_max_rate_mm_per_min: Option<f64>,
    /// X/Y 加速度（mm/s²）：GRBL `$120/$121`，FluidNC `acceleration_mm_per_sec2`（工时估算与自动过扫描用）
    pub x_accel_mm_s2: Option<f64>,
    pub y_accel_mm_s2: Option<f64>,
    /// 转角偏差（mm）：GRBL `$11`，FluidNC `junction_deviation_mm`
    pub junction_deviation_mm: Option<f64>,
}

impl MachineLimits {
    /// 按字段叠加覆盖：仅当 `preferred` 显式给出某项参数时才覆盖原值，避免 `$CD` 单轴配置擦掉 `$$` 的其余轴行程。
    pub fn merge_prefer(&mut self, preferred: &Self) {
        if preferred.x_travel_mm.is_some() {
            self.x_travel_mm = preferred.x_travel_mm;
        }
        if preferred.y_travel_mm.is_some() {
            self.y_travel_mm = preferred.y_travel_mm;
        }
        if preferred.z_travel_mm.is_some() {
            self.z_travel_mm = preferred.z_travel_mm;
        }
        if preferred.x_max_rate_mm_per_min.is_some() {
            self.x_max_rate_mm_per_min = preferred.x_max_rate_mm_per_min;
        }
        if preferred.y_max_rate_mm_per_min.is_some() {
            self.y_max_rate_mm_per_min = preferred.y_max_rate_mm_per_min;
        }
        if preferred.x_accel_mm_s2.is_some() {
            self.x_accel_mm_s2 = preferred.x_accel_mm_s2;
        }
        if preferred.y_accel_mm_s2.is_some() {
            self.y_accel_mm_s2 = preferred.y_accel_mm_s2;
        }
        if preferred.junction_deviation_mm.is_some() {
            self.junction_deviation_mm = preferred.junction_deviation_mm;
        }
    }

    pub fn has_xy_travel(&self) -> bool {
        self.x_travel_mm.is_some() || self.y_travel_mm.is_some()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MachineFeatures {
    pub has_air_assist: bool,
    pub has_door: bool,
    pub speed_map: Option<String>,
    /// `control.estop_pin` / `control.fault_pin` 已接线：状态报告 `Pn:` 会出现 `E`/`F`，固件会阻止解锁与回零
    pub has_estop_or_fault_pin: bool,
    /// `macros.after_unlock` 非空：`$X` 返回 ok 后固件会继续自动执行一段 G-code
    pub has_after_unlock_macro: bool,
    /// `macros.after_homing` 非空：`$H` 完成后固件会继续自动执行一段 G-code
    pub has_after_homing_macro: bool,
    /// `macros.after_reset` 非空：复位后固件会自动执行一段 G-code
    pub has_after_reset_macro: bool,
    /// `planner_blocks`（FluidNC 默认 16，范围 10~120）
    pub planner_blocks: Option<u32>,
    /// `use_line_numbers: true` 时状态报告带 `Ln:`
    pub use_line_numbers: bool,
    /// `verbose_errors: true` 时 `error:N` 之前会先发一行 `[MSG:ERR: ...]`
    pub verbose_errors: bool,
    /// `start.must_home: true` 时上电即 `ALARM:14`，必须先 `$H`
    pub must_home: bool,
    /// 配置里有顶层 `Laser:` 主轴段
    pub has_laser: bool,
    /// `Laser.off_on_alarm`：报警时是否由固件关激光。FluidNC 默认 false（`Spindle.h`），没写这一项就是 `None`
    pub laser_off_on_alarm: Option<bool>,
    /// 配置里明确写了 `verbose_errors: false`（FluidNC 默认 true）：报错不再附 `[MSG:ERR: 原因]`
    pub verbose_errors_disabled: bool,
}

// ---------------------------------------------------------------------------
// FluidNC 协议解析工具：状态报告、复位横幅、注释剥离与行长限制
// ---------------------------------------------------------------------------

/// FluidNC `Channel::maxLine = 255`（含结尾 NUL），超出部分被固件**静默丢弃**且仍回 `ok`。
/// 上位机侧允许的最大行长（不含换行）。
pub const MAX_GCODE_LINE_BYTES: usize = 254;

/// 剥离 G-code 行内注释：`;` 之后的内容与 `( ... )` 括号注释，返回去除首尾空白的指令部分。
pub fn strip_gcode_comments(line: &str) -> String {
    let code = line.split(';').next().unwrap_or("");
    let mut out = String::with_capacity(code.len());
    let mut depth = 0usize;
    for ch in code.chars() {
        match ch {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// 发送前规整结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedGcode {
    /// 已剥离注释、去空行后的待发送行
    pub lines: Vec<String>,
    /// 原文件中被剥离过注释的行数
    pub comment_stripped_lines: usize,
    /// 原文件中的空行 / 纯注释行数（不发送）
    pub skipped_lines: usize,
}

/// 单行发送前校验：行长与 ASCII。FluidNC 把所有 ≥0x80 的字节当实时字符走 UTF-8 解码而不进入行缓冲，
/// 因此非 ASCII 内容既不会被固件当作 G-code，也会让上位机字符计数与固件实际占用不一致。
pub fn validate_gcode_line(line: &str) -> Result<(), String> {
    if line.len() > MAX_GCODE_LINE_BYTES {
        return Err(format!(
            "行长 {} 字节超过固件上限 {MAX_GCODE_LINE_BYTES}（FluidNC 会静默截断并回 ok）",
            line.len()
        ));
    }
    if let Some(pos) = line
        .bytes()
        .position(|b| b >= 0x80 || (b < 0x20 && b != b'\t'))
    {
        return Err(format!(
            "第 {} 列含非 ASCII 或控制字符，固件不会将其当作 G-code",
            pos + 1
        ));
    }
    Ok(())
}

/// 把 G-code 文本整理为可发送的行：剥离注释、丢弃空行、校验每行长度与字符集。
/// 出错时报告原文件行号（从 1 起）。不修改磁盘文件，SHA 门禁仍针对原文件。
pub fn prepare_gcode_lines(text: &str) -> Result<PreparedGcode, String> {
    let mut prepared = PreparedGcode::default();
    for (index, raw) in text.lines().enumerate() {
        let stripped = strip_gcode_comments(raw);
        if stripped.len() != raw.trim().len() {
            prepared.comment_stripped_lines += 1;
        }
        if stripped.is_empty() {
            prepared.skipped_lines += 1;
            continue;
        }
        validate_gcode_line(&stripped).map_err(|e| format!("G-code 第 {} 行：{e}", index + 1))?;
        prepared.lines.push(stripped);
    }
    Ok(prepared)
}

/// 控制器复位/重启横幅。`Grbl ` 前缀来自 `$Start/Message`（用户可改）；
/// `[MSG:INFO: Restarting]` 是 `$Bye` / `[ESP444]` 重启前的最后一行；`[VER:` 只在 `$I` 出现，
/// 未主动查询时收到即视为固件在重发启动信息。
pub fn is_reset_banner(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("Grbl ")
        || trimmed.starts_with("[VER:")
        || trimmed.starts_with("[MSG:INFO: Restarting")
}

/// 固件在 `ALARM:N` / `error:N` 之前附带的人类可读说明行。
/// FluidNC：`[MSG:INFO: ALARM: Hard limit]` 先于 `ALARM:1`；`verbose_errors` 时 `[MSG:ERR: ...]` 先于 `error:N`。
pub fn extract_device_explanation(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let body = trimmed
        .strip_prefix("[MSG:INFO: ALARM:")
        .or_else(|| trimmed.strip_prefix("[MSG:ERR:"))
        .or_else(|| trimmed.strip_prefix("[MSG:FATAL:"))?;
    let text = body.trim_end_matches(']').trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// SD 脱机作业进度字段。
#[derive(Debug, Clone, PartialEq)]
pub enum SdProgress {
    /// `SD:45.50,/sd/job.gcode`
    Running { percent: f64, path: String },
    /// 作业读完后的 `SD: job.gcode: Sent`（无逗号，含空格）
    Sent { name: String },
    /// 无法解析但存在 `SD:` 字段
    Unknown(String),
}

/// 状态报告分类，覆盖 FluidNC `state_name()` 全部取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineActivity {
    Idle,
    /// `Run` / `Jog` / `Home` / `Starting`：正在动作，等待即可
    Busy,
    /// `Hold:0` / `Hold:1`
    Held,
    /// `Door:0..3`
    DoorOpen,
    Alarm,
    Sleep,
    Check,
    Unknown,
}

/// 解析后的 `<...>` 实时状态报告。
#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
    pub state: String,
    pub activity: MachineActivity,
    /// `Pn:` 字段原文（如 `XYE`），无则为空
    pub pins: String,
    pub sd: Option<SdProgress>,
    /// `Bf:` 第二个数：串口/网络接收缓冲剩余字节
    pub rx_buffer_free: Option<usize>,
    /// `Bf:` 第一个数：规划器剩余块数
    pub planner_free: Option<usize>,
    /// `MPos:x,y,z` 机床坐标（mm）
    pub mpos: Option<(f64, f64, f64)>,
    /// `WPos:x,y,z` 工件坐标（mm）
    pub wpos: Option<(f64, f64, f64)>,
    /// `Ln:N` 当前正在执行的行号（`use_line_numbers: true` 时报告）
    pub line_number: Option<usize>,
    pub raw: String,
}

impl StatusReport {
    /// 会阻止解锁 / 回零 / 加工的引脚：`E` 急停、`F` 故障、`D` 门开。
    pub fn blocking_pins(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.pins.contains('E') {
            out.push("急停按钮 (E)");
        }
        if self.pins.contains('F') {
            out.push("故障输入 (F)");
        }
        if self.pins.contains('D') {
            out.push("安全门 (D)");
        }
        out
    }

    pub fn has_sd_job(&self) -> bool {
        matches!(
            self.sd,
            Some(SdProgress::Running { .. }) | Some(SdProgress::Unknown(_))
        )
    }
}

fn parse_xyz_triplet(raw: &str) -> Option<(f64, f64, f64)> {
    let mut parts = raw.split(',');
    let x = parts.next()?.trim().parse::<f64>().ok()?;
    let y = parts.next()?.trim().parse::<f64>().ok()?;
    let z = parts
        .next()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    (x.is_finite() && y.is_finite() && z.is_finite()).then_some((x, y, z))
}

/// 解析 `<State|MPos:..|Bf:15,1200|Pn:XE|SD:12.50,/sd/a.nc>`；非状态报告返回 `None`。
pub fn parse_status_report(line: &str) -> Option<StatusReport> {
    let inner = line.trim().strip_prefix('<')?.strip_suffix('>')?;
    let mut fields = inner.split('|');
    let state = fields.next().unwrap_or("").trim().to_string();
    let activity = match state.as_str() {
        "Idle" => MachineActivity::Idle,
        "Run" | "Jog" | "Home" | "Starting" => MachineActivity::Busy,
        "Alarm" => MachineActivity::Alarm,
        "Sleep" => MachineActivity::Sleep,
        "Check" => MachineActivity::Check,
        s if s.starts_with("Hold") => MachineActivity::Held,
        s if s.starts_with("Door") => MachineActivity::DoorOpen,
        _ => MachineActivity::Unknown,
    };
    let mut report = StatusReport {
        state,
        activity,
        pins: String::new(),
        sd: None,
        rx_buffer_free: None,
        planner_free: None,
        mpos: None,
        wpos: None,
        line_number: None,
        raw: line.trim().to_string(),
    };
    for field in fields {
        if let Some(pins) = field.strip_prefix("Pn:") {
            report.pins = pins.trim().to_string();
        } else if let Some(ln) = field.strip_prefix("Ln:") {
            report.line_number = ln.trim().parse::<usize>().ok().filter(|n| *n > 0);
        } else if let Some(mpos) = field.strip_prefix("MPos:") {
            report.mpos = parse_xyz_triplet(mpos);
        } else if let Some(wpos) = field.strip_prefix("WPos:") {
            report.wpos = parse_xyz_triplet(wpos);
        } else if let Some(bf) = field.strip_prefix("Bf:") {
            let mut parts = bf.split(',');
            report.planner_free = parts.next().and_then(|v| v.trim().parse().ok());
            report.rx_buffer_free = parts.next().and_then(|v| v.trim().parse().ok());
        } else if let Some(sd) = field.strip_prefix("SD:") {
            let sd = sd.trim();
            report.sd = Some(if let Some((pct, path)) = sd.split_once(',') {
                match pct.trim().parse::<f64>() {
                    Ok(percent) => SdProgress::Running {
                        percent,
                        path: path.trim().to_string(),
                    },
                    Err(_) => SdProgress::Unknown(sd.to_string()),
                }
            } else if let Some(name) = sd.strip_suffix(": Sent") {
                SdProgress::Sent {
                    name: name.trim().to_string(),
                }
            } else {
                SdProgress::Unknown(sd.to_string())
            });
        }
    }
    Some(report)
}

/// 解析 FluidNC YAML 配置文本，提取机械行程与外设引脚能力
/// 2026-11-09：FluidNC 的 `Laser.off_on_alarm` 默认关（`Spindle.h`），报警时固件不主动关激光；
/// M3 恒功率模式下停在原地也可能继续出光。配置了激光段却没开时给出提醒（只提醒，不拦截）。
pub fn laser_off_on_alarm_warning(features: &MachineFeatures) -> Option<&'static str> {
    (features.has_laser && features.laser_off_on_alarm != Some(true)).then_some(
        "config.yaml 的 Laser 段没有开启 off_on_alarm：报警时固件不会主动关激光，建议加上 off_on_alarm: true 后重启控制器",
    )
}

pub fn parse_fluidnc_yaml(yaml: &str) -> (MachineLimits, MachineFeatures) {
    let mut limits = MachineLimits::default();
    let mut features = MachineFeatures::default();
    let mut current_section = "";
    let mut current_axis = "";

    for line in yaml.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.chars().take_while(|c| c.is_whitespace()).count();
        if indent == 0 && trimmed.ends_with(':') {
            current_section = trimmed.trim_end_matches(':').trim();
            current_axis = "";
            features.has_laser |= current_section.eq_ignore_ascii_case("laser");
            continue;
        }
        if current_section == "axes" && indent <= 4 && trimmed.ends_with(':') {
            // 只认轴名（x / y / z / a / b / c）；同级非轴段重置 current_axis，`homing:`/`motor0:` 子段不改当前轴
            let name = trimmed.trim_end_matches(':').trim();
            if matches!(name, "x" | "y" | "z" | "a" | "b" | "c") {
                current_axis = name;
            } else if indent <= 2 {
                current_axis = "";
            }
            continue;
        }

        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim();
            let val = v.trim();
            let pin_wired = !val.is_empty() && !val.eq_ignore_ascii_case("no_pin");
            if current_section == "macros" {
                let has_body = !val.is_empty() && val != "\"\"" && val != "''";
                match key {
                    "after_unlock" => features.has_after_unlock_macro = has_body,
                    "after_homing" => features.has_after_homing_macro = has_body,
                    "after_reset" => features.has_after_reset_macro = has_body,
                    _ => {}
                }
                continue;
            }
            if current_section == "control" && matches!(key, "estop_pin" | "fault_pin") {
                features.has_estop_or_fault_pin |= pin_wired;
                continue;
            }
            if current_section == "start" && key == "must_home" {
                features.must_home = val.eq_ignore_ascii_case("true");
                continue;
            }
            if current_section.eq_ignore_ascii_case("laser") && key == "off_on_alarm" {
                features.laser_off_on_alarm = Some(val.eq_ignore_ascii_case("true"));
                continue;
            }
            if indent == 0 {
                match key {
                    "planner_blocks" => features.planner_blocks = val.parse().ok(),
                    "use_line_numbers" => {
                        features.use_line_numbers = val.eq_ignore_ascii_case("true")
                    }
                    "verbose_errors" => {
                        features.verbose_errors = val.eq_ignore_ascii_case("true");
                        features.verbose_errors_disabled = val.eq_ignore_ascii_case("false");
                    }
                    _ => {}
                }
            }
            let positive = || {
                val.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.0)
            };
            if key == "max_rate_mm_per_min" || key == "acceleration_mm_per_sec2" {
                let rate = key == "max_rate_mm_per_min";
                match (current_axis, rate) {
                    ("x", true) => limits.x_max_rate_mm_per_min = positive(),
                    ("y", true) => limits.y_max_rate_mm_per_min = positive(),
                    ("x", false) => limits.x_accel_mm_s2 = positive(),
                    ("y", false) => limits.y_accel_mm_s2 = positive(),
                    _ => {}
                }
                continue;
            }
            if key == "junction_deviation_mm" {
                limits.junction_deviation_mm = positive();
                continue;
            }
            if key == "max_travel_mm" {
                if let Some(num) = positive() {
                    match current_axis {
                        "x" => limits.x_travel_mm = Some(num),
                        "y" => limits.y_travel_mm = Some(num),
                        "z" => limits.z_travel_mm = Some(num),
                        _ => {}
                    }
                }
            } else if key == "speed_map" {
                features.speed_map = Some(val.to_string());
            } else if (key.contains("air_assist") || key == "flood_pin" || key == "mist_pin")
                && !val.eq_ignore_ascii_case("no_pin")
                && !val.is_empty()
            {
                features.has_air_assist = true;
            } else if (key.contains("door") || key == "safety_door_pin")
                && !val.eq_ignore_ascii_case("no_pin")
                && !val.is_empty()
            {
                features.has_door = true;
            }
        }
    }
    (limits, features)
}

/// 从标准 GRBL $$ 设置行中提取 X/Y 行程（$130 为 X 行程，$131 为 Y 行程），
/// 以及 `$110/$111` 最大速度、`$120/$121` 加速度、`$11` 转角偏差（2026-11-16）
pub fn parse_grbl_settings_limits(settings: &[String]) -> MachineLimits {
    let mut limits = MachineLimits::default();
    for line in settings {
        let positive = |prefix: &str| {
            line.strip_prefix(prefix)
                .and_then(|val| val.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
        };
        for (prefix, slot) in [
            ("$110=", &mut limits.x_max_rate_mm_per_min),
            ("$111=", &mut limits.y_max_rate_mm_per_min),
            ("$120=", &mut limits.x_accel_mm_s2),
            ("$121=", &mut limits.y_accel_mm_s2),
            ("$11=", &mut limits.junction_deviation_mm),
        ] {
            if line.starts_with(prefix) {
                *slot = positive(prefix);
            }
        }
        if let Some(val) = line.strip_prefix("$130=") {
            limits.x_travel_mm = val
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0);
        } else if let Some(val) = line.strip_prefix("$131=") {
            limits.y_travel_mm = val
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0);
        } else if let Some(val) = line.strip_prefix("$132=") {
            limits.z_travel_mm = val
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0);
        }
    }
    limits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fluidnc_yaml_and_grbl_limits_extract_limits_and_features() {
        let yaml = r#"
axes:
  x:
    steps_per_mm: 80.000
    max_rate_mm_per_min: 6000.000
    acceleration_mm_per_sec2: 800.000
    max_travel_mm: 300.000
    homing:
      cycle: 2
    motor0:
      limit_neg_pin: gpio.17
  y:
    steps_per_mm: 80.000
    acceleration_mm_per_sec2: 600.000
    max_travel_mm: 200.000
  z:
    max_travel_mm: 50.000
laser:
  speed_map: 0=0% 1000=100%
coolant:
  flood_pin: gpio.22
control:
  safety_door_pin: gpio.35
"#;
        let (limits, features) = parse_fluidnc_yaml(yaml);
        assert_eq!(limits.x_travel_mm, Some(300.0));
        assert_eq!(limits.y_travel_mm, Some(200.0));
        assert_eq!(limits.z_travel_mm, Some(50.0));
        assert_eq!(limits.x_max_rate_mm_per_min, Some(6000.0));
        assert_eq!(limits.x_accel_mm_s2, Some(800.0));
        assert_eq!(limits.y_accel_mm_s2, Some(600.0));
        assert_eq!(features.speed_map.as_deref(), Some("0=0% 1000=100%"));
        assert!(features.has_air_assist);
        assert!(features.has_door);

        let grbl_settings = vec![
            "$130=280.000".to_string(),
            "$131=180.000".to_string(),
            "$132=45.000".to_string(),
            "$11=0.010".to_string(),
            "$110=5000.000".to_string(),
            "$111=4000.000".to_string(),
            "$120=1500.000".to_string(),
            "$121=1200.000".to_string(),
            "$1=25".to_string(),
        ];
        let grbl_limits = parse_grbl_settings_limits(&grbl_settings);
        assert_eq!(grbl_limits.x_max_rate_mm_per_min, Some(5000.0));
        assert_eq!(grbl_limits.y_max_rate_mm_per_min, Some(4000.0));
        assert_eq!(grbl_limits.x_accel_mm_s2, Some(1500.0));
        assert_eq!(grbl_limits.y_accel_mm_s2, Some(1200.0));
        assert_eq!(grbl_limits.junction_deviation_mm, Some(0.01));
        assert_eq!(grbl_limits.x_travel_mm, Some(280.0));
        assert_eq!(grbl_limits.y_travel_mm, Some(180.0));
        assert_eq!(grbl_limits.z_travel_mm, Some(45.0));
    }

    /// 2026-11-09：FluidNC `Laser.off_on_alarm` 默认 false，配置了激光段却没开时握手摘要给出提醒。
    #[test]
    fn laser_off_on_alarm_is_read_from_config_and_warned_when_off() {
        let missing = "Laser:\n  pwm_hz: 5000\n  speed_map: 0=0% 1000=100%\n";
        let (_, features) = parse_fluidnc_yaml(missing);
        assert!(features.has_laser);
        assert_eq!(features.laser_off_on_alarm, None);
        assert!(
            laser_off_on_alarm_warning(&features)
                .unwrap()
                .contains("off_on_alarm: true")
        );

        let off = "laser:\n  off_on_alarm: false\n";
        let (_, features) = parse_fluidnc_yaml(off);
        assert_eq!(features.laser_off_on_alarm, Some(false), "段名大小写都认");
        assert!(laser_off_on_alarm_warning(&features).is_some());

        let on = "Laser:\n  off_on_alarm: true\n";
        let (_, features) = parse_fluidnc_yaml(on);
        assert_eq!(features.laser_off_on_alarm, Some(true));
        assert!(laser_off_on_alarm_warning(&features).is_none());

        let (_, features) =
            parse_fluidnc_yaml("verbose_errors: false\nPWM:\n  output_pin: gpio.17\n");
        assert!(
            features.verbose_errors_disabled,
            "明确关掉 verbose_errors 要提醒"
        );
        let (_, features) = parse_fluidnc_yaml("Laser:\n  off_on_alarm: true\n");
        assert!(
            !features.verbose_errors_disabled,
            "没写这一项按固件默认（开）处理，不提醒"
        );
        let (_, features) = parse_fluidnc_yaml("PWM:\n  output_pin: gpio.17\n");
        assert!(!features.has_laser);
        assert!(
            laser_off_on_alarm_warning(&features).is_none(),
            "不是激光主轴不提醒"
        );
    }

    // -----------------------------------------------------------------------
    // FluidNC 适配：状态词 / 引脚 / 行长 / 注释 / SD 完成串 / 说明行 / $10 窗口 / $CD / $RI
    // -----------------------------------------------------------------------

    #[test]
    fn status_report_parser_covers_fluidnc_states_pins_buffer_and_sd() {
        let r =
            parse_status_report("<Home|MPos:0,0,0|Bf:15,1200|Pn:XE|SD:12.50,/sd/a.nc>").unwrap();
        assert_eq!(r.activity, MachineActivity::Busy);
        assert_eq!(r.pins, "XE");
        assert_eq!(r.blocking_pins(), vec!["急停按钮 (E)"]);
        assert_eq!(r.planner_free, Some(15));
        assert_eq!(r.rx_buffer_free, Some(1200));
        assert_eq!(
            r.sd,
            Some(SdProgress::Running {
                percent: 12.5,
                path: "/sd/a.nc".into()
            })
        );
        assert!(r.has_sd_job());

        let sent = parse_status_report("<Idle|MPos:0,0,0|SD: a.nc: Sent>").unwrap();
        assert_eq!(
            sent.sd,
            Some(SdProgress::Sent {
                name: "a.nc".into()
            })
        );
        assert!(!sent.has_sd_job(), "Sent 不是运行中");

        for (line, act) in [
            ("<Idle>", MachineActivity::Idle),
            ("<Run>", MachineActivity::Busy),
            ("<Jog>", MachineActivity::Busy),
            ("<Starting>", MachineActivity::Busy),
            ("<Hold:0>", MachineActivity::Held),
            ("<Hold:1>", MachineActivity::Held),
            ("<Door:1>", MachineActivity::DoorOpen),
            ("<Alarm>", MachineActivity::Alarm),
            ("<Sleep>", MachineActivity::Sleep),
            ("<Check>", MachineActivity::Check),
        ] {
            assert_eq!(parse_status_report(line).unwrap().activity, act, "{line}");
        }
        assert!(parse_status_report("ok").is_none());
        assert!(parse_status_report("[MSG:INFO: x]").is_none());
    }

    #[test]
    fn fluidnc_yaml_exposes_macros_estop_and_planner_fields() {
        let yaml = "control:\n  estop_pin: gpio.4:low\n  fault_pin: NO_PIN\nmacros:\n  after_unlock: G0 X0 Y0\n  after_homing:\n  after_reset: \"\"\nstart:\n  must_home: true\nplanner_blocks: 32\nuse_line_numbers: true\nverbose_errors: true\n";
        let (_, f) = parse_fluidnc_yaml(yaml);
        assert!(f.has_estop_or_fault_pin);
        assert!(f.has_after_unlock_macro);
        assert!(!f.has_after_homing_macro);
        assert!(!f.has_after_reset_macro);
        assert!(f.must_home);
        assert_eq!(f.planner_blocks, Some(32));
        assert!(f.use_line_numbers && f.verbose_errors);
    }
}
