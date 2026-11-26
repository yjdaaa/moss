/**
 * 材料实验室（MaterialLabPage）纯状态与校验逻辑。
 * 不依赖 React，供页面与单元测试共用。
 */

import { approvalPendingMessage, isApprovalRequiredResponse } from "./approvalState.js";
import { thicknessesEqual } from "./materialSelectState.js";

export const MATRIX_MODE_OPTIONS = [
  { id: "raster", label: "光栅雕刻", code: "Raster" },
  { id: "outline", label: "轮廓扫描", code: "Outline" },
  { id: "cut", label: "边缘切割", code: "Cut" },
];

/**
 * 后端 0–1000 功率值（S 值）换算为前端显示的 0–100% 数值（最多一位小数）。
 * 例：0 -> 0，420 -> 42，425 -> 42.5，1000 -> 100。
 */
export function powerScaleToPercent(sValue) {
  const num = Number(sValue);
  if (!Number.isFinite(num)) return 0;
  return Number((Math.round(num) / 10).toFixed(1));
}

/**
 * 前端输入的 0–100% 功率值换算为后端存储与接口使用的 0–1000 整数 S 值。
 * 先消去 IEEE-754 乘 10 尾差再四舍五入，确保与 powerScaleToPercent 往返不漂移；
 * 不在此处静默钳制越界值，交由表单校验拦截误填（如把 45% 误输成 450）。
 */
export function percentToPowerScale(percentValue) {
  if (percentValue === "" || percentValue === null || percentValue === undefined || typeof percentValue === "boolean") {
    return NaN;
  }
  const trimmed = typeof percentValue === "string" ? percentValue.trim() : percentValue;
  if (trimmed === "") return NaN;
  const num = Number(trimmed);
  if (!Number.isFinite(num)) return NaN;
  return Math.round(Number((num * 10).toPrecision(12)));
}

/** 输入框旁小字提示对应的 GRBL S 值（0–1000）。 */
export function formatPowerScaleHint(sValue, { withScale = false } = {}) {
  const num = Number(sValue);
  if (sValue === "" || sValue === null || sValue === undefined || !Number.isFinite(num)) {
    return withScale ? "对应 S— / 1000" : "对应 S—";
  }
  const rounded = Math.round(num);
  return withScale ? `对应 S${rounded} / 1000` : `对应 S${rounded}`;
}

/** 默认未保存单元格的干净初始值，避免切材料或切模式时残留上一项脏数据。 */
export function defaultCellValues(mode = "raster") {
  if (mode === "cut") {
    return {
      minPower: 0,
      maxPower: 750,
      feedRate: 360,
      travelRate: 3000,
      passes: 2,
      pixelSize: 0.1,
      threshold: 128,
      notes: "",
      title: "",
    };
  }
  if (mode === "outline") {
    return {
      minPower: 0,
      maxPower: 400,
      feedRate: 1500,
      travelRate: 3000,
      passes: 1,
      pixelSize: 0.1,
      threshold: -1,
      notes: "",
      title: "",
    };
  }
  return {
    minPower: 0,
    maxPower: 400,
    feedRate: 1500,
    travelRate: 3000,
    passes: 1,
    pixelSize: 0.1,
    threshold: -1,
    notes: "",
    title: "",
  };
}

/**
 * 材料库刷新后决定选中的材料与厚度：
 * - 优先保留当前材料（及数值匹配的厚度）；
 * - 当前材料已不在库中时回退到第 1 个材料的第 1 档厚度；
 * - 整个材料库为空时返回空串并标 `empty: true`。
 */
export function resolveMaterialLabSelection(materialsMap, preferredMaterial, preferredThickness) {
  const map = materialsMap && typeof materialsMap === "object" ? materialsMap : {};
  const names = Object.keys(map);
  if (names.length === 0) {
    return { material: "", thickness: "", empty: true };
  }

  const preferredName = String(preferredMaterial ?? "").trim();
  const material = preferredName && Object.prototype.hasOwnProperty.call(map, preferredName)
    ? preferredName
    : names[0];

  const thickKeys = Object.keys(map[material]?.thicknesses || {});
  if (thickKeys.length === 0) {
    return { material, thickness: String(preferredThickness || "3"), empty: false };
  }

  const exactHit = thickKeys.find((k) => k === String(preferredThickness ?? ""));
  const numericHit = exactHit ?? thickKeys.find((k) => thicknessesEqual(k, preferredThickness));
  return {
    material,
    thickness: numericHit ?? thickKeys[0],
    empty: false,
  };
}

