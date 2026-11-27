// 运行诊断：设备连接提示、内置配通清单、错误码大白话、事件流、审批记录、日志遮挡与耗时报告。

import { builtinSectionSourceView } from "./modelSources.js";

function isBuiltinModelSectionReady(section, builtinStatus) {
  const view = builtinSectionSourceView(section, builtinStatus);
  if (view.source === "colab") return true;
  if (section === "llm") return Boolean(view.cloudConfigured);
  return view.localOn ? Boolean(view.localReady) : Boolean(view.cloudConfigured);
}

/**
 * 内置语音链路三步配通清单（2026-11-23 r59）：
 * 只读现有 builtinStatus，不发额外请求；
 * ① 启动内置服务 → ② 配好识别/对话/合成模型 → ③ USB 配网并允许设备；
 * 三步全绿时返回 `{ visible: false, allDone: true, steps }` 自动隐藏。
 */
export function builtinSetupChecklist(mode, builtinStatus) {
  if (mode !== "builtin" || !builtinStatus || builtinStatus.error) return null;
  const serviceDone = Boolean(builtinStatus.running);
  const hasModelSnapshot = Boolean(builtinStatus.models || builtinStatus.local);
  const modelsDone = hasModelSnapshot
    ? (
        isBuiltinModelSectionReady("asr", builtinStatus)
        && isBuiltinModelSectionReady("llm", builtinStatus)
        && isBuiltinModelSectionReady("tts", builtinStatus)
      )
    : builtinStatus.mode === "chat";
  const devices = Array.isArray(builtinStatus.devices) ? builtinStatus.devices : [];
  const hasAllowedDevice = devices.some((d) => d?.status === "allowed");
  const hasPendingDevice = devices.some((d) => (d?.status || "pending") === "pending");

  const steps = [
    {
      key: "service",
      num: "①",
      label: "启动内置服务",
      done: serviceDone,
      tab: "service",
      group: "builtin-service",
      target: serviceDone ? "voice-builtin-card" : "voice-builtin-start",
    },
    {
      key: "models",
      num: "②",
      label: "配好识别/对话/合成模型",
      done: modelsDone,
      tab: "models",
      group: "",
      target: "voice-builtin-mix-bar",
    },
    {
      key: "device",
      num: "③",
      label: "USB 配网并允许设备",
      done: hasAllowedDevice,
      tab: "devices",
      group: hasPendingDevice || hasAllowedDevice ? "builtin-devices" : "device-usb-provision",
      target: hasPendingDevice || hasAllowedDevice ? "voice-builtin-devices" : "",
    },
  ];
  const allDone = steps.every((s) => s.done);
  return {
    visible: !allDone,
    allDone,
    steps,
  };
}

/** 设备连接与 OTA 提示（§9.4.2 规则 2：无法判断时显示“未知”，不猜）。2026-11-17 起只有官方与内置两条链路。 */
export function deviceView(mode) {
  if (mode === "builtin") {
    return {
      text: "见内置服务",
      tone: "info",
      hint: "内置链路：设备的 OTA 地址应指向本机；地址见“服务”标签，设备连接情况见“设备”标签。",
    };
  }
  return {
    text: "未知",
    tone: "off",
    hint: "官方链路下 MOSS 无法得知设备连接。若之前改过设备 OTA 地址，请改回官方服务器。",
  };
}

export const VOICE_DISMISSED_HINTS_KEY = "moss_voice_dismissed_hints";

