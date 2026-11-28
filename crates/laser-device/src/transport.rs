use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(doc)]
use crate::DeviceConnection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeCommand {
    EmergencyStop,
    FeedHold,
    Resume,
    JogCancel,
    StatusQuery,
    // 进给速度倍率控制 (Feed Override)
    FeedOvrReset,       // 0x90: 恢复 100%
    FeedOvrCoarsePlus,  // 0x91: +10%
    FeedOvrCoarseMinus, // 0x92: -10%
    FeedOvrFinePlus,    // 0x93: +1%
    FeedOvrFineMinus,   // 0x94: -1%
    // 激光/主轴功率倍率控制 (Spindle/Laser Power Override)
    SpindleOvrReset,       // 0x99: 恢复 100%
    SpindleOvrCoarsePlus,  // 0x9A: +10%
    SpindleOvrCoarseMinus, // 0x9B: -10%
    SpindleOvrFinePlus,    // 0x9C: +1%
    SpindleOvrFineMinus,   // 0x9D: -1%
    // 空程快速移动倍率控制 (Rapid Override)
    RapidOvrReset,  // 0x95: 恢复 100%
    RapidOvrMedium, // 0x96: 50%
    RapidOvrLow,    // 0x97: 25%
    // 辅助吹气实时控制 (Air Assist Toggle)
    AirAssistToggle, // 0xA0: 实时切换辅助吹气
}

impl RealtimeCommand {
    pub const fn byte(self) -> u8 {
        match self {
            Self::EmergencyStop => 0x18,
            Self::FeedHold => b'!',
            Self::Resume => b'~',
            Self::JogCancel => 0x85,
            Self::StatusQuery => b'?',
            Self::FeedOvrReset => 0x90,
            Self::FeedOvrCoarsePlus => 0x91,
            Self::FeedOvrCoarseMinus => 0x92,
            Self::FeedOvrFinePlus => 0x93,
            Self::FeedOvrFineMinus => 0x94,
            Self::SpindleOvrReset => 0x99,
            Self::SpindleOvrCoarsePlus => 0x9A,
            Self::SpindleOvrCoarseMinus => 0x9B,
            Self::SpindleOvrFinePlus => 0x9C,
            Self::SpindleOvrFineMinus => 0x9D,
            Self::RapidOvrReset => 0x95,
            Self::RapidOvrMedium => 0x96,
            Self::RapidOvrLow => 0x97,
            Self::AirAssistToggle => 0xA0,
        }
    }
}

pub trait DeviceTransport: Send + Sync {
    fn write_line(&self, line: &str) -> Result<(), String>;
    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String>;
    fn read_line(&self) -> Result<Option<String>, String>;
    fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
        None
    }
    /// 传输层建议的字符计数窗口上限（字节）。默认取经典 GRBL 1.1 串口可用的 127 字节；
    /// 实际窗口还要经 [`DeviceConnection::effective_rx_window`] 按设备身份与 `Bf:` 观测收紧。
    fn default_rx_buffer_size(&self) -> usize {
        GRBL_CLASSIC_RX_WINDOW
    }
    /// 当前传输通道是否启用了空跑强制策略（不出光、限速）；开启时由底层与流控层同步执行。
    fn dry_run_policy(&self) -> Option<crate::DryRunPolicy> {
        None
    }
}

/// 经典 GRBL 1.1 串口 RX 环形缓冲 `RX_BUFFER_SIZE = 128`，环形结构实际只能存 127 字节；
/// 未确认设备容量前，字符计数窗口不得超过它（2026-11-05 修订）。
pub const GRBL_CLASSIC_RX_WINDOW: usize = 127;
/// 已通过 `$CD` 确认是 FluidNC、但尚未观测到空闲 `Bf:` 时的网络初始窗口：FluidNC `Channel` 通用 256 字节。
pub const FLUIDNC_CHANNEL_RX_WINDOW: usize = 256;
/// FluidNC 每个通道同一时刻只处理一行，其余字节进行队列，上限 `4 * maxLine` = 1020 字节；
/// 新的一行开始时队列已满就整行丢弃且不回错误（FluidNC `Channel.h` `_queue_limit`、
/// `Channel.cpp` `queue_push`）。Telnet 空闲时 `Bf:` 却报约 1200，不能照单全收（2026-11-09 核对源码）。
pub const FLUIDNC_LINE_QUEUE_LIMIT: usize = 4 * 255;
/// 字符计数窗口硬上限（字节），即使设备报告更大的空闲缓冲也不超过它：
/// 取 FluidNC 行队列上限再留 4 字节余量，未确认字节永远到不了整行丢弃的边界。
pub const MAX_RX_WINDOW: usize = FLUIDNC_LINE_QUEUE_LIMIT - 4;
/// 半行续接缓冲上限：设备持续输出不带换行的数据时丢弃，避免无界增长。
const MAX_PARTIAL_LINE_BYTES: usize = 64 * 1024;
/// 限制连续拼接半行的时间；单次阻塞读取仍受传输层读超时约束。
const LINE_READ_BUDGET: Duration = Duration::from_millis(500);

