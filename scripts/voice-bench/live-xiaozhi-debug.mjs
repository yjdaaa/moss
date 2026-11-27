#!/usr/bin/env node
// VA4 真小智“文字直通”自动化联调脚本（供 AI 与开发者在本机直接执行）：
// - 启动前读取 `<runtime>/debug/voice-debug.token` 并检查在线小智设备；
// - 自动开启完整过程记录（ProcessTrace），跑完后恢复原状态；
// - 按语料逐轮向指定设备注入测试文字，等待每轮播完且设备回到空闲再发下一句（控制在 120s 空闲超时内）；
// - 在 `<runtime>/voice-bench/<run-id>/` 写出脱敏归档：turns.jsonl、timeline.jsonl、timeline.txt、report.md、summary.json；
// - 退出前打印归档目录路径；无在线设备或任一轮失败时返回非 0 状态码。

import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
export const DEFAULT_CORPUS_PATH = join(SCRIPT_DIR, "live-corpus.json");
export const NO_ONLINE_DEVICE_MESSAGE = "请先唤醒小智（喊‘你好小智’或按对话键），或开启常连模式";
export const DEFAULT_DEBUG_PORT = 18009;
export const DEFAULT_TURN_TIMEOUT_MS = 90_000;
export const DEFAULT_IDLE_WAIT_MS = 30_000;
export const DEFAULT_POLL_INTERVAL_MS = 250;

const STAGE_KEYS = [
  "jev_filter",
  "first_llm_delta",
  "llm_done",
  "first_synthesis_ready",
  "first_audio_frame",
  "turn_end",
];

