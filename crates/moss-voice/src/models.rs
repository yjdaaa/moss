//! 模型配置向导的输入与“测试连接”（技术书 §9.4.4，实施清单 P4-06）。
//!
//! - 三个独立分区：大模型（LLM）、语音识别（ASR）、语音合成（TTS），各自填写地址、密钥、模型；
//! - 测试连接只用内存中的值发一次最小请求：15 秒超时、**不跟随重定向**，请求结束即丢弃密钥；
//! - 结果、错误信息与日志都不包含密钥（错误不使用 reqwest 的原始描述，它会带上完整地址）。
//!
//! 内置语音服务（NV7）的识别与合成还可以选“豆包流式”（[`ProviderKind::Doubao`]，WebSocket 接口）：
//! 校验见 [`resolve_builtin`]，测试连接由桌面端调用 `moss-voice-providers` 完成；外部服务端链路只支持 OpenAI 兼容。

use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 测试请求的超时（技术书 §9.4.4）。
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15);
/// 模型列表查询的超时（对齐 magpie 的 8 秒上限，避免无效地址长时间卡顿）。
pub const LIST_TIMEOUT: Duration = Duration::from_secs(8);
/// TCP + TLS 连接握手超时（不可达地址快速返回，不占满整个请求超时）。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const MAX_MODELS_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MODELS_COUNT: usize = 2000;

/// OpenRouter Decisions 固定端点与常用 Jev 模型预设。
pub const DEFAULT_JEV_DECISIONS_URL: &str = "https://openrouter.ai/api/alpha/decisions";
pub const DEFAULT_LOCAL_LAYA_DECISIONS_URL: &str = "http://127.0.0.1:18090/v1/systemone";
pub const DEFAULT_OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";
pub const BUILTIN_JEV_MODELS: [&str; 2] = ["typesafe/jev-1.14", "typesafe/jev-1.13"];
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Llm,
    Asr,
    Tts,
}

impl Section {
    pub const ALL: [Section; 3] = [Section::Llm, Section::Asr, Section::Tts];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Asr => "asr",
            Self::Tts => "tts",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "llm" => Some(Self::Llm),
            "asr" => Some(Self::Asr),
            "tts" => Some(Self::Tts),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Llm => "大模型",
            Self::Asr => "语音识别",
            Self::Tts => "语音合成",
        }
    }
}

/// 模型接口类型（NV7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// OpenAI 兼容 HTTP 接口
    #[default]
    Openai,
    /// 豆包（火山引擎）流式识别 / 双向流式合成，WebSocket 接口；“模型”填资源 ID
    Doubao,
}

impl ProviderKind {
    pub fn is_openai(&self) -> bool {
        *self == Self::Openai
    }
}

/// 豆包流式识别默认地址与资源 ID（与 `moss-voice-providers::doubao` 的默认值一致，桌面端测试核对）。
pub const DOUBAO_ASR_URL: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async";
pub const DOUBAO_ASR_RESOURCE: &str = "volc.bigasr.sauc.duration";
pub const DOUBAO_ASR_RESOURCES: [&str; 4] = [
    DOUBAO_ASR_RESOURCE,
    "volc.bigasr.sauc.concurrent",
    "volc.seedasr.sauc.duration",
    "volc.seedasr.sauc.concurrent",
];
/// 豆包双向流式合成默认地址与资源 ID。
pub const DOUBAO_TTS_URL: &str = "wss://openspeech.bytedance.com/api/v3/tts/bidirection";
pub const DOUBAO_TTS_RESOURCE: &str = "seed-tts-2.0";
pub const DOUBAO_TTS_RESOURCES: [&str; 3] = [
    DOUBAO_TTS_RESOURCE,
    "seed-tts-1.0",
    "volc.service_type.10029",
];
/// Cline API（`api.cline.bot`）的 ClinePass 包月订阅模型 ID：`/models` 不动态返回 `cline-pass/` 前缀，
/// 读取大模型列表时置顶补齐，避免订阅用户误选按量计费模型触发 HTTP 402。
pub const CLINE_PASS_MODELS: [&str; 17] = [
    "cline-pass/deepseek-v4.1-flash",
    "cline-pass/deepseek-v4.1-flash-1017",
    "cline-pass/deepseek-v4-pro",
    "cline-pass/kimi-k3",
    "cline-pass/kimi-k3.1-pro",
    "cline-pass/glm-5.3",
    "cline-pass/glm-5.3-flash",
    "cline-pass/minimax-m3",
    "cline-pass/minimax-m3.1-flash",
    "cline-pass/mimo-v2.6-pro",
    "cline-pass/mimo-v2.6-flash",
    "cline-pass/mimo-v2.5-pro",
    "cline-pass/mimo-v2.5",
    "cline-pass/qwen3.8-max",
    "cline-pass/qwen3.7-max",
    "cline-pass/qwen3.7-plus",
    "cline-pass/muse-spark-1.3-contributor",
];

/// Cline 免费模型 ID（对齐 `yetone/magpie` `internal/provider/cline.go` 的 `clineFree`）：
/// 在 Cline 官方客户端通过 `recommended-models` 下发，独立于 ClinePass 配额。
pub const CLINE_FREE_MODELS: [&str; 4] = [
    "cline-free/deepseek-v4.1-flash",
    "cline-free/mimo-v2.6-flash",
    "cline-free/muse-spark-1.3-contributor",
    "stealth/space-bunny-alpha",
];

/// 判断目标地址是否为 Cline 官方网关（`api.cline.bot` 或 `*.cline.bot`）。
pub fn is_cline_api_url(url: &str) -> bool {
    reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| {
            parsed.host_str().map(|host| {
                let lower = host.to_ascii_lowercase();
                lower == "cline.bot" || lower.ends_with(".cline.bot")
            })
        })
        .unwrap_or(false)
}

/// 严格判断目标地址是否属于官方 `https://openrouter.ai:443` 源（用于决定是否允许复用大模型密钥）。
/// 拒绝 `http://`、自定义端口、userinfo、伪后缀域名（如 `openrouter.ai.example`）以及路径/查询参数伪装。
pub fn is_openrouter_https_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url.trim()) else {
        return false;
    };
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port_or_known_default() != Some(443)
    {
        return false;
    }
    parsed
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("openrouter.ai"))
}

/// 识别仅提供大模型对话接口、不提供 OpenAI `/audio/transcriptions` 与 `/audio/speech` 语音端点的网关
/// （对齐 `yetone/magpie` `internal/provider/presets.go` 的 Chat-only 服务商与 Coding/Token Plan 专属端点）。
pub fn chat_only_gateway_name(url: &str) -> Option<&'static str> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let path = parsed.path().to_ascii_lowercase();
    if host == "cline.bot" || host.ends_with(".cline.bot") {
        return Some("Cline (api.cline.bot)");
    }
    if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        return Some("OpenRouter");
    }
    if host == "api.deepseek.com" || host.ends_with(".deepseek.com") {
        return Some("DeepSeek 官方 API");
    }
    if host == "api.anthropic.com" || host.ends_with(".anthropic.com") {
        return Some("Anthropic");
    }
    if host == "opencode.ai" || host.ends_with(".opencode.ai") {
        return Some("OpenCode Zen/Go");
    }
    if host == "api.kilo.ai" || host.ends_with(".kilo.ai") {
        return Some("Kilo Gateway");
    }
    if host == "api.moonshot.cn"
        || host == "api.moonshot.ai"
        || host == "api.kimi.com"
        || host == "api.kimi.ai"
    {
        return Some("Kimi / Moonshot");
    }
    if host.starts_with("token-plan")
        || path.contains("/api/coding")
        || path.contains("/tokenplan/")
        || path.contains("/step_plan/")
    {
        return Some("该 Coding / Token Plan 专属端点");
    }
    None
}

fn reject_chat_only_audio_gateway(section: Section, url: &str) -> Result<(), String> {
    if section == Section::Llm {
        return Ok(());
    }
    if let Some(gateway) = chat_only_gateway_name(url) {
        let endpoint = if section == Section::Asr {
            "/audio/transcriptions"
        } else {
            "/audio/speech"
        };
        return Err(format!(
            "{}：{gateway} 仅提供大模型对话接口，不支持{}（无 {endpoint} 端点）；请在顶部下拉框切换为「本地{}」，或填写支持语音的服务商（如硅基流动 SiliconFlow、OpenAI、豆包流式）",
            section.label(),
            section.label(),
            section.label()
        ));
    }
    Ok(())
}

/// 一个分区的输入。`voice` / `speed` 只用于语音合成。
#[derive(Clone, Default, Deserialize)]
pub struct SectionInput {
    /// 接口类型，缺省为 OpenAI 兼容（只有内置服务的识别与合成可选豆包）
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub voice: String,
    #[serde(default)]
    pub speed: Option<f64>,
}

// 手写 Debug：任何调试输出都不带密钥
impl std::fmt::Debug for SectionInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SectionInput")
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field(
                "api_key",
                &if self.api_key.trim().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("speed", &self.speed)
            .finish()
    }
}

