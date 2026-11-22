// 首次标定向导（r40，2026-11-16 用户决定）：把现有测试按顺序串起来，每步“生成 → 用户确认发送实刻 →
// 填测量结果 / 选格 → MOSS 保存”，每步都可跳过，绝不自动发送。
// 进度存在本机浏览器：跟机器走的步骤（双向扫描偏移、工时）一份，跟材料走的步骤按“材料|厚度”各一份；
// 测出来的值存到该去的地方——线距、功率速度、过扫描、切缝补偿进材料库条目，扫描偏移表进本机填充设置。

export const WIZARD_STORAGE_KEY = "moss.calibrationWizard.v1";

export const WIZARD_STEPS = Object.freeze([
  {
    id: "interval",
    scope: "material",
    title: "线距测试",
    summary: "一排不同线距的填充小方块，选扫描线刚好相接的那块，线距写进材料库",
  },
  {
    id: "matrix",
    scope: "material",
    title: "功率 × 速度矩阵",
    summary: "用上一步的线距刻功率 × 速度网格，选效果最好的格子写进材料库",
  },
  {
    id: "scan_offset",
    scope: "machine",
    title: "双向扫描偏移",
    summary: "几种速度各刻一个实心小方块，量来回两行的错位，按速度存进偏移表（跟机器走）",
  },
  {
    id: "overscan",
    scope: "material",
    title: "过扫描检查",
    summary: "左块不过扫描、右块按当前设置，看两头是否发黑，必要时加大过扫描存进材料库",
  },
  {
    id: "kerf",
    scope: "material",
    title: "切缝补偿",
    summary: "切一个 20 mm 方块，量切下来的边长，算出切缝补偿存进该厚度的切割条目",
  },
  {
    id: "time",
    scope: "machine",
    title: "工时校准",
    summary: "正常加工完成后自动比较预估与实际用时，至少 2 次后按中位数校准之后的预估",
  },
]);

const STEP_IDS = new Set(WIZARD_STEPS.map((step) => step.id));
const STATUSES = new Set(["done", "skipped"]);

/** 材料步骤的进度键：材料名 + 厚度。 */
export function wizardMaterialKey(material, thickness) {
  return `${String(material || "").trim()}|${String(thickness ?? "").trim()}`;
}

function cleanRecord(raw) {
  if (!raw || typeof raw !== "object" || !STATUSES.has(raw.status)) return null;
  const at = Number(raw.at);
  return {
    status: raw.status,
    at: Number.isFinite(at) ? at : 0,
    note: typeof raw.note === "string" ? raw.note.slice(0, 200) : "",
  };
}

function cleanSteps(raw) {
  const out = {};
  if (!raw || typeof raw !== "object") return out;
  for (const [id, value] of Object.entries(raw)) {
    const record = STEP_IDS.has(id) ? cleanRecord(value) : null;
    if (record) out[id] = record;
  }
  return out;
}

/** 规整任意来源（存储、旧版本）的向导状态。 */
export function normalizeWizardState(raw) {
  const source = raw && typeof raw === "object" ? raw : {};
  const materials = {};
  if (source.materials && typeof source.materials === "object") {
    for (const [key, steps] of Object.entries(source.materials).slice(0, 200)) {
      const clean = cleanSteps(steps);
      if (Object.keys(clean).length) materials[key] = clean;
    }
  }
  return { machine: cleanSteps(source.machine), materials, open: source.open !== false };
}

export function readWizardState(storage = globalThis.localStorage) {
  try {
    const raw = storage?.getItem(WIZARD_STORAGE_KEY);
    return normalizeWizardState(raw ? JSON.parse(raw) : null);
  } catch {
    return normalizeWizardState(null);
  }
}

export function writeWizardState(state, storage = globalThis.localStorage) {
  try {
    storage?.setItem(WIZARD_STORAGE_KEY, JSON.stringify(normalizeWizardState(state)));
  } catch {
    // 存储被禁用时只是不记住进度
  }
}

function stepScope(id) {
  return WIZARD_STEPS.find((step) => step.id === id)?.scope || "material";
}

/** 某一步在当前材料下的记录（没有时为 null）。 */
export function stepRecord(state, id, materialKey) {
  const s = normalizeWizardState(state);
  return stepScope(id) === "machine" ? s.machine[id] || null : s.materials[materialKey]?.[id] || null;
}

