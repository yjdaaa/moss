// 矢量填充与走刀设置（2026-11-16，对齐 LightBurn Fill 模式与优化设置）。
// 工作台文字（实心 / 排线填充）与创意画板（矢量模式）共用一份，存在本机浏览器里：
// 扫描偏移、机器加速度这类“跟机器走”的值不用每次重填。提交时转成后端 `fill_options` / `path_options`。
// 同日补充（对齐 LightBurn）：双向扫描偏移可按“速度 → 偏移”表填写（`scan_offset_table`）；工作台文字选
// “外框切割”时另有切割设置（切缝补偿、桥位、引入 / 引出线、过切，转成 `cut_options`，默认全关）。
// 创意画板只生成雕刻任务，不带切割设置。
// 2026-11-17（技术书 r45）：位图（工作台图片、画板转图片）也用这份设置里的扫描子集（`rasterScanPayload`）。

import {
  MACHINE_MOTION_EVENT,
  MACHINE_MOTION_KEY,
  MAX_SAVED_TARGETS,
  connectionTargetKey,
} from "./deviceConnection.js";
export { MACHINE_MOTION_KEY };

export const FILL_SETTINGS_KEY = "moss.fillSettings.v1";

export const FILL_GROUPING_OPTIONS = [
  { value: "all", label: "一次填完（每行横穿所有图形，速度均匀，默认）" },
  { value: "individual", label: "逐块填充（一块块扫完，空白多时更快）" },
];

export const TAB_MODE_OPTIONS = [
  { value: "off", label: "不加桥位" },
  { value: "count", label: "按个数" },
  { value: "spacing", label: "按间距" },
];

/** 速度 → 偏移表最多几行（与后端一致）。 */
export const MAX_SCAN_OFFSET_ROWS = 32;

export const DEFAULT_FILL_SETTINGS = Object.freeze({
  grouping: "all",
  angleDeg: "0",
  crossHatch: false,
  bidirectional: true,
  overscanAuto: true,
  overscanMm: "2",
  accelerationMmS2: "",
  scanOffsetMm: "0",
  // [{ speed, offset }]，字符串；空表 = 用固定偏移
  scanOffsetTable: Object.freeze([]),
  preferCorners: true,
  removeOverlaps: true,
  // 切割设置（只在外框切割时提交），默认全关
  kerfMm: "0",
  tabMode: "off",
  tabCount: "4",
  tabSpacingMm: "50",
  tabWidthMm: "0.5",
  tabsSkipInner: true,
  leadInMm: "0",
  leadOutMm: "0",
  leadAngleDeg: "90",
  overcutMm: "0",
});

function normalizeOffsetTable(raw) {
  if (!Array.isArray(raw)) return [];
  return raw
    .slice(0, MAX_SCAN_OFFSET_ROWS)
    .filter((row) => row && typeof row === "object")
    .map((row) => ({ speed: text(row.speed, ""), offset: text(row.offset, "") }));
}

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
    scanOffsetTable: normalizeOffsetTable(source.scanOffsetTable),
    preferCorners: bool(source.preferCorners, d.preferCorners),
    removeOverlaps: bool(source.removeOverlaps, d.removeOverlaps),
    kerfMm: text(source.kerfMm, d.kerfMm),
    tabMode: TAB_MODE_OPTIONS.some((opt) => opt.value === source.tabMode) ? source.tabMode : d.tabMode,
    tabCount: text(source.tabCount, d.tabCount),
    tabSpacingMm: text(source.tabSpacingMm, d.tabSpacingMm),
    tabWidthMm: text(source.tabWidthMm, d.tabWidthMm),
    tabsSkipInner: bool(source.tabsSkipInner, d.tabsSkipInner),
    leadInMm: text(source.leadInMm, d.leadInMm),
    leadOutMm: text(source.leadOutMm, d.leadOutMm),
    leadAngleDeg: text(source.leadAngleDeg, d.leadAngleDeg),
    overcutMm: text(source.overcutMm, d.overcutMm),
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

