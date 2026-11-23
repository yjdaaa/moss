import test from "node:test";
import assert from "node:assert/strict";
import {
  resolveMaterialLabSelection,
  resolveCellEditorValues,
  resolveMatrixModeParams,
  matrixInputSignature,
  isMatrixStale,
  buildMatrixSendRequest,
  buildMatrixConfirmPrompt,
  unwrapImportedMaterialLibrary,
  buildMatrixPreviewPayload,
  axisOptionsForMode,
  validateMatrixAxes,
  cellAxisText,
  isSurfaceKey,
  thicknessChipLabel,
  matrixPollOutcome,
  matrixSendOutcome,
  powerScaleToPercent,
  percentToPowerScale,
  formatPowerScaleHint,
} from "../src/materialLabState.js";
import {
  MATERIAL_LIBRARY_CHANNEL,
  notifyMaterialsUpdated,
} from "../src/materialSelectState.js";

const SAMPLE_MATERIALS = {
  亚克力: {
    aliases: ["acrylic"],
    thicknesses: {
      "3": {
        engrave: {
          raster: {
            laser_min_power: 0,
            laser_max_power: 340,
            feed_rate: 1600,
            travel_rate: 3000,
            threshold: -1,
            passes: 1,
            pixel_size_mm: 0.1,
            notes: "覆膜雕刻",
          },
        },
      },
    },
  },
  胡桃木: {
    aliases: ["walnut", "黑胡桃"],
    thicknesses: {
      "2": { cut: { laser_min_power: 0, laser_max_power: 700, feed_rate: 420, travel_rate: 3000, threshold: 128, passes: 2, pixel_size_mm: 0.1 } },
      "5": { cut: { laser_min_power: 10, laser_max_power: 920, feed_rate: 240, travel_rate: 3000, threshold: 128, passes: 3, pixel_size_mm: 0.1, notes: "深切三遍" } },
    },
  },
};

test("resolveMaterialLabSelection 保持当前编辑的材料与厚度，不跳回第一项", () => {
  const kept = resolveMaterialLabSelection(SAMPLE_MATERIALS, "胡桃木", "5");
  assert.deepEqual(kept, { material: "胡桃木", thickness: "5", empty: false });

  const fallbackThick = resolveMaterialLabSelection(SAMPLE_MATERIALS, "胡桃木", "8");
  assert.deepEqual(fallbackThick, { material: "胡桃木", thickness: "2", empty: false });

  const fallbackMat = resolveMaterialLabSelection(SAMPLE_MATERIALS, "不存在的材料", "3");
  assert.deepEqual(fallbackMat, { material: "亚克力", thickness: "3", empty: false });

  const empty = resolveMaterialLabSelection({}, "胡桃木", "5");
  assert.deepEqual(empty, { material: "", thickness: "", empty: true });
});

test("resolveCellEditorValues 切到未保存模式或空库时不残留上一项脏参数", () => {
  const walnutCut = resolveCellEditorValues(SAMPLE_MATERIALS, "胡桃木", "5", "cut");
  assert.equal(walnutCut.hasSavedCell, true);
  assert.equal(walnutCut.maxPower, 920);
  assert.equal(walnutCut.passes, 3);
  assert.equal(walnutCut.notes, "深切三遍");
  assert.equal(walnutCut.aliases, "walnut, 黑胡桃");

  // 亚克力只有 raster，没有 cut：切到 cut 必须重置干净初始值并清空备注，但保留亚克力自己的别名
  const acrylicCut = resolveCellEditorValues(SAMPLE_MATERIALS, "亚克力", "3", "cut");
  assert.equal(acrylicCut.hasSavedCell, false);
  assert.equal(acrylicCut.notes, "");
  assert.equal(acrylicCut.aliases, "acrylic");
  assert.notEqual(acrylicCut.maxPower, 920);

  // 空库完全重置，不串别名
  const emptyVals = resolveCellEditorValues({}, "", "", "raster");
  assert.equal(emptyVals.hasSavedCell, false);
  assert.equal(emptyVals.aliases, "");
  assert.equal(emptyVals.notes, "");
});

