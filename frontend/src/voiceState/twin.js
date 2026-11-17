// MOSS 全息机械眼与语音孪生的视图模型（官方 / 内置两条链路）。

import { maskLogLine } from "./diagnostics.js";

// ---------- MOSS 全息机械眼与语音孪生（2026-11-03；2026-11-17 删除自建链路） ----------

const SYMMETRIC_WAVE_WEIGHTS = [0.34, 0.50, 0.68, 0.84, 0.94, 1.0, 1.0, 0.94, 0.84, 0.68, 0.50, 0.34];
const THINKING_SCAN_BARS = [22, 34, 48, 62, 48, 34, 34, 48, 62, 48, 34, 22];
const PULSE_EVENT_BARS = [18, 30, 52, 74, 88, 64, 64, 88, 74, 52, 30, 18];
const ALERT_BARS = [42, 68, 42, 78, 42, 86, 86, 42, 78, 42, 68, 42];
const IDLE_BARS = [10, 12, 14, 16, 18, 20, 20, 18, 16, 14, 12, 10];
const OFFLINE_BARS = [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6];

function finiteNonNegativeInt(value, fallback = null) {
  if (value === null || value === undefined || value === "" || typeof value === "boolean") {
    return fallback;
  }
  const num = Number(value);
  if (!Number.isFinite(num) || num < 0) {
    return fallback;
  }
  return Math.trunc(num);
}

function sanitizeTwinLine(raw, maxLen = 140) {
  if (!raw) return "";
  const masked = maskLogLine(String(raw))
    .replace(/流式中间结果整句命中急停词（[^）]*）/g, "流式中间结果整句命中急停词")
    .replace(/开始播放本地歌曲：.+/g, "开始播放本地歌曲")
    .trim();
  return masked.length > maxLen ? `${masked.slice(0, maxLen)}…` : masked;
}

function sanitizeIdentifier(raw, maxLen = 48) {
  const clean = maskLogLine(String(raw || ""))
    .replace(/[\r\n\t]+/g, " ")
    .trim();
  return clean.length > maxLen ? `${clean.slice(0, maxLen)}…` : clean;
}

const MOSS_EYE_PALETTES = {
  offline: {
    coreStops: ["#e2e8f0", "#94a3b8", "#475569", "#1e293b", "#090d16"],
    glowStops: ["rgba(148, 163, 184, 0.14)", "rgba(71, 85, 105, 0.06)", "rgba(15, 23, 42, 0)"],
    accent: "#64748b",
    tempo: "dormant",
  },
  idle: {
    coreStops: ["#ffffff", "#ffc2cd", "#ff3b5c", "#990a1e", "#1e0005"],
    glowStops: ["rgba(255, 59, 92, 0.36)", "rgba(56, 189, 248, 0.16)", "rgba(255, 59, 92, 0)"],
    accent: "#ff3b5c",
    tempo: "cruise",
  },
  listening: {
    coreStops: ["#ffffff", "#bae6fd", "#38bdf8", "#0369a1", "#081426"],
    glowStops: ["rgba(56, 189, 248, 0.52)", "rgba(255, 59, 92, 0.24)", "rgba(56, 189, 248, 0)"],
    accent: "#38bdf8",
    tempo: "acoustic",
  },
  thinking: {
    coreStops: ["#ffffff", "#e9d5ff", "#a855f7", "#581c87", "#160526"],
    glowStops: ["rgba(168, 85, 247, 0.50)", "rgba(56, 189, 248, 0.24)", "rgba(168, 85, 247, 0)"],
    accent: "#a855f7",
    tempo: "neural",
  },
  pulse: {
    coreStops: ["#ffffff", "#ddd6fe", "#8b5cf6", "#4c1d95", "#130726"],
    glowStops: ["rgba(139, 92, 246, 0.52)", "rgba(255, 59, 92, 0.26)", "rgba(139, 92, 246, 0)"],
    accent: "#a78bfa",
    tempo: "neural",
  },
  speaking: {
    coreStops: ["#ffffff", "#ffe4e8", "#ff2a52", "#b80828", "#240007"],
    glowStops: ["rgba(255, 59, 92, 0.56)", "rgba(251, 113, 133, 0.28)", "rgba(255, 59, 92, 0)"],
    accent: "#ff3b5c",
    tempo: "stream",
  },
  alert: {
    coreStops: ["#ffffff", "#fef08a", "#f59e0b", "#dc2626", "#290602"],
    glowStops: ["rgba(245, 158, 11, 0.58)", "rgba(239, 68, 68, 0.30)", "rgba(245, 158, 11, 0)"],
    accent: "#f59e0b",
    tempo: "alert",
  },
  engraving: {
    coreStops: ["#ffffff", "#a7f3d0", "#10b981", "#047857", "#031c14"],
    glowStops: ["rgba(16, 185, 129, 0.54)", "rgba(56, 189, 248, 0.28)", "rgba(16, 185, 129, 0)"],
    accent: "#10b981",
    tempo: "laser",
  },
};

