import React, { useState } from "react";
import { AlertCircle, X } from "lucide-react";

export function CloseConfirmModal({ visible, onClose, onConfirm, voiceServerRunning = false }) {
  const [selectedAction, setSelectedAction] = useState("tray");
  const [remember, setRemember] = useState(false);
  // 技术书 §9.4.3：完全退出时询问是否同时停止 MOSS 启动的语音服务端，默认“是”
  const [stopServer, setStopServer] = useState(true);

  if (!visible) return null;

  const handleConfirm = () => {
    onConfirm(selectedAction, remember, stopServer);
  };

  return (
    <div className="mat-modal-backdrop close-confirm-box">
      <div className="mat-modal close-confirm-box-2" role="dialog">
        <div className="mat-modal-head">
          <h3 className="close-confirm-heading">
            <AlertCircle size={18} className="close-confirm-icon" />
            <span>关闭主窗口</span>
          </h3>
          <button className="mat-modal-close" onClick={onClose} title="取消">
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          <p className="close-confirm-note">
            您点击了关闭窗口，请选择您期望的操作：
          </p>

          <div className="close-confirm-row">
            <label
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: "10px",
                padding: "10px 12px",
                borderRadius: "8px",
                background: selectedAction === "tray" ? "rgba(139, 123, 255, 0.08)" : "rgba(255, 255, 255, 0.02)",
                border: `1px solid ${selectedAction === "tray" ? "rgba(139, 123, 255, 0.3)" : "var(--line, #242b35)"}`,
                cursor: "pointer",
                transition: "all 0.15s ease"
              }}
            >
              <input
                type="radio"
                name="closeAction"
                value="tray"
                checked={selectedAction === "tray"}
                onChange={() => setSelectedAction("tray")}
                className="close-confirm-field"
              />
              <div>
                <div className="close-confirm-box-3">
                  最小化到系统托盘
                </div>
                <div className="close-confirm-box-4">
                  窗口隐藏到任务栏托盘区，后台继续保持激光作业与网络服务
                </div>
              </div>
            </label>

            <label
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: "10px",
                padding: "10px 12px",
                borderRadius: "8px",
                background: selectedAction === "exit" ? "rgba(239, 68, 68, 0.08)" : "rgba(255, 255, 255, 0.02)",
                border: `1px solid ${selectedAction === "exit" ? "rgba(239, 68, 68, 0.3)" : "var(--line, #242b35)"}`,
                cursor: "pointer",
                transition: "all 0.15s ease"
              }}
            >
              <input
                type="radio"
                name="closeAction"
                value="exit"
                checked={selectedAction === "exit"}
                onChange={() => setSelectedAction("exit")}
                className="close-confirm-field"
              />
              <div>
                <div className="close-confirm-box-3">
                  完全退出程序
                </div>
                <div className="close-confirm-box-4">
                  停止激光加工任务，触发安全急停软复位并彻底杀死进程
                </div>
              </div>
            </label>
          </div>

          {voiceServerRunning && selectedAction === "exit" && (
            <label className="close-voice-option" id="close-stop-voice-server">
              <input type="checkbox" checked={stopServer} onChange={(e) => setStopServer(e.target.checked)} className="close-confirm-field-2" />
              <span>同时停止语音服务端（由 MOSS 启动的 xiaozhi-server）。取消勾选则退出后服务端继续运行，其输出不再记录。</span>
            </label>
          )}

          <div className="close-confirm-box-5">
            <label className="close-confirm-label">
              <input
                type="checkbox"
                checked={remember}
                onChange={(e) => setRemember(e.target.checked)}
              />
              <span>记住我的选择，以后不再询问（可在右上角设置中更改）</span>
            </label>
          </div>
        </div>

        <div className="mat-modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            取消
          </button>
          <button
            className={`btn ${selectedAction === "exit" ? "btn-danger" : "btn-primary"}`}
            onClick={handleConfirm}
          >
            确定
          </button>
        </div>
      </div>
    </div>
  );
}
