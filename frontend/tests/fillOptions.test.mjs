import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_FILL_SETTINGS,
  cutOptionsPayload,
  cutResultNote,
  offsetTablePayload,
  FILL_SETTINGS_KEY,
  fillResultNote,
  fillSettingsPayload,
  fillSettingsSignature,
  fillSettingsSummary,
  normalizeFillSettings,
  rasterScanPayload,
  rasterScanSignature,
  readFillSettings,
  writeFillSettings,
  MACHINE_MOTION_KEY,
  machineMotionFromLimits,
  machineMotionPayload,
  readMachineMotion,
  timeEstimateNote,
  writeMachineMotion,
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
  const manual = fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, overscanAuto: false, overscanMm: "3.5", grouping: "individual", angleDeg: "90", crossHatch: true, bidirectional: true, scanOffsetMm: "0.12" });
  assert.equal(manual.fill_options.overscan_mm, 3.5);
  assert.equal(manual.fill_options.grouping, "individual");
  assert.equal(manual.fill_options.angle_deg, 90);
  assert.equal(manual.fill_options.cross_hatch, true);
  assert.equal(manual.fill_options.bidirectional, true);
  assert.equal(manual.fill_options.scan_offset_mm, 0.12);
  const unidir = fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, bidirectional: false, scanOffsetMm: "0.12" });
  assert.equal(unidir.fill_options.bidirectional, false);
  assert.equal(unidir.fill_options.scan_offset_mm, 0, "单向扫描不套用双向偏移");
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
  const form = { material: "椴木", thickness_mm: "3" };
  const drawSig = (fill) => drawGenerationSignature(form, { enabled: false, fill: fillSettingsSignature(fill) }, "vector");
  assert.notEqual(drawSig(DEFAULT_FILL_SETTINGS), drawSig(changed));
  // 转图片（光栅扫描）不用矢量填充设置：页面只给扫描子集的签名（2026-11-17 r45）
  const rasterSig = (fill) => drawGenerationSignature({ ...form, image_strategy: "raster" }, { enabled: false, fill: rasterScanSignature(fill) }, "image");
  assert.equal(rasterSig(DEFAULT_FILL_SETTINGS), rasterSig(changed));
  assert.notEqual(rasterSig(DEFAULT_FILL_SETTINGS), rasterSig({ ...DEFAULT_FILL_SETTINGS, bidirectional: false }), "双向扫描影响位图刀路");
  // 轮廓描边不看填充 / 扫描设置
  const outlineSig = (fill) => drawGenerationSignature({ ...form, image_strategy: "outline" }, { enabled: false, fill }, "image");
  assert.equal(outlineSig(fillSettingsSignature(DEFAULT_FILL_SETTINGS)), outlineSig(fillSettingsSignature(changed)));
});

test("预览结果说明：实际过扫描与去重条数", () => {
  assert.equal(
    fillResultNote({ grouping: "all", bidirectional: true, overscan_mm: 1.3, overscan_auto: true }, 2),
    "一次填完，双向，过扫描 1.3 mm（按速度自动）；去掉 2 条重复线",
  );
  assert.equal(fillResultNote({ grouping: "individual", bidirectional: false, overscan_mm: 0 }), "逐块填充，单向，无过扫描");
  assert.equal(fillResultNote(null, 0), "");
});

