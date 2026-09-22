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
    pub fn resolve(explicit: Option<&Path>, exe: Option<&Path>) -> Result<Self, RuntimeRootError> {
        let base = if let Some(path) = explicit {
            path.to_path_buf()
        } else if let Some(path) = exe {
            path.parent().unwrap_or(path).join(".runtime")
        } else if let Ok(appdata) = std::env::var("APPDATA") {
            PathBuf::from(appdata).join("moss")
        } else {
            std::env::current_dir()?.join(".runtime")
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
