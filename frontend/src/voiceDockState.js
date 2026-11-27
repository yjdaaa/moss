/**
 * 全局“小智 · 雕刻同步”悬浮小窗的纯逻辑（2026-11-16 用户要求：语音让 MOSS 干什么，任何页面都能同步看见，
 * 只显示雕刻相关、可收成小胶囊）。
 *
 * 数据来源全部是既有只读接口，这里只做展示换算：
 * - 语音阶段：App 已在轮询的 `/api/system/info` 的 `voice`（`mode` / `device_sessions` / `phase`）；
 * - 本轮工具名：桌面端 `voice_builtin_status`（lite）的 `sessions[].active_tool`，只有注册名，不含参数与结果；
 * - 雕刻进度：`JobProgressDock` 已轮询 `/api/jobs/stop-targets` 换算出的任务视图；
 * - 待确认预览 / 待批准请求：只给指向既有 AiPreviewCenter / ApprovalCenter 的入口，不重复确认与发送。
 * 绝不读取或显示识别原文、回复原文与调试轨迹（`debug_traces`）。
 */

/** 读取内置语音会话（拿本轮工具名）的轮询间隔：只在小智非空闲时轮询。 */
export const VOICE_DOCK_POLL_MS = 1500;

/** 一轮对话结束（会话回到空闲、工具名被清空）后，“刚才调用”仍保留的时长。 */
export const VOICE_DOCK_TOOL_LINGER_MS = 20_000;

/** 折叠状态在本机记忆的键名。 */
export const VOICE_DOCK_COLLAPSED_KEY = "moss_voice_dock_collapsed";

/** 激光工具注册名 → 中文说法；不在表里的工具（打开应用、快捷键等）不显示，窗口只讲雕刻。 */
const LASER_TOOL_LABELS = {
  laser_workflow_tool: "雕刻任务",
  check_laser_connection_tool: "检查机器连接",
  laser_safe_action_tool: "停止 / 调速",
  material_params_tool: "查材料参数",
  recommend_laser_params_tool: "推荐加工参数",
  refine_laser_params_from_feedback_tool: "按效果调参数",
  generate_text_laser_task_tool: "生成雕刻预览",
  regenerate_text_laser_task_tool: "重新生成预览",
  run_calibration_grid_tool: "测试矩阵",
  select_calibration_cell_tool: "选定测试格参数",
  start_tuned_job_tool: "按调好的参数开刻",
  laser_grbl_tool: "设备指令",
  laser_network_grbl_tool: "设备指令",
};

const PHASE_TEXT = {
  idle: "待命",
  listening: "在听你说",
  thinking: "思考中",
  speaking: "正在回答",
};

const PHASE_PRIORITY = { speaking: 4, thinking: 3, listening: 2, idle: 1 };

/** 工具注册名 → 中文说法；非激光工具或非法输入返回空字符串。 */
export function laserToolLabel(name) {
  if (typeof name !== "string") return "";
  const key = name.trim();
  return Object.prototype.hasOwnProperty.call(LASER_TOOL_LABELS, key) ? LASER_TOOL_LABELS[key] : "";
}

/** 归一化语音阶段；未知值按空闲处理。 */
export function voicePhase(raw) {
  return raw === "listening" || raw === "thinking" || raw === "speaking" ? raw : "idle";
}

function deviceCount(voiceStatus) {
  const n = Number(voiceStatus?.device_sessions);
  return Number.isFinite(n) && n > 0 ? Math.trunc(n) : 0;
}

/** 语音部分是否可显示：只在桌面端、内置语音链路且至少一台小智在线时。 */
export function voiceSectionAvailable({ voiceEnabled = false, voiceStatus = null } = {}) {
  return Boolean(voiceEnabled) && voiceStatus?.mode === "builtin" && deviceCount(voiceStatus) > 0;
}

/** 是否需要读内置语音会话拿工具名：语音部分可显示且小智不在空闲（空闲时后端已清空工具名）。 */
export function shouldPollVoiceSessions({ voiceEnabled = false, voiceStatus = null, visible = true } = {}) {
  return Boolean(visible) && voiceSectionAvailable({ voiceEnabled, voiceStatus }) && voicePhase(voiceStatus?.phase) !== "idle";
}

/**
 * 从内置语音状态里挑出最值得显示的激光工具：优先正在说/想的会话，只看激光工具。
 * 只读 `sessions[].state` 与 `sessions[].active_tool`，其余字段一概不碰。
 */
