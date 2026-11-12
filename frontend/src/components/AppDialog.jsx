import React, { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AlertTriangle, CheckCircle2, Info, ShieldAlert, X } from "lucide-react";
import { resolveDialog, subscribe } from "../appDialog.js";

const TONE_META = {
  info: { Icon: Info, label: "INFO" },
  success: { Icon: CheckCircle2, label: "OK" },
  warning: { Icon: AlertTriangle, label: "WARN" },
  error: { Icon: ShieldAlert, label: "ERROR" },
};

/**
 * 判断给定模态框遮罩是否为当前可见模态栈的最顶层（含跨 Portal 与根层叠比较），
 * 防止多层弹窗叠加时 window 冒泡监听按 FIFO 先关掉底层弹窗。
 */
export function isTopmostModalBackdrop(backdropEl, doc = globalThis.document, win = globalThis.window) {
  if (!backdropEl || !doc) return true;
  if (doc.querySelector?.('[data-testid="app-dialog"]')) return false;
  const all = Array.from(doc.querySelectorAll?.(".mat-modal-backdrop, .modal-backdrop") || []);
  const visible = all.filter((el) => {
    const style = win?.getComputedStyle?.(el);
    if (!style) return true;
    return style.display !== "none" && style.visibility !== "hidden" && Number(style.opacity || "1") > 0;
  });
  if (visible.length === 0) return true;
  let topmost = null;
  let topZ = -Infinity;
  for (const el of visible) {
    const z = el.closest?.(".ob-main")
      ? 1
      : Number.parseInt(win?.getComputedStyle?.(el)?.zIndex ?? "0", 10) || 0;
    if (z >= topZ) {
      topZ = z;
      topmost = el;
    }
  }
  return topmost === backdropEl;
}

/**
 * 全局应用内对话框宿主。挂载一次即可；由 appDialog.js 的
 * showAlert / showConfirm 驱动，样式复用 mat-modal 体系。
 */
export function AppDialogHost() {
  const [dialog, setDialog] = useState(null);
  const [promptText, setPromptText] = useState("");
  const promptTextRef = useRef("");
  const confirmRef = useRef(null);
  const inputRef = useRef(null);

  useEffect(() => subscribe(setDialog), []);

  useEffect(() => {
    if (!dialog) return undefined;
    const initialText = dialog.kind === "prompt" ? String(dialog.defaultValue ?? "") : "";
    setPromptText(initialText);
    promptTextRef.current = initialText;
    const timer = window.setTimeout(() => {
      if (dialog.kind === "prompt") {
        inputRef.current?.focus();
        inputRef.current?.select?.();
      } else {
        confirmRef.current?.focus();
      }
    }, 30);
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        // Esc 等同关闭：confirm 视为取消；choice 返回 "dismiss"；prompt 返回 null
        resolveDialog(dialog.id, null);
      } else if (e.key === "Enter") {
        const action = e.target?.closest?.("[data-dialog-action]")?.getAttribute("data-dialog-action");
        e.preventDefault();
        e.stopPropagation();
        if (action === "dismiss") {
          resolveDialog(dialog.id, null);
        } else if (action === "cancel") {
          resolveDialog(dialog.id, dialog.kind === "prompt" ? null : false);
        } else {
          resolveDialog(dialog.id, dialog.kind === "prompt" ? promptTextRef.current : true);
        }
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [dialog]);

  if (!dialog) return null;

  const meta = TONE_META[dialog.tone] || TONE_META.info;
  const { Icon } = meta;
  const isPrompt = dialog.kind === "prompt";
  const isConfirm = dialog.kind === "confirm" || dialog.kind === "choice" || isPrompt;
  const confirmClass = dialog.danger
    ? "btn btn-danger"
    : dialog.tone === "error"
      ? "btn btn-danger"
      : "btn btn-primary";

  const content = (
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
            data-dialog-action="dismiss"
            onClick={() => resolveDialog(dialog.id, null)}
            title={isConfirm ? "取消" : "关闭"}
            aria-label={isConfirm ? "取消" : "关闭"}
          >
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body app-dialog-body">
          <p className="app-dialog-message">{dialog.message}</p>
          {isPrompt && (
            <input
              ref={inputRef}
              type="text"
              className="text-input mono"
              value={promptText}
              placeholder={dialog.placeholder || ""}
              onChange={(e) => {
                setPromptText(e.target.value);
                promptTextRef.current = e.target.value;
              }}
              data-testid="app-dialog-input"
            />
          )}
        </div>

        <div className="mat-modal-foot app-dialog-foot">
          {isConfirm && (
            <button
              type="button"
              className="btn btn-ghost"
              data-dialog-action="cancel"
              onClick={() => resolveDialog(dialog.id, isPrompt ? null : false)}
            >
              {dialog.cancelText}
            </button>
          )}
          <button
            ref={confirmRef}
            type="button"
            className={confirmClass}
            data-dialog-action="confirm"
            onClick={() => resolveDialog(dialog.id, isPrompt ? promptText : true)}
          >
            {dialog.confirmText}
          </button>
        </div>
      </div>
    </div>
  );
  return typeof document !== "undefined" && document.body
    ? createPortal(content, document.body)
    : content;
}
