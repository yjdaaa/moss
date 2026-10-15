//! 语音中心设置（技术书 §9.4.2 / §9.4.3，实施清单 P4-01）。
//!
//! 存放在运行目录 `settings.json` 的 `voice` 对象中，读-改-写时保留其他键；顶层 `mcp_endpoint`
//! 仍由原有设置使用。切到自建模式前把当时的官方 `mcp_endpoint` 备份到
//! `voice.official_mcp_endpoint`，切回官方时恢复。模型密钥**不**保存在这里：外部服务端链路只写服务端配置文件，
//! 内置语音服务（NV5）的密钥存 SQLite `app_secrets`（名称见 [`builtin_secret_name`]），这里只存地址与模型名。

mod home_transfer;
pub use home_transfer::{export_home_template, parse_home_template};

use std::path::{Path, PathBuf};

static SETTINGS_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

use serde::{Deserialize, Serialize};

use crate::models::{ProviderKind, Section};
use crate::local_models::{ModelKind, ModelSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceMode {
    /// 官方小智（默认，保持现状）
    #[default]
    Official,
    /// 自建 xiaozhi-server + 接入点
    SelfHosted,
    /// MOSS 内置语音服务（内置语音技术书，NV5）：设备 OTA 指向 MOSS，内置服务随 MOSS 启动
    Builtin,
}

impl VoiceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::SelfHosted => "self_hosted",
            Self::Builtin => "builtin",
        }
    }
}

fn default_args() -> Vec<String> {
    vec!["app.py".into()]
}

fn default_server_port() -> u16 {
    8000
}

fn default_builtin_ws_port() -> u16 {
    8000
}

fn default_builtin_http_port() -> u16 {
    8003
}

fn default_speed() -> f32 {
    1.0
}

/// 内置语音服务的一个模型提供方。地址已规范化：OpenAI 兼容时大模型为基础地址、识别 / 合成为完整地址；
/// 豆包流式（NV7）为 `wss://` 地址，`model` 为资源 ID。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinProvider {
    /// 接口类型；旧版设置没有此字段，按 OpenAI 兼容读取
    #[serde(default, skip_serializing_if = "ProviderKind::is_openai")]
    pub kind: ProviderKind,
    pub url: String,
    pub model: String,
    /// 合成音色（只用于语音合成）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice: String,
    /// 合成语速（只用于语音合成）
    #[serde(default = "default_speed")]
    pub speed: f32,
}

/// 内置语音服务的三个模型（NV5）。三个都配置后内置服务以对话模式启动，否则为回声模式。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BuiltinModels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<BuiltinProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr: Option<BuiltinProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts: Option<BuiltinProvider>,
}

impl BuiltinModels {
    pub fn is_empty(&self) -> bool {
        self.llm.is_none() && self.asr.is_none() && self.tts.is_none()
    }

    pub fn get(&self, section: Section) -> Option<&BuiltinProvider> {
        match section {
            Section::Llm => self.llm.as_ref(),
            Section::Asr => self.asr.as_ref(),
            Section::Tts => self.tts.as_ref(),
        }
    }

    pub fn set(&mut self, section: Section, provider: Option<BuiltinProvider>) {
        match section {
            Section::Llm => self.llm = provider,
            Section::Asr => self.asr = provider,
            Section::Tts => self.tts = provider,
        }
    }

    pub fn complete(&self) -> bool {
        self.llm.is_some() && self.asr.is_some() && self.tts.is_some()
    }
}

fn default_speaker() -> i32 {
    50
}

fn default_vad_model() -> String {
    "silero_vad".into()
}

fn default_asr_model() -> String {
    "sense_voice".into()
}

fn default_tts_model() -> String {
    "kokoro".into()
}
fn default_on() -> bool { true }
fn default_barge_in_ms() -> u32 { 400 }

pub fn validate_barge_in_ms(ms: u64) -> Result<u32, String> {
    if (300..=600).contains(&ms) { Ok(ms as u32) }
    else { Err("插话时长须为 300 ~ 600 毫秒的整数".into()) }
}

pub fn normalize_hotwords(words: &[String]) -> Result<Vec<String>, String> {
    if words.len() > 64 { return Err("自定义热词最多 64 条".into()); }
    let mut normalized = Vec::new();
    for word in words {
        if word.chars().any(char::is_control) { return Err("热词不能包含控制字符".into()); }
        let word = word.trim();
        if word.is_empty() { continue; }
        if word.chars().count() > 32 { return Err("每条热词最多 32 个字符".into()); }
        if !normalized.iter().any(|v| v == word) { normalized.push(word.to_owned()); }
    }
    if normalized.iter().map(String::len).sum::<usize>() > 4096 { return Err("热词总长度超过 4096 字节".into()); }
    Ok(normalized)
}

/// 顺序保留自定义词、材料名称和行业词；运行时只取有界集合，不写入数据库。
pub fn effective_hotwords(custom: &[String], materials: &[String]) -> Vec<String> {
    let industry = ["激光", "雕刻", "切割", "功率", "速度", "椴木", "亚克力", "牛皮纸", "急停", "停止", "原点", "毫米"];
    let mut words = Vec::new();
    for word in custom.iter().chain(materials.iter()).map(String::as_str).chain(industry) {
        if words.len() >= 64 { break; }
        if word.is_empty() || word.chars().count() > 32 || word.chars().any(char::is_control) || words.iter().any(|v| v == word) { continue; }
        words.push(word.to_owned());
    }
    words
}

/// 内置语音服务的本地模型开关与选用的模型（NV6；可选模型见技术书 §13.7）。
/// 模型文件在运行目录 `models/`，由辅助程序 `moss-voice-local` 运行。
/// 旧设置没有 `*_model` 字段，按 NV6 的默认模型（Silero、SenseVoice 2024、Kokoro v1.0）读取，行为不变。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinLocal {
    /// 仅设备 realtime + 设备端 AEC 可用；旧设置默认关闭。
    #[serde(default)]
    pub barge_in: bool,
    #[serde(default = "default_barge_in_ms")]
    pub barge_in_ms: u32,
    /// NV8a 体验设置（也应用于云端识别）；停止快路径不可关闭。
    #[serde(default = "default_on")]
    pub read_only_shortcuts: bool,
    #[serde(default = "default_on")]
    pub asr_beam_search: bool,
    #[serde(default)]
    pub custom_hotwords: Vec<String>,
    /// 语音识别用本地模型（代替云端识别）
    #[serde(default)]
    pub asr: bool,
    /// 语音合成用本地模型（代替云端合成）
    #[serde(default)]
    pub tts: bool,
    /// 自动拾音的判停用本地模型（否则用能量判停）
    #[serde(default)]
    pub vad: bool,
    /// 选用的判停模型（`local_models::CATALOG` 中的 id）
    #[serde(default = "default_vad_model")]
    pub vad_model: String,
    #[serde(default = "default_asr_model")]
    pub asr_model: String,
    #[serde(default = "default_tts_model")]
    pub tts_model: String,
    /// 合成音色编号（多音色模型；单音色模型忽略）
    #[serde(default = "default_speaker")]
    pub tts_speaker: i32,
    #[serde(default = "default_speed")]
    pub tts_speed: f32,
    /// 模型下载镜像（发布地址前缀）；为空用官方地址。下载后一律校验 SHA-256
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<String>,
}

