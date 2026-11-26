import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

const frontendRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

async function withSsrServer(fn) {
  const server = await createServer({
    root: frontendRoot,
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    await fn(server);
  } finally {
    await server.close();
  }
}

// 2026-11-24 n111：新安装默认就选着第一套，模型没下载时不能显示“✓ 正在用”，要写明还缺什么并给一键下载
test("按场景选一套模型：选着但没下载完的组合写明缺什么，下载完才显示正在用", async () => {
  await withSsrServer(async (server) => {
    const { ModelPairingGuide } = await server.ssrLoadModule("/src/components/voice-builtin/ModelPairingGuide.jsx");
    const { MODEL_COMBOS, comboPatch } = await server.ssrLoadModule("/src/voiceState.js");
    const first = MODEL_COMBOS[0];
    const settings = comboPatch(first);
    const render = (models) =>
      renderToStaticMarkup(React.createElement(ModelPairingGuide, { settings, models, busy: "", onApply: () => null, onDownload: () => null }));

    const missingHtml = render([
      { id: "silero_vad_v6", label: "Silero v6.2", installed: true },
      { id: "qwen3_asr_0_6b", label: "Qwen3-ASR", installed: false, progress: { state: "downloading", downloaded: 1, total: 2 } },
    ]);
    assert.match(missingHtml, new RegExp(`id="voice-combo-missing-${first.id}"`));
    assert.match(missingHtml, /还没下载：[^<]*Qwen3-ASR（下载中）/);
    assert.match(missingHtml, new RegExp(`id="voice-combo-download-${first.id}"`), "还有没开始下载的，给一键下载");
    assert.doesNotMatch(missingHtml.split(`id="voice-combo-${MODEL_COMBOS[1].id}"`)[0], /✓ 正在用/, "没下载完不算正在用");

    const ready = [first.vad, first.kws, first.asr, first.tts].map((id) => ({ id, label: id, installed: true }));
    const readyHtml = render(ready);
    assert.match(readyHtml, /✓ 正在用/);
    assert.doesNotMatch(readyHtml, /voice-combo-missing-/);
    assert.match(readyHtml, new RegExp(`id="voice-combo-need-${MODEL_COMBOS[1].id}"`), "未激活套系缺模型时直接列出缺失项并提供下载，不误显示用这套");
  });
});
