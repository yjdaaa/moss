import test from "node:test";
import assert from "node:assert/strict";
import { HIDE_TOP_PILLS_KEY, readFoldOpen, readHideTopPills, writeFoldOpen, writeHideTopPills } from "../src/foldState.js";

function memory() {
  const map = new Map();
  return { getItem: (k) => (map.has(k) ? map.get(k) : null), setItem: (k, v) => map.set(k, String(v)) };
}

test("折叠状态按 id 记住，未记录时用默认值", () => {
  const storage = memory();
  assert.equal(readFoldOpen("wb-link", true, storage), true);
  assert.equal(readFoldOpen("twin-timeline", true, storage), true);
  assert.equal(readFoldOpen("settings-port", false, storage), false);
  writeFoldOpen("wb-link", false, storage);
  writeFoldOpen("twin-timeline", false, storage);
  assert.equal(readFoldOpen("wb-link", true, storage), false);
  assert.equal(readFoldOpen("twin-timeline", true, storage), false);
  writeFoldOpen("wb-link", true, storage);
  writeFoldOpen("twin-timeline", true, storage);
  assert.equal(readFoldOpen("wb-link", false, storage), true);
  assert.equal(readFoldOpen("twin-timeline", false, storage), true);
});

test("存储不可用时不抛错，退回默认值", () => {
  const broken = { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("denied"); } };
  assert.equal(readFoldOpen("x", true, broken), true);
  assert.doesNotThrow(() => writeFoldOpen("x", false, broken));
  assert.equal(readHideTopPills(broken), false);
  assert.doesNotThrow(() => writeHideTopPills(true, broken));
});

test("顶栏右上角状态胶囊隐藏开关：默认 false（不隐藏），开启后记住，关闭后恢复", () => {
  const storage = memory();
  assert.equal(HIDE_TOP_PILLS_KEY, "moss_hide_top_pills");
  assert.equal(readHideTopPills(storage), false);
  writeHideTopPills(true, storage);
  assert.equal(readHideTopPills(storage), true);
  writeHideTopPills(false, storage);
  assert.equal(readHideTopPills(storage), false);
});
