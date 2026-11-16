import React from "react";
import { Activity, AlertTriangle, Eye, FileJson, RefreshCw, Save, Send, Square, UploadCloud } from "lucide-react";
import { writeFramingShape } from "../../aiPreviewState.js";
import { drawTargetMissingText } from "../../drawConnection.js";
import { REGISTRATION_DRAW_SESSION_KEY, drawOriginNote } from "../../registrationState.js";
import { FeedbackPanel } from "../FeedbackPanel.jsx";
import { FoldHeader } from "../FoldHeader.jsx";
import RegistrationPanel from "../RegistrationPanel.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";
import { JobSummaryCard } from "./JobSummaryCard.jsx";
import { PreviewImage } from "./PreviewImage.jsx";

/** 创意画板 ③ 预览与发送卡：生成预览、确认发送、走边框、两点对位、作业摘要、加工反馈、预览图与下载。原样搬自 pages/DrawPage.jsx（2026-11-16 按职责拆分） */
export function DrawSendCard({
  form,
  busy,
  excalidrawAPI,
  workflowId,
  result,
  web,
  rawText,
  jobSummary,
  previewStale,
  hostReady,
  canActOnWorkflow,
  canConfirmSend,
  summaryAllowsSend,
  framingLight,
  setFramingLight,
  framingShape,
  setFramingShape,
  motionPayload,
  setStatus,
  generatePreview,
  openSendConfirmation,
  runFraming,
  workflowAction,
  adoptRegistered,
  adoptFeedbackWorkflow,
  downloadFromUrl,
}) {
  return (
    <WbCard id="draw-send" className="hud-card ob-card control-panel result-panel" label="预览与发送">
      <FoldHeader id="draw-send" title="预览与发送" k="PREVIEW · SEND" n={3} />
      <div className="action-stack">
        <button type="button" className="primary-button" onClick={generatePreview} disabled={busy || !excalidrawAPI}>
          <UploadCloud size={18} />
          {busy ? "正在解算..." : workflowId ? "重新生成预览" : "生成预览"}
        </button>
        <button type="button" id="draw-confirm-send-btn" className="danger-button" onClick={openSendConfirmation} disabled={busy || !canConfirmSend}>
          <Send size={18} />
          {previewStale ? "画布或参数已改 · 请先重算" : "确认发送"}
        </button>
        <button
          type="button"
          id="draw-framing-btn"
          className="secondary-button draw-btn"
          onClick={runFraming}
          disabled={busy || previewStale || !hostReady || !jobSummary?.actual_width_mm}
          title="从激光头当前位置（左下角起点）沿作品外框绕一圈对位；默认不出光"
        >
          <Eye size={18} />
          先走边框
        </button>
        <label className="checkbox-row draw-label" title="勾选后走边框时以 S1（约 0.1% 功率）出极弱的光，便于在材料上看到边框">
          <input type="checkbox" checked={framingLight} onChange={(event) => setFramingLight(event.target.checked)} />
          走边框时弱光指示（默认不出光）
        </label>
        <label className="checkbox-row draw-label" title="勾选后沿图形外轮廓（橡皮筋）走，而不是外接矩形">
          <input
            type="checkbox"
            id="draw-framing-hull"
            checked={framingShape === "hull"}
            onChange={(event) => {
              const next = event.target.checked ? "hull" : "box";
              setFramingShape(next);
              writeFramingShape(next);
            }}
          />
          走边框沿图形外轮廓（橡皮筋）
        </label>
        {previewStale && (
          <div className="wb-stale-notice" id="draw-stale-notice" role="status">
            画布图形或左侧参数已改动，当前预览仍是上一版旧图；请先点「重新生成预览」后再走边框或发送。
          </div>
        )}
        {canActOnWorkflow && !previewStale && (
          <div className="wb-origin-note" id="draw-origin-note">
            {drawOriginNote(result?.result)}
          </div>
        )}
        {canActOnWorkflow && !hostReady && <div className="inline-hint">{drawTargetMissingText(form)}后再发送</div>}
        {canActOnWorkflow && !previewStale && hostReady && !summaryAllowsSend && (
          <div className="inline-hint">当前不可发送：请检查精确厚度参数与服务端确认权限</div>
        )}
        <div className="split-actions">
          <button type="button" onClick={() => workflowAction("status")} disabled={busy || !canActOnWorkflow}>
            <Activity size={17} />
            刷新状态
          </button>
          <button type="button" onClick={() => workflowAction("cancel")} disabled={busy || !canActOnWorkflow}>
            <Square size={17} />
            取消任务
          </button>
        </div>
      </div>

      {/* 两点对位（2026-11-16，画板入口）：默认收起；对位结果是新的画板任务，发送仍走本页确认框 */}
      {canActOnWorkflow && (
        <RegistrationPanel
          workflow={result?.result}
          connectionForm={form}
          machineMotion={motionPayload.machine_motion}
          disabled={
            busy
            || previewStale
            || ["sending", "running", "preparing", "awaiting_completion"].includes(result?.result?.status)
          }
          onLog={(_tag, text) => setStatus(text)}
          onRegistered={adoptRegistered}
          idPrefix="draw-reg"
          rootId="draw-registration"
          sessionKey={REGISTRATION_DRAW_SESSION_KEY}
        />
      )}

      <div className="workflow-chip">
        <RefreshCw size={16} />
        <span>
          {workflowId
            ? (previewStale ? `预览已过期 · ${workflowId}` : `当前任务 · ${workflowId}`)
            : "等待生成预览"}
        </span>
      </div>

      <JobSummaryCard summary={jobSummary} />
      <FeedbackPanel workflowId={workflowId} origin={result?.result?.feedback_origin}
        disabled={busy || ["sending", "running", "preparing", "awaiting_completion"].includes(result?.result?.status)}
        onAdopt={adoptFeedbackWorkflow} />
      <PreviewImage title="素材预览" url={web.material_preview_url || web.uploaded_image_url} stale={previewStale} />
      <PreviewImage title="路径预览" url={web.gcode_preview_url || web.image_preview_url} stale={previewStale} />
      <div className="link-list">
        {web.gcode_download_url && (
          <button
            type="button"
            className="secondary-button"
            onClick={() => downloadFromUrl(web.gcode_download_url, "laser-job.gcode", "G-code ")}
          >
            <Save size={16} />
            下载 G-code
          </button>
        )}
        {web.scene_download_url && (
          <button
            type="button"
            className="secondary-button"
            onClick={() => downloadFromUrl(web.scene_download_url, "excalidraw-laser-lab.excalidraw.json", "场景 JSON ")}
          >
            <FileJson size={16} />
            下载场景 JSON
          </button>
        )}
      </div>
      <details className="diagnostics">
        <summary className="diagnostics-title">
          <AlertTriangle size={16} />
          诊断输出
        </summary>
        <pre>{rawText || "等待预览结果..."}</pre>
      </details>
    </WbCard>
  );
}
