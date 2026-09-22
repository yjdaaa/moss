import React, { useState, useEffect } from "react";
import { Cpu, Layers, PenTool, Wifi, WifiOff, KeyRound, Settings, ShieldCheck, Activity, Zap, Radio } from "lucide-react";
import { readAuthToken } from "../api/client.js";

export function Navbar({ currentPath, onNavigate, connectionStatus, mcpStatus, onAuthClick, onSettingsClick }) {
  const [hasToken, setHasToken] = useState(Boolean(readAuthToken()));

  useEffect(() => {
    const handleAuthChange = () => setHasToken(Boolean(readAuthToken()));
    window.addEventListener("laser-auth-required", handleAuthChange);
    return () => window.removeEventListener("laser-auth-required", handleAuthChange);
  }, []);

  const navItems = [
    { path: "/", label: "雕刻工作台", icon: Cpu, badge: "HOST" },
    { path: "/material-lab", label: "材料实验室", icon: Layers, badge: "LAB" },
    { path: "/draw", label: "创作画板", icon: PenTool, badge: "CAD" },
  ];

  return (
    <header
      className="site-header"
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        padding: "10px 24px",
        background: "rgba(6, 9, 18, 0.85)",
        backdropFilter: "blur(18px)",
        WebkitBackdropFilter: "blur(18px)",
        borderBottom: "1px solid rgba(53, 224, 255, 0.2)",
        position: "sticky",
        top: 0,
        zIndex: 50,
        boxShadow: "0 4px 24px rgba(0, 0, 0, 0.7), 0 0 20px rgba(53, 224, 255, 0.06)",
      }}
    >
      {/* 左侧：MOSS 核心标识 */}
      <div style={{ display: "flex", alignItems: "center", gap: "28px" }}>
        <div
          onClick={() => onNavigate("/")}
          style={{
            cursor: "pointer",
            display: "flex",
            alignItems: "center",
            gap: "10px",
            userSelect: "none",
          }}
        >
          {/* MOSS 经典红光眼睛传感器 */}
          <div className="moss-eye-sensor" title="MOSS 550W 量子核心在线（点击返回主页；若要关闭程序请使用右上角 ❌）" />
          <div style={{ display: "flex", flexDirection: "column" }}>
            <div style={{ display: "flex", alignItems: "baseline", gap: "6px" }}>
              <span style={{ fontWeight: "900", fontSize: "16px", letterSpacing: "1.5px", color: "var(--ink, #fff)" }}>
                MOSS
              </span>
              <span style={{ fontSize: "11px", fontWeight: "700", color: "var(--laser, #35e0ff)", letterSpacing: "2px" }}>
                LASER FORGE
              </span>
            </div>
            <span style={{ fontSize: "9px", fontFamily: "var(--font-mono)", color: "var(--ink-faint)", letterSpacing: "1px" }}>
              550W · QUANTUM INDUSTRIAL COMMAND
            </span>
          </div>
        </div>

        {/* 中间：HUD 导航 Tab 组 */}
        <nav style={{ display: "flex", gap: "6px" }}>
          {navItems.map((item) => {
            const Icon = item.icon;
            const active = currentPath === item.path;
            return (
              <button
                key={item.path}
                onClick={() => onNavigate(item.path)}
                className={`nav-tab ${active ? "active" : ""}`}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: "7px",
                  padding: "7px 16px",
                  borderRadius: "6px",
                  background: active ? "rgba(53, 224, 255, 0.12)" : "rgba(255, 255, 255, 0.02)",
                  color: active ? "var(--laser, #35e0ff)" : "var(--ink-dim, #94a3b8)",
                  border: `1px solid ${active ? "rgba(53, 224, 255, 0.45)" : "transparent"}`,
                  cursor: "pointer",
                  fontSize: "13px",
                  fontWeight: active ? "700" : "500",
                  transition: "all 0.2s cubic-bezier(0.16, 1, 0.3, 1)",
                  boxShadow: active ? "0 0 16px rgba(53, 224, 255, 0.25)" : "none",
                  position: "relative",
                }}
              >
                <Icon size={15} style={{ color: active ? "var(--laser-2)" : "inherit" }} />
                <span>{item.label}</span>
                <span
                  style={{
                    fontSize: "9px",
                    fontFamily: "var(--font-mono)",
                    opacity: active ? 0.9 : 0.4,
                    padding: "1px 4px",
                    borderRadius: "3px",
                    background: active ? "rgba(53, 224, 255, 0.2)" : "rgba(255, 255, 255, 0.05)",
                  }}
                >
                  {item.badge}
                </span>
                {active && (
                  <span
                    style={{
                      position: "absolute",
                      bottom: "-1px",
                      left: "20%",
                      right: "20%",
                      height: "2px",
                      background: "var(--laser)",
                      boxShadow: "0 0 8px var(--laser)",
                      borderRadius: "2px",
                    }}
                  />
                )}
              </button>
            );
          })}
        </nav>
      </div>

      {/* 右侧：状态指示与系统工具 */}
      <div style={{ display: "flex", alignItems: "center", gap: "10px" }}>
        {/* 设置面板按钮 */}
        <button
          onClick={onSettingsClick}
          className="btn btn-ghost"
          style={{
            display: "flex",
            alignItems: "center",
            gap: "5px",
            fontSize: "12px",
            padding: "5px 10px",
          }}
          title="系统偏好设置 (关闭行为 / 硬件参数)"
        >
          <Settings size={14} />
          <span>偏好设置</span>
        </button>

        {/* 配对认证状态 */}
        <button
          onClick={onAuthClick}
          className="btn btn-ghost"
          style={{
            display: "flex",
            alignItems: "center",
            gap: "5px",
            fontSize: "12px",
            padding: "5px 10px",
            color: hasToken ? "var(--green, #00f59b)" : "var(--ink-dim, #94a3b8)",
            borderColor: hasToken ? "rgba(0, 245, 155, 0.3)" : "var(--line)",
          }}
          title={hasToken ? "已完成局域网配对" : "输入配对码以授权移动端/局域网"}
        >
          {hasToken ? <ShieldCheck size={14} /> : <KeyRound size={14} />}
          <span>{hasToken ? "授权已通" : "配对认证"}</span>
        </button>

        {/* 胶囊 1: 激光硬件控制器在线指示（可点击跳转工作台通信链路） */}
        <button
          onClick={() => onNavigate("/")}
          className={`conn-pill ${connectionStatus?.connected ? "online" : "offline"}`}
          style={{
            display: "flex",
            alignItems: "center",
            gap: "6px",
            fontSize: "11px",
            padding: "5px 10px",
            borderRadius: "6px",
            background: connectionStatus?.connected
              ? "rgba(0, 245, 155, 0.12)"
              : "rgba(255, 51, 75, 0.12)",
            color: connectionStatus?.connected ? "var(--green, #00f59b)" : "var(--danger, #ff334b)",
            border: `1px solid ${
              connectionStatus?.connected
                ? "rgba(0, 245, 155, 0.35)"
                : "rgba(255, 51, 75, 0.35)"
            }`,
            fontFamily: "var(--font-mono)",
            letterSpacing: "0.5px",
            cursor: "pointer",
          }}
          title="激光雕刻机硬件状态（点击前往工作台进行握手探测）"
          id="nav-hardware-status"
        >
          <Zap size={12} style={{ color: connectionStatus?.connected ? "#00f59b" : "#ff334b" }} />
          <span>硬件: {connectionStatus?.connected ? "ONLINE" : "OFFLINE"}</span>
        </button>

        {/* 胶囊 2: 小智 AI 语音/MCP 网关状态指示 */}
        <div
          className={`conn-pill ${mcpStatus?.configured ? "online" : "offline"}`}
          style={{
            display: "flex",
            alignItems: "center",
            gap: "6px",
            fontSize: "11px",
            padding: "5px 10px",
            borderRadius: "6px",
            background: mcpStatus?.configured
              ? "rgba(0, 229, 255, 0.12)"
              : "rgba(255, 255, 255, 0.04)",
            color: mcpStatus?.configured ? "var(--laser-cyan, #00e5ff)" : "var(--ink-dim, #94a3b8)",
            border: `1px solid ${
              mcpStatus?.configured
                ? "rgba(0, 229, 255, 0.35)"
                : "rgba(255, 255, 255, 0.1)"
            }`,
            fontFamily: "var(--font-mono)",
            letterSpacing: "0.5px",
          }}
          title={`小智 MCP 网关状态: ${mcpStatus?.configured ? (mcpStatus.client_info || mcpStatus.endpoint) : "未配置 (需设置 MCP_ENDPOINT)"}`}
          id="nav-mcp-status"
        >
          <Radio size={12} style={{ color: mcpStatus?.configured ? "#00e5ff" : "#64748b" }} />
          <span>小智: {mcpStatus?.configured ? "CONNECTED" : "OFFLINE"}</span>
        </div>

      </div>
    </header>
  );
}
