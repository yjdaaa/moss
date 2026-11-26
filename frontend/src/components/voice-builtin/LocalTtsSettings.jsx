import React, { useCallback, useEffect, useRef, useState } from "react";
import { Play, Square, Upload, Trash2, Check } from "lucide-react";
import { voiceApi } from "../../voiceApi.js";
import {
  QWEN3_TTS_1_7B_SPEED_OPTIONS,
  isAudioCppTtsModel,
  localModelSettingsError,
  localTtsSpeedFixed,
  localTtsSpeakerGroups,
  normalizeQwen3Tts17bSpeed,
  voiceErrorText,
} from "../../voiceState.js";

const MAX_CLONE_WAV_BYTES = 7 * 1024 * 1024;

function readFileAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ""));
    reader.onerror = () => reject(new Error("读取本地录音文件失败"));
    reader.readAsDataURL(file);
  });
}

/**
 * 5. 语音合成模型包下方的本地合成音色、离线克隆参考录音与语速（2026-11-25）。
 * - 多音色模型按名称下拉选择（读不到时退回编号框）；单音色模型隐藏音色栏（n129）；
 * - 离线声音克隆模型（`qwen3_tts_1_7b_clone`，n131）就近提供参考 WAV 上传、参考原话编辑与试听，不显示预设语言包弹窗；
 * - 不支持调语速的模型不显示语速框（n126）。
 */
