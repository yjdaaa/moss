//! 完整过程记录（2026-11-22 用户决定，技术书 n105 / 融合技术书 r55）：语音对话与激光生成的中间过程写进
//! 运行目录 `logs/process-trace/`，方便事后核对“当时到底说了什么、模型怎么回的、生成了什么”。
//!
//! - **默认关闭**：只有 `settings.json` 顶层 `process_trace_enabled` 为 `true` 时才写；关着时
//!   [`ProcessTrace::record`] / [`ProcessTrace::save_file`] 什么都不做，行为与从前一致。开关每 2 秒最多读一次
//!   设置文件，改了不用重启。
//! - **目录**：`logs/process-trace/<本地日期>/` 下语音与激光按发生先后写进同一条时间线：`timeline.jsonl`
//!   一行一条完整记录（序号、所属流程、步骤类型、一句中文说明与细节），`timeline.txt` 一行一步可直接阅读；
//!   录音、合成语音、预览图等附件放在同日的 `<域>-files/` 下，记录里只写相对记录根目录的路径。
//!   桌面原生选择框可指定 `process_trace_dir`，新记录立即生效；一组附件与记录用 [`ProcessTrace::scoped`]
//!   固定目录完成。自定义根目录不可用时暂停写入，不重建挂载路径、不回退，旧目录不搬迁也不继续清理。
//! - **顺序**（2026-11-23 n110）：取时刻与入队在同一把锁里完成，写线程按入队先后编 `seq` 落盘，所以文件里的
//!   先后、`seq` 与 `at` 三者一致，就是调用方真实发生的先后。
//! - **不限大小**（2026-11-23 n110 用户决定“不要限制多少硬盘占用”）：不按总量删、不丢记录、不截附件；
//!   写入队列不设上限（入队从不阻塞，急停与音频线程不会被慢盘卡住）。只按目录名日期保留最近
//!   [`RETENTION_DAYS`] 天，清理不看开关，关掉后旧记录照样到期删除。附件真没写成（磁盘满、权限等）时
//!   在时间线里写一步说明，不留指向不存在文件的路径而不提。
//! - 调用方负责在写入前去掉密钥与令牌；本模块不读取、不记录任何凭据，也不发往网络。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, NaiveDate};
use serde_json::{Map, Value};

mod directory;
#[cfg(test)]
use directory::day_dirs;
use directory::{TraceDirectory, append_line, day_record_size};

/// 过程记录时间戳类型（调用方可在请求开始时采样，与记录本身的 `at` 格式完全一致）。
pub type TraceTimestamp = DateTime<Local>;

/// 采样当前本地时间，供请求入口等延迟写入的步骤记录开始时刻。
pub fn now_timestamp() -> TraceTimestamp {
    Local::now()
}

/// 把时间戳格式化成记录里 `at` 的写法（RFC 3339，毫秒，带时区）。
pub fn format_timestamp(at: TraceTimestamp) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()
}

/// `settings.json` 顶层开关键（默认关）。
pub const SETTING_KEY: &str = "process_trace_enabled";
/// 仅由桌面原生文件夹选择框修改的最终保存目录。
pub const DIRECTORY_KEY: &str = "process_trace_dir";
/// 记录目录（相对运行目录）。
pub const TRACE_DIR: &str = "logs/process-trace";
/// 按日期保留的天数（含今天；2026-11-23 n110 由 7 天改为 30 天，不再按大小删）。
pub const RETENTION_DAYS: i64 = 30;
/// 一行一条完整记录（机器读）。
pub const TIMELINE_JSONL: &str = "timeline.jsonl";
/// 一行一步的中文时间线（人直接看）。
pub const TIMELINE_TXT: &str = "timeline.txt";

const SETTING_CACHE: Duration = Duration::from_secs(2);
/// 内存里排队待写的附件字节数高水位（n110 审查修复）：硬盘不限，但盘太慢时附件不能无限堆在内存里；
/// 超过时这个附件不收，并在时间线里写明原因（记录本身照收）。
pub const MAX_QUEUED_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// 当前记录占用（给设置页显示）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TraceUsage {
    pub bytes: u64,
    pub days: u64,
}

/// 设置页使用的当前目录与可用状态；不向公开系统信息暴露本机路径。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraceDirectoryStatus {
    pub dir: PathBuf,
    pub custom_dir: Option<PathBuf>,
    pub available: bool,
    pub error: Option<String>,
}

enum WriteOp {
    Record {
        directory: Arc<TraceDirectory>,
        at: DateTime<Local>,
        domain: String,
        flow: String,
        kind: String,
        step: String,
        fields: Value,
        dropped_before: u64,
    },
    SaveFile {
        directory: Arc<TraceDirectory>,
        at: DateTime<Local>,
        domain: String,
        rel: String,
        bytes: Vec<u8>,
    },
    Sweep {
        directory: Arc<TraceDirectory>,
        today: NaiveDate,
        ack: Option<SyncSender<()>>,
    },
    Clear {
        directory: Arc<TraceDirectory>,
        ack: SyncSender<std::io::Result<()>>,
    },
    Flush {
        ack: SyncSender<()>,
    },
    #[cfg(test)]
    PauseForTest {
        ready: SyncSender<()>,
        resume: mpsc::Receiver<()>,
    },
}

struct Inner {
    runtime_dir: PathBuf,
    directory: Arc<Mutex<Arc<TraceDirectory>>>,
    /// 目录保存串行化；这把锁不用于记录入队，慢盘不会阻塞急停或音频线程。
    directory_change: Mutex<()>,
    /// 开关的最近读数（原子量：读开关永不排队等锁，n110 审查修复）。
    enabled_flag: AtomicBool,
    /// 是否已经读过一次设置文件。
    loaded: AtomicBool,
    /// 上次读 `settings.json` 的时刻；谁拿到锁谁去刷新，其余线程直接用上次读数。
    checked: Mutex<Option<Instant>>,
    /// 后台单写线程通道（懒启动，关着时不建线程）；不反向持有 `Inner`，避免循环引用。
    /// 这把锁同时保证“取时刻 + 入队”不被别的线程插队，文件里的先后就是真实先后。
    writer: Mutex<Option<Sender<WriteOp>>>,
    /// 写线程起不来或已退出时没能入队的条数（下一条成功写入的记录会注明）。
    dropped: Arc<AtomicU64>,
    /// 附件文件名里的编号（本次运行内递增，不回绕）。
    file_seq: AtomicU64,
    /// 已入队、还没落盘的附件字节数。
    queued_file_bytes: Arc<AtomicU64>,
    max_queued_file_bytes: u64,
    run_id: i64,
}

