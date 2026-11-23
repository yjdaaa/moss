import React, { useRef, useState } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";
import { isMossTwinConversing, mossEyeCorePalette, mossEyeTwinView } from "../voiceState.js";
import { MossEye } from "./MossEye.jsx";

const STATE_BADGE = {
  offline: { label: "未连接" },
  idle: { label: "待命" },
  listening: { label: "正在听" },
  thinking: { label: "思考中" },
  speaking: { label: "播报中" },
  pulse: { label: "执行指令" },
  alert: { label: "待批准" },
};

/**
 * 语音中心状态卡（非对话时压缩为单行状态条，点击或正在对话时展开完整声波与指标）：
 * - builtin：真实 PCM 振幅驱动机械眼光圈与对称声波 + 会话阶段/识别字数/已播句数；
 * - official：同步小智云端连接状态、指令帧数与最近调用。
 */
export function MossVoiceTwin({
  mode = "official",
  status = null,
  builtinStatus = null,
  mcpStatus = null,
  approvalCount = 0,
}) {
  const twin = mossEyeTwinView({
    mode,
    status,
    builtinStatus,
    mcpStatus,
    approvalCount,
  });
  const stateMeta = STATE_BADGE[twin.visualState] || STATE_BADGE.idle;
  const palette = mossEyeCorePalette(twin.visualState);
  const levelRatio = Math.max(0, Math.min(1, (Number(twin.level) || 0) / 100));
  const conversing = isMossTwinConversing(twin.visualState);

  const rootRef = useRef(null);
  const [gaze, setGaze] = useState({ x: 0, y: 0 });
  const [focusPulse, setFocusPulse] = useState(false);
  const [manualOpen, setManualOpen] = useState(false);
  const expanded = conversing || manualOpen;

  const handlePointerMove = (e) => {
    const el = rootRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    const nx = ((e.clientX - rect.left) / rect.width - 0.25) * 2;
    const ny = ((e.clientY - rect.top) / rect.height - 0.5) * 2;
    setGaze({
      x: Math.max(-1, Math.min(1, nx)),
      y: Math.max(-1, Math.min(1, ny)),
    });
  };

  const handlePointerLeave = () => {
    setGaze({ x: 0, y: 0 });
  };

  const triggerFocusPulse = () => {
    setFocusPulse(false);
    requestAnimationFrame(() => setFocusPulse(true));
  };

  if (!expanded) {
    return (
      <div
        ref={rootRef}
        className={`moss-twin moss-twin--compact moss-twin--${twin.visualState}`}
        id="voice-moss-twin"
        data-mode={twin.mode}
        data-visual-state={twin.visualState}
        data-tempo={palette.tempo}
        data-expanded="false"
        style={{
          "--moss-twin-level": levelRatio.toFixed(2),
          "--eye-gaze-x": gaze.x.toFixed(3),
          "--eye-gaze-y": gaze.y.toFixed(3),
          "--moss-accent": palette.accent,
        }}
        onPointerMove={handlePointerMove}
        onPointerLeave={handlePointerLeave}
        role="region"
        aria-label="MOSS 语音状态"
      >
        <button
          type="button"
          className="moss-twin-compact-btn"
          id="voice-moss-twin-toggle"
          aria-expanded="false"
          onClick={() => setManualOpen(true)}
          title="点击展开语音状态详情"
        >
          <span className="moss-twin-compact-eye" aria-hidden="true">
            <MossEye state={twin.visualState} level={twin.level} size="sm" className="moss-twin-compact-svg" />
          </span>
          <span className="moss-twin-state-pill" id="voice-moss-twin-state">
            <span className="moss-twin-dot" aria-hidden="true" />
            <b>{stateMeta.label}</b>
          </span>
          <span className="moss-twin-compact-headline" id="voice-moss-twin-headline">
            {twin.headline}
          </span>
          <span className="moss-twin-source" id="voice-moss-twin-source">
            <b>{twin.sourceBadge}</b>
          </span>
          <span className="moss-twin-compact-more">
            <span>展开</span>
            <ChevronDown size={14} aria-hidden="true" />
          </span>
        </button>
      </div>
    );
  }

  return (
    <div
      ref={rootRef}
      className={`moss-twin moss-twin--${twin.visualState}${focusPulse ? " moss-twin--focus-pulse" : ""}`}
      id="voice-moss-twin"
      data-mode={twin.mode}
      data-visual-state={twin.visualState}
      data-tempo={palette.tempo}
      data-expanded="true"
      style={{
        "--moss-twin-level": levelRatio.toFixed(2),
        "--eye-gaze-x": gaze.x.toFixed(3),
        "--eye-gaze-y": gaze.y.toFixed(3),
        "--moss-accent": palette.accent,
      }}
      onPointerMove={handlePointerMove}
      onPointerLeave={handlePointerLeave}
      role="region"
      aria-label="MOSS 语音状态"
    >
      <div className="moss-twin-ocular">
        <div
          className="moss-twin-lens-wrap"
          aria-hidden="true"
          onClick={triggerFocusPulse}
          onAnimationEnd={() => setFocusPulse(false)}
          title="戳一下 MOSS 的眼睛"
        >
          <MossEye state={twin.visualState} level={twin.level} size="lg" className="moss-twin-svg" />
        </div>

        <div
          className="moss-twin-wave"
          id="voice-moss-twin-wave"
          aria-hidden="true"
        >
          {twin.bars.map((height, index) => {
            const distFromCenter = Math.abs(index - 5.5);
            return (
              <span
                key={index}
                className="moss-twin-bar"
                style={{
                  "--bar-h": `${height}%`,
                  "--bar-idx": index,
                  "--bar-dist": distFromCenter.toFixed(1),
                }}
              />
            );
          })}
        </div>
      </div>
      <div className="moss-twin-hud">
        <div className="moss-twin-head">
          <span className="moss-twin-state-pill" id="voice-moss-twin-state">
            <span className="moss-twin-dot" aria-hidden="true" />
            <b>{stateMeta.label}</b>
          </span>
          <span className="moss-twin-head-end">
            <span className="moss-twin-source" id="voice-moss-twin-source">
              <b>{twin.sourceBadge}</b>
              <span>{twin.sourceLabel}</span>
            </span>
            {!conversing && (
              <button
                type="button"
                className="btn btn-ghost moss-twin-collapse-btn"
                id="voice-moss-twin-toggle"
                aria-expanded="true"
                onClick={() => setManualOpen(false)}
              >
                <span>收起</span>
                <ChevronUp size={13} aria-hidden="true" />
              </button>
            )}
          </span>
        </div>

        <div className="moss-twin-headline" id="voice-moss-twin-headline">
          {twin.headline}
        </div>
        <div className="moss-twin-subline mono" id="voice-moss-twin-subline">
          {twin.subline}
        </div>

        <div className="moss-twin-metrics" id="voice-moss-twin-metrics">
          {twin.metrics.map((item) => (
            <div key={item.label} className="moss-twin-metric">
              <span className="moss-twin-metric-k">{item.label}</span>
              <strong className="moss-twin-metric-v mono">{item.value}</strong>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
export default MossVoiceTwin;
