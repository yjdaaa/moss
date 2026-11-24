import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { MIN_TURNS, defaultTraceDir, formatReport, readTimelines, review } from "../voice-bench/endpoint-review.mjs";

const decision = (session, turn, textWait, held, extra = 0) => ({
  domain: "voice", kind: "hold_decision", session_id: session, turn,
  held, text_wait_ms: textWait, total_wait_ms: textWait, extra_wait_ms: extra,
});
const continued = (session, turn, fields) => ({ domain: "voice", kind: "user_continued", session_id: session, turn, ...fields });

test("按句尾规则分档统计疑似抢话、接住的接着说和白等", () => {
  const records = [
    // 说完档：开播 600 ms 用户又出声 = 疑似抢话
    decision("s", 1, 250, false),
    continued("s", 1, { phase: "speaking", since_playback_ms: 600, text: "把功率调到", pause_estimate_ms: 700 }),
    // 说完档：开播 5 秒后插话是正常打断，不算抢话
    decision("s", 2, 250, false),
    continued("s", 2, { phase: "speaking", since_playback_ms: 5000, pause_estimate_ms: 6000 }),
    // 拿不准档：还没开口就接着说并合并 = 接住了
    decision("s", 3, 400, true, 150),
    continued("s", 3, { phase: "thinking", merged: true, too_late: false, pause_estimate_ms: 520 }),
    // 没说完档：多等了 1050 ms 但用户没再说 = 白等
    decision("s", 4, 1300, true, 1050),
    // 已开始执行工具后才接着说：也算抢话
    decision("s", 5, 400, true, 150),
    continued("s", 5, { phase: "thinking", merged: false, too_late: true, text: "雕刻这个", pause_estimate_ms: 480 }),
    // 没有判停结论（按键说话）：不计入
    continued("s", 6, { phase: "speaking", since_playback_ms: 300 }),
    // 别的领域的记录忽略
    { domain: "laser", kind: "hold_decision", session_id: "s", turn: 7, text_wait_ms: 250 },
  ];
  const r = review(records, { bargeWindowMs: 2000 });
  assert.equal(r.speculative_turns, 5);
  const byWait = Object.fromEntries(r.buckets.map((b) => [b.text_wait_ms, b]));
  assert.deepEqual([byWait[250].turns, byWait[250].early_cut], [2, 1]);
  assert.deepEqual([byWait[400].turns, byWait[400].early_cut, byWait[400].caught_follow_up], [2, 1, 1]);
  assert.deepEqual([byWait[1300].wasted_wait_turns, byWait[1300].wasted_wait_ms], [1, 1050]);
  assert.equal(byWait[400].wasted_wait_turns, 0, "接着说了就不算白等");
  assert.deepEqual(r.pause_ms, { count: 3, p50: 520, p90: 700 }, "只统计抢话与接住的停顿，正常打断不算");
  assert.deepEqual(r.early_cut_samples.map((s) => s.text), ["把功率调到", "雕刻这个"]);
  assert.match(r.notes[0], /样本只有 5 轮/, "样本太少不下结论");
  const text = formatReport(r, { dir: "<path>", dayCount: 1, badLines: 0 });
  assert.match(text, /说完（250 ms）：2 轮，疑似抢话 1（50\.0%）/);
  assert.match(text, /「把功率调到」 句尾规则 250 ms，实际等 250 ms，开播 600 ms 后用户又出声/);
});

test("样本够了才给结论：抢话率高提示试文本判停，停顿长提示自适应等待", () => {
  const records = [];
  for (let i = 0; i < MIN_TURNS; i += 1) {
    records.push(decision("s", i, 400, true, 150));
    if (i < 6) records.push(continued("s", i, { phase: "speaking", since_playback_ms: 500, pause_estimate_ms: 900 }));
    else if (i < 16) records.push(continued("s", i, { phase: "thinking", merged: true, pause_estimate_ms: 800 }));
  }
  const r = review(records);
  assert.match(r.notes[0], /20\.0% 疑似抢话，值得试中文文本判停模型/);
  assert.ok(r.notes.some((n) => /P90 约 900 ms.*自适应等待可能有用/.test(n)));
  assert.ok(r.notes.some((n) => /有 14 轮多等了但用户没再说（平均白等 150 ms）/.test(n)));
});

test("读最近几天的 timeline.jsonl，跳过坏行和过期目录", () => {
  const dir = mkdtempSync(join(tmpdir(), "endpoint-review-"));
  try {
    mkdirSync(join(dir, "2026-11-24"));
    mkdirSync(join(dir, "2026-10-01"));
    mkdirSync(join(dir, "attachments"));
    writeFileSync(
      join(dir, "2026-11-24", "timeline.jsonl"),
      `${JSON.stringify(decision("s", 1, 250, false))}\nnot json\n\n`,
    );
    writeFileSync(join(dir, "2026-10-01", "timeline.jsonl"), `${JSON.stringify(decision("s", 2, 250, false))}\n`);
    const out = readTimelines(dir, 30, new Date("2026-11-24T12:00:00Z"));
    assert.equal(out.records.length, 1);
    assert.equal(out.badLines, 1);
    assert.equal(out.dayCount, 1);
    assert.deepEqual(readTimelines(join(dir, "missing")), { records: [], badLines: 0, dayCount: 0 });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("默认目录与 laser_runtime::RuntimeRoot::default_base 一致", () => {
  assert.equal(defaultTraceDir("darwin", {}, "/h"), join("/h", "Library", "Application Support", "moss", "logs", "process-trace"));
  assert.equal(defaultTraceDir("win32", { APPDATA: "C:/A" }, "/h"), join("C:/A", "moss", "logs", "process-trace"));
  assert.equal(defaultTraceDir("linux", {}, "/h"), join("/h", ".local", "share", "moss", "logs", "process-trace"));
});