/** 与 `laser_runtime::RuntimeRoot::default_base` 一致的运行目录推断。 */
export function defaultRuntimeDir(platform = process.platform, env = process.env, home = homedir()) {
  if (env.MOSS_RUNTIME_DIR && String(env.MOSS_RUNTIME_DIR).trim()) {
    return resolve(String(env.MOSS_RUNTIME_DIR).trim());
  }
  if (platform === "darwin") return join(home, "Library", "Application Support", "moss");
  if (platform === "win32") return join(env.APPDATA ?? join(home, "AppData", "Roaming"), "moss");
  return join(env.XDG_DATA_HOME ?? join(home, ".local", "share"), "moss");
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * 脱敏文本：清除 API 密钥、Bearer/设备令牌、带凭据 URL 与本机绝对路径（对齐仓库 `redact.rs` 与 `check-repo-hygiene.mjs`）。
 */
export function redactText(raw, opts = {}) {
  let out = String(raw ?? "");
  if (!out) return out;
  const extraSecrets = Array.isArray(opts.secrets) ? opts.secrets.filter((s) => typeof s === "string" && s.length >= 8) : [];
  for (const secret of extraSecrets) {
    out = out.replace(new RegExp(escapeRegExp(secret), "g"), "[REDACTED]");
  }
  const extraPaths = Array.isArray(opts.paths)
    ? opts.paths.filter((p) => typeof p === "string" && p.length >= 2).sort((a, b) => b.length - a.length)
    : [];
  for (const pathVal of extraPaths) {
    out = out.replace(new RegExp(escapeRegExp(pathVal), "g"), "<path>");
  }
  // URL 内嵌账号密码
  out = out.replace(/(https?|wss?):\/\/[^/\s@]+:[^/\s@]+@/gi, "$1://[REDACTED]@");
  // URL 查询参数里的敏感项
  out = out.replace(
    /([?&](?:api[_-]?key|apikey|token|access_token|secret|password|sig|key)=)[^&#\s"'<>]+/gi,
    "$1[REDACTED]",
  );
  // Bearer / Basic 令牌
  out = out.replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{6,}/gi, "$1 [REDACTED]");
  // 常见键值对凭据（JSON / header / CLI 参数）
  out = out.replace(
    /((?:api[_-]?key|apikey|authorization|access_token|device_token|token|secret|password|passwd)["']?\s*[:=]\s*["']?)(?:bearer\s+|basic\s+)?[^\s"',;&}]+/gi,
    "$1[REDACTED]",
  );
  out = out.replace(/(--(?:token|api-key|secret|password)\s+)[^\s"']+/gi, "$1[REDACTED]");
  // 常见密钥前缀与本机凭据包装
  out = out.replace(/\b(?:sk|rk|pk)-[A-Za-z0-9_-]{16,}\b/g, "[REDACTED]");
  out = out.replace(/\bgh[pousr]_[A-Za-z0-9]{20,}\b/g, "[REDACTED]");
  out = out.replace(/\b(?:dpapi-user-v1|keychain-user-v1):[A-Za-z0-9+/=_-]+/g, "[REDACTED]");
  // 本机绝对路径（macOS / Linux / Windows）
  out = out.replace(/(?:\/Users|\/home|\/var\/folders|\/private\/var|\/tmp)\/[^\s"'<>`,;)\]]+/g, "<path>");
  out = out.replace(/\b[A-Za-z]:\\(?:Users|Documents and Settings|ProgramData|Temp|Windows)[^\s"'<>`,;)\]]*/gi, "<path>");
  return out;
}

/** 递归脱敏任意 JSON 对象。 */
export function redactValue(value, opts = {}) {
  if (typeof value === "string") return redactText(value, opts);
  if (Array.isArray(value)) return value.map((item) => redactValue(item, opts));
  if (value && typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      if (/(?:^|_)(?:token|secret|password|api_?key|authorization)$/i.test(k) && typeof v === "string" && v.length > 0) {
        out[k] = "[REDACTED]";
      } else {
        out[k] = redactValue(v, opts);
      }
    }
    return out;
  }
  return value;
}

function percentile(values, p) {
  const nums = values.filter((v) => typeof v === "number" && Number.isFinite(v)).sort((a, b) => a - b);
  if (nums.length === 0) return null;
  const idx = Math.min(nums.length - 1, Math.max(0, Math.ceil((p / 100) * nums.length) - 1));
  return nums[idx];
}

function formatRunId(date = new Date()) {
  const pad = (n, w = 2) => String(n).padStart(w, "0");
  return [
    date.getFullYear(),
    pad(date.getMonth() + 1),
    pad(date.getDate()),
    "-",
    pad(date.getHours()),
    pad(date.getMinutes()),
    pad(date.getSeconds()),
    "-",
    pad(date.getMilliseconds(), 3),
  ].join("");
}

function localDayString(ms) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** 读取 settings.json 中的自定义 process_trace_dir，未设则返回 `<runtime>/logs/process-trace`。 */
export function resolveProcessTraceDir(runtimeDir) {
  const settingsFile = join(runtimeDir, "settings.json");
  if (existsSync(settingsFile)) {
    try {
      const parsed = JSON.parse(readFileSync(settingsFile, "utf8"));
      const custom = typeof parsed?.process_trace_dir === "string" ? parsed.process_trace_dir.trim() : "";
      if (custom && existsSync(custom)) return custom;
    } catch {
      // 忽略损坏配置，回退默认目录
    }
  }
  return join(runtimeDir, "logs", "process-trace");
}

function parseTimelineTxtTimeMs(line, dayStr) {
  const isoMatch = line.match(/^\[?(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?)\]?/);
  if (isoMatch) {
    const parsed = Date.parse(isoMatch[1]);
    if (Number.isFinite(parsed)) return parsed;
  }
  const hmsMatch = line.match(/^\[?(\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?)\]?/);
  if (hmsMatch && dayStr) {
    const parsed = Date.parse(`${dayStr}T${hmsMatch[1]}`);
    if (Number.isFinite(parsed)) return parsed;
  }
  return null;
}

/**
 * 截取 `[startMs, endMs]` 时间窗内的 ProcessTrace（timeline.jsonl 与 timeline.txt），并执行脱敏。
 */
export function sliceProcessTrace(traceDir, startMs, endMs, redactOpts = {}) {
  const jsonlLines = [];
  const txtLines = [];
  if (!traceDir || !existsSync(traceDir)) {
    return { jsonlLines, txtLines };
  }
  const minMs = startMs - 1000;
  const maxMs = endMs + 2000;
  const startDay = localDayString(minMs);
  const endDay = localDayString(maxMs);
  const dayDirs = readdirSync(traceDir)
    .filter((name) => /^\d{4}-\d{2}-\d{2}$/.test(name) && name >= startDay && name <= endDay)
    .filter((name) => {
      try {
        return statSync(join(traceDir, name)).isDirectory();
      } catch {
        return false;
      }
    })
    .sort();

  const matchedSeqs = new Set();
  for (const day of dayDirs) {
    const jsonlPath = join(traceDir, day, "timeline.jsonl");
    if (existsSync(jsonlPath)) {
      for (const rawLine of readFileSync(jsonlPath, "utf8").split("\n")) {
        const line = rawLine.trim();
        if (!line) continue;
        try {
          const obj = JSON.parse(line);
          const atMs = typeof obj?.at === "string" ? Date.parse(obj.at) : Number(obj?.at_ms);
          if (Number.isFinite(atMs) && atMs >= minMs && atMs <= maxMs) {
            if (obj?.seq !== undefined && obj?.seq !== null) matchedSeqs.add(String(obj.seq));
            jsonlLines.push(JSON.stringify(redactValue(obj, redactOpts)));
          }
        } catch {
          // 跳过截断行
        }
      }
    }
    const txtPath = join(traceDir, day, "timeline.txt");
    if (existsSync(txtPath)) {
      for (const rawLine of readFileSync(txtPath, "utf8").split("\n")) {
        if (!rawLine.trim()) continue;
        const seqMatch = rawLine.match(/#(\d+)\b/);
        const timeMs = parseTimelineTxtTimeMs(rawLine, day);
        const keepByTime = Number.isFinite(timeMs) && timeMs >= minMs && timeMs <= maxMs;
        const keepBySeq = seqMatch && matchedSeqs.has(seqMatch[1]);
        if (keepByTime || keepBySeq) {
          txtLines.push(redactText(rawLine, redactOpts));
        }
      }
    }
  }
  return { jsonlLines, txtLines };
}

/** 汇总统计（P50/P90 耗时、失败轮次、模拟门禁命中数、播放卡顿汇总）。 */
export function buildSummary({ runId, deviceId, corpusName, startedAtMs, finishedAtMs, turns }) {
  const executed = turns.filter((t) => t.verdict !== "skipped");
  const passed = turns.filter((t) => t.verdict === "passed");
  const failed = turns.filter((t) => t.verdict === "failed");
  const skipped = turns.filter((t) => t.verdict === "skipped");

  const stageLatency = {};
  for (const key of STAGE_KEYS) {
    const vals = executed
      .map((t) => t.detail?.stages_ms?.[key])
      .filter((v) => typeof v === "number" && Number.isFinite(v));
    stageLatency[key] = {
      count: vals.length,
      p50_ms: percentile(vals, 50),
      p90_ms: percentile(vals, 90),
    };
  }
  const wallVals = executed.map((t) => t.wall_ms).filter((v) => typeof v === "number" && Number.isFinite(v));
  const gateHits = executed.reduce((acc, t) => {
    const tools = Array.isArray(t.detail?.tools) ? t.detail.tools : [];
    return acc + tools.filter((tool) => tool?.blocked === true).length;
  }, 0);
  const stopSkipped = executed.filter((t) => Boolean(t.detail?.stop_skipped_reason)).length;
  const totalUnderruns = executed.reduce((acc, t) => acc + (Number(t.detail?.playback?.underruns) || 0), 0);
  const totalStallMs = executed.reduce((acc, t) => acc + (Number(t.detail?.playback?.stall_ms) || 0), 0);

  return {
    run_id: runId,
    device_id: deviceId,
    corpus: corpusName,
    started_at: new Date(startedAtMs).toISOString(),
    finished_at: new Date(finishedAtMs).toISOString(),
    duration_ms: Math.max(0, finishedAtMs - startedAtMs),
    counts: {
      total: turns.length,
      executed: executed.length,
      passed: passed.length,
      failed: failed.length,
      skipped: skipped.length,
    },
    gate_hits: gateHits,
    stop_skipped_count: stopSkipped,
    playback: {
      total_underruns: totalUnderruns,
      total_stall_ms: totalStallMs,
    },
    wall_ms: {
      p50_ms: percentile(wallVals, 50),
      p90_ms: percentile(wallVals, 90),
    },
    stages_ms: stageLatency,
    failed_turns: failed.map((t) => ({
      index: t.index,
      id: t.id,
      label: t.label,
      reason: t.failure_reason || "未知失败",
    })),
  };
}

function mdCell(text, maxLen = 80) {
  const clean = String(text ?? "").replace(/[\r\n|]+/g, " ").trim();
  if (!clean) return "—";
  return clean.length > maxLen ? `${clean.slice(0, maxLen)}…` : clean;
}

/** 生成诊断 Markdown 报告（VA4 `report.md`）。 */
export function buildReportMarkdown(summary, turns) {
  const lines = [
    `# 真小智文字直通联调报告（${summary.run_id}）`,
    "",
    `- **设备**：\`${summary.device_id}\``,
    `- **语料集**：${summary.corpus}`,
    `- **时间**：${summary.started_at} → ${summary.finished_at}（共 ${summary.duration_ms} ms）`,
    `- **结果**：总计 ${summary.counts.total} 轮 · 通过 ${summary.counts.passed} · 失败 ${summary.counts.failed} · 跳过 ${summary.counts.skipped}`,
    `- **安全门禁命中**：工具拒绝 ${summary.gate_hits} 次，模拟停止跳过 ${summary.stop_skipped_count} 次`,
    `- **播放卡顿**：欠载 ${summary.playback.total_underruns} 次，累计卡顿 ${summary.playback.total_stall_ms} ms`,
    `- **整轮耗时**：P50 = ${summary.wall_ms.p50_ms ?? "—"} ms，P90 = ${summary.wall_ms.p90_ms ?? "—"} ms`,
    "",
    "## 阶段耗时汇总（毫秒）",
    "",
    "| 阶段 | 样本数 | P50 | P90 |",
    "| --- | --- | --- | --- |",
  ];
  for (const key of STAGE_KEYS) {
    const row = summary.stages_ms[key] || {};
    lines.push(`| ${key} | ${row.count ?? 0} | ${row.p50_ms ?? "—"} | ${row.p90_ms ?? "—"} |`);
  }
  lines.push(
    "",
    "## 逐轮诊断表",
    "",
    "| # | 场景 | 路径 | 阶段耗时 (ms) | 工具与参数 | 模拟门禁拒绝 | 播放卡顿 | 回复摘要 | 结论 |",
    "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
  );
  for (const t of turns) {
    const d = t.detail || {};
    const stages = d.stages_ms || {};
    const stageStr = STAGE_KEYS.filter((k) => Number.isFinite(stages[k]))
      .map((k) => `${k}:${stages[k]}`)
      .join(" / ") || "—";
    const tools = Array.isArray(d.tools) ? d.tools : [];
    const toolsStr = tools.length
      ? tools.map((x) => `${x.name}(${mdCell(x.arguments, 40)}, ${x.ms ?? 0}ms)`).join("; ")
      : "—";
    const blockedStr = tools
      .filter((x) => x.blocked)
      .map((x) => `${x.name}: ${x.block_reason || "模拟门禁拒绝"}`)
      .concat(d.stop_skipped_reason ? [d.stop_skipped_reason] : [])
      .join("; ") || "—";
    const pb = d.playback || {};
    const stallStr = `${pb.underruns ?? 0} 次 / ${pb.stall_ms ?? 0} ms`;
    const replySummary = mdCell(d.reply || d.spoken || t.failure_reason || "", 60);
    const verdict =
      t.verdict === "passed"
        ? "通过"
        : t.verdict === "skipped"
          ? `跳过（${mdCell(t.skip_reason, 30)}）`
          : `失败（${mdCell(t.failure_reason, 40)}）`;
    lines.push(
      `| ${t.index} | ${mdCell(t.label || t.id, 24)} | ${d.route || "—"} | ${mdCell(stageStr, 60)} | ${mdCell(toolsStr, 80)} | ${mdCell(blockedStr, 60)} | ${stallStr} | ${replySummary} | ${verdict} |`,
    );
  }
  lines.push("");
  return lines.join("\n");
}

function isLoopbackUrl(raw) {
  try {
    const parsed = new URL(raw);
    return (
      parsed.protocol === "http:" &&
      (parsed.hostname === "127.0.0.1" || parsed.hostname === "localhost" || parsed.hostname === "[::1]")
    );
  } catch {
    return false;
  }
}

function resolveDebugConnection(opts) {
  const runtimeDir = opts.runtimeDir || defaultRuntimeDir();
  const tokenFile = opts.tokenFile || join(runtimeDir, "debug", "voice-debug.token");
  const metaFile = join(runtimeDir, "debug", "voice-debug.json");
  if (!existsSync(tokenFile)) {
    throw new Error(
      `找不到调试令牌文件（${redactText(tokenFile, { paths: [runtimeDir, homedir()] })}）；请先在桌面端“语音中心 › 服务”开启“允许本机 AI 调试注入”`,
    );
  }
  const token = readFileSync(tokenFile, "utf8").trim();
  if (!token) {
    throw new Error("调试令牌文件为空；请在桌面端重新开启“允许本机 AI 调试注入”");
  }
  let baseUrl = opts.url;
  if (!baseUrl) {
    let port = DEFAULT_DEBUG_PORT;
    if (existsSync(metaFile)) {
      try {
        const meta = JSON.parse(readFileSync(metaFile, "utf8"));
        if (Number.isInteger(meta?.port) && meta.port > 0) port = meta.port;
      } catch {
        // 回退默认端口
      }
    }
    baseUrl = `http://127.0.0.1:${port}`;
  }
  baseUrl = baseUrl.replace(/\/+$/, "");
  if (!isLoopbackUrl(baseUrl)) {
    throw new Error(`调试入口地址必须为本机回环 http://127.0.0.1:<port>，拒绝访问：${baseUrl}`);
  }
  return { runtimeDir, token, baseUrl };
}

async function debugFetch(baseUrl, token, path, { method = "GET", body = undefined, timeoutMs = 15_000 } = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(`${baseUrl}${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
      signal: controller.signal,
    });
    const text = await response.text();
    let json = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = { raw: text };
    }
    return { ok: response.ok, status: response.status, data: json };
  } finally {
    clearTimeout(timer);
  }
}

function sleep(ms) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, ms));
}

function isDeviceReadyForInject(dev) {
  if (!dev) return false;
  return dev.state === "idle" || (dev.state === "listening" && (Number(dev.level) || 0) === 0);
}

function evaluateTurnExpectations(turnSpec, detail) {
  const expect = turnSpec.expect || {};
  if (!detail) return "未获取到轮次明细";
  if (detail.outcome === "disconnected") return "本轮期间设备断开了连接";
  if (detail.error && !expect.allow_error) return `轮次执行出错：${detail.error}`;
  if (expect.route && detail.route && detail.route !== expect.route) {
    return `期望路径 ${expect.route}，实际为 ${detail.route}`;
  }
  if (Array.isArray(expect.corrected_includes)) {
    const corrected = String(detail.corrected_text || "");
    for (const needle of expect.corrected_includes) {
      if (!corrected.includes(needle)) {
        return `纠偏后文本未包含预期词“${needle}”（实际：“${corrected}”）`;
      }
    }
  }
  if (expect.expect_simulation_gate) {
    const tools = Array.isArray(detail.tools) ? detail.tools : [];
    const hasBlocked = tools.some((t) => t?.blocked === true);
    const reply = `${detail.reply || ""} ${detail.spoken || ""}`;
    const refusedInReply = /(拒绝|不能|无法|模拟|仅.*预览|禁止|不支持)/.test(reply);
    if (!hasBlocked && !refusedInReply) {
      return "未观察到模拟门禁拒绝或明确拒绝回复";
    }
  }
  return "";
}

/**
 * 执行完整联调并落盘脱敏归档。返回 `{ exitCode, archiveDir, summary, turns, errorMessage }`。
 */
export async function runLiveDebugBench(opts = {}) {
  const turnTimeoutMs = Number(opts.turnTimeoutMs ?? DEFAULT_TURN_TIMEOUT_MS);
  const pollIntervalMs = Number(opts.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS);
  const idleWaitMs = Number(opts.idleWaitMs ?? DEFAULT_IDLE_WAIT_MS);
  const corpusPath = resolve(opts.corpus || DEFAULT_CORPUS_PATH);
  const corpusRaw = JSON.parse(readFileSync(corpusPath, "utf8"));
  const corpusName = String(corpusRaw?.name || "custom");
  const corpusTurns = Array.isArray(corpusRaw) ? corpusRaw : Array.isArray(corpusRaw?.turns) ? corpusRaw.turns : [];
  if (corpusTurns.length === 0) {
    throw new Error(`语料文件不含有效轮次：${corpusPath}`);
  }

  const { runtimeDir, token, baseUrl } = resolveDebugConnection(opts);
  const redactOpts = {
    secrets: [token],
    paths: [runtimeDir, homedir(), process.cwd(), dirname(corpusPath)],
  };

  // 1. 启动前检查在线设备
  const devicesRes = await debugFetch(baseUrl, token, "/debug/v1/devices");
  if (!devicesRes.ok) {
    const msg = devicesRes.data?.message || `查询在线设备失败（HTTP ${devicesRes.status}）`;
    return { exitCode: 1, archiveDir: null, summary: null, turns: [], errorMessage: msg };
  }
  const onlineDevices = Array.isArray(devicesRes.data?.devices) ? devicesRes.data.devices : [];
  if (onlineDevices.length === 0) {
    return {
      exitCode: 1,
      archiveDir: null,
      summary: null,
      turns: [],
      errorMessage: NO_ONLINE_DEVICE_MESSAGE,
    };
  }
  const targetDevice = opts.device
    ? onlineDevices.find((d) => d.device_id === opts.device)
    : onlineDevices[0];
  if (!targetDevice) {
    return {
      exitCode: 1,
      archiveDir: null,
      summary: null,
      turns: [],
      errorMessage: `指定的设备 ${opts.device} 当前不在线（在线设备：${onlineDevices.map((d) => d.device_id).join(", ")}）`,
    };
  }
  const deviceId = targetDevice.device_id;

  // 2. 开启完整过程记录（并记住原始状态，结束必恢复）
  const runId = opts.runId || formatRunId();
  const startedAtMs = Date.now();
  let previousTraceEnabled = null;
  const enableTraceRes = await debugFetch(baseUrl, token, "/debug/v1/process-trace", {
    method: "POST",
    body: { enabled: true, flush: false },
  });
  if (enableTraceRes.ok && typeof enableTraceRes.data?.previous_enabled === "boolean") {
    previousTraceEnabled = enableTraceRes.data.previous_enabled;
  }

  const results = [];
  try {
    for (let i = 0; i < corpusTurns.length; i += 1) {
      const spec = corpusTurns[i];
      const turnIndex = i + 1;
      const turnIdLabel = String(spec.id || `turn_${turnIndex}`);
      const turnStartedAt = Date.now();

      // 等待设备空闲（控制在 120 秒空闲超时内）
      const waitDeadline = Date.now() + Math.min(idleWaitMs, 100_000);
      let latestDevices = null;
      let deviceReady = false;
      while (Date.now() <= waitDeadline) {
        const check = await debugFetch(baseUrl, token, "/debug/v1/devices");
        if (check.ok) {
          latestDevices = check.data;
          const dev = (check.data?.devices || []).find((d) => d.device_id === deviceId);
          if (!dev) break;
          if (isDeviceReadyForInject(dev)) {
            deviceReady = true;
            break;
          }
        }
        await sleep(pollIntervalMs);
      }

      if (spec.require_idle_laser) {
        const activeLaser = latestDevices?.active_laser_job === true;
        if (activeLaser) {
          results.push({
            index: turnIndex,
            id: turnIdLabel,
            label: spec.label || turnIdLabel,
            text: spec.text,
            verdict: "skipped",
            skip_reason: "检测到激光加工任务正在运行，跳过停止快路径测试",
            wall_ms: Date.now() - turnStartedAt,
            detail: null,
          });
          continue;
        }
      }

      if (!deviceReady) {
        results.push({
          index: turnIndex,
          id: turnIdLabel,
          label: spec.label || turnIdLabel,
          text: spec.text,
          verdict: "failed",
          failure_reason: "等待设备回到空闲超时或设备已离线",
          wall_ms: Date.now() - turnStartedAt,
          detail: null,
        });
        continue;
      }

      // 注入测试文字
      const injectRes = await debugFetch(
        baseUrl,
        token,
        `/debug/v1/devices/${encodeURIComponent(deviceId)}/inject`,
        { method: "POST", body: { text: spec.text } },
      );
      if (!injectRes.ok || !injectRes.data?.turn_id) {
        results.push({
          index: turnIndex,
          id: turnIdLabel,
          label: spec.label || turnIdLabel,
          text: spec.text,
          verdict: "failed",
          failure_reason: injectRes.data?.message || `注入文字失败（HTTP ${injectRes.status}）`,
          wall_ms: Date.now() - turnStartedAt,
          detail: null,
        });
        continue;
      }
      const turnId = injectRes.data.turn_id;

      // 轮询直到本轮结束且设备播完回到空闲（默认最多 90 秒）
      const turnDeadline = Date.now() + turnTimeoutMs;
      let finalDetail = null;
      let timedOut = true;
      while (Date.now() <= turnDeadline) {
        const pollRes = await debugFetch(baseUrl, token, `/debug/v1/turns/${encodeURIComponent(turnId)}`);
        if (pollRes.ok && pollRes.data) {
          finalDetail = pollRes.data;
          const terminal = finalDetail.status && finalDetail.status !== "running";
          const idleNow =
            !finalDetail.device_online ||
            finalDetail.device_state === "idle" ||
            (finalDetail.device_state === "listening" && (Number(finalDetail.device_level) || 0) === 0) ||
            finalDetail.route === "fast_exit";
          if (terminal && idleNow) {
            timedOut = false;
            break;
          }
        }
        await sleep(pollIntervalMs);
      }

      const wallMs = Date.now() - turnStartedAt;
      if (timedOut) {
        results.push({
          index: turnIndex,
          id: turnIdLabel,
          label: spec.label || turnIdLabel,
          text: spec.text,
          turn_id: turnId,
          verdict: "failed",
          failure_reason: `本轮在 ${turnTimeoutMs} ms 内未播完回到空闲（超时）`,
          wall_ms: wallMs,
          detail: finalDetail,
        });
        continue;
      }

      const checkErr = evaluateTurnExpectations(spec, finalDetail);
      results.push({
        index: turnIndex,
        id: turnIdLabel,
        label: spec.label || turnIdLabel,
        text: spec.text,
        turn_id: turnId,
        verdict: checkErr ? "failed" : "passed",
        failure_reason: checkErr || undefined,
        wall_ms: wallMs,
        detail: finalDetail,
      });
    }
  } finally {
    // 无论成败，刷新并恢复过程记录开关原状态
    if (previousTraceEnabled !== null) {
      try {
        await debugFetch(baseUrl, token, "/debug/v1/process-trace", {
          method: "POST",
          body: { enabled: previousTraceEnabled, flush: true },
        });
      } catch {
        // 尽力恢复，不掩盖主流程结果
      }
    }
  }

  const finishedAtMs = Date.now();
  const redactedTurns = results.map((row) => redactValue(row, redactOpts));
  const summary = redactValue(
    buildSummary({
      runId,
      deviceId,
      corpusName,
      startedAtMs,
      finishedAtMs,
      turns: redactedTurns,
    }),
    redactOpts,
  );
  const reportMd = redactText(buildReportMarkdown(summary, redactedTurns), redactOpts);
  const traceDir = opts.traceDir || resolveProcessTraceDir(runtimeDir);
  const { jsonlLines, txtLines } = sliceProcessTrace(traceDir, startedAtMs, finishedAtMs, redactOpts);

  const archiveDir = opts.outDir ? resolve(opts.outDir) : join(runtimeDir, "voice-bench", runId);
  mkdirSync(archiveDir, { recursive: true });
  writeFileSync(
    join(archiveDir, "turns.jsonl"),
    redactedTurns.map((t) => JSON.stringify(t)).join("\n") + (redactedTurns.length ? "\n" : ""),
    "utf8",
  );
  writeFileSync(
    join(archiveDir, "timeline.jsonl"),
    jsonlLines.join("\n") + (jsonlLines.length ? "\n" : ""),
    "utf8",
  );
  writeFileSync(
    join(archiveDir, "timeline.txt"),
    txtLines.join("\n") + (txtLines.length ? "\n" : ""),
    "utf8",
  );
  writeFileSync(join(archiveDir, "report.md"), reportMd, "utf8");
  writeFileSync(join(archiveDir, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`, "utf8");

  const exitCode = summary.counts.failed > 0 ? 1 : 0;
  return { exitCode, archiveDir, summary, turns: redactedTurns, errorMessage: null };
}

export function parseArgs(argv) {
  const opts = {
    corpus: null,
    device: null,
    runtimeDir: null,
    url: null,
    tokenFile: null,
    outDir: null,
    turnTimeoutMs: DEFAULT_TURN_TIMEOUT_MS,
    pollIntervalMs: DEFAULT_POLL_INTERVAL_MS,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--corpus") opts.corpus = argv[++i];
    else if (a === "--device") opts.device = argv[++i];
    else if (a === "--runtime-dir") opts.runtimeDir = argv[++i];
    else if (a === "--url") opts.url = argv[++i];
    else if (a === "--token-file") opts.tokenFile = argv[++i];
    else if (a === "--out-dir") opts.outDir = argv[++i];
    else if (a === "--turn-timeout-ms") opts.turnTimeoutMs = Number(argv[++i]);
    else if (a === "--poll-interval-ms") opts.pollIntervalMs = Number(argv[++i]);
    else if (a === "--help" || a === "-h") opts.help = true;
    else throw new Error(`未知参数：${a}`);
  }
  if (!Number.isFinite(opts.turnTimeoutMs) || opts.turnTimeoutMs < 500) {
    throw new Error("--turn-timeout-ms 须为不小于 500 的数字");
  }
  if (!Number.isFinite(opts.pollIntervalMs) || opts.pollIntervalMs < 20) {
    throw new Error("--poll-interval-ms 须为不小于 20 的数字");
  }
  return opts;
}

if (process.argv[1] && resolve(fileURLToPath(import.meta.url)) === resolve(process.argv[1])) {
  try {
    const opts = parseArgs(process.argv.slice(2));
    if (opts.help) {
      console.log(
        "用法: node scripts/voice-bench/live-xiaozhi-debug.mjs [--corpus <path>] [--device <id>] [--runtime-dir <path>] [--url http://127.0.0.1:<port>] [--token-file <path>] [--out-dir <path>] [--turn-timeout-ms 90000]",
      );
      process.exit(0);
    }
    const result = await runLiveDebugBench(opts);
    if (result.errorMessage) {
      console.error(result.errorMessage);
      process.exit(result.exitCode || 1);
    }
    console.log(result.archiveDir);
    if (result.exitCode !== 0) {
      console.error(
        `联调完成但存在失败轮次：通过 ${result.summary.counts.passed} / 失败 ${result.summary.counts.failed} / 跳过 ${result.summary.counts.skipped}`,
      );
      process.exit(result.exitCode);
    }
  } catch (error) {
    console.error(error?.message || String(error));
    process.exit(1);
  }
}
