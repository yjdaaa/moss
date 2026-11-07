// n68（2026-11-07）记录与诊断：大白话映射、错误文字兜底、结果标签、事件筛选与覆盖提示。全部用合成数据。
import test from "node:test";
import assert from "node:assert/strict";
import {
  VOICE_DIAG_CATEGORIES,
  VOICE_OUTCOMES,
  approvalListSummary,
  approvalOutcomeReason,
  filterVoiceEvents,
  formatEventTime,
  voiceErrorText,
  voiceEventDropNotices,
  voiceEventView,
  voiceOutcomeLabel,
} from "../src/voiceState.js";

test("诊断分类闭集与 Rust DiagCategory 一致，每类都有大白话原因；出问题的类有下一步", () => {
  assert.deepEqual(Object.keys(VOICE_DIAG_CATEGORIES), [
    "general", "network", "auth", "quota", "config", "device", "firmware", "model", "audio", "internal",
  ]);
  for (const [key, meta] of Object.entries(VOICE_DIAG_CATEGORIES)) {
    assert.ok(meta.label && meta.cause, key);
    if (key !== "general") assert.ok(meta.next, `${key} 需要下一步建议`);
  }
  const causes = new Set(Object.values(VOICE_DIAG_CATEGORIES).map((meta) => meta.cause));
  assert.equal(causes.size, 10, "每一类的原因说法互不相同，用户能分清是哪类问题");
});

test("voiceErrorText：错误码优先、附具体原因与建议，任何输入都不输出 [object Object]", () => {
  assert.equal(voiceErrorText({ code: "voice_acl_denied", message: "" }), "这个操作只能在 MOSS 主窗口里做。");
  assert.equal(
    voiceErrorText({ code: "builtin_start_failed", message: "端口 8000 已被占用" }),
    "内置语音服务没能启动。具体原因：端口 8000 已被占用",
  );
  assert.equal(voiceErrorText({ code: "unknown_x", message: "具体原因" }), "具体原因");
  assert.equal(voiceErrorText({ code: "unknown_x" }), "操作失败（错误码 unknown_x）");
  assert.equal(voiceErrorText({ category: "quota" }), VOICE_DIAG_CATEGORIES.quota.cause);
  assert.equal(
    voiceErrorText({ message: "大模型返回 HTTP 401，密钥无效或没有权限", category: "auth", hint: "重新填写密钥" }),
    "大模型返回 HTTP 401，密钥无效或没有权限。重新填写密钥",
  );
  assert.equal(voiceErrorText({ message: { message: "嵌套原因" } }), "嵌套原因");
  assert.equal(voiceErrorText(new Error("网络连接失败")), "网络连接失败");
  assert.equal(voiceErrorText(null), "操作失败（没有收到具体原因）");
  assert.equal(voiceErrorText({}), "操作失败（没有收到具体原因）");
  assert.equal(voiceErrorText(42), "42");
  for (const input of [{}, { foo: 1 }, { message: {} }, { message: { a: 1 } }, [], { code: 5 }]) {
    assert.doesNotMatch(voiceErrorText(input), /\[object Object\]/, JSON.stringify(input));
  }
  assert.match(voiceErrorText({ foo: "bar" }), /原始信息：\{"foo":"bar"\}/);
});

test("每种对话结果都有中文标签，未知结果原样标出不冒充“未完成”", () => {
  for (const outcome of ["running", "done", "interrupted", "ignored", "music_started", "disconnected", "failed"]) {
    const label = voiceOutcomeLabel(outcome);
    assert.equal(label, VOICE_OUTCOMES[outcome]);
    assert.doesNotMatch(label.text, /[a-z_]/, `${outcome} 必须是中文`);
  }
  assert.equal(voiceOutcomeLabel("ignored").text, "没识别出文字，已忽略");
  assert.equal(voiceOutcomeLabel("disconnected").text, "设备断开，本轮中止");
  assert.deepEqual(voiceOutcomeLabel("weird"), { text: "未知结果（weird）", tone: "off" });
  assert.equal(voiceOutcomeLabel(undefined).text, "未知结果");
});

