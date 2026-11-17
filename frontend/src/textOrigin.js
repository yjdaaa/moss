// 文字对刀起点全局设置（2026-11-17 用户决定，技术书 r48）：只有一个值，存在后端（`GET / POST /api/text-origin`）；
// 工作台“对刀起点”读写它，语音 / AI 生成文字任务时没说起点也按它，两边不会不同步。创作画板与图片固定左下角。
// 局域网（手机连电脑）只读：改了只对本次生效，不保存。这里只放纯逻辑；读写与页面同步在 useTextOrigin.js。

export const TEXT_ORIGINS = Object.freeze(["center", "lower_left"]);
export const DEFAULT_TEXT_ORIGIN = "center";

/** 本页内广播起点变化的 window 事件名（detail: `{ origin }`）。 */
export const TEXT_ORIGIN_EVENT = "moss:text-origin";

/** 下拉旁的说明（title）。 */
export const TEXT_ORIGIN_SHARED_HINT = "工作台与语音共用：语音刻字没说起点时也按这里";

/** 没设置过或值无效时按木牌中心。 */
export function normalizeTextOrigin(value) {
  return TEXT_ORIGINS.includes(value) ? value : DEFAULT_TEXT_ORIGIN;
}

/** `/api/text-origin` 响应（client.js `{ ok, data }`）→ 起点；失败或值无效时返回 null（保留界面原值）。 */
export function textOriginFromResponse(res) {
  const origin = res?.ok && res.data?.success ? res.data.result?.origin : null;
  return TEXT_ORIGINS.includes(origin) ? origin : null;
}

/** 保存（POST）的结果：成功返回 `{ origin }`；失败返回 `{ origin: previous, error }`。 */
export function textOriginSaveOutcome(res, previous) {
  const origin = textOriginFromResponse(res);
  if (origin) return { origin };
  const reason = (typeof res?.data?.result === "string" && res.data.result) || res?.error || "后端无响应";
  return { origin: normalizeTextOrigin(previous), error: `对刀起点没有保存：${reason}` };
}
