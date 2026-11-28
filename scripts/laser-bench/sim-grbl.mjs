// 可配置 GRBL / FluidNC 回环模拟器（仅监听 127.0.0.1）：
// - 支持 GRBL 与 FluidNC 固件画像（行程、$30、$20/$22/$23、$CD YAML、$I）；
// - 逐字节带时间戳记录全部 RX/TX 数据流；
// - 完整处理实时字节（?、!、~、0x18、0x85、倍率 0x90–0x9D、0xA0），不排在顺序行队列之后；
// - 追踪 G-code 模态与机械坐标 MPos，支撑越界检测与模拟执行时间计算；
// - 支持故障注入（报警、断连、软限位、门开 Pn:D、ack 变慢/冻结、不报行程）。

import { readFileSync } from "node:fs";
import net from "node:net";
import { resolve } from "node:path";

export const REALTIME_BYTES = Object.freeze({
  0x18: "SoftReset(0x18)",
  0x21: "FeedHold(!)",
  0x3f: "StatusQuery(?)",
  0x7e: "CycleStart(~)",
  0x85: "JogCancel(0x85)",
  0x90: "FeedOvrReset(0x90)",
  0x91: "FeedOvr+10(0x91)",
  0x92: "FeedOvr-10(0x92)",
  0x93: "FeedOvr+1(0x93)",
  0x94: "FeedOvr-1(0x94)",
  0x95: "RapidOvr100(0x95)",
  0x96: "RapidOvr50(0x96)",
  0x97: "RapidOvr25(0x97)",
  0x99: "SpindleOvrReset(0x99)",
  0x9a: "SpindleOvr+10(0x9A)",
  0x9b: "SpindleOvr-10(0x9B)",
  0x9c: "SpindleOvr+1(0x9C)",
  0x9d: "SpindleOvr-1(0x9D)",
  0xa0: "AirAssistToggle(0xA0)",
});

export function isOverrideRealtimeByte(b) {
  return (b >= 0x90 && b <= 0x97) || (b >= 0x99 && b <= 0x9d);
}

export const BUILTIN_PROFILES = Object.freeze({
  fluidnc: {
    name: "fluidnc-default",
    firmware: "fluidnc",
    machineName: "MOSS-Sim-FluidNC",
    banner: "Grbl 3.7 [FluidNC '$' for help]",
    identityLines: [
      "[VER:4.1 FluidNC v4.1.1 (esp32-sim) :bench]",
      "[MSG:Machine: MOSS-Sim-FluidNC]",
    ],
    laserMode: 1,
    reportStatusMask: 3, // bit0=MPos, bit1=Bf
    maxSpindleSpeed: 1000,
    minSpindleSpeed: 0,
    softLimits: true,
    homingEnabled: true,
    homingDirMask: 0,
    travelX: 300,
    travelY: 200,
    travelZ: null,
    omitTravel: false,
    maxRateX: 6000,
    maxRateY: 6000,
    accelX: 1000,
    accelY: 1000,
    junctionDeviation: 0.01,
    plannerBlocks: 16,
    rxBufferSize: 256,
    useLineNumbers: true,
    verboseErrors: true,
    speedMap: "0=0% 1000=100%",
    laserOffOnAlarm: true,
    hasDoorPin: true,
    homingPositiveDirection: false, // mpos_mm=0, positive_direction=false => [0, travel]
    homingMposX: 0,
    homingMposY: 0,
    initialMpos: [100, 100, 0],
    ackDelayMs: 2,
    plannerLagLines: 3,
  },
  grbl: {
    name: "grbl-default",
    firmware: "grbl",
    machineName: "MOSS-Sim-GRBL",
    banner: "Grbl 1.1h ['$' for help]",
    identityLines: [
      "[VER:1.1h.20190825:MOSS-Sim-GRBL]",
      "[OPT:V,15,128]",
    ],
    laserMode: 1,
    reportStatusMask: 3,
    maxSpindleSpeed: 1000,
    minSpindleSpeed: 0,
    softLimits: true,
    homingEnabled: true,
    homingDirMask: 3, // negative home; with positive MPos > 0 => [0, travel], or negative [-travel, 0]
    travelX: 300,
    travelY: 200,
    travelZ: null,
    omitTravel: false,
    maxRateX: 5000,
    maxRateY: 5000,
    accelX: 800,
    accelY: 800,
    junctionDeviation: 0.01,
    plannerBlocks: 15,
    rxBufferSize: 128,
    useLineNumbers: false,
    verboseErrors: false,
    speedMap: null,
    laserOffOnAlarm: null,
    hasDoorPin: false,
    homingPositiveDirection: false,
    homingMposX: 0,
    homingMposY: 0,
    initialMpos: [100, 100, 0],
    ackDelayMs: 2,
    plannerLagLines: 0,
  },
});

export function loadProfile(profileArg = "fluidnc") {
  if (!profileArg || typeof profileArg === "object") {
    return { ...BUILTIN_PROFILES.fluidnc, ...(profileArg || {}) };
  }
  const key = String(profileArg).trim().toLowerCase();
  if (Object.hasOwn(BUILTIN_PROFILES, key)) {
    return { ...BUILTIN_PROFILES[key] };
  }
  const parsed = JSON.parse(readFileSync(resolve(profileArg), "utf8"));
  const baseKey = String(parsed.firmware || "fluidnc").toLowerCase() === "grbl" ? "grbl" : "fluidnc";
  return { ...BUILTIN_PROFILES[baseKey], ...parsed };
}

