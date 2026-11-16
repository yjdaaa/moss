// 无摄像头两点对位（2026-11-16，对齐 LightBurn Print and Cut）的纯逻辑，不依赖 React。
// 坐标约定与后端相同：设计参考点用“相对任务开始时激光头位置”的坐标（与预览的 frame_mm 相同），
// 实测点是状态查询读到的机器坐标（MPos，固件只报 WPos 时用 WPos，两点必须同一种）。

export const REGISTRATION_SESSION_KEY = "moss-registration-v1";
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
export function readRegistrationSession(storage = globalThis.sessionStorage) {
  try {
    const raw = storage?.getItem(REGISTRATION_SESSION_KEY);
    if (!raw) return null;
    const value = JSON.parse(raw);
    return value && typeof value === "object" ? value : null;
  } catch {
    return null;
  }
}

export function writeRegistrationSession(value, storage = globalThis.sessionStorage) {
  try {
    if (value) storage?.setItem(REGISTRATION_SESSION_KEY, JSON.stringify(value));
    else storage?.removeItem(REGISTRATION_SESSION_KEY);
    return true;
  } catch {
    return false;
  }
}