/**
 * 提取指定材料、厚度、模式下的参数编辑器数值。
 * 当该模式尚未保存参数或库为空时，返回干净默认值并置 `hasSavedCell: false`。
 */
export function resolveCellEditorValues(materialsMap, materialName, thicknessKey, mode = "raster") {
  const normalizedMode = mode === "cut" ? "cut" : mode === "outline" ? "outline" : "raster";
  const defaults = defaultCellValues(normalizedMode);
  const map = materialsMap && typeof materialsMap === "object" ? materialsMap : {};
  const mat = materialName ? map[materialName] : null;
  if (!mat) {
    return {
      ...defaults,
      aliases: "",
      hasSavedCell: false,
    };
  }

  const aliases = Array.isArray(mat.aliases) ? mat.aliases.join(", ") : "";
  const thicknesses = mat.thicknesses || {};
  const matchedKey = Object.prototype.hasOwnProperty.call(thicknesses, String(thicknessKey))
    ? String(thicknessKey)
    : Object.keys(thicknesses).find((k) => thicknessesEqual(k, thicknessKey));
  const thickObj = matchedKey ? thicknesses[matchedKey] : null;
  const cell = !thickObj
    ? null
    : normalizedMode === "cut"
      ? thickObj.cut
      : thickObj.engrave?.[normalizedMode];

  if (!cell) {
    return {
      ...defaults,
      aliases,
      hasSavedCell: false,
    };
  }

  return {
    minPower: cell.laser_min_power ?? defaults.minPower,
    maxPower: cell.laser_max_power ?? defaults.maxPower,
    feedRate: cell.feed_rate ?? defaults.feedRate,
    travelRate: cell.travel_rate ?? defaults.travelRate,
    passes: cell.passes ?? defaults.passes,
    pixelSize: cell.pixel_size_mm ?? defaults.pixelSize,
    threshold: cell.threshold ?? defaults.threshold,
    notes: cell.notes ?? "",
    title: typeof cell.title === "string" ? cell.title : "",
    aliases,
    hasSavedCell: true,
  };
}

/**
 * 判断材料参数面板当前编辑值是否相对于材料库已存值（或未存模式的初始默认值）有未保存修改。
 */
export function isCellEditorDirty(materialsMap, materialName, thicknessKey, mode = "raster", current = {}) {
  if (!materialName) return false;
  const baseline = resolveCellEditorValues(materialsMap, materialName, thicknessKey, mode);
  const normAliases = (raw) =>
    String(raw ?? "")
      .split(/[,，\s]+/)
      .map((s) => s.trim())
      .filter(Boolean)
      .join(", ");
  if (Math.round(Number(current.maxPower) || 0) !== Math.round(Number(baseline.maxPower) || 0)) return true;
  if (Math.round(Number(current.feedRate) || 0) !== Math.round(Number(baseline.feedRate) || 0)) return true;
  if (Math.round(Number(current.passes) || 1) !== Math.round(Number(baseline.passes) || 1)) return true;
  if (mode === "raster") {
    const curPix = Number(current.pixelSize);
    const basePix = Number(baseline.pixelSize);
    if (Number.isFinite(curPix) && Number.isFinite(basePix) && Math.abs(curPix - basePix) > 1e-6) return true;
  }
  if (normAliases(current.aliases) !== normAliases(baseline.aliases)) return true;
  if (String(current.entryTitle ?? "").trim() !== String(baseline.title ?? "").trim()) return true;
  if (String(current.notes ?? "").trim() !== String(baseline.notes ?? "").trim()) return true;
  return false;
}