/** 速度 → 偏移表：去掉整行留空的，校验后按速度从小到大；没有有效行返回 `{ value: null }`。 */
export function offsetTablePayload(rows) {
  const filled = normalizeOffsetTable(rows).filter((row) => row.speed.trim() !== "" || row.offset.trim() !== "");
  if (!filled.length) return { value: null };
  const table = [];
  for (const [index, row] of filled.entries()) {
    const speed = numberField(row.speed, `偏移表第 ${index + 1} 行的速度`, { min: 1, max: 1000000 });
    if (speed.error) return { error: speed.error };
    const offset = numberField(row.offset, `偏移表第 ${index + 1} 行的偏移`, { min: -2, max: 2 });
    if (offset.error) return { error: offset.error };
    table.push([speed.value, offset.value]);
  }
  table.sort((a, b) => a[0] - b[0]);
  if (table.some((row, i) => i > 0 && row[0] === table[i - 1][0])) {
    return { error: "偏移表里有重复的速度" };
  }
  return { value: table };
}

/** 切割设置 → 后端 `cut_options`；填写有误返回 `{ error }`。 */
export function cutOptionsPayload(settings) {
  const s = normalizeFillSettings(settings);
  const kerf = numberField(s.kerfMm, "切缝补偿", { min: -2, max: 2, allowEmpty: true });
  if (kerf.error) return { error: kerf.error };
  const leadIn = numberField(s.leadInMm, "引入线长度", { min: 0, max: 10, allowEmpty: true });
  if (leadIn.error) return { error: leadIn.error };
  const leadOut = numberField(s.leadOutMm, "引出线长度", { min: 0, max: 10, allowEmpty: true });
  if (leadOut.error) return { error: leadOut.error };
  const hasLead = (leadIn.value ?? 0) > 0 || (leadOut.value ?? 0) > 0;
  const angle = hasLead ? numberField(s.leadAngleDeg, "引线角度", { min: 1, max: 90, allowEmpty: true }) : { value: 90 };
  if (angle.error) return { error: angle.error };
  const overcut = numberField(s.overcutMm, "过切", { min: 0, max: 10, allowEmpty: true });
  if (overcut.error) return { error: overcut.error };
  const cut = {
    kerf_mm: kerf.value ?? 0,
    lead_in_mm: leadIn.value ?? 0,
    lead_out_mm: leadOut.value ?? 0,
    lead_angle_deg: angle.value ?? 90,
    overcut_mm: overcut.value ?? 0,
  };
  if (s.tabMode !== "off") {
    const width = numberField(s.tabWidthMm, "桥位宽度", { min: 0.05, max: 10 });
    if (width.error) return { error: width.error };
    if (s.tabMode === "count") {
      const count = numberField(s.tabCount, "桥位个数", { min: 1, max: 100 });
      if (count.error) return { error: count.error };
      if (!Number.isInteger(count.value)) return { error: "桥位个数须为整数" };
      cut.tab_count = count.value;
    } else {
      const spacing = numberField(s.tabSpacingMm, "桥位间距", { min: 1, max: 1000 });
      if (spacing.error) return { error: spacing.error };
      cut.tab_spacing_mm = spacing.value;
    }
    cut.tab_width_mm = width.value;
    cut.tabs_skip_inner = s.tabsSkipInner;
  }
  return { value: cut };
}

/**
 * 提交给后端的字段；`withFill=false`（只描轮廓 / 切割）时不带填充选项，`withCut=true`（外框切割）时带切割设置。
 * 填写有误返回 `{ error }`。
 */
