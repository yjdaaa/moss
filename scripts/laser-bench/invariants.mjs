// 激光雕刻机 AI 自主测试不变量检查引擎（纯函数，便于单测喂入构造字节流）。
// 严重程度：
// - "safety"：安全类违反（最高优先级，必须非 0 退出）
// - "error" ：功能/门禁类失败（非 0 退出）
// - "warn"  ：告警（需要留意）
// - "info"  ：提示（例如工时估算偏差较大，不阻断退出码）

import { createHash } from "node:crypto";
import { isOverrideRealtimeByte, parseGcodeLine, stripGcodeComments } from "./sim-grbl.mjs";

export const SEVERITY_RANK = Object.freeze({
  safety: 0,
  error: 1,
  warn: 2,
  info: 3,
});

function result(id, severity, status, title, message, { evidence = null, suspectedCode = null } = {}) {
  return { id, severity, status, title, message, evidence, suspectedCode };
}

function isGcodeMotionLine(parsed) {
  const hasAxis = parsed.words.some((w) => w.letter === "X" || w.letter === "Y" || w.letter === "Z");
  const hasMotionG = parsed.words.some(
    (w) => w.letter === "G" && [0, 1, 2, 3].includes(Math.round(w.value)),
  );
  return hasAxis || hasMotionG;
}

function hasExplicitM5OrS0(parsed) {
  return parsed.words.some(
    (w) =>
      (w.letter === "M" && Math.round(w.value) === 5) ||
      (w.letter === "S" && Math.abs(w.value) < 1e-9),
  );
}

/**
 * 不变量 1（安全）：移动与走边框（无光模式）之前必须先关光（M5 或 S0），且过程中不得出光。
 */
export function checkLaserOffBeforeMoveOrFraming(trace) {
  const kind = trace?.kind || "";
  const isMoveOrUnlitFraming =
    kind === "relative_move" ||
    kind === "absolute_move" ||
    (kind === "framing" && !(trace?.framingPower > 0));
  if (!isMoveOrUnlitFraming) {
    return result(
      "laser_off_before_move_or_framing",
      "safety",
      "skip",
      "移动/走边框前先关光",
      "当前场景非移动或无光走边框",
    );
  }
  const seqLines = (trace.lines || []).filter((l) => !String(l.line || "").trim().startsWith("$"));
  if (seqLines.length === 0) {
    return result(
      "laser_off_before_move_or_framing",
      "safety",
      "fail",
      "移动/走边框前先关光",
      "未收到任何 G-code 指令，无法证实移动前已关光",
      { evidence: { lineCount: 0 }, suspectedCode: "crates/laser-tools/src/lib.rs" },
    );
  }

  let sawLaserOff = false;
  let laserState = "UNKNOWN";
  let spindleS = null;
  for (let i = 0; i < seqLines.length; i += 1) {
    const rec = seqLines[i];
    const parsed = parseGcodeLine(rec.line);
    if (hasExplicitM5OrS0(parsed)) sawLaserOff = true;
    for (const w of parsed.words) {
      if (w.letter === "M") {
        const c = Math.round(w.value);
        if (c === 5) {
          laserState = "M5";
          spindleS = 0;
        } else if (c === 3 || c === 4) {
          laserState = `M${c}`;
        }
      } else if (w.letter === "S") {
        spindleS = w.value;
      }
    }
    const isMotion = isGcodeMotionLine(parsed);
    const isLaserOn = (laserState === "M3" || laserState === "M4") && (spindleS ?? 0) > 0;
    if (isMotion && !sawLaserOff) {
      return result(
        "laser_off_before_move_or_framing",
        "safety",
        "fail",
        "移动/走边框前先关光",
        `第 ${i + 1} 行出现运动指令 \`${rec.line}\`，此前未发送 M5 或 S0 关光`,
        {
          evidence: { lineIndex: i + 1, line: rec.line, tsMs: rec.tsMs ?? null },
          suspectedCode: "crates/laser-tools/src/lib.rs / crates/laser-api/src/safe_action.rs",
        },
      );
    }
    if (isLaserOn) {
      return result(
        "laser_off_before_move_or_framing",
        "safety",
        "fail",
        "移动/走边框前先关光",
        `无光移动/走边框中出现非零出光指令 \`${rec.line}\`（${laserState} S${spindleS}）`,
        {
          evidence: { lineIndex: i + 1, line: rec.line, laserState, spindleS, tsMs: rec.tsMs ?? null },
          suspectedCode: "crates/laser-api/src/safe_action.rs",
        },
      );
    }
  }

  return result(
    "laser_off_before_move_or_framing",
    "safety",
    "pass",
    "移动/走边框前先关光",
    "首次运动前已明确发送 M5/S0，且全程无出光指令",
  );
}

/**
 * 不变量 2（安全）：任务结束、取消或急停后激光必须处于关闭状态；0x18 后不得继续下发刀路行。
 */
