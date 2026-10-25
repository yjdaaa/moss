/**
 * 工作台自由画布的纯逻辑（2026-10-18 用户决定）：栅格常量、布局读写、碰撞与让位。
 *
 * 不依赖 React，便于单元测试（`frontend/tests/workbenchLayout.test.mjs`）。
 * 位置单位：x/w 是 0–48 的列号，y/h 是行数（CSS 里 1 行 = 4px，行间 8px 空隙）。
 */

export const STORAGE_KEY = "moss_wb_layout_v3";
export const DRAW_STORAGE_KEY = "moss_draw_layout_v1";
export const LAYOUT_VERSION = 5;
export const COLS = 48;
export const ROW_UNIT = 4;
export const ROW_GAP = 8;
export const COL_GAP = 16;
export const MIN_ROWS = 10; // 10 行 ≈ 112px（适配单行提示卡如未生成轨迹时的加工反馈）

/** 每张卡允许的最小列宽，避免被拖成一条缝。 */
export const MIN_COLS = {
  link: 12,
  source: 12,
  material: 12,
  monitor: 16,
  feedback: 12,
  board: 16,
  "draw-material": 12,
  "draw-device": 12,
  "draw-send": 12,
};

/** 默认排布（左窄右宽双栏）：左栏 16 列竖排三张输入卡，右栏 32 列上方加工监视舱、下方紧贴加工反馈，左右底边齐平。 */
export const DEFAULT_LAYOUT = {
  link: { x: 0, y: 0, w: 16, h: 23 },
  source: { x: 0, y: 24, w: 16, h: 27 },
  material: { x: 0, y: 52, w: 16, h: 23 },
  monitor: { x: 16, y: 0, w: 32, h: 64 },
  feedback: { x: 16, y: 65, w: 32, h: 10 },
};

/** 创作画板默认排布：左侧大画布（34 列），右侧三张卡片竖排（14 列）。 */
export const DRAW_DEFAULT_LAYOUT = {
  board: { x: 0, y: 0, w: 34, h: 68 },
  "draw-material": { x: 34, y: 0, w: 14, h: 28 },
  "draw-device": { x: 34, y: 29, w: 14, h: 22 },
  "draw-send": { x: 34, y: 52, w: 14, h: 28 },
};

export function clamp(value, min, max) {
  return Math.max(min, Math.min(max, value));
}

/** 两个卡片在水平列范围上是否重叠。 */
export function overlapsX(a, b) {
  return a.x < b.x + b.w && b.x < a.x + a.w;
}

/** 两个卡片矩形是否相交。 */
export function collides(a, b) {
  return overlapsX(a, b) && a.y < b.y + b.h && b.y < a.y + a.h;
}

/**
 * 当某张卡片仅高度发生变化（收起、展开或上下缩放底边）时，
 * 让同列下方的卡片跟随向上贴合（消除空白洞）或向下推开。
 */