/** 读取已单独关闭的语音中心顶部提示键列表（读写均 try/catch）。 */
export function readDismissedVoiceHints(storage = globalThis.localStorage) {
  try {
    const raw = storage?.getItem?.(VOICE_DISMISSED_HINTS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((item) => typeof item === "string" && item) : [];
  } catch {
    return [];
  }
}

/** 记住某条已关闭的顶部提示，返回去重后的新列表。 */
export function dismissVoiceHint(currentKeys, key, storage = globalThis.localStorage) {
  const base = Array.isArray(currentKeys) ? currentKeys.filter((item) => typeof item === "string" && item) : [];
  const next = base.includes(key) ? base : [...base, key];
  try {
    storage?.setItem?.(VOICE_DISMISSED_HINTS_KEY, JSON.stringify(next));
  } catch {
    // 存储受限时仅当次会话生效
  }
  return next;
}

/** 从已关闭列表中移除某条提示键（例如开启严格模式后重置 strict_off，使之后再次关闭严格模式时重新弹出提示）。 */
export function restoreVoiceHint(currentKeys, key, storage = globalThis.localStorage) {
  const base = Array.isArray(currentKeys) ? currentKeys.filter((item) => typeof item === "string" && item) : [];
  if (!base.includes(key)) return base;
  const next = base.filter((item) => item !== key);
  try {
    storage?.setItem?.(VOICE_DISMISSED_HINTS_KEY, JSON.stringify(next));
  } catch {
    // 存储受限时仅当次会话生效
  }
  return next;
}

/**
 * 语音中心顶部独立提示条列表（2026-11-23 r58）：
 * 设备连接说明与严格模式未开启提示拆成两条独立提示，每条可单独关闭，严格模式条在官方与内置两条链路均显示并保留开启入口。
 */
export function visibleVoiceHints({ mode, strictEnabled, dismissed = [] }) {
  const hidden = new Set(Array.isArray(dismissed) ? dismissed : []);
  const device = deviceView(mode);
  const items = [];
  const otaKey = `ota_${mode === "builtin" ? "builtin" : "official"}`;
  if (device.hint && !hidden.has(otaKey)) {
    items.push({
      key: otaKey,
      id: "voice-ota-hint",
      tone: device.tone,
      text: device.hint,
      actionLabel: "",
    });
  }
  if (!strictEnabled && !hidden.has("strict_off")) {
    items.push({
      key: "strict_off",
      id: "voice-strict-hint",
      tone: "warn",
      text: "严格模式未开启：大模型可以自行确认加工任务。",
      actionLabel: "开启严格模式",
    });
  }
  return items;
}

/**
 * 诊断分类 → 大白话（2026-11-07 技术书 n68；与 Rust `moss_voice_providers::DiagCategory` 一一对应）。
 * `cause` 说是哪一类问题，`next` 是没有更具体建议时的通用下一步。
 */
export const VOICE_DIAG_CATEGORIES = {
  general: { label: "一般", cause: "正常运行记录", next: "" },
  network: { label: "网络", cause: "网络问题：电脑连不上对方服务器，或连接中途断了", next: "检查电脑网络、代理和地址是否填对，稍后再试" },
  auth: { label: "密钥/权限", cause: "密钥或权限问题：密钥填错、过期，或没有开通这项服务", next: "到服务方控制台核对密钥和权限后重新填写" },
  quota: { label: "额度", cause: "额度问题：余额不足、今天的额度用完或调用太频繁", next: "充值或等一会儿再试，也可以到控制台提高限额" },
  config: { label: "配置", cause: "配置问题：地址、模型名或参数填得不对", next: "对照服务方文档检查设置" },
  device: { label: "设备", cause: "小智设备问题：断线、握手失败或设备回复了错误", next: "检查设备供电和 Wi-Fi，必要时重启设备或升级固件" },
  firmware: { label: "固件", cause: "固件或资源包问题：设备固件不支持，或推送令牌已失效", next: "确认固件版本，在对应页面重新推送" },
  model: { label: "模型服务", cause: "模型服务问题：服务方出错或返回了异常内容，或本地模型程序出错", next: "稍后再试，或换一个模型服务" },
  audio: { label: "声音", cause: "声音处理问题：音频解码、判停或播放出错", next: "换用标准小智固件或换一首歌再试" },
  internal: { label: "MOSS 内部", cause: "MOSS 程序内部出错（数据库、线程等）", next: "重启 MOSS 再试；一直出现请导出诊断包反馈" },
};

/** 常见错误码 → 大白话（后端 `message` 不够直白时补上“是什么问题、怎么办”）。 */
export const VOICE_ERROR_CODES = {
  voice_unavailable: "语音中心不可用：本机后端未启动。",
  voice_desktop_only: "语音中心仅在 MOSS 桌面端可用。",
  voice_acl_denied: "这个操作只能在 MOSS 主窗口里做。",
  voice_internal: "MOSS 内部出错了，重启 MOSS 再试；一直出现请导出诊断包反馈。",
  builtin_start_failed: "内置语音服务没能启动。",
  diagnostics_export_failed: "诊断包没有导出成功。",
};

function compactJson(value) {
  try {
    const text = JSON.stringify(value);
    if (!text || text === "{}") return "";
    return text.length > 200 ? `${text.slice(0, 200)}…` : text;
  } catch {
    return "";
  }
}

/**
 * 桌面端命令错误（`{code, message, category?, hint?}`、Error 或字符串）转为界面文字。
 * 先看错误码与分类给出大白话，再附上后端的具体原因与下一步；任何输入都不会输出 “[object Object]”。
 */
export function voiceErrorText(error) {
  if (error === null || error === undefined || error === "") return "操作失败（没有收到具体原因）";
  if (typeof error === "string") return error;
  if (typeof error !== "object") return String(error);
  const rawMessage = error.message;
  const message = typeof rawMessage === "string"
    ? rawMessage.trim()
    : rawMessage && typeof rawMessage === "object"
      ? voiceErrorText(rawMessage)
      : "";
  const code = typeof error.code === "string" ? error.code : "";
  const known = code ? VOICE_ERROR_CODES[code] : "";
  const category = typeof error.category === "string" ? VOICE_DIAG_CATEGORIES[error.category] : null;
  const hint = typeof error.hint === "string" ? error.hint.trim() : "";
  let text;
  if (known) {
    text = message && !known.includes(message) ? `${known}具体原因：${message}` : known;
  } else if (message) {
    text = message;
  } else if (category && category.cause !== VOICE_DIAG_CATEGORIES.general.cause) {
    text = category.cause;
  } else if (code) {
    text = `操作失败（错误码 ${code}）`;
  } else {
    const raw = compactJson(error);
    text = raw ? `操作失败（原始信息：${raw}）` : "操作失败（没有收到具体原因）";
  }
  if (hint && !text.includes(hint)) text = `${text.replace(/[。.]$/, "")}。${hint}`;
  return text;
}

/** 对话 / 排障轮次的结果（后端 `TurnTiming.outcome` / `TurnDebugTrace.outcome`）→ 中文。 */
export const VOICE_OUTCOMES = {
  running: { text: "进行中", tone: "info" },
  done: { text: "完成", tone: "ok" },
  interrupted: { text: "被打断", tone: "warn" },
  ignored: { text: "没识别出文字，已忽略", tone: "off" },
  music_started: { text: "转去播放音乐", tone: "info" },
  disconnected: { text: "设备断开，本轮中止", tone: "bad" },
  failed: { text: "失败", tone: "bad" },
};

export function voiceOutcomeLabel(outcome) {
  const key = String(outcome || "");
  return VOICE_OUTCOMES[key] || { text: key ? `未知结果（${key}）` : "未知结果", tone: "off" };
}

const EVENT_LEVELS = {
  info: { text: "记录", tone: "off" },
  warn: { text: "警告", tone: "warn" },
  error: { text: "错误", tone: "bad" },
};

function pad(value, size = 2) {
  return String(value).padStart(size, "0");
}

/** 事件时间：月-日 时:分:秒.毫秒（本机时区）；旧格式只有秒时不编造毫秒。 */
export function formatEventTime(event) {
  const ms = Number(event?.at_ms);
  const seconds = Number(event?.at);
  const hasMs = Number.isFinite(ms) && ms > 0;
  if (!hasMs && !(Number.isFinite(seconds) && seconds > 0)) return "—";
  const date = new Date(hasMs ? ms : seconds * 1000);
  const base = `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return hasMs ? `${base}.${pad(date.getMilliseconds(), 3)}` : base;
}

/** 一条服务事件的显示：级别、分类、时间、说明、下一步（事件自带优先，警告/错误缺省时用分类的通用建议）、技术细节。 */
/**
 * 事件来源（2026-11-09 用户要求）：语音中心“记录与诊断”同时显示小智、激光雕刻机与 MOSS 软件的事件，
 * 每条前面标明来源，方便判断是哪里出的错。旧记录没有 `source` 时按小智处理。
 */
export const VOICE_EVENT_SOURCES = {
  xiaozhi: { label: "小智", tone: "info" },
  laser: { label: "激光雕刻", tone: "warn" },
  moss: { label: "MOSS 软件", tone: "ok" },
};

export function voiceEventView(event) {
  const level = EVENT_LEVELS[event?.level] ? event.level : "info";
  const source = VOICE_EVENT_SOURCES[event?.source] ? event.source : "xiaozhi";
  const category = VOICE_DIAG_CATEGORIES[event?.category] ? event.category : "general";
  const meta = VOICE_DIAG_CATEGORIES[category];
  const own = typeof event?.hint === "string" ? event.hint.trim() : "";
  return {
    key: event?.seq ?? `${event?.at_ms ?? event?.at}-${event?.text}`,
    level,
    levelText: EVENT_LEVELS[level].text,
    tone: EVENT_LEVELS[level].tone,
    source,
    sourceLabel: VOICE_EVENT_SOURCES[source].label,
    sourceTone: VOICE_EVENT_SOURCES[source].tone,
    category,
    categoryLabel: meta.label,
    cause: level === "info" ? "" : meta.cause,
    time: formatEventTime(event),
    text: String(event?.text ?? ""),
    hint: own || (level === "info" ? "" : meta.next),
    detail: typeof event?.detail === "string" ? event.detail : "",
  };
}

/** “只看错误和警告”与来源筛选（`source` 为空或 `all` 时不按来源筛）；不改动原数组顺序（新的在前）。 */
export function filterVoiceEvents(events, onlyProblems, source = "all") {
  const list = Array.isArray(events) ? events : [];
  if (!onlyProblems && (!source || source === "all")) return list;
  return list.filter((event) => {
    if (onlyProblems && event?.level !== "warn" && event?.level !== "error") return false;
    if (source && source !== "all" && (VOICE_EVENT_SOURCES[event?.source] ? event.source : "xiaozhi") !== source) return false;
    return true;
  });
}

/** 各来源的事件条数与错误/警告条数（来源筛选按钮上显示）。 */
export function voiceEventSourceCounts(events) {
  const counts = Object.fromEntries(Object.keys(VOICE_EVENT_SOURCES).map((key) => [key, { total: 0, problems: 0 }]));
  for (const event of Array.isArray(events) ? events : []) {
    const key = VOICE_EVENT_SOURCES[event?.source] ? event.source : "xiaozhi";
    counts[key].total += 1;
    if (event?.level === "warn" || event?.level === "error") counts[key].problems += 1;
  }
  return counts;
}

/** 被覆盖记录的提示（计数为 0 时不提示）；绝不悄悄丢记录。 */
export function voiceEventDropNotices(drops) {
  const notices = [];
  const info = Number(drops?.info) || 0;
  const problems = Number(drops?.problems) || 0;
  const timings = Number(drops?.timings) || 0;
  if (problems) notices.push(`有 ${problems} 条较早的错误或警告已被覆盖（最多保留 ${drops?.max_problems ?? 200} 条）`);
  if (info) notices.push(`有 ${info} 条较早的日常记录已被覆盖（最多保留 ${drops?.max_info ?? 300} 条）`);
  if (timings) notices.push(`有 ${timings} 轮较早的耗时记录已被覆盖（最多保留 ${drops?.max_timings ?? 50} 轮）`);
  return notices;
}

/** 审批列表标题：后端最多返回 `limit` 条，满额时说明更早的没显示，不把它当总数。 */
export function approvalListSummary(count, limit, error) {
  if (error) return "读取失败";
  if (!count) return "暂无";
  const max = Number(limit) || 0;
  return max && count >= max ? `最近 ${count} 条（只显示最近 ${max} 条）` : `最近 ${count} 条`;
}

/** 审批失败 / 取消 / 摘要不一致时的原因（`outcome.message` 与错误码）；成功或尚无结果时为空。 */
export function approvalOutcomeReason(record) {
  const outcome = record?.outcome;
  if (!outcome || outcome.success) return "";
  const message = String(outcome.message || "").trim();
  const code = String(outcome.error_code || "").trim();
  if (message && code) return `${message}（错误码 ${code}）`;
  return message || (code ? `错误码 ${code}` : "");
}

const APPROVAL_STATUS = {
  pending: "待批准",
  approved: "已批准",
  rejected: "已拒绝",
  expired: "已过期",
};

/** 审批记录的状态（§9.5.1：待批准 / 已批准 / 已拒绝 / 已过期 / 摘要不一致 / 已完成 / 已取消 / 急停）。 */
export function approvalHistoryStatus(record) {
  const status = record?.status;
  const outcome = record?.outcome || {};
  const phase = outcome.phase;
  const message = String(outcome.message || "");
  if (outcome.error_code === "approval_mismatch") return { text: "摘要不一致", tone: "bad" };
  if (status === "approved" && phase === "running") return { text: "执行中", tone: "info" };
  if (phase === "cancelled" || status === "cancelled") {
    return /急停|0x18|estop/i.test(message) ? { text: "急停", tone: "bad" } : { text: "已取消", tone: "warn" };
  }
  if (status === "executed" || phase === "completed") {
    if (phase === "failed") return { text: "失败", tone: "bad" };
    if (phase === "unknown") return { text: "结果未知", tone: "warn" };
    return { text: "已完成", tone: "ok" };
  }
  if (status === "failed") return { text: "失败", tone: "bad" };
  if (APPROVAL_STATUS[status]) {
    const tone = status === "pending" ? "warn" : status === "approved" ? "info" : "off";
    return { text: APPROVAL_STATUS[status], tone };
  }
  return { text: "未知", tone: "off" };
}

/** 切换确认框正文。 */
const MODE_TITLE = {
  builtin: "切换到 MOSS 内置服务？",
  official: "切换回官方小智？",
};

export function modeConfirmText(target, changes) {
  const title = MODE_TITLE[target] || MODE_TITLE.official;
  const lines = (Array.isArray(changes) ? changes : []).map((line) => `• ${line}`);
  return { title, message: ["以下设置将被修改（写入 settings.json）：", ...lines].join("\n") };
}

/** 日志行中残留的密钥样式再做一次遮挡（桌面端已脱敏，这里是兜底）。 */
export function maskLogLine(line) {
  return String(line || "").replace(
    /((?:api[_-]?key|apikey|authorization|access_token|token|secret|password)["']?\s*[:=]\s*["']?)(?:bearer\s+|basic\s+)?[^\s"',;&}]+/gi,
    (_, prefix) => `${prefix}***`,
  );
}

export const VOICE_TIMING_STAGES = {
  barge_in: "自然插话确认",
  recording_start: "开始拾音", vad_start: "VAD 确认开始", vad_end: "VAD 判停", utterance_end: "说完 / 松开", text_injected: "文字直通注入", smart_turn: "语义判停",
  interim_stop: "中间结果命中停止", final_asr: "最终识别", jev_filter: "Jev 筛选分流", fast_route: "快捷路由",
  first_llm_delta: "模型首字", llm_done: "模型完成", first_sentence: "第一句切出", first_synthesis_start: "首次合成开始",
  first_synthesis_ready: "首段音频就绪", first_audio_frame: "首帧发给设备", turn_end: "本轮结束",
};

/** 复制只取诊断字段；不复制完整服务状态、对话或模型地址。
 * n68：带会话号、设备编号与结束时间；后端新增的阶段也照抄（名称为原始键），只要值是有限数字。 */
export function voiceTimingReport(timings) {
  return (Array.isArray(timings) ? timings : []).slice(0, 50).map((item) => {
    const stages = item?.stages && typeof item.stages === "object" ? item.stages : {};
    const known = Object.keys(VOICE_TIMING_STAGES).filter((key) => Number.isFinite(stages[key]));
    const extra = Object.keys(stages).filter((key) => !VOICE_TIMING_STAGES[key] && Number.isFinite(stages[key])).sort();
    return {
      session_id: String(item?.session_id ?? ""),
      device_id: String(item?.device_id ?? ""),
      at_ms: Number.isFinite(item?.at_ms) ? item.at_ms : null,
      turn: item?.turn, outcome: item?.outcome,
      models: (Array.isArray(item?.models) ? item.models : []).map(String),
      stages: Object.fromEntries([...known, ...extra].map((key) => [key, stages[key]])),
      tools: (Array.isArray(item?.tools) ? item.tools : []).slice(0, 50).map(({ name, ms }) => ({ name, ms })),
    };
  });
}
