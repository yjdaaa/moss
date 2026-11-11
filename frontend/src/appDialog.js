/**
 * 应用内对话框状态中心（替代原生 window.alert / window.confirm）。
 *
 * - 纯逻辑模块，不依赖 React，便于单元测试。
 * - 通过 `subscribe` 由 <AppDialogHost /> 订阅当前对话框；
 *   `showAlert` / `showConfirm` 返回 Promise，调用方可以 await 用户操作。
 * - 未挂载 Host 时（例如极早期启动或纯脚本环境）自动降级为原生弹窗，
 *   保证提示不会静默丢失。
 */

const TONES = new Set(["info", "success", "warning", "error"]);

const listeners = new Set();
const queue = [];
let seq = 0;
let hostMounted = false;

function notify() {
  const current = getCurrentDialog();
  listeners.forEach((fn) => {
    try {
      fn(current);
    } catch (err) {
      console.error("appDialog listener error", err);
    }
  });
}

export function normalizeTone(tone) {
  return TONES.has(tone) ? tone : "info";
}

/**
 * 兼容历史写法：`【发送失败 · 设备未连接】\n\n正文...`
 * 将首行的【】标题拆出，返回 { title, body }。
 */
export function parseLegacyMessage(text) {
  const raw = text == null ? "" : String(text);
  const match = raw.match(/^\s*【([^】]+)】\s*\n*([\s\S]*)$/);
  if (match) {
    return { title: match[1].trim(), body: match[2].trim() };
  }
  return { title: "", body: raw.trim() };
}

export function getCurrentDialog() {
  return queue.length ? queue[0] : null;
}

export function subscribe(listener) {
  listeners.add(listener);
  hostMounted = true;
  listener(getCurrentDialog());
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) hostMounted = false;
  };
}

export function isHostMounted() {
  return hostMounted;
}

/**
 * 通用入口。options:
 *  - kind: "alert" | "confirm" | "choice"（choice 区分“确认 / 取消按钮 / 直接关闭”三种结果）
 *  - title, message, tone
 *  - confirmText, cancelText
 *  - danger: 确认按钮是否使用危险样式
 */
export function showDialog(options = {}) {
  const kind =
    options.kind === "confirm" || options.kind === "choice" || options.kind === "prompt"
      ? options.kind
      : "alert";
  const parsed = parseLegacyMessage(options.message);
  const tone = normalizeTone(options.tone);
  const title = (options.title || parsed.title || defaultTitle(kind, tone)).trim();
  const message = options.title ? String(options.message ?? "").trim() : parsed.body;
  const defaultValue = options.defaultValue == null ? "" : String(options.defaultValue);
  const placeholder = options.placeholder == null ? "" : String(options.placeholder);

  if (!hostMounted) {
    return Promise.resolve(fallbackNative(kind, title, message, defaultValue));
  }

  return new Promise((resolve) => {
    queue.push({
      id: ++seq,
      kind,
      tone,
      title,
      message,
      defaultValue,
      placeholder,
      confirmText: options.confirmText || "确定",
      cancelText: options.cancelText || "取消",
      danger: Boolean(options.danger),
      resolve,
    });
    if (queue.length === 1) notify();
  });
}

export function showAlert(message, options = {}) {
  return showDialog({ ...options, kind: "alert", message }).then(() => undefined);
}

export function showConfirm(message, options = {}) {
  return showDialog({ ...options, kind: "confirm", message });
}

export function showPrompt(message, options = {}) {
  return showDialog({ ...options, kind: "prompt", message });
}

/**
 * 两个动作 + 关闭：返回 "confirm"（确认按钮）、"cancel"（取消按钮，可作为第二个动作）或 "dismiss"（× / Esc）。
 */
export function showChoice(message, options = {}) {
  return showDialog({ ...options, kind: "choice", message });
}

function choiceValue(value) {
  if (value === true) return "confirm";
  if (value === false) return "cancel";
  return "dismiss";
}

/** Host 在用户点击后调用；alert 固定 resolve(true)，confirm 传入布尔值。 */
export function resolveDialog(id, value) {
  const index = queue.findIndex((d) => d.id === id);
  if (index === -1) return false;
  const [dialog] = queue.splice(index, 1);
  dialog.resolve(
    dialog.kind === "confirm"
      ? Boolean(value)
      : dialog.kind === "choice"
        ? choiceValue(value)
        : dialog.kind === "prompt"
          ? typeof value === "string"
            ? value
            : null
          : true,
  );
  notify();
  return true;
}

/** 测试与热重载用：清空所有挂起对话框（全部按取消处理）。 */
export function resetDialogs() {
  while (queue.length) {
    const dialog = queue.shift();
    dialog.resolve(
      dialog.kind === "confirm"
        ? false
        : dialog.kind === "choice"
          ? "dismiss"
          : dialog.kind === "prompt"
            ? null
            : true,
    );
  }
  notify();
}

function defaultTitle(kind, tone) {
  if (kind === "confirm") return "请确认";
  if (kind === "prompt") return "请输入";
  switch (tone) {
    case "error":
      return "操作失败";
    case "warning":
      return "注意";
    case "success":
      return "操作成功";
    default:
      return "提示";
  }
}

/**
 * 原生降级。桌面端 Tauri 的对话框插件把 window.confirm 换成了返回 Promise 的异步函数，
 * 直接当布尔值用会永远判为“确认”（2026-10-15 实机发现）；所以一律 await，出错按取消处理。
 */
async function fallbackNative(kind, title, message, defaultValue = "") {
  const text = title && message ? `${title}\n\n${message}` : title || message;
  if (typeof window === "undefined") {
    return kind === "confirm" ? false : kind === "choice" ? "dismiss" : kind === "prompt" ? null : true;
  }
  if (kind === "prompt") {
    if (typeof window.prompt !== "function") return null;
    try {
      const val = await window.prompt(text, defaultValue);
      return typeof val === "string" ? val : null;
    } catch (_error) {
      return null;
    }
  }
  const ask = async () => {
    if (typeof window.confirm !== "function") return null;
    try {
      return (await window.confirm(text)) === true;
    } catch (_error) {
      return null;
    }
  };
  if (kind === "confirm") return (await ask()) === true;
  if (kind === "choice") {
    const answer = await ask();
    return answer === null ? "dismiss" : answer ? "confirm" : "cancel";
  }
  try {
    if (typeof window.alert === "function") await window.alert(text);
  } catch (_error) {
    // 提示失败不影响流程
  }
  return true;
}
