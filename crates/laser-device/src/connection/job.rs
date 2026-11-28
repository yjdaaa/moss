use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{DeviceConnection, DeviceState};
use crate::{DeviceTransport, RealtimeCommand};

#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Default)]
pub struct JobProgress {
    pub lines_sent: AtomicUsize,
    /// 经规划器状态或空闲回检确认实际完成执行的行数（不高于 `lines_sent`，避免 `ok` 入队未刻行在续雕时被跳过）。
    pub lines_completed: AtomicUsize,
    pub total_lines: AtomicUsize,
    pub updated_at_millis: AtomicU64,
    pub awaiting_completion: AtomicBool,
    /// 短作业已登记急停目标，握手和预检尚未结束。
    pub preflighting: AtomicBool,
    pub last_status_report: std::sync::Mutex<Option<String>>,
    /// 走边框被暂停（进给保持 `!`）还没继续：只用于界面显示（2026-11-18）
    pub paused: AtomicBool,
}

impl JobProgress {
    pub fn record_status_report(&self, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.last_status_report.lock() {
            *guard = Some(trimmed.to_string());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.updated_at_millis.store(now, Ordering::Relaxed);
    }

    pub fn status_report(&self) -> Option<String> {
        self.last_status_report
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }
}

/// 根据目标功率标尺对 G-code 行中的 S 参数进行动态等比缩放。
/// 例如当设备标尺为 255（标准 1000）时，scale_factor = 255.0 / 1000.0 = 0.255。
/// "M4 S1000" -> "M4 S255", "G1 X10 Y10 S500" -> "G1 X10 Y10 S128"。
/// 紧凑写法 "G1X10S500" 同样识别（S 紧跟在上一个字的数值之后）；`$` 开头的系统指令原样保留。
pub fn rescale_gcode_line_s(line: &str, scale_factor: f64) -> String {
    if (scale_factor - 1.0).abs() < 1e-6
        || scale_factor <= 0.0
        || line.trim_start().starts_with('$')
    {
        return line.to_string();
    }
    let mut result = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        // S 是 G-code 字地址：位于行首、空白之后或上一个字的数值之后（紧凑写法）
        let at_word_start = result
            .chars()
            .next_back()
            .is_none_or(|prev| prev.is_whitespace() || prev.is_ascii_digit() || prev == '.');
        if (c == 'S' || c == 's') && at_word_start {
            // GRBL / FluidNC 解析前会剥空白并允许前导 `+`（如 `M4 S 500`、`G1 X10 S+500`）
            let mut lookahead = chars.clone();
            while let Some(&nc) = lookahead.peek() {
                if nc == ' ' || nc == '\t' {
                    lookahead.next();
                } else {
                    break;
                }
            }
            if matches!(lookahead.peek(), Some('+')) {
                lookahead.next();
            }
            let mut num_str = String::new();
            while let Some(&nc) = lookahead.peek() {
                if nc.is_ascii_digit() || nc == '.' {
                    num_str.push(lookahead.next().unwrap());
                } else {
                    break;
                }
            }
            if !num_str.is_empty()
                && let Ok(val) = num_str.parse::<f64>()
                && val.is_finite()
            {
                chars = lookahead;
                let rescaled = (val * scale_factor).round() as i64;
                result.push(c);
                result.push_str(&rescaled.to_string());
                continue;
            }
        }
        result.push(c);
    }
    result
}

pub struct JobWorker<T: DeviceTransport> {
    pub device: DeviceConnection<T>,
    pub cancel: CancellationToken,
    pub progress: Arc<JobProgress>,
    pub scale_factor: f64,
}

impl<T: DeviceTransport> JobWorker<T> {
    pub fn new(device: DeviceConnection<T>, cancel: CancellationToken) -> Self {
        Self::with_progress(device, cancel, Arc::new(JobProgress::default()))
    }

    pub fn with_progress(
        mut device: DeviceConnection<T>,
        cancel: CancellationToken,
        progress: Arc<JobProgress>,
    ) -> Self {
        device.progress_sink = Some(progress.clone());
        Self {
            device,
            cancel,
            progress,
            scale_factor: 1.0,
        }
    }

    pub fn with_scale_factor(mut self, scale_factor: f64) -> Self {
        if scale_factor > 0.0 && scale_factor.is_finite() {
            self.scale_factor = scale_factor;
        }
        self
    }

