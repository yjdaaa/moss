// 两点对位的画板入口与低功率点射（2026-11-16）。
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  POINTER_DURATION_OPTIONS,
  POINTER_MAX_DURATION_MS,
  POINTER_MAX_POWER,
  POINTER_POWER_OPTIONS,
  REGISTRATION_DRAW_SESSION_KEY,
  REGISTRATION_SESSION_KEY,
  adoptRegisteredDrawRecord,
  drawFramingExtent,
  drawOriginNote,
  pointerFireConfirm,
  pointerFireRequest,
  readRegistrationSession,
  writeRegistrationSession,
} from "../src/registrationState.js";
import { buildDrawFramingPayload, framingConfirmText } from "../src/drawConnection.js";
import { readPageSource } from "./pageSource.mjs";

const srcRoot = path.join(path.dirname(fileURLToPath(import.meta.url)), "../src");
const form = { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: "23" };
const fingerprint = "a".repeat(64);

function registeredRecord(overrides = {}) {
  return {
    workflow_id: "wf-reg-1",
    status: "preview_ready",
    registered_from: "draw-wf-1",
    registration: { rotation_deg: 2, scale: 1 },
    artifacts: {
      frame_mm: { min_x: -40.123456, max_x: 0.5, min_y: -3, max_y: 20 },
      origin_note: "开刻前激光头必须停在第 2 个对位标记上（记录第 2 点时的位置）",
    },
    draw_lab: {
      job_summary: { material: "椴木", can_send: true, actual_width_mm: 40.6, actual_height_mm: 23, registered: true },
      content_fingerprint: fingerprint,
      registered_from: "draw-wf-1",
    },
    next_actions: ["status", "cancel", "confirm_send"],
    ...overrides,
  };
}

test("画板采用对位结果：换成新任务、新指纹，状态写明对刀起点；缺画板视图或来源不符时拒绝", () => {
  const record = registeredRecord();
  const adopted = adoptRegisteredDrawRecord(record, "draw-wf-1");
  assert.equal(adopted.workflowId, "wf-reg-1");
  assert.deepEqual(adopted.result, { success: true, result: record });
  assert.equal(adopted.fingerprint, fingerprint);
  assert.match(adopted.status, /尚未发送/);
  assert.match(adopted.status, /第 2 个对位标记/);
  assert.match(adoptRegisteredDrawRecord(registeredRecord({ draw_lab: undefined })).error, /画板视图/);
  assert.match(adoptRegisteredDrawRecord(registeredRecord(), "draw-other").error, /不是当前画板任务/);
  assert.match(adoptRegisteredDrawRecord({ workflow_id: "x" }).error, /无效/);
  assert.match(adoptRegisteredDrawRecord(null).error, /无效/);
});

test("画板走边框：对位结果按 frame_mm（相对第 2 个标记点，可为负），普通任务以激光头为左下角", () => {
  const registered = drawFramingExtent(registeredRecord());
  assert.deepEqual(registered.frame, { min_x: -40.123, max_x: 0.5, min_y: -3, max_y: 20 });
  assert.ok(Math.abs(registered.width - 40.623) < 1e-9);
  assert.equal(registered.height, 23);
  const payload = buildDrawFramingPayload(form, registered.width, registered.height, null, registered.frame);
  assert.equal(payload.relative_to_head, true);
  assert.equal(payload.min_x, -40.123);
  assert.equal(payload.max_y, 20);
  assert.equal(payload.power, 0, "默认不出光");
  assert.match(framingConfirmText(form, registered.width, registered.height, 0, true), /第 2 个对位标记/);
  assert.match(framingConfirmText(form, 80, 50), /左下角起点/);

  const plain = drawFramingExtent({ draw_lab: { job_summary: { actual_width_mm: 80, actual_height_mm: 50 } } });
  assert.deepEqual(plain, { frame: null, width: 80, height: 50 });
  const plainPayload = buildDrawFramingPayload(form, plain.width, plain.height, null, plain.frame);
  assert.deepEqual([plainPayload.min_x, plainPayload.min_y, plainPayload.max_x, plainPayload.max_y], [0, 0, 80, 50]);
  assert.equal(drawFramingExtent(registeredRecord({ artifacts: {} })), null, "对位结果没有范围时不走边框");
  assert.equal(
    drawFramingExtent(registeredRecord({ artifacts: { frame_mm: { min_x: 1, max_x: 1, min_y: 0, max_y: 2 } } })),
    null,
  );
});

test("对刀起点文字：对位结果用后端 origin_note，普通画板任务是作品左下角", () => {
  assert.match(drawOriginNote(registeredRecord()), /第 2 个对位标记/);
  assert.match(drawOriginNote(registeredRecord({ artifacts: {} })), /第 2 个对位标记/);
  assert.match(drawOriginNote({ workflow_id: "draw-1" }), /作品左下角/);
  assert.match(drawOriginNote(null), /作品左下角/);
});