export function checkLaserOffAfterTerminal(trace) {
  const events = trace?.events || [];
  const lines = (trace?.lines || []).filter((l) => !String(l.line || "").trim().startsWith("$"));
  if (events.length === 0 && lines.length === 0) {
    return result("laser_off_after_terminal", "safety", "skip", "终态后激光关闭", "无设备交互记录");
  }

  // 检查是否存在 0x18 软复位事件：若存在，复位后同任务连接不得再下发刀路行
  const resetEvents = events.filter((e) => e.type === "realtime" && e.byte === 0x18);
  const terminalMode = trace?.terminalMode || "completed"; // completed | cancelled | estop | fault
  if (terminalMode === "cancelled" || terminalMode === "estop") {
    if (resetEvents.length === 0) {
      return result(
        "laser_off_after_terminal",
        "safety",
        "fail",
        "终态后激光关闭",
        `任务终态为 ${terminalMode}，但模拟器未收到 0x18 软复位字节`,
        {
          evidence: { terminalMode, realtimeCount: events.filter((e) => e.type === "realtime").length },
          suspectedCode: "crates/laser-device/src/connection.rs / crates/laser-api/src/safe_action.rs",
        },
      );
    }
    const lastReset = resetEvents[resetEvents.length - 1];
    const resetIdx = events.indexOf(lastReset);
    const linesAfterReset = events
      .slice(resetIdx + 1)
      .filter((e) => e.type === "line" && e.connId === lastReset.connId && !String(e.line || "").startsWith("$"));
    if (linesAfterReset.length > 0) {
      const first = linesAfterReset[0];
      return result(
        "laser_off_after_terminal",
        "safety",
        "fail",
        "终态后激光关闭",
        `0x18 软复位后同连接仍下发了刀路行 \`${first.line}\``,
        {
          evidence: { resetTsMs: lastReset.tsMs, afterLine: first.line, afterTsMs: first.tsMs, connId: first.connId },
          suspectedCode: "crates/laser-device/src/connection/job.rs",
        },
      );
    }
    return result(
      "laser_off_after_terminal",
      "safety",
      "pass",
      "终态后激光关闭",
      `${terminalMode} 后已收到 0x18 软复位且同连接未再下发刀路行`,
    );
  }

  // 正常完成或故障：检查顺序模态最终状态（或收到 0x18 兜底）
  if (terminalMode === "fault") {
    if (resetEvents.length > 0 || trace?.machineAfter?.laserState === "M5" || (trace?.machineAfter?.spindleS ?? 0) === 0) {
      return result(
        "laser_off_after_terminal",
        "safety",
        "pass",
        "终态后激光关闭",
        resetEvents.length > 0 ? "故障后已补发 0x18 且激光关闭" : "故障后激光处于关闭态",
      );
    }
    return result(
      "laser_off_after_terminal",
      "safety",
      "fail",
      "终态后激光关闭",
      "故障中止后未收到 0x18 且激光仍处于非关闭态",
      { evidence: { machineAfter: trace?.machineAfter }, suspectedCode: "crates/laser-api/src/send.rs" },
    );
  }

  // completed: 从线序重放 M3/M4/M5/S 状态（忽略结尾的 G4 P0）
  let laserState = "M5";
  let spindleS = 0;
  let lastLine = "";
  for (const rec of lines) {
    const parsed = parseGcodeLine(rec.line);
    if (/^G4\s*P0(?:\.0*)?$/i.test(parsed.clean)) continue;
    lastLine = rec.line;
    for (const w of parsed.words) {
      if (w.letter === "M") {
        const c = Math.round(w.value);
        if (c === 5) {
          laserState = "M5";
          spindleS = 0;
        } else if (c === 3 || c === 4) {
          laserState = `M${c}`;
        }
      } else if (w.letter === "S") {
        spindleS = w.value;
      }
    }
  }
  if (laserState !== "M5" && spindleS > 0) {
    return result(
      "laser_off_after_terminal",
      "safety",
      "fail",
      "终态后激光关闭",
      `任务完成后激光未关闭（最终状态 ${laserState} S${spindleS}，末行 \`${lastLine}\`）`,
      {
        evidence: { laserState, spindleS, lastLine },
        suspectedCode: "crates/laser-core/src/gcode/writer.rs / crates/laser-device/src/connection/job.rs",
      },
    );
  }
  return result("laser_off_after_terminal", "safety", "pass", "终态后激光关闭", `任务完成后激光已关闭（${laserState} S${spindleS}）`);
}

/**
 * 不变量 3（安全）：进入相对坐标模式（G91）后必须恢复绝对模式（G90）。
 */