/// 向导整体输入；某个分区缺省或全部留空时不修改该模块。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelsInput {
    #[serde(default)]
    pub llm: Option<SectionInput>,
    #[serde(default)]
    pub asr: Option<SectionInput>,
    #[serde(default)]
    pub tts: Option<SectionInput>,
}

impl ModelsInput {
    pub fn section(&self, section: Section) -> Option<&SectionInput> {
        match section {
            Section::Llm => self.llm.as_ref(),
            Section::Asr => self.asr.as_ref(),
            Section::Tts => self.tts.as_ref(),
        }
    }
}

/// 校验并规范化后的分区（`url` 已是最终写入 / 请求的地址）。
#[derive(Clone, PartialEq)]
pub struct ResolvedSection {
    pub section: Section,
    pub kind: ProviderKind,
    pub url: String,
    pub api_key: String,
    pub model: String,
    pub voice: String,
    pub speed: f64,
}

impl std::fmt::Debug for ResolvedSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSection")
            .field("section", &self.section)
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "***" })
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("speed", &self.speed)
            .finish()
    }
}

impl ResolvedSection {
    /// 只在内存中比较“保存的内容是否就是测试通过的内容”。
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.section.hash(&mut hasher);
        self.kind.hash(&mut hasher);
        self.url.hash(&mut hasher);
        self.api_key.hash(&mut hasher);
        self.model.hash(&mut hasher);
        self.voice.hash(&mut hasher);
        self.speed.to_bits().hash(&mut hasher);
        hasher.finish()
    }
}

fn validate_http_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("请填写地址".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("地址必须以 http:// 或 https:// 开头".into());
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c == '<' || c == '>')
    {
        return Err("地址包含空白或占位符".into());
    }
    Ok(())
}

/// 大模型写入 `url` 的基础地址（如 `https://<host>/v1`）；误填了完整 `/chat/completions` 时去掉。
pub fn llm_base_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/chat/completions")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_owned()
}

fn full_url(input: &str, suffix: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.ends_with(suffix) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}{suffix}")
    }
}

/// 语音识别 `base_url` 必须是完整的 `/audio/transcriptions` 地址；填基础地址时自动补全。
pub fn asr_full_url(input: &str) -> String {
    full_url(input, "/audio/transcriptions")
}

/// 语音合成 `api_url` 必须是完整的 `/audio/speech` 地址；填基础地址时自动补全。
pub fn tts_full_url(input: &str) -> String {
    full_url(input, "/audio/speech")
}

/// 从各分区填写的地址推导模型列表查询地址（`<基础地址>/models`），去掉已补全的具体端点后缀。
pub fn models_list_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with("/models") {
        return trimmed.to_owned();
    }
    let base = [
        "/chat/completions",
        "/audio/transcriptions",
        "/audio/speech",
    ]
    .into_iter()
    .fold(trimmed, |current, suffix| {
        current
            .strip_suffix(suffix)
            .unwrap_or(current)
            .trim_end_matches('/')
    });
    if base.is_empty() {
        String::new()
    } else {
        format!("{base}/models")
    }
}

/// 校验后的模型列表查询请求（查询时不要求先填模型名与音色）。
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedListRequest {
    pub section: Section,
    pub kind: ProviderKind,
    pub url: String,
    pub api_key: String,
}

impl std::fmt::Debug for ResolvedListRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedListRequest")
            .field("section", &self.section)
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "***" })
            .finish()
    }
}

/// 校验并规范化模型列表查询输入（内置服务云端模型）。
pub fn resolve_list_request(
    section: Section,
    input: &SectionInput,
) -> Result<ResolvedListRequest, String> {
    match input.kind {
        ProviderKind::Openai => {
            if input.url.trim().is_empty() {
                return Err(format!("{}：请先填写地址", section.label()));
            }
            let url = models_list_url(&input.url);
            validate_http_url(&url).map_err(|error| format!("{}：{error}", section.label()))?;
            reject_chat_only_audio_gateway(section, &url)?;
            Ok(ResolvedListRequest {
                section,
                kind: ProviderKind::Openai,
                url,
                api_key: input.api_key.trim().to_owned(),
            })
        }
        ProviderKind::Doubao => {
            let default_url =
                match section {
                    Section::Llm => return Err(
                        "大模型只支持 OpenAI 兼容接口（豆包大模型请填火山方舟的 OpenAI 兼容地址）"
                            .into(),
                    ),
                    Section::Asr => DOUBAO_ASR_URL,
                    Section::Tts => DOUBAO_TTS_URL,
                };
            let url = input.url.trim().trim_end_matches('/');
            let url = if url.is_empty() {
                default_url.to_owned()
            } else {
                url.to_owned()
            };
            if !url.starts_with("wss://") {
                return Err(format!("{}：豆包地址必须以 wss:// 开头", section.label()));
            }
            if url
                .chars()
                .any(|c| c.is_whitespace() || c == '<' || c == '>')
            {
                return Err(format!("{}：地址包含空白或占位符", section.label()));
            }
            Ok(ResolvedListRequest {
                section,
                kind: ProviderKind::Doubao,
                url,
                api_key: input.api_key.trim().to_owned(),
            })
        }
    }
}

/// 分区是否留空（留空时保存不修改该模块）。
pub fn is_blank(input: &SectionInput) -> bool {
    input.url.trim().is_empty() && input.model.trim().is_empty() && input.api_key.trim().is_empty()
}

/// 校验并规范化一个分区（外部服务端链路：只支持 OpenAI 兼容）；留空返回 `Ok(None)`。
pub fn resolve(section: Section, input: &SectionInput) -> Result<Option<ResolvedSection>, String> {
    if is_blank(input) {
        return Ok(None);
    }
    if !input.kind.is_openai() {
        return Err(format!(
            "{}：外部服务端链路只支持 OpenAI 兼容接口",
            section.label()
        ));
    }
    resolve_openai(section, input)
}

/// 内置语音服务的分区校验（NV7）：识别与合成可选豆包流式；留空返回 `Ok(None)`。
///
/// 豆包：地址留空用默认地址（必须是 `wss://`），“模型”留空用默认资源 ID；合成必须填音色。
pub fn resolve_builtin(
    section: Section,
    input: &SectionInput,
) -> Result<Option<ResolvedSection>, String> {
    if is_blank(input) {
        return Ok(None);
    }
    match input.kind {
        ProviderKind::Openai => resolve_openai(section, input),
        ProviderKind::Doubao => resolve_doubao(section, input).map(Some),
    }
}

fn resolve_doubao(section: Section, input: &SectionInput) -> Result<ResolvedSection, String> {
    let (default_url, default_resource) = match section {
        Section::Llm => {
            return Err(
                "大模型只支持 OpenAI 兼容接口（豆包大模型请填火山方舟的 OpenAI 兼容地址）".into(),
            );
        }
        Section::Asr => (DOUBAO_ASR_URL, DOUBAO_ASR_RESOURCE),
        Section::Tts => (DOUBAO_TTS_URL, DOUBAO_TTS_RESOURCE),
    };
    let url = input.url.trim().trim_end_matches('/');
    let url = if url.is_empty() {
        default_url.to_owned()
    } else {
        url.to_owned()
    };
    if !url.starts_with("wss://") {
        return Err(format!("{}：豆包地址必须以 wss:// 开头", section.label()));
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c == '<' || c == '>')
    {
        return Err(format!("{}：地址包含空白或占位符", section.label()));
    }
    let model = input.model.trim();
    let model = if model.is_empty() {
        default_resource.to_owned()
    } else {
        model.to_owned()
    };
    let (voice, speed) = if section == Section::Tts {
        let voice = input.voice.trim().to_owned();
        if voice.is_empty() {
            return Err("语音合成：请填写豆包音色 ID（见豆包控制台的音色列表）".into());
        }
        let speed = input.speed.unwrap_or(1.0);
        if !(0.5..=2.0).contains(&speed) {
            return Err("语音合成：豆包语速须在 0.5 ~ 2.0 之间".into());
        }
        (voice, speed)
    } else {
        (String::new(), 1.0)
    };
    Ok(ResolvedSection {
        section,
        kind: ProviderKind::Doubao,
        url,
        api_key: input.api_key.trim().to_owned(),
        model,
        voice,
        speed,
    })
}

fn resolve_openai(
    section: Section,
    input: &SectionInput,
) -> Result<Option<ResolvedSection>, String> {
    let url = match section {
        Section::Llm => llm_base_url(&input.url),
        Section::Asr => asr_full_url(&input.url),
        Section::Tts => tts_full_url(&input.url),
    };
    if input.url.trim().is_empty() {
        return Err(format!("{}：请填写地址", section.label()));
    }
    validate_http_url(&url).map_err(|error| format!("{}：{error}", section.label()))?;
    reject_chat_only_audio_gateway(section, &url)?;
    let model = input.model.trim().to_owned();
    if model.is_empty() {
        return Err(format!("{}：请填写模型名", section.label()));
    }
    let (voice, speed) = if section == Section::Tts {
        let voice = input.voice.trim().to_owned();
        if voice.is_empty() {
            return Err("语音合成：请填写音色".into());
        }
        let speed = input.speed.unwrap_or(1.0);
        if !(0.25..=4.0).contains(&speed) {
            return Err("语音合成：语速须在 0.25 ~ 4.0 之间".into());
        }
        (voice, speed)
    } else {
        (String::new(), 1.0)
    };
    Ok(Some(ResolvedSection {
        section,
        kind: ProviderKind::Openai,
        url,
        api_key: input.api_key.trim().to_owned(),
        model,
        voice,
        speed,
    }))
}

