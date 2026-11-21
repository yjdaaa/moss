use std::sync::atomic::Ordering;

use super::*;
use crate::*;

fn fake(lines: &[&str]) -> FakeGrbl {
    FakeGrbl::new(lines.iter().map(|line| (*line).to_owned()))
}

/// 2026-11-09：局域网扫描只认按 GRBL / FluidNC 回应的设备；只发只读的 `?` 与 `$I`。
#[test]
fn identify_grbl_accepts_only_grbl_replies_and_sends_read_only_queries() {
    let fluidnc = fake(&[
        "<Idle|MPos:0.000,0.000,0.000|FS:0,0>",
        "[VER:4.1 FluidNC v4.1.1 (esp32-wifi) :yisu",
        "[OPT:PHS",
        "[CLUSTER:16",
        "[MSG:Machine: Yisu V1.0]",
        "[MSG:Mode=STA:SSID=lab:Status=Connected:IP=192.0.2.20:MAC=00-00-00-00-00-00]",
        "ok",
    ]);
    let identity = identify_grbl(&fluidnc, Duration::from_millis(200)).expect("FluidNC 应被识别");
    assert_eq!(identity.firmware.as_deref(), Some("FluidNC v4.1.1"));
    assert_eq!(identity.state.as_deref(), Some("Idle"));
    assert_eq!(identity.machine.as_deref(), Some("Yisu V1.0"));
    assert_eq!(
        firmware_from_line("[VER:3.7.8 FluidNC v3.7.8:]").as_deref(),
        Some("FluidNC v3.7.8")
    );
    assert_eq!(
        firmware_from_line("Grbl 4.1 [FluidNC v4.1.1 (esp32-wifi) '$' for help]").as_deref(),
        Some("FluidNC v4.1.1"),
        "启动横幅里的 FluidNC 版本"
    );
    assert_eq!(fluidnc.realtime(), vec![b'?'], "只发实时状态查询");
    assert_eq!(
        fluidnc.lines(),
        vec!["$I".to_owned()],
        "只发读固件信息，不发任何运动或出光命令"
    );

    let grbl = fake(&["Grbl 1.1h ['$' for help]", "<Alarm|MPos:0,0,0|FS:0,0>"]);
    let identity = identify_grbl(&grbl, Duration::from_millis(200)).unwrap();
    assert_eq!(identity.firmware.as_deref(), Some("Grbl 1.1h"));
    assert_eq!(identity.state.as_deref(), Some("Alarm"));
    let classic = fake(&["[VER:1.1h.20190825:]", "ok"]);
    assert_eq!(
        identify_grbl(&classic, Duration::from_millis(200))
            .unwrap()
            .firmware
            .as_deref(),
        Some("Grbl 1.1h")
    );

    // 回了别的协议的横幅：立即判定，不等时限
    let started = Instant::now();
    assert!(
        identify_grbl(
            &fake(&["login: ", "<Idle|MPos:0,0,0>"]),
            Duration::from_secs(3)
        )
        .is_none()
    );
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    for other in [
        vec!["HTTP/1.1 400 Bad Request"],
        vec!["SSH-2.0-OpenSSH_9.6"],
        vec!["ok"],
        vec![],
    ] {
        assert!(
            identify_grbl(&fake(&other), Duration::from_millis(100)).is_none(),
            "{other:?} 不是雕刻机"
        );
    }
}

#[test]
fn subnet_scan_lists_other_hosts_and_finds_open_ports() {
    let hosts = subnet_hosts(std::net::Ipv4Addr::new(192, 168, 1, 37));
    assert_eq!(hosts.len(), 253);
    assert!(
        !hosts.contains(&std::net::Ipv4Addr::new(192, 168, 1, 37)),
        "不含本机"
    );
    assert_eq!(
        hosts.first(),
        Some(&std::net::Ipv4Addr::new(192, 168, 1, 1))
    );
    assert_eq!(
        hosts.last(),
        Some(&std::net::Ipv4Addr::new(192, 168, 1, 254))
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let open = scan_hosts_port(
        vec![
            std::net::Ipv4Addr::LOCALHOST,
            std::net::Ipv4Addr::new(127, 0, 0, 2),
        ],
        port,
        Duration::from_millis(300),
    );
    assert!(open.contains(&std::net::Ipv4Addr::LOCALHOST), "{open:?}");
    // 只取局域网网卡：都是私有地址、不是回环（代理 TUN 的 198.18.x 等虚拟地址不会出现）
    for ip in local_lan_ipv4() {
        assert!(ip.is_private() && !ip.is_loopback(), "{ip}");
    }
}

#[test]
fn available_serial_ports_returns_sorted_unique_list_in_process() {
    let started = Instant::now();
    let ports = available_serial_ports();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "进程内枚举串口耗时过长: {:?}",
        started.elapsed()
    );
    let mut sorted = ports.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(ports, sorted);
    assert!(ports.iter().all(|name| !name.trim().is_empty()));
}