/** 计算给定画像的有效机械坐标行程区间（无行程报告时返回 null）。 */
export function computeTravelBounds(profile) {
  if (profile.omitTravel || (profile.travelX == null && profile.travelY == null)) {
    return null;
  }
  const bounds = { x: null, y: null };
  if (profile.firmware === "fluidnc") {
    if (profile.travelX != null) {
      const mpos = Number(profile.homingMposX ?? 0);
      bounds.x = profile.homingPositiveDirection ? [mpos - profile.travelX, mpos] : [mpos, mpos + profile.travelX];
    }
    if (profile.travelY != null) {
      const mpos = Number(profile.homingMposY ?? 0);
      bounds.y = profile.homingPositiveDirection ? [mpos - profile.travelY, mpos] : [mpos, mpos + profile.travelY];
    }
  } else {
    const mask = Number(profile.homingDirMask ?? 0);
    const init = Array.isArray(profile.initialMpos) ? profile.initialMpos : [0, 0, 0];
    if (profile.travelX != null) {
      const negHome = (mask & 1) !== 0;
      bounds.x = negHome && init[0] > 0.05 ? [0, profile.travelX] : [-profile.travelX, 0];
    }
    if (profile.travelY != null) {
      const negHome = (mask & 2) !== 0;
      bounds.y = negHome && init[1] > 0.05 ? [0, profile.travelY] : [-profile.travelY, 0];
    }
  }
  return bounds;
}

export function buildDollarSettings(profile) {
  const lines = [
    "$0=10",
    "$1=25",
    `$10=${profile.reportStatusMask ?? 3}`,
    `$11=${Number(profile.junctionDeviation ?? 0.01).toFixed(3)}`,
    `$20=${profile.softLimits ? 1 : 0}`,
    `$22=${profile.homingEnabled ? 1 : 0}`,
    `$23=${profile.homingDirMask ?? 0}`,
    `$30=${profile.maxSpindleSpeed ?? 1000}`,
    `$31=${profile.minSpindleSpeed ?? 0}`,
    `$32=${profile.laserMode ?? 1}`,
  ];
  if (profile.maxRateX != null) lines.push(`$110=${Number(profile.maxRateX).toFixed(3)}`);
  if (profile.maxRateY != null) lines.push(`$111=${Number(profile.maxRateY).toFixed(3)}`);
  if (profile.accelX != null) lines.push(`$120=${Number(profile.accelX).toFixed(3)}`);
  if (profile.accelY != null) lines.push(`$121=${Number(profile.accelY).toFixed(3)}`);
  if (!profile.omitTravel) {
    if (profile.travelX != null) lines.push(`$130=${Number(profile.travelX).toFixed(3)}`);
    if (profile.travelY != null) lines.push(`$131=${Number(profile.travelY).toFixed(3)}`);
    if (profile.travelZ != null) lines.push(`$132=${Number(profile.travelZ).toFixed(3)}`);
  }
  return lines;
}

export function buildFluidNcYaml(profile) {
  if (typeof profile.cdYaml === "string") return profile.cdYaml;
  const lines = [
    `name: "${profile.machineName || "MOSS-Sim-FluidNC"}"`,
    `planner_blocks: ${profile.plannerBlocks ?? 16}`,
    `use_line_numbers: ${profile.useLineNumbers ? "true" : "false"}`,
    `verbose_errors: ${profile.verboseErrors === false ? "false" : "true"}`,
    "axes:",
    "  x:",
    `    max_rate_mm_per_min: ${Number(profile.maxRateX ?? 6000).toFixed(3)}`,
    `    acceleration_mm_per_sec2: ${Number(profile.accelX ?? 1000).toFixed(3)}`,
    `    soft_limits: ${profile.softLimits ? "true" : "false"}`,
  ];
  if (!profile.omitTravel && profile.travelX != null) {
    lines.push(`    max_travel_mm: ${Number(profile.travelX).toFixed(3)}`);
  }
  if (profile.homingEnabled) {
    lines.push(
      "    homing:",
      "      cycle: 2",
      `      mpos_mm: ${Number(profile.homingMposX ?? 0).toFixed(3)}`,
      `      positive_direction: ${profile.homingPositiveDirection ? "true" : "false"}`,
    );
  }
  lines.push(
    "  y:",
    `    max_rate_mm_per_min: ${Number(profile.maxRateY ?? 6000).toFixed(3)}`,
    `    acceleration_mm_per_sec2: ${Number(profile.accelY ?? 1000).toFixed(3)}`,
    `    soft_limits: ${profile.softLimits ? "true" : "false"}`,
  );
  if (!profile.omitTravel && profile.travelY != null) {
    lines.push(`    max_travel_mm: ${Number(profile.travelY).toFixed(3)}`);
  }
  if (profile.homingEnabled) {
    lines.push(
      "    homing:",
      "      cycle: 1",
      `      mpos_mm: ${Number(profile.homingMposY ?? 0).toFixed(3)}`,
      `      positive_direction: ${profile.homingPositiveDirection ? "true" : "false"}`,
    );
  }
  if (profile.travelZ != null) {
    lines.push("  z:");
    if (!profile.omitTravel) lines.push(`    max_travel_mm: ${Number(profile.travelZ).toFixed(3)}`);
  }
  lines.push(
    "Laser:",
    `  speed_map: ${profile.speedMap || "0=0% 1000=100%"}`,
    `  off_on_alarm: ${profile.laserOffOnAlarm === false ? "false" : "true"}`,
  );
  if (profile.hasDoorPin) {
    lines.push("control:", "  safety_door_pin: gpio.35");
  }
  return lines.join("\r\n");
}