/**
 * 切换校准矩阵列参数（x）或行参数（y）：若新选的轴与另一轴相同，自动把另一轴互换为原轴参数。
 */
export function swapMatrixAxis(xAxis, yAxis, which, nextAxis) {
  if (which === "x") {
    return {
      xAxis: nextAxis,
      yAxis: nextAxis === yAxis ? (xAxis !== nextAxis ? xAxis : (nextAxis === "power" ? "speed" : "power")) : yAxis,
    };
  }
  return {
    xAxis: nextAxis === xAxis ? (yAxis !== nextAxis ? yAxis : (nextAxis === "speed" ? "power" : "speed")) : xAxis,
    yAxis: nextAxis,
  };
}

/** 将界面激活模式（raster / outline / cut）映射为标定矩阵接口字段。 */
export function resolveMatrixModeParams(mode = "raster") {
  if (mode === "cut") {
    return {
      mode: "cut",
      label: "边缘切割",
      laser_mode: "cut",
      engraving_mode: "",
      matrix_kind: "cut",
    };
  }
  if (mode === "outline") {
    return {
      mode: "outline",
      label: "轮廓扫描",
      laser_mode: "engrave",
      engraving_mode: "outline",
      matrix_kind: "engrave",
    };
  }
  return {
    mode: "raster",
    label: "光栅雕刻",
    laser_mode: "engrave",
    engraving_mode: "raster",
    matrix_kind: "engrave",
  };
}

/** 材料库里不分厚度的“表面通用”条目键（对齐 LightBurn No thickness 条目，只存雕刻参数）。 */
export const SURFACE_THICKNESS_KEY = "surface";

export function isSurfaceKey(key) {
  return String(key ?? "").trim() === SURFACE_THICKNESS_KEY;
}

/** 表面通用条目只存雕刻参数，不显示“边缘切割”模式。 */
export function modeOptionsForThickness(thickness) {
  return isSurfaceKey(thickness) ? MATRIX_MODE_OPTIONS.filter((m) => m.id !== "cut") : MATRIX_MODE_OPTIONS;
}

/** 厚度标签：表面通用条目显示“表面通用”，其余显示“N mm”。 */
export function thicknessChipLabel(key) {
  return isSurfaceKey(key) ? "表面通用" : `${key} mm`;
}

/** 测试类型（2026-11-16 对齐 LightBurn Material Test / Interval Test）。 */
export const TEST_TYPE_OPTIONS = [
  { id: "matrix", label: "参数网格" },
  { id: "interval", label: "线距测试" },
];

/** 网格轴可选参数；线距只对光栅 / 填充雕刻有意义。 */
export const TEST_AXIS_OPTIONS = [
  { id: "power", label: "功率", unit: "%" },
  { id: "speed", label: "速度", unit: "mm/min" },
  { id: "interval", label: "线距", unit: "mm" },
  { id: "passes", label: "遍数", unit: "遍" },
];

export function axisOptionsForMode(mode = "raster") {
  return TEST_AXIS_OPTIONS.filter((axis) => axis.id !== "interval" || mode === "raster");
}

function axisInfo(axis) {
  return TEST_AXIS_OPTIONS.find((item) => item.id === axis) || TEST_AXIS_OPTIONS[0];
}

/** 单元格在某个轴上的显示值（后端给了 x_label / y_label 时优先用它们，与刻在样板上的一致）。 */
export function cellAxisText(cell, axis, which) {
  const fromBackend = which === "x" ? cell?.x_label : which === "y" ? cell?.y_label : undefined;
  const info = axisInfo(axis);
  if (fromBackend != null && fromBackend !== "") {
    return info.id === "power" ? `${fromBackend}%` : info.id === "passes" ? `${fromBackend}遍` : String(fromBackend);
  }
  if (info.id === "power") return `${Math.round(Number(cell?.power) || 0) / 10}%`;
  if (info.id === "speed") return `F${cell?.speed ?? "-"}`;
  if (info.id === "interval") {
    const v = Number(cell?.line_interval_mm);
    return Number.isFinite(v) ? v.toFixed(2) : "-";
  }
  return `${cell?.passes ?? 1}遍`;
}

