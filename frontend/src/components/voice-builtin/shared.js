import { focusVoiceGroup, gotoVoiceGroup } from "../VoiceLayout.jsx";
import { isFoldedLocalModel, restartEffect } from "../../voiceState.js";

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

// 2026-11-25：顶部下拉选了还没下载的本地模型时，直接跳到这个模型那一行、聚焦它的“下载”按钮并高亮一下，
// 不再只展开整个本地模型分组让用户自己找。行或按钮暂不存在（如正在下载只剩“取消”）时退回到 `fallback`。
const LOCAL_ROW_FLASH = "voice-local-row--flash";
function focusLocalModel(id, fallback = "") {
  if (isFoldedLocalModel(id)) revealLocalMore();
  const row = id ? document.getElementById(`voice-local-row-${id}`) : null;
  const button = id ? document.getElementById(`voice-local-download-${id}`) : null;
  focusVoiceGroup("local-models", button?.id || row?.id || fallback);
  // 分组原本收起时按钮还在 hidden 里，focus 静默失败；展开渲染完成前多试几次，用户已把焦点移走就不再抢
  if (button) {
    let tries = 0;
    const retry = () => {
      if (document.activeElement === button || tries++ >= 8) return;
      const active = document.activeElement;
      if (active && active !== document.body && !active.id?.startsWith("voice-builtin-mix-select-")) return;
      if (button.isConnected && button.offsetParent !== null && !button.disabled) {
        button.scrollIntoView?.({ behavior: "smooth", block: "center" });
        button.focus({ preventScroll: true });
        return;
      }
      globalThis.setTimeout?.(retry, 50);
    };
    globalThis.setTimeout?.(retry, 0);
  }
  if (!row) return;
  row.classList.remove(LOCAL_ROW_FLASH);
  void row.offsetWidth; // 连续选两次同一个模型时让高亮动画重新播放
  row.classList.add(LOCAL_ROW_FLASH);
  globalThis.setTimeout?.(() => row.classList.remove(LOCAL_ROW_FLASH), 2600);
}

function formatTime(seconds) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

export { JEV_GROUP_TITLE, focusJevSwitch, TONE_CLASS, LAYA_HINT_CLASS, formatTime, restartEffect, LOCAL_MORE_REVEAL_EVENT, revealLocalMore, consumeLocalMoreReveal, focusLocalModel };
