// 工作台自由画布浏览器测试：真 React 页面 + 隔离 Vite 回环 + 假桌面端。
// 覆盖：五张卡按栅格定位、拖动后落盘、刷新后保持、还原默认布局、锁定后不能再拖。
// Run: node tests/workbench-layout.browser.mjs   (from frontend/; needs Playwright + Edge)
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
  const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1600, height: 1000 } });
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
  await page.locator(".ob-canvas").waitFor({ timeout: 20_000 });

  const card = (id) => page.locator(`[data-wb-card="${id}"]`);
  for (const id of ["link", "source", "material", "monitor", "feedback"]) {
    assert.equal(await card(id).count(), 1, `${id} 应在画布里`);
  }
  const gridOf = async (id) => (await card(id).evaluate((el) => el.style.gridRow)) || "";
  const colOf = async (id) => (await card(id).evaluate((el) => el.style.gridColumn)) || "";
  const initialLinkRow = await gridOf("link");
  assert.match(initialLinkRow, /^1 \/ span \d+$/, "默认第一张卡在第 1 行");
  const initialSpanMatch = initialLinkRow.match(/span (\d+)$/);
  assert.ok(initialSpanMatch, "默认应带有效 span 行数");
  const initialSpan = initialSpanMatch[1];

  // 验证左窄（16 列）右宽（32 列）双栏排布，且左右两栏底边对齐不留大截空位
  assert.equal(await colOf("link"), "1 / span 16", "左栏卡片占 16 列");
  assert.equal(await colOf("monitor"), "17 / span 32", "右栏加工监视舱占 32 列");
  assert.equal(await colOf("feedback"), "17 / span 32", "右栏加工反馈紧贴监视舱下方占 32 列");
  const matBox = await card("material").boundingBox();
  const fbBox = await card("feedback").boundingBox();
  assert.ok(
    Math.abs((matBox.y + matBox.height) - (fbBox.y + fbBox.height)) <= 16,
    `双栏左右底边应齐平，实际左底 ${matBox.y + matBox.height}px vs 右底 ${fbBox.y + fbBox.height}px`,
  );

  // 切换左栏加工内容模式（矢量文字 -> 图片处理 -> 矢量文字）时，右栏监视舱高度自动跟随对齐
  await page.locator('.hud-tool-card:has-text("图片处理")').click();
  await page.waitForTimeout(100);
  const matBoxImg = await card("material").boundingBox();
  const fbBoxImg = await card("feedback").boundingBox();
  assert.ok(
    Math.abs((matBoxImg.y + matBoxImg.height) - (fbBoxImg.y + fbBoxImg.height)) <= 16,
    `切换到图片处理后双栏左右底边仍应齐平，实际左底 ${matBoxImg.y + matBoxImg.height}px vs 右底 ${fbBoxImg.y + fbBoxImg.height}px`,
  );
  await page.locator('.hud-tool-card:has-text("矢量文字")').click();
  await page.waitForTimeout(100);

  // 验证卡片高度紧贴内部内容，底部不留大片空白框
  for (const id of ["link", "source", "material", "feedback"]) {
    const gapPx = await card(id).evaluate((el) => {
      const content = el.querySelector(".wb-card-content");
      return el.clientHeight - (content ? content.offsetHeight + 36 : 0);
    });
    assert.ok(gapPx <= 24, `${id} 卡片底部不应出现大片空白，实际多余高度 ${gapPx}px`);
  }
  const monitorGapPx = await card("monitor").evaluate((el) => {
    const dispatch = el.querySelector(".job-dispatch-card");
    if (!dispatch) return 0;
    const cardRect = el.getBoundingClientRect();
    const dispatchRect = dispatch.getBoundingClientRect();
    return cardRect.bottom - dispatchRect.bottom;
  });
  assert.ok(monitorGapPx <= 32, `加工监视舱底部进度栏下方不应出现大片空白，实际距离底边 ${monitorGapPx}px`);

  const drag = async (id, dx, dy) => {
    const box = await card(id).boundingBox();
    const startX = box.x + Math.min(60, box.width / 2);
    const startY = box.y + 20; // 卡片头部
    await page.mouse.move(startX, startY);
    await page.mouse.down();
    await page.mouse.move(startX + dx / 2, startY + dy / 2, { steps: 4 });
    await page.mouse.move(startX + dx, startY + dy, { steps: 6 });
    await page.mouse.up();
  };

  // 1. 往下拖「通信链路」，位置要变且写进 localStorage
  await drag("link", 0, 320);
  const movedRow = await gridOf("link");
  assert.notEqual(movedRow, initialLinkRow, `拖动后应换行，实际 ${movedRow}`);
  const stored = await page.evaluate(() => globalThis.localStorage.getItem("moss_wb_layout_v3"));
  assert.ok(stored && stored.includes("version"), "布局应写进 localStorage");

  // 2. 刷新后保持
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(".ob-canvas").waitFor();
  assert.equal(await gridOf("link"), movedRow, "刷新后应保持用户拖过的高度");

  // 3. 折叠只留标题条，内容隐藏且移除纵向缩放把手，展开恢复原来的高度
  const foldButton = page.locator('[data-wb-card="link"] .ob-card-h button');
  await foldButton.click();
  assert.match(await gridOf("link"), /span 8$/, "折叠后高度应收到标题条");
  assert.equal(await card("link").getAttribute("data-wb-collapsed"), "true", "折叠后标记 data-wb-collapsed=true");
  assert.equal(await page.locator('[data-wb-card="link"] [data-wb-resize="s"]').count(), 0, "折叠后移除底部高度缩放把手");
  assert.equal(await page.locator('[data-wb-card="link"] .wb-btn').isVisible(), false, "折叠后卡片主体内容彻底隐藏");
  await foldButton.click();
  assert.match(await gridOf("link"), new RegExp(`span ${initialSpan}$`), "展开应恢复原来的高度");
  assert.equal(await page.locator('[data-wb-card="link"] .wb-btn').isVisible(), true, "展开后卡片主体内容重新显示");

  // 4. 拖右边框可以改宽度
  const beforeCols = await colOf("material");
  const handle = page.locator('[data-wb-card="material"] [data-wb-resize="e"]');
  await handle.scrollIntoViewIfNeeded();
  const handleBox = await handle.boundingBox();
  await page.mouse.move(handleBox.x + handleBox.width / 2, handleBox.y + handleBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(handleBox.x + 220, handleBox.y + handleBox.height / 2, { steps: 8 });
  await page.mouse.up();
  assert.notEqual(await colOf("material"), beforeCols, "拖右边框应改变宽度");

  // 5. 还原默认布局
  await page.locator("#wb-layout-reset").click();
  assert.equal(await gridOf("link"), initialLinkRow, "还原后回到默认位置");
  assert.equal(await page.evaluate(() => globalThis.localStorage.getItem("moss_wb_layout_v3")), null, "还原应清掉记忆");

  // 6. 锁定后拖不动
  await page.locator("#wb-layout-lock").click();
  assert.equal(await page.locator("#wb-layout-lock").getAttribute("title"), "解锁后可以拖动卡片");
  await drag("link", 0, 320);
  assert.equal(await gridOf("link"), initialLinkRow, "锁定时拖动不应改变布局");

  // 7. 创作画板（/draw）同样支持缩放面板、折叠收起与独立布局记忆
  await page.goto(new URL("draw", local).href, { waitUntil: "domcontentloaded" });
  await page.locator(".ob-draw-page .ob-canvas").waitFor({ timeout: 20_000 });
  for (const id of ["board", "draw-material", "draw-device", "draw-send"]) {
    assert.equal(await card(id).count(), 1, `创作画板 ${id} 应在画布里`);
  }
  // 画布占满整行，设置卡在画布下方三张并排（2026-11-07 用户决定）
  assert.equal(await colOf("board"), "1 / span 48", "创作画布默认占满整行");
  assert.deepEqual(
    await Promise.all(["draw-material", "draw-device", "draw-send"].map(colOf)),
    ["1 / span 16", "17 / span 16", "33 / span 16"],
    "三张设置卡在画布下方并排",
  );
  const wideBoard = await card("board").boundingBox();
  assert.ok((await card("draw-send").boundingBox()).y >= wideBoard.y + wideBoard.height, "预览与发送在画布下方");
  const boardFoldBtn = page.locator('[data-wb-card="board"] .ob-card-h button');
  await boardFoldBtn.click();
  assert.match(await gridOf("board"), /span 8$/, "创作画布折叠后高度应收到标题条");
  assert.equal(await card("board").getAttribute("data-wb-collapsed"), "true", "创作画布折叠后标记 data-wb-collapsed=true");
  await boardFoldBtn.click();
  assert.match(await gridOf("board"), /span 62$/, "创作画布展开应恢复原来的高度");

  const beforeBoardRows = await gridOf("board");
  const boardSouthHandle = page.locator('[data-wb-card="board"] [data-wb-resize="s"]');
  await boardSouthHandle.scrollIntoViewIfNeeded();
  const southBox = await boardSouthHandle.boundingBox();
  await page.mouse.move(southBox.x + southBox.width / 2, southBox.y + southBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(southBox.x + southBox.width / 2, southBox.y - 240, { steps: 8 });
  await page.mouse.up();
  assert.notEqual(await gridOf("board"), beforeBoardRows, "拖底边框应能缩小创作画布高度");
  const drawStored = await page.evaluate(() => globalThis.localStorage.getItem("moss_draw_layout_v2"));
  assert.ok(drawStored && drawStored.includes("version"), "创作画板布局应写进 moss_draw_layout_v2");

  // 8. 窄窗口（761–1200px）自动两栏并排，不再整页竖排；手机宽度才单列；拉宽后回到自由布局（2026-11-07 用户决定）
  const sideBySide = async (leftId, rightId, label) => {
    const left = await card(leftId).boundingBox();
    const right = await card(rightId).boundingBox();
    assert.ok(right.x >= left.x + left.width, `${label}：${rightId} 应在 ${leftId} 右侧`);
    assert.ok(Math.abs(right.y - left.y) <= 2, `${label}：两栏顶部对齐`);
  };
  const noHorizontalOverflow = async (label) => {
    const [sw, cw] = await page.evaluate(() => [document.scrollingElement.scrollWidth, document.documentElement.clientWidth]);
    assert.ok(sw <= cw, `${label}：不应出现横向滚动（${sw} > ${cw}）`);
  };
  // 第 6 段把工作台锁定了（锁定时本来就没有把手），这里清掉工作台记忆回到未锁定的默认布局
  await page.evaluate(() => globalThis.localStorage.removeItem("moss_wb_layout_v3"));
  for (const width of [1180, 960]) {
    await page.setViewportSize({ width, height: 760 });
    await page.goto(new URL("index.html", local).href, { waitUntil: "domcontentloaded" });
    await page.locator(".ob-canvas.ob-canvas--two").waitFor({ timeout: 20_000 });
    await sideBySide("link", "monitor", `工作台 ${width}px`);
    const source = await card("source").boundingBox();
    const link = await card("link").boundingBox();
    assert.ok(source.y > link.y && Math.abs(source.x - link.x) <= 2, `工作台 ${width}px：左栏上下堆叠`);
    // 两栏模式只保留上下边框把手：不能移动、不能改宽，但能调高度
    assert.equal(await page.locator(".wb-handle--e, .wb-handle--w, .wb-handle--se").count(), 0, "两栏模式没有左右与角把手");
    assert.ok(await page.locator(".wb-handle--s").count() >= 5, "两栏模式每张卡都有下边框把手");
    await noHorizontalOverflow(`工作台 ${width}px`);
    await page.goto(new URL("draw", local).href, { waitUntil: "domcontentloaded" });
    await page.locator(".ob-draw-page .ob-canvas.ob-canvas--two").waitFor({ timeout: 20_000 });
    const board = await card("board").boundingBox();
    const drawMaterial = await card("draw-material").boundingBox();
    assert.ok(drawMaterial.y >= board.y + board.height, `画板 ${width}px：设置卡在画布下方`);
    await sideBySide("draw-material", "draw-device", `画板 ${width}px`);
    // 用户反馈：窄窗口下画板也要能拖上下边框放大缩小
    for (const [dir, dy, grow] of [["s", 160, true], ["n", -120, true], ["s", -200, false]]) {
      const handle = card("board").locator(`[data-wb-resize="${dir}"]`);
      await handle.scrollIntoViewIfNeeded();
      const hb = await handle.boundingBox();
      const before = (await card("board").boundingBox()).height;
      await page.mouse.move(hb.x + hb.width / 2, hb.y + hb.height / 2);
      await page.mouse.down();
      await page.mouse.move(hb.x + hb.width / 2, hb.y + hb.height / 2 + dy, { steps: 8 });
      await page.mouse.up();
      await page.waitForTimeout(200);
      const after = (await card("board").boundingBox()).height;
      assert.ok(grow ? after > before + 60 : after < before - 60, `画板 ${width}px：拖${dir === "s" ? "下" : "上"}边框 ${dy}px 高度应${grow ? "变大" : "变小"}（${before} → ${after}）`);
    }
    assert.ok(await card("board").evaluate((el) => el.style.gridColumn === "1 / -1"), `画板 ${width}px：调完高度仍占满整行`);
    await noHorizontalOverflow(`画板 ${width}px`);
  }
  await page.setViewportSize({ width: 700, height: 900 });
  await page.goto(new URL("index.html", local).href, { waitUntil: "domcontentloaded" });
  await page.locator(".ob-canvas").waitFor({ timeout: 20_000 });
  assert.equal(await page.locator(".ob-canvas--two").count(), 0, "手机宽度不进两栏");
  const narrowMonitor = await card("monitor").boundingBox();
  const narrowLink = await card("link").boundingBox();
  assert.ok(narrowMonitor.y > narrowLink.y && Math.abs(narrowMonitor.x - narrowLink.x) <= 2, "手机宽度单列堆叠");
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.locator(".ob-canvas:not(.ob-canvas--two)").waitFor({ timeout: 5_000 });
  assert.equal(await colOf("monitor"), "17 / span 32", "拉宽后回到默认自由布局");

  assert.deepEqual(errors, [], "不应有前端未处理异常");
  console.log("WORKBENCH_LAYOUT_BROWSER_TEST=PASS");
  await context.close();
} finally {
  if (browser) await browser.close();
  await vite.close();
}
