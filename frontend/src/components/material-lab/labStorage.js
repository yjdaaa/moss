// 材料实验室读写本机存储（与工作台共享连接与材料选择键）。原样搬自 pages/MaterialLabPage.jsx（2026-11-16 按职责拆分）

export function readLocalSafe(key, fallback = "") {
  try {
    const val = globalThis.localStorage?.getItem(key);
    return val == null ? fallback : val;
  } catch {
    return fallback;
  }
}

export function writeLocalSafe(key, value) {
  try {
    globalThis.localStorage?.setItem(key, String(value ?? ""));
  } catch {
    // 存储被禁用时静默跳过
  }
}
