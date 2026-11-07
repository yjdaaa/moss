use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;

#[derive(Debug)]
pub struct RuntimeRoot {
    pub path: PathBuf,
    pub uploads: PathBuf,
    pub previews: PathBuf,
    pub generated_assets: PathBuf,
    pub database: PathBuf,
    _lock: File,
}

#[derive(Debug)]
pub enum RuntimeRootError {
    Io(std::io::Error),
    AlreadyLocked(PathBuf),
}

impl From<std::io::Error> for RuntimeRootError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl RuntimeRoot {
    /// 未显式指定时的默认运行时数据目录（不创建、不加锁）。
    ///
    /// - Windows：`%APPDATA%\moss`（与安装目录无关，覆盖升级、卸载重装都不会碰到数据）；
    /// - macOS：`~/Library/Application Support/moss`（避免双击 `.app` 时当前目录为只读 `/`）；
    /// - Linux / 桌面 Unix：`$XDG_DATA_HOME/moss`（绝对路径时）或 `~/.local/share/moss`；
    /// - 缺少对应用户目录环境变量时：回退到当前目录下的 `.runtime`。
    ///
    /// `resolve(None, None)` 与 `moss-host migrate` 的默认目标都用这个函数，保证两处一致。
    pub fn default_base() -> std::io::Result<PathBuf> {
        let cwd = std::env::current_dir()?;
        Ok(default_base_from(
            std::env::consts::OS,
            std::env::var_os("APPDATA").as_deref(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
            &cwd,
        ))
    }

    pub fn resolve(explicit: Option<&Path>, exe: Option<&Path>) -> Result<Self, RuntimeRootError> {
        let base = if let Some(path) = explicit {
            path.to_path_buf()
        } else if let Some(path) = exe {
            path.parent().unwrap_or(path).join(".runtime")
        } else {
            Self::default_base()?
        };
        fs::create_dir_all(&base)?;
        let path = fs::canonicalize(&base)?;
        for child in ["uploads", "previews", "generated_assets"] {
            fs::create_dir_all(path.join(child))?;
        }
        let lock_path = path.join("moss.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        lock.try_lock_exclusive()
            .map_err(|_| RuntimeRootError::AlreadyLocked(lock_path))?;
        Ok(Self {
            uploads: path.join("uploads"),
            previews: path.join("previews"),
            generated_assets: path.join("generated_assets"),
            database: path.join("moss.sqlite"),
            path,
            _lock: lock,
        })
    }

    pub fn exposed_path(&self, path: &Path, loopback: bool) -> String {
        if loopback {
            path.strip_prefix(&self.path)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string()
        }
    }
}

fn non_empty_path(raw: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    raw.filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn default_base_from(
    os: &str,
    appdata: Option<&std::ffi::OsStr>,
    xdg_data_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    current_dir: &Path,
) -> PathBuf {
    if let Some(appdata) = non_empty_path(appdata) {
        return appdata.join("moss");
    }
    match os {
        "macos" => match non_empty_path(home) {
            Some(home) => home
                .join("Library")
                .join("Application Support")
                .join("moss"),
            None => current_dir.join(".runtime"),
        },
        "linux" | "freebsd" | "openbsd" | "netbsd" | "dragonfly" => {
            if let Some(xdg) = non_empty_path(xdg_data_home).filter(|p| p.is_absolute()) {
                return xdg.join("moss");
            }
            match non_empty_path(home) {
                Some(home) => home.join(".local").join("share").join("moss"),
                None => current_dir.join(".runtime"),
            }
        }
        _ => current_dir.join(".runtime"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn explicit_root_derives_all_runtime_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = RuntimeRoot::resolve(Some(temp.path()), None).unwrap();
        assert!(root.path.is_absolute());
        assert_eq!(root.uploads, root.path.join("uploads"));
        assert!(root.database.ends_with("moss.sqlite"));

        let file = root.uploads.join("sample.png");
        assert_eq!(root.exposed_path(&file, true), "uploads/sample.png");
        assert_eq!(root.exposed_path(&file, false), "sample.png");
    }

    #[test]
    fn default_base_matches_platform_rule() {
        let base = RuntimeRoot::default_base().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let expected = default_base_from(
            std::env::consts::OS,
            std::env::var_os("APPDATA").as_deref(),
            std::env::var_os("XDG_DATA_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
            &cwd,
        );
        assert_eq!(base, expected);
    }

    #[test]
    fn default_base_resolves_windows_macos_linux_and_fallback() {
        let cwd = Path::new("/work/moss");
        assert_eq!(
            default_base_from(
                "windows",
                Some(OsStr::new(r"C:\Users\Alice\AppData\Roaming")),
                None,
                None,
                cwd
            ),
            PathBuf::from(r"C:\Users\Alice\AppData\Roaming").join("moss"),
        );
        assert_eq!(
            default_base_from("windows", None, None, Some(OsStr::new("/Users/alice")), cwd),
            cwd.join(".runtime"),
        );
        assert_eq!(
            default_base_from(
                "macos",
                None,
                None,
                Some(OsStr::new("/Users/alice")),
                Path::new("/")
            ),
            PathBuf::from("/Users/alice/Library/Application Support/moss"),
        );
        assert_eq!(
            default_base_from("macos", None, None, None, cwd),
            cwd.join(".runtime"),
        );
        let abs_xdg = if cfg!(windows) {
            r"C:\xdg-data"
        } else {
            "/custom/xdg"
        };
        assert_eq!(
            default_base_from(
                "linux",
                None,
                Some(OsStr::new(abs_xdg)),
                Some(OsStr::new("/home/alice")),
                cwd
            ),
            PathBuf::from(abs_xdg).join("moss"),
        );
        // XDG_DATA_HOME relative path is ignored per XDG spec, falling back to ~/.local/share/moss.
        assert_eq!(
            default_base_from(
                "linux",
                None,
                Some(OsStr::new("relative/xdg")),
                Some(OsStr::new("/home/alice")),
                cwd
            ),
            PathBuf::from("/home/alice")
                .join(".local")
                .join("share")
                .join("moss"),
        );
    }
}