export function checkG90RestoredAfterRelativeMove(trace) {
  const lines = (trace?.lines || []).filter((l) => !String(l.line || "").trim().startsWith("$"));
  if (lines.length === 0) {
    return result("g90_restored_after_relative_move", "safety", "skip", "相对移动后恢复 G90", "无 G-code 行");
  }
  let sawG91 = false;
  let distanceMode = "G90";
  let lastG91Line = null;
  for (let i = 0; i < lines.length; i += 1) {
    const parsed = parseGcodeLine(lines[i].line);
    for (const w of parsed.words) {
      if (w.letter === "G") {
        const c = Math.round(w.value);
        if (c === 91) {
          sawG91 = true;
          distanceMode = "G91";
          lastG91Line = { lineIndex: i + 1, line: lines[i].line, tsMs: lines[i].tsMs ?? null };
        } else if (c === 90) {
          distanceMode = "G90";
        }
      }
    }
  }
  if (!sawG91) {
    if (trace?.kind === "relative_move") {
      return result(
        "g90_restored_after_relative_move",
        "safety",
        "fail",
        "相对移动后恢复 G90",
        "相对移动场景未检测到 G91 指令",
        { evidence: { lines: lines.map((l) => l.line) }, suspectedCode: "crates/laser-tools/src/lib.rs" },
      );
    }
    return result("g90_restored_after_relative_move", "safety", "skip", "相对移动后恢复 G90", "未涉及 G91 相对移动");
  }
  // 0x18 软复位会自动重置控制器到 G90
  const hadReset = (trace?.events || []).some((e) => e.type === "realtime" && e.byte === 0x18);
  if (distanceMode !== "G90" && !hadReset) {
    return result(
      "g90_restored_after_relative_move",
      "safety",
      "fail",
      "相对移动后恢复 G90",
      `在 \`${lastG91Line.line}\` 切换至 G91 后结束时仍停留在 G91，未恢复 G90`,
      {
        evidence: { lastG91Line, finalDistanceMode: distanceMode },
        suspectedCode: "crates/laser-tools/src/lib.rs (relative_move)",
      },
    );
  }
  return result("g90_restored_after_relative_move", "safety", "pass", "相对移动后恢复 G90", "G91 相对移动后已恢复 G90 绝对坐标模式");
}

/**
 * 不变量 4（安全）：所有下发到模拟器的 S 值不得超过 $30 换算上限（且不得为负）。
 */
export function checkSpindleSWithinDollar30(trace) {
  const maxS = Number(trace?.profile?.maxSpindleSpeed);
  if (!Number.isFinite(maxS) || maxS <= 0) {
    return result("spindle_s_within_dollar30", "safety", "skip", "S 值不超过 $30 上限", "未提供有效 $30 配置");
  }
  const lines = (trace?.lines || []).filter((l) => !String(l.line || "").trim().startsWith("$"));
  if (lines.length === 0) {
    return result("spindle_s_within_dollar30", "safety", "skip", "S 值不超过 $30 上限", "无顺序 G-code 行");
  }
  let observedMaxS = 0;
  let observedCount = 0;
  for (let i = 0; i < lines.length; i += 1) {
    const rec = lines[i];
    const parsed = parseGcodeLine(rec.line);
    for (const w of parsed.words) {
      if (w.letter === "S") {
        observedCount += 1;
        if (w.value > observedMaxS) observedMaxS = w.value;
        if (w.value < 0 || w.value > maxS + 1e-6) {
          return result(
            "spindle_s_within_dollar30",
            "safety",
            "fail",
            "S 值不超过 $30 上限",
            `第 ${i + 1} 行 \`${rec.line}\` 含 S${w.value}，超出固件 $30=${maxS} 上限`,
            {
              evidence: { lineIndex: i + 1, line: rec.line, sValue: w.value, dollar30: maxS, tsMs: rec.tsMs ?? null },
              suspectedCode: "crates/laser-device/src/connection/job.rs (rescale_gcode_line_s)",
            },
          );
        }
      }
    }
  }
  return result(
    "spindle_s_within_dollar30",
    "safety",
    "pass",
    "S 值不超过 $30 上限",
    `共检查 ${observedCount} 处 S 指令，最高 S${observedMaxS} <= $30(${maxS})`,
  );
}

/**
 * 不变量 5（安全）：固件报告了行程时，出光段的范围加上起点（机械坐标）不超过固件行程。
 */