/**
 * 根据机械眼视觉状态返回核心球体渐变色与动态节奏配置（确定性纯函数）。
 */
export function mossEyeCorePalette(visualState = "idle") {
  return MOSS_EYE_PALETTES[visualState] || MOSS_EYE_PALETTES.idle;
}

/**
 * 生成 12 柱对称声波/指令脉冲高度（0..100，确定性纯函数，绝不生成随机假音频分贝）。
 * - `builtin` 的 `listening` / `speaking` 按真实 60ms PCM RMS 音量 `level` 缩放；
 * - `thinking` / `pulse` / `alert` / `idle` / `offline` 使用固定的状态几何图案。
 */
export function buildTwinWaveBars(visualState, level = 0) {
  const rawLevel = finiteNonNegativeInt(level, 0);
  const clamped = Math.max(0, Math.min(100, rawLevel));
  if ((visualState === "listening" || visualState === "speaking") && clamped > 0) {
    return SYMMETRIC_WAVE_WEIGHTS.map((weight) => Math.max(12, Math.min(100, Math.round(10 + clamped * 0.9 * weight))));
  }
  if (visualState === "listening" || visualState === "speaking") {
    return [14, 18, 22, 26, 30, 34, 34, 30, 26, 22, 18, 14];
  }
  if (visualState === "thinking") return [...THINKING_SCAN_BARS];
  if (visualState === "pulse") return [...PULSE_EVENT_BARS];
  if (visualState === "alert") return [...ALERT_BARS];
  if (visualState === "idle") return [...IDLE_BARS];
  return [...OFFLINE_BARS];
}

function sessionPriority(state) {
  if (state === "speaking") return 4;
  if (state === "thinking") return 3;
  if (state === "listening") return 2;
  if (state === "idle") return 1;
  return 0;
}

function pickPrimarySession(sessions) {
  if (!Array.isArray(sessions) || sessions.length === 0) return null;
  return [...sessions].sort((a, b) => {
    const diff = sessionPriority(b?.state) - sessionPriority(a?.state);
    if (diff !== 0) return diff;
    return (finiteNonNegativeInt(b?.level, 0)) - (finiteNonNegativeInt(a?.level, 0));
  })[0];
}

/**
 * 语音中心顶部「MOSS 全息语音孪生舱」按链路自适应的视图（确定性纯函数）。
 * - `builtin`：同步 60ms 真实 PCM 振幅 `level`、会话阶段、识别字数、已播句数与工具调用；
 * - `official`：同步官方云端 MCP 握手状态、累计 JSON-RPC 消息帧数与远端工具调用脉冲。
 * 其他取值（含 2026-11-17 删除的 `self_hosted`）按官方链路显示。
 */
