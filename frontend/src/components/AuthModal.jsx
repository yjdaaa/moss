import React, { useState } from "react";
import { KeyRound, X } from "lucide-react";
import { storeAuthToken, clearAuthToken, readAuthToken, isRemoteMode } from "../authSession.js";
import { postJson } from "../api/client.js";

// 关闭时整个对话框卸载，下次打开输入框与报错从空白开始；Hooks 放在内层组件，保证每次渲染调用顺序一致
export function AuthModal({ visible, ...props }) {
  return visible ? <AuthModalDialog {...props} /> : null;
}

function AuthModalDialog({ onClose, onAuthenticated }) {
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
    <div className="mat-modal-backdrop auth-box">
      <div className="mat-modal auth-box-2" role="dialog">
        <div className="mat-modal-head">
          <h3 className="auth-heading">
            <KeyRound size={18} />
            <span>局域网访问配对认证</span>
          </h3>
          <button className="mat-modal-close" onClick={onClose} title="关闭">
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          <p className="auth-note">
            手机或局域网设备首次访问电脑 MOSS 时需要配对：输入电脑“偏好设置 → 手机连接”中设置的 6 位固定配对码（或生成的临时配对码）。配对成功后将长久保持连接，软件或电脑重启后无需重配。
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
              className="auth-field"
              autoComplete="off"
            />
          </div>

          {error && (
            <div className="auth-box-3">
              {error}
            </div>
          )}

          {currentToken && (
            <div className="auth-row">
              <span>当前会话已持有有效 Token</span>
              <button
                onClick={handleClearToken}
                className="auth-btn"
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
