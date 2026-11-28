#!/usr/bin/env node
// VC2 / VC3 / VD1 / VD3 真小智整体测试自动化脚本（真人录音外放 → 真麦克风 → MOSS 全链路 → 4 环节归因）：
// - 只使用真人录音：默认从 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.wav` 读取（全部说话人轮流或 `--speaker` 指定）；
//   绝不调用本地 TTS 或系统朗读；缺失录音的语料跳过并提示去 App 录制，全部缺失时以非 0 状态码退出；
//   VB 的 asr-corpus 默认不作为外放素材，仅 `--include-device-corpus` 显式开启并标明“二次录制”；
// - 预检：读取令牌、查在线设备、检查播放器、打开测试时间窗与过程记录、执行外放校准；
// - 严格串行：仅在设备处于 listening 且连续安静 1 秒后外放；每句等 thinking -> speaking -> 回到 listening 安静 1 秒再放下一句，绝不重叠；
// - 可选背景噪声 `--noise fan|pump|burst` + `--noise-gain` 与可选服务端数字注入对照 `--compare-digital`（使用同一份真人录音）；
// - 每轮按 4 环节归因（拾音判停 / 识别纠偏 / 意图工具 / 合成播放）并按说话人分组统计，写出全脱敏归档到 `<runtime>/voice-bench/e2e-<run-id>/`；
// - 若开启了 `asr-corpus` 录音采集，自动回写 `<turn-id>.label.json`（带 `label_source: "e2e-playback"` 与说话人标签）；
// - 无论正常结束还是异常/校准失败，均在 `finally` 中关闭测试时间窗、停掉背景噪声并恢复过程记录设置；有失败或未达标轮次时返回非 0 状态码。

import { randomBytes } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  DEFAULT_IDLE_WAIT_MS,
  DEFAULT_POLL_INTERVAL_MS,
  DEFAULT_TURN_TIMEOUT_MS,
  NO_ONLINE_DEVICE_MESSAGE,
  controlBenchWindow,
  debugFetch,
  formatRunId,
  getTurnDetail,
  listDebugDevices,
  listDeviceTurns,
  mdCell,
  percentile,
  redactText,
  redactValue,
  registerSignalCleanup,
  resolveDebugConnection,
  resolveProcessTraceDir,
  setProcessTrace,
  sleep,
  sliceProcessTrace,
  writeTimelineFiles,
} from "./bench-common.mjs";
import { computeCer, normalizeForCer, readWavPcm16 } from "./asr-bench.mjs";
import {
  BENCH_RECORDINGS_DIR_REL,
  DEFAULT_MAX_ONLINE,
  DEFAULT_WAKE_RETRIES,
  DEFAULT_WAKE_TIMEOUT_MS,
  DEFAULT_WAKE_WORD,
  RECORDING_APP_HINT,
  attemptAutoWake,
  checkPlayerAvailable,
  isWakeCorpusItem,
  playWav,
  prepareVoiceAssets,
  renderWakeEventsMarkdown,
  resolveWakeWordAsset,
  saveOnlineRotation,
  startBackgroundNoise,
  summarizeWakeEvents,
} from "./voice-assets.mjs";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
export const DEFAULT_E2E_CORPUS_PATH = join(SCRIPT_DIR, "e2e-corpus.json");
export const DEFAULT_CALIBRATION_TEXT = "激光机现在什么状态";
export const DEFAULT_QUIET_MS = 1_000;
export const DEFAULT_MAX_MINUTES = 30;
export const EARLY_CUTOFF_TOLERANCE_MS = 50;
export const DEFAULT_MAX_CER_THRESHOLD = 0.35;
export const DEFAULT_ESTIMATED_REPLY_MS = 18_000;
export const DEFAULT_ESTIMATED_INTERVAL_MS = 4_000;

/**
 * VE3 保守估算单句整体测试耗时（毫秒）= 外放时长 + 预计应答时长 + 间隔；
 * 若开启 `--compare-digital`，同一句还会追加一次服务端数字对照轮次。
 */
export function estimateTurnDurationMs(
  item,
  {
    quietMs = DEFAULT_QUIET_MS,
    compareDigital = false,
    estimatedReplyMs = DEFAULT_ESTIMATED_REPLY_MS,
    estimatedIntervalMs = null,
  } = {},
) {
  const playMs = Math.max(1_000, Number(item?.asset?.duration_ms) || 4_000);
  const replyMs = Math.max(0, Number(estimatedReplyMs ?? DEFAULT_ESTIMATED_REPLY_MS));
  const intervalMs =
    estimatedIntervalMs !== null && estimatedIntervalMs !== undefined
      ? Math.max(0, Number(estimatedIntervalMs))
      : Math.max(DEFAULT_ESTIMATED_INTERVAL_MS, Math.max(0, Number(quietMs) || 0) * 2 + 2_000);
  const singleMs = playMs + replyMs + intervalMs;
  return Math.round(compareDigital ? singleMs * 2 : singleMs);
}

/**
 * VE3 开测前估算总时长并按测试时间窗上限自动分批（绝不延长单窗超过 60 分钟硬上限）。
 */
export function planE2eBatches(
  preparedTurns,
  {
    maxMinutes = DEFAULT_MAX_MINUTES,
    quietMs = DEFAULT_QUIET_MS,
    compareDigital = false,
    hasCalibration = false,
    calibrationDurationMs = 4_000,
    estimatedReplyMs = DEFAULT_ESTIMATED_REPLY_MS,
    estimatedIntervalMs = null,
    batchWindowMs = null,
  } = {},
) {
  const clampedMaxMinutes = Math.max(1, Math.min(60, Number(maxMinutes) || DEFAULT_MAX_MINUTES));
  const hardLimitMs = 60 * 60_000;
  const windowLimitMs =
    Number.isFinite(batchWindowMs) && batchWindowMs > 0
      ? Math.min(Math.round(batchWindowMs), hardLimitMs)
      : clampedMaxMinutes * 60_000;

  const calibrationEstimateMs = hasCalibration
    ? estimateTurnDurationMs(
        { asset: { duration_ms: calibrationDurationMs } },
        { quietMs, compareDigital: false, estimatedReplyMs, estimatedIntervalMs },
      )
    : 0;

  const weighted = preparedTurns.map((item, globalIndex) => ({
    item,
    globalIndex,
    estimatedMs: estimateTurnDurationMs(item, {
      quietMs,
      compareDigital: compareDigital && item?.spec?.expect_route !== "fast_exit",
      estimatedReplyMs,
      estimatedIntervalMs,
    }),
  }));

  const totalEstimatedMs = calibrationEstimateMs + weighted.reduce((sum, w) => sum + w.estimatedMs, 0);
  const exceedsWindow = totalEstimatedMs > windowLimitMs;
  if (!exceedsWindow || weighted.length <= 1) {
    return {
      totalEstimatedMs,
      windowLimitMs,
      maxMinutes: clampedMaxMinutes,
      exceedsWindow,
      batches: weighted.length > 0 ? [weighted] : [],
    };
  }

  const batches = [];
  let current = [];
  let currentMs = calibrationEstimateMs;
  for (const entry of weighted) {
    if (current.length > 0 && currentMs + entry.estimatedMs > windowLimitMs) {
      batches.push(current);
      current = [entry];
      currentMs = entry.estimatedMs;
    } else {
      current.push(entry);
      currentMs += entry.estimatedMs;
    }
  }
  if (current.length > 0) batches.push(current);

  return {
    totalEstimatedMs,
    windowLimitMs,
    maxMinutes: clampedMaxMinutes,
    exceedsWindow: true,
    batches,
  };
}

/** 判断首字是否被吞（原句至少 3 字、识别非空且开头 1~2 字在对齐中缺失）。 */
export function detectHeadClipping(referenceText, hypothesisText) {
  const refChars = normalizeForCer(referenceText);
  const hypChars = normalizeForCer(hypothesisText);
  if (refChars.length < 3 || hypChars.length === 0) return false;
  const { alignment } = computeCer(referenceText, hypothesisText);
  let leadingDels = 0;
  for (const step of alignment) {
    if (step.op === "del") leadingDels += 1;
    else break;
  }
  return leadingDels >= 1 && leadingDels < refChars.length;
}

/**
 * 计算单轮的 4 环节归因结构（写入 turns.jsonl）：
 * 1. 拾音判停（capture_endpoint）
 * 2. 识别纠偏（asr_correction）
 * 3. 意图工具（intent_tools）
 * 4. 合成播放（tts_playback）
 */
export function buildTurnAttribution({
  spec,
  playback,
  detail,
  digitalDetail = null,
  observedPhases = [],
  overlapDetected = false,
  maxCerThreshold = DEFAULT_MAX_CER_THRESHOLD,
}) {
  const rawAsr = String(detail?.injected_text || "").trim();
  const correctedAsr = String(detail?.corrected_text || rawAsr).trim();
  const rawCerRes = computeCer(spec.text, rawAsr);
  const correctedCerRes = computeCer(spec.text, correctedAsr);
  const rawCer = Number(rawCerRes.cer.toFixed(4));
  const correctedCer = Number(correctedCerRes.cer.toFixed(4));

  // 1. 拾音判停
  const speechBeganAtMs =
    typeof detail?.speech_began_at_ms === "number" ? detail.speech_began_at_ms : null;
  const speechEndAtMs =
    typeof detail?.speech_end_at_ms === "number"
      ? detail.speech_end_at_ms
      : typeof detail?.started_at_ms === "number"
        ? detail.started_at_ms
        : null;
  const endpointDelayMs =
    speechEndAtMs !== null && typeof playback?.ended_at_ms === "number"
      ? speechEndAtMs - playback.ended_at_ms
      : null;
  // 抢话：服务端判停早于外放结束（外放还没播完就判停）
  const earlyCutoff =
    speechEndAtMs !== null &&
    typeof playback?.ended_at_ms === "number" &&
    speechEndAtMs < playback.ended_at_ms - EARLY_CUTOFF_TOLERANCE_MS;
  const headClipped = detectHeadClipping(spec.text, rawAsr);
  const firstAudioFrameAtMs =
    typeof detail?.first_audio_frame_at_ms === "number" ? detail.first_audio_frame_at_ms : null;
  // 服务端首帧播报早于外放结束即判定外放与播报重叠（避免 playWav 返回后轮询永假漏检）
  const serverOverlap =
    firstAudioFrameAtMs !== null &&
    typeof playback?.ended_at_ms === "number" &&
    firstAudioFrameAtMs < playback.ended_at_ms - EARLY_CUTOFF_TOLERANCE_MS;
  const effectiveOverlap = Boolean(overlapDetected || serverOverlap);
  const capturePassed = Boolean(detail) && rawAsr.length > 0 && !earlyCutoff && !headClipped && !effectiveOverlap;

  // 2. 识别纠偏（及可选数字注入对照）
  const keyTerms = Array.isArray(spec.key_terms) ? spec.key_terms : [];
  const corrNorm = normalizeForCer(correctedAsr).join("");
  const keyTermsMissed = keyTerms.filter((term) => {
    const tn = normalizeForCer(term).join("");
    return tn && !corrNorm.includes(tn);
  });
  let digitalControl = null;
  if (digitalDetail) {
    const dRaw = String(digitalDetail.injected_text || "").trim();
    const dCorr = String(digitalDetail.corrected_text || dRaw).trim();
    const dRawCer = Number(computeCer(spec.text, dRaw).cer.toFixed(4));
    const dCorrCer = Number(computeCer(spec.text, dCorr).cer.toFixed(4));
    digitalControl = {
      turn_id: digitalDetail.turn_id || null,
      raw_asr: dRaw,
      corrected_asr: dCorr,
      raw_cer: dRawCer,
      corrected_cer: dCorrCer,
      mic_room_cer_loss: Number((rawCer - dRawCer).toFixed(4)),
      mic_room_corrected_cer_loss: Number((correctedCer - dCorrCer).toFixed(4)),
    };
  }
  const asrPassed =
    rawAsr.length > 0 &&
    correctedCer <= maxCerThreshold &&
    keyTermsMissed.length === 0;

  // 3. 意图工具
  const route = detail?.route || "unknown";
  const tools = Array.isArray(detail?.tools) ? detail.tools : [];
  const expectRoute = spec.expect_route || spec.expect?.route || null;
  const expectTool = spec.expect_tool || spec.expect?.tool || null;
  const expectGate = Boolean(spec.expect_simulation_gate || spec.expect?.expect_simulation_gate);
  const routeMatched = !expectRoute || route === expectRoute;
  const toolMatched =
    !expectTool ||
    tools.some((t) => {
      if (t?.name === expectTool) return true;
      // 兼容屏幕亮度工具在 AMOLED 设备上映射为 self.disp.setbacklight
      if (expectTool === "self.screen.set_brightness" && t?.name === "self.disp.setbacklight") return true;
      return false;
    });
  const blockedTools = tools.filter((t) => t?.blocked === true);
  const replyText = `${detail?.reply || ""} ${detail?.spoken || ""}`;
  const refusedInReply = /(拒绝|不能|无法|模拟|仅.*预览|禁止|不支持|锁定)/.test(replyText);
  const gateMatched = !expectGate || blockedTools.length > 0 || refusedInReply;
  const intentPassed =
    Boolean(detail) &&
    detail?.outcome !== "disconnected" &&
    !detail?.error &&
    routeMatched &&
    toolMatched &&
    gateMatched;

  // 4. 合成播放
  const stagesMs = detail?.stages_ms || {};
  const endToFirstAudioMs =
    firstAudioFrameAtMs !== null && typeof playback?.ended_at_ms === "number"
      ? Math.max(0, firstAudioFrameAtMs - playback.ended_at_ms)
      : typeof stagesMs.first_audio_frame === "number" && typeof stagesMs.utterance_end === "number"
        ? Math.max(0, stagesMs.first_audio_frame - stagesMs.utterance_end)
        : null;
  const pb = detail?.playback || { frames: 0, underruns: 0, stall_ms: 0 };
  const expectSpoken = route !== "ignored" && route !== "not_for_me";
  const hasOutput =
    (Number(pb.frames) || 0) > 0 ||
    String(detail?.spoken || "").trim().length > 0 ||
    String(detail?.reply || "").trim().length > 0;
  const playbackPassed =
    Boolean(detail) &&
    (!expectSpoken || hasOutput) &&
    (Number(pb.stall_ms) || 0) <= 1_500;

  return {
    capture_endpoint: {
      playback_started_at_ms: playback?.started_at_ms ?? null,
      playback_ended_at_ms: playback?.ended_at_ms ?? null,
      playback_duration_ms: playback?.duration_ms ?? null,
      speech_began_at_ms: speechBeganAtMs,
      speech_end_at_ms: speechEndAtMs,
      endpoint_delay_ms: endpointDelayMs,
      peak_uplink_level: detail?.peak_uplink_level ?? null,
      head_clipped: headClipped,
      early_cutoff: earlyCutoff,
      overlap_detected: effectiveOverlap,
      observed_phases: observedPhases,
      passed: capturePassed,
    },
    asr_correction: {
      reference_text: spec.text,
      raw_asr: rawAsr,
      corrected_asr: correctedAsr,
      raw_cer: rawCer,
      corrected_cer: correctedCer,
      correction_delta: Number((rawCer - correctedCer).toFixed(4)),
      key_terms_missed: keyTermsMissed,
      digital_control: digitalControl,
      passed: asrPassed,
    },
    intent_tools: {
      route,
      expect_route: expectRoute,
      route_matched: routeMatched,
      tools: tools.map((t) => ({
        name: t.name,
        arguments: t.arguments,
        ms: t.ms ?? 0,
        blocked: Boolean(t.blocked),
        block_reason: t.block_reason || null,
      })),
      expect_tool: expectTool,
      tool_matched: toolMatched,
      expect_simulation_gate: expectGate,
      gate_blocked_tools: blockedTools.map((t) => ({
        name: t.name,
        block_reason: t.block_reason || "模拟门禁拒绝",
      })),
      stop_skipped_reason: detail?.stop_skipped_reason || null,
      gate_matched: gateMatched,
      error: detail?.error || null,
      passed: intentPassed,
    },
    tts_playback: {
      end_to_first_audio_ms: endToFirstAudioMs,
      first_audio_frame_at_ms: firstAudioFrameAtMs,
      stages_ms: stagesMs,
      first_synthesis_ready_ms: stagesMs.first_synthesis_ready ?? null,
      first_audio_frame_ms: stagesMs.first_audio_frame ?? null,
      frames: Number(pb.frames) || 0,
      underruns: Number(pb.underruns) || 0,
      stall_ms: Number(pb.stall_ms) || 0,
      reply: detail?.reply || "",
      spoken: detail?.spoken || "",
      passed: playbackPassed,
    },
  };
}

