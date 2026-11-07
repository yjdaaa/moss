// 应用内更新与启动顶栏提示条浏览器实测（2026-11-07 用户决定：默认启动自动检查更新、主界面顶栏一键跳转、桌面端与 Android 应用内更新）。
// 全部为回环假数据：不连真实设备、不请求公网 GitHub。
// Run: PLAYWRIGHT_CHANNEL=chrome node tests/app-update.browser.mjs (from frontend/)
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

  async function runScenario(platform) {
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1360, height: 900 } });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));

    const installCalls = [];
    let progressState = {
      supported: true,
      unsupported_reason: "",
      platform,
      phase: "idle",
      version: "",
      downloaded: 0,
      total: null,
      error: "",
    };

    await page.addInitScript(({ initialProgress, loopbackOrigin }) => {
      window.__MOCK_PROGRESS__ = { ...initialProgress };
      window.__MOCK_INSTALL_CALLS__ = [];
      window.__TAURI__ = {
        core: {
          invoke: async (cmd, args) => {
            if (cmd === "get_backend_origin") return loopbackOrigin;
            if (cmd === "get_desktop_session") {
              if (initialProgress.platform === "android") throw new Error("移动端没有桌面会话");
              return "a".repeat(64);
            }
            if (cmd === "app_update_status") {
              return { ...window.__MOCK_PROGRESS__ };
            }
            if (cmd === "app_update_install") {
              window.__MOCK_INSTALL_CALLS__.push(args);
              const ver = String(args?.tag || "v0.4.0").replace(/^[vV]/, "");
              window.__MOCK_PROGRESS__ = {
                ...window.__MOCK_PROGRESS__,
                phase: "downloading",
                version: ver,
                downloaded: 5 * 1048576,
                total: 20 * 1048576,
                error: "",
              };
              return { ...window.__MOCK_PROGRESS__ };
            }
            if (cmd === "get_export_settings") {
              return { directory: "", default_directory: "/tmp/moss-exports", ask_each_time: false, supported: initialProgress.platform !== "android" };
            }
            throw new Error(`Unexpected Tauri command: ${cmd}`);
          },
        },
      };
    }, { initialProgress: progressState, loopbackOrigin: local.replace(/\/$/, "") });

    await page.route("**/*", async (route) => {
      const req = route.request();
      const url = new URL(req.url());
      if (url.pathname === "/api/system/info") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({
            success: true,
            result: {
              version: "0.3.7",
              auto_check_update: true,
              check_prerelease: false,
              actual_port: 9666,
              preferred_port: 9666,
              is_fallback: false,
              web_host: "127.0.0.1",
              mcp: { configured: false, connected: false, endpoint: "", endpoints: [], recent_events: [] },
              voice: null,
            },
          }),
        });
      }
      if (url.pathname === "/api/settings") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: "ok" }),
        });
      }
      if (url.pathname === "/api/system/check-update") {
        let bodyJson = {};
        try {
          bodyJson = JSON.parse(req.postData() || "{}");
        } catch {}
        const includePre = Boolean(bodyJson.include_prerelease);
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({
            success: true,
            result: {
              current_version: "0.3.7",
              latest_version: includePre ? "0.4.1-beta.1" : "0.4.0",
              has_update: true,
              prerelease: includePre,
              published_at: "2026-11-07T06:00:00Z",
              release_url: includePre
                ? "https://github.com/yjdaaa/moss/releases/tag/v0.4.1-beta.1"
                : "https://github.com/yjdaaa/moss/releases/tag/v0.4.0",
              notes: "支持桌面与 Android 软件内直接更新",
              checked_at: Math.floor(Date.now() / 1000),
            },
          }),
        });
      }
      if (url.pathname === "/api/ui/runtime-config") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({
            success: true,
            result: {
              connection: {
                default_mode: "network",
                preferred_modes: ["network", "serial"],
                serial: { default_port: "", baudrate: 115200 },
                network: { default_host: "", transport: "telnet", http_port: 80, telnet_port: 23, timeout: 5 },
              },
              upload: { gcode_suffixes: [".gcode", ".nc"] },
            },
          }),
        });
      }
      if (url.pathname === "/api/ui/material-options") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: { materials: [] } }),
        });
      }
      if (url.pathname === "/api/ui/serial-ports") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: { ports: [], default_port: "", baudrate: 115200 } }),
        });
      }
      if (url.pathname === "/api/jobs/stop-targets") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: { targets: [] } }),
        });
      }
      if (url.pathname === "/api/material-lab/recent") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: { items: [] } }),
        });
      }
      if (url.pathname.startsWith("/v1/approvals")) {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: { approvals: [], server_time: 0, ttl_seconds: 300 } }),
        });
      }
      if (url.pathname === "/api/system/lan-access" || url.pathname === "/api/jev/status") {
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: {} }),
        });
      }
      return route.continue();
    });

    await page.goto(local, { waitUntil: "domcontentloaded" });
    const banner = page.locator("#app-update-banner");
    await banner.waitFor({ state: "visible" });
    assert.match(await banner.textContent(), /MOSS v0\.4\.0/);

    // 点击“立即更新”直接展开设置中的“软件更新”并带入版本信息
    await page.locator("#app-update-banner-open").click();
    const installBtn = page.locator("#settings-install-update-btn");
    await installBtn.waitFor({ state: "visible" });
    assert.equal(await page.locator("#settings-auto-check-update").isChecked(), true, "默认勾选启动自动检查更新");

    if (platform === "macos") {
      assert.match(await installBtn.textContent(), /下载并重启更新/);
      await installBtn.click();
      await page.getByRole("button", { name: "下载并更新", exact: true }).click();
      await page.locator(".app-update-progress").waitFor({ state: "visible" });
      assert.match(await page.locator(".app-update-progress").textContent(), /正在下载 5\.0 \/ 20\.0 MB/);
    } else {
      assert.match(await installBtn.textContent(), /下载并安装更新/);
      await installBtn.click();
      await page.getByRole("button", { name: "下载并安装", exact: true }).click();
      await page.locator(".app-update-progress").waitFor({ state: "visible" });
      assert.match(await page.locator(".app-update-progress").textContent(), /正在下载 5\.0 \/ 20\.0 MB/);

      // 模拟下载验签完成并拉起 Android 系统安装器
      await page.evaluate(() => {
        window.__MOCK_PROGRESS__ = {
          ...window.__MOCK_PROGRESS__,
          phase: "installer_launched",
          downloaded: 20 * 1048576,
          total: 20 * 1048576,
        };
      });
      await page.waitForFunction(
        () => document.querySelector("#settings-install-update-btn")?.textContent?.includes("重新打开安装界面"),
      );
      assert.match(await page.locator(".app-update-progress").textContent(), /已打开系统安装界面/);

      // 再次点击无需重复弹确认框即可直接调用 app_update_install
      await installBtn.click();
      assert.equal(
        (await page.evaluate(() => window.__MOCK_INSTALL_CALLS__)).length,
        2,
        "Android 授予安装权限后再次点击应直接调用安装，无需二次确认弹窗",
      );

      // 切回已打开安装器状态后，开启预览版并检查出更新号 v0.4.1-beta.1：必须清空旧版状态并重新要求确认
      await page.evaluate(() => {
        window.__MOCK_PROGRESS__ = {
          ...window.__MOCK_PROGRESS__,
          phase: "installer_launched",
          version: "0.4.0",
        };
      });
      await page.locator("#settings-check-prerelease").check();
      await page.locator("#settings-check-update-btn").click();
      await page.waitForFunction(
        () => document.querySelector("#settings-install-update-btn")?.textContent?.includes("下载并安装更新"),
      );
      assert.equal(await page.locator(".app-update-progress").count(), 0, "切换到新版本后清除旧版安装器提示");
      await installBtn.click();
      await page.getByRole("button", { name: "下载并安装", exact: true }).click();
      installCalls.push(...(await page.evaluate(() => window.__MOCK_INSTALL_CALLS__)));
      assert.equal(installCalls.length, 3);
      assert.equal(installCalls[2]?.tag, "v0.4.1-beta.1");
    }

    if (installCalls.length === 0) {
      installCalls.push(...(await page.evaluate(() => window.__MOCK_INSTALL_CALLS__)));
    }
    assert.equal(installCalls[0]?.tag, "v0.4.0");
    assert.deepEqual(errors, []);
    await context.close();
  }

  await runScenario("macos");
  await runScenario("android");
  console.log("APP_UPDATE_BROWSER_TEST=PASS");
} finally {
  await browser?.close();
  await vite.close();
}
