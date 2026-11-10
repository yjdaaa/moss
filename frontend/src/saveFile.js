/**
 * 统一的“导出 / 下载文件”（技术书 §9.7）。
 *
 * 桌面端与 Android 内嵌页（Tauri）里，网页的 `<a download>` 不可靠，所以交给宿主命令
 * `save_export_file`：写入用户在偏好设置里选的导出目录（未设置时为“下载”文件夹），返回真实路径；
 * 开启“每次询问”时宿主会弹“另存为”框，用户取消返回 null。普通浏览器里才用 `<a download>`。
 */
import { showAlert, showChoice } from "./appDialog.js";

export const EXPORT_DIR_UNAVAILABLE = "export_dir_unavailable:";

/** 自定义导出目录不可用（U 盘拔出、网络盘断开、权限变更等）。不会自动改存别处。 */
export class ExportDirUnavailableError extends Error {
  constructor(dir, reason) {
    super(`导出目录不可用：${dir}${reason ? `（${reason}）` : ""}`);
    this.name = "ExportDirUnavailableError";
    this.code = "export_dir_unavailable";
    this.dir = dir;
    this.reason = reason || "";
  }
}

export function tauriInvoke(win = globalThis.window) {
  return win?.__TAURI__?.core?.invoke || null;
}

/** 解析宿主返回的 `export_dir_unavailable:<路径>|<原因>`；不是这种错误时返回 null */
export function parseExportDirUnavailable(error) {
  const text = typeof error === "string" ? error : error?.message;
  if (typeof text !== "string" || !text.startsWith(EXPORT_DIR_UNAVAILABLE)) return null;
  const rest = text.slice(EXPORT_DIR_UNAVAILABLE.length);
  const bar = rest.lastIndexOf("|");
  return bar === -1 ? { dir: rest, reason: "" } : { dir: rest.slice(0, bar), reason: rest.slice(bar + 1) };
}

/**
 * 保存一个文件。返回 { path, cancelled }：
 * - Tauri：path 为实际保存路径；用户在“另存为”框里取消时 path=null、cancelled=true；
 * - 浏览器：path=null（由浏览器下载）。
 * 自定义目录不可用时抛 ExportDirUnavailableError；options.fallbackDefault=true 表示本次改存默认目录。
 */
export async function saveFile(blob, filename, win = globalThis.window, options = {}) {
  const invoke = tauriInvoke(win);
  if (invoke) {
    const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()));
    let path;
    try {
      path = await invoke("save_export_file", {
        filename,
        bytes,
        fallbackDefault: Boolean(options.fallbackDefault),
      });
    } catch (error) {
      const unavailable = parseExportDirUnavailable(error);
      if (unavailable) throw new ExportDirUnavailableError(unavailable.dir, unavailable.reason);
      throw error instanceof Error ? error : new Error(String(error));
    }
    if (typeof path !== "string") return { path: null, cancelled: true };
    return { path, cancelled: false };
  }
  const doc = win?.document;
  if (!doc) throw new Error("当前环境无法保存文件");
  const url = URL.createObjectURL(blob);
  const anchor = doc.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.style.display = "none";
  doc.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  // 立即回收会让部分浏览器的下载失败，延迟回收
  setTimeout(() => URL.revokeObjectURL(url), 30000);
  return { path: null, cancelled: false };
}

/** 导出结果的提示文字（用于页面状态行） */
export function savedMessage(label, result) {
  if (result?.cancelled) return "已取消导出";
  return result?.path ? `${label}已保存到：${result.path}` : `${label}已导出（见浏览器下载）`;
}

/**
 * 带交互的导出：处理“目录不可用”（重新选择 / 本次保存到下载文件夹）、取消，
 * 成功后提示路径并可“打开所在文件夹”。返回 saveFile 的结果；失败时抛出错误由调用方提示。
 * deps 便于测试注入。
 */
export async function exportFile(blob, filename, label, deps = {}) {
  const win = deps.win ?? globalThis.window;
  const choice = deps.showChoice ?? showChoice;
  const notifyAlert = deps.showAlert ?? showAlert;
  const invoke = tauriInvoke(win);

  let result;
  try {
    result = await saveFile(blob, filename, win);
  } catch (error) {
    if (!(error instanceof ExportDirUnavailableError) || !invoke) throw error;
    const action = await choice(
      `${error.message}\n\n目录不会被自动创建，文件也不会悄悄存到别处。`,
      {
        title: "导出目录不可用",
        tone: "warning",
        confirmText: "本次保存到下载文件夹",
        cancelText: "重新选择",
      },
    );
    if (action === "confirm") {
      result = await saveFile(blob, filename, win, { fallbackDefault: true });
    } else if (action === "cancel") {
      const picked = await invoke("pick_export_dir");
      if (!picked) return { path: null, cancelled: true };
      result = await saveFile(blob, filename, win);
    } else {
      return { path: null, cancelled: true };
    }
  }

  if (result.cancelled) {
    await notifyAlert("已取消导出", { title: "导出", tone: "info" });
    return result;
  }
  if (result.path && invoke) {
    const action = await choice(`${label}已保存到：\n${result.path}`, {
      title: "导出完成",
      tone: "success",
      confirmText: "打开所在文件夹",
      cancelText: "关闭",
    });
    if (action === "confirm") {
      try {
        await invoke("reveal_exported_file", { path: result.path });
      } catch (_error) {
        // 打不开文件夹不影响导出结果
      }
    }
  }
  return result;
}
