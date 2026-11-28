/**
 * “跟机器走”的后端设置同步与一次性迁移（2026-11-28，技术书 r78）：
 *
 * 后端 `settings.json` 是权威存储源（经 `GET /api/system/info` 读取、`POST /api/settings` 写入，
 * 复用操作者设置门禁，局域网只读客户端只读不可写）：
 * 1. 按连接目标 `target_key` 分桶保存的机器档案（最多 16 个目标）：
 *    - 手填行程（`manual_travel`）
 *    - 手填运动参数（`manual_motion`，含 `accelerationMmS2`）
 *    - 双向扫描偏移校准（`scan_offset_mm` / `scan_offset_table`）
 * 2. 全局走边框形状（`laser_framing_shape`：`"box"` / `"hull"`），工作台、画板、AI 预览窗与语音/MCP 共用。
 *
 * 迁移与去留策略：
 * - 首次在可写主窗口收到 `/api/system/info` 时，把旧浏览器 `localStorage` 里上述手填字段一次性迁移到后端
 *   （`machine_profiles_migrate` / `laser_framing_shape_migrate`，后端已有值时不覆盖）；
 * - 迁移成功后立即从 `localStorage` 清除这些已迁移的字段（保留硬件握手缓存 `hardware`/`identity`/`safety`
 *   以及与机器无关的界面偏好/草稿），此后内存态严格以后端为准，不再写回 `localStorage`，避免多端漂移。
 */

import { postJson } from "./api/client.js";
import { isReadOnlyReferenceClient } from "./authSession.js";
import {
  MACHINE_MOTION_EVENT,
  MACHINE_MOTION_KEY,
  MAX_SAVED_TARGETS,
  MAX_XY_TRAVEL_MM,
  MAX_Z_TRAVEL_MM,
  WORKSPACE_LIMITS_EVENT,
  WORKSPACE_LIMITS_KEY,
  connectionTargetKey,
  readRawLocalWorkspaceLimitsStore,
  readUnifiedConnection,
  registerBackendWorkspaceManualReader,
} from "./deviceConnection.js";
import {
  DEFAULT_FILL_SETTINGS,
  FILL_SETTINGS_KEY,
  MAX_SCAN_OFFSET_ROWS,
  normalizeFillSettings,
  offsetTablePayload,
  readRawLocalMachineMotionStore,
  registerBackendMotionManualReader,
} from "./fillOptions.js";
import {
  FRAMING_SHAPE_KEY,
  registerBackendFramingShapeReader,
} from "./aiPreviewState.js";

export const MACHINE_PROFILE_EVENT = "moss:machine-profile";
export const FRAMING_SHAPE_EVENT = "moss:framing-shape";
export const MAX_MACHINE_PROFILES = MAX_SAVED_TARGETS;

const MOTION_FIELD_LIMITS = Object.freeze({
  x_accel_mm_s2: 100000,
  y_accel_mm_s2: 100000,
  x_max_rate_mm_per_min: 1000000,
  y_max_rate_mm_per_min: 1000000,
  junction_deviation_mm: 10,
});

const runtimeState = {
  hydrated: false,
  canWrite: false,
  profiles: { targets: {} },
  framingShape: "box",
  verifiedTargetKey: "",
  migrationAttempted: false,
  pendingSaves: 0,
};

function posNumber(value, max) {
  if (value === null || value === undefined || value === "") return undefined;
  const num = Number(value);
  if (!Number.isFinite(num) || num <= 0 || num > max) return undefined;
  return num;
}

function scanOffsetNumber(value) {
  if (value === null || value === undefined || value === "") return undefined;
  const num = Number(value);
  if (!Number.isFinite(num) || Math.abs(num) > 2) return undefined;
  return Math.abs(num) <= 1e-9 ? undefined : num;
}

function normalizeTableRows(raw) {
  if (!Array.isArray(raw)) return [];
  const rows = [];
  for (const row of raw.slice(0, MAX_SCAN_OFFSET_ROWS)) {
    if (Array.isArray(row) && row.length === 2) {
      const speed = posNumber(row[0], 1000000);
      const offset = Number(row[1]);
      if (speed !== undefined && Number.isFinite(offset) && Math.abs(offset) <= 2) {
        rows.push([speed, offset]);
      }
    }
  }
  rows.sort((a, b) => a[0] - b[0]);
  return rows.filter((row, idx) => idx === 0 || row[0] !== rows[idx - 1][0]);
}

