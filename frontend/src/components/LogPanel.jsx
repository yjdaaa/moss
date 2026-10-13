import React from "react";
import { Terminal, Trash2 } from "lucide-react";

export function LogPanel({ logs = [], onClear }) {
  return (
    <div className="log-console">
      <div className="log-head log-panel-row">
        <div className="log-panel-row-2">
          <Terminal size={15} />
          <span>MOSS · 事件流</span>
        </div>
        {onClear && (
          <button
            className="btn btn-ghost log-panel-btn"
            onClick={onClear}
            title="清空日志"
          >
            <Trash2 size={12} className="log-panel-icon" />
            清空
          </button>
        )}
      </div>

      <div className="log-body log-panel-box" id="log-body">
        {logs.length === 0 ? (
          <div className="log-empty log-panel-box-2">
            暂无事件记录 · 操作工作台后实时日志将在此呈现
          </div>
        ) : (
          logs.map((item, idx) => (
            <div key={idx} className={`log-item log-${item.level || "info"} log-panel-row-3`}>
              <span className="log-time log-panel-text">
                {item.time}
              </span>
              <span className={`log-badge badge-${item.level || "info"} log-panel-text-2`}>
                {item.tag || "系统"}
              </span>
              <span className="log-msg log-panel-text-3">
                {item.message}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
