import React, { useState } from "react";
import { ChevronDown } from "lucide-react";
import { readFoldOpen, writeFoldOpen } from "../foldState.js";

/**
 * 可折叠标题（卡片与设置分区共用）。点击标题收起 / 展开同一容器里标题以外的内容；
 * 内容只是隐藏、不卸载，所以收起不会丢失已填写的内容或中断进行中的任务。
 * 收起状态按 id 记住。
 *
 * variant="card"：卡片标题（带步骤编号 n 与右侧英文标签 k）
 * variant="section"：偏好设置等弹窗里的分区标题（带图标 icon）
 */
export function FoldHeader({ id, title, n, k, icon: Icon, variant = "card", defaultOpen = true }) {
  const [open, setOpen] = useState(() => readFoldOpen(id, defaultOpen));
  const toggle = () => {
    setOpen((current) => {
      writeFoldOpen(id, !current);
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
        {k ? <span className="ob-k">{k}</span> : <span className="ob-fold-spacer" />}
        <ChevronDown size={16} className="ob-fold-chevron" aria-hidden="true" />
      </button>
    </div>
  );
}
