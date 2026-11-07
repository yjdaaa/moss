import React, { useEffect, useRef, useState } from "react";
import { Check, MessageSquare, RefreshCw } from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { showConfirm } from "../appDialog.js";
import { feedbackSourceLabel } from "../feedbackView.js";

const labels = {
  too_dark: "过焦或过深", too_light: "过浅", not_cut_through: "未切透",
  rough_edge: "毛边", broken_line: "断线", acceptable: "效果正常", unknown: "需要补充反馈",
};

function paramsText(params) {
  if (!params) return "未采用";
  return `S${params.laser_max_power} / ${params.power_scale} · F${params.feed_rate} mm/min · ${params.passes} 次`;
}

export function FeedbackPanel({ workflowId, origin = null, disabled = false, onAdopt }) {
  const [text, setText] = useState("");
  const [history, setHistory] = useState([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const generation = useRef(0);

  useEffect(() => {
    const token = ++generation.current;
    setHistory([]);
    setText("");
    setError("");
    setBusy(Boolean(workflowId));
    if (workflowId) {
      getJson(`/api/feedback/history?workflow_id=${encodeURIComponent(workflowId)}`).then((res) => {
        if (generation.current !== token) return;
        if (res.ok && res.data?.success) setHistory(res.data.result.feedback_history || []);
        else setError(res.data?.result || "历史读取失败");
        setBusy(false);
      });
    }
    return () => { generation.current++; };
  }, [workflowId]);

  async function submit(event) {
    event.preventDefault();
    if (busy || disabled || !text.trim()) return;
    const token = generation.current;
    setBusy(true);
    setError("");
    const res = await postJson("/api/feedback", { workflow_id: workflowId, feedback_text: text.trim() });
    if (generation.current !== token) return;
    if (res.ok && res.data?.success) {
      setHistory((items) => [...items, res.data.result]);
      setText("");
    } else setError(res.data?.result || "反馈失败");
    setBusy(false);
  }

  async function adopt(entry) {
    const token = generation.current;
    setBusy(true);
    setError("");
    try {
      const confirmed = await showConfirm(`新预览参数：${paramsText(entry.suggested_params)}`, {
        title: "采用建议并生成新预览", confirmText: "生成预览",
      });
      if (!confirmed || token !== generation.current) return;
      const res = await postJson("/api/feedback/accept", {
        workflow_id: workflowId, feedback_id: entry.feedback_id, confirmed: true,
      });
      if (token !== generation.current) return;
      if (!res.ok || !res.data?.success) throw new Error(res.data?.result || "采用失败");
      setHistory((items) => items.map((item) => item.feedback_id === entry.feedback_id ? res.data.result : item));
      const preview = await postJson("/api/workflow", {
        action: "status", workflow_id: res.data.result.adopted_workflow_id,
      });
      if (token !== generation.current) return;
      if (!preview.ok || !preview.data?.success) throw new Error("新预览读取失败，可从历史记录重试打开");
      await onAdopt(preview.data.result);
    } catch (e) {
      if (token === generation.current) setError(e.message);
    } finally {
      if (token === generation.current) setBusy(false);
    }
  }

  if (!workflowId) return null;
  return (
    <section className="feedback-panel" aria-label="加工反馈">
      <h3><MessageSquare size={16} />加工反馈</h3>
      <form onSubmit={submit}>
        <textarea aria-label="加工效果反馈" value={text} maxLength={2000}
          onChange={(event) => setText(event.target.value)} disabled={busy || disabled}
          placeholder="例如：文字太浅，边缘清晰" rows={3} />
        <button className="btn btn-ghost" disabled={busy || disabled || !text.trim()}>
          <MessageSquare size={15} />{busy ? "处理中..." : "提交反馈"}
        </button>
      </form>
      {error && <p role="alert">{String(error)}</p>}
      <div className="feedback-history">
        {[...history].reverse().concat(origin ? [origin] : []).map((entry) => (
          <details key={entry.feedback_id} open>
            <summary>{labels[entry.feedback_type] || "未知"} · {feedbackSourceLabel(entry)}</summary>
            <p>{entry.feedback_text}</p>
            {entry.fallback_reason && <p className="feedback-muted">{entry.fallback_reason}</p>}
            <dl>
              <dt>原参数</dt><dd>{paramsText(entry.original_params)}</dd>
              <dt>建议参数</dt><dd>{entry.suggested_params ? paramsText(entry.suggested_params) : "不调整"}</dd>
              <dt>已采用</dt><dd>{paramsText(entry.adopted_params)}</dd>
            </dl>
            {entry.decision?.confidence != null && <p className="feedback-muted">判断置信度 {Math.round(entry.decision.confidence * 100)}%</p>}
            {entry.can_adopt && entry.workflow_id === workflowId && (
              <button type="button" className="btn btn-ghost" disabled={busy || disabled} onClick={() => adopt(entry)}>
                {entry.adopted_params ? <Check size={15} /> : <RefreshCw size={15} />}
                {entry.adopted_params ? "打开已生成预览" : "采用并生成新预览"}
              </button>
            )}
          </details>
        ))}
      </div>
    </section>
  );
}
