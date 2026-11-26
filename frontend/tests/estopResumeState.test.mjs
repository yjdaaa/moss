import test from "node:test";
import assert from "node:assert/strict";
import {
  canResumeJob,
  getResumeStartLine,
  formatResumeActionLabel,
  formatResumePrompt,
  sendProgressFromResult,
} from "../src/estopResumeState.js";

test("急停断点续雕: 仅在 cancelled 且设备确认 Hold Ln 边界时允许续雕，起始行为 resumable_from_line + 1", () => {
  const progress = sendProgressFromResult({
    can_resume: true,
    resumable_from_line: 420,
    lines_completed: 420,
    lines_sent: 450,
    total_lines: 1200,
    resume_boundary_source: "device_hold_ln",
    mode: "stream",
  });
  assert.equal(canResumeJob(progress, "cancelled"), true, "急停取消且有精确完成边界时支持续雕");

  // 起始行必须取设备确认完成的 resumable_from_line + 1（421），而不是已发往串口缓冲区的 lines_sent + 1（451）
  assert.equal(getResumeStartLine(progress), 421, "续雕起始行必须为 resumable_from_line + 1");
  assert.equal(
    formatResumeActionLabel(progress),
    "从第 421 行继续加工 (断点续雕)",
  );
  assert.ok(
    formatResumePrompt(progress).includes("从第 421 行断点继续下发剩余 780 行代码"),
  );
});

test("急停断点续雕门禁对齐后端: failed、can_resume=false、sd_offline、非 device_hold_ln 或边界越界一律禁止续雕", () => {
  const validBase = {
    can_resume: true,
    resumable_from_line: 300,
    lines_completed: 300,
    sent: 320,
    total: 1000,
    resume_boundary_source: "device_hold_ln",
    mode: "stream",
  };

  assert.equal(canResumeJob(validBase, "failed"), false, "failed 状态后端拒绝续雕，前端不显示续雕按钮");
  assert.equal(canResumeJob(validBase, "running"), false, "运行中状态不可发起断点续雕");
  assert.equal(canResumeJob({ ...validBase, can_resume: false }, "cancelled"), false);
  assert.equal(canResumeJob({ ...validBase, mode: "sd_offline" }, "cancelled"), false);
  assert.equal(canResumeJob({ ...validBase, resume_boundary_source: "unproven" }, "cancelled"), false);
  assert.equal(canResumeJob({ ...validBase, resume_boundary_source: "" }, "cancelled"), false);
  assert.equal(canResumeJob({ ...validBase, resumable_from_line: 0, lines_completed: 0 }, "cancelled"), false);
  assert.equal(
    canResumeJob({ ...validBase, resumable_from_line: 1000, lines_completed: 1000, sent: 1000 }, "cancelled"),
    false,
  );
  assert.equal(
    canResumeJob({ ...validBase, resumable_from_line: 330, lines_completed: 330, sent: 320 }, "cancelled"),
    false,
  );
  assert.equal(
    canResumeJob({ ...validBase, lines_completed: 299 }, "cancelled"),
    false,
  );
  assert.equal(canResumeJob(null, "cancelled"), false);
  assert.equal(canResumeJob(undefined, "cancelled"), false);
});