export function fillSettingsPayload(settings, { withFill = true, withCut = false } = {}) {
  const s = normalizeFillSettings(settings);
  const path_options = { prefer_corners: s.preferCorners, remove_overlaps: s.removeOverlaps };
  const cut = withCut ? cutOptionsPayload(s) : null;
  if (cut?.error) return { error: cut.error };
  const cutPart = cut ? { cut_options: cut.value } : {};
  if (!withFill) return { path_options, ...cutPart };
  const angle = numberField(s.angleDeg, "扫描角度", { min: -360, max: 360 });
  if (angle.error) return { error: angle.error };
  const scan = scanOptionsPart(s);
  if (scan.error) return { error: scan.error };
  const fill_options = {
    grouping: s.grouping,
    angle_deg: angle.value,
    cross_hatch: s.crossHatch,
    ...scan.value,
  };
  return { fill_options, path_options, ...cutPart };
}

/** 逐行扫描共用的部分：双向、扫描偏移（含速度表）、过扫描（自动时带机器加速度）。 */
function scanOptionsPart(s) {
  const scan = { bidirectional: s.bidirectional, scan_offset_mm: 0 };
  if (s.bidirectional) {
    const offset = numberField(s.scanOffsetMm, "双向扫描偏移", { min: -2, max: 2, allowEmpty: true });
    if (offset.error) return { error: offset.error };
    scan.scan_offset_mm = offset.value ?? 0;
    const table = offsetTablePayload(s.scanOffsetTable);
    if (table.error) return { error: table.error };
    if (table.value) scan.scan_offset_table = table.value;
  }
  if (s.overscanAuto) {
    scan.overscan_mm = "auto";
    const accel = numberField(s.accelerationMmS2, "机器加速度", { min: 10, max: 100000, allowEmpty: true });
    if (accel.error) return { error: accel.error };
    if (accel.value !== null) scan.acceleration_mm_s2 = accel.value;
  } else {
    const over = numberField(s.overscanMm, "过扫描距离", { min: 0, max: 20 });
    if (over.error) return { error: over.error };
    scan.overscan_mm = over.value;
  }
  return { value: scan };
}

/**
 * 位图（工作台图片、创作画板转图片）提交的 `fill_options` 扫描子集（2026-11-17 技术书 r45）：
 * 过扫描（自动按速度）、双向扫描、扫描偏移与速度表、机器加速度；不带填充分组 / 角度 / 交叉这些矢量项。
 * 填写有误返回 `{ error }`。
 */
export function rasterScanPayload(settings) {
  const scan = scanOptionsPart(normalizeFillSettings(settings));
  return scan.error ? { error: scan.error } : { fill_options: scan.value };
}

function activeScanSignatureValues(s) {
  return [
    s.bidirectional,
    s.bidirectional ? s.scanOffsetMm : "0",
    s.bidirectional ? s.scanOffsetTable : [],
    s.overscanAuto,
    s.overscanAuto ? "" : s.overscanMm,
    s.overscanAuto ? s.accelerationMmS2 : "",
  ];
}

function activeCutSignatureValues(s) {
  const hasLead = (Number(s.leadInMm) || 0) > 0 || (Number(s.leadOutMm) || 0) > 0;
  return [
    s.kerfMm,
    s.tabMode,
    s.tabMode === "count" ? s.tabCount : "",
    s.tabMode === "spacing" ? s.tabSpacingMm : "",
    s.tabMode !== "off" ? s.tabWidthMm : "",
    s.tabMode !== "off" ? s.tabsSkipInner : false,
    s.leadInMm,
    s.leadOutMm,
    hasLead ? s.leadAngleDeg : "",
    s.overcutMm,
  ];
}

/** 位图扫描设置的签名：只看当前真正生效的扫描子集键。 */
export function rasterScanSignature(settings) {
  const s = normalizeFillSettings(settings);
  return `scan:${JSON.stringify(activeScanSignatureValues(s))}`;
}

