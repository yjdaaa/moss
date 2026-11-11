// 界面端到端：真实浏览器 + moss-host 内嵌的正式前端包 + 真实后端 + 回环 GRBL 模拟器。
// 由 scripts/test_e2e.mjs 调度（npm run test:e2e），只点界面控件，设备侧断言看模拟器实际收到的字节。
// moss-host 在编译时内嵌 frontend/dist，改前端后须先 npm run build 再重新构建 moss-host。
import test, { after, before, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { chromium } from "playwright";
import { startRecordingGrbl } from "./recordingGrbl.mjs";

if (!process.env.TEST_API_BASE) {
  throw new Error("界面 E2E 必须由隔离调度器注入 TEST_API_BASE");
}

const API_BASE = process.env.TEST_API_BASE;
const GRBL_HOST = "127.0.0.1";
const STEP_TIMEOUT_MS = 20000;

let browser;

before(async () => {
  browser = await chromium.launch({
    headless: true,
    channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined),
  });
});

after(async () => {
  await browser?.close();
});

// 前一用例取消后任务按设计留在“待确认停止”，顶栏急停会优先发给它。
// 每个用例开始前走真实的“已确认停稳”接口清掉，相当于操作者已到现场核实。
beforeEach(async () => {
  const targets = await (await fetch(`${API_BASE}/api/jobs/stop-targets`)).json();
  for (const job of targets.result?.jobs || []) {
    const res = await fetch(`${API_BASE}/api/jobs/acknowledge-stop`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ job_id: job.job_id, confirmed: true }),
    });
    assert.equal(res.status, 200, `清理待确认停止任务失败: ${job.job_id}`);
  }
});

async function waitFor(predicate, message, timeoutMs = STEP_TIMEOUT_MS) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(message);
}

/** 失败时打印界面弹窗与最近的接口响应，便于定位是界面、门禁还是设备侧的问题。 */
function traceApi(page) {
  const traffic = [];
  page.on("response", async (response) => {
    if (!response.url().includes("/api/") || response.request().method() !== "POST") return;
    const body = await response.text().catch(() => "");
    traffic.push(`${response.request().method()} ${response.url().replace(API_BASE, "")} ${response.status()} ${body.slice(0, 300)}`);
  });
  return async (error) => {
    const dialog = await page.getByTestId("app-dialog").allInnerTexts().catch(() => []);
    console.error(`[UI-E2E] 弹窗: ${JSON.stringify(dialog)}`);
    for (const line of traffic.slice(-12)) console.error(`[UI-E2E] ${line}`);
    throw error;
  };
}

async function openWorkbench(port) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  const diagnose = traceApi(page);
  await page.goto(`${API_BASE}/`);
  await page.getByPlaceholder("控制器 IP (例 192.168.1.10)").fill(GRBL_HOST);
  await page.locator("input.wb-field-3").fill(String(port));
  return { context, page, diagnose };
}

/** 工作台点击流程：填文字 → 计算加工轨迹 → 开始雕刻 → 二次确认弹窗确认开刻。 */
async function generateAndSend(page, { text, fontSizeMm }) {
  await page.getByPlaceholder("输入要雕刻的字符...").fill(text);
  await page.locator("input.wb-field-5").fill(String(fontSizeMm));
  await page.getByRole("button", { name: "计算加工轨迹", exact: true }).click();
  const send = page.getByRole("button", { name: "开始雕刻", exact: true });
  await send.waitFor({ timeout: STEP_TIMEOUT_MS });
  await send.click();
  await page.getByTestId("app-dialog").getByRole("button", { name: /确认开刻$/ }).click();
}

test("[UI-01] 工作台: 生成 → 发送 → 发送中在任务浮层点「取消」，设备依次收到 ! ? 0x18 且不再收到刀路", async () => {
  const rec = await startRecordingGrbl({ ackDelayMs: 100 });
  const { context, page, diagnose } = await openWorkbench(rec.port);
  try {
    await generateAndSend(page, { text: "MOSS-UI-E2E", fontSizeMm: 11 }); // 约 80 mm 宽，须在 100 mm 幅面内
    await waitFor(() => rec.events.includes("line"), "设备必须先收到刀路行，取消才算发生在发送中");

    await page.locator("#job-progress-cancel-0").click({ timeout: STEP_TIMEOUT_MS });
    await page.getByTestId("app-dialog").getByRole("button", { name: "取消加工" }).click();
    await waitFor(() => rec.events.includes("0x18"), "取消后设备必须收到 0x18 软复位");

    const hold = rec.events.indexOf("!");
    assert.ok(hold >= 0, `必须收到 ! 进给保持: ${rec.events.slice(-20).join(" ")}`);
    const query = rec.events.indexOf("?", hold);
    assert.ok(query > hold, "! 之后必须轮询 ? 确认保持状态");
    const reset = rec.events.indexOf("0x18", query);
    assert.ok(reset > query, "确认保持后必须发送 0x18 软复位");
    assert.equal(rec.events.indexOf("line", reset), -1, "0x18 软复位后不得再下发刀路行");
  } catch (error) {
    await diagnose(error);
  } finally {
    await context.close();
    rec.close();
  }
});