test("机器运动参数：握手读到的加速度记在本机并随预览转发，手填加速度优先", () => {
  const limits = { x_travel_mm: 100, x_accel_mm_s2: 800, y_accel_mm_s2: 600, x_max_rate_mm_per_min: 6000, y_max_rate_mm_per_min: null, junction_deviation_mm: 0.01 };
  const motion = machineMotionFromLimits(limits);
  assert.deepEqual(motion, { x_accel_mm_s2: 800, y_accel_mm_s2: 600, x_max_rate_mm_per_min: 6000, junction_deviation_mm: 0.01 });
  assert.equal(machineMotionFromLimits({ x_travel_mm: 100 }), null, "只有行程时不算运动参数");
  assert.equal(machineMotionFromLimits(null), null);

  const store = memoryStore();
  writeMachineMotion(limits, store);
  assert.deepEqual(readMachineMotion(store), motion);
  store.data[MACHINE_MOTION_KEY] = "{bad";
  assert.equal(readMachineMotion(store), null);

  assert.deepEqual(machineMotionPayload(DEFAULT_FILL_SETTINGS, motion), { machine_motion: motion });
  assert.deepEqual(machineMotionPayload(DEFAULT_FILL_SETTINGS, null), {}, "都没有时交给后端默认值");
  const manual = machineMotionPayload({ ...DEFAULT_FILL_SETTINGS, accelerationMmS2: "2500" }, motion);
  assert.equal(manual.machine_motion.x_accel_mm_s2, 2500);
  assert.equal(manual.machine_motion.y_accel_mm_s2, 2500);
  assert.equal(manual.machine_motion.x_max_rate_mm_per_min, 6000, "手填只覆盖加速度");
});

test("工时说明写明按多大加速度估算、来自哪里", () => {
  assert.equal(
    timeEstimateNote({ model: "acceleration", acceleration_mm_s2: { x: 1000, y: 1000 }, acceleration_source: "default" }),
    "按默认加速度 1000 mm/s² 估算加减速（连接机器后自动读取实际值）",
  );
  assert.equal(
    timeEstimateNote({ model: "acceleration", acceleration_mm_s2: { x: 800, y: 600 }, acceleration_source: "machine" }),
    "按机器加速度 X 800 / Y 600 mm/s² 估算加减速",
  );
  assert.equal(timeEstimateNote({ estimated_seconds: 3 }), "");
  assert.equal(timeEstimateNote(null), "");
});

test("按速度的扫描偏移表：排序、去掉空行、校验，填了才提交并覆盖固定偏移", () => {
  const rows = [{ speed: "3000", offset: "0.3" }, { speed: "", offset: "" }, { speed: "1000", offset: "0.1" }];
  assert.deepEqual(offsetTablePayload(rows), { value: [[1000, 0.1], [3000, 0.3]] });
  assert.deepEqual(offsetTablePayload([]), { value: null });
  const payload = fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, scanOffsetTable: rows });
  assert.deepEqual(payload.fill_options.scan_offset_table, [[1000, 0.1], [3000, 0.3]]);
  assert.equal(fillSettingsPayload(DEFAULT_FILL_SETTINGS).fill_options.scan_offset_table, undefined, "空表不提交");
  assert.match(offsetTablePayload([{ speed: "1000", offset: "5" }]).error, /偏移/);
  assert.match(offsetTablePayload([{ speed: "", offset: "0.1" }]).error, /速度/);
  assert.match(offsetTablePayload([{ speed: "1000", offset: "0.1" }, { speed: "1000", offset: "0.2" }]).error, /重复/);
  assert.match(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, scanOffsetTable: [{ speed: "x", offset: "0" }] }).error, /速度/);
  // 存储往返、坏数据收成空表，最多 32 行
  assert.deepEqual(normalizeFillSettings({ scanOffsetTable: "bad" }).scanOffsetTable, []);
  assert.equal(normalizeFillSettings({ scanOffsetTable: Array.from({ length: 40 }, () => ({ speed: "1", offset: "0" })) }).scanOffsetTable.length, 32);
  // 改表让填充预览过期，摘要提示按速度表
  const withTable = { ...DEFAULT_FILL_SETTINGS, scanOffsetTable: rows };
  assert.notEqual(fillSettingsSignature(DEFAULT_FILL_SETTINGS), fillSettingsSignature(withTable));
  assert.match(fillSettingsSummary(withTable), /偏移按速度表/);
  assert.equal(
    fillResultNote({ grouping: "all", bidirectional: true, overscan_mm: 0, scan_offset_mm: 0.2, scan_offset_source: "table" }),
    "一次填完，双向，无过扫描，扫描偏移 0.200 mm（按速度表）",
  );
});