    /// 发送 G-code 行序列。采用 GRBL/FluidNC 字符计数流控（Character-Counting Streaming Flow Control）：
    /// 未确认字节总数不超过窗口；窗口规则见 [`DeviceConnection::effective_rx_window`]（2026-11-05 修订：
    /// 未知设备从 127 字节起步，只按 `Bf:` 观测到的容量调整，不再套用 255 字节下限）。
    /// 收到 ACK 时释放对应行字符配额并推进进度。
    pub fn send_lines(&mut self, lines: impl IntoIterator<Item = String>) -> Result<usize, String> {
        let transport_default = self.device.transport.default_rx_buffer_size();
        let guess = self.device.rx_window_guess(transport_default);
        self.stream_lines_with_buffer_size(lines, guess)
    }

    /// `rx_buffer_size` 是尚未观测到设备容量时的初始窗口；一旦在空闲时观测到 `Bf:`，以设备报告为准。
    pub fn stream_lines_with_buffer_size(
        &mut self,
        lines: impl IntoIterator<Item = String>,
        rx_buffer_size: usize,
    ) -> Result<usize, String> {
        if self.cancel_if_requested()? {
            return Ok(0);
        }
        // FluidNC 可主动周期上报状态（$RI），替代 500ms 心跳轮询；旧 GRBL 返回 error:3 时自动关闭。
        // 必须在首行 G-code 之前完成并消费掉它的 ok，否则会与行 ack 计数错位；无法确认时直接报错不发送。
        let auto_report = match self
            .device
            .enable_auto_report_cancellable(200, Some(&self.cancel))
        {
            Ok(enabled) => enabled,
            Err(e) => {
                if e.starts_with("用户取消，") {
                    self.finalize_auto_report_or_cancel(true)?;
                    return Ok(0);
                }
                let _ = self.finalize_auto_report_or_cancel(true);
                return Err(e);
            }
        };
        let mut acked_count = 0;
        match self.stream_lines_inner(lines, rx_buffer_size, &mut acked_count) {
            Ok(true) => Ok(acked_count),
            Err(error) => {
                // 报警、复位或断线优先于同时到达的取消，收尾不能把真实故障改成“已取消”。
                let _ = self.finalize_auto_report_or_cancel(auto_report);
                Err(error)
            }
            Ok(false) => {
                self.finalize_auto_report_or_cancel(auto_report)?;
                Ok(acked_count)
            }
        }
    }

