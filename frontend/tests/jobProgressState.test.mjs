import test from "node:test";
import assert from "node:assert/strict";
import {
  JOB_LINGER_MS,
  JOB_POLL_MS,
  cancelPayload,
  dockPillText,
  estopPayload,
  formatDuration,
  jobGcodeUrl,
  jobPercent,
  jobPhaseText,
  jobRemainingText,
  jobView,
  jobsView,
} from "../src/jobProgressState.js";

const liveJob = {
  job_id: "job-9",
  workflow_id: "wf-9",
  state: "running",
  phase: "streaming",
  mode: "network",
  target: "127.0.0.1:2301",
  lines_sent: 37,
  total_lines: 100,
  elapsed_ms: 62_000,
};

test("激光任务浮层：进度、剩余时间与阶段只按后端字段换算（2026-11-02）", () => {
  assert.equal(JOB_POLL_MS, 1000);
  assert.equal(JOB_LINGER_MS, 5000);

  assert.equal(formatDuration(0), "00:00");
  assert.equal(formatDuration(-5), "00:00");
  assert.equal(formatDuration(62_000), "01:02");
  assert.equal(formatDuration(Number.NaN), "00:00");

  assert.equal(jobPercent(liveJob), 37);
  assert.equal(jobPercent({ lines_sent: 5, total_lines: 0 }), null, "总行数未知时不给 0%，交给界面写“进度未知”");
  assert.equal(jobPercent({ lines_sent: 120, total_lines: 100 }), 100, "百分比夹到 100");

  assert.equal(jobRemainingText(liveJob), "剩余约 01:45");
  assert.equal(jobRemainingText({ lines_sent: 0, total_lines: 100, elapsed_ms: 1000 }), "", "没有进度样本就不猜剩余时间");
  assert.equal(jobRemainingText({ lines_sent: 100, total_lines: 100, elapsed_ms: 1000 }), "", "已发完不显示剩余");

  assert.match(jobPhaseText(liveJob), /正在发送/);
  assert.match(jobPhaseText({ phase: "awaiting_completion" }), /等待设备完成/);
  assert.match(jobPhaseText({ state: "unknown" }), /进度未知/, "连接异常时不冒充运行中");
});

test("激光任务浮层：视图只暴露可执行的动作，缺 workflow_id 时说明无法温和取消", () => {
  const view = jobView(liveJob);
  assert.equal(view.jobId, "job-9");
  assert.equal(view.workflowId, "wf-9");
  assert.equal(view.percentText, "37%");
  assert.equal(view.linesText, "已发 37 / 100 行");
  assert.equal(view.mode, "网络");
  assert.equal(view.target, "127.0.0.1:2301");
  assert.equal(view.canCancel, true);
  assert.equal(view.canEstop, true);
  assert.equal(view.cancelHint, "");

  const unknown = jobView({ job_id: "job-1", state: "unknown", target: "127.0.0.1:2301" });
  assert.equal(unknown.unknown, true);
  assert.equal(unknown.percentText, "进度未知");
  assert.equal(unknown.canCancel, false, "未知态不给温和取消");
  assert.equal(unknown.canEstop, true, "未知态仍保留 0x18 急停重试入口，与后端 stop-targets 契约一致");
  assert.match(unknown.cancelHint, /workflow_id/);

  const unknownWithWf = jobView({ job_id: "job-2", workflow_id: "wf-2", state: "unknown", target: "COM3" });
  assert.equal(unknownWithWf.canCancel, false, "未知态带 workflow_id 也不允许温和取消");
  assert.equal(unknownWithWf.canEstop, true, "未知态有 job_id 即可尝试急停");
  assert.match(unknownWithWf.cancelHint, /连接状态未知/);

  const missingJobId = jobView({ workflow_id: "wf-only", state: "running" });
  assert.equal(missingJobId.canEstop, false, "没有 job_id 时不能发软件急停");
});

test("激光任务浮层：载荷与 G-code 地址复用既有接口，不新造通道", () => {
  const view = jobView(liveJob);
  assert.deepEqual(cancelPayload(view), { action: "cancel", workflow_id: "wf-9" });
  assert.deepEqual(estopPayload(view), { action: "emergency_stop", confirmed: true, job_id: "job-9" });
  assert.equal(estopPayload(view).confirmed, true, "急停必须显式 confirmed");

  assert.equal(jobGcodeUrl({ gcode_download_url: "/preview?path=a&download=1" }), "/preview?path=a&download=1");
  assert.equal(jobGcodeUrl({ web: { gcode_download_url: "/preview?path=b&download=1" } }), "/preview?path=b&download=1");
  assert.equal(jobGcodeUrl({ artifacts: { gcode_file: "job 1.gcode" } }), "/preview?path=job%201.gcode&download=1");
  assert.equal(jobGcodeUrl({ gcode_file: "x.gcode" }), "/preview?path=x.gcode&download=1");
  assert.equal(jobGcodeUrl({}), "", "没有文件就返回空，界面隐藏按钮");
  assert.equal(jobGcodeUrl(null), "");
});

test("激光任务浮层：接口载荷转视图，坏数据不炸也不编造", () => {
  assert.deepEqual(jobsView({ jobs: [] }), []);
  assert.deepEqual(jobsView(null), []);
  assert.deepEqual(jobsView({ jobs: "oops" }), []);

  const views = jobsView({ jobs: [liveJob, { job_id: "job-1", state: "unknown" }, { nothing: true }] });
  assert.equal(views.length, 2, "没有 job_id / workflow_id 的条目被忽略");
  assert.deepEqual(views.map((item) => item.jobId), ["job-1", "job-9"], "按任务标识稳定排序");

  assert.equal(dockPillText(views), "2 个任务");
  assert.equal(dockPillText(jobsView({ jobs: [liveJob] })), "37%");
  assert.equal(dockPillText(jobsView({ jobs: [{ job_id: "j", state: "unknown" }] })), "进行中");
  assert.equal(dockPillText([]), "");
});
