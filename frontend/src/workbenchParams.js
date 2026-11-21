// n73（2026-11-08 用户决定）：工作台“这次自定义功率和速度”。只影响这一次生成的刀路，不写回材料库；
// 范围与后端 `manual_params` 校验一致（功率 1~100%、速度 100~6000 mm/min、遍数 1~5）。
// 2026-11-11 增补：文字刻法映射（对齐后端 `text_fill`）、图片尺寸/算法校验、左侧参数改动后的预览过期判定与开刻前确认文案。
// 2026-11-16（技术书 r41）：图片行间距倍数（质量预设“快速 / 均衡 / 精细”用，见 qualityPresets.js）。
// 2026-11-17（技术书 r45）：雕刻路线（矢量 / 转图片）进文字签名，图片扫描设置进图片签名。

import { normalizeEngraveRoute } from "./engraveRoute.js";
import { intervalScaleValue } from "./qualityPresets.js";

export const MANUAL_LIMITS = {
  power: { min: 1, max: 100 },
  feed: { min: 100, max: 6000 },
  passes: { min: 1, max: 5 },
};

export const IMAGE_DIMENSION_LIMIT_MM = 1000;

export const TEXT_STYLE_OPTIONS = [
  { value: "outline", label: "轮廓描边" },
  { value: "fill", label: "实心填充（矢量密排线）" },
  { value: "hatch", label: "排线填充（线距更宽、更快）" },
  { value: "cut", label: "外框切割" },
];

export const TEXT_ORIGIN_OPTIONS = [
  { value: "center", label: "木牌中心 (默认)" },
  { value: "lower_left", label: "左下角" },
];

// 2026-11-16（技术书 r38）对齐 LightBurn Image Mode；值与后端 `laser_core::image_job::TONE_MODES` 一致。
export const IMAGE_MODE_OPTIONS = [
  { value: "auto", label: "自动识别" },
  { value: "dither", label: "照片灰度抖动" },
  { value: "threshold", label: "清晰黑白线条" },
  { value: "jarvis", label: "Jarvis 抖动（照片推荐）" },
  { value: "stucki", label: "Stucki 抖动" },
  { value: "atkinson", label: "Atkinson 抖动" },
  { value: "ordered", label: "有序抖动（大面积色块）" },
  { value: "halftone", label: "网点（可调网格与角度）" },
  { value: "newsprint", label: "报纸网点" },
  { value: "sketch", label: "素描（只刻边缘）" },
  { value: "grayscale", label: "灰度变功率（二极管）" },
];

/** 图片高级项的空白初值；值均为输入框原始字符串 / 勾选状态。 */
export const DEFAULT_IMAGE_EXTRAS = {
  negative: false,
  passThrough: false,
  dotWidthMm: "",
  halftoneCellMm: "",
  halftoneAngleDeg: "",
  grayMinPowerPercent: "",
  // 行间距 = 材料推荐线距 × 倍数；留空 = 1（均衡）
  intervalScale: "",
};

/** 行间距倍数范围，与后端 `line_interval_scale` 一致。 */
export const IMAGE_INTERVAL_SCALE_LIMITS = { min: 0.5, max: 3 };

/** 图片“高级设置”折叠时的一行摘要：转换方式、行间距倍数与打开的开关。 */
export function imageAdvancedSummary(imageMode, extras = {}) {
  const label = IMAGE_MODE_OPTIONS.find((opt) => opt.value === normalizeImageMode(imageMode))?.label || "自动识别";
  const parts = [label.replace(/（.*）$/, "")];
  const scale = intervalScaleValue(extras.intervalScale);
  if (extras.passThrough) parts.push("直通");
  else parts.push(`行间距 ×${Number.isFinite(scale) ? scale : "?"}`);
  if (extras.negative) parts.push("反相");
  if (String(extras.dotWidthMm ?? "").trim() && Number(extras.dotWidthMm) > 0) parts.push(`点宽 ${extras.dotWidthMm} mm`);
  return parts.join(" · ");
}

/** 当前转换方式需要显示哪些专属输入。 */
export function imageModeFields(imageMode) {
  return {
    halftone: imageMode === "halftone",
    grayscale: imageMode === "grayscale",
  };
}

export function normalizeImageMode(imageMode) {
  return IMAGE_MODE_OPTIONS.some((opt) => opt.value === imageMode) ? imageMode : "auto";
}

