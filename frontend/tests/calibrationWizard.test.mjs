import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import {
  WIZARD_STEPS,
  buildPatternPreviewPayload,
  currentWizardStep,
  kerfFromMeasurement,
  markWizardStep,
  mergeScanOffsetTable,
  normalizeWizardState,
  overscanSuggestion,
  patternSaveTarget,
  readWizardState,
  scanOffsetRowsFromMeasurements,
  stepRecord,
  timeCalibrationSummary,
  wizardMaterialKey,
  wizardProgress,
  writeWizardState,
} from "../src/calibrationWizard.js";
import { wizardPatternLine } from "../src/materialLabState.js";

function memoryStorage() {
  const map = new Map();
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(k, String(v)),
  };
}

test("标定向导：六步按顺序，材料步骤按材料|厚度分开，机器步骤全局共用，可跳过与重做", () => {
  assert.deepEqual(WIZARD_STEPS.map((s) => s.id), ["interval", "matrix", "scan_offset", "overscan", "kerf", "time"]);
  const basswood = wizardMaterialKey("椴木", "3");
  const acrylic = wizardMaterialKey("亚克力", "3");
  let state = normalizeWizardState(null);
  assert.equal(currentWizardStep(state, basswood), "interval");

  state = markWizardStep(state, "interval", "done", { materialKey: basswood, note: "线距 0.1 mm", at: 1 });
  state = markWizardStep(state, "matrix", "skipped", { materialKey: basswood, at: 2 });
  assert.equal(currentWizardStep(state, basswood), "scan_offset");
  assert.equal(stepRecord(state, "interval", basswood).note, "线距 0.1 mm");
  // 别的材料从头开始
  assert.equal(currentWizardStep(state, acrylic), "interval");

  state = markWizardStep(state, "scan_offset", "done", { materialKey: basswood, at: 3 });
  // 机器步骤对所有材料生效
  assert.equal(stepRecord(state, "scan_offset", acrylic).status, "done");
  assert.deepEqual(wizardProgress(state, basswood), { done: 2, skipped: 1, total: 6 });

  // 重做：清除记录
  state = markWizardStep(state, "matrix", null, { materialKey: basswood });
  assert.equal(currentWizardStep(state, basswood), "matrix");
  // 未知步骤与坏数据不影响状态
  assert.deepEqual(markWizardStep(state, "laser_on", "done"), normalizeWizardState(state));
  const dirty = normalizeWizardState({ machine: { time: { status: "maybe" } }, materials: { x: { kerf: { status: "done", note: 5 } } } });
  assert.equal(dirty.machine.time, undefined);
  assert.equal(dirty.materials.x.kerf.note, "");

  const storage = memoryStorage();
  writeWizardState(state, storage);
  assert.deepEqual(readWizardState(storage), normalizeWizardState(state));
  assert.deepEqual(readWizardState({ getItem: () => "{bad" }), normalizeWizardState(null));
});

test("切缝补偿：切缝宽度 = 设计 − 实测，补偿 = 一半；超范围报错", () => {
  assert.deepEqual(kerfFromMeasurement(20, "19.8"), { kerfMm: 0.1, widthMm: 0.2 });
  assert.deepEqual(kerfFromMeasurement(20, 20), { kerfMm: 0, widthMm: 0 });
  assert.equal(kerfFromMeasurement(20, "20.1").kerfMm, -0.05);
  assert.match(kerfFromMeasurement(20, "").error, /量得/);
  assert.match(kerfFromMeasurement(20, "10").error, /±2/);
});

test("双向扫描偏移：按速度生成偏移表行，并进本机表时同速度替换、按速度排序", () => {
  const parsed = scanOffsetRowsFromMeasurements([
    { speed: 3000, measured: "0.15" },
    { speed: 1000, measured: "0.05" },
    { speed: 2000, measured: "" },
  ]);
  assert.deepEqual(parsed.rows, [
    { speed: "3000", offset: "0.15" },
    { speed: "1000", offset: "0.05" },
  ]);
  assert.match(scanOffsetRowsFromMeasurements([{ speed: 1000, measured: "" }]).error, /至少/);
  assert.match(scanOffsetRowsFromMeasurements([{ speed: 1000, measured: "3" }]).error, /-2 ~ 2/);
  const merged = mergeScanOffsetTable([{ speed: "1000", offset: "0.2" }, { speed: "5000", offset: "0.3" }], parsed.rows);
  assert.deepEqual(merged, [
    { speed: "1000", offset: "0.05" },
    { speed: "3000", offset: "0.15" },
    { speed: "5000", offset: "0.3" },
  ]);
});