/**
 * 校验并规整两个轴：两轴不能相同；线距轴只能用于光栅模式。
 * 返回 `{ xAxis, yAxis }` 或 `{ error }`。
 */
export function validateMatrixAxes({ xAxis = "power", yAxis = "speed", mode = "raster" } = {}) {
  const allowed = axisOptionsForMode(mode).map((axis) => axis.id);
  if (!allowed.includes(xAxis) || !allowed.includes(yAxis)) {
    return { error: "线距只能用于光栅雕刻的填充测试，请换一个轴参数" };
  }
  if (xAxis === yAxis) {
    return { error: "列和行必须选两个不同的参数" };
  }
  return { xAxis, yAxis };
}

/**
 * 构造 /api/material-lab/preview 请求体。默认（参数网格、列 = 功率、行 = 速度）与改版前完全相同，
 * 只多了 x_axis / y_axis / labels 字段；非默认轴才带对应区间。错误时返回 `{ error }`。
 */
export function buildMatrixPreviewPayload({
  material = "",
  thickness = "",
  fallbackThickness = 3,
  mode = "raster",
  testType = "matrix",
  rows = 5,
  cols = 5,
  powerMin = 100,
  powerMax = 600,
  speedMin = 500,
  speedMax = 2500,
  xAxis = "power",
  yAxis = "speed",
  intervalMin = 0.08,
  intervalMax = 0.16,
  intervalCount = 9,
  passesMin = 1,
  passesMax = 3,
  labels = true,
} = {}) {
  const modeInfo = resolveMatrixModeParams(mode);
  const surface = isSurfaceKey(thickness);
  if (surface && modeInfo.laser_mode === "cut") {
    return { error: "表面通用条目只存雕刻参数，切割标定请选择真实厚度" };
  }
  const thicknessNumber = surface ? Number(fallbackThickness) || 3 : Number(thickness) || 3;
  const payload = {
    material,
    thickness_mm: thicknessNumber,
    laser_mode: modeInfo.laser_mode,
    engraving_mode: modeInfo.engraving_mode,
    matrix_kind: modeInfo.matrix_kind,
    labels: Boolean(labels),
  };
  if (surface) payload.surface = true;
  if (testType === "interval") {
    if (modeInfo.mode !== "raster") {
      return { error: "线距测试只用于光栅雕刻（填充）" };
    }
    const min = Number(intervalMin);
    const max = Number(intervalMax);
    if (!(min > 0) || !(max >= min)) {
      return { error: "线距区间无效：起始值必须大于 0 且不大于结束值" };
    }
    return {
      payload: {
        ...payload,
        test_type: "interval",
        rows: 1,
        columns: Number(intervalCount),
        interval_min_mm: min,
        interval_max_mm: max,
      },
    };
  }
  const axes = validateMatrixAxes({ xAxis, yAxis, mode: modeInfo.mode });
  if (axes.error) return axes;
  const uses = (axis) => axes.xAxis === axis || axes.yAxis === axis;
  const pMin = uses("power") ? Number(powerMin) : 100;
  const pMax = uses("power") ? Number(powerMax) : 600;
  if (uses("power") && (!Number.isFinite(pMin) || !Number.isFinite(pMax) || pMin < 0 || pMax > 1000 || pMin > pMax)) {
    return { error: "功率区间须在 0 ~ 100% 之间，且下限不大于上限" };
  }
  const sMin = uses("speed") ? Number(speedMin) : 500;
  const sMax = uses("speed") ? Number(speedMax) : 2500;
  if (uses("speed") && (!Number.isFinite(sMin) || !Number.isFinite(sMax) || sMin <= 0 || sMin > sMax)) {
    return { error: "速度区间无效：下限必须大于 0 且不大于上限" };
  }
  Object.assign(payload, {
    rows: Number(rows),
    columns: Number(cols),
    power_min: Math.round(pMin),
    power_max: Math.round(pMax),
    speed_min: sMin,
    speed_max: sMax,
    x_axis: axes.xAxis,
    y_axis: axes.yAxis,
  });
  if (uses("interval")) {
    payload.interval_min_mm = Number(intervalMin);
    payload.interval_max_mm = Number(intervalMax);
  }
  if (uses("passes")) {
    payload.passes_min = Number(passesMin);
    payload.passes_max = Number(passesMax);
  }
  return { payload };
}