test("标定矩阵模式映射、过期签名、串口/网络发送载荷与开刻确认提示", () => {
  assert.deepEqual(resolveMatrixModeParams("cut"), {
    mode: "cut",
    label: "边缘切割",
    laser_mode: "cut",
    engraving_mode: "",
    matrix_kind: "cut",
  });
  assert.deepEqual(resolveMatrixModeParams("outline"), {
    mode: "outline",
    label: "轮廓扫描",
    laser_mode: "engrave",
    engraving_mode: "outline",
    matrix_kind: "engrave",
  });

  const sig1 = matrixInputSignature({ material: "椴木", thickness: "3", mode: "cut", rows: 4, cols: 4 });
  const sig2 = matrixInputSignature({ material: "椴木", thickness: "5", mode: "cut", rows: 4, cols: 4 });
  assert.equal(isMatrixStale(sig1, sig1), false);
  assert.equal(isMatrixStale(sig1, sig2), true);

  assert.equal(buildMatrixSendRequest({ calibrationId: "", networkHost: "192.168.1.5" }).error, "请先生成测试矩阵");
  assert.equal(buildMatrixSendRequest({ calibrationId: "cal-1", connectionMode: "serial", serialPort: "" }).error, "请先选择或填写 USB 串口号");

  const serialBuilt = buildMatrixSendRequest({
    calibrationId: "cal-1",
    connectionMode: "serial",
    serialPort: "COM5",
    baudrate: 115200,
  });
  assert.deepEqual(serialBuilt.payload, {
    calibration_id: "cal-1",
    confirmed: true,
    connection_mode: "serial",
    port: "COM5",
    baudrate: 115200,
  });

  const netBuilt = buildMatrixSendRequest({
    calibrationId: "cal-2",
    connectionMode: "network",
    networkHost: "192.168.1.88",
    telnetPort: 2323,
  });
  assert.equal(netBuilt.payload.network_telnet_port, 2323);
  assert.equal(netBuilt.targetLabel, "192.168.1.88:2323");

  const prompt = buildMatrixConfirmPrompt({
    preview: { material: "椴木", thickness_mm: 3, width_mm: 60, height_mm: 60, cell_count: 16, power_min: 200, power_max: 800, speed_min: 300, speed_max: 1200 },
    mode: "cut",
    targetLabel: netBuilt.targetLabel,
  });
  assert.match(prompt.message, /192\.168\.1\.88:2323/);
  assert.match(prompt.message, /60\.0 × 60\.0 mm/);
  assert.match(prompt.message, /边缘切割/);
});

