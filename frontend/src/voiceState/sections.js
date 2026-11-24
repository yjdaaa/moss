// 云端模型分区（大模型 / 语音识别 / 语音合成）：表单、地址补全、模型列表与音色预设。

export const VOICE_POLL_MS = 4000;

export const SECTIONS = [
  { key: "llm", label: "大模型", hint: "填写基础地址，如 https://<服务地址>/v1" },
  { key: "asr", label: "语音识别", hint: "可填基础地址，自动补全为 …/audio/transcriptions" },
  { key: "tts", label: "语音合成", hint: "可填基础地址，自动补全为 …/audio/speech" },
];

/** 只有 MOSS 桌面端（Tauri 窗口、本机后端、有桌面会话）才显示语音中心入口（A-69）。 */
export function voiceCenterAvailable({ hasInvoke, desktopSession, remote }) {
  return Boolean(hasInvoke) && Boolean(desktopSession) && !remote;
}

function isAlibabaCompatHost(host) {
  const lower = String(host || "").toLowerCase();
  return (
    lower === "dashscope.aliyuncs.com" ||
    lower === "dashscope-intl.aliyuncs.com" ||
    lower === "qianwenaiapi.com" ||
    lower.endsWith(".qianwenaiapi.com") ||
    lower === "qwencloudapi.com" ||
    lower.endsWith(".qwencloudapi.com") ||
    lower === "maas.aliyuncs.com" ||
    lower.endsWith(".maas.aliyuncs.com")
  );
}

