// Manual, isolated browser repro for the Voice Center route.
// Uses only a Vite loopback server and synthetic Tauri/HTTP fixtures.
// Run: node tests/voice-center-render.smoke.mjs (from frontend/).
import assert from "node:assert/strict";
import { createServer, preview } from "vite";
import { chromium } from "playwright";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
const production = process.argv.includes("--preview");
const vite = production
  ? await preview({ root, preview: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" })
  : await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
let browser;
try {
  if (!production) await vite.listen();
  const local = vite.resolvedUrls?.local?.[0];
  if (!local) throw new Error("Vite loopback server URL unavailable");
  browser = await chromium.launch({
    headless: true,
    executablePath: "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
  });
  const fixture = {
    voice_status: {
      mode: "official", settings: { server_dir: "", python: "", args: ["app.py"], server_port: 8000,
        builtin_ws_port: 8000, builtin_http_port: 8003 },
      server: { state: "stopped", owned: false }, server_config: { exists: false, full_module: false },
      endpoint: { health: null }, device: { state: "unknown" }, bridge: { connected: false },
      strict: { enabled: true, locked: false, from_env: false }, gateway: { enabled: false },
      models: {}, mcp: { endpoints: [], tools: [], max_extra_endpoints: 4 },
    },
    voice_builtin_status: {
      running: false, mode: "echo", link_mode: "official", ws_port: 8000,
      http_port: 8003, models: {}, local: { settings: {}, models: [], ready: {} },
      sessions: [], events: [], timings: [], devices: [], memory_enabled: false,
    },
    voice_mcp_status: { servers: [], connections: [], audit: [], revision: "mock", max_servers: 8 },
    voice_home_status: { revision: 1, settings: { enabled: false, entities: [], base_url: "" } },
    voice_music_status: { settings: { enabled: false, directory: "" }, track_count: 0 },
    voice_agent_status: { enabled: false, sandbox_verified: false, runs: [], settings: {} },
    voice_vision_status: { enabled: false, settings: {} },
    voice_plugins_status: { settings: { news_sources: [], rag_dataset_ids: [] }, keys: {} },
    voice_memory_list: [],
  };
  const scenarios = [
    {name: "open-from-sidebar", navigateFromHome: true, overrides: {}},
    {name: "healthy", overrides: {}},
    {name: "missing-optional-mcp", overrides: {voice_mcp_status: {revision: "mock"}}},
    {name: "missing-optional-music", overrides: {voice_music_status: {}}},
    {name: "missing-optional-builtin", overrides: {voice_builtin_status: {running: false}}},
    {name: "render-error-fallback", expectFallback: true, overrides: {
      voice_mcp_status: { revision: "mock", max_servers: 8, audit: [],
        servers: [{config: {name: "synthetic-broken", enabled: true, transport: {type: "stdio"}}}],
        connections: [{name: "synthetic-broken", tools: [{name: "synthetic-tool", permission: "ask"}]}] },
    }},
  ];
  for (const scenario of scenarios) {
    const context = await browser.newContext({ serviceWorkers: "block" });
    await context.addInitScript(({ commands }) => {
      window.__TAURI__ = {
        core: {
          invoke: async (command) => {
            if (command === "get_backend_origin") return "http://127.0.0.1:9666";
            if (command === "get_desktop_session") return "a".repeat(64);
            if (Object.prototype.hasOwnProperty.call(commands, command)) return commands[command];
            throw new Error(`Synthetic Tauri command unavailable: ${command}`);
          },
        },
        event: { listen: async () => () => {} },
      };
    }, { commands: {...fixture, ...scenario.overrides} });
    await context.route("**/*", async (route) => {
      const target = new URL(route.request().url());
      if (target.pathname === "/api/system/info" || target.pathname === "/v1/approvals") {
        return route.fulfill({ status: 200, contentType: "application/json",
          body: JSON.stringify({success: true, result: {mcp: {}, approvals: []}}) });
      }
      if (target.origin === new URL(local).origin) return route.continue();
      if (target.hostname === "127.0.0.1" && target.port === "9666") {
        return route.fulfill({ status: 200, contentType: "application/json",
          body: JSON.stringify({success: true, result: {mcp: {}, approvals: []}}) });
      }
      return route.abort("blockedbyclient");
    });
    const page = await context.newPage();
    const errors = [];
    const resources = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("response", (response) => {
      if (response.status() >= 400) resources.push(`${response.status()} ${new URL(response.url()).pathname}`);
    });
    page.on("console", (msg) => {
      // Vite preview has no Tauri/API server: auxiliary 404s are recorded
      // separately. They cannot be mistaken for React runtime exceptions.
      if (msg.type() === "error" && !msg.text().startsWith("Failed to load resource:")) {
        errors.push("console: " + msg.text());
      }
    });
    await page.goto(new URL(scenario.navigateFromHome ? "" : "voice", local).href, {waitUntil: "domcontentloaded"});
    if (scenario.navigateFromHome) {
      await page.getByRole("navigation").getByRole("button", { name: /语音中心/ }).click();
      assert.equal(new URL(page.url()).pathname, "/voice", "侧栏应切换到语音中心");
    }
    await page.waitForTimeout(1200);
    const result = { scenario: scenario.name, page: await page.locator("#voice-mode-card").count(),
      music: await page.locator('section[aria-label="本地音乐"]').count(),
      fallback: await page.locator("#voice-render-error").count(),
      root: (await page.locator("#root").innerText()).slice(0, 120),
      errors: errors.slice(0, 2).map((text) => text.slice(0, 180)),
      resources: resources.slice(0, 5) };
    console.log("VOICE_RENDER_TEST=" + JSON.stringify(result));
    assert.ok(result.root.length > 0, "整个 React 根节点不应被卸载成黑屏");
    if (scenario.expectFallback) {
      assert.equal(result.fallback, 1, "异常组件应触发可见的语音页隔离提示");
      assert.equal(await page.locator(".ob-main").count(), 1, "主工作台框架应保留");
      await page.getByRole("button", { name: "返回工作台" }).click();
      assert.equal(new URL(page.url()).pathname, "/", "用户应能离开故障语音页");
    } else {
      assert.equal(result.page, 1, "语音中心应正常显示");
      assert.equal(result.music, 1, "内置语音子卡片应正常显示");
      assert.equal(result.fallback, 0, "有效配置不应触发错误页面");
      assert.deepEqual(errors, [], "不应有前端未处理异常");
    }
    await context.close();
  }
} finally {
  if (browser) await browser.close();
  if (production) await new Promise((resolve) => vite.httpServer.close(resolve));
  else await vite.close();
}
