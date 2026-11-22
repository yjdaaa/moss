//! 完整过程记录（2026-11-22 用户决定，技术书 n105 / 融合技术书 r55）：语音对话与激光生成的中间过程写进
//! 运行目录 `logs/process-trace/`，方便事后核对“当时到底说了什么、模型怎么回的、生成了什么”。
//!
//! - **默认关闭**：只有 `settings.json` 顶层 `process_trace_enabled` 为 `true` 时才写；关着时
//!   [`ProcessTrace::record`] / [`ProcessTrace::save_file`] 什么都不做，行为与从前一致。开关每 2 秒最多读一次
//!   设置文件，改了不用重启。
//! - **目录**：`logs/process-trace/<本地日期>/` 下语音与激光按发生先后写进同一条时间线：`timeline.jsonl`
//!   一行一条完整记录（序号、所属流程、步骤类型、一句中文说明与细节），`timeline.txt` 一行一步可直接阅读；
//!   录音、合成语音、预览图等附件放在同日的 `<域>-files/` 下，记录里只写相对 `logs/process-trace/` 的路径。
//! - **清理**：按目录名日期保留最近 [`RETENTION_DAYS`] 天；总大小超过 [`MAX_TOTAL_BYTES`] 时从最旧的日期删起，
//!   只剩当天仍超限时停止保存文件（事件照写）。清理不看开关，关掉后旧记录照样到期删除。
//! - 调用方负责在写入前去掉密钥与令牌；本模块不读取、不记录任何凭据，也不发往网络。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use serde_json::{Map, Value};

/// `settings.json` 顶层开关键（默认关）。
pub const SETTING_KEY: &str = "process_trace_enabled";
/// 记录目录（相对运行目录）。
pub const TRACE_DIR: &str = "logs/process-trace";
/// 按日期保留的天数（含今天）。
pub const RETENTION_DAYS: i64 = 7;
/// 记录目录总大小上限。
pub const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// 只剩当天且超总上限时停存附件后，时间线文本额外允许的硬上限（防止 JSONL 无界撑爆磁盘）。
const MAX_TIMELINE_OVERFLOW_BYTES: u64 = 64 * 1024 * 1024;
/// 当天时间线：一行一条完整记录（机器读）。
pub const TIMELINE_JSONL: &str = "timeline.jsonl";
/// 当天时间线：一行一步的中文说明（人直接看）。
pub const TIMELINE_TXT: &str = "timeline.txt";
/// 单个附件上限（超过的不保存，只在事件里注明）。
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

const SETTING_CACHE: Duration = Duration::from_secs(2);
const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// 后台写入队列上限：满时丢弃记录而不阻塞急停或音频线程。
const WRITER_QUEUE_CAP: usize = 512;

/// 当前记录占用（给设置页显示）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TraceUsage {
    pub bytes: u64,
    pub days: u64,
}

enum WriteOp {
    Record {
        domain: String,
        flow: String,
        kind: String,
        step: String,
        fields: Value,
    },
    SaveFile {
        day: String,
        folder: String,
        name: String,
        bytes: Vec<u8>,
    },
    Sweep {
        today: NaiveDate,
        max_bytes: u64,
        ack: Option<SyncSender<()>>,
    },
    Clear {
        ack: SyncSender<std::io::Result<()>>,
    },
    Flush {
        ack: SyncSender<()>,
    },
}

struct Inner {
    runtime_dir: PathBuf,
    cached: Mutex<Option<(Instant, bool)>>,
    /// 后台单写线程通道（懒启动，关着时不建线程）；不反向持有 `Inner`，避免循环引用。
    writer: Mutex<Option<SyncSender<WriteOp>>>,
    over_cap: Arc<AtomicBool>,
    seq: AtomicU64,
    run_id: i64,
}