/** 高级项 → 请求字段；只带当前模式相关、且用户填写过的字段，填错返回 { error }。 */
function imageExtrasPayload(imageMode, extras = {}) {
  const payload = {};
  const fields = imageModeFields(imageMode);
  const parse = (raw, check, message) => {
    const trimmed = String(raw ?? "").trim();
    if (!trimmed) return { empty: true };
    const val = Number(trimmed);
    if (!Number.isFinite(val) || !check(val)) return { error: message };
    return { value: Math.round(val * 1000) / 1000 };
  };
  if (extras.negative) payload.negative = true;
  if (extras.passThrough) payload.pass_through = true;
  // 行间距倍数：为 1 时不提交（与以前的请求完全相同）；直通模式按原图像素出行，不用倍数
  const scale = intervalScaleValue(extras.intervalScale);
  if (!Number.isFinite(scale) || scale < IMAGE_INTERVAL_SCALE_LIMITS.min || scale > IMAGE_INTERVAL_SCALE_LIMITS.max) {
    return { error: "行间距倍数要在 0.5 ~ 3 之间（留空为 1）" };
  }
  if (scale !== 1 && !extras.passThrough) payload.line_interval_scale = Math.round(scale * 1000) / 1000;
  const dot = parse(extras.dotWidthMm, (v) => v >= 0 && v < 1, "点宽补偿要是 0 ~ 1 mm 的数字，且须小于行间距（留空不补偿）");
  if (dot.error) return { error: dot.error };
  if (!dot.empty && dot.value > 0) payload.dot_width_mm = dot.value;
  if (fields.halftone) {
    const cell = parse(extras.halftoneCellMm, (v) => v > 0 && v <= 10, "网点网格要是大于 0、不超过 10 mm 的数字（留空自动）");
    if (cell.error) return { error: cell.error };
    if (!cell.empty) payload.halftone_cell_mm = cell.value;
    const angle = parse(extras.halftoneAngleDeg, (v) => v >= -90 && v <= 90, "网点角度要在 -90 ~ 90 度之间（留空为 45°）");
    if (angle.error) return { error: angle.error };
    if (!angle.empty) payload.halftone_angle_deg = angle.value;
  }
  if (fields.grayscale) {
    const min = parse(extras.grayMinPowerPercent, (v) => v >= 0 && v < 100, "灰度最小功率要在 0 ~ 100% 之间，且小于本次功率（留空为 0）");
    if (min.error) return { error: min.error };
    if (!min.empty) payload.gray_min_power_percent = min.value;
  }
  return payload;
}

const TEXT_FILL_LABELS = {
  outline: "轮廓描边",
  fill: "实心填充",
  hatch: "排线填充",
};

/** 归一化文字刻法（兼容旧版本存储值 `raster` -> `fill`），返回后端生成与材料库推荐所需的真实字段。 */
export function resolveTextStyle(rawStyle) {
  const normalized = rawStyle === "raster" ? "fill" : rawStyle;
  switch (normalized) {
    case "fill":
      return { style: "fill", label: "实心填充（矢量密排线）", laserMode: "engrave", engravingMode: "raster", textFill: "fill" };
    case "hatch":
      return { style: "hatch", label: "排线填充（线距更宽、更快）", laserMode: "engrave", engravingMode: "outline", textFill: "hatch" };
    case "cut":
      return { style: "cut", label: "外框切割", laserMode: "cut", engravingMode: "cut", textFill: "outline" };
    case "outline":
    default:
      return { style: "outline", label: "轮廓描边", laserMode: "engrave", engravingMode: "outline", textFill: "outline" };
  }
}

/** 根据当前左侧选中的加工内容，计算查询 `/api/ui/material-recommendation` 所需的模式参数。 */
export function materialRecommendationModes({ activeTool, textStyle }) {
  if (activeTool === "image") {
    return { laserMode: "engrave", engravingMode: "raster" };
  }
  const spec = resolveTextStyle(textStyle);
  return {
    laserMode: spec.laserMode,
    engravingMode: spec.laserMode === "cut" ? null : spec.engravingMode,
  };
}

/**
 * 图片预览可选尺寸、算法与高级项（反相、直通、点宽补偿、网点网格 / 角度、灰度最小功率）
 * -> `/api/workflow` preview 字段；留空不传，填错返回 `{ error }`。
 */
