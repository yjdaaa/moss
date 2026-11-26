import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = process.env.PLAYWRIGHT_MODULE
  ? await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href)
  : require("playwright");

const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));

const vite = await createServer({
  root,
  optimizeDeps: { entries: [], include: ["react", "react-dom/client"] },
  server: { host: "127.0.0.1", port: 0, strictPort: false },
  logLevel: "error",
  plugins: [
    {
      name: "voice-device-allow-fixture",
      configureServer(server) {
        server.middlewares.use(async (req, res, next) => {
          if (req.url !== "/voice-device-allow-test") {
            next();
            return;
          }
          res.setHeader("Content-Type", "text/html");
          res.end(
            await server.transformIndexHtml(
              "/voice-device-allow-test",
              `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"></head><body><div id="root"></div><script type="module">
                import React, { useState } from "react";
                import { createRoot } from "react-dom/client";
                import { AppDialogHost } from "/src/components/AppDialog.jsx";
                import { VoiceDeviceAllowModal } from "/src/components/VoiceDeviceAllowModal.jsx";
                import "/src/styles.css";

                function Harness() {
                  const [open, setOpen] = useState(false);
                  return React.createElement(
                    "main",
                    { style: { padding: 20 } },
                    React.createElement(
                      "button",
                      {
                        type: "button",
                        id: "trigger-open-modal",
                        className: "btn btn-ghost",
                        onClick: () => setOpen(true),
                      },
                      "模拟新小智连入",
                    ),
                    open
                      ? React.createElement(VoiceDeviceAllowModal, {
                          voiceEnabled: true,
                          currentPath: "/voice",
                          initialStatus: window.__TEST_STATUS__,
                          nowSeconds: 2000,
                        })
                      : null,
                    React.createElement(AppDialogHost),
                  );
                }

                createRoot(document.getElementById("root")).render(React.createElement(Harness));
              </script></body></html>`,
            ),
          );
        });
      },
    },
  ],
});

await vite.listen();
const base = vite.resolvedUrls?.local?.[0]?.replace(/\/$/, "");
const browser = await chromium.launch({
  headless: true,
  channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined),
});