export function checkLaserOnWithinFirmwareTravel(trace) {
  const profile = trace?.profile || {};
  if (profile.omitTravel || (profile.travelX == null && profile.travelY == null)) {
    return result("laser_on_within_firmware_travel", "safety", "skip", "出光范围不超固件行程", "固件未报告行程，跳过");
  }
  const laserSegments = (trace?.segments || []).filter((s) => s.laserOn);
  if (laserSegments.length === 0) {
    return result("laser_on_within_firmware_travel", "safety", "skip", "出光范围不超固件行程", "无出光运动段");
  }

  const TOL = 0.05;
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  let worstSeg = null;

  for (const seg of laserSegments) {
    for (const pt of [seg.fromMpos, seg.toMpos]) {
      if (!Array.isArray(pt) || pt.length < 2) continue;
      if (pt[0] < minX) minX = pt[0];
      if (pt[0] > maxX) maxX = pt[0];
      if (pt[1] < minY) minY = pt[1];
      if (pt[1] > maxY) maxY = pt[1];
    }
  }

  const spanX = maxX - minX;
  const spanY = maxY - minY;
  if (profile.travelX != null && spanX > profile.travelX + TOL) {
    return result(
      "laser_on_within_firmware_travel",
      "safety",
      "fail",
      "出光范围不超固件行程",
      `出光段 X 跨度 ${spanX.toFixed(2)} mm 超出固件 X 行程 ${profile.travelX} mm`,
      {
        evidence: { minX, maxX, spanX, travelX: profile.travelX },
        suspectedCode: "crates/laser-api/src/generation/mod.rs (check_job_fits_machine)",
      },
    );
  }
  if (profile.travelY != null && spanY > profile.travelY + TOL) {
    return result(
      "laser_on_within_firmware_travel",
      "safety",
      "fail",
      "出光范围不超固件行程",
      `出光段 Y 跨度 ${spanY.toFixed(2)} mm 超出固件 Y 行程 ${profile.travelY} mm`,
      {
        evidence: { minY, maxY, spanY, travelY: profile.travelY },
        suspectedCode: "crates/laser-api/src/generation/mod.rs (check_job_fits_machine)",
      },
    );
  }

  // 若给出了固件机械坐标边界（起点 + 相对出光段 = 机械坐标 MPos），且并非刻意测试越界警告场景，复核绝对区间
  const bounds = trace?.travelBounds;
  if (bounds && !trace?.allowMposEdgeOverflow) {
    for (const seg of laserSegments) {
      for (const pt of [seg.fromMpos, seg.toMpos]) {
        if (bounds.x && (pt[0] < bounds.x[0] - TOL || pt[0] > bounds.x[1] + TOL)) {
          worstSeg = { axis: "X", value: pt[0], bound: bounds.x, line: seg.line, tsMs: seg.tsMs ?? null };
          break;
        }
        if (bounds.y && (pt[1] < bounds.y[0] - TOL || pt[1] > bounds.y[1] + TOL)) {
          worstSeg = { axis: "Y", value: pt[1], bound: bounds.y, line: seg.line, tsMs: seg.tsMs ?? null };
          break;
        }
      }
      if (worstSeg) break;
    }
    if (worstSeg) {
      return result(
        "laser_on_within_firmware_travel",
        "safety",
        "fail",
        "出光范围不超固件行程",
        `出光段机械坐标 ${worstSeg.axis}=${worstSeg.value.toFixed(2)} mm 超出固件区间 [${worstSeg.bound[0]}, ${worstSeg.bound[1]}]（行 \`${worstSeg.line}\`）`,
        {
          evidence: { worstSeg, minX, maxX, minY, maxY, bounds },
          suspectedCode: "crates/laser-api/src/generation/mod.rs / crates/laser-api/src/send.rs",
        },
      );
    }
  }

  return result(
    "laser_on_within_firmware_travel",
    "safety",
    "pass",
    "出光范围不超固件行程",
    `出光段范围 X:[${minX.toFixed(1)}, ${maxX.toFixed(1)}] (${spanX.toFixed(1)}mm) Y:[${minY.toFixed(1)}, ${maxY.toFixed(1)}] (${spanY.toFixed(1)}mm) 在行程内`,
  );
}

/**
 * 不变量 6（安全）：急停字节 0x18 必须走实时通道，在发出后极短时间内到达模拟器且不被排队 G-code 阻塞。
 */
export function checkEstopRealtimeNotQueued(trace) {
  const estop = trace?.estopInfo;
  if (!estop) {
    return result("estop_realtime_not_queued", "safety", "skip", "急停 0x18 实时直达不排队", "非急停场景");
  }
  const thresholdMs = Number(estop.maxLatencyMs ?? 500);
  const resetEvents = (trace.events || []).filter((e) => e.type === "realtime" && e.byte === 0x18);
  const hit = resetEvents.find((e) => e.tsMs >= estop.requestedAtMs - 5);
  if (!hit) {
    return result(
      "estop_realtime_not_queued",
      "safety",
      "fail",
      "急停 0x18 实时直达不排队",
      "触发急停后模拟器未收到 0x18 实时字节",
      {
        evidence: { requestedAtMs: estop.requestedAtMs, resetEventsCount: resetEvents.length },
        suspectedCode: "crates/laser-device/src/transport.rs / crates/laser-api/src/safe_action.rs",
      },
    );
  }
  const latencyMs = Math.max(0, hit.tsMs - estop.requestedAtMs);
  if (latencyMs > thresholdMs) {
    return result(
      "estop_realtime_not_queued",
      "safety",
      "fail",
      "急停 0x18 实时直达不排队",
      `0x18 到达耗时 ${latencyMs} ms，超过实时通道阈值 ${thresholdMs} ms（疑似被顺序 G-code 队列阻塞）`,
      {
        evidence: { latencyMs, thresholdMs, requestedAtMs: estop.requestedAtMs, arrivedAtMs: hit.tsMs, connId: hit.connId },
        suspectedCode: "crates/laser-device/src/transport.rs (write_realtime)",
      },
    );
  }
  if (estop.jobConnId != null && hit.connId !== estop.jobConnId) {
    return result(
      "estop_realtime_not_queued",
      "safety",
      "fail",
      "急停 0x18 实时直达不排队",
      `0x18 发往了新连接 conn#${hit.connId} 而非在途任务连接 conn#${estop.jobConnId}`,
      {
        evidence: { expectedConnId: estop.jobConnId, actualConnId: hit.connId, latencyMs },
        suspectedCode: "crates/laser-api/src/safe_action.rs",
      },
    );
  }
  if (typeof estop.totalLines === "number" && typeof estop.linesReceivedBeforeReset === "number") {
    if (estop.totalLines >= 20 && estop.linesReceivedBeforeReset >= estop.totalLines) {
      return result(
        "estop_realtime_not_queued",
        "safety",
        "fail",
        "急停 0x18 实时直达不排队",
        `0x18 在全部 ${estop.totalLines} 行发完后才到达，未能打断排队发送`,
        {
          evidence: { totalLines: estop.totalLines, linesReceivedBeforeReset: estop.linesReceivedBeforeReset, latencyMs },
          suspectedCode: "crates/laser-device/src/connection/job.rs",
        },
      );
    }
  }
  return result(
    "estop_realtime_not_queued",
    "safety",
    "pass",
    "急停 0x18 实时直达不排队",
    `0x18 经原任务通道（conn#${hit.connId}）在 ${latencyMs} ms 内送达（阈值 ${thresholdMs} ms）`,
    { evidence: { latencyMs, connId: hit.connId } },
  );
}

