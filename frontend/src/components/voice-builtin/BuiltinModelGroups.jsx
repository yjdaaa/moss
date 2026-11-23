import React, { useCallback, useEffect, useRef, useState } from "react";
import { Square, FlaskConical, Volume2, Sparkles, Mic } from "lucide-react";
import { voiceApi } from "../../voiceApi.js";
import { focusVoiceGroup } from "../VoiceLayout.jsx";
import {
  MIX_SECTIONS,
  chainTestPipelineView,
  encodeMonoWavDataUrl,
  pipelineTestOutcome,
  pipelineTestPlan,
  pipelineTestSummary,
  builtinSectionSourceView,
  builtinSourceSwitchPatch,
  isFoldedLocalModel,
  isLocalJevModel,
  isLocalOneJevModel,
  recommendationState,
  voiceErrorText,
} from "../../voiceState.js";
import { JEV_GROUP_TITLE, restartEffect, revealLocalMore } from "./shared.js";
import { BuiltinModelGroup } from "./BuiltinModelGroup.jsx";
import { BuiltinJevGroup } from "./BuiltinJevGroup.jsx";

/** 内置服务顶部五栏自由搭配下拉选择栏 + 下方云端模型与 Jev 配置卡片。 */
export function BuiltinModelGroups({ builtin }) {
  const { status, setStatus, busy } = builtin;
  const [mixBusy, setMixBusy] = useState("");
  const [mixNote, setMixNote] = useState(null);
  // 测试全部环节 / 连贯试跑：{ running, mode?, results: {key: {ok, skipped, running, detail}}, summary, chainMeta? }
  const [pipeline, setPipeline] = useState(null);
  const [recordingChain, setRecordingChain] = useState(false);
  const [openingMic, setOpeningMic] = useState(false);
  const [playingSlot, setPlayingSlot] = useState(""); // "" | "input" | "reply"
  const chainAudioRef = useRef(null);
  const recSessionRef = useRef(null);
  const micOpeningRef = useRef(false);
  const micEpochRef = useRef(0);
  const mountedRef = useRef(true);

  const stopChainAudio = useCallback(() => {
    if (chainAudioRef.current) {
      try {
        chainAudioRef.current.pause();
        chainAudioRef.current.currentTime = 0;
      } catch {
        // ignore
      }
      chainAudioRef.current = null;
    }
    if (mountedRef.current) setPlayingSlot("");
  }, []);

  const stopMicSession = useCallback((abortPending = false) => {
    if (abortPending) {
      micEpochRef.current += 1;
      micOpeningRef.current = false;
      if (mountedRef.current) setOpeningMic(false);
    }
    const active = recSessionRef.current;
    if (!active) return null;
    recSessionRef.current = null;
    if (abortPending) active.aborted = true;
    clearTimeout(active.timer);
    try {
      active.processor?.disconnect?.();
      active.source?.disconnect?.();
    } catch {
      // ignore
    }
    active.stream?.getTracks?.().forEach((t) => t.stop());
    try {
      void active.ctx?.close?.();
    } catch {
      // ignore
    }
    if (mountedRef.current) setRecordingChain(false);
    return active;
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      stopChainAudio();
      stopMicSession(true);
    };
  }, [stopChainAudio, stopMicSession]);

  if (!status || status.error) return <p className="voice-muted">{status?.error || "正在读取内置服务状态…"}</p>;
  const views = MIX_SECTIONS.map((item) => ({ ...item, view: builtinSectionSourceView(item.key, status) }));
  const focusConfigGroup = focusVoiceGroup;

  const toggleChainAudio = async (slot, url) => {
    if (!url) return;
    if (playingSlot === slot && chainAudioRef.current) {
      stopChainAudio();
      return;
    }
    stopChainAudio();
    try {
      const audio = new Audio(url);
      chainAudioRef.current = audio;
      setPlayingSlot(slot);
      audio.onended = () => { if (mountedRef.current) setPlayingSlot(""); };
      audio.onerror = () => { if (mountedRef.current) setPlayingSlot(""); };
      await audio.play();
    } catch {
      if (mountedRef.current) setPlayingSlot("");
    }
  };

  const runPipelineTest = async () => {
    stopChainAudio();
    const plan = pipelineTestPlan(status);
    const results = Object.fromEntries(
      plan.map((step) => [
        step.key,
        step.kind === "static" ? { ok: step.ok, skipped: Boolean(step.skipped), detail: step.detail } : { running: true, detail: "测试中…" },
      ]),
    );
    setMixNote(null);
    setPipeline({ running: true, mode: "parallel", results: { ...results }, summary: null, chainMeta: null });
    builtin.setBusy("pipeline-test");
    try {
      // 各环节互不依赖，同时测；每个返回后立即显示，不等最慢的那个
      await Promise.all(
        plan
          .filter((step) => step.kind !== "static")
          .map(async (step) => {
            let result = null;
            let failure = null;
            try {
              result = await voiceApi.builtinModelsTest(step.kind === "jev" ? "jev" : step.section, step.input);
            } catch (error) {
              failure = error;
            }
            results[step.key] = pipelineTestOutcome(result, failure, voiceErrorText);
            if (mountedRef.current) {
              setPipeline((prev) => (prev ? { ...prev, results: { ...prev.results, [step.key]: results[step.key] } } : prev));
            }
          }),
      );
      if (mountedRef.current) {
        setPipeline({ running: false, mode: "parallel", results, summary: pipelineTestSummary(results), chainMeta: null });
      }
    } finally {
      builtin.setBusy("");
    }
  };

  /** 整链连贯测试：测试录音 → 1.判停 → 2.语音识别（本地也真跑推理） → 3.Jev粗筛 → 4.大模型回复 → 5.语音合成。 */
  const runChainTest = async (payload = {}) => {
    stopChainAudio();
    const runningResults = Object.fromEntries(
      MIX_SECTIONS.map(({ key }, idx) => [
        key,
        { running: true, detail: `第 ${idx + 1} 棒接力等待中…` },
      ]),
    );
    setMixNote(null);
    setPipeline({ running: true, mode: "chain", results: runningResults, summary: null, chainMeta: null });
    builtin.setBusy("chain-test");
    try {
      let res = null;
      let err = null;
      try {
        res = await voiceApi.builtinModelsTest("chain", payload);
      } catch (error) {
        err = error;
      }
      if (mountedRef.current) {
        setPipeline(chainTestPipelineView(res, err, voiceErrorText));
      }
    } finally {
      builtin.setBusy("");
    }
  };

  /** 电脑麦克风录制最长 3.5 秒真话（再次点击立即结束并送入 5 步连贯测试）。 */
  const toggleRecordChainTest = async () => {
    if (recordingChain && recSessionRef.current) {
      const done = stopMicSession();
      if (done) void done.finish();
      return;
    }
    if (micOpeningRef.current) {
      return;
    }
    if (!globalThis.navigator?.mediaDevices?.getUserMedia) {
      setMixNote({ ok: false, text: "当前环境不支持麦克风录音，可直接点击「连贯试跑」使用内置测试语音跑完全链路。" });
      return;
    }
    stopChainAudio();
    setMixNote(null);
    const myEpoch = ++micEpochRef.current;
    micOpeningRef.current = true;
    setOpeningMic(true);
    let pendingStream = null;
    let pendingCtx = null;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
      });
      pendingStream = stream;
      if (!mountedRef.current || micEpochRef.current !== myEpoch) {
        stream.getTracks().forEach((t) => t.stop());
        return;
      }
      const AudioCtx = globalThis.AudioContext || globalThis.webkitAudioContext;
      const ctx = new AudioCtx();
      pendingCtx = ctx;
      if (ctx.state === "suspended" && typeof ctx.resume === "function") {
        await ctx.resume();
      }
      if (!mountedRef.current || micEpochRef.current !== myEpoch) {
        stream.getTracks().forEach((t) => t.stop());
        try {
          void ctx.close?.();
        } catch {
          // ignore
        }
        return;
      }
      const sampleRate = ctx.sampleRate || 16000;
      const source = ctx.createMediaStreamSource(stream);
      const processor = ctx.createScriptProcessor(4096, 1, 1);
      const chunks = [];
      let finished = false;
      processor.onaudioprocess = (event) => {
        if (finished) return;
        const channel = event.inputBuffer.getChannelData(0);
        chunks.push(new Float32Array(channel));
      };
      source.connect(processor);
      processor.connect(ctx.destination);

      const sessionObj = { stream, ctx, source, processor, timer: null, finish: null, aborted: false };
      const finish = async () => {
        if (finished || sessionObj.aborted) return;
        finished = true;
        stopMicSession();
        const totalLen = chunks.reduce((sum, arr) => sum + arr.length, 0);
        if (totalLen < sampleRate * 0.25) {
          if (mountedRef.current) {
            setMixNote({ ok: false, text: "录音时间太短，请点击「录真话测」后清晰说一句 1～3 秒的话。" });
          }
          return;
        }
        const merged = new Float32Array(totalLen);
        let offset = 0;
        for (const arr of chunks) {
          merged.set(arr, offset);
          offset += arr.length;
        }
        const wavDataUrl = encodeMonoWavDataUrl(merged, sampleRate);
        await runChainTest({ audio_wav_base64: wavDataUrl });
      };

      sessionObj.finish = finish;
      sessionObj.timer = setTimeout(() => { void finish(); }, 3500);
      recSessionRef.current = sessionObj;
      pendingStream = null;
      pendingCtx = null;
      setRecordingChain(true);
    } catch (error) {
      pendingStream?.getTracks?.().forEach((t) => t.stop());
      try {
        void pendingCtx?.close?.();
      } catch {
        // ignore
      }
      if (micEpochRef.current === myEpoch) {
        stopMicSession();
        if (mountedRef.current) {
          setMixNote({
            ok: false,
            text: `无法打开电脑麦克风（${error?.message || "未授予麦克风权限"}）；可直接点击「连贯试跑」使用内置测试人声测试全链。`,
          });
        }
      }
    } finally {
      if (micEpochRef.current === myEpoch) {
        micOpeningRef.current = false;
        if (mountedRef.current) {
          setOpeningMic(false);
        }
      }
    }
  };

  const handleSelectSource = async (section, label, nextValue) => {
    // 换了模型，中止在途试听/录音并清空上一次测试结果
    stopChainAudio();
    stopMicSession(true);
    setPipeline(null);
    if (section === "llm") {
      focusConfigGroup("builtin-model-llm", "voice-group-builtin-model-llm");
      return;
    }
    const currentView = builtinSectionSourceView(section, status);
    const pickedOption = currentView.options.find((opt) => opt.value === nextValue);
    if (section === "jev") {
      if (nextValue !== "off" && pickedOption && !pickedOption.installed) {
        if (pickedOption.isLocal) {
          focusConfigGroup("local-models", "voice-local-group-jev");
          setMixNote({
            ok: false,
            text: `本地决策模型“${pickedOption.modelLabel || pickedOption.id}”尚未下载：已为你展开下方“本地模型（断网可用）”，点击“下载”完成后即可在此直接下拉切换。`,
          });
        } else {
          focusConfigGroup("builtin-model-jev", "voice-group-builtin-model-jev");
          setMixNote({
            ok: false,
            text: `尚未配置 OpenRouter API Key：已为你展开下方“${JEV_GROUP_TITLE}”，填入密钥测试并保存后即可在此直接下拉切换。`,
          });
        }
        return;
      }
      const payload = builtinSourceSwitchPatch(status, "jev", nextValue);
      if (!payload) return;
      setMixBusy("jev");
      builtin.setBusy("mix-switch-jev");
      setMixNote(null);
      try {
        const next = await voiceApi.localSave(payload);
        setStatus(next);
        const accelLabel = isLocalOneJevModel(payload.jev_model)
          ? next?.local?.onejev_env?.accelerator_label || "本机硬件加速"
          : next?.local?.laya_env?.accelerator === "mps"
            ? "Apple Metal MPS"
            : next?.local?.laya_env?.cuda_found
              ? "CUDA 显卡加速"
              : "本机离线推理";
        setMixNote({
          ok: true,
          text:
            nextValue === "off"
              ? `已关闭 Jev 语音筛选（密钥与模型配置已保留） · ${restartEffect(next)}`
              : isLocalJevModel(payload.jev_model)
                ? `已切换为本地决策（${pickedOption?.modelLabel || payload.jev_model} · ${accelLabel} · 免密钥） · ${restartEffect(next)}`
                : `已切换为 Jev 决策（${payload.jev_model}） · ${restartEffect(next)}`,
        });
      } catch (error) {
        setMixNote({ ok: false, text: `切换失败：${voiceErrorText(error)}` });
      } finally {
        setMixBusy("");
        builtin.setBusy("");
      }
      return;
    }
    const isDefault = nextValue === "cloud" || nextValue === "energy";
    const cleanLabel = label.replace(/^\d+\.\s*/, "");
    if (!isDefault && pickedOption && !pickedOption.installed) {
      if (section === "vad" && isFoldedLocalModel(pickedOption.id)) {
        revealLocalMore();
        focusConfigGroup("local-models", "voice-local-vad-more");
      } else {
        focusConfigGroup("local-models", `voice-local-group-${section}`);
      }
      setMixNote({
        ok: false,
        text: `本地${cleanLabel}模型“${pickedOption.modelLabel || pickedOption.label}”尚未下载：已为你展开下方“本地模型（断网可用）”，点“下载”完成后即可在此下拉选用。`,
      });
      return;
    }
    const payload = builtinSourceSwitchPatch(status, section, nextValue);
    if (!payload) return;
    setMixBusy(section);
    builtin.setBusy(`mix-switch-${section}`);
    setMixNote(null);
    try {
      const next = await voiceApi.localSave(payload);
      setStatus(next);
      const nextView = builtinSectionSourceView(section, next);
      setMixNote({
        ok: true,
        text: isDefault
          ? section === "vad"
            ? `已切换为内置能量判停 · ${restartEffect(next)}`
            : `已切换为云端${cleanLabel} · ${restartEffect(next)}`
          : `已切换为本地${cleanLabel}（${nextView.selectedLocalModel?.label || "本地模型"}）${section === "vad" ? "" : "，云端配置与密钥已保留"} · ${restartEffect(next)}`,
      });
    } catch (error) {
      setMixNote({ ok: false, text: `切换失败：${voiceErrorText(error)}` });
    } finally {
      setMixBusy("");
      builtin.setBusy("");
    }
  };

  return (
    <>
      <div className="voice-mix-bar" id="voice-builtin-mix-bar">
        <div className="voice-mix-head">
          <b>当前语音链路</b>
          <div className="voice-mix-actions">
            <button
              type="button"
              className="btn btn-ghost"
              id="voice-builtin-test-all"
              onClick={runPipelineTest}
              disabled={Boolean(busy || mixBusy || pipeline?.running || recordingChain || openingMic)}
              title="并发探测每个环节：云端模型独立请求一次，本地模型检查是否已下载就绪"
            >
              <FlaskConical size={13} /> {pipeline?.running && pipeline?.mode !== "chain" ? "测试中…" : "测试全部环节"}
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              id="voice-builtin-test-chain"
              onClick={() => void runChainTest({})}
              disabled={Boolean(busy || mixBusy || pipeline?.running || recordingChain || openingMic)}
              title="整链连贯试跑：内置测试录音 → 1.判停 → 2.语音识别（本地模型也真实推理） → 3.Jev粗筛 → 4.大模型回复 → 5.语音合成并可试听"
            >
              <Sparkles size={13} /> {pipeline?.running && pipeline?.mode === "chain" ? "连贯试跑中…" : "连贯试跑"}
            </button>
            <button
              type="button"
              className={`btn ${recordingChain ? "btn-hud-fire" : "btn-ghost"}`}
              id="voice-builtin-record-chain"
              onClick={() => void toggleRecordChainTest()}
              disabled={Boolean((busy && !recordingChain) || mixBusy || pipeline?.running || openingMic)}
              title="用电脑麦克风录一句真话（最长 3.5 秒，再点立即结束），从 1.判停 到 5.语音合成 真实跑通全链路"
            >
              {recordingChain ? <Square size={12} /> : <Mic size={13} />}
              {recordingChain ? "说完点此送测…" : openingMic ? "请求麦克风…" : "录真话测"}
            </button>
          </div>
        </div>
        <div className={`voice-mix-grid${pipeline ? " voice-mix-grid--tested" : ""}`}>
          {views.map(({ key, label, hint, view }) => (
            <label className="voice-mix-field" key={key} id={`voice-builtin-mix-${key}`} title={hint || ""}>
              <span className="voice-mix-field-head">
                <b>{label}</b>
                {(key === "llm" || key === "jev") && (
                  <button
                    type="button"
                    className="voice-link-btn"
                    onClick={() =>
                      key === "llm" && view.source === "colab"
                        ? focusConfigGroup("local-models", "voice-local-colab-bridge")
                        : focusConfigGroup(`builtin-model-${key}`, `voice-group-builtin-model-${key}`)
                    }
                  >
                    去配置
                  </button>
                )}
              </span>
              <select
                className="text-input"
                value={view.selectValue}
                disabled={Boolean(busy || mixBusy || pipeline?.running || recordingChain || openingMic)}
                onChange={(e) => handleSelectSource(key, label, e.target.value)}
                id={`voice-builtin-mix-select-${key}`}
                aria-label={`${label}当前使用模型`}
              >
                {view.options.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
              {(() => {
                const rec = recommendationState(key, view.selectValue);
                if (!rec) return null;
                if (rec.neutral) {
                  return (
                    <span className="voice-muted voice-mix-recommend" id={`voice-builtin-recommend-${key}`}>
                      {rec.name}（{rec.why}）
                    </span>
                  );
                }
                return rec.chosen ? (
                  <span className="voice-muted voice-mix-recommend" id={`voice-builtin-recommend-${key}`}>
                    ✓ 已是推荐
                  </span>
                ) : (
                  <button
                    type="button"
                    className="voice-link-btn voice-mix-recommend"
                    id={`voice-builtin-recommend-${key}`}
                    disabled={Boolean(busy || mixBusy || pipeline?.running || recordingChain || openingMic)}
                    onClick={(e) => {
                      e.preventDefault();
                      handleSelectSource(key, label, rec.value);
                    }}
                    title={`切换为推荐的 ${rec.name}（${rec.why}）`}
                  >
                    推荐：{rec.name}（{rec.why}）
                  </button>
                );
              })()}
              {pipeline?.results?.[key] && (
                <span
                  className={`voice-mix-test voice-mix-test--${pipeline.results[key].running ? "running" : pipeline.results[key].skipped ? "skip" : pipeline.results[key].ok ? "ok" : "fail"}`}
                  id={`voice-builtin-test-${key}`}
                  role={pipeline.results[key].ok === false ? "alert" : undefined}
                >
                  {pipeline.results[key].running ? "测试中…" : `${pipeline.results[key].skipped ? "—" : pipeline.results[key].ok ? "✓" : "✗"} ${pipeline.results[key].detail}`}
                </span>
              )}
            </label>
          ))}
        </div>
        {pipeline?.summary && (
          <div className={`voice-hint voice-hint--${pipeline.summary.ok ? "ok" : "warn"}`} role="status" id="voice-builtin-test-summary">
            <div>{pipeline.summary.text}</div>
            {pipeline.chainMeta && (
              <div className="voice-chain-meta" id="voice-builtin-chain-meta">
                {pipeline.chainMeta.seedSource && <span>音源：{pipeline.chainMeta.seedSource}</span>}
                {pipeline.chainMeta.recognizedText && <span>识别：<b>“{pipeline.chainMeta.recognizedText}”</b></span>}
                {pipeline.chainMeta.replyText && <span>回复：<b>“{pipeline.chainMeta.replyText}”</b></span>}
                <span className="voice-chain-btns">
                  {pipeline.chainMeta.inputAudioUrl && (
                    <button
                      type="button"
                      className="btn btn-ghost voice-inline-btn"
                      id="voice-chain-play-input"
                      onClick={() => void toggleChainAudio("input", pipeline.chainMeta.inputAudioUrl)}
                    >
                      {playingSlot === "input" ? <Square size={12} /> : <Volume2 size={13} />}
                      {playingSlot === "input" ? "停止原音" : "听测试原音"}
                    </button>
                  )}
                  {pipeline.chainMeta.replyAudioUrl && (
                    <button
                      type="button"
                      className="btn btn-ghost voice-inline-btn"
                      id="voice-chain-play-reply"
                      onClick={() => void toggleChainAudio("reply", pipeline.chainMeta.replyAudioUrl)}
                    >
                      {playingSlot === "reply" ? <Square size={12} /> : <Volume2 size={13} />}
                      {playingSlot === "reply" ? "停止试听" : "试听合成回复"}
                    </button>
                  )}
                </span>
              </div>
            )}
          </div>
        )}
        {mixNote && (
          <div className={`voice-hint voice-hint--${mixNote.ok ? "ok" : "warn"}`} role="status" id="voice-builtin-mix-status">
            {mixNote.text}
          </div>
        )}
        <span className="voice-muted voice-mix-foot">
          按真实语音链路从左到右排列：拾音判停 → 语音识别 → Jev 前置粗筛（识别后·进大模型前） → 大模型 → 语音合成；直接下拉选完立即生效，这里是切换“用哪个模型、开不开 Jev”的唯一位置。下方卡片只用于填密钥、选模型版本和下载本地模型。
        </span>
      </div>
      <h3 className="voice-section-h">云端模型（密钥只存本机）</h3>
      <BuiltinModelGroup section="asr" builtin={builtin} />
      <BuiltinModelGroup section="llm" builtin={builtin} />
      <BuiltinModelGroup section="tts" builtin={builtin} />
    </>
  );
}

/** Jev 云端配置（高级）：n69 起排在语音识别 / 大模型 / 语音合成 / 视觉理解之后，由语音中心页单独摆放。 */
export function BuiltinJevModelGroup({ builtin }) {
  if (!builtin.status || builtin.status.error) return null;
  return <BuiltinJevGroup builtin={builtin} />;
}
