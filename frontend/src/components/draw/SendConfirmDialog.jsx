import React, { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { Send, Wifi, X } from "lucide-react";
import { isTopmostModalBackdrop } from "../AppDialog.jsx";
import { OriginBadge, inferOriginKind } from "../OriginBadge.jsx";
import { useModalFocusTrap } from "../useModalFocusTrap.js";
import { JobSummaryCard } from "./JobSummaryCard.jsx";

// 创意画板发送确认框。原样搬自 pages/DrawPage.jsx（2026-11-16 按职责拆分）

export function SendConfirmDialog({
  summary,
  originNote = "",
  target,
  sdAvailable = true,
  busy,
  sendAllowed = false,
  onCancel,
  onConfirm,
  requireMovedAck = false,
  movedAcknowledged = false,
  onMovedAckChange,
}) {
  // 默认走在线流式发送（与雕刻工作台一致），避免未插 SD 卡或固件未开脱机文件支持时默认脱机直接报错失败
  const [offlineSdChoice, setOfflineSd] = React.useState(false);
  const backdropRef = useRef(null);
  const dialogRef = useRef(null);
  const cancelBtnRef = useRef(null);
  const backdropPressRef = useRef(false);
  // SD 脱机只能经网口上传；串口只能逐行发送
  const offlineSd = sdAvailable && offlineSdChoice;
  const canConfirm = !requireMovedAck || movedAcknowledged;

  useModalFocusTrap({ backdropRef, dialogRef, initialFocusRef: cancelBtnRef });

  useEffect(() => {
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        if (!isTopmostModalBackdrop(backdropRef.current)) return;
        e.preventDefault();
        if (busy) return;
        onCancel?.();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onCancel]);

  const content = (
    <div
      ref={backdropRef}
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(e) => {
        backdropPressRef.current = e.button === 0 && e.target === e.currentTarget;
      }}
      onMouseUp={(e) => {
        if (e.target !== e.currentTarget) backdropPressRef.current = false;
      }}
      onClick={(e) => {
        if (backdropPressRef.current && e.target === e.currentTarget && !busy) {
          onCancel?.();
        }
        backdropPressRef.current = false;
      }}
    >
      <section ref={dialogRef} className="send-confirm-dialog" role="dialog" aria-modal="true" aria-labelledby="send-confirm-title">
        <div className="confirm-heading">
          <div>
            <Send size={18} />
            <h2 id="send-confirm-title">确认开始雕刻</h2>
          </div>
          <button type="button" className="icon-button" onClick={onCancel} aria-label="关闭确认面板" disabled={busy}>
            <X size={18} />
          </button>
        </div>
        <JobSummaryCard summary={summary} compact />
        <div className="wb-origin-note wb-origin-inline" id="draw-confirm-origin">
          <OriginBadge origin={inferOriginKind(originNote)} />
          <span>对刀起点：{originNote || "开刻前请把激光头对准作品左下角"}</span>
        </div>
        <div className="confirm-device">
          <Wifi size={17} />
          <span>设备：{target}</span>
        </div>
        {sdAvailable ? (
          <label className="checkbox-row confirm-moved-ack draw-label">
            <input
              type="checkbox"
              checked={offlineSd}
              onChange={(e) => setOfflineSd(e.target.checked)}
            />
            改用 SD 卡脱机离线加工（需机器插卡并开启脱机文件支持）
          </label>
        ) : null}
        {requireMovedAck ? (
          <label className="checkbox-row confirm-moved-ack">
            <input
              type="checkbox"
              checked={Boolean(movedAcknowledged)}
              onChange={(event) => onMovedAckChange?.(event.target.checked)}
            />
            已移动材料或原点（相同图案再次发送前必须勾选）
          </label>
        ) : null}
        <div className="confirm-actions">
          <button ref={cancelBtnRef} type="button" className="secondary-button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="danger-button"
            onClick={() => onConfirm(offlineSd)}
            disabled={busy || !target || !sendAllowed || !canConfirm}
          >
            <Send size={18} />
            {busy ? "发送中..." : (offlineSd ? "离线脱机发送" : "在线流式发送")}
          </button>
        </div>
      </section>
    </div>
  );
  return typeof document !== "undefined" && document.body
    ? createPortal(content, document.body)
    : content;
}