impl Default for BuiltinLocal {
    fn default() -> Self {
        Self {
            barge_in: false,
            barge_in_ms: default_barge_in_ms(),
            read_only_shortcuts: true,
            asr_beam_search: true,
            custom_hotwords: Vec::new(),
            asr: false,
            tts: false,
            vad: false,
            vad_model: default_vad_model(),
            asr_model: default_asr_model(),
            tts_model: default_tts_model(),
            tts_speaker: default_speaker(),
            tts_speed: default_speed(),
            mirror: None,
        }
    }
}

impl BuiltinLocal {
    pub fn any(&self) -> bool {
        self.asr || self.tts || self.vad
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// 某类别打开了本地开关时选用的模型（开关关闭或模型 id 无效时为空）。
    pub fn selected(&self, kind: ModelKind) -> Option<&'static ModelSpec> {
        let (enabled, id) = match kind {
            ModelKind::Vad => (self.vad, &self.vad_model),
            ModelKind::Asr => (self.asr, &self.asr_model),
            ModelKind::Tts => (self.tts, &self.tts_model),
            ModelKind::Speaker => return None,
        };
        enabled.then(|| crate::local_models::spec_of(id, kind)).flatten()
    }
}

/// 内置语音模型密钥在 `app_secrets` 中的名称（内置语音技术书 §6：`voice.<类别>.<名称>`）。
pub fn builtin_secret_name(section: Section) -> &'static str {
    match section {
        Section::Llm => "voice.llm.main",
        Section::Asr => "voice.asr.main",
        Section::Tts => "voice.tts.main",
    }
}

/// NV11 opt-in information tools; API secrets live only in app_secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltinPlugins {
    pub weather_enabled: bool,
    pub weather_host: String,
    pub weather_latitude: Option<f64>,
    pub weather_longitude: Option<f64>,
    pub weather_auth: String,
    pub lunar_enabled: bool,
    pub lunar_utc_offset_minutes: i32,
    pub news_enabled: bool,
    pub news_sources: Vec<NewsSource>,
    pub search_enabled: bool,
    pub search_provider: SearchProvider,
    pub search_host: String,
    pub metaso_host: String,
    pub rag_enabled: bool,
    pub rag_host: String,
    pub rag_dataset_ids: Vec<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewsSourceKind { #[default] Rss, Newsnow }
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchProvider { #[default] Tavily, Metaso }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsSource {
    pub id: String,
    #[serde(default)]
    pub kind: NewsSourceKind,
    #[serde(default)]
    pub source_id: String,
    pub url: String,
}
impl Default for BuiltinPlugins {
    fn default() -> Self {
        Self {
            weather_enabled: false, weather_host: String::new(),
            weather_latitude: None, weather_longitude: None,
            weather_auth: "api_key".into(),
            lunar_enabled: false, lunar_utc_offset_minutes: 480,
            news_enabled: false, news_sources: vec![],
            search_enabled: false, search_host: "https://api.tavily.com".into(),
            search_provider: SearchProvider::Tavily, metaso_host: "https://metaso.cn".into(),
            rag_enabled: false, rag_host: String::new(), rag_dataset_ids: vec![],
        }
    }
}

/// NV13: CLI task configuration only. The desktop cannot mark itself sandbox-verified.
/// Paths/flags are inert until a separately authorized live sandbox negative test.
#[derive(Debug,Clone,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default)]
pub struct BuiltinAgents {
    pub enabled:bool,
    pub workspace:String,
    pub codex_executable:String,
    pub claude_executable:String,
}

/// NV12 independent opt-in vision configuration. Credentials live in SQLite app_secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltinVision {
    pub enabled: bool,
    pub url: String,
    pub model: String,
}

/// NV19: long-term context. Retrieval and extraction are separately opt-in and off for every
/// existing and new installation. The current project is chosen in the private window; its
/// generation changes on every switch so in-flight work for the old project is discarded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltinContext {
    pub retrieval: bool,
    pub extraction: bool,
    pub project_id: String,
    pub project_name: String,
    pub project_generation: u64,
}

/// NV14: local music stays disabled for all existing installations.
/// The directory is an explicitly selected local path; never probe user home.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltinMusic {
    pub enabled: bool,
    pub directory: String,
}