/// 完整过程记录器；克隆共享同一份状态。
#[derive(Clone)]
pub struct ProcessTrace {
    inner: Arc<Inner>,
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
        Self {
            inner: Arc::new(Inner {
                runtime_dir: runtime_dir.into(),
                cached: Mutex::new(None),
                writer: Mutex::new(None),
                over_cap: Arc::new(AtomicBool::new(false)),
                seq: AtomicU64::new(0),
                run_id: Local::now().timestamp_millis(),
            }),
        }
    }

    /// 记录目录 `<运行目录>/logs/process-trace`。
    pub fn dir(&self) -> PathBuf {
        self.inner.runtime_dir.join(TRACE_DIR)
    }

    /// 开关是否打开（读 `settings.json`，2 秒内复用上次结果）。
    pub fn enabled(&self) -> bool {
        let mut cached = lock(&self.inner.cached);
        if let Some((at, value)) = *cached
            && at.elapsed() < SETTING_CACHE
        {
            return value;
        }
        let value = read_setting(&self.inner.runtime_dir);
        *cached = Some((Instant::now(), value));
        value
    }

    /// 设置页刚保存过开关时调用，下一次 [`Self::enabled`] 立即重新读取。
    pub fn invalidate(&self) {
        *lock(&self.inner.cached) = None;
    }

    fn writer_tx(&self) -> Option<SyncSender<WriteOp>> {
        let mut slot = lock(&self.inner.writer);
        if let Some(tx) = slot.as_ref() {
            return Some(tx.clone());
        }
        let (tx, rx) = mpsc::sync_channel::<WriteOp>(WRITER_QUEUE_CAP);
        let dir = self.dir();
        let run_id = self.inner.run_id;
        let over_cap = self.inner.over_cap.clone();
        std::thread::Builder::new()
            .name("moss-process-trace".into())
            .spawn(move || writer_loop(rx, dir, run_id, over_cap))
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
        let Some(tx) = self.writer_tx() else {
            return;
        };
        // 非阻塞入队，防止慢盘或清理阻塞急停 0x18 或异步语音线程
        let _ = tx.try_send(WriteOp::Record {
            domain: domain.to_owned(),
            flow: flow.to_owned(),
            kind: kind.to_owned(),
            step: step.to_owned(),
            fields,
        });
    }

    /// 保存一个附件（录音、合成语音、预览图等），返回相对 `logs/process-trace/` 的路径。
    /// 开关关着、超过单文件上限、记录总量已满或写入队列满时返回 `None`（绝不阻塞调用方）。
    pub fn save_file(&self, domain: &str, stem: &str, ext: &str, bytes: &[u8]) -> Option<String> {
        if !self.enabled()
            || bytes.len() as u64 > MAX_FILE_BYTES
            || self.inner.over_cap.load(Ordering::Relaxed)
        {
            return None;
        }
        let tx = self.writer_tx()?;
        let now = Local::now();
        let day = now.format("%Y-%m-%d").to_string();
        let folder = format!("{}-files", safe_name(domain));
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed) % 1000;
        let name = format!(
            "{}-{seq:03}-{}.{}",
            now.format("%H%M%S%.3f").to_string().replace('.', ""),
            safe_name(stem),
            safe_name(ext)
        );
        let rel = format!("{day}/{folder}/{name}");
        tx.try_send(WriteOp::SaveFile {
            day,
            folder,
            name,
            bytes: bytes.to_vec(),
        })
        .ok()?;
        Some(rel)
    }

    /// 复制一个已有文件作为附件（激光生成的预览图等），返回相对路径。
    pub fn copy_file(&self, domain: &str, stem: &str, source: &Path) -> Option<String> {
        if !self.enabled() || self.inner.over_cap.load(Ordering::Relaxed) {
            return None;
        }
        let size = fs::metadata(source).ok()?.len();
        if size > MAX_FILE_BYTES {
            return None;
        }
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("bin")
            .to_owned();
        let bytes = fs::read(source).ok()?;
        self.save_file(domain, stem, &ext, &bytes)
    }

    /// 当前占用：总字节数与日期目录数。
    pub fn usage(&self) -> TraceUsage {
        self.flush();
        let mut usage = TraceUsage::default();
        for (_, path) in day_dirs(&self.dir()) {
            usage.days += 1;
            usage.bytes += dir_size(&path);
        }
        usage
    }

    /// 删除全部记录（设置页“清空过程记录”）。
    pub fn clear(&self) -> std::io::Result<()> {
        let tx = {
            let slot = lock(&self.inner.writer);
            if let Some(tx) = slot.as_ref() {
                Some(tx.clone())
            } else {
                // 尚未启动后台写线程时持锁直接清理，避免并发首写插入
                return clear_dir(&self.dir(), &self.inner.over_cap);
            }
        };
        if let Some(tx) = tx {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            if tx.send(WriteOp::Clear { ack: ack_tx }).is_ok()
                && let Ok(res) = ack_rx.recv()
            {
                return res;
            }
        }
        clear_dir(&self.dir(), &self.inner.over_cap)
    }

    /// 到期与超量清理（不看开关）：删掉早于保留期的日期目录，再从最旧的删起直到不超过总量上限。
    pub fn sweep(&self) {
        self.sweep_at(Local::now().date_naive(), MAX_TOTAL_BYTES);
    }

    fn sweep_at(&self, today: NaiveDate, max_bytes: u64) {
        let tx = {
            let slot = lock(&self.inner.writer);
            if let Some(tx) = slot.as_ref() {
                Some(tx.clone())
            } else {
                let _ = sweep_on_disk(&self.dir(), &self.inner.over_cap, today, max_bytes);
                return;
            }
        };
        if let Some(tx) = tx {
            let (ack_tx, ack_rx) = mpsc::sync_channel(1);
            if tx
                .send(WriteOp::Sweep {
                    today,
                    max_bytes,
                    ack: Some(ack_tx),
                })
                .is_ok()
            {
                let _ = ack_rx.recv();
                return;
            }
        }
        let _ = sweep_on_disk(&self.dir(), &self.inner.over_cap, today, max_bytes);
    }
}

