import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import {
  DEFAULT_FILL_SETTINGS,
  applyMaterialLayer,
  fillResultNote,
  localSettingsToStore,
  materialLayerFromRecommendation,
  materialLayerHint,
  materialLayerPayload,
  materialLayerSaveRequest,
  normalizeFillSettings,
  settingsFromMaterialLayer,
  timeEstimateNote,
} from "../src/fillOptions.js";

// r40：随材料保存的填充 / 切割设置（对齐 LightBurn 库条目）与工时自校准说明
test("材料库条目的填充 / 切割设置载入面板：只盖条目里有的键，机器级设置保持本机值", () => {
  const local = normalizeFillSettings({ ...DEFAULT_FILL_SETTINGS, angleDeg: "30", scanOffsetMm: "0.08", accelerationMmS2: "800" });
  const { settings, applied } = applyMaterialLayer(local, {
    fill_options: { angle_deg: 45, overscan_mm: 3, bidirectional: false, scan_offset_mm: 0.5 },
  });
  assert.equal(settings.angleDeg, "45");
  assert.equal(settings.overscanAuto, false);
  assert.equal(settings.overscanMm, "3");
  assert.equal(settings.bidirectional, false);
  assert.equal(settings.scanOffsetMm, "0.08", "扫描偏移跟机器走，不从材料库载入");
  assert.equal(settings.accelerationMmS2, "800");
  assert.deepEqual(applied.sort(), ["angleDeg", "bidirectional", "overscanAuto", "overscanMm"]);

  assert.deepEqual(settingsFromMaterialLayer({ fill_options: { overscan_mm: "auto" } }), { overscanAuto: true });
  assert.deepEqual(settingsFromMaterialLayer({ cut_options: { kerf_mm: 0.1, tab_count: 0 } }), { kerfMm: "0.1", tabMode: "off" });
  assert.deepEqual(settingsFromMaterialLayer({ cut_options: { tab_spacing_mm: 30 } }), { tabMode: "spacing", tabSpacingMm: "30" });
  assert.deepEqual(applyMaterialLayer(local, null), { settings: local, applied: [] });

  assert.equal(materialLayerHint([], "椴木 3mm 光栅"), "");
  assert.match(materialLayerHint(["angleDeg", "overscanAuto", "overscanMm"], "椴木 3mm 光栅"), /来自材料库「椴木 3mm 光栅」：扫描角度、过扫描/);
});

test("保存到材料库：雕刻只存填充设置（不含扫描偏移与加速度），切割存切割设置且关桥位记 0", () => {
  const settings = normalizeFillSettings({ ...DEFAULT_FILL_SETTINGS, angleDeg: "90", overscanAuto: false, overscanMm: "2.5", scanOffsetMm: "0.1" });
  assert.deepEqual(materialLayerPayload(settings, { withFill: true }), {
    fill_options: { grouping: "all", angle_deg: 90, cross_hatch: false, bidirectional: true, overscan_mm: 2.5 },
  });
  assert.equal(materialLayerPayload({ ...settings, overscanAuto: true }).fill_options.overscan_mm, "auto");
  assert.deepEqual(materialLayerPayload({ ...settings, kerfMm: "0.12" }, { withCut: true }), {
    cut_options: { kerf_mm: 0.12, lead_in_mm: 0, lead_out_mm: 0, lead_angle_deg: 90, overcut_mm: 0, tab_count: 0 },
  });
  assert.match(materialLayerPayload({ ...settings, angleDeg: "999" }).error, /扫描角度/);
  assert.match(materialLayerPayload(settings, { withFill: false }).error, /没有可存/);

  const layer = materialLayerFromRecommendation({
    material: "椴木", thickness_mm: 8, matched_thickness_mm: 8, match: "surface",
    laser_mode: "engrave", engraving_mode: "raster", fill_options: { angle_deg: 45 },
  });
  assert.equal(layer.label, "椴木 表面通用 光栅");
  assert.deepEqual(materialLayerSaveRequest(layer, { fill_options: { angle_deg: 0 } }), {
    action: "save_layer_options", material: "椴木", surface: true, laser_mode: "engrave",
    engraving_mode: "raster", fill_options: { angle_deg: 0 },
  });
  const cut = materialLayerFromRecommendation({ material: "椴木", thickness_mm: 3, matched_thickness_mm: 3, match: "exact", laser_mode: "cut" });
  assert.equal(cut.label, "椴木 3mm 切割");
  assert.equal(materialLayerSaveRequest(cut, {}).thickness_mm, 3);
  assert.notEqual(cut.key, layer.key);
  assert.equal(materialLayerFromRecommendation(null), null);
});

test("改动取自材料库的键只影响这次生成，不写进本机习惯值", () => {
  const stored = normalizeFillSettings({ ...DEFAULT_FILL_SETTINGS, angleDeg: "0" });
  const next = normalizeFillSettings({ ...DEFAULT_FILL_SETTINGS, angleDeg: "45", crossHatch: true });
  const saved = localSettingsToStore(next, stored, ["angleDeg"]);
  assert.equal(saved.angleDeg, "0");
  assert.equal(saved.crossHatch, true);
  assert.deepEqual(localSettingsToStore(next, stored, []), next);
});

test("结果说明：取自材料库的设置与工时校准都写明", () => {
  assert.match(
    fillResultNote({ grouping: "all", bidirectional: true, overscan_mm: 2, overscan_auto: false, from_material: ["angle_deg"] }),
    /部分设置取自材料库/,
  );
  assert.doesNotMatch(fillResultNote({ grouping: "all", bidirectional: true, overscan_mm: 2, from_material: [] }), /材料库/);
  const base = { model: "acceleration", acceleration_mm_s2: { x: 1000, y: 1000 }, acceleration_source: "default" };
  assert.doesNotMatch(timeEstimateNote(base), /校准/);
  assert.match(
    timeEstimateNote({ ...base, raw_estimated_seconds: 100, estimated_seconds: 120, calibration: { factor: 1.2, samples: 3 } }),
    /已按最近 3 次实际用时校准（×1\.20，原估 100 秒）/,
  );
});

test("填充面板：来自材料库提示与保存按钮", async () => {
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
    const plain = render({});
    assert.doesNotMatch(plain, /wb-material-layer/);
    const loaded = render({ materialHint: "来自材料库「椴木 3mm 光栅」：扫描角度", onSaveToMaterial: () => {} });
    assert.match(loaded, /来自材料库「椴木 3mm 光栅」：扫描角度/);
    assert.match(loaded, /· 来自材料库/);
    assert.match(loaded, /id="wb-material-save"/);
    const empty = render({ materialEntryLabel: "椴木 3mm 光栅", onSaveToMaterial: () => {} });
    assert.match(empty, /还没有存这组设置/);
    assert.match(empty, /保存到材料库/);
  } finally {
    await server.close();
  }
});
