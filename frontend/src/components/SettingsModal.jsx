import React, { useState, useEffect } from "react";
import { Settings, X, ShieldAlert, Monitor, Network, Link2, CheckCircle2, AlertCircle, Eye, EyeOff, Trash2 } from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";

import { FoldHeader } from "./FoldHeader.jsx";
import { ExportSettingsSection } from "./ExportSettingsSection.jsx";
import { PhoneLinkSection } from "./PhoneLinkSection.jsx";
import { isRemoteMode } from "../authSession.js";

export function SettingsModal({ visible, onClose, onOpenVoice = null }) {
  const [closeAction, setCloseAction] = useState("ask");
  const [portInput, setPortInput] = useState("9666");
  const [systemInfo, setSystemInfo] = useState(null);
  const [saveStatus, setSaveStatus] = useState("");
  const [savingPort, setSavingPort] = useState(false);
  const [mcpEndpointInput, setMcpEndpointInput] = useState("");
  const [savingMcpEndpoint, setSavingMcpEndpoint] = useState(false);
  const [mcpSaveStatus, setMcpSaveStatus] = useState("");
  const [jevEnabled, setJevEnabled] = useState(false);
  const [jevApiKeyInput, setJevApiKeyInput] = useState("");
  const [jevMaskedKey, setJevMaskedKey] = useState("");
  const [jevRevealedKey, setJevRevealedKey] = useState("");
  const [jevKeyVisible, setJevKeyVisible] = useState(false);
  const [savingJev, setSavingJev] = useState(false);
  const [testingJev, setTestingJev] = useState(false);
  const [jevSaveStatus, setJevSaveStatus] = useState("");

  useEffect(() => {
    if (!visible) return;

    const saved = localStorage.getItem("moss_close_action") || "ask";
    setCloseAction(saved);

    // 查询系统实际运行端口与首选端口
    getJson("/api/system/info").then((res) => {
      if (res.ok && res.data?.success) {
        const info = res.data.result || {};
        setSystemInfo(info);
        setPortInput(String(info.preferred_port || 9666));
      }
    });

    getJson("/api/jev/status").then((res) => {
      if (res.ok && res.data?.success) {
        const jev = res.data.result || {};
        setJevEnabled(Boolean(jev.enabled));
        setJevMaskedKey(jev.masked_key || "");
      }
    });
  }, [visible]);

  if (!visible) return null;

  const handleActionChange = (action) => {
    setCloseAction(action);
    localStorage.setItem("moss_close_action", action);
  };

  const handleSaveMcpEndpoint = async () => {
    const endpoint = mcpEndpointInput.trim();
    if (endpoint && !/^wss?:\/\//i.test(endpoint)) {
      showAlert("请输入 ws:// 或 wss:// 开头的小智 MCP 地址；留空可关闭桥接", {
        title: "MCP 端点无效",
        tone: "warning",
      });
      return;
    }
    if (endpoint.includes("<")) {
      showAlert("请替换地址中的模板占位符后再保存", { title: "MCP 端点无效", tone: "warning" });
      return;
    }
    setSavingMcpEndpoint(true);
    setMcpSaveStatus("");
    try {
      const res = await postJson("/api/settings", { mcp_endpoint: endpoint });
      setSavingMcpEndpoint(false);
      if (res.ok && res.data?.success) {
        setMcpSaveStatus(endpoint ? "已保存，重启后连接" : "已关闭桥接");
        setMcpEndpointInput("");
        setTimeout(() => setMcpSaveStatus(""), 5000);
      } else {
        showAlert(res.data?.result || "保存失败", { title: "MCP 端点保存失败", tone: "error" });
      }
    } catch (e) {
      setSavingMcpEndpoint(false);
      showAlert(`保存端点网络异常: ${e.message}`, { title: "MCP 端点保存失败", tone: "error" });
    }
  };

  const handleSavePort = async () => {
    const p = Number(portInput.trim());
    if (!p || p < 1024 || p > 65535) {
      showAlert("请输入合法的端口号 (1024 ~ 65535)", { title: "端口无效", tone: "warning" });
      return;
    }
    setSavingPort(true);
    setSaveStatus("");
    try {
      const res = await postJson("/api/settings", { web_port: p });
      setSavingPort(false);
      if (res.ok && res.data?.success) {
        setSaveStatus("✓ 端口已保存，下次启动时生效");
        setTimeout(() => setSaveStatus(""), 4000);
      } else {
        showAlert(res.data?.result || "保存失败", { title: "端口保存失败", tone: "error" });
      }
    } catch (e) {
      setSavingPort(false);
      showAlert(`保存端口网络异常: ${e.message}`, { title: "端口保存失败", tone: "error" });
    }
  };

  const handleRevealJevKey = async () => {
    if (jevKeyVisible) {
      setJevKeyVisible(false);
      setJevRevealedKey("");
      return;
    }
    const res = await postJson("/api/jev/reveal");
    if (res.ok && res.data?.success) {
      setJevRevealedKey(res.data.result?.api_key || "");
      setJevKeyVisible(true);
    } else {
      showAlert(res.data?.result || "Jev API Key 尚未配置", { title: "无法查看 Jev API Key", tone: "warning" });
    }
  };

  const handleSaveJev = async () => {
    setSavingJev(true);
    setJevSaveStatus("");
    const body = {
      jev_enabled: jevEnabled,
    };
    if (jevApiKeyInput.trim()) body.jev_api_key = jevApiKeyInput.trim();
    try {
      const res = await postJson("/api/settings", body);
      if (res.ok && res.data?.success) {
        setJevApiKeyInput("");
        setJevKeyVisible(false);
        setJevRevealedKey("");
        const status = await getJson("/api/jev/status");
        if (status.ok && status.data?.success) setJevMaskedKey(status.data.result?.masked_key || "");
        setJevSaveStatus("已保存");
        setTimeout(() => setJevSaveStatus(""), 4000);
      } else {
        showAlert(res.data?.result || "保存失败", { title: "Jev 设置保存失败", tone: "error" });
      }
    } catch (e) {
      showAlert(`保存 Jev 设置网络异常: ${e.message}`, { title: "Jev 设置保存失败", tone: "error" });
    } finally {
      setSavingJev(false);
    }
  };

  const handleDeleteJevKey = async () => {
    if (savingJev || testingJev) return;
    setSavingJev(true);
    try {
    const confirmed = await showConfirm("删除后需要重新输入 Jev API Key 才能使用判断服务。", {
      title: "删除 Jev API Key",
      tone: "warning",
      danger: true,
      confirmText: "删除",
    });
    if (!confirmed) return;
    const res = await postJson("/api/settings", { jev_delete_api_key: true });
    if (res.ok && res.data?.success) {
      setJevApiKeyInput("");
      setJevMaskedKey("");
      setJevKeyVisible(false);
      setJevRevealedKey("");
      setJevSaveStatus("密钥已删除");
    } else {
      showAlert(res.data?.result || "删除失败", { title: "Jev API Key 删除失败", tone: "error" });
    }
    } finally {
      setSavingJev(false);
    }
  };

  const handleTestJev = async () => {
    setTestingJev(true);
    try {
      const res = await postJson("/api/jev/test");
      if (res.ok && res.data?.success) {
        setJevSaveStatus("连接正常");
      } else {
        showAlert(res.data?.result || "Jev 连接失败", { title: "Jev 连接测试失败", tone: "error" });
      }
    } catch (e) {
      showAlert(`测试 Jev 网络异常: ${e.message}`, { title: "Jev 连接测试失败", tone: "error" });
    } finally {
      setTestingJev(false);
    }
  };

  return (
    <div className="mat-modal-backdrop" style={{ zIndex: 110 }}>
      <div className="mat-modal settings-modal" style={{ maxWidth: "480px" }} role="dialog" aria-modal="true">
        <div className="mat-modal-head">
          <h3 style={{ display: "flex", alignItems: "center", gap: "8px", margin: 0 }}>
            <Settings size={18} style={{ color: "var(--laser, #8b7bff)" }} />
            <span>系统偏好设置</span>
          </h3>
          <button className="mat-modal-close" onClick={onClose} title="关闭">
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          {/* 手机连接（电脑端：允许连接 / 配对码；手机端：连接电脑） */}
          <PhoneLinkSection />

          {/* 连接电脑时这些设置属于电脑，手机只读（后端也会拒绝），直接隐藏 */}
          {!isRemoteMode() && (<>
          {/* 网络与服务端口设置 */}
          <div className="ob-fold" style={{ marginBottom: "20px", paddingBottom: "18px", borderBottom: "1px solid var(--line)" }}>
            <FoldHeader variant="section" id="settings-port" icon={Network} title="服务监听端口 (Web Port)" defaultOpen={false} />

            <div style={{ fontSize: "12px", color: "var(--ink-dim)", marginBottom: "12px" }}>
              上位机 HTTP 服务监听端口，供本桌面端同源访问及手机/局域网连接：
            </div>

            <div style={{ display: "flex", gap: "10px", alignItems: "center", marginBottom: "10px" }}>
              <input
                type="number"
                min={1024}
                max={65535}
                value={portInput}
                onChange={(e) => setPortInput(e.target.value)}
                className="text-input"
                style={{ width: "130px", fontFamily: "var(--font-mono)", fontWeight: "700" }}
                placeholder="9666"
              />
              <button
                type="button"
                className="btn btn-hud-primary"
                onClick={handleSavePort}
                disabled={savingPort}
                style={{ padding: "6px 14px", fontSize: "12px" }}
              >
                <span>{savingPort ? "保存中..." : "保存端口设置"}</span>
              </button>
              {saveStatus && (
                <span style={{ fontSize: "11px", color: "var(--green)", fontWeight: 600 }}>
                  {saveStatus}
                </span>
              )}
            </div>

            {/* 动态端口与降级状态指示 */}
            {systemInfo && (
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: "6px",
                  fontSize: "11px",
                  padding: "6px 10px",
                  borderRadius: "4px",
                  background: systemInfo.is_fallback ? "rgba(255, 170, 0, 0.1)" : "rgba(52, 211, 153, 0.08)",
                  border: `1px solid ${systemInfo.is_fallback ? "rgba(255, 170, 0, 0.3)" : "rgba(52, 211, 153, 0.25)"}`,
                  color: systemInfo.is_fallback ? "var(--gold)" : "var(--green)",
                }}
              >
                {systemInfo.is_fallback ? <AlertCircle size={13} /> : <CheckCircle2 size={13} />}
                <span>
                  {systemInfo.is_fallback
                    ? `首选端口 ${systemInfo.preferred_port} 被占用，当前已自动降级至安全端口: ${systemInfo.actual_port}`
                    : `当前同源通信正常运行于: ${systemInfo.actual_port} 端口 (默认: 9666)`}
                </span>
              </div>
            )}
          </div>

          {/* 小智 MCP 桥接设置：桌面端统一在语音中心修改（技术书 §9.5.1，A-77）；
              独立 moss-host、手机等没有语音中心的环境仍在此填写（/api/settings 保持兼容） */}
          {onOpenVoice ? (
            <div className="ob-fold" id="settings-mcp-moved" style={{ marginBottom: "20px", paddingBottom: "18px", borderBottom: "1px solid var(--line)" }}>
              <FoldHeader variant="section" id="settings-mcp" icon={Link2} title="小智 MCP 网关端点" defaultOpen={false} />
              <div style={{ display: "flex", gap: "10px", alignItems: "center", flexWrap: "wrap", fontSize: "12px", color: "var(--ink-dim)" }}>
                <span>小智连接在语音中心设置（官方 / 自建链路、接入点地址、服务端与模型）。</span>
                <button type="button" className="btn btn-hud-primary" onClick={onOpenVoice} style={{ padding: "6px 14px", fontSize: "12px" }} id="settings-open-voice">
                  前往语音中心
                </button>
              </div>
              {systemInfo?.mcp && (
                <div style={{ fontSize: "11px", color: "var(--ink-dim)", lineHeight: "1.4", marginTop: "8px" }}>
                  当前状态：{systemInfo.mcp.configured ? (systemInfo.mcp.connected ? "已连接" : "已配置，未连接") : "未配置"}
                </div>
              )}
            </div>
          ) : (
            <div className="ob-fold" style={{ marginBottom: "20px", paddingBottom: "18px", borderBottom: "1px solid var(--line)" }}>
              <FoldHeader variant="section" id="settings-mcp" icon={Link2} title="小智 MCP 网关端点" defaultOpen={false} />
              <div style={{ fontSize: "12px", color: "var(--ink-dim)", marginBottom: "12px" }}>
                输入小智控制台生成的完整 WebSocket 地址；保存后重启 MOSS 生效。留空并保存可关闭桥接。
              </div>
              <div style={{ display: "flex", gap: "10px", alignItems: "center", marginBottom: "8px" }}>
                <input
                  type="text"
                  value={mcpEndpointInput}
                  onChange={(e) => setMcpEndpointInput(e.target.value)}
                  className="text-input"
                  style={{ flex: 1, minWidth: 0, fontFamily: "var(--font-mono)", fontSize: "12px" }}
                  placeholder="wss://example.com/mcp/?token=..."
                  spellCheck="false"
                  autoComplete="off"
                />
                <button
                  type="button"
                  className="btn btn-hud-primary"
                  onClick={handleSaveMcpEndpoint}
                  disabled={savingMcpEndpoint}
                  style={{ padding: "6px 14px", fontSize: "12px", whiteSpace: "nowrap" }}
                >
                  {savingMcpEndpoint ? "保存中..." : "保存端点"}
                </button>
              </div>
              {mcpSaveStatus && (
                <div style={{ fontSize: "11px", color: "var(--green)", fontWeight: 600, marginBottom: "8px" }}>
                  {mcpSaveStatus}
                </div>
              )}
              {systemInfo?.mcp && (
                <div style={{ fontSize: "11px", color: "var(--ink-dim)", lineHeight: "1.4" }}>
                  当前状态：{systemInfo.mcp.configured ? (systemInfo.mcp.connected ? "已连接" : "已配置，未连接") : "未配置"}
                  {systemInfo.mcp.endpoint ? `；${systemInfo.mcp.endpoint}` : ""}
                </div>
              )}
            </div>
          )}

          {/* Jev 判断服务设置 */}
          <div className="ob-fold" style={{ marginBottom: "20px", paddingBottom: "18px", borderBottom: "1px solid var(--line)" }}>
            <FoldHeader variant="section" id="settings-jev" icon={Link2} title="OpenRouter · Jev 判断服务" defaultOpen={false} />
            <div style={{ fontSize: "12px", color: "var(--ink-dim)", marginBottom: "12px" }}>
              Jev 只帮助判断“太焦、太浅、切不透”等加工反馈，不直接控制激光机。
            </div>
            <label style={{ display: "flex", alignItems: "center", gap: "10px", marginBottom: "10px", fontSize: "13px", color: "var(--ink)" }}>
              <input type="checkbox" checked={jevEnabled} onChange={(e) => setJevEnabled(e.target.checked)} />
              <span>启用 Jev 判断</span>
            </label>
            <div style={{ fontSize: "12px", marginBottom: "8px", overflowWrap: "anywhere" }}>
              typesafe/jev-1.13
            </div>
            <div style={{ display: "flex", gap: "8px", alignItems: "center", marginBottom: "8px" }}>
              <input
                type={jevKeyVisible ? "text" : "password"}
                value={jevKeyVisible ? jevRevealedKey : jevApiKeyInput}
                onChange={(e) => {
                  if (jevKeyVisible) {
                    setJevRevealedKey(e.target.value);
                    setJevApiKeyInput(e.target.value);
                  } else {
                    setJevApiKeyInput(e.target.value);
                  }
                }}
                className="text-input"
                style={{ flex: 1, minWidth: 0, fontFamily: "var(--font-mono)", fontSize: "12px" }}
                placeholder={jevMaskedKey || "输入 OpenRouter API Key"}
                spellCheck="false"
                autoComplete="new-password"
              />
              <button
                type="button"
                className="btn btn-ghost"
                onClick={handleRevealJevKey}
                title={jevKeyVisible ? "隐藏 Jev API Key" : "查看 Jev API Key"}
                aria-label={jevKeyVisible ? "隐藏 Jev API Key" : "查看 Jev API Key"}
                style={{ padding: "7px 9px" }}
              >
                {jevKeyVisible ? <EyeOff size={15} /> : <Eye size={15} />}
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={handleDeleteJevKey}
                disabled={savingJev || testingJev}
                title="删除 Jev API Key"
                aria-label="删除 Jev API Key"
                style={{ padding: "7px 9px", color: "var(--red, #ff3b5c)" }}
              >
                <Trash2 size={15} />
              </button>
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: "10px" }}>
              <button
                type="button"
                className="btn btn-hud-primary"
                onClick={handleSaveJev}
                disabled={savingJev}
                style={{ padding: "6px 14px", fontSize: "12px" }}
              >
                {savingJev ? "保存中..." : "保存 Jev 设置"}
              </button>
              <button
                type="button"
                className="btn btn-ghost"
                onClick={handleTestJev}
                disabled={testingJev || savingJev}
                style={{ padding: "6px 12px", fontSize: "12px" }}
              >
                {testingJev ? "测试中..." : "测试连接"}
              </button>
              {jevSaveStatus && <span style={{ fontSize: "11px", color: "var(--green)", fontWeight: 600 }}>{jevSaveStatus}</span>}
            </div>
          </div>

          </>)}

          {/* 导出目录（仅桌面版） */}
          <ExportSettingsSection />

          {/* 桌面客户端关闭行为 */}
          <div className="ob-fold" style={{ marginBottom: "18px" }}>
            <FoldHeader variant="section" id="settings-close" icon={Monitor} title="窗口关闭行为" defaultOpen={false} />

            <div style={{ fontSize: "12px", color: "var(--ink-dim)", marginBottom: "12px" }}>
              点击主窗口右上角关闭按钮（[X]）时的默认行为：
            </div>

            <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
              <label
                style={{
                  display: "flex",
                  alignItems: "flex-start",
                  gap: "10px",
                  padding: "8px 12px",
                  borderRadius: "6px",
                  background: closeAction === "ask" ? "rgba(139, 123, 255, 0.08)" : "rgba(255, 255, 255, 0.02)",
                  border: `1px solid ${closeAction === "ask" ? "rgba(139, 123, 255, 0.4)" : "var(--line)"}`,
                  cursor: "pointer"
                }}
              >
                <input
                  type="radio"
                  name="settingCloseAction"
                  value="ask"
                  checked={closeAction === "ask"}
                  onChange={() => handleActionChange("ask")}
                  style={{ marginTop: "3px" }}
                />
                <div>
                  <div style={{ fontSize: "13px", fontWeight: 600, color: "var(--ink)" }}>
                    每次询问（推荐）
                  </div>
                  <div style={{ fontSize: "11px", color: "var(--ink-dim)" }}>
                    点击关闭时弹出对话框，由您临时决定是最小化还是完全退出
                  </div>
                </div>
              </label>

              <label
                style={{
                  display: "flex",
                  alignItems: "flex-start",
                  gap: "10px",
                  padding: "8px 12px",
                  borderRadius: "6px",
                  background: closeAction === "tray" ? "rgba(139, 123, 255, 0.08)" : "rgba(255, 255, 255, 0.02)",
                  border: `1px solid ${closeAction === "tray" ? "rgba(139, 123, 255, 0.4)" : "var(--line)"}`,
                  cursor: "pointer"
                }}
              >
                <input
                  type="radio"
                  name="settingCloseAction"
                  value="tray"
                  checked={closeAction === "tray"}
                  onChange={() => handleActionChange("tray")}
                  style={{ marginTop: "3px" }}
                />
                <div>
                  <div style={{ fontSize: "13px", fontWeight: 600, color: "var(--ink)" }}>
                    最小化到系统托盘
                  </div>
                  <div style={{ fontSize: "11px", color: "var(--ink-dim)" }}>
                    直接隐藏窗口到右下角托盘区，后台作业不中断，可在托盘右键退出
                  </div>
                </div>
              </label>

              <label
                style={{
                  display: "flex",
                  alignItems: "flex-start",
                  gap: "10px",
                  padding: "8px 12px",
                  borderRadius: "6px",
                  background: closeAction === "exit" ? "rgba(255, 59, 92, 0.08)" : "rgba(255, 255, 255, 0.02)",
                  border: `1px solid ${closeAction === "exit" ? "rgba(255, 59, 92, 0.4)" : "var(--line)"}`,
                  cursor: "pointer"
                }}
              >
                <input
                  type="radio"
                  name="settingCloseAction"
                  value="exit"
                  checked={closeAction === "exit"}
                  onChange={() => handleActionChange("exit")}
                  style={{ marginTop: "3px" }}
                />
                <div>
                  <div style={{ fontSize: "13px", fontWeight: 600, color: "var(--ink)" }}>
                    完全退出程序
                  </div>
                  <div style={{ fontSize: "11px", color: "var(--ink-dim)" }}>
                    直接安全停止激光并杀死主进程（适合习惯直接关闭窗口的用户）
                  </div>
                </div>
              </label>
            </div>
          </div>

          <div style={{ padding: "10px 12px", background: "rgba(255, 170, 0, 0.08)", border: "1px solid rgba(255, 170, 0, 0.2)", borderRadius: "6px", display: "flex", gap: "8px", alignItems: "flex-start" }}>
            <ShieldAlert size={16} style={{ color: "var(--gold)", flexShrink: 0, marginTop: "2px" }} />
            <div style={{ fontSize: "11px", color: "var(--gold)", lineHeight: "1.4" }}>
              <strong>安全机制提示</strong>：当选择完全退出程序时，系统会自动向激光下位机发送 0x18 软复位指令并清零任务，确保激光器熄灭断电，杜绝静止出光火灾隐患。
            </div>
          </div>
        </div>

        <div className="mat-modal-foot">
          <button className="btn btn-hud-primary" onClick={onClose} style={{ padding: "8px 24px" }}>
            完成设置
          </button>
        </div>
      </div>
    </div>
  );
}
