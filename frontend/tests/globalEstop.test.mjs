import test from "node:test";
import assert from "node:assert/strict";
import {
  estopTargetArgs,
  isJobCoveringPageTarget,
  isSameEstopTarget,
  stopKnownJob,
  stopPageTarget,
  triggerTopbarEmergencyStop,
} from "../src/globalEstop.js";

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

test("页面急停：任务仍在登记时只走原任务通道", async () => {
  const calls = [];
  const result = await stopPageTarget(
    async (path, data) => { calls.push({ path, data }); return { ok: true, data: { success: true, result: "已发送" } }; },
    { jobId: "wf-1", target: { connection_mode: "network", host: "127.0.0.1", telnet_port: 23 } },
  );
  assert.equal(result.kind, "sent");
  assert.equal(result.via, "job");
  assert.deepEqual(calls, [{ path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "wf-1" } }]);
});

test("页面急停：任务已发送完（后端找不到任务）时仍按设备目标写 0x18，绝不改走 cancel", async () => {
  const calls = [];
  const result = await stopPageTarget(
    async (path, data) => {
      calls.push({ path, data });
      return data.job_id
        ? { ok: true, data: { success: false, result: "找不到此任务的设备连接" } }
        : { ok: true, data: { success: true, result: "急停 0x18 已发送到 127.0.0.1:23" } };
    },
    { jobId: "wf-done", target: estopTargetArgs({ connection_mode: "network", network_host: " 127.0.0.1 ", network_telnet_port: "23" }) },
  );
  assert.equal(result.kind, "sent");
  assert.equal(result.via, "target");
  assert.ok(calls.every((call) => call.path === "/api/safe-action"), "急停只能走 safe-action");
  assert.deepEqual(calls[1].data, { action: "emergency_stop", confirmed: true, connection_mode: "network", host: "127.0.0.1", telnet_port: 23 });
});

test("页面急停：没有任务也没有设备目标时不发送、如实返回", async () => {
  let called = false;
  const result = await stopPageTarget(async () => { called = true; }, { jobId: "", target: estopTargetArgs({ connection_mode: "network", network_host: "" }) });
  assert.equal(result.kind, "no_target");
  assert.equal(called, false);
});

test("页面急停：两条通道都失败时报告失败并带回后端原因", async () => {
  const result = await stopPageTarget(
    async (_, data) => ({ ok: false, data: { success: false, result: data.job_id ? "任务不存在" : "连接被拒绝" } }),
    { jobId: "wf-x", target: estopTargetArgs({ connection_mode: "serial", port: "COM9", baudrate: "115200" }) },
  );
  assert.equal(result.kind, "failed");
  assert.match(result.message, /任务不存在.*连接被拒绝/);
});

test("急停目标：串口模式未选串口时为 null，网口默认 23", () => {
  assert.equal(estopTargetArgs({ connection_mode: "serial", port: " " }), null);
  assert.deepEqual(estopTargetArgs({ connection_mode: "serial", port: "COM3", baudrate: "" }), { connection_mode: "serial", port: "COM3", baudrate: 115200 });
  assert.deepEqual(estopTargetArgs({ connection_mode: "network", network_host: "h", network_telnet_port: "" }), { connection_mode: "network", host: "h", telnet_port: 23 });
});

test("顶栏急停：后端残留 unknown 旧记录且返回成功时，仍同时向当前页面的设备发送 0x18", async () => {
  const calls = [];
  const pageTarget = estopTargetArgs({
    connection_mode: "network",
    network_host: "127.0.0.1",
    network_telnet_port: "2302",
  });
  const outcome = await triggerTopbarEmergencyStop(
    async () => ({
      ok: true,
      data: {
        success: true,
        result: {
          jobs: [{ job_id: "stale-old-job", target: "127.0.0.1:2301", state: "unknown" }],
        },
      },
    }),
    async (path, data) => {
      calls.push({ path, data });
      return { ok: true, data: { success: true, result: "0x18 已写出" } };
    },
    ({ stoppedJob }) => {
      if (isJobCoveringPageTarget(stoppedJob, { jobId: "", target: pageTarget })) {
        return false;
      }
      void stopPageTarget(
        async (path, data) => {
          calls.push({ path, data });
          return { ok: true, data: { success: true, result: "页面设备已急停" } };
        },
        { jobId: "", target: pageTarget },
      );
      return true;
    },
  );
  assert.equal(outcome.kind, "sent");
  assert.equal(outcome.pageHandled, true);
  assert.deepEqual(calls, [
    { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "stale-old-job" } },
    {
      path: "/api/safe-action",
      data: { action: "emergency_stop", confirmed: true, connection_mode: "network", host: "127.0.0.1", telnet_port: 2302 },
    },
  ]);
});

test("顶栏急停：后端已停掉当前页面同一 running 设备时不重复发第二次，不同设备时双发", () => {
  const netTarget = estopTargetArgs({
    connection_mode: "network",
    network_host: "127.0.0.1",
    network_telnet_port: "23",
  });
  assert.equal(isSameEstopTarget(netTarget, "127.0.0.1:23"), true);
  assert.equal(isSameEstopTarget(netTarget, "tcp://127.0.0.1:23"), true);
  assert.equal(isSameEstopTarget(netTarget, "127.0.0.1:2301"), false);

  // running 且同目标 → 已覆盖
  assert.equal(
    isJobCoveringPageTarget(
      { job_id: "wf-1", target: "127.0.0.1:23", state: "running" },
      { jobId: "", target: netTarget },
    ),
    true,
  );
  // unknown 状态哪怕同端口也不视为已覆盖（旧模拟实例可能已关，需对当前页面连接补发）
  assert.equal(
    isJobCoveringPageTarget(
      { job_id: "wf-old", target: "127.0.0.1:23", state: "unknown" },
      { jobId: "wf-old", target: netTarget },
    ),
    false,
  );
});
