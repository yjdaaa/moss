use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::NaiveDate;

use super::{TIMELINE_JSONL, TIMELINE_TXT};

pub(super) const CUSTOM_DIR: &str = "moss-process-trace";
const ROOT_MARKER: &str = ".moss-process-trace";
const DAY_MARKER: &str = ".moss-process-trace-day";
const MARKER_CONTENT: &[u8] = b"moss-process-trace-v1\n";
const ATTACHMENT_DIRS: [&str; 3] = ["voice-files", "laser-files", "diag-files"];

pub(super) struct TraceDirectory {
    pub path: PathBuf,
    pub custom: bool,
}

impl TraceDirectory {
    pub fn new(path: PathBuf, custom: bool) -> Self {
        Self { path, custom }
    }

    pub fn selected(selected: &Path) -> io::Result<Self> {
        reject_unsafe_components(selected)?;
        check_ancestors_no_link(selected)?;
        let selected = normalize_canonical_path(selected.canonicalize()?)?;
        reject_unsafe_components(&selected)?;
        let path = if selected.file_name().is_some_and(|name| name == CUSTOM_DIR) {
            selected
        } else {
            selected.join(CUSTOM_DIR)
        };
        create_plain_dir(&path)?;
        claim_empty_dir(&path, ROOT_MARKER)?;
        let directory = Self::new(path, true);
        directory.check_available()?;
        Ok(directory)
    }

    pub fn status_error(&self) -> Option<String> {
        self.check_available().err().map(|error| error.to_string())
    }

    fn check_available(&self) -> io::Result<()> {
        if self.custom {
            self.check_root()?;
            probe_writable(&self.path)?;
            return self.check_today_if_exists();
        }
        // 关闭记录时不因查看设置而创建记录目录；只探测最近的现有父目录。
        self.check_root()?;
        let existing = self
            .path
            .ancestors()
            .find(|path| path.exists())
            .ok_or_else(|| io::Error::other("保存位置不存在"))?;
        plain_dir(existing)?;
        probe_writable(existing)?;
        self.check_today_if_exists()
    }

