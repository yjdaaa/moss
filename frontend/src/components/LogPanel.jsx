import React from "react";
import { Terminal, Trash2 } from "lucide-react";

export function LogPanel({ logs = [], onClear }) {
  return (
    <div className="log-console">
      <div className="log-head" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <div style={{ display: "flex", alignItems: "center", gap: "6px" }}>
          <Terminal size={15} />
          <span>MOSS · 事件流</span>
        </div>
        {onClear && (
          <button
            className="btn btn-ghost"
            style={{ padding: "2px 8px", fontSize: "12px", height: "auto" }}
            onClick={onClear}
            title="清空日志"
          >
            <Trash2 size={12} style={{ marginRight: 4 }} />
            清空
          </button>
        )}
      </div>

      <div className="log-body" id="log-body" style={{ maxHeight: "240px", overflowY: "auto" }}>
        {logs.length === 0 ? (
          <div className="log-empty" style={{ padding: "12px", color: "var(--ink-faint, #667085)", fontSize: "12px" }}>
            暂无事件记录 · 操作工作台后实时日志将在此呈现
          </div>
        ) : (
          logs.map((item, idx) => (
            <div key={idx} className={`log-item log-${item.level || "info"}`} style={{ display: "flex", gap: "8px", padding: "4px 8px", fontSize: "12px", borderBottom: "1px solid rgba(255,255,255,0.03)" }}>
              <span className="log-time" style={{ color: "var(--ink-faint, #667085)", fontFamily: "var(--font-mono)" }}>
                {item.time}
              </span>
              <span className={`log-badge badge-${item.level || "info"}`} style={{ padding: "0 4px", borderRadius: "3px", fontSize: "10px", fontWeight: "bold" }}>
                {item.tag || "系统"}
              </span>
              <span className="log-msg" style={{ flex: 1, wordBreak: "break-all" }}>
                {item.message}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
