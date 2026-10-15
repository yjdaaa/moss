// P4-08 / UI-3 voice center browser test: real React app, isolated Vite loopback server,
// synthetic stateful Tauri bridge (no real xiaozhi-server, device, model or network).
// Covers link switching with confirmation, server start/stop, grouped saves that submit
// only their own fields, unsaved markers that survive tab switches, and narrow layout.
// Run: node tests/voice-center.browser.mjs [--baseline]   (from frontend/; needs Playwright + Edge)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
const baselineDir = path.join(root, "tests", "visual-baseline");
const writeBaseline = process.argv.includes("--baseline");

const vite = await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
let browser;
try {
  await vite.listen();
  const local = vite.resolvedUrls?.local?.[0];
  if (!local) throw new Error("Vite loopback server URL unavailable");
  browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) });
  const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  const errors = [];
  const calls = [];
  page.on("pageerror", (error) => errors.push(error.message));

  // ---------- 有状态的假桌面端 ----------
  const masked = "wss://api.example.test/mcp/?token=***";
  const state = {
    mode: "official",
    server: { state: "stopped", owned: false },
    settings: { server_dir: "", python: "", args: ["app.py"], server_port: 8000, builtin_ws_port: 8000, builtin_http_port: 8003, official_mcp_endpoint: masked, official_mcp_endpoint_set: true },
    builtin: {
      running: false, mode: "echo", link_mode: "official", ws_port: 8000, http_port: 8003, ota_url: "http://192.0.2.10:8003/xiaozhi/ota/",
      models: {}, local: { settings: {}, models: [], helper_found: true }, sessions: [], events: [], timings: [], devices: [],
    },
  };
  const voiceStatus = (extra = {}) => ({
    mode: state.mode, settings: structuredClone(state.settings), server: structuredClone(state.server),
    server_config: { exists: true, full_module: false }, endpoint: { health: null }, device: { state: state.mode === "self_hosted" ? "waiting" : "unknown" },
    bridge: { connected: true }, strict: { enabled: state.mode === "self_hosted", locked: state.mode === "self_hosted", from_env: false }, gateway: { enabled: false },
    models: {}, mcp_endpoint: masked,
    mcp: { endpoints: [{ id: "primary", endpoint: masked, connected: true }], tools: [{ name: "get_status", description: "状态", enabled: true }], max_extra_endpoints: 4 },
    ...extra,
  });
  const handlers = {
    get_backend_origin: () => "http://127.0.0.1:9666",
    get_desktop_session: () => "a".repeat(64),
    voice_status: () => voiceStatus(),
    voice_server_log: () => ({ lines: ["[fake] started api_key=secret"] }),
    voice_builtin_status: () => structuredClone(state.builtin),
    voice_mcp_status: () => ({ servers: [], connections: [], audit: [], revision: "r1", max_servers: 8 }),
    voice_music_status: () => ({ settings: { enabled: false, directory: "" }, track_count: 0 }),
    voice_vision_status: () => ({ settings: { enabled: false, url: "", model: "" }, key_configured: false }),
    voice_agent_status: () => ({ enabled: false, sandbox_verified: false, runs: [], settings: {} }),
    voice_save_settings: ({ settings }) => {
      // 与 center.rs 一致：官方/内置链路下官方接入点变了才重连，并在返回里报告 reconnected
      let reconnected = false;
      for (const [key, value] of Object.entries(settings)) {
        if (key === "official_mcp_endpoint") {
          reconnected = state.mode !== "self_hosted" && state.officialRaw !== value;
          state.officialRaw = value;
          state.settings[key] = masked;
        } else state.settings[key] = value;
      }
      return voiceStatus({ reconnected });
    },
    voice_mode_changes: ({ mode }) => ({ changes: [`语音链路：${state.mode} → ${mode}`] }),
    voice_set_mode: ({ mode }) => {
      const stillRunning = mode !== "self_hosted" && state.server.state === "running";
      state.mode = mode;
      state.builtin.link_mode = mode;
      return voiceStatus({ server_still_running: stillRunning });
    },
    voice_server_start: () => {
      state.server = { state: "running", owned: true, pid: 4242, started_at: 1_790_000_000 };
      return voiceStatus();
    },
    voice_server_stop: () => {
      state.server = { state: "stopped", owned: false };
      return voiceStatus();
    },
    voice_builtin_models_test: () => ({ ok: true, elapsed_ms: 320, detail: "合成测试响应" }),
    voice_builtin_models_save: ({ input }) => {
      for (const [section, value] of Object.entries(input)) state.builtin.models[section] = { configured: true, url: value.url, model: value.model, key_set: true };
      return { ...structuredClone(state.builtin), restarted: false };
    },
    voice_local_save: ({ settings }) => {
      state.builtin.local.settings = structuredClone(settings);
      return { ...structuredClone(state.builtin), restarted: false };
    },
  };
  await page.exposeFunction("__fakeInvoke", async (command, args = {}) => {
    calls.push({ command, args: structuredClone(args) });
    if (!handlers[command]) throw new Error(`Synthetic Tauri command unavailable: ${command}`);
    return handlers[command](args);
  });
  await context.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: (command, args) => window.__fakeInvoke(command, args) }, event: { listen: async () => () => {} } };
  });
  await context.route("**/*", async (route) => {
    const target = new URL(route.request().url());
    if (target.origin === new URL(local).origin) return route.continue();
    if (target.hostname === "127.0.0.1" && target.port === "9666") {
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { mcp: {}, approvals: [] } }) });
    }
    return route.abort("blockedbyclient");
  });

  const last = (command) => [...calls].reverse().find((call) => call.command === command);
  const confirmDialog = async (button) => {
    const dialog = page.getByRole("alertdialog").filter({ has: page.getByRole("button", { name: button, exact: true }) });
    await dialog.waitFor();
    const id = await dialog.getAttribute("aria-labelledby");
    await dialog.getByRole("button", { name: button, exact: true }).click();
    await page.locator(`[aria-labelledby="${id}"]`).waitFor({ state: "detached" });
  };
  const tab = (name) => page.getByRole("tab", { name: new RegExp(`^${name}`) });
  const openGroup = async (id) => {
    const header = page.locator(`#voice-group-${id} > .voice-group-h`);
    if ((await header.getAttribute("aria-expanded")) !== "true") await header.click();
  };
  const shot = async (name) => {
    if (writeBaseline) await page.screenshot({ path: path.join(baselineDir, `${name}.png`) });
  };

  await page.goto(new URL("voice", local).href, { waitUntil: "domcontentloaded" });
  await page.locator("#voice-mode-card").waitFor();
  assert.equal(await page.getByRole("tab").count(), 5, "五个标签");

  // 1. 服务与设备：官方链路只显示接入点，其他链路折叠，需要时能展开
  await tab("服务与设备").click();
  assert.ok(await page.locator("#voice-endpoint-card").isVisible());
  assert.equal(await page.locator("#voice-builtin-card").isVisible(), false, "内置服务在官方链路下默认折叠");
  await page.locator("#voice-other-service").click();
  assert.ok(await page.locator("#voice-builtin-card").isVisible(), "展开后能看到其他链路");
  await page.locator("#voice-other-service").click();
  await shot("voice-official");

  // 2. 连接：只保存官方接入点这一组；未保存标记在切换标签后仍在，填写不丢
  await tab("连接").click();
  assert.equal(await page.locator("#voice-group-settings-builtinPorts").isVisible(), false, "内置端口不属于官方链路");
  await openGroup("settings-official");
  const save = page.locator("#voice-settings-save-official");
  assert.ok(await save.isDisabled(), "没改动不能保存");
  await page.locator("#voice-official-endpoint").fill("wss://api.example.test/mcp/?token=fake-ui-token");
  assert.ok(await page.locator("#voice-group-settings-official .voice-dirty").isVisible(), "分组显示未保存");
  await tab("服务与设备").click();
  assert.ok(await tab("连接").locator(".voice-tab-dot").isVisible(), "标签显示未保存");
  await tab("连接").click();
  assert.equal(await page.locator("#voice-official-endpoint").inputValue(), "wss://api.example.test/mcp/?token=fake-ui-token", "切换标签不丢填写");
  await save.click();
  await page.locator("#voice-group-settings-official [role=status]").filter({ hasText: "已保存官方接入点" }).filter({ hasText: "正在重新连接小智" }).waitFor();
  assert.deepEqual(last("voice_save_settings").args.settings, { official_mcp_endpoint: "wss://api.example.test/mcp/?token=fake-ui-token" }, "只提交本组字段");
  assert.equal(await page.locator("#voice-group-settings-official .voice-dirty").count(), 0);
  assert.equal(await tab("连接").locator(".voice-tab-dot").count(), 0);
  assert.ok(!(await page.locator("body").innerText()).includes("fake-ui-token"), "令牌不回显");

  // 3. 切到自建：确认框 → 切换；启动、停止服务端
  await page.locator("#voice-mode-self_hosted").click();
  await shot("voice-mode-confirm");
  await confirmDialog("确认切换");
  await page.locator("#voice-mode-self_hosted[aria-checked=true]").waitFor();
  assert.equal(last("voice_set_mode").args.mode, "self_hosted");
  await tab("服务与设备").click();
  assert.ok(await page.locator("#voice-server-card").isVisible(), "自建链路显示服务端");
  await page.locator("#voice-server-start").click();
  await page.locator("#voice-server-state").filter({ hasText: "运行中" }).waitFor();
  await shot("voice-self-hosted");
  await page.locator("#voice-server-stop").click();
  await page.locator("#voice-server-state").filter({ hasText: "未运行" }).waitFor();
  // 意外退出：显示退出码、不自动重启，可再次启动
  state.server = { state: "exited", owned: true, exit_code: 1 };
  await page.locator("#voice-server-state").filter({ hasText: "意外退出" }).waitFor({ timeout: 10_000 });
  assert.ok(await page.getByText("退出码 1，不会自动重启").isVisible());
  await shot("voice-crashed");
  await page.locator("#voice-server-start").click();
  await page.locator("#voice-server-state").filter({ hasText: "运行中" }).waitFor();

  // 4. 切到内置：服务端仍在运行时再问是否停止
  await page.locator("#voice-mode-builtin").click();
  await confirmDialog("确认切换");
  await confirmDialog("停止服务端");
  await page.locator("#voice-mode-builtin[aria-checked=true]").waitFor();
  assert.equal(last("voice_server_stop") !== undefined, true);
  assert.equal(state.server.state, "stopped");

  // 5. 内置大模型：先测试、后保存，只提交这一项
  await tab("模型").click();
  await openGroup("builtin-model-llm");
  const llmSave = page.locator("#voice-builtin-model-save-llm");
  await page.locator("#voice-builtin-model-url-llm").fill("https://llm.example.test/v1");
  await page.locator("#voice-builtin-model-key-llm").fill("fake-llm-key");
  await page.locator("#voice-builtin-model-name-llm").fill("fake-model");
  assert.ok(await llmSave.isDisabled(), "未测试不能保存");
  await page.locator("#voice-builtin-model-test-llm").click();
  await page.locator("#voice-builtin-model-result-llm").filter({ hasText: "测试通过" }).waitFor();
  await llmSave.click();
  await page.locator("#voice-group-builtin-model-llm [role=status]").filter({ hasText: "已保存大模型设置" }).waitFor();
  assert.deepEqual(Object.keys(last("voice_builtin_models_save").args.input), ["llm"]);
  assert.equal(await page.locator("#voice-builtin-model-key-llm").inputValue(), "", "保存后清空密钥");

  // 6. 本地模型与语音体验分开保存：保存体验时不带上本地模型未保存的改动
  await openGroup("local-models");
  await openGroup("experience");
  await page.getByLabel("本地合成语速（0.5 ~ 2.0）").fill("1.5");
  await page.locator("#voice-barge-in").check();
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已保存语音体验设置" }).waitFor();
  const localPayload = last("voice_local_save").args.settings;
  assert.equal(localPayload.barge_in, true);
  assert.equal(localPayload.tts_speed, 1, "本地模型的未保存改动没有被一起提交");
  assert.ok(await page.locator("#voice-group-local-models .voice-dirty").isVisible(), "本地模型仍显示未保存");
  await page.locator("#voice-group-experience").scrollIntoViewIfNeeded();
  await shot("voice-builtin");

  // 7. 窄屏无横向滚动
  await page.setViewportSize({ width: 820, height: 1000 });
  await tab("连接").click();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  assert.ok(overflow <= 1, `窄屏横向溢出 ${overflow}px`);
  await shot("voice-narrow");

  assert.deepEqual(errors, [], "不应有前端未处理异常");
  console.log(`VOICE_CENTER_BROWSER_TEST=PASS calls=${calls.length}`);
  await context.close();
} finally {
  if (browser) await browser.close();
  await vite.close();
}
