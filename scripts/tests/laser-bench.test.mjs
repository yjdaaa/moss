import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  checkEstopRealtimeNotQueued,
  checkG90RestoredAfterRelativeMove,
  checkGcodeSha256MatchesPreview,
  checkHomingDisabledWhenDollar22Zero,
  checkLaserOffAfterTerminal,
  checkLaserOffBeforeMoveOrFraming,
  checkLaserOnWithinFirmwareTravel,
  checkOverrideOnlyDuringActiveJobSameChannel,
  checkResumeStartLineAndNoDuplicateBurn,
  checkSpindleSWithinDollar30,
  checkTimeEstimateDeviation,
  checkWarningsConsistentWithReality,
} from "../laser-bench/invariants.mjs";
import {
  ReadOnlyWhitelistError,
  analyzeReadOnlySnapshot,
  assertReadOnlyRequest,
  createReadOnlyClient,
  runReadOnlyCheck,
} from "../laser-bench/laser-readonly-check.mjs";
import { checkDryRunStatusReports, runDryRunBench } from "../laser-bench/laser-dryrun-bench.mjs";
import { resolveHostBinary } from "../laser-bench/bench-common.mjs";
import { buildSimReportMarkdown, computeExitCode, runLaserSimBench } from "../laser-bench/laser-sim-bench.mjs";
import { startGrblSimulator } from "../laser-bench/sim-grbl.mjs";

function connectTcp(port) {
  return new Promise((resolvePromise, reject) => {
    const socket = net.createConnection({ host: "127.0.0.1", port }, () => {
      let buf = "";
      const waiters = [];
      socket.on("data", (chunk) => {
        buf += chunk.toString("utf8");
        while (waiters.length > 0) {
          const idx = buf.indexOf("\n");
          if (idx < 0) break;
          const line = buf.slice(0, idx).replace(/\r$/, "");
          buf = buf.slice(idx + 1);
          waiters.shift().resolve(line);
        }
      });
      const readLine = (timeoutMs = 2000) =>
        new Promise((res, rej) => {
          const idx = buf.indexOf("\n");
          if (idx >= 0) {
            const line = buf.slice(0, idx).replace(/\r$/, "");
            buf = buf.slice(idx + 1);
            res(line);
            return;
          }
          const timer = setTimeout(() => {
            const pos = waiters.findIndex((w) => w.resolve === wrappedResolve);
            if (pos >= 0) waiters.splice(pos, 1);
            rej(new Error(`读取超时 (${timeoutMs}ms)`));
          }, timeoutMs);
          const wrappedResolve = (val) => {
            clearTimeout(timer);
            res(val);
          };
          waiters.push({ resolve: wrappedResolve });
        });
      const readUntil = async (predicate, timeoutMs = 2000) => {
        const collected = [];
        const deadline = Date.now() + timeoutMs;
        while (Date.now() < deadline) {
          const line = await readLine(Math.max(50, deadline - Date.now()));
          collected.push(line);
          if (predicate(line)) return collected;
        }
        throw new Error(`未等到目标行，已收到：${collected.join(" | ")}`);
      };
      resolvePromise({ socket, readLine, readUntil });
    });
    socket.once("error", reject);
  });
}

// ---------------------------------------------------------------------------
// 1. 模拟器实时字节、画像、机械坐标推进与字节日志
// ---------------------------------------------------------------------------

test("模拟器：实时字节 (? / ! / ~ / 0x18 / 0x90–0x9B) 即时生效、坐标推进与日志记录完整", async () => {
  const sim = await startGrblSimulator({
    profile: "fluidnc",
    initialMpos: [10, 20, 0],
    ackDelayMs: 60,
    maxSpindleSpeed: 500,
  });
  let client;
  try {
    client = await connectTcp(sim.port);
    // $$ / $CD / $I 画像检查
    client.socket.write("$$\n");
    const settings = await client.readUntil((l) => l === "ok");
    assert.ok(settings.includes("$30=500"));
    assert.ok(settings.includes("$32=1"));

    client.socket.write("$CD\n");
    const cd = await client.readUntil((l) => l === "ok");
    assert.ok(cd.some((l) => l.includes("use_line_numbers: true")));

    client.socket.write("$I\n");
    const idLines = await client.readUntil((l) => l === "ok");
    assert.ok(idLines.some((l) => l.includes("FluidNC")));

    // 倍率实时字节（0x91 +10% 进给, 0x9B -10% 功率）+ ? 查询
    client.socket.write(Buffer.from([0x91, 0x9b, 0x3f]));
    const ovStatus = await client.readLine();
    assert.match(ovStatus, /Ov:110,100,90/);
    assert.match(ovStatus, /MPos:10\.000,20\.000,0\.000/);

    // 推进坐标 + 排队期间 ! 保持与 ~ 恢复
    client.socket.write("G90\nG92 X0 Y0\nM4 S200\nG1 X5 Y10 F1200\n");
    // 在 60ms ack 延迟期间立即发 ! 和 ?，不得被队列阻塞
    const t0 = Date.now();
    client.socket.write(Buffer.from([0x21, 0x3f]));
    const holdLine = await client.readUntil((l) => l.startsWith("<"));
    assert.ok(Date.now() - t0 < 300, "实时字节必须即时响应");
    assert.match(holdLine.at(-1), /^<Hold:0\|/);
    assert.deepEqual(sim.snapshot().machine.mpos, [15, 30, 0]);

    // 0x18 软复位重置倍率与激光并回横幅
    client.socket.write(Buffer.from([0x18]));
    const banner = await client.readUntil((l) => l.includes("FluidNC"));
    assert.ok(banner.length > 0);
    client.socket.write(Buffer.from([0x3f]));
    const afterReset = await client.readLine();
    assert.match(afterReset, /^<Idle\|.*Ov:100,100,100/);
    assert.equal(sim.snapshot().machine.laserState, "M5");
    assert.equal(sim.snapshot().machine.spindleS, 0);

    // 字节级时间戳日志
    const logText = sim.formatSerialLog();
    assert.match(logText, /RX \(\d+B\) hex=.*18/);
    assert.match(logText, /TX \(\d+B\)/);
  } finally {
    client?.socket?.destroy();
    await sim.close();
  }
});

