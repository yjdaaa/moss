import React, { useEffect, useRef, useState } from "react";
import { Lock, ChevronDown } from "lucide-react";
import VoiceMemoryCard from "./VoiceMemoryCard.jsx";
import VoicePluginsCard from "./VoicePluginsCard.jsx";
import VoiceSpeakerCard from "./VoiceSpeakerCard.jsx";
import VoiceHomeCard from "./VoiceHomeCard.jsx";
import VoiceHomeApprovalCard from "./VoiceHomeApprovalCard.jsx";
import { HOME_APPROVALS, VOICE_FOCUS_EVENT, clearVoiceFocus, peekVoiceFocus } from "../voiceFocus.js";

// 内置语音服务（内置语音技术书 NV1 起）。设备连到 MOSS 本身，不需要外部服务端。
// 识别、大模型、合成三项配齐后为对话模式（NV5），否则为回声测试。
// 语音中心页按标签分开摆放本文件导出的各部分（UI-3；n69 起）：服务、设备、模型、隐私与家居、记录与诊断。
// 实现按组件拆在 ./voice-builtin/ 下（2026-11-13 纯搬移），本文件只保留入口、导出名单与“隐私与家居”入口组件。

export { useBuiltinStatus } from "./voice-builtin/useBuiltinStatus.js";
export { BuiltinServicePanel } from "./voice-builtin/BuiltinServicePanel.jsx";
export { BuiltinDevicesPanel } from "./voice-builtin/BuiltinDevicesPanel.jsx";
export { BuiltinScreenPreview } from "./voice-builtin/BuiltinScreenPreview.jsx";
export { XiaozhiUsbProvisionCard } from "./voice-builtin/XiaozhiUsbProvisionCard.jsx";
export { BuiltinModelGroups } from "./voice-builtin/BuiltinModelGroups.jsx";
export { BuiltinJevModelGroup } from "./voice-builtin/BuiltinModelGroups.jsx";
export { LocalModelsGroup } from "./voice-builtin/LocalModelsGroup.jsx";
export { ExperienceGroup } from "./voice-builtin/ExperienceGroup.jsx";
export { LiveDebugTraces } from "./voice-builtin/LiveDebugTraces.jsx";
export { BuiltinEvents } from "./voice-builtin/BuiltinEvents.jsx";

const PRIVATE_TASKS = [
  { key: "memory", title: "记忆", description: "查看和管理小智记住的内容" },
  { key: "plugins", title: "信息服务", description: "选择天气、新闻和搜索等服务" },
  { key: "speaker", title: "声纹", description: "登记家人，完成标定后再启用" },
  { key: "home", title: "家居", description: "连接设备；每次控制仍需批准" },
];

export function PrivateSettingsEntry({ status }) {
  const [opened, setOpened] = useState(() => new Set());
  const [advanced, setAdvanced] = useState(() => new Set());
  const expand = (key) => setOpened((old) => (old.has(key) ? old : new Set([...old, key])));
  const expandAdvanced = (key) => setAdvanced((old) => (old.has(key) ? old : new Set([...old, key])));
  const homeRef = useRef(null);
  const homeApprovalsRef = useRef(null);
  const [focusApprovals, setFocusApprovals] = useState(false);
  // 全局审批卡“去家居审批”跳过来时，展开“家居 → 家居审批”并滚到那里
  useEffect(() => {
    const focusHomeApprovals = () => {
      expand("home");
      expandAdvanced("home");
      if (homeRef.current) homeRef.current.open = true;
      setFocusApprovals(true);
      clearVoiceFocus(HOME_APPROVALS);
    };
    if (peekVoiceFocus() === HOME_APPROVALS) focusHomeApprovals();
    const onFocus = (event) => {
      if (event.detail === HOME_APPROVALS) setTimeout(focusHomeApprovals, 0);
    };
    window.addEventListener(VOICE_FOCUS_EVENT, onFocus);
    return () => window.removeEventListener(VOICE_FOCUS_EVENT, onFocus);
  }, []);
  // “家居”展开后家居审批才渲染，渲染出来再展开并滚动过去
  useEffect(() => {
    if (!focusApprovals || !homeApprovalsRef.current) return;
    homeApprovalsRef.current.open = true;
    homeApprovalsRef.current.scrollIntoView?.({ block: "start", behavior: "smooth" });
    setFocusApprovals(false);
  }, [focusApprovals, opened]);
  return (
    <section className="voice-private-entry" aria-label="隐私与家居">
      <div className="voice-private-entry-heading">
        <Lock size={17} aria-hidden="true" />
        <div><h3>你想让小智做什么？</h3><p>点开哪一项就设置哪一项，不用一次看完所有选项。</p></div>
      </div>
      <div className="voice-private-tasks">
        {PRIVATE_TASKS.map(({ key, title, description }) => (
          <details key={key} className="voice-private-item" id={`voice-private-${key}-item`}
            ref={key === "home" ? homeRef : undefined}
            onToggle={(event) => { if (event.currentTarget.open) expand(key); }}>
            <summary>
              <ChevronDown className="voice-private-item-arrow" size={15} aria-hidden="true" />
              <strong>{title}</strong>
              <small>{description}</small>
            </summary>
            <div className="voice-private-item-body">
              {key === "memory" && opened.has("memory") && <VoiceMemoryCard status={status} />}
              {key === "plugins" && opened.has("plugins") && <VoicePluginsCard />}
              {key === "speaker" && opened.has("speaker") && <VoiceSpeakerCard />}
              {key === "home" && opened.has("home") && (
                <>
                  <VoiceHomeCard />
                  <details className="voice-private-more" id="voice-private-home-approvals"
                    ref={homeApprovalsRef}
                    onToggle={(event) => { if (event.currentTarget.open) expandAdvanced("home"); }}>
                    <summary>家居审批</summary>
                    {advanced.has("home") && <VoiceHomeApprovalCard />}
                  </details>
                </>
              )}
            </div>
          </details>
        ))}
      </div>
      <p className="voice-muted">内容只保存在本机，不上传；家居每次动作仍需你在电脑上批准，语音报告只作参考。</p>
    </section>
  );
}

