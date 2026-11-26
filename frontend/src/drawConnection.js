// 创作画板的设备连接、成品尺寸、预览过期判定与场景导入校验（2026-10-14；2026-11-11 增补防呆与互通）。
// 支持网口（Telnet）和串口；宽 / 高留空表示按画板导出尺寸，不再用 1 mm 兜底。
// 2026-11-17（技术书 r45）：“生成策略”不再有矢量一项，改由雕刻路线全局开关（矢量 / 转图片，与工作台、语音
// 同一个值）决定；转图片时再选“转图片方式”：光栅扫描（默认，与工作台图片同一套引擎）或轮廓描边。

import { approvalPendingMessage, isApprovalRequiredResponse } from "./approvalState.js";
import { readUnifiedConnection } from "./deviceConnection.js";
import { normalizeEngraveRoute } from "./engraveRoute.js";
import { intervalScaleValue } from "./qualityPresets.js";
import { imageParamsPayload, normalizeImageMode } from "./workbenchParams.js";

/** 转图片方式（雕刻路线为“转图片”时）。 */
export const DRAW_IMAGE_STRATEGIES = Object.freeze([
  { value: "outline", label: "轮廓（描出深色边缘）" },
  { value: "raster", label: "填充（逐行扫描，默认）" },
]);

/**
 * 矢量刻法（2026-11-17 用户决定，技术书 r46：“轮廓和填充可以选择”）：只影响闭合图形与文字，
 * 手绘、开放直线与箭头始终刻一笔。
 */
export const DRAW_VECTOR_STYLES = Object.freeze([
  { value: "outline", label: "轮廓（闭合图形与文字只描边，默认）" },
  { value: "fill", label: "填充（闭合图形与文字都填满）" },
  { value: "cut", label: "切割（沿闭合图形与文字切穿，先内后外）" },
]);

/** 没存过或值无效时按轮廓（与工作台文字默认只描轮廓一致）。 */
export function normalizeDrawVectorStyle(style) {
  return DRAW_VECTOR_STYLES.some((opt) => opt.value === style) ? style : "outline";
}

/** 按画布是否含位图或材料是否仅为表面通用条目，返回当前可选的矢量刻法列表。 */
export function availableDrawVectorStyles({ hasSceneImages = false, surfaceOnly = false } = {}) {
  return hasSceneImages || surfaceOnly
    ? DRAW_VECTOR_STYLES.filter((opt) => opt.value !== "cut")
    : DRAW_VECTOR_STYLES;
}

/**
 * 计算当前实际生效的矢量刻法（不改写用户已保存的 `form.vector_style`）：
 * 画布含位图或材料只有表面通用条目时临时回退为 `"outline"`，删掉图片或换回带厚度材料后自动恢复原选的 `"cut"`。
 */
export function effectiveDrawVectorStyle(style, options = {}) {
  const normalized = normalizeDrawVectorStyle(style);
  const allowed = availableDrawVectorStyles(options);
  return allowed.some((opt) => opt.value === normalized) ? normalized : "outline";
}

/** 查材料推荐用哪一档：光栅扫描与矢量“填充”用填充（raster）参数，矢量“切割”用切割参数，其余用描边参数。 */
export function drawRecommendEngravingMode(mode, vectorStyle) {
  if (mode === "raster") return "raster";
  if (mode === "vector" && normalizeDrawVectorStyle(vectorStyle) === "fill") return "raster";
  if (mode === "vector" && normalizeDrawVectorStyle(vectorStyle) === "cut") return "cut";
  return "outline";
}

/** 画板是不是切割任务（矢量刻法“切割”，r47）。 */
export function drawIsCut(mode, vectorStyle) {
  return mode === "vector" && normalizeDrawVectorStyle(vectorStyle) === "cut";
}

/** 矢量刻法对应的填充 / 切割设置开关（`fillSettingsPayload` 等的 `{ withFill, withCut }`）。 */
export function drawVectorLayerFlags(vectorStyle) {
  const style = normalizeDrawVectorStyle(vectorStyle);
  return { withFill: style === "fill", withCut: style === "cut" };
}

/** 没存过或值无效时按光栅扫描。 */
export function normalizeDrawImageStrategy(strategy) {
  return strategy === "outline" ? "outline" : "raster";
}

/** 画板请求实际的 `mode`：矢量路线 → vector；转图片 → 光栅扫描 raster / 轮廓描边 outline。 */
export function drawRequestMode(route, imageStrategy) {
  return normalizeEngraveRoute(route) === "image" ? normalizeDrawImageStrategy(imageStrategy) : "vector";
}

