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

export function isDoubao(form) {
  return form?.kind === "doubao";
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

/** 与桌面端一致的最终地址规则：大模型写基础地址，语音识别 / 合成写完整接口地址；豆包留空用默认地址。 */
export function finalUrl(section, url, kind) {
  if (kind === "doubao") return trimEndSlash(url) || DOUBAO_DEFAULTS[section]?.url || "";
  const base = trimEndSlash(url);
  if (!base) return "";
  if (section === "llm") return base.replace(/\/chat\/completions$/, "").replace(/\/+$/, "");
  const suffix = section === "asr" ? "/audio/transcriptions" : "/audio/speech";
  return base.endsWith(suffix) ? base : `${base}${suffix}`;
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
      hint: "内置链路：设备的 OTA 地址应指向本机，地址与设备连接情况见“服务与设备”。",
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

const ERROR_TEXT = {
  voice_unavailable: "语音中心不可用：本机后端未启动。",
  voice_desktop_only: "语音中心仅在 MOSS 桌面端可用。",
  self_hosted_endpoint_missing: "请先在“服务端设置”中填写自建接入点地址。",
  server_not_owned: "正在运行的服务端不是 MOSS 启动的，请在启动它的地方关闭。",
  server_external: "服务端端口已被其他程序占用（外部运行的服务端），MOSS 不会再启动一个。",
  model_test_required: "请先用当前填写的内容测试连接并成功后再保存。",
};

/** 桌面端命令错误（`{code, message}`、Error 或字符串）转为界面文字。 */
export function voiceErrorText(error) {
  if (!error) return "操作失败";
  if (typeof error === "string") return error;
  if (typeof error.message === "string" && error.message) return error.message;
  if (error.code && ERROR_TEXT[error.code]) return ERROR_TEXT[error.code];
  return String(error);
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

/** 密钥输入框的占位文字：已保存时说明留空即沿用，绝不显示密钥本身。 */
export function builtinKeyPlaceholder(entry) {
  return entry?.key_set ? "已设置（留空则沿用已保存的密钥）" : "未设置（本地模型服务可以不填）";
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
  if (progress?.state === "verifying") return { text: "校验中", tone: "info", percent: 100, canDownload: false, canCancel: true, canDelete: false };
  if (progress?.state === "installing") return { text: "安装中", tone: "info", percent: 100, canDownload: false, canCancel: true, canDelete: false };
  if (model?.installed) return { text: "已安装", tone: "ok", percent: null, canDownload: false, canCancel: false, canDelete: true };
  if (progress?.state === "failed") {
    return { text: "下载失败", detail: progress.message || "未知原因", tone: "bad", percent: null, canDownload: true, canCancel: false, canDelete: false };
  }
  if (progress?.state === "cancelled") return { text: "已取消", tone: "off", percent: null, canDownload: true, canCancel: false, canDelete: false };
  return { text: "未下载", tone: "off", percent: null, canDownload: true, canCancel: false, canDelete: false };
}

/** 本地模型的三个类别：开关文字与默认模型（与后端 `BuiltinLocal` 的默认值一致）。 */
export const LOCAL_KINDS = [
  { kind: "vad", title: "判停", toggle: "自动拾音判停用本地模型（否则用能量判停）", fallback: "silero_vad" },
  { kind: "asr", title: "语音识别", toggle: "语音识别用本地模型（代替云端识别）", fallback: "sense_voice" },
  { kind: "tts", title: "语音合成", toggle: "语音合成用本地模型（代替云端合成）", fallback: "kokoro" },
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
    custom_hotwords: (Array.isArray(value.custom_hotwords) ? value.custom_hotwords : []).join("\n"),
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
    custom_hotwords: [...new Set(String(form.custom_hotwords || "").split(/\r?\n/).map((word) => word.trim()).filter(Boolean))],
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
  interim_stop: "中间结果命中停止", final_asr: "最终识别", fast_route: "快捷路由",
  first_llm_delta: "模型首字", first_sentence: "第一句切出", first_synthesis_start: "首次合成开始",
  first_synthesis_ready: "首段音频就绪", first_audio_frame: "首帧发给设备", turn_end: "本轮结束",
};

/** 复制只取诊断字段；不复制完整服务状态、对话或模型地址。 */
export function voiceTimingReport(timings) {
  return (Array.isArray(timings) ? timings : []).slice(0, 20).map((item) => ({
    turn: item.turn, outcome: item.outcome,
    models: (Array.isArray(item.models) ? item.models : []).map(String),
    stages: Object.fromEntries(Object.keys(VOICE_TIMING_STAGES).filter((key) => Number.isFinite(item.stages?.[key])).map((key) => [key, item.stages[key]])),
    tools: (Array.isArray(item.tools) ? item.tools : []).slice(0, 50).map(({ name, ms }) => ({ name, ms })),
  }));
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

/** 工具调用统计的简短文字。 */
export function toolStatText(tool) {
  const calls = Number(tool?.calls || 0);
  if (!calls) return "本次运行未调用";
  const parts = [`调用 ${calls} 次`];
  if (tool.failures) parts.push(`失败 ${tool.failures}`);
  if (tool.denied) parts.push(`拒绝 ${tool.denied}`);
  return parts.join(" · ");
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

/** 页面标签。所有标签内容始终挂载、只隐藏，切换标签不会丢失未保存的填写。 */
export const VOICE_TABS = [
  { key: "service", label: "服务与设备" },
  { key: "models", label: "模型" },
  { key: "connect", label: "连接" },
  { key: "features", label: "功能扩展" },
  { key: "records", label: "记录与诊断" },
];

export const VOICE_MODE_LABEL = { official: "官方小智", self_hosted: "自建服务端", builtin: "MOSS 内置服务" };

/**
 * 链路与服务端设置拆成独立保存的分组：每组只提交自己的字段（后端按字段合并，见 center.rs save_settings）。
 * `secret` 字段带令牌，不回填；填了才算改动，留空表示不改。
 */
export const SETTINGS_GROUPS = {
  official: { mode: "official", title: "官方接入点", keys: ["official_mcp_endpoint"], secret: ["official_mcp_endpoint"] },
  selfHostedServer: { mode: "self_hosted", title: "自建服务端程序", keys: ["server_dir", "python", "args", "server_port"], secret: [] },
  selfHostedEndpoint: {
    mode: "self_hosted",
    title: "自建接入点",
    keys: ["self_hosted_mcp_endpoint", "endpoint_health_url"],
    secret: ["self_hosted_mcp_endpoint", "endpoint_health_url"],
  },
  builtinPorts: { mode: "builtin", title: "内置服务端口", keys: ["builtin_ws_port", "builtin_http_port"], secret: [] },
};

const PORT_KEYS = new Set(["server_port", "builtin_ws_port", "builtin_http_port"]);

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
    } else if (PORT_KEYS.has(key)) {
      const port = Number(value);
      payload[key] = Number.isFinite(port) ? port : 0;
    } else {
      payload[key] = value;
    }
  }
  return payload;
}

/** 本地模型设置拆成两组分别保存：模型选择与语音体验。 */
export const LOCAL_MODEL_KEYS = ["vad", "vad_model", "asr", "asr_model", "tts", "tts_model", "tts_speaker", "tts_speed", "mirror"];
export const EXPERIENCE_KEYS = ["read_only_shortcuts", "asr_beam_search", "barge_in", "barge_in_ms", "custom_hotwords"];

/** 只取草稿中本组字段，其余用已保存的值，避免保存一组时顺带提交另一组未保存的改动。 */
export function mergeGroup(baseline, draft, keys) {
  const next = { ...baseline };
  for (const key of keys) next[key] = draft[key];
  return next;
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
