//! 语音中心的业务编排（技术书 §9.4，实施清单 P4-05 / P4-07）。
//!
//! 不依赖 Tauri：桌面壳把 MOSS 后端的能力（严格模式状态、小智桥重连、设备会话遥测、内置服务启停）
//! 通过 [`HostHooks`] 交进来，页面操作经 Tauri 命令调用本模块。
//!
//! - 链路切换：官方小智 / MOSS 内置服务互斥；切入内置时启动内置服务（失败整体回滚），离开时停止；
//!   切换不改接入点，也不改严格模式。2026-11-17 起删除自建服务端链路（用户决定）。
//! - 接入点与工具（NV8）：官方接入点（顶层 `mcp_endpoint`）、附加官方接入点（`mcp_extra_endpoints`）、
//!   工具开关（`mcp_disabled_tools`）；改动后重连小智桥，让接入点重新获取工具清单。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::redact::redact_url;
use crate::settings::{self, VoiceMode, VoiceSettings};

/// MOSS 后端提供给语音中心的能力。
pub trait HostHooks: Send + Sync {
    fn strict_mode(&self) -> bool;
    /// `.env` 中的严格模式设置（页面提示当前开关的来源）
    fn strict_from_env(&self) -> bool;
    /// 先断旧桥，再按 `settings.json` 当前的 `mcp_endpoint` 接新桥；返回新桥是否已启动。
    fn restart_bridge(&self) -> bool;
    /// 小智桥当前是否已握手
    fn bridge_connected(&self) -> bool;
    /// 更新顶栏使用的设备会话数（`None` 表示未知）
    fn set_device_sessions(&self, sessions: Option<u32>);
    /// 启动 MOSS 内置语音服务（切到内置链路时，NV5）。未实现的宿主返回错误，切换随之回滚。
    fn builtin_start(&self) -> Result<(), String> {
        Err("此环境不支持内置语音服务".into())
    }
    /// 停止内置语音服务（离开内置链路时）。
    fn builtin_stop(&self) {}
    /// 查询切到内置语音链路后是否满足对话模式条件（含历史模型回退目录、外置覆盖目录与 Colab 密钥）；
    /// 返回 `None` 时由 `VoiceCenter` 按默认 `<运行目录>/models` 兜底计算。
    fn builtin_chat_ready(&self, _voice: &VoiceSettings) -> Option<bool> {
        None
    }
    /// “接入点与工具”卡片的数据：各接入点的状态与身份、每个工具的开关与调用统计（NV8）。
    fn mcp_overview(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// 校验并立即应用关闭的工具列表（NV8），返回规范化后的列表；未知或不能关闭的工具返回错误。
    fn apply_disabled_tools(&self, _names: &[String]) -> Result<Vec<String>, String> {
        Err("此环境不支持工具开关".into())
    }
}

/// 附加接入点的数量上限（与 `laser_mcp::MAX_EXTRA_ENDPOINTS` 一致；本 crate 不依赖 laser-*）。
pub const MAX_EXTRA_ENDPOINTS: usize = 4;

/// 页面需要的错误：机器可读的 `code` 与中文说明。
///
/// 2026-11-07（n68）：可选 `detail` 放原始技术信息（调用方负责脱敏），`message` 只写大白话。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CenterError {
    pub code: String,
    pub message: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CenterError {
    pub fn new(code: &str, status: u16, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            status,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }
}

type CenterResult<T> = Result<T, CenterError>;

fn valid_ws(url: &str) -> bool {
    (url.starts_with("ws://") || url.starts_with("wss://"))
        && !url.contains('<')
        && !url.chars().any(char::is_whitespace)
}

pub struct VoiceCenter {
    settings_file: PathBuf,
    hooks: Arc<dyn HostHooks>,
    /// 链路切换与设置保存串行执行
    switching: Mutex<()>,
}

impl VoiceCenter {
    /// `runtime_dir` 为 MOSS 运行目录，设置在其中的 `settings.json`。
    pub fn new(runtime_dir: &Path, hooks: Arc<dyn HostHooks>) -> Self {
        Self {
            settings_file: runtime_dir.join("settings.json"),
            hooks,
            switching: Mutex::new(()),
        }
    }

    pub fn settings(&self) -> VoiceSettings {
        settings::load(&self.settings_file)
    }

    // ---------- 状态 ----------

    /// 页面所需的全部状态（地址中的令牌已脱敏，不含任何模型密钥）。
    pub fn status(&self) -> serde_json::Value {
        let voice = self.settings();
        // 官方接入点就是当前顶层 `mcp_endpoint`（官方与内置链路共用）
        let official = settings::mcp_endpoint(&self.settings_file);
        serde_json::json!({
            "mode": voice.mode.as_str(),
            "settings": {
                "builtin_ws_port": voice.builtin_ws_port,
                "builtin_http_port": voice.builtin_http_port,
                "builtin_mqtt_enabled": voice.builtin_mqtt_enabled,
                "builtin_mqtt_port": voice.builtin_mqtt_port,
                "builtin_udp_port": voice.builtin_udp_port,
                "official_mcp_endpoint": official.as_deref().map(redact_url),
                "official_mcp_endpoint_set": official.is_some(),
            },
            "mcp_endpoint": official.as_deref().map(redact_url),
            "bridge": {"connected": self.hooks.bridge_connected()},
            "strict": {
                "enabled": self.hooks.strict_mode(),
                "from_env": self.hooks.strict_from_env(),
            },
            "gateway": {"enabled": false, "note": "网关属于可选的 P2b，本版本未启用"},
            "mcp": self.hooks.mcp_overview(),
        })
    }