export function imageParamsPayload({ widthMm = "", heightMm = "", imageMode = "auto", extras = {} } = {}) {
  const mode = normalizeImageMode(imageMode);
  const payload = { image_mode: mode };
  const extra = imageExtrasPayload(mode, extras);
  if (extra.error) return { error: extra.error };
  Object.assign(payload, extra);
  const parseDim = (raw, label) => {
    const trimmed = String(raw ?? "").trim();
    if (!trimmed) return { empty: true };
    const val = Number(trimmed);
    if (!Number.isFinite(val) || val <= 0 || val > IMAGE_DIMENSION_LIMIT_MM) {
      return { error: `${label}要是大于 0、不超过 ${IMAGE_DIMENSION_LIMIT_MM} mm 的数字（留空则自动推算）` };
    }
    return { value: Math.round(val * 100) / 100 };
  };
  const w = parseDim(widthMm, "成品宽度");
  if (w.error) return { error: w.error };
  if (!w.empty) payload.width_mm = w.value;

  const h = parseDim(heightMm, "成品高度");
  if (h.error) return { error: h.error };
  if (!h.empty) payload.height_mm = h.value;

  return payload;
}

/** 自定义参数 → 生成请求里的 `manual_params`；没开自定义返回 null，填错返回 { error }。 */
export function manualParamsPayload({ enabled, powerPercent, feedRate, passes }) {
  if (!enabled) return null;
  const power = Number(powerPercent);
  const feed = Number(feedRate);
  const count = Number(passes);
  if (!Number.isFinite(power) || power < MANUAL_LIMITS.power.min || power > MANUAL_LIMITS.power.max) {
    return { error: "功率要在 1 ~ 100% 之间" };
  }
  if (!Number.isInteger(feed) || feed < MANUAL_LIMITS.feed.min || feed > MANUAL_LIMITS.feed.max) {
    return { error: "速度要是 100 ~ 6000 mm/min 的整数" };
  }
  if (!Number.isInteger(count) || count < MANUAL_LIMITS.passes.min || count > MANUAL_LIMITS.passes.max) {
    return { error: "遍数要是 1 ~ 5 的整数" };
  }
  return { power_percent: power, feed_rate: feed, passes: count };
}

/** 任务记录里这次实际用的参数，一行文字；没有参数返回空串。 */
export function actualParamsText(workflow) {
  const params = workflow?.artifacts?.actual_params;
  const power = Number(params?.laser_max_power);
  const feed = Number(params?.feed_rate);
  if (!Number.isFinite(power) || !Number.isFinite(feed)) return "";
  const scale = Number(params?.power_scale) > 0 ? Number(params.power_scale) : 1000;
  const passes = Number(params?.passes) || 1;
  const source = workflow?.regenerated_from ? "反馈调参" : workflow?.input?.manual_params ? "本次自定义" : "材料库推荐";
  const percent = Number((power * 100 / scale).toFixed(3));
  const base = `功率 ${percent}% · ${Math.round(feed)} mm/min · ${passes} 遍（${source}）`;
  const fillLabel = TEXT_FILL_LABELS[workflow?.artifacts?.text_fill];
  return fillLabel ? `${fillLabel} · ${base}` : base;
}

/**
 * 计算左侧所有会影响加工刀路的输入签名。
 * 生成预览时记下签名；之后用户只要改了文字、字高、刻法、起点、图片、尺寸、材料、厚度或自定义参数，
 * 当前签名就会变化，用于标记旧预览已过期并拦截误下发上一轮旧刀路。
 */
export function workbenchInputSignature({
  activeTool = "text",
  textContent = "",
  fontSizeMm = 12,
  textStyle = "outline",
  textOrigin = "center",
  fillSignature = "",
  engraveRoute = "vector",
  uploadedImagePath = "",
  imageWidthMm = "",
  imageHeightMm = "",
  imageMode = "auto",
  imageExtras = DEFAULT_IMAGE_EXTRAS,
  scanSignature = "",
  uploadedGcodePath = "",
  selectedMaterial = "",
  selectedThickness = "",
  customParams = false,
  customPower = "",
  customFeed = "",
  customPasses = "",
  presetParams = "",
} = {}) {
  const base = {
    tool: activeTool,
    material: String(selectedMaterial || ""),
    thickness: String(selectedThickness || ""),
    manual:
      activeTool !== "gcode" && Boolean(customParams)
        ? `${String(customPower).trim()}/${String(customFeed).trim()}/${String(customPasses).trim()}`
        : activeTool !== "gcode" && presetParams
          ? `preset:${presetParams}`
          : "preset",
  };
  if (activeTool === "text") {
    return JSON.stringify({
      ...base,
      text: String(textContent || "").trim(),
      fontSizeMm: Number(fontSizeMm) || 0,
      textStyle: resolveTextStyle(textStyle).style,
      textOrigin: textOrigin === "lower_left" ? "lower_left" : "center",
      fill: String(fillSignature || ""),
      // 雕刻路线是全局开关，工作台不随请求发送；切换后旧预览必须作废
      route: normalizeEngraveRoute(engraveRoute),
    });
  }
  if (activeTool === "image") {
    return JSON.stringify({
      ...base,
      image: String(uploadedImagePath || ""),
      widthMm: String(imageWidthMm || "").trim(),
      heightMm: String(imageHeightMm || "").trim(),
      imageMode: normalizeImageMode(imageMode),
      // 反相、直通、点宽补偿与当前模式的专属项（隐藏的其他模式输入不影响签名）
      extras: imageExtrasSignature(normalizeImageMode(imageMode), imageExtras),
      // 过扫描、双向扫描、扫描偏移与机器加速度（随请求发 fill_options 扫描子集）
      scan: String(scanSignature || ""),
    });
  }
  if (activeTool === "gcode") {
    return JSON.stringify({
      tool: "gcode",
      gcode: String(uploadedGcodePath || ""),
      material: String(selectedMaterial || ""),
      thickness: String(selectedThickness || ""),
    });
  }
  return JSON.stringify(base);
}

