import test from "node:test";
import assert from "node:assert/strict";
import { actualParamsText, manualParamsPayload } from "../src/workbenchParams.js";

test("n73 工作台自定义参数：没开不提交，填对转成 manual_params，越界给出原因", () => {
  assert.equal(manualParamsPayload({ enabled: false, powerPercent: 60, feedRate: 1200, passes: 1 }), null);
  assert.deepEqual(manualParamsPayload({ enabled: true, powerPercent: "60", feedRate: "1500", passes: "2" }), {
    power_percent: 60, feed_rate: 1500, passes: 2,
  });
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 0, feedRate: 1200, passes: 1 }).error, /功率/);
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 60, feedRate: 50, passes: 1 }).error, /速度/);
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 60, feedRate: 1200.5, passes: 1 }).error, /速度/);
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 60, feedRate: 1200, passes: 6 }).error, /遍数/);
});

test("n73 刀路摘要显示这次实际用的参数并注明来源", () => {
  const wf = { artifacts: { actual_params: { laser_max_power: 370, feed_rate: 900, passes: 2, power_scale: 1000 } }, input: {} };
  assert.equal(actualParamsText(wf), "功率 37% · 900 mm/min · 2 遍（材料库推荐）");
  wf.input.manual_params = { power_percent: 37 };
  assert.equal(actualParamsText(wf), "功率 37% · 900 mm/min · 2 遍（本次自定义）");
  assert.equal(actualParamsText({}), "");
});
