import test from "node:test";
import assert from "node:assert/strict";
import {
  MIN_POINT_DISTANCE_MM,
  REGISTRATION_SESSION_KEY,
  WORKBENCH_JOG_STEPS_MM,
  WORKBENCH_MAX_JOG_STEP_MM,
  createHeadSafeActionRunner,
  designPoint,
  distance,
  jobBounds,
  jogRequest,
  planHeadSafeAction,
  readRegistrationSession,
  recordedPoint,
  registrationPreviewPayload,
  registrationSummary,
  resolveHeadJogGuard,
  safeOriginActionRequest,
  workbenchJogRequest,
  writeRegistrationSession,
} from "../src/registrationState.js";

test("任务范围优先用出光凸包（不含过扫描），否则用 frame_mm", () => {
  const artifacts = {
    frame_mm: { min_x: 0, max_x: 42, min_y: 0, max_y: 20 },
    frame_hull_mm: [[1, 0], [41, 0], [41, 20], [1, 20]],
  };
  assert.deepEqual(jobBounds(artifacts), { min_x: 1, max_x: 41, min_y: 0, max_y: 20 });
  assert.deepEqual(jobBounds({ frame_mm: artifacts.frame_mm }), artifacts.frame_mm);
  assert.equal(jobBounds({ frame_mm: { min_x: 0, max_x: 0, min_y: 0, max_y: 1 } }), null);
  assert.equal(jobBounds({}), null);
});

test("锚点换算成相对激光头起点的设计坐标，自定义按距左下角", () => {
  const bounds = { min_x: -10, max_x: 30, min_y: -5, max_y: 15 };
  assert.deepEqual(designPoint(bounds, "lower_left"), { x: -10, y: -5 });
  assert.deepEqual(designPoint(bounds, "upper_right"), { x: 30, y: 15 });
  assert.deepEqual(designPoint(bounds, "center"), { x: 10, y: 5 });
  assert.deepEqual(designPoint(bounds, "custom", { x: "4", y: "2.5" }), { x: -6, y: -2.5 });
  assert.equal(designPoint(bounds, "custom", { x: "", y: "1" }), null);
  assert.equal(designPoint(null, "lower_left"), null);
});

test("读位置结果只认有限坐标，坐标类型保留 MPos / WPos", () => {
  assert.deepEqual(recordedPoint({ frame: "mpos", position: { x: 1.5, y: 2 } }), { x: 1.5, y: 2, frame: "mpos" });
  assert.deepEqual(recordedPoint({ frame: "wpos", position: { x: 0, y: 0 } }), { x: 0, y: 0, frame: "wpos" });
  assert.equal(recordedPoint({ position: { x: "a", y: 1 } }), null);
  assert.equal(recordedPoint(null), null);
});

test("对位预览请求体：校验顺序、距离与坐标类型，数值保留三位小数", () => {
  const design = [{ x: 0, y: 0 }, { x: 40, y: 0 }];
  const measured = [{ x: 100.12345, y: 50, frame: "mpos" }, { x: 100, y: 90, frame: "mpos" }];
  const payload = registrationPreviewPayload({ workflowId: "wf-1", design, measured, allowScale: true, machineMotion: { x_accel_mm_s2: 500 } });
  assert.deepEqual(payload, {
    workflow_id: "wf-1",
    design_points: [{ x: 0, y: 0 }, { x: 40, y: 0 }],
    measured_points: [{ x: 100.123, y: 50 }, { x: 100, y: 90 }],
    position_frame: "mpos",
    allow_scale: true,
    machine_motion: { x_accel_mm_s2: 500 },
  });
  assert.match(registrationPreviewPayload({ design, measured }).error, /加工轨迹/);
  assert.match(registrationPreviewPayload({ workflowId: "wf", design, measured: [measured[0], null] }).error, /第 1、第 2/);
  assert.match(
    registrationPreviewPayload({ workflowId: "wf", design, measured: [measured[0], { ...measured[1], frame: "wpos" }] }).error,
    /坐标类型/,
  );
  assert.match(
    registrationPreviewPayload({ workflowId: "wf", design: [{ x: 0, y: 0 }, { x: MIN_POINT_DISTANCE_MM - 1, y: 0 }], measured }).error,
    /设计参考点/,
  );
  assert.match(
    registrationPreviewPayload({ workflowId: "wf", design, measured: [measured[1], { ...measured[1], x: 101 }] }).error,
    /标记点至少/,
  );
  assert.equal(registrationPreviewPayload({ workflowId: "wf", design, measured }).allow_scale, false);
});