    fn check_today_if_exists(&self) -> io::Result<()> {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let day_path = self.path.join(day);
        match fs::symlink_metadata(&day_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
            Ok(metadata) if !is_plain_dir(&metadata) => {
                return Err(io::Error::other("当天记录目录不能是链接或普通文件"));
            }
            Ok(_) => {}
        }
        if self.custom {
            check_marker(&day_path.join(DAY_MARKER))?;
        }
        for name in [TIMELINE_JSONL, TIMELINE_TXT] {
            let file = day_path.join(name);
            match fs::symlink_metadata(&file) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
                Ok(metadata) if !metadata.is_file() || is_link(&metadata) => {
                    return Err(io::Error::other("时间线文件不能是链接或目录"));
                }
                Ok(_) => {}
            }
        }
        Ok(())
    }

    pub fn check_root(&self) -> io::Result<()> {
        if self.custom {
            reject_unsafe_components(&self.path)?;
            // 自定义路径已经规范化；父目录被换成链接后不再沿链接访问。
            for parent in self.path.ancestors() {
                plain_dir(parent)?;
            }
            check_marker(&self.path.join(ROOT_MARKER))
        } else {
            for path in [self.path.parent(), Some(self.path.as_path())]
                .into_iter()
                .flatten()
            {
                match fs::symlink_metadata(path) {
                    Ok(metadata) if is_plain_dir(&metadata) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Ok(_) => return Err(io::Error::other("记录目录不能是链接或普通文件")),
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }
    }

    pub fn prepare_day(&self, day: &str) -> io::Result<PathBuf> {
        self.check_root()?;
        if !self.custom {
            fs::create_dir_all(&self.path)?;
            self.check_root()?;
        }
        let path = self.path.join(day);
        create_plain_dir(&path)?;
        if self.custom {
            claim_empty_dir(&path, DAY_MARKER)?;
        }
        Ok(path)
    }

    pub fn write_attachment(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let rel = Path::new(rel);
        let mut components = rel.iter();
        // rel 完全由记录器生成：日期 / 域-files / 文件名，不接收外部路径。
        let day = components
            .next()
            .and_then(|part| part.to_str())
            .ok_or_else(|| io::Error::other("附件路径缺少日期"))?;
        let day_path = self.prepare_day(day)?;
        let folder = components
            .next()
            .ok_or_else(|| io::Error::other("附件路径缺少目录"))?;
        create_plain_dir(&day_path.join(folder))?;
        let path = self.path.join(rel);
        // 不覆盖已有文件，也不跟随同名链接。
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(bytes)
    }

    pub fn days(&self) -> Vec<(NaiveDate, PathBuf)> {
        if self.check_root().is_err() {
            return Vec::new();
        }
        day_dirs(&self.path)
            .into_iter()
            .filter(|(_, path)| !self.custom || check_marker(&path.join(DAY_MARKER)).is_ok())
            .collect()
    }

    pub fn clear_day(&self, path: &Path) -> io::Result<()> {
        self.check_root()?;
        plain_dir(path)?;
        if self.custom {
            check_marker(&path.join(DAY_MARKER))?;
        }
        // 根目录、用户额外放入的文件和未知子目录均保留。
        for name in [TIMELINE_JSONL, TIMELINE_TXT] {
            remove_plain_file(&path.join(name))?;
        }
        for name in ATTACHMENT_DIRS {
            let attachments = path.join(name);
            match fs::symlink_metadata(&attachments) {
                Ok(metadata) if is_plain_dir(&metadata) => fs::remove_dir_all(attachments)?,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let only_marker = fs::read_dir(path)?.all(|entry| {
            entry.is_ok_and(|entry| {
                let name = entry.file_name();
                name == DAY_MARKER || name == ".DS_Store"
            })
        });
        if only_marker {
            remove_plain_file(&path.join(DAY_MARKER))?;
            remove_plain_file(&path.join(".DS_Store"))?;
            match fs::remove_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// 拒绝相对路径、`.`/`..` 段以及 Windows UNC / 设备命名空间前缀（防止启动清理或状态查询触发阻塞式 SMB 访问）。
pub(super) fn reject_unsafe_components(path: &Path) -> io::Result<()> {
    let raw = path.to_string_lossy();
    if raw.starts_with(r"\\") || raw.starts_with("//") {
        return Err(io::Error::other(
            "不支持网络共享或设备路径，请选择本机硬盘文件夹",
        ));
    }
    if !path.is_absolute() {
        return Err(io::Error::other("保存位置必须是绝对路径"));
    }
    for comp in path.components() {
        if matches!(
            comp,
            std::path::Component::ParentDir | std::path::Component::CurDir
        ) {
            return Err(io::Error::other("保存位置不能包含相对跳转段"));
        }
    }
    Ok(())
}

fn check_ancestors_no_link(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        #[cfg(target_os = "macos")]
        if ancestor == Path::new("/var") || ancestor == Path::new("/tmp") {
            // macOS 只读系统卷将 /var -> private/var、/tmp -> private/tmp；canonicalize 后再按真实路径逐级校验
            continue;
        }
        plain_dir(ancestor)?;
    }
    Ok(())
}

/// Windows `canonicalize` 会附带 `\\?\` 逐字前缀，导致界面路径异常且 `explorer.exe` 无法直接打开；
/// 这里剥离常规盘符的 `\\?\` 前缀，并拒绝 `\\?\UNC\` 网络共享盘。
#[cfg(any(windows, test))]
pub(super) fn normalize_canonical_str(raw: &str) -> io::Result<String> {
    if raw.starts_with(r"\\?\UNC\") {
        return Err(io::Error::other("不支持网络共享路径，请选择本机硬盘文件夹"));
    }
    Ok(crate::paths::strip_verbatim(raw).unwrap_or_else(|| raw.to_owned()))
}

fn normalize_canonical_path(path: PathBuf) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        let raw = path
            .to_str()
            .ok_or_else(|| io::Error::other("保存位置包含无法识别的字符"))?;
        Ok(PathBuf::from(normalize_canonical_str(raw)?))
    }
    #[cfg(not(windows))]
    {
        Ok(path)
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn is_plain_dir(metadata: &Metadata) -> bool {
    metadata.is_dir() && !is_link(metadata)
}

fn plain_dir(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if is_plain_dir(&metadata) {
        Ok(())
    } else {
        Err(io::Error::other("记录目录不能是链接或普通文件"))
    }
}

fn create_plain_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => plain_dir(path),
        Err(error) => Err(error),
    }
}

fn check_marker(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if is_link(&metadata)
        || !metadata.is_file()
        || metadata.len() != MARKER_CONTENT.len() as u64
        || fs::read(path)? != MARKER_CONTENT
    {
        return Err(io::Error::other("该文件夹不是 MOSS 的过程记录目录"));
    }
    Ok(())
}

fn claim_empty_dir(path: &Path, marker: &str) -> io::Result<()> {
    let marker = path.join(marker);
    match fs::symlink_metadata(&marker) {
        Ok(_) => return check_marker(&marker),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() == ".DS_Store" {
            continue;
        }
        return Err(io::Error::other(
            "同名文件夹已有其他内容，请选择另一个保存位置",
        ));
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)?
        .write_all(MARKER_CONTENT)
}

fn probe_writable(path: &Path) -> io::Result<()> {
    static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);
    let probe = path.join(format!(
        ".moss-trace-probe-{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    let result = file.write_all(b"moss");
    drop(file);
    let removed = fs::remove_file(probe);
    result.and(removed)
}

pub(super) fn append_line(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || is_link(&metadata) => {
            return Err(io::Error::other("时间线文件不能是链接或目录"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(bytes)
}

fn remove_plain_file(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn day_dirs(root: &Path) -> Vec<(NaiveDate, PathBuf)> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            if !is_plain_dir(&fs::symlink_metadata(entry.path()).ok()?) {
                return None;
            }
            let name = entry.file_name();
            let name = name.to_str()?;
            let date = NaiveDate::parse_from_str(name, "%Y-%m-%d").ok()?;
            if date.to_string() != name {
                return None;
            }
            Some((date, entry.path()))
        })
        .collect()
}

/// 只统计 MOSS 自身记录文件（时间线与三个附件目录）的字节数，与 `clear_day` 清理范围一致；
/// 保留的用户无关文件、外部链接、`.DS_Store` 与 `.moss-process-trace-day` 标记均不计入占用。
pub(super) fn day_record_size(path: &Path) -> (bool, u64) {
    let mut has_records = false;
    let mut bytes = 0u64;
    for name in [TIMELINE_JSONL, TIMELINE_TXT] {
        if let Ok(metadata) = fs::symlink_metadata(path.join(name))
            && metadata.is_file()
            && !is_link(&metadata)
        {
            has_records = true;
            bytes += metadata.len();
        }
    }
    for name in ATTACHMENT_DIRS {
        let attachments = path.join(name);
        if let Ok(metadata) = fs::symlink_metadata(&attachments)
            && is_plain_dir(&metadata)
        {
            let sub = dir_size(&attachments);
            if sub > 0 {
                has_records = true;
                bytes += sub;
            }
        }
    }
    (has_records, bytes)
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match fs::symlink_metadata(entry.path()) {
            Ok(metadata) if is_plain_dir(&metadata) => dir_size(&entry.path()),
            Ok(metadata) if metadata.is_file() && !is_link(&metadata) => metadata.len(),
            _ => 0,
        })
        .sum()
}
