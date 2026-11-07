// 加工反馈记录的分类来源文字（2026-11-07：本机 Jev 与 OpenRouter 分开显示）。
export function feedbackSourceLabel(entry) {
  if (entry?.source === "openrouter") return "OpenRouter Jev";
  if (entry?.source === "local_jev") {
    const model = typeof entry.decision?.model === "string" ? entry.decision.model.trim() : "";
    return model ? `本机 Jev · ${model}` : "本机 Jev";
  }
  return "本地规则";
}
