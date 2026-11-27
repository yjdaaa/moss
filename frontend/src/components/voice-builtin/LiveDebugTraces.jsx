import React, { useState } from "react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup } from "../VoiceLayout.jsx";
import {
  isAsrHotwordsSupported,
  localSettingsForm,
  voiceErrorText,
  voiceInputSourceView,
  voiceOutcomeLabel,
  VOICE_TIMING_STAGES,
  voiceTimingReport,
} from "../../voiceState.js";
import { formatTime } from "./shared.js";

/** 默认开启并落盘保存（本机 SQLite 最多保留 500 条）的语音对话记录与排障面板，联动完整过程记录区分 4 种音源并支持试听与顺手打标测评真值。 */
export function LiveDebugTraces({ builtin }) {
  const status = builtin?.status;
  // 保存期间先显示目标状态，避免受控复选框在后端返回前弹回；保存结束（成功或失败）后回到后端真实值。
  const [pendingEnabled, setPendingEnabled] = useState(null);
  const [pendingTraceEnabled, setPendingTraceEnabled] = useState(null);
  const enabled = pendingEnabled ?? Boolean(status?.debug_enabled ?? status?.local?.settings?.chat_log_enabled ?? true);
  const processTraceEnabled = pendingTraceEnabled ?? Boolean(status?.process_trace_enabled);
  const traces = Array.isArray(status?.debug_traces) ? status.debug_traces : [];
  const devices = Array.isArray(status?.devices) ? status.devices : [];
  const hotwordsSupported = isAsrHotwordsSupported(localSettingsForm(status?.local?.settings), status);
  const timingItems = voiceTimingReport(status?.timings);
  const [query, setQuery] = useState("");
  const [selectedWord, setSelectedWord] = useState("");
  const [rightWord, setRightWord] = useState("");
  const [activeTrace, setActiveTrace] = useState(null);
  const [audioUrls, setAudioUrls] = useState({});
  const [localLabels, setLocalLabels] = useState({});
  const [feedback, setFeedback] = useState("");
  const [saving, setSaving] = useState(false);

  const deviceLabel = (deviceId) => {
    if (!deviceId) return "";
    const found = devices.find((d) => d.device_id === deviceId);
    const name = String(found?.name || "").trim();
    return name ? `${name} (${deviceId})` : deviceId;
  };

  const traceKey = (trace, idx = 0) =>
    trace?.id ? `log-${trace.id}` : `${trace?.session_id || "s"}-${trace?.turn || idx}`;

  const trimmedQuery = query.trim().toLowerCase();
  const filteredTraces = trimmedQuery
    ? traces.filter((t) => {
        const srcView = voiceInputSourceView(t);
        const hay = [
          t.raw_asr,
          t.corrected_asr,
          t.reference_text,
          t.reply,
          t.spoken,
          t.device_id,
          deviceLabel(t.device_id),
          srcView.text,
          srcView.key,
          t.bench_window ? "模拟（整体测试） 整体测试 bench_window" : "",
          t.simulated ? "模拟 simulated" : "",
          ...(Array.isArray(t.tools) ? t.tools.map((x) => `${x.name} ${x.arguments} ${x.result} ${x.block_reason || ""}`) : []),
        ]
          .filter(Boolean)
          .join(" ")
          .toLowerCase();
        return hay.includes(trimmedQuery);
      })
    : traces;

  const copyTimings = async () => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(timingItems, null, 2));
      await showAlert("耗时数据已复制", { title: "已复制", tone: "success" });
    } catch {
      await showAlert("无法访问剪贴板，请检查浏览器权限", { title: "复制失败", tone: "error" });
    }
  };

  const summaryParts = [
    enabled ? `记录已开 (${traces.length} 条)` : `已暂停记录 (${traces.length} 条历史)`,
    processTraceEnabled ? "完整过程记录与录音已开" : "",
    timingItems.length ? `耗时 ${timingItems.length} 轮` : "",
  ].filter(Boolean);

  const toggleDebug = async (checked) => {
    setSaving(true);
    setPendingEnabled(checked);
    setFeedback("");
    try {
      const next = await voiceApi.localSave({ chat_log_enabled: checked, debug_trace_enabled: checked });
      if (next && builtin?.setStatus) builtin.setStatus(next);
      if (!checked) {
        setSelectedWord("");
        setRightWord("");
        setActiveTrace(null);
      }
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "切换对话记录开关失败", tone: "error" });
    } finally {
      setPendingEnabled(null);
      setSaving(false);
    }
  };

  const toggleProcessTrace = async (checked) => {
    setSaving(true);
    setPendingTraceEnabled(checked);
    setFeedback("");
    try {
      await voiceApi.asrCorpusManage({ action: "set_process_trace", enabled: checked });
      await builtin?.refresh?.();
      setFeedback(
        checked
          ? "已开启完整过程记录与原始录音（按音源打标存入 logs/process-trace/，供离线识别测评与排障复用）"
          : "已关闭完整过程记录（已有历史录音与真值标注仍保留在本机）",
      );
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "切换完整过程记录失败", tone: "error" });
    } finally {
      setPendingTraceEnabled(null);
      setSaving(false);
    }
  };

  const clearDebug = async () => {
    const ok = await showConfirm("确定清空本机保存的全部语音对话记录吗？清空后无法恢复。", {
      title: "清空对话记录",
      confirmText: "清空",
      tone: "warning",
      danger: true,
    });
    if (!ok) return;
    setSaving(true);
    try {
      const next = await voiceApi.localSave({ clear_debug_traces: true });
      if (next && builtin?.setStatus) builtin.setStatus(next);
      setSelectedWord("");
      setRightWord("");
      setActiveTrace(null);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "清空失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  const captureSelection = (trace) => {
    const raw = String(globalThis.getSelection?.()?.toString?.() || "").trim();
    if (raw && [...raw].length <= 24 && !raw.includes("\n")) {
      setSelectedWord(raw);
      setActiveTrace(trace || null);
      setFeedback("");
    }
  };

  const saveTraceGroundTruth = async (trace, referenceText, verifiedCorrect = false, silent = false) => {
    if (!trace || !trace.date || !referenceText) return;
    const srcView = voiceInputSourceView(trace);
    const key = traceKey(trace);
    try {
      await voiceApi.asrCorpusManage({
        action: "trace_label",
        date: trace.date,
        device_id: trace.device_id || "",
        session_id: trace.session_id || "",
        turn: trace.turn || 0,
        at_ms: trace.at_ms || 0,
        trace_audio_file: trace.trace_audio_file || "",
        raw_text: trace.raw_asr || "",
        corrected_text: trace.corrected_asr || trace.raw_asr || "",
        reference_text: referenceText,
        input_source: srcView.key,
        verified_correct: verifiedCorrect,
      });
      setLocalLabels((prev) => ({ ...prev, [key]: referenceText }));
      if (!silent) {
        setFeedback(`已将第 ${trace.turn} 轮标记为识别测评真值：「${referenceText}」（音源：${srcView.text}）`);
      }
    } catch (error) {
      if (!silent) {
        await showAlert(voiceErrorText(error), { title: "标记测评真值失败", tone: "error" });
      }
    }
  };

  const playTraceAudio = async (trace) => {
    if (!trace?.date) return;
    const key = traceKey(trace);
    setSaving(true);
    try {
      const res = await voiceApi.asrCorpusManage({
        action: "trace_audio",
        date: trace.date,
        device_id: trace.device_id || "",
        session_id: trace.session_id || "",
        turn: trace.turn || 0,
        at_ms: trace.at_ms || 0,
        trace_audio_file: trace.trace_audio_file || "",
      });
      if (res?.audio_data_url) {
        setAudioUrls((prev) => ({ ...prev, [key]: res.audio_data_url }));
      }
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "读取本轮录音失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  const addCorrection = async () => {
    const wrong = selectedWord.trim();
    const right = rightWord.trim();
    if (!wrong || !right) return;
    setSaving(true);
    setFeedback("");
    try {
      const current = Array.isArray(status?.local?.settings?.custom_asr_corrections)
        ? status.local.settings.custom_asr_corrections
        : [];
      const nextRules = [...current.filter((item) => !String(item).startsWith(`${wrong}=`)), `${wrong}=${right}`];
      const next = await voiceApi.localSave({ custom_asr_corrections: nextRules });
      if (next && builtin?.setStatus) builtin.setStatus(next);

      if (activeTrace?.date && activeTrace?.raw_asr) {
        const fullRef =
          activeTrace.raw_asr === wrong
            ? right
            : activeTrace.raw_asr.includes(wrong)
              ? activeTrace.raw_asr.replaceAll(wrong, right)
              : right;
        await saveTraceGroundTruth(activeTrace, fullRef, false, true);
      }
      setFeedback(`已添加同音纠偏：${wrong} → ${right}（若开启了完整过程记录，已同步写入该轮测评真值）`);
      setSelectedWord("");
      setRightWord("");
      setActiveTrace(null);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "添加同音纠偏失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  const addHotword = async (wordRaw) => {
    const word = String(wordRaw || selectedWord).trim();
    if (!word) return;
    setSaving(true);
    setFeedback("");
    try {
      const current = Array.isArray(status?.local?.settings?.custom_hotwords)
        ? status.local.settings.custom_hotwords
        : [];
      const nextWords = current.includes(word) ? current : [...current, word];
      const next = await voiceApi.localSave({ custom_hotwords: nextWords });
      if (next && builtin?.setStatus) builtin.setStatus(next);
      setFeedback(`已加入自定义热词：${word}（列表在“模型 › 对话体验”里管理）`);
      setSelectedWord("");
      setActiveTrace(null);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "添加热词失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  return (
    <VoiceGroup
      id="live-debug-traces"
      title="对话记录与排障"
      summary={summaryParts.join(" · ")}
    >
      <div className="voice-stack" id="voice-live-debug">
        <div className="voice-actions" style={{ flexWrap: "wrap", gap: "8px 14px" }}>
          <label className="voice-check">
            <input
              type="checkbox"
              checked={enabled}
              disabled={saving}
              onChange={(e) => toggleDebug(e.target.checked)}
              id="voice-debug-trace-switch"
            />
            <span>保存语音对话记录（本机最多保留最近 500 条，重启不丢失）</span>
          </label>
          <label className="voice-check">
            <input
              type="checkbox"
              checked={processTraceEnabled}
              disabled={saving}
              onChange={(e) => toggleProcessTrace(e.target.checked)}
              id="voice-process-trace-switch"
            />
            <span>同时保存完整过程记录与原始录音（按音源打标，供离线识别测评复用）</span>
          </label>
          {traces.length > 0 && (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={saving}
              onClick={clearDebug}
              id="voice-debug-trace-clear"
            >
              清空对话记录
            </button>
          )}
          {timingItems.length > 0 && (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={copyTimings}
              id="voice-timings-copy"
            >
              复制耗时数据（{timingItems.length} 轮）
            </button>
          )}
        </div>
        {traces.length > 0 && (
          <div className="voice-actions">
            <input
              className="text-input"
              type="search"
              placeholder="搜索原话、回复内容、音源类型、工具名或设备…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              id="voice-chat-log-search"
              style={{ maxWidth: 360 }}
            />
            {trimmedQuery && (
              <span className="voice-muted">
                匹配 {filteredTraces.length} / {traces.length} 条
              </span>
            )}
          </div>
        )}
        {!enabled && (
          <p className="voice-muted">当前已暂停记录新的语音对话；下方仍可查看之前已保存的历史记录。</p>
        )}
        {feedback && <div className="voice-ok">{feedback}</div>}
        {selectedWord && (
          <div className="voice-debug-selection-bar" id="voice-debug-selection-bar">
            <span>已选中识别词：<b className="mono">{selectedWord}</b></span>
            <input
              className="text-input"
              type="text"
              maxLength={24}
              placeholder="输入正确词（如 椴木）"
              value={rightWord}
              onChange={(e) => setRightWord(e.target.value)}
              id="voice-debug-right-word"
            />
            <button
              type="button"
              className="btn btn-primary"
              disabled={saving || !rightWord.trim()}
              onClick={addCorrection}
              id="voice-debug-add-correction"
            >
              加入同音纠偏
            </button>
            {hotwordsSupported && (
              <button
                type="button"
                className="btn btn-ghost"
                disabled={saving}
                onClick={() => addHotword(selectedWord)}
                id="voice-debug-add-hotword"
              >
                直接加为热词
              </button>
            )}
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => {
                setSelectedWord("");
                setActiveTrace(null);
              }}
            >
              取消
            </button>
            <span className="voice-muted voice-debug-note">
              这里顺手加一条纠偏并同步记为本轮识别测评真值；{hotwordsSupported ? "纠偏表和热词的完整列表" : "同音纠偏表"}在“模型 › 对话体验”里管理。
            </span>
          </div>
        )}
        {traces.length === 0 ? (
          <p className="voice-muted">暂无语音对话记录——唤醒小智说一句话后，你说的话、音源类型、工具调用与 MOSS 的回复会自动保存在这里（鼠标划选听错的词可一键加入纠偏表并记为测评真值）。</p>
        ) : filteredTraces.length === 0 ? (
          <p className="voice-muted">没有找到包含“{query.trim()}”的对话记录。</p>
        ) : (
          filteredTraces.map((trace, idx) => {
            const key = traceKey(trace, idx);
            const correctedChanged = trace.corrected_asr && trace.corrected_asr !== trace.raw_asr;
            const spokenDiffers = trace.spoken && trace.reply && trace.spoken !== trace.reply;
            const srcView = voiceInputSourceView(trace);
            const refText = localLabels[key] || trace.reference_text || "";
            const audioSrc = audioUrls[key];
            return (
              <div className="voice-debug-card" key={key}>
                <div className="voice-debug-head" style={{ flexWrap: "wrap" }}>
                  <b>第 {trace.turn} 轮</b>
                  <span
                    className={`ob-pill ob-pill--${srcView.tone} voice-tag`}
                    data-input-source={srcView.key}
                    title="过程记录与离线识别测评按此音源分类统计"
                  >
                    音源：{srcView.text}
                  </span>
                  {(trace.bench_window || trace.simulated) && (
                    <span
                      className={`ob-pill ${trace.bench_window ? "ob-pill--warn" : "ob-pill--info"} voice-tag`}
                      title={
                        trace.bench_window
                          ? "本轮在整体测试时间窗内执行，已锁定物理动作且不写入长期记忆"
                          : "本轮来自文字直通模拟联调，已禁止物理动作且不写入长期记忆"
                      }
                    >
                      {trace.bench_window ? "模拟（整体测试）" : "模拟"}
                    </span>
                  )}
                  <span className="voice-muted mono">{formatTime(trace.at)}</span>
                  {trace.device_id && <span className="voice-muted">{deviceLabel(trace.device_id)}</span>}
                  <span className={`ob-pill ob-pill--${voiceOutcomeLabel(trace.outcome).tone}`}>{voiceOutcomeLabel(trace.outcome).text}</span>
                  {refText && (
                    <span className="ob-pill ob-pill--ok" title="已写入过程记录真值标注，可直接用于离线识别测评">
                      测评真值：{refText}
                    </span>
                  )}
                  {trace.truncated && (
                    <span className="ob-pill ob-pill--warn" title="超长文本或过多工具调用已按上限裁剪">
                      已截断{trace.tools_dropped ? `（另有 ${trace.tools_dropped} 个工具调用未保留）` : ""}
                    </span>
                  )}
                  {trace.has_audio && (
                    <button
                      type="button"
                      className="btn btn-ghost voice-inline-btn"
                      disabled={saving}
                      onClick={() => void playTraceAudio(trace)}
                    >
                      {audioSrc ? "重新加载录音" : "▶ 听录音"}
                    </button>
                  )}
                </div>
                {audioSrc && (
                  <audio controls src={audioSrc} className="ai-bench-audio-player" />
                )}
                <div className="voice-debug-line" onMouseUp={() => captureSelection(trace)}>
                  <span className="voice-muted">听到原话（划选错词可纠偏）：</span>
                  <span className="voice-debug-asr">{trace.raw_asr || "—"}</span>
                  {trace.raw_asr && (
                    <>
                      <button
                        type="button"
                        className="btn btn-ghost voice-inline-btn"
                        onClick={() => {
                          setSelectedWord(trace.raw_asr.slice(0, 24));
                          setActiveTrace(trace);
                          setFeedback("");
                        }}
                      >
                        纠偏此句
                      </button>
                      {trace.date && srcView.key !== "text_inject" && (
                        <button
                          type="button"
                          className="btn btn-ghost voice-inline-btn"
                          disabled={saving}
                          onClick={() =>
                            void saveTraceGroundTruth(trace, trace.corrected_asr || trace.raw_asr, true)
                          }
                        >
                          ✓ 识别正确
                        </button>
                      )}
                    </>
                  )}
                </div>
                {correctedChanged && (
                  <div className="voice-debug-line">
                    <span className="voice-ok">纠偏后：</span>
                    <b>{trace.corrected_asr}</b>
                  </div>
                )}
                {Array.isArray(trace.tools) && trace.tools.length > 0 && (
                  <div className="voice-debug-tools">
                    {trace.tools.map((t, ti) => (
                      <details key={ti}>
                        <summary className="mono">
                          工具调用：{t.name}
                          {t.blocked && (
                            <span className="ob-pill ob-pill--warn voice-tag" style={{ marginLeft: 6 }}>
                              模拟门禁已拒绝
                            </span>
                          )}
                        </summary>
                        <div className="voice-muted mono">参数：{t.arguments || "{}"}</div>
                        {t.block_reason && <div className="voice-muted">拦截原因：{t.block_reason}</div>}
                        <div className="voice-muted mono">结果：{t.result || "—"}</div>
                      </details>
                    ))}
                  </div>
                )}
                {trace.reply && (
                  <div className="voice-debug-line">
                    <span className="voice-muted">MOSS 回复：</span>
                    <span>{trace.reply}</span>
                  </div>
                )}
                {spokenDiffers && (
                  <div className="voice-debug-line">
                    <span className="voice-muted">屏幕已播：</span>
                    <span>{trace.spoken}</span>
                  </div>
                )}
              </div>
            );
          })
        )}
        {timingItems.length > 0 && (
          <div id="voice-turn-timings" className="voice-stack">
            <span className="voice-muted">最近对话各阶段耗时（最近 {timingItems.length} 轮，最多保留 50 轮，服务重启不清空；从本轮开始拾音算起）：</span>
            {timingItems.map((item, index) => (
              <details key={index} open={index === 0}>
                <summary>
                  第 {item.turn} 轮 · {voiceOutcomeLabel(item.outcome).text} · {item.models.join(" / ")}{item.device_id ? ` · 设备 ${item.device_id}` : ""}
                </summary>
                <div className="voice-table-wrap">
                  <table className="voice-table">
                    <thead>
                      <tr>
                        <th>阶段</th>
                        <th>毫秒</th>
                      </tr>
                    </thead>
                    <tbody>
                      {Object.entries(item.stages).map(([stage, ms]) => (
                        <tr key={stage}>
                          <td>{VOICE_TIMING_STAGES[stage] || stage}</td>
                          <td>{ms}</td>
                        </tr>
                      ))}
                      {item.tools.map((tool, i) => (
                        <tr key={`tool-${i}`}>
                          <td>工具 {tool.name}</td>
                          <td>{tool.ms}（调用耗时）</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </details>
            ))}
          </div>
        )}
      </div>
    </VoiceGroup>
  );
}
