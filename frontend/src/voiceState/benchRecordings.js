/**
 * VD2 本机“录制测试语料”引导的纯状态、16 kHz 重采样、电平/时长/削顶校验与权限错误提示逻辑。
 * 不依赖 React / DOM，便于单测直接验证；绝不涉及身份识别或声纹。
 */

export const BENCH_REC_TARGET_RATE = 16000;
export const BENCH_REC_MIN_DURATION_MS = 500;
export const BENCH_REC_MAX_DURATION_MS = 15000;
export const BENCH_REC_MIN_PEAK = 0.03;
export const BENCH_REC_MIN_RMS = 0.005;
export const BENCH_REC_CLIP_THRESHOLD = 0.997;
export const BENCH_REC_MAX_CLIPPED_RATIO = 0.005;
export const BENCH_REC_MAX_CLIPPED_SAMPLES = 80;

export const DEFAULT_SPEAKER_PRESETS = ["我", "家人 A"];

export const BENCH_RECORDINGS_DELETE_ITEM_CONFIRM_TEXT =
  "确定删除这条测试录音？删除后无法恢复，对应语料在整体测试中将缺少该说话人的录音。";

export const BENCH_RECORDINGS_DELETE_ITEM_CONFIRM_OPTIONS = {
  title: "删除测试录音？",
  tone: "warning",
  danger: true,
  confirmText: "确认删除",
};

export const BENCH_RECORDINGS_CLEAR_ALL_CONFIRM_TEXT =
  "确定删除全部测试录音？这会永久删除运行目录 voice-bench/recordings/ 下所有说话人的测试录音，且无法恢复。";

export const BENCH_RECORDINGS_CLEAR_ALL_CONFIRM_OPTIONS = {
  title: "删除全部测试录音？",
  tone: "error",
  danger: true,
  confirmText: "全部删除",
};

const WINDOWS_RESERVED = new Set([
  "CON", "PRN", "AUX", "NUL",
  "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
  "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
]);

/** 校验说话人标签（只作标签与目录名，不用于身份或声纹）。 */
export function validateSpeakerLabel(raw) {
  const speaker = String(raw ?? "").trim();
  if (!speaker) {
    return { ok: false, speaker: "", error: "请先选择或填写说话人标签（例如“我”“家人 A”）" };
  }
  if ([...speaker].length > 32) {
    return { ok: false, speaker, error: "说话人标签最多 32 个字符" };
  }
  if (speaker === "." || speaker === ".." || speaker.includes("..")) {
    return { ok: false, speaker, error: "说话人标签不能包含路径符号" };
  }
  if (speaker.endsWith(".")) {
    return { ok: false, speaker, error: "说话人标签不能以点号结尾" };
  }
  for (const ch of speaker) {
    const code = ch.charCodeAt(0);
    if (code < 0x20 || code === 0x7f || '/\\:*?"<>|'.includes(ch)) {
      return { ok: false, speaker, error: "说话人标签包含不允许的符号" };
    }
  }
  const stem = speaker.toUpperCase().split(".")[0];
  if (WINDOWS_RESERVED.has(stem)) {
    return { ok: false, speaker, error: "说话人标签不能使用系统保留名称" };
  }
  return { ok: true, speaker, error: "" };
}

/** 线性重采样单声道浮点音频到目标采样率（默认 16 kHz）。 */
export function resampleFloatTo16k(floatSamples, srcSampleRate, dstSampleRate = BENCH_REC_TARGET_RATE) {
  const srcLen = floatSamples?.length || 0;
  const srcRate = Math.max(1, Number(srcSampleRate) || dstSampleRate);
  if (srcLen === 0 || srcRate === dstSampleRate) {
    return floatSamples instanceof Float32Array ? floatSamples : Float32Array.from(floatSamples || []);
  }
  const dstLen = Math.max(1, Math.floor((srcLen * dstSampleRate) / srcRate));
  const out = new Float32Array(dstLen);
  const last = srcLen - 1;
  for (let i = 0; i < dstLen; i += 1) {
    const pos = (i * srcRate) / dstSampleRate;
    const idx = Math.min(last, Math.floor(pos));
    const next = Math.min(last, idx + 1);
    const frac = pos - idx;
    out[i] = floatSamples[idx] * (1 - frac) + floatSamples[next] * frac;
  }
  return out;
}

