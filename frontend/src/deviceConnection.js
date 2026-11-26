// 设备连接参数的本机存储（2026-11-21 第三批重构 3B）：工作台、材料实验室、数字孪生、创作画板与 AI 预览
// 以前各自手写同一组 localStorage 键，这里收口成一处。
//
// - 工作台一组（`laser-forge-*`）：雕刻工作台与材料实验室共用，数字孪生只读；
// - 创作画板一组（`laserLab.*`）：画板自己的记忆，没填时回退工作台那一组（见 drawConnection.js）。
// 存储被禁用或读写抛错时逐项退回默认值 / 静默跳过，页面照常工作，急停路径也不会因此中断。

import { isRemoteMode, isTauriHost } from "./authSession.js";
import { detectClientOs } from "./platform.js";

/** 连接表单字段 → 工作台存储键。 */
export const WORKBENCH_CONNECTION_KEYS = Object.freeze({
  connection_mode: "laser-forge-conn-mode",
  network_host: "laser-forge-host",
  network_telnet_port: "laser-forge-telnet-port",
  port: "laser-forge-serial-port",
  baudrate: "laser-forge-baud",
});

/** 连接表单字段 → 创作画板存储键（旧版独立存储，读取时作迁移回退，统一写入时自动清理以避免两处各存一份）。 */
export const DRAW_CONNECTION_KEYS = Object.freeze({
  connection_mode: "laserLab.connection_mode",
  network_host: "laserLab.network_host",
  network_telnet_port: "laserLab.network_telnet_port",
  port: "laserLab.port",
  baudrate: "laserLab.baudrate",
});

export const DEFAULT_TELNET_PORT = "23";
export const DEFAULT_BAUDRATE = "115200";
export const SERIAL_BAUDRATES = Object.freeze([
  9600,
  19200,
  38400,
  57600,
  115200,
  230400,
  250000,
  460800,
  921600,
]);
export const DEVICE_CONNECTION_EVENT = "moss:device-connection";
export const MACHINE_MOTION_EVENT = "moss:machine-motion";
export const WORKSPACE_LIMITS_KEY = "moss_yisu_workspace_limits_v1";
export const WORKSPACE_LIMITS_EVENT = "moss:workspace-limits";
export const MACHINE_FEATURES_KEY = "moss.machineFeatures.v1";
export const MACHINE_FEATURES_EVENT = "moss:machine-features";
export const MAX_SAVED_TARGETS = 16;
export const DEFAULT_XY_TRAVEL_MM = 100;
export const DEFAULT_Z_TRAVEL_MM = 20;
export const MAX_XY_TRAVEL_MM = 2000;
export const MAX_Z_TRAVEL_MM = 500;
export const DEVICE_LOG_EVENT = "moss:device-log";

const USE_DEFAULT_STORAGE = Symbol("defaultStorage");
let sessionFallbackForm = null;
const recentDeviceLogs = [];
const MAX_BUFFERED_DEVICE_LOGS = 50;

export function emitDeviceLog(tag, message, level = "info") {
  const entry = {
    time: new Date().toLocaleTimeString(),
    tag: String(tag || "DEVICE"),
    message: String(message || ""),
    level: String(level || "info"),
  };
  recentDeviceLogs.push(entry);
  if (recentDeviceLogs.length > MAX_BUFFERED_DEVICE_LOGS) {
    recentDeviceLogs.splice(0, recentDeviceLogs.length - MAX_BUFFERED_DEVICE_LOGS);
  }
  try {
    if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
      window.dispatchEvent(new CustomEvent(DEVICE_LOG_EVENT, { detail: entry }));
    }
  } catch (_error) {
    // 非浏览器环境忽略事件广播
  }
  return entry;
}

export function readBufferedDeviceLogs() {
  return [...recentDeviceLogs];
}

export function clearBufferedDeviceLogs() {
  recentDeviceLogs.length = 0;
}

function clampTravel(val, min, max, fallback) {
  const num = Number(val);
  if (!Number.isFinite(num) || num <= 0) return fallback;
  return Math.min(max, Math.max(min, num));
}

