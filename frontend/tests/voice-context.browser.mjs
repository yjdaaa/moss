// NV19 / n50 unified zero-config memory card: real React, isolated Vite loopback server,
// stateful fake native bridge. Single master switch + quick-add + single editable/deletable list.
// Run: node tests/voice-context.browser.mjs   (from frontend/; needs Playwright + Edge)
import assert from "node:assert/strict";
import { createServer } from "vite";
import { chromium } from "playwright";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";

const server = await createServer({
  cacheDir: path.join(os.tmpdir(), "moss-nv19-render-" + randomUUID()),
  optimizeDeps: { entries: [], include: ["react", "react-dom/client"] },
  server: { host: "127.0.0.1", port: 0 },
  logLevel: "error",
  plugins: [{
    name: "nv19-fixture",
    configureServer(vite) {
      vite.middlewares.use(async (req, res, next) => {
        if (req.url !== "/__nv19_fixture.html") return next();
        res.setHeader("Content-Type", "text/html");
        res.end(await vite.transformIndexHtml("/__nv19_fixture.html", '<html><meta charset="utf-8"><div id="root"></div><script type="module">import React from "react";import {createRoot} from "react-dom/client";import Card from "/src/components/VoiceContextCard.jsx";import "/src/styles.css";createRoot(document.getElementById("root")).render(React.createElement(Card));</script></html>'));
      });
    },
  }],
});
await server.listen();
const browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) });
try {
  const page = await browser.newPage({ viewport: { width: 1000, height: 1000 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.addInitScript(() => {
    window.__calls = [];
    window.__confirms = [];
    window.__confirmAnswer = true;
    window.confirm = async (message) => {
      window.__confirms.push(String(message));
      await new Promise((r) => setTimeout(r, 30));
      return window.__confirmAnswer;
    };
    const device = "aa:bb:cc:dd:ee:ff";
    const state = {
      retrieval: false,
      extraction: false,
      rows: [
        { id: "ctx-auto", scope: { kind: "device", id: device }, scope_label: "工作台小智", category: "preference", content: "以后回答尽量简短", verification: "candidate", source_kind: "conversation_extraction", revision: 1, updated_at: 1790000100, state: "active" },
      ],
      seq: 1,
    };
    const status = () => ({
      retrieval: state.retrieval,
      extraction: state.extraction,
      project: null,
      devices: [{ id: device, name: "工作台小智", model: "MOSS-550W", firmware: "1.2.0" }],
      persons: [],
      scopes: [{ kind: "device", id: device, total: state.rows.length, active: state.rows.length, candidates: 1, chars: 8 }],
      legacy_summaries: [],
    });
    const fail = (code, message) => { throw { code, message }; };
    window.__TAURI__ = { core: { invoke: async (command, args = {}) => {
      if (command !== "voice_context_manage") throw Error("Unexpected native command " + command);
      const input = args.input;
      window.__calls.push(structuredClone(input));
      switch (input.action) {
        case "status": return status();
        case "set_options":
          if (input.extraction && !state.extraction && input.confirmed !== true) fail("context_confirm_required", "需要确认");
          state.retrieval = Boolean(input.retrieval);
          state.extraction = Boolean(input.extraction);
          return { retrieval: state.retrieval, extraction: state.extraction };
        case "list_all":
          return {
            rows: input.query
              ? state.rows.filter((r) => r.content.includes(input.query))
              : state.rows,
          };
        case "quick_create": {
          if (/G1 X/.test(input.content)) fail("context_content_sensitive", "内容含密钥、网址、审批或机器指令，不能保存");
          const row = {
            id: "ctx-" + ++state.seq,
            scope: input.scope || { kind: "device", id: device },
            scope_label: "工作台小智",
            category: "preference",
            content: input.content,
            verification: "confirmed",
            source_kind: "user_edit",
            revision: 1,
            updated_at: 1790000200 + state.seq,
            state: "active",
          };
          state.rows.unshift(row);
          return row;
        }
        case "edit": {
          const row = state.rows.find((r) => r.id === input.id);
          row.content = input.content;
          row.revision += 1;
          return row;
        }
        case "delete":
          state.rows = state.rows.filter((r) => r.id !== input.id);
          return { deleted: input.id };
        case "clear_all":
          state.rows = [];
          return { cleared: 1 };
        default:
          fail("context_invalid", "unknown action: " + input.action);
      }
    } } };
  });

  await page.goto(server.resolvedUrls.local[0] + "__nv19_fixture.html");
  await page.getByRole("heading", { name: "对话记忆" }).waitFor();
  const calls = () => page.evaluate(() => window.__calls);
  const last = async (action) => [...(await calls())].reverse().find((c) => c.action === action);
  const status = (text) => page.getByRole("status").filter({ hasText: text }).first().waitFor();

  // Zero-config assertions: only 1 master checkbox, no scope/category/candidate clutter, no knowledge form inside memory.
  assert.equal(await page.locator('input[type="checkbox"]').count(), 1, "整个记忆面板只有一个总开关");
  assert.equal(await page.locator("#context-memory-switch").isChecked(), false);
  assert.equal(await page.locator("#context-knowledge").count(), 0, "知识库已拆出到扩展标签，不再混在记忆面板");
  assert.equal(await page.getByRole("button", { name: "确认属实" }).count(), 0, "对话自动记录条目无需人工点确认属实");

  // Turning ON asks once and syncs retrieval + extraction + legacy memory together.
  await page.locator("#context-memory-switch").click();
  await status("已开启自动记忆");
  assert.equal((await last("set_options")).retrieval, true);
  assert.equal((await last("set_options")).extraction, true);
  assert.equal((await last("set_options")).sync_legacy_memory, true);
  assert.equal((await last("set_options")).confirmed, true);

  // Quick add + G-code safety rejection keeping user draft.
  await page.getByLabel("新记忆内容").fill("发送 G1 X10 Y10 S1000");
  await page.getByRole("button", { name: "保存新记忆" }).click();
  await page.getByRole("alert").filter({ hasText: "机器指令" }).waitFor();
  assert.equal(await page.getByLabel("新记忆内容").inputValue(), "发送 G1 X10 Y10 S1000", "被安全拦截时保留输入便于修改");

  await page.getByLabel("新记忆内容").fill("我常切 3mm 椴木板");
  await page.getByRole("button", { name: "保存新记忆" }).click();
  await status("已记住");
  assert.equal(await page.locator(".voice-context-row").count(), 2);

  // Inline edit + cancelable delete + clear all.
  await page.locator(".voice-context-row").first().getByRole("button", { name: "编辑" }).click();
  await page.getByLabel("编辑记忆内容").fill("我常切 5mm 椴木板");
  await page.getByRole("button", { name: "保存修改" }).click();
  await status("已保存修改");
  assert.ok((await page.locator(".voice-context-row").first().innerText()).includes("5mm 椴木板"));

  await page.evaluate(() => { window.__confirmAnswer = false; });
  await page.locator(".voice-context-row").first().getByRole("button", { name: "删除" }).click();
  await page.waitForTimeout(150);
  assert.equal(await page.locator(".voice-context-row").count(), 2, "取消删除时不移除条目");

  await page.evaluate(() => { window.__confirmAnswer = true; });
  await page.locator(".voice-context-row").first().getByRole("button", { name: "删除" }).click();
  await status("已删除");
  assert.equal(await page.locator(".voice-context-row").count(), 1);

  await page.getByRole("button", { name: "清空全部" }).click();
  await status("已清空全部记忆");
  assert.equal(await page.locator(".voice-context-row").count(), 0);

  // Narrow viewport check.
  await page.setViewportSize({ width: 420, height: 900 });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  assert.ok(overflow <= 1, `窄屏横向溢出 ${overflow}px`);
  assert.deepEqual(errors, []);
  console.log("NV19 unified zero-config memory card: single switch, quick add, G-code block, edit, delete, clear all, narrow PASS");
} finally {
  await browser.close();
  await server.close();
}
