import test from "node:test";
import assert from "node:assert/strict";
import {
  normalizeWorkflowStatus,
  getSendOverlayStage,
  extractWorkflowInfo,
} from "../src/state/statusNormalize.js";

test("D10: normalizes queued to pending and canceled to cancelled", () => {
  assert.equal(normalizeWorkflowStatus("queued"), "pending");
  assert.equal(normalizeWorkflowStatus("QUEUED"), "pending");
  assert.equal(normalizeWorkflowStatus("canceled"), "cancelled");
  assert.equal(normalizeWorkflowStatus("CANCELED"), "cancelled");
  assert.equal(normalizeWorkflowStatus("running"), "running");
  assert.equal(normalizeWorkflowStatus("completed"), "completed");
  assert.equal(normalizeWorkflowStatus(""), "pending");
  assert.equal(normalizeWorkflowStatus(null), "pending");
});

test("D10: overlay stage mapping matches frontend-inventory spec", () => {
  const pending = getSendOverlayStage("pending");
  assert.equal(pending.step, 0);
  assert.equal(pending.label, "等待");
  assert.equal(pending.terminal, false);

  const queued = getSendOverlayStage("queued");
  assert.equal(queued.step, 0);
  assert.equal(queued.normalized, "pending");

  const preparing = getSendOverlayStage("preparing");
  assert.equal(preparing.step, 1);
  assert.equal(preparing.label, "准备");
  assert.equal(preparing.terminal, false);

  const running = getSendOverlayStage("running");
  assert.equal(running.step, 2);
  assert.equal(running.label, "发送中");
  assert.equal(running.terminal, false);

  const sending = getSendOverlayStage("sending");
  assert.equal(sending.step, 2);
  assert.equal(sending.label, "发送中");
  assert.equal(sending.terminal, false);

  const completed = getSendOverlayStage("completed");
  assert.equal(completed.step, 3);
  assert.equal(completed.label, "完成");
  assert.equal(completed.terminal, true);
  assert.equal(completed.isSuccess, true);

  const failed = getSendOverlayStage("failed");
  assert.equal(failed.step, 2);
  assert.equal(failed.label, "失败");
  assert.equal(failed.terminal, true);
  assert.equal(failed.isError, true);

  const cancelled = getSendOverlayStage("cancelled");
  assert.equal(cancelled.step, 2);
  assert.equal(cancelled.label, "已取消");
  assert.equal(cancelled.terminal, true);
  assert.equal(cancelled.isError, true);

  const singleLCanceled = getSendOverlayStage("canceled");
  assert.equal(singleLCanceled.normalized, "cancelled");
  assert.equal(singleLCanceled.terminal, true);
});

test("D10: extractWorkflowInfo extracts nested workflow and runtime status", () => {
  const payload1 = {
    workflow: {
      status: "queued",
      runtime_job: { job_id: "job_123" },
      speech: "等待排队",
    },
  };
  const info1 = extractWorkflowInfo(payload1);
  assert.equal(info1.status, "pending");
  assert.equal(info1.rawStatus, "queued");
  assert.equal(info1.jobId, "job_123");
  assert.equal(info1.speech, "等待排队");

  const payload2 = {
    status: "canceled",
  };
  const info2 = extractWorkflowInfo(payload2);
  assert.equal(info2.status, "cancelled");
});
