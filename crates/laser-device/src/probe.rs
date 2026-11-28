use std::time::Duration;

use crate::{
    ConnectionSpec, DeviceConnection, DeviceIdentity, DeviceState, DeviceTransport,
    MachineFeatures, MachineLimits, laser_off_on_alarm_warning, open_transport,
    parse_grbl_laser_mode, parse_grbl_settings_limits, parse_grbl_spindle_max,
};

/// 有效行程缺省值（mm，翼宿 V1.0 默认幅面，与工作台一致）。
pub const DEFAULT_WORK_AREA_MM: f64 = 100.0;

/// 单轴有效行程的取值来源（严格按“手填 → 固件 → 默认 100 mm”顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TravelSource {
    Manual,
    Firmware,
    Default,
}

impl TravelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Firmware => "firmware",
            Self::Default => "default",
        }
    }

    pub fn label_zh(self) -> &'static str {
        match self {
            Self::Manual => "手填",
            Self::Firmware => "固件",
            Self::Default => "默认",
        }
    }
}

/// 单轴有效行程与来源。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveAxisTravel {
    pub mm: f64,
    pub source: TravelSource,
}

/// 整理后的机器能力摘要（r77）：所有固件字段仅取真实读回值，读不到一律为 `None`（展示为“未知”），不猜。
#[derive(Debug, Clone, PartialEq)]
pub struct MachineCapabilities {
    pub x_travel: EffectiveAxisTravel,
    pub y_travel: EffectiveAxisTravel,
    pub firmware_x_travel_mm: Option<f64>,
    pub firmware_y_travel_mm: Option<f64>,
    pub homing_enabled: Option<bool>,
    pub soft_limits_enabled: Option<bool>,
    pub spindle_max_s: Option<f64>,
    pub laser_mode_enabled: Option<bool>,
    pub firmware_type: Option<String>,
}

fn format_cap_number(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if (rounded.fract()).abs() < 1e-6 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

fn format_bool_zh(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "已开启",
        Some(false) => "未开启",
        None => "未知",
    }
}

fn resolve_axis_travel(manual: Option<f64>, firmware: Option<f64>) -> EffectiveAxisTravel {
    if let Some(mm) = manual.filter(|v| v.is_finite() && *v > 0.0) {
        EffectiveAxisTravel {
            mm,
            source: TravelSource::Manual,
        }
    } else if let Some(mm) = firmware.filter(|v| v.is_finite() && *v > 0.0) {
        EffectiveAxisTravel {
            mm,
            source: TravelSource::Firmware,
        }
    } else {
        EffectiveAxisTravel {
            mm: DEFAULT_WORK_AREA_MM,
            source: TravelSource::Default,
        }
    }
}

fn format_firmware_type(
    identity: Option<&DeviceIdentity>,
    features: Option<&MachineFeatures>,
) -> Option<String> {
    if let Some(id) = identity {
        let fw = id
            .firmware
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let ver = id
            .version
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let head = match (fw, ver) {
            (Some(f), Some(v)) if f.contains(v) => Some(f.to_owned()),
            (Some(f), Some(v)) => Some(format!("{f} {v}")),
            (Some(f), None) => Some(f.to_owned()),
            (None, Some(v)) => Some(v.to_owned()),
            (None, None) => None,
        };
        if head.is_some() {
            return head;
        }
    }
    features.map(|_| "FluidNC".to_owned())
}

