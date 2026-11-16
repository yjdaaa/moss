import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_FILL_SETTINGS,
  FILL_SETTINGS_KEY,
  fillResultNote,
  fillSettingsPayload,
  fillSettingsSignature,
  fillSettingsSummary,
  normalizeFillSettings,
  readFillSettings,
  writeFillSettings,
} from "../src/fillOptions.js";
import { workbenchInputSignature } from "../src/workbenchParams.js";
import { drawGenerationSignature } from "../src/drawConnection.js";

const memoryStore = () => {
  const data = {};
  return { getItem: (k) => data[k] ?? null, setItem: (k, v) => { data[k] = String(v); }, data };
};

test("填充设置默认对齐 LightBurn：一次填完、横扫、双向、过扫描自动、起刀放尖角、去重复线", () => {
  const payload = fillSettingsPayload(DEFAULT_FILL_SETTINGS);
  assert.deepEqual(payload, {
    fill_options: { grouping: "all", angle_deg: 0, cross_hatch: false, bidirectional: true, scan_offset_mm: 0, overscan_mm: "auto" },
    path_options: { prefer_corners: true, remove_overlaps: true },
  });
  assert.equal(fillSettingsSummary(DEFAULT_FILL_SETTINGS), "一次填完 · 横扫 · 双向 · 过扫描自动");
  // 只描轮廓 / 切割：只带走刀选项
  assert.deepEqual(fillSettingsPayload(DEFAULT_FILL_SETTINGS, { withFill: false }), {
    path_options: { prefer_corners: true, remove_overlaps: true },
  });
});

test("填充设置：手动过扫描、加速度、偏移与校验", () => {
  const manual = fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, overscanAuto: false, overscanMm: "3.5", grouping: "individual", angleDeg: "90", crossHatch: true, bidirectional: false, scanOffsetMm: "0.12" });
  assert.equal(manual.fill_options.overscan_mm, 3.5);
  assert.equal(manual.fill_options.grouping, "individual");
  assert.equal(manual.fill_options.angle_deg, 90);
  assert.equal(manual.fill_options.cross_hatch, true);
  assert.equal(manual.fill_options.bidirectional, false);
  assert.equal(manual.fill_options.scan_offset_mm, 0.12);
  assert.equal(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, accelerationMmS2: "2500" }).fill_options.acceleration_mm_s2, 2500);

  assert.match(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, overscanAuto: false, overscanMm: "30" }).error, /过扫描/);
  assert.match(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, scanOffsetMm: "3" }).error, /偏移/);
  assert.match(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, angleDeg: "abc" }).error, /角度/);
  assert.match(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, accelerationMmS2: "1" }).error, /加速度/);
  // 偏移留空按 0
  assert.equal(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, scanOffsetMm: "" }).fill_options.scan_offset_mm, 0);
});

test("填充设置存在本机：读写往返，存储坏了或被禁用时退回默认", () => {
  const store = memoryStore();
  writeFillSettings({ ...DEFAULT_FILL_SETTINGS, grouping: "individual", scanOffsetMm: "0.08" }, store);
  assert.equal(readFillSettings(store).grouping, "individual");
  assert.equal(readFillSettings(store).scanOffsetMm, "0.08");
  store.data[FILL_SETTINGS_KEY] = "{oops";
  assert.deepEqual(readFillSettings(store), normalizeFillSettings(null));
  const blocked = { getItem() { throw new Error("denied"); }, setItem() { throw new Error("denied"); } };
  assert.deepEqual(readFillSettings(blocked), normalizeFillSettings(null));
  assert.doesNotThrow(() => writeFillSettings(DEFAULT_FILL_SETTINGS, blocked));
  // 未知分组、错类型回到默认
  assert.equal(normalizeFillSettings({ grouping: "zigzag", bidirectional: "yes" }).grouping, "all");
  assert.equal(normalizeFillSettings({ bidirectional: "yes" }).bidirectional, true);
});

test("改填充设置让工作台文字与画板矢量预览过期；只描轮廓时只看走刀选项", () => {
  const changed = { ...DEFAULT_FILL_SETTINGS, crossHatch: true };
  const textSig = (fill) => workbenchInputSignature({ activeTool: "text", textContent: "MOSS", textStyle: "fill", fillSignature: fillSettingsSignature(fill) });
  assert.notEqual(textSig(DEFAULT_FILL_SETTINGS), textSig(changed));
  assert.equal(
    fillSettingsSignature(DEFAULT_FILL_SETTINGS, { withFill: false }),
    fillSettingsSignature(changed, { withFill: false }),
    "描边不受填充选项影响",
  );
  const form = { material: "椴木", thickness_mm: "3", mode: "vector" };
  const drawSig = (fill) => drawGenerationSignature(form, { enabled: false, fill: fillSettingsSignature(fill) });
  assert.notEqual(drawSig(DEFAULT_FILL_SETTINGS), drawSig(changed));
  // 光栅模式不用矢量填充设置
  const rasterSig = (fill) => drawGenerationSignature({ ...form, mode: "raster" }, { enabled: false, fill: fillSettingsSignature(fill) });
  assert.equal(rasterSig(DEFAULT_FILL_SETTINGS), rasterSig(changed));
});

test("预览结果说明：实际过扫描与去重条数", () => {
  assert.equal(
    fillResultNote({ grouping: "all", bidirectional: true, overscan_mm: 1.3, overscan_auto: true }, 2),
    "一次填完，双向，过扫描 1.3 mm（按速度自动）；去掉 2 条重复线",
  );
  assert.equal(fillResultNote({ grouping: "individual", bidirectional: false, overscan_mm: 0 }), "逐块填充，单向，无过扫描");
  assert.equal(fillResultNote(null, 0), "");
});