export function normalizeBackendProfileEntry(raw) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const manual_travel = {};
  if (raw.manual_travel && typeof raw.manual_travel === "object") {
    const x = posNumber(raw.manual_travel.x_travel_mm ?? raw.manual_travel.x, MAX_XY_TRAVEL_MM);
    const y = posNumber(raw.manual_travel.y_travel_mm ?? raw.manual_travel.y, MAX_XY_TRAVEL_MM);
    const z = posNumber(raw.manual_travel.z_travel_mm ?? raw.manual_travel.z, MAX_Z_TRAVEL_MM);
    if (x !== undefined) manual_travel.x_travel_mm = x;
    if (y !== undefined) manual_travel.y_travel_mm = y;
    if (z !== undefined) manual_travel.z_travel_mm = z;
  }
  const manual_motion = {};
  if (raw.manual_motion && typeof raw.manual_motion === "object") {
    for (const [key, max] of Object.entries(MOTION_FIELD_LIMITS)) {
      const val = posNumber(raw.manual_motion[key], max);
      if (val !== undefined) manual_motion[key] = val;
    }
  }
  const scan_offset_mm = scanOffsetNumber(raw.scan_offset_mm);
  const scan_offset_table = normalizeTableRows(raw.scan_offset_table);
  const hasAny =
    Object.keys(manual_travel).length > 0
    || Object.keys(manual_motion).length > 0
    || scan_offset_mm !== undefined
    || scan_offset_table.length > 0;
  if (!hasAny) return null;
  const out = {
    updated_at: Number.isFinite(Number(raw.updated_at)) ? Number(raw.updated_at) : 0,
  };
  if (Object.keys(manual_travel).length > 0) out.manual_travel = manual_travel;
  if (Object.keys(manual_motion).length > 0) out.manual_motion = manual_motion;
  if (scan_offset_mm !== undefined) out.scan_offset_mm = scan_offset_mm;
  if (scan_offset_table.length > 0) out.scan_offset_table = scan_offset_table;
  return out;
}

export function normalizeBackendProfilesStore(raw) {
  const targetsObj = raw && typeof raw === "object" && !Array.isArray(raw) ? raw.targets : null;
  if (!targetsObj || typeof targetsObj !== "object" || Array.isArray(targetsObj)) {
    return { targets: {} };
  }
  const entries = [];
  for (const [rawKey, rawEntry] of Object.entries(targetsObj)) {
    const key = String(rawKey || "").trim();
    if (!key) continue;
    const clean = normalizeBackendProfileEntry(rawEntry);
    if (clean) entries.push([key, clean]);
  }
  if (entries.length > MAX_MACHINE_PROFILES) {
    entries.sort((a, b) => (a[1].updated_at || 0) - (b[1].updated_at || 0));
    entries.splice(0, entries.length - MAX_MACHINE_PROFILES);
  }
  return { targets: Object.fromEntries(entries) };
}

/** 把后端手填行程合并到工作区行程映射：manual 槽位严格以后端为准，hardware/identity/safety 保留本地握手缓存。 */
export function mergeBackendManualIntoWorkspaceStore(localStore, profilesStore) {
  const localTargets = localStore?.targets && typeof localStore.targets === "object" ? localStore.targets : {};
  const profileTargets = profilesStore?.targets && typeof profilesStore.targets === "object" ? profilesStore.targets : {};
  const allKeys = new Set([...Object.keys(localTargets), ...Object.keys(profileTargets)]);
  const merged = {};
  for (const key of allKeys) {
    const local = localTargets[key];
    const prof = profileTargets[key];
    const hardware = local?.hardware && typeof local.hardware === "object" ? { ...local.hardware } : {};
    const manual = {};
    if (prof?.manual_travel) {
      if (prof.manual_travel.x_travel_mm !== undefined) manual.x = prof.manual_travel.x_travel_mm;
      if (prof.manual_travel.y_travel_mm !== undefined) manual.y = prof.manual_travel.y_travel_mm;
      if (prof.manual_travel.z_travel_mm !== undefined) manual.z = prof.manual_travel.z_travel_mm;
    }
    const identity = local?.identity || null;
    const safety = local?.safety || null;
    if (!Object.keys(hardware).length && !Object.keys(manual).length && !identity && !safety) {
      continue;
    }
    const entry = {
      hardware,
      manual,
      updated_at: Math.max(Number(local?.updated_at) || 0, Number(prof?.updated_at) || 0),
    };
    if (identity) entry.identity = identity;
    if (safety) entry.safety = safety;
    merged[key] = entry;
  }
  return { targets: merged };
}

/** 把后端手填运动参数覆盖到握手固件运动参数之上（手填优先于固件）。 */
export function mergeBackendManualIntoMotionStore(localStore, profilesStore) {
  const localTargets = localStore?.targets && typeof localStore.targets === "object" ? localStore.targets : {};
  const profileTargets = profilesStore?.targets && typeof profilesStore.targets === "object" ? profilesStore.targets : {};
  const allKeys = new Set([...Object.keys(localTargets), ...Object.keys(profileTargets)]);
  const merged = {};
  for (const key of allKeys) {
    const local = localTargets[key];
    const prof = profileTargets[key];
    const motion = {
      ...(local?.motion && typeof local.motion === "object" ? local.motion : {}),
      ...(prof?.manual_motion && typeof prof.manual_motion === "object" ? prof.manual_motion : {}),
    };
    if (!Object.keys(motion).length) continue;
    merged[key] = {
      motion,
      updated_at: Math.max(Number(local?.updated_at) || 0, Number(prof?.updated_at) || 0),
    };
  }
  return { targets: merged };
}

