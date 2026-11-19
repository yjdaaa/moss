import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { onEmergencyStop, requestEmergencyStop } from "../src/estopBus.js";
import {
  claimEstopContext,
  createPageEstopResponder,
  drawEstopContexts,
  estopTargetFromDescription,
  estopTargetArgs,
  isJobCoveringPageTarget,
  isSameEstopTarget,
  stopKnownJob,
  stopPageTarget,
  triggerTopbarEmergencyStop,
  visibleEstopContext,
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
    { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "active-running", connection_mode: "serial", port: "COM3", baudrate: 115200 } },
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
  assert.deepEqual(calls, [{ path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "wf-1", connection_mode: "network", host: "127.0.0.1", telnet_port: 23 } }]);
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
    { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "stale-old-job", connection_mode: "network", host: "127.0.0.1", telnet_port: 2301 } },
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

test("顶栏急停：任务查询挂起时到时限即让页面按本页目标补发 0x18，不无限等待", async () => {
  const calls = [];
  const pageTarget = estopTargetArgs({ connection_mode: "network", network_host: "127.0.0.1", network_telnet_port: "2302" });
  let releaseLookup;
  const hungLookup = new Promise((resolve) => { releaseLookup = resolve; });
  let notifiedWith = "never";
  const pending = triggerTopbarEmergencyStop(
    () => hungLookup,
    async (path, data) => { calls.push({ path, data }); return { ok: true, data: { success: true } }; },
    ({ stoppedJob }) => {
      notifiedWith = stoppedJob;
      void stopPageTarget(async (path, data) => {
        calls.push({ path, data });
        return { ok: true, data: { success: true, result: "页面设备已急停" } };
      }, { jobId: "", target: pageTarget });
      return true;
    },
    { pageDeadlineMs: 20 },
  );
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.equal(notifiedWith, null, "查询仍挂起时必须已通知页面（不带 stoppedJob）");
  assert.deepEqual(calls, [
    { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, connection_mode: "network", host: "127.0.0.1", telnet_port: 2302 } },
  ]);
  // 查询最终以超时失败返回：结果如实报告，页面不会被第二次通知
  releaseLookup({ ok: false, status: 0, timedOut: true, data: { success: false, result: "请求超时" } });
  const outcome = await pending;
  assert.equal(outcome.kind, "lookup_failed");
  assert.equal(outcome.pageHandled, true);
  assert.equal(outcome.pageNotifiedEarly, true);
  assert.equal(calls.length, 1);
});

test("顶栏急停：按任务写 0x18 挂起时同样到时限通知页面；后续结果不重复通知", async () => {
  let notifications = 0;
  let releaseStop;
  const pending = triggerTopbarEmergencyStop(
    async () => ({ ok: true, data: { success: true, result: { jobs: [{ job_id: "wf-1", target: "127.0.0.1:23", state: "running" }] } } }),
    () => new Promise((resolve) => { releaseStop = resolve; }),
    () => { notifications += 1; return true; },
    { pageDeadlineMs: 20 },
  );
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.equal(notifications, 1);
  releaseStop({ ok: true, data: { success: true, result: "0x18 已写出" } });
  const outcome = await pending;
  assert.equal(outcome.kind, "sent");
  assert.equal(notifications, 1, "已提前通知过页面，不得再通知第二次");
});

test("顶栏急停：多任务无法确定（ambiguous）时不猜设备，但仍通知当前页面按本页目标补发", async () => {
  let backendStopCalled = false;
  const pageCalls = [];
  const outcome = await triggerTopbarEmergencyStop(
    async () => ({ ok: true, data: { success: true, result: { jobs: [
      { job_id: "a", target: "192.168.1.9:23", state: "running" },
      { job_id: "b", target: "COM3@115200", state: "running" },
    ] } } }),
    async () => { backendStopCalled = true; return { ok: true, data: { success: true } }; },
    ({ stoppedJob }) => { pageCalls.push(stoppedJob); return true; },
  );
  assert.equal(outcome.kind, "ambiguous");
  assert.equal(backendStopCalled, false, "多任务时顶栏绝不随机挑选任务");
  assert.deepEqual(pageCalls, [null]);
  assert.equal(outcome.pageHandled, true);
});

test("顶栏急停：后端任务正常返回时不提前通知页面，保持先任务后页面的顺序", async () => {
  const order = [];
  const outcome = await triggerTopbarEmergencyStop(
    async () => ({ ok: true, data: { success: true, result: { jobs: [] } } }),
    async () => ({ ok: true, data: { success: true } }),
    ({ stoppedJob }) => { order.push(["page", stoppedJob]); return false; },
    { pageDeadlineMs: 1000 },
  );
  assert.equal(outcome.kind, "none");
  assert.equal(outcome.pageNotifiedEarly, false);
  assert.deepEqual(order, [["page", null]]);
});

