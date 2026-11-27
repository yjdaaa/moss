import React, { useCallback, useEffect, useState } from "react";
import { showAlert, showChoice, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup } from "../VoiceLayout.jsx";
import {
  ASR_CORPUS_CLEAR_ALL_CONFIRM_OPTIONS,
  ASR_CORPUS_CLEAR_ALL_CONFIRM_TEXT,
  ASR_CORPUS_DELETE_ITEM_CONFIRM_OPTIONS,
  ASR_CORPUS_DELETE_ITEM_CONFIRM_TEXT,
  ASR_CORPUS_DISABLE_CHOICE_OPTIONS,
  ASR_CORPUS_DISABLE_CHOICE_TEXT,
  ASR_CORPUS_ENABLE_CONFIRM_OPTIONS,
  ASR_CORPUS_ENABLE_CONFIRM_TEXT,
  ASR_CORPUS_TAGS,
  asrCorpusItemSourceView,
  asrCorpusTextDiffView,
  asrCorpusView,
  buildAsrCorpusLabelPayload,
  toggleAsrCorpusTag,
  voiceErrorText,
} from "../../voiceState.js";
import { formatTime } from "./shared.js";

/** VB1/VB2 识别（ASR）真实录音语料采集与人工标注面板（仅受信任主窗口，默认关闭；VD6 起可内嵌到“AI 联调与测试”分组）。 */
export function VoiceAsrCorpusCard({ builtin, active = true, embedded = false }) {
  const initialSummary = builtin?.status?.asr_corpus;
  const [data, setData] = useState(() => ({
    enabled: Boolean(initialSummary?.enabled),
    max_items: initialSummary?.max_items || 500,
    max_days: initialSummary?.max_days || 7,
    corpus_dir_rel: initialSummary?.corpus_dir_rel || "asr-corpus",
    total_count: 0,
    labeled_count: 0,
    items: [],
  }));
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [filter, setFilter] = useState("all");
  const [maxItemsInput, setMaxItemsInput] = useState(String(initialSummary?.max_items || 500));
  const [maxDaysInput, setMaxDaysInput] = useState(String(initialSummary?.max_days || 7));
  const [drafts, setDrafts] = useState({});
  const [editingMap, setEditingMap] = useState({});
  const [audioUrls, setAudioUrls] = useState({});
  const [feedback, setFeedback] = useState("");

  const statusSummary = builtin?.status?.asr_corpus;
  const view = asrCorpusView(data);

  const applyPayload = useCallback((res) => {
    if (!res || typeof res !== "object") return;
    setData(res);
    setMaxItemsInput(String(res.max_items || 500));
    setMaxDaysInput(String(res.max_days || 7));
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const res = await voiceApi.asrCorpusManage({ action: "list" });
      applyPayload(res);
    } catch (error) {
      setFeedback(`读取识别语料失败：${voiceErrorText(error)}`);
    } finally {
      setLoading(false);
    }
  }, [applyPayload]);

  useEffect(() => {
    if (active) refresh();
  }, [active, refresh]);

  const statusTotalCount = Number(statusSummary?.total_count) || 0;
  const statusLabeledCount = Number(statusSummary?.labeled_count) || 0;
  const prevStatusCountsRef = React.useRef({ total: statusTotalCount, labeled: statusLabeledCount });
  useEffect(() => {
    const prev = prevStatusCountsRef.current;
    prevStatusCountsRef.current = { total: statusTotalCount, labeled: statusLabeledCount };
    if (!active) return;
    if (statusTotalCount !== prev.total || statusLabeledCount !== prev.labeled) {
      void refresh();
    }
  }, [active, statusTotalCount, statusLabeledCount, refresh]);

  const itemKey = (item) => `${item.date}/${item.turn_id}`;

  const getDraft = (item) => {
    const key = itemKey(item);
    if (drafts[key]) return drafts[key];
    return {
      text: item.label?.reference_text ?? item.raw_text ?? "",
      tags: Array.isArray(item.label?.tags) ? item.label.tags : [],
    };
  };

  const updateDraft = (item, patch) => {
    const key = itemKey(item);
    setDrafts((prev) => {
      const base = prev[key] || {
        text: item.label?.reference_text ?? item.raw_text ?? "",
        tags: Array.isArray(item.label?.tags) ? item.label.tags : [],
      };
      return { ...prev, [key]: { ...base, ...patch } };
    });
  };

  const toggleCapture = async (checked) => {
    setFeedback("");
    if (checked) {
      const ok = await showConfirm(ASR_CORPUS_ENABLE_CONFIRM_TEXT, ASR_CORPUS_ENABLE_CONFIRM_OPTIONS);
      if (!ok) return;
      setBusy(true);
      try {
        const res = await voiceApi.asrCorpusManage({
          action: "set_config",
          enabled: true,
          confirmed: true,
        });
        applyPayload(res);
        setFeedback("已开启识别录音语料采集（仅存本机，不进诊断包）");
        await builtin?.refresh?.();
      } catch (error) {
        await showAlert(voiceErrorText(error), { title: "开启识别语料采集失败", tone: "error" });
      } finally {
        setBusy(false);
      }
      return;
    }

    let latestCount = view.totalCount;
    if (latestCount === 0) {
      try {
        const fresh = await voiceApi.asrCorpusManage({ action: "status" });
        if (fresh && typeof fresh === "object") {
          applyPayload(fresh);
          latestCount = Number(fresh.total_count) || 0;
        }
      } catch {
        // 状态探测失败时退回本地计数
      }
    }
    let deleteExisting = false;
    if (latestCount > 0) {
      const choice = await showChoice(ASR_CORPUS_DISABLE_CHOICE_TEXT, ASR_CORPUS_DISABLE_CHOICE_OPTIONS);
      if (choice === "dismiss") return;
      deleteExisting = choice === "confirm";
    }
    setBusy(true);
    try {
      const res = await voiceApi.asrCorpusManage({
        action: "set_config",
        enabled: false,
        delete_existing: deleteExisting,
      });
      applyPayload(res);
      setAudioUrls({});
      setDrafts({});
      setEditingMap({});
      setFeedback(
        deleteExisting
          ? `已关闭采集并删除 ${res?.deleted_count ?? 0} 条已采集语料`
          : "已关闭识别录音语料采集（已有语料暂保留在本机）",
      );
      await builtin?.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "关闭识别语料采集失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const saveRetention = async () => {
    const maxItems = Number.parseInt(maxItemsInput, 10);
    const maxDays = Number.parseInt(maxDaysInput, 10);
    if (!Number.isFinite(maxItems) || maxItems < 1 || maxItems > 5000) {
      await showAlert("最多保留条数须为 1 ~ 5000 的整数", { title: "参数无效", tone: "warning" });
      return;
    }
    if (!Number.isFinite(maxDays) || maxDays < 1 || maxDays > 90) {
      await showAlert("最多保留天数须为 1 ~ 90 的整数", { title: "参数无效", tone: "warning" });
      return;
    }
    setBusy(true);
    setFeedback("");
    try {
      const res = await voiceApi.asrCorpusManage({
        action: "set_config",
        enabled: view.enabled,
        max_items: maxItems,
        max_days: maxDays,
      });
      applyPayload(res);
      setFeedback(`已更新保留上限：最多 ${maxItems} 条 / ${maxDays} 天`);
      await builtin?.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "保存上限失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const playAudio = async (item) => {
    const key = itemKey(item);
    setBusy(true);
    try {
      const res = await voiceApi.asrCorpusManage({
        action: "audio",
        date: item.date,
        turn_id: item.turn_id,
      });
      if (res?.audio_data_url) {
        setAudioUrls((prev) => ({ ...prev, [key]: res.audio_data_url }));
      }
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "读取录音失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const saveLabel = async (item, referenceText, tags, verifiedCorrect) => {
    const built = buildAsrCorpusLabelPayload(item, referenceText, tags, verifiedCorrect);
    if (built.error) {
      await showAlert(built.error, { title: "无法保存标注", tone: "warning" });
      return;
    }
    const key = itemKey(item);
    setBusy(true);
    setFeedback("");
    try {
      const res = await voiceApi.asrCorpusManage(built.payload);
      setData((prev) => ({
        ...prev,
        total_count: res?.total_count ?? prev.total_count,
        labeled_count: res?.labeled_count ?? prev.labeled_count,
        items: (prev.items || []).map((row) =>
          row.date === item.date && row.turn_id === item.turn_id
            ? { ...row, label: res.label }
            : row,
        ),
      }));
      updateDraft(item, { text: res.label.reference_text, tags: res.label.tags || [] });
      setEditingMap((prev) => ({ ...prev, [key]: false }));
      setFeedback(`已保存标注：${item.turn_id}`);
      await builtin?.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "保存标注失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const deleteItem = async (item) => {
    const ok = await showConfirm(ASR_CORPUS_DELETE_ITEM_CONFIRM_TEXT, ASR_CORPUS_DELETE_ITEM_CONFIRM_OPTIONS);
    if (!ok) return;
    setBusy(true);
    setFeedback("");
    try {
      const res = await voiceApi.asrCorpusManage({
        action: "delete",
        date: item.date,
        turn_id: item.turn_id,
      });
      applyPayload(res);
      setFeedback(`已删除语料：${item.turn_id}`);
      await builtin?.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "删除语料失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const clearAll = async () => {
    const ok = await showConfirm(ASR_CORPUS_CLEAR_ALL_CONFIRM_TEXT, ASR_CORPUS_CLEAR_ALL_CONFIRM_OPTIONS);
    if (!ok) return;
    setBusy(true);
    setFeedback("");
    try {
      const res = await voiceApi.asrCorpusManage({ action: "clear", confirmed: true });
      applyPayload(res);
      setAudioUrls({});
      setDrafts({});
      setEditingMap({});
      setFeedback(`已清空 ${res?.cleared ?? 0} 条识别语料`);
      await builtin?.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "清空语料失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const unlabeledCount = view.items.filter((i) => !i.label).length;
  const playbackCount = view.items.filter((i) => asrCorpusItemSourceView(i).isPlayback).length;
  const manualCount = view.items.filter((i) => {
    const s = asrCorpusItemSourceView(i);
    return s.isLabeled && !s.isPlayback;
  }).length;

  const filteredItems = view.items.filter((item) => {
    const src = asrCorpusItemSourceView(item);
    if (filter === "labeled") return src.isLabeled;
    if (filter === "unlabeled") return !src.isLabeled;
    if (filter === "manual") return src.kind === "manual";
    if (filter === "playback") return src.kind === "playback";
    return true;
  });

  const filterOptions = [
    ["all", `全部 (${view.items.length})`],
    ["unlabeled", `未标注 (${unlabeledCount})`],
    ["labeled", `已标注 (${view.labeledCount})`],
    ...(manualCount > 0 ? [["manual", `人工原声 (${manualCount})`]] : []),
    ...(playbackCount > 0 ? [["playback", `外放回采 (${playbackCount})`]] : []),
  ];

  const content = (
    <>
      <div className="ai-bench-corpus-toolbar">
        <label className="voice-check">
          <input
            type="checkbox"
            id="voice-asr-corpus-switch"
            checked={view.enabled}
            disabled={busy}
            onChange={(e) => toggleCapture(e.target.checked)}
          />
          <span>保存最近的识别录音用于测评（存 <code className="mono">{`<运行目录>/${view.corpusDirRel}/`}</code>，模拟轮次不保存）</span>
        </label>
        <div className="ai-bench-corpus-limits">
          <label className="voice-muted ai-bench-limit-field">
            <span>上限条数</span>
            <input
              type="number"
              min={1}
              max={5000}
              id="voice-asr-corpus-max-items"
              className="text-input mono"
              value={maxItemsInput}
              disabled={busy}
              onChange={(e) => setMaxItemsInput(e.target.value)}
            />
          </label>
          <label className="voice-muted ai-bench-limit-field">
            <span>天数</span>
            <input
              type="number"
              min={1}
              max={90}
              id="voice-asr-corpus-max-days"
              className="text-input mono"
              value={maxDaysInput}
              disabled={busy}
              onChange={(e) => setMaxDaysInput(e.target.value)}
            />
          </label>
          <button type="button" className="btn btn-ghost" id="voice-asr-corpus-save-limits" disabled={busy} onClick={saveRetention}>
            保存上限
          </button>
          {view.totalCount > 0 && (
            <button type="button" className="btn btn-ghost" id="voice-asr-corpus-clear" disabled={busy} onClick={clearAll}>
              清空全部（{view.totalCount}）
            </button>
          )}
        </div>
      </div>

      {feedback && (
        <div className="voice-hint voice-hint--info" id="voice-asr-corpus-feedback">
          {feedback}
        </div>
      )}

      {!view.panelUsable ? (
        <div>
          <p className="voice-muted" id="voice-asr-corpus-disabled-note">
            采集开关默认关闭。开启后可在此试听真实录音、校对“正确文字”并打标签，供离线测评脚本自动评估各识别模型。
          </p>
          {view.retainedItems.length > 0 && (
            <div className="voice-stack" id="voice-asr-corpus-retained-list" style={{ marginTop: "8px" }}>
              <p className="voice-muted">
                当前采集已关闭，以下为本机暂存的历史语料（仅允许单独删除或点上方“清空全部”，开启采集后才可试听与标注）：
              </p>
              {view.retainedItems.map((item) => (
                <div
                  key={itemKey(item)}
                  className="voice-debug-row ai-bench-corpus-row is-compact"
                  id={`voice-asr-corpus-retained-${item.turn_id}`}
                >
                  <div className="ai-bench-corpus-row-head">
                    <div className="ai-bench-corpus-meta">
                      <span className="mono">{formatTime(Math.floor((item.captured_at_ms || 0) / 1000))}</span>
                      {" · "}
                      <span className="mono">{item.date}/{item.turn_id}</span>
                      {" · "}
                      <span className="mono">{((item.duration_ms || 0) / 1000).toFixed(1)}s</span>
                      <span className={`ob-pill ${item.labeled ? "ob-pill--ok" : "ob-pill--off"}`}>
                        {item.labeled ? "已标注" : "未标注"}
                      </span>
                    </div>
                    <button
                      type="button"
                      className="btn btn-ghost voice-inline-btn"
                      disabled={busy}
                      onClick={() => deleteItem(item)}
                    >
                      删除
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      ) : (
        <div id="voice-asr-corpus-panel">
          <div className="ai-bench-corpus-filterbar">
            <div className="voice-inline-actions">
              {filterOptions.map(([key, label]) => (
                <button
                  key={key}
                  type="button"
                  className={filter === key ? "btn btn-primary" : "btn btn-ghost"}
                  onClick={() => setFilter(key)}
                >
                  {label}
                </button>
              ))}
            </div>
            <button type="button" className="btn btn-ghost" id="voice-asr-corpus-refresh" disabled={loading || busy} onClick={refresh}>
              {loading ? "读取中…" : "刷新列表"}
            </button>
          </div>

          {filteredItems.length === 0 ? (
            <p className="voice-muted">暂无匹配的识别录音语料。唤醒小智说几句话后点“刷新列表”。</p>
          ) : (
            <div className="voice-stack">
              {filteredItems.map((item) => {
                const key = itemKey(item);
                const draft = getDraft(item);
                const audioSrc = audioUrls[key];
                const sourceView = asrCorpusItemSourceView(item);
                const diff = asrCorpusTextDiffView(item);
                const isEditing = !sourceView.isLabeled || Boolean(editingMap[key]);
                return (
                  <div
                    key={key}
                    className={`voice-debug-row ai-bench-corpus-row${sourceView.isLabeled && !isEditing ? " is-compact" : ""}`}
                    id={`voice-asr-corpus-row-${item.turn_id}`}
                  >
                    <div className="ai-bench-corpus-row-head">
                      <div className="ai-bench-corpus-meta">
                        <span className="mono">{formatTime(Math.floor((item.captured_at_ms || 0) / 1000))}</span>
                        {" · "}
                        <span>{item.asr_model || "未知模型"}</span>
                        {" · "}
                        <span className="mono">{((item.duration_ms || 0) / 1000).toFixed(1)}s</span>
                        <span className={`ob-pill ob-pill--${sourceView.tone}`}>
                          {sourceView.badgeText}
                        </span>
                        {diff.correctedRescued && (
                          <span className="ob-pill ob-pill--ok">同音纠偏已命中</span>
                        )}
                        {Array.isArray(item.label?.tags) &&
                          item.label.tags.map((t) => (
                            <span key={t} className="ob-pill ob-pill--off">
                              {t}
                            </span>
                          ))}
                      </div>
                      <div className="voice-inline-actions">
                        <button
                          type="button"
                          className="btn btn-ghost voice-inline-btn"
                          disabled={busy}
                          onClick={() => playAudio(item)}
                        >
                          {audioSrc ? "重新加载录音" : "▶ 播放录音"}
                        </button>
                        {sourceView.isLabeled && (
                          <button
                            type="button"
                            className="btn btn-ghost voice-inline-btn"
                            disabled={busy}
                            onClick={() => setEditingMap((prev) => ({ ...prev, [key]: !isEditing }))}
                          >
                            {isEditing ? "收起编辑" : "修改标注"}
                          </button>
                        )}
                        <button
                          type="button"
                          className="btn btn-ghost voice-inline-btn"
                          disabled={busy}
                          onClick={() => deleteItem(item)}
                        >
                          删除
                        </button>
                      </div>
                    </div>

                    {audioSrc && (
                      <audio controls src={audioSrc} className="ai-bench-audio-player" />
                    )}

                    <div className="ai-bench-corpus-diff">
                      <span className="voice-muted">
                        识别结果：<strong>{diff.rawText || "（空）"}</strong>
                        {diff.wasCorrected ? (
                          <span> → 纠偏后：<strong>{diff.correctedText}</strong></span>
                        ) : null}
                      </span>
                      {sourceView.isLabeled && (
                        <span className="voice-muted">
                          {" · "}正确文字：
                          <strong className={diff.matchesReference ? "voice-ok" : "voice-warn"}>
                            {diff.referenceText}
                          </strong>
                        </span>
                      )}
                    </div>

                    {isEditing && (
                      <div className="ai-bench-corpus-editor">
                        <div className="ai-bench-corpus-input-row">
                          <input
                            type="text"
                            className="text-input"
                            placeholder="输入或修改正确文字"
                            aria-label="正确文字"
                            value={draft.text}
                            disabled={busy}
                            onChange={(e) => updateDraft(item, { text: e.target.value })}
                          />
                          <button
                            type="button"
                            className="btn btn-ghost"
                            disabled={busy || !String(item.raw_text || "").trim()}
                            onClick={() => saveLabel(item, item.raw_text, draft.tags, true)}
                          >
                            ✓ 正确
                          </button>
                          <button
                            type="button"
                            className="btn btn-primary"
                            disabled={busy || !String(draft.text || "").trim()}
                            onClick={() =>
                              saveLabel(
                                item,
                                draft.text,
                                draft.tags,
                                draft.text.trim() === String(item.raw_text || "").trim(),
                              )
                            }
                          >
                            保存标注
                          </button>
                        </div>

                        <div className="ai-bench-corpus-tags">
                          {ASR_CORPUS_TAGS.map((tag) => (
                            <label key={tag} className="voice-check">
                              <input
                                type="checkbox"
                                checked={draft.tags.includes(tag)}
                                disabled={busy}
                                onChange={() => updateDraft(item, { tags: toggleAsrCorpusTag(draft.tags, tag) })}
                              />
                              <span>{tag}</span>
                            </label>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </>
  );

  if (embedded) {
    return (
      <div className="voice-block" id="voice-asr-corpus-section">
        <div className="voice-block-h">
          <h3>识别语料（离线 ASR 测评）</h3>
          <span className={`ob-pill ${view.enabled ? "ob-pill--ok" : "ob-pill--off"}`}>{view.summary}</span>
        </div>
        {content}
      </div>
    );
  }

  return (
    <VoiceGroup id="asr-corpus" title="识别语料" summary={view.summary}>
      {content}
    </VoiceGroup>
  );
}