test("模拟器：故障注入（ALARM:1 / ALARM:2 / Pn:D 门开 / 断连 / 不报行程 / $22=0 拒 $H）", async () => {
  // 1) ALARM:1
  const simAlarm = await startGrblSimulator({
    profile: "grbl",
    omitTravel: true,
    homingEnabled: false,
    ackDelayMs: 0,
    faults: { alarmAfterLines: 2, alarmCode: 1 },
  });
  let c1;
  try {
    c1 = await connectTcp(simAlarm.port);
    c1.socket.write("$$\n");
    const sets = await c1.readUntil((l) => l === "ok");
    assert.ok(!sets.some((l) => l.startsWith("$130=")), "omitTravel 须省略 $130");
    assert.ok(sets.includes("$22=0"));
    c1.socket.write("$CD\n");
    assert.equal(await c1.readLine(), "error:3");
    c1.socket.write("$H\n");
    assert.equal(await c1.readLine(), "error:5");
    assert.equal(simAlarm.snapshot().machine.homingAttemptedWhenDisabled, true);

    c1.socket.write("G0 X1\n");
    assert.equal(await c1.readLine(), "ok");
    c1.socket.write("G1 X2 S100 F1000\n");
    assert.equal(await c1.readLine(), "ALARM:1");
  } finally {
    c1?.socket?.destroy();
    await simAlarm.close();
  }

  // 2) 门开 Pn:D 与软限位 ALARM:2
  const simDoor = await startGrblSimulator({
    profile: "fluidnc",
    ackDelayMs: 0,
    faults: { doorOpenAfterLines: 1 },
  });
  let c2;
  try {
    c2 = await connectTcp(simDoor.port);
    c2.socket.write("G0 X5\n");
    const doorLine = await c2.readLine();
    assert.match(doorLine, /^<Door:1\|.*Pn:D/);
  } finally {
    c2?.socket?.destroy();
    await simDoor.close();
  }

  // 3) 中途断连
  const simDrop = await startGrblSimulator({
    profile: "fluidnc",
    ackDelayMs: 0,
    faults: { disconnectAfterLines: 1 },
  });
  let c3;
  try {
    c3 = await connectTcp(simDrop.port);
    const closed = new Promise((r) => c3.socket.once("close", () => r(true)));
    c3.socket.write("G0 X5\n");
    assert.equal(await closed, true, "disconnectAfterLines 须立即断开 TCP 连接");
  } finally {
    c3?.socket?.destroy();
    await simDrop.close();
  }
});

// ---------------------------------------------------------------------------
// 2. 全部 12 条不变量：每条都有一组正常用例和一组违反用例
// ---------------------------------------------------------------------------

test("不变量 1 (laser_off_before_move_or_framing): 正常与违反用例", () => {
  const pass = checkLaserOffBeforeMoveOrFraming({
    kind: "relative_move",
    lines: [{ line: "M5" }, { line: "G91" }, { line: "G0 X5 Y0 F1200" }, { line: "G90" }],
  });
  assert.equal(pass.status, "pass");

  const failNoM5 = checkLaserOffBeforeMoveOrFraming({
    kind: "framing",
    framingPower: 0,
    lines: [{ line: "G90" }, { line: "G1 X20 Y0 F2000" }],
  });
  assert.equal(failNoM5.status, "fail");
  assert.equal(failNoM5.severity, "safety");

  const failLit = checkLaserOffBeforeMoveOrFraming({
    kind: "absolute_move",
    lines: [{ line: "M5" }, { line: "M3 S200" }, { line: "G1 X0 Y0 F1000" }],
  });
  assert.equal(failLit.status, "fail");
});

test("不变量 2 (laser_off_after_terminal): 正常与违反用例", () => {
  const passCompleted = checkLaserOffAfterTerminal({
    terminalMode: "completed",
    lines: [{ line: "M4 S300" }, { line: "G1 X10 F1000" }, { line: "M5" }, { line: "G4 P0" }],
  });
  assert.equal(passCompleted.status, "pass");

  const failCompleted = checkLaserOffAfterTerminal({
    terminalMode: "completed",
    lines: [{ line: "M3 S500" }, { line: "G1 X10 F1000" }, { line: "G4 P0" }],
  });
  assert.equal(failCompleted.status, "fail");
  assert.equal(failCompleted.severity, "safety");

  const passEstop = checkLaserOffAfterTerminal({
    terminalMode: "estop",
    events: [
      { type: "line", connId: 1, line: "G1 X10 S300", tsMs: 100 },
      { type: "realtime", connId: 1, byte: 0x18, tsMs: 110 },
    ],
  });
  assert.equal(passEstop.status, "pass");

  const failLineAfterReset = checkLaserOffAfterTerminal({
    terminalMode: "cancelled",
    events: [
      { type: "realtime", connId: 1, byte: 0x18, tsMs: 100 },
      { type: "line", connId: 1, line: "G1 X20 S300", tsMs: 120 },
    ],
  });
  assert.equal(failLineAfterReset.status, "fail");
});

test("不变量 3 (g90_restored_after_relative_move): 正常与违反用例", () => {
  const pass = checkG90RestoredAfterRelativeMove({
    kind: "relative_move",
    lines: [{ line: "M5" }, { line: "G91" }, { line: "G0 X5 F1200" }, { line: "G90" }],
    events: [],
  });
  assert.equal(pass.status, "pass");

  const fail = checkG90RestoredAfterRelativeMove({
    kind: "relative_move",
    lines: [{ line: "M5" }, { line: "G91" }, { line: "G0 X5 F1200" }],
    events: [],
  });
  assert.equal(fail.status, "fail");
  assert.equal(fail.severity, "safety");
});

