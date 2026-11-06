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

/** 工作台点击流程：填文字 → 计算加工轨迹 → 启动下发，停在发送覆盖层出现。 */
async function generateAndSend(page, { text, fontSizeMm }) {
  await page.getByPlaceholder("输入要雕刻的字符...").fill(text);
  await page.locator("input.wb-field-5").fill(String(fontSizeMm));
  await page.getByRole("button", { name: "计算加工轨迹", exact: true }).click();
  const send = page.getByRole("button", { name: "启动 JobWorker 逐行下发" });
  await send.waitFor({ timeout: STEP_TIMEOUT_MS });
  await send.click();
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
