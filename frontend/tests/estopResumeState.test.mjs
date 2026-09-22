import test from "node:test";
import assert from "node:assert/strict";
import {
  canResumeJob,
  getResumeStartLine,
  formatResumeActionLabel,
  formatResumePrompt,
} from "../src/estopResumeState.js";

test("急停断点续雕: 在 cancelled 或 failed 且存在部分已发送进度时允许断点续雕", () => {
  const progress = { sent: 450, total: 1200 };
  assert.equal(canResumeJob(progress, "cancelled"), true, "急停取消状态下必须支持续雕");
  assert.equal(canResumeJob(progress, "failed"), true, "异常故障中断状态下必须支持续雕");

  // 起始行计算必须为 sent + 1
  assert.equal(getResumeStartLine(progress), 451, "续雕起始行必须为断点行加 1");
  assert.equal(
    formatResumeActionLabel(progress),
    "从第 451 行继续加工 (断点续雕)",
  );
  assert.ok(
    formatResumePrompt(progress).includes("从第 451 行断点继续下发剩余 750 行代码"),
  );
});

test("急停断点续雕安全反向边界: 尚未下发或全量完成禁止发起断点续雕", () => {
  // 尚未发送任何行 (sent = 0)
  assert.equal(
    canResumeJob({ sent: 0, total: 1000 }, "cancelled"),
    false,
    "尚未开始下发的任务不可发起续雕",
  );

  // 全部行已发送完成 (sent = total)，严禁断点续雕（防二次烧黑工件）
  assert.equal(
    canResumeJob({ sent: 1000, total: 1000 }, "cancelled"),
    false,
    "已全量完成的任务禁止续雕",
  );

  // 任务正在运行中，禁止触发续雕
  assert.equal(
    canResumeJob({ sent: 300, total: 1000 }, "running"),
    false,
    "运行中状态不可发起断点续雕",
  );

  // 空值安全防御
  assert.equal(canResumeJob(null, "cancelled"), false);
  assert.equal(canResumeJob(undefined, "cancelled"), false);
  assert.equal(canResumeJob({ sent: -1, total: 100 }, "cancelled"), false);
});