function isStorageLike(candidate) {
  return Boolean(
    candidate
    && typeof candidate === "object"
    && (typeof candidate.getItem === "function" || typeof candidate.setItem === "function"),
  );
}

/**
 * 规范化当前连接目标键（串口 `serial:<port>:<baud>`，网络 `network:<host>:<telnetPort>`；未填目标返回 `""`）。
 * 用于将探测或手动填写的机器行程严格绑定到对应设备，切换设备后自动回退默认 100 mm。
 */
export function connectionTargetKey(form) {
  if (typeof form === "string") return form.trim();
  if (!form || typeof form !== "object") return "";
  const mode = form.connection_mode === "serial" ? "serial" : "network";
  if (mode === "serial") {
    const port = String(form.port ?? "").trim();
    if (!port) return "";
    const baud = Number(form.baudrate) || Number(DEFAULT_BAUDRATE);
    return `serial:${port}:${baud}`;
  }
  const host = String(form.network_host ?? "").trim();
  if (!host) return "";
  const telnet = Number(form.network_telnet_port) || Number(DEFAULT_TELNET_PORT);
  return `network:${host}:${telnet}`;
}

function validAxisValue(val, min, max) {
  const num = Number(val);
  if (val === null || val === undefined || val === "" || !Number.isFinite(num) || num <= 0) {
    return undefined;
  }
  return Math.min(max, Math.max(min, num));
}

function normalizeAxisTriple(raw) {
  if (!raw || typeof raw !== "object") return {};
  const out = {};
  const x = validAxisValue(raw.x ?? raw.x_travel_mm, 20, MAX_XY_TRAVEL_MM);
  const y = validAxisValue(raw.y ?? raw.y_travel_mm, 20, MAX_XY_TRAVEL_MM);
  const z = validAxisValue(raw.z ?? raw.z_travel_mm, 5, MAX_Z_TRAVEL_MM);
  if (x !== undefined) out.x = x;
  if (y !== undefined) out.y = y;
  if (z !== undefined) out.z = z;
  return out;
}

function pruneTargetMap(targets, maxCount = MAX_SAVED_TARGETS) {
  const entries = Object.entries(targets || {}).filter(([k]) => Boolean(String(k || "").trim()));
  if (entries.length <= maxCount) {
    return Object.fromEntries(entries);
  }
  entries.sort((a, b) => (Number(a[1]?.updated_at) || 0) - (Number(b[1]?.updated_at) || 0));
  return Object.fromEntries(entries.slice(entries.length - maxCount));
}

function nextUpdatedAt(targets) {
  let maxTs = Date.now();
  for (const entry of Object.values(targets || {})) {
    const ts = Number(entry?.updated_at) || 0;
    if (ts >= maxTs) maxTs = ts + 1;
  }
  return maxTs;
}

function defaultWorkspaceLimits(targetKey = "") {
  return {
    x_travel_mm: DEFAULT_XY_TRAVEL_MM,
    y_travel_mm: DEFAULT_XY_TRAVEL_MM,
    z_travel_mm: DEFAULT_Z_TRAVEL_MM,
    sources: { x: "default", y: "default", z: "default" },
    hardware: { x: null, y: null, z: null },
    manual_values: { x: null, y: null, z: null },
    has_manual: false,
    has_hardware: false,
    from_hardware: false,
    manual: false,
    target_key: targetKey,
  };
}

function parseWorkspaceLimitsStoreObject(parsed) {
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { targets: {} };
  }
  if (parsed.targets && typeof parsed.targets === "object" && !Array.isArray(parsed.targets)) {
    const normalized = {};
    for (const [rawKey, item] of Object.entries(parsed.targets)) {
      const key = String(rawKey || "").trim();
      if (!key || !item || typeof item !== "object") continue;
      const hardware = normalizeAxisTriple(item.hardware);
      const manual = normalizeAxisTriple(item.manual);
      if (!Object.keys(hardware).length && !Object.keys(manual).length) continue;
      normalized[key] = {
        hardware,
        manual,
        updated_at: Number.isFinite(Number(item.updated_at)) ? Number(item.updated_at) : 0,
      };
    }
    return { targets: pruneTargetMap(normalized) };
  }
  // 兼容旧格式单条记录：有 target_key 的按原来源迁移到对应槽位，没有 target_key 的直接丢弃
  const legacyKey = typeof parsed.target_key === "string" ? parsed.target_key.trim() : "";
  if (!legacyKey) {
    return { targets: {} };
  }
  const axes = normalizeAxisTriple(parsed);
  if (!Object.keys(axes).length) {
    return { targets: {} };
  }
  if (parsed.manual) {
    return { targets: { [legacyKey]: { hardware: {}, manual: axes, updated_at: 1 } } };
  }
  if (parsed.from_hardware) {
    return { targets: { [legacyKey]: { hardware: axes, manual: {}, updated_at: 1 } } };
  }
  return { targets: {} };
}

