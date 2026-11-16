// 质量预设“快速 / 均衡 / 精细”（2026-11-16 用户决定，技术书 r41）：先给三个预设，细项收进“高级设置”。
// 预设只是把下面列出的高级字段填成固定值，不另存状态：当前字段与哪个预设完全一致就显示哪个，
// 用户改了其中任何一项就显示“自定义”。预设字段本身进入预览签名，所以切换预设会让旧预览过期。
// “均衡”恰好等于各处原来的默认值，不点预设时行为与以前完全一致。

export const CUSTOM_PRESET = "custom";

export const QUALITY_PRESET_OPTIONS = [
  { value: "fast", label: "快速" },
  { value: "balanced", label: "均衡" },
  { value: "fine", label: "精细" },
];

/**
 * 矢量填充（工作台文字实心 / 排线填充、创意画板矢量模式）的预设，对齐 LightBurn Fill 的取舍：
 * - 快速：逐块填充（LightBurn “Fill shapes individually”，图形间空白多时少跑空行程），双向出光；
 * - 均衡：一次填完、双向出光（原默认）；
 * - 精细：只单向出光（来回错位不影响边缘）并加交叉排线（再转 90° 扫一遍，更实更匀），耗时约 3~4 倍。
 * 三档都按速度自动算过扫描。扫描角度、扫描偏移、机器加速度属于“跟机器 / 跟设计走”的值，预设不改。
 */
export const FILL_QUALITY_PRESETS = Object.freeze({
  fast: Object.freeze({ grouping: "individual", bidirectional: true, crossHatch: false, overscanAuto: true }),
  balanced: Object.freeze({ grouping: "all", bidirectional: true, crossHatch: false, overscanAuto: true }),
  fine: Object.freeze({ grouping: "all", bidirectional: false, crossHatch: true, overscanAuto: true }),
});

export const FILL_PRESET_HINTS = {
  fast: "逐块填充、双向出光：空白多的图案更快",
  balanced: "一次填完、双向出光（默认）",
  fine: "单向出光 + 交叉排线：更齐更实，约 3~4 倍时间",
};

/** 当前填充设置对应的预设；有任何一项与预设不同返回 "custom"。 */
export function detectFillPreset(settings) {
  const s = settings && typeof settings === "object" ? settings : {};
  for (const [key, preset] of Object.entries(FILL_QUALITY_PRESETS)) {
    if (Object.entries(preset).every(([field, value]) => s[field] === value)) return key;
  }
  return CUSTOM_PRESET;
}

/** 套用预设：只改预设列出的字段，其余（角度、偏移表、切割设置等）原样保留。 */
export function applyFillPreset(settings, presetKey) {
  const preset = FILL_QUALITY_PRESETS[presetKey];
  const base = settings && typeof settings === "object" ? settings : {};
  return preset ? { ...base, ...preset } : { ...base };
}

/**
 * 工作台图片的预设，对齐 LightBurn 图片雕刻的常用取舍（材料推荐线距默认 0.1 mm ≈ 254 DPI）：
 * - 快速：自动识别（照片抖动 / 线稿黑白），行间距 ×1.5（约 170 DPI，行数少三分之一）；
 * - 均衡：自动识别，行间距 ×1（原默认）；
 * - 精细：Jarvis 抖动（LightBurn 对照片的推荐；纯黑白线稿结果与黑白线条基本相同），行间距 ×0.8（约 318 DPI）。
 *   线距变密时同样功率会刻得更深，必要时用“这次自定义功率和速度”调低功率。
 * 行间距倍数提交为后端 `line_interval_scale`（为 1 时不提交，请求与以前完全相同）。
 */
export const IMAGE_QUALITY_PRESETS = Object.freeze({
  fast: Object.freeze({ imageMode: "auto", intervalScale: 1.5 }),
  balanced: Object.freeze({ imageMode: "auto", intervalScale: 1 }),
  fine: Object.freeze({ imageMode: "jarvis", intervalScale: 0.8 }),
});

export const IMAGE_PRESET_HINTS = {
  fast: "自动识别，行间距 ×1.5：更快、颗粒更粗",
  balanced: "自动识别，按材料推荐行间距（默认）",
  fine: "Jarvis 抖动，行间距 ×0.8：细节更多、更慢更深",
};

/** 行间距倍数输入框的原始值 → 数字；留空按 1。 */
export function intervalScaleValue(raw) {
  const trimmed = String(raw ?? "").trim();
  return trimmed === "" ? 1 : Number(trimmed);
}

/** 当前图片转换方式与行间距倍数对应的预设；不一致返回 "custom"。 */
export function detectImagePreset(imageMode, intervalScale) {
  const scale = intervalScaleValue(intervalScale);
  for (const [key, preset] of Object.entries(IMAGE_QUALITY_PRESETS)) {
    if (preset.imageMode === imageMode && preset.intervalScale === scale) return key;
  }
  return CUSTOM_PRESET;
}

/** 套用图片预设：返回新的转换方式与行间距倍数（输入框字符串）；未知预设返回 null。 */
export function applyImagePreset(presetKey) {
  const preset = IMAGE_QUALITY_PRESETS[presetKey];
  if (!preset) return null;
  return { imageMode: preset.imageMode, intervalScale: String(preset.intervalScale) };
}
