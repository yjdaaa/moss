import React, { useState, useEffect, useCallback } from "react";
import { createRoot } from "react-dom/client";
import { Navbar } from "./components/Navbar.jsx";
import { AuthModal } from "./components/AuthModal.jsx";
import { CloseConfirmModal } from "./components/CloseConfirmModal.jsx";
import { SettingsModal } from "./components/SettingsModal.jsx";
import { CyberBackground } from "./components/CyberBackground.jsx";
import { AppDialogHost } from "./components/AppDialog.jsx";
import { showAlert, showConfirm } from "./appDialog.js";
import { getJson } from "./api/client.js";
import { HomePage } from "./pages/HomePage.jsx";
import { MaterialLabPage } from "./pages/MaterialLabPage.jsx";
import { DrawPage } from "./pages/DrawPage.jsx";
import "./styles.css";

function App() {
  const [currentPath, setCurrentPath] = useState(() => {
    const p = window.location.pathname;
    if (p.startsWith("/material-lab")) return "/material-lab";
    if (p.startsWith("/draw")) return "/draw";
    return "/";
  });

  const [connectionStatus, setConnectionStatus] = useState({
    connected: false,
    summary: "未连接",
  });
  const [mcpStatus, setMcpStatus] = useState(null);
  const [showAuthModal, setShowAuthModal] = useState(false);
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  const [showCloseModal, setShowCloseModal] = useState(false);

  const executeDesktopAction = useCallback(async (action) => {
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
          document.body.innerHTML = "<div style='display:grid;place-items:center;height:100vh;background:#05070d;color:#ff2a44;font-family:sans-serif;'><h2>MOSS 会话已安全终止，请关闭本页面</h2></div>";
        }
        return true;
      }
      return false;
    }

    try {
      await invoke(command);
      return true;
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      console.error(`执行桌面关闭命令 ${command} 失败`, error);
      await showAlert(detail, { title: "关闭操作失败", tone: "error" });
      return false;
    }
  }, []);

  const triggerCloseHandling = useCallback(() => {
    const pref = localStorage.getItem("moss_close_action") || "ask";
    if (pref === "tray") {
      void executeDesktopAction("tray");
    } else if (pref === "exit") {
      void executeDesktopAction("exit");
    } else {
      setShowCloseModal(true);
    }
  }, [executeDesktopAction]);

  const handleCloseConfirm = useCallback(async (action, remember) => {
    if (remember) {
      localStorage.setItem("moss_close_action", action);
    }
    const succeeded = await executeDesktopAction(action);
    if (succeeded) {
      setShowCloseModal(false);
    }
  }, [executeDesktopAction]);

  useEffect(() => {
    const handlePopState = () => {
      const p = window.location.pathname;
      if (p.startsWith("/material-lab")) setCurrentPath("/material-lab");
      else if (p.startsWith("/draw")) setCurrentPath("/draw");
      else setCurrentPath("/");
    };

    window.addEventListener("popstate", handlePopState);
    window.addEventListener("laser-auth-required", () => setShowAuthModal(true));
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
      window.removeEventListener("laser-auth-required", () => setShowAuthModal(true));
      window.removeEventListener("moss:close-requested", triggerCloseHandling);
      if (unlistenTauri) unlistenTauri();
    };
  }, [triggerCloseHandling]);

  // 定期同步小智 MCP 真实运行状态
  useEffect(() => {
    let cancelled = false;
    async function syncMcp() {
      const res = await getJson("/api/system/info");
      if (!cancelled && res.ok && res.data?.success && res.data?.result?.mcp) {
        setMcpStatus(res.data.result.mcp);
      }
    }
    syncMcp();
    const timer = setInterval(syncMcp, 4000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  const navigateTo = (path) => {
    if (window.location.pathname !== path) {
      window.history.pushState({}, "", path);
      setCurrentPath(path);
    }
  };

  return (
    <div className="laser-app-root" style={{ minHeight: "100vh", position: "relative", color: "var(--ink, #e8ecf6)" }}>
      <CyberBackground />
      <Navbar
        currentPath={currentPath}
        onNavigate={navigateTo}
        connectionStatus={connectionStatus}
        mcpStatus={mcpStatus}
        onAuthClick={() => setShowAuthModal(true)}
        onSettingsClick={() => setShowSettingsModal(true)}
      />

      <main className="main-content-viewport">
        {currentPath === "/material-lab" && (
          <MaterialLabPage onNavigate={navigateTo} />
        )}
        {currentPath === "/draw" && (
          <DrawPage onNavigate={navigateTo} />
        )}
        {currentPath === "/" && (
          <HomePage
            onNavigate={navigateTo}
            connectionStatus={connectionStatus}
            setConnectionStatus={setConnectionStatus}
          />
        )}
      </main>

      <AuthModal
        visible={showAuthModal}
        onClose={() => setShowAuthModal(false)}
        onAuthenticated={() => setShowAuthModal(false)}
      />

      <SettingsModal
        visible={showSettingsModal}
        onClose={() => setShowSettingsModal(false)}
      />

      <CloseConfirmModal
        visible={showCloseModal}
        onClose={() => setShowCloseModal(false)}
        onConfirm={handleCloseConfirm}
      />

      <AppDialogHost />
    </div>
  );
}

createRoot(document.getElementById("root")).render(<App />);
