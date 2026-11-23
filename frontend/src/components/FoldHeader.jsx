import React, { useEffect, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";
import { readFoldOpen, writeFoldOpen } from "../foldState.js";

/**
 * 可折叠标题（卡片与设置分区共用）。点击标题收起 / 展开同一容器里标题以外的内容；
 * 内容只是隐藏、不卸载，所以收起不会丢失已填写的内容或中断进行中的任务。
 * 默认按 id 记住收起状态；传入 persist={false} 时仅在当次会话内生效（如工作台无刀路时默认收起的加工反馈）。
 *
 * variant="card"：卡片标题（带步骤编号 n）
 * variant="section"：偏好设置等弹窗里的分区标题（带图标 icon 与右侧当前值摘要 summary）
 */
export function FoldHeader({
  id,
  title,
  n,
  icon: Icon,
  summary = "",
  variant = "card",
  defaultOpen = true,
  persist = true,
  autoExpandKey = "",
}) {
  const [open, setOpen] = useState(() => (persist ? readFoldOpen(id, defaultOpen) : Boolean(defaultOpen)));
  const prevAutoKeyRef = useRef(autoExpandKey);
  useEffect(() => {
    if (autoExpandKey && autoExpandKey !== prevAutoKeyRef.current) {
      setOpen(true);
    }
    prevAutoKeyRef.current = autoExpandKey;
  }, [autoExpandKey]);
  const toggle = () => {
    setOpen((current) => {
      if (persist) writeFoldOpen(id, !current);
      return !current;
    });
  };
  const rootClass = variant === "section" ? "ob-fold-h" : "ob-card-h";
  return (
    <div className={rootClass} data-open={open ? "true" : "false"}>
      <button
        type="button"
        className="ob-fold-toggle"
        onClick={toggle}
        aria-expanded={open}
        title={open ? "收起" : "展开"}
      >
        {Icon ? <Icon size={16} className="ob-fold-icon" aria-hidden="true" /> : null}
        {n ? <span className="ob-n">{n}</span> : null}
        <span className="ob-fold-title">{title}</span>
        <span className="ob-fold-spacer" />
        {summary ? <span className="ob-fold-summary">{summary}</span> : null}
        <ChevronDown size={16} className="ob-fold-chevron" aria-hidden="true" />
      </button>
    </div>
  );
}