/** 从后端机器档案取出指定目标的校准项（加速度、扫描偏移、速度偏移表）转为 FillOptionsPanel 字段。 */
export function profileCalibrationForTarget(profilesStore, target) {
  const key = connectionTargetKey(target);
  const prof = key ? profilesStore?.targets?.[key] : null;
  if (!prof) {
    return {
      accelerationMmS2: "",
      scanOffsetMm: DEFAULT_FILL_SETTINGS.scanOffsetMm,
      scanOffsetTable: [],
      manualMotion: null,
    };
  }
  const accel = prof.manual_motion?.x_accel_mm_s2 ?? prof.manual_motion?.y_accel_mm_s2;
  const table = Array.isArray(prof.scan_offset_table)
    ? prof.scan_offset_table.map(([speed, offset]) => ({ speed: String(speed), offset: String(offset) }))
    : [];
  return {
    accelerationMmS2: accel !== undefined ? String(accel) : "",
    scanOffsetMm: prof.scan_offset_mm !== undefined ? String(prof.scan_offset_mm) : "0",
    scanOffsetTable: table,
    manualMotion: prof.manual_motion ? { ...prof.manual_motion } : null,
  };
}

/** 把后端档案中该目标的机器校准项叠加到填充设置对象上。 */
export function applyMachineCalibrationToFillSettings(fillSettings, profilesStore, target) {
  const base = normalizeFillSettings(fillSettings);
  const cal = profileCalibrationForTarget(profilesStore, target);
  return {
    ...base,
    accelerationMmS2: cal.accelerationMmS2,
    scanOffsetMm: cal.scanOffsetMm,
    scanOffsetTable: cal.scanOffsetTable,
  };
}

/** 写回本地草稿 `moss.fillSettings.v1` 前剥离已迁往后端的机器校准字段，防止本地留存旧副本。 */
export function stripMachineCalibrationFromFillSettings(fillSettings) {
  const base = normalizeFillSettings(fillSettings);
  return {
    ...base,
    accelerationMmS2: "",
    scanOffsetMm: DEFAULT_FILL_SETTINGS.scanOffsetMm,
    scanOffsetTable: [],
  };
}

/** 把填充设置里的 3 个机器校准字段转成 `machine_profile_patch`（无目标或正在输入无效值时返回 null / error）。 */
export function buildCalibrationPatchFromFillSettings(fillSettings, target) {
  const targetKey = connectionTargetKey(target);
  if (!targetKey) return null;
  const s = normalizeFillSettings(fillSettings);

  const rawAccel = String(s.accelerationMmS2 ?? "").trim();
  let accelValue = null;
  if (rawAccel !== "") {
    const num = Number(rawAccel);
    if (!Number.isFinite(num) || num < 10 || num > 100000) {
      return { error: "机器加速度须在 10 ~ 100000 之间" };
    }
    accelValue = num;
  }

  const rawOffset = String(s.scanOffsetMm ?? "").trim();
  let offsetValue = null;
  if (rawOffset !== "") {
    const num = Number(rawOffset);
    if (!Number.isFinite(num) || Math.abs(num) > 2) {
      return { error: "双向扫描偏移须在 -2 ~ 2 mm 之间" };
    }
    offsetValue = Math.abs(num) <= 1e-9 ? null : num;
  }

  const parsedTable = offsetTablePayload(s.scanOffsetTable);
  if (parsedTable.error) {
    return { error: parsedTable.error };
  }

  return {
    patch: {
      target_key: targetKey,
      manual_motion: { acceleration_mm_s2: accelValue },
      scan_offset_mm: offsetValue,
      scan_offset_table: parsedTable.value ?? [],
    },
  };
}

/**
 * 扫描浏览器 `localStorage` 中尚未迁移的历史手填值，生成发给 `POST /api/settings` 的一次性迁移载荷。
 * 只提取手填项（绝不把硬件握手缓存 `hardware` 或固件 `machineMotion` 当手填上传）。
 */