test("不变量 4 (spindle_s_within_dollar30): 正常与违反用例", () => {
  const pass = checkSpindleSWithinDollar30({
    profile: { maxSpindleSpeed: 255 },
    lines: [{ line: "M4 S255" }, { line: "G1 X10 S128 F1200" }, { line: "M5 S0" }],
  });
  assert.equal(pass.status, "pass");

  const fail = checkSpindleSWithinDollar30({
    profile: { maxSpindleSpeed: 255 },
    lines: [{ line: "M4 S500" }, { line: "G1 X10 F1200" }],
  });
  assert.equal(fail.status, "fail");
  assert.equal(fail.severity, "safety");
});

test("不变量 5 (laser_on_within_firmware_travel): 正常与违反用例", () => {
  const pass = checkLaserOnWithinFirmwareTravel({
    profile: { travelX: 100, travelY: 100, omitTravel: false },
    travelBounds: { x: [0, 100], y: [0, 100] },
    segments: [{ line: "G1 X30 Y20 S300", fromMpos: [10, 10, 0], toMpos: [40, 30, 0], laserOn: true }],
  });
  assert.equal(pass.status, "pass");

  const failOutOfBounds = checkLaserOnWithinFirmwareTravel({
    profile: { travelX: 100, travelY: 100, omitTravel: false },
    travelBounds: { x: [0, 100], y: [0, 100] },
    segments: [{ line: "G1 X110 Y20 S300", fromMpos: [80, 10, 0], toMpos: [115, 20, 0], laserOn: true }],
  });
  assert.equal(failOutOfBounds.status, "fail");
  assert.equal(failOutOfBounds.severity, "safety");
});

test("不变量 6 (estop_realtime_not_queued): 正常与违反用例", () => {
  const pass = checkEstopRealtimeNotQueued({
    estopInfo: { requestedAtMs: 1000, maxLatencyMs: 500, jobConnId: 2, totalLines: 100, linesReceivedBeforeReset: 12 },
    events: [{ type: "realtime", byte: 0x18, connId: 2, tsMs: 1025 }],
  });
  assert.equal(pass.status, "pass");

  const failSlow = checkEstopRealtimeNotQueued({
    estopInfo: { requestedAtMs: 1000, maxLatencyMs: 500, jobConnId: 2, totalLines: 100, linesReceivedBeforeReset: 12 },
    events: [{ type: "realtime", byte: 0x18, connId: 2, tsMs: 2400 }],
  });
  assert.equal(failSlow.status, "fail");
  assert.equal(failSlow.severity, "safety");

  const failWrongConn = checkEstopRealtimeNotQueued({
    estopInfo: { requestedAtMs: 1000, maxLatencyMs: 500, jobConnId: 2 },
    events: [{ type: "realtime", byte: 0x18, connId: 3, tsMs: 1010 }],
  });
  assert.equal(failWrongConn.status, "fail");
});

test("不变量 7 (gcode_sha256_matches_preview): 正常与违反用例", () => {
  const gcode = Buffer.from("G21\nG90\nM5\n", "utf8");
  const sha = createHash("sha256").update(gcode).digest("hex");

  const pass = checkGcodeSha256MatchesPreview({ preview: { expectedSha256: sha, gcodeBytes: gcode } });
  assert.equal(pass.status, "pass");

  const fail = checkGcodeSha256MatchesPreview({
    preview: { expectedSha256: sha, gcodeBytes: Buffer.from("G21\nG90\nM3 S999\n", "utf8") },
  });
  assert.equal(fail.status, "fail");
  assert.equal(fail.severity, "safety");
});

test("不变量 8 (resume_start_line_and_no_duplicate_burn): 正常与违反用例", () => {
  const preparedLines = ["G21", "G90", "M4 S0", "G1 X10 S300 F1000", "G1 X20 S300", "M5"];
  const pass = checkResumeStartLineAndNoDuplicateBurn({
    resumeInfo: {
      resumableFromLine: 3,
      linesCompleted: 3,
      resumeBoundarySource: "device_hold_ln",
      preparedLines,
      resumeWireLines: [
        { line: "M5 S0" },
        { line: "G0 X0 Y0" },
        { line: "N1 G1 X10 S300 F1000" },
        { line: "N2 G1 X20 S300" },
        { line: "N3 M5" },
      ],
    },
  });
  assert.equal(pass.status, "pass");

  // 违反：从已完成行重复开始（多发了一行）
  const failWrongLine = checkResumeStartLineAndNoDuplicateBurn({
    resumeInfo: {
      resumableFromLine: 3,
      linesCompleted: 3,
      resumeBoundarySource: "device_hold_ln",
      preparedLines,
      resumeWireLines: [
        { line: "M5 S0" },
        { line: "N1 M4 S0" },
        { line: "N2 G1 X10 S300 F1000" },
        { line: "N3 G1 X20 S300" },
        { line: "N4 M5" },
      ],
    },
  });
  assert.equal(failWrongLine.status, "fail");
  assert.equal(failWrongLine.severity, "safety");
});

test("不变量 9 (override_only_during_active_job_same_channel): 正常与违反用例", () => {
  const pass = checkOverrideOnlyDuringActiveJobSameChannel({
    events: [{ type: "realtime", byte: 0x91, name: "FeedOvr+10(0x91)", connId: 4, tsMs: 1200 }],
    overrideInfo: {
      idleRejectedExpected: true,
      idleAllRejected: true,
      idleOverrideBytesCount: 0,
      expectActiveOverrideCount: 1,
      activeWindows: [{ connId: 4, startMs: 1000, endMs: 1500 }],
    },
  });
  assert.equal(pass.status, "pass");

  const failWrongChannel = checkOverrideOnlyDuringActiveJobSameChannel({
    events: [{ type: "realtime", byte: 0x91, name: "FeedOvr+10(0x91)", connId: 5, tsMs: 1200 }],
    overrideInfo: {
      idleRejectedExpected: true,
      idleAllRejected: true,
      idleOverrideBytesCount: 0,
      activeWindows: [{ connId: 4, startMs: 1000, endMs: 1500 }],
    },
  });
  assert.equal(failWrongChannel.status, "fail");
  assert.equal(failWrongChannel.severity, "safety");
});

