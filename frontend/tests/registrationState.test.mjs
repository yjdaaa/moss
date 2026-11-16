import test from "node:test";
import assert from "node:assert/strict";
import {
  MIN_POINT_DISTANCE_MM,
  REGISTRATION_SESSION_KEY,
  designPoint,
  distance,
  jobBounds,
  jogRequest,
  readRegistrationSession,
  recordedPoint,
  registrationPreviewPayload,
  registrationSummary,
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