    /// 检查取消位；若已取消则走实时急停收尾并返回 `Ok(true)`。
    fn cancel_if_requested(&mut self) -> Result<bool, String> {
        if self.cancel.is_cancelled() {
            self.device.cancel()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// 收尾检查：若已取消则走实时急停，不再写 `$RI=0`；否则按需关闭自动上报并在期间遇取消时急停。
    fn finalize_auto_report_or_cancel(
        &mut self,
        disable_auto_report: bool,
    ) -> Result<bool, String> {
        if self.cancel_if_requested()? {
            return Ok(true);
        }
        if disable_auto_report
            && !self
                .device
                .disable_auto_report_cancellable(Some(&self.cancel))
        {
            self.device.cancel()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// 检查取消位；若已取消则执行实时急停并按控制器 `Hold:0|Ln` 证据记录已完成行数，返回 `Ok(true)`。
    fn cancel_with_proven_completion(
        &mut self,
        use_numbered_wire: bool,
        acked_count: usize,
    ) -> Result<bool, String> {
        if self.cancel_if_requested()? {
            self.record_cancelled_completion(use_numbered_wire, acked_count);
            return Ok(true);
        }
        Ok(false)
    }

    /// 返回 `Ok(true)` 表示已在内部因取消走完 `device.cancel()`，外层不得再写 `$RI=0`。
    fn stream_lines_inner(
        &mut self,
        lines: impl IntoIterator<Item = String>,
        rx_buffer_size: usize,
        acked_count: &mut usize,
    ) -> Result<bool, String> {
        self.progress.lines_completed.store(0, Ordering::Relaxed);
        let dry_run = self.device.transport.dry_run_policy();
        if let Some(policy) = dry_run {
            // LB2 空跑模式：任务开头先同步发送关光与限速初始化行（不占 N1..Nk 行号，避免影响断点续雕计数）
            let preamble = policy.preamble_line();
            let acked = self
                .device
                .send_line_wait_ack_or_cancel(&preamble, Some(&self.cancel))?;
            if !acked {
                self.cancel_with_proven_completion(false, *acked_count)?;
                return Ok(true);
            }
        }
        let prepared_lines: Vec<String> = lines
            .into_iter()
            .map(|raw| {
                let scaled = if (self.scale_factor - 1.0).abs() > 1e-6 {
                    rescale_gcode_line_s(&raw, self.scale_factor)
                } else {
                    raw
                };
                match dry_run {
                    // 漏网的禁止指令保留原文交给 DryRunTransport::write_line 替换成注释并触发告警回调
                    Some(policy)
                        if crate::detect_dry_run_blocked_line_for_transport(&scaled).is_none() =>
                    {
                        policy.rewrite_line(&scaled)
                    }
                    _ => scaled,
                }
            })
            .collect();
        let use_numbered_wire = self
            .device
            .features
            .as_ref()
            .is_some_and(|f| f.use_line_numbers)
            && !prepared_lines.is_empty()
            && prepared_lines.iter().enumerate().all(|(idx, line)| {
                let trimmed = line.trim_start();
                !trimmed.is_empty()
                    && !trimmed.starts_with('$')
                    && !trimmed.starts_with(';')
                    && !trimmed.starts_with(['N', 'n'])
                    && format!("N{} {line}", idx + 1).len() <= crate::MAX_GCODE_LINE_BYTES
            });

        let mut lines_iter = prepared_lines.into_iter().enumerate().peekable();
        let mut in_flight: VecDeque<usize> = VecDeque::new();
        let mut chars_in_buffer: usize = 0;
        let mut window = self.device.rx_window(rx_buffer_size);
        let mut heartbeat_sent = false;

        while lines_iter.peek().is_some() || !in_flight.is_empty() {
            // 1. 检查取消令牌
            if self.cancel_with_proven_completion(use_numbered_wire, *acked_count)? {
                return Ok(true);
            }

            // 2. 填充 RX 缓冲区：只要缓冲区还有空间且有待发行，就持续写入；每次写入前都重查取消令牌
            while let Some((idx, raw_line)) = lines_iter.peek() {
                if self.cancel_with_proven_completion(use_numbered_wire, *acked_count)? {
                    return Ok(true);
                }
                let line = if use_numbered_wire {
                    format!("N{} {raw_line}", idx + 1)
                } else {
                    raw_line.clone()
                };
                let effective_len = match dry_run {
                    Some(policy) => line.len().max(policy.rewrite_line(&line).len()),
                    None => line.len(),
                };
                let line_len = effective_len + 1; // +1 表示尾随换行符 '\n'
                // 如果单行就超过 current_rx_buffer_size，在缓冲区完全排空时单独发送该行
                if chars_in_buffer + line_len <= window.size()
                    || (chars_in_buffer == 0 && in_flight.is_empty())
                {
                    // 在途顺序写成功返回后可能已触发取消，下一笔写前必须复查取消位
                    if self.cancel_with_proven_completion(use_numbered_wire, *acked_count)? {
                        return Ok(true);
                    }
                    let _ = lines_iter.next();
                    // 传输 I/O 失败不得被并发取消吞并改写为“已取消”；外层错误分支仍会尽力补发 0x18
                    self.device.send_line(&line)?;
                    chars_in_buffer += line_len;
                    in_flight.push_back(line_len);
                } else {
                    break;
                }
            }

            // 末行或满窗退出内层后也立刻复查，避免取消后继续发心跳或阻塞读响应
            if self.cancel_with_proven_completion(use_numbered_wire, *acked_count)? {
                return Ok(true);
            }

            // 3. 监控失联超时并发送心跳
            let silent = self.device.last_response_at.elapsed();
            if silent > Duration::from_millis(500) {
                if heartbeat_sent {
                    self.device.state = DeviceState::Lost;
                    return Err("控制器连续 500ms 无响应，已判定失联，急停无法送达".to_string());
                }
                self.device
                    .transport
                    .write_realtime(RealtimeCommand::StatusQuery)?;
                heartbeat_sent = true;
                self.device.last_response_at = Instant::now();
            }

            // 4. 读取下位机响应，消费 ok / 错误 / 状态报告
            match self.device.transport.read_line() {
                Ok(Some(response)) if response.eq_ignore_ascii_case("ok") => {
                    self.device.last_response_at = Instant::now();
                    heartbeat_sent = false;
                    if let Some(len) = in_flight.pop_front() {
                        chars_in_buffer = chars_in_buffer.saturating_sub(len);
                    }
                    *acked_count += 1;
                    self.progress.lines_sent.fetch_add(1, Ordering::Relaxed);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    self.progress
                        .updated_at_millis
                        .store(now, Ordering::Relaxed);
                }
                Ok(Some(response)) => {
                    // 说明行 / 复位横幅 / ALARM / error / 状态报告统一分类；出错即中止
                    let report = self.device.classify_line(&response, "流式发送")?;
                    heartbeat_sent = false;
                    if let Some(report) = report {
                        // |Bf:blocks,rx_free 是此刻剩余字节：只用来估计容量（见 RxWindow），绝不当作窗口下限
                        if let Some(free) = report.rx_buffer_free {
                            let idle = in_flight.is_empty();
                            window.observe(free, idle);
                            if idle {
                                self.device.record_idle_rx_free(&report);
                            }
                        }
                        let blocking = report.blocking_pins();
                        if !blocking.is_empty() {
                            self.device.state = DeviceState::Alarm;
                            return Err(format!(
                                "发送中检测到阻断引脚触发：{}（{}）",
                                blocking.join("、"),
                                report.raw
                            ));
                        }
                    }
                    continue;
                }
                Ok(None) => {
                    self.device.state = DeviceState::Disconnected;
                    return Err("设备连接在收到 ok/error 前断开".to_string());
                }
                Err(_) => {
                    // 读取超时，继续循环以检查取消令牌和心跳
                    continue;
                }
            }
        }

        // 5. 全部文件内容发送并 ACK 完成后，进入执行完成等待阶段
        self.progress
            .awaiting_completion
            .store(true, Ordering::Release);
        // G4 P0 同步 FluidNC/GRBL 规划器缓冲区；报警/复位/断线/超时优先于并发取消，绝不吞掉真实故障
        let g4_acked = self
            .device
            .send_line_wait_ack_or_cancel("G4 P0", Some(&self.cancel))?;
        if !g4_acked {
            self.cancel_with_proven_completion(use_numbered_wire, *acked_count)?;
            return Ok(true);
        }
        let idle_ok = self
            .device
            .wait_until_idle_or_cancel(Duration::from_secs(30), Some(&self.cancel))?;
        if !idle_ok {
            self.cancel_with_proven_completion(use_numbered_wire, *acked_count)?;
            return Ok(true);
        }
        if self.cancel_with_proven_completion(use_numbered_wire, *acked_count)? {
            return Ok(true);
        }
        self.progress
            .lines_completed
            .store(*acked_count, Ordering::Relaxed);
        Ok(false)
    }

    /// 取消停机后，只采信固件在 `Hold:0` 静止状态直接汇报且已加顺序 `N` 行号的 `Ln:N` 边界；
    /// 禁止用 ACK、Bf 占用数、异步 Idle 或任何估算推断已完成行数，不能证实则一律记 0（上层 `can_resume=false`）。
    fn record_cancelled_completion(&self, use_numbered_wire: bool, acked_count: usize) {
        let completed = self.device_proven_completed_count(use_numbered_wire, acked_count);
        self.progress
            .lines_completed
            .store(completed, Ordering::Relaxed);
    }

    fn device_proven_completed_count(&self, use_numbered_wire: bool, acked_count: usize) -> usize {
        if !use_numbered_wire || acked_count == 0 {
            return 0;
        }
        let Some(report) = self.device.last_cancel_report.as_ref() else {
            return 0;
        };
        if !report.blocking_pins().is_empty()
            || report.has_sd_job()
            || report.activity != crate::MachineActivity::Held
            || !report.raw.contains("Hold:0")
        {
            return 0;
        }
        let Some(ln) = report.line_number else {
            return 0;
        };
        if ln == 0 || ln > acked_count {
            return 0;
        }
        ln - 1
    }
}

#[cfg(test)]
mod tests;
