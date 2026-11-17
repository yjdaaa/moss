// 创作画板的画布与上次预览本机保存（2026-11-17 用户决定，技术书 r49：“我希望我做的这些设置以及画板文字这些东西
// 都要保持上一次”）。切页面时画板常驻（main.jsx），这里负责关掉 MOSS 再打开也能接着画：
// - 画布：未删除的元素、插入图片的数据（总量不超过 MAX_FILES_CHARS，超了只存元素并标记）与视图位置；
// - 上次预览：任务编号与生成那一刻的画布 / 参数签名，打开后按编号重新读任务，画布和参数没变就不算过期。
// 存储不可用或内容损坏时返回 null，画板照常从空白开始。

export const DRAW_SCENE_KEY = "moss.draw.scene.v1";
export const DRAW_LAST_PREVIEW_KEY = "moss.draw.lastPreview.v1";

/** 图片数据（data URL）总字符数上限，约 3 MB，避免撑满本机存储。 */
export const MAX_FILES_CHARS = 3_000_000;

const APP_STATE_KEYS = ["viewBackgroundColor", "scrollX", "scrollY", "zoom", "gridSize"];

/** 画布 → 要保存的字符串；没有元素时返回 ""（表示清掉保存）。 */
export function serializeDrawScene(elements, appState = {}, files = {}) {
  const live = (Array.isArray(elements) ? elements : []).filter((el) => el && !el.isDeleted);
  if (!live.length) return "";
  const usedFileIds = new Set(live.map((el) => el.fileId).filter(Boolean));
  const keptFiles = {};
  let chars = 0;
  let filesDropped = false;
  for (const id of usedFileIds) {
    const file = files?.[id];
    if (!file || typeof file.dataURL !== "string") continue;
    if (chars + file.dataURL.length > MAX_FILES_CHARS) {
      filesDropped = true;
      continue;
    }
    chars += file.dataURL.length;
    keptFiles[id] = { id, mimeType: file.mimeType, dataURL: file.dataURL, created: file.created };
  }
  const view = {};
  for (const key of APP_STATE_KEYS) {
    if (appState && appState[key] !== undefined) view[key] = appState[key];
  }
  return JSON.stringify({ version: 1, elements: live, appState: view, files: keptFiles, filesDropped });
}

/** 保存的字符串 → Excalidraw `initialData`；损坏或为空时返回 null。 */
export function parseDrawScene(raw) {
  if (typeof raw !== "string" || !raw) return null;
  try {
    const data = JSON.parse(raw);
    if (!data || data.version !== 1 || !Array.isArray(data.elements) || !data.elements.length) return null;
    const appState = data.appState && typeof data.appState === "object" ? { ...data.appState } : {};
    // zoom 在 Excalidraw 里是 { value }
    if (appState.zoom && typeof appState.zoom !== "object") delete appState.zoom;
    return {
      elements: data.elements,
      appState,
      files: data.files && typeof data.files === "object" ? data.files : {},
      filesDropped: Boolean(data.filesDropped),
    };
  } catch {
    return null;
  }
}

/** 上次成功预览 → 字符串。 */
export function serializeLastPreview(workflowId, snap) {
  if (!workflowId || !snap) return "";
  return JSON.stringify({ workflowId, sceneRev: snap.sceneRev, formSig: snap.formSig });
}

/** 字符串 → `{ workflowId, snap }`；无效返回 null。 */
export function parseLastPreview(raw) {
  if (typeof raw !== "string" || !raw) return null;
  try {
    const data = JSON.parse(raw);
    if (!data || typeof data.workflowId !== "string" || !data.workflowId) return null;
    if (typeof data.sceneRev !== "string" || typeof data.formSig !== "string") return null;
    return { workflowId: data.workflowId, snap: { sceneRev: data.sceneRev, formSig: data.formSig } };
  } catch {
    return null;
  }
}

/** 读 / 写本机存储（失败时静默：返回 null / false）。 */
export function readStored(storage, key) {
  try {
    return storage?.getItem?.(key) ?? null;
  } catch {
    return null;
  }
}

export function writeStored(storage, key, value) {
  try {
    if (!storage) return false;
    if (value) storage.setItem(key, value);
    else storage.removeItem(key);
    return true;
  } catch {
    return false;
  }
}
