#!/usr/bin/env node
// 判停复盘（n112，2026-11-24）：从“完整过程记录”的 timeline.jsonl 统计静音判停抢跑后，
// 有多少句其实没说完（回答开播不久用户又出声 = 疑似抢话）、多少句是白等（多等了但用户没再说），
// 以及用户真实的停顿长短，用来决定要不要接中文文本判停模型或做自适应等待。
//
// 用法：
//   node scripts/voice-bench/endpoint-review.mjs [--dir <过程记录目录>] [--days 30] [--barge-window-ms 2000] [--json]
// 不给 --dir 时读本机运行目录下的 logs/process-trace（在界面里改过保存目录的，用 --dir 指过去）。
// 只读记录，不改任何文件。

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

/** 与 `moss_voice_server::fast::endpoint_wait_ms` 的三档一致 */
export const BUCKETS = [
  { wait: 250, label: "说完（250 ms）" },
  { wait: 400, label: "拿不准（400 ms）" },
  { wait: 1300, label: "没说完（1300 ms）" },
];
/** 样本少于这么多轮时不给结论 */
export const MIN_TURNS = 30;

function percentile(sorted, p) {
  if (sorted.length === 0) return null;
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[idx];
}

/**
 * 统计一组过程记录（timeline.jsonl 的每一行解析后的对象）。
 * @param {object[]} records
 * @param {{bargeWindowMs?: number}} [opts]
 */
export function review(records, { bargeWindowMs = 2000 } = {}) {
  const turns = new Map();
  const keyOf = (r) => `${r.session_id ?? ""}#${r.turn ?? ""}`;
  for (const r of records) {
    if (r?.domain !== "voice") continue;
    if (r.kind !== "hold_decision" && r.kind !== "user_continued") continue;
    const key = keyOf(r);
    if (!turns.has(key)) turns.set(key, { decision: null, continued: [] });
    const t = turns.get(key);
    if (r.kind === "hold_decision") t.decision ??= r;
    else t.continued.push(r);
  }

  const buckets = new Map(
    BUCKETS.map((b) => [b.wait, { ...b, turns: 0, early: 0, caught: 0, wastedTurns: 0, wastedMs: 0 }]),
  );
  const other = { wait: null, label: "其他", turns: 0, early: 0, caught: 0, wastedTurns: 0, wastedMs: 0 };
  const pauses = [];
  const earlySamples = [];
  let speculative = 0;

  for (const { decision, continued } of turns.values()) {
    if (!decision) continue; // 不是静音判停抢跑的轮次（按键说话、设备结束拾音等）
    speculative += 1;
    const bucket = buckets.get(decision.text_wait_ms) ?? other;
    bucket.turns += 1;
    const early = continued.find(
      (c) =>
        c.too_late === true ||
        (c.phase === "speaking" && Number.isFinite(c.since_playback_ms) && c.since_playback_ms <= bargeWindowMs),
    );
    const caught = continued.find((c) => c.phase === "thinking" && c.merged === true);
    if (early) {
      bucket.early += 1;
      if (earlySamples.length < 10) {
        earlySamples.push({
          text: early.text ?? null,
          text_wait_ms: decision.text_wait_ms ?? null,
          total_wait_ms: decision.total_wait_ms ?? null,
          since_playback_ms: early.since_playback_ms ?? null,
          too_late: early.too_late === true,
        });
      }
    } else if (caught) {
      bucket.caught += 1;
    }
    for (const c of continued) {
      if (c === early || c === caught) {
        if (Number.isFinite(c.pause_estimate_ms)) pauses.push(c.pause_estimate_ms);
      }
    }
    if (decision.held === true && continued.length === 0) {
      bucket.wastedTurns += 1;
      bucket.wastedMs += Number(decision.extra_wait_ms) || 0;
    }
  }

  pauses.sort((a, b) => a - b);
  const rows = [...buckets.values(), ...(other.turns ? [other] : [])];
  const notes = [];
  if (speculative < MIN_TURNS) {
    notes.push(`样本只有 ${speculative} 轮（少于 ${MIN_TURNS} 轮），先开着完整过程记录多用几天再看，现在下结论不可靠。`);
  } else {
    const quick = rows.filter((r) => r.wait === 250 || r.wait === 400);
    const quickTurns = quick.reduce((n, r) => n + r.turns, 0);
    const quickEarly = quick.reduce((n, r) => n + r.early, 0);
    const rate = quickTurns ? quickEarly / quickTurns : 0;
    if (rate >= 0.05) {
      notes.push(
        `句尾规则判“说完 / 拿不准”的 ${quickTurns} 轮里有 ${(rate * 100).toFixed(1)}% 疑似抢话，值得试中文文本判停模型（先看下面的句子是哪类结尾）。`,
      );
    } else {
      notes.push(`句尾规则判“说完 / 拿不准”的轮次疑似抢话 ${(rate * 100).toFixed(1)}%（低于 5%），暂时不需要文本判停模型。`);
    }
    const p90 = percentile(pauses, 90);
    if (pauses.length >= 10 && p90 !== null && p90 > 400) {
      notes.push(`用户接着说之前的停顿 P90 约 ${p90} ms，比“拿不准”档的 400 ms 长，自适应等待可能有用。`);
    }
    const wasted = rows.reduce((n, r) => n + r.wastedTurns, 0);
    if (wasted > 0) {
      const wastedMs = rows.reduce((n, r) => n + r.wastedMs, 0);
      notes.push(`有 ${wasted} 轮多等了但用户没再说（平均白等 ${Math.round(wastedMs / wasted)} ms）。`);
    }
  }

  return {
    speculative_turns: speculative,
    barge_window_ms: bargeWindowMs,
    buckets: rows.map(({ wait, label, turns: n, early, caught, wastedTurns, wastedMs }) => ({
      text_wait_ms: wait,
      label,
      turns: n,
      early_cut: early,
      early_cut_rate: n ? early / n : 0,
      caught_follow_up: caught,
      wasted_wait_turns: wastedTurns,
      wasted_wait_ms: wastedMs,
    })),
    pause_ms: {
      count: pauses.length,
      p50: percentile(pauses, 50),
      p90: percentile(pauses, 90),
    },
    early_cut_samples: earlySamples,
    notes,
  };
}