export function extractLegacyMigrationPayload(storage, fallbackTargetKey = "") {
  const targets = {};
  const ensureTarget = (key, updatedAt = 0) => {
    if (!targets[key]) targets[key] = { updated_at: updatedAt };
    else if (updatedAt > (targets[key].updated_at || 0)) targets[key].updated_at = updatedAt;
    return targets[key];
  };

  let hasWorkspaceManual = false;
  let newestWorkspaceKey = "";
  let newestWorkspaceTs = -1;
  try {
    const rawWs = storage?.getItem?.(WORKSPACE_LIMITS_KEY);
    if (rawWs) {
      const parsed = JSON.parse(rawWs);
      if (parsed?.targets && typeof parsed.targets === "object" && !Array.isArray(parsed.targets)) {
        for (const [rawKey, item] of Object.entries(parsed.targets)) {
          const key = String(rawKey || "").trim();
          if (!key || !item || typeof item !== "object") continue;
          const ts = Number.isFinite(Number(item.updated_at)) ? Number(item.updated_at) : 0;
          if (ts >= newestWorkspaceTs) {
            newestWorkspaceTs = ts;
            newestWorkspaceKey = key;
          }
          const man = item.manual && typeof item.manual === "object" ? item.manual : null;
          if (!man) continue;
          const travel = {};
          const x = posNumber(man.x ?? man.x_travel_mm, MAX_XY_TRAVEL_MM);
          const y = posNumber(man.y ?? man.y_travel_mm, MAX_XY_TRAVEL_MM);
          const z = posNumber(man.z ?? man.z_travel_mm, MAX_Z_TRAVEL_MM);
          if (x !== undefined) travel.x_travel_mm = x;
          if (y !== undefined) travel.y_travel_mm = y;
          if (z !== undefined) travel.z_travel_mm = z;
          if (Object.keys(travel).length > 0) {
            ensureTarget(key, ts).manual_travel = travel;
            hasWorkspaceManual = true;
          }
        }
      } else if (parsed?.manual && typeof parsed.target_key === "string" && parsed.target_key.trim()) {
        const key = parsed.target_key.trim();
        const travel = {};
        const x = posNumber(parsed.x ?? parsed.x_travel_mm, MAX_XY_TRAVEL_MM);
        const y = posNumber(parsed.y ?? parsed.y_travel_mm, MAX_XY_TRAVEL_MM);
        const z = posNumber(parsed.z ?? parsed.z_travel_mm, MAX_Z_TRAVEL_MM);
        if (x !== undefined) travel.x_travel_mm = x;
        if (y !== undefined) travel.y_travel_mm = y;
        if (z !== undefined) travel.z_travel_mm = z;
        if (Object.keys(travel).length > 0) {
          ensureTarget(key, 1).manual_travel = travel;
          hasWorkspaceManual = true;
          newestWorkspaceKey = key;
        }
      }
    }
  } catch {
    // 忽略损坏的旧存储
  }

  let hasMotionManual = false;
  try {
    const rawMotion = storage?.getItem?.(MACHINE_MOTION_KEY);
    if (rawMotion) {
      const parsed = JSON.parse(rawMotion);
      if (parsed?.targets && typeof parsed.targets === "object" && !Array.isArray(parsed.targets)) {
        for (const [rawKey, item] of Object.entries(parsed.targets)) {
          const key = String(rawKey || "").trim();
          const man = item?.manual && typeof item.manual === "object" ? item.manual : null;
          if (!key || !man) continue;
          const motion = {};
          for (const [field, max] of Object.entries(MOTION_FIELD_LIMITS)) {
            const val = posNumber(man[field], max);
            if (val !== undefined) motion[field] = val;
          }
          if (Object.keys(motion).length > 0) {
            const entry = ensureTarget(key, Number(item.updated_at) || 0);
            entry.manual_motion = { ...(entry.manual_motion || {}), ...motion };
            hasMotionManual = true;
          }
        }
      }
    }
  } catch {
    // ignore
  }

  let hasFillCalibration = false;
  try {
    const rawFill = storage?.getItem?.(FILL_SETTINGS_KEY);
    if (rawFill) {
      const s = normalizeFillSettings(JSON.parse(rawFill));
      const accel = posNumber(s.accelerationMmS2, 100000);
      const offset = scanOffsetNumber(s.scanOffsetMm);
      const tableRes = offsetTablePayload(s.scanOffsetTable);
      const table = !tableRes.error && Array.isArray(tableRes.value) && tableRes.value.length > 0 ? tableRes.value : null;
      if (accel !== undefined || offset !== undefined || table) {
        const currentKey = connectionTargetKey(readUnifiedConnection(storage));
        const destKey = currentKey || String(fallbackTargetKey || "").trim() || newestWorkspaceKey;
        if (destKey) {
          const entry = ensureTarget(destKey, Date.now());
          if (accel !== undefined) {
            entry.manual_motion = {
              ...(entry.manual_motion || {}),
              x_accel_mm_s2: accel,
              y_accel_mm_s2: accel,
            };
          }
          if (offset !== undefined) entry.scan_offset_mm = offset;
          if (table) entry.scan_offset_table = table;
          hasFillCalibration = true;
        }
      }
    }
  } catch {
    // ignore
  }

  let framingShape = null;
  let hasFramingShape = false;
  try {
    const rawShape = storage?.getItem?.(FRAMING_SHAPE_KEY);
    if (rawShape === "hull" || rawShape === "box") {
      framingShape = rawShape;
      hasFramingShape = true;
    }
  } catch {
    // ignore
  }

  const hasTargets = Object.keys(targets).length > 0;
  return {
    machine_profiles_migrate: hasTargets ? { targets } : null,
    laser_framing_shape_migrate: framingShape,
    hasWorkspaceManual,
    hasMotionManual,
    hasFillCalibration,
    hasFramingShape,
    hasAny: hasTargets || hasFramingShape,
  };
}