function cleanSchemeAndWhitespace(text) {
  const trimmed = String(text || "").trim();
  if (!trimmed) return "";
  const cleaned =
    /^(?:https?|wss?):\/\//i.test(trimmed) && !/[<>]/.test(trimmed)
      ? trimmed.replace(/\s+/g, "").replace(/^(https?|wss?):\/\//i, (m) => m.toLowerCase())
      : trimmed;
  return cleaned.replace(/\/+$/, "");
}

function completeAlibabaBareRoot(base) {
  if (!base || !/^https?:\/\//i.test(base)) return base;
  try {
    const parsed = new URL(base);
    if (
      (parsed.pathname === "/" || parsed.pathname === "") &&
      !parsed.search &&
      !parsed.hash &&
      isAlibabaCompatHost(parsed.hostname)
    ) {
      return `${base}/compatible-mode/v1`;
    }
  } catch {
    return base;
  }
  return base;
}

/** 清理多行复制粘贴带入的空白字符、统一小写协议头（保留 `<` `>` 以便拦截占位符），并为千问/百炼裸根域名自动补齐 `/compatible-mode/v1`。 */
export function cleanUrlInput(text) {
  return completeAlibabaBareRoot(cleanSchemeAndWhitespace(text));
}

function trimEndSlash(text) {
  return String(text || "").trim().replace(/\/+$/, "");
}

/**
 * 豆包流式（NV7，只用于内置服务的识别与合成）的默认地址与资源 ID，与桌面端 `moss_voice::models` 一致。
 * 表单的 `kind` 为 "doubao" 时使用；没有 `kind` 即 OpenAI 兼容（外部服务端向导的表单永远没有）。
 */
export const DOUBAO_DEFAULTS = {
  asr: { url: "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async", model: "volc.bigasr.sauc.duration" },
  tts: { url: "wss://openspeech.bytedance.com/api/v3/tts/bidirection", model: "seed-tts-2.0" },
};

/** 豆包流式无在线模型列表接口，内置常用资源 ID 供下拉选取（仍可手动填写其他控制台资源 ID）。 */
export const DOUBAO_MODEL_OPTIONS = {
  asr: [
    { id: "volc.bigasr.sauc.duration", hint: "流式识别 1.0 · 小时版（默认）" },
    { id: "volc.bigasr.sauc.concurrent", hint: "流式识别 1.0 · 并发版" },
    { id: "volc.seedasr.sauc.duration", hint: "流式识别 2.0 · 小时版" },
    { id: "volc.seedasr.sauc.concurrent", hint: "流式识别 2.0 · 并发版" },
  ],
  tts: [
    { id: "seed-tts-2.0", hint: "语音合成 2.0（默认）" },
    { id: "seed-tts-1.0", hint: "语音合成 1.0" },
    { id: "volc.service_type.10029", hint: "大模型语音合成 · 并发版" },
  ],
};

export function isDoubao(form) {
  return form?.kind === "doubao";
}

const OPENAI_VOICE_PRESETS = [
  { id: "alloy", hint: "Alloy · 中性均衡（默认）" },
  { id: "nova", hint: "Nova · 活力女声" },
  { id: "shimmer", hint: "Shimmer · 温柔女声" },
  { id: "echo", hint: "Echo · 温暖男声" },
  { id: "onyx", hint: "Onyx · 浑厚男声" },
  { id: "fable", hint: "Fable · 叙事音色" },
  { id: "coral", hint: "Coral · 自然女声" },
  { id: "sage", hint: "Sage · 知性音色" },
];

const COSYVOICE_SPEAKERS = [
  { suffix: "anna", hint: "Anna · 沉稳女声（默认）" },
  { suffix: "alex", hint: "Alex · 沉稳男声" },
  { suffix: "bella", hint: "Bella · 活力女声" },
  { suffix: "benjamin", hint: "Benjamin · 低沉男声" },
  { suffix: "charles", hint: "Charles · 磁性男声" },
  { suffix: "claire", hint: "Claire · 温柔女声" },
  { suffix: "david", hint: "David · 欢快男声" },
  { suffix: "diana", hint: "Diana · 欢快女声" },
];

const DOUBAO_V2_VOICE_PRESETS = [
  { id: "zh_female_vv_uranus_bigtts", hint: "Vivi 2.0 · 中文女声（默认）" },
  { id: "zh_female_xiaohe_uranus_bigtts", hint: "小荷 2.0 · 自然女声" },
  { id: "zh_male_m191_uranus_bigtts", hint: "云舟 2.0 · 沉稳男声" },
  { id: "zh_male_taocheng_uranus_bigtts", hint: "小天 2.0 · 阳光男声" },
];

const DOUBAO_V1_VOICE_PRESETS = [
  { id: "zh_female_cancan_mars_bigtts", hint: "灿灿 · 甜美女声（默认）" },
  { id: "zh_female_shuangkuaisisi_moon_bigtts", hint: "爽快思思 · 中文女声" },
  { id: "zh_male_wennuanahu_moon_bigtts", hint: "温暖阿虎 · 中文男声" },
  { id: "zh_female_wanwanxiaohe_moon_bigtts", hint: "湾湾小何 · 温柔女声" },
];

/** 根据当前语音合成表单（接口类型 + 模型名）返回推荐音色列表，仍支持手动填写任意音色。 */
export function ttsVoiceOptionsFor(form) {
  const model = String(form?.model || "").trim();
  if (isDoubao(form)) {
    const isV1 = model === "seed-tts-1.0" || model === "volc.service_type.10029";
    return isV1 ? DOUBAO_V1_VOICE_PRESETS : DOUBAO_V2_VOICE_PRESETS;
  }
  if (/cosyvoice|moss-ttsd/i.test(model)) {
    return COSYVOICE_SPEAKERS.map((item) => ({
      id: `${model}:${item.suffix}`,
      hint: item.hint,
    }));
  }
  return OPENAI_VOICE_PRESETS;
}

/** 选定合成模型或切换接口类型时，若音色为空或仍是旧模型的 `<oldModel>:<speaker>` 前缀，自动填入新模型默认音色。 */
export function applyDefaultTtsVoice(form, nextModel = form?.model) {
  const next = { ...form, model: nextModel };
  const options = ttsVoiceOptionsFor(next);
  const defaultVoice = options[0]?.id || "";
  const currentVoice = String(next.voice || "").trim();
  if (!currentVoice) {
    next.voice = defaultVoice;
    return next;
  }
  if (!isDoubao(next) && currentVoice.includes(":")) {
    const suffix = currentVoice.slice(currentVoice.lastIndexOf(":") + 1);
    if (/cosyvoice|moss-ttsd/i.test(String(nextModel || ""))) {
      next.voice = `${String(nextModel).trim()}:${suffix || "anna"}`;
    } else {
      next.voice = defaultVoice;
    }
  }
  return next;
}

/** 切换接口类型：切到豆包时填入默认地址与资源 ID；切回 OpenAI 兼容时清掉豆包地址。密钥、音色保留。 */
export function switchKind(section, form, kind) {
  const next = { ...form };
  if (kind === "doubao" && DOUBAO_DEFAULTS[section]) {
    next.kind = "doubao";
    if (!/^wss:\/\//i.test(String(next.url || "").trim())) next.url = DOUBAO_DEFAULTS[section].url;
    if (!String(next.model || "").trim() || !isDoubao(form)) next.model = DOUBAO_DEFAULTS[section].model;
  } else {
    delete next.kind;
    if (/^wss?:\/\//i.test(String(next.url || "").trim())) next.url = "";
    if (isDoubao(form)) next.model = "";
  }
  return next;
}

export const ALIBABA_TOKEN_PLAN_MODELS = [
  "auto",
  "qwen4-max",
  "qwen4-max-preview",
  "qwen3.8-max",
  "qwen3.8-flash",
  "qwen3.7-max",
  "qwen3.7-plus",
  "qwen3.6-flash",
  "deepseek-v4.1-flash",
  "deepseek-v4.1-flash-1017",
  "deepseek-v4-pro",
  "deepseek-v4-pro-0813",
  "deepseek-v4-flash-0731",
  "glm-5.3",
  "glm-5.2",
];

export const QWEN_MAAS_PRESET_MODELS = [
  "qwen3.8-flash",
  "qwen3.8-max",
  "qwen3.7-plus",
  "qwen3.7-max",
  "qwen3.6-flash",
  "qwen4-max",
  "qwen3-max",
  "qwen-plus",
  "qwen-turbo",
  "qwen-max",
  "qwen3-coder-plus",
];

export const ARK_PLAN_MODELS = [
  "ark-code-latest",
  "doubao-seed-evolving",
  "doubao-seed-2.1-pro",
  "doubao-seed-2.1-lite",
  "doubao-seed-2.0-mini",
  "minimax-m3",
  "glm-5.3",
  "glm-5.3-flash",
  "deepseek-v4.1-flash",
  "deepseek-v4.1-flash-economy",
  "deepseek-v4-flash",
  "deepseek-v4-pro",
  "kimi-k2.8-preview",
  "kimi-k3",
  "kimi-k3.1-pro",
];

/**
 * 尚未填写密钥或服务端 `/models` 不可用时，为千问开放平台 / 百炼 / Token Plan / 方舟套餐预先提供可选模型列表（对齐 `yetone/magpie` 内置目录与套餐列表）。
 */
export function presetModelOptionsFor(section, url) {
  if (section !== "llm") return [];
  const raw = cleanUrlInput(url);
  if (!raw || !/^https?:\/\//i.test(raw)) return [];
  try {
    const parsed = new URL(raw);
    const host = (parsed.hostname || "").toLowerCase();
    const path = (parsed.pathname || "").toLowerCase();
    if (host.startsWith("token-plan.") && isAlibabaCompatHost(host)) {
      return ALIBABA_TOKEN_PLAN_MODELS.map((id) => ({ id, hint: "Token Plan 套餐 · 大模型", sections: ["llm"] }));
    }
    if (isAlibabaCompatHost(host)) {
      return QWEN_MAAS_PRESET_MODELS.map((id) => ({ id, hint: "千问常用预设 · 大模型", sections: ["llm"] }));
    }
    if ((host === "volces.com" || host.endsWith(".volces.com")) && (path.includes("/api/coding") || path.includes("/api/plan"))) {
      return ARK_PLAN_MODELS.map((id) => ({ id, hint: "方舟套餐 · 大模型", sections: ["llm"] }));
    }
  } catch {
    return [];
  }
  return [];
}

/**
 * 检测千问开放平台 / 百炼 / Qwen Cloud 的按量地址与 `sk-sp-` 套餐密钥混填，返回提示文案与一键修正地址（纯内存判断，绝不拼接或回显密钥）。
 */
export function tokenPlanEndpointMismatch(url, apiKey) {
  const raw = cleanUrlInput(url);
  const key = String(apiKey || "").trim();
  if (!raw || !key || !/^https?:\/\//i.test(raw)) return null;
  try {
    const parsed = new URL(raw);
    const host = (parsed.hostname || "").toLowerCase();
    if (!isAlibabaCompatHost(host)) return null;
    const isPlanHost = host.startsWith("token-plan.");
    const isPlanKey = key.startsWith("sk-sp-");
    if (isPlanKey && !isPlanHost) {
      if (host === "qianwenaiapi.com" || host.endsWith(".qianwenaiapi.com")) {
        return {
          message: "检测到 sk-sp- 开头的千问套餐（Token Plan）密钥，按量地址无法鉴权。",
          suggestedUrl: "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1",
          actionLabel: "切换为千问套餐地址",
        };
      }
      if (host === "qwencloudapi.com" || host.endsWith(".qwencloudapi.com")) {
        return {
          message: "检测到 sk-sp- 开头的 Qwen Cloud 套餐密钥，需使用套餐专属地址。",
          suggestedUrl: "https://token-plan.maas.qwencloudapi.com/compatible-mode/v1",
          actionLabel: "切换为套餐专属地址",
        };
      }
      if (host === "dashscope-intl.aliyuncs.com") {
        return {
          message: "检测到 sk-sp- 开头的套餐密钥，国际站需使用套餐专属地址。",
          suggestedUrl: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
          actionLabel: "切换为国际站套餐地址",
        };
      }
      return {
        message: "检测到 sk-sp- 开头的百炼套餐（Token Plan）密钥，需使用套餐专属地址。",
        suggestedUrl: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        actionLabel: "切换为百炼套餐地址",
      };
    }
    if (!isPlanKey && isPlanHost) {
      if (host.endsWith(".qianwenaiapi.com")) {
        return {
          message: "当前为 Token Plan 套餐专属地址（需 sk-sp- 开头密钥）；按量付费密钥请用按量地址。",
          suggestedUrl: "https://maas.qianwenaiapi.com/compatible-mode/v1",
          actionLabel: "切换为千问按量地址",
        };
      }
      if (host.endsWith(".qwencloudapi.com")) {
        return {
          message: "当前为 Token Plan 套餐专属地址（需 sk-sp- 开头密钥）；按量付费密钥请用地域按量地址。",
          suggestedUrl: "https://ap-southeast-1.maas.qwencloudapi.com/compatible-mode/v1",
          actionLabel: "切换为按量地址",
        };
      }
      return {
        message: "当前为 Token Plan 套餐专属地址（需 sk-sp- 开头密钥）；百炼按量付费密钥请用按量地址。",
        suggestedUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        actionLabel: "切换为百炼按量地址",
      };
    }
  } catch {
    return null;
  }
  return null;
}

/**
 * 识别仅提供大模型对话接口、不提供 OpenAI `/audio/transcriptions` 与 `/audio/speech` 语音端点的网关
 * （对齐 `yetone/magpie` `internal/provider/presets.go` 的 Chat-only 服务商与 Coding/Token Plan 专属端点）。
 */
export function chatOnlyGatewayName(url) {
  const raw = cleanUrlInput(url);
  if (!raw || !/^https?:\/\//i.test(raw)) return "";
  try {
    const parsed = new URL(raw);
    const host = (parsed.hostname || "").toLowerCase();
    const path = (parsed.pathname || "").toLowerCase();
    if (host === "cline.bot" || host.endsWith(".cline.bot")) return "Cline (api.cline.bot)";
    if (host === "openrouter.ai" || host.endsWith(".openrouter.ai")) return "OpenRouter";
    if (host === "api.deepseek.com" || host.endsWith(".deepseek.com")) return "DeepSeek 官方 API";
    if (host === "api.anthropic.com" || host.endsWith(".anthropic.com")) return "Anthropic";
    if (host === "opencode.ai" || host.endsWith(".opencode.ai")) return "OpenCode Zen/Go";
    if (host === "api.kilo.ai" || host.endsWith(".kilo.ai")) return "Kilo Gateway";
    if (host === "api.moonshot.cn" || host === "api.moonshot.ai" || host === "api.kimi.com" || host === "api.kimi.ai") {
      return "Kimi / Moonshot";
    }
    if (
      host.startsWith("token-plan") ||
      path.includes("/api/coding") ||
      path.includes("/api/plan") ||
      path.includes("/plan/v") ||
      path.includes("/tokenplan/") ||
      path.includes("/step_plan/")
    ) {
      return "该 Coding / Token Plan 专属端点";
    }
  } catch {
    return "";
  }
  return "";
}

export function chatOnlyAudioGatewayError(section, url) {
  if (!section || section === "llm") return "";
  const gateway = chatOnlyGatewayName(url);
  if (!gateway) return "";
  const label = SECTION_USAGE_LABEL[section] || section;
  const endpoint = section === "asr" ? "/audio/transcriptions" : "/audio/speech";
  return `${gateway} 仅提供大模型对话接口，不支持${label}（无 ${endpoint} 端点）；请在顶部下拉框切换为「本地${label}」，或填写支持语音的服务商（如硅基流动 SiliconFlow、OpenAI、豆包流式）`;
}

/** 与桌面端一致的最终地址规则：大模型写基础地址，语音识别 / 合成写完整接口地址；豆包留空用默认地址；仅对话网关在语音分区不拼凑虚假音频路径。 */
export function finalUrl(section, url, kind) {
  const cleaned = cleanSchemeAndWhitespace(url);
  if (kind === "doubao") return cleaned || DOUBAO_DEFAULTS[section]?.url || "";
  if (!cleaned) return "";
  if (section === "llm") {
    const stripped = trimEndSlash(cleaned.replace(/\/chat\/completions$/, ""));
    return completeAlibabaBareRoot(stripped);
  }
  if (chatOnlyGatewayName(cleaned)) return "";
  const suffix = section === "asr" ? "/audio/transcriptions" : "/audio/speech";
  const stripped = cleaned.endsWith(suffix) ? trimEndSlash(cleaned.slice(0, -suffix.length)) : cleaned;
  const base = completeAlibabaBareRoot(stripped);
  return `${base}${suffix}`;
}

/** 从任意分区地址还原基础服务地址（去掉 `/chat/completions`、`/audio/transcriptions`、`/audio/speech`、`/models` 后再补齐裸根）。 */
export function baseServiceUrl(url) {
  const cleaned = cleanSchemeAndWhitespace(url);
  if (!cleaned) return "";
  const stripped = ["/chat/completions", "/audio/transcriptions", "/audio/speech", "/models"].reduce(
    (current, suffix) => (current.endsWith(suffix) ? trimEndSlash(current.slice(0, -suffix.length)) : current),
    cleaned,
  );
  return completeAlibabaBareRoot(stripped);
}

/** 与桌面端 `models_list_url` 一致：从任意分区填写的地址推导 `/models` 列表查询地址。 */
export function modelsListUrl(url) {
  const base = baseServiceUrl(url);
  return base ? `${base}/models` : "";
}

/**
 * 查找其他分区已保存的 OpenAI 兼容服务商信息：
 * - `sameOrigin`: 当前填写的 `formUrl` 与另一个已保存密钥的分区同源（基础 `/models` 地址一致），密钥留空可直接复用；
 * - `reusable`: 当前分区可一键填入的已配置服务商地址与本机密钥来源（优先大模型 `llm`；仅支持大模型的网关不会提供给语音识别/合成）。
 */
export function findPeerCloudSource(section, status, formUrl = "") {
  const models = status?.models || {};
  const currentListUrl = modelsListUrl(formUrl);
  let sameOrigin = null;
  let reusable = null;
  for (const item of SECTIONS) {
    if (item.key === section) continue;
    const peer = models[item.key];
    if (!peer?.configured || peer.kind === "doubao") continue;
    if (section !== "llm" && chatOnlyGatewayName(peer.url)) continue;
    const peerBase = baseServiceUrl(peer.url);
    const peerListUrl = modelsListUrl(peer.url);
    if (!peerBase || !peerListUrl) continue;
    const info = {
      section: item.key,
      label: item.label,
      baseUrl: peerBase,
      keySet: Boolean(peer.key_set),
    };
    if (currentListUrl && peerListUrl === currentListUrl && peer.key_set && !sameOrigin) {
      sameOrigin = info;
    }
    if (!reusable && (!currentListUrl || peerListUrl !== currentListUrl)) {
      reusable = info;
    }
  }
  return { sameOrigin, reusable };
}

/** 读取模型列表前的地址检查（不要求提前填写模型名与音色）。 */
export function modelsListError(form, section = "") {
  if (isDoubao(form)) return "";
  const url = cleanUrlInput(form?.url);
  if (!url) return "请先填写地址";
  if (!/^https?:\/\//i.test(url)) return "地址必须以 http:// 或 https:// 开头";
  if (/[<>\s]/.test(url)) return "地址包含空白或占位符";
  const gatewayErr = chatOnlyAudioGatewayError(section, url);
  if (gatewayErr) return gatewayErr;
  return "";
}

/** 是否满足自动读取模型列表的条件：OpenAI 兼容、地址有效，且已有密钥（含同源已存密钥或本机回环服务）。 */
export function canAutoFetchModels(form, entry, status = null, section = "") {
  if (isDoubao(form) || modelsListError(form, section)) return false;
  const cleanedUrl = cleanUrlInput(form?.url);
  if (String(form?.api_key || "").trim() || entry?.key_set) return true;
  if (status && section && findPeerCloudSource(section, status, cleanedUrl).sameOrigin) return true;
  return /^https?:\/\/(?:127\.0\.0\.1|localhost)(?::\d+)?(?:\/|$)/i.test(cleanedUrl);
}
/** 当前表单对应的模型列表缓存键（地址或密钥变动后重新读取）。 */
export function modelsListKey(form, entry, peerKeySet = false) {
  if (isDoubao(form)) return `doubao:${cleanSchemeAndWhitespace(form?.url)}`;
  return JSON.stringify([modelsListUrl(form?.url), String(form?.api_key || "").trim(), Boolean(entry?.key_set || peerKeySet)]);
}
const SECTION_USAGE_LABEL = { llm: "大模型", asr: "语音识别", tts: "语音合成" };
const OMNI_ALL_RE = /(?:qwen[\d.]*-omni|gpt-4o(?:-[a-z0-9]+)*-(?:audio|realtime)|gemini-[\d.]+-[a-z0-9-]*live|glm-4-voice|step-1o|minimax-omni)/i;
const OMNI_LLM_ASR_RE = /(?:qwen2-audio|qwen-audio-(?:turbo|chat)|ultravox|voxtral|salmonn|kimi-audio)/i;
const ASR_MODEL_RE =
  /(?:whisper|sensevoice|sense-voice|paraformer|funasr|teleasr|telespeech|fireredasr|zipformer|transcribe|speech-to-text|audio-to-text|canary-|dolphin-asr|wenet|conformer|(?:^|[/_-])(?:x-asr|asr|stt)(?:$|[/_-]|\d)|volc\.(?:bigasr|seedasr))/i;
const TTS_MODEL_RE =
  /(?:cosyvoice|fish-speech|fishaudio|indextts|index-tts|kokoro|matcha|f5-tts|spark-tts|chattts|melotts|vits|text-to-speech|text2speech|audio-speech|voice-clone|bark|seed-tts|volc\.service_type|(?:^|[/_-])tts(?:$|[/_-]|\d)|(?:^|[/_-])speech-(?:01|02|hd|turbo))/i;
const NON_CHAT_VOICE_RE =
  /(?:embed|bge-(?:large|base|small|m3|reranker)|rerank|bce-reranker|moderation|dall-e|flux[.\-_/]|stable-diffusion|sdxl|kolors|qwen-image|wanx|wan\d\.\d|happyhorse|cogview|cogvideo|seedream|imagen|hidream|sora|kling|vidu|hunyuanvideo|ltx-video|mochi-1|decision-model|gemini-decision|typesafe\/jev-(?!router))/i;

/**
 * 识别模型 ID 可用于哪些分区（`["llm" | "asr" | "tts"]`）：
 * - 全能/多模态语音模型（如 `Qwen2.5-Omni`、`gpt-4o-audio`、`Qwen2-Audio`）会同时返回 2 个或 3 个分区，在各对应分区均会显示；
 * - 向量、重排、生图、生视频等模型返回 `[]`，不混入默认推荐。
 */
export function classifyModelSections(modelId) {
  const id = String(modelId || "").trim();
  if (!id || NON_CHAT_VOICE_RE.test(id)) return [];
  if (OMNI_ALL_RE.test(id)) return ["llm", "asr", "tts"];
  if (OMNI_LLM_ASR_RE.test(id)) return ["llm", "asr"];
  if (/step-audio/i.test(id) && !/step-audio-(?:tts|asr)/i.test(id)) return ["asr", "tts"];
  const isAsr = ASR_MODEL_RE.test(id);
  const isTts = !/sensevoice|sense-voice/i.test(id) && TTS_MODEL_RE.test(id);
  if (isAsr || isTts) {
    const out = [];
    if (/(?:chat|instruct)/i.test(id)) out.push("llm");
    if (isAsr) out.push("asr");
    if (isTts) out.push("tts");
    return out;
  }
  return ["llm"];
}

export function modelUsageLabel(sections) {
  const list = Array.isArray(sections) ? sections.filter((s) => SECTION_USAGE_LABEL[s]) : [];
  if (list.length === 0) return "其他用途";
  return list.map((s) => SECTION_USAGE_LABEL[s]).join(" · ");
}

/** 下拉模型列表过滤：支持按当前分区用途（`section` + `onlyMatched`）自动筛选，并支持多词子串查询。 */
/** 下拉模型列表过滤：融合服务端 `modelSections` 元数据与模型名规则，支持按分区用途（`section` + `onlyMatched`）自动筛选及多词查询。 */
export function filterModelOptions(options, query, filtering = false, { section = "", onlyMatched = false, modelSections = null } = {}) {
  const normalized = (Array.isArray(options) ? options : [])
    .map((item) => {
      if (typeof item === "string") {
        const id = item.trim();
        const byName = classifyModelSections(id);
        const byMeta = modelSections && Array.isArray(modelSections[id]) ? modelSections[id] : null;
        const sections =
          byMeta !== null
            ? byMeta.length === 0
              ? []
              : [...new Set([...byMeta, ...byName.filter((s) => s !== "llm")])]
            : byName;
        const usageHint = modelUsageLabel(sections);
        const hint = id.startsWith("cline-pass/")
          ? "ClinePass 订阅"
          : id.startsWith("cline-free/") || id === "stealth/space-bunny-alpha"
            ? "Cline 免费"
            : "";
        const displayHint = [hint, usageHint].filter(Boolean).join(" · ");
        return { id, hint, sections, usageHint, displayHint };
      }
      const id = String(item?.id || "").trim();
      const hint = String(item?.hint || "").trim();
      const sections = Array.isArray(item?.sections) ? item.sections : section ? [section] : classifyModelSections(id);
      const usageHint = item?.usageHint || modelUsageLabel(sections);
      const displayHint = hint || usageHint;
      return { id, hint, sections, usageHint, displayHint };
    })
    .filter((item) => item.id);
  const matched =
    onlyMatched && section
      ? normalized.filter((item) => item.sections.includes(section))
      : normalized;
  const anyClassified = normalized.some((item) => item.sections.length > 0);
  const pool = onlyMatched && section && anyClassified ? matched : normalized;
  if (!filtering) return pool;
  const tokens = String(query || "").trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return pool;
  return pool.filter((item) => {
    const haystack = `${item.id} ${item.hint} ${item.usageHint}`.toLowerCase();
    return tokens.every((token) => haystack.includes(token));
  });
}

export function emptyForm(section) {
  return section === "tts"
    ? { url: "", api_key: "", model: "", voice: "", speed: "1" }
    : { url: "", api_key: "", model: "" };
}

export function isSectionBlank(form) {
  const value = form || {};
  return !String(value.url || "").trim() && !String(value.model || "").trim() && !String(value.api_key || "").trim();
}

/** 提交给桌面端的分区内容（自动清理多行粘贴带入的空白，语速转为数字）。 */
export function sectionPayload(section, form) {
  const value = form || {};
  const payload = {
    url: cleanUrlInput(value.url),
    api_key: String(value.api_key || "").trim(),
    model: String(value.model || "").trim(),
  };
  if (isDoubao(value)) payload.kind = "doubao";
  if (section === "tts") {
    payload.voice = String(value.voice || "").trim();
    const speed = Number(value.speed);
    payload.speed = Number.isFinite(speed) ? speed : 1;
  }
  return payload;
}

/**
 * 表单指纹：判断“要保存的内容就是测试通过的内容”。只在页面内存中使用，不写入任何存储。
 * 与桌面端的规则一致（地址按最终值比较）。
 */
export function sectionFingerprint(section, form) {
  const payload = sectionPayload(section, form);
  const kind = payload.kind || "openai";
  return JSON.stringify([section, kind, finalUrl(section, payload.url, payload.kind), payload.api_key, payload.model, payload.voice || "", payload.speed ?? 1]);
}

/** 前端先做的必填检查；返回错误文字或空串。 */
export function sectionInputError(section, form) {
  const payload = sectionPayload(section, form);
  if (payload.kind === "doubao") {
    if (section === "llm") return "大模型只支持 OpenAI 兼容接口";
    if (payload.url && !/^wss:\/\//i.test(payload.url)) return "豆包地址必须以 wss:// 开头";
    if (/[<>\s]/.test(payload.url)) return "地址包含空白或占位符";
    if (section === "tts") {
      if (!payload.voice) return "请填写豆包音色";
      if (!(payload.speed >= 0.5 && payload.speed <= 2)) return "豆包语速须在 0.5 ~ 2.0 之间";
    }
    return "";
  }
  if (!payload.url) return "请填写地址";
  if (!/^https?:\/\//i.test(payload.url)) return "地址必须以 http:// 或 https:// 开头";
  if (/[<>\s]/.test(payload.url)) return "地址包含空白或占位符";
  const gatewayErr = chatOnlyAudioGatewayError(section, payload.url);
  if (gatewayErr) return gatewayErr;
  if (!payload.model) return "请填写模型名";
  if (section === "tts") {
    if (!payload.voice) return "请填写音色";
    if (!(payload.speed >= 0.25 && payload.speed <= 4)) return "语速须在 0.25 ~ 4.0 之间";
  }
  return "";
}

/**
 * 模型分区的“保存”是否可点：该分区最近一次测试成功，且测试后没有改动（先测试后保存）。
 * `test` 为 `{ok, fingerprint}`。
 */
export function canSaveSection(section, form, test) {
  if (isSectionBlank(form) || sectionInputError(section, form)) return false;
  return Boolean(test && test.ok && test.fingerprint === sectionFingerprint(section, form));
}

/** 用已保存的内置模型设置（不含密钥）预填表单。 */
export function builtinModelForm(section, entry) {
  const form = emptyForm(section);
  if (!entry?.configured) return form;
  form.url = entry.url || "";
  form.model = entry.model || "";
  if (entry.kind === "doubao") form.kind = "doubao";
  if (section === "tts") {
    form.voice = entry.voice || "";
    form.speed = String(entry.speed ?? 1);
  }
  return form;
}

/** 密钥输入框的占位文字：已保存或同源已存时说明留空即沿用，绝不显示密钥本身。 */
export function builtinKeyPlaceholder(entry, sameOriginPeer = null) {
  if (entry?.key_set) return "已设置（留空则沿用已保存的密钥）";
  if (sameOriginPeer?.keySet) return `留空将自动复用“${sameOriginPeer.label}”已保存的密钥`;
  return "未设置（本地模型服务可以不填）";
}

/** 内置服务模型分组折叠时的一行摘要。 */
export function builtinModelSummary(section, entry, localOn = false) {
  if (localOn && section !== "llm") return entry?.configured ? "使用本地模型（云端设置作备用）" : "使用本地模型";
  if (!entry?.configured) return "未配置";
  const kind = entry.kind === "doubao" ? "豆包流式" : "";
  return ["已配置", kind, entry.model].filter(Boolean).join(" · ");
}
