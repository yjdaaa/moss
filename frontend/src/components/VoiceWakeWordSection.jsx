import React from "react";
import { VoiceGroup } from "./VoiceLayout.jsx";
import VoiceWakeWordCard from "./VoiceWakeWordCard.jsx";

// 设备标签里的“唤醒词”（2026-11-07 用户决定唤醒词单独放一处；n67 同日取消独立安全窗口，唤醒词卡直接挂在主窗口语音中心；
// n69 同日“完整整理”：原“唤醒词与音量”标签并入“设备”，音量只在“已连接设备”的快捷控制里调，不再重复一份）。
// 推送会重启设备并写闪存，每台推送前仍逐台确认（VoiceWakeWordCard 内）。
export default function VoiceWakeWordSection({ mode, active = true }) {
  if (mode !== "builtin") {
    return (
      <div className="voice-hint" id="voice-wake-volume-mode-hint">
        唤醒词、设备音量和屏幕都要通过 MOSS 内置语音服务下发到小智设备。请先在页面顶部“语音链路”卡片选择“MOSS 内置服务”。
      </div>
    );
  }
  return (
    <VoiceGroup id="wake-word" title="唤醒词" summary="默认“你好小智”，可改成“你好 MOSS”等；每台推送前确认" defaultOpen lazy>
      <div className="voice-stack" id="voice-wake-word-section">
        {/* 只在“设备”标签打开、且本组展开时挂载，避免后台每 5 秒查询唤醒词状态 */}
        {active && <VoiceWakeWordCard />}
      </div>
    </VoiceGroup>
  );
}