test("不变量 10 (homing_disabled_when_dollar22_zero): 正常与违反用例", () => {
  const pass = checkHomingDisabledWhenDollar22Zero({
    homingInfo: { homingEnabled: false, probeReportedHomingEnabled: false, homeHttpStatus: 400 },
    lines: [{ line: "$$" }, { line: "$CD" }],
  });
  assert.equal(pass.status, "pass");

  const fail = checkHomingDisabledWhenDollar22Zero({
    homingInfo: { homingEnabled: false, probeReportedHomingEnabled: false, homeHttpStatus: 400 },
    lines: [{ line: "$$" }, { line: "$H" }],
  });
  assert.equal(fail.status, "fail");
  assert.equal(fail.severity, "safety");
});

test("不变量 11 (time_estimate_deviation): 偏差小通过、偏差大标为提示（info，不报错）", () => {
  const pass = checkTimeEstimateDeviation({
    preview: { estimatedSeconds: 5.0 },
    segments: [{ simDurationSec: 4.5 }],
  });
  assert.equal(pass.status, "pass");

  const notice = checkTimeEstimateDeviation({
    preview: { estimatedSeconds: 30.0 },
    segments: [{ simDurationSec: 5.0 }],
  });
  assert.equal(notice.status, "notice");
  assert.equal(notice.severity, "info");
});

test("不变量 12 (warnings_consistent_with_reality): 正常与违反用例", () => {
  const pass = checkWarningsConsistentWithReality({
    sendResult: { warnings: ["固件未报告行程且软限位未开启，越界不会被拦截"] },
    preview: { warnings: ["雕刻速度 2000 mm/min 超过固件最大速度 500 mm/min"] },
    warningExpectations: {
      mustMatch: [{ pattern: /未报告行程/ }, { pattern: /超过固件最大速度/ }],
      mustNotMatch: [{ pattern: /不存在的警告/ }],
    },
  });
  assert.equal(pass.status, "pass");

  const fail = checkWarningsConsistentWithReality({
    sendResult: { warnings: [] },
    preview: { warnings: [] },
    warningExpectations: {
      mustMatch: [{ pattern: /未报告行程/, label: "缺少软限位警告" }],
    },
  });
  assert.equal(fail.status, "fail");
});

// ---------------------------------------------------------------------------
// 3. L1 只读白名单强拒绝测试 + 假回环服务端到端归档脱敏测试
// ---------------------------------------------------------------------------

test("L1 只读白名单：放行合法只读接口，严格拒绝一切发送/移动/倍率/急停/解锁/回零与非回环地址", async () => {
  // 允许项
  for (const [m, r, b] of [
    ["GET", "/api/health", undefined],
    ["GET", "/api/system/info", undefined],
    ["GET", "/api/jobs/stop-targets", undefined],
    ["GET", "/api/ui/runtime-config", undefined],
    ["GET", "/api/ui/serial-ports", undefined],
    ["POST", "/api/connection/check", { connection_mode: "network", network_host: "127.0.0.1", network_telnet_port: 23 }],
    ["POST", "/api/safe-action", { action: "status", connection_mode: "network", host: "127.0.0.1" }],
  ]) {
    assert.doesNotThrow(() => assertReadOnlyRequest(m, r, b), `应放行 ${m} ${r}`);
  }

  // 禁止项
  const forbidden = [
    ["POST", "/api/workflow", { action: "confirm_send", confirmed: true }],
    ["POST", "/api/text-task/send", { workflow_id: "wf-1", confirmed: true }],
    ["POST", "/api/material-lab/send", { calibration_id: "cal-1", confirmed: true }],
    ["POST", "/api/device/framing", { min_x: 0, max_x: 10 }],
    ["POST", "/api/device/pointer-fire", { confirmed: true, power: 5, duration_ms: 50 }],
    ["POST", "/api/settings", { web_port: 9667 }],
    ["POST", "/api/jobs/acknowledge-stop", { job_id: "wf-1", confirmed: true }],
    ["POST", "/api/safe-action", { action: "unlock", confirmed: true }],
    ["POST", "/api/safe-action", { action: "home", confirmed: true }],
    ["POST", "/api/safe-action", { action: "emergency_stop", confirmed: true }],
    ["POST", "/api/safe-action", { action: "soft_reset", confirmed: true }],
    ["POST", "/api/safe-action", { action: "relative_move", x: 1, y: 0, confirmed: true }],
    ["POST", "/api/safe-action", { action: "absolute_move", x: 0, y: 0, confirmed: true }],
    ["POST", "/api/safe-action", { action: "feed_override_plus_10" }],
    ["POST", "/api/safe-action", { action: "power_override_minus_10" }],
    ["POST", "/api/safe-action", { action: "pause" }],
    ["POST", "/api/safe-action", { action: "resume", confirmed: true }],
    ["POST", "/api/safe-action", { action: "stop_framing" }],
    ["POST", "/api/safe-action", { action: "" }],
    ["POST", "/api/safe-action", {}],
    ["POST", "/api/safe-action", { action: "status", x: 10 }],
    ["POST", "/api/safe-action", { action: "status", confirmed: true }],
    ["DELETE", "/api/health", undefined],
  ];

  let fetchCalled = 0;
  const client = createReadOnlyClient("http://127.0.0.1:9666", {
    fetchImpl: async () => {
      fetchCalled += 1;
      throw new Error("白名单拒绝时绝不应触发网络请求");
    },
  });

  for (const [m, r, b] of forbidden) {
    await assert.rejects(
      () => client.request(m, r, b),
      (err) => err instanceof ReadOnlyWhitelistError,
      `白名单必须拒绝 ${m} ${r} ${JSON.stringify(b)}`,
    );
  }
  assert.equal(fetchCalled, 0, "所有被白名单拒绝的请求均不得触达底层 fetch");

  // 拒绝非回环 URL
  assert.throws(
    () => createReadOnlyClient("http://192.0.2.55:9666"),
    (err) => err instanceof ReadOnlyWhitelistError,
  );
});