fn writer_loop(rx: mpsc::Receiver<WriteOp>, root: PathBuf, run_id: i64, over_cap: Arc<AtomicBool>) {
    let mut step_seq = 0u64;
    let mut last_sweep: Option<Instant> = None;
    let mut last_sweep_date: Option<NaiveDate> = None;
    let mut approx_bytes = 0u64;

    let maybe_sweep = |last_sweep: &mut Option<Instant>,
                       last_sweep_date: &mut Option<NaiveDate>,
                       approx_bytes: &mut u64| {
        let today = Local::now().date_naive();
        let due = last_sweep.is_none_or(|at| at.elapsed() >= SWEEP_INTERVAL)
            || *last_sweep_date != Some(today)
            || (!over_cap.load(Ordering::Relaxed) && *approx_bytes > MAX_TOTAL_BYTES);
        if due {
            *approx_bytes = sweep_on_disk(&root, &over_cap, today, MAX_TOTAL_BYTES);
            *last_sweep = Some(Instant::now());
            *last_sweep_date = Some(today);
        }
    };

    while let Ok(op) = rx.recv() {
        match op {
            WriteOp::Record {
                domain,
                flow,
                kind,
                step,
                fields,
            } => {
                // 先检查到期/跨日清理，确保跨天后超限旧目录可被回收而不永久锁死
                maybe_sweep(&mut last_sweep, &mut last_sweep_date, &mut approx_bytes);
                // 只剩当天仍超总上限时停存附件；若时间线文本本身也超硬上限，停止写入防止磁盘撑爆
                if over_cap.load(Ordering::Relaxed)
                    && approx_bytes >= MAX_TOTAL_BYTES.saturating_add(MAX_TIMELINE_OVERFLOW_BYTES)
                {
                    continue;
                }
                let now = Local::now();
                step_seq += 1;
                let seq = step_seq;
                let mut line = Map::new();
                line.insert("seq".into(), Value::from(seq));
                line.insert("run".into(), Value::from(run_id));
                line.insert(
                    "at".into(),
                    Value::String(now.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()),
                );
                line.insert("at_ms".into(), Value::from(now.timestamp_millis()));
                line.insert("domain".into(), Value::String(domain));
                line.insert("flow".into(), Value::String(flow.clone()));
                line.insert("kind".into(), Value::String(kind));
                line.insert("step".into(), Value::String(step.clone()));
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
                    continue;
                };
                json.push('\n');
                let readable = format!(
                    "{} #{seq:06} [{}] {}\n",
                    now.format("%H:%M:%S%.3f"),
                    flow.replace(['\r', '\n'], " "),
                    step.replace(['\r', '\n'], " ")
                );
                let day_dir = root.join(now.format("%Y-%m-%d").to_string());
                if fs::create_dir_all(&day_dir).is_ok() {
                    for (name, text) in [(TIMELINE_JSONL, &json), (TIMELINE_TXT, &readable)] {
                        if let Ok(mut file) = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(day_dir.join(name))
                            && file.write_all(text.as_bytes()).is_ok()
                        {
                            approx_bytes = approx_bytes.saturating_add(text.len() as u64);
                        }
                    }
                }
                maybe_sweep(&mut last_sweep, &mut last_sweep_date, &mut approx_bytes);
            }
            WriteOp::SaveFile {
                day,
                folder,
                name,
                bytes,
            } => {
                maybe_sweep(&mut last_sweep, &mut last_sweep_date, &mut approx_bytes);
                if over_cap.load(Ordering::Relaxed) {
                    continue;
                }
                let dir = root.join(&day).join(&folder);
                if fs::create_dir_all(&dir).is_ok() && fs::write(dir.join(&name), &bytes).is_ok() {
                    approx_bytes = approx_bytes.saturating_add(bytes.len() as u64);
                }
                maybe_sweep(&mut last_sweep, &mut last_sweep_date, &mut approx_bytes);
            }
            WriteOp::Sweep {
                today,
                max_bytes,
                ack,
            } => {
                approx_bytes = sweep_on_disk(&root, &over_cap, today, max_bytes);
                last_sweep = Some(Instant::now());
                last_sweep_date = Some(today);
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            WriteOp::Clear { ack } => {
                let res = clear_dir(&root, &over_cap);
                if res.is_ok() {
                    approx_bytes = 0;
                    last_sweep = None;
                    last_sweep_date = None;
                }
                let _ = ack.send(res);
            }
            WriteOp::Flush { ack } => {
                let _ = ack.send(());
            }
        }
    }
}

