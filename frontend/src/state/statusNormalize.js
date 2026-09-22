/**
 * D10 状态词表规范与归一化映射器
 *
 * 涵盖三套系统状态词表：
 * 1. WORKFLOW_STATUSES: 工作流生命周期状态 (workflows 表与工作流接口)
 * 2. JOB_STATUSES: 底层硬件发送任务状态 (jobs 表与 GRBL worker)
 * 3. RUNTIME_STATUSES: 任务运行时状态 (旧 Python 兼容层对外暴露状态)
 */

export const WORKFLOW_STATUSES = [
  "collecting",
  "preview_ready",
  "sending",
  "completed",
  "failed",
  "cancelled",
  "feedback_recorded",
];

export const JOB_STATUSES = [
  "pending",
  "running",
  "completed",
  "failed",
  "cancelled",
];

export const RUNTIME_STATUS_ALIASES = {
  queued: "pending",
  canceled: "cancelled",
};

export const TERMINAL_STATUSES = new Set(["completed", "failed", "cancelled"]);

/**
 * 归一化工作流/任务状态 (D10 拍板方案)
 * - "queued" -> "pending"
 * - "canceled" -> "cancelled"
 */
export function normalizeWorkflowStatus(value) {
  const raw = String(value || "").trim().toLowerCase();
  if (RUNTIME_STATUS_ALIASES[raw]) {
    return RUNTIME_STATUS_ALIASES[raw];
  }
  return raw || "pending";
}

export const STAGE_MAP = {
  pending: { step: 0, label: "等待", title: "等待任务" },
  queued: { step: 0, label: "排队", title: "等待任务" },
  preparing: { step: 1, label: "准备", title: "准备发送" },
  running: { step: 2, label: "发送中", title: "正在向设备发送" },
  sending: { step: 2, label: "发送中", title: "正在向设备发送" },
  completed: { step: 3, label: "完成", title: "发送完成" },
  failed: { step: 2, label: "失败", title: "发送失败" },
  cancelled: { step: 2, label: "已取消", title: "任务已取消" },
};

/**
 * 计算发送覆盖层 (SendOverlay) 阶段信息
 */
export function getSendOverlayStage(status) {
  const normalized = normalizeWorkflowStatus(status);
  const info = STAGE_MAP[normalized] || STAGE_MAP.pending;
  const terminal = TERMINAL_STATUSES.has(normalized);

  return {
    normalized,
    step: info.step,
    label: info.label,
    title: info.title,
    terminal,
    isSuccess: normalized === "completed",
    isError: normalized === "failed" || normalized === "cancelled",
  };
}

/**
 * 从 API 结果对象中提取并归一化状态信息
 */
export function extractWorkflowInfo(result) {
  const source = (result && typeof result === "object") ? result : {};
  const workflow = (source.workflow && typeof source.workflow === "object") ? source.workflow : source;
  const runtime = (workflow.runtime_job && typeof workflow.runtime_job === "object")
    ? workflow.runtime_job
    : ((source.runtime_job && typeof source.runtime_job === "object") ? source.runtime_job : {});

  const rawStatus = workflow.status || source.status || runtime.status;
  const status = normalizeWorkflowStatus(rawStatus);
  const jobId = String(runtime.job_id || source.job_id || "");
  const speech = String(workflow.speech || source.speech || source.result || "");

  return {
    workflow,
    runtime,
    status,
    rawStatus: String(rawStatus || ""),
    jobId,
    speech,
  };
}