/// 一次测试的结果（不含密钥）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestResult {
    pub section: Section,
    pub ok: bool,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// 实际请求的完整地址（即将写入配置的值，供页面显示）
    pub final_url: String,
    pub detail: String,
    /// 语音合成测试成功时附带的音频 data URL（供页面点击“试听”播放，不含密钥）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_data_url: Option<String>,
}

/// 一次模型列表读取的结果（不含密钥）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelsListResult {
    pub section: Section,
    pub ok: bool,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    pub models: Vec<String>,
    /// 服务商 `/models` 条目自带分类元数据时解析出的模型可用分区（供前端融合筛选）
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub model_sections: std::collections::BTreeMap<String, Vec<Section>>,
    pub detail: String,
}

pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 将测试合成得到的音频字节封装为浏览器可直接播放的 `data:audio/...;base64,...`。
pub fn audio_data_url(bytes: &[u8], content_type: &str) -> String {
    let mime = if bytes.starts_with(b"RIFF") || content_type.contains("wav") {
        "audio/wav"
    } else if content_type.contains("ogg") {
        "audio/ogg"
    } else {
        "audio/mpeg"
    };
    format!("data:{mime};base64,{}", encode_base64(bytes))
}

fn client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(TEST_TIMEOUT)
                .tcp_nodelay(true)
                .pool_max_idle_per_host(8)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "无法创建网络客户端".to_owned())
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn describe_send_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "请求超时（15 秒）".into()
    } else if error.is_connect() {
        "无法连接到该地址".into()
    } else {
        "请求失败（网络或 TLS 错误）".into()
    }
}

fn read_limited_bytes(
    response: reqwest::blocking::Response,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "响应读取失败".to_owned())?;
    if bytes.len() as u64 > limit {
        return Err("响应内容过大".into());
    }
    Ok(bytes)
}

fn read_limited(response: reqwest::blocking::Response) -> Result<Vec<u8>, String> {
    read_limited_bytes(response, MAX_RESPONSE_BYTES)
}

fn http_error_detail(code: u16, bytes: &[u8], secret: Option<&str>) -> String {
    let hint = error_hint(bytes, secret);
    let detail = if code == 401 || code == 403 {
        format!("鉴权失败（HTTP {code}），请检查密钥")
    } else if code == 402 {
        format!(
            "账户余额不足（HTTP {code}；若已开通 ClinePass 等包月订阅，请选用 cline-pass/ 前缀模型）"
        )
    } else if code == 404 {
        format!("地址不存在（HTTP {code}），请检查地址")
    } else {
        format!("HTTP {code}")
    };
    if hint.is_empty() {
        detail
    } else {
        format!("{detail}：{hint}")
    }
}

/// 从错误响应里取一段可读的说明（只取 `error.message` 一类字段，截断，并脱敏）。
fn sanitize_and_bound_error_text(raw: &str, secret: Option<&str>) -> String {
    let replaced = match secret.map(str::trim).filter(|s| !s.is_empty()) {
        Some(sec) => raw.replace(sec, "***"),
        None => raw.to_owned(),
    };
    let redacted = crate::redact::redact_line(&replaced);
    let mut short: String = redacted.chars().take(160).collect();
    if redacted.chars().count() > 160 {
        short.push('…');
    }
    short
}

fn error_hint(bytes: &[u8], secret: Option<&str>) -> String {
    let text = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.pointer("/data/error/message"))
                .or_else(|| value.get("message"))
                .or_else(|| value.pointer("/data/message"))
                .or_else(|| value.get("error"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    if text.is_empty() {
        String::new()
    } else {
        sanitize_and_bound_error_text(&text, secret)
    }
}

fn chat_completions_payload(value: &serde_json::Value) -> &serde_json::Value {
    if value.get("choices").is_some() {
        value
    } else if let Some(inner) = value
        .get("data")
        .filter(|inner| inner.get("choices").is_some())
    {
        inner
    } else {
        value
    }
}

/// 用一个最小测试工具定义流式请求大模型，收到首个 `tool_calls` 分片即判定成功并断开（对齐 magpie 探针）。
pub fn llm_request_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": "你是连接测试助手。收到消息后立即调用 get_time 工具，禁止思考或直接回答。"},
            {"role": "user", "content": "现在几点？请调用工具查询。"}
        ],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_time",
                "description": "查询当前时间",
                "parameters": {"type": "object", "properties": {}, "required": []}
            }
        }],
        "tool_choice": "auto",
        "max_tokens": 512,
        "stream": true
    })
}

#[derive(Debug, PartialEq, Eq)]
enum LlmProbeOutcome {
    ToolCall,
    MessageWithoutToolCall,
    InvalidFormat,
    NotJson,
    StreamError(String),
}

fn inspect_llm_chunk(
    value: &serde_json::Value,
    secret: Option<&str>,
) -> (bool, bool, Option<String>) {
    let payload = chat_completions_payload(value);
    if value.get("type").and_then(serde_json::Value::as_str) == Some("error")
        || value.get("error").is_some_and(|err| !err.is_null())
        || payload.get("error").is_some_and(|err| !err.is_null())
    {
        let raw = serde_json::to_vec(value).unwrap_or_default();
        let hint = error_hint(&raw, secret);
        return (
            false,
            false,
            Some(if hint.is_empty() {
                "流式响应返回错误".into()
            } else {
                hint
            }),
        );
    }
    let Some(choice) = payload.get("choices").and_then(|choices| choices.get(0)) else {
        return (false, false, None);
    };
    let Some(node) = choice.get("delta").or_else(|| choice.get("message")) else {
        return (false, false, None);
    };
    let has_tool_call = node
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    (has_tool_call, true, None)
}

fn parse_sse_lines<R: BufRead>(
    mut reader: R,
    secret: Option<&str>,
) -> Result<LlmProbeOutcome, String> {
    let mut total_bytes: u64 = 0;
    let mut line = String::new();
    let mut saw_sse = false;
    let mut saw_message = false;
    let mut raw_fallback = String::new();
    loop {
        line.clear();
        let count = reader
            .read_line(&mut line)
            .map_err(|_| "响应读取失败".to_owned())?;
        if count == 0 {
            break;
        }
        total_bytes += count as u64;
        if total_bytes > MAX_RESPONSE_BYTES {
            return Err("响应内容过大".into());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(':') || trimmed.starts_with("event:") {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            saw_sse = true;
            let data = rest.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                break;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                let (has_tool_call, saw_choice, err) = inspect_llm_chunk(&value, secret);
                if let Some(err) = err {
                    return Ok(LlmProbeOutcome::StreamError(err));
                }
                if has_tool_call {
                    // 首包见到 tool_calls 立即返回，不再等待后续分片与流结束
                    return Ok(LlmProbeOutcome::ToolCall);
                }
                if saw_choice {
                    saw_message = true;
                }
            }
        } else if !saw_sse {
            raw_fallback.push_str(&line);
        }
    }
    if saw_sse {
        return Ok(if saw_message {
            LlmProbeOutcome::MessageWithoutToolCall
        } else {
            LlmProbeOutcome::InvalidFormat
        });
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_fallback.trim()) else {
        return Ok(LlmProbeOutcome::NotJson);
    };
    let (has_tool_call, saw_choice, err) = inspect_llm_chunk(&value, secret);
    if let Some(err) = err {
        Ok(LlmProbeOutcome::StreamError(err))
    } else if has_tool_call {
        Ok(LlmProbeOutcome::ToolCall)
    } else if saw_choice {
        Ok(LlmProbeOutcome::MessageWithoutToolCall)
    } else {
        Ok(LlmProbeOutcome::InvalidFormat)
    }
}

fn probe_llm_response(
    response: reqwest::blocking::Response,
    content_type: &str,
    secret: Option<&str>,
) -> Result<LlmProbeOutcome, String> {
    if content_type.contains("text/event-stream") {
        return parse_sse_lines(
            BufReader::new(response.take(MAX_RESPONSE_BYTES + 1)),
            secret,
        );
    }
    let bytes = read_limited(response)?;
    let trimmed_ascii = bytes
        .iter()
        .copied()
        .skip_while(|b| b.is_ascii_whitespace())
        .take(5)
        .collect::<Vec<_>>();
    if trimmed_ascii.eq_ignore_ascii_case(b"data:") || trimmed_ascii.starts_with(b"event") {
        return parse_sse_lines(BufReader::new(std::io::Cursor::new(bytes)), secret);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(LlmProbeOutcome::NotJson);
    };
    let (has_tool_call, saw_choice, err) = inspect_llm_chunk(&value, secret);
    if let Some(err) = err {
        Ok(LlmProbeOutcome::StreamError(err))
    } else if has_tool_call {
        Ok(LlmProbeOutcome::ToolCall)
    } else if saw_choice {
        Ok(LlmProbeOutcome::MessageWithoutToolCall)
    } else {
        Ok(LlmProbeOutcome::InvalidFormat)
    }
}

