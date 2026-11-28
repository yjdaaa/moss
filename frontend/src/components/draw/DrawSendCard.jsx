import React from "react";
import { Eye, Flame, Save, Send, Square, UploadCloud } from "lucide-react";
import { drawTargetMissingText } from "../../drawConnection.js";
import { canResumeJob, formatResumeActionLabel, sendProgressFromResult } from "../../estopResumeState.js";
import { REGISTRATION_DRAW_SESSION_KEY, drawOriginNote } from "../../registrationState.js";
import { FeedbackPanel } from "../FeedbackPanel.jsx";
import { FoldHeader } from "../FoldHeader.jsx";
import { OriginBadge } from "../OriginBadge.jsx";
import RegistrationPanel from "../RegistrationPanel.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";
import { JobSummaryCard } from "./JobSummaryCard.jsx";
import { PreviewImage } from "./PreviewImage.jsx";

const ACTIVE_STATUSES = ["sending", "running", "preparing", "awaiting_completion"];

/**
 * 判断画板当前任务是否应显示「取消任务」按钮：
 * - 状态为运行态（`sending` / `running` / `preparing` / `awaiting_completion`）或明确 `unknown`；
 * - 或发送/续雕请求返回 HTTP 0 / 轮询失联（`jobStatusUnknown === true`）：状态未知时保留取消入口；
 * - 或后端活动任务表 `activeJobs`（含严格模式审批刚放行启动的任务）已包含当前 `workflowId`。
 */
export function isDrawJobActive({ result = null, workflowId = "", jobStatusUnknown = false, activeJobs = [] } = {}) {
  const status = result?.result?.status;
  if (ACTIVE_STATUSES.includes(status) || status === "unknown") return true;
  if (jobStatusUnknown && Boolean(workflowId)) return true;
  if (
    workflowId
    && Array.isArray(activeJobs)
    && activeJobs.some((job) => (job?.workflowId || job?.workflow_id || job?.jobId || job?.job_id) === workflowId)
  ) {
    return true;
  }
  return false;
}

/**
 * 当前最该看的一条提示（r49 布局）：旧预览 > 没设连接目标 > 不可发送；都没有时为 null。
 * 开刻前激光头放哪另起一行（`#draw-origin-note`），不和警告挤在一起。
 */
export function drawSendNotice({ canActOnWorkflow, previewStale, hostReady, summaryAllowsSend, form }) {
  if (previewStale) {
    return {
      id: "draw-stale-notice",
      tone: "warn",
      text: "画布或左侧参数已改动，下面还是上一版旧刀路；请先点「重新生成预览」再走边框或发送。",
    };
  }
  if (!canActOnWorkflow) return null;
  if (!hostReady) {
    return { id: "draw-target-notice", tone: "warn", text: `${drawTargetMissingText(form)}后再发送` };
  }
  if (!summaryAllowsSend) {
    return { id: "draw-send-blocked", tone: "warn", text: "当前不可发送：请检查精确厚度参数与服务端确认权限" };
  }
  return null;
}

/**
 * 创意画板 ③ 预览与发送卡（r49 重新布局，2026-11-17 用户要求）：
 * ① 主操作“生成预览 / 确认发送”并排；② 只显示一条最该看的提示与开刻前激光头位置；
 * ③ 深色底刀路预览（与画板一致）；④ 作业摘要；⑤ 有任务时一行次要操作：先走边框、沿外轮廓、下载 G-code、取消任务；
 * ⑥ 两点对位与刻完反馈默认收起。
 */
