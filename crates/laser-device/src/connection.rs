mod job;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub use job::{CancellationToken, JobProgress, JobWorker, rescale_gcode_line_s};

use crate::{
    DeviceTransport, FLUIDNC_CHANNEL_RX_WINDOW, GRBL_CLASSIC_RX_WINDOW, MAX_RX_WINDOW,
    MachineActivity, MachineFeatures, MachineLimits, RealtimeCommand, SdProgress, StatusReport,
    extract_device_explanation, is_reset_banner, parse_fluidnc_yaml, parse_grbl_settings_limits,
    parse_status_report, validate_gcode_line,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Disconnected,
    Ready,
    Hold,
    Alarm,
    Lost,
}

/// `$RI` 设置的常规应答等待时间。
const AUTO_REPORT_ACK_TIMEOUT: Duration = Duration::from_secs(2);
/// `$RI` 应答超时后补发 `G4 P0` 同步时，收齐两个终止应答的额外时限。
const AUTO_REPORT_RESYNC_TIMEOUT: Duration = Duration::from_secs(3);

/// 字符计数流控窗口（2026-11-05 修订）。
///
/// `Bf:` 第二个数是**此刻剩余**的 RX 字节，不是总容量；任何时刻剩余值都不超过容量，
/// 所以观测到的最大剩余值是容量的安全下界，在顺序信道没有未确认字节时观测到的值就等于容量。
/// - `idle_confirmed`：窗口 = 最大剩余 − 1（经典 GRBL 空闲报 128、实际可存 127），可低于初始猜测；
/// - 只有发送中的观测：窗口 = max(初始猜测, 最大剩余 − 1)，只升不降；
/// - 始终不超过 `cap`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RxWindow {
    guess: usize,
    max_free_seen: Option<usize>,
    idle_confirmed: bool,
    cap: usize,
}

impl RxWindow {
    fn observe(&mut self, free: usize, idle: bool) {
        self.max_free_seen = Some(self.max_free_seen.map_or(free, |seen| seen.max(free)));
        self.idle_confirmed |= idle;
    }

    fn size(&self) -> usize {
        let observed = self.max_free_seen.map(|free| free.saturating_sub(1));
        let size = match observed {
            Some(observed) if self.idle_confirmed => observed,
            Some(observed) => self.guess.max(observed),
            None => self.guess,
        };
        size.min(self.cap)
    }
}

pub struct DeviceConnection<T: DeviceTransport> {
    pub transport: T,
    pub state: DeviceState,
    pub last_response_at: Instant,
    pub max_spindle_speed: Option<f64>,
    /// `$10` / `$Report/Status` 掩码：bit0 = MPos(1)/WPos(0)，bit1 = 附带 `Bf:` 缓冲字段。
    /// FluidNC 出厂默认 1，此时状态报告不带 `Bf:`，字符计数流控无法自适应。
    pub report_status_mask: Option<u32>,
    /// `$CD` 读到的 FluidNC 配置特性；`None` 表示不是 FluidNC 或未读取。
    pub features: Option<MachineFeatures>,
    /// 当前通道已通过 `$RI=<ms>` 开启固件自动上报的周期；0 表示未开启。
    pub auto_report_ms: u32,
    /// SD 脱机作业进行中：固件会丢弃本通道的顺序指令，只接受实时单字节。
    pub sd_job_active: bool,
    /// 最近一条固件附带的人类可读说明（`[MSG:INFO: ALARM: ...]` / `[MSG:ERR: ...]`），
    /// 用于给随后的 `ALARM:N` / `error:N` 补充原因。
    last_explanation: Option<String>,
    /// 流式发送或脱机监控期间绑定的实时进度槽位：收到 `<...>` 状态报告时自动同步最新坐标报文。
    progress_sink: Option<Arc<JobProgress>>,
    /// 顺序信道没有未确认字节时 `Bf:` 报告的最大剩余 RX 字节，即设备 RX 缓冲的真实容量；
    /// `None` 表示尚未观测到。由预检 `?` 与完成等待阶段记录，字符计数窗口据此确定。
    pub rx_capacity_observed: Option<usize>,
    /// 机器 X/Y 行程：握手 `$$` 的 `$130/$131`，FluidNC `$CD` 的 `max_travel_mm` 按字段覆盖合并。
    /// 发送前用来复核任务范围（2026-11-06）；都没有时为空，调用方跳过该项检查。
    pub travel_limits: MachineLimits,
    /// 最近一次状态报告里的机床坐标 `MPos`（用于断点续雕在 `0x18` 复位后精确重建 `G92` 偏移）。
    pub last_mpos: Option<(f64, f64, f64)>,
    /// 最近一次 `cancel()` 发送 `0x18` 前观测到的状态报告（用于评估规划器队列剩余深度）。
    pub last_cancel_report: Option<StatusReport>,
    /// `$CD` / `$RI` 超时且未能排空终止应答后置位，阻止后续顺序发送消费迟到的 `ok`。
    sequential_channel_dirty: bool,
}

