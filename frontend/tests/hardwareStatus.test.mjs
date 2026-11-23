import test from "node:test";
import assert from "node:assert/strict";
import { hardwarePillView } from "../src/state/hardwareStatus.js";

test("顶栏硬件胶囊：显示最近一次探测结果与时间，未探测为中性灰 off 不误用故障红", () => {
  const unprobed = hardwarePillView({ connected: false, summary: "未连接" });
  assert.equal(unprobed.text, "硬件 · 未探测");
  assert.equal(unprobed.tone, "off", "未探测应使用中性 off 色调而非红色故障态");
  assert.equal(hardwarePillView(null).online, false);
  assert.equal(hardwarePillView(null).tone, "off");
  const at = new Date(2026, 9, 14, 9, 5, 7).getTime();
  const ok = hardwarePillView({ connected: true, summary: "x", checkedAt: at });
  assert.equal(ok.online, true);
  assert.equal(ok.tone, "ok");
  assert.equal(ok.text, "硬件 · 握手通过");
  assert.match(ok.title, /09:05:07/);
  assert.match(ok.title, /不是实时状态/);
  const bad = hardwarePillView({ connected: false, summary: "握手超时", checkedAt: at });
  assert.equal(bad.tone, "bad", "已探测但未连通才显示红色告警态");
  assert.equal(bad.text, "硬件 · 未连接");
  assert.match(bad.title, /握手超时/);
});
