import React, { useState } from "react";
import { Check, Copy, OctagonX, X } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup, gotoVoiceGroup } from "../VoiceLayout.jsx";
import {
  DEBUG_TEXT_INJECT_CONFIRM_OPTIONS,
  DEBUG_TEXT_INJECT_CONFIRM_TEXT,
  aiBenchChecklistView,
  buildAiBenchPrompt,
  debugInjectStateView,
  voiceErrorText,
} from "../../voiceState.js";
import { BenchRecordingsSection } from "./BenchRecordingsSection.jsx";
import { formatTime } from "./shared.js";

/**
 * VD6 语音中心“记录与诊断 › AI 联调与测试”统一分组（仅内置链路显示，默认展开）：
 * 按用户确认顺序依次放置（识别语料已合并进完整过程记录与下方的“对话记录与排障”）：
 * 1. 4 项联调状态清单（紧凑双列网格，已就绪项单行展示，未就绪项展开修复指引）；
 * 2. 一键复制 AI 整体测试 / 文字直通 / 识别测评提示词按钮（支持勾选数字注入对照与背景噪声）；
 * 3. VD2 “录制测试语料”引导；
 * 4. 从“服务”页挪来的“允许本机 AI 调试注入”开关（保留确认弹窗与 #voice-debug-inject-switch）。
 */
