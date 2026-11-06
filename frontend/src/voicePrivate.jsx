import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import VoicePluginsCard from "./components/VoicePluginsCard.jsx";
import VoiceMemoryCard from "./components/VoiceMemoryCard.jsx";
import VoiceHomeCard from "./components/VoiceHomeCard.jsx";
import VoiceHomeApprovalCard from "./components/VoiceHomeApprovalCard.jsx";
import VoiceSpeakerCard from "./components/VoiceSpeakerCard.jsx";
import VoiceFirmwareCard from "./components/VoiceFirmwareCard.jsx";
import VoiceWakeWordCard from "./components/VoiceWakeWordCard.jsx";
import { AppDialogHost } from "./components/AppDialog.jsx";
import { voiceApi } from "./voiceApi.js";
import "./styles.css";

// This is a separately packaged Tauri page, never a route on the loopback-hosted /voice page.
const PRIVATE_TASKS = [
  { key: "memory", title: "记忆", description: "看小智记住了什么，决定是否继续记住" },
  { key: "plugins", title: "信息服务", description: "按需要选择天气、新闻或搜索" },
  { key: "speaker", title: "声纹", description: "登记家人，完成标定后再开启" },
  { key: "home", title: "家居", description: "连接设备；每次动作仍需你批准" },
];
const PRIVATE_SECTIONS = new Set([...PRIVATE_TASKS.map((item) => item.key), "firmware", "approvals", "overview"]);
const initialPrivateSection = () => PRIVATE_SECTIONS.has(window.__MOSS_PRIVATE_TARGET__) ? window.__MOSS_PRIVATE_TARGET__ : null;

export function VoicePrivatePage() {
  const [status, setStatus] = useState(null);
  const [error, setError] = useState("");
  const [section, setSection] = useState(() => initialPrivateSection() === "overview" ? null : initialPrivateSection());
  const [visited, setVisited] = useState(() => new Set(initialPrivateSection() && initialPrivateSection() !== "overview" ? [initialPrivateSection()] : []));
  const select = (next) => {
    setSection(next);
    if (next) setVisited((old) => new Set([...old, next]));
  };
  useEffect(() => {
    const onSelection = (event) => { if (PRIVATE_SECTIONS.has(event.detail)) select(event.detail === "overview" ? null : event.detail); };
    window.addEventListener("moss:voice-private-selection", onSelection);
    const initial = initialPrivateSection();
    if (initial) select(initial === "overview" ? null : initial);
    return () => window.removeEventListener("moss:voice-private-selection", onSelection);
  }, []);
  useEffect(() => {
    let alive = true;
    const refresh = async () => {
      try {
        const next = await voiceApi.builtinStatus();
        if (alive) { setStatus(next); setError(""); }
      } catch (cause) {
        if (alive) setError(cause?.message || "无法读取语音服务状态");
      }
    };
    void refresh();
    const timer = setInterval(refresh, 10_000);
    return () => { alive = false; clearInterval(timer); };
  }, []);

  const title = section === "firmware" ? "设备维护" : section === "approvals" ? "家居审批" : PRIVATE_TASKS.find((item) => item.key === section)?.title;
  return (
    <main className="voice-private-page voice-private-box">
      {section ? (
        <header className="voice-private-header">
          <button type="button" className="btn btn-ghost" onClick={() => select(null)}>‹ 返回选择</button>
          <h1>{title}</h1>
          <p>仅在 MOSS 本机安全页面管理。</p>
        </header>
      ) : (
        <>
          <h1>隐私与家居</h1>
          <p className="voice-muted">一次只设置一件事。敏感内容不会在语音中心的普通页面显示。</p>
          <div className="voice-private-tasks">
            {PRIVATE_TASKS.map((item) => (
              <button key={item.key} type="button" className="voice-private-task" onClick={() => select(item.key)}>
                <span><strong>{item.title}</strong><small>{item.description}</small></span><span aria-hidden="true">进入 ›</span>
              </button>
            ))}
          </div>
          <button type="button" className="btn btn-ghost voice-private-secondary" onClick={() => select("firmware")}>设备维护：唤醒词与固件升级…</button>
        </>
      )}
      {error && <p role="alert">{error}</p>}
      {visited.has("memory") && <div className="voice-private-panel" hidden={section !== "memory"}>
        <VoiceMemoryCard status={status} />
      </div>}
      {visited.has("plugins") && <div className="voice-private-panel" hidden={section !== "plugins"}><VoicePluginsCard /></div>}
      {visited.has("speaker") && <div className="voice-private-panel" hidden={section !== "speaker"}><VoiceSpeakerCard /></div>}
      {visited.has("home") && <div className="voice-private-panel" hidden={section !== "home"}>
        <VoiceHomeCard />
        <button type="button" className="btn btn-ghost" onClick={() => select("approvals")}>查看家居审批</button>
      </div>}
      {visited.has("approvals") && <div className="voice-private-panel" hidden={section !== "approvals"}><VoiceHomeApprovalCard /></div>}
      {visited.has("firmware") && <div className="voice-private-panel" hidden={section !== "firmware"}><VoiceWakeWordCard /><VoiceFirmwareCard /></div>}
      <AppDialogHost />
    </main>
  );
}

const mount = document.getElementById("root");
if (mount) {
  const root = createRoot(mount);
  if (!window.__TAURI__?.core?.invoke) {
    root.render(<main role="alert">语音隐私设置只能从 MOSS 桌面 App 的安全窗口打开。</main>);
  } else {
    root.render(<VoicePrivatePage />);
  }
}