test("点动只生成带 confirmed 的 relative_move，零位移不发", () => {
  const target = { connection_mode: "network", host: "<device-ip>", telnet_port: 23 };
  assert.deepEqual(jogRequest(target, 0.1, 0), {
    action: "relative_move", confirmed: true, x: 0.1, y: 0, feed_rate: 1000, ...target,
  });
  assert.equal(jogRequest(target, 0, 0), null);
  assert.equal(jogRequest(null, 1, 0), null);
  assert.equal(jogRequest(target, Number.NaN, 1), null);
});

test("结果摘要写明旋转、缩放、残差与偏移", () => {
  const lines = registrationSummary({
    rotation_deg: 1.234, scale: 1, scale_applied: false, distance_ratio: 1.002, residual_mm: 0.04,
    offset_mm: { x: -40, y: 0.5 },
  });
  assert.equal(lines.length, 4);
  assert.match(lines[0], /旋转 1\.23°/);
  assert.match(lines[1], /不缩放.*100\.20%/);
  assert.match(lines[2], /0\.040 mm/);
  assert.match(lines[3], /X -40\.00 \/ Y 0\.50/);
  assert.match(registrationSummary({ scale: 1.03, scale_applied: true })[1], /等比缩放 103\.00%/);
  assert.deepEqual(registrationSummary(null), []);
  assert.ok(Math.abs(distance({ x: 0, y: 0 }, { x: 3, y: 4 }) - 5) < 1e-9);
});

test("会话记录读写容错：存储不可用不抛错", () => {
  const store = new Map();
  const storage = {
    getItem: (key) => (store.has(key) ? store.get(key) : null),
    setItem: (key, value) => store.set(key, value),
    removeItem: (key) => store.delete(key),
  };
  assert.equal(writeRegistrationSession({ points: [null, null] }, storage), true);
  assert.deepEqual(readRegistrationSession(storage), { points: [null, null] });
  assert.equal(writeRegistrationSession(null, storage), true);
  assert.equal(store.has(REGISTRATION_SESSION_KEY), false);
  const broken = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
  assert.equal(readRegistrationSession(broken), null);
  assert.equal(writeRegistrationSession({ a: 1 }, broken), false);
  storage.setItem(REGISTRATION_SESSION_KEY, "not json");
  assert.equal(readRegistrationSession(storage), null);
});

