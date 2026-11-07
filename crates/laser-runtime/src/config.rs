use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct LaserSettings {
    pub default_connection_mode: String,
    pub engraving_mode: String,
    pub web_host: String,
    pub web_port: u16,
    pub material_params_file: Option<String>,
}

impl LaserSettings {
    pub fn from_env() -> Result<Self, String> {
        Self::from_env_with_runtime_dir(None)
    }

    pub fn from_env_with_runtime_dir(runtime_dir: Option<&Path>) -> Result<Self, String> {
        load_env_files(runtime_dir);
        let settings_file = runtime_dir
            .map(|d| d.join("settings.json"))
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let port_from_file = settings_file
            .as_ref()
            .and_then(|v| v.get("web_port").and_then(|p| p.as_u64()))
            .map(|p| p as u16);
        // 偏好设置“允许手机连接”（settings.json `lan_access`），下次启动生效
        let lan_access = settings_file
            .as_ref()
            .and_then(|v| v.get("lan_access").and_then(|value| value.as_bool()))
            .unwrap_or(false);

        let web_port = if let Ok(val) = env::var("LASER_WEB_PORT") {
            val.parse::<u16>()
                .map_err(|_| "LASER_WEB_PORT must be a valid port".to_owned())?
        } else if let Some(p) = port_from_file {
            p
        } else {
            9666
        };
        let mode = env::var("LASER_DEFAULT_CONNECTION_MODE").unwrap_or_else(|_| "network".into());
        if !matches!(mode.as_str(), "network" | "serial") {
            return Err("LASER_DEFAULT_CONNECTION_MODE must be network or serial".into());
        }
        let engraving = env::var("LASER_ENGRAVING_MODE").unwrap_or_else(|_| "raster".into());
        if !matches!(engraving.as_str(), "raster" | "outline") {
            return Err("LASER_ENGRAVING_MODE must be raster or outline".into());
        }
        Ok(Self {
            default_connection_mode: mode,
            engraving_mode: engraving,
            web_host: web_host_from(env::var("LASER_WEB_HOST").ok(), lan_access),
            web_port,
            material_params_file: env::var("LASER_MATERIAL_PARAMS_FILE").ok(),
        })
    }
}

/// HTTP 监听地址：环境变量 `LASER_WEB_HOST` 优先；否则偏好设置开启“允许手机连接”时监听全部网卡
/// `0.0.0.0`（局域网访问仍需配对 token），默认只监听本机 `127.0.0.1`。
pub fn web_host_from(env_value: Option<String>, lan_access: bool) -> String {
    match env_value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None if lan_access => "0.0.0.0".into(),
        None => "127.0.0.1".into(),
    }
}

/// 加载 `.env`，优先级从高到低：
/// 1. 进程已有的环境变量（系统 / 用户环境变量、命令行临时设置）；
/// 2. 启动时当前目录（及其上级目录）里的 `.env`——开发、`cargo run` 场景；
/// 3. 运行时数据目录里的 `.env`（Windows 为 `%APPDATA%\moss\.env`）——安装版场景。
///
/// 安装版从开始菜单或桌面快捷方式启动时，当前目录不确定，第 2 步可能找不到文件；
/// 第 3 步保证数据目录里的 `.env` 一定能被读到，且与安装目录无关。
/// dotenvy 不覆盖已存在的变量，所以先加载的来源优先。文件不存在时静默跳过。
///
/// 返回实际加载成功的文件路径（供日志与测试）。
pub fn load_env_files(runtime_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut loaded = Vec::new();
    if let Ok(path) = dotenvy::dotenv() {
        loaded.push(path);
    }
    if let Some(dir) = runtime_dir {
        let path = dir.join(".env");
        if path.is_file() && dotenvy::from_path(&path).is_ok() && !loaded.contains(&path) {
            loaded.push(path);
        }
    }
    loaded
}