/// 跨读超时保留半行的行读取器（2026-11-05 修订）。
///
/// `BufRead::read_line` 在读超时返回错误时，已从内部缓冲取出的字节只留在调用方传入的缓冲里；
/// 若每次调用都新建局部 `String`，Wi-Fi 抖动把一条 `ok` 拆成 `o` + 超时 + `k\n` 时前半截就丢了，
/// 流控的未确认计数永远对不上。这里把半行保存在读取器内，下次调用继续拼接。
pub struct LineReader<R> {
    inner: BufReader<R>,
    partial: Vec<u8>,
    dropping_oversized_line: bool,
}

impl<R: Read> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            partial: Vec::new(),
            dropping_oversized_line: false,
        }
    }

    /// `Ok(Some(line))`：完整一行（只去行尾 CR/LF，保留行首缩进）；`Ok(None)`：EOF；
    /// `Err`：读超时或单行超限丢弃错误，合法半行保留到下次调用。
    ///
    /// 2026-11-18：改用 `fill_buf` + `consume` 逐块限长，避免无 `\n` 洪流先写满内存或卡死上层截止计时。
    pub fn read_line(&mut self) -> Result<Option<String>, String> {
        self.read_line_with_budget(LINE_READ_BUDGET)
    }

    fn read_line_with_budget(&mut self, budget: Duration) -> Result<Option<String>, String> {
        let started = Instant::now();
        loop {
            if started.elapsed() >= budget {
                return Err("读取设备响应超时，已保留半行状态".into());
            }
            let chunk = match std::io::BufRead::fill_buf(&mut self.inner) {
                Ok(buf) => buf,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.to_string()),
            };
            if chunk.is_empty() {
                if self.dropping_oversized_line {
                    self.dropping_oversized_line = false;
                    return Err(format!(
                        "设备输出超过 {MAX_PARTIAL_LINE_BYTES} 字节仍无换行，已丢弃"
                    ));
                }
                if self.partial.is_empty() {
                    return Ok(None);
                }
                // EOF 前最后一段没有换行：作为最后一行交出，下次调用再报 EOF
                return Ok(Some(Self::take_line(&mut self.partial)));
            }
            let newline_pos = chunk.iter().position(|&b| b == b'\n');
            if self.dropping_oversized_line {
                match newline_pos {
                    Some(pos) => {
                        std::io::BufRead::consume(&mut self.inner, pos + 1);
                        self.dropping_oversized_line = false;
                        continue;
                    }
                    None => {
                        let len = chunk.len();
                        std::io::BufRead::consume(&mut self.inner, len);
                        return Err(format!(
                            "设备输出超过 {MAX_PARTIAL_LINE_BYTES} 字节仍无换行，已丢弃"
                        ));
                    }
                }
            }
            match newline_pos {
                Some(pos) => {
                    let take = pos + 1;
                    if self.partial.len().saturating_add(take) > MAX_PARTIAL_LINE_BYTES {
                        std::io::BufRead::consume(&mut self.inner, take);
                        self.partial.clear();
                        return Err(format!(
                            "设备输出单行超过 {MAX_PARTIAL_LINE_BYTES} 字节，已丢弃"
                        ));
                    }
                    self.partial.extend_from_slice(&chunk[..take]);
                    std::io::BufRead::consume(&mut self.inner, take);
                    return Ok(Some(Self::take_line(&mut self.partial)));
                }
                None => {
                    let len = chunk.len();
                    if self.partial.len().saturating_add(len) > MAX_PARTIAL_LINE_BYTES {
                        std::io::BufRead::consume(&mut self.inner, len);
                        self.partial.clear();
                        self.dropping_oversized_line = true;
                        return Err(format!(
                            "设备输出超过 {MAX_PARTIAL_LINE_BYTES} 字节仍无换行，已丢弃"
                        ));
                    }
                    self.partial.extend_from_slice(chunk);
                    std::io::BufRead::consume(&mut self.inner, len);
                    continue;
                }
            }
        }
    }

    fn take_line(partial: &mut Vec<u8>) -> String {
        let bytes = std::mem::take(partial);
        // 只去行尾 CR/LF：$CD 返回的 YAML 依赖行首缩进表达层级，不能 trim 行首
        String::from_utf8_lossy(&bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string()
    }
}