test("页面急停响应：无目标不处理；顶栏已停掉覆盖本页的 running 任务不重复；否则异步执行并返回 true", async () => {
  const runs = [];
  const target = estopTargetArgs({ connection_mode: "network", network_host: "127.0.0.1", network_telnet_port: "23" });
  const none = createPageEstopResponder(() => ({ jobId: " ", target: null }), (ctx) => runs.push(ctx));
  assert.equal(none({ stoppedJob: null }), false);

  const responder = createPageEstopResponder(() => ({ jobId: " wf-1 ", target }), (ctx) => runs.push(ctx));
  assert.equal(responder({ stoppedJob: { job_id: "wf-1", target: "x", state: "running" } }), false);
  assert.equal(responder({ stoppedJob: { job_id: "other", target: "127.0.0.1:23", state: "running" } }), false);
  assert.equal(runs.length, 0);
  // unknown 旧记录不算覆盖
  assert.equal(responder({ stoppedJob: { job_id: "wf-1", target: "127.0.0.1:23", state: "unknown" } }), true);
  assert.equal(responder({ stoppedJob: null }), true);
  assert.deepEqual(runs, [{ jobId: "wf-1", target }, { jobId: "wf-1", target }]);
});

test("页面急停：任务通道请求超时后仍按设备目标补发", async () => {
  const calls = [];
  const result = await stopPageTarget(
    async (path, data) => {
      calls.push(data);
      return data.job_id
        ? { ok: false, status: 0, timedOut: true, data: { success: false, result: "请求超时（4000 ms 未响应）" } }
        : { ok: true, data: { success: true, result: "急停 0x18 已发送" } };
    },
    { jobId: "wf-hung", target: estopTargetArgs({ connection_mode: "serial", port: "COM4", baudrate: "115200" }) },
  );
  assert.equal(result.kind, "sent");
  assert.equal(result.via, "target");
  assert.equal(calls.length, 2);
});