/** 影响刀路的签名：只计入当前打开且生效的项，隐藏项变化不误判旧预览过期。 */
export function fillSettingsSignature(settings, { withFill = true, withCut = false } = {}) {
  const s = normalizeFillSettings(settings);
  const cutSig = withCut ? `|cut:${JSON.stringify(activeCutSignatureValues(s))}` : "";
  if (!withFill) return `path:${s.preferCorners}/${s.removeOverlaps}${cutSig}`;
  const fillOnly = {
    grouping: s.grouping,
    angleDeg: s.angleDeg,
    crossHatch: s.crossHatch,
    scan: activeScanSignatureValues(s),
    preferCorners: s.preferCorners,
    removeOverlaps: s.removeOverlaps,
  };
  return JSON.stringify(fillOnly) + cutSig;
}

/** 切割设置的简短摘要片段（全关时为空数组）。 */
function cutSummaryParts(s) {
  const parts = [];
  const kerf = Number(s.kerfMm) || 0;
  if (kerf !== 0) parts.push(`切缝${kerf > 0 ? "外扩" : "内缩"} ${Math.abs(kerf)} mm`);
  if (s.tabMode === "count") parts.push(`桥位 ${s.tabCount} 个`);
  if (s.tabMode === "spacing") parts.push(`桥位每 ${s.tabSpacingMm} mm`);
  if ((Number(s.leadInMm) || 0) > 0) parts.push(`引入 ${s.leadInMm} mm`);
  if ((Number(s.leadOutMm) || 0) > 0) parts.push(`引出 ${s.leadOutMm} mm`);
  if ((Number(s.overcutMm) || 0) > 0) parts.push(`过切 ${s.overcutMm} mm`);
  return parts;
}

/** 折叠时显示的一行摘要。 */
export function fillSettingsSummary(settings, { withFill = true, withCut = false } = {}) {
  const s = normalizeFillSettings(settings);
  const parts = [];
  if (withCut) parts.push(...cutSummaryParts(s));
  if (withFill) {
    parts.push(s.grouping === "individual" ? "逐块" : "一次填完");
    const angle = Number(s.angleDeg) || 0;
    parts.push(angle === 0 ? "横扫" : angle === 90 ? "竖扫" : `${angle}°`);
    if (s.crossHatch) parts.push("交叉");
    parts.push(s.bidirectional ? "双向" : "单向");
    parts.push(s.overscanAuto ? "过扫描自动" : `过扫描 ${s.overscanMm || 0} mm`);
    if (s.bidirectional && offsetTablePayload(s.scanOffsetTable).value) parts.push("偏移按速度表");
  }
  if (!s.preferCorners) parts.push("起刀不限尖角");
  if (!s.removeOverlaps) parts.push("保留重复线");
  return parts.join(" · ") || "默认";
}

/** 预览结果里的实际切割设置（后端 `cut_options` 回显），转成一句说明；全关或没有时返回空串。 */
export function cutResultNote(cut) {
  if (!cut || typeof cut !== "object" || !cut.active) return "";
  const report = cut.report && typeof cut.report === "object" ? cut.report : {};
  const parts = [];
  const kerf = Number(cut.kerf_mm) || 0;
  if (kerf !== 0) parts.push(`切缝${kerf > 0 ? "外扩" : "内缩"} ${Math.abs(kerf).toFixed(2)} mm`);
  if (Number(report.tabs) > 0) parts.push(`桥位 ${Number(report.tabs)} 个`);
  const leads = (Number(report.lead_ins) || 0) + (Number(report.lead_outs) || 0);
  if (leads > 0) parts.push(`引线 ${leads} 条`);
  const overcut = Number(report.overcut_mm) || 0;
  if (overcut > 0) parts.push(`过切共 ${overcut.toFixed(1)} mm`);
  if (Array.isArray(cut.from_material) && cut.from_material.length) parts.push("部分设置取自材料库");
  if (typeof cut.note === "string" && cut.note) parts.push(cut.note);
  return parts.join("，");
}

