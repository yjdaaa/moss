import test from "node:test";
import assert from "node:assert/strict";
import {
  getCurrentDialog,
  isHostMounted,
  parseLegacyMessage,
  resetDialogs,
  resolveDialog,
  showAlert,
  showChoice,
  showConfirm,
  subscribe,
} from "../src/appDialog.js";

test("parseLegacyMessage 拆分【标题】与正文", () => {
  const parsed = parseLegacyMessage("【发送失败 · 设备未连接】\n\n未检测到网络控制器连接。");
  assert.equal(parsed.title, "发送失败 · 设备未连接");
  assert.equal(parsed.body, "未检测到网络控制器连接。");

  const plain = parseLegacyMessage("请先生成测试矩阵");
  assert.equal(plain.title, "");
  assert.equal(plain.body, "请先生成测试矩阵");
});

test("未挂载 Host 时降级到原生弹窗且不会静默丢失", async () => {
  resetDialogs();
  assert.equal(isHostMounted(), false);
  const calls = [];
  globalThis.window = {
    alert: (text) => calls.push(["alert", text]),
    confirm: (text) => {
      calls.push(["confirm", text]);
      return true;
    },
  };
  try {
    await showAlert("【发送失败】\n\n原因 X", { tone: "error" });
    const ok = await showConfirm("删除吗？");
    assert.equal(ok, true);
    assert.deepEqual(calls, [
      ["alert", "发送失败\n\n原因 X"],
      ["confirm", "请确认\n\n删除吗？"],
    ]);
  } finally {
    delete globalThis.window;
  }
});

test("挂载 Host 后按队列顺序展示，并按用户操作 resolve", async () => {
  resetDialogs();
  const seen = [];
  const unsubscribe = subscribe((d) => seen.push(d ? d.title : null));
  assert.equal(isHostMounted(), true);

  const alertDone = showAlert("【发送失败 · 设备未连接】\n\n请先握手探测", { tone: "error" });
  const confirmDone = showConfirm("确定要删除材料吗？", { title: "删除材料", danger: true });

  let current = getCurrentDialog();
  assert.equal(current.kind, "alert");
  assert.equal(current.tone, "error");
  assert.equal(current.title, "发送失败 · 设备未连接");
  assert.equal(current.message, "请先握手探测");

  assert.equal(resolveDialog(current.id, true), true);
  await alertDone;

  current = getCurrentDialog();
  assert.equal(current.kind, "confirm");
  assert.equal(current.title, "删除材料");
  assert.equal(current.danger, true);
  resolveDialog(current.id, false);
  assert.equal(await confirmDone, false);

  assert.equal(getCurrentDialog(), null);
  assert.deepEqual(seen, [null, "发送失败 · 设备未连接", "删除材料", null]);

  unsubscribe();
  assert.equal(isHostMounted(), false);
});

test("显式 title 优先于消息中的【】标题；未知 tone 回退 info", async () => {
  resetDialogs();
  const unsubscribe = subscribe(() => {});
  const p = showAlert("【旧标题】\n\n正文", { title: "新标题", tone: "weird" });
  const current = getCurrentDialog();
  assert.equal(current.title, "新标题");
  assert.equal(current.message, "【旧标题】\n\n正文");
  assert.equal(current.tone, "info");
  resolveDialog(current.id, true);
  await p;
  unsubscribe();
});

// 2026-10-15：桌面端 Tauri 的 window.confirm 是返回 Promise 的异步函数，直接当布尔值会永远“确认”。
test("原生降级会等待异步 confirm，并在出错时按取消处理", async () => {
  const saved = globalThis.window;
  try {
    for (const [confirm, expectConfirm, expectChoice] of [
      [async () => false, false, "cancel"],
      [async () => true, true, "confirm"],
      [() => false, false, "cancel"],
      [async () => { throw new Error("not allowed"); }, false, "dismiss"],
      [undefined, false, "dismiss"],
    ]) {
      globalThis.window = { confirm, alert: async () => {} };
      assert.equal(await showConfirm("删除？"), expectConfirm);
      assert.equal(await showChoice("怎么办？"), expectChoice);
    }
    globalThis.window = { alert: async () => { throw new Error("blocked"); } };
    await showAlert("提示");
  } finally {
    globalThis.window = saved;
  }
});
