import test from "node:test";
import assert from "node:assert/strict";
import { VOICE_TABS, voiceTabsFor } from "../src/voiceState.js";

test("官方链路没有模型标签，自建与内置链路保留全部标签", () => {
  const official = voiceTabsFor("official");
  assert.equal(official.some((item) => item.key === "models"), false, "官方链路的模型在小智控制台配置");
  assert.deepEqual(official.map((item) => item.key), ["service", "devices", "features", "privacy", "records"]);
  for (const mode of ["self_hosted", "builtin"]) {
    const tabs = voiceTabsFor(mode);
    assert.equal(tabs.length, VOICE_TABS.length, `${mode} 应显示全部标签`);
    assert.ok(tabs.some((item) => item.key === "models"), `${mode} 应有模型标签`);
  }
});