/** 读取按 `target_key` 分桶保存的机器行程映射（最多保留 16 个目标）。 */
export function readWorkspaceLimitsStore(storage = defaultStorage()) {
  try {
    const raw = storage?.getItem?.(WORKSPACE_LIMITS_KEY);
    if (!raw) return { targets: {} };
    return parseWorkspaceLimitsStoreObject(JSON.parse(raw));
  } catch (_error) {
    return { targets: {} };
  }
}

function persistWorkspaceLimitsStore(store, storage) {
  const clean = { targets: pruneTargetMap(store?.targets || {}) };
  try {
    storage?.setItem?.(WORKSPACE_LIMITS_KEY, JSON.stringify(clean));
  } catch (_error) {
    // 存储受限时仅在当前会话广播
  }
  try {
    if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
      window.dispatchEvent(new CustomEvent(WORKSPACE_LIMITS_EVENT, { detail: clean }));
    }
  } catch (_error) {
    // 非浏览器环境忽略事件广播
  }
  return clean;
}

/**
 * 按当前连接目标解析有效机器行程：同一目标逐轴按“手填 → 固件 → 默认（100/100/20 mm）”取值，
 * 并返回每轴来源（`manual` / `hardware` / `default`）与原始固件/手填槽位供界面展示。
 */
export function resolveWorkspaceLimitsForTarget(storeOrLimits, target) {
  const expectedKey = connectionTargetKey(target);
  if (!expectedKey) {
    return defaultWorkspaceLimits("");
  }
  const store = isStorageLike(storeOrLimits)
    ? readWorkspaceLimitsStore(storeOrLimits)
    : parseWorkspaceLimitsStoreObject(storeOrLimits);
  const entry = store.targets[expectedKey];
  if (!entry) {
    return defaultWorkspaceLimits(expectedKey);
  }
  const hw = entry.hardware || {};
  const man = entry.manual || {};
  const pickAxis = (axis, fallback) => {
    if (man[axis] !== undefined) return { value: man[axis], source: "manual" };
    if (hw[axis] !== undefined) return { value: hw[axis], source: "hardware" };
    return { value: fallback, source: "default" };
  };
  const rx = pickAxis("x", DEFAULT_XY_TRAVEL_MM);
  const ry = pickAxis("y", DEFAULT_XY_TRAVEL_MM);
  const rz = pickAxis("z", DEFAULT_Z_TRAVEL_MM);
  const sources = { x: rx.source, y: ry.source, z: rz.source };
  const hasManual = sources.x === "manual" || sources.y === "manual" || sources.z === "manual";
  const hasHardware = hw.x !== undefined || hw.y !== undefined || hw.z !== undefined;
  return {
    x_travel_mm: rx.value,
    y_travel_mm: ry.value,
    z_travel_mm: rz.value,
    sources,
    hardware: { x: hw.x ?? null, y: hw.y ?? null, z: hw.z ?? null },
    manual_values: { x: man.x ?? null, y: man.y ?? null, z: man.z ?? null },
    has_manual: hasManual,
    has_hardware: hasHardware,
    from_hardware: sources.x === "hardware" || sources.y === "hardware" || sources.z === "hardware",
    manual: hasManual,
    target_key: expectedKey,
  };
}

