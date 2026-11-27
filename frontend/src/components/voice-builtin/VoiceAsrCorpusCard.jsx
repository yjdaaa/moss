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
  const [audioUrls, setAudioUrls] = useState({});
  const [feedback, setFeedback] = useState("");

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
      } catch (error) {
        await showAlert(voiceErrorText(error), { title: "开启识别语料采集失败", tone: "error" });
      } finally {
        setBusy(false);
      }
      return;
    }

    let deleteExisting = false;
    if (view.totalCount > 0) {
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
      setFeedback(
        deleteExisting
          ? `已关闭采集并删除 ${res?.deleted_count ?? 0} 条已采集语料`
          : "已关闭识别录音语料采集（已有语料暂保留在本机）",
      );
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
      setFeedback(`已保存标注：${item.turn_id}`);
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
      setFeedback(`已清空 ${res?.cleared ?? 0} 条识别语料`);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "清空语料失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const filteredItems = view.items.filter((item) => {
    if (filter === "labeled") return Boolean(item.label);
    if (filter === "unlabeled") return !item.label;
    return true;
  });

  const content = (
    <>
      <div className="voice-inline-actions" style={{ justifyContent: "space-between", flexWrap: "wrap", gap: "8px" }}>
        <label className="voice-check">
          <input
            type="checkbox"
            id="voice-asr-corpus-switch"
            checked={view.enabled}
            disabled={busy}
            onChange={(e) => toggleCapture(e.target.checked)}
          />
          <span>保存最近的识别录音用于测评</span>
        </label>
        <div className="voice-inline-actions" style={{ gap: "6px", flexWrap: "wrap" }}>
          <label className="voice-muted" style={{ display: "inline-flex", alignItems: "center", gap: "4px" }}>
            <span>上限条数</span>
            <input
              type="number"
              min={1}
              max={5000}
              id="voice-asr-corpus-max-items"
              className="text-input mono"
              style={{ width: "76px" }}
              value={maxItemsInput}
              disabled={busy}
              onChange={(e) => setMaxItemsInput(e.target.value)}
            />
          </label>
          <label className="voice-muted" style={{ display: "inline-flex", alignItems: "center", gap: "4px" }}>
            <span>天数</span>
            <input
              type="number"
              min={1}
              max={90}
              id="voice-asr-corpus-max-days"
              className="text-input mono"
              style={{ width: "64px" }}
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

      <p className="voice-muted" style={{ marginTop: "6px" }}>
        仅存本机 <span className="mono">{`<运行目录>/${view.corpusDirRel}/<日期>/`}</span>（16 kHz 单声道 WAV，先达条数或天数上限即自动清理），不进诊断包与过程记录，不用于声纹识别或身份授权；模拟联调轮次不保存。
      </p>

      {feedback && (
        <div className="voice-hint voice-hint--info" id="voice-asr-corpus-feedback">
          {feedback}
        </div>
      )}

      {!view.panelUsable ? (
        <p className="voice-muted" id="voice-asr-corpus-disabled-note">
          采集开关默认关闭。开启后可在此试听真实录音、校对“正确文字”并打标签，供离线测评脚本自动评估各识别模型。
        </p>
      ) : (
        <div id="voice-asr-corpus-panel" style={{ marginTop: "10px" }}>
          <div className="voice-inline-actions" style={{ justifyContent: "space-between", marginBottom: "8px", flexWrap: "wrap", gap: "6px" }}>
            <div className="voice-inline-actions" style={{ gap: "6px" }}>
              {[
                ["all", `全部 (${view.items.length})`],
                ["unlabeled", `未标注 (${view.items.filter((i) => !i.label).length})`],
                ["labeled", `已标注 (${view.labeledCount})`],
              ].map(([key, label]) => (
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
            <div className="voice-stack" style={{ gap: "10px" }}>
              {filteredItems.map((item) => {
                const key = itemKey(item);
                const draft = getDraft(item);
                const audioSrc = audioUrls[key];
                const isLabeled = Boolean(item.label);
                return (
                  <div key={key} className="voice-debug-row" id={`voice-asr-corpus-row-${item.turn_id}`}>
                    <div className="voice-debug-head" style={{ justifyContent: "space-between", flexWrap: "wrap", gap: "6px" }}>
                      <div>
                        <span className="mono">{formatTime(Math.floor((item.captured_at_ms || 0) / 1000))}</span>
                        {" · "}
                        <span>{item.asr_model || "未知模型"}</span>
                        {" · "}
                        <span className="mono">{((item.duration_ms || 0) / 1000).toFixed(1)}s</span>
                        {isLabeled ? (
                          <span className="ob-pill ob-pill--ok" style={{ marginLeft: "6px" }}>
                            已标注
                          </span>
                        ) : (
                          <span className="ob-pill ob-pill--off" style={{ marginLeft: "6px" }}>
                            未标注
                          </span>
                        )}
                      </div>
                      <div className="voice-inline-actions" style={{ gap: "6px" }}>
                        <button
                          type="button"
                          className="btn btn-ghost"
                          disabled={busy}
                          onClick={() => playAudio(item)}
                        >
                          {audioSrc ? "重新加载录音" : "▶ 播放录音"}
                        </button>
                        <button
                          type="button"
                          className="btn btn-ghost"
                          disabled={busy}
                          onClick={() => deleteItem(item)}
                        >
                          删除
                        </button>
                      </div>
                    </div>

                    {audioSrc && (
                      <audio controls src={audioSrc} style={{ width: "100%", height: "32px", marginTop: "6px" }} />
                    )}

                    <div style={{ marginTop: "6px" }}>
                      <div className="voice-muted">
                        识别结果：<strong style={{ color: "var(--text)" }}>{item.raw_text || "（空）"}</strong>
                        {item.corrected_text && item.corrected_text !== item.raw_text ? (
                          <span>（纠偏后：{item.corrected_text}）</span>
                        ) : null}
                      </div>
                    </div>

                    <div className="voice-inline-actions" style={{ marginTop: "6px", gap: "6px", flexWrap: "wrap" }}>
                      <input
                        type="text"
                        className="text-input"
                        style={{ flex: "1 1 240px" }}
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
                        onClick={() => saveLabel(item, draft.text, draft.tags, draft.text.trim() === String(item.raw_text || "").trim())}
                      >
                        保存标注
                      </button>
                    </div>

                    <div className="voice-inline-actions" style={{ marginTop: "6px", gap: "10px", flexWrap: "wrap" }}>
                      {ASR_CORPUS_TAGS.map((tag) => (
                        <label key={tag} className="voice-check" style={{ fontSize: "12px" }}>
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
