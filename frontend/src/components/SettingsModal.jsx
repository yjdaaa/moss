import React, { useState, useEffect } from "react";
import { Settings, X, ShieldAlert, Monitor, Network, Link2, CheckCircle2, AlertCircle, Eye, EyeOff, Trash2, RefreshCw } from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";

import { FoldHeader } from "./FoldHeader.jsx";
import { ExportSettingsSection } from "./ExportSettingsSection.jsx";
import { PhoneLinkSection } from "./PhoneLinkSection.jsx";
import { isRemoteMode } from "../authSession.js";
import { readHideTopPills, writeHideTopPills } from "../foldState.js";
import { trayActionCopy } from "../platform.js";
import { appUpdateActionLabel, appUpdateAvailable, appUpdateBusy, appUpdateInstall, appUpdateProgressView, appUpdateScopeProgress, appUpdateStatus } from "../appUpdate.js";

export function SettingsModal({
  visible,
  onClose,
  onOpenVoice = null,
  hideTopPills: hideTopPillsProp,
  onHideTopPillsChange = null,
  initialUpdateReport = null,
  focusUpdateSection = false,
  onUpdateReport = null,
}) {
  const [closeAction, setCloseAction] = useState("ask");
  const [hideTopPills, setHideTopPills] = useState(() =>
    typeof hideTopPillsProp === "boolean" ? hideTopPillsProp : readHideTopPills()
  );
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
  const [autoCheckUpdate, setAutoCheckUpdate] = useState(true);
  const [checkPrerelease, setCheckPrerelease] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateReport, setUpdateReport] = useState(() => initialUpdateReport);
  const [updateError, setUpdateError] = useState("");
  // 应用内更新（2026-11-07）：supported/unsupported_reason/platform 来自客户端主窗口，progress 为下载安装进度
  const [installInfo, setInstallInfo] = useState(null);
  const [installProgress, setInstallProgress] = useState(null);

  useEffect(() => {
    if (!visible) return;
    if (
      initialUpdateReport &&
      (!updateReport ||
        (initialUpdateReport.has_update && initialUpdateReport.latest_version !== updateReport.latest_version))
    ) {
      setUpdateReport(initialUpdateReport);
    }
    if (focusUpdateSection) {
      requestAnimationFrame(() => {
        document.getElementById("settings-update-section")?.scrollIntoView({ behavior: "smooth", block: "nearest" });
      });
    }
  }, [visible, initialUpdateReport, focusUpdateSection, updateReport]);

  useEffect(() => {
    if (!visible) return;

    const saved = localStorage.getItem("moss_close_action") || "ask";
    setCloseAction(saved);
    setHideTopPills(typeof hideTopPillsProp === "boolean" ? hideTopPillsProp : readHideTopPills());

    // 查询系统实际运行端口、首选端口与更新设置
    getJson("/api/system/info").then((res) => {
      if (res.ok && res.data?.success) {
        const info = res.data.result || {};
        setSystemInfo(info);
        setPortInput(String(info.preferred_port || 9666));
        setAutoCheckUpdate(Boolean(info.auto_check_update ?? true));
        setCheckPrerelease(Boolean(info.check_prerelease));
      }
    });
    if (!onOpenVoice) {
      getJson("/api/jev/status").then((res) => {
        if (res.ok && res.data?.success) {
          const jev = res.data.result || {};
          setJevEnabled(Boolean(jev.enabled));
          setJevMaskedKey(jev.masked_key || "");
        }
      });
    }
  }, [visible, hideTopPillsProp, onOpenVoice]);

  useEffect(() => {
    if (!updateReport?.has_update || !appUpdateAvailable()) return;
    let alive = true;
    appUpdateStatus()
      .then((info) => {
        if (alive) {
          setInstallInfo(info);
          setInstallProgress(appUpdateScopeProgress(info, updateReport.latest_version));
        }
      })
      .catch((e) => { if (alive) setInstallInfo({ supported: false, unsupported_reason: e.message }); });
    return () => { alive = false; };
  }, [updateReport]);

  useEffect(() => {
    if (!appUpdateBusy(installProgress)) return;
    const timer = setInterval(() => {
      appUpdateStatus().then(setInstallProgress).catch(() => {});
    }, 500);
    return () => clearInterval(timer);
  }, [installProgress?.phase]);

  if (!visible) return null;

  const activeUpdateProgress = appUpdateScopeProgress(installProgress, updateReport?.latest_version);

  const handleActionChange = (action) => {
    setCloseAction(action);
    localStorage.setItem("moss_close_action", action);
  };

  const handleHideTopPillsChange = (checked) => {
    const next = Boolean(checked);
    setHideTopPills(next);
    writeHideTopPills(next);
    if (onHideTopPillsChange) {
      onHideTopPillsChange(next);
    }
  };

  const handleToggleUpdatePref = async (key, checked) => {
    const next = Boolean(checked);
    if (key === "auto_check_update") setAutoCheckUpdate(next);
    if (key === "check_prerelease") setCheckPrerelease(next);
    try {
      const res = await postJson("/api/settings", { [key]: next });
      if (!(res.ok && res.data?.success)) {
        if (key === "auto_check_update") setAutoCheckUpdate(!next);
        if (key === "check_prerelease") setCheckPrerelease(!next);
        await showAlert(res.data?.result || "保存失败", { title: "更新设置保存失败", tone: "error" });
      }
    } catch (e) {
      if (key === "auto_check_update") setAutoCheckUpdate(!next);
      if (key === "check_prerelease") setCheckPrerelease(!next);
      await showAlert(`保存网络异常: ${e.message}`, { title: "更新设置保存失败", tone: "error" });
    }
  };

  const handleInstallUpdate = async () => {
    if (!updateReport?.has_update || appUpdateBusy(activeUpdateProgress)) return;
    const version = updateReport.latest_version;
    const isAndroid = installInfo?.platform === "android";
    if (!(isAndroid && activeUpdateProgress?.phase === "installer_launched")) {
      const ok = await showConfirm(
        isAndroid
          ? `下载 MOSS v${version} 安装包并校验签名，完成后打开系统安装界面由您确认升级。\n\n设备正在加工或等待确认停机时不能更新。`
          : `下载 MOSS v${version}，校验签名后安装并自动重启。\n\n` +
            "重启前会像“完全退出”一样先停止激光任务通道和语音服务；设备正在加工或等待确认停机时不能更新。",
        {
          title: isAndroid ? "下载并安装更新？" : "下载并重启更新？",
          confirmText: isAndroid ? "下载并安装" : "下载并更新",
        },
      );
      if (!ok) return;
    }
    try {
      setInstallProgress(await appUpdateInstall(`v${version}`));
    } catch (e) {
      setInstallProgress({ phase: "error", version, error: e.message });
    }
  };

  const handleCheckUpdateNow = async () => {
    if (checkingUpdate) return;
    setCheckingUpdate(true);
    setUpdateError("");
    try {
      const res = await postJson("/api/system/check-update", { include_prerelease: checkPrerelease });
      if (res.ok && res.data?.success && res.data?.result) {
        setUpdateReport(res.data.result);
        onUpdateReport?.(res.data.result);
      } else {
        setUpdateError(res.data?.result || "检查更新失败，请稍后重试");
      }
    } catch (e) {
      setUpdateError(`检查更新网络异常: ${e.message}`);
    } finally {
      setCheckingUpdate(false);
    }
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
    <div className="mat-modal-backdrop settings-box">
      <div className="mat-modal settings-modal settings-box-2" role="dialog" aria-modal="true">
        <div className="mat-modal-head">
          <h3 className="settings-heading">
            <Settings size={18} className="settings-icon" />
            <span>系统偏好设置</span>
          </h3>
          <button className="mat-modal-close" onClick={onClose} title="关闭">
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          {/* 顶栏右上角状态显示 */}
          <div className="ob-fold settings-box-3" id="settings-top-pills-section">
            <FoldHeader variant="section" id="settings-top-pills" icon={EyeOff} title="顶栏右上角状态显示" defaultOpen={true} />
            <div className="settings-box-4">
              开启后隐藏页面右上角的「硬件」「小智」「审批」三个状态胶囊（未开启时保持原样显示；常驻「急停 0x18」按钮与新审批自动弹窗保持不变）：
            </div>
            <label className="settings-label">
              <input
                type="checkbox"
                id="settings-hide-top-pills"
                checked={hideTopPills}
                onChange={(e) => handleHideTopPillsChange(e.target.checked)}
              />
              <span>隐藏右上角状态胶囊（硬件 / 小智 / 审批）</span>
            </label>
          </div>

          {/* 手机连接（电脑端：允许连接 / 配对码；手机端：连接电脑） */}
          <PhoneLinkSection />

          {/* 连接电脑时这些设置属于电脑，手机只读（后端也会拒绝），直接隐藏 */}
          {!isRemoteMode() && (<>
          {/* 网络与服务端口设置 */}
          <div className="ob-fold settings-box-3">
            <FoldHeader variant="section" id="settings-port" icon={Network} title="服务监听端口 (Web Port)" defaultOpen={false} />

            <div className="settings-box-4">
              上位机 HTTP 服务监听端口，供本桌面端同源访问及手机/局域网连接：
            </div>

            <div className="settings-row">
              <input
                type="number"
                min={1024}
                max={65535}
                value={portInput}
                onChange={(e) => setPortInput(e.target.value)}
                className="text-input settings-field"
                placeholder="9666"
              />
              <button
                type="button"
                className="btn btn-hud-primary settings-btn"
                onClick={handleSavePort}
                disabled={savingPort}
              >
                <span>{savingPort ? "保存中..." : "保存端口设置"}</span>
              </button>
              {saveStatus && (
                <span className="settings-text">
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

          {/* 小智 MCP 桥接设置：桌面端已统一收归「语音中心」管理，偏好设置中不再重复展示；
              仅在无语音中心的独立 moss-host 等环境保留兼容配置 */}
          {!onOpenVoice && (
            <div className="ob-fold settings-box-3">
              <FoldHeader variant="section" id="settings-mcp" icon={Link2} title="小智 MCP 网关端点" defaultOpen={false} />
              <div className="settings-box-4">
                输入小智控制台生成的完整 WebSocket 地址；保存后重启 MOSS 生效。留空并保存可关闭桥接。
              </div>
              <div className="settings-row-3">
                <input
                  type="text"
                  value={mcpEndpointInput}
                  onChange={(e) => setMcpEndpointInput(e.target.value)}
                  className="text-input settings-field-2"
                  placeholder="wss://example.com/mcp/?token=..."
                  spellCheck="false"
                  autoComplete="off"
                />
                <button
                  type="button"
                  className="btn btn-hud-primary settings-btn-2"
                  onClick={handleSaveMcpEndpoint}
                  disabled={savingMcpEndpoint}
                >
                  {savingMcpEndpoint ? "保存中..." : "保存端点"}
                </button>
              </div>
              {mcpSaveStatus && (
                <div className="settings-box-6">
                  {mcpSaveStatus}
                </div>
              )}
              {systemInfo?.mcp && (
                <div className="settings-box-7">
                  当前状态：{systemInfo.mcp.configured ? (systemInfo.mcp.connected ? "已连接" : "已配置，未连接") : "未配置"}
                  {systemInfo.mcp.endpoint ? `；${systemInfo.mcp.endpoint}` : ""}
                </div>
              )}
            </div>
          )}

          {/* Jev 判断服务：桌面端已统一收归「语音中心 → 模型」管理，右上角偏好设置不再重复展示；
              仅在无语音中心的独立 moss-host 网页环境保留兼容配置 */}
          {!onOpenVoice && (
          <div className="ob-fold settings-box-3">
            <FoldHeader variant="section" id="settings-jev" icon={Link2} title="OpenRouter · Jev 判断服务" defaultOpen={false} />
            <div className="settings-box-4">
              Jev 只帮助判断“太焦、太浅、切不透”等加工反馈，不直接控制激光机。
            </div>
            <label className="settings-label">
              <input type="checkbox" checked={jevEnabled} onChange={(e) => setJevEnabled(e.target.checked)} />
              <span>启用 Jev 判断</span>
            </label>
            <div className="settings-box-8">
              typesafe/jev-1.13
            </div>
            <div className="settings-row-4">
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
                className="text-input settings-field-2"
                placeholder={jevMaskedKey || "输入 OpenRouter API Key"}
                spellCheck="false"
                autoComplete="new-password"
              />
              <button
                type="button"
                className="btn btn-ghost settings-btn-3"
                onClick={handleRevealJevKey}
                title={jevKeyVisible ? "隐藏 Jev API Key" : "查看 Jev API Key"}
                aria-label={jevKeyVisible ? "隐藏 Jev API Key" : "查看 Jev API Key"}
              >
                {jevKeyVisible ? <EyeOff size={15} /> : <Eye size={15} />}
              </button>
              <button
                type="button"
                className="btn btn-ghost settings-btn-4"
                onClick={handleDeleteJevKey}
                disabled={savingJev || testingJev}
                title="删除 Jev API Key"
                aria-label="删除 Jev API Key"
              >
                <Trash2 size={15} />
              </button>
            </div>
            <div className="settings-row-5">
              <button
                type="button"
                className="btn btn-hud-primary settings-btn"
                onClick={handleSaveJev}
                disabled={savingJev}
              >
                {savingJev ? "保存中..." : "保存 Jev 设置"}
              </button>
              <button
                type="button"
                className="btn btn-ghost settings-btn-5"
                onClick={handleTestJev}
                disabled={testingJev || savingJev}
              >
                {testingJev ? "测试中..." : "测试连接"}
              </button>
              {jevSaveStatus && <span className="settings-text">{jevSaveStatus}</span>}
            </div>
          </div>
          )}
          </>)}

          {/* 导出目录（仅桌面版） */}
          <ExportSettingsSection />

          {/* 软件更新（Magpie 风格：当前版本 + 启动时自动检查更新 + 包含预览版 + 立即检查更新） */}
          {!isRemoteMode() && (
            <div className="ob-fold settings-box-3" id="settings-update-section">
              <FoldHeader variant="section" id="settings-update" icon={RefreshCw} title="软件更新" defaultOpen={false} />
              <div className="settings-box-4">
                当前版本：<strong className="mono">MOSS v{systemInfo?.version || "0.2.0"}</strong>
              </div>
              <label className="settings-label">
                <input
                  type="checkbox"
                  id="settings-auto-check-update"
                  checked={autoCheckUpdate}
                  onChange={(e) => void handleToggleUpdatePref("auto_check_update", e.target.checked)}
                />
                <span>启动时自动检查更新</span>
              </label>
              <label className="settings-label" style={{ marginTop: "6px" }}>
                <input
                  type="checkbox"
                  id="settings-check-prerelease"
                  checked={checkPrerelease}
                  onChange={(e) => void handleToggleUpdatePref("check_prerelease", e.target.checked)}
                />
                <span>检查更新时包含预览版 (Pre-release)</span>
              </label>
              <div className="settings-row" style={{ marginTop: "10px", flexWrap: "wrap", gap: "10px" }}>
                <button
                  type="button"
                  className="btn btn-hud-primary settings-btn"
                  onClick={() => void handleCheckUpdateNow()}
                  disabled={checkingUpdate}
                  id="settings-check-update-btn"
                >
                  <RefreshCw size={13} />
                  <span>{checkingUpdate ? "正在检查更新…" : "立即检查更新"}</span>
                </button>
                {updateReport && !updateError && (
                  <span
                    className="settings-text"
                    style={{ color: updateReport.has_update ? "var(--gold)" : "var(--green)" }}
                  >
                    {updateReport.has_update
                      ? `发现新版本 v${updateReport.latest_version}${updateReport.prerelease ? " (预览版)" : ""}`
                      : `✓ 已是最新版本 (v${updateReport.current_version || systemInfo?.version || "0.2.0"})`}
                  </span>
                )}
                {updateError && (
                  <span className="settings-text" style={{ color: "var(--danger)" }}>
                    {updateError}
                  </span>
                )}
              </div>
              {updateReport?.has_update && (
                <div
                  style={{
                    marginTop: "10px",
                    padding: "8px 10px",
                    borderRadius: "6px",
                    background: "rgba(251, 191, 36, 0.08)",
                    border: "1px solid rgba(251, 191, 36, 0.35)",
                    fontSize: "12px",
                  }}
                >
                  <div>
                    最新版本：<strong className="mono">v{updateReport.latest_version}</strong>
                    {updateReport.published_at ? ` · 发布于 ${updateReport.published_at.slice(0, 10)}` : ""}
                  </div>
                  {updateReport.notes && (
                    <div style={{ marginTop: "4px", color: "var(--ink-dim)", whiteSpace: "pre-wrap" }}>
                      {updateReport.notes}
                    </div>
                  )}
                  {installInfo?.supported && (() => {
                    const busy = appUpdateBusy(activeUpdateProgress);
                    const view = appUpdateProgressView(activeUpdateProgress);
                    return (
                      <div className="app-update-install" id="settings-app-update">
                        <button
                          type="button"
                          className="btn btn-hud-primary settings-btn"
                          id="settings-install-update-btn"
                          disabled={busy}
                          onClick={() => void handleInstallUpdate()}
                        >
                          <RefreshCw size={13} className={busy ? "spin" : undefined} />
                          <span>{appUpdateActionLabel(installInfo, activeUpdateProgress)}</span>
                        </button>
                        {view.text && (
                          <div className="app-update-progress" role="status" data-phase={activeUpdateProgress?.phase}>
                            {busy && (
                              <div className="app-update-bar" data-indeterminate={view.percent === null ? "true" : "false"}>
                                <span style={{ width: view.percent === null ? undefined : `${view.percent}%` }} />
                              </div>
                            )}
                            <span>{view.text}</span>
                          </div>
                        )}
                      </div>
                    );
                  })()}
                  {installInfo && !installInfo.supported && installInfo.unsupported_reason && (
                    <div style={{ marginTop: "6px", color: "var(--ink-dim)" }}>{installInfo.unsupported_reason}</div>
                  )}
                  {updateReport.release_url && (
                    <div style={{ marginTop: "6px" }}>
                      <a href={updateReport.release_url} target="_blank" rel="noreferrer">
                        {installInfo?.supported ? "查看更新说明 / 手动下载 →" : "前往 GitHub Releases 下载更新 →"}
                      </a>
                    </div>
                  )}
                </div>
              )}
            </div>
          )}

          {/* 桌面客户端关闭行为 */}
          <div className="ob-fold settings-spacer">
            <FoldHeader variant="section" id="settings-close" icon={Monitor} title="窗口关闭行为" defaultOpen={false} />

            <div className="settings-box-4">
              点击主窗口右上角关闭按钮（[X]）时的默认行为：
            </div>

            <div className="settings-row-6">
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
                  className="settings-field-3"
                />
                <div>
                  <div className="settings-box-9">
                    每次询问（推荐）
                  </div>
                  <div className="settings-box-10">
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
                  className="settings-field-3"
                />
                <div>
                  <div className="settings-box-9">
                    {trayActionCopy().title}
                  </div>
                  <div className="settings-box-10">
                    {trayActionCopy().settingsDetail}
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
                  className="settings-field-3"
                />
                <div>
                  <div className="settings-box-9">
                    完全退出程序
                  </div>
                  <div className="settings-box-10">
                    直接安全停止激光并杀死主进程（适合习惯直接关闭窗口的用户）
                  </div>
                </div>
              </label>
            </div>
          </div>

          <div className="settings-row-7">
            <ShieldAlert size={16} className="settings-icon-2" />
            <div className="settings-box-11">
              <strong>安全机制提示</strong>：完全退出程序时，会停止正在下发的任务，并向正在加工的设备发送 0x18 软复位。0x18 只是软件停止请求，发送成功也不代表激光已经熄灭或断电；请以设备实际状态为准，紧急情况使用硬件急停。
            </div>
          </div>
        </div>

        <div className="mat-modal-foot">
          <button className="btn btn-hud-primary settings-btn-6" onClick={onClose}>
            完成设置
          </button>
        </div>
      </div>
    </div>
  );
}
