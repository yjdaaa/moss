import React, { useCallback, useEffect, useRef, useState } from "react";
import { ChevronDown, Download, OctagonX, Pause, Play, Square } from "lucide-react";
import { apiFetch, DEFAULT_READ_TIMEOUT_MS, getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { exportFile, savedMessage } from "../saveFile.js";
import {
  JOB_LINGER_MS,
  JOB_POLL_MS,
  acknowledgeStopPayload,
  cancelPayload,
  dockPillText,
  estopPayload,
  framingControlNote,
  framingControlPayload,
  jobGcodeUrl,
  jobsView,
} from "../jobProgressState.js";

function gcodeFilenameFromUrl(url, fallback) {
  try {
    const parsed = new URL(url, "http://localhost");
    const raw = parsed.searchParams.get("path") || parsed.pathname;
    const name = raw.split(/[\\/]/).pop();
    return name && name.includes(".") ? name : fallback;
  } catch {
    return fallback;
  }
}

// 激光任务进度浮层（2026-11-02 用户决定）：挂在 App 级，任何页面都能看见正在雕刻的进度；
// 数据只读 `/api/jobs/stop-targets`，停止只走既有的 workflow cancel 与 safe-action 急停通道。
// 不点按钮不发任何写请求；浮层不遮挡顶栏急停。

const MINIMIZED_KEY = "moss_job_dock_minimized";

function readMinimized() {
  try {
    return globalThis.localStorage?.getItem(MINIMIZED_KEY) === "1";
  } catch {
    return false;
  }
}

function writeMinimized(value) {
  try {
    globalThis.localStorage?.setItem(MINIMIZED_KEY, value ? "1" : "0");
  } catch {
    /* 存储不可用时只是不记忆折叠状态 */
  }
}

export function JobProgressDock({ onJobsChange = null } = {}) {
  const [views, setViews] = useState([]);
  const [error, setError] = useState("");
  const [minimized, setMinimized] = useState(readMinimized);
  const [note, setNote] = useState("");
  const [lingering, setLingering] = useState(false);
  const [busy, setBusy] = useState(false);
  const [downloading, setDownloading] = useState(false);
  const alive = useRef(true);
  const lingerTimer = useRef(null);

  useEffect(() => {
    alive.current = true;
    let cancelled = false;
    let timer;
    let pollGen = 0;
    let activeController = null;
    const poll = async () => {
      const myGen = ++pollGen;
      clearTimeout(timer);
      activeController?.abort();
      activeController = typeof AbortController !== "undefined" ? new AbortController() : null;
      const res = await getJson("/api/jobs/stop-targets", {
        signal: activeController?.signal,
        timeoutMs: DEFAULT_READ_TIMEOUT_MS,
      });
      if (cancelled || !alive.current || myGen !== pollGen) return;
      const jobs = res?.data?.result?.jobs;
      if (res?.ok && res?.data?.success && Array.isArray(jobs)) {
        const next = jobsView(jobs);
        setError("");
        setViews((previous) => {
          if (next.length === 0 && previous.length === 0) {
            return previous;
          }
          if (next.length === 0 && previous.length > 0) {
            // 任务从运行列表消失：只说“已结束”，不冒充“雕刻成功”
            setLingering(true);
            clearTimeout(lingerTimer.current);
            lingerTimer.current = setTimeout(() => alive.current && setLingering(false), JOB_LINGER_MS);
            return next;
          }
          if (
            next.length === previous.length &&
            JSON.stringify(next) === JSON.stringify(previous)
          ) {
            return previous;
          }
          return next;
        });
      } else {
        // 读取失败时明确说数据可能过期，不把上一次的百分比当成还在跑
        setError("读取任务进度失败，下面的进度可能已过期");
      }
      if (!cancelled && !document.hidden) timer = setTimeout(poll, JOB_POLL_MS);
    };
    const onVisibility = () => {
      if (!document.hidden && !cancelled) {
        clearTimeout(timer);
        void poll();
      }
    };
    void poll();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      cancelled = true;
      alive.current = false;
      activeController?.abort();
      clearTimeout(timer);
      clearTimeout(lingerTimer.current);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);

  useEffect(() => {
    if (typeof onJobsChange === "function") {
      onJobsChange(views);
    }
  }, [views, onJobsChange]);

  const toggleMinimized = useCallback(() => {
    setMinimized((value) => {
      writeMinimized(!value);
      return !value;
    });
  }, []);

  const cancel = useCallback(async (view) => {
    if (!view.canCancel) {
      await showAlert(view.cancelHint, { title: "无法温和取消", tone: "warning" });
      return;
    }
    const ok = await showConfirm(
      "取消后不再下发后续 G-code，已发位置会被记成断点，可在工作台从断点续雕。",
      { title: "取消这次加工？", tone: "warning", confirmText: "取消加工" },
    );
    if (!ok) return;
    setBusy(true);
    setNote("");
    const res = await postJson("/api/workflow", cancelPayload(view));
    setBusy(false);
    setNote(res?.ok && res?.data?.success ? "已请求取消，正在停止下发" : `取消失败：${res?.data?.result || "请重试"}`);
  }, []);

  // 走边框（2026-11-18）：暂停 / 停止不用确认（只会让机器停下）；继续会让机器再动，先确认
  const controlFraming = useCallback(async (view, action) => {
    if (action === "resume") {
      const ok = await showConfirm("激光头会从停下的地方接着沿边框走，走完回到起点（不出光）。请确认机器周围没有障碍。", {
        title: "继续走边框？",
        tone: "warning",
        confirmText: "继续",
      });
      if (!ok) return;
    }
    setBusy(true);
    setNote("");
    const res = await postJson("/api/safe-action", framingControlPayload(view, action));
    setBusy(false);
    setNote(framingControlNote(action, res));
  }, []);

  const estop = useCallback(async (view) => {
    if (!view.canEstop) {
      await showAlert("这条任务缺少任务编号，请直接用机器上的实体急停。", { title: "无法急停", tone: "error" });
      return;
    }
    const ok = await showConfirm(
      "急停会立刻向设备发实时软复位（0x18）：立即停止，但可能丢步，之后需要重新回零再加工。",
      { title: "急停？", danger: true, confirmText: "急停" },
    );
    if (!ok) return;
    // 急停不进入 busy：取消 / 确认停稳等请求挂起时也必须能立即再发 0x18
    setNote("");
    const res = await postJson("/api/safe-action", estopPayload(view));
    // 请求成功只证明实时指令已写出，不证明物理停机
    setNote(res?.ok && res?.data?.success ? "已发出急停指令，请到设备旁确认已停" : `急停失败：${res?.data?.result || "请用实体急停"}`);
  }, []);

  const acknowledgeStop = useCallback(async (view) => {
    if (!view.jobId) return;
    setBusy(true);
    setNote("");
    const res = await postJson("/api/jobs/acknowledge-stop", acknowledgeStopPayload(view));
    setBusy(false);
    if (res?.ok && res?.data?.success) {
      setViews((prev) => prev.filter((item) => item.jobId !== view.jobId));
      setNote("已确认设备停稳并移除未知状态卡片");
    } else {
      setNote(`确认失败：${res?.data?.result || "请重试"}`);
    }
  }, []);

  const download = useCallback(async (view) => {
    if (!view.workflowId) {
      await showAlert("这条任务没有关联的刀路记录，无法下载 G-code。", { title: "无法下载", tone: "warning" });
      return;
    }
    setDownloading(true);
    setNote("");
    try {
      const res = await postJson("/api/workflow", { action: "status", workflow_id: view.workflowId });
      const url = jobGcodeUrl(res?.data?.result);
      if (!url) {
        await showAlert("这个任务没有可下载的 G-code 文件（例如直接执行的单条指令）。", { title: "没有文件", tone: "info" });
        return;
      }
      const response = await apiFetch(url);
      if (!response.ok) {
        throw new Error(`读取文件失败（HTTP ${response.status}）`);
      }
      const blob = await response.blob();
      const filename = gcodeFilenameFromUrl(url, `${view.workflowId}.gcode`);
      const saved = await exportFile(blob, filename, "G-code ");
      if (alive.current) setNote(savedMessage("G-code ", saved));
    } catch (err) {
      const message = `下载 G-code 失败：${err?.message || err}`;
      if (alive.current) setNote(message);
      await showAlert(message, { title: "下载失败", tone: "error" });
    } finally {
      if (alive.current) setDownloading(false);
    }
  }, []);

  const visible = views.length > 0;
  if (!visible && !lingering) return null;

  const pillText = visible ? dockPillText(views) : "已结束";
  const single = views.length === 1 ? views[0] : null;

  if (minimized) {
    return (
      <div className="job-dock job-dock--pill" id="job-progress-pill">
        <button type="button" className="job-dock-pill-main" onClick={toggleMinimized} id="job-progress-pill-open" title="展开雕刻任务面板">
          <span className="job-dock-pill-dot" aria-hidden="true" />
          <span className="job-dock-pill-text" id="job-progress-pill-text">{pillText}</span>
        </button>
        {single && single.canEstop && (
          <button
            type="button"
            className="job-dock-pill-estop"
            onClick={() => estop(single)}
            id="job-progress-pill-estop"
            title="急停（0x18 实时软复位）"
          >
            <OctagonX size={14} />
          </button>
        )}
      </div>
    );
  }

  return (
    <div className="job-dock" id="job-progress-dock" role="region" aria-label="激光雕刻任务">
      <div className="job-dock-h">
        <span className="job-dock-title">
          激光任务{visible ? `（${views.length}）` : ""}
        </span>
        <button type="button" className="job-dock-icon" onClick={toggleMinimized} id="job-progress-minimize" title="收成小胶囊">
          <ChevronDown size={14} />
        </button>
      </div>

      {!visible && <p className="job-dock-note">任务已从运行列表消失：可能已完成、已取消或已失败，请到工作台或记录里查看结果。</p>}
      {error && <p className="job-dock-warn">{error}</p>}

      {views.map((view, index) => (
        <div className="job-dock-item" key={`${view.jobId || view.workflowId}-${index}`} id={`job-progress-item-${index}`}>
          <div className="job-dock-item-h">
            <span className="job-dock-percent" id={`job-progress-percent-${index}`}>{view.percentText}</span>
            <span className="job-dock-phase">{view.phaseText}</span>
          </div>
          <div className="job-dock-bar" aria-hidden="true">
            <div className="job-dock-bar-fill" style={{ width: `${view.unknown ? 0 : view.percent}%` }} />
          </div>
          <div className="job-dock-meta">
            <span>{view.linesText}</span>
            <span>{view.elapsedText}</span>
            {view.remainingText && <span>{view.remainingText}</span>}
          </div>
          <div className="job-dock-meta">
            {view.target && <span className="mono">{view.target}</span>}
            {view.mode && <span>{view.mode}</span>}
            <span className="mono" title="任务标识">{view.workflowId || view.jobId}</span>
          </div>
          <div className="job-dock-actions">
            {view.framing ? (
              view.canControlFraming && (
                <>
                  {view.paused ? (
                    <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => controlFraming(view, "resume")} id={`job-progress-resume-${index}`}>
                      <Play size={13} /> 继续
                    </button>
                  ) : (
                    <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => controlFraming(view, "pause")} id={`job-progress-pause-${index}`}>
                      <Pause size={13} /> 暂停
                    </button>
                  )}
                  <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => controlFraming(view, "stop_framing")} id={`job-progress-stop-framing-${index}`}>
                    <Square size={13} /> 停止走边框
                  </button>
                </>
              )
            ) : (
              <>
                <button type="button" className="btn btn-ghost" disabled={busy || downloading} onClick={() => download(view)} id={`job-progress-gcode-${index}`}>
                  <Download size={13} /> G-code
                </button>
                <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => cancel(view)} id={`job-progress-cancel-${index}`}>
                  <Square size={13} /> 取消（记断点）
                </button>
              </>
            )}
            {(view.canAcknowledgeStop ?? (view.unknown && Boolean(view.jobId))) && (
              <button
                type="button"
                className="btn btn-ghost"
                disabled={busy}
                onClick={() => acknowledgeStop(view)}
                id={`job-progress-ack-${index}`}
              >
                已确认停稳
              </button>
            )}
            <button type="button" className="btn btn-danger" onClick={() => estop(view)} id={`job-progress-estop-${index}`}>
              <OctagonX size={13} /> 急停
            </button>
          </div>
          {view.cancelHint && <p className="job-dock-note">{view.cancelHint}</p>}
        </div>
      ))}

      {note && <p className="job-dock-note" role="status" id="job-progress-note">{note}</p>}
    </div>
  );
}
