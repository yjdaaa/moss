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
    agent: {
      settings: { enabled: false, workspace: "", codex_executable: "", claude_executable: "" },
      strict_mode: false, providers: [], runs: [],
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
    voice_memory_list: () => [],
    voice_detect_python: () => ({ candidates: ["C:\\Python312\\python.exe"], suggested: "C:\\Python312\\python.exe" }),
    voice_strict_set: ({ enabled }) => ({ enabled, applied: true, needs_restart: false, env_file: "C:\\moss\\.env" }),
    voice_music_status: () => ({
      settings: { enabled: true, directory: "D:\\unplugged-music" },
      track_count: 0,
      warning: "本地音乐目录当前不可用（换电脑或移动盘拔出时请重新选择音乐文件夹），本次启动已跳过本地音乐",
    }),
    voice_vision_status: () => ({ settings: { enabled: false, url: "", model: "" }, key_configured: false }),
    voice_agent_status: () => ({ configured: state.agent.settings.enabled, strict_mode: state.agent.strict_mode, providers: state.agent.providers, runs: structuredClone(state.agent.runs), settings: structuredClone(state.agent.settings) }),
    open_voice_private: () => null,
    voice_agent_save: ({ input }) => {
      state.agent.settings = structuredClone(input);
      state.agent.providers = input.codex_executable
        ? [{ provider: "codex", ready: true, version: "codex-cli 0.157.1" }, { provider: "claude_code", ready: false, error_code: "agent_capability_missing" }]
        : [];
      return handlers.voice_agent_status();
    },
    voice_agent_stop: ({ runId }) => {
      const run = state.agent.runs.find((item) => item.id === runId);
      const stopped = Boolean(run && run.status === "running");
      if (stopped) Object.assign(run, { status: "cancelled", stoppable: false, error_code: "agent_stopped" });
      return { ...handlers.voice_agent_status(), stopped };
    },
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
  assert.equal(await page.getByRole("tab").count(), 5, "官方链路没有“模型”标签");
  assert.equal(await page.getByRole("tab", { name: /^模型/ }).count(), 0, "官方链路的模型在小智控制台配置");

  // 1. 服务：官方链路只显示接入点与地址；其他链路的面板不渲染（不是折叠）
  await tab("服务").click();
  assert.ok(await page.locator("#voice-endpoint-card").isVisible(), "官方链路显示接入点");
  assert.equal(await page.locator("#voice-builtin-card").count(), 0, "官方链路不渲染内置服务面板");
  assert.equal(await page.locator("#voice-server-card").count(), 0, "官方链路不渲染自建服务端面板");
  await shot("voice-official");

  // 2. 连接：只保存官方接入点这一组；未保存标记在切换标签后仍在，填写不丢
  await tab("服务").click();
  assert.equal(await page.locator("#voice-group-settings-builtinPorts").count(), 0, "内置端口不属于官方链路");
  await openGroup("settings-official");
  const save = page.locator("#voice-settings-save-official");
  assert.ok(await save.isDisabled(), "没改动不能保存");
  await page.locator("#voice-official-endpoint").fill("wss://api.example.test/mcp/?token=fake-ui-token");
  assert.ok(await page.locator("#voice-group-settings-official .voice-dirty").isVisible(), "分组显示未保存");
  await tab("服务").click();
  assert.ok(await tab("服务").locator(".voice-tab-dot").isVisible(), "标签显示未保存");
  await tab("服务").click();
  assert.equal(await page.locator("#voice-official-endpoint").inputValue(), "wss://api.example.test/mcp/?token=fake-ui-token", "切换标签不丢填写");
  await save.click();
  await page.locator("#voice-group-settings-official [role=status]").filter({ hasText: "已保存官方接入点" }).filter({ hasText: "正在重新连接小智" }).waitFor();
  assert.deepEqual(last("voice_save_settings").args.settings, { official_mcp_endpoint: "wss://api.example.test/mcp/?token=fake-ui-token" }, "只提交本组字段");
  assert.equal(await page.locator("#voice-group-settings-official .voice-dirty").count(), 0);
  assert.equal(await tab("服务").locator(".voice-tab-dot").count(), 0);
  assert.ok(!(await page.locator("body").innerText()).includes("fake-ui-token"), "令牌不回显");

  // 3. 切到自建：确认框 → 切换；启动、停止服务端
  await page.locator("#voice-mode-self_hosted").click();
  await shot("voice-mode-confirm");
  await confirmDialog("确认切换");
  await page.locator("#voice-mode-self_hosted[aria-checked=true]").waitFor();
  assert.equal(last("voice_set_mode").args.mode, "self_hosted");
  await tab("服务").click();
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

  // 7. Agent（Claude Code / Codex）：默认关闭；开启要有工作目录与 CLI；严格模式没开时说清楚；运行中可以停止
  await tab("扩展").click();
  await openGroup("agent");
  const agentGroup = page.locator("#voice-group-agent");
  assert.ok(/未开启|读取中/.test(await agentGroup.locator(".voice-group-summary").innerText()), "默认未开启");
  assert.ok(await page.locator("#voice-agent-save").isDisabled(), "没改动不能保存");
  await page.locator("#voice-agent-enabled").check();
  assert.ok(await agentGroup.getByText("开启前必须填写工作目录").isVisible(), "缺工作目录时说明原因");
  assert.ok(await page.locator("#voice-agent-save").isDisabled(), "缺工作目录不能保存");
  await agentGroup.locator("input.text-input").nth(0).fill("C:\\agent-workspace");
  await agentGroup.locator("input.text-input").nth(1).fill("C:\\tools\\codex.exe");
  assert.equal(await page.locator("#voice-agent-save").isDisabled(), false, "填好工作目录与 CLI 后可以保存");
  await page.locator("#voice-agent-save").click();
  await agentGroup.locator("[role=status]").filter({ hasText: "已保存Agent 设置" }).waitFor();
  assert.deepEqual(last("voice_agent_save").args.input, { enabled: true, workspace: "C:\\agent-workspace", codex_executable: "C:\\tools\\codex.exe", claude_executable: "" }, "开启状态与路径一起提交");
  assert.ok(await agentGroup.getByText("严格模式当前未开启，Agent 不会运行").isVisible(), "严格模式没开时给出原因");
  assert.match(await agentGroup.locator(".voice-group-summary").innerText(), /需要严格模式/);
  await shot("voice-agent");
  // 小智发起并经桌面批准后：桌面能停止运行中的任务，停止后记为已停止
  state.agent.strict_mode = true;
  state.agent.runs = [{ id: "agentrun-1", approval_id: "a1", provider: "codex", workspace_name: "agent-workspace", status: "running", stoppable: true, output: "正在读 README", error_code: null }];
  const stopButton = agentGroup.getByRole("button", { name: /停止/ });
  await stopButton.waitFor({ timeout: 15_000 });
  await stopButton.click();
  await confirmDialog("停止任务");
  await agentGroup.locator("[role=status]").filter({ hasText: "已请求停止" }).waitFor();
  assert.deepEqual(last("voice_agent_stop").args, { runId: "agentrun-1" }, "停止的是这条运行记录");
  await agentGroup.getByText("已停止").first().waitFor();
  assert.equal(await agentGroup.getByRole("button", { name: /停止/ }).count(), 0, "已结束的任务不再提供停止");
  await page.locator("#voice-agent-enabled").uncheck();
  await page.locator("#voice-agent-save").click();
  await agentGroup.locator("[role=status]").filter({ hasText: "已保存Agent 设置" }).waitFor();
  assert.equal(last("voice_agent_save").args.input.enabled, false, "可以再关掉");
  assert.match(await page.locator("#voice-group-music .voice-group-summary").innerText(), /目录不可用/, "换机音乐目录缺失时摘要显示目录不可用");
  await openGroup("music");
  assert.ok(await page.locator("#voice-group-music").getByText("本次启动已跳过本地音乐").isVisible(), "换机音乐目录缺失时展示降级提示");

  // 8. 隐私与家居：四项各自下拉，点开哪一项就在主页面就地展开对应设置（n26，2026-10-18）。
  await tab("隐私与家居").click();
  for (const key of ["memory", "plugins", "speaker", "home"]) {
    const item = page.locator(`#voice-private-${key}-item`);
    assert.equal(await item.count(), 1, `${key} 是一项下拉`);
    assert.equal(await item.evaluate((el) => el.open), false, `${key} 默认收起`);
  }
  assert.equal(await page.locator(".voice-private-item-body .voice-card").count(), 0, "未展开时不挂载设置卡");
  assert.equal(await page.locator("#voice-private-settings-open").isVisible(), false, "固件入口不在隐私与家居");
  if (process.env.VOICE_QA_DIR) await page.screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-privacy-tab.png") });
  const memoryItem = page.locator("#voice-private-memory-item");
  await memoryItem.locator("> summary").click();
  assert.equal(await memoryItem.evaluate((el) => el.open), true, "点标题展开记忆设置");
  await memoryItem.getByRole("heading", { name: "对话记忆" }).waitFor();
  assert.ok(last("voice_memory_list"), "主窗口就地读出设备记忆");
  assert.equal(await page.locator("#voice-private-memory-advanced").evaluate((el) => el.open), false, "长期记忆默认收起");
  if (process.env.VOICE_QA_DIR) await page.screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-privacy-tab-open.png") });
  await memoryItem.locator("> summary").click();
  assert.equal(await memoryItem.evaluate((el) => el.open), false, "再点收起");
  await tab("记录与诊断").click();
  assert.equal(await page.locator("#voice-private-settings-open").isVisible(), false, "固件入口默认收在设备维护组里");
  await openGroup("device-maintenance");
  await page.locator("#voice-private-settings-open").click();
  assert.deepEqual(last("open_voice_private").args, { section: "firmware" }, "设备维护入口打开私密固件任务");

  // 9. 窄屏无横向滚动
  await page.setViewportSize({ width: 820, height: 1000 });
  await tab("设备").click();
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
