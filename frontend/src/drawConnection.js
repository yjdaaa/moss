// 创作画板的设备连接与成品尺寸（2026-10-14）：与雕刻工作台一样支持网口（Telnet）和串口，
// 宽 / 高留空表示按画板导出尺寸，不再用 1 mm 兜底。

export const DRAW_MODES = ["raster", "outline"];

/** 旧版本可能存了已移除的 "auto"，一律按光栅处理。 */
export function normalizeDrawMode(mode) {
  return DRAW_MODES.includes(mode) ? mode : "raster";
}

/** 宽 / 高输入：空或非正数为 null（后端按“自动”处理）。 */
export function drawSizeValue(value) {
  if (String(value ?? "").trim() === "") return null;
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : null;
}

export function isSerial(form) {
  return form?.connection_mode === "serial";
}

/** 发送、走边框、测试连接共用的连接字段。 */
export function drawConnectionPayload(form) {
  if (isSerial(form)) {
    return { connection_mode: "serial", port: String(form.port || "").trim(), baudrate: Number(form.baudrate) || 115200 };
  }
  return {
    connection_mode: "network",
    network_host: String(form.network_host || "").trim(),
    network_telnet_port: Number(form.network_telnet_port) || 23,
  };
}

/** 是否已填好连接目标（串口名或设备地址）。 */
export function drawTargetReady(form) {
  return isSerial(form) ? Boolean(String(form.port || "").trim()) : Boolean(String(form.network_host || "").trim());
}

/** 连接目标的显示文字。 */
export function drawTargetLabel(form) {
  if (isSerial(form)) return `串口 ${String(form.port || "").trim() || "未选择"} · ${Number(form.baudrate) || 115200}`;
  return `${String(form.network_host || "").trim()}:${Number(form.network_telnet_port) || 23}`;
}

/** 目标没填时的提示。 */
export function drawTargetMissingText(form) {
  return isSerial(form) ? "请先选择串口" : "请先填写设备 host/IP";
}

/** 走边框响应（按 client.js `{ status, ok, data }` 契约）转界面状态文案。 */
export function formatFramingStatus(res, width, height, framingLight) {
  if (res?.ok && res?.data?.success) {
    return `走边框完成 (${width.toFixed(1)} x ${height.toFixed(1)} mm${framingLight ? "，弱光" : "，未出光"})`;
  }
  const errMsg = typeof res?.data?.result === "string" && res.data.result ? res.data.result : "未知错误";
  return `走边框未完成：${errMsg}`;
}

/**
 * 画板表单默认值（2026-11-05）：从 `laserLab.*` 读上次保存的配置；存储被禁用或读取抛错时
 * 逐项退回内置默认值，页面照常加载。
 */
export function readDrawDefaultForm(storage = safeBrowserStorage()) {
  const read = (key) => {
    try {
      return storage?.getItem?.(`laserLab.${key}`) ?? null;
    } catch (_error) {
      return null;
    }
  };
  return {
    material: read("material") || "椴木",
    thickness_mm: read("thickness_mm") || "3",
    width_mm: read("width_mm") || "",
    height_mm: read("height_mm") || "",
    mode: normalizeDrawMode(read("mode")),
    connection_mode: read("connection_mode") === "serial" ? "serial" : "network",
    network_host: read("network_host") || "",
    network_telnet_port: read("network_telnet_port") || "23",
    port: read("port") || "",
    baudrate: read("baudrate") || "115200",
    vector_simplify_factor: read("vector_simplify_factor") || "3",
    output_format: "gcode",
  };
}

/** 取浏览器 localStorage；访问本身抛错（存储被禁用、沙箱）时返回 null。 */
export function safeBrowserStorage() {
  try {
    return globalThis.localStorage || null;
  } catch (_error) {
    return null;
  }
}

/**
 * 走边框确认文案（2026-11-05 用户决定：走边框会移动机器，必须先确认；勾选弱光时说明会低功率出光）。
 */
export function framingConfirmText(form, width, height, framingLight) {
  const size = `${Number(width).toFixed(1)} × ${Number(height).toFixed(1)} mm`;
  const light = framingLight
    ? "已勾选“弱光指示”：走边框期间激光会以 S1（约 0.1%）低功率出光，请佩戴护目镜、确认材料与周围无易燃物。"
    : "激光保持关闭，只移动激光头。";
  return `将连接 ${drawTargetLabel(form)}，机器会实际移动，激光头沿 ${size} 的边框走一圈。${light}\n请确认行程内无遮挡、手已离开机台；随时可按顶栏急停。`;
}
