import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

import {
  DRY_RUN_CONFIRM_MESSAGE,
  DRY_RUN_DEFAULT_MAX_FEED,
  DRY_RUN_MAX_FEED,
  DRY_RUN_MIN_FEED,
  LASER_BENCH_PROMPT_MAX_LENGTH,
  buildLaserBenchPrompt,
  formatDryRunBannerText,
  formatDryRunRemaining,
  formatLaserBenchStatusLine,
} from "../src/laserBenchPrompt.js";

const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));
const FRONTEND_ROOT = path.resolve(TEST_DIR, "..");

test("LB5: buildLaserBenchPrompt 内容精简、完整覆盖 L0/L1/L2 与前提、满足长度上限且零敏感泄露", () => {
  const prompt = buildLaserBenchPrompt();
  assert.ok(
    prompt.length > 200 && prompt.length <= LASER_BENCH_PROMPT_MAX_LENGTH,
    `提示词长度须在 (200, ${LASER_BENCH_PROMPT_MAX_LENGTH}] 内，当前 ${prompt.length}`,
  );
  // 开头引用项目规范，不重复堆砌内部实现细节
  assert.match(prompt, /遵守 `AGENTS\.md` 与 `docs\/developer\/safety\.md`/);
  assert.doesNotMatch(prompt, /launch_send_job_for|FakeGrbl|sim_guard\.rs/);

  // 三条命令与用途、前提
  assert.match(prompt, /node scripts\/laser-bench\/laser-sim-bench\.mjs/);
  assert.match(prompt, /node scripts\/laser-bench\/laser-readonly-check\.mjs/);
  assert.match(prompt, /node scripts\/laser-bench\/laser-dryrun-bench\.mjs/);
  assert.match(prompt, /打开空跑开关/);
  assert.match(prompt, /出光永远由用户手动/);
  assert.match(prompt, /<runtime>\/laser-bench\/\{sim,readonly,dryrun\}-<run-id>\//);
  assert.match(prompt, /scripts\/laser-bench\/README\.md/);

  // 汇报开头：问题数 + 安全类问题数
  assert.match(prompt, /问题总数/);
  assert.match(prompt, /不变量违反（安全类）数量/);

  // 子代理 4 角色分工与最多 3 轮
  assert.match(prompt, /如果你支持子代理，按以下角色分工；不支持就由你自己按同样的阶段顺序执行/);
  assert.match(prompt, /1\. 诊断（只测不改）/);
  assert.match(prompt, /2\. 对症修复/);
  assert.match(prompt, /3\. 独立复测/);
  assert.match(prompt, /4\. 提交前审查/);
  assert.match(prompt, /最多 3 轮/);

  // 绝不含令牌、绝对路径或真实设备地址
  assert.doesNotMatch(prompt, /\bBearer\s+[A-Za-z0-9._~+/=-]{6,}/i);
  assert.doesNotMatch(prompt, /\bsk-[A-Za-z0-9_-]{16,}\b/);
  assert.doesNotMatch(prompt, /\/Users\/|\/home\/|[A-Z]:\\Users\\/i);
  assert.doesNotMatch(prompt, /\/dev\/(?:cu|tty)\.|COM\d+/i);
  assert.doesNotMatch(prompt, /\b(?:10|172|192)\.\d{1,3}\.\d{1,3}\.\d{1,3}\b/);

  const grblPrompt = buildLaserBenchPrompt({ profile: "grbl" });
  assert.match(grblPrompt, /laser-sim-bench\.mjs --profile grbl/);
  assert.ok(grblPrompt.length <= LASER_BENCH_PROMPT_MAX_LENGTH);
});

test("LB5: 空跑状态行、顶栏标记文案与开启前确认提示符合约定", () => {
  assert.equal(DRY_RUN_DEFAULT_MAX_FEED, 2000);
  assert.equal(DRY_RUN_MIN_FEED, 300);
  assert.equal(DRY_RUN_MAX_FEED, 5000);
  assert.match(DRY_RUN_CONFIRM_MESSAGE, /不出光/);
  assert.match(DRY_RUN_CONFIRM_MESSAGE, /限速/);
  assert.match(DRY_RUN_CONFIRM_MESSAGE, /急停照常可用/);
  assert.match(DRY_RUN_CONFIRM_MESSAGE, /60 分钟后自动关闭/);

  assert.equal(formatDryRunRemaining(3540), "59 分钟");
  assert.equal(formatDryRunRemaining(25), "不足 1 分钟");
  assert.equal(formatLaserBenchStatusLine({ connected: true, dryRun: { enabled: false } }), "已连接 · 空跑关闭");
  assert.equal(formatLaserBenchStatusLine({ connected: false, dryRun: { enabled: false } }), "未连接 · 空跑关闭");
  assert.equal(
    formatLaserBenchStatusLine({
      connected: true,
      dryRun: { enabled: true, max_feed_mm_min: 1800, remaining_seconds: 3480 },
    }),
    "已连接 · 空跑开启（限速 1800 mm/min · 剩余 58 分钟）",
  );
  assert.equal(formatDryRunBannerText({ enabled: false }), "");
  assert.equal(
    formatDryRunBannerText({ enabled: true, max_feed_mm_min: 1800, remaining_seconds: 3480 }),
    "空跑模式（不出光 · 限速 1800 mm/min · 剩余 58 分钟）",
  );
});

test("LB5: LaserBenchSection 精简卡片仅含三项（状态行、空跑开关与限速、复制按钮），开启需确认，顶栏展示空跑标记", async () => {
  const server = await createServer({
    root: FRONTEND_ROOT,
    logLevel: "error",
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    const { LaserBenchSection } = await server.ssrLoadModule("/src/components/LaserBenchSection.jsx");
    const { SettingsModal } = await server.ssrLoadModule("/src/components/SettingsModal.jsx");
    const { Navbar } = await server.ssrLoadModule("/src/components/Navbar.jsx");

    // 1) 关闭态渲染：只含 3 样核心元素，不含 L0/L1/L2 分级列表或“查看记录”按钮
    const offHtml = renderToStaticMarkup(
      React.createElement(LaserBenchSection, {
        canEdit: true,
        connectionStatus: { connected: true },
        initialDryRun: { enabled: false, max_feed_mm_min: 2000, remaining_seconds: 0 },
      }),
    );
    assert.match(offHtml, /id="settings-laser-bench-section"/);
    assert.match(offHtml, /id="settings-laser-bench-status"[^>]*>已连接 · 空跑关闭</);
    assert.match(offHtml, /id="settings-laser-dry-run-toggle"/);
    assert.match(offHtml, /允许 AI 真机空跑测试（不出光、限速）/);
    assert.match(offHtml, /id="settings-laser-dry-run-feed"/);
    assert.match(offHtml, /id="settings-laser-bench-copy-btn"/);
    assert.match(offHtml, /一键复制 AI 激光测试提示词/);
    // 不包含被删掉的分级列表和多余按钮
    assert.doesNotMatch(offHtml, /settings-laser-bench-levels|L0 模拟机全自动|L1 真机只读核对|L2 真机空跑|查看记录/);
    // 除 FoldHeader 折叠按钮外，卡片内仅有一个操作按钮（复制提示词）
    const actionButtons = offHtml.match(/<button\b[^>]*id="settings-laser-bench-[^"]*"/g) || [];
    assert.equal(actionButtons.length, 1, `卡片内只应有一个操作按钮，实际：${actionButtons.join(", ")}`);

    // 2) 开启态渲染：显示剩余时间
    const onHtml = renderToStaticMarkup(
      React.createElement(LaserBenchSection, {
        canEdit: true,
        connectionStatus: { connected: true },
        initialDryRun: { enabled: true, max_feed_mm_min: 1500, remaining_seconds: 2400 },
      }),
    );
    assert.match(onHtml, /id="settings-laser-dry-run-remaining"[^>]*>剩余 40 分钟</);
    assert.match(onHtml, /已连接 · 空跑开启（限速 1500 mm\/min · 剩余 40 分钟）/);

    // 3) 无操作者权限时隐藏
    assert.equal(renderToStaticMarkup(React.createElement(LaserBenchSection, { canEdit: false })), "");

    // 4) 开关开启必须经 showConfirm 二次确认
    const sectionSrc = readFileSync(path.join(FRONTEND_ROOT, "src/components/LaserBenchSection.jsx"), "utf8");
    assert.match(sectionSrc, /showConfirm\s*\(\s*DRY_RUN_CONFIRM_MESSAGE/);
    assert.match(sectionSrc, /<FoldHeader[^>]*summary=/, "折叠标题须带当前值摘要");

    // 5) 接入 SettingsModal 诊断区（紧邻完整过程记录之后）
    const settingsHtml = renderToStaticMarkup(
      React.createElement(SettingsModal, { visible: true, onClose: () => {} }),
    );
    const traceIdx = settingsHtml.indexOf('id="settings-process-trace-section"');
    const benchIdx = settingsHtml.indexOf('id="settings-laser-bench-section"');
    assert.ok(traceIdx > 0 && benchIdx > traceIdx, "AI 激光自主测试卡片应位于偏好设置完整过程记录旁");

    // 6) 顶栏在空跑开启时显示醒目标记，关闭时不显示
    const navOn = renderToStaticMarkup(
      React.createElement(Navbar, {
        currentPath: "/",
        onNavigate: () => {},
        connectionStatus: { connected: true },
        dryRunStatus: { enabled: true, max_feed_mm_min: 1800, remaining_seconds: 3000 },
      }),
    );
    assert.match(navOn, /id="nav-dry-run-badge"/);
    assert.match(navOn, /空跑模式（不出光 · 限速 1800 mm\/min · 剩余 50 分钟）/);

    const navOff = renderToStaticMarkup(
      React.createElement(Navbar, {
        currentPath: "/",
        onNavigate: () => {},
        connectionStatus: { connected: true },
        dryRunStatus: { enabled: false, max_feed_mm_min: 1800, remaining_seconds: 0 },
      }),
    );
    assert.doesNotMatch(navOff, /id="nav-dry-run-badge"/);
  } finally {
    await server.close();
  }
});
