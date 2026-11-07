/**
 * 工作台自由画布的纯逻辑（2026-10-18 用户决定）：栅格常量、布局读写、碰撞与让位。
 *
 * 不依赖 React，便于单元测试（`frontend/tests/workbenchLayout.test.mjs`）。
 * 位置单位：x/w 是 0–48 的列号，y/h 是行数（CSS 里 1 行 = 4px，行间 8px 空隙）。
 */

export const STORAGE_KEY = "moss_wb_layout_v3";
// v2：2026-11-07 画布改为整行、设置卡移到画布下方，旧的“右侧竖排”记忆不再沿用
export const DRAW_STORAGE_KEY = "moss_draw_layout_v2";
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

/**
 * 创作画板默认排布（2026-11-07 用户决定“右边的设置放到下面，画板可以更好绘画”）：
 * 画布占满整行（62 行 ≈ 一屏），材料与尺寸 / 设备 / 预览与发送三张卡在画布下方并排。
 */
export const DRAW_DEFAULT_LAYOUT = {
  board: { x: 0, y: 0, w: 48, h: 62 },
  "draw-material": { x: 0, y: 63, w: 16, h: 28 },
  "draw-device": { x: 16, y: 63, w: 16, h: 22 },
  "draw-send": { x: 32, y: 63, w: 16, h: 28 },
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

/** 窄窗口两栏模式下，窄的一侧至少占的列数（48 列里的 20 列 ≈ 42%），避免表单卡被挤扁。 */
export const TWO_COLUMN_MIN = 20;

/** 宽度达到这么多列的卡片在两栏模式下仍占满整行（如整行的创作画布）。 */
export const FULL_ROW_MIN = 40;

/**
 * 窄窗口（761–1200px）自动两栏（2026-11-07 用户决定“可以并排显示 2 个，没必要做竖排”）：
 * 按上下顺序处理，宽度 ≥ FULL_ROW_MIN 的卡片单独占满整行；夹在整行卡之间的其余卡片按中心落在左半还是右半
 * 分成两栏，各栏按原来的上下顺序紧贴堆叠（行距 1 行），高度沿用布局里的行数。
 * 分界沿用左栏原来的宽度，但钳制在 [TWO_COLUMN_MIN, COLS - TWO_COLUMN_MIN]；整行卡输出 w = COLS。
 * 只用于显示，不写回记忆；用户拖过的宽布局回到宽窗口时原样恢复。
 */
export function stackTwoColumns(layout) {
  const byTop = (a, b) => a[1].y - b[1].y || a[1].x - b[1].x;
  const entries = Object.entries(layout || {}).filter(([, box]) => box).sort(byTop);
  const isFull = ([, box]) => box.w >= FULL_ROW_MIN;
  const leftCards = entries.filter((entry) => !isFull(entry) && entry[1].x + entry[1].w / 2 < COLS / 2);
  const leftEdge = leftCards.length ? Math.max(...leftCards.map(([, box]) => box.x + box.w)) : COLS / 2;
  const split = clamp(leftEdge, TWO_COLUMN_MIN, COLS - TWO_COLUMN_MIN);
  const out = {};
  let top = 0;
  const flush = (items) => {
    if (!items.length) return;
    let left = items.filter(([, box]) => box.x + box.w / 2 < COLS / 2);
    let right = items.filter(([, box]) => box.x + box.w / 2 >= COLS / 2);
    if (items.length > 1 && (left.length === 0 || right.length === 0)) {
      // 卡片都在同一侧：按上下顺序交替分到两栏，仍然并排
      left = items.filter((_, index) => index % 2 === 0);
      right = items.filter((_, index) => index % 2 === 1);
    }
    let bottom = top;
    for (const [column, x, w] of [[left, 0, split], [right, split, COLS - split]]) {
      let y = top;
      for (const [id, box] of column) {
        out[id] = { x, y, w, h: box.h };
        y += box.h + 1;
      }
      bottom = Math.max(bottom, y);
    }
    top = bottom;
  };
  let segment = [];
  for (const entry of entries) {
    if (!isFull(entry)) {
      segment.push(entry);
      continue;
    }
    flush(segment);
    segment = [];
    out[entry[0]] = { x: 0, y: top, w: COLS, h: entry[1].h };
    top += entry[1].h + 1;
  }
  flush(segment);
  return out;
}