/// NV15 desktop-authored Home Assistant allowlist, never a control channel.
/// Default disabled. No API tokens, network calls or credentials are stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuiltinHomeAssistant {
    pub enabled: bool,
    pub base_url: String,
    pub entities: Vec<BuiltinHomeEntity>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinHomeEntity {
    pub entity_id: String,
    pub alias: String,
    pub enabled: bool,
    pub policy: HomeApprovalPreference,
    #[serde(default)]
    pub media:Vec<BuiltinHomeMedia>,
}
#[derive(Debug,Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinHomeMedia {pub id:String,pub media_type:String}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HomeApprovalPreference {
    #[default]
    Ask,
    Allow,
}
impl BuiltinHomeAssistant {
    /// Checks inert settings only; NEVER authorizes real Home Assistant I/O.
    pub fn validate_offline(&self) -> Result<(), &'static str> {
        use std::collections::HashSet;
        if self.enabled { return Err("Home Assistant 真实认证和审批未通过，禁止启用"); }
        if self.base_url.len() > 512 || self.base_url.chars().any(char::is_control) {
            return Err("Home Assistant URL 不符合安全格式");
        }
        if !self.base_url.is_empty() {
            let url = reqwest::Url::parse(&self.base_url)
                .map_err(|_| "Home Assistant URL 格式错误")?;
            if url.scheme() != "https" || url.host_str().is_none()
                || !url.username().is_empty() || url.password().is_some()
                || url.query().is_some() || url.fragment().is_some()
                || url.path() != "/" {
                return Err("必须填写不含账号、密码、参数和子路径的 HTTPS URL");
            }
        }
        if self.entities.len() > 128 { return Err("最多允许配置128个实体"); }
        let mut ids = HashSet::new();
        let mut aliases = HashSet::new();
        for entry in &self.entities {
            let mut media_ids=HashSet::new();
            if entry.media.len()>20 || entry.media.iter().any(|m| {
                entry.entity_id.split('.').next()!=Some("media_player") || !matches!(m.media_type.as_str(),"music"|"playlist")
                    || !m.id.starts_with("library_") || m.id.len()>100 || !m.id.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'))
                    || !media_ids.insert(m.id.clone())
            }) {return Err("音乐只接受 media_player 的本地 library_ 白名单，最多20项且不可重复");}
            let Some((domain, name)) = entry.entity_id.split_once('.') else {
                return Err("实体 ID 格式错误");
            };
            if !matches!(domain, "light"|"switch"|"climate"|"lock"
                |"alarm_control_panel"|"cover"|"media_player")
                || name.is_empty() || entry.entity_id.len() > 128
                || !name.bytes().all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte,b'_'|b'-')) {
                return Err("实体 ID 不在允许的设备分类内");
            }
            let alias = entry.alias.trim().to_lowercase();
            if alias.is_empty() || alias.chars().count() > 64
                || alias.chars().any(char::is_control)
                || !ids.insert(entry.entity_id.to_ascii_lowercase()) || !aliases.insert(alias) {
                return Err("设备 ID 或别名重复或格式无效");
            }
            if matches!(domain,"lock"|"alarm_control_panel"|"cover")
                && entry.policy == HomeApprovalPreference::Allow {
                return Err("门锁、报警和窗帘必须每次独立审批");
            }
        }
        // Reserve every legal Home Assistant entity-shaped name, including
        // IDs not yet selected or discovered by this desktop. Otherwise a
        // model request spelling an unlisted ID could accidentally target an
        // unrelated listed device through a user-defined alias.
        if self.entities.iter().any(|entry| {
            let alias = entry.alias.trim().to_lowercase();
            let looks_like_id = alias.split_once('.').is_some_and(|(domain, name)| {
                matches!(domain, "light"|"switch"|"climate"|"lock"
                    |"alarm_control_panel"|"cover"|"media_player")
                    && !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'_'|b'-'))
            });
            alias != entry.entity_id.to_ascii_lowercase() && (ids.contains(&alias) || looks_like_id)
        }) {
            return Err("设备别名不得伪装成其他 Home Assistant 实体 ID");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceSettings {
    /// Third-party MCP: credentials are references only; writes use save_mcp_servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<moss_mcp_client::ServerConfig>,
    #[serde(default)]
    pub mode: VoiceMode,
    /// xiaozhi-server 源码目录（`main/xiaozhi-server`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_dir: Option<String>,
    /// Python 3.10 解释器路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    /// 启动参数，默认 `["app.py"]`
    #[serde(default = "default_args")]
    pub args: Vec<String>,
    /// 服务端 WebSocket 端口（用于识别外部已运行的实例），默认 8000
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    /// 自建接入点地址（写入 MOSS 的 `mcp_endpoint` 与服务端配置的 `mcp_endpoint`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_hosted_mcp_endpoint: Option<String>,
    /// 切到自建前备份的官方接入点地址
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_mcp_endpoint: Option<String>,
    /// 接入点健康检查地址（`…/mcp_endpoint/health?key=…`），用于判断设备是否在线；不填则显示“未知”
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_health_url: Option<String>,
    /// 内置语音服务的设备连接（WebSocket）端口，默认 8000（内置语音技术书 V3）
    #[serde(default = "default_builtin_ws_port")]
    pub builtin_ws_port: u16,
    /// 内置语音服务的 OTA 端口，默认 8003
    #[serde(default = "default_builtin_http_port")]
    pub builtin_http_port: u16,
    /// 内置语音服务的模型（不含密钥）
    #[serde(default, skip_serializing_if = "BuiltinModels::is_empty")]
    pub builtin_models: BuiltinModels,
    /// NV13 stays disabled until an externally verified read-only CLI policy exists.
    #[serde(default)]
    pub builtin_agents:BuiltinAgents,
    /// NV15 inert Home Assistant settings, disabled until real authorization.
    #[serde(default)]
    pub builtin_home: BuiltinHomeAssistant,
    /// NV14: opt-in local music, no directory scanning while disabled.
    #[serde(default)]
    pub builtin_music: BuiltinMusic,
    /// NV12: vision is disabled for all existing and new installations.
    #[serde(default)]
    pub builtin_vision: BuiltinVision,
    /// NV11 plugins: independently disabled for all existing installations.
    #[serde(default)]
    pub builtin_plugins: BuiltinPlugins,
    /// NV10: opt-in memory summaries, disabled for existing installations.
    #[serde(default)]
    pub builtin_memory_enabled: bool,
    /// Private opt-in only; calibration is a separate fail-closed prerequisite.
    #[serde(default)]
    pub builtin_person_memory_enabled:bool,
    /// NV19: long-term context switches and current project, all off by default.
    #[serde(default)]
    pub builtin_context: BuiltinContext,
    /// 内置语音服务的本地模型开关（NV6）
    #[serde(default, skip_serializing_if = "BuiltinLocal::is_default")]
    pub builtin_local: BuiltinLocal,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            mode: VoiceMode::Official,
            mcp_servers: Vec::new(),
            server_dir: None,
            python: None,
            args: default_args(),
            server_port: default_server_port(),
            self_hosted_mcp_endpoint: None,
            official_mcp_endpoint: None,
            endpoint_health_url: None,
            builtin_ws_port: default_builtin_ws_port(),
            builtin_http_port: default_builtin_http_port(),
            builtin_models: BuiltinModels::default(),
            builtin_plugins: BuiltinPlugins::default(),
            builtin_agents:BuiltinAgents::default(),
            builtin_home: BuiltinHomeAssistant::default(),
            builtin_music: BuiltinMusic::default(),
            builtin_vision: BuiltinVision::default(),
            builtin_memory_enabled: false,
            builtin_person_memory_enabled:false,
            builtin_context: BuiltinContext::default(),
            builtin_local: BuiltinLocal::default(),
        }
    }
}

impl VoiceSettings {
    /// 某个分区实际是否可用：本地开关打开且模型已安装，或云端已配置（大模型只有云端）。
    pub fn builtin_section_ready(&self, section: Section, models_root: &Path) -> bool {
        let local = |kind| {
            self.builtin_local.selected(kind).is_some_and(|spec| crate::local_models::installed(models_root, spec))
        };
        match section {
            Section::Llm => self.builtin_models.llm.is_some(),
            Section::Asr => local(ModelKind::Asr) || self.builtin_models.asr.is_some(),
            Section::Tts => local(ModelKind::Tts) || self.builtin_models.tts.is_some(),
        }
    }

    /// 三个分区都可用时内置服务以对话模式运行。
    pub fn builtin_chat_ready(&self, models_root: &Path) -> bool {
        Section::ALL.into_iter().all(|section| self.builtin_section_ready(section, models_root))
    }

    /// 服务端配置文件 `data/.config.yaml` 的位置（未设置目录时为空）。
    pub fn config_path(&self) -> Option<PathBuf> {
        self.server_dir
            .as_deref()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
            .map(|dir| Path::new(dir).join("data").join(".config.yaml"))
    }
}

fn read_root(settings_file: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(settings_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn write_root(settings_file: &Path, root: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    if let Some(parent) = settings_file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("创建运行目录失败: {error}"))?;
    }
    let text = serde_json::to_string_pretty(root).map_err(|error| error.to_string())?;
    let temp = settings_file.with_extension("json.tmp");
    std::fs::write(&temp, text).map_err(|error| format!("写入设置失败: {error}"))?;
    std::fs::rename(&temp, settings_file).map_err(|error| format!("保存设置失败: {error}"))
}