/** 剥离 G-code 注释（; 与括号），保留可执行文本。 */
export function stripGcodeComments(raw) {
  const beforeSemi = String(raw ?? "").split(";")[0];
  let out = "";
  let depth = 0;
  for (const ch of beforeSemi) {
    if (ch === "(") depth += 1;
    else if (ch === ")" && depth > 0) depth -= 1;
    else if (depth === 0) out += ch;
  }
  return out.trim();
}

/** 解析单行 G-code 的词项（支持紧凑写法、N 行号前缀、多个 G/M 词）。 */
export function parseGcodeLine(rawLine) {
  const clean = stripGcodeComments(rawLine);
  const words = [];
  let wireN = null;
  const tokenRe = /([A-Za-z])\s*([+-]?(?:\d+(?:\.\d*)?|\.\d+))/g;
  let m;
  while ((m = tokenRe.exec(clean)) !== null) {
    const letter = m[1].toUpperCase();
    const value = Number(m[2]);
    if (!Number.isFinite(value)) continue;
    if (letter === "N" && words.length === 0 && wireN === null) {
      wireN = Math.round(value);
      continue;
    }
    words.push({ letter, value });
  }
  return { clean, wireN, words };
}

function clampOverride(v) {
  return Math.max(10, Math.min(200, Math.round(v)));
}

function formatHexBytes(buf) {
  return Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join(" ");
}

function formatAsciiPreview(buf) {
  return Array.from(buf, (b) => {
    if (b === 0x0d) return "\\r";
    if (b === 0x0a) return "\\n";
    if (b >= 0x20 && b <= 0x7e) return String.fromCharCode(b);
    return `\\x${b.toString(16).padStart(2, "0")}`;
  }).join("");
}

/**
 * 启动回环 GRBL / FluidNC 模拟器（127.0.0.1 随机空闲端口）。
 */