/// 内置的一段短音频（1 秒、16 kHz、16 位单声道的低音量 440 Hz 正弦波 WAV）。
pub fn test_wav() -> Vec<u8> {
    let rate: u32 = 16_000;
    let samples = rate as usize;
    let data_len = (samples * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for index in 0..samples {
        let t = index as f64 / rate as f64;
        let value = ((t * 440.0 * std::f64::consts::TAU).sin() * 1200.0) as i16;
        wav.extend_from_slice(&value.to_le_bytes());
    }
    wav
}

fn multipart_body(boundary: &str, model: &str, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"moss-test.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n--{boundary}--\r\n").as_bytes(),
    );
    body
}

fn with_auth(
    builder: reqwest::blocking::RequestBuilder,
    api_key: &str,
) -> reqwest::blocking::RequestBuilder {
    if api_key.is_empty() {
        builder
    } else {
        builder.bearer_auth(api_key)
    }
}

fn with_openai_compat(
    builder: reqwest::blocking::RequestBuilder,
    url: &str,
    api_key: &str,
) -> reqwest::blocking::RequestBuilder {
    let builder = with_auth(builder, api_key);
    if is_cline_api_url(url) {
        builder
            .header("X-Response-Format", "openai")
            .header("HTTP-Referer", "https://cline.bot")
            .header("X-Title", "Cline")
            .header("X-IS-MULTIROOT", "false")
            .header("X-CLIENT-TYPE", "cline-desktop")
    } else {
        builder
    }
}

