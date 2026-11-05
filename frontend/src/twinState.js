// 数字孪生核心纯逻辑：
// 1. G-code 刀路解析（模态跟踪 G0/G1/G90/G91/M3/M4/M5/F/S、三维线段、包络、行号索引与连续进度插值）
// 2. GRBL / FluidNC <State|MPos|WPos|Bf|FS|Pn|Ov|SD> 实时遥测报文解析
// 3. 机台物理空间包络与“离线仿真 / 下发同步 / 实机遥测”位姿融合

const DEFAULT_TRAVEL_RATE_MM_MIN = 3000;
const DEFAULT_FEED_RATE_MM_MIN = 1200;
const DEFAULT_POWER_SCALE = 1000;

const PIN_DEFINITIONS = {
  E: { label: "急停按钮 (E)", blocking: true },
  F: { label: "故障输入 (F)", blocking: true },
  D: { label: "安全门 (D)", blocking: true },
  X: { label: "X 轴限位 (X)", blocking: false },
  Y: { label: "Y 轴限位 (Y)", blocking: false },
  Z: { label: "Z 轴限位 (Z)", blocking: false },
  P: { label: "对刀探针 (P)", blocking: false },
  H: { label: "进给保持 (H)", blocking: false },
  R: { label: "软复位 (R)", blocking: false },
  S: { label: "循环启动 (S)", blocking: false },
};

const STATE_META = {
  Idle: { label: "空闲就绪 (Idle)", tone: "ok" },
  Run: { label: "加工运行 (Run)", tone: "info" },
  Jog: { label: "点动移轴 (Jog)", tone: "info" },
  Home: { label: "回零寻位 (Home)", tone: "info" },
  Hold: { label: "暂停保持 (Hold)", tone: "warn" },
  Door: { label: "安全门开 (Door)", tone: "bad" },
  Alarm: { label: "报警锁定 (Alarm)", tone: "bad" },
  Check: { label: "校验模式 (Check)", tone: "info" },
  Sleep: { label: "休眠态 (Sleep)", tone: "off" },
};

function clamp(val, min, max) {
  return Math.min(max, Math.max(min, val));
}

function stripGcodeComment(line) {
  if (!line) return "";
  let out = String(line);
  const semi = out.indexOf(";");
  if (semi !== -1) out = out.slice(0, semi);
  out = out.replace(/\([^)]*\)/g, "");
  return out.trim();
}

/**
 * 解析单行 G-code 中的字母-数值对，如 G1 X12.5 Y-3 F1200 S800
 */
export function parseGcodeWords(rawLine) {
  const cleaned = stripGcodeComment(rawLine);
  if (!cleaned || cleaned.startsWith("$") || cleaned.startsWith("%")) {
    return [];
  }
  const words = [];
  const regex = /([A-Za-z])\s*([+-]?(?:\d+\.?\d*|\.\d+))/g;
  let match;
  while ((match = regex.exec(cleaned)) !== null) {
    const letter = match[1].toUpperCase();
    const value = Number(match[2]);
    if (Number.isFinite(value)) {
      words.push({ letter, value });
    }
  }
  return words;
}

/**
 * 将完整 G-code 文本解析为可用于 3D 渲染、行号同步与时间轴回放的轨迹结构。
 */
