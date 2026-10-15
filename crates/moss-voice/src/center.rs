//! 语音中心的业务编排（技术书 §9.4，实施清单 P4-05 / P4-06 / P4-07）。
//!
//! 不依赖 Tauri：桌面壳把 MOSS 后端的能力（严格模式开关、小智桥重连、设备会话遥测）
//! 通过 [`HostHooks`] 交进来，页面操作经 Tauri 命令调用本模块。
//!
//! - 链路切换：官方 / 自建互斥；先断旧桥再接新桥；切到自建必开严格模式，`mcp_endpoint` 指向
//!   自建接入点并备份官方地址；任一步失败整体回滚（A-60、A-61）。
//! - 模型配置：先测试后保存（保存的内容必须与测试通过的完全一致）；密钥只在内存与服务端配置文件中。
//! - 接入点与工具（NV8）：附加官方接入点（`mcp_extra_endpoints`，自建模式下暂停连接）、工具开关
//!   （`mcp_disabled_tools`）；改动后重连小智桥，让接入点重新获取工具清单。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config_yaml;
use crate::health::{self, HealthCounts, redact_url};
use crate::logfile::RotatingLog;
use crate::models::{self, ModelsInput, Section, TestResult};
use crate::settings::{self, VoiceMode, VoiceSettings};
use crate::supervisor::{ServerSpec, ServerState, Supervisor, VoiceError};

/// MOSS 后端提供给语音中心的能力。
pub trait HostHooks: Send + Sync {
    fn strict_mode(&self) -> bool;
    fn set_strict_mode(&self, enabled: bool);
    /// `.env` 中的严格模式设置（切回官方链路时恢复为该值）
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CenterError {
    pub code: String,
    pub message: String,
    pub status: u16,
}

impl CenterError {
    pub fn new(code: &str, status: u16, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), status }
    }
}

impl From<VoiceError> for CenterError {
    fn from(error: VoiceError) -> Self {
        Self { code: error.code().into(), message: error.message(), status: error.http_status() }
    }
}

type CenterResult<T> = Result<T, CenterError>;

#[derive(Debug, Clone, Serialize)]
struct TestRecord {
    ok: bool,
    elapsed_ms: u64,
    at: f64,
    detail: String,
    final_url: String,
    #[serde(skip)]
    fingerprint: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct HealthSnapshot {
    #[serde(flatten)]
    counts: HealthCounts,
    checked_at: Option<f64>,
    error: Option<String>,
}

fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn valid_ws(url: &str) -> bool {
    (url.starts_with("ws://") || url.starts_with("wss://")) && !url.contains('<') && !url.chars().any(char::is_whitespace)
}

fn valid_http(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && !url.contains('<') && !url.chars().any(char::is_whitespace)
}

pub struct VoiceCenter {
    settings_file: PathBuf,
    supervisor: Supervisor,
    hooks: Arc<dyn HostHooks>,
    tests: Mutex<BTreeMap<Section, TestRecord>>,
    health: Mutex<HealthSnapshot>,
    /// 链路切换与设置保存串行执行
    switching: Mutex<()>,
    poller_stop: Arc<AtomicBool>,
}

impl VoiceCenter {
    /// `runtime_dir` 为 MOSS 运行目录：设置在 `settings.json`，服务端日志在 `logs/xiaozhi-server.log`。
    pub fn new(runtime_dir: &Path, hooks: Arc<dyn HostHooks>) -> Self {
        let log = RotatingLog::new(runtime_dir.join("logs").join("xiaozhi-server.log"));
        Self::with_supervisor(runtime_dir.join("settings.json"), Supervisor::new(log), hooks)
    }

