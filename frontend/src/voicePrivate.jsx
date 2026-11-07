import React, { useEffect, useState } from "react";
import { Brain, Newspaper, AudioLines, House, Mic, Cpu, ChevronLeft, ChevronRight, ShieldCheck } from "lucide-react";
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
  { key: "memory", title: "记忆", description: "看小智记住了什么，决定是否继续记住", icon: Brain },
  { key: "plugins", title: "信息服务", description: "按需要选择天气、新闻或搜索", icon: Newspaper },
  { key: "speaker", title: "声纹", description: "登记家人，完成标定后再开启", icon: AudioLines },
  { key: "home", title: "家居", description: "连接设备；每次动作仍需你批准", icon: House },
];
// 2026-11-07 用户决定：唤醒词与固件升级分成两个独立任务，不再合在“设备维护”里。
const DEVICE_TASKS = [
  { key: "wakeword", title: "唤醒词", description: "把“你好小智”换成“你好 MOSS”等，逐台推送", icon: Mic },
  { key: "firmware", title: "固件升级", description: "导入自己编译的固件，逐台确认推送", icon: Cpu },
];
const SECTION_TITLES = { approvals: "家居审批", ...Object.fromEntries([...PRIVATE_TASKS, ...DEVICE_TASKS].map((item) => [item.key, item.title])) };
const PRIVATE_SECTIONS = new Set([...Object.keys(SECTION_TITLES), "overview"]);

function TaskButton({ item, onSelect }) {
  const Icon = item.icon;
  return (
    <button type="button" className="voice-private-task" id={`voice-private-task-${item.key}`} onClick={() => onSelect(item.key)}>
      <span className="voice-private-task-icon" aria-hidden="true"><Icon size={18} /></span>
      <span className="voice-private-task-text"><strong>{item.title}</strong><small>{item.description}</small></span>
      <ChevronRight className="voice-private-task-arrow" size={18} aria-hidden="true" />
    </button>
  );
}
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

  const title = SECTION_TITLES[section];
  return (
    <main className="voice-private-page voice-private-box">
      {section ? (
        <header className="voice-private-header">
          <button type="button" className="voice-private-back" onClick={() => select(null)}><ChevronLeft size={16} aria-hidden="true" />返回选择</button>
          <h1>{title}</h1>
          <p><ShieldCheck size={14} aria-hidden="true" /> 仅在 MOSS 本机安全窗口管理，不经过网页或手机。</p>
        </header>
      ) : (
        <>
          <h1>隐私与家居</h1>
          <p className="voice-muted">一次只设置一件事。敏感内容不会在语音中心的普通页面显示。</p>
          <div className="voice-private-tasks" role="group" aria-label="隐私与家居">
            {PRIVATE_TASKS.map((item) => <TaskButton key={item.key} item={item} onSelect={select} />)}
          </div>
          <h2 className="voice-private-group-title">设备</h2>
          <div className="voice-private-tasks" role="group" aria-label="设备">
            {DEVICE_TASKS.map((item) => <TaskButton key={item.key} item={item} onSelect={select} />)}
          </div>
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
      {visited.has("wakeword") && <div className="voice-private-panel" hidden={section !== "wakeword"}><VoiceWakeWordCard /></div>}
      {visited.has("firmware") && <div className="voice-private-panel" hidden={section !== "firmware"}><VoiceFirmwareCard /></div>}
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
