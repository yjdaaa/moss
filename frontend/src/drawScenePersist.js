import { computeSceneRevision } from "./drawConnection.js";

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

/** 区分“后端没有存档”和“后端保存了清空”，两者到画布就绪后才应用。 */
export function drawSceneToRestore(raw) {
  if (raw === "") return { elements: [], appState: {}, files: {} };
  return parseDrawScene(raw);
}

export function takePendingDrawScene(pending, { apiReady, canvasReady, sceneDirty }) {
  return apiReady && canvasReady && !sceneDirty ? pending : null;
}

export function snapshotDrawScene(elements, appState = {}, files = {}) {
  const referencedFiles = new Map();
  for (const element of elements || []) {
    if (element?.isDeleted || !element?.fileId) continue;
    const file = files?.[element.fileId];
    if (typeof file?.dataURL === "string") {
      referencedFiles.set(element.fileId, { dataURL: file.dataURL, mimeType: file.mimeType });
    }
  }
  return {
    sceneRev: computeSceneRevision(elements),
    referencedFiles,
    view: APP_STATE_KEYS.map((key) => key === "zoom" ? appState?.zoom?.value : appState?.[key]),
  };
}

export function drawSceneChanges(previous, next) {
  const content = previous.sceneRev !== next.sceneRev
    || previous.referencedFiles.size !== next.referencedFiles.size
    || [...next.referencedFiles].some(([id, file]) => {
      const old = previous.referencedFiles.get(id);
      return old?.dataURL !== file.dataURL || old.mimeType !== file.mimeType;
    });
  return { content, view: next.view.some((value, index) => value !== previous.view[index]) };
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

/**
 * 判断是否应当持久化 `DRAW_LAST_PREVIEW_KEY`（#23）：
 * 恢复查询尚未落定前，`workflowId` 与 `previewSnap` 仍是初始空值，不得提前把旧指针写空；
 * 一旦恢复完成，或用户主动生成/采纳了新预览，才写入或清空。
 */
export function shouldPersistLastPreview({ workflowId, previewSnap, lastPreviewHydrated }) {
  if (workflowId && previewSnap) return true;
  return Boolean(lastPreviewHydrated);
}

/**
 * 判断迟到的恢复查询结果是否仍可写入实时状态（#23）：
 * 组件已卸载或期间用户已新建/采纳预览时丢弃。
 */
export function shouldApplyRestoredPreview({ cancelled, requestSeq, currentSeq }) {
  return !cancelled && requestSeq === currentSeq;
}

/**
 * 合并后端持久化的画板状态（`GET /api/draw/state`）与当前 origin 的 `localStorage`（#24）：
 * 后端已保存时以应用数据目录为准（不覆盖本轮用户已主动改动的画布/表单/预览），
 * 后端为空而本地 `localStorage` 有旧数据时安排一次性回填。
 */
export function planDrawHydration({
  remoteState = null,
  localSceneRaw = null,
  localLastPreviewRaw = null,
  localForm = {},
  localFingerprint = "",
  configKeys = [],
  sceneDirty = false,
  dirtyFormKeys = new Set(),
  previewActionSeq = 0,
} = {}) {
  const remote = remoteState && typeof remoteState === "object" ? remoteState : null;
  const backfill = {};

  let sceneToApply = null;
  if (!sceneDirty) {
    if (typeof remote?.scene === "string") {
      sceneToApply = remote.scene;
      const remoteScene = parseDrawScene(remote.scene);
      const localScene = parseDrawScene(localSceneRaw);
      if (remoteScene && localScene && !drawSceneChanges(
        snapshotDrawScene(remoteScene.elements, remoteScene.appState, remoteScene.files),
        snapshotDrawScene(localScene.elements, localScene.appState, localScene.files),
      ).content) {
        sceneToApply = localSceneRaw;
      }
    } else if (typeof localSceneRaw === "string" && localSceneRaw) {
      backfill.scene = localSceneRaw;
    }
  }

  let lastPreviewRaw = null;
  if (previewActionSeq === 0) {
    if (typeof remote?.last_preview === "string") {
      lastPreviewRaw = remote.last_preview;
    } else if (typeof localLastPreviewRaw === "string" && localLastPreviewRaw) {
      lastPreviewRaw = localLastPreviewRaw;
      backfill.last_preview = localLastPreviewRaw;
    }
  }

  const remoteForm = remote?.form && typeof remote.form === "object" ? remote.form : {};
  const formUpdates = {};
  const backfillForm = {};
  for (const key of configKeys) {
    if (typeof remoteForm[key] === "string") {
      if (!dirtyFormKeys.has(key)) {
        formUpdates[key] = remoteForm[key];
      }
    } else if (typeof localForm?.[key] === "string" && localForm[key] !== "") {
      backfillForm[key] = localForm[key];
    }
  }
  if (Object.keys(backfillForm).length > 0) {
    backfill.form = backfillForm;
  }

  let fingerprintUpdate = null;
  if (typeof remote?.last_accepted_fingerprint === "string") {
    fingerprintUpdate = remote.last_accepted_fingerprint;
  } else if (typeof localFingerprint === "string" && localFingerprint) {
    backfill.last_accepted_fingerprint = localFingerprint;
  }

  return {
    sceneToApply,
    lastPreviewRaw,
    formUpdates,
    fingerprintUpdate,
    backfill,
  };
}