test("过扫描建议、工时校准说明与测试图案请求体", () => {
  assert.equal(overscanSuggestion(2).nextMm, 3);
  assert.equal(overscanSuggestion(4).nextMm, 6);
  assert.equal(overscanSuggestion(18).nextMm, 20);
  assert.match(overscanSuggestion(2, { auto: true }).text, /\$120/);
  assert.doesNotMatch(overscanSuggestion(2, { auto: false }).text, /\$120/);

  assert.match(timeCalibrationSummary(null), /满 2 次/);
  const summary = timeCalibrationSummary({
    min_samples: 2,
    targets: [
      { target: "m1", samples: 3, factor: 1.234, active: true },
      { target: "m2", samples: 1, factor: null, active: false },
    ],
  });
  assert.match(summary, /已按最近 3 次实际用时校准（实际约为原估算的 1\.23 倍）/);
  assert.match(summary, /m2：已记录 1 次，再完成 1 次/);

  const scan = buildPatternPreviewPayload({
    testType: "scan_offset",
    material: "椴木",
    thickness: "3",
    speeds: ["1000", "2000"],
    motion: { machine_motion: { x_accel_mm_s2: 800 } },
  });
  assert.deepEqual(scan.payload, {
    material: "椴木",
    thickness_mm: 3,
    test_type: "scan_offset",
    speeds: [1000, 2000],
    machine_motion: { x_accel_mm_s2: 800 },
  });
  const over = buildPatternPreviewPayload({ testType: "overscan", material: "椴木", thickness: 3, fillOptions: { overscan_mm: "auto" } });
  assert.deepEqual(over.payload.fill_options, { overscan_mm: "auto" });
  assert.match(buildPatternPreviewPayload({ testType: "kerf", material: "椴木", thickness: "surface" }).error, /具体厚度/);
  assert.match(buildPatternPreviewPayload({ testType: "laser", material: "椴木", thickness: 3 }).error, /未知/);
});

test("确认框里说明向导测试图案（切缝会切穿材料）", () => {
  assert.equal(wizardPatternLine({ test_type: "interval" }), "");
  assert.match(
    wizardPatternLine({ wizard_pattern: true, test_type: "scan_offset", cells: [{ speed: 1000 }, { speed: 2000 }] }),
    /2 个实心方块，速度 1000 \/ 2000/,
  );
  assert.match(
    wizardPatternLine({ wizard_pattern: true, test_type: "overscan", cells: [{ overscan_mm: 0 }, { overscan_mm: 2.4 }] }),
    /右块过扫描 2\.4 mm/,
  );
  assert.match(
    wizardPatternLine({ wizard_pattern: true, test_type: "kerf", square_mm: 20, cells: [{ power: 900, speed: 300, passes: 2 }] }),
    /S900 · F300 × 2 遍切一个 20 mm 方块.*切穿/,
  );
});

test("标定向导面板：列出六步、当前步骤展开、只读时不给操作按钮", async () => {
  const server = await createServer({
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    server: { middlewareMode: true },
  });
  try {
    const { CalibrationWizard } = await server.ssrLoadModule("/src/components/CalibrationWizard.jsx");
    const render = (props) =>
      renderToStaticMarkup(
        React.createElement(CalibrationWizard, {
          wizard: normalizeWizardState(null),
          onWizardChange: () => {},
          material: "椴木",
          thickness: "3",
          onPrepareMatrix: () => {},
          onGeneratePattern: async () => null,
          onSend: () => {},
          ...props,
        }),
      );
    const html = render({});
    assert.match(html, /首次标定向导/);
    assert.match(html, /椴木 · 3mm · 已完成 0 \/ 6/);
    for (const title of ["线距测试", "功率 × 速度矩阵", "双向扫描偏移", "过扫描检查", "切缝补偿", "工时校准"]) {
      assert.match(html, new RegExp(title));
    }
    assert.match(html, /设置线距测试/);
    assert.match(html, /跳过这一步/);
    assert.doesNotMatch(html, /发送测试/, "没有生成测试图案时不出现发送按钮");

    // 当前步骤是切缝且已生成切缝图案：出现发送（先确认）与测量输入
    let state = normalizeWizardState(null);
    const key = wizardMaterialKey("椴木", "3");
    for (const id of ["interval", "matrix", "scan_offset", "overscan"]) {
      state = markWizardStep(state, id, "skipped", { materialKey: key });
    }
    const kerf = render({
      wizard: state,
      patternPreview: { wizard_pattern: true, test_type: "kerf", square_mm: 20, material: "椴木", thickness_mm: 3, cells: [] },
    });
    assert.match(kerf, /发送测试（先确认）/);
    assert.match(kerf, /量得的方块边长/);
    assert.match(kerf, /\(20 − 实测\) ÷ 2/);

    const readOnly = render({ readOnly: true });
    assert.match(readOnly, /只能查看/);
    assert.doesNotMatch(readOnly, /跳过这一步/);
  } finally {
    await server.close();
  }
});

test("向导结果写回测试参数所在的材料库条目（r43）", () => {
  assert.deepEqual(patternSaveTarget({ thickness_mm: 3, matched_thickness_mm: 3, surface_entry: false }, "3"), {
    request: { thickness_mm: 3 },
    note: "",
  });
  const nearest = patternSaveTarget({ thickness_mm: 3.2, matched_thickness_mm: 3, surface_entry: false }, "3.2");
  assert.deepEqual(nearest.request, { thickness_mm: 3 });
  assert.match(nearest.note, /3mm 条目/);
  const surface = patternSaveTarget({ thickness_mm: 3, matched_thickness_mm: 3, surface_entry: true }, "3");
  assert.deepEqual(surface.request, { surface: true });
  assert.match(surface.note, /表面通用/);
  // 旧记录没有来源字段：按请求厚度
  assert.deepEqual(patternSaveTarget({ thickness_mm: 5 }, "5").request, { thickness_mm: 5 });
  assert.deepEqual(patternSaveTarget(null, "4").request, { thickness_mm: 4 });
});
