/**
 * 数控激光断点续雕状态判定器
 * 对齐后端 `workflow.rs::validate_resume_record` 与 `send.rs`：
 * - 只有状态为 `cancelled`、非 `sd_offline`、`can_resume === true`、`resume_boundary_source === "device_hold_ln"`
 *   且设备证实已完成行数 `resumable_from_line > 0`（小于总行数、不超过 `lines_sent`）时才可断点续雕；
 * - 续雕起始行为 `resumable_from_line + 1`（而非已发送入队的 `lines_sent + 1`）。
 */

export function sendProgressFromResult(sendResult) {
  if (!sendResult || typeof sendResult !== "object") return null;
  const total = Number(sendResult.total_lines ?? sendResult.total) || 0;
  const sentRaw = sendResult.lines_sent ?? sendResult.sent;
  const sent = Number.isFinite(Number(sentRaw)) ? Number(sentRaw) : 0;
  const resumableRaw = Number(sendResult.resumable_from_line);
  const resumable = Number.isFinite(resumableRaw) && resumableRaw > 0 ? Math.trunc(resumableRaw) : 0;
  const completedRaw = sendResult.lines_completed;
  const linesCompleted =
    completedRaw !== undefined && completedRaw !== null && Number.isFinite(Number(completedRaw))
      ? Math.trunc(Number(completedRaw))
      : undefined;
  const warnings = Array.isArray(sendResult.warnings)
    ? sendResult.warnings.map((w) => (typeof w === "string" ? w.trim() : "")).filter(Boolean)
    : [];
  return {
    sent,
    total,
    can_resume: sendResult.can_resume === true,
    resumable_from_line: resumable,
    resume_boundary_source: typeof sendResult.resume_boundary_source === "string" ? sendResult.resume_boundary_source : "",
    mode: typeof sendResult.mode === "string" ? sendResult.mode : "",
    ...(linesCompleted !== undefined ? { lines_completed: linesCompleted } : {}),
    target: typeof sendResult.target === "string" ? sendResult.target : "",
    ...(typeof sendResult.phase === "string" ? { phase: sendResult.phase } : {}),
    ...(warnings.length > 0 ? { warnings } : {}),
  };
}

export function canResumeJob(sendProgress, overlayStatus) {
  if (!sendProgress || typeof sendProgress !== "object") return false;
  if (overlayStatus !== "cancelled") return false;
  if (sendProgress.can_resume !== true) return false;
  if (sendProgress.mode === "sd_offline") return false;
  if (sendProgress.resume_boundary_source !== "device_hold_ln") return false;
  const resumable = Math.trunc(Number(sendProgress.resumable_from_line));
  if (!Number.isFinite(resumable) || resumable <= 0) return false;
  const total = Number(sendProgress.total_lines ?? sendProgress.total) || 0;
  if (total > 0 && resumable >= total) return false;
  if (
    sendProgress.lines_completed !== undefined
    && sendProgress.lines_completed !== null
    && Number(sendProgress.lines_completed) !== resumable
  ) {
    return false;
  }
  const ackedRaw = sendProgress.lines_sent ?? sendProgress.sent;
  if (ackedRaw !== undefined && ackedRaw !== null && Number.isFinite(Number(ackedRaw))) {
    if (resumable > Number(ackedRaw)) return false;
  }
  return true;
}

export function getResumeStartLine(sendProgress) {
  const resumable = Math.max(0, Math.trunc(Number(sendProgress?.resumable_from_line) || 0));
  return resumable + 1;
}

export function formatResumeActionLabel(sendProgress) {
  const startLine = getResumeStartLine(sendProgress);
  return `从第 ${startLine} 行继续加工 (断点续雕)`;
}

export function formatResumePrompt(sendProgress) {
  const resumable = Math.max(0, Math.trunc(Number(sendProgress?.resumable_from_line) || 0));
  const total = Math.max(0, Math.trunc(Number(sendProgress?.total_lines ?? sendProgress?.total) || 0));
  const startLine = resumable + 1;
  return `当前任务已在第 ${resumable}/${total} 行安全停机。可从第 ${startLine} 行断点继续下发剩余 ${Math.max(0, total - resumable)} 行代码。`;
}
