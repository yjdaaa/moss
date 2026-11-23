// 无摄像头两点对位（2026-11-16，对齐 LightBurn Print and Cut）与工作台现场移头/对刀原点的纯逻辑，不依赖 React。
// 坐标约定与后端相同：设计参考点用“相对任务开始时激光头位置”的坐标（与预览的 frame_mm 相同），
// 实测点是状态查询读到的机器坐标（MPos，固件只报 WPos 时用 WPos，两点必须同一种）。

import { approvalPendingMessage, isApprovalRequiredResponse } from "./approvalState.js";
import { drawTargetLabel } from "./drawConnection.js";
import { estopTargetArgs } from "./globalEstop.js";

export const REGISTRATION_SESSION_KEY = "moss-registration-v1";
/** 画板的对位进度单独存，和工作台互不清掉对方的记录点。 */
export const REGISTRATION_DRAW_SESSION_KEY = "moss-registration-draw-v1";
/** 两个参考点至少相距多少毫米（与后端 `MIN_POINT_DISTANCE_MM` 一致）。 */
export const MIN_POINT_DISTANCE_MM = 10;
/** 生成对位预览前复核：激光头离第 2 个标记点超过这个距离就拒绝（mm）。 */
export const START_DRIFT_TOLERANCE_MM = 0.1;
export const JOG_STEPS_MM = [0.1, 1, 10];
export const JOG_FEED_MM_PER_MIN = 1000;

export const REGISTRATION_ANCHORS = [
  { value: "lower_left", label: "左下角" },
  { value: "lower_right", label: "右下角" },
  { value: "upper_right", label: "右上角" },
  { value: "upper_left", label: "左上角" },
  { value: "center", label: "中心" },
  { value: "custom", label: "自定义（距左下角）" },
];