export function parseGcodeToolpath(gcodeText, options = {}) {
  const powerScale = Number(options.powerScale) > 0 ? Number(options.powerScale) : DEFAULT_POWER_SCALE;
  const rawLines = typeof gcodeText === "string" && gcodeText.length > 0
    ? gcodeText.replace(/\r\n/g, "\n").split("\n")
    : [];

  let distanceMode = "G90"; // G90 absolute | G91 relative
  let motionMode = "G0"; // G0 rapid | G1 linear cut
  let spindleArmed = false; // M3/M4 enabled
  let explicitM5 = false; // M5 explicitly active until next M3/M4 or S>0
  let currentPower = 0;
  let currentFeed = DEFAULT_FEED_RATE_MM_MIN;
  let x = 0;
  let y = 0;
  let z = 0;

  const segments = [];
  const lineToSegmentIndex = new Int32Array(rawLines.length + 1);
  lineToSegmentIndex.fill(-1);

  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  let minZ = Infinity;
  let maxZ = -Infinity;

  let totalLengthMm = 0;
  let rapidLengthMm = 0;
  let cutLengthMm = 0;
  let totalSeconds = 0;
  let cutSeconds = 0;
  let rapidSeconds = 0;
  let laserLineCount = 0;

  const touchPoint = (px, py, pz) => {
    if (px < minX) minX = px;
    if (px > maxX) maxX = px;
    if (py < minY) minY = py;
    if (py > maxY) maxY = py;
    if (pz < minZ) minZ = pz;
    if (pz > maxZ) maxZ = pz;
  };

  for (let i = 0; i < rawLines.length; i++) {
    const lineNumber = i + 1;
    const words = parseGcodeWords(rawLines[i]);
    if (words.length === 0) {
      lineToSegmentIndex[lineNumber] = segments.length - 1;
      continue;
    }

    let nextX = x;
    let nextY = y;
    let nextZ = z;
    let hasCoord = false;
    let sawS = false;

    for (const { letter, value } of words) {
      if (letter === "G") {
        const code = Math.round(value);
        if (code === 0) motionMode = "G0";
        else if (code === 1 || code === 2 || code === 3) motionMode = "G1";
        else if (code === 90) distanceMode = "G90";
        else if (code === 91) distanceMode = "G91";
        else if (code === 92) {
          // G92 设定当前坐标，不产生轨迹段
          hasCoord = false;
        }
      } else if (letter === "M") {
        const code = Math.round(value);
        if (code === 3 || code === 4) {
          spindleArmed = true;
          explicitM5 = false;
        } else if (code === 5 || code === 2 || code === 30) {
          spindleArmed = false;
          explicitM5 = true;
          currentPower = 0;
        }
      } else if (letter === "S") {
        currentPower = Math.max(0, value);
        sawS = true;
        if (currentPower > 0 && !explicitM5) {
          spindleArmed = true;
        }
      } else if (letter === "F") {
        if (value > 0) currentFeed = value;
      }
    }

    // 如果同内既有 S>0 又有 G1 且未在同行 M5，则允许出光
    if (sawS && currentPower > 0 && motionMode === "G1") {
      spindleArmed = true;
      explicitM5 = false;
    }

    const isG92 = words.some((w) => w.letter === "G" && Math.round(w.value) === 92);
    for (const { letter, value } of words) {
      if (letter === "X") {
        nextX = isG92 ? value : distanceMode === "G90" ? value : x + value;
        hasCoord = !isG92;
      } else if (letter === "Y") {
        nextY = isG92 ? value : distanceMode === "G90" ? value : y + value;
        hasCoord = !isG92;
      } else if (letter === "Z") {
        nextZ = isG92 ? value : distanceMode === "G90" ? value : z + value;
        hasCoord = !isG92;
      }
    }

    if (isG92) {
      x = nextX;
      y = nextY;
      z = nextZ;
      lineToSegmentIndex[lineNumber] = segments.length - 1;
      continue;
    }

    if (hasCoord) {
      const dx = nextX - x;
      const dy = nextY - y;
      const dz = nextZ - z;
      const lengthMm = Math.hypot(dx, dy, dz);
      if (lengthMm > 1e-6) {
        const isCut = motionMode === "G1" && spindleArmed && currentPower > 0;
        const effectivePower = isCut ? currentPower : 0;
        const powerRatio = isCut ? clamp(effectivePower / powerScale, 0, 1) : 0;
        const effectiveFeed = motionMode === "G0" ? DEFAULT_TRAVEL_RATE_MM_MIN : currentFeed;
        const durationSeconds = lengthMm / (Math.max(1, effectiveFeed) / 60);

        touchPoint(x, y, z);
        touchPoint(nextX, nextY, nextZ);

        totalLengthMm += lengthMm;
        if (isCut) {
          cutLengthMm += lengthMm;
          cutSeconds += durationSeconds;
          laserLineCount += 1;
        } else {
          rapidLengthMm += lengthMm;
          rapidSeconds += durationSeconds;
        }
        totalSeconds += durationSeconds;

        segments.push({
          index: segments.length,
          lineNumber,
          mode: motionMode,
          from: { x, y, z },
          to: { x, y, z: nextZ },
          lengthMm,
          durationSeconds,
          laserOn: isCut,
          power: effectivePower,
          powerRatio,
          feedRate: effectiveFeed,
          cumulativeLengthMm: totalLengthMm,
          cumulativeSeconds: totalSeconds,
          rawLine: stripGcodeComment(rawLines[i]),
        });
        segments[segments.length - 1].to = { x: nextX, y: nextY, z: nextZ };
      }
      x = nextX;
      y = nextY;
      z = nextZ;
    }

    lineToSegmentIndex[lineNumber] = segments.length - 1;
  }

  const hasBounds = Number.isFinite(minX) && Number.isFinite(maxX);
  const bounds = hasBounds
    ? {
        minX,
        maxX,
        minY,
        maxY,
        minZ,
        maxZ,
        widthMm: Math.max(0, maxX - minX),
        heightMm: Math.max(0, maxY - minY),
      }
    : {
        minX: 0,
        maxX: 0,
        minY: 0,
        maxY: 0,
        minZ: 0,
        maxZ: 0,
        widthMm: 0,
        heightMm: 0,
      };

  return {
    totalLines: rawLines.length,
    laserLineCount,
    segments,
    lineToSegmentIndex,
    bounds,
    totalLengthMm,
    rapidLengthMm,
    cutLengthMm,
    estimatedSeconds: totalSeconds,
    cutSeconds,
    rapidSeconds,
    powerScale,
  };
}