test("[UI-02] 工作台: 任务全量发送完后按顶栏急停，仍向设备写出 0x18", async () => {
  const rec = await startRecordingGrbl({ ackDelayMs: 0 });
  const { context, page, diagnose } = await openWorkbench(rec.port);
  try {
    await generateAndSend(page, { text: "E2E", fontSizeMm: 5 });
    await page.getByText("控制器完成状态已核验").first().waitFor({ timeout: STEP_TIMEOUT_MS });
    assert.ok(rec.events.includes("line"), "任务必须真实下发过刀路");
    const before = rec.events.length;

    await page.locator("#nav-estop").click();
    await waitFor(() => rec.events.slice(before).includes("0x18"), "任务结束后顶栏急停必须真实写出 0x18");
    await page.getByTestId("app-dialog").getByText("停止请求已发送", { exact: true }).waitFor({ timeout: STEP_TIMEOUT_MS });
  } catch (error) {
    await diagnose(error);
  } finally {
    await context.close();
    rec.close();
  }
});

test("[UI-03] 画板: 没有活动任务时按顶栏急停，向本页设备目标写出 0x18（不再退化为取消）", async () => {
  const rec = await startRecordingGrbl();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.addInitScript(([host, port]) => {
    localStorage.setItem("laserLab.connection_mode", "network");
    localStorage.setItem("laserLab.network_host", host);
    localStorage.setItem("laserLab.network_telnet_port", port);
  }, [GRBL_HOST, String(rec.port)]);
  const page = await context.newPage();
  const cancels = [];
  page.on("request", (request) => {
    if (request.url().endsWith("/api/workflow") && (request.postData() || "").includes('"cancel"')) cancels.push(request.url());
  });
  const diagnose = traceApi(page);
  try {
    await page.goto(`${API_BASE}/draw`);
    await page.locator("#nav-estop").click();
    await waitFor(() => rec.events.includes("0x18"), "画板顶栏急停必须真实写出 0x18", 5000);
    await page.getByTestId("app-dialog").getByText("已发送急停", { exact: true }).waitFor({ timeout: STEP_TIMEOUT_MS });
    assert.deepEqual(cancels, [], "急停不得走 workflow cancel");
  } catch (error) {
    await diagnose(error);
  } finally {
    await context.close();
    rec.close();
  }
});

test("[UI-04] 工作台: 实心扫描填充真实生效 + 先走边框二次确认 + 改参数立即标过期禁用开刻", async () => {
  const rec = await startRecordingGrbl({ ackDelayMs: 0 });
  const { context, page, diagnose } = await openWorkbench(rec.port);
  try {
    await page.getByPlaceholder("输入要雕刻的字符...").fill("OK");
    await page.locator("input.wb-field-5").fill("8");
    await page.locator("#wb-text-style").selectOption("fill");
    await page.getByRole("button", { name: "计算加工轨迹", exact: true }).click();

    await page.locator("#wb-actual-params").filter({ hasText: "实心填充" }).waitFor({ timeout: STEP_TIMEOUT_MS });
    const framingBtn = page.locator("#wb-btn-framing");
    await framingBtn.waitFor({ timeout: STEP_TIMEOUT_MS });

    // 先点取消，确认机器不收任何刀路行
    const beforeCancel = rec.events.length;
    await framingBtn.click();
    await page.getByTestId("app-dialog").locator("button.btn", { hasText: "取消" }).click();
    assert.equal(rec.events.slice(beforeCancel).includes("line"), false, "取消走边框不得驱动机器移动");

    // 再次点走边框并确认，机器真实收到移动指令
    await framingBtn.click();
    await page.getByTestId("app-dialog").getByRole("button", { name: "开始走边框", exact: true }).click();
    await waitFor(() => rec.events.slice(beforeCancel).includes("line"), "确认走边框后设备必须收到移动指令");
    await page.getByText(/走边框完成/).first().waitFor({ timeout: STEP_TIMEOUT_MS });

    // 修改左侧文字，监视舱立即标记预览已过期并禁用开刻与走边框，拦截误刻旧刀路
    await page.getByPlaceholder("输入要雕刻的字符...").fill("NEW");
    await page.locator("#wb-stale-notice").waitFor({ timeout: 5000 });
    assert.equal(await page.locator("#wb-btn-send").isDisabled(), true, "参数改动后必须禁用旧刀路下发");
    assert.equal(await framingBtn.isDisabled(), true, "参数改动后必须禁用旧边框对位");
  } catch (error) {
    await diagnose(error);
  } finally {
    await context.close();
    rec.close();
  }
});