#[test]
fn handshake_recovers_after_boot_banner_without_duplicating_welcome_banner_ack() {
    // 1. FluidNC Telnet / 已就绪串口先吐欢迎横幅但未丢首条 $$：绝不能盲补第二条 $$ 污染后续 ACK
    let welcome = FakeGrbl::new(
        [
            "Grbl 3.7 [FluidNC v3.7.12 '$' for help]",
            "$30=1000",
            "$32=1",
            "$130=100",
            "ok",
            "axes:",
            "  x:",
            "    max_travel_mm: 220",
            "ok",
        ]
        .map(str::to_owned),
    );
    let mut welcome_dev = DeviceConnection::new(welcome.clone());
    let settings = welcome_dev
        .handshake_with_deadline(Duration::from_secs(1))
        .expect("带欢迎横幅的正常握手应一次通过");
    assert_eq!(settings.len(), 3);
    assert_eq!(welcome.lines(), vec!["$$".to_owned()]);
    assert_eq!(welcome_dev.load_features(Duration::from_secs(1)), Ok(true));
    assert_eq!(welcome_dev.travel_limits.x_travel_mm, Some(220.0));

    // 2. USB 串口 DTR/RTS 软重启：引导期超时丢弃首条 $$，吐出启动横幅后沉默超时，此时补发一次 $$ 成功握手
    struct DtrRebootGrbl {
        writes: Mutex<Vec<String>>,
        reads: std::sync::atomic::AtomicUsize,
    }
    impl DeviceTransport for DtrRebootGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.writes.lock().unwrap().push(line.to_owned());
            Ok(())
        }
        fn write_realtime(&self, _cmd: RealtimeCommand) -> Result<(), String> {
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            match self.reads.fetch_add(1, Ordering::Relaxed) {
                // 引导期（例如 >1.0s）连续读超时，不得提前耗掉重试机会
                0 | 1 => Err("timed out".into()),
                // 板卡重启完成吐出横幅（首条 $$ 已被引导程序丢弃）
                2 => Ok(Some("Grbl 1.1h ['$' for help]".into())),
                // 横幅后无设置返回（读超时），触发补发一次 $$
                3 => Err("timed out".into()),
                4 => Ok(Some("$30=1000".into())),
                5 => Ok(Some("$32=1".into())),
                6 => Ok(Some("$130=100".into())),
                7 => Ok(Some("ok".into())),
                _ => Ok(None),
            }
        }
    }
    let dtr = DtrRebootGrbl {
        writes: Mutex::new(Vec::new()),
        reads: std::sync::atomic::AtomicUsize::new(0),
    };
    let mut device = DeviceConnection::new(dtr);
    let settings = device
        .handshake_with_deadline(Duration::from_secs(2))
        .expect("DTR 重启横幅后沉默超时应补发一次 $$ 完成握手");
    assert_eq!(settings.len(), 3);
    assert_eq!(
        *device.transport.writes.lock().unwrap(),
        vec!["$$".to_owned(), "$$".to_owned()]
    );

    // 3. 仅输出了启动横幅而未返回任何 $ 设置行时，不得谎报 $32=1 门禁未通过
    let banner_only = FakeGrbl::new(["Grbl 1.1h ['$' for help]"].map(str::to_owned));
    let mut dev2 = DeviceConnection::new(banner_only);
    let err = dev2
        .handshake_with_deadline(Duration::from_millis(200))
        .unwrap_err();
    assert!(!err.contains("$32=1"), "不应误报 $32=1：{err}");
    assert!(err.contains("未收到 GRBL 固件设置响应"), "{err}");
}

#[test]
fn wait_until_idle_extends_deadline_while_axes_actively_progress() {
    struct SlowDrainGrbl {
        step: std::sync::atomic::AtomicUsize,
    }
    impl DeviceTransport for SlowDrainGrbl {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn write_realtime(&self, _cmd: RealtimeCommand) -> Result<(), String> {
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            let idx = self.step.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(35));
            if idx < 6 {
                Ok(Some(format!("<Run|MPos:{idx}.500,0.000,0.000>")))
            } else {
                Ok(Some("<Idle|MPos:6.000,0.000,0.000>".into()))
            }
        }
    }
    let mut device = DeviceConnection::new(SlowDrainGrbl {
        step: std::sync::atomic::AtomicUsize::new(0),
    });
    device.state = DeviceState::Ready;
    // 单段无进展超时仅 90ms，但总耗时约 245ms：因 MPos 每帧都在前进，不应中途误超时
    device
        .wait_until_idle(Duration::from_millis(90), None)
        .expect("轴坐标持续前进时不应在无进展时限处误判超时");
    assert_eq!(device.last_mpos, Some((6.0, 0.0, 0.0)));
}

#[test]
fn verify_job_ready_for_resume_unlocks_alarm_and_records_mpos() {
    let fake = FakeGrbl::new(
        [
            "$30=1000",
            "$32=1",
            "ok", // handshake
            "<Alarm|MPos:42.500,18.250,0.000>",
            "ok", // $X ack
            "<Idle|MPos:42.500,18.250,0.000>",
            "<Idle|MPos:42.500,18.250,0.000>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let scale = device
        .verify_job_ready_for_resume(Duration::from_secs(1))
        .unwrap();
    assert_eq!(scale, 1000.0);
    assert_eq!(device.last_mpos, Some((42.5, 18.25, 0.0)));
    assert_eq!(fake.lines(), vec!["$$".to_owned(), "$X".to_owned()]);
}
