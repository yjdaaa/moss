/**
 * 材料实验室（MaterialLabPage）纯状态与校验逻辑。
 * 不依赖 React，供页面与单元测试共用。
 */

import { thicknessesEqual } from "./materialSelectState.js";

export const MATRIX_MODE_OPTIONS = [
  { id: "raster", label: "光栅雕刻", code: "Raster" },
  { id: "outline", label: "轮廓扫描", code: "Outline" },
  { id: "cut", label: "边缘切割", code: "Cut" },
];

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
    aliases,
    hasSavedCell: true,
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

/** 标定矩阵生成输入快照签名，用于判定修改参数后矩阵预览是否已过期。 */
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
} = {}) {
  return JSON.stringify([
    String(material).trim(),
    String(thickness).trim(),
    mode === "cut" ? "cut" : mode === "outline" ? "outline" : "raster",
    Number(rows),
    Number(cols),
    Number(powerMin),
    Number(powerMax),
    Number(speedMin),
    Number(speedMax),
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
  const sMin = preview?.speed_min ?? "-";
  const sMax = preview?.speed_max ?? "-";
  return {
    title: "确认下发标定矩阵出光测试",
    confirmText: "确认开刻测试",
    message: [
      `目标设备：${targetLabel || "未指定"}`,
      `标定对象：${mat} · ${thick} mm（${modeInfo.label}）`,
      `矩阵规格：${cellCount} 格 · 外框尺寸 ${sizeText}`,
      `参数区间：功率 S${pMin}~S${pMax} · 速度 F${sMin}~F${sMax} mm/min`,
      "",
      "注意：标定刀路以当前激光头位置为左下角起点（G92 X0 Y0）。请确认样板已放稳、排风开启并已佩戴护目镜。",
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
