import test from "node:test";
import assert from "node:assert/strict";
import {
  drawConnectionPayload,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  normalizeDrawMode,
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