impl<T: DeviceTransport> DeviceConnection<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            state: DeviceState::Disconnected,
            last_response_at: Instant::now(),
            max_spindle_speed: None,
            report_status_mask: None,
            features: None,
            auto_report_ms: 0,
            sd_job_active: false,
            last_explanation: None,
            progress_sink: None,
            rx_capacity_observed: None,
            travel_limits: MachineLimits::default(),
            last_mpos: None,
            last_cancel_report: None,
            sequential_channel_dirty: false,
        }
    }

    pub fn state(&self) -> DeviceState {
        self.state
    }

    /// 记录固件说明行；返回是否为说明行。
    fn note_line(&mut self, line: &str) -> bool {
        if let Some(text) = extract_device_explanation(line) {
            self.last_explanation = Some(text);
            return true;
        }
        false
    }

    /// 把 `ALARM:N` / `error:N` 与之前收到的说明行拼成一条错误信息。
    fn explain(&mut self, code_line: &str) -> String {
        match self.last_explanation.take() {
            Some(text) => format!("{code_line} ({text})"),
            None => code_line.to_string(),
        }
    }

    /// 状态报告是否带 `Bf:`：`$10` bit1。未知（非 GRBL 兼容设置）时返回 `None`。
    pub fn reports_buffer_state(&self) -> Option<bool> {
        self.report_status_mask.map(|mask| mask & 0b10 != 0)
    }

    /// 字符计数流控的当前窗口（字节，2026-11-05 修订）。
    ///
    /// - 已在顺序信道空闲时观测到 `Bf:`：窗口 = 观测到的最大剩余字节 − 1（经典 GRBL 空闲报 128、
    ///   环形缓冲只能存 127），不再叠加任何高于设备报告值的下限；
    /// - 尚未观测：经 `$CD` 确认是 FluidNC 时取 `min(传输层建议, 256)`，否则一律 `min(传输层建议, 127)`；
    /// - 固件不报 `Bf:`（`$10` 缺 bit1）时上限 256，其余情况硬上限 [`MAX_RX_WINDOW`]。
    pub fn effective_rx_window(&self, transport_default: usize) -> usize {
        self.rx_window(self.rx_window_guess(transport_default))
            .size()
    }

    /// 未观测到设备容量时的保守初始窗口。
    fn rx_window_guess(&self, transport_default: usize) -> usize {
        let limit = if self.features.is_some() {
            FLUIDNC_CHANNEL_RX_WINDOW
        } else {
            GRBL_CLASSIC_RX_WINDOW
        };
        transport_default.min(limit)
    }

    fn rx_window(&self, guess: usize) -> RxWindow {
        let cap = if self.reports_buffer_state() == Some(false) {
            FLUIDNC_CHANNEL_RX_WINDOW
        } else {
            MAX_RX_WINDOW
        };
        RxWindow {
            guess: guess.min(cap),
            max_free_seen: self.rx_capacity_observed,
            idle_confirmed: self.rx_capacity_observed.is_some(),
            cap,
        }
    }

    /// 顺序信道无未确认字节时收到的 `Bf:` 剩余值即 RX 缓冲容量，取历史最大值。
    fn record_idle_rx_free(&mut self, report: &StatusReport) {
        if let Some(free) = report.rx_buffer_free {
            self.rx_capacity_observed = Some(
                self.rx_capacity_observed
                    .map_or(free, |seen| seen.max(free)),
            );
        }
    }

    pub fn handshake(&mut self) -> Result<(), String> {
        self.handshake_with_deadline(Duration::from_secs(3))
            .map(|_| ())
    }

    /// 有界握手：消费完整 `$$` 响应直到终止 `ok`，避免旧 ACK 被下一条命令使用。
    /// 读超时视为"设备沉默"继续等待；EOF 视为断开。返回收到的设置行。
    ///
    /// USB 串口打开 ESP32 / Arduino 控制器时 DTR/RTS 电平跳变可能触发板卡软重启（约 0.8~1.5s 后才吐出
    /// `Grbl ...` 启动横幅，此前发出的首条 `$$` 会被引导程序丢弃）；而 FluidNC Telnet 建连也会先发欢迎横幅但
    /// 不会丢弃首条 `$$`（紧接着就会吐出 `$` 设置行）。因此：
    /// - 尚未收到 `$` 设置行时遇到欢迎/复位横幅只记标记，不立刻盲发第二条 `$$`（避免双份响应错位污染后续 ACK），
    ///   仅当横幅之后读超时仍未收到任何 `$` 设置行时才补发一次 `$$`；
    /// - 若已在输出 `$` 设置行中途收到复位横幅，则清空半截状态并补发一次 `$$`；
    /// - 只有真正收到了 `$` 设置行但 `$32!=1` 时才报激光模式门禁错误。
    pub fn handshake_with_deadline(&mut self, deadline: Duration) -> Result<Vec<String>, String> {
        self.handshake_with_deadline_cancellable(deadline, None)
    }

    pub fn handshake_with_deadline_cancellable(
        &mut self,
        deadline: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<Vec<String>, String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("用户取消，未执行握手".into());
        }
        self.state = DeviceState::Disconnected;
        self.max_spindle_speed = None;
        self.report_status_mask = None;
        self.rx_capacity_observed = None;
        self.travel_limits = MachineLimits::default();
        self.last_mpos = None;
        self.last_cancel_report = None;
        self.sequential_channel_dirty = false;
        self.transport.write_line("$$")?;
        let start = Instant::now();
        let mut laser_mode = None;
        let mut settings = Vec::new();
        let mut got_settings = false;
        let mut saw_reset_banner = false;
        let mut retried_after_boot = false;
        let mut terminated = false;
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err("用户取消，停止握手".into());
            }
            if start.elapsed() > deadline {
                break;
            }
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    if self.note_line(&line) {
                        continue;
                    }
                    if is_reset_banner(&line) {
                        saw_reset_banner = true;
                        if got_settings && !retried_after_boot {
                            retried_after_boot = true;
                            laser_mode = None;
                            settings.clear();
                            got_settings = false;
                            self.max_spindle_speed = None;
                            self.report_status_mask = None;
                            self.transport.write_line("$$")?;
                        }
                        continue;
                    }
                    if line.starts_with('$') {
                        got_settings = true;
                        settings.push(line.clone());
                    }
                    if let Some(value) = line.strip_prefix("$32=") {
                        laser_mode = Some(value.trim() == "1");
                    }
                    if let Some(value) = line.strip_prefix("$30=") {
                        self.max_spindle_speed = value
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|value| value.is_finite() && *value > 0.0);
                    }
                    if let Some(value) = line.strip_prefix("$10=") {
                        self.report_status_mask = value.trim().parse::<u32>().ok();
                    }
                    if line.eq_ignore_ascii_case("ok") {
                        if got_settings {
                            terminated = true;
                            break;
                        }
                        continue;
                    }
                    if line.starts_with("error:") || line.starts_with("ALARM:") {
                        self.state = DeviceState::Alarm;
                        return Err(self.explain(&line));
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    if saw_reset_banner && !got_settings && !retried_after_boot {
                        retried_after_boot = true;
                        self.transport.write_line("$$")?;
                    }
                    continue;
                }
            }
        }
        if laser_mode != Some(true) {
            if got_settings {
                self.state = DeviceState::Alarm;
                return Err(
                    "$32=1 固件激光模式门禁未通过，控制器已进入 Alarm 态，拒绝发送".to_string(),
                );
            }
            self.state = DeviceState::Disconnected;
            return Err(format!(
                "{}ms 内未收到 GRBL 固件设置响应（若机器刚上电重启请稍候重试；若正在执行 SD 脱机作业，固件会丢弃顺序指令，请先急停）",
                deadline.as_millis()
            ));
        }
        if !terminated {
            return Err("固件设置响应不完整（缺少终止 ok），拒绝发送".into());
        }
        self.state = DeviceState::Ready;
        self.travel_limits = parse_grbl_settings_limits(&settings);
        Ok(settings)
    }

    /// 握手探测阶段发送只读 `$I` 读取固件版本与 FluidNC `Machine:` 名称（r66，不移动轴、不出光）。
    /// 仅用于短探测连接（返回后即关闭通道）；旧模拟固件未响应或返回 `error:3` 时静默返回 `None`。
    pub fn query_identity(&mut self, timeout: Duration) -> Option<crate::DeviceIdentity> {
        if self.transport.write_line("$I").is_err() {
            return None;
        }
        let deadline = Instant::now() + timeout.min(Duration::from_millis(800));
        let mut identity = crate::DeviceIdentity::default();
        while Instant::now() < deadline {
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    crate::parse_device_identity_line(trimmed, &mut identity);
                    if trimmed.eq_ignore_ascii_case("ok")
                        || trimmed.to_ascii_lowercase().starts_with("error:")
                        || trimmed.starts_with("ALARM:")
                    {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        (!identity.is_empty()).then_some(identity)
    }

    /// 握手后读取 FluidNC `$CD` 配置并缓存到 `features`。仅旧 GRBL 返回 `error:3`（不支持 `$CD`）时
    /// 静默返回 `Ok(false)` 并保持 `features = None`；复位、报警、断线或超时均返回 `Err`，
    /// 按字段叠加合并行程上限，不擦除 `$$` 已报告的其余轴或运动参数。
    pub fn load_features(&mut self, timeout: Duration) -> Result<bool, String> {
        match self.fetch_fluidnc_config(timeout)? {
            Some((limits, features)) => {
                self.travel_limits.merge_prefer(&limits);
                self.features = Some(features);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 开启 FluidNC 本通道自动状态上报（`$RI=<ms>`，最小 50ms）。仅在已确认是 FluidNC
    /// （`features` 已加载）时发送；旧 GRBL 会返回 `error:3`，此时静默关闭该功能。
    /// 必须在流式发送开始前调用并消费掉它的 `ok`，避免 ack 计数错位。
    ///
    /// 2026-11-05 修订：`$RI` 的应答在 2 秒内未到时不再直接放行（迟到的 `ok` 会被当成首行 G-code 的 ack），
    /// 而是补发 `G4 P0`（不运动、不出光）同步，按顺序应答规则在限定时间内收齐 `$RI` 与 `G4 P0`
    /// 两个终止应答后才放行；收不齐则置 `Lost` 与失步标记并返回错误，调用方不得开始流式发送。
    pub fn enable_auto_report(&mut self, interval_ms: u32) -> Result<bool, String> {
        self.enable_auto_report_cancellable(interval_ms, None)
    }

    /// 可取消版 [`Self::enable_auto_report`]：发 `$RI`、等应答及补发 `G4 P0` 同步前均复查 `cancel`，
    /// 命中后立刻中止并交由调用方走实时急停收尾。
    pub fn enable_auto_report_cancellable(
        &mut self,
        interval_ms: u32,
        cancel: Option<&CancellationToken>,
    ) -> Result<bool, String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("用户取消，停止开启自动上报".into());
        }
        if self.sequential_channel_dirty {
            return Err(
                "控制器顺序信道此前未完成响应排空（$CD/$RI 超时残留），拒绝开启自动上报并禁止发送，请重连设备"
                    .into(),
            );
        }
        if self.features.is_none() {
            return Ok(false);
        }
        let interval_ms = interval_ms.max(50);
        self.transport.write_line(&format!("$RI={interval_ms}"))?;
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("用户取消，停止开启自动上报".into());
        }
        let mut acks = Vec::new();
        self.collect_terminal_acks(&mut acks, 1, AUTO_REPORT_ACK_TIMEOUT, cancel)?;
        if acks.is_empty() {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err("用户取消，停止同步自动上报".into());
            }
            self.transport.write_line("G4 P0")?;
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err("用户取消，停止等待自动上报同步".into());
            }
            self.collect_terminal_acks(&mut acks, 2, AUTO_REPORT_RESYNC_TIMEOUT, cancel)?;
            if acks.len() < 2 {
                if cancel.is_some_and(CancellationToken::is_cancelled) {
                    return Err("用户取消，停止等待自动上报同步".into());
                }
                // 无法判断收到的应答属于哪条命令：保守地认为自动上报可能已开启，同时锁定顺序信道，防止迟到 ok 污染后续任务
                self.auto_report_ms = interval_ms;
                self.state = DeviceState::Lost;
                self.sequential_channel_dirty = true;
                return Err(format!(
                    "$RI 自动上报设置在 {}ms 内未确认，G4 P0 同步也未在 {}ms 内收齐应答，ack 计数无法对齐；未开始发送任何 G-code",
                    AUTO_REPORT_ACK_TIMEOUT.as_millis(),
                    AUTO_REPORT_RESYNC_TIMEOUT.as_millis()
                ));
            }
        }
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("用户取消，停止开启自动上报".into());
        }
        let enabled = acks[0].is_ok();
        self.auto_report_ms = if enabled { interval_ms } else { 0 };
        if let Some(Err(line)) = acks.get(1) {
            return Err(format!(
                "同步指令 G4 P0 被固件拒绝（{line}），未开始发送任何 G-code"
            ));
        }
        Ok(enabled)
    }

    /// 读取顺序信道的终止应答（`ok` → `Ok(())`，`error:N` → `Err`）直到凑满 `want` 个或超时。
    /// 状态报告与说明行照常分类；复位横幅 / ALARM / 断开直接返回错误。
    fn collect_terminal_acks(
        &mut self,
        acks: &mut Vec<Result<(), String>>,
        want: usize,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        let start = Instant::now();
        while acks.len() < want && start.elapsed() < timeout {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err("用户取消，停止等待自动上报确认".into());
            }
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    if line.eq_ignore_ascii_case("ok") {
                        acks.push(Ok(()));
                    } else if line.to_ascii_lowercase().starts_with("error:") {
                        let message = self.explain(&line);
                        acks.push(Err(message));
                    } else {
                        self.classify_line(&line, "设置自动上报")?;
                    }
                }
                Ok(None) => {
                    self.state = DeviceState::Disconnected;
                    return Err("设置自动上报时连接断开".into());
                }
                Err(_) => continue,
            }
        }
        Ok(())
    }

    /// 关闭自动上报（`$RI=0`）。尽力在窗口内消费其终止应答，若仍未收齐则标记信道未排空。
    pub fn disable_auto_report(&mut self) {
        let _ = self.disable_auto_report_cancellable(None);
    }

    /// 可取消版 [`Self::disable_auto_report`]；未取消时返回 `true`，命中取消时跳过或中止并返回 `false`。
    pub fn disable_auto_report_cancellable(&mut self, cancel: Option<&CancellationToken>) -> bool {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return false;
        }
        if self.auto_report_ms == 0 {
            return true;
        }
        self.auto_report_ms = 0;
        if self.transport.write_line("$RI=0").is_ok() {
            let start = Instant::now();
            let mut got_term = false;
            let mut saw_eof = false;
            while start.elapsed() < Duration::from_millis(800) {
                if cancel.is_some_and(CancellationToken::is_cancelled) {
                    return false;
                }
                match self.transport.read_line() {
                    Ok(Some(line))
                        if line.eq_ignore_ascii_case("ok") || line.starts_with("error:") =>
                    {
                        got_term = true;
                        break;
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        saw_eof = true;
                        break;
                    }
                    Err(_) => continue,
                }
            }
            if !got_term && !saw_eof {
                self.sequential_channel_dirty = true;
            }
        }
        !cancel.is_some_and(CancellationToken::is_cancelled)
    }

    /// 主动状态轮询的间隔：已开启自动上报时放宽到两倍上报周期作为兜底，否则用调用方给定值。
    fn poll_interval(&self, default: Duration) -> Duration {
        if self.auto_report_ms > 0 {
            Duration::from_millis(u64::from(self.auto_report_ms) * 2).max(default)
        } else {
            default
        }
    }
}