/// 读取语音设置；文件缺失、损坏或旧版本没有 `voice` 时返回默认值。
pub fn load(settings_file: &Path) -> VoiceSettings {
    read_root(settings_file)
        .get("voice")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// 保存语音设置，保留 `settings.json` 中的其他键。
pub fn save(settings_file: &Path, voice: &VoiceSettings) -> Result<(), String> {
    save_all(settings_file, voice, None, None, None)
}

/// 当前顶层 `mcp_endpoint`（MOSS 小智桥使用的地址）。
pub fn mcp_endpoint(settings_file: &Path) -> Option<String> {
    read_root(settings_file)
        .get("mcp_endpoint")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// 一次写入顶层 `mcp_endpoint` 与 `voice`（链路切换用，避免中途状态）。`None` 表示删除 `mcp_endpoint`。
pub fn save_with_endpoint(settings_file: &Path, voice: &VoiceSettings, endpoint: Option<&str>) -> Result<(), String> {
    save_all(settings_file, voice, Some(endpoint), None, None)
}

/// 附加接入点（顶层 `mcp_extra_endpoints`，NV8）。
pub fn extra_mcp_endpoints(settings_file: &Path) -> Vec<String> {
    string_list(&read_root(settings_file), "mcp_extra_endpoints")
}

/// 用户关闭的工具（顶层 `mcp_disabled_tools`，NV8；MOSS 后端启动时读取）。
pub fn disabled_tools(settings_file: &Path) -> Vec<String> {
    string_list(&read_root(settings_file), "mcp_disabled_tools")
}

fn string_list(root: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    root.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// 一次写入 `voice` 与可选的顶层键（避免中途状态）：`endpoint` 为 `Some(None)` 时删除 `mcp_endpoint`；
/// 列表为空时删除对应的键。
pub fn save_all(
    settings_file: &Path,
    voice: &VoiceSettings,
    endpoint: Option<Option<&str>>,
    extra_endpoints: Option<&[String]>,
    disabled_tools: Option<&[String]>,
) -> Result<(), String> {
    let _guard = SETTINGS_WRITE.lock().map_err(|_| "设置锁不可用")?;
    let mut root = read_root(settings_file);
    let external = root.get("voice").and_then(|v| v.get("mcp_servers")).cloned();
    let home = root.get("voice").and_then(|v| v.get("builtin_home")).cloned();
    let home_revision = root.get("voice").and_then(|v| v.get("builtin_home_revision")).cloned();
    let mut updated = serde_json::to_value(voice).map_err(|error| error.to_string())?;
    // Generic saves must not overwrite independently maintained MCP or Home
    // settings; in particular, a stale voice form cannot undo home revocation.
    if let Some(external) = external { updated["mcp_servers"] = external; }
    else if let Some(object) = updated.as_object_mut() { object.remove("mcp_servers"); }
    // Only the version-checked home writer may introduce or modify HA settings.
    // A generic settings save must never enable HA on a fresh installation.
    updated["builtin_home"] = home.unwrap_or_else(|| serde_json::json!({
        "enabled":false,"base_url":"","entities":[]
    }));
    if let Some(home_revision) = home_revision { updated["builtin_home_revision"] = home_revision; }
    else if let Some(object) = updated.as_object_mut() { object.remove("builtin_home_revision"); }
    root.insert("voice".into(), updated);
    if let Some(endpoint) = endpoint {
        match endpoint.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => {
                root.insert("mcp_endpoint".into(), serde_json::Value::String(value.to_owned()));
            }
            None => {
                root.remove("mcp_endpoint");
            }
        }
    }
    for (key, list) in [("mcp_extra_endpoints", extra_endpoints), ("mcp_disabled_tools", disabled_tools)] {
        match list {
            Some([]) => {
                root.remove(key);
            }
            Some(list) => {
                root.insert(key.into(), serde_json::json!(list));
            }
            None => {}
        }
    }
    write_root(settings_file, &root)
}

/// Strict MCP read: a damaged settings file must not silently become an empty server list.
pub fn read_mcp_servers(file: &Path) -> Result<Vec<moss_mcp_client::ServerConfig>, String> {
    let root = strict_root(file)?;
    let value = root.get("voice").and_then(|v| v.get("mcp_servers")).cloned().unwrap_or_else(|| serde_json::json!([]));
    let servers = serde_json::from_value::<Vec<moss_mcp_client::ServerConfig>>(value).map_err(|_| "第三方 MCP 配置格式不正确")?;
    moss_mcp_client::validate_servers(&servers).map_err(|error| error.to_string())?;
    Ok(servers)
}

fn strict_root(file: &Path) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).ok().and_then(|v| v.as_object().cloned()).ok_or_else(|| "设置文件损坏，未覆盖原文件".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(_) => Err("无法读取设置文件".into()),
    }
}

/// NV15: isolated, versioned offline Home Assistant settings. Nothing here
/// authenticates to HA, exposes a token, or grants any device control.
const HOME_REVISION_MAX: u64 = 9_007_199_254_740_991; // JS Number.MAX_SAFE_INTEGER

