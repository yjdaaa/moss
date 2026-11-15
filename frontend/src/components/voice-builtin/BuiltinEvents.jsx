import React, { useState } from "react";
import { Download } from "lucide-react";
import { showAlert } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup } from "../VoiceLayout.jsx";
import {
  filterVoiceEvents,
  voiceEventDropNotices,
  voiceEventView,
  voiceErrorText,
  VOICE_EVENT_SOURCES,
  voiceEventSourceCounts,
} from "../../voiceState.js";

/**
 * 事件记录（小智 · 激光雕刻 · MOSS 软件，n76）与诊断包（2026-11-07 技术书 n68）：显示全部保留的事件（日常记录 ≤300、错误与警告 ≤200，
 * 服务停止 / 重启不清空），带级别、分类、下一步与技术细节；“只看错误和警告”筛选；被覆盖条数明确提示；
 * “导出诊断包”由后端生成脱敏 JSON 并弹出系统保存框。
 */
export function BuiltinEvents({ builtin }) {
  const status = builtin?.status;
  const all = Array.isArray(status?.events) ? status.events : [];
  const [onlyProblems, setOnlyProblems] = useState(false);
  const [sourceFilter, setSourceFilter] = useState("all");
  const [exporting, setExporting] = useState(false);
  const [notice, setNotice] = useState("");
  const shown = filterVoiceEvents(all, onlyProblems, sourceFilter).map(voiceEventView);
  const sourceCounts = voiceEventSourceCounts(all);
  const problems = all.filter((event) => event?.level === "warn" || event?.level === "error").length;
  const drops = voiceEventDropNotices(status?.events_dropped);
  const warnings = Array.isArray(status?.warnings) ? status.warnings : [];

  const exportBundle = async () => {
    setExporting(true);
    setNotice("");
    try {
      const result = await voiceApi.diagnosticsExport();
      if (result?.saved) setNotice(`诊断包已保存：${result.file_name}（${result.events} 条事件，${Math.ceil((result.bytes || 0) / 1024)} KB）`);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "导出诊断包失败", tone: "error" });
    } finally {
      setExporting(false);
    }
  };

  return (
    <VoiceGroup
      id="builtin-events"
      title="事件记录（小智 · 激光雕刻 · MOSS 软件）与诊断包"
      summary={all.length ? `${all.length} 条${problems ? ` · 错误/警告 ${problems} 条` : ""}` : "暂无"}
      defaultOpen
    >
      <div className="voice-stack" id="voice-builtin-events-card">
        <div className="voice-actions">
          <div className="voice-event-sources" role="group" aria-label="按来源筛选" id="voice-events-source-filter">
            {[["all", "全部", all.length, 0], ...Object.entries(VOICE_EVENT_SOURCES).map(([key, meta]) => [key, meta.label, sourceCounts[key].total, sourceCounts[key].problems])].map(([key, label, total, problemCount]) => (
              <button
                type="button"
                key={key}
                className={`voice-chip${sourceFilter === key ? " is-active" : ""}`}
                aria-pressed={sourceFilter === key}
                onClick={() => setSourceFilter(key)}
                id={`voice-events-source-${key}`}
              >
                {label} {total}{problemCount ? ` · 问题 ${problemCount}` : ""}
              </button>
            ))}
          </div>
          <label className="voice-check">
            <input type="checkbox" checked={onlyProblems} onChange={(e) => setOnlyProblems(e.target.checked)} id="voice-events-only-problems" />
            <span>只看错误和警告</span>
          </label>
          <button type="button" className="btn btn-ghost" disabled={exporting} onClick={exportBundle} id="voice-diagnostics-export">
            <Download size={13} /> {exporting ? "正在导出…" : "导出诊断包"}
          </button>
        </div>
        <p className="voice-muted">
          汇总小智（设备与对话）、激光雕刻机（握手、加工、报警、急停）和 MOSS 软件本身的事件，每条前面标明来源。只存在本机内存，服务停止或重启不清空，退出 MOSS 后清空。诊断包会去掉密钥、令牌、带账号密码的地址和 IP，不含录音与对话记忆。
        </p>
        {notice && <div className="voice-ok" id="voice-diagnostics-export-result">{notice}</div>}
        {status?.last_error && (
          <div className="voice-hint voice-hint--bad" id="voice-events-last-error">最近一次启动失败：{status.last_error}</div>
        )}
        {warnings.map((warning) => (
          <div className="voice-hint voice-hint--warn" key={warning}>{warning}</div>
        ))}
        {drops.map((text) => (
          <div className="voice-hint voice-hint--warn voice-events-dropped" key={text}>{text}</div>
        ))}
        {shown.length === 0 ? (
          <p className="voice-muted" id="voice-events-empty">{onlyProblems ? "没有错误或警告。" : "暂无事件。"}{sourceFilter !== "all" ? `（只看“${VOICE_EVENT_SOURCES[sourceFilter].label}”）` : ""}</p>
        ) : (
          <ul className="voice-event-list" id="voice-builtin-events">
            {shown.map((view) => (
              <li key={view.key} className={`voice-event voice-event--${view.level}`} data-level={view.level} data-category={view.category} data-source={view.source}>
                <div className="voice-event-head">
                  <span className={`voice-event-source voice-event-source--${view.source}`}>{view.sourceLabel}</span>
                  <span className={`ob-pill ob-pill--${view.tone}`}>{view.levelText}</span>
                  <span className="ob-pill ob-pill--off" title={view.cause || undefined}>{view.categoryLabel}</span>
                  <span className="voice-muted mono">{view.time}</span>
                </div>
                <div className="voice-event-text">{view.text}</div>
                {view.hint && <div className="voice-event-hint">怎么办：{view.hint}</div>}
                {view.detail && (
                  <details className="voice-event-detail">
                    <summary>技术细节</summary>
                    <pre className="voice-log mono">{view.detail}</pre>
                  </details>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </VoiceGroup>
  );
}
