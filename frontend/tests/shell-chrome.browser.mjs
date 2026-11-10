// 外壳浏览器测试（2026-11-09）：左侧导航可收起为图标栏、顶栏可精简，两者记在本机；
// 精简顶栏时急停按钮保持完整；侧栏与顶栏为半透明。真 React 页面 + 隔离 Vite 回环 + 假桌面端。
// Run: node tests/shell-chrome.browser.mjs   (from frontend/; needs Playwright + Edge/Chrome)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
const shotDir = process.env.SHELL_SHOT_DIR || "";

const vite = await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
let browser;
try {
  await vite.listen();
  const local = vite.resolvedUrls?.local?.[0];
  if (!local) throw new Error("Vite loopback server URL unavailable");
  browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) });
  const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    window.__TAURI__ = {
      core: {
        invoke: async (command) => {
          if (command === "get_backend_origin") return "http://127.0.0.1:9666";
          if (command === "get_desktop_session") return "a".repeat(64);
          return null;
        },
      },
      event: { listen: async () => () => {} },
    };
  });
  await context.route("**/*", async (route) => {
    const target = new URL(route.request().url());
    if (target.origin === new URL(local).origin) return route.continue();
    if (target.hostname === "127.0.0.1" && target.port === "9666") {
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: {} }) });
    }
    return route.abort("blockedbyclient");
  });

  await page.goto(new URL("index.html", local).href, { waitUntil: "domcontentloaded" });
  const side = page.locator("aside.ob-side");
  const top = page.locator("header.ob-top");
  await side.waitFor({ timeout: 20_000 });
  const settle = () => page.waitForTimeout(400);
  const sideWidth = async () => Math.round((await side.boundingBox()).width);
  const alphaOf = (locator) =>
    locator.evaluate((el) => {
      const style = getComputedStyle(el);
      const color = style.backgroundColor !== "rgba(0, 0, 0, 0)" ? style.backgroundColor : style.backgroundImage;
      const alphas = [...color.matchAll(/rgba\([^)]*,\s*([\d.]+)\)/g)].map((m) => Number(m[1]));
      return alphas.length ? Math.max(...alphas) : 1;
    });

  // 默认：侧栏展开、顶栏完整，且都是半透明
  assert.ok((await sideWidth()) >= 200, "默认侧栏展开");
  assert.equal(await side.getAttribute("data-collapsed"), "false");
  assert.equal(await top.getAttribute("data-compact"), "false");
  assert.ok((await alphaOf(side)) <= 0.55, "侧栏半透明");
  assert.ok((await alphaOf(top)) <= 0.55, "顶栏半透明");
  assert.match(await page.locator("#nav-hardware-status").innerText(), /\S/, "顶栏状态胶囊带文字");
  if (shotDir) await page.screenshot({ path: path.join(shotDir, "shell-default.png") });

  // 收起侧栏：只留图标，名称放到悬停提示，内容区跟着变宽
  await page.locator("#side-collapse-toggle").click();
  await settle();
  assert.equal(await side.getAttribute("data-collapsed"), "true");
  assert.ok((await sideWidth()) <= 80, `收起后侧栏只留图标宽度，实际 ${await sideWidth()}`);
  const firstItem = side.locator(".ob-nav .ob-nav-item").first();
  assert.equal(await firstItem.getAttribute("title"), "雕刻工作台", "收起后悬停显示名称");
  assert.equal(await firstItem.locator("span").first().isVisible(), false, "收起后不显示文字");
  assert.equal(await page.locator("#side-collapse-toggle").getAttribute("aria-expanded"), "false");

  // 精简顶栏：化为右上角悬浮胶囊，释放顶部全宽高度，状态只留图标，急停按钮文字不变，切换按钮固定在右侧不跳位
  const toggleXBefore = (await page.locator("#top-compact-toggle").boundingBox()).x;
  await page.locator("#top-compact-toggle").click();
  await settle();
  assert.equal(await top.getAttribute("data-compact"), "true");
  assert.equal(await top.locator(".ob-title").isVisible(), false, "收起后隐藏左侧大标题，释放全宽顶栏");
  const topBox = await top.boundingBox();
  assert.ok(topBox.width < 420, `收起后化为右上角紧凑悬浮胶囊，实际宽度 ${topBox.width}`);
  const toggleXAfter = (await page.locator("#top-compact-toggle").boundingBox()).x;
  assert.ok(Math.abs(toggleXAfter - toggleXBefore) <= 24, `收起按钮固定在右侧不跳位 (${toggleXBefore} -> ${toggleXAfter})`);
  assert.equal(await page.evaluate(() => document.documentElement.style.getPropertyValue("--ob-top-h")), "0px");
  assert.match(await page.evaluate(() => document.documentElement.style.getPropertyValue("--ob-compact-top-w")), /^\d+px$/, "收起后写入实测胶囊避让宽度");
  assert.equal(await page.locator("#nav-hardware-status span").isVisible(), false, "精简后状态胶囊只留图标");
  assert.equal(await page.locator("#nav-hardware-status svg").isVisible(), true);
  assert.equal((await page.locator("#nav-estop").innerText()).trim(), "急停 0x18", "急停按钮保持完整");
  assert.ok(await page.locator("#nav-estop").isVisible());
  if (shotDir) await page.screenshot({ path: path.join(shotDir, "shell-collapsed.png") });

  // 键盘聚焦 <select> 按空格弹出曜石毛玻璃浮层，按 Escape 收起
  const firstSelect = page.locator("select").first();
  if (await firstSelect.count()) {
    await firstSelect.focus();
    await page.keyboard.press(" ");
    await page.locator(".ob-select-popover").waitFor({ timeout: 5_000 });
    await page.keyboard.press("Escape");
    await page.locator(".ob-select-popover").waitFor({ state: "detached", timeout: 5_000 });
  }

  // 点击顶栏小智状态胶囊可直达语音中心，再点硬件状态胶囊回到工作台
  await page.locator("#nav-mcp-status").click();
  await settle();
  assert.equal(new URL(page.url()).pathname, "/voice", "点击小智胶囊跳转语音中心");
  await page.locator("#nav-hardware-status").click();
  await settle();
  assert.equal(new URL(page.url()).pathname, "/", "点击硬件胶囊返回工作台");

  // 记在本机：刷新后保持
  await page.reload({ waitUntil: "domcontentloaded" });
  await side.waitFor();
  await settle();
  assert.equal(await side.getAttribute("data-collapsed"), "true", "刷新后侧栏仍收起");
  assert.equal(await top.getAttribute("data-compact"), "true", "刷新后顶栏仍精简");

  // 再点一次恢复
  await page.locator("#side-collapse-toggle").click();
  await page.locator("#top-compact-toggle").click();
  await settle();
  assert.ok((await sideWidth()) >= 200, "展开后恢复原宽度");
  assert.equal(await page.locator("#nav-hardware-status span").isVisible(), true);

  // 窄屏：侧栏本来就是横排，不显示收起按钮
  await page.setViewportSize({ width: 820, height: 900 });
  await settle();
  assert.equal(await page.locator("#side-collapse-toggle").isVisible(), false, "窄屏不显示收起按钮");

  assert.deepEqual(errors, [], "页面无脚本错误");
  console.log("SHELL_CHROME_BROWSER_TEST=PASS");
} finally {
  await browser?.close();
  await vite.close();
}
