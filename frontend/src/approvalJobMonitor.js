import { extractWorkflowInfo, normalizeWorkflowStatus } from "./state/statusNormalize.js";

const VALID = new Set(["pending", "preparing", "running", "sending", "completed", "failed", "cancelled"]);
export function applyJobPoll(current, id, kind, response) {
  if (!current || current.id !== id || current.kind !== kind) return current;
  const record = response?.data?.result;
  const raw = kind === "calibration" ? record?.status : extractWorkflowInfo(record || {}).rawStatus;
  const status = normalizeWorkflowStatus(raw);
  if (!(response?.ok && response?.data?.success && raw && VALID.has(status))) {
    return { ...current, connectionLost: true, pollFailures: (current.pollFailures || 0) + 1 };
  }
  const send = record?.send_result || {};
  const valid = Number.isFinite(send.total_lines) && send.total_lines > 0 && Number.isFinite(send.lines_sent) && send.lines_sent >= 0 && send.lines_sent <= send.total_lines;
  return { ...current, status, connectionLost: false, pollFailures: 0,
    progress: valid ? { sent: send.lines_sent, total: send.total_lines, target: "" } : current.progress };
}
export function markStopRequested(current, id) {
  return current?.id === id ? { ...current, stopRequested: true } : current;
}
export function jobSafetyNotice(job) {
  if (job?.connectionLost) return "状态未知：暂时无法查询设备，机器可能仍在运行。正在重试；软件急停未送达时请使用实体急停。";
  if (job?.stopRequested) return "停止请求已发送，但尚未确认设备已停止。急停按钮仍然可用，请到设备旁确认。";
  return "";
}

/** One request at a time; bounded timeout and cleanup fence against late replies. */
export function startJobPolling(request, onResult, { intervalMs = 1000, timeoutMs = 5000 } = {}) {
  let stopped = false, timer, controller;
  const tick = async () => {
    controller = new AbortController();
    let deadline;
    const timeout = new Promise((resolve) => {
      deadline = setTimeout(() => { controller.abort(); resolve({ ok: false }); }, timeoutMs);
    });
    const response = await Promise.race([
      Promise.resolve().then(() => request(controller.signal)).catch(() => ({ ok: false })), timeout,
    ]);
    clearTimeout(deadline);
    if (stopped) return;
    onResult(response);
    if (!stopped) timer = setTimeout(tick, intervalMs);
  };
  timer = setTimeout(tick, 0);
  return () => { stopped = true; clearTimeout(timer); controller?.abort(); };
}
