// 左侧导航「摄像头」标签页端到端浏览器实测（2026-11-07）
// 验证：
// 1. 桌面端左侧栏显示「摄像头」标签，点击进入 /camera 时绝不自动触发硬件拍照（隐私边界）。
// 2. 点击「抓拍一张」后调用 capture_photo，渲染回传画面与分辨率，支持水平镜像/旋转 180°。
// 3. 开启「自动刷新」后，当小智进入 speaking 对话状态时自动暂缓抓拍并给出提示。
// 4. 语音拍照问答（source: "voice"）同步显示提问与视觉模型回答；点击「清空画面」立即清除。
// Run: PLAYWRIGHT_CHANNEL=chrome node tests/camera.browser.mjs (from frontend/)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));

const ONE_PIXEL_PNG =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

const vite = await createServer({
  root,
  server: { host: "127.0.0.1", port: 0, strictPort: false },
  logLevel: "error",
});
let browser;
try {
  await vite.listen();
  const local = vite.resolvedUrls?.local?.[0];
  if (!local) throw new Error("Vite loopback server URL unavailable");
  browser = await chromium.launch({
    headless: true,
    channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined),
  });

  const context = await browser.newContext({
    serviceWorkers: "block",
    viewport: { width: 1380, height: 900 },
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (err) => errors.push(err.message));

  await page.addInitScript(({ pngDataUrl, loopbackOrigin }) => {
    window.__CAM_STATE__ = {
      captureCount: 0,
      clearCount: 0,
      sessionState: "idle",
      frame: null,
      lastError: null,
      errorSeq: null,
      seq: 0,
    };
    window.__TAURI__ = {
      core: {
        invoke: async (cmd, args) => {
          if (cmd === "get_backend_origin") return loopbackOrigin;
          if (cmd === "get_desktop_session") return "b".repeat(64);
          if (cmd === "check_app_update") {
            return { current_version: "0.3.8", latest_version: "0.3.8", has_update: false };
          }
          if (cmd === "voice_status") {
            return {
              mode: "builtin",
              server: { state: "stopped", owned: false },
              strict: { enabled: true, locked: false, from_env: false },
              mcp: { endpoints: [], tools: [], max_extra_endpoints: 4 },
            };
          }
          if (cmd === "voice_builtin_status") {
            return {
              running: true,
              mode: "chat",
              link_mode: "builtin",
              ws_port: 8000,
              http_port: 8003,
              models: {},
              local: { settings: {}, models: [], ready: {} },
              sessions: [
                {
                  session_id: "sess-1",
                  device_id: "aa:bb:cc:11:22:33",
                  ip: "192.168.31.88",
                  state: window.__CAM_STATE__.sessionState,
                  level: 0,
                  device_tools: ["self.get_device_status", "self.camera.take_photo"],
                },
              ],
              events: [],
              timings: [],
              devices: [
                {
                  device_id: "aa:bb:cc:11:22:33",
                  name: "实战派小智S3",
                  model: "lichuang-dev",
                  firmware_version: "2.0.4",
                  ip: "192.168.31.88",
                  status: "allowed",
                  last_seen_at: 1790000000,
                },
              ],
              memory_enabled: false,
            };
          }
          if (cmd === "voice_device_request") {
            const req = args?.request;
            if (req === "capture_photo") {
              window.__CAM_STATE__.captureCount += 1;
              window.__CAM_STATE__.seq += 1;
              window.__CAM_STATE__.lastError = null;
              window.__CAM_STATE__.errorSeq = null;
              window.__CAM_STATE__.frame = {
                seq: window.__CAM_STATE__.seq,
                captured_at: 1790000123,
                source: "preview",
                question: "",
                summary: "",
                mime: "image/jpeg",
                width: 320,
                height: 240,
                size_bytes: 14320,
                data_url: pngDataUrl,
              };
              return { requested: "capture_photo", ticket_seq: window.__CAM_STATE__.seq };
            }
            if (req === "camera_frame") {
              const afterSeq = args?.value?.after_seq ?? null;
              const current = window.__CAM_STATE__.frame;
              const unchanged = Boolean(current && afterSeq === current.seq);
              return {
                running: true,
                online: true,
                session_state: window.__CAM_STATE__.sessionState,
                has_camera_tool: true,
                unchanged,
                pending: false,
                last_error: window.__CAM_STATE__.lastError,
                error_seq: window.__CAM_STATE__.errorSeq,
                frame_seq: current ? current.seq : null,
                frame: unchanged ? null : current,
              };
            }
            if (req === "clear_camera_frame") {
              window.__CAM_STATE__.clearCount += 1;
              window.__CAM_STATE__.frame = null;
              window.__CAM_STATE__.lastError = null;
              window.__CAM_STATE__.errorSeq = null;
              return { requested: "clear_camera_frame" };
            }
          }
          return {};
        },
      },
      event: { listen: async () => () => {} },
    };
  }, { pngDataUrl: ONE_PIXEL_PNG, loopbackOrigin: local.replace(/\/$/, "") });

  await context.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (
      url.pathname.startsWith("/api/")
      || url.pathname.startsWith("/v1/")
    ) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ success: true, result: { mcp: {}, approvals: [], items: [] } }),
      });
    }
    if (url.origin === new URL(local).origin) {
      return route.continue();
    }
    return route.abort("blockedbyclient");
  });

  await page.goto(local, { waitUntil: "domcontentloaded" });
  const navBtn = page.getByRole("navigation").getByRole("button", { name: /摄像头/ });
  await navBtn.waitFor({ state: "visible", timeout: 8000 });
  await navBtn.click();
  assert.equal(new URL(page.url()).pathname, "/camera", "点击左侧栏摄像头标签应切换到 /camera");

  await page.locator("#camera-page").waitFor({ state: "visible", timeout: 5000 });
  const topTitle = await page.locator("header.ob-top h1.ob-title").textContent();
  assert.match(topTitle || "", /摄像头/, "顶栏标题应切换为摄像头而不是工作台");
  await page.waitForTimeout(450);

  // 1. 打开页面不得静默触发拍照
  const initialCaptures = await page.evaluate(() => window.__CAM_STATE__.captureCount);
  assert.equal(initialCaptures, 0, "仅切换到摄像头页时严禁自动触发硬件拍照");
  assert.equal(await page.locator("#camera-placeholder").count(), 1);

  // 2. 点击「抓拍一张」：调用 capture_photo 并显示画面与分辨率
  await page.locator("#camera-capture-btn").click();
  await page.locator("#camera-frame-img").waitFor({ state: "visible", timeout: 5000 });
  assert.equal((await page.locator("#camera-resolution").innerText()).trim(), "320 × 240");
  assert.equal((await page.locator("#camera-source-pill").innerText()).trim(), "桌面抓拍");

  // 3. 镜像与旋转 180° 切换
  await page.locator("#camera-mirror-btn").click();
  await page.locator("#camera-flip-btn").click();
  const transformStyle = await page
    .locator("#camera-frame-img")
    .evaluate((el) => el.style.transform);
  assert.equal(transformStyle, "scaleX(-1) rotate(180deg)");

  // 4. 自动刷新在小智 speaking 时自动让路暂缓
  await page.evaluate(() => {
    window.__CAM_STATE__.sessionState = "speaking";
  });
  await page.locator("#camera-auto-toggle").click();
  await page.locator("#camera-auto-paused-reason").waitFor({ state: "visible", timeout: 5000 });
  const pausedText = await page.locator("#camera-auto-paused-reason").innerText();
  assert.match(pausedText, /自动刷新已暂缓/);

  // 4b. 抓拍报错时自动停止自动刷新；用户再次点击开启自动刷新时不被旧 error_seq 反弹关掉，能正常重拍
  await page.evaluate(() => {
    window.__CAM_STATE__.sessionState = "idle";
    window.__CAM_STATE__.seq += 1;
    window.__CAM_STATE__.errorSeq = window.__CAM_STATE__.seq;
    window.__CAM_STATE__.lastError = "设备摄像头抓拍失败，请检查排线";
  });
  await page.waitForFunction(
    () => document.querySelector("#camera-auto-toggle")?.getAttribute("aria-pressed") === "false",
    null,
    { timeout: 5000 },
  );
  const countBeforeRetry = await page.evaluate(() => window.__CAM_STATE__.captureCount);
  await page.locator("#camera-auto-toggle").click();
  await page.waitForFunction(
    (prev) => window.__CAM_STATE__.captureCount > prev,
    countBeforeRetry,
    { timeout: 5000 },
  );
  assert.equal(
    await page.locator("#camera-auto-toggle").getAttribute("aria-pressed"),
    "true",
    "重新开启自动刷新后不应被旧报错反弹关闭",
  );
  await page.locator("#camera-auto-toggle").click();

  // 5. 模拟小智语音触发「拍一下看看」同步展示问答结果，随后点「清空画面」清除
  await page.evaluate((pngDataUrl) => {
    window.__CAM_STATE__.sessionState = "idle";
    window.__CAM_STATE__.seq += 1;
    window.__CAM_STATE__.frame = {
      seq: window.__CAM_STATE__.seq,
      captured_at: 1790000200,
      source: "voice",
      question: "拍一下看看桌面上是什么",
      summary: "桌面上有一块椴木板和激光头。",
      mime: "image/jpeg",
      width: 640,
      height: 480,
      size_bytes: 28400,
      data_url: pngDataUrl,
    };
  }, ONE_PIXEL_PNG);
  await page.locator("#camera-voice-qa").waitFor({ state: "visible", timeout: 5000 });
  const qaText = await page.locator("#camera-voice-qa").innerText();
  assert.match(qaText, /拍一下看看桌面上是什么/);
  assert.match(qaText, /椴木板和激光头/);

  await page.locator("#camera-clear-btn").click();
  await page.locator("#camera-placeholder").waitFor({ state: "visible", timeout: 5000 });
  const clearCount = await page.evaluate(() => window.__CAM_STATE__.clearCount);
  assert.equal(clearCount, 1);
  assert.deepEqual(errors, [], "摄像头页不应产生前端异常");
  console.log("CAMERA_BROWSER_TEST=PASS");
  await context.close();
} finally {
  if (browser) await browser.close();
  await vite.close();
}