/** 迁移成功（或后端已有权威值）后，从浏览器 localStorage 清理已迁往后端的手填字段。 */
export function stripMigratedLegacyLocalStorage(storage, plan) {
  if (!storage || !plan) return;
  if (plan.hasWorkspaceManual) {
    try {
      const raw = storage.getItem?.(WORKSPACE_LIMITS_KEY);
      if (raw) {
        const parsed = JSON.parse(raw);
        if (parsed?.targets && typeof parsed.targets === "object" && !Array.isArray(parsed.targets)) {
          const nextTargets = {};
          for (const [key, item] of Object.entries(parsed.targets)) {
            if (!item || typeof item !== "object") continue;
            const hw = item.hardware && typeof item.hardware === "object" ? item.hardware : {};
            if (!Object.keys(hw).length && !item.identity && !item.safety) continue;
            const clean = { hardware: hw, manual: {}, updated_at: Number(item.updated_at) || 0 };
            if (item.identity) clean.identity = item.identity;
            if (item.safety) clean.safety = item.safety;
            nextTargets[key] = clean;
          }
          storage.setItem?.(WORKSPACE_LIMITS_KEY, JSON.stringify({ targets: nextTargets }));
        } else if (parsed?.manual) {
          storage.removeItem?.(WORKSPACE_LIMITS_KEY);
        }
      }
    } catch {
      // ignore
    }
  }
  if (plan.hasMotionManual) {
    try {
      const raw = storage.getItem?.(MACHINE_MOTION_KEY);
      if (raw) {
        const parsed = JSON.parse(raw);
        if (parsed?.targets && typeof parsed.targets === "object" && !Array.isArray(parsed.targets)) {
          const nextTargets = {};
          for (const [key, item] of Object.entries(parsed.targets)) {
            if (!item || typeof item !== "object") continue;
            const rest = { ...item };
            delete rest.manual;
            if (rest.motion && Object.keys(rest.motion).length > 0) {
              nextTargets[key] = rest;
            }
          }
          storage.setItem?.(MACHINE_MOTION_KEY, JSON.stringify({ targets: nextTargets }));
        }
      }
    } catch {
      // ignore
    }
  }
  if (plan.hasFillCalibration) {
    try {
      const raw = storage.getItem?.(FILL_SETTINGS_KEY);
      if (raw) {
        const stripped = stripMachineCalibrationFromFillSettings(JSON.parse(raw));
        storage.setItem?.(FILL_SETTINGS_KEY, JSON.stringify(stripped));
      }
    } catch {
      // ignore
    }
  }
  if (plan.hasFramingShape) {
    try {
      storage.removeItem?.(FRAMING_SHAPE_KEY);
    } catch {
      // ignore
    }
  }
}

function dispatchBrowserEvent(name, detail) {
  try {
    if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
      window.dispatchEvent(new CustomEvent(name, { detail }));
    }
  } catch {
    // 非浏览器环境忽略
  }
}

function broadcastAllMachineEvents(profilesChanged, framingChanged) {
  if (profilesChanged) {
    const mergedWs = mergeBackendManualIntoWorkspaceStore(
      readRawLocalWorkspaceLimitsStore(),
      runtimeState.profiles,
    );
    const mergedMotion = mergeBackendManualIntoMotionStore(
      readRawLocalMachineMotionStore(),
      runtimeState.profiles,
    );
    dispatchBrowserEvent(WORKSPACE_LIMITS_EVENT, mergedWs);
    dispatchBrowserEvent(MACHINE_MOTION_EVENT, mergedMotion);
    dispatchBrowserEvent(MACHINE_PROFILE_EVENT, runtimeState.profiles);
  }
  if (framingChanged) {
    dispatchBrowserEvent(FRAMING_SHAPE_EVENT, { shape: runtimeState.framingShape });
  }
}

export function getRuntimeMachineState() {
  return {
    hydrated: runtimeState.hydrated,
    canWrite: runtimeState.canWrite,
    profiles: runtimeState.profiles,
    framingShape: runtimeState.framingShape,
    verifiedTargetKey: runtimeState.verifiedTargetKey,
  };
}