export function AiBenchAssistantCard({ builtin, active = true }) {
  const status = builtin?.status;
  const busy = builtin?.busy;
  const act = builtin?.act;
  const [copiedKind, setCopiedKind] = useState("");
  const [stoppingId, setStoppingId] = useState("");

  if (!status || status.error) return null;

  const checklist = aiBenchChecklistView(status);
  const debugInject = debugInjectStateView(status);
  const hasBenchWindow = checklist.activeBenchWindows.length > 0;
  const summary = hasBenchWindow
    ? "整体测试中（物理动作已锁定）"
    : "开启开关并连上小智 → 一键复制提示词交由 AI 全自动找语音、跑测试并调优";

  const copyPrompt = async (kind = "e2e", label = "AI 全自动联调与调试") => {
    const prompt = buildAiBenchPrompt(status, kind);
    try {
      await navigator.clipboard.writeText(prompt);
      setCopiedKind(label);
    } catch {
      await showAlert("无法写入剪贴板，请检查系统剪贴板权限", { title: "复制失败", tone: "error" });
    }
  };

  const handleGotoItem = (goto) => {
    if (!goto) return;
    gotoVoiceGroup(goto.tab, goto.group, goto.target);
    if (goto.target && typeof document !== "undefined") {
      const el = document.getElementById(goto.target);
      if (el) {
        const fold = el.closest("details");
        if (fold) fold.open = true;
        if (typeof el.scrollIntoView === "function") {
          el.scrollIntoView({ behavior: "smooth", block: "center" });
        }
        const box = el.closest(".voice-block, .voice-debug-card") || el;
        box.classList.add("ai-bench-flash");
        setTimeout(() => box.classList.remove("ai-bench-flash"), 1400);
      }
    }
  };

  const stopBenchWindow = async (deviceId) => {
    setStoppingId(deviceId);
    try {
      await voiceApi.deviceRequest(deviceId, "close_bench_window");
      await builtin.refresh?.();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "结束测试时间窗失败", tone: "error" });
    } finally {
      setStoppingId("");
    }
  };

  const toggleDebugInject = async (on) => {
    if (on) {
      const ok = await showConfirm(
        DEBUG_TEXT_INJECT_CONFIRM_TEXT,
        DEBUG_TEXT_INJECT_CONFIRM_OPTIONS,
      );
      if (!ok) return;
    }
    await act?.(on ? "开启本机 AI 调试注入" : "关闭本机 AI 调试注入", () =>
      voiceApi.localSave({ debug_text_inject_enabled: on }),
    );
  };

  return (
    <VoiceGroup
      id="ai-bench-hub"
      title="AI 联调与测试"
      summary={summary}
      tone={hasBenchWindow ? "warn" : undefined}
      defaultOpen
    >
      <div className="voice-stack" id="voice-ai-bench-card">
        {hasBenchWindow && (
          <div className="voice-hint voice-hint--warn" id="voice-ai-bench-active-banner">
            <div className="ai-bench-banner-row">
              <span>
                <b>整体测试中（物理动作已锁定）</b>：
                {checklist.activeBenchWindows.map((w) => w.device_id).join("、")}
                的所有轮次均按模拟处理，不触发物理动作、不写长期记忆。
              </span>
              <span className="voice-actions">
                {checklist.activeBenchWindows.map((w) => (
                  <button
                    key={w.device_id}
                    type="button"
                    className="btn btn-primary"
                    id={`voice-ai-bench-stop-${w.device_id}`}
                    disabled={Boolean(stoppingId)}
                    onClick={() => void stopBenchWindow(w.device_id)}
                  >
                    <OctagonX size={13} /> 立即结束测试
                  </button>
                ))}
              </span>
            </div>
          </div>
        )}

        {/* 1. 状态清单（紧凑双列网格：开关可就地一键切换，语音素材由 AI 自动准备并缓存） */}
        <div
          className="ai-bench-checklist-grid"
          id="voice-ai-bench-checklist"
          role="list"
          aria-label="AI 联调前状态清单"
        >
          {checklist.items.map((item) => {
            const isAutoPreparedRecording = item.key === "bench_recordings";
            const visualOk = item.ok || isAutoPreparedRecording;
            const compact = visualOk && !item.active;
            return (
              <div
                key={item.key}
                role="listitem"
                className={`voice-debug-card ai-bench-check-card${compact ? " is-compact" : " is-actionable"}`}
                id={`voice-ai-bench-item-${item.key}`}
                data-ready={item.ok ? "true" : "false"}
                title={item.fixHint}
              >
                <div className="ai-bench-check-head">
                  <span className="ai-bench-check-title">
                    <span
                      className={`ob-pill ${item.active ? "ob-pill--warn" : visualOk ? "ob-pill--ok" : "ob-pill--off"}`}
                      aria-label={visualOk ? "已就绪" : "待处理"}
                    >
                      {visualOk ? <Check size={12} /> : <X size={12} />} {item.statusText}
                    </span>
                    <b>{item.label}</b>
                  </span>
                  {item.key === "debug_inject" ? (
                    <button
                      type="button"
                      className={`btn ${debugInject.enabled ? "btn-ghost" : "btn-primary"} voice-inline-btn`}
                      disabled={Boolean(busy)}
                      onClick={() => void toggleDebugInject(!debugInject.enabled)}
                    >
                      {debugInject.enabled ? "关闭开关" : "立即开启"}
                    </button>
                  ) : !visualOk && item.goto ? (
                    <button
                      type="button"
                      className="btn btn-ghost voice-inline-btn"
                      onClick={() => handleGotoItem(item.goto)}
                    >
                      去连接
                    </button>
                  ) : null}
                </div>
                <div className="voice-muted ai-bench-check-hint">{item.fixHint}</div>
              </div>
            );
          })}
        </div>

        {/* 2. 唯一主操作：一键复制 AI 全自动调试提示词 */}
        <div className="ai-bench-prompt-box" id="voice-ai-bench-prompt-actions">
          <div className="ai-bench-prompt-main">
            <button
              type="button"
              className="btn btn-primary ai-bench-copy-hero"
              id="voice-ai-bench-copy-e2e"
              onClick={() => void copyPrompt("e2e", "AI 全自动联调与调试")}
            >
              <Copy size={14} /> 一键复制 AI 全自动调试提示词
            </button>
            <div className="voice-muted ai-bench-prompt-desc">
              <b>你只需做两步：</b>① 点上方「立即开启」打开联调开关 → ② 唤醒小智放在电脑喇叭旁（30–50 cm）→ 点此按钮复制发给 AI。
              <br />
              AI 会<b>自动从网上下载中文真人语音并缓存到本机复用</b>（无需你开口说话），自动按顺序跑完<b>文字直通快检、真小智外放全链路（含数字对照与风扇噪声）与离线识别测评</b>，并自动改代码复测。
            </div>
          </div>
        </div>

        {copiedKind && (
          <div className="voice-ok" id="voice-ai-bench-copied-notice">
            已复制「{copiedKind}」提示词到剪贴板，直接粘贴给 AI 即可开始全自动调试（不含任何令牌、密钥或本机绝对路径）。
          </div>
        )}

        {/* 3. VD2 “录制测试语料”引导（默认折叠：平时由 AI 自动从网上找真人语音并缓存，想用自己声音时再展开） */}
        <details className="voice-item ai-bench-optional-fold" id="voice-bench-recordings-fold">
          <summary>
            <span>自己用麦克风录声音（可选 · 默认不用录，AI 会自动从网上准备真人语音并缓存复用）</span>
            <span className={`ob-pill ${checklist.totalRecordings > 0 ? "ob-pill--ok" : "ob-pill--off"}`}>
              {checklist.totalRecordings > 0 ? `本机已缓存 ${checklist.totalRecordings} 条` : "选填"}
            </span>
          </summary>
          <div className="voice-item-b">
            <BenchRecordingsSection builtin={builtin} active={active} />
          </div>
        </details>

        {/* 4. 从“服务”页挪来的“允许本机 AI 调试注入”开关（保留确认弹窗与原 id） */}
        <div className="voice-block" id="voice-debug-inject-box">
          <div className="voice-block-h">
            <h3>允许 AI 联调开关</h3>
            <span className={`ob-pill ${debugInject.enabled ? "ob-pill--ok" : "ob-pill--off"}`}>
              {debugInject.enabled ? "已开启" : "未开启"}
            </span>
          </div>
          <label className="voice-check">
            <input
              type="checkbox"
              id="voice-debug-inject-switch"
              checked={debugInject.enabled}
              disabled={Boolean(busy)}
              onChange={(e) => void toggleDebugInject(e.target.checked)}
            />
            <span>允许本机 AI 联调脚本连接调试（仅限本机 127.0.0.1，24 小时自动关闭，测试期间全程锁死激光机物理动作）</span>
          </label>
          {debugInject.enabled ? (
            <span className="voice-muted" id="voice-debug-inject-meta">
              令牌位置：<code className="mono">{`<运行目录>/${debugInject.tokenFileRel}`}</code>
              {debugInject.active && debugInject.port ? ` · 本机入口 127.0.0.1:${debugInject.port}` : " · 启动内置服务后开放本机入口"}
              {debugInject.expiresAt ? ` · 到期 ${formatTime(debugInject.expiresAt)}` : ""}
            </span>
          ) : (
            <span className="voice-muted">
              默认关闭。开启后可由本机联调脚本运行整体测试，或在“设备 › 已连接设备”中向在线小智直接发送测试文字。
            </span>
          )}
        </div>
      </div>
    </VoiceGroup>
  );
}