pub struct TcpGrbl {
    reader: Mutex<LineReader<TcpStream>>,
    writer: Mutex<TcpStream>,
    realtime_writer: Mutex<TcpStream>,
    raw_abort_stream: Option<Arc<TcpStream>>,
}

impl TcpGrbl {
    pub fn connect(address: &str) -> Result<Self, String> {
        let stream = TcpStream::connect(address).map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        Self::from_stream(stream)
    }

    pub fn from_stream(stream: TcpStream) -> Result<Self, String> {
        let reader = stream.try_clone().map_err(|e| e.to_string())?;
        // 读取端显式设置超时，避免静默 peer 让 read_line 无限期阻塞（心跳失联兜底前提）
        let _ = reader.set_read_timeout(Some(Duration::from_millis(500)));
        let realtime = stream.try_clone().map_err(|e| e.to_string())?;
        let raw_abort = stream.try_clone().ok().map(Arc::new);
        Ok(Self {
            reader: Mutex::new(LineReader::new(reader)),
            writer: Mutex::new(stream),
            realtime_writer: Mutex::new(realtime),
            raw_abort_stream: raw_abort,
        })
    }
}

/// 控制器在字节流任意位置（包括 `;` 注释内）都会立即执行实时单字节，所以顺序信道按整行检查。
fn contains_realtime_byte(line: &str) -> bool {
    line.bytes().any(
        |byte| matches!(byte, 0x18 | b'!' | b'~' | 0x85 | b'?' | 0x90..=0x97 | 0x99..=0x9D | 0xA0),
    )
}

impl DeviceTransport for TcpGrbl {
    fn write_line(&self, line: &str) -> Result<(), String> {
        if contains_realtime_byte(line) {
            return Err("实时单字节禁止混入顺序 G-code 信道".into());
        }
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "TCP writer lock poisoned".to_string())?;
        writer
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        if command == RealtimeCommand::EmergencyStop
            && let Some(raw) = self.raw_abort_stream.as_ref()
        {
            let mut s = raw.as_ref();
            if s.write_all(&[0x18]).is_ok() {
                return Ok(());
            }
        }
        // Direct non-queuing send via dedicated realtime writer stream
        let mut writer = if command == RealtimeCommand::EmergencyStop {
            self.realtime_writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        } else {
            self.realtime_writer
                .lock()
                .map_err(|_| "TCP realtime writer lock poisoned".to_string())?
        };
        writer
            .write_all(&[command.byte()])
            .map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    fn read_line(&self) -> Result<Option<String>, String> {
        self.reader
            .lock()
            .map_err(|_| "TCP reader lock poisoned".to_string())?
            .read_line()
    }

    fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
        let stream = self.raw_abort_stream.clone()?;
        Some(Box::new(move || {
            let mut s = stream.as_ref();
            s.write_all(&[0x18]).map_err(|e| e.to_string())
        }))
    }

    fn default_rx_buffer_size(&self) -> usize {
        MAX_RX_WINDOW
    }
}

impl DeviceTransport for Box<dyn DeviceTransport> {
    fn write_line(&self, line: &str) -> Result<(), String> {
        (**self).write_line(line)
    }
    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        (**self).write_realtime(command)
    }
    fn read_line(&self) -> Result<Option<String>, String> {
        (**self).read_line()
    }
    fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
        (**self).raw_abort_handle()
    }
    fn default_rx_buffer_size(&self) -> usize {
        (**self).default_rx_buffer_size()
    }
    fn dry_run_policy(&self) -> Option<crate::DryRunPolicy> {
        (**self).dry_run_policy()
    }
}

