use super::*;

/// 前端页面路由：刷新这些地址时返回 index.html，由前端决定显示哪一页（摄像头与语音中心只在桌面端显示）。
pub(crate) fn is_spa_route(route: &str) -> bool {
    matches!(
        route,
        "/draw" | "/material-lab" | "/voice" | "/camera" | "/twin"
    ) || route.starts_with("/draw/")
}

const PREVIEW_SUFFIXES: [&str; 9] = [
    ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp", ".gcode", ".nc", ".json",
];
const UPLOAD_SUFFIXES: [&str; 8] = [
    ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp", ".gcode", ".nc",
];
const GCODE_FILE_SUFFIXES: [&str; 2] = [".gcode", ".nc"];

pub fn resolve_preview_file(
    requested: &std::path::Path,
    allowed_roots: &[std::path::PathBuf],
) -> Result<std::path::PathBuf, String> {
    let suffix = requested
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    if !PREVIEW_SUFFIXES.contains(&suffix.as_str()) {
        return Err("不支持预览该文件类型".into());
    }

    // 1. 如果请求的是相对文件名或相对子路径，优先在允许的根目录列表中拼接查找
    for root in allowed_roots {
        let candidate = root.join(requested);
        if let Ok(resolved) = std::fs::canonicalize(&candidate)
            && let Ok(canonical_root) = std::fs::canonicalize(root)
            && resolved.starts_with(canonical_root)
        {
            return Ok(resolved);
        }
    }

    // 2. 如果请求的是完整路径，校验是否位于任一允许的根目录内
    if let Ok(resolved) = std::fs::canonicalize(requested)
        && allowed_roots.iter().any(|root| {
            std::fs::canonicalize(root)
                .ok()
                .is_some_and(|canonical_root| resolved.starts_with(canonical_root))
        })
    {
        return Ok(resolved);
    }

    Err("文件不存在或不在允许的预览目录内".into())
}

pub(crate) fn resolve_gcode_file(
    requested: &std::path::Path,
    allowed_roots: &[std::path::PathBuf],
) -> Result<std::path::PathBuf, String> {
    let suffix = requested
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    if !GCODE_FILE_SUFFIXES.contains(&suffix.as_str()) {
        return Err("不支持该刀路文件类型".into());
    }
    for root in allowed_roots {
        let candidate = root.join(requested);
        if let Ok(resolved) = std::fs::canonicalize(&candidate)
            && let Ok(canonical_root) = std::fs::canonicalize(root)
            && resolved.starts_with(canonical_root)
        {
            return Ok(resolved);
        }
    }
    if let Ok(resolved) = std::fs::canonicalize(requested)
        && allowed_roots.iter().any(|root| {
            std::fs::canonicalize(root)
                .ok()
                .is_some_and(|canonical_root| resolved.starts_with(canonical_root))
        })
    {
        return Ok(resolved);
    }
    Err("G-code 刀路文件不存在或不在允许目录内".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    pub path: std::path::PathBuf,
    pub filename: String,
    pub size: usize,
    pub suffix: String,
}

pub fn save_upload(
    filename: &str,
    encoded: &str,
    upload_root: &std::path::Path,
    unix_seconds: u64,
) -> Result<UploadResult, String> {
    let basename = filename.rsplit(['\\', '/']).next().unwrap_or(filename);
    let cleaned = basename
        .replace(['<', '>', ':', '"', '|', '?', '*'], "_")
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    let path = std::path::Path::new(&cleaned);
    let suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    if !UPLOAD_SUFFIXES.contains(&suffix.as_str()) {
        return Err("不支持上传该文件类型".into());
    }
    let raw = encoded
        .split_once(',')
        .map(|(_, value)| value)
        .unwrap_or(encoded);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| "上传内容不是合法 base64".to_string())?;
    if bytes.is_empty() {
        return Err("上传文件为空".into());
    }
    if bytes.len() > 20 * 1024 * 1024 {
        return Err("上传文件超出 20MB 限制".into());
    }
    if matches!(suffix.as_str(), ".gcode" | ".nc" | ".txt") && bytes.contains(&0) {
        return Err("文本刀路文件不能包含二进制空字节".into());
    }
    let digest = sha1::Sha1::digest(&bytes);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("upload")
        .chars()
        .take(32)
        .collect::<String>();
    let filename = format!(
        "{unix_seconds}_{}_{stem}{suffix}",
        &hex::encode(digest)[..10]
    );
    std::fs::create_dir_all(upload_root).map_err(|error| error.to_string())?;
    let destination = upload_root.join(&filename);
    std::fs::write(&destination, &bytes).map_err(|error| error.to_string())?;
    Ok(UploadResult {
        path: destination,
        filename,
        size: bytes.len(),
        suffix,
    })
}

/// 允许作为 `image=latest_upload` 图片源的后缀（不含 `.gcode` / `.nc` / `.txt`）。
pub const IMAGE_UPLOAD_SUFFIXES: &[&str] = &[".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp"];

