import test from "node:test";
import assert from "node:assert/strict";
import {
  drawConnectionPayload,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  framingConfirmText,
  normalizeDrawMode,
  readDrawDefaultForm,
} from "../src/drawConnection.js";

test("画板连接：网口与串口各自只带本方式的字段", () => {
  const network = { connection_mode: "network", network_host: " laser.local ", network_telnet_port: "23", port: "COM3", baudrate: "115200" };
  assert.deepEqual(drawConnectionPayload(network), { connection_mode: "network", network_host: "laser.local", network_telnet_port: 23 });
  assert.equal(drawTargetReady(network), true);
  assert.equal(drawTargetLabel(network), "laser.local:23");

  const serial = { ...network, connection_mode: "serial", network_host: "" };
  assert.deepEqual(drawConnectionPayload(serial), { connection_mode: "serial", port: "COM3", baudrate: 115200 });
  assert.equal(drawTargetReady(serial), true);
  assert.match(drawTargetLabel(serial), /串口 COM3/);
  assert.equal(drawTargetReady({ ...serial, port: " " }), false);
  assert.equal(drawTargetMissingText(serial), "请先选择串口");
  assert.equal(drawTargetReady({ connection_mode: "network", network_host: "" }), false);
});

test("画板尺寸：留空为自动（null），不再用 1 mm 兜底；旧的 auto 模式按光栅处理", () => {
  assert.equal(drawSizeValue(""), null);
  assert.equal(drawSizeValue("  "), null);
  assert.equal(drawSizeValue("0"), null);
  assert.equal(drawSizeValue("-5"), null);
  assert.equal(drawSizeValue("50"), 50);
  assert.equal(drawSizeValue("12.5"), 12.5);
  assert.equal(normalizeDrawMode("auto"), "raster");
  assert.equal(normalizeDrawMode(null), "raster");
  assert.equal(normalizeDrawMode("outline"), "outline");
});

test("画板默认表单：存储被禁用（读取抛错）时逐项退回默认值，不让页面加载失败", () => {
  const blocked = { getItem() { throw new Error("SecurityError: storage disabled"); } };
  const form = readDrawDefaultForm(blocked);
  assert.equal(form.material, "椴木");
  assert.equal(form.thickness_mm, "3");
  assert.equal(form.connection_mode, "network");
  assert.equal(form.network_telnet_port, "23");
  assert.equal(form.baudrate, "115200");
  assert.equal(form.mode, "raster");
  assert.equal(form.output_format, "gcode");
  assert.deepEqual(readDrawDefaultForm(null), form);

  const saved = { getItem: (key) => ({ "laserLab.material": "亚克力", "laserLab.connection_mode": "serial", "laserLab.port": "COM7", "laserLab.mode": "outline" })[key] ?? null };
  const restored = readDrawDefaultForm(saved);
  assert.equal(restored.material, "亚克力");
  assert.equal(restored.connection_mode, "serial");
  assert.equal(restored.port, "COM7");
  assert.equal(restored.mode, "outline");
});

test("走边框确认文案：说明机器会移动；勾选弱光时明确会低功率出光", () => {
  const form = { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: "23" };
  const dark = framingConfirmText(form, 80, 50, false);
  assert.match(dark, /机器会实际移动/);
  assert.match(dark, /80\.0 × 50\.0 mm/);
  assert.match(dark, /激光保持关闭/);
  assert.doesNotMatch(dark, /出光/);
  const lit = framingConfirmText(form, 80, 50, true);
  assert.match(lit, /机器会实际移动/);
  assert.match(lit, /低功率出光/);
  assert.match(lit, /<device-ip>:23/);
});
