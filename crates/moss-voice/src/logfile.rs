//! 服务端日志（技术书 §9.4.3）：写入运行目录 `logs/xiaozhi-server.log`，
//! 每行先脱敏；超过上限时轮转为 `.1`、`.2`，共保留 3 份。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::redact::redact_line;

pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const DEFAULT_KEEP: usize = 3;

pub struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    lock: Mutex<()>,
}

impl RotatingLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_limits(path, DEFAULT_MAX_BYTES, DEFAULT_KEEP)
    }

    pub fn with_limits(path: impl Into<PathBuf>, max_bytes: u64, keep: usize) -> Self {
        Self {
            path: path.into(),
            max_bytes: max_bytes.max(1),
            keep: keep.max(1),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn rotated(&self, index: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    /// 追加一行（自动脱敏）。写日志失败不影响服务端运行，只返回错误供调用方忽略。
    pub fn write_line(&self, line: &str) -> std::io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let size = fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        if size >= self.max_bytes {
            self.rotate()?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let clean = redact_line(line.trim_end_matches(['\r', '\n']));
        writeln!(file, "{clean}")
    }

    fn rotate(&self) -> std::io::Result<()> {
        // keep=3：当前文件 + .1 + .2；最老的一份删除
        let oldest = self.rotated(self.keep - 1);
        if self.keep > 1 && oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for index in (1..self.keep.saturating_sub(1)).rev() {
            let from = self.rotated(index);
            if from.exists() {
                fs::rename(&from, self.rotated(index + 1))?;
            }
        }
        if self.keep > 1 {
            fs::rename(&self.path, self.rotated(1))?;
        } else {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    /// 最近 `count` 行（当前文件不足时补上一份），已脱敏。读取时再脱敏一次，防止旧版本写入的内容泄露。
    pub fn tail(&self, count: usize) -> Vec<String> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut lines: Vec<String> = Vec::new();
        for path in [self.rotated(1), self.path.clone()] {
            if let Ok(text) = fs::read_to_string(&path) {
                lines.extend(text.lines().map(redact_line));
            }
        }
        let skip = lines.len().saturating_sub(count);
        lines.split_off(skip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_redacted_and_rotated_keeping_three_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("xiaozhi-server.log");
        let log = RotatingLog::with_limits(&path, 64, 3);
        for index in 0..20 {
            log.write_line(&format!("line {index:02} api_key: sk-secret-{index}"))
                .unwrap();
        }
        assert!(path.exists());
        assert!(dir.path().join("logs/xiaozhi-server.log.1").exists());
        assert!(dir.path().join("logs/xiaozhi-server.log.2").exists());
        assert!(!dir.path().join("logs/xiaozhi-server.log.3").exists());
        for name in [
            "xiaozhi-server.log",
            "xiaozhi-server.log.1",
            "xiaozhi-server.log.2",
        ] {
            let text = fs::read_to_string(dir.path().join("logs").join(name)).unwrap();
            assert!(!text.contains("sk-secret"), "{name}: {text}");
            assert!(
                fs::metadata(dir.path().join("logs").join(name))
                    .unwrap()
                    .len()
                    <= 64 + 40
            );
        }
        let tail = log.tail(2);
        assert_eq!(tail.len(), 2);
        assert!(tail[1].starts_with("line 19"), "{tail:?}");
        assert!(tail.iter().all(|line| line.contains("api_key: ***")));
    }

    #[test]
    fn nv15_legacy_raw_bearer_is_redacted_on_tail_without_touching_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.log");
        std::fs::write(&path, b"Bearer synthetic-old-token\n").unwrap();
        let log = RotatingLog::new(&path);
        assert_eq!(log.tail(1), vec!["Bearer ***"]);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"Bearer synthetic-old-token\n"
        );
    }
    #[test]
    fn tail_of_missing_log_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = RotatingLog::new(dir.path().join("none.log"));
        assert!(log.tail(10).is_empty());
    }
}