/** 标记一步完成 / 跳过；`status=null` 时清除（重做）。返回新状态。 */
export function markWizardStep(state, id, status, { materialKey = "", note = "", at = Date.now() } = {}) {
  const s = normalizeWizardState(state);
  if (!STEP_IDS.has(id)) return s;
  const record = status ? cleanRecord({ status, note, at }) : null;
  if (stepScope(id) === "machine") {
    const machine = { ...s.machine };
    if (record) machine[id] = record;
    else delete machine[id];
    return { ...s, machine };
  }
  const steps = { ...(s.materials[materialKey] || {}) };
  if (record) steps[id] = record;
  else delete steps[id];
  const materials = { ...s.materials };
  if (Object.keys(steps).length) materials[materialKey] = steps;
  else delete materials[materialKey];
  return { ...s, materials };
}

/** 当前该做的一步（第一步既没完成也没跳过的）；全部处理过时为 null。 */
export function currentWizardStep(state, materialKey) {
  return WIZARD_STEPS.find((step) => !stepRecord(state, step.id, materialKey))?.id ?? null;
}

/**
 * 向导里展开的是哪一步（2026-11-22 用户要求每一步都能单独收起）。
 * `openStep` 为 `undefined` 表示跟随“当前该做的一步”（全部处理过时落到工时校准）；
 * 为空字符串表示用户把展开的那步收起了，全部收起；其余为用户点开的步骤 id。
 */
export function resolveOpenWizardStep(openStep, current) {
  if (openStep === undefined) return current || "time";
  return openStep || null;
}

/** 点某一步的标题：已展开就收起（全部收起），否则展开它。 */
export function toggleWizardStep(activeStep, stepId) {
  return activeStep === stepId ? "" : stepId;
}

/** 进度：完成、跳过与总步数。 */
export function wizardProgress(state, materialKey) {
  let done = 0;
  let skipped = 0;
  for (const step of WIZARD_STEPS) {
    const record = stepRecord(state, step.id, materialKey);
    if (record?.status === "done") done += 1;
    if (record?.status === "skipped") skipped += 1;
  }
  return { done, skipped, total: WIZARD_STEPS.length };
}

const toNumber = (value) => {
  const raw = String(value ?? "").trim();
  return raw === "" ? NaN : Number(raw);
};

/**
 * 切缝补偿：设计边长 `designMm` 的方块切下来量得 `measuredMm`。每条边被烧掉切缝宽度的一半，
 * 所以切缝宽度 w = 设计 − 实测，补偿（外轮廓往外挪）kerf_mm = w ÷ 2（与后端 `kerf_offset_mm` 相同）。
 */
export function kerfFromMeasurement(designMm, measuredMm) {
  const design = toNumber(designMm);
  const measured = toNumber(measuredMm);
  if (!Number.isFinite(design) || design <= 0) return { error: "测试方块边长无效" };
  if (!Number.isFinite(measured) || measured <= 0) return { error: "请填写量得的方块边长（mm）" };
  const kerf = Math.round(((design - measured) / 2) * 1000) / 1000;
  if (Math.abs(kerf) > 2) {
    return { error: `算出的切缝补偿 ${kerf} mm 超出 ±2 mm，请确认量的是切下来的方块、单位是 mm` };
  }
  return { kerfMm: kerf, widthMm: Math.round((design - measured) * 1000) / 1000 };
}

/**
 * 双向扫描偏移测量 → 偏移表行：每个速度量得的来回两行错位（mm，行行都往行进方向拖后填正数）。
 * 留空的速度跳过；没有任何有效值时报错。
 */
export function scanOffsetRowsFromMeasurements(measurements) {
  const rows = [];
  for (const item of Array.isArray(measurements) ? measurements : []) {
    const speed = toNumber(item?.speed);
    const raw = String(item?.measured ?? "").trim();
    if (raw === "") continue;
    const offset = Number(raw);
    if (!Number.isFinite(speed) || speed <= 0) return { error: "测试速度无效" };
    if (!Number.isFinite(offset) || Math.abs(offset) > 2) {
      return { error: `速度 ${speed} 的错位须在 -2 ~ 2 mm 之间` };
    }
    rows.push({ speed: String(Math.round(speed)), offset: String(Math.round(offset * 1000) / 1000) });
  }
  if (!rows.length) return { error: "请至少填一个速度量得的错位（没有错位填 0）" };
  return { rows };
}

