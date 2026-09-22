export const AUTH_TOKEN_KEY = "laserLab.authToken";

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
    // 当运行在 Tauri 桌面客户端环境下 (tauri.localhost 或 tauri: 协议)，或处于独立前端开发模式下
    // 统一自动重定向至本地后端默认 9666 端口，确保双保险
    if (
      window.__TAURI__ ||
      loc?.hostname === "tauri.localhost" ||
      loc?.protocol === "tauri:" ||
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

export function readAuthToken(storage = globalThis.sessionStorage) {
  try {
    return storage?.getItem(AUTH_TOKEN_KEY) || "";
  } catch (_error) {
    return "";
  }
}

export function storeAuthToken(token, storage = globalThis.sessionStorage) {
  if (typeof token !== "string" || token.length === 0) {
    throw new Error("认证 token 不能为空");
  }
  storage?.setItem(AUTH_TOKEN_KEY, token);
  return token;
}

export function clearAuthToken(storage = globalThis.sessionStorage) {
  storage?.removeItem(AUTH_TOKEN_KEY);
}

export function authenticatedHeaders(headers = {}, storage = globalThis.sessionStorage) {
  const result = new Headers(headers);
  const token = readAuthToken(storage);
  if (token) {
    result.set("Authorization", `Bearer ${token}`);
  }
  return result;
}

export async function fetchWithAuth(fetchImpl, input, init = {}, storage = globalThis.sessionStorage) {
  const headers = authenticatedHeaders(init.headers, storage);
  const resolvedInput = resolveApiUrl(input);
  return fetchImpl(resolvedInput, { ...init, headers });
}