/** 预览结果里的实际填充设置，转成一句说明（没有填充时返回空串）；带上切割设置的回显。 */
export function fillResultNote(fill, removedDuplicates = 0, cut = null) {
  const notes = [];
  if (fill && typeof fill === "object") {
    const over = Number(fill.overscan_mm);
    const overText = Number.isFinite(over) && over > 0
      ? `过扫描 ${over.toFixed(1)} mm${fill.overscan_auto ? "（按速度自动）" : ""}`
      : "无过扫描";
    let line = `${fill.grouping === "individual" ? "逐块填充" : "一次填完"}，${fill.bidirectional === false ? "单向" : "双向"}，${overText}`;
    const offset = Number(fill.scan_offset_mm);
    if (fill.scan_offset_source === "table" && Number.isFinite(offset)) {
      line += `，扫描偏移 ${offset.toFixed(3)} mm（按速度表）`;
    }
    if (Array.isArray(fill.from_material) && fill.from_material.length) line += "，部分设置取自材料库";
    notes.push(line);
  }
  const cutNote = cutResultNote(cut);
  if (cutNote) notes.push(cutNote);
  if (Number(removedDuplicates) > 0) notes.push(`去掉 ${Number(removedDuplicates)} 条重复线`);
  return notes.join("；");
}

// ---- 机器运动参数（2026-11-16，按加速度估算工时）----
// 握手（/api/connection/check）读到的 `machine_limits` 里的加速度 / 最大速度记在本机，生成预览时原样转发为
// `machine_motion`；填充设置里手填的“机器加速度”优先（两轴都用它）。

const MOTION_KEYS = ["x_accel_mm_s2", "y_accel_mm_s2", "x_max_rate_mm_per_min", "y_max_rate_mm_per_min", "junction_deviation_mm"];

/** 从握手结果的 `machine_limits` 取出运动参数；一个都没有时返回 null。 */
export function machineMotionFromLimits(limits) {
  if (!limits || typeof limits !== "object") return null;
  const motion = {};
  for (const key of MOTION_KEYS) {
    const value = Number(limits[key]);
    if (limits[key] !== null && limits[key] !== undefined && Number.isFinite(value) && value > 0) motion[key] = value;
  }
  return Object.keys(motion).length ? motion : null;
}

function parseMotionStoreObject(parsed) {
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { targets: {} };
  }
  if (!parsed.targets || typeof parsed.targets !== "object" || Array.isArray(parsed.targets)) {
    // 旧格式未绑定连接目标，丢弃以免切设备时沿用上一台机器的加速度
    return { targets: {} };
  }
  const entries = [];
  for (const [rawKey, entry] of Object.entries(parsed.targets)) {
    const key = String(rawKey || "").trim();
    const motion = machineMotionFromLimits(entry?.motion ?? entry);
    if (key && motion) {
      entries.push([
        key,
        {
          motion,
          updated_at: Number.isFinite(Number(entry?.updated_at)) ? Number(entry.updated_at) : 0,
        },
      ]);
    }
  }
  if (entries.length > MAX_SAVED_TARGETS) {
    entries.sort((a, b) => a[1].updated_at - b[1].updated_at);
    entries.splice(0, entries.length - MAX_SAVED_TARGETS);
  }
  return { targets: Object.fromEntries(entries) };
}

export function readMachineMotionStore(storage = globalThis.localStorage) {
  try {
    const raw = storage?.getItem?.(MACHINE_MOTION_KEY);
    return raw ? parseMotionStoreObject(JSON.parse(raw)) : { targets: {} };
  } catch {
    return { targets: {} };
  }
}

export function resolveMachineMotionForTarget(storeOrMotion, target) {
  const key = connectionTargetKey(target);
  if (!key || !storeOrMotion || typeof storeOrMotion !== "object") return null;
  if (storeOrMotion.targets && typeof storeOrMotion.targets === "object") {
    return machineMotionFromLimits(storeOrMotion.targets[key]?.motion) || null;
  }
  return null;
}

/** 读取指定连接目标保存的机器运动参数；目标为空或未记录时返回 null。 */
export function readMachineMotion(target, storage = globalThis.localStorage) {
  const key = connectionTargetKey(target);
  if (!key) return null;
  const store = readMachineMotionStore(storage);
  return resolveMachineMotionForTarget(store, key);
}

