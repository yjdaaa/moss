// 数字孪生与动态背景的渲染开销回归（2026-11-08）。
// - 反复进出孪生页只复用一个 WebGL 渲染器：此前每次进入都新建上下文，macOS WebKit 每次留下约 100 MB 不还；
// - 孪生画面静止时（推演暂停或播完）不重画（按需渲染），推演播放、拖动 / 滚轮缩放视角时才出帧；
// - 窗口失去焦点时动态背景与页面 CSS 循环动画一起暂停，回到前台继续。
// 全部为回环假数据：不连设备、不连真实后端。
// Run: node tests/twin-render.browser.mjs   (from frontend/; needs Playwright chromium, macOS 加 PLAYWRIGHT_CHANNEL=chrome)
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

  await context.addInitScript(() => {
    // 统计新建的 WebGL 上下文个数与 clear 调用次数（three.js 每渲染一帧至少 clear 一次；背景着色器不调用 clear）
    window.__gl = { contexts: 0, clears: 0 };
    const seen = new WeakSet();
    const getContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function patchedGetContext(type, ...rest) {
      const ctx = getContext.call(this, type, ...rest);
      if (ctx && /webgl/.test(String(type)) && !seen.has(ctx)) {
        seen.add(ctx);
        window.__gl.contexts += 1;
      }
      return ctx;
    };
    for (const proto of [window.WebGLRenderingContext?.prototype, window.WebGL2RenderingContext?.prototype]) {
      if (!proto) continue;
      const clear = proto.clear;
      proto.clear = function patchedClear(...args) {
        window.__gl.clears += 1;
        return clear.apply(this, args);
      };
    }
  });
  await context.route("**/*", async (route) => {
    const target = new URL(route.request().url());
    if (target.origin === new URL(local).origin) return route.continue();
    if (target.hostname === "127.0.0.1") {
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { workflows: [], jobs: [], mcp: {}, approvals: [] } }) });
    }
    return route.abort("blockedbyclient");
  });

  const go = (to) => page.evaluate((p) => { window.history.pushState({}, "", p); window.dispatchEvent(new PopStateEvent("popstate")); }, to);
  const gl = () => page.evaluate(() => ({ ...window.__gl }));
  const clearsDuring = async (ms, action) => {
    const before = (await gl()).clears;
    if (action) await action();
    await page.waitForTimeout(ms);
    return (await gl()).clears - before;
  };
  const stage = page.locator(".twin-webgl-canvas canvas");

  await page.goto(new URL("twin", local).href, { waitUntil: "domcontentloaded" });
  await stage.waitFor({ timeout: 20_000 });
  // 进入孪生页默认播放离线推演（激光头在动，每帧都要画）；先暂停，再检查静止画面
  const pausePlayback = async () => {
    const pause = page.getByRole("button", { name: "暂停推演" });
    if (await pause.count()) await pause.click();
  };
  await pausePlayback();
  await page.waitForTimeout(1500);

  // 1. 静止时不重画：2 秒内最多偶发几帧（页面轮询引起的重新渲染），常驻 60 fps 时是数百次
  const idle = await clearsDuring(2000);
  assert.ok(idle <= 12, `孪生画面静止时不应持续重画（2 秒内 clear ${idle} 次）`);

  // 推演播放时照常逐帧画
  const playing = await clearsDuring(500, () => page.getByRole("button", { name: "播放推演" }).click());
  assert.ok(playing > 10, `推演播放时应逐帧重画（0.5 秒内 clear ${playing} 次）`);
  await pausePlayback();
  await page.waitForTimeout(300);

  // 2. 滚轮缩放视角时要出帧
  const box = await stage.boundingBox();
  const zoomed = await clearsDuring(400, async () => {
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.wheel(0, 240);
  });
  assert.ok(zoomed > 0, "滚轮缩放后应重画");

  // 3. 拖动旋转视角时要出帧
  const dragged = await clearsDuring(300, async () => {
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 80, box.y + box.height / 2 + 20, { steps: 6 });
    await page.mouse.up();
  });
  assert.ok(dragged > 0, "拖动视角后应重画");

  // 4. 反复进出孪生页：WebGL 上下文最多两个（动态背景 1 个 + 孪生共享 1 个）
  for (let i = 0; i < 3; i += 1) {
    await go("/");
    await page.waitForTimeout(300);
    await go("/twin");
    await stage.waitFor({ timeout: 20_000 });
    await page.waitForTimeout(300);
  }
  await pausePlayback();
  const { contexts } = await gl();
  assert.ok(contexts <= 2, `反复进出孪生页应复用渲染器（新建了 ${contexts} 个 WebGL 上下文）`);
  await page.waitForTimeout(1200);
  const idleAgain = await clearsDuring(1500);
  assert.ok(idleAgain <= 10, `重新进入后静止时也不应持续重画（1.5 秒内 clear ${idleAgain} 次）`);

  // 5. 动态背景：窗口失去焦点暂停，回到前台继续
  const bgState = () => page.locator(".plasma-bg").getAttribute("data-state");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  assert.equal(await bgState(), "running");
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  assert.equal(await bgState(), "paused", "窗口不在前台时背景应暂停");
  const cssPaused = await page.evaluate(() => ({
    flag: document.documentElement.dataset.appActive,
    states: document.getAnimations().filter((a) => a instanceof CSSAnimation).map((a) => getComputedStyle(a.effect.target).animationPlayState),
  }));
  assert.equal(cssPaused.flag, "false");
  assert.ok(cssPaused.states.length > 0, "页面上应有可检查的 CSS 循环动画（顶栏状态眼等）");
  assert.ok(cssPaused.states.every((state) => state.split(",").every((s) => s.trim() === "paused")), `后台时 CSS 循环动画应暂停：${JSON.stringify(cssPaused.states)}`);
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  assert.equal(await bgState(), "running", "回到前台后背景继续");

  assert.deepEqual(errors, [], "页面运行期无异常");
  console.log(`TWIN_RENDER_BROWSER_TEST=PASS idle_clears=${idle} contexts=${contexts} zoom_clears=${zoomed}`);
} finally {
  await browser?.close();
  await vite.close();
}
