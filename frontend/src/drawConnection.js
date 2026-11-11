// 创作画板的设备连接、成品尺寸、预览过期判定与场景导入校验（2026-10-14；2026-11-11 增补防呆与互通）。
// 支持网口（Telnet）和串口；宽 / 高留空表示按画板导出尺寸，不再用 1 mm 兜底。

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

/** 判断 `/api/connection/check` 响应在当前所选连接方式（串口或网络）下是否真正握手连通。 */
export function isDrawConnectionOk(form, result) {
  if (!result?.success) return false;
  const payload = result.result && typeof result.result === "object" ? result.result : {};
  return isSerial(form) ? Boolean(payload.serial_connected) : Boolean(payload.network_connected);
}

/**
 * 构造创作画板走边框请求载荷：统一附带 `relative_to_head: true`，
 * 与画板正式雕刻刀路开头的 `G92 X0 Y0 Z0`（以激光头当前位置为左下角起点）保持同一参照系。
 */
export function buildDrawFramingPayload(form, width, height, framingLight = false) {
  return {
    ...drawConnectionPayload(form),
    relative_to_head: true,
    min_x: 0,
    min_y: 0,
    max_x: Number(width) || 100,
    max_y: Number(height) || 100,
    feed_rate: 2000,
    power: framingLight ? 1 : 0,
  };
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
 * 画板表单默认值（2026-11-05；2026-11-11 互通升级）：
 * 优先从 `laserLab.*` 读画板自己的记忆；若画板尚未保存过任何设备地址/串口，自动回退复用雕刻工作台已填好的 `laser-forge-*` 连接参数。
 * 存储被禁用或读取抛错时逐项退回内置默认值，页面照常加载。
 */
export function readDrawDefaultForm(storage = safeBrowserStorage()) {
  const read = (fullKey) => {
    try {
      return storage?.getItem?.(fullKey) ?? null;
    } catch (_error) {
      return null;
    }
  };
  const labModeRaw = read("laserLab.connection_mode");
  const labHost = read("laserLab.network_host") || "";
  const labPort = read("laserLab.port") || "";
  const hasLabTarget = Boolean(labModeRaw || labHost.trim() || labPort.trim());

  const forgeConnMode = read("laser-forge-conn-mode") === "serial" ? "serial" : "network";
  const connection_mode = hasLabTarget
    ? (labModeRaw === "serial" ? "serial" : "network")
    : forgeConnMode;
  const network_host = labHost || read("laser-forge-host") || "";
  const network_telnet_port = read("laserLab.network_telnet_port") || read("laser-forge-telnet-port") || "23";
  const port = labPort || read("laser-forge-serial-port") || "";
  const baudrate = read("laserLab.baudrate") || read("laser-forge-baud") || "115200";

  return {
    material: read("laserLab.material") || "椴木",
    thickness_mm: read("laserLab.thickness_mm") || "3",
    width_mm: read("laserLab.width_mm") || "",
    height_mm: read("laserLab.height_mm") || "",
    mode: normalizeDrawMode(read("laserLab.mode")),
    connection_mode,
    network_host,
    network_telnet_port,
    port,
    baudrate,
    vector_simplify_factor: read("laserLab.vector_simplify_factor") || "3",
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
  return `将连接 ${drawTargetLabel(form)}，机器会实际移动，从激光头当前位置（左下角起点）沿 ${size} 的边框走一圈再回到原处。${light}\n请确认行程内无遮挡、手已离开机台；随时可按顶栏急停。`;
}

/** 计算当前 Excalidraw 画布有效元素的轻量修订签名，用于感知增删改笔迹而无需整图重导出。 */
export function computeSceneRevision(elements) {
  if (!Array.isArray(elements) || elements.length === 0) return "empty";
  const parts = [];
  for (const el of elements) {
    if (!el || el.isDeleted) continue;
    parts.push(`${el.id || ""}:${el.version || 0}:${el.versionNonce || 0}`);
  }
  return parts.length === 0 ? "empty" : parts.join("|");
}

/** 计算影响画板刀路生成的左侧参数签名（材料、厚度、宽高、生成策略、平滑度与临时自定义功率速度）。 */
export function drawGenerationSignature(form, custom = {}) {
  const mode = normalizeDrawMode(form?.mode);
  return JSON.stringify({
    material: String(form?.material ?? "").trim(),
    thickness_mm: String(form?.thickness_mm ?? "").trim(),
    width_mm: String(form?.width_mm ?? "").trim(),
    height_mm: String(form?.height_mm ?? "").trim(),
    mode,
    simplify: mode === "outline" ? String(form?.vector_simplify_factor ?? "3").trim() : "",
    manual: custom?.enabled
      ? `${String(custom.power ?? "").trim()}/${String(custom.feed ?? "").trim()}/${String(custom.passes ?? "").trim()}`
      : "preset",
  });
}

/** 判断已生成的画板预览是否因画布重画或参数修改而过期。 */
export function isDrawPreviewStale(savedSnapshot, currentSnapshot) {
  if (!savedSnapshot?.formSig) return false;
  return (
    savedSnapshot.formSig !== currentSnapshot?.formSig
    || savedSnapshot.sceneRev !== currentSnapshot?.sceneRev
  );
}

/** 校验并提取导入的 `.excalidraw` / `.json` 场景数据。 */
export function parseImportedSceneJson(rawText) {
  let parsed;
  try {
    parsed = JSON.parse(String(rawText ?? ""));
  } catch {
    return { error: "场景文件不是合法的 JSON 格式" };
  }
  if (!parsed || typeof parsed !== "object" || !Array.isArray(parsed.elements)) {
    return { error: "场景文件缺少 elements 画布元素列表，无法导入" };
  }
  const appState = parsed.appState && typeof parsed.appState === "object" ? parsed.appState : {};
  const files = parsed.files && typeof parsed.files === "object" ? parsed.files : null;
  return {
    elements: parsed.elements,
    appState: {
      ...(typeof appState.gridSize === "number" ? { gridSize: appState.gridSize } : {}),
      ...(typeof appState.viewBackgroundColor === "string" ? { viewBackgroundColor: appState.viewBackgroundColor } : {}),
    },
    files,
  };
}