/** 读取本机保存的雕刻机行程：传入 `target` 时返回该目标的解析结果，省略时返回完整存储映射 `{ targets }`。 */
export function readWorkspaceLimits(storage = defaultStorage(), target = undefined) {
  const store = readWorkspaceLimitsStore(storage);
  if (target !== undefined) {
    return resolveWorkspaceLimitsForTarget(store, target);
  }
  return store;
}

function resolveTargetAndStorage(targetOrStorage, maybeStorage) {
  const hasStorageSecondArg = isStorageLike(targetOrStorage) && maybeStorage === undefined;
  const resolvedStorage = hasStorageSecondArg
    ? targetOrStorage
    : maybeStorage !== undefined
      ? maybeStorage
      : defaultStorage();
  const targetKey = hasStorageSecondArg || targetOrStorage === undefined
    ? connectionTargetKey(readUnifiedConnection(resolvedStorage))
    : connectionTargetKey(targetOrStorage);
  return { resolvedStorage, targetKey };
}

/**
 * 写入指定连接目标的手填行程（只改 `manual` 槽位，绝不覆盖固件上报的 `hardware` 值）。
 */
export function writeManualWorkspaceLimits(patch = {}, targetOrStorage = undefined, maybeStorage = undefined) {
  const { resolvedStorage, targetKey: argKey } = resolveTargetAndStorage(targetOrStorage, maybeStorage);
  const targetKey = patch?.target !== undefined
    ? connectionTargetKey(patch.target)
    : typeof patch?.target_key === "string" && patch.target_key.trim()
      ? patch.target_key.trim()
      : argKey;
  if (!targetKey) {
    return defaultWorkspaceLimits("");
  }
  const store = readWorkspaceLimitsStore(resolvedStorage);
  const prev = store.targets[targetKey] || { hardware: {}, manual: {}, updated_at: 0 };
  const nextManual = { ...(prev.manual || {}) };
  const applyAxis = (rawVal, axis, min, max) => {
    if (rawVal === undefined) return;
    if (rawVal === null || rawVal === "") {
      delete nextManual[axis];
      return;
    }
    const clamped = clampTravel(rawVal, min, max, null);
    if (clamped !== null) {
      nextManual[axis] = clamped;
    }
  };
  applyAxis(patch?.x ?? patch?.x_travel_mm, "x", 20, MAX_XY_TRAVEL_MM);
  applyAxis(patch?.y ?? patch?.y_travel_mm, "y", 20, MAX_XY_TRAVEL_MM);
  applyAxis(patch?.z ?? patch?.z_travel_mm, "z", 5, MAX_Z_TRAVEL_MM);

  const nextTargets = { ...store.targets };
  if (!Object.keys(prev.hardware || {}).length && !Object.keys(nextManual).length) {
    delete nextTargets[targetKey];
  } else {
    nextTargets[targetKey] = {
      hardware: { ...(prev.hardware || {}) },
      manual: nextManual,
      updated_at: nextUpdatedAt(store.targets),
    };
  }
  const savedStore = persistWorkspaceLimitsStore({ targets: nextTargets }, resolvedStorage);
  return resolveWorkspaceLimitsForTarget(savedStore, targetKey);
}

/** 清除当前连接目标的手填行程（“恢复为固件值”），保留已读取的固件行程。 */
export function clearManualWorkspaceLimits(targetOrStorage = undefined, maybeStorage = undefined) {
  const { resolvedStorage, targetKey } = resolveTargetAndStorage(targetOrStorage, maybeStorage);
  if (!targetKey) {
    return defaultWorkspaceLimits("");
  }
  const store = readWorkspaceLimitsStore(resolvedStorage);
  const prev = store.targets[targetKey];
  if (!prev) {
    return defaultWorkspaceLimits(targetKey);
  }
  const nextTargets = { ...store.targets };
  if (!Object.keys(prev.hardware || {}).length) {
    delete nextTargets[targetKey];
  } else {
    nextTargets[targetKey] = {
      hardware: { ...prev.hardware },
      manual: {},
      updated_at: nextUpdatedAt(store.targets),
    };
  }
  const savedStore = persistWorkspaceLimitsStore({ targets: nextTargets }, resolvedStorage);
  return resolveWorkspaceLimitsForTarget(savedStore, targetKey);
}