impl DeviceTransport for Arc<dyn DeviceTransport> {
    fn write_line(&self, line: &str) -> Result<(), String> {
        (**self).write_line(line)
    }
    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        (**self).write_realtime(command)
    }
    fn read_line(&self) -> Result<Option<String>, String> {
        (**self).read_line()
    }
    fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
        (**self).raw_abort_handle()
    }
    fn default_rx_buffer_size(&self) -> usize {
        (**self).default_rx_buffer_size()
    }
    fn dry_run_policy(&self) -> Option<crate::DryRunPolicy> {
        (**self).dry_run_policy()
    }
}

/// 串口 GRBL 传输：与 `TcpGrbl` 对等，顺序 G-code 写入与实时单字节写入使用独立的串口句柄克隆，
/// 保证背压时 `!` / `0x18` 不会被顺序写锁排挤。
pub struct SerialGrbl {
    reader: Mutex<LineReader<Box<dyn serialport::SerialPort>>>,
    writer: Mutex<Box<dyn serialport::SerialPort>>,
    realtime_writer: Mutex<Box<dyn serialport::SerialPort>>,
    raw_abort_port: Option<Arc<Mutex<Box<dyn serialport::SerialPort>>>>,
}

impl SerialGrbl {
    pub fn open(port_name: &str, baud_rate: u32) -> Result<Self, String> {
        if port_name.trim().is_empty() {
            return Err("串口名称不能为空".into());
        }
        let port = serialport::new(port_name, baud_rate)
            .timeout(Duration::from_millis(500))
            .open()
            .map_err(|e| format!("打开串口 {port_name} 失败: {e}"))?;
        let reader = port.try_clone().map_err(|e| e.to_string())?;
        let realtime = port.try_clone().map_err(|e| e.to_string())?;
        let raw_abort = port.try_clone().ok().map(|p| Arc::new(Mutex::new(p)));
        // GRBL 上电/打开串口后会输出欢迎行，短暂等待后清空输入缓冲
        std::thread::sleep(Duration::from_millis(150));
        let _ = port.clear(serialport::ClearBuffer::Input);
        Ok(Self {
            reader: Mutex::new(LineReader::new(reader)),
            writer: Mutex::new(port),
            realtime_writer: Mutex::new(realtime),
            raw_abort_port: raw_abort,
        })
    }
}

impl DeviceTransport for SerialGrbl {
    fn write_line(&self, line: &str) -> Result<(), String> {
        if contains_realtime_byte(line) {
            return Err("实时单字节禁止混入顺序 G-code 信道".into());
        }
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "串口 writer lock poisoned".to_string())?;
        writer
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        if command == RealtimeCommand::EmergencyStop
            && let Some(raw) = self.raw_abort_port.as_ref()
        {
            let mut guard = raw.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.write_all(&[0x18]).is_ok() && guard.flush().is_ok() {
                return Ok(());
            }
        }
        let mut writer = if command == RealtimeCommand::EmergencyStop {
            self.realtime_writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        } else {
            self.realtime_writer
                .lock()
                .map_err(|_| "串口 realtime writer lock poisoned".to_string())?
        };
        writer
            .write_all(&[command.byte()])
            .map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())
    }

    fn read_line(&self) -> Result<Option<String>, String> {
        self.reader
            .lock()
            .map_err(|_| "串口 reader lock poisoned".to_string())?
            .read_line()
    }

    fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
        let port = self.raw_abort_port.clone()?;
        Some(Box::new(move || {
            let mut guard = match port.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err("串口急停句柄忙".to_string());
                }
            };
            guard.write_all(&[0x18]).map_err(|e| e.to_string())?;
            guard.flush().map_err(|e| e.to_string())
        }))
    }

    fn default_rx_buffer_size(&self) -> usize {
        GRBL_CLASSIC_RX_WINDOW
    }
}

/// 设备连接描述：由 HTTP 层从用户填写的连接参数构造。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionSpec {
    Tcp { host: String, port: u16 },
    Serial { port: String, baud_rate: u32 },
}

