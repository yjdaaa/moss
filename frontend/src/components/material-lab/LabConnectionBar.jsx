import React from "react";
import { Settings2 } from "lucide-react";
import { connectionSummaryView } from "../../deviceConnection.js";

/**
 * 材料实验室 ③ 校准矩阵 · 设备连接摘要条（与工作台、创作画板、顶栏硬件胶囊共用同一份设备连接设置）：
 * 点击「修改连接」打开全局统一的 `DeviceConnectionModal`（不再在页面内枚举覆盖串口或维护独立波特率列表）。
 */
export function LabConnectionBar({
  connectionForm,
  connectionStatus,
  onOpenConnection,
  disabled = false,
}) {
  const view = connectionSummaryView(connectionForm, connectionStatus);
  return (
    <div className="conn-summary-card lab-conn-summary" id="lab-conn-summary">
      <div className="conn-summary-info">
        <span className={`conn-summary-dot is-${view.statusTone}`} aria-hidden="true" />
        <span className="conn-summary-mode">{view.modeLabel}</span>
        <span className="conn-summary-sep" aria-hidden="true">·</span>
        <span className={`conn-summary-target mono${view.targetReady ? "" : " is-empty"}`}>{view.targetText}</span>
        <span className="conn-summary-sep" aria-hidden="true">·</span>
        <span className={`conn-summary-status is-${view.statusTone}`}>{view.statusText}</span>
      </div>
      <button
        type="button"
        id="lab-open-connection"
        className="btn btn-ghost wb-btn conn-summary-btn"
        onClick={onOpenConnection}
        disabled={disabled}
      >
        <Settings2 size={13} className="wb-icon" aria-hidden="true" />
        <span>修改连接</span>
      </button>
    </div>
  );
}
