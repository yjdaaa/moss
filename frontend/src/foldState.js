/** 折叠状态记忆：按 id 保存在 localStorage（只是界面偏好，读写失败时退回默认值） */
const PREFIX = "moss_ui_fold_";

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