/// `image=latest_upload` 采信最近上传图片的最长有效期（30 分钟，避免误刻很久之前的旧图）。
pub const LATEST_UPLOAD_MAX_AGE_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestUploadedImage {
    pub filename: String,
    pub display_name: String,
    pub uploaded_at: u64,
    pub age_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatestUploadError {
    NotFound,
    Expired {
        display_name: String,
        uploaded_at: u64,
        age_seconds: u64,
    },
}

fn parse_saved_upload_meta(filename: &str) -> Option<(u64, String)> {
    let (ts_part, rest) = filename.split_once('_')?;
    let ts = ts_part.parse::<u64>().ok()?;
    let (hash_part, display) = rest.split_once('_')?;
    if hash_part.len() == 10
        && hash_part.bytes().all(|b| b.is_ascii_hexdigit())
        && !display.is_empty()
    {
        Some((ts, display.to_owned()))
    } else {
        None
    }
}

/// 在 MOSS 上传目录 `upload_root` 内查找最近上传的一张普通图片文件（严格拒绝符号链接与子目录，
/// 仅限 [`IMAGE_UPLOAD_SUFFIXES`]，超时返回 [`LatestUploadError::Expired`]）。
pub fn find_latest_uploaded_image(
    upload_root: &std::path::Path,
    now_unix: u64,
    max_age_secs: u64,
) -> Result<LatestUploadedImage, LatestUploadError> {
    let entries = std::fs::read_dir(upload_root).map_err(|_| LatestUploadError::NotFound)?;
    let mut best: Option<(u64, String, String)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let suffix = path
            .extension()
            .and_then(|v| v.to_str())
            .map(|v| format!(".{}", v.to_ascii_lowercase()))
            .unwrap_or_default();
        if !IMAGE_UPLOAD_SUFFIXES.contains(&suffix.as_str()) {
            continue;
        }
        let (uploaded_at, display_name) = parse_saved_upload_meta(&filename).unwrap_or_else(|| {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (mtime, filename.clone())
        });
        let better = match &best {
            None => true,
            Some((best_ts, best_name, _)) => {
                uploaded_at > *best_ts || (uploaded_at == *best_ts && filename > *best_name)
            }
        };
        if better {
            best = Some((uploaded_at, filename, display_name));
        }
    }
    let Some((uploaded_at, filename, display_name)) = best else {
        return Err(LatestUploadError::NotFound);
    };
    let age_seconds = now_unix.saturating_sub(uploaded_at);
    if age_seconds > max_age_secs {
        return Err(LatestUploadError::Expired {
            display_name,
            uploaded_at,
            age_seconds,
        });
    }
    Ok(LatestUploadedImage {
        filename,
        display_name,
        uploaded_at,
        age_seconds,
    })
}

/// 将图片上传距今秒数整理为适合语音播报的中文相对时间。
pub fn format_uploaded_age_zh(age_seconds: u64) -> String {
    if age_seconds < 60 {
        if age_seconds <= 5 {
            "刚刚".into()
        } else {
            format!("{age_seconds} 秒前")
        }
    } else if age_seconds < 3600 {
        let mins = age_seconds / 60;
        let secs = age_seconds % 60;
        if secs == 0 {
            format!("{mins} 分钟前")
        } else {
            format!("{mins} 分 {secs} 秒前")
        }
    } else {
        let hours = age_seconds / 3600;
        let mins = (age_seconds % 3600) / 60;
        if mins == 0 {
            format!("{hours} 小时前")
        } else {
            format!("{hours} 小时 {mins} 分钟前")
        }
    }
}

pub(crate) fn safe_asset_path(path: &str) -> Option<String> {
    let decoded = percent_decode(path);
    if decoded.is_empty() || decoded.chars().any(|c| c == '\0' || c.is_control()) {
        return None;
    }
    // 先把反斜杠统一成正斜杠，再按路径组件检查，防止 Unix 上 `..\..\` 绕过 ParentDir 拦截
    let normalized = decoded.replace('\\', "/");
    let candidate = std::path::Path::new(&normalized);
    if candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(normalized)
}

pub(crate) fn embedded_content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "woff" | "woff2" => "font/woff2",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub(crate) fn content_type(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "json" => "application/json",
        "gcode" | "nc" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub fn exposed_path(
    path: &std::path::Path,
    runtime_root: &std::path::Path,
    loopback: bool,
) -> String {
    if loopback {
        path.strip_prefix(runtime_root)
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

pub fn embedded_fixture_is_valid() -> bool {
    FrontendFixture::get("index.html")
        .map(|file| {
            file.data
                .windows(b"<div id=\"root\"></div>".len())
                .any(|window| window == b"<div id=\"root\"></div>")
        })
        .unwrap_or(false)
}