/// 完整过程记录器；克隆共享同一份状态。
#[derive(Clone)]
pub struct ProcessTrace {
    inner: Arc<Inner>,
    /// 只为一组附件与记录固定目录；普通句柄随设置切换。
    scoped_directory: Option<Arc<TraceDirectory>>,
}

impl std::fmt::Debug for ProcessTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessTrace")
            .field("dir", &self.dir())
            .finish()
    }
}

impl ProcessTrace {
    pub fn new(runtime_dir: impl Into<PathBuf>) -> Self {
        let runtime_dir = runtime_dir.into();
        let settings =
            crate::settings_store::read(&crate::settings_store::settings_path(&runtime_dir));
        let custom = match settings.get(DIRECTORY_KEY) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) if raw.trim().is_empty() => None,
            Some(Value::String(raw)) => Some(PathBuf::from(raw)),
            Some(_) => Some(PathBuf::from("<invalid-process-trace-dir>")),
        };
        let directory = Arc::new(TraceDirectory::new(
            custom
                .clone()
                .unwrap_or_else(|| runtime_dir.join(TRACE_DIR)),
            custom.is_some(),
        ));
        Self {
            scoped_directory: None,
            inner: Arc::new(Inner {
                runtime_dir,
                directory: Arc::new(Mutex::new(directory)),
                directory_change: Mutex::new(()),
                enabled_flag: AtomicBool::new(false),
                loaded: AtomicBool::new(false),
                checked: Mutex::new(None),
                writer: Mutex::new(None),
                dropped: Arc::new(AtomicU64::new(0)),
                file_seq: AtomicU64::new(0),
                queued_file_bytes: Arc::new(AtomicU64::new(0)),
                max_queued_file_bytes: MAX_QUEUED_FILE_BYTES,
                run_id: Local::now().timestamp_millis(),
            }),
        }
    }

    #[cfg(test)]
    fn pause_writer_for_test(&self) -> SyncSender<()> {
        let tx = {
            let mut slot = lock(&self.inner.writer);
            self.writer_in(&mut slot).expect("writer thread")
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        tx.send(WriteOp::PauseForTest {
            ready: ready_tx,
            resume: resume_rx,
        })
        .unwrap();
        ready_rx.recv().unwrap();
        resume_tx
    }

    fn directory(&self) -> Arc<TraceDirectory> {
        self.scoped_directory
            .clone()
            .unwrap_or_else(|| lock(&self.inner.directory).clone())
    }

    /// 固定一组附件与对应记录的目录，避免设置切换插在两次写入之间。
    pub fn scoped(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            scoped_directory: Some(self.directory()),
        }
    }

    /// 当前记录目录；默认是 `<运行目录>/logs/process-trace`。
    pub fn dir(&self) -> PathBuf {
        self.directory().path.clone()
    }

    pub fn directory_status(&self) -> TraceDirectoryStatus {
        let directory = self.directory();
        let error = directory.status_error();
        TraceDirectoryStatus {
            dir: directory.path.clone(),
            custom_dir: directory.custom.then(|| directory.path.clone()),
            available: error.is_none(),
            error,
        }
    }

    /// 仅供受信任的桌面原生选择框调用；保存成功后新记录立即使用新目录。
    /// 已经开始的一组附件与记录由 [`Self::scoped`] 留在原目录完成，不搬迁旧记录。
    pub fn set_directory(&self, selected: Option<&Path>) -> Result<(), String> {
        let _change = lock(&self.inner.directory_change);
        let directory = match selected {
            Some(selected) => {
                TraceDirectory::selected(selected).map_err(|error| error.to_string())?
            }
            None => TraceDirectory::new(self.inner.runtime_dir.join(TRACE_DIR), false),
        };
        if let Some(error) = directory.status_error() {
            return Err(error);
        }
        let path = directory
            .path
            .to_str()
            .ok_or_else(|| "保存位置包含无法保存的字符".to_owned())?;
        crate::settings_store::update(
            &crate::settings_store::settings_path(&self.inner.runtime_dir),
            |settings| {
                if directory.custom {
                    settings.insert(DIRECTORY_KEY.into(), Value::String(path.to_owned()));
                } else {
                    settings.remove(DIRECTORY_KEY);
                }
                Ok(())
            },
        )?;
        *lock(&self.inner.directory) = Arc::new(directory);
        Ok(())
    }

    /// 开关是否打开（读 `settings.json`，2 秒内复用上次结果）。
    ///
    /// 只用 `try_lock` 抢刷新权：别的线程正在读设置文件时不等，直接返回上次读数，
    /// 所以急停、发送、音频线程不会因为别人在读盘而卡住。
    pub fn enabled(&self) -> bool {
        let mut checked = match self.inner.checked.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
            // 已读过一次：别人正在刷新时直接用上次读数，不等
            Err(std::sync::TryLockError::WouldBlock)
                if self.inner.loaded.load(Ordering::Acquire) =>
            {
                return self.inner.enabled_flag.load(Ordering::Acquire);
            }
            // 还一次都没读过：没有可用的读数，只能等第一次读完（否则这几条会被当成“关着”丢掉）
            Err(std::sync::TryLockError::WouldBlock) => lock(&self.inner.checked),
        };
        if let Some(at) = *checked
            && at.elapsed() < SETTING_CACHE
        {
            return self.inner.enabled_flag.load(Ordering::Acquire);
        }
        let value = read_setting(&self.inner.runtime_dir);
        self.inner.enabled_flag.store(value, Ordering::Release);
        self.inner.loaded.store(true, Ordering::Release);
        *checked = Some(Instant::now());
        value
    }

    /// 设置页刚保存过开关时调用：立即重读一次，之后 [`Self::enabled`] 拿到的就是新值。
    pub fn invalidate(&self) {
        let mut checked = lock(&self.inner.checked);
        let value = read_setting(&self.inner.runtime_dir);
        self.inner.enabled_flag.store(value, Ordering::Release);
        self.inner.loaded.store(true, Ordering::Release);
        *checked = Some(Instant::now());
    }

    /// 取出（必要时启动）写线程通道；调用方已持有 `writer` 锁。
    fn writer_in(&self, slot: &mut Option<Sender<WriteOp>>) -> Option<Sender<WriteOp>> {
        if let Some(tx) = slot.as_ref() {
            return Some(tx.clone());
        }
        let (tx, rx) = mpsc::channel::<WriteOp>();
        let directory = self.inner.directory.clone();
        let run_id = self.inner.run_id;
        let dropped = self.inner.dropped.clone();
        let queued = self.inner.queued_file_bytes.clone();
        std::thread::Builder::new()
            .name("moss-process-trace".into())
            .spawn(move || writer_loop(rx, directory, run_id, dropped, queued))
            .ok()?;
        *slot = Some(tx.clone());
        Some(tx)
    }

    /// 等待已排队的写入全部落盘（供统计、测试或退出前同步；没写过时立刻返回）。
    pub fn flush(&self) {
        let tx = lock(&self.inner.writer).clone();
        let Some(tx) = tx else {
            return;
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if tx.send(WriteOp::Flush { ack: ack_tx }).is_ok() {
            let _ = ack_rx.recv();
        }
    }

    /// 追加一步到当天的时间线（非阻塞入队，由单一后台线程按先后顺序落盘）。
    ///
    /// - `domain`：`voice` / `laser` / `diag`；`flow`：这一步属于哪条流程（如“语音·a1b2·第3轮”“激光·wf-12”）；
    /// - `kind`：机器可读的步骤类型；`step`：一句中文说明这一步在干什么；
    /// - `fields`：细节（对象并入顶层，否则放在 `data` 下）。
    ///
    /// 同时写 `timeline.jsonl`（一行一条完整记录）与 `timeline.txt`（一行一步，直接打开就能看）。
    /// 每条带本次运行内递增的序号 `seq` 与运行编号 `run`（MOSS 启动时刻），开关关着时什么也不做。
    pub fn record(&self, domain: &str, flow: &str, kind: &str, step: &str, fields: Value) {
        if !self.enabled() {
            return;
        }
        self.enqueue_record(None, domain, flow, kind, step, fields);
    }

    /// 补齐成对步骤的收尾那一步，不看开关：开始那一步已经记下，处理中开关刚被关掉时，
    /// 结束这一步照样写，免得时间线里留下没有结果的“开始处理”。只给成对步骤的收尾用。
    pub fn record_closing(&self, domain: &str, flow: &str, kind: &str, step: &str, fields: Value) {
        self.enqueue_record(None, domain, flow, kind, step, fields);
    }

    /// 取时刻与入队在同一把锁里：先拿到锁的先记时刻、先入队，`seq` 与 `at` 不会倒挂。
    fn enqueue_record(
        &self,
        at: Option<DateTime<Local>>,
        domain: &str,
        flow: &str,
        kind: &str,
        step: &str,
        fields: Value,
    ) {
        let mut slot = lock(&self.inner.writer);
        let at = at.unwrap_or_else(Local::now);
        let Some(tx) = self.writer_in(&mut slot) else {
            self.inner.dropped.fetch_add(1, Ordering::AcqRel);
            return;
        };
        let dropped_before = self.inner.dropped.swap(0, Ordering::AcqRel);
        let op = WriteOp::Record {
            directory: self.directory(),
            at,
            domain: domain.to_owned(),
            flow: flow.to_owned(),
            kind: kind.to_owned(),
            step: step.to_owned(),
            fields,
            dropped_before,
        };
        if tx.send(op).is_err() {
            // 写线程已退出：下次重新起一个，这几条计入丢失
            *slot = None;
            self.inner
                .dropped
                .fetch_add(dropped_before.saturating_add(1), Ordering::AcqRel);
        }
    }

    #[cfg(test)]
    fn record_at(
        &self,
        at: DateTime<Local>,
        domain: &str,
        flow: &str,
        kind: &str,
        step: &str,
        fields: Value,
    ) {
        if self.enabled() {
            self.enqueue_record(Some(at), domain, flow, kind, step, fields);
        }
    }

    /// 保存一个附件（录音、合成语音、预览图等），返回相对 `logs/process-trace/` 的路径。
    /// 不限大小；只有开关关着或写线程起不来时返回 `None`。真正落盘失败时写线程会在时间线里补一步说明。
    pub fn save_file(&self, domain: &str, stem: &str, ext: &str, bytes: &[u8]) -> Option<String> {
        if !self.enabled() {
            return None;
        }
        let size = bytes.len() as u64;
        let queued = self
            .inner
            .queued_file_bytes
            .fetch_add(size, Ordering::AcqRel);
        if queued.saturating_add(size) > self.inner.max_queued_file_bytes && queued > 0 {
            self.inner
                .queued_file_bytes
                .fetch_sub(size, Ordering::AcqRel);
            self.record(
                domain,
                "诊断·过程记录",
                "attachment_failed",
                &format!(
                    "附件「{stem}.{ext}」没保存：硬盘写得太慢，内存里还有 {} MB 附件没写完（{} 字节）",
                    queued / 1024 / 1024,
                    size
                ),
                serde_json::json!({"stem": stem, "ext": ext, "bytes": size, "queued_bytes": queued}),
            );
            return None;
        }
        // 拷贝放在锁外，持锁时只取时刻、编号与入队
        let bytes = bytes.to_vec();
        let mut slot = lock(&self.inner.writer);
        let Some(tx) = self.writer_in(&mut slot) else {
            self.inner
                .queued_file_bytes
                .fetch_sub(size, Ordering::AcqRel);
            self.inner.dropped.fetch_add(1, Ordering::AcqRel);
            return None;
        };
        let at = Local::now();
        let seq = self.inner.file_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let rel = format!(
            "{}/{}-files/{}-{seq:06}-{}.{}",
            at.format("%Y-%m-%d"),
            safe_name(domain),
            at.format("%H%M%S%.3f").to_string().replace('.', ""),
            safe_name(stem),
            safe_name(ext)
        );
        let op = WriteOp::SaveFile {
            directory: self.directory(),
            at,
            domain: domain.to_owned(),
            rel: rel.clone(),
            bytes,
        };
        if tx.send(op).is_err() {
            *slot = None;
            self.inner
                .queued_file_bytes
                .fetch_sub(size, Ordering::AcqRel);
            self.inner.dropped.fetch_add(1, Ordering::AcqRel);
            return None;
        }
        Some(rel)
    }

    /// 复制一个已有文件作为附件（激光生成的预览图等），返回相对路径。
    pub fn copy_file(&self, domain: &str, stem: &str, source: &Path) -> Option<String> {
        if !self.enabled() {
            return None;
        }
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_owned();
        match fs::read(source) {
            Ok(bytes) => self.save_file(domain, stem, &ext, &bytes),
            Err(error) => {
                self.record(
                    domain,
                    "诊断·过程记录",
                    "attachment_failed",
                    &format!("附件没保存：读不到 {}（{error}）", source.display()),
                    serde_json::json!({"source": source.display().to_string(), "error": error.to_string()}),
                );
                None
            }
        }
    }

    /// 当前占用：总字节数与含 MOSS 记录的日期数（忽略归属标记与用户无关文件）。
    pub fn usage(&self) -> TraceUsage {
        self.flush();
        let mut usage = TraceUsage::default();
        for (_, path) in self.directory().days() {
            let (has_records, bytes) = day_record_size(&path);
            if has_records {
                usage.days += 1;
                usage.bytes += bytes;
            }
        }
        usage
    }

    /// 删除全部记录（设置页“清空过程记录”）。
    pub fn clear(&self) -> std::io::Result<()> {
        // 删目录交给写线程做（与写入排在同一队列里，不会和首写交错）；持锁只为取通道，不在锁里碰磁盘
        let tx = {
            let mut slot = lock(&self.inner.writer);
            self.writer_in(&mut slot)
        };
        let directory = self.directory();
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if let Some(tx) = tx
            && tx
                .send(WriteOp::Clear {
                    directory: directory.clone(),
                    ack: ack_tx,
                })
                .is_ok()
            && let Ok(res) = ack_rx.recv()
        {
            return res;
        }
        let res = clear_dir(&directory);
        if res.is_ok() {
            self.inner.dropped.store(0, Ordering::Relaxed);
        }
        res
    }

    /// 到期清理（不看开关）：删掉早于保留期的日期目录。
    pub fn sweep(&self) {
        self.sweep_at(Local::now().date_naive());
    }

    fn sweep_at(&self, today: NaiveDate) {
        let directory = self.directory();
        let tx = {
            let slot = lock(&self.inner.writer);
            slot.clone()
        };
        let Some(tx) = tx else {
            // 还没写过：没有并发写入，锁外直接清理
            sweep_on_disk(&directory, today);
            return;
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if tx
            .send(WriteOp::Sweep {
                directory: directory.clone(),
                today,
                ack: Some(ack_tx),
            })
            .is_ok()
        {
            let _ = ack_rx.recv();
            return;
        }
        sweep_on_disk(&directory, today);
    }
}

/// 后台写线程里的状态：编 `seq`、写两份时间线。
struct TimelineWriter {
    run_id: i64,
    seq: u64,
    /// 写线程自己没能落盘的条数（目录建不起来等），并入下一条成功记录的说明。
    unwritten: u64,
}

impl TimelineWriter {
    #[allow(clippy::too_many_arguments)]
    fn write(
        &mut self,
        directory: &TraceDirectory,
        at: DateTime<Local>,
        domain: &str,
        flow: &str,
        kind: &str,
        step: &str,
        fields: Value,
        dropped_before: u64,
    ) {
        let dropped_before = dropped_before.saturating_add(std::mem::take(&mut self.unwritten));
        self.seq += 1;
        let seq = self.seq;
        let mut line = Map::new();
        line.insert("seq".into(), Value::from(seq));
        line.insert("run".into(), Value::from(self.run_id));
        line.insert("at".into(), Value::String(format_timestamp(at)));
        line.insert("at_ms".into(), Value::from(at.timestamp_millis()));
        line.insert("domain".into(), Value::String(domain.to_owned()));
        line.insert("flow".into(), Value::String(flow.to_owned()));
        line.insert("kind".into(), Value::String(kind.to_owned()));
        line.insert("step".into(), Value::String(step.to_owned()));
        if dropped_before > 0 {
            line.insert("dropped_before".into(), Value::from(dropped_before));
        }
        match fields {
            Value::Object(map) => {
                for (key, value) in map {
                    line.entry(key).or_insert(value);
                }
            }
            Value::Null => {}
            other => {
                line.insert("data".into(), other);
            }
        }
        let Ok(mut json) = serde_json::to_string(&Value::Object(line)) else {
            self.unwritten = self
                .unwritten
                .saturating_add(dropped_before)
                .saturating_add(1);
            return;
        };
        json.push('\n');
        let drop_note = if dropped_before > 0 {
            format!("（此前有 {dropped_before} 条记录没能写入：目录不可用或写入失败）")
        } else {
            String::new()
        };
        let readable = format!(
            "{} #{seq:06} [{}] {}{drop_note}\n",
            at.format("%H:%M:%S%.3f"),
            flow.replace(['\r', '\n'], " "),
            step.replace(['\r', '\n'], " ")
        );
        let result = directory
            .prepare_day(&at.format("%Y-%m-%d").to_string())
            .and_then(|day_dir| {
                append_line(&day_dir.join(TIMELINE_JSONL), json.as_bytes())?;
                append_line(&day_dir.join(TIMELINE_TXT), readable.as_bytes())
            });
        if result.is_err() {
            self.unwritten = self
                .unwritten
                .saturating_add(dropped_before)
                .saturating_add(1);
        }
    }
}

fn writer_loop(
    rx: mpsc::Receiver<WriteOp>,
    current_directory: Arc<Mutex<Arc<TraceDirectory>>>,
    run_id: i64,
    dropped: Arc<AtomicU64>,
    queued: Arc<AtomicU64>,
) {
    let mut writer = TimelineWriter {
        run_id,
        seq: 0,
        unwritten: 0,
    };
    let mut last_sweep: Option<Instant> = None;
    let mut last_sweep_date: Option<NaiveDate> = None;
    let mut last_sweep_dir: Option<PathBuf> = None;

    while let Ok(op) = rx.recv() {
        // 只自动维护当前配置目录；旧 scoped 记录的收尾不会重新接管旧目录的保留期。
        if matches!(&op, WriteOp::Record { .. } | WriteOp::SaveFile { .. }) {
            let directory = lock(&current_directory).clone();
            let today = Local::now().date_naive();
            if last_sweep.is_none_or(|at| at.elapsed() >= SWEEP_INTERVAL)
                || last_sweep_date != Some(today)
                || last_sweep_dir.as_ref() != Some(&directory.path)
            {
                sweep_on_disk(&directory, today);
                last_sweep = Some(Instant::now());
                last_sweep_date = Some(today);
                last_sweep_dir = Some(directory.path.clone());
            }
        }
        match op {
            WriteOp::Record {
                directory,
                at,
                domain,
                flow,
                kind,
                step,
                fields,
                dropped_before,
            } => {
                writer.write(
                    &directory,
                    at,
                    &domain,
                    &flow,
                    &kind,
                    &step,
                    fields,
                    dropped_before,
                );
            }
            WriteOp::SaveFile {
                directory,
                at,
                domain,
                rel,
                bytes,
            } => {
                let result = directory.write_attachment(&rel, &bytes);
                queued.fetch_sub(bytes.len() as u64, Ordering::AcqRel);
                if let Err(error) = &result {
                    writer.write(
                        &directory, at, &domain, "诊断·过程记录", "attachment_failed",
                        &format!("附件没写成：{rel}（{} 字节，{error}）", bytes.len()),
                        serde_json::json!({"file": rel, "bytes": bytes.len(), "error": error.to_string()}), 0,
                    );
                }
            }
            WriteOp::Sweep {
                directory,
                today,
                ack,
            } => {
                sweep_on_disk(&directory, today);
                last_sweep = Some(Instant::now());
                last_sweep_date = Some(today);
                last_sweep_dir = Some(directory.path.clone());
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            WriteOp::Clear { directory, ack } => {
                let res = clear_dir(&directory);
                if res.is_ok() {
                    writer.unwritten = 0;
                    last_sweep = None;
                    last_sweep_date = None;
                    dropped.store(0, Ordering::Relaxed);
                }
                let _ = ack.send(res);
            }
            WriteOp::Flush { ack } => {
                let _ = ack.send(());
            }
            #[cfg(test)]
            WriteOp::PauseForTest { ready, resume } => {
                let _ = ready.send(());
                let _ = resume.recv();
            }
        }
    }
}

fn clear_dir(directory: &TraceDirectory) -> std::io::Result<()> {
    directory.check_root()?;
    for (_, path) in directory.days() {
        directory.clear_day(&path)?;
    }
    Ok(())
}

/// 只清理当前目录里早于保留期的记录；不删除未知文件或链接。
fn sweep_on_disk(directory: &TraceDirectory, today: NaiveDate) {
    let oldest_kept = today - chrono::Days::new((RETENTION_DAYS - 1).max(0) as u64);
    for (date, path) in directory.days() {
        if date < oldest_kept {
            let _ = directory.clear_day(&path);
        }
    }
}

fn read_setting(runtime_dir: &Path) -> bool {
    crate::settings_store::read(&crate::settings_store::settings_path(runtime_dir))
        .get(SETTING_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 文件名只留字母数字、`-`、`_`，中文等其他字符换成 `_`，最长 48 字符。
fn safe_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if cleaned.is_empty() {
        "item".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enable(dir: &Path, on: bool) {
        crate::settings_store::update(&crate::settings_store::settings_path(dir), |map| {
            map.insert(SETTING_KEY.into(), Value::Bool(on));
            Ok(())
        })
        .unwrap();
    }

    fn day_files(trace: &ProcessTrace) -> Vec<PathBuf> {
        day_dirs(&trace.dir()).into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn writes_nothing_when_switch_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let trace = ProcessTrace::new(tmp.path());
        assert!(!trace.enabled(), "默认关闭");
        trace.record(
            "voice",
            "语音",
            "asr",
            "识别结果",
            serde_json::json!({"text": "你好"}),
        );
        assert_eq!(trace.save_file("voice", "utterance", "wav", b"RIFF"), None);
        assert!(!trace.dir().exists(), "关着时不创建记录目录");
    }

    #[test]
    fn records_events_and_files_when_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path(), true);
        let trace = ProcessTrace::new(tmp.path());
        trace.record(
            "voice",
            "语音·a1·第1轮",
            "asr",
            "识别结果：「你好」\n换行不打断",
            serde_json::json!({"session_id": "s1", "raw": "你好"}),
        );
        trace.record(
            "laser",
            "激光·wf-1",
            "generate",
            "生成预览",
            serde_json::json!("plain"),
        );
        let rel = trace
            .save_file("voice", "turn 1/用户", "wav", b"RIFF-data")
            .unwrap();
        trace.flush();
        let days = day_files(&trace);
        assert_eq!(days.len(), 1);
        let timeline = fs::read_to_string(days[0].join(TIMELINE_JSONL)).unwrap();
        let lines: Vec<Value> = timeline
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "语音与激光写进同一条时间线");
        assert_eq!(lines[0]["domain"], "voice");
        assert_eq!(lines[0]["kind"], "asr");
        assert_eq!(lines[0]["flow"], "语音·a1·第1轮");
        assert_eq!(lines[0]["raw"], "你好");
        assert_eq!(lines[0]["seq"], 1);
        assert_eq!(lines[1]["seq"], 2, "按先后递增编号");
        assert_eq!(lines[1]["data"], "plain");
        assert!(lines[0]["at"].as_str().unwrap().contains('T'));
        let readable = fs::read_to_string(days[0].join(TIMELINE_TXT)).unwrap();
        let rows: Vec<&str> = readable.lines().collect();
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].ends_with("#000001 [语音·a1·第1轮] 识别结果：「你好」 换行不打断"),
            "{}",
            rows[0]
        );
        assert!(rows[1].contains("#000002 [激光·wf-1] 生成预览"));
        assert!(rel.contains("/voice-files/"), "{rel}");
        assert!(rel.ends_with("-turn_1___.wav"), "文件名只留安全字符：{rel}");
        assert_eq!(fs::read(trace.dir().join(&rel)).unwrap(), b"RIFF-data");
        assert!(trace.usage().bytes > 0);
        assert_eq!(trace.usage().days, 1);

        trace.clear().unwrap();
        assert!(trace.dir().is_dir(), "清空保留记录根目录");
        assert_eq!(trace.usage(), TraceUsage::default());
    }

    #[test]
    fn switch_changes_take_effect_after_invalidate() {
        let tmp = tempfile::tempdir().unwrap();
        let trace = ProcessTrace::new(tmp.path());
        assert!(!trace.enabled());
        enable(tmp.path(), true);
        trace.invalidate();
        assert!(trace.enabled());
        enable(tmp.path(), false);
        trace.invalidate();
        assert!(!trace.enabled());
    }

    #[test]
    fn sweep_only_drops_days_older_than_retention_and_never_by_size() {
        assert_eq!(RETENTION_DAYS, 30);
        let tmp = tempfile::tempdir().unwrap();
        let trace = ProcessTrace::new(tmp.path());
        let today = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        for (day, size) in [
            ("2026-10-23", 10usize),
            ("2026-10-24", 10),
            ("2026-11-16", 3_000_000),
            ("2026-11-22", 3_000_000),
        ] {
            let dir = trace.dir().join(day).join("voice-files");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("a.wav"), vec![0u8; size]).unwrap();
        }
        fs::create_dir_all(trace.dir().join("not-a-date")).unwrap();
        trace.sweep_at(today);
        let mut kept: Vec<String> = day_dirs(&trace.dir())
            .into_iter()
            .map(|(d, _)| d.to_string())
            .collect();
        kept.sort();
        // 含今天共 30 天：10-24 ~ 11-22 保留，10-23 到期；大的目录不因大小被删
        assert_eq!(kept, ["2026-10-24", "2026-11-16", "2026-11-22"]);
        assert!(trace.dir().join("not-a-date").exists(), "不认识的目录不动");
    }

    #[test]
    fn concurrent_records_keep_monotonic_seq_and_sanitize_flow_newlines() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path(), true);
        let trace = ProcessTrace::new(tmp.path());
        let handles: Vec<_> = (0..4)
            .map(|worker| {
                let trace = trace.clone();
                std::thread::spawn(move || {
                    for i in 0..10 {
                        trace.record(
                            "laser",
                            &format!("激光·wf-{worker}\nline-{i}"),
                            "api_call",
                            &format!("步骤 {worker}-{i}"),
                            serde_json::json!({"worker": worker, "i": i}),
                        );
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        trace.flush();

        let days = day_files(&trace);
        assert_eq!(days.len(), 1);
        let jsonl = fs::read_to_string(days[0].join(TIMELINE_JSONL)).unwrap();
        let seqs: Vec<u64> = jsonl
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["seq"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(seqs.len(), 40);
        assert!(
            seqs.windows(2).all(|w| w[0] + 1 == w[1]),
            "并发写入落盘序号必须严格单调递增：{seqs:?}"
        );
        let ats: Vec<i64> = jsonl
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["at_ms"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert!(
            ats.windows(2).all(|w| w[0] <= w[1]),
            "取时刻与入队在同一把锁里：序号在前的时刻不会更晚：{ats:?}"
        );
        let readable = fs::read_to_string(days[0].join(TIMELINE_TXT)).unwrap();
        assert_eq!(
            readable.lines().count(),
            40,
            "flow 含换行时 timeline.txt 仍保持一行一步"
        );
    }

    #[test]
    fn record_uses_enqueue_timestamp_instead_of_delayed_flush_time() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path(), true);
        let trace = ProcessTrace::new(tmp.path());
        let resume = trace.pause_writer_for_test();

        trace.record("laser", "激光·wf-9", "job_start", "开始加工", Value::Null);
        let after_enqueue_ms = Local::now().timestamp_millis();
        std::thread::sleep(Duration::from_millis(60));
        let _ = resume.send(());
        trace.flush();

        let days = day_files(&trace);
        let jsonl = fs::read_to_string(days[0].join(TIMELINE_JSONL)).unwrap();
        let row: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        let recorded_ms = row["at_ms"].as_i64().unwrap();
        assert!(
            recorded_ms <= after_enqueue_ms,
            "记录时间戳应取入队时刻而非延后的写盘时刻：recorded={recorded_ms}, after_enqueue={after_enqueue_ms}"
        );

        // 23:59:59 入队但跨日后才落盘的事件应写进其入队当天的目录
        let yesterday = Local::now().date_naive() - chrono::Days::new(1);
        let late_night = yesterday
            .and_hms_milli_opt(23, 59, 59, 500)
            .unwrap()
            .and_local_timezone(Local)
            .earliest()
            .unwrap();
        trace.record_at(
            late_night,
            "laser",
            "激光·wf-10",
            "job_end",
            "跨日前完成",
            Value::Null,
        );
        trace.flush();
        let y_dir = trace.dir().join(yesterday.format("%Y-%m-%d").to_string());
        let y_jsonl = fs::read_to_string(y_dir.join(TIMELINE_JSONL)).unwrap();
        let y_row: Value = serde_json::from_str(y_jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(y_row["at_ms"], late_night.timestamp_millis());
        assert_eq!(y_row["seq"], 2, "seq 按入队先后递增");
    }

    #[test]
    fn queue_never_drops_records_while_writer_is_slow() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path(), true);
        let trace = ProcessTrace::new(tmp.path());
        let resume = trace.pause_writer_for_test();
        // 原先队列 512 条满了就丢；现在不设上限，入队也不阻塞
        for i in 0..2000 {
            trace.record(
                "voice",
                "语音·s1·第1轮",
                "asr",
                &format!("步骤-{i}"),
                Value::Null,
            );
        }
        let big = vec![7u8; 3 * 1024 * 1024];
        let rel = trace
            .save_file("voice", "big", "wav", &big)
            .expect("大附件照样保存");
        let _ = resume.send(());
        trace.flush();
        let days = day_files(&trace);
        let jsonl = fs::read_to_string(days[0].join(TIMELINE_JSONL)).unwrap();
        assert_eq!(jsonl.lines().count(), 2000, "一条都不丢");
        assert!(!jsonl.contains("dropped_before"));
        assert_eq!(fs::read(trace.dir().join(&rel)).unwrap().len(), big.len());
        assert!(rel.contains("-000001-big.wav"), "附件编号不回绕：{rel}");
    }

    #[test]
    fn queued_attachment_memory_guard_refuses_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path(), true);
        let mut trace = ProcessTrace::new(tmp.path());
        Arc::get_mut(&mut trace.inner)
            .unwrap()
            .max_queued_file_bytes = 1000;
        let resume = trace.pause_writer_for_test();
        assert!(trace.save_file("voice", "a", "wav", &[1u8; 600]).is_some());
        assert!(
            trace.save_file("voice", "b", "wav", &[2u8; 600]).is_none(),
            "盘太慢、内存里待写附件超过高水位时不收"
        );
        let _ = resume.send(());
        trace.flush();
        assert!(
            trace.save_file("voice", "c", "wav", &[3u8; 5000]).is_some(),
            "写完释放后恢复；队列空时单个大附件照收（不限单个大小）"
        );
        trace.flush();
        let days = day_files(&trace);
        let readable = fs::read_to_string(days[0].join(TIMELINE_TXT)).unwrap();
        assert!(
            readable.contains("附件「b.wav」没保存：硬盘写得太慢"),
            "{readable}"
        );
    }

    #[test]
    fn attachment_write_failure_is_written_into_timeline() {
        let tmp = tempfile::tempdir().unwrap();
        enable(tmp.path(), true);
        let trace = ProcessTrace::new(tmp.path());
        let day = Local::now().format("%Y-%m-%d").to_string();
        let day_dir = trace.dir().join(&day);
        fs::create_dir_all(&day_dir).unwrap();
        // 附件目录的位置被一个普通文件占了：建目录必然失败
        fs::write(day_dir.join("voice-files"), b"x").unwrap();
        let rel = trace
            .save_file("voice", "utt", "wav", b"RIFF")
            .expect("路径已返回给调用方");
        trace.flush();
        let jsonl = fs::read_to_string(day_dir.join(TIMELINE_JSONL)).unwrap();
        let row: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(row["kind"], "attachment_failed");
        assert_eq!(row["file"], rel.as_str());
        let readable = fs::read_to_string(day_dir.join(TIMELINE_TXT)).unwrap();
        assert!(
            readable.contains(&format!("附件没写成：{rel}")),
            "{readable}"
        );
    }

    #[test]
    fn custom_directory_uses_dedicated_subdir_without_double_nesting_and_rejects_non_empty_unmarked()
     {
        let runtime = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join(".DS_Store"), b"finder").unwrap();
        let trace = ProcessTrace::new(runtime.path());
        trace.set_directory(Some(parent.path())).unwrap();
        let chosen = trace.dir();
        assert!(chosen.ends_with("moss-process-trace"));
        assert_eq!(
            chosen.parent().unwrap(),
            parent.path().canonicalize().unwrap()
        );
        let status = trace.directory_status();
        assert!(status.available);
        assert_eq!(status.custom_dir.as_deref(), Some(chosen.as_path()));

        // 直接选择已有的 moss-process-trace 子目录时不再重复套一层
        trace.set_directory(Some(&chosen)).unwrap();
        assert_eq!(trace.dir(), chosen);

        // 同名文件夹已有未标记的个人内容时拒绝接管，且原内容完好无损
        let busy_parent = tempfile::tempdir().unwrap();
        let busy_sub = busy_parent.path().join("moss-process-trace");
        fs::create_dir_all(&busy_sub).unwrap();
        fs::write(busy_sub.join("notes.txt"), b"keep").unwrap();
        let err = trace.set_directory(Some(busy_parent.path())).unwrap_err();
        assert!(err.contains("同名文件夹已有其他内容"), "{err}");
        assert_eq!(trace.dir(), chosen, "拒绝时保持原目录不变");
        assert_eq!(fs::read(busy_sub.join("notes.txt")).unwrap(), b"keep");

        // Windows 逐字前缀剥离与 UNC 拒绝规则
        assert_eq!(
            directory::normalize_canonical_str(r"\\?\D:\Logs\moss-process-trace").unwrap(),
            r"D:\Logs\moss-process-trace"
        );
        assert!(directory::normalize_canonical_str(r"\\?\UNC\server\share\logs").is_err());

        // settings.json 含 null / 空串时按默认处理，非法类型标为不可用而不回退写盘
        crate::settings_store::update(&crate::settings_store::settings_path(runtime.path()), |m| {
            m.insert(DIRECTORY_KEY.into(), Value::Null);
            Ok(())
        })
        .unwrap();
        let reset_by_null = ProcessTrace::new(runtime.path());
        assert!(reset_by_null.directory_status().custom_dir.is_none());
        assert!(reset_by_null.dir().ends_with(TRACE_DIR));

        crate::settings_store::update(&crate::settings_store::settings_path(runtime.path()), |m| {
            m.insert(DIRECTORY_KEY.into(), serde_json::json!(42));
            Ok(())
        })
        .unwrap();
        let corrupt = ProcessTrace::new(runtime.path());
        assert!(!corrupt.directory_status().available);
        assert!(corrupt.directory_status().custom_dir.is_some());

        // settings.json 被写入 UNC 网络共享或含 `..` 路径段时，check_root 入口直接拒绝，不发起阻塞 SMB / 磁盘元数据访问
        for unsafe_path in [
            r"\\server\share\moss-process-trace",
            "/tmp/a/../moss-process-trace",
        ] {
            assert!(trace.set_directory(Some(Path::new(unsafe_path))).is_err());
            crate::settings_store::update(
                &crate::settings_store::settings_path(runtime.path()),
                |m| {
                    m.insert(DIRECTORY_KEY.into(), serde_json::json!(unsafe_path));
                    Ok(())
                },
            )
            .unwrap();
            let rejected = ProcessTrace::new(runtime.path());
            let st = rejected.directory_status();
            assert!(!st.available, "{unsafe_path}");
            assert!(st.error.is_some(), "{unsafe_path}");
        }
    }

    #[test]
    fn scoped_keeps_attachment_and_record_in_same_dir_and_old_dir_is_not_swept() {
        let runtime = tempfile::tempdir().unwrap();
        let next_parent = tempfile::tempdir().unwrap();
        enable(runtime.path(), true);
        let trace = ProcessTrace::new(runtime.path());
        let old_dir = trace.dir();

        // 旧目录里放一个切换时尚未过期的日期目录，验证切走后跨过 30 天也不再被新目录自动清理
        let old_history_day = (Local::now().date_naive() - chrono::Days::new(10))
            .format("%Y-%m-%d")
            .to_string();
        let expired_old = old_dir.join(&old_history_day).join("voice-files");
        fs::create_dir_all(&expired_old).unwrap();
        fs::write(expired_old.join("old.wav"), b"history").unwrap();

        let scoped = trace.scoped();
        let rel = scoped
            .save_file("voice", "user", "wav", b"PCM-OLD")
            .unwrap();
        // 在附件入队后、时间线入队前切换目录
        trace.set_directory(Some(next_parent.path())).unwrap();
        let new_dir = trace.dir();
        assert_ne!(old_dir, new_dir);

        scoped.record(
            "voice",
            "语音·2233·第1轮",
            "user_audio",
            "旧轮次录音",
            serde_json::json!({"file": rel}),
        );
        trace.record(
            "voice",
            "语音·2233·第2轮",
            "asr",
            "切换后的新记录",
            Value::Null,
        );
        trace.flush();

        // 同一 scoped 下的附件与记录都完整留在旧目录，新记录落在新目录
        assert_eq!(fs::read(old_dir.join(&rel)).unwrap(), b"PCM-OLD");
        let old_day = old_dir.join(Local::now().format("%Y-%m-%d").to_string());
        let old_jsonl = fs::read_to_string(old_day.join(TIMELINE_JSONL)).unwrap();
        assert!(old_jsonl.contains("user_audio") && !old_jsonl.contains("切换后的新记录"));
        let new_day = new_dir.join(Local::now().format("%Y-%m-%d").to_string());
        let new_jsonl = fs::read_to_string(new_day.join(TIMELINE_JSONL)).unwrap();
        assert!(new_jsonl.contains("切换后的新记录") && !new_jsonl.contains("user_audio"));

        // 40 天后到期清理：新目录记录被清理，旧目录记录原封不动保留
        trace.sweep_at(Local::now().date_naive() + chrono::Days::new(40));
        assert!(!new_day.join(TIMELINE_JSONL).exists());
        assert!(old_day.join(TIMELINE_JSONL).exists());
        assert!(
            expired_old.join("old.wav").exists(),
            "旧目录不再参与新目录的到期清理"
        );
    }

    #[test]
    fn clear_and_sweep_preserve_unrelated_files_unmarked_dates_and_symlinks() {
        let runtime = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::write(external.path().join(" precious.txt"), b"do-not-delete").unwrap();

        enable(runtime.path(), true);
        let trace = ProcessTrace::new(runtime.path());
        trace.set_directory(Some(parent.path())).unwrap();
        let root = trace.dir();
        trace.record("voice", "语音", "asr", "今天记录", Value::Null);
        let rel = trace.save_file("voice", "clip", "wav", b"WAV").unwrap();
        trace.flush();
        assert!(root.join(&rel).exists());

        // 用户放入根目录的文件、未标记的日期文件夹、当天目录里的个人备注都不应被删
        fs::write(root.join("readme.txt"), b"user-root").unwrap();
        let unmarked_date = root.join("2026-01-01");
        fs::create_dir_all(&unmarked_date).unwrap();
        fs::write(unmarked_date.join("photo.jpg"), b"user-photo").unwrap();

        let today_str = Local::now().format("%Y-%m-%d").to_string();
        let today_dir = root.join(&today_str);
        fs::write(today_dir.join("keep.md"), b"user-note").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), today_dir.join("diag-files")).unwrap();

        trace.sweep_at(Local::now().date_naive());
        assert!(
            unmarked_date.join("photo.jpg").exists(),
            "未标记归属的日期目录绝不被到期清理误删"
        );

        trace.clear().unwrap();
        assert_eq!(
            trace.usage(),
            TraceUsage::default(),
            "保留用户无关文件与标记时，清空后记录占用必须归零"
        );
        assert!(!today_dir.join(TIMELINE_JSONL).exists());
        assert!(!today_dir.join(TIMELINE_TXT).exists());
        assert!(!root.join(&rel).exists());
        assert_eq!(fs::read(root.join("readme.txt")).unwrap(), b"user-root");
        assert_eq!(fs::read(today_dir.join("keep.md")).unwrap(), b"user-note");
        assert_eq!(
            fs::read(external.path().join(" precious.txt")).unwrap(),
            b"do-not-delete",
            "清空不得沿符号链接删除外部目录内容"
        );

        #[cfg(unix)]
        {
            let link_parent = tempfile::tempdir().unwrap();
            let real_target = tempfile::tempdir().unwrap();
            let linked = link_parent.path().join("linked");
            std::os::unix::fs::symlink(real_target.path(), &linked).unwrap();
            let sub_under_link = real_target.path().join("sub");
            fs::create_dir_all(&sub_under_link).unwrap();
            assert!(
                trace.set_directory(Some(&linked.join("sub"))).is_err(),
                "祖先含符号链接的路径必须拒绝"
            );
        }
    }

    #[test]
    fn unavailable_custom_directory_pauses_without_fallback_and_resumes_on_reconnect() {
        let runtime = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        enable(runtime.path(), true);
        let trace = ProcessTrace::new(runtime.path());
        trace.set_directory(Some(parent.path())).unwrap();
        let chosen = trace.dir();
        let unplugged = chosen.with_file_name("unplugged-drive");

        fs::rename(&chosen, &unplugged).unwrap();
        let status = trace.directory_status();
        assert!(!status.available);
        assert_eq!(status.dir, chosen);
        trace.record("voice", "语音", "asr", "拔盘期间的记录", Value::Null);
        trace.flush();
        assert!(!chosen.exists(), "不得自动重建缺失的自选目录");
        assert!(
            !runtime.path().join(TRACE_DIR).exists(),
            "不得偷偷回退到默认运行目录"
        );

        fs::rename(&unplugged, &chosen).unwrap();
        assert!(trace.directory_status().available);
        trace.record("voice", "语音", "asr", "接回硬盘后的记录", Value::Null);
        trace.flush();
        let day = chosen.join(Local::now().format("%Y-%m-%d").to_string());
        let txt = fs::read_to_string(day.join(TIMELINE_TXT)).unwrap();
        assert!(!txt.contains("拔盘期间的记录"), "中断期间内容不补回：{txt}");
        assert!(txt.contains("接回硬盘后的记录"), "{txt}");
        assert!(txt.contains("此前有 1 条记录没能写入"), "{txt}");
    }
}