#[derive(Debug, PartialEq, Eq)]
pub enum HomeOfflineError {
    Invalid(&'static str),
    Conflict,
    Storage(String),
}

fn home_snapshot(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<(BuiltinHomeAssistant, u64), HomeOfflineError> {
    let Some(voice) = root.get("voice") else {
        return Ok((BuiltinHomeAssistant::default(), 0));
    };
    let voice = voice.as_object().ok_or(HomeOfflineError::Invalid("语音配置格式无效"))?;
    let home = voice
        .get("builtin_home")
        .map(|value| serde_json::from_value::<BuiltinHomeAssistant>(value.clone())
            .map_err(|_| HomeOfflineError::Invalid("Home Assistant 配置已损坏")))
        .transpose()?
        .unwrap_or_default();
    home.validate_offline().map_err(HomeOfflineError::Invalid)?;
    let revision = match voice.get("builtin_home_revision") {
        None => 0,
        Some(value) => value.as_u64().filter(|v| *v <= HOME_REVISION_MAX)
            .ok_or(HomeOfflineError::Invalid("Home Assistant 配置版本无效"))?,
    };
    Ok((home, revision))
}

/// Strict status read: unlike generic voice settings, corruption is never
/// silently translated to an empty, seemingly safe Home Assistant allowlist.
pub fn read_home_offline(
    file: &Path,
) -> Result<(BuiltinHomeAssistant, u64), HomeOfflineError> {
    let root = strict_root(file).map_err(HomeOfflineError::Storage)?;
    home_snapshot(&root)
}

/// Atomic optimistic-concurrency save under the shared settings write lock.
/// Stale desktop windows can neither resurrect removed entities nor overwrite
/// a later policy revision. Other voice sections and top-level keys survive.
pub fn save_home_offline(
    file: &Path,
    next: &BuiltinHomeAssistant,
    expected_revision: u64,
) -> Result<u64, HomeOfflineError> {
    next.validate_offline().map_err(HomeOfflineError::Invalid)?;
    let _guard = SETTINGS_WRITE.lock()
        .map_err(|_| HomeOfflineError::Storage("设置锁不可用".into()))?;
    let mut root = strict_root(file).map_err(HomeOfflineError::Storage)?;
    let (current, revision) = home_snapshot(&root)?;
    if revision != expected_revision {
        return Err(HomeOfflineError::Conflict);
    }
    if &current == next {
        return Ok(revision);
    }
    let updated_revision = revision.checked_add(1)
        .filter(|v| *v <= HOME_REVISION_MAX)
        .ok_or(HomeOfflineError::Invalid("Home Assistant 配置版本已达上限"))?;
    let voice = root.entry("voice").or_insert_with(|| serde_json::json!({}));
    let voice = voice.as_object_mut().ok_or(HomeOfflineError::Invalid("语音配置格式无效"))?;
    voice.insert("builtin_home".into(), serde_json::to_value(next)
        .map_err(|_| HomeOfflineError::Invalid("Home Assistant 配置序列化失败"))?);
    voice.insert("builtin_home_revision".into(), serde_json::json!(updated_revision));
    write_root(file, &root).map_err(HomeOfflineError::Storage)?;
    Ok(updated_revision)
}


/// NV15 portable Home profile format; never contains app_secrets, DPAPI
/// envelopes, approvals, client tokens, voice grants, or task history.
/// Importing is an inert *preview* until the credential is revoked and the
/// caller commits the version-checked new settings.
const HOME_PORTABLE_FORMAT: &str = "moss-home-profile-v1";
const MAX_HOME_BACKUP_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableHomeProfile {
    format: String,
    credentials: String,
    home: BuiltinHomeAssistant,
}

/// Serialize only a dedicated, validated, explicitly allowlisted schema.
/// No generic JSON settings fields or SQLite tables are copied.
pub fn export_home_portable(file: &Path) -> Result<String, HomeOfflineError> {
    let (home, _) = read_home_offline(file)?;
    // Migration transfers an inactive list, never a URL, media-library ID,
    // credential or an authorization snapshot from another machine.
    let safe = PortableHomeProfile {
        format: HOME_PORTABLE_FORMAT.into(),
        credentials: "reenter_on_restore".into(),
        home: home_transfer::sanitize(home),
    };
    let text = serde_json::to_string_pretty(&safe)
        .map_err(|_| HomeOfflineError::Invalid("安全备份生成失败"))?;
    if text.len() > MAX_HOME_BACKUP_BYTES {
        return Err(HomeOfflineError::Invalid("家居备份超过64KiB"));
    }
    Ok(text)
}

/// Parse an untrusted, potentially transferred JSON backup. Reject unknown
/// fields including token, ciphertext, grants and approvals. A restore never
/// auto-activates an entity or preserves automatic permission preferences.
pub fn validate_home_portable(data: &[u8])
    -> Result<BuiltinHomeAssistant, HomeOfflineError> {
    if data.is_empty() || data.len() > MAX_HOME_BACKUP_BYTES {
        return Err(HomeOfflineError::Invalid("备份为空或超过64KiB"));
    }
    let safe: PortableHomeProfile = serde_json::from_slice(data)
        .map_err(|_| HomeOfflineError::Invalid("家居备份格式或字段无效"))?;
    if safe.format != HOME_PORTABLE_FORMAT || safe.credentials != "reenter_on_restore" {
        return Err(HomeOfflineError::Invalid("备份来源格式或凭据策略不受支持"));
    }
    safe.home.validate_offline().map_err(HomeOfflineError::Invalid)?;
    // Older portable-v1 files might still carry a host URL or media IDs.
    // Treat imported data as untrusted, always sanitize again at restore.
    let config = home_transfer::sanitize(safe.home);
    config.validate_offline().map_err(HomeOfflineError::Invalid)?;
    Ok(config)
}

/// Dedicated desktop write; preserves all other voice and root settings.
pub fn save_mcp_servers(file: &Path, servers: &[moss_mcp_client::ServerConfig]) -> Result<(), String> {
    moss_mcp_client::validate_servers(servers).map_err(|error| error.to_string())?;
    let _guard = SETTINGS_WRITE.lock().map_err(|_| "设置锁不可用")?;
    let mut root = strict_root(file)?;
    let voice = root.entry("voice").or_insert_with(|| serde_json::json!({})).as_object_mut().ok_or("语音设置格式不正确")?;
    voice.insert("mcp_servers".into(), serde_json::to_value(servers).map_err(|_| "第三方 MCP 配置格式不正确")?);
    write_root(file,&root)
}

#[cfg(test)]
mod tests {
    use super::*;



    #[test]
    fn nv15_portable_profile_excludes_secrets_and_resets_all_authorizations() {
        let temp = tempfile::tempdir().unwrap();
        let origin = temp.path().join("origin.json");
        let destination = temp.path().join("restored.json");
        let fake_token = "NV15_ONLY_SYNTHETIC_SENSITIVE_MIGRATION_TOKEN_ABC";
        let initial = serde_json::json!({
            "voice": {
                "builtin_home_revision": 2,
                "builtin_home": {
                    "enabled":false,
                    "base_url":"https://ha.example.test:8123/",
                    "entities":[
                      {"entity_id":"light.desk","alias":"台灯","enabled":true,"policy":"allow","media":[]},
                      {"entity_id":"lock.front","alias":"前门","enabled":true,"policy":"ask","media":[]}
                    ]
                },
                "builtin_llm":{"key":fake_token},
                "mcp_servers":[{"authorization":fake_token}],
                "home_voice_grant":fake_token
            },
            "other_private_secret":fake_token
        });
        std::fs::write(&origin,initial.to_string()).unwrap();
        let backup=export_home_portable(&origin).unwrap();
        assert!(!backup.contains(fake_token));
        assert!(!backup.contains("builtin_llm") && !backup.contains("mcp_servers"));
        assert!(!backup.contains("other_private_secret") && !backup.contains("home_voice_grant"));
        assert!(!backup.contains("https://ha.example.test"));
        assert!(!backup.contains("ha.example.test"));
        assert!(!backup.contains("media_player_private_library_id"));
        assert!(backup.contains("reenter_on_restore"));
        let restored=validate_home_portable(backup.as_bytes()).unwrap();
        assert!(restored.base_url.is_empty());
        assert!(restored.entities.iter().all(|entry|entry.media.is_empty()));
        assert!(!restored.enabled);
        assert!(restored.entities.iter().all(|e|
            !e.enabled && e.policy == HomeApprovalPreference::Ask));
        assert_eq!(restored.entities.len(),2);
        assert_eq!(save_home_offline(&destination,&restored,0).unwrap(),1);
        let (roundtrip,rev)=read_home_offline(&destination).unwrap();
        assert_eq!(rev,1);
        assert_eq!(roundtrip,restored);
        assert!(!std::fs::read_to_string(&destination).unwrap().contains(fake_token));
    }
    #[test]
    fn nv15_portable_profile_rejects_credentials_nested_or_old_schema() {
        let temp=tempfile::tempdir().unwrap();
        let cfg=temp.path().join("settings.json");
        let backup=export_home_portable(&cfg).unwrap();
        let mut value: serde_json::Value=serde_json::from_str(&backup).unwrap();
        for field in ["token","dpapi","app_secrets","approvals","voice_control_consent"] {
            value[field]=serde_json::json!("SYNTHETIC_CREDENTIAL");
            assert!(validate_home_portable(value.to_string().as_bytes()).is_err(),"{field}");
            value.as_object_mut().unwrap().remove(field);
        }
        value["home"]["token"]=serde_json::json!("LEAK");
        assert!(validate_home_portable(value.to_string().as_bytes()).is_err());
        value["home"].as_object_mut().unwrap().remove("token");
        value["credentials"]=serde_json::json!("include");
        assert!(validate_home_portable(value.to_string().as_bytes()).is_err());
        value["credentials"]=serde_json::json!("reenter_on_restore");
        value["format"]=serde_json::json!("moss-home-profile-v0");
        assert!(validate_home_portable(value.to_string().as_bytes()).is_err());
        assert!(validate_home_portable(&vec![b'a';65537]).is_err());
        assert!(validate_home_portable(b"{}").is_err());
    }
    #[test]
    fn nv15_home_configuration_is_inert_default_denied_and_never_accepts_secret_fields() {
        assert!(!BuiltinHomeAssistant::default().enabled);
        assert!(BuiltinHomeAssistant::default().validate_offline().is_ok());
        let valid = BuiltinHomeAssistant {
            enabled:false, base_url:"https://ha.example.test:8123/".into(),
            entities:vec![BuiltinHomeEntity{
                entity_id:"light.desk".into(),alias:"书桌灯".into(),
                enabled:true,policy:HomeApprovalPreference::Ask,media:vec![],
            }],
        };
        assert!(valid.validate_offline().is_ok());
        for enabled in [true] {
            let next = BuiltinHomeAssistant { enabled, ..valid.clone() };
            assert!(next.validate_offline().is_err());
        }
        for url in [
            "http://127.0.0.1:8123/", "https://user:secret@ha.example.test/",
            "https://ha.example.test/api/services/light/turn_on",
            "https://ha.example.test/?token=secret",
            "file:///C:/private","https://ha.example.test/#authorization",
        ] {
            let next = BuiltinHomeAssistant { base_url:url.into(), ..valid.clone() };
            assert!(next.validate_offline().is_err(), "{url}");
        }
        let credential = serde_json::json!({
            "enabled":false,"base_url":"","entities":[],"token":"NV15_SHOULD_NOT_SAVE"
        });
        assert!(serde_json::from_value::<BuiltinHomeAssistant>(credential).is_err());
        let text = serde_json::to_string(&valid).unwrap();
        assert!(!text.contains("token") && !text.contains("password") && !text.contains("api_key"));
    }

    #[test]
    fn nv15_home_entity_allowlist_never_grants_dangerous_device_automatic_control() {
        let entry = BuiltinHomeEntity {
            entity_id:"lock.front".into(),alias:"门锁".into(),
            enabled:true,policy:HomeApprovalPreference::Allow,media:vec![],
        };
        let base = BuiltinHomeAssistant{
            enabled:false,base_url:String::new(),entities:vec![entry.clone()],
        };
        assert!(base.validate_offline().is_err());
        for id in ["alarm_control_panel.home","cover.blinds"] {
            let next = BuiltinHomeAssistant{
                entities:vec![BuiltinHomeEntity{entity_id:id.into(),..entry.clone()}],
                ..base.clone()
            };
            assert!(next.validate_offline().is_err());
        }
        for id in ["../../etc/passwd","light.desk/../../secrets",
            "script.shell_command","automation.turn_on","light.desk?key=secret"] {
            let next = BuiltinHomeAssistant{
                entities:vec![BuiltinHomeEntity{
                    entity_id:id.into(),policy:HomeApprovalPreference::Ask,..entry.clone()
                }],
                ..base.clone()
            };
            assert!(next.validate_offline().is_err(), "{id}");
        }
        let duplicate = BuiltinHomeAssistant {
            entities:vec![
                BuiltinHomeEntity { entity_id:"light.desk".into(),alias:"台灯".into(),
                    enabled:true,policy:HomeApprovalPreference::Ask,media:vec![] },
                BuiltinHomeEntity { entity_id:"switch.fan".into(),alias:" 台灯 ".into(),
                    enabled:true,policy:HomeApprovalPreference::Ask,media:vec![] },
            ],
            ..base
        };
        assert!(duplicate.validate_offline().is_err());
    }

    #[test]
    fn nv15_saved_aliases_cannot_shadow_another_entity_id() {
        let first = BuiltinHomeEntity {
            entity_id: "light.desk".into(), alias: "台灯".into(),
            enabled: true, policy: HomeApprovalPreference::Ask,media:vec![],
        };
        let second = BuiltinHomeEntity {
            entity_id: "switch.fan".into(), alias: "风扇".into(),
            enabled: false, policy: HomeApprovalPreference::Ask,media:vec![],
        };
        let mut settings = BuiltinHomeAssistant {
            enabled: false, base_url: String::new(), entities: vec![first, second],
        };
        assert!(settings.validate_offline().is_ok());
        settings.entities[0].alias = " SWITCH.FAN ".into();
        assert!(settings.validate_offline().is_err());
        for alias in ["light.not_selected", "switch.future_device"] {
            settings.entities[0].alias = alias.into();
            assert!(settings.validate_offline().is_err(), "{alias}");
        }
        settings.entities[0].alias = "light.desk".into();
        assert!(settings.validate_offline().is_ok(), "own entity ID remains unambiguous");
        settings.entities[1].enabled = true;
        assert!(settings.validate_offline().is_ok());
        settings.entities[1].entity_id = "light.DESK".into();
        assert!(settings.validate_offline().is_err(), "case-variant IDs are duplicates");
    }
    #[test]
    fn nv15_generic_settings_cannot_introduce_ha_policy_on_fresh_install() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let mut generic = VoiceSettings::default();
        generic.builtin_home.enabled = true;
        generic.builtin_home.base_url = "https://unexpected.example.test/".into();
        save(&file, &generic).unwrap();
        let (saved, revision) = read_home_offline(&file).unwrap();
        assert_eq!(revision, 0);
        assert_eq!(saved, BuiltinHomeAssistant::default());
        assert!(!load(&file).builtin_home.enabled);
    }
    #[test]
    fn nv15_home_versioned_save_rejects_stale_overwrite_and_preserves_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let (empty, revision) = read_home_offline(&file).unwrap();
        assert_eq!((empty, revision), (BuiltinHomeAssistant::default(), 0));

        let mut old_settings = VoiceSettings::default();
        old_settings.builtin_music.directory = "existing-music-setting".into();
        save(&file, &old_settings).unwrap();
        let mut home = BuiltinHomeAssistant {
            enabled: false,
            base_url: "https://ha.example.test:8123/".into(),
            entities: vec![BuiltinHomeEntity {
                entity_id: "light.desk".into(), alias: "台灯".into(),
                enabled: true, policy: HomeApprovalPreference::Ask,media:vec![],
            }],
        };
        assert_eq!(save_home_offline(&file, &home, 0), Ok(1));
        let unchanged = std::fs::read(&file).unwrap();
        assert_eq!(save_home_offline(&file, &home, 0), Err(HomeOfflineError::Conflict));
        assert_eq!(std::fs::read(&file).unwrap(), unchanged);
        assert_eq!(save_home_offline(&file, &home, 1), Ok(1));
        assert_eq!(std::fs::read(&file).unwrap(), unchanged);

        home.entities.clear(); // A later desktop revokes the visible target.
        assert_eq!(save_home_offline(&file, &home, 1), Ok(2));
        assert_eq!(save_home_offline(&file, &BuiltinHomeAssistant::default(), 1),
                   Err(HomeOfflineError::Conflict));

        // Saving a stale whole voice object must not restore the entity.
        save(&file, &old_settings).unwrap();
        assert_eq!(read_home_offline(&file).unwrap(), (home, 2));
        assert_eq!(load(&file).builtin_music.directory, "existing-music-setting");
        let root = strict_root(&file).unwrap();
        assert!(root["voice"].get("builtin_home_revision").is_some());
    }

