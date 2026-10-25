// Private voice page: task-first navigation using a fake native bridge. No real device or secret.
import assert from "node:assert/strict";
import { createServer } from "vite";
import { chromium } from "playwright";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";

const server = await createServer({
  cacheDir: path.join(os.tmpdir(), "moss-private-nav-" + randomUUID()),
  optimizeDeps: { entries: [], include: ["react", "react-dom/client"] },
  server: { host: "127.0.0.1", port: 0 }, logLevel: "error",
});
await server.listen();
const browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) });
try {
  const page = await browser.newPage({ viewport: { width: 920, height: 850 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    const builtin = { memory_enabled: false, devices: [] };
    window.__TAURI__ = { core: { invoke: async (command) => {
      if (command === "voice_builtin_status") return builtin;
      if (command === "voice_memory_list") return [];
      if (command === "voice_plugins_status") return { settings: {}, keys: {} };
      if (command === "voice_home_status") return { revision: 1, credential_revision: 0, settings: { base_url: "", entities: [] }, key_configured: false };
      if (command === "voice_home_approvals") return { approvals: [], can_control: false };
      if (command === "voice_speaker_manage") return { speakers: [], sessions: [], calibration: { state: "none" }, enrollment: { active: false, collected: 0, message: "未登记" } };
      if (command === "voice_firmware_status") return { artifacts: [], devices: [], records: [] };
      throw Error("Unexpected private command " + command);
    } } };
  });
  await page.goto(new URL("voice-private.html", server.resolvedUrls.local[0]).href);
  await page.getByRole("heading", { name: "隐私与家居" }).waitFor();
  assert.equal(await page.locator(".voice-private-task").count(), 4);
  assert.equal(await page.locator("input, textarea").count(), 0, "task overview has no form fields");
  if (process.env.VOICE_QA_DIR) await page.screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-private-overview.png") });
  await page.getByRole("button", { name: /信息服务/ }).click();
  await page.getByRole("heading", { name: "信息服务", exact: true }).first().waitFor();
  assert.equal(await page.getByLabel("搜索服务").count(), 0, "search settings stay hidden until chosen");
  await page.getByLabel("要设置的服务").selectOption("search");
  await page.getByLabel("搜索服务").selectOption("metaso");
  await page.getByLabel("秘塔 API 地址").fill("https://search.example.test");
  await page.getByRole("button", { name: /返回选择/ }).click();
  await page.getByRole("button", { name: /记忆/ }).click();
  await page.getByRole("heading", { name: "对话记忆" }).waitFor();
  assert.equal(await page.getByRole("heading", { name: "长期记忆、资料与恢复摘要" }).count(), 0, "advanced memory is not opened by default");
  await page.getByRole("button", { name: /返回选择/ }).click();
  await page.getByRole("button", { name: /信息服务/ }).click();
  assert.equal(await page.getByLabel("秘塔 API 地址").inputValue(), "https://search.example.test", "switching tasks preserves an unsaved draft");
  await page.evaluate(() => {
    window.__MOSS_PRIVATE_TARGET__ = "home";
    window.dispatchEvent(new CustomEvent("moss:voice-private-selection", { detail: "home" }));
  });
  await page.getByRole("heading", { name: "家居", exact: true }).waitFor();
  assert.equal(await page.getByLabel("配置备份 JSON").isVisible(), false, "backup fields stay out of the primary flow");
  assert.equal(await page.getByRole("button", { name: "添加白名单实体" }).isVisible(), false, "device controls require opening the device step");
  assert.equal(await page.getByLabel(/Home Assistant HTTPS 地址/).isVisible(), true, "connection stays available as the primary step");
  if (process.env.VOICE_QA_DIR) await page.screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-private-home.png") });
  await page.evaluate(() => window.dispatchEvent(new CustomEvent("moss:voice-private-selection", { detail: "unknown" })));
  assert.equal(await page.getByRole("heading", { name: "家居", exact: true }).count(), 1, "unknown target is ignored");
  await page.setViewportSize({ width: 720, height: 850 });
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "private navigation has no horizontal overflow");
  assert.deepEqual(errors, []);
  console.log("VOICE_PRIVATE_NAV_BROWSER_TEST=PASS");
} finally {
  await browser.close();
  await server.close();
}
