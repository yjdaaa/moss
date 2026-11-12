import React, { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AlertCircle, X } from "lucide-react";
import { trayActionCopy } from "../platform.js";
import { isTopmostModalBackdrop } from "./AppDialog.jsx";

export function CloseConfirmModal({ visible, onClose, onConfirm, voiceServerRunning = false }) {
  const [selectedAction, setSelectedAction] = useState("tray");
  const [remember, setRemember] = useState(false);
  // 技术书 §9.4.3：完全退出时询问是否同时停止 MOSS 启动的语音服务端，默认“是”
  const [stopServer, setStopServer] = useState(true);
  const backdropRef = useRef(null);
  const backdropPressRef = useRef(false);
  const trayCopy = trayActionCopy();

  useEffect(() => {
    if (!visible) return undefined;
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        if (!isTopmostModalBackdrop(backdropRef.current)) return;
        e.preventDefault();
        onClose?.();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [visible, onClose]);

  if (!visible) return null;

  const handleConfirm = () => {
    onConfirm(selectedAction, remember, stopServer);
  };

  const content = (
    <div
      ref={backdropRef}
      className="mat-modal-backdrop close-confirm-box"
      onMouseDown={(e) => {
        backdropPressRef.current = e.button === 0 && e.target === e.currentTarget;
      }}
      onMouseUp={(e) => {
        if (e.target !== e.currentTarget) backdropPressRef.current = false;
      }}
      onClick={(e) => {
        if (backdropPressRef.current && e.target === e.currentTarget) {
          onClose?.();
        }
        backdropPressRef.current = false;
      }}
    >
      <div className="mat-modal close-confirm-box-2" role="dialog" aria-modal="true" aria-labelledby="close-confirm-title">
        <div className="mat-modal-head">
          <h3 id="close-confirm-title" className="close-confirm-heading">
            <AlertCircle size={18} className="close-confirm-icon" />
            <span>关闭主窗口</span>
          </h3>
          <button type="button" className="mat-modal-close" onClick={onClose} title="取消" aria-label="取消">
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          <p className="close-confirm-note">
            您点击了关闭窗口，请选择您期望的操作：
          </p>

          <div className="close-confirm-row">
            <label
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: "10px",
                padding: "10px 12px",
                borderRadius: "8px",
                background: selectedAction === "tray" ? "rgba(139, 123, 255, 0.08)" : "rgba(255, 255, 255, 0.02)",
                border: `1px solid ${selectedAction === "tray" ? "rgba(139, 123, 255, 0.3)" : "var(--line, #242b35)"}`,
                cursor: "pointer",
                transition: "all 0.15s ease"
              }}
            >
              <input
                type="radio"
                name="closeAction"
                value="tray"
                checked={selectedAction === "tray"}
                onChange={() => setSelectedAction("tray")}
                className="close-confirm-field"
              />
              <div>
                <div className="close-confirm-box-3">
                  {trayCopy.title}
                </div>
                <div className="close-confirm-box-4">
                  {trayCopy.modalDetail}
                </div>
              </div>
            </label>

            <label
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: "10px",
                padding: "10px 12px",
                borderRadius: "8px",
                background: selectedAction === "exit" ? "rgba(239, 68, 68, 0.08)" : "rgba(255, 255, 255, 0.02)",
                border: `1px solid ${selectedAction === "exit" ? "rgba(239, 68, 68, 0.3)" : "var(--line, #242b35)"}`,
                cursor: "pointer",
                transition: "all 0.15s ease"
              }}
            >
              <input
                type="radio"
                name="closeAction"
                value="exit"
                checked={selectedAction === "exit"}
                onChange={() => setSelectedAction("exit")}
                className="close-confirm-field"
              />
              <div>
                <div className="close-confirm-box-3">
                  完全退出程序
                </div>
                <div className="close-confirm-box-4">
                  停止激光加工任务，触发安全急停软复位并彻底杀死进程
                </div>
              </div>
            </label>
          </div>

          {voiceServerRunning && selectedAction === "exit" && (
            <label className="close-voice-option" id="close-stop-voice-server">
              <input type="checkbox" checked={stopServer} onChange={(e) => setStopServer(e.target.checked)} className="close-confirm-field-2" />
              <span>同时停止语音服务端（由 MOSS 启动的 xiaozhi-server）。取消勾选则退出后服务端继续运行，其输出不再记录。</span>
            </label>
          )}

          <div className="close-confirm-box-5">
            <label className="close-confirm-label">
              <input
                type="checkbox"
                checked={remember}
                onChange={(e) => setRemember(e.target.checked)}
              />
              <span>记住我的选择，以后不再询问（可在右上角设置中更改）</span>
            </label>
          </div>
        </div>

        <div className="mat-modal-foot">
          <button type="button" className="btn btn-ghost" onClick={onClose}>
            取消
          </button>
          <button
            type="button"
            className={`btn ${selectedAction === "exit" ? "btn-danger" : "btn-primary"}`}
            onClick={handleConfirm}
          >
            确定
          </button>
        </div>
      </div>
    </div>
  );
  return typeof document !== "undefined" && document.body
    ? createPortal(content, document.body)
    : content;
}
