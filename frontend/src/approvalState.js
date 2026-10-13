/**
 * 全局审批卡的纯逻辑（技术书 §9.5.2，实施清单 P1-09）：不依赖 React，便于单元测试。
 * 审批数据来自 `GET /v1/approvals?status=pending`；只有桌面会话能读取与批准。
 */

export const APPROVAL_POLL_MS = 4000;
export const SITE_CHECK_TEXT = "我在机器旁，材料已放好、原点已确认，排风已开启";
export function approvalCheckText(kind) { return kind === "external_tool" ? "我已核对第三方工具、参数及数据访问范围，同意本次执行" : SITE_CHECK_TEXT; }

const ERROR_TEXT = {
  approval_request_conflict: "同一请求编号的内容或目标发生变化，请重新发起审批。旧审批没有用于本次操作。",
  voice_session_revoked: "来源设备已撤销授权或断开，请重新连接并发起请求。",
  approval_mismatch:
    "无法执行：请求创建后，加工内容、参数或目标设备已经变化（摘要不一致）。没有向设备发送任何指令，请让小智重新发起。",
  approval_expired: "授权已过期，没有向设备发送任何指令。请让小智重新发起。",
  approval_consumed: "该请求已经执行过或已失败，不能重复执行。",
  approval_rejected: "该请求已被拒绝。",
  approval_not_found: "找不到该审批请求，可能已被处理或过期。",
  approval_unavailable: "审批服务不可用（本地数据库或审计记录无法写入），没有向设备发送任何指令。",
  desktop_session_required: "只能在 MOSS 桌面端批准。",
  desktop_session_invalid: "桌面会话已失效，请重新打开 MOSS 桌面端。",
};

/** 批准失败时在卡内显示的原因：已知错误码用固定说明，其他用后端返回的文字。 */
export function approvalErrorText(code, fallback = "", kind = "") {
  if (kind === "external_tool") {
    const messages = { approval_mismatch: "第三方配置、凭据、权限或参数已经变化，未执行；请重新发起。", approval_expired: "本次授权已过期，未执行；请重新发起。", approval_unavailable: "审批或审计不可用，未调用第三方工具。", result_unknown: "第三方执行结果未知，不会自动重试，请先核实结果。" };
    if (messages[code]) return messages[code];
  }
  if (code && ERROR_TEXT[code]) return ERROR_TEXT[code];
  return fallback || "批准未完成。";
}

export const KIND_LABEL = {
  external_tool: "调用第三方工具",
  workflow_confirm_send: "开始加工",
  calibration_send: "发送标定矩阵",
  safe_action: "设备动作",
  framing: "走边框",
};

export const SOURCE_LABEL = {
  mcp_bridge: "小智语音（MCP 接入点）",
  lan: "局域网配对设备",
  gateway: "小智网关",
  voice: "小智语音（MOSS 内置）",
};

export function kindLabel(kind) {
  return KIND_LABEL[kind] || "设备动作";
}

export function sourceLabel(source, sessionId = "") {
  const base = SOURCE_LABEL[source] || "未知来源";
  if (!sessionId) return base;
  const tail = String(sessionId).slice(-4);
  // 内置语音记录的是发起的设备（MAC），其他来源为会话
  const noun = source === "voice" ? "设备" : "会话";
  return `${base} · ${noun} …${tail}`;
}

/** 目标设备只显示连接方式与名称，不显示 IP（后端也不返回）。 */
export function targetText(target) {
  if (!target || typeof target !== "object") return "未知设备";
  const mode = target.mode === "serial" ? "串口" : target.mode === "network" ? "网络" : "";
  const label = target.label || "";
  return mode ? `${mode} · ${label}` : label || "未知设备";
}

/** 摘要只显示前后各 4 位，便于与小智播报或日志核对。 */
export function shortDigest(digest) {
  const text = String(digest || "");
  return text.length > 12 ? `${text.slice(0, 4)}…${text.slice(-4)}` : text;
}

