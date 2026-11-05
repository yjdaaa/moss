import React, { useEffect, useId, useRef, useState } from "react";
import { Cpu, Layers, PenTool, Settings, KeyRound, ShieldCheck, AudioLines, Box } from "lucide-react";
import { readAuthToken } from "../api/client.js";
import { globalEyeSensorView, mossEyeCorePalette } from "../voiceState.js";
// 左侧竖向导航（技术书 §9.5.5 B）。窄屏（≤900px）由 CSS 折叠为顶部横排。
// 语音中心为第四个标签，只在 MOSS 桌面端显示（技术书 §9.4.7 / §9.5.1，A-69）。
export const NAV_ITEMS = [
  { path: "/", label: "雕刻工作台", icon: Cpu, tag: "HOST" },
  { path: "/draw", label: "创作画板", icon: PenTool, tag: "CAD" },
  { path: "/material-lab", label: "材料实验室", icon: Layers, tag: "LAB" },
  { path: "/twin", label: "数字孪生", icon: Box, tag: "TWIN" },
  { path: "/voice", label: "语音中心", icon: AudioLines, tag: "VOICE", desktopOnly: true },
];

/** 当前环境可见的导航项。 */
export function visibleNavItems(showVoice) {
  return NAV_ITEMS.filter((item) => !item.desktopOnly || showVoice);
}