export function isMachineProfilesHydrated() {
  return runtimeState.hydrated;
}

export function canEditMachineProfiles() {
  return runtimeState.hydrated ? runtimeState.canWrite : !isReadOnlyReferenceClient();
}

/** 比较两份填充设置里属于“跟机器走”的校准字段（加速度、扫描偏移、速度偏移表）是否变化。 */
export function isMachineCalibrationChanged(prevFill, nextFill) {
  const a = normalizeFillSettings(prevFill);
  const b = normalizeFillSettings(nextFill);
  return (
    a.accelerationMmS2 !== b.accelerationMmS2
    || a.scanOffsetMm !== b.scanOffsetMm
    || JSON.stringify(a.scanOffsetTable) !== JSON.stringify(b.scanOffsetTable)
  );
}

/** 读取当前目标的填充设置：未完成后端同步前读本地，同步后自动叠加后端机器档案中的校准值。 */
export function readFillSettingsWithMachineCalibration(readLocalFillSettings, target) {
  const local = readLocalFillSettings();
  if (!runtimeState.hydrated) return local;
  return applyMachineCalibrationToFillSettings(local, runtimeState.profiles, target);
}

function applyOptimisticPatchToStore(store, patch) {
  const key = String(patch?.target_key || "").trim();
  if (!key) return store;
  const nextTargets = { ...(store?.targets || {}) };
  if (patch.clear) {
    delete nextTargets[key];
    return { targets: nextTargets };
  }
  const prev = nextTargets[key] || {};
  const entry = {
    updated_at: Math.max(Date.now(), (Number(prev.updated_at) || 0) + 1),
  };
  if (prev.manual_travel) entry.manual_travel = { ...prev.manual_travel };
  if (prev.manual_motion) entry.manual_motion = { ...prev.manual_motion };
  if (prev.scan_offset_mm !== undefined) entry.scan_offset_mm = prev.scan_offset_mm;
  if (Array.isArray(prev.scan_offset_table) && prev.scan_offset_table.length > 0) {
    entry.scan_offset_table = prev.scan_offset_table.map((row) => [...row]);
  }

  if ("manual_travel" in patch) {
    if (patch.manual_travel === null) {
      delete entry.manual_travel;
    } else if (patch.manual_travel && typeof patch.manual_travel === "object") {
      const nextTravel = { ...(entry.manual_travel || {}) };
      for (const axis of ["x_travel_mm", "y_travel_mm", "z_travel_mm"]) {
        if (axis in patch.manual_travel) {
          if (patch.manual_travel[axis] === null) delete nextTravel[axis];
          else nextTravel[axis] = patch.manual_travel[axis];
        }
      }
      if (Object.keys(nextTravel).length > 0) entry.manual_travel = nextTravel;
      else delete entry.manual_travel;
    }
  }
  if ("manual_motion" in patch) {
    if (patch.manual_motion === null) {
      delete entry.manual_motion;
    } else if (patch.manual_motion && typeof patch.manual_motion === "object") {
      const nextMotion = { ...(entry.manual_motion || {}) };
      if ("acceleration_mm_s2" in patch.manual_motion) {
        if (patch.manual_motion.acceleration_mm_s2 === null) {
          delete nextMotion.x_accel_mm_s2;
          delete nextMotion.y_accel_mm_s2;
        } else {
          nextMotion.x_accel_mm_s2 = patch.manual_motion.acceleration_mm_s2;
          nextMotion.y_accel_mm_s2 = patch.manual_motion.acceleration_mm_s2;
        }
      }
      for (const field of Object.keys(MOTION_FIELD_LIMITS)) {
        if (field in patch.manual_motion) {
          if (patch.manual_motion[field] === null) delete nextMotion[field];
          else nextMotion[field] = patch.manual_motion[field];
        }
      }
      if (Object.keys(nextMotion).length > 0) entry.manual_motion = nextMotion;
      else delete entry.manual_motion;
    }
  }
  if ("scan_offset_mm" in patch) {
    if (patch.scan_offset_mm === null || Math.abs(Number(patch.scan_offset_mm) || 0) <= 1e-9) {
      delete entry.scan_offset_mm;
    } else {
      entry.scan_offset_mm = Number(patch.scan_offset_mm);
    }
  }
  if ("scan_offset_table" in patch) {
    if (!Array.isArray(patch.scan_offset_table) || patch.scan_offset_table.length === 0) {
      delete entry.scan_offset_table;
    } else {
      entry.scan_offset_table = patch.scan_offset_table.map((row) => [...row]);
    }
  }

  const normalized = normalizeBackendProfileEntry(entry);
  if (normalized) nextTargets[key] = normalized;
  else delete nextTargets[key];
  return normalizeBackendProfilesStore({ targets: nextTargets });
}