export function DrawSendCard({
  form,
  busy,
  excalidrawAPI,
  workflowId,
  result,
  web,
  jobSummary,
  previewStale,
  hostReady,
  canActOnWorkflow,
  canConfirmSend,
  summaryAllowsSend,
  framingShape,
  setFramingShape,
  motionPayload,
  setStatus,
  generatePreview,
  openSendConfirmation,
  resumeWorkflow,
  runFraming,
  cancelWorkflow,
  adoptRegistered,
  adoptFeedbackWorkflow,
  downloadFromUrl,
  jobStatusUnknown = false,
  activeJobs = [],
}) {
  const status = result?.result?.status;
  const jobActive = isDrawJobActive({ result, workflowId, jobStatusUnknown, activeJobs });
  const sendProgress = sendProgressFromResult(result?.result?.send_result);
  const canResume = Boolean(
    canActOnWorkflow && !previewStale && hostReady && !jobActive && canResumeJob(sendProgress, status),
  );
  const notice = drawSendNotice({ canActOnWorkflow, previewStale, hostReady, summaryAllowsSend, form });
  const pathUrl = web.gcode_preview_url || web.image_preview_url;
  return (
    <WbCard id="draw-send" className="hud-card ob-card control-panel result-panel draw-send-card" label="预览与发送">
      <FoldHeader id="draw-send" title="预览与发送" n={2} />

      <div className="draw-send-primary">
        <button type="button" className="primary-button" onClick={generatePreview} disabled={busy || !excalidrawAPI}>
          <UploadCloud size={18} />
          {busy ? "正在解算..." : workflowId ? "重新生成预览" : "生成预览"}
        </button>
        <button
          type="button"
          id="draw-confirm-send-btn"
          className="danger-button"
          onClick={openSendConfirmation}
          disabled={busy || !canConfirmSend}
        >
          <Send size={18} />
          {previewStale ? "参数已改 · 请先重算" : "确认发送"}
        </button>
      </div>

      {notice ? (
        <div className={`draw-send-notice is-${notice.tone}`} id={notice.id} role="status">
          {notice.text}
        </div>
      ) : null}
      {canActOnWorkflow && !previewStale ? (
        <div className="draw-send-origin wb-origin-inline" id="draw-origin-note">
          <OriginBadge origin={result?.result?.registration ? "point2" : "lower_left"} />
          <span>{drawOriginNote(result?.result)}</span>
        </div>
      ) : null}

      <PreviewImage
        url={pathUrl}
        stale={previewStale}
        tone="dark"
        invert={result?.result?.artifacts?.mode === "raster"}
        empty="生成预览后在这里看实际要刻的刀路"
      />

      <JobSummaryCard summary={jobSummary} compact />

      {canActOnWorkflow ? (
        <div className="draw-send-secondary" role="toolbar" aria-label="预览后的操作">
          <button
            type="button"
            id="draw-framing-btn"
            className="btn btn-ghost wb-sec-btn"
            onClick={runFraming}
            disabled={busy || previewStale || !hostReady || !jobSummary?.actual_width_mm}
            title="从激光头当前位置（左下角起点）沿作品外框绕一圈对位，不出光"
          >
            <Eye size={14} aria-hidden="true" />
            <span>先走边框</span>
          </button>
          {Array.isArray(result?.result?.artifacts?.frame_hull_mm) && result.result.artifacts.frame_hull_mm.length >= 3 ? (
            <label className="checkbox-row wb-framing-shape" title="沿图形外轮廓（橡皮筋）走，而不是外接矩形">
              <input
                type="checkbox"
                id="draw-framing-hull"
                checked={framingShape === "hull"}
                onChange={(event) => {
                  const next = event.target.checked ? "hull" : "box";
                  setFramingShape(next);
                }}
              />
              <span>沿外轮廓</span>
            </label>
          ) : null}
          {web.gcode_download_url ? (
            <button
              type="button"
              className="btn btn-ghost wb-sec-btn"
              onClick={() => downloadFromUrl(web.gcode_download_url, "laser-job.gcode", "G-code ")}
            >
              <Save size={14} aria-hidden="true" />
              <span>下载 G-code</span>
            </button>
          ) : null}
          {canResume && resumeWorkflow ? (
            <button
              type="button"
              id="draw-resume-send-btn"
              className="btn btn-hud-fire btn-resume wb-sec-btn"
              onClick={resumeWorkflow}
              disabled={busy}
              title="从上次安全停机记录的断点行继续流式下发剩余 G-code"
            >
              <Flame size={14} aria-hidden="true" />
              <span>{formatResumeActionLabel(sendProgress)}</span>
            </button>
          ) : null}
          {jobActive ? (
            <button
              type="button"
              id="draw-cancel-job-btn"
              className="btn btn-ghost wb-sec-btn is-warn"
              onClick={cancelWorkflow}
              disabled={busy}
              title="停止后续发送并请求安全停机；紧急情况请按顶栏急停"
            >
              <Square size={14} aria-hidden="true" />
              <span>取消任务</span>
            </button>
          ) : null}
        </div>
      ) : null}

      {/* 两点对位（默认收起）：对位结果是新的画板任务，发送仍走本页确认框 */}
      {canActOnWorkflow ? (
        <RegistrationPanel
          workflow={result?.result}
          connectionForm={form}
          machineMotion={motionPayload.machine_motion}
          disabled={busy || previewStale || jobActive}
          onLog={(_tag, text) => setStatus(text)}
          onRegistered={adoptRegistered}
          idPrefix="draw-reg"
          rootId="draw-registration"
          sessionKey={REGISTRATION_DRAW_SESSION_KEY}
        />
      ) : null}
      {canActOnWorkflow ? (
        <FeedbackPanel
          workflowId={workflowId}
          origin={result?.result?.feedback_origin}
          disabled={busy || previewStale || jobActive}
          onAdopt={adoptFeedbackWorkflow}
        />
      ) : null}
    </WbCard>
  );
}