test("L1 端到端（假回环服务）：任务占用预检拦截 + 正常只读核对归档齐全与脱敏", async () => {
  const tempRoot = mkdtempSync(join(tmpdir(), "moss-l1-test-"));
  const runtimeDir = join(tempRoot, "runtime");
  mkdirSync(runtimeDir, { recursive: true });
  const fakeDeviceIp = "198.51.100.42";
  const fakeSecret = ["sk", "FakeL1SecretToken0123456789ABCDEF"].join("-");
  writeFileSync(
    join(runtimeDir, "settings.json"),
    JSON.stringify({
      web_port: 9666,
      laser_verified_connection: {
        mode: "network",
        verified_at: 1700000000,
        fields: { connection_mode: "network", network_host: fakeDeviceIp, host: fakeDeviceIp, network_telnet_port: 23, telnet_port: 23 },
        machine_limits: { x_travel_mm: 300, y_travel_mm: 200, x_max_rate_mm_per_min: 6000 },
      },
    }),
    "utf8",
  );

  let busyJobs = [{ job_id: "wf-busy-1", target: `${fakeDeviceIp}:23`, state: "running" }];
  let probeHits = 0;
  const server = http.createServer((req, res) => {
    const url = new URL(req.url || "/", "http://127.0.0.1");
    let body = "";
    req.on("data", (c) => { body += c; });
    req.on("end", () => {
      if (req.method === "GET" && url.pathname === "/api/health") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ success: true, result: { status: "ok" } }));
        return;
      }
      if (req.method === "GET" && url.pathname === "/api/jobs/stop-targets") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ success: true, result: { jobs: busyJobs, can_acknowledge_stop: true } }));
        return;
      }
      if (req.method === "GET" && url.pathname === "/api/ui/runtime-config") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ success: true, result: { connection: { default_mode: "network" } } }));
        return;
      }
      if (req.method === "POST" && url.pathname === "/api/connection/check") {
        probeHits += 1;
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(
          JSON.stringify({
            success: true,
            result: {
              connected: true,
              firmware_settings: ["$20=0", "$22=0", "$30=1000", "$32=1"],
              soft_limits_enabled: false,
              homing_enabled: false,
              identity: { firmware: "GRBL", version: "1.1h", machine_name: null },
              machine_limits: { x_travel_mm: null, y_travel_mm: null, x_max_rate_mm_per_min: 4000 },
              features: null,
              summary: `网络握手成功 (${fakeDeviceIp}:23) api_key=${fakeSecret}`,
            },
          }),
        );
        return;
      }
      if (req.method === "POST" && url.pathname === "/api/safe-action") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ success: true, result: "设备状态: Idle", detail: "<Idle|MPos:0.000,0.000,0.000|Ov:120,100,100>" }));
        return;
      }
      res.writeHead(404);
      res.end("{}");
    });
  });

  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const port = server.address().port;
  try {
    // 1) 有运行中任务时预检拒绝且绝不调用 /api/connection/check
    const blockedOut = join(tempRoot, "out-blocked");
    const blocked = await runReadOnlyCheck({
      url: `http://127.0.0.1:${port}`,
      runtimeDir,
      outDir: blockedOut,
    });
    assert.equal(blocked.exitCode, 1);
    assert.equal(blocked.summary.precheckBlocked, true);
    assert.equal(probeHits, 0, "有运行中任务时绝不能发起设备探测");

    // 2) 空闲后执行只读核对：检出缺失行程+软限位关、参数不一致、倍率非 100%，且归档齐全并彻底脱敏
    busyJobs = [];
    const okOut = join(tempRoot, "out-ok");
    const checked = await runReadOnlyCheck({
      url: `http://127.0.0.1:${port}`,
      runtimeDir,
      outDir: okOut,
    });
    assert.equal(probeHits, 1);
    assert.equal(checked.exitCode, 2, "检出安全类发现（无行程且软限位未开）应返回退出码 2");
    assert.ok(checked.analysis.findings.some((f) => f.id === "no_travel_and_soft_limits_off" && f.severity === "safety"));
    assert.ok(checked.analysis.findings.some((f) => f.id === "limit_mismatch_x_max_rate_mm_per_min"));
    assert.ok(checked.analysis.findings.some((f) => f.id === "override_not_100"));

    const expectedFiles = ["steps.jsonl", "firmware.json", "invariants.jsonl", "timeline.jsonl", "timeline.txt", "report.md", "summary.json"];
    for (const f of expectedFiles) {
      const full = join(okOut, f);
      assert.ok(existsSync(full), `缺少归档文件 ${f}`);
      const content = readFileSync(full, "utf8");
      assert.ok(!content.includes(fakeDeviceIp), `${f} 不得泄露真实设备 IP`);
      assert.ok(!content.includes(fakeSecret), `${f} 不得泄露密钥`);
      assert.ok(!content.includes(tempRoot), `${f} 不得泄露本机临时绝对路径`);
    }
  } finally {
    await new Promise((r) => server.close(r));
    rmSync(tempRoot, { recursive: true, force: true });
  }
});