impl ConnectionSpec {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Tcp { .. } => "network",
            Self::Serial { .. } => "serial",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Tcp { host, port } if host.contains(':') => format!("[{host}]:{port}"),
            Self::Tcp { host, port } => format!("{host}:{port}"),
            Self::Serial { port, baud_rate } => format!("{port}@{baud_rate}"),
        }
    }

    pub fn matches_target(&self, recorded: &str) -> bool {
        // 旧版将 IPv6 地址与端口直接拼接。只接受当前明确目标的同一旧格式，不猜端口边界。
        if let Self::Tcp { host, port } = self
            && host.contains(':')
            && recorded
                .trim()
                .eq_ignore_ascii_case(&format!("{host}:{port}"))
        {
            return true;
        }
        let Some(parsed) = normalize_connection_target(recorded) else {
            return false;
        };
        match (self, parsed) {
            (
                Self::Tcp { host, port },
                NormalizedTarget::Tcp {
                    host: rec_host,
                    port: rec_port,
                },
            ) => host.eq_ignore_ascii_case(&rec_host) && *port == rec_port,
            (
                Self::Serial { port, baud_rate },
                NormalizedTarget::Serial {
                    port: rec_port,
                    baud_rate: rec_baud,
                },
            ) => {
                normalize_serial_port(port) == rec_port && rec_baud.is_none_or(|b| b == *baud_rate)
            }
            _ => false,
        }
    }
}

/// 解析历史或规范设备标识（如 `127.0.0.1:23`、`tcp://127.0.0.1:23`、`COM3@115200`、`serial:COM3:115200`）
/// 为结构化目标，无法识别或非法格式返回 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedTarget {
    Tcp {
        host: String,
        port: u16,
    },
    Serial {
        port: String,
        baud_rate: Option<u32>,
    },
}

fn normalize_serial_port(port: &str) -> String {
    let trimmed = port.trim();
    if is_windows_com_port(trimmed) {
        return format!("COM{}", &trimmed[3..]);
    }
    trimmed.to_owned()
}

fn is_windows_com_port(raw: &str) -> bool {
    raw.len() >= 4
        && raw
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("com"))
        && raw[3..].bytes().all(|b| b.is_ascii_digit())
}

fn parse_tcp_host_port(raw: &str) -> Option<(String, u16)> {
    let body = raw.trim().trim_end_matches('/');
    if body.is_empty() || body.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some(rest) = body.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        if after.is_empty() {
            return Some((host.to_ascii_lowercase(), 23));
        }
        let port = after
            .strip_prefix(':')?
            .parse::<u16>()
            .ok()
            .filter(|&p| p > 0)?;
        return Some((host.to_ascii_lowercase(), port));
    }
    if let Some((host, port_str)) = body.rsplit_once(':') {
        if host.is_empty() || host.contains(':') || host.contains('/') {
            return None;
        }
        let port = port_str.parse::<u16>().ok().filter(|&p| p > 0)?;
        return Some((host.to_ascii_lowercase(), port));
    }
    if body.contains('/') {
        return None;
    }
    Some((body.to_ascii_lowercase(), 23))
}

pub fn normalize_connection_target(raw: &str) -> Option<NormalizedTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("tcp://") {
        let (host, port) = parse_tcp_host_port(&trimmed[6..])?;
        return Some(NormalizedTarget::Tcp { host, port });
    }
    if lower.starts_with("serial://") || lower.starts_with("serial:") {
        let rest = if lower.starts_with("serial://") {
            trimmed[9..].trim()
        } else {
            trimmed[7..].trim()
        };
        if rest.is_empty() {
            return None;
        }
        if let Some((port_part, baud_part)) =
            rest.rsplit_once('@').or_else(|| rest.rsplit_once(':'))
        {
            let baud = baud_part
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|b| (1200..=4_000_000).contains(b))?;
            let port = normalize_serial_port(port_part);
            if port.is_empty() {
                return None;
            }
            return Some(NormalizedTarget::Serial {
                port,
                baud_rate: Some(baud),
            });
        }
        return Some(NormalizedTarget::Serial {
            port: normalize_serial_port(rest),
            baud_rate: None,
        });
    }
    if lower.contains("://") {
        return None;
    }
    if let Some((port_part, baud_part)) = trimmed.rsplit_once('@') {
        let baud = baud_part
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|b| (1200..=4_000_000).contains(b))?;
        let port = normalize_serial_port(port_part);
        if port.is_empty() {
            return None;
        }
        return Some(NormalizedTarget::Serial {
            port,
            baud_rate: Some(baud),
        });
    }
    if is_windows_com_port(trimmed) || trimmed.starts_with('/') {
        return Some(NormalizedTarget::Serial {
            port: normalize_serial_port(trimmed),
            baud_rate: None,
        });
    }
    let (host, port) = parse_tcp_host_port(trimmed)?;
    Some(NormalizedTarget::Tcp { host, port })
}

