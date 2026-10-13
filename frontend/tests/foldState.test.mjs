import test from "node:test";
import assert from "node:assert/strict";
import { readFoldOpen, writeFoldOpen } from "../src/foldState.js";

function memory() {
  const map = new Map();
  return { getItem: (k) => (map.has(k) ? map.get(k) : null), setItem: (k, v) => map.set(k, String(v)) };
}

test("折叠状态按 id 记住，未记录时用默认值", () => {
  const storage = memory();
  assert.equal(readFoldOpen("wb-link", true, storage), true);
  assert.equal(readFoldOpen("settings-port", false, storage), false);
  writeFoldOpen("wb-link", false, storage);
  assert.equal(readFoldOpen("wb-link", true, storage), false);
  writeFoldOpen("wb-link", true, storage);
  assert.equal(readFoldOpen("wb-link", false, storage), true);
});

test("存储不可用时不抛错，退回默认值", () => {
  const broken = { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("denied"); } };
  assert.equal(readFoldOpen("x", true, broken), true);
  assert.doesNotThrow(() => writeFoldOpen("x", false, broken));
});