function strokeLabelWidthMm(text, heightMm) {
  const s = String(text || "");
  if (!s) return 0;
  let units = 0;
  for (let i = 0; i < s.length; i += 1) {
    const ch = s[i];
    if (ch === " ") units += 2.5;
    else if (ch === ".") units += 1.5;
    else if (ch === "(" || ch === ")") units += 2.0;
    else units += 4.0;
    if (i + 1 < s.length) units += 1.5;
  }
  return units * (heightMm / 6.0);
}

/**
 * 按现有单元尺寸、间距、行列数与行列标注规则估算标定矩阵外框宽高（mm），对齐 `laser_core::material::layout_material_test`。
 */
export function estimateMatrixDimensionsMm({
  testType = "matrix",
  rows = 5,
  cols = 5,
  intervalCount = 9,
  yAxis = "speed",
  speedMax = 2500,
  powerMax = 600,
  intervalMax = 0.16,
  passesMax = 3,
  labels = true,
  cellSizeMm = undefined,
  gapMm = undefined,
} = {}) {
  const isInterval = testType === "interval";
  const r = isInterval ? 1 : Math.max(1, Math.round(Number(rows) || 5));
  const c = Math.max(1, Math.round(Number(isInterval ? intervalCount : cols) || (isInterval ? 9 : 5)));
  const size = Number(cellSizeMm) > 0 ? Number(cellSizeMm) : (isInterval ? 8 : 12);
  const gap = Number(gapMm) >= 0 ? Number(gapMm) : 4;
  const gridWidth = c * size + Math.max(0, c - 1) * gap;
  const gridHeight = r * size + Math.max(0, r - 1) * gap;
  if (!labels) {
    return {
      widthMm: Number(gridWidth.toFixed(1)),
      heightMm: Number(gridHeight.toFixed(1)),
    };
  }
  const labelH = isInterval ? 2.5 : 3.0;
  const labelGap = 1.5;
  let rowSample = "2500";
  if (yAxis === "power") {
    const pct = Math.round(Number(powerMax) || 600) / 10;
    rowSample = Number.isInteger(pct) ? String(pct) : pct.toFixed(1);
  } else if (yAxis === "interval") {
    rowSample = (Number(intervalMax) || 0.16).toFixed(2);
  } else if (yAxis === "passes") {
    rowSample = String(Math.max(1, Math.round(Number(passesMax) || 3)));
  } else {
    rowSample = String(Math.max(100, Math.round(Number(speedMax) || 2500)));
  }
  const leftOffset = r > 1 ? labelH + labelGap + strokeLabelWidthMm(rowSample, labelH) + labelGap : 0;
  const topOffset = c > 1 ? 2 * (labelGap + labelH) : 0;
  return {
    widthMm: Number((gridWidth + leftOffset).toFixed(1)),
    heightMm: Number((gridHeight + topOffset).toFixed(1)),
  };
}

function formatMatrixMm(v) {
  const rounded = Number(Number(v || 0).toFixed(1));
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}

/** 生成矩阵前校验估算外框是否超出当前连接目标的有效行程。 */
export function checkMatrixFitsWorkspace(dimensions, workspaceLimits) {
  const widthMm = Number(dimensions?.widthMm) || 0;
  const heightMm = Number(dimensions?.heightMm) || 0;
  const maxW = Number(workspaceLimits?.x_travel_mm) > 0 ? Number(workspaceLimits.x_travel_mm) : 100;
  const maxH = Number(workspaceLimits?.y_travel_mm) > 0 ? Number(workspaceLimits.y_travel_mm) : 100;
  if (widthMm > maxW + 0.01 || heightMm > maxH + 0.01) {
    return {
      ok: false,
      widthMm,
      heightMm,
      error: `矩阵约 ${formatMatrixMm(widthMm)}×${formatMatrixMm(heightMm)} mm，超出当前行程 ${formatMatrixMm(maxW)}×${formatMatrixMm(maxH)} mm，请减少行列或缩小单元`,
    };
  }
  return { ok: true, widthMm, heightMm };
}

