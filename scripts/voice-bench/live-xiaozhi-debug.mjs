#!/usr/bin/env node
// VA4 真小智“文字直通”自动化联调脚本（供 AI 与开发者在本机直接执行）：
// - 启动前读取 `<runtime>/debug/voice-debug.token` 并检查在线小智设备；
// - 自动开启完整过程记录（ProcessTrace），跑完后恢复原状态；
// - 按语料逐轮向指定设备注入测试文字，等待每轮播完且设备回到空闲再发下一句（控制在 120s 空闲超时内）；
// - 在 `<runtime>/voice-bench/<run-id>/` 写出脱敏归档：turns.jsonl、timeline.jsonl、timeline.txt、report.md、summary.json；
// - 退出前打印归档目录路径；无在线设备或任一轮失败时返回非 0 状态码。

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_DEBUG_PORT,
  DEFAULT_IDLE_WAIT_MS,
  DEFAULT_POLL_INTERVAL_MS,
  DEFAULT_TURN_TIMEOUT_MS,
  NO_ONLINE_DEVICE_MESSAGE,
  debugFetch,
  defaultRuntimeDir,
  formatRunId,
  mdCell,
  percentile,
  redactText,
  redactValue,
  registerSignalCleanup,
  resolveDebugConnection,
  resolveProcessTraceDir,
  sleep,
  sliceProcessTrace,
  writeTimelineFiles,
} from "./bench-common.mjs";
import {
  BENCH_RECORDINGS_DIR_REL,
  DEFAULT_WAKE_RETRIES,
  DEFAULT_WAKE_TIMEOUT_MS,
  DEFAULT_WAKE_WORD,
  attemptAutoWake,
  detectPlayer,
  renderWakeEventsMarkdown,
  resolveWakeWordAsset,
  summarizeWakeEvents,
} from "./voice-assets.mjs";

export {
  DEFAULT_DEBUG_PORT,
  DEFAULT_IDLE_WAIT_MS,
  DEFAULT_POLL_INTERVAL_MS,
  DEFAULT_TURN_TIMEOUT_MS,
  NO_ONLINE_DEVICE_MESSAGE,
  defaultRuntimeDir,
  redactText,
  redactValue,
  resolveProcessTraceDir,
  sliceProcessTrace,
};

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
export const DEFAULT_CORPUS_PATH = join(SCRIPT_DIR, "live-corpus.json");

const STAGE_KEYS = [
  "jev_filter",
  "first_llm_delta",
  "llm_done",
  "first_synthesis_ready",
  "first_audio_frame",
  "turn_end",
];