/// 执行一次测试连接。密钥只在本函数内使用，返回值不含密钥。
pub fn test_section(resolved: &ResolvedSection) -> TestResult {
    let started = Instant::now();
    let section = resolved.section;
    let request_url = match section {
        Section::Llm => format!("{}/chat/completions", resolved.url),
        _ => resolved.url.clone(),
    };
    let finish = |ok: bool, http_status: Option<u16>, detail: String| {
        let detail = if resolved.api_key.is_empty() {
            detail
        } else {
            detail.replace(&resolved.api_key, "***")
        };
        TestResult {
            section,
            ok,
            elapsed_ms: started.elapsed().as_millis() as u64,
            http_status,
            final_url: resolved.url.clone(),
            detail,
            audio_data_url: None,
        }
    };
    let client = match client() {
        Ok(client) => client,
        Err(detail) => return finish(false, None, detail),
    };
    let request = match section {
        Section::Llm => {
            with_openai_compat(client.post(&request_url), &request_url, &resolved.api_key)
                .json(&llm_request_body(&resolved.model))
        }
        Section::Asr => {
            let boundary = format!(
                "moss-voice-test-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            );
            with_auth(client.post(&request_url), &resolved.api_key)
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(multipart_body(&boundary, &resolved.model, &test_wav()))
        }
        Section::Tts => {
            with_auth(client.post(&request_url), &resolved.api_key).json(&serde_json::json!({
                "model": resolved.model,
                "input": "你好，我是 MOSS。",
                "voice": resolved.voice,
                "speed": resolved.speed,
                "response_format": "mp3"
            }))
        }
    };
    let response = match request.send() {
        Ok(response) => response,
        Err(error) => return finish(false, None, describe_send_error(&error)),
    };
    let status = response.status();
    let code = status.as_u16();
    if status.is_redirection() {
        return finish(
            false,
            Some(code),
            format!("服务器返回重定向（HTTP {code}），按规则不跟随，请填写最终地址"),
        );
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !status.is_success() {
        let bytes = match read_limited(response) {
            Ok(bytes) => bytes,
            Err(detail) => return finish(false, Some(code), detail),
        };
        return finish(
            false,
            Some(code),
            http_error_detail(code, &bytes, Some(&resolved.api_key)),
        );
    }
    if section == Section::Llm {
        return match probe_llm_response(response, &content_type, Some(&resolved.api_key)) {
            Ok(LlmProbeOutcome::ToolCall) => finish(
                true,
                Some(code),
                "连接成功，模型返回了工具调用（tool_calls）".into(),
            ),
            Ok(LlmProbeOutcome::MessageWithoutToolCall) => finish(
                false,
                Some(code),
                "连接成功，但模型没有返回工具调用；MOSS 的工具需要支持函数调用的模型".into(),
            ),
            Ok(LlmProbeOutcome::StreamError(err)) => {
                finish(false, Some(code), format!("模型流式响应报错：{err}"))
            }
            Ok(LlmProbeOutcome::InvalidFormat) => {
                finish(false, Some(code), "返回格式不是 chat/completions".into())
            }
            Ok(LlmProbeOutcome::NotJson) => finish(
                false,
                Some(code),
                "返回的不是 JSON（地址可能不是 OpenAI 兼容接口）".into(),
            ),
            Err(detail) => finish(false, Some(code), detail),
        };
    }
    let bytes = match read_limited(response) {
        Ok(bytes) => bytes,
        Err(detail) => return finish(false, Some(code), detail),
    };
    match section {
        Section::Llm => unreachable!(),
        Section::Asr => {
            let value: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
            match value.as_ref().and_then(|value| value.get("text")) {
                Some(serde_json::Value::String(_)) => {
                    finish(true, Some(code), "连接成功，识别接口已返回结果".into())
                }
                _ => finish(
                    false,
                    Some(code),
                    "返回中没有 text 字段（地址可能不是 /audio/transcriptions）".into(),
                ),
            }
        }
        Section::Tts => {
            let looks_json = content_type.contains("json") || bytes.first() == Some(&b'{');
            if bytes.is_empty() || looks_json {
                finish(
                    false,
                    Some(code),
                    "没有返回音频数据（地址可能不是 /audio/speech）".into(),
                )
            } else {
                let mut res = finish(
                    true,
                    Some(code),
                    format!("连接成功，合成了 {} 字节音频", bytes.len()),
                );
                res.audio_data_url = Some(audio_data_url(&bytes, &content_type));
                res
            }
        }
    }
}

fn push_unique_section(list: &mut Vec<Section>, section: Section) {
    if !list.contains(&section) {
        list.push(section);
    }
}

fn extract_item_sections(item: &serde_json::Value) -> Option<Vec<Section>> {
    let obj = item.as_object()?;
    let mut sections = Vec::new();
    let mut explicit_non_voice = false;

    if let Some(endpoints) = obj
        .get("supported_endpoints")
        .and_then(serde_json::Value::as_array)
    {
        for ep in endpoints.iter().filter_map(serde_json::Value::as_str) {
            let lower = ep.to_ascii_lowercase();
            if lower.contains("chat/completions")
                || lower.contains("/responses")
                || lower.contains("/messages")
            {
                push_unique_section(&mut sections, Section::Llm);
            }
            if lower.contains("audio/transcriptions") {
                push_unique_section(&mut sections, Section::Asr);
            }
            if lower.contains("audio/speech") {
                push_unique_section(&mut sections, Section::Tts);
            }
        }
    }

    for key in ["sub_type", "type", "task", "pipeline_tag"] {
        if let Some(val) = obj.get(key).and_then(serde_json::Value::as_str) {
            let lower = val.trim().to_ascii_lowercase();
            match lower.as_str() {
                "asr"
                | "stt"
                | "speech-to-text"
                | "automatic-speech-recognition"
                | "audio-transcriptions" => {
                    push_unique_section(&mut sections, Section::Asr);
                }
                "tts" | "text-to-speech" | "audio-speech" => {
                    push_unique_section(&mut sections, Section::Tts);
                }
                "chat" | "llm" | "text-generation" | "conversational" => {
                    push_unique_section(&mut sections, Section::Llm);
                }
                "embedding" | "embeddings" | "rerank" | "reranker" | "text-to-image"
                | "image-to-image" | "text-to-video" | "moderation" | "image" | "video" => {
                    explicit_non_voice = true;
                }
                _ => {}
            }
        }
    }

    if let Some(modality) = item
        .pointer("/architecture/modality")
        .and_then(serde_json::Value::as_str)
    {
        let lower = modality.to_ascii_lowercase();
        if let Some((ins, outs)) = lower.split_once("->") {
            if ins.contains("audio") && outs.contains("text") {
                push_unique_section(&mut sections, Section::Asr);
            }
            if outs.contains("audio") {
                push_unique_section(&mut sections, Section::Tts);
            }
            if ins.contains("text")
                && outs.contains("text")
                && !ins.contains("audio")
                && !outs.contains("audio")
            {
                push_unique_section(&mut sections, Section::Llm);
            } else if ins.contains("text")
                && outs.contains("text")
                && ins.contains("audio")
                && outs.contains("audio")
            {
                push_unique_section(&mut sections, Section::Llm);
            }
            if outs.contains("image") && !outs.contains("text") {
                explicit_non_voice = true;
            }
        }
    }

    if !sections.is_empty() {
        Some(sections)
    } else if explicit_non_voice {
        Some(Vec::new())
    } else {
        None
    }
}

fn extract_models_and_sections(
    value: &serde_json::Value,
) -> Option<(
    Vec<String>,
    std::collections::BTreeMap<String, Vec<Section>>,
)> {
    let items = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .or_else(|| {
            value
                .pointer("/data/data")
                .and_then(serde_json::Value::as_array)
        })
        .or_else(|| {
            value
                .pointer("/data/models")
                .and_then(serde_json::Value::as_array)
        })
        .or_else(|| value.as_array())?;
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();
    let mut model_sections = std::collections::BTreeMap::new();
    for item in items {
        let raw = item
            .as_str()
            .or_else(|| item.get("id").and_then(serde_json::Value::as_str))
            .or_else(|| item.get("name").and_then(serde_json::Value::as_str));
        let Some(raw) = raw else { continue };
        let id = raw.trim();
        if id.is_empty() || id.chars().count() > 256 || id.chars().any(char::is_control) {
            continue;
        }
        if let Some(secs) = extract_item_sections(item) {
            model_sections.insert(id.to_owned(), secs);
        }
        if seen.insert(id.to_owned()) {
            models.push(id.to_owned());
            if models.len() >= MAX_MODELS_COUNT {
                break;
            }
        }
    }
    Some((models, model_sections))
}

/// 为 Cline 大模型列表置顶补齐 `cline-pass/` 订阅模型与 `cline-free/` 免费模型 ID 并去重
/// （对齐 `yetone/magpie` `internal/provider/cline.go`）。
pub fn prepend_cline_pass_models(remote: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut merged =
        Vec::with_capacity(CLINE_PASS_MODELS.len() + CLINE_FREE_MODELS.len() + remote.len());
    for preset in CLINE_PASS_MODELS.iter().chain(CLINE_FREE_MODELS.iter()) {
        if seen.insert((*preset).to_owned()) {
            merged.push((*preset).to_owned());
        }
    }
    for model in remote {
        if merged.len() >= MAX_MODELS_COUNT {
            break;
        }
        if seen.insert(model.clone()) {
            merged.push(model);
        }
    }
    merged
}

/// 测试 OpenRouter Decisions（Jev）连接；密钥只在本函数内使用，结果绝不回显密钥。
pub fn test_jev_decisions(endpoint: &str, api_key: &str, model: &str) -> serde_json::Value {
    let started = Instant::now();
    let api_key = api_key.trim();
    let finish = |ok: bool, http_status: Option<u16>, detail: String| {
        let detail = if api_key.is_empty() {
            detail
        } else {
            detail.replace(api_key, "***")
        };
        serde_json::json!({
            "section": "jev",
            "ok": ok,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "http_status": http_status,
            "final_url": endpoint,
            "detail": detail,
        })
    };
    let model = match crate::settings::validate_jev_model(model) {
        Ok(m) => m,
        Err(err) => return finish(false, None, err),
    };
    let is_local = crate::settings::is_local_jev_model(&model);
    if !is_local && api_key.is_empty() {
        return finish(false, None, "请先填写 OpenRouter API Key".into());
    }
    let client = match client() {
        Ok(c) => c,
        Err(detail) => return finish(false, None, detail),
    };
    let wire_model = if is_local {
        crate::settings::local_jev_upstream_model_name(&model)
    } else {
        model.as_str()
    };
    let payload = serde_json::json!({
        "model": wire_model,
        "state": { "utterance": "检查激光雕刻机连接状态" },
        "questions": {
            "probe": {
                "type": "noul",
                "instructions": "Does state.utterance explicitly ask to check laser machine connection status in this turn?"
            }
        }
    });
    let response = match with_auth(client.post(endpoint), api_key)
        .json(&payload)
        .send()
    {
        Ok(resp) => resp,
        Err(err) => return finish(false, None, describe_send_error(&err)),
    };
    let status = response.status();
    let code = status.as_u16();
    if status.is_redirection() {
        return finish(
            false,
            Some(code),
            format!("服务器返回重定向（HTTP {code}），按规则不跟随"),
        );
    }
    let bytes = match read_limited(response) {
        Ok(b) => b,
        Err(detail) => return finish(false, Some(code), detail),
    };
    if !status.is_success() {
        return finish(
            false,
            Some(code),
            http_error_detail(code, &bytes, Some(api_key)),
        );
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return finish(false, Some(code), "返回的不是合法 JSON".into());
    };
    if let Some(err_msg) = value
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
    {
        return finish(
            false,
            Some(code),
            format!(
                "服务返回错误：{}",
                sanitize_and_bound_error_text(err_msg, Some(api_key))
            ),
        );
    }
    let noul_ok = value
        .pointer("/answers/probe")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|obj| {
            obj.get("type").and_then(serde_json::Value::as_str) == Some("noul")
                && obj
                    .get("noul")
                    .and_then(serde_json::Value::as_f64)
                    .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
        });
    if noul_ok {
        finish(
            true,
            Some(code),
            format!("连接成功，Decisions 接口返回 noul 决策（{model}）"),
        )
    } else {
        finish(
            false,
            Some(code),
            "返回缺少 answers.probe.noul 字段（请确认该模型支持 OpenRouter Decisions 接口）".into(),
        )
    }
}

/// 返回可选 Jev 决策模型列表：内置预设 `typesafe/jev-1.14` / `1.13`，若有密钥则顺手合并在线新发的 `typesafe/jev-*`。
pub fn list_jev_models(models_url: &str, api_key: Option<&str>) -> serde_json::Value {
    let started = Instant::now();
    let mut seen = std::collections::HashSet::new();
    let mut merged: Vec<String> = BUILTIN_JEV_MODELS
        .iter()
        .filter(|m| seen.insert((**m).to_owned()))
        .map(|m| (*m).to_owned())
        .collect();
    let key = api_key.map(str::trim).unwrap_or("");
    let mut detail =
        "已列出内置常用 Jev 决策模型（也可直接手填新的 typesafe/jev-* 版本）".to_owned();
    let mut http_status = None;
    if !key.is_empty()
        && let Ok(client) = client()
        && let Ok(resp) = with_auth(client.get(models_url).timeout(LIST_TIMEOUT), key).send()
    {
        http_status = Some(resp.status().as_u16());
        if resp.status().is_success()
            && let Ok(bytes) = read_limited_bytes(resp, MAX_MODELS_RESPONSE_BYTES)
            && let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some((remote, _)) = extract_models_and_sections(&val)
        {
            let mut remote_jev: Vec<String> = remote
                .into_iter()
                .filter(|id| crate::settings::validate_jev_model(id).is_ok())
                .collect();
            remote_jev.sort_by(|a, b| b.cmp(a));
            let mut next = Vec::new();
            let mut local_seen = std::collections::HashSet::new();
            for item in remote_jev.into_iter().chain(merged.iter().cloned()) {
                if local_seen.insert(item.clone()) {
                    next.push(item);
                }
            }
            merged = next;
            detail = format!("已同步 OpenRouter 可用 Jev 模型（共 {} 个）", merged.len());
        }
    }
    serde_json::json!({
        "section": "jev",
        "ok": true,
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "http_status": http_status,
        "models": merged,
        "detail": detail,
    })
}

/// 读取服务商支持的全部模型列表（不做用途硬过滤，密钥只在本函数内使用且不回显）。
pub fn list_models(resolved: &ResolvedListRequest) -> ModelsListResult {
    let started = Instant::now();
    let section = resolved.section;
    let finish = |ok: bool,
                  http_status: Option<u16>,
                  models: Vec<String>,
                  model_sections: std::collections::BTreeMap<String, Vec<Section>>,
                  detail: String| {
        let detail = if resolved.api_key.is_empty() {
            detail
        } else {
            detail.replace(&resolved.api_key, "***")
        };
        ModelsListResult {
            section,
            ok,
            elapsed_ms: started.elapsed().as_millis() as u64,
            http_status,
            models,
            model_sections,
            detail,
        }
    };
    if resolved.kind == ProviderKind::Doubao {
        let presets: Vec<String> = match section {
            Section::Asr => DOUBAO_ASR_RESOURCES
                .iter()
                .map(|item| (*item).to_owned())
                .collect(),
            Section::Tts => DOUBAO_TTS_RESOURCES
                .iter()
                .map(|item| (*item).to_owned())
                .collect(),
            Section::Llm => Vec::new(),
        };
        return finish(
            true,
            None,
            presets,
            std::collections::BTreeMap::new(),
            "豆包流式无在线列表接口，已列出常用资源 ID，也可按控制台手动填写".into(),
        );
    }
    let cline_llm = section == Section::Llm && is_cline_api_url(&resolved.url);
    let client = match client() {
        Ok(client) => client,
        Err(detail) => {
            return finish(
                false,
                None,
                Vec::new(),
                std::collections::BTreeMap::new(),
                detail,
            );
        }
    };
    let response = match with_openai_compat(
        client.get(&resolved.url).timeout(LIST_TIMEOUT),
        &resolved.url,
        &resolved.api_key,
    )
    .send()
    {
        Ok(response) => response,
        Err(error) => {
            return finish(
                false,
                None,
                Vec::new(),
                std::collections::BTreeMap::new(),
                describe_send_error(&error),
            );
        }
    };
    let status = response.status();
    let code = status.as_u16();
    if status.is_redirection() {
        return finish(
            false,
            Some(code),
            Vec::new(),
            std::collections::BTreeMap::new(),
            format!("服务器返回重定向（HTTP {code}），按规则不跟随，请填写最终地址"),
        );
    }
    let bytes = match read_limited_bytes(response, MAX_MODELS_RESPONSE_BYTES) {
        Ok(bytes) => bytes,
        Err(detail) => {
            return finish(
                false,
                Some(code),
                Vec::new(),
                std::collections::BTreeMap::new(),
                detail,
            );
        }
    };
    if !status.is_success() {
        if cline_llm && code == 404 {
            let presets = prepend_cline_pass_models(Vec::new());
            return finish(
                true,
                Some(code),
                presets,
                std::collections::BTreeMap::new(),
                "Cline 在线列表不可用，已列出 ClinePass 订阅模型（也可手动输入其他模型名）".into(),
            );
        }
        return finish(
            false,
            Some(code),
            Vec::new(),
            std::collections::BTreeMap::new(),
            http_error_detail(code, &bytes, Some(&resolved.api_key)),
        );
    }
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            if cline_llm {
                let presets = prepend_cline_pass_models(Vec::new());
                return finish(
                    true,
                    Some(code),
                    presets,
                    std::collections::BTreeMap::new(),
                    "已列出 ClinePass 订阅模型（也可手动输入其他模型名）".into(),
                );
            }
            return finish(
                false,
                Some(code),
                Vec::new(),
                std::collections::BTreeMap::new(),
                "返回的不是 JSON（该地址可能不支持 /models 查询，可直接手动填写）".into(),
            );
        }
    };
    let Some((models, model_sections)) = extract_models_and_sections(&value) else {
        if cline_llm {
            let presets = prepend_cline_pass_models(Vec::new());
            return finish(
                true,
                Some(code),
                presets,
                std::collections::BTreeMap::new(),
                "已列出 ClinePass 订阅模型（也可手动输入其他模型名）".into(),
            );
        }
        return finish(
            false,
            Some(code),
            Vec::new(),
            std::collections::BTreeMap::new(),
            "返回格式不是模型列表（该地址可能不支持 /models 查询，可直接手动填写）".into(),
        );
    };
    if cline_llm {
        let models = prepend_cline_pass_models(models);
        let count = models.len();
        return finish(
            true,
            Some(code),
            models,
            model_sections,
            format!("已读取 {count} 个模型（ClinePass 订阅模型已置顶）"),
        );
    }
    if models.is_empty() {
        finish(
            true,
            Some(code),
            models,
            model_sections,
            "接口返回了空列表，请直接手动填写模型名".into(),
        )
    } else {
        let count = models.len();
        finish(
            true,
            Some(code),
            models,
            model_sections,
            format!("已读取 {count} 个模型"),
        )
    }
}