/** 光栅扫描的转换方式、行间距倍数与网点/灰度专属参数 → 请求字段；填错返回 `{ error }`。 */
export function drawRasterImagePayload(form) {
  return imageParamsPayload({
    imageMode: form?.image_mode,
    extras: {
      intervalScale: form?.line_interval_scale ?? "",
      halftoneCellMm: form?.halftone_cell_mm ?? "",
      halftoneAngleDeg: form?.halftone_angle_deg ?? "",
      grayMinPowerPercent: form?.gray_min_power_percent ?? "",
    },
  });
}

/** 画布有效元素里是否包含插入的位图图片。 */
export function hasSceneImageElements(elements) {
  return Array.isArray(elements) && elements.some((el) => el && !el.isDeleted && el.type === "image");
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

/**
 * 按当前雕刻机最大雕刻范围校验画板输入的宽 / 高（留空表示自动缩放到幅面内）：
 * 填了非正数或超过机器行程时给出明确原因拦截。
 */
export function validateDrawDimensions(form, { maxWidthMm = 100, maxHeightMm = 100 } = {}) {
  const limitW = Number(maxWidthMm) > 0 ? Number(maxWidthMm) : 100;
  const limitH = Number(maxHeightMm) > 0 ? Number(maxHeightMm) : 100;
  const check = (raw, label, maxMm) => {
    const trimmed = String(raw ?? "").trim();
    if (!trimmed) return { value: null };
    const num = Number(trimmed);
    if (!Number.isFinite(num) || num <= 0) {
      return { error: `${label}须为大于 0 的数字（留空为自动）` };
    }
    if (num > maxMm) {
      return {
        error: `${label} ${num} mm 超出当前雕刻机最大行程 ${maxMm} mm（若已更换大幅面机器，请先握手探测或在数字孪生页同步行程）`,
      };
    }
    return { value: num };
  };
  const w = check(form?.width_mm, "成品宽度", limitW);
  if (w.error) return { error: w.error };
  const h = check(form?.height_mm, "成品高度", limitH);
  if (h.error) return { error: h.error };
  return { widthMm: w.value, heightMm: h.value };
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
export function buildDrawFramingPayload(form, width, height, outline = null, frame = null) {
  // 2026-11-16 两点对位结果：范围相对第 2 个标记点，取变换后的 frame_mm（可为负）
  const box = frame
    ? { min_x: frame.min_x, min_y: frame.min_y, max_x: frame.max_x, max_y: frame.max_y }
    : { min_x: 0, min_y: 0, max_x: Number(width) || 100, max_y: Number(height) || 100 };
  return {
    ...drawConnectionPayload(form),
    relative_to_head: true,
    ...box,
    feed_rate: 2000,
    power: 0,
    // 2026-11-16 橡皮筋走边框：沿出光点外轮廓走（来自预览 frame_hull_mm）
    ...(Array.isArray(outline) && outline.length >= 3 ? { outline } : {}),
  };
}

/** 走边框响应（按 client.js `{ status, ok, data }` 契约）转界面状态文案。 */
export function formatFramingStatus(res, width, height) {
  if (isApprovalRequiredResponse(res)) {
    return approvalPendingMessage(res, "走边框");
  }
  if (res?.ok && res?.data?.success) {
    return `走边框完成 (${width.toFixed(1)} x ${height.toFixed(1)} mm，未出光)`;
  }
  const errMsg = typeof res?.data?.result === "string" && res.data.result ? res.data.result : "未知错误";
  return `走边框未完成：${errMsg}`;
}

/**
 * 画板表单默认值（2026-11-05；2026-11-11 互通升级）：
 * 优先从 `laserLab.*` 读画板自己的记忆；若画板尚未保存过任何设备地址/串口，自动回退复用雕刻工作台已填好的 `laser-forge-*` 连接参数。
 * 存储被禁用或读取抛错时逐项退回内置默认值，页面照常加载。
 */
export function readDrawDefaultForm(...args) {
  const explicitStorage = args.length > 0;
  const storage = explicitStorage ? args[0] : safeBrowserStorage();
  const read = (fullKey) => {
    try {
      return storage?.getItem?.(fullKey) ?? null;
    } catch (_error) {
      return null;
    }
  };
  const conn = explicitStorage ? readUnifiedConnection(storage) : readUnifiedConnection();

  return {
    material: read("laserLab.material") || "椴木",
    thickness_mm: read("laserLab.thickness_mm") || "3",
    width_mm: read("laserLab.width_mm") || "",
    height_mm: read("laserLab.height_mm") || "",
    // 转图片方式；旧版本的“生成策略”（laserLab.mode）存了轮廓描边的迁移成轮廓描边，其余按光栅扫描
    image_strategy: normalizeDrawImageStrategy(read("laserLab.image_strategy") ?? read("laserLab.mode")),
    // 矢量刻法（r46）：按画布颜色 / 轮廓 / 填充
    vector_style: normalizeDrawVectorStyle(read("laserLab.vector_style")),
    image_mode: normalizeImageMode(read("laserLab.image_mode")),
    // 行间距倍数（质量预设用）；留空 = 1
    line_interval_scale: read("laserLab.line_interval_scale") || "",
    halftone_cell_mm: read("laserLab.halftone_cell_mm") || "",
    halftone_angle_deg: read("laserLab.halftone_angle_deg") || "",
    gray_min_power_percent: read("laserLab.gray_min_power_percent") || "",
    connection_mode: conn.connection_mode,
    network_host: conn.network_host,
    network_telnet_port: conn.network_telnet_port,
    port: conn.port,
    baudrate: conn.baudrate,
    vector_simplify_factor: read("laserLab.vector_simplify_factor") || "3",
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
 * 走边框确认文案（2026-11-05 用户决定：走边框会移动机器，必须先确认；r47 起一律不出光）。
 */
export function framingConfirmText(form, width, height, outlinePoints = 0, registered = false) {
  const size = outlinePoints >= 3
    ? `图形外轮廓（${outlinePoints} 个点，范围 ${Number(width).toFixed(1)} × ${Number(height).toFixed(1)} mm）`
    : `${Number(width).toFixed(1)} × ${Number(height).toFixed(1)} mm 的边框`;
  const start = registered ? "第 2 个对位标记" : "左下角起点";
  return `将连接 ${drawTargetLabel(form)}，机器会实际移动，从激光头当前位置（${start}）沿 ${size}走一圈再回到原处。激光保持关闭，只移动激光头。\n请确认行程内无遮挡、手已离开机台；随时可按顶栏急停。`;
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
  const effectiveMode = mode === "vector" && custom?.vectorRasterFallback ? "raster" : mode;
  return JSON.stringify({
    material: String(form?.material ?? "").trim(),
    thickness_mm: String(form?.thickness_mm ?? "").trim(),
    width_mm: String(form?.width_mm ?? "").trim(),
    height_mm: String(form?.height_mm ?? "").trim(),
    route: normalizeEngraveRoute(route),
    mode,
    vector_style: effectiveMode === "vector" ? normalizeDrawVectorStyle(form?.vector_style) : "",
    simplify: effectiveMode === "outline" ? String(form?.vector_simplify_factor ?? "3").trim() : "",
    image: mode === "raster"
      ? (() => {
          const imgMode = normalizeImageMode(form?.image_mode);
          const extra =
            imgMode === "halftone"
              ? `:${String(form?.halftone_cell_mm ?? "").trim()}@${String(form?.halftone_angle_deg ?? "").trim()}`
              : imgMode === "grayscale"
                ? `:${String(form?.gray_min_power_percent ?? "").trim()}`
                : "";
          return `${imgMode}x${intervalScaleValue(form?.line_interval_scale)}${extra}`;
        })()
      : "",
    fill: effectiveMode === "outline" ? "" : String(custom?.fill ?? ""),
    manual: custom?.enabled
      ? `${String(custom.power ?? "").trim()}/${String(custom.feed ?? "").trim()}/${String(custom.passes ?? "").trim()}`
      : custom?.presetParams
        ? `preset:${custom.presetParams}`
        : "preset",
    ...(custom?.workArea && custom.workArea !== "100x100" ? { work_area: String(custom.workArea) } : {}),
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

/**
 * 画板采用反馈调参或派生预览前的门禁校验：
 * 必须关联当前画板任务，且当前预览未过期；通过时沿用原快照签名（不按当前已改动状态重新盖章）。
 */
export function checkDerivedDrawAdoption({ workflow, currentWorkflowId, previewSnap, previewStale }) {
  if (!workflow?.workflow_id) return { ok: false, reason: "派生预览记录无效" };
  const parentId = workflow.regenerated_from || workflow.registered_from || "";
  if (parentId && currentWorkflowId && parentId !== currentWorkflowId) {
    return { ok: false, reason: "派生预览不是当前画板任务生成的，已忽略" };
  }
  if (previewStale || !previewSnap?.formSig) {
    return {
      ok: false,
      reason: "画布或左侧参数已改动，当前预览已过期，已拦截采用旧任务派生结果；请先重新生成预览。",
    };
  }
  return { ok: true, inheritedSnap: previewSnap };
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
