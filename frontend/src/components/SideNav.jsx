import React, { useEffect, useRef, useState } from "react";
import { Cpu, Layers, PenTool, Settings, KeyRound, ShieldCheck, AudioLines, Box } from "lucide-react";
import { readAuthToken } from "../api/client.js";
import { globalEyeSensorView } from "../voiceState.js";
import { MossEye } from "./MossEye.jsx";
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
          className="ob-eye moss-eye-sensor"
          data-eye-state={eye.visualState}
          data-hardware-state={eye.hardwareState}
          aria-hidden="true"
        >
          <MossEye state={eye.visualState} level={eye.level} hardware={eye.hardwareState} size="sm" className="ob-eye-svg" />
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