/**
 * 硬件握手成功后同步当前连接目标的固件行程：
 * - 只写入该目标的 `hardware` 槽位，绝不覆盖用户已填写的 `manual` 值；
 * - 握手未报行程或某轴未报时，清空该轴 `hardware`，无手填值时自动回退默认 100/20 mm。
 */
export function syncHardwareWorkspaceLimits(machineLimits, targetOrStorage = undefined, maybeStorage = undefined) {
  const { resolvedStorage, targetKey } = resolveTargetAndStorage(targetOrStorage, maybeStorage);
  if (!targetKey) {
    return defaultWorkspaceLimits("");
  }
  const store = readWorkspaceLimitsStore(resolvedStorage);
  const prev = store.targets[targetKey] || { hardware: {}, manual: {}, updated_at: 0 };
  const nextHardware = {};
  const x = validAxisValue(machineLimits?.x_travel_mm, 20, MAX_XY_TRAVEL_MM);
  const y = validAxisValue(machineLimits?.y_travel_mm, 20, MAX_XY_TRAVEL_MM);
  const z = validAxisValue(machineLimits?.z_travel_mm, 5, MAX_Z_TRAVEL_MM);
  if (x !== undefined) nextHardware.x = x;
  if (y !== undefined) nextHardware.y = y;
  if (z !== undefined) nextHardware.z = z;

  const nextTargets = { ...store.targets };
  const nextManual = { ...(prev.manual || {}) };
  if (!Object.keys(nextHardware).length && !Object.keys(nextManual).length) {
    delete nextTargets[targetKey];
  } else {
    nextTargets[targetKey] = {
      hardware: nextHardware,
      manual: nextManual,
      updated_at: nextUpdatedAt(store.targets),
    };
  }
  const savedStore = persistWorkspaceLimitsStore({ targets: nextTargets }, resolvedStorage);
  return resolveWorkspaceLimitsForTarget(savedStore, targetKey);
}

/** 生成界面显示的行程来源短文案（如“固件 300×200”、“手填 400×300”、“默认 100”）。 */
export function formatWorkspaceLimitsLabel(resolved) {
  const x = Number(resolved?.x_travel_mm) || DEFAULT_XY_TRAVEL_MM;
  const y = Number(resolved?.y_travel_mm) || DEFAULT_XY_TRAVEL_MM;
  const sx = resolved?.sources?.x || "default";
  const sy = resolved?.sources?.y || "default";
  const nameOf = (src) => (src === "manual" ? "手填" : src === "hardware" ? "固件" : "默认");
  if (sx === sy) {
    if (sx === "default" && x === DEFAULT_XY_TRAVEL_MM && y === DEFAULT_XY_TRAVEL_MM) {
      return `默认 ${DEFAULT_XY_TRAVEL_MM}`;
    }
    return `${nameOf(sx)} ${x}×${y}`;
  }
  return `X ${nameOf(sx)} ${x} · Y ${nameOf(sy)} ${y}`;
}

/** 当用户手填值大于该连接目标固件上报的行程时，返回安全提示文案；否则返回空串。 */
export function workspaceLimitsExceedsHardwareWarning(resolved) {
  const hwX = resolved?.hardware?.x;
  const hwY = resolved?.hardware?.y;
  const exceedsX = resolved?.sources?.x === "manual" && Number.isFinite(hwX) && resolved.x_travel_mm > hwX;
  const exceedsY = resolved?.sources?.y === "manual" && Number.isFinite(hwY) && resolved.y_travel_mm > hwY;
  if (!exceedsX && !exceedsY) return "";
  if (Number.isFinite(hwX) && Number.isFinite(hwY)) {
    return `大于固件报告的 ${hwX}×${hwY} mm，发送时仍按固件行程拦截`;
  }
  if (Number.isFinite(hwX)) {
    return `大于固件报告的 X ${hwX} mm，发送时仍按固件行程拦截`;
  }
  return `大于固件报告的 Y ${hwY} mm，发送时仍按固件行程拦截`;
}