    // ---------- 设置 ----------

    /// 保存内置服务端口与常连模式、官方接入点，以及附加接入点与工具开关（NV8）。
    /// 地址类字段留空表示不改；`clear_*` 为真时删除。不认识的键（含已删除的自建链路字段）忽略。
    ///
    /// - `official_mcp_endpoint` / `clear_official_mcp_endpoint`：顶层 `mcp_endpoint`，改了立即重连；
    /// - `add_extra_mcp_endpoint`：新增一个附加官方接入点（最多 [`MAX_EXTRA_ENDPOINTS`] 个，不能与已有地址重复）；
    /// - `remove_extra_mcp_endpoint`：按编号（`extra-1`……）删除；
    /// - `disabled_tools`：关闭的工具名单（整体替换）。
    ///
    /// 接入点、附加接入点或工具开关改动后重连小智桥（接入点重新获取工具清单）。
    pub fn save_settings(&self, patch: &serde_json::Value) -> CenterResult<serde_json::Value> {
        let _guard = self
            .switching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let text = |key: &str| {
            patch
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
        };
        let invalid = |message: &str| CenterError::new("voice_settings_invalid", 400, message);

        let mut port_updates: Vec<(&str, u16)> = Vec::new();
        for (key, label) in [
            ("builtin_ws_port", "内置服务设备连接端口"),
            ("builtin_http_port", "内置服务 OTA 端口"),
            ("builtin_mqtt_port", "常连模式 MQTT 端口"),
            ("builtin_udp_port", "常连模式语音 UDP 端口"),
        ] {
            if let Some(port) = patch.get(key) {
                let port = port
                    .as_u64()
                    .filter(|port| (1..=65535).contains(port))
                    .ok_or_else(|| invalid(&format!("{label}必须在 1 ~ 65535 之间")))?
                    as u16;
                port_updates.push((key, port));
            }
        }
        let mqtt_enabled_patch = patch
            .get("builtin_mqtt_enabled")
            .map(|enabled| {
                enabled
                    .as_bool()
                    .ok_or_else(|| invalid("常连模式开关格式不正确"))
            })
            .transpose()?;

        // 官方接入点：就是当前 `mcp_endpoint`（改了立即重连）
        let mut official_change: Option<Option<String>> = None;
        if let Some(endpoint) = text("official_mcp_endpoint").filter(|value| !value.is_empty()) {
            if !valid_ws(endpoint) {
                return Err(invalid(
                    "官方接入点必须以 ws:// 或 wss:// 开头，且不含占位符",
                ));
            }
            official_change = Some(Some(endpoint.to_owned()));
        }
        if patch
            .get("clear_official_mcp_endpoint")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            official_change = Some(None);
        }

        let add_extra = text("add_extra_mcp_endpoint")
            .filter(|value| !value.is_empty())
            .map(|endpoint| {
                if !valid_ws(endpoint) {
                    return Err(invalid(
                        "附加接入点必须以 ws:// 或 wss:// 开头，且不含占位符",
                    ));
                }
                Ok(endpoint.to_owned())
            })
            .transpose()?;
        let remove_extra = text("remove_extra_mcp_endpoint")
            .filter(|value| !value.is_empty())
            .map(str::to_owned);

        let disabled_tools_patch: Option<Vec<String>> = patch
            .get("disabled_tools")
            .map(|names| {
                names
                    .as_array()
                    .ok_or_else(|| invalid("disabled_tools 必须是工具名列表"))?
                    .iter()
                    .map(|name| {
                        name.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| invalid("disabled_tools 必须是工具名列表"))
                    })
                    .collect::<Result<Vec<String>, _>>()
            })
            .transpose()?;

