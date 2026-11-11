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
