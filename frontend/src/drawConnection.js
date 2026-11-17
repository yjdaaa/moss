// 创作画板的设备连接、成品尺寸、预览过期判定与场景导入校验（2026-10-14；2026-11-11 增补防呆与互通）。
// 支持网口（Telnet）和串口；宽 / 高留空表示按画板导出尺寸，不再用 1 mm 兜底。
// 2026-11-17（技术书 r45）：“生成策略”不再有矢量一项，改由雕刻路线全局开关（矢量 / 转图片，与工作台、语音
// 同一个值）决定；转图片时再选“转图片方式”：光栅扫描（默认，与工作台图片同一套引擎）或轮廓描边。

import { normalizeEngraveRoute } from "./engraveRoute.js";
import { intervalScaleValue } from "./qualityPresets.js";
import { imageParamsPayload, normalizeImageMode } from "./workbenchParams.js";

/** 转图片方式（雕刻路线为“转图片”时）。 */
export const DRAW_IMAGE_STRATEGIES = Object.freeze([
  { value: "raster", label: "光栅扫描（逐行扫描填满，默认）" },
  { value: "outline", label: "轮廓描边（描出深色边缘）" },
]);

/** 没存过或值无效时按光栅扫描。 */
export function normalizeDrawImageStrategy(strategy) {
  return strategy === "outline" ? "outline" : "raster";
}

/** 画板请求实际的 `mode`：矢量路线 → vector；转图片 → 光栅扫描 raster / 轮廓描边 outline。 */
export function drawRequestMode(route, imageStrategy) {
  return normalizeEngraveRoute(route) === "image" ? normalizeDrawImageStrategy(imageStrategy) : "vector";
}

/** 光栅扫描的转换方式与行间距倍数 → 请求字段（`image_mode`、`line_interval_scale`）；填错返回 `{ error }`。 */
export function drawRasterImagePayload(form) {
  return imageParamsPayload({
    imageMode: form?.image_mode,
    extras: { intervalScale: form?.line_interval_scale ?? "" },
  });
}

/**
 * 导出 PNG 前要不要把彩色笔迹压成纯黑（`prepareRasterExportElements` 的模式参数）：只在光栅扫描且按黑白
 * 处理（自动识别 / 清晰黑白线条）时压；选了抖动、灰度等要深浅的方式时保留颜色深浅。
 */
export function drawInkExportMode(mode, imageMode) {
  if (mode !== "raster") return mode;
  return ["auto", "threshold"].includes(normalizeImageMode(imageMode)) ? "raster" : "raster-tone";
}

/** 宽 / 高输入：空或非正数为 null（后端按“自动”处理）。 */
export function drawSizeValue(value) {
  if (String(value ?? "").trim() === "") return null;
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : null;
}

export function isSerial(form) {
  return form?.connection_mode === "serial";
}

/** 发送、走边框、测试连接共用的连接字段。 */
export function drawConnectionPayload(form) {
  if (isSerial(form)) {
    return { connection_mode: "serial", port: String(form.port || "").trim(), baudrate: Number(form.baudrate) || 115200 };
  }
  return {
    connection_mode: "network",
    network_host: String(form.network_host || "").trim(),
    network_telnet_port: Number(form.network_telnet_port) || 23,
  };
}

/** 是否已填好连接目标（串口名或设备地址）。 */
export function drawTargetReady(form) {
  return isSerial(form) ? Boolean(String(form.port || "").trim()) : Boolean(String(form.network_host || "").trim());
}

/** 连接目标的显示文字。 */
export function drawTargetLabel(form) {
  if (isSerial(form)) return `串口 ${String(form.port || "").trim() || "未选择"} · ${Number(form.baudrate) || 115200}`;
  return `${String(form.network_host || "").trim()}:${Number(form.network_telnet_port) || 23}`;
}

/** 目标没填时的提示。 */
export function drawTargetMissingText(form) {
  return isSerial(form) ? "请先选择串口" : "请先填写设备 host/IP";
}

/** 判断 `/api/connection/check` 响应在当前所选连接方式（串口或网络）下是否真正握手连通。 */
export function isDrawConnectionOk(form, result) {
  if (!result?.success) return false;
  const payload = result.result && typeof result.result === "object" ? result.result : {};
  return isSerial(form) ? Boolean(payload.serial_connected) : Boolean(payload.network_connected);
}

/**
 * 构造创作画板走边框请求载荷：统一附带 `relative_to_head: true`，
 * 与画板正式雕刻刀路开头的 `G92 X0 Y0 Z0`（以激光头当前位置为左下角起点）保持同一参照系。
 */