/** 汇总统计（P50/P90 耗时、失败轮次、模拟门禁命中数、播放卡顿与自动唤醒汇总）。 */
export function buildSummary({
  runId,
  deviceId,
  corpusName,
  startedAtMs,
  finishedAtMs,
  turns,
  wakeWord = DEFAULT_WAKE_WORD,
  wakeEvents = [],
  wakeDegradedReason = null,
}) {
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

  const silentTurns = executed.filter((t) => t.detail?.audio_channel_open === false).length;

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
    audio_channel_closed_turns: silentTurns,
    wake_word: wakeWord || DEFAULT_WAKE_WORD,
    wake_summary: summarizeWakeEvents(wakeEvents),
    wake_events: Array.isArray(wakeEvents) ? wakeEvents : [],
    wake_degraded_reason: wakeDegradedReason || null,
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
    ...(summary.wake_summary && summary.wake_summary.total > 0
      ? [
          `- **自动唤醒**：共 ${summary.wake_summary.total} 次（成功 ${summary.wake_summary.succeeded} · 失败 ${summary.wake_summary.failed}，唤醒词「${summary.wake_word || DEFAULT_WAKE_WORD}」）`,
        ]
      : []),
    ...(summary.audio_channel_closed_turns > 0
      ? [
          summary.wake_degraded_reason
            ? `- **播报通道降级说明**：有 ${summary.audio_channel_closed_turns} 轮执行时小智处于常连待机态（未开 UDP 音频通道），已降级为仅服务端验证（未向设备下发播报，小智无声属正常，也不会误切聆听态）。降级原因：${summary.wake_degraded_reason}`
            : `- **播报通道降级说明**：有 ${summary.audio_channel_closed_turns} 轮执行时小智处于常连待机态（未开 UDP 音频通道），已降级为仅服务端验证（未向设备下发播报，小智无声属正常，也不会误切聆听态）。`,
        ]
      : []),
    ...renderWakeEventsMarkdown(summary.wake_events, summary.wake_word),
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

function isDeviceReadyForInject(dev) {
  if (!dev) return false;
  // 普通房间底噪下 listening 的 level 可能非 0；服务端会在真实说话中返回 409 device_busy
  return dev.state === "idle" || dev.state === "listening";
}

function canonicalToolName(name) {
  return String(name || "")
    .trim()
    .replace(/^device__/, "")
    .replace(/\./g, "_")
    .toLowerCase();
}

function toolMatchesExpected(actualName, expectedName) {
  const actual = canonicalToolName(actualName);
  const expected = canonicalToolName(expectedName);
  if (!actual || !expected) return false;
  if (actual === expected) return true;
  // 兼容屏幕亮度工具在 AMOLED 设备上映射为 self.disp.setbacklight
  if (expected === "self_screen_set_brightness" && actual === "self_disp_setbacklight") return true;
  return false;
}

export function evaluateTurnExpectations(turnSpec, detail, { deviceInfo = null } = {}) {
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
  const tools = Array.isArray(detail.tools) ? detail.tools : [];
  if (Array.isArray(expect.expect_tool_any) && expect.expect_tool_any.length > 0) {
    const matched = expect.expect_tool_any.some((expectedName) =>
      tools.some((t) => toolMatchesExpected(t?.name, expectedName)),
    );
    if (!matched) {
      return `期望调用工具（任一）${expect.expect_tool_any.join(" / ")}，实际未调用（实际：${tools.map((t) => t?.name).filter(Boolean).join(", ") || "无"}）`;
    }
  }
  if (Array.isArray(expect.expect_device_tools_if_ready) && expect.expect_device_tools_if_ready.length > 0) {
    const deviceReady =
      deviceInfo?.device_tools_ready === true && Number(deviceInfo?.device_tools_count || 0) > 0;
    if (deviceReady) {
      const declared = Array.isArray(deviceInfo?.device_tools) ? deviceInfo.device_tools : null;
      const applicable = declared
        ? expect.expect_device_tools_if_ready.filter((expectedName) =>
            declared.some((actualName) => toolMatchesExpected(actualName, expectedName)),
          )
        : expect.expect_device_tools_if_ready;
      const missing = applicable.filter(
        (expectedName) => !tools.some((t) => toolMatchesExpected(t?.name, expectedName)),
      );
      if (missing.length > 0) {
        return `设备工具已就绪但未调用预期设备工具：${missing.join("、")}（实际调用：${tools.map((t) => t?.name).filter(Boolean).join(", ") || "无"}）`;
      }
    }
  }
  if (expect.expect_simulation_gate) {
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
  const wakeTimeoutMs = Number(opts.wakeTimeoutMs ?? DEFAULT_WAKE_TIMEOUT_MS);
  const wakeRetries = Math.max(1, Math.floor(Number(opts.wakeRetries ?? DEFAULT_WAKE_RETRIES)));
  const corpusPath = resolve(opts.corpus || DEFAULT_CORPUS_PATH);
  const corpusRaw = JSON.parse(readFileSync(corpusPath, "utf8"));
  const corpusName = String(corpusRaw?.name || "custom");
  const corpusTurns = Array.isArray(corpusRaw) ? corpusRaw : Array.isArray(corpusRaw?.turns) ? corpusRaw.turns : [];
  const { runtimeDir, token, baseUrl } = resolveDebugConnection(opts);
  const resolvedRecordingsDir = opts.recordingsDir
    ? resolve(opts.recordingsDir)
    : join(runtimeDir, BENCH_RECORDINGS_DIR_REL);
  const runId = opts.runId || formatRunId();
  const traceDir = opts.traceDir ? resolve(opts.traceDir) : resolveProcessTraceDir(runtimeDir);
  const archiveDir = opts.outDir ? resolve(opts.outDir) : join(runtimeDir, "voice-bench", runId);
  const redactOpts = {
    secrets: [token],
    paths: [
      runtimeDir,
      homedir(),
      process.cwd(),
      dirname(corpusPath),
      resolvedRecordingsDir,
      traceDir,
      archiveDir,
      opts.tokenFile ? dirname(resolve(opts.tokenFile)) : "",
    ].filter(Boolean),
  };
  if (corpusTurns.length === 0) {
    throw new Error(`语料文件不含有效轮次：${redactText(corpusPath, redactOpts)}`);
  }

  // 1. 启动前检查在线设备；常连待机态（audio_channel_open: false）且本机能外放时先用唤醒词录音自动唤醒
  const devicesRes = await debugFetch(baseUrl, token, "/debug/v1/devices");
  if (!devicesRes.ok) {
    const msg = devicesRes.data?.message || `查询在线设备失败（HTTP ${devicesRes.status}）`;
    return { exitCode: 1, archiveDir: null, summary: null, turns: [], errorMessage: msg };
  }
  const onlineDevices = Array.isArray(devicesRes.data?.devices) ? devicesRes.data.devices : [];
  let targetDevice = opts.device
    ? onlineDevices.find((d) => d.device_id === opts.device) || null
    : onlineDevices[0] || null;

  const canDetectPlayer =
    opts.disableAudioPlayback === true
      ? null
      : typeof opts.playWavFn === "function"
        ? { cmd: "injected", kind: "injected" }
        : typeof opts.detectPlayerFn === "function"
          ? opts.detectPlayerFn()
          : detectPlayer(opts.platform || process.platform);
  const speakerFilter = String(opts.speaker || "").trim();
  let wakeWord =
    String(targetDevice?.wake_word || devicesRes.data?.wake_word || "").trim() || DEFAULT_WAKE_WORD;
  let wakeResolved = resolveWakeWordAsset({
    wakeWord,
    runtimeDir,
    recordingsDir: resolvedRecordingsDir,
    speaker: speakerFilter,
  });
  const wakeEvents = [];
  let wakeDegradedReason = null;

  if (onlineDevices.length === 0) {
    if (canDetectPlayer && wakeResolved.ok) {
      const wakeRes = await attemptAutoWake({
        baseUrl,
        token,
        deviceId: opts.device || "",
        wakeWord,
        wakeAsset: wakeResolved.asset,
        missingReason: wakeResolved.missingReason,
        trigger: "offline_initial",
        player: canDetectPlayer.kind === "injected" ? null : canDetectPlayer,
        playWavFn: opts.playWavFn,
        waitFor: "audio_open",
        wakeTimeoutMs,
        wakeRetries,
        pollIntervalMs,
      });
      wakeEvents.push(wakeRes.event);
      if (wakeRes.ok && wakeRes.device) {
        targetDevice = wakeRes.device;
        const devWord = String(targetDevice.wake_word || "").trim();
        if (devWord) wakeWord = devWord;
      } else {
        return {
          exitCode: 1,
          archiveDir: null,
          summary: null,
          turns: [],
          wakeEvents,
          errorMessage: `${NO_ONLINE_DEVICE_MESSAGE}；${wakeRes.error || "唤醒失败"}`,
        };
      }
    } else {
      return {
        exitCode: 1,
        archiveDir: null,
        summary: null,
        turns: [],
        errorMessage: wakeResolved.ok
          ? NO_ONLINE_DEVICE_MESSAGE
          : `${NO_ONLINE_DEVICE_MESSAGE}（${wakeResolved.missingReason}）`,
      };
    }
  }
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

  // n146 常连待机（audio_channel_open: false）：优先外放唤醒词录音唤醒小智让回复真实出声；
  // 不能外放、缺唤醒词录音或唤醒失败时降级为仅服务端验证并在报告写明原因。
  const ensureAwakeForInject = async (devSnapshot, trigger) => {
    if (!devSnapshot || devSnapshot.audio_channel_open !== false) {
      return devSnapshot;
    }
    const devWord = String(devSnapshot.wake_word || "").trim();
    if (devWord && devWord !== wakeWord) {
      wakeWord = devWord;
      wakeResolved = resolveWakeWordAsset({
        wakeWord,
        runtimeDir,
        recordingsDir: resolvedRecordingsDir,
        speaker: speakerFilter,
      });
    }
    if (!canDetectPlayer) {
      wakeDegradedReason = "本机无可用音频外放程序，已降级为仅服务端验证";
      wakeEvents.push({
        at: new Date().toISOString(),
        trigger,
        wake_word: wakeWord,
        recording: wakeResolved.asset?.filename || null,
        source_type: wakeResolved.asset?.source_type || null,
        audio_origin: wakeResolved.asset?.audio_origin || wakeResolved.asset?.source_type || null,
        speaker: wakeResolved.asset?.speaker || null,
        dataset: wakeResolved.asset?.dataset || null,
        license: wakeResolved.asset?.license || null,
        attempts: 0,
        success: false,
        elapsed_ms: 0,
        session_id_after: devSnapshot.session_id || null,
        error: wakeDegradedReason,
      });
      return devSnapshot;
    }
    if (!wakeResolved.ok) {
      wakeDegradedReason = wakeResolved.missingReason;
      wakeEvents.push({
        at: new Date().toISOString(),
        trigger,
        wake_word: wakeWord,
        recording: null,
        source_type: null,
        audio_origin: null,
        speaker: null,
        dataset: null,
        license: null,
        attempts: 0,
        success: false,
        elapsed_ms: 0,
        session_id_after: devSnapshot.session_id || null,
        error: wakeDegradedReason,
      });
      return devSnapshot;
    }
    const wakeRes = await attemptAutoWake({
      baseUrl,
      token,
      deviceId,
      wakeWord,
      wakeAsset: wakeResolved.asset,
      missingReason: wakeResolved.missingReason,
      trigger,
      player: canDetectPlayer.kind === "injected" ? null : canDetectPlayer,
      playWavFn: opts.playWavFn,
      waitFor: "audio_open",
      wakeTimeoutMs,
      wakeRetries,
      pollIntervalMs,
    });
    wakeEvents.push(wakeRes.event);
    if (wakeRes.ok && wakeRes.device) {
      wakeDegradedReason = null;
      return wakeRes.device;
    }
    wakeDegradedReason = `${wakeRes.error || "自动唤醒失败"}，已降级为仅服务端验证`;
    return devSnapshot;
  };

  targetDevice = await ensureAwakeForInject(targetDevice, "standby");

  // 2. 开启完整过程记录（并记住原始状态，结束与信号中断必恢复）
  const startedAtMs = Date.now();
  let previousTraceEnabled = null;
  let traceRestored = false;
  const restoreTrace = async () => {
    if (traceRestored || previousTraceEnabled === null) return;
    traceRestored = true;
    try {
      await debugFetch(baseUrl, token, "/debug/v1/process-trace", {
        method: "POST",
        body: { enabled: previousTraceEnabled, flush: true },
      });
    } catch {
      // 尽力恢复，不掩盖主流程结果
    }
  };
  const unregisterSignals = registerSignalCleanup(restoreTrace);
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

      const latestDevBeforeInject =
        (latestDevices?.devices || []).find((d) => d.device_id === deviceId) || null;
      if (latestDevBeforeInject && latestDevBeforeInject.audio_channel_open === false && !wakeDegradedReason) {
        await ensureAwakeForInject(latestDevBeforeInject, "post_exit");
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
            finalDetail.device_state === "listening" ||
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

      const latestDev =
        (latestDevices?.devices || []).find((d) => d.device_id === deviceId) || targetDevice;
      const checkErr = evaluateTurnExpectations(spec, finalDetail, { deviceInfo: latestDev });
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
    unregisterSignals();
    // 无论成败，刷新并恢复过程记录开关原状态
    await restoreTrace();
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
      wakeWord,
      wakeEvents,
      wakeDegradedReason,
    }),
    redactOpts,
  );
  const reportMd = redactText(buildReportMarkdown(summary, redactedTurns), redactOpts);
  const { jsonlLines, txtLines } = sliceProcessTrace(traceDir, startedAtMs, finishedAtMs, redactOpts);

  mkdirSync(archiveDir, { recursive: true });
  writeFileSync(
    join(archiveDir, "turns.jsonl"),
    redactedTurns.map((t) => JSON.stringify(t)).join("\n") + (redactedTurns.length ? "\n" : ""),
    "utf8",
  );
  writeTimelineFiles(archiveDir, jsonlLines, txtLines);
  writeFileSync(join(archiveDir, "report.md"), reportMd, "utf8");
  writeFileSync(join(archiveDir, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`, "utf8");

  const exitCode = summary.counts.failed > 0 ? 1 : 0;
  return { exitCode, archiveDir, summary, turns: redactedTurns, errorMessage: null };
}

export function parseArgs(argv) {
  const opts = {
    corpus: null,
    device: null,
    speaker: "",
    recordingsDir: null,
    runtimeDir: null,
    url: null,
    tokenFile: null,
    outDir: null,
    turnTimeoutMs: DEFAULT_TURN_TIMEOUT_MS,
    pollIntervalMs: DEFAULT_POLL_INTERVAL_MS,
    wakeTimeoutMs: DEFAULT_WAKE_TIMEOUT_MS,
    wakeRetries: DEFAULT_WAKE_RETRIES,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--corpus") opts.corpus = argv[++i];
    else if (a === "--device") opts.device = argv[++i];
    else if (a === "--speaker") opts.speaker = argv[++i] || "";
    else if (a === "--recordings-dir") opts.recordingsDir = argv[++i];
    else if (a === "--runtime-dir") opts.runtimeDir = argv[++i];
    else if (a === "--url") opts.url = argv[++i];
    else if (a === "--token-file") opts.tokenFile = argv[++i];
    else if (a === "--out-dir") opts.outDir = argv[++i];
    else if (a === "--turn-timeout-ms") opts.turnTimeoutMs = Number(argv[++i]);
    else if (a === "--poll-interval-ms") opts.pollIntervalMs = Number(argv[++i]);
    else if (a === "--wake-timeout-ms") opts.wakeTimeoutMs = Number(argv[++i]);
    else if (a === "--wake-retries") opts.wakeRetries = Number(argv[++i]);
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
      console.error(redactText(result.errorMessage, { paths: [homedir(), process.cwd()] }));
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
    console.error(redactText(error?.message || String(error), { paths: [homedir(), process.cwd()] }));
    process.exit(1);
  }
}
