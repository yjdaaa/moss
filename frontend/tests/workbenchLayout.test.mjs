import test from "node:test";
import assert from "node:assert/strict";
import {
  COLS, DEFAULT_LAYOUT, DRAW_DEFAULT_LAYOUT, DRAW_STORAGE_KEY, MIN_ROWS, STORAGE_KEY,
  clearLayout, collides, normalizeLayout, readLayout, reflowVertical, settle, writeLayout,
} from "../src/workbenchLayout.js";

/** 画布上任意两张卡都不允许重叠。 */
function assertNoOverlap(layout, label) {
  const boxes = Object.entries(layout);
  for (let i = 0; i < boxes.length; i += 1) {
    for (let j = i + 1; j < boxes.length; j += 1) {
      const [idA, a] = boxes[i];
      const [idB, b] = boxes[j];
      assert.equal(collides(a, b), false, `${label}：${idA} 与 ${idB} 重叠`);
    }
  }
}

function fakeStorage(initial = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => map.set(key, String(value)),
    removeItem: (key) => map.delete(key),
    dump: () => Object.fromEntries(map),
  };
}

test("默认布局本身不重叠，且都在 48 列之内（左 16 列三卡 + 右 32 列监视舱与反馈双栏对齐）", () => {
  assertNoOverlap(DEFAULT_LAYOUT, "默认布局");
  for (const [id, box] of Object.entries(DEFAULT_LAYOUT)) {
    assert.ok(box.x >= 0 && box.x + box.w <= COLS, `${id} 超出列范围`);
    assert.ok(box.h >= MIN_ROWS, `${id} 高度低于最小值`);
  }
  assert.equal(DEFAULT_LAYOUT.link.x, 0);
  assert.equal(DEFAULT_LAYOUT.source.x, 0);
  assert.equal(DEFAULT_LAYOUT.material.x, 0);
  assert.equal(DEFAULT_LAYOUT.link.w, 16);
  assert.equal(DEFAULT_LAYOUT.monitor.x, 16);
  assert.equal(DEFAULT_LAYOUT.feedback.x, 16);
  assert.equal(DEFAULT_LAYOUT.monitor.w, 32);
  assert.equal(DEFAULT_LAYOUT.feedback.w, 32);
  assert.equal(
    DEFAULT_LAYOUT.material.y + DEFAULT_LAYOUT.material.h,
    DEFAULT_LAYOUT.feedback.y + DEFAULT_LAYOUT.feedback.h,
    "默认双栏左右底边应齐平",
  );
  assertNoOverlap(DRAW_DEFAULT_LAYOUT, "创作画板默认布局");
  for (const [id, box] of Object.entries(DRAW_DEFAULT_LAYOUT)) {
    assert.ok(box.x >= 0 && box.x + box.w <= COLS, `${id} 超出列范围`);
    assert.ok(box.h >= MIN_ROWS, `${id} 高度低于最小值`);
  }
});

test("拖到别人头上时把它往下推，且结果仍不重叠", () => {
  const moved = settle({ ...DEFAULT_LAYOUT, monitor: { x: 0, y: 0, w: 6, h: 84 } }, "monitor");
  assertNoOverlap(moved, "让位后");
  assert.deepEqual(moved.monitor, { x: 0, y: 0, w: 6, h: 84 }, "被拖的卡保持原位");
  assert.ok(moved.link.y >= 84, `记忆卡被推到监视舱下面，实际 y=${moved.link.y}`);
});

test("不冲突的卡片位置不动", () => {
  // 把材质卡挪到右下方的空位：其它卡都不应该被推动
  const moved = settle({ ...DEFAULT_LAYOUT, material: { x: 24, y: 120, w: 12, h: 35 } }, "material");
  assert.deepEqual(moved.link, DEFAULT_LAYOUT.link);
  assert.deepEqual(moved.monitor, DEFAULT_LAYOUT.monitor);
  assert.deepEqual(moved.feedback, DEFAULT_LAYOUT.feedback);
});

