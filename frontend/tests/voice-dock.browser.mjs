// 全局“小智 · 雕刻同步”悬浮小窗的浏览器测试（2026-11-16 用户要求）。
// 需求：语音让 MOSS 干什么，任何页面都能同步看到（只显示雕刻相关）；可收成小胶囊并记在本机；
// 不显示对话原文；待确认预览只给入口，不重复确认 / 发送；浏览器（无桌面端）模式隐藏语音部分。
// 全部为回环假数据：不连设备、不连真实后端。
// Run: node tests/voice-dock.browser.mjs   (from frontend/; needs Playwright + Edge, or PLAYWRIGHT_CHANNEL=chrome)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));

const vite = await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
let browser;
try {
  await vite.listen();
  const local = vite.resolvedUrls?.local?.[0];
  if (!local) throw new Error("Vite loopback server URL unavailable");
  browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) });

  const now = Math.floor(Date.now() / 1000);
  const secretTranscript = "帮我刻生日快乐给妈妈";
  const state = {
    voice: { mode: "builtin", device_sessions: 1, phase: "thinking", level: 0 },
    sessions: [{ session_id: "s1", device_id: "aa:bb:cc:11:22:33", state: "thinking", active_tool: "laser_workflow_tool" }],
    jobs: [{ job_id: "job-9", workflow_id: "wf-9", state: "running", phase: "streaming", mode: "network", target: "127.0.0.1:2301", lines_sent: 37, total_lines: 100, elapsed_ms: 62_000 }],
    workflows: [],
  };
  const posts = [];
  const invoked = [];

  const setup = async (context, { desktop }) => {
    if (desktop) {
      await context.exposeFunction("__fakeInvoke", async (command) => {
        invoked.push(command);
        if (command === "get_backend_origin") return "http://127.0.0.1:9666";
        if (command === "get_desktop_session") return "a".repeat(64);
        if (command === "voice_status") return { mode: "builtin", settings: {}, server: {}, device: {}, strict: { enabled: false }, mcp: { endpoints: [], tools: [] } };
        if (command === "voice_builtin_status") {
          // 真实状态里还带调试轨迹（识别原文与回复）；小窗绝不能显示这些
          return { running: true, mode: "chat", sessions: state.sessions, debug_traces: [{ raw_asr: secretTranscript, reply: "好的，正在生成预览" }], events: [{ text: secretTranscript }] };
        }
        throw new Error(`Synthetic Tauri command unavailable: ${command}`);
      });
      await context.addInitScript(() => {
        window.__TAURI__ = { core: { invoke: (command, args) => window.__fakeInvoke(command, args) }, event: { listen: async () => () => {} } };
        localStorage.setItem("laser-forge-conn-mode", "network");
        localStorage.setItem("laser-forge-host", "192.0.2.10");
        localStorage.setItem("laser-forge-telnet-port", "23");
      });
    }
    const json = (route, result) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result }) });
    await context.route("**/*", async (route) => {
      const request = route.request();
      const target = new URL(request.url());
      if (target.origin === new URL(local).origin) {
        // 浏览器模式下前端直接打同源 /api：同样回假数据
        if (!target.pathname.startsWith("/api/") && !target.pathname.startsWith("/v1/")) return route.continue();
      } else if (!(target.hostname === "127.0.0.1" && target.port === "9666")) {
        return route.abort("blockedbyclient");
      }
      const body = request.postDataJSON?.() || {};
      if (request.method() === "POST") posts.push({ path: target.pathname, body });
      if (target.pathname === "/api/system/info") return json(route, { mcp: { connected: false }, voice: state.voice });
      if (target.pathname === "/api/jobs/stop-targets") return json(route, { jobs: state.jobs });
      if (target.pathname === "/api/workflow" && body.action === "recent") return json(route, { workflows: state.workflows });
      if (target.pathname === "/v1/approvals") return json(route, { approvals: [], server_time: now, ttl_seconds: 120 });
      return json(route, { mcp: {}, approvals: [], jobs: [] });
    });
  };

  // ---------- 桌面端 ----------
  const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 1000 } });
  await setup(context, { desktop: true });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(local, { waitUntil: "domcontentloaded" });

  // 1. 小智在想、调用了雕刻工具、有任务在跑：任何页面右下角都能看到，且不显示对话原文
  const dock = page.locator("#voice-dock");
  await dock.waitFor({ timeout: 20_000 });
  await page.locator("#voice-dock-tool").filter({ hasText: "正在调用：雕刻任务" }).waitFor({ timeout: 10_000 });
  assert.equal(await page.locator("#voice-dock-phase").innerText(), "思考中");
  assert.match(await dock.innerText(), /1 台小智在线/);
  await page.locator("#voice-dock-job-text").filter({ hasText: "雕刻 37% · 正在发送 G-code" }).waitFor({ timeout: 10_000 });
  assert.ok(!(await page.locator("body").innerText()).includes(secretTranscript), "不得显示识别原文或回复");

  // 2. 与雕刻任务浮层上下排开、不重叠
  await page.locator("#job-progress-dock").waitFor({ timeout: 10_000 });
  const voiceBox = await dock.boundingBox();
  const jobBox = await page.locator("#job-progress-dock").boundingBox();
  assert.ok(voiceBox.y + voiceBox.height <= jobBox.y + 0.5, `小窗应在任务浮层上方：${JSON.stringify({ voiceBox, jobBox })}`);

  // 3. 收成小胶囊，折叠状态记在本机，刷新后仍是胶囊
  await page.locator("#voice-dock-collapse").click();
  await page.locator("#voice-dock-pill-text").filter({ hasText: "小智思考中 · 雕刻任务 · 37%" }).waitFor();
  assert.equal(await page.evaluate(() => localStorage.getItem("moss_voice_dock_collapsed")), "1");
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator("#voice-dock-pill").waitFor({ timeout: 20_000 });
  assert.equal(await page.locator("#voice-dock").count(), 0);
  await page.locator("#voice-dock-pill-open").click();
  await dock.waitFor();
  assert.equal(await page.evaluate(() => localStorage.getItem("moss_voice_dock_collapsed")), "0");

  // 4. 一轮结束回到待命：工具名转成“刚才调用”，空闲时不再读语音会话
  await page.locator("#voice-dock-tool").filter({ hasText: "正在调用：雕刻任务" }).waitFor({ timeout: 10_000 });
  state.voice = { ...state.voice, phase: "idle" };
  state.sessions = [{ ...state.sessions[0], state: "idle", active_tool: null }];
  await page.locator("#voice-dock-phase").filter({ hasText: "待命" }).waitFor({ timeout: 10_000 });
  await page.locator("#voice-dock-tool").filter({ hasText: "刚才调用：雕刻任务" }).waitFor({ timeout: 10_000 });
  const idleCalls = invoked.filter((c) => c === "voice_builtin_status").length;
  await page.waitForTimeout(3500);
  assert.equal(invoked.filter((c) => c === "voice_builtin_status").length, idleCalls, "空闲时不轮询语音会话");

  // 5. AI 预览被“稍后再看”收起后，小窗给出入口；点击只把预览窗叫回来，不发送任何确认
  state.workflows = [{
    workflow_id: "wf-ai-1", status: "preview_ready", origin: "voice", source_type: "text", created_at: now,
    input: { text: "生日快乐", material: "椴木", thickness_mm: 3 },
    artifacts: { width_mm: 60, height_mm: 20, actual_params: { laser_max_power: 420, power_scale: 1000, feed_rate: 1800, passes: 1 } },
  }];
  await page.locator("#ai-preview-card").waitFor({ timeout: 15_000 });
  await page.locator("#ai-preview-later").click();
  await page.locator("#ai-preview-card").waitFor({ state: "detached" });
  await page.locator("#voice-dock-preview").filter({ hasText: "有待确认的预览" }).waitFor({ timeout: 10_000 });
  await page.locator("#voice-dock-preview").click();
  await page.locator("#ai-preview-card").waitFor({ timeout: 5_000 });
  assert.equal(await page.locator("#voice-dock-preview").count(), 0, "预览回到前台后入口消失");
  assert.ok(!posts.some((p) => p.body?.action === "confirm_send"), "小窗不得发送确认");
  await page.locator("#ai-preview-discard").click();
  await page.locator("#ai-preview-card").waitFor({ state: "detached", timeout: 5_000 });

  // 6. 小智全部下线、没有任务：整个小窗隐藏
  state.voice = { ...state.voice, device_sessions: 0, phase: null };
  state.sessions = [];
  state.jobs = [];
  await page.locator("#voice-dock").waitFor({ state: "detached", timeout: 10_000 });
  assert.equal(await page.locator("#voice-dock-pill").count(), 0);
  assert.deepEqual(errors, [], "桌面端页面运行期无异常");
  await context.close();

  // ---------- 浏览器 / 局域网模式：没有桌面端语音接口，语音部分隐藏 ----------
  state.voice = { mode: "builtin", device_sessions: 1, phase: "thinking", level: 0 };
  state.jobs = [{ job_id: "job-b", workflow_id: "wf-b", state: "running", phase: "streaming", mode: "network", target: "127.0.0.1:2301", lines_sent: 10, total_lines: 100, elapsed_ms: 5_000 }];
  const before = invoked.length;
  const browserContext = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 1000 } });
  await setup(browserContext, { desktop: false });
  const lan = await browserContext.newPage();
  const lanErrors = [];
  lan.on("pageerror", (error) => lanErrors.push(error.message));
  await lan.goto(local, { waitUntil: "domcontentloaded" });
  await lan.locator("#job-progress-dock").waitFor({ timeout: 20_000 });
  await lan.waitForTimeout(1500);
  assert.equal(await lan.locator("#voice-dock").count(), 0, "浏览器模式不显示语音小窗");
  assert.equal(await lan.locator("#voice-dock-pill").count(), 0);
  assert.equal(invoked.length, before, "浏览器模式不调用桌面端命令");
  assert.deepEqual(lanErrors, [], "浏览器模式页面运行期无异常");
  await browserContext.close();

  console.log(`VOICE_DOCK_BROWSER_TEST=PASS posts=${posts.length} invokes=${invoked.length}`);
} finally {
  await browser?.close();
  await vite.close();
}
