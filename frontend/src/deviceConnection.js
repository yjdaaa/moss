// 设备连接参数的本机存储（2026-11-21 第三批重构 3B）：工作台、材料实验室、数字孪生、创作画板与 AI 预览
// 以前各自手写同一组 localStorage 键，这里收口成一处。
//
// - 工作台一组（`laser-forge-*`）：雕刻工作台与材料实验室共用，数字孪生只读；
// - 创作画板一组（`laserLab.*`）：画板自己的记忆，没填时回退工作台那一组（见 drawConnection.js）。
// 存储被禁用或读写抛错时逐项退回默认值 / 静默跳过，页面照常工作，急停路径也不会因此中断。

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
