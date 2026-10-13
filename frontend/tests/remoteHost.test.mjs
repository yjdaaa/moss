import assert from "node:assert/strict";
import test from "node:test";
import {
  REMOTE_HOST_KEY,
  clearRemoteOrigin,
  isAndroidDevice,
  normalizeRemoteOrigin,
  probeRemoteOrigin,
  readRemoteOrigin,
  saveRemoteOrigin,
} from "../src/remoteHost.js";
import {
  fetchWithAuth,
  getApiBaseUrl,
  initializeBackendOrigin,
  isRemoteMode,
  refreshDesktopSession,
  remoteOriginLabel,
} from "../src/authSession.js";

function storage() {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, String(value)),
    removeItem: (key) => values.delete(key),
  };
}

test("computer address is normalized to http://host:port", () => {
  assert.equal(normalizeRemoteOrigin("192.168.1.20"), "http://192.168.1.20:9666");
  assert.equal(normalizeRemoteOrigin(" 192.168.1.20:9670 "), "http://192.168.1.20:9670");
  assert.equal(normalizeRemoteOrigin("http://192.168.1.20:9666/"), "http://192.168.1.20:9666");
  assert.equal(normalizeRemoteOrigin("MOSS-PC.local:9666"), "http://moss-pc.local:9666");
  for (const bad of [
    "",
    "https://192.168.1.20",
    "127.0.0.1:9666",
    "localhost",
    "192.168.1.300",
    "192.168.1.20:70000",
    "192.168.1.20/api",
    "user@192.168.1.20",
    "a b",
    null,
  ]) {
    assert.equal(normalizeRemoteOrigin(bad), null, String(bad));
  }
});

test("saved address round-trips and can be cleared", () => {
  const store = storage();
  assert.equal(readRemoteOrigin(store), null);
  assert.equal(saveRemoteOrigin("10.0.0.5", store), "http://10.0.0.5:9666");
  assert.equal(readRemoteOrigin(store), "http://10.0.0.5:9666");
  store.setItem(REMOTE_HOST_KEY, "https://evil.example");
  assert.equal(readRemoteOrigin(store), null);
  clearRemoteOrigin(store);
  assert.equal(store.getItem(REMOTE_HOST_KEY), null);
  assert.throws(() => saveRemoteOrigin("bad address", store), /格式不正确/);
});

test("android detection uses the user agent", () => {
  assert.equal(isAndroidDevice({ userAgent: "Mozilla/5.0 (Linux; Android 14; Pixel)" }), true);
  assert.equal(isAndroidDevice({ userAgent: "Mozilla/5.0 (Windows NT 10.0)" }), false);
});

test("probe reports a readable reason when the computer is unreachable", async () => {
  await probeRemoteOrigin("http://10.0.0.5:9666", async (url) => {
    assert.equal(url, "http://10.0.0.5:9666/api/health");
    return { ok: true };
  });
  await assert.rejects(
    probeRemoteOrigin("http://10.0.0.5:9666", async () => { throw new Error("Failed to fetch"); }),
    /无法连接电脑 MOSS（http:\/\/10\.0\.0\.5:9666）.*同一个 WiFi/,
  );
});

test("remote mode sends requests to the computer without the desktop session", async () => {
  const previousWindow = globalThis.window;
  try {
    let sessionAsked = false;
    globalThis.window = {
      location: { hostname: "tauri.localhost", protocol: "http:", port: "" },
      __TAURI__: { core: { invoke: async (name) => {
        if (name === "get_desktop_session") sessionAsked = true;
        return "0".repeat(64);
      } } },
    };
    await initializeBackendOrigin({ readRemote: () => "http://10.0.0.5:9666", probe: async () => true });
    assert.equal(isRemoteMode(), true);
    assert.equal(remoteOriginLabel(), "http://10.0.0.5:9666");
    assert.equal(getApiBaseUrl(), "http://10.0.0.5:9666");
    assert.equal(await refreshDesktopSession(), false);
    assert.equal(sessionAsked, false);
    const tokens = storage();
    tokens.setItem("laserLab.authToken", "tok");
    let seenUrl;
    let seenHeaders;
    await fetchWithAuth(async (url, init) => {
      seenUrl = url;
      seenHeaders = init.headers;
      return { ok: true };
    }, "/api/health", {}, tokens);
    assert.equal(seenUrl, "http://10.0.0.5:9666/api/health");
    assert.equal(seenHeaders.get("Authorization"), "Bearer tok");
    assert.equal(seenHeaders.get("X-Moss-Desktop-Session"), null);

    // 探测失败：仍处于远程模式（启动页据此提供“改用本机”），并把原因抛给调用方
    await assert.rejects(
      initializeBackendOrigin({ readRemote: () => "http://10.0.0.5:9666", probe: async () => { throw new Error("无法连接电脑 MOSS"); } }),
      /无法连接电脑/,
    );
    assert.equal(isRemoteMode(), true);

    // 未设置电脑地址时回到本机模式
    globalThis.window.__TAURI__.core.invoke = async () => "http://127.0.0.1:9667";
    await initializeBackendOrigin({ readRemote: () => null });
    assert.equal(isRemoteMode(), false);
    assert.equal(getApiBaseUrl(), "http://127.0.0.1:9667");
  } finally {
    globalThis.window = previousWindow;
  }
});
