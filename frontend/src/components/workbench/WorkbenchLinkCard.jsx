import React from "react";
import { Settings2 } from "lucide-react";
import { connectionSummaryView } from "../../deviceConnection.js";
import { FoldHeader } from "../FoldHeader.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";

/**
 * 雕刻工作台 ① 通信链路卡（2026-11-23 r58 精简为紧凑状态摘要）：
 * 展示全站共用的连接方式、目标地址与最近握手状态；点击「修改连接」打开统一的设备连接面板。
 */
export function WorkbenchLinkCard({
  connectionForm,
  connectionStatus,
  onOpenConnection,
}) {
  const view = connectionSummaryView(connectionForm, connectionStatus);
  return (
    <WbCard id="link" className="hud-card ob-card" label="通信链路">
      <FoldHeader id="wb-link" title="通信链路" n={1} />
      <div className="conn-summary-card" id="wb-link-summary">
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
          id="wb-open-connection"
          className="btn btn-ghost wb-btn conn-summary-btn"
          onClick={onOpenConnection}
        >
          <Settings2 size={13} className="wb-icon" aria-hidden="true" />
          <span>修改连接</span>
        </button>
      </div>
    </WbCard>
  );
}
