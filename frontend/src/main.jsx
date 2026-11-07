import React, { useState, useEffect, useCallback, useRef } from "react";
import { createRoot } from "react-dom/client";
import { Navbar } from "./components/Navbar.jsx";
import { SideNav } from "./components/SideNav.jsx";
import { requestEmergencyStop } from "./estopBus.js";
import { ESTOP_LOOKUP_TIMEOUT_MS, ESTOP_SEND_TIMEOUT_MS, triggerTopbarEmergencyStop } from "./globalEstop.js";
import { createPollRunner } from "./pollGuard.js";
import { installCardSpotlight } from "./spotlight.js";
import { AuthModal } from "./components/AuthModal.jsx";
import { CloseConfirmModal } from "./components/CloseConfirmModal.jsx";
import { SettingsModal } from "./components/SettingsModal.jsx";
import { CyberBackground } from "./components/CyberBackground.jsx";
import { AppDialogHost } from "./components/AppDialog.jsx";
import { showAlert, showConfirm } from "./appDialog.js";
import { getJson, postJson } from "./api/client.js";
import { backendStartupErrorText, hasDesktopSession, initializeBackendOrigin, isRemoteMode, refreshDesktopSession } from "./authSession.js";
import { ApprovalCenter } from "./components/ApprovalCenter.jsx";
import { AiPreviewCenter } from "./components/AiPreviewCenter.jsx";
import { JobProgressDock } from "./components/JobProgressDock.jsx";
import { clearRemoteOrigin } from "./remoteHost.js";
import { HomePage } from "./pages/HomePage.jsx";
import { MaterialLabPage } from "./pages/MaterialLabPage.jsx";
import { DrawPage } from "./pages/DrawPage.jsx";
import { TwinPage } from "./pages/TwinPage.jsx";
import { VoiceErrorBoundary } from "./components/VoiceErrorBoundary.jsx";
import { VoicePage } from "./pages/VoicePage.jsx";
import { CameraPage } from "./pages/CameraPage.jsx";
import { isVoiceCenterAvailable, voiceApi } from "./voiceApi.js";
import { readHideTopPills, writeFoldOpen, writeHideTopPills } from "./foldState.js";
import { appUpdateAvailable } from "./appUpdate.js";
import "./styles.css";

function pathToRoute(pathname, voiceEnabled) {
  if (pathname.startsWith("/material-lab")) return "/material-lab";
  if (pathname.startsWith("/draw")) return "/draw";
  if (pathname.startsWith("/twin")) return "/twin";
  // 摄像头与语音中心只在 MOSS 桌面端可用（技术书 §9.4.7）；其他环境回到工作台
  if (pathname.startsWith("/camera") && voiceEnabled) return "/camera";
  if (pathname.startsWith("/voice") && voiceEnabled) return "/voice";
  return "/";
}

