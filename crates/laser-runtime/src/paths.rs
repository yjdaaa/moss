//! Windows 路径的 `\\?\` 逐字前缀。
//!
//! `std::fs::canonicalize` 在 Windows 上返回带 `\\?\` 前缀的路径（如 `\\?\C:\Users\…`）。程序自己读写没问题，
//! 但显示给用户很怪，资源管理器（`explorer.exe`）也打不开它。规范化之后统一去掉：普通盘符变回 `C:\…`，
//! 网络共享 `\\?\UNC\服务器\共享\…` 变回 `\\服务器\共享\…`。去掉后超过 260 个字符（老程序的路径上限）的保持原样。

use std::path::PathBuf;

/// 老 Windows 程序认的最长路径（含结尾空字符）：去掉前缀后达到它就保留前缀，免得反而打不开
#[cfg(any(windows, test))]
const MAX_PATH: usize = 260;

/// 去掉 Windows 的 `\\?\` 逐字前缀；不是 Windows 时原样返回。
pub fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(stripped) = path.to_str().and_then(strip_verbatim) {
            return PathBuf::from(stripped);
        }
    }
    path
}

/// 能去掉前缀时返回去掉后的写法：只认 `\\?\X:\…`（X 为字母）与 `\\?\UNC\…`，其余（设备路径等）不动。
#[cfg(any(windows, test))]
pub(crate) fn strip_verbatim(raw: &str) -> Option<String> {
    let rest = raw.strip_prefix(r"\\?\")?;
    let stripped = if let Some(share) = rest.strip_prefix(r"UNC\") {
        format!(r"\\{share}")
    } else {
        let bytes = rest.as_bytes();
        let drive = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\';
        if !drive {
            return None;
        }
        rest.to_owned()
    };
    (stripped.chars().count() < MAX_PATH).then_some(stripped)
}

#[cfg(test)]
mod tests {
    use super::strip_verbatim;

    #[test]
    fn verbatim_prefix_is_removed_from_drive_and_share_paths_only() {
        assert_eq!(
            strip_verbatim(r"\\?\C:\Users\me\AppData\Roaming\moss").as_deref(),
            Some(r"C:\Users\me\AppData\Roaming\moss")
        );
        assert_eq!(
            strip_verbatim(r"\\?\UNC\nas\share\moss").as_deref(),
            Some(r"\\nas\share\moss")
        );
        // 没有前缀、设备路径、去掉后超长的都不动
        assert_eq!(strip_verbatim(r"C:\moss"), None);
        assert_eq!(strip_verbatim(r"\\?\Volume{1234}\moss"), None);
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(strip_verbatim(&long), None);
    }
}
