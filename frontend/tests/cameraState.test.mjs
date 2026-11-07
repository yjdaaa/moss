import test from "node:test";
import assert from "node:assert/strict";
import {
  cameraDevicesView,
  cameraFrameSourceView,
  cameraTransformValue,
  shouldAutoCaptureNext,
} from "../src/cameraState.js";

test("cameraDevicesView 按在线与拍照能力排序，并跳过未允许设备", () => {
  const view = cameraDevicesView({
    running: true,
    mode: "chat",
    http_port: 8003,
    ws_port: 8000,
    devices: [
      { device_id: "pending-01", status: "pending", name: "新设备" },
      { device_id: "revoked-01", status: "revoked", name: "已撤销设备" },
      { device_id: "offline-01", status: "allowed", name: "卧室小智", last_seen_at: 100 },
      { device_id: "no-cam-01", status: "allowed", name: "纯语音小智", last_seen_at: 200 },
      { device_id: "cam-01", status: "allowed", name: "实战派S3", model: "lichuang-dev", last_seen_at: 150 },
    ],
    sessions: [
      {
        device_id: "no-cam-01",
        ip: "192.168.1.21",
        state: "idle",
        device_tools: ["self.get_device_status"],
      },
      {
        device_id: "cam-01",
        ip: "192.168.1.22",
        state: "idle",
        device_tools: ["self.get_device_status", "self.camera.take_photo"],
      },
    ],
  });

  assert.equal(view.running, true);
  assert.equal(view.pendingCount, 1);
  assert.deepEqual(
    view.devices.map((d) => d.deviceId),
    ["cam-01", "no-cam-01", "offline-01"],
  );
  assert.equal(view.defaultDeviceId, "cam-01");
  assert.equal(view.devices[0].canCapture, true);
  assert.equal(view.devices[1].canCapture, false);
  assert.match(view.devices[1].disabledReason, /self\.camera\.take_photo/);
  assert.equal(view.devices[2].canCapture, false);
  assert.match(view.devices[2].disabledReason, /离线/);
});

test("shouldAutoCaptureNext 在等待回传或小智说话期间自动暂缓，空闲时恢复", () => {
  const camDevice = {
    deviceId: "cam-01",
    canCapture: true,
    sessionState: "idle",
    disabledReason: "",
  };

  assert.deepEqual(
    shouldAutoCaptureNext({
      autoEnabled: false,
      device: camDevice,
      frameView: { online: true, pending: false, session_state: "idle" },
    }),
    { allow: false, pausedReason: "" },
  );

  assert.deepEqual(
    shouldAutoCaptureNext({
      autoEnabled: true,
      device: camDevice,
      frameView: { online: true, pending: false, session_state: "idle" },
    }),
    { allow: true, pausedReason: "" },
  );

  const pendingGate = shouldAutoCaptureNext({
    autoEnabled: true,
    device: camDevice,
    frameView: { online: true, pending: true, session_state: "idle" },
  });
  assert.equal(pendingGate.allow, false);
  assert.match(pendingGate.pausedReason, /上一帧/);

  for (const busyState of ["listening", "thinking", "speaking"]) {
    const busyGate = shouldAutoCaptureNext({
      autoEnabled: true,
      device: camDevice,
      frameView: { online: true, pending: false, session_state: busyState },
    });
    assert.equal(busyGate.allow, false, `${busyState} 期间必须让路`);
    assert.match(busyGate.pausedReason, /自动刷新已暂缓/);
  }

  const errGate = shouldAutoCaptureNext({
    autoEnabled: true,
    device: camDevice,
    frameView: { online: true, pending: false, last_error: "等待设备回传画面超时", session_state: "idle" },
  });
  assert.equal(errGate.allow, false, "最近抓拍报错时必须暂停自动刷新，禁止死循环重试");
  assert.match(errGate.pausedReason, /暂停/);
});

test("cameraTransformValue 与 cameraFrameSourceView 输出准确", () => {
  assert.equal(cameraTransformValue({ mirror: false, flip180: false }), "");
  assert.equal(cameraTransformValue({ mirror: true, flip180: false }), "scaleX(-1)");
  assert.equal(cameraTransformValue({ mirror: false, flip180: true }), "rotate(180deg)");
  assert.equal(cameraTransformValue({ mirror: true, flip180: true }), "scaleX(-1) rotate(180deg)");

  assert.deepEqual(cameraFrameSourceView(null), { label: "暂无画面", tone: "off" });
  assert.deepEqual(cameraFrameSourceView({ source: "preview" }), { label: "桌面抓拍", tone: "ok" });
  assert.deepEqual(cameraFrameSourceView({ source: "voice" }), { label: "语音拍照问答", tone: "info" });
});
