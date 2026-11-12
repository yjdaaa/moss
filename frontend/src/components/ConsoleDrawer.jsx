import React, { useState, useEffect, useRef } from "react";
import {
  Terminal,
  Radio,
  FileCode,
  Trash2,
  Cpu,
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
  const [gcodeScrollTop, setGcodeScrollTop] = useState(0);
  const consoleFeedRef = useRef(null);
  const gcodeViewerRef = useRef(null);
  const isAutoScrollRef = useRef(true);

  const GCODE_ROW_HEIGHT = 22;
  const GCODE_OVERSCAN = 20;
  const GCODE_VIEWPORT_ROWS = 24;

  // 真实 MCP 状态判断：只有桥当前连上接入点才算在线（configured 只表示填了地址）
  const isMcpConfigured = Boolean(mcpStatus?.configured);
  const isMcpOnline = isMcpConfigured && mcpStatus?.connected === true;
  const mcpEndpoint = mcpStatus?.endpoint || "未配置（在偏好设置填写接入点，或设置 MCP_ENDPOINT）";

  // 拆分 G-code 行
  const gcodeLines = React.useMemo(() => {
    if (!gcodeText) return [];
    return gcodeText.split("\n");
  }, [gcodeText]);

  useEffect(() => {
    setGcodeScrollTop(0);
    if (gcodeViewerRef.current) {
      gcodeViewerRef.current.scrollTop = 0;
    }
  }, [gcodeText]);

  // 控制台自动吸底
  useEffect(() => {
    if (activeTab === "console" && consoleFeedRef.current && isAutoScrollRef.current) {
      consoleFeedRef.current.scrollTop = consoleFeedRef.current.scrollHeight;
    }
  }, [logs, activeTab]);

  // G-code 实时行高亮滚动跟随；切回 G 码标签页且未在跟随时恢复上次手动滚动偏移，避免新挂载容器 scrollTop=0 与虚拟窗口错位
  useEffect(() => {
    if (activeTab !== "gcode" || !gcodeViewerRef.current) return;
    if (currentLine > 0) {
      const targetScrollTop = Math.max(0, (currentLine - 5) * GCODE_ROW_HEIGHT);
      setGcodeScrollTop(targetScrollTop);
      gcodeViewerRef.current.scrollTo({
        top: targetScrollTop,
        behavior: "smooth",
      });
    } else if (gcodeViewerRef.current.scrollTop !== gcodeScrollTop) {
      gcodeViewerRef.current.scrollTop = gcodeScrollTop;
    }
  }, [currentLine, activeTab]); // eslint-disable-line react-hooks/exhaustive-deps

  const gcodeStartIdx = Math.max(
    0,
    Math.min(gcodeLines.length, Math.floor(gcodeScrollTop / GCODE_ROW_HEIGHT) - GCODE_OVERSCAN),
  );
  const gcodeEndIdx = Math.min(
    gcodeLines.length,
    gcodeStartIdx + GCODE_VIEWPORT_ROWS + GCODE_OVERSCAN * 2,
  );

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
          <Terminal size={13} className="drawer-icon" />
          <span title="通信终端与状态监控 (CONSOLE FEED)">终端</span>
        </div>

        <div
          className={`drawer-tab ${activeTab === "mcp" ? "active" : ""}`}
          onClick={() => setActiveTab("mcp")}
          id="tab-mcp-bridge"
        >
          <Radio size={13} className="drawer-icon" />
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
          <FileCode size={13} className="drawer-icon" />
          <span title="G-CODE 实时行视图">G 码行视图</span>
          {gcodeLines.length > 0 && (
            <span
              className="drawer-text"
            >
              [{currentLine}/{gcodeLines.length} 行]
            </span>
          )}
        </div>

        {/* 辅助工具：清空控制台日志 */}
        {activeTab === "console" && onClear && (
          <div className="drawer-row">
            <button
              className="btn btn-ghost drawer-btn"
              onClick={onClear}
              title="清空控制台日志"
            >
              <Trash2 size={12} className="drawer-icon-2" />
              清空终端
            </button>
          </div>
        )}

        {/* 辅助工具：清空 G-code 刀路视图 */}
        {activeTab === "gcode" && onClearGcode && gcodeLines.length > 0 && (
          <div className="drawer-row">
            <button
              className="btn btn-ghost drawer-btn-2"
              onClick={onClearGcode}
              title="清空当前 G-code 刀路视图"
              id="btn-clear-gcode"
            >
              <Trash2 size={12} className="drawer-icon-2" />
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
          <div className="drawer-grid">
            <div className="mcp-card">
              <div className="mcp-card-header">
                <div className="drawer-row-2">
                  {isMcpOnline ? (
                    <Wifi size={16} className="drawer-icon-3" />
                  ) : (
                    <WifiOff size={16} className="drawer-icon-4" />
                  )}
                  <span className="drawer-text-2">WebSocket 协议网关</span>
                </div>
                <span className={`badge ${isMcpOnline ? "badge-success" : "badge-error"}`}>
                  {isMcpOnline ? "ONLINE" : "OFFLINE"}
                </span>
              </div>
              <div className="mcp-card-body">
                <div className="mcp-row">
                  <span className="mcp-label">客户端标识</span>
                  <span className="mcp-val mono drawer-text-3">
                    {mcpStatus?.client_info || "—"}
                  </span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">网关端点</span>
                  <span className="mcp-val mono drawer-text-4">{mcpEndpoint}</span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">协议版本</span>
                  <span className="mcp-val mono">{mcpStatus?.protocol_version ? `MCP ${mcpStatus.protocol_version}` : "未知（尚未握手）"}</span>
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
                <div className="drawer-row-2">
                  <Cpu size={16} className="drawer-icon-5" />
                  <span className="drawer-text-2">AI 工具路由与安全执行</span>
                </div>
                <span className="badge badge-info">{mcpStatus?.tools_count ?? "—"} 项工具</span>
              </div>
              <div className="mcp-card-body">
                <div className="mcp-row">
                  <span className="mcp-label">核心调度工具（固定）</span>
                  <span className="mcp-val mono">{mcpStatus?.core_tool || "laser_workflow_tool"}</span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">最新处理请求</span>
                  <span className="mcp-val mono drawer-text-5">
                    {mcpStatus?.last_request || "—"}
                  </span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">发送门禁（固定规则）</span>
                  <span className="mcp-val drawer-icon-5">
                    {mcpStatus?.safety_gate || "发送前重新计算 G-code 文件 SHA-256，与确认时的快照比对"}
                  </span>
                </div>
                <div className="mcp-row">
                  <span className="mcp-label">急停方式（固定规则）</span>
                  <span className="mcp-val drawer-icon-4">
                    {mcpStatus?.estop_channel || "0x18 软复位走实时通道、不排队；写出成功不代表已停机"}
                  </span>
                </div>
              </div>
            </div>
          </div>

          {/* 真实 JSON-RPC 交互流审计终端 */}
          <div className="drawer-spacer">
            <div className="drawer-row-3">
              <span className="drawer-text-6">
                小智 JSON-RPC 实时交互流 (RPC AUDIT LOG)
              </span>
              <span className="drawer-text-7">
                已收到: {mcpStatus?.messages_count ?? 0} 帧
              </span>
            </div>
            <div className="console-feed drawer-box">
              {mcpStatus?.recent_events && mcpStatus.recent_events.length > 0 ? (
                mcpStatus.recent_events.map((evt, i) => (
                  <div key={i} className="console-line drawer-box-2">
                    <span className="console-msg drawer-text-8">
                      {evt}
                    </span>
                  </div>
                ))
              ) : (
                <div className="console-empty drawer-spacer-2">
                  &gt; 暂无小智 MCP 交互记录
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      {/* Tab 3: G-CODE 实时行视图（虚拟滚动，10 万行光栅刀路只挂载可视区行） */}
      {activeTab === "gcode" && (
        <div
          className="gcode-viewer-container"
          ref={gcodeViewerRef}
          onScroll={(e) => setGcodeScrollTop(e.currentTarget.scrollTop)}
        >
          {gcodeLines.length === 0 ? (
            <div className="console-empty">
              &gt; 暂无 G-code 刀路数据 · 请在左侧配置参数后点击【计算加工轨迹】
            </div>
          ) : (
            <div className="gcode-lines-list">
              {gcodeStartIdx > 0 && (
                <div style={{ height: gcodeStartIdx * GCODE_ROW_HEIGHT }} aria-hidden="true" />
              )}
              {gcodeLines.slice(gcodeStartIdx, gcodeEndIdx).map((lineText, offset) => {
                const idx = gcodeStartIdx + offset;
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
              {gcodeEndIdx < gcodeLines.length && (
                <div style={{ height: (gcodeLines.length - gcodeEndIdx) * GCODE_ROW_HEIGHT }} aria-hidden="true" />
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