/**
 * 格式化秒数为易读工时文字列（如 "8.4 s" 或 "01m 15.2s (75.2 s)"）。
 */
export function formatDurationSeconds(sec) {
  const s = Math.max(0, Number(sec) || 0);
  if (s < 60) {
    return `${s.toFixed(1)} s`;
  }
  const mins = Math.floor(s / 60);
  const rem = s - mins * 60;
  return `${mins}分${rem.toFixed(1)}秒 (${s.toFixed(1)}s)`;
}

/**
 * 根据 G-code 行号（如 JobWorker 的 lines_sent）定位激光头位姿与已完成轨迹段。
 */
export function resolvePoseAtLine(toolpath, lineNumber) {
  const totalSec = Number(toolpath?.estimatedSeconds) || 0;
  if (!toolpath || !Array.isArray(toolpath.segments) || toolpath.segments.length === 0) {
    return {
      x: 0,
      y: 0,
      z: 0,
      laserOn: false,
      power: 0,
      powerRatio: 0,
      feedRate: 0,
      mode: "G0",
      lineNumber: 0,
      completedSegmentCount: 0,
      progressRatio: 0,
      traveledMm: 0,
      elapsedSeconds: 0,
      totalSeconds: 0,
      remainingSeconds: 0,
      rawLine: "",
    };
  }

  const clampedLine = clamp(Math.floor(Number(lineNumber) || 0), 0, toolpath.totalLines);
  if (clampedLine <= 0) {
    const first = toolpath.segments[0];
    return {
      x: first.from.x,
      y: first.from.y,
      z: first.from.z,
      laserOn: false,
      power: 0,
      powerRatio: 0,
      feedRate: 0,
      mode: "G0",
      lineNumber: 0,
      completedSegmentCount: 0,
      progressRatio: 0,
      traveledMm: 0,
      elapsedSeconds: 0,
      totalSeconds: totalSec,
      remainingSeconds: totalSec,
      rawLine: "",
    };
  }

  const segIdx = toolpath.lineToSegmentIndex[clampedLine] ?? -1;
  if (segIdx < 0) {
    const first = toolpath.segments[0];
    return {
      x: first.from.x,
      y: first.from.y,
      z: first.from.z,
      laserOn: false,
      power: 0,
      powerRatio: 0,
      feedRate: 0,
      mode: "G0",
      lineNumber: clampedLine,
      completedSegmentCount: 0,
      progressRatio: toolpath.totalLines > 0 ? clampedLine / toolpath.totalLines : 0,
      traveledMm: 0,
      elapsedSeconds: 0,
      totalSeconds: totalSec,
      remainingSeconds: totalSec,
      rawLine: "",
    };
  }

  const seg = toolpath.segments[segIdx];
  const elapsedSec = Number(seg.cumulativeSeconds) || 0;
  return {
    x: seg.to.x,
    y: seg.to.y,
    z: seg.to.z,
    laserOn: seg.laserOn,
    power: seg.power,
    powerRatio: seg.powerRatio,
    feedRate: seg.feedRate,
    mode: seg.mode,
    lineNumber: clampedLine,
    completedSegmentCount: segIdx + 1,
    progressRatio: totalSec > 0
      ? elapsedSec / totalSec
      : toolpath.totalLengthMm > 0
      ? seg.cumulativeLengthMm / toolpath.totalLengthMm
      : 1,
    traveledMm: seg.cumulativeLengthMm,
    elapsedSeconds: elapsedSec,
    totalSeconds: totalSec,
    remainingSeconds: Math.max(0, totalSec - elapsedSec),
    rawLine: seg.rawLine,
  };
}

/**
 * 根据连续时间进度（0..1，严格对应 0..estimatedSeconds 真实加工时间）平滑插值激光头位姿。
 * 每段按其实际进给速度 F（durationSeconds）推进，确保数字孪生雕刻时间与 G-code 真实加工耗时 1:1 一一对应。
 */
