import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import VoicePluginsCard from "./components/VoicePluginsCard.jsx";
import VoiceMemoryCard from "./components/VoiceMemoryCard.jsx";
import VoiceHomeCard from "./components/VoiceHomeCard.jsx";
import VoiceHomeApprovalCard from "./components/VoiceHomeApprovalCard.jsx";
import VoiceSpeakerCard from "./components/VoiceSpeakerCard.jsx";
import VoiceFirmwareCard from "./components/VoiceFirmwareCard.jsx";
import { voiceApi } from "./voiceApi.js";
import "./styles.css";

// This is a separate packaged Tauri page, not a route on the remotely
// loaded /voice page. Its native commands are separately origin-gated.
export function VoicePrivatePage() {
  const [status, setStatus] = useState(null);
  const [error, setError] = useState("");
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

  return (
    <main className="voice-private-page voice-private-box">
      <h1>语音隐私设置</h1>
      <p>此窗口由 MOSS 桌面安装包直接加载，专用于私人记忆、信息插件和 Home Assistant 凭据及白名单设置。令牌不会回显，普通网页无法调用敏感命令。</p>
      {error && <p role="alert">{error}</p>}
      <VoicePluginsCard />
      <VoiceMemoryCard status={status} />
      <VoiceSpeakerCard />
      <VoiceFirmwareCard />
      <VoiceHomeCard />
      <VoiceHomeApprovalCard />
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
