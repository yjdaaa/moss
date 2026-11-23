// 2026-11-23（r58）易用性改版浏览器测试：
// 1) 1280×760 首屏可见「计算加工轨迹」与急停（顶栏 + 卡内），空态预览占位紧凑，滚动时操作卡 sticky 常驻，对刀起点不截断；
// 2) 点击顶栏硬件胶囊打开统一「设备连接」面板，切换连接方式与输入 IP 不自动发探测/扫描请求，Esc 关闭并还原焦点，跨页共享同一份连接摘要；
// 3) 创作画板 1280×760 首屏可见吸底操作条（材料·厚度、连接状态、生成预览、确认发送），点击「确认发送」仍弹出 SendConfirmDialog 确认框；
// 4) 材料实验室标定向导点「稍后」收成单行（带「继续」）并跨刷新保持；语音中心两条独立提示可单独关闭并记住。
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
  browser = await chromium.launch({
    headless: true,
    channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined),
  });
  const context = await browser.newContext({
    serviceWorkers: "block",
    viewport: { width: 1280, height: 760 },
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));

  const apiCalls = {
    connectionCheck: 0,
    discover: 0,
    confirmSend: 0,
    drawPreview: 0,
  };
  const mockVoice = {
    mode: "official",
    strict: false,
  };
  await page.exposeFunction("__usabilityInvoke", async (command, args = {}) => {
    if (command === "get_backend_origin") return "http://127.0.0.1:9666";
    if (command === "get_desktop_session") return "a".repeat(64);
    const buildVoiceStatus = () => ({
      mode: mockVoice.mode,
      settings: { builtin_ws_port: 8000, builtin_http_port: 8003, official_mcp_endpoint: "wss://example.test/mcp/?token=***", official_mcp_endpoint_set: true },
      bridge: { connected: true },
      strict: { enabled: mockVoice.strict, from_env: false },
      gateway: { enabled: false },
      mcp_endpoint: "wss://example.test/mcp/?token=***",
      mcp: { endpoints: [], tools: [], full_tool_list: false, max_extra_endpoints: 4 },
    });
    if (command === "voice_status") return buildVoiceStatus();
    if (command === "voice_strict_set") {
      mockVoice.strict = Boolean(args.enabled);
      return { enabled: mockVoice.strict, applied: true, needs_restart: false, env_file: ".env" };
    }
    if (command === "voice_mode_changes") {
      return { changes: [`切换到 ${args.target || args.mode || "目标链路"}`] };
    }
    if (command === "voice_set_mode") {
      mockVoice.mode = args.mode || args.target || "builtin";
      return buildVoiceStatus();
    }
    if (command === "voice_builtin_status") {
      return {
        running: false,
        mode: "echo",
        link_mode: mockVoice.mode,
        ws_port: 8000,
        http_port: 8003,
        models: {},
        jev: { key_set: false, voice_enabled: false, feedback_enabled: false },
        local: { settings: {}, models: [], helper_found: false },
        sessions: [],
        events: [],
        devices: [],
      };
    }
    return {};
  });

  await context.addInitScript(() => {
    window.__TAURI__ = {
      core: {
        invoke: (command, args) => window.__usabilityInvoke(command, args),
      },
      event: { listen: async () => () => {} },
    };
  });

  await context.route("**/*", async (route) => {
    const target = new URL(route.request().url());
    if (target.origin === new URL(local).origin && !target.pathname.startsWith("/api/") && !target.pathname.startsWith("/v1/")) {
      return route.continue();
    }
    if (target.pathname === "/api/connection/check") {
      apiCalls.connectionCheck += 1;
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ success: true, result: { network_connected: true, serial_connected: true, summary: "Idle ($32=1)" } }),
      });
    }
    if (target.pathname === "/api/ui/serial-ports") {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ success: true, result: { ports: ["/dev/cu.Bluetooth-Incoming-Port"] } }),
      });
    }
    if (target.pathname === "/api/ui/discover-controllers") {
      apiCalls.discover += 1;
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ success: true, result: { controllers: [], busy: [], checked: 0, not_controllers: 0, hint: "" } }),
      });
    }
    if (target.pathname === "/api/ui/material-options") {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          success: true,
          result: {
            materials: [
              { name: "椴木", thicknesses: [3, 5] },
              { name: "亚克力", thicknesses: [3] },
            ],
          },
        }),
      });
    }
    if (target.pathname === "/api/text-task/generate") {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          success: true,
          result: {
            workflow_id: "wf-wb-reg-1",
            status: "preview_ready",
            artifacts: {
              width_mm: 40,
              height_mm: 15,
              origin: "center",
              origin_note: "开刻前把激光头移到木牌中心",
              frame_mm: { min_x: -20, max_x: 20, min_y: -7.5, max_y: 7.5 },
              actual_params: { laser_max_power: 420, power_scale: 1000, feed_rate: 1800, passes: 1 },
              time_estimate: { estimated_seconds: 18, estimated_minutes: 0.3 },
            },
          },
        }),
      });
    }
    if (target.pathname.endsWith("/material-lab/bootstrap")) {
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          success: true,
          result: {
            materials: {
              power_scale: 1000,
              materials: {
                椴木: {
                  aliases: [],
                  thicknesses: {
                    "3": {
                      engrave: { raster: { laser_min_power: 0, laser_max_power: 500, feed_rate: 1800, travel_rate: 3000, threshold: 128, passes: 1, pixel_size_mm: 0.1 } },
                    },
                  },
                },
                亚克力: {
                  aliases: [],
                  thicknesses: {
                    "3": {
                      engrave: { raster: { laser_min_power: 0, laser_max_power: 600, feed_rate: 1200, travel_rate: 3000, threshold: 128, passes: 1, pixel_size_mm: 0.1 } },
                    },
                  },
                },
              },
            },
          },
        }),
      });
    }
    if (target.pathname === "/api/draw/preview") {
      apiCalls.drawPreview += 1;
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          success: true,
          result: {
            workflow_id: "wf-draw-dock-1",
            status: "preview_ready",
            speech: "预览已生成",
            next_actions: ["confirm_send"],
            artifacts: {
              mode: "vector",
              width_mm: 50,
              height_mm: 35,
            },
            web: {
              gcode_preview_url: "",
              image_preview_url: "",
            },
            draw_lab: {
              content_fingerprint: "b".repeat(64),
              job_summary: {
                material: "椴木",
                thickness_mm: 3,
                mode: "vector",
                actual_width_mm: 50,
                actual_height_mm: 35,
                line_count: 48,
                estimated_seconds: 12,
                can_send: true,
                parameter_source: "library",
              },
            },
          },
        }),
      });
    }
    if (target.pathname === "/api/workflow") {
      const body = JSON.parse(route.request().postData() || "{}");
      if (body.action === "confirm_send") {
        apiCalls.confirmSend += 1;
      }
      return route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ success: true, result: { workflows: [] } }),
      });
    }
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ success: true, result: { workflows: [], jobs: [], ports: [], mcp: {}, approvals: [] } }),
    });
  });

  // ========== 1. 雕刻工作台：1280×760 首屏主按钮与急停常驻可见 ==========
  await page.goto(new URL("index.html", local).href, { waitUntil: "domcontentloaded" });
  await page.locator(".ob-canvas").waitFor({ timeout: 20_000 });

  const genBtn = page.getByRole("button", { name: "计算加工轨迹", exact: true });
  const cardEstop = page.locator("#btn-estop-trigger");
  const navEstop = page.locator("#nav-estop");
  await genBtn.waitFor({ timeout: 10_000 });

  // B4 / B7 / B8（2026-11-23 r59）：侧栏与卡片头无重复英文徽标、卡内急停与终端改人话、无刀路时反馈默认收起成标题行、对刀起点带小图示
  assert.equal(await page.locator(".ob-nav-tag").count(), 0, "侧栏不再显示 HOST/CAD/LAB 等英文徽标");
  assert.equal(await page.locator(".ob-k").count(), 0, "卡片头不再重复显示英文小标");
  assert.equal((await cardEstop.innerText()).trim(), "紧急停止（软件复位）");
  assert.equal(await cardEstop.getAttribute("title"), "发送 0x18 软件复位；紧急情况请按机器实体急停");
  assert.match(await page.locator("#consoleFeed").innerText(), /开始雕刻或探测设备后，这里显示与雕刻机的实时通信记录/);
  assert.equal(await page.locator('[data-wb-card="feedback"]').getAttribute("data-wb-collapsed"), "true", "无刀路且无反馈历史时加工反馈默认收起成标题行");
  assert.equal(await page.locator('label:has(#wb-text-origin) svg.origin-badge[data-origin="center"]').count(), 1, "矢量文字起点选择旁显示木牌中心红点图示");

  const genBox = await genBtn.boundingBox();
  const cardEstopBox = await cardEstop.boundingBox();
  const navEstopBox = await navEstop.boundingBox();
  assert.ok(genBox && genBox.y >= 0 && genBox.y + genBox.height <= 760, `1280×760 首屏必须完整看到「计算加工轨迹」，实际 bottom=${genBox && genBox.y + genBox.height}`);
  assert.ok(cardEstopBox && cardEstopBox.y >= 0 && cardEstopBox.y + cardEstopBox.height <= 760, `1280×760 首屏必须完整看到卡内急停条，实际 bottom=${cardEstopBox && cardEstopBox.y + cardEstopBox.height}`);
  assert.ok(navEstopBox && navEstopBox.y >= 0 && navEstopBox.y + navEstopBox.height <= 760, "顶栏急停必须在首屏可见");

  const assertEstopHitSelf = async (label) => {
    const box = await navEstop.boundingBox();
    assert.ok(box && box.width > 40 && box.height > 20, `${label}：#nav-estop 必须可见`);
    const cx = box.x + box.width / 2;
    const cy = box.y + box.height / 2;
    const hit = await page.evaluate(([x, y]) => {
      const el = document.elementFromPoint(x, y);
      return {
        matched: Boolean(el && el.closest("#nav-estop")),
        desc: el ? `${el.tagName}#${el.id || ""}.${String(el.className || "")}` : "null",
      };
    }, [cx, cy]);
    assert.equal(hit.matched, true, `${label}：#nav-estop 中心点 elementFromPoint 必须命中急停本身，实际命中 ${hit.desc}`);
  };

  // 第 7 条：未探测时顶栏硬件胶囊使用中性灰 ob-pill--off，不误用故障红 ob-pill--bad
  const initialHwClass = await page.locator("#nav-hardware-status").getAttribute("class");
  assert.match(initialHwClass, /\bob-pill--off\b/, "尚未探测时硬件胶囊应为中性灰色 ob-pill--off");
  assert.doesNotMatch(initialHwClass, /\bob-pill--bad\b/, "尚未探测时不得误显红色故障态");
  await assertEstopHitSelf("1280×760 默认顶栏");

  // 第 3 条：空态预览区弹性填满监视舱消除中部空洞，左栏折叠时又能自动压回紧凑最小高度
  const emptyBox = await page.locator(".chamber-viewport.is-empty").boundingBox();
  const initialDispatchBox = await page.locator('[data-wb-card="monitor"] .job-dispatch-card').boundingBox();
  assert.ok(emptyBox && emptyBox.height >= 90, `空态预览占位至少保持 96px 最小高度，实际 height=${emptyBox?.height}`);
  assert.ok(
    emptyBox && initialDispatchBox && initialDispatchBox.y - (emptyBox.y + emptyBox.height) <= 24,
    `空态预览区与底部操作卡之间不得裂出中部空白洞，实际间距=${initialDispatchBox && emptyBox && (initialDispatchBox.y - (emptyBox.y + emptyBox.height))}`,
  );

  // B3（2026-11-23 r60）：「现场移头与对刀原点」默认收起；展开后记住 foldState，未连接时全部按钮禁用并说明原因，
  // 且 1280×760 首屏的计算/开始/急停操作区不被挤出首屏、也不被 sticky 遮挡，顶栏与卡内急停中心点均命中自身
  const headJog = page.locator("#wb-head-jog");
  assert.equal(await headJog.evaluate((el) => el.open), false, "现场移头与对刀原点面板默认必须收起");
  await page.locator("#wb-head-jog-toggle").click();
  assert.equal(await headJog.evaluate((el) => el.open), true, "点击标题展开现场移头面板");
  assert.equal(await page.evaluate(() => localStorage.getItem("moss_ui_fold_wb-head-jog")), "open", "展开状态记入 foldState");
  assert.equal(await page.locator("#wb-jog-step").isDisabled(), true, "未连接时步长下拉禁用");
  for (const sel of ["#wb-jog-x-minus", "#wb-jog-x-plus", "#wb-jog-y-plus", "#wb-jog-y-minus", "#wb-jog-set-origin", "#wb-jog-unlock"]) {
    assert.equal(await page.locator(sel).isDisabled(), true, `未连接时 ${sel} 必须禁用`);
  }
  assert.match(await page.locator("#wb-jog-reason").innerText(), /尚未配置设备地址|握手探测/);
  await assertEstopHitSelf("1280×760 展开现场移头面板时顶栏急停");
  const cardEstopWhenJogOpen = await cardEstop.boundingBox();
  const genWhenJogOpen = await genBtn.boundingBox();
  const jogBox = await headJog.boundingBox();
  const dispatchWhenJogOpen = await page.locator('[data-wb-card="monitor"] .job-dispatch-card').boundingBox();
  assert.ok(
    genWhenJogOpen && genWhenJogOpen.y >= 0 && genWhenJogOpen.y + genWhenJogOpen.height <= 760,
    `展开现场移头面板后 1280×760 首屏仍须完整显示「计算加工轨迹」，实际 bottom=${genWhenJogOpen && genWhenJogOpen.y + genWhenJogOpen.height}`,
  );
  assert.ok(
    cardEstopWhenJogOpen && cardEstopWhenJogOpen.y >= 0 && cardEstopWhenJogOpen.y + cardEstopWhenJogOpen.height <= 760,
    `展开现场移头面板后 1280×760 首屏仍须完整显示卡内急停，实际 bottom=${cardEstopWhenJogOpen && cardEstopWhenJogOpen.y + cardEstopWhenJogOpen.height}`,
  );
  assert.ok(
    jogBox && dispatchWhenJogOpen && jogBox.y + jogBox.height <= dispatchWhenJogOpen.y,
    `展开现场移头面板不得被吸底操作卡遮挡（jogBottom=${jogBox && jogBox.y + jogBox.height}, dispatchTop=${dispatchWhenJogOpen?.y}）`,
  );
  const cardEstopHit = await page.evaluate(([x, y]) => Boolean(document.elementFromPoint(x, y)?.closest("#btn-estop-trigger")), [
    cardEstopWhenJogOpen.x + cardEstopWhenJogOpen.width / 2,
    cardEstopWhenJogOpen.y + cardEstopWhenJogOpen.height / 2,
  ]);
  assert.equal(cardEstopHit, true, "展开现场移头面板后卡内急停中心点必须命中自身");
  await page.locator("#wb-head-jog-toggle").click();
  assert.equal(await headJog.evaluate((el) => el.open), false);
  assert.equal(await page.evaluate(() => localStorage.getItem("moss_ui_fold_wb-head-jog")), "closed", "收起状态记入 foldState");

  // 展开左侧填充高级设置拉长页面后，主操作与卡内急停在首屏及滚动中均因 sticky 保持在可视区内
  await page.locator("#wb-text-style").selectOption("fill");
  await page.locator("#wb-fill-options > summary").click();
  await page.waitForTimeout(100);
  const stickyGenBox = await genBtn.boundingBox();
  const stickyEstopBox = await cardEstop.boundingBox();
  assert.ok(stickyGenBox && stickyGenBox.y + stickyGenBox.height <= 760, `左侧展开拉高后首屏仍须看到生成按钮，实际 bottom=${stickyGenBox && stickyGenBox.y + stickyGenBox.height}`);
  assert.ok(stickyEstopBox && stickyEstopBox.y + stickyEstopBox.height <= 760, `左侧展开拉高后首屏仍须看到卡内急停，实际 bottom=${stickyEstopBox && stickyEstopBox.y + stickyEstopBox.height}`);
  await page.locator("#wb-fill-options > summary").click();
  await page.locator("#wb-text-style").selectOption("outline");

  // 收起左栏两张卡片时，空态预览区自动压缩，且加工监视舱按自身真实内容高度下限撑住，绝不向下溢出遮挡「加工反馈」卡
  await page.locator('[data-wb-card="source"] .ob-card-h button').click();
  await page.locator('[data-wb-card="material"] .ob-card-h button').click();
  await page.waitForTimeout(100);
  const compactEmptyBox = await page.locator(".chamber-viewport.is-empty").boundingBox();
  assert.ok(compactEmptyBox && compactEmptyBox.height >= 90 && compactEmptyBox.height <= 180, `左栏收起时空态视口应压缩到紧凑高度，实际=${compactEmptyBox?.height}`);
  const dispatchAfterFold = await page.locator('[data-wb-card="monitor"] .job-dispatch-card').boundingBox();
  const feedbackAfterFold = await page.locator('[data-wb-card="feedback"]').boundingBox();
  assert.ok(
    dispatchAfterFold && feedbackAfterFold && dispatchAfterFold.y + dispatchAfterFold.height <= feedbackAfterFold.y + 2,
    `左栏收起后监视舱内容不得盖住加工反馈卡（dispatchBottom=${dispatchAfterFold && dispatchAfterFold.y + dispatchAfterFold.height}, feedbackTop=${feedbackAfterFold?.y}）`,
  );
  await page.locator('[data-wb-card="material"] .ob-card-h button').click();
  await page.locator('[data-wb-card="source"] .ob-card-h button').click();
  await page.waitForTimeout(100);

  // 第 3 条：生成加工轨迹并展开「两点对位」面板时，暂停操作卡 sticky 叠压，对位控件不被盖住且真实可点
  await page.getByPlaceholder("输入要雕刻的字符...").fill("MOSS");
  await genBtn.click();
  await page.locator("#wb-registration").waitFor({ timeout: 10_000 });
  assert.equal(await page.locator('[data-wb-card="feedback"]').getAttribute("data-wb-collapsed"), "false", "生成加工轨迹后加工反馈自动展开");
  await page.locator(".feedback-chip", { hasText: "刻太浅" }).click();
  assert.equal(await page.getByLabel("加工效果反馈").inputValue(), "刻太浅", "点击症状快捷胶囊填入反馈输入框");
  await page.locator("#wb-registration > summary").click();
  await page.locator("#wb-reg-arm-jog").waitFor({ timeout: 5_000 });
  const regPreviewBtn = page.locator("#wb-reg-preview");
  await regPreviewBtn.scrollIntoViewIfNeeded();
  const regBtnBox = await regPreviewBtn.boundingBox();
  const dispatchWhenRegOpen = await page.locator('[data-wb-card="monitor"] .job-dispatch-card').boundingBox();
  assert.ok(
    regBtnBox && dispatchWhenRegOpen && regBtnBox.y + regBtnBox.height <= dispatchWhenRegOpen.y,
    `展开两点对位时操作卡不得吸底叠盖对位按钮（regBottom=${regBtnBox && regBtnBox.y + regBtnBox.height}, dispatchTop=${dispatchWhenRegOpen?.y}）`,
  );
  const regHit = await page.evaluate(([x, y]) => Boolean(document.elementFromPoint(x, y)?.closest("#wb-reg-preview")), [
    regBtnBox.x + regBtnBox.width / 2,
    regBtnBox.y + regBtnBox.height / 2,
  ]);
  assert.equal(regHit, true, "展开两点对位后生成对位预览按钮中心点必须命中自身而非被操作卡遮挡");
  await page.locator("#wb-registration > summary").click();
  await page.evaluate(() => window.scrollTo(0, 0));

  // 1280 宽下对刀起点下拉「木牌中心」不截断
  const originSelect = page.locator("#wb-text-origin");
  assert.equal(await originSelect.locator("option:checked").innerText(), "木牌中心");
  const originTruncated = await originSelect.evaluate((el) => el.scrollWidth > el.clientWidth + 2);
  assert.equal(originTruncated, false, "1280px 下对刀起点下拉不得横向截断文字");

  // 右上角「布局」菜单按钮省去整行工具条，无违规 role="menu"，点击展开、Esc 关闭并将焦点还给触发按钮
  assert.equal(await page.locator(".home-page .ob-canvas-tools").count(), 0, "工作台不再占用整行布局工具条");
  await page.locator("#wb-layout-menu-btn").click();
  await page.locator("#wb-layout-menu").waitFor({ timeout: 5_000 });
  assert.notEqual(await page.locator("#wb-layout-menu").getAttribute("role"), "menu", "布局浮层不得标无方向键实现的 role=menu");
  assert.match(await page.locator("#wb-layout-menu").innerText(), /拖标题移动/);
  await page.locator("#wb-layout-lock").focus();
  await page.keyboard.press("Escape");
  await page.locator("#wb-layout-menu").waitFor({ state: "detached", timeout: 5_000 });
  assert.equal(await page.evaluate(() => document.activeElement?.id || ""), "wb-layout-menu-btn", "布局浮层内按 Escape 关闭后焦点须回到 #wb-layout-menu-btn");

  // 第 1 条 & 第 10 条：820×1000 精简顶栏与 375×844 打开布局菜单时，#nav-estop 中心点 elementFromPoint 均命中急停本身
  await page.locator("#top-compact-toggle").click();
  await page.waitForTimeout(200);
  await assertEstopHitSelf("1280×760 精简顶栏");
  const compactTopBox = await page.locator("header.ob-top").boundingBox();
  const monHeadBox = await page.locator('[data-wb-card="monitor"] .ob-card-h').boundingBox();
  assert.ok(monHeadBox && compactTopBox && monHeadBox.y >= compactTopBox.y + compactTopBox.height - 4, "精简顶栏悬浮胶囊不得遮挡监视舱卡片标题栏");

  // 缩窄至 820×1000（保持 compact 状态）：顶栏自动退回常规顶栏，不被侧栏遮挡，#nav-estop 中心点命中自身
  await page.setViewportSize({ width: 820, height: 1000 });
  await page.waitForTimeout(200);
  assert.equal(await page.locator("header.ob-top").getAttribute("data-compact"), "true");
  assert.equal(await page.locator("#top-compact-toggle").isVisible(), false, "820px 窄屏隐藏精简顶栏切换按钮");
  await assertEstopHitSelf("820×1000 精简顶栏");

  // 缩窄至 375×844 打开布局菜单：单列提示不谎称可拖拽、拖卡片头不空飘、且打开菜单时 #nav-estop 中心点命中急停本身
  await page.setViewportSize({ width: 375, height: 844 });
  await page.waitForTimeout(200);
  await page.locator("#wb-layout-menu-btn").click();
  await page.locator("#wb-layout-menu").waitFor({ timeout: 5_000 });
  assert.match(await page.locator("#wb-layout-menu").innerText(), /单列/, "375px 单列下菜单须如实说明单列状态");
  assert.doesNotMatch(await page.locator("#wb-layout-menu").innerText(), /拖标题移动/, "375px 单列下不得提示无效拖拽");
  await assertEstopHitSelf("375×844 打开布局菜单");
  await page.keyboard.press("Escape");
  await page.locator("#wb-layout-menu").waitFor({ state: "detached", timeout: 5_000 });

  // 375×844 单列下拖动卡片头不得产生 transform 空飘
  const linkHeadBox = await page.locator('[data-wb-card="link"]').boundingBox();
  await page.mouse.move(linkHeadBox.x + 40, linkHeadBox.y + 18);
  await page.mouse.down();
  await page.mouse.move(linkHeadBox.x + 40, linkHeadBox.y + 120, { steps: 5 });
  const dragTransform = await page.locator('[data-wb-card="link"]').evaluate((el) => el.style.transform);
  assert.equal(dragTransform, "", "单列模式下拖动卡片头不得空飘");
  await page.mouse.up();

  // 恢复 1280×760 常规展开顶栏
  await page.setViewportSize({ width: 1280, height: 760 });
  await page.waitForTimeout(200);
  await page.locator("#top-compact-toggle").click();
  await page.waitForTimeout(200);

  // ========== 2. 统一连接面板：零自动探测 + 创作画板首屏不挡 Excalidraw 缩放/撤销 + 未配连接引导 ==========
  assert.equal(apiCalls.connectionCheck, 0, "未保存连接目标时进入工作台首屏不得自动发起握手探测");
  apiCalls.discover = 0;

  // 先在尚未配置设备 IP 的状态下切到创作画板，验证第 2 条（不挡画布底部控件）与第 6 条（未配连接提示 + 摘要胶囊）
  await page.locator("nav.ob-nav button", { hasText: "创作画板" }).click();
  await page.locator("#draw-board-action-bar").waitFor({ timeout: 20_000 });
  await page.waitForTimeout(250);
  assert.equal(await page.locator(".topbar-actions button", { hasText: "工作台" }).count(), 0, "画板工具栏去掉与侧栏重复的工作台按钮");
  assert.equal(await page.locator('#draw-dock-origin svg.origin-badge[data-origin="lower_left"]').count(), 1, "画板吸底条显示左下角起点图示");

  const stageBox = await page.locator(".ob-draw-page .ob-board-stage").boundingBox();
  const dockBarBox = await page.locator("#draw-board-action-bar").boundingBox();
  assert.ok(dockBarBox && dockBarBox.y >= 0 && dockBarBox.y + dockBarBox.height <= 760, `创作画板 1280×760 首屏必须看到底部操作条，实际 bottom=${dockBarBox && dockBarBox.y + dockBarBox.height}`);
  assert.ok(stageBox && dockBarBox && stageBox.y + stageBox.height <= dockBarBox.y + 1, `创作画板画布与底部操作条须上下分栏不叠压（stageBottom=${stageBox && stageBox.y + stageBox.height}, dockTop=${dockBarBox?.y}）`);

  // 验证 Excalidraw 自带的左下缩放/撤销按钮在 1280×760 首屏完全不被 #draw-board-action-bar 遮挡
  const excalidrawFooterBtn = page.locator(".ob-board-stage .excalidraw .App-bottom-bar button, .ob-board-stage .excalidraw .zoom-actions button").first();
  if (await excalidrawFooterBtn.count()) {
    const zBox = await excalidrawFooterBtn.boundingBox();
    assert.ok(zBox && zBox.y + zBox.height <= dockBarBox.y, "Excalidraw 底部缩放/撤销栏必须位于快捷操作条上方");
    const zHit = await page.evaluate(([x, y]) => {
      const el = document.elementFromPoint(x, y);
      return Boolean(el && !el.closest("#draw-board-action-bar") && el.closest(".excalidraw"));
    }, [zBox.x + zBox.width / 2, zBox.y + zBox.height / 2]);
    assert.equal(zHit, true, "1280×760 首屏 Excalidraw 底部工具按钮中心点不得被操作条拦截");
  }

  // 导入图形 -> 未配连接时点吸底条「生成预览」：出现尺寸耗时摘要胶囊，且右侧按钮显示「请先配置连接」可点打开连接面板
  const scenePayload = JSON.stringify({
    type: "excalidraw",
    version: 2,
    elements: [
      {
        id: "dock-rect-1",
        type: "rectangle",
        x: 20,
        y: 20,
        width: 100,
        height: 60,
        angle: 0,
        strokeColor: "#1e1e1e",
        backgroundColor: "#1e1e1e",
        fillStyle: "solid",
        strokeWidth: 2,
        strokeStyle: "solid",
        roughness: 0,
        opacity: 100,
        groupIds: [],
        frameId: null,
        roundness: null,
        seed: 1,
        version: 1,
        versionNonce: 1,
        isDeleted: false,
        boundElements: null,
        updated: 1,
        link: null,
        locked: false,
      },
    ],
    appState: { viewBackgroundColor: "#ffffff" },
  });
  await page.locator(".topbar-actions input[type='file']").setInputFiles({
    name: "dock-test.excalidraw.json",
    mimeType: "application/json",
    buffer: Buffer.from(scenePayload, "utf8"),
  });
  await page.getByText(/已导入场景：dock-test\.excalidraw\.json/).waitFor({ timeout: 10_000 });

  await page.locator("#draw-dock-preview-btn").click();
  await page.locator("#draw-dock-preview-summary").waitFor({ timeout: 10_000 });
  assert.equal(apiCalls.drawPreview, 1, "吸底条生成预览应复用同一预览流程");
  assert.match(await page.locator("#draw-dock-preview-summary").innerText(), /50×35mm · 约 12s/, "首屏吸底条应展示预览尺寸与预估耗时摘要");
  assert.match(await page.locator("#draw-dock-send-btn").innerText(), /请先配置连接/, "未配连接时吸底条发送按钮应明确提示请先配置连接");
  assert.equal(await page.locator("#draw-dock-send-btn").isDisabled(), false, "未配连接时点击吸底条按钮应可唤起连接弹窗");

  // 点击吸底条「请先配置连接」直接打开统一连接面板；切换方式与输入 IP 零自动探测；Esc 关闭焦点回到触发按钮
  await page.locator("#draw-dock-send-btn").focus();
  await page.locator("#draw-dock-send-btn").click();
  const connModal = page.locator("#device-connection-modal");
  await connModal.waitFor({ timeout: 5_000 });
  await connModal.locator("#conn-mode-select").selectOption("serial");
  await connModal.locator("#conn-mode-select").selectOption("network");
  await connModal.locator("#conn-network-host").fill("192.0.2.88");
  await connModal.locator("#conn-telnet-port").fill("2323");
  await page.waitForTimeout(200);
  assert.equal(apiCalls.connectionCheck, 0, "打开面板、切换连接方式或填写 IP 不得自动发起握手探测");
  assert.equal(apiCalls.discover, 0, "打开面板或编辑输入不得自动发起局域网扫描");

  await page.keyboard.press("Escape");
  await connModal.waitFor({ state: "detached", timeout: 5_000 });
  assert.equal(await page.evaluate(() => document.activeElement?.id || ""), "draw-dock-send-btn", "按 Escape 关闭连接面板后焦点应回到触发按钮");
  assert.match(await page.locator("#draw-dock-connection").innerText(), /192\.0\.2\.88:2323/, "画板吸底条应看到同一份连接设置");
  assert.match(await page.locator("#draw-device-summary").innerText(), /192\.0\.2\.88:2323/, "画板下方设备卡应看到同一份连接摘要");
  assert.match(await page.locator("#draw-dock-send-btn").innerText(), /确认发送/, "配好连接后吸底条按钮立即变为确认发送");

  // 点击材料胶囊滚动定位到材料与尺寸卡，再滚回顶部测确认发送弹窗
  await page.locator("#draw-dock-material").click();
  await page.waitForTimeout(350);
  const scrolledY = await page.evaluate(() => window.scrollY);
  assert.ok(scrolledY > 80, "点击吸底条材料胶囊应向下滚动定位到材料卡");
  await page.evaluate(() => window.scrollTo(0, 0));

  await page.locator("#draw-dock-send-btn").click();
  const sendDialog = page.getByRole("dialog", { name: "确认开始雕刻" });
  await sendDialog.waitFor({ timeout: 5_000 });
  assert.equal(await page.locator("body > .modal-backdrop > .send-confirm-dialog").count(), 1, "画板发送确认框须经 Portal 挂到 body 顶层");
  assert.equal(await sendDialog.locator('#draw-confirm-origin svg.origin-badge[data-origin="lower_left"]').count(), 1, "画板发送确认框显示左下角起点图示");
  assert.equal(apiCalls.confirmSend, 0, "点击吸底条确认发送只能弹出确认框，绝不得绕过确认直接发送");
  await page.keyboard.press("Escape");
  await sendDialog.waitFor({ state: "detached", timeout: 5_000 });
  await page.locator("#draw-dock-send-btn").click();
  await sendDialog.waitFor({ timeout: 5_000 });
  await sendDialog.getByRole("button", { name: "取消" }).click();
  await sendDialog.waitFor({ state: "detached", timeout: 5_000 });

  // 从外页切回雕刻工作台后等待 700ms，验证仍不会偷偷向刚填的 IP 发起自动探测
  await page.locator("nav.ob-nav button", { hasText: "雕刻工作台" }).click();
  await page.waitForTimeout(700);
  assert.equal(apiCalls.connectionCheck, 0, "外页改完连接切回工作台也不得自动发起握手探测");
  assert.match(await page.locator("#wb-link-summary").innerText(), /192\.0\.2\.88:2323/, "工作台紧凑连接摘要应立即同步");

  // 第 7 条：点击「握手探测」成功后，弹窗提示包含“语音和 AI 发送时没指定串口或 IP 就用这个连接”，且日志写入工作台底部终端
  const hwPill = page.locator("#nav-hardware-status");
  await hwPill.focus();
  await hwPill.click();
  await connModal.waitFor({ timeout: 5_000 });
  await connModal.locator("#conn-probe-btn").click();
  await connModal.locator("#conn-probe-status").filter({ hasText: "语音和 AI 发送时没指定串口或 IP 就用这个连接" }).waitFor({ timeout: 5_000 });
  assert.equal(apiCalls.connectionCheck, 1);
  await page.keyboard.press("Escape");
  await connModal.waitFor({ state: "detached", timeout: 5_000 });
  assert.equal(await page.evaluate(() => document.activeElement?.id || ""), "nav-hardware-status");
  assert.match(await page.locator(".bottom-drawer").innerText(), /语音和 AI 发送时没指定串口或 IP 就用这个连接/, "连接弹窗握手日志须同步写入工作台终端");

  // ========== 4. 材料实验室：不覆盖已存非空串口 + 统一连接摘要弹窗 + 标定向导 6/6 回看收起不封死新材料 ==========
  await page.evaluate(() => {
    localStorage.setItem("laser-forge-conn-mode", "serial");
    localStorage.setItem("laser-forge-serial-port", "/dev/ttyUSB-SAVED-LASER");
    localStorage.setItem("laser-forge-baud", "38400");
    window.dispatchEvent(new Event("storage"));
  });
  await page.locator("nav.ob-nav button", { hasText: "材料实验室" }).click();
  const wizard = page.locator("#lab-wizard");
  await wizard.waitFor({ timeout: 15_000 });

  // 验证进入材料实验室不会把已存串口篡改为系统枚举第 1 项 /dev/cu.Bluetooth-Incoming-Port，且连接条与弹窗显示 38400
  await page.waitForTimeout(250);
  assert.equal(await page.evaluate(() => localStorage.getItem("laser-forge-serial-port")), "/dev/ttyUSB-SAVED-LASER", "进入材料实验室不得篡改已存非空串口");
  assert.match(await page.locator("#lab-max-power-s").innerText(), /对应 S500 \/ 1000/, "实验室功率以百分比显示并标注对应 S 值");
  assert.equal(await page.locator(".lab-test-axes label select").count(), 0, "校准矩阵测试类型与轴参数下拉已改为标签的兄弟节点防截断");
  assert.match(await page.locator("#lab-conn-summary").innerText(), /\/dev\/ttyUSB-SAVED-LASER · 38400/, "材料实验室连接摘要条应准确显示串口与 38400 波特率");
  await page.locator("#lab-open-connection").click();
  await connModal.waitFor({ timeout: 5_000 });
  assert.equal(await connModal.locator("#conn-baudrate").inputValue(), "38400", "统一连接面板须支持并准确选中 38400 波特率");
  await connModal.locator("#conn-refresh-ports").click();
  await connModal.locator("#conn-probe-status").waitFor({ timeout: 5_000 });
  assert.equal(await connModal.locator("#conn-serial-port").inputValue(), "/dev/ttyUSB-SAVED-LASER", "刷新串口列表不得覆盖已存的非空串口号");
  await page.keyboard.press("Escape");
  await connModal.waitFor({ state: "detached", timeout: 5_000 });

  // 验证向导无 <summary><button> 违规嵌套，点「稍后」收成单行（带「继续」）并跨刷新保持
  assert.equal(await wizard.locator("summary button").count(), 0, "禁止 <summary> 内部嵌套 <button>");
  assert.equal(await wizard.getAttribute("data-open"), "true", "初始未完成向导默认展开");
  await page.locator("#lab-wizard-defer").click();
  assert.equal(await wizard.getAttribute("data-open"), "false", "点击稍后将向导整体收成一行");
  assert.match(await wizard.locator(".lab-wizard-head").innerText(), /继续/, "收起摘要行应提示继续");

  await page.reload({ waitUntil: "domcontentloaded" });
  await wizard.waitFor({ timeout: 15_000 });
  assert.equal(await wizard.getAttribute("data-open"), "false", "刷新页面后向导仍保持收起状态");

  // 第 9 条：6/6 已完成材料展开回看显示「收起」，点击「收起」不写全局 closed，切换到未标定的新材料「亚克力」仍自动展开
  await page.evaluate(() => {
    localStorage.removeItem("moss_ui_fold_lab-wizard");
    localStorage.setItem(
      "moss.calibrationWizard.v1",
      JSON.stringify({
        machine: {
          scan_offset: { status: "done", at: 1, note: "" },
          time: { status: "done", at: 1, note: "" },
        },
        materials: {
          "椴木|3": {
            interval: { status: "done", at: 1, note: "" },
            matrix: { status: "done", at: 1, note: "" },
            overscan: { status: "done", at: 1, note: "" },
            kerf: { status: "done", at: 1, note: "" },
          },
        },
        open: true,
      }),
    );
  });
  await page.reload({ waitUntil: "domcontentloaded" });
  await wizard.filter({ hasText: "已完成 6 / 6" }).waitFor({ timeout: 15_000 });
  assert.equal(await wizard.getAttribute("data-open"), "false", "6/6 已完成时初始默认收起");
  await page.locator("#lab-wizard-toggle").click();
  assert.equal(await wizard.getAttribute("data-open"), "true", "点击标题可展开回看已完成步骤");
  assert.equal((await page.locator("#lab-wizard-defer").innerText()).trim(), "收起", "6/6 已完成展开时按钮文案应为收起而非稍后");
  await page.locator("#lab-wizard-defer").click();
  assert.equal(await wizard.getAttribute("data-open"), "false");
  assert.equal(await page.evaluate(() => localStorage.getItem("moss_ui_fold_lab-wizard")), null, "6/6 已完成回看后再收起不得写入全局 closed");
  await page.getByRole("cell", { name: /亚克力/ }).click();
  const afterSwitchProgress = await wizard.locator(".lab-wizard-progress").innerText();
  assert.equal(await wizard.getAttribute("data-open"), "true", `切换到未标定的新材料亚克力时向导仍应自动展开（progress=${afterSwitchProgress}）`);

  // ========== 5. 语音中心：独立提示关闭记忆 + 严格模式重开再关恢复警告 + 内置链路同显警告 ==========
  await page.locator("nav.ob-nav button", { hasText: "语音中心" }).click();
  await page.locator("#voice-ota-hint").waitFor({ timeout: 15_000 });
  await page.locator("#voice-strict-hint").waitFor({ timeout: 15_000 });
  assert.ok(await page.locator("#voice-strict-hint-action").isVisible(), "严格模式提示保留开启开关操作");

  await page.locator("#voice-ota-hint-close").click();
  assert.equal(await page.locator("#voice-ota-hint").count(), 0, "设备连接提示可单独关闭");
  assert.equal(await page.locator("#voice-strict-hint").count(), 1, "另一条严格模式提示不受影响");

  // 关闭严格模式警告后，开启严格模式再切回关闭时，警告条必须重新出现；切到内置链路也显示该警告
  await page.locator("#voice-strict-hint-close").click();
  assert.equal(await page.locator("#voice-strict-hint").count(), 0, "严格模式警告点 × 后暂时关闭");
  await page.locator("#voice-strict-state").click();
  await page.getByTestId("app-dialog").getByRole("button", { name: "开启", exact: true }).click();
  await page.getByTestId("app-dialog").getByRole("button", { name: "确定", exact: true }).click();
  await page.locator("#voice-strict-state").filter({ hasText: "已开启" }).waitFor({ timeout: 5_000 });

  await page.locator("#voice-strict-state").click();
  await page.getByTestId("app-dialog").getByRole("button", { name: "关闭", exact: true }).click();
  await page.getByTestId("app-dialog").getByRole("button", { name: "确定", exact: true }).click();
  await page.locator("#voice-strict-hint").waitFor({ timeout: 5_000 });
  assert.equal(await page.locator("#voice-ota-hint").count(), 0, "已关闭的官方 OTA 提示仍保持关闭");

  await page.locator("#voice-mode-builtin").click();
  await page.getByTestId("app-dialog").getByRole("button", { name: "确认切换", exact: true }).click();
  await page.locator("#voice-ota-hint").waitFor({ timeout: 5_000 });
  assert.equal(await page.locator("#voice-strict-hint").count(), 1, "内置语音服务链路未开严格模式时同样必须显示警告");
  await page.locator("#voice-builtin-checklist").waitFor({ timeout: 5_000 });
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-expanded"), "false", "非对话时语音孪生压成单行状态条");
  await page.locator("#voice-setup-step-models").click();
  assert.equal(await page.locator("#voice-page-tab-models").getAttribute("aria-selected"), "true", "点击清单第 2 步跳转到模型标签");

  // ========== 6. 第 12 条：恢复启动时的单次静默握手（已有保存目标时启动 600ms 后静默握手一次，切页不重复） ==========
  await page.evaluate(() => {
    localStorage.setItem("laser-forge-conn-mode", "network");
    localStorage.setItem("laser-forge-host", "192.0.2.88");
    localStorage.setItem("laser-forge-telnet-port", "2323");
  });
  apiCalls.connectionCheck = 0;
  await page.goto(new URL("index.html", local).href, { waitUntil: "domcontentloaded" });
  await page.locator("#nav-hardware-status").filter({ hasText: "硬件 · 握手通过" }).waitFor({ timeout: 5_000 });
  assert.equal(apiCalls.connectionCheck, 1, "已有已保存目标时，应用启动 600ms 后自动静默握手且只握手一次");
  await page.locator("nav.ob-nav button", { hasText: "创作画板" }).click();
  await page.locator("#draw-board-action-bar").waitFor({ timeout: 10_000 });
  await page.locator("nav.ob-nav button", { hasText: "雕刻工作台" }).click();
  await page.waitForTimeout(750);
  assert.equal(apiCalls.connectionCheck, 1, "从其他页面切回工作台不得重复自动握手");

  assert.deepEqual(errors, [], `页面不应有未捕获脚本错误: ${errors.join("; ")}`);
  console.log("USABILITY_LAYOUT_BROWSER_TEST=PASS");
  await context.close();
} finally {
  await browser?.close();
  await vite.close();
}
