// n73（2026-11-08 用户决定）：工作台“这次自定义功率和速度”。只影响这一次生成的刀路，不写回材料库；
// 范围与后端 `manual_params` 校验一致（功率 1~100%、速度 100~6000 mm/min、遍数 1~5）。
// 2026-11-11 增补：文字刻法映射（对齐后端 `text_fill`）、图片尺寸/算法校验、左侧参数改动后的预览过期判定与开刻前确认文案。

export const MANUAL_LIMITS = {
  power: { min: 1, max: 100 },
  feed: { min: 100, max: 6000 },
  passes: { min: 1, max: 5 },
};

export const IMAGE_DIMENSION_LIMIT_MM = 1000;

export const TEXT_STYLE_OPTIONS = [
  { value: "outline", label: "轮廓描边" },
  { value: "fill", label: "实心扫描填充" },
  { value: "hatch", label: "矢量排线填充" },
  { value: "cut", label: "外框切割" },
];

export const TEXT_ORIGIN_OPTIONS = [
  { value: "center", label: "木牌中心 (默认)" },
  { value: "lower_left", label: "左下角" },
];

export const IMAGE_MODE_OPTIONS = [
  { value: "auto", label: "自动识别" },
  { value: "dither", label: "照片灰度抖动" },
  { value: "threshold", label: "清晰黑白线条" },
];

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
      return { style: "fill", label: "实心扫描填充", laserMode: "engrave", engravingMode: "raster", textFill: "fill" };
    case "hatch":
      return { style: "hatch", label: "矢量排线填充", laserMode: "engrave", engravingMode: "outline", textFill: "hatch" };
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

/** 图片预览可选尺寸与算法 -> `/api/workflow` preview 字段；留空不传尺寸，填错返回 `{ error }`。 */
export function imageParamsPayload({ widthMm = "", heightMm = "", imageMode = "auto" } = {}) {
  const mode = IMAGE_MODE_OPTIONS.some((opt) => opt.value === imageMode) ? imageMode : "auto";
  const payload = { image_mode: mode };
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
  const source = workflow?.input?.manual_params ? "本次自定义" : "材料库推荐";
  const base = `功率 ${Math.round((power / scale) * 100)}% · ${Math.round(feed)} mm/min · ${passes} 遍（${source}）`;
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
  uploadedImagePath = "",
  imageWidthMm = "",
  imageHeightMm = "",
  imageMode = "auto",
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
    });
  }
  if (activeTool === "image") {
    return JSON.stringify({
      ...base,
      image: String(uploadedImagePath || ""),
      widthMm: String(imageWidthMm || "").trim(),
      heightMm: String(imageHeightMm || "").trim(),
      imageMode: IMAGE_MODE_OPTIONS.some((opt) => opt.value === imageMode) ? imageMode : "auto",
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

/** 判断已生成的预览相对当前左侧表单是否已过期。 */
export function isWorkbenchPreviewStale(previewSignature, currentSignature) {
  if (!previewSignature) return false;
  return previewSignature !== currentSignature;
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
