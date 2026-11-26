import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import { DEFAULT_FILL_SETTINGS } from "../src/fillOptions.js";

// 2026-11-16：外框切割时显示“切割设置”，填充时显示按速度的扫描偏移表
test("填充与走刀设置面板：切割设置只在外框切割时出现，偏移表随填充出现", async () => {
  const server = await createServer({
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    server: { middlewareMode: true },
  });
  try {
    const { default: FillOptionsPanel } = await server.ssrLoadModule("/src/components/FillOptionsPanel.jsx");
    const render = (props) =>
      renderToStaticMarkup(
        React.createElement(FillOptionsPanel, { idPrefix: "wb", settings: DEFAULT_FILL_SETTINGS, onChange: () => {}, ...props }),
      );

    const cut = render({ withFill: false, withCut: true });
    assert.match(cut, /切割与走刀设置/);
    assert.match(cut, /id="wb-cut-kerf"/);
    assert.match(cut, /id="wb-cut-tab-mode"/);
    assert.match(cut, /id="wb-cut-lead-in"/);
    assert.match(cut, /id="wb-cut-overcut"/);
    assert.doesNotMatch(cut, /wb-cut-tab-width/, "不加桥位时不显示桥位宽度");
    assert.doesNotMatch(cut, /wb-cut-lead-angle/, "未设引入/引出线时不显示引线角度");
    assert.doesNotMatch(cut, /wb-fill-offset-table/);

    const withLead = render({ withFill: false, withCut: true, settings: { ...DEFAULT_FILL_SETTINGS, leadInMm: "1.5" } });
    assert.match(withLead, /id="wb-cut-lead-angle"/, "开启引入线后显示引线角度");

    const tabbed = render({ withFill: false, withCut: true, settings: { ...DEFAULT_FILL_SETTINGS, tabMode: "count" } });
    assert.match(tabbed, /id="wb-cut-tab-count"/);
    assert.match(tabbed, /id="wb-cut-tab-width"/);
    assert.match(tabbed, /id="wb-cut-tab-skip-inner"/);

    const fill = render({
      withFill: true,
      settings: { ...DEFAULT_FILL_SETTINGS, scanOffsetTable: [{ speed: "1000", offset: "0.1" }] },
    });
    // 2026-11-16（r41）：有填充时先显示质量预设，细项折叠在“高级设置”里
    assert.match(fill, /^<div class="quality-presets" id="wb-fill-preset".*<details[^>]*>.*高级设置 · 填充与走刀设置/s);
    assert.doesNotMatch(cut, /quality-presets/, "只描轮廓 / 切割没有填充项，不显示预设");
    assert.match(fill, /id="wb-fill-offset-table"/);
    assert.match(fill, /value="1000"/);
    assert.match(fill, /id="wb-fill-offset-add"/);
    assert.doesNotMatch(fill, /wb-cut-kerf/, "雕刻不显示切割设置");

    const unidirFill = render({
      withFill: true,
      settings: { ...DEFAULT_FILL_SETTINGS, bidirectional: false, scanOffsetMm: "0.2", scanOffsetTable: [{ speed: "1000", offset: "0.1" }] },
    });
    assert.doesNotMatch(unidirFill, /wb-fill-offset|wb-fill-offset-table|偏移/, "单向扫描时不显示双向扫描偏移与速度表");
  } finally {
    await server.close();
  }
});