test("切割设置：默认全关，外框切割才提交 cut_options，桥位按个数或间距二选一", () => {
  assert.deepEqual(cutOptionsPayload(DEFAULT_FILL_SETTINGS), {
    value: { kerf_mm: 0, lead_in_mm: 0, lead_out_mm: 0, lead_angle_deg: 90, overcut_mm: 0 },
  });
  // 描边 / 雕刻不带切割设置
  assert.equal(fillSettingsPayload(DEFAULT_FILL_SETTINGS, { withFill: false }).cut_options, undefined);
  const cut = fillSettingsPayload(
    { ...DEFAULT_FILL_SETTINGS, kerfMm: "0.1", tabMode: "count", tabCount: "3", tabWidthMm: "0.4", leadInMm: "1", overcutMm: "0.5" },
    { withFill: false, withCut: true },
  );
  assert.deepEqual(cut, {
    path_options: { prefer_corners: true, remove_overlaps: true },
    cut_options: {
      kerf_mm: 0.1, lead_in_mm: 1, lead_out_mm: 0, lead_angle_deg: 90, overcut_mm: 0.5,
      tab_count: 3, tab_width_mm: 0.4, tabs_skip_inner: true,
    },
  });
  const spaced = cutOptionsPayload({ ...DEFAULT_FILL_SETTINGS, tabMode: "spacing", tabSpacingMm: "40", tabsSkipInner: false });
  assert.equal(spaced.value.tab_spacing_mm, 40);
  assert.equal(spaced.value.tab_count, undefined);
  assert.equal(spaced.value.tabs_skip_inner, false);

  assert.match(cutOptionsPayload({ ...DEFAULT_FILL_SETTINGS, kerfMm: "3" }).error, /切缝/);
  assert.match(cutOptionsPayload({ ...DEFAULT_FILL_SETTINGS, tabMode: "count", tabCount: "2.5" }).error, /整数/);
  assert.match(cutOptionsPayload({ ...DEFAULT_FILL_SETTINGS, tabMode: "spacing", tabSpacingMm: "0.5" }).error, /间距/);
  assert.match(cutOptionsPayload({ ...DEFAULT_FILL_SETTINGS, leadInMm: "1", leadAngleDeg: "0" }).error, /角度/);
  assert.equal(cutOptionsPayload({ ...DEFAULT_FILL_SETTINGS, leadAngleDeg: "0" }).error, undefined, "未设引入/引出线时不校验已隐藏的引线角度");
  assert.match(fillSettingsPayload({ ...DEFAULT_FILL_SETTINGS, overcutMm: "20" }, { withFill: false, withCut: true }).error, /过切/);
  assert.equal(normalizeFillSettings({ tabMode: "zigzag" }).tabMode, "off");

  // 切割设置只让外框切割的预览过期
  const changed = { ...DEFAULT_FILL_SETTINGS, kerfMm: "0.1" };
  assert.equal(
    fillSettingsSignature(DEFAULT_FILL_SETTINGS, { withFill: false }),
    fillSettingsSignature(changed, { withFill: false }),
  );
  assert.notEqual(
    fillSettingsSignature(DEFAULT_FILL_SETTINGS, { withFill: false, withCut: true }),
    fillSettingsSignature(changed, { withFill: false, withCut: true }),
  );
  assert.equal(fillSettingsSignature(DEFAULT_FILL_SETTINGS), fillSettingsSignature(changed), "填充预览不看切割设置");
  assert.equal(
    fillSettingsSummary({ ...changed, tabMode: "count", tabCount: "4", leadInMm: "1" }, { withFill: false, withCut: true }),
    "切缝外扩 0.1 mm · 桥位 4 个 · 引入 1 mm",
  );
});

