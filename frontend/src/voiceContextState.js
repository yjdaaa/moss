/**
 * NV19 / n50 无感记忆面板的纯逻辑（技术书 §15）：来源标签与范围显示。
 * 不依赖 React，便于单元测试。
 */

export const SOURCE_LABEL = {
  user_edit: "用户填写",
  conversation_extraction: "对话提取",
  legacy_nv10_import: "旧摘要导入",
  document_index: "资料",
  trusted_test: "测试结果",
  agent_run_record: "任务记录",
};

/** 范围的显示名：设备用名称或编号，项目用项目名。 */
export function scopeLabel(scope, status) {
  if (!scope) return "";
  if (scope.kind === "project") return `项目：${status?.project?.name || scope.id}`;
  if (scope.kind === "person") {
    const person = (status?.persons || []).find((p) => p.id === scope.id);
    return `个人：${person?.name || scope.id}`;
  }
  const device = (status?.devices || []).find((d) => d.id === scope.id);
  return `设备：${device?.name || scope.id}`;
}

/** 状态里列出的全部可选范围（已允许的设备 + 已标定说话人 + 当前项目）。 */
export function scopeOptions(status) {
  const devices = (status?.devices || []).map((d) => ({ kind: "device", id: d.id }));
  const persons = (status?.persons || []).map((p) => ({ kind: "person", id: p.id }));
  return status?.project ? [...devices, ...persons, { kind: "project", id: status.project.id }] : [...devices, ...persons];
}