test("双设备切页急停：隐藏画板有在途任务时补发，空闲时让出", async () => {
  const bus = new EventTarget();
  const targetDraw = estopTargetArgs({ connection_mode: "network", network_host: "127.0.0.1", network_telnet_port: "2301" });
  const targetHome = estopTargetArgs({ connection_mode: "network", network_host: "127.0.0.1", network_telnet_port: "2302" });
  let drawActive = true;
  let inFlight = [];
  let backendJobs = [];
  const calls = [];
  const alerts = [];

  const getTargets = async () => ({ ok: true, data: { success: true, result: { jobs: backendJobs } } });
  const sendStop = async (path, data) => {
    calls.push({ path, data });
    return { ok: true, data: { success: true, result: "0x18 已写出" } };
  };
  const notifyPage = (opts) => requestEmergencyStop(bus, opts);
  const flushAsync = () => new Promise((resolve) => setTimeout(resolve, 0));

  // 模拟 main.jsx 挂载顺序：#draw-keepalive 的 DrawPage 先挂并常驻，切走后 HomePage 才后挂
  let offHome = null;
  const offDraw = onEmergencyStop(
    createPageEstopResponder(
      () => drawEstopContexts({ active: drawActive, target: targetDraw, inFlight }),
      async (ctx) => {
        const outcome = await stopPageTarget(sendStop, ctx);
        alerts.push({ page: "draw", outcome });
        return outcome;
      },
    ),
    bus,
  );

  try {
    // 1. 画板激活时（active=true）：无后台任务时停画板设备 A
    const onDraw = await triggerTopbarEmergencyStop(getTargets, sendStop, notifyPage);
    await flushAsync();
    assert.equal(onDraw.pageHandled, true);
    assert.deepEqual(calls, [
      { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, ...targetDraw } },
    ]);
    assert.deepEqual(alerts.map((a) => a.page), ["draw"]);

    // 2. 切到工作台设备 B（drawActive=false，HomePage 后注册监听）：隐藏画板返回 null 让位，只停设备 B
    calls.length = 0;
    alerts.length = 0;
    drawActive = false;
    offHome = onEmergencyStop(
      createPageEstopResponder(
        () => ({ jobId: "", target: targetHome }),
        async (ctx) => {
          const outcome = await stopPageTarget(sendStop, ctx);
          alerts.push({ page: "home", outcome });
          return outcome;
        },
      ),
      bus,
    );

    const onHome = await triggerTopbarEmergencyStop(getTargets, sendStop, notifyPage);
    await flushAsync();
    assert.equal(onHome.pageHandled, true);
    assert.deepEqual(calls, [
      { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, ...targetHome } },
    ]);
    assert.deepEqual(alerts.map((a) => a.page), ["home"]);

    // 3. 画板隐藏且后端登记了后台作业时：全局通道仍停后台作业，且当前工作台设备 B 照常补发、不被隐藏画板截流
    calls.length = 0;
    alerts.length = 0;
    inFlight = [{ jobId: "wf-draw-bg", target: targetDraw }];
    backendJobs = [{ job_id: "wf-draw-bg", target: "127.0.0.1:2301", state: "running" }];
    const withBgJob = await triggerTopbarEmergencyStop(getTargets, sendStop, notifyPage);
    await flushAsync();
    assert.equal(withBgJob.kind, "sent");
    assert.equal(withBgJob.pageHandled, true);
    assert.deepEqual(calls, [
      { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "wf-draw-bg", ...targetDraw } },
      { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, ...targetHome } },
    ]);
    assert.deepEqual(alerts.map((a) => a.page), ["home"]);

    // 4. 无页面急停的语音页：先由后端停已登记作业。登记消失时仍由画板按原目标补发。
    offHome();
    offHome = null;
    calls.length = 0;
    alerts.length = 0;
    const voiceWithBg = await triggerTopbarEmergencyStop(getTargets, sendStop, notifyPage);
    await flushAsync();
    assert.equal(voiceWithBg.kind, "sent");
    assert.equal(voiceWithBg.pageHandled, false);
    assert.deepEqual(calls, [
      { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "wf-draw-bg", ...targetDraw } },
    ]);
    assert.equal(alerts.length, 0);

    calls.length = 0;
    backendJobs = [];
    const voiceInFlight = await triggerTopbarEmergencyStop(getTargets, async () => assert.fail("无后台记录"), notifyPage);
    await flushAsync();
    assert.equal(voiceInFlight.kind, "none");
    assert.equal(voiceInFlight.pageHandled, true, "隐藏画板补发后顶栏知道已有响应");
    assert.deepEqual(calls, [
      { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, job_id: "wf-draw-bg", ...targetDraw } },
    ]);
    assert.deepEqual(alerts.map((a) => a.page), ["draw"]);
    calls.length = 0;
    alerts.length = 0;
    inFlight = [];
    const voiceNoJob = await triggerTopbarEmergencyStop(getTargets, sendStop, notifyPage);
    await flushAsync();
    assert.equal(voiceNoJob.kind, "none");
    assert.equal(voiceNoJob.pageHandled, false);
    assert.equal(calls.length, 0, "隐藏画板不得在无目标页面下拦截并触发设备 A 急停");

    // 5. 切回画板（drawActive=true）：画板恢复响应顶栏急停
    drawActive = true;
    const backToDraw = await triggerTopbarEmergencyStop(getTargets, sendStop, notifyPage);
    await flushAsync();
    assert.equal(backToDraw.pageHandled, true);
    assert.deepEqual(calls, [
      { path: "/api/safe-action", data: { action: "emergency_stop", confirmed: true, ...targetDraw } },
    ]);
    assert.deepEqual(alerts.map((a) => a.page), ["draw"]);
  } finally {
    offHome?.();
    offDraw();
  }
});

