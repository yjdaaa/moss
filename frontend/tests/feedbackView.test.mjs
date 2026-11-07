import test from "node:test";
import assert from "node:assert/strict";
import { feedbackSourceLabel } from "../src/feedbackView.js";

test("加工反馈来源：本机 Jev 不再被标成 OpenRouter", () => {
  assert.equal(feedbackSourceLabel({ source: "openrouter" }), "OpenRouter Jev");
  assert.equal(feedbackSourceLabel({ source: "local_jev", decision: { model: "onejev_1_6_4b" } }), "本机 Jev · onejev_1_6_4b");
  assert.equal(feedbackSourceLabel({ source: "local_jev" }), "本机 Jev");
  assert.equal(feedbackSourceLabel({ source: "local_rules" }), "本地规则");
  assert.equal(feedbackSourceLabel(null), "本地规则");
});