/// 按连接描述打开传输层（TCP 使用带超时的连接；串口按指定波特率打开）。
pub fn open_transport(
    spec: &ConnectionSpec,
    connect_timeout: Duration,
) -> Result<Box<dyn DeviceTransport>, String> {
    match spec {
        ConnectionSpec::Tcp { host, port } => {
            let addrs = (host.as_str(), *port)
                .to_socket_addrs()
                .map_err(|e| format!("解析地址 {host}:{port} 失败: {e}"))?;
            let mut last_error = format!("地址 {host}:{port} 无法解析");
            for addr in addrs {
                match TcpStream::connect_timeout(&addr, connect_timeout) {
                    Ok(stream) => {
                        stream.set_nodelay(true).map_err(|e| e.to_string())?;
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                        return Ok(Box::new(TcpGrbl::from_stream(stream)?));
                    }
                    Err(e) => last_error = format!("连接 {addr} 失败: {e}"),
                }
            }
            Err(last_error)
        }
        ConnectionSpec::Serial { port, baud_rate } => {
            Ok(Box::new(SerialGrbl::open(port, *baud_rate)?))
        }
    }
}

#[derive(Clone)]
pub struct FakeGrbl {
    lines: Arc<Mutex<Vec<String>>>,
    realtime: Arc<Mutex<Vec<u8>>>,
    realtime_events: Arc<Mutex<Vec<(u8, Instant)>>>,
    pub(crate) responses: Arc<Mutex<VecDeque<String>>>,
    artificial_backpressure: Arc<Mutex<Option<Duration>>>,
}

impl FakeGrbl {
    pub fn new(responses: impl IntoIterator<Item = String>) -> Self {
        Self {
            lines: Arc::new(Mutex::new(Vec::new())),
            realtime: Arc::new(Mutex::new(Vec::new())),
            realtime_events: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            artificial_backpressure: Arc::new(Mutex::new(None)),
        }
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    pub fn realtime(&self) -> Vec<u8> {
        self.realtime.lock().unwrap().clone()
    }

    pub fn realtime_events(&self) -> Vec<(u8, Instant)> {
        self.realtime_events.lock().unwrap().clone()
    }

    pub fn set_backpressure(&self, delay: Duration) {
        *self.artificial_backpressure.lock().unwrap() = Some(delay);
    }
}

impl DeviceTransport for FakeGrbl {
    fn write_line(&self, line: &str) -> Result<(), String> {
        if contains_realtime_byte(line) {
            return Err("实时单字节禁止混入顺序 G-code 信道".into());
        }
        let delay = *self.artificial_backpressure.lock().unwrap();
        if let Some(d) = delay {
            std::thread::sleep(d);
        }
        self.lines.lock().unwrap().push(line.to_string());
        Ok(())
    }

    fn write_realtime(&self, command: RealtimeCommand) -> Result<(), String> {
        // Preemptive channel: separate lock, never blocked by lines lock
        self.realtime.lock().unwrap().push(command.byte());
        self.realtime_events
            .lock()
            .unwrap()
            .push((command.byte(), Instant::now()));
        Ok(())
    }

    fn read_line(&self) -> Result<Option<String>, String> {
        Ok(self.responses.lock().unwrap().pop_front())
    }

