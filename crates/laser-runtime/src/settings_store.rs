//! 运行目录 `settings.json` 的唯一读写入口（2026-11-21 第三批重构 3A）。
//!
//! 激光后端（`laser-api`）、语音中心（`moss-voice`）、启动编排（`laser-host`）和桌面壳的导出设置都写同一份
//! `settings.json`。此前各包各写一套“读 → 改 → 写”，移动端导出设置还绕过了进程内锁，可能和
//! `/api/settings` 并发保存时互相覆盖。现在统一走这里：
//!
//! - [`update`]：持进程内全局锁读取最新文件、修改、先写临时文件再改名；非空但无法解析为 JSON 对象的
//!   文件一律拒绝覆盖，修改闭包返回错误时不写盘；需要读完再决定写不写的调用方用 [`lock`]；
//! - [`read`]：宽松读取（缺失、损坏或不是对象时返回空表），只给“读不到就按默认值”的场景用；
//! - [`read_checked`]：严格读取，供需要区分“没有文件”和“文件损坏”的调用方使用。
//!
//! 写入是原子替换，读取方不需要持锁也不会读到半截文件。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// 设置文件名。
pub const SETTINGS_FILE_NAME: &str = "settings.json";

/// 设置表（`settings.json` 顶层对象）。
pub type SettingsMap = serde_json::Map<String, serde_json::Value>;

/// 非空设置文件无法解析为 JSON 对象时的错误文本（调用方和测试依赖这句原文）。
pub const CORRUPT_SETTINGS_ERROR: &str = "settings.json 格式损坏，拒绝覆盖";

/// 运行目录下设置文件的路径。
pub fn settings_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SETTINGS_FILE_NAME)
}

/// 宽松读取：文件缺失、读不了、损坏或顶层不是对象时返回空表。
pub fn read(settings_file: &Path) -> SettingsMap {
    std::fs::read(settings_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// 严格读取：文件缺失或只有空白时返回空表；非空但不是 JSON 对象时返回 [`CORRUPT_SETTINGS_ERROR`]。
pub fn read_checked(settings_file: &Path) -> Result<SettingsMap, String> {
    match std::fs::read(settings_file) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(SettingsMap::new()),
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::Object(map)) => Ok(map),
            _ => Err(CORRUPT_SETTINGS_ERROR.to_owned()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SettingsMap::new()),
        Err(error) => Err(format!("读取 settings.json 失败：{error}")),
    }
}

/// 持锁读 → 改 → 写。`change` 返回错误时不写盘；读写失败的文本原样返回。
pub fn update<R>(
    settings_file: &Path,
    change: impl FnOnce(&mut SettingsMap) -> Result<R, String>,
) -> Result<R, String> {
    let lock = lock(settings_file);
    let mut root = lock.read_checked()?;
    let out = change(&mut root)?;
    lock.write(&root)?;
    Ok(out)
}

/// 取得进程内设置写锁。需要“读完决定不写”（如无需迁移、内容未变）或自定义错误类型的调用方
/// 用它在锁内 [`SettingsLock::read_checked`] / [`SettingsLock::write`]；写盘只能经由锁对象。
pub fn lock(settings_file: &Path) -> SettingsLock<'_> {
    static SETTINGS_FILE_LOCK: Mutex<()> = Mutex::new(());
    SettingsLock {
        settings_file,
        _guard: SETTINGS_FILE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    }
}

/// 持有中的设置写锁：同一进程里所有 `settings.json` 写入串行，避免并发保存互相覆盖对方刚写入的字段。
pub struct SettingsLock<'a> {
    settings_file: &'a Path,
    _guard: MutexGuard<'static, ()>,
}

impl SettingsLock<'_> {
    /// 锁内严格读取最新文件，见 [`read_checked`]。
    pub fn read_checked(&self) -> Result<SettingsMap, String> {
        read_checked(self.settings_file)
    }

    /// 锁内原子写回整张设置表。
    pub fn write(&self, root: &SettingsMap) -> Result<(), String> {
        write_atomic(self.settings_file, root)
    }
}

/// 先写同目录临时文件再改名；只经由 [`SettingsLock::write`] 调用，所以按进程号命名的临时文件不会被同进程并发写入撞车。
fn write_atomic(settings_file: &Path, root: &SettingsMap) -> Result<(), String> {
    if let Some(parent) = settings_file.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| format!("创建运行目录失败：{error}"))?;
    }
    let data = serde_json::to_vec_pretty(root).map_err(|error| error.to_string())?;
    let temp = settings_file.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&temp, data)
        .and_then(|()| std::fs::rename(&temp, settings_file))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("写入 settings.json 失败：{error}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn update_keeps_other_keys_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = settings_path(dir.path());
        std::fs::write(&file, r#"{"keep": 1, "voice": {"mode": "builtin"}}"#).unwrap();
        update(&file, |root| {
            root.insert("export".into(), json!({"ask_each_time": true}));
            Ok(())
        })
        .unwrap();
        let root = read(&file);
        assert_eq!(root["keep"], json!(1));
        assert_eq!(root["voice"]["mode"], json!("builtin"));
        assert_eq!(root["export"]["ask_each_time"], json!(true));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != SETTINGS_FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件：{leftovers:?}");
    }

    #[test]
    fn corrupt_or_non_object_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let file = settings_path(dir.path());
        for original in ["{broken", "[1, 2]", "\"text\""] {
            std::fs::write(&file, original).unwrap();
            let error = update(&file, |root| {
                root.insert("x".into(), json!(1));
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error, CORRUPT_SETTINGS_ERROR);
            assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
            assert!(read(&file).is_empty());
        }
    }

    #[test]
    fn missing_or_blank_file_starts_empty_and_parent_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let file = settings_path(&dir.path().join("nested"));
        assert!(read_checked(&file).unwrap().is_empty());
        update(&file, |root| {
            root.insert("a".into(), json!(true));
            Ok(())
        })
        .unwrap();
        assert_eq!(read(&file)["a"], json!(true));

        std::fs::write(&file, "  \n").unwrap();
        assert!(read_checked(&file).unwrap().is_empty());
    }

    #[test]
    fn failed_change_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = settings_path(dir.path());
        std::fs::write(&file, r#"{"a": 1}"#).unwrap();
        let error = update(&file, |root| {
            root.insert("a".into(), json!(2));
            Err::<(), _>("拒绝".to_owned())
        })
        .unwrap_err();
        assert_eq!(error, "拒绝");
        assert_eq!(read(&file)["a"], json!(1));
    }

    #[test]
    fn concurrent_updates_of_different_keys_all_survive() {
        let dir = tempfile::tempdir().unwrap();
        let file = settings_path(dir.path());
        let threads: Vec<_> = (0..16)
            .map(|index| {
                let file = file.clone();
                std::thread::spawn(move || {
                    update(&file, |root| {
                        root.insert(format!("k{index}"), json!(index));
                        Ok(())
                    })
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let root = read(&file);
        for index in 0..16 {
            assert_eq!(root[&format!("k{index}")], json!(index));
        }
    }
}