export function resolvePoseAtProgress(toolpath, progressRatio) {
  if (!toolpath || !Array.isArray(toolpath.segments) || toolpath.segments.length === 0) {
    return resolvePoseAtLine(toolpath, 0);
  }
  const totalSec = Number(toolpath.estimatedSeconds) || 0;
  const ratio = clamp(Number(progressRatio) || 0, 0, 1);
  if (ratio <= 0) {
    return resolvePoseAtLine(toolpath, 0);
  }
  if (ratio >= 1) {
    const last = toolpath.segments[toolpath.segments.length - 1];
    return {
      x: last.to.x,
      y: last.to.y,
      z: last.to.z,
      laserOn: false,
      power: last.power,
      powerRatio: last.powerRatio,
      feedRate: last.feedRate,
      mode: last.mode,
      lineNumber: toolpath.totalLines,
      completedSegmentCount: toolpath.segments.length,
      activeSegmentFraction: 1,
      progressRatio: 1,
      traveledMm: toolpath.totalLengthMm,
      elapsedSeconds: totalSec,
      totalSeconds: totalSec,
      remainingSeconds: 0,
      rawLine: last.rawLine,
    };
  }

  // 严格按物理加工时长 (cumulativeSeconds) 二分查找当前所处轨迹段
  const targetSec = ratio * totalSec;
  let lo = 0;
  let hi = toolpath.segments.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (toolpath.segments[mid].cumulativeSeconds < targetSec) {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }

  const seg = toolpath.segments[lo];
  const prevCumulativeSec = lo > 0 ? toolpath.segments[lo - 1].cumulativeSeconds : 0;
  const prevCumulativeLen = lo > 0 ? toolpath.segments[lo - 1].cumulativeLengthMm : 0;
  const segDuration = seg.durationSeconds ?? Math.max(0, seg.cumulativeSeconds - prevCumulativeSec);
  const localSec = Math.max(0, targetSec - prevCumulativeSec);
  const t = segDuration > 1e-9 ? clamp(localSec / segDuration, 0, 1) : 1;
  const traveledMm = prevCumulativeLen + seg.lengthMm * t;

  return {
    x: seg.from.x + (seg.to.x - seg.from.x) * t,
    y: seg.from.y + (seg.to.y - seg.from.y) * t,
    z: seg.from.z + (seg.to.z - seg.from.z) * t,
    laserOn: seg.laserOn,
    power: seg.power,
    powerRatio: seg.powerRatio,
    feedRate: seg.feedRate,
    mode: seg.mode,
    lineNumber: seg.lineNumber,
    completedSegmentCount: lo,
    activeSegmentIndex: lo,
    activeSegmentFraction: t,
    progressRatio: ratio,
    traveledMm,
    elapsedSeconds: targetSec,
    totalSeconds: totalSec,
    remainingSeconds: Math.max(0, totalSec - targetSec),
    rawLine: seg.rawLine,
  };
}

function parseTriplet(str) {
  if (!str) return null;
  const parts = str.split(",").map((s) => Number(s.trim()));
  if (parts.length < 2 || !Number.isFinite(parts[0]) || !Number.isFinite(parts[1])) {
    return null;
  }
  return {
    x: parts[0],
    y: parts[1],
    z: Number.isFinite(parts[2]) ? parts[2] : 0,
  };
}

/**
 * 解析 GRBL / FluidNC 的 `<State|MPos:...|WPos:...|Bf:...|FS:...|Pn:...|Ov:...|SD:...>` 报文。
 * 兼容传入完整对象（如 `{ state, raw, pins, planner_free, rx_buffer_free }`）或原始状态行字符串。
 */