    fn raw_abort_handle(&self) -> Option<Box<dyn Fn() -> Result<(), String> + Send + Sync>> {
        let rt = self.realtime.clone();
        let ev = self.realtime_events.clone();
        Some(Box::new(move || {
            rt.lock().unwrap().push(0x18);
            ev.lock().unwrap().push((0x18, Instant::now()));
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_bytes_in_comments_are_rejected_on_sequential_channel() {
        for line in ["G0 X10 ; check!", "G1 X1 ;?", "M5 ; resume ~"] {
            assert!(contains_realtime_byte(line), "{line}");
        }
        for line in ["G0 X10 ; plain comment", "$$", "$RI=200"] {
            assert!(!contains_realtime_byte(line), "{line}");
        }
    }

    #[test]
    fn tcp_realtime_write_bypasses_blocked_sequential_writer() {
        use std::io::Read;
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut first = [0u8; 1];
            socket.read_exact(&mut first).unwrap();
            first[0]
        });
        let grbl = Arc::new(TcpGrbl::connect(&address.to_string()).unwrap());
        // 模拟顺序流因下位机背压长期持有顺序写锁
        let held_sequential_writer = grbl.writer.lock().unwrap();
        let realtime = grbl.clone();
        let started = Instant::now();
        let abort =
            std::thread::spawn(move || realtime.write_realtime(RealtimeCommand::EmergencyStop));
        assert!(abort.join().unwrap().is_ok());
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "0x18 must not wait on the sequential writer lock"
        );
        drop(held_sequential_writer);
        assert_eq!(server.join().unwrap(), 0x18);
    }

    #[test]
    fn tcp_read_line_times_out_instead_of_blocking_forever() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            // 静默 peer：保持连接但永不回应
            std::thread::sleep(Duration::from_millis(1500));
            drop(socket);
        });
        let grbl = TcpGrbl::connect(&address.to_string()).unwrap();
        let started = Instant::now();
        let result = grbl.read_line();
        assert!(
            result.is_err(),
            "silent peer must surface a timeout error, got {result:?}"
        );
        assert!(started.elapsed() < Duration::from_millis(1200));
        server.join().unwrap();
    }

    #[test]
    fn raw_abort_handle_executes_synchronously() {
        let fake = FakeGrbl::new(["$32=1".to_string()]);
        let handle = fake.raw_abort_handle().unwrap();
        handle().unwrap();
        assert_eq!(fake.realtime(), vec![0x18]);
    }

    // ---- 2026-11-05：读超时半行续接 ----

    /// 按脚本逐段吐出字节或 IO 错误，脚本耗尽后 EOF。
    struct ScriptedReader(VecDeque<Result<&'static [u8], std::io::ErrorKind>>);
    impl Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.pop_front() {
                None => Ok(0),
                Some(Err(kind)) => Err(std::io::Error::new(kind, "scripted timeout")),
                Some(Ok(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(bytes);
                    Ok(bytes.len())
                }
            }
        }
    }

    #[test]
    fn line_reader_bounds_slow_partial_line_and_keeps_it_for_next_read() {
        struct DelayedChunks(VecDeque<(Duration, &'static [u8])>);

        impl Read for DelayedChunks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let Some((delay, chunk)) = self.0.pop_front() else {
                    return Ok(0);
                };
                std::thread::sleep(delay);
                buf[..chunk.len()].copy_from_slice(chunk);
                Ok(chunk.len())
            }
        }

        let mut reader = LineReader::new(DelayedChunks(VecDeque::from([
            (Duration::from_millis(80), b"o".as_slice()),
            (Duration::ZERO, b"k\n".as_slice()),
        ])));
        assert!(
            reader
                .read_line_with_budget(Duration::from_millis(30))
                .is_err()
        );
        assert_eq!(reader.partial, b"o");
        assert_eq!(reader.read_line().unwrap().as_deref(), Some("ok"));
    }

    #[test]
    fn line_reader_keeps_partial_line_across_read_timeout() {
        use std::io::ErrorKind::{TimedOut, WouldBlock};
        let script = [
            Ok(&b"o"[..]),
            Err(TimedOut),
            Ok(&b"k\r\n<Idle|Bf:15,"[..]),
            Err(WouldBlock),
            Ok(&b"128>\n  indented: 1\ntail"[..]),
        ];
        let mut reader = LineReader::new(ScriptedReader(script.into_iter().collect()));
        assert!(reader.read_line().is_err(), "第一次读到半行后超时应报错");
        assert_eq!(reader.read_line().unwrap().as_deref(), Some("ok"));
        assert!(reader.read_line().is_err());
        assert_eq!(
            reader.read_line().unwrap().as_deref(),
            Some("<Idle|Bf:15,128>")
        );
        assert_eq!(
            reader.read_line().unwrap().as_deref(),
            Some("  indented: 1")
        );
        // EOF 前无换行的尾巴作为最后一行交出，随后报 EOF
        assert_eq!(reader.read_line().unwrap().as_deref(), Some("tail"));
        assert_eq!(reader.read_line().unwrap(), None);
    }

    #[test]
    fn tcp_read_line_completes_ok_split_by_read_timeout() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.write_all(b"o").unwrap();
            socket.flush().unwrap();
            // 间隔超过 500ms 读超时，模拟 Wi-Fi 抖动把 ok 拆成两段
            std::thread::sleep(Duration::from_millis(800));
            socket.write_all(b"k\r\n").unwrap();
            socket.flush().unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        let grbl = TcpGrbl::connect(&address.to_string()).unwrap();
        assert!(
            grbl.read_line().is_err(),
            "半行之后的读超时应报错而不是交出半行"
        );
        let mut completed = None;
        for _ in 0..4 {
            match grbl.read_line() {
                Ok(line) => {
                    completed = line;
                    break;
                }
                Err(_) => continue,
            }
        }
        assert_eq!(completed.as_deref(), Some("ok"));
        server.join().unwrap();
    }

    #[test]
    fn line_reader_drops_unbounded_no_newline_flood_and_recovers_on_next_newline() {
        struct EndlessFloodReader {
            remaining: usize,
            tail: &'static [u8],
            tail_pos: usize,
        }
        impl Read for EndlessFloodReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.remaining > 0 {
                    let n = buf.len().min(self.remaining);
                    buf[..n].fill(b'A');
                    self.remaining -= n;
                    return Ok(n);
                }
                if self.tail_pos < self.tail.len() {
                    let n = buf.len().min(self.tail.len() - self.tail_pos);
                    buf[..n].copy_from_slice(&self.tail[self.tail_pos..self.tail_pos + n]);
                    self.tail_pos += n;
                    return Ok(n);
                }
                Ok(0)
            }
        }

        // 1. 超过 64 KiB 无换行且无超时：必须立即报错返回而非无限攒内存或阻塞，并在换行后恢复读取下一行
        let flood = EndlessFloodReader {
            remaining: MAX_PARTIAL_LINE_BYTES + 20_000,
            tail: b"\nok\r\n",
            tail_pos: 0,
        };
        let mut reader = LineReader::new(flood);
        let first_err = reader.read_line().unwrap_err();
        assert!(
            first_err.contains("字节"),
            "超限无换行输入必须立即报错丢弃: {first_err}"
        );
        assert!(reader.partial.is_empty(), "超限丢弃后半行缓存必须清空");
        // 丢弃完残余垃圾直到 '\n' 后，下一行 ok 必须完整读出
        loop {
            match reader.read_line() {
                Ok(line) => {
                    assert_eq!(line.as_deref(), Some("ok"));
                    break;
                }
                Err(err) => assert!(err.contains("已丢弃"), "{err}"),
            }
        }
        assert_eq!(reader.read_line().unwrap(), None);

        // 2. 上层截止计时测试：无限无换行洪流不能卡死 handshake / 设备读取循环
        struct InfiniteNoNewline;
        impl Read for InfiniteNoNewline {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf.fill(b'X');
                Ok(buf.len())
            }
        }
        let mut flood_reader = LineReader::new(InfiniteNoNewline);
        let err = flood_reader.read_line().unwrap_err();
        assert!(err.contains("已丢弃"), "{err}");
    }

    #[test]
    fn ipv6_target_description_round_trips_for_resume() {
        let spec = ConnectionSpec::Tcp {
            host: "2001:db8::1".into(),
            port: 2301,
        };
        assert_eq!(spec.describe(), "[2001:db8::1]:2301");
        assert!(spec.matches_target(&spec.describe()));
        assert!(spec.matches_target("2001:db8::1:2301"));
        assert!(!spec.matches_target("[2001:db8::1]:2302"));
        assert!(!spec.matches_target("[2001:db8::2]:2301"));
    }

    #[test]
    fn normalize_and_match_target_covers_tcp_serial_and_rejects_mismatches() {
        let tcp = ConnectionSpec::Tcp {
            host: "127.0.0.1".into(),
            port: 23,
        };
        assert!(tcp.matches_target("127.0.0.1:23"));
        assert!(tcp.matches_target("tcp://127.0.0.1:23"));
        assert!(tcp.matches_target("TCP://127.0.0.1"));
        assert!(!tcp.matches_target("127.0.0.1:24"));
        assert!(!tcp.matches_target("192.168.1.99:23"));
        assert!(!tcp.matches_target("COM3@115200"));
        assert!(!tcp.matches_target("invalid://target"));
        assert!(!tcp.matches_target(""));

        let serial = ConnectionSpec::Serial {
            port: "COM3".into(),
            baud_rate: 115_200,
        };
        assert!(serial.matches_target("COM3@115200"));
        assert!(serial.matches_target("com3@115200"));
        assert!(serial.matches_target("serial:COM3:115200"));
        assert!(serial.matches_target("COM3"));
        assert!(!serial.matches_target("COM3@9600"));
        assert!(!serial.matches_target("COM4@115200"));
        assert!(!serial.matches_target("127.0.0.1:23"));
    }
}
