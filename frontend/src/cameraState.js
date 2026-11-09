/**
 * 桌面端「摄像头」标签页纯状态与轮询门禁计算（无 DOM 依赖，供单测直接验证）。
 *
 * 原则：
 * 1. 打开页面绝不自动触发硬件拍照，必须由用户点击「抓拍一张」或显式开启「自动刷新」。
 * 2. 自动刷新为串行节拍（上一帧未回传前绝不叠发第二帧），且设备处于拾音/思考/播报时自动让路。
 */

// 2026-11-09 用户反馈“一卡一卡的”：官方小智固件只有拍照、没有视频流，连续刷新就是逐张抓拍。
// 原先固定 1.6 秒一张、0.32 秒取一次图，约 0.6 帧/秒；改为上一张回来后歇一小会儿就拍下一张（仍一次只拍一张），
// 等图时更勤地取，实际帧率由小智拍照 + 上传的耗时决定。
export const CAMERA_AUTO_INTERVAL_MS = 450;
/** 上一张回来后至少歇这么久再拍下一张，给小智留出处理语音与网络的空档。 */
export const CAMERA_AUTO_GAP_MS = 120;
export const CAMERA_STATUS_POLL_MS = 4000;
export const CAMERA_FRAME_POLL_MS = 150;
/** 估算帧率时取最近几张的到达时间。 */
export const CAMERA_FPS_WINDOW = 6;

const SESSION_PHASE_TEXT = {
  idle: "空闲待命",
  listening: "正在听你说话",
  thinking: "正在思考",
  speaking: "正在播报语音",
};

function parseJsonSafe(raw) {
  if (!raw || typeof raw !== "string") return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

/** 汇总内置语音服务下的摄像头设备列表与默认选中项。 */
export function cameraDevicesView(builtinStatus) {
  const running = Boolean(builtinStatus?.running);
  const mode = builtinStatus?.mode === "chat" ? "chat" : "echo";
  const httpPort = Number(builtinStatus?.http_port) || 8003;
  const wsPort = Number(builtinStatus?.ws_port) || 8000;
  const otaUrl = builtinStatus?.ota_ip_url || builtinStatus?.ota_url || "";
  const rawDevices = Array.isArray(builtinStatus?.devices) ? builtinStatus.devices : [];
  const sessions = Array.isArray(builtinStatus?.sessions) ? builtinStatus.sessions : [];
  const sessionByDevice = new Map();
  for (const s of sessions) {
    if (s?.device_id) {
      sessionByDevice.set(s.device_id, s);
    }
  }

  let pendingCount = 0;
  const devices = [];
  for (const d of rawDevices) {
    if (!d?.device_id) continue;
    if (d.status === "pending") {
      pendingCount += 1;
      continue;
    }
    if (d.status !== "allowed") {
      continue;
    }
    const session = sessionByDevice.get(d.device_id) || null;
    const helloObj = parseJsonSafe(d.hello_json);
    const tools = Array.isArray(session?.device_tools) ? session.device_tools : [];
    const toolListReady = tools.length > 0;
    const online = Boolean(session);
    const mcpSupported = online && (toolListReady || helloObj?.mcp !== false);
    const hasCameraTool = toolListReady ? tools.includes("self.camera.take_photo") : null;
    const canCapture = Boolean(running && online && mcpSupported && hasCameraTool !== false);
    let disabledReason = "";
    if (!running) {
      disabledReason = "内置语音服务未运行";
    } else if (!online) {
      disabledReason = "设备当前离线";
    } else if (!mcpSupported) {
      disabledReason = "设备未启用设备端 MCP";
    } else if (hasCameraTool === false) {
      disabledReason = "该设备固件未上报摄像头工具（self.camera.take_photo）";
    }
    const sessionState = session?.state || null;
    devices.push({
      deviceId: d.device_id,
      title: (d.name || "").trim() || d.device_id,
      model: d.model || "",
      firmware: d.firmware_version || "",
      ip: session?.ip || d.ip || "",
      online,
      sessionState,
      sessionStateLabel: sessionState ? SESSION_PHASE_TEXT[sessionState] || sessionState : "离线",
      mcpSupported,
      toolListReady,
      hasCameraTool,
      canCapture,
      disabledReason,
      lastSeenAt: Number(d.last_seen_at) || 0,
    });
  }

  devices.sort((a, b) => {
    if (a.canCapture !== b.canCapture) return a.canCapture ? -1 : 1;
    if (a.online !== b.online) return a.online ? -1 : 1;
    return b.lastSeenAt - a.lastSeenAt;
  });

  const defaultDevice =
    devices.find((d) => d.canCapture)
    || devices.find((d) => d.online)
    || devices[0]
    || null;

  return {
    running,
    mode,
    httpPort,
    wsPort,
    otaUrl,
    pendingCount,
    devices,
    defaultDeviceId: defaultDevice ? defaultDevice.deviceId : "",
  };
}

/** 判断自动刷新下一拍是否允许发起抓拍，或在对话占线/等待回传时自动让路。 */
export function shouldAutoCaptureNext({ autoEnabled, device, frameView }) {
  if (!autoEnabled) {
    return { allow: false, pausedReason: "" };
  }
  if (!device) {
    return { allow: false, pausedReason: "未选择小智设备" };
  }
  if (!device.canCapture) {
    return { allow: false, pausedReason: device.disabledReason || "当前设备暂不可抓拍" };
  }
  if (frameView?.has_camera_tool === false) {
    return { allow: false, pausedReason: "该设备固件未上报摄像头工具（self.camera.take_photo）" };
  }
  if (frameView?.online === false) {
    return { allow: false, pausedReason: "设备刚刚离线，等待重连…" };
  }
  if (frameView?.pending) {
    return { allow: false, pausedReason: "等待上一帧画面回传…" };
  }
  if (frameView?.last_error) {
    return { allow: false, pausedReason: "最近一次抓拍未成功，自动刷新已暂停" };
  }
  const liveState = frameView?.session_state || device.sessionState || "idle";
  if (liveState !== "idle") {
    return {
      allow: false,
      pausedReason: `小智${SESSION_PHASE_TEXT[liveState] || "对话中"}，自动刷新已暂缓（说完了自动恢复）`,
    };
  }
  return { allow: true, pausedReason: "" };
}

/** 生成画面镜像/倒置旋转的 CSS transform（倒装支架查看激光台面时一键扶正）。 */
export function cameraTransformValue({ mirror = false, flip180 = false } = {}) {
  const parts = [];
  if (mirror) parts.push("scaleX(-1)");
  if (flip180) parts.push("rotate(180deg)");
  return parts.join(" ");
}

/** 画面来源文案与状态徽章色调。 */
export function cameraFrameSourceView(frame) {
  if (!frame) {
    return { label: "暂无画面", tone: "off" };
  }
  if (frame.source === "voice") {
    return { label: "语音拍照问答", tone: "info" };
  }
  return { label: "桌面抓拍", tone: "ok" };
}

/** 按最近几张画面的到达时间（毫秒）估算连续刷新的实际帧率；不足两张时返回 null。 */
export function cameraFpsView(arrivals) {
  const list = (Array.isArray(arrivals) ? arrivals : []).filter((t) => Number.isFinite(t)).slice(-CAMERA_FPS_WINDOW);
  if (list.length < 2) return null;
  const span = list[list.length - 1] - list[0];
  if (span <= 0) return null;
  const fps = ((list.length - 1) * 1000) / span;
  return { fps, text: fps >= 1 ? `约 ${fps.toFixed(1)} 帧/秒` : `约 ${(1 / fps).toFixed(1)} 秒一张` };
}