/// 根据本次探测结果与可选手填/记住行程整理机器能力摘要。
///
/// - 有效行程按“手填 → 固件 → 默认 100 mm”逐轴取值并标明来源；
/// - 回零（`$22` / `$CD`）、软限位（`$20` / `$CD`）、S 上限（`$30`）、激光模式（`$32`）、固件类型
///   仅以固件真实读回值为准，读不到一律保留 `None`（文案写“未知”）。
#[allow(clippy::too_many_arguments)]
pub fn build_machine_capabilities(
    manual_x_mm: Option<f64>,
    manual_y_mm: Option<f64>,
    firmware_limits: Option<&MachineLimits>,
    remembered_firmware_travel: (Option<f64>, Option<f64>),
    firmware_settings: &[String],
    features: Option<&MachineFeatures>,
    identity: Option<&DeviceIdentity>,
) -> MachineCapabilities {
    let fw_x = firmware_limits
        .and_then(|l| l.x_travel_mm)
        .or(remembered_firmware_travel.0)
        .filter(|v| v.is_finite() && *v > 0.0);
    let fw_y = firmware_limits
        .and_then(|l| l.y_travel_mm)
        .or(remembered_firmware_travel.1)
        .filter(|v| v.is_finite() && *v > 0.0);
    MachineCapabilities {
        x_travel: resolve_axis_travel(manual_x_mm, fw_x),
        y_travel: resolve_axis_travel(manual_y_mm, fw_y),
        firmware_x_travel_mm: fw_x,
        firmware_y_travel_mm: fw_y,
        homing_enabled: firmware_limits.and_then(|l| l.homing_enabled),
        soft_limits_enabled: firmware_limits.and_then(|l| l.soft_limits_enabled),
        spindle_max_s: parse_grbl_spindle_max(firmware_settings),
        laser_mode_enabled: parse_grbl_laser_mode(firmware_settings),
        firmware_type: format_firmware_type(identity, features),
    }
}

impl MachineCapabilities {
    pub fn homing_label(&self) -> &'static str {
        format_bool_zh(self.homing_enabled)
    }

    pub fn soft_limits_label(&self) -> &'static str {
        format_bool_zh(self.soft_limits_enabled)
    }

    pub fn laser_mode_label(&self) -> &'static str {
        format_bool_zh(self.laser_mode_enabled)
    }

    pub fn spindle_max_s_label(&self) -> String {
        self.spindle_max_s
            .map(format_cap_number)
            .unwrap_or_else(|| "未知".to_owned())
    }

    pub fn firmware_type_label(&self) -> &str {
        self.firmware_type.as_deref().unwrap_or("未知")
    }

    /// 供语音播报与 AI 直接复述的一句中文能力摘要。
    pub fn summary_zh(&self) -> String {
        let travel = if self.x_travel.source == self.y_travel.source {
            format!(
                "有效行程 {}×{} mm（{}）",
                format_cap_number(self.x_travel.mm),
                format_cap_number(self.y_travel.mm),
                self.x_travel.source.label_zh()
            )
        } else {
            format!(
                "有效行程 X {} mm（{}）、Y {} mm（{}）",
                format_cap_number(self.x_travel.mm),
                self.x_travel.source.label_zh(),
                format_cap_number(self.y_travel.mm),
                self.y_travel.source.label_zh()
            )
        };
        format!(
            "{travel}，回零{}，软限位{}，S 上限{}，激光模式{}，固件{}",
            self.homing_label(),
            self.soft_limits_label(),
            self.spindle_max_s_label(),
            self.laser_mode_label(),
            self.firmware_type_label(),
        )
    }
}

/// 握手探测报告：只发 `$$` / `$CD` / `$I` 读取固件设置与身份，不移动轴、不出光。
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeReport {
    pub mode: &'static str,
    pub target: String,
    pub connected: bool,
    pub laser_mode_ok: bool,
    pub firmware_settings: Vec<String>,
    pub machine_limits: Option<MachineLimits>,
    pub features: Option<MachineFeatures>,
    pub identity: Option<DeviceIdentity>,
    pub summary: String,
}

/// 进程内枚举系统可用串口名（Windows 直接通过 Win32 SetupAPI / 注册表读取，不拉起外部终端子进程，也不打开串口句柄）。
pub fn available_serial_ports() -> Vec<String> {
    let mut ports: Vec<String> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|info| info.port_name)
        .filter(|name| !name.trim().is_empty())
        .collect();
    ports.sort();
    ports.dedup();
    ports
}

