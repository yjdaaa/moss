import React, { useEffect, useRef, useState } from "react";
import { Play, Square } from "lucide-react";
import { voiceApi } from "../../voiceApi.js";
import { localModelSettingsError, localTtsSpeedFixed, localTtsSpeakerGroups, voiceErrorText } from "../../voiceState.js";

/**
 * 5. 语音合成模型包下方的本地合成音色与语速（2026-11-25）。
 * 音色按名称下拉选择（读该模型的音色表，读不到时退回编号输入框）；所选模型不支持调语速时语速框置灰；
 * “按当前音色和语速试一句”用未保存的草稿当场合成，保存后才影响对话与连贯试跑。
 */
export function LocalTtsSettings({ form, update, models, voices, onBrowse }) {
  const model = models.find((item) => item.kind === "tts" && item.id === form.tts_model) || null;
  const speedFixed = localTtsSpeedFixed(model);
  const [catalog, setCatalog] = useState({ modelId: "", speakers: null });
  const [trial, setTrial] = useState({ state: "idle", text: "" });
  const audioRef = useRef(null);
  const trialSeqRef = useRef(0);

  const stopAudio = () => {
    if (audioRef.current) {
      try {
        audioRef.current.pause();
        audioRef.current.src = "";
      } catch {
        // 忽略释放异常
      }
      audioRef.current = null;
    }
  };
  useEffect(() => () => stopAudio(), []);

  // 换模型时读一次该模型的音色表（名称、性别、语种）；读不到就保留编号输入框
  const modelId = voices ? form.tts_model : "";
  useEffect(() => {
    if (!modelId) return undefined;
    let alive = true;
    voiceApi
      .localTtsVoices(modelId)
      .then((res) => {
        if (alive) setCatalog({ modelId, speakers: Array.isArray(res?.speakers) ? res.speakers : null });
      })
      .catch(() => {
        if (alive) setCatalog({ modelId, speakers: null });
      });
    return () => {
      alive = false;
    };
  }, [modelId]);

  // 换模型、换音色或改语速后，上一次试听的结果不再对应当前草稿
  useEffect(() => {
    trialSeqRef.current += 1;
    stopAudio();
    setTrial({ state: "idle", text: "" });
  }, [form.tts_model, form.tts_speaker, form.tts_speed]);

  const speakers = catalog.modelId === modelId ? catalog.speakers : null;
  const groups = speakers ? localTtsSpeakerGroups(speakers, form.tts_speaker) : null;

  const tryVoice = async () => {
    if (trial.state === "playing") {
      trialSeqRef.current += 1;
      stopAudio();
      setTrial({ state: "idle", text: "" });
      return;
    }
    const seq = ++trialSeqRef.current;
    stopAudio();
    setTrial({ state: "loading", text: "正在按当前音色和语速合成…" });
    try {
      const speed = speedFixed ? 1 : Number(form.tts_speed);
      const res = await voiceApi.localTtsPreview(form.tts_model, voices ? Number(form.tts_speaker) : 0, speed);
      if (seq !== trialSeqRef.current) return;
      const url = res?.audio_data_url || "";
      if (!url) throw new Error("没有拿到试听音频");
      const audio = new Audio(url);
      audioRef.current = audio;
      const done = () => {
        if (audioRef.current === audio && seq === trialSeqRef.current) setTrial({ state: "idle", text: "" });
      };
      audio.onended = done;
      audio.onerror = done;
      setTrial({ state: "playing", text: `正在播放：“${res.sample_text || ""}”（${Number(res.speed ?? speed).toFixed(2).replace(/0$/, "")}x）` });
      await audio.play();
    } catch (err) {
      if (seq !== trialSeqRef.current) return;
      stopAudio();
      setTrial({ state: "error", text: `试听失败：${voiceErrorText(err)}` });
    }
  };

  const trialBlocked = !model
    ? "先在上面选一个本地合成模型"
    : !model.installed
      ? "所选合成模型还没下载"
      : localModelSettingsError({ ...form, mirror: "" }, models)
        ? "先改正音色或语速"
        : "";

  return (
    <div className="voice-form-grid" id="voice-local-tts-settings">
      <label>
        <span className="voice-label-with-link">
          <span>本地合成音色</span>
          <button type="button" className="voice-link-btn" onClick={onBrowse} id="voice-local-speaker-browse">
            {voices ? "试听选音色" : "查看语言包与试听"}
          </button>
        </span>
        {!voices ? (
          <span className="voice-muted">所选合成模型只有一个音色</span>
        ) : groups ? (
          <select className="text-input" value={String(form.tts_speaker)} onChange={(e) => update("tts_speaker", e.target.value)} id="voice-local-speaker">
            {groups.map((group) => (
              <optgroup key={group.label} label={group.label}>
                {group.options.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
        ) : (
          <>
            <input className="text-input mono" inputMode="numeric" value={form.tts_speaker} onChange={(e) => update("tts_speaker", e.target.value)} id="voice-local-speaker" />
            <small className="voice-muted">音色编号 0 ~ {voices.max}（默认 {voices.default}）</small>
          </>
        )}
      </label>
      <label>
        <span>本地合成语速（0.5 ~ 2.0）</span>
        <input className="text-input" inputMode="decimal" value={form.tts_speed} disabled={speedFixed} onChange={(e) => update("tts_speed", e.target.value)} id="voice-local-speed" />
        {speedFixed && (
          <small className="voice-muted" id="voice-local-speed-fixed">
            {model.label} 不支持调语速，固定自然语速（1.0x）；换成 Kokoro / Matcha / ZipVoice / MOSS 原声才生效
          </small>
        )}
      </label>
      <div className="voice-wide voice-local-tts-trial">
        <button
          type="button"
          className="btn btn-ghost voice-inline-btn"
          disabled={Boolean(trialBlocked) || trial.state === "loading"}
          title={trialBlocked || "用当前（未保存的）音色和语速合成一句试听"}
          onClick={tryVoice}
          id="voice-local-tts-try"
        >
          {trial.state === "playing" ? <Square size={13} /> : <Play size={13} />}
          {trial.state === "playing" ? "停止" : trial.state === "loading" ? "合成中…" : "按当前音色和语速试一句"}
        </button>
        <small className={trial.state === "error" ? "voice-local-tts-trial-bad" : "voice-muted"} id="voice-local-tts-try-note" role="status">
          {trial.text || trialBlocked || "试听不用先保存；要让对话和连贯试跑用上，改完点下方“保存”"}
        </small>
      </div>
    </div>
  );
}