function imageExtrasSignature(imageMode, extras = {}) {
  const fields = imageModeFields(imageMode);
  const text = (value) => String(value ?? "").trim();
  return [
    extras.negative ? "neg" : "",
    extras.passThrough ? "pass" : "",
    text(extras.dotWidthMm),
    `x${intervalScaleValue(extras.intervalScale)}`,
    fields.halftone ? `${text(extras.halftoneCellMm)}@${text(extras.halftoneAngleDeg)}` : "",
    fields.grayscale ? text(extras.grayMinPowerPercent) : "",
  ].join("|");
}

/** 判断已生成的预览相对当前左侧表单是否已过期。 */
export function isWorkbenchPreviewStale(previewSignature, currentSignature) {
  if (!previewSignature) return false;
  return previewSignature !== currentSignature;
}

/** 按反馈实际采用的参数回填自定义表单，并按这些输入保存预览签名。 */
export function workbenchFeedbackInputs(workflow, inputs) {
  if (!workflow?.regenerated_from || inputs.activeTool === "gcode") return null;
  const actual = workflow.artifacts?.actual_params;
  const scale = Number(actual?.power_scale) > 0 ? Number(actual.power_scale) : 1000;
  const power = Number(actual?.laser_max_power) * 100 / scale;
  const feed = Number(actual?.feed_rate);
  const passes = Number(actual?.passes ?? 1);
  if (![power, feed, passes].every(Number.isFinite) || power <= 0 || feed <= 0 || passes <= 0) return null;
  // 旧建议也要如实显示。若功率低于手动范围，下一次生成由表单的现有校验明示。
  const fields = { customParams: true, customPower: String(power), customFeed: String(feed), customPasses: String(passes) };
  return { ...fields, signature: workbenchInputSignature({ ...inputs, ...fields }) };
}

/**
 * 工作台采用派生任务（调参反馈 / 两点对位）前的门禁校验：
 * 必须匹配当前任务且原预览未过期；通过时沿用原 `previewSignature`，不得用当前新输入给旧快照盖章。
 */
export function checkDerivedWorkbenchAdoption({
  workflow,
  currentWorkflowId,
  currentWorkflow,
  previewSignature,
  previewStale,
  label = "派生预览",
}) {
  if (!workflow?.workflow_id) return { ok: false, reason: `${label}记录无效` };
  const parentId = workflow.regenerated_from || workflow.registered_from || "";
  const expectedParent = workflow.registered_from && currentWorkflow?.registration
    ? currentWorkflow.registered_from || currentWorkflow.workflow_id
    : currentWorkflow?.workflow_id || currentWorkflowId;
  if (parentId && expectedParent && parentId !== expectedParent) {
    return { ok: false, reason: `${label}不是当前任务生成的，已忽略` };
  }
  if (previewStale || !previewSignature) {
    return {
      ok: false,
      reason: `输入、材料或参数已改动，旧预览已过期，已拦截采用${label}；请先重新生成轨迹。`,
    };
  }
  return { ok: true, inheritedSignature: previewSignature };
}

/** 加工监视舱进度栏右侧的大白话状态文案（替代原先裸露的英文大写）。 */
export function workbenchStatusLabel(overlayStatus, phase, hasWorkflow = false) {
  if (overlayStatus === "preparing") return "握手与安全预检中…";
  if (overlayStatus === "running" || overlayStatus === "sending") {
    return phase === "awaiting_completion" ? "等待机器执行完成" : "正在雕刻下发";
  }
  if (overlayStatus === "completed") return "雕刻已完成";
  if (overlayStatus === "failed") return "下发未完成";
  if (overlayStatus === "cancelled") return "已暂停取消（可续雕）";
  return hasWorkflow ? "就绪 · 等待开刻" : "等待计算轨迹";
}

