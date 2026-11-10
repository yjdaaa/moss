import React, { useCallback, useEffect, useRef, useState } from "react";
import { Sparkles } from "lucide-react";
import { fetchPreviewBlobUrl, getJson, postJson } from "../api/client.js";
import { hasDesktopSession } from "../authSession.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { onEmergencyStop } from "../estopBus.js";
import { extractWorkflowInfo, TERMINAL_STATUSES } from "../state/statusNormalize.js";
import { SITE_CHECK_TEXT, workflowDetails } from "../approvalState.js";
import {
  AI_PREVIEW_LOOKBACK_SECONDS,
  AI_PREVIEW_POLL_MS,
  aiOriginLabel,
  aiPreviewNotes,
  aiWorkflowStarted,
  canStartAiPreview,
  connectionView,
  framingRequest,
  originNote,
  pendingAiPreviews,
  savedConnectionForm,
  strictHintText,
} from "../aiPreviewState.js";
import { createPollRunner } from "../pollGuard.js";
import { SendOverlay } from "./SendOverlay.jsx";
import { voiceApi } from "../voiceApi.js";
import { applyJobPoll, markStopRequested, jobSafetyNotice, startJobPolling } from "../approvalJobMonitor.js";

/**
 * AI 生成任务的桌面预览窗（2026-11-06 用户决定）：语音小智、小智接入点或网关（打字的 AI 客户端）
 * 生成雕刻任务后，在任何页面弹出刀路预览，用户勾选现场确认后点“同意开刻”才发送。
 * 严格模式关闭时用户也可以对小智口头确认；AI 开始发送后窗口切到发送进度（可取消后续发送 / 急停）。
 * 严格模式下 AI 口头确认只会生成审批，此时由全局审批卡处理，本窗口不重复弹出。
 * 只在桌面端（有桌面会话）工作。
 */
