import React, { useState, useEffect, useRef } from "react";
import {
  Terminal,
  Radio,
  FileCode,
  Trash2,
  CheckCircle2,
  AlertCircle,
  Cpu,
  ArrowRight,
  Wifi,
  WifiOff,
} from "lucide-react";

export function ConsoleDrawer({
  logs = [],
  onClear,
  gcodeText = "",
  onClearGcode,
  currentLine = 0,
  totalLines = 0,
  mcpStatus = null,
}) {
  const [activeTab, setActiveTab] = useState("console"); // console | mcp | gcode
  const consoleFeedRef = useRef(null);
  const gcodeViewerRef = useRef(null);
  const isAutoScrollRef = useRef(true);

  // 真实 MCP 状态判断：只有桥当前连上接入点才算在线（configured 只表示填了地址）
  const isMcpConfigured = Boolean(mcpStatus?.configured);
  const isMcpOnline = isMcpConfigured && mcpStatus?.connected === true;
  const mcpEndpoint = mcpStatus?.endpoint || "未配置（在偏好设置填写接入点，或设置 MCP_ENDPOINT）";

  // 拆分 G-code 行
  const gcodeLines = React.useMemo(() => {
    if (!gcodeText) return [];
    return gcodeText.split("\n");
  }, [gcodeText]);

  // 控制台自动吸底
  useEffect(() => {
    if (activeTab === "console" && consoleFeedRef.current && isAutoScrollRef.current) {
      consoleFeedRef.current.scrollTop = consoleFeedRef.current.scrollHeight;
    }
  }, [logs, activeTab]);

  // G-code 实时行高亮滚动跟随
  useEffect(() => {
    if (activeTab === "gcode" && gcodeViewerRef.current && currentLine > 0) {
      const lineHeight = 22; // 每行高度约 22px
      const targetScrollTop = Math.max(0, (currentLine - 5) * lineHeight);
      gcodeViewerRef.current.scrollTo({
        top: targetScrollTop,
        behavior: "smooth",
      });
    }
  }, [currentLine, activeTab]);

  // 格式化 Tag 色彩类
  const getTagClass = (tag = "") => {
    const t = tag.toUpperCase();
    if (t.includes("DISPATCH") || t.includes("STREAM")) return "tag-cyan";
    if (t.includes("SUCCESS") || t.includes("OK")) return "tag-green";
    if (t.includes("ESTOP") || t.includes("ERROR") || t.includes("ALARM") || t.includes("CANCEL")) return "tag-red";
    if (t.includes("GRBL") || t.includes("DEVICE") || t.includes("GATE") || t.includes("SERIAL")) return "tag-yellow";
    if (t.includes("MCP") || t.includes("AI") || t.includes("PREVIEW")) return "tag-purple";
    if (t.includes("FILE") || t.includes("GCODE")) return "tag-blue";
    return "tag-blue";
  };

  const getMsgClass = (level = "info") => {
    if (level === "ok" || level === "success") return "msg-success";
    if (level === "error" || level === "fail") return "msg-error";
    if (level === "warn") return "msg-warn";
    return "msg-info";
  };

  return (
    <div className="bottom-drawer" id="bottomDrawer">
      {/* 抽屉导航 Tab 栏 */}
      <div className="drawer-nav">
        <div
          className={`drawer-tab ${activeTab === "console" ? "active" : ""}`}
          onClick={() => setActiveTab("console")}
          id="tab-console-feed"
        >
          <Terminal size={13} style={{ marginRight: 6 }} />
          <span title="通信终端与状态监控 (CONSOLE FEED)">终端</span>
        </div>

        <div
          className={`drawer-tab ${activeTab === "mcp" ? "active" : ""}`}
          onClick={() => setActiveTab("mcp")}
          id="tab-mcp-bridge"
        >
          <Radio size={13} style={{ marginRight: 6 }} />
          <span title={`小智 MCP 桥接状态（${isMcpOnline ? "已连接" : "未连接"}）`}>小智网关</span>
          <span
            style={{
              display: "inline-block",
              width: 6,
              height: 6,
              borderRadius: "50%",
              background: isMcpOnline ? "var(--laser-green, #34d399)" : "var(--laser-red, #ff3b5c)",
              marginLeft: 6,
              boxShadow: isMcpOnline ? "0 0 6px var(--laser-green, #34d399)" : "none",
            }}
          />
        </div>

        <div
          className={`drawer-tab ${activeTab === "gcode" ? "active" : ""}`}
          onClick={() => setActiveTab("gcode")}
          id="tab-gcode-view"
        >
          <FileCode size={13} style={{ marginRight: 6 }} />
          <span title="G-CODE 实时行视图">G 码行视图</span>
          {gcodeLines.length > 0 && (
            <span
              style={{
                marginLeft: 6,
                fontSize: "11px",
                fontFamily: "var(--font-mono)",
                color: "var(--laser-cyan, #8b7bff)",
              }}
            >
              [{currentLine}/{gcodeLines.length} 行]
            </span>
          )}
        </div>

        {/* 辅助工具：清空控制台日志 */}
        {activeTab === "console" && onClear && (
          <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", paddingRight: 10 }}>
            <button
              className="btn btn-ghost"
              style={{ padding: "2px 8px", fontSize: "11px", height: "auto", color: "var(--text-muted, #94a3b8)" }}
              onClick={onClear}
              title="清空控制台日志"
            >
              <Trash2 size={12} style={{ marginRight: 4 }} />
              清空终端
            </button>
          </div>
        )}

        {/* 辅助工具：清空 G-code 刀路视图 */}
        {activeTab === "gcode" && onClearGcode && gcodeLines.length > 0 && (
          <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", paddingRight: 10 }}>
            <button
              className="btn btn-ghost"
              style={{ padding: "2px 8px", fontSize: "11px", height: "auto", color: "var(--laser-cyan, #8b7bff)", borderColor: "rgba(139, 123, 255, 0.3)" }}
              onClick={onClearGcode}
              title="清空当前 G-code 刀路视图"
              id="btn-clear-gcode"
            >
              <Trash2 size={12} style={{ marginRight: 4 }} />
              清空视图 ({gcodeLines.length} 行)
            </button>
          </div>
        )}
      </div>

      {/* Tab 1: 通信终端与状态监控 */}
      {activeTab === "console" && (
        <div
          className="console-feed"
          id="consoleFeed"
          ref={consoleFeedRef}
          onScroll={(e) => {
            const { scrollTop, scrollHeight, clientHeight } = e.currentTarget;
            // 距离底部 30px 内认为是在底部
            isAutoScrollRef.current = scrollHeight - scrollTop - clientHeight < 30;
          }}
        >
          {logs.length === 0 ? (
            <div className="console-empty">
              &gt; 控制台就绪 · 启动 JobWorker 逐行下发后，底层串口/TCP 通信与 ok 遥测流将在此实时呈现...
            </div>
          ) : (
            logs.map((item, idx) => (
              <div key={idx} className="console-line">
                <span className="console-time">{item.time || "--:--:--"}</span>
                <span className={`console-tag ${getTagClass(item.tag)}`}>
                  [{item.tag || "HOST"}]
                </span>
                <span className={`console-msg ${getMsgClass(item.level)}`}>
                  {item.message}
                </span>
              </div>
            ))
          )}
        </div>
      )}

      {/* Tab 2: 小智 MCP 桥接状态 */}
      {activeTab === "mcp" && (
        <div className="mcp-bridge-panel">
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))", gap: 14 }}>
            <div className="mcp-card">
              <div className="mcp-card-header">
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  {isMcpOnline ? (
                    <Wifi size={16} style={{ color: "var(--laser-green, #34d399)" }} />
                  ) : (
                    <WifiOff size={16} style={{ color: "var(--laser-red, #ff3b5c)" }} />
                  )}
                  <span style={{ fontWeight: 700, fontSize: "13px" }}>WebSocket 协议网关</span>
                </div>
                <span className={`badge ${isMcpOnline ? "badge-success" : "badge-error"}`}>
                  {isMcpOnline ? "ONLINE" : "OFFLINE"}
                </span>
              </div>
              <div className="mcp-card-body">
                <div className="mcp-row">
                  <span className="mcp-label">客户端标识</span>
                  <span className="mcp-val mono" style={{ fontSize: "11px", color: "var(--laser-cyan, #8b7bff)" }}>
                    {mcpStatus?.client_info || "—"}
                  </span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">网关端点</span>
                  <span className="mcp-val mono" style={{ fontSize: "11px" }}>{mcpEndpoint}</span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">协议版本</span>
                  <span className="mcp-val mono">{mcpStatus?.protocol_version || "JSON-RPC 2.0 · MCP 2024-11-05"}</span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">连接状态</span>
                  <span className="mcp-val" style={{ color: isMcpOnline ? "var(--laser-green, #34d399)" : "var(--ink-dim, #94a3b8)" }}>
                    {isMcpOnline ? "已连接接入点" : isMcpConfigured ? "未连接（正在重连或接入点不可达）" : "未配置"}
                  </span>
                </div>
              </div>
            </div>

            <div className="mcp-card">
              <div className="mcp-card-header">
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <Cpu size={16} style={{ color: "var(--laser-cyan, #8b7bff)" }} />
                  <span style={{ fontWeight: 700, fontSize: "13px" }}>AI 工具路由与安全执行</span>
                </div>
                <span className="badge badge-info">{mcpStatus?.tools_count ?? "—"} 项工具</span>
              </div>
              <div className="mcp-card-body">
                <div className="mcp-row">
                  <span className="mcp-label">核心调度工具</span>
                  <span className="mcp-val mono">{mcpStatus?.core_tool || "laser_workflow_tool"}</span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">最新处理请求</span>
                  <span className="mcp-val mono" style={{ fontSize: "11px", color: "var(--laser-green, #34d399)" }}>
                    {mcpStatus?.last_request || "—"}
                  </span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">安全门禁校验</span>
                  <span className="mcp-val" style={{ color: "var(--laser-cyan, #8b7bff)" }}>
                    {mcpStatus?.safety_gate || "强制物理 SHA-256 指纹比对"}
                  </span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">急停拦截信道</span>
                  <span className="mcp-val mono" style={{ color: "var(--laser-red, #ff3b5c)" }}>
                    {mcpStatus?.estop_channel || "0x18 软复位 (优先中断级)"}
                  </span>
                </div>
              </div>
            </div>
          </div>

          {/* 真实 JSON-RPC 交互流审计终端 */}
          <div style={{ marginTop: 14 }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 6 }}>
              <span style={{ fontSize: "11px", fontWeight: 700, color: "var(--laser-cyan, #8b7bff)", letterSpacing: "0.5px" }}>
                小智 JSON-RPC 实时交互流 (RPC AUDIT LOG)
              </span>
              <span style={{ fontSize: "10px", fontFamily: "var(--font-mono)", color: "var(--ink-dim, #94a3b8)" }}>
                已收到: {mcpStatus?.messages_count ?? 0} 帧
              </span>
            </div>
            <div className="console-feed" style={{ minHeight: "100px", maxHeight: "140px", padding: "8px 12px" }}>
              {mcpStatus?.recent_events && mcpStatus.recent_events.length > 0 ? (
                mcpStatus.recent_events.map((evt, i) => (
                  <div key={i} className="console-line" style={{ fontSize: "11px", borderBottom: "1px solid rgba(255,255,255,0.03)" }}>
                    <span className="console-msg" style={{ fontFamily: "var(--font-mono)", color: "#c4b5fd" }}>
                      {evt}
                    </span>
                  </div>
                ))
              ) : (
                <div className="console-empty" style={{ padding: "10px 0" }}>
                  &gt; 暂无小智 MCP 交互记录
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      {/* Tab 3: G-CODE 实时行视图 */}
      {activeTab === "gcode" && (
        <div className="gcode-viewer-container" ref={gcodeViewerRef}>
          {gcodeLines.length === 0 ? (
            <div className="console-empty">
              &gt; 暂无 G-code 刀路数据 · 请在左侧配置参数后点击【计算加工轨迹】
            </div>
          ) : (
            <div className="gcode-lines-list">
              {gcodeLines.map((lineText, idx) => {
                const lineNum = idx + 1;
                const isCurrent = lineNum === currentLine;
                const isPassed = lineNum < currentLine;

                return (
                  <div
                    key={idx}
                    className={`gcode-line ${isCurrent ? "active-line" : ""} ${isPassed ? "passed-line" : ""}`}
                  >
                    <span className="gcode-lineno">{lineNum}</span>
                    <span className="gcode-pointer">{isCurrent ? "►" : " "}</span>
                    <span className="gcode-code">{lineText}</span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
