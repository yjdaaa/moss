/**
 * 手机 App 的“连接电脑 MOSS”模式（技术书 §9.8）。
 *
 * 只记一个电脑地址（形如 `http://192.168.1.20:9666`）。设置后，手机 App 不再使用自己内置的后端，
 * 所有请求发往这台电脑；电脑是唯一的数据源，手机上材料、校准参数与系统设置只读（后端强制）。
 */
export const REMOTE_HOST_KEY = "moss.remoteHost";
export const DEFAULT_PORT = 9666;

function defaultStorage() {
  try {
    return globalThis.localStorage ?? null;
  } catch (_error) {
    return null;
  }
}

/**
 * 把用户输入规范为 `http://主机:端口`；不合法时返回 `null`。
 * 接受 `192.168.1.20`、`192.168.1.20:9666`、`http://192.168.1.20:9666/`、局域网主机名。
 * 拒绝 https（电脑端只提供 http）、回环地址（那是手机自己）、带路径或账号的地址。
 */
export function normalizeRemoteOrigin(input) {
  if (typeof input !== "string") return null;
  let text = input.trim();
  if (!text) return null;
  if (/^https:\/\//i.test(text)) return null;
  text = text.replace(/^http:\/\//i, "").replace(/\/+$/, "");
  if (!text || /[\/@?#\s]/.test(text)) return null;
  let host = text;
  let port = DEFAULT_PORT;
  const match = text.match(/^(.+):(\d{1,5})$/);
  if (match) {
    host = match[1];
    port = Number(match[2]);
  }
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null;
  host = host.toLowerCase();
  const ipv4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (ipv4) {
    const parts = ipv4.slice(1).map(Number);
    if (parts.some((part) => part > 255)) return null;
    if (parts[0] === 127 || parts[0] === 0) return null;
  } else if (!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?)*$/.test(host) || host === "localhost") {
    return null;
  }
  return `http://${host}:${port}`;
}

export function readRemoteOrigin(storage = defaultStorage()) {
  try {
    const value = storage?.getItem(REMOTE_HOST_KEY) || "";
    return normalizeRemoteOrigin(value);
  } catch (_error) {
    return null;
  }
}

export function saveRemoteOrigin(origin, storage = defaultStorage()) {
  const normalized = normalizeRemoteOrigin(origin);
  if (!normalized) throw new Error("电脑地址格式不正确，例如 192.168.1.20:9666");
  storage?.setItem(REMOTE_HOST_KEY, normalized);
  return normalized;
}

export function clearRemoteOrigin(storage = defaultStorage()) {
  try {
    storage?.removeItem(REMOTE_HOST_KEY);
  } catch (_error) {
    // 存储不可用时视为已清除
  }
}

/** 是否运行在 Android（Tauri 内嵌页或手机浏览器）。 */
export function isAndroidDevice(nav = globalThis.navigator) {
  return /Android/i.test(nav?.userAgent || "");
}

/**
 * 探测电脑 MOSS 是否可达：`GET /api/health` 为公开接口，不需要配对。
 * 超时或失败时抛出带原因的错误。
 */
export async function probeRemoteOrigin(origin, fetchImpl = globalThis.fetch, timeoutMs = 5000) {
  const controller = typeof AbortController === "function" ? new AbortController() : null;
  const timer = controller ? setTimeout(() => controller.abort(), timeoutMs) : null;
  try {
    const response = await fetchImpl(`${origin}/api/health`, {
      method: "GET",
      headers: { Accept: "application/json" },
      signal: controller?.signal,
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return true;
  } catch (error) {
    const reason = error?.name === "AbortError" ? "连接超时" : (error?.message || String(error));
    throw new Error(
      `无法连接电脑 MOSS（${origin}）：${reason}。请确认电脑上的 MOSS 已打开、已在“偏好设置 → 手机连接”中允许手机连接并重启过，且手机与电脑在同一个 WiFi。`,
    );
  } finally {
    if (timer) clearTimeout(timer);
  }
}