/**
 * 按连接目标写入机器运动参数：
 * - 握手未报运动参数（`motion` 为空）时清空该目标记录，不再沿用旧值；
 * - 最多保留 16 个连接目标，超出淘汰最旧项，并广播 `MACHINE_MOTION_EVENT`。
 */
export function writeMachineMotion(motion, target, storage = globalThis.localStorage) {
  const key = connectionTargetKey(target);
  if (!key) return null;
  const store = readMachineMotionStore(storage);
  const nextTargets = { ...store.targets };
  const clean = machineMotionFromLimits(motion);
  if (clean) {
    let maxTs = Date.now();
    for (const item of Object.values(nextTargets)) {
      const ts = Number(item?.updated_at) || 0;
      if (ts >= maxTs) maxTs = ts + 1;
    }
    nextTargets[key] = { motion: clean, updated_at: maxTs };
  } else {
    delete nextTargets[key];
  }
  const saved = parseMotionStoreObject({ targets: nextTargets });
  try {
    storage?.setItem?.(MACHINE_MOTION_KEY, JSON.stringify(saved));
  } catch {
    // 存储被禁用时只是不记住
  }
  try {
    if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
      window.dispatchEvent(new CustomEvent(MACHINE_MOTION_EVENT, { detail: saved }));
    }
  } catch {
    // 非浏览器环境忽略广播
  }
  return clean;
}

/** 提交给后端的 `machine_motion`；没有任何值时返回空对象（后端按默认 1000 mm/s² 估算）。 */
export function machineMotionPayload(settings, stored) {
  const manual = Number(String(normalizeFillSettings(settings).accelerationMmS2 ?? "").trim() || NaN);
  const motion = { ...(machineMotionFromLimits(stored) || {}) };
  if (Number.isFinite(manual) && manual >= 10 && manual <= 100000) {
    motion.x_accel_mm_s2 = manual;
    motion.y_accel_mm_s2 = manual;
  }
  return Object.keys(motion).length ? { machine_motion: motion } : {};
}

/** 工时自校准说明（r40）：`time_estimate.calibration` 存在时返回“已按最近 N 次实际用时校准”。 */
export function timeCalibrationNote(estimate) {
  const cal = estimate && typeof estimate === "object" ? estimate.calibration : null;
  const samples = Number(cal?.samples);
  const factor = Number(cal?.factor);
  if (!cal || !Number.isFinite(samples) || samples <= 0 || !Number.isFinite(factor)) return "";
  const raw = Number(estimate.raw_estimated_seconds);
  const rawText = Number.isFinite(raw) ? `，原估 ${Math.round(raw)} 秒` : "";
  return `已按最近 ${samples} 次实际用时校准（×${factor.toFixed(2)}${rawText}）`;
}

/** 工时估算说明：用了多大的加速度、来自哪里；校准过时再加一句。 */
export function timeEstimateNote(estimate) {
  if (!estimate || estimate.model !== "acceleration") return "";
  const x = Number(estimate.acceleration_mm_s2?.x);
  const y = Number(estimate.acceleration_mm_s2?.y);
  if (!Number.isFinite(x) || !Number.isFinite(y)) return "";
  const accel = x === y ? `${x}` : `X ${x} / Y ${y}`;
  const base = estimate.acceleration_source === "machine"
    ? `按机器加速度 ${accel} mm/s² 估算加减速`
    : `按默认加速度 ${accel} mm/s² 估算加减速（连接机器后自动读取实际值）`;
  const cal = timeCalibrationNote(estimate);
  return cal ? `${base}；${cal}` : base;
}

// ---- 随材料保存的填充 / 切割设置（r40，2026-11-16，对齐 LightBurn 库条目保存整套图层设置）----
// 扫描角度、分组、交叉、双向、过扫描与切割设置跟着材料走，存进材料库条目；
// 双向扫描偏移（含速度表）、机器加速度与走刀偏好跟机器 / 本人走，仍只存在本机。

