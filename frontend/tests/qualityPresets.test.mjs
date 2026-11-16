import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import { DEFAULT_FILL_SETTINGS, fillSettingsPayload, fillSettingsSignature } from "../src/fillOptions.js";
import {
  CUSTOM_PRESET,
  FILL_QUALITY_PRESETS,
  applyFillPreset,
  applyImagePreset,
  detectFillPreset,
  detectImagePreset,
} from "../src/qualityPresets.js";
import { DEFAULT_IMAGE_EXTRAS, imageParamsPayload, workbenchInputSignature } from "../src/workbenchParams.js";

// 2026-11-16（技术书 r41）质量预设：“均衡”等于原默认值，预设只填高级字段，改了预设字段就显示“自定义”

test("填充预设：默认即均衡，套用后只改预设字段，并进入请求与预览签名", () => {
  assert.equal(detectFillPreset(DEFAULT_FILL_SETTINGS), "balanced");
  const custom = { ...DEFAULT_FILL_SETTINGS, angleDeg: "45", scanOffsetMm: "0.1", kerfMm: "0.1" };
  // 角度、扫描偏移、切割设置不属于预设，改了仍算均衡
  assert.equal(detectFillPreset(custom), "balanced");

  const fast = applyFillPreset(custom, "fast");
  assert.equal(detectFillPreset(fast), "fast");
  assert.equal(fast.angleDeg, "45");
  assert.equal(fast.kerfMm, "0.1");
  assert.equal(fillSettingsPayload(fast).fill_options.grouping, "individual");

  const fine = applyFillPreset(DEFAULT_FILL_SETTINGS, "fine");
  assert.equal(detectFillPreset(fine), "fine");
  const payload = fillSettingsPayload(fine).fill_options;
  assert.equal(payload.cross_hatch, true);
  assert.equal(payload.bidirectional, false);
  assert.equal(payload.overscan_mm, "auto");
  assert.notEqual(fillSettingsSignature(fine), fillSettingsSignature(DEFAULT_FILL_SETTINGS));

  // 均衡请求与原默认完全相同
  assert.deepEqual(
    fillSettingsPayload(applyFillPreset(DEFAULT_FILL_SETTINGS, "balanced")),
    fillSettingsPayload(DEFAULT_FILL_SETTINGS),
  );
  // 手动改了预设字段 → 自定义
  assert.equal(detectFillPreset({ ...fine, bidirectional: true }), CUSTOM_PRESET);
  assert.equal(detectFillPreset({ ...DEFAULT_FILL_SETTINGS, overscanAuto: false }), CUSTOM_PRESET);
  // 未知预设不改设置；三个预设互不相同
  assert.deepEqual(applyFillPreset(DEFAULT_FILL_SETTINGS, "bogus"), { ...DEFAULT_FILL_SETTINGS });
  const keys = Object.values(FILL_QUALITY_PRESETS).map((preset) => JSON.stringify(preset));
  assert.equal(new Set(keys).size, 3);
});

test("图片预设：均衡请求与原来相同，快速 / 精细换行间距倍数与转换方式", () => {
  assert.equal(detectImagePreset("auto", DEFAULT_IMAGE_EXTRAS.intervalScale), "balanced");
  assert.deepEqual(imageParamsPayload({ imageMode: "auto", extras: DEFAULT_IMAGE_EXTRAS }), { image_mode: "auto" });

  const fast = applyImagePreset("fast");
  assert.deepEqual(fast, { imageMode: "auto", intervalScale: "1.5" });
  assert.equal(detectImagePreset(fast.imageMode, fast.intervalScale), "fast");
  assert.deepEqual(
    imageParamsPayload({ imageMode: fast.imageMode, extras: { ...DEFAULT_IMAGE_EXTRAS, intervalScale: fast.intervalScale } }),
    { image_mode: "auto", line_interval_scale: 1.5 },
  );

  const fine = applyImagePreset("fine");
  assert.equal(detectImagePreset(fine.imageMode, fine.intervalScale), "fine");
  assert.deepEqual(
    imageParamsPayload({ imageMode: fine.imageMode, extras: { ...DEFAULT_IMAGE_EXTRAS, intervalScale: fine.intervalScale } }),
    { image_mode: "jarvis", line_interval_scale: 0.8 },
  );

  // 套用“均衡”后的 "1" 与留空等价：请求与签名都不变，不会把旧预览判为过期
  const balanced = applyImagePreset("balanced");
  assert.deepEqual(
    imageParamsPayload({ imageMode: balanced.imageMode, extras: { ...DEFAULT_IMAGE_EXTRAS, intervalScale: balanced.intervalScale } }),
    { image_mode: "auto" },
  );
  const sig = (imageMode, intervalScale) =>
    workbenchInputSignature({ activeTool: "image", uploadedImagePath: "a.png", imageMode, imageExtras: { ...DEFAULT_IMAGE_EXTRAS, intervalScale } });
  assert.equal(sig("auto", ""), sig("auto", "1"));
  assert.notEqual(sig("auto", ""), sig("auto", "1.5"));

  // 改了转换方式或倍数 → 自定义；倍数越界报错；直通模式不提交倍数
  assert.equal(detectImagePreset("jarvis", ""), CUSTOM_PRESET);
  assert.equal(detectImagePreset("auto", "1.2"), CUSTOM_PRESET);
  assert.match(imageParamsPayload({ imageMode: "auto", extras: { intervalScale: "5" } }).error, /行间距倍数/);
  assert.match(imageParamsPayload({ imageMode: "auto", extras: { intervalScale: "abc" } }).error, /行间距倍数/);
  assert.deepEqual(imageParamsPayload({ imageMode: "auto", extras: { intervalScale: "1.5", passThrough: true } }), {
    image_mode: "auto",
    pass_through: true,
  });
  assert.equal(applyImagePreset("bogus"), null);
});

test("预设条：当前预设高亮，与三个预设都不同时显示“自定义”", async () => {
  const server = await createServer({
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    server: { middlewareMode: true },
  });
  try {
    const { default: FillOptionsPanel } = await server.ssrLoadModule("/src/components/FillOptionsPanel.jsx");
    const render = (settings) =>
      renderToStaticMarkup(React.createElement(FillOptionsPanel, { idPrefix: "draw", settings, onChange: () => {} }));
    const balanced = render(DEFAULT_FILL_SETTINGS);
    assert.match(balanced, /id="draw-fill-preset-balanced"[^>]*class="ob-chip on"/);
    assert.match(balanced, /id="draw-fill-preset-fast"[^>]*class="ob-chip"/);
    assert.doesNotMatch(balanced, /自定义/);
    const custom = render({ ...DEFAULT_FILL_SETTINGS, crossHatch: true });
    assert.match(custom, /quality-preset-custom[^>]*>自定义/);
    assert.doesNotMatch(custom, /class="ob-chip on"[^>]*>(快速|均衡|精细)/);
  } finally {
    await server.close();
  }
});
