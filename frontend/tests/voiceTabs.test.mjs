import test from "node:test";
import assert from "node:assert/strict";
import { VOICE_TABS, voiceTabsFor } from "../src/voiceState.js";

test("按链路过滤标签：官方链路无模型与隐私与家居，自建无隐私与家居，内置保留全部标签", () => {
  const official = voiceTabsFor("official");
  assert.equal(official.some((item) => item.key === "models"), false, "官方链路的模型在小智控制台配置");
  assert.equal(official.some((item) => item.key === "privacy"), false, "官方链路不显示仅内置生效的隐私与家居标签");
  assert.deepEqual(official.map((item) => item.key), ["service", "devices", "wake", "features", "records"]);

  const selfHosted = voiceTabsFor("self_hosted");
  assert.equal(selfHosted.some((item) => item.key === "models"), true, "自建链路显示模型标签");
  assert.equal(selfHosted.some((item) => item.key === "privacy"), false, "自建链路不显示仅内置生效的隐私与家居标签");
  assert.deepEqual(selfHosted.map((item) => item.key), ["service", "models", "devices", "wake", "features", "records"]);

  const builtin = voiceTabsFor("builtin");
  assert.equal(builtin.length, VOICE_TABS.length, "builtin 显示全部 7 个标签");
  assert.ok(builtin.some((item) => item.key === "privacy"), "builtin 显示隐私与家居标签");
});
