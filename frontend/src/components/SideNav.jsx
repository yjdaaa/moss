import React, { useEffect, useState } from "react";
import { Cpu, Layers, PenTool, Settings, KeyRound, ShieldCheck, AudioLines, Box } from "lucide-react";
import { readAuthToken } from "../api/client.js";

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

export function SideNav({ currentPath, onNavigate, onAuthClick, onSettingsClick, showVoice = false }) {
  const [hasToken, setHasToken] = useState(Boolean(readAuthToken()));

  useEffect(() => {
    const handleAuthChange = () => setHasToken(Boolean(readAuthToken()));
    window.addEventListener("laser-auth-required", handleAuthChange);
    return () => window.removeEventListener("laser-auth-required", handleAuthChange);
  }, []);

  return (
    <aside className="ob-side" aria-label="主导航">
      <button
        type="button"
        className="ob-logo"
        onClick={() => onNavigate("/")}
        title="返回雕刻工作台（关闭程序请使用窗口右上角 ×）"
      >
        <span className="ob-eye moss-eye-sensor" aria-hidden="true" />
        <span className="ob-logo-text">
          <b>MOSS</b>
          <small>LASER FORGE</small>
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
        <button
          type="button"
          className={`ob-nav-item${hasToken ? " ob-nav-item--ok" : ""}`}
          onClick={onAuthClick}
          title={hasToken ? "已完成局域网配对" : "输入配对码以授权移动端/局域网"}
        >
          {hasToken ? <ShieldCheck size={18} aria-hidden="true" /> : <KeyRound size={18} aria-hidden="true" />}
          <span>{hasToken ? "授权已通" : "配对认证"}</span>
        </button>
      </div>
    </aside>
  );
}
