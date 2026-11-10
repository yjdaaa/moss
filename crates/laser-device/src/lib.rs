use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Disconnected,
    Ready,
    Hold,
    Alarm,
    Lost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPreflight {
    pub status: String,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
    pub gcode_path: Option<std::path::PathBuf>,
    pub workflow_snapshot_valid: bool,
    pub calibration_sha_valid: bool,
}

pub fn authorize_send(preflight: &SendPreflight) -> Result<(), String> {
    if preflight.status != "pending" {
        return Err("job 不处于 pending，拒绝发送".into());
    }
    if preflight.expected_sha256.len() != 64
        || !preflight
            .expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("缺少有效的 expected_gcode_sha256，拒绝发送".into());
    }

    let actual_sha = if let Some(path) = &preflight.gcode_path {
        if !path.is_file() {
            return Err("G-code 文件不存在，拒绝发送".into());
        }
        let bytes = std::fs::read(path).map_err(|e| format!("读取 G-code 文件失败: {e}"))?;
        hex::encode(Sha256::digest(&bytes))
    } else if let Some(sha) = &preflight.actual_sha256 {
        sha.clone()
    } else {
        return Err("缺少待校验的 G-code 来源，拒绝发送".into());
    };

    if !preflight.expected_sha256.eq_ignore_ascii_case(&actual_sha) {
        return Err("G-code SHA-256 与发送前重算值不一致，拒绝发送".into());
    }
    if !preflight.workflow_snapshot_valid && !preflight.calibration_sha_valid {
        return Err("缺少有效 workflow 快照或 calibration SHA，拒绝发送".into());
    }
    Ok(())
}

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

/// 跨读超时保留半行的行读取器（2026-11-05 修订）。
///
/// `BufRead::read_line` 在读超时返回错误时，已从内部缓冲取出的字节只留在调用方传入的缓冲里；
/// 若每次调用都新建局部 `String`，Wi-Fi 抖动把一条 `ok` 拆成 `o` + 超时 + `k\n` 时前半截就丢了，
/// 流控的未确认计数永远对不上。这里把半行保存在读取器内，下次调用继续拼接。
pub struct LineReader<R> {
    inner: BufReader<R>,
    partial: Vec<u8>,
}

