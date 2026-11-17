import React from "react";
import { Clock, Ruler, Settings2, ShieldAlert } from "lucide-react";
import { sendBadgeLabel, summaryDisplayWarnings, thicknessDisplayRows } from "../../jobSummaryView.js";

// 创意画板作业摘要卡（预览与发送卡、发送确认框共用）。原样搬自 pages/DrawPage.jsx（2026-11-16 按职责拆分）

export function JobSummaryCard({ summary, compact = false }) {
  if (!summary) {
    return (
      <section className="job-summary-card muted-summary">
        <div className="summary-heading">
          <Settings2 size={17} />
          <h2>作业摘要</h2>
        </div>
        <p>生成预览后显示材料、尺寸、参数和预计时间。</p>
      </section>
    );
  }

  const displayWarnings = summaryDisplayWarnings(summary, formatWithUnit);
  const thicknessRows = thicknessDisplayRows(summary);
  const sizeText =
    summary.actual_width_mm && summary.actual_height_mm
      ? `${formatMm(summary.actual_width_mm)} x ${formatMm(summary.actual_height_mm)}`
      : "自动";

  return (
    <section className={`job-summary-card ${compact ? "compact" : ""}`}>
      <div className="summary-heading">
        <Settings2 size={17} />
        <h2>作业摘要</h2>
        <span className={summary.can_send ? "summary-badge ok" : "summary-badge warn"}>
          {sendBadgeLabel(summary)}
        </span>
      </div>
      <div className="summary-grid">
        <SummaryItem label="材料" value={summary.material || "未识别"} />
        {thicknessRows.map((row) => (
          <SummaryItem key={row.label} label={row.label} value={formatWithUnit(summary[row.valueKey], "mm")} />
        ))}
        <SummaryItem label="策略" value={modeLabel(summary.mode, summary.task_type, summary.vector_style)} />
        <SummaryItem label="尺寸" value={sizeText} icon={<Ruler size={15} />} />
        <SummaryItem label="预计时间" value={summary.estimated_display || "待估算"} icon={<Clock size={15} />} />
        <SummaryItem label="功率" value={summary.power ? `S${summary.power}` : "材料库默认"} />
        <SummaryItem label="速度" value={summary.speed ? `F${summary.speed}` : "材料库默认"} />
        <SummaryItem label="次数" value={summary.passes || "默认"} />
        {summary.mode === "raster" ? (
          <SummaryItem label="线距" value={formatWithUnit(summary.pixel_size_mm, "mm")} />
        ) : null}
      </div>
      {summary.size_note && <p className="size-note">{summary.size_note}</p>}
      {displayWarnings.length > 0 && (
        <div className="warning-list">
          <ShieldAlert size={16} />
          <span>{displayWarnings.join("；")}</span>
        </div>
      )}
    </section>
  );
}

function SummaryItem({ label, value, icon = null }) {
  return (
    <div className="summary-item">
      <span>{label}</span>
      <strong>
        {icon}
        {value || "未知"}
      </strong>
    </div>
  );
}

const VECTOR_STYLE_LABELS = { outline: "轮廓", fill: "填充", cut: "切割", canvas: "按画布颜色" };

function modeLabel(mode, taskType, vectorStyle) {
  if (mode === "vector") return VECTOR_STYLE_LABELS[vectorStyle] ? `矢量 · ${VECTOR_STYLE_LABELS[vectorStyle]}` : "矢量";
  if (mode === "raster" || taskType === "engrave_photo") return "光栅填充";
  if (mode === "outline" || taskType === "engrave_logo") return "轮廓描边";
  if (taskType === "cut_contour") return "轮廓切割";
  return mode || taskType || "自动";
}

function formatMm(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "未知";
  return `${Number(number.toFixed(2)).toString()} mm`;
}

function formatWithUnit(value, unit) {
  if (value === undefined || value === null || value === "") return "默认";
  const number = Number(value);
  if (!Number.isFinite(number)) return String(value);
  return `${Number(number.toFixed(3)).toString()} ${unit}`;
}