fn clear_dir(dir: &Path, over_cap: &AtomicBool) -> std::io::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    over_cap.store(false, Ordering::Relaxed);
    Ok(())
}

fn sweep_on_disk(root: &Path, over_cap: &AtomicBool, today: NaiveDate, max_bytes: u64) -> u64 {
    let mut days = day_dirs(root);
    days.sort_by_key(|(date, _)| *date);
    let oldest_kept = today - chrono::Days::new((RETENTION_DAYS - 1).max(0) as u64);
    days.retain(|(date, path)| {
        if *date < oldest_kept {
            let _ = fs::remove_dir_all(path);
            false
        } else {
            true
        }
    });
    let mut sizes: Vec<(NaiveDate, PathBuf, u64)> = days
        .into_iter()
        .map(|(date, path)| {
            let size = dir_size(&path);
            (date, path, size)
        })
        .collect();
    let mut total: u64 = sizes.iter().map(|(_, _, size)| size).sum();
    // 只有今天这一天允许在超限时保留；跨日后昨日超限目录应及时清理恢复记录能力
    while total > max_bytes
        && (sizes.len() > 1 || sizes.first().is_some_and(|(d, _, _)| *d < today))
    {
        let (_, path, size) = sizes.remove(0);
        let _ = fs::remove_dir_all(&path);
        total = total.saturating_sub(size);
    }
    over_cap.store(total > max_bytes, Ordering::Relaxed);
    total
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

fn day_dirs(root: &Path) -> Vec<(NaiveDate, PathBuf)> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let name = entry.file_name();
            let date = NaiveDate::parse_from_str(name.to_str()?, "%Y-%m-%d").ok()?;
            Some((date, path))
        })
        .collect()
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
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
        assert!(!trace.dir().exists());
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
    fn sweep_drops_expired_days_then_oldest_until_under_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let trace = ProcessTrace::new(tmp.path());
        let today = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        for (day, size) in [
            ("2026-11-10", 10usize),
            ("2026-11-15", 10),
            ("2026-11-16", 300),
            ("2026-11-20", 300),
            ("2026-11-22", 300),
        ] {
            let dir = trace.dir().join(day).join("voice-files");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("a.wav"), vec![0u8; size]).unwrap();
        }
        fs::create_dir_all(trace.dir().join("not-a-date")).unwrap();
        trace.sweep_at(today, 700);
        let mut kept: Vec<String> = day_dirs(&trace.dir())
            .into_iter()
            .map(|(d, _)| d.to_string())
            .collect();
        kept.sort();
        // 11-10 / 11-15 超过 7 天（11-16 ~ 11-22 保留），再按总量从最旧的 11-16 删
        assert_eq!(kept, ["2026-11-20", "2026-11-22"]);
        assert!(trace.dir().join("not-a-date").exists(), "不认识的目录不动");
        assert!(!trace.inner.over_cap.load(Ordering::Relaxed));

        // 只剩当天仍超限：不删当天，停止保存附件
        trace.sweep_at(today, 100);
        assert_eq!(day_dirs(&trace.dir()).len(), 1);
        assert!(trace.inner.over_cap.load(Ordering::Relaxed));
        enable(tmp.path(), true);
        trace.invalidate();
        assert_eq!(trace.save_file("voice", "x", "wav", b"1"), None);

        // 跨日后昨日超限目录不再被当作“今天”保留，应被清理并解除超限锁
        let tomorrow = today + chrono::Days::new(1);
        trace.sweep_at(tomorrow, 100);
        assert!(day_dirs(&trace.dir()).is_empty());
        assert!(!trace.inner.over_cap.load(Ordering::Relaxed));
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
        let readable = fs::read_to_string(days[0].join(TIMELINE_TXT)).unwrap();
        assert_eq!(
            readable.lines().count(),
            40,
            "flow 含换行时 timeline.txt 仍保持一行一步"
        );
    }
}