test("B3 工作台现场移头：步长严格限制为 0.1 / 0.5 / 1 / 5 / 10 mm（上限 10 mm），越界或斜向返回 null 不截断", () => {
  const target = { connection_mode: "network", host: "127.0.0.1", telnet_port: 2302 };
  assert.deepEqual(WORKBENCH_JOG_STEPS_MM, [0.1, 0.5, 1, 5, 10]);
  assert.equal(WORKBENCH_MAX_JOG_STEP_MM, 10);

  for (const step of [0.1, 0.5, 1, 5, 10]) {
    assert.deepEqual(workbenchJogRequest(target, 1, 0, step), {
      action: "relative_move",
      confirmed: true,
      x: step,
      y: 0,
      feed_rate: 1000,
      ...target,
    });
    assert.deepEqual(workbenchJogRequest(target, 0, -1, step), {
      action: "relative_move",
      confirmed: true,
      x: 0,
      y: -step,
      feed_rate: 1000,
      ...target,
    });
  }

  for (const badStep of [0, -1, 0.05, 0.2, 2, 10.1, 15, 20, 100, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.equal(workbenchJogRequest(target, 1, 0, badStep), null, `越界或非允许步长 ${badStep} 必须拒绝`);
    assert.equal(planHeadSafeAction({ kind: "jog", target, dxUnit: 1, dyUnit: 0, stepMm: badStep }), null);
  }
  assert.equal(workbenchJogRequest(target, 1, 1, 5), null, "禁止双轴斜向点动");
  assert.equal(workbenchJogRequest(target, 0, 0, 5), null);
  assert.equal(workbenchJogRequest(null, 1, 0, 5), null);

  assert.deepEqual(safeOriginActionRequest(target, "set_origin"), { action: "set_origin", confirmed: true, ...target });
  assert.deepEqual(safeOriginActionRequest(target, "unlock"), { action: "unlock", confirmed: true, ...target });
  assert.equal(safeOriginActionRequest(target, "pointer_fire"), null, "不提供出光动作");
  assert.equal(safeOriginActionRequest(null, "set_origin"), null);
});

test("B3 工作台现场移头：未配地址、未探测、未连接、正在加工、在途执行时禁用并说明原因", () => {
  const netForm = { connection_mode: "network", network_host: "127.0.0.1", network_telnet_port: "2302" };
  const okStatus = { connected: true, checkedAt: 1700000000000, summary: "Idle ($32=1)" };

  const unconfigured = resolveHeadJogGuard({
    connectionForm: { connection_mode: "network", network_host: "" },
    connectionStatus: null,
  });
  assert.equal(unconfigured.disabled, true);
  assert.equal(unconfigured.code, "unconfigured");
  assert.match(unconfigured.reason, /修改连接/);

  const unprobed = resolveHeadJogGuard({
    connectionForm: netForm,
    connectionStatus: { connected: false, summary: "未连接" },
  });
  assert.equal(unprobed.disabled, true);
  assert.equal(unprobed.code, "unprobed");
  assert.match(unprobed.reason, /握手探测/);

  const disconnected = resolveHeadJogGuard({
    connectionForm: netForm,
    connectionStatus: { connected: false, checkedAt: 1700000000000, summary: "连接超时" },
  });
  assert.equal(disconnected.disabled, true);
  assert.equal(disconnected.code, "disconnected");
  assert.match(disconnected.reason, /设备未连接.*连接超时/);

  for (const overlayStatus of ["preparing", "sending", "running", "awaiting_completion"]) {
    const running = resolveHeadJogGuard({ connectionForm: netForm, connectionStatus: okStatus, overlayStatus });
    assert.equal(running.disabled, true, `overlayStatus=${overlayStatus} 必须禁用`);
    assert.equal(running.code, "job_active");
    assert.match(running.reason, /正在加工或发送指令中/);
  }

  const framing = resolveHeadJogGuard({ connectionForm: netForm, connectionStatus: okStatus, framingBusy: true });
  assert.equal(framing.disabled, true);
  assert.equal(framing.code, "job_active");

  const globalJob = resolveHeadJogGuard({
    connectionForm: netForm,
    connectionStatus: okStatus,
    activeJobs: [{ job_id: "wf-voice-1", state: "running" }],
  });
  assert.equal(globalJob.disabled, true);
  assert.equal(globalJob.code, "job_active");

  const computing = resolveHeadJogGuard({ connectionForm: netForm, connectionStatus: okStatus, busy: true });
  assert.equal(computing.disabled, true);
  assert.equal(computing.code, "busy");

  const inFlight = resolveHeadJogGuard({ connectionForm: netForm, connectionStatus: okStatus, workingAction: "jog" });
  assert.equal(inFlight.disabled, true);
  assert.equal(inFlight.code, "in_flight");
  assert.match(inFlight.reason, /不排队连续移动/);

  const ready = resolveHeadJogGuard({ connectionForm: netForm, connectionStatus: okStatus });
  assert.equal(ready.disabled, false);
  assert.equal(ready.code, "ready");
  assert.deepEqual(ready.stopTarget, { connection_mode: "network", host: "127.0.0.1", telnet_port: 2302 });
});

test("B3 工作台现场移头：每次动作先二次确认、在途防连点排队、严格模式 202 进桌面审批", async () => {
  const readyGuard = resolveHeadJogGuard({
    connectionForm: { connection_mode: "network", network_host: "127.0.0.1", network_telnet_port: "2302" },
    connectionStatus: { connected: true, checkedAt: 1700000000000, summary: "Idle" },
  });

  // 1. 用户取消二次确认：绝不向 /api/safe-action 发送请求
  const cancelPosts = [];
  const cancelRunner = createHeadSafeActionRunner({
    confirmFn: async () => false,
    postFn: async (url, body) => {
      cancelPosts.push([url, body]);
      return { ok: true, data: { success: true } };
    },
  });
  const cancelled = await cancelRunner({ kind: "jog", dxUnit: 1, dyUnit: 0, stepMm: 5 }, readyGuard);
  assert.equal(cancelled.status, "cancelled");
  assert.deepEqual(cancelPosts, [], "取消确认框后不得发送任何请求");

  // 2. 连续快速点击：确认框挂起与请求挂起期间同步拦截重入，只执行 1 次移动、绝不排队
  let releaseConfirm;
  let releasePost;
  let confirmCount = 0;
  const posts = [];
  const workingStates = [];
  const slowRunner = createHeadSafeActionRunner({
    confirmFn: async () => {
      confirmCount += 1;
      await new Promise((resolve) => {
        releaseConfirm = resolve;
      });
      return true;
    },
    postFn: async (url, body) => {
      posts.push({ url, body });
      await new Promise((resolve) => {
        releasePost = resolve;
      });
      return { ok: true, status: 200, data: { success: true, result: "相对安全移动 (+10.000, +0.000) 已执行" } };
    },
    onWorkingChange: (state) => workingStates.push(state),
  });

  const firstClick = slowRunner({ kind: "jog", dxUnit: 1, dyUnit: 0, stepMm: 10 }, readyGuard);
  const rapidClickDuringConfirm = await slowRunner({ kind: "jog", dxUnit: 1, dyUnit: 0, stepMm: 10 }, readyGuard);
  assert.equal(rapidClickDuringConfirm.status, "blocked", "确认框尚未关闭时的快速连点必须被同步拦截");
  assert.equal(confirmCount, 1, "不得叠加弹出第二个确认框");

  releaseConfirm();
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(posts.length, 1, "确认后发出单次 /api/safe-action 请求");
  const rapidClickDuringPost = await slowRunner({ kind: "set_origin" }, readyGuard);
  assert.equal(rapidClickDuringPost.status, "blocked", "请求在途期间连点其他动作也必须被拦截");

  releasePost();
  const firstOutcome = await firstClick;
  assert.equal(firstOutcome.status, "ok");
  assert.deepEqual(posts, [
    {
      url: "/api/safe-action",
      body: {
        action: "relative_move",
        confirmed: true,
        x: 10,
        y: 0,
        feed_rate: 1000,
        connection_mode: "network",
        host: "127.0.0.1",
        telnet_port: 2302,
      },
    },
  ]);
  assert.deepEqual(workingStates, ["jog", ""], "完成后释放工作锁恢复可点");

  // 3. 严格模式返回 HTTP 202 approval_required：提示等待桌面审批
  const alerts = [];
  const strictRunner = createHeadSafeActionRunner({
    confirmFn: async () => true,
    postFn: async () => ({
      ok: true,
      status: 202,
      data: { success: true, result: { status: "approval_required", speech: "请在电脑端批准设为工作原点" } },
    }),
    alertFn: async (msg, opts) => alerts.push({ msg, opts }),
  });
  const approvalRes = await strictRunner({ kind: "set_origin" }, readyGuard);
  assert.equal(approvalRes.status, "approval_required");
  assert.equal(alerts.length, 1);
  assert.equal(alerts[0].opts.title, "等待桌面审批");
  assert.match(alerts[0].msg, /批准/);

  // 4. 回工作原点（absolute_move X0 Y0 · G1 控速）、机械回零（$H，固件未开回零时禁用）与加工中实时调参
  const { liveOverrideRequest, resolveLiveOverrideTarget } = await import("../src/registrationState.js");
  const returnPlan = planHeadSafeAction({
    kind: "return_origin",
    target: readyGuard.stopTarget,
    targetLabel: "127.0.0.1:2302",
  });
  assert.match(returnPlan.dialog.title, /G1 X0 Y0/, "N4：回工作原点确认框须标明 G1 控速移回");
  assert.match(returnPlan.dialog.message, /1000 mm\/min/);
  assert.deepEqual(returnPlan.buildConfirmedPayload(), {
    action: "absolute_move",
    confirmed: true,
    x: 0,
    y: 0,
    feed_rate: 1000,
    connection_mode: "network",
    host: "127.0.0.1",
    telnet_port: 2302,
  });
  assert.deepEqual(
    planHeadSafeAction({ kind: "home", target: readyGuard.stopTarget, targetLabel: "127.0.0.1:2302" })
      .buildConfirmedPayload(),
    {
      action: "home",
      confirmed: true,
      connection_mode: "network",
      host: "127.0.0.1",
      telnet_port: 2302,
    },
  );
  assert.equal(
    planHeadSafeAction({ kind: "home", target: readyGuard.stopTarget, homingDisabled: true }),
    null,
    "固件未开启回零（$22=0）时不得生成机械回零请求",
  );
  const noHomingGuard = resolveHeadJogGuard({
    connectionForm: { mode: "network", host: "127.0.0.1", telnetPort: "2302" },
    connectionStatus: { checkedAt: 1700000000000, connected: true, homingEnabled: false },
  });
  assert.equal(noHomingGuard.homingDisabled, true);
  assert.match(noHomingGuard.homingReason, /\$22=0/);
  assert.deepEqual(liveOverrideRequest(readyGuard.stopTarget, "power_override_minus_10"), {
    action: "power_override_minus_10",
    confirmed: true,
    connection_mode: "network",
    host: "127.0.0.1",
    telnet_port: 2302,
  });
  assert.equal(liveOverrideRequest(readyGuard.stopTarget, "unknown_action"), null);

  // N1：加工中实时调参必须走任务原通道，连接面板中途改成别的设备时不受影响，无运行任务时返回 null
  assert.deepEqual(
    resolveLiveOverrideTarget({
      sendProgress: { target: "127.0.0.1:2301" },
      activeJobs: [{ job_id: "wf-1", workflow_id: "wf-1", state: "running", target: "127.0.0.1:2301" }],
      currentWorkflow: { workflow_id: "wf-1" },
    }),
    { connection_mode: "network", host: "127.0.0.1", telnet_port: 2301 },
  );
  assert.deepEqual(
    resolveLiveOverrideTarget({
      sendProgress: { target: "" },
      activeJobs: [
        { job_id: "wf-other", workflow_id: "wf-other", state: "running", target: "10.0.0.9:23" },
        { job_id: "wf-cur", workflow_id: "wf-cur", state: "running", target: "COM5@115200" },
      ],
      currentWorkflow: { workflow_id: "wf-cur" },
    }),
    { connection_mode: "serial", port: "COM5", baudrate: 115200 },
  );
  assert.equal(
    resolveLiveOverrideTarget({
      sendProgress: null,
      activeJobs: [{ job_id: "wf-old", state: "unknown", target: "127.0.0.1:2301" }],
      currentWorkflow: { workflow_id: "wf-1" },
    }),
    null,
    "没有运行中任务通道时不得退回连接表单",
  );
});