function finite(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

/**
 * 任务实际出光范围：优先用橡皮筋轮廓（frame_hull_mm）的外接矩形（不含光栅两侧过扫描），否则用 frame_mm。
 * 返回 `{min_x, max_x, min_y, max_y}`，拿不到时为 null。
 */
export function jobBounds(artifacts) {
  const hull = Array.isArray(artifacts?.frame_hull_mm) ? artifacts.frame_hull_mm : null;
  if (hull && hull.length >= 3 && hull.every((p) => Array.isArray(p) && finite(p[0]) !== null && finite(p[1]) !== null)) {
    const xs = hull.map((p) => Number(p[0]));
    const ys = hull.map((p) => Number(p[1]));
    return { min_x: Math.min(...xs), max_x: Math.max(...xs), min_y: Math.min(...ys), max_y: Math.max(...ys) };
  }
  const frame = artifacts?.frame_mm;
  const keys = ["min_x", "max_x", "min_y", "max_y"];
  if (!frame || !keys.every((key) => finite(frame[key]) !== null)) return null;
  if (Number(frame.max_x) <= Number(frame.min_x) || Number(frame.max_y) <= Number(frame.min_y)) return null;
  return Object.fromEntries(keys.map((key) => [key, Number(frame[key])]));
}

/** 按锚点取设计参考点；自定义时 `custom = {x, y}` 为距任务左下角的毫米数。 */
export function designPoint(bounds, anchor, custom = {}) {
  if (!bounds) return null;
  const { min_x: x0, max_x: x1, min_y: y0, max_y: y1 } = bounds;
  switch (anchor) {
    case "lower_left":
      return { x: x0, y: y0 };
    case "lower_right":
      return { x: x1, y: y0 };
    case "upper_right":
      return { x: x1, y: y1 };
    case "upper_left":
      return { x: x0, y: y1 };
    case "center":
      return { x: (x0 + x1) / 2, y: (y0 + y1) / 2 };
    case "custom": {
      const cx = finite(custom.x);
      const cy = finite(custom.y);
      if (cx === null || cy === null || String(custom.x).trim() === "" || String(custom.y).trim() === "") return null;
      return { x: x0 + cx, y: y0 + cy };
    }
    default:
      return null;
  }
}

export function distance(a, b) {
  if (!a || !b) return NaN;
  return Math.hypot(Number(a.x) - Number(b.x), Number(a.y) - Number(b.y));
}

/** `/api/registration/read-position` 成功结果 → 记录点 `{x, y, frame}`；不可用时 null。 */
export function recordedPoint(result) {
  const x = finite(result?.position?.x);
  const y = finite(result?.position?.y);
  if (x === null || y === null) return null;
  return { x, y, frame: result?.frame === "wpos" ? "wpos" : "mpos" };
}

/**
 * 生成对位预览的请求体；客户端先做与后端相同的基本校验，失败返回 `{error}`。
 */
export function registrationPreviewPayload({ workflowId, design, measured, allowScale = false, machineMotion } = {}) {
  if (!workflowId) return { error: "请先生成加工轨迹" };
  if (!Array.isArray(design) || design.length !== 2 || design.some((p) => !p)) {
    return { error: "请选好两个设计参考点" };
  }
  if (!Array.isArray(measured) || measured.length !== 2 || measured.some((p) => !p)) {
    return { error: "请先依次记录第 1、第 2 个标记点" };
  }
  if (measured[0].frame !== measured[1].frame) {
    return { error: "两个点的坐标类型不同（MPos / WPos），请重新记录" };
  }
  if (distance(design[0], design[1]) < MIN_POINT_DISTANCE_MM) {
    return { error: `两个设计参考点至少要相距 ${MIN_POINT_DISTANCE_MM} mm，越远越准` };
  }
  if (distance(measured[0], measured[1]) < MIN_POINT_DISTANCE_MM) {
    return { error: `两个标记点至少要相距 ${MIN_POINT_DISTANCE_MM} mm，请确认记录的是两个不同的点` };
  }
  const point = (p) => ({ x: Math.round(Number(p.x) * 1000) / 1000, y: Math.round(Number(p.y) * 1000) / 1000 });
  return {
    workflow_id: workflowId,
    design_points: design.map(point),
    measured_points: measured.map(point),
    position_frame: measured[0].frame,
    allow_scale: Boolean(allowScale),
    ...(machineMotion && typeof machineMotion === "object" ? { machine_motion: machineMotion } : {}),
  };
}

/** 点动请求：只走 `/api/safe-action` 的 `relative_move`（激光关闭），显式 confirmed。 */
export function jogRequest(target, dx, dy, feed = JOG_FEED_MM_PER_MIN) {
  if (!target) return null;
  const round = (v) => Math.round(Number(v) * 1000) / 1000;
  if (!Number.isFinite(Number(dx)) || !Number.isFinite(Number(dy)) || (Number(dx) === 0 && Number(dy) === 0)) return null;
  return { action: "relative_move", confirmed: true, x: round(dx), y: round(dy), feed_rate: feed, ...target };
}

/** 结果摘要（显示在面板里）。 */
export function registrationSummary(registration) {
  if (!registration || typeof registration !== "object") return [];
  const fixed = (value, digits) => (Number.isFinite(Number(value)) ? Number(value).toFixed(digits) : "?");
  const lines = [
    `旋转 ${fixed(registration.rotation_deg, 2)}°（逆时针为正）`,
    registration.scale_applied
      ? `等比缩放 ${fixed(Number(registration.scale) * 100, 2)}%`
      : `不缩放（实测距离是设计的 ${fixed(Number(registration.distance_ratio) * 100, 2)}%）`,
    `对位残差 ${fixed(registration.residual_mm, 3)} mm`,
    `原起点相对第 2 个标记点偏移 X ${fixed(registration.offset_mm?.x, 2)} / Y ${fixed(registration.offset_mm?.y, 2)} mm`,
  ];
  return lines;
}

/** 会话内记住对位进度（关闭页面即清空）；存储不可用时只在当前页面生效。 */
export function readRegistrationSession(storage = globalThis.sessionStorage, key = REGISTRATION_SESSION_KEY) {
  try {
    const raw = storage?.getItem(key);
    if (!raw) return null;
    const value = JSON.parse(raw);
    return value && typeof value === "object" ? value : null;
  } catch {
    return null;
  }
}

export function writeRegistrationSession(value, storage = globalThis.sessionStorage, key = REGISTRATION_SESSION_KEY) {
  try {
    if (value) storage?.setItem(key, JSON.stringify(value));
    else storage?.removeItem(key);
    return true;
  } catch {
    return false;
  }
}

// ── 低功率点射（2026-11-16，对齐 LightBurn “Fire”）：没有红光点的机器对准标记时用 ──
// 默认关闭；每次点射都弹确认框（第一次附护目镜等安全提醒）；后端另把功率 / 时长卡死在上限内。

/** 点射功率上限：S10（0~1000 标尺的 1%），与后端 `POINTER_FIRE_MAX_POWER` 一致。 */
export const POINTER_MAX_POWER = 10;
/** 点射时长上限（毫秒），与后端 `POINTER_FIRE_MAX_DURATION_MS` 一致。 */
export const POINTER_MAX_DURATION_MS = 200;
export const POINTER_POWER_OPTIONS = [
  { value: 3, label: "0.3%（S3）" },
  { value: 5, label: "0.5%（S5）" },
  { value: 10, label: "1%（S10）" },
];
export const POINTER_DURATION_OPTIONS = [50, 100, 200];

/** 点射请求体；功率 / 时长不是选项内的整数或超出上限时返回 null（不截断）。 */
export function pointerFireRequest(target, power, durationMs) {
  if (!target) return null;
  const p = Number(power);
  const d = Number(durationMs);
  if (!Number.isInteger(p) || p < 1 || p > POINTER_MAX_POWER) return null;
  if (!Number.isInteger(d) || d < 10 || d > POINTER_MAX_DURATION_MS) return null;
  return { ...target, confirmed: true, power: p, duration_ms: d };
}

/** 点射确认框文案；`first` 为本页第一次点射，附完整安全提醒。 */
export function pointerFireConfirm({ targetLabel, power, durationMs, first }) {
  const pct = (Number(power) / 10).toFixed(1);
  const what = `目标设备：${targetLabel}\n激光将在当前位置以 ${pct}% 功率（S${power}）出光 ${durationMs} ms，然后自动关闭（激光头不移动）。`;
  if (first) {
    return {
      title: "第一次点射：请先做好防护",
      message: `${what}\n\n这是真实出光：即使功率很低，直视或经反光照到眼睛也会造成伤害。\n· 戴好对应激光波长的护目镜，周围的人也要戴或离开；\n· 确认激光头下方是要雕刻的材料，没有镜面、金属反光物或易燃物；\n· 机器状态必须是空闲（Idle），否则后端会拒绝；顶栏急停随时可用。`,
      confirmText: "已戴护目镜，点射",
    };
  }
  return {
    title: "确认点射（会出光）",
    message: `${what}\n请确认已戴好护目镜。`,
    confirmText: "点射",
  };
}

// ── 画板入口（2026-11-16）：对位结果沿用画板的作业摘要、确认框与重复发送防呆 ──

/** 画板“开始雕刻”确认框与提示里的对刀起点文字。 */
export function drawOriginNote(record) {
  if (record?.registration) {
    return typeof record?.artifacts?.origin_note === "string" && record.artifacts.origin_note
      ? record.artifacts.origin_note
      : "开刻前激光头必须停在第 2 个对位标记上（记录第 2 点时的位置）";
  }
  return "开刻前把激光头移到作品左下角（走边框与正式开刻共用该起点）";
}

/**
 * 画板走边框范围：普通画板任务以激光头为左下角、宽高取作业摘要；对位结果相对第 2 个标记点，
 * 范围取变换后的 `frame_mm`（可为负）。拿不到时返回 null。
 */
export function drawFramingExtent(record) {
  if (record?.registration) {
    const frame = record?.artifacts?.frame_mm;
    const keys = ["min_x", "max_x", "min_y", "max_y"];
    if (!frame || !keys.every((key) => finite(frame[key]) !== null)) return null;
    const box = Object.fromEntries(keys.map((key) => [key, Math.round(Number(frame[key]) * 1000) / 1000]));
    if (box.max_x <= box.min_x || box.max_y <= box.min_y) return null;
    return { frame: box, width: box.max_x - box.min_x, height: box.max_y - box.min_y };
  }
  const summary = record?.draw_lab?.job_summary;
  return { frame: null, width: Number(summary?.actual_width_mm) || 100, height: Number(summary?.actual_height_mm) || 100 };
}

/**
 * 画板采用对位结果：后端返回的新 workflow 必须带画板视图（`draw_lab`，指回原画板任务）。
 * 返回画板页要换上的状态；不是画板任务的对位结果时返回 `{error}`。
 */
export function adoptRegisteredDrawRecord(record, sourceWorkflowId) {
  if (!record?.workflow_id || !record?.registration) return { error: "对位结果无效" };
  if (!record.draw_lab?.job_summary) return { error: "对位结果缺少画板视图，请重新生成画板预览后再对位" };
  if (sourceWorkflowId && record.registered_from !== sourceWorkflowId) {
    return { error: "对位结果不是当前画板任务生成的" };
  }
  return {
    workflowId: record.workflow_id,
    result: { success: true, result: record },
    fingerprint: record.draw_lab.content_fingerprint || "",
    status: `对位预览已生成：${record.workflow_id}，尚未发送；${drawOriginNote(record)}`,
  };
}

// ── 工作台“现场移头与对刀原点”（2026-11-23 r60 B3）：完全复用 `/api/safe-action` 与确认 / R3 审批门禁 ──

export const HEAD_JOG_FOLD_ID = "wb-head-jog";
export const WORKBENCH_JOG_STEPS_MM = Object.freeze([1, 5, 10]);
export const WORKBENCH_MAX_JOG_STEP_MM = 10;

const ACTIVE_OVERLAY_STATUSES = new Set(["running", "sending", "preparing", "awaiting_completion"]);

/**
 * 工作台单步点动请求：严格限定在允许步长 `[1, 5, 10]` mm 且单步 `<= 10 mm`、单轴 `±1`，
 * 越界或非单轴一律返回 `null`（绝不静默截断），内部直接复用 `jogRequest`。
 */
export function workbenchJogRequest(target, dxUnit, dyUnit, stepMm) {
  if (!target) return null;
  const step = Number(stepMm);
  if (
    !Number.isFinite(step)
    || step <= 0
    || step > WORKBENCH_MAX_JOG_STEP_MM
    || !WORKBENCH_JOG_STEPS_MM.includes(step)
  ) {
    return null;
  }
  const dx = Number(dxUnit);
  const dy = Number(dyUnit);
  const singleAxis = ((dx === -1 || dx === 1) && dy === 0) || (dx === 0 && (dy === -1 || dy === 1));
  if (!singleAxis) return null;
  return jogRequest(target, dx * step, dy * step, JOG_FEED_MM_PER_MIN);
}

/** 设为工作原点（`set_origin`）与解除报警锁定（`unlock`）请求体，仅走 `/api/safe-action`。 */
export function safeOriginActionRequest(target, action) {
  if (!target) return null;
  if (action !== "set_origin" && action !== "unlock") return null;
  return { action, confirmed: true, ...target };
}

/**
 * 计算工作台现场移头面板的禁用状态与原因说明：
 * 未配置连接、未握手探测、未连接、正在加工/发送/走边框、解算中或有在途动作时全部禁用并给出明确原因。
 */
export function resolveHeadJogGuard({
  connectionForm,
  connectionStatus,
  busy = false,
  framingBusy = false,
  overlayStatus = "pending",
  activeJobs = [],
  workingAction = "",
} = {}) {
  const stopTarget = estopTargetArgs(connectionForm);
  const targetLabel = stopTarget ? drawTargetLabel(connectionForm) : "";

  if (workingAction) {
    return {
      disabled: true,
      code: "in_flight",
      reason: "当前指令正在确认或执行中，请等待完成后再操作（不排队连续移动）",
      shortSummary: "执行中…",
      stopTarget,
      targetLabel,
    };
  }
  const hasRunningJob = Array.isArray(activeJobs) && activeJobs.some((job) => job && job.state === "running");
  if (framingBusy || ACTIVE_OVERLAY_STATUSES.has(overlayStatus) || hasRunningJob) {
    return {
      disabled: true,
      code: "job_active",
      reason: "正在加工或发送指令中，所有移头、设原点与解锁按钮已禁用",
      shortSummary: "加工进行中已锁定",
      stopTarget,
      targetLabel,
    };
  }
  if (busy) {
    return {
      disabled: true,
      code: "busy",
      reason: "正在计算加工轨迹，请稍候",
      shortSummary: "解算轨迹中",
      stopTarget,
      targetLabel,
    };
  }
  if (!stopTarget) {
    return {
      disabled: true,
      code: "unconfigured",
      reason: "尚未配置设备地址，请先点「修改连接」填写控制器 IP 或串口并完成握手探测",
      shortSummary: "尚未配置连接",
      stopTarget: null,
      targetLabel: "",
    };
  }
  if (!connectionStatus?.checkedAt) {
    return {
      disabled: true,
      code: "unprobed",
      reason: "设备尚未握手探测，请先在「修改连接」点击「握手探测」确认在线",
      shortSummary: "需先握手探测",
      stopTarget,
      targetLabel,
    };
  }
  if (!connectionStatus?.connected) {
    const detail = typeof connectionStatus.summary === "string" && connectionStatus.summary.trim()
      ? `（${connectionStatus.summary.trim()}）`
      : "";
    return {
      disabled: true,
      code: "disconnected",
      reason: `设备未连接${detail}，请先检查连接并重新握手探测`,
      shortSummary: "设备未连接",
      stopTarget,
      targetLabel,
    };
  }
  return {
    disabled: false,
    code: "ready",
    reason: "",
    shortSummary: "",
    stopTarget,
    targetLabel,
  };
}

function jogDirectionLabel(dxUnit, dyUnit) {
  if (dxUnit < 0) return "X− 方向";
  if (dxUnit > 0) return "X+ 方向";
  if (dyUnit > 0) return "Y+ 方向";
  return "Y− 方向";
}

/**
 * 规划一次现场安全动作（点动 / 设工作原点 / 解锁 $X）：
 * 先校验参数并返回二次确认弹窗文案；`buildConfirmedPayload` 仅在用户点击确认后调用，杜绝未确认自动带 `confirmed=true`。
 */
export function planHeadSafeAction({ kind, target, targetLabel, dxUnit = 0, dyUnit = 0, stepMm = 1 } = {}) {
  if (!target) return null;
  const label = targetLabel || (target.connection_mode === "serial" ? target.port : `${target.host}:${target.telnet_port}`);
  if (kind === "jog") {
    if (!workbenchJogRequest(target, dxUnit, dyUnit, stepMm)) return null;
    const step = Number(stepMm);
    const dir = jogDirectionLabel(dxUnit, dyUnit);
    return {
      actionLabel: "点动",
      dialog: {
        title: "确认点动移头（机器将移动）",
        message: `目标设备：${label}\n激光头将向 ${dir} 移动 ${step} mm（速度 ${JOG_FEED_MM_PER_MIN} mm/min，激光全程关闭）。\n请人在机器旁观察，确认台面无障碍物；严格模式开启时需桌面审批，急停随时可用。`,
        confirmText: "确认移动",
      },
      buildConfirmedPayload: () => workbenchJogRequest(target, dxUnit, dyUnit, step),
    };
  }
  if (kind === "set_origin") {
    return {
      actionLabel: "设为工作原点",
      dialog: {
        title: "确认设为工作原点",
        message: `目标设备：${label}\n将把激光头当前位置设为工作原点（G92 X0 Y0，激光头不移动、不出光）。\n严格模式开启时需桌面审批；请人在机器旁确认当前对刀位置无误。`,
        confirmText: "设为工作原点",
      },
      buildConfirmedPayload: () => safeOriginActionRequest(target, "set_origin"),
    };
  }
  if (kind === "unlock") {
    return {
      actionLabel: "解锁 $X",
      dialog: {
        title: "确认解除报警锁定 ($X)",
        message: `目标设备：${label}\n将向控制器发送 $X 解除报警锁轴（激光关闭）。\n解锁后尚未回零前控制器可能不掌握准确机械限位，请人在机器旁确认故障已排除后再解锁；严格模式开启时需桌面审批。`,
        confirmText: "确认解锁",
      },
      buildConfirmedPayload: () => safeOriginActionRequest(target, "unlock"),
    };
  }
  return null;
}

/**
 * 创建带同步在途锁的安全动作执行器：
 * - 连续快速点击时在同步入口立即丢弃后续点击，绝不排队多次确认或多次移动；
 * - 每次动作必先经 `confirmFn` 二次确认，取消则零请求；
 * - 响应照 `RegistrationPanel` 复用 `isApprovalRequiredResponse` / `approvalPendingMessage` 处理严格模式 R3 审批与错误提示。
 */
export function createHeadSafeActionRunner({ confirmFn, postFn, alertFn, onLog, onWorkingChange } = {}) {
  let inFlight = false;
  return async function runAction(spec, guard) {
    if (inFlight || guard?.disabled) {
      return { status: "blocked" };
    }
    const plan = planHeadSafeAction({
      ...spec,
      target: guard?.stopTarget,
      targetLabel: guard?.targetLabel,
    });
    if (!plan) {
      return { status: "invalid" };
    }
    inFlight = true;
    onWorkingChange?.(spec.kind);
    try {
      const confirmed = await confirmFn(plan.dialog.message, {
        title: plan.dialog.title,
        tone: "warning",
        confirmText: plan.dialog.confirmText,
        cancelText: "取消",
      });
      if (!confirmed) {
        onLog?.(`已取消${plan.actionLabel}，未向设备发送指令`, "info");
        return { status: "cancelled" };
      }
      const payload = plan.buildConfirmedPayload();
      if (!payload) {
        return { status: "invalid" };
      }
      const res = await postFn("/api/safe-action", payload);
      if (isApprovalRequiredResponse(res)) {
        const msg = approvalPendingMessage(res, plan.actionLabel);
        onLog?.(msg, "warn");
        await alertFn?.(msg, { title: "等待桌面审批", tone: "info" });
        return { status: "approval_required", message: msg };
      }
      if (res?.ok && res?.data?.success) {
        const summary = (typeof res.data.result === "string" && res.data.result) || `${plan.actionLabel}已执行`;
        onLog?.(summary, "ok");
        return { status: "ok", message: summary };
      }
      const reason = (typeof res?.data?.result === "string" && res.data.result) || `${plan.actionLabel}没有完成`;
      onLog?.(`${plan.actionLabel}没有完成：${reason}`, "error");
      await alertFn?.(reason, { title: `${plan.actionLabel}没有完成`, tone: "error" });
      return { status: "failed", message: reason };
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      onLog?.(`${plan.actionLabel}请求异常：${message}`, "error");
      await alertFn?.(`${plan.actionLabel}请求异常：${message}\n请到设备旁确认机器状态，必要时按急停。`, {
        title: `${plan.actionLabel}状态未知`,
        tone: "error",
      });
      return { status: "error", message };
    } finally {
      inFlight = false;
      onWorkingChange?.("");
    }
  };
}
