// AI 生成任务的桌面预览窗浏览器测试（2026-11-06 用户决定：不管语音还是打字，AI 生成后必须在界面上
// 弹出预览，点同意才开刻；严格模式关闭时也可以口头确认，开启时只能在界面同意）。
// 全部为回环假数据：不连设备、不连真实后端。
// Run: node tests/ai-preview.browser.mjs   (from frontend/; needs Playwright)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
// 1×1 透明 PNG
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==", "base64");

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
  page.on("pageerror", (error) => errors.push(error.message));

  const now = Math.floor(Date.now() / 1000);
  const aiRecord = (id, overrides = {}) => ({
    workflow_id: id,
    status: "preview_ready",
    origin: "mcp_bridge",
    source_type: "text",
    created_at: now,
    input: { text: "生日快乐", material: "椴木", thickness_mm: 3 },
    artifacts: {
      image_file: `${id}.png`,
      width_mm: 60,
      height_mm: 20,
      actual_params: { laser_max_power: 420, power_scale: 1000, feed_rate: 1800, passes: 1 },
      time_estimate: { estimated_seconds: 150 },
      text_fill: "outline",
      origin: "center",
      origin_note: "开刻前把激光头移到木牌中心",
      frame_mm: { min_x: -30, max_x: 30, min_y: -10, max_y: 10 },
    },
    ...overrides,
  });

  // ---------- 有状态的假后端 ----------
  const posts = [];
  const state = {
    strict: false,
    workflows: [
      aiRecord("wf-ai-1"),
      // 桌面自己生成的预览、旧版没有来源的记录都不弹
      aiRecord("wf-desktop", { origin: "desktop" }),
      aiRecord("wf-legacy", { origin: undefined }),
    ],
    approvals: [],
  };
  const setStatus = (id, status) => {
    state.workflows = state.workflows.map((item) => (item.workflow_id === id ? { ...item, status } : item));
  };
  await page.exposeFunction("__fakeInvoke", async (command) => {
    if (command === "get_backend_origin") return "http://127.0.0.1:9666";
    if (command === "get_desktop_session") return "a".repeat(64);
    if (command === "voice_status") return { mode: "official", settings: {}, server: {}, device: {}, strict: { enabled: state.strict }, mcp: { endpoints: [], tools: [] } };
    throw new Error(`Synthetic Tauri command unavailable: ${command}`);
  });
  await context.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: (command, args) => window.__fakeInvoke(command, args) }, event: { listen: async () => () => {} } };
    // 雕刻工作台保存的设备目标（文档占位地址，回环假后端不会真的连接）
    localStorage.setItem("laser-forge-conn-mode", "network");
    localStorage.setItem("laser-forge-host", "192.0.2.10");
    localStorage.setItem("laser-forge-telnet-port", "23");
  });
  const json = (route, result) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result }) });
  await context.route("**/*", async (route) => {
    const request = route.request();
    const target = new URL(request.url());
    if (target.origin === new URL(local).origin) return route.continue();
    if (target.hostname === "127.0.0.1" && target.port === "9666") {
      const body = request.postDataJSON?.() || {};
      if (request.method() === "POST") posts.push({ path: target.pathname, body });
      if (target.pathname === "/api/workflow" && body.action === "recent") return json(route, { workflows: state.workflows });
      if (target.pathname === "/v1/approvals") return json(route, { approvals: state.approvals, server_time: now, ttl_seconds: 120 });
      if (target.pathname === "/api/workflow" && body.action === "confirm_send") {
        setStatus(body.workflow_id, "sending");
        return json(route, { workflow_id: body.workflow_id, status: "sending" });
      }
      if (target.pathname === "/api/workflow" && body.action === "cancel") {
        setStatus(body.workflow_id, "cancelled");
        return json(route, { workflow_id: body.workflow_id, status: "cancelled" });
      }
      if (target.pathname === "/api/workflow" && body.action === "status") {
        const record = state.workflows.find((item) => item.workflow_id === body.workflow_id);
        return json(route, record || { workflow_id: body.workflow_id, status: "sending" });
      }
      if (target.pathname === "/preview") return route.fulfill({ status: 200, contentType: "image/png", body: PNG });
      if (target.pathname === "/api/device/framing") return json(route, { laser_on: false });
      return json(route, { mcp: {}, approvals: [], jobs: [] });
    }
    return route.abort("blockedbyclient");
  });

  await page.goto(local, { waitUntil: "domcontentloaded" });

  // 1. AI 生成的预览在任何页面自动弹出，显示内容、材料、参数、尺寸、时长与工作台保存的设备
  const card = page.locator("#ai-preview-card");
  await card.waitFor({ timeout: 20_000 });
  const text = await card.innerText();
  assert.match(text, /小智生成了雕刻预览/);
  assert.match(text, /文字“生日快乐”/);
  assert.match(text, /椴木 · 3 mm/);
  assert.match(text, /功率 420\/1000（42%）/);
  assert.match(text, /60 × 20 mm/);
  assert.match(text, /约 3 分钟/);
  assert.equal(await page.locator("#ai-preview-target").innerText(), "192.0.2.10:23");
  await page.locator("#ai-preview-strict-hint").filter({ hasText: "也可以直接对小智说「确认开刻」" }).waitFor();
  assert.doesNotMatch(text, /wf-desktop|wf-legacy/);

  // 1b. 起点提示醒目显示；“先走边框”确认后以激光头当前位置为原点、不出光地绕预览范围一圈
  assert.equal(await page.locator("#ai-preview-origin").innerText(), "开刻前把激光头移到木牌中心");
  assert.match(await page.locator("#ai-preview-notes").innerText(), /刻法：只描轮廓/);
  await page.locator("#ai-preview-frame").click();
  const frameDialog = page.getByRole("alertdialog").filter({ has: page.getByRole("button", { name: "开始走边框", exact: true }) });
  await frameDialog.waitFor();
  assert.match(await frameDialog.innerText(), /60 × 20 mm/);
  await frameDialog.getByRole("button", { name: "开始走边框", exact: true }).click();
  await page.locator("#ai-preview-framing-status").filter({ hasText: "走边框完成" }).waitFor();
  assert.deepEqual(posts.find((item) => item.path === "/api/device/framing").body, {
    connection_mode: "network",
    network_host: "192.0.2.10",
    network_telnet_port: 23,
    relative_to_head: true,
    min_x: -30,
    max_x: 30,
    min_y: -10,
    max_y: 10,
    feed_rate: 2000,
    power: 0,
  });

  // 2. 不勾现场确认不能开刻；勾选后同意 → confirm_send 只带本任务与设备目标，随后显示发送进度
  assert.equal(await page.locator("#ai-preview-start").isDisabled(), true);
  await page.locator("#ai-preview-site-check").check();
  await page.locator("#ai-preview-start").click();
  await page.locator("#send-overlay.show").waitFor();
  assert.match(await page.locator("#send-title").innerText(), /AI 任务/);
  const send = posts.find((item) => item.body.action === "confirm_send");
  assert.deepEqual(send.body, {
    action: "confirm_send",
    workflow_id: "wf-ai-1",
    confirmed: true,
    connection_mode: "network",
    network_host: "192.0.2.10",
    network_telnet_port: 23,
  });
  setStatus("wf-ai-1", "completed");
  await page.locator("#send-overlay.show").waitFor({ state: "detached", timeout: 15_000 }).catch(() => {});
  await page.waitForFunction(() => !document.querySelector("#send-overlay.show") || /COMPLETED/.test(document.querySelector("#send-pct")?.textContent || ""), null, { timeout: 15_000 });
  const close = page.locator("#send-overlay.show button", { hasText: "关闭" });
  if (await close.count()) await close.first().click();

  // 3. 严格模式开启：提示只能在界面同意；“取消这个任务”走 workflow cancel
  state.strict = true;
  state.workflows = [aiRecord("wf-ai-2", { origin: "voice", created_at: now + 1 }), ...state.workflows];
  await page.locator("#ai-preview-card").filter({ hasText: "MOSS 语音生成了雕刻预览" }).waitFor({ timeout: 10_000 });
  await page.locator("#ai-preview-strict-hint").filter({ hasText: "口头确认不会直接开刻" }).waitFor();
  await page.locator("#ai-preview-discard").click();
  await card.waitFor({ state: "detached", timeout: 10_000 });
  assert.deepEqual(posts.filter((item) => item.body.action === "cancel").map((item) => item.body), [{ action: "cancel", workflow_id: "wf-ai-2" }]);

  // 4. 严格模式下 AI 口头确认生成了审批：交给审批卡，本窗口不重复弹出
  state.workflows = [aiRecord("wf-ai-3", { origin: "gateway", created_at: now + 2 }), ...state.workflows];
  state.approvals = [{ approval_id: "ap-1", status: "pending", created_at: now, kind: "workflow_confirm_send", target_record: "wf-ai-3", source: "gateway", digest: "d".repeat(64), summary: "开始加工", expires_at: now + 120, target: {} }];
  await page.locator("#approval-card").waitFor({ timeout: 10_000 });
  assert.equal(await page.locator("#ai-preview-card").count(), 0, "进入审批的任务不再弹 AI 预览窗");
  await page.locator("#approval-later").click();
  state.approvals = [];
  setStatus("wf-ai-3", "cancelled");

  // 5. 严格模式关闭：用户对小智口头确认、AI 开始发送后，预览窗自动切到发送进度（可取消 / 急停）
  state.strict = false;
  state.workflows = [aiRecord("wf-ai-4", { origin: "voice", created_at: now + 3 }), ...state.workflows];
  await page.locator("#ai-preview-card").filter({ hasText: "MOSS 语音生成了雕刻预览" }).waitFor({ timeout: 10_000 });
  setStatus("wf-ai-4", "sending");
  await page.locator("#send-overlay.show").waitFor({ timeout: 10_000 });
  assert.equal(await page.locator("#ai-preview-card").count(), 0);
  assert.equal(posts.filter((item) => item.body.action === "confirm_send").length, 1, "口头确认路径不由桌面再发一次");

  assert.deepEqual(errors, [], "页面运行期无异常");
  console.log(`AI_PREVIEW_BROWSER_TEST=PASS posts=${posts.length}`);
} finally {
  await browser?.close();
  await vite.close();
}