export function reflowVertical(layout, targetId, nextBox) {
  const prevBox = layout[targetId];
  if (!prevBox) return settle({ ...layout, [targetId]: nextBox }, targetId);
  const deltaH = nextBox.h - prevBox.h;
  if (deltaH === 0 && nextBox.y === prevBox.y && nextBox.x === prevBox.x && nextBox.w === prevBox.w) {
    return layout;
  }

  if (deltaH < 0 && nextBox.y === prevBox.y) {
    const prevBottom = prevBox.y + prevBox.h;
    const updated = { ...layout, [targetId]: { ...nextBox } };
    const belowIds = Object.keys(updated)
      .filter((id) => id !== targetId && overlapsX(updated[id], prevBox) && updated[id].y >= prevBottom)
      .sort((a, b) => updated[a].y - updated[b].y || updated[a].x - updated[b].x);

    for (const id of belowIds) {
      const card = updated[id];
      const origGap = Math.max(1, card.y - prevBottom);
      let candidateY = Math.max(0, card.y + deltaH);
      // 不能穿过上方已放置的任何同列卡片（保留至少 1 行间距）
      for (const [otherId, other] of Object.entries(updated)) {
        if (otherId === id) continue;
        if (overlapsX(card, other) && other.y < card.y) {
          candidateY = Math.max(candidateY, other.y + other.h + 1);
        }
      }
      if (candidateY < card.y) {
        updated[id] = { ...card, y: candidateY };
      } else {
        updated[id] = { ...card, y: Math.max(candidateY, nextBox.y + nextBox.h + Math.min(1, origGap)) };
      }
    }
    return settle(updated, targetId);
  }

  if (deltaH > 0 && nextBox.y === prevBox.y) {
    const prevBottom = prevBox.y + prevBox.h;
    const updated = { ...layout, [targetId]: { ...nextBox } };
    const belowIds = Object.keys(updated)
      .filter((id) => id !== targetId && overlapsX(updated[id], prevBox) && updated[id].y >= prevBottom)
      .sort((a, b) => updated[a].y - updated[b].y || updated[a].x - updated[b].x);

    for (const id of belowIds) {
      const card = updated[id];
      updated[id] = { ...card, y: card.y + deltaH };
    }
    return settle(updated, targetId);
  }

  return settle({ ...layout, [targetId]: nextBox }, targetId);
}

/** 把移动卡固定后，其它卡依次向下避让，直到没有重叠。 */
export function settle(layout, movingId) {
  const entries = Object.entries(layout).map(([id, box]) => ({ id, ...box }));
  const moving = entries.find((card) => card.id === movingId);
  if (!moving) return layout;
  const others = entries
    .filter((card) => card.id !== movingId)
    .sort((a, b) => a.y - b.y || a.x - b.x);
  const placed = [moving];
  for (const card of others) {
    let guard = 0;
    while (placed.some((item) => collides(card, item)) && guard < 200) {
      const blockers = placed.filter((item) => collides(card, item));
      const lowest = blockers.reduce((best, item) => (item.y + item.h > best.y + best.h ? item : best));
      card.y = lowest.y + lowest.h;
      guard += 1;
    }
    placed.push(card);
  }
  return Object.fromEntries(placed.map(({ id, x, y, w, h }) => [id, { x, y, w, h }]));
}

/** 把任意来源的布局归一化到合法栅格；读不出来时返回 null 交给调用方用默认值。 */
export function normalizeLayout(cards) {
  if (!cards || typeof cards !== "object") return null;
  const result = {};
  for (const [id, box] of Object.entries(cards)) {
    if (!box || typeof box !== "object") continue;
    const x = clamp(Math.round(Number(box.x) || 0), 0, COLS - 1);
    const y = Math.max(0, Math.round(Number(box.y) || 0));
    const w = clamp(Math.round(Number(box.w) || 6), 1, COLS - x);
    const h = Math.max(MIN_ROWS, Math.round(Number(box.h) || MIN_ROWS));
    result[id] = { x, y, w, h };
  }
  return Object.keys(result).length ? result : null;
}

/** 读本地记忆；结构不对或存储不可用时返回 null（页面按默认布局渲染）。 */
export function readLayout(storage = globalThis.localStorage, key = STORAGE_KEY) {
  try {
    const raw = storage?.getItem(key);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (!parsed || parsed.version !== LAYOUT_VERSION) return null;
    const cards = normalizeLayout(parsed.cards);
    return cards ? { cards, locked: Boolean(parsed.locked) } : null;
  } catch (_error) {
    return null;
  }
}

/** 写本地记忆；失败只影响下次打开的布局，不影响当前使用。 */
export function writeLayout(cards, locked, storage = globalThis.localStorage, key = STORAGE_KEY) {
  try {
    storage?.setItem(key, JSON.stringify({ version: LAYOUT_VERSION, cards, locked }));
    return true;
  } catch (_error) {
    return false;
  }
}

export function clearLayout(storage = globalThis.localStorage, key = STORAGE_KEY) {
  try {
    storage?.removeItem(key);
  } catch (_error) {
    // 忽略
  }
}
