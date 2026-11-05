import test from "node:test";
import assert from "node:assert/strict";
import {
  parseGcodeWords,
  parseGcodeToolpath,
  resolvePoseAtLine,
  resolvePoseAtProgress,
  parseGrblStatusReport,
  resolveMachineWorkspace,
  resolveTwinHeadPose,
  YISU_V1_PROFILE,
  normalizeWorkspaceLimits,
  buildTwinConnectionPayload,
  buildTwinEmergencyStopPayload,
} from "../src/twinState.js";

test("parseGcodeWords 忽略注释与系统指令并提取模态参数", () => {
  assert.deepEqual(parseGcodeWords("; header comment"), []);
  assert.deepEqual(parseGcodeWords("$H"), []);
  assert.deepEqual(parseGcodeWords("G1 X12.5 Y-3.2 F1200 S800 (cut line)"), [
    { letter: "G", value: 1 },
    { letter: "X", value: 12.5 },
    { letter: "Y", value: -3.2 },
    { letter: "F", value: 1200 },
    { letter: "S", value: 800 },
  ]);
});

test("parseGcodeToolpath 正确区分 G0 空移与 G1 出光段并计算包络与行索引", () => {
  const sampleGcode = [
    "; MOSS laser job",
    "G90",
    "M5 S0",
    "G0 X10 Y20",
    "M3 S600",
    "G1 X50 Y20 F1200",
    "G1 X50 Y45 S900",
    "M5",
    "G0 X0 Y0",
  ].join("\n");

  const tp = parseGcodeToolpath(sampleGcode, { powerScale: 1000 });
  assert.equal(tp.totalLines, 9);
  assert.equal(tp.segments.length, 4);
  assert.equal(tp.laserLineCount, 2);

  // 第 1 段：G0 空移 (0,0) -> (10,20)，不出光
  assert.equal(tp.segments[0].mode, "G0");
  assert.equal(tp.segments[0].laserOn, false);
  assert.deepEqual(tp.segments[0].to, { x: 10, y: 20, z: 0 });

  // 第 2 段：G1 雕刻 (10,20) -> (50,20)，S=600 出光
  assert.equal(tp.segments[1].mode, "G1");
  assert.equal(tp.segments[1].laserOn, true);
  assert.equal(tp.segments[1].power, 600);
  assert.equal(tp.segments[1].powerRatio, 0.6);
  assert.equal(tp.segments[1].lengthMm, 40);

  // 第 3 段：G1 雕刻 (50,20) -> (50,45)，S=900 出光
  assert.equal(tp.segments[2].laserOn, true);
  assert.equal(tp.segments[2].powerRatio, 0.9);
  assert.equal(tp.segments[2].lengthMm, 25);

  // 第 4 段：M5 后 G0 回原点，不出光
  assert.equal(tp.segments[3].laserOn, false);
  assert.deepEqual(tp.segments[3].to, { x: 0, y: 0, z: 0 });

  assert.equal(tp.bounds.minX, 0);
  assert.equal(tp.bounds.maxX, 50);
  assert.equal(tp.bounds.minY, 0);
  assert.equal(tp.bounds.maxY, 45);
  assert.equal(tp.cutLengthMm, 65);

  // 按行号定位（模拟 JobWorker lines_sent = 6，即第 6 行 G1 X50 Y20 执行完毕）
  const poseAt6 = resolvePoseAtLine(tp, 6);
  assert.equal(poseAt6.x, 50);
  assert.equal(poseAt6.y, 20);
  assert.equal(poseAt6.laserOn, true);
  assert.equal(poseAt6.completedSegmentCount, 2);

  // 验证 G-code 真实雕刻时长推算：
  // 第 2 段 40mm @ F1200 (20mm/s) = 2.0s；第 3 段 25mm @ F1200 (20mm/s) = 1.25s => cutSeconds = 3.25s
  assert.equal(Number(tp.cutSeconds.toFixed(2)), 3.25);
  assert.equal(Number((tp.cutSeconds + tp.rapidSeconds).toFixed(4)), Number(tp.estimatedSeconds.toFixed(4)));

  // 按连续时间进度插值定位（严格 1:1 对应物理雕刻时长）
  const poseHalf = resolvePoseAtProgress(tp, 0.5);
  assert.equal(poseHalf.x > 0, true);
  assert.equal(poseHalf.progressRatio, 0.5);
  assert.equal(Number(poseHalf.elapsedSeconds.toFixed(4)), Number((tp.estimatedSeconds * 0.5).toFixed(4)));
  assert.equal(
    Number((poseHalf.elapsedSeconds + poseHalf.remainingSeconds).toFixed(4)),
    Number(tp.estimatedSeconds.toFixed(4)),
  );
});

test("parseGcodeToolpath 支持 G91 相对坐标与 G92 坐标偏置", () => {
  const gcode = [
    "G90",
    "G0 X10 Y10",
    "G91",
    "M3 S500",
    "G1 X5 Y-5 F600",
    "G92 X0 Y0",
    "G90",
    "G1 X20 Y10",
  ].join("\n");

  const tp = parseGcodeToolpath(gcode);
  assert.equal(tp.segments.length, 3);
  assert.deepEqual(tp.segments[1].to, { x: 15, y: 5, z: 0 });
  assert.deepEqual(tp.segments[2].from, { x: 0, y: 0, z: 0 });
  assert.deepEqual(tp.segments[2].to, { x: 20, y: 10, z: 0 });
});