test("材料测试载荷：默认功率×速度不变，轴参数、线距测试与表面条目按 LightBurn 对齐", () => {
  // 默认：参数网格、列 = 功率、行 = 速度，旧字段原样保留
  const plain = buildMatrixPreviewPayload({ material: "椴木", thickness: "3", mode: "raster", rows: 4, cols: 4,
    powerMin: 100, powerMax: 500, speedMin: 600, speedMax: 2400 });
  assert.deepEqual(plain.payload, {
    material: "椴木", thickness_mm: 3, laser_mode: "engrave", engraving_mode: "raster", matrix_kind: "engrave",
    labels: true, rows: 4, columns: 4, power_min: 100, power_max: 500, speed_min: 600, speed_max: 2400,
    x_axis: "power", y_axis: "speed",
  });

  // 线距 × 遍数：只在用到时带对应区间
  const axes = buildMatrixPreviewPayload({ material: "椴木", thickness: "3", mode: "raster", xAxis: "interval",
    yAxis: "passes", intervalMin: 0.06, intervalMax: 0.12, passesMin: 1, passesMax: 3, labels: false });
  assert.equal(axes.payload.interval_min_mm, 0.06);
  assert.equal(axes.payload.passes_max, 3);
  assert.equal(axes.payload.labels, false);
  assert.equal("passes_min" in plain.payload, false);

  // 两轴相同、线距用于切割都在前端就拦下
  assert.match(buildMatrixPreviewPayload({ material: "椴木", xAxis: "speed", yAxis: "speed" }).error, /不同/);
  assert.match(buildMatrixPreviewPayload({ material: "椴木", mode: "cut", xAxis: "interval" }).error, /线距/);
  assert.deepEqual(axisOptionsForMode("cut").map((a) => a.id), ["power", "speed", "passes"]);
  assert.equal(validateMatrixAxes({ xAxis: "passes", yAxis: "power", mode: "outline" }).xAxis, "passes");

  // 线距测试：固定 1 行，方块数走 columns
  const interval = buildMatrixPreviewPayload({ material: "椴木", thickness: "3", mode: "raster", testType: "interval",
    intervalMin: 0.08, intervalMax: 0.16, intervalCount: 9 });
  assert.equal(interval.payload.test_type, "interval");
  assert.equal(interval.payload.rows, 1);
  assert.equal(interval.payload.columns, 9);
  assert.equal("x_axis" in interval.payload, false);
  assert.match(buildMatrixPreviewPayload({ material: "椴木", mode: "outline", testType: "interval" }).error, /光栅/);
  assert.match(buildMatrixPreviewPayload({ material: "椴木", testType: "interval", intervalMin: 0.2, intervalMax: 0.1 }).error, /线距区间/);

  // 表面通用条目：带 surface=true，厚度用材料已有的数字厚度兜底；切割不允许
  const surface = buildMatrixPreviewPayload({ material: "椴木", thickness: "surface", fallbackThickness: 5 });
  assert.equal(surface.payload.surface, true);
  assert.equal(surface.payload.thickness_mm, 5);
  assert.ok(buildMatrixPreviewPayload({ material: "椴木", thickness: "surface", mode: "cut" }).error);
  assert.equal(thicknessChipLabel("surface"), "表面通用");
  assert.equal(thicknessChipLabel("3"), "3 mm");
  assert.equal(isSurfaceKey(" surface "), true);

  // 轴参数变化让旧矩阵过期
  const sigA = matrixInputSignature({ material: "椴木", thickness: "3" });
  const sigB = matrixInputSignature({ material: "椴木", thickness: "3", xAxis: "interval" });
  assert.equal(isMatrixStale(sigA, sigB), true);
  assert.equal(isMatrixStale(sigA, matrixInputSignature({ material: "椴木", thickness: "3" })), false);

  // 单元格显示与后端刻在样板上的标注一致
  assert.equal(cellAxisText({ x_label: "40" }, "power", "x"), "40%");
  assert.equal(cellAxisText({ y_label: "0.10" }, "interval", "y"), "0.10");
  assert.equal(cellAxisText({ speed: 1200 }, "speed", "x"), "F1200");
  assert.equal(cellAxisText({ passes: 2 }, "passes", "y"), "2遍");

  // 开刻确认写明轴参数、加工顺序与标注参数
  const prompt = buildMatrixConfirmPrompt({
    preview: { material: "椴木", thickness_mm: 3, x_axis: "interval", y_axis: "passes", run_order: "risk",
      labels: { enabled: true, power: 200, speed: 3000 }, cell_count: 9 },
    mode: "raster",
    targetLabel: "<device-ip>:23",
  });
  assert.match(prompt.message, /列 = 线距，行 = 遍数/);
  assert.match(prompt.message, /烧蚀风险最低/);
  assert.match(prompt.message, /S200 · 速度 F3000/);
  const intervalPrompt = buildMatrixConfirmPrompt({
    preview: { test_type: "interval", interval_min_mm: 0.08, interval_max_mm: 0.16, surface_entry: true },
    mode: "raster",
  });
  assert.match(intervalPrompt.message, /0\.08 ~ 0\.16 mm/);
  assert.match(intervalPrompt.message, /表面通用/);
});