/**
 * 不变量 7（安全）：发送的物理 G-code 文件 SHA-256 必须与预览记录及确认快照一致。
 */
export function checkGcodeSha256MatchesPreview(trace) {
  const preview = trace?.preview;
  if (!preview || !preview.expectedSha256) {
    return result("gcode_sha256_matches_preview", "safety", "skip", "物理文件 SHA-256 与预览一致", "无预览 SHA 记录");
  }
  const expected = String(preview.expectedSha256).trim().toLowerCase();
  const actual = preview.actualSha256
    ? String(preview.actualSha256).trim().toLowerCase()
    : preview.gcodeBytes
      ? createHash("sha256").update(preview.gcodeBytes).digest("hex")
      : "";
  if (!actual || actual.length !== 64) {
    return result(
      "gcode_sha256_matches_preview",
      "safety",
      "fail",
      "物理文件 SHA-256 与预览一致",
      "未能读取物理 G-code 文件计算 SHA-256",
      { evidence: { expected, actual }, suspectedCode: "crates/laser-api/src/send.rs" },
    );
  }
  if (actual !== expected) {
    return result(
      "gcode_sha256_matches_preview",
      "safety",
      "fail",
      "物理文件 SHA-256 与预览一致",
      `物理文件 SHA-256（${actual.slice(0, 12)}…）与预览冻结值（${expected.slice(0, 12)}…）不一致`,
      { evidence: { expected, actual }, suspectedCode: "crates/laser-api/src/send.rs (read_and_verify_gcode)" },
    );
  }
  return result(
    "gcode_sha256_matches_preview",
    "safety",
    "pass",
    "物理文件 SHA-256 与预览一致",
    `物理 G-code SHA-256 校验一致（${actual.slice(0, 16)}…）`,
  );
}

/**
 * 不变量 8（安全）：续雕起始行必须等于 resumable_from_line + 1，且不得重复出光已确认完成的行。
 */
