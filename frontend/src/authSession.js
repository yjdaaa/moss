import { probeRemoteOrigin, readRemoteOrigin } from "./remoteHost.js";

export const AUTH_TOKEN_KEY = "laserLab.authToken";
export const DESKTOP_SESSION_HEADER = "X-Moss-Desktop-Session";

let backendOrigin = null;
// 手机 App 连接电脑 MOSS（技术书 §9.8）：请求发往电脑，不使用本机内置后端
let remoteMode = false;
// 桌面会话（技术书 §5.3 / §9.5.4）：只在内存，不写 sessionStorage / localStorage，不打印。
let desktopSession = "";

export async function initializeBackendOrigin(options = {}) {
  backendOrigin = null;
  remoteMode = false;
  if (typeof window === "undefined") return;
  const nativeAsset = window.location?.hostname === "tauri.localhost" || window.location?.protocol === "tauri:";
  const invoke = window.__TAURI__?.core?.invoke;
  if (!invoke) {
    if (nativeAsset) throw new Error("无法读取本机后端地址");
    return;
  }
  // 只有 App 内嵌页（Tauri）才支持连接电脑；先设模式再探测，探测失败时启动页据此提供“改用本机”
  const remote = (options.readRemote ?? readRemoteOrigin)();
  if (remote) {
    backendOrigin = remote;
    remoteMode = true;
    await (options.probe ?? probeRemoteOrigin)(remote);
    return;
  }
  const origin = await invoke("get_backend_origin");
  const match = typeof origin === "string" && origin.match(/^http:\/\/127\.0\.0\.1:([1-9]\d{0,4})$/);
  if (!match || Number(match[1]) > 65535) throw new Error("本机后端地址无效");
  backendOrigin = origin;
}

/** 当前是否为“连接电脑 MOSS”模式。 */
export function isRemoteMode() {
  return remoteMode;
}

/** 远程模式的电脑地址（仅用于界面显示）。 */
export function remoteOriginLabel() {
  return remoteMode ? backendOrigin : "";
}

/** 配对 token 的存放处：远程模式放 localStorage（App 重启后仍可用，电脑重启后需重新配对），否则 sessionStorage。 */
function tokenStorage() {
  try {
    return remoteMode ? globalThis.localStorage : globalThis.sessionStorage;
  } catch (_error) {
    return undefined;
  }
}

/** 启动失败页显示的原因文字（Tauri invoke 拒绝时给出的是字符串）。 */
export function backendStartupErrorText(error) {
  if (typeof error === "string") return error;
  if (error && typeof error.message === "string") return error.message;
  return "";
}

export function getApiBaseUrl() {
  if (typeof window !== "undefined") {
    const loc = window.location;
    // 如果已经在 127.0.0.1 或 localhost 端口下运行，并且不是 vite 本地开发热更端口，说明已经同源
    if (
      (loc?.hostname === "127.0.0.1" || loc?.hostname === "localhost") &&
      loc?.port !== "5177" &&
      loc?.port !== "5178" &&
      loc?.port !== "5173"
    ) {
      return "";
    }
    if (backendOrigin) return backendOrigin;
    if (loc?.hostname === "tauri.localhost" || loc?.protocol === "tauri:") {
      throw new Error("本机后端服务尚未就绪");
    }
    // 独立前端开发模式使用默认端口；Tauri 页面必须先取得实际端口。
    if (
      loc?.port === "5177" ||
      loc?.port === "5178" ||
      loc?.port === "5173"
    ) {
      return "http://127.0.0.1:9666";
    }
  }
  return "";
}

export function resolveApiUrl(input) {
  if (typeof input === "string" && input.startsWith("/")) {
    return getApiBaseUrl() + input;
  }
  return input;
}

/**
 * 桌面端（Tauri）取得本次启动的桌面会话。浏览器与 Android 没有会话，静默为空，不影响其他功能。
 * 后端据此识别桌面端：严格模式下的发送与审批批准都需要它。
 */
export async function refreshDesktopSession() {
  desktopSession = "";
  // 远程模式绝不取、也绝不发送桌面会话：电脑会把手机当作局域网客户端对待
  if (remoteMode) return false;
  const invoke = typeof window !== "undefined" ? window.__TAURI__?.core?.invoke : null;
  if (!invoke) return false;
  try {
    const value = await invoke("get_desktop_session");
    if (typeof value === "string" && /^[0-9a-f]{64}$/.test(value)) {
      desktopSession = value;
    }
  } catch (_error) {
    desktopSession = "";
  }
  return desktopSession !== "";
}

export function hasDesktopSession() {
  return desktopSession !== "";
}

/** 仅供测试：清空内存中的桌面会话。 */
export function clearDesktopSession() {
  desktopSession = "";
}

export function readAuthToken(storage = tokenStorage()) {
  try {
    return storage?.getItem(AUTH_TOKEN_KEY) || "";
  } catch (_error) {
    return "";
  }
}

export function storeAuthToken(token, storage = tokenStorage()) {
  if (typeof token !== "string" || token.length === 0) {
    throw new Error("认证 token 不能为空");
  }
  storage?.setItem(AUTH_TOKEN_KEY, token);
  return token;
}

export function clearAuthToken(storage = tokenStorage()) {
  storage?.removeItem(AUTH_TOKEN_KEY);
}

export function authenticatedHeaders(headers = {}, storage = tokenStorage()) {
  const result = new Headers(headers);
  const token = readAuthToken(storage);
  if (token) {
    result.set("Authorization", `Bearer ${token}`);
  }
  return result;
}

export async function fetchWithAuth(fetchImpl, input, init = {}, storage = tokenStorage()) {
  const headers = authenticatedHeaders(init.headers, storage);
  // 桌面会话只发给本机后端的相对路径，绝不随绝对地址发往别处
  if (desktopSession && !remoteMode && typeof input === "string" && input.startsWith("/")) {
    headers.set(DESKTOP_SESSION_HEADER, desktopSession);
  }
  const resolvedInput = resolveApiUrl(input);
  return fetchImpl(resolvedInput, { ...init, headers });
}

async function isDesktopSessionRejected(response) {
  if (!response || response.status !== 403 || typeof response.clone !== "function") return false;
  try {
    const data = await response.clone().json();
    return data?.detail?.error_code === "desktop_session_invalid";
  } catch (_error) {
    return false;
  }
}

/**
 * 带桌面会话的请求：后端以 403 `desktop_session_invalid` 拒绝时（例如后端重启换了会话），
 * 重新取一次会话并只重试一次；仍失败就把响应交给调用方，不循环。
 */
export async function fetchWithDesktopSession(fetchImpl, input, init = {}, storage = tokenStorage()) {
  const response = await fetchWithAuth(fetchImpl, input, init, storage);
  if (!desktopSession || !(await isDesktopSessionRejected(response))) {
    return response;
  }
  await refreshDesktopSession();
  return fetchWithAuth(fetchImpl, input, init, storage);
}
