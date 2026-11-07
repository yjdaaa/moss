//! Shared read-only local-path safety for user-selected folders (NV14 music, NV19-05
//! knowledge base): explicit absolute local roots only, no symlink / junction / reparse point
//! anywhere in the ancestry, no volume, system, profile or credential directories, and opened
//! handles re-verified against the OS-reported final path.
use std::{
    fs::{self, Metadata},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsError {
    InvalidRoot,
    Unavailable,
    UnsafePath,
}

pub(crate) fn is_link_or_reparse(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// Check every existing component, not just the canonicalized final target.
pub(crate) fn no_link_ancestry(path: &Path) -> Result<(), FsError> {
    for component in path.ancestors() {
        #[cfg(target_os = "macos")]
        if component == Path::new("/var") || component == Path::new("/tmp") {
            // macOS Signed System Volume maps /var -> private/var and /tmp -> private/tmp;
            // user subpaths and canonical /private/... paths are still checked strictly.
            continue;
        }
        let meta = fs::symlink_metadata(component).map_err(|_| FsError::Unavailable)?;
        if is_link_or_reparse(&meta) {
            return Err(FsError::UnsafePath);
        }
    }
    Ok(())
}

pub(crate) fn sensitive_catalog_root(root: &Path) -> bool {
    // Avoid accidentally making the entire volume, a user profile root,
    // an operating-system directory, or credentials into a music library.
    if root.parent().is_none() {
        return true;
    }
    let forbidden_ancestors = [
        "windows",
        "program files",
        "program files (x86)",
        "programdata",
        "system volume information",
        "$recycle.bin",
        "system",
        "library",
        "applications",
        "etc",
        "proc",
        "sys",
        "dev",
        "bin",
        "sbin",
        ".trash",
        ".git",
        ".ssh",
    ];
    if root
        .ancestors()
        .filter_map(Path::file_name)
        .any(|part| forbidden_ancestors.contains(&part.to_string_lossy().to_lowercase().as_str()))
    {
        return true;
    }
    let leaf = root
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("")
        .to_lowercase();
    if matches!(
        leaf.as_str(),
        "users"
            | "home"
            | "volumes"
            | "appdata"
            | "local"
            | "roaming"
            | "locallow"
            | "documents and settings"
    ) {
        return true;
    }
    root.parent()
        .and_then(Path::file_name)
        .and_then(|part| part.to_str())
        .is_some_and(|part| matches!(part.to_lowercase().as_str(), "users" | "home" | "volumes"))
}
/// Prevent indexing credentials hidden inside an otherwise allowed music root.
pub(crate) fn sensitive_descendant(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                ".git"
                    | ".ssh"
                    | ".aws"
                    | ".config"
                    | ".gnupg"
                    | "appdata"
                    | "library"
                    | "keychains"
                    | ".trash"
                    | "credentials"
                    | "secrets"
                    | "private"
                    | "tokens"
                    | "keys"
            )
        })
}

pub(crate) fn valid_local_root(root: &Path) -> Result<PathBuf, FsError> {
    if !root.is_absolute()
        || sensitive_catalog_root(root)
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(FsError::InvalidRoot);
    }
    #[cfg(windows)]
    {
        // UNC, device paths and extended namespace prefixes are excluded:
        // music directories must be explicitly chosen local drive paths.
        let src = root.to_str().ok_or(FsError::InvalidRoot)?;
        if src.starts_with(r"\\") || src.starts_with("//") {
            return Err(FsError::InvalidRoot);
        }
    }
    no_link_ancestry(root)?;
    let canonical = root.canonicalize().map_err(|_| FsError::Unavailable)?;
    if !canonical.is_dir() {
        return Err(FsError::InvalidRoot);
    }
    no_link_ancestry(&canonical)?;
    if sensitive_catalog_root(&canonical) {
        return Err(FsError::UnsafePath);
    }
    Ok(canonical)
}

/// OS-verified opened-handle path, never just the caller-supplied pathname.
/// On unreviewed operating systems fail closed rather than guessing.
pub(crate) fn actual_handle_path(file: &std::fs::File) -> Result<PathBuf, FsError> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let mut chars = vec![0u16; 32768];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                chars.as_mut_ptr(),
                chars.len() as u32,
                0,
            )
        } as usize;
        if length == 0 || length >= chars.len() {
            return Err(FsError::UnsafePath);
        }
        let actual = PathBuf::from(OsString::from_wide(&chars[..length]));
        return actual.canonicalize().map_err(|_| FsError::UnsafePath);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let actual = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|_| FsError::UnsafePath)?;
        return actual.canonicalize().map_err(|_| FsError::UnsafePath);
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::{CStr, OsStr};
        use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
        const F_GETPATH: std::ffi::c_int = 50;
        const MAXPATHLEN: usize = 1024;
        unsafe extern "C" {
            fn fcntl(fd: std::ffi::c_int, cmd: std::ffi::c_int, ...) -> std::ffi::c_int;
        }
        let mut buf = [0u8; MAXPATHLEN];
        let rc = unsafe { fcntl(file.as_raw_fd(), F_GETPATH, buf.as_mut_ptr()) };
        if rc == -1 {
            return Err(FsError::UnsafePath);
        }
        let cstr = CStr::from_bytes_until_nul(&buf).map_err(|_| FsError::UnsafePath)?;
        if cstr.to_bytes().is_empty() {
            return Err(FsError::UnsafePath);
        }
        let actual = PathBuf::from(OsStr::from_bytes(cstr.to_bytes()));
        return actual.canonicalize().map_err(|_| FsError::UnsafePath);
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = file;
        Err(FsError::UnsafePath)
    }
}

/// Open read-only without following a replaced final link, then confirm the handle's real
/// path is exactly `canonical` and lies under `root`.
pub(crate) fn open_verified(canonical: &Path, root: &Path) -> Result<std::fs::File, FsError> {
    #[cfg(windows)]
    let opened = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT prevents a replaced final link
        // from quietly following another file during open.
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x0020_0000)
            .open(canonical)
            .map_err(|_| FsError::Unavailable)?
    };
    #[cfg(target_os = "macos")]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW (0x0100 on Darwin) refuses to follow a replaced final symlink.
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x0100)
            .open(canonical)
            .map_err(|_| FsError::Unavailable)?
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let opened = std::fs::File::open(canonical).map_err(|_| FsError::Unavailable)?;
    let final_path = actual_handle_path(&opened)?;
    if final_path != canonical || !final_path.starts_with(root) {
        return Err(FsError::UnsafePath);
    }
    Ok(opened)
}
