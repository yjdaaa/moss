import React, { useEffect, useRef, useState } from "react";
import { Check, Copy, OctagonX, X } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup, gotoVoiceGroup } from "../VoiceLayout.jsx";
import { LaserBenchSection } from "../LaserBenchSection.jsx";
import { DRY_RUN_DEFAULT_MAX_FEED, formatLaserBenchStatusLine } from "../../laserBenchPrompt.js";
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
 * VD6 / LD 语音中心「记录与诊断 › AI 联调与测试」统一分组（所有语音链路均显示，默认展开）：
 * - 内置语音（builtin）模式下依次显示语音部分（4 项状态清单［首项内置 #voice-debug-inject-switch 开关与令牌说明］→ 一键复制语音提示词 → 可选录制测试语料）与「激光雕刻机测试」子块；
 * - 非内置模式下只显示「激光雕刻机测试」子块（一行状态 + 空跑开关与限速 + 一键复制 AI 激光测试提示词），摘要相应调整。
 */
export function AiBenchAssistantCard({
  mode = "builtin",
  builtin = null,
  active = true,
  connectionStatus = null,
  dryRunStatus = null,
  onDryRunChange = null,
}) {
  const status = builtin?.status;
  const busy = builtin?.busy;
  const act = builtin?.act;
  const [copied, setCopied] = useState(false);
  const [stoppingId, setStoppingId] = useState("");
  const copyTimerRef = useRef(null);

  useEffect(() => () => {
    if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
  }, []);

  const isBuiltin = mode === "builtin";
  const showVoice = isBuiltin && Boolean(status && !status.error);
  const checklist = showVoice ? aiBenchChecklistView(status) : null;
  const debugInject = showVoice ? debugInjectStateView(status) : null;
  const hasBenchWindow = Boolean(checklist?.activeBenchWindows?.length);
  const laserStatusLine = formatLaserBenchStatusLine({
    connected: Boolean(connectionStatus?.connected),
    dryRun: dryRunStatus,
  });

  const summary = showVoice
    ? hasBenchWindow
      ? "整体测试中（物理动作已锁定）"
      : dryRunStatus?.enabled
        ? `小智语音联调 · 激光空跑开启（限速 ${Number(dryRunStatus.max_feed_mm_min) || DRY_RUN_DEFAULT_MAX_FEED} mm/min）`
        : "小智语音联调与激光雕刻机测试（空跑开关、一键复制提示词）"
    : `激光雕刻机测试 · ${laserStatusLine}`;

  const groupTone = (showVoice && hasBenchWindow) || dryRunStatus?.enabled ? "warn" : undefined;

  const copyPrompt = async (kind = "e2e") => {
    const prompt = buildAiBenchPrompt(status, kind);
    try {
      await navigator.clipboard.writeText(prompt);
      setCopied(true);
      if (copyTimerRef.current) clearTimeout(copyTimerRef.current);
      copyTimerRef.current = setTimeout(() => setCopied(false), 2500);
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
      await builtin?.refresh?.();
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
      tone={groupTone}
      defaultOpen
    >
      <div className="voice-stack" id="voice-ai-bench-card">
        {showVoice && (
          <div className="voice-stack" id="voice-ai-bench-voice-section">
            <div className="ai-bench-section-head">小智语音联调</div>
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
                        <b>{item.label}</b>
                      </span>
                      <span className="ai-bench-check-meta">
                        <span
                          className={`ob-pill ${item.active ? "ob-pill--warn" : visualOk ? "ob-pill--ok" : "ob-pill--off"}`}
                          aria-label={visualOk ? "已就绪" : "待处理"}
                        >
                          {visualOk ? <Check size={12} /> : <X size={12} />}
                          <span>{item.statusText}</span>
                        </span>
                        {item.key === "debug_inject" ? (
                          <input
                            type="checkbox"
                            id="voice-debug-inject-switch"
                            className="ai-bench-toggle"
                            checked={debugInject.enabled}
                            disabled={Boolean(busy)}
                            aria-label="允许本机 AI 联调开关"
                            onChange={(e) => void toggleDebugInject(e.target.checked)}
                          />
                        ) : !visualOk && item.goto ? (
                          <button
                            type="button"
                            className="btn btn-ghost voice-inline-btn"
                            onClick={() => handleGotoItem(item.goto)}
                          >
                            去连接
                          </button>
                        ) : null}
                      </span>
                    </div>
                    <div className="voice-muted ai-bench-check-hint">{item.fixHint}</div>
                    {item.key === "debug_inject" && debugInject.enabled && (
                      <div className="voice-muted ai-bench-check-meta-line" id="voice-debug-inject-meta">
                        令牌：<code className="mono">{`<运行目录>/${debugInject.tokenFileRel}`}</code>
                        {debugInject.active && debugInject.port
                          ? ` · 入口 127.0.0.1:${debugInject.port}`
                          : " · 启动内置服务后开放入口"}
                        {debugInject.expiresAt ? ` · 到期 ${formatTime(debugInject.expiresAt)}` : ""}
                      </div>
                    )}
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
                  onClick={() => void copyPrompt("e2e")}
                >
                  {copied ? <Check size={14} /> : <Copy size={14} />}
                  <span>{copied ? "已复制，直接发给 AI 即可" : "一键复制 AI 全自动调试提示词"}</span>
                </button>
                <div className="voice-muted ai-bench-prompt-desc">
                  <b>两步开始：</b>先打开上方联调开关，唤醒小智放到电脑喇叭旁（30–50 cm），再点左侧复制发给 AI。
                  <br />
                  AI 会<b>自动准备中文真人语音并缓存复用</b>，<b>仅测试小智语音链路、不测试激光雕刻机</b>，完成后按结果调优（不含密钥或本机路径）。
                </div>
              </div>
            </div>

            {/* 3. VD2 “录制测试语料”引导（默认折叠：平时由 AI 自动从网上找真人语音并缓存，想用自己声音时再展开） */}
            <details className="voice-fold ai-bench-optional-fold" id="voice-bench-recordings-fold">
              <summary>
                <span className="ai-bench-optional-fold-title">
                  自己用麦克风录声音（可选 · 默认不用录，AI 会自动准备真人语音并缓存复用）
                </span>
                <span
                  className={`ob-pill ai-bench-optional-fold-pill ${checklist.totalRecordings > 0 ? "ob-pill--ok" : "ob-pill--off"}`}
                >
                  {checklist.totalRecordings > 0 ? `本机已缓存 ${checklist.totalRecordings} 条` : "选填"}
                </span>
              </summary>
              <div className="ai-bench-optional-fold-body">
                <BenchRecordingsSection builtin={builtin} active={active} />
              </div>
            </details>
          </div>
        )}

        {/* 5. LD（2026-11-28 用户决定）：激光雕刻机测试子块，所有语音链路下始终显示 */}
        <div className={showVoice ? "ai-bench-laser-divider" : undefined}>
          <LaserBenchSection
            embedded
            connectionStatus={connectionStatus}
            initialDryRun={dryRunStatus}
            onDryRunChange={onDryRunChange}
          />
        </div>
      </div>
    </VoiceGroup>
  );
}
