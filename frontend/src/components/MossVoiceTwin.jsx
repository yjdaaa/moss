import React, { useId } from "react";
import { mossEyeTwinView } from "../voiceState.js";

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
 * MOSS 全息语音孪生舱（三链路自适应）：
 * - builtin：60ms 真实 PCM 振幅驱动机械眼光圈与对称声波 + 会话阶段/识别字数/已播句数；
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
  const levelRatio = Math.max(0, Math.min(1, (Number(twin.level) || 0) / 100));
  const uid = useId().replace(/:/g, "");
  const coreGradId = `${uid}-moss-core-grad`;
  const glowGradId = `${uid}-moss-glow-grad`;

  return (
    <div
      className={`moss-twin moss-twin--${twin.visualState}`}
      id="voice-moss-twin"
      data-mode={twin.mode}
      data-visual-state={twin.visualState}
      style={{ "--moss-twin-level": levelRatio.toFixed(2) }}
      role="region"
      aria-label="MOSS 语音数字孪生监视器"
    >
      <div className="moss-twin-ocular">
        <div className="moss-twin-lens-wrap" aria-hidden="true">
          <svg className="moss-twin-svg" viewBox="0 0 120 120">
            <defs>
              <radialGradient id={coreGradId} cx="38%" cy="34%" r="68%">
                <stop offset="0%" stopColor="#ffffff" />
                <stop offset="14%" stopColor="#ffc2cd" />
                <stop offset="40%" stopColor="#ff3b5c" />
                <stop offset="74%" stopColor="#990a1e" />
                <stop offset="100%" stopColor="#1e0005" />
              </radialGradient>
              <radialGradient id={glowGradId} cx="50%" cy="50%" r="50%">
                <stop offset="0%" stopColor="rgba(56, 189, 248, 0.42)" />
                <stop offset="65%" stopColor="rgba(255, 59, 92, 0.22)" />
                <stop offset="100%" stopColor="rgba(255, 59, 92, 0)" />
              </radialGradient>
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
            {/* 中层旋转扫描环（思考/指令脉冲/播报时旋转） */}
            <circle
              className="moss-twin-ring-scan"
              cx="60"
              cy="60"
              r="44"
              fill="none"
              strokeWidth="1.8"
              strokeDasharray="24 14 8 14"
            />
            {/* 声学共鸣包络光环（随真实 60ms 音频振幅缩放） */}
            <circle
              className="moss-twin-ring-acoustic"
              cx="60"
              cy="60"
              r="33"
              fill={`url(#${glowGradId})`}
              strokeWidth="1.5"
            />
            {/* 机械光圈内环 */}
            <circle
              className="moss-twin-ring-iris"
              cx="60"
              cy="60"
              r="24"
              fill="none"
              strokeWidth="1.2"
              strokeDasharray="10 6"
            />
            {/* MOSS 核心红光传感器透镜 */}
            <circle
              className="moss-twin-core"
              cx="60"
              cy="60"
              r="16"
              fill={`url(#${coreGradId})`}
            />
            {/* 中心瞳孔与高光反射点 */}
            <circle className="moss-twin-pupil" cx="60" cy="60" r="5.5" />
            <circle className="moss-twin-glint" cx="54.5" cy="54" r="2.4" />
          </svg>
        </div>

        <div
          className="moss-twin-wave"
          id="voice-moss-twin-wave"
          aria-hidden="true"
        >
          {twin.bars.map((height, index) => (
            <span
              key={index}
              className="moss-twin-bar"
              style={{ "--bar-h": `${height}%` }}
            />
          ))}
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