test("放大一张卡会推挤同一列的卡", () => {
  const grown = settle({ ...DEFAULT_LAYOUT, link: { ...DEFAULT_LAYOUT.link, h: 60 } }, "link");
  assertNoOverlap(grown, "放大后");
  assert.ok(grown.source.y >= DEFAULT_LAYOUT.link.y + 60, `加工内容被推下去，实际 y=${grown.source.y}`);
});

test("读回的脏数据会被夹到合法栅格", () => {
  const normalized = normalizeLayout({
    a: { x: 20, y: -5, w: 99, h: 1 },
    b: { x: "3", y: "4", w: "2", h: "10" },
    broken: null,
  });
  assert.deepEqual(normalized.a, { x: 20, y: 0, w: COLS - 20, h: MIN_ROWS });
  assert.deepEqual(normalized.b, { x: 3, y: 4, w: 2, h: MIN_ROWS });
  assert.equal(normalized.broken, undefined);
  assert.equal(normalizeLayout({}), null);
  assert.equal(normalizeLayout(null), null);
});

test("布局记忆能写能读，版本不对时按默认处理", () => {
  const storage = fakeStorage();
  const cards = { link: { x: 1, y: 2, w: 4, h: 30 } };
  assert.equal(writeLayout(cards, true, storage), true);
  const loaded = readLayout(storage);
  assert.deepEqual(loaded, { cards, locked: true });
  // 版本升级后旧结构不再生效，页面回到默认布局
  storage.setItem(STORAGE_KEY, JSON.stringify({ version: 99, cards }));
  assert.equal(readLayout(storage), null);
  // 内容坏掉时同样不影响使用
  storage.setItem(STORAGE_KEY, "{not json");
  assert.equal(readLayout(storage), null);
  clearLayout(storage);
  assert.equal(readLayout(storage), null);
  // 支持按自定义 storageKey（如创作画板）独立读写与清空
  assert.equal(writeLayout(DRAW_DEFAULT_LAYOUT, false, storage, DRAW_STORAGE_KEY), true);
  assert.deepEqual(readLayout(storage, DRAW_STORAGE_KEY), { cards: DRAW_DEFAULT_LAYOUT, locked: false });
  assert.equal(readLayout(storage, STORAGE_KEY), null);
  clearLayout(storage, DRAW_STORAGE_KEY);
  assert.equal(readLayout(storage, DRAW_STORAGE_KEY), null);
});

test("存储不可用时读写都不抛异常", () => {
  const broken = {
    getItem: () => { throw new Error("denied"); },
    setItem: () => { throw new Error("denied"); },
    removeItem: () => { throw new Error("denied"); },
  };
  assert.equal(readLayout(broken), null);
  assert.equal(writeLayout(DEFAULT_LAYOUT, false, broken), false);
  clearLayout(broken);
});

test("收起或调矮上方卡片时，同列下方卡片自动往上贴合；展开时再推回", () => {
  const shrunk = reflowVertical(DEFAULT_LAYOUT, "link", {
    ...DEFAULT_LAYOUT.link,
    h: 5,
  });
  assertNoOverlap(shrunk, "收起后");
  const delta = DEFAULT_LAYOUT.link.h - 5;
  assert.equal(shrunk.source.y, DEFAULT_LAYOUT.source.y - delta, "同列紧邻的加工内容卡向上贴合");
  assert.equal(shrunk.material.y, DEFAULT_LAYOUT.material.y - delta, "同列更下方的材质卡同步向上贴合");
  assert.equal(shrunk.monitor.y, DEFAULT_LAYOUT.monitor.y, "其它列的监视舱位置不变");

  const restored = reflowVertical(shrunk, "link", {
    ...shrunk.link,
    h: DEFAULT_LAYOUT.link.h,
  });
  assertNoOverlap(restored, "展开恢复后");
  assert.equal(restored.source.y, DEFAULT_LAYOUT.source.y, "展开后加工内容卡回到原位");
  assert.equal(restored.material.y, DEFAULT_LAYOUT.material.y, "展开后材质卡回到原位");
});