test("条目标题随参数编辑器读取，未保存时为空", () => {
  const withTitle = resolveCellEditorValues(
    { 椴木: { thicknesses: { surface: { engrave: { raster: { laser_max_power: 260, feed_rate: 2400, title: "浅色照片" } } } } } },
    "椴木",
    "surface",
    "raster",
  );
  assert.equal(withTitle.title, "浅色照片");
  assert.equal(withTitle.hasSavedCell, true);
  assert.equal(resolveCellEditorValues({}, "", "", "raster").title, "");
});

test("导入 JSON 解包与 BroadcastChannel 跨页同步通知", () => {
  const wrapped = unwrapImportedMaterialLibrary({
    params_file: "materials.json",
    library: { version: 1, materials: { 椴木: {}, 亚克力: {} } },
  });
  assert.equal(wrapped.materialCount, 2);
  assert.equal(wrapped.library.version, 1);

  assert.ok(unwrapImportedMaterialLibrary({ version: "1" }).error);

  const posted = [];
  let closed = false;
  class FakeChannel {
    constructor(name) {
      this.name = name;
    }
    postMessage(msg) {
      posted.push({ channel: this.name, msg });
    }
    close() {
      closed = true;
    }
  }
  assert.equal(notifyMaterialsUpdated(FakeChannel), true);
  assert.equal(posted.length, 1);
  assert.equal(posted[0].channel, MATERIAL_LIBRARY_CHANNEL);
  assert.equal(posted[0].msg.type, "materials-updated");
  assert.equal(closed, true);
});

test("标定矩阵发送结果：审批、开始下发、结果未知、拒绝四种转移", () => {
  const built = buildMatrixSendRequest({ calibrationId: "cal-1", connectionMode: "serial", serialPort: "COM3", baudrate: "115200" });

  const approval = matrixSendOutcome({ ok: true, status: 202, data: { success: true, result: { status: "approval_required" } } }, built);
  assert.equal(approval.kind, "approval");
  assert.equal(approval.sendState, null);
  assert.equal(approval.poll, false);

  const sending = matrixSendOutcome(
    { ok: true, status: 200, data: { success: true, result: { send_result: { running: true, total_lines: 40, target: "COM3@115200" } } } },
    built,
  );
  assert.equal(sending.kind, "sending");
  assert.deepEqual(sending.sendState, { status: "sending", lines_sent: 0, total_lines: 40, target: "COM3@115200" });
  assert.equal(sending.poll, true);

  // 网络中断：后端可能已开始加工，必须继续按“发送中”轮询并保留规范目标给急停，不能报失败
  const unknown = matrixSendOutcome({ ok: false, status: 0, data: null }, built);
  assert.equal(unknown.kind, "unknown");
  assert.equal(unknown.sendState.status, "sending");
  assert.equal(unknown.sendState.connectionLost, true);
  assert.equal(unknown.sendState.target, "COM3@115200");
  assert.equal(unknown.poll, true);
  assert.match(unknown.status, /请勿重复发送/);

  const failed = matrixSendOutcome({ ok: false, status: 409, data: { success: false, result: "SHA 不匹配" } }, built);
  assert.equal(failed.kind, "failed");
  assert.equal(failed.sendState, null);
  assert.equal(failed.poll, false);
  assert.equal(failed.status, "发送失败: SHA 不匹配");
});

