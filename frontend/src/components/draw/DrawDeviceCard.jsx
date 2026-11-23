import React from "react";
import { Settings2 } from "lucide-react";
import { connectionSummaryView } from "../../deviceConnection.js";
import { FoldHeader } from "../FoldHeader.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";

/**
 * 创意画板 ② 设备卡（2026-11-23 r58 精简为紧凑状态摘要）：
 * 与雕刻工作台、顶栏硬件胶囊共用同一份设备连接设置与状态；点「修改连接」打开统一设备连接面板。
 */
export function DrawDeviceCard({
  form,
  connectionStatus,
  onOpenConnection,
  busy = false,
}) {
  const view = connectionSummaryView(form, connectionStatus);
  return (
    <WbCard id="draw-device" className="hud-card ob-card control-panel" label="设备">
      <FoldHeader id="draw-device" title="设备" n={2} />
      <div className="conn-summary-card" id="draw-device-summary">
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
          id="draw-open-connection"
          className="btn btn-ghost wb-btn conn-summary-btn"
          onClick={onOpenConnection}
          disabled={busy}
        >
          <Settings2 size={14} className="wb-icon" aria-hidden="true" />
          <span>修改连接</span>
        </button>
      </div>
    </WbCard>
  );
}