export function parseGrblStatusReport(input, fallbackSummary = "") {
  let raw = "";
  if (typeof input === "string") {
    raw = input.trim();
  } else if (input && typeof input === "object") {
    raw = typeof input.raw === "string" ? input.raw.trim() : "";
  }

  // 从文本中提取 `<...>` 段
  const match = raw.match(/<([^>]+)>/);
  const inner = match ? match[1] : "";

  if (!inner && (!input || typeof input !== "object" || !input.state)) {
    return {
      online: false,
      state: "Offline",
      stateLabel: "未连接 / 无报文",
      tone: "off",
      mpos: null,
      wpos: null,
      wco: null,
      bf: null,
      fs: null,
      ov: null,
      pins: { raw: "", active: [] },
      sd: null,
      raw: raw || fallbackSummary || "",
    };
  }

  const fields = inner ? inner.split("|") : [];
  const rawState = (fields[0] || input?.state || "Unknown").trim();
  const baseState = rawState.split(":")[0];
  const meta = STATE_META[baseState] || { label: rawState || "未知状态", tone: "warn" };

  let mpos = null;
  let wpos = null;
  let wco = null;
  let bf = null;
  let fs = null;
  let ov = null;
  let pinsRaw = typeof input?.pins === "string" ? input.pins : "";
  let sd = null;

  for (let i = 1; i < fields.length; i++) {
    const f = fields[i].trim();
    if (f.startsWith("MPos:")) {
      mpos = parseTriplet(f.slice(5));
    } else if (f.startsWith("WPos:")) {
      wpos = parseTriplet(f.slice(5));
    } else if (f.startsWith("WCO:")) {
      wco = parseTriplet(f.slice(4));
    } else if (f.startsWith("Bf:")) {
      const parts = f.slice(3).split(",");
      const plannerFree = Number(parts[0]);
      const rxBufferFree = Number(parts[1]);
      if (Number.isFinite(plannerFree) || Number.isFinite(rxBufferFree)) {
        bf = {
          plannerFree: Number.isFinite(plannerFree) ? plannerFree : null,
          rxBufferFree: Number.isFinite(rxBufferFree) ? rxBufferFree : null,
        };
      }
    } else if (f.startsWith("FS:")) {
      const parts = f.slice(3).split(",");
      const feedRate = Number(parts[0]);
      const spindlePower = Number(parts[1]);
      fs = {
        feedRate: Number.isFinite(feedRate) ? feedRate : 0,
        spindlePower: Number.isFinite(spindlePower) ? spindlePower : 0,
      };
    } else if (f.startsWith("F:")) {
      const feedRate = Number(f.slice(2));
      if (Number.isFinite(feedRate)) {
        fs = { feedRate, spindlePower: fs?.spindlePower ?? 0 };
      }
    } else if (f.startsWith("Ov:")) {
      const parts = f.slice(3).split(",").map((v) => Number(v.trim()));
      if (parts.length >= 3 && parts.every(Number.isFinite)) {
        ov = { feed: parts[0], rapid: parts[1], spindle: parts[2] };
      }
    } else if (f.startsWith("Pn:")) {
      pinsRaw = f.slice(3).trim();
    } else if (f.startsWith("SD:")) {
      const sdBody = f.slice(3).trim();
      const comma = sdBody.indexOf(",");
      if (comma !== -1) {
        const pct = Number(sdBody.slice(0, comma));
        sd = {
          percent: Number.isFinite(pct) ? pct : null,
          path: sdBody.slice(comma + 1).trim(),
        };
      } else {
        sd = { percent: null, path: sdBody };
      }
    }
  }

  if (!bf && input && typeof input === "object") {
    if (Number.isFinite(input.planner_free) || Number.isFinite(input.rx_buffer_free)) {
      bf = {
        plannerFree: Number.isFinite(input.planner_free) ? input.planner_free : null,
        rxBufferFree: Number.isFinite(input.rx_buffer_free) ? input.rx_buffer_free : null,
      };
    }
  }

  if (!wpos && mpos && wco) {
    wpos = {
      x: mpos.x - wco.x,
      y: mpos.y - wco.y,
      z: mpos.z - wco.z,
    };
  }
  if (!mpos && wpos && wco) {
    mpos = {
      x: wpos.x + wco.x,
      y: wpos.y + wco.y,
      z: wpos.z + wco.z,
    };
  }

  const activePins = [];
  const seenPin = new Set();
  for (const ch of pinsRaw) {
    const code = ch.toUpperCase();
    if (seenPin.has(code)) continue;
    seenPin.add(code);
    const def = PIN_DEFINITIONS[code] || { label: `引脚 (${code})`, blocking: false };
    activePins.push({ code, label: def.label, blocking: def.blocking });
  }

  return {
    online: true,
    state: rawState,
    baseState,
    stateLabel: meta.label,
    tone: meta.tone,
    mpos,
    wpos,
    wco,
    bf,
    fs,
    ov,
    pins: {
      raw: pinsRaw,
      active: activePins,
      hasBlockingPin: activePins.some((p) => p.blocking),
    },
    sd,
    raw: raw || `<${rawState}>`,
  };
}

/**
 * 大鱼 DIY 翼宿 V1.0 (Yisu V1.0) 真实硬件与图纸包物理参数基线：
 * - 标称有效雕刻行程：100 × 100 mm（安全边距 5mm 时为 90 × 90 mm）
 * - 切割垫板：121.9 × 121.9 × 4.5 mm（底板外框 193 × 222 × 6 mm）
 * - 运动机构：3 根 140mm MGN9H 线性导轨 + 2 台 42 步进电机 (高 40mm) + Ø8×186mm 联动光轴 + GT2 同步带
 * - 激光总成：PETG 打印《激光头转接板_5W》+《X轴连接件》+《线束固定器》+ 24V 5W 半导体激光头
 * - 排烟与外罩：后置 4028 风扇 (24V 10000RPM) + G3 主板盖 + 3mm 半透亚克力顶部观察板 (186.8×213mm) 与前遮光板 (158×107mm)
 */
