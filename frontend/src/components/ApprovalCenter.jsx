import React, { useCallback, useEffect, useRef, useState } from "react";
import { ShieldAlert } from "lucide-react";
import { fetchPreviewBlobUrl, getJson, postJson } from "../api/client.js";
import { hasDesktopSession } from "../authSession.js";
import { showAlert } from "../appDialog.js";
import { onEmergencyStop } from "../estopBus.js";
import { extractWorkflowInfo, normalizeWorkflowStatus, TERMINAL_STATUSES } from "../state/statusNormalize.js";
import {
  APPROVAL_POLL_MS,
  approvalCheckText,
  approvalErrorText,
  canApprove,
  clockOffsetSeconds,
  kindLabel,
  newPendingIds,
  orderPending,
  secondsLeft,
  shortDigest,
  sourceLabel,
  targetText,
  workflowDetails,
} from "../approvalState.js";
import { SendOverlay } from "./SendOverlay.jsx";
import { applyJobPoll, markStopRequested, jobSafetyNotice, startJobPolling } from "../approvalJobMonitor.js";

const SOURCE_SHORT = { mcp_bridge: "小智", lan: "局域网设备", gateway: "小智" };

/**
 * 全局审批卡（技术书 §9.5.2，实施清单 P1-09）：挂在 App 根组件，任何页面都会弹出。
 * 只在桌面端（有桌面会话）工作；浏览器与 Android 不轮询、不显示。
 * 批准后加工任务进入发送覆盖层（STAGE_MAP 四步不变），全程可“取消后续发送”与“急停 0x18”。
 */
