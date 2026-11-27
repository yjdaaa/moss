/**
 * AI 生成任务的桌面预览窗（2026-11-06 用户决定）的纯逻辑：不依赖 React，便于单元测试。
 *
 * 不管是语音小智、小智官方接入点还是网关（打字的 AI 客户端）生成的雕刻任务，后端都在 workflow
 * 记录里写下 `origin`；桌面端轮询最近任务，把 AI 来源、仍是 `preview_ready` 的任务弹出预览窗。
 * - 严格模式开启：AI 口头“确认”只会生成审批，必须在预览窗（或审批卡）点同意才开刻；
 * - 严格模式关闭：用户也可以对小智说“确认”，AI 调 confirm_send 后预览窗切到发送进度。
 */
import { readUnifiedConnection } from "./deviceConnection.js";
import { drawConnectionPayload, drawTargetLabel, drawTargetMissingText, drawTargetReady } from "./drawConnection.js";
import { normalizeWorkflowStatus } from "./state/statusNormalize.js";

export const AI_PREVIEW_POLL_MS = 3000;
/** 打开 MOSS 前多久内生成的 AI 预览仍会弹出（秒）。 */
export const AI_PREVIEW_LOOKBACK_SECONDS = 120;

const ORIGIN_LABEL = {
  voice: "MOSS 语音",
  mcp_bridge: "小智",
  gateway: "小智网关 / AI 客户端",
};

export function isAiOrigin(origin) {
  return Object.prototype.hasOwnProperty.call(ORIGIN_LABEL, origin);
}

export function aiOriginLabel(origin) {
  return ORIGIN_LABEL[origin] || "AI";
}

/** 已开始发送（AI 口头确认后或别处点了开始）的状态。 */
export function aiWorkflowStarted(record) {
  return ["preparing", "running", "sending"].includes(normalizeWorkflowStatus(record?.status));
}

/**
 * 需要弹出的 AI 预览：AI 来源、仍待确认、在 `since`（服务器秒）之后生成、没被用户关掉、
 * 也没有进入严格模式审批（审批卡会单独弹出，避免同一任务两个窗口）。最新的在前。
 */
export function pendingAiPreviews(workflows, { since = 0, dismissed = new Set(), approvalTargets = new Set() } = {}) {
  if (!Array.isArray(workflows)) return [];
  return workflows
    .filter((record) => record && typeof record === "object" && typeof record.workflow_id === "string")
    .filter((record) => isAiOrigin(record.origin))
    .filter((record) => normalizeWorkflowStatus(record.status) === "preview_ready")
    .filter((record) => Number(record.created_at) >= since)
    .filter((record) => !dismissed.has(record.workflow_id) && !approvalTargets.has(record.workflow_id))
    .sort((a, b) => Number(b.created_at) - Number(a.created_at));
}

/** 统一设备连接目标（优先读取有效 `laser-forge-*`，兼容旧 `laserLab.*` 回退与受限存储会话副本）。 */
export function savedConnectionForm(...args) {
  return args.length > 0 ? readUnifiedConnection(args[0]) : readUnifiedConnection();
}

export function connectionView(form) {
  const ready = drawTargetReady(form);
  return {
    ready,
    label: ready ? drawTargetLabel(form) : drawTargetMissingText(form),
    payload: ready ? drawConnectionPayload(form) : null,
  };
}

/** 预览窗底部说明：严格模式开 / 关 / 未知。 */
export function strictHintText(strict) {
  if (strict === true) return "严格模式已开启：只能在这里点「同意开刻」开始，对小智口头确认不会直接开刻。";
  if (strict === false) return "也可以直接对小智说「确认开刻」开始；不想刻就点「取消这个任务」。";
  return "在这里点「同意开刻」开始；不想刻就点「取消这个任务」。";
}

export function canStartAiPreview({ checked, busy, targetReady }) {
  return Boolean(checked && !busy && targetReady);
}

const TEXT_FILL_LABEL = { outline: "只描轮廓", fill: "扫描填充", hatch: "排线填充" };