function normalizeStoredFeatures(raw) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const out = {};
  if (Array.isArray(raw.axes)) {
    out.axes = raw.axes.map((a) => String(a || "").trim().toLowerCase()).filter(Boolean);
  }
  if (typeof raw.has_air_assist === "boolean") out.has_air_assist = raw.has_air_assist;
  if (typeof raw.has_door === "boolean") out.has_door = raw.has_door;
  if (typeof raw.has_estop_or_fault_pin === "boolean") out.has_estop_or_fault_pin = raw.has_estop_or_fault_pin;
  if (typeof raw.speed_map === "string" && raw.speed_map.trim()) out.speed_map = raw.speed_map.trim();
  return Object.keys(out).length ? out : null;
}

export function readMachineFeaturesStore(storage = defaultStorage()) {
  try {
    const raw = storage?.getItem?.(MACHINE_FEATURES_KEY);
    if (!raw) return { targets: {} };
    const parsed = JSON.parse(raw);
    if (!parsed?.targets || typeof parsed.targets !== "object" || Array.isArray(parsed.targets)) {
      return { targets: {} };
    }
    const normalized = {};
    for (const [k, entry] of Object.entries(parsed.targets)) {
      const key = String(k || "").trim();
      const features = normalizeStoredFeatures(entry?.features ?? entry);
      if (key && features) {
        normalized[key] = {
          features,
          updated_at: Number.isFinite(Number(entry?.updated_at)) ? Number(entry.updated_at) : 0,
        };
      }
    }
    return { targets: pruneTargetMap(normalized) };
  } catch (_error) {
    return { targets: {} };
  }
}

export function resolveMachineFeaturesForTarget(storeOrStorage, target) {
  const key = connectionTargetKey(target);
  if (!key) return null;
  const store = isStorageLike(storeOrStorage)
    ? readMachineFeaturesStore(storeOrStorage)
    : storeOrStorage?.targets && typeof storeOrStorage.targets === "object"
      ? storeOrStorage
      : { targets: {} };
  return store.targets[key]?.features || null;
}

export function syncHardwareMachineFeatures(features, targetOrStorage = undefined, maybeStorage = undefined) {
  const { resolvedStorage, targetKey } = resolveTargetAndStorage(targetOrStorage, maybeStorage);
  if (!targetKey) return null;
  const store = readMachineFeaturesStore(resolvedStorage);
  const nextTargets = { ...store.targets };
  const clean = normalizeStoredFeatures(features);
  if (clean) {
    nextTargets[targetKey] = {
      features: clean,
      updated_at: nextUpdatedAt(store.targets),
    };
  } else {
    delete nextTargets[targetKey];
  }
  const saved = { targets: pruneTargetMap(nextTargets) };
  try {
    resolvedStorage?.setItem?.(MACHINE_FEATURES_KEY, JSON.stringify(saved));
  } catch (_error) {
    // ignore
  }
  try {
    if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
      window.dispatchEvent(new CustomEvent(MACHINE_FEATURES_EVENT, { detail: saved }));
    }
  } catch (_error) {
    // ignore
  }
  return clean;
}

function defaultStorage() {
  try {
    return globalThis.localStorage ?? null;
  } catch (_error) {
    return null;
  }
}

function normalizeFormObject(raw = {}) {
  return {
    connection_mode: raw.connection_mode === "serial" ? "serial" : "network",
    network_host: String(raw.network_host ?? ""),
    network_telnet_port: String(raw.network_telnet_port || DEFAULT_TELNET_PORT),
    port: String(raw.port ?? ""),
    baudrate: String(raw.baudrate || DEFAULT_BAUDRATE),
  };
}

function hasConfiguredTarget(raw) {
  if (!raw || typeof raw !== "object") return false;
  const mode = raw.connection_mode === "serial" ? "serial" : "network";
  return mode === "serial"
    ? Boolean(raw.port && String(raw.port).trim())
    : Boolean(raw.network_host && String(raw.network_host).trim());
}

function readKey(storage, key) {
  try {
    return storage?.getItem?.(key) ?? null;
  } catch (_error) {
    return null;
  }
}