test("画板与工作台的对位进度分开存，互不覆盖", () => {
  const store = new Map();
  const storage = {
    getItem: (key) => (store.has(key) ? store.get(key) : null),
    setItem: (key, value) => store.set(key, value),
    removeItem: (key) => store.delete(key),
  };
  assert.notEqual(REGISTRATION_DRAW_SESSION_KEY, REGISTRATION_SESSION_KEY);
  writeRegistrationSession({ source: { workflow_id: "wf-1" } }, storage);
  writeRegistrationSession({ source: { workflow_id: "draw-1" } }, storage, REGISTRATION_DRAW_SESSION_KEY);
  assert.equal(readRegistrationSession(storage).source.workflow_id, "wf-1");
  assert.equal(readRegistrationSession(storage, REGISTRATION_DRAW_SESSION_KEY).source.workflow_id, "draw-1");
  writeRegistrationSession(null, storage, REGISTRATION_DRAW_SESSION_KEY);
  assert.equal(store.has(REGISTRATION_DRAW_SESSION_KEY), false);
  assert.equal(readRegistrationSession(storage).source.workflow_id, "wf-1");
});

test("点射请求：功率不超过 S10、时长不超过 200 ms，越界返回 null 而不是截断", () => {
  const target = { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: 23 };
  assert.deepEqual(pointerFireRequest(target, 10, 100), { ...target, confirmed: true, power: 10, duration_ms: 100 });
  assert.deepEqual(pointerFireRequest(target, 3, 50), { ...target, confirmed: true, power: 3, duration_ms: 50 });
  for (const [power, ms] of [[11, 100], [0, 100], [5.5, 100], [10, 201], [10, 5], [10, 100.5], ["x", 100]]) {
    assert.equal(pointerFireRequest(target, power, ms), null, `${power} / ${ms}`);
  }
  assert.equal(pointerFireRequest(null, 10, 100), null);
  assert.equal(POINTER_MAX_POWER, 10);
  assert.equal(POINTER_MAX_DURATION_MS, 200);
  assert.ok(POINTER_POWER_OPTIONS.every((opt) => opt.value >= 1 && opt.value <= POINTER_MAX_POWER));
  assert.ok(POINTER_DURATION_OPTIONS.every((ms) => ms <= POINTER_MAX_DURATION_MS));
});

test("点射确认：第一次附护目镜与反光等安全提醒，之后每次仍要确认", () => {
  const first = pointerFireConfirm({ targetLabel: "<device-ip>:23", power: 10, durationMs: 100, first: true });
  assert.match(first.message, /护目镜/);
  assert.match(first.message, /反光/);
  assert.match(first.message, /1\.0% 功率（S10）出光 100 ms/);
  assert.match(first.confirmText, /护目镜/);
  const later = pointerFireConfirm({ targetLabel: "<device-ip>:23", power: 3, durationMs: 50, first: false });
  assert.match(later.message, /0\.3% 功率（S3）出光 50 ms/);
  assert.match(later.message, /护目镜/);
});

test("点射默认关闭、先确认再请求；画板复用对位面板并单独存进度", () => {
  const panel = readFileSync(path.join(srcRoot, "components/RegistrationPanel.jsx"), "utf8");
  assert.match(panel, /const \[pointerEnabled, setPointerEnabled\] = useState\(false\)/, "点射默认关闭");
  const fire = panel.slice(panel.indexOf("const firePointer = async () =>"), panel.indexOf("const readPosition = async () =>"));
  assert.ok(
    fire.indexOf("showConfirm(") > 0 && fire.indexOf("showConfirm(") < fire.indexOf('postJson("/api/device/pointer-fire"'),
    "每次点射必须先确认再出光",
  );
  assert.match(fire, /if \(!confirmed\) return;/);
  assert.match(fire, /pointerFireRequest\(/, "请求体经 pointerFireRequest 卡上限");

  const draw = readPageSource("pages/DrawPage.jsx");
  assert.match(draw, /<RegistrationPanel[\s\S]*?sessionKey=\{REGISTRATION_DRAW_SESSION_KEY\}/);
  assert.match(draw, /idPrefix="draw-reg"/);
  assert.match(draw, /onRegistered=\{adoptRegistered\}/);
  const adopt = draw.slice(draw.indexOf("function adoptRegistered(record)"), draw.indexOf("async function cancelWorkflow("));
  assert.match(
    adopt,
    /adoptRegisteredDrawRecord\(record, registrationSourceRef\.current\)/,
    "迟到的对位结果须核对是否来自当前画板任务",
  );
  assert.match(draw, /registrationSourceRef\.current = result\?\.result\?\.registered_from \|\| workflowId/);
  assert.match(adopt, /applyPreviewFingerprintSuccess/, "对位结果换指纹，重复发送防呆照常");
  assert.doesNotMatch(adopt, /confirm_send/, "采用对位结果不得顺带发送");
});
