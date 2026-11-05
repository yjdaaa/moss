/**
 * 顶栏急停：直接查询后端持有的原设备任务通道，不依赖当前 React 页面。
 * 多任务时不擅自选择设备；请求成功只证明实时指令已写出，不证明物理停机。
 */
export async function stopKnownJob(getTargets, sendStop) {
  const targets = await getTargets("/api/jobs/stop-targets");
  const jobs = targets?.data?.result?.jobs;
  if (!targets?.ok || !targets?.data?.success || !Array.isArray(jobs)) {
    return { kind: "lookup_failed" };
  }
  if (jobs.length === 0) return { kind: "none" };
  let job = jobs[0];
  if (jobs.length > 1) {
    const running = jobs.filter((item) => item && item.state === "running");
    const othersAllUnknown = jobs.every(
      (item) => item && (item.state === "running" || item.state === "unknown"),
    );
    const primaryTarget =
      running.length === 1 && typeof running[0]?.target === "string"
        ? running[0].target.trim()
        : "";
    const sameSingleTarget =
      primaryTarget !== "" &&
      jobs.every(
        (item) => typeof item?.target === "string" && item.target.trim() === primaryTarget,
      );
    if (running.length === 1 && othersAllUnknown && sameSingleTarget) {
      job = running[0];
    } else {
      return { kind: "ambiguous", jobs };
    }
  }
  if (typeof job.job_id !== "string" || !job.job_id.trim()) {
    return { kind: "lookup_failed" };
  }
  const response = await sendStop("/api/safe-action", {
    action: "emergency_stop", confirmed: true, job_id: job.job_id,
  });
  return {
    kind: response?.ok && response?.data?.success ? "sent" : "failed",
    job,
    message: typeof response?.data?.result === "string" ? response.data.result : "",
  };
}

/**
 * 页面表单 → `/api/safe-action` 的显式设备目标；目标没填时返回 null（绝不猜设备）。
 * 表单字段与发送请求一致：`connection_mode`、`port`、`baudrate`、`network_host`、`network_telnet_port`。
 */
export function estopTargetArgs(form) {
  if (!form) return null;
  if (form.connection_mode === "serial") {
    const port = String(form.port || "").trim();
    return port ? { connection_mode: "serial", port, baudrate: Number(form.baudrate) || 115200 } : null;
  }
  const host = String(form.network_host || "").trim();
  return host ? { connection_mode: "network", host, telnet_port: Number(form.network_telnet_port) || 23 } : null;
}

/**
 * 页面急停（2026-11-05）：先按任务编号走原设备通道；后端已找不到该任务时（刚全量发送完，
 * GRBL 缓冲区仍可能在执行；或任务从未登记），改按本页设备目标新开连接写 0x18。
 * 绝不退化为 workflow cancel——无活动任务时 cancel 不向设备写任何字节。
 */
export async function stopPageTarget(sendStop, { jobId, target } = {}) {
  const attempts = [];
  if (typeof jobId === "string" && jobId.trim()) {
    attempts.push({ action: "emergency_stop", confirmed: true, job_id: jobId.trim() });
  }
  if (target) attempts.push({ action: "emergency_stop", confirmed: true, ...target });
  if (attempts.length === 0) return { kind: "no_target" };
  const messages = [];
  for (const body of attempts) {
    const response = await sendStop("/api/safe-action", body);
    const message = typeof response?.data?.result === "string" ? response.data.result : "";
    if (response?.ok && response?.data?.success) {
      return { kind: "sent", via: body.job_id ? "job" : "target", message };
    }
    if (message) messages.push(message);
  }
  return { kind: "failed", message: messages.join("；") };
}

/**
 * 判断页面显式设备目标是否与后端任务记录的 target 描述指向同一设备。
 */
export function isSameEstopTarget(target, jobTarget) {
  if (!target || typeof jobTarget !== "string" || !jobTarget.trim()) return false;
  const normalized = jobTarget.trim().toLowerCase();
  if (target.connection_mode === "serial") {
    const port = String(target.port || "").trim().toLowerCase();
    const baud = Number(target.baudrate) || 115200;
    if (!port) return false;
    return normalized === port || normalized.includes(`${port}@${baud}`) || normalized.endsWith(`/${port}`);
  }
  if (target.connection_mode === "network") {
    const host = String(target.host || "").trim().toLowerCase();
    const port = Number(target.telnet_port) || 23;
    if (!host) return false;
    return normalized === `${host}:${port}` || normalized.endsWith(`://${host}:${port}`);
  }
  return false;
}

/**
 * 判断后端刚刚通过 stopKnownJob 停掉的任务是否已经覆盖当前页面的设备目标。
 * 注意：若后端记录处于 `unknown`（待确认停止的旧记录，对应连接或模拟设备可能已关闭），
 * 绝不视为已覆盖当前页面设备，必须继续向当前页面的设备目标发送一次 0x18。
 */
export function isJobCoveringPageTarget(stoppedJob, { jobId, target } = {}) {
  if (!stoppedJob || stoppedJob.state !== "running") return false;
  const trimmedJobId = typeof jobId === "string" ? jobId.trim() : "";
  if (trimmedJobId && stoppedJob.job_id === trimmedJobId) return true;
  return isSameEstopTarget(target, stoppedJob.target);
}

/**
 * 顶栏全局急停编排：先停后端登记的任务；当后端无任务、失败、仅停了 `unknown` 旧记录
 * 或停的是其他设备时，同时触发当前页面向本页设备目标补发 0x18。
 */
export async function triggerTopbarEmergencyStop(getTargets, sendStop, notifyPage) {
  const outcome = await stopKnownJob(getTargets, sendStop);
  let pageHandled = false;
  if (typeof notifyPage === "function" && outcome.kind !== "ambiguous") {
    pageHandled = Boolean(
      notifyPage({
        stoppedJob: outcome.kind === "sent" ? outcome.job : null,
      }),
    );
  }
  return { ...outcome, pageHandled };
}