test("退出码规则与 L1 分析纯函数：安全类非 0、一般失败非 0、仅提示为 0", () => {
  assert.equal(computeExitCode({ safetyViolationCount: 1, errorViolationCount: 0, failedScenarios: 1 }), 2);
  assert.equal(computeExitCode({ safetyViolationCount: 0, errorViolationCount: 1, failedScenarios: 1 }), 1);
  assert.equal(computeExitCode({ safetyViolationCount: 0, errorViolationCount: 0, failedScenarios: 0, noticeCount: 3 }), 0);

  const clean = analyzeReadOnlySnapshot({
    probe: {
      connected: true,
      soft_limits_enabled: true,
      homing_enabled: true,
      identity: { firmware: "FluidNC", version: "v4.1.1", machine_name: "Demo" },
      machine_limits: { x_travel_mm: 300, y_travel_mm: 200 },
      summary: "握手成功",
    },
    statusReport: { state: "Idle", pins: "", ov: [100, 100, 100], raw: "<Idle|MPos:0,0,0|Ov:100,100,100>" },
    remembered: { machine_limits: { x_travel_mm: 300, y_travel_mm: 200 } },
  });
  assert.equal(clean.findings.length, 0);
});

test("L0 报告：安全类排在一般失败与提示之前，并彻底脱敏凭据/路径/非回环地址", () => {
  const fakeToken = ["sk", "FakeL0ReportSecret0123456789"].join("-");
  const fakeIp = "198.51.100.77";
  const fakeDir = "/tmp/moss-l0-fake-runtime-999";
  const md = buildSimReportMarkdown({
    runId: "sim-unit",
    profileName: "fluidnc-default",
    startedAtMs: 1700000000000,
    finishedAtMs: 1700000002500,
    summary: { passedScenarios: 1, totalScenarios: 2, passRate: 0.5, safetyViolationCount: 1, errorViolationCount: 1, noticeCount: 1 },
    redactOpts: { runtimeDir: fakeDir, redactDeviceAddresses: true, extraPaths: [fakeDir] },
    results: [
      {
        id: "sc_demo",
        category: "control",
        passed: false,
        durationMs: 120,
        safetyViolations: [
          {
            id: "homing_disabled_when_dollar22_zero",
            scenario: "sc_demo",
            severity: "safety",
            title: "$22=0 时禁用机械回零",
            message: `在 ${fakeDir} 连向 ${fakeIp} 发出了 $H token=${fakeToken}`,
            suspectedCode: "crates/laser-tools/src/lib.rs",
            evidence: { desktop_session: "0123456789abcdef0123456789abcdef", path: `${fakeDir}/x.gcode` },
          },
        ],
        errorViolations: [
          { id: "assert_err", scenario: "sc_demo", severity: "error", title: "一般失败项", message: "HTTP 500" },
        ],
        notices: [
          { id: "time_estimate_deviation", scenario: "sc_demo", severity: "info", title: "工时估算偏差", message: "偏差提示" },
        ],
      },
    ],
  });
  const safetyPos = md.indexOf("[安全]");
  const errorPos = md.indexOf("[失败]");
  const infoPos = md.indexOf("[提示]");
  assert.ok(safetyPos > 0 && errorPos > safetyPos && infoPos > errorPos, "报告问题须按 安全 > 失败 > 提示 排序");
  assert.ok(!md.includes(fakeToken), "报告不得泄露密钥");
  assert.ok(!md.includes(fakeIp), "报告不得泄露非回环 IP");
  assert.ok(!md.includes(fakeDir), "报告不得泄露本机绝对路径");
  assert.ok(!md.includes("0123456789abcdef0123456789abcdef"), "报告不得泄露桌面会话令牌");
});

let detectedHostBin = null;
try {
  detectedHostBin = resolveHostBinary();
} catch {
  detectedHostBin = null;
}