export function buildDrawFramingPayload(form, width, height, framingLight = false, outline = null, frame = null) {
  // 2026-11-16 两点对位结果：范围相对第 2 个标记点，取变换后的 frame_mm（可为负）
  const box = frame
    ? { min_x: frame.min_x, min_y: frame.min_y, max_x: frame.max_x, max_y: frame.max_y }
    : { min_x: 0, min_y: 0, max_x: Number(width) || 100, max_y: Number(height) || 100 };
  return {
    ...drawConnectionPayload(form),
    relative_to_head: true,
    ...box,
    feed_rate: 2000,
    power: framingLight ? 1 : 0,
    // 2026-11-16 橡皮筋走边框：沿出光点外轮廓走（来自预览 frame_hull_mm）
    ...(Array.isArray(outline) && outline.length >= 3 ? { outline } : {}),
  };
}

/** 走边框响应（按 client.js `{ status, ok, data }` 契约）转界面状态文案。 */
export function formatFramingStatus(res, width, height, framingLight) {
  if (res?.ok && res?.data?.success) {
    return `走边框完成 (${width.toFixed(1)} x ${height.toFixed(1)} mm${framingLight ? "，弱光" : "，未出光"})`;
  }
  const errMsg = typeof res?.data?.result === "string" && res.data.result ? res.data.result : "未知错误";
  return `走边框未完成：${errMsg}`;
}

/**
 * 画板表单默认值（2026-11-05；2026-11-11 互通升级）：
 * 优先从 `laserLab.*` 读画板自己的记忆；若画板尚未保存过任何设备地址/串口，自动回退复用雕刻工作台已填好的 `laser-forge-*` 连接参数。
 * 存储被禁用或读取抛错时逐项退回内置默认值，页面照常加载。
 */
export function readDrawDefaultForm(storage = safeBrowserStorage()) {
  const read = (fullKey) => {
    try {
      return storage?.getItem?.(fullKey) ?? null;
    } catch (_error) {
      return null;
    }
  };
  const labModeRaw = read("laserLab.connection_mode");
  const labHost = read("laserLab.network_host") || "";
  const labPort = read("laserLab.port") || "";
  const hasLabTarget = Boolean(labModeRaw || labHost.trim() || labPort.trim());

  const forgeConnMode = read("laser-forge-conn-mode") === "serial" ? "serial" : "network";
  const connection_mode = hasLabTarget
    ? (labModeRaw === "serial" ? "serial" : "network")
    : forgeConnMode;
  const network_host = labHost || read("laser-forge-host") || "";
  const network_telnet_port = read("laserLab.network_telnet_port") || read("laser-forge-telnet-port") || "23";
  const port = labPort || read("laser-forge-serial-port") || "";
  const baudrate = read("laserLab.baudrate") || read("laser-forge-baud") || "115200";

  return {
    material: read("laserLab.material") || "椴木",
    thickness_mm: read("laserLab.thickness_mm") || "3",
    width_mm: read("laserLab.width_mm") || "",
    height_mm: read("laserLab.height_mm") || "",
    // 转图片方式；旧版本的“生成策略”（laserLab.mode）存了轮廓描边的迁移成轮廓描边，其余按光栅扫描
    image_strategy: normalizeDrawImageStrategy(read("laserLab.image_strategy") ?? read("laserLab.mode")),
    image_mode: normalizeImageMode(read("laserLab.image_mode")),
    // 行间距倍数（质量预设用）；留空 = 1
    line_interval_scale: read("laserLab.line_interval_scale") || "",
    connection_mode,
    network_host,
    network_telnet_port,
    port,
    baudrate,
    vector_simplify_factor: read("laserLab.vector_simplify_factor") || "3",
    output_format: "gcode",
  };
}

/** 取浏览器 localStorage；访问本身抛错（存储被禁用、沙箱）时返回 null。 */
export function safeBrowserStorage() {
  try {
    return globalThis.localStorage || null;
  } catch (_error) {
    return null;
  }
}

/**
 * 走边框确认文案（2026-11-05 用户决定：走边框会移动机器，必须先确认；勾选弱光时说明会低功率出光）。
 */
