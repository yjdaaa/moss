import React, { useEffect, useRef, useState } from "react";
import { Cpu, Layers, PenTool, Settings, KeyRound, ShieldCheck, AudioLines, Box, Camera, PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { readAuthToken } from "../api/client.js";
import { globalEyeSensorView } from "../voiceState.js";
import { MossEye } from "./MossEye.jsx";
// 左侧竖向导航（技术书 §9.5.5 B）。窄屏（≤900px）由 CSS 折叠为顶部横排。
// 语音中心与摄像头只在 MOSS 桌面端显示（技术书 §9.4.7 / §9.5.1，A-69）。
export const NAV_ITEMS = [
  { path: "/", label: "雕刻工作台", icon: Cpu, tag: "HOST" },
  { path: "/draw", label: "创作画板", icon: PenTool, tag: "CAD" },
  { path: "/material-lab", label: "材料实验室", icon: Layers, tag: "LAB" },
  { path: "/twin", label: "数字孪生", icon: Box, tag: "TWIN" },
  { path: "/camera", label: "摄像头", icon: Camera, tag: "CAM", desktopOnly: true },
  { path: "/voice", label: "语音中心", icon: AudioLines, tag: "VOICE", desktopOnly: true },
];

/** 当前环境可见的导航项。 */
export function visibleNavItems(showVoice) {
  return NAV_ITEMS.filter((item) => !item.desktopOnly || showVoice);
}

export function SideNav({
  currentPath,
  onNavigate,
  onPreload,
  onAuthClick,
  onSettingsClick,
  showVoice = false,
  mcpStatus = null,
  voiceStatus = null,
  approvalCount = 0,
  connectionStatus = null,
  activeJobs = [],
  collapsed = false,
  onToggleCollapsed,
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

    let eyeCenter = null;
    let rafId = 0;
    let lastX = 0;
    let lastY = 0;

    const measureEye = () => {
      const rect = el.getBoundingClientRect();
      eyeCenter = rect.width && rect.height
        ? { cx: rect.left + rect.width * 0.5, cy: rect.top + rect.height * 0.5 }
        : null;
    };

    const flushGaze = () => {
      rafId = 0;
      if (!eyeCenter) measureEye();
      if (!eyeCenter) return;
      const dx = (lastX - eyeCenter.cx) / Math.max(180, window.innerWidth * 0.35);
      const dy = (lastY - eyeCenter.cy) / Math.max(180, window.innerHeight * 0.35);
      const gx = Math.max(-1, Math.min(1, dx));
      const gy = Math.max(-1, Math.min(1, dy));
      el.style.setProperty("--eye-gaze-x", gx.toFixed(3));
      el.style.setProperty("--eye-gaze-y", gy.toFixed(3));
    };

    const handlePointerMove = (e) => {
      lastX = e.clientX;
      lastY = e.clientY;
      if (!rafId) rafId = window.requestAnimationFrame(flushGaze);
    };
    const handlePointerLeave = () => {
      if (rafId) {
        window.cancelAnimationFrame(rafId);
        rafId = 0;
      }
      el.style.setProperty("--eye-gaze-x", "0");
      el.style.setProperty("--eye-gaze-y", "0");
    };
    const handleResize = () => {
      eyeCenter = null;
    };

    measureEye();
    window.addEventListener("pointermove", handlePointerMove, { passive: true });
    window.addEventListener("resize", handleResize, { passive: true });
    window.addEventListener("blur", handlePointerLeave);
    return () => {
      if (rafId) window.cancelAnimationFrame(rafId);
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("resize", handleResize);
      window.removeEventListener("blur", handlePointerLeave);
    };
  }, [collapsed]);

  return (
    <aside className={`ob-side${collapsed ? " is-collapsed" : ""}`} aria-label="主导航" data-collapsed={collapsed ? "true" : "false"}>
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
              onMouseEnter={() => onPreload?.(item.path)}
              onFocus={() => onPreload?.(item.path)}
              onPointerDown={() => onPreload?.(item.path)}
              className={`ob-nav-item nav-tab${active ? " on active" : ""}`}
              aria-current={active ? "page" : undefined}
              aria-label={collapsed ? item.label : undefined}
              title={collapsed ? item.label : undefined}
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
        {onToggleCollapsed && (
          <button
            type="button"
            className="ob-nav-item ob-side-toggle"
            onClick={onToggleCollapsed}
            id="side-collapse-toggle"
            aria-expanded={!collapsed}
            aria-label={collapsed ? "展开侧栏" : "收起侧栏"}
            title={collapsed ? "展开侧栏" : "收起侧栏（只留图标）"}
          >
            {collapsed ? <PanelLeftOpen size={18} aria-hidden="true" /> : <PanelLeftClose size={18} aria-hidden="true" />}
            <span>{collapsed ? "展开侧栏" : "收起侧栏"}</span>
          </button>
        )}
        <button
          type="button"
          className="ob-nav-item"
          onClick={onSettingsClick}
          aria-label={collapsed ? "偏好设置" : undefined}
          title={collapsed ? "偏好设置" : "系统偏好设置 (关闭行为 / 硬件参数)"}
        >
          <Settings size={18} aria-hidden="true" />
          <span>偏好设置</span>
        </button>
        {!showVoice && (
          <button
            type="button"
            className={`ob-nav-item${hasToken ? " ob-nav-item--ok" : ""}`}
            onClick={onAuthClick}
            aria-label={collapsed ? (hasToken ? "授权已通" : "配对认证") : undefined}
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
