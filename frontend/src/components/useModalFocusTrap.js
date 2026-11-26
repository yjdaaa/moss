import { useEffect } from "react";
import { isTopmostModalBackdrop } from "./AppDialog.jsx";

export const MODAL_FOCUSABLE_SELECTOR =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * 在当前顶层模态框内处理 Tab / Shift+Tab 焦点循环（纯逻辑，便于单测）。
 * 返回将被聚焦的目标元素（若无需拦截则返回 null）。
 */
export function resolveModalTabWrap({ dialogEl, activeElement, shiftKey = false }) {
  if (!dialogEl || typeof dialogEl.querySelectorAll !== "function") return null;
  const focusables = Array.from(dialogEl.querySelectorAll(MODAL_FOCUSABLE_SELECTOR));
  if (focusables.length === 0) return null;
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  const contains = typeof dialogEl.contains === "function" ? dialogEl.contains(activeElement) : focusables.includes(activeElement);
  if (!contains) {
    return shiftKey ? last : first;
  }
  if (shiftKey && activeElement === first) {
    return last;
  }
  if (!shiftKey && activeElement === last) {
    return first;
  }
  return null;
}

/**
 * 模态弹窗焦点管理：
 * 1. 打开时记录触发前焦点，并将焦点移入 initialFocusRef（或弹窗内首个可聚焦控件）；
 * 2. 按 Tab / Shift+Tab 时若当前弹窗位于最顶层，将焦点锁定在弹窗内部循环；
 * 3. 关闭卸载时将焦点还原给打开前的触发控件。
 */
export function useModalFocusTrap({ backdropRef, dialogRef, initialFocusRef = null }) {
  useEffect(() => {
    if (typeof document === "undefined" || typeof window === "undefined") return undefined;
    const previousFocus = document.activeElement;
    const focusTimer = window.setTimeout(() => {
      const preferred = initialFocusRef?.current;
      if (preferred && !preferred.disabled && typeof preferred.focus === "function") {
        preferred.focus();
        return;
      }
      const first = dialogRef?.current?.querySelector?.(MODAL_FOCUSABLE_SELECTOR);
      if (first && typeof first.focus === "function") {
        first.focus();
      }
    }, 0);
    return () => {
      window.clearTimeout(focusTimer);
      if (
        previousFocus
        && typeof previousFocus.focus === "function"
        && typeof document.contains === "function"
        && document.contains(previousFocus)
      ) {
        previousFocus.focus();
      }
    };
    // 仅在弹窗挂载与卸载时捕获/还原焦点
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (typeof window === "undefined") return undefined;
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key !== "Tab" || !dialogRef?.current || !isTopmostModalBackdrop(backdropRef?.current)) return;
      const target = resolveModalTabWrap({
        dialogEl: dialogRef.current,
        activeElement: typeof document !== "undefined" ? document.activeElement : null,
        shiftKey: Boolean(e.shiftKey),
      });
      if (target && typeof target.focus === "function") {
        e.preventDefault();
        target.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [backdropRef, dialogRef]);
}
