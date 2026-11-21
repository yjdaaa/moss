use super::*;
use crate::*;

#[test]
fn firmware_gate_rejects_spindle_mode() {
    let fake = FakeGrbl::new(["$32=0".to_string()]);
    let mut device = DeviceConnection::new(fake);
    assert!(device.handshake().is_err());
    assert_eq!(device.state(), DeviceState::Alarm);
}

#[test]
fn realtime_commands_are_separate_from_gcode() {
    let fake = FakeGrbl::new(["$32=1", "ok"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.send_line("M5").unwrap();
    device.emergency_stop().unwrap();
    assert_eq!(fake.lines(), vec!["$$".to_string(), "M5".to_string()]);
    assert_eq!(fake.realtime(), vec![0x18]);
    assert_eq!(device.state(), DeviceState::Alarm);
}

#[test]
fn cancel_orders_hold_status_and_reset() {
    let fake = FakeGrbl::new(["$32=1", "ok", "<Hold:0>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.cancel().unwrap();
    assert_eq!(fake.realtime(), vec![b'!', b'?', 0x18]);
    assert_eq!(device.state(), DeviceState::Alarm);
}

#[test]
fn stream_cancel_preserves_hold_write_failure_and_blocks_sequential_send() {
    struct FailedHold(FakeGrbl);

    impl DeviceTransport for FailedHold {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.0.write_line(line)
        }

        fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
            self.0.write_realtime(command)?;
            if command == RealtimeCommand::FeedHold {
                return Err("进给保持写入失败".into());
            }
            Ok(())
        }

        fn read_line(&self) -> Result<Option<String>, String> {
            self.0.read_line()
        }
    }

    let fake = FakeGrbl::new(std::iter::empty());
    let mut device = DeviceConnection::new(FailedHold(fake.clone()));
    device.state = DeviceState::Ready;
    device.auto_report_ms = 100;
    assert_eq!(device.cancel().unwrap_err(), "进给保持写入失败");
    assert_eq!(device.state(), DeviceState::Lost);
    assert_eq!(device.auto_report_ms, 0);
    assert!(device.send_line("M5").is_err());
    assert!(fake.lines().is_empty());
    assert_eq!(fake.realtime(), [b'!', 0x18]);
}

#[test]
fn unlock_and_home_recover_alarm_state() {
    // 解锁：ok 之后仍要等到 Idle（FluidNC 可能跑 after_unlock 宏），中间的 [MSG:INFO: Caution: Unlocked] 被忽略
    let fake = FakeGrbl::new(
        [
            "$32=1",
            "ok",
            "[MSG:INFO: Caution: Unlocked]",
            "ok",
            "<Run|MPos:0,0,0>",
            "<Idle|MPos:0,0,0>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.emergency_stop().unwrap();
    assert_eq!(device.state(), DeviceState::Alarm);

    device.unlock().unwrap();
    assert_eq!(device.state(), DeviceState::Ready);
    assert_eq!(fake.lines().last().unwrap(), "$X");

    // 回零：Home 状态视为忙碌持续等待，不再被当成报警
    let fake = FakeGrbl::new(
        [
            "$32=1",
            "ok",
            "ok",
            "<Home|MPos:0,0,0>",
            "<Home|MPos:1,0,0>",
            "<Idle|MPos:0,0,0>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.emergency_stop().unwrap();
    device.home().unwrap();
    assert_eq!(device.state(), DeviceState::Ready);
    assert_eq!(fake.lines().last().unwrap(), "$H");

    // 解锁只收到 ok 而机器一直不回 Idle：不得标记就绪
    let fake = FakeGrbl::new(["$32=1", "ok", "ok", "<Alarm|MPos:0,0,0>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.emergency_stop().unwrap();
    assert!(device.unlock().is_err());
    assert_ne!(device.state(), DeviceState::Ready);
}

#[test]
fn ack_wait_tolerates_status_reports_but_flags_lost_after_silent_heartbeat() {
    /// 响应耗尽后模拟"线路沉默"（读超时 Err）而非 EOF，贴近真实串口/TCP 行为
    struct SilentAfterDrain(FakeGrbl);
    impl DeviceTransport for SilentAfterDrain {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.0.write_line(line)
        }
        fn write_realtime(&self, c: RealtimeCommand) -> Result<(), String> {
            self.0.write_realtime(c)
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            match self.0.read_line()? {
                Some(line) => Ok(Some(line)),
                None => {
                    std::thread::sleep(Duration::from_millis(50));
                    Err("timed out".into())
                }
            }
        }
    }
    // 规划器满载：ack 前夹杂状态报告，不应误判失联
    let fake = FakeGrbl::new([
        "$32=1".into(),
        "ok".into(),
        "<Run|MPos:1,1,0>".into(),
        "ok".into(),
    ]);
    let mut device = DeviceConnection::new(SilentAfterDrain(fake.clone()));
    device.handshake().unwrap();
    assert!(device.send_line_wait_ack("G1 X1").is_ok());
    // 之后完全沉默：先发 ? 心跳，再 500ms 无字节 → Lost
    let started = Instant::now();
    let err = device.send_line_wait_ack("G1 X2").unwrap_err();
    assert!(err.contains("失联"), "{err}");
    assert_eq!(device.state(), DeviceState::Lost);
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(fake.realtime().contains(&b'?'));
}

#[test]
fn handshake_consumes_terminal_ack_and_all_settings() {
    let fake = FakeGrbl::new(["$32=1", "$30=1000", "$130=100", "ok", "error:2"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    let settings = device
        .handshake_with_deadline(Duration::from_secs(1))
        .unwrap();
    assert_eq!(settings.len(), 3);
    assert_eq!(device.max_spindle_speed, Some(1000.0));
    assert_eq!(device.send_line_wait_ack("M5").unwrap_err(), "error:2");
    assert_eq!(device.report_status_mask, None);
}

#[test]
fn handshake_without_terminal_ack_never_becomes_ready() {
    let mut device = DeviceConnection::new(FakeGrbl::new(["$32=1".into()]));
    assert!(device.handshake().unwrap_err().contains("缺少终止 ok"));
    assert_ne!(device.state(), DeviceState::Ready);
}

#[test]
fn preflight_requires_valid_power_scale_and_idle() {
    // 缺少 $30 或非有限正数：拒绝且不发任何指令（包括 ? 预检）
    for max in [None, Some("0"), Some("NaN"), Some("inf"), Some("-5")] {
        let mut responses: Vec<String> = max.map(|m| format!("$30={m}")).into_iter().collect();
        responses.extend(["$32=1".to_string(), "ok".to_string()]);
        let fake = FakeGrbl::new(responses);
        let mut device = DeviceConnection::new(fake.clone());
        device.handshake().unwrap();
        assert!(
            device
                .verify_job_ready_adaptive(Duration::from_secs(1))
                .unwrap_err()
                .contains("$30"),
            "{max:?}"
        );
        assert_eq!(fake.lines(), ["$$"]);
        assert!(fake.realtime().is_empty());
    }
    for state in [
        "<Run|MPos:0,0,0>",
        "<Alarm|MPos:0,0,0>",
        "<Hold:0>",
        "<Check>",
        "<Idle|SD:50,file.nc>",
    ] {
        let fake = FakeGrbl::new(["$30=1000", "$32=1", "ok", state].map(str::to_owned));
        let mut device = DeviceConnection::new(fake.clone());
        device.handshake().unwrap();
        assert!(
            device
                .verify_job_ready_adaptive(Duration::from_secs(1))
                .is_err(),
            "{state}"
        );
        assert_eq!(fake.lines(), ["$$"]);
    }
    let fake = FakeGrbl::new(["$30=1000", "$32=1", "ok", "<Idle|MPos:0,0,0>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    device.handshake().unwrap();
    assert_eq!(
        device
            .verify_job_ready_adaptive(Duration::from_secs(1))
            .unwrap(),
        1000.0
    );
}

#[test]
fn verify_job_ready_adaptive_accepts_valid_scales() {
    for max in ["255", "1000", "500"] {
        let fake = FakeGrbl::new([
            format!("$30={max}"),
            "$32=1".into(),
            "ok".into(),
            "<Idle>".into(),
        ]);
        let mut device = DeviceConnection::new(fake);
        device.handshake().unwrap();
        let scale = device
            .verify_job_ready_adaptive(Duration::from_secs(1))
            .unwrap();
        assert_eq!(scale, max.parse::<f64>().unwrap());
    }
}

#[test]
fn wait_until_sd_completed_tracks_percentage_and_finishes_on_idle() {
    let responses = [
        "<Run|MPos:10,20,0|SD:25.00,/sd/job.gcode>".into(),
        "<Run|MPos:30,40,0|SD:75.00,/sd/job.gcode>".into(),
        "<Idle|MPos:0,0,0>".into(),
    ];
    let fake = FakeGrbl::new(responses);
    let mut device = DeviceConnection::new(fake);
    device.state = DeviceState::Ready;
    let progress = Arc::new(JobProgress::default());
    progress.total_lines.store(100, Ordering::Relaxed);
    let cancel = CancellationToken::default();
    device
        .wait_until_sd_completed(Duration::from_secs(2), Some(&progress), Some(&cancel))
        .unwrap();
    assert_eq!(progress.lines_sent.load(Ordering::Relaxed), 100);
    assert_eq!(device.state(), DeviceState::Ready);
}

/// 2026-11-09：表里没有的新错误码也能看到原因——FluidNC 默认 `verbose_errors: true`，
/// 在 `error:N` 之前先发 `[MSG:ERR: …]`，上位机把它拼进报错（不额外发 `$E` 查询）。
#[test]
fn unknown_error_code_still_carries_firmware_explanation() {
    let fake = FakeGrbl::new(
        ["[MSG:ERR: Brand new firmware check failed]", "error:199"].map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake);
    device.state = DeviceState::Ready;
    let error = device.send_line_wait_ack("G1 X1 F600").unwrap_err();
    assert!(error.contains("error:199"), "{error}");
    assert!(
        error.contains("Brand new firmware check failed"),
        "带上固件自己的原因: {error}"
    );
}

/// 2026-11-09：按 FluidNC 源码规则（JSON 每满 100 个字符切一行；Telnet 原样、串口每段加 `[JSON:`；
/// 哈希为大写十六进制）构造 `$File/ShowHash` 输出，哈希被切成两行时仍能拼出并核对。
#[test]
fn sd_file_hash_is_reassembled_from_100_char_json_chunks() {
    let sha = "9F2C4E7A1B3D5F60718293A4B5C6D7E8F90A1B2C3D4E5F60718293A4B5C6D7E8";
    let json = format!(
        r#"{{"signature":{{"algorithm":"SHA2-256","value":"{sha}"}},"path":"/sd/moss_job.gcode"}}"#
    );
    let chunks: Vec<String> = json
        .as_bytes()
        .chunks(100)
        .map(|chunk| String::from_utf8(chunk.to_vec()).unwrap())
        .collect();
    assert!(
        chunks.len() >= 2 && !chunks[0].contains(sha),
        "哈希确实被切开"
    );
    for uart in [false, true] {
        let mut lines: Vec<String> = chunks
            .iter()
            .map(|chunk| {
                if uart {
                    format!("[JSON:{chunk}")
                } else {
                    chunk.clone()
                }
            })
            .collect();
        lines.push("ok".into());
        let mut device = DeviceConnection::new(FakeGrbl::new(lines.clone()));
        assert_eq!(
            device.verify_sd_file_hash(
                "/sd/moss_job.gcode",
                &sha.to_ascii_lowercase(),
                Duration::from_secs(1)
            ),
            Ok(true),
            "uart={uart}"
        );
        let mut device = DeviceConnection::new(FakeGrbl::new(lines));
        let mismatch = device
            .verify_sd_file_hash(
                "/sd/moss_job.gcode",
                &"0".repeat(64),
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(mismatch.contains("不匹配"), "{mismatch}");
    }
    // 固件没有返回哈希（未启用 HashFS）：仍按“没有哈希”处理，由上层拒绝执行
    let mut device = DeviceConnection::new(FakeGrbl::new(["ok".to_owned()]));
    assert_eq!(
        device.verify_sd_file_hash("/sd/x.gcode", &"0".repeat(64), Duration::from_secs(1)),
        Ok(false)
    );
}

#[test]
fn unknown_status_never_triggers_unlock_in_resume_preflight() {
    let fake =
        FakeGrbl::new(["$30=1000", "$32=1", "ok", "<Mystery|MPos:0,0,0>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let err = device
        .verify_job_ready_for_resume(Duration::from_secs(1))
        .unwrap_err();
    assert!(err.contains("未知状态"), "{err}");
    assert_eq!(
        fake.lines(),
        vec!["$$".to_owned()],
        "Unknown 状态绝不能触发自动 $X 解锁"
    );
}

#[test]
fn partial_cd_fails_and_single_axis_cd_preserves_existing_limits() {
    // 1. 仅配置 x 轴的 $CD 不得擦除 $$ 返回的 $131（y 行程）与加速度
    let fake = FakeGrbl::new(
        [
            "$30=1000",
            "$32=1",
            "$130=200",
            "$131=180",
            "$120=900",
            "ok",
            "axes:",
            "  x:",
            "    max_travel_mm: 250",
            "ok",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake);
    device.handshake().unwrap();
    assert_eq!(device.load_features(Duration::from_secs(1)), Ok(true));
    assert_eq!(device.travel_limits.x_travel_mm, Some(250.0));
    assert_eq!(device.travel_limits.y_travel_mm, Some(180.0));
    assert_eq!(device.travel_limits.x_accel_mm_s2, Some(900.0));

    // 2. 半截 $CD（缺少 ok）必须拒收且不得加载或擦除既有行程
    struct StallingCdTransport {
        reads: std::sync::atomic::AtomicUsize,
    }
    impl DeviceTransport for StallingCdTransport {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn write_realtime(&self, _command: RealtimeCommand) -> Result<(), String> {
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            match self.reads.fetch_add(1, Ordering::Relaxed) {
                0 => Ok(Some("$30=1000".into())),
                1 => Ok(Some("$32=1".into())),
                2 => Ok(Some("$130=300".into())),
                3 => Ok(Some("$131=220".into())),
                4 => Ok(Some("ok".into())),
                5 => Ok(Some("axes:".into())),
                6 => Ok(Some("  x:".into())),
                7 => Ok(Some("    max_travel_mm: 50".into())),
                _ => {
                    std::thread::sleep(Duration::from_millis(15));
                    Err("timed out".into())
                }
            }
        }
    }
    let mut dev2 = DeviceConnection::new(StallingCdTransport {
        reads: std::sync::atomic::AtomicUsize::new(0),
    });
    dev2.handshake().unwrap();
    assert!(
        dev2.load_features(Duration::from_millis(60)).is_err(),
        "未收到终止 ok 的半截 $CD 必须失败"
    );
    assert!(dev2.features.is_none());
    assert_eq!(dev2.travel_limits.x_travel_mm, Some(300.0));
    assert_eq!(dev2.travel_limits.y_travel_mm, Some(220.0));
    let dirty_err = dev2.send_line("G1 X1").unwrap_err();
    assert!(dirty_err.contains("未排空"), "{dirty_err}");
}

/// 2026-11-09：FluidNC 的 `$SD/Run` 回推迟应答（文件跑完才回 ok）。发出后直接监控：
/// 发出后的第一次空闲不能当完成，进入 SD 运行后按百分比推进度，收到推迟的 ok 后回到空闲才完成。
#[test]
fn run_sd_file_waits_for_start_then_completes_on_deferred_ok() {
    let responses = [
        "<Idle|MPos:0,0,0>",
        "<Run|MPos:10,20,0|SD:40.00,/sd/job.gcode>",
        "<Run|MPos:30,40,0|SD: job.gcode: Sent>",
        "ok",
        "<Idle|MPos:0,0,0>",
    ]
    .map(str::to_owned);
    let fake = FakeGrbl::new(responses);
    let mut device = DeviceConnection::new(fake.clone());
    device.state = DeviceState::Ready;
    let progress = Arc::new(JobProgress::default());
    progress.total_lines.store(100, Ordering::Relaxed);
    device
        .run_sd_file_and_wait(
            "/sd/job.gcode",
            Duration::from_secs(5),
            Some(&progress),
            None,
        )
        .unwrap();
    assert_eq!(fake.lines(), vec!["$SD/Run=/sd/job.gcode".to_owned()]);
    assert_eq!(progress.lines_sent.load(Ordering::Relaxed), 100);
    assert_eq!(device.state(), DeviceState::Ready);
    assert!(!device.sd_job_active, "结束后放开顺序信道");
}

#[test]
fn run_sd_file_reports_firmware_refusal_and_honours_cancel() {
    // 文件不存在等：固件立即回 error（不是推迟应答），按失败报告
    let fake =
        FakeGrbl::new(["[MSG:ERR: /sd/missing.gcode not found]", "error:60"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    device.state = DeviceState::Ready;
    let error = device
        .run_sd_file_and_wait("/sd/missing.gcode", Duration::from_secs(2), None, None)
        .unwrap_err();
    assert!(error.contains("error:60"), "{error}");

    // 作业进行中取消：走 ! → ? → 0x18 停机序列，不再卡在等推迟的 ok
    let fake = FakeGrbl::new(["<Run|MPos:1,1,0|SD:10.00,/sd/job.gcode>".to_owned()]);
    let mut device = DeviceConnection::new(fake.clone());
    device.state = DeviceState::Ready;
    let cancel = CancellationToken::default();
    let progress = Arc::new(JobProgress::default());
    progress.total_lines.store(100, Ordering::Relaxed);
    cancel.cancel();
    let error = device
        .wait_sd_job(Duration::from_secs(2), Some(&progress), Some(&cancel), true)
        .unwrap_err();
    assert!(error.contains("用户取消"), "{error}");
    assert!(fake.realtime().contains(&0x18), "取消要发出软复位");
}

#[test]
fn verify_sd_file_hash_checks_sha256() {
    let sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let fake = FakeGrbl::new([
        format!(
            "{{\"signature\":{{\"algorithm\":\"SHA2-256\",\"value\":\"{sha}\"}},\"path\":\"/sd/test.gcode\"}}"
        ),
        "ok".to_string(),
    ]);
    let mut device = DeviceConnection::new(fake);
    assert!(
        device
            .verify_sd_file_hash("/sd/test.gcode", sha, Duration::from_secs(1))
            .is_ok()
    );

    // 校验失败分支
    let fake_bad = FakeGrbl::new([
            "{\"signature\":{\"algorithm\":\"SHA2-256\",\"value\":\"0000000000000000000000000000000000000000000000000000000000000000\"},\"path\":\"/sd/test.gcode\"}".to_string(),
            "ok".to_string(),
        ]);
    let mut device_bad = DeviceConnection::new(fake_bad);
    let err = device_bad
        .verify_sd_file_hash("/sd/test.gcode", sha, Duration::from_secs(1))
        .unwrap_err();
    assert!(err.contains("物理校验不匹配"), "{err}");
}

#[test]
fn wait_until_idle_treats_home_and_door_as_transient_not_alarm() {
    let fake = FakeGrbl::new(
        [
            "<Home|MPos:0,0,0>",
            "<Door:1|MPos:0,0,0>",
            "<Hold:0>",
            "<Idle|MPos:0,0,0>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake);
    device.state = DeviceState::Ready;
    device
        .wait_until_idle(Duration::from_secs(2), None)
        .unwrap();
    assert_eq!(device.state(), DeviceState::Ready);

    for bad in ["<Sleep>", "<Check>", "<Alarm|MPos:0,0,0>"] {
        let mut device = DeviceConnection::new(FakeGrbl::new([bad.to_string()]));
        device.state = DeviceState::Ready;
        let err = device
            .wait_until_idle(Duration::from_secs(1), None)
            .unwrap_err();
        assert!(err.contains("异常状态"), "{bad}: {err}");
    }
}

#[test]
fn preflight_rejects_blocking_pins_and_explains_which_one() {
    for (pins, expect) in [("E", "急停按钮"), ("XF", "故障输入"), ("D", "安全门")] {
        let status = format!("<Idle|MPos:0,0,0|Pn:{pins}>");
        let fake = FakeGrbl::new([
            "$30=1000".to_string(),
            "$32=1".to_string(),
            "ok".to_string(),
            status,
        ]);
        let mut device = DeviceConnection::new(fake);
        device.handshake().unwrap();
        let err = device
            .verify_job_ready_adaptive(Duration::from_secs(1))
            .unwrap_err();
        assert!(err.contains(expect), "{pins}: {err}");
        assert_eq!(device.state(), DeviceState::Alarm);
    }
    // 限位字母 X/Y 不阻断（只是限位开关状态）
    let fake =
        FakeGrbl::new(["$30=1000", "$32=1", "ok", "<Idle|MPos:0,0,0|Pn:XY>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    device.handshake().unwrap();
    assert!(
        device
            .verify_job_ready_adaptive(Duration::from_secs(1))
            .is_ok()
    );
}

#[test]
fn gcode_preparation_strips_comments_rejects_long_and_non_ascii_lines() {
    assert_eq!(strip_gcode_comments("G1 X1 ; 中文注释"), "G1 X1");
    assert_eq!(strip_gcode_comments("G1 (inline) X2 (more)"), "G1  X2");
    assert_eq!(strip_gcode_comments("; 全注释"), "");
    assert_eq!(strip_gcode_comments("(unclosed G1"), "");

    let text = "; header 中文\nG1 X1 ; trailing\n\nM5\n";
    let p = prepare_gcode_lines(text).unwrap();
    assert_eq!(p.lines, vec!["G1 X1", "M5"]);
    assert_eq!(p.comment_stripped_lines, 2);
    assert_eq!(p.skipped_lines, 2);

    let long = format!("G1 X{}", "1".repeat(260));
    let err = prepare_gcode_lines(&format!("M5\n{long}\n")).unwrap_err();
    assert!(err.contains("第 2 行") && err.contains("254"), "{err}");

    let err = prepare_gcode_lines("G1 X1 Y中\n").unwrap_err();
    assert!(err.contains("非 ASCII"), "{err}");

    // send_line 层同样拒绝，且不写入传输层
    let fake = FakeGrbl::new([]);
    let mut device = DeviceConnection::new(fake.clone());
    device.state = DeviceState::Ready;
    assert!(device.send_line(&long).is_err());
    assert!(fake.lines().is_empty());
}

#[test]
fn sd_wait_rejects_sequential_commands_and_parses_sent_terminator() {
    let responses = [
        "<Run|MPos:10,20,0|SD:25.00,/sd/job.gcode>".into(),
        "<Run|MPos:30,40,0|SD: job.gcode: Sent>".into(),
        "<Idle|MPos:0,0,0>".into(),
    ];
    let fake = FakeGrbl::new(responses);
    let mut device = DeviceConnection::new(fake.clone());
    device.state = DeviceState::Ready;
    let progress = Arc::new(JobProgress::default());
    progress.total_lines.store(200, Ordering::Relaxed);
    device
        .wait_until_sd_completed(Duration::from_secs(2), Some(&progress), None)
        .unwrap();
    assert_eq!(progress.lines_sent.load(Ordering::Relaxed), 200);
    assert!(!device.sd_job_active);

    // SD 作业进行中：顺序指令立即被拒绝，不等心跳超时
    let mut busy = DeviceConnection::new(FakeGrbl::new([]));
    busy.state = DeviceState::Ready;
    busy.sd_job_active = true;
    let started = Instant::now();
    let err = busy.send_line_wait_ack("M5").unwrap_err();
    assert!(err.contains("SD 脱机作业"), "{err}");
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(busy.unlock().unwrap_err().contains("SD 脱机作业"));
}

#[test]
fn firmware_explanation_lines_are_attached_to_following_alarm_or_error() {
    let fake = FakeGrbl::new(["[MSG:INFO: ALARM: Unhomed]", "ALARM:14"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    device.state = DeviceState::Ready;
    let err = device.send_line_wait_ack("G1 X1").unwrap_err();
    assert!(err.starts_with("ALARM:14"), "{err}");
    assert!(err.contains("Unhomed"), "{err}");
    assert!(diagnose_device_message(&err).unwrap().contains("未回零"));

    let fake = FakeGrbl::new(["[MSG:ERR: Read-only setting]", "error:162"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    device.state = DeviceState::Ready;
    let err = device.send_line_wait_ack("$30=1000").unwrap_err();
    assert_eq!(err, "error:162 (Read-only setting)");
    assert!(diagnose_device_message(&err).unwrap().contains("只读设置"));
}

#[test]
fn reset_detection_covers_restart_message_and_ver_banner() {
    let banners = [
        "Grbl 3.9 [FluidNC v3.9.1 (wifi) for help]",
        "[MSG:INFO: Restarting]",
        "[VER:3.9 FluidNC v3.9.1:]",
    ];
    for banner in banners {
        assert!(is_reset_banner(banner), "{banner}");
        let mut device = DeviceConnection::new(FakeGrbl::new([banner.to_string()]));
        device.state = DeviceState::Ready;
        let err = device.send_line_wait_ack("M5").unwrap_err();
        assert!(err.contains("复位"), "{banner}: {err}");
        assert_eq!(device.state(), DeviceState::Lost);
    }
    assert!(!is_reset_banner("[MSG:INFO: Caution: Unlocked]"));
}

#[test]
fn sd_hash_verification_reports_unverified_when_hashfs_missing() {
    let fake = FakeGrbl::new(["ok".to_string()]);
    let mut device = DeviceConnection::new(fake);
    let zeros = "0".repeat(64);
    assert!(
        !device
            .verify_sd_file_hash("/sd/x.nc", &zeros, Duration::from_secs(1))
            .unwrap()
    );
}

/// 2026-11-18：等待完成时控制器一直处于保持（用户暂停走边框去摆木板），时间超过无进展超时也要接着等，
/// 恢复后正常完成；暂停状态跟着控制器报告走（保持 → paused，恢复运行 → 不再 paused）。
#[test]
fn wait_until_idle_keeps_waiting_through_a_long_hold() {
    struct HoldThenIdle {
        started: Instant,
    }
    impl DeviceTransport for HoldThenIdle {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn write_realtime(&self, _command: RealtimeCommand) -> Result<(), String> {
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            std::thread::sleep(Duration::from_millis(10));
            let elapsed = self.started.elapsed();
            Ok(Some(if elapsed < Duration::from_millis(400) {
                "<Hold:0|MPos:1,2,0>".into()
            } else {
                "<Idle|MPos:0,0,0>".into()
            }))
        }
    }
    let progress = Arc::new(crate::JobProgress::default());
    let mut device = DeviceConnection::new(HoldThenIdle {
        started: Instant::now(),
    });
    device.progress_sink = Some(progress.clone());
    device.state = DeviceState::Ready;
    let watcher = {
        let progress = progress.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(350) {
                if progress.paused.load(std::sync::atomic::Ordering::Acquire) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            false
        })
    };
    // 无进展超时只有 100 ms，保持持续 400 ms
    device
        .wait_until_idle(Duration::from_millis(100), None)
        .expect("保持期间不能判超时");
    assert_eq!(device.state(), DeviceState::Ready);
    assert!(watcher.join().unwrap(), "保持期间应显示已暂停");
    assert!(
        !progress.paused.load(std::sync::atomic::Ordering::Acquire),
        "恢复后不再显示暂停"
    );
}

#[test]
fn sd_monitor_distinguishes_normal_cancellation_from_firmware_failure() {
    let cancel = CancellationToken::default();
    cancel.cancel();
    let fake = FakeGrbl::new(["<Hold:0|MPos:0,0,0>".to_owned()]);
    let mut device = DeviceConnection::new(fake.clone());
    assert!(
        !device
            .wait_sd_job_or_cancel(Duration::from_secs(2), None, Some(&cancel), true)
            .unwrap()
    );
    assert_eq!(fake.realtime(), [b'?', b'!', b'?', 0x18]);

    for (reply, state) in [
        ("ALARM:1", DeviceState::Alarm),
        ("Grbl 1.1h ['$' for help]", DeviceState::Lost),
    ] {
        assert_sd_cancel_reports_firmware_error(reply, state);
    }
    let fake = FakeGrbl::new(Vec::<String>::new());
    let mut device = DeviceConnection::new(fake.clone());
    let error = device
        .wait_sd_job_or_cancel(Duration::from_secs(2), None, Some(&cancel), true)
        .unwrap_err();
    assert!(error.contains("连接断开"), "{error}");
    assert_eq!(fake.realtime(), [b'?', b'!', b'?', 0x18]);
}

fn assert_sd_cancel_reports_firmware_error(reply: &str, state: DeviceState) {
    let cancel = CancellationToken::default();
    cancel.cancel();
    let fake = FakeGrbl::new([reply, "<Hold:0|MPos:0,0,0>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.state = DeviceState::Ready;
    device.auto_report_ms = 100;
    let error = device
        .wait_sd_job_or_cancel(Duration::from_secs(2), None, Some(&cancel), true)
        .unwrap_err();
    assert!(error.contains(reply), "{error}");
    assert_eq!(device.state(), state);
    assert_eq!(device.auto_report_ms, 0);
    assert!(device.send_line("M5").is_err());
    assert!(fake.lines().is_empty());
    assert_eq!(fake.realtime(), [b'?', b'!', b'?', 0x18]);
}

#[test]
fn sd_cancel_reports_alarm_status_as_failure() {
    assert_sd_cancel_reports_firmware_error("<Alarm|MPos:0,0,0>", DeviceState::Alarm);
}

#[test]
fn sd_cancel_preserves_deferred_error() {
    assert_sd_cancel_reports_firmware_error("error:60", DeviceState::Lost);
}

#[test]
fn sd_cancel_reports_disconnect_after_status_loses_sd_field() {
    struct CancelOnStatus {
        fake: FakeGrbl,
        cancel: CancellationToken,
    }

    impl DeviceTransport for CancelOnStatus {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.fake.write_line(line)
        }

        fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
            self.fake.write_realtime(command)
        }

        fn read_line(&self) -> Result<Option<String>, String> {
            let line = self.fake.read_line()?;
            if line.as_deref() == Some("<Run|MPos:1,0,0>") {
                // 监控仍在等停稳，但最后一份运行报告已去掉 SD 字段。
                self.cancel.cancel();
            }
            Ok(line)
        }
    }

    let fake =
        FakeGrbl::new(["<Run|SD:10.00,/sd/job.gcode>", "<Run|MPos:1,0,0>"].map(str::to_owned));
    let cancel = CancellationToken::default();
    let mut device = DeviceConnection::new(CancelOnStatus {
        fake: fake.clone(),
        cancel: cancel.clone(),
    });
    device.state = DeviceState::Ready;

    let error = device
        .run_sd_file_or_cancel("/sd/job.gcode", Duration::from_secs(2), None, Some(&cancel))
        .unwrap_err();
    assert!(error.contains("连接断开"), "{error}");
    assert_eq!(device.state(), DeviceState::Disconnected);
    assert_eq!(fake.lines(), ["$SD/Run=/sd/job.gcode"]);
    assert_eq!(fake.realtime(), [b'?', b'!', b'?', 0x18]);
}

#[test]
fn verify_sd_file_hash_rejects_unterminated_reset_and_eof() {
    let sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let hash_line = format!(
        "{{\"signature\":{{\"algorithm\":\"SHA2-256\",\"value\":\"{sha}\"}},\"path\":\"/sd/test.gcode\"}}"
    );
    // 1. 收到匹配哈希但紧接着连接断开（无终止 ok）→ 必须失败并标记 Disconnected
    let mut eof_dev = DeviceConnection::new(FakeGrbl::new([hash_line.clone()]));
    let eof_err = eof_dev
        .verify_sd_file_hash("/sd/test.gcode", sha, Duration::from_millis(80))
        .unwrap_err();
    assert!(eof_err.contains("断开"), "{eof_err}");
    assert_eq!(eof_dev.state(), DeviceState::Disconnected);

    // 2. 收到匹配哈希后遇复位横幅 → 必须失败并标记 Lost
    let mut reset_dev = DeviceConnection::new(FakeGrbl::new([
        hash_line.clone(),
        "Grbl 3.7 ['$' for help]".into(),
    ]));
    let reset_err = reset_dev
        .verify_sd_file_hash("/sd/test.gcode", sha, Duration::from_millis(80))
        .unwrap_err();
    assert!(reset_err.contains("复位"), "{reset_err}");
    assert_eq!(reset_dev.state(), DeviceState::Lost);

    // 3. 收到匹配哈希但尾随 ok 超时未到 → 必须失败并标记顺序信道脏
    struct HashWithoutOk {
        line: String,
        sent: std::sync::atomic::AtomicBool,
    }
    impl DeviceTransport for HashWithoutOk {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn write_realtime(&self, _command: RealtimeCommand) -> Result<(), String> {
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            if !self.sent.swap(true, Ordering::SeqCst) {
                return Ok(Some(self.line.clone()));
            }
            std::thread::sleep(Duration::from_millis(15));
            Err("timed out".into())
        }
    }
    let mut stalled = DeviceConnection::new(HashWithoutOk {
        line: hash_line,
        sent: std::sync::atomic::AtomicBool::new(false),
    });
    let timeout_err = stalled
        .verify_sd_file_hash("/sd/test.gcode", sha, Duration::from_millis(50))
        .unwrap_err();
    assert!(timeout_err.contains("未收到终止 ok"), "{timeout_err}");
    assert_eq!(stalled.state(), DeviceState::Lost);
    let dirty_err = stalled.send_line("G1 X1").unwrap_err();
    assert!(dirty_err.contains("未排空"), "{dirty_err}");
}

#[test]
fn cd_reset_blocks_subsequent_idle_preflight() {
    let fake = FakeGrbl::new(
        [
            "$30=1000",
            "$32=1",
            "ok",
            "axes:",
            "[MSG:INFO: Restarting]",
            "<Idle|MPos:0,0,0>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake);
    device.handshake().unwrap();
    let cd_err = device.load_features(Duration::from_secs(1)).unwrap_err();
    assert!(cd_err.contains("复位"), "{cd_err}");
    assert_eq!(device.state(), DeviceState::Lost);
    let err = device
        .verify_job_ready_adaptive(Duration::from_secs(1))
        .unwrap_err();
    assert!(err.contains("复位") && err.contains("连接断开"), "{err}");
}

#[test]
fn cd_alarm_and_error3_are_distinguished_in_load_features_and_probe() {
    // 1. 旧 GRBL 返回 error:3 视为不支持 $CD，静默返回 Ok(false)
    let grbl = FakeGrbl::new(["$30=1000", "$32=1", "ok", "error:3"].map(str::to_owned));
    let mut dev = DeviceConnection::new(grbl);
    dev.handshake().unwrap();
    assert_eq!(dev.load_features(Duration::from_secs(1)), Ok(false));
    assert_eq!(dev.state(), DeviceState::Ready);

    // 2. $CD 期间触发 ALARM 必须向上报错，且 probe_opened_device 不得伪报激光模式就绪
    let spec = crate::ConnectionSpec::Tcp {
        host: "127.0.0.1".into(),
        port: 23,
    };
    let alarming = FakeGrbl::new(["$30=1000", "$32=1", "ok", "ALARM:1"].map(str::to_owned));
    let report = crate::probe_opened_device(&spec, Box::new(alarming), Duration::from_secs(1));
    assert!(!report.laser_mode_ok, "{}", report.summary);
    assert!(report.summary.contains("报警"), "{}", report.summary);
}

#[test]
fn stream_cancel_reports_eof_as_disconnect_while_sending_estop() {
    let fake = FakeGrbl::new(Vec::<String>::new());
    let mut device = DeviceConnection::new(fake.clone());
    device.state = DeviceState::Ready;
    let err = device.cancel().unwrap_err();
    assert!(err.contains("连接断开"), "{err}");
    assert_eq!(device.state(), DeviceState::Disconnected);
    assert_eq!(fake.realtime(), [b'!', b'?', 0x18]);
}
