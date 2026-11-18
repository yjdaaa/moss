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
    pub total_lines: AtomicUsize,
    pub updated_at_millis: AtomicU64,
    pub awaiting_completion: AtomicBool,
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
            let mut num_str = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_ascii_digit() || nc == '.' {
                    num_str.push(chars.next().unwrap());
                } else {
                    break;
                }
            }
            if !num_str.is_empty() {
                if let Ok(val) = num_str.parse::<f64>() {
                    let rescaled = (val * scale_factor).round() as i64;
                    result.push(c);
                    result.push_str(&rescaled.to_string());
                    continue;
                } else {
                    result.push(c);
                    result.push_str(&num_str);
                    continue;
                }
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
        // FluidNC 可主动周期上报状态（$RI），替代 500ms 心跳轮询；旧 GRBL 返回 error:3 时自动关闭。
        // 必须在首行 G-code 之前完成并消费掉它的 ok，否则会与行 ack 计数错位；无法确认时直接报错不发送。
        let auto_report = match self.device.enable_auto_report(200) {
            Ok(enabled) => enabled,
            Err(e) => {
                self.device.disable_auto_report();
                return Err(e);
            }
        };
        let result = self.stream_lines_inner(lines, rx_buffer_size);
        if auto_report {
            self.device.disable_auto_report();
        }
        result
    }

    fn stream_lines_inner(
        &mut self,
        lines: impl IntoIterator<Item = String>,
        rx_buffer_size: usize,
    ) -> Result<usize, String> {
        let mut lines_iter = lines.into_iter().peekable();
        let mut in_flight: VecDeque<usize> = VecDeque::new();
        let mut chars_in_buffer: usize = 0;
        let mut window = self.device.rx_window(rx_buffer_size);
        let mut acked_count: usize = 0;
        let mut heartbeat_sent = false;

        while lines_iter.peek().is_some() || !in_flight.is_empty() {
            // 1. 检查取消令牌
            if self.cancel.is_cancelled() {
                self.device.cancel()?;
                return Ok(acked_count);
            }

            // 2. 填充 RX 缓冲区：只要缓冲区还有空间且有待发行，就持续写入
            while let Some(raw_line) = lines_iter.peek() {
                let line = if (self.scale_factor - 1.0).abs() > 1e-6 {
                    rescale_gcode_line_s(raw_line, self.scale_factor)
                } else {
                    raw_line.clone()
                };
                let line_len = line.len() + 1; // +1 表示尾随换行符 '\n'
                // 如果单行就超过 current_rx_buffer_size，在缓冲区完全排空时单独发送该行
                if chars_in_buffer + line_len <= window.size()
                    || (chars_in_buffer == 0 && in_flight.is_empty())
                {
                    let _ = lines_iter.next();
                    if let Err(e) = self.device.send_line(&line) {
                        if self.cancel.is_cancelled() {
                            self.device.cancel()?;
                            return Ok(acked_count);
                        }
                        return Err(e);
                    }
                    chars_in_buffer += line_len;
                    in_flight.push_back(line_len);
                } else {
                    break;
                }
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
                    acked_count += 1;
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
        // G4 P0 同步 FluidNC/GRBL 规划器缓冲区
        let completion = self
            .device
            .send_line_wait_ack_cancellable("G4 P0", Some(&self.cancel))
            .and_then(|()| {
                self.device
                    .wait_until_idle(Duration::from_secs(30), Some(&self.cancel))
            });
        if self.cancel.is_cancelled() {
            self.device.cancel()?;
            return Ok(acked_count);
        }
        completion?;
        Ok(acked_count)
    }
}

#[cfg(test)]
mod tests;
