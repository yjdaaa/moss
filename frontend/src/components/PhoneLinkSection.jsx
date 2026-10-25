import React, { useEffect, useState } from "react";
import { Smartphone, KeyRound, RefreshCw, Unlink } from "lucide-react";
import { FoldHeader } from "./FoldHeader.jsx";
import { getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { clearAuthToken, isRemoteMode, remoteOriginLabel } from "../authSession.js";
import { clearRemoteOrigin, isAndroidDevice, normalizeRemoteOrigin, probeRemoteOrigin, saveRemoteOrigin } from "../remoteHost.js";
import { phoneLinkHostCopy } from "../platform.js";

const foldStyle = { marginBottom: "20px", paddingBottom: "18px", borderBottom: "1px solid var(--line)" };
const hintStyle = { fontSize: "12px", color: "var(--ink-dim)", marginBottom: "10px", lineHeight: 1.5 };

/**
 * 偏好设置 → 手机连接（技术书 §9.8）。
 * - 电脑上：开关“允许手机连接”、显示本机地址、生成配对码；
 * - 手机 App 上：填写电脑地址连接 / 断开回到本机模式。
 */
export function PhoneLinkSection() {
  const inApp = typeof window !== "undefined" && Boolean(window.__TAURI__?.core?.invoke);
  if (isRemoteMode()) return <RemoteConnectedPanel />;
  if (inApp && isAndroidDevice()) return <ConnectToComputerPanel />;
  return <ComputerHostPanel />;
}

/** 电脑端：本面板依赖仅本机可用的 `/api/system/lan-info`，拿不到（局域网浏览器访问）时不显示。 */
function ComputerHostPanel() {
  const [info, setInfo] = useState(null);
  const [busy, setBusy] = useState(false);
  const [code, setCode] = useState(null);

  const load = async () => {
    const res = await getJson("/api/system/lan-info");
    setInfo(res.ok && res.data?.success ? res.data.result : false);
  };

  useEffect(() => {
    void load();
  }, []);

  if (!info) return null;

  const toggle = async (enabled) => {
    setBusy(true);
    try {
      const res = await postJson("/api/settings", { lan_access: enabled });
      if (!(res.ok && res.data?.success)) {
        throw new Error(res.data?.result || res.data?.detail || `HTTP ${res.status}`);
      }
      await load();
      await showAlert(
        enabled
          ? phoneLinkHostCopy().enableAlert
          : "已保存。重新打开 MOSS 后，手机将无法再连接这台电脑。",
        { title: "需要重启 MOSS", tone: "info" },
      );
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "保存失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const generateCode = async () => {
    setBusy(true);
    try {
      const res = await postJson("/v1/auth/challenge", {});
      if (!(res.ok && res.data?.success)) {
        throw new Error(res.data?.result || `HTTP ${res.status}`);
      }
      setCode({ value: res.data.result.pairing_code, expiresAt: Date.now() + 300 * 1000 });
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "生成配对码失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const address = info.addresses?.[0] ? `${info.addresses[0]}:${info.port}` : null;

  return (
    <div className="ob-fold" style={foldStyle}>
      <FoldHeader variant="section" id="settings-phone-link" icon={Smartphone} title="手机连接" defaultOpen={false} />
      <div style={hintStyle}>
        开启后，同一 WiFi 下的手机 App 可以连接这台电脑，用电脑的数据操作雕刻机（连接、预览、发送、急停）。
        手机上材料、校准参数和系统设置只读，只能在电脑上修改。
      </div>
      <label className="phone-link-label">
        <input
          type="checkbox"
          checked={Boolean(info.lan_access_saved)}
          disabled={busy}
          onChange={(event) => void toggle(event.target.checked)}
        />
        <span>允许手机连接（保存后重启 MOSS 生效）</span>
      </label>
      {info.restart_required && (
        <div className="phone-link-box">设置已保存，重启 MOSS 后生效。</div>
      )}
      {info.lan_access_active && (
        <>
          <div className="phone-link-box-2">
            手机 App 中填写的电脑地址：{" "}
            <span className="mono phone-link-text">{address || `本机 IP:${info.port}`}</span>
            {!address && <span className="phone-link-text-2">{phoneLinkHostCopy().missingIpHint}</span>}
          </div>
          <div className="phone-link-row">
            <button type="button" className="btn btn-hud-primary phone-link-btn" disabled={busy} onClick={() => void generateCode()}>
              <KeyRound size={13} />
              <span>生成配对码</span>
            </button>
            {code && (
              <span className="mono phone-link-text-3">
                {code.value}
              </span>
            )}
          </div>
          {code && (
            <div className="phone-link-box-3">
              5 分钟内在手机上输入；电脑重启后手机需要重新配对。
            </div>
          )}
        </>
      )}
    </div>
  );
}

/** 手机 App（本机模式）：填写电脑地址并切换到“连接电脑”模式。 */
function ConnectToComputerPanel() {
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);

  const connect = async () => {
    const origin = normalizeRemoteOrigin(input);
    if (!origin) {
      await showAlert("请填写电脑 MOSS 显示的地址，例如 192.168.1.20:9666", { title: "地址格式不正确", tone: "warning" });
      return;
    }
    setBusy(true);
    try {
      await probeRemoteOrigin(origin);
      saveRemoteOrigin(origin);
      window.location.reload();
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "连接失败", tone: "error" });
      setBusy(false);
    }
  };

  return (
    <div className="ob-fold" style={foldStyle}>
      <FoldHeader variant="section" id="settings-phone-link" icon={Smartphone} title="连接电脑 MOSS" defaultOpen={false} />
      <div style={hintStyle}>
        连接后手机使用电脑上的数据（材料库、任务记录），通过电脑操作雕刻机；手机上材料与设置只读。
        需要电脑开着 MOSS 并在“偏好设置 → 手机连接”中允许手机连接，手机与电脑在同一个 WiFi。
      </div>
      <div className="phone-link-row">
        <input
          type="text"
          inputMode="url"
          className="text-input phone-link-field"
          value={input}
          placeholder="例如 192.168.1.20:9666"
          onChange={(event) => setInput(event.target.value)}
        />
        <button type="button" className="btn btn-hud-primary phone-link-btn" disabled={busy} onClick={() => void connect()}>
          <RefreshCw size={13} />
          <span>{busy ? "连接中…" : "连接"}</span>
        </button>
      </div>
    </div>
  );
}

/** 已连接电脑：显示地址，可断开回到手机本机模式。 */
function RemoteConnectedPanel() {
  const disconnect = async () => {
    const ok = await showConfirm("断开后手机改用自己内置的 MOSS（数据与电脑不互通）。确定断开吗？", {
      title: "断开电脑连接",
      tone: "warning",
      confirmText: "断开",
    });
    if (!ok) return;
    clearAuthToken();
    clearRemoteOrigin();
    window.location.reload();
  };

  return (
    <div className="ob-fold" style={foldStyle}>
      <FoldHeader variant="section" id="settings-phone-link" icon={Smartphone} title="连接电脑 MOSS" defaultOpen />
      <div style={hintStyle}>
        已连接电脑：<span className="mono phone-link-text">{remoteOriginLabel()}</span>
        <br />
        材料、校准参数和系统设置只读，请在电脑上修改；以下其他设置项已隐藏。
      </div>
      <button type="button" className="btn btn-ghost phone-link-btn" onClick={() => void disconnect()}>
        <Unlink size={13} />
        <span>断开，改用手机本机模式</span>
      </button>
    </div>
  );
}