/** 原样读出一组键（没存过为 null），供需要区分“没填”与“填了默认值”的调用方使用。 */
export function readRawConnection(keys, storage = defaultStorage()) {
  return Object.fromEntries(Object.entries(keys).map(([field, key]) => [field, readKey(storage, key)]));
}

/** 读一组键为连接表单（字符串）：模式只认 serial / network，端口与波特率空时给默认值。 */
export function readConnectionForm(keys, storage = defaultStorage()) {
  const raw = readRawConnection(keys, storage);
  return {
    connection_mode: raw.connection_mode === "serial" ? "serial" : "network",
    network_host: raw.network_host || "",
    network_telnet_port: raw.network_telnet_port || DEFAULT_TELNET_PORT,
    port: raw.port || "",
    baudrate: raw.baudrate || DEFAULT_BAUDRATE,
  };
}

/** 工作台保存的连接表单。 */
export function readWorkbenchConnection(storage = defaultStorage()) {
  return readConnectionForm(WORKBENCH_CONNECTION_KEYS, storage);
}

/** 写回工作台连接表单里给出的字段（值转字符串，null / undefined 存空串）；写不进去时静默跳过。 */
export function writeWorkbenchConnection(partial, storage = defaultStorage()) {
  for (const [field, value] of Object.entries(partial ?? {})) {
    const key = WORKBENCH_CONNECTION_KEYS[field];
    if (!key) continue;
    try {
      storage?.setItem?.(key, String(value ?? ""));
    } catch (_error) {
      // 存储受限时仅在当前会话生效
    }
  }
}

/**
 * 全局统一读取连接设置（2026-11-23 r58）：
 * 先按已填有效目标（host / port）优先选 `laser-forge-*`，未填时回退历史画板键 `laserLab.*`；
 * 默认浏览器存储被禁用或抛错时自动回退当次会话内存副本，保证全站页面与 AI 预览始终一致。
 */
export function readUnifiedConnection(storage = USE_DEFAULT_STORAGE) {
  const isDefault = storage === USE_DEFAULT_STORAGE;
  const resolved = isDefault ? defaultStorage() : storage;
  const wbRaw = readRawConnection(WORKBENCH_CONNECTION_KEYS, resolved);
  const drawRaw = readRawConnection(DRAW_CONNECTION_KEYS, resolved);
  if (hasConfiguredTarget(wbRaw)) {
    return readConnectionForm(WORKBENCH_CONNECTION_KEYS, resolved);
  }
  if (hasConfiguredTarget(drawRaw)) {
    return readConnectionForm(DRAW_CONNECTION_KEYS, resolved);
  }
  if (wbRaw.connection_mode) {
    return readConnectionForm(WORKBENCH_CONNECTION_KEYS, resolved);
  }
  if (drawRaw.connection_mode) {
    return readConnectionForm(DRAW_CONNECTION_KEYS, resolved);
  }
  if (isDefault && sessionFallbackForm) {
    return { ...sessionFallbackForm };
  }
  return readConnectionForm(WORKBENCH_CONNECTION_KEYS, resolved);
}

/**
 * 全局统一写入连接设置：只写入单一存储源 `laser-forge-*`，清理旧 `laserLab.*` 副本，并在默认存储下同步会话内存兜底。
 */
export function writeUnifiedConnection(partial, storage = USE_DEFAULT_STORAGE) {
  const isDefault = storage === USE_DEFAULT_STORAGE;
  const resolved = isDefault ? defaultStorage() : storage;
  const base = isDefault && sessionFallbackForm
    ? sessionFallbackForm
    : readUnifiedConnection(resolved);
  const merged = normalizeFormObject({ ...base, ...(partial ?? {}) });
  if (isDefault) {
    sessionFallbackForm = merged;
  }
  writeWorkbenchConnection(merged, resolved);
  for (const key of Object.values(DRAW_CONNECTION_KEYS)) {
    try {
      resolved?.removeItem?.(key);
    } catch (_error) {
      // 存储受限时跳过清理
    }
  }
}

