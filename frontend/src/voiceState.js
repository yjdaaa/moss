/**
 * 语音中心页的纯逻辑（技术书 §9.4 / §9.5.1，实施清单 P4-08）：不依赖 React，便于单元测试。
 * 页面通过 Tauri 命令与桌面端通信（见 voiceApi.js）；浏览器、局域网、手机端没有语音中心。
 */

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

/**
 * 识别仅提供大模型对话接口、不提供 OpenAI `/audio/transcriptions` 与 `/audio/speech` 语音端点的网关
 * （对齐 `yetone/magpie` `internal/provider/presets.go` 的 Chat-only 服务商与 Coding/Token Plan 专属端点）。
 */
export function chatOnlyGatewayName(url) {
  const raw = String(url || "").trim();
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
    if (host.startsWith("token-plan") || path.includes("/api/coding") || path.includes("/tokenplan/") || path.includes("/step_plan/")) {
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
  if (kind === "doubao") return trimEndSlash(url) || DOUBAO_DEFAULTS[section]?.url || "";
  const base = trimEndSlash(url);
  if (!base) return "";
  if (section === "llm") return base.replace(/\/chat\/completions$/, "").replace(/\/+$/, "");
  if (chatOnlyGatewayName(base)) return "";
  const suffix = section === "asr" ? "/audio/transcriptions" : "/audio/speech";
  return base.endsWith(suffix) ? base : `${base}${suffix}`;
}

/** 从任意分区地址还原基础服务地址（去掉 `/chat/completions`、`/audio/transcriptions`、`/audio/speech`、`/models`）。 */
export function baseServiceUrl(url) {
  const trimmed = trimEndSlash(url);
  if (!trimmed) return "";
  return ["/chat/completions", "/audio/transcriptions", "/audio/speech", "/models"].reduce(
    (current, suffix) => (current.endsWith(suffix) ? trimEndSlash(current.slice(0, -suffix.length)) : current),
    trimmed,
  );
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
  const url = String(form?.url || "").trim();
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
  if (String(form?.api_key || "").trim() || entry?.key_set) return true;
  if (status && section && findPeerCloudSource(section, status, form?.url).sameOrigin) return true;
  return /^https?:\/\/(?:127\.0\.0\.1|localhost)(?::\d+)?(?:\/|$)/i.test(String(form?.url || "").trim());
}
/** 当前表单对应的模型列表缓存键（地址或密钥变动后重新读取）。 */
export function modelsListKey(form, entry, peerKeySet = false) {
  if (isDoubao(form)) return `doubao:${trimEndSlash(form?.url)}`;
  return JSON.stringify([modelsListUrl(form?.url), String(form?.api_key || "").trim(), Boolean(entry?.key_set || peerKeySet)]);
}
const SECTION_USAGE_LABEL = { llm: "大模型", asr: "语音识别", tts: "语音合成" };
const OMNI_ALL_RE = /(?:qwen[\d.]*-omni|gpt-4o(?:-[a-z0-9]+)*-(?:audio|realtime)|gemini-[\d.]+-[a-z0-9-]*live|glm-4-voice|step-1o|minimax-omni)/i;
const OMNI_LLM_ASR_RE = /(?:qwen2?-audio|ultravox|voxtral|salmonn|kimi-audio)/i;
const ASR_MODEL_RE =
  /(?:whisper|sensevoice|sense-voice|paraformer|funasr|teleasr|telespeech|fireredasr|zipformer|transcribe|speech-to-text|audio-to-text|canary-|dolphin-asr|wenet|conformer|(?:^|[/_-])(?:x-asr|asr|stt)(?:$|[/_-]|\d)|volc\.(?:bigasr|seedasr))/i;
const TTS_MODEL_RE =
  /(?:cosyvoice|fish-speech|fishaudio|indextts|index-tts|kokoro|matcha|f5-tts|spark-tts|chattts|melotts|vits|text-to-speech|text2speech|audio-speech|voice-clone|bark|seed-tts|volc\.service_type|(?:^|[/_-])tts(?:$|[/_-]|\d)|(?:^|[/_-])speech-(?:01|02|hd|turbo))/i;
const NON_CHAT_VOICE_RE =
  /(?:embed|bge-(?:large|base|small|m3|reranker)|rerank|bce-reranker|moderation|dall-e|flux[.\-_/]|stable-diffusion|sdxl|kolors|wanx|wan2\.\d|cogview|cogvideo|seedream|imagen|hidream|sora|kling|vidu|hunyuanvideo|ltx-video|mochi-1)/i;

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

/** 提交给桌面端的分区内容（语速转为数字）。 */
export function sectionPayload(section, form) {
  const value = form || {};
  const payload = {
    url: String(value.url || "").trim(),
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
 * “保存到服务端配置”是否可点：该分区最近一次测试成功，且测试后没有改动（先测试后保存）。
 * `test` 为 `{ok, fingerprint}`。
 */
export function canSaveSection(section, form, test, { fullModule = false } = {}) {
  if (fullModule) return false;
  if (isSectionBlank(form) || sectionInputError(section, form)) return false;
  return Boolean(test && test.ok && test.fingerprint === sectionFingerprint(section, form));
}

export const SERVER_STATE = {
  running: { text: "运行中（由 MOSS 启动）", tone: "ok" },
  external: { text: "外部运行", tone: "info" },
  exited: { text: "意外退出", tone: "bad" },
  stopped: { text: "未运行", tone: "off" },
};

export function serverStateView(server) {
  const state = server?.state || "stopped";
  const base = SERVER_STATE[state] || SERVER_STATE.stopped;
  return {
    ...base,
    state,
    canStart: state === "stopped" || state === "exited",
    // 外部启动的实例不显示停止按钮（A-63）
    canStop: state === "running" && Boolean(server?.owned),
    canRestart: state === "running" && Boolean(server?.owned),
  };
}

/** 设备卡与 OTA 一致性提示（§9.4.2 规则 2：无法判断时显示“未知”，不猜）。 */
export function deviceView(mode, device) {
  const state = device?.state || "unknown";
  if (mode === "builtin") {
    return {
      text: "见内置服务",
      tone: "info",
      hint: "内置链路：设备的 OTA 地址应指向本机；地址见“服务”标签，设备连接情况见“设备”标签。",
    };
  }
  if (mode !== "self_hosted") {
    return {
      text: "未知",
      tone: "off",
      hint: "官方链路下 MOSS 无法得知设备连接。若之前改过设备 OTA 地址，请改回官方服务器。",
    };
  }
  if (state === "online") {
    return { text: `在线（${device.sessions} 个会话）`, tone: "ok", hint: "一致：自建服务端已看到设备连接。" };
  }
  if (state === "waiting") {
    return {
      text: "等待设备",
      tone: "warn",
      hint: "自建服务端没有看到设备：请检查设备 OTA 地址是否指向自建服务端（http://<服务端地址>:8003/xiaozhi/ota/）。MOSS 不会修改设备。",
    };
  }
  return { text: "未知", tone: "off", hint: `无法判断设备当前连接的服务端：${device?.reason || "未填写接入点健康检查地址"}。` };
}

/**
 * 诊断分类 → 大白话（2026-11-07 技术书 n68；与 Rust `moss_voice_providers::DiagCategory` 一一对应）。
 * `cause` 说是哪一类问题，`next` 是没有更具体建议时的通用下一步。
 */
export const VOICE_DIAG_CATEGORIES = {
  general: { label: "一般", cause: "正常运行记录", next: "" },
  network: { label: "网络", cause: "网络问题：电脑连不上对方服务器，或连接中途断了", next: "检查电脑网络、代理和地址是否填对，稍后再试" },
  auth: { label: "密钥/权限", cause: "密钥或权限问题：密钥填错、过期，或没有开通这项服务", next: "到服务方控制台核对密钥和权限后重新填写" },
  quota: { label: "额度", cause: "额度问题：余额不足、今天的额度用完或调用太频繁", next: "充值或等一会儿再试，也可以到控制台提高限额" },
  config: { label: "配置", cause: "配置问题：地址、模型名或参数填得不对", next: "对照服务方文档检查设置" },
  device: { label: "设备", cause: "小智设备问题：断线、握手失败或设备回复了错误", next: "检查设备供电和 Wi-Fi，必要时重启设备或升级固件" },
  firmware: { label: "固件", cause: "固件或资源包问题：设备固件不支持，或推送令牌已失效", next: "确认固件版本，在对应页面重新推送" },
  model: { label: "模型服务", cause: "模型服务问题：服务方出错或返回了异常内容，或本地模型程序出错", next: "稍后再试，或换一个模型服务" },
  audio: { label: "声音", cause: "声音处理问题：音频解码、判停或播放出错", next: "换用标准小智固件或换一首歌再试" },
  internal: { label: "MOSS 内部", cause: "MOSS 程序内部出错（数据库、线程等）", next: "重启 MOSS 再试；一直出现请导出诊断包反馈" },
};

/** 常见错误码 → 大白话（后端 `message` 不够直白时补上“是什么问题、怎么办”）。 */
export const VOICE_ERROR_CODES = {
  voice_unavailable: "语音中心不可用：本机后端未启动。",
  voice_desktop_only: "语音中心仅在 MOSS 桌面端可用。",
  self_hosted_endpoint_missing: "还没有可用的自建接入点：在“服务 → 自建接入点”填好 ws:// 地址并保存。",
  server_not_owned: "正在运行的服务端不是 MOSS 启动的，请在启动它的地方关闭。",
  server_external: "服务端端口已被其他程序占用（外部运行的服务端），MOSS 不会再启动一个。",
  model_test_required: "请先用当前填写的内容测试连接并成功后再保存。",
  voice_acl_denied: "这个操作只能在 MOSS 主窗口里做。",
  voice_internal: "MOSS 内部出错了，重启 MOSS 再试；一直出现请导出诊断包反馈。",
  builtin_start_failed: "内置语音服务没能启动。",
  diagnostics_export_failed: "诊断包没有导出成功。",
};

function compactJson(value) {
  try {
    const text = JSON.stringify(value);
    if (!text || text === "{}") return "";
    return text.length > 200 ? `${text.slice(0, 200)}…` : text;
  } catch {
    return "";
  }
}

/**
 * 桌面端命令错误（`{code, message, category?, hint?}`、Error 或字符串）转为界面文字。
 * 先看错误码与分类给出大白话，再附上后端的具体原因与下一步；任何输入都不会输出 “[object Object]”。
 */
export function voiceErrorText(error) {
  if (error === null || error === undefined || error === "") return "操作失败（没有收到具体原因）";
  if (typeof error === "string") return error;
  if (typeof error !== "object") return String(error);
  const rawMessage = error.message;
  const message = typeof rawMessage === "string"
    ? rawMessage.trim()
    : rawMessage && typeof rawMessage === "object"
      ? voiceErrorText(rawMessage)
      : "";
  const code = typeof error.code === "string" ? error.code : "";
  const known = code ? VOICE_ERROR_CODES[code] : "";
  const category = typeof error.category === "string" ? VOICE_DIAG_CATEGORIES[error.category] : null;
  const hint = typeof error.hint === "string" ? error.hint.trim() : "";
  let text;
  if (known) {
    text = message && !known.includes(message) ? `${known}具体原因：${message}` : known;
  } else if (message) {
    text = message;
  } else if (category && category.cause !== VOICE_DIAG_CATEGORIES.general.cause) {
    text = category.cause;
  } else if (code) {
    text = `操作失败（错误码 ${code}）`;
  } else {
    const raw = compactJson(error);
    text = raw ? `操作失败（原始信息：${raw}）` : "操作失败（没有收到具体原因）";
  }
  if (hint && !text.includes(hint)) text = `${text.replace(/[。.]$/, "")}。${hint}`;
  return text;
}

/** 对话 / 排障轮次的结果（后端 `TurnTiming.outcome` / `TurnDebugTrace.outcome`）→ 中文。 */
export const VOICE_OUTCOMES = {
  running: { text: "进行中", tone: "info" },
  done: { text: "完成", tone: "ok" },
  interrupted: { text: "被打断", tone: "warn" },
  ignored: { text: "没识别出文字，已忽略", tone: "off" },
  music_started: { text: "转去播放音乐", tone: "info" },
  disconnected: { text: "设备断开，本轮中止", tone: "bad" },
  failed: { text: "失败", tone: "bad" },
};

export function voiceOutcomeLabel(outcome) {
  const key = String(outcome || "");
  return VOICE_OUTCOMES[key] || { text: key ? `未知结果（${key}）` : "未知结果", tone: "off" };
}

const EVENT_LEVELS = {
  info: { text: "记录", tone: "off" },
  warn: { text: "警告", tone: "warn" },
  error: { text: "错误", tone: "bad" },
};

function pad(value, size = 2) {
  return String(value).padStart(size, "0");
}

/** 事件时间：月-日 时:分:秒.毫秒（本机时区）；旧格式只有秒时不编造毫秒。 */
export function formatEventTime(event) {
  const ms = Number(event?.at_ms);
  const seconds = Number(event?.at);
  const hasMs = Number.isFinite(ms) && ms > 0;
  if (!hasMs && !(Number.isFinite(seconds) && seconds > 0)) return "—";
  const date = new Date(hasMs ? ms : seconds * 1000);
  const base = `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return hasMs ? `${base}.${pad(date.getMilliseconds(), 3)}` : base;
}

/** 一条服务事件的显示：级别、分类、时间、说明、下一步（事件自带优先，警告/错误缺省时用分类的通用建议）、技术细节。 */
/**
 * 事件来源（2026-11-09 用户要求）：语音中心“记录与诊断”同时显示小智、激光雕刻机与 MOSS 软件的事件，
 * 每条前面标明来源，方便判断是哪里出的错。旧记录没有 `source` 时按小智处理。
 */
export const VOICE_EVENT_SOURCES = {
  xiaozhi: { label: "小智", tone: "info" },
  laser: { label: "激光雕刻", tone: "warn" },
  moss: { label: "MOSS 软件", tone: "ok" },
};

export function voiceEventView(event) {
  const level = EVENT_LEVELS[event?.level] ? event.level : "info";
  const source = VOICE_EVENT_SOURCES[event?.source] ? event.source : "xiaozhi";
  const category = VOICE_DIAG_CATEGORIES[event?.category] ? event.category : "general";
  const meta = VOICE_DIAG_CATEGORIES[category];
  const own = typeof event?.hint === "string" ? event.hint.trim() : "";
  return {
    key: event?.seq ?? `${event?.at_ms ?? event?.at}-${event?.text}`,
    level,
    levelText: EVENT_LEVELS[level].text,
    tone: EVENT_LEVELS[level].tone,
    source,
    sourceLabel: VOICE_EVENT_SOURCES[source].label,
    sourceTone: VOICE_EVENT_SOURCES[source].tone,
    category,
    categoryLabel: meta.label,
    cause: level === "info" ? "" : meta.cause,
    time: formatEventTime(event),
    text: String(event?.text ?? ""),
    hint: own || (level === "info" ? "" : meta.next),
    detail: typeof event?.detail === "string" ? event.detail : "",
  };
}

/** “只看错误和警告”与来源筛选（`source` 为空或 `all` 时不按来源筛）；不改动原数组顺序（新的在前）。 */
export function filterVoiceEvents(events, onlyProblems, source = "all") {
  const list = Array.isArray(events) ? events : [];
  if (!onlyProblems && (!source || source === "all")) return list;
  return list.filter((event) => {
    if (onlyProblems && event?.level !== "warn" && event?.level !== "error") return false;
    if (source && source !== "all" && (VOICE_EVENT_SOURCES[event?.source] ? event.source : "xiaozhi") !== source) return false;
    return true;
  });
}

/** 各来源的事件条数与错误/警告条数（来源筛选按钮上显示）。 */
export function voiceEventSourceCounts(events) {
  const counts = Object.fromEntries(Object.keys(VOICE_EVENT_SOURCES).map((key) => [key, { total: 0, problems: 0 }]));
  for (const event of Array.isArray(events) ? events : []) {
    const key = VOICE_EVENT_SOURCES[event?.source] ? event.source : "xiaozhi";
    counts[key].total += 1;
    if (event?.level === "warn" || event?.level === "error") counts[key].problems += 1;
  }
  return counts;
}

/** 被覆盖记录的提示（计数为 0 时不提示）；绝不悄悄丢记录。 */
export function voiceEventDropNotices(drops) {
  const notices = [];
  const info = Number(drops?.info) || 0;
  const problems = Number(drops?.problems) || 0;
  const timings = Number(drops?.timings) || 0;
  if (problems) notices.push(`有 ${problems} 条较早的错误或警告已被覆盖（最多保留 ${drops?.max_problems ?? 200} 条）`);
  if (info) notices.push(`有 ${info} 条较早的日常记录已被覆盖（最多保留 ${drops?.max_info ?? 300} 条）`);
  if (timings) notices.push(`有 ${timings} 轮较早的耗时记录已被覆盖（最多保留 ${drops?.max_timings ?? 50} 轮）`);
  return notices;
}

/** 审批列表标题：后端最多返回 `limit` 条，满额时说明更早的没显示，不把它当总数。 */
export function approvalListSummary(count, limit, error) {
  if (error) return "读取失败";
  if (!count) return "暂无";
  const max = Number(limit) || 0;
  return max && count >= max ? `最近 ${count} 条（只显示最近 ${max} 条）` : `最近 ${count} 条`;
}

/** 审批失败 / 取消 / 摘要不一致时的原因（`outcome.message` 与错误码）；成功或尚无结果时为空。 */
export function approvalOutcomeReason(record) {
  const outcome = record?.outcome;
  if (!outcome || outcome.success) return "";
  const message = String(outcome.message || "").trim();
  const code = String(outcome.error_code || "").trim();
  if (message && code) return `${message}（错误码 ${code}）`;
  return message || (code ? `错误码 ${code}` : "");
}

const APPROVAL_STATUS = {
  pending: "待批准",
  approved: "已批准",
  rejected: "已拒绝",
  expired: "已过期",
};

/** 审批记录的状态（§9.5.1：待批准 / 已批准 / 已拒绝 / 已过期 / 摘要不一致 / 已完成 / 已取消 / 急停）。 */
export function approvalHistoryStatus(record) {
  const status = record?.status;
  const outcome = record?.outcome || {};
  const phase = outcome.phase;
  const message = String(outcome.message || "");
  if (outcome.error_code === "approval_mismatch") return { text: "摘要不一致", tone: "bad" };
  if (status === "approved" && phase === "running") return { text: "执行中", tone: "info" };
  if (phase === "cancelled" || status === "cancelled") {
    return /急停|0x18|estop/i.test(message) ? { text: "急停", tone: "bad" } : { text: "已取消", tone: "warn" };
  }
  if (status === "executed" || phase === "completed") {
    if (phase === "failed") return { text: "失败", tone: "bad" };
    if (phase === "unknown") return { text: "结果未知", tone: "warn" };
    return { text: "已完成", tone: "ok" };
  }
  if (status === "failed") return { text: "失败", tone: "bad" };
  if (APPROVAL_STATUS[status]) {
    const tone = status === "pending" ? "warn" : status === "approved" ? "info" : "off";
    return { text: APPROVAL_STATUS[status], tone };
  }
  return { text: "未知", tone: "off" };
}

/** 切换确认框正文。 */
const MODE_TITLE = {
  self_hosted: "切换到自建服务端？",
  builtin: "切换到 MOSS 内置服务？",
  official: "切换回官方小智？",
};

export function modeConfirmText(target, changes) {
  const title = MODE_TITLE[target] || MODE_TITLE.official;
  const lines = (Array.isArray(changes) ? changes : []).map((line) => `• ${line}`);
  return { title, message: ["以下设置将被修改（写入 settings.json）：", ...lines].join("\n") };
}

/** 日志行中残留的密钥样式再做一次遮挡（桌面端已脱敏，这里是兜底）。 */
export function maskLogLine(line) {
  return String(line || "").replace(
    /((?:api[_-]?key|apikey|authorization|access_token|token|secret|password)["']?\s*[:=]\s*["']?)(?:bearer\s+|basic\s+)?[^\s"',;&}]+/gi,
    (_, prefix) => `${prefix}***`,
  );
}

const DEVICE_STATUS = {
  pending: { text: "待允许", tone: "warn" },
  allowed: { text: "已允许", tone: "ok" },
  revoked: { text: "已撤销", tone: "off" },
};

/**
 * 设备信息快照（schema 14）：把设备开机上报（info）、连接握手（hello）、实时状态（status）
 * 三段原始 JSON 摊成可显示的行。字段随固件版本变化，缺什么就显示「—」，不猜。
 */
export function builtinDeviceInfoView(device) {
  const parse = (text) => {
    if (!text) return null;
    try {
      const value = JSON.parse(text);
      return value && typeof value === "object" ? value : null;
    } catch {
      return null;
    }
  };
  const text = (value) => (value === undefined || value === null || value === "" ? "—" : String(value));
  const size = (bytes) => {
    const number = Number(bytes);
    if (!Number.isFinite(number) || number <= 0) return "—";
    return number >= 1024 * 1024 ? `${Math.round(number / 1024 / 1024)} MB` : `${Math.round(number / 1024)} KB`;
  };
  const info = parse(device?.info_json);
  const hello = parse(device?.hello_json);
  const status = parse(device?.status_json);
  const app = info?.application || {};
  const audio = hello?.audio;
  const rows = [
    ["固件版本", text(app.version || device?.firmware_version)],
    ["应用名", text(app.name)],
    ["芯片", text(info?.chip_model_name)],
    ["Flash / PSRAM", `${size(info?.flash_size)} / ${size(info?.psram_size)}`],
    ["服务端 AEC 声明", typeof hello?.aec === "boolean" ? (hello.aec ? "已声明" : "未声明") : "未上报"],
    ["设备端回声消除", "固件不上报，看实际拾音模式（realtime 即已开启），本面板不推断"],
    [
      "实际拾音模式",
      (() => {
        const mode = parse(device?.listen_json)?.mode;
        if (mode === "realtime") return "realtime（固件选了实时对话，说明设备端 AEC 开启）";
        if (mode === "auto") return "auto（设备端 AEC 关闭或未启用）";
        if (mode === "manual") return "manual（手动说话，无法判断 AEC）";
        return "未上报";
      })(),
    ],
    ["帧协议", hello ? `v${text(hello.protocol_version)} · ${text(hello.transport)}` : "—"],
    ["上行音频", audio ? `${text(audio.sample_rate)} Hz · ${text(audio.format)} · ${text(audio.frame_duration)} ms` : "—"],
    ["设备自带 MCP", hello ? (hello.mcp ? "支持" : "不支持") : "—"],
    ["界面语言", text(info?.language)],
    ["IDF 版本", text(app.idf_version)],
    ["编译时间", text(app.compile_time)],
    ["MAC", text(info?.mac_address)],
    ["UUID", text(info?.uuid)],
  ];
  return {
    rows,
    hasInfo: Boolean(info),
    hasHello: Boolean(hello),
    hasStatus: Boolean(status),
    status: status || null,
    updatedAt: Number(device?.info_at) || 0,
  };
}
/**
 * 内置说话角色（n59；编号与后端 `BUILTIN_PERSONA_IDS` 一致）。角色只改语气，声音由语音合成设置决定。
 * n75（2026-11-08 用户决定）：7 个减为 4 个；“风险评估”改为所有角色可用的开关 `persona_risk_notes`。
 */
export const PERSONA_STYLE_OPTIONS = [
  { id: "workshop", label: "简明工坊助手（默认）", hint: "干练直接，专注激光雕刻指令与状态反馈" },
  { id: "beginner_guide", label: "新手引导", hint: "耗材、对焦、排风、走边框多给一句大白话提醒" },
  { id: "moss_station", label: "MOSS（电影原版）", hint: "像《流浪地球》的 MOSS：收到、先报结论，平稳无起伏，用“提示：”“警告：”开头；没有真实数据就说“数据不足，无法计算”" },
  { id: "moss_deadpan", label: "MOSS 冷面幽默", hint: "正事说完偶尔一句冷幽默；出光、急停、故障时不开玩笑" },
];

/** n75 已合并的旧角色编号 → 现在的编号（与后端 `canonical_persona_style` 一致）。 */
export function canonicalPersonaStyle(id) {
  if (id === "moss_briefing" || id === "moss_risk") return "moss_station";
  if (id === "moss_night") return "workshop";
  return id;
}
export const MAX_CUSTOM_PERSONAS = 6;

/** 自建角色在表单里存为 JSON 文字（便于未保存改动比较），这里读回数组。 */
export function parseCustomPersonas(raw) {
  try {
    const list = typeof raw === "string" ? JSON.parse(raw || "[]") : raw;
    return (Array.isArray(list) ? list : []).map((role) => ({
      id: String(role?.id || ""),
      name: String(role?.name || ""),
      prompt: String(role?.prompt || ""),
    }));
  } catch {
    return [];
  }
}

/** 下拉框里的全部角色：内置在前，自建在后。 */
export function personaOptions(customRaw) {
  const custom = parseCustomPersonas(customRaw)
    .filter((role) => role.id && role.name.trim())
    .map((role) => ({ id: role.id, label: `${role.name.trim()}（自建）`, hint: role.prompt.trim().slice(0, 40) }));
  return [...PERSONA_STYLE_OPTIONS, ...custom];
}

export function newCustomPersonaId(existing = []) {
  const used = new Set(existing.map((role) => role.id));
  for (let i = 1; ; i += 1) {
    const id = `custom_${Date.now().toString(36).slice(-6)}${i}`;
    if (!used.has(id)) return id;
  }
}

/** 语音按名字切换时只看文字和数字、不分大小写（与后端 `persona_name_key` 一致）。 */
function personaNameKey(name) {
  return String(name || "").replace(/[^\p{L}\p{N}]/gu, "").toLowerCase();
}

function customPersonasError(raw) {
  if (typeof raw === "string") {
    try {
      if (!Array.isArray(JSON.parse(raw || "[]"))) return "自建角色数据损坏，请刷新页面";
    } catch {
      return "自建角色数据损坏，请刷新页面";
    }
  }
  const roles = parseCustomPersonas(raw);
  if (roles.length > MAX_CUSTOM_PERSONAS) return `自建角色最多 ${MAX_CUSTOM_PERSONAS} 个`;
  const names = new Set();
  const ids = new Set();
  for (const role of roles) {
    if (!/^custom_[a-z0-9]{1,16}$/.test(role.id)) return "自建角色编号无效，请删除后重新新增";
    if (ids.has(role.id)) return "自建角色编号重复，请删除后重新新增";
    ids.add(role.id);
    const name = role.name.trim();
    if (!name) return "自建角色需要名字";
    if ([...name].length > 12) return "角色名最多 12 个字";
    if (/[\x00-\x1f\x7f]/.test(name) || containsDangerousGcodeOrGrblJs(name)) return "角色名不能包含控制字符或 G-code";
    const key = personaNameKey(name);
    if (!key) return "角色名需要包含文字或数字";
    if (names.has(key)) return `角色名“${name}”和已有角色读起来一样`;
    names.add(key);
    const prompt = role.prompt.replace(/\r\n?/g, "\n").trim();
    if (!prompt) return `角色“${name}”需要写人设`;
    if (/[\x00-\x09\x0b-\x1f\x7f]/.test(prompt)) return "角色人设不能包含控制字符";
    if ([...prompt].length > 300) return "角色人设最多 300 个字符";
    if (containsDangerousGcodeOrGrblJs(prompt)) return "角色人设不能包含 G-code 或控制器 $n= 指令";
  }
  return "";
}

export const SCREEN_CARD_STYLE_OPTIONS = [
  {
    id: "moss_hud",
    label: "硬核太空舱（MOSS HUD）",
    shortLabel: "硬核太空舱",
    hint: "直角瞄准刻度、深空暗底与 MOSS 红色光学眼",
  },
  {
    id: "goggle_bot",
    label: "可爱像素小机器人（Goggle Bot）",
    shortLabel: "可爱像素小机器人",
    hint: "星空暗底、小 MOSS 护目镜像素小机器人陪你聊天、雕刻、审批、听歌",
  },
];

// n70（2026-11-08）：小智全屏画风 12 个场景，顺序与 `moss-voice-server::screen_pack::SCENES` 一致。
export const GOGGLE_BOT_POSES = [
  { id: "idle", label: "待命眨眼", desc: "护目镜架额头眨眼；固件待命时显示", mossLabel: "太空舱待命红眼", mossDesc: "红色光学眼慢呼吸；固件待命时显示", botLabel: "待命眨眼", botDesc: "护目镜架额头眨眼；固件待命时显示" },
  { id: "listen", label: "竖耳聆听", desc: "耳罩亮起，手拢在耳边", mossLabel: "太空舱声波聆听", mossDesc: "青色声波圈一圈圈扩散", botLabel: "竖耳聆听", botDesc: "耳罩亮起，手拢在耳边" },
  { id: "think", label: "托腮思考", desc: "眼睛往右上看，冒思考泡泡", mossLabel: "太空舱扫描思考", mossDesc: "琥珀扫描环转圈", botLabel: "托腮思考", botDesc: "眼睛往右上看，冒思考泡泡" },
  { id: "talk", label: "张嘴说话", desc: "嘴巴一张一合，小手比划", mossLabel: "太空舱波形说话", mossDesc: "两侧声波柱跟着跳", botLabel: "张嘴说话", botDesc: "嘴巴一张一合，小手比划" },
  { id: "happy", label: "开心举手", desc: "开心类表情：月牙笑眼、双手举起", mossLabel: "太空舱青色愉悦", mossDesc: "开心类表情：青色光眼 + 上升箭头", botLabel: "开心举手", botDesc: "开心类表情：月牙笑眼、双手举起" },
  { id: "sad", label: "难过掉泪", desc: "难过类表情：耷拉眼、小乌云下雨", mossLabel: "太空舱蓝色低落", mossDesc: "难过类表情：蓝色光眼 + 雨丝", botLabel: "难过掉泪", botDesc: "难过类表情：耷拉眼、小乌云下雨" },
  { id: "surprised", label: "惊讶圆眼", desc: "惊讶类表情：圆眼 O 形嘴、感叹号", mossLabel: "太空舱白光惊讶", mossDesc: "惊讶类表情：白光眼 + 闪光环", botLabel: "惊讶圆眼", botDesc: "惊讶类表情：圆眼 O 形嘴、感叹号" },
  { id: "engrave", label: "戴护目镜雕刻", desc: "开始雕刻后显示；有刀路时中央叠刀路与进度", mossLabel: "太空舱雕刻表盘", mossDesc: "开始雕刻后显示；有刀路时中央叠刀路与进度", botLabel: "戴护目镜雕刻", botDesc: "开始雕刻后显示；有刀路时中央叠刀路与进度" },
  { id: "approval", label: "待电脑审批提醒", desc: "举牌挥手，倒计时写在字幕", mossLabel: "太空舱审批警戒门禁", mossDesc: "琥珀边框闪烁，倒计时写在字幕", botLabel: "待电脑审批提醒", botDesc: "举牌挥手，倒计时写在字幕" },
  { id: "music", label: "听歌摇摆", desc: "戴耳机摇摆、音符飘起，停播为止", mossLabel: "太空舱黑胶像素唱机", mossDesc: "唱盘旋转、音符飘起，停播为止", botLabel: "听歌摇摆", botDesc: "戴耳机摇摆、音符飘起，停播为止" },
  { id: "celebrate", label: "完工欢呼", desc: "竖大拇指、彩纸落下，停留约 10 秒", mossLabel: "太空舱完工徽章", mossDesc: "青色光芒旋转，停留约 10 秒", botLabel: "完工欢呼", botDesc: "竖大拇指、彩纸落下，停留约 10 秒" },
  { id: "halted", label: "急停护盾", desc: "戴好护目镜举盾，停留约 10 秒", mossLabel: "太空舱安全急停", mossDesc: "红框警示角闪烁，停留约 10 秒", botLabel: "急停护盾", botDesc: "戴好护目镜举盾，停留约 10 秒" },
];

export const BUILTIN_ASR_CORRECTION_PREVIEW =
  "段木/端木→椴木 · 亚克立→亚克力 · 一宿→翼宿 · 牛皮指→牛皮纸 · 胶合版→胶合板 · 走边筐→走边框";

const PROTECTED_STOP_WORDS = new Set([
  "停", "急停", "停止", "立刻停", "马上停", "快停", "取消", "停止加工", "暂停",
  "退出", "再见", "拜拜", "退下", "关闭对话", "确认", "开始",
]);

const MOTION_GCODES = ["G0", "G00", "G1", "G01", "G2", "G02", "G3", "G03", "G28", "M3", "M03", "M4", "M04", "M5", "M05"];

function containsDangerousGcodeOrGrblJs(text) {
  const upper = String(text || "").toUpperCase();
  for (const token of upper.split(/[^A-Z0-9$=]+/)) {
    if (!token) continue;
    // Whole word (G1) or compact form followed by a parameter letter (G1X10, M3S1000); mirrors the backend list.
    if (MOTION_GCODES.some((code) => token.startsWith(code) && /^(?:$|[A-Z])/.test(token.slice(code.length)))) return true;
    if (/^\$\d+=/.test(token)) return true;
  }
  return false;
}

export function parseAsrCorrectionLine(rawLine) {
  const line = String(rawLine || "");
  if (/[\x00-\x1f\x7f]/.test(line)) return { error: "同音纠错规则不能包含控制字符" };
  const trimmed = line.trim();
  if (!trimmed) return { empty: true };
  const match = trimmed.match(/^(.*?)(?:=|->|→|＝)(.*)$/);
  if (!match) return { error: "每条同音纠错请按“错词=正词”填写（如 段木=椴木）" };
  const wrong = match[1].trim();
  const right = match[2].trim();
  if (!wrong || !right) return { error: "同音纠错的错词与正词都不能为空" };
  if (wrong === right) return { error: "同音纠错的错词与正词不能相同" };
  if (wrong.includes("=") || right.includes("=")) return { error: "单条同音纠错只能包含一个“=”分隔符" };
  if ([...wrong].length > 24 || [...right].length > 24) return { error: "同音纠错单侧最多 24 个字符" };
  if (PROTECTED_STOP_WORDS.has(wrong.toLowerCase()) || PROTECTED_STOP_WORDS.has(wrong)) {
    return { error: `安全保护词“${wrong}”不允许被同音纠错改写` };
  }
  if (containsDangerousGcodeOrGrblJs(right)) {
    return { error: "同音纠错目标词不能包含 G-code 或控制器 $n= 指令" };
  }
  return { wrong, right, formatted: `${wrong}=${right}` };
}

/** 计算单台小智设备的在线快捷控制能力与当前显示值（结合实时快照、会话工具列表与本机记忆偏好）。 */
export function builtinDeviceControlsView(device, session, devicePrefs = {}) {
  let statusObj = null;
  try {
    statusObj = device?.status_json ? JSON.parse(device.status_json) : null;
  } catch {
    statusObj = null;
  }
  let helloObj = null;
  try {
    helloObj = device?.hello_json ? JSON.parse(device.hello_json) : null;
  } catch {
    helloObj = null;
  }
  let listenObj = null;
  let parsedListen = null;
  try {
    parsedListen = device?.listen_json ? JSON.parse(device.listen_json) : null;
  } catch {
    parsedListen = null;
  }
  void listenObj;
  const pref = (devicePrefs && device?.device_id && devicePrefs[device.device_id]) || {};
  const tools = Array.isArray(session?.device_tools) ? session.device_tools : [];
  const hasToolList = tools.length > 0;
  const mcpSupported = Boolean(session) && (hasToolList || helloObj?.mcp !== false);
  const canVolume = hasToolList
    ? tools.includes("self.audio_speaker.set_volume")
    : Boolean(mcpSupported);
  const canBrightness = hasToolList
    ? tools.includes("self.screen.set_brightness") || tools.includes("self.disp.setbacklight")
    : Boolean(statusObj?.screen && statusObj.screen.brightness !== undefined);
  const canTheme = hasToolList
    ? tools.includes("self.screen.set_theme")
    : Boolean(statusObj?.screen && statusObj.screen.theme);
  const canPressToTalk = hasToolList
    ? tools.includes("self.set_press_to_talk")
    : false;
  const screenMonochrome =
    session?.screen_monochrome === true
      ? true
      : session?.screen_monochrome === false
        ? false
        : null;
  const screenWidth = Number(session?.screen_width);
  const screenHeight = Number(session?.screen_height);
  const screenResolution =
    Number.isFinite(screenWidth) && screenWidth > 0 && Number.isFinite(screenHeight) && screenHeight > 0
      ? `${Math.round(screenWidth)}×${Math.round(screenHeight)}`
      : null;
  const canPreviewImage = hasToolList
    ? tools.includes("self.screen.preview_image") && screenMonochrome !== true
    : false;
  const clampPct = (val, fallback) => {
    const num = Number(val);
    if (!Number.isFinite(num)) return fallback;
    return Math.max(0, Math.min(100, Math.round(num)));
  };
  const liveVol = statusObj?.audio_speaker?.volume;
  const liveBri = statusObj?.screen?.brightness;
  const liveTheme = statusObj?.screen?.theme;
  const volume = liveVol !== undefined ? clampPct(liveVol, 60) : clampPct(pref.volume, 60);
  const brightness = liveBri !== undefined ? clampPct(liveBri, 80) : clampPct(pref.brightness, 80);
  const theme = liveTheme === "light" || liveTheme === "dark"
    ? liveTheme
    : pref.theme === "light" || pref.theme === "dark"
      ? pref.theme
      : "dark";
  const talkMode =
    pref.talk_mode === "press_to_talk" || pref.talk_mode === "click_to_talk"
      ? pref.talk_mode
      : parsedListen?.mode === "manual"
        ? "press_to_talk"
        : "click_to_talk";

  return {
    online: Boolean(session),
    mcpSupported,
    canVolume,
    canBrightness,
    canTheme,
    canPressToTalk,
    canPreviewImage,
    screenMonochrome,
    screenResolution,
    volume,
    brightness,
    theme,
    talkMode,
    savedPref: {
      volume: pref.volume ?? null,
      brightness: pref.brightness ?? null,
      theme: pref.theme || "",
      talk_mode: pref.talk_mode || "",
    },
  };
}
/** 内置语音服务的设备状态与可用操作（内置语音技术书 §6：新设备首次连接需在电脑上允许）。 */
export function builtinDeviceView(device) {
  const status = device?.status || "pending";
  const base = DEVICE_STATUS[status] || DEVICE_STATUS.pending;
  return {
    ...base,
    status,
    title: (device?.name || "").trim() || device?.device_id || "未知设备",
    canAllow: status !== "allowed",
    canRevoke: status !== "revoked",
  };
}

/** 允许设备前的确认文字。 */
export function allowDeviceConfirmText(device) {
  const facts = [
    `设备：${builtinDeviceView(device).title}`,
    device?.model ? `型号：${device.model}` : "",
    device?.firmware_version ? `固件：${device.firmware_version}` : "",
  ].filter(Boolean);
  return [
    ...facts,
    "",
    "允许后，这台设备可以通过 MOSS 的内置语音服务对话。请确认是你自己的设备。",
    "允许后需重启设备，让它重新获取授权。",
  ].join("\n");
}

/** 内置语音服务当前（或下次启动）的工作模式（NV5）。 */
export function builtinModeView(status) {
  const chat = status?.mode === "chat";
  const linked = status?.link_mode === "builtin";
  return {
    text: chat ? "对话" : "回声测试",
    tone: chat ? "ok" : "info",
    hint: chat
      ? "已配置识别、大模型与合成：设备说话后由 MOSS 识别、回答并朗读。开始加工前会先复述参数等你口头确认；开启严格模式时还需在电脑上批准。"
      : "识别、大模型、合成三项没有配置齐全：先以回声测试运行（说完稍停即把你的声音播回）。识别、合成也可用本地模型；在“模型”标签配齐后自动切换为对话。",
    autostart: linked
      ? "语音链路为“MOSS 内置服务”：随 MOSS 自动启动。"
      : "语音链路切到“MOSS 内置服务”后会随 MOSS 自动启动；现在也可以手动启动测试。",
  };
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

/** 字节数显示为 MB（保留 1 位小数，小于 1 MB 显示 KB）。 */
export function formatBytes(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024 * 1024) return `${Math.max(1, Math.round(value / 1024))} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

/** 本地模型一行的状态与可用操作（NV6）。 */
export function localModelView(model) {
  const progress = model?.progress;
  if (progress?.state === "downloading") {
    const percent = progress.total > 0 ? Math.min(100, Math.floor((progress.done / progress.total) * 100)) : 0;
    return { text: `下载中 ${percent}%`, tone: "info", percent, canDownload: false, canCancel: true, canDelete: false };
  }
  // 2026-11-07：OneJev 先准备三档共用的推理引擎；多档同时下载时后来的排队等待
  if (progress?.state === "preparing_runtime") {
    const percent = progress.total > 0 ? Math.min(100, Math.floor((progress.done / progress.total) * 100)) : 0;
    return { text: `准备推理引擎 ${percent}%`, tone: "info", percent, canDownload: false, canCancel: true, canDelete: false };
  }
  if (progress?.state === "waiting_runtime") return { text: "等待推理引擎", tone: "info", percent: 0, canDownload: false, canCancel: true, canDelete: false };
  if (progress?.state === "verifying") return { text: "校验中", tone: "info", percent: 100, canDownload: false, canCancel: true, canDelete: false };
  if (progress?.state === "installing") return { text: "安装中", tone: "info", percent: 100, canDownload: false, canCancel: true, canDelete: false };
  if (model?.installed) return { text: "已安装", tone: "ok", percent: null, canDownload: false, canCancel: false, canDelete: true };
  if (progress?.state === "failed") {
    return { text: "下载失败", detail: progress.message || "未知原因", tone: "bad", percent: null, canDownload: true, canCancel: false, canDelete: false };
  }
  if (progress?.state === "cancelled") return { text: "已取消", tone: "off", percent: null, canDownload: true, canCancel: false, canDelete: false };
  return { text: "未下载", tone: "off", percent: null, canDownload: true, canCancel: false, canDelete: false };
}

function formatElapsed(secs) {
  const total = Math.max(0, Math.floor(Number(secs) || 0));
  const m = Math.floor(total / 60);
  const sec = String(total % 60).padStart(2, "0");
  return `${m}:${sec}`;
}

/**
 * Laya 运行环境自动准备的显示（2026-11-07）：`setup` 为 `local.laya_setup`，`env` 为 `local.laya_env`。
 * 返回 null 表示无需显示；`canStart` 时显示“准备运行环境 / 重新准备环境”按钮。
 */
export function layaSetupView(setup, env) {
  const state = setup?.state || "idle";
  const ready = Boolean(env?.ready);
  const retryLabel = ready ? "重新准备环境" : "准备运行环境";
  if (state === "running") {
    const step = setup.steps ? `第 ${setup.step || 0}/${setup.steps} 步 · ` : "";
    return {
      tone: "info",
      text: `正在准备 Laya 运行环境：${step}${setup.stage || ""}（已用时 ${formatElapsed(setup.elapsed_secs)}）`,
      detail: setup.detail || "",
      canStart: false,
      canCancel: true,
      startLabel: retryLabel,
    };
  }
  if (state === "failed") {
    return { tone: "bad", text: `Laya 运行环境准备失败：${setup.message || "未知原因"}`, detail: (setup.log || []).slice(-3).join("\n"), canStart: true, canCancel: false, startLabel: "重试准备环境" };
  }
  if (state === "cancelled") return { tone: "off", text: "已取消准备 Laya 运行环境", detail: "", canStart: true, canCancel: false, startLabel: retryLabel };
  if (state === "done") return { tone: "ok", text: setup.summary || "Laya 运行环境已准备好", detail: "", canStart: ready && env?.accelerator === "cpu", canCancel: false, startLabel: retryLabel };
  if (!ready) return { tone: "warn", text: env?.summary || "尚未准备 Laya 运行环境", detail: "", canStart: true, canCancel: false, startLabel: retryLabel };
  // 已就绪但只能用 CPU（如装好显卡驱动之前准备的）：允许重新准备以换成 CUDA
  return env?.accelerator === "cpu" && env?.managed ? { tone: "ok", text: env.summary, detail: "", canStart: true, canCancel: false, startLabel: retryLabel } : null;
}

// 2026-11-07：本地 Jev（OneJev / Laya）实际运行情况。后台启动失败、推理进程意外退出时说明原因，
// 不再出现“看着开着、实际每轮都被跳过”；云端模型与未下载时不显示。
export function localJevRuntimeView(runtime) {
  if (!runtime || runtime.state === "cloud" || runtime.state === "not_installed") return null;
  const accel = runtime.accelerator ? `（${runtime.accelerator}）` : "";
  const notice = runtime.notice || "";
  switch (runtime.state) {
    case "ready":
      return { tone: notice ? "warn" : "ok", text: `本地 Jev 已就绪${accel}`, detail: notice };
    case "starting":
      return { tone: "info", text: "本地 Jev 正在加载模型，加载完成前语音筛选会先跳过", detail: notice };
    case "failed":
      return { tone: "bad", text: `本地 Jev 启动失败：${runtime.error || "未知原因"}`, detail: notice };
    case "stopped":
      return { tone: "warn", text: "本地 Jev 未在运行：重新保存设置或重启语音服务即可拉起", detail: notice };
    case "idle":
      return notice ? { tone: "off", text: "本地 Jev 未启用（语音筛选与加工反馈都关闭时不占内存）", detail: notice } : null;
    default:
      return null;
  }
}

const LOCAL_BUSY_STATES = new Set(["downloading", "preparing_runtime", "waiting_runtime", "verifying", "installing"]);

/** 删除本地模型前的确认文字：说明会一并删除的附带文件（2026-11-07）。 */
export function localModelDeleteText(model, models, local) {
  const label = model?.label || model?.id || "";
  let extra = "";
  if (model?.id === "laya_multilingual" && local?.laya_env?.managed) {
    extra = "会同时删除 MOSS 自动创建的 Laya Python 运行环境（含 PyTorch，可能有几 GB），下次使用时重新准备。";
  } else if (model?.kind === "jev" && model?.id !== "laya_multilingual") {
    const otherOneJev = (Array.isArray(models) ? models : []).some(
      (item) => item.kind === "jev" && item.id !== "laya_multilingual" && item.id !== model.id && (item.installed || LOCAL_BUSY_STATES.has(item.progress?.state)),
    );
    if (!otherOneJev) extra = "这是最后一档 OneJev，会同时删除 MOSS 下载的推理引擎 llama-server。";
  }
  return `删除本地模型“${label}”？${extra}需要时可以重新下载。`;
}

/** 本地模型的三个类别与默认模型（开关与选用统一由顶部四栏下拉框控制）。 */
export const LOCAL_KINDS = [
  { kind: "vad", title: "拾音判停", fallback: "silero_vad" },
  { kind: "asr", title: "语音识别", fallback: "sense_voice" },
  { kind: "tts", title: "语音合成", fallback: "kokoro" },
  { kind: "vision", title: "视觉理解", fallback: "qwen3_vl_2b" },
];

/** 按类别分组的本地模型（保持后端清单顺序）。 */
export function localModelGroups(models) {
  const list = Array.isArray(models) ? models : [];
  return LOCAL_KINDS.map((item) => ({ ...item, models: list.filter((model) => model.kind === item.kind) }));
}

/** 本地模型设置表单（数字以字符串保存，便于输入）。 */
export function localSettingsForm(settings) {
  const value = settings || {};
  return {
    asr: Boolean(value.asr),
    barge_in: Boolean(value.barge_in),
    barge_in_ms: String(value.barge_in_ms ?? 400),
    read_only_shortcuts: value.read_only_shortcuts !== false,
    asr_beam_search: value.asr_beam_search !== false,
    job_done_announce: value.job_done_announce !== false,
    llm_fast_reply: value.llm_fast_reply !== false,
    jev_filter_enabled: Boolean(value.jev_filter_enabled),
    jev_model: value.jev_model || "typesafe/jev-1.14",
    jev_unfinished_hold: value.jev_unfinished_hold !== false,
    jev_fast_route: value.jev_fast_route !== false,
    jev_tool_prune: value.jev_tool_prune !== false,
    jev_memory_filter: value.jev_memory_filter !== false,
    custom_hotwords: (Array.isArray(value.custom_hotwords) ? value.custom_hotwords : []).join("\n"),
    asr_homophone_fix: value.asr_homophone_fix !== false,
    custom_asr_corrections: (Array.isArray(value.custom_asr_corrections) ? value.custom_asr_corrections : []).join("\n"),
    persona_style: canonicalPersonaStyle(value.persona_style || "workshop"),
    persona_risk_notes: value.persona_risk_notes === true,
    persona_prompt: value.persona_prompt || "",
    custom_personas: JSON.stringify(parseCustomPersonas(value.custom_personas)),
    screen_preview_sync: value.screen_preview_sync !== false,
    screen_card_style: value.screen_card_style === "goggle_bot" ? "goggle_bot" : "moss_hud",
    tts: Boolean(value.tts),
    vad: Boolean(value.vad),
    vad_model: value.vad_model || "silero_vad",
    asr_model: value.asr_model || "sense_voice",
    tts_model: value.tts_model || "kokoro",
    tts_speaker: String(value.tts_speaker ?? 50),
    tts_speed: String(value.tts_speed ?? 1),
    mirror: value.mirror || "",
  };
}

/** 改选某类模型；改选多音色合成模型时音色编号换成该模型的默认音色。 */
export function selectLocalModel(form, models, kind, id) {
  const next = { ...form, [`${kind}_model`]: id };
  const model = (Array.isArray(models) ? models : []).find((item) => item.id === id);
  if (kind === "tts" && id !== form.tts_model && model?.voices) next.tts_speaker = String(model.voices.default);
  return next;
}

/** 当前选中的合成模型的音色范围（单音色模型或未知模型为空）。 */
export function selectedVoices(form, models) {
  const model = (Array.isArray(models) ? models : []).find((item) => item.id === form.tts_model);
  return model?.voices || null;
}

/** 云端 TTS 启用时（`!form.tts`），返回当前作为云端异常优先兜底的已安装本地 TTS 模型；未安装任何本地 TTS 或已启用本地主 TTS 时返回 `null`。 */
export function resolveFallbackTtsModel(form, models = []) {
  if (!form || Boolean(form.tts)) return null;
  const ttsModels = (Array.isArray(models) ? models : []).filter(
    (item) => (!item.kind || item.kind === "tts") && item.installed
  );
  if (ttsModels.length === 0) return null;
  return ttsModels.find((item) => item.id === form.tts_model) || ttsModels[0] || null;
}

/** 前端先做的检查，按保存分组拆开；返回错误文字或空串。语音体验分组（插话、热词）： */
export function experienceSettingsError(form) {
  const bargeMs = Number(form.barge_in_ms ?? 400);
  if (!Number.isInteger(bargeMs) || bargeMs < 300 || bargeMs > 600) return "插话时长须为 300 ~ 600 毫秒的整数";
  const words = String(form.custom_hotwords || "").split(/\r?\n/);
  if (words.some((word) => /[\x00-\x1f\x7f]/.test(word))) return "热词不能包含控制字符";
  const effective = [...new Set(words.map((word) => word.trim()).filter(Boolean))];
  if (effective.length > 64) return "自定义热词最多 64 条";
  if (effective.some((word) => [...word].length > 32)) return "每条热词最多 32 个字符";
  if (new TextEncoder().encode(effective.join("")).length > 4096) return "热词总长度超过 4096 字节";
  const customError = customPersonasError(form.custom_personas);
  if (customError) return customError;
  const style = String(form.persona_style || "workshop").trim();
  if (!personaOptions(form.custom_personas).some((opt) => opt.id === style)) return "请选择有效的说话角色";
  const cardStyle = String(form.screen_card_style || "moss_hud").trim();
  if (!["moss_hud", "goggle_bot"].includes(cardStyle)) return "请选择有效的屏幕卡片画风";
  const prompt = String(form.persona_prompt || "").replace(/\r\n?/g, "\n");
  if (/[\x00-\x09\x0b-\x1f\x7f]/.test(prompt)) return "补充说明不能包含控制字符";
  if ([...prompt.trim()].length > 300) return "补充说明最多 300 个字符";
  if (containsDangerousGcodeOrGrblJs(prompt)) return "补充说明不能包含 G-code 或控制器 $n= 指令";
  const corrLines = String(form.custom_asr_corrections || "").split(/\r?\n/);
  const seenWrong = new Map();
  for (const line of corrLines) {
    const parsed = parseAsrCorrectionLine(line);
    if (parsed.empty) continue;
    if (parsed.error) return parsed.error;
    seenWrong.set(parsed.wrong, parsed.formatted);
  }
  if (seenWrong.size > 64) return "自定义同音纠错最多 64 条";
  if (new TextEncoder().encode([...seenWrong.values()].join("")).length > 4096) return "同音纠错总长度超过 4096 字节";
  return "";
}

/** 本地模型分组（音色、语速、下载地址）。`models` 为状态里的模型列表，用于按所选合成模型检查音色编号。 */
export function localModelSettingsError(form, models = []) {
  const voices = selectedVoices(form, models);
  const max = voices ? voices.max : 999;
  const speaker = Number(form.tts_speaker);
  if (!Number.isInteger(speaker) || speaker < 0 || speaker > max) return `音色编号须为 0 ~ ${max} 的整数`;
  const speed = Number(form.tts_speed);
  if (!(speed >= 0.5 && speed <= 2)) return "本地合成语速须在 0.5 ~ 2.0 之间";
  const mirror = String(form.mirror || "").trim();
  if (mirror && (!/^https?:\/\//i.test(mirror) || /\s/.test(mirror))) return "下载地址必须以 http:// 或 https:// 开头";
  return "";
}

/** 两组合起来的检查。 */
export function localSettingsError(form, models = []) {
  return experienceSettingsError(form) || localModelSettingsError(form, models);
}

export function localSettingsPayload(form) {
  return {
    asr: Boolean(form.asr),
    barge_in: Boolean(form.barge_in),
    barge_in_ms: Number(form.barge_in_ms ?? 400),
    read_only_shortcuts: form.read_only_shortcuts !== false,
    asr_beam_search: form.asr_beam_search !== false,
    job_done_announce: form.job_done_announce !== false,
    llm_fast_reply: form.llm_fast_reply !== false,
    jev_filter_enabled: Boolean(form.jev_filter_enabled),
    jev_model: String(form.jev_model || "typesafe/jev-1.14").trim() || "typesafe/jev-1.14",
    jev_unfinished_hold: form.jev_unfinished_hold !== false,
    jev_fast_route: form.jev_fast_route !== false,
    jev_tool_prune: form.jev_tool_prune !== false,
    jev_memory_filter: form.jev_memory_filter !== false,
    custom_hotwords: [...new Set(String(form.custom_hotwords || "").split(/\r?\n/).map((word) => word.trim()).filter(Boolean))],
    asr_homophone_fix: form.asr_homophone_fix !== false,
    custom_asr_corrections: (() => {
      const map = new Map();
      for (const line of String(form.custom_asr_corrections || "").split(/\r?\n/)) {
        const parsed = parseAsrCorrectionLine(line);
        if (!parsed.empty && !parsed.error) map.set(parsed.wrong, parsed.formatted);
      }
      return [...map.values()];
    })(),
    persona_style: canonicalPersonaStyle(String(form.persona_style || "workshop").trim() || "workshop"),
    persona_risk_notes: form.persona_risk_notes === true,
    persona_prompt: String(form.persona_prompt || "").replace(/\r\n?/g, "\n").trim(),
    custom_personas: parseCustomPersonas(form.custom_personas).map((role) => ({
      id: role.id,
      name: role.name.trim(),
      prompt: role.prompt.replace(/\r\n?/g, "\n").trim(),
    })),
    screen_preview_sync: form.screen_preview_sync !== false,
    screen_card_style: form.screen_card_style === "goggle_bot" ? "goggle_bot" : "moss_hud",
    tts: Boolean(form.tts),
    vad: Boolean(form.vad),
    vad_model: form.vad_model,
    asr_model: form.asr_model,
    tts_model: form.tts_model,
    tts_speaker: Number(form.tts_speaker),
    tts_speed: Number(form.tts_speed),
    mirror: String(form.mirror || "").trim(),
  };
}

export const VOICE_TIMING_STAGES = {
  barge_in: "自然插话确认",
  recording_start: "开始拾音", vad_start: "VAD 确认开始", vad_end: "VAD 判停", utterance_end: "说完 / 松开",
  interim_stop: "中间结果命中停止", final_asr: "最终识别", jev_filter: "Jev 筛选分流", fast_route: "快捷路由",
  first_llm_delta: "模型首字", first_sentence: "第一句切出", first_synthesis_start: "首次合成开始",
  first_synthesis_ready: "首段音频就绪", first_audio_frame: "首帧发给设备", turn_end: "本轮结束",
};

/** 复制只取诊断字段；不复制完整服务状态、对话或模型地址。
 * n68：带会话号、设备编号与结束时间；后端新增的阶段也照抄（名称为原始键），只要值是有限数字。 */
export function voiceTimingReport(timings) {
  return (Array.isArray(timings) ? timings : []).slice(0, 50).map((item) => {
    const stages = item?.stages && typeof item.stages === "object" ? item.stages : {};
    const known = Object.keys(VOICE_TIMING_STAGES).filter((key) => Number.isFinite(stages[key]));
    const extra = Object.keys(stages).filter((key) => !VOICE_TIMING_STAGES[key] && Number.isFinite(stages[key])).sort();
    return {
      session_id: String(item?.session_id ?? ""),
      device_id: String(item?.device_id ?? ""),
      at_ms: Number.isFinite(item?.at_ms) ? item.at_ms : null,
      turn: item?.turn, outcome: item?.outcome,
      models: (Array.isArray(item?.models) ? item.models : []).map(String),
      stages: Object.fromEntries([...known, ...extra].map((key) => [key, stages[key]])),
      tools: (Array.isArray(item?.tools) ? item.tools : []).slice(0, 50).map(({ name, ms }) => ({ name, ms })),
    };
  });
}

// ---------- NV8 接入点与工具 ----------

const ENDPOINT_LABELS = { primary: "主接入点" };

/** 接入点编号的中文名：primary → 主接入点，extra-2 → 附加接入点 2。 */
export function endpointLabel(id) {
  if (ENDPOINT_LABELS[id]) return ENDPOINT_LABELS[id];
  const match = /^extra-(\d+)$/.exec(String(id || ""));
  return match ? `附加接入点 ${match[1]}` : String(id || "接入点");
}

/** 一个接入点的状态显示：`{tone, text, detail}`。 */
export function endpointView(endpoint, extrasPaused = false) {
  if (!endpoint) return { tone: "off", text: "未配置", detail: "" };
  if (!endpoint.active) {
    return { tone: "off", text: "已暂停", detail: extrasPaused ? "自建模式下附加接入点不连接，切回官方或内置时恢复" : "" };
  }
  const reconnects = Math.max(0, Number(endpoint.connections || 0) - 1);
  if (endpoint.connected) {
    const parts = [];
    if (endpoint.client) parts.push(`对方：${endpoint.client}`);
    if (reconnects > 0) parts.push(`已自动重连 ${reconnects} 次`);
    return { tone: "ok", text: "已连接", detail: parts.join(" · ") };
  }
  if (endpoint.last_error) return { tone: "bad", text: "重连中", detail: `上次断开：${endpoint.last_error}` };
  return { tone: "warn", text: "连接中", detail: "" };
}

/** 接入点 token 里的身份：`{text, expired}`；不是 JWT 时 text 为空。 */
export function identityView(identity, nowSeconds = Date.now() / 1000) {
  if (!identity || typeof identity !== "object") return { text: "", expired: false };
  const fields = identity.fields && typeof identity.fields === "object" ? identity.fields : {};
  const parts = Object.entries(fields).map(([key, value]) => `${key} ${value}`);
  const exp = Number(identity.expires_at);
  let expired = false;
  if (Number.isFinite(exp) && exp > 0) {
    expired = exp < nowSeconds;
    const date = new Date(exp * 1000).toLocaleDateString("zh-CN");
    parts.push(expired ? `已于 ${date} 过期` : `有效期至 ${date}`);
  }
  return { text: parts.join(" · "), expired };
}

/** 切换一个工具后的“关闭名单”（排序、去重）；不能关闭的工具不会进入名单。 */
export function toolSwitchPayload(tools, name, enabled) {
  const list = Array.isArray(tools) ? tools : [];
  const disabled = new Set(list.filter((tool) => tool && tool.enabled === false && tool.can_disable !== false).map((tool) => tool.name));
  const target = list.find((tool) => tool?.name === name);
  if (target && target.can_disable !== false) {
    if (enabled) disabled.delete(name);
    else disabled.add(name);
  }
  return [...disabled].sort();
}

/**
 * n72（2026-11-08 用户决定）：内置工具卡片给人看的名字、分组和风险标签。
 * `risk`：read 只读 / file 只生成预览或文件 / confirm 会让机器动、要你确认 / stop 停止与调速（不用确认）/ desktop 控制电脑。
 * 精简清单（默认给 AI）以后端 `in_compact` 为准；这里只管显示。
 */
export const TOOL_GROUPS = [
  { id: "safety", title: "检查与安全" },
  { id: "job", title: "刻字与发送" },
  { id: "tuning", title: "材料与调参" },
  { id: "advanced", title: "高级：重复入口与底层收发（只在“完整工具”打开时给 AI）" },
  { id: "desktop", title: "电脑控制（内置语音和严格模式下不开放）" },
  { id: "other", title: "其他" },
];

export const TOOL_RISK_LABELS = {
  read: "只读",
  file: "只生成预览",
  stop: "停止与调速",
  confirm: "会让机器动 · 要你确认",
  desktop: "控制电脑",
};

export const TOOL_META = {
  check_laser_connection_tool: { label: "检查机器连接", group: "safety", risk: "read", summary: "看激光机连没连上、现在空闲还是在刻。" },
  laser_safe_action_tool: { label: "停止与机器控制", group: "safety", risk: "confirm", summary: "急停、边刻边调速度和功率不用确认；回零、设原点、移动激光头要你确认。不能关闭。" },
  laser_workflow_tool: { label: "刻字：预览、发送、进度", group: "job", risk: "confirm", summary: "先生成预览弹到电脑上，你核对并确认后才开始刻；也能查进度、取消、断点续刻。" },
  regenerate_text_laser_task_tool: { label: "按建议重新生成预览", group: "job", risk: "file", summary: "采用调参建议或改文字、材料后重新出预览；发送仍要你确认。" },
  material_params_tool: { label: "材料参数库", group: "tuning", risk: "read", summary: "查某种材料该用多大功率、多快速度；保存和删除只能在电脑上做。" },
  refine_laser_params_from_feedback_tool: { label: "按效果给调参建议", group: "tuning", risk: "file", summary: "你说“太焦”“太浅”后给出调整建议，不改参数、不动机器。" },
  run_calibration_grid_tool: { label: "测试矩阵", group: "tuning", risk: "confirm", summary: "刻一块不同功率×速度的小方格找最佳参数；开始刻前要你确认。" },
  select_calibration_cell_tool: { label: "保存最佳格子", group: "tuning", risk: "file", summary: "你说第几格最好，就把它的参数存进材料库。" },
  recommend_laser_params_tool: { label: "推荐参数（旧入口）", group: "advanced", risk: "read", summary: "和“材料参数库”的查询重复。" },
  start_tuned_job_tool: { label: "按调参结果开刻（旧入口）", group: "advanced", risk: "confirm", summary: "和“刻字”的确认发送重复。" },
  generate_text_laser_task_tool: { label: "生成刻字任务（旧入口）", group: "advanced", risk: "file", summary: "和“刻字”的预览重复。" },
  generate_text_image_gcode_tool: { label: "文字转刀路文件", group: "advanced", risk: "file", summary: "只生成图片和刀路文件。" },
  ai_laser_gcode_tool: { label: "素材转刀路（旧入口）", group: "advanced", risk: "file", summary: "和“刻字”的预览重复。" },
  generate_text_image_tool: { label: "生成文字图片", group: "advanced", risk: "file", summary: "只生成白底黑字图片。" },
  laser_grbl_tool: { label: "串口底层收发", group: "advanced", risk: "confirm", summary: "直接对串口查状态、取消；发任意 G-code 已禁用。" },
  laser_network_grbl_tool: { label: "网络底层收发", group: "advanced", risk: "confirm", summary: "直接对网络雕刻机查状态、取消；发任意 G-code 已禁用。" },
  open_app_tool: { label: "打开电脑应用", group: "desktop", risk: "desktop", summary: "打开浏览器、微信等应用或网址。" },
  shortcut_key_execution_tool: { label: "电脑快捷键", group: "desktop", risk: "desktop", summary: "播放、暂停音乐，调电脑音量。" },
  open_jsjds_website_tool: { label: "打开计算机设计大赛官网", group: "desktop", risk: "desktop", summary: "打开大赛官网。" },
  command_execution_tool: { label: "执行终端命令（开发者）", group: "desktop", risk: "desktop", summary: "只在开发者模式出现。" },
};

/** 工具按分组排好，未登记的工具放进“其他”，用后端给的说明。 */
export function groupedTools(tools) {
  const list = Array.isArray(tools) ? tools : [];
  return TOOL_GROUPS.map((group) => ({
    ...group,
    tools: list
      .map((tool) => {
        const meta = TOOL_META[tool.name];
        return {
          ...tool,
          label: meta?.label || tool.name,
          group: meta?.group || "other",
          risk: meta?.risk || "",
          summary: meta?.summary || tool.description || "",
        };
      })
      .filter((tool) => tool.group === group.id),
  })).filter((group) => group.tools.length > 0);
}

/** 工具调用统计的简短文字。 */
export function toolStatText(tool) {
  const calls = Number(tool?.calls || 0);
  if (!calls) return "本次运行未调用";
  const parts = [`调用 ${calls} 次`];
  if (tool.failures) parts.push(`失败 ${tool.failures}`);
  if (tool.denied) parts.push(`拒绝 ${tool.denied}`);
  return parts.join(" · ");
}

/**
 * 内置工具（编译在 MOSS 里的原生工具）不能在界面上增删，只能开关；
 * 这两段是交给 AI 编码助手的施工说明，用户复制后粘给 AI 由它改代码（2026-11-02 用户决定）。
 * 增删第三方外部工具走「扩展 → 第三方 MCP 工具」，与这里无关。
 */
export const TOOL_EDIT_PROMPTS = {
  add: {
    title: "让 AI 新增一个内置工具",
    text: [
      "请在 MOSS 仓库里新增一个内置工具：<工具名>，用途是 <一句话说明它做什么>。",
      "",
      "按下面顺序改，只动相关文件，不要顺手重构：",
      "1. 登记：crates/laser-tools/src/lib.rs 的 PRIORITY_TOOLS / SECONDARY_TOOLS 名单里加入新工具名（snake_case、以 _tool 结尾，不含双下划线 __，与现有工具一致）；ToolCatalog 会自动把它收进名单。",
      "2. 声明：tests/golden/mcp_tools.json（被 crates/laser-mcp/src/lib.rs 的 tools_list_json / tools_list_with 编译期加载）增加它的 name、description 与 inputSchema（JSON Schema，必填项与取值范围写清楚）。默认给 AI 的是精简清单：若这个工具也要默认提供，同时把面向用户意图写的短说明加进 tests/golden/mcp_tools_compact.json、名字加进 laser_mcp::COMPACT_LASER_TOOLS 与 laser_api::laser_mcp_compact_tool，并在 frontend/src/voiceState.js 的 TOOL_META 里写中文名、分组与风险标签。",
      "3. 分发与分级：crates/laser-mcp/src/lib.rs 的 dispatch_tool_call_with 增加同名 match 分支（非法参数返回结构化错误，不 panic），并在 crates/laser-contracts/src/risk.rs 的 tool_risk 显式标定 R0–R4 风险等级。",
      "4. 实现：复用现有的连接解析 resolve_connection_spec、发送门禁 authorize_send 与错误处理，不新增抽象层。",
      "5. 门禁：涉及运动、出光、发送 G-code、急停、删除、OTA、密钥的动作必须走现有审批与严格模式门禁，不得放宽；只读工具不得有副作用；急停类工具必须 can_disable=false。",
      "6. Jev 粗筛归组：在 crates/moss-voice-server/src/jev.rs 的 groups_of_builtin_tool 将新工具归入现有 6 个粗粒度 ToolGroup（LaserQuery / LaserGenerate / LaserControl / InfoPlugin / HomeAndAgent / MusicAndDevice，跨类激光工具可挂多组）；严禁为单个新工具平铺独立的 noul 问题。",
      "7. 测试：在 crates/laser-mcp、crates/laser-tools 与 crates/moss-voice-server 增加单元测试（参数校验、关闭后不可见、Jev 归组、失败路径），硬件与网络副作用一律用 mock 或 FakeGrbl。",
      "8. 记录：同步 docs/developer/api-contracts.md 与对应技术书，在 CHANGELOG.md 的 [Unreleased] 加一条；跑 cargo test --workspace。",
      "",
      "完成后回报：新工具名、用途、风险等级与所属粗筛组、是否需要审批、改了哪些文件、跑了哪些测试及结果。",
    ].join("\n"),
  },
  remove: {
    title: "让 AI 删除或下线一个内置工具",
    text: [
      "请把 MOSS 内置工具里的 <工具名> 下线或删除。",
      "",
      "先判断用哪种做法：",
      "1. 只是不想让模型用到它：不用改代码。在「扩展 → MOSS 内置工具」里取消勾选即可，关闭的工具不会出现在任何接入点与内置语音的工具列表中。",
      "2. 要彻底删掉代码：按下面顺序执行。",
      "",
      "彻底删除的顺序：",
      "1. 从 crates/laser-tools/src/lib.rs 的 PRIORITY_TOOLS / SECONDARY_TOOLS 名单移除。",
      "2. 从 tests/golden/mcp_tools.json、tests/golden/mcp_tools_compact.json（若在精简清单里，同时从 COMPACT_LASER_TOOLS 与 laser_api::laser_mcp_compact_tool 去掉）、crates/laser-mcp/src/lib.rs（tools_list_with / dispatch_tool_call_with）及 crates/laser-contracts/src/risk.rs（tool_risk）移除对应 schema、分发与分级分支，并删掉 frontend/src/voiceState.js 的 TOOL_META 条目。",
      "3. 删除该工具的实现、只属于它的测试，并从 crates/moss-voice-server/src/jev.rs 的 groups_of_builtin_tool 清理该工具名（Jev 按当轮启用工具动态提问，删空某组也无需改 ToolGroup 枚举）。",
      "4. 在 crates/、frontend/、docs/、scripts/ 全局搜索残留引用并逐条清理，把清单告诉我。",
      "",
      "不要动：laser_safe_action_tool（急停与停机）、严格模式、审批与 ACL 门禁；crates/laser-host/src/lib.rs 的 validate_disabled_tools 中不可关闭的约束必须保留。",
      "",
      "完成后：跑 cargo test --workspace 与前端 npm test，同步 docs/developer/api-contracts.md 与 CHANGELOG.md，并回报实际做法、删除清单、残留引用检查结果与测试结果。",
    ].join("\n"),
  },
};

/** 取指定模板的纯文本；类型未知时返回空字符串。 */
export function toolEditPrompt(kind) {
  return TOOL_EDIT_PROMPTS[kind]?.text || "";
}

/** 新增附加接入点前的校验；通过时返回空字符串。 */
export function extraEndpointError(url, endpoints = [], max = 4) {
  const value = String(url || "").trim();
  if (!value) return "请填写接入点地址";
  if (!/^wss?:\/\//.test(value) || value.includes("<") || /\s/.test(value)) return "接入点必须以 ws:// 或 wss:// 开头，且不含占位符";
  const extras = (Array.isArray(endpoints) ? endpoints : []).filter((endpoint) => String(endpoint?.id || "").startsWith("extra-"));
  if (extras.length >= max) return `附加接入点最多 ${max} 个`;
  return "";
}

// ---------- 语音中心分组与保存（UI-3，技术书 §9.5.1 r15） ----------

/**
 * 页面标签。**当前链路用不到的部分不渲染**（2026-10-18 用户决定）；切标签不会丢失未保存的填写。
 * n69（2026-11-07 用户决定“完整整理”）：“唤醒词与音量”并入“设备”，“设备与工具”改名“设备”，内置工具移到“扩展”。
 */
export const VOICE_TABS = [
  { key: "service", label: "服务" },
  { key: "devices", label: "设备" },
  { key: "models", label: "模型" },
  { key: "features", label: "扩展" },
  { key: "privacy", label: "隐私与家居" },
  { key: "records", label: "记录与诊断" },
];

/** 已取消的旧标签 → 新位置（记住的标签或跳转目标仍指向旧标签时使用）。 */
const LEGACY_VOICE_TABS = { wake: "devices" };

/** 把记住的 / 外部传来的标签 key 换成现有标签；不认识的返回空串。 */
export function normalizeVoiceTab(key) {
  const value = LEGACY_VOICE_TABS[key] || key;
  return VOICE_TABS.some((item) => item.key === value) ? value : "";
}

/**
 * 当前链路要显示的标签（2026-10-18 / 2026-11-02 用户决定）：
 * - 官方链路的模型在小智控制台配置，不显示“模型”标签；
 * - “隐私与家居”四项（对话记忆、信息服务、声纹、家居）均挂在 MOSS 内置语音会话上，只在 `builtin` 链路显示。
 */
export function voiceTabsFor(mode) {
  return VOICE_TABS.filter((item) => {
    if (item.key === "models" && mode === "official") return false;
    if (item.key === "privacy" && mode !== "builtin") return false;
    return true;
  });
}

export const VOICE_MODE_LABEL = { official: "官方小智", self_hosted: "自建服务端", builtin: "MOSS 内置服务" };

/**
 * 链路与服务端设置拆成独立保存的分组：每组只提交自己的字段（后端按字段合并，见 center.rs save_settings）。
 * `secret` 字段带令牌，不回填；填了才算改动，留空表示不改。
 */
export const SETTINGS_GROUPS = {
  official: { mode: "official", title: "官方接入点", keys: ["official_mcp_endpoint"], secret: ["official_mcp_endpoint"] },
  selfHostedServer: { mode: "self_hosted", title: "自建服务端程序", keys: ["server_dir", "python", "args", "server_port"], secret: [], advanced: ["args"] },
  selfHostedEndpoint: {
    mode: "self_hosted",
    title: "自建接入点",
    keys: ["self_hosted_mcp_endpoint", "endpoint_health_url"],
    secret: ["self_hosted_mcp_endpoint", "endpoint_health_url"],
  },
  builtinPorts: { mode: "builtin", title: "内置服务端口", keys: ["builtin_ws_port", "builtin_http_port"], secret: [] },
  // n71（2026-11-08）：常连模式（MQTT + 加密 UDP），默认关
  builtinMqtt: { mode: "builtin", title: "常连模式（实验）", keys: ["builtin_mqtt_enabled", "builtin_mqtt_port", "builtin_udp_port"], secret: [] },
};

const PORT_KEYS = new Set(["server_port", "builtin_ws_port", "builtin_http_port", "builtin_mqtt_port", "builtin_udp_port"]);
const BOOL_KEYS = new Set(["builtin_mqtt_enabled"]);

/** 表单值与已保存值逐项比较（都按去首尾空白的字符串比较）；`keys` 缺省为两边字段的并集。 */
export function formDirty(baseline, form, keys) {
  if (!baseline || !form) return false;
  return (keys || [...new Set([...Object.keys(baseline), ...Object.keys(form)])]).some((key) => String(form[key] ?? "").trim() !== String(baseline[key] ?? "").trim());
}

/**
 * 设置分组保存后按钮旁的生效说明。`reconnected` 取自后端返回（center.rs save_settings），
 * 只在真的重新发起了连接时才说“正在重新连接”；是否连上看页面顶部的连接状态。
 */
export function settingsSaveEffect(group, mode, reconnected) {
  if (group === "selfHostedServer") return "下次启动服务端时生效";
  if (group === "builtinPorts") return "重启内置服务后生效";
  if (group === "builtinMqtt") return "重启内置服务后生效；小智要再重启一次才会换连接方式";
  if (reconnected) return group === "official" ? "正在重新连接小智" : "正在重新连接自建接入点";
  if (group === "official") return mode === "self_hosted" ? "切回官方链路时使用" : "接入点未变，没有重新连接";
  return mode === "self_hosted" ? "接入点未变，没有重新连接" : "切到自建链路时使用";
}

/** 某个设置分组是否有未保存的改动。 */
export function settingsGroupDirty(group, form, baseline) {
  const spec = SETTINGS_GROUPS[group];
  if (!spec || !form) return false;
  const plain = spec.keys.filter((key) => !spec.secret.includes(key));
  return spec.secret.some((key) => String(form[key] || "").trim() !== "") || formDirty(baseline, form, plain);
}

/** 某个设置分组的保存内容：只含本组字段，端口转为数字，带令牌的字段留空时不提交。 */
export function settingsGroupPayload(group, form) {
  const spec = SETTINGS_GROUPS[group];
  const payload = {};
  for (const key of spec.keys) {
    const value = String(form[key] ?? "").trim();
    if (spec.secret.includes(key)) {
      if (value) payload[key] = value;
    } else if (BOOL_KEYS.has(key)) {
      payload[key] = value === "true";
    } else if (PORT_KEYS.has(key)) {
      const port = Number(value);
      payload[key] = Number.isFinite(port) ? port : 0;
    } else {
      payload[key] = value;
    }
  }
  return payload;
}

/**
 * 本地模型设置拆成两组分别保存：模型选择与对话体验。
 * n69（2026-11-07）：`screen_card_style` 只在“设备 › 副屏预览”点选即存，`jev_filter_enabled` 只由“模型”标签顶部第 3 栏切换，
 * 两者都不再属于对话体验组的草稿与保存载荷（避免两处编辑同一个键）。
 */
export const LOCAL_MODEL_KEYS = ["vad", "vad_model", "asr", "asr_model", "tts", "tts_model", "tts_speaker", "tts_speed", "mirror"];
export const EXPERIENCE_KEYS = [
  "read_only_shortcuts",
  "asr_beam_search",
  "job_done_announce",
  "llm_fast_reply",
  "screen_preview_sync",
  "persona_style",
  "persona_prompt",
  "custom_personas",
  "persona_risk_notes",
  "asr_homophone_fix",
  "custom_asr_corrections",
  "jev_unfinished_hold",
  "jev_fast_route",
  "jev_tool_prune",
  "jev_memory_filter",
  "barge_in",
  "barge_in_ms",
  "custom_hotwords",
];

/** 只取草稿中本组字段，其余用已保存的值，避免保存一组时顺带提交另一组未保存的改动。 */
export function mergeGroup(baseline, draft, keys) {
  const next = { ...baseline };
  for (const key of keys) next[key] = draft[key];
  return next;
}

/**
 * 外部渠道（如顶部 5 栏下拉框或 Jev 模型卡片）保存后刷新 baseline 时：
 * 仅保留用户在当前分组草稿中相对上一次 baseline 真实手动改动过的字段，其余未改动字段自动跟随新 baseline。
 */
export function syncLocalDraft(prevBaseline, nextBaseline, currentDraft, keys) {
  const prev = prevBaseline || nextBaseline;
  const next = { ...nextBaseline };
  for (const key of keys) {
    if (currentDraft && currentDraft[key] !== prev[key]) {
      next[key] = currentDraft[key];
    }
  }
  return next;
}

/** 仅提取本分组相对当前已保存 baseline 真正改动的键值作为局部保存载荷，防止跨组旧草稿覆盖外部刚切换的开关。 */
export function localGroupPatch(baseline, draft, keys) {
  const basePayload = localSettingsPayload(baseline);
  const mergedPayload = localSettingsPayload(mergeGroup(baseline, draft, keys));
  const patch = {};
  for (const key of keys) {
    if (JSON.stringify(mergedPayload[key]) !== JSON.stringify(basePayload[key])) {
      patch[key] = mergedPayload[key];
    }
  }
  return Object.keys(patch).length > 0 ? patch : Object.fromEntries(keys.map((k) => [k, mergedPayload[k]]));
}

function clock(at) {
  return at.toLocaleTimeString("zh-CN", { hour12: false });
}

/** 保存成功后按钮旁的一行结果：保存了什么、几点、什么时候生效。 */
export function savedNoteText(target, effect, at = new Date()) {
  return [`已保存${target}`, clock(at), effect].filter(Boolean).join(" · ");
}

/** 有未保存改动的分组所在的标签（显示标签上的提示点）。`dirty` 为 {分组 id: 标签 key}。 */
export function dirtyTabs(dirty) {
  return new Set(Object.values(dirty || {}).filter(Boolean));
}

/** 内置服务模型分组折叠时的一行摘要。 */
export function builtinModelSummary(section, entry, localOn = false) {
  if (localOn && section !== "llm") return entry?.configured ? "使用本地模型（云端设置作备用）" : "使用本地模型";
  if (!entry?.configured) return "未配置";
  const kind = entry.kind === "doubao" ? "豆包流式" : "";
  return ["已配置", kind, entry.model].filter(Boolean).join(" · ");
}
export const DEFAULT_JEV_MODEL = "typesafe/jev-1.14";
export const LOCAL_LAYA_JEV_MODEL = "laya_multilingual";
export const LOCAL_ONEJEV_0_8B_MODEL = "onejev_1_6_0_8b";
export const LOCAL_ONEJEV_4B_MODEL = "onejev_1_6_4b";
export const LOCAL_ONEJEV_9B_MODEL = "onejev_1_6_9b";
export const LOCAL_JEV_FALLBACK_MODELS = [
  { id: LOCAL_ONEJEV_0_8B_MODEL, label: "OneJev 1.6（0.8B 极速图文，Q8_0）" },
  { id: LOCAL_ONEJEV_4B_MODEL, label: "OneJev 1.6（4B 均衡图文，Q4_K_M）" },
  { id: LOCAL_ONEJEV_9B_MODEL, label: "OneJev 1.6（9B 旗舰图文，Q4_K_M）" },
  { id: LOCAL_LAYA_JEV_MODEL, label: "Laya 多语言决策（322M，中英 100+ 语言）" },
];
export const DEFAULT_JEV_DECISIONS_URL = "https://openrouter.ai/api/alpha/decisions";
export const JEV_PRESET_MODELS = [
  { id: "typesafe/jev-1.14", hint: "推荐 · 语音筛选与决策" },
  { id: "typesafe/jev-1.13", hint: "经典版 · 加工反馈" },
];

export function canonicalLocalJevModel(rawModel) {
  const model = String(rawModel || "").trim();
  if (!model) return "";
  const lower = model.toLowerCase();
  if (
    lower === "laya_multilingual" ||
    lower === "laya/multilingual" ||
    lower === "convaiinnovations/laya-multilingual" ||
    lower === "local/laya-multilingual"
  ) {
    return LOCAL_LAYA_JEV_MODEL;
  }
  if (
    lower === "onejev_1_6_0_8b" ||
    lower === "omnijev/onejev1.6-0.8b" ||
    lower === "onejev1.6-0.8b" ||
    lower === "onejev-1.6-0.8b" ||
    lower === "local/onejev-1.6-0.8b"
  ) {
    return LOCAL_ONEJEV_0_8B_MODEL;
  }
  if (
    lower === "onejev_1_6_4b" ||
    lower === "omnijev/onejev1.6-4b" ||
    lower === "onejev1.6-4b" ||
    lower === "onejev-1.6-4b" ||
    lower === "local/onejev-1.6-4b"
  ) {
    return LOCAL_ONEJEV_4B_MODEL;
  }
  if (
    lower === "onejev_1_6_9b" ||
    lower === "omnijev/onejev1.6-9b" ||
    lower === "onejev1.6-9b" ||
    lower === "onejev-1.6-9b" ||
    lower === "local/onejev-1.6-9b"
  ) {
    return LOCAL_ONEJEV_9B_MODEL;
  }
  return "";
}

export function isLocalLayaJevModel(rawModel) {
  return canonicalLocalJevModel(rawModel) === LOCAL_LAYA_JEV_MODEL;
}

export function isLocalOneJevModel(rawModel) {
  const id = canonicalLocalJevModel(rawModel);
  return Boolean(id && id !== LOCAL_LAYA_JEV_MODEL);
}

export function isLocalJevModel(rawModel) {
  return Boolean(canonicalLocalJevModel(rawModel));
}

export function isSpecificLocalJevInstalled(status, canonicalId) {
  if (!canonicalId) return false;
  const allLocal = Array.isArray(status?.local?.models) ? status.local.models : [];
  const item = allLocal.find((m) => m && m.id === canonicalId);
  if (item) return Boolean(item.installed);
  const rawActive = status?.local?.settings?.jev_model || status?.jev?.model || "";
  if (canonicalLocalJevModel(rawActive) === canonicalId) {
    return Boolean(status?.jev?.local_installed);
  }
  return false;
}

export function jevModelError(rawModel) {
  const model = String(rawModel || "").trim();
  if (!model) return "请选择或填写 Jev 模型名称（如 typesafe/jev-1.14）";
  if (model.length > 64 || model.includes("..") || !/^typesafe\/jev-[A-Za-z0-9._-]+$/.test(model)) {
    return "Jev 模型须以 typesafe/jev- 开头（如 typesafe/jev-1.14 或 typesafe/jev-1.13）";
  }
  return "";
}

export function builtinJevForm(status) {
  const jev = status?.jev || {};
  const local = status?.local?.settings || {};
  const rawModel = jev.model || local.jev_model || DEFAULT_JEV_MODEL;
  const cloudModel = isLocalJevModel(rawModel) ? DEFAULT_JEV_MODEL : rawModel;
  return {
    api_key: "",
    model: cloudModel,
    voice_enabled: Boolean(jev.voice_enabled ?? local.jev_filter_enabled),
    feedback_enabled: Boolean(jev.feedback_enabled),
  };
}

/** 同步 Jev 模型卡片草稿：仅保留用户未保存的输入（如手填 api_key 或未保存的开关改动），其余跟随新 baseline。 */
export function syncJevDraft(prevBaseline, nextBaseline, currentForm) {
  const prev = prevBaseline || nextBaseline;
  if (!currentForm) return { ...nextBaseline };
  return {
    api_key: currentForm.api_key || "",
    model: currentForm.model !== prev.model ? currentForm.model : nextBaseline.model,
    voice_enabled:
      currentForm.voice_enabled !== prev.voice_enabled ? currentForm.voice_enabled : nextBaseline.voice_enabled,
    feedback_enabled:
      currentForm.feedback_enabled !== prev.feedback_enabled
        ? currentForm.feedback_enabled
        : nextBaseline.feedback_enabled,
  };
}

export function builtinJevKeyPlaceholder(status) {
  if (status?.jev?.key_set) return "已设置（留空则沿用已保存的密钥）";
  if (status?.jev?.openrouter_llm_key_set) return "留空将自动复用“大模型”已保存的 OpenRouter 密钥";
  return "未设置（请输入 OpenRouter API Key）";
}

export function builtinJevSummary(status) {
  const rawModel = status?.jev?.model || status?.local?.settings?.jev_model || DEFAULT_JEV_MODEL;
  const isLocal = isLocalJevModel(rawModel);
  const voiceOn = Boolean(status?.jev?.voice_enabled ?? status?.local?.settings?.jev_filter_enabled);
  const feedbackOn = Boolean(status?.jev?.feedback_enabled);
  const cloudKeyReady = Boolean(
    status?.jev?.key_set || status?.jev?.openrouter_llm_key_set || (!isLocal && status?.local?.jev_key_configured)
  );
  if (voiceOn && isLocal) {
    return cloudKeyReady ? "使用本地模型（云端设置作备用）" : "使用本地模型";
  }
  if (!voiceOn && feedbackOn && isLocal) {
    return cloudKeyReady ? "加工反馈开（本地模型） · 云端设置作备用" : "加工反馈开（本地模型）";
  }
  if (!cloudKeyReady) return "未配置";
  const model = isLocal ? DEFAULT_JEV_MODEL : rawModel;
  const scopes = [
    voiceOn ? "语音筛选开" : "",
    feedbackOn ? "加工反馈开（固定 1.13）" : "",
  ].filter(Boolean);
  if (!voiceOn && feedbackOn) {
    return `加工反馈开（固定 1.13） · 语音待用 ${model}`;
  }
  return scopes.length ? `${scopes.join(" + ")} · ${model}` : `已配密钥（未启用） · ${model}`;
}

export function jevFingerprint(form) {
  return JSON.stringify([String(form?.api_key || "").trim(), String(form?.model || "").trim()]);
}

/**
 * 构造 Jev 决策卡片的保存载荷：
 * 当当前实际路由为本地 Jev（`OneJev 1.6` 或 `Laya`）且用户未主动修改云端模型名时，
 * 保留当前本地模型 ID（只更新开关或备用 OpenRouter 密钥），绝不把本地离线路由悄悄覆写成云端模型；
 * 若本地权重已卸载且用户显式手填了云端 API Key，则按表单云端模型提交。
 */
export function builtinJevSavePayload(status, form, baseline) {
  const rawModel = status?.local?.settings?.jev_model || status?.jev?.model || DEFAULT_JEV_MODEL;
  const activeLocalId = canonicalLocalJevModel(rawModel);
  const isCurrentLocal = Boolean(activeLocalId);
  const localInstalled = activeLocalId ? isSpecificLocalJevInstalled(status, activeLocalId) : false;
  const typedKey = String(form?.api_key || "").trim();
  const typedModel = String(form?.model || DEFAULT_JEV_MODEL).trim();
  const baseModel = String(baseline?.model || DEFAULT_JEV_MODEL).trim();
  const modelChanged = typedModel !== baseModel;
  const switchUninstalledLocalToCloud = isCurrentLocal && !localInstalled && Boolean(typedKey);
  const preserveLocal = isCurrentLocal && !modelChanged && !switchUninstalledLocalToCloud;
  return {
    api_key: String(form?.api_key || ""),
    model: preserveLocal ? activeLocalId : typedModel,
    voice_enabled: Boolean(form?.voice_enabled),
    feedback_enabled: Boolean(form?.feedback_enabled),
  };
}

/**
 * 顶部自由混搭栏按真实语音处理链路排序（从收音到播报）：
 * 拾音判停 (vad) → 语音识别 (asr) → Jev 前置粗筛 (jev，识别后·进大模型前) → 大模型 (llm) → 语音合成 (tts)
 */
export const MIX_SECTIONS = [
  { key: "vad", label: "1. 拾音判停", hint: "检测说话开始与结束（本地模型可抗风扇/机器噪音）" },
  { key: "asr", label: "2. 语音识别", hint: "将语音转为文字（可混搭云端或本地模型）" },
  { key: "jev", label: "3. Jev 前置粗筛", hint: "识别后·大模型前：半截话续听、快答分流、工具与记忆粗筛" },
  { key: "llm", label: "4. 大模型", hint: "接收粗筛后的上下文与工具，生成对话回复与调用" },
  { key: "tts", label: "5. 语音合成", hint: "将回复合成为语音播报（可混搭云端或本地模型）" },
];

/**
 * 内置服务单项（llm / asr / tts / vad / jev）的顶部下拉切换视图：
 * - `asr` / `tts` 可在“云端模型”与“本地模型”之间独立切换；
 * - `vad` 可在“内置能量判停（免下载）”与“本地判停模型（Silero VAD / TEN-VAD）”之间独立切换；
 * - `jev` 可在“关闭（仅本地规则）”、“开启 · typesafe/jev-1.14 / 1.13”与“本地 · OneJev 1.6 (0.8B/4B/9B) / Laya”之间直接切换；
 * - 切换时已保存的密钥与备用配置原样保留。
 */
export function builtinSectionSourceView(section, status) {
  if (section === "jev") {
    const jevOn = Boolean(status?.local?.settings?.jev_filter_enabled ?? status?.jev?.voice_enabled);
    const rawJevModel = status?.local?.settings?.jev_model || status?.jev?.model || DEFAULT_JEV_MODEL;
    const activeLocalId = canonicalLocalJevModel(rawJevModel);
    const isCurrentLocal = Boolean(activeLocalId);
    const cloudModel = isCurrentLocal ? DEFAULT_JEV_MODEL : rawJevModel;
    const allLocal = Array.isArray(status?.local?.models) ? status.local.models : [];
    const catalogJevItems = allLocal.filter((m) => m && m.kind === "jev");
    const localSpecs = catalogJevItems.length > 0 ? catalogJevItems : LOCAL_JEV_FALLBACK_MODELS;
    const localOptions = localSpecs.map((item) => {
      const installed = catalogJevItems.length > 0 ? Boolean(item.installed) : isSpecificLocalJevInstalled(status, item.id);
      return {
        value: `local:${item.id}`,
        id: item.id,
        modelLabel: item.label,
        isLocal: true,
        installed,
        label: `本地 · ${item.label}${installed ? "" : "（未下载）"}`,
      };
    });
    const selectedLocalOption = isCurrentLocal
      ? localOptions.find((item) => item.id === activeLocalId) || null
      : null;
    const selectedLocalModel = isCurrentLocal
      ? catalogJevItems.find((item) => item.id === activeLocalId) ||
        (selectedLocalOption
          ? { id: selectedLocalOption.id, label: selectedLocalOption.modelLabel, installed: selectedLocalOption.installed }
          : null)
      : null;
    const installedLocalModel =
      (selectedLocalModel && selectedLocalModel.installed ? selectedLocalModel : null) ||
      catalogJevItems.find((item) => item.installed) ||
      (localOptions.find((item) => item.installed)
        ? {
            id: localOptions.find((item) => item.installed).id,
            label: localOptions.find((item) => item.installed).modelLabel,
            installed: true,
          }
        : null);
    const activeLocalInstalled = Boolean(selectedLocalOption?.installed);
    const cloudKeyReady = Boolean(
      status?.jev?.key_set ||
        status?.jev?.openrouter_llm_key_set ||
        (!isCurrentLocal && status?.local?.jev_key_configured)
    );
    const currentReady = isCurrentLocal ? activeLocalInstalled : cloudKeyReady;
    const presets = JEV_PRESET_MODELS.some((item) => item.id === cloudModel)
      ? JEV_PRESET_MODELS
      : [{ id: cloudModel, hint: "当前选用" }, ...JEV_PRESET_MODELS];
    const options = [
      { value: "off", label: "关闭（仅本地 0ms 规则）", installed: true },
      ...presets.map((item) => ({
        value: `jev:${item.id}`,
        id: item.id,
        modelLabel: item.id,
        isLocal: false,
        installed: cloudKeyReady,
        label: `开启 · ${item.id}${item.id === DEFAULT_JEV_MODEL ? "（推荐）" : ""}${cloudKeyReady ? "" : "（需配密钥）"}`,
      })),
      ...localOptions,
    ];
    const activeLocalLabel = selectedLocalModel?.label || activeLocalId || "本地决策模型";
    return {
      section: "jev",
      canUseLocal: true,
      source: jevOn ? (isCurrentLocal ? "local" : "jev") : "off",
      localOn: Boolean(jevOn && isCurrentLocal),
      localReady: currentReady,
      cloudConfigured: cloudKeyReady,
      cloudModel,
      activeText: jevOn
        ? isCurrentLocal
          ? activeLocalInstalled
            ? `本地（${activeLocalLabel}）`
            : `本地（${activeLocalLabel} · 未下载）`
          : currentReady
            ? `开启（${cloudModel}）`
            : `开启（${cloudModel} · 未配密钥）`
        : "关闭（仅本地 0ms 规则）",
      selectValue: jevOn ? (isCurrentLocal ? `local:${activeLocalId}` : `jev:${cloudModel}`) : "off",
      options,
      installedLocalModel,
      selectedLocalModel,
    };
  }
  const isVad = section === "vad";
  const entry = !isVad ? status?.models?.[section] : null;
  const cloudConfigured = Boolean(entry?.configured);
  const cloudModel = entry?.model || "";
  const defaultValue = isVad ? "energy" : "cloud";
  const defaultOptionLabel = isVad
    ? "内置能量判停（免下载）"
    : cloudConfigured
      ? `云端（${entry?.kind === "doubao" ? `豆包 · ${cloudModel}` : cloudModel}）`
      : "云端（未配置）";
  if (section === "llm") {
    return {
      section,
      canUseLocal: false,
      source: "cloud",
      localOn: false,
      localReady: false,
      cloudConfigured,
      cloudModel,
      activeText: cloudConfigured ? `云端（${cloudModel}）` : "未配置",
      selectValue: "cloud",
      options: [{ value: "cloud", label: defaultOptionLabel, installed: true }],
      installedLocalModel: null,
      selectedLocalModel: null,
    };
  }
  const localSettings = localSettingsForm(status?.local?.settings);
  const localOn = Boolean(status?.local?.settings?.[section]);
  const allLocal = Array.isArray(status?.local?.models) ? status.local.models : [];
  const kindModels = allLocal.filter((item) => item && item.kind === section);
  const selectedId = localSettings[`${section}_model`];
  const selectedLocalModel = kindModels.find((item) => item.id === selectedId) || kindModels[0] || null;
  const installedLocalModel =
    selectedLocalModel && selectedLocalModel.installed
      ? selectedLocalModel
      : kindModels.find((item) => item.installed) || null;
  const localReady = Boolean(localOn && selectedLocalModel?.installed);
  const source = localOn ? "local" : defaultValue;
  const activeText = localOn
    ? selectedLocalModel?.installed
      ? `本地（${selectedLocalModel.label}）`
      : `本地（${selectedLocalModel?.label || "未下载"} · 未安装）`
    : defaultOptionLabel;
  const localOptions =
    kindModels.length > 0
      ? kindModels.map((item) => ({
          value: `local:${item.id}`,
          id: item.id,
          modelLabel: item.label,
          installed: Boolean(item.installed),
          label: `本地 · ${item.label}${item.installed ? "" : "（未下载）"}`,
        }))
      : [
          {
            value: "local",
            id: selectedId,
            modelLabel: "本地模型",
            installed: false,
            label: "本地模型（未下载）",
          },
        ];
  const selectValue = localOn
    ? selectedLocalModel
      ? `local:${selectedLocalModel.id}`
      : "local"
    : defaultValue;
  return {
    section,
    canUseLocal: true,
    source,
    localOn,
    localReady,
    cloudConfigured,
    cloudModel,
    activeText,
    selectValue,
    options: [{ value: defaultValue, label: defaultOptionLabel, installed: true }, ...localOptions],
    installedLocalModel,
    selectedLocalModel,
  };
}

/**
 * 生成将某个分区（asr / tts / vad / jev）在顶部下拉框切换后的 localSave 载荷。
 */
export function builtinSourceSwitchPayload(status, section, targetSource) {
  if (section === "jev") {
    let form = localSettingsForm(status?.local?.settings);
    const target = String(targetSource || "");
    if (target === "off") {
      form = { ...form, jev_filter_enabled: false };
    } else if (target.startsWith("local:")) {
      const localId = canonicalLocalJevModel(target.slice("local:".length));
      if (!localId) return null;
      form = { ...form, jev_filter_enabled: true, jev_model: localId };
    } else if (target.startsWith("jev:")) {
      const rawId = target.slice("jev:".length).trim() || DEFAULT_JEV_MODEL;
      const maybeLocal = canonicalLocalJevModel(rawId);
      if (maybeLocal) {
        form = { ...form, jev_filter_enabled: true, jev_model: maybeLocal };
      } else {
        if (jevModelError(rawId)) return null;
        form = { ...form, jev_filter_enabled: true, jev_model: rawId };
      }
    } else {
      return null;
    }
    return localSettingsPayload(form);
  }
  if (section !== "asr" && section !== "tts" && section !== "vad") return null;
  const allLocal = Array.isArray(status?.local?.models) ? status.local.models : [];
  let form = localSettingsForm(status?.local?.settings);
  const target = String(targetSource || "");
  if (target.startsWith("local:")) {
    const modelId = target.slice("local:").length ? target.slice("local:".length).trim() : "";
    if (modelId) {
      form = selectLocalModel(form, allLocal, section, modelId);
    }
    form = { ...form, [section]: true };
  } else if (target === "local") {
    const view = builtinSectionSourceView(section, status);
    if (view.installedLocalModel && view.installedLocalModel.id !== form[`${section}_model`]) {
      form = selectLocalModel(form, allLocal, section, view.installedLocalModel.id);
    }
    form = { ...form, [section]: true };
  } else {
    form = { ...form, [section]: false };
  }
  return localSettingsPayload(form);
}

/**
 * 生成顶部混搭栏单分区切换的最小增量补丁（仅提交本次切换涉及的字段，避免覆盖其他分组在途或刚保存的改动）。
 */
export function builtinSourceSwitchPatch(status, section, targetSource) {
  if (section === "jev") {
    const target = String(targetSource || "");
    if (target === "off") {
      return { jev_filter_enabled: false };
    }
    if (target.startsWith("local:")) {
      const localId = canonicalLocalJevModel(target.slice("local:".length));
      if (!localId) return null;
      return { jev_filter_enabled: true, jev_model: localId };
    }
    if (target.startsWith("jev:")) {
      const rawId = target.slice("jev:".length).trim() || DEFAULT_JEV_MODEL;
      const maybeLocal = canonicalLocalJevModel(rawId);
      if (maybeLocal) {
        return { jev_filter_enabled: true, jev_model: maybeLocal };
      }
      if (jevModelError(rawId)) return null;
      return { jev_filter_enabled: true, jev_model: rawId };
    }
    return null;
  }
  const full = builtinSourceSwitchPayload(status, section, targetSource);
  if (!full) return null;
  const keys =
    section === "tts"
      ? ["tts", "tts_model", "tts_speaker"]
      : [section, `${section}_model`];
  return Object.fromEntries(keys.map((key) => [key, full[key]]));
}

/** 外部服务端模型分组折叠时的一行摘要（MOSS 不读回服务端配置，只能显示最近一次测试）。 */
export function externalModelSummary(record) {
  if (!record) return "未测试";
  return `上次测试${record.ok ? "通过" : "未通过"}`;
}

// ---------- 声纹简易标定（2026-10-14 用户决定，取代“源码写死不开放”） ----------

/** 标定区的显示：状态文字、每人测试音条数、能否计算阈值。 */
export function speakerCalibrationView(data) {
  const speakers = Array.isArray(data?.speakers) ? data.speakers : [];
  const clips = data?.test_clips && typeof data.test_clips === "object" ? data.test_clips : {};
  const calibration = data?.calibration || { state: "none" };
  const report = calibration.report;
  const result = report
    ? `阈值 ${Number(report.threshold).toFixed(2)} · 本人认对 ${report.genuine_accepted}/${report.genuine_trials} · 认成别人 ${report.false_accepts}/${report.impostor_trials}`
    : "";
  const status = {
    valid: `已标定（${report?.people ?? 0} 人）：${result}`,
    stale: "登记名单在标定后变过（有人新增、删除或重新登记），需要重新标定；在此之前不识别身份",
    none: "未标定：身份识别与按人记忆保持关闭",
  }[calibration.state] || "未标定：身份识别与按人记忆保持关闭";
  const counts = Object.fromEntries(speakers.map((row) => [row.id, Number(clips[row.id]) || 0]));
  const ready = speakers.length >= 2 && speakers.every((row) => counts[row.id] >= 3);
  const blocked = speakers.length < 2 ? "至少需要两个已登记的人" : ready ? "" : "每个人都要录满三段测试音";
  return { state: calibration.state || "none", status, counts, ready, blocked };
}

// ---------- 固件 OTA 推送记录（2026-10-15） ----------

/** 推送记录的显示：只有设备上报了新版本才算升级完成；下载开始不等于刷写成功。 */
export function firmwareDeliveryView(row, now = Date.now() / 1000) {
  if (row?.device_report_status === "reported") return { text: "已升级（设备已上报新版本）", cancellable: false };
  const started = row?.download_status === "downloading" || row?.download_status === "unknown";
  if (!(Number(row?.expires_at) > Number(now))) {
    return { text: started ? "推送已失效；设备曾开始下载，请核对设备当前版本" : "推送已失效（已取消或已过期）", cancellable: false };
  }
  if (row.download_status === "pending") return { text: "等待设备检查更新（重启设备即可）", cancellable: true };
  if (row.download_status === "downloading") return { text: "设备已开始下载，等待上报新版本", cancellable: true };
  return { text: "下载结果未知（下载时 MOSS 重启过），等待设备上报版本", cancellable: true };
}

// ---------- Agent（Claude Code / Codex，2026-10-15 起可用） ----------

const AGENT_ERROR_TEXT = {
  agent_disabled: "未开启",
  agent_strict_mode_required: "需要先开启严格模式",
  agent_path_denied: "路径不可用（工作目录或程序路径不符合要求）",
  agent_capability_missing: "无法识别这个 CLI，或缺少需要的安全参数",
  agent_run_failed: "运行失败",
  agent_timeout: "超时（5 分钟）已停止",
  agent_stopped: "已手动停止",
  agent_permission_expired: "设备授权或配置变化，已停止",
  agent_sandbox_unavailable: "当前系统不支持",
  agent_thread_spawn_failed: "Agent 任务线程启动失败，已记为失败",
};

export function agentErrorText(code) {
  return AGENT_ERROR_TEXT[code] || code || "";
}

const RUN_STATUS = { running: "运行中", completed: "已完成", failed: "失败", cancelled: "已停止" };

/** Agent 卡的显示：总状态、每个 CLI 能否用、每条运行记录。 */
export function agentView(info) {
  const providers = (Array.isArray(info?.providers) ? info.providers : []).map((item) => {
    const sourceTag = item.source === "auto_fallback"
      ? " · 已自动适配当前设备"
      : item.source === "auto"
        ? " · 自动读取"
        : "";
    return {
      name: item.provider === "codex" ? "Codex" : "Claude Code",
      ready: Boolean(item.ready),
      source: item.source || "configured",
      executable: item.executable || "",
      text: item.ready
        ? `可用${item.version ? `（${item.version}）` : ""}${sourceTag}`
        : agentErrorText(item.error_code),
    };
  });
  const summary = !info?.configured ? "未开启" : !info?.strict_mode ? "已开启 · 需要严格模式" : providers.some((p) => p.ready) ? "可用" : "已开启 · 没有可用的 CLI";
  const runs = (Array.isArray(info?.runs) ? info.runs : []).slice(0, 50).map((run) => ({
    ...run,
    statusText: RUN_STATUS[run.status] || run.status,
    detail: run.output ? String(run.output) : agentErrorText(run.error_code) || "无结果",
  }));
  return { summary, providers, runs, strictMissing: Boolean(info?.configured) && !info?.strict_mode };
}

// ---------- MOSS 全息机械眼与三链路语音孪生（2026-11-03） ----------

const SYMMETRIC_WAVE_WEIGHTS = [0.34, 0.50, 0.68, 0.84, 0.94, 1.0, 1.0, 0.94, 0.84, 0.68, 0.50, 0.34];
const THINKING_SCAN_BARS = [22, 34, 48, 62, 48, 34, 34, 48, 62, 48, 34, 22];
const PULSE_EVENT_BARS = [18, 30, 52, 74, 88, 64, 64, 88, 74, 52, 30, 18];
const ALERT_BARS = [42, 68, 42, 78, 42, 86, 86, 42, 78, 42, 68, 42];
const IDLE_BARS = [10, 12, 14, 16, 18, 20, 20, 18, 16, 14, 12, 10];
const OFFLINE_BARS = [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6];

function finiteNonNegativeInt(value, fallback = null) {
  if (value === null || value === undefined || value === "" || typeof value === "boolean") {
    return fallback;
  }
  const num = Number(value);
  if (!Number.isFinite(num) || num < 0) {
    return fallback;
  }
  return Math.trunc(num);
}

function sanitizeTwinLine(raw, maxLen = 140) {
  if (!raw) return "";
  const masked = maskLogLine(String(raw))
    .replace(/流式中间结果整句命中急停词（[^）]*）/g, "流式中间结果整句命中急停词")
    .replace(/开始播放本地歌曲：.+/g, "开始播放本地歌曲")
    .trim();
  return masked.length > maxLen ? `${masked.slice(0, maxLen)}…` : masked;
}

function sanitizeIdentifier(raw, maxLen = 48) {
  const clean = maskLogLine(String(raw || ""))
    .replace(/[\r\n\t]+/g, " ")
    .trim();
  return clean.length > maxLen ? `${clean.slice(0, maxLen)}…` : clean;
}

const MOSS_EYE_PALETTES = {
  offline: {
    coreStops: ["#e2e8f0", "#94a3b8", "#475569", "#1e293b", "#090d16"],
    glowStops: ["rgba(148, 163, 184, 0.14)", "rgba(71, 85, 105, 0.06)", "rgba(15, 23, 42, 0)"],
    accent: "#64748b",
    tempo: "dormant",
  },
  idle: {
    coreStops: ["#ffffff", "#ffc2cd", "#ff3b5c", "#990a1e", "#1e0005"],
    glowStops: ["rgba(255, 59, 92, 0.36)", "rgba(56, 189, 248, 0.16)", "rgba(255, 59, 92, 0)"],
    accent: "#ff3b5c",
    tempo: "cruise",
  },
  listening: {
    coreStops: ["#ffffff", "#bae6fd", "#38bdf8", "#0369a1", "#081426"],
    glowStops: ["rgba(56, 189, 248, 0.52)", "rgba(255, 59, 92, 0.24)", "rgba(56, 189, 248, 0)"],
    accent: "#38bdf8",
    tempo: "acoustic",
  },
  thinking: {
    coreStops: ["#ffffff", "#e9d5ff", "#a855f7", "#581c87", "#160526"],
    glowStops: ["rgba(168, 85, 247, 0.50)", "rgba(56, 189, 248, 0.24)", "rgba(168, 85, 247, 0)"],
    accent: "#a855f7",
    tempo: "neural",
  },
  pulse: {
    coreStops: ["#ffffff", "#ddd6fe", "#8b5cf6", "#4c1d95", "#130726"],
    glowStops: ["rgba(139, 92, 246, 0.52)", "rgba(255, 59, 92, 0.26)", "rgba(139, 92, 246, 0)"],
    accent: "#a78bfa",
    tempo: "neural",
  },
  speaking: {
    coreStops: ["#ffffff", "#ffe4e8", "#ff2a52", "#b80828", "#240007"],
    glowStops: ["rgba(255, 59, 92, 0.56)", "rgba(251, 113, 133, 0.28)", "rgba(255, 59, 92, 0)"],
    accent: "#ff3b5c",
    tempo: "stream",
  },
  alert: {
    coreStops: ["#ffffff", "#fef08a", "#f59e0b", "#dc2626", "#290602"],
    glowStops: ["rgba(245, 158, 11, 0.58)", "rgba(239, 68, 68, 0.30)", "rgba(245, 158, 11, 0)"],
    accent: "#f59e0b",
    tempo: "alert",
  },
  engraving: {
    coreStops: ["#ffffff", "#a7f3d0", "#10b981", "#047857", "#031c14"],
    glowStops: ["rgba(16, 185, 129, 0.54)", "rgba(56, 189, 248, 0.28)", "rgba(16, 185, 129, 0)"],
    accent: "#10b981",
    tempo: "laser",
  },
};

/**
 * 根据机械眼视觉状态返回核心球体渐变色与动态节奏配置（确定性纯函数）。
 */
export function mossEyeCorePalette(visualState = "idle") {
  return MOSS_EYE_PALETTES[visualState] || MOSS_EYE_PALETTES.idle;
}

/**
 * 生成 12 柱对称声波/指令脉冲高度（0..100，确定性纯函数，绝不生成随机假音频分贝）。
 * - `builtin` 的 `listening` / `speaking` 按真实 60ms PCM RMS 音量 `level` 缩放；
 * - `thinking` / `pulse` / `alert` / `idle` / `offline` 使用固定的状态几何图案。
 */
export function buildTwinWaveBars(visualState, level = 0) {
  const rawLevel = finiteNonNegativeInt(level, 0);
  const clamped = Math.max(0, Math.min(100, rawLevel));
  if ((visualState === "listening" || visualState === "speaking") && clamped > 0) {
    return SYMMETRIC_WAVE_WEIGHTS.map((weight) => Math.max(12, Math.min(100, Math.round(10 + clamped * 0.9 * weight))));
  }
  if (visualState === "listening" || visualState === "speaking") {
    return [14, 18, 22, 26, 30, 34, 34, 30, 26, 22, 18, 14];
  }
  if (visualState === "thinking") return [...THINKING_SCAN_BARS];
  if (visualState === "pulse") return [...PULSE_EVENT_BARS];
  if (visualState === "alert") return [...ALERT_BARS];
  if (visualState === "idle") return [...IDLE_BARS];
  return [...OFFLINE_BARS];
}

function sessionPriority(state) {
  if (state === "speaking") return 4;
  if (state === "thinking") return 3;
  if (state === "listening") return 2;
  if (state === "idle") return 1;
  return 0;
}

function pickPrimarySession(sessions) {
  if (!Array.isArray(sessions) || sessions.length === 0) return null;
  return [...sessions].sort((a, b) => {
    const diff = sessionPriority(b?.state) - sessionPriority(a?.state);
    if (diff !== 0) return diff;
    return (finiteNonNegativeInt(b?.level, 0)) - (finiteNonNegativeInt(a?.level, 0));
  })[0];
}

/**
 * 语音中心顶部「MOSS 全息语音孪生舱」三链路自适应视图（确定性纯函数）。
 * - `builtin`：同步 60ms 真实 PCM 振幅 `level`、会话阶段、识别字数、已播句数与工具调用；
 * - `self_hosted`：同步自建 `xiaozhi-server` 进程状态、健康检查会话数、脱敏日志与 MCP 工具脉冲；
 * - `official`：同步官方云端 MCP 握手状态、累计 JSON-RPC 消息帧数与远端工具调用脉冲。
 */
export function mossEyeTwinView({
  mode = "official",
  status = null,
  builtinStatus = null,
  mcpStatus = null,
  logLines = [],
  approvalCount = 0,
} = {}) {
  const normalizedMode = mode === "builtin" || mode === "self_hosted" || mode === "official" ? mode : "official";
  const approvals = finiteNonNegativeInt(approvalCount, 0);
  const approvalAlert = approvals > 0;

  if (normalizedMode === "builtin") {
    const running = Boolean(builtinStatus?.running);
    const sessions = Array.isArray(builtinStatus?.sessions) ? builtinStatus.sessions : [];
    const events = Array.isArray(builtinStatus?.events) ? builtinStatus.events : [];
    const primary = pickPrimarySession(sessions);
    const rawState = primary?.state || "idle";
    const activeAudio = running && sessions.length > 0 && (rawState === "listening" || rawState === "speaking");
    const level = activeAudio ? Math.max(0, Math.min(100, finiteNonNegativeInt(primary?.level, 0))) : 0;
    const activeTool = sanitizeIdentifier(primary?.active_tool, 48);
    const emotion = sanitizeIdentifier(primary?.emotion, 24);
    const utteranceChars = finiteNonNegativeInt(primary?.utterance_chars, 0);
    const spokenSentences = finiteNonNegativeInt(primary?.spoken_sentences, 0);

    let visualState = "offline";
    if (running) {
      if (rawState === "speaking") visualState = "speaking";
      else if (rawState === "thinking") visualState = "thinking";
      else if (rawState === "listening") visualState = "listening";
      else if (approvalAlert) visualState = "alert";
      else visualState = "idle";
    } else if (approvalAlert) {
      visualState = "alert";
    }

    let headline = "内置语音未启动 · 光学镜头待命";
    if (running) {
      if (sessions.length === 0) {
        headline = "内置服务运行中 · 等待小智设备接入";
      } else if (rawState === "speaking") {
        headline = "MOSS 正在播报 · 60ms 帧同步输出";
      } else if (rawState === "thinking") {
        headline = activeTool ? `正在执行工具 · ${activeTool}` : "神经网络推理中 · Jev / LLM 路由";
      } else if (rawState === "listening") {
        headline = level > 0 ? "正在拾音倾听 · 实时声波锁定" : "拾音通道开启 · 等待语音输入";
      } else if (approvalAlert) {
        headline = `等待桌面安全审批 (${approvals} 项)`;
      } else {
        headline = `小智设备在线 (${sessions.length} 台) · 随时唤醒`;
      }
    }

    const latestEvent = sanitizeTwinLine(events[0]?.text);
    const subline = latestEvent
      || (running
        ? `工作模式：${builtinStatus?.mode === "chat" ? "AI 对话模式" : "回声测试模式"}`
        : "启动内置语音服务后，此处实时镜像小智设备的拾音振幅、思考路由与播报节奏");

    const progressValue = spokenSentences > 0
      ? `已播 ${spokenSentences} 句`
      : activeTool
        ? activeTool
        : emotion
          ? `情绪 ${emotion}`
          : running
            ? "待命"
            : "未运行";

    return {
      mode: "builtin",
      visualState,
      sourceBadge: "REALTIME AUDIO TWIN",
      sourceLabel: "内置服务 · 60ms 实时音频孪生",
      level,
      approvalAlert,
      approvalCount: approvals,
      activePolling: running && sessions.some((s) => s?.state === "listening" || s?.state === "thinking" || s?.state === "speaking"),
      headline,
      subline,
      bars: buildTwinWaveBars(visualState, level),
      metrics: [
        { label: "在线会话", value: `${sessions.length} 台` },
        { label: "实时振幅", value: activeAudio ? `${level}%` : "0% (静音)" },
        { label: "本轮识别", value: utteranceChars > 0 ? `${utteranceChars} 字` : "—" },
        { label: "播报/路由", value: progressValue },
      ],
    };
  }

  if (normalizedMode === "self_hosted") {
    const serverState = status?.server?.state || "stopped";
    const serverRunning = serverState === "running";
    const serverExited = serverState === "exited";
    const bridgeConnected = Boolean(status?.bridge?.connected ?? mcpStatus?.connected);
    const healthSessions = finiteNonNegativeInt(status?.endpoint?.health?.robot_connections, null);
    const mcpTools = Array.isArray(status?.mcp?.tools) ? status.mcp.tools : [];
    const latestCalledTool = [...mcpTools].filter((t) => finiteNonNegativeInt(t?.calls, 0) > 0).sort((a, b) => (Number(b?.last_at) || 0) - (Number(a?.last_at) || 0))[0];
    const rawLastRequest = String(mcpStatus?.last_request || (latestCalledTool ? `tools/call: ${latestCalledTool.name}` : "")).trim();
    const lastRequest = sanitizeTwinLine(rawLastRequest, 72);
    const sanitizedLines = (Array.isArray(logLines) ? logLines : [])
      .map((line) => sanitizeTwinLine(line))
      .filter(Boolean);
    const lastLog = sanitizedLines.length > 0 ? sanitizedLines[sanitizedLines.length - 1] : "";
    const hasToolCall = lastRequest.startsWith("tools/call:");
    const toolName = hasToolCall ? sanitizeIdentifier(lastRequest.slice("tools/call:".length), 48) : "";

    let visualState = "offline";
    if (approvalAlert) {
      visualState = "alert";
    } else if (serverRunning || bridgeConnected) {
      visualState = hasToolCall ? "pulse" : "idle";
    }

    let headline = "自建服务端未运行 · 桥接休眠";
    if (approvalAlert) {
      headline = `自建链路发起受控操作 · ${approvals} 项待桌面批准`;
    } else if (serverExited) {
      headline = `自建服务端意外退出 (退出码 ${status?.server?.exit_code ?? "未知"})`;
    } else if (hasToolCall && (serverRunning || bridgeConnected)) {
      headline = `正在响应自建服务指令 · ${toolName}`;
    } else if (serverRunning && healthSessions !== null && healthSessions > 0) {
      headline = `自建服务运行中 · ${healthSessions} 台小智会话在线`;
    } else if (serverRunning) {
      headline = "自建服务运行中 · 监听进程日志与 MCP 信号";
    } else if (bridgeConnected) {
      headline = "自建 MCP 桥已连接 · 等待服务端会话";
    }

    const recentMcpEvent = Array.isArray(mcpStatus?.recent_events) && mcpStatus.recent_events[0]
      ? sanitizeTwinLine(mcpStatus.recent_events[0])
      : "";
    const subline = lastLog
      ? `最新日志：${lastLog}`
      : recentMcpEvent || "音频流由外部 xiaozhi-server 处理；此处同步进程脱敏日志与 MCP 工具脉冲（不伪造音频分贝）";

    const pid = finiteNonNegativeInt(status?.server?.pid, null);
    return {
      mode: "self_hosted",
      visualState,
      sourceBadge: "LOCAL SERVER BRIDGE",
      sourceLabel: "自建服务 · 进程日志与 MCP 桥接",
      level: 0,
      approvalAlert,
      approvalCount: approvals,
      activePolling: false,
      headline,
      subline,
      bars: buildTwinWaveBars(visualState, 0),
      metrics: [
        {
          label: "服务端进程",
          value: serverRunning ? (pid !== null ? `运行中 (PID ${pid})` : "运行中") : serverExited ? "异常退出" : "未运行",
        },
        { label: "MCP 桥接", value: bridgeConnected ? "已连接" : "未连接" },
        { label: "设备会话", value: healthSessions !== null ? `${healthSessions} 台` : "未配置健康检查" },
        { label: "最近指令", value: lastRequest || "尚无工具调用" },
      ],
    };
  }

  // official 官方链路
  const configured = Boolean(status?.mcp_endpoint || mcpStatus?.configured);
  const bridgeConnected = Boolean(status?.bridge?.connected ?? mcpStatus?.connected);
  const mcpTools = Array.isArray(status?.mcp?.tools) ? status.mcp.tools : [];
  const latestCalledTool = [...mcpTools].filter((t) => finiteNonNegativeInt(t?.calls, 0) > 0).sort((a, b) => (Number(b?.last_at) || 0) - (Number(a?.last_at) || 0))[0];
  const messagesCount = finiteNonNegativeInt(mcpStatus?.messages_count, 0);
  const rawLastRequest = String(mcpStatus?.last_request || (latestCalledTool ? `tools/call: ${latestCalledTool.name}` : "")).trim();
  const lastRequest = sanitizeTwinLine(rawLastRequest, 72);
  const lastRequestTime = sanitizeIdentifier(mcpStatus?.last_request_time, 16);
  const hasToolCall = lastRequest.startsWith("tools/call:");
  const toolName = hasToolCall ? sanitizeIdentifier(lastRequest.slice("tools/call:".length), 48) : "";
  const recentEvent = Array.isArray(mcpStatus?.recent_events) && mcpStatus.recent_events[0]
    ? sanitizeTwinLine(mcpStatus.recent_events[0])
    : "";
  const rawToolsCount = finiteNonNegativeInt(mcpStatus?.tools_count, null);
  const enabledToolsCount = rawToolsCount !== null
    ? rawToolsCount
    : mcpTools.length > 0
      ? mcpTools.filter((t) => t?.enabled !== false).length
      : null;

  let visualState = "offline";
  if (approvalAlert) {
    visualState = "alert";
  } else if (configured && bridgeConnected) {
    visualState = hasToolCall ? "pulse" : "idle";
  }

  let headline = "官方 MCP 接入点未配置";
  if (approvalAlert) {
    headline = `云端发起受控操作 · ${approvals} 项待桌面批准`;
  } else if (!configured) {
    headline = "官方 MCP 接入点未配置 · 请在下方填写";
  } else if (!bridgeConnected) {
    headline = "官方 MCP 桥未连接 · 正在等待握手";
  } else if (hasToolCall) {
    headline = `收到云端小智指令 · ${toolName}`;
  } else {
    headline = "云端 MCP 神经桥在线 · 实时监听工具指令";
  }

  const subline = recentEvent
    || "语音闲聊在官方云端处理；当小智查询机床状态或发起雕刻时，此处同步触发指令脉冲";

  return {
    mode: "official",
    visualState,
    sourceBadge: "CLOUD MCP TELEMETRY",
    sourceLabel: "官方链路 · 云端 MCP 指令遥测",
    level: 0,
    approvalAlert,
    approvalCount: approvals,
    activePolling: false,
    headline,
    subline,
    bars: buildTwinWaveBars(visualState, 0),
    metrics: [
      { label: "云端网关", value: bridgeConnected ? "已握手在线" : configured ? "连接中断" : "未配置" },
      { label: "指令帧数", value: `${messagesCount} 帧` },
      { label: "最近指令", value: lastRequest ? `${lastRequest}${lastRequestTime ? ` (${lastRequestTime})` : ""}` : "暂无调用" },
      { label: "可用工具", value: enabledToolsCount !== null ? `${enabledToolsCount} 项` : "—" },
    ],
  };
}

/**
 * 左侧全局导航栏（SideNav）左上角 MOSS 光学机械眼的「硬件 + 加工 + 语音 + 审批」多维实时状态推导。
 * - 外环（`hardwareState`）：反映激光硬件握手连接与 G-code 加工任务（`engraving` / `online` / `fault` / `unprobed`）；
 * - 内核（`visualState`）：反映安全警戒、实时语音拾音/思考/播报、MCP 工具脉冲与激光雕刻聚焦。
 */
export function globalEyeSensorView({
  mcpStatus = null,
  voiceStatus = null,
  approvalCount = 0,
  connectionStatus = null,
  activeJobs = [],
} = {}) {
  const approvals = finiteNonNegativeInt(approvalCount, 0);
  const jobs = Array.isArray(activeJobs) ? activeJobs.filter(Boolean) : [];
  const hasUncertainJob = jobs.some((job) => Boolean(job?.unknown || job?.state === "unknown"));
  const activeJobCount = jobs.length;
  const primaryJob = jobs[0] || null;
  const rawJobPercent = finiteNonNegativeInt(primaryJob?.percent, null);
  const jobProgress = rawJobPercent !== null ? Math.max(0, Math.min(100, rawJobPercent)) : null;

  let hardwareState = "unprobed";
  let hwLabel = "";
  if (hasUncertainJob) {
    hardwareState = "fault";
    hwLabel = "激光任务连接状态未知（请现场确认）";
  } else if (activeJobCount > 0) {
    hardwareState = "engraving";
    hwLabel = jobProgress !== null ? `激光加工中 ${jobProgress}%` : `激光加工进行中（${activeJobCount} 项）`;
  } else if (connectionStatus?.connected) {
    hardwareState = "online";
    hwLabel = "硬件握手已通";
  } else if (connectionStatus?.checkedAt) {
    hardwareState = "fault";
    hwLabel = "硬件未连接";
  }

  const withHwSuffix = (baseLabel) => (hwLabel ? `${baseLabel} · ${hwLabel}` : baseLabel);
  const engravingBadge = jobProgress !== null ? `正在发送 ${jobProgress}%` : "正在发送加工";

  if (approvals > 0) {
    return {
      visualState: "alert",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: `待批准 (${approvals})`,
      label: withHwSuffix(`MOSS 警戒：${approvals} 个请求等待批准`),
    };
  }
  if (hasUncertainJob) {
    return {
      visualState: "alert",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: "加工连接异常",
      label: "MOSS 警戒：激光任务连接异常，请在机器旁确认停稳",
    };
  }

  const mode = voiceStatus?.mode || "official";
  if (mode === "builtin") {
    const phase = voiceStatus?.phase || null;
    const level = Math.max(0, Math.min(100, finiteNonNegativeInt(voiceStatus?.level, 0)));
    const sessions = finiteNonNegativeInt(voiceStatus?.device_sessions, null);
    if (phase === "speaking") {
      return {
        visualState: "speaking",
        hardwareState,
        jobProgress,
        level,
        shortBadge: activeJobCount > 0 ? `播报中 · ${engravingBadge}` : `正在播报 ${level}%`,
        label: withHwSuffix(`MOSS 正在播报（振幅 ${level}%）`),
      };
    }
    if (phase === "thinking") {
      return {
        visualState: "thinking",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: activeJobCount > 0 ? `思考中 · ${engravingBadge}` : "AI 思考路由",
        label: withHwSuffix("MOSS 正在思考 / 调用工具"),
      };
    }
    if (phase === "listening") {
      return {
        visualState: "listening",
        hardwareState,
        jobProgress,
        level,
        shortBadge: activeJobCount > 0 ? `拾音中 · ${engravingBadge}` : `正在拾音 ${level}%`,
        label: withHwSuffix(`MOSS 正在倾听（振幅 ${level}%）`),
      };
    }
    if (activeJobCount > 0) {
      return {
        visualState: "engraving",
        hardwareState,
        jobProgress,
        level: jobProgress !== null ? Math.max(25, jobProgress) : 60,
        shortBadge: engravingBadge,
        label: `${hwLabel}${sessions !== null && sessions > 0 ? ` · 内置语音在线（${sessions} 台）` : ""}`,
      };
    }
    if (sessions !== null && sessions > 0) {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: hardwareState === "online" ? "硬件·语音已连" : `语音已连 (${sessions}台)`,
        label: withHwSuffix(`MOSS 内置语音在线（${sessions} 台设备）`),
      };
    }
    if (sessions === 0) {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: hardwareState === "online" ? "硬件已连·待唤醒" : "语音就绪·待设备",
        label: withHwSuffix("MOSS 内置语音运行中（等待设备连接）"),
      };
    }
    if (hardwareState === "online") {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: "硬件已连接",
        label: "激光硬件已就绪 · 内置语音未启动",
      };
    }
    return {
      visualState: "offline",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: hardwareState === "fault" ? "硬件未连接" : "未连接 · 待机",
      label: withHwSuffix("MOSS 内置语音未启动"),
    };
  }

  const connected = Boolean(mcpStatus?.connected);
  const lastReq = sanitizeTwinLine(mcpStatus?.last_request, 72);
  if (connected && lastReq.startsWith("tools/call:")) {
    return {
      visualState: "pulse",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: activeJobCount > 0 ? `指令脉冲 · ${engravingBadge}` : "指令脉冲执行",
      label: withHwSuffix(`小智最近调用：${sanitizeIdentifier(lastReq.slice("tools/call:".length), 48)}`),
    };
  }
  if (activeJobCount > 0) {
    return {
      visualState: "engraving",
      hardwareState,
      jobProgress,
      level: jobProgress !== null ? Math.max(25, jobProgress) : 60,
      shortBadge: engravingBadge,
      label: `${hwLabel} · ${connected ? "小智 MCP 已连接" : "小智 MCP 未连接"}`,
    };
  }
  if (!connected) {
    if (hardwareState === "online") {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: "硬件已连接",
        label: `激光硬件已就绪 · ${mode === "self_hosted" ? "自建 MCP 桥未连接" : "小智 MCP 桥未连接"}`,
      };
    }
    return {
      visualState: "offline",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: hardwareState === "fault" ? "硬件未连接" : "未连接 · 待机",
      label: withHwSuffix(mode === "self_hosted" ? "自建 MCP 桥未连接" : "小智 MCP 桥未连接"),
    };
  }
  return {
    visualState: "idle",
    hardwareState,
    jobProgress,
    level: 0,
    shortBadge: hardwareState === "online" ? "硬件·小智已连" : "小智已连接",
    label: withHwSuffix(mode === "self_hosted" ? "自建 MCP 桥已连接" : "官方小智 MCP 桥已连接"),
  };
}

// n61 设备唤醒词（2026-11-06）：预设与前端校验；拼音音节是否被模型认识由后端按模型词表最终核对。
export const WAKE_WORD_PRESETS = [
  { text: "你好 MOSS", command: "ni hao mo si" },
  { text: "MOSS MOSS", command: "mo si mo si" },
  { text: "嘿 MOSS", command: "hei mo si" },
];

export function wakeWordDraftError(draft) {
  const text = String(draft?.text ?? "").trim();
  if (!text) return "请填写唤醒词";
  if ([...text].length > 16) return "唤醒词最多 16 个字";
  if (/\p{Cc}/u.test(text)) return "唤醒词里不能有控制字符";
  const syllables = String(draft?.command ?? "").trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (syllables.length < 2 || syllables.length > 6) return "拼音需要 2～6 个音节，用空格隔开，例如 ni hao mo si";
  const bad = syllables.find((s) => !/^[a-z]+$/.test(s));
  if (bad) return `“${bad}”不是拼音音节（只写字母、不带声调）`;
  const threshold = Number(draft?.threshold);
  if (!Number.isFinite(threshold) || threshold < 0.05 || threshold > 0.95) return "灵敏度阈值须在 0.05～0.95 之间";
  return "";
}

/**
 * “测试全部环节”（2026-11-08 用户要求：链路栏右上角一键测试，哪个环节不通就在哪里报错）。
 * 按每个环节**当前选用**的来源决定怎么测，只用已保存的配置（密钥留空由桌面端沿用已保存的）：
 * - `static`：无需请求，直接给出结论（内置能量判停、Jev 已关闭、本地模型就绪 / 未下载、云端未配置等）；
 * - `section`：调用 `voice_builtin_models_test(section, payload)` 真实请求一次云端识别 / 大模型 / 合成；
 * - `jev`：调用 `voice_builtin_models_test("jev", {model})`，云端用已保存密钥、本地会先确认推理进程已启动。
 * 本地识别 / 合成 / 判停没有推理测试接口，只检查模型是否已下载、本地模型程序是否存在，文案如实说明。
 */
export function pipelineTestPlan(status) {
  const helperFound = status?.local?.helper_found !== false;
  const localStep = (key, label, view) => {
    const model = view.selectedLocalModel;
    if (!model?.installed) {
      return { key, kind: "static", ok: false, detail: `本地${label}模型“${model?.label || "未选择"}”还没下载：到下方“本地模型”下载，或在上方改回${key === "vad" ? "内置能量判停" : "云端"}` };
    }
    if (!helperFound) {
      return { key, kind: "static", ok: false, detail: "找不到本地模型程序 moss-voice-local（应与 MOSS 放在同一目录），本地模型无法运行" };
    }
    return { key, kind: "static", ok: true, detail: `本地 ${model.label} 已下载（本地模型只检查是否就绪，不做试跑）` };
  };
  return MIX_SECTIONS.map(({ key, label }) => {
    const cleanLabel = label.replace(/^\d+\.\s*/, "");
    const view = builtinSectionSourceView(key, status);
    if (key === "vad") {
      if (view.source !== "local") return { key, kind: "static", ok: true, detail: "内置能量判停，无需测试" };
      return localStep(key, cleanLabel, view);
    }
    if (key === "jev") {
      if (view.source === "off") return { key, kind: "static", ok: true, skipped: true, detail: "已关闭，跳过" };
      if (view.source === "local") {
        const model = view.selectedLocalModel;
        if (!model?.installed) {
          return { key, kind: "static", ok: false, detail: `本地决策模型“${model?.label || "未选择"}”还没下载：到下方“本地模型”下载，或在上方关闭 Jev` };
        }
        return { key, kind: "jev", input: { model: model.id } };
      }
      if (!view.cloudConfigured) {
        return { key, kind: "static", ok: false, detail: "还没配置 OpenRouter API Key：到下方 Jev 卡片填写，或在上方关闭 Jev" };
      }
      return { key, kind: "jev", input: { api_key: "", model: view.cloudModel } };
    }
    if (view.source === "local") return localStep(key, cleanLabel, view);
    const entry = status?.models?.[key];
    if (!entry?.configured) {
      return { key, kind: "static", ok: false, detail: `云端${cleanLabel}还没配置：到下方“${cleanLabel}”卡片填写并保存${key === "llm" ? "" : "，或在上方改用本地模型"}` };
    }
    const form = builtinModelForm(key, entry);
    const inputError = sectionInputError(key, form);
    if (inputError) return { key, kind: "static", ok: false, detail: `已保存的配置不完整：${inputError}` };
    return { key, kind: "section", section: key, input: sectionPayload(key, form) };
  });
}

/** 把一次测试返回（`TestResult`）或调用异常变成环节结论；`errorText` 由调用方传入（通常是 `voiceErrorText`）。 */
export function pipelineTestOutcome(result, error, errorText = (e) => String(e?.message || e || "测试失败")) {
  if (error) return { ok: false, detail: errorText(error) };
  const ok = Boolean(result?.ok);
  const elapsed = Number(result?.elapsed_ms);
  const timing = Number.isFinite(elapsed) && elapsed > 0 ? `（${elapsed} ms）` : "";
  const detail = String(result?.detail || "").trim() || (ok ? "连接正常" : "测试未通过");
  // 大模型结果自带分段耗时（首次响应 / 思考 / 工具调用，n73），不再重复补总耗时
  const hasTiming = /首次响应/.test(detail);
  return { ok, detail: `${detail}${ok && !hasTiming ? timing : ""}` };
}

/** 汇总一句话：全部通过 / 哪几个环节不通。`results` 为 `{key: {ok, skipped}}`。 */
export function pipelineTestSummary(results) {
  const failed = MIX_SECTIONS.filter(({ key }) => results?.[key] && results[key].ok === false).map(({ label }) => label.replace(/^\d+\.\s*/, ""));
  const passed = MIX_SECTIONS.filter(({ key }) => results?.[key]?.ok && !results[key].skipped).length;
  if (failed.length === 0) return { ok: true, text: `全部环节测试通过（${passed} 项正常${passed < MIX_SECTIONS.length ? "，其余已关闭跳过" : ""}）` };
  return { ok: false, text: `${failed.length} 个环节不通：${failed.join("、")}。原因见对应下拉框下方` };
}

/**
 * 视觉理解的看图来源（n74，2026-11-08）：`local` 本地视觉模型、`dialog` 当前对话大模型、`cloud` 云端视觉模型。
 * 存储上仍是 `use_dialog_llm` 与 `local_model` 两个字段：对话大模型优先，其次本地，否则云端。
 */
export function visionSource(config) {
  if (config?.use_dialog_llm) return "dialog";
  if (config?.local_model) return "local";
  return "cloud";
}

/** 切换看图来源时要改的字段；切到本地时优先选已下载的模型。 */
export function visionSourcePatch(source, config, localModels = []) {
  if (source === "dialog") return { use_dialog_llm: true, local_model: "" };
  if (source === "local") {
    const list = Array.isArray(localModels) ? localModels : [];
    const keep = list.find((item) => item.id === config?.local_model);
    const pick = keep || list.find((item) => item.installed) || list[0];
    return { use_dialog_llm: false, local_model: pick?.id || "" };
  }
  return { use_dialog_llm: false, local_model: "" };
}

/** 本地视觉推理进程的状态说明。 */
export function visionRuntimeText(runtime) {
  switch (runtime?.state) {
    case "ready":
      return `本地视觉模型已就绪${runtime.accelerator ? `（${runtime.accelerator}）` : ""}，照片只在这台电脑上处理。`;
    case "starting":
      return "正在加载本地视觉模型（首次把权重读进内存约十几秒到一分钟）…";
    case "failed":
      return `本地视觉模型启动失败：${runtime.error || "未知原因"}`;
    default:
      return "内置服务启动后会在后台加载本地视觉模型；点“测试模型”会等它加载完再测。";
  }
}

/**
 * 设备全屏画风包状态（2026-11-09）：以设备整包下载完为准，推送了没下载不算“已刷”。
 * `canQuickPack`：彩屏、支持在线更新资源包、还没装好也没在等下载时，显示“一键刷入全屏画风”。
 */
export function screenPackDeviceView(device) {
  const size = Array.isArray(device?.screen_pack_size) ? device.screen_pack_size : null;
  const state = device?.screen_pack_state || (device?.screen_pack ? "installed" : "none");
  const withPack = device?.screen_pack_pending_with_pack !== false;
  let text;
  let tone = "off";
  if (state === "pending_download") {
    text = withPack ? "已推送，等设备下载全屏画风…" : "已推送，等设备下载资源包…";
    tone = "info";
  } else if (state === "download_expired") {
    text = "推送后 30 分钟没下载完：确认设备联网后重新推送";
    tone = "warn";
  } else if (state === "installed") {
    text = "已刷全屏画风，上电就是全屏画面";
    tone = "ok";
  } else if (state === "style_outdated") {
    // 2026-11-09 用户决定：换了画风要重刷，开机待命画面才跟着换
    text = "装的是换画风之前的全屏画风：重刷后开机待命画面才会换";
    tone = "warn";
  } else if (size) {
    text = `彩屏 ${size[0]}×${size[1]}，还没刷全屏画风`;
    tone = "warn";
  } else {
    text = "没报彩屏分辨率，只能推唤醒词";
  }
  const canQuickPack = Boolean(device?.supported && size && state !== "installed" && state !== "pending_download");
  const quickLabel = state === "style_outdated" ? "一键重刷全屏画风" : "一键刷入全屏画风";
  return { state, text, tone, canQuickPack, quickLabel };
}
