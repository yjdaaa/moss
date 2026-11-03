// 激光任务进度浮层的浏览器测试（2026-11-02 用户决定）。
// 需求：任何页面都能看见正在雕刻的进度；「取消（记断点）」与「急停」各自走既有接口，绝不猜 job。
// 全部为回环假数据：不连设备、不连真实后端。
// Run: node tests/job-dock.browser.mjs   (from frontend/; needs Playwright + Edge)
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
  const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));

  // ---------- 有状态的假后端 ----------
  const posts = [];
  const savedExports = [];
  const previewAuthHeaders = [];
  const state = {
    jobs: [
      { job_id: "job-9", workflow_id: "wf-9", state: "running", phase: "streaming", mode: "network", target: "127.0.0.1:2301", lines_sent: 37, total_lines: 100, elapsed_ms: 62_000 },
    ],
  };
  await page.exposeFunction("__fakeInvoke", async (command, args = {}) => {
    if (command === "get_backend_origin") return "http://127.0.0.1:9666";
    if (command === "get_desktop_session") return "a".repeat(64);
    if (command === "voice_status") return { mode: "official", settings: {}, server: {}, device: {}, strict: {}, mcp: { endpoints: [], tools: [] } };
    if (command === "save_export_file") {
      savedExports.push({ filename: args.filename, size: Array.isArray(args.bytes) ? args.bytes.length : 0 });
      return `C:/Downloads/${args.filename}`;
    }
    if (command === "reveal_exported_file") return null;
    throw new Error(`Synthetic Tauri command unavailable: ${command}`);
  });
  await context.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: (command, args) => window.__fakeInvoke(command, args) }, event: { listen: async () => () => {} } };
  });
  await context.route("**/*", async (route) => {
    const request = route.request();
    const target = new URL(request.url());
    if (target.origin === new URL(local).origin) return route.continue();
    if (target.hostname === "127.0.0.1" && target.port === "9666") {
      const body = request.postDataJSON?.() || {};
      if (request.method() === "POST") posts.push({ path: target.pathname, body });
      if (target.pathname === "/api/jobs/stop-targets") {
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { jobs: state.jobs } }) });
      }
      if (target.pathname === "/api/workflow" && body.action === "cancel") {
        state.jobs = [];
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { workflow_id: body.workflow_id, status: "cancelled" } }) });
      }
      if (target.pathname === "/api/workflow" && body.action === "status") {
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { workflow_id: body.workflow_id, status: "sending", artifacts: { gcode_file: `${body.workflow_id}.gcode` } } }) });
      }
      if (target.pathname === "/preview") {
        previewAuthHeaders.push(request.headers()["x-moss-desktop-session"] || "");
        return route.fulfill({ status: 200, contentType: "text/plain", body: "G90\nG1 X10 Y10 F600\n" });
      }
      if (target.pathname === "/api/safe-action") {
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { job_id: body.job_id } }) });
      }
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { mcp: {}, approvals: [] } }) });
    }
    return route.abort("blockedbyclient");
  });

  const confirmDialog = async (button) => {
    const dialog = page.getByRole("alertdialog").filter({ has: page.getByRole("button", { name: button, exact: true }) });
    await dialog.waitFor();
    await dialog.getByRole("button", { name: button, exact: true }).click();
  };

  await page.goto(local, { waitUntil: "domcontentloaded" });

  // 1. 有活动任务时，浮层在任何页面都自动出现，并显示真实进度而不是编造的数字
  const dock = page.locator("#job-progress-dock");
  await dock.waitFor({ timeout: 20_000 });
  assert.equal(await page.locator("#job-progress-percent-0").innerText(), "37%");
  assert.match(await dock.innerText(), /已发 37 \/ 100 行/);
  assert.match(await dock.innerText(), /127\.0\.0\.1:2301/);
  assert.match(await dock.innerText(), /wf-9/);

  // 2. 「取消（记断点）」走 workflow cancel，只带当前 workflow_id
  await page.locator("#job-progress-cancel-0").click();
  await confirmDialog("取消加工");
  await page.locator("#job-progress-note").filter({ hasText: "已请求取消" }).waitFor();
  const cancel = posts.find((item) => item.path === "/api/workflow" && item.body.action === "cancel");
  assert.deepEqual(cancel.body, { action: "cancel", workflow_id: "wf-9" }, "取消只提交当前 workflow_id");

  // 3. 任务消失后只说“已结束”，不冒充雕刻成功
  await page.locator(".job-dock-note").filter({ hasText: /已完成、已取消或已失败/ }).waitFor({ timeout: 10_000 });

  // 4. 新任务出现时改走急停：必须带 confirmed 与原 job_id
  state.jobs = [{ job_id: "job-10", workflow_id: "wf-10", state: "running", phase: "awaiting_completion", mode: "serial", target: "COM3", lines_sent: 100, total_lines: 100, elapsed_ms: 90_000 }];
  await page.locator("#job-progress-estop-0").click({ timeout: 15_000 });
  await confirmDialog("急停");
  await page.locator("#job-progress-note").filter({ hasText: "已发出急停指令" }).waitFor();
  const estop = posts.find((item) => item.path === "/api/safe-action");
  assert.deepEqual(estop.body, { action: "emergency_stop", confirmed: true, job_id: "job-10" }, "急停带 confirmed 与原 job_id");

  // 5. G-code 下载走鉴权 apiFetch + saveFile，不走裸 window.open；下载弹窗打开期间绝不禁用急停与取消
  await page.locator("#job-progress-gcode-0").click();
  await page.getByRole("alertdialog").waitFor();
  assert.equal(await page.locator("#job-progress-gcode-0").isDisabled(), true);
  assert.equal(await page.locator("#job-progress-estop-0").isDisabled(), false, "下载未结束时急停按钮必须保持可用");
  assert.equal(await page.locator("#job-progress-cancel-0").isDisabled(), false, "下载未结束时取消按钮必须保持可用");
  await confirmDialog("关闭");
  await page.locator("#job-progress-note").filter({ hasText: "G-code 已保存到：C:/Downloads/wf-10.gcode" }).waitFor();
  assert.deepEqual(savedExports, [{ filename: "wf-10.gcode", size: 20 }]);
  assert.deepEqual(previewAuthHeaders, ["a".repeat(64)], "下载携带桌面会话头");

  // 6. 收成胶囊后仍显示进度，可再展开；未知态仍允许重试 0x18 急停
  await page.locator("#job-progress-minimize").click();
  await page.locator("#job-progress-pill").waitFor();
  assert.equal(await page.locator("#job-progress-pill-text").innerText(), "100%");
  await page.locator("#job-progress-pill-open").click();
  await page.locator("#job-progress-dock").waitFor();

  state.jobs = [{ job_id: "job-unknown", workflow_id: "wf-unknown", state: "unknown", mode: "network", target: "127.0.0.1:2301" }];
  await page.locator("#job-progress-percent-0").filter({ hasText: "进度未知" }).waitFor({ timeout: 10_000 });
  await page.locator("#job-progress-estop-0").click();
  await confirmDialog("急停");
  const unknownEstop = posts.find((item) => item.path === "/api/safe-action" && item.body.job_id === "job-unknown");
  assert.deepEqual(unknownEstop.body, { action: "emergency_stop", confirmed: true, job_id: "job-unknown" }, "未知态仍可发 0x18 急停");

  assert.deepEqual(errors, [], "页面运行期无异常");
  console.log(`JOB_DOCK_BROWSER_TEST=PASS posts=${posts.length}`);
} finally {
  await browser?.close();
  await vite.close();
}