test("parseGrblStatusReport 解析 MPos、WCO 换算 WPos、缓冲 Bf、转速功率 FS 与阻塞引脚 Pn", () => {
  const raw = "<Run|MPos:42.500,18.250,0.000|Bf:14,120|FS:1500,850|Pn:XD|WCO:2.500,8.250,0.000|Ov:100,100,100>";
  const parsed = parseGrblStatusReport(raw);
  assert.equal(parsed.online, true);
  assert.equal(parsed.baseState, "Run");
  assert.deepEqual(parsed.mpos, { x: 42.5, y: 18.25, z: 0 });
  assert.deepEqual(parsed.wpos, { x: 40, y: 10, z: 0 });
  assert.deepEqual(parsed.bf, { plannerFree: 14, rxBufferFree: 120 });
  assert.deepEqual(parsed.fs, { feedRate: 1500, spindlePower: 850 });
  assert.equal(parsed.pins.hasBlockingPin, true);
  assert.equal(parsed.pins.active.length, 2);

  const offline = parseGrblStatusReport("");
  assert.equal(offline.online, false);
  assert.equal(offline.state, "Offline");
});

test("resolveMachineWorkspace 与 resolveTwinHeadPose 正确处理翼宿 V1.0 默认行程 (100×100mm)、1:1 刀路尺寸同步、自定义外框与材料内文字位置钳制", () => {
  const defaultWs = resolveMachineWorkspace(null, { widthMm: 80, heightMm: 60, maxX: 80, maxY: 60 }, null);
  assert.equal(defaultWs.xTravelMm, 100);
  assert.equal(defaultWs.yTravelMm, 100);
  assert.equal(defaultWs.zTravelMm, 20);
  assert.equal(defaultWs.hasHardwareLimits, false);
  assert.equal(defaultWs.modelName, YISU_V1_PROFILE.modelName);
  // 默认 1:1 同步 G-code 刀路包络尺寸，不额外膨胀
  assert.equal(defaultWs.material.widthMm, 80);
  assert.equal(defaultWs.material.heightMm, 60);
  assert.equal(defaultWs.offsetX, 0);
  assert.equal(defaultWs.offsetY, 0);
  assert.equal(defaultWs.textOffsetX, 0);
  assert.equal(defaultWs.textOffsetY, 0);

  // 自定义材料外框尺寸、台面起点坐标 (X0, Y0) 与雕刻文字在材料内位置 (textOffsetX, textOffsetY)
  const customWs = resolveMachineWorkspace(
    { x_travel_mm: 100, y_travel_mm: 100, z_travel_mm: 20, from_hardware: true },
    { widthMm: 50, heightMm: 40, maxX: 50, maxY: 40 },
    { material: "亚克力", thickness_mm: 5, width_mm: 50, height_mm: 40 },
    { autoSize: false, widthMm: 70, heightMm: 60, offsetX: 15, offsetY: 20, textOffsetX: 10, textOffsetY: 12 },
  );
  assert.equal(customWs.hasHardwareLimits, true);
  assert.equal(customWs.xTravelMm, 100);
  assert.equal(customWs.yTravelMm, 100);
  assert.equal(customWs.material.name, "亚克力");
  assert.equal(customWs.material.thicknessMm, 5);
  assert.equal(customWs.material.widthMm, 70);
  assert.equal(customWs.material.heightMm, 60);
  assert.equal(customWs.offsetX, 15);
  assert.equal(customWs.offsetY, 20);
  assert.equal(customWs.material.maxTextOffsetX, 20); // 70 - 50
  assert.equal(customWs.material.maxTextOffsetY, 20); // 60 - 40
  assert.equal(customWs.textOffsetX, 10);
  assert.equal(customWs.textOffsetY, 12);
  assert.equal(customWs.toolpathOffsetX, 25); // 15 + 10
  assert.equal(customWs.toolpathOffsetY, 32); // 20 + 12

  // 边界钳制：无论怎么调 textOffsetX/Y 或缩小材料宽高，雕刻文字都绝不超出材料外框
  const clampedWs = resolveMachineWorkspace(
    { x_travel_mm: 100, y_travel_mm: 100, z_travel_mm: 20 },
    { minX: 0, maxX: 40, minY: 0, maxY: 15, widthMm: 40, heightMm: 15 },
    null,
    { autoSize: false, widthMm: 80, heightMm: 60, offsetX: 10, offsetY: 10, textOffsetX: 999, textOffsetY: -50 },
  );
  assert.equal(clampedWs.material.maxTextOffsetX, 40); // 80 - 40
  assert.equal(clampedWs.material.maxTextOffsetY, 45); // 60 - 15
  assert.equal(clampedWs.textOffsetX, 40); // 999 被钳制在材料右边界 40
  assert.equal(clampedWs.textOffsetY, 0); // -50 被钳制在材料下边界 0
  assert.equal(clampedWs.toolpathOffsetX, 50); // 10 + 40，文字右边缘恰好 = 50 + 40 = 90 = 材料右边缘 (10 + 80)
  assert.equal(clampedWs.toolpathOffsetY, 10);

  // 自由调节材料宽高：即使将材料宽高调得比原始 G-code 刀路包络小，也能自由生效并自动计算刀路跟随缩放比 toolpathScaleX/Y
  const shrunkMatWs = resolveMachineWorkspace(
    { x_travel_mm: 100, y_travel_mm: 100, z_travel_mm: 20 },
    { minX: 0, maxX: 45, minY: 0, maxY: 20, widthMm: 45, heightMm: 20 },
    null,
    { autoSize: false, widthMm: 20, heightMm: 10, textOffsetX: 30, textOffsetY: 30 },
  );
  assert.equal(shrunkMatWs.material.widthMm, 20);
  assert.equal(shrunkMatWs.material.heightMm, 10);
  assert.equal(shrunkMatWs.toolpathScaleX, Number((20 / 45).toFixed(4)));
  assert.equal(shrunkMatWs.toolpathScaleY, 0.5);
  assert.equal(shrunkMatWs.textOffsetX, 0);
  assert.equal(shrunkMatWs.textOffsetY, 0);

  const simPose = { x: 12, y: 24, z: 0, laserOn: true, power: 700, powerRatio: 0.7, feedRate: 1200 };
  const telemetry = parseGrblStatusReport("<Idle|MPos:5.000,6.000,0.000|FS:0,0>");

  const fromSim = resolveTwinHeadPose({ driveMode: "sim", simPose, telemetry });
  assert.equal(fromSim.x, 12);
  assert.equal(fromSim.isLiveHardware, false);

  const fromTel = resolveTwinHeadPose({ driveMode: "telemetry", simPose, telemetry });
  assert.equal(fromTel.x, 5);
  assert.equal(fromTel.y, 6);
  assert.equal(fromTel.laserOn, false);
  assert.equal(fromTel.isLiveHardware, true);
});

