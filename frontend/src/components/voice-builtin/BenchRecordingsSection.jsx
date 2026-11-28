import React, { useCallback, useEffect, useRef, useState } from "react";
import { Check, Mic, Play, RotateCcw, Save, SkipForward, Square, Trash2 } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import {
  BENCH_REC_MAX_DURATION_MS,
  BENCH_REC_TARGET_RATE,
  BENCH_RECORDINGS_CLEAR_ALL_CONFIRM_OPTIONS,
  BENCH_RECORDINGS_CLEAR_ALL_CONFIRM_TEXT,
  BENCH_RECORDINGS_DELETE_ITEM_CONFIRM_OPTIONS,
  BENCH_RECORDINGS_DELETE_ITEM_CONFIRM_TEXT,
  DEFAULT_SPEAKER_PRESETS,
  analyzeRecordingQuality,
  benchRecordingGuideState,
  encodeMonoWavDataUrl,
  findNextUnrecordedIndex,
  formatCorpusPromptSegments,
  liveMicLevelView,
  micPermissionErrorHint,
  resampleFloatTo16k,
  validateSpeakerLabel,
  voiceErrorText,
} from "../../voiceState.js";

/** VD2 App 内“录制测试语料”引导子块（使用电脑麦克风录制 16 kHz 单声道 WAV，仅存本机主窗口管理）。 */
export function BenchRecordingsSection({ builtin, active = true }) {
  const [data, setData] = useState({
    recordings_dir_rel: "voice-bench/recordings",
    corpus_name: "",
    corpus_count: 0,
    corpus_items: [],
    total_recordings: 0,
    speakers: [],
    recordings: [],
  });
  const [speaker, setSpeaker] = useState(DEFAULT_SPEAKER_PRESETS[0]);
  const [newSpeakerInput, setNewSpeakerInput] = useState("");
  const [currentIndex, setCurrentIndex] = useState(0);
  const [recording, setRecording] = useState(false);
  const [openingMic, setOpeningMic] = useState(false);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [livePeak, setLivePeak] = useState(0);
  const [saving, setSaving] = useState(false);
  const [loading, setLoading] = useState(false);
  const [draft, setDraft] = useState(null); // { wavDataUrl, quality }
  const [savedAudioUrl, setSavedAudioUrl] = useState("");
  const [feedback, setFeedback] = useState(null); // { tone, text }

  const mountedRef = useRef(true);
  const micSessionRef = useRef(null);
  const micEpochRef = useRef(0);
  const previewEpochRef = useRef(0);
  const audioElRef = useRef(null);

  const stopPreviewAudio = useCallback(() => {
    previewEpochRef.current += 1;
    try {
      audioElRef.current?.pause?.();
    } catch {
      // 忽略暂停异常
    }
  }, []);

  const stopMicHardware = useCallback(() => {
    const sess = micSessionRef.current;
    micSessionRef.current = null;
    if (!sess) return null;
    if (sess.tickTimer) clearInterval(sess.tickTimer);
    if (sess.maxTimer) clearTimeout(sess.maxTimer);
    try {
      sess.processor?.disconnect?.();
      sess.source?.disconnect?.();
    } catch {
      // 忽略断开异常
    }
    sess.stream?.getTracks?.().forEach((t) => t.stop());
    try {
      void sess.ctx?.close?.();
    } catch {
      // 忽略关闭异常
    }
    setRecording(false);
    setLivePeak(0);
    return sess;
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      micEpochRef.current += 1;
      stopPreviewAudio();
      const sess = stopMicHardware();
      if (sess) sess.aborted = true;
    };
  }, [stopMicHardware, stopPreviewAudio]);

  useEffect(() => {
    if (!active) {
      // 语音中心切标签使用 hidden={!active} 不卸载组件，隐藏时立即释放麦克风并停止试听
      micEpochRef.current += 1;
      stopPreviewAudio();
      const sess = stopMicHardware();
      if (sess) sess.aborted = true;
      setOpeningMic(false);
    }
  }, [active, stopMicHardware, stopPreviewAudio]);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const res = await voiceApi.benchRecordingsManage({ action: "list" });
      if (!mountedRef.current || !res || typeof res !== "object") return;
      setData(res);
      if (Array.isArray(res.speakers) && res.speakers.length > 0) {
        setSpeaker((prev) => prev || res.speakers[0].speaker);
      }
    } catch (error) {
      if (mountedRef.current) {
        setFeedback({ tone: "bad", text: `读取测试录音列表失败：${voiceErrorText(error)}` });
      }
    } finally {
      if (mountedRef.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (active) void refresh();
  }, [active, refresh]);

  const guide = benchRecordingGuideState({
    corpusItems: data.corpus_items,
    recordings: data.recordings,
    speaker,
    currentIndex,
    recording,
    openingMic,
    saving,
    draft,
  });

  const allSpeakerOptions = Array.from(
    new Set([
      ...DEFAULT_SPEAKER_PRESETS,
      ...(Array.isArray(data.speakers) ? data.speakers.map((s) => s.speaker) : []),
      ...(speaker ? [speaker] : []),
    ]),
  );

  const selectSentence = (nextIdx) => {
    if (recording || openingMic || saving) return;
    stopPreviewAudio();
    setCurrentIndex(nextIdx);
    setDraft(null);
    setSavedAudioUrl("");
    setFeedback(null);
  };

  const selectSpeaker = (label) => {
    if (recording || openingMic || saving) return;
    stopPreviewAudio();
    setSpeaker(label);
    setDraft(null);
    setSavedAudioUrl("");
    setFeedback(null);
  };

  const addSpeaker = () => {
    const check = validateSpeakerLabel(newSpeakerInput);
    if (!check.ok) {
      setFeedback({ tone: "warn", text: check.error });
      return;
    }
    stopPreviewAudio();
    setSpeaker(check.speaker);
    setNewSpeakerInput("");
    setDraft(null);
    setSavedAudioUrl("");
    setFeedback({ tone: "info", text: `已切换到说话人标签「${check.speaker}」（仅作测试分组标签，不用于声纹识别）` });
  };

  const startRecording = async () => {
    if (!guide.canStart) return;
    if (!globalThis.navigator?.mediaDevices?.getUserMedia) {
      setFeedback({
        tone: "bad",
        text: "当前运行环境不支持浏览器麦克风采集，请在 MOSS 桌面主窗口中录制。",
      });
      return;
    }
    stopPreviewAudio();
    setDraft(null);
    setSavedAudioUrl("");
    setFeedback(null);
    setElapsedMs(0);
    setLivePeak(0);

    const epoch = ++micEpochRef.current;
    setOpeningMic(true);
    let pendingStream = null;
    let pendingCtx = null;

    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          channelCount: 1,
          echoCancellation: false,
          noiseSuppression: false,
          autoGainControl: false,
        },
      });
      pendingStream = stream;
      if (!mountedRef.current || micEpochRef.current !== epoch) {
        stream.getTracks().forEach((t) => t.stop());
        return;
      }
      const AudioCtx = globalThis.AudioContext || globalThis.webkitAudioContext;
      const ctx = new AudioCtx();
      pendingCtx = ctx;
      if (ctx.state === "suspended" && typeof ctx.resume === "function") {
        await ctx.resume();
      }
      if (!mountedRef.current || micEpochRef.current !== epoch) {
        stream.getTracks().forEach((t) => t.stop());
        try {
          void ctx.close?.();
        } catch {
          // ignore
        }
        return;
      }

      const inputRate = ctx.sampleRate || 48000;
      const source = ctx.createMediaStreamSource(stream);
      const processor = ctx.createScriptProcessor(4096, 1, 1);
      const chunks = [];
      let finished = false;
      let latestChunkPeak = 0;
      const startedAt = Date.now();

      processor.onaudioprocess = (event) => {
        if (finished) return;
        const ch = event.inputBuffer.getChannelData(0);
        chunks.push(new Float32Array(ch));
        let peak = 0;
        for (let i = 0; i < ch.length; i += 4) {
          const a = Math.abs(ch[i]);
          if (a > peak) peak = a;
        }
        latestChunkPeak = Math.min(100, Math.round(peak * 100));
      };
      source.connect(processor);
      processor.connect(ctx.destination);

      const sessionObj = {
        stream,
        ctx,
        source,
        processor,
        tickTimer: null,
        maxTimer: null,
        aborted: false,
        finish: null,
      };

      const finish = () => {
        if (finished || sessionObj.aborted) return;
        finished = true;
        stopMicHardware();
        const totalSamples = chunks.reduce((sum, a) => sum + a.length, 0);
        const merged = new Float32Array(totalSamples);
        let offset = 0;
        for (const arr of chunks) {
          merged.set(arr, offset);
          offset += arr.length;
        }
        const pcm16k = resampleFloatTo16k(merged, inputRate, BENCH_REC_TARGET_RATE);
        const quality = analyzeRecordingQuality(pcm16k, BENCH_REC_TARGET_RATE);
        const wavDataUrl = encodeMonoWavDataUrl(pcm16k, BENCH_REC_TARGET_RATE);
        if (!mountedRef.current) return;
        setDraft({ wavDataUrl, quality });
        if (!quality.ok) {
          setFeedback({ tone: "warn", text: quality.error });
        } else {
          setFeedback({
            tone: "ok",
            text: `录制完成（${(quality.durationMs / 1000).toFixed(1)} 秒 · 峰值电平 ${quality.peakLevel}%），点“保存”（或按回车）即可保存并跳到下一句。`,
          });
        }
      };

      sessionObj.finish = finish;
      sessionObj.tickTimer = setInterval(() => {
        if (mountedRef.current) {
          setElapsedMs(Date.now() - startedAt);
          setLivePeak(latestChunkPeak);
        }
      }, 100);
      // 超过 15 秒上限自动停止
      sessionObj.maxTimer = setTimeout(() => {
        finish();
      }, BENCH_REC_MAX_DURATION_MS + 100);

      micSessionRef.current = sessionObj;
      pendingStream = null;
      pendingCtx = null;
      setRecording(true);
    } catch (error) {
      pendingStream?.getTracks?.().forEach((t) => t.stop());
      try {
        void pendingCtx?.close?.();
      } catch {
        // ignore
      }
      if (mountedRef.current && micEpochRef.current === epoch) {
        setFeedback({ tone: "bad", text: micPermissionErrorHint(error) });
      }
    } finally {
      if (mountedRef.current && micEpochRef.current === epoch) {
        setOpeningMic(false);
      }
    }
  };

  const stopRecording = () => {
    const sess = micSessionRef.current;
    if (sess && typeof sess.finish === "function") {
      sess.finish();
    }
  };

  const retryRecording = () => {
    setDraft(null);
    setSavedAudioUrl("");
    setFeedback(null);
    void startRecording();
  };

  const previewAudio = async () => {
    if (draft?.wavDataUrl) {
      previewEpochRef.current += 1;
      if (audioElRef.current) {
        audioElRef.current.src = draft.wavDataUrl;
        void audioElRef.current.play?.().catch(() => {});
      }
      return;
    }
    if (!guide.currentSaved || !guide.currentItem) return;
    const reqEpoch = ++previewEpochRef.current;
    try {
      const res = await voiceApi.benchRecordingsManage({
        action: "audio",
        speaker: guide.speaker,
        corpus_id: guide.currentItem.id,
      });
      if (!mountedRef.current || previewEpochRef.current !== reqEpoch) return;
      if (res?.audio_data_url) {
        setSavedAudioUrl(res.audio_data_url);
        setTimeout(() => {
          if (!mountedRef.current || previewEpochRef.current !== reqEpoch) return;
          if (audioElRef.current) void audioElRef.current.play?.().catch(() => {});
        }, 30);
      }
    } catch (error) {
      if (!mountedRef.current || previewEpochRef.current !== reqEpoch) return;
      await showAlert(voiceErrorText(error), { title: "读取测试录音失败", tone: "error" });
    }
  };

  const saveRecording = async () => {
    if (!guide.canSave || !guide.currentItem || !draft?.wavDataUrl) return;
    stopPreviewAudio();
    const savedIndex = guide.currentIndex;
    const savedCorpusId = guide.currentItem.id;
    setSaving(true);
    setFeedback(null);
    try {
      const res = await voiceApi.benchRecordingsManage({
        action: "save",
        speaker: guide.speaker,
        corpus_id: savedCorpusId,
        text: guide.currentItem.text,
        audio_wav_base64: draft.wavDataUrl,
      });
      if (!mountedRef.current) return;
      setData(res);
      setDraft(null);
      setSavedAudioUrl("");

      const nextItems = Array.isArray(res?.corpus_items) ? res.corpus_items : data.corpus_items;
      const nextRecs = Array.isArray(res?.recordings) ? res.recordings : data.recordings;
      const recMap = new Map();
      for (const r of nextRecs) {
        if (r && r.speaker === guide.speaker && r.corpus_id) recMap.set(r.corpus_id, r);
      }
      recMap.set(savedCorpusId, true);
      const allDone = nextItems.length > 0 && nextItems.every((item) => recMap.has(item.id));
      const nextIdx = findNextUnrecordedIndex(nextItems, recMap, savedIndex);
      const durSec = ((res?.saved?.duration_ms || 0) / 1000).toFixed(1);

      const savedIsWake = guide.isCurrentWakeWord;
      const savedPosLabel = savedIsWake
        ? "唤醒词"
        : `第 ${guide.currentStandardPosition || savedIndex + 1} 句`;
      const nextItem = nextItems[nextIdx];
      const nextIsWake = nextItem && (nextItem.purpose === "wake" || nextItem.id === "wake_word");
      const nextStdPos =
        nextItem && !nextIsWake
          ? nextItems.filter((it) => it.purpose !== "wake" && it.id !== "wake_word").findIndex((it) => it.id === nextItem.id) + 1
          : 0;
      const nextPosLabel = nextIsWake ? "唤醒词" : `第 ${nextStdPos || nextIdx + 1} 句`;

      if (!allDone && nextIdx !== savedIndex) {
        setCurrentIndex(nextIdx);
        setFeedback({
          tone: "ok",
          text: `已保存「${guide.speaker}」${savedPosLabel}（${durSec} 秒），已自动跳到${nextPosLabel}。`,
        });
      } else {
        setFeedback({
          tone: "ok",
          text: allDone
            ? `已保存「${guide.speaker}」${savedPosLabel}（${durSec} 秒）· 该说话人全部语料已录齐！`
            : `已保存「${guide.speaker}」${savedPosLabel}录音（16 kHz 单声道 WAV，${durSec} 秒）。`,
        });
      }
      await builtin?.refresh?.();
    } catch (error) {
      if (mountedRef.current) {
        setFeedback({ tone: "bad", text: voiceErrorText(error) });
      }
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  const deleteCurrentRecording = async () => {
    if (!guide.currentSaved || !guide.currentItem) return;
    const ok = await showConfirm(
      BENCH_RECORDINGS_DELETE_ITEM_CONFIRM_TEXT,
      BENCH_RECORDINGS_DELETE_ITEM_CONFIRM_OPTIONS,
    );
    if (!ok) return;
    stopPreviewAudio();
    setSaving(true);
    try {
      const res = await voiceApi.benchRecordingsManage({
        action: "delete",
        speaker: guide.speaker,
        corpus_id: guide.currentItem.id,
      });
      if (!mountedRef.current) return;
      setData(res);
      setSavedAudioUrl("");
      setFeedback({ tone: "info", text: `已删除「${guide.speaker}」的当前句测试录音` });
      await builtin?.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "删除测试录音失败", tone: "error" });
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  const clearAllRecordings = async () => {
    const ok = await showConfirm(
      BENCH_RECORDINGS_CLEAR_ALL_CONFIRM_TEXT,
      BENCH_RECORDINGS_CLEAR_ALL_CONFIRM_OPTIONS,
    );
    if (!ok) return;
    stopPreviewAudio();
    setSaving(true);
    try {
      const res = await voiceApi.benchRecordingsManage({
        action: "clear",
        confirmed: true,
      });
      if (!mountedRef.current) return;
      setData(res);
      setDraft(null);
      setSavedAudioUrl("");
      setFeedback({ tone: "info", text: `已删除全部 ${res?.deleted_count ?? 0} 条测试录音` });
      await builtin?.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "删除全部测试录音失败", tone: "error" });
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  const handleRecorderKeyDown = (e) => {
    const tag = String(e.target?.tagName || "").toUpperCase();
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || tag === "AUDIO") return;
    if (e.code === "Space") {
      e.preventDefault();
      if (guide.canStop) stopRecording();
      else if (guide.canStart) void startRecording();
    } else if (e.key === "Enter" && guide.canSave) {
      e.preventDefault();
      void saveRecording();
    }
  };

  const promptParts = formatCorpusPromptSegments(guide.currentItem);
  const previewSrc = draft?.wavDataUrl || savedAudioUrl;
  const vu = liveMicLevelView(livePeak);
  let standardSeq = 0;

  return (
    <div className="voice-block" id="voice-bench-recordings-box">
      <div className="voice-block-h">
        <h3>录制测试语料（电脑麦克风 · 真人录音）</h3>
        <span
          className={`ob-pill ${guide.complete ? "ob-pill--ok" : "ob-pill--off"}`}
          id="voice-bench-recordings-progress"
        >
          {guide.progressText}
        </span>
        {guide.hasWakeItem && (
          <span
            className={`ob-pill ${guide.wakeRecorded ? "ob-pill--ok" : "ob-pill--off"}`}
            id="voice-bench-wake-progress"
          >
            {guide.wakeRecorded ? "唤醒词已录" : "唤醒词未录"}
          </span>
        )}
        {data.total_recordings > 0 && (
          <button
            type="button"
            className="btn btn-ghost voice-push"
            id="voice-bench-recordings-clear-all"
            disabled={recording || openingMic || saving}
            onClick={() => void clearAllRecordings()}
          >
            <Trash2 size={13} /> 删除全部测试录音（{data.total_recordings}）
          </button>
        )}
      </div>

      <p className="voice-muted">
        可选补充你自己的声音，用于补齐 13 句工作室标准语料（默认不录也可用，AI 会自动准备网上真人语音；录音为 16 kHz 单声道 WAV 存本机{" "}
        <code className="mono">{`<运行目录>/${data.recordings_dir_rel || "voice-bench/recordings"}/`}</code>
        ，支持多说话人标签，不进诊断包与过程记录，不用于声纹）。
      </p>

      {/* 说话人标签选择 / 新建（左选标签、右紧凑新建，统一等高对齐） */}
      <div className="ai-bench-speaker-bar">
        <div className="ai-bench-speaker-list">
          <span className="voice-muted">说话人：</span>
          {allSpeakerOptions.map((label) => {
            const spInfo = (data.speakers || []).find((s) => s.speaker === label);
            const activeSpeaker = label === speaker;
            return (
              <button
                key={label}
                type="button"
                className={activeSpeaker ? "btn btn-primary" : "btn btn-ghost"}
                disabled={recording || openingMic || saving}
                onClick={() => selectSpeaker(label)}
              >
                {label}
                {spInfo ? ` (${spInfo.recorded_count}/${spInfo.corpus_count})` : ""}
              </button>
            );
          })}
        </div>
        <div className="ai-bench-speaker-add">
          <input
            type="text"
            id="voice-bench-new-speaker"
            className="text-input"
            placeholder="新建标签如 家人 B"
            value={newSpeakerInput}
            disabled={recording || openingMic || saving}
            onChange={(e) => setNewSpeakerInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addSpeaker();
              }
            }}
          />
          <button
            type="button"
            className="btn btn-ghost"
            id="voice-bench-add-speaker"
            disabled={recording || openingMic || saving || !newSpeakerInput.trim()}
            onClick={addSpeaker}
          >
            添加说话人
          </button>
        </div>
      </div>

      {loading && guide.itemCount === 0 ? (
        <p className="voice-muted">正在加载测试语料列表…</p>
      ) : !guide.currentItem ? (
        <p className="voice-muted">未读到测试语料列表。</p>
      ) : (
        <div
          className="voice-debug-card ai-bench-recorder-card"
          id="voice-bench-recorder-card"
          tabIndex={0}
          onKeyDown={handleRecorderKeyDown}
        >
          {/* 句序号三态快捷导航（已录绿边 / 当前高亮 / 未录虚线；唤醒词独立标“唤醒”，不计入 13 句标准语料） */}
          <div className="ai-bench-sentence-nav" role="navigation" aria-label="测试语料句号导航">
            {data.corpus_items.map((item, idx) => {
              const recorded = guide.recordedMap.has(item.id);
              const isCur = idx === guide.currentIndex;
              const isWake = item.purpose === "wake" || item.id === "wake_word";
              const pillText = isWake ? "唤醒" : String(++standardSeq);
              return (
                <button
                  key={item.id}
                  type="button"
                  className={`btn ${isCur ? "btn-primary is-cur" : "btn-ghost"} ai-bench-step-btn ${
                    recorded ? "is-recorded" : "is-unrecorded"
                  }`}
                  disabled={recording || openingMic || saving}
                  onClick={() => selectSentence(idx)}
                  title={`${isWake ? "唤醒词" : `第 ${pillText} 句`}. ${item.label}${recorded ? "（已录）" : "（未录）"}`}
                >
                  {recorded ? <Check size={11} /> : null} {pillText}
                </button>
              );
            })}
          </div>

          <div className="ai-bench-rec-head">
            <span className="ai-bench-rec-title">
              <b>
                {guide.isCurrentWakeWord
                  ? `唤醒词 · ${guide.currentItem.label}`
                  : `第 ${guide.currentStandardPosition} / ${guide.totalCount} 句 · ${guide.currentItem.label}`}
              </b>
              {guide.currentSaved ? (
                <span className="ob-pill ob-pill--ok">
                  已录（{((guide.currentSaved.duration_ms || 0) / 1000).toFixed(1)}s · 电平 {guide.currentSaved.peak_level}%）
                </span>
              ) : (
                <span className="ob-pill ob-pill--off">未录</span>
              )}
            </span>
            {guide.currentSaved && (
              <button
                type="button"
                className="btn btn-ghost voice-inline-btn"
                id="voice-bench-rec-delete-one"
                disabled={recording || openingMic || saving}
                onClick={() => void deleteCurrentRecording()}
              >
                删除此句录音
              </button>
            )}
          </div>

          {/* 朗读正文 + 句中停顿醒目提示 */}
          <div className="ai-bench-rec-prompt" id="voice-bench-rec-prompt">
            {promptParts.map((part, idx) =>
              part.type === "pause" ? (
                <span key={`p-${idx}`} className="ob-pill ob-pill--warn ai-bench-pause-pill">
                  {part.label}
                </span>
              ) : (
                <span key={`t-${idx}`}>{part.text}</span>
              ),
            )}
          </div>

          {/* 停顿提示与该句考核重点胶囊 */}
          {(guide.currentItem.pause_hint || guide.expectBadges.length > 0) && (
            <div className="ai-bench-expect-row" id="voice-bench-rec-expects">
              {guide.expectBadges.map((b) => (
                <span key={b.key} className={`ob-pill ob-pill--${b.tone}`}>
                  {b.text}
                </span>
              ))}
              {guide.currentItem.pause_hint && (
                <span className="voice-muted">停顿说明：{guide.currentItem.pause_hint}</span>
              )}
            </div>
          )}

          {/* 录音中实时麦克风音量电平条（VU Meter） */}
          {recording && (
            <div className="ai-bench-vu-box" id="voice-bench-rec-meter" role="status">
              <div className="ai-bench-vu-track">
                <div
                  className={`ai-bench-vu-fill is-${vu.tone}`}
                  style={{ width: `${Math.max(3, vu.percent)}%` }}
                />
              </div>
              <span className="voice-muted mono">
                实时电平 {vu.percent}% · {vu.hint}
              </span>
            </div>
          )}

          {/* 录制操作按钮：同一时刻只有一个主色/危险色焦点按钮 */}
          <div className="ai-bench-rec-actions">
            <button
              type="button"
              className={guide.canStart && !guide.canSave ? "btn btn-primary" : "btn btn-ghost"}
              id="voice-bench-rec-start"
              disabled={!guide.canStart}
              onClick={() => void startRecording()}
            >
              <Mic size={13} /> {openingMic ? "正在打开麦克风…" : "开始录"}
            </button>
            <button
              type="button"
              className={guide.canStop ? "btn btn-danger" : "btn btn-ghost"}
              id="voice-bench-rec-stop"
              disabled={!guide.canStop}
              onClick={stopRecording}
            >
              <Square size={13} /> 停止{recording ? `（${(elapsedMs / 1000).toFixed(1)}s）` : ""}
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              id="voice-bench-rec-retry"
              disabled={!guide.canRetry}
              onClick={retryRecording}
            >
              <RotateCcw size={13} /> 重录
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              id="voice-bench-rec-preview"
              disabled={!guide.canPreview}
              onClick={() => void previewAudio()}
            >
              <Play size={13} /> 试听
            </button>
            <button
              type="button"
              className={guide.canSave ? "btn btn-primary" : "btn btn-ghost"}
              id="voice-bench-rec-save"
              disabled={!guide.canSave}
              onClick={() => void saveRecording()}
            >
              <Save size={13} /> {saving ? "保存中…" : "保存"}
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              id="voice-bench-rec-next"
              disabled={!guide.canNext}
              onClick={() => selectSentence(guide.nextIndex)}
            >
              <SkipForward size={13} /> 下一句
            </button>
            <span className="voice-muted ai-bench-key-hint">快捷键：空格 录/停 · 回车 保存并跳下一句</span>
          </div>

          {previewSrc && (
            <audio
              ref={audioElRef}
              controls
              src={previewSrc}
              className="ai-bench-audio-player"
            />
          )}

          {feedback && (
            <div
              className={`voice-hint voice-hint--${feedback.tone || "info"}`}
              id="voice-bench-rec-feedback"
            >
              {feedback.text}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
