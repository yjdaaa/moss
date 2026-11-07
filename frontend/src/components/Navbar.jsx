import React, { useLayoutEffect, useRef } from "react";
import { Zap, Radio, OctagonAlert, ShieldCheck } from "lucide-react";
import { mcpPillState } from "../state/mcpStatus.js";
import { hardwarePillView } from "../state/hardwareStatus.js";

// 顶栏（技术书 §9.5.5 B）：页面标题 + 硬件状态 + 小智四态 + 审批（仅桌面端）+ 常驻急停。

const PAGE_TITLES = {
  "/": { title: "雕刻工作台", sub: "LASER FORGE" },
  "/draw": { title: "创作画板", sub: "CANVAS" },
  "/material-lab": { title: "材料实验室", sub: "MATERIAL LAB" },
  "/twin": { title: "数字孪生舱", sub: "DIGITAL TWIN 3D" },
  "/camera": { title: "摄像头", sub: "CAMERA VIEW" },
  "/voice": { title: "语音中心", sub: "VOICE CENTER" },
};

const MCP_PILL_CLASS = {
  unconfigured: "ob-pill--off",
  disconnected: "ob-pill--bad",
  waiting_device: "ob-pill--warn",
  connected: "ob-pill--info",
};

export function Navbar({
  currentPath,
  onNavigate,
  connectionStatus,
  mcpStatus,
  voiceStatus,
  onEmergencyStop,
  showApprovals = false,
  approvalCount = 0,
  onApprovalsClick,
  hideStatusPills = false,
  updateNotice = null,
}) {
  const page = PAGE_TITLES[currentPath] || PAGE_TITLES["/"];
  const mcpPill = mcpPillState(mcpStatus, voiceStatus);
  const pillClass =
    mcpPill.key === "connected" && mcpPill.link === "自建" ? "ob-pill--ok" : MCP_PILL_CLASS[mcpPill.key];
  const hardware = hardwarePillView(connectionStatus);
  const headerRef = useRef(null);

  // 顶栏实际高度写进 CSS 变量 --ob-top-h，页面里的吸顶元素（如语音中心标签栏）贴在它下面；顶栏换行时也跟着变
  useLayoutEffect(() => {
    const element = headerRef.current;
    if (!element) return undefined;
    const apply = () => document.documentElement.style.setProperty("--ob-top-h", `${element.offsetHeight}px`);
    apply();
    if (typeof ResizeObserver === "undefined") return undefined;
    const observer = new ResizeObserver(apply);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  return (
    <header className="ob-top" ref={headerRef}>
      <h1 className="ob-title">
        {page.title}
        <small>{page.sub}</small>
      </h1>
      <div className="ob-top-spacer" />
      {updateNotice}

      {!hideStatusPills && (
        <>
          <button
            type="button"
            onClick={() => onNavigate("/")}
            className={`ob-pill conn-pill ${hardware.online ? "ob-pill--ok online" : "ob-pill--bad offline"}`}
            title={hardware.title}
            id="nav-hardware-status"
          >
            <Zap size={13} aria-hidden="true" />
            <span>{hardware.text}</span>
          </button>

          <div
            className={`ob-pill conn-pill mcp-pill--${mcpPill.key} ${pillClass}`}
            title={
              mcpPill.key === "unconfigured"
                ? "小智 MCP 网关未配置（桌面端在语音中心填写接入点；独立 moss-host 在偏好设置中填写，或设置 MCP_ENDPOINT）"
                : `小智 MCP 网关：${mcpPill.label}${mcpStatus?.client_info ? ` · ${mcpStatus.client_info}` : ""}${mcpStatus?.endpoint ? ` · ${mcpStatus.endpoint}` : ""}`
            }
            id="nav-mcp-status"
            data-state={mcpPill.key}
          >
            <Radio size={13} aria-hidden="true" />
            <span>{mcpPill.text}</span>
          </div>

          {showApprovals && (
            <button
              type="button"
              className={`ob-pill approval-pill ${approvalCount > 0 ? "ob-pill--bad" : "ob-pill--off"}`}
              onClick={onApprovalsClick}
              id="nav-approvals"
              title={approvalCount > 0 ? `${approvalCount} 个加工请求等待在本机批准` : "当前没有待批准的请求"}
            >
              <ShieldCheck size={13} aria-hidden="true" />
              <span>审批</span>
              {approvalCount > 0 && (
                <span className="approval-count" aria-label={`${approvalCount} 个待批准`}>{approvalCount}</span>
              )}
            </button>
          )}
        </>
      )}

      <button
        type="button"
        className="ob-estop"
        onClick={onEmergencyStop}
        id="nav-estop"
        title="向当前页面正在下发的任务请求 0x18 软件复位（与工作台急停为同一动作）；不能替代控制器硬件急停"
      >
        <OctagonAlert size={16} aria-hidden="true" />
        <span>急停 0x18</span>
      </button>
    </header>
  );
}
