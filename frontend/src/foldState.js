/** 折叠状态记忆：按 id 保存在 localStorage（只是界面偏好，读写失败时退回默认值） */
const PREFIX = "moss_ui_fold_";

/** 顶栏右上角状态胶囊（硬件 / 小智 / 审批）隐藏开关：保存在 localStorage，默认 false（不隐藏） */
export const HIDE_TOP_PILLS_KEY = "moss_hide_top_pills";

export function readFoldOpen(id, defaultOpen = true, storage = globalThis.localStorage) {
  try {
    const value = storage?.getItem(PREFIX + id);
    if (value === "open") return true;
    if (value === "closed") return false;
  } catch (_error) {
    // 存储不可用时按默认值显示
  }
  return defaultOpen;
}

export function writeFoldOpen(id, open, storage = globalThis.localStorage) {
  try {
    storage?.setItem(PREFIX + id, open ? "open" : "closed");
  } catch (_error) {
    // 忽略：只影响下次打开时的默认展开状态
  }
}

export function readHideTopPills(storage = globalThis.localStorage) {
  try {
    const value = storage?.getItem(HIDE_TOP_PILLS_KEY);
    return value === "1" || value === "true";
  } catch (_error) {
    return false;
  }
}

export function writeHideTopPills(hidden, storage = globalThis.localStorage) {
  try {
    storage?.setItem(HIDE_TOP_PILLS_KEY, hidden ? "1" : "0");
  } catch (_error) {
    // 忽略：只影响下次打开时的默认显示状态
  }
}
