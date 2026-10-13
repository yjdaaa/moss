import test from "node:test";
import assert from "node:assert/strict";
import { onEmergencyStop, requestEmergencyStop } from "../src/estopBus.js";

test("顶栏急停只在页面确实发起停止时报告已处理", () => {
  const target = new EventTarget();
  assert.equal(requestEmergencyStop(target), false, "没有页面注册时不得假装已停机");

  let calls = 0;
  const off = onEmergencyStop(() => {
    calls += 1;
    return false; // 页面当前没有任务
  }, target);
  assert.equal(requestEmergencyStop(target), false);
  assert.equal(calls, 1);
  off();

  const offActive = onEmergencyStop(() => true, target);
  assert.equal(requestEmergencyStop(target), true);
  offActive();
  assert.equal(requestEmergencyStop(target), false, "页面卸载后不再处理");
});

test("多个处理函数时只执行到第一个真正处理的为止", () => {
  const target = new EventTarget();
  const order = [];
  onEmergencyStop(() => { order.push("a"); return true; }, target);
  onEmergencyStop(() => { order.push("b"); return true; }, target);
  assert.equal(requestEmergencyStop(target), true);
  assert.deepEqual(order, ["a"]);
});
