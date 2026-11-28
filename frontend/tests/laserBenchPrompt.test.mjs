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

test("LB5/LD: buildLaserBenchPrompt 内容精简、指向语音中心开关位置、完整覆盖 L0/L1/L2 且零敏感泄露", () => {
  const prompt = buildLaserBenchPrompt();
  assert.ok(
    prompt.length > 200 && prompt.length <= LASER_BENCH_PROMPT_MAX_LENGTH,
    `提示词长度须在 (200, ${LASER_BENCH_PROMPT_MAX_LENGTH}] 内，当前 ${prompt.length}`,
  );
  // 开头引用项目规范，不重复堆砌内部实现细节
  assert.match(prompt, /遵守 `AGENTS\.md` 与 `docs\/developer\/safety\.md`/);
  assert.doesNotMatch(prompt, /launch_send_job_for|FakeGrbl|sim_guard\.rs/);

  // 代码同步前提、L0 构建前提与三条命令/路径
  assert.match(prompt, /git pull/);
  assert.match(prompt, /node scripts\/verify\.mjs --finish/);
  assert.match(prompt, /最后 30 行/);
  assert.match(prompt, /cargo build -p moss-host --release/);
  assert.match(prompt, /node scripts\/laser-bench\/laser-sim-bench\.mjs/);
  assert.match(prompt, /node scripts\/laser-bench\/laser-readonly-check\.mjs/);
  assert.match(prompt, /node scripts\/laser-bench\/laser-dryrun-bench\.mjs/);
  assert.match(prompt, /语音中心 › 记录与诊断 › AI 联调与测试 › 激光雕刻机测试/);
  assert.match(prompt, /打开空跑开关/);
  assert.doesNotMatch(prompt, /偏好设置/);
  assert.match(prompt, /出光永远由用户手动/);
  assert.match(prompt, /<runtime>\/laser-bench\/\{sim,readonly,dryrun\}-<run-id>\//);
  assert.match(prompt, /scripts\/laser-bench\/README\.md/);

  // 汇报开头：L0/L1/L2 执行情况 + 问题数 + 安全类问题数
  assert.match(prompt, /L0 \/ L1 \/ L2 各自是否执行/);
  assert.match(prompt, /问题总数/);
  assert.match(prompt, /不变量违反（安全类）数量/);

  // 子代理 4 角色分工、复测前快更重启复查空跑开关、最多 3 轮与收尾关开关
  assert.match(prompt, /如果你支持子代理，按以下角色分工；不支持就由你自己按同样的阶段顺序执行/);
  assert.match(prompt, /1\. 诊断（只测不改）/);
  assert.match(prompt, /2\. 对症修复/);
  assert.match(prompt, /3\. 独立复测[\s\S]*verify\.mjs --finish[\s\S]*重启 MOSS[\s\S]*空跑开关[\s\S]*纯脚本或测试改动免快更重启/);
  assert.match(prompt, /4\. 提交前审查/);
  assert.match(prompt, /最多 3 轮/);
  assert.match(prompt, /## 收尾[\s\S]*语音中心 › 记录与诊断 › AI 联调与测试 › 激光雕刻机测试[\s\S]*关闭空跑开关/);

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

test("LD: 激光测试卡片挪入语音中心 AI 联调与测试分组（全链路可见、只读隐藏开关）、偏好设置不再出现且顶栏保留空跑标记", async () => {
  const server = await createServer({
    root: FRONTEND_ROOT,
    logLevel: "error",
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    const { LaserBenchSection } = await server.ssrLoadModule("/src/components/LaserBenchSection.jsx");
    const { AiBenchAssistantCard } = await server.ssrLoadModule("/src/components/voice-builtin/AiBenchAssistantCard.jsx");
    const { SettingsModal } = await server.ssrLoadModule("/src/components/SettingsModal.jsx");
    const { Navbar } = await server.ssrLoadModule("/src/components/Navbar.jsx");

    // 1) embedded 模式关闭态渲染：只含 3 样核心元素（状态行、空跑开关与限速、复制按钮），不含 L0/L1/L2 分级与多余按钮
    const offHtml = renderToStaticMarkup(
      React.createElement(LaserBenchSection, {
        embedded: true,
        canEdit: true,
        connectionStatus: { connected: true },
        initialDryRun: { enabled: false, max_feed_mm_min: 2000, remaining_seconds: 0 },
      }),
    );
    assert.match(offHtml, /id="voice-laser-bench-box"/);
    assert.match(offHtml, /激光雕刻机测试/);
    assert.match(offHtml, /id="voice-laser-bench-status"[^>]*>已连接 · 空跑关闭</);
    assert.match(offHtml, /id="voice-laser-dry-run-toggle"/);
    assert.match(offHtml, /允许 AI 真机空跑测试（不出光、限速）/);
    assert.match(offHtml, /id="voice-laser-dry-run-feed"/);
    assert.match(offHtml, /空跑限速/);
    assert.match(offHtml, /约 33 mm\/s/);
    assert.match(offHtml, /id="voice-laser-bench-copy-btn"/);
    assert.match(offHtml, /一键复制 AI 激光测试提示词/);
    assert.doesNotMatch(offHtml, /settings-laser-bench-levels|L0 模拟机全自动|L1 真机只读核对|L2 真机空跑|查看记录/);
    const actionButtons = offHtml.match(/<button\b/g) || [];
    assert.equal(actionButtons.length, 1, `卡片内只应有一个操作按钮，实际 ${actionButtons.length} 个`);

    // 2) 开启态渲染：显示剩余时间
    const onHtml = renderToStaticMarkup(
      React.createElement(LaserBenchSection, {
        embedded: true,
        canEdit: true,
        connectionStatus: { connected: true },
        initialDryRun: { enabled: true, max_feed_mm_min: 1500, remaining_seconds: 2400 },
      }),
    );
    assert.match(onHtml, /id="voice-laser-dry-run-remaining"[^>]*>剩余 40 分钟</);
    assert.match(onHtml, /已连接 · 空跑开启（限速 1500 mm\/min · 剩余 40 分钟）/);

    // 3) 只读客户端（canEdit=false）：仍显示状态行与复制提示词按钮，隐藏空跑开关与限速输入框
    const readonlyHtml = renderToStaticMarkup(
      React.createElement(LaserBenchSection, {
        embedded: true,
        canEdit: false,
        connectionStatus: { connected: true },
        initialDryRun: { enabled: false, max_feed_mm_min: 2000, remaining_seconds: 0 },
      }),
    );
    assert.match(readonlyHtml, /id="voice-laser-bench-status"[^>]*>已连接 · 空跑关闭</);
    assert.match(readonlyHtml, /id="voice-laser-bench-copy-btn"/);
    assert.doesNotMatch(readonlyHtml, /id="voice-laser-dry-run-toggle"/);
    assert.doesNotMatch(readonlyHtml, /id="voice-laser-dry-run-feed"/);

    // 4) 开关开启必须经 showConfirm 二次确认
    const sectionSrc = readFileSync(path.join(FRONTEND_ROOT, "src/components/LaserBenchSection.jsx"), "utf8");
    assert.match(sectionSrc, /showConfirm\s*\(\s*DRY_RUN_CONFIRM_MESSAGE/);

    // 5) 偏好设置不再渲染激光测试卡片或空跑开关，完整过程记录保持不变
    const settingsHtml = renderToStaticMarkup(
      React.createElement(SettingsModal, { visible: true, onClose: () => {} }),
    );
    assert.match(settingsHtml, /id="settings-process-trace-section"/, "完整过程记录保持不变");
    assert.doesNotMatch(
      settingsHtml,
      /settings-laser-bench-section|voice-laser-bench-box|dry-run-toggle|允许 AI 真机空跑测试/,
      "偏好设置中不再出现激光测试卡片与空跑开关",
    );

    // 6) 语音中心 AI 联调与测试分组（#voice-group-ai-bench-hub）：
    //    - builtin 模式：语音部分在前，激光雕刻机测试子块在后；
    //    - official（非 builtin）模式：只渲染激光雕刻机测试子块，摘要相应调整，不依赖 builtin.status。
    const builtinHubHtml = renderToStaticMarkup(
      React.createElement(AiBenchAssistantCard, {
        mode: "builtin",
        builtin: {
          status: {
            debug_inject: { enabled: false, active: false },
            sessions: [],
            bench_recordings: { corpus_count: 13, total_recordings: 0, speaker_count: 0, complete_speaker_count: 0 },
          },
        },
        connectionStatus: { connected: true },
        dryRunStatus: { enabled: false, max_feed_mm_min: 2000, remaining_seconds: 0 },
      }),
    );
    assert.match(builtinHubHtml, /id="voice-group-ai-bench-hub"/);
    assert.match(builtinHubHtml, /id="voice-debug-inject-switch"/);
    const voiceFoldIdx = builtinHubHtml.indexOf('id="voice-bench-recordings-fold"');
    const laserBoxIdx = builtinHubHtml.indexOf('id="voice-laser-bench-box"');
    assert.ok(voiceFoldIdx > 0 && laserBoxIdx > voiceFoldIdx, "builtin 下语音部分须在前、激光雕刻机测试子块须在后");

    const officialHubHtml = renderToStaticMarkup(
      React.createElement(AiBenchAssistantCard, {
        mode: "official",
        builtin: { status: null },
        connectionStatus: { connected: false },
        dryRunStatus: { enabled: false, max_feed_mm_min: 2000, remaining_seconds: 0 },
      }),
    );
    assert.match(officialHubHtml, /id="voice-group-ai-bench-hub"/);
    assert.match(officialHubHtml, /激光雕刻机测试 · 未连接 · 空跑关闭/, "非 builtin 模式摘要须调整为激光状态");
    assert.match(officialHubHtml, /id="voice-laser-bench-box"/);
    assert.doesNotMatch(
      officialHubHtml,
      /voice-ai-bench-checklist|voice-ai-bench-prompt-actions|voice-bench-recordings-fold|voice-debug-inject-switch/,
      "非 builtin 模式下不渲染语音联调子块",
    );

    // 7) VoicePage 在所有语音模式下均挂载 ai-bench-hub
    const voicePageSrc = readFileSync(path.join(FRONTEND_ROOT, "src/pages/VoicePage.jsx"), "utf8");
    const recordsTab = voicePageSrc.split('<VoiceTabPanel tab="records"')[1].split("</VoiceTabPanel>")[0];
    assert.match(recordsTab, /key:\s*"ai-bench-hub",\s*modes:\s*\["official",\s*"builtin"\]/);

    // 8) 顶栏在空跑开启时显示醒目标记，关闭时不显示
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