enum IdleCheckError {
    Alarm(String),
    Fatal(String),
}

impl IdleCheckError {
    fn into_message(self) -> String {
        match self {
            Self::Alarm(msg) | Self::Fatal(msg) => msg,
        }
    }
}

enum DrainResult {
    Terminated,
    Disconnected,
    TimedOut,
}

impl<T: DeviceTransport> DeviceConnection<T> {
    fn drain_until_terminator(&mut self, timeout: Duration) -> DrainResult {
        let start = Instant::now();
        while start.elapsed() < timeout {
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    if let Some(report) = parse_status_report(&line) {
                        self.sd_job_active = report.has_sd_job();
                        if let Some(mpos) = report.mpos {
                            self.last_mpos = Some(mpos);
                        }
                        continue;
                    }
                    if line.eq_ignore_ascii_case("ok")
                        || line.to_ascii_lowercase().starts_with("error:")
                        || line.starts_with("ALARM:")
                    {
                        return DrainResult::Terminated;
                    }
                }
                Ok(None) => return DrainResult::Disconnected,
                Err(_) => continue,
            }
        }
        DrainResult::TimedOut
    }
}

impl<T: DeviceTransport> DeviceConnection<T> {
    /// 自适应功率标尺检查：只要设备 $30 是合法有限正数即通过，并返回设备实际标尺（如 255.0、1000.0 等）。
    pub fn verify_job_ready_adaptive(&mut self, timeout: Duration) -> Result<f64, String> {
        self.verify_job_ready_adaptive_cancellable(timeout, None)
    }

