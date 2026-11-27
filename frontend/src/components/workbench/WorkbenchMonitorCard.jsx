import React from "react";
import { AlertTriangle, Box, Clock, Cpu, Download, Flame, PauseCircle, Ruler, Square } from "lucide-react";
import { resolveApiUrl } from "../../api/client.js";
import { isRemoteMode, readAuthToken } from "../../authSession.js";
import { writeFramingShape } from "../../aiPreviewState.js";
import { canResumeJob, formatResumeActionLabel } from "../../estopResumeState.js";
import { timeEstimateNote } from "../../fillOptions.js";
import {
  actualParamsText,
  workbenchPreviewWarnings,
  workbenchSendWarnings,
  workbenchStatusLabel,
} from "../../workbenchParams.js";
import { FoldHeader } from "../FoldHeader.jsx";
import RegistrationPanel from "../RegistrationPanel.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";
import { WorkbenchHeadJogPanel } from "./WorkbenchHeadJogPanel.jsx";

/** 雕刻工作台 ④ 加工监视舱：现场移头与对刀原点、刀路预览、刀路摘要、生成与下发、走边框 / 导出 / 取消、两点对位、急停与下发进度。原样搬自 pages/HomePage.jsx（2026-11-16 按职责拆分） */
// 2026-11-26 合并点动：工作台只保留「现场移头与对刀原点」一套方向键（r60），两点对位改为引导到那里点动
function openHeadJogPanel() {
  const panel = document.getElementById("wb-head-jog");
  if (!panel) return;
  if (!panel.open) panel.querySelector("summary")?.click();
  panel.scrollIntoView?.({ block: "nearest", behavior: "smooth" });
}
const WORKBENCH_REGISTRATION_JOG = {
  hint: "点动：用上方「现场移头与对刀原点」把激光头（激光关闭）对准标记，再点“记录”",
  openLabel: "打开移头面板",
  onOpen: openHeadJogPanel,
};

