// 矢量填充与走刀设置（2026-11-16，对齐 LightBurn Fill 模式与优化设置）。
// 工作台文字（实心 / 排线填充）与创意画板（矢量模式）共用一份，存在本机浏览器里：
// 扫描偏移、机器加速度这类“跟机器走”的值不用每次重填。提交时转成后端 `fill_options` / `path_options`。
// 同日补充（对齐 LightBurn）：双向扫描偏移可按“速度 → 偏移”表填写（`scan_offset_table`）；工作台文字选
// “外框切割”时另有切割设置（切缝补偿、桥位、引入 / 引出线、过切，转成 `cut_options`，默认全关）。
// 创意画板只生成雕刻任务，不带切割设置。

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

const CUT_KEYS = ["kerfMm", "tabMode", "tabCount", "tabSpacingMm", "tabWidthMm", "tabsSkipInner", "leadInMm", "leadOutMm", "leadAngleDeg", "overcutMm"];

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
  const angle = numberField(s.leadAngleDeg, "引线角度", { min: 1, max: 90, allowEmpty: true });
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
  const offset = numberField(s.scanOffsetMm, "双向扫描偏移", { min: -2, max: 2, allowEmpty: true });
  if (offset.error) return { error: offset.error };
  const fill_options = {
    grouping: s.grouping,
    angle_deg: angle.value,
    cross_hatch: s.crossHatch,
    bidirectional: s.bidirectional,
    scan_offset_mm: offset.value ?? 0,
  };
  const table = offsetTablePayload(s.scanOffsetTable);
  if (table.error) return { error: table.error };
  if (table.value) fill_options.scan_offset_table = table.value;
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
  return { fill_options, path_options, ...cutPart };
}

/** 影响刀路的签名：改了任何一项，旧预览就算过期（切割设置只在外框切割时算进去）。 */
export function fillSettingsSignature(settings, { withFill = true, withCut = false } = {}) {
  const s = normalizeFillSettings(settings);
  const cutSig = withCut ? `|cut:${JSON.stringify(CUT_KEYS.map((key) => s[key]))}` : "";
  if (!withFill) return `path:${s.preferCorners}/${s.removeOverlaps}${cutSig}`;
  const fillOnly = Object.fromEntries(Object.entries(s).filter(([key]) => !CUT_KEYS.includes(key)));
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
    if (offsetTablePayload(s.scanOffsetTable).value) parts.push("偏移按速度表");
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

export const MACHINE_MOTION_KEY = "moss.machineMotion.v1";
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

export function readMachineMotion(storage = globalThis.localStorage) {
  try {
    const raw = storage?.getItem(MACHINE_MOTION_KEY);
    return raw ? machineMotionFromLimits(JSON.parse(raw)) : null;
  } catch {
    return null;
  }
}

export function writeMachineMotion(motion, storage = globalThis.localStorage) {
  const clean = machineMotionFromLimits(motion);
  if (!clean) return;
  try {
    storage?.setItem(MACHINE_MOTION_KEY, JSON.stringify(clean));
  } catch {
    // 存储被禁用时只是不记住
  }
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

/** 工时估算说明：用了多大的加速度、来自哪里。 */
export function timeEstimateNote(estimate) {
  if (!estimate || estimate.model !== "acceleration") return "";
  const x = Number(estimate.acceleration_mm_s2?.x);
  const y = Number(estimate.acceleration_mm_s2?.y);
  if (!Number.isFinite(x) || !Number.isFinite(y)) return "";
  const accel = x === y ? `${x}` : `X ${x} / Y ${y}`;
  return estimate.acceleration_source === "machine"
    ? `按机器加速度 ${accel} mm/s² 估算加减速`
    : `按默认加速度 ${accel} mm/s² 估算加减速（连接机器后自动读取实际值）`;
}
