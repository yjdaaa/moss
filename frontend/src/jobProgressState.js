/**
 * 激光任务进度浮层的纯逻辑（2026-11-02 用户决定）。
 *
 * 数据只来自后端只读接口 `GET /api/jobs/stop-targets`（已带 workflow_id / phase / lines_sent /
 * total_lines / elapsed_ms / mode / target）。这里只做展示换算与载荷构造，不包含任何设备写入；
 * 停止动作一律走既有 `/api/workflow {action:"cancel"}` 与 `/api/safe-action {action:"emergency_stop"}`。
 */

/** 浮层轮询间隔（毫秒）。 */
export const JOB_POLL_MS = 1000;

/** 任务从运行列表消失后，浮层保留终态提示的时长。 */
export const JOB_LINGER_MS = 5000;

/** `mm:ss`；负数与非数字都按 0 处理。 */
export function formatDuration(milliseconds) {
  const ms = Number(milliseconds);
  if (!Number.isFinite(ms) || ms <= 0) return "00:00";
  const total = Math.floor(ms / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

/** 已发百分比；总行数未知时返回 null（界面显示“进度未知”，不编造 0%）。 */
export function jobPercent(job) {
  const total = Number(job?.total_lines) || 0;
  const sent = Number(job?.lines_sent) || 0;
  if (total <= 0) return null;
  return Math.max(0, Math.min(100, Math.floor((sent / total) * 100)));
}

/** 剩余时间估算：按已发比例线性外推；样本不足时返回空字符串，界面不显示。 */
export function jobRemainingText(job) {
  const total = Number(job?.total_lines) || 0;
  const sent = Number(job?.lines_sent) || 0;
  const elapsed = Number(job?.elapsed_ms) || 0;
  if (sent <= 0 || total <= sent || elapsed <= 0) return "";
  const remaining = (elapsed * (total - sent)) / sent;
  return `剩余约 ${formatDuration(remaining)}`;
}

/** 阶段文案：后端 phase 只有 streaming / awaiting_completion，未知态明确写“进度未知”。 */
export function jobPhaseText(job) {
  if (!job || job.state === "unknown") return "进度未知（连接异常，可试急停并在机器旁确认）";
  if (job.phase === "awaiting_completion") return "代码已发完，等待设备完成";
  if (job.phase === "streaming") return "正在发送 G-code";
  return "运行中";
}

/** 单条任务的展示视图。停不掉的字段一律为 null，界面据此隐藏按钮。 */
export function jobView(job) {
  const jobId = typeof job?.job_id === "string" && job.job_id.trim() ? job.job_id.trim() : "";
  const workflowId = typeof job?.workflow_id === "string" && job.workflow_id.trim() ? job.workflow_id.trim() : "";
  const percent = jobPercent(job);
  const sent = Number(job?.lines_sent) || 0;
  const total = Number(job?.total_lines) || 0;
  const unknown = job?.state === "unknown" || percent === null;
  return {
    jobId,
    workflowId,
    unknown,
    percent,
    percentText: percent === null ? "进度未知" : `${percent}%`,
    linesText: total > 0 ? `已发 ${sent} / ${total} 行` : "已发行数未知",
    elapsedText: `用时 ${formatDuration(job?.elapsed_ms)}`,
    remainingText: jobRemainingText(job),
    phaseText: jobPhaseText(job),
    target: typeof job?.target === "string" ? job.target : "",
    mode: job?.mode === "serial" ? "串口" : job?.mode === "network" ? "网络" : "",
    canCancel: Boolean(workflowId) && job?.state !== "unknown",
    canEstop: Boolean(jobId),
    canAcknowledgeStop: Boolean(jobId) && job?.state === "unknown",
    // 「取消」只在知道 workflow_id 且非未知态时可用；未知态保留软急停重试入口，但须在机器旁确认。
    cancelHint: !workflowId
      ? "这条任务缺少 workflow_id，无法温和取消，只能用急停"
      : job?.state === "unknown"
        ? "连接状态未知，无法温和取消，请用急停并在机器旁确认"
        : "",
  };
}

/** 接口载荷 → 视图数组；非法输入按空列表处理，避免一个坏字段整块消失。 */
export function jobsView(payload) {
  const jobs = Array.isArray(payload?.jobs) ? payload.jobs : Array.isArray(payload) ? payload : [];
  return jobs
    .filter((job) => job && (job.job_id || job.workflow_id))
    .map(jobView)
    .sort((a, b) => (a.jobId || a.workflowId).localeCompare(b.jobId || b.workflowId));
}

/** 胶囊文案：单任务显示百分比，多任务显示数量。 */
export function dockPillText(views) {
  const list = Array.isArray(views) ? views : [];
  if (list.length === 0) return "";
  if (list.length > 1) return `${list.length} 个任务`;
  return list[0].unknown ? "进行中" : list[0].percentText;
}

/** 温和取消：走既有 `/api/workflow` cancel，记录断点，可续雕。 */
export function cancelPayload(view) {
  return { action: "cancel", workflow_id: view?.workflowId || "" };
}

/** 急停：走既有 `/api/safe-action` 的实时 0x18 通道，必须显式 confirmed。 */
export function estopPayload(view) {
  return { action: "emergency_stop", confirmed: true, job_id: view?.jobId || "" };
}

/** 未知态人工现场核对停稳后移除卡片：走 `/api/jobs/acknowledge-stop`，必须显式 confirmed。 */
export function acknowledgeStopPayload(view) {
  return { job_id: view?.jobId || "", confirmed: true };
}

/** 从 workflow 状态载荷里找 G-code 下载地址；找不到返回空字符串（界面隐藏按钮）。 */
export function jobGcodeUrl(status) {
  const map = status && typeof status === "object" ? status : {};
  const direct = map.gcode_download_url || map?.web?.gcode_download_url;
  if (typeof direct === "string" && direct.trim()) return direct.trim();
  const name = map?.artifacts?.gcode_file || map.gcode_file;
  if (typeof name === "string" && name.trim()) return `/preview?path=${encodeURIComponent(name.trim())}&download=1`;
  return "";
}
