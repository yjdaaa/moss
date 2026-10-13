import React, { useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, Info, ShieldAlert, X } from "lucide-react";
import { resolveDialog, subscribe } from "../appDialog.js";

const TONE_META = {
  info: { Icon: Info, label: "INFO" },
  success: { Icon: CheckCircle2, label: "OK" },
  warning: { Icon: AlertTriangle, label: "WARN" },
  error: { Icon: ShieldAlert, label: "ERROR" },
};

/**
 * 全局应用内对话框宿主。挂载一次即可；由 appDialog.js 的
 * showAlert / showConfirm 驱动，样式复用 mat-modal 体系。
 */
export function AppDialogHost() {
  const [dialog, setDialog] = useState(null);
  const confirmRef = useRef(null);

  useEffect(() => subscribe(setDialog), []);

  useEffect(() => {
    if (!dialog) return undefined;
    const timer = window.setTimeout(() => confirmRef.current?.focus(), 30);
    const onKey = (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        // Esc 等同关闭：confirm 视为取消；choice 返回 "dismiss"
        resolveDialog(dialog.id, null);
      } else if (e.key === "Enter") {
        e.preventDefault();
        resolveDialog(dialog.id, true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("keydown", onKey);
    };
  }, [dialog]);

  if (!dialog) return null;

  const meta = TONE_META[dialog.tone] || TONE_META.info;
  const { Icon } = meta;
  const isConfirm = dialog.kind === "confirm" || dialog.kind === "choice";
  const confirmClass = dialog.danger
    ? "btn btn-danger"
    : dialog.tone === "error"
      ? "btn btn-danger"
      : "btn btn-primary";

  return (
    <div className="mat-modal-backdrop app-dialog-backdrop app-dialog-ext-box">
      <div
        className={`mat-modal app-dialog app-dialog--${dialog.tone}`}
        role={isConfirm ? "alertdialog" : "dialog"}
        aria-modal="true"
        aria-labelledby={`app-dialog-title-${dialog.id}`}
        data-testid="app-dialog"
      >
        <div className="mat-modal-head app-dialog-head">
          <h3 id={`app-dialog-title-${dialog.id}`} className="app-dialog-title">
            <span className="app-dialog-icon">
              <Icon size={18} />
            </span>
            <span className="app-dialog-title-text">{dialog.title}</span>
            <span className="app-dialog-tag">{meta.label}</span>
          </h3>
          <button
            type="button"
            className="mat-modal-close"
            onClick={() => resolveDialog(dialog.id, null)}
            title={isConfirm ? "取消" : "关闭"}
          >
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body app-dialog-body">
          <p className="app-dialog-message">{dialog.message}</p>
        </div>

        <div className="mat-modal-foot app-dialog-foot">
          {isConfirm && (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => resolveDialog(dialog.id, false)}
            >
              {dialog.cancelText}
            </button>
          )}
          <button
            ref={confirmRef}
            type="button"
            className={confirmClass}
            onClick={() => resolveDialog(dialog.id, true)}
          >
            {dialog.confirmText}
          </button>
        </div>
      </div>
    </div>
  );
}
