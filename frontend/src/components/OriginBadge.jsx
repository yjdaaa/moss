import React from "react";

/**
 * 从对刀起点枚举值或提示文本推断图示类型：
 * - "center"：木牌/图案中心起点
 * - "lower_left"：左下角起点
 * - "point2"：两点对位第 2 标记起点
 */
export function inferOriginKind(originOrNote) {
  if (originOrNote === "center" || originOrNote === "lower_left" || originOrNote === "point2") {
    return originOrNote;
  }
  const text = String(originOrNote || "");
  if (text.includes("第 2 个对位") || text.includes("对位标记")) return "point2";
  if (text.includes("中心")) return "center";
  return "lower_left";
}

/**
 * 纯展示对刀起点小图示（20×20 SVG）：
 * 直观对比「中心红点」与「左下角红点」（以及两点对位标记起点），无交互副作用。
 */
export function OriginBadge({ origin = "lower_left", className = "", title }) {
  const kind = inferOriginKind(origin);
  return (
    <svg
      viewBox="0 0 20 20"
      width="20"
      height="20"
      className={`origin-badge origin-badge--${kind}${className ? ` ${className}` : ""}`}
      data-origin={kind}
      aria-hidden={title ? undefined : "true"}
      role={title ? "img" : undefined}
      aria-label={title || undefined}
    >
      {title ? <title>{title}</title> : null}
      <rect x="2.5" y="2.5" width="15" height="15" rx="2.5" className="origin-badge-board" />
      {kind === "center" ? (
        <>
          <line x1="10" y1="4.5" x2="10" y2="15.5" className="origin-badge-guide" />
          <line x1="4.5" y1="10" x2="15.5" y2="10" className="origin-badge-guide" />
          <circle cx="10" cy="10" r="3.8" className="origin-badge-halo" />
          <circle cx="10" cy="10" r="2.2" className="origin-badge-dot" />
        </>
      ) : kind === "point2" ? (
        <>
          <line x1="5.5" y1="14.5" x2="14.5" y2="5.5" className="origin-badge-guide" />
          <circle cx="5.5" cy="14.5" r="1.6" className="origin-badge-ref" />
          <circle cx="14.5" cy="5.5" r="3.6" className="origin-badge-halo" />
          <circle cx="14.5" cy="5.5" r="2.1" className="origin-badge-dot" />
        </>
      ) : (
        <>
          <path d="M4.5 9.5V15.5H10.5" className="origin-badge-guide" />
          <circle cx="4.5" cy="15.5" r="3.8" className="origin-badge-halo" />
          <circle cx="4.5" cy="15.5" r="2.2" className="origin-badge-dot" />
        </>
      )}
    </svg>
  );
}

export default OriginBadge;
