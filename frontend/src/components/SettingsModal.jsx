import React, { useState, useEffect } from "react";
import { Settings, X, ShieldAlert, Monitor, Network, CheckCircle2, AlertCircle } from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { showAlert } from "../appDialog.js";

export function SettingsModal({ visible, onClose }) {
  const [closeAction, setCloseAction] = useState("ask");
  const [portInput, setPortInput] = useState("9666");
  const [systemInfo, setSystemInfo] = useState(null);
  const [saveStatus, setSaveStatus] = useState("");
  const [savingPort, setSavingPort] = useState(false);

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
  }, [visible]);

  if (!visible) return null;

  const handleActionChange = (action) => {
    setCloseAction(action);
    localStorage.setItem("moss_close_action", action);
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

  return (
    <div className="mat-modal-backdrop" style={{ zIndex: 110 }}>
      <div className="mat-modal" style={{ maxWidth: "480px" }} role="dialog">
        <div className="mat-modal-head">
          <h3 style={{ display: "flex", alignItems: "center", gap: "8px", margin: 0 }}>
            <Settings size={18} style={{ color: "var(--laser, #00e5ff)" }} />
            <span>系统偏好设置</span>
          </h3>
          <button className="mat-modal-close" onClick={onClose} title="关闭">
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          {/* 网络与服务端口设置 */}
          <div style={{ marginBottom: "20px", paddingBottom: "18px", borderBottom: "1px solid var(--line)" }}>
            <div style={{ display: "flex", alignItems: "center", gap: "6px", fontWeight: 700, fontSize: "14px", color: "var(--ink)", marginBottom: "6px" }}>
              <Network size={16} style={{ color: "var(--laser, #00e5ff)" }} />
              <span>服务监听端口 (Web Port)</span>
            </div>

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
                  background: systemInfo.is_fallback ? "rgba(255, 170, 0, 0.1)" : "rgba(0, 245, 155, 0.08)",
                  border: `1px solid ${systemInfo.is_fallback ? "rgba(255, 170, 0, 0.3)" : "rgba(0, 245, 155, 0.25)"}`,
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

          {/* 桌面客户端关闭行为 */}
          <div style={{ marginBottom: "18px" }}>
            <div style={{ display: "flex", alignItems: "center", gap: "6px", fontWeight: 700, fontSize: "14px", color: "var(--ink)", marginBottom: "6px" }}>
              <Monitor size={16} style={{ color: "var(--laser, #00e5ff)" }} />
              <span>窗口关闭行为</span>
            </div>

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
                  background: closeAction === "ask" ? "rgba(0, 229, 255, 0.08)" : "rgba(255, 255, 255, 0.02)",
                  border: `1px solid ${closeAction === "ask" ? "rgba(0, 229, 255, 0.4)" : "var(--line)"}`,
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
                  background: closeAction === "tray" ? "rgba(0, 229, 255, 0.08)" : "rgba(255, 255, 255, 0.02)",
                  border: `1px solid ${closeAction === "tray" ? "rgba(0, 229, 255, 0.4)" : "var(--line)"}`,
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
                  background: closeAction === "exit" ? "rgba(255, 43, 66, 0.08)" : "rgba(255, 255, 255, 0.02)",
                  border: `1px solid ${closeAction === "exit" ? "rgba(255, 43, 66, 0.4)" : "var(--line)"}`,
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
