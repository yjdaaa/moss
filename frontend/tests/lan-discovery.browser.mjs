// 工作台“扫描局域网”（2026-11-09）：后端只返回按 GRBL / FluidNC 回应了只读查询的设备；
// 多台时不自动填入、给出选择框，选中后填入 IP 与端口；只有 1 台时直接填入；一台都没有时说明原因、不改已填的 IP。
// 全部为回环假数据：不连设备、不扫真实网络。
// Run: node tests/lan-discovery.browser.mjs   (from frontend/; needs Playwright chromium, macOS 加 PLAYWRIGHT_CHANNEL=chrome)
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

  const fluidnc = { ip: "192.0.2.20", telnet_port: 23, hostname: "fluidnc", http_port: 80, source: "mdns+scan", firmware: "FluidNC v3.7.8", state: "Idle" };
  const grbl = { ip: "192.0.2.31", telnet_port: 2323, hostname: "", http_port: null, source: "scan", firmware: "Grbl 1.1h", state: "Alarm" };
  const state = { result: { controllers: [fluidnc, grbl], busy: [], checked: 5, not_controllers: 3, hint: "" }, scans: 0 };

  await context.addInitScript(() => {
    localStorage.setItem("laser-forge-conn-mode", "network");
    localStorage.setItem("laser-forge-host", "192.0.2.99");
    localStorage.setItem("laser-forge-telnet-port", "23");
  });
  await context.route("**/*", async (route) => {
    const target = new URL(route.request().url());
    if (target.origin === new URL(local).origin && !target.pathname.startsWith("/api/")) return route.continue();
    if (target.pathname === "/api/ui/discover-controllers") {
      state.scans += 1;
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: state.result }) });
    }
    if (target.hostname === "127.0.0.1" || target.origin === new URL(local).origin) {
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { workflows: [], jobs: [], ports: [], mcp: {}, approvals: [] } }) });
    }
    return route.abort("blockedbyclient");
  });

  await page.goto(local, { waitUntil: "domcontentloaded" });
  const host = page.getByPlaceholder("控制器 IP (例 192.168.1.10)");
  await host.waitFor({ timeout: 20_000 });
  const scan = page.getByRole("button", { name: /扫描局域网/ });
  const chooser = page.locator("#wb-discovered-controllers");

  // 1. 找到 2 台：不替用户选第一个，给出选择框；选中后填入 IP 与端口
  await scan.click();
  await chooser.waitFor();
  assert.equal(await host.inputValue(), "192.0.2.99", "多台时不自动改掉已填的 IP");
  const options = await chooser.locator("option").allInnerTexts();
  assert.deepEqual(options.slice(1), ["192.0.2.20:23 · fluidnc.local · FluidNC v3.7.8 · 空闲", "192.0.2.31:2323 · Grbl 1.1h · 报警锁定"]);
  await chooser.selectOption("192.0.2.31:2323");
  assert.equal(await host.inputValue(), "192.0.2.31");
  assert.equal(await page.locator("input[type=number]").first().inputValue(), "2323", "端口跟着填入");

  // 2. 只找到 1 台：直接填入，选择框消失
  state.result = { controllers: [fluidnc], busy: [], checked: 4, not_controllers: 3, hint: "" };
  await scan.click();
  await page.waitForFunction(() => document.querySelector('input[placeholder="控制器 IP (例 192.168.1.10)"]')?.value === "192.0.2.20");
  assert.equal(await chooser.count(), 0);

  // 3. 一台都没有：说明原因，不改已填的 IP
  state.result = { controllers: [], busy: [], checked: 3, not_controllers: 3, hint: "检查了 3 个地址，没有设备按 GRBL / FluidNC 回应" };
  await scan.click();
  await page.getByText("检查了 3 个地址，没有设备按 GRBL / FluidNC 回应").first().waitFor();
  assert.equal(await host.inputValue(), "192.0.2.20");
  assert.equal(state.scans, 3);

  // 4. 串口模式给出握手探测会打开串口、部分板子会重启的提示
  await page.locator("select.select-input.wb-field").first().selectOption("serial");
  await page.locator("#wb-serial-probe-hint").filter({ hasText: "自动重启" }).waitFor();

  assert.deepEqual(errors, [], "页面运行期无异常");
  console.log(`LAN_DISCOVERY_BROWSER_TEST=PASS scans=${state.scans}`);
} finally {
  await browser?.close();
  await vite.close();
}