test("切割结果说明：回显的补偿、桥位、引线与后端提示", () => {
  assert.equal(cutResultNote(null), "");
  assert.equal(cutResultNote({ active: false }), "");
  assert.equal(
    cutResultNote({
      active: true, kerf_mm: -0.1,
      report: { tabs: 4, lead_ins: 2, lead_outs: 0, overcut_mm: 1 },
      note: "1 条引线会碰到别的轮廓或落到零件一侧，已省略",
    }),
    "切缝内缩 0.10 mm，桥位 4 个，引线 2 条，过切共 1.0 mm，1 条引线会碰到别的轮廓或落到零件一侧，已省略",
  );
  assert.equal(
    fillResultNote(null, 1, { active: true, kerf_mm: 0.1, report: {} }),
    "切缝外扩 0.10 mm；去掉 1 条重复线",
  );
});

// 2026-11-17（技术书 r45）：工作台图片与画板转图片同样按速度自动过扫描、套用双向扫描偏移表
test("位图扫描子集：只带过扫描 / 双向 / 扫描偏移（含速度表）/ 加速度，不带矢量填充项", () => {
  assert.deepEqual(rasterScanPayload(DEFAULT_FILL_SETTINGS), {
    fill_options: { bidirectional: true, scan_offset_mm: 0, overscan_mm: "auto" },
  });
  const tuned = rasterScanPayload({
    ...DEFAULT_FILL_SETTINGS,
    grouping: "individual",
    angleDeg: "45",
    crossHatch: true,
    bidirectional: true,
    scanOffsetMm: "0.08",
    scanOffsetTable: [{ speed: "3000", offset: "0.3" }, { speed: "1000", offset: "0.1" }],
    accelerationMmS2: "2500",
  });
  assert.deepEqual(tuned, {
    fill_options: {
      bidirectional: true,
      scan_offset_mm: 0.08,
      scan_offset_table: [[1000, 0.1], [3000, 0.3]],
      overscan_mm: "auto",
      acceleration_mm_s2: 2500,
    },
  });
  assert.deepEqual(
    rasterScanPayload({
      ...DEFAULT_FILL_SETTINGS,
      bidirectional: false,
      scanOffsetMm: "0.08",
      scanOffsetTable: [{ speed: "1000", offset: "0.1" }],
      accelerationMmS2: "2500",
    }),
    {
      fill_options: {
        bidirectional: false,
        scan_offset_mm: 0,
        overscan_mm: "auto",
        acceleration_mm_s2: 2500,
      },
    },
  );
  assert.equal(rasterScanPayload({ ...DEFAULT_FILL_SETTINGS, overscanAuto: false, overscanMm: "1.5" }).fill_options.overscan_mm, 1.5);
  assert.match(rasterScanPayload({ ...DEFAULT_FILL_SETTINGS, scanOffsetMm: "3" }).error, /偏移/);
  assert.match(rasterScanPayload({ ...DEFAULT_FILL_SETTINGS, overscanAuto: false, overscanMm: "30" }).error, /过扫描/);
  assert.match(rasterScanPayload({ ...DEFAULT_FILL_SETTINGS, angleDeg: "abc" }).fill_options.overscan_mm, /auto/, "扫描角度不影响位图");

  const base = rasterScanSignature(DEFAULT_FILL_SETTINGS);
  assert.equal(rasterScanSignature({ ...DEFAULT_FILL_SETTINGS, angleDeg: "90", crossHatch: true, kerfMm: "0.2" }), base, "矢量填充与切割项不进位图签名");
  assert.notEqual(rasterScanSignature({ ...DEFAULT_FILL_SETTINGS, bidirectional: false }), base);
  assert.notEqual(rasterScanSignature({ ...DEFAULT_FILL_SETTINGS, scanOffsetTable: [{ speed: "1000", offset: "0.1" }] }), base);
  assert.notEqual(rasterScanSignature({ ...DEFAULT_FILL_SETTINGS, overscanAuto: false }), base);
});