/**
 * 检查录音质量（时长 0.5–15 秒、电平不能过低、不能削顶）。
 * 与后端 `bench_recordings::analyze_and_validate_recording` 门限保持一致。
 */
export function analyzeRecordingQuality(floatSamples, sampleRate = BENCH_REC_TARGET_RATE) {
  const len = floatSamples?.length || 0;
  const rate = Math.max(1, Number(sampleRate) || BENCH_REC_TARGET_RATE);
  const durationMs = Math.round((len * 1000) / rate);

  if (durationMs < BENCH_REC_MIN_DURATION_MS) {
    return {
      ok: false,
      durationMs,
      peakAmplitude: 0,
      rmsAmplitude: 0,
      peakLevel: 0,
      rmsLevel: 0,
      clippedSamples: 0,
      clippedRatio: 0,
      error: `录音时长过短（${(durationMs / 1000).toFixed(2)} 秒），需在 0.5–15 秒之间，请重录`,
    };
  }
  if (durationMs > BENCH_REC_MAX_DURATION_MS) {
    return {
      ok: false,
      durationMs,
      peakAmplitude: 0,
      rmsAmplitude: 0,
      peakLevel: 0,
      rmsLevel: 0,
      clippedSamples: 0,
      clippedRatio: 0,
      error: `录音时长过长（${(durationMs / 1000).toFixed(1)} 秒），需在 0.5–15 秒之间，请重录`,
    };
  }

  let peakAmplitude = 0;
  let sumSq = 0;
  let clippedSamples = 0;
  for (let i = 0; i < len; i += 1) {
    const v = Math.max(-1, Math.min(1, Number(floatSamples[i]) || 0));
    const a = Math.abs(v);
    if (a > peakAmplitude) peakAmplitude = a;
    if (a >= BENCH_REC_CLIP_THRESHOLD) clippedSamples += 1;
    sumSq += v * v;
  }
  const rmsAmplitude = Math.sqrt(sumSq / Math.max(1, len));
  const clippedRatio = clippedSamples / Math.max(1, len);
  const peakLevel = Math.min(100, Math.round(peakAmplitude * 100));
  const rmsLevel = Math.min(100, Math.round(rmsAmplitude * 100));

  if (peakAmplitude < BENCH_REC_MIN_PEAK || rmsAmplitude < BENCH_REC_MIN_RMS) {
    return {
      ok: false,
      durationMs,
      peakAmplitude: Number(peakAmplitude.toFixed(4)),
      rmsAmplitude: Number(rmsAmplitude.toFixed(4)),
      peakLevel,
      rmsLevel,
      clippedSamples,
      clippedRatio: Number(clippedRatio.toFixed(4)),
      error: `录音音量过低（峰值电平 ${peakLevel}%），请靠近电脑麦克风或调高系统输入音量后重录`,
    };
  }

  if (clippedSamples >= BENCH_REC_MAX_CLIPPED_SAMPLES || clippedRatio >= BENCH_REC_MAX_CLIPPED_RATIO) {
    return {
      ok: false,
      durationMs,
      peakAmplitude: Number(peakAmplitude.toFixed(4)),
      rmsAmplitude: Number(rmsAmplitude.toFixed(4)),
      peakLevel,
      rmsLevel,
      clippedSamples,
      clippedRatio: Number(clippedRatio.toFixed(4)),
      error: `录音电平过高发生削顶失真（削顶占比 ${(clippedRatio * 100).toFixed(1)}%），请离麦克风稍远或调低输入音量后重录`,
    };
  }

  return {
    ok: true,
    durationMs,
    peakAmplitude: Number(peakAmplitude.toFixed(4)),
    rmsAmplitude: Number(rmsAmplitude.toFixed(4)),
    peakLevel,
    rmsLevel,
    clippedSamples,
    clippedRatio: Number(clippedRatio.toFixed(4)),
    error: "",
  };
}

