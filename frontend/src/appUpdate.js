/**
 * 应用内更新（2026-11-07 用户决定）：检查到新版后，在软件内下载、校验签名并安装（桌面端自动重启，Android 拉起系统安装界面确认）。
 * 命令只在 MOSS 客户端受信任主窗口可用（Rust 侧 app_update 模块校验来源）；网页或手机远程连接电脑模式不显示按钮。
 */
function tauriInvoke() {
  return typeof window !== "undefined" ? window.__TAURI__?.core?.invoke : undefined;
}

export function appUpdateAvailable() {
  return Boolean(tauriInvoke());
}

async function call(command, args) {
  const invoke = tauriInvoke();
  if (!invoke) throw new Error("应用内更新仅在 MOSS 客户端可用");
  try {
    return await invoke(command, args);
  } catch (raw) {
    throw new Error(typeof raw === "string" ? raw : raw?.message || "操作失败");
  }
}

export const appUpdateStatus = () => call("app_update_status");
export const appUpdateInstall = (tag) => call("app_update_install", { tag });

/** 进行中的阶段：轮询直到离开这些阶段。 */
export function appUpdateBusy(progress) {
  return ["checking", "downloading", "installing", "restarting"].includes(progress?.phase);
}

/**
 * 只保留正在执行中的进度，或与当前展示的 `latestVersion` 一致的完成/错误状态；
 * 切换版本检查结果（例如开/关预览版发现更新号变化）时自动丢弃旧版本的残留状态，防止跳过确认弹窗或误报。
 */
export function appUpdateScopeProgress(progress, latestVersion) {
  if (!progress) return null;
  if (appUpdateBusy(progress)) return progress;
  const pv = String(progress.version || "").trim().replace(/^[vV]/, "");
  const lv = String(latestVersion || "").trim().replace(/^[vV]/, "");
  return pv && lv && pv === lv ? progress : null;
}

/** 更新按钮主文案（按平台与当前阶段切换）。 */
export function appUpdateActionLabel(installInfo, progress) {
  if (appUpdateBusy(progress)) return "正在更新…";
  const isAndroid = installInfo?.platform === "android";
  if (progress?.phase === "installer_launched") {
    return "重新打开安装界面";
  }
  if (progress?.phase === "error") {
    return isAndroid ? "重试下载并安装" : "重试下载并更新";
  }
  return isAndroid ? "下载并安装更新" : "下载并重启更新";
}

function mb(bytes) {
  return (Number(bytes) / 1048576).toFixed(1);
}

/** 进度文字与百分比（`percent` 为 null 表示不确定进度）。 */
export function appUpdateProgressView(progress) {
  const phase = progress?.phase || "idle";
  const downloaded = Number(progress?.downloaded) || 0;
  const total = Number(progress?.total) || 0;
  switch (phase) {
    case "checking":
      return { text: "正在获取更新包信息…", percent: null };
    case "downloading": {
      const percent = total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : null;
      return { text: total > 0 ? `正在下载 ${mb(downloaded)} / ${mb(total)} MB` : `正在下载 ${mb(downloaded)} MB`, percent };
    }
    case "installing":
      return { text: "下载完成，签名校验通过，正在安装…", percent: 100 };
    case "restarting":
      return { text: "安装完成，MOSS 正在重启…", percent: 100 };
    case "installer_launched":
      return { text: "已打开系统安装界面，请在系统弹窗中确认安装（如刚授予安装权限，可再次点击上方按钮直接安装）", percent: 100 };
    case "error":
      return { text: progress?.error || "更新失败", percent: null };
    default:
      return { text: "", percent: null };
  }
}