    #[test]
    fn nv15_home_versioned_save_is_atomic_across_concurrent_desktop_writers() {
        use std::sync::{Arc, Barrier};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let gate = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2).map(|n| {
            let file = file.clone();
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                let next = BuiltinHomeAssistant {
                    enabled: false, base_url: format!("https://ha{n}.example.test/"),
                    entities: vec![],
                };
                gate.wait();
                save_home_offline(&file, &next, 0)
            })
        }).collect();
        gate.wait();
        let results: Vec<_> = handles.into_iter().map(|join| join.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|result| **result == Ok(1)).count(), 1);
        assert_eq!(results.iter().filter(|result| **result == Err(HomeOfflineError::Conflict)).count(), 1);
        let (saved, revision) = read_home_offline(&file).unwrap();
        assert_eq!(revision, 1);
        assert!(saved.base_url.starts_with("https://ha"));
    }

    #[test]
    fn nv15_home_corrupt_revision_and_unsafe_settings_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"other":"keep","voice":{"builtin_home_revision":"bad"}}"#).unwrap();
        let before = std::fs::read(&file).unwrap();
        assert!(matches!(read_home_offline(&file), Err(HomeOfflineError::Invalid(_))));
        assert!(matches!(save_home_offline(&file, &BuiltinHomeAssistant::default(), 0),
            Err(HomeOfflineError::Invalid(_))));
        assert_eq!(std::fs::read(&file).unwrap(), before);

        std::fs::write(&file, r#"{"other":"keep","voice":{"builtin_home":{"enabled":true,"base_url":"","entities":[]}}}"#).unwrap();
        assert!(matches!(read_home_offline(&file), Err(HomeOfflineError::Invalid(_))));
        assert!(matches!(save_home_offline(&file, &BuiltinHomeAssistant::default(), 0),
            Err(HomeOfflineError::Invalid(_))));

        std::fs::write(&file, b"not-json").unwrap();
        assert!(matches!(read_home_offline(&file), Err(HomeOfflineError::Storage(_))));
        assert!(matches!(save_home_offline(&file, &BuiltinHomeAssistant::default(), 0),
            Err(HomeOfflineError::Storage(_))));
        assert_eq!(std::fs::read(&file).unwrap(), b"not-json");
    }

    #[test]
    fn old_settings_without_voice_load_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        assert_eq!(load(&file), VoiceSettings::default());
        std::fs::write(&file, br#"{"web_port": 9666, "mcp_endpoint": "wss://example.test/mcp/?token=t"}"#).unwrap();
        let voice = load(&file);
        assert_eq!(voice.mode, VoiceMode::Official);
        assert_eq!(voice.args, vec!["app.py".to_string()]);
        assert_eq!(voice.server_port, 8000);
        std::fs::write(&file, b"{broken").unwrap();
        assert_eq!(load(&file), VoiceSettings::default());
        // voice 字段部分缺失也能读
        std::fs::write(&file, br#"{"voice": {"mode": "builtin"}}"#).unwrap();
        assert_eq!(load(&file).mode, VoiceMode::Builtin);
        assert!(load(&file).builtin_models.is_empty());
        assert_eq!(load(&file).builtin_local, BuiltinLocal::default());
        assert!(!load(&file).builtin_memory_enabled, "旧配置不能自动开启会话记忆");
        assert!(!load(&file).builtin_person_memory_enabled, "旧配置不能自动开启按人记忆");
        assert_eq!(load(&file).builtin_context, BuiltinContext::default(), "旧配置不能自动开启长期记忆检索或提取");
        assert!(!load(&file).builtin_home.enabled);
        assert!(load(&file).builtin_home.entities.is_empty());
        assert!(!load(&file).builtin_music.enabled);
        assert!(load(&file).builtin_music.directory.is_empty());
        let plugins=load(&file).builtin_plugins;
        assert!(!plugins.weather_enabled && !plugins.lunar_enabled && !plugins.news_enabled
            && !plugins.search_enabled && !plugins.rag_enabled);
        assert!(plugins.news_sources.is_empty() && plugins.rag_dataset_ids.is_empty());
        std::fs::write(&file, br#"{"voice": {"mode": "self_hosted", "server_dir": "D:/srv"}}"#).unwrap();
        let voice = load(&file);
        assert_eq!(voice.mode, VoiceMode::SelfHosted);
        assert_eq!(voice.args, vec!["app.py".to_string()]);
        assert_eq!(voice.config_path().unwrap(), Path::new("D:/srv").join("data").join(".config.yaml"));
    }

    #[test]
    fn save_keeps_other_keys_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, br#"{"web_port": 9777, "export": {"dir": "X"}, "mcp_endpoint": "wss://official.test/?token=a"}"#).unwrap();
        let voice = VoiceSettings {
            mcp_servers: Vec::new(),
            mode: VoiceMode::SelfHosted,
            server_dir: Some("srv".into()),
            python: Some("py".into()),
            args: vec!["app.py".into(), "--debug".into()],
            server_port: 8100,
            self_hosted_mcp_endpoint: Some("ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=b".into()),
            official_mcp_endpoint: Some("wss://official.test/?token=a".into()),
            endpoint_health_url: None,
            builtin_ws_port: 8100,
            builtin_http_port: 8103,
            builtin_models: BuiltinModels {
                llm: Some(BuiltinProvider {
                    kind: ProviderKind::Openai,
                    url: "https://llm.test/v1".into(),
                    model: "m".into(),
                    voice: String::new(),
                    speed: 1.0,
                }),
                asr: None,
                tts: Some(BuiltinProvider {
                    kind: ProviderKind::Doubao,
                    url: "wss://tts.test/api/v3/tts/bidirection".into(),
                    model: "seed-tts-2.0".into(),
                    voice: "v".into(),
                    speed: 1.2,
                }),
            },
            builtin_plugins: BuiltinPlugins::default(),
            builtin_agents:BuiltinAgents::default(),
            builtin_home: BuiltinHomeAssistant::default(),
            builtin_music: BuiltinMusic::default(),
            builtin_vision: BuiltinVision::default(),
            builtin_memory_enabled: false,
            builtin_person_memory_enabled:false,
            builtin_context: BuiltinContext::default(),
            builtin_local: BuiltinLocal { asr: true, mirror: Some("https://mirror.test/x".into()), ..BuiltinLocal::default() },
        };
        save(&file, &voice).unwrap();
        assert_eq!(load(&file), voice);
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(root["web_port"], 9777);
        assert_eq!(root["export"]["dir"], "X");
        assert_eq!(mcp_endpoint(&file).as_deref(), Some("wss://official.test/?token=a"));
        let text = std::fs::read_to_string(&file).unwrap();
        // NV11 weather_auth may legitimately contain the literal "api_key" as an auth-mode label.
        // Assert that no actual credential field or secret value is serialized.
        assert!(!text.contains("\"api_key\":"), "设置不能包含原始 API 密钥字段");
        assert!(!text.contains("sk-test-secret-value"), "设置不能包含密钥值");
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(saved["voice"]["builtin_models"].get("asr").is_none(), "未配置的分区不写入");
        assert!(saved["voice"]["builtin_models"]["llm"].get("kind").is_none(), "OpenAI 兼容不写 kind，旧版程序可读");
        assert_eq!(saved["voice"]["builtin_models"]["tts"]["kind"], "doubao");
        assert_eq!(saved["voice"]["builtin_local"]["asr"], true);
        assert_eq!(saved["voice"]["builtin_local"]["asr_model"], "sense_voice");
        // NV6 保存的旧设置没有 *_model：按默认模型读取
        let old: BuiltinLocal = serde_json::from_value(serde_json::json!({"asr": true, "tts_speaker": 3})).unwrap();
        assert_eq!((old.vad_model.as_str(), old.asr_model.as_str(), old.tts_model.as_str()), ("silero_vad", "sense_voice", "kokoro"));
        assert_eq!(old.selected(ModelKind::Asr).map(|spec| spec.id), Some("sense_voice"));
        assert!(old.selected(ModelKind::Tts).is_none(), "开关关闭");

        save_with_endpoint(&file, &voice, voice.self_hosted_mcp_endpoint.as_deref()).unwrap();
        assert_eq!(mcp_endpoint(&file).as_deref(), Some("ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=b"));
        save_with_endpoint(&file, &VoiceSettings::default(), None).unwrap();
        assert_eq!(mcp_endpoint(&file), None);
        assert_eq!(load(&file), VoiceSettings::default());
        assert!(!dir.path().join("settings.json.tmp").exists());
    }

    #[test]
    fn barge_settings_default_and_bounds() {
        let old: BuiltinLocal = serde_json::from_str("{}").unwrap();
        assert!(!old.barge_in);
        assert_eq!(old.barge_in_ms, 400);
        for value in [300, 400, 600] { assert_eq!(validate_barge_in_ms(value).unwrap(), value as u32); }
        for value in [0, 299, 601, u64::MAX] { assert!(validate_barge_in_ms(value).is_err()); }
        let enabled = BuiltinLocal { barge_in: true, barge_in_ms: 600, ..Default::default() };
        let value = serde_json::to_string(&enabled).unwrap();
        assert_eq!(serde_json::from_str::<BuiltinLocal>(&value).unwrap(), enabled);
    }

    #[test]
    fn search_provider_migration_keeps_tavily_and_separate_metaso_host() {
        let old:BuiltinPlugins=serde_json::from_value(serde_json::json!({"search_enabled":true,"search_host":"https://search.example.org"})).unwrap();
        assert_eq!(old.search_provider,SearchProvider::Tavily);
        assert_eq!(old.metaso_host,"https://metaso.cn");
        assert_eq!(old.search_host,"https://search.example.org");
        let metaso=BuiltinPlugins{search_provider:SearchProvider::Metaso,..old};
        assert_eq!(serde_json::from_value::<BuiltinPlugins>(serde_json::to_value(&metaso).unwrap()).unwrap(),metaso);
        assert!(serde_json::from_value::<BuiltinPlugins>(serde_json::json!({"search_provider":"unknown"})).is_err());
        assert!(!BuiltinPlugins::default().search_enabled);
    }

    #[test]
    fn chat_readiness_counts_installed_local_models() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        let cloud =
            |url: &str| BuiltinProvider { kind: ProviderKind::Openai, url: url.into(), model: "m".into(), voice: "v".into(), speed: 1.0 };
        let mut voice = VoiceSettings::default();
        voice.builtin_models.llm = Some(cloud("https://llm.test/v1"));
        assert!(!voice.builtin_chat_ready(&models));
        voice.builtin_local.asr = true;
        voice.builtin_local.tts = true;
        assert!(!voice.builtin_chat_ready(&models), "开关打开但模型未安装不算可用");
        let install = |id: &str| {
            let spec = crate::local_models::spec(id).unwrap();
            let root = crate::local_models::install_path(&models, spec);
            for item in spec.required {
                let path = root.join(item);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, b"x").unwrap();
            }
        };
        // 装好默认的 SenseVoice 与 Kokoro 所需文件
        install("sense_voice");
        install("kokoro");
        assert!(voice.builtin_chat_ready(&models));
        // 改选流式识别：未安装就不算可用；装好后可用
        voice.builtin_local.asr_model = "x_asr_streaming".into();
        assert!(!voice.builtin_section_ready(Section::Asr, &models));
        install("x_asr_streaming");
        assert!(voice.builtin_section_ready(Section::Asr, &models));
        assert_eq!(voice.builtin_local.selected(ModelKind::Asr).map(|spec| spec.id), Some("x_asr_streaming"));
        // 无效或类别不符的模型 id 不算选中
        voice.builtin_local.tts_model = "ten_vad".into();
        assert!(voice.builtin_local.selected(ModelKind::Tts).is_none());
        assert!(!voice.builtin_section_ready(Section::Tts, &models));
        voice.builtin_local.tts_model = "kokoro".into();
        voice.builtin_models.llm = None;
        assert!(!voice.builtin_chat_ready(&models), "大模型只有云端");
        voice.builtin_models.llm = Some(cloud("https://llm.test/v1"));
        voice.builtin_local.tts = false;
        assert!(!voice.builtin_section_ready(Section::Tts, &models));
        voice.builtin_models.tts = Some(cloud("https://tts.test/v1/audio/speech"));
        assert!(voice.builtin_chat_ready(&models), "合成走云端");
    }
}