/** 预览产物里的提示：刻法、雕刻路线（r45）、自动缩小到幅面内、照片抖动、超速与软限位风险提示。 */
export function aiPreviewNotes(record, { safetyWarning = "" } = {}) {
  const notes = [];
  const artifacts = record?.artifacts || {};
  if (TEXT_FILL_LABEL[artifacts.text_fill]) notes.push(`刻法：${TEXT_FILL_LABEL[artifacts.text_fill]}`);
  // 语音生成的文字任务按雕刻路线全局开关走（2026-11-17 技术书 r45）：转图片时说明一句，后端的改动说明原样带上
  if (artifacts.text_fill && artifacts.engrave_route === "image") notes.push("雕刻路线：转图片（文字渲染成位图逐行扫描）");
  if (typeof artifacts.route_note === "string" && artifacts.route_note) notes.push(artifacts.route_note);
  if (artifacts.auto_fitted && typeof artifacts.fit_note === "string") notes.push(artifacts.fit_note);
  if (artifacts.image_mode === "dither") notes.push("照片按灰度抖动雕刻");
  if (artifacts.image_mode === "grayscale") notes.push("灰度变功率雕刻：越黑功率越高");
  if (artifacts.negative === true) notes.push("已反相：原图浅色处出光");
  if (Array.isArray(artifacts.warnings)) {
    for (const warning of artifacts.warnings) {
      if (typeof warning === "string" && warning.trim() && !notes.includes(warning.trim())) {
        notes.push(warning.trim());
      }
    }
  }
  const cleanSafety = typeof safetyWarning === "string" ? safetyWarning.trim() : "";
  if (cleanSafety && !notes.includes(cleanSafety)) {
    notes.push(cleanSafety);
  }
  return notes;
}

/** 开刻前激光头要对准的位置（2026-11-06 用户决定：文字默认对准木牌中心）。 */
export function originNote(record) {
  const note = record?.artifacts?.origin_note;
  return typeof note === "string" && note.trim() ? note.trim() : "";
}

/**
 * “先走边框”的请求：以激光头当前位置为原点（与即将开刻的任务同一参照），沿预览返回的 `frame_mm`
 * 绕一圈后回到起点；默认不出光。没有范围或没有设备目标时返回 null（绝不猜）。
 */
// 2026-11-16 橡皮筋走边框（对齐 LightBurn Rubber Band Framing）：沿出光点的外轮廓（凸包）走，
// 而不是外接矩形；偏好存在本机，工作台、AI 预览窗与画板共用。默认外接矩形。
export const FRAMING_SHAPE_KEY = "moss.framingShape.v1";

export function readFramingShape(storage = globalThis.localStorage) {
  try {
    return storage?.getItem(FRAMING_SHAPE_KEY) === "hull" ? "hull" : "box";
  } catch {
    return "box";
  }
}

export function writeFramingShape(shape, storage = globalThis.localStorage) {
  try {
    storage?.setItem(FRAMING_SHAPE_KEY, shape === "hull" ? "hull" : "box");
  } catch {
    // 存储被禁用时只是不记住
  }
}

/** 预览结果里的外轮廓 `frame_hull_mm`：3 ~ 256 个数字点，否则 null（退回外接矩形）。 */
export function framingOutline(artifacts) {
  const hull = artifacts?.frame_hull_mm;
  if (!Array.isArray(hull) || hull.length < 3 || hull.length > 256) return null;
  const round = (value) => Math.round(Number(value) * 1000) / 1000;
  const points = hull.map((p) => (Array.isArray(p) && p.length === 2 ? [Number(p[0]), Number(p[1])] : null));
  if (points.some((p) => !p || !Number.isFinite(p[0]) || !Number.isFinite(p[1]))) return null;
  return points.map(([x, y]) => [round(x), round(y)]);
}

/** 确认框里的走法说明。 */
export function framingShapeText(request) {
  if (Array.isArray(request?.outline)) return `沿图形外轮廓（${request.outline.length} 个点）`;
  const w = Math.round((Number(request?.max_x) - Number(request?.min_x)) * 10) / 10;
  const h = Math.round((Number(request?.max_y) - Number(request?.min_y)) * 10) / 10;
  return `沿 ${w} × ${h} mm 的外接矩形`;
}

export function framingRequest(record, connectionPayload, { shape = "box" } = {}) {
  const frame = record?.artifacts?.frame_mm;
  const keys = ["min_x", "max_x", "min_y", "max_y"];
  if (!connectionPayload || !frame || !keys.every((key) => Number.isFinite(Number(frame[key])))) return null;
  if (Number(frame.max_x) <= Number(frame.min_x) || Number(frame.max_y) <= Number(frame.min_y)) return null;
  const round = (value) => Math.round(Number(value) * 1000) / 1000;
  return {
    ...connectionPayload,
    relative_to_head: true,
    min_x: round(frame.min_x),
    max_x: round(frame.max_x),
    min_y: round(frame.min_y),
    max_y: round(frame.max_y),
    feed_rate: 2000,
    power: 0,
    ...(shape === "hull" && framingOutline(record?.artifacts) ? { outline: framingOutline(record.artifacts) } : {}),
  };
}
