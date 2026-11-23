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
  // n110：不限大小，只按天数到期删除
  const base = { retention_days: 30 };
  assert.equal(processTraceSummary({ ...base, bytes: 0, days: 0 }), "目前没有记录。只保留最近 30 天，不限大小。");
  assert.equal(
    processTraceSummary({ ...base, bytes: 3 * 1024 * 1024 * 1024, days: 2 }),
    "已记录 2 天，共 3.0 GB。只保留最近 30 天，不限大小。",
  );
});

test("完整过程记录：打开前的确认说明写清录音、看图照片、模型内容、激光生成与不保存密钥", () => {
  for (const phrase of ["录音", "看图的照片", "发给大模型的完整内容", "合成的语音", "激光生成", "加工进度", "时间线", "不保存任何密钥", "不限大小", "30 天", "降噪", "判停"]) {
    assert.match(PROCESS_TRACE_CONFIRM, new RegExp(phrase));
  }
  assert.match(PROCESS_TRACE_CONFIRM, /当前保存位置只保留最近 30 天/);
  assert.match(PROCESS_TRACE_CONFIRM, /旧记录留在原处，需自行清理/);
});

test("完整过程记录：目录不可用时不误报没有记录，恢复后显示实际占用", () => {
  const status = { enabled: true, custom_dir: "/tmp/trace/moss-process-trace", available: false, bytes: 0, days: 0, retention_days: 30 };
  assert.equal(processTraceSummary(status), "所选目录当前不可用，记录已暂停。目录恢复后继续记录，中断期间的内容不会补回。");
  assert.equal(processTraceSummary({ ...status, enabled: false }), "所选目录当前不可用，请接回硬盘或重新选择保存位置。");
  assert.match(processTraceSummary({ ...status, custom_dir: null }), /^保存目录当前不可用/);
  assert.match(processTraceSummary({ ...status, available: true, bytes: 1024, days: 1 }), /^已记录 1 天，共 1\.0 KB/);
});