/** 构造 `/api/connection/check` 握手请求载荷。 */
export function buildConnectionCheckPayload(form) {
  const mode = form?.connection_mode === "serial" ? "serial" : "network";
  if (mode === "serial") {
    return {
      connection_mode: "serial",
      port: String(form?.port ?? "").trim(),
      baudrate: Number(form?.baudrate) || Number(DEFAULT_BAUDRATE),
    };
  }
  return {
    connection_mode: "network",
    network_host: String(form?.network_host ?? "").trim(),
    network_telnet_port: Number(form?.network_telnet_port) || Number(DEFAULT_TELNET_PORT),
  };
}

/**
 * 统一执行一次硬件只读握手探测（`/api/connection/check`），供启动静默握手、统一连接弹窗与工作台开刻门禁共用。
 */
export async function probeControllerConnection(postJsonFn, form, now = Date.now()) {
  const mode = form?.connection_mode === "serial" ? "serial" : "network";
  const payload = buildConnectionCheckPayload(form);
  try {
    const res = await postJsonFn("/api/connection/check", payload);
    if (res?.ok && res?.data?.success) {
      const result = res.data.result || {};
      const connected = mode === "serial" ? Boolean(result.serial_connected) : Boolean(result.network_connected);
      const summary = result.summary || (connected ? "控制器已连接 ($32=1 就绪)" : "未检测到控制器连接");
      const text = connected
        ? `硬件握手通过：${summary}；语音和 AI 发送时没指定串口或 IP 就用这个连接`
        : summary;
      return {
        connected,
        status: {
          connected,
          summary: result.summary,
          checkedAt: now,
          isFluidnc: Boolean(result.features),
          features: result.features || null,
        },
        bannerTone: connected ? "ok" : "bad",
        logLevel: connected ? "ok" : "warn",
        text,
        machineLimits: result.machine_limits || null,
      };
    }
    const errMsg = res?.data?.result || "探测失败：未检测到控制器连接";
    return {
      connected: false,
      status: { connected: false, summary: errMsg, checkedAt: now, isFluidnc: false, features: null },
      bannerTone: "bad",
      logLevel: "error",
      text: errMsg,
      machineLimits: null,
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return {
      connected: false,
      status: { connected: false, summary: "连接探测网络异常", checkedAt: now, isFluidnc: false, features: null },
      bannerTone: "bad",
      logLevel: "error",
      text: `连接探测网络异常：${message}`,
      machineLimits: null,
    };
  }
}

/**
 * 当前连接的后端宿主是否支持 USB 串口：
 * 串口能力属于后端宿主——仅当运行在 Tauri Android App 内且未开启“连接电脑 MOSS”时隐藏串口；
 * 电脑桌面端、电脑浏览器，以及安卓手机浏览器通过 `http://<device-ip>:<port>` 访问电脑端时均保留串口。
 */
export function isHostSerialAvailable({
  win = globalThis.window,
  nav = globalThis.navigator,
  remote = isRemoteMode(),
} = {}) {
  if (remote) return true;
  const inLocalAndroidApp = isTauriHost(win) && detectClientOs(nav) === "android";
  return !inLocalAndroidApp;
}

/** 工作台连接卡、画板连接卡与画板底部操作条共用的紧凑连接摘要视图。 */
export function connectionSummaryView(form, status) {
  const serial = form?.connection_mode === "serial";
  const modeLabel = serial ? "串口" : "网络";
  const host = String(form?.network_host ?? "").trim();
  const port = String(form?.port ?? "").trim();
  const telnetPort = Number(form?.network_telnet_port) || Number(DEFAULT_TELNET_PORT);
  const baud = Number(form?.baudrate) || Number(DEFAULT_BAUDRATE);
  const targetReady = serial ? Boolean(port) : Boolean(host);
  const targetText = serial
    ? (targetReady ? `${port} · ${baud}` : "未选串口")
    : (targetReady ? `${host}:${telnetPort}` : "未填 IP");
  if (!status?.checkedAt) {
    return { modeLabel, targetReady, targetText, statusText: "未探测", statusTone: "off" };
  }
  if (status.connected) {
    return { modeLabel, targetReady, targetText, statusText: "握手通过", statusTone: "ok" };
  }
  return {
    modeLabel,
    targetReady,
    targetText,
    statusText: status.summary || "未连接",
    statusTone: "bad",
  };
}
