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
  if (jobs.length > 1) return { kind: "ambiguous", jobs };
  const job = jobs[0];
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
