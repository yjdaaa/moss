/** “导出”分区只在桌面版 Tauri 显示：Android 写入方式不同（SAF），浏览器由自身下载设置决定 */
export function shouldShowExportSettings(win = globalThis.window, nav = globalThis.navigator) {
  const invoke = win?.__TAURI__?.core?.invoke;
  if (!invoke) return false;
  const ua = String(nav?.userAgent || "");
  return !/Android/i.test(ua);
}