/** 标定矩阵生成输入快照签名，用于判定修改参数后矩阵预览是否已过期（仅纳入当前测试类型与选中坐标轴真正用到的字段）。 */
export function matrixInputSignature({
  material = "",
  thickness = "",
  mode = "raster",
  rows = 5,
  cols = 5,
  powerMin = 100,
  powerMax = 600,
  speedMin = 500,
  speedMax = 2500,
  testType = "matrix",
  xAxis = "power",
  yAxis = "speed",
  intervalMin = 0.08,
  intervalMax = 0.16,
  intervalCount = 9,
  passesMin = 1,
  passesMax = 3,
  labels = true,
} = {}) {
  const normMode = mode === "cut" ? "cut" : mode === "outline" ? "outline" : "raster";
  if (testType === "interval") {
    return JSON.stringify([
      String(material).trim(),
      String(thickness).trim(),
      normMode,
      "interval",
      Number(intervalMin),
      Number(intervalMax),
      Number(intervalCount),
      Boolean(labels),
    ]);
  }
  const uses = (axis) => xAxis === axis || yAxis === axis;
  return JSON.stringify([
    String(material).trim(),
    String(thickness).trim(),
    normMode,
    Number(rows),
    Number(cols),
    uses("power") ? Number(powerMin) : null,
    uses("power") ? Number(powerMax) : null,
    uses("speed") ? Number(speedMin) : null,
    uses("speed") ? Number(speedMax) : null,
    "matrix",
    String(xAxis),
    String(yAxis),
    uses("interval") ? Number(intervalMin) : null,
    uses("interval") ? Number(intervalMax) : null,
    uses("passes") ? Number(passesMin) : null,
    uses("passes") ? Number(passesMax) : null,
    Boolean(labels),
  ]);
}

export function isMatrixStale(savedSignature, currentSignature) {
  if (!savedSignature) return false;
  return savedSignature !== currentSignature;
}

/** 构造标定矩阵发送请求载荷（兼容网络与串口模式，错误时返回 `{ error }`）。 */
export function buildMatrixSendRequest({
  calibrationId,
  connectionMode = "network",
  networkHost = "",
  telnetPort = 23,
  serialPort = "",
  baudrate = 115200,
} = {}) {
  const id = String(calibrationId || "").trim();
  if (!id) {
    return { error: "请先生成测试矩阵" };
  }
  const mode = connectionMode === "serial" ? "serial" : "network";
  if (mode === "serial") {
    const port = String(serialPort || "").trim();
    if (!port) {
      return { error: "请先选择或填写 USB 串口号" };
    }
    return {
      payload: {
        calibration_id: id,
        confirmed: true,
        connection_mode: "serial",
        port,
        baudrate: Number(baudrate) || 115200,
      },
      targetLabel: `${port} @ ${Number(baudrate) || 115200}`,
      estopForm: {
        connection_mode: "serial",
        port,
        baudrate: Number(baudrate) || 115200,
      },
    };
  }

  const host = String(networkHost || "").trim();
  if (!host) {
    return { error: "请填写设备 host / IP" };
  }
  const portNum = Number(telnetPort) || 23;
  return {
    payload: {
      calibration_id: id,
      confirmed: true,
      connection_mode: "network",
      network_host: host,
      network_telnet_port: portNum,
    },
    targetLabel: `${host}:${portNum}`,
    estopForm: {
      connection_mode: "network",
      network_host: host,
      network_telnet_port: portNum,
    },
  };
}

/**
 * 标定矩阵发送请求返回后的状态转移（2026-11-21 从页面抽出）：
 * - `approval`：严格模式下进了桌面审批，只提示，不轮询；
 * - `sending`：后端已开始逐行下发，开始轮询；
 * - `unknown`：网络中断或超时（status 0），后端可能已开始加工——保留规范目标供顶栏急停并立即轮询，
 *   严禁报“失败”诱导用户重发；
 * - `failed`：门禁或后端明确拒绝。
 * `sendState` 为 null 表示不改动当前发送状态。
 */