export function LocalTtsSettings({ form, update, models, voices, local = null, onStatus = null, onBrowse }) {
  const model = models.find((item) => item.kind === "tts" && item.id === form.tts_model) || null;
  const isCloneModel = isAudioCppTtsModel(model);
  const speedFixed = localTtsSpeedFixed(model);
  const cloneRef = local?.tts_clone_ref || null;
  const cloneConfigured = Boolean(cloneRef?.configured);
  const savedCloneTranscript = String(cloneRef?.transcript || "");

  const [catalog, setCatalog] = useState({ modelId: "", speakers: null });
  const [trial, setTrial] = useState({ state: "idle", text: "" });
  const [cloneTranscript, setCloneTranscript] = useState(savedCloneTranscript);
  const [cloneBusy, setCloneBusy] = useState("");
  const [cloneNotice, setCloneNotice] = useState(null);

  const audioRef = useRef(null);
  const trialSeqRef = useRef(0);
  const cloneFileRef = useRef(null);

  const stopAudio = useCallback(() => {
    if (audioRef.current) {
      try {
        audioRef.current.pause();
        audioRef.current.src = "";
      } catch {
        // 忽略释放异常
      }
      audioRef.current = null;
    }
  }, []);
  useEffect(() => () => stopAudio(), [stopAudio]);

  // 后端参考录音代次或原话变化时同步输入框（含同名文件重传）
  useEffect(() => {
    setCloneTranscript(savedCloneTranscript);
  }, [cloneConfigured, cloneRef?.generation, savedCloneTranscript]);

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

  const cancelPendingTrial = useCallback(() => {
    trialSeqRef.current += 1;
    stopAudio();
    setTrial({ state: "idle", text: "" });
  }, [stopAudio]);

  // 切换合成模型时清空克隆操作提示；改音色或语速时仅重置上一次试听状态
  useEffect(() => {
    setCloneNotice(null);
  }, [form.tts_model]);

  useEffect(() => {
    cancelPendingTrial();
  }, [form.tts_model, form.tts_speaker, form.tts_speed, cancelPendingTrial]);

  const speakers = catalog.modelId === modelId ? catalog.speakers : null;
  const groups = speakers ? localTtsSpeakerGroups(speakers, form.tts_speaker) : null;
  const transcriptDirty = cloneConfigured && cloneTranscript.trim() !== savedCloneTranscript.trim();

  const handleCloneUpload = async (event) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file || cloneBusy) return;
    if (file.size > MAX_CLONE_WAV_BYTES) {
      setCloneNotice({ tone: "bad", text: "参考录音超过 7 MB，请截取 5 ~ 20 秒片段后再上传" });
      return;
    }
    cancelPendingTrial();
    setCloneBusy("uploading");
    setCloneNotice(null);
    try {
      const dataUrl = await readFileAsDataUrl(file);
      // 首次上传若用户已先填了原话则沿用；更换录音时传空以自动识别新录音原话
      const initialTranscript = !cloneConfigured && cloneTranscript.trim() ? cloneTranscript.trim() : "";
      const res = await voiceApi.localTtsCloneManage({
        action: "upload",
        file_name: file.name || "reference.wav",
        audio_base64: dataUrl,
        transcript: initialTranscript,
      });
      if (res?.status) onStatus?.(res.status);
      setCloneTranscript(String(res?.tts_clone_ref?.transcript || ""));
      setCloneNotice({ tone: "ok", text: res?.detail || "已保存参考录音" });
    } catch (err) {
      setCloneNotice({ tone: "bad", text: `上传失败：${voiceErrorText(err)}` });
    } finally {
      setCloneBusy("");
    }
  };

  const handleSaveCloneTranscript = async () => {
    if (!cloneConfigured || !transcriptDirty || cloneBusy) return;
    cancelPendingTrial();
    setCloneBusy("saving");
    setCloneNotice(null);
    try {
      const res = await voiceApi.localTtsCloneManage({
        action: "update_transcript",
        transcript: cloneTranscript,
      });
      if (res?.status) onStatus?.(res.status);
      setCloneTranscript(String(res?.tts_clone_ref?.transcript || ""));
      setCloneNotice({ tone: "ok", text: res?.detail || "已更新参考原话" });
    } catch (err) {
      setCloneNotice({ tone: "bad", text: `保存失败：${voiceErrorText(err)}` });
    } finally {
      setCloneBusy("");
    }
  };

  const handleClearClone = async () => {
    if (!cloneConfigured || cloneBusy) return;
    cancelPendingTrial();
    setCloneBusy("clearing");
    setCloneNotice(null);
    try {
      const res = await voiceApi.localTtsCloneManage({ action: "clear" });
      if (res?.status) onStatus?.(res.status);
      setCloneTranscript("");
      setCloneNotice({ tone: "ok", text: res?.detail || "已移除参考录音" });
    } catch (err) {
      setCloneNotice({ tone: "bad", text: `移除失败：${voiceErrorText(err)}` });
    } finally {
      setCloneBusy("");
    }
  };

  const tryVoice = async () => {
    if (trial.state === "playing") {
      trialSeqRef.current += 1;
      stopAudio();
      setTrial({ state: "idle", text: "" });
      return;
    }
    const seq = ++trialSeqRef.current;
    stopAudio();
    setTrial({
      state: "loading",
      text: isCloneModel ? "正在按参考录音合成试听（首次加载模型约需数秒）…" : speedFixed ? "正在按当前音色合成…" : "正在按当前音色和语速合成…",
    });
    try {
      const speed = speedFixed
        ? 1
        : Number(form.tts_model === "qwen3_tts_1_7b" ? normalizeQwen3Tts17bSpeed(form.tts_speed) : form.tts_speed);
      const res = await voiceApi.localTtsPreview(
        form.tts_model,
        voices ? Number(form.tts_speaker) : 0,
        speed,
        isCloneModel ? cloneTranscript : undefined,
      );
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
      const rate = speedFixed ? "" : `（${Number(res.speed ?? speed).toFixed(2).replace(/0$/, "")}x）`;
      setTrial({ state: "playing", text: `正在播放：“${res.sample_text || ""}”${rate}` });
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
      : isCloneModel && !cloneConfigured
        ? "先在上方上传一段 3 ~ 30 秒参考 WAV 录音"
        : localModelSettingsError({ ...form, mirror: "" }, models)
          ? "先改正音色或语速"
          : "";

  const tryButtonLabel =
    trial.state === "playing"
      ? "停止"
      : trial.state === "loading"
        ? "合成中…"
        : isCloneModel
          ? "按当前参考音色试一句"
          : speedFixed
            ? "按当前音色试一句"
            : "按当前音色和语速试一句";

  return (
    <div className="voice-form-grid" id="voice-local-tts-settings">
      {isCloneModel && (
        <>
          <div className="voice-wide" id="voice-local-clone-box">
            <div className="voice-label-with-link">
              <span>参考录音（3 ~ 30 秒清晰干声 WAV，须使用自有或获授权音频）</span>
              <span className="voice-actions">
                <input
                  ref={cloneFileRef}
                  type="file"
                  accept=".wav,audio/wav,audio/x-wav"
                  hidden
                  onChange={handleCloneUpload}
                  id="voice-local-clone-file"
                />
                <button
                  type="button"
                  className="btn btn-ghost voice-inline-btn"
                  disabled={Boolean(cloneBusy)}
                  onClick={() => cloneFileRef.current?.click()}
                  id="voice-local-clone-upload-btn"
                >
                  <Upload size={13} />
                  {cloneBusy === "uploading" ? "处理中…" : cloneConfigured ? "更换参考 WAV" : "上传参考 WAV"}
                </button>
                {cloneConfigured && (
                  <button
                    type="button"
                    className="btn btn-ghost voice-inline-btn"
                    disabled={Boolean(cloneBusy)}
                    onClick={handleClearClone}
                    id="voice-local-clone-clear-btn"
                  >
                    <Trash2 size={13} /> 移除
                  </button>
                )}
              </span>
            </div>
            <small className="voice-muted" id="voice-local-clone-summary">
              {cloneConfigured
                ? `当前参考录音：${cloneRef.file_name || "reference.wav"}（${Number(cloneRef.duration_secs || 0).toFixed(1)} 秒 · ${Math.round((Number(cloneRef.sample_rate) || 0) / 100) / 10} kHz · ${cloneRef.mode === "x_vector_only" ? "声纹模式" : "台词对齐模式"}）`
                : "尚未上传：支持 16/24/32 位标准 WAV（≥ 16 kHz，≤ 7 MB），音频只留在本机电脑"}
            </small>
          </div>
          <label className="voice-wide">
            <span className="voice-label-with-link">
              <span>参考录音原话（填准原话克隆最像；留空只提取声纹）</span>
              {transcriptDirty && (
                <button
                  type="button"
                  className="voice-link-btn"
                  disabled={Boolean(cloneBusy)}
                  onClick={handleSaveCloneTranscript}
                  id="voice-local-clone-save-transcript"
                >
                  <Check size={12} /> {cloneBusy === "saving" ? "保存中…" : "保存原话"}
                </button>
              )}
            </span>
            <input
              className="text-input"
              value={cloneTranscript}
              disabled={Boolean(cloneBusy)}
              placeholder="上传 WAV 时若本机已安装本地语音识别会自动填入，也可手动输入或校对录音里说的那句话"
              onChange={(e) => setCloneTranscript(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.nativeEvent.isComposing && transcriptDirty) {
                  e.preventDefault();
                  handleSaveCloneTranscript();
                }
              }}
              id="voice-local-clone-transcript"
            />
            {cloneNotice && (
              <small
                className={cloneNotice.tone === "bad" ? "voice-local-tts-trial-bad" : "voice-muted"}
                id="voice-local-clone-notice"
                role="status"
              >
                {cloneNotice.text}
              </small>
            )}
          </label>
        </>
      )}
      {voices && (
        <label>
          <span className="voice-label-with-link">
            <span>本地合成音色</span>
            <button type="button" className="voice-link-btn" onClick={onBrowse} id="voice-local-speaker-browse">
              试听选音色
            </button>
          </span>
          {groups ? (
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
      )}
      {!speedFixed && (
        <label>
          <span>{form.tts_model === "qwen3_tts_1_7b" ? "本地合成语速（说话指令档位）" : "本地合成语速（0.5 ~ 2.0）"}</span>
          {form.tts_model === "qwen3_tts_1_7b" ? (
            <select
              className="text-input"
              value={normalizeQwen3Tts17bSpeed(form.tts_speed)}
              onChange={(e) => update("tts_speed", e.target.value)}
              id="voice-local-speed"
            >
              {QWEN3_TTS_1_7B_SPEED_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
          ) : (
            <input className="text-input" inputMode="decimal" value={form.tts_speed} onChange={(e) => update("tts_speed", e.target.value)} id="voice-local-speed" />
          )}
        </label>
      )}
      <div className="voice-wide voice-local-tts-trial">
        <button
          type="button"
          className="btn btn-ghost voice-inline-btn"
          disabled={Boolean(trialBlocked) || trial.state === "loading" || Boolean(cloneBusy)}
          title={trialBlocked || (isCloneModel ? "用当前参考录音和原话合成一句试听" : speedFixed ? "用当前（未保存的）音色合成一句试听" : "用当前（未保存的）音色和语速合成一句试听")}
          onClick={tryVoice}
          id="voice-local-tts-try"
        >
          {trial.state === "playing" ? <Square size={13} /> : <Play size={13} />}
          {tryButtonLabel}
        </button>
        {!voices && !isCloneModel && (
          <button type="button" className="voice-link-btn" onClick={onBrowse} id="voice-local-speaker-browse">
            查看语言包与试听
          </button>
        )}
        <small className={trial.state === "error" ? "voice-local-tts-trial-bad" : "voice-muted"} id="voice-local-tts-try-note" role="status">
          {trial.text ||
            trialBlocked ||
            (isCloneModel
              ? transcriptDirty
                ? "试听会直接用上方新改的原话；确认满意点“保存原话”即可让对话立即生效"
                : "参考录音上传后已自动保存；切换合成模型后记得点下方“保存”"
              : "试听不用先保存；要让对话和连贯试跑用上，改完点下方“保存”")}
        </small>
      </div>
    </div>
  );
}