const SETTING_LABELS = {
  grouping: "填充分组",
  angleDeg: "扫描角度",
  crossHatch: "交叉排线",
  bidirectional: "双向扫描",
  overscanAuto: "过扫描",
  overscanMm: "过扫描",
  kerfMm: "切缝补偿",
  tabMode: "桥位",
  tabCount: "桥位",
  tabSpacingMm: "桥位",
  tabWidthMm: "桥位宽度",
  tabsSkipInner: "孔不加桥位",
  leadInMm: "引入线",
  leadOutMm: "引出线",
  leadAngleDeg: "引线角度",
  overcutMm: "过切",
};

const isObj = (value) => Boolean(value) && typeof value === "object" && !Array.isArray(value);
const finite = (value) => value !== null && value !== undefined && value !== "" && Number.isFinite(Number(value));

/** 材料库条目回显的 `fill_options` / `cut_options`（后端字段名）→ 面板设置的部分值。 */
export function settingsFromMaterialLayer(layer) {
  const out = {};
  const fill = isObj(layer?.fill_options) ? layer.fill_options : null;
  const cut = isObj(layer?.cut_options) ? layer.cut_options : null;
  if (fill) {
    if (FILL_GROUPING_OPTIONS.some((opt) => opt.value === fill.grouping)) out.grouping = fill.grouping;
    if (finite(fill.angle_deg)) out.angleDeg = String(fill.angle_deg);
    if (typeof fill.cross_hatch === "boolean") out.crossHatch = fill.cross_hatch;
    if (typeof fill.bidirectional === "boolean") out.bidirectional = fill.bidirectional;
    if (typeof fill.overscan_mm === "string" && fill.overscan_mm.trim().toLowerCase() === "auto") {
      out.overscanAuto = true;
    } else if (finite(fill.overscan_mm)) {
      out.overscanAuto = false;
      out.overscanMm = String(fill.overscan_mm);
    }
  }
  if (cut) {
    if (finite(cut.kerf_mm)) out.kerfMm = String(cut.kerf_mm);
    if (finite(cut.tab_count) && Number(cut.tab_count) > 0) {
      out.tabMode = "count";
      out.tabCount = String(cut.tab_count);
    } else if (finite(cut.tab_spacing_mm) && Number(cut.tab_spacing_mm) > 0) {
      out.tabMode = "spacing";
      out.tabSpacingMm = String(cut.tab_spacing_mm);
    } else if (finite(cut.tab_count) || finite(cut.tab_spacing_mm)) {
      out.tabMode = "off";
    }
    if (finite(cut.tab_width_mm)) out.tabWidthMm = String(cut.tab_width_mm);
    if (typeof cut.tabs_skip_inner === "boolean") out.tabsSkipInner = cut.tabs_skip_inner;
    if (finite(cut.lead_in_mm)) out.leadInMm = String(cut.lead_in_mm);
    if (finite(cut.lead_out_mm)) out.leadOutMm = String(cut.lead_out_mm);
    if (finite(cut.lead_angle_deg)) out.leadAngleDeg = String(cut.lead_angle_deg);
    if (finite(cut.overcut_mm)) out.overcutMm = String(cut.overcut_mm);
  }
  return out;
}

/**
 * 把材料库条目里的设置盖到本机设置上：返回 `{ settings, applied }`，`applied` 是取自材料库的面板键。
 * 条目没有图层设置时原样返回（applied 为空）。用户之后仍可改，改动只影响本次生成。
 */
export function applyMaterialLayer(base, layer) {
  const partial = settingsFromMaterialLayer(layer);
  return { settings: normalizeFillSettings({ ...normalizeFillSettings(base), ...partial }), applied: Object.keys(partial) };
}

/** “来自材料库”提示：列出载入了哪些项；没有时返回空串。 */
export function materialLayerHint(applied, entryLabel = "") {
  if (!Array.isArray(applied) || !applied.length) return "";
  const names = [...new Set(applied.map((key) => SETTING_LABELS[key]).filter(Boolean))];
  return `来自材料库${entryLabel ? `「${entryLabel}」` : ""}：${names.join("、")}（可改，只影响这次生成）`;
}