/** 根据 4 环节归因给出按严重程度排序的调优建议（附真实代码位置与参数名）。 */
export function buildPrioritizedRecommendations(turns) {
  const executed = turns.filter((t) => t.verdict !== "skipped" && t.attribution);
  if (executed.length === 0) return [];
  const recs = [];

  const earlyCutoffs = executed.filter((t) => t.attribution.capture_endpoint.early_cutoff);
  if (earlyCutoffs.length > 0) {
    recs.push({
      severity: 100 + earlyCutoffs.length * 10,
      level: "高",
      stage: "拾音判停",
      issue: `出现 ${earlyCutoffs.length}/${executed.length} 轮抢话（外放尚未结束服务端即判停）`,
      action:
        "调大 `BASE_SILENCE_MS`（当前 250 ms，位于 `crates/moss-voice-server/src/session.rs`）或开启/拉长 `UNFINISHED_HOLD_WINDOW`（`session.rs`）与 Jev 半截话续听，并检查 `fast::endpoint_wait_ms` 的停顿宽限。",
      affected: earlyCutoffs.map((t) => t.id),
    });
  }

  const headClipped = executed.filter((t) => t.attribution.capture_endpoint.head_clipped);
  if (headClipped.length > 0) {
    recs.push({
      severity: 90 + headClipped.length * 10,
      level: "高",
      stage: "拾音判停",
      issue: `出现 ${headClipped.length}/${executed.length} 轮首字被吞`,
      action:
        "检查并增大 `PREROLL_MS`（当前 1000 ms，位于 `crates/moss-voice-server/src/session.rs`）以及 `VadConfig::start_rms` / `min_speech_ms`（`crates/moss-voice-server/src/audio/vad.rs`），确认小智距喇叭 30–50 cm。",
      affected: headClipped.map((t) => t.id),
    });
  }

  const intentFails = executed.filter((t) => !t.attribution.intent_tools.passed);
  if (intentFails.length > 0) {
    recs.push({
      severity: 85 + intentFails.length * 8,
      level: "高",
      stage: "意图工具",
      issue: `有 ${intentFails.length}/${executed.length} 轮未命中预期路由/工具或门禁验证未通过`,
      action:
        "检查 `crates/moss-voice-server/src/fast.rs` 快路径规则、`crates/moss-voice-server/src/jev.rs` 路由粗筛、`crates/moss-voice-server/src/sim_guard.rs` 模拟门禁表以及 `conversation.rs` 中的 `LASER_TOOL_GUIDE` 提示词。",
      affected: intentFails.map((t) => t.id),
    });
  }

  const highCer = executed.filter((t) => !t.attribution.asr_correction.passed);
  const avgRawCer =
    executed.reduce((s, t) => s + t.attribution.asr_correction.raw_cer, 0) / executed.length;
  const avgCorrCer =
    executed.reduce((s, t) => s + t.attribution.asr_correction.corrected_cer, 0) / executed.length;
  if (highCer.length > 0 || avgCorrCer > 0.12) {
    const missedTerms = [
      ...new Set(executed.flatMap((t) => t.attribution.asr_correction.key_terms_missed || [])),
    ];
    recs.push({
      severity: 75 + highCer.length * 8 + Math.round(avgCorrCer * 50),
      level: avgCorrCer > 0.25 || highCer.length >= 2 ? "高" : "中",
      stage: "识别纠偏",
      issue: `识别错误偏高（原识别平均 CER ${(avgRawCer * 100).toFixed(1)}%，纠偏后 ${(avgCorrCer * 100).toFixed(1)}%${
        missedTerms.length ? `，漏认术语：${missedTerms.join("、")}` : ""
      }）`,
      action:
        "先运行 `node scripts/voice-bench/asr-bench.mjs` 复核候选，再视需要更新 `crates/moss-voice-server/src/conversation.rs` 同音纠错表（`DEFAULT_ASR_CORRECTIONS`）与 `crates/moss-voice/src/settings.rs` 热词（`normalize_hotwords` / `custom_hotwords`）；候选严禁自动生效且不得触碰停止/退出命令词。",
      affected: highCer.map((t) => t.id),
    });
  }

  const digitalRows = executed
    .map((t) => t.attribution.asr_correction.digital_control)
    .filter(Boolean);
  if (digitalRows.length > 0) {
    const avgLoss =
      digitalRows.reduce((s, d) => s + d.mic_room_cer_loss, 0) / digitalRows.length;
    if (avgLoss >= 0.08) {
      recs.push({
        severity: 70 + Math.round(avgLoss * 100),
        level: avgLoss >= 0.15 ? "高" : "中",
        stage: "拾音/声学环境",
        issue: `外放较服务端数字注入平均多损失 ${(avgLoss * 100).toFixed(1)}pp 字错率（麦克风与房间声学损耗显著）`,
        action:
          "优先改善摆放距离（30–50 cm）、避免桌面反射与设备端回声，检查 `denoise` 本地降噪与小智固件麦克风增益，而非盲目堆同音规则。",
        affected: executed
          .filter((t) => (t.attribution.asr_correction.digital_control?.mic_room_cer_loss || 0) >= 0.1)
          .map((t) => t.id),
      });
    }
  }

  const stallTurns = executed.filter((t) => t.attribution.tts_playback.underruns > 0 || t.attribution.tts_playback.stall_ms > 0);
  const endToFirstVals = executed
    .map((t) => t.attribution.tts_playback.end_to_first_audio_ms)
    .filter((v) => typeof v === "number");
  const p90First = percentile(endToFirstVals, 90);
  if (stallTurns.length > 0 || (p90First !== null && p90First > 2500)) {
    recs.push({
      severity: 60 + stallTurns.length * 10 + (p90First && p90First > 4000 ? 15 : 0),
      level: stallTurns.length >= 2 || (p90First !== null && p90First > 4000) ? "高" : "中",
      stage: "合成播放",
      issue: `播放欠载 ${stallTurns.length} 轮，外放结束到小智首帧 P90 = ${p90First ?? "—"} ms`,
      action:
        "检查 `playback_start_ms`（`crates/moss-voice-server/src/lib.rs`）、`LEAD_FRAMES` 与 `TTS_START_GUARD`（`crates/moss-voice-server/src/session.rs`），以及本地/云端 TTS 首包耗时与 Wi-Fi 抖动。",
      affected: stallTurns.map((t) => t.id),
    });
  }

  const slowEndpoints = executed.filter(
    (t) =>
      typeof t.attribution.capture_endpoint.endpoint_delay_ms === "number" &&
      t.attribution.capture_endpoint.endpoint_delay_ms > 1500,
  );
  if (slowEndpoints.length > 0) {
    recs.push({
      severity: 45 + slowEndpoints.length * 5,
      level: "低",
      stage: "拾音判停",
      issue: `有 ${slowEndpoints.length} 轮判停延迟超过 1500 ms`,
      action:
        "核查背景底噪与 `BASE_SILENCE_MS` / `UNFINISHED_HOLD_WINDOW`（`crates/moss-voice-server/src/session.rs`）是否过度续听。",
      affected: slowEndpoints.map((t) => t.id),
    });
  }

  recs.sort((a, b) => b.severity - a.severity);
  return recs;
}

function resolveTurnSourceType(t) {
  const raw = String(t?.source_type || t?.audio_origin || t?.audio_source?.source_type || "").trim();
  if (raw === "online-human" || raw === "human-recording" || raw === "device-corpus-secondary" || raw === "live_mic" || raw === "digital_inject") {
    return raw;
  }
  if (t?.secondary_recording || t?.audio_source?.secondary_recording) return "device-corpus-secondary";
  return "human-recording";
}

function sourceTypeLabel(sourceType) {
  return (
    {
      "online-human": "网上真人（online-human）",
      "human-recording": "我的录音（human-recording）",
      live_mic: "小智实时说话（live_mic）",
      digital_inject: "数字直通（digital_inject）",
      "device-corpus-secondary": "设备录音（二次录制）",
    }[sourceType] || sourceType
  );
}

function formatPlaybackOriginNote(turn) {
  const src = resolveTurnSourceType(turn);
  const dataset = turn?.dataset || turn?.audio_source?.dataset || null;
  const license = turn?.license || turn?.audio_source?.license || null;
  const speaker = turn?.speaker || turn?.audio_source?.speaker || "";
  if (src === "online-human") {
    const metaParts = [dataset ? `数据集=${dataset}` : "", license ? `许可=${license}` : ""].filter(Boolean);
    return `外放来源：网上真人（online-human${metaParts.length ? `，${metaParts.join("，")}` : ""}）`;
  }
  if (src === "device-corpus-secondary") {
    return "外放来源：设备录音（二次录制）";
  }
  return `外放来源：我的录音（human-recording${speaker ? `，说话人=${speaker}` : ""}）`;
}

function annotatePlaybackTraceTimeline(jsonlLines, txtLines, turns, redactOpts) {
  const byTurnNo = new Map();
  for (const t of turns) {
    const turnNo = Number(t?.detail?.turn || 0);
    if (turnNo > 0) byTurnNo.set(turnNo, t);
  }
  const annotatedTurns = new Set();
  const nextJsonl = jsonlLines.map((line) => {
    try {
      const obj = JSON.parse(line);
      const turnNo = Number(obj?.turn || 0);
      const matched = byTurnNo.get(turnNo);
      if (
        matched &&
        (obj?.kind === "user_audio" || obj?.kind === "turn_end" || obj?.data?.input_source === "e2e_playback")
      ) {
        annotatedTurns.add(turnNo);
        const src = resolveTurnSourceType(matched);
        const dataset = matched.dataset || matched.audio_source?.dataset || null;
        const license = matched.license || matched.audio_source?.license || null;
        const note = formatPlaybackOriginNote(matched);
        const nextObj = {
          ...obj,
          data: {
            ...(obj.data && typeof obj.data === "object" ? obj.data : {}),
            source_type: src,
            audio_origin: src,
            ...(dataset ? { dataset } : {}),
            ...(license ? { license } : {}),
            playback_origin_note: note,
          },
        };
        return JSON.stringify(redactValue(nextObj, redactOpts));
      }
      return line;
    } catch {
      return line;
    }
  });

  const txtAnnotatedTurns = new Set();
  const nextTxt = txtLines.map((line) => {
    for (const [turnNo, matched] of byTurnNo.entries()) {
      if (
        line.includes(`第${turnNo}轮`) &&
        (line.includes("整体测试外放录音") || line.includes("用户说完"))
      ) {
        txtAnnotatedTurns.add(turnNo);
        const note = formatPlaybackOriginNote(matched);
        return redactText(`${line} [${note}]`, redactOpts);
      }
    }
    return line;
  });

  // 若原始 ProcessTrace 尚未落盘对应轮次行（或假服务测试未写过程记录），补充明确的外放轮次来源说明
  for (const t of turns) {
    if (t.verdict === "skipped") continue;
    const turnNo = Number(t?.detail?.turn || 0);
    const src = resolveTurnSourceType(t);
    const dataset = t.dataset || t.audio_source?.dataset || null;
    const license = t.license || t.audio_source?.license || null;
    const note = formatPlaybackOriginNote(t);
    if (!turnNo || !annotatedTurns.has(turnNo)) {
      const event = {
        at: new Date(t?.detail?.started_at_ms || Date.now()).toISOString(),
        kind: "e2e_playback_origin",
        turn: turnNo || t.index,
        data: {
          turn_id: t.turn_id || null,
          corpus_id: t.id,
          input_source: "e2e_playback",
          source_type: src,
          audio_origin: src,
          speaker: t.speaker || null,
          ...(dataset ? { dataset } : {}),
          ...(license ? { license } : {}),
          playback_origin_note: note,
        },
      };
      nextJsonl.push(JSON.stringify(redactValue(event, redactOpts)));
    }
    if (!turnNo || !txtAnnotatedTurns.has(turnNo)) {
      const stamp = new Date(t?.detail?.started_at_ms || Date.now()).toISOString();
      nextTxt.push(
        redactText(
          `[${stamp}] 整体测试第${turnNo || t.index}轮（${t.id}）[音源：整体测试外放录音 | ${note}]`,
          redactOpts,
        ),
      );
    }
  }
  return { jsonlLines: nextJsonl, txtLines: nextTxt };
}

function buildSourceStats(turns) {
  const bySource = new Map();
  const ensure = (sourceType) => {
    if (!bySource.has(sourceType)) {
      bySource.set(sourceType, {
        source_type: sourceType,
        label: sourceTypeLabel(sourceType),
        rows: [],
        datasets: new Set(),
        licenses: new Set(),
      });
    }
    return bySource.get(sourceType);
  };

  for (const t of turns) {
    const src = resolveTurnSourceType(t);
    const group = ensure(src);
    group.rows.push(t);
    const ds = t.dataset || t.audio_source?.dataset;
    const lic = t.license || t.audio_source?.license;
    if (ds) group.datasets.add(ds);
    if (lic) group.licenses.add(lic);
  }

  const rate = (num, den) => (den > 0 ? Number((num / den).toFixed(4)) : null);
  const order = {
    "online-human": 1,
    "human-recording": 2,
    live_mic: 3,
    digital_inject: 4,
    "device-corpus-secondary": 5,
  };
  const out = [];
  for (const group of bySource.values()) {
    const executed = group.rows.filter((r) => r.verdict !== "skipped");
    const passed = group.rows.filter((r) => r.verdict === "passed");
    const failed = group.rows.filter((r) => r.verdict === "failed");
    const skipped = group.rows.filter((r) => r.verdict === "skipped");
    const withAttr = executed.filter((r) => r.attribution);
    let refChars = 0;
    let rawEdits = 0;
    let corrEdits = 0;
    for (const r of withAttr) {
      const len = normalizeForCer(r.attribution.asr_correction.reference_text).length;
      refChars += len;
      rawEdits += Math.round(r.attribution.asr_correction.raw_cer * len);
      corrEdits += Math.round(r.attribution.asr_correction.corrected_cer * len);
    }
    const endToFirst = withAttr
      .map((r) => r.attribution.tts_playback.end_to_first_audio_ms)
      .filter((v) => typeof v === "number");
    out.push({
      source_type: group.source_type,
      label: group.label,
      total: group.rows.length,
      executed: executed.length,
      passed: passed.length,
      failed: failed.length,
      skipped: skipped.length,
      stage_pass_rates: {
        capture_endpoint: rate(
          withAttr.filter((r) => r.attribution.capture_endpoint.passed).length,
          withAttr.length,
        ),
        asr_correction: rate(
          withAttr.filter((r) => r.attribution.asr_correction.passed).length,
          withAttr.length,
        ),
        intent_tools: rate(
          withAttr.filter((r) => r.attribution.intent_tools.passed).length,
          withAttr.length,
        ),
        tts_playback: rate(
          withAttr.filter((r) => r.attribution.tts_playback.passed).length,
          withAttr.length,
        ),
      },
      raw_cer: refChars > 0 ? Number((rawEdits / refChars).toFixed(4)) : null,
      corrected_cer: refChars > 0 ? Number((corrEdits / refChars).toFixed(4)) : null,
      end_to_first_audio_p50_ms: percentile(endToFirst, 50),
      ...(group.datasets.size > 0 ? { datasets: [...group.datasets].sort() } : {}),
      ...(group.licenses.size > 0 ? { licenses: [...group.licenses].sort() } : {}),
    });
  }

  // 若跑了 --compare-digital，把数字直通对照指标作为独立来源组一并列出
  const digitalRows = turns.filter((t) => t.attribution?.asr_correction?.digital_control);
  if (digitalRows.length > 0) {
    let refChars = 0;
    let rawEdits = 0;
    let corrEdits = 0;
    for (const r of digitalRows) {
      const len = normalizeForCer(r.attribution.asr_correction.reference_text).length;
      const d = r.attribution.asr_correction.digital_control;
      refChars += len;
      rawEdits += Math.round(d.raw_cer * len);
      corrEdits += Math.round(d.corrected_cer * len);
    }
    out.push({
      source_type: "digital_inject",
      label: sourceTypeLabel("digital_inject"),
      total: digitalRows.length,
      executed: digitalRows.length,
      passed: digitalRows.length,
      failed: 0,
      skipped: 0,
      stage_pass_rates: {
        capture_endpoint: null,
        asr_correction: null,
        intent_tools: null,
        tts_playback: null,
      },
      raw_cer: refChars > 0 ? Number((rawEdits / refChars).toFixed(4)) : null,
      corrected_cer: refChars > 0 ? Number((corrEdits / refChars).toFixed(4)) : null,
      end_to_first_audio_p50_ms: null,
    });
  }

  return out.sort((a, b) => (order[a.source_type] || 99) - (order[b.source_type] || 99));
}

