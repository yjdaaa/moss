//! 完整过程记录（2026-11-22 用户决定，技术书 n105 / 融合技术书 r55）：语音对话与激光生成的中间过程写进
//! 运行目录 `logs/process-trace/`，方便事后核对“当时到底说了什么、模型怎么回的、生成了什么”。
//!
//! - **默认关闭**：只有 `settings.json` 顶层 `process_trace_enabled` 为 `true` 时才写；关着时
//!   [`ProcessTrace::record`] / [`ProcessTrace::save_file`] 什么都不做，行为与从前一致。开关每 2 秒最多读一次
//!   设置文件，改了不用重启。
//! - **目录**：`logs/process-trace/<本地日期>/<域>.jsonl` 一行一条事件（`voice` / `laser`），录音、合成语音、
//!   预览图等文件放在同日的 `<域>-files/` 下，事件里只写相对 `logs/process-trace/` 的路径。
//! - **清理**：按目录名日期保留最近 [`RETENTION_DAYS`] 天；总大小超过 [`MAX_TOTAL_BYTES`] 时从最旧的日期删起，
//!   只剩当天仍超限时停止保存文件（事件照写）。清理不看开关，关掉后旧记录照样到期删除。
//! - 调用方负责在写入前去掉密钥与令牌；本模块不读取、不记录任何凭据，也不发往网络。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// 单个附件上限（超过的不保存，只在事件里注明）。
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

const SETTING_CACHE: Duration = Duration::from_secs(2);
const SWEEP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// 当前记录占用（给设置页显示）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct TraceUsage {
    pub bytes: u64,
    pub days: u64,
}

struct Inner {
    runtime_dir: PathBuf,
    cached: Mutex<Option<(Instant, bool)>>,
    write_lock: Mutex<()>,
    last_sweep: Mutex<Option<Instant>>,
    over_cap: AtomicBool,
    seq: AtomicU64,
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
                write_lock: Mutex::new(()),
                last_sweep: Mutex::new(None),
                over_cap: AtomicBool::new(false),
                seq: AtomicU64::new(0),
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

    /// 追加一条事件。`fields` 为对象时并入顶层，否则放在 `data` 下。开关关着时什么也不做。
    pub fn record(&self, domain: &str, kind: &str, fields: Value) {
        if !self.enabled() {
            return;
        }
        let now = Local::now();
        let mut line = Map::new();
        line.insert(
            "at".into(),
            Value::String(now.format("%Y-%m-%dT%H:%M:%S%.3f%:z").to_string()),
        );
        line.insert("at_ms".into(), Value::from(now.timestamp_millis()));
        line.insert("domain".into(), Value::String(domain.to_owned()));
        line.insert("kind".into(), Value::String(kind.to_owned()));
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
        let Ok(mut text) = serde_json::to_string(&Value::Object(line)) else {
            return;
        };
        text.push('\n');
        let day_dir = self.dir().join(now.format("%Y-%m-%d").to_string());
        {
            let _guard = lock(&self.inner.write_lock);
            if fs::create_dir_all(&day_dir).is_err() {
                return;
            }
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(day_dir.join(format!("{}.jsonl", safe_name(domain))))
            {
                let _ = file.write_all(text.as_bytes());
            }
        }
        self.maybe_sweep();
    }

    /// 保存一个附件（录音、合成语音、预览图等），返回相对 `logs/process-trace/` 的路径。
    /// 开关关着、超过单文件上限或记录总量已满时返回 `None`。
    pub fn save_file(&self, domain: &str, stem: &str, ext: &str, bytes: &[u8]) -> Option<String> {
        if !self.enabled()
            || bytes.len() as u64 > MAX_FILE_BYTES
            || self.inner.over_cap.load(Ordering::Relaxed)
        {
            return None;
        }
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
        let dir = self.dir().join(&day).join(&folder);
        fs::create_dir_all(&dir).ok()?;
        fs::write(dir.join(&name), bytes).ok()?;
        self.maybe_sweep();
        Some(format!("{day}/{folder}/{name}"))
    }

    /// 复制一个已有文件作为附件（激光生成的预览图等），返回相对路径。
    pub fn copy_file(&self, domain: &str, stem: &str, source: &Path) -> Option<String> {
        if !self.enabled() {
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
        let mut usage = TraceUsage::default();
        for (_, path) in day_dirs(&self.dir()) {
            usage.days += 1;
            usage.bytes += dir_size(&path);
        }
        usage
    }

    /// 删除全部记录（设置页“清空过程记录”）。
    pub fn clear(&self) -> std::io::Result<()> {
        let _guard = lock(&self.inner.write_lock);
        let dir = self.dir();
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.inner.over_cap.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// 到期与超量清理（不看开关）：删掉早于保留期的日期目录，再从最旧的删起直到不超过总量上限。
    pub fn sweep(&self) {
        self.sweep_at(Local::now().date_naive(), MAX_TOTAL_BYTES);
    }

    fn sweep_at(&self, today: NaiveDate, max_bytes: u64) {
        let _guard = lock(&self.inner.write_lock);
        *lock(&self.inner.last_sweep) = Some(Instant::now());
        let mut days = day_dirs(&self.dir());
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
        while total > max_bytes && sizes.len() > 1 {
            let (_, path, size) = sizes.remove(0);
            let _ = fs::remove_dir_all(&path);
            total = total.saturating_sub(size);
        }
        self.inner
            .over_cap
            .store(total > max_bytes, Ordering::Relaxed);
    }

    fn maybe_sweep(&self) {
        let due = lock(&self.inner.last_sweep).is_none_or(|at| at.elapsed() >= SWEEP_INTERVAL);
        if due {
            self.sweep();
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
        trace.record("voice", "asr", serde_json::json!({"text": "你好"}));
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
            "asr",
            serde_json::json!({"session_id": "s1", "raw": "你好"}),
        );
        trace.record("laser", "generate", serde_json::json!("plain"));
        let rel = trace
            .save_file("voice", "turn 1/用户", "wav", b"RIFF-data")
            .unwrap();
        let days = day_files(&trace);
        assert_eq!(days.len(), 1);
        let voice = fs::read_to_string(days[0].join("voice.jsonl")).unwrap();
        let line: Value = serde_json::from_str(voice.lines().next().unwrap()).unwrap();
        assert_eq!(line["domain"], "voice");
        assert_eq!(line["kind"], "asr");
        assert_eq!(line["raw"], "你好");
        assert!(line["at"].as_str().unwrap().contains('T'));
        let laser = fs::read_to_string(days[0].join("laser.jsonl")).unwrap();
        assert!(laser.contains("\"data\":\"plain\""));
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
    }
}