impl<R: Read> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            partial: Vec::new(),
        }
    }

    /// `Ok(Some(line))`：完整一行（只去行尾 CR/LF，保留行首缩进）；`Ok(None)`：EOF；
    /// `Err`：读超时等错误，已读到的半行保留到下次调用。
    pub fn read_line(&mut self) -> Result<Option<String>, String> {
        loop {
            match self.inner.read_until(b'\n', &mut self.partial) {
                Ok(0) => {
                    if self.partial.is_empty() {
                        return Ok(None);
                    }
                    // EOF 前最后一段没有换行：作为最后一行交出，下次调用再报 EOF
                    return Ok(Some(Self::take_line(&mut self.partial)));
                }
                Ok(_) if self.partial.last() == Some(&b'\n') => {
                    return Ok(Some(Self::take_line(&mut self.partial)));
                }
                // 未遇到换行就返回说明已到 EOF，下一轮读到 0 字节时交出
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if self.partial.len() > MAX_PARTIAL_LINE_BYTES {
                        self.partial.clear();
                        return Err(format!(
                            "设备输出超过 {MAX_PARTIAL_LINE_BYTES} 字节仍无换行，已丢弃: {e}"
                        ));
                    }
                    return Err(e.to_string());
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
        // Direct non-queuing send via dedicated realtime writer stream
        let mut writer = self
            .realtime_writer
            .lock()
            .map_err(|_| "TCP realtime writer lock poisoned".to_string())?;
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
        let mut writer = self
            .realtime_writer
            .lock()
            .map_err(|_| "串口 realtime writer lock poisoned".to_string())?;
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
            let mut guard = port
                .lock()
                .map_err(|_| "串口急停句柄 lock poisoned".to_string())?;
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
            Self::Tcp { host, port } => format!("{host}:{port}"),
            Self::Serial { port, baud_rate } => format!("{port}@{baud_rate}"),
        }
    }
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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MachineLimits {
    pub x_travel_mm: Option<f64>,
    pub y_travel_mm: Option<f64>,
    pub z_travel_mm: Option<f64>,
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
        raw: line.trim().to_string(),
    };
    for field in fields {
        if let Some(pins) = field.strip_prefix("Pn:") {
            report.pins = pins.trim().to_string();
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
            current_axis = trimmed.trim_end_matches(':').trim();
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
            if key == "max_travel_mm" {
                if let Ok(num) = val.parse::<f64>() {
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

/// 从标准 GRBL $$ 设置行中提取 X/Y 行程（$130 为 X 行程，$131 为 Y 行程）
pub fn parse_grbl_settings_limits(settings: &[String]) -> MachineLimits {
    let mut limits = MachineLimits::default();
    for line in settings {
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

/// 握手探测报告：只发 `$$` 读取固件设置，不移动轴、不出光。
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeReport {
    pub mode: &'static str,
    pub target: String,
    pub connected: bool,
    pub laser_mode_ok: bool,
    pub firmware_settings: Vec<String>,
    pub machine_limits: Option<MachineLimits>,
    pub features: Option<MachineFeatures>,
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
                summary: format!("{label} 连接失败: {error}"),
            };
        }
    };
    let mut device = DeviceConnection::new(transport);
    match device.handshake_with_deadline(timeout) {
        Ok(settings) => {
            let fallback_limits = parse_grbl_settings_limits(&settings);
            // FluidNC：再读 $CD 拿 YAML 配置（宏、急停引脚、planner_blocks 等）；旧 GRBL 回 error:3 时静默跳过
            let (yaml_limits, features) = match device.fetch_fluidnc_config(timeout) {
                Ok((limits, features)) => {
                    device.features = Some(features.clone());
                    (Some(limits), Some(features))
                }
                Err(_) => (None, None),
            };
            let limits = match yaml_limits {
                Some(l) if l.x_travel_mm.is_some() || l.y_travel_mm.is_some() => Some(l),
                _ if fallback_limits.x_travel_mm.is_some()
                    || fallback_limits.y_travel_mm.is_some() =>
                {
                    Some(fallback_limits)
                }
                _ => None,
            };
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
                summary: if alarm {
                    format!("已连接 {target}，但 {error}")
                } else {
                    format!("{target} 无 GRBL 响应: {error}")
                },
            }
        }
    }
}

#[derive(Clone)]
pub struct FakeGrbl {
    lines: Arc<Mutex<Vec<String>>>,
    realtime: Arc<Mutex<Vec<u8>>>,
    realtime_events: Arc<Mutex<Vec<(u8, Instant)>>>,
    responses: Arc<Mutex<VecDeque<String>>>,
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
    /// 机器 X/Y 行程：握手 `$$` 的 `$130/$131`，FluidNC `$CD` 的 `max_travel_mm` 优先。
    /// 发送前用来复核任务范围（2026-11-06）；都没有时为空，调用方跳过该项检查。
    pub travel_limits: MachineLimits,
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
    pub fn handshake_with_deadline(&mut self, deadline: Duration) -> Result<Vec<String>, String> {
        self.state = DeviceState::Disconnected;
        self.max_spindle_speed = None;
        self.report_status_mask = None;
        self.rx_capacity_observed = None;
        self.travel_limits = MachineLimits::default();
        self.transport.write_line("$$")?;
        let start = Instant::now();
        let mut laser_mode = None;
        let mut settings = Vec::new();
        let mut got_any = false;
        let mut terminated = false;
        loop {
            if start.elapsed() > deadline {
                break;
            }
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    got_any = true;
                    if self.note_line(&line) {
                        continue;
                    }
                    if line.starts_with('$') {
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
                        terminated = true;
                        break;
                    }
                    if line.starts_with("error:") || line.starts_with("ALARM:") {
                        self.state = DeviceState::Alarm;
                        return Err(self.explain(&line));
                    }
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        if laser_mode != Some(true) {
            if got_any {
                self.state = DeviceState::Alarm;
                return Err(
                    "$32=1 固件激光模式门禁未通过，控制器已进入 Alarm 态，拒绝发送".to_string(),
                );
            }
            self.state = DeviceState::Disconnected;
            return Err(format!(
                "{}ms 内未收到 GRBL 固件设置响应（若机器正在执行 SD 脱机作业，固件会丢弃其他通道的顺序指令，请先急停）",
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

    /// 握手后读取 FluidNC `$CD` 配置并缓存到 `features`。非 FluidNC（`error:3`）、超时或空响应时
    /// 保持 `None` 且不报错；返回是否读到了配置。
    pub fn load_features(&mut self, timeout: Duration) -> bool {
        match self.fetch_fluidnc_config(timeout) {
            Ok((limits, features)) => {
                if limits.x_travel_mm.is_some() || limits.y_travel_mm.is_some() {
                    self.travel_limits = limits;
                }
                self.features = Some(features);
                true
            }
            Err(_) => false,
        }
    }

    /// 开启 FluidNC 本通道自动状态上报（`$RI=<ms>`，最小 50ms）。仅在已确认是 FluidNC
    /// （`features` 已加载）时发送；旧 GRBL 会返回 `error:3`，此时静默关闭该功能。
    /// 必须在流式发送开始前调用并消费掉它的 `ok`，避免 ack 计数错位。
    ///
    /// 2026-11-05 修订：`$RI` 的应答在 2 秒内未到时不再直接放行（迟到的 `ok` 会被当成首行 G-code 的 ack），
    /// 而是补发 `G4 P0`（不运动、不出光）同步，按顺序应答规则在限定时间内收齐 `$RI` 与 `G4 P0`
    /// 两个终止应答后才放行；收不齐则置 `Lost` 并返回错误，调用方不得开始流式发送。
    pub fn enable_auto_report(&mut self, interval_ms: u32) -> Result<bool, String> {
        if self.features.is_none() {
            return Ok(false);
        }
        let interval_ms = interval_ms.max(50);
        self.transport.write_line(&format!("$RI={interval_ms}"))?;
        let mut acks = Vec::new();
        self.collect_terminal_acks(&mut acks, 1, AUTO_REPORT_ACK_TIMEOUT)?;
        if acks.is_empty() {
            self.transport.write_line("G4 P0")?;
            self.collect_terminal_acks(&mut acks, 2, AUTO_REPORT_RESYNC_TIMEOUT)?;
            if acks.len() < 2 {
                // 无法判断收到的应答属于哪条命令：保守地认为自动上报可能已开启，交给调用方 `$RI=0` 收尾
                self.auto_report_ms = interval_ms;
                self.state = DeviceState::Lost;
                return Err(format!(
                    "$RI 自动上报设置在 {}ms 内未确认，G4 P0 同步也未在 {}ms 内收齐应答，ack 计数无法对齐；未开始发送任何 G-code",
                    AUTO_REPORT_ACK_TIMEOUT.as_millis(),
                    AUTO_REPORT_RESYNC_TIMEOUT.as_millis()
                ));
            }
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
    ) -> Result<(), String> {
        let start = Instant::now();
        while acks.len() < want && start.elapsed() < timeout {
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

    /// 关闭自动上报（`$RI=0`）。尽力而为：任务结束或断开前调用，不关心结果。
    pub fn disable_auto_report(&mut self) {
        if self.auto_report_ms == 0 {
            return;
        }
        self.auto_report_ms = 0;
        if self.transport.write_line("$RI=0").is_ok() {
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(800) {
                match self.transport.read_line() {
                    Ok(Some(line))
                        if line.eq_ignore_ascii_case("ok") || line.starts_with("error:") =>
                    {
                        break;
                    }
                    Ok(Some(_)) => continue,
                    _ => break,
                }
            }
        }
    }

    /// 主动状态轮询的间隔：已开启自动上报时放宽到两倍上报周期作为兜底，否则用调用方给定值。
    fn poll_interval(&self, default: Duration) -> Duration {
        if self.auto_report_ms > 0 {
            Duration::from_millis(u64::from(self.auto_report_ms) * 2).max(default)
        } else {
            default
        }
    }

    /// 自适应功率标尺检查：只要设备 $30 是合法有限正数即通过，并返回设备实际标尺（如 255.0、1000.0 等）。
    pub fn verify_job_ready_adaptive(&mut self, timeout: Duration) -> Result<f64, String> {
        let scale = self
            .max_spindle_speed
            .ok_or_else(|| "固件未报告有效的 $30 功率标尺（缺少 $30 或非正数）".to_string())?;
        self.require_idle(timeout, None)?;
        Ok(scale)
    }

    /// 发送 $CD 读取 FluidNC 的 YAML 配置文件并解析出机械行程与外设引脚
    pub fn fetch_fluidnc_config(
        &mut self,
        timeout: Duration,
    ) -> Result<(MachineLimits, MachineFeatures), String> {
        self.transport.write_line("$CD")?;
        let start = Instant::now();
        let mut yaml_lines = Vec::new();
        let mut terminated = false;
        while start.elapsed() < timeout {
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    self.last_response_at = Instant::now();
                    if line.eq_ignore_ascii_case("ok") {
                        terminated = true;
                        break;
                    }
                    if line.starts_with("error:") {
                        return Err(format!("设备不支持 $CD 或报错: {line}"));
                    }
                    yaml_lines.push(line);
                }
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        if !terminated && yaml_lines.is_empty() {
            return Err("读取 $CD 超时或未响应".into());
        }
        let raw = yaml_lines.join("\n");
        Ok(parse_fluidnc_yaml(&raw))
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
            if let Some(prog) = &self.progress_sink {
                prog.record_status_report(&report.raw);
            }
            return Ok(Some(report));
        }
        Ok(None)
    }

    /// 发送前预检：`?` 一次，要求 Idle、无 SD 作业、无 E/F/D 阻断引脚。
    /// Run/Jog/Home 直接拒绝（不等待），Hold/Door/Alarm/Sleep/Check 均拒绝并说明原因。
    fn require_idle(
        &mut self,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        self.transport
            .write_realtime(RealtimeCommand::StatusQuery)?;
        let start = Instant::now();
        while start.elapsed() < timeout {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err("用户取消，停止等待控制器".into());
            }
            match self.transport.read_line() {
                Ok(Some(line)) => {
                    let report = match self.classify_line(&line, "预检")? {
                        Some(report) => report,
                        None => continue,
                    };
                    // 预检前所有顺序命令都已确认，此刻的 Bf: 剩余值就是 RX 缓冲容量
                    self.record_idle_rx_free(&report);
                    let blocking = report.blocking_pins();
                    if !blocking.is_empty() {
                        self.state = DeviceState::Alarm;
                        return Err(format!(
                            "控制器输入引脚处于触发状态：{}（状态 {}），固件会拒绝解锁与运动，请先复位物理开关",
                            blocking.join("、"),
                            report.raw
                        ));
                    }
                    match report.activity {
                        MachineActivity::Idle if !report.has_sd_job() => {
                            self.state = DeviceState::Ready;
                            return Ok(());
                        }
                        MachineActivity::Idle => {
                            return Err(format!(
                                "控制器正在执行 SD 脱机作业 ({})，拒绝启动新任务",
                                report.raw
                            ));
                        }
                        MachineActivity::Busy => {
                            return Err(format!(
                                "控制器正在运行中 ({})，拒绝启动新任务",
                                report.raw
                            ));
                        }
                        MachineActivity::Held => {
                            self.state = DeviceState::Hold;
                            return Err(format!(
                                "控制器处于进给保持 ({})，请先恢复或急停后再发送",
                                report.raw
                            ));
                        }
                        MachineActivity::DoorOpen => {
                            self.state = DeviceState::Hold;
                            return Err(format!("安全门打开 ({})，请关门后再发送", report.raw));
                        }
                        MachineActivity::Sleep => {
                            self.state = DeviceState::Alarm;
                            return Err(format!("控制器处于休眠 ({})，需要复位唤醒", report.raw));
                        }
                        MachineActivity::Check => {
                            self.state = DeviceState::Alarm;
                            return Err(format!(
                                "控制器处于 G-code 检查模式 ({})，请先退出检查模式",
                                report.raw
                            ));
                        }
                        MachineActivity::Alarm | MachineActivity::Unknown => {
                            self.state = DeviceState::Alarm;
                            return Err(format!(
                                "控制器尚未安全就绪：{}，需要人工核验",
                                report.raw
                            ));
                        }
                    }
                }
                Ok(None) => {
                    self.state = DeviceState::Disconnected;
                    return Err("等待 Idle 时连接断开，执行结果未知".into());
                }
                Err(_) => continue,
            }
        }
        self.state = DeviceState::Lost;
        Err("未收到 Idle 状态，执行结果未知；禁止自动重发".into())
    }

    /// 发送文件全部内容后，等待控制器清空规划器缓冲区并恢复 Idle 状态。
    /// Run / Jog / Home（含解锁、回零后的自动宏）持续等待；Hold / Door 也等待（用户可能恢复），
    /// Alarm / Sleep / Check 立即失败；支持取消与超时。
    pub fn wait_until_idle(
        &mut self,
        timeout: Duration,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        let start = Instant::now();
        let mut last_poll = Instant::now();
        let interval = self.poll_interval(Duration::from_millis(200));
        self.transport
            .write_realtime(RealtimeCommand::StatusQuery)?;
        while start.elapsed() < timeout {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err("用户取消，停止等待控制器".into());
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
                            return Ok(());
                        }
                        MachineActivity::Idle | MachineActivity::Busy => {
                            std::thread::sleep(Duration::from_millis(10));
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
                    if line.starts_with("error:") || line.starts_with("ALARM:") {
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
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        if got_hash.is_none() {
            got_hash = sha256_from_hash_json(&json);
        }
        if !terminated && got_hash.is_none() {
            return Err("读取固件文件哈希超时或未响应".into());
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
        self.send_line(&format!("$SD/Run={remote_path}"))?;
        self.last_response_at = Instant::now();
        self.wait_sd_job(timeout, progress, cancel, true)
    }

    fn wait_sd_job(
        &mut self,
        timeout: Duration,
        progress: Option<&Arc<JobProgress>>,
        cancel: Option<&CancellationToken>,
        require_start: bool,
    ) -> Result<(), String> {
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
                self.cancel()?;
                break Err("用户取消，已停止 SD 脱机任务".into());
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
                            break Ok(());
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

    fn send_line_wait_ack_cancellable(
        &mut self,
        line: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), String> {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("用户取消，停止发送".into());
        }
        self.send_line(line)?;
        self.last_response_at = Instant::now();
        let mut heartbeat_sent = false;
        loop {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                return Err("用户取消，停止等待 ACK".into());
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
                    return Ok(());
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
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), String> {
        // 1. 立即下发 '!' 进给保持
        self.transport.write_realtime(RealtimeCommand::FeedHold)?;
        // 2. 轮询 '?' 状态，等待 Hold:0（硬超时 500ms）
        let start = Instant::now();
        let timeout = Duration::from_millis(500);
        while start.elapsed() < timeout {
            self.transport
                .write_realtime(RealtimeCommand::StatusQuery)?;
            if let Ok(Some(line)) = self.transport.read_line() {
                self.last_response_at = Instant::now();
                if line.contains("Hold:0") || line.contains("Alarm") {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // 3. 发送 0x18 软复位
        self.transport
            .write_realtime(RealtimeCommand::EmergencyStop)?;
        self.state = DeviceState::Alarm;
        self.sd_job_active = false;
        Ok(())
    }

    /// 解锁 `$X`：等 `ok`，再等到 Idle。FluidNC 在 `$X` 后会执行 `macros.after_unlock`
    /// （可能含运动），所以 ok 之后不能立刻当作就绪；`Pn:` 含 E/F 时固件会拒绝解锁。
    pub fn unlock(&mut self) -> Result<(), String> {
        self.recover_with("$X", "解锁", Duration::from_secs(15))
    }

    /// 回零 `$H`：`Home` 状态视为忙碌持续等待；完成后还会执行 `macros.after_homing`。
    pub fn home(&mut self) -> Result<(), String> {
        self.recover_with("$H", "回零", Duration::from_secs(120))
    }

    fn recover_with(
        &mut self,
        command: &str,
        label: &str,
        settle_timeout: Duration,
    ) -> Result<(), String> {
        if self.sd_job_active {
            return Err(format!(
                "{label}拒绝执行：控制器正在执行 SD 脱机作业，请先急停"
            ));
        }
        self.state = DeviceState::Ready;
        if let Err(e) = self.send_line_wait_ack(command) {
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
        self.wait_until_idle(timeout, None).map_err(|e| {
            if has_macro {
                format!("{label}后固件自动宏尚未结束或出错：{e}")
            } else {
                format!("{label}后控制器未回到 Idle：{e}")
            }
        })
    }
}

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

/// 解析 GRBL / FluidNC 的错误、报警与硬件诊断信息，转换为结构化自然语言说明。
pub fn diagnose_device_message(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(msg) = trimmed.strip_prefix("[MSG:ERR:") {
        let err_text = msg.trim_end_matches(']').trim();
        return Some(format!("控制器硬件错误: {err_text}"));
    }
    if let Some(msg) = trimmed.strip_prefix("[MSG:WARN:") {
        let warn_text = msg.trim_end_matches(']').trim();
        return Some(format!("控制器警告: {warn_text}"));
    }
    let upper = trimmed.to_ascii_uppercase();
    if let Some(alarm_part) = upper.strip_prefix("ALARM:") {
        let code = alarm_part
            .split_whitespace()
            .next()
            .unwrap_or(alarm_part)
            .trim();
        let desc = match code {
            "1" => {
                "硬限位开关触发（物理撞机或轴触碰限位，控制器已紧急制动停机，建议重新执行 $H 回零）"
            }
            "2" => "软限位超程（目标运动坐标超出了设置的最大行程，已安全拦截，可执行 $X 解锁）",
            "3" => "运动中控制器复位（可能丢失步进步数，建议重新执行 $H 回零）",
            "4" => "对刀/探针初始状态错误（探针在探测前已处于闭合导通状态）",
            "5" => "对刀/探针未接触（在最大探测行程内未检测到接触信号）",
            "6" => "回零过程被复位中断",
            "7" => "回零过程中安全门打开",
            "8" => "回零脱开限位失败（建议增大 pull-off 距离或检查限位开关接线）",
            "9" => "回零未找到限位开关（未在搜索距离内触发开关，请检查接线或行程）",
            "10" => "双电机双限位脱开失败",
            "11" => {
                "启动引脚电平异常（限位开关或急停按键在上电时已处于触发闭合状态，请检查接线或引脚反相设置）"
            }
            "12" => "回零开关信号歧义（双限位开关触发顺序异常）",
            "13" => "硬停止保护触发（控制器强制断电保护）",
            "14" => "未回零安全锁定（机床启用了强制回零策略，请先执行 $H 回零再开始加工）",
            "15" => "固件初始化配置锁定（配置文件加载异常）",
            "16" => "IO 扩展芯片复位故障",
            "17" => {
                "文件作业中遇到无效 G-code 行，固件已停止作业（FluidNC GCodeError，原因见前一行 [MSG:ERR: Bad GCode: …]）"
            }
            "18" => "对刀过程触碰到了物理硬限位开关",
            _ => "控制器安全锁定，请检查机床状态",
        };
        return Some(format!("报警 ALARM:{code}：{desc}"));
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(error_part) = lower.strip_prefix("error:") {
        let code = error_part
            .split_whitespace()
            .next()
            .unwrap_or(error_part)
            .trim();
        let desc = match code {
            "1" => "G-code 语法错误：缺少字母前缀",
            "2" => "G-code 数值格式无效或缺少预期数值",
            "3" => "未识别或不支持的 '$' 系统命令",
            "4" => "设置值不能为负数",
            "5" => "该设置项在当前固件中已禁用",
            "6" => "步进脉冲宽度过短（最小 3 微秒）",
            "7" => "读取 EEPROM/NVS 设置失败，已恢复默认值",
            "8" => "该命令只能在 Idle 空闲状态执行",
            "9" => "控制器处于报警或锁定状态，拒绝执行移动指令（请先执行 $H 回零或 $X 解锁）",
            "10" => "软限位触发：目标坐标超出行程（请检查图形尺寸与原点）",
            "11" | "14" => "指令行超过 255 字节上限，固件已丢弃超出部分",
            "12" => "超过最大步进速率（进给 F 太高或 steps_per_mm 过大）",
            "13" => "安全门打开，固件拒绝解锁/运动",
            "15" => "点动目标超出机床行程",
            "16" => "点动命令 $J= 格式无效",
            "17" => "激光模式需要 PWM 输出引脚，当前主轴配置不满足",
            "18" => "配置中未定义回零轴，无法执行 $H",
            "19" => "固件不允许单轴回零",
            "20" => "不支持或无效的 G-code 指令",
            "21" => "同一行出现互斥的模态 G 指令（如 G0 与 G1）",
            "22" => "未设置进给速度 F（G1/G2/G3 前需要 F 值）",
            "23" => "G/M 指令值必须是整数",
            "24" => "同一程序段内存在冲突的轴控制指令",
            "25" => "同一行重复出现相同字母参数",
            "26" => "运动指令缺少轴坐标（G1 后无 X/Y）",
            "27" => "行号 N 无效",
            "28" => "指令缺少必需的参数值（如 G4 缺 P）",
            "29" => "不支持的工件坐标系（G59.1+）",
            "30" => "G53 只能与 G0/G1 同行使用",
            "31" => "该指令不允许带轴坐标",
            "32" => "圆弧指令缺少当前平面的轴坐标",
            "33" => "运动目标无效（圆弧端点无法到达）",
            "34" => "圆弧半径 R 无法构成有效圆弧",
            "35" => "圆弧缺少 I/J/K 偏移量",
            "36" => "行内存在未使用的多余参数",
            "37" => "G43.1 动态刀长补偿轴错误",
            "38" => "G-code 数值超出允许最大值",
            "39" => "P 参数超出允许范围",
            "40" => "启动引脚状态异常，拒绝运行",
            "60" => "SD 卡挂载失败（SD 卡未插入、接触不良或格式不支持，请格式化为 FAT32）",
            "61" | "66" => "无法打开或读取 SD 卡中的文件",
            "62" => "无法打开 SD 卡或本地目录",
            "63" => "目录不存在",
            "64" => "文件为空",
            "65" => "指定的文件在 SD 卡或内部 Flash 中不存在",
            "67" => "文件系统正忙（正在进行文件读写或已有脱机任务运行）",
            "68" | "69" => "删除 SD 卡文件或目录失败",
            "70" => "重命名文件失败",
            "80" => "设置数值超出允许范围",
            "81" => "设置字符串或参数无效",
            "82" => "创建文件失败（SD 卡满或只读）",
            "83" => "格式化文件系统失败",
            "90" => "向其他通道转发消息失败",
            "100" | "101" => "写入或读取 NVS 持久化设置失败",
            "110" => "Web 认证失败（密码错误）",
            "111" | "112" | "114" => "行/文件结束或无数据（通常不是错误）",
            "113" => "固件在执行中被复位",
            "120" => {
                "另一个通道正在运行作业（如 SD 卡任务或 WebUI 发起的加工），本连接的 G-code 被拒绝；等它结束或先停止它"
            }
            "130" => "点动 (Jog) 被取消",
            "150" => "引脚配置冲突（YAML 文件中 GPIO 编号重复或引脚不存在）",
            "151" => "运行时设置无效",
            "152" => "YAML 配置文件语法错误",
            "160" => "文件上传失败",
            "161" => "文件下载失败",
            "162" => "只读设置：$30/$32/$100 等在 FluidNC 中是 YAML 的只读代理，请改 config.yaml",
            "170" | "171" | "172" | "173" | "174" | "175" => "宏表达式语法或求值错误",
            "176" | "177" | "178" | "179" => "宏流程控制错误（仅在宏内允许，或内存/栈不足）",
            "180" => "参数赋值失败",
            "181" => "G-code 参数值无效",
            _ => "控制器指令错误",
        };
        return Some(format!("错误 error:{code}：{desc}"));
    }
    None
}

/// 局域网内通过 mDNS 发现的控制器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredController {
    /// 主机名（不含 `.local`），FluidNC 默认 `fluidnc`
    pub hostname: String,
    pub ip: String,
    /// telnet 端口（`_telnet._tcp`），未广播时为 None
    pub telnet_port: Option<u16>,
    /// HTTP/WebUI 端口（`_http._tcp`），未广播时为 None
    pub http_port: Option<u16>,
}

/// 通过 mDNS 浏览 `_telnet._tcp` 与 `_http._tcp` 在指定时长内收集局域网控制器。
/// FluidNC 会把两种服务都注册在 `<hostname>.local`（默认 `fluidnc.local`）。
/// 只读操作，不向任何设备发送命令；没有多播权限或无网卡时返回空列表而不报错。
pub fn discover_controllers(timeout: Duration) -> Vec<DiscoveredController> {
    discover_controllers_with(timeout, |_| {})
}

/// 同 [`discover_controllers`]，每解析出一个地址（或补上一个端口）就立即回调，便于边发现边确认（2026-11-09）。
pub fn discover_controllers_with(
    timeout: Duration,
    mut on_found: impl FnMut(&DiscoveredController),
) -> Vec<DiscoveredController> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    use std::collections::BTreeMap;

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let receivers: Vec<(&str, _)> = ["_telnet._tcp.local.", "_http._tcp.local."]
        .into_iter()
        .filter_map(|svc| daemon.browse(svc).ok().map(|rx| (svc, rx)))
        .collect();
    if receivers.is_empty() {
        let _ = daemon.shutdown();
        return Vec::new();
    }
    let mut found: BTreeMap<(String, String), DiscoveredController> = BTreeMap::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut idle = true;
        for (svc, rx) in &receivers {
            while let Ok(event) = rx.try_recv() {
                idle = false;
                if let ServiceEvent::ServiceResolved(info) = event {
                    let hostname = info
                        .get_hostname()
                        .trim_end_matches('.')
                        .trim_end_matches(".local")
                        .to_string();
                    for addr in info.get_addresses() {
                        let ip = addr.to_string();
                        // IPv6 链路本地地址对上位机 TCP 连接价值不大，优先记录 IPv4
                        if ip.contains(':')
                            && found
                                .keys()
                                .any(|(h, i)| h == &hostname && !i.contains(':'))
                        {
                            continue;
                        }
                        let entry =
                            found
                                .entry((hostname.clone(), ip.clone()))
                                .or_insert_with(|| DiscoveredController {
                                    hostname: hostname.clone(),
                                    ip: ip.clone(),
                                    telnet_port: None,
                                    http_port: None,
                                });
                        if svc.starts_with("_telnet") {
                            entry.telnet_port = Some(info.get_port());
                        } else {
                            entry.http_port = Some(info.get_port());
                        }
                        on_found(entry);
                    }
                }
            }
        }
        if idle {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    for (svc, _) in &receivers {
        let _ = daemon.stop_browse(svc);
    }
    let _ = daemon.shutdown();
    let mut list: Vec<DiscoveredController> = found.into_values().collect();
    // IPv4 排前面；同一主机同时有 IPv4 与 IPv6 时只保留 IPv4
    list.sort_by_key(|c| (c.ip.contains(':'), c.hostname.clone(), c.ip.clone()));
    let hosts_with_v4: std::collections::HashSet<String> = list
        .iter()
        .filter(|c| !c.ip.contains(':'))
        .map(|c| c.hostname.clone())
        .collect();
    list.retain(|c| !c.ip.contains(':') || !hosts_with_v4.contains(&c.hostname));
    list
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

/// 局域网扫描时识别出的控制器信息（2026-11-09，只读识别）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControllerIdentity {
    /// 固件名与版本，如 `FluidNC v3.7.8`、`Grbl 1.1h`；只回了状态报告时为 None
    pub firmware: Option<String>,
    /// 状态报告里的机器状态，如 `Idle`、`Run`、`Alarm`
    pub state: Option<String>,
    /// FluidNC `$I` 的 `[MSG:Machine: <配置名>]`，多台机器时用来区分
    pub machine: Option<String>,
}

/// `FluidNC` 后面紧跟的版本号（`v4.1.1`），去掉结尾的 `:` / `]`。
fn fluidnc_version(text: &str) -> Option<String> {
    let rest = &text[text.find("FluidNC")? + "FluidNC".len()..];
    let version = rest
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches([':', ']']);
    Some(if version.is_empty() {
        "FluidNC".to_owned()
    } else {
        format!("FluidNC {version}")
    })
}

/// `[VER:...]` / 启动横幅 → 可读的固件名（2026-11-09 按 FluidNC 源码格式核对）。
/// - FluidNC 4.x `$I`：`[VER:4.1 FluidNC v4.1.1 (esp32-wifi) :<build>`（没有右括号）；3.x：`[VER:3.7.8 FluidNC v3.7.8:]`
/// - 经典 GRBL：`[VER:1.1h.20190825:]`
/// - 启动横幅：`Grbl 4.1 [FluidNC v4.1.1 (esp32-wifi) '$' for help]`、`Grbl 1.1h ['$' for help]`
fn firmware_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("[VER:") {
        if rest.contains("FluidNC") {
            return fluidnc_version(rest);
        }
        let version = rest.split([':', ' ', ']']).next().unwrap_or("");
        let short = version.split('.').take(2).collect::<Vec<_>>().join(".");
        return Some(format!(
            "Grbl {}",
            if short.is_empty() { version } else { &short }
        ));
    }
    if trimmed.starts_with("Grbl ") || trimmed.starts_with("GrblHAL ") {
        if trimmed.contains("[FluidNC") {
            return fluidnc_version(trimmed);
        }
        return trimmed
            .split(" [")
            .next()
            .map(|banner| banner.trim().to_owned());
    }
    None
}

/// 只读识别 GRBL / FluidNC：发实时状态查询 `?` 与 `$I`（读固件信息），不发任何会让机器运动或出光的命令。
/// 只有回了状态报告 `<State|...>`、`[VER:...]` 或 GRBL 启动横幅才算雕刻机控制器；单回一个 `ok` 不算。
pub fn identify_grbl(
    transport: &dyn DeviceTransport,
    timeout: Duration,
) -> Option<ControllerIdentity> {
    let _ = transport.write_realtime(RealtimeCommand::StatusQuery);
    let _ = transport.write_line("$I");
    let started = Instant::now();
    let deadline = started + timeout;
    // GRBL / FluidNC 回 `?` 只要几毫秒：这么久一行都没有就不是（扫描时不在别的设备上白等满时限）
    let first_reply_deadline = started + timeout.min(Duration::from_millis(700));
    let mut identity = ControllerIdentity::default();
    let mut grbl = false;
    let mut saw_ok = false;
    let mut any_line = false;
    while Instant::now() < deadline {
        if !any_line && Instant::now() >= first_reply_deadline {
            break;
        }
        match transport.read_line() {
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                any_line = true;
                // 还没认出是 GRBL 时先回了别的协议（`login:`、SSH / HTTP 横幅等）：立即判定，不等时限
                let grbl_like = line.starts_with('<')
                    || line.starts_with('[')
                    || line.starts_with("Grbl")
                    || line.eq_ignore_ascii_case("ok")
                    || line.starts_with("error:")
                    || line.starts_with("ALARM:");
                if !grbl && !grbl_like {
                    return None;
                }
                if line.starts_with('<') && line.ends_with('>') && line.contains('|') {
                    let state = line[1..].split(['|', ':', '>']).next().unwrap_or("").trim();
                    if !state.is_empty() {
                        identity.state = Some(state.to_owned());
                        grbl = true;
                    }
                } else if let Some(firmware) = firmware_from_line(line) {
                    identity.firmware = Some(firmware);
                    grbl = true;
                } else if let Some(name) = line
                    .strip_prefix("[MSG:Machine:")
                    .map(|rest| rest.trim_end_matches(']').trim())
                    .filter(|name| !name.is_empty())
                {
                    identity.machine = Some(name.to_owned());
                } else if line.eq_ignore_ascii_case("ok") {
                    saw_ok = true;
                }
                // `$I` 以 ok 结束（VER / OPT / Machine 都在它之前），收齐即返回
                if identity.state.is_some() && saw_ok {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    grbl.then_some(identity)
}

/// 连上 `spec` 做一次只读识别；连不上或不是 GRBL 时返回原因。
pub fn identify_controller(
    spec: &ConnectionSpec,
    connect_timeout: Duration,
    reply_timeout: Duration,
) -> Result<ControllerIdentity, String> {
    let transport = open_transport(spec, connect_timeout)?;
    identify_grbl(transport.as_ref(), reply_timeout)
        .ok_or_else(|| "没有按 GRBL / FluidNC 回应".to_owned())
}

/// 本机局域网网卡所在 IPv4 私有网段（各取本机所在的 /24）里开着 `port` 的地址（不含本机）。
/// 只做 TCP 连接尝试，不发数据。直接枚举网卡而不是看出口路由：开着代理 TUN 时出口是虚拟网卡。
pub fn scan_subnet_port(port: u16, connect_timeout: Duration) -> Vec<std::net::Ipv4Addr> {
    scan_subnet_port_with(port, connect_timeout, &|_| {})
}

/// 同 [`scan_subnet_port`]，每连上一个地址就立即回调（2026-11-09：边扫边确认）。
pub fn scan_subnet_port_with(
    port: u16,
    connect_timeout: Duration,
    on_open: &(dyn Fn(std::net::Ipv4Addr) + Sync),
) -> Vec<std::net::Ipv4Addr> {
    let locals = local_lan_ipv4();
    let mut hosts: Vec<std::net::Ipv4Addr> = Vec::new();
    for local in &locals {
        for host in subnet_hosts(*local) {
            if !hosts.contains(&host) && !locals.contains(&host) {
                hosts.push(host);
            }
        }
    }
    scan_hosts_port_with(hosts, port, connect_timeout, on_open)
}

/// 本机局域网网卡的私有 IPv4 地址：有广播地址（Wi-Fi / 有线网），跳过回环与代理 / VPN 的点对点虚拟网卡。
pub fn local_lan_ipv4() -> Vec<std::net::Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for interface in interfaces {
        let if_addrs::IfAddr::V4(v4) = &interface.addr else {
            continue;
        };
        let name = interface.name.to_ascii_lowercase();
        let virtual_tunnel = ["utun", "tun", "tap", "ppp", "wg", "ipsec", "gif", "stf"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
        if v4.ip.is_loopback() || !v4.ip.is_private() || v4.broadcast.is_none() || virtual_tunnel {
            continue;
        }
        if !found.contains(&v4.ip) {
            found.push(v4.ip);
        }
    }
    found
}

/// `local` 所在 /24 的其余 253 个主机地址。
pub fn subnet_hosts(local: std::net::Ipv4Addr) -> Vec<std::net::Ipv4Addr> {
    let [a, b, c, d] = local.octets();
    (1..=254u8)
        .filter(|host| *host != d)
        .map(|host| std::net::Ipv4Addr::new(a, b, c, host))
        .collect()
}

/// 并行尝试连接一批地址的 `port`，返回连得上的。
pub fn scan_hosts_port(
    hosts: Vec<std::net::Ipv4Addr>,
    port: u16,
    connect_timeout: Duration,
) -> Vec<std::net::Ipv4Addr> {
    scan_hosts_port_with(hosts, port, connect_timeout, &|_| {})
}

/// 每个地址一个线程同时连接（一个 /24 最多 253 个，约一个连接超时即可扫完），连上即回调。
pub fn scan_hosts_port_with(
    hosts: Vec<std::net::Ipv4Addr>,
    port: u16,
    connect_timeout: Duration,
    on_open: &(dyn Fn(std::net::Ipv4Addr) + Sync),
) -> Vec<std::net::Ipv4Addr> {
    let open = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for host in hosts {
            let open = &open;
            scope.spawn(move || {
                let addr = std::net::SocketAddr::from((host, port));
                if TcpStream::connect_timeout(&addr, connect_timeout).is_ok() {
                    on_open(host);
                    if let Ok(mut found) = open.lock() {
                        found.push(host);
                    }
                }
            });
        }
    });
    let mut found = open.into_inner().unwrap_or_default();
    found.sort();
    found
}

/// 将 G-code 文件内容通过 FluidNC 的 HTTP /upload 接口上传至 SD 卡
pub fn upload_gcode_to_fluidnc_sd(
    host: &str,
    port: u16,
    remote_filename: &str,
    file_bytes: &[u8],
    timeout: Duration,
) -> Result<(), String> {
    use std::io::Read;

    let boundary = "---------------------------MOSSBoundary7d93b28b";
    let header_part = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{remote_filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    let footer_part = format!("\r\n--{boundary}--\r\n");
    let content_length = header_part.len() + file_bytes.len() + footer_part.len();

    let request_header = format!(
        "POST /upload HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         User-Agent: MOSS-Laser\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\n\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n\r\n"
    );

    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("解析地址 {host}:{port} 失败: {e}"))?;

    let mut last_err = format!("连接 {host}:{port} 失败");
    let mut stream_opt = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
                stream_opt = Some(s);
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }

    let mut stream = stream_opt.ok_or(last_err)?;
    stream
        .write_all(request_header.as_bytes())
        .map_err(|e| e.to_string())?;
    stream
        .write_all(header_part.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(file_bytes).map_err(|e| e.to_string())?;
    stream
        .write_all(footer_part.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    let resp_str = String::from_utf8_lossy(&response);
    let status_line = resp_str.lines().next().unwrap_or("");
    if status_line.contains(" 200 ")
        || status_line.contains(" 204 ")
        || status_line.contains(" 201 ")
    {
        Ok(())
    } else {
        Err(format!("FluidNC HTTP 上传失败，响应: {status_line}"))
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
    fn send_preflight_verifies_three_gates_strictly() {
        let temp = tempfile::tempdir().unwrap();
        let gcode_file = temp.path().join("cut.gcode");
        std::fs::write(&gcode_file, b"G1 X10 Y10").unwrap();
        let correct_sha = hex::encode(Sha256::digest(b"G1 X10 Y10"));

        let valid = SendPreflight {
            status: "pending".into(),
            expected_sha256: correct_sha.clone(),
            actual_sha256: None,
            gcode_path: Some(gcode_file.clone()),
            workflow_snapshot_valid: true,
            calibration_sha_valid: false,
        };
        assert!(authorize_send(&valid).is_ok());

        // Gate 1: Non-pending
        let mut bad_status = valid.clone();
        bad_status.status = "running".into();
        assert!(authorize_send(&bad_status).is_err());

        // Gate 2: SHA mismatch
        let mut bad_sha = valid.clone();
        bad_sha.expected_sha256 = "0".repeat(64);
        assert!(authorize_send(&bad_sha).is_err());

        // Gate 3: Neither workflow nor calibration
        let mut bad_auth = valid.clone();
        bad_auth.workflow_snapshot_valid = false;
        bad_auth.calibration_sha_valid = false;
        assert!(authorize_send(&bad_auth).is_err());
    }

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
    fn raw_abort_handle_executes_synchronously() {
        let fake = FakeGrbl::new(["$32=1".to_string()]);
        let handle = fake.raw_abort_handle().unwrap();
        handle().unwrap();
        assert_eq!(fake.realtime(), vec![0x18]);
    }

    #[test]
    fn handshake_consumes_terminal_ack_and_all_settings() {
        let fake =
            FakeGrbl::new(["$32=1", "$30=1000", "$130=100", "ok", "error:2"].map(str::to_owned));
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
        let fake =
            FakeGrbl::new(["$30=1000", "$32=1", "ok", "<Idle|MPos:0,0,0>"].map(str::to_owned));
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
    fn diagnose_device_message_translates_alarms_errors_and_fluidnc_msgs() {
        assert_eq!(
            diagnose_device_message("[MSG:ERR: Hard limit on X axis]"),
            Some("控制器硬件错误: Hard limit on X axis".into())
        );
        assert_eq!(
            diagnose_device_message("[MSG:WARN: Reset during motion]"),
            Some("控制器警告: Reset during motion".into())
        );
        let alarm1 = diagnose_device_message("ALARM:1").unwrap();
        assert!(alarm1.contains("硬限位开关触发"));
        let alarm2 = diagnose_device_message("ALARM:2").unwrap();
        assert!(alarm2.contains("软限位超程"));
        let err20 = diagnose_device_message("error:20").unwrap();
        assert!(err20.contains("不支持或无效的 G-code 指令"));
        let err9 = diagnose_device_message("error:9").unwrap();
        assert!(err9.contains("控制器处于报警或锁定状态"));
        assert_eq!(diagnose_device_message("ok"), None);
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
    fn parse_fluidnc_yaml_and_grbl_limits_extract_limits_and_features() {
        let yaml = r#"
axes:
  x:
    steps_per_mm: 80.000
    max_travel_mm: 300.000
  y:
    steps_per_mm: 80.000
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
        assert_eq!(features.speed_map.as_deref(), Some("0=0% 1000=100%"));
        assert!(features.has_air_assist);
        assert!(features.has_door);

        let grbl_settings = vec![
            "$130=280.000".to_string(),
            "$131=180.000".to_string(),
            "$132=45.000".to_string(),
        ];
        let grbl_limits = parse_grbl_settings_limits(&grbl_settings);
        assert_eq!(grbl_limits.x_travel_mm, Some(280.0));
        assert_eq!(grbl_limits.y_travel_mm, Some(180.0));
        assert_eq!(grbl_limits.z_travel_mm, Some(45.0));
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
        let fake = FakeGrbl::new(
            ["[MSG:ERR: /sd/missing.gcode not found]", "error:60"].map(str::to_owned),
        );
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
    fn diagnose_device_message_translates_fluidnc_alarms_and_errors() {
        let alarm11 = diagnose_device_message("ALARM:11").unwrap();
        assert!(alarm11.contains("启动引脚电平异常"), "{alarm11}");
        let alarm14 = diagnose_device_message("ALARM:14").unwrap();
        assert!(alarm14.contains("未回零安全锁定"), "{alarm14}");
        let alarm18 = diagnose_device_message("ALARM:18").unwrap();
        assert!(
            alarm18.contains("对刀过程触碰到了物理硬限位开关"),
            "{alarm18}"
        );

        let err60 = diagnose_device_message("error:60").unwrap();
        assert!(err60.contains("SD 卡挂载失败"), "{err60}");
        let err65 = diagnose_device_message("error:65").unwrap();
        assert!(
            err65.contains("指定的文件在 SD 卡或内部 Flash 中不存在"),
            "{err65}"
        );
        let err150 = diagnose_device_message("error:150").unwrap();
        assert!(err150.contains("引脚配置冲突"), "{err150}");
        let err152 = diagnose_device_message("error:152").unwrap();
        assert!(err152.contains("YAML 配置文件语法错误"), "{err152}");
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
        let fake = FakeGrbl::new(
            ["$30=1000", "$32=1", "ok", "<Idle|MPos:0,0,0|Pn:XY>"].map(str::to_owned),
        );
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
    fn auto_report_is_enabled_only_for_fluidnc_and_its_ack_is_consumed_before_gcode() {
        // 旧 GRBL（无 features）：不发 $RI
        let fake = FakeGrbl::new(["$32=1", "ok", "ok", "ok", "<Idle>"].map(str::to_owned));
        let mut device = DeviceConnection::new(fake.clone());
        device.handshake().unwrap();
        let mut worker = JobWorker::new(device, CancellationToken::default());
        assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
        assert_eq!(fake.lines(), ["$$", "M5", "G4 P0"]);

        // FluidNC：$RI=200 的 ok 先被消费，行 ack 计数不受影响；结束后 $RI=0
        let fake =
            FakeGrbl::new(["$32=1", "ok", "ok", "ok", "ok", "<Idle>", "ok"].map(str::to_owned));
        let mut device = DeviceConnection::new(fake.clone());
        device.handshake().unwrap();
        device.features = Some(MachineFeatures::default());
        let mut worker = JobWorker::new(device, CancellationToken::default());
        assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
        assert_eq!(worker.progress.lines_sent.load(Ordering::Relaxed), 1);
        assert_eq!(worker.progress.status_report().as_deref(), Some("<Idle>"));
        assert_eq!(fake.lines(), ["$$", "$RI=200", "M5", "G4 P0", "$RI=0"]);

        // 固件不认识 $RI（error:3）：静默关闭，不影响后续
        let fake =
            FakeGrbl::new(["$32=1", "ok", "error:3", "ok", "ok", "<Idle>"].map(str::to_owned));
        let mut device = DeviceConnection::new(fake.clone());
        device.handshake().unwrap();
        device.features = Some(MachineFeatures::default());
        let mut worker = JobWorker::new(device, CancellationToken::default());
        assert_eq!(worker.send_lines(["M5".into()]).unwrap(), 1);
        assert_eq!(worker.device.auto_report_ms, 0);
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
                    let until =
                        *gate.get_or_insert_with(|| Instant::now() + Duration::from_millis(ms));
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
}

#[cfg(test)]
mod discovery_identity_tests {
    use super::*;

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
        let identity =
            identify_grbl(&fluidnc, Duration::from_millis(200)).expect("FluidNC 应被识别");
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
}