try {
  const page = await browser.newPage({ viewport: { width: 960, height: 760 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));

  await page.addInitScript(() => {
    const state = {
      local: { settings: { auto_allow_devices: false } },
      devices: [
        {
          device_id: "aa:bb:cc:00:00:01",
          name: "",
          status: "pending",
          first_seen: 1700,
          last_seen: 1996,
          model: "lichuang-dev",
          firmware_version: "2.2.0",
          last_ip: "192.0.2.21",
        },
        {
          device_id: "aa:bb:cc:00:00:02",
          name: "书桌小智",
          status: "pending",
          first_seen: 1100,
          last_seen: 1950,
          model: "esp32-s3-box",
          firmware_version: "2.3.0",
          last_ip: "192.0.2.22",
        },
        {
          device_id: "aa:bb:cc:00:00:03",
          name: "客厅小智",
          status: "pending",
          first_seen: 900,
          last_seen: 1900,
          model: "atom-s3r",
          firmware_version: "2.1.0",
        },
      ],
    };
    window.__TEST_STATUS__ = structuredClone(state);
    window.__TAURI_CALLS__ = [];
    window.__HOLD_PROMISES__ = {};
    window.__CLIPBOARD_TEXT__ = "";
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async (txt) => {
          window.__CLIPBOARD_TEXT__ = txt;
        },
      },
    });
    window.__TAURI__ = {
      core: {
        invoke: async (cmd, args) => {
          window.__TAURI_CALLS__.push({ cmd, args });
          if (window.__HOLD_PROMISES__[cmd]) {
            await window.__HOLD_PROMISES__[cmd];
          }
          if (cmd === "voice_device_set_status") {
            const dev = state.devices.find((d) => d.device_id === args.deviceId);
            if (dev) dev.status = args.status;
            return structuredClone(state);
          }
          if (cmd === "voice_device_rename") {
            const dev = state.devices.find((d) => d.device_id === args.deviceId);
            if (dev) dev.name = args.name;
            return structuredClone(state);
          }
          if (cmd === "voice_local_save") {
            Object.assign(state.local.settings, args.settings || {});
            return structuredClone(state);
          }
          return structuredClone(state);
        },
      },
    };
  });

  await page.goto(`${base}/voice-device-allow-test`);
  const trigger = page.locator("#trigger-open-modal");
  await trigger.focus();
  await trigger.click();
  await page.locator("#voice-device-allow-modal").waitFor();

  // C1 防冒认提示与 C4 初始焦点落在备注名输入框（不在允许按钮上，回车不误放行）
  await page.waitForFunction(() => document.activeElement?.id === "voice-device-allow-name");
  assert.notEqual(await page.evaluate(() => document.activeElement?.id), "voice-device-allow-confirm");
  assert.match(await page.locator("#voice-device-allow-verify-tip").textContent(), /请确认身边的小智屏幕正显示「等待允许」，再点允许。/);

  // C2 标题显示型号（无名称时）、MAC 复制与相对时间
  assert.equal(await page.locator("#voice-device-allow-heading").textContent(), "lichuang-dev");
  assert.equal(await page.locator("#voice-device-allow-seen").textContent(), "5 分钟前 / 刚刚");
  await page.locator("#voice-device-allow-copy-mac").click();
  assert.equal(await page.evaluate(() => window.__CLIPBOARD_TEXT__), "aa:bb:cc:00:00:01");
  await page.locator("#voice-device-allow-copy-mac").getByText("已复制").waitFor();

  // C4 备注名输入框内按回车不得触发允许，拒绝按钮为非实心描边样式且允许按钮在最右侧
  await page.locator("#voice-device-allow-name").fill("工作台一号");
  await page.locator("#voice-device-allow-name").press("Enter");
  assert.equal(await page.evaluate(() => window.__TAURI_CALLS__.length), 0, "输入框回车不得触发允许请求");

  const btnMetrics = await page.evaluate(() => {
    const rejectEl = document.getElementById("voice-device-allow-reject");
    const confirmEl = document.getElementById("voice-device-allow-confirm");
    const laterEl = document.getElementById("voice-device-allow-later");
    const inputEl = document.getElementById("voice-device-allow-name");
    const bodyEl = document.querySelector("#voice-device-allow-modal .mat-modal-body");
    const bodyStyle = getComputedStyle(bodyEl);
    const bodyContentWidth =
      bodyEl.clientWidth - parseFloat(bodyStyle.paddingLeft) - parseFloat(bodyStyle.paddingRight);
    const queueEl = document.getElementById("voice-device-allow-queue");
    const verifyEl = document.getElementById("voice-device-allow-verify-tip");
    return {
      rejectBg: getComputedStyle(rejectEl).backgroundColor,
      rejectHasDanger: rejectEl.classList.contains("btn-danger"),
      confirmRight: confirmEl.getBoundingClientRect().right,
      rejectRight: rejectEl.getBoundingClientRect().right,
      laterRight: laterEl.getBoundingClientRect().right,
      inputWidthRatio: inputEl.getBoundingClientRect().width / bodyContentWidth,
      queueBorderColor: getComputedStyle(queueEl).borderColor,
      verifyBorderColor: getComputedStyle(verifyEl).borderColor,
    };
  });
  assert.equal(btnMetrics.rejectHasDanger, false);
  assert.equal(btnMetrics.rejectBg, "rgba(0, 0, 0, 0)", "拒绝接入按钮应为透明底描边样式而非实心红底");
  assert.ok(btnMetrics.confirmRight > btnMetrics.rejectRight && btnMetrics.rejectRight > btnMetrics.laterRight);
  assert.ok(
    btnMetrics.inputWidthRatio >= 0.9,
    `备注名输入框宽度应不小于弹窗内容区宽度的 90%，实测比例 ${btnMetrics.inputWidthRatio.toFixed(3)}`,
  );
  assert.notEqual(btnMetrics.queueBorderColor, btnMetrics.verifyBorderColor, "队列条使用中性描边，不与琥珀色防冒认提示撞色");

  // C3 多台设备切换与草稿隔离
  await page.locator("#voice-device-allow-auto-all").check();
  assert.match(await page.locator("#voice-device-allow-queue").textContent(), /第 1 \/ 3 台/);
  await page.locator("#voice-device-allow-next").click();
  assert.match(await page.locator("#voice-device-allow-queue").textContent(), /第 2 \/ 3 台/);
  assert.equal(await page.locator("#voice-device-allow-heading").textContent(), "书桌小智");
  assert.equal(await page.locator("#voice-device-allow-name").inputValue(), "书桌小智");
  assert.equal(await page.locator("#voice-device-allow-auto-all").isChecked(), false, "第 1 台的勾选不得串到第 2 台");

  await page.locator("#voice-device-allow-prev").click();
  assert.equal(await page.locator("#voice-device-allow-name").inputValue(), "工作台一号", "切回第 1 台保留原草稿");
  assert.equal(await page.locator("#voice-device-allow-auto-all").isChecked(), true);
  await page.locator("#voice-device-allow-auto-all").uncheck();

  // C4/E3/E4 窄屏（360px）关闭按钮固定在右上角与标题同行、复选框文字正常换行无截断、无横向滚动
  await page.setViewportSize({ width: 360, height: 740 });
  const narrowMetrics = await page.evaluate(() => {
    const checkText = document.getElementById("voice-device-allow-auto-text");
    const closeBtn = document.getElementById("voice-device-allow-close");
    const titleEl = document.getElementById("voice-device-allow-title");
    const inputEl = document.getElementById("voice-device-allow-name");
    const bodyEl = document.querySelector("#voice-device-allow-modal .mat-modal-body");
    const bodyStyle = getComputedStyle(bodyEl);
    const bodyContentWidth =
      bodyEl.clientWidth - parseFloat(bodyStyle.paddingLeft) - parseFloat(bodyStyle.paddingRight);
    return {
      docOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      bodyOverflow: bodyEl.scrollWidth - bodyEl.clientWidth,
      checkScrollWidth: checkText.scrollWidth,
      checkClientWidth: checkText.clientWidth,
      headTopDiff: Math.abs(closeBtn.getBoundingClientRect().top - titleEl.getBoundingClientRect().top),
      inputWidthRatio: inputEl.getBoundingClientRect().width / bodyContentWidth,
    };
  });
  assert.ok(narrowMetrics.docOverflow <= 0, `360px 窄屏不得出现横向滚动，实测溢出 ${narrowMetrics.docOverflow}px`);
  assert.ok(narrowMetrics.bodyOverflow <= 0, `360px 弹窗主体不得出现横向溢出，实测 ${narrowMetrics.bodyOverflow}px`);
  assert.ok(
    narrowMetrics.checkScrollWidth <= narrowMetrics.checkClientWidth,
    `360px 下复选框文字 scrollWidth(${narrowMetrics.checkScrollWidth}) 不得大于 clientWidth(${narrowMetrics.checkClientWidth})`,
  );
  assert.ok(
    narrowMetrics.headTopDiff <= 8,
    `360px 下关闭按钮 top 与标题 top 相差不得超过 8px，实测 ${narrowMetrics.headTopDiff}px`,
  );
  assert.ok(narrowMetrics.inputWidthRatio >= 0.9, "360px 下备注名输入框仍占满内容区宽度");
  await page.setViewportSize({ width: 960, height: 760 });

  // C4 单按钮加载态 & C3/C6 拒绝后 role="status" 播报并自动切到下一台
  await page.locator("#voice-device-allow-next").click(); // 切到第 2 台（书桌小智）测试拒绝
  await page.evaluate(() => {
    window.__HOLD_PROMISES__.voice_device_set_status = new Promise((resolve) => {
      window.__RELEASE_HOLD__ = resolve;
    });
  });
  const rejectClick = page.locator("#voice-device-allow-reject").click();
  await page.locator("#voice-device-allow-reject").getByText("正在拒绝…").waitFor();
  assert.equal(
    await page.locator("#voice-device-allow-confirm").textContent(),
    "允许并记住此设备",
    "拒绝处理中只有被点击的拒绝按钮显示加载态，允许按钮保持原文字并禁用",
  );
  assert.equal(await page.locator("#voice-device-allow-confirm").isDisabled(), true);
  await page.evaluate(() => {
    delete window.__HOLD_PROMISES__.voice_device_set_status;
    window.__RELEASE_HOLD__();
  });
  await rejectClick;
  // 第 2 台（书桌小智）被拒后自动衔接到原第 3 台（客厅小智），且 role="status" 播报拒绝结果
  assert.equal(await page.locator("#voice-device-allow-heading").textContent(), "客厅小智");
  assert.match(await page.locator('#voice-device-allow-status[role="status"]').textContent(), /已拒绝设备「书桌小智」/);

  // C5/E5 允许成功后展示约 1.5s 成功反馈（不渲染拒绝按钮，还有下一台时按钮显示“下一台”），随后自动切到剩余设备
  await page.locator("#voice-device-allow-confirm").click();
  const statusBanner = page.locator('#voice-device-allow-status[role="status"]');
  await statusBanner.waitFor();
  assert.equal(await page.locator("#voice-device-allow-reject").count(), 0, "反馈态下不渲染拒绝接入按钮");
  assert.equal(await page.locator("#voice-device-allow-later").textContent(), "下一台", "队列还有下一台时按钮显示下一台");
  const statusText = await statusBanner.textContent();
  assert.match(statusText, /已允许，现在唤醒小智即可对话/);
  assert.match(statusText, /MQTT 常连模式.*重启设备/);
  assert.match(statusText, /拍照识图.*下次重启后才可用/);
  // 1.5s 后自动切到最后剩余的第 1 台（aa:bb:cc:00:00:01）
  await page.waitForFunction(
    () => document.getElementById("voice-device-allow-mac")?.textContent === "aa:bb:cc:00:00:01",
    null,
    { timeout: 3000 },
  );
  assert.equal(await page.locator("#voice-device-allow-name").inputValue(), "工作台一号");

  // C6 Esc 等同“稍后处理”，关闭后焦点还原到打开弹窗前的按钮
  await page.keyboard.press("Escape");
  await page.locator("#voice-device-allow-modal").waitFor({ state: "detached" });
  assert.equal(
    await page.evaluate(() => document.activeElement?.id),
    "trigger-open-modal",
    "关闭弹窗后焦点须还原到打开前的触发按钮",
  );

  assert.deepEqual(errors, []);
  console.log("PASS voice-device-allow browser UI: anti-spoof, queue isolation, focus guard, 360px wrap, 1.5s status");
  await page.close();
} finally {
  await browser.close();
  await vite.close();
}