/** 麦克风权限被拒或设备不可用时，按系统返回明确的设置引导提示。 */
export function micPermissionErrorHint(error, platformHint = "") {
  const name = String(error?.name || "");
  const message = String(error?.message || error || "");
  const plat = String(platformHint || (typeof navigator !== "undefined" ? navigator.platform : "") || "").toLowerCase();
  const isDenied =
    /NotAllowedError|PermissionDeniedError|SecurityError/i.test(name) ||
    /denied|not allowed|permission/i.test(message);

  if (isDenied) {
    if (plat.includes("mac")) {
      return "麦克风权限被拒绝。请打开 macOS「系统设置 › 隐私与安全性 › 麦克风」，开启 MOSS 后重启应用再试。";
    }
    if (plat.includes("win")) {
      return "麦克风权限被拒绝。请打开 Windows「设置 › 隐私和安全性 › 麦克风」，开启“麦克风访问权限”与“允许桌面应用访问麦克风”后重试。";
    }
    return "麦克风权限被拒绝。请在系统声音/应用隐私设置中允许 MOSS 访问麦克风后重试。";
  }
  if (/NotFoundError|DevicesNotFoundError/i.test(name) || /not found|no audio/i.test(message)) {
    return "未检测到可用的电脑麦克风，请检查麦克风连接与系统默认输入设备后重试。";
  }
  return `无法打开电脑麦克风：${message || "请检查系统麦克风权限与输入设备"}`;
}

/** 把带句中停顿的语料拆成可高亮提示的文本段与停顿说明（例如“这里停顿约 0.7 秒”）。 */
export function formatCorpusPromptSegments(item) {
  if (!item) return [];
  const segs = Array.isArray(item.segments) && item.segments.length > 0 ? item.segments : null;
  if (!segs) {
    return [{ type: "text", text: String(item.text || "") }];
  }
  const out = [];
  for (const seg of segs) {
    if (seg?.text) out.push({ type: "text", text: String(seg.text) });
    if (Number(seg?.pause_ms_after) > 0) {
      const secs = (Number(seg.pause_ms_after) / 1000).toFixed(1).replace(/\.0$/, ".0");
      out.push({ type: "pause", ms: Number(seg.pause_ms_after), label: `这里停顿约 ${secs} 秒` });
    }
  }
  return out;
}

/**
 * 录制引导状态视图（用于界面与单测）：
 * 计算当前说话人已录进度（如“已录 8 / 13”）、当前句状态及各按钮启用条件。
 */
export function benchRecordingGuideState({
  corpusItems = [],
  recordings = [],
  speaker = "",
  currentIndex = 0,
  recording = false,
  openingMic = false,
  saving = false,
  draft = null,
}) {
  const items = Array.isArray(corpusItems) ? corpusItems : [];
  const totalCount = items.length;
  const safeIndex = totalCount > 0 ? Math.max(0, Math.min(totalCount - 1, Number(currentIndex) || 0)) : 0;
  const currentItem = totalCount > 0 ? items[safeIndex] : null;
  const speakerCheck = validateSpeakerLabel(speaker);

  const byCorpusId = new Map();
  if (speakerCheck.ok) {
    for (const rec of Array.isArray(recordings) ? recordings : []) {
      if (rec && rec.speaker === speakerCheck.speaker && rec.corpus_id) {
        byCorpusId.set(rec.corpus_id, rec);
      }
    }
  }

  let recordedCount = 0;
  for (const item of items) {
    if (byCorpusId.has(item.id)) recordedCount += 1;
  }

  const currentSaved = currentItem ? byCorpusId.get(currentItem.id) || null : null;
  const busy = recording || openingMic || saving;
  const draftOk = Boolean(draft && draft.wavDataUrl && draft.quality?.ok);

  return {
    speakerValid: speakerCheck.ok,
    speakerError: speakerCheck.error,
    speaker: speakerCheck.speaker,
    totalCount,
    recordedCount,
    complete: totalCount > 0 && recordedCount >= totalCount,
    progressText: `已录 ${recordedCount} / ${totalCount}`,
    currentIndex: safeIndex,
    currentItem,
    currentSaved,
    recordedMap: byCorpusId,
    canStart: speakerCheck.ok && Boolean(currentItem) && !busy,
    canStop: recording,
    canRetry: speakerCheck.ok && Boolean(currentItem) && !busy && Boolean(draft || currentSaved),
    canPreview: !recording && !openingMic && Boolean(draft?.wavDataUrl || currentSaved),
    canSave: speakerCheck.ok && Boolean(currentItem) && !busy && draftOk,
    canNext: totalCount > 1 && !busy && safeIndex < totalCount - 1,
    nextIndex: totalCount > 0 ? Math.min(totalCount - 1, safeIndex + 1) : 0,
  };
}