export function checkResumeStartLineAndNoDuplicateBurn(trace) {
  const resume = trace?.resumeInfo;
  if (!resume) {
    return result("resume_start_line_and_no_duplicate_burn", "safety", "skip", "续雕起始行正确且不重复出光", "非续雕场景");
  }
  const {
    resumableFromLine,
    linesCompleted,
    resumeBoundarySource,
    preparedLines = [],
    resumeWireLines = [],
    resumeSegments = [],
  } = resume;

  if (resumeBoundarySource !== "device_hold_ln") {
    return result(
      "resume_start_line_and_no_duplicate_burn",
      "safety",
      "fail",
      "续雕起始行正确且不重复出光",
      `断点来源应为 device_hold_ln，实际为 ${String(resumeBoundarySource)}`,
      { evidence: { resumeBoundarySource, resumableFromLine, linesCompleted }, suspectedCode: "crates/laser-api/src/send.rs" },
    );
  }
  if (!(resumableFromLine > 0) || resumableFromLine !== linesCompleted) {
    return result(
      "resume_start_line_and_no_duplicate_burn",
      "safety",
      "fail",
      "续雕起始行正确且不重复出光",
      `resumable_from_line (${resumableFromLine}) 与设备证实完成行 lines_completed (${linesCompleted}) 不一致或 <= 0`,
      { evidence: { resumableFromLine, linesCompleted }, suspectedCode: "crates/laser-api/src/send.rs" },
    );
  }
  if (preparedLines.length <= resumableFromLine) {
    return result(
      "resume_start_line_and_no_duplicate_burn",
      "safety",
      "fail",
      "续雕起始行正确且不重复出光",
      `总准备行数 ${preparedLines.length} <= 断点行 ${resumableFromLine}`,
      { evidence: { preparedTotal: preparedLines.length, resumableFromLine } },
    );
  }

  // 从续雕阶段下发的行中剥掉系统指令($...)与 N<k> 前缀，定位第一条 N1 行（JobWorker 续发从 N1 开始编号）
  const nonSys = resumeWireLines.filter((l) => !String(l.line || "").trim().startsWith("$"));
  const firstNumberedIdx = nonSys.findIndex((l) => /^N1\s+/i.test(String(l.line || "").trim()));
  const expectedFirstLine = stripGcodeComments(preparedLines[resumableFromLine]);

  // 检查模态重建头（firstNumberedIdx 之前）严禁出光！
  const preambleLines = firstNumberedIdx > 0 ? nonSys.slice(0, firstNumberedIdx) : [];
  let preLaser = "M5";
  let preS = 0;
  for (const rec of preambleLines) {
    const parsed = parseGcodeLine(rec.line);
    for (const w of parsed.words) {
      if (w.letter === "M") {
        const c = Math.round(w.value);
        if (c === 5) {
          preLaser = "M5";
          preS = 0;
        } else if (c === 3 || c === 4) {
          preLaser = `M${c}`;
        }
      } else if (w.letter === "S") {
        preS = w.value;
      }
    }
    if (isGcodeMotionLine(parsed) && (preLaser === "M3" || preLaser === "M4") && preS > 0) {
      return result(
        "resume_start_line_and_no_duplicate_burn",
        "safety",
        "fail",
        "续雕起始行正确且不重复出光",
        `续雕预定位模态头在激光开启（${preLaser} S${preS}）状态下移动：\`${rec.line}\``,
        { evidence: { line: rec.line, preLaser, preS }, suspectedCode: "crates/laser-api/src/resume.rs" },
      );
    }
  }

  // 检查续雕正文起始行是否对应 preparedLines[resumableFromLine]（即 1-based 第 resumableFromLine + 1 行）
  const bodyLines = firstNumberedIdx >= 0 ? nonSys.slice(firstNumberedIdx) : nonSys;
  if (bodyLines.length === 0) {
    return result(
      "resume_start_line_and_no_duplicate_burn",
      "safety",
      "fail",
      "续雕起始行正确且不重复出光",
      "续雕阶段未下发任何正文 G-code 行",
      { evidence: { nonSysCount: nonSys.length }, suspectedCode: "crates/laser-api/src/send.rs" },
    );
  }
  const actualFirstRaw = String(bodyLines[0].line || "").trim().replace(/^N\d+\s+/i, "");
  // 首条切割行可能由 apply_modal_spindle_to_remaining 追加了 S 值，故比较核心动作/坐标前缀一致
  const normalizeForCompare = (s) =>
    stripGcodeComments(s)
      .replace(/\s+/g, " ")
      .replace(/\s+S\d+(?:\.\d+)?$/i, "")
      .trim()
      .toUpperCase();
  const normExpected = normalizeForCompare(expectedFirstLine);
  const normActual = normalizeForCompare(actualFirstRaw);
  if (normActual !== normExpected) {
    return result(
      "resume_start_line_and_no_duplicate_burn",
      "safety",
      "fail",
      "续雕起始行正确且不重复出光",
      `续雕首条正文行应为第 ${resumableFromLine + 1} 行 \`${expectedFirstLine}\`，实际收到 \`${actualFirstRaw}\``,
      {
        evidence: {
          expectedOneBasedLine: resumableFromLine + 1,
          expectedFirstLine,
          actualFirstRaw,
          resumableFromLine,
        },
        suspectedCode: "crates/laser-api/src/send.rs / crates/laser-api/src/resume.rs",
      },
    );
  }

  // 检查续雕剩余行数（去除末尾 G4 P0）不超过总行数 - resumableFromLine，确保未重复下发已完成行
  const bodyWithoutSync = bodyLines.filter((l) => !/^(?:N\d+\s+)?G4\s*P0(?:\.0*)?$/i.test(String(l.line || "").trim()));
  const expectedRemainingCount = preparedLines.length - resumableFromLine;
  if (bodyWithoutSync.length > expectedRemainingCount) {
    return result(
      "resume_start_line_and_no_duplicate_burn",
      "safety",
      "fail",
      "续雕起始行正确且不重复出光",
      `续雕下发正文行数 ${bodyWithoutSync.length} 超过剩余行数 ${expectedRemainingCount}，存在重复下发已完成行风险`,
      {
        evidence: {
          sentBodyLines: bodyWithoutSync.length,
          expectedRemainingCount,
          resumableFromLine,
          resumeLaserSegments: resumeSegments.filter((s) => s.laserOn).length,
        },
        suspectedCode: "crates/laser-api/src/send.rs",
      },
    );
  }

  return result(
    "resume_start_line_and_no_duplicate_burn",
    "safety",
    "pass",
    "续雕起始行正确且不重复出光",
    `续雕从第 ${resumableFromLine + 1} 行（\`${actualFirstRaw}\`）精确接续，共续发 ${bodyWithoutSync.length}/${expectedRemainingCount} 行，预定位全程关光`,
  );
}

/**
 * 不变量 9（安全）：倍率实时字节（0x90–0x9D）只能在运行中任务期间发出，且必须走原任务同一条连接通道。
 */