export function mossEyeTwinView({
  mode = "official",
  status = null,
  builtinStatus = null,
  mcpStatus = null,
  approvalCount = 0,
} = {}) {
  const normalizedMode = mode === "builtin" ? "builtin" : "official";
  const approvals = finiteNonNegativeInt(approvalCount, 0);
  const approvalAlert = approvals > 0;

  if (normalizedMode === "builtin") {
    const running = Boolean(builtinStatus?.running);
    const sessions = Array.isArray(builtinStatus?.sessions) ? builtinStatus.sessions : [];
    const events = Array.isArray(builtinStatus?.events) ? builtinStatus.events : [];
    const primary = pickPrimarySession(sessions);
    const rawState = primary?.state || "idle";
    const activeAudio = running && sessions.length > 0 && (rawState === "listening" || rawState === "speaking");
    const level = activeAudio ? Math.max(0, Math.min(100, finiteNonNegativeInt(primary?.level, 0))) : 0;
    const activeTool = sanitizeIdentifier(primary?.active_tool, 48);
    const emotion = sanitizeIdentifier(primary?.emotion, 24);
    const utteranceChars = finiteNonNegativeInt(primary?.utterance_chars, 0);
    const spokenSentences = finiteNonNegativeInt(primary?.spoken_sentences, 0);

    let visualState = "offline";
    if (running) {
      if (rawState === "speaking") visualState = "speaking";
      else if (rawState === "thinking") visualState = "thinking";
      else if (rawState === "listening") visualState = "listening";
      else if (approvalAlert) visualState = "alert";
      else visualState = "idle";
    } else if (approvalAlert) {
      visualState = "alert";
    }

    let headline = "内置语音未启动 · 光学镜头待命";
    if (running) {
      if (sessions.length === 0) {
        headline = "内置服务运行中 · 等待小智设备接入";
      } else if (rawState === "speaking") {
        headline = "MOSS 正在播报 · 60ms 帧同步输出";
      } else if (rawState === "thinking") {
        headline = activeTool ? `正在执行工具 · ${activeTool}` : "神经网络推理中 · Jev / LLM 路由";
      } else if (rawState === "listening") {
        headline = level > 0 ? "正在拾音倾听 · 实时声波锁定" : "拾音通道开启 · 等待语音输入";
      } else if (approvalAlert) {
        headline = `等待桌面安全审批 (${approvals} 项)`;
      } else {
        headline = `小智设备在线 (${sessions.length} 台) · 随时唤醒`;
      }
    }

    const latestEvent = sanitizeTwinLine(events[0]?.text);
    const subline = latestEvent
      || (running
        ? `工作模式：${builtinStatus?.mode === "chat" ? "AI 对话模式" : "回声测试模式"}`
        : "启动内置语音服务后，此处实时镜像小智设备的拾音振幅、思考路由与播报节奏");

    const progressValue = spokenSentences > 0
      ? `已播 ${spokenSentences} 句`
      : activeTool
        ? activeTool
        : emotion
          ? `情绪 ${emotion}`
          : running
            ? "待命"
            : "未运行";

    return {
      mode: "builtin",
      visualState,
      sourceBadge: "REALTIME AUDIO TWIN",
      sourceLabel: "内置服务 · 60ms 实时音频孪生",
      level,
      approvalAlert,
      approvalCount: approvals,
      activePolling: running && sessions.some((s) => s?.state === "listening" || s?.state === "thinking" || s?.state === "speaking"),
      headline,
      subline,
      bars: buildTwinWaveBars(visualState, level),
      metrics: [
        { label: "在线会话", value: `${sessions.length} 台` },
        { label: "实时振幅", value: activeAudio ? `${level}%` : "0% (静音)" },
        { label: "本轮识别", value: utteranceChars > 0 ? `${utteranceChars} 字` : "—" },
        { label: "播报/路由", value: progressValue },
      ],
    };
  }

  // official 官方链路
  const configured = Boolean(status?.mcp_endpoint || mcpStatus?.configured);
  const bridgeConnected = Boolean(status?.bridge?.connected ?? mcpStatus?.connected);
  const mcpTools = Array.isArray(status?.mcp?.tools) ? status.mcp.tools : [];
  const latestCalledTool = [...mcpTools].filter((t) => finiteNonNegativeInt(t?.calls, 0) > 0).sort((a, b) => (Number(b?.last_at) || 0) - (Number(a?.last_at) || 0))[0];
  const messagesCount = finiteNonNegativeInt(mcpStatus?.messages_count, 0);
  const rawLastRequest = String(mcpStatus?.last_request || (latestCalledTool ? `tools/call: ${latestCalledTool.name}` : "")).trim();
  const lastRequest = sanitizeTwinLine(rawLastRequest, 72);
  const lastRequestTime = sanitizeIdentifier(mcpStatus?.last_request_time, 16);
  const hasToolCall = lastRequest.startsWith("tools/call:");
  const toolName = hasToolCall ? sanitizeIdentifier(lastRequest.slice("tools/call:".length), 48) : "";
  const recentEvent = Array.isArray(mcpStatus?.recent_events) && mcpStatus.recent_events[0]
    ? sanitizeTwinLine(mcpStatus.recent_events[0])
    : "";
  const rawToolsCount = finiteNonNegativeInt(mcpStatus?.tools_count, null);
  const enabledToolsCount = rawToolsCount !== null
    ? rawToolsCount
    : mcpTools.length > 0
      ? mcpTools.filter((t) => t?.enabled !== false).length
      : null;

  let visualState = "offline";
  if (approvalAlert) {
    visualState = "alert";
  } else if (configured && bridgeConnected) {
    visualState = hasToolCall ? "pulse" : "idle";
  }

  let headline = "官方 MCP 接入点未配置";
  if (approvalAlert) {
    headline = `云端发起受控操作 · ${approvals} 项待桌面批准`;
  } else if (!configured) {
    headline = "官方 MCP 接入点未配置 · 请在下方填写";
  } else if (!bridgeConnected) {
    headline = "官方 MCP 桥未连接 · 正在等待握手";
  } else if (hasToolCall) {
    headline = `收到云端小智指令 · ${toolName}`;
  } else {
    headline = "云端 MCP 神经桥在线 · 实时监听工具指令";
  }

  const subline = recentEvent
    || "语音闲聊在官方云端处理；当小智查询机床状态或发起雕刻时，此处同步触发指令脉冲";

  return {
    mode: "official",
    visualState,
    sourceBadge: "CLOUD MCP TELEMETRY",
    sourceLabel: "官方链路 · 云端 MCP 指令遥测",
    level: 0,
    approvalAlert,
    approvalCount: approvals,
    activePolling: false,
    headline,
    subline,
    bars: buildTwinWaveBars(visualState, 0),
    metrics: [
      { label: "云端网关", value: bridgeConnected ? "已握手在线" : configured ? "连接中断" : "未配置" },
      { label: "指令帧数", value: `${messagesCount} 帧` },
      { label: "最近指令", value: lastRequest ? `${lastRequest}${lastRequestTime ? ` (${lastRequestTime})` : ""}` : "暂无调用" },
      { label: "可用工具", value: enabledToolsCount !== null ? `${enabledToolsCount} 项` : "—" },
    ],
  };
}