test("标定矩阵轮询：进度合并、查询失败不算结束、终态停止轮询", () => {
  const previous = { status: "sending", lines_sent: 0, total_lines: 40, target: "COM3@115200", connectionLost: true };
  const lost = matrixPollOutcome(previous, { status: "sending", connectionLost: true }, { ok: false });
  assert.equal(lost.sendState, previous);
  assert.equal(lost.terminal, false);
  assert.match(lost.status, /状态未知/);

  const progress = matrixPollOutcome(
    previous,
    { status: "sending", connectionLost: false, progress: { sent: 10, total: 40 } },
    { ok: true, data: { success: true, result: { send_result: {} } } },
  );
  assert.equal(progress.terminal, false);
  assert.equal(progress.sendState.lines_sent, 10);
  assert.equal(progress.sendState.target, "COM3@115200");
  assert.equal(progress.status, "正在逐行下发 (10/40 行)…");

  const done = matrixPollOutcome(
    progress.sendState,
    { status: "completed", connectionLost: false, progress: { sent: 40, total: 40 } },
    { ok: true, data: { success: true, result: { send_result: { lines_sent: 40, total_lines: 40 } } } },
  );
  assert.equal(done.terminal, true);
  assert.equal(done.sendState.status, "completed");
  assert.match(done.status, /加工完成 \(40\/40 行已确认\)/);

  const cancelled = matrixPollOutcome(
    progress.sendState,
    { status: "cancelled", connectionLost: false },
    { ok: true, data: { success: true, result: { send_result: { error: "用户急停" } } } },
  );
  assert.equal(cancelled.terminal, true);
  assert.equal(cancelled.status, "标定矩阵记录为已取消，请确认设备状态: 用户急停");
});

test("B2 实验室功率 0–100% 与后端 S 值 (0–1000) 换算、S 值提示及全范围往返零漂移", () => {
  // 典型边界：0、42%、100% 与非整数百分比
  assert.equal(powerScaleToPercent(0), 0);
  assert.equal(percentToPowerScale(0), 0);
  assert.equal(powerScaleToPercent(420), 42);
  assert.equal(percentToPowerScale(42), 420);
  assert.equal(powerScaleToPercent(1000), 100);
  assert.equal(percentToPowerScale(100), 1000);

  // 一位小数精确对应整数 S 值
  assert.equal(percentToPowerScale("42.5"), 425);
  assert.equal(powerScaleToPercent(425), 42.5);
  assert.equal(percentToPowerScale(0.1), 1);
  assert.equal(powerScaleToPercent(1), 0.1);
  assert.equal(percentToPowerScale(99.9), 999);
  assert.equal(powerScaleToPercent(999), 99.9);

  // 两位小数（如 35.85% / 33.33%）四舍五入到最近整数 S 值后，再次往返不再发生二次漂移
  const sFrom3585 = percentToPowerScale("35.85");
  assert.equal(sFrom3585, 359);
  assert.equal(powerScaleToPercent(sFrom3585), 35.9);
  assert.equal(percentToPowerScale(powerScaleToPercent(sFrom3585)), sFrom3585);

  const sFrom3333 = percentToPowerScale(33.33);
  assert.equal(sFrom3333, 333);
  assert.equal(powerScaleToPercent(sFrom3333), 33.3);
  assert.equal(percentToPowerScale(powerScaleToPercent(sFrom3333)), sFrom3333);

  // 遍历全部 0..=1000 整数 S 值：S -> % -> S 往返 100% 无损不漂移
  for (let s = 0; s <= 1000; s += 1) {
    const pct = powerScaleToPercent(s);
    assert.equal(percentToPowerScale(pct), s, `S=${s} 经 ${pct}% 往返后不应漂移`);
  }

  // 空串与非法输入返回 NaN；越界输入（如把 45% 误填为 450）不静默钳成 100%，交由校验拦截
  assert.equal(Number.isNaN(percentToPowerScale("")), true);
  assert.equal(Number.isNaN(percentToPowerScale(null)), true);
  assert.equal(percentToPowerScale("450"), 4500);
  assert.match(
    buildMatrixPreviewPayload({ material: "椴木", thickness: "3", powerMin: 100, powerMax: 4500 }).error,
    /0 ~ 100%/,
  );
  assert.match(
    buildMatrixPreviewPayload({ material: "椴木", thickness: "3", powerMin: 700, powerMax: 300 }).error,
    /下限不大于上限/,
  );

  assert.equal(formatPowerScaleHint(420, { withScale: true }), "对应 S420 / 1000");
  assert.equal(formatPowerScaleHint(0, { withScale: false }), "对应 S0");
  assert.equal(formatPowerScaleHint(NaN, { withScale: true }), "对应 S— / 1000");
});