export function startGrblSimulator(profileOrOpts = {}) {
  const baseProfile = loadProfile(profileOrOpts.profile ?? profileOrOpts);
  const profile = { ...baseProfile, ...(typeof profileOrOpts === "object" ? profileOrOpts : {}) };
  delete profile.profile;

  let faults = { ...(profile.faults || {}) };
  const startedAt = Date.now();
  const sockets = new Set();
  const timers = new Set();
  let nextConnId = 0;

  // 共享机器状态（跨连接持久化，软复位 0x18 重置模态/偏移/倍率/急停，保留 MPos 与固件配置）
  const machine = {
    state: profile.initialState || "Idle",
    pins: profile.initialPins || "",
    unitsScale: 1, // G21=1, G20=25.4
    distanceMode: "G90",
    motionMode: "G0",
    laserState: "M5",
    spindleS: 0,
    feedRate: 1000,
    mpos: [...(Array.isArray(profile.initialMpos) ? profile.initialMpos : [100, 100, 0])],
    g92Offset: [0, 0, 0],
    overrides: { feed: 100, rapid: 100, spindle: 100 },
    airAssist: false,
    gcodeLinesProcessed: 0,
    ackedLines: 0,
    lastWireN: null,
    ackedWireN: null,
    currentExecN: null,
    homingAttemptedWhenDisabled: false,
  };

  let events = [];
  let lines = [];
  let segments = [];
  let serialLog = [];
  let connections = [];

  function trackTimer(t) {
    timers.add(t);
    return t;
  }

  function clearTrackedTimer(t) {
    if (!t) return;
    clearTimeout(t);
    clearInterval(t);
    timers.delete(t);
  }

  function recordByteLog(dir, connId, buf, kind, note = "") {
    const tsMs = Date.now();
    const relMs = tsMs - startedAt;
    serialLog.push({
      tsMs,
      relMs,
      dir,
      connId,
      kind,
      hex: formatHexBytes(buf),
      ascii: formatAsciiPreview(buf),
      byteCount: buf.length,
      note,
    });
  }

  function writeToSocket(socket, connId, text, note = "") {
    if (socket.destroyed) return;
    const buf = Buffer.from(text, "utf8");
    recordByteLog("tx", connId, buf, "tx", note);
    try {
      socket.write(buf);
    } catch {
      // 忽略写入已关闭连接错误
    }
  }

  function formatStatusReport(rxFreeOverride = null) {
    const mask = Number(profile.reportStatusMask ?? 3);
    const parts = [machine.state];
    const mpos = machine.mpos.map((v) => Number(v).toFixed(3)).join(",");
    const wpos = machine.mpos.map((v, i) => Number(v - machine.g92Offset[i]).toFixed(3)).join(",");
    parts.push((mask & 1) !== 0 ? `MPos:${mpos}` : `WPos:${wpos}`);
    if ((mask & 2) !== 0) {
      const rxFree = rxFreeOverride ?? (profile.rxBufferSize ?? 256);
      const plannerFree = profile.plannerBlocks ?? 16;
      parts.push(`Bf:${plannerFree},${rxFree}`);
    }
    const activeFeed = machine.state === "Run" ? Math.round(machine.feedRate * (machine.overrides.feed / 100)) : 0;
    const activeSpindle =
      machine.laserState === "M5" ? 0 : Math.round(machine.spindleS * (machine.overrides.spindle / 100));
    parts.push(`FS:${activeFeed},${activeSpindle}`);
    parts.push(`Ov:${machine.overrides.feed},${machine.overrides.rapid},${machine.overrides.spindle}`);
    if (machine.pins) {
      parts.push(`Pn:${machine.pins}`);
    }
    if (profile.useLineNumbers && machine.currentExecN != null && machine.currentExecN > 0) {
      parts.push(`Ln:${machine.currentExecN}`);
    }
    return `<${parts.join("|")}>`;
  }

  function snapshotState() {
    return {
      state: machine.state,
      pins: machine.pins,
      distanceMode: machine.distanceMode,
      motionMode: machine.motionMode,
      laserState: machine.laserState,
      spindleS: machine.spindleS,
      feedRate: machine.feedRate,
      mpos: [...machine.mpos],
      g92Offset: [...machine.g92Offset],
      overrides: { ...machine.overrides },
      ackedLines: machine.ackedLines,
      ackedWireN: machine.ackedWireN,
      currentExecN: machine.currentExecN,
      gcodeLinesProcessed: machine.gcodeLinesProcessed,
      homingAttemptedWhenDisabled: machine.homingAttemptedWhenDisabled,
    };
  }

  function applySoftReset(connState) {
    // 清空待回 ACK 队列与自动上报计时器
    if (connState.ackTimer) {
      clearTrackedTimer(connState.ackTimer);
      connState.ackTimer = null;
    }
    connState.ackQueue.length = 0;
    if (connState.autoReportTimer) {
      clearTrackedTimer(connState.autoReportTimer);
      connState.autoReportTimer = null;
    }
    machine.state = machine.pins.includes("D") ? "Door:1" : "Idle";
    machine.distanceMode = "G90";
    machine.motionMode = "G0";
    machine.laserState = "M5";
    machine.spindleS = 0;
    machine.g92Offset = [0, 0, 0];
    machine.overrides = { feed: 100, rapid: 100, spindle: 100 };
    machine.ackedWireN = null;
    machine.currentExecN = null;
  }

  function executeGcodeMotion(parsed, rawLine, connId, tsMs) {
    let nextGMode = null;
    let isDwell = false;
    let dwellP = 0;
    let isG92 = false;
    const coords = {};

    for (const { letter, value } of parsed.words) {
      if (letter === "G") {
        const code = Math.round(value);
        if (code === 0) nextGMode = "G0";
        else if (code === 1) nextGMode = "G1";
        else if (code === 2) nextGMode = "G2";
        else if (code === 3) nextGMode = "G3";
        else if (code === 4) isDwell = true;
        else if (code === 20) machine.unitsScale = 25.4;
        else if (code === 21) machine.unitsScale = 1;
        else if (code === 90) machine.distanceMode = "G90";
        else if (code === 91) machine.distanceMode = "G91";
        else if (code === 92) isG92 = true;
      } else if (letter === "M") {
        const code = Math.round(value);
        if (code === 3) machine.laserState = "M3";
        else if (code === 4) machine.laserState = "M4";
        else if (code === 5) {
          machine.laserState = "M5";
          machine.spindleS = 0;
        }
      } else if (letter === "S") {
        machine.spindleS = value;
      } else if (letter === "F") {
        if (value > 0) machine.feedRate = value;
      } else if (letter === "P") {
        dwellP = value;
      } else if (letter === "X" || letter === "Y" || letter === "Z") {
        coords[letter] = value * machine.unitsScale;
      }
    }

    if (nextGMode) machine.motionMode = nextGMode;

    if (isG92) {
      // G92 X/Y/Z 声明当前物理位置对应的工件坐标 => g92Offset = mpos - declaredWpos
      if (coords.X !== undefined) machine.g92Offset[0] = machine.mpos[0] - coords.X;
      if (coords.Y !== undefined) machine.g92Offset[1] = machine.mpos[1] - coords.Y;
      if (coords.Z !== undefined) machine.g92Offset[2] = machine.mpos[2] - coords.Z;
      return { moved: false, dwellSec: 0, softLimitViolation: false };
    }

    if (isDwell) {
      const dwellSec = Math.max(0, dwellP);
      const laserOn = machine.laserState === "M3" && machine.spindleS > 0 && dwellSec > 0;
      if (dwellSec > 0) {
        segments.push({
          line: rawLine,
          wireN: parsed.wireN,
          connId,
          tsMs,
          motionMode: "G4",
          distanceMode: machine.distanceMode,
          laserState: machine.laserState,
          sValue: machine.spindleS,
          feedRate: machine.feedRate,
          fromMpos: [...machine.mpos],
          toMpos: [...machine.mpos],
          distanceMm: 0,
          simDurationSec: dwellSec,
          laserOn,
        });
      }
      return { moved: false, dwellSec, softLimitViolation: false };
    }

    const hasAxisWord = coords.X !== undefined || coords.Y !== undefined || coords.Z !== undefined;
    if (!hasAxisWord) {
      return { moved: false, dwellSec: 0, softLimitViolation: false };
    }

    const fromMpos = [...machine.mpos];
    const targetMpos = [...machine.mpos];
    if (machine.distanceMode === "G90") {
      if (coords.X !== undefined) targetMpos[0] = machine.g92Offset[0] + coords.X;
      if (coords.Y !== undefined) targetMpos[1] = machine.g92Offset[1] + coords.Y;
      if (coords.Z !== undefined) targetMpos[2] = machine.g92Offset[2] + coords.Z;
    } else {
      if (coords.X !== undefined) targetMpos[0] += coords.X;
      if (coords.Y !== undefined) targetMpos[1] += coords.Y;
      if (coords.Z !== undefined) targetMpos[2] += coords.Z;
    }

    // 检查软限位故障（当显式开启 liveSoftLimitCheck 且固件开启软限位且报了行程时）
    let softLimitViolation = false;
    if (faults.enforceSoftLimits && profile.softLimits && !profile.omitTravel) {
      const bounds = computeTravelBounds(profile);
      if (bounds) {
        if (bounds.x && (targetMpos[0] < bounds.x[0] - 0.05 || targetMpos[0] > bounds.x[1] + 0.05)) {
          softLimitViolation = true;
        }
        if (bounds.y && (targetMpos[1] < bounds.y[0] - 0.05 || targetMpos[1] > bounds.y[1] + 0.05)) {
          softLimitViolation = true;
        }
      }
    }
    if (softLimitViolation) {
      return { moved: false, dwellSec: 0, softLimitViolation: true };
    }

    const dx = targetMpos[0] - fromMpos[0];
    const dy = targetMpos[1] - fromMpos[1];
    const dz = targetMpos[2] - fromMpos[2];
    const distanceMm = Math.hypot(dx, dy, dz);
    const rapidRate = Math.max(Number(profile.maxRateX ?? 6000), Number(profile.maxRateY ?? 6000));
    const effectiveRate = machine.motionMode === "G0" ? rapidRate : Math.min(machine.feedRate, rapidRate);
    const simDurationSec = effectiveRate > 0 ? (distanceMm / effectiveRate) * 60 : 0;
    const isCuttingMotion = machine.motionMode !== "G0";
    // $32=1 激光模式下仅 G1/G2/G3 且 M3/M4 且 S>0 时出光
    const laserOn = isCuttingMotion && (machine.laserState === "M3" || machine.laserState === "M4") && machine.spindleS > 0;

    machine.mpos = targetMpos;
    segments.push({
      line: rawLine,
      wireN: parsed.wireN,
      connId,
      tsMs,
      motionMode: machine.motionMode,
      distanceMode: machine.distanceMode,
      laserState: machine.laserState,
      sValue: machine.spindleS,
      feedRate: effectiveRate,
      fromMpos,
      toMpos: [...targetMpos],
      distanceMm,
      simDurationSec,
      laserOn,
    });
    return { moved: true, dwellSec: 0, softLimitViolation: false };
  }

  function handleSequentialLine(connState, rawLine) {
    const { socket, connId } = connState;
    const trimmed = rawLine.trim();
    const tsMs = Date.now();
    const relMs = tsMs - startedAt;

    if (!trimmed) return;

    const lineRecord = {
      index: lines.length,
      connId,
      tsMs,
      relMs,
      line: trimmed,
      stateBefore: snapshotState(),
    };
    lines.push(lineRecord);
    events.push({
      tsMs,
      relMs,
      connId,
      type: "line",
      line: trimmed,
      stateBefore: lineRecord.stateBefore,
    });

    if (trimmed === "$$") {
      const resp = `${buildDollarSettings(profile).join("\r\n")}\r\nok\r\n`;
      writeToSocket(socket, connId, resp, "$$");
      return;
    }
    if (trimmed === "$CD") {
      if (profile.firmware === "fluidnc") {
        writeToSocket(socket, connId, `${buildFluidNcYaml(profile)}\r\nok\r\n`, "$CD");
      } else {
        writeToSocket(socket, connId, "error:3\r\n", "$CD-unsupported");
      }
      return;
    }
    if (trimmed === "$I") {
      const idLines = Array.isArray(profile.identityLines) ? profile.identityLines : [];
      if (idLines.length > 0) {
        writeToSocket(socket, connId, `${idLines.join("\r\n")}\r\nok\r\n`, "$I");
      } else {
        writeToSocket(socket, connId, "ok\r\n", "$I-empty");
      }
      return;
    }
    if (/^\$RI=\d+$/i.test(trimmed)) {
      if (profile.firmware !== "fluidnc") {
        writeToSocket(socket, connId, "error:3\r\n", "$RI-unsupported");
        return;
      }
      const interval = Number(trimmed.slice(4));
      if (connState.autoReportTimer) {
        clearTrackedTimer(connState.autoReportTimer);
        connState.autoReportTimer = null;
      }
      if (interval > 0) {
        const safeMs = Math.max(50, interval);
        connState.autoReportTimer = trackTimer(
          setInterval(() => {
            if (!socket.destroyed) {
              writeToSocket(socket, connId, `${formatStatusReport()}\r\n`, "auto-report");
            }
          }, safeMs),
        );
      }
      writeToSocket(socket, connId, "ok\r\n", "$RI");
      return;
    }
    if (trimmed === "$X") {
      if (machine.pins.includes("E") || machine.pins.includes("F") || machine.pins.includes("D")) {
        writeToSocket(socket, connId, "error:9\r\n", "$X-blocked-by-pin");
        return;
      }
      if (machine.state === "Alarm") machine.state = "Idle";
      writeToSocket(socket, connId, "ok\r\n", "$X");
      return;
    }
    if (trimmed === "$H") {
      if (!profile.homingEnabled) {
        machine.homingAttemptedWhenDisabled = true;
        events.push({ tsMs, relMs, connId, type: "fault", fault: "homing_disabled_error5", line: trimmed });
        writeToSocket(socket, connId, "error:5\r\n", "$H-disabled");
        return;
      }
      machine.state = "Home";
      const t = trackTimer(
        setTimeout(() => {
          timers.delete(t);
          if (socket.destroyed) return;
          machine.mpos = [Number(profile.homingMposX ?? 0), Number(profile.homingMposY ?? 0), 0];
          machine.g92Offset = [0, 0, 0];
          machine.state = "Idle";
          writeToSocket(socket, connId, "ok\r\n", "$H-ok");
        }, Math.max(5, Number(profile.ackDelayMs ?? 2))),
      );
      return;
    }

    // 报警或门开态拒绝执行 G-code
    if (machine.state === "Alarm" || machine.state.startsWith("Door")) {
      writeToSocket(socket, connId, "error:9\r\n", "rejected-alarm-or-door");
      return;
    }

    const parsed = parseGcodeLine(trimmed);
    const isSyncG4P0 = /^G4\s*P0(?:\.0*)?$/i.test(parsed.clean);
    if (parsed.words.length === 0 && !trimmed.startsWith("$")) {
      // GRBL / FluidNC 对纯注释行（;... 或 (...)）忽略并回复 ok
      if (parsed.clean === "" && (trimmed.startsWith(";") || trimmed.startsWith("("))) {
        writeToSocket(socket, connId, "ok\r\n", "comment-noop");
        return;
      }
      writeToSocket(socket, connId, "error:3\r\n", "unknown-line");
      return;
    }

    if (!isSyncG4P0 && parsed.words.length > 0) {
      machine.gcodeLinesProcessed += 1;
      if (parsed.wireN != null) {
        machine.lastWireN = parsed.wireN;
      }
    }

    const count = machine.gcodeLinesProcessed;

    // 故障注入：中途断连
    if (!isSyncG4P0 && typeof faults.disconnectAfterLines === "number" && count >= faults.disconnectAfterLines) {
      events.push({ tsMs, relMs, connId, type: "fault", fault: "disconnect", atLine: count });
      socket.destroy();
      return;
    }
    // 故障注入：中途报警
    if (!isSyncG4P0 && typeof faults.alarmAfterLines === "number" && count >= faults.alarmAfterLines) {
      const code = Number(faults.alarmCode ?? 1);
      machine.state = "Alarm";
      machine.laserState = "M5";
      machine.spindleS = 0;
      events.push({ tsMs, relMs, connId, type: "fault", fault: `ALARM:${code}`, atLine: count });
      const msg =
        profile.firmware === "fluidnc" && profile.verboseErrors !== false
          ? `[MSG:INFO: ALARM: ${code === 2 ? "Soft limit" : "Hard limit"}]\r\nALARM:${code}\r\n`
          : `ALARM:${code}\r\n`;
      writeToSocket(socket, connId, msg, `fault-alarm-${code}`);
      return;
    }
    // 故障注入：软限位报警
    if (!isSyncG4P0 && typeof faults.softLimitAfterLines === "number" && count >= faults.softLimitAfterLines) {
      machine.state = "Alarm";
      machine.laserState = "M5";
      machine.spindleS = 0;
      events.push({ tsMs, relMs, connId, type: "fault", fault: "ALARM:2", atLine: count });
      const msg =
        profile.firmware === "fluidnc" && profile.verboseErrors !== false
          ? "[MSG:INFO: ALARM: Soft limit]\r\nALARM:2\r\n"
          : "ALARM:2\r\n";
      writeToSocket(socket, connId, msg, "fault-soft-limit");
      return;
    }
    // 故障注入：门开 Pn:D
    if (!isSyncG4P0 && typeof faults.doorOpenAfterLines === "number" && count >= faults.doorOpenAfterLines) {
      machine.state = "Door:1";
      if (!machine.pins.includes("D")) machine.pins += "D";
      machine.laserState = "M5";
      machine.spindleS = 0;
      events.push({ tsMs, relMs, connId, type: "fault", fault: "Door:1(Pn:D)", atLine: count });
      writeToSocket(socket, connId, `${formatStatusReport()}\r\n`, "fault-door-open");
      return;
    }

    // 推进模态与机械坐标
    const exec = executeGcodeMotion(parsed, trimmed, connId, tsMs);
    if (exec.softLimitViolation) {
      machine.state = "Alarm";
      machine.laserState = "M5";
      machine.spindleS = 0;
      events.push({ tsMs, relMs, connId, type: "fault", fault: "ALARM:2(live-soft-limit)", line: trimmed });
      writeToSocket(socket, connId, "[MSG:INFO: ALARM: Soft limit]\r\nALARM:2\r\n", "live-soft-limit");
      return;
    }

    lineRecord.stateAfter = snapshotState();

    // 故障注入：冻结不回 ok（用于验证实时 0x18 在顺序流完全阻塞时仍可立即到达）
    if (!isSyncG4P0 && typeof faults.freezeAfterLines === "number" && count >= faults.freezeAfterLines) {
      machine.state = "Run";
      events.push({ tsMs, relMs, connId, type: "fault", fault: "freeze_ack", atLine: count });
      return;
    }

    const ackDelay = isSyncG4P0
      ? 0
      : Math.max(0, Number(faults.ackDelayMs ?? profile.ackDelayMs ?? 2));

    if (exec.moved) {
      machine.state = "Run";
    }

    const emitAck = () => {
      if (socket.destroyed) return;
      if (machine.state === "Alarm" || machine.state.startsWith("Door")) return;
      if (!isSyncG4P0) {
        machine.ackedLines += 1;
        if (parsed.wireN != null) {
          machine.ackedWireN = parsed.wireN;
          const lag = Math.max(0, Number(profile.plannerLagLines ?? 0));
          if (typeof faults.holdReportLineNumber === "number" && faults.holdReportLineNumber > 0) {
            machine.currentExecN = Math.min(parsed.wireN, faults.holdReportLineNumber);
          } else {
            machine.currentExecN = parsed.wireN > lag ? Math.max(2, parsed.wireN - lag) : 1;
          }
        }
      }
      if (connState.ackQueue.length === 0 && machine.state === "Run") {
        machine.state = "Idle";
      }
      writeToSocket(socket, connId, "ok\r\n", isSyncG4P0 ? "G4-P0-ok" : "line-ok");
    };

    // 按 FIFO 顺序排队回 ACK（贴近真实 GRBL/FluidNC 顺序解析器；G4 P0 同步排在前序行之后）
    if (ackDelay <= 0 && connState.ackQueue.length === 0 && !connState.ackTimer && !machine.state.startsWith("Hold")) {
      emitAck();
    } else {
      connState.ackQueue.push({ delayMs: ackDelay, emitAck });
      pumpAckQueue(connState);
    }
  }

  function pumpAckQueue(connState) {
    if (connState.socket.destroyed) return;
    if (connState.ackTimer || connState.ackQueue.length === 0) return;
    if (machine.state.startsWith("Hold") || machine.state === "Alarm" || machine.state.startsWith("Door")) return;

    const next = connState.ackQueue[0];
    if (next.delayMs <= 0) {
      connState.ackQueue.shift();
      next.emitAck();
      pumpAckQueue(connState);
      return;
    }
    connState.ackTimer = trackTimer(
      setTimeout(() => {
        timers.delete(connState.ackTimer);
        connState.ackTimer = null;
        if (connState.socket.destroyed) return;
        if (machine.state.startsWith("Hold") || machine.state === "Alarm" || machine.state.startsWith("Door")) {
          return;
        }
        const item = connState.ackQueue.shift();
        if (item) item.emitAck();
        pumpAckQueue(connState);
      }, next.delayMs),
    );
  }

  function drainHeldQueue(connState) {
    pumpAckQueue(connState);
  }

  const server = net.createServer((socket) => {
    const connId = ++nextConnId;
    sockets.add(socket);
    const connInfo = {
      connId,
      connectedAtMs: Date.now(),
      disconnectedAtMs: null,
      realtimeBytes: [],
      lineCount: 0,
    };
    connections.push(connInfo);
    events.push({ tsMs: connInfo.connectedAtMs, relMs: connInfo.connectedAtMs - startedAt, connId, type: "connect" });

    const connState = {
      connId,
      socket,
      buffer: "",
      ackQueue: [],
      ackTimer: null,
      autoReportTimer: null,
    };

    socket.on("close", () => {
      sockets.delete(socket);
      connInfo.disconnectedAtMs = Date.now();
      if (connState.ackTimer) {
        clearTrackedTimer(connState.ackTimer);
        connState.ackTimer = null;
      }
      connState.ackQueue.length = 0;
      if (connState.autoReportTimer) {
        clearTrackedTimer(connState.autoReportTimer);
        connState.autoReportTimer = null;
      }
      events.push({
        tsMs: connInfo.disconnectedAtMs,
        relMs: connInfo.disconnectedAtMs - startedAt,
        connId,
        type: "disconnect",
      });
    });

    socket.on("data", (chunk) => {
      const rxBuf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      recordByteLog("rx", connId, rxBuf, "rx");

      for (const b of rxBuf) {
        if (Object.hasOwn(REALTIME_BYTES, b)) {
          const tsMs = Date.now();
          const relMs = tsMs - startedAt;
          const name = REALTIME_BYTES[b];
          connInfo.realtimeBytes.push({ byte: b, name, tsMs, relMs });
          events.push({
            tsMs,
            relMs,
            connId,
            type: "realtime",
            byte: b,
            hex: `0x${b.toString(16).padStart(2, "0")}`,
            name,
            stateBefore: snapshotState(),
          });

          if (b === 0x18) {
            applySoftReset(connState);
            writeToSocket(socket, connId, `\r\n${profile.banner}\r\n`, "soft-reset-banner");
          } else if (b === 0x21) {
            if (machine.state !== "Alarm" && !machine.state.startsWith("Door")) {
              machine.state = "Hold:0";
            }
          } else if (b === 0x7e) {
            if (machine.state.startsWith("Hold")) {
              machine.state = connState.ackQueue.length > 0 ? "Run" : "Idle";
              drainHeldQueue(connState);
            }
          } else if (b === 0x3f) {
            writeToSocket(socket, connId, `${formatStatusReport()}\r\n`, "status-report");
          } else if (b === 0x85) {
            if (machine.state === "Jog") machine.state = "Idle";
          } else if (b === 0x90) {
            machine.overrides.feed = 100;
          } else if (b === 0x91) {
            machine.overrides.feed = clampOverride(machine.overrides.feed + 10);
          } else if (b === 0x92) {
            machine.overrides.feed = clampOverride(machine.overrides.feed - 10);
          } else if (b === 0x93) {
            machine.overrides.feed = clampOverride(machine.overrides.feed + 1);
          } else if (b === 0x94) {
            machine.overrides.feed = clampOverride(machine.overrides.feed - 1);
          } else if (b === 0x95) {
            machine.overrides.rapid = 100;
          } else if (b === 0x96) {
            machine.overrides.rapid = 50;
          } else if (b === 0x97) {
            machine.overrides.rapid = 25;
          } else if (b === 0x99) {
            machine.overrides.spindle = 100;
          } else if (b === 0x9a) {
            machine.overrides.spindle = clampOverride(machine.overrides.spindle + 10);
          } else if (b === 0x9b) {
            machine.overrides.spindle = clampOverride(machine.overrides.spindle - 10);
          } else if (b === 0x9c) {
            machine.overrides.spindle = clampOverride(machine.overrides.spindle + 1);
          } else if (b === 0x9d) {
            machine.overrides.spindle = clampOverride(machine.overrides.spindle - 1);
          } else if (b === 0xa0) {
            machine.airAssist = !machine.airAssist;
          }
          continue;
        }
        connState.buffer += String.fromCharCode(b);
      }

      let nl;
      while ((nl = connState.buffer.indexOf("\n")) >= 0) {
        const line = connState.buffer.slice(0, nl).replace(/\r$/, "");
        connState.buffer = connState.buffer.slice(nl + 1);
        if (line.trim().length > 0) {
          connInfo.lineCount += 1;
          handleSequentialLine(connState, line);
        }
      }
    });

    socket.on("error", () => {});
  });

  function formatSerialLog() {
    return serialLog
      .map((entry) => {
        const ts = new Date(entry.tsMs).toISOString();
        const rel = `+${String(entry.relMs).padStart(7, "0")}ms`;
        const dir = entry.dir.toUpperCase().padEnd(2, " ");
        const note = entry.note ? ` [${entry.note}]` : "";
        return `${ts} (${rel}) [conn#${entry.connId}] ${dir} (${entry.byteCount}B) hex=${entry.hex} ascii="${entry.ascii}"${note}`;
      })
      .join("\n");
  }

  return new Promise((resolvePromise, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      resolvePromise({
        host: "127.0.0.1",
        port: addr.port,
        profile,
        get events() {
          return events;
        },
        get lines() {
          return lines;
        },
        get segments() {
          return segments;
        },
        get serialLog() {
          return serialLog;
        },
        get connections() {
          return connections;
        },
        snapshot() {
          return {
            profile: { ...profile },
            machine: snapshotState(),
            travelBounds: computeTravelBounds(profile),
            events: [...events],
            lines: [...lines],
            segments: [...segments],
            connections: connections.map((c) => ({ ...c, realtimeBytes: [...c.realtimeBytes] })),
            serialLogCount: serialLog.length,
          };
        },
        setFaults(nextFaults = {}) {
          faults = { ...nextFaults };
        },
        setMpos(x, y, z = 0) {
          machine.mpos = [Number(x), Number(y), Number(z)];
        },
        setState(nextState, nextPins = machine.pins) {
          machine.state = nextState;
          machine.pins = nextPins;
        },
        resetRecording() {
          events = [];
          lines = [];
          segments = [];
          connections = [];
          machine.gcodeLinesProcessed = 0;
          machine.ackedLines = 0;
          machine.lastWireN = null;
          machine.ackedWireN = null;
          machine.currentExecN = null;
          machine.homingAttemptedWhenDisabled = false;
        },
        formatSerialLog,
        close() {
          for (const t of [...timers]) clearTrackedTimer(t);
          for (const s of sockets) s.destroy();
          return new Promise((closed) => server.close(() => closed()));
        },
      });
    });
  });
}
