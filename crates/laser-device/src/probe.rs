use std::time::Duration;

use crate::{
    ConnectionSpec, DeviceConnection, DeviceIdentity, DeviceState, DeviceTransport,
    MachineFeatures, MachineLimits, laser_off_on_alarm_warning, open_transport,
    parse_grbl_settings_limits,
};

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
}