    pub fn with_supervisor(settings_file: PathBuf, supervisor: Supervisor, hooks: Arc<dyn HostHooks>) -> Self {
        Self {
            settings_file,
            supervisor,
            hooks,
            tests: Mutex::new(BTreeMap::new()),
            health: Mutex::new(HealthSnapshot::default()),
            switching: Mutex::new(()),
            poller_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn settings(&self) -> VoiceSettings {
        settings::load(&self.settings_file)
    }

    pub fn supervisor(&self) -> &Supervisor {
        &self.supervisor
    }

    // ---------- 状态 ----------

    /// 页面所需的全部状态（地址中的令牌已脱敏，不含任何模型密钥）。
    pub fn status(&self) -> serde_json::Value {
        let voice = self.settings();
        let server = self.supervisor.status(voice.server_port);
        let config_path = voice.config_path();
        let config_text = config_path.as_ref().and_then(|path| std::fs::read_to_string(path).ok());
        let health = self.health.lock().map(|guard| guard.clone()).unwrap_or_default();
        let self_hosted = voice.mode == VoiceMode::SelfHosted;
        let device = if !self_hosted {
            serde_json::json!({"state": "unknown", "sessions": null, "reason": "官方链路无法得知设备会话"})
        } else {
            match health.counts.robot_connections {
                Some(0) => serde_json::json!({"state": "waiting", "sessions": 0}),
                Some(count) => serde_json::json!({"state": "online", "sessions": count}),
                None => {
                    let reason: String = if voice.endpoint_health_url.is_some() {
                        health.error.clone().unwrap_or_else(|| "等待首次检查".into())
                    } else {
                        "未填写接入点健康检查地址".into()
                    };
                    serde_json::json!({"state": "unknown", "sessions": null, "reason": reason})
                }
            }
        };
        let tests = self.tests.lock().map(|guard| guard.clone()).unwrap_or_default();
        let models: serde_json::Map<String, serde_json::Value> = Section::ALL
            .iter()
            .map(|section| {
                (
                    section.as_str().to_owned(),
                    tests.get(section).map(|record| serde_json::to_value(record).unwrap_or_default()).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect();
        // 官方接入点：官方模式下为当前 `mcp_endpoint`，自建模式下为切换前的备份
        let official = if self_hosted { voice.official_mcp_endpoint.clone() } else { settings::mcp_endpoint(&self.settings_file) };
        let health_view = if self_hosted && voice.endpoint_health_url.is_some() {
            serde_json::to_value(&health).unwrap_or_default()
        } else {
            serde_json::Value::Null
        };
        serde_json::json!({
            "mode": voice.mode.as_str(),
            "settings": {
                "server_dir": voice.server_dir,
                "python": voice.python,
                "args": voice.args,
                "server_port": voice.server_port,
                "builtin_ws_port": voice.builtin_ws_port,
                "builtin_http_port": voice.builtin_http_port,
                "self_hosted_mcp_endpoint": voice.self_hosted_mcp_endpoint.as_deref().map(redact_url),
                "self_hosted_mcp_endpoint_set": voice.self_hosted_mcp_endpoint.is_some(),
                "endpoint_health_url": voice.endpoint_health_url.as_deref().map(redact_url),
                "endpoint_health_url_set": voice.endpoint_health_url.is_some(),
                "official_backup_set": voice.official_mcp_endpoint.is_some(),
                "official_mcp_endpoint": official.as_deref().map(redact_url),
                "official_mcp_endpoint_set": official.is_some(),
            },
            "mcp_endpoint": settings::mcp_endpoint(&self.settings_file).as_deref().map(redact_url),
            "server": server,
            "server_config": {
                "exists": config_text.is_some(),
                "full_module": config_text.as_deref().map(config_yaml::is_full_module).unwrap_or(false),
            },
            "endpoint": {
                "configured": voice.self_hosted_mcp_endpoint.is_some(),
                "health": health_view,
            },
            "device": device,
            "bridge": {"connected": self.hooks.bridge_connected()},
            "strict": {
                "enabled": self.hooks.strict_mode(),
                "locked": self_hosted,
                "from_env": self.hooks.strict_from_env(),
            },
            "gateway": {"enabled": false, "note": "网关属于可选的 P2b，本版本未启用"},
            "models": models,
            "mcp": self.hooks.mcp_overview(),
        })
    }

    // ---------- 服务端设置 ----------

    /// 保存服务端目录、Python、启动参数、端口、自建接入点与健康检查地址，以及附加接入点与工具开关（NV8）。
    /// 地址类字段留空表示不改；`clear_*` 为真时删除。自建模式下改了接入点会立即重连小智桥。
    ///
    /// - `add_extra_mcp_endpoint`：新增一个附加官方接入点（最多 [`MAX_EXTRA_ENDPOINTS`] 个，不能与已有地址重复）；
    /// - `remove_extra_mcp_endpoint`：按编号（`extra-1`……）删除；
    /// - `disabled_tools`：关闭的工具名单（整体替换）。
    ///
    /// 附加接入点在官方与内置模式下改动后立即重连；工具开关改动后在任何模式下都重连（接入点重新获取工具清单）。
    pub fn save_settings(&self, patch: &serde_json::Value) -> CenterResult<serde_json::Value> {
        let _guard = self.switching.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut voice = self.settings();
        let text = |key: &str| patch.get(key).and_then(serde_json::Value::as_str).map(str::trim);
        let invalid = |message: &str| CenterError::new("voice_settings_invalid", 400, message);
        if let Some(dir) = text("server_dir") {
            voice.server_dir = (!dir.is_empty()).then(|| dir.to_owned());
        }
        if let Some(python) = text("python") {
            voice.python = (!python.is_empty()).then(|| python.to_owned());
        }
        if let Some(args) = patch.get("args") {
            let parsed: Vec<String> = match args {
                serde_json::Value::String(line) => line.split_whitespace().map(str::to_owned).collect(),
                serde_json::Value::Array(items) => {
                    items.iter().filter_map(serde_json::Value::as_str).map(str::trim).filter(|item| !item.is_empty()).map(str::to_owned).collect()
                }
                _ => return Err(invalid("启动参数格式不正确")),
            };
            voice.args = if parsed.is_empty() { vec!["app.py".into()] } else { parsed };
        }
        if let Some(port) = patch.get("server_port") {
            let port = port.as_u64().filter(|port| (1..=65535).contains(port)).ok_or_else(|| invalid("服务端端口必须在 1 ~ 65535 之间"))?;
            voice.server_port = port as u16;
        }
        for (key, label) in [("builtin_ws_port", "内置服务设备连接端口"), ("builtin_http_port", "内置服务 OTA 端口")] {
            if let Some(port) = patch.get(key) {
                let port = port
                    .as_u64()
                    .filter(|port| (1..=65535).contains(port))
                    .ok_or_else(|| invalid(&format!("{label}必须在 1 ~ 65535 之间")))? as u16;
                if key == "builtin_ws_port" {
                    voice.builtin_ws_port = port;
                } else {
                    voice.builtin_http_port = port;
                }
            }
        }
        if voice.builtin_ws_port == voice.builtin_http_port {
            return Err(invalid("内置服务的两个端口不能相同"));
        }
        let mut endpoint_changed = false;
        if let Some(endpoint) = text("self_hosted_mcp_endpoint").filter(|value| !value.is_empty()) {
            if !valid_ws(endpoint) {
                return Err(invalid("自建接入点必须以 ws:// 或 wss:// 开头，且不含占位符"));
            }
            endpoint_changed = voice.self_hosted_mcp_endpoint.as_deref() != Some(endpoint);
            voice.self_hosted_mcp_endpoint = Some(endpoint.to_owned());
        }
        if patch.get("clear_self_hosted_mcp_endpoint").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            if voice.mode == VoiceMode::SelfHosted {
                return Err(invalid("自建模式下不能删除自建接入点，请先切回官方链路"));
            }
            voice.self_hosted_mcp_endpoint = None;
        }
        if let Some(url) = text("endpoint_health_url").filter(|value| !value.is_empty()) {
            if !valid_http(url) {
                return Err(invalid("健康检查地址必须以 http:// 或 https:// 开头"));
            }
            voice.endpoint_health_url = Some(url.to_owned());
        }
        if patch.get("clear_endpoint_health_url").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            voice.endpoint_health_url = None;
        }
        // 官方接入点：官方模式下就是当前 `mcp_endpoint`（改了立即重连）；自建模式下只改备份
        let mut official_change: Option<Option<String>> = None;
        if let Some(endpoint) = text("official_mcp_endpoint").filter(|value| !value.is_empty()) {
            if !valid_ws(endpoint) {
                return Err(invalid("官方接入点必须以 ws:// 或 wss:// 开头，且不含占位符"));
            }
            official_change = Some(Some(endpoint.to_owned()));
        }
        if patch.get("clear_official_mcp_endpoint").and_then(serde_json::Value::as_bool).unwrap_or(false) {
            official_change = Some(None);
        }
        // 附加接入点（NV8）
        let mut extras = settings::extra_mcp_endpoints(&self.settings_file);
        let mut extras_changed = false;
        if let Some(endpoint) = text("add_extra_mcp_endpoint").filter(|value| !value.is_empty()) {
            if !valid_ws(endpoint) {
                return Err(invalid("附加接入点必须以 ws:// 或 wss:// 开头，且不含占位符"));
            }
            let primary = settings::mcp_endpoint(&self.settings_file);
            let taken = [primary.as_deref(), voice.official_mcp_endpoint.as_deref(), voice.self_hosted_mcp_endpoint.as_deref()];
            if extras.iter().any(|existing| existing == endpoint) || taken.contains(&Some(endpoint)) {
                return Err(invalid("这个接入点已经在列表中"));
            }
            if extras.len() >= MAX_EXTRA_ENDPOINTS {
                return Err(invalid(&format!("附加接入点最多 {MAX_EXTRA_ENDPOINTS} 个")));
            }
            extras.push(endpoint.to_owned());
            extras_changed = true;
        }
        if let Some(id) = text("remove_extra_mcp_endpoint").filter(|value| !value.is_empty()) {
            let index = id
                .strip_prefix("extra-")
                .and_then(|number| number.parse::<usize>().ok())
                .filter(|number| (1..=extras.len()).contains(number))
                .ok_or_else(|| invalid("没有这个附加接入点"))?;
            extras.remove(index - 1);
            extras_changed = true;
        }
        // 工具开关（NV8）：先由后端校验并立即生效，写文件失败时恢复原来的名单
        let previous_tools = settings::disabled_tools(&self.settings_file);
        let mut tools_change: Option<Vec<String>> = None;
        if let Some(names) = patch.get("disabled_tools") {
            let names: Vec<String> = names
                .as_array()
                .ok_or_else(|| invalid("disabled_tools 必须是工具名列表"))?
                .iter()
                .map(|name| name.as_str().map(str::to_owned).ok_or_else(|| invalid("disabled_tools 必须是工具名列表")))
                .collect::<Result<_, _>>()?;
            let applied = self.hooks.apply_disabled_tools(&names).map_err(|message| invalid(&message))?;
            if applied != previous_tools {
                tools_change = Some(applied);
            }
        }
        let mut reconnect = false;
        let endpoint_to_write: Option<Option<String>> = match voice.mode {
            VoiceMode::SelfHosted => {
                if let Some(change) = official_change {
                    voice.official_mcp_endpoint = change;
                }
                reconnect = endpoint_changed;
                endpoint_changed.then(|| voice.self_hosted_mcp_endpoint.clone())
            }
            // 内置链路下顶层 mcp_endpoint 仍是官方接入点（小智桥照常按它连接）
            VoiceMode::Official | VoiceMode::Builtin => official_change.map(|change| {
                reconnect = settings::mcp_endpoint(&self.settings_file) != change;
                change
            }),
        };
        reconnect |= extras_changed && voice.mode != VoiceMode::SelfHosted;
        reconnect |= tools_change.is_some();
        let saved = settings::save_all(
            &self.settings_file,
            &voice,
            endpoint_to_write.as_ref().map(|endpoint| endpoint.as_deref()),
            extras_changed.then_some(extras.as_slice()),
            tools_change.as_deref(),
        );
        if let Err(error) = saved {
            if tools_change.is_some() {
                let _ = self.hooks.apply_disabled_tools(&previous_tools);
            }
            return Err(CenterError::new("voice_settings_write_failed", 500, error));
        }
        // `reconnected`：本次保存是否重新发起了接入点连接（且有桥启动）；握手结果看 `bridge.connected`
        let reconnected = reconnect && self.hooks.restart_bridge();
        if let Ok(mut health) = self.health.lock() {
            *health = HealthSnapshot::default();
        }
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
        let current_endpoint = settings::mcp_endpoint(&self.settings_file);
        let show = |value: Option<&str>| value.map(redact_url).unwrap_or_else(|| "（未设置）".into());
        let mut changes = Vec::new();
        let label = |mode: VoiceMode| match mode {
            VoiceMode::Official => "官方小智",
            VoiceMode::SelfHosted => "自建服务端",
            VoiceMode::Builtin => "MOSS 内置服务",
        };
        let link = format!("语音链路：{} → {}", label(voice.mode), label(target));
        match target {
            VoiceMode::SelfHosted => {
                let endpoint = voice.self_hosted_mcp_endpoint.as_deref().ok_or_else(|| {
                    CenterError::new("self_hosted_endpoint_missing", 400, "请先在“服务端设置”中填写自建接入点地址")
                })?;
                changes.push(link);
                if voice.mode == VoiceMode::Builtin {
                    changes.push("停止 MOSS 内置语音服务".into());
                }
                changes.push("严格模式：开启（自建模式下不可关闭；运动与出光需要桌面审批）".into());
                changes.push(format!("MOSS 小智接入点（mcp_endpoint）：{} → {}", show(current_endpoint.as_deref()), redact_url(endpoint)));
                changes.push("备份当前官方接入点，切回官方时恢复".into());
                changes.push("断开当前小智桥，再连接自建接入点".into());
                let extras = settings::extra_mcp_endpoints(&self.settings_file).len();
                if extras > 0 {
                    changes.push(format!("{extras} 个附加官方接入点暂停连接（切回官方或内置时恢复）"));
                }
                changes.push("设备的 OTA 地址需要你自行改为自建服务端（MOSS 不会修改设备）".into());
            }
            VoiceMode::Official | VoiceMode::Builtin => {
                changes.push(link);
                if voice.mode == VoiceMode::SelfHosted {
                    let strict = if self.hooks.strict_from_env() { "开启" } else { "关闭" };
                    changes.push(format!(
                        "MOSS 小智接入点（mcp_endpoint）：{} → {}",
                        show(current_endpoint.as_deref()),
                        voice.official_mcp_endpoint.as_deref().map(redact_url).unwrap_or_else(|| "删除（没有可恢复的官方地址）".into())
                    ));
                    changes.push(format!("严格模式：恢复为 .env 中的设置（{strict}）"));
                    changes.push("断开自建接入点的小智桥，再连接官方接入点".into());
                    if self.supervisor.owns_running_server() {
                        changes.push("MOSS 启动的服务端仍在运行，切换后会询问是否停止".into());
                    }
                }
                if target == VoiceMode::Builtin {
                    let models_root = self.settings_file.parent().map(|dir| dir.join("models")).unwrap_or_default();
                    let models = if voice.builtin_chat_ready(&models_root) {
                        "已配置识别、大模型与合成：以对话模式运行"
                    } else {
                        "模型未配置齐全：先以回声测试模式运行，配置后自动切换为对话"
                    };
                    changes.push(format!("启动 MOSS 内置语音服务（端口 {} / {}），之后随 MOSS 自动启动；{models}", voice.builtin_ws_port, voice.builtin_http_port));
                    let gate = if self.hooks.strict_mode() {
                        "严格模式已开启：内置语音发起的运动与出光需要在电脑上批准"
                    } else {
                        "严格模式未开启：内置语音发起的运动与出光在你口头确认后直接执行（与官方小智相同）"
                    };
                    changes.push(gate.into());
                    changes.push(format!("设备的 OTA 地址需要你自行改为 http://<本机局域网地址>:{}/xiaozhi/ota/（MOSS 不会修改设备）", voice.builtin_http_port));
                } else {
                    if voice.mode == VoiceMode::Builtin {
                        changes.push("停止 MOSS 内置语音服务".into());
                    }
                    changes.push("设备的 OTA 地址需要你自行改回官方（MOSS 不会修改设备）".into());
                }
            }
        }
        Ok(changes)
    }

    /// 执行链路切换；失败时恢复 `settings.json`、严格模式与原来的小智桥。
    pub fn set_mode(&self, target: VoiceMode) -> CenterResult<serde_json::Value> {
        let _guard = self.switching.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let old_voice = self.settings();
        if old_voice.mode == target {
            return Ok(self.status());
        }
        let old_endpoint = settings::mcp_endpoint(&self.settings_file);
        let old_strict = self.hooks.strict_mode();
        let mut new_voice = old_voice.clone();
        new_voice.mode = target;
        let (new_endpoint, new_strict) = match target {
            // 从官方切到内置：接入点与严格模式不变
            VoiceMode::Builtin if old_voice.mode == VoiceMode::Official => (old_endpoint.clone(), old_strict),
            VoiceMode::SelfHosted => {
                let endpoint = old_voice.self_hosted_mcp_endpoint.clone().ok_or_else(|| {
                    CenterError::new("self_hosted_endpoint_missing", 400, "请先在“服务端设置”中填写自建接入点地址")
                })?;
                if !valid_ws(&endpoint) {
                    return Err(CenterError::new("voice_settings_invalid", 400, "自建接入点地址无效"));
                }
                // 只备份官方地址（当前值若已是自建接入点则保留原备份）
                if old_endpoint.as_deref() != Some(endpoint.as_str()) {
                    new_voice.official_mcp_endpoint = old_endpoint.clone();
                }
                (Some(endpoint), true)
            }
            VoiceMode::Official | VoiceMode::Builtin => match old_voice.mode {
                VoiceMode::SelfHosted => (old_voice.official_mcp_endpoint.clone(), self.hooks.strict_from_env()),
                _ => (old_endpoint.clone(), old_strict),
            },
        };
        let leaving_builtin = old_voice.mode == VoiceMode::Builtin;
        let rollback = |reason: String| -> CenterError {
            let _ = settings::save_with_endpoint(&self.settings_file, &old_voice, old_endpoint.as_deref());
            self.hooks.set_strict_mode(old_strict);
            self.hooks.restart_bridge();
            if target == VoiceMode::Builtin {
                self.hooks.builtin_stop();
            } else if leaving_builtin {
                let _ = self.hooks.builtin_start();
            }
            CenterError::new("voice_mode_switch_failed", 500, format!("{reason}；已回滚到切换前的状态"))
        };
        // 1. 先开严格模式（切到自建时），保证新桥接上之前门禁已就位
        if new_strict {
            self.hooks.set_strict_mode(true);
            if !self.hooks.strict_mode() {
                return Err(rollback("严格模式开启失败".into()));
            }
        }
        // 2. 写设置（mcp_endpoint 与 voice 一次写入）
        if let Err(error) = settings::save_with_endpoint(&self.settings_file, &new_voice, new_endpoint.as_deref()) {
            return Err(rollback(format!("保存设置失败：{error}")));
        }
        // 3. 接入点变了才重连：先断旧桥再接新桥
        if new_endpoint != old_endpoint {
            let started = self.hooks.restart_bridge();
            if new_endpoint.is_some() && !started {
                return Err(rollback("新的小智桥没有启动（接入点地址无效）".into()));
            }
        }
        // 3b. 内置服务：切入时启动（失败整体回滚），离开时停止
        if target == VoiceMode::Builtin {
            if let Err(error) = self.hooks.builtin_start() {
                return Err(rollback(format!("内置语音服务启动失败：{error}")));
            }
        } else if leaving_builtin {
            self.hooks.builtin_stop();
        }
        // 4. 切回官方后严格模式恢复为 .env 设置
        if !new_strict {
            self.hooks.set_strict_mode(false);
        }
        if let Ok(mut health) = self.health.lock() {
            *health = HealthSnapshot::default();
        }
        // 官方链路无法得知设备会话；内置链路的会话数由内置服务自己上报
        if target == VoiceMode::Official {
            self.hooks.set_device_sessions(None);
        }
        let mut status = self.status();
        status["server_still_running"] = serde_json::Value::Bool(target != VoiceMode::SelfHosted && self.supervisor.owns_running_server());
        Ok(status)
    }

    // ---------- 服务端启停 ----------

    pub fn server_start(&self) -> CenterResult<serde_json::Value> {
        let voice = self.settings();
        // 全模块部署也可以由 MOSS 启动，只是模型配置不经向导写入
        let spec = ServerSpec::from_settings(&voice)?;
        self.supervisor.start(&spec)?;
        Ok(self.status())
    }

    pub fn server_stop(&self) -> CenterResult<serde_json::Value> {
        let port = self.settings().server_port;
        self.supervisor.stop(port)?;
        Ok(self.status())
    }

    /// 重启只针对 MOSS 启动的实例。
    pub fn server_restart(&self) -> CenterResult<serde_json::Value> {
        let voice = self.settings();
        let status = self.supervisor.status(voice.server_port);
        match status.state {
            ServerState::Running => {
                self.supervisor.stop(voice.server_port)?;
            }
            ServerState::External => return Err(VoiceError::NotOwned.into()),
            ServerState::Stopped | ServerState::Exited => {}
        }
        self.server_start()
    }

    /// 已脱敏的日志尾部。
    pub fn log_tail(&self, count: usize) -> Vec<String> {
        self.supervisor.log().tail(count.clamp(1, 1000))
    }

    /// MOSS 退出：`stop` 为真时停止服务端（先请求正常退出，5 秒后强制），否则放开让它继续运行。
    /// 必须在 `ShutdownCoordinator` 急停之后调用（技术书 §9.4.3）。
    pub fn shutdown(&self, stop: bool) {
        self.poller_stop.store(true, Ordering::SeqCst);
        if !self.supervisor.owns_running_server() {
            return;
        }
        if stop {
            let _ = self.supervisor.stop(self.settings().server_port);
        } else {
            let _ = self.supervisor.detach();
        }
    }

    // ---------- 模型配置向导 ----------

    /// 测试一个分区；结果（不含密钥）记在内存中供状态卡显示。
    pub fn models_test(&self, section: Section, input: &models::SectionInput) -> CenterResult<TestResult> {
        let resolved = models::resolve(section, input)
            .map_err(|error| CenterError::new("model_input_invalid", 400, error))?
            .ok_or_else(|| CenterError::new("model_input_invalid", 400, format!("{}：请填写地址与模型", section.label())))?;
        let result = models::test_section(&resolved);
        if let Ok(mut tests) = self.tests.lock() {
            tests.insert(
                section,
                TestRecord {
                    ok: result.ok,
                    elapsed_ms: result.elapsed_ms,
                    at: unix_now(),
                    detail: result.detail.clone(),
                    final_url: result.final_url.clone(),
                    fingerprint: resolved.fingerprint(),
                },
            );
        }
        Ok(result)
    }

    /// 保存到服务端配置。每个非空分区都必须与最近一次**测试通过**的内容完全一致。
    pub fn models_save(&self, input: &ModelsInput) -> CenterResult<serde_json::Value> {
        let voice = self.settings();
        let config_path = voice
            .config_path()
            .ok_or_else(|| CenterError::new("voice_config_invalid", 400, "请先在“服务端设置”中填写 xiaozhi-server 目录"))?;
        if !voice.server_dir.as_deref().map(Path::new).map(Path::is_dir).unwrap_or(false) {
            return Err(CenterError::new("voice_config_invalid", 400, "xiaozhi-server 目录不存在"));
        }
        let mut sections = Vec::new();
        for section in Section::ALL {
            let Some(raw) = input.section(section) else { continue };
            let Some(resolved) = models::resolve(section, raw).map_err(|error| CenterError::new("model_input_invalid", 400, error))? else {
                continue;
            };
            let tested = self
                .tests
                .lock()
                .ok()
                .and_then(|tests| tests.get(&section).filter(|record| record.ok).map(|record| record.fingerprint));
            if tested != Some(resolved.fingerprint()) {
                return Err(CenterError::new(
                    "model_test_required",
                    409,
                    format!("{}：请先用当前填写的内容测试连接并成功后再保存", section.label()),
                ));
            }
            sections.push(resolved);
        }
        if sections.is_empty() {
            return Err(CenterError::new("model_input_invalid", 400, "三个分区都留空，没有需要保存的内容"));
        }
        let edits = config_yaml::edits_for(&sections, voice.self_hosted_mcp_endpoint.as_deref());
        let outcome = config_yaml::write(&config_path, &edits).map_err(|error| {
            let code = if error.contains("智控台") { "server_full_module" } else { "server_config_write_failed" };
            CenterError::new(code, 409, error)
        })?;
        let _ = self.supervisor.log().write_line(&format!(
            "[MOSS] 已更新服务端配置：{}",
            sections.iter().map(|section| section.section.label()).collect::<Vec<_>>().join("、")
        ));
        Ok(serde_json::json!({
            "saved": sections.iter().map(|section| section.section.as_str()).collect::<Vec<_>>(),
            "created": outcome.created,
            "backup": outcome.backup.is_some(),
            "changed_keys": outcome.changed_keys.iter().filter(|key| !key.ends_with("api_key")).collect::<Vec<_>>(),
            "keys_written": outcome.changed_keys.iter().filter(|key| key.ends_with("api_key")).count(),
            "restart_hint": self.supervisor.owns_running_server(),
            "status": self.status(),
        }))
    }

    // ---------- 健康检查 ----------

    /// 立即检查一次接入点（自建模式且填写了健康检查地址时）。
    pub fn refresh_health(&self) {
        let voice = self.settings();
        let snapshot = match (voice.mode, voice.endpoint_health_url.as_deref()) {
            (VoiceMode::SelfHosted, Some(url)) => match health::fetch(url) {
                Ok(counts) => HealthSnapshot { counts, checked_at: Some(unix_now()), error: None },
                Err(error) => HealthSnapshot { counts: HealthCounts::default(), checked_at: Some(unix_now()), error: Some(error) },
            },
            _ => HealthSnapshot::default(),
        };
        let sessions = if voice.mode == VoiceMode::SelfHosted { snapshot.counts.robot_connections } else { None };
        self.hooks.set_device_sessions(sessions);
        if let Ok(mut guard) = self.health.lock() {
            *guard = snapshot;
        }
    }

    /// 后台每 `interval` 检查一次，直到 [`shutdown`](Self::shutdown)。
    pub fn spawn_health_poller(self: &Arc<Self>, interval: Duration) {
        let center = Arc::downgrade(self);
        let stop = self.poller_stop.clone();
        let _ = std::thread::Builder::new().name("voice-health".into()).spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Some(center) = center.upgrade() else { break };
                center.refresh_health();
                drop(center);
                let mut slept = Duration::ZERO;
                while slept < interval && !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(200));
                    slept += Duration::from_millis(200);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SectionInput;
    use crate::models::fake_http::serve_once;
    use std::sync::atomic::AtomicUsize;

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
        fn set_strict_mode(&self, enabled: bool) {
            self.strict.store(enabled || self.env_strict, Ordering::SeqCst);
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
            let known = ["material_params_tool", "open_app_tool", "laser_safe_action_tool"];
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
    const SELF_HOSTED: &str = "ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=self-token";

    fn fixture() -> (tempfile::TempDir, Arc<FakeHost>, VoiceCenter) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, format!(r#"{{"web_port": 9666, "mcp_endpoint": "{OFFICIAL}"}}"#)).unwrap();
        let host = Arc::new(FakeHost { settings_file: file.clone(), ..Default::default() });
        host.bridge_ok.store(true, Ordering::SeqCst);
        let center = VoiceCenter::new(dir.path(), host.clone());
        (dir, host, center)
    }

    /// A-60 / A-61：切到自建开启严格模式并指向自建接入点；切回官方恢复备份；两次都先断旧桥再接新桥。
    #[test]
    fn mode_switch_round_trip_restores_official_endpoint() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        assert_eq!(center.set_mode(VoiceMode::SelfHosted).unwrap_err().code, "self_hosted_endpoint_missing");
        center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 0, "官方模式下保存自建地址不重连");
        let changes = center.mode_changes(VoiceMode::SelfHosted).unwrap();
        assert!(changes.iter().any(|line| line.contains("严格模式")));
        assert!(changes.iter().all(|line| !line.contains("official-token") && !line.contains("self-token")), "{changes:?}");

        let status = center.set_mode(VoiceMode::SelfHosted).unwrap();
        assert_eq!(status["mode"], "self_hosted");
        assert_eq!(status["strict"]["enabled"], true);
        assert_eq!(status["strict"]["locked"], true);
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(SELF_HOSTED));
        assert_eq!(settings::load(&file).official_mcp_endpoint.as_deref(), Some(OFFICIAL));
        assert_eq!(host.endpoints.lock().unwrap().last().unwrap().as_deref(), Some(SELF_HOSTED));
        let shown = status.to_string();
        assert!(!shown.contains("self-token") && !shown.contains("official-token"), "{shown}");
        // 其他键保留
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(root["web_port"], 9666);

        let status = center.set_mode(VoiceMode::Official).unwrap();
        assert_eq!(status["mode"], "official");
        assert_eq!(status["strict"]["enabled"], false);
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));
        assert_eq!(host.endpoints.lock().unwrap().last().unwrap().as_deref(), Some(OFFICIAL));
        assert_eq!(host.restarts.load(Ordering::SeqCst), 2);
        // 同一模式再切一次是空操作
        center.set_mode(VoiceMode::Official).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 2);
        assert!(center.mode_changes(VoiceMode::Official).unwrap().is_empty());
    }

    /// NV5：内置链路——切入时启动内置服务、接入点与严格模式不变；离开时停止；从自建切入时恢复官方接入点。
    #[test]
    fn builtin_mode_starts_and_stops_the_builtin_service() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        host.builtin_ok.store(true, Ordering::SeqCst);
        let changes = center.mode_changes(VoiceMode::Builtin).unwrap();
        assert!(changes[0].contains("官方小智 → MOSS 内置服务"), "{changes:?}");
        assert!(changes.iter().any(|line| line.contains("回声测试模式")), "模型未配置齐全时说明以回声运行");
        assert!(changes.iter().any(|line| line.contains("严格模式未开启") && line.contains("口头确认")), "{changes:?}");
        assert!(!changes.iter().any(|line| line.contains("mcp_endpoint")), "官方切到内置不改接入点");

        let status = center.set_mode(VoiceMode::Builtin).unwrap();
        assert_eq!(status["mode"], "builtin");
        assert!(host.builtin_running.load(Ordering::SeqCst));
        assert_eq!(host.restarts.load(Ordering::SeqCst), 0, "接入点没变，不重连小智桥");
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));
        assert!(!host.strict.load(Ordering::SeqCst));
        // 内置链路下修改官方接入点立即重连（与官方模式相同）
        let other = "wss://official.example/mcp/?token=other";
        center.save_settings(&serde_json::json!({"official_mcp_endpoint": other})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);

        center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED})).unwrap();
        assert!(center.mode_changes(VoiceMode::SelfHosted).unwrap().iter().any(|line| line.contains("停止 MOSS 内置语音服务")));
        center.set_mode(VoiceMode::SelfHosted).unwrap();
        assert!(!host.builtin_running.load(Ordering::SeqCst), "离开内置链路时停止内置服务");
        assert_eq!(settings::load(&file).official_mcp_endpoint.as_deref(), Some(other));
        assert!(host.strict.load(Ordering::SeqCst));

        center.set_mode(VoiceMode::Builtin).unwrap();
        assert!(host.builtin_running.load(Ordering::SeqCst));
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(other), "从自建切入时恢复官方接入点");
        assert!(!host.strict.load(Ordering::SeqCst), "严格模式恢复为 .env 设置");
        let restarts = host.restarts.load(Ordering::SeqCst);
        center.set_mode(VoiceMode::Official).unwrap();
        assert!(!host.builtin_running.load(Ordering::SeqCst));
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts, "内置切回官方不重连");
    }

    #[test]
    fn builtin_start_failure_rolls_back() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let error = center.set_mode(VoiceMode::Builtin).unwrap_err();
        assert_eq!(error.code, "voice_mode_switch_failed");
        assert!(error.message.contains("端口 8000 已被占用"), "{error:?}");
        assert_eq!(settings::load(&file).mode, VoiceMode::Official);
        assert!(!host.builtin_running.load(Ordering::SeqCst));
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));
    }

    /// A-77：官方接入点只在语音中心修改；官方模式下保存即重连，自建模式下只改备份。
    #[test]
    fn official_endpoint_is_edited_in_voice_center() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let other = "wss://official.example/mcp/?token=new-official";
        assert_eq!(center.save_settings(&serde_json::json!({"official_mcp_endpoint": "https://x"})).unwrap_err().code, "voice_settings_invalid");
        let status = center.save_settings(&serde_json::json!({"official_mcp_endpoint": other})).unwrap();
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(other));
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
        assert_eq!(status["settings"]["official_mcp_endpoint"], "wss://official.example/mcp/?token=***");
        assert_eq!(status["reconnected"], true);
        // 相同地址再保存不重连
        let status = center.save_settings(&serde_json::json!({"official_mcp_endpoint": other})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
        assert_eq!(status["reconnected"], false);
        center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED})).unwrap();
        center.set_mode(VoiceMode::SelfHosted).unwrap();
        let restarts = host.restarts.load(Ordering::SeqCst);
        center.save_settings(&serde_json::json!({"official_mcp_endpoint": OFFICIAL})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts, "自建模式下只改备份");
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(SELF_HOSTED));
        center.set_mode(VoiceMode::Official).unwrap();
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));
        center.save_settings(&serde_json::json!({"clear_official_mcp_endpoint": true})).unwrap();
        assert_eq!(settings::mcp_endpoint(&file), None);
    }

    /// A-61：新桥启动失败时整体回滚（设置、严格模式、原来的桥）。
    #[test]
    fn failed_switch_rolls_back_everything() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED})).unwrap();
        let before = std::fs::read(&file).unwrap();
        host.bridge_ok.store(false, Ordering::SeqCst);
        let error = center.set_mode(VoiceMode::SelfHosted).unwrap_err();
        assert_eq!(error.code, "voice_mode_switch_failed");
        assert!(error.message.contains("回滚"));
        let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let before: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(after, before);
        assert!(!host.strict_mode(), "严格模式恢复为切换前");
        // 回滚后重新按旧地址接桥
        assert_eq!(host.endpoints.lock().unwrap().last().unwrap().as_deref(), Some(OFFICIAL));
        assert_eq!(settings::load(&file).mode, VoiceMode::Official);
    }

    /// `.env` 开启严格模式时，切回官方后仍保持开启。
    #[test]
    fn env_strict_survives_switch_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let host = Arc::new(FakeHost { settings_file: file.clone(), env_strict: true, ..Default::default() });
        host.strict.store(true, Ordering::SeqCst);
        host.bridge_ok.store(true, Ordering::SeqCst);
        let center = VoiceCenter::new(dir.path(), host.clone());
        center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED})).unwrap();
        center.set_mode(VoiceMode::SelfHosted).unwrap();
        // 没有官方备份：切回官方时删除 mcp_endpoint，桥不启动也不算失败
        let status = center.set_mode(VoiceMode::Official).unwrap();
        assert_eq!(status["strict"]["enabled"], true);
        assert_eq!(settings::mcp_endpoint(&file), None);
    }

    #[test]
    fn settings_validation_and_self_hosted_endpoint_change_reconnects() {
        let (_dir, host, center) = fixture();
        for bad in [
            serde_json::json!({"self_hosted_mcp_endpoint": "http://x"}),
            serde_json::json!({"self_hosted_mcp_endpoint": "ws://<host>/mcp"}),
            serde_json::json!({"endpoint_health_url": "ws://x"}),
            serde_json::json!({"server_port": 0}),
        ] {
            assert_eq!(center.save_settings(&bad).unwrap_err().code, "voice_settings_invalid", "{bad}");
        }
        center
            .save_settings(&serde_json::json!({"server_dir": " D:/srv ", "python": "py", "args": "app.py --x", "server_port": 8100, "self_hosted_mcp_endpoint": SELF_HOSTED}))
            .unwrap();
        let voice = center.settings();
        assert_eq!(voice.server_dir.as_deref(), Some("D:/srv"));
        assert_eq!(voice.args, vec!["app.py".to_string(), "--x".to_string()]);
        assert_eq!(voice.server_port, 8100);
        center.save_settings(&serde_json::json!({"builtin_person_memory_enabled":true})).unwrap();
        assert!(!center.settings().builtin_person_memory_enabled, "普通窗口设置不能绕过私有窗口的标定与同意门禁");
        center.save_settings(&serde_json::json!({"builtin_context":{"retrieval":true,"extraction":true,"project_id":"p-x"}})).unwrap();
        assert_eq!(center.settings().builtin_context, crate::settings::BuiltinContext::default(), "主窗口设置不能打开长期记忆或改当前项目");
        center.set_mode(VoiceMode::SelfHosted).unwrap();
        let restarts = host.restarts.load(Ordering::SeqCst);
        // 留空表示不改
        let status = center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": ""})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts);
        assert_eq!(status["reconnected"], false, "没有重连时不能报告已重连");
        let status = center.save_settings(&serde_json::json!({"endpoint_health_url": "http://127.0.0.1:8004/mcp_endpoint/health?key=k"})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts);
        assert_eq!(status["reconnected"], false, "只改健康检查地址不重连");
        let other = "ws://127.0.0.1:8005/mcp_endpoint/mcp/?token=other";
        let status = center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": other})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts + 1);
        assert_eq!(status["reconnected"], true);
        assert_eq!(host.endpoints.lock().unwrap().last().unwrap().as_deref(), Some(other));
        // 发起了重连但没有桥启动：不能报告正在重连
        host.bridge_ok.store(false, Ordering::SeqCst);
        let status = center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts + 2);
        assert_eq!(status["reconnected"], false);
        host.bridge_ok.store(true, Ordering::SeqCst);
        assert_eq!(center.save_settings(&serde_json::json!({"clear_self_hosted_mcp_endpoint": true})).unwrap_err().code, "voice_settings_invalid");
    }

    /// A-67：测试不落盘；保存必须先测试通过；密钥只出现在服务端配置文件中。
    #[test]
    fn models_require_successful_test_and_keys_stay_out_of_moss_storage() {
        let (dir, _host, center) = fixture();
        let server_dir = dir.path().join("xiaozhi-server");
        std::fs::create_dir_all(&server_dir).unwrap();
        center
            .save_settings(&serde_json::json!({"server_dir": server_dir.to_string_lossy(), "python": "python"}))
            .unwrap();
        let fake_key = "sk-moss-fake-key-A67";
        let reply = br#"{"choices":[{"message":{"tool_calls":[{"id":"1","type":"function","function":{"name":"get_time","arguments":"{}"}}]}}]}"#;
        let (base, _rx) = serve_once("HTTP/1.1 200 OK", "Content-Type: application/json\r\n", reply.to_vec());
        let llm = SectionInput { url: format!("{base}/v1"), api_key: fake_key.into(), model: "m".into(), ..Default::default() };
        let input = ModelsInput { llm: Some(llm.clone()), ..Default::default() };
        // 未测试不能保存
        assert_eq!(center.models_save(&input).unwrap_err().code, "model_test_required");
        let result = center.models_test(Section::Llm, &llm).unwrap();
        assert!(result.ok, "{result:?}");
        // 测试不落盘
        assert!(!server_dir.join("data").join(".config.yaml").exists());
        // 测试后改了内容也不能保存
        let changed = ModelsInput { llm: Some(SectionInput { model: "m2".into(), ..llm.clone() }), ..Default::default() };
        assert_eq!(center.models_save(&changed).unwrap_err().code, "model_test_required");
        let saved = center.models_save(&input).unwrap();
        assert_eq!(saved["saved"][0], "llm");
        assert_eq!(saved["keys_written"], 1);
        let config = std::fs::read_to_string(server_dir.join("data").join(".config.yaml")).unwrap();
        assert!(config.contains(fake_key));
        // MOSS 的 settings.json、日志、返回给页面的状态中都没有密钥
        for path in [dir.path().join("settings.json"), dir.path().join("logs").join("xiaozhi-server.log")] {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(!text.contains(fake_key), "{} 中出现了密钥", path.display());
        }
        assert!(!saved.to_string().contains(fake_key));
        assert!(!serde_json::to_string(&result).unwrap().contains(fake_key));
        assert_eq!(center.status()["models"]["llm"]["ok"], true);
        // 全模块部署：拒绝写入
        std::fs::write(server_dir.join("data").join(".config.yaml"), "manager-api:\n  url: http://127.0.0.1:8002/xiaozhi\n").unwrap();
        assert_eq!(center.models_save(&input).unwrap_err().code, "server_full_module");
        assert_eq!(center.status()["server_config"]["full_module"], true);
    }

    #[test]
    fn health_updates_device_sessions_only_in_self_hosted_mode() {
        let (_dir, host, center) = fixture();
        let (base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            br#"{"data":{"robot_connections":1,"tool_connections":1}}"#.to_vec(),
        );
        center
            .save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED, "endpoint_health_url": format!("{base}/mcp_endpoint/health?key=k")}))
            .unwrap();
        center.refresh_health();
        assert_eq!(*host.sessions.lock().unwrap(), None, "官方模式不提供设备会话");
        assert_eq!(center.status()["device"]["state"], "unknown");
        center.set_mode(VoiceMode::SelfHosted).unwrap();
        center.refresh_health();
        assert_eq!(*host.sessions.lock().unwrap(), Some(1));
        let status = center.status();
        assert_eq!(status["device"]["state"], "online");
        assert_eq!(status["endpoint"]["health"]["tool_connections"], 1);
        assert!(!status.to_string().contains("key=k"));
        // 接入点不可达：未知，不猜
        center.refresh_health();
        assert_eq!(center.status()["device"]["state"], "unknown");
        assert_eq!(*host.sessions.lock().unwrap(), None);
    }

    #[test]
    fn server_errors_map_to_codes() {
        let (_dir, _host, center) = fixture();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        center.save_settings(&serde_json::json!({"server_port": port})).unwrap();
        assert_eq!(center.server_start().unwrap_err().code, "voice_config_invalid");
        assert_eq!(center.server_stop().unwrap_err().code, "server_not_running");
        let status = center.status();
        assert_eq!(status["server"]["state"], "stopped");
        assert_eq!(status["gateway"]["enabled"], false);
        center.shutdown(true);
    }

    /// NV8：附加接入点——校验、上限、去重、按编号删除；官方模式下改动立即重连，自建模式下只保存并在切换说明里提示暂停。
    #[test]
    fn extra_endpoints_are_validated_saved_and_reconnect_outside_self_hosted_mode() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let extra = |n: usize| format!("wss://official.example/mcp/?token=extra-{n}");
        for (bad, why) in [
            ("http://official.example/mcp/", "以 ws:// 或 wss://"),
            ("wss://x/mcp/?token=<your-token>", "占位符"),
            (OFFICIAL, "已经在列表中"),
        ] {
            let error = center.save_settings(&serde_json::json!({"add_extra_mcp_endpoint": bad})).unwrap_err();
            assert!(error.message.contains(why), "{bad}: {error:?}");
        }
        for n in 1..=MAX_EXTRA_ENDPOINTS {
            center.save_settings(&serde_json::json!({"add_extra_mcp_endpoint": extra(n)})).unwrap();
        }
        assert_eq!(host.restarts.load(Ordering::SeqCst), MAX_EXTRA_ENDPOINTS, "官方模式下每次改动都重连");
        let error = center.save_settings(&serde_json::json!({"add_extra_mcp_endpoint": extra(9)})).unwrap_err();
        assert!(error.message.contains("最多"), "{error:?}");
        let error = center.save_settings(&serde_json::json!({"add_extra_mcp_endpoint": extra(2)})).unwrap_err();
        assert!(error.message.contains("已经在列表中"));
        center.save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-2"})).unwrap();
        assert_eq!(settings::extra_mcp_endpoints(&file), vec![extra(1), extra(3), extra(4)]);
        for bad in ["extra-0", "extra-4", "primary", "extra-x"] {
            assert!(center.save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": bad})).is_err(), "{bad}");
        }
        // 其他设置原样保留，主接入点不受影响
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(root["web_port"], 9666);
        assert_eq!(settings::mcp_endpoint(&file).as_deref(), Some(OFFICIAL));

        // 自建模式：附加接入点暂停连接，改动只保存不重连；切换说明里提示
        center.save_settings(&serde_json::json!({"self_hosted_mcp_endpoint": SELF_HOSTED})).unwrap();
        let changes = center.mode_changes(VoiceMode::SelfHosted).unwrap();
        assert!(changes.iter().any(|line| line.contains("3 个附加官方接入点暂停连接")), "{changes:?}");
        host.bridge_ok.store(true, Ordering::SeqCst);
        center.set_mode(VoiceMode::SelfHosted).unwrap();
        let restarts = host.restarts.load(Ordering::SeqCst);
        center.save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-1"})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), restarts, "自建模式下附加接入点不连接，改动不重连");
        assert_eq!(settings::extra_mcp_endpoints(&file), vec![extra(3), extra(4)]);
        // 删光后不留空键
        center.save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-1"})).unwrap();
        center.save_settings(&serde_json::json!({"remove_extra_mcp_endpoint": "extra-1"})).unwrap();
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(root.get("mcp_extra_endpoints").is_none());
    }

    /// NV8：工具开关——后端校验后立即生效并写入 settings.json，然后重连让接入点重新获取工具清单；
    /// 不能关闭或不存在的工具被拒绝，什么也不改；名单没变不重连。
    #[test]
    fn tool_switches_are_applied_saved_and_reconnect() {
        let (dir, host, center) = fixture();
        let file = dir.path().join("settings.json");
        let status = center.save_settings(&serde_json::json!({"disabled_tools": ["material_params_tool", "open_app_tool"]})).unwrap();
        assert_eq!(settings::disabled_tools(&file), vec!["material_params_tool".to_string(), "open_app_tool".to_string()]);
        assert_eq!(*host.disabled.lock().unwrap(), settings::disabled_tools(&file));
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1);
        assert_eq!(status["mcp"]["disabled"], serde_json::json!(["material_params_tool", "open_app_tool"]));

        let error = center.save_settings(&serde_json::json!({"disabled_tools": ["laser_safe_action_tool"]})).unwrap_err();
        assert!(error.message.contains("不能关闭"), "{error:?}");
        assert!(center.save_settings(&serde_json::json!({"disabled_tools": "material_params_tool"})).is_err());
        assert!(center.save_settings(&serde_json::json!({"disabled_tools": [1]})).is_err());
        assert_eq!(settings::disabled_tools(&file).len(), 2, "被拒绝的改动不写文件");
        assert_eq!(host.disabled.lock().unwrap().len(), 2, "被拒绝的改动不生效");

        center.save_settings(&serde_json::json!({"disabled_tools": ["open_app_tool", "material_params_tool"]})).unwrap();
        assert_eq!(host.restarts.load(Ordering::SeqCst), 1, "名单没变不重连");
        center.save_settings(&serde_json::json!({"disabled_tools": []})).unwrap();
        assert!(settings::disabled_tools(&file).is_empty());
        assert_eq!(host.restarts.load(Ordering::SeqCst), 2);
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(root.get("mcp_disabled_tools").is_none(), "全部打开后不留空键");
    }
}