export const YISU_V1_PROFILE = {
  modelName: "大鱼 DIY 翼宿 V1.0",
  xTravelMm: 100,
  yTravelMm: 100,
  zTravelMm: 20,
  safeMarginMm: 5,
  cuttingMatSizeMm: 121.9,
  chassisWidthMm: 193,
  chassisDepthMm: 222,
  chassisHeightMm: 107,
  railSpec: "3 × 140mm MGN9H 线性滑轨",
  motorSpec: "2 × 42 步进电机 (40mm) + Ø8×186mm 联动轴",
  laserSpec: "24V 5W 激光头 + PETG 5W转接板总成",
  fanSpec: "4028 排烟风扇 (24V 10000RPM)",
};

/**
 * 计算三维机台工作区尺寸、板材外框尺寸、板材台面起点坐标 (`offsetX`, `offsetY`) 与雕刻内容在材料内的相对位置 (`textOffsetX`, `textOffsetY`)。
 * - 默认情况（`materialConfig.autoSize !== false`）下，板材宽高与 G-code 刀路包络严格 1:1 同步；
 * - 支持用户手动调整材料外框尺寸（`widthMm`、`heightMm`、`thicknessMm`，自动限制不小于雕刻刀路尺寸且不超出机台行程）；
 * - 支持调整激光雕刻文字/刀路在材料内的位置（`textOffsetX`、`textOffsetY`），严格钳制在材料四周边距内（`0 <= minX + textOffsetX` 且 `maxX + textOffsetX <= materialWidthMm`），确保怎么调整都在材料内部、绝不超出材料外框。
 */