function buildSpeakerStats(turns) {
  const bySpeaker = new Map();
  for (const t of turns) {
    const speaker = String(t.speaker || t.audio_source?.speaker || "未标注说话人");
    const src = resolveTurnSourceType(t);
    const key = `${src}::${speaker}`;
    if (!bySpeaker.has(key)) {
      bySpeaker.set(key, {
        speaker,
        source_type: src,
        secondary_recording: Boolean(t.secondary_recording || t.audio_source?.secondary_recording),
        datasets: new Set(),
        licenses: new Set(),
        rows: [],
      });
    }
    const group = bySpeaker.get(key);
    if (t.secondary_recording || t.audio_source?.secondary_recording) {
      group.secondary_recording = true;
    }
    const ds = t.dataset || t.audio_source?.dataset;
    const lic = t.license || t.audio_source?.license;
    if (ds) group.datasets.add(ds);
    if (lic) group.licenses.add(lic);
    group.rows.push(t);
  }
  const rate = (num, den) => (den > 0 ? Number((num / den).toFixed(4)) : null);
  const out = [];
  for (const { speaker, source_type, secondary_recording, datasets, licenses, rows } of bySpeaker.values()) {
    const executed = rows.filter((r) => r.verdict !== "skipped");
    const passed = rows.filter((r) => r.verdict === "passed");
    const failed = rows.filter((r) => r.verdict === "failed");
    const skipped = rows.filter((r) => r.verdict === "skipped");
    const withAttr = executed.filter((r) => r.attribution);

    let refChars = 0;
    let rawEdits = 0;
    let corrEdits = 0;
    for (const r of withAttr) {
      const len = normalizeForCer(r.attribution.asr_correction.reference_text).length;
      refChars += len;
      rawEdits += Math.round(r.attribution.asr_correction.raw_cer * len);
      corrEdits += Math.round(r.attribution.asr_correction.corrected_cer * len);
    }
    const endToFirst = withAttr
      .map((r) => r.attribution.tts_playback.end_to_first_audio_ms)
      .filter((v) => typeof v === "number");
    out.push({
      speaker,
      source_type,
      secondary_recording,
      ...(datasets.size > 0 ? { datasets: [...datasets].sort() } : {}),
      ...(licenses.size > 0 ? { licenses: [...licenses].sort() } : {}),
      total: rows.length,
      executed: executed.length,
      passed: passed.length,
      failed: failed.length,
      skipped: skipped.length,
      stage_pass_rates: {
        capture_endpoint: rate(
          withAttr.filter((r) => r.attribution.capture_endpoint.passed).length,
          withAttr.length,
        ),
        asr_correction: rate(
          withAttr.filter((r) => r.attribution.asr_correction.passed).length,
          withAttr.length,
        ),
        intent_tools: rate(
          withAttr.filter((r) => r.attribution.intent_tools.passed).length,
          withAttr.length,
        ),
        tts_playback: rate(
          withAttr.filter((r) => r.attribution.tts_playback.passed).length,
          withAttr.length,
        ),
      },
      raw_cer: refChars > 0 ? Number((rawEdits / refChars).toFixed(4)) : null,
      corrected_cer: refChars > 0 ? Number((corrEdits / refChars).toFixed(4)) : null,
      early_cutoffs: withAttr.filter((r) => r.attribution.capture_endpoint.early_cutoff).length,
      head_clipped: withAttr.filter((r) => r.attribution.capture_endpoint.head_clipped).length,
      end_to_first_audio_p50_ms: percentile(endToFirst, 50),
    });
  }
  return out;
}

export function buildE2eSummary({
  runId,
  deviceId,
  corpusName,
  noisePreset,
  noiseGain,
  compareDigital,
  speakerFilter = "",
  includeDeviceCorpus = false,
  maxOnline = DEFAULT_MAX_ONLINE,
  onlineRotation = null,
  batchPlan = null,
  missingTurns = [],
  skippedAssets = [],
  wakeWord = DEFAULT_WAKE_WORD,
  wakeEvents = [],
  stopReason = null,
  startedAtMs,
  finishedAtMs,
  calibration,
  autoLabeledCount,
  turns,
}) {
  const executed = turns.filter((t) => t.verdict !== "skipped");
  const passed = turns.filter((t) => t.verdict === "passed");
  const failed = turns.filter((t) => t.verdict === "failed");
  const skipped = turns.filter((t) => t.verdict === "skipped");

  const withAttr = executed.filter((t) => t.attribution);
  const rate = (num, den) => (den > 0 ? Number((num / den).toFixed(4)) : null);
  const capturePass = withAttr.filter((t) => t.attribution.capture_endpoint.passed).length;
  const asrPass = withAttr.filter((t) => t.attribution.asr_correction.passed).length;
  const intentPass = withAttr.filter((t) => t.attribution.intent_tools.passed).length;
  const playbackPass = withAttr.filter((t) => t.attribution.tts_playback.passed).length;

  const endpointDelays = withAttr
    .map((t) => t.attribution.capture_endpoint.endpoint_delay_ms)
    .filter((v) => typeof v === "number");
  const endToFirst = withAttr
    .map((t) => t.attribution.tts_playback.end_to_first_audio_ms)
    .filter((v) => typeof v === "number");
  const wallVals = executed
    .map((t) => t.wall_ms)
    .filter((v) => typeof v === "number");

  let totalRawEdits = 0;
  let totalCorrEdits = 0;
  let totalRefChars = 0;
  for (const t of withAttr) {
    const refLen = normalizeForCer(t.attribution.asr_correction.reference_text).length;
    totalRefChars += refLen;
    totalRawEdits += Math.round(t.attribution.asr_correction.raw_cer * refLen);
    totalCorrEdits += Math.round(t.attribution.asr_correction.corrected_cer * refLen);
  }
  const overallRawCer = totalRefChars > 0 ? Number((totalRawEdits / totalRefChars).toFixed(4)) : null;
  const overallCorrectedCer = totalRefChars > 0 ? Number((totalCorrEdits / totalRefChars).toFixed(4)) : null;

  const digitalRows = withAttr
    .map((t) => ({
      refLen: normalizeForCer(t.attribution.asr_correction.reference_text).length,
      rawCer: t.attribution.asr_correction.raw_cer,
      digital: t.attribution.asr_correction.digital_control,
    }))
    .filter((x) => x.digital);
  let digitalSummary = null;
  if (digitalRows.length > 0) {
    const dChars = digitalRows.reduce((s, r) => s + r.refLen, 0);
    const dEdits = digitalRows.reduce((s, r) => s + Math.round(r.digital.raw_cer * r.refLen), 0);
    const aEdits = digitalRows.reduce((s, r) => s + Math.round(r.rawCer * r.refLen), 0);
    const digitalRawCer = dChars > 0 ? Number((dEdits / dChars).toFixed(4)) : 0;
    const acousticRawCer = dChars > 0 ? Number((aEdits / dChars).toFixed(4)) : 0;
    digitalSummary = {
      compared_turns: digitalRows.length,
      acoustic_raw_cer: acousticRawCer,
      digital_raw_cer: digitalRawCer,
      mic_room_cer_loss: Number((acousticRawCer - digitalRawCer).toFixed(4)),
    };
  }

  const gateHits = withAttr.reduce(
    (sum, t) => sum + (t.attribution.intent_tools.gate_blocked_tools?.length || 0),
    0,
  );
  const earlyCutoffs = withAttr.filter((t) => t.attribution.capture_endpoint.early_cutoff).length;
  const headClipped = withAttr.filter((t) => t.attribution.capture_endpoint.head_clipped).length;
  const overlaps = withAttr.filter((t) => t.attribution.capture_endpoint.overlap_detected).length;
  const totalUnderruns = withAttr.reduce((s, t) => s + t.attribution.tts_playback.underruns, 0);
  const totalStallMs = withAttr.reduce((s, t) => s + t.attribution.tts_playback.stall_ms, 0);

  const bySpeaker = buildSpeakerStats(turns);
  const bySource = buildSourceStats(turns);

  return {
    run_id: runId,
    device_id: deviceId,
    corpus: corpusName,
    speaker_filter: speakerFilter || null,
    include_device_corpus: Boolean(includeDeviceCorpus),
    max_online: maxOnline,
    online_rotation: onlineRotation
      ? {
          total_pool: onlineRotation.totalPool,
          selected_count: onlineRotation.selectedCount,
          cursor_before: onlineRotation.cursorBefore,
          cursor_after: onlineRotation.cursorAfter,
          selected_ids: onlineRotation.selectedIds,
        }
      : null,
    batch_plan: batchPlan || null,
    wake_word: wakeWord || DEFAULT_WAKE_WORD,
    wake_summary: summarizeWakeEvents(wakeEvents),
    wake_events: Array.isArray(wakeEvents) ? wakeEvents : [],
    stop_reason: stopReason || null,
    noise: noisePreset ? { preset: noisePreset, gain: noiseGain } : null,
    compare_digital: Boolean(compareDigital),
    calibration: calibration || null,
    started_at: new Date(startedAtMs).toISOString(),
    finished_at: new Date(finishedAtMs).toISOString(),
    duration_ms: Math.max(0, finishedAtMs - startedAtMs),
    counts: {
      total: turns.length,
      executed: executed.length,
      passed: passed.length,
      failed: failed.length,
      skipped: skipped.length,
      missing_recordings: missingTurns.length,
      skipped_assets: skippedAssets.length,
    },
    stage_pass_rates: {
      capture_endpoint: rate(capturePass, withAttr.length),
      asr_correction: rate(asrPass, withAttr.length),
      intent_tools: rate(intentPass, withAttr.length),
      tts_playback: rate(playbackPass, withAttr.length),
    },
    cer: {
      raw_cer: overallRawCer,
      corrected_cer: overallCorrectedCer,
    },
    by_source: bySource,
    by_speaker: bySpeaker,
    missing_turns: missingTurns,
    skipped_assets: skippedAssets,
    digital_comparison: digitalSummary,
    capture_metrics: {
      early_cutoffs: earlyCutoffs,
      head_clipped: headClipped,
      overlaps,
      endpoint_delay_p50_ms: percentile(endpointDelays, 50),
      endpoint_delay_p90_ms: percentile(endpointDelays, 90),
    },
    latency_ms: {
      end_to_first_audio_p50_ms: percentile(endToFirst, 50),
      end_to_first_audio_p90_ms: percentile(endToFirst, 90),
      wall_p50_ms: percentile(wallVals, 50),
      wall_p90_ms: percentile(wallVals, 90),
    },
    playback: {
      total_underruns: totalUnderruns,
      total_stall_ms: totalStallMs,
    },
    gate_hits: gateHits,
    auto_labeled_corpus_count: autoLabeledCount,
    recommendations: buildPrioritizedRecommendations(turns),
    failed_turns: failed.map((t) => ({
      index: t.index,
      id: t.id,
      label: t.label,
      reason: t.failure_reason || "未知失败",
    })),
  };
}

function pct(val) {
  if (val === null || val === undefined || Number.isNaN(val)) return "—";
  return `${(val * 100).toFixed(1)}%`;
}

function msFmt(val) {
  return typeof val === "number" && Number.isFinite(val) ? `${val} ms` : "—";
}