/// 打开连接并执行有界握手，返回人类可读的探测报告。
pub fn probe_device(spec: &ConnectionSpec, timeout: Duration) -> ProbeReport {
    let target = spec.describe();
    let mode = spec.mode();
    let label = if mode == "network" {
        "网络"
    } else {
        "串口"
    };
    let transport = match open_transport(spec, timeout) {
        Ok(t) => t,
        Err(error) => {
            return ProbeReport {
                mode,
                target,
                connected: false,
                laser_mode_ok: false,
                firmware_settings: Vec::new(),
                machine_limits: None,
                features: None,
                identity: None,
                summary: format!("{label} 连接失败: {error}"),
            };
        }
    };
    probe_opened_device(spec, transport, timeout)
}

/// 对已打开的传输通道执行有界握手与 `$CD` 探测，便于上层在释放设备打开锁后再执行耗时 I/O。
pub fn probe_opened_device(
    spec: &ConnectionSpec,
    transport: Box<dyn DeviceTransport>,
    timeout: Duration,
) -> ProbeReport {
    let target = spec.describe();
    let mode = spec.mode();
    let label = if mode == "network" {
        "网络"
    } else {
        "串口"
    };
    let mut device = DeviceConnection::new(transport);
    match device.handshake_with_deadline(timeout) {
        Ok(settings) => {
            let fallback_limits = parse_grbl_settings_limits(&settings);
            // FluidNC：再读 $CD 拿 YAML 配置（宏、急停引脚、planner_blocks 等）；仅旧 GRBL 回 error:3 时静默跳过
            let (yaml_limits, features) = match device.fetch_fluidnc_config(timeout) {
                Ok(Some((limits, features))) => {
                    device.features = Some(features.clone());
                    (Some(limits), Some(features))
                }
                Ok(None) => (None, None),
                Err(error) => {
                    let alarm = device.state() == DeviceState::Alarm;
                    return ProbeReport {
                        mode,
                        target: target.clone(),
                        connected: alarm,
                        laser_mode_ok: false,
                        firmware_settings: Vec::new(),
                        machine_limits: None,
                        features: None,
                        identity: None,
                        summary: if alarm {
                            format!("已连接 {target}，但读取 $CD 时触发报警: {error}")
                        } else {
                            format!("{target} 读取 $CD 异常: {error}")
                        },
                    };
                }
            };
            let identity = device.query_identity(timeout);
            let mut merged_limits = fallback_limits;
            if let Some(l) = yaml_limits {
                merged_limits.merge_prefer(&l);
            }
            let limits = (merged_limits != MachineLimits::default()).then_some(merged_limits);
            let mut summary = format!("{label} 握手成功 ({target})，固件 $32=1 激光模式就绪");
            if device.reports_buffer_state() == Some(false) {
                summary.push_str("；$10 未开启缓冲汇报（建议设 $10=3），发送窗口保持保守值（串口 127 字节，网络 FluidNC 不超过 256 字节）");
            }
            if let Some(f) = &features {
                if f.has_after_unlock_macro || f.has_after_homing_macro {
                    summary.push_str("；配置了解锁/回零后自动宏，解锁与回零将等待宏执行完毕");
                }
                if f.must_home {
                    summary.push_str("；start.must_home=true，上电后必须先回零");
                }
                if let Some(warning) = laser_off_on_alarm_warning(f) {
                    summary.push('；');
                    summary.push_str(warning);
                }
                if f.verbose_errors_disabled {
                    summary.push_str("；config.yaml 关掉了 verbose_errors：固件报错不再附原因，遇到新错误码只能看到编号，建议改回 verbose_errors: true");
                }
            }
            ProbeReport {
                mode,
                target: target.clone(),
                connected: true,
                laser_mode_ok: true,
                firmware_settings: settings,
                machine_limits: limits,
                features,
                identity,
                summary,
            }
        }
        Err(error) => {
            let alarm = device.state() == DeviceState::Alarm;
            ProbeReport {
                mode,
                target: target.clone(),
                connected: alarm,
                laser_mode_ok: false,
                firmware_settings: Vec::new(),
                machine_limits: None,
                features: None,
                identity: None,
                summary: if alarm {
                    format!("已连接 {target}，但 {error}")
                } else {
                    format!("{target} 无 GRBL 响应: {error}")
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SerialGrbl;

    #[test]
    fn serial_open_on_missing_port_reports_error_and_probe_says_not_connected() {
        let bogus = if cfg!(windows) {
            "COM254"
        } else {
            "/dev/ttyMOSS_NOT_EXIST"
        };
        assert!(SerialGrbl::open(bogus, 115_200).is_err());
        assert!(SerialGrbl::open("", 115_200).is_err());
        let report = probe_device(
            &ConnectionSpec::Serial {
                port: bogus.into(),
                baud_rate: 115_200,
            },
            Duration::from_millis(500),
        );
        assert_eq!(report.mode, "serial");
        assert!(!report.connected);
        assert!(
            report.summary.contains("串口 连接失败"),
            "{}",
            report.summary
        );
    }

    #[test]
    fn tcp_probe_handshakes_with_loopback_grbl_and_rejects_spindle_mode() {
        use std::io::{BufRead, BufReader, Write};
        for (laser_mode, expect_ok) in [(1, true), (0, false)] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut writer = stream;
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() && line.trim() == "$$" {
                        let _ = write!(writer, "$0=10\r\n$32={laser_mode}\r\nok\r\n");
                        line.clear();
                        if reader.read_line(&mut line).is_ok() && line.trim() == "$CD" {
                            let _ = write!(writer, "error:3\r\n");
                        }
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
            let report = probe_device(
                &ConnectionSpec::Tcp {
                    host: "127.0.0.1".into(),
                    port,
                },
                Duration::from_secs(2),
            );
            assert_eq!(report.mode, "network");
            assert_eq!(report.laser_mode_ok, expect_ok, "{}", report.summary);
            if expect_ok {
                assert!(report.connected);
                assert!(report.firmware_settings.iter().any(|l| l == "$32=1"));
            } else {
                assert!(report.summary.contains("$32=1"), "{}", report.summary);
            }
        }
        // 无监听端口：连接失败而非挂起
        let dead = probe_device(
            &ConnectionSpec::Tcp {
                host: "127.0.0.1".into(),
                port: 1,
            },
            Duration::from_millis(500),
        );
        assert!(!dead.connected);
        assert!(dead.summary.contains("连接失败"), "{}", dead.summary);
    }

    #[test]
    fn probe_reads_cd_after_handshake_and_reports_features() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok() && !line.is_empty() {
                    match line.trim() {
                        "$$" => {
                            let _ = write!(writer, "$10=1\r\n$30=1000\r\n$32=1\r\nok\r\n");
                        }
                        "$CD" => {
                            let _ = write!(
                                writer,
                                "macros:\r\n  after_unlock: G0 X0\r\naxes:\r\n  x:\r\n    max_travel_mm: 100\r\nok\r\n"
                            );
                        }
                        _ => {
                            let _ = write!(writer, "ok\r\n");
                        }
                    }
                    line.clear();
                }
            }
        });
        let report = probe_device(
            &ConnectionSpec::Tcp {
                host: "127.0.0.1".into(),
                port,
            },
            Duration::from_secs(2),
        );
        assert!(report.connected, "{}", report.summary);
        let f = report.features.expect("features from $CD");
        assert!(f.has_after_unlock_macro);
        assert_eq!(report.machine_limits.unwrap().x_travel_mm, Some(100.0));
        assert!(report.summary.contains("$10"), "{}", report.summary);
        assert!(report.summary.contains("自动宏"), "{}", report.summary);
    }

    #[test]
    fn probe_reads_dollar_i_identity_for_fluidnc_and_grbl() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok() && !line.is_empty() {
                    match line.trim() {
                        "$$" => {
                            let _ = write!(writer, "$20=1\r\n$22=1\r\n$23=3\r\n$32=1\r\nok\r\n");
                        }
                        "$CD" => {
                            let _ = write!(
                                writer,
                                "axes:\r\n  x:\r\n    max_travel_mm: 300\r\n    soft_limits: true\r\n  y:\r\n    max_travel_mm: 200\r\n    soft_limits: true\r\nok\r\n"
                            );
                        }
                        "$I" => {
                            let _ = write!(
                                writer,
                                "[VER:4.1 FluidNC v4.1.1 (esp32-wifi) :g1234]\r\n[MSG:Machine: Yisu-3020]\r\nok\r\n"
                            );
                        }
                        _ => {
                            let _ = write!(writer, "ok\r\n");
                        }
                    }
                    line.clear();
                }
            }
        });
        let report = probe_device(
            &ConnectionSpec::Tcp {
                host: "127.0.0.1".into(),
                port,
            },
            Duration::from_secs(2),
        );
        assert!(report.connected, "{}", report.summary);
        assert_eq!(
            report.identity,
            Some(DeviceIdentity {
                firmware: Some("FluidNC".into()),
                version: Some("v4.1.1".into()),
                machine_name: Some("Yisu-3020".into()),
            })
        );
        let lim = report.machine_limits.expect("limits");
        assert_eq!(lim.soft_limits_enabled, Some(true));
        assert_eq!(lim.homing_dir_mask, Some(3));
    }

    #[test]
    fn machine_capabilities_resolve_sources_and_unknown_fields() {
        let limits = MachineLimits {
            x_travel_mm: Some(300.0),
            y_travel_mm: Some(200.0),
            homing_enabled: Some(true),
            soft_limits_enabled: Some(false),
            ..Default::default()
        };
        let settings = vec![
            "$20=0".into(),
            "$22=1".into(),
            "$30=1000".into(),
            "$32=1".into(),
        ];
        let identity = DeviceIdentity {
            firmware: Some("FluidNC".into()),
            version: Some("v4.1.1".into()),
            machine_name: Some("Yisu-3020".into()),
        };
        let full = build_machine_capabilities(
            Some(250.0),
            None,
            Some(&limits),
            (None, None),
            &settings,
            None,
            Some(&identity),
        );
        assert_eq!(full.x_travel.mm, 250.0);
        assert_eq!(full.x_travel.source, TravelSource::Manual);
        assert_eq!(full.y_travel.mm, 200.0);
        assert_eq!(full.y_travel.source, TravelSource::Firmware);
        assert_eq!(full.spindle_max_s, Some(1000.0));
        assert_eq!(full.laser_mode_enabled, Some(true));
        assert_eq!(full.firmware_type.as_deref(), Some("FluidNC v4.1.1"));
        let summary = full.summary_zh();
        assert!(
            summary.contains("有效行程 X 250 mm（手填）、Y 200 mm（固件）"),
            "{summary}"
        );
        assert!(summary.contains("回零已开启"), "{summary}");
        assert!(summary.contains("软限位未开启"), "{summary}");
        assert!(summary.contains("S 上限1000"), "{summary}");
        assert!(summary.contains("激光模式已开启"), "{summary}");
        assert!(summary.contains("固件FluidNC v4.1.1"), "{summary}");

        // 固件未报行程与各项设置时：行程回退默认 100 mm，其余全部写“未知”，绝不猜
        let empty = build_machine_capabilities(None, None, None, (None, None), &[], None, None);
        assert_eq!(empty.x_travel.mm, DEFAULT_WORK_AREA_MM);
        assert_eq!(empty.x_travel.source, TravelSource::Default);
        assert_eq!(empty.y_travel.mm, DEFAULT_WORK_AREA_MM);
        assert_eq!(empty.y_travel.source, TravelSource::Default);
        assert_eq!(empty.homing_label(), "未知");
        assert_eq!(empty.soft_limits_label(), "未知");
        assert_eq!(empty.spindle_max_s_label(), "未知");
        assert_eq!(empty.laser_mode_label(), "未知");
        assert_eq!(empty.firmware_type_label(), "未知");
        let empty_summary = empty.summary_zh();
        assert!(
            empty_summary.contains("有效行程 100×100 mm（默认）"),
            "{empty_summary}"
        );
        assert!(empty_summary.contains("回零未知"), "{empty_summary}");
        assert!(empty_summary.contains("软限位未知"), "{empty_summary}");
        assert!(empty_summary.contains("S 上限未知"), "{empty_summary}");
        assert!(empty_summary.contains("激光模式未知"), "{empty_summary}");
        assert!(empty_summary.contains("固件未知"), "{empty_summary}");
    }
}
