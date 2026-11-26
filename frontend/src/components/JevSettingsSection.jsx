import React, { useCallback, useEffect, useRef, useState } from "react";
import { Link2, Eye, EyeOff, Trash2 } from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { FoldHeader } from "./FoldHeader.jsx";
import { VoiceGroup } from "./VoiceLayout.jsx";
import { jevSettingsSummary } from "../voiceState.js";

/** 子内容卸载时回调一次（用来在卡片收起时清掉明文密钥）。 */
function ClearOnUnmount({ onUnmount, children }) {
  const ref = useRef(onUnmount);
  ref.current = onUnmount;
  useEffect(() => () => ref.current?.(), []);
  return children;
}

/**
 * OpenRouter · Jev 判断服务（加工反馈判断，`/api/jev`）的独立配置块。
 *
 * 入口规则（2026-11-26 用户决定，取代 r32 的“官方链路放偏好设置”）：
 * - 桌面端一律在「语音中心」配置：内置链路在“模型 → Jev”，官方链路在“服务”标签（`frame="voice"`）；
 * - 只有没有语音中心的网页 / 手机环境，才在右上角偏好设置里显示（`frame="settings"`）。
 *
 * `onBusyChange` 把保存 / 测试中的状态交给外层（偏好设置用它阻止保存中关闭弹窗）。
 */
export function JevSettingsSection({ frame = "settings", onBusyChange = null }) {
  const [jevEnabled, setJevEnabled] = useState(false);
  const [savedJevEnabled, setSavedJevEnabled] = useState(false);
  const [jevApiKeyInput, setJevApiKeyInput] = useState("");
  const [jevMaskedKey, setJevMaskedKey] = useState("");
  const [jevRevealedKey, setJevRevealedKey] = useState("");
  const [jevKeyVisible, setJevKeyVisible] = useState(false);
  const [savingJev, setSavingJev] = useState(false);
  const [testingJev, setTestingJev] = useState(false);
  const [jevSaveStatus, setJevSaveStatus] = useState("");

  useEffect(() => {
    let alive = true;
    getJson("/api/jev/status").then((res) => {
      if (!alive) return;
      if (res.ok && res.data?.success) {
        const jev = res.data.result || {};
        const enabled = Boolean(jev.enabled);
        setJevEnabled(enabled);
        setSavedJevEnabled(enabled);
        setJevMaskedKey(jev.masked_key || "");
      }
    });
    return () => { alive = false; };
  }, []);

  const busy = savingJev || testingJev;
  useEffect(() => {
    onBusyChange?.(busy);
  }, [busy, onBusyChange]);

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

  const saveJevSettings = async ({ silentSuccess = false } = {}) => {
    setSavingJev(true);
    setJevSaveStatus("");
    const body = {
      jev_enabled: jevEnabled,
    };
    if (jevApiKeyInput.trim()) body.jev_api_key = jevApiKeyInput.trim();
    try {
      const res = await postJson("/api/settings", body);
      if (res.ok && res.data?.success) {
        setSavedJevEnabled(jevEnabled);
        setJevApiKeyInput("");
        setJevKeyVisible(false);
        setJevRevealedKey("");
        const status = await getJson("/api/jev/status");
        if (status.ok && status.data?.success) setJevMaskedKey(status.data.result?.masked_key || "");
        if (!silentSuccess) {
          setJevSaveStatus("已保存");
          setTimeout(() => setJevSaveStatus(""), 4000);
        }
        return true;
      }
      await showAlert(res.data?.result || "保存失败", { title: "Jev 设置保存失败", tone: "error" });
      return false;
    } catch (e) {
      await showAlert(`保存 Jev 设置网络异常: ${e.message}`, { title: "Jev 设置保存失败", tone: "error" });
      return false;
    } finally {
      setSavingJev(false);
    }
  };

  const handleSaveJev = async () => {
    await saveJevSettings({ silentSuccess: false });
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
    // 存在尚未保存的 Jev 输入或开关改动时先走保存，保存失败如实提示并中止，不得静默测旧密钥
    const hasUnsavedJev = Boolean(jevApiKeyInput.trim()) || jevEnabled !== savedJevEnabled;
    if (hasUnsavedJev) {
      const savedOk = await saveJevSettings({ silentSuccess: true });
      if (!savedOk) return;
    }
    setTestingJev(true);
    setJevSaveStatus("");
    try {
      const res = await postJson("/api/jev/test");
      if (res.ok && res.data?.success) {
        setJevSaveStatus(hasUnsavedJev ? "已保存并连接正常" : "连接正常");
      } else {
        await showAlert(res.data?.result || "Jev 连接失败", { title: "Jev 连接测试失败", tone: "error" });
      }
    } catch (e) {
      await showAlert(`测试 Jev 网络异常: ${e.message}`, { title: "Jev 连接测试失败", tone: "error" });
    } finally {
      setTestingJev(false);
    }
  };

  const forgetPlaintext = useCallback(() => {
    setJevKeyVisible(false);
    setJevRevealedKey("");
    setJevApiKeyInput("");
  }, []);

  const summary = jevSettingsSummary({ enabled: jevEnabled, maskedKey: jevMaskedKey });
  const prefix = frame === "voice" ? "voice-official-jev" : "settings-jev";

  const body = (
    <>
      <div className="settings-box-4">
        Jev 只帮助判断“太焦、太浅、切不透”等加工反馈，不直接控制激光机。
      </div>
      <label className="settings-label">
        <input type="checkbox" checked={jevEnabled} onChange={(e) => setJevEnabled(e.target.checked)} />
        <span>启用 Jev 判断</span>
      </label>
      <div className="settings-box-8">
        模型：typesafe/jev-1.13
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
          aria-label="Jev API Key"
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
          disabled={busy}
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
          id={`${prefix}-test`}
          className="btn btn-ghost settings-btn-5"
          onClick={handleTestJev}
          disabled={busy}
        >
          {savingJev ? "保存中..." : testingJev ? "测试中..." : "保存并测试"}
        </button>
        {jevSaveStatus && <span className="settings-text">{jevSaveStatus}</span>}
      </div>
    </>
  );

  if (frame === "voice") {
    return (
      // lazy：收起即卸载内容，卸载时隐藏查看过的明文密钥并清掉未保存的密钥输入（同偏好设置“关闭即重置”）
      <VoiceGroup id="official-jev" title="OpenRouter · Jev 判断服务" summary={summary} lazy>
        <ClearOnUnmount onUnmount={forgetPlaintext}>{body}</ClearOnUnmount>
      </VoiceGroup>
    );
  }
  return (
    <div className="ob-fold settings-box-3" id="settings-jev">
      <FoldHeader variant="section" id="settings-jev" icon={Link2} title="OpenRouter · Jev 判断服务" summary={summary} defaultOpen={false} />
      {body}
    </div>
  );
}