/** 把新测的行并进本机偏移表：同速度替换，按速度排序，最多 32 行。 */
export function mergeScanOffsetTable(existing, rows) {
  const bySpeed = new Map();
  for (const row of [...(Array.isArray(existing) ? existing : []), ...(Array.isArray(rows) ? rows : [])]) {
    const speed = toNumber(row?.speed);
    if (!Number.isFinite(speed) || speed <= 0) continue;
    bySpeed.set(Math.round(speed), { speed: String(Math.round(speed)), offset: String(row?.offset ?? "") });
  }
  return [...bySpeed.entries()]
    .sort((a, b) => a[0] - b[0])
    .slice(0, 32)
    .map(([, row]) => row);
}

/**
 * 过扫描检查的建议：右块（当前设置）两头仍发黑时，建议把过扫描加大到当前的 1.5 倍（至少 +1 mm，最多 20 mm），
 * 并提示填写机器加速度（$120 / $121，自动过扫描按它算）。
 */
export function overscanSuggestion(currentMm, { auto = true } = {}) {
  const current = Number(currentMm);
  const base = Number.isFinite(current) && current > 0 ? current : 1;
  const next = Math.min(20, Math.round(Math.max(base * 1.5, base + 1) * 10) / 10);
  const accelHint = auto
    ? "过扫描是按速度和机器加速度自动算的：先在填充设置里填上机器实际的加速度（GRBL 的 $120 / $121）再测一次；"
    : "";
  return {
    nextMm: next,
    text: `${accelHint}也可以直接把这个材料的过扫描加大到 ${next} mm 存进材料库，再测一次确认两头不再发黑。`,
  };
}

/** 工时校准状态（`GET /api/time-calibration`）→ 一句说明。 */
export function timeCalibrationSummary(status) {
  const targets = Array.isArray(status?.targets) ? status.targets : [];
  const min = Number(status?.min_samples) || 2;
  if (!targets.length) {
    return `还没有记录。正常加工完成（不是取消、急停或断点续雕，且不少于 20 秒）后自动记录，满 ${min} 次开始校准。`;
  }
  return targets
    .map((t) => {
      const samples = Number(t.samples) || 0;
      return t.active && Number.isFinite(Number(t.factor))
        ? `${t.target}：已按最近 ${samples} 次实际用时校准（实际约为原估算的 ${Number(t.factor).toFixed(2)} 倍）`
        : `${t.target}：已记录 ${samples} 次，再完成 ${Math.max(0, min - samples)} 次开始校准`;
    })
    .join("；");
}

/**
 * 向导结果写回哪个材料库条目：测试图案记录里 `surface_entry` / `matched_thickness_mm` 标明参数取自哪一条
 * （所选厚度没有光栅参数时回退到“表面通用”或最近厚度），写回同一条；旧记录没有这两项时按请求厚度。
 * `note` 在写回的条目与所选厚度不同时给出说明。
 */
export function patternSaveTarget(pattern, fallbackThickness) {
  const requested = Number(pattern?.thickness_mm ?? fallbackThickness);
  if (pattern?.surface_entry === true) {
    return { request: { surface: true }, note: "（参数来自“表面通用”条目，已存到该条目）" };
  }
  const matched = Number(pattern?.matched_thickness_mm);
  if (Number.isFinite(matched) && matched > 0 && matched !== requested) {
    return { request: { thickness_mm: matched }, note: `（参数来自 ${matched}mm 条目，已存到该条目）` };
  }
  return { request: { thickness_mm: requested }, note: "" };
}

/** 向导测试图案的预览请求体（`/api/material-lab/preview`）。 */
export function buildPatternPreviewPayload({ testType, material, thickness, speeds = null, fillOptions = null, motion = null }) {
  if (!["scan_offset", "overscan", "kerf"].includes(testType)) return { error: "未知的测试类型" };
  const mat = String(material || "").trim();
  const thick = Number(thickness);
  if (!mat) return { error: "请先在左侧选择材料" };
  if (!Number.isFinite(thick) || thick <= 0) {
    return { error: "请先选一个具体厚度（表面通用条目不能做这项测试）" };
  }
  const payload = { material: mat, thickness_mm: thick, test_type: testType };
  if (testType === "scan_offset" && Array.isArray(speeds) && speeds.length) {
    const list = speeds.map(Number).filter((v) => Number.isFinite(v) && v > 0).map(Math.round);
    if (!list.length || list.length > 6) return { error: "测试速度填 1 ~ 6 个" };
    payload.speeds = list;
  }
  if (testType === "overscan" && fillOptions) payload.fill_options = fillOptions;
  if (motion) Object.assign(payload, motion);
  return { payload };
}
