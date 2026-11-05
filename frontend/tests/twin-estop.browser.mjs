// 数字孪生页急停与 3D 场景回归（2026-11-05）。
// - 页面急停复用 usePageEmergencyStop：无运行任务时按页面连接目标写 0x18，只走 /api/safe-action；
// - 顶栏急停在任务查询挂起时也会限时让页面按本页目标补发，不无限等待；
// - 拖动板材 / 雕刻起点滑块不报错，离开页面卸载 3D 场景不报错。
// 全部为回环假数据：不连设备、不连真实后端。
// Run: node tests/twin-estop.browser.mjs   (from frontend/; needs Playwright chromium)
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

  const posts = [];
  const state = { hangLookup: false, releaseLookup: null };
  await page.exposeFunction("__fakeInvoke", async (command) => {
    if (command === "get_backend_origin") return "http://127.0.0.1:9666";
    if (command === "get_desktop_session") return "a".repeat(64);
    if (command === "voice_status") return { mode: "official", settings: {}, server: {}, device: {}, strict: {}, mcp: { endpoints: [], tools: [] } };
    throw new Error(`Synthetic Tauri command unavailable: ${command}`);
  });
  await context.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: (command, args) => window.__fakeInvoke(command, args) }, event: { listen: async () => () => {} } };
    // 工作台保存的连接参数（数字孪生页读取）；占位回环地址，不连真实设备
    localStorage.setItem("laser-forge-conn-mode", "network");
    localStorage.setItem("laser-forge-host", "127.0.0.1");
    localStorage.setItem("laser-forge-telnet-port", "2302");
  });
  await context.route("**/*", async (route) => {
    const request = route.request();
    const target = new URL(request.url());
    if (target.origin === new URL(local).origin) return route.continue();
    if (target.hostname === "127.0.0.1" && target.port === "9666") {
      const body = request.postDataJSON?.() || {};
      if (request.method() === "POST") posts.push({ path: target.pathname, body, at: Date.now() });
      if (target.pathname === "/api/jobs/stop-targets") {
        if (state.hangLookup) {
          await new Promise((resolve) => { state.releaseLookup = resolve; });
        }
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { jobs: [] } }) }).catch(() => {});
      }
      if (target.pathname === "/api/workflow" && body.action === "recent") {
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { workflows: [] } }) });
      }
      if (target.pathname === "/api/safe-action") {
        const result = body.action === "emergency_stop" ? "急停 0x18 已发送到 127.0.0.1:2302" : "<Idle|MPos:0.000,0.000,0.000|FS:0,0>";
        return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result }) });
      }
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { mcp: {}, approvals: [] } }) });
    }
    return route.abort("blockedbyclient");
  });

  const dismissAlert = async (title) => {
    // 提示框（alert）为 role="dialog"；确认框才是 alertdialog
    const dialog = page.getByRole("dialog").filter({ hasText: title });
    await dialog.waitFor({ timeout: 10_000 }).catch(async (error) => {
      const visible = await page.getByRole("dialog").allInnerTexts().catch(() => []);
      throw new Error(`${error.message}\n当前对话框: ${JSON.stringify(visible)}\n请求: ${JSON.stringify(posts.map((p) => [p.path, p.body]))}\n页面错误: ${JSON.stringify(errors)}`);
    });
    await dialog.getByRole("button", { name: "确定", exact: true }).click();
    await dialog.waitFor({ state: "detached" });
  };
  const estopPosts = () => posts.filter((item) => item.path === "/api/safe-action" && item.body.action === "emergency_stop");
  const expectedTarget = { action: "emergency_stop", confirmed: true, connection_mode: "network", host: "127.0.0.1", telnet_port: 2302 };

  await page.goto(new URL("twin", local).href, { waitUntil: "domcontentloaded" });
  const pageEstop = page.locator(".btn-estop-large");
  await pageEstop.waitFor({ timeout: 20_000 });

  // 1. 页面急停：无运行任务时不带旧 job_id，直接按页面连接目标写 0x18
  await pageEstop.click();
  await dismissAlert("已发送 0x18 急停");
  assert.deepEqual(estopPosts().map((item) => item.body), [expectedTarget]);

  // 2. 顶栏急停：任务查询挂起时，限时后页面按本页目标补发，不等查询返回
  state.hangLookup = true;
  const clickedAt = Date.now();
  await page.locator("#nav-estop").click();
  await dismissAlert("已发送 0x18 急停");
  const second = estopPosts()[1];
  assert.ok(second, "查询挂起时页面必须已补发急停");
  assert.deepEqual(second.body, expectedTarget);
  assert.ok(second.at - clickedAt < 3000, `页面补发不得排在挂起的查询之后（耗时 ${second.at - clickedAt} ms）`);
  state.hangLookup = false;
  state.releaseLookup?.();

  // 3. 拖动板材起点与雕刻位置滑块：只平移对象，不报错
  for (const label of ["拖动调节 X0 坐标", "拖动调节 Y0 坐标"]) {
    const slider = page.getByLabel(label);
    assert.equal(await slider.count(), 1, `${label} 滑块应存在`);
    for (const value of ["5", "10", "15", "0"]) await slider.fill(value);
  }

  const webgl = await page.evaluate(() => Boolean(document.querySelector("canvas")?.getContext?.("webgl2") || document.querySelector("canvas")?.getContext?.("webgl")));

  // 4. 离开页面：卸载 3D 场景（释放资源、丢弃 WebGL 上下文）不报错
  await page.evaluate(() => { window.history.pushState({}, "", "/"); window.dispatchEvent(new PopStateEvent("popstate")); });
  await page.waitForTimeout(500);

  assert.deepEqual(errors, [], "页面运行期无异常");
  assert.ok(posts.every((item) => item.path !== "/api/workflow" || item.body.action !== "cancel"), "急停绝不退化为 workflow cancel");
  console.log(`TWIN_ESTOP_BROWSER_TEST=PASS estops=${estopPosts().length} webgl=${webgl}`);
} finally {
  await browser?.close();
  await vite.close();
}