export function ApprovalCenter({ open, onOpenChange, onPendingCountChange }) {
  const [pending, setPending] = useState([]);
  const [offset, setOffset] = useState(0);
  const [ttl, setTtl] = useState(120);
  const [shown, setShown] = useState(null);
  const [checked, setChecked] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState(null);
  const [details, setDetails] = useState(null);
  const [previewUrl, setPreviewUrl] = useState("");
  const [, setTick] = useState(0);
  const [job, setJob] = useState(null);
  const seenRef = useRef(new Set());
  const dismissedRef = useRef(new Set());
  const jobRef = useRef(null);
  jobRef.current = job;
  const enabled = hasDesktopSession();

  // 每 4 秒同步待审批列表；有新请求时自动弹出（发送覆盖层显示期间不打断）
  useEffect(() => {
    if (!enabled) return undefined;
    let cancelled = false;
    const sync = async () => {
      const res = await getJson("/v1/approvals?status=pending");
      if (cancelled || !(res.ok && res.data?.success)) return;
      const result = res.data.result || {};
      const list = orderPending(result.approvals).filter((item) => !dismissedRef.current.has(item.approval_id));
      setOffset(clockOffsetSeconds(result.server_time));
      if (typeof result.ttl_seconds === "number") setTtl(result.ttl_seconds);
      setPending(list);
      onPendingCountChange?.(list.length);
      const fresh = newPendingIds(seenRef.current, list);
      list.forEach((item) => seenRef.current.add(item.approval_id));
      if (fresh.length > 0 && !jobRef.current) onOpenChange?.(true);
    };
    void sync();
    const timer = setInterval(sync, APPROVAL_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [enabled, onOpenChange, onPendingCountChange]);

  // 选择当前显示的请求：失败原因显示期间保持不变
  useEffect(() => {
    if (failure) return;
    if (shown && pending.some((item) => item.approval_id === shown.approval_id)) return;
    const next = pending[0] || null;
    setShown(next);
    setChecked(false);
    if (!next && open) onOpenChange?.(false);
  }, [pending, shown, failure, open, onOpenChange]);

  // 加工内容详情与刀路预览
  useEffect(() => {
    let cancelled = false;
    let blobUrl = "";
    setDetails(null);
    setPreviewUrl("");
    if (shown?.kind === "workflow_confirm_send") {
      (async () => {
        const res = await postJson("/api/workflow", { action: "status", workflow_id: shown.target_record });
        if (cancelled || !(res.ok && res.data?.success)) return;
        const info = workflowDetails(res.data.result);
        setDetails(info);
        if (info.previewPath) {
          blobUrl = await fetchPreviewBlobUrl(info.previewPath);
          if (cancelled) {
            if (blobUrl) URL.revokeObjectURL(blobUrl);
            return;
          }
          setPreviewUrl(blobUrl);
        }
      })();
    }
    return () => {
      cancelled = true;
      if (blobUrl) URL.revokeObjectURL(blobUrl);
    };
  }, [shown?.approval_id]);

  // 倒计时：每秒刷新；归零时自动关闭卡片（服务端同时把请求记为过期）
  const remaining = shown ? secondsLeft(shown.expires_at, offset) : 0;
  useEffect(() => {
    if (!open || !shown || failure) return undefined;
    const timer = setInterval(() => setTick((value) => value + 1), 1000);
    return () => clearInterval(timer);
  }, [open, shown, failure]);
  useEffect(() => {
    if (!shown || failure || busy || remaining > 0) return;
    dismissedRef.current.add(shown.approval_id);
    setPending((list) => list.filter((item) => item.approval_id !== shown.approval_id));
  }, [remaining, shown, failure, busy]);

  const removeShown = useCallback((approvalId) => {
    dismissedRef.current.add(approvalId);
    setFailure(null);
    setShown(null);
    setPending((list) => {
      const next = list.filter((item) => item.approval_id !== approvalId);
      onPendingCountChange?.(next.length);
      return next;
    });
  }, [onPendingCountChange]);

  const handleApprove = async () => {
    if (!shown || !canApprove({ checked, busy, failed: Boolean(failure), remaining })) return;
    const approval = shown;
    setBusy(true);
    const res = await postJson(`/v1/approvals/${encodeURIComponent(approval.approval_id)}/approve`, {
      digest: approval.digest,
    });
    setBusy(false);
    if (!(res.ok && res.data?.success)) {
      const code = res.data?.detail?.error_code || null;
      const fallback = typeof res.data?.result === "string" ? res.data.result : "";
      setFailure({ code, text: approvalErrorText(code, fallback, approval.kind) });
      return;
    }
    removeShown(approval.approval_id);
    onOpenChange?.(false);
    const result = res.data.result?.result;
    if (approval.kind === "workflow_confirm_send") {
      const info = extractWorkflowInfo(result);
      setJob({ kind: "workflow", id: approval.target_record, status: info.status || "sending", progress: null });
    } else if (approval.kind === "calibration_send") {
      setJob({ kind: "calibration", id: approval.target_record, status: "sending", progress: null });
    } else {
      const message = typeof result === "string" ? result : approval.summary;
      void showAlert(message || "已执行", { title: `已批准：${kindLabel(approval.kind)}`, tone: "success" });
    }
  };

  const handleReject = async () => {
    if (!shown || busy) return;
    const approval = shown;
    setBusy(true);
    const res = await postJson(`/v1/approvals/${encodeURIComponent(approval.approval_id)}/reject`, {});
    setBusy(false);
    if (!(res.ok && res.data?.success)) {
      const code = res.data?.detail?.error_code || null;
      const fallback = typeof res.data?.result === "string" ? res.data.result : "";
      setFailure({ code, text: approvalErrorText(code, fallback, approval.kind) });
      return;
    }
    removeShown(approval.approval_id);
  };

  // 查询失败不等于任务失败；轮询互不重叠，旧请求不能覆盖新任务。
  useEffect(() => {
    if (!job?.id || TERMINAL_STATUSES.has(job.status)) return undefined;
    const { id, kind } = job;
    return startJobPolling(
      (signal) => kind === "calibration"
        ? postJson("/api/material-lab/status", { calibration_id: id }, { signal })
        : postJson("/api/workflow", { action: "status", workflow_id: id }, { signal }),
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
      void showAlert(`${text || "0x18 已发送"}。0x18 不代替控制器上的硬件急停按钮，请确认设备已停止。`, {
        title: "已发送急停 0x18",
        tone: "warning",
      });
    } else {
      void showAlert(`${text || "急停发送失败"}。如设备仍在运动或出光，请立即按控制器上的硬件急停。`, {
        title: "急停未确认",
        tone: "error",
      });
    }
  }, []);

  const handleCancel = useCallback(async () => {
    if (!job) return;
    if (job.kind !== "workflow") {
      await handleEmergencyStop();
      return;
    }
    const res = await postJson("/api/workflow", { action: "cancel", workflow_id: job.id });
    if (res.ok && res.data?.success) {
      setJob((current) => markStopRequested(current, job.id));
    } else {
      void showAlert(typeof res.data?.result === "string" ? res.data.result : "取消请求失败", { title: "取消未完成", tone: "error" });
    }
  }, [job, handleEmergencyStop]);

  // 顶栏急停在审批执行期间也有效（发送覆盖层盖住顶栏时用覆盖层里的按钮）
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

  const approvalOpen = open && shown;
  const percent = shown && ttl > 0 ? Math.max(0, Math.min(100, (remaining / ttl) * 100)) : 0;
  const isWorkflow = shown?.kind === "workflow_confirm_send";

  return (
    <>
      {approvalOpen && (
        <div className="mat-modal-backdrop approval-backdrop approval-center-box">
          <div className="mat-modal approval-card" role="dialog" aria-modal="true" aria-labelledby="approval-title" id="approval-card">
            <div className="mat-modal-head">
              <h3 id="approval-title" className="approval-title">
                <ShieldAlert size={18} aria-hidden="true" />
                <span>{SOURCE_SHORT[shown.source] || "远程"}请求{kindLabel(shown.kind)}</span>
              </h3>
              <span className="approval-tag">{shown.kind === "external_tool" ? "第三方工具 · 本次批准" : "R3 · 运动与出光"}</span>
            </div>
            <div className="mat-modal-body">
              {pending.length > 1 && (
                <div className="approval-queue">还有 {pending.length - 1} 个请求等待处理</div>
              )}
              <div className="approval-grid">
                <div className="approval-preview" aria-label="刀路预览">
                  {previewUrl ? (
                    <img src={previewUrl} alt="刀路预览" />
                  ) : (
                    <span>{isWorkflow ? (details ? "无预览图" : "正在读取…") : kindLabel(shown.kind)}</span>
                  )}
                </div>
                <dl className="approval-kv">
                  <dt>内容</dt>
                  <dd>{details?.content || shown.summary}</dd>
                  {isWorkflow && (
                    <>
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
                    </>
                  )}
                  <dt>目标设备</dt>
                  <dd>{targetText(shown.target)}</dd>
                  <dt>来源</dt>
                  <dd>{sourceLabel(shown.source, shown.session_id)}</dd>
                  <dt>摘要</dt>
                  <dd className="mono" title={shown.digest}>sha256 {shortDigest(shown.digest)}</dd>
                </dl>
              </div>
              {shown.kind === "external_tool" && <div className="voice-hint"><b>参数预览（敏感字段已隐藏，长内容可能截断）</b><pre className="approval-center-code">{JSON.stringify(shown.external?.arguments ?? {}, null, 2)}</pre><span>这是第三方操作，不代表 MOSS 验证过该服务的安全性。不确定时请选择拒绝。</span></div>}
              <p className="approval-summary">{shown.summary}</p>
              {!failure && (
                <div className="approval-timer" aria-live="polite">
                  <span className="mono">{remaining} s</span>
                  <div className="approval-bar"><i style={{ width: `${percent}%` }} /></div>
                  <span>过期后需重新发起</span>
                </div>
              )}
              <label className="approval-check">
                <input
                  type="checkbox"
                  id="approval-site-check"
                  checked={checked}
                  disabled={Boolean(failure) || busy}
                  onChange={(event) => setChecked(event.target.checked)}
                />
                <span>{approvalCheckText(shown.kind)}</span>
              </label>
              {failure && (
                <div className="approval-error" role="alert" id="approval-error">{failure.text}</div>
              )}
            </div>
            <div className="mat-modal-foot">
              {failure ? (
                <button type="button" className="btn btn-ghost" id="approval-close" onClick={() => removeShown(shown.approval_id)}>
                  关闭
                </button>
              ) : (
                <>
                  <button type="button" className="btn btn-ghost" id="approval-later" onClick={() => onOpenChange?.(false)} disabled={busy}>
                    稍后处理
                  </button>
                  <button type="button" className="btn btn-danger" id="approval-reject" onClick={handleReject} disabled={busy}>
                    拒绝
                  </button>
                  <button
                    type="button"
                    className="btn btn-primary"
                    id="approval-approve"
                    onClick={handleApprove}
                    disabled={!canApprove({ checked, busy, failed: Boolean(failure), remaining })}
                  >
                    {busy ? "正在处理…" : shown.kind === "external_tool" ? "批准本次调用" : "批准并开始"}
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      )}
      <SendOverlay
        visible={Boolean(job)}
        status={job?.status}
        progress={job?.progress}
        badge="桌面审批"
        safetyNotice={jobSafetyNotice(job)}
        keepEmergencyStop={Boolean(job)}
        cancelLabel={job?.kind === "workflow" ? "取消后续发送" : "停止"}
        onCancel={handleCancel}
        onEmergencyStop={handleEmergencyStop}
        onClose={() => setJob(null)}
      />
    </>
  );
}