function App() {
  // 桌面会话在首次渲染前已取得（见文件末尾），这里只需判断一次
  const voiceEnabled = isVoiceCenterAvailable();
  const [currentPath, setCurrentPath] = useState(() => pathToRoute(window.location.pathname, voiceEnabled));

  const [connectionStatus, setConnectionStatus] = useState({
    connected: false,
    summary: "未连接",
  });
  const [mcpStatus, setMcpStatus] = useState(null);
  const [voiceStatus, setVoiceStatus] = useState(null);
  const [activeJobs, setActiveJobs] = useState([]);
  const [showAuthModal, setShowAuthModal] = useState(false);
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  const [showCloseModal, setShowCloseModal] = useState(false);
  const [startupUpdate, setStartupUpdate] = useState(null);
  const [dismissedUpdateVersion, setDismissedUpdateVersion] = useState("");
  const [focusUpdateSection, setFocusUpdateSection] = useState(false);
  const [hideTopPills, setHideTopPills] = useState(() => readHideTopPills());
  const handleHideTopPillsChange = useCallback((next) => {
    const hidden = Boolean(next);
    setHideTopPills(hidden);
    writeHideTopPills(hidden);
  }, []);
  // 退出时是否有 MOSS 启动的语音服务端在运行（决定关闭弹窗是否显示“同时停止语音服务端”）
  const [voiceServerOwned, setVoiceServerOwned] = useState(false);
  // 全局审批卡（技术书 §9.5.2）：只在桌面端（有桌面会话）显示
  const [approvalOpen, setApprovalOpen] = useState(false);
  const [approvalCount, setApprovalCount] = useState(0);
  const approvalsEnabled = hasDesktopSession();
  const openApprovals = useCallback(() => {
    if (approvalCount > 0) {
      setApprovalOpen(true);
    } else {
      void showAlert("当前没有待批准的请求。小智或局域网设备请求开始加工时，审批卡会自动弹出。", { title: "审批", tone: "info" });
    }
  }, [approvalCount]);

  const executeDesktopAction = useCallback(async (action, { stopServer = true } = {}) => {
    const invoke = window.__TAURI__?.core?.invoke;
    const command = action === "tray"
      ? "minimize_to_tray"
      : action === "exit"
        ? "exit_application"
        : null;

    if (!invoke || !command) {
      if (action === "exit") {
        if (await showConfirm("确定要退出 MOSS 激光控制系统吗？", { title: "退出程序", tone: "warning", danger: true, confirmText: "退出" })) {
          window.close();
          document.body.innerHTML = "<div style='display:grid;place-items:center;height:100vh;background:#09090b;color:#ff3b5c;font-family:sans-serif;'><h2>MOSS 会话已安全终止，请关闭本页面</h2></div>";
        }
        return true;
      }
      return false;
    }

    try {
      // 完全退出时告诉桌面端是否一并停止 MOSS 启动的语音服务端（默认是，技术书 §9.4.3）
      await invoke(command, command === "exit_application" ? { stopServer } : undefined);
      return true;
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      console.error(`执行桌面关闭命令 ${command} 失败`, error);
      await showAlert(detail, { title: "关闭操作失败", tone: "error" });
      return false;
    }
  }, []);

  const triggerCloseHandling = useCallback(async () => {
    let pref = "ask";
    try {
      pref = globalThis.localStorage?.getItem("moss_close_action") || "ask";
    } catch (_error) {
      // 存储被禁用时按“每次询问”处理
    }
    let owned = false;
    if (voiceEnabled) {
      try {
        const status = await voiceApi.status();
        owned = status?.server?.state === "running" && Boolean(status?.server?.owned);
      } catch {
        owned = false;
      }
    }
    setVoiceServerOwned(owned);
    if (pref === "tray") {
      void executeDesktopAction("tray");
    } else if (pref === "exit" && !owned) {
      void executeDesktopAction("exit");
    } else {
      // 记住了“退出”但语音服务端在运行时仍弹窗，让用户决定是否一并停止
      setShowCloseModal(true);
    }
  }, [executeDesktopAction, voiceEnabled]);

  const handleCloseConfirm = useCallback(async (action, remember, stopServer = true) => {
    if (remember) {
      try {
        globalThis.localStorage?.setItem("moss_close_action", action);
      } catch (_error) {
        // 存储被禁用：本次照常执行，只是不记住选择
      }
    }
    const succeeded = await executeDesktopAction(action, { stopServer });
    if (succeeded) {
      setShowCloseModal(false);
    }
  }, [executeDesktopAction]);

  useEffect(() => {
    const handlePopState = () => {
      setCurrentPath(pathToRoute(window.location.pathname, voiceEnabled));
    };
    const handleAuthRequired = () => {
      setShowAuthModal(true);
    };

    window.addEventListener("popstate", handlePopState);
    window.addEventListener("laser-auth-required", handleAuthRequired);
    window.addEventListener("moss:close-requested", triggerCloseHandling);

    let unlistenTauri = null;
    if (window.__TAURI__?.event?.listen) {
      window.__TAURI__.event.listen("moss:close-requested", triggerCloseHandling)
        .then((unlisten) => {
          unlistenTauri = unlisten;
        })
        .catch((error) => {
          const detail = error instanceof Error ? error.message : String(error);
          console.error("注册桌面关闭事件失败", error);
        });
    }

    return () => {
      window.removeEventListener("popstate", handlePopState);
      window.removeEventListener("laser-auth-required", handleAuthRequired);
      window.removeEventListener("moss:close-requested", triggerCloseHandling);
      if (unlistenTauri) unlistenTauri();
    };
  }, [triggerCloseHandling, voiceEnabled]);

  // 定期同步小智 MCP 与语音遥测状态（活跃拾音/思考/播报时自适应加密至 240ms，跨页面实时驱动左上角 MOSS 机械眼）
  const activeVoicePhase = voiceStatus?.phase === "listening" || voiceStatus?.phase === "thinking" || voiceStatus?.phase === "speaking";
  const autoUpdateCheckedRef = useRef(false);
  // 2026-11-05：上一次请求未返回时跳过本节拍（240ms 高频时不叠加请求），切换间隔或卸载后丢弃旧响应
  useEffect(() => {
    let cancelled = false;
    const syncMcp = createPollRunner(async (isCurrent) => {
      const res = await getJson("/api/system/info");
      if (!cancelled && isCurrent() && res.ok && res.data?.success && res.data?.result?.mcp) {
        const result = res.data.result;
        setMcpStatus(result.mcp);
        setVoiceStatus(result.voice ?? null);
        if (!autoUpdateCheckedRef.current) {
          autoUpdateCheckedRef.current = true;
          if (result.auto_check_update && !isRemoteMode()) {
            postJson("/api/system/check-update", {
              include_prerelease: Boolean(result.check_prerelease),
            }).then((up) => {
              if (!cancelled && up.ok && up.data?.success && up.data?.result?.has_update) {
                setStartupUpdate(up.data.result);
              }
            }).catch(() => {});
          }
        }
      }
    });
    void syncMcp();
    const intervalMs = activeVoicePhase ? 240 : 3000;
    const timer = setInterval(() => void syncMcp(), intervalMs);
    return () => {
      cancelled = true;
      syncMcp.invalidate();
      clearInterval(timer);
    };
  }, [activeVoicePhase]);

  useEffect(() => installCardSpotlight(), []);

  // 顶栏急停先读后端全局任务（含执行结果未知的作业），跨页面也可停止原目标；
  // 若后端仅有 unknown 待确认旧记录、停止失败、多任务无法确定或停的是其他设备，同时向当前页面的设备发送 0x18。
  // 2026-11-05：查询与写出都带短时限；后端任务通道 800ms 内没有结果就先让页面按本页目标补发，不再无限等待。
  const estopBusy = useRef(false);
  const handleGlobalEmergencyStop = useCallback(async () => {
    if (estopBusy.current) return;
    estopBusy.current = true;
    let outcome;
    try {
      outcome = await triggerTopbarEmergencyStop(
        (path) => getJson(path, { timeoutMs: ESTOP_LOOKUP_TIMEOUT_MS }),
        (path, body) => postJson(path, body, { timeoutMs: ESTOP_SEND_TIMEOUT_MS }),
        (opts) => requestEmergencyStop(opts),
      );
    } finally {
      // 请求结束即释放，提示框打开期间再次按急停也能立即重发
      estopBusy.current = false;
    }
    if (outcome.kind === "ambiguous") {
      // 本页即便已按自己的设备目标补发，其他任务仍未停止，必须提示
      await showAlert(
        outcome.pageHandled
          ? `已由当前页面按本页设备目标补发急停 0x18（结果见页面提示）；另检测到 ${outcome.jobs.length} 个可能仍在运行的任务，无法安全地猜测其设备，请在对应任务中按急停；紧急情况使用实体急停。`
          : `检测到 ${outcome.jobs.length} 个可能仍在运行的任务。无法安全地猜测要停止哪台设备，请在对应任务中按急停；紧急情况使用实体急停。`,
        { title: "多个停止目标", tone: "warning" },
      );
      return;
    }
    if (outcome.pageHandled) {
      return;
    }
    if (outcome.kind === "sent") {
      await showAlert(`${outcome.message || "急停 0x18 已写出"}。停止尚未确认，请现场检查机床状态；必要时按实体急停。`, { title: "已发送急停", tone: "warning" });
    } else if (outcome.kind === "failed") {
      await showAlert(`${outcome.message || "急停发送失败"}。请立即使用实体急停，并检查设备连接。`, { title: "急停未确认", tone: "error" });
    } else {
      await showAlert(
        outcome.kind === "lookup_failed"
          ? "无法查询后端当前任务，不能确认软件急停能否发送。请立即使用实体急停。"
          : "没有找到正在下发的任务。如设备仍在运动或出光，请立即使用实体急停。",
        { title: "停止状态未知", tone: "warning" },
      );
    }
  }, []);

  const navigateTo = (path) => {
    if (window.location.pathname !== path) {
      window.history.pushState({}, "", path);
      setCurrentPath(path);
    }
  };

  return (
    <div className="laser-app-root ob-shell boot-box">
      <CyberBackground />
      <SideNav
        currentPath={currentPath}
        onNavigate={navigateTo}
        onAuthClick={() => setShowAuthModal(true)}
        onSettingsClick={() => setShowSettingsModal(true)}
        showVoice={voiceEnabled}
        mcpStatus={mcpStatus}
        voiceStatus={voiceStatus}
        approvalCount={approvalCount}
        connectionStatus={connectionStatus}
        activeJobs={activeJobs}
      />
      <div className="ob-main">
        <Navbar
          currentPath={currentPath}
          onNavigate={navigateTo}
          connectionStatus={connectionStatus}
          mcpStatus={mcpStatus}
          voiceStatus={voiceStatus}
          onEmergencyStop={handleGlobalEmergencyStop}
          showApprovals={approvalsEnabled}
          approvalCount={approvalCount}
          onApprovalsClick={openApprovals}
          hideStatusPills={hideTopPills}
        />

        {startupUpdate?.has_update && !isRemoteMode() && dismissedUpdateVersion !== startupUpdate.latest_version && (
          <div className="app-update-banner" id="app-update-banner" role="status">
            <span className="app-update-banner-text">
              发现新版本 <strong className="mono">MOSS v{startupUpdate.latest_version}</strong>
              {startupUpdate.current_version ? `（当前 v${startupUpdate.current_version}）` : ""}
              {startupUpdate.prerelease ? " · 预览版" : ""}
            </span>
            <div className="app-update-banner-actions">
              <button
                type="button"
                className="btn btn-hud-primary app-update-banner-btn"
                id="app-update-banner-open"
                onClick={() => {
                  writeFoldOpen("settings-update", true);
                  setFocusUpdateSection(true);
                  setShowSettingsModal(true);
                }}
              >
                {appUpdateAvailable() ? "立即更新" : "查看更新"}
              </button>
              <button
                type="button"
                className="btn btn-ghost app-update-banner-btn"
                id="app-update-banner-dismiss"
                onClick={() => setDismissedUpdateVersion(startupUpdate.latest_version)}
              >
                稍后
              </button>
            </div>
          </div>
        )}

        <main className="main-content-viewport" key={currentPath}>
          {currentPath === "/material-lab" && (
            <MaterialLabPage onNavigate={navigateTo} />
          )}
          {currentPath === "/draw" && (
            <DrawPage onNavigate={navigateTo} />
          )}
          {currentPath === "/twin" && (
            <TwinPage
              onNavigate={navigateTo}
              connectionStatus={connectionStatus}
              setConnectionStatus={setConnectionStatus}
            />
          )}
          {currentPath === "/camera" && voiceEnabled && (
            <VoiceErrorBoundary onBack={navigateTo}>
              <CameraPage onNavigate={navigateTo} />
            </VoiceErrorBoundary>
          )}
          {currentPath === "/voice" && voiceEnabled && (
            <VoiceErrorBoundary onBack={navigateTo}>
              <VoicePage mcpStatus={mcpStatus} approvalCount={approvalCount} />
            </VoiceErrorBoundary>
          )}
          {currentPath === "/" && (
            <HomePage
              onNavigate={navigateTo}
              connectionStatus={connectionStatus}
              setConnectionStatus={setConnectionStatus}
            />
          )}
        </main>
      </div>

      <AuthModal
        visible={showAuthModal}
        onClose={() => setShowAuthModal(false)}
        onAuthenticated={() => setShowAuthModal(false)}
      />

      <SettingsModal
        visible={showSettingsModal}
        onClose={() => {
          setShowSettingsModal(false);
          setFocusUpdateSection(false);
        }}
        hideTopPills={hideTopPills}
        onHideTopPillsChange={handleHideTopPillsChange}
        initialUpdateReport={startupUpdate}
        focusUpdateSection={focusUpdateSection}
        onUpdateReport={setStartupUpdate}
        onOpenVoice={
          voiceEnabled
            ? () => {
                setShowSettingsModal(false);
                navigateTo("/voice");
              }
            : null
        }
      />

      <CloseConfirmModal
        visible={showCloseModal}
        onClose={() => setShowCloseModal(false)}
        onConfirm={handleCloseConfirm}
        voiceServerRunning={voiceServerOwned}
      />

      <ApprovalCenter
        open={approvalOpen}
        onOpenChange={setApprovalOpen}
        onPendingCountChange={setApprovalCount}
      />

      <AiPreviewCenter />

      <JobProgressDock onJobsChange={setActiveJobs} />

      <AppDialogHost />
    </div>
  );
}

const root = createRoot(document.getElementById("root"));
initializeBackendOrigin()
  // 桌面会话取不到（浏览器、Android、后端未开启会话）不影响启动
  .then(() => refreshDesktopSession())
  .then(() => root.render(<App />))
  .catch((error) => {
    console.error("后端启动失败", error);
    const remote = isRemoteMode();
    root.render(
      <main role="alert" className="boot-grid">
        <h1 className="boot-heading">{remote ? "无法连接电脑 MOSS" : "MOSS 后端无法启动"}</h1>
        <p>{remote ? "手机当前设置为连接电脑上的 MOSS。" : "无法连接本机服务，请检查是否已有 MOSS 实例运行。"}</p>
        {backendStartupErrorText(error) && (
          <pre className="boot-code">
            {backendStartupErrorText(error)}
          </pre>
        )}
        <button type="button" onClick={() => window.location.reload()}>重试</button>
        {remote && (
          <button
            type="button"
            className="boot-btn"
            onClick={() => {
              clearRemoteOrigin();
              window.location.reload();
            }}
          >
            改用手机本机模式
          </button>
        )}
      </main>,
    );
  });
