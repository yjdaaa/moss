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
    const context = await browser.newContext({
      serviceWorkers: "block", viewport: { width: 1360, height: 900 },
      ...(platform === "android" ? { userAgent: "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/130.0.0.0 Mobile Safari/537.36" } : {}),
    });
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
            if (["pick_process_trace_dir", "reset_process_trace_dir", "open_process_trace_dir"].includes(cmd)) {
              return window.__MOCK_TRACE_DIRECTORY__(cmd);
            }
            throw new Error(`Unexpected Tauri command: ${cmd}`);
          },
        },
      };
    }, { initialProgress: progressState, loopbackOrigin: local.replace(/\/$/, "") });

    // n105 完整过程记录：设置页开关、确认框、占用显示与清空
    const defaultTraceDir = "/tmp/moss-runtime/logs/process-trace";
    const traceState = {
      enabled: false, bytes: 0, days: 0, settings: [], cleared: 0,
      dir: defaultTraceDir, custom_dir: null, available: true, error: null,
      picked: null, directoryError: null, directoryCalls: [], statusCalls: 0,
    };
    const traceResult = () => ({
      enabled: traceState.enabled,
      dir: traceState.dir,
      custom_dir: traceState.custom_dir,
      available: traceState.available,
      error: traceState.error,
      bytes: traceState.bytes,
      days: traceState.days,
      retention_days: 30,
    });
    await page.exposeFunction("__MOCK_TRACE_DIRECTORY__", (command) => {
      traceState.directoryCalls.push(command);
      if (traceState.directoryError) throw new Error(traceState.directoryError);
      if (command === "open_process_trace_dir") return;
      if (command === "pick_process_trace_dir" && !traceState.picked) return null;
      traceState.custom_dir = command === "reset_process_trace_dir" ? null : traceState.picked;
      traceState.dir = traceState.custom_dir || defaultTraceDir;
      traceState.available = true;
      traceState.error = null;
      return traceResult();
    });
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
      if (url.pathname === "/api/process-trace") {
        traceState.statusCalls += 1;
        const body = JSON.parse(req.postData() || "{}");
        if (body.action === "clear") {
          traceState.cleared += 1;
          traceState.bytes = 0;
          traceState.days = 0;
        }
        return route.fulfill({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ success: true, result: traceResult() }),
        });
      }
      if (url.pathname === "/api/settings") {
        const body = JSON.parse(req.postData() || "{}");
        if ("process_trace_enabled" in body) {
          traceState.settings.push(body.process_trace_enabled);
          traceState.enabled = body.process_trace_enabled;
          if (traceState.enabled) {
            traceState.bytes = 5 * 1024 * 1024;
            traceState.days = 1;
          }
        }
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

    if (platform === "macos") {
      // 完整过程记录默认关；打开前先弹确认框说明会保存哪些内容，取消则不保存
      await page.getByRole("button", { name: "完整过程记录" }).click();
      const traceBox = page.locator("#settings-process-trace");
      await traceBox.waitFor({ state: "visible" });
      assert.equal(await traceBox.isChecked(), false, "完整过程记录默认关闭");
      await page.locator("#settings-process-trace-status").getByText(/目前没有记录。只保留最近 30 天，不限大小/).waitFor();
      assert.equal(await page.locator("#settings-process-trace-clear").isDisabled(), true, "没有记录时不能清空");
      await traceBox.click();
      const traceDialog = page.locator('[data-testid="app-dialog"]');
      await traceDialog.getByText(/你对小智说话的录音/).waitFor();
      await traceDialog.getByText(/不保存任何密钥或令牌/).waitFor();
      await traceDialog.locator('[data-dialog-action="cancel"]').click();
      assert.deepEqual(traceState.settings, [], "取消确认不写设置");
      assert.equal(await traceBox.isChecked(), false);
      await traceBox.click();
      assert.equal(await traceDialog.locator('[data-dialog-action="confirm"]').innerText(), "打开记录");
      await traceDialog.locator('[data-dialog-action="confirm"]').click();
      await page.waitForFunction(() => document.querySelector("#settings-process-trace")?.checked === true);
      assert.deepEqual(traceState.settings, [true]);
      await page.locator("#settings-process-trace-status").getByText(/已记录 1 天，共 5\.0 MB/).waitFor();
      // n110 审查：折叠头摘要跟随后端保留天数，不再写死 7 天
      await page.locator("#settings-process-trace-section").getByText("已开启 · 保留 30 天").waitFor();
      await page.locator("#settings-process-trace-status").getByText("/tmp/moss-runtime/logs/process-trace").waitFor();
      // 清空要再确认；关闭开关不弹确认
      await page.locator("#settings-process-trace-clear").click();
      assert.equal(await traceDialog.locator('[data-dialog-action="confirm"]').innerText(), "清空");
      await traceDialog.locator('[data-dialog-action="confirm"]').click();
      await page.locator("#settings-process-trace-status").getByText(/目前没有记录/).waitFor();
      assert.equal(traceState.cleared, 1);

      const pickDir = page.locator("#settings-process-trace-pick");
      const resetDir = page.locator("#settings-process-trace-reset");
      const openDir = page.locator("#settings-process-trace-open");
      assert.equal(await resetDir.isDisabled(), true, "默认位置不能重复恢复默认");
      const callsBeforeCancel = traceState.statusCalls;
      await pickDir.click();
      await page.waitForFunction(() => !document.querySelector("#settings-process-trace-pick")?.disabled);
      assert.equal(traceState.statusCalls, callsBeforeCancel, "取消文件夹选择不刷新或修改位置");
      assert.equal(traceState.dir, defaultTraceDir);

      traceState.picked = "/tmp/moss-custom/moss-process-trace";
      await pickDir.click();
      await page.locator("#settings-process-trace-status").getByText(traceState.picked).waitFor();
      assert.equal(await resetDir.isDisabled(), false);
      await page.locator("#settings-process-trace-section").getByText(/旧记录留在原处，需自行清理/).waitFor();
      await openDir.click();
      await page.waitForFunction(() => !document.querySelector("#settings-process-trace-open")?.disabled);
      assert.deepEqual(traceState.directoryCalls, ["pick_process_trace_dir", "pick_process_trace_dir", "open_process_trace_dir"]);

      traceState.available = false;
      traceState.error = "所选硬盘已断开";
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await page.locator("#settings-process-trace-status").getByText(/所选目录当前不可用，记录已暂停/).waitFor();
      assert.equal(await openDir.isDisabled(), true);
      assert.equal(await resetDir.isDisabled(), false, "目录失联仍能恢复默认");
      assert.equal(await page.locator("#settings-process-trace-clear").isDisabled(), true);
      traceState.available = true;
      traceState.error = null;
      await page.evaluate(() => window.dispatchEvent(new Event("focus")));
      await page.waitForFunction(() => !document.querySelector("#settings-process-trace-open")?.disabled);
      assert.doesNotMatch(await page.locator("#settings-process-trace-status").textContent(), /记录已暂停/);

      traceState.directoryError = "所选目录不可写";
      await pickDir.click();
      await traceDialog.getByText("所选目录不可写").waitFor();
      await traceDialog.locator('[data-dialog-action="confirm"]').click();
      assert.equal(traceState.dir, traceState.picked, "失败保留原保存位置");
      traceState.directoryError = null;
      await resetDir.click();
      await page.locator("#settings-process-trace-status").getByText(defaultTraceDir).waitFor();
      assert.equal(await resetDir.isDisabled(), true);

      await traceBox.click();
      await page.waitForFunction(() => document.querySelector("#settings-process-trace")?.checked === false);
      assert.deepEqual(traceState.settings, [true, false]);

      // 普通浏览器没有原生目录按钮，原有开关与状态仍可使用。
      await page.evaluate(() => {
        window.__SAVED_TAURI__ = window.__TAURI__;
        delete window.__TAURI__;
        window.dispatchEvent(new Event("focus"));
      });
      await pickDir.waitFor({ state: "detached" });
      assert.equal(await traceBox.isVisible(), true);
      await page.evaluate(() => { window.__TAURI__ = window.__SAVED_TAURI__; });

      // 手机连电脑时整个分区不挂载，也不会发起记录目录请求。
      await page.evaluate(async () => {
        const { initializeBackendOrigin } = await import("/src/authSession.js");
        await initializeBackendOrigin({ readRemote: () => "http://moss-test.invalid:9666", probe: async () => {} });
        const [{ default: { createRoot } }, { default: React }, { ProcessTraceSection }] = await Promise.all([
          import("/node_modules/.vite/deps/react-dom_client.js"),
          import("/node_modules/.vite/deps/react.js"),
          import("/src/components/ProcessTraceSection.jsx"),
        ]);
        const mount = document.createElement("div");
        mount.id = "remote-process-trace-test";
        document.body.appendChild(mount);
        createRoot(mount).render(React.createElement(ProcessTraceSection));
        await new Promise((resolve) => requestAnimationFrame(resolve));
      });
      assert.equal(await page.locator("#remote-process-trace-test #settings-process-trace-section").count(), 0);
    } else {
      assert.equal(await page.locator("#settings-process-trace-pick").count(), 0, "Android 不提供桌面目录命令");
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