export function resolveMachineWorkspace(
  machineLimits,
  toolpathBounds,
  workflowArtifacts,
  materialConfig = null,
) {
  const xTravelMm = Number(machineLimits?.x_travel_mm) > 0
    ? Number(machineLimits.x_travel_mm)
    : YISU_V1_PROFILE.xTravelMm;
  const yTravelMm = Number(machineLimits?.y_travel_mm) > 0
    ? Number(machineLimits.y_travel_mm)
    : YISU_V1_PROFILE.yTravelMm;
  const zTravelMm = Number(machineLimits?.z_travel_mm) > 0
    ? Number(machineLimits.z_travel_mm)
    : YISU_V1_PROFILE.zTravelMm;

  // G-code 刀路包络真实尺寸
  const minBoundX = Number.isFinite(Number(toolpathBounds?.minX)) ? Number(toolpathBounds.minX) : 0;
  const minBoundY = Number.isFinite(Number(toolpathBounds?.minY)) ? Number(toolpathBounds.minY) : 0;
  const maxBoundX = Number.isFinite(Number(toolpathBounds?.maxX)) ? Number(toolpathBounds.maxX) : 0;
  const maxBoundY = Number.isFinite(Number(toolpathBounds?.maxY)) ? Number(toolpathBounds.maxY) : 0;

  const gcodeWidth = Number(toolpathBounds?.widthMm) > 0
    ? Number(toolpathBounds.widthMm)
    : Math.max(0, maxBoundX - Math.min(0, minBoundX));
  const gcodeHeight = Number(toolpathBounds?.heightMm) > 0
    ? Number(toolpathBounds.heightMm)
    : Math.max(0, maxBoundY - Math.min(0, minBoundY));

  const spanX = Math.max(gcodeWidth, maxBoundX - Math.min(0, minBoundX));
  const spanY = Math.max(gcodeHeight, maxBoundY - Math.min(0, minBoundY));

  const rawAutoW = gcodeWidth > 0
    ? gcodeWidth
    : Number(workflowArtifacts?.width_mm) > 0
    ? Number(workflowArtifacts.width_mm)
    : Math.min(60, xTravelMm);
  const rawAutoH = gcodeHeight > 0
    ? gcodeHeight
    : Number(workflowArtifacts?.height_mm) > 0
    ? Number(workflowArtifacts.height_mm)
    : Math.min(60, yTravelMm);

  const autoWidthMm = clamp(Number(rawAutoW.toFixed(1)), 1, xTravelMm);
  const autoHeightMm = clamp(Number(rawAutoH.toFixed(1)), 1, yTravelMm);

  // 允许用户在 [1, xTravelMm] / [1, yTravelMm] 范围内自由调节 3D 材料外框宽高 W/H；
  // 当材料宽高小于原始刀路包络时自动计算缩放比 toolpathScaleX/Y，确保刀路与激光头始终跟随材料且不越界
  const minMatWidthMm = 1;
  const minMatHeightMm = 1;

  const isCustomSize = materialConfig?.autoSize === false;
  const parsedCustomW = Number(materialConfig?.widthMm);
  const parsedCustomH = Number(materialConfig?.heightMm);
  const materialWidthMm = isCustomSize && Number.isFinite(parsedCustomW) && parsedCustomW > 0
    ? clamp(Number(parsedCustomW.toFixed(1)), minMatWidthMm, xTravelMm)
    : autoWidthMm;
  const materialHeightMm = isCustomSize && Number.isFinite(parsedCustomH) && parsedCustomH > 0
    ? clamp(Number(parsedCustomH.toFixed(1)), minMatHeightMm, yTravelMm)
    : autoHeightMm;
  const thicknessMm = clamp(
    Number(materialConfig?.thicknessMm ?? workflowArtifacts?.thickness_mm) || 3,
    0.5,
    20,
  );

  // 材料在机台台面上的起点坐标 (X0, Y0)，钳制在机台有效行程内
  const maxMaterialOffsetX = Math.max(0, Number((xTravelMm - materialWidthMm).toFixed(1)));
  const maxMaterialOffsetY = Math.max(0, Number((yTravelMm - materialHeightMm).toFixed(1)));
  const offsetX = clamp(
    Number(Number(materialConfig?.offsetX || 0).toFixed(1)),
    0,
    maxMaterialOffsetX,
  );
  const offsetY = clamp(
    Number(Number(materialConfig?.offsetY || 0).toFixed(1)),
    0,
    maxMaterialOffsetY,
  );

  // 激光雕刻文字/刀路在材料内部的相对位置 (textOffsetX, textOffsetY)
  // 严格限制在 [minTextOffset, maxTextOffset] 闭区间内，保证雕刻轨迹任何一点都不超出材料四边
  const contentMaxX = Math.max(gcodeWidth, maxBoundX);
  const contentMaxY = Math.max(gcodeHeight, maxBoundY);
  const toolpathScaleX = contentMaxX > 0 && materialWidthMm < contentMaxX
    ? Number((materialWidthMm / contentMaxX).toFixed(4))
    : 1;
  const toolpathScaleY = contentMaxY > 0 && materialHeightMm < contentMaxY
    ? Number((materialHeightMm / contentMaxY).toFixed(4))
    : 1;
  const effectiveContentMaxX = Number((contentMaxX * toolpathScaleX).toFixed(2));
  const effectiveContentMaxY = Number((contentMaxY * toolpathScaleY).toFixed(2));

  const minTextOffsetX = minBoundX < 0
    ? clamp(Number((-minBoundX * toolpathScaleX).toFixed(1)), 0, materialWidthMm)
    : 0;
  const minTextOffsetY = minBoundY < 0
    ? clamp(Number((-minBoundY * toolpathScaleY).toFixed(1)), 0, materialHeightMm)
    : 0;
  const maxTextOffsetX = Math.max(
    minTextOffsetX,
    Number(Math.max(0, materialWidthMm - effectiveContentMaxX).toFixed(1)),
  );
  const maxTextOffsetY = Math.max(
    minTextOffsetY,
    Number(Math.max(0, materialHeightMm - effectiveContentMaxY).toFixed(1)),
  );

  const rawTextOffsetX = Number(
    materialConfig?.textOffsetX ?? materialConfig?.contentOffsetX ?? 0,
  );
  const rawTextOffsetY = Number(
    materialConfig?.textOffsetY ?? materialConfig?.contentOffsetY ?? 0,
  );
  const textOffsetX = clamp(
    Number((Number.isFinite(rawTextOffsetX) ? rawTextOffsetX : 0).toFixed(1)),
    minTextOffsetX,
    maxTextOffsetX,
  );
  const textOffsetY = clamp(
    Number((Number.isFinite(rawTextOffsetY) ? rawTextOffsetY : 0).toFixed(1)),
    minTextOffsetY,
    maxTextOffsetY,
  );

  // 雕刻刀路与激光头在机台世界坐标系下的合成起点 (材料起点 + 材料内偏移)
  const toolpathOffsetX = Number((offsetX + textOffsetX).toFixed(1));
  const toolpathOffsetY = Number((offsetY + textOffsetY).toFixed(1));

  return {
    modelName: YISU_V1_PROFILE.modelName,
    xTravelMm,
    yTravelMm,
    zTravelMm,
    offsetX,
    offsetY,
    textOffsetX,
    textOffsetY,
    toolpathOffsetX,
    toolpathOffsetY,
    toolpathScaleX,
    toolpathScaleY,
    hasHardwareLimits: Boolean(
      machineLimits?.from_hardware ||
      (Number(machineLimits?.x_travel_mm) > 0 && Number(machineLimits?.y_travel_mm) > 0 && machineLimits?.from_hardware !== false)
    ),
    material: {
      name: materialConfig?.name || workflowArtifacts?.material || "椴木板",
      widthMm: materialWidthMm,
      heightMm: materialHeightMm,
      thicknessMm,
      minWidthMm: minMatWidthMm,
      minHeightMm: minMatHeightMm,
      autoWidthMm,
      autoHeightMm,
      autoSize: !isCustomSize,
      offsetX,
      offsetY,
      maxOffsetX: maxMaterialOffsetX,
      maxOffsetY: maxMaterialOffsetY,
      textOffsetX,
      textOffsetY,
      minTextOffsetX,
      maxTextOffsetX,
      minTextOffsetY,
      maxTextOffsetY,
      toolpathOffsetX,
      toolpathOffsetY,
      toolpathScaleX,
      toolpathScaleY,
    },
  };
}

