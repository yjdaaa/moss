/**
 * NV19 长期记忆私有页面的纯逻辑（技术书 §15、实施细则 §11）：名称、状态说明与表单转换。
 * 不依赖 React，便于单元测试。
 */

export const CATEGORY_LABEL = {
  preference: "偏好",
  device_fact: "设备事实",
  technical_note: "技术资料",
  task_context: "任务",
  troubleshooting_experience: "故障经验",
};

/** 每类记忆能放的范围（与 moss-voice-server::context 一致）。 */
export const CATEGORY_SCOPES = {
  preference: ["device"],
  device_fact: ["device"],
  technical_note: ["project"],
  task_context: ["project"],
  troubleshooting_experience: ["device"],
};

/** 需要写明机型与固件的类别。 */
export const NEEDS_DEVICE = new Set(["device_fact", "troubleshooting_experience"]);

export const SOURCE_LABEL = {
  user_edit: "用户填写",
  conversation_extraction: "对话提取",
  legacy_nv10_import: "旧摘要导入",
  document_index: "资料",
  trusted_test: "测试结果",
  agent_run_record: "任务记录",
};

const SKIP_REASON = {
  op_not_allowed: "模型试图修改或删除（不允许）",
  category_unknown: "类别无效",
  content_missing: "内容为空",
  too_many_operations: "超过每次 8 条",
  duplicate: "与已有记录重复",
  no_project_scope: "没有当前项目",
  applicability_unknown: "设备没有机型 / 固件记录",
  invalid_output: "模型输出格式不对",
  tool_calls_rejected: "模型试图调用工具（整次作废）",
  context_content_sensitive: "含敏感内容或机器指令",
  context_content_invalid: "内容过长或无效",
  context_source_not_allowed: "这类记忆不能自动提取",
  context_scope_stale: "提取时设备或项目已变化",
  run_stopped: "提取中途停止",
};

export function skipReasonText(code) {
  return SKIP_REASON[code] || code;
}

/** 一行一条，去空白与空行。 */
export function linesToList(text) {
  return String(text || "")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

export function listToLines(list) {
  return (Array.isArray(list) ? list : []).join("\n");
}

/** 记录状态胶囊。 */
export function rowBadge(row) {
  if (row?.state === "expired") return { text: "已过期", tone: "off" };
  if (row?.state === "invalidated" || row?.verification === "invalidated") return { text: "已作废", tone: "off" };
  if (row?.verification === "confirmed") return { text: "已确认", tone: "ok" };
  return { text: "候选（未确认）", tone: "warn" };
}

/** 范围的显示名：设备用名称或编号，项目用项目名。 */
export function scopeLabel(scope, status) {
  if (!scope) return "";
  if (scope.kind === "project") return `项目：${status?.project?.name || scope.id}`;
  const device = (status?.devices || []).find((d) => d.id === scope.id);
  return `设备：${device?.name || scope.id}`;
}

/** 状态里列出的全部可选范围（已允许的设备 + 当前项目）。 */
export function scopeOptions(status) {
  const devices = (status?.devices || []).map((d) => ({ kind: "device", id: d.id }));
  return status?.project ? [...devices, { kind: "project", id: status.project.id }] : devices;
}

/** 能记录故障经验的设备：机型和固件都已登记。 */
export function experienceDevices(status) {
  return (status?.devices || []).filter((d) => d.model && d.firmware);
}
