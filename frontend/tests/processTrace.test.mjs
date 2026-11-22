import assert from "node:assert/strict";
import test from "node:test";
import {
  PROCESS_TRACE_CONFIRM,
  formatTraceBytes,
  processTraceSummary,
} from "../src/processTrace.js";

test("完整过程记录：占用显示与状态行", () => {
  assert.equal(formatTraceBytes(0), "0 B");
  assert.equal(formatTraceBytes(512), "512 B");
  assert.equal(formatTraceBytes(1536), "1.5 KB");
  assert.equal(formatTraceBytes(5 * 1024 * 1024), "5.0 MB");
  assert.equal(formatTraceBytes(2 * 1024 * 1024 * 1024), "2.0 GB");
  assert.equal(formatTraceBytes(300 * 1024 * 1024), "300 MB");
  assert.equal(processTraceSummary(null), "读取中…");
  const base = { retention_days: 7, max_bytes: 2 * 1024 * 1024 * 1024 };
  assert.equal(processTraceSummary({ ...base, bytes: 0, days: 0 }), "目前没有记录。只保留最近 7 天，最多 2.0 GB。");
  assert.equal(
    processTraceSummary({ ...base, bytes: 3 * 1024 * 1024, days: 2 }),
    "已记录 2 天，共 3.0 MB。只保留最近 7 天，最多 2.0 GB。",
  );
});

test("完整过程记录：打开前的确认说明写清录音、看图照片、模型内容、激光生成与不保存密钥", () => {
  for (const phrase of ["录音", "看图的照片", "发给大模型的完整内容", "合成的语音", "激光生成", "加工进度", "时间线", "不保存任何密钥", "7 天"]) {
    assert.match(PROCESS_TRACE_CONFIRM, new RegExp(phrase));
  }
});
