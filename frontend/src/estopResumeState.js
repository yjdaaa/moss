/**
 * 数控激光断点续雕状态判定器
 * 负责严格核验急停/停机后的断点位置，计算剩余行数与续雕起始行。
 */

export function canResumeJob(sendProgress, overlayStatus) {
  if (!sendProgress) return false;
  const sent = Number(sendProgress.sent) || 0;
  const total = Number(sendProgress.total) || 0;
  if (sent <= 0 || (total > 0 && sent >= total)) {
    return false;
  }
  return overlayStatus === "cancelled" || overlayStatus === "failed";
}

export function getResumeStartLine(sendProgress) {
  const sent = Number(sendProgress?.sent) || 0;
  return sent + 1;
}

export function formatResumeActionLabel(sendProgress) {
  const sent = Number(sendProgress?.sent) || 0;
  return `从第 ${sent + 1} 行继续加工 (断点续雕)`;
}

export function formatResumePrompt(sendProgress) {
  const sent = Number(sendProgress?.sent) || 0;
  const total = Number(sendProgress?.total) || 0;
  return `当前任务已在第 ${sent}/${total} 行安全停机。可从第 ${sent + 1} 行断点继续下发剩余 ${total - sent} 行代码。`;
}