        let mut tools_rollback: Option<Vec<String>> = None;
        let saved = settings::update_voice_with_root(
            &self.settings_file,
            |root, voice| {
                for (key, port) in &port_updates {
                    match *key {
                        "builtin_ws_port" => voice.builtin_ws_port = *port,
                        "builtin_http_port" => voice.builtin_http_port = *port,
                        "builtin_mqtt_port" => voice.builtin_mqtt_port = *port,
                        _ => voice.builtin_udp_port = *port,
                    }
                }
                if let Some(enabled) = mqtt_enabled_patch {
                    voice.builtin_mqtt_enabled = enabled;
                }
                if voice.builtin_ws_port == voice.builtin_http_port {
                    return Err(invalid("内置服务的两个端口不能相同"));
                }
                // n71：常连 MQTT 端口与另外两个 TCP 端口不能相同，也不能是 8883（小智固件见到 8883 会改走加密连接，连不上）
                if voice.builtin_mqtt_port == 8883 {
                    return Err(invalid(
                        "常连模式 MQTT 端口不能用 8883（小智固件会改走加密连接）",
                    ));
                }
                if voice.builtin_mqtt_port == voice.builtin_ws_port
                    || voice.builtin_mqtt_port == voice.builtin_http_port
                {
                    return Err(invalid(
                        "常连模式 MQTT 端口不能和设备连接端口、OTA 端口相同",
                    ));
                }

                // 附加接入点（NV8）：在设置锁内读最新列表
                let primary = root
                    .get("mcp_endpoint")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let mut extras = settings::string_list(root, "mcp_extra_endpoints");
                let mut extras_changed = false;
                if let Some(endpoint) = &add_extra {
                    if extras.iter().any(|existing| existing == endpoint)
                        || primary == Some(endpoint.as_str())
                    {
                        return Err(invalid("这个接入点已经在列表中"));
                    }
                    if extras.len() >= MAX_EXTRA_ENDPOINTS {
                        return Err(invalid(&format!("附加接入点最多 {MAX_EXTRA_ENDPOINTS} 个")));
                    }
                    extras.push(endpoint.clone());
                    extras_changed = true;
                }
                if let Some(id) = &remove_extra {
                    let index = id
                        .strip_prefix("extra-")
                        .and_then(|number| number.parse::<usize>().ok())
                        .filter(|number| (1..=extras.len()).contains(number))
                        .ok_or_else(|| invalid("没有这个附加接入点"))?;
                    extras.remove(index - 1);
                    extras_changed = true;
                }

                let mut tools_change: Option<Vec<String>> = None;
                if let Some(names) = &disabled_tools_patch {
                    let previous_tools = settings::string_list(root, "mcp_disabled_tools");
                    let applied = self
                        .hooks
                        .apply_disabled_tools(names)
                        .map_err(|message| invalid(&message))?;
                    if applied != previous_tools {
                        tools_rollback = Some(previous_tools);
                        tools_change = Some(applied);
                    }
                }

                let mut reconnect = false;
                let endpoint_to_write = official_change.inspect(|change| {
                    reconnect = primary != change.as_deref();
                });
                reconnect |= extras_changed || tools_change.is_some();

                Ok((
                    reconnect,
                    settings::RootVoiceUpdate {
                        endpoint: endpoint_to_write,
                        extra_endpoints: extras_changed.then_some(extras),
                        disabled_tools: tools_change,
                        jev_feedback: None,
                    },
                ))
            },
            |error| CenterError::new("voice_settings_write_failed", 500, error),
        );
        let should_reconnect = match saved {
            Ok(reconnect) => reconnect,
            Err(err) => {
                if let Some(previous_tools) = tools_rollback {
                    let _ = self.hooks.apply_disabled_tools(&previous_tools);
                }
                return Err(err);
            }
        };
        // `reconnected`：本次保存是否重新发起了接入点连接（且有桥启动）；握手结果看 `bridge.connected`
        let reconnected = should_reconnect && self.hooks.restart_bridge();
        let mut status = self.status();
        if let Some(object) = status.as_object_mut() {
            object.insert("reconnected".into(), reconnected.into());
        }
        Ok(status)
    }

    // ---------- 链路切换 ----------

    /// 切换前列出将改动的项（确认框用）；`target` 与当前相同时返回空列表。
    pub fn mode_changes(&self, target: VoiceMode) -> CenterResult<Vec<String>> {
        let voice = self.settings();
        if voice.mode == target {
            return Ok(Vec::new());
        }
        let label = |mode: VoiceMode| match mode {
            VoiceMode::Official => "官方小智",
            VoiceMode::Builtin => "MOSS 内置服务",
        };
        let mut changes = vec![format!(
            "语音链路：{} → {}",
            label(voice.mode),
            label(target)
        )];
        match target {
            VoiceMode::Builtin => {
                let chat_ready = self.hooks.builtin_chat_ready(&voice).unwrap_or_else(|| {
                    let models_root = self
                        .settings_file
                        .parent()
                        .map(|dir| dir.join("models"))
                        .unwrap_or_default();
                    voice.builtin_chat_ready(&models_root)
                });
                let models = if chat_ready {
                    "已配置识别、大模型与合成：以对话模式运行"
                } else {
                    "模型未配置齐全：先以回声测试模式运行，配置后自动切换为对话"
                };
                changes.push(format!(
                    "启动 MOSS 内置语音服务（端口 {} / {}），之后随 MOSS 自动启动；{models}",
                    voice.builtin_ws_port, voice.builtin_http_port
                ));
                let gate = if self.hooks.strict_mode() {
                    "严格模式已开启：内置语音发起的运动与出光需要在电脑上批准"
                } else {
                    "严格模式未开启：内置语音发起的运动与出光在你口头确认后直接执行（与官方小智相同）"
                };
                changes.push(gate.into());
                changes.push(format!("设备的 OTA 地址需要你自行改为 http://<本机局域网地址>:{}/xiaozhi/ota/（MOSS 不会修改设备）", voice.builtin_http_port));
            }
            VoiceMode::Official => {
                changes.push("停止 MOSS 内置语音服务".into());
                changes.push("设备的 OTA 地址需要你自行改回官方（MOSS 不会修改设备）".into());
            }
        }
        Ok(changes)
    }

    /// 执行链路切换：接入点与严格模式都不变；切入内置时启动内置服务，失败时在设置锁内只回滚 `mode`
    /// 并停止内置服务（不覆盖并发修改的隐私开关）；离开内置时停止内置服务。
    pub fn set_mode(&self, target: VoiceMode) -> CenterResult<serde_json::Value> {
        let _guard = self
            .switching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev_mode = match settings::update_voice(&self.settings_file, |voice| {
            if voice.mode == target {
                return Ok(None);
            }
            let prev = voice.mode;
            voice.mode = target;
            Ok(Some(prev))
        }) {
            Ok(Some(prev)) => prev,
            Ok(None) => return Ok(self.status()),
            Err(error) => {
                if target == VoiceMode::Builtin {
                    self.hooks.builtin_stop();
                }
                return Err(CenterError::new(
                    "voice_mode_switch_failed",
                    500,
                    format!("保存设置失败：{error}；已保留切换前的状态"),
                ));
            }
        };
        let rollback = |reason: String| -> CenterError {
            let _ = settings::update_voice(&self.settings_file, |voice| {
                voice.mode = prev_mode;
                Ok(())
            });
            if target == VoiceMode::Builtin {
                self.hooks.builtin_stop();
            }
            CenterError::new(
                "voice_mode_switch_failed",
                500,
                format!("{reason}；已回滚到切换前的状态"),
            )
        };
        match target {
            VoiceMode::Builtin => {
                if let Err(error) = self.hooks.builtin_start() {
                    return Err(rollback(format!("内置语音服务启动失败：{error}")));
                }
            }
            VoiceMode::Official => {
                self.hooks.builtin_stop();
                // 官方链路无法得知设备会话；内置链路的会话数由内置服务自己上报
                self.hooks.set_device_sessions(None);
            }
        }
        Ok(self.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct FakeHost {
        strict: AtomicBool,
        env_strict: bool,
        bridge_ok: AtomicBool,
        restarts: AtomicUsize,
        /// 每次重连桥时读到的 settings.json 中的 mcp_endpoint
        endpoints: Mutex<Vec<Option<String>>>,
        settings_file: PathBuf,
        sessions: Mutex<Option<u32>>,
        builtin_ok: AtomicBool,
        builtin_running: AtomicBool,
        /// 当前生效的关闭工具名单（NV8）
        disabled: Mutex<Vec<String>>,
    }

    impl HostHooks for FakeHost {
        fn strict_mode(&self) -> bool {
            self.strict.load(Ordering::SeqCst)
        }
        fn strict_from_env(&self) -> bool {
            self.env_strict
        }
        fn restart_bridge(&self) -> bool {
            self.restarts.fetch_add(1, Ordering::SeqCst);
            let endpoint = settings::mcp_endpoint(&self.settings_file);
            self.endpoints.lock().unwrap().push(endpoint.clone());
            endpoint.is_some() && self.bridge_ok.load(Ordering::SeqCst)
        }
        fn bridge_connected(&self) -> bool {
            false
        }
        fn set_device_sessions(&self, sessions: Option<u32>) {
            *self.sessions.lock().unwrap() = sessions;
        }
        fn builtin_start(&self) -> Result<(), String> {
            if !self.builtin_ok.load(Ordering::SeqCst) {
                return Err("端口 8000 已被占用".into());
            }
            self.builtin_running.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn builtin_stop(&self) {
            self.builtin_running.store(false, Ordering::SeqCst);
        }
        fn mcp_overview(&self) -> serde_json::Value {
            serde_json::json!({"disabled": *self.disabled.lock().unwrap()})
        }
        fn apply_disabled_tools(&self, names: &[String]) -> Result<Vec<String>, String> {
            let known = [
                "material_params_tool",
                "open_app_tool",
                "laser_safe_action_tool",
            ];
            let mut accepted = Vec::new();
            for name in names {
                if !known.contains(&name.as_str()) {
                    return Err(format!("没有名为 {name} 的工具"));
                }
                if name == "laser_safe_action_tool" {
                    return Err(format!("{name} 负责停止与急停，不能关闭"));
                }
                accepted.push(name.clone());
            }
            accepted.sort();
            accepted.dedup();
            *self.disabled.lock().unwrap() = accepted.clone();
            Ok(accepted)
        }
    }

    const OFFICIAL: &str = "wss://official.example/mcp/?token=official-token";

    fn fixture() -> (tempfile::TempDir, Arc<FakeHost>, VoiceCenter) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(
            &file,
            format!(r#"{{"web_port": 9666, "mcp_endpoint": "{OFFICIAL}"}}"#),
        )
        .unwrap();
        let host = Arc::new(FakeHost {
            settings_file: file.clone(),
            ..Default::default()
        });
        host.bridge_ok.store(true, Ordering::SeqCst);
        let center = VoiceCenter::new(dir.path(), host.clone());
        (dir, host, center)
    }

    /// 2026-11-17：状态里只剩官方 / 内置链路要用的字段，令牌脱敏；自建链路的字段与严格模式锁都不再出现。
    #[test]
    fn status_reports_only_official_and_builtin_fields() {
        let (_dir, _host, center) = fixture();
        let status = center.status();
        assert_eq!(status["mode"], "official");
        assert_eq!(
            status["settings"]["official_mcp_endpoint"],
            "wss://official.example/mcp/?token=***"
        );
        assert_eq!(status["settings"]["official_mcp_endpoint_set"], true);
        assert_eq!(
            status["mcp_endpoint"],
            status["settings"]["official_mcp_endpoint"]
        );
        assert_eq!(status["gateway"]["enabled"], false);
        assert!(!status.to_string().contains("official-token"));
        for removed in ["server", "server_config", "endpoint", "device", "models"] {
            assert!(status.get(removed).is_none(), "{removed}: {status}");
        }
        for removed in [
            "server_dir",
            "python",
            "args",
            "server_port",
            "self_hosted_mcp_endpoint",
            "self_hosted_mcp_endpoint_set",
            "endpoint_health_url",
            "endpoint_health_url_set",
            "official_backup_set",
        ] {
            assert!(status["settings"].get(removed).is_none(), "{removed}");
        }
        assert!(status["strict"].get("locked").is_none(), "{status}");
    }

    /// NV5：内置链路——切入时启动内置服务、接入点与严格模式不变；离开时停止，同样不改接入点与严格模式。
    #[test]
    fn builtin_mode_starts_and_stops_the_builtin_service() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        host.builtin_ok.store(true, Ordering::SeqCst);
        assert!(center.mode_changes(VoiceMode::Official).unwrap().is_empty());
        let changes = center.mode_changes(VoiceMode::Builtin).unwrap();
        assert!(
            changes[0].contains("官方小智 → MOSS 内置服务"),
            "{changes:?}"
        );
        assert!(
            changes.iter().any(|line| line.contains("回声测试模式")),
            "模型未配置齐全时说明以回声运行"
        );
        assert!(
            changes
                .iter()
                .any(|line| line.contains("严格模式未开启") && line.contains("口头确认")),
            "{changes:?}"
        );
        assert!(
            !changes.iter().any(|line| line.contains("mcp_endpoint")),
            "官方切到内置不改接入点"
        );

        let status = center.set_mode(VoiceMode::Builtin).unwrap();
        assert_eq!(status["mode"], "builtin");
        assert!(host.builtin_running.load(Ordering::SeqCst));
        assert_eq!(
            host.restarts.load(Ordering::SeqCst),
            0,
            "接入点没变，不重连小智桥"
        );
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));
        assert!(!host.strict.load(Ordering::SeqCst));
        // 内置链路下修改官方接入点立即重连（与官方模式相同）
        let other = "wss://official.example/mcp/?token=other";
        center
            .save_settings(&serde_json::json!({"official_mcp_endpoint": other}))
            .unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);

        let changes = center.mode_changes(VoiceMode::Official).unwrap();
        assert!(
            changes[0].contains("MOSS 内置服务 → 官方小智"),
            "{changes:?}"
        );
        assert!(
            changes
                .iter()
                .any(|line| line.contains("停止 MOSS 内置语音服务"))
        );
        assert!(changes.iter().any(|line| line.contains("改回官方")));
        *host.sessions.lock().unwrap() = Some(2);
        let status = center.set_mode(VoiceMode::Official).unwrap();
        assert_eq!(status["mode"], "official");
        assert!(!host.builtin_running.load(Ordering::SeqCst));
        assert_eq!(
            host.restarts.load(Ordering::SeqCst),
            1,
            "内置切回官方不重连"
        );
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(other));
        assert!(!host.strict.load(Ordering::SeqCst), "切换不改严格模式");
        assert_eq!(
            *host.sessions.lock().unwrap(),
            None,
            "官方链路无法得知设备会话"
        );
    }

    /// 内置服务启动失败：设置文件原样恢复、内置服务停止、接入点与严格模式不变。
    #[test]
    fn builtin_start_failure_rolls_back() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let error = center.set_mode(VoiceMode::Builtin).unwrap_err();
        assert_eq!(error.code, "voice_mode_switch_failed");
        assert!(error.message.contains("端口 8000 已被占用"), "{error:?}");
        assert!(error.message.contains("回滚"));
        assert_eq!(settings::load(&file).mode, VoiceMode::Official);
        assert!(!host.builtin_running.load(Ordering::SeqCst));
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(after["mcp_endpoint"], before["mcp_endpoint"]);
        assert_eq!(after["web_port"], before["web_port"]);
        assert_eq!(after["voice"]["mode"], "official");
        assert_eq!(
            host.restarts.load(Ordering::SeqCst),
            0,
            "接入点没变，不重连"
        );
        assert!(!host.strict_mode());
    }

    /// n71 常连模式：开关与两个端口随“内置服务”设置保存；8883 和与其他 TCP 端口撞车的端口被拒绝。
    #[test]
    fn mqtt_continuous_mode_settings_are_validated() {
        let (dir, _host, center) = fixture();
        let file = dir.path().join("settings.json");
        let status = center.status();
        assert_eq!(status["settings"]["builtin_mqtt_enabled"], false, "默认关");
        assert_eq!(status["settings"]["builtin_mqtt_port"], 1883);
        assert_eq!(status["settings"]["builtin_udp_port"], 8888);
        let saved = center
            .save_settings(&serde_json::json!({
                "builtin_mqtt_enabled": true,
                "builtin_mqtt_port": 1884,
                "builtin_udp_port": 8890
            }))
            .unwrap();
        assert_eq!(saved["settings"]["builtin_mqtt_enabled"], true);
        let stored = settings::load(&file);
        assert!(stored.builtin_mqtt_enabled);
        assert_eq!(
            (stored.builtin_mqtt_port, stored.builtin_udp_port),
            (1884, 8890)
        );
        for bad in [
            serde_json::json!({"builtin_mqtt_port": 8883}),
            serde_json::json!({"builtin_mqtt_port": 8000}),
            serde_json::json!({"builtin_mqtt_port": 8003}),
            serde_json::json!({"builtin_udp_port": 0}),
            serde_json::json!({"builtin_mqtt_enabled": "yes"}),
        ] {
            assert_eq!(
                center.save_settings(&bad).unwrap_err().code,
                "voice_settings_invalid",
                "{bad}"
            );
        }
        assert_eq!(
            settings::load(&file).builtin_mqtt_port,
            1884,
            "被拒绝的保存不改设置"
        );
    }

    /// A-77：官方接入点只在语音中心修改；保存即重连，相同地址不重连；可以删除。
    #[test]
    fn official_endpoint_is_edited_in_voice_center() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let other = "wss://official.example/mcp/?token=new-official";
        assert_eq!(
            center
                .save_settings(&serde_json::json!({"official_mcp_endpoint": "https://x"}))
                .unwrap_err()
                .code,
            "voice_settings_invalid"
        );
        let status = center
            .save_settings(&serde_json::json!({"official_mcp_endpoint": other}))
            .unwrap();
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(other));
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
        assert_eq!(
            status["settings"]["official_mcp_endpoint"],
            "wss://official.example/mcp/?token=***"
        );
        assert_eq!(status["reconnected"], true);
        // 相同地址再保存不重连
        let status = center
            .save_settings(&serde_json::json!({"official_mcp_endpoint": other}))
            .unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
        assert_eq!(status["reconnected"], false);
        // 发起了重连但没有桥启动：不能报告已重连
        host.bridge_ok.store(false, Ordering::SeqCst);
        let status = center
            .save_settings(&serde_json::json!({"official_mcp_endpoint": OFFICIAL}))
            .unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 2);
        assert_eq!(status["reconnected"], false);
        assert_eq!(
            host.endpoints.lock().unwrap().last().unwrap().as_deref(),
            Some(OFFICIAL)
        );
        host.bridge_ok.store(true, Ordering::SeqCst);
        center
            .save_settings(&serde_json::json!({"clear_official_mcp_endpoint": true}))
            .unwrap();
        assert_eq!(settings::mcp_endpoint(&file), None);
        assert_eq!(
            center.status()["settings"]["official_mcp_endpoint_set"],
            false
        );
    }

    /// `.env` 开启的严格模式与链路无关：切换前后都照实显示，切换不会去改它。
    #[test]
    fn strict_mode_is_reported_and_not_changed_by_link_switch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let host = Arc::new(FakeHost {
            settings_file: file.clone(),
            env_strict: true,
            ..Default::default()
        });
        host.strict.store(true, Ordering::SeqCst);
        host.builtin_ok.store(true, Ordering::SeqCst);
        let center = VoiceCenter::new(dir.path(), host.clone());
        let changes = center.mode_changes(VoiceMode::Builtin).unwrap();
        assert!(
            changes.iter().any(|line| line.contains("严格模式已开启")),
            "{changes:?}"
        );
        let status = center.set_mode(VoiceMode::Builtin).unwrap();
        assert_eq!(status["strict"]["enabled"], true);
        assert_eq!(status["strict"]["from_env"], true);
        let status = center.set_mode(VoiceMode::Official).unwrap();
        assert_eq!(status["strict"]["enabled"], true);
        assert!(host.strict_mode());
    }

    /// 2026-11-17 删除自建链路后：旧页面或脚本传来的自建字段一律忽略（不报错、不写入、不重连）；
    /// 主窗口设置也不能打开私有窗口才能开的记忆开关。
    #[test]
    fn removed_self_hosted_and_private_keys_are_ignored() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let status = center
            .save_settings(&serde_json::json!({
                "server_dir": " D:/srv ",
                "python": "py",
                "args": "app.py --x",
                "server_port": 0,
                "self_hosted_mcp_endpoint": "ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=self",
                "clear_self_hosted_mcp_endpoint": true,
                "endpoint_health_url": "ws://x",
                "clear_endpoint_health_url": true
            }))
            .unwrap();
        assert_eq!(status["reconnected"], false);
        assert_eq!(host.restarts.load(Ordering::SeqCst), 0);
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));
        let root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        for key in [
            "server_dir",
            "python",
            "args",
            "server_port",
            "self_hosted_mcp_endpoint",
            "endpoint_health_url",
        ] {
            assert!(root["voice"].get(key).is_none(), "{key}: {root}");
        }
        center
            .save_settings(&serde_json::json!({"builtin_person_memory_enabled":true}))
            .unwrap();
        assert!(
            !center.settings().builtin_person_memory_enabled,
            "普通窗口设置不能绕过私有窗口的标定与同意门禁"
        );
        center.save_settings(&serde_json::json!({"builtin_context":{"retrieval":true,"extraction":true,"project_id":"p-x"}})).unwrap();
        assert_eq!(
            center.settings().builtin_context,
            crate::settings::BuiltinContext::default(),
            "主窗口设置不能打开长期记忆或改当前项目"
        );
    }

    /// NV8：附加接入点——校验、上限、去重、按编号删除；官方与内置链路下改动都立即重连。
    #[test]
    fn extra_endpoints_are_validated_saved_and_reconnect() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let extra = |n: usize| format!("wss://official.example/mcp/?token=extra-{n}");
        for (bad, why) in [
            ("http://official.example/mcp/", "以 ws:// 或 wss://"),
            ("wss://x/mcp/?token=<your-token>", "占位符"),
            (OFFICIAL, "已经在列表中"),
        ] {
            let error = center
                .save_settings(&serde_json::json!({"add_extra_mcp_endpoint": bad}))
                .unwrap_err();
            assert!(error.message.contains(why), "{bad}: {error:?}");
        }
        for n in 1..=MAX_EXTRA_ENDPOINTS {
            center
                .save_settings(&serde_json::json!({"add_extra_mcp_endpoint": extra(n)}))
                .unwrap();
        }
        assert_eq!(
            host.restarts.load(Ordering::SeqCst),
            MAX_EXTRA_ENDPOINTS,
            "每次改动都重连"
        );
        let error = center
            .save_settings(&serde_json::json!({"add_extra_mcp_endpoint": extra(9)}))
            .unwrap_err();
        assert!(error.message.contains("最多"), "{error:?}");
        let error = center
            .save_settings(&serde_json::json!({"add_extra_mcp_endpoint": extra(2)}))
            .unwrap_err();
        assert!(error.message.contains("已经在列表中"));
        center
            .save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-2"}))
            .unwrap();
        assert_eq!(
            settings::extra_mcp_endpoints(&file),
            vec![extra(1), extra(3), extra(4)]
        );
        for bad in ["extra-0", "extra-4", "primary", "extra-x"] {
            assert!(
                center
                    .save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": bad}))
                    .is_err(),
                "{bad}"
            );
        }
        // 其他设置原样保留，主接入点不受影响
        let root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(root["web_port"], 9666);
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));

        // 内置链路下改动同样立即重连
        host.builtin_ok.store(true, Ordering::SeqCst);
        center.set_mode(VoiceMode::Builtin).unwrap();
        let restarts = host.restarts.load(Ordering::SeqCst);
        center
            .save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-1"}))
            .unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts + 1);
        assert_eq!(
            settings::extra_mcp_endpoints(&file),
            vec![extra(3), extra(4)]
        );
        // 删光后不留空键
        center
            .save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-1"}))
            .unwrap();
        center
            .save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-1"}))
            .unwrap();
        let root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(root.get("mcp_extra_endpoints").is_none());
    }

    /// NV8：工具开关——后端校验后立即生效并写入 settings.json，然后重连让接入点重新获取工具清单；
    /// 不能关闭或不存在的工具被拒绝，什么也不改；名单没变不重连。
    #[test]
    fn tool_switches_are_applied_saved_and_reconnect() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let status = center
            .save_settings(
                &serde_json::json!({"disabled_tools": ["material_params_tool", "open_app_tool"]}),
            )
            .unwrap();
        assert_eq!(
            settings::disabled_tools(&file),
            vec![
                "material_params_tool".to_string(),
                "open_app_tool".to_string()
            ]
        );
        assert_eq!(
            *host.disabled.lock().unwrap(),
            settings::disabled_tools(&file)
        );
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
        assert_eq!(
            status["mcp"]["disabled"],
            serde_json::json!(["material_params_tool", "open_app_tool"])
        );

        let error = center
            .save_settings(&serde_json::json!({"disabled_tools": ["laser_safe_action_tool"]}))
            .unwrap_err();
        assert!(error.message.contains("不能关闭"), "{error:?}");
        assert!(
            center
                .save_settings(&serde_json::json!({"disabled_tools": "material_params_tool"}))
                .is_err()
        );
        assert!(
            center
                .save_settings(&serde_json::json!({"disabled_tools": [1]}))
                .is_err()
        );
        assert_eq!(
            settings::disabled_tools(&file).len(),
            2,
            "被拒绝的改动不写文件"
        );
        assert_eq!(host.disabled.lock().unwrap().len(), 2, "被拒绝的改动不生效");

        center
            .save_settings(
                &serde_json::json!({"disabled_tools": ["open_app_tool", "material_params_tool"]}),
            )
            .unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1, "名单没变不重连");
        center
            .save_settings(&serde_json::json!({"disabled_tools": []}))
            .unwrap();
        assert!(settings::disabled_tools(&file).is_empty());
        assert_eq!(host.restarts.load(Ordering::SeqCst), 2);
        let root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(
            root.get("mcp_disabled_tools").is_none(),
            "全部打开后不留空键"
        );
    }

    /// r76：已移除的 `mcp_full_tool_list` 键在旧配置文件与保存接口中均静默忽略，不报错、不重连、不丢其他设置。
    #[test]
    fn legacy_mcp_full_tool_list_is_ignored_on_load_and_save() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        std::fs::write(
            &file,
            serde_json::json!({
                "web_port": 9666,
                "voice": {
                    "mode": "official",
                    "builtin_ws_port": 8088,
                    "mcp_full_tool_list": true
                }
            })
            .to_string(),
        )
        .unwrap();
        let loaded = settings::load(&file);
        assert_eq!(loaded.builtin_ws_port, 8088);

        let before = host.restarts.load(Ordering::SeqCst);
        center
            .save_settings(&serde_json::json!({"mcp_full_tool_list": true}))
            .unwrap();
        center
            .save_settings(
                &serde_json::json!({"mcp_full_tool_list": "yes", "builtin_http_port": 8099}),
            )
            .unwrap();
        assert_eq!(
            host.restarts.load(Ordering::SeqCst),
            before,
            "忽略旧键不应触发接入点重连"
        );
        let after = settings::load(&file);
        assert_eq!(after.builtin_ws_port, 8088);
        assert_eq!(after.builtin_http_port, 8099);
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(raw["web_port"], 9666);
        assert!(raw["voice"].get("mcp_full_tool_list").is_none());
    }

    #[test]
    fn save_settings_and_mode_rollback_preserve_concurrent_privacy_switches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");

        settings::update_voice(&file, |voice| {
            voice.mode = VoiceMode::Official;
            voice.builtin_agents.enabled = true;
            voice.builtin_memory_enabled = true;
            voice.builtin_person_memory_enabled = true;
            voice.builtin_context.retrieval = true;
            Ok(())
        })
        .unwrap();

        struct ConcurrentToggleHost {
            inner: FakeHost,
        }
        impl HostHooks for ConcurrentToggleHost {
            fn strict_mode(&self) -> bool {
                self.inner.strict_mode()
            }
            fn strict_from_env(&self) -> bool {
                self.inner.strict_from_env()
            }
            fn restart_bridge(&self) -> bool {
                self.inner.restart_bridge()
            }
            fn bridge_connected(&self) -> bool {
                self.inner.bridge_connected()
            }
            fn set_device_sessions(&self, sessions: Option<u32>) {
                self.inner.set_device_sessions(sessions)
            }
            fn builtin_start(&self) -> Result<(), String> {
                // 模拟切换中途用户并发关闭隐私/Agent/记忆开关
                settings::update_voice(&self.inner.settings_file, |voice| {
                    voice.builtin_agents.enabled = false;
                    voice.builtin_memory_enabled = false;
                    voice.builtin_person_memory_enabled = false;
                    voice.builtin_context.retrieval = false;
                    Ok(())
                })
                .unwrap();
                Err("模拟启动失败".into())
            }
            fn builtin_stop(&self) {
                self.inner.builtin_stop()
            }
            fn apply_disabled_tools(&self, names: &[String]) -> Result<Vec<String>, String> {
                self.inner.apply_disabled_tools(names)
            }
        }

        let host = Arc::new(ConcurrentToggleHost {
            inner: FakeHost {
                settings_file: file.clone(),
                ..Default::default()
            },
        });
        let center = Arc::new(VoiceCenter::new(dir.path(), host.clone()));

        // 1) set_mode(Builtin) 启动失败回滚时，只回滚 mode，不得抹掉启动期间并发关闭的隐私开关
        let err = center.set_mode(VoiceMode::Builtin).unwrap_err();
        assert_eq!(err.code, "voice_mode_switch_failed");
        let after_rollback = settings::load(&file);
        assert_eq!(after_rollback.mode, VoiceMode::Official);
        assert!(!after_rollback.builtin_agents.enabled);
        assert!(!after_rollback.builtin_memory_enabled);
        assert!(!after_rollback.builtin_person_memory_enabled);
        assert!(!after_rollback.builtin_context.retrieval);

        // 2) save_settings 与并发 update_voice 竞争时互不覆盖
        let file_bg = file.clone();
        let bg = std::thread::spawn(move || {
            for i in 0..20 {
                settings::update_voice(&file_bg, |voice| {
                    voice.builtin_agents.enabled = i % 2 == 0;
                    voice.builtin_memory_enabled = i == 19;
                    Ok(())
                })
                .unwrap();
            }
        });
        for i in 0..20 {
            let port = 8100 + i;
            center
                .save_settings(&serde_json::json!({
                    "builtin_ws_port": port
                }))
                .unwrap();
        }
        bg.join().unwrap();

        let after_race = settings::load(&file);
        assert_eq!(after_race.builtin_ws_port, 8119);
        assert!(after_race.builtin_memory_enabled);
        assert!(!after_race.builtin_agents.enabled);

        // 3) settings.json 损坏或写入失败时，工具开关运行态缓存不能留脏状态
        center
            .save_settings(&serde_json::json!({"disabled_tools": ["material_params_tool"]}))
            .unwrap();
        assert_eq!(
            *host.inner.disabled.lock().unwrap(),
            vec!["material_params_tool".to_string()]
        );

        // 用目录占住临时文件路径，稳定触发写盘失败（含 root 和 Windows）。
        let blocked_temp = file.with_extension(format!("json.tmp-{}", std::process::id()));
        std::fs::create_dir(&blocked_temp).unwrap();
        let dir_err = center
            .save_settings(&serde_json::json!({
                "disabled_tools": ["open_app_tool"]
            }))
            .unwrap_err();
        std::fs::remove_dir(&blocked_temp).unwrap();
        assert_eq!(dir_err.code, "voice_settings_write_failed");
        assert_eq!(
            *host.inner.disabled.lock().unwrap(),
            vec!["material_params_tool".to_string()],
            "写盘失败后必须把内存 disabled_tools 回滚到旧值"
        );

        std::fs::write(&file, "{broken json").unwrap();
        let write_err = center
            .save_settings(&serde_json::json!({
                "disabled_tools": ["open_app_tool"]
            }))
            .unwrap_err();
        assert_eq!(write_err.code, "voice_settings_write_failed");
        assert_eq!(
            *host.inner.disabled.lock().unwrap(),
            vec!["material_params_tool".to_string()]
        );
    }

    #[test]
    fn mode_changes_uses_host_hook_for_fallback_or_colab_readiness() {
        struct FallbackReadyHost;
        impl HostHooks for FallbackReadyHost {
            fn strict_mode(&self) -> bool {
                false
            }
            fn strict_from_env(&self) -> bool {
                false
            }
            fn restart_bridge(&self) -> bool {
                true
            }
            fn bridge_connected(&self) -> bool {
                false
            }
            fn set_device_sessions(&self, _sessions: Option<u32>) {}
            fn builtin_chat_ready(&self, _voice: &VoiceSettings) -> Option<bool> {
                Some(true)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            format!(r#"{{"web_port": 9666, "mcp_endpoint": "{OFFICIAL}"}}"#),
        )
        .unwrap();
        let center = VoiceCenter::new(dir.path(), Arc::new(FallbackReadyHost));
        let changes = center.mode_changes(VoiceMode::Builtin).unwrap();
        assert!(
            changes.iter().any(|c| c.contains("以对话模式运行")),
            "宿主回退目录或 Colab 已满足对话模式时，切换确认框不得误报回声模式: {changes:?}"
        );
    }
}
