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

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
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
  RECORDING_APP_HINT,
  checkPlayerAvailable,
  playWav,
  prepareVoiceAssets,
  startBackgroundNoise,
} from "./voice-assets.mjs";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
export const DEFAULT_E2E_CORPUS_PATH = join(SCRIPT_DIR, "e2e-corpus.json");
export const DEFAULT_CALIBRATION_TEXT = "激光机现在什么状态";
export const DEFAULT_QUIET_MS = 1_000;
export const DEFAULT_MAX_MINUTES = 30;
export const EARLY_CUTOFF_TOLERANCE_MS = 50;
export const DEFAULT_MAX_CER_THRESHOLD = 0.35;

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
  const capturePassed = Boolean(detail) && rawAsr.length > 0 && !earlyCutoff && !headClipped && !overlapDetected;

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
  const firstAudioFrameAtMs =
    typeof detail?.first_audio_frame_at_ms === "number" ? detail.first_audio_frame_at_ms : null;
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
      overlap_detected: overlapDetected,
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

function buildSpeakerStats(turns) {
  const bySpeaker = new Map();
  for (const t of turns) {
    const speaker = String(t.speaker || t.audio_source?.speaker || "未标注说话人");
    if (!bySpeaker.has(speaker)) {
      bySpeaker.set(speaker, {
        speaker,
        secondary_recording: Boolean(t.secondary_recording || t.audio_source?.secondary_recording),
        rows: [],
      });
    }
    const group = bySpeaker.get(speaker);
    if (t.secondary_recording || t.audio_source?.secondary_recording) {
      group.secondary_recording = true;
    }
    group.rows.push(t);
  }
  const rate = (num, den) => (den > 0 ? Number((num / den).toFixed(4)) : null);
  const out = [];
  for (const { speaker, secondary_recording, rows } of bySpeaker.values()) {
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
      secondary_recording,
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
  missingTurns = [],
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

  return {
    run_id: runId,
    device_id: deviceId,
    corpus: corpusName,
    speaker_filter: speakerFilter || null,
    include_device_corpus: Boolean(includeDeviceCorpus),
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
    by_speaker: bySpeaker,
    missing_turns: missingTurns,
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

  if (Array.isArray(summary.by_speaker) && summary.by_speaker.length > 0) {
    lines.push(
      "",
      "## 按说话人分组统计",
      "",
      "| 说话人 | 来源类型 | 通过/执行 | 拾音判停 | 识别纠偏 | 意图工具 | 合成播放 | 原 CER | 纠偏 CER | 首帧 P50 |",
      "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    );
    for (const sp of summary.by_speaker) {
      const sourceLabel = sp.secondary_recording ? "设备录音（二次录制）" : "电脑麦克风真人录音";
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

function isQuietListening(dev, allowIdle = false) {
  if (!dev) return false;
  if (dev.state === "listening" && (Number(dev.level) || 0) === 0) return true;
  if (allowIdle && dev.state === "idle" && (Number(dev.level) || 0) === 0) return true;
  return false;
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
      if (isQuietListening(latestDevice, allowIdle)) {
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
}) {
  const deadline = Date.now() + timeoutMs;
  const observedPhases = [];
  let matchedTurnId = null;
  let detail = null;
  let quietSince = null;
  let maxObservedLevel = 0;
  let speakingStartedBeforePlaybackEnd = false;

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
      if (!matchedTurnId && Number(dev.latest_turn || 0) > prevTurnNumber) {
        matchedTurnId = dev.latest_turn_id || `${dev.session_id || sessionId}:${dev.latest_turn}`;
      }
    }

    if (!matchedTurnId) {
      const turnsRes = await listDeviceTurns(baseUrl, token, deviceId, 5);
      if (turnsRes.ok && Array.isArray(turnsRes.data?.turns)) {
        const newer = turnsRes.data.turns.find(
          (t) =>
            Number(t.turn || 0) > prevTurnNumber &&
            (!sessionId || t.session_id === sessionId || !dev),
        );
        if (newer?.turn_id) {
          matchedTurnId = newer.turn_id;
          detail = newer;
        }
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
      const isExit = expectExit || detail.route === "fast_exit" || !dev;
      if (isExit && (!dev || dev.state === "idle" || dev.state === "listening")) {
        return {
          timedOut: false,
          turnId: matchedTurnId,
          detail,
          observedPhases,
          maxObservedLevel,
          overlapDetected: speakingStartedBeforePlaybackEnd,
        };
      }
      if (dev && isQuietListening(dev, detail.route === "fast_exit")) {
        if (quietSince === null) quietSince = Date.now();
        if (Date.now() - quietSince >= quietMs) {
          return {
            timedOut: false,
            turnId: matchedTurnId,
            detail,
            observedPhases,
            maxObservedLevel,
            overlapDetected: speakingStartedBeforePlaybackEnd,
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
  };
}

/** VC3 #7：外放轮次若已落盘到 `<runtime>/asr-corpus/<date>/<turn-id>.wav`，自动写出 `.label.json`。 */
async function tryAutoLabelAsrCorpus({ runtimeDir, detail, spec, asset, noisePreset }) {
  if (!detail?.corpus_date || !detail?.corpus_turn_id) return false;
  const dayDir = join(runtimeDir, "asr-corpus", detail.corpus_date);
  const wavPath = join(dayDir, `${detail.corpus_turn_id}.wav`);
  const metaPath = join(dayDir, `${detail.corpus_turn_id}.json`);
  const deadline = Date.now() + 1_500;
  while (Date.now() <= deadline && (!existsSync(wavPath) || !existsSync(metaPath))) {
    await sleep(50);
  }
  if (!existsSync(wavPath) || !existsSync(metaPath)) return false;

  const rawAsr = String(detail.injected_text || "").trim();
  const cerRes = computeCer(spec.text, rawAsr);
  const validTags = new Set(["命令词", "数字", "材料名", "有噪声"]);
  const tags = [
    ...(Array.isArray(spec.tags) ? spec.tags : []),
    ...(noisePreset ? ["有噪声"] : []),
  ].filter((t, idx, arr) => validTags.has(t) && arr.indexOf(t) === idx);

  const label = {
    turn_id: detail.corpus_turn_id,
    date: detail.corpus_date,
    reference_text: spec.text,
    verified_correct: cerRes.edits === 0,
    tags,
    updated_at_ms: Date.now(),
    label_source: "e2e-playback",
    voice_profile: {
      ...(asset?.voice_profile || { source: asset?.source_type || "human-recording" }),
      ...(asset?.speaker ? { speaker: asset.speaker } : {}),
      ...(asset?.secondary_recording ? { secondary_recording: true } : {}),
      ...(noisePreset ? { noise: noisePreset } : {}),
    },
  };
  writeFileSync(
    join(dayDir, `${detail.corpus_turn_id}.label.json`),
    `${JSON.stringify(label, null, 2)}\n`,
    "utf8",
  );
  return true;
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
}) {
  const ready = await waitForQuietListening({
    baseUrl,
    token,
    deviceId,
    quietMs,
    timeoutMs: Math.min(30_000, turnTimeoutMs),
    pollIntervalMs,
    allowIdle: true,
  });
  if (!ready.ready) {
    return { error: "数字注入前等待设备空闲超时" };
  }
  const { pcm, sampleRate } = readWavPcm16(wavPath);
  const injectRes = await debugFetch(
    baseUrl,
    token,
    `/debug/v1/devices/${encodeURIComponent(deviceId)}/inject-audio`,
    {
      method: "POST",
      body: {
        pcm_base64: pcm.toString("base64"),
        sample_rate: sampleRate,
        frame_ms: 60,
      },
      timeoutMs: 45_000,
    },
  );
  if (!injectRes.ok || !injectRes.data?.turn_id) {
    return { error: injectRes.data?.message || `数字音频注入失败（HTTP ${injectRes.status}）` };
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
          (detail.device_state === "listening" && (Number(detail.device_level) || 0) === 0);
        if (devIdle) return { detail };
      }
    }
    await sleep(pollIntervalMs);
  }
  return { detail, error: "数字对照轮次超时" };
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
 * VC3 真小智整体测试主入口（支持注入假播放器、假素材准备器和假噪声器供单测验证）。
 */
export async function runXiaozhiE2e(opts = {}) {
  const turnTimeoutMs = Number(opts.turnTimeoutMs ?? DEFAULT_TURN_TIMEOUT_MS);
  const pollIntervalMs = Number(opts.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS);
  const idleWaitMs = Number(opts.idleWaitMs ?? DEFAULT_IDLE_WAIT_MS);
  const quietMs = Number(opts.quietMs ?? DEFAULT_QUIET_MS);
  const maxMinutes = Math.max(1, Math.min(60, Number(opts.maxMinutes ?? DEFAULT_MAX_MINUTES)));
  const calibrationText = String(opts.calibrationText || DEFAULT_CALIBRATION_TEXT);
  const noisePreset = opts.noise ? String(opts.noise).trim().toLowerCase() : null;
  const noiseGain = Number(opts.noiseGain ?? 0.22);
  const compareDigital = Boolean(opts.compareDigital);

  const corpusPath = resolve(opts.corpus || DEFAULT_E2E_CORPUS_PATH);
  const corpusRaw = JSON.parse(readFileSync(corpusPath, "utf8"));
  const corpusName = String(corpusRaw?.name || "custom-e2e");
  const corpusTurns = Array.isArray(corpusRaw)
    ? corpusRaw
    : Array.isArray(corpusRaw?.turns)
      ? corpusRaw.turns
      : [];
  if (corpusTurns.length === 0) {
    throw new Error(`语料文件不含有效轮次：${corpusPath}`);
  }

  // 1. 预检：读取令牌、查在线设备、检查播放器
  const { runtimeDir, token, baseUrl } = resolveDebugConnection(opts);
  const redactOpts = {
    secrets: [token],
    paths: [runtimeDir, homedir(), process.cwd(), dirname(corpusPath)],
  };

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
  const sessionId = targetDevice.session_id || "";

  let player = null;
  if (typeof opts.playWavFn !== "function") {
    player =
      typeof opts.checkPlayerFn === "function"
        ? opts.checkPlayerFn()
        : checkPlayerAvailable(opts.platform || process.platform);
  }

  const speakerFilter = String(opts.speaker || "").trim();
  const includeDeviceCorpus = Boolean(opts.includeDeviceCorpus);
  const assetPreparer = opts.prepareVoiceAssetsFn || prepareVoiceAssets;
  const assets = await assetPreparer({
    turns: corpusTurns,
    calibrationText: opts.skipCalibration ? "" : calibrationText,
    runtimeDir,
    recordingsDir: opts.recordingsDir || join(runtimeDir, BENCH_RECORDINGS_DIR_REL),
    corpusDir: opts.corpusDir || join(runtimeDir, "asr-corpus"),
    speaker: speakerFilter,
    includeDeviceCorpus,
  });

  const missingTurns = Array.isArray(assets.missingTurns) ? assets.missingTurns : [];
  if (!Array.isArray(assets.preparedTurns) || assets.preparedTurns.length === 0) {
    const missingList = missingTurns.map((m) => `- ${m.id}（${m.label}）：${m.text}`).join("\n");
    const speakerHint = speakerFilter ? `（已指定说话人「${speakerFilter}」）` : "";
    return {
      exitCode: 1,
      archiveDir: null,
      summary: null,
      turns: [],
      missingTurns,
      errorMessage: [
        `未找到任何可用的真人测试录音${speakerHint}，已终止整体测试（绝不使用合成声音替代）。`,
        RECORDING_APP_HINT,
        missingList ? `缺少真人录音的语料：\n${missingList}` : "",
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
  const effectiveCalibrationText = assets.calibrationEffectiveText || calibrationText;

  // 2. 打开测试时间窗 + 打开过程记录（全程包裹在 try/finally，任何异常都必关窗与恢复）
  const runId = opts.runId || formatRunId("e2e");
  const startedAtMs = Date.now();
  let benchWindowOpened = false;
  let previousTraceEnabled = null;
  let noiseHandle = null;
  let calibrationReport = null;
  let autoLabeledCount = 0;
  const results = [];

  try {
    const winRes = await controlBenchWindow(baseUrl, token, {
      deviceId,
      action: "open",
      maxMinutes,
    });
    if (!winRes.ok) {
      return {
        exitCode: 1,
        archiveDir: null,
        summary: null,
        turns: [],
        errorMessage: winRes.data?.message || `打开测试时间窗失败（HTTP ${winRes.status}）`,
      };
    }
    benchWindowOpened = true;

    const traceRes = await setProcessTrace(baseUrl, token, true, false);
    if (traceRes.ok && typeof traceRes.data?.previous_enabled === "boolean") {
      previousTraceEnabled = traceRes.data.previous_enabled;
    }

    // 3. 校准（在已锁定的测试时间窗内执行）
    if (!opts.skipCalibration && assets.calibrationAsset) {
      const calReady = await waitForQuietListening({
        baseUrl,
        token,
        deviceId,
        quietMs,
        timeoutMs: idleWaitMs,
        pollIntervalMs,
        allowIdle: true,
      });
      if (!calReady.ready) {
        return {
          exitCode: 1,
          archiveDir: null,
          summary: null,
          turns: [],
          errorMessage: calReady.offline
            ? `校准前设备 ${deviceId} 断开了连接`
            : "校准前等待设备进入安静聆听超时，请确认小智已唤醒并处于聆听状态",
        };
      }
      const prevTurn = Number(calReady.device?.latest_turn || 0);
      const calPlay = await playWav(assets.calibrationAsset.wavPath, {
        player,
        playWavFn: opts.playWavFn,
        spec: { id: "calibration", text: effectiveCalibrationText },
      });
      const calWait = await waitForAcousticTurnCompletion({
        baseUrl,
        token,
        deviceId,
        sessionId: calReady.device?.session_id || sessionId,
        prevTurnNumber: prevTurn,
        playbackEndedAtMs: calPlay.ended_at_ms,
        quietMs,
        timeoutMs: Math.min(30_000, turnTimeoutMs),
        pollIntervalMs,
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
    }

    // 5. 严格串行逐句执行
    for (let i = 0; i < assets.preparedTurns.length; i += 1) {
      const { spec, asset } = assets.preparedTurns[i];
      const turnIndex = i + 1;
      const turnIdLabel = String(spec.id || `turn_${turnIndex}`);
      const turnStartedAt = Date.now();

      const waitQuiet = await waitForQuietListening({
        baseUrl,
        token,
        deviceId,
        quietMs,
        timeoutMs: Math.min(idleWaitMs, 100_000),
        pollIntervalMs,
        allowIdle: false,
      });

      const turnSpeaker = asset.speaker || asset.voice_profile?.speaker || "未标注说话人";
      const turnSecondary = Boolean(asset.secondary_recording || asset.voice_profile?.secondary_recording);
      const audioSourceMeta = {
        filename: asset.filename,
        source_type: asset.source_type,
        speaker: turnSpeaker,
        secondary_recording: turnSecondary,
        voice_profile: asset.voice_profile,
        duration_ms: asset.duration_ms,
      };

      if (spec.require_idle_laser && waitQuiet.payload?.active_laser_job === true) {
        results.push({
          index: turnIndex,
          id: turnIdLabel,
          label: spec.label || turnIdLabel,
          text: spec.text,
          speaker: turnSpeaker,
          secondary_recording: turnSecondary,
          verdict: "skipped",
          skip_reason: "检测到激光加工任务正在运行，跳过停止快路径测试",
          wall_ms: Date.now() - turnStartedAt,
          audio_source: audioSourceMeta,
          attribution: null,
          detail: null,
        });
        continue;
      }

      if (!waitQuiet.ready) {
        results.push({
          index: turnIndex,
          id: turnIdLabel,
          label: spec.label || turnIdLabel,
          text: spec.text,
          speaker: turnSpeaker,
          secondary_recording: turnSecondary,
          verdict: "failed",
          failure_reason: waitQuiet.offline
            ? "设备已断开连接"
            : `等待设备进入 listening 且安静 ${quietMs} ms 超时`,
          wall_ms: Date.now() - turnStartedAt,
          audio_source: audioSourceMeta,
          attribution: null,
          detail: null,
        });
        continue;
      }

      const prevTurn = Number(waitQuiet.device?.latest_turn || 0);
      const playback = await playWav(asset.wavPath, {
        player,
        playWavFn: opts.playWavFn,
        spec,
        asset,
        index: turnIndex,
      });

      const waitDone = await waitForAcousticTurnCompletion({
        baseUrl,
        token,
        deviceId,
        sessionId: waitQuiet.device?.session_id || sessionId,
        prevTurnNumber: prevTurn,
        playbackEndedAtMs: playback.ended_at_ms,
        quietMs,
        timeoutMs: turnTimeoutMs,
        pollIntervalMs,
        expectExit: spec.expect_route === "fast_exit",
      });

      const wallMs = Date.now() - turnStartedAt;
      if (waitDone.timedOut) {
        results.push({
          index: turnIndex,
          id: turnIdLabel,
          label: spec.label || turnIdLabel,
          text: spec.text,
          speaker: turnSpeaker,
          secondary_recording: turnSecondary,
          turn_id: waitDone.turnId,
          verdict: "failed",
          failure_reason: `本轮在 ${turnTimeoutMs} ms 内未完成并回到安静聆听（超时）`,
          wall_ms: wallMs,
          audio_source: audioSourceMeta,
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

      // VC3 #7：自动回写 asr-corpus 标注
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
      if (compareDigital && spec.expect_route !== "fast_exit" && waitDone.detail?.route !== "fast_exit") {
        const digitalRes = await runDigitalControlTurn({
          baseUrl,
          token,
          deviceId,
          wavPath: asset.wavPath,
          turnTimeoutMs,
          pollIntervalMs,
          quietMs,
        });
        if (digitalRes.detail) digitalDetail = digitalRes.detail;
      }

      const attribution = buildTurnAttribution({
        spec,
        playback,
        detail: waitDone.detail,
        digitalDetail,
        observedPhases: waitDone.observedPhases,
        overlapDetected: waitDone.overlapDetected,
      });
      const stageFailures = [];
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
        index: turnIndex,
        id: turnIdLabel,
        label: spec.label || turnIdLabel,
        text: spec.text,
        speaker: turnSpeaker,
        secondary_recording: turnSecondary,
        turn_id: waitDone.turnId,
        verdict: stageFailures.length === 0 ? "passed" : "failed",
        failure_reason: stageFailures.length ? stageFailures.join("；") : undefined,
        wall_ms: wallMs,
        auto_labeled: labeled,
        audio_source: audioSourceMeta,
        attribution,
        detail: waitDone.detail,
      });
    }
  } finally {
    if (noiseHandle) {
      try {
        noiseHandle.stop();
      } catch {
        // 忽略噪声进程停止异常
      }
    }
    if (benchWindowOpened) {
      try {
        await controlBenchWindow(baseUrl, token, { deviceId, action: "close" });
      } catch {
        // 尽力关闭测试时间窗
      }
    }
    if (previousTraceEnabled !== null) {
      try {
        await setProcessTrace(baseUrl, token, previousTraceEnabled, true);
      } catch {
        // 尽力恢复过程记录开关
      }
    }
  }

  // 6. 归档到 `<runtime>/voice-bench/e2e-<run-id>/`（全量脱敏，不复制用户录音）
  const finishedAtMs = Date.now();
  const redactedTurns = results.map((row) => redactValue(row, redactOpts));
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
      missingTurns,
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
      note: "仅记录外放真人录音的文件名、说话人标签与来源类型，绝不复制用户原始录音文件。",
      noise: noiseHandle
        ? { filename: noiseHandle.filename, preset: noiseHandle.preset, gain: noiseHandle.gain }
        : null,
      calibration: assets.calibrationAsset
        ? {
            filename: assets.calibrationAsset.filename,
            source_type: assets.calibrationAsset.source_type,
            speaker: assets.calibrationAsset.speaker || null,
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
        speaker: asset.speaker || null,
        secondary_recording: Boolean(asset.secondary_recording),
        voice_profile: asset.voice_profile,
        duration_ms: asset.duration_ms,
      })),
      missing_turns: missingTurns,
    },
    redactOpts,
  );

  const traceDir = opts.traceDir || resolveProcessTraceDir(runtimeDir);
  const { jsonlLines, txtLines } = sliceProcessTrace(traceDir, startedAtMs, finishedAtMs, redactOpts);
  const archiveDir = opts.outDir ? resolve(opts.outDir) : join(runtimeDir, "voice-bench", runId);
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

  const exitCode = summary.counts.failed > 0 ? 1 : 0;
  return { exitCode, archiveDir, summary, turns: redactedTurns, audioManifest, errorMessage: null };
}

export function parseE2eArgs(argv) {
  const opts = {
    corpus: null,
    device: null,
    speaker: "",
    recordingsDir: null,
    includeDeviceCorpus: false,
    runtimeDir: null,
    url: null,
    tokenFile: null,
    outDir: null,
    maxMinutes: DEFAULT_MAX_MINUTES,
    turnTimeoutMs: DEFAULT_TURN_TIMEOUT_MS,
    pollIntervalMs: DEFAULT_POLL_INTERVAL_MS,
    quietMs: DEFAULT_QUIET_MS,
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
    else if (a === "--runtime-dir") opts.runtimeDir = argv[++i];
    else if (a === "--url") opts.url = argv[++i];
    else if (a === "--token-file") opts.tokenFile = argv[++i];
    else if (a === "--out-dir") opts.outDir = argv[++i];
    else if (a === "--max-minutes") opts.maxMinutes = Number(argv[++i]);
    else if (a === "--turn-timeout-ms") opts.turnTimeoutMs = Number(argv[++i]);
    else if (a === "--poll-interval-ms") opts.pollIntervalMs = Number(argv[++i]);
    else if (a === "--quiet-ms") opts.quietMs = Number(argv[++i]);
    else if (a === "--noise") opts.noise = argv[++i];
    else if (a === "--noise-gain") opts.noiseGain = Number(argv[++i]);
    else if (a === "--compare-digital") opts.compareDigital = true;
    else if (a === "--skip-calibration") opts.skipCalibration = true;
    else if (a === "--calibration-text") opts.calibrationText = argv[++i];
    else if (a === "--python") opts.pythonBin = argv[++i];
    else if (a === "--help" || a === "-h") opts.help = true;
    else throw new Error(`未知参数：${a}`);
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
        "用法: node scripts/voice-bench/xiaozhi-e2e.mjs [--corpus <path>] [--device <id>] [--speaker <标签>] [--recordings-dir <path>] [--include-device-corpus] [--max-minutes 30] [--noise fan|pump|burst] [--noise-gain 0.22] [--compare-digital] [--runtime-dir <path>] [--url http://127.0.0.1:<port>] [--out-dir <path>]",
      );
      process.exit(0);
    }
    const result = await runXiaozhiE2e(opts);
    if (result.errorMessage) {
      console.error(result.errorMessage);
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
    console.error(redactText(error?.message || String(error)));
    process.exit(1);
  }
}