export function framingConfirmText(form, width, height, framingLight, outlinePoints = 0, registered = false) {
  const size = outlinePoints >= 3
    ? `图形外轮廓（${outlinePoints} 个点，范围 ${Number(width).toFixed(1)} × ${Number(height).toFixed(1)} mm）`
    : `${Number(width).toFixed(1)} × ${Number(height).toFixed(1)} mm 的边框`;
  const light = framingLight
    ? "已勾选“弱光指示”：走边框期间激光会以 S1（约 0.1%）低功率出光，请佩戴护目镜、确认材料与周围无易燃物。"
    : "激光保持关闭，只移动激光头。";
  const start = registered ? "第 2 个对位标记" : "左下角起点";
  return `将连接 ${drawTargetLabel(form)}，机器会实际移动，从激光头当前位置（${start}）沿 ${size}走一圈再回到原处。${light}\n请确认行程内无遮挡、手已离开机台；随时可按顶栏急停。`;
}

/** 计算当前 Excalidraw 画布有效元素的轻量修订签名，用于感知增删改笔迹而无需整图重导出。 */
export function computeSceneRevision(elements) {
  if (!Array.isArray(elements) || elements.length === 0) return "empty";
  const parts = [];
  for (const el of elements) {
    if (!el || el.isDeleted) continue;
    parts.push(`${el.id || ""}:${el.version || 0}:${el.versionNonce || 0}`);
  }
  return parts.length === 0 ? "empty" : parts.join("|");
}

/**
 * 计算影响画板刀路生成的左侧参数签名（材料、厚度、宽高、雕刻路线与实际模式、转换方式与行间距、平滑度、
 * 填充 / 扫描设置与临时自定义功率速度）。`custom.fill` 由页面按实际模式给出（矢量填充设置或位图扫描设置）。
 */
export function drawGenerationSignature(form, custom = {}, route = "vector") {
  const mode = drawRequestMode(route, form?.image_strategy);
  return JSON.stringify({
    material: String(form?.material ?? "").trim(),
    thickness_mm: String(form?.thickness_mm ?? "").trim(),
    width_mm: String(form?.width_mm ?? "").trim(),
    height_mm: String(form?.height_mm ?? "").trim(),
    route: normalizeEngraveRoute(route),
    mode,
    simplify: mode === "outline" ? String(form?.vector_simplify_factor ?? "3").trim() : "",
    image: mode === "raster"
      ? `${normalizeImageMode(form?.image_mode)}x${intervalScaleValue(form?.line_interval_scale)}`
      : "",
    fill: mode === "outline" ? "" : String(custom?.fill ?? ""),
    manual: custom?.enabled
      ? `${String(custom.power ?? "").trim()}/${String(custom.feed ?? "").trim()}/${String(custom.passes ?? "").trim()}`
      : custom?.presetParams
        ? `preset:${custom.presetParams}`
        : "preset",
  });
}

/**
 * 只刻选中的图形（2026-11-16，对齐 LightBurn“只加工选中图形”）：Excalidraw `appState.selectedElementIds`
 * 里为 true 的 id，排序后返回；没有选中时返回空数组。
 */
export function selectedElementIds(appState) {
  const map = appState?.selectedElementIds;
  if (!map || typeof map !== "object") return [];
  return Object.keys(map).filter((id) => map[id] === true && id).sort();
}

/** 判断已生成的画板预览是否因画布重画或参数修改而过期。 */
export function isDrawPreviewStale(savedSnapshot, currentSnapshot) {
  if (!savedSnapshot?.formSig) return false;
  return (
    savedSnapshot.formSig !== currentSnapshot?.formSig
    || savedSnapshot.sceneRev !== currentSnapshot?.sceneRev
  );
}

/** 校验并提取导入的 `.excalidraw` / `.json` 场景数据。 */
export function parseImportedSceneJson(rawText) {
  let parsed;
  try {
    parsed = JSON.parse(String(rawText ?? ""));
  } catch {
    return { error: "场景文件不是合法的 JSON 格式" };
  }
  if (!parsed || typeof parsed !== "object" || !Array.isArray(parsed.elements)) {
    return { error: "场景文件缺少 elements 画布元素列表，无法导入" };
  }
  const appState = parsed.appState && typeof parsed.appState === "object" ? parsed.appState : {};
  const files = parsed.files && typeof parsed.files === "object" ? parsed.files : null;
  return {
    elements: parsed.elements,
    appState: {
      ...(typeof appState.gridSize === "number" ? { gridSize: appState.gridSize } : {}),
      ...(typeof appState.viewBackgroundColor === "string" ? { viewBackgroundColor: appState.viewBackgroundColor } : {}),
    },
    files,
  };
}