export function SideNav({
  currentPath,
  onNavigate,
  onAuthClick,
  onSettingsClick,
  showVoice = false,
  mcpStatus = null,
  voiceStatus = null,
  approvalCount = 0,
  connectionStatus = null,
  activeJobs = [],
}) {
  const [hasToken, setHasToken] = useState(Boolean(readAuthToken()));
  const eye = globalEyeSensorView({
    mcpStatus,
    voiceStatus,
    approvalCount,
    connectionStatus,
    activeJobs,
  });
  const palette = mossEyeCorePalette(eye.visualState);
  const eyeLevelRatio = Math.max(0, Math.min(1, (Number(eye.level) || 0) / 100));
  const uid = useId().replace(/:/g, "");
  const coreGradId = `${uid}-nav-eye-core`;
  const glowGradId = `${uid}-nav-eye-glow`;
  const eyeRef = useRef(null);

  useEffect(() => {
    const handleAuthChange = () => setHasToken(Boolean(readAuthToken()));
    window.addEventListener("laser-auth-required", handleAuthChange);
    return () => window.removeEventListener("laser-auth-required", handleAuthChange);
  }, []);

  useEffect(() => {
    const el = eyeRef.current;
    if (!el || typeof window === "undefined") return undefined;
    const reduceMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches ?? false;
    if (reduceMotion) return undefined;

    const handlePointerMove = (e) => {
      const rect = el.getBoundingClientRect();
      if (!rect.width || !rect.height) return;
      const cx = rect.left + rect.width * 0.5;
      const cy = rect.top + rect.height * 0.5;
      const dx = (e.clientX - cx) / Math.max(180, window.innerWidth * 0.35);
      const dy = (e.clientY - cy) / Math.max(180, window.innerHeight * 0.35);
      const gx = Math.max(-1, Math.min(1, dx));
      const gy = Math.max(-1, Math.min(1, dy));
      el.style.setProperty("--eye-gaze-x", gx.toFixed(3));
      el.style.setProperty("--eye-gaze-y", gy.toFixed(3));
    };
    const handlePointerLeave = () => {
      el.style.setProperty("--eye-gaze-x", "0");
      el.style.setProperty("--eye-gaze-y", "0");
    };
    window.addEventListener("pointermove", handlePointerMove, { passive: true });
    window.addEventListener("blur", handlePointerLeave);
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("blur", handlePointerLeave);
    };
  }, []);

  return (
    <aside className="ob-side" aria-label="主导航">
      <button
        type="button"
        className="ob-logo"
        onClick={() => onNavigate("/")}
        title={`返回雕刻工作台 · ${eye.label}（关闭程序请使用窗口右上角 ×）`}
      >
        <span
          ref={eyeRef}
          className={`ob-eye moss-eye-sensor moss-eye--${eye.visualState} moss-eye-hw--${eye.hardwareState}`}
          data-eye-state={eye.visualState}
          data-hardware-state={eye.hardwareState}
          style={{
            "--moss-eye-level": eyeLevelRatio.toFixed(2),
            "--moss-eye-accent": palette.accent,
          }}
          aria-hidden="true"
        >
          <svg className="ob-eye-svg" viewBox="0 0 40 40">
            <defs>
              <radialGradient id={coreGradId} cx="36%" cy="32%" r="68%">
                <stop offset="0%" stopColor={palette.coreStops[0]} />
                <stop offset="16%" stopColor={palette.coreStops[1]} />
                <stop offset="45%" stopColor={palette.coreStops[2]} />
                <stop offset="78%" stopColor={palette.coreStops[3]} />
                <stop offset="100%" stopColor={palette.coreStops[4]} />
              </radialGradient>
              <radialGradient id={glowGradId} cx="50%" cy="50%" r="50%">
                <stop offset="0%" stopColor={palette.glowStops[0]} />
                <stop offset="68%" stopColor={palette.glowStops[1]} />
                <stop offset="100%" stopColor={palette.glowStops[2]} />
              </radialGradient>
            </defs>

            {/* 外层激光硬件状态环（随硬件握手/雕刻出光变色与旋转） */}
            <circle
              className="ob-eye-ring-hw"
              cx="20"
              cy="20"
              r="18.2"
              fill="none"
              strokeWidth="1.2"
              strokeDasharray="3.2 2.8"
            />
            {/* 四向激光准星刻度 */}
            <path
              className="ob-eye-crosshair"
              d="M20 0.8 V3.6 M20 36.4 V39.2 M0.8 20 H3.6 M36.4 20 H39.2"
              strokeWidth="1.2"
              strokeLinecap="round"
            />
            {/* 双层交替扩散脉冲波纹环 */}
            <circle
              className="ob-eye-ring-sonar"
              cx="20"
              cy="20"
              r="10.5"
              fill="none"
              strokeWidth="1.35"
            />
            <circle
              className="ob-eye-ring-sonar ob-eye-ring-sonar--delay"
              cx="20"
              cy="20"
              r="10.5"
              fill="none"
              strokeWidth="1.2"
            />
            <circle
              className="ob-eye-ring-scan"
              cx="20"
              cy="20"
              r="14.6"
              fill={`url(#${glowGradId})`}
              strokeWidth="1.35"
              strokeDasharray="11 6 4 6"
            />
            {/* 机械光圈内环 */}
            <circle
              className="ob-eye-ring-iris"
              cx="20"
              cy="20"
              r="10.8"
              fill="none"
              strokeWidth="1"
              strokeDasharray="5 3.5"
            />
            {/* 球体核心与顶部弧形反光 */}
            <g className="ob-eye-core-group">
              <circle
                className="ob-eye-core"
                cx="20"
                cy="20"
                r="7.4"
                fill={`url(#${coreGradId})`}
              />
              <path
                className="ob-eye-spec-arc"
                d="M 14.8 16.2 A 6 6 0 0 1 25.2 16.2"
                fill="none"
                stroke="rgba(255,255,255,0.42)"
                strokeWidth="0.95"
                strokeLinecap="round"
              />
            </g>
            {/* 独立瞳孔与双点高光（随全局光标视差移动） */}
            <g className="ob-eye-pupil-group">
              <circle className="ob-eye-pupil" cx="20" cy="20" r="2.5" />
              <circle className="ob-eye-glint" cx="17.5" cy="17.3" r="1.2" />
              <circle className="ob-eye-glint-sub" cx="22.1" cy="22.2" r="0.6" />
            </g>
          </svg>
        </span>
        <span className="ob-logo-text">
          <b>MOSS</b>
          <small className={`ob-logo-sub ob-logo-sub--${eye.visualState} ob-logo-hw--${eye.hardwareState}`}>
            <i className="ob-logo-dot" aria-hidden="true" />
            <span>{eye.shortBadge || "LASER FORGE"}</span>
          </small>
        </span>
      </button>
      <div className="ob-nav-label">工作区</div>
      <nav className="ob-nav">
        {visibleNavItems(showVoice).map((item) => {
          const Icon = item.icon;
          const active = currentPath === item.path;
          return (
            <button
              key={item.path}
              type="button"
              onClick={() => onNavigate(item.path)}
              className={`ob-nav-item nav-tab${active ? " on active" : ""}`}
              aria-current={active ? "page" : undefined}
            >
              <Icon size={18} aria-hidden="true" />
              <span>{item.label}</span>
              <span className="ob-nav-tag">{item.tag}</span>
            </button>
          );
        })}
      </nav>

      <div className="ob-side-grow" />

      <div className="ob-side-foot">
        <button
          type="button"
          className="ob-nav-item"
          onClick={onSettingsClick}
          title="系统偏好设置 (关闭行为 / 硬件参数)"
        >
          <Settings size={18} aria-hidden="true" />
          <span>偏好设置</span>
        </button>
        {!showVoice && (
          <button
            type="button"
            className={`ob-nav-item${hasToken ? " ob-nav-item--ok" : ""}`}
            onClick={onAuthClick}
            title={hasToken ? "已完成局域网配对" : "输入配对码以授权移动端/局域网"}
          >
            {hasToken ? <ShieldCheck size={18} aria-hidden="true" /> : <KeyRound size={18} aria-hidden="true" />}
            <span>{hasToken ? "授权已通" : "配对认证"}</span>
          </button>
        )}
      </div>
    </aside>
  );
}
