use super::*;

pub(crate) fn current_moss_version() -> &'static str {
    match option_env!("MOSS_RELEASE_VERSION") {
        Some(ver) if !ver.is_empty() => ver,
        _ => env!("CARGO_PKG_VERSION"),
    }
}
pub(crate) const DEFAULT_GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/yjdaaa/moss/releases";
const DEFAULT_GITHUB_RELEASES_PAGE: &str = "https://github.com/yjdaaa/moss/releases";

fn parse_semver_tuple(raw: &str) -> Option<(u64, u64, u64, String)> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    if trimmed.is_empty() {
        return None;
    }
    let (core, suffix) = match trimmed.split_once('-') {
        Some((c, s)) => (c, s.split('+').next().unwrap_or(s)),
        None => (trimmed.split('+').next().unwrap_or(trimmed), ""),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    Some((major, minor, patch, suffix.trim().to_owned()))
}

fn cmp_prerelease(candidate: &str, current: &str) -> std::cmp::Ordering {
    match (candidate.is_empty(), current.is_empty()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => {
            let mut c_iter = candidate.split('.');
            let mut cur_iter = current.split('.');
            loop {
                match (c_iter.next(), cur_iter.next()) {
                    (None, None) => return std::cmp::Ordering::Equal,
                    (Some(_), None) => return std::cmp::Ordering::Greater,
                    (None, Some(_)) => return std::cmp::Ordering::Less,
                    (Some(a), Some(b)) => {
                        let ord = match (a.parse::<u64>(), b.parse::<u64>()) {
                            (Ok(na), Ok(nb)) => na.cmp(&nb),
                            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                            (Err(_), Err(_)) => a.cmp(b),
                        };
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                }
            }
        }
    }
}

/// 比较 `candidate` 是否比 `current` 更新。
pub fn is_newer_version(candidate: &str, current: &str) -> bool {
    let Some((c_maj, c_min, c_pat, c_pre)) = parse_semver_tuple(candidate) else {
        return false;
    };
    let Some((cur_maj, cur_min, cur_pat, cur_pre)) = parse_semver_tuple(current) else {
        return false;
    };
    match (c_maj, c_min, c_pat).cmp(&(cur_maj, cur_min, cur_pat)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => cmp_prerelease(&c_pre, &cur_pre).is_gt(),
    }
}

/// 从指定 Releases JSON API 检查是否有新版本（禁用重定向、6s 超时）。
pub fn check_github_release_update(
    current_version: &str,
    include_prerelease: bool,
    api_url: &str,
) -> Result<serde_json::Value, String> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(6))
        .user_agent(format!("MOSS-Desktop/{current_version}"))
        .build()
        .map_err(|e| format!("初始化更新检查客户端失败：{e}"))?;
    let response = client
        .get(api_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("无法连接更新服务器：{e}"))?;
    let status = response.status().as_u16();
    if status == 404 {
        return Ok(serde_json::json!({
            "current_version": current_version,
            "latest_version": current_version,
            "has_update": false,
            "prerelease": false,
            "published_at": "",
            "release_url": DEFAULT_GITHUB_RELEASES_PAGE,
            "notes": "",
            "checked_at": unix_seconds(),
        }));
    }
    if !(200..300).contains(&status) {
        return Err(format!("更新服务器返回 HTTP {status}"));
    }
    let payload: serde_json::Value = response
        .json()
        .map_err(|e| format!("解析版本列表失败：{e}"))?;
    let releases = match payload.as_array() {
        Some(arr) => arr.clone(),
        None if payload.is_object() => vec![payload],
        None => Vec::new(),
    };
    let chosen = releases.into_iter().find(|item| {
        let draft = item.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
        if draft {
            return false;
        }
        let pre = item
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        include_prerelease || !pre
    });
    let Some(release) = chosen else {
        return Ok(serde_json::json!({
            "current_version": current_version,
            "latest_version": current_version,
            "has_update": false,
            "prerelease": false,
            "published_at": "",
            "release_url": DEFAULT_GITHUB_RELEASES_PAGE,
            "notes": "",
            "checked_at": unix_seconds(),
        }));
    };
    let raw_tag = release
        .get("tag_name")
        .or_else(|| release.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(current_version)
        .trim();
    let latest_clean = raw_tag.trim_start_matches(['v', 'V']);
    let has_update = is_newer_version(latest_clean, current_version);
    let prerelease = release
        .get("prerelease")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let published_at = release
        .get("published_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let html_url = release
        .get("html_url")
        .and_then(|v| v.as_str())
        .filter(|u| u.starts_with("https://github.com/"))
        .unwrap_or(DEFAULT_GITHUB_RELEASES_PAGE)
        .to_owned();
    let notes: String = release
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(600)
        .collect();
    Ok(serde_json::json!({
        "current_version": current_version,
        "latest_version": if latest_clean.is_empty() { current_version } else { latest_clean },
        "has_update": has_update,
        "prerelease": prerelease,
        "published_at": published_at,
        "release_url": html_url,
        "notes": notes,
        "checked_at": unix_seconds(),
    }))
}