/**
 * “保存到材料库”要提交的值：雕刻条目存 `fill_options`（不含扫描偏移与加速度），切割条目存 `cut_options`
 * （不加桥位时记 `tab_count: 0`，载入时据此关掉桥位）。填写有误返回 `{ error }`。
 */
export function materialLayerPayload(settings, { withFill = true, withCut = false } = {}) {
  const s = normalizeFillSettings(settings);
  if (withCut) {
    const cut = cutOptionsPayload(s);
    if (cut.error) return { error: cut.error };
    const cut_options = { ...cut.value };
    if (s.tabMode === "off") cut_options.tab_count = 0;
    return { cut_options };
  }
  if (!withFill) return { error: "当前刻法没有可存进材料库的填充或切割设置" };
  const angle = numberField(s.angleDeg, "扫描角度", { min: -360, max: 360 });
  if (angle.error) return { error: angle.error };
  const fill_options = {
    grouping: s.grouping,
    angle_deg: angle.value,
    cross_hatch: s.crossHatch,
    bidirectional: s.bidirectional,
  };
  if (s.overscanAuto) {
    fill_options.overscan_mm = "auto";
  } else {
    const over = numberField(s.overscanMm, "过扫描距离", { min: 0, max: 20 });
    if (over.error) return { error: over.error };
    fill_options.overscan_mm = over.value;
  }
  return { fill_options };
}

/**
 * 推荐接口（`/api/ui/material-recommendation`）结果 → 面板用的材料库条目信息；
 * `key` 变了才需要重新载入（同一条目刷新不打断用户正在改的值）。
 */
export function materialLayerFromRecommendation(rec) {
  if (!isObj(rec) || !rec.material) return null;
  const surface = rec.match === "surface";
  const thickness = Number(rec.matched_thickness_mm ?? rec.thickness_mm);
  const laserMode = rec.laser_mode === "cut" ? "cut" : "engrave";
  const engravingMode = laserMode === "cut" ? "" : rec.engraving_mode === "outline" ? "outline" : "raster";
  const modeLabel = laserMode === "cut" ? "切割" : engravingMode === "outline" ? "轮廓" : "光栅";
  const layer = {
    material: rec.material,
    thicknessMm: Number.isFinite(thickness) ? thickness : null,
    surface,
    laserMode,
    engravingMode,
    label: `${rec.material} ${surface ? "表面通用" : `${Number.isFinite(thickness) ? thickness : "?"}mm`} ${modeLabel}`,
    fill_options: isObj(rec.fill_options) ? rec.fill_options : null,
    cut_options: isObj(rec.cut_options) ? rec.cut_options : null,
  };
  layer.key = JSON.stringify([layer.label, layer.fill_options, layer.cut_options]);
  return layer;
}

/** `save_layer_options` 请求体（材料实验室管理接口）。 */
export function materialLayerSaveRequest(layer, payload) {
  return {
    action: "save_layer_options",
    material: layer.material,
    ...(layer.surface ? { surface: true } : { thickness_mm: layer.thicknessMm }),
    laser_mode: layer.laserMode,
    engraving_mode: layer.engravingMode,
    ...payload,
  };
}

/**
 * 用户改设置时写进本机存储的值：取自材料库的那些键保留本机原来的值（改动只影响这次生成，
 * 换到没有存设置的材料时仍回到自己上次的习惯值）；其余键照常记住。
 */
export function localSettingsToStore(next, stored, applied) {
  const keep = Array.isArray(applied) ? applied : [];
  if (!keep.length) return normalizeFillSettings(next);
  const base = normalizeFillSettings(stored);
  const out = { ...normalizeFillSettings(next) };
  for (const key of keep) {
    if (key in base) out[key] = base[key];
  }
  return out;
}
