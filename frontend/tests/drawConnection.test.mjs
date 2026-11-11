import test from "node:test";
import assert from "node:assert/strict";
import {
  buildDrawFramingPayload,
  computeSceneRevision,
  drawConnectionPayload,
  drawGenerationSignature,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  framingConfirmText,
  isDrawConnectionOk,
  isDrawPreviewStale,
  normalizeDrawMode,
  parseImportedSceneJson,
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

test("画板默认表单：存储被禁用时退回默认值；未存画板设备时自动复用工作台连接", () => {
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

  // 画板尚未存设备目标时，自动带入工作台已填好的网络或串口连接
  const fromWorkbench = {
    getItem: (key) => ({
      "laser-forge-conn-mode": "network",
      "laser-forge-host": "192.0.2.88",
      "laser-forge-telnet-port": "2323",
    })[key] ?? null,
  };
  const shared = readDrawDefaultForm(fromWorkbench);
  assert.equal(shared.connection_mode, "network");
  assert.equal(shared.network_host, "192.0.2.88");
  assert.equal(shared.network_telnet_port, "2323");
});

test("走边框确认文案与载荷：统一附带 relative_to_head 与正式雕刻同起点；串口连接判断看 serial_connected", () => {
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

  const payload = buildDrawFramingPayload(form, 80, 50, false);
  assert.equal(payload.relative_to_head, true, "画板走边框必须与 G92 X0 Y0 正式雕刻共用当前激光头起点");
  assert.equal(payload.power, 0);
  assert.equal(buildDrawFramingPayload(form, 80, 50, true).power, 1);

  const serialForm = { connection_mode: "serial", port: "COM5", baudrate: "115200" };
  assert.equal(
    isDrawConnectionOk(serialForm, { success: true, result: { serial_connected: true, network_connected: false } }),
    true,
    "串口握手成功必须判为连通",
  );
  assert.equal(
    isDrawConnectionOk(form, { success: true, result: { serial_connected: true, network_connected: false } }),
    false,
  );
});

test("画板预览过期检测与场景 JSON 导入校验", () => {
  const rev1 = computeSceneRevision([{ id: "rect-1", version: 2, versionNonce: 10, isDeleted: false }]);
  const revDeleted = computeSceneRevision([{ id: "rect-1", version: 3, versionNonce: 11, isDeleted: true }]);
  assert.equal(revDeleted, "empty");
  const baseForm = { material: "椴木", thickness_mm: "3", width_mm: "", height_mm: "", mode: "raster", vector_simplify_factor: "3" };
  const sig1 = drawGenerationSignature(baseForm, { enabled: false });
  const snap = { sceneRev: rev1, formSig: sig1 };
  assert.equal(isDrawPreviewStale(null, snap), false);
  assert.equal(isDrawPreviewStale(snap, { sceneRev: rev1, formSig: sig1 }), false);
  assert.equal(
    isDrawPreviewStale(snap, { sceneRev: computeSceneRevision([{ id: "rect-1", version: 3, versionNonce: 12 }]), formSig: sig1 }),
    true,
    "画布元素版本变化立即标过期",
  );
  assert.equal(
    isDrawPreviewStale(snap, { sceneRev: rev1, formSig: drawGenerationSignature({ ...baseForm, width_mm: "60" }, { enabled: false }) }),
    true,
    "改尺寸立即标过期",
  );
  assert.equal(
    isDrawPreviewStale(snap, { sceneRev: rev1, formSig: drawGenerationSignature(baseForm, { enabled: true, power: "55", feed: "1200", passes: "1" }) }),
    true,
    "开启临时自定义参数立即标过期",
  );

  assert.match(parseImportedSceneJson("not-json").error, /合法的 JSON/);
  assert.match(parseImportedSceneJson(JSON.stringify({ type: "other" })).error, /elements/);
  const ok = parseImportedSceneJson(JSON.stringify({
    type: "excalidraw",
    elements: [{ id: "a", type: "rectangle" }],
    appState: { gridSize: 20, viewBackgroundColor: "#ffffff", secret: "ignored" },
  }));
  assert.equal(ok.error, undefined);
  assert.equal(ok.elements.length, 1);
  assert.deepEqual(ok.appState, { gridSize: 20, viewBackgroundColor: "#ffffff" });
});
