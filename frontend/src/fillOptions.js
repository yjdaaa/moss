// 矢量填充与走刀设置（2026-11-16，对齐 LightBurn Fill 模式与优化设置）。
// 工作台文字（实心 / 排线填充）与创意画板（矢量模式）共用一份，存在本机浏览器里：
// 扫描偏移、机器加速度这类“跟机器走”的值不用每次重填。提交时转成后端 `fill_options` / `path_options`。

export const FILL_SETTINGS_KEY = "moss.fillSettings.v1";

export const FILL_GROUPING_OPTIONS = [
  { value: "all", label: "一次填完（每行横穿所有图形，速度均匀，默认）" },
  { value: "individual", label: "逐块填充（一块块扫完，空白多时更快）" },
];

export const DEFAULT_FILL_SETTINGS = Object.freeze({
  grouping: "all",
  angleDeg: "0",
  crossHatch: false,
  bidirectional: true,
  overscanAuto: true,
  overscanMm: "2",
  accelerationMmS2: "",
  scanOffsetMm: "0",
  preferCorners: true,
  removeOverlaps: true,
});

const text = (value, fallback) => (value === undefined || value === null ? fallback : String(value));
const bool = (value, fallback) => (typeof value === "boolean" ? value : fallback);

/** 把任意来源（存储、旧版本）的设置收成完整、类型正确的对象。 */
export function normalizeFillSettings(raw) {
  const source = raw && typeof raw === "object" ? raw : {};
  const d = DEFAULT_FILL_SETTINGS;
  return {
    grouping: FILL_GROUPING_OPTIONS.some((opt) => opt.value === source.grouping) ? source.grouping : d.grouping,
    angleDeg: text(source.angleDeg, d.angleDeg),
    crossHatch: bool(source.crossHatch, d.crossHatch),
    bidirectional: bool(source.bidirectional, d.bidirectional),
    overscanAuto: bool(source.overscanAuto, d.overscanAuto),
    overscanMm: text(source.overscanMm, d.overscanMm),
    accelerationMmS2: text(source.accelerationMmS2, d.accelerationMmS2),
    scanOffsetMm: text(source.scanOffsetMm, d.scanOffsetMm),
    preferCorners: bool(source.preferCorners, d.preferCorners),
    removeOverlaps: bool(source.removeOverlaps, d.removeOverlaps),
  };
}

export function readFillSettings(storage = globalThis.localStorage) {
  try {
    const raw = storage?.getItem(FILL_SETTINGS_KEY);
    return normalizeFillSettings(raw ? JSON.parse(raw) : null);
  } catch {
    return normalizeFillSettings(null);
  }
}

export function writeFillSettings(settings, storage = globalThis.localStorage) {
  try {
    storage?.setItem(FILL_SETTINGS_KEY, JSON.stringify(normalizeFillSettings(settings)));
  } catch {
    // 存储被禁用时只是不记住，不影响本次生成
  }
}

function numberField(value, label, { min, max, allowEmpty = false } = {}) {
  const raw = String(value ?? "").trim();
  if (raw === "") return allowEmpty ? { value: null } : { error: `请填写${label}` };
  const num = Number(raw);
  if (!Number.isFinite(num) || num < min || num > max) {
    return { error: `${label}须在 ${min} ~ ${max} 之间` };
  }
  return { value: num };
}

/**
 * 提交给后端的字段；`withFill=false`（只描轮廓 / 切割）时只带走刀选项。
 * 填写有误返回 `{ error }`。
 */
export function fillSettingsPayload(settings, { withFill = true } = {}) {
  const s = normalizeFillSettings(settings);
  const path_options = { prefer_corners: s.preferCorners, remove_overlaps: s.removeOverlaps };
  if (!withFill) return { path_options };
  const angle = numberField(s.angleDeg, "扫描角度", { min: -360, max: 360 });
  if (angle.error) return { error: angle.error };
  const offset = numberField(s.scanOffsetMm, "双向扫描偏移", { min: -2, max: 2, allowEmpty: true });
  if (offset.error) return { error: offset.error };
  const fill_options = {
    grouping: s.grouping,
    angle_deg: angle.value,
    cross_hatch: s.crossHatch,
    bidirectional: s.bidirectional,
    scan_offset_mm: offset.value ?? 0,
  };
  if (s.overscanAuto) {
    fill_options.overscan_mm = "auto";
    const accel = numberField(s.accelerationMmS2, "机器加速度", { min: 10, max: 100000, allowEmpty: true });
    if (accel.error) return { error: accel.error };
    if (accel.value !== null) fill_options.acceleration_mm_s2 = accel.value;
  } else {
    const over = numberField(s.overscanMm, "过扫描距离", { min: 0, max: 20 });
    if (over.error) return { error: over.error };
    fill_options.overscan_mm = over.value;
  }
  return { fill_options, path_options };
}

/** 影响刀路的签名：改了任何一项，旧预览就算过期。 */
export function fillSettingsSignature(settings, { withFill = true } = {}) {
  const s = normalizeFillSettings(settings);
  if (!withFill) return `path:${s.preferCorners}/${s.removeOverlaps}`;
  return JSON.stringify(s);
}

/** 折叠时显示的一行摘要。 */
export function fillSettingsSummary(settings, { withFill = true } = {}) {
  const s = normalizeFillSettings(settings);
  const parts = [];
  if (withFill) {
    parts.push(s.grouping === "individual" ? "逐块" : "一次填完");
    const angle = Number(s.angleDeg) || 0;
    parts.push(angle === 0 ? "横扫" : angle === 90 ? "竖扫" : `${angle}°`);
    if (s.crossHatch) parts.push("交叉");
    parts.push(s.bidirectional ? "双向" : "单向");
    parts.push(s.overscanAuto ? "过扫描自动" : `过扫描 ${s.overscanMm || 0} mm`);
  }
  if (!s.preferCorners) parts.push("起刀不限尖角");
  if (!s.removeOverlaps) parts.push("保留重复线");
  return parts.join(" · ") || "默认";
}

/** 预览结果里的实际填充设置，转成一句说明（没有填充时返回空串）。 */
export function fillResultNote(fill, removedDuplicates = 0) {
  const notes = [];
  if (fill && typeof fill === "object") {
    const over = Number(fill.overscan_mm);
    const overText = Number.isFinite(over) && over > 0
      ? `过扫描 ${over.toFixed(1)} mm${fill.overscan_auto ? "（按速度自动）" : ""}`
      : "无过扫描";
    notes.push(`${fill.grouping === "individual" ? "逐块填充" : "一次填完"}，${fill.bidirectional === false ? "单向" : "双向"}，${overText}`);
  }
  if (Number(removedDuplicates) > 0) notes.push(`去掉 ${Number(removedDuplicates)} 条重复线`);
  return notes.join("；");
}