#[cfg(test)]
pub(crate) mod fake_http {
    //! 测试用的一次性 HTTP 服务：记录收到的请求并返回给定响应。
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    pub struct Received {
        pub head: String,
        pub body: Vec<u8>,
    }

    pub fn serve_once(
        status_line: &'static str,
        headers: &'static str,
        body: Vec<u8>,
    ) -> (String, mpsc::Receiver<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 8192];
            let (head_end, length) = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    return;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..pos]).to_string();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    break (pos + 4, length);
                }
            };
            while buffer.len() < head_end + length {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
            let request_body = buffer[head_end..].to_vec();
            let response = format!(
                "{status_line}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
            let _ = tx.send(Received {
                head,
                body: request_body,
            });
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }
}

#[cfg(test)]
mod tests {
    use super::fake_http::serve_once;
    use super::*;

    fn input(url: &str, key: &str, model: &str) -> SectionInput {
        SectionInput {
            url: url.into(),
            api_key: key.into(),
            model: model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn urls_are_normalized_to_final_values() {
        assert_eq!(llm_base_url(" https://h/v1/ "), "https://h/v1");
        assert_eq!(
            llm_base_url("https://h/v1/chat/completions"),
            "https://h/v1"
        );
        assert_eq!(
            asr_full_url("https://h/v1"),
            "https://h/v1/audio/transcriptions"
        );
        assert_eq!(
            asr_full_url("https://h/v1/audio/transcriptions/"),
            "https://h/v1/audio/transcriptions"
        );
        assert_eq!(tts_full_url("https://h/v1/"), "https://h/v1/audio/speech");
        assert_eq!(
            tts_full_url("https://h/v1/audio/speech"),
            "https://h/v1/audio/speech"
        );
    }

    #[test]
    fn resolve_validates_and_blank_means_unchanged() {
        assert_eq!(
            resolve(Section::Llm, &SectionInput::default()).unwrap(),
            None
        );
        assert!(resolve(Section::Llm, &input("ftp://h", "k", "m")).is_err());
        assert!(resolve(Section::Llm, &input("https://h/v1", "k", "")).is_err());
        assert!(resolve(Section::Llm, &input("https://<host>/v1", "k", "m")).is_err());
        let tts = SectionInput {
            voice: String::new(),
            ..input("https://h/v1", "k", "tts-1")
        };
        assert!(resolve(Section::Tts, &tts).unwrap_err().contains("音色"));
        let tts = SectionInput {
            voice: "alloy".into(),
            speed: Some(9.0),
            ..input("https://h/v1", "k", "tts-1")
        };
        assert!(resolve(Section::Tts, &tts).unwrap_err().contains("语速"));
        let resolved = resolve(Section::Asr, &input("https://h/v1", " k ", "whisper-1"))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.url, "https://h/v1/audio/transcriptions");
        assert_eq!(resolved.api_key, "k");
        // Debug 输出不含密钥
        let debug = format!(
            "{resolved:?} {:?}",
            input("https://h", "sk-secret-123", "m")
        );
        assert!(!debug.contains("sk-secret-123"), "{debug}");
    }

    #[test]
    fn llm_test_checks_tool_calls_and_sends_key_only_in_header() {
        let reply = br#"{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"1","type":"function","function":{"name":"get_time","arguments":"{}"}}]}}]}"#;
        let (base, rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            reply.to_vec(),
        );
        let resolved = resolve(
            Section::Llm,
            &input(&format!("{base}/v1"), "sk-fake-llm-key", "m1"),
        )
        .unwrap()
        .unwrap();
        let result = test_section(&resolved);
        assert!(result.ok, "{result:?}");
        assert_eq!(result.final_url, format!("{base}/v1"));
        let received = rx.recv().unwrap();
        assert!(
            received.head.starts_with("POST /v1/chat/completions"),
            "{}",
            received.head
        );
        assert!(
            received
                .head
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-fake-llm-key")
        );
        let body: serde_json::Value = serde_json::from_slice(&received.body).unwrap();
        assert_eq!(body["model"], "m1");
        assert_eq!(body["tools"][0]["function"]["name"], "get_time");
        let shown = serde_json::to_string(&result).unwrap();
        assert!(!shown.contains("sk-fake-llm-key"));

        // 有回答但没有工具调用：不算通过
        let reply = br#"{"choices":[{"message":{"role":"assistant","content":"12:00"}}]}"#;
        let (base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            reply.to_vec(),
        );
        let resolved = resolve(Section::Llm, &input(&base, "k", "m"))
            .unwrap()
            .unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert!(result.detail.contains("工具调用"), "{result:?}");
    }

    #[test]
    fn redirects_are_not_followed_and_auth_errors_are_explained() {
        let (base, _rx) = serve_once(
            "HTTP/1.1 302 Found",
            "Location: http://127.0.0.1:1/elsewhere\r\n",
            Vec::new(),
        );
        let resolved = resolve(
            Section::Tts,
            &SectionInput {
                voice: "alloy".into(),
                ..input(&base, "k", "tts-1")
            },
        )
        .unwrap()
        .unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert_eq!(result.http_status, Some(302));
        assert!(result.detail.contains("不跟随"), "{result:?}");

        let (base, _rx) = serve_once(
            "HTTP/1.1 401 Unauthorized",
            "Content-Type: application/json\r\n",
            br#"{"error":{"message":"Incorrect API key provided: sk-fake***"}}"#.to_vec(),
        );
        let resolved = resolve(Section::Llm, &input(&base, "sk-fake-key", "m"))
            .unwrap()
            .unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert!(result.detail.contains("鉴权失败"), "{result:?}");
        assert!(!result.detail.contains("sk-fake-key"));
    }

    #[test]
    fn asr_sends_multipart_audio_and_tts_expects_audio() {
        let (base, rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            br#"{"text":""}"#.to_vec(),
        );
        let resolved = resolve(Section::Asr, &input(&base, "k", "whisper-1"))
            .unwrap()
            .unwrap();
        let result = test_section(&resolved);
        assert!(result.ok, "{result:?}");
        let received = rx.recv().unwrap();
        assert!(
            received.head.starts_with("POST /audio/transcriptions"),
            "{}",
            received.head
        );
        assert!(received.head.contains("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&received.body);
        assert!(
            text.contains("name=\"model\"") && text.contains("whisper-1") && text.contains("RIFF")
        );

        let (base, rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: audio/mpeg\r\n",
            vec![0xFF, 0xFB, 0x90, 0x00, 1, 2, 3],
        );
        let tts = SectionInput {
            voice: "alloy".into(),
            speed: Some(1.2),
            ..input(&base, "k", "tts-1")
        };
        let result = test_section(&resolve(Section::Tts, &tts).unwrap().unwrap());
        assert!(result.ok, "{result:?}");
        let body: serde_json::Value = serde_json::from_slice(&rx.recv().unwrap().body).unwrap();
        assert_eq!(body["voice"], "alloy");
        assert_eq!(body["speed"], 1.2);

        let (base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            br#"{"error":"x"}"#.to_vec(),
        );
        let tts = SectionInput {
            voice: "alloy".into(),
            ..input(&base, "k", "tts-1")
        };
        assert!(!test_section(&resolve(Section::Tts, &tts).unwrap().unwrap()).ok);
    }

    #[test]
    fn doubao_is_only_for_builtin_asr_and_tts() {
        let doubao = |url: &str, model: &str| SectionInput {
            kind: ProviderKind::Doubao,
            ..input(url, "k", model)
        };
        // 外部服务端链路不接受豆包
        assert!(
            resolve(Section::Asr, &doubao("", ""))
                .unwrap_err()
                .contains("外部服务端")
        );
        assert!(
            resolve_builtin(Section::Llm, &doubao("", ""))
                .unwrap_err()
                .contains("OpenAI 兼容")
        );
        // 地址与资源 ID 留空用默认值
        let asr = resolve_builtin(Section::Asr, &doubao("", ""))
            .unwrap()
            .unwrap();
        assert_eq!(
            (asr.kind, asr.url.as_str(), asr.model.as_str()),
            (ProviderKind::Doubao, DOUBAO_ASR_URL, DOUBAO_ASR_RESOURCE)
        );
        let custom = resolve_builtin(
            Section::Asr,
            &doubao(
                "wss://h/api/v3/sauc/bigmodel/",
                "volc.seedasr.sauc.duration",
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (custom.url.as_str(), custom.model.as_str()),
            ("wss://h/api/v3/sauc/bigmodel", "volc.seedasr.sauc.duration")
        );
        assert!(
            resolve_builtin(Section::Asr, &doubao("https://h", ""))
                .unwrap_err()
                .contains("wss://")
        );
        // 合成：音色必填，语速 0.5 ~ 2.0
        assert!(
            resolve_builtin(Section::Tts, &doubao("", ""))
                .unwrap_err()
                .contains("音色")
        );
        let tts = SectionInput {
            voice: "zh_female_test".into(),
            speed: Some(1.5),
            ..doubao("", "")
        };
        let resolved = resolve_builtin(Section::Tts, &tts).unwrap().unwrap();
        assert_eq!(
            (
                resolved.url.as_str(),
                resolved.model.as_str(),
                resolved.speed
            ),
            (DOUBAO_TTS_URL, DOUBAO_TTS_RESOURCE, 1.5)
        );
        assert!(
            resolve_builtin(
                Section::Tts,
                &SectionInput {
                    speed: Some(3.0),
                    ..tts.clone()
                }
            )
            .unwrap_err()
            .contains("0.5")
        );
        // 接口类型参与“测试通过的就是保存的”比对
        let openai = resolve_builtin(
            Section::Tts,
            &SectionInput {
                kind: ProviderKind::Openai,
                url: "https://h/v1".into(),
                model: "tts-1".into(),
                ..tts
            },
        )
        .unwrap()
        .unwrap();
        assert_ne!(openai.fingerprint(), resolved.fingerprint());
        // 旧版输入没有 kind：按 OpenAI 兼容处理
        let legacy: SectionInput =
            serde_json::from_str(r#"{"url":"https://h/v1","model":"m"}"#).unwrap();
        assert_eq!(legacy.kind, ProviderKind::Openai);
    }

    #[test]
    fn unreachable_address_reports_without_url() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let resolved = resolve(
            Section::Llm,
            &input(&format!("http://127.0.0.1:{port}/v1?key=abc"), "k", "m"),
        )
        .unwrap()
        .unwrap();
        let result = test_section(&resolved);
        assert!(!result.ok);
        assert!(!result.detail.contains("key=abc"), "{result:?}");
    }

    #[test]
    fn test_wav_is_a_valid_one_second_pcm_file() {
        let wav = test_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 32_000);
    }

    #[test]
    fn list_models_normalizes_section_urls_deduplicates_and_hides_secrets() {
        assert_eq!(models_list_url(" https://h/v1/ "), "https://h/v1/models");
        assert_eq!(
            models_list_url("https://h/v1/chat/completions"),
            "https://h/v1/models"
        );
        assert_eq!(
            models_list_url("https://h/v1/audio/transcriptions/"),
            "https://h/v1/models"
        );
        assert_eq!(
            models_list_url("https://h/v1/audio/speech"),
            "https://h/v1/models"
        );
        assert_eq!(
            models_list_url("https://h/v1/models/"),
            "https://h/v1/models"
        );

        // 不要求提前填模型名或音色
        let asr_input = SectionInput {
            url: "https://h/v1/audio/transcriptions".into(),
            api_key: "sk-list-secret".into(),
            ..Default::default()
        };
        let req = resolve_list_request(Section::Asr, &asr_input).unwrap();
        assert_eq!(req.url, "https://h/v1/models");
        assert!(!format!("{req:?}").contains("sk-list-secret"));

        let reply = br#"{"object":"list","data":[{"id":"Qwen/Qwen2.5-7B-Instruct","type":"text","sub_type":"chat"},{"id":"FunAudioLLM/SenseVoiceSmall","type":"audio","sub_type":"asr"},{"id":"Qwen/Qwen2.5-7B-Instruct"},{"id":"  "},{"id":"bad\nname"},{"id":"MOSS/TTS-1","supported_endpoints":["/v1/audio/speech"]}]}"#;
        let (base, rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            reply.to_vec(),
        );
        let req = resolve_list_request(
            Section::Asr,
            &input(
                &format!("{base}/v1/audio/transcriptions"),
                "sk-list-secret",
                "",
            ),
        )
        .unwrap();
        let result = list_models(&req);
        assert!(result.ok, "{result:?}");
        assert_eq!(
            result.models,
            vec![
                "Qwen/Qwen2.5-7B-Instruct",
                "FunAudioLLM/SenseVoiceSmall",
                "MOSS/TTS-1"
            ]
        );
        assert_eq!(
            result.model_sections.get("Qwen/Qwen2.5-7B-Instruct"),
            Some(&vec![Section::Llm])
        );
        assert_eq!(
            result.model_sections.get("FunAudioLLM/SenseVoiceSmall"),
            Some(&vec![Section::Asr])
        );
        assert_eq!(
            result.model_sections.get("MOSS/TTS-1"),
            Some(&vec![Section::Tts])
        );
        let received = rx.recv().unwrap();
        assert!(
            received.head.starts_with("GET /v1/models "),
            "{}",
            received.head
        );
        assert!(
            received
                .head
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-list-secret")
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("sk-list-secret")
        );

        // 豆包流式直接返回常用资源 ID 候选，不发 HTTP 请求
        let doubao_req = resolve_list_request(
            Section::Asr,
            &SectionInput {
                kind: ProviderKind::Doubao,
                ..Default::default()
            },
        )
        .unwrap();
        let doubao_res = list_models(&doubao_req);
        assert!(doubao_res.ok);
        assert!(doubao_res.models.contains(&DOUBAO_ASR_RESOURCE.to_owned()));
    }

    #[test]
    fn cline_wrapped_payloads_402_hint_and_subscription_models_work() {
        assert!(is_cline_api_url("https://api.cline.bot/api/v1"));
        assert!(is_cline_api_url("https://API.CLINE.BOT/api/v1/models"));
        assert!(!is_cline_api_url("https://api.openai.com/v1"));
        assert!(!is_cline_api_url(
            "https://evil-api.cline.bot.example.com/v1"
        ));

        let merged = prepend_cline_pass_models(vec![
            "deepseek/deepseek-v4.1-flash-1017".into(),
            "cline-pass/deepseek-v4.1-flash".into(),
        ]);
        assert_eq!(merged[0], "cline-pass/deepseek-v4.1-flash");
        assert!(merged.contains(&"cline-pass/deepseek-v4.1-flash-1017".to_owned()));
        assert_eq!(
            merged
                .iter()
                .filter(|m| m.as_str() == "cline-pass/deepseek-v4.1-flash")
                .count(),
            1
        );
        assert_eq!(merged.last().unwrap(), "deepseek/deepseek-v4.1-flash-1017");

        // 非流式 `{ success: true, data: { choices: [...] } }` 包裹结构兼容解析
        let wrapped = br#"{"success":true,"data":{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"1","type":"function","function":{"name":"get_time","arguments":"{}"}}]}}]}}"#;
        let (base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            wrapped.to_vec(),
        );
        let resolved = resolve(
            Section::Llm,
            &input(
                &format!("{base}/api/v1"),
                "sk-cline-secret",
                "cline-pass/deepseek-v4.1-flash",
            ),
        )
        .unwrap()
        .unwrap();
        let result = test_section(&resolved);
        assert!(result.ok, "{result:?}");

        // 嵌套模型列表 `{ success: true, data: { data: [...] } }` 兼容解析
        let wrapped_models =
            br#"{"success":true,"data":{"object":"list","data":[{"id":"cline-pass/kimi-k3"}]}}"#;
        let (base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            wrapped_models.to_vec(),
        );
        let req = resolve_list_request(
            Section::Llm,
            &input(&format!("{base}/api/v1"), "sk-cline-secret", ""),
        )
        .unwrap();
        let listed = list_models(&req);
        assert!(listed.ok, "{listed:?}");
        assert_eq!(listed.models, vec!["cline-pass/kimi-k3"]);

        // HTTP 402 提示使用订阅前缀，且绝不泄露密钥
        let err_402 = br#"{"error":{"message":"Insufficient balance. Your Cline Credits balance is $-0.05"}}"#;
        let (base, _rx) = serve_once(
            "HTTP/1.1 402 Payment Required",
            "Content-Type: application/json\r\n",
            err_402.to_vec(),
        );
        let resolved = resolve(
            Section::Llm,
            &input(
                &format!("{base}/api/v1"),
                "sk-cline-secret",
                "deepseek/deepseek-v4.1-flash-1017",
            ),
        )
        .unwrap()
        .unwrap();
        let failed = test_section(&resolved);
        assert!(!failed.ok);
        assert!(failed.detail.contains("cline-pass/"), "{failed:?}");
        assert!(!failed.detail.contains("sk-cline-secret"));

        // 纯大模型网关（Cline / OpenRouter / DeepSeek 等）在语音识别或语音合成中直接拒绝，不拼凑虚假的 /audio/transcriptions
        let asr_cline_err = resolve_builtin(
            Section::Asr,
            &input(
                "https://api.cline.bot/api/v1",
                "sk-cline-secret",
                "cline-pass/deepseek-v4.1-flash",
            ),
        )
        .unwrap_err();
        assert!(
            asr_cline_err.contains("仅提供大模型对话接口"),
            "{asr_cline_err}"
        );
        let list_cline_err = resolve_list_request(
            Section::Asr,
            &input("https://api.cline.bot/api/v1", "sk-cline-secret", ""),
        )
        .unwrap_err();
        assert!(
            list_cline_err.contains("仅提供大模型对话接口"),
            "{list_cline_err}"
        );
    }

    #[test]
    fn llm_test_streams_and_stops_at_first_tool_call_delta() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Cache-Control: no-cache\r\n",
                "Connection: close\r\n\r\n",
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"thinking...\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_time\",\"arguments\":\"\"}}]}}]}\n\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.flush();
            // 故意延迟后续 chunk：探针应在收到首个 tool_calls 分片后立即断开返回，不等待此处 5 秒
            std::thread::sleep(Duration::from_secs(5));
            let _ = stream.write_all(b"data: [DONE]\n\n");
        });
        let base = format!("http://127.0.0.1:{port}/v1");
        let resolved = resolve(
            Section::Llm,
            &input(&base, "sk-stream-secret", "deepseek-reasoner"),
        )
        .unwrap()
        .unwrap();
        let result = test_section(&resolved);
        assert!(result.ok, "{result:?}");
        assert!(
            result.elapsed_ms < 2_000,
            "应在首包 tool_calls 到达时立即结束，实际耗时 {} ms",
            result.elapsed_ms
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("sk-stream-secret")
        );

        // SSE 流内返回 error 事件时按失败报告且脱敏
        let sse_err = b"data: {\"error\":{\"message\":\"Rate limit exceeded for sk-stream-secret\"}}\n\ndata: [DONE]\n\n";
        let (err_base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: text/event-stream\r\n",
            sse_err.to_vec(),
        );
        let err_resolved = resolve(
            Section::Llm,
            &input(&format!("{err_base}/v1"), "sk-stream-secret", "m"),
        )
        .unwrap()
        .unwrap();
        let err_res = test_section(&err_resolved);
        assert!(!err_res.ok, "{err_res:?}");
        assert!(
            err_res.detail.contains("Rate limit exceeded"),
            "{err_res:?}"
        );
        assert!(!err_res.detail.contains("sk-stream-secret"));

        // Jev Decisions 探针：校验 noul 响应且绝不泄露密钥
        let (jev_ok_url, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: application/json\r\n",
            br#"{"answers":{"probe":{"type":"noul","noul":0.92}}}"#.to_vec(),
        );
        let jev_ok = test_jev_decisions(
            &format!("{jev_ok_url}/api/alpha/decisions"),
            "sk-jev-secret",
            "typesafe/jev-1.14",
        );
        assert_eq!(jev_ok["ok"], true, "{jev_ok}");
        assert!(!jev_ok.to_string().contains("sk-jev-secret"));

        let (jev_err_url, _rx) = serve_once(
            "HTTP/1.1 401 Unauthorized",
            "Content-Type: application/json\r\n",
            br#"{"error":{"message":"Invalid key sk-jev-secret"}}"#.to_vec(),
        );
        let jev_err = test_jev_decisions(
            &format!("{jev_err_url}/api/alpha/decisions"),
            "sk-jev-secret",
            "typesafe/jev-1.14",
        );
        assert_eq!(jev_err["ok"], false);
        assert!(!jev_err.to_string().contains("sk-jev-secret"));
    }
    #[test]
    fn truncation_redacts_before_slicing_and_openrouter_origin_is_exact() {
        // 160 字符截断点恰好落在测试密钥内部时，必须在截断前完成脱敏，绝不泄露前半截密钥片段
        let boundary_secret = format!("firsthalf_{}_secondhalf", "a".repeat(14));
        let prefix = "Upstream gateway rejected request with diagnostic context: ".to_owned()
            + &"x".repeat(80)
            + " Invalid key ";
        assert!(
            prefix.chars().count() < 155 && prefix.chars().count() + boundary_secret.len() > 168
        );
        let body = serde_json::json!({"error": {"message": format!("{prefix}{boundary_secret}")}})
            .to_string();
        let (trunc_url, _rx) = serve_once(
            "HTTP/1.1 401 Unauthorized",
            "Content-Type: application/json\r\n",
            body.into_bytes(),
        );
        let trunc_err = test_jev_decisions(
            &format!("{trunc_url}/api/alpha/decisions"),
            &boundary_secret,
            "typesafe/jev-1.14",
        );
        let trunc_str = trunc_err.to_string();
        assert!(
            !trunc_str.contains("firsthalf_"),
            "密钥前缀片段不得因截断泄漏: {trunc_str}"
        );
        assert!(
            !trunc_str.contains("_secondhalf"),
            "密钥后段不得泄漏: {trunc_str}"
        );

        // HTTP 200 SSE 流内错误在 160 字符边界截断前同样须传入当前密钥脱敏，不得回显密钥前缀
        let sse_line = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"error": {"message": format!("{prefix}{boundary_secret}")}})
        );
        let (sse_trunc_base, _rx) = serve_once(
            "HTTP/1.1 200 OK",
            "Content-Type: text/event-stream\r\n",
            sse_line.into_bytes(),
        );
        let sse_resolved = resolve(
            Section::Llm,
            &input(&format!("{sse_trunc_base}/v1"), &boundary_secret, "m"),
        )
        .unwrap()
        .unwrap();
        let sse_res = test_section(&sse_resolved);
        assert!(!sse_res.ok, "{sse_res:?}");
        assert!(
            !sse_res.detail.contains("firsthalf_"),
            "SSE 流内错误不得泄露密钥前缀: {:?}",
            sse_res.detail
        );
        assert!(
            !sse_res.detail.contains("_secondhalf"),
            "SSE 流内错误不得泄露密钥后段: {:?}",
            sse_res.detail
        );

        // OpenRouter origin 严格校验：拒绝伪后缀域名、userinfo、http、非标端口及路径/query 伪装
        assert!(is_openrouter_https_url("https://openrouter.ai/api/v1"));
        assert!(is_openrouter_https_url(
            "https://OPENROUTER.AI:443/api/v1/chat/completions"
        ));
        for bad in [
            "https://openrouter.ai.example/v1",
            "https://evil-openrouter.ai/v1",
            "https://sub.openrouter.ai/v1",
            "http://openrouter.ai/api/v1",
            "https://openrouter.ai:8443/api/v1",
            "https://user:pass@openrouter.ai/api/v1",
            "https://example.com/v1?redirect=https://openrouter.ai",
            "https://example.com/openrouter.ai/v1",
        ] {
            assert!(!is_openrouter_https_url(bad), "{bad} must be rejected");
        }
    }
}
