// 雕刻路线全局开关（2026-11-17 用户决定，技术书 r45）：矢量还是转图片。
// 只有一个值，存在后端（`GET / POST /api/engrave-route`）；工作台、创作画板上的两个开关是同一个值的两处入口，
// 语音 / AI 生成的文字任务也按它走，不会两边不同步。局域网（手机连电脑）只读。
// 转图片时文字一律实心、渲染成位图逐行扫描；界面只列当前路线能用的刻法（2026-11-17 r46 用户决定：
// “只显示他有的功能”）——矢量：轮廓描边 / 实心填充 / 排线填充 / 外框切割；转图片：只有实心扫描。
// 这里只放纯逻辑（归一化、响应解析、文字刻法联动）；读写与页面同步在 useEngraveRoute.js。

export const ENGRAVE_ROUTES = Object.freeze(["vector", "image"]);
export const DEFAULT_ENGRAVE_ROUTE = "vector";

/** 本页内广播路线变化的 window 事件名（detail: `{ route }`）。 */
export const ENGRAVE_ROUTE_EVENT = "moss:engrave-route";

export const ENGRAVE_ROUTE_OPTIONS = Object.freeze([
  { value: "vector", label: "矢量", hint: "直接用文字 / 图形的矢量规划路线（线条准、细）" },
  { value: "image", label: "转图片", hint: "先渲染成黑白图再逐行扫描（实心、适合要深浅效果）" },
]);

export const ENGRAVE_ROUTE_SHARED_HINT = "工作台、创作画板与语音共用这一个开关";
export const ENGRAVE_ROUTE_READONLY_HINT = "连接电脑时只能查看雕刻路线，请在电脑上的 MOSS 里切换";

/** 转图片时文字刻法的说明（工作台刻法下方）。 */
export const IMAGE_ROUTE_TEXT_NOTE = "转图片：文字渲染成黑白图逐行扫描；要描边、排线或外框切割请切回矢量";

/** 转图片时“实心填充”在下拉里的名字。 */
export const IMAGE_ROUTE_FILL_LABEL = "实心扫描（转图片逐行扫描）";

/** 没设置过或值无效时按矢量。 */
export function normalizeEngraveRoute(value) {
  return ENGRAVE_ROUTES.includes(value) ? value : DEFAULT_ENGRAVE_ROUTE;
}

/** 当前路线的一句说明。 */
export function engraveRouteHint(route) {
  const value = normalizeEngraveRoute(route);
  return ENGRAVE_ROUTE_OPTIONS.find((opt) => opt.value === value).hint;
}

/** `/api/engrave-route` 响应（client.js `{ ok, data }`）→ 路线；失败或值无效时返回 null（保留界面原值）。 */
export function engraveRouteFromResponse(res) {
  const route = res?.ok && res.data?.success ? res.data.result?.route : null;
  return ENGRAVE_ROUTES.includes(route) ? route : null;
}

/**
 * 保存（POST）的结果：成功返回 `{ route }`；失败返回 `{ route: previous, error }`，界面回到原值并提示。
 * 局域网只读时后端回 403。
 */
export function engraveRouteSaveOutcome(res, previous) {
  const route = engraveRouteFromResponse(res);
  if (route) return { route };
  const fallback = normalizeEngraveRoute(previous);
  if (res?.status === 403) return { route: fallback, error: ENGRAVE_ROUTE_READONLY_HINT };
  const reason = (typeof res?.data?.result === "string" && res.data.result) || res?.error || "后端无响应";
  return { route: fallback, error: `雕刻路线没有保存：${reason}` };
}

/** 文字刻法在当前路线下能不能用：转图片时只有实心（扫描），其余不列出。 */
export function textStyleAvailable(style, route) {
  if (normalizeEngraveRoute(route) !== "image") return true;
  return style === "fill";
}

/**
 * 实际生效的文字刻法：转图片时一律实心（用户原来的选择照样记着，切回矢量即恢复）。
 */
export function effectiveTextStyle(style, route) {
  return textStyleAvailable(style, route) ? style : "fill";
}
