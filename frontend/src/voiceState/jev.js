// Jev 判停模型：云端与本地型号归一、表单、摘要与保存载荷。

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
