import React, { useRef, useState } from "react";
import { History } from "lucide-react";
import { postJson } from "../../api/client.js";
import { recentTaskLabel, recentTaskRecall } from "../../workbenchParams.js";

/**
 * 雕刻工作台 ② 加工内容 · 最近任务（2026-11-26）：展开时读一次 `/api/workflow {action:"recent"}`，
 * 选一条后把文字、字高、刻法、外框、材料、厚度与这次实际用的功率 / 速度 / 遍数回填到左侧表单。
 * 不重发旧刀路：回填后要重新计算加工轨迹，确认、SHA-256 与严格模式审批等开刻门禁照常。
 */
export function WorkbenchRecentTasks({ onRecall, disabled = false }) {
  const [items, setItems] = useState([]);
  const [selected, setSelected] = useState("");
  const [state, setState] = useState("idle"); // idle | loading | ready | error
  const [message, setMessage] = useState("");
  const loadSeq = useRef(0);

  const load = async () => {
    const seq = ++loadSeq.current;
    setState("loading");
    setMessage("");
    try {
      const res = await postJson("/api/workflow", { action: "recent" });
      if (seq !== loadSeq.current) return;
      const list = Array.isArray(res.data?.result?.workflows) ? res.data.result.workflows : [];
      if (!res.ok || !res.data?.success) {
        setState("error");
        setMessage(res.data?.result || `读取失败（HTTP ${res.status}）`);
        return;
      }
      const usable = list
        .filter((wf) => wf?.workflow_id && recentTaskRecall(wf))
        .slice(0, 20);
      setItems(usable);
      setSelected(usable[0]?.workflow_id || "");
      setState("ready");
    } catch (error) {
      if (seq !== loadSeq.current) return;
      setState("error");
      setMessage(error?.message || String(error));
    }
  };

  const chosen = items.find((wf) => wf.workflow_id === selected) || null;
  return (
    <details
      className="fill-options wb-recent-tasks"
      id="wb-recent-tasks"
      onToggle={(event) => {
        if (event.currentTarget.open) void load();
      }}
    >
      <summary>
        <History size={13} aria-hidden="true" /> 最近任务
        <small>选一条把参数填回来，再重新计算轨迹</small>
      </summary>
      <div className="wb-recent-body">
        {state === "loading" ? <div className="wb-params-hint">正在读取最近任务…</div> : null}
        {state === "error" ? <div className="wb-params-hint" role="alert">读取最近任务失败：{message}</div> : null}
        {state === "ready" && items.length === 0 ? <div className="wb-params-hint">还没有可回填的任务</div> : null}
        {state === "ready" && items.length > 0 ? (
          <div className="wb-recent-row">
            <select
              id="wb-recent-select"
              className="select-input"
              value={selected}
              onChange={(e) => setSelected(e.target.value)}
              aria-label="最近任务"
            >
              {items.map((wf) => (
                <option key={wf.workflow_id} value={wf.workflow_id}>{recentTaskLabel(wf)}</option>
              ))}
            </select>
            <button
              type="button"
              id="wb-recent-recall"
              className="btn btn-ghost wb-sec-btn"
              disabled={disabled || !chosen}
              onClick={() => chosen && onRecall?.(recentTaskRecall(chosen), chosen)}
            >
              回填参数
            </button>
          </div>
        ) : null}
      </div>
    </details>
  );
}
