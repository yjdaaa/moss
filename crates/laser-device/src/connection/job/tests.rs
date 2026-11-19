use std::sync::Mutex;

use super::*;
use crate::*;

#[test]
fn worker_cancellation_does_not_interleave_realtime_with_gcode() {
    let fake = FakeGrbl::new(["$32=1", "ok", "<Hold:0>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let token = CancellationToken::default();
    let mut worker = JobWorker::new(device, token.clone());
    token.cancel();
    assert_eq!(worker.send_lines(vec!["G1 X1".to_string()]).unwrap(), 0);
    assert_eq!(fake.lines(), vec!["$$".to_string()]);
    assert_eq!(fake.realtime(), vec![b'!', b'?', 0x18]);
}

#[test]
fn worker_stops_on_error_ack() {
    let fake = FakeGrbl::new(["$32=1", "ok", "ok", "error:2"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    device.handshake().unwrap();
    let mut worker = JobWorker::new(device, CancellationToken::default());
    let result = worker.send_lines(vec!["G1 X1".to_string(), "G1 X2".to_string()]);
    assert!(result.is_err());
}

#[test]
fn fake_device_100ms_backpressure_emergency_stop_test() {
    // Verify §19 Stage 4 requirement: 10,000 lines backlog, emergency stop reaches write end < 100ms
    let responses = (0..10_000).map(|_| "ok".to_string());
    let fake = FakeGrbl::new(
        ["$32=1", "ok"]
            .map(str::to_owned)
            .into_iter()
            .chain(responses),
    );
    // Add artificial backpressure so 10,000 lines take appreciable wall-clock time
    fake.set_backpressure(Duration::from_micros(100));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();

    let token = CancellationToken::default();
    let progress = Arc::new(JobProgress::default());
    let mut worker = JobWorker::with_progress(device, token.clone(), progress.clone());

    let lines: Vec<String> = (0..10_000).map(|i| format!("G1 X{i}")).collect();

    // Spawn worker sending 10,000 lines in background
    let worker_handle = std::thread::spawn(move || worker.send_lines(lines));

    // Sleep briefly to let worker start sending
    std::thread::sleep(Duration::from_millis(15));

    // Fire emergency stop via separate realtime handle
    let abort_start = Instant::now();
    fake.write_realtime(RealtimeCommand::EmergencyStop).unwrap();
    token.cancel();

    let abort_events = fake.realtime_events();
    let first_stop = abort_events.iter().find(|(cmd, _)| *cmd == 0x18).unwrap();
    let duration = first_stop.1.duration_since(abort_start);
    assert!(
        duration < Duration::from_millis(100),
        "0x18 must arrive within 100ms"
    );

    let _ = worker_handle.join();
    assert!(progress.lines_sent.load(Ordering::Relaxed) < 10_000);
}

#[test]
fn completion_requires_fence_ack_then_idle_and_does_not_count_fence() {
    let fake = FakeGrbl::new(
        [
            "$32=1",
            "ok",
            "ok",
            "<Run|MPos:1,0,0>",
            "ok",
            "<Idle|MPos:1,0,0>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let mut worker = JobWorker::new(device, CancellationToken::default());
    assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
    assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 1);
    assert!(worker.progress.awaiting_completion.load(Ordering::Acquire));
    assert_eq!(fake.lines(), ["$$", "M5", "G4 P0"]);
    assert_eq!(fake.realtime(), [b'?']);
}

#[test]
fn completion_waits_for_run_to_transition_to_idle() {
    let fake = FakeGrbl::new(
        [
            "$32=1",
            "ok", // handshake
            "ok", // M5
            "ok", // G4 P0
            "<Run|MPos:1,0,0>",
            "<Run|MPos:2,0,0>",
            "<Idle|MPos:2,0,0>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let mut worker = JobWorker::new(device, CancellationToken::default());
    assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
    assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 1);
    assert!(worker.progress.awaiting_completion.load(Ordering::Acquire));
}

#[test]
fn completed_file_ack_without_execution_confirmation_is_not_success() {
    for tail in [
        vec![],
        vec!["error:2"],
        vec!["ok"],
        vec!["ok", "<Run|MPos:1,0,0>"],
        vec!["Grbl 1.1h"],
    ] {
        let fake = FakeGrbl::new(
            ["$32=1", "ok", "ok"]
                .into_iter()
                .chain(tail)
                .map(str::to_owned),
        );
        let mut device = DeviceConnection::new(fake);
        device.handshake().unwrap();
        let mut worker = JobWorker::new(device, CancellationToken::default());
        assert!(worker.send_lines(["M5".into()]).is_err());
        assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn cancellation_interrupts_ack_wait_with_continuous_status_output() {
    struct CancelOnRead {
        fake: FakeGrbl,
        cancel: CancellationToken,
    }
    impl DeviceTransport for CancelOnRead {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.fake.write_line(line)
        }
        fn write_realtime(&self, cmd: RealtimeCommand) -> Result<(), String> {
            self.fake.write_realtime(cmd)
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            self.cancel.cancel();
            Ok(Some("<Hold:0>".into()))
        }
    }
    let cancel = CancellationToken::default();
    let fake = FakeGrbl::new([]);
    let mut device = DeviceConnection::new(CancelOnRead {
        fake: fake.clone(),
        cancel: cancel.clone(),
    });
    device.state = DeviceState::Ready;
    let mut worker = JobWorker::new(device, cancel);
    // 在字符计数流控下，M5 (3B) 与 G1 X1 (6B) 均在 127 字节 RX 缓冲内连续下发；
    // 随后在等待 ACK 时触发取消，此时 0 行被确认 (ACK)，立即触发 ! -> ? -> 0x18 急停序列
    assert_eq!(worker.send_lines(["M5".into(), "G1 X1".into()]).unwrap(), 0);
    assert_eq!(fake.lines(), ["M5", "G1 X1"]);
    assert_eq!(fake.realtime(), [b'!', b'?', 0x18]);
}

#[test]
fn character_counting_streaming_manages_buffer_and_bursts_lines() {
    // 4 行，每行 5 字符 + '\n' = 6 字节。
    // 设置 buffer_size = 15 字节：
    // 前 2 行 (6 + 6 = 12 字节 <= 15) 在无任何 ACK 时立即连续下发；
    // 第 3 行 (12 + 6 = 18 > 15) 必须等待第 1 行的 ok 被消费后才下发。
    let responses = [
        "$32=1", "ok",     // handshake
        "ok",     // ack for line 1 -> triggers line 3 to be sent
        "ok",     // ack for line 2 -> triggers line 4 to be sent
        "ok",     // ack for line 3
        "ok",     // ack for line 4
        "ok",     // ack for G4 P0
        "<Idle>", // wait_until_idle
    ]
    .map(str::to_owned);
    let fake = FakeGrbl::new(responses);
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let mut worker = JobWorker::new(device, CancellationToken::default());
    let lines = vec![
        "G1 X1".to_string(),
        "G1 X2".to_string(),
        "G1 X3".to_string(),
        "G1 X4".to_string(),
    ];
    let sent = worker.stream_lines_with_buffer_size(lines, 15).unwrap();
    assert_eq!(sent, 4);
    assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 4);
    assert_eq!(
        fake.lines(),
        vec!["$$", "G1 X1", "G1 X2", "G1 X3", "G1 X4", "G4 P0"]
    );
}

#[test]
fn rescale_gcode_line_s_scales_properly() {
    let factor = 255.0 / 1000.0;
    assert_eq!(rescale_gcode_line_s("M4 S1000", factor), "M4 S255");
    assert_eq!(
        rescale_gcode_line_s("G1 X10 Y20 S500 F3000", factor),
        "G1 X10 Y20 S128 F3000"
    );
    assert_eq!(rescale_gcode_line_s("M5 S0", factor), "M5 S0");
    assert_eq!(rescale_gcode_line_s("M4 S1000", 1.0), "M4 S1000");
}

#[test]
fn rescale_gcode_line_s_handles_compact_words_and_keeps_system_commands() {
    let factor = 255.0 / 1000.0;
    // 紧凑写法（含剥离括号注释后拼接出的 "X10S500"）同样必须换算，否则按原值满功率出光
    assert_eq!(rescale_gcode_line_s("G1X10S500", factor), "G1X10S128");
    assert_eq!(rescale_gcode_line_s("M4S1000", factor), "M4S255");
    assert_eq!(
        rescale_gcode_line_s("G1 X10.5S1000F600", factor),
        "G1 X10.5S255F600"
    );
    // `$` 系统指令（如 $SD/Run=）不是 G-code 字，不得改写
    assert_eq!(
        rescale_gcode_line_s("$SD/Run=/sd/S500.gcode", factor),
        "$SD/Run=/sd/S500.gcode"
    );
}

#[test]
fn stream_lines_dynamically_adapts_to_reported_rx_buffer() {
    let responses = [
        "$32=1",
        "ok",
        "<Run|Bf:35,1024>", // 动态报告 1024 字节 RX 缓冲
        "ok",
        "ok",
        "ok",
        "ok",
        "<Idle>",
    ]
    .map(str::to_owned);
    let fake = FakeGrbl::new(responses);
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let mut worker = JobWorker::new(device, CancellationToken::default());
    let lines = vec![
        "G1 X1".to_string(),
        "G1 X2".to_string(),
        "G1 X3".to_string(),
    ];
    let sent = worker.send_lines(lines).unwrap();
    assert_eq!(sent, 3);
    assert_eq!(fake.lines(), vec!["$$", "G1 X1", "G1 X2", "G1 X3", "G4 P0"]);
}

#[test]
fn rx_window_is_capped_at_256_when_firmware_does_not_report_buffer() {
    // 未确认设备身份：无论传输层建议多大，初始窗口都不超过经典 GRBL 的 127 字节
    let fake = FakeGrbl::new(["$10=1", "$32=1", "ok"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake);
    device.handshake().unwrap();
    assert_eq!(device.reports_buffer_state(), Some(false));
    assert_eq!(device.effective_rx_window(1024), GRBL_CLASSIC_RX_WINDOW);
    assert_eq!(device.effective_rx_window(255), GRBL_CLASSIC_RX_WINDOW);
    // 已确认 FluidNC 但不报 Bf:：网络窗口 256，串口仍是 127
    device.features = Some(MachineFeatures::default());
    assert_eq!(device.effective_rx_window(1024), 256);
    assert_eq!(
        device.effective_rx_window(GRBL_CLASSIC_RX_WINDOW),
        GRBL_CLASSIC_RX_WINDOW
    );

    // 报 Bf: 但尚未观测：仍按保守初始值；预检 ? 观测到空闲 Bf:15,1200 后放大到硬上限（1016）
    let fake = FakeGrbl::new(
        [
            "$10=3",
            "$30=1000",
            "$32=1",
            "ok",
            "<Idle|MPos:0,0,0|Bf:15,1200>",
        ]
        .map(str::to_owned),
    );
    let mut device = DeviceConnection::new(fake);
    device.handshake().unwrap();
    assert_eq!(device.reports_buffer_state(), Some(true));
    assert_eq!(device.effective_rx_window(1024), GRBL_CLASSIC_RX_WINDOW);
    device.features = Some(MachineFeatures::default());
    assert_eq!(device.effective_rx_window(1024), 256);
    device
        .verify_job_ready_adaptive(Duration::from_secs(1))
        .unwrap();
    assert_eq!(device.rx_capacity_observed, Some(1200));
    assert_eq!(device.effective_rx_window(1024), MAX_RX_WINDOW);

    // $10=1 时即使 Bf: 报告 1200，窗口也不得扩到 256 以上；3 行仍应正常完成
    let responses = [
        "$10=1",
        "$32=1",
        "ok",
        "<Run|Bf:15,1200>",
        "ok",
        "ok",
        "ok",
        "ok",
        "<Idle>",
    ]
    .map(str::to_owned);
    let fake = FakeGrbl::new(responses);
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let mut worker = JobWorker::new(device, CancellationToken::default());
    assert_eq!(
        worker
            .send_lines(["G1 X1".into(), "G1 X2".into(), "G1 X3".into()])
            .unwrap(),
        3
    );
}

#[test]
fn auto_report_is_enabled_only_for_fluidnc_and_its_ack_is_consumed_before_gcode() {
    // 旧 GRBL（无 features）：不发 $RI
    let fake = FakeGrbl::new(["$32=1", "ok", "ok", "ok", "<Idle>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    let mut worker = JobWorker::new(device, CancellationToken::default());
    assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
    assert_eq!(fake.lines(), ["$$", "M5", "G4 P0"]);

    // FluidNC：$RI=200 的 ok 先被消费，行 ack 计数不受影响；结束后 $RI=0
    let fake = FakeGrbl::new(["$32=1", "ok", "ok", "ok", "ok", "<Idle>", "ok"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.features = Some(MachineFeatures::default());
    let mut worker = JobWorker::new(device, CancellationToken::default());
    assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
    assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 1);
    assert_eq!(worker.progress.status_report().as_deref(), Some("<Idle>"));
    assert_eq!(fake.lines(), ["$$", "$RI=200", "M5", "G4 P0", "$RI=0"]);

    // 固件不认识 $RI（error:3）：静默关闭，不影响后续
    let fake = FakeGrbl::new(["$32=1", "ok", "error:3", "ok", "ok", "<Idle>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.features = Some(MachineFeatures::default());
    let mut worker = JobWorker::new(device, CancellationToken::default());
    assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
    assert_eq!(worker.device.auto_report_ms, 0);
}

// ---- 2026-11-05：字符计数窗口不得超过设备 RX 容量 ----

#[derive(Default)]
struct SimRxState {
    pending: VecDeque<usize>,
    written: Vec<String>,
    /// 每次写入后设备 RX 缓冲中的字节数
    occupancy: Vec<usize>,
    overflow: bool,
    status_requested: bool,
    reads: usize,
}

/// RX 缓冲模拟器：最坏情况下设备在回 ok 前不取走任何字节，未确认字节全部占着 RX 缓冲；
/// 超过 `usable` 即记为溢出（真实串口会丢字节、破坏 G-code）。每读一次处理一行回 `ok`，
/// 每 `report_every` 次读插入一条 `Bf:` 报告（模拟 `$RI` 自动上报），收到 `?` 时回当前状态。
struct SimRx {
    usable: usize,
    idle_reported_free: usize,
    transport_default: usize,
    report_every: usize,
    state: Mutex<SimRxState>,
}

impl SimRx {
    fn new(
        usable: usize,
        idle_reported_free: usize,
        transport_default: usize,
        report_every: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            usable,
            idle_reported_free,
            transport_default,
            report_every,
            state: Mutex::default(),
        })
    }
    fn peak(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .occupancy
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
    }
    fn peak_after_writes(&self, skip: usize) -> usize {
        self.state
            .lock()
            .unwrap()
            .occupancy
            .iter()
            .skip(skip)
            .copied()
            .max()
            .unwrap_or(0)
    }
}

impl DeviceTransport for Arc<SimRx> {
    fn write_line(&self, line: &str) -> Result<(), String> {
        let mut st = self.state.lock().unwrap();
        st.pending.push_back(line.len() + 1);
        st.written.push(line.to_string());
        let used: usize = st.pending.iter().sum();
        st.occupancy.push(used);
        if used > self.usable {
            st.overflow = true;
        }
        Ok(())
    }
    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        if command == RealtimeCommand::StatusQuery {
            self.state.lock().unwrap().status_requested = true;
        }
        Ok(())
    }
    fn read_line(&self) -> Result<Option<String>, String> {
        let mut st = self.state.lock().unwrap();
        st.reads += 1;
        let periodic = !st.pending.is_empty() && st.reads.is_multiple_of(self.report_every);
        if st.status_requested || periodic {
            st.status_requested = false;
            let used: usize = st.pending.iter().sum();
            let free = self.idle_reported_free.saturating_sub(used);
            let state = if st.pending.is_empty() { "Idle" } else { "Run" };
            return Ok(Some(format!(
                "<{state}|MPos:0.000,0.000,0.000|Bf:15,{free}>"
            )));
        }
        if st.pending.pop_front().is_some() {
            return Ok(Some("ok".into()));
        }
        drop(st);
        std::thread::sleep(Duration::from_millis(2));
        Err("timed out".into())
    }
    fn default_rx_buffer_size(&self) -> usize {
        self.transport_default
    }
}

fn sim_device(sim: &Arc<SimRx>, fluidnc: bool) -> DeviceConnection<Arc<SimRx>> {
    let mut device = DeviceConnection::new(sim.clone());
    device.state = DeviceState::Ready;
    device.report_status_mask = Some(3);
    device.max_spindle_speed = Some(1000.0);
    if fluidnc {
        device.features = Some(MachineFeatures::default());
    }
    device
}

fn sim_lines(count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("G1 X{:.3} Y{:.3} S500", i as f64 * 0.1, i as f64 * 0.2))
        .collect()
}

#[test]
fn rx_window_never_exceeds_classic_grbl_rx_buffer() {
    // 经典 GRBL 1.1：128 字节环形缓冲只能存 127；空闲时 Bf 报 128（部分变体报 127）。
    // 传输层建议 255（旧串口默认值）也不能被采信。
    for idle_free in [127usize, 128] {
        for preflight in [false, true] {
            let sim = SimRx::new(GRBL_CLASSIC_RX_WINDOW, idle_free, 255, 3);
            let mut device = sim_device(&sim, false);
            if preflight {
                device
                    .verify_job_ready_adaptive(Duration::from_secs(1))
                    .unwrap();
                assert_eq!(device.rx_capacity_observed, Some(idle_free));
                assert!(device.effective_rx_window(255) <= GRBL_CLASSIC_RX_WINDOW);
            }
            let mut worker = JobWorker::new(device, CancellationToken::default());
            assert_eq!(
                worker.send_lines(sim_lines(200)).unwrap(),
                200,
                "Bf:15,{idle_free} preflight={preflight}"
            );
            let peak = sim.peak();
            assert!(
                !sim.state.lock().unwrap().overflow,
                "RX 溢出：峰值 {peak} 字节 (Bf:15,{idle_free}, preflight={preflight})"
            );
            assert!(
                peak <= GRBL_CLASSIC_RX_WINDOW && peak < idle_free,
                "峰值 {peak} 超过设备容量 {idle_free}"
            );
            // 窗口仍被有效利用，不是退化成逐行往返
            assert!(peak >= 100, "峰值 {peak} 过低");
            assert_eq!(sim.state.lock().unwrap().written.len(), 201); // 200 行 + G4 P0
        }
    }
}

#[test]
fn fluidnc_large_reported_rx_capacity_is_used_and_not_shrunk_by_mid_stream_free() {
    // FluidNC Telnet 空闲报 Bf:15,1200：窗口应放大到硬上限（1016，低于固件 1020 字节行队列），
    // 且发送中 Bf 剩余值变小不能把窗口压回 255。
    for preflight in [true, false] {
        let sim = SimRx::new(1200, 1200, MAX_RX_WINDOW, 4);
        let mut device = sim_device(&sim, true);
        if preflight {
            device
                .verify_job_ready_adaptive(Duration::from_secs(1))
                .unwrap();
            assert_eq!(device.effective_rx_window(MAX_RX_WINDOW), MAX_RX_WINDOW);
        } else {
            assert_eq!(
                device.effective_rx_window(MAX_RX_WINDOW),
                FLUIDNC_CHANNEL_RX_WINDOW
            );
        }
        let mut worker = JobWorker::new(device, CancellationToken::default());
        assert_eq!(worker.send_lines(sim_lines(400)).unwrap(), 400);
        assert!(!sim.state.lock().unwrap().overflow);
        assert!(
            sim.peak() <= MAX_RX_WINDOW,
            "峰值 {} 超过窗口上限",
            sim.peak()
        );
        assert!(
            sim.peak() < FLUIDNC_LINE_QUEUE_LIMIT,
            "峰值 {} 达到 FluidNC 行队列上限，固件会整行丢弃",
            sim.peak()
        );
        // 后半程（已收到多次发送中 Bf 报告之后）仍保持大窗口
        let late_peak = sim.peak_after_writes(200);
        assert!(
            late_peak > 900,
            "后半程峰值 {late_peak}，大容量未被利用 (preflight={preflight})"
        );
        let written = sim.state.lock().unwrap().written.clone();
        assert_eq!(written.first().map(String::as_str), Some("$RI=200"));
        assert_eq!(written.last().map(String::as_str), Some("$RI=0"));
    }
}

// ---- 2026-11-05：$RI 迟到的 ok 不得被当成 G-code 行的 ack ----

/// 响应队列中的 `"\u{0}wait:<ms>"` 标记模拟线路沉默；队列耗尽后持续读超时而不是 EOF。
struct SlowLink {
    fake: FakeGrbl,
    gate: Mutex<Option<Instant>>,
}
impl DeviceTransport for SlowLink {
    fn write_line(&self, line: &str) -> Result<(), String> {
        self.fake.write_line(line)
    }
    fn write_realtime(&self, c: RealtimeCommand) -> Result<(), String> {
        self.fake.write_realtime(c)
    }
    fn read_line(&self) -> Result<Option<String>, String> {
        let mut queue = self.fake.responses.lock().unwrap();
        match queue.front().cloned() {
            Some(front) if front.starts_with("\u{0}wait:") => {
                let ms: u64 = front["\u{0}wait:".len()..].parse().unwrap();
                let mut gate = self.gate.lock().unwrap();
                let until = *gate.get_or_insert_with(|| Instant::now() + Duration::from_millis(ms));
                if Instant::now() >= until {
                    queue.pop_front();
                    *gate = None;
                }
            }
            Some(_) => return Ok(queue.pop_front()),
            None => {}
        }
        drop(queue);
        std::thread::sleep(Duration::from_millis(20));
        Err("timed out".into())
    }
}

fn slow_fluidnc(responses: &[&str]) -> (FakeGrbl, DeviceConnection<SlowLink>) {
    let fake = FakeGrbl::new(responses.iter().map(|s| s.to_string()));
    let mut device = DeviceConnection::new(SlowLink {
        fake: fake.clone(),
        gate: Mutex::new(None),
    });
    device.handshake().unwrap();
    device.features = Some(MachineFeatures::default());
    (fake, device)
}

#[test]
fn late_auto_report_ack_is_resynchronised_before_streaming() {
    let (fake, device) = slow_fluidnc(&[
        "$32=1",
        "ok",             // 握手
        "\u{0}wait:2300", // $RI 应答迟到，超过 2s 常规等待
        "ok",             // $RI 的 ok（迟到）
        "ok",             // 同步 G4 P0 的 ok
        "ok",             // M5
        "ok",             // 完成栅栏 G4 P0
        "<Idle>",
        "ok", // $RI=0
    ]);
    let mut worker = JobWorker::new(device, CancellationToken::default());
    assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
    assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 1);
    // 迟到的 ok 被同步消费掉，M5 只在同步之后发出，完成后关闭自动上报
    assert_eq!(
        fake.lines(),
        ["$$", "$RI=200", "G4 P0", "M5", "G4 P0", "$RI=0"]
    );
}

#[test]
fn unconfirmed_auto_report_ack_aborts_before_streaming() {
    // 超时后只收到一个 ok：无法判断它属于 $RI 还是 G4 P0，必须拒绝开始发送
    let (fake, device) = slow_fluidnc(&["$32=1", "ok", "\u{0}wait:2300", "ok"]);
    let mut worker = JobWorker::new(device, CancellationToken::default());
    let err = worker.send_lines(["M5".into()]).unwrap_err();
    assert!(err.contains("未开始发送"), "{err}");
    assert_eq!(worker.device.state(), DeviceState::Lost);
    assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 0);
    let lines = fake.lines();
    assert!(!lines.iter().any(|l| l == "M5"), "{lines:?}");
    assert_eq!(lines, ["$$", "$RI=200", "G4 P0", "$RI=0"]);
}

#[derive(Default)]
struct ControlledWriteGate {
    armed: bool,
    writes_after_arm: usize,
    first_write_in_flight: bool,
    release_first_write: bool,
}

/// 受控 FakeGrbl 传输层：卡住 `arm()` 后的第一笔顺序写，同时保持实时通道无锁独立。
struct ControlledFakeTransport {
    fake: FakeGrbl,
    sequential_lock: Mutex<()>,
    gate: Mutex<ControlledWriteGate>,
    cv: std::sync::Condvar,
}

impl ControlledFakeTransport {
    fn new(fake: FakeGrbl) -> Arc<Self> {
        Arc::new(Self {
            fake,
            sequential_lock: Mutex::new(()),
            gate: Mutex::new(ControlledWriteGate::default()),
            cv: std::sync::Condvar::new(),
        })
    }

    fn arm(&self) {
        self.gate.lock().unwrap().armed = true;
    }

    fn wait_until_first_write_in_flight(&self) {
        let mut st = self.gate.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !st.first_write_in_flight {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for first sequential write to enter in-flight state"
            );
            let (next, _) = self.cv.wait_timeout(st, remaining).unwrap();
            st = next;
        }
    }

    fn release_first_write(&self) {
        let mut st = self.gate.lock().unwrap();
        st.release_first_write = true;
        self.cv.notify_all();
    }

    fn writes_after_arm(&self) -> usize {
        self.gate.lock().unwrap().writes_after_arm
    }
}

impl DeviceTransport for Arc<ControlledFakeTransport> {
    fn write_line(&self, line: &str) -> Result<(), String> {
        let _seq_guard = self.sequential_lock.lock().unwrap();
        let is_first_armed_write = {
            let mut st = self.gate.lock().unwrap();
            if st.armed {
                st.writes_after_arm += 1;
                if st.writes_after_arm == 1 {
                    st.first_write_in_flight = true;
                    self.cv.notify_all();
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if is_first_armed_write {
            let mut st = self.gate.lock().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !st.release_first_write {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "timed out waiting to release first sequential write"
                );
                let (next, _) = self.cv.wait_timeout(st, remaining).unwrap();
                st = next;
            }
        }
        self.fake.write_line(line)
    }

    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        // 实时通道独立于串行写锁，在途写阻塞期间仍可立即送达
        self.fake.write_realtime(command)
    }

    fn read_line(&self) -> Result<Option<String>, String> {
        self.fake.read_line()
    }
}

#[test]
fn cancellation_during_in_flight_sequential_write_forbids_next_write() {
    let fake = FakeGrbl::new(["$32=1", "ok", "<Hold:0>"].map(str::to_owned));
    let transport = ControlledFakeTransport::new(fake.clone());
    let mut device = DeviceConnection::new(transport.clone());
    device.handshake().unwrap();
    transport.arm();

    let token = CancellationToken::default();
    let mut worker = JobWorker::new(device, token.clone());
    let worker_handle = std::thread::spawn(move || {
        let res = worker.send_lines(["G1 X1".into(), "G1 X2".into(), "G1 X3".into()]);
        (res, worker.device.state())
    });

    // 1. 等到第一笔正常顺序写已在途（持有顺序锁且尚未完成）
    transport.wait_until_first_write_in_flight();

    // 2. 在途期间发送实时急停并置取消；实时通道不得等待顺序写锁
    transport
        .write_realtime(RealtimeCommand::EmergencyStop)
        .unwrap();
    token.cancel();

    // 3. 放行第一笔在途写使其最终成功返回 Ok(())
    transport.release_first_write();

    let (res, final_state) = worker_handle.join().unwrap();
    assert_eq!(res.unwrap(), 0);
    assert_eq!(final_state, DeviceState::Alarm);
    assert_eq!(
        transport.writes_after_arm(),
        1,
        "第一笔写期间已取消，此后禁止开始下一笔正常顺序写"
    );
    assert_eq!(fake.lines(), vec!["$$".to_string(), "G1 X1".to_string()]);
    assert_eq!(fake.realtime(), vec![0x18, b'!', b'?', 0x18]);
}

#[test]
fn fluidnc_cancellation_before_start_or_in_flight_ri_skips_resync_and_disable() {
    // 1. FluidNC 在调用前已取消：不发 $RI=200，不补 G4 P0，不写 $RI=0
    let fake = FakeGrbl::new(["$32=1", "ok", "<Hold:0>"].map(str::to_owned));
    let mut device = DeviceConnection::new(fake.clone());
    device.handshake().unwrap();
    device.features = Some(MachineFeatures::default());
    let token = CancellationToken::default();
    let mut worker = JobWorker::new(device, token.clone());
    token.cancel();
    assert_eq!(worker.send_lines(["G1 X1".into()]).unwrap(), 0);
    assert_eq!(worker.device.state(), DeviceState::Alarm);
    assert_eq!(fake.lines(), vec!["$$".to_string()]);
    assert_eq!(fake.realtime(), vec![b'!', b'?', 0x18]);

    // 2. FluidNC 在 $RI=200 顺序写在途期间被取消且写最终成功：及时走实时收尾，禁止 G4 P0 / G1 / $RI=0
    let fake = FakeGrbl::new(["$32=1", "ok", "<Hold:0>"].map(str::to_owned));
    let transport = ControlledFakeTransport::new(fake.clone());
    let mut device = DeviceConnection::new(transport.clone());
    device.handshake().unwrap();
    device.features = Some(MachineFeatures::default());
    transport.arm();

    let token = CancellationToken::default();
    let mut worker = JobWorker::new(device, token.clone());
    let worker_handle = std::thread::spawn(move || {
        let res = worker.send_lines(["G1 X1".into(), "G1 X2".into()]);
        (res, worker.device.state(), worker.device.auto_report_ms)
    });

    transport.wait_until_first_write_in_flight();
    transport
        .write_realtime(RealtimeCommand::EmergencyStop)
        .unwrap();
    token.cancel();
    transport.release_first_write();

    let (res, final_state, auto_report_ms) = worker_handle.join().unwrap();
    assert_eq!(res.unwrap(), 0);
    assert_eq!(final_state, DeviceState::Alarm);
    assert_eq!(auto_report_ms, 0);
    assert_eq!(transport.writes_after_arm(), 1);
    assert_eq!(fake.lines(), vec!["$$".to_string(), "$RI=200".to_string()]);
    assert_eq!(fake.realtime(), vec![0x18, b'!', b'?', 0x18]);
}

#[test]
fn fluidnc_cancellation_while_waiting_ri_ack_or_streaming_skips_g4_and_ri_zero() {
    // 1. 等 $RI=200 应答超时/读等待期间取消：立刻走实时收尾，不得等待超时后补 G4 P0 或写 $RI=0
    struct CancelDuringRiAckWait {
        fake: FakeGrbl,
        cancel: CancellationToken,
        ri_sent: AtomicBool,
    }
    impl DeviceTransport for CancelDuringRiAckWait {
        fn write_line(&self, line: &str) -> Result<(), String> {
            if line.starts_with("$RI=") {
                self.ri_sent.store(true, Ordering::Release);
            }
            self.fake.write_line(line)
        }
        fn write_realtime(&self, cmd: RealtimeCommand) -> Result<(), String> {
            self.fake.write_realtime(cmd)
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            if self.ri_sent.load(Ordering::Acquire) && !self.cancel.is_cancelled() {
                self.cancel.cancel();
                return Err("timed out".into());
            }
            self.fake.read_line()
        }
    }

    let fake = FakeGrbl::new(["$32=1", "ok", "<Hold:0>"].map(str::to_owned));
    let cancel = CancellationToken::default();
    let mut device = DeviceConnection::new(CancelDuringRiAckWait {
        fake: fake.clone(),
        cancel: cancel.clone(),
        ri_sent: AtomicBool::new(false),
    });
    device.handshake().unwrap();
    device.features = Some(MachineFeatures::default());
    let mut worker = JobWorker::new(device, cancel);
    assert_eq!(worker.send_lines(["G1 X1".into()]).unwrap(), 0);
    assert_eq!(worker.device.state(), DeviceState::Alarm);
    assert_eq!(fake.lines(), vec!["$$".to_string(), "$RI=200".to_string()]);
    assert_eq!(fake.realtime(), vec![b'!', b'?', 0x18]);

    // 2. $RI=200 已成功启用后，在发送流中途取消：走实时收尾后外层也不得再写 $RI=0
    struct CancelAfterRiInStream {
        fake: FakeGrbl,
        cancel: CancellationToken,
    }
    impl DeviceTransport for CancelAfterRiInStream {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.fake.write_line(line)
        }
        fn write_realtime(&self, cmd: RealtimeCommand) -> Result<(), String> {
            self.fake.write_realtime(cmd)
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            let next = self.fake.read_line()?;
            if next.as_deref() == Some("<Run|Bf:15,1024>") {
                self.cancel.cancel();
            }
            Ok(next)
        }
    }

    let fake =
        FakeGrbl::new(["$32=1", "ok", "ok", "<Run|Bf:15,1024>", "<Hold:0>"].map(str::to_owned));
    let cancel = CancellationToken::default();
    let mut device = DeviceConnection::new(CancelAfterRiInStream {
        fake: fake.clone(),
        cancel: cancel.clone(),
    });
    device.handshake().unwrap();
    device.features = Some(MachineFeatures::default());
    let mut worker = JobWorker::new(device, cancel);
    assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 0);
    assert_eq!(worker.device.state(), DeviceState::Alarm);
    assert_eq!(worker.device.auto_report_ms, 0);
    assert_eq!(
        fake.lines(),
        vec!["$$".to_string(), "$RI=200".to_string(), "M5".to_string()]
    );
    assert_eq!(fake.realtime(), vec![b'!', b'?', 0x18]);
}

#[test]
fn inner_window_rechecks_cancel_before_next_send_and_preserves_cancel_error() {
    #[derive(Clone)]
    struct FlipCancelOnFirstSend {
        sent: Arc<Mutex<Vec<String>>>,
        responses: Arc<Mutex<std::collections::VecDeque<String>>>,
        cancel: CancellationToken,
    }
    impl DeviceTransport for FlipCancelOnFirstSend {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.sent.lock().unwrap().push(line.to_owned());
            if line != "$$" {
                self.cancel.cancel();
            }
            Ok(())
        }
        fn write_realtime(&self, _command: RealtimeCommand) -> Result<(), String> {
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }
    let cancel = CancellationToken::default();
    let tr = FlipCancelOnFirstSend {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: Arc::new(Mutex::new(
            ["$32=1", "ok", "<Hold:0>"].map(str::to_owned).into(),
        )),
        cancel: cancel.clone(),
    };
    let mut dev = DeviceConnection::new(tr.clone());
    dev.handshake().unwrap();
    let mut worker = JobWorker::new(dev, cancel);
    // 第 1 行已经在途完成写入后才置 cancel，内层循环写入第 2 行前必须重查 cancel 并立刻停止
    let acked = worker
        .send_lines(["G1 X1".into(), "G1 X2".into(), "G1 X3".into()])
        .unwrap();
    assert_eq!(acked, 0);
    assert_eq!(
        *tr.sent.lock().unwrap(),
        vec!["$$".to_owned(), "G1 X1".to_owned()],
        "置位 cancel 后内层不得再向窗口写入后续行"
    );

    // 取消路径自身报错（write_realtime 失败）必须保留 Err，不得返回 Ok
    struct FailCancelRealtime;
    impl DeviceTransport for FailCancelRealtime {
        fn write_line(&self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn write_realtime(&self, _command: RealtimeCommand) -> Result<(), String> {
            Err("usb disconnected on abort".into())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(Some("ok".into()))
        }
    }
    let mut dev_fail = DeviceConnection::new(FailCancelRealtime);
    dev_fail.state = DeviceState::Ready;
    let tok2 = CancellationToken::default();
    tok2.cancel();
    let mut worker2 = JobWorker::new(dev_fail, tok2);
    let err = worker2.send_lines(["G1 X1".into()]).unwrap_err();
    assert!(err.contains("usb disconnected"), "{err}");
}

#[test]
fn cancel_completion_requires_device_proven_hold_ln_and_blocks_estimates() {
    // 1. 未开启 use_line_numbers 或无 Hold:0|Ln:K 时（即便收到异步 Idle 或 Bf），lines_completed 必须为 0
    for status_line in [
        "<Idle|MPos:0,0,0>",
        "<Hold:0|MPos:0,0,0|Bf:12,120>",
        "<Hold:1|MPos:0,0,0|Ln:2>",
    ] {
        #[derive(Clone)]
        struct CancelMidSend {
            sent_lines: Arc<Mutex<Vec<String>>>,
            responses: Arc<Mutex<std::collections::VecDeque<String>>>,
            cancel: CancellationToken,
            cancel_report: &'static str,
        }
        impl DeviceTransport for CancelMidSend {
            fn write_line(&self, line: &str) -> Result<(), String> {
                let mut sent = self.sent_lines.lock().unwrap();
                sent.push(line.to_owned());
                if sent
                    .iter()
                    .filter(|l| l.starts_with('G') || l.starts_with('N'))
                    .count()
                    == 2
                {
                    self.responses.lock().unwrap().push_back("ok".into());
                } else if line == "$$" {
                    self.responses
                        .lock()
                        .unwrap()
                        .extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                }
                Ok(())
            }
            fn write_realtime(&self, cmd: RealtimeCommand) -> Result<(), String> {
                if cmd == RealtimeCommand::StatusQuery {
                    self.responses
                        .lock()
                        .unwrap()
                        .push_back(self.cancel_report.into());
                }
                Ok(())
            }
            fn read_line(&self) -> Result<Option<String>, String> {
                let line = self.responses.lock().unwrap().pop_front();
                // 仅在 4 行均已发出且 read_line 消费到这条加工 ACK 时再置 cancel（握手仅 $$ 不会触发）
                if line.as_deref() == Some("ok")
                    && self
                        .sent_lines
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|l| l.starts_with('G') || l.starts_with('N'))
                        .count()
                        == 4
                {
                    self.cancel.cancel();
                }
                Ok(line)
            }
        }
        let tok = CancellationToken::default();
        let tr = CancelMidSend {
            sent_lines: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(Default::default())),
            cancel: tok.clone(),
            cancel_report: status_line,
        };
        let mut dev = DeviceConnection::new(tr);
        dev.handshake().unwrap();
        let prog = Arc::new(JobProgress::default());
        let mut worker = JobWorker::with_progress(dev, tok, prog.clone());
        let acked = worker
            .send_lines([
                "G1 X1".into(),
                "G1 X2".into(),
                "G1 X3".into(),
                "G1 X4".into(),
            ])
            .unwrap();
        assert_eq!(acked, 1, "{status_line}");
        assert_eq!(
            prog.lines_completed.load(Ordering::Relaxed),
            0,
            "未启用行号+Hold:0+Ln 时不得凭 Idle/Bf 猜断点: {status_line}"
        );
    }

    // 2. 启用 use_line_numbers 并发送 N1..N4，Hold:0|Ln:3 证实第 1..2 行已完成（第 3 行正受 Hold）
    #[derive(Clone)]
    struct NumberedHoldGrbl {
        sent: Arc<Mutex<Vec<String>>>,
        responses: Arc<Mutex<std::collections::VecDeque<String>>>,
        machining_acks: Arc<std::sync::atomic::AtomicUsize>,
        cancel: CancellationToken,
    }
    impl DeviceTransport for NumberedHoldGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            let mut sent = self.sent.lock().unwrap();
            sent.push(line.to_owned());
            if line == "$$" {
                self.responses
                    .lock()
                    .unwrap()
                    .extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else if line == "$CD" {
                self.responses
                    .lock()
                    .unwrap()
                    .extend(["use_line_numbers: true", "ok"].map(str::to_owned));
            } else if line.starts_with("$RI=") || line.starts_with('N') {
                self.responses.lock().unwrap().push_back("ok".into());
            }
            Ok(())
        }
        fn write_realtime(&self, cmd: RealtimeCommand) -> Result<(), String> {
            if cmd == RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Hold:0|MPos:2,0,0|Ln:3>".into());
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            let line = self.responses.lock().unwrap().pop_front();
            if line.as_deref() == Some("ok")
                && self.sent.lock().unwrap().iter().any(|l| l.starts_with('N'))
                && self.machining_acks.fetch_add(1, Ordering::Relaxed) + 1 == 4
            {
                self.cancel.cancel();
            }
            Ok(line)
        }
    }
    let tok = CancellationToken::default();
    let tr = NumberedHoldGrbl {
        sent: Arc::new(Mutex::new(Vec::new())),
        responses: Arc::new(Mutex::new(Default::default())),
        machining_acks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        cancel: tok.clone(),
    };
    let mut dev = DeviceConnection::new(tr.clone());
    dev.handshake().unwrap();
    assert!(dev.load_features(Duration::from_secs(1)));
    let prog = Arc::new(JobProgress::default());
    let mut worker = JobWorker::with_progress(dev, tok, prog.clone());
    // 这里只传 4 行并在 read_line 消费到第 4 个加工 ACK 时置 cancel（模拟 4 行皆收 ok 后仍在运行/Hold:0|Ln:3）
    let _ = worker
        .send_lines([
            "G1 X1".into(),
            "G1 X2".into(),
            "G1 X3".into(),
            "G1 X4".into(),
        ])
        .unwrap();
    let wire = tr.sent.lock().unwrap().clone();
    assert!(wire.contains(&"N1 G1 X1".to_owned()), "{wire:?}");
    assert!(wire.contains(&"N3 G1 X3".to_owned()), "{wire:?}");
    assert_eq!(prog.lines_completed.load(Ordering::Relaxed), 2);
}
