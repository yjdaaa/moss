// 从语音中心以外的地方跳到语音中心的某一处（2026-11-07 n67：独立安全窗口取消后，
// 全局审批卡的“去审批”改为跳到“隐私与家居 → 家居 → 家居审批”）。
// 只是导航提示：先写进 sessionStorage（目标页面尚未挂载时由它在挂载时读取），再广播事件
// （页面已经打开时立即响应）。不携带任何数据。
const KEY = "moss_voice_focus";
export const VOICE_FOCUS_EVENT = "moss:voice-focus";
export const HOME_APPROVALS = "home-approvals";

export function requestVoiceFocus(target, storage = globalThis.sessionStorage) {
  try {
    storage?.setItem(KEY, target);
  } catch {
    // 存储不可用时只靠事件
  }
  globalThis.dispatchEvent?.(new CustomEvent(VOICE_FOCUS_EVENT, { detail: target }));
}

export function peekVoiceFocus(storage = globalThis.sessionStorage) {
  try {
    return storage?.getItem(KEY) || "";
  } catch {
    return "";
  }
}

export function clearVoiceFocus(target, storage = globalThis.sessionStorage) {
  try {
    if (storage?.getItem(KEY) === target) storage.removeItem(KEY);
  } catch {
    // 忽略
  }
}
