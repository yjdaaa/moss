import assert from "node:assert/strict";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { mkdir } from "node:fs/promises";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = process.env.PLAYWRIGHT_MODULE
  ? await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href)
  : require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
let vite = null;
let base = process.env.FEEDBACK_TEST_URL;
if (!base) {
  vite = await createServer({
    root,
    optimizeDeps: { entries: [], include: ["react", "react-dom/client"] },
    server: { host: "127.0.0.1", port: 0, strictPort: false },
    logLevel: "error",
    plugins: [{
      name: "feedback-fixture",
      configureServer(server) {
        server.middlewares.use(async (req, res, next) => {
          if (req.url !== "/feedback-test") { next(); return; }
          res.setHeader("Content-Type", "text/html");
          res.end(await server.transformIndexHtml("/feedback-test", `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"></head><body><div id="root"></div><script type="module">
            import React from "react";
            import { createRoot } from "react-dom/client";
            import { FeedbackPanel } from "/src/components/FeedbackPanel.jsx";
            import "/src/styles.css";
            createRoot(document.getElementById("root")).render(React.createElement("main", { style: { maxWidth: 640, padding: 16, margin: "auto" } },
              React.createElement(FeedbackPanel, { workflowId: "wf-test", onAdopt: (workflow) => { window.adopted = workflow; } })));
          </script></body></html>`));
        });
      },
    }],
  });
  await vite.listen();
  base = vite.resolvedUrls?.local?.[0]?.replace(/\/$/, "");
}
const output = process.env.FEEDBACK_SCREENSHOT_DIR;
if (output) await mkdir(output, { recursive: true });
const browser = await chromium.launch({
  headless: true,
  channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined),
});
try {
  for (const width of [1280, 390]) {
    const page = await browser.newPage({ viewport: { width, height: 900 } });
    const errors = [];
    const calls = [];
    let entries = [];
    let adopted = false;
    page.on("pageerror", (error) => { errors.push(error.message); console.error(error.message); });
    page.on("requestfailed", (request) => console.error(request.url(), request.failure()?.errorText));
    page.on("dialog", (dialog) => dialog.accept());
    if (process.env.FEEDBACK_TEST_URL) {
      await page.route("**/feedback-test", (route) => route.fulfill({
        contentType: "text/html", body: `<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/src/styles.css"></head><body><div id="root"></div><script type="module">
          const React = (await import("/node_modules/.vite/deps/react.js")).default;
          const {createRoot} = (await import("/node_modules/.vite/deps/react-dom_client.js")).default;
          const {FeedbackPanel} = await import("/src/components/FeedbackPanel.jsx");
          createRoot(document.getElementById("root")).render(React.createElement("main", {style:{maxWidth:640,padding:16,margin:"auto"}},
            React.createElement(FeedbackPanel,{workflowId:"wf-test",onAdopt:(workflow)=>{window.adopted=workflow;}})));
        </script></body></html>`,
      }));
    }
    await page.route((url) => url.pathname.startsWith("/api/"), (route) => {
      const path = new URL(route.request().url()).pathname;
      const body = route.request().postDataJSON();
      calls.push({ path, body });
      let result;
      if (path === "/api/feedback/history") result = { feedback_history: entries };
      else if (path === "/api/feedback") {
        result = {
          feedback_id: "fb-1", workflow_id: "wf-test", feedback_text: body.feedback_text,
          feedback_type: "too_dark", source: "local_rules", fallback_reason: "未启用 OpenRouter Jev",
          original_params: { laser_max_power: 400, feed_rate: 1000, passes: 1, power_scale: 1000 },
          suggested_params: { laser_max_power: 340, feed_rate: 1150, passes: 1, power_scale: 1000 },
          adopted_params: null, can_adopt: true,
        };
        entries = [result];
      } else if (path === "/api/feedback/accept") {
        assert.equal(body.confirmed, true);
        assert.equal(body.feedback_id, "fb-1");
        result = { ...entries[0], adopted_params: entries[0].suggested_params, adopted_workflow_id: "wf-new" };
        entries = [result];
        adopted = true;
      } else if (path === "/api/workflow") {
        assert.equal(body.action, "status");
        result = { workflow_id: "wf-new", status: "preview_ready", artifacts: {} };
      } else throw new Error(`Unexpected API request: ${path}`);
      return route.fulfill({ contentType: "application/json", body: JSON.stringify({ success: true, result }) });
    });
    await page.goto(`${base}/feedback-test`);
    const input = page.getByRole("textbox", { name: "加工效果反馈" });
    await input.fill("太焦了");
    await page.getByRole("button", { name: "提交反馈", exact: true }).click();
    await page.getByText("本地规则", { exact: false }).waitFor();
    assert.equal(adopted, false);
    if (output) await page.screenshot({ path: `${output}/feedback-${width}.png`, fullPage: true });
    await page.getByRole("button", { name: "采用并生成新预览", exact: true }).click();
    await page.waitForFunction(() => window.adopted?.workflow_id === "wf-new");
    assert.equal(adopted, true);
    await page.reload();
    await page.getByRole("button", { name: "打开已生成预览" }).waitFor();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true);
    assert.deepEqual(errors, []);
    assert.equal(calls.some((call) => call.body?.action === "confirm_send"), false);
    console.log(`PASS feedback UI ${width}px: submit, fallback, adopt, reload history, no send, no overflow`);
    await page.close();
  }
} finally {
  await browser.close();
  if (vite) await vite.close();
}