/** 服务端时间与本机时间之差（秒）。倒计时按服务端时钟计算，避免两边时钟不一致。 */
export function clockOffsetSeconds(serverTime, nowMs = Date.now()) {
  return typeof serverTime === "number" && Number.isFinite(serverTime) ? serverTime - nowMs / 1000 : 0;
}

export function secondsLeft(expiresAt, offsetSeconds = 0, nowMs = Date.now()) {
  if (typeof expiresAt !== "number" || !Number.isFinite(expiresAt)) return 0;
  return Math.max(0, Math.ceil(expiresAt - (nowMs / 1000 + offsetSeconds)));
}

/** “批准并开始”可点击的条件：已勾选现场确认、未在请求中、未失败、未过期。 */
export function canApprove({ checked, busy, failed, remaining }) {
  return Boolean(checked) && !busy && !failed && remaining > 0;
}

/** 待审批按创建时间排序（先来的先处理）。 */
export function orderPending(approvals) {
  return (Array.isArray(approvals) ? approvals : [])
    .filter((item) => item && item.status === "pending" && item.approval_id)
    .slice()
    .sort((a, b) => (a.created_at || 0) - (b.created_at || 0));
}

/** 本次轮询新出现的审批 ID（用于自动弹出）。 */
export function newPendingIds(seenIds, approvals) {
  const seen = seenIds instanceof Set ? seenIds : new Set(seenIds || []);
  return orderPending(approvals)
    .map((item) => item.approval_id)
    .filter((id) => !seen.has(id));
}

function numberOr(value, fallback = null) {
  const number = typeof value === "string" ? Number(value) : value;
  return typeof number === "number" && Number.isFinite(number) ? number : fallback;
}

/**
 * 从工作流记录整理审批卡上的加工信息。缺失的字段返回 null，由界面显示“—”，不编造。
 */
export function workflowDetails(record) {
  const source = record && typeof record === "object" ? record : {};
  const input = source.input || {};
  const artifacts = source.artifacts || {};
  const snapshot = source.confirmation_snapshot || {};
  const params = artifacts.actual_params || {};
  const material = input.material || snapshot.material || null;
  const thickness = numberOr(input.thickness_mm, numberOr(snapshot.thickness_mm));
  const power = numberOr(params.laser_max_power, numberOr(input.laser_max_power, numberOr(input.power)));
  const scale = numberOr(params.power_scale, numberOr(input.power_scale, 1000));
  const feed = numberOr(params.feed_rate, numberOr(input.feed_rate));
  const passes = numberOr(params.passes, numberOr(input.passes));
  const width = numberOr(artifacts.width_mm);
  const height = numberOr(artifacts.height_mm);
  const seconds = numberOr(artifacts.time_estimate?.estimated_seconds);
  const powerPercent = power !== null && scale ? Math.max(0, Math.min(100, (power / scale) * 100)) : null;
  let content = null;
  if (typeof input.text === "string" && input.text.trim()) {
    content = `文字“${input.text.trim().slice(0, 40)}”`;
  } else if (source.source_type === "image" || source.source_type === "draw") {
    content = source.source_type === "draw" ? "画板图形" : "上传图片";
  }
  return {
    content,
    material: material ? (thickness !== null ? `${material} · ${thickness} mm` : String(material)) : null,
    power: power !== null ? `${power}/${scale}（${Math.round(powerPercent)}%）` : null,
    powerPercent,
    feed: feed !== null ? `${feed} mm/min` : null,
    passes: passes !== null ? `${passes} 遍` : null,
    size: width !== null && height !== null ? `${Math.round(width * 10) / 10} × ${Math.round(height * 10) / 10} mm` : null,
    duration: seconds !== null ? (seconds >= 60 ? `约 ${Math.round(seconds / 60)} 分钟` : `约 ${Math.round(seconds)} 秒`) : null,
    previewPath: artifacts.preview_file || artifacts.image_file || null,
  };
}