/// 解析 `MOSS_ALLOWED_ORIGINS`（逗号分隔，去空白，忽略空项）。
///
/// 每项必须是完整来源 `scheme://host[:port]`（scheme 为 http、https 或 tauri），不能带路径；
/// 出现 `*` 时返回错误——MOSS 不允许任意来源跨站访问（技术书 §6.5 / D11）。
pub fn parse_allowed_origins(raw: &str) -> Result<Vec<String>, String> {
    let mut origins = Vec::new();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        if item.contains('*') {
            return Err("MOSS_ALLOWED_ORIGINS 不允许包含 *（禁止任意来源跨站访问）".into());
        }
        let rest = item
            .strip_prefix("http://")
            .or_else(|| item.strip_prefix("https://"))
            .or_else(|| item.strip_prefix("tauri://"))
            .ok_or_else(|| {
                format!(
                    "MOSS_ALLOWED_ORIGINS 项 `{item}` 必须以 http://、https:// 或 tauri:// 开头"
                )
            })?;
        if rest.is_empty() || rest.contains('/') || rest.contains(char::is_whitespace) {
            return Err(format!(
                "MOSS_ALLOWED_ORIGINS 项 `{item}` 必须是 scheme://host[:port]，不能带路径或末尾斜杠"
            ));
        }
        if !origins.iter().any(|existing: &String| existing == item) {
            origins.push(item.to_owned());
        }
    }
    Ok(origins)
}

/// 读取环境变量 `MOSS_ALLOWED_ORIGINS`（需先调用 [`load_env_files`] 或 [`LaserSettings::from_env_with_runtime_dir`]）。
/// 未设置时返回空列表。
pub fn allowed_origins_from_env() -> Result<Vec<String>, String> {
    match env::var("MOSS_ALLOWED_ORIGINS") {
        Ok(raw) => parse_allowed_origins(&raw),
        Err(_) => Ok(Vec::new()),
    }
}

fn parse_bool_setting(name: &str, raw: Option<String>) -> Result<Option<bool>, String> {
    let Some(raw) = raw else { return Ok(None) };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "true" | "1" | "yes" | "on" => Ok(Some(true)),
        "false" | "0" | "no" | "off" => Ok(Some(false)),
        _ => Err(format!(
            "{name} 只能是 true 或 false，当前为 `{}`",
            raw.trim()
        )),
    }
}