/** 仅用于单测重置内存态。 */
export function resetRuntimeMachineState() {
  runtimeState.hydrated = false;
  runtimeState.canWrite = false;
  runtimeState.profiles = { targets: {} };
  runtimeState.framingShape = "box";
  runtimeState.verifiedTargetKey = "";
  runtimeState.migrationAttempted = false;
  runtimeState.pendingSaves = 0;
}

function applyAuthoritativeValues(nextProfilesRaw, nextShapeRaw) {
  const nextProfiles = normalizeBackendProfilesStore(nextProfilesRaw);
  const nextShape = nextShapeRaw === "hull" ? "hull" : "box";
  const wasHydrated = runtimeState.hydrated;
  const profilesChanged = !wasHydrated || JSON.stringify(runtimeState.profiles) !== JSON.stringify(nextProfiles);
  const framingChanged = !wasHydrated || runtimeState.framingShape !== nextShape;
  runtimeState.hydrated = true;
  runtimeState.profiles = nextProfiles;
  runtimeState.framingShape = nextShape;
  broadcastAllMachineEvents(profilesChanged, framingChanged);
}

/**
 * 消费 `GET /api/system/info` 的结果：
 * 1. 首次在可写主窗口执行一次性旧本地数据迁移（后端已有值时不覆盖，成功后清理本地旧手填值）；
 * 2. 更新内存权威态并广播事件，驱动所有页面与跨窗口/局域网只读端同步刷新。
 */
export async function hydrateMachineProfilesFromSystemInfo(
  systemInfoResult,
  { post = postJson, storage = globalThis.localStorage } = {},
) {
  if (!systemInfoResult || typeof systemInfoResult !== "object") return;
  const canWrite = systemInfoResult.can_edit_operator_settings !== false;
  runtimeState.canWrite = canWrite;
  if (typeof systemInfoResult.verified_connection_target_key === "string") {
    runtimeState.verifiedTargetKey = systemInfoResult.verified_connection_target_key;
  }

  if (!runtimeState.migrationAttempted && canWrite && storage && typeof post === "function") {
    runtimeState.migrationAttempted = true;
    const plan = extractLegacyMigrationPayload(storage, runtimeState.verifiedTargetKey);
    if (plan.hasAny) {
      const body = {};
      if (plan.machine_profiles_migrate) body.machine_profiles_migrate = plan.machine_profiles_migrate;
      if (plan.laser_framing_shape_migrate) body.laser_framing_shape_migrate = plan.laser_framing_shape_migrate;
      try {
        runtimeState.pendingSaves += 1;
        const res = await post("/api/settings", body);
        if (res?.ok && res?.data?.success) {
          stripMigratedLegacyLocalStorage(storage, plan);
          applyAuthoritativeValues(
            res.data.result?.machine_profiles ?? systemInfoResult.machine_profiles,
            res.data.result?.laser_framing_shape ?? systemInfoResult.laser_framing_shape,
          );
          return;
        }
        // 非 403 的瞬时失败允许下次轮询重试；403 表示不可写，不重试
        if (res?.status !== 403) {
          runtimeState.migrationAttempted = false;
        }
      } catch {
        runtimeState.migrationAttempted = false;
      } finally {
        runtimeState.pendingSaves -= 1;
      }
    }
  }

  if (runtimeState.pendingSaves > 0) return;
  if ("machine_profiles" in systemInfoResult || "laser_framing_shape" in systemInfoResult) {
    applyAuthoritativeValues(
      systemInfoResult.machine_profiles ?? runtimeState.profiles,
      systemInfoResult.laser_framing_shape ?? runtimeState.framingShape,
    );
  }
}

async function submitMachineProfilePatch(patch, { post = postJson } = {}) {
  const prev = runtimeState.profiles;
  const wasHydrated = runtimeState.hydrated;
  runtimeState.hydrated = true;
  runtimeState.profiles = applyOptimisticPatchToStore(prev, patch);
  broadcastAllMachineEvents(true, false);
  runtimeState.pendingSaves += 1;
  try {
    const res = await post("/api/settings", { machine_profile_patch: patch });
    if (res?.ok && res?.data?.success) {
      applyAuthoritativeValues(
        res.data.result?.machine_profiles ?? runtimeState.profiles,
        res.data.result?.laser_framing_shape ?? runtimeState.framingShape,
      );
      return { ok: true, profiles: runtimeState.profiles };
    }
    runtimeState.hydrated = wasHydrated;
    runtimeState.profiles = prev;
    broadcastAllMachineEvents(true, false);
    const reason = (typeof res?.data?.result === "string" && res.data.result) || res?.error || "后端无响应";
    return { ok: false, status: res?.status, error: reason };
  } catch (error) {
    runtimeState.hydrated = wasHydrated;
    runtimeState.profiles = prev;
    broadcastAllMachineEvents(true, false);
    return { ok: false, error: error?.message || String(error) };
  } finally {
    runtimeState.pendingSaves -= 1;
  }
}

