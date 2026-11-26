import React, { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AlertCircle, X } from "lucide-react";
import { trayActionCopy } from "../platform.js";
import { isTopmostModalBackdrop } from "./AppDialog.jsx";
import { useModalFocusTrap } from "./useModalFocusTrap.js";

export function CloseConfirmModal({ visible, onClose, onConfirm }) {
  // 关闭即卸载内层状态，防止取消后残留“完全退出+记住选择”在下次打开时误杀进程
  if (!visible) return null;
  return <CloseConfirmModalBody onClose={onClose} onConfirm={onConfirm} />;
}

function CloseConfirmModalBody({ onClose, onConfirm }) {
  const [selectedAction, setSelectedAction] = useState("tray");
  const [remember, setRemember] = useState(false);
  const backdropRef = useRef(null);
  const dialogRef = useRef(null);
  const backdropPressRef = useRef(false);
  const trayCopy = trayActionCopy();

  useModalFocusTrap({ backdropRef, dialogRef });

  useEffect(() => {
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
  }, [onClose]);

  const handleConfirm = () => {
    onConfirm(selectedAction, remember);
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
      <div ref={dialogRef} className="mat-modal close-confirm-box-2" role="dialog" aria-modal="true" aria-labelledby="close-confirm-title">
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
            请选择关闭主窗口时的操作：
          </p>

          <div className="close-confirm-row">
            <label className={`close-confirm-option${selectedAction === "tray" ? " is-active" : ""}`}>
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

            <label className={`close-confirm-option is-danger${selectedAction === "exit" ? " is-active" : ""}`}>
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
                  停止激光任务与语音服务并直接退出程序
                </div>
              </div>
            </label>
          </div>

          <div className="close-confirm-box-5">
            <label className="close-confirm-label">
              <input
                type="checkbox"
                checked={remember}
                onChange={(e) => setRemember(e.target.checked)}
              />
              <span>记住我的选择，以后不再询问（可在左侧栏底部的「偏好设置」中更改）</span>
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