export function activeLaserTool(builtinStatus) {
  const sessions = Array.isArray(builtinStatus?.sessions) ? builtinStatus.sessions : [];
  let best = null;
  for (const session of sessions) {
    const label = laserToolLabel(session?.active_tool);
    if (!label) continue;
    const priority = PHASE_PRIORITY[voicePhase(session?.state)] || 0;
    if (!best || priority > best.priority) best = { label, priority };
  }
  return best ? best.label : "";
}

/** 记住最近一次看到的激光工具，供一轮结束后显示“刚才调用”。 */
export function nextRecentTool(previous, label, now) {
  if (label) return { label, at: Number(now) || 0 };
  return previous || null;
}

/** 当前该显示的工具行：正在调用 / 刚才调用（保留 VOICE_DOCK_TOOL_LINGER_MS）/ 无。 */
export function toolLine({ phase = "idle", currentTool = "", recentTool = null, now = 0 } = {}) {
  if (currentTool && phase !== "idle") return { active: true, text: `正在调用：${currentTool}` };
  if (recentTool?.label && Number(now) - Number(recentTool.at) <= VOICE_DOCK_TOOL_LINGER_MS) {
    return { active: false, text: `刚才调用：${recentTool.label}` };
  }
  return null;
}

/** 雕刻进度行：直接用 JobProgressDock 的任务视图，不另行轮询，也不编造百分比。 */
export function jobLine(jobs) {
  const list = Array.isArray(jobs) ? jobs.filter(Boolean) : [];
  if (list.length === 0) return null;
  if (list.length > 1) return { percent: null, short: `${list.length} 个任务`, text: `${list.length} 个雕刻任务进行中` };
  const job = list[0];
  const percent = job.unknown || typeof job.percent !== "number" ? null : job.percent;
  return {
    percent,
    short: percent === null ? "雕刻中" : `${percent}%`,
    text: `雕刻 ${percent === null ? "进度未知" : `${percent}%`} · ${job.phaseText || "运行中"}`,
  };
}

/**
 * 悬浮小窗的整体视图。`visible=false` 时整个窗口不渲染：
 * 没有可显示的语音（浏览器 / 局域网 / 非内置链路 / 没有小智在线）且没有待确认预览与待批准请求时隐藏；
 * 只有雕刻任务时也隐藏——右下角的雕刻任务浮层已完整显示进度与停止按钮，不重复占位。
 */
export function voiceDockView({
  voiceEnabled = false,
  voiceStatus = null,
  currentTool = "",
  recentTool = null,
  jobs = [],
  previewWaiting = 0,
  approvalWaiting = 0,
  now = 0,
} = {}) {
  const voiceOn = voiceSectionAvailable({ voiceEnabled, voiceStatus });
  const previews = Math.max(0, Math.trunc(Number(previewWaiting) || 0));
  const approvals = Math.max(0, Math.trunc(Number(approvalWaiting) || 0));
  const phase = voiceOn ? voicePhase(voiceStatus?.phase) : "idle";
  const devices = voiceOn ? deviceCount(voiceStatus) : 0;
  const tool = voiceOn ? toolLine({ phase, currentTool, recentTool, now }) : null;
  const job = voiceOn ? jobLine(jobs) : null;
  const visible = voiceOn || previews > 0 || approvals > 0;

  const previewText = previews > 0 ? (previews > 1 ? `${previews} 个预览待确认` : "有待确认的预览") : "";
  const approvalText = approvals > 0 ? `${approvals} 个请求待批准` : "";

  const pillParts = [];
  if (voiceOn) pillParts.push(`小智${PHASE_TEXT[phase]}`);
  if (tool?.active) pillParts.push(currentTool);
  if (job) pillParts.push(job.short);
  if (previews > 0) pillParts.push("预览待确认");
  else if (approvals > 0) pillParts.push("待批准");

  return {
    visible,
    voice: voiceOn
      ? {
          phase,
          phaseText: PHASE_TEXT[phase],
          devicesText: `${devices} 台小智在线`,
        }
      : null,
    tool,
    job,
    previewText,
    approvalText,
    attention: previews > 0 || approvals > 0,
    pillText: pillParts.join(" · "),
  };
}

export function readVoiceDockCollapsed(storage = globalThis.localStorage) {
  try {
    return storage?.getItem(VOICE_DOCK_COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}

export function writeVoiceDockCollapsed(collapsed, storage = globalThis.localStorage) {
  try {
    storage?.setItem(VOICE_DOCK_COLLAPSED_KEY, collapsed ? "1" : "0");
  } catch {
    /* 存储不可用时只是不记忆折叠状态 */
  }
}