    pub fn verify_job_ready_adaptive_cancellable(
        &mut self,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<f64, String> {
        let scale = self
            .max_spindle_speed
            .ok_or_else(|| "固件未报告有效的 $30 功率标尺（缺少 $30 或非正数）".to_string())?;
        self.require_idle_typed(timeout, cancel)
            .map_err(IdleCheckError::into_message)?;
        Ok(scale)
    }

    /// 断点续雕预检：先做标准空闲检查；仅当控制器明确处于无阻断引脚的 `<Alarm>` 锁定态时，
    /// 才执行 `$X` 解锁（含等待 `after_unlock` 宏回到 Idle）再复核空闲与坐标；`Unknown` 等非 Alarm 态拒绝盲目解锁。
    pub fn verify_job_ready_for_resume(&mut self, timeout: Duration) -> Result<f64, String> {
        self.verify_job_ready_for_resume_cancellable(timeout, None)
    }

    pub fn verify_job_ready_for_resume_cancellable(
        &mut self,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<f64, String> {
        let scale = self
            .max_spindle_speed
            .ok_or_else(|| "固件未报告有效的 $30 功率标尺（缺少 $30 或非正数）".to_string())?;
        match self.require_idle_typed(timeout, cancel) {
            Ok(()) => {
                // 已在 Idle 时发送 $X 为固件级空操作（立即回 ok），保持续雕契约一致
                self.send_line_wait_ack_cancellable("$X", cancel)?;
                Ok(scale)
            }
            Err(IdleCheckError::Alarm(_)) => {
                self.unlock_cancellable(cancel)?;
                self.require_idle_typed(timeout, cancel)
                    .map_err(IdleCheckError::into_message)?;
                Ok(scale)
            }
            Err(IdleCheckError::Fatal(err)) => Err(err),
        }
    }

    /// 发送 $CD 读取 FluidNC 的 YAML 配置文件并解析出机械行程与外设引脚；必须收到终止 `ok`。
    /// 仅旧 GRBL 返回 `error:3`（不支持 `$CD`）时返回 `Ok(None)`；复位、报警、断线、超时或其他错误均返回 `Err`。
    pub fn fetch_fluidnc_config(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<(MachineLimits, MachineFeatures)>, String> {
        if self.sequential_channel_dirty {
            return Err("控制器顺序信道此前未排空，拒绝读取 $CD，请重连设备".into());
        }
        self.transport.write_line("$CD")?;
        let start = Instant::now();
        let mut yaml_lines = Vec::new();
        let mut terminated = false;
        let mut saw_eof = false;
        while start.elapsed() < timeout {
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    if line.eq_ignore_ascii_case("ok") {
                        terminated = true;
                        break;
                    }
                    let lower = line.trim().to_ascii_lowercase();
                    if let Some(rest) = lower.strip_prefix("error:3")
                        && !rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                    {
                        return Ok(None);
                    }
                    if lower.starts_with("error:") {
                        return Err(format!("读取 $CD 报错: {line}"));
                    }
                    if is_reset_banner(&line) {
                        self.state = DeviceState::Lost;
                        return Err(format!("读取 $CD 时控制器复位（{}）", line.trim()));
                    }
                    if line.starts_with("ALARM:") {
                        self.state = DeviceState::Alarm;
                        return Err(self.explain(&line));
                    }
                    if let Some(report) = parse_status_report(&line) {
                        self.sd_job_active = report.has_sd_job();
                        if let Some(mpos) = report.mpos {
                            self.last_mpos = Some(mpos);
                        }
                        continue;
                    }
                    yaml_lines.push(line);
                }
                Ok(None) => {
                    saw_eof = true;
                    break;
                }
                Err(_) => continue,
            }
        }
        if !terminated {
            if saw_eof {
                self.state = DeviceState::Disconnected;
                return Err(if yaml_lines.is_empty() {
                    "读取 $CD 时连接断开（未收到响应）".into()
                } else {
                    "读取 $CD 响应不完整（连接断开，缺少终止 ok）".into()
                });
            }
            match self.drain_until_terminator(Duration::from_millis(150)) {
                DrainResult::Terminated => {
                    return Err("读取 $CD 超时（响应延迟已排空并丢弃，未使用半截配置）".into());
                }
                DrainResult::Disconnected => {
                    self.state = DeviceState::Disconnected;
                    return Err("读取 $CD 超时且连接已断开".into());
                }
                DrainResult::TimedOut => {
                    self.sequential_channel_dirty = true;
                    self.state = DeviceState::Lost;
                    return Err(
                        "读取 $CD 未收到终止 ok（顺序信道存在残留/失步风险，请重连设备）".into(),
                    );
                }
            }
        }
        let raw = yaml_lines.join("\n");
        Ok(Some(parse_fluidnc_yaml(&raw)))
    }

    /// 消费一行状态类响应的公共处理：记录说明行、识别复位横幅、更新 SD 作业标记。
    /// 返回 `Some(report)` 表示这是状态报告；`Err` 表示遇到必须中止的行（复位 / ALARM / error）。
    fn classify_line(&mut self, line: &str, context: &str) -> Result<Option<StatusReport>, String> {
        self.last_response_at = Instant::now();
        if self.note_line(line) {
            return Ok(None);
        }
        if is_reset_banner(line) {
            self.state = DeviceState::Lost;
            self.sd_job_active = false;
            return Err(format!(
                "控制器在{context}时复位（{}），执行结果未知；禁止自动重发",
                line.trim()
            ));
        }
        if line.starts_with("ALARM:") {
            self.state = DeviceState::Alarm;
            self.sd_job_active = false;
            // 保持以 ALARM:N 开头，便于 diagnose_device_message 继续按码翻译
            return Err(self.explain(line));
        }
        if line.to_ascii_lowercase().starts_with("error:") {
            return Err(self.explain(line));
        }
        if let Some(report) = parse_status_report(line) {
            self.sd_job_active = report.has_sd_job();
            if let Some(mpos) = report.mpos {
                self.last_mpos = Some(mpos);
            }
            if let Some(prog) = &self.progress_sink {
                prog.record_status_report(&report.raw);
                // 暂停状态以控制器报告为准（2026-11-18）：门开或实体保持也算暂停，`~` 被忽略时不误显示“在走”
                match report.activity {
                    MachineActivity::Held | MachineActivity::DoorOpen => {
                        prog.paused
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                    MachineActivity::Busy | MachineActivity::Idle => {
                        prog.paused
                            .store(false, std::sync::atomic::Ordering::Release);
                    }
                    _ => {}
                }
            }
            return Ok(Some(report));
        }
        Ok(None)
    }

    /// 发送前预检：`?` 一次，要求 Idle、无 SD 作业、无 E/F/D 阻断引脚。
    /// Run/Jog/Home 直接拒绝（不等待），Hold/Door/Alarm/Sleep/Check/Unknown 均拒绝并说明原因。
    fn require_idle_typed(
        &mut self,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), IdleCheckError> {
        if self.sequential_channel_dirty {
            return Err(IdleCheckError::Fatal(
                "控制器顺序信道存在未排空残留（$CD/$RI 超时），拒绝执行任务，请重连设备".into(),
            ));
        }
        if matches!(
            self.state,
            DeviceState::Lost | DeviceState::Disconnected | DeviceState::Alarm
        ) {
            return Err(IdleCheckError::Fatal(
                "控制器在预检期间发生复位、报警或连接断开（状态异常），拒绝执行任务，请重连设备"
                    .into(),
            ));
        }
        self.transport
            .write_realtime(RealtimeCommand::StatusQuery)
            .map_err(IdleCheckError::Fatal)?;
        let start = Instant::now();
        while start.elapsed() < timeout {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err(IdleCheckError::Fatal("用户取消，停止等待控制器".into()));
            }
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    let report = match self
                        .classify_line(&line, "预检")
                        .map_err(IdleCheckError::Fatal)?
                    {
                        Some(report) => report,
                        None => continue,
                    };
                    // 预检前所有顺序命令都已确认，此刻的 Bf: 剩余值就是 RX 缓冲容量
                    self.record_idle_rx_free(&report);
                    let blocking = report.blocking_pins();
                    if !blocking.is_empty() {
                        self.state = DeviceState::Alarm;
                        return Err(IdleCheckError::Fatal(format!(
                            "控制器输入引脚处于触发状态：{}（状态 {}），固件会拒绝解锁与运动，请先复位物理开关",
                            blocking.join("、"),
                            report.raw
                        )));
                    }
                    match report.activity {
                        MachineActivity::Idle if !report.has_sd_job() => {
                            self.state = DeviceState::Ready;
                            return Ok(());
                        }
                        MachineActivity::Idle => {
                            return Err(IdleCheckError::Fatal(format!(
                                "控制器正在执行 SD 脱机作业 ({})，拒绝启动新任务",
                                report.raw
                            )));
                        }
                        MachineActivity::Busy => {
                            return Err(IdleCheckError::Fatal(format!(
                                "控制器正在运行中 ({})，拒绝启动新任务",
                                report.raw
                            )));
                        }
                        MachineActivity::Held => {
                            self.state = DeviceState::Hold;
                            return Err(IdleCheckError::Fatal(format!(
                                "控制器处于进给保持 ({})，请先恢复或急停后再发送",
                                report.raw
                            )));
                        }
                        MachineActivity::DoorOpen => {
                            self.state = DeviceState::Hold;
                            return Err(IdleCheckError::Fatal(format!(
                                "安全门打开 ({})，请关门后再发送",
                                report.raw
                            )));
                        }
                        MachineActivity::Sleep => {
                            self.state = DeviceState::Alarm;
                            return Err(IdleCheckError::Fatal(format!(
                                "控制器处于休眠 ({})，需要复位唤醒",
                                report.raw
                            )));
                        }
                        MachineActivity::Check => {
                            self.state = DeviceState::Alarm;
                            return Err(IdleCheckError::Fatal(format!(
                                "控制器处于 G-code 检查模式 ({})，请先退出检查模式",
                                report.raw
                            )));
                        }
                        MachineActivity::Alarm => {
                            self.state = DeviceState::Alarm;
                            return Err(IdleCheckError::Alarm(format!(
                                "控制器尚未安全就绪：{}，需要解锁或人工核验",
                                report.raw
                            )));
                        }
                        MachineActivity::Unknown => {
                            self.state = DeviceState::Alarm;
                            return Err(IdleCheckError::Fatal(format!(
                                "控制器返回未知状态（{}），禁止自动解锁，请到设备旁人工核验",
                                report.raw
                            )));
                        }
                    }
                }
                Ok(None) => {
                    self.state = DeviceState::Disconnected;
                    return Err(IdleCheckError::Fatal(
                        "等待 Idle 时连接断开，执行结果未知".into(),
                    ));
                }
                Err(_) => continue,
            }
        }
        self.state = DeviceState::Lost;
        Err(IdleCheckError::Fatal(
            "未收到 Idle 状态，执行结果未知；禁止自动重发".into(),
        ))
    }

    /// 发送文件全部内容后，等待控制器清空规划器缓冲区并恢复 Idle 状态。
    /// Run / Jog / Home（含解锁、回零后的自动宏）持续等待；Hold / Door 也等待（用户可能恢复），
    /// Alarm / Sleep / Check 立即失败；支持取消与超时。
    /// 当控制器仍在执行长距离慢速切割（`<Run|...>` 的坐标或规划器块数持续变化）时，刷新无进展计时器，
    /// 避免 16~32 段慢切耗尽规划器缓冲超过 30 秒时被误判失联强行停机（硬上限 30 分钟兜底）。
    pub fn wait_until_idle(
        &mut self,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        match self.wait_until_idle_or_cancel(timeout, cancel)? {
            true => Ok(()),
            false => Err("用户取消，停止等待控制器".into()),
        }
    }

    /// 等待控制器回到 Idle：已确认空闲返回 `Ok(true)`，主动取消返回 `Ok(false)`；
    /// 读到的报警、复位、断线或超时一律作为 `Err` 返回，不与取消混淆。
    pub fn wait_until_idle_or_cancel(
        &mut self,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<bool, String> {
        const MAX_ACTIVE_DRAIN: Duration = Duration::from_secs(1800);
        let start = Instant::now();
        let mut last_activity_at = start;
        let mut last_motion_sig: Option<(
            Option<(i64, i64, i64)>,
            Option<(i64, i64, i64)>,
            Option<usize>,
        )> = None;
        let hard_cap = timeout.max(MAX_ACTIVE_DRAIN);
        let mut last_poll = Instant::now();
        let interval = self.poll_interval(Duration::from_millis(200));
        self.transport
            .write_realtime(RealtimeCommand::StatusQuery)?;
        while last_activity_at.elapsed() < timeout && start.elapsed() < hard_cap {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Ok(false);
            }
            if last_poll.elapsed() >= interval {
                let _ = self.transport.write_realtime(RealtimeCommand::StatusQuery);
                last_poll = Instant::now();
            }
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    let report = match self.classify_line(&line, "等待完成")? {
                        Some(report) => report,
                        None => continue,
                    };
                    // 完成等待阶段同步指令已确认，顺序信道无未确认字节
                    self.record_idle_rx_free(&report);
                    match report.activity {
                        MachineActivity::Idle if !report.has_sd_job() => {
                            self.state = DeviceState::Ready;
                            return Ok(true);
                        }
                        MachineActivity::Idle | MachineActivity::Busy => {
                            let quantize = |p: (f64, f64, f64)| {
                                (
                                    (p.0 * 1000.0).round() as i64,
                                    (p.1 * 1000.0).round() as i64,
                                    (p.2 * 1000.0).round() as i64,
                                )
                            };
                            let sig = (
                                report.mpos.map(quantize),
                                report.wpos.map(quantize),
                                report.planner_free,
                            );
                            if (sig.0.is_some() || sig.1.is_some() || sig.2.is_some())
                                && let Some(prev) = last_motion_sig
                                && prev != sig
                            {
                                last_activity_at = Instant::now();
                            }
                            last_motion_sig = Some(sig);
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        MachineActivity::Held | MachineActivity::DoorOpen => {
                            // 保持 / 门开是人为的暂停，控制器仍在回报：不算“无进展”，一直等到恢复
                            // （2026-11-18：走边框暂停去摆木板超过 30 秒曾被判超时并自动 0x18；硬上限 30 分钟兜底）
                            self.state = DeviceState::Hold;
                            last_activity_at = Instant::now();
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        MachineActivity::Alarm
                        | MachineActivity::Sleep
                        | MachineActivity::Check
                        | MachineActivity::Unknown => {
                            self.state = DeviceState::Alarm;
                            return Err(format!(
                                "控制器进入异常状态：{}，需要人工核验",
                                report.raw
                            ));
                        }
                    }
                }
                Ok(None) => {
                    self.state = DeviceState::Disconnected;
                    return Err("等待 Idle 时连接断开，执行结果未知".into());
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
            }
        }
        self.state = DeviceState::Lost;
        Err("等待控制器空闲超时，执行结果未知；禁止自动重发".into())
    }

    /// 校验 SD 卡上的文件 SHA-256 哈希值（发送 $File/ShowHash=<path> 并比对固件计算结果）。
    /// 返回 `Ok(true)` 表示固件返回了哈希并匹配；`Ok(false)` 表示固件未启用 HashFS（返回 ok 但无哈希），
    /// 调用方必须把"未做物理校验"明确告知用户；哈希不匹配或固件报错返回 `Err`。
    pub fn verify_sd_file_hash(
        &mut self,
        file_path: &str,
        expected_sha256: &str,
        timeout: Duration,
    ) -> Result<bool, String> {
        self.transport
            .write_line(&format!("$File/ShowHash={file_path}"))?;
        let start = Instant::now();
        let mut got_hash: Option<String> = None;
        let mut terminated = false;
        // 2026-11-09：FluidNC 的 JSON 输出每满 100 个字符切一行（`JSONEncoder::add`），
        // `"value":"` 前缀占 46 个字符，64 位哈希必然被切成两行；Telnet 为原始分段，
        // 串口每段前加 `[JSON:`（`UartChannel::out`）。先拼起来再取值。
        let mut json = String::new();
        while start.elapsed() < timeout {
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    if self.note_line(&line) {
                        continue;
                    }
                    if !line.eq_ignore_ascii_case("ok")
                        && !line.starts_with("error:")
                        && !line.starts_with("ALARM:")
                        && !line.starts_with("[MSG:")
                        && !line.starts_with('<')
                    {
                        json.push_str(
                            line.strip_prefix("[JSON:")
                                .unwrap_or(&line)
                                .trim_end_matches(['\r', '\n']),
                        );
                    }
                    if line.eq_ignore_ascii_case("ok") {
                        terminated = true;
                        break;
                    }
                    if is_reset_banner(&line) {
                        self.state = DeviceState::Lost;
                        return Err(format!(
                            "校验 SD 文件哈希时控制器复位（{}），拒绝执行",
                            line.trim()
                        ));
                    }
                    if line.starts_with("ALARM:") {
                        self.state = DeviceState::Alarm;
                        let msg = self.explain(&line);
                        return Err(format!("固件计算文件哈希期间触发报警: {msg}"));
                    }
                    if line.starts_with("error:") {
                        let msg = self.explain(&line);
                        return Err(format!("固件计算文件哈希失败: {msg}"));
                    }
                    if let Some(val_idx) = line.find("\"value\"")
                        && let Some(colon) = line[val_idx..].find(':')
                    {
                        let after = &line[val_idx + colon + 1..];
                        let cleaned = after
                            .trim_start_matches(|c: char| c.is_whitespace() || c == '"')
                            .split(['"', ',', '}'])
                            .next()
                            .unwrap_or("")
                            .trim();
                        if cleaned.len() == 64 && cleaned.bytes().all(|b| b.is_ascii_hexdigit()) {
                            got_hash = Some(cleaned.to_ascii_lowercase());
                        }
                    }
                }
                Ok(None) => {
                    self.state = DeviceState::Disconnected;
                    return Err("读取固件文件哈希期间设备连接断开，拒绝执行".into());
                }
                Err(_) => continue,
            }
        }
        if got_hash.is_none() {
            got_hash = sha256_from_hash_json(&json);
        }
        if !terminated {
            match self.drain_until_terminator(Duration::from_millis(150)) {
                DrainResult::Terminated => {
                    return Err("读取固件文件哈希超时（已排空迟到终止符），拒绝执行".into());
                }
                DrainResult::Disconnected => {
                    self.state = DeviceState::Disconnected;
                    return Err("读取固件文件哈希超时且设备连接已断开，拒绝执行".into());
                }
                DrainResult::TimedOut => {
                    self.sequential_channel_dirty = true;
                    self.state = DeviceState::Lost;
                    return Err(
                        "读取固件文件哈希未收到终止 ok（顺序信道存在残留/失步风险），拒绝执行"
                            .into(),
                    );
                }
            }
        }
        match got_hash {
            Some(actual) if actual.eq_ignore_ascii_case(expected_sha256) => Ok(true),
            Some(actual) => Err(format!(
                "SD 卡文件 SHA-256 物理校验不匹配！预期 {expected_sha256}，固件计算返回 {actual}，拒绝执行"
            )),
            None => Ok(false),
        }
    }

    /// 等待 SD 卡脱机作业执行完成：
    /// 持续轮询 '?' 状态，解析形如 `<Run|...|SD:45.50,/sd/job.gcode|...>` 的进度并更新 progress；
    /// 文件读完后固件报 `SD: job.gcode: Sent`，此时进度置 100%，继续等到 Idle 且无 SD 字段才算完成。
    /// 期间本通道的顺序指令会被固件丢弃，只允许实时单字节；支持用户取消（! -> 0x18）。
    pub fn wait_until_sd_completed(
        &mut self,
        timeout: Duration,
        progress: Option<&Arc<JobProgress>>,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        self.wait_sd_job(timeout, progress, cancel, false)
    }

    /// 发 `$SD/Run=<path>` 并监控到作业结束（2026-11-09 按 FluidNC 源码修正）。
    ///
    /// FluidNC 的 `$SD/Run` 回的是推迟应答（`FileCommands.cpp` `runFile` 返回 `Error::Deferred`）：
    /// `ok` / `error:N` 要等整个文件跑完才回，期间本通道不再接收新行，实时字节照常。
    /// 因此不能用“发一行等 ok”——那会在整个作业期间既不更新进度、也看不到取消，暂停后永远等不到 ok。
    /// 这里发出后直接进入监控：看到 `SD:` 进度、运行态或推迟回来的 `ok` 才算已开始，之后回到空闲才完成；
    /// 发出后长时间空闲且没有任何开始迹象则报错。
    pub fn run_sd_file_and_wait(
        &mut self,
        remote_path: &str,
        timeout: Duration,
        progress: Option<&Arc<JobProgress>>,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("用户取消，未启动 SD 脱机任务".into());
        }
        match self.run_sd_file_or_cancel(remote_path, timeout, progress, cancel)? {
            true => Ok(()),
            false => Err("用户取消，已停止 SD 脱机任务".into()),
        }
    }

    /// 已正常完成返回 true，主动取消返回 false，控制器异常或停机序列失败时返回错误。
    pub fn run_sd_file_or_cancel(
        &mut self,
        remote_path: &str,
        timeout: Duration,
        progress: Option<&Arc<JobProgress>>,
        cancel: Option<&CancellationToken>,
    ) -> Result<bool, String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Ok(false);
        }
        self.send_line(&format!("$SD/Run={remote_path}"))?;
        self.last_response_at = Instant::now();
        self.wait_sd_job_or_cancel(timeout, progress, cancel, true)
    }

    fn wait_sd_job(
        &mut self,
        timeout: Duration,
        progress: Option<&Arc<JobProgress>>,
        cancel: Option<&CancellationToken>,
        require_start: bool,
    ) -> Result<(), String> {
        match self.wait_sd_job_or_cancel(timeout, progress, cancel, require_start)? {
            true => Ok(()),
            false => Err("用户取消，已停止 SD 脱机任务".into()),
        }
    }

    fn wait_sd_job_or_cancel(
        &mut self,
        timeout: Duration,
        progress: Option<&Arc<JobProgress>>,
        cancel: Option<&CancellationToken>,
        require_start: bool,
    ) -> Result<bool, String> {
        /// 发出 `$SD/Run` 后这么久仍空闲且没有任何开始迹象，判定没有启动
        const START_GRACE: Duration = Duration::from_secs(5);
        let mut started = !require_start;
        let start = Instant::now();
        let mut last_poll = Instant::now();
        let interval = self.poll_interval(Duration::from_millis(500));
        self.sd_job_active = true;
        self.transport
            .write_realtime(RealtimeCommand::StatusQuery)?;
        let result = loop {
            if start.elapsed() >= timeout {
                self.state = DeviceState::Lost;
                break Err("等待 SD 任务完成超时".to_string());
            }
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                // 监控仍在运行，即使最近一份状态报告已没有 SD 字段。
                break self.cancel_with_sd_monitoring(true).map(|()| false);
            }
            if last_poll.elapsed() >= interval {
                let _ = self.transport.write_realtime(RealtimeCommand::StatusQuery);
                last_poll = Instant::now();
            }
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    // 推迟应答：文件已跑完（或空文件），接下来回到空闲即完成
                    if line.trim().eq_ignore_ascii_case("ok") {
                        started = true;
                        let _ = self.transport.write_realtime(RealtimeCommand::StatusQuery);
                        last_poll = Instant::now();
                        continue;
                    }
                    let report = match self.classify_line(&line, "SD 执行") {
                        Ok(Some(report)) => report,
                        Ok(None) => continue,
                        Err(e) => break Err(e),
                    };
                    if let Some(prog) = progress {
                        prog.record_status_report(&report.raw);
                        let total = prog.total_lines.load(Ordering::Relaxed);
                        match &report.sd {
                            Some(SdProgress::Running { percent, .. }) => {
                                let sent = ((total as f64) * (percent / 100.0)).round() as usize;
                                prog.lines_sent.store(sent.min(total), Ordering::Relaxed);
                            }
                            Some(SdProgress::Sent { .. }) => {
                                prog.lines_sent.store(total, Ordering::Relaxed);
                            }
                            _ => {}
                        }
                    }
                    if report.has_sd_job() || report.activity == MachineActivity::Busy {
                        started = true;
                    }
                    match report.activity {
                        MachineActivity::Idle if !report.has_sd_job() && !started => {
                            if start.elapsed() >= START_GRACE {
                                break Err(
                                    "发出 $SD/Run 后控制器一直空闲，SD 任务没有开始（文件不存在或固件拒绝时会回 error）"
                                        .into(),
                                );
                            }
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        MachineActivity::Idle if !report.has_sd_job() => {
                            if let Some(prog) = progress {
                                let total = prog.total_lines.load(Ordering::Relaxed);
                                prog.lines_sent.store(total, Ordering::Relaxed);
                            }
                            self.state = DeviceState::Ready;
                            break Ok(true);
                        }
                        MachineActivity::Idle | MachineActivity::Busy => {
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        MachineActivity::Held | MachineActivity::DoorOpen => {
                            self.state = DeviceState::Hold;
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        MachineActivity::Alarm
                        | MachineActivity::Sleep
                        | MachineActivity::Check
                        | MachineActivity::Unknown => {
                            self.state = DeviceState::Alarm;
                            break Err(format!("控制器在 SD 执行中进入异常状态：{}", report.raw));
                        }
                    }
                }
                Ok(None) => {
                    self.state = DeviceState::Disconnected;
                    break Err("等待 SD 任务完成时连接断开".into());
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            }
        };
        self.sd_job_active = false;
        result
    }

    pub fn send_line(&self, line: &str) -> Result<(), String> {
        if self.sequential_channel_dirty {
            return Err(
                "控制器顺序信道此前未排空（$CD/$RI 超时残留），拒绝发送，请重连设备".into(),
            );
        }
        if self.state != DeviceState::Ready {
            return Err(format!("设备未就绪（当前状态: {:?}）", self.state));
        }
        if self.sd_job_active {
            return Err("控制器正在执行 SD 脱机作业，固件会静默丢弃顺序指令；只允许急停/暂停/继续等实时命令".into());
        }
        validate_gcode_line(line)?;
        self.transport.write_line(line)
    }

    /// 发送一行并等待 `ok`/`error`。失联判定保持 500ms：若 500ms 内既无 ack 也无任何响应，
    /// 先发 `?` 心跳；心跳后再 500ms 仍无任何字节则判定失联。规划器满载导致 ack 延迟时，
    /// 状态报告 `<...>` 会持续刷新 last_response_at，因而长轨迹不会被误判失联。
    pub fn send_line_wait_ack(&mut self, line: &str) -> Result<(), String> {
        self.send_line_wait_ack_cancellable(line, None)
    }

    pub fn send_line_wait_ack_cancellable(
        &mut self,
        line: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("用户取消，停止发送".into());
        }
        match self.send_line_wait_ack_or_cancel(line, cancel)? {
            true => Ok(()),
            false => Err("用户取消，停止等待 ACK".into()),
        }
    }

    /// 已确认返回 true，主动取消返回 false。读到的报警、复位和断线仍作为错误返回。
    pub fn send_line_wait_ack_or_cancel(
        &mut self,
        line: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<bool, String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Ok(false);
        }
        self.send_line(line)?;
        self.last_response_at = Instant::now();
        let mut heartbeat_sent = false;
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Ok(false);
            }
            let silent = self.last_response_at.elapsed();
            if silent > Duration::from_millis(500) {
                if heartbeat_sent {
                    self.state = DeviceState::Lost;
                    return Err("控制器连续 500ms 无响应，已判定失联，急停无法送达".to_string());
                }
                self.transport
                    .write_realtime(RealtimeCommand::StatusQuery)?;
                heartbeat_sent = true;
                self.last_response_at = Instant::now();
            }
            match self.transport.read_line() {
                Ok(Some(response)) if response.eq_ignore_ascii_case("ok") => {
                    self.last_response_at = Instant::now();
                    return Ok(true);
                }
                Ok(Some(response)) => match self.classify_line(&response, "等待 ACK") {
                    Ok(_) => {
                        heartbeat_sent = false;
                        continue;
                    }
                    Err(e) => return Err(e),
                },
                Ok(None) => {
                    self.state = DeviceState::Disconnected;
                    return Err("设备连接在收到 ok/error 前断开".to_string());
                }
                Err(_) => continue,
            }
        }
    }

    pub fn emergency_stop(&mut self) -> Result<(), String> {
        self.transport
            .write_realtime(RealtimeCommand::EmergencyStop)?;
        self.state = DeviceState::Alarm;
        self.sd_job_active = false;
        self.auto_report_ms = 0;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), String> {
        self.cancel_with_sd_monitoring(self.sd_job_active)
    }

    fn cancel_with_sd_monitoring(&mut self, monitoring_sd: bool) -> Result<(), String> {
        // 1. 立即下发 '!' 进给保持
        let hold = self.transport.write_realtime(RealtimeCommand::FeedHold);
        // 2. 轮询 '?' 等待停稳，并在 0x18 清空队列前保留最后一份状态报告。
        let start = Instant::now();
        let timeout = Duration::from_millis(500);
        let mut pre_reset_report = None;
        let stopped = hold.and_then(|()| {
            loop {
                if start.elapsed() >= timeout {
                    break if monitoring_sd {
                        self.state = DeviceState::Lost;
                        Err("用户取消期间未能确认 SD 作业停稳，执行结果未知".to_owned())
                    } else {
                        Ok(())
                    };
                }
                if let Err(error) = self.transport.write_realtime(RealtimeCommand::StatusQuery) {
                    break Err(error);
                }
                match self.transport.read_line() {
                    Ok(Some(line)) => {
                        let parsed = if monitoring_sd
                            || line.starts_with("ALARM:")
                            || is_reset_banner(&line)
                        {
                            match self.classify_line(&line, "等待取消停稳") {
                                Ok(report) => report,
                                Err(error) => break Err(error),
                            }
                        } else {
                            self.last_response_at = Instant::now();
                            parse_status_report(&line)
                        };
                        if let Some(report) = parsed {
                            if !monitoring_sd && let Some(mpos) = report.mpos {
                                self.last_mpos = Some(mpos);
                            }
                            let alarm = report.activity == MachineActivity::Alarm;
                            let at_rest = report.state == "Hold:0"
                                || (report.activity == MachineActivity::Idle
                                    && !report.has_sd_job());
                            pre_reset_report = Some(report);
                            if monitoring_sd && alarm {
                                self.state = DeviceState::Alarm;
                                break Err(format!("控制器在等待 SD 取消停稳时报告报警：{line}"));
                            }
                            if at_rest {
                                break Ok(());
                            }
                        }
                        if !monitoring_sd && (line.contains("Hold:0") || line.contains("Alarm")) {
                            break Ok(());
                        }
                    }
                    Ok(None) => {
                        self.state = DeviceState::Disconnected;
                        break Err(if monitoring_sd {
                            "用户取消期间设备连接断开，SD 作业状态未知".to_owned()
                        } else {
                            "用户取消期间设备连接断开，作业状态未知".to_owned()
                        });
                    }
                    _ => {}
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        self.last_cancel_report = pre_reset_report;
        // 即使停稳过程中读到故障，也尽力写复位并保留原故障。
        let reset = self
            .transport
            .write_realtime(RealtimeCommand::EmergencyStop);
        self.auto_report_ms = 0;
        if let Err(error) = stopped.and(reset) {
            // 保留明确的报警或断线状态，其他故障也必须阻断后续顺序指令。
            if matches!(self.state, DeviceState::Ready | DeviceState::Hold) {
                self.state = DeviceState::Lost;
            }
            return Err(error);
        }
        self.state = DeviceState::Alarm;
        self.sd_job_active = false;
        Ok(())
    }

    /// 解锁 `$X`：等 `ok`，再等到 Idle。FluidNC 在 `$X` 后会执行 `macros.after_unlock`
    /// （可能含运动），所以 ok 之后不能立刻当作就绪；`Pn:` 含 E/F 时固件会拒绝解锁。
    pub fn unlock(&mut self) -> Result<(), String> {
        self.unlock_cancellable(None)
    }

    pub fn unlock_cancellable(&mut self, cancel: Option<&CancellationToken>) -> Result<(), String> {
        self.recover_with_cancellable("$X", "解锁", Duration::from_secs(15), cancel)
    }

    /// 回零 `$H`：`Home` 状态视为忙碌持续等待；完成后还会执行 `macros.after_homing`。
    pub fn home(&mut self) -> Result<(), String> {
        self.home_cancellable(None)
    }

    pub fn home_cancellable(&mut self, cancel: Option<&CancellationToken>) -> Result<(), String> {
        self.recover_with_cancellable("$H", "回零", Duration::from_secs(120), cancel)
    }

    fn recover_with_cancellable(
        &mut self,
        command: &str,
        label: &str,
        settle_timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err(format!("用户取消，未执行{label}"));
        }
        if self.sd_job_active {
            return Err(format!(
                "{label}拒绝执行：控制器正在执行 SD 脱机作业，请先急停"
            ));
        }
        self.state = DeviceState::Ready;
        if let Err(e) = self.send_line_wait_ack_cancellable(command, cancel) {
            if self.state == DeviceState::Ready {
                self.state = DeviceState::Alarm;
            }
            return Err(format!("{label} {command} 未被固件接受：{e}"));
        }
        let has_macro = self.features.as_ref().is_some_and(|f| match command {
            "$X" => f.has_after_unlock_macro,
            "$H" => f.has_after_homing_macro,
            _ => false,
        });
        let timeout = if has_macro {
            settle_timeout * 2
        } else {
            settle_timeout
        };
        self.wait_until_idle(timeout, cancel).map_err(|e| {
            if has_macro {
                format!("{label}后固件自动宏尚未结束或出错：{e}")
            } else {
                format!("{label}后控制器未回到 Idle：{e}")
            }
        })
    }
}

/// 从拼接后的 `$File/ShowHash` JSON（`{"signature":{"algorithm":"SHA2-256","value":"<HEX>"},...}`）
/// 取出 64 位十六进制哈希并转小写；没有或长度不对时为 None。
fn sha256_from_hash_json(json: &str) -> Option<String> {
    let after = &json[json.find("\"value\"")? + "\"value\"".len()..];
    let after = after
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let value: String = after.chars().take_while(|c| *c != '"').collect();
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests;