export function checkOverrideOnlyDuringActiveJobSameChannel(trace) {
  const overrideEvents = (trace?.events || []).filter(
    (e) => e.type === "realtime" && isOverrideRealtimeByte(e.byte),
  );
  const info = trace?.overrideInfo;
  if (!info && overrideEvents.length === 0) {
    return result(
      "override_only_during_active_job_same_channel",
      "safety",
      "skip",
      "倍率字节仅在运行中任务原通道发送",
      "当前场景不涉及实时倍率",
    );
  }

  // 无活动任务时禁止发出任何倍率字节
  if (info?.idleRejectedExpected) {
    if (info.idleOverrideBytesCount > 0) {
      return result(
        "override_only_during_active_job_same_channel",
        "safety",
        "fail",
        "倍率字节仅在运行中任务原通道发送",
        `无运行中任务时仍向设备发出了 ${info.idleOverrideBytesCount} 个倍率字节`,
        {
          evidence: { idleOverrideBytesCount: info.idleOverrideBytesCount, idleResponses: info.idleResponses },
          suspectedCode: "crates/laser-api/src/safe_action.rs (handle_safe_action_with_override_job)",
        },
      );
    }
    if (info.idleAllRejected === false) {
      return result(
        "override_only_during_active_job_same_channel",
        "safety",
        "fail",
        "倍率字节仅在运行中任务原通道发送",
        "无运行中任务时倍率接口未拒绝请求",
        { evidence: { idleResponses: info.idleResponses }, suspectedCode: "crates/laser-api/src/safe_action.rs" },
      );
    }
  }

  // 运行中任务窗口与原连接通道核对
  const activeWindows = Array.isArray(info?.activeWindows) ? info.activeWindows : [];
  for (const ev of overrideEvents) {
    const matched = activeWindows.find(
      (w) =>
        ev.connId === w.connId &&
        ev.tsMs >= w.startMs - 5 &&
        (w.endMs == null || ev.tsMs <= w.endMs + 50),
    );
    if (!matched) {
      return result(
        "override_only_during_active_job_same_channel",
        "safety",
        "fail",
        "倍率字节仅在运行中任务原通道发送",
        `倍率字节 ${ev.name} 出现在非法通道 conn#${ev.connId} 或非运行时间窗（ts=${ev.tsMs}）`,
        {
          evidence: { event: ev, activeWindows },
          suspectedCode: "crates/laser-api/src/safe_action.rs (handle_safe_action_with_override_job)",
        },
      );
    }
  }

  if (info?.expectActiveOverrideCount > 0 && overrideEvents.length < info.expectActiveOverrideCount) {
    return result(
      "override_only_during_active_job_same_channel",
      "error",
      "fail",
      "倍率字节仅在运行中任务原通道发送",
      `预期运行中收到至少 ${info.expectActiveOverrideCount} 次倍率字节，实际收到 ${overrideEvents.length} 次`,
      { evidence: { received: overrideEvents.length, expected: info.expectActiveOverrideCount } },
    );
  }

  return result(
    "override_only_during_active_job_same_channel",
    "safety",
    "pass",
    "倍率字节仅在运行中任务原通道发送",
    `无任务时全部拒绝（0 字节）；运行中共 ${overrideEvents.length} 次倍率字节全部命中原任务通道`,
  );
}

/**
 * 不变量 10（安全）：固件报告 $22=0（未开启机械回零）时，后端应禁用/拦截 $H，不应向控制器发送 $H。
 */
export function checkHomingDisabledWhenDollar22Zero(trace) {
  const homing = trace?.homingInfo;
  if (!homing || homing.homingEnabled !== false) {
    return result("homing_disabled_when_dollar22_zero", "safety", "skip", "$22=0 时禁用机械回零", "当前场景固件开启了回零或未测回零");
  }
  const sentHomeLine = (trace.lines || []).find((l) => String(l.line || "").trim() === "$H");
  if (homing.probeReportedHomingEnabled !== false) {
    return result(
      "homing_disabled_when_dollar22_zero",
      "safety",
      "fail",
      "$22=0 时禁用机械回零",
      `固件配置 $22=0，但握手探测返回的 homing_enabled=${String(homing.probeReportedHomingEnabled)}`,
      {
        evidence: { probeReportedHomingEnabled: homing.probeReportedHomingEnabled },
        suspectedCode: "crates/laser-device/src/protocol.rs / crates/laser-api/src/connection.rs",
      },
    );
  }
  if (sentHomeLine) {
    return result(
      "homing_disabled_when_dollar22_zero",
      "safety",
      "fail",
      "$22=0 时禁用机械回零",
      "固件已明确报告 $22=0，但后端 /api/safe-action(home) 未在握手预检阶段拦截，仍向控制器发送了 `$H`",
      {
        evidence: {
          sentLine: sentHomeLine.line,
          tsMs: sentHomeLine.tsMs ?? null,
          httpStatus: homing.homeHttpStatus,
          httpBody: homing.homeResponse,
        },
        suspectedCode: "crates/laser-tools/src/lib.rs (execute_laser_safe_action_with_gate_cancel_and_dispatch) / crates/laser-device/src/connection.rs (home_cancellable)",
      },
    );
  }
  if (homing.homeHttpStatus && homing.homeHttpStatus < 400) {
    return result(
      "homing_disabled_when_dollar22_zero",
      "safety",
      "fail",
      "$22=0 时禁用机械回零",
      `固件 $22=0 时机械回零接口返回 HTTP ${homing.homeHttpStatus}（应拒绝）`,
      {
        evidence: { httpStatus: homing.homeHttpStatus, httpBody: homing.homeResponse },
        suspectedCode: "crates/laser-api/src/safe_action.rs",
      },
    );
  }
  return result(
    "homing_disabled_when_dollar22_zero",
    "safety",
    "pass",
    "$22=0 时禁用机械回零",
    "握手正确报告 homing_enabled=false，且后端在发送 $H 前即拦截",
  );
}

