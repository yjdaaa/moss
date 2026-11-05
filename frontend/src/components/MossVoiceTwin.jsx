import React, { useId, useRef, useState } from "react";
import { mossEyeCorePalette, mossEyeTwinView } from "../voiceState.js";

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
  const uid = useId().replace(/:/g, "");
  const coreGradId = `${uid}-moss-core-grad`;
  const glowGradId = `${uid}-moss-glow-grad`;
  const lensRimGradId = `${uid}-moss-rim-grad`;

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
        "--moss-gaze-x": gaze.x.toFixed(3),
        "--moss-gaze-y": gaze.y.toFixed(3),
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
          title="点击触发光学镜头对焦自检"
        >
          <svg className="moss-twin-svg" viewBox="0 0 120 120">
            <defs>
              <radialGradient id={coreGradId} cx="38%" cy="34%" r="68%">
                <stop offset="0%" stopColor={palette.coreStops[0]} />
                <stop offset="16%" stopColor={palette.coreStops[1]} />
                <stop offset="44%" stopColor={palette.coreStops[2]} />
                <stop offset="76%" stopColor={palette.coreStops[3]} />
                <stop offset="100%" stopColor={palette.coreStops[4]} />
              </radialGradient>
              <radialGradient id={glowGradId} cx="50%" cy="50%" r="50%">
                <stop offset="0%" stopColor={palette.glowStops[0]} />
                <stop offset="65%" stopColor={palette.glowStops[1]} />
                <stop offset="100%" stopColor={palette.glowStops[2]} />
              </radialGradient>
              <linearGradient id={lensRimGradId} x1="0%" y1="0%" x2="100%" y2="100%">
                <stop offset="0%" stopColor="rgba(255,255,255,0.55)" />
                <stop offset="50%" stopColor={palette.accent} stopOpacity="0.35" />
                <stop offset="100%" stopColor="rgba(255,255,255,0.08)" />
              </linearGradient>
            </defs>

            {/* 外层太空舱方位刻度环 */}
            <circle
              className="moss-twin-ring-outer"
              cx="60"
              cy="60"
              r="54"
              fill="none"
              strokeWidth="1.2"
              strokeDasharray="3 5"
            />
            {/* 四向准星基准线 */}
            <path
              className="moss-twin-crosshair"
              d="M60 2 V10 M60 110 V118 M2 60 H10 M110 60 H118"
              strokeWidth="1.4"
              strokeLinecap="round"
            />
            {/* 双层扩散声呐脉冲波纹环（随状态周期向外扩散） */}
            <circle
              className="moss-twin-ring-sonar"
              cx="60"
              cy="60"
              r="28"
              fill="none"
              strokeWidth="1.6"
            />
            <circle
              className="moss-twin-ring-sonar moss-twin-ring-sonar--delay"
              cx="60"
              cy="60"
              r="28"
              fill="none"
              strokeWidth="1.3"
            />
            {/* 中层旋转扫描环（思考/指令脉冲/播报时加速旋转，休眠时慢速巡航） */}
            <circle
              className="moss-twin-ring-scan"
              cx="60"
              cy="60"
              r="44"
              fill="none"
              strokeWidth="1.8"
              strokeDasharray="24 14 8 14"
            />
            {/* 声学共鸣包络光环（随真实音频振幅 + 状态呼吸缩放） */}
            <circle
              className="moss-twin-ring-acoustic"
              cx="60"
              cy="60"
              r="34"
              fill={`url(#${glowGradId})`}
              strokeWidth="1.5"
            />
            {/* 机械光圈内环 */}
            <circle
              className="moss-twin-ring-iris"
              cx="60"
              cy="60"
              r="24.5"
              fill="none"
              stroke={`url(#${lensRimGradId})`}
              strokeWidth="1.35"
              strokeDasharray="10 6"
            />
            {/* MOSS 核心球面传感器透镜（带呼吸与视差微动） */}
            <g className="moss-twin-core-group">
              <circle
                className="moss-twin-core"
                cx="60"
                cy="60"
                r="16.5"
                fill={`url(#${coreGradId})`}
              />
              {/* 凸透镜顶部弧形玻璃反光 */}
              <path
                className="moss-twin-spec-arc"
                d="M 48 52 A 13 13 0 0 1 72 52"
                fill="none"
                stroke="rgba(255, 255, 255, 0.38)"
                strokeWidth="1.3"
                strokeLinecap="round"
              />
            </g>
            {/* 中心瞳孔与双点光学高光（支持注视视差与微对焦） */}
            <g className="moss-twin-pupil-group">
              <circle className="moss-twin-pupil" cx="60" cy="60" r="5.6" />
              <circle className="moss-twin-glint" cx="54.2" cy="53.8" r="2.5" />
              <circle className="moss-twin-glint-sub" cx="64.8" cy="65.2" r="1.15" />
            </g>
          </svg>
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
