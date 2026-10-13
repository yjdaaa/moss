import assert from "node:assert/strict";
import test from "node:test";
import {
  authenticatedHeaders,
  backendStartupErrorText,
  clearAuthToken,
  fetchWithAuth,
  getApiBaseUrl,
  initializeBackendOrigin,
  readAuthToken,
  resolveApiUrl,
  storeAuthToken,
} from "../src/authSession.js";

function storage() {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
    removeItem: (key) => values.delete(key),
  };
}

test("auth token is session-scoped and added without replacing headers", async () => {
  const session = storage();
  storeAuthToken("abc", session);
  assert.equal(readAuthToken(session), "abc");
  const headers = authenticatedHeaders({ "Content-Type": "application/json" }, session);
  assert.equal(headers.get("Content-Type"), "application/json");
  assert.equal(headers.get("Authorization"), "Bearer abc");
  let seen;
  await fetchWithAuth(async (_url, init) => {
    seen = init.headers;
    return { ok: true };
  }, "/api/health", {}, session);
  assert.equal(seen.get("Authorization"), "Bearer abc");
  clearAuthToken(session);
  assert.equal(readAuthToken(session), "");
});

test("Tauri assets wait for the actual backend port and never guess on failure", async () => {
  const previousWindow = globalThis.window;
  try {
    globalThis.window = {
      location: { hostname: "tauri.localhost", protocol: "http:", port: "" },
      __TAURI__: { core: { invoke: async (name) => {
        assert.equal(name, "get_backend_origin");
        return "http://127.0.0.1:9667";
      } } },
    };
    await initializeBackendOrigin();
    assert.equal(getApiBaseUrl(), "http://127.0.0.1:9667");
    assert.equal(resolveApiUrl("/api/health"), "http://127.0.0.1:9667/api/health");

    globalThis.window.__TAURI__.core.invoke = async () => { throw new Error("后端启动失败"); };
    await assert.rejects(initializeBackendOrigin(), /后端启动失败/);
    assert.throws(getApiBaseUrl, /尚未就绪/);

    globalThis.window.__TAURI__.core.invoke = async () => "http://example.com:9667";
    await assert.rejects(initializeBackendOrigin(), /地址无效/);
    assert.throws(getApiBaseUrl, /尚未就绪/);
  } finally {
    globalThis.window = previousWindow;
  }
});

test("Vite development retains the default port without Tauri", async () => {
  const previousWindow = globalThis.window;
  try {
    globalThis.window = { location: { hostname: "localhost", protocol: "http:", port: "5173" } };
    await initializeBackendOrigin();
    assert.equal(resolveApiUrl("/api/health"), "http://127.0.0.1:9666/api/health");
  } finally {
    globalThis.window = previousWindow;
  }
});

test("startup failure page shows the backend's reason", () => {
  assert.equal(backendStartupErrorText("后端服务初始化失败: 已有 MOSS 实例正在运行"), "后端服务初始化失败: 已有 MOSS 实例正在运行");
  assert.equal(backendStartupErrorText(new Error("本机后端地址无效")), "本机后端地址无效");
  assert.equal(backendStartupErrorText(undefined), "");
  assert.equal(backendStartupErrorText({}), "");
});

test("desktop session is memory-only, sent only to relative API paths, and refreshed once on rejection", async () => {
  const {
    clearDesktopSession,
    fetchWithDesktopSession,
    hasDesktopSession,
    refreshDesktopSession,
  } = await import("../src/authSession.js");
  const previousWindow = globalThis.window;
  const sessions = ["a".repeat(64), "b".repeat(64)];
  try {
    // 浏览器（没有 Tauri）：没有会话，不发送该头
    globalThis.window = { location: { hostname: "127.0.0.1", port: "9666", protocol: "http:" } };
    assert.equal(await refreshDesktopSession(), false);
    let seen = [];
    const record = async (url, init) => {
      seen.push({ url, header: init.headers.get("X-Moss-Desktop-Session") });
      return { status: 200, ok: true };
    };
    await fetchWithDesktopSession(record, "/api/health", {}, storage());
    assert.equal(seen[0].header, null);

    // 桌面端：取得会话；只加到相对路径，绝对地址不带
    globalThis.window.__TAURI__ = { core: { invoke: async (name) => {
      assert.equal(name, "get_desktop_session");
      return sessions.shift();
    } } };
    assert.equal(await refreshDesktopSession(), true);
    assert.equal(hasDesktopSession(), true);
    seen = [];
    await fetchWithDesktopSession(record, "/api/health", {}, storage());
    await fetchWithDesktopSession(record, "https://example.com/x", {}, storage());
    assert.equal(seen[0].header, "a".repeat(64));
    assert.equal(seen[1].header, null);

    // 会话被拒：重新取一次并只重试一次
    seen = [];
    const rejectedJson = { success: false, detail: { error_code: "desktop_session_invalid" } };
    const rejected = () => ({ status: 403, ok: false, clone: () => ({ json: async () => rejectedJson }) });
    const alwaysReject = async (url, init) => {
      seen.push(init.headers.get("X-Moss-Desktop-Session"));
      return rejected();
    };
    const response = await fetchWithDesktopSession(alwaysReject, "/v1/approvals", {}, storage());
    assert.equal(response.status, 403);
    assert.deepEqual(seen, ["a".repeat(64), "b".repeat(64)], "只重试一次，且使用新会话");

    // 其他 403（例如 strict_mode_blocked）不重试
    seen = [];
    const otherForbidden = async (_url, init) => {
      seen.push(init.headers.get("X-Moss-Desktop-Session"));
      return { status: 403, ok: false, clone: () => ({ json: async () => ({ detail: { error_code: "strict_mode_blocked" } }) }) };
    };
    await fetchWithDesktopSession(otherForbidden, "/api/workflow", {}, storage());
    assert.equal(seen.length, 1);

    // Tauri 返回非法值或报错（Android）：没有会话
    globalThis.window.__TAURI__.core.invoke = async () => { throw new Error("移动端没有桌面会话"); };
    assert.equal(await refreshDesktopSession(), false);
    globalThis.window.__TAURI__.core.invoke = async () => "not-a-session";
    assert.equal(await refreshDesktopSession(), false);
  } finally {
    clearDesktopSession();
    globalThis.window = previousWindow;
  }
});