/** 读目录下最近 `days` 天的 `<日期>/timeline.jsonl`；坏行跳过并计数。 */
export function readTimelines(dir, days = 30, now = new Date()) {
  const records = [];
  let badLines = 0;
  if (!existsSync(dir)) return { records, badLines, dayCount: 0 };
  const oldest = new Date(now);
  oldest.setDate(oldest.getDate() - (days - 1));
  const oldestDay = oldest.toISOString().slice(0, 10);
  const dayDirs = readdirSync(dir)
    .filter((name) => /^\d{4}-\d{2}-\d{2}$/.test(name) && name >= oldestDay)
    .filter((name) => statSync(join(dir, name)).isDirectory())
    .sort();
  for (const day of dayDirs) {
    const file = join(dir, day, "timeline.jsonl");
    if (!existsSync(file)) continue;
    for (const line of readFileSync(file, "utf8").split("\n")) {
      if (!line.trim()) continue;
      try {
        records.push(JSON.parse(line));
      } catch {
        badLines += 1;
      }
    }
  }
  return { records, badLines, dayCount: dayDirs.length };
}

export function defaultTraceDir(platform = process.platform, env = process.env, home = homedir()) {
  if (platform === "darwin") return join(home, "Library", "Application Support", "moss", "logs", "process-trace");
  if (platform === "win32") return join(env.APPDATA ?? join(home, "AppData", "Roaming"), "moss", "logs", "process-trace");
  return join(env.XDG_DATA_HOME ?? join(home, ".local", "share"), "moss", "logs", "process-trace");
}

export function formatReport(result, { dir, dayCount, badLines }) {
  const pct = (x) => `${(x * 100).toFixed(1)}%`;
  const lines = [`判停复盘：${dir}（${dayCount} 天，静音判停抢跑 ${result.speculative_turns} 轮）`];
  if (badLines) lines.push(`（跳过 ${badLines} 行无法解析的记录）`);
  lines.push("按句尾规则分档：");
  for (const b of result.buckets) {
    lines.push(
      `  ${b.label}：${b.turns} 轮，疑似抢话 ${b.early_cut}（${pct(b.early_cut_rate)}），` +
        `还没开口就接着说并接住 ${b.caught_follow_up}，白等 ${b.wasted_wait_turns} 轮（共 ${b.wasted_wait_ms} ms）`,
    );
  }
  const p = result.pause_ms;
  lines.push(
    p.count ? `用户接着说之前的停顿：${p.count} 次，中位 ${p.p50} ms，P90 ${p.p90} ms（估算）` : "用户接着说之前的停顿：没有记录",
  );
  if (result.early_cut_samples.length) {
    lines.push(`疑似抢话的句子（回答开播 ${result.barge_window_ms} ms 内用户又出声，或已执行工具后接着说）：`);
    for (const s of result.early_cut_samples) {
      const when = s.too_late ? "已开始执行工具" : `开播 ${s.since_playback_ms} ms 后`;
      lines.push(`  「${s.text ?? "（没记下原话）"}」 句尾规则 ${s.text_wait_ms} ms，实际等 ${s.total_wait_ms} ms，${when}用户又出声`);
    }
  }
  lines.push("结论：");
  for (const n of result.notes) lines.push(`  ${n}`);
  return lines.join("\n");
}

function parseArgs(argv) {
  const opts = { dir: null, days: 30, bargeWindowMs: 2000, json: false };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--dir") opts.dir = argv[++i];
    else if (a === "--days") opts.days = Number(argv[++i]);
    else if (a === "--barge-window-ms") opts.bargeWindowMs = Number(argv[++i]);
    else if (a === "--json") opts.json = true;
    else throw new Error(`未知参数：${a}`);
  }
  if (!Number.isInteger(opts.days) || opts.days < 1) throw new Error("--days 要是正整数");
  if (!Number.isFinite(opts.bargeWindowMs) || opts.bargeWindowMs < 0) throw new Error("--barge-window-ms 要是非负数");
  return opts;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const opts = parseArgs(process.argv.slice(2));
  const dir = opts.dir ?? defaultTraceDir();
  const { records, badLines, dayCount } = readTimelines(dir, opts.days);
  const result = review(records, { bargeWindowMs: opts.bargeWindowMs });
  if (opts.json) console.log(JSON.stringify({ dir, day_count: dayCount, bad_lines: badLines, ...result }, null, 2));
  else console.log(formatReport(result, { dir, dayCount, badLines }));
}