test("数字孪生：实机行程上限支持 200/300/400mm 至 2000mm，默认值保持 100×100×20mm", () => {
  const defaults = normalizeWorkspaceLimits({});
  assert.equal(defaults.x_travel_mm, 100);
  assert.equal(defaults.y_travel_mm, 100);
  assert.equal(defaults.z_travel_mm, 20);

  const grbl400 = normalizeWorkspaceLimits({ x_travel_mm: 400, y_travel_mm: 300, z_travel_mm: 80, from_hardware: true });
  assert.equal(grbl400.x_travel_mm, 400);
  assert.equal(grbl400.y_travel_mm, 300);
  assert.equal(grbl400.z_travel_mm, 80);
  assert.equal(grbl400.from_hardware, true);

  const upperClamped = normalizeWorkspaceLimits({ x_travel_mm: 5000, y_travel_mm: 3000, z_travel_mm: 999 });
  assert.equal(upperClamped.x_travel_mm, 2000);
  assert.equal(upperClamped.y_travel_mm, 2000);
  assert.equal(upperClamped.z_travel_mm, 500);
});

test("数字孪生：连接参数按模式隔离，页面内急停在非运行态不误带已完成 workflow job_id", () => {
  const serialConn = buildTwinConnectionPayload({
    mode: "serial",
    port: "COM5",
    baudrate: 115200,
    networkHost: "192.168.1.50",
    telnetPort: 23,
  });
  assert.deepEqual(serialConn, {
    connection_mode: "serial",
    port: "COM5",
    baudrate: 115200,
  });
  assert.equal("host" in serialConn, false);
  assert.equal("network_host" in serialConn, false);

  const netConn = buildTwinConnectionPayload({
    mode: "network",
    port: "COM5",
    baudrate: 115200,
    networkHost: "192.168.1.50",
    telnetPort: 2323,
  });
  assert.deepEqual(netConn, {
    connection_mode: "network",
    network_host: "192.168.1.50",
    host: "192.168.1.50",
    network_telnet_port: 2323,
    telnet_port: 2323,
  });
  assert.equal("port" in netConn, false);

  // 运行中任务：只带 job_id，不混入冲突连接字段
  assert.deepEqual(
    buildTwinEmergencyStopPayload({
      isJobRunning: true,
      selectedWorkflowId: "wf-live-1",
      connectionPayload: netConn,
    }),
    { action: "emergency_stop", confirmed: true, job_id: "wf-live-1" },
  );

  // 已完成或仅预览任务 (!isJobRunning)：绝不带旧 job_id，直接走当前控制器连接急停
  const directStop = buildTwinEmergencyStopPayload({
    isJobRunning: false,
    selectedWorkflowId: "wf-completed-old",
    connectionPayload: serialConn,
  });
  assert.equal("job_id" in directStop, false);
  assert.deepEqual(directStop, {
    connection_mode: "serial",
    port: "COM5",
    baudrate: 115200,
    action: "emergency_stop",
    confirmed: true,
  });
});