/**
 * 左侧全局导航栏（SideNav）左上角 MOSS 光学机械眼的「硬件 + 加工 + 语音 + 审批」多维实时状态推导。
 * - 外环（`hardwareState`）：反映激光硬件握手连接与 G-code 加工任务（`engraving` / `online` / `fault` / `unprobed`）；
 * - 内核（`visualState`）：反映安全警戒、实时语音拾音/思考/播报、MCP 工具脉冲与激光雕刻聚焦。
 */
export function globalEyeSensorView({
  mcpStatus = null,
  voiceStatus = null,
  approvalCount = 0,
  connectionStatus = null,
  activeJobs = [],
} = {}) {
  const approvals = finiteNonNegativeInt(approvalCount, 0);
  const jobs = Array.isArray(activeJobs) ? activeJobs.filter(Boolean) : [];
  const hasUncertainJob = jobs.some((job) => Boolean(job?.unknown || job?.state === "unknown"));
  const activeJobCount = jobs.length;
  const primaryJob = jobs[0] || null;
  const rawJobPercent = finiteNonNegativeInt(primaryJob?.percent, null);
  const jobProgress = rawJobPercent !== null ? Math.max(0, Math.min(100, rawJobPercent)) : null;

  let hardwareState = "unprobed";
  let hwLabel = "";
  if (hasUncertainJob) {
    hardwareState = "fault";
    hwLabel = "激光任务连接状态未知（请现场确认）";
  } else if (activeJobCount > 0) {
    hardwareState = "engraving";
    hwLabel = jobProgress !== null ? `激光加工中 ${jobProgress}%` : `激光加工进行中（${activeJobCount} 项）`;
  } else if (connectionStatus?.connected) {
    hardwareState = "online";
    hwLabel = "硬件握手已通";
  } else if (connectionStatus?.checkedAt) {
    hardwareState = "fault";
    hwLabel = "硬件未连接";
  }

  const withHwSuffix = (baseLabel) => (hwLabel ? `${baseLabel} · ${hwLabel}` : baseLabel);
  const engravingBadge = jobProgress !== null ? `正在发送 ${jobProgress}%` : "正在发送加工";

  if (approvals > 0) {
    return {
      visualState: "alert",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: `待批准 (${approvals})`,
      label: withHwSuffix(`MOSS 警戒：${approvals} 个请求等待批准`),
    };
  }
  if (hasUncertainJob) {
    return {
      visualState: "alert",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: "加工连接异常",
      label: "MOSS 警戒：激光任务连接异常，请在机器旁确认停稳",
    };
  }

  const mode = voiceStatus?.mode || "official";
  if (mode === "builtin") {
    const phase = voiceStatus?.phase || null;
    const level = Math.max(0, Math.min(100, finiteNonNegativeInt(voiceStatus?.level, 0)));
    const sessions = finiteNonNegativeInt(voiceStatus?.device_sessions, null);
    if (phase === "speaking") {
      return {
        visualState: "speaking",
        hardwareState,
        jobProgress,
        level,
        shortBadge: activeJobCount > 0 ? `播报中 · ${engravingBadge}` : `正在播报 ${level}%`,
        label: withHwSuffix(`MOSS 正在播报（振幅 ${level}%）`),
      };
    }
    if (phase === "thinking") {
      return {
        visualState: "thinking",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: activeJobCount > 0 ? `思考中 · ${engravingBadge}` : "AI 思考路由",
        label: withHwSuffix("MOSS 正在思考 / 调用工具"),
      };
    }
    if (phase === "listening") {
      return {
        visualState: "listening",
        hardwareState,
        jobProgress,
        level,
        shortBadge: activeJobCount > 0 ? `拾音中 · ${engravingBadge}` : `正在拾音 ${level}%`,
        label: withHwSuffix(`MOSS 正在倾听（振幅 ${level}%）`),
      };
    }
    if (activeJobCount > 0) {
      return {
        visualState: "engraving",
        hardwareState,
        jobProgress,
        level: jobProgress !== null ? Math.max(25, jobProgress) : 60,
        shortBadge: engravingBadge,
        label: `${hwLabel}${sessions !== null && sessions > 0 ? ` · 内置语音在线（${sessions} 台）` : ""}`,
      };
    }
    if (sessions !== null && sessions > 0) {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: hardwareState === "online" ? "硬件·语音已连" : `语音已连 (${sessions}台)`,
        label: withHwSuffix(`MOSS 内置语音在线（${sessions} 台设备）`),
      };
    }
    if (sessions === 0) {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: hardwareState === "online" ? "硬件已连·待唤醒" : "语音就绪·待设备",
        label: withHwSuffix("MOSS 内置语音运行中（等待设备连接）"),
      };
    }
    if (hardwareState === "online") {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: "硬件已连接",
        label: "激光硬件已就绪 · 内置语音未启动",
      };
    }
    return {
      visualState: "offline",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: hardwareState === "fault" ? "硬件未连接" : "未连接 · 待机",
      label: withHwSuffix("MOSS 内置语音未启动"),
    };
  }

  const connected = Boolean(mcpStatus?.connected);
  const lastReq = sanitizeTwinLine(mcpStatus?.last_request, 72);
  if (connected && lastReq.startsWith("tools/call:")) {
    return {
      visualState: "pulse",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: activeJobCount > 0 ? `指令脉冲 · ${engravingBadge}` : "指令脉冲执行",
      label: withHwSuffix(`小智最近调用：${sanitizeIdentifier(lastReq.slice("tools/call:".length), 48)}`),
    };
  }
  if (activeJobCount > 0) {
    return {
      visualState: "engraving",
      hardwareState,
      jobProgress,
      level: jobProgress !== null ? Math.max(25, jobProgress) : 60,
      shortBadge: engravingBadge,
      label: `${hwLabel} · ${connected ? "小智 MCP 已连接" : "小智 MCP 未连接"}`,
    };
  }
  if (!connected) {
    if (hardwareState === "online") {
      return {
        visualState: "idle",
        hardwareState,
        jobProgress,
        level: 0,
        shortBadge: "硬件已连接",
        label: "激光硬件已就绪 · 小智 MCP 桥未连接",
      };
    }
    return {
      visualState: "offline",
      hardwareState,
      jobProgress,
      level: 0,
      shortBadge: hardwareState === "fault" ? "硬件未连接" : "未连接 · 待机",
      label: withHwSuffix("小智 MCP 桥未连接"),
    };
  }
  return {
    visualState: "idle",
    hardwareState,
    jobProgress,
    level: 0,
    shortBadge: hardwareState === "online" ? "硬件·小智已连" : "小智已连接",
    label: withHwSuffix("官方小智 MCP 桥已连接"),
  };
}