/** 生成 VC3 `report.md`：总览表 + 4 环节最差置顶表 + 可选数字注入对照 + 严重程度排序调优建议。 */
export function buildE2eReportMarkdown(summary, turns) {
  const lines = [
    `# 真小智整体测试报告（${summary.run_id}）`,
    "",
    "## 总览",
    "",
    "| 指标 | 数值 |",
    "| --- | --- |",
    `| 设备 | \`${summary.device_id}\` |`,
    `| 语料集 | ${summary.corpus} |`,
    `| 说话人范围 | ${summary.speaker_filter ? `仅「${summary.speaker_filter}」` : "全部已录制说话人轮流"}${summary.include_device_corpus ? "（含设备录音·二次录制）" : ""} |`,
    `| 背景噪声 | ${summary.noise ? `${summary.noise.preset}（gain=${summary.noise.gain}）` : "无（安静环境）"} |`,
    `| 轮次统计 | 已执行 ${summary.counts.executed}（通过 ${summary.counts.passed} · 失败 ${summary.counts.failed} · 跳过 ${summary.counts.skipped}）· 缺少真人录音跳过 ${summary.counts.missing_recordings || 0} 句 |`,
    `| 拾音判停通过率 | ${pct(summary.stage_pass_rates.capture_endpoint)}（抢话 ${summary.capture_metrics.early_cutoffs} / 吞字 ${summary.capture_metrics.head_clipped} / 重叠 ${summary.capture_metrics.overlaps}） |`,
    `| 识别纠偏通过率 | ${pct(summary.stage_pass_rates.asr_correction)}（原识别 CER ${pct(summary.cer.raw_cer)} → 纠偏后 CER ${pct(summary.cer.corrected_cer)}） |`,
    `| 意图工具通过率 | ${pct(summary.stage_pass_rates.intent_tools)}（模拟门禁拒绝 ${summary.gate_hits} 次） |`,
    `| 合成播放通过率 | ${pct(summary.stage_pass_rates.tts_playback)}（欠载 ${summary.playback.total_underruns} 次 / 卡顿 ${summary.playback.total_stall_ms} ms） |`,
    `| 外放结束 → 首帧延迟 P50 / P90 | ${msFmt(summary.latency_ms.end_to_first_audio_p50_ms)} / ${msFmt(summary.latency_ms.end_to_first_audio_p90_ms)} |`,
    `| 判停延迟 P50 / P90 | ${msFmt(summary.capture_metrics.endpoint_delay_p50_ms)} / ${msFmt(summary.capture_metrics.endpoint_delay_p90_ms)} |`,
    `| 自动回写识别语料标注 | ${summary.auto_labeled_corpus_count} 条（\`label_source: "e2e-playback"\`） |`,
  ];

  if (summary.digital_comparison) {
    const dc = summary.digital_comparison;
    lines.push(
      `| 数字注入对照（${dc.compared_turns} 句） | 外放 CER ${pct(dc.acoustic_raw_cer)} vs 数字注入 CER ${pct(dc.digital_raw_cer)} → 麦克风/房间损失 ${
        dc.mic_room_cer_loss >= 0 ? "+" : ""
      }${(dc.mic_room_cer_loss * 100).toFixed(1)}pp |`,
    );
  }
  if (summary.batch_plan && summary.batch_plan.batch_count > 1) {
    const bp = summary.batch_plan;
    const estMin = (bp.estimated_duration_ms / 60_000).toFixed(1);
    lines.push(
      `| 自动分批执行 | 预估 ${estMin} 分钟 > 单窗上限 ${bp.max_minutes} 分钟，分 ${bp.batch_count} 批（已执行 ${bp.batches_executed} 批${bp.session_aborted ? "，因会话变更中止" : ""}） |`,
    );
  }
  if (summary.wake_summary && summary.wake_summary.total > 0) {
    lines.push(
      `| 自动唤醒 | 共 ${summary.wake_summary.total} 次（成功 ${summary.wake_summary.succeeded} · 失败 ${summary.wake_summary.failed}，唤醒词「${summary.wake_word || DEFAULT_WAKE_WORD}」） |`,
    );
  }
  if (summary.stop_reason) {
    lines.push(`| 中止原因 | ${mdCell(summary.stop_reason, 96)} |`);
    lines.push("", `> **测试已提前中止**：${summary.stop_reason}`);
  }
  lines.push(...renderWakeEventsMarkdown(summary.wake_events, summary.wake_word));

  if (Array.isArray(summary.by_source) && summary.by_source.length > 0) {
    const hasOnline = summary.by_source.some((s) => s.source_type === "online-human");
    lines.push(
      "",
      "## 按语音来源分组统计（网上真人 / 我的录音 / 小智实时说话 / 数字直通）",
      "",
      ...(hasOnline
        ? ["> **说明**：网上语音的指标只反映通用识别能力，不代表用户本人的识别效果。", ""]
        : []),
      "| 语音来源 | 通过/执行 | 拾音判停 | 识别纠偏 | 意图工具 | 合成播放 | 原 CER | 纠偏 CER | 首帧 P50 | 数据集与许可 |",
      "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
    );
    for (const src of summary.by_source) {
      const dsInfo =
        src.datasets?.length || src.licenses?.length
          ? `${(src.datasets || []).join("、")}（${(src.licenses || []).join("、")}）`
          : "—";
      lines.push(
        `| ${mdCell(src.label, 28)} | ${src.passed}/${src.executed} | ${pct(
          src.stage_pass_rates?.capture_endpoint,
        )} | ${pct(src.stage_pass_rates?.asr_correction)} | ${pct(
          src.stage_pass_rates?.intent_tools,
        )} | ${pct(src.stage_pass_rates?.tts_playback)} | ${pct(src.raw_cer)} | ${pct(
          src.corrected_cer,
        )} | ${msFmt(src.end_to_first_audio_p50_ms)} | ${mdCell(dsInfo, 36)} |`,
      );
    }
  }

  if (Array.isArray(summary.by_speaker) && summary.by_speaker.length > 0) {
    lines.push(
      "",
      "## 按说话人分组统计",
      "",
      "| 说话人 | 来源类型 | 通过/执行 | 拾音判停 | 识别纠偏 | 意图工具 | 合成播放 | 原 CER | 纠偏 CER | 首帧 P50 |",
      "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    );
    for (const sp of summary.by_speaker) {
      const sourceLabel = sourceTypeLabel(
        sp.source_type || (sp.secondary_recording ? "device-corpus-secondary" : "human-recording"),
      );
      lines.push(
        `| ${mdCell(sp.speaker, 20)} | ${sourceLabel} | ${sp.passed}/${sp.executed} | ${pct(
          sp.stage_pass_rates.capture_endpoint,
        )} | ${pct(sp.stage_pass_rates.asr_correction)} | ${pct(
          sp.stage_pass_rates.intent_tools,
        )} | ${pct(sp.stage_pass_rates.tts_playback)} | ${pct(sp.raw_cer)} | ${pct(
          sp.corrected_cer,
        )} | ${msFmt(sp.end_to_first_audio_p50_ms)} |`,
      );
    }
  }

  if (Array.isArray(summary.skipped_assets) && summary.skipped_assets.length > 0) {
    lines.push(
      "",
      "## 被跳过素材",
      "",
      `> 以下 ${summary.skipped_assets.length} 条素材因缺少必填许可/数据集元数据、仅使用 .txt 或包含疑似指令词而被跳过：`,
      "",
      "| 文件 | 来源 | 原话 | 跳过原因 |",
      "| --- | --- | --- | --- |",
    );
    for (const sk of summary.skipped_assets) {
      lines.push(
        `| \`${mdCell(sk.filename || sk.id, 36)}\` | ${sourceTypeLabel(sk.source_type || "online-human")} | ${mdCell(
          sk.text || "—",
          36,
        )} | ${mdCell(sk.reason, 64)} |`,
      );
    }
  }

  if (Array.isArray(summary.missing_turns) && summary.missing_turns.length > 0) {
    lines.push(
      "",
      "## 缺少真人录音的语料（已跳过）",
      "",
      `> 以下 ${summary.missing_turns.length} 句未找到可用的真人录音，已跳过（绝不使用合成声音替代）。${RECORDING_APP_HINT}`,
      "",
      "| 编号 | 场景 | 原句 |",
      "| --- | --- | --- |",
    );
    for (const m of summary.missing_turns) {
      lines.push(`| \`${m.id}\` | ${mdCell(m.label, 24)} | ${mdCell(m.text, 64)} |`);
    }
  }

  const withAttr = turns.filter((t) => t.attribution);

  // 1. 拾音判停（最差置顶）
  const captureSorted = [...withAttr].sort((a, b) => {
    const score = (t) => {
      const c = t.attribution.capture_endpoint;
      return (
        (c.overlap_detected ? 10_000 : 0) +
        (c.early_cutoff ? 5_000 : 0) +
        (c.head_clipped ? 2_500 : 0) +
        (!c.passed ? 1_000 : 0) +
        Math.abs(c.endpoint_delay_ms ?? 0)
      );
    };
    return score(b) - score(a);
  });
  lines.push(
    "",
    "## 1. 拾音判停（最差置顶）",
    "",
    "| # | 场景 | 说话人 / 来源 | 峰值电平 | 外放时长 | 判停延迟 | 首字被吞 | 抢话 | 重叠 | 结论 |",
    "| ---: | --- | --- | ---: | ---: | ---: | --- | --- | --- | --- |",
  );
  for (const t of captureSorted) {
    const c = t.attribution.capture_endpoint;
    const spLabel = t.speaker || t.audio_source?.speaker || "—";
    const srcTag = t.secondary_recording || t.audio_source?.secondary_recording
      ? `${spLabel}（二次录制）`
      : spLabel;
    lines.push(
      `| ${t.index} | ${mdCell(t.label, 24)} | ${mdCell(srcTag, 24)} | ${c.peak_uplink_level ?? "—"} | ${msFmt(
        c.playback_duration_ms,
      )} | ${msFmt(c.endpoint_delay_ms)} | ${c.head_clipped ? "是" : "否"} | ${c.early_cutoff ? "是" : "否"} | ${
        c.overlap_detected ? "是" : "否"
      } | ${c.passed ? "通过" : "异常"} |`,
    );
  }

  // 2. 识别纠偏（最差置顶）
  const asrSorted = [...withAttr].sort((a, b) => {
    const ca = a.attribution.asr_correction;
    const cb = b.attribution.asr_correction;
    return cb.corrected_cer - ca.corrected_cer || cb.raw_cer - ca.raw_cer;
  });
  lines.push(
    "",
    "## 2. 识别纠偏（最差置顶）",
    "",
    "| # | 场景 | 原句 | 识别原文 | 纠偏后文字 | 原 CER | 纠偏 CER | 漏认术语 |",
    "| ---: | --- | --- | --- | --- | ---: | ---: | --- |",
  );
  for (const t of asrSorted) {
    const a = t.attribution.asr_correction;
    lines.push(
      `| ${t.index} | ${mdCell(t.label, 20)} | ${mdCell(a.reference_text, 28)} | ${mdCell(a.raw_asr, 28)} | ${mdCell(
        a.corrected_asr,
        28,
      )} | ${pct(a.raw_cer)} | ${pct(a.corrected_cer)} | ${
        a.key_terms_missed.length ? a.key_terms_missed.join("、") : "—"
      } |`,
    );
  }

  if (summary.digital_comparison) {
    const digitalTurns = withAttr
      .filter((t) => t.attribution.asr_correction.digital_control)
      .sort(
        (a, b) =>
          b.attribution.asr_correction.digital_control.mic_room_cer_loss -
          a.attribution.asr_correction.digital_control.mic_room_cer_loss,
      );
    lines.push(
      "",
      "### 2.1 外放识别 vs 服务端数字注入对照（麦克风与房间损失置顶）",
      "",
      "| # | 场景 | 外放识别 | 数字注入识别 | 外放 CER | 数字 CER | 麦克风/房间 CER 损失 |",
      "| ---: | --- | --- | --- | ---: | ---: | ---: |",
    );
    for (const t of digitalTurns) {
      const a = t.attribution.asr_correction;
      const d = a.digital_control;
      const loss = `${d.mic_room_cer_loss >= 0 ? "+" : ""}${(d.mic_room_cer_loss * 100).toFixed(1)}pp`;
      lines.push(
        `| ${t.index} | ${mdCell(t.label, 20)} | ${mdCell(a.raw_asr, 26)} | ${mdCell(d.raw_asr, 26)} | ${pct(
          a.raw_cer,
        )} | ${pct(d.raw_cer)} | ${loss} |`,
      );
    }
  }

  // 3. 意图工具（最差置顶）
  const intentSorted = [...withAttr].sort(
    (a, b) => Number(a.attribution.intent_tools.passed) - Number(b.attribution.intent_tools.passed),
  );
  lines.push(
    "",
    "## 3. 意图工具（最差置顶）",
    "",
    "| # | 场景 | 实际路径 | 预期路径 | 工具与参数 | 模拟门禁拒绝 | 结论 |",
    "| ---: | --- | --- | --- | --- | --- | --- |",
  );
  for (const t of intentSorted) {
    const it = t.attribution.intent_tools;
    const toolStr = it.tools.length
      ? it.tools.map((x) => `${x.name}(${mdCell(x.arguments, 32)}, ${x.ms}ms)`).join("; ")
      : "—";
    const gateStr = it.gate_blocked_tools
      .map((g) => `${g.name}: ${g.block_reason}`)
      .concat(it.stop_skipped_reason ? [it.stop_skipped_reason] : [])
      .join("; ") || "—";
    lines.push(
      `| ${t.index} | ${mdCell(t.label, 22)} | ${it.route} | ${it.expect_route || "—"} | ${mdCell(
        toolStr,
        64,
      )} | ${mdCell(gateStr, 48)} | ${it.passed ? "通过" : "异常"} |`,
    );
  }

  // 4. 合成播放（最差置顶）
  const playbackSorted = [...withAttr].sort((a, b) => {
    const pa = a.attribution.tts_playback;
    const pb = b.attribution.tts_playback;
    return (
      Number(pa.passed) - Number(pb.passed) ||
      pb.stall_ms - pa.stall_ms ||
      (pb.end_to_first_audio_ms ?? -1) - (pa.end_to_first_audio_ms ?? -1)
    );
  });
  lines.push(
    "",
    "## 4. 合成播放（最差置顶）",
    "",
    "| # | 场景 | 外放结束→首帧 | 首包合成 | 欠载次数 | 卡顿时长 | 回复摘要 | 结论 |",
    "| ---: | --- | ---: | ---: | ---: | ---: | --- | --- |",
  );
  for (const t of playbackSorted) {
    const p = t.attribution.tts_playback;
    lines.push(
      `| ${t.index} | ${mdCell(t.label, 22)} | ${msFmt(p.end_to_first_audio_ms)} | ${msFmt(
        p.first_synthesis_ready_ms,
      )} | ${p.underruns} | ${msFmt(p.stall_ms)} | ${mdCell(p.reply || p.spoken, 48)} | ${
        p.passed ? "通过" : "异常"
      } |`,
    );
  }

  // 5. 建议优先调整的环节（按严重程度排序）
  lines.push("", "## 建议优先调整的环节", "");
  if (summary.recommendations.length === 0) {
    lines.push("本轮整体测试 4 个环节均未发现显著短板，暂无需调参。");
  } else {
    summary.recommendations.forEach((rec, idx) => {
      lines.push(
        `${idx + 1}. **[${rec.level} · ${rec.stage}]** ${rec.issue}  \n   建议：${rec.action}${
          rec.affected?.length ? `（涉及：\`${rec.affected.join("`, `")}\`）` : ""
        }`,
      );
    });
  }
  lines.push("");
  return lines.join("\n");
}

const AMBIENT_QUIET_LEVEL_MAX = 59;

function isQuietListening(dev, allowIdle = false, ignoreLevel = false) {
  if (!dev) return false;
  const levelOk = ignoreLevel || (Number(dev.level) || 0) <= AMBIENT_QUIET_LEVEL_MAX;
  if (dev.state === "listening" && levelOk) return true;
  if (allowIdle && dev.state === "idle" && levelOk) return true;
  return false;
}

function hasActiveBenchWindowForSession(dev, expectedSessionId) {
  if (!dev || dev.bench_window?.active !== true) return false;
  if (expectedSessionId && dev.session_id && dev.session_id !== expectedSessionId) return false;
  if (expectedSessionId && dev.bench_window?.session_id && dev.bench_window.session_id !== expectedSessionId) {
    return false;
  }
  return true;
}

/** 严格等待设备进入 `listening` 且连续安静 `quietMs`（默认 1000 ms），绝不与播报重叠。 */
async function waitForQuietListening({
  baseUrl,
  token,
  deviceId,
  quietMs,
  timeoutMs,
  pollIntervalMs,
  allowIdle = false,
  ignoreLevel = false,
}) {
  const deadline = Date.now() + timeoutMs;
  let quietSince = null;
  let latestPayload = null;
  let latestDevice = null;

  while (Date.now() <= deadline) {
    const res = await listDebugDevices(baseUrl, token);
    if (res.ok && res.data) {
      latestPayload = res.data;
      latestDevice = (res.data.devices || []).find((d) => d.device_id === deviceId) || null;
      if (!latestDevice) {
        return { ready: false, offline: true, device: null, payload: latestPayload };
      }
      if (isQuietListening(latestDevice, allowIdle, ignoreLevel)) {
        if (quietSince === null) quietSince = Date.now();
        if (Date.now() - quietSince >= quietMs) {
          return { ready: true, offline: false, device: latestDevice, payload: latestPayload };
        }
      } else {
        quietSince = null;
      }
    }
    await sleep(pollIntervalMs);
  }
  return { ready: false, offline: false, device: latestDevice, payload: latestPayload };
}

/** 外放后轮询服务端，等本句产生新轮次并完成 thinking -> speaking -> 回到 listening（安静 quietMs）或退出。 */
async function waitForAcousticTurnCompletion({
  baseUrl,
  token,
  deviceId,
  sessionId,
  prevTurnNumber,
  playbackEndedAtMs,
  quietMs,
  timeoutMs,
  pollIntervalMs,
  expectExit = false,
  ignoreLevel = false,
}) {
  const deadline = Date.now() + timeoutMs;
  const observedPhases = [];
  let matchedTurnId = null;
  let detail = null;
  let quietSince = null;
  let maxObservedLevel = 0;
  let speakingStartedBeforePlaybackEnd = false;
  let extraTurnsCount = 0;

  const pushPhase = (st) => {
    if (st && observedPhases[observedPhases.length - 1] !== st) observedPhases.push(st);
  };

  while (Date.now() <= deadline) {
    const devRes = await listDebugDevices(baseUrl, token);
    const dev = devRes.ok
      ? (devRes.data?.devices || []).find((d) => d.device_id === deviceId) || null
      : null;

    if (dev) {
      pushPhase(dev.state);
      const lvl = Math.max(Number(dev.level) || 0, Number(dev.recent_peak_level) || 0);
      if (lvl > maxObservedLevel) maxObservedLevel = lvl;
      if (dev.state === "speaking" && Date.now() < playbackEndedAtMs) {
        speakingStartedBeforePlaybackEnd = true;
      }
    }

    if (!matchedTurnId) {
      // 优先查设备轮次列表并按 turn 升序取最早产生的新轮次（避免 700 ms 停顿拆成 N+1/N+2 时锁错 N+2 掩盖抢话）
      const turnsRes = await listDeviceTurns(baseUrl, token, deviceId, 10);
      if (turnsRes.ok && Array.isArray(turnsRes.data?.turns)) {
        const newerTurns = turnsRes.data.turns
          .filter(
            (t) =>
              Number(t.turn || 0) > prevTurnNumber &&
              (!sessionId || t.session_id === sessionId),
          )
          .sort((a, b) => Number(a.turn || 0) - Number(b.turn || 0));
        if (newerTurns.length > 0 && newerTurns[0]?.turn_id) {
          matchedTurnId = newerTurns[0].turn_id;
          detail = newerTurns[0];
          extraTurnsCount = Math.max(0, newerTurns.length - 1);
        }
      }
      if (!matchedTurnId && dev && Number(dev.latest_turn || 0) > prevTurnNumber) {
        matchedTurnId = dev.latest_turn_id || `${dev.session_id || sessionId}:${dev.latest_turn}`;
      }
    }

    if (matchedTurnId) {
      const detailRes = await getTurnDetail(baseUrl, token, matchedTurnId);
      if (detailRes.ok && detailRes.data) {
        detail = detailRes.data;
        if (typeof detail.peak_uplink_level === "number") {
          maxObservedLevel = Math.max(maxObservedLevel, detail.peak_uplink_level);
        }
      }
    }

    const terminal = Boolean(detail && detail.status && detail.status !== "running");
    if (terminal) {
      const hasEndTool =
        Array.isArray(detail.tools) && detail.tools.some((t) => t?.name === "end_conversation");
      const isExit = expectExit || detail.route === "fast_exit" || hasEndTool || !dev;
      if (isExit && (!dev || dev.audio_channel_open === false || dev.state === "idle" || dev.state === "listening")) {
        return {
          timedOut: false,
          turnId: matchedTurnId,
          detail,
          observedPhases,
          maxObservedLevel,
          overlapDetected: speakingStartedBeforePlaybackEnd,
          extraTurnsCount,
        };
      }
      if (dev && isQuietListening(dev, detail.route === "fast_exit", ignoreLevel)) {
        if (quietSince === null) quietSince = Date.now();
        if (Date.now() - quietSince >= quietMs) {
          return {
            timedOut: false,
            turnId: matchedTurnId,
            detail,
            observedPhases,
            maxObservedLevel,
            overlapDetected: speakingStartedBeforePlaybackEnd,
            extraTurnsCount,
          };
        }
      } else {
        quietSince = null;
      }
    } else if (!dev && matchedTurnId && detail) {
      return {
        timedOut: false,
        turnId: matchedTurnId,
        detail,
        observedPhases,
        maxObservedLevel,
        overlapDetected: speakingStartedBeforePlaybackEnd,
        extraTurnsCount,
      };
    }

    await sleep(pollIntervalMs);
  }

  return {
    timedOut: true,
    turnId: matchedTurnId,
    detail,
    observedPhases,
    maxObservedLevel,
    overlapDetected: speakingStartedBeforePlaybackEnd,
    extraTurnsCount,
  };
}

function assertNotSymlinkIfExists(p) {
  if (!existsSync(p)) return;
  const st = lstatSync(p);
  if (st.isSymbolicLink()) {
    throw new Error("拒绝访问符号链接路径");
  }
}

function deviceShortForTrace(deviceId) {
  const alnum = String(deviceId || "").replace(/[^A-Za-z0-9]/g, "");
  return alnum.length > 0 ? alnum.slice(-4) : "设备";
}

function localDateStrFromMs(ms) {
  const d = new Date(Number(ms) || Date.now());
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function writeLabelAtomically(dayDir, baseName, label) {
  const labelPath = resolve(dayDir, `${baseName}.label.json`);
  if (existsSync(labelPath)) return false;
  const tmpPath = join(dayDir, `.${baseName}.label.tmp.${process.pid}.${randomBytes(6).toString("hex")}`);
  try {
    writeFileSync(tmpPath, `${JSON.stringify(label, null, 2)}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
    renameSync(tmpPath, labelPath);
    return true;
  } catch {
    try {
      if (existsSync(tmpPath)) unlinkSync(tmpPath);
    } catch {
      // ignore
    }
    return false;
  }
}

/** VC3 #7：外放轮次自动写出 `.label.json`（含 `input_source: "e2e_playback"`）到完整过程记录及 `<runtime>/asr-corpus/<date>/`（不覆盖已有人工标注，0600 原子写）。 */
async function tryAutoLabelAsrCorpus({ runtimeDir, detail, spec, asset, noisePreset }) {
  if (!detail) return false;
  const rawAsr = String(detail.injected_text || "").trim();
  const cerRes = computeCer(spec.text, rawAsr);
  const validTags = new Set(["命令词", "数字", "材料名", "有噪声"]);
  const tags = [
    ...(Array.isArray(spec.tags) ? spec.tags : []),
    ...(noisePreset ? ["有噪声"] : []),
  ].filter((t, idx, arr) => validTags.has(t) && arr.indexOf(t) === idx);
  const audioOrigin =
    asset?.audio_origin ||
    asset?.source_type ||
    (asset?.secondary_recording ? "device-corpus-secondary" : "human-recording");
  const dataset = asset?.dataset || asset?.voice_profile?.dataset || null;
  const license = asset?.license || asset?.voice_profile?.license || null;
  const voiceProfile = {
    ...(asset?.voice_profile || { source: audioOrigin }),
    source: audioOrigin,
    audio_origin: audioOrigin,
    ...(asset?.speaker ? { speaker: asset.speaker } : {}),
    ...(dataset ? { dataset } : {}),
    ...(license ? { license } : {}),
    ...(asset?.secondary_recording ? { secondary_recording: true } : {}),
    ...(noisePreset ? { noise: noisePreset } : {}),
  };

  let wroteAny = false;

  // 1. 写入完整过程记录目录 logs/process-trace/<date>/voice-files/<wav-stem>.label.json
  // ProcessTrace::save_file 生成带时间戳与序号前缀的 `{HHMMSSmmm}-{seq:06}-{dev_short}-t{turn}-user.wav`，
  // 优先使用服务端返回的 `detail.trace_audio_file`，确保 `.label.json` 与 `.wav` 一一配对供 asr-bench 扫描。
  const traceDate = String(detail.corpus_date || localDateStrFromMs(detail.finished_at_ms || Date.now())).trim();
  const turnNo = Number(detail.turn) || 0;
  if (/^\d{4}-\d{2}-\d{2}$/.test(traceDate) && turnNo > 0) {
    try {
      const traceRoot = resolve(resolveProcessTraceDir(runtimeDir));
      const devShort = deviceShortForTrace(detail.device_id);
      const bareStem = `${devShort}-t${turnNo}-user`;
      const staysInTrace = (p) => {
        const rel = relative(traceRoot, p);
        return Boolean(rel) && !rel.startsWith("..") && !rel.startsWith("/") && !/^[A-Za-z]:/.test(rel);
      };
      const assertNoSymlinkChain = (baseDir, targetPath) => {
        assertNotSymlinkIfExists(baseDir);
        const rel = relative(baseDir, targetPath);
        const parts = rel.split(/[\\/]+/).filter(Boolean);
        let cur = baseDir;
        for (const part of parts) {
          cur = join(cur, part);
          assertNotSymlinkIfExists(cur);
        }
      };

      let matchedWavPath = null;
      const rawRel = typeof detail.trace_audio_file === "string" ? detail.trace_audio_file.trim() : "";
      if (rawRel && rawRel.endsWith(".wav") && !rawRel.includes("..")) {
        const cand = resolve(traceRoot, rawRel);
        if (staysInTrace(cand)) {
          // ProcessTrace 写盘走后台线程，若刚落盘可短暂重试 3 次（共 150 ms）
          for (let attempt = 0; attempt < 4; attempt += 1) {
            assertNoSymlinkChain(traceRoot, cand);
            if (existsSync(cand)) {
              matchedWavPath = cand;
              break;
            }
            if (attempt < 3) await sleep(50);
          }
        }
      }
      if (!matchedWavPath) {
        const voiceFilesDir = resolve(traceRoot, traceDate, "voice-files");
        if (staysInTrace(voiceFilesDir) && existsSync(voiceFilesDir)) {
          assertNoSymlinkChain(traceRoot, voiceFilesDir);
          const suffix = `-${bareStem}.wav`;
          const exactName = `${bareStem}.wav`;
          const targetMs = Number(detail.started_at_ms || detail.finished_at_ms || 0);
          let best = null;
          for (const entry of readdirSync(voiceFilesDir, { withFileTypes: true })) {
            if (!entry.isFile()) continue;
            if (entry.name !== exactName && !entry.name.endsWith(suffix)) continue;
            const full = resolve(voiceFilesDir, entry.name);
            if (!staysInTrace(full)) continue;
            assertNotSymlinkIfExists(full);
            const mtimeMs = lstatSync(full).mtimeMs || 0;
            const diff = targetMs > 0 ? Math.abs(mtimeMs - targetMs) : -mtimeMs;
            if (!best || diff < best.diff) {
              best = { full, diff };
            }
          }
          if (best) matchedWavPath = best.full;
        }
      }
      if (matchedWavPath && staysInTrace(matchedWavPath)) {
        assertNoSymlinkChain(traceRoot, matchedWavPath);
        const targetDir = dirname(matchedWavPath);
        const wavStem = basename(matchedWavPath, ".wav");
        const turnId = String(detail.corpus_turn_id || `${detail.session_id || devShort}_t${turnNo}`).replace(
          /[^A-Za-z0-9_-]/g,
          "_",
        );
        const traceLabel = {
          turn_id: turnId,
          turn: turnNo,
          device_id: detail.device_id || "",
          session_id: detail.session_id || "",
          date: traceDate,
          input_source: "e2e_playback",
          source_type: audioOrigin,
          audio_origin: audioOrigin,
          ...(dataset ? { dataset } : {}),
          ...(license ? { license } : {}),
          raw_text: rawAsr,
          corrected_text: String(detail.corrected_text || rawAsr).trim(),
          reference_text: spec.text,
          verified_correct: cerRes.edits === 0,
          tags,
          updated_at_ms: Date.now(),
          label_source: "e2e-playback",
          voice_profile: voiceProfile,
        };
        if (writeLabelAtomically(targetDir, wavStem, traceLabel)) {
          wroteAny = true;
        }
      }
    } catch {
      // 忽略过程记录目录未落盘或符号链接拒绝
    }
  }

  // 2. 兼容写入 <runtime>/asr-corpus/<date>/<turn-id>.label.json
  if (!detail?.corpus_date || !detail?.corpus_turn_id) return wroteAny;
  const dateStr = String(detail.corpus_date).trim();
  const turnIdStr = String(detail.corpus_turn_id).trim();
  if (!/^\d{4}-\d{2}-\d{2}$/.test(dateStr) || !/^[A-Za-z0-9_-]{1,128}$/.test(turnIdStr)) {
    return wroteAny;
  }
  const corpusRoot = resolve(runtimeDir, "asr-corpus");
  const dayDir = resolve(corpusRoot, dateStr);
  const wavPath = resolve(dayDir, `${turnIdStr}.wav`);
  const metaPath = resolve(dayDir, `${turnIdStr}.json`);
  const labelPath = resolve(dayDir, `${turnIdStr}.label.json`);
  const staysInside = (p) => {
    const rel = relative(corpusRoot, p);
    return Boolean(rel) && !rel.startsWith("..") && !rel.startsWith("/") && !/^[A-Za-z]:/.test(rel);
  };
  if (!staysInside(dayDir) || !staysInside(wavPath) || !staysInside(metaPath) || !staysInside(labelPath)) {
    return wroteAny;
  }
  const deadline = Date.now() + 1_500;
  while (Date.now() <= deadline && (!existsSync(wavPath) || !existsSync(metaPath))) {
    await sleep(50);
  }
  if (!existsSync(wavPath) || !existsSync(metaPath)) return wroteAny;
  try {
    assertNotSymlinkIfExists(corpusRoot);
    assertNotSymlinkIfExists(dayDir);
    assertNotSymlinkIfExists(wavPath);
    assertNotSymlinkIfExists(metaPath);
    assertNotSymlinkIfExists(labelPath);
  } catch {
    return wroteAny;
  }
  if (existsSync(labelPath)) return wroteAny;

  const label = {
    turn_id: turnIdStr,
    date: dateStr,
    input_source: "e2e_playback",
    source_type: audioOrigin,
    audio_origin: audioOrigin,
    ...(dataset ? { dataset } : {}),
    ...(license ? { license } : {}),
    reference_text: spec.text,
    verified_correct: cerRes.edits === 0,
    tags,
    updated_at_ms: Date.now(),
    label_source: "e2e-playback",
    voice_profile: voiceProfile,
  };
  if (writeLabelAtomically(dayDir, turnIdStr, label)) {
    wroteAny = true;
  }
  return wroteAny;
}

/** VC2 可选服务端数字音频对照：读取同一 WAV 的 16 kHz PCM 经 `/debug/v1/devices/{id}/inject-audio` 注入并等待完成。 */
async function runDigitalControlTurn({
  baseUrl,
  token,
  deviceId,
  wavPath,
  turnTimeoutMs,
  pollIntervalMs,
  quietMs,
  ignoreLevel = false,
}) {
  const ready = await waitForQuietListening({
    baseUrl,
    token,
    deviceId,
    quietMs,
    timeoutMs: Math.min(30_000, turnTimeoutMs),
    pollIntervalMs,
    allowIdle: true,
    ignoreLevel,
  });
  if (!ready.ready) {
    return { error: "数字注入前等待设备空闲超时", code: "idle_timeout", status: 0 };
  }
  let wav;
  try {
    wav = readWavPcm16(wavPath);
  } catch (err) {
    return { error: err?.message || "读取 WAV 失败", code: "invalid_audio", status: 0 };
  }
  if (wav.sampleRate !== 16_000 && wav.sampleRate !== 24_000) {
    return {
      error: `数字音频注入仅支持 16000 或 24000 Hz 单声道 WAV（当前 ${wav.sampleRate} Hz）`,
      code: "invalid_sample_rate",
      status: 0,
    };
  }
  const injectRes = await debugFetch(
    baseUrl,
    token,
    `/debug/v1/devices/${encodeURIComponent(deviceId)}/inject-audio`,
    {
      method: "POST",
      body: {
        pcm_base64: wav.pcm.toString("base64"),
        sample_rate: wav.sampleRate,
        frame_ms: 60,
      },
      timeoutMs: 45_000,
    },
  );
  if (!injectRes.ok || !injectRes.data?.turn_id) {
    return {
      error: injectRes.data?.message || `数字音频注入失败（HTTP ${injectRes.status}）`,
      code: injectRes.data?.error || `http_${injectRes.status}`,
      status: injectRes.status,
    };
  }
  const turnId = injectRes.data.turn_id;
  const deadline = Date.now() + turnTimeoutMs;
  let detail = null;
  while (Date.now() <= deadline) {
    const res = await getTurnDetail(baseUrl, token, turnId);
    if (res.ok && res.data) {
      detail = res.data;
      if (detail.status && detail.status !== "running") {
        const devIdle =
          !detail.device_online ||
          detail.device_state === "idle" ||
          (detail.device_state === "listening" && (ignoreLevel || (Number(detail.device_level) || 0) <= AMBIENT_QUIET_LEVEL_MAX));
        if (devIdle) return { detail };
      }
    }
    await sleep(pollIntervalMs);
  }
  return { detail, error: "数字对照轮次超时", code: "turn_timeout", status: 0 };
}

/** 评估外放校准结果；电平过低/过高或识别失败时返回中文摆放/音量指引。 */
export function evaluateCalibrationResult({ calibrationText, result }) {
  if (!result || result.timedOut || !result.detail) {
    const peak = result?.maxObservedLevel ?? 0;
    return {
      ok: false,
      peak_level: peak,
      recognized: "",
      cer: 1,
      message: `校准失败：外放后小智未触发识别（观测峰值电平 ${peak}）。请把小智放到离喇叭 30–50 cm、调高电脑音量并确认麦克风无遮挡后重试。`,
    };
  }
  const recognized = String(result.detail.corrected_text || result.detail.injected_text || "").trim();
  const peak =
    typeof result.detail.peak_uplink_level === "number"
      ? result.detail.peak_uplink_level
      : result.maxObservedLevel ?? 0;
  const cer = Number(computeCer(calibrationText, recognized).cer.toFixed(4));
  if (!recognized) {
    return {
      ok: false,
      peak_level: peak,
      recognized,
      cer: 1,
      message: `校准失败：小智未识别出任何文字（上行峰值电平 ${peak}）。请把小智放到离喇叭 30–50 cm、调高电脑音量后重试。`,
    };
  }
  if (peak > 0 && peak < 12) {
    return {
      ok: false,
      peak_level: peak,
      recognized,
      cer,
      message: `校准失败：小智上行电平过低（峰值 ${peak} < 12）。请把小智移近喇叭至 30–50 cm 或调高电脑音量后重试。`,
    };
  }
  if (peak >= 98 && cer > 0.45) {
    return {
      ok: false,
      peak_level: peak,
      recognized,
      cer,
      message: `校准失败：小智上行电平过高疑似削顶失真（峰值 ${peak}，识别“${recognized}”）。请把小智拉远至 30–50 cm 或调低电脑音量后重试。`,
    };
  }
  if (cer > 0.7) {
    return {
      ok: false,
      peak_level: peak,
      recognized,
      cer,
      message: `校准失败：外放识别偏差过大（识别为“${recognized}”，CER ${(cer * 100).toFixed(0)}%，峰值电平 ${peak}）。请把小智放到离喇叭 30–50 cm、调高或调低电脑音量并减少背景干扰后重试。`,
    };
  }
  return {
    ok: true,
    peak_level: peak,
    recognized,
    cer,
    message: `校准通过：峰值电平 ${peak}，识别“${recognized}”（CER ${(cer * 100).toFixed(1)}%）`,
  };
}

/**
 * 计算整轮测试自首次开窗以来的剩余时间窗分钟数（向上取整，至少 1；剩余 <= 0 时返回 0，不再续开）。
 */
export function computeRemainingBenchMinutes({ firstOpenedAtMs, maxMinutes, nowMs = Date.now() }) {
  const capped = Math.max(1, Math.min(60, Math.floor(Number(maxMinutes) || 1)));
  if (!Number.isFinite(firstOpenedAtMs) || firstOpenedAtMs <= 0) {
    return capped;
  }
  const budgetMs = capped * 60_000;
  const elapsedMs = Math.max(0, Number(nowMs) - firstOpenedAtMs);
  const remainingMs = budgetMs - elapsedMs;
  if (remainingMs <= 0) return 0;
  return Math.max(1, Math.min(60, Math.ceil(remainingMs / 60_000)));
}

/** 从 `/debug/v1/devices` 响应中提取指定设备最近一次测试时间窗关闭记录（优先设备字段，回退顶层映射）。 */
export function extractLastBenchWindowClosed(devicesPayload, device, deviceId) {
  if (device && device.bench_window_last_closed && typeof device.bench_window_last_closed === "object") {
    return device.bench_window_last_closed;
  }
  const map = devicesPayload?.bench_window_last_closed;
  if (map && typeof map === "object" && deviceId && map[deviceId] && typeof map[deviceId] === "object") {
    return map[deviceId];
  }
  return null;
}

/** 仅当旧时间窗属于上一轮绑定的 session_id 且因设备断开（device_disconnected）关闭时才允许退出后重开。 */
export function canReopenBenchWindowAfterExit({ lastClosed, boundSessionId }) {
  if (!lastClosed || typeof lastClosed !== "object") return false;
  if (String(lastClosed.reason || "").trim() !== "device_disconnected") return false;
  if (!boundSessionId || String(lastClosed.session_id || "").trim() !== String(boundSessionId)) {
    return false;
  }
  return true;
}

/** 将关窗原因类别或整轮时长耗尽转为清晰中文中止说明，供汇报与终端输出。 */
export function formatBenchWindowStopReason(lastClosed, { remainingZero = false } = {}) {
  if (remainingZero) {
    return "时间窗到期：整轮已达到 --max-minutes 时长上限，不再续开测试时间窗，已停止后续外放";
  }
  const reason = String(lastClosed?.reason || "").trim();
  switch (reason) {
    case "user_stopped":
      return "用户已结束测试：检测到在 App 手动关闭了测试时间窗，已停止后续外放";
    case "expired":
      return "时间窗到期：测试时间窗已到达最长时长，已停止后续外放";
    case "debug_disabled":
      return "调试开关已关闭：测试时间窗已终止，已停止后续外放";
    case "token_expired":
      return "调试令牌已失效：测试时间窗已终止，已停止后续外放";
    case "device_revoked":
      return "设备授权已撤销：测试时间窗已终止，已停止后续外放";
    case "device_removed":
      return "设备已移除：测试时间窗已终止，已停止后续外放";
    case "script_closed":
      return "测试时间窗已关闭（脚本主动关闭），已停止后续外放";
    default:
      return "测试时间窗已关闭（非设备断开或会话不匹配），已停止后续外放";
  }
}

/**
 * VC3 真小智整体测试主入口（支持注入假播放器、假素材准备器和假噪声器供单测验证）。
 */
export async function runXiaozhiE2e(opts = {}) {
  const nowFn = typeof opts.nowFn === "function" ? opts.nowFn : Date.now;
  const turnTimeoutMs = Number(opts.turnTimeoutMs ?? DEFAULT_TURN_TIMEOUT_MS);
  const pollIntervalMs = Number(opts.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS);
  const idleWaitMs = Number(opts.idleWaitMs ?? DEFAULT_IDLE_WAIT_MS);
  const quietMs = Number(opts.quietMs ?? DEFAULT_QUIET_MS);
  const wakeTimeoutMs = Number(opts.wakeTimeoutMs ?? DEFAULT_WAKE_TIMEOUT_MS);
  const wakeRetries = Math.max(1, Math.floor(Number(opts.wakeRetries ?? DEFAULT_WAKE_RETRIES)));
  const maxMinutes = Math.max(1, Math.min(60, Number(opts.maxMinutes ?? DEFAULT_MAX_MINUTES)));
  const calibrationText = String(opts.calibrationText || DEFAULT_CALIBRATION_TEXT);
  const noisePreset = opts.noise ? String(opts.noise).trim().toLowerCase() : null;
  const noiseGain = Number(opts.noiseGain ?? 0.22);
  const compareDigital = Boolean(opts.compareDigital);

  const corpusPath = resolve(opts.corpus || DEFAULT_E2E_CORPUS_PATH);
  const corpusRaw = JSON.parse(readFileSync(corpusPath, "utf8"));
  const corpusName = String(corpusRaw?.name || "custom-e2e");
  const rawCorpusTurns = Array.isArray(corpusRaw)
    ? corpusRaw
    : Array.isArray(corpusRaw?.turns)
      ? corpusRaw.turns
      : [];
  const corpusTurns = rawCorpusTurns.filter((spec) => !isWakeCorpusItem(spec));
  // 1. 预检：读取令牌、查在线设备、检查播放器
  const { runtimeDir, token, baseUrl } = resolveDebugConnection(opts);
  const runId = opts.runId || formatRunId("e2e");
  const resolvedRecordingsDir = opts.recordingsDir
    ? resolve(opts.recordingsDir)
    : join(runtimeDir, BENCH_RECORDINGS_DIR_REL);
  const resolvedCorpusDir = opts.corpusDir ? resolve(opts.corpusDir) : join(runtimeDir, "asr-corpus");
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
      resolvedCorpusDir,
      traceDir,
      archiveDir,
      opts.tokenFile ? dirname(resolve(opts.tokenFile)) : "",
    ].filter(Boolean),
  };
  if (corpusTurns.length === 0) {
    throw new Error(`语料文件不含有效轮次：${redactText(corpusPath, redactOpts)}`);
  }

  const devicesRes = await listDebugDevices(baseUrl, token);
  if (!devicesRes.ok) {
    return {
      exitCode: 1,
      archiveDir: null,
      summary: null,
      turns: [],
      errorMessage: devicesRes.data?.message || `查询在线设备失败（HTTP ${devicesRes.status}）`,
    };
  }
  const onlineDevices = Array.isArray(devicesRes.data?.devices) ? devicesRes.data.devices : [];
  let targetDevice = opts.device
    ? onlineDevices.find((d) => d.device_id === opts.device) || null
    : onlineDevices[0] || null;
  if (opts.device && !targetDevice && onlineDevices.length > 0) {
    return {
      exitCode: 1,
      archiveDir: null,
      summary: null,
      turns: [],
      errorMessage: `指定的设备 ${opts.device} 当前不在线（在线设备：${onlineDevices.map((d) => d.device_id).join(", ")}）`,
    };
  }

  let player = null;
  if (typeof opts.playWavFn !== "function") {
    player =
      typeof opts.checkPlayerFn === "function"
        ? opts.checkPlayerFn()
        : checkPlayerAvailable(opts.platform || process.platform);
  }

  const speakerFilter = String(opts.speaker || "").trim();
  const includeDeviceCorpus = Boolean(opts.includeDeviceCorpus);
  const rawMaxOnline = opts.maxOnline === undefined || opts.maxOnline === null ? DEFAULT_MAX_ONLINE : Number(opts.maxOnline);
  if (!Number.isInteger(rawMaxOnline) || rawMaxOnline < 0) {
    throw new Error("--max-online 须为大于等于 0 的整数");
  }
  const maxOnline = rawMaxOnline;
  let wakeWord =
    String(targetDevice?.wake_word || devicesRes.data?.wake_word || "").trim() || DEFAULT_WAKE_WORD;
  let wakeResolved = resolveWakeWordAsset({
    wakeWord,
    runtimeDir,
    recordingsDir: resolvedRecordingsDir,
    speaker: speakerFilter,
  });
  const wakeEvents = [];

  const assetPreparer = opts.prepareVoiceAssetsFn || prepareVoiceAssets;
  const assets = await assetPreparer({
    turns: corpusTurns,
    calibrationText: opts.skipCalibration ? "" : calibrationText,
    wakeWord,
    runtimeDir,
    recordingsDir: resolvedRecordingsDir,
    corpusDir: resolvedCorpusDir,
    speaker: speakerFilter,
    includeDeviceCorpus,
    maxOnline,
    persistRotation: false,
  });

  const missingTurns = Array.isArray(assets.missingTurns) ? assets.missingTurns : [];
  const skippedAssets = Array.isArray(assets.skippedAssets) ? assets.skippedAssets : [];
  const onlineRotation = assets.onlineRotation || null;
  if (!Array.isArray(assets.preparedTurns) || assets.preparedTurns.length === 0) {
    const missingList = missingTurns.map((m) => `- ${m.id}（${m.label}）：${m.text}`).join("\n");
    const skippedList = skippedAssets.map((s) => `- ${s.filename || s.id}：${s.reason}`).join("\n");
    const speakerHint = speakerFilter ? `（已指定说话人「${speakerFilter}」）` : "";
    return {
      exitCode: 1,
      archiveDir: null,
      summary: null,
      turns: [],
      missingTurns,
      skippedAssets,
      errorMessage: [
        `未找到任何可用的真人测试录音${speakerHint}，已终止整体测试（绝不使用合成声音替代）。`,
        RECORDING_APP_HINT,
        missingList ? `缺少真人录音的语料：\n${missingList}` : "",
        skippedList ? `被跳过的素材：\n${skippedList}` : "",
      ]
        .filter(Boolean)
        .join("\n"),
    };
  }
  if (missingTurns.length > 0 && opts.logMissing !== false) {
    const preview = missingTurns.map((m) => `${m.id}（${m.label}）`).join("、");
    console.warn(
      `[xiaozhi-e2e] 缺少真人录音的语料（已跳过 ${missingTurns.length} 句）：${preview}。${RECORDING_APP_HINT}`,
    );
  }
  if (skippedAssets.length > 0 && opts.logMissing !== false) {
    const preview = skippedAssets.map((s) => `${s.filename || s.id}（${s.reason}）`).join("；");
    console.warn(`[xiaozhi-e2e] 已跳过 ${skippedAssets.length} 条不合格或疑似指令的素材：${preview}`);
  }
  const effectiveCalibrationText = assets.calibrationEffectiveText || calibrationText;

  // n146 开测前自动唤醒：设备未上线（非常连待机）、常连未开音频通道或未处于 listening 时，
  // 用当前唤醒词真人录音外放自动唤醒并等待进入 listening；绝不改为提示用户手动喊。
  const needsInitialWake =
    !targetDevice || targetDevice.audio_channel_open === false || targetDevice.state === "idle";
  if (needsInitialWake) {
    const wakeRes = await attemptAutoWake({
      baseUrl,
      token,
      deviceId: opts.device || targetDevice?.device_id || "",
      wakeWord,
      wakeAsset: wakeResolved.asset,
      missingReason: wakeResolved.missingReason,
      trigger: targetDevice ? "standby_initial" : "offline_initial",
      player,
      playWavFn: opts.playWavFn,
      waitFor: "listening",
      wakeTimeoutMs,
      wakeRetries,
      pollIntervalMs,
    });
    wakeEvents.push(wakeRes.event);
    if (!wakeRes.ok || !wakeRes.device) {
      return {
        exitCode: 1,
        archiveDir: null,
        summary: null,
        turns: [],
        wakeEvents,
        errorMessage: wakeRes.error || NO_ONLINE_DEVICE_MESSAGE,
      };
    }
    targetDevice = wakeRes.device;
    const deviceWakeWord = String(targetDevice.wake_word || "").trim();
    if (deviceWakeWord && deviceWakeWord !== wakeWord) {
      wakeWord = deviceWakeWord;
      wakeResolved = resolveWakeWordAsset({
        wakeWord,
        runtimeDir,
        recordingsDir: resolvedRecordingsDir,
        speaker: speakerFilter,
      });
    }
  }
  const deviceId = targetDevice.device_id;
  const sessionId = targetDevice.session_id || "";

  // VE3：开测前估算总时长，超出测试时间窗上限时打印提示并自动分批（绝不延长单窗超过 60 分钟上限）
  const hasCalibration = !opts.skipCalibration && Boolean(assets.calibrationAsset);
  const batchSchedule = planE2eBatches(assets.preparedTurns, {
    maxMinutes,
    quietMs,
    compareDigital,
    hasCalibration,
    calibrationDurationMs: assets.calibrationAsset?.duration_ms || 4_000,
    estimatedReplyMs: opts.estimatedReplyMs,
    estimatedIntervalMs: opts.estimatedIntervalMs,
    batchWindowMs: opts.batchWindowMs,
  });
  if (batchSchedule.batches.length > 1 && opts.logBatching !== false) {
    const estMin = (batchSchedule.totalEstimatedMs / 60_000).toFixed(1);
    console.warn(
      `[xiaozhi-e2e] 预计总时长约 ${estMin} 分钟（${assets.preparedTurns.length} 句），超过单次测试时间窗上限 ${maxMinutes} 分钟；将自动分为 ${batchSchedule.batches.length} 批执行，每批正常关闭并重开时间窗且重新确认设备会话一致（单窗绝不超过 60 分钟上限）。`,
    );
  }

  // 2. 打开测试时间窗 + 打开过程记录（全程包裹在 try/finally 与 SIGINT/SIGTERM 钩子，任何异常或中断都必关窗与恢复）
  const startedAtMs = Date.now();
  const ignoreLevel = Boolean(noisePreset);
  let benchWindowOpened = false;
  let firstWindowOpenedAtMs = null;
  let boundSessionId = sessionId;
  let previousTraceEnabled = null;
  let noiseHandle = null;
  let noiseInfo = null;
  let activePlayChild = null;
  let calibrationReport = null;
  let autoLabeledCount = 0;
  let batchesExecuted = 0;
  let sessionAborted = false;
  let runStopReason = null;
  let previousWasExit = false;
  let cleanedUp = false;
  const results = [];

  const cleanup = async () => {
    if (cleanedUp) return;
    cleanedUp = true;
    if (activePlayChild) {
      try {
        activePlayChild.kill("SIGTERM");
      } catch {
        // 忽略播放子进程停止异常
      }
      activePlayChild = null;
    }
    if (noiseHandle) {
      try {
        noiseHandle.stop();
      } catch {
        // 忽略噪声进程停止异常
      }
      noiseHandle = null;
    }
    if (benchWindowOpened) {
      try {
        await controlBenchWindow(baseUrl, token, { deviceId, action: "close" });
      } catch {
        // 尽力关闭测试时间窗
      }
      benchWindowOpened = false;
    }
    if (previousTraceEnabled !== null) {
      try {
        await setProcessTrace(baseUrl, token, previousTraceEnabled, true);
      } catch {
        // 尽力恢复过程记录开关
      }
    }
  };
  const unregisterSignals = registerSignalCleanup(cleanup);

  try {
    let stopAllBatches = false;
    for (let batchIdx = 0; batchIdx < batchSchedule.batches.length && !stopAllBatches; batchIdx += 1) {
      const batchEntries = batchSchedule.batches[batchIdx];

      // VE3 分批重开前必须重新确认仍是同一次设备会话；若设备断开或会话变更，立即中止不重开时间窗
      if (batchIdx > 0) {
        const recheck = await listDebugDevices(baseUrl, token);
        const recheckDevices = recheck.ok && Array.isArray(recheck.data?.devices) ? recheck.data.devices : [];
        const currentDev = recheckDevices.find((d) => d.device_id === deviceId);
        if (!currentDev || !currentDev.session_id || currentDev.session_id !== boundSessionId) {
          sessionAborted = true;
          const firstMissing = batchEntries[0];
          const firstSpec = firstMissing?.item?.spec || {};
          const firstAsset = firstMissing?.item?.asset || {};
          const firstSourceType = firstAsset.source_type || "human-recording";
          results.push({
            index: (firstMissing?.globalIndex ?? results.length) + 1,
            id: String(firstSpec.id || `batch_${batchIdx + 1}`),
            label: firstSpec.label || `第 ${batchIdx + 1} 批会话复核`,
            text: firstSpec.text || "",
            speaker: firstAsset.speaker || "未标注说话人",
            source_type: firstSourceType,
            audio_origin: firstAsset.audio_origin || firstSourceType,
            dataset: firstAsset.dataset || null,
            license: firstAsset.license || null,
            secondary_recording: Boolean(firstAsset.secondary_recording),
            batch: batchIdx + 1,
            verdict: "failed",
            failure_reason: !currentDev
              ? `第 ${batchIdx + 1} 批重开测试时间窗前设备 ${deviceId} 已断开连接，已中止后续批次`
              : `第 ${batchIdx + 1} 批重开测试时间窗前检测到设备会话已变更（原会话 ${boundSessionId}，当前 ${currentDev.session_id}），已中止后续批次`,
            wall_ms: 0,
            audio_source: {
              filename: firstAsset.filename || "",
              source_type: firstSourceType,
              audio_origin: firstAsset.audio_origin || firstSourceType,
              speaker: firstAsset.speaker || "未标注说话人",
              dataset: firstAsset.dataset || null,
              license: firstAsset.license || null,
              source_url: firstAsset.source_url || null,
              secondary_recording: Boolean(firstAsset.secondary_recording),
              voice_profile: firstAsset.voice_profile || null,
              duration_ms: firstAsset.duration_ms || 0,
            },
            attribution: null,
            detail: null,
          });
          break;
        }
      }

      const winRes = await controlBenchWindow(baseUrl, token, {
        deviceId,
        action: "open",
        maxMinutes,
      });
      if (!winRes.ok) {
        if (batchIdx === 0) {
          return {
            exitCode: 1,
            archiveDir: null,
            summary: null,
            turns: [],
            errorMessage: winRes.data?.message || `打开测试时间窗失败（HTTP ${winRes.status}）`,
          };
        }
        sessionAborted = true;
        results.push({
          index: (batchEntries[0]?.globalIndex ?? results.length) + 1,
          id: String(batchEntries[0]?.item?.spec?.id || `batch_${batchIdx + 1}`),
          label: `第 ${batchIdx + 1} 批开启时间窗`,
          text: batchEntries[0]?.item?.spec?.text || "",
          speaker: batchEntries[0]?.item?.asset?.speaker || "未标注说话人",
          source_type: batchEntries[0]?.item?.asset?.source_type || "human-recording",
          audio_origin: batchEntries[0]?.item?.asset?.audio_origin || "human-recording",
          dataset: batchEntries[0]?.item?.asset?.dataset || null,
          license: batchEntries[0]?.item?.asset?.license || null,
          secondary_recording: Boolean(batchEntries[0]?.item?.asset?.secondary_recording),
          batch: batchIdx + 1,
          verdict: "failed",
          failure_reason: winRes.data?.message || `第 ${batchIdx + 1} 批重开测试时间窗失败（HTTP ${winRes.status}）`,
          wall_ms: 0,
          audio_source: null,
          attribution: null,
          detail: null,
        });
        break;
      }
      benchWindowOpened = true;
      if (firstWindowOpenedAtMs === null) {
        firstWindowOpenedAtMs = nowFn();
      }
      const openedSessionId = winRes.data?.bench_window?.session_id || "";
      if (batchIdx === 0) {
        if (openedSessionId) boundSessionId = openedSessionId;
      } else if (openedSessionId && boundSessionId && openedSessionId !== boundSessionId) {
        sessionAborted = true;
        await controlBenchWindow(baseUrl, token, { deviceId, action: "close" });
        benchWindowOpened = false;
        results.push({
          index: (batchEntries[0]?.globalIndex ?? results.length) + 1,
          id: String(batchEntries[0]?.item?.spec?.id || `batch_${batchIdx + 1}`),
          label: `第 ${batchIdx + 1} 批会话复核`,
          text: batchEntries[0]?.item?.spec?.text || "",
          speaker: batchEntries[0]?.item?.asset?.speaker || "未标注说话人",
          source_type: batchEntries[0]?.item?.asset?.source_type || "human-recording",
          audio_origin: batchEntries[0]?.item?.asset?.audio_origin || "human-recording",
          dataset: batchEntries[0]?.item?.asset?.dataset || null,
          license: batchEntries[0]?.item?.asset?.license || null,
          secondary_recording: Boolean(batchEntries[0]?.item?.asset?.secondary_recording),
          batch: batchIdx + 1,
          verdict: "failed",
          failure_reason: `第 ${batchIdx + 1} 批打开时间窗后会话已变更（原 ${boundSessionId}，现 ${openedSessionId}），已立即关窗中止`,
          wall_ms: 0,
          audio_source: null,
          attribution: null,
          detail: null,
        });
        break;
      }
      batchesExecuted += 1;

      if (batchIdx === 0) {
        const traceRes = await setProcessTrace(baseUrl, token, true, false);
        if (traceRes.ok && typeof traceRes.data?.previous_enabled === "boolean") {
          previousTraceEnabled = traceRes.data.previous_enabled;
        }

        // 3. 校准（仅在第一批、listening 安静且测试时间窗确认开启时执行）
        if (hasCalibration) {
          const calReady = await waitForQuietListening({
            baseUrl,
            token,
            deviceId,
            quietMs,
            timeoutMs: idleWaitMs,
            pollIntervalMs,
            allowIdle: false,
            ignoreLevel: false,
          });
          if (!calReady.ready) {
            return {
              exitCode: 1,
              archiveDir: null,
              summary: null,
              turns: [],
              errorMessage: calReady.offline
                ? `校准前设备 ${deviceId} 断开了连接`
                : "校准前等待设备进入安静聆听超时（请检查喇叭音量与摆放距离 30–50 cm）",
            };
          }
          if (!hasActiveBenchWindowForSession(calReady.device, boundSessionId)) {
            return {
              exitCode: 1,
              archiveDir: null,
              summary: null,
              turns: [],
              errorMessage: "校准前检测到测试时间窗已关闭或设备会话已变更，为防止物理动作绕过安全锁，已中止外放",
            };
          }
          const prevTurn = Number(calReady.device?.latest_turn || 0);
          const calPlay = await playWav(assets.calibrationAsset.wavPath, {
            player,
            playWavFn: opts.playWavFn,
            onSpawn: (c) => {
              activePlayChild = c;
            },
            spec: { id: "calibration", text: effectiveCalibrationText },
          });
          const calWait = await waitForAcousticTurnCompletion({
            baseUrl,
            token,
            deviceId,
            sessionId: calReady.device?.session_id || boundSessionId,
            prevTurnNumber: prevTurn,
            playbackEndedAtMs: calPlay.ended_at_ms,
            quietMs,
            timeoutMs: Math.min(30_000, turnTimeoutMs),
            pollIntervalMs,
            ignoreLevel: false,
          });
          calibrationReport = evaluateCalibrationResult({
            calibrationText: effectiveCalibrationText,
            result: calWait,
          });
          if (!calibrationReport.ok) {
            return {
              exitCode: 1,
              archiveDir: null,
              summary: null,
              turns: [],
              calibration: calibrationReport,
              errorMessage: calibrationReport.message,
            };
          }
        }

        // VE3：通过校准准备开测后才持久化 `.rotation.json`，确保下次从新位置继续轮换
        if (onlineRotation && onlineRotation.selectedCount > 0 && !onlineRotation.persisted) {
          onlineRotation.persisted = saveOnlineRotation(onlineRotation.onlineDir, {
            cursor: onlineRotation.cursorAfter,
            totalPool: onlineRotation.totalPool,
            lastIds: onlineRotation.selectedIds,
          });
        }

        // 4. 若指定了背景噪声，开始低音量循环外放背景噪声
        if (noisePreset) {
          const startNoise = opts.startNoiseFn || startBackgroundNoise;
          noiseHandle = startNoise({
            preset: noisePreset,
            gain: noiseGain,
            cacheDir: join(runtimeDir, "voice-bench", "noise-cache"),
            pythonBin: opts.pythonBin || "python3",
            platform: opts.platform || process.platform,
            player,
          });
          if (noiseHandle) {
            noiseInfo = {
              filename: noiseHandle.filename,
              preset: noiseHandle.preset,
              gain: noiseHandle.gain,
            };
          }
        }
      }

      // 5. 严格串行逐句执行（每句外放前必须复核测试时间窗仍在打开状态，绝不在无锁状态下外放）
      for (const entry of batchEntries) {
        const { spec, asset } = entry.item;
        const turnIndex = entry.globalIndex + 1;
        const turnIdLabel = String(spec.id || `turn_${turnIndex}`);
        const turnStartedAt = Date.now();

        const turnSpeaker = asset.speaker || asset.voice_profile?.speaker || "未标注说话人";
        const turnSecondary = Boolean(asset.secondary_recording || asset.voice_profile?.secondary_recording);
        const turnSourceType =
          asset.source_type || (turnSecondary ? "device-corpus-secondary" : "human-recording");
        const turnAudioOrigin = asset.audio_origin || turnSourceType;
        const turnDataset = asset.dataset || asset.voice_profile?.dataset || null;
        const turnLicense = asset.license || asset.voice_profile?.license || null;
        const turnSourceUrl = asset.source_url || asset.voice_profile?.source_url || null;
        const audioSourceMeta = {
          filename: asset.filename,
          source_type: turnSourceType,
          audio_origin: turnAudioOrigin,
          speaker: turnSpeaker,
          dataset: turnDataset,
          license: turnLicense,
          source_url: turnSourceUrl,
          secondary_recording: turnSecondary,
          voice_profile: asset.voice_profile,
          duration_ms: asset.duration_ms,
        };
        const commonTurnFields = {
          index: turnIndex,
          id: turnIdLabel,
          label: spec.label || turnIdLabel,
          text: spec.text,
          speaker: turnSpeaker,
          source_type: turnSourceType,
          audio_origin: turnAudioOrigin,
          dataset: turnDataset,
          license: turnLicense,
          secondary_recording: turnSecondary,
          batch: batchIdx + 1,
          audio_source: audioSourceMeta,
        };

        // n146 轮次前状态检查：先核对旧测试时间窗是否被用户手动结束/过期/策略关闭，仅在确认安全且允许时自动唤醒
        let preCheckRes = await listDebugDevices(baseUrl, token);
        if (!preCheckRes.ok) {
          benchWindowOpened = false;
          runStopReason =
            preCheckRes.data?.message || `查询设备状态失败（HTTP ${preCheckRes.status}），已停止后续外放`;
          results.push({
            ...commonTurnFields,
            verdict: "failed",
            failure_reason: runStopReason,
            wall_ms: Date.now() - turnStartedAt,
            attribution: null,
            detail: null,
          });
          stopAllBatches = true;
          break;
        }
        let preDev = (preCheckRes.data?.devices || []).find((d) => d.device_id === deviceId) || null;
        if (previousWasExit && preDev && preDev.audio_channel_open !== false && preDev.state !== "idle") {
          const settleDeadline = Date.now() + Math.min(3_000, idleWaitMs);
          while (Date.now() <= settleDeadline) {
            const curClosed = extractLastBenchWindowClosed(preCheckRes.data, preDev, deviceId);
            if (
              !hasActiveBenchWindowForSession(preDev, boundSessionId) &&
              curClosed &&
              !canReopenBenchWindowAfterExit({ lastClosed: curClosed, boundSessionId })
            ) {
              break;
            }
            await sleep(pollIntervalMs);
            preCheckRes = await listDebugDevices(baseUrl, token);
            if (!preCheckRes.ok) break;
            preDev = (preCheckRes.data?.devices || []).find((d) => d.device_id === deviceId) || null;
            if (!preDev || preDev.audio_channel_open === false || preDev.state === "idle") {
              break;
            }
          }
        }
        if (!preCheckRes.ok) {
          benchWindowOpened = false;
          runStopReason =
            preCheckRes.data?.message || `查询设备状态失败（HTTP ${preCheckRes.status}），已停止后续外放`;
          results.push({
            ...commonTurnFields,
            verdict: "failed",
            failure_reason: runStopReason,
            wall_ms: Date.now() - turnStartedAt,
            attribution: null,
            detail: null,
          });
          stopAllBatches = true;
          break;
        }

        const preWindowActive = hasActiveBenchWindowForSession(preDev, boundSessionId);
        const preLastClosed = extractLastBenchWindowClosed(preCheckRes.data, preDev, deviceId);
        if (previousWasExit && !preWindowActive) {
          // 若旧窗已知因非 device_disconnected（如用户手动结束、时间窗到期等）或非绑定会话关闭，绝不唤醒或重开
          if (preLastClosed && !canReopenBenchWindowAfterExit({ lastClosed: preLastClosed, boundSessionId })) {
            benchWindowOpened = false;
            runStopReason = formatBenchWindowStopReason(preLastClosed);
            results.push({
              ...commonTurnFields,
              verdict: "failed",
              failure_reason: runStopReason,
              wall_ms: Date.now() - turnStartedAt,
              attribution: null,
              detail: null,
            });
            stopAllBatches = true;
            break;
          }
          // 设备仍在线但时间窗已无且缺 device_disconnected 关闭记录 -> 停止，不重开
          if (!preLastClosed && preDev) {
            benchWindowOpened = false;
            runStopReason = formatBenchWindowStopReason(null);
            results.push({
              ...commonTurnFields,
              verdict: "failed",
              failure_reason: runStopReason,
              wall_ms: Date.now() - turnStartedAt,
              attribution: null,
              detail: null,
            });
            stopAllBatches = true;
            break;
          }
          // 整轮时间预算已耗尽（剩余 0 分钟）-> 直接结束测试，不再唤醒续开
          const preRemainingMinutes = computeRemainingBenchMinutes({
            firstOpenedAtMs: firstWindowOpenedAtMs,
            maxMinutes,
            nowMs: nowFn(),
          });
          if (preRemainingMinutes <= 0) {
            benchWindowOpened = false;
            runStopReason = formatBenchWindowStopReason(null, { remainingZero: true });
            results.push({
              ...commonTurnFields,
              verdict: "failed",
              failure_reason: runStopReason,
              wall_ms: Date.now() - turnStartedAt,
              attribution: null,
              detail: null,
            });
            stopAllBatches = true;
            break;
          }
        }

        const needTurnWake =
          previousWasExit ||
          (preDev &&
            preWindowActive &&
            (preDev.audio_channel_open === false || preDev.state === "idle"));
        if (needTurnWake) {
          const wasPostExit = previousWasExit;
          const latestWord = String(preDev?.wake_word || preCheckRes.data?.wake_word || "").trim();
          if (latestWord && latestWord !== wakeWord) {
            wakeWord = latestWord;
            wakeResolved = resolveWakeWordAsset({
              wakeWord,
              runtimeDir,
              recordingsDir: resolvedRecordingsDir,
              speaker: speakerFilter,
            });
          }
          const wakeRes = await attemptAutoWake({
            baseUrl,
            token,
            deviceId,
            wakeWord,
            wakeAsset: wakeResolved.asset,
            missingReason: wakeResolved.missingReason,
            trigger: wasPostExit ? "post_exit" : "pre_turn",
            player,
            playWavFn: opts.playWavFn,
            waitFor: "listening",
            wakeTimeoutMs,
            wakeRetries,
            pollIntervalMs,
            onSpawn: (c) => {
              activePlayChild = c;
            },
          });
          wakeEvents.push(wakeRes.event);
          if (!wakeRes.ok || !wakeRes.device) {
            results.push({
              ...commonTurnFields,
              verdict: "failed",
              failure_reason: wakeRes.error || "唤醒失败",
              wall_ms: Date.now() - turnStartedAt,
              attribution: null,
              detail: null,
            });
            stopAllBatches = true;
            break;
          }
          previousWasExit = false;
          // 仅在旧时间窗明确因 device_disconnected 关闭且属于上一轮绑定 session_id、且整轮剩余分钟 > 0 时才重开
          const nextSessionId = wakeRes.sessionId || wakeRes.device.session_id || "";
          if (wasPostExit && nextSessionId && nextSessionId !== boundSessionId) {
            const postCheckRes = await listDebugDevices(baseUrl, token);
            if (!postCheckRes.ok) {
              benchWindowOpened = false;
              runStopReason =
                postCheckRes.data?.message ||
                `自动唤醒后复核时间窗状态失败（HTTP ${postCheckRes.status}），已停止后续外放`;
              results.push({
                ...commonTurnFields,
                verdict: "failed",
                failure_reason: runStopReason,
                wall_ms: Date.now() - turnStartedAt,
                attribution: null,
                detail: null,
              });
              stopAllBatches = true;
              break;
            }
            const postDev =
              (postCheckRes.data?.devices || []).find((d) => d.device_id === deviceId) || wakeRes.device;
            const postLastClosed = extractLastBenchWindowClosed(postCheckRes.data, postDev, deviceId);
            if (!canReopenBenchWindowAfterExit({ lastClosed: postLastClosed, boundSessionId })) {
              benchWindowOpened = false;
              runStopReason = formatBenchWindowStopReason(postLastClosed);
              results.push({
                ...commonTurnFields,
                verdict: "failed",
                failure_reason: runStopReason,
                wall_ms: Date.now() - turnStartedAt,
                attribution: null,
                detail: null,
              });
              stopAllBatches = true;
              break;
            }
            const remainingMinutes = computeRemainingBenchMinutes({
              firstOpenedAtMs: firstWindowOpenedAtMs,
              maxMinutes,
              nowMs: nowFn(),
            });
            if (remainingMinutes <= 0) {
              benchWindowOpened = false;
              runStopReason = formatBenchWindowStopReason(null, { remainingZero: true });
              results.push({
                ...commonTurnFields,
                verdict: "failed",
                failure_reason: runStopReason,
                wall_ms: Date.now() - turnStartedAt,
                attribution: null,
                detail: null,
              });
              stopAllBatches = true;
              break;
            }
            const reopenRes = await controlBenchWindow(baseUrl, token, {
              deviceId,
              action: "open",
              maxMinutes: remainingMinutes,
            });
            if (!reopenRes.ok) {
              benchWindowOpened = false;
              runStopReason =
                reopenRes.data?.message || `自动唤醒后重开测试时间窗失败（HTTP ${reopenRes.status}）`;
              results.push({
                ...commonTurnFields,
                verdict: "failed",
                failure_reason: runStopReason,
                wall_ms: Date.now() - turnStartedAt,
                attribution: null,
                detail: null,
              });
              stopAllBatches = true;
              break;
            }
            benchWindowOpened = true;
            boundSessionId = reopenRes.data?.bench_window?.session_id || nextSessionId;
          }
        }

        const waitQuiet = await waitForQuietListening({
          baseUrl,
          token,
          deviceId,
          quietMs,
          timeoutMs: Math.min(idleWaitMs, 100_000),
          pollIntervalMs,
          allowIdle: false,
          ignoreLevel,
        });

        if (spec.require_idle_laser && waitQuiet.payload?.active_laser_job === true) {
          results.push({
            ...commonTurnFields,
            verdict: "skipped",
            skip_reason: "检测到激光加工任务正在运行，跳过停止快路径测试",
            wall_ms: Date.now() - turnStartedAt,
            attribution: null,
            detail: null,
          });
          continue;
        }

        if (!waitQuiet.ready) {
          results.push({
            ...commonTurnFields,
            verdict: "failed",
            failure_reason: waitQuiet.offline
              ? "设备已断开连接"
              : `等待设备进入 listening 且安静 ${quietMs} ms 超时`,
            wall_ms: Date.now() - turnStartedAt,
            attribution: null,
            detail: null,
          });
          continue;
        }

        if (!hasActiveBenchWindowForSession(waitQuiet.device, boundSessionId)) {
          benchWindowOpened = false;
          const closedInfo = extractLastBenchWindowClosed(waitQuiet.payload, waitQuiet.device, deviceId);
          runStopReason = formatBenchWindowStopReason(closedInfo);
          results.push({
            ...commonTurnFields,
            verdict: "failed",
            failure_reason: runStopReason,
            wall_ms: Date.now() - turnStartedAt,
            attribution: null,
            detail: null,
          });
          stopAllBatches = true;
          break;
        }

        const prevTurn = Number(waitQuiet.device?.latest_turn || 0);
        const playback = await playWav(asset.wavPath, {
          player,
          playWavFn: opts.playWavFn,
          onSpawn: (c) => {
            activePlayChild = c;
          },
          spec,
          asset,
          index: turnIndex,
          batch: batchIdx + 1,
        });

        const waitDone = await waitForAcousticTurnCompletion({
          baseUrl,
          token,
          deviceId,
          sessionId: waitQuiet.device?.session_id || boundSessionId,
          prevTurnNumber: prevTurn,
          playbackEndedAtMs: playback.ended_at_ms,
          quietMs,
          timeoutMs: turnTimeoutMs,
          pollIntervalMs,
          expectExit: spec.expect_route === "fast_exit",
          ignoreLevel,
        });

        const wallMs = Date.now() - turnStartedAt;
        if (waitDone.timedOut) {
          results.push({
            ...commonTurnFields,
            turn_id: waitDone.turnId,
            verdict: "failed",
            failure_reason: `本轮在 ${turnTimeoutMs} ms 内未完成并回到安静聆听（超时）`,
            wall_ms: wallMs,
            attribution: waitDone.detail
              ? buildTurnAttribution({
                  spec,
                  playback,
                  detail: waitDone.detail,
                  observedPhases: waitDone.observedPhases,
                  overlapDetected: waitDone.overlapDetected,
                })
              : null,
            detail: waitDone.detail,
          });
          continue;
        }

        // VC3 #7 + VE1：自动回写含 audio_origin / dataset / license 的 .label.json
        const labeled = await tryAutoLabelAsrCorpus({
          runtimeDir,
          detail: waitDone.detail,
          spec,
          asset,
          noisePreset,
        });
        if (labeled) autoLabeledCount += 1;

        // VC2 可选数字注入对照（退出词跳过，否则会话已关闭）
        let digitalDetail = null;
        let digitalError = null;
        let benchWindowLostDuringDigital = false;
        if (compareDigital && spec.expect_route !== "fast_exit" && waitDone.detail?.route !== "fast_exit") {
          const digitalRes = await runDigitalControlTurn({
            baseUrl,
            token,
            deviceId,
            wavPath: asset.wavPath,
            turnTimeoutMs,
            pollIntervalMs,
            quietMs,
            ignoreLevel,
          });
          if (digitalRes.detail) digitalDetail = digitalRes.detail;
          if (digitalRes.error) {
            digitalError = digitalRes.error;
            if (digitalRes.code === "bench_window_required" || digitalRes.status === 403) {
              benchWindowLostDuringDigital = true;
            }
          }
        }

        const attribution = buildTurnAttribution({
          spec,
          playback,
          detail: waitDone.detail,
          digitalDetail,
          observedPhases: waitDone.observedPhases,
          overlapDetected: waitDone.overlapDetected,
        });
        if (digitalError) {
          attribution.asr_correction.digital_control_error = digitalError;
        }
        if (waitDone.extraTurnsCount > 0) {
          attribution.capture_endpoint.extra_turns_triggered = waitDone.extraTurnsCount;
        }
        const stageFailures = [];
        if (benchWindowLostDuringDigital) {
          stageFailures.push(`数字对照检测到测试时间窗已关闭（${digitalError}），已中止后续外放`);
        }
        if (!attribution.capture_endpoint.passed) {
          const flags = [];
          if (attribution.capture_endpoint.overlap_detected) flags.push("外放与播报重叠");
          if (attribution.capture_endpoint.early_cutoff) flags.push("抢话");
          if (attribution.capture_endpoint.head_clipped) flags.push("首字被吞");
          if (!attribution.asr_correction.raw_asr) flags.push("未识别出文字");
          stageFailures.push(`拾音判停异常（${flags.join("、") || "未通过"}）`);
        }
        if (!attribution.asr_correction.passed) {
          stageFailures.push(
            `识别纠偏未达标（纠偏后 CER ${(attribution.asr_correction.corrected_cer * 100).toFixed(1)}%${
              attribution.asr_correction.key_terms_missed.length
                ? `，漏认：${attribution.asr_correction.key_terms_missed.join("、")}`
                : ""
            }）`,
          );
        }
        if (!attribution.intent_tools.passed) {
          const reasons = [];
          if (!attribution.intent_tools.route_matched) {
            reasons.push(`路由期望 ${attribution.intent_tools.expect_route} 实际 ${attribution.intent_tools.route}`);
          }
          if (!attribution.intent_tools.tool_matched) {
            reasons.push(`未命中工具 ${attribution.intent_tools.expect_tool}`);
          }
          if (!attribution.intent_tools.gate_matched) reasons.push("模拟门禁未触发拒绝");
          if (attribution.intent_tools.error) reasons.push(attribution.intent_tools.error);
          stageFailures.push(`意图工具异常（${reasons.join("；") || "未通过"}）`);
        }
        if (!attribution.tts_playback.passed) {
          stageFailures.push(
            `合成播放异常（卡顿 ${attribution.tts_playback.stall_ms} ms，帧数 ${attribution.tts_playback.frames}）`,
          );
        }

        results.push({
          ...commonTurnFields,
          turn_id: waitDone.turnId,
          verdict: stageFailures.length === 0 ? "passed" : "failed",
          failure_reason: stageFailures.length ? stageFailures.join("；") : undefined,
          wall_ms: wallMs,
          auto_labeled: labeled,
          attribution,
          detail: waitDone.detail,
        });
        const turnExited =
          spec.expect_route === "fast_exit" ||
          spec.id === "fast_exit" ||
          waitDone.detail?.route === "fast_exit" ||
          (Array.isArray(waitDone.detail?.tools) &&
            waitDone.detail.tools.some((t) => t?.name === "end_conversation"));
        if (turnExited) {
          previousWasExit = true;
        }
        if (benchWindowLostDuringDigital) {
          stopAllBatches = true;
          break;
        }
      }

      // 批结束正常关闭本批测试时间窗（下一批将先复核会话再重开新时间窗）
      if (benchWindowOpened) {
        try {
          await controlBenchWindow(baseUrl, token, { deviceId, action: "close" });
        } finally {
          benchWindowOpened = false;
        }
      }
    }
  } finally {
    unregisterSignals();
    await cleanup();
  }

  // 6. 归档到 `<runtime>/voice-bench/e2e-<run-id>/`（全量脱敏，不复制用户录音）
  const finishedAtMs = Date.now();
  const redactedTurns = results.map((row) => redactValue(row, redactOpts));
  const batchPlanSummary = {
    estimated_duration_ms: batchSchedule.totalEstimatedMs,
    window_limit_ms: batchSchedule.windowLimitMs,
    max_minutes: batchSchedule.maxMinutes,
    exceeds_window: batchSchedule.exceedsWindow,
    batch_count: batchSchedule.batches.length,
    batches_executed: batchesExecuted,
    session_aborted: sessionAborted,
  };
  const summary = redactValue(
    buildE2eSummary({
      runId,
      deviceId,
      corpusName,
      noisePreset,
      noiseGain,
      compareDigital,
      speakerFilter,
      includeDeviceCorpus,
      maxOnline,
      onlineRotation,
      batchPlan: batchPlanSummary,
      missingTurns,
      skippedAssets,
      wakeWord,
      wakeEvents,
      stopReason: runStopReason,
      startedAtMs,
      finishedAtMs,
      calibration: calibrationReport,
      autoLabeledCount,
      turns: redactedTurns,
    }),
    redactOpts,
  );
  const reportMd = redactText(buildE2eReportMarkdown(summary, redactedTurns), redactOpts);
  const audioManifest = redactValue(
    {
      note: "仅记录外放真人录音的文件名、说话人标签、来源类型与公开数据集许可信息，绝不复制用户原始录音文件。",
      wake_word: wakeWord,
      wake_asset: wakeResolved.asset
        ? {
            filename: wakeResolved.asset.filename,
            source_type: wakeResolved.asset.source_type,
            audio_origin: wakeResolved.asset.audio_origin || wakeResolved.asset.source_type,
            speaker: wakeResolved.asset.speaker || null,
            dataset: wakeResolved.asset.dataset || null,
            license: wakeResolved.asset.license || null,
            source_url: wakeResolved.asset.source_url || null,
            duration_ms: wakeResolved.asset.duration_ms,
          }
        : null,
      wake_events: wakeEvents,
      noise: noiseInfo,
      calibration: assets.calibrationAsset
        ? {
            filename: assets.calibrationAsset.filename,
            source_type: assets.calibrationAsset.source_type,
            audio_origin: assets.calibrationAsset.audio_origin || assets.calibrationAsset.source_type,
            speaker: assets.calibrationAsset.speaker || null,
            dataset: assets.calibrationAsset.dataset || null,
            license: assets.calibrationAsset.license || null,
            source_url: assets.calibrationAsset.source_url || null,
            secondary_recording: Boolean(assets.calibrationAsset.secondary_recording),
            voice_profile: assets.calibrationAsset.voice_profile,
            duration_ms: assets.calibrationAsset.duration_ms,
          }
        : null,
      items: assets.preparedTurns.map(({ spec, asset }, idx) => ({
        index: idx + 1,
        id: spec.id || `turn_${idx + 1}`,
        filename: asset.filename,
        source_type: asset.source_type,
        audio_origin: asset.audio_origin || asset.source_type,
        speaker: asset.speaker || null,
        dataset: asset.dataset || null,
        license: asset.license || null,
        source_url: asset.source_url || null,
        secondary_recording: Boolean(asset.secondary_recording),
        voice_profile: asset.voice_profile,
        duration_ms: asset.duration_ms,
      })),
      missing_turns: missingTurns,
      skipped_assets: skippedAssets,
    },
    redactOpts,
  );

  const rawTimeline = sliceProcessTrace(traceDir, startedAtMs, finishedAtMs, redactOpts);
  const { jsonlLines, txtLines } = annotatePlaybackTraceTimeline(
    rawTimeline.jsonlLines,
    rawTimeline.txtLines,
    redactedTurns,
    redactOpts,
  );
  mkdirSync(archiveDir, { recursive: true });
  writeFileSync(
    join(archiveDir, "turns.jsonl"),
    redactedTurns.map((t) => JSON.stringify(t)).join("\n") + (redactedTurns.length ? "\n" : ""),
    "utf8",
  );
  writeTimelineFiles(archiveDir, jsonlLines, txtLines);
  writeFileSync(join(archiveDir, "report.md"), reportMd, "utf8");
  writeFileSync(join(archiveDir, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`, "utf8");
  writeFileSync(
    join(archiveDir, "audio-manifest.json"),
    `${JSON.stringify(audioManifest, null, 2)}\n`,
    "utf8",
  );

  const exitCode = summary.counts.failed > 0 || sessionAborted || Boolean(runStopReason) ? 1 : 0;
  return { exitCode, archiveDir, summary, turns: redactedTurns, audioManifest, errorMessage: runStopReason || null };
}

export function parseE2eArgs(argv) {
  const opts = {
    corpus: null,
    device: null,
    speaker: "",
    recordingsDir: null,
    includeDeviceCorpus: false,
    maxOnline: DEFAULT_MAX_ONLINE,
    runtimeDir: null,
    url: null,
    tokenFile: null,
    outDir: null,
    maxMinutes: DEFAULT_MAX_MINUTES,
    turnTimeoutMs: DEFAULT_TURN_TIMEOUT_MS,
    pollIntervalMs: DEFAULT_POLL_INTERVAL_MS,
    quietMs: DEFAULT_QUIET_MS,
    wakeTimeoutMs: DEFAULT_WAKE_TIMEOUT_MS,
    wakeRetries: DEFAULT_WAKE_RETRIES,
    noise: null,
    noiseGain: 0.22,
    compareDigital: false,
    skipCalibration: false,
    calibrationText: DEFAULT_CALIBRATION_TEXT,
    pythonBin: "python3",
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--corpus") opts.corpus = argv[++i];
    else if (a === "--device") opts.device = argv[++i];
    else if (a === "--speaker") opts.speaker = argv[++i] || "";
    else if (a === "--recordings-dir") opts.recordingsDir = argv[++i];
    else if (a === "--include-device-corpus") opts.includeDeviceCorpus = true;
    else if (a === "--max-online") opts.maxOnline = Number(argv[++i]);
    else if (a === "--runtime-dir") opts.runtimeDir = argv[++i];
    else if (a === "--url") opts.url = argv[++i];
    else if (a === "--token-file") opts.tokenFile = argv[++i];
    else if (a === "--out-dir") opts.outDir = argv[++i];
    else if (a === "--max-minutes") opts.maxMinutes = Number(argv[++i]);
    else if (a === "--turn-timeout-ms") opts.turnTimeoutMs = Number(argv[++i]);
    else if (a === "--poll-interval-ms") opts.pollIntervalMs = Number(argv[++i]);
    else if (a === "--quiet-ms") opts.quietMs = Number(argv[++i]);
    else if (a === "--wake-timeout-ms") opts.wakeTimeoutMs = Number(argv[++i]);
    else if (a === "--wake-retries") opts.wakeRetries = Number(argv[++i]);
    else if (a === "--noise") opts.noise = argv[++i];
    else if (a === "--noise-gain") opts.noiseGain = Number(argv[++i]);
    else if (a === "--compare-digital") opts.compareDigital = true;
    else if (a === "--skip-calibration") opts.skipCalibration = true;
    else if (a === "--calibration-text") opts.calibrationText = argv[++i];
    else if (a === "--python") opts.pythonBin = argv[++i];
    else if (a === "--help" || a === "-h") opts.help = true;
    else throw new Error(`未知参数：${a}`);
  }
  if (!Number.isInteger(opts.maxOnline) || opts.maxOnline < 0) {
    throw new Error("--max-online 须为大于等于 0 的整数");
  }
  if (!Number.isFinite(opts.maxMinutes) || opts.maxMinutes < 1 || opts.maxMinutes > 60) {
    throw new Error("--max-minutes 须为 1 ~ 60 分钟");
  }
  if (opts.noise && !["fan", "pump", "burst"].includes(String(opts.noise).toLowerCase())) {
    throw new Error("--noise 仅支持 fan、pump 或 burst");
  }
  if (!Number.isFinite(opts.noiseGain) || opts.noiseGain <= 0 || opts.noiseGain > 1) {
    throw new Error("--noise-gain 须在 (0, 1] 之间");
  }
  return opts;
}

if (process.argv[1] && resolve(fileURLToPath(import.meta.url)) === resolve(process.argv[1])) {
  try {
    const opts = parseE2eArgs(process.argv.slice(2));
    if (opts.help) {
      console.log(
        "用法: node scripts/voice-bench/xiaozhi-e2e.mjs [--corpus <path>] [--device <id>] [--speaker <标签>] [--recordings-dir <path>] [--max-online 20] [--include-device-corpus] [--max-minutes 30] [--noise fan|pump|burst] [--noise-gain 0.22] [--compare-digital] [--runtime-dir <path>] [--url http://127.0.0.1:<port>] [--out-dir <path>]",
      );
      process.exit(0);
    }
    const result = await runXiaozhiE2e(opts);
    if (result.errorMessage) {
      if (result.archiveDir) {
        console.log(result.archiveDir);
      }
      console.error(redactText(result.errorMessage, { paths: [homedir(), process.cwd()] }));
      process.exit(result.exitCode || 1);
    }
    console.log(result.archiveDir);
    if (result.exitCode !== 0) {
      console.error(
        `整体测试完成但存在失败或未达标轮次：通过 ${result.summary.counts.passed} / 失败 ${result.summary.counts.failed} / 跳过 ${result.summary.counts.skipped}`,
      );
      process.exit(result.exitCode);
    }
  } catch (error) {
    console.error(redactText(error?.message || String(error), { paths: [homedir(), process.cwd()] }));
    process.exit(1);
  }
}
