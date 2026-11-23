import { gotoVoiceGroup } from "../VoiceLayout.jsx";
import { restartEffect } from "../../voiceState.js";

/** Jev 云端配置分组的标题：顶部混搭栏的提示与对话体验里的跳转文字都引用它，改名时只改这里。 */
const JEV_GROUP_TITLE = "Jev 前置粗筛 · OpenRouter 云端（高级）";

/** 跳到“模型”标签顶部第 3 栏 Jev 下拉框：Jev 开关只在那里切换（n69 去重）。 */
function focusJevSwitch() {
  gotoVoiceGroup("models", "", "voice-builtin-mix-select-jev");
}

const TONE_CLASS = { ok: "ob-pill--ok", info: "ob-pill--info", warn: "ob-pill--warn", bad: "ob-pill--bad", off: "ob-pill--off" };
const LAYA_HINT_CLASS = { ok: "voice-hint--ok", warn: "voice-hint--warn", bad: "voice-hint--bad" };

// 本地模型页“更多 / 实验”折叠区（n109）：顶部下拉选了收起区里还没下载的模型时先展开再定位。
// 本地模型分组可能还没挂载，所以既发事件、也留一个挂载时读取的标记。
const LOCAL_MORE_REVEAL_EVENT = "moss:local-models-more";
let localMoreRevealPending = false;
function revealLocalMore() {
  localMoreRevealPending = true;
  globalThis.dispatchEvent?.(new Event(LOCAL_MORE_REVEAL_EVENT));
}
function consumeLocalMoreReveal() {
  const pending = localMoreRevealPending;
  localMoreRevealPending = false;
  return pending;
}

function formatTime(seconds) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

export { JEV_GROUP_TITLE, focusJevSwitch, TONE_CLASS, LAYA_HINT_CLASS, formatTime, restartEffect, LOCAL_MORE_REVEAL_EVENT, revealLocalMore, consumeLocalMoreReveal };