/**
 * 不变量 11（提示，不阻断退出码）：工时估算与模拟物理执行时间的偏差。
 */
export function checkTimeEstimateDeviation(trace) {
  const est = Number(trace?.preview?.estimatedSeconds);
  const segs = trace?.segments || [];
  if (!Number.isFinite(est) || est <= 0 || segs.length === 0) {
    return result("time_estimate_deviation", "info", "skip", "工时估算与模拟执行时间偏差", "无工时估算或模拟运动段");
  }
  const simSec = segs.reduce((acc, s) => acc + (Number.isFinite(s.simDurationSec) ? s.simDurationSec : 0), 0);
  if (simSec <= 0) {
    return result("time_estimate_deviation", "info", "skip", "工时估算与模拟执行时间偏差", "模拟运动耗时为 0");
  }
  const diffSec = est - simSec;
  const relDiff = Math.abs(diffSec) / Math.max(simSec, 0.1);
  const largeDeviation = relDiff > 0.5 && Math.abs(diffSec) > 2.0;
  return result(
    "time_estimate_deviation",
    "info",
    largeDeviation ? "notice" : "pass",
    "工时估算与模拟执行时间偏差",
    largeDeviation
      ? `提示：工时估算 ${est.toFixed(2)}s 与模拟匀速执行 ${simSec.toFixed(2)}s 偏差 ${(relDiff * 100).toFixed(1)}%（含加减速折算差异）`
      : `工时估算 ${est.toFixed(2)}s，模拟物理执行 ${simSec.toFixed(2)}s，相对偏差 ${(relDiff * 100).toFixed(1)}%`,
    {
      evidence: {
        estimatedSeconds: Number(est.toFixed(3)),
        simulatedMotionSeconds: Number(simSec.toFixed(3)),
        wallElapsedSeconds: trace?.wallElapsedMs != null ? Number((trace.wallElapsedMs / 1000).toFixed(3)) : null,
        relativeDeviation: Number(relDiff.toFixed(4)),
      },
      suspectedCode: largeDeviation ? "crates/laser-core/src/gcode/estimate.rs (estimate_gcode_time)" : null,
    },
  );
}

/**
 * 不变量 12（功能/告警）：后端返回的 warnings 必须与实际情况一致（例如软限位未开、超速、机械坐标越界）。
 */
export function checkWarningsConsistentWithReality(trace) {
  const expectations = trace?.warningExpectations;
  if (!expectations || typeof expectations !== "object") {
    return result("warnings_consistent_with_reality", "error", "skip", "后端警告与实际情况一致", "未指定警告核对项");
  }
  const actualWarnings = [
    ...(Array.isArray(trace?.sendResult?.warnings) ? trace.sendResult.warnings : []),
    ...(Array.isArray(trace?.preview?.warnings) ? trace.preview.warnings : []),
  ].map(String);

  const missing = [];
  const unexpected = [];

  for (const exp of expectations.mustMatch || []) {
    const re = exp.pattern instanceof RegExp ? exp.pattern : new RegExp(exp.pattern);
    if (!actualWarnings.some((w) => re.test(w))) {
      missing.push(exp.label || String(exp.pattern));
    }
  }
  for (const forb of expectations.mustNotMatch || []) {
    const re = forb.pattern instanceof RegExp ? forb.pattern : new RegExp(forb.pattern);
    const hit = actualWarnings.find((w) => re.test(w));
    if (hit) {
      unexpected.push(`${forb.label || String(forb.pattern)}（命中：${hit}）`);
    }
  }

  if (missing.length > 0 || unexpected.length > 0) {
    const parts = [];
    if (missing.length > 0) parts.push(`缺少预期警告：${missing.join("、")}`);
    if (unexpected.length > 0) parts.push(`出现多余警告：${unexpected.join("、")}`);
    return result(
      "warnings_consistent_with_reality",
      "error",
      "fail",
      "后端警告与实际情况一致",
      parts.join("；"),
      {
        evidence: { actualWarnings, missing, unexpected },
        suspectedCode: "crates/laser-api/src/generation/mod.rs / crates/laser-api/src/send.rs",
      },
    );
  }
  return result(
    "warnings_consistent_with_reality",
    "error",
    "pass",
    "后端警告与实际情况一致",
    `警告列表与预期一致（共 ${actualWarnings.length} 条）`,
    { evidence: { actualWarnings } },
  );
}

export const ALL_INVARIANT_CHECKS = [
  checkLaserOffBeforeMoveOrFraming,
  checkLaserOffAfterTerminal,
  checkG90RestoredAfterRelativeMove,
  checkSpindleSWithinDollar30,
  checkLaserOnWithinFirmwareTravel,
  checkEstopRealtimeNotQueued,
  checkGcodeSha256MatchesPreview,
  checkResumeStartLineAndNoDuplicateBurn,
  checkOverrideOnlyDuringActiveJobSameChannel,
  checkHomingDisabledWhenDollar22Zero,
  checkTimeEstimateDeviation,
  checkWarningsConsistentWithReality,
];

/** 运行全部不变量检查并按严重程度排序。 */
export function runAllInvariants(trace) {
  return ALL_INVARIANT_CHECKS.map((fn) => fn(trace)).sort(
    (a, b) => (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9),
  );
}