/** 写入指定连接目标的手填行程到后端机器档案（只更新 manual_travel，不动固件值）。 */
export async function saveManualWorkspaceLimits(patch = {}, target = undefined, options = {}) {
  const targetKey = patch?.target !== undefined
    ? connectionTargetKey(patch.target)
    : typeof patch?.target_key === "string" && patch.target_key.trim()
      ? patch.target_key.trim()
      : connectionTargetKey(target ?? readUnifiedConnection());
  if (!targetKey) return { ok: false, skipped: true, error: "未指定连接目标" };
  const manualTravel = {};
  const addAxis = (rawVal, field, min, max) => {
    if (rawVal === undefined) return;
    if (rawVal === null || rawVal === "") {
      manualTravel[field] = null;
      return;
    }
    const num = Number(rawVal);
    if (Number.isFinite(num) && num > 0) {
      manualTravel[field] = Math.min(max, Math.max(min, num));
    }
  };
  addAxis(patch?.x ?? patch?.x_travel_mm, "x_travel_mm", 20, MAX_XY_TRAVEL_MM);
  addAxis(patch?.y ?? patch?.y_travel_mm, "y_travel_mm", 20, MAX_XY_TRAVEL_MM);
  addAxis(patch?.z ?? patch?.z_travel_mm, "z_travel_mm", 5, MAX_Z_TRAVEL_MM);
  if (!Object.keys(manualTravel).length) return { ok: true, skipped: true };
  return submitMachineProfilePatch({ target_key: targetKey, manual_travel: manualTravel }, options);
}

/** 清除指定连接目标的手填行程（恢复为固件值）。 */
export async function resetManualWorkspaceLimits(target = undefined, options = {}) {
  const targetKey = connectionTargetKey(target ?? readUnifiedConnection());
  if (!targetKey) return { ok: false, skipped: true, error: "未指定连接目标" };
  return submitMachineProfilePatch({ target_key: targetKey, manual_travel: null }, options);
}

/** 换机检测到身份变化时清空该连接目标在后端的手填档案。 */
export async function clearTargetMachineProfile(target = undefined, options = {}) {
  const targetKey = connectionTargetKey(target ?? readUnifiedConnection());
  if (!targetKey || !runtimeState.canWrite) return { ok: false, skipped: true };
  return submitMachineProfilePatch({ target_key: targetKey, clear: true }, options);
}

/** 保存当前目标的机器校准项（手填加速度 + 双向扫描偏移 / 速度偏移表）到后端档案。 */
export async function saveTargetMachineCalibration(fillSettings, target = undefined, options = {}) {
  const built = buildCalibrationPatchFromFillSettings(fillSettings, target ?? readUnifiedConnection());
  if (!built) return { ok: false, skipped: true };
  if (built.error) return { ok: false, skipped: true, error: built.error };
  return submitMachineProfilePatch(built.patch, options);
}

/** 保存全局走边框形状（box / hull）到后端设置。 */
export async function saveFramingShapeSetting(shape, { post = postJson } = {}) {
  const next = shape === "hull" ? "hull" : "box";
  const prev = runtimeState.framingShape;
  runtimeState.framingShape = next;
  broadcastAllMachineEvents(false, true);
  runtimeState.pendingSaves += 1;
  try {
    const res = await post("/api/settings", { laser_framing_shape: next });
    if (res?.ok && res?.data?.success) {
      applyAuthoritativeValues(
        res.data.result?.machine_profiles ?? runtimeState.profiles,
        res.data.result?.laser_framing_shape ?? next,
      );
      return { ok: true, shape: runtimeState.framingShape };
    }
    runtimeState.framingShape = prev;
    broadcastAllMachineEvents(false, true);
    const reason = (typeof res?.data?.result === "string" && res.data.result) || res?.error || "后端无响应";
    return { ok: false, status: res?.status, shape: prev, error: reason };
  } catch (error) {
    runtimeState.framingShape = prev;
    broadcastAllMachineEvents(false, true);
    return { ok: false, shape: prev, error: error?.message || String(error) };
  } finally {
    runtimeState.pendingSaves -= 1;
  }
}

// 注册运行时读取钩子：后端 hydrate 后，默认调用 readWorkspaceLimitsStore / readMachineMotionStore / readFramingShape
// 自动以后端权威态为准；测试显式传入自定义 storage 时仍按纯函数本地语义运行。
registerBackendWorkspaceManualReader(() => (runtimeState.hydrated ? runtimeState.profiles : null));
registerBackendMotionManualReader(() => (runtimeState.hydrated ? runtimeState.profiles : null));
registerBackendFramingShapeReader(() => (runtimeState.hydrated ? runtimeState.framingShape : null));