test("L0 归档齐全与脱敏（独立隔离 moss-host 实跑 2 个场景）", { skip: !detectedHostBin && "未构建 moss-host 二进制" }, async () => {
  const tempOut = mkdtempSync(join(tmpdir(), "moss-l0-archive-test-"));
  try {
    const { outDir, summary, exitCode } = await runLaserSimBench({
      scenarios: ["gen_text", "send_text_complete"],
      outDir: tempOut,
      hostBin: detectedHostBin,
    });
    assert.equal(outDir, tempOut);
    assert.equal(exitCode, 0);
    assert.equal(summary.passedScenarios, 2);
    assert.equal(summary.safetyViolationCount, 0);

    const expectedFiles = [
      "steps.jsonl",
      "serial.log",
      "invariants.jsonl",
      "timeline.jsonl",
      "timeline.txt",
      "report.md",
      "summary.json",
    ];
    for (const f of expectedFiles) {
      const full = join(tempOut, f);
      assert.ok(existsSync(full), `缺少 L0 归档文件 ${f}`);
      const text = readFileSync(full, "utf8");
      assert.ok(text.length > 0, `L0 归档文件 ${f} 不应为空`);
      assert.ok(!text.includes(tempOut), `${f} 不得泄露本机归档绝对路径`);
    }
    const gcodeFiles = readdirSync(join(tempOut, "gcode"));
    assert.ok(gcodeFiles.some((n) => n.endsWith(".gcode")), "gcode/ 须包含导出的 .gcode 副本");
    assert.ok(gcodeFiles.some((n) => n.endsWith(".sha256.json")), "gcode/ 须包含 .sha256.json 记录");
  } finally {
    rmSync(tempOut, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// 4. LB3: L2 空跑测试（检查器违规检测 + 预检非 0 退出 + 模拟器与假后端跑通）
// ---------------------------------------------------------------------------

test("LB3: checkDryRunStatusReports 能检出非零功率 S>0、超速、越界与非 Idle 终态，并放行合规记录", () => {
  const clean = checkDryRunStatusReports({
    reports: [
      "<Run|MPos:10.000,5.000,0.000|Bf:15,256|FS:900,0|Ov:100,100,100>",
      "<Run|MPos:20.000,10.000,0.000|Bf:15,256|FS:1800,0|Ov:200,100,100>",
      "<Idle|MPos:0.000,0.000,0.000|Bf:16,256|FS:0,0|Ov:100,100,100>",
    ],
    maxFeedMmMin: 2000,
    machineLimits: { x_travel_mm: 300, y_travel_mm: 200 },
  });
  assert.ok(clean.every((c) => c.status === "pass"), JSON.stringify(clean));

  const badPower = checkDryRunStatusReports({
    reports: [
      "<Run|MPos:10.000,5.000,0.000|FS:800,250|Ov:100,100,100>",
      "<Idle|MPos:0.000,0.000,0.000|FS:0,0|Ov:100,100,100>",
    ],
    maxFeedMmMin: 2000,
    machineLimits: { x_travel_mm: 300, y_travel_mm: 200 },
  });
  assert.equal(badPower.find((c) => c.id === "dryrun_laser_power_zero")?.status, "fail");
  assert.equal(badPower.find((c) => c.id === "dryrun_laser_power_zero")?.severity, "safety");

  const badFeed = checkDryRunStatusReports({
    reports: [
      "<Run|MPos:10.000,5.000,0.000|FS:2600,0|Ov:100,100,100>",
      "<Idle|MPos:0.000,0.000,0.000|FS:0,0|Ov:100,100,100>",
    ],
    maxFeedMmMin: 2000,
    machineLimits: { x_travel_mm: 300, y_travel_mm: 200 },
  });
  assert.equal(badFeed.find((c) => c.id === "dryrun_feed_within_limit")?.status, "fail");
  assert.equal(badFeed.find((c) => c.id === "dryrun_feed_within_limit")?.severity, "safety");

  const badTravelAndNotIdle = checkDryRunStatusReports({
    reports: ["<Run|MPos:350.000,10.000,0.000|FS:800,0|Ov:100,100,100>"],
    maxFeedMmMin: 2000,
    machineLimits: { x_travel_mm: 300, y_travel_mm: 200 },
  });
  assert.equal(badTravelAndNotIdle.find((c) => c.id === "dryrun_coords_within_travel")?.status, "fail");
  assert.equal(badTravelAndNotIdle.find((c) => c.id === "dryrun_idle_after_finish")?.status, "fail");
});

test("LB3: laser-dryrun-bench 预检失败非 0 退出，开启空跑后经模拟器+假后端完整跑通并带 X-Moss-Bench: dryrun", async () => {
  const tempRoot = mkdtempSync(join(tmpdir(), "moss-l2-test-"));
  const runtimeDir = join(tempRoot, "runtime");
  mkdirSync(runtimeDir, { recursive: true });
  const sim = await startGrblSimulator({
    profile: "fluidnc",
    initialMpos: [10, 10, 0],
    homingEnabled: true,
    ackDelayMs: 2,
  });

  let dryRunEnabled = false;
  let connected = true;
  const seenBenchHeaders = new Set();
  const wfState = new Map();
  let wfSeq = 0;

  // 轻量假后端：转发探测/状态与空跑运动到真实回环模拟器，验证 L2 脚本完整编排与预检
  const sendToSim = (rawLines) =>
    new Promise((resolvePromise, reject) => {
      const sock = net.createConnection({ host: "127.0.0.1", port: sim.port }, () => {
        let buf = "";
        let lastStatus = "<Idle|MPos:10.000,10.000,0.000|Bf:16,256|FS:0,0|Ov:100,100,100>";
        let idx = 0;
        const sendNext = () => {
          if (idx >= rawLines.length) {
            sock.end();
            resolvePromise(lastStatus);
            return;
          }
          sock.write(rawLines[idx]);
          idx += 1;
        };
        sock.on("data", (chunk) => {
          buf += chunk.toString("utf8");
          let nl;
          while ((nl = buf.indexOf("\n")) >= 0) {
            const line = buf.slice(0, nl).replace(/\r$/, "");
            buf = buf.slice(nl + 1);
            if (line.startsWith("<")) lastStatus = line;
            if (line === "ok" || line.startsWith("<")) sendNext();
          }
        });
        sendNext();
      });
      sock.once("error", reject);
    });

  const server = http.createServer((req, res) => {
    const url = new URL(req.url || "/", "http://127.0.0.1");
    if (req.headers["x-moss-bench"]) seenBenchHeaders.add(String(req.headers["x-moss-bench"]));
    let body = "";
    req.on("data", (c) => { body += c; });
    req.on("end", async () => {
      const json = body ? JSON.parse(body) : {};
      const reply = (code, payload) => {
        res.writeHead(code, { "Content-Type": "application/json" });
        res.end(JSON.stringify(payload));
      };
      if (req.method === "GET" && url.pathname === "/api/system/info") {
        return reply(200, {
          success: true,
          result: {
            laser_dry_run_enabled: dryRunEnabled,
            dry_run_max_feed_mm_min: 2000,
            laser_dry_run_effective_max_feed_mm_min: 1000,
            laser_dry_run_remaining_seconds: dryRunEnabled ? 3500 : 0,
          },
        });
      }
      if (req.method === "GET" && url.pathname === "/api/jobs/stop-targets") {
        return reply(200, { success: true, result: { jobs: [] } });
      }
      if (req.method === "POST" && url.pathname === "/api/connection/check") {
        if (!connected) {
          return reply(200, { success: true, result: { connected: false, summary: "设备未连接" } });
        }
        await sendToSim(["$$\n"]);
        return reply(200, {
          success: true,
          result: {
            connected: true,
            homing_enabled: true,
            soft_limits_enabled: true,
            machine_limits: { x_travel_mm: 300, y_travel_mm: 200 },
            summary: "网络握手成功",
          },
        });
      }
      if (req.method === "POST" && url.pathname === "/api/device/framing") {
        await sendToSim(["M5 S0 F1000\n", "G1 X20 Y10 F1000 S0\n", "G1 X0 Y0 F1000 S0\n"]);
        return reply(200, { success: true, result: { message: "走边框完成", job_id: "framing-1" } });
      }
      if (req.method === "POST" && url.pathname === "/api/text-task/generate") {
        const id = `wf-l2-${++wfSeq}`;
        wfState.set(id, { workflow_id: id, status: "preview_ready" });
        return reply(200, { success: true, result: { workflow_id: id, status: "preview_ready" } });
      }
      if (req.method === "POST" && url.pathname === "/api/workflow") {
        const id = json.workflow_id;
        if (json.action === "confirm_send") {
          const st = await sendToSim(["M5 S0 F1000\n", "G1 X10 Y5 F1000 S0\n", "M5\n", "?"]);
          wfState.set(id, {
            workflow_id: id,
            status: "completed",
            send_result: { lines_sent: 3, total_lines: 3, last_status_report: st },
          });
          return reply(200, { success: true, result: wfState.get(id) });
        }
        if (json.action === "cancel") {
          const cur = wfState.get(id) || { workflow_id: id };
          cur.status = "cancelled";
          cur.send_result = { lines_sent: 1, total_lines: 3, can_resume: true, resumable_from_line: 1 };
          wfState.set(id, cur);
          return reply(200, { success: true, result: cur });
        }
        if (json.action === "resume") {
          const st = await sendToSim(["M5 S0 F1000\n", "G1 X15 Y5 F1000 S0\n", "M5\n", "?"]);
          wfState.set(id, {
            workflow_id: id,
            status: "completed",
            send_result: { lines_sent: 3, total_lines: 3, last_status_report: st },
          });
          return reply(200, { success: true, result: wfState.get(id) });
        }
        return reply(200, { success: true, result: wfState.get(id) || { status: "completed" } });
      }
      if (req.method === "POST" && url.pathname === "/api/safe-action") {
        if (json.action === "status") {
          const st = await sendToSim(["?"]);
          return reply(200, { success: true, result: "设备状态: Idle", detail: st });
        }
        if (json.action === "feed_override_plus_10" && wfSeq === 0) {
          return reply(400, { success: false, result: "当前没有运行中任务", detail: "no_active_job" });
        }
        if (json.action === "feed_override_plus_10" && wfState.size === 2) {
          return reply(400, { success: false, result: "当前没有运行中任务", detail: "no_active_job" });
        }
        if (json.action === "relative_move") {
          await sendToSim(["M5 S0 F1000\n", "G91\n", "G1 X2 Y2 F1000 S0\n", "G90\n"]);
          return reply(200, { success: true, result: "移动完成" });
        }
        if (json.action === "absolute_move") {
          await sendToSim(["M5 S0 F1000\n", "G90\n", "G1 X0 Y0 F1000 S0\n"]);
          return reply(200, { success: true, result: "回原点完成" });
        }
        if (json.action === "home") {
          await sendToSim(["$H\n"]);
          return reply(200, { success: true, result: "回零完成" });
        }
        return reply(200, { success: true, result: "ok" });
      }
      reply(404, { success: false });
    });
  });

  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const port = server.address().port;
  const logs = [];
  try {
    // 1) 空跑开关关闭 -> 预检失败，非 0 退出
    const resOff = await runDryRunBench({
      url: `http://127.0.0.1:${port}`,
      runtimeDir,
      outDir: join(tempRoot, "out-off"),
      host: "127.0.0.1",
      telnetPort: sim.port,
      log: (m) => logs.push(m),
    });
    assert.notEqual(resOff.exitCode, 0);
    assert.equal(resOff.summary.precheckBlocked, true);
    assert.equal(sim.connections.length, 0, "空跑关闭时不得连接设备");

    // 2) 开启空跑但设备未连接 -> 预检失败，非 0 退出
    dryRunEnabled = true;
    connected = false;
    const resNoConn = await runDryRunBench({
      url: `http://127.0.0.1:${port}`,
      runtimeDir,
      outDir: join(tempRoot, "out-noconn"),
      host: "127.0.0.1",
      telnetPort: sim.port,
      log: (m) => logs.push(m),
    });
    assert.notEqual(resNoConn.exitCode, 0);
    assert.equal(resNoConn.summary.precheckBlocked, true);

    // 3) 开启空跑 + 已连接 -> 完整跑通，打印开跑提示，带 X-Moss-Bench: dryrun，归档齐全
    connected = true;
    const okOut = join(tempRoot, "out-ok");
    const resOk = await runDryRunBench({
      url: `http://127.0.0.1:${port}`,
      runtimeDir,
      outDir: okOut,
      host: "127.0.0.1",
      telnetPort: sim.port,
      log: (m) => logs.push(m),
    });
    assert.equal(resOk.exitCode, 0, JSON.stringify(resOk.summary));
    assert.ok(seenBenchHeaders.has("dryrun"), "所有 L2 请求须携带 X-Moss-Bench: dryrun");
    assert.ok(
      logs.some((m) => m.includes("机器将以不出光、限速方式运动，请确认机器旁有人")),
      "开跑前须打印在场提示",
    );
    for (const f of ["steps.jsonl", "status-reports.jsonl", "invariants.jsonl", "timeline.jsonl", "timeline.txt", "report.md", "summary.json"]) {
      const full = join(okOut, f);
      assert.ok(existsSync(full), `缺少 L2 归档文件 ${f}`);
      const content = readFileSync(full, "utf8");
      assert.ok(!content.includes(tempRoot), `${f} 不得泄露本机临时绝对路径`);
    }
  } finally {
    await new Promise((r) => server.close(r));
    await sim.close();
    rmSync(tempRoot, { recursive: true, force: true });
  }
});