export function WorkbenchMonitorCard({
  onNavigate,
  previewUrl,
  previewStale,
  currentWorkflow,
  dimensions,
  sendProgress,
  gcodeText,
  timeEstimate,
  fitNote,
  routeNote,
  fillNote,
  busy,
  framingBusy,
  canGenerate = true,
  overlayStatus,
  framingShape,
  setFramingShape,
  connectionForm,
  connectionStatus = null,
  activeJobs = [],
  motionPayload,
  addLog,
  adoptDerivedWorkflow,
  handleGeneratePreview,
  handleConfirmSend,
  handleResumeSend,
  handleFraming,
  handleExportGcode,
  handleCancelJob,
  handleEStop,
}) {
  const previewWarnings = workbenchPreviewWarnings(currentWorkflow);
  const sendWarnings = workbenchSendWarnings(sendProgress, currentWorkflow);
  return (
    <WbCard id="monitor" className="hud-card ob-card ob-monitor" label="加工监视舱">
      <FoldHeader id="wb-monitor" title="加工监视舱" n={4} />
        <WorkbenchHeadJogPanel
          connectionForm={connectionForm}
          connectionStatus={connectionStatus}
          busy={busy}
          framingBusy={framingBusy}
          overlayStatus={overlayStatus}
          activeJobs={activeJobs}
          onLog={addLog}
        />
        <div className={`chamber-viewport ${previewUrl ? "has-preview" : "is-empty"}`}>
          {/* 坐标雷达网格背景 */}
          <div className="chamber-grid" />

          {previewUrl ? (
            <div className={`wb-row-7${previewStale ? " is-stale" : ""}`}>
              <img
                src={previewUrl}
                alt="加工轨迹预览"
                onError={(e) => {
                  // 远程 App 或局域网配对浏览器下 /preview 需要 Bearer 头，<img> 直链会 401，不覆写为无鉴权直链
                  if (isRemoteMode() || Boolean(readAuthToken())) return;
                  const imgFile = currentWorkflow?.artifacts?.image_file;
                  if (imgFile && !e.currentTarget.dataset.retried) {
                    e.currentTarget.dataset.retried = "true";
                    e.currentTarget.src = resolveApiUrl(`/preview?path=${encodeURIComponent(imgFile)}&t=${Date.now()}`);
                  }
                }}
                className="wb-box-7"
              />
              <div
                className={`wb-box-8${previewStale ? " is-stale" : ""}`}
                id="wb-preview-badge"
              >
                {previewStale
                  ? "参数已改动 · 需重新生成轨迹"
                  : `刀路预览就绪${currentWorkflow?.artifacts?.origin === "center" ? " · 中心起点" : currentWorkflow?.artifacts?.origin === "lower_left" ? " · 左下角起点" : ""}`}
              </div>
            </div>
          ) : (
            <div className="wb-box-9">
              <div
                className="wb-grid-4"
              >
                <Cpu size={28} className="wb-icon-2" />
              </div>
              <div className="wb-box-10">
                加工监视舱就绪
              </div>
              <div className="wb-box-11">
                请在左侧设定参数后点击【计算加工轨迹】
              </div>
            </div>
          )}
        </div>

        {/* 两点对位（2026-11-16，对齐 LightBurn Print and Cut）：默认收起，放在吸底操作卡上方以免展开后撑高常驻条 */}
        {currentWorkflow?.workflow_id && (
          <RegistrationPanel
            workflow={currentWorkflow}
            connectionForm={connectionForm}
            machineMotion={motionPayload.machine_motion}
            disabled={
              busy
              || framingBusy
              || previewStale
              || ["running", "sending", "preparing", "awaiting_completion"].includes(overlayStatus)
            }
            onLog={addLog}
            onRegistered={(workflow) => adoptDerivedWorkflow(workflow, "对位预览")}
            externalJog={WORKBENCH_REGISTRATION_JOG}
          />
        )}

        <div className="job-dispatch-card">
          {/* 刀路摘要：尺寸与预估耗时（r47 去掉了刀路总行数与 SHA-256 这两项技术细节） */}
          <div className="toolpath-capsule" id="wb-toolpath-stats">
            <div className="toolpath-item">
              <span className="toolpath-label"><Ruler size={11} aria-hidden="true" /> 轮廓尺寸</span>
              <span className="toolpath-value">
                {dimensions ? `${dimensions.width.toFixed(1)} × ${dimensions.height.toFixed(1)} mm` : "—"}
              </span>
            </div>
            <div className="toolpath-item">
              <span className="toolpath-label"><Clock size={11} aria-hidden="true" /> 预估耗时</span>
              <span className="toolpath-value wb-text-5">
                {timeEstimate ? `${timeEstimate.estimated_seconds}s（${timeEstimate.estimated_minutes} 分钟）` : "—"}
              </span>
            </div>
          </div>
          {previewStale && (
            <div className="wb-stale-notice" id="wb-stale-notice" role="status">
              左侧内容或参数已改动，当前预览仍是上一轮旧刀路；请先点「重新生成轨迹」后再走边框或开刻。
            </div>
          )}
          {actualParamsText(currentWorkflow) && (
            <div className="wb-origin-note" id="wb-actual-params">加工参数：{actualParamsText(currentWorkflow)}</div>
          )}
          {dimensions?.originNote && (
            <div className="wb-origin-note" id="wb-origin-note">{dimensions.originNote}</div>
          )}
          {fitNote && (
            <div className="wb-origin-note" id="wb-fit-note">{fitNote}</div>
          )}
          {routeNote && (
            <div className="wb-origin-note" id="wb-route-note">{routeNote}</div>
          )}
          {fillNote && (
            <div className="wb-origin-note" id="wb-fill-note">刀路：{fillNote}</div>
          )}
          {timeEstimateNote(timeEstimate) && (
            <div className="wb-origin-note" id="wb-time-note">耗时：{timeEstimateNote(timeEstimate)}</div>
          )}
          {previewWarnings.length > 0 && (
            <div className="wb-preview-warnings" id="wb-preview-warnings" role="status">
              {previewWarnings.map((warning, idx) => (
                <div key={`${idx}-${warning}`} className="wb-stale-notice">
                  {warning}
                </div>
              ))}
            </div>
          )}

          {/* 生成与下发按钮放在监视舱刀路摘要下方，与孪生入口同一行 */}
          <div className="wb-dispatch-actions" id="wb-dispatch-actions" role="toolbar" aria-label="生成与下发">
            <button
              type="button"
              id="wb-btn-generate"
              className="btn btn-hud-primary"
              onClick={handleGeneratePreview}
              disabled={busy || framingBusy || !canGenerate}
              title={!canGenerate ? "请先在左侧输入要雕刻的文字" : undefined}
            >
              <Cpu size={15} aria-hidden="true" />
              <span>{busy ? "正在解算..." : currentWorkflow ? "重新生成轨迹" : "计算加工轨迹"}</span>
            </button>
            {(() => {
              const isActiveSend = overlayStatus === "running" || overlayStatus === "sending" || overlayStatus === "preparing";
              const canResume = !previewStale && canResumeJob(sendProgress, overlayStatus);
              const canFire = Boolean(currentWorkflow) && !previewStale && !isActiveSend;
              return (
                <button
                  type="button"
                  id="wb-btn-send"
                  className={`btn ${canFire ? "btn-hud-fire" : "btn-ghost"}${canResume ? " btn-resume" : ""}`}
                  onClick={canResume ? handleResumeSend : handleConfirmSend}
                  disabled={busy || framingBusy || !currentWorkflow || previewStale || isActiveSend}
                >
                  <Flame size={16} aria-hidden="true" />
                  <span>
                    {isActiveSend
                      ? "正在雕刻…"
                      : canResume
                      ? formatResumeActionLabel(sendProgress)
                      : previewStale
                      ? "参数已改 · 请先重算"
                      : currentWorkflow
                      ? "开始雕刻"
                      : "请先计算轨迹"}
                  </span>
                </button>
              );
            })()}
            <button
              type="button"
              className="btn btn-ghost twin-jump-btn"
              onClick={() => onNavigate("/twin")}
              id="btn-open-twin"
              title="进入 3D 数字孪生舱（刀路仿真 / 实机遥测）"
            >
              <Box size={14} aria-hidden="true" />
              <span>3D 孪生</span>
            </button>
          </div>

          {/* 辅助操作条（仅在已生成预览时出现，不增加默认空态高度）：先走边框 · 导出 G-code · 温和取消 */}
          {currentWorkflow?.workflow_id && (
            <div className="wb-secondary-actions" id="wb-secondary-actions">
              {Boolean(currentWorkflow?.artifacts?.frame_mm) && (
                <>
                  <button
                    type="button"
                    id="wb-btn-framing"
                    className="btn btn-ghost wb-sec-btn"
                    onClick={handleFraming}
                    disabled={
                      busy
                      || framingBusy
                      || previewStale
                      || overlayStatus === "running"
                      || overlayStatus === "sending"
                    }
                    title="激光头沿刀路外框绕一圈（默认不出光），核对木牌摆放位置"
                  >
                    <Square size={13} aria-hidden="true" />
                    <span>{framingBusy ? "走边框中…" : "先走边框"}</span>
                  </button>
                  {Array.isArray(currentWorkflow?.artifacts?.frame_hull_mm) && currentWorkflow.artifacts.frame_hull_mm.length >= 3 && (
                    <label
                      className="checkbox-row wb-framing-shape"
                      title="勾选后沿图形外轮廓（橡皮筋）走，而不是外接矩形"
                    >
                      <input
                        type="checkbox"
                        id="wb-framing-hull"
                        checked={framingShape === "hull"}
                        onChange={(e) => {
                          const next = e.target.checked ? "hull" : "box";
                          setFramingShape(next);
                          writeFramingShape(next);
                        }}
                      />
                      <span>沿外轮廓</span>
                    </label>
                  )}
                </>
              )}
              <button
                type="button"
                id="wb-btn-export-gcode"
                className="btn btn-ghost wb-sec-btn"
                onClick={handleExportGcode}
                disabled={!gcodeText}
                title="将当前生成的 G-code 刀路保存到本地"
              >
                <Download size={13} aria-hidden="true" />
                <span>导出 G-code</span>
              </button>
              {(overlayStatus === "running" || overlayStatus === "sending") && (
                <button
                  type="button"
                  id="wb-btn-cancel"
                  className="btn btn-ghost wb-sec-btn is-warn"
                  onClick={handleCancelJob}
                  title="温和停止后续刀路下发并记录已完成行数，停稳后可断点续雕"
                >
                  <PauseCircle size={13} aria-hidden="true" />
                  <span>暂停取消（记断点）</span>
                </button>
              )}
            </div>
          )}

          {/* 大号醒目急停安全制动按钮 */}
          <button
            type="button"
            className="btn-estop-large"
            onClick={handleEStop}
            title="发送 0x18 软件复位；紧急情况请按机器实体急停"
            id="btn-estop-trigger"
          >
            <AlertTriangle size={16} />
            <span>紧急停止（软件复位）</span>
          </button>

          {/* 雕刻下发进度 */}
          <div className="streaming-progress-group">
            <div className="streaming-progress-header">
              <span>雕刻下发进度</span>
              <span className="wb-text-6">
                {sendProgress && sendProgress.total > 0
                  ? `${Math.min(100, Math.round((sendProgress.sent / sendProgress.total) * 100))}%`
                  : "0%"}
              </span>
            </div>

            <div className="streaming-progress-track">
              <div
                className={`streaming-progress-bar ${
                  overlayStatus === "running" || overlayStatus === "sending" ? "active" : ""
                }`}
                style={{
                  width: `${
                    sendProgress && sendProgress.total > 0
                      ? Math.min(100, Math.round((sendProgress.sent / sendProgress.total) * 100))
                      : 0
                  }%`,
                }}
              />
            </div>

            <div className="streaming-progress-footer">
              <span>
                {sendProgress && sendProgress.total > 0
                  ? `${sendProgress.sent.toLocaleString()} / ${sendProgress.total.toLocaleString()} 行`
                  : "0 / 0 行"}
              </span>
              <span id="wb-progress-status">
                {workbenchStatusLabel(overlayStatus, sendProgress?.phase, Boolean(currentWorkflow?.workflow_id))}
              </span>
            </div>
            {sendWarnings.length > 0 && (
              <div className="wb-send-warnings" id="wb-send-warnings" role="status">
                {sendWarnings.map((warning, idx) => (
                  <div key={`${idx}-${warning}`} className="wb-stale-notice">
                    {warning}
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
    </WbCard>
  );
}
