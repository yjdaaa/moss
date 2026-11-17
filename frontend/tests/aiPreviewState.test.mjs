import assert from "node:assert/strict";
import test from "node:test";
import {
  aiOriginLabel,
  aiPreviewNotes,
  aiWorkflowStarted,
  canStartAiPreview,
  connectionView,
  framingOutline,
  framingRequest,
  framingShapeText,
  isAiOrigin,
  FRAMING_SHAPE_KEY,
  readFramingShape,
  writeFramingShape,
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
  assert.deepEqual(aiPreviewNotes({ artifacts: { image_mode: "grayscale", negative: true } }), [
    "灰度变功率雕刻：越黑功率越高",
    "已反相：原图浅色处出光",
  ]);
  assert.deepEqual(aiPreviewNotes({ artifacts: { image_mode: "jarvis", negative: false } }), []);
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
  // 2026-11-17（r45）：语音文字任务按雕刻路线全局开关走，转图片与后端改动说明都要让用户看到
  assert.deepEqual(
    aiPreviewNotes({ artifacts: { text_fill: "fill", engrave_route: "image", route_note: "转图片时文字一律实心扫描（描边、排线只在矢量下可选）" } }),
    ["刻法：扫描填充", "雕刻路线：转图片（文字渲染成位图逐行扫描）", "转图片时文字一律实心扫描（描边、排线只在矢量下可选）"],
  );
  assert.deepEqual(
    aiPreviewNotes({ artifacts: { text_fill: "outline", engrave_route: "vector", route_note: "外框切割只能走矢量，本次按矢量生成" } }),
    ["刻法：只描轮廓", "外框切割只能走矢量，本次按矢量生成"],
  );
  assert.deepEqual(aiPreviewNotes({ artifacts: { text_fill: "fill", engrave_route: "vector", route_note: null } }), ["刻法：扫描填充"]);
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

test("橡皮筋走边框：选了沿外轮廓且预览给了轮廓才带 outline，否则仍走外接矩形", () => {
  const target = { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: 23 };
  const artifacts = {
    frame_mm: { min_x: -10, max_x: 10, min_y: -10, max_y: 5 },
    frame_hull_mm: [[-10, -10], [10, -10], [0.12345, 5]],
  };
  const hull = framingRequest({ artifacts }, target, { shape: "hull" });
  assert.deepEqual(hull.outline, [[-10, -10], [10, -10], [0.123, 5]]);
  assert.equal(framingShapeText(hull), "沿图形外轮廓（3 个点）");
  const box = framingRequest({ artifacts }, target);
  assert.equal(box.outline, undefined, "默认外接矩形");
  assert.equal(framingShapeText(box), "沿 20 × 15 mm 的外接矩形");
  // 没有轮廓（含圆弧的外部刀路）时退回矩形
  const noHull = framingRequest({ artifacts: { ...artifacts, frame_hull_mm: null } }, target, { shape: "hull" });
  assert.equal(noHull.outline, undefined);
  assert.equal(framingOutline({ frame_hull_mm: [[0, 0], [1, "x"], [2, 2]] }), null);
  assert.equal(framingOutline({ frame_hull_mm: [[0, 0], [1, 1]] }), null);

  const data = {};
  const store = { getItem: (k) => data[k] ?? null, setItem: (k, v) => { data[k] = v; } };
  assert.equal(readFramingShape(store), "box");
  writeFramingShape("hull", store);
  assert.equal(data[FRAMING_SHAPE_KEY], "hull");
  assert.equal(readFramingShape(store), "hull");
  assert.equal(readFramingShape({ getItem() { throw new Error("denied"); } }), "box");
});