test("事件视图：级别、分类、下一步（自带优先，问题类缺省用分类建议）、毫秒时间与技术细节", () => {
  const at = new Date(2026, 10, 7, 9, 5, 3, 45).getTime();
  const error = voiceEventView({ seq: 7, at: Math.floor(at / 1000), at_ms: at, level: "error", category: "auth", text: "对话出错：大模型返回 HTTP 401", hint: "重新填写密钥", detail: "服务方原话：invalid key" });
  assert.equal(error.levelText, "错误");
  assert.equal(error.tone, "bad");
  assert.equal(error.categoryLabel, "密钥/权限");
  assert.equal(error.hint, "重新填写密钥");
  assert.equal(error.detail, "服务方原话：invalid key");
  assert.equal(error.time, "11-07 09:05:03.045");
  assert.equal(error.key, 7);
  const warn = voiceEventView({ at: 100, level: "warn", category: "device", text: "设备断开" });
  assert.equal(warn.hint, VOICE_DIAG_CATEGORIES.device.next, "没有自带建议时用分类建议");
  const info = voiceEventView({ at: 100, text: "识别完成：7 字" });
  assert.equal(info.level, "info");
  assert.equal(info.category, "general");
  assert.equal(info.hint, "", "日常记录不硬塞建议");
  const legacy = voiceEventView({ at: Math.floor(at / 1000), text: "旧格式" });
  assert.equal(legacy.time, "11-07 09:05:03", "只有秒时不编造毫秒");
  assert.equal(formatEventTime({}), "—");
  assert.equal(voiceEventView({ level: "panic", category: "nope", text: "x" }).level, "info");
});

test("“只看错误和警告”筛选保持新的在前；覆盖计数明确提示", () => {
  const events = [
    { seq: 4, level: "info", text: "a" },
    { seq: 3, level: "error", text: "b" },
    { seq: 2, level: "warn", text: "c" },
    { seq: 1, text: "d" },
  ];
  assert.deepEqual(filterVoiceEvents(events, true).map((e) => e.seq), [3, 2]);
  assert.equal(filterVoiceEvents(events, false), events);
  assert.deepEqual(filterVoiceEvents(null, true), []);
  assert.deepEqual(voiceEventDropNotices({ info: 0, problems: 0, timings: 0 }), []);
  assert.deepEqual(voiceEventDropNotices(undefined), []);
  assert.deepEqual(voiceEventDropNotices({ info: 40, problems: 2, timings: 5, max_info: 300, max_problems: 200, max_timings: 50 }), [
    "有 2 条较早的错误或警告已被覆盖（最多保留 200 条）",
    "有 40 条较早的日常记录已被覆盖（最多保留 300 条）",
    "有 5 轮较早的耗时记录已被覆盖（最多保留 50 轮）",
  ]);
});

test("审批列表标题不把 50 条上限当总数；失败原因带错误码", () => {
  assert.equal(approvalListSummary(0, 50, ""), "暂无");
  assert.equal(approvalListSummary(12, 50, ""), "最近 12 条");
  assert.equal(approvalListSummary(50, 50, ""), "最近 50 条（只显示最近 50 条）");
  assert.equal(approvalListSummary(3, 0, ""), "最近 3 条");
  assert.equal(approvalListSummary(3, 50, "网络连接失败"), "读取失败");
  assert.equal(approvalOutcomeReason({ outcome: { success: false, message: "控制器忙", error_code: "controller_busy" } }), "控制器忙（错误码 controller_busy）");
  assert.equal(approvalOutcomeReason({ outcome: { success: false, message: "", error_code: "approval_mismatch" } }), "错误码 approval_mismatch");
  assert.equal(approvalOutcomeReason({ outcome: { success: true, message: "已完成" } }), "");
  assert.equal(approvalOutcomeReason({}), "");
});