export function matrixSendOutcome(res, built) {
  if (isApprovalRequiredResponse(res)) {
    return { kind: "approval", sendState: null, status: approvalPendingMessage(res, "发送标定矩阵"), poll: false };
  }
  const sendResult = res?.data?.result?.send_result;
  if (res?.ok && res?.data?.success && sendResult?.running) {
    const total = sendResult.total_lines || 0;
    return {
      kind: "sending",
      sendState: { status: "sending", lines_sent: 0, total_lines: total, target: sendResult.target },
      status: `握手通过，正在逐行下发 (0/${total} 行)…`,
      poll: true,
    };
  }
  if (res?.status === 0) {
    const payload = built?.payload;
    const target =
      payload?.connection_mode === "serial"
        ? `${payload.port}@${Number(payload.baudrate) || 115200}`
        : built?.targetLabel;
    return {
      kind: "unknown",
      sendState: { status: "sending", lines_sent: 0, total_lines: 0, target, connectionLost: true },
      status: "发送结果未知（连接中断或超时），正在查询原标定任务，请勿重复发送；必要时使用顶栏急停。",
      poll: true,
    };
  }
  return { kind: "failed", sendState: null, status: `发送失败: ${res?.data?.result || "门禁拦截"}`, poll: false };
}

const MATRIX_TERMINAL_STATUSES = new Set(["completed", "failed", "cancelled"]);

/**
 * 一次标定任务轮询后的状态转移：`job` 为 `applyJobPoll` 合并后的任务状态，`res` 为本次原始响应。
 * 查询失败（`connectionLost`）时不改发送状态、不当作结束，只提示状态未知；`terminal` 为真时调用方停止轮询。
 */
export function matrixPollOutcome(previous, job, res) {
  if (job?.connectionLost) {
    return {
      sendState: previous ?? null,
      status: "状态未知：暂时无法查询设备，正在重试；机器可能仍在运行，请到现场确认。",
      terminal: false,
    };
  }
  const sr = res?.data?.result?.send_result || {};
  const sendState = {
    ...previous,
    status: job.status,
    target: sr.target || previous?.target,
    lines_sent: job.progress?.sent ?? previous?.lines_sent ?? 0,
    total_lines: job.progress?.total ?? previous?.total_lines ?? 0,
    error: sr.error,
  };
  if (MATRIX_TERMINAL_STATUSES.has(job.status)) {
    return {
      sendState,
      status:
        job.status === "completed"
          ? `标定矩阵加工完成 (${sr.lines_sent || 0}/${sr.total_lines || 0} 行已确认)，请观察样板并点选最佳单元格`
          : `标定矩阵记录为${job.status === "cancelled" ? "已取消" : "发送失败"}，请确认设备状态${sr.error ? `: ${sr.error}` : ""}`,
      terminal: true,
    };
  }
  return {
    sendState,
    status: `正在逐行下发 (${job.progress?.sent || 0}/${job.progress?.total || 0} 行)…`,
    terminal: false,
  };
}

/** 标定向导测试图案（r40：scan_offset / overscan / kerf）在确认框里的说明；不是向导图案时返回空串。 */
export function wizardPatternLine(preview) {
  if (!preview?.wizard_pattern) return "";
  const cells = Array.isArray(preview.cells) ? preview.cells : [];
  if (preview.test_type === "scan_offset") {
    return `双向扫描偏移测试：${cells.length} 个实心方块，速度 ${cells.map((c) => c.speed).join(" / ")} mm/min，偏移 0、过扫描按速度自动`;
  }
  if (preview.test_type === "overscan") {
    const over = Number(cells[1]?.overscan_mm);
    return `过扫描检查：左块不过扫描，右块过扫描 ${Number.isFinite(over) ? over.toFixed(1) : "-"} mm（当前设置）`;
  }
  if (preview.test_type === "kerf") {
    const cell = cells[0] || {};
    return `切缝测试：按切割参数 S${cell.power ?? "-"} · F${cell.speed ?? "-"} × ${cell.passes ?? 1} 遍切一个 ${preview.square_mm ?? 20} mm 方块（不做切缝补偿），会切穿材料`;
  }
  return "";
}

