import React, { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDown, Mic, ShieldAlert, Sparkles } from "lucide-react";
import { createPollRunner } from "../pollGuard.js";
import { voiceApi } from "../voiceApi.js";
import {
  VOICE_DOCK_POLL_MS,
  VOICE_DOCK_TOOL_LINGER_MS,
  activeLaserTool,
  nextRecentTool,
  readVoiceDockCollapsed,
  shouldPollVoiceSessions,
  voiceDockView,
  writeVoiceDockCollapsed,
} from "../voiceDockState.js";

/**
 * 全局“小智 · 雕刻同步”悬浮小窗（2026-11-16 用户要求）：挂在 App 级，任何页面都能看到小智此刻在听 / 想 / 说、
 * 刚调用了哪个雕刻工具、当前雕刻进度，以及有待确认预览或待批准请求时的入口。
 * 只读展示：不显示对话原文，不发任何写请求；确认、开刻、审批仍只在 AiPreviewCenter / ApprovalCenter 里完成。
 */
export function VoiceActivityDock({
  voiceEnabled = false,
  voiceStatus = null,
  jobs = [],
  previewWaiting = 0,
  onOpenPreview = null,
  approvalWaiting = 0,
  onOpenApprovals = null,
} = {}) {
  const [collapsed, setCollapsed] = useState(readVoiceDockCollapsed);
  const [currentTool, setCurrentTool] = useState("");
  const [recentTool, setRecentTool] = useState(null);
  const [, setClock] = useState(0);
  const polling = shouldPollVoiceSessions({ voiceEnabled, voiceStatus });
  const pollingRef = useRef(polling);
  pollingRef.current = polling;

  // 小智非空闲时读内置语音会话，只取工具注册名；空闲时后端已清空工具名，不再轮询
  useEffect(() => {
    if (!polling) {
      setCurrentTool("");
      return undefined;
    }
    let cancelled = false;
    const sync = createPollRunner(async (isCurrent) => {
      let status = null;
      try {
        status = await voiceApi.builtinStatus({ lite: true });
      } catch {
        return;
      }
      if (cancelled || !isCurrent() || !pollingRef.current) return;
      const label = activeLaserTool(status);
      setCurrentTool(label);
      if (label) setRecentTool((prev) => nextRecentTool(prev, label, Date.now()));
    });
    void sync();
    const timer = setInterval(() => {
      if (!document.hidden) void sync();
    }, VOICE_DOCK_POLL_MS);
    return () => {
      cancelled = true;
      sync.invalidate();
      clearInterval(timer);
    };
  }, [polling]);

  // “刚才调用”到期后重绘一次即可消失
  useEffect(() => {
    if (!recentTool || polling) return undefined;
    const left = recentTool.at + VOICE_DOCK_TOOL_LINGER_MS - Date.now();
    if (left <= 0) return undefined;
    const timer = setTimeout(() => setClock((n) => n + 1), left + 50);
    return () => clearTimeout(timer);
  }, [recentTool, polling]);

  const toggle = useCallback(() => {
    setCollapsed((value) => {
      writeVoiceDockCollapsed(!value);
      return !value;
    });
  }, []);

  const view = voiceDockView({
    voiceEnabled,
    voiceStatus,
    currentTool,
    recentTool,
    jobs,
    previewWaiting,
    approvalWaiting,
    now: Date.now(),
  });
  if (!view.visible) return null;
  const phase = view.voice?.phase || "none";

  if (collapsed) {
    return (
      <div className="voice-dock voice-dock--pill" id="voice-dock-pill" data-phase={phase} data-attention={view.attention ? "true" : "false"}>
        <button type="button" className="voice-dock-pill-main" onClick={toggle} id="voice-dock-pill-open" title="展开小智雕刻同步">
          <span className="voice-dock-dot" aria-hidden="true" />
          <span className="voice-dock-pill-text" id="voice-dock-pill-text">{view.pillText}</span>
        </button>
      </div>
    );
  }

  return (
    <div className="voice-dock" id="voice-dock" role="region" aria-label="小智雕刻同步" data-phase={phase}>
      <div className="voice-dock-h">
        <span className="voice-dock-title">
          <Mic size={13} aria-hidden="true" />
          小智 · 雕刻同步
        </span>
        <button type="button" className="voice-dock-icon" onClick={toggle} id="voice-dock-collapse" title="收成小胶囊">
          <ChevronDown size={14} />
        </button>
      </div>

      {view.voice && (
        <div className="voice-dock-row" id="voice-dock-voice">
          <span className="voice-dock-dot" aria-hidden="true" />
          <span className="voice-dock-phase" id="voice-dock-phase">{view.voice.phaseText}</span>
          <span className="voice-dock-meta">{view.voice.devicesText}</span>
        </div>
      )}

      {view.tool && (
        <div className={`voice-dock-tool${view.tool.active ? " is-active" : ""}`} id="voice-dock-tool">
          {view.tool.text}
        </div>
      )}

      {view.job && (
        <div className="voice-dock-job" id="voice-dock-job" title="停止与急停在下方雕刻任务浮层">
          <span id="voice-dock-job-text">{view.job.text}</span>
          {view.job.percent !== null && (
            <span className="voice-dock-bar" aria-hidden="true">
              <i style={{ width: `${view.job.percent}%` }} />
            </span>
          )}
        </div>
      )}

      {view.previewText && (
        <button
          type="button"
          className="voice-dock-link"
          id="voice-dock-preview"
          onClick={() => typeof onOpenPreview === "function" && onOpenPreview()}
        >
          <Sparkles size={13} aria-hidden="true" />
          <span>{view.previewText}</span>
          <b>查看</b>
        </button>
      )}

      {view.approvalText && (
        <button
          type="button"
          className="voice-dock-link"
          id="voice-dock-approval"
          onClick={() => typeof onOpenApprovals === "function" && onOpenApprovals()}
        >
          <ShieldAlert size={13} aria-hidden="true" />
          <span>{view.approvalText}</span>
          <b>查看</b>
        </button>
      )}
    </div>
  );
}
