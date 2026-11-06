import React, { useRef, useState } from "react";
import { mossEyeCorePalette, mossEyeTwinView } from "../voiceState.js";
import { MossEye } from "./MossEye.jsx";

const STATE_BADGE = {
  offline: { code: "OPTIC · OFFLINE", label: "休眠待命" },
  idle: { code: "OPTIC · STANDBY", label: "在线守望" },
  listening: { code: "ACOUSTIC · LOCK", label: "实时拾音" },
  thinking: { code: "NEURAL · ROUTE", label: "神经推理" },
  speaking: { code: "VOICE · STREAM", label: "同步播报" },
  pulse: { code: "MCP · TELEMETRY", label: "指令脉冲" },
  alert: { code: "SAFETY · ALERT", label: "安全警戒" },
};

/**
 * MOSS 全息语音孪生舱（三链路自适应 + 多态光学球核与注视联动）：
 * - builtin：真实 PCM 振幅驱动机械眼光圈、核心呼吸与对称交错声波 + 会话阶段/识别字数/已播句数；
 * - self_hosted：同步自建 xiaozhi-server 进程状态、健康检查会话、脱敏日志与 MCP 工具脉冲；
 * - official：同步云端 MCP 握手状态、累计 JSON-RPC 指令帧数与远端工具调用脉冲。
 */
export function MossVoiceTwin({
  mode = "official",
  status = null,
  builtinStatus = null,
  mcpStatus = null,
  logLines = [],
  approvalCount = 0,
}) {
  const twin = mossEyeTwinView({
    mode,
    status,
    builtinStatus,
    mcpStatus,
    logLines,
    approvalCount,
  });
  const stateMeta = STATE_BADGE[twin.visualState] || STATE_BADGE.idle;
  const palette = mossEyeCorePalette(twin.visualState);
  const levelRatio = Math.max(0, Math.min(1, (Number(twin.level) || 0) / 100));

  const rootRef = useRef(null);
  const [gaze, setGaze] = useState({ x: 0, y: 0 });
  const [focusPulse, setFocusPulse] = useState(false);

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

  return (
    <div
      ref={rootRef}
      className={`moss-twin moss-twin--${twin.visualState}${focusPulse ? " moss-twin--focus-pulse" : ""}`}
      id="voice-moss-twin"
      data-mode={twin.mode}
      data-visual-state={twin.visualState}
      data-tempo={palette.tempo}
      style={{
        "--moss-twin-level": levelRatio.toFixed(2),
        "--eye-gaze-x": gaze.x.toFixed(3),
        "--eye-gaze-y": gaze.y.toFixed(3),
        "--moss-accent": palette.accent,
      }}
      onPointerMove={handlePointerMove}
      onPointerLeave={handlePointerLeave}
      role="region"
      aria-label="MOSS 语音数字孪生监视器"
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
            <small className="mono">{stateMeta.code}</small>
          </span>
          <span className="moss-twin-source" id="voice-moss-twin-source">
            <b className="mono">{twin.sourceBadge}</b>
            <span>{twin.sourceLabel}</span>
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
