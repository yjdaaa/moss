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

/** 连接表单字段 → 创作画板存储键。 */
export const DRAW_CONNECTION_KEYS = Object.freeze({
  connection_mode: "laserLab.connection_mode",
  network_host: "laserLab.network_host",
  network_telnet_port: "laserLab.network_telnet_port",
  port: "laserLab.port",
  baudrate: "laserLab.baudrate",
});

export const DEFAULT_TELNET_PORT = "23";
export const DEFAULT_BAUDRATE = "115200";

function defaultStorage() {
  try {
    return globalThis.localStorage ?? null;
  } catch (_error) {
    return null;
  }
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
