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

test("同一物理目标上新运行任务与旧未知记录并存时急停该目标；跨设备未知任务绝不静默跳过", async () => {
  // 跨设备（设备 A 处于 unknown 仍可能运动，设备 B 处于 running）：必须返回 ambiguous，绝不静默忽略 A
  let crossDeviceCalled = false;
  const crossDevice = await stopKnownJob(
    async () => ({
      ok: true,
      data: {
        success: true,
        result: {
          jobs: [
            { job_id: "device-a-unknown", target: "192.168.1.9:23", state: "unknown" },
            { job_id: "device-b-running", target: "COM3@115200", state: "running" },
          ],
        },
      },
    }),
    async () => {
      crossDeviceCalled = true;
    },
  );
  assert.equal(crossDevice.kind, "ambiguous");
  assert.equal(crossDeviceCalled, false);

  // 同一物理目标（COM3@115200）存在唯一 running 与残留同目标 unknown 时，向该目标发送急停
  const calls = [];
  const sameTarget = await stopKnownJob(
    async () => ({
      ok: true,
      data: {
        success: true,
        result: {
          jobs: [
            { job_id: "stale-unknown", target: "COM3@115200", state: "unknown" },
            { job_id: "active-running", target: "COM3@115200", state: "running" },
          ],
        },
      },
    }),
    async (path, data) => {
      calls.push({ path, data });
      return { ok: true, data: { success: true, result: "0x18 已写出" } };
    },
  );
  assert.equal(sameTarget.kind, "sent");
  assert.deepEqual(calls, [
    { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "active-running" } },
  ]);

  const twoUnknown = await stopKnownJob(
    async () => ({
      ok: true,
      data: {
        success: true,
        result: {
          jobs: [
            { job_id: "u1", target: "COM3@115200", state: "unknown" },
            { job_id: "u2", target: "COM3@115200", state: "unknown" },
          ],
        },
      },
    }),
    async () => {
      throw new Error("should not pick");
    },
  );
  assert.equal(twoUnknown.kind, "ambiguous");
});
