import {
  chatOnlyGatewayName,
  modelsListKey,
  isDashscope,
  isDoubao,
  ownKeyReusable,
} from "../../voiceState.js";

/** 已读取的服务商模型列表内存缓存（切换标签页或同服务商跨分区时直接复用，点“刷新列表”时强制重读）。 */
const REMOTE_MODELS_CACHE = new Map();

function remoteCacheKey(section, form, entry, peerKeySet = false) {
  const baseKey = modelsListKey(form, entry, peerKeySet, section);
  if (isDoubao(form) || isDashscope(form)) return baseKey;
  const hasKey = Boolean(String(form?.api_key || "").trim() || ownKeyReusable(entry, form, section) || peerKeySet);
  const isClineLlm = section === "llm" && chatOnlyGatewayName(form?.url) === "Cline (api.cline.bot)";
  return hasKey && !isClineLlm ? `shared:${baseKey}` : `${section}:${baseKey}`;
}

export { REMOTE_MODELS_CACHE, remoteCacheKey };
