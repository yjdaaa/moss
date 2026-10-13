import React, { useState } from "react";
import { KeyRound, X, Check } from "lucide-react";
import { storeAuthToken, clearAuthToken, readAuthToken, isRemoteMode } from "../authSession.js";
import { postJson } from "../api/client.js";

export function AuthModal({ visible, onClose, onAuthenticated }) {
  if (!visible) return null;

  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const currentToken = readAuthToken();

  const handlePair = async () => {
    const trimmed = code.trim();
    if (trimmed.length !== 6 || !/^\d{6}$/.test(trimmed)) {
      setError("请输入正确的 6 位数字配对码");
      return;
    }

    setBusy(true);
    setError("");

    try {
      const res = await postJson("/v1/auth/pair", { pairing_code: trimmed });
      setBusy(false);

      if (res.ok && res.data?.success) {
        const token = res.data.result?.token;
        if (token) {
          storeAuthToken(token);
          if (onAuthenticated) onAuthenticated(token);
          onClose();
          // 手机连接电脑：配对前各页面的数据请求都被拒绝了，刷新一次重新加载
          if (isRemoteMode()) window.location.reload();
        } else {
          setError("服务端未返回有效授权 Token");
        }
      } else {
        setError(res.data?.result || res.data?.detail || "配对失败：配对码无效或已过期");
      }
    } catch (e) {
      setBusy(false);
      setError(`配对网络异常: ${e.message}`);
    }
  };

  const handleClearToken = () => {
    clearAuthToken();
    setCode("");
    setError("");
    if (onAuthenticated) onAuthenticated("");
  };

  return (
    <div className="mat-modal-backdrop" style={{ zIndex: 100 }}>
      <div className="mat-modal" style={{ maxWidth: "380px" }} role="dialog">
        <div className="mat-modal-head">
          <h3 style={{ display: "flex", alignItems: "center", gap: "6px" }}>
            <KeyRound size={18} />
            <span>局域网访问配对认证</span>
          </h3>
          <button className="mat-modal-close" onClick={onClose} title="关闭">
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          <p style={{ fontSize: "13px", color: "var(--ink-dim, #94a3b8)", marginTop: 0 }}>
            手机或局域网设备访问电脑 MOSS 时需要配对：在电脑 MOSS 的“偏好设置 → 手机连接”中点“生成配对码”，把 6 位数字输入下面（5 分钟内有效）。电脑重启后需重新配对。
          </p>

          <div className="mat-field">
            <label htmlFor="auth-code-input">6 位配对码</label>
            <input
              id="auth-code-input"
              type="text"
              maxLength={6}
              placeholder="例如：123456"
              value={code}
              onChange={(e) => setCode(e.target.value.replace(/\D/g, ""))}
              style={{
                fontSize: "20px",
                letterSpacing: "4px",
                textAlign: "center",
                fontFamily: "var(--font-mono, monospace)"
              }}
              autoComplete="off"
            />
          </div>

          {error && (
            <div style={{ color: "var(--danger, #ef4444)", fontSize: "12px", marginTop: "8px" }}>
              {error}
            </div>
          )}

          {currentToken && (
            <div style={{ marginTop: "12px", padding: "8px", background: "rgba(16, 185, 129, 0.08)", border: "1px solid rgba(16, 185, 129, 0.2)", borderRadius: "4px", fontSize: "12px", color: "#10b981", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <span>当前会话已持有有效 Token</span>
              <button
                onClick={handleClearToken}
                style={{ background: "transparent", border: "none", color: "#ef4444", cursor: "pointer", fontSize: "11px", textDecoration: "underline" }}
              >
                撤销清除
              </button>
            </div>
          )}
        </div>

        <div className="mat-modal-foot">
          <button className="btn btn-ghost" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button className="btn btn-primary" onClick={handlePair} disabled={busy || code.length !== 6}>
            {busy ? "验证中..." : "提交配对"}
          </button>
        </div>
      </div>
    </div>
  );
}