/** 构造发送标定矩阵前的二次安全确认文案。 */
export function buildMatrixConfirmPrompt({
  preview,
  material,
  thickness,
  mode,
  targetLabel,
} = {}) {
  const modeInfo = resolveMatrixModeParams(mode);
  const mat = preview?.material || material || "当前材料";
  const thick = preview?.thickness_mm ?? thickness ?? "-";
  const width = Number(preview?.width_mm);
  const height = Number(preview?.height_mm);
  const sizeText = Number.isFinite(width) && Number.isFinite(height)
    ? `${width.toFixed(1)} × ${height.toFixed(1)} mm`
    : "按网格推算";
  const cellCount = preview?.cell_count || preview?.cells?.length || 0;
  const pMin = preview?.power_min ?? "-";
  const pMax = preview?.power_max ?? "-";
  const powerRangeText =
    pMin !== "-" && pMax !== "-" && Number.isFinite(Number(pMin)) && Number.isFinite(Number(pMax))
      ? `${powerScaleToPercent(pMin)}% ~ ${powerScaleToPercent(pMax)}%（S${pMin}~S${pMax}）`
      : `S${pMin}~S${pMax}`;
  const sMin = preview?.speed_min ?? "-";
  const sMax = preview?.speed_max ?? "-";
  const isInterval = preview?.test_type === "interval";
  const patternLine = wizardPatternLine(preview);
  const axisLine = patternLine
    ? patternLine
    : isInterval
    ? `线距测试：${preview?.interval_min_mm ?? "-"} ~ ${preview?.interval_max_mm ?? "-"} mm，目标是相邻扫描线刚好相接`
    : preview?.x_axis
      ? `网格轴：列 = ${axisInfo(preview.x_axis).label}，行 = ${axisInfo(preview.y_axis).label}`
      : "";
  const orderLine = preview?.run_order === "grid"
    ? "加工顺序：按网格顺序"
    : preview?.run_order
      ? "加工顺序：从烧蚀风险最低（高速、低功率、宽线距、少遍数）到最高"
      : "";
  const labelLine = preview?.labels?.enabled
    ? `行列标注：单线文字，功率 S${preview.labels.power} · 速度 F${preview.labels.speed}`
    : "";
  return {
    title: "确认下发标定矩阵出光测试",
    confirmText: "确认开刻测试",
    message: [
      `目标设备：${targetLabel || "未指定"}`,
      `标定对象：${mat} · ${preview?.surface_entry ? "表面通用" : `${thick} mm`}（${modeInfo.label}）`,
      `矩阵规格：${cellCount} 格 · 外框尺寸 ${sizeText}`,
      `参数区间：功率 ${powerRangeText} · 速度 F${sMin}~F${sMax} mm/min`,
      ...[axisLine, orderLine, labelLine].filter(Boolean),
      "",
      "注意：标定刀路以当前激光头位置为左下角起点（G92 X0 Y0），行列标注也在这个范围内。请确认样板已放稳、排风开启并已佩戴护目镜。",
    ].join("\n"),
  };
}

/** 解包导入的材料库 JSON（兼容直接导出对象与 `{ library: {...} }` 包装）。 */
export function unwrapImportedMaterialLibrary(parsed) {
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { error: "导入文件不是有效的 JSON 对象" };
  }
  const candidate = parsed.version == null && parsed.library && typeof parsed.library === "object"
    ? parsed.library
    : parsed;
  if (Number(candidate.version) !== 1 || typeof candidate.version !== "number") {
    return { error: "导入材料库格式不支持：需要 version=1 的 JSON 对象" };
  }
  const materials = candidate.materials && typeof candidate.materials === "object" && !Array.isArray(candidate.materials)
    ? candidate.materials
    : {};
  return {
    library: candidate,
    materialCount: Object.keys(materials).length,
  };
}