/// 严格模式开关（技术书 §6.2）：`MOSS_FUSION_STRICT`，默认 `false`。
/// `MOSS_GATEWAY_ENABLED=true` 而严格模式关闭时返回错误，宿主据此拒绝启动；写错的值也报错，不静默当作关闭。
pub fn fusion_strict_from(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, String> {
    let strict =
        parse_bool_setting("MOSS_FUSION_STRICT", lookup("MOSS_FUSION_STRICT"))?.unwrap_or(false);
    let gateway = parse_bool_setting("MOSS_GATEWAY_ENABLED", lookup("MOSS_GATEWAY_ENABLED"))?
        .unwrap_or(false);
    if gateway && !strict {
        return Err("已开启 MOSS_GATEWAY_ENABLED，必须同时设置 MOSS_FUSION_STRICT=true".into());
    }
    Ok(strict)
}

/// 从进程环境变量读取严格模式（需先加载 `.env`）。
pub fn fusion_strict_from_env() -> Result<bool, String> {
    fusion_strict_from(|name| env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fusion_strict_defaults_off_and_gateway_requires_it() {
        fn env_of(
            pairs: &'static [(&'static str, &'static str)],
        ) -> impl Fn(&str) -> Option<String> {
            move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        }
        assert_eq!(fusion_strict_from(env_of(&[])), Ok(false));
        assert_eq!(
            fusion_strict_from(env_of(&[("MOSS_FUSION_STRICT", "")])),
            Ok(false)
        );
        assert_eq!(
            fusion_strict_from(env_of(&[("MOSS_FUSION_STRICT", "TRUE")])),
            Ok(true)
        );
        assert_eq!(
            fusion_strict_from(env_of(&[("MOSS_FUSION_STRICT", "0")])),
            Ok(false)
        );
        assert!(fusion_strict_from(env_of(&[("MOSS_FUSION_STRICT", "maybe")])).is_err());
        assert!(fusion_strict_from(env_of(&[("MOSS_GATEWAY_ENABLED", "true")])).is_err());
        assert!(
            fusion_strict_from(env_of(&[
                ("MOSS_GATEWAY_ENABLED", "true"),
                ("MOSS_FUSION_STRICT", "false")
            ]))
            .is_err()
        );
        assert_eq!(
            fusion_strict_from(env_of(&[
                ("MOSS_GATEWAY_ENABLED", "true"),
                ("MOSS_FUSION_STRICT", "true")
            ])),
            Ok(true)
        );
    }
    #[test]
    fn web_host_prefers_env_then_lan_access_then_loopback() {
        assert_eq!(web_host_from(None, false), "127.0.0.1");
        assert_eq!(web_host_from(None, true), "0.0.0.0");
        assert_eq!(web_host_from(Some("  ".into()), true), "0.0.0.0");
        assert_eq!(
            web_host_from(Some("192.168.1.5".into()), false),
            "192.168.1.5"
        );
        assert_eq!(web_host_from(Some("127.0.0.1".into()), true), "127.0.0.1");
    }

    #[test]
    fn defaults_match_env_contract() {
        let settings = LaserSettings::from_env().unwrap();
        assert_eq!(settings.web_port, 9666);
        assert!(matches!(
            settings.default_connection_mode.as_str(),
            "network" | "serial"
        ));
    }

    #[test]
    fn runtime_dir_env_file_is_loaded() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".env"),
            "MOSS_CONFIG_TEST_RUNTIME_DOTENV=from_runtime_dir\n",
        )
        .unwrap();
        let loaded = load_env_files(Some(temp.path()));
        assert!(loaded.contains(&temp.path().join(".env")));
        assert_eq!(
            env::var("MOSS_CONFIG_TEST_RUNTIME_DOTENV").as_deref(),
            Ok("from_runtime_dir")
        );
    }

    #[test]
    fn runtime_dir_env_file_does_not_override_existing_vars() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".env"),
            "MOSS_CONFIG_TEST_PRECEDENCE=from_runtime_dir\n",
        )
        .unwrap();
        // 仅本测试使用的变量名，不与其他测试冲突。
        unsafe { env::set_var("MOSS_CONFIG_TEST_PRECEDENCE", "from_process") };
        load_env_files(Some(temp.path()));
        assert_eq!(
            env::var("MOSS_CONFIG_TEST_PRECEDENCE").as_deref(),
            Ok("from_process")
        );
    }

    #[test]
    fn missing_runtime_dir_env_file_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let loaded = load_env_files(Some(temp.path()));
        assert!(!loaded.contains(&temp.path().join(".env")));
    }

    #[test]
    fn allowed_origins_parse_rules() {
        assert_eq!(parse_allowed_origins("").unwrap(), Vec::<String>::new());
        assert_eq!(parse_allowed_origins(" , ").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_allowed_origins("http://lab.example:8080").unwrap(),
            vec!["http://lab.example:8080".to_owned()]
        );
        assert_eq!(
            parse_allowed_origins(" http://a.example:1 ,https://b.example, http://a.example:1 ")
                .unwrap(),
            vec![
                "http://a.example:1".to_owned(),
                "https://b.example".to_owned()
            ]
        );
        assert!(parse_allowed_origins("*").is_err());
        assert!(parse_allowed_origins("http://a.example, *").is_err());
        assert!(parse_allowed_origins("http://*.example").is_err());
        assert!(parse_allowed_origins("a.example").is_err());
        assert!(parse_allowed_origins("http://a.example/").is_err());
        assert!(parse_allowed_origins("http://a.example/path").is_err());
        assert!(parse_allowed_origins("http://").is_err());
    }
}