test("[UI-05] 画板: 自动继承工作台设备 + 导入场景与自定义参数生成预览 + relative_to_head 走边框 + 默认在线发送 + 改参数标过期拦截", async () => {
  const rec = await startRecordingGrbl({ ackDelayMs: 0 });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  // 只写工作台连接键，验证创作画板首次进入时自动互通带入 IP 与端口
  await context.addInitScript(([host, port]) => {
    localStorage.setItem("laser-forge-conn-mode", "network");
    localStorage.setItem("laser-forge-host", host);
    localStorage.setItem("laser-forge-telnet-port", port);
  }, [GRBL_HOST, String(rec.port)]);
  const page = await context.newPage();
  let framingPayload = null;
  page.on("request", (request) => {
    if (request.url().endsWith("/api/device/framing") && request.method() === "POST") {
      framingPayload = JSON.parse(request.postData() || "{}");
    }
  });
  const diagnose = traceApi(page);
  try {
    await page.goto(`${API_BASE}/draw`);
    const hostInput = page.locator('[data-wb-card="draw-device"] input').first();
    await hostInput.waitFor({ timeout: STEP_TIMEOUT_MS });
    assert.equal(await hostInput.inputValue(), GRBL_HOST, "画板未单独配置时应自动带入工作台已填 IP");

    // 导入一份包含单个实心深色矩形的 Excalidraw 场景 JSON
    const scenePayload = JSON.stringify({
      type: "excalidraw",
      version: 2,
      elements: [
        {
          id: "e2e-rect-1",
          type: "rectangle",
          x: 10,
          y: 10,
          width: 120,
          height: 80,
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
    await page.locator('[data-wb-card="board"] input[type="file"], .topbar-actions input[type="file"]').first().setInputFiles({
      name: "e2e-box.excalidraw.json",
      mimeType: "application/json",
      buffer: Buffer.from(scenePayload, "utf8"),
    });
    await page.getByText(/已导入场景：e2e-box\.excalidraw\.json/).waitFor({ timeout: STEP_TIMEOUT_MS });

    // 开启本次自定义功率和速度并生成预览
    await page.locator("#draw-custom-params").check();
    await page.locator("#draw-custom-power").fill("45");
    await page.locator("#draw-custom-feed").fill("1400");
    await page.getByRole("button", { name: "生成预览", exact: true }).click();
    await page.locator("#draw-origin-note").waitFor({ timeout: STEP_TIMEOUT_MS });

    // 先走边框：确认后请求必须带 relative_to_head: true 且模拟器收到 G92 移动行
    const beforeFrame = rec.events.length;
    await page.locator("#draw-framing-btn").click();
    await page.getByTestId("app-dialog").getByRole("button", { name: "开始走边框", exact: true }).click();
    await waitFor(() => rec.events.slice(beforeFrame).includes("line"), "画板走边框必须驱动设备");
    assert.equal(framingPayload?.relative_to_head, true, "画板走边框必须携带 relative_to_head: true");

    // 打开确认发送面板：默认应为「在线流式发送」（未默认强勾 SD 脱机）
    await page.locator("#draw-confirm-send-btn").click();
    await page.getByRole("dialog", { name: "确认开始雕刻" }).getByRole("button", { name: "在线流式发送" }).waitFor({ timeout: 5000 });
    await page.getByRole("dialog", { name: "确认开始雕刻" }).getByRole("button", { name: "取消" }).click();

    // 修改左侧成品宽度，立即标过期并禁用发送与走边框
    await page.locator('[data-wb-card="draw-material"] .dimension-grid input').first().fill("40");
    await page.locator("#draw-stale-notice").waitFor({ timeout: 5000 });
    assert.equal(await page.locator("#draw-confirm-send-btn").isDisabled(), true, "改尺寸后必须禁用旧预览发送");
    assert.equal(await page.locator("#draw-framing-btn").isDisabled(), true, "改尺寸后必须禁用旧边框对位");
  } catch (error) {
    await diagnose(error);
  } finally {
    await context.close();
    rec.close();
  }
});

test("[UI-06] 材料实验室: 合并导入确认框 + 保存不跳首项 + 标定矩阵改参过期拦截 + 开刻确认下发 + 联动工作台推荐遍数", async () => {
  const rec = await startRecordingGrbl({ ackDelayMs: 0 });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.addInitScript(([host, port]) => {
    localStorage.setItem("laser-forge-conn-mode", "network");
    localStorage.setItem("laser-forge-host", host);
    localStorage.setItem("laser-forge-telnet-port", port);
  }, [GRBL_HOST, String(rec.port)]);
  const page = await context.newPage();
  const diagnose = traceApi(page);
  try {
    await page.goto(`${API_BASE}/material-lab`);
    await page.getByText("材料库").first().waitFor({ timeout: STEP_TIMEOUT_MS });

    // 1. 导入外部材料库：弹出「选择材料库导入方式」，点击「合并到现有库（推荐）」保留原有椴木并合入新材料
    const externalLib = JSON.stringify({
      version: 1,
      power_scale: 1000,
      materials: {
        UI联动竹板: {
          aliases: ["ui-bamboo"],
          thicknesses: {
            "4": {
              cut: {
                laser_min_power: 0,
                laser_max_power: 800,
                feed_rate: 360,
                travel_rate: 3000,
                threshold: 128,
                passes: 3,
                pixel_size_mm: 0.1,
              },
            },
          },
        },
      },
    });
    await page.locator('.material-lab-page input[type="file"]').setInputFiles({
      name: "external-materials.json",
      mimeType: "application/json",
      buffer: Buffer.from(externalLib, "utf8"),
    });
    await page.getByTestId("app-dialog").getByRole("button", { name: "合并到现有库（推荐）", exact: true }).click();
    await page.getByTestId("app-dialog").getByRole("button", { name: "确定", exact: true }).click();
    await page.getByRole("cell", { name: /UI联动竹板/ }).waitFor({ timeout: STEP_TIMEOUT_MS });
    await page.getByRole("cell", { name: /椴木/ }).waitFor({ timeout: STEP_TIMEOUT_MS });

    // 2. 选中非首项材料「UI联动竹板 · 4 mm」，切到边缘切割修改参数保存后焦点仍保持在「UI联动竹板」
    await page.getByRole("cell", { name: /UI联动竹板/ }).click();
    await page.getByRole("tab", { name: /边缘切割/ }).click();
    await page.getByRole("button", { name: "保存到材料库" }).click();
    await page.getByText("参数已成功保存！").waitFor({ timeout: STEP_TIMEOUT_MS });
    await page.locator(".lab-heading").filter({ hasText: "UI联动竹板 · 4 mm" }).waitFor({ timeout: 5000 });

    // 3. 生成 2x2 切割标定矩阵 -> 改功率区间立即触发过期拦截 -> 重新生成后点发送弹 showConfirm 并完成下发
    const matrixInputs = page.locator(".lab-grid-2 input");
    await matrixInputs.nth(4).fill("2");
    await matrixInputs.nth(5).fill("2");
    await page.getByRole("button", { name: "生成矩阵" }).click();
    await page.getByText(/边缘切割矩阵已生成/).waitFor({ timeout: STEP_TIMEOUT_MS });

    await matrixInputs.nth(1).fill("850");
    await page.locator(".lab-stale-note").waitFor({ timeout: 5000 });
    assert.equal(await page.getByRole("button", { name: "发送测试" }).isDisabled(), true, "修改标定区间后必须禁用旧矩阵发送");

    await page.getByRole("button", { name: "重新生成矩阵" }).click();
    await page.getByText(/边缘切割矩阵已生成/).waitFor({ timeout: STEP_TIMEOUT_MS });
    await page.getByRole("button", { name: "发送测试" }).click();
    await page.getByTestId("app-dialog").getByRole("button", { name: "确认开刻测试", exact: true }).click();
    await page.getByText(/标定矩阵加工完成/).waitFor({ timeout: STEP_TIMEOUT_MS });

    // 4. 点击「去工作台使用」直达首页，切到文字「外框切割」，验证自动选中「UI联动竹板 4mm」并显示真实「3 遍」推荐
    await page.getByRole("button", { name: "去工作台使用" }).click();
    await page.locator("#wb-text-style").waitFor({ timeout: STEP_TIMEOUT_MS });
    await page.locator("#wb-text-style").selectOption("cut");
    await page.getByText(/功率 80% · 速度 360 mm\/min · 3 遍/).waitFor({ timeout: STEP_TIMEOUT_MS });
  } catch (error) {
    await diagnose(error);
  } finally {
    await context.close();
    rec.close();
  }
});