test("DrawPage 隐藏时沿用提交时的目标，当前浏览器设置仅供空闲可见画板使用", () => {
  const drawSource = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  const mainSource = readFileSync(new URL("../src/main.jsx", import.meta.url), "utf8");
  assert.match(mainSource, /<DrawPage[^>]*\bactive=\{drawActive\}/);
  assert.match(drawSource, /usePageEmergencyStop\(\s*\(\) => drawEstopContexts\(\{[^}]*inFlight: inFlightJobsRef\.current\.values/);
  assert.match(drawSource, /const inFlight = \{ jobId: workflowId, target: estopTargetArgs\(form\) \}/);
});

test("审批设备和当前页都收到急停，同一作业和同一设备只认领一次", async () => {
  for (const pageFirst of [true, false]) {
    const bus = new EventTarget();
    const calls = [];
    const sendStop = async (path, body) => {
      calls.push([path, body]);
      return { ok: true, data: { success: true, result: "0x18 已写出" } };
    };
    const approval = { jobId: "approved-a", target: estopTargetFromDescription("[2001:db8::1]:2301") };
    const pageTarget = estopTargetFromDescription("127.0.0.1:2302");
    const page = visibleEstopContext(approval.jobId, pageTarget, "[2001:db8::1]:2301");
    assert.equal(isJobCoveringPageTarget({ job_id: approval.jobId, target: "[2001:db8::1]:2301", state: "running" },
      { jobId: approval.jobId, target: pageTarget }), false, "共同编号不能覆盖其他设备");
    const respond = (ctx) => onEmergencyStop(createPageEstopResponder(() => ctx, (data) => stopPageTarget(sendStop, data)), bus, { byTarget: true });
    if (pageFirst) respond(page);
    // 审批窗和 AI 预览窗即使都跟踪到同一作业，也只有第一个触发写出。
    for (const ctx of [approval, { jobId: approval.jobId }, { target: approval.target }]) {
      onEmergencyStop((detail) => {
        if (claimEstopContext(detail, ctx)) void stopPageTarget(sendStop, ctx);
        return false;
      }, bus, { byTarget: true });
    }
    if (!pageFirst) respond(page);
    assert.equal(requestEmergencyStop(bus), true);
    assert.deepEqual(calls.map(([path]) => path), ["/api/safe-action", "/api/safe-action"]);
    assert.deepEqual(calls.map(([, body]) => body.job_id || body.telnet_port).sort(), [2302, "approved-a"]);
  }
});

test("隐藏画板的 SD 作业脱离登记时，在语音页按原设备补发 0x18", async () => {
  const bus = new EventTarget();
  const calls = [];
  const target = estopTargetFromDescription("127.0.0.1:2301");
  const context = () => drawEstopContexts({ active: false, inFlight: [{ jobId: "sd-a", target }] });
  const respond = createPageEstopResponder(context, (ctx) => stopPageTarget(async (path, body) => {
    calls.push([path, body]);
    const ok = !body.job_id && body.telnet_port === 2301;
    return { ok, data: { success: ok } };
  }, ctx));
  onEmergencyStop(respond, bus);
  assert.equal(requestEmergencyStop(bus), true);
  await Promise.resolve();
  assert.deepEqual(calls, [
    ["/api/safe-action", { action: "emergency_stop", confirmed: true, job_id: "sd-a", ...target }],
    ["/api/safe-action", { action: "emergency_stop", confirmed: true, ...target }],
  ]);
});

test("换了新预览的隐藏画板仍认领旧作业，区分两个仅串口大小写不同的设备", () => {
  const oldTarget = estopTargetFromDescription("/dev/cu.Moss@115200");
  const nextTarget = estopTargetFromDescription("/dev/cu.moss@115200");
  const inFlight = new Map([["old", { jobId: "old", target: oldTarget }]]);
  const bus = new EventTarget();
  const calls = [];
  const respond = createPageEstopResponder(() => drawEstopContexts({
    active: false, target: nextTarget, inFlight: inFlight.values(),
  }), (ctx) => calls.push(ctx));
  onEmergencyStop(respond, bus);
  assert.equal(requestEmergencyStop(bus), true);
  assert.deepEqual(calls, [{ jobId: "old", target: oldTarget }]);
  inFlight.set("new", { jobId: "new", target: nextTarget });
  calls.length = 0;
  requestEmergencyStop(bus);
  assert.deepEqual(calls.map(({ jobId }) => jobId), ["old", "new"]);
});

test("同一作业编号和两个明确目标时，各只向匹配的设备写出一次", async () => {
  for (const order of [[2301, 2302], [2302, 2301]]) {
    const bus = new EventTarget();
    const requests = [];
    const writes = [];
    const pending = [];
    for (const port of order) {
      const context = { jobId: "shared", target: estopTargetFromDescription(`127.0.0.1:${port}`) };
      onEmergencyStop(createPageEstopResponder(() => context, (params) => {
        pending.push(stopPageTarget(async (path, body) => {
          requests.push({ path, body });
          // 原任务在设备 B。带编号的请求指定其他设备时，后端拒绝写出。
          const ok = !body.job_id || body.telnet_port === 2302;
          if (ok) writes.push(body.telnet_port);
          return { ok, data: { success: ok } };
        }, params));
      }), bus, { byTarget: true });
    }
    assert.equal(requestEmergencyStop(bus), true);
    await Promise.all(pending);
    assert.deepEqual(writes.sort(), [2301, 2302]);
    assert.deepEqual(requests.filter(({ body }) => body.job_id).map(({ body }) => body.telnet_port), order);
    assert.ok(requests.every(({ path }) => path === "/api/safe-action"));
  }
});

test("确认发送的响应丢失时，画板继续轮询原任务并保留急停目标", () => {
  const source = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  assert.match(source, /else if \(res.status === 0\) \{[^}]*startDrawJobPolling\(workflowId, inFlight\);/);
  assert.match(source, /if \(!inFlight.stop\) inFlightJobsRef.current.delete\(workflowId\)/);
});
