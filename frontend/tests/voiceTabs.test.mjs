import test from "node:test";
import assert from "node:assert/strict";
import { VOICE_TABS, normalizeVoiceTab, voiceTabsFor } from "../src/voiceState.js";

test("按链路过滤标签：官方链路无模型与隐私与家居，自建无隐私与家居，内置保留全部标签", () => {
  const official = voiceTabsFor("official");
  assert.equal(official.some((item) => item.key === "models"), false, "官方链路的模型在小智控制台配置");
  assert.equal(official.some((item) => item.key === "privacy"), false, "官方链路不显示仅内置生效的隐私与家居标签");
  assert.deepEqual(official.map((item) => item.key), ["service", "devices", "features", "records"]);

  const selfHosted = voiceTabsFor("self_hosted");
  assert.equal(selfHosted.some((item) => item.key === "models"), true, "自建链路显示模型标签");
  assert.equal(selfHosted.some((item) => item.key === "privacy"), false, "自建链路不显示仅内置生效的隐私与家居标签");
  assert.deepEqual(selfHosted.map((item) => item.key), ["service", "devices", "models", "features", "records"]);

  const builtin = voiceTabsFor("builtin");
  assert.equal(builtin.length, VOICE_TABS.length, "builtin 显示全部 6 个标签");
  assert.ok(builtin.some((item) => item.key === "privacy"), "builtin 显示隐私与家居标签");
});

test("n69：内置链路 6 个标签依次为 服务 / 设备 / 模型 / 扩展 / 隐私与家居 / 记录与诊断", () => {
  assert.deepEqual(
    voiceTabsFor("builtin").map((item) => item.label),
    ["服务", "设备", "模型", "扩展", "隐私与家居", "记录与诊断"],
  );
  assert.equal(VOICE_TABS.some((item) => item.key === "wake"), false, "“唤醒词与音量”已并入“设备”");
});

test("n69：记住的旧标签 wake 落到设备标签，未知值返回空串", () => {
  assert.equal(normalizeVoiceTab("wake"), "devices");
  assert.equal(normalizeVoiceTab("models"), "models");
  assert.equal(normalizeVoiceTab("nope"), "");
  assert.equal(normalizeVoiceTab(null), "");
});
