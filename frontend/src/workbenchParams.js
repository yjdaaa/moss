// n73（2026-11-08 用户决定）：工作台“这次自定义功率和速度”。只影响这一次生成的刀路，不写回材料库；
// 范围与后端 `manual_params` 校验一致（功率 1~100%、速度 100~6000 mm/min、遍数 1~5）。

export const MANUAL_LIMITS = {
  power: { min: 1, max: 100 },
  feed: { min: 100, max: 6000 },
  passes: { min: 1, max: 5 },
};

/** 自定义参数 → 生成请求里的 `manual_params`；没开自定义返回 null，填错返回 { error }。 */
export function manualParamsPayload({ enabled, powerPercent, feedRate, passes }) {
  if (!enabled) return null;
  const power = Number(powerPercent);
  const feed = Number(feedRate);
  const count = Number(passes);
  if (!Number.isFinite(power) || power < MANUAL_LIMITS.power.min || power > MANUAL_LIMITS.power.max) {
    return { error: "功率要在 1 ~ 100% 之间" };
  }
  if (!Number.isInteger(feed) || feed < MANUAL_LIMITS.feed.min || feed > MANUAL_LIMITS.feed.max) {
    return { error: "速度要是 100 ~ 6000 mm/min 的整数" };
  }
  if (!Number.isInteger(count) || count < MANUAL_LIMITS.passes.min || count > MANUAL_LIMITS.passes.max) {
    return { error: "遍数要是 1 ~ 5 的整数" };
  }
  return { power_percent: power, feed_rate: feed, passes: count };
}

/** 任务记录里这次实际用的参数，一行文字；没有参数返回空串。 */
export function actualParamsText(workflow) {
  const params = workflow?.artifacts?.actual_params;
  const power = Number(params?.laser_max_power);
  const feed = Number(params?.feed_rate);
  if (!Number.isFinite(power) || !Number.isFinite(feed)) return "";
  const scale = Number(params?.power_scale) > 0 ? Number(params.power_scale) : 1000;
  const passes = Number(params?.passes) || 1;
  const source = workflow?.input?.manual_params ? "本次自定义" : "材料库推荐";
  return `功率 ${Math.round((power / scale) * 100)}% · ${Math.round(feed)} mm/min · ${passes} 遍（${source}）`;
}