/** 构造点击「开始雕刻」时的二次确认弹窗内容；相同物理 SHA-256 再次下发时追加防重刻烧穿提醒。 */
export function workbenchConfirmSendPrompt({
  workflow,
  material,
  thickness,
  targetLabel,
  dimensions,
  lastSentSha256 = "",
} = {}) {
  const sha256 = String(
    workflow?.confirmation_snapshot?.gcode_sha256 || workflow?.artifacts?.gcode_sha256 || "",
  ).trim();
  const isRepeat = Boolean(sha256 && lastSentSha256 && sha256 === lastSentSha256);
  const sizeLine =
    dimensions && Number.isFinite(Number(dimensions.width)) && Number.isFinite(Number(dimensions.height))
      ? `轮廓尺寸：${Number(dimensions.width).toFixed(1)} × ${Number(dimensions.height).toFixed(1)} mm`
      : "";
  const paramsLine = actualParamsText(workflow);
  const originLine = dimensions?.originNote || workflow?.artifacts?.origin_note || "";
  const lines = [
    `目标设备：${targetLabel || "当前连接设备"}`,
    `加工材质：${material || "—"} ${thickness ? `${thickness} mm` : ""}`.trim(),
    sizeLine,
    paramsLine ? `加工参数：${paramsLine}` : "",
    originLine ? `对刀起点：${originLine}` : "",
    "请确认我在机器旁、材料已放稳、排风已开启且佩戴护目镜。",
  ].filter(Boolean);
  if (isRepeat) {
    lines.unshift("⚠️ 注意：这份刀路与上次已下发内容完全相同！请确认已挪动材料或更换木牌，防止在原位置重复烧穿。");
  }
  return {
    title: isRepeat ? "相同刀路再次雕刻确认" : "确认开始雕刻",
    message: lines.join("\n"),
    confirmText: isRepeat ? "已挪动材料，确认开刻" : "确认开刻",
    danger: isRepeat,
    sha256,
  };
}

/** 工作台连接参数 → 请求里的连接字段（波特率、Telnet 端口按数字）。握手、开刻、续雕共用。 */
export function workbenchConnectionFields({ connectionMode, serialPort, baudrate, networkHost, telnetPort }) {
  return {
    connection_mode: connectionMode,
    port: serialPort,
    baudrate: Number(baudrate),
    network_host: networkHost,
    network_telnet_port: Number(telnetPort),
  };
}

/** 与后端 `send_result.target` 同格式的规范目标：串口 `端口@波特率`，网口 `主机:端口`。 */
export function workbenchCanonicalTarget({ connectionMode, serialPort, baudrate, networkHost, telnetPort }) {
  return connectionMode === "serial"
    ? `${serialPort}@${Number(baudrate) || 115200}`
    : `${networkHost}:${Number(telnetPort) || 23}`;
}

/**
 * 开刻 / 续雕请求断连、超时或抛错（结果未知）时的发送进度：后端可能已开始加工，已有目标的进度原样保留
 * （急停仍按原通道），否则补上规范目标，保证顶栏急停有处可发。
 */
export function sendProgressForUnknownResult(previous, fallbackTotal, target) {
  if (previous?.target) return previous;
  return { sent: previous?.sent ?? 0, total: previous?.total ?? fallbackTotal, target };
}

/** 构造点击「断点续雕」时的二次确认弹窗内容，防止误触直接出光或丢步错位。 */
export function workbenchResumeSendPrompt({
  workflow,
  material,
  thickness,
  targetLabel,
  startLine = 1,
  totalLines = 0,
} = {}) {
  const safeStart = Math.max(1, Number(startLine) || 1);
  const safeTotal = Number(totalLines) || 0;
  const paramsLine = actualParamsText(workflow);
  const lines = [
    `目标设备：${targetLabel || "当前连接设备"}`,
    `加工材质：${material || "—"} ${thickness ? `${thickness} mm` : ""}`.trim(),
    `续雕位置：从第 ${safeStart}${safeTotal > 0 ? ` / ${safeTotal}` : ""} 行继续下发（自动先发 $X 解锁）`,
    paramsLine ? `加工参数：${paramsLine}` : "",
    "请现场核实：材料没有挪动、急停或断连没有造成激光头丢步偏移、排风已开启且佩戴护目镜；若坐标系已偏请点取消并重新对刀生成预览。",
  ].filter(Boolean);
  return {
    title: "确认断点续雕",
    message: lines.join("\n"),
    confirmText: `确认从第 ${safeStart} 行续雕`,
    danger: false,
  };
}