export function AiPreviewCenter() {
  const [pending, setPending] = useState([]);
  const [shown, setShown] = useState(null);
  const [hidden, setHidden] = useState(false);
  const [checked, setChecked] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");
  const [framingStatus, setFramingStatus] = useState("");
  const [previewUrl, setPreviewUrl] = useState("");
  const [strict, setStrict] = useState(null);
  const [connection, setConnection] = useState(() => connectionView(savedConnectionForm()));
  const [job, setJob] = useState(null);
  const dismissedRef = useRef(new Set());
  const watchedRef = useRef(new Map());
  const sinceRef = useRef(Math.floor(Date.now() / 1000) - AI_PREVIEW_LOOKBACK_SECONDS);
  const jobRef = useRef(null);
  jobRef.current = job;
  const enabled = hasDesktopSession();

  // 轮询最近任务：新的 AI 预览弹出；已弹出的任务被 AI 口头确认发送后转为发送进度
  useEffect(() => {
    if (!enabled) return undefined;
    let cancelled = false;
    const sync = createPollRunner(async (isCurrent) => {
      const [recent, approvals] = await Promise.all([
        postJson("/api/workflow", { action: "recent" }),
        getJson("/v1/approvals?status=pending"),
      ]);
      if (cancelled || !isCurrent() || !(recent.ok && recent.data?.success)) return;
      const workflows = Array.isArray(recent.data.result?.workflows) ? recent.data.result.workflows : [];
      const approvalTargets = new Set(
        (approvals.ok && approvals.data?.success ? approvals.data.result?.approvals || [] : [])
          .map((item) => item?.target_record)
          .filter(Boolean),
      );
      // 进入严格模式审批的任务交给审批卡（含之后的发送进度），本窗口不再跟踪，避免两个进度窗
      approvalTargets.forEach((id) => watchedRef.current.delete(id));
      for (const record of workflows) {
        if (!watchedRef.current.has(record?.workflow_id) || jobRef.current) continue;
        if (aiWorkflowStarted(record)) {
          const info = extractWorkflowInfo(record);
          watchedRef.current.delete(record.workflow_id);
          setJob({ kind: "workflow", id: record.workflow_id, status: info.status || "sending", progress: null });
        } else if (record.status !== "preview_ready") {
          watchedRef.current.delete(record.workflow_id);
        }
      }
      const list = pendingAiPreviews(workflows, { since: sinceRef.current, dismissed: dismissedRef.current, approvalTargets });
      list.forEach((record) => watchedRef.current.set(record.workflow_id, true));
      setPending((previous) => {
        if (list.length === 0 && previous.length === 0) return previous;
        if (list.some((record) => !previous.some((item) => item.workflow_id === record.workflow_id))) setHidden(false);
        if (
          list.length === previous.length &&
          list.every((record, idx) => record.workflow_id === previous[idx]?.workflow_id && record.status === previous[idx]?.status)
        ) {
          return previous;
        }
        return list;
      });
    });
    void sync();
    const timer = setInterval(() => {
      if (!document.hidden) void sync();
    }, AI_PREVIEW_POLL_MS);
    const onVisibility = () => {
      if (!document.hidden) void sync();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      sync.invalidate();
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [enabled]);

  // 当前显示最新的一条；切换时重新读取设备目标与严格模式（失败原因显示期间保持当前任务）
  useEffect(() => {
    if (failure && pending.some((item) => item.workflow_id === shown?.workflow_id)) return;
    const next = pending[0] || null;
    if (next?.workflow_id === shown?.workflow_id) {
      if (next) setShown(next);
      return;
    }
    setShown(next);
    setChecked(false);
    setFramingStatus("");
    if (next) {
      setConnection(connectionView(savedConnectionForm()));
      voiceApi
        .status()
        .then((status) => setStrict(typeof status?.strict?.enabled === "boolean" ? status.strict.enabled : null))
        .catch(() => setStrict(null));
    }
  }, [pending, shown?.workflow_id, failure]);

  // 刀路预览图
  useEffect(() => {
    let cancelled = false;
    let blobUrl = "";
    setPreviewUrl("");
    const path = shown ? workflowDetails(shown).previewPath : null;
    if (path) {
      (async () => {
        blobUrl = await fetchPreviewBlobUrl(path);
        if (cancelled) {
          if (blobUrl) URL.revokeObjectURL(blobUrl);
          return;
        }
        setPreviewUrl(blobUrl);
      })();
    }
    return () => {
      cancelled = true;
      if (blobUrl) URL.revokeObjectURL(blobUrl);
    };
  }, [shown?.workflow_id]);

  const forget = useCallback((workflowId) => {
    dismissedRef.current.add(workflowId);
    setFailure("");
    setPending((list) => list.filter((item) => item.workflow_id !== workflowId));
  }, []);

  const handleStart = async () => {
    if (!shown || !canStartAiPreview({ checked, busy, targetReady: connection.ready })) return;
    const record = shown;
    setBusy(true);
    setFailure("");
    const res = await postJson("/api/workflow", {
      action: "confirm_send",
      workflow_id: record.workflow_id,
      confirmed: true,
      ...connection.payload,
    });
    setBusy(false);
    if (!(res.ok && res.data?.success)) {
      setFailure(typeof res.data?.result === "string" && res.data.result ? res.data.result : "没有开始加工，请检查设备连接后重试");
      return;
    }
    watchedRef.current.delete(record.workflow_id);
    forget(record.workflow_id);
    const info = extractWorkflowInfo(res.data.result);
    setJob({ kind: "workflow", id: record.workflow_id, status: info.status || "sending", progress: null });
  };

  // 先走边框（2026-11-06）：激光头从当前位置沿刀路范围绕一圈再回到起点，默认不出光；会移动机器，先确认
  const handleFraming = async () => {
    const request = shown ? framingRequest(shown, connection.payload) : null;
    if (!request || busy) return;
    const ok = await showConfirm(
      `激光头会从当前位置出发，沿 ${Math.round((request.max_x - request.min_x) * 10) / 10} × ${Math.round((request.max_y - request.min_y) * 10) / 10} mm 的范围绕一圈再回到原处，不出光。请确认机器周围没有障碍。`,
      { title: "先走边框", confirmText: "开始走边框", cancelText: "取消" },
    );
    if (!ok) return;
    setBusy(true);
    setFailure("");
    setFramingStatus("正在走边框（不出光）…顶栏急停可随时停止");
    const res = await postJson("/api/device/framing", request);
    setBusy(false);
    if (res.ok && res.data?.success) {
      setFramingStatus("走边框完成：确认范围在木牌上后再开刻");
    } else {
      setFramingStatus("");
      setFailure(typeof res.data?.result === "string" && res.data.result ? `走边框未完成：${res.data.result}` : "走边框未完成");
    }
  };

  const handleDiscard = async () => {
    if (!shown || busy) return;
    const record = shown;
    setBusy(true);
    setFailure("");
    const res = await postJson("/api/workflow", { action: "cancel", workflow_id: record.workflow_id });
    setBusy(false);
    if (!(res.ok && res.data?.success)) {
      setFailure(typeof res.data?.result === "string" && res.data.result ? res.data.result : "取消失败");
      return;
    }
    watchedRef.current.delete(record.workflow_id);
    forget(record.workflow_id);
  };

  // 发送进度轮询（与审批卡同一套：查询失败不等于任务失败）
  useEffect(() => {
    if (!job?.id || TERMINAL_STATUSES.has(job.status)) return undefined;
    const { id, kind } = job;
    return startJobPolling(
      (signal) => postJson("/api/workflow", { action: "status", workflow_id: id }, { signal }),
      (res) => setJob((current) => applyJobPoll(current, id, kind, res)),
    );
  }, [job?.id, job?.kind, job?.status]);

  const handleEmergencyStop = useCallback(async () => {
    const id = jobRef.current?.id;
    if (!id) return;
    const res = await postJson("/api/safe-action", { action: "emergency_stop", confirmed: true, job_id: id });
    const text = typeof res.data?.result === "string" ? res.data.result : "";
    if (res.ok && res.data?.success) {
      setJob((current) => markStopRequested(current, id));
      void showAlert(`${text || "0x18 已发送"}。0x18 不代替控制器上的硬件急停按钮，请确认设备已停止。`, { title: "已发送急停 0x18", tone: "warning" });
    } else {
      void showAlert(`${text || "急停发送失败"}。如设备仍在运动或出光，请立即按控制器上的硬件急停。`, { title: "急停未确认", tone: "error" });
    }
  }, []);

  const handleCancelSend = useCallback(async () => {
    const id = jobRef.current?.id;
    if (!id) return;
    const res = await postJson("/api/workflow", { action: "cancel", workflow_id: id });
    if (res.ok && res.data?.success) {
      setJob((current) => markStopRequested(current, id));
    } else {
      void showAlert(typeof res.data?.result === "string" ? res.data.result : "取消请求失败", { title: "取消未完成", tone: "error" });
    }
  }, []);

  // 顶栏急停在发送覆盖层期间也交给本窗口的任务
  const estopRef = useRef(null);
  estopRef.current = job ? handleEmergencyStop : null;
  useEffect(
    () =>
      onEmergencyStop(() => {
        if (!estopRef.current) return false;
        void estopRef.current();
        return true;
      }),
    [],
  );

  if (!enabled) return null;
  const details = shown ? workflowDetails(shown) : null;
  const notes = shown ? aiPreviewNotes(shown) : [];
  const origin = shown ? originNote(shown) : "";
  const canFrame = Boolean(shown && framingRequest(shown, connection.payload));
  const visible = Boolean(shown) && !hidden && !job;

  return (
    <>
      {visible && (
        <div className="mat-modal-backdrop approval-backdrop approval-center-box">
          <div className="mat-modal approval-card" role="dialog" aria-modal="true" aria-labelledby="ai-preview-title" id="ai-preview-card">
            <div className="mat-modal-head">
              <h3 id="ai-preview-title" className="approval-title">
                <Sparkles size={18} aria-hidden="true" />
                <span>{aiOriginLabel(shown.origin)}生成了雕刻预览</span>
              </h3>
              <span className="approval-tag">AI 生成 · 待你确认</span>
            </div>
            <div className="mat-modal-body">
              {pending.length > 1 && <div className="approval-queue">还有 {pending.length - 1} 个 AI 预览等待处理</div>}
              <div className="approval-grid">
                <div className="approval-preview" aria-label="刀路预览">
                  {previewUrl ? <img src={previewUrl} alt="刀路预览" /> : <span>{details?.previewPath ? "正在读取…" : "无预览图"}</span>}
                </div>
                <dl className="approval-kv">
                  <dt>内容</dt>
                  <dd>{details?.content || "—"}</dd>
                  <dt>材料</dt>
                  <dd>{details?.material || "—"}</dd>
                  <dt>参数</dt>
                  <dd>
                    <span className="mono">
                      {[details?.power && `功率 ${details.power}`, details?.feed, details?.passes].filter(Boolean).join(" · ") || "—"}
                    </span>
                    {typeof details?.powerPercent === "number" && (
                      <span className="approval-ruler" aria-label={`功率 ${Math.round(details.powerPercent)}%`}>
                        <i style={{ width: `${details.powerPercent}%` }} />
                      </span>
                    )}
                  </dd>
                  <dt>尺寸 / 时长</dt>
                  <dd>{[details?.size, details?.duration].filter(Boolean).join(" · ") || "—"}</dd>
                  <dt>目标设备</dt>
                  <dd id="ai-preview-target">{connection.label}</dd>
                </dl>
              </div>
              {origin && <div className="voice-hint ai-preview-origin" id="ai-preview-origin"><b>{origin}</b></div>}
              {notes.length > 0 && <p className="approval-summary" id="ai-preview-notes">{notes.join("；")}</p>}
              {framingStatus && <p className="approval-summary" id="ai-preview-framing-status" aria-live="polite">{framingStatus}</p>}
              <p className="voice-hint" id="ai-preview-strict-hint">{strictHintText(strict)}</p>
              {!connection.ready && (
                <div className="approval-error" role="alert">还没有设备地址：请先在“雕刻工作台”填好设备 IP 或选择串口，再回来点同意。</div>
              )}
              <label className="approval-check">
                <input
                  type="checkbox"
                  id="ai-preview-site-check"
                  checked={checked}
                  disabled={busy}
                  onChange={(event) => setChecked(event.target.checked)}
                />
                <span>{SITE_CHECK_TEXT}</span>
              </label>
              {failure && <div className="approval-error" role="alert" id="ai-preview-error">{failure}</div>}
            </div>
            <div className="mat-modal-foot">
              <button type="button" className="btn btn-ghost" id="ai-preview-later" onClick={() => setHidden(true)} disabled={busy}>
                稍后再看
              </button>
              <button type="button" className="btn btn-ghost" id="ai-preview-frame" onClick={handleFraming} disabled={busy || !canFrame}>
                先走边框
              </button>
              <button type="button" className="btn btn-danger" id="ai-preview-discard" onClick={handleDiscard} disabled={busy}>
                取消这个任务
              </button>
              <button
                type="button"
                className="btn btn-primary"
                id="ai-preview-start"
                onClick={handleStart}
                disabled={!canStartAiPreview({ checked, busy, targetReady: connection.ready })}
              >
                {busy ? "正在处理…" : "同意开刻"}
              </button>
            </div>
          </div>
        </div>
      )}
      <SendOverlay
        visible={Boolean(job)}
        status={job?.status}
        progress={job?.progress}
        badge="AI 任务"
        safetyNotice={jobSafetyNotice(job)}
        keepEmergencyStop={Boolean(job)}
        cancelLabel="取消后续发送"
        onCancel={handleCancelSend}
        onEmergencyStop={handleEmergencyStop}
        onClose={() => setJob(null)}
      />
    </>
  );
}
