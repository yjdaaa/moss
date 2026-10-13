import { fetchWithDesktopSession, readAuthToken, storeAuthToken, clearAuthToken, resolveApiUrl, getApiBaseUrl } from "../authSession.js";

export { readAuthToken, storeAuthToken, clearAuthToken, resolveApiUrl, getApiBaseUrl };

/**
 * 统一 API fetch 客户端：自动注入 Bearer Token 与桌面会话，拦截 401；
 * 桌面会话失效（403 desktop_session_invalid）时重新取会话并只重试一次。
 */
export async function apiFetch(input, init = {}) {
  const response = await fetchWithDesktopSession(fetch, input, init);
  if (response.status === 401) {
    window.dispatchEvent(new CustomEvent("laser-auth-required"));
  }
  return response;
}

/**
 * GET JSON 请求并解析响应体
 */
export async function getJson(path) {
  try {
    const response = await apiFetch(path, {
      method: "GET",
      headers: { Accept: "application/json" },
    });
    const data = await response.json().catch(() => ({
      success: false,
      result: `HTTP ${response.status} 响应解析失败`,
    }));
    return { status: response.status, ok: response.ok, data };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      status: 0,
      ok: false,
      data: { success: false, result: `网络连接失败: ${message}` },
    };
  }
}

/**
 * POST JSON 请求并解析响应体
 */
export async function postJson(path, body = {}, options = {}) {
  try {
    const response = await apiFetch(path, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body: JSON.stringify(body),
      signal: options.signal,
    });
    const data = await response.json().catch(() => ({
      success: false,
      result: `HTTP ${response.status} 响应解析失败`,
    }));
    return { status: response.status, ok: response.ok, data };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      status: 0,
      ok: false,
      data: { success: false, result: `网络连接失败: ${message}` },
    };
  }
}

/**
 * 获取预览资源 Blob URL (用于鉴权模式下的图片预览)
 */
export async function fetchPreviewBlobUrl(path) {
  if (!path) return "";
  // 如果是相对路径或带参数的 /preview
  const url = path.startsWith("/") ? path : `/preview?path=${encodeURIComponent(path)}`;
  try {
    const response = await apiFetch(url);
    if (!response.ok) return "";
    const blob = await response.blob();
    return URL.createObjectURL(blob);
  } catch (_e) {
    return "";
  }
}