/**
 * 融合“离线仿真推演 (sim)”、“加工下发同步 (job)”与“实机状态映射 (telemetry)”三种信号源，
 * 输出最终驱动 3D 龙门架、激光头与光斑的状态，绝不编造硬件在线状态。
 */
export function resolveTwinHeadPose({
  driveMode = "sim",
  simPose = null,
  jobPose = null,
  telemetry = null,
  jobRunning = false,
}) {
  const base = (driveMode === "job" && jobPose) ? jobPose : (simPose || jobPose || {
    x: 0,
    y: 0,
    z: 0,
    laserOn: false,
    power: 0,
    powerRatio: 0,
    feedRate: 0,
    mode: "G0",
    lineNumber: 0,
    completedSegmentCount: 0,
    progressRatio: 0,
    traveledMm: 0,
    rawLine: "",
  });

  if (driveMode === "telemetry" && telemetry?.online) {
    const pos = telemetry.wpos || telemetry.mpos || { x: base.x, y: base.y, z: base.z };
    const spindle = telemetry.fs?.spindlePower ?? 0;
    const feed = telemetry.fs?.feedRate ?? 0;
    const isRunningState = telemetry.baseState === "Run";
    const laserOn = isRunningState && spindle > 0;
    return {
      ...base,
      x: pos.x,
      y: pos.y,
      z: pos.z ?? 0,
      laserOn,
      power: spindle,
      powerRatio: clamp(spindle / DEFAULT_POWER_SCALE, 0, 1),
      feedRate: feed,
      sourceLabel: "控制器实机坐标 (MPos/WPos)",
      isLiveHardware: true,
    };
  }

  if (driveMode === "job") {
    return {
      ...base,
      laserOn: Boolean(jobRunning && base.laserOn),
      sourceLabel: jobRunning ? "JobWorker 实时下发同步" : "任务记录断点行映射",
      isLiveHardware: Boolean(jobRunning),
    };
  }

  return {
    ...base,
    sourceLabel: "离线 G-code 刀路推演",
    isLiveHardware: false,
  };
}

export const MAX_XY_TRAVEL_MM = 2000;
export const MAX_Z_TRAVEL_MM = 500;

export function normalizeWorkspaceLimits(patch = {}, base = null) {
  const fallback = base || {
    x_travel_mm: YISU_V1_PROFILE.xTravelMm,
    y_travel_mm: YISU_V1_PROFILE.yTravelMm,
    z_travel_mm: YISU_V1_PROFILE.zTravelMm,
    from_hardware: false,
  };
  return {
    ...fallback,
    ...patch,
    x_travel_mm: clamp(
      Number(patch?.x_travel_mm ?? fallback.x_travel_mm) || YISU_V1_PROFILE.xTravelMm,
      20,
      MAX_XY_TRAVEL_MM,
    ),
    y_travel_mm: clamp(
      Number(patch?.y_travel_mm ?? fallback.y_travel_mm) || YISU_V1_PROFILE.yTravelMm,
      20,
      MAX_XY_TRAVEL_MM,
    ),
    z_travel_mm: clamp(
      Number(patch?.z_travel_mm ?? fallback.z_travel_mm) || YISU_V1_PROFILE.zTravelMm,
      5,
      MAX_Z_TRAVEL_MM,
    ),
  };
}

export function buildTwinConnectionPayload({
  mode = "network",
  port = "",
  baudrate = 115200,
  networkHost = "",
  telnetPort = 23,
} = {}) {
  if (mode === "serial") {
    return {
      connection_mode: "serial",
      port,
      baudrate: Number(baudrate) || 115200,
    };
  }
  const parsedTelnetPort = Number(telnetPort) || 23;
  return {
    connection_mode: "network",
    network_host: networkHost,
    host: networkHost,
    network_telnet_port: parsedTelnetPort,
    telnet_port: parsedTelnetPort,
  };
}

export function buildTwinEmergencyStopPayload({
  isJobRunning,
  selectedWorkflowId,
  connectionPayload,
} = {}) {
  if (isJobRunning && selectedWorkflowId) {
    return {
      action: "emergency_stop",
      confirmed: true,
      job_id: selectedWorkflowId,
    };
  }
  return {
    ...(connectionPayload || {}),
    action: "emergency_stop",
    confirmed: true,
  };
}
