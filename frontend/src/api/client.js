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
 * 可选超时（2026-11-05）：`timeoutMs` 为正数时用 AbortController 在到点后中止请求，
 * 并与调用方传入的 `signal` 合并；不传时行为与原来一致（不超时）。
 * 超时只说明前端不再等待，后端可能已经处理了请求。
 */
function requestSignal({ signal, timeoutMs } = {}) {
  const ms = Number(timeoutMs);
  if (!(ms > 0) || typeof AbortController === "undefined") {
    return { signal, timedOut: () => false, done: () => {} };
  }
  const controller = new AbortController();
  let expired = false;
  const timer = setTimeout(() => {
    expired = true;
    controller.abort();
  }, ms);
  const onAbort = () => controller.abort();
  if (signal) {
    if (signal.aborted) controller.abort();
    else signal.addEventListener("abort", onAbort, { once: true });
  }
  return {
    signal: controller.signal,
    timedOut: () => expired,
    done: () => {
      clearTimeout(timer);
      signal?.removeEventListener?.("abort", onAbort);
    },
  };
}

async function requestJson(path, init, options = {}) {
  const request = requestSignal(options);
  try {
    const response = await apiFetch(path, { ...init, signal: request.signal });
    const data = await response.json().catch(() => ({
      success: false,
      result: `HTTP ${response.status} 响应解析失败`,
    }));
    return { status: response.status, ok: response.ok, data };
  } catch (error) {
    if (request.timedOut()) {
      return {
        status: 0,
        ok: false,
        timedOut: true,
        data: { success: false, result: `请求超时（${Number(options.timeoutMs)} ms 未响应），后端是否已处理未知` },
      };
    }
    const message = error instanceof Error ? error.message : String(error);
    return {
      status: 0,
      ok: false,
      data: { success: false, result: `网络连接失败: ${message}` },
    };
  } finally {
    request.done();
  }
}

/**
 * GET JSON 请求并解析响应体；`options`：`{ signal, timeoutMs }`（均可选）
 */
export async function getJson(path, options = {}) {
  return requestJson(path, {
    method: "GET",
    headers: { Accept: "application/json" },
  }, options);
}

/**
 * POST JSON 请求并解析响应体；`options`：`{ signal, timeoutMs }`（均可选）
 */
export async function postJson(path, body = {}, options = {}) {
  return requestJson(path, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json",
    },
    body: JSON.stringify(body),
  }, options);
}

/** 后端产物预览地址：已是绝对路径（`/preview?...`）则原样使用，否则按文件名拼 `/preview?path=` */
export function previewResourcePath(path) {
  return path.startsWith("/") ? path : `/preview?path=${encodeURIComponent(path)}`;
}

/**
 * 读取文本预览资源（G-code 等）。非 2xx 时返回 `{ ok:false, error }`，绝不把错误页当正文。
 */
export async function fetchPreviewText(path, options = {}) {
  if (!path) return { ok: false, status: 0, text: "", error: "缺少资源路径" };
  try {
    const response = await apiFetch(previewResourcePath(path), { signal: options.signal });
    if (!response.ok) {
      return { ok: false, status: response.status, text: "", error: `资源读取失败（HTTP ${response.status}）` };
    }
    return { ok: true, status: response.status, text: await response.text(), error: "" };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return { ok: false, status: 0, text: "", error: `资源读取失败：${message}` };
  }
}

/**
 * 获取预览资源 Blob URL (用于鉴权模式下的图片预览)
 */
export async function fetchPreviewBlobUrl(path) {
  if (!path) return "";
  const url = previewResourcePath(path);
  try {
    const response = await apiFetch(url);
    if (!response.ok) return "";
    const blob = await response.blob();
    return URL.createObjectURL(blob);
  } catch (_e) {
    return "";
  }
}
