import assert from "node:assert/strict";
import test from "node:test";
import {
  aiOriginLabel,
  aiPreviewNotes,
  aiWorkflowStarted,
  canStartAiPreview,
  connectionView,
  framingRequest,
  isAiOrigin,
  originNote,
  pendingAiPreviews,
  savedConnectionForm,
  strictHintText,
} from "../src/aiPreviewState.js";

const record = (id, overrides = {}) => ({ workflow_id: id, status: "preview_ready", origin: "voice", created_at: 1000, ...overrides });

test("only AI-originated previews that still await confirmation pop up, newest first", () => {
  const list = pendingAiPreviews(
    [
      record("wf-old", { created_at: 900 }),
      record("wf-voice", { created_at: 1001 }),
      record("wf-bridge", { origin: "mcp_bridge", created_at: 1005 }),
      record("wf-gateway", { origin: "gateway", created_at: 1003 }),
      record("wf-desktop", { origin: "desktop" }),
      record("wf-lan", { origin: "lan" }),
      record("wf-legacy", { origin: undefined }),
      record("wf-sent", { status: "sending" }),
      record("wf-cancelled", { status: "cancelled" }),
      record("wf-dismissed"),
      record("wf-approval"),
      null,
      { status: "preview_ready", origin: "voice" },
    ],
    { since: 950, dismissed: new Set(["wf-dismissed"]), approvalTargets: new Set(["wf-approval"]) },
  );
  assert.deepEqual(list.map((item) => item.workflow_id), ["wf-bridge", "wf-gateway", "wf-voice"]);
  assert.deepEqual(pendingAiPreviews(null), []);
});

test("origin labels and started states", () => {
  assert.equal(isAiOrigin("voice"), true);
  assert.equal(isAiOrigin("desktop"), false);
  assert.equal(isAiOrigin("toString"), false);
  assert.equal(aiOriginLabel("mcp_bridge"), "小智");
  assert.equal(aiOriginLabel("unknown"), "AI");
  assert.equal(aiWorkflowStarted({ status: "sending" }), true);
  assert.equal(aiWorkflowStarted({ status: "running" }), true);
  assert.equal(aiWorkflowStarted({ status: "preview_ready" }), false);
  assert.equal(aiWorkflowStarted(null), false);
});

function memoryStorage(values) {
  return { getItem: (key) => (key in values ? values[key] : null) };
}

test("device target comes from the workbench, falling back to the draw board, never guessed", () => {
  const forge = savedConnectionForm(memoryStorage({ "laser-forge-conn-mode": "network", "laser-forge-host": "<device-ip>", "laser-forge-telnet-port": "2323" }));
  const view = connectionView(forge);
  assert.equal(view.ready, true);
  assert.deepEqual(view.payload, { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: 2323 });

  const serial = connectionView(savedConnectionForm(memoryStorage({ "laser-forge-conn-mode": "serial", "laser-forge-serial-port": "<port>", "laser-forge-baud": "115200" })));
  assert.deepEqual(serial.payload, { connection_mode: "serial", port: "<port>", baudrate: 115200 });

  const lab = connectionView(savedConnectionForm(memoryStorage({ "laserLab.connection_mode": "network", "laserLab.network_host": "<lab-ip>" })));
  assert.equal(lab.payload.network_host, "<lab-ip>");

  const none = connectionView(savedConnectionForm(memoryStorage({})));
  assert.equal(none.ready, false);
  assert.equal(none.payload, null);
  assert.match(none.label, /host\/IP/);

  const throwing = { getItem: () => { throw new Error("blocked"); } };
  assert.equal(connectionView(savedConnectionForm(throwing)).ready, false);
});

test("start needs the on-site check, a device target and no request in flight", () => {
  assert.equal(canStartAiPreview({ checked: true, busy: false, targetReady: true }), true);
  assert.equal(canStartAiPreview({ checked: false, busy: false, targetReady: true }), false);
  assert.equal(canStartAiPreview({ checked: true, busy: true, targetReady: true }), false);
  assert.equal(canStartAiPreview({ checked: true, busy: false, targetReady: false }), false);
});

test("strict mode hint says whether a spoken confirmation can start the job", () => {
  assert.match(strictHintText(true), /口头确认不会直接开刻/);
  assert.match(strictHintText(false), /确认开刻/);
  assert.match(strictHintText(null), /同意开刻/);
});

test("preview notes explain auto-fit and dithering", () => {
  assert.deepEqual(aiPreviewNotes({ artifacts: { auto_fitted: true, fit_note: "已等比缩小到 98.0 × 24.5 mm", image_mode: "dither" } }), [
    "已等比缩小到 98.0 × 24.5 mm",
    "照片按灰度抖动雕刻",
  ]);
  assert.deepEqual(aiPreviewNotes({ artifacts: { image_mode: "threshold" } }), []);
  assert.deepEqual(aiPreviewNotes(null), []);
});

test("origin note, fill label and head-relative framing request", () => {
  const record = {
    artifacts: {
      text_fill: "fill",
      origin_note: "开刻前把激光头移到木牌中心",
      frame_mm: { min_x: -30.1234, max_x: 30.1234, min_y: -8, max_y: 8 },
    },
  };
  assert.equal(originNote(record), "开刻前把激光头移到木牌中心");
  assert.equal(originNote({ artifacts: {} }), "");
  assert.deepEqual(aiPreviewNotes(record), ["刻法：扫描填充"]);
  const target = { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: 23 };
  assert.deepEqual(framingRequest(record, target), {
    ...target,
    relative_to_head: true,
    min_x: -30.123,
    max_x: 30.123,
    min_y: -8,
    max_y: 8,
    feed_rate: 2000,
    power: 0,
  });
  assert.equal(framingRequest(record, null), null, "没有设备目标不走边框");
  assert.equal(framingRequest({ artifacts: {} }, target), null, "没有范围不猜");
  assert.equal(framingRequest({ artifacts: { frame_mm: { min_x: 0, max_x: 0, min_y: 0, max_y: 5 } } }, target), null);
});
