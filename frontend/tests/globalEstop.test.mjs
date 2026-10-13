import test from "node:test";
import assert from "node:assert/strict";
import { stopKnownJob } from "../src/globalEstop.js";

test("顶栏独立于页面，使用后端唯一任务的原 job_id 发实时急停", async () => {
  const calls = [];
  const result = await stopKnownJob(
    async (path) => {
      assert.equal(path, "/api/jobs/stop-targets");
      return { ok: true, data: { success: true, result: { jobs: [{ job_id: "wf-offpage", target: "fake", state: "running" }] } } };
    },
    async (path, data) => { calls.push({ path, data }); return { ok: true, data: { success: true, result: "0x18 已写出" } }; },
  );
  assert.equal(result.kind, "sent");
  assert.deepEqual(calls, [{ path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "wf-offpage" } }]);
});

test("结果未知的任务仍能用原任务编号重试急停", async () => {
  const result = await stopKnownJob(
    async () => ({ ok: true, data: { success: true, result: { jobs: [{ job_id: "failed-job", state: "unknown" }] } } }),
    async (_, data) => ({ ok: data.job_id === "failed-job", data: { success: true } }),
  );
  assert.equal(result.kind, "sent");
});

test("多个设备任务不随机选择、不发送急停", async () => {
  let called = false;
  const result = await stopKnownJob(
    async () => ({ ok: true, data: { success: true, result: { jobs: [{ job_id: "1" }, { job_id: "2" }] } } }),
    async () => { called = true; },
  );
  assert.equal(result.kind, "ambiguous");
  assert.equal(called, false);
});

test("查询失败、无任务和停止失败分别报告，不谎称停机", async () => {
  assert.equal((await stopKnownJob(async () => ({ ok: false }), async () => { throw Error("no"); })).kind, "lookup_failed");
  assert.equal((await stopKnownJob(async () => ({ ok: true, data: { success: true, result: { jobs: [] } } }), async () => { throw Error("no"); })).kind, "none");
  assert.equal((await stopKnownJob(async () => ({ ok: true, data: { success: true, result: { jobs: [{ job_id: "1" }] } } }), async () => ({ ok: false, data: { result: "write failed" } }))).kind, "failed");
});
