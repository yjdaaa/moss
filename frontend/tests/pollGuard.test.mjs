import test from "node:test";
import assert from "node:assert/strict";
import { createPollRunner } from "../src/pollGuard.js";

test("轮询守卫：请求未返回时跳过节拍，不叠加并发请求", async () => {
  let inFlight = 0;
  let maxInFlight = 0;
  let calls = 0;
  const releases = [];
  const tick = createPollRunner(async () => {
    calls += 1;
    inFlight += 1;
    maxInFlight = Math.max(maxInFlight, inFlight);
    await new Promise((resolve) => releases.push(resolve));
    inFlight -= 1;
  });
  const first = tick();
  assert.equal(await tick(), false);
  assert.equal(await tick(), false);
  assert.equal(calls, 1);
  releases.shift()();
  assert.equal(await first, true);
  const second = tick();
  releases.shift()();
  await second;
  assert.equal(calls, 2);
  assert.equal(maxInFlight, 1);
});

test("轮询守卫：invalidate 之后返回的旧响应被识别为过期；任务抛错也会释放占用", async () => {
  const applied = [];
  let release;
  const tick = createPollRunner(async (isCurrent) => {
    const value = await new Promise((resolve) => { release = resolve; });
    if (isCurrent()) applied.push(value);
  });
  const pending = tick();
  tick.invalidate();
  release("stale");
  await pending;
  assert.deepEqual(applied, []);
  const fresh = tick();
  release("fresh");
  await fresh;
  assert.deepEqual(applied, ["fresh"]);

  const failing = createPollRunner(async () => { throw new Error("boom"); });
  assert.equal(await failing(), true);
  assert.equal(failing.isPending(), false);
});
