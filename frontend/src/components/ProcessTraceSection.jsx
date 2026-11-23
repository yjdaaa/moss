import React, { useCallback, useEffect, useRef, useState } from "react";
import { FolderOpen, FolderSearch, History, RotateCcw, Trash2 } from "lucide-react";
import { FoldHeader } from "./FoldHeader.jsx";
import { postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { isRemoteMode } from "../authSession.js";
import { shouldShowExportSettings } from "../exportSettings.js";
import { tauriInvoke } from "../saveFile.js";
import { PROCESS_TRACE_CONFIRM, processTraceSummary } from "../processTrace.js";

/**
 * 偏好设置 → “完整过程记录”（2026-11-22 用户决定，技术书 n105 / 融合技术书 r55）。默认关闭；关着时 MOSS
 * 行为与从前一致。只在电脑本机显示（手机连电脑时不显示，后端也拒绝局域网调用）。
 */
export function ProcessTraceSection() {
  if (isRemoteMode()) return null;
  return <ProcessTracePanel />;
}

function ProcessTracePanel() {
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState(false);
  const requestSeq = useRef(0);
  const canChooseDirectory = shouldShowExportSettings();

  const call = useCallback(async (body) => {
    const seq = ++requestSeq.current;
    const res = await postJson("/api/process-trace", { action: "status", ...body });
    if (res.ok && res.data?.success) {
      if (seq === requestSeq.current) setStatus(res.data.result);
      return res.data.result;
    }
    throw new Error(res.data?.result || res.error || "读取失败");
  }, []);

  useEffect(() => {
    call({}).catch(() => setStatus(null));
  }, [call]);

  useEffect(() => {
    if (busy) return;
    const refresh = () => { void call({}).catch(() => setStatus(null)); };
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [busy, call]);

  const directoryCommand = async (command) => {
    setBusy(true);
    // 选择框打开前的状态请求不能在选完后把位置覆盖回去。
    requestSeq.current += 1;
    try {
      const result = await tauriInvoke()(command);
      if (command !== "pick_process_trace_dir" || result !== null) await call({});
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "保存位置设置失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (checked) => {
    if (checked) {
      const ok = await showConfirm(PROCESS_TRACE_CONFIRM, {
        title: "打开完整过程记录",
        confirmText: "打开记录",
        cancelText: "取消",
        tone: "warning",
      });
      if (!ok) return;
    }
    setBusy(true);
    try {
      const res = await postJson("/api/settings", { process_trace_enabled: Boolean(checked) });
      if (!(res.ok && res.data?.success)) {
        throw new Error(res.data?.result || res.error || "保存失败");
      }
      await call({});
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "完整过程记录设置失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    const ok = await showConfirm("删除当前保存文件夹中的全部 MOSS 过程记录（录音、看图照片、合成语音、对话与激光操作记录）？其他位置的旧记录不受影响，删除后无法恢复。", {
      title: "清空过程记录",
      confirmText: "清空",
      cancelText: "取消",
      tone: "warning",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await call({ action: "clear" });
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "清空过程记录失败", tone: "error" });
    } finally {
      setBusy(false);
    }
  };

  const traceSummary = !status ? "读取中…" : status.enabled
    ? status.available === false ? "记录已暂停 · 保存位置不可用" : `已开启 · 保留 ${status.retention_days} 天`
    : "已关闭";

  return (
    <div className="ob-fold settings-spacer" id="settings-process-trace-section">
      <FoldHeader variant="section" id="settings-process-trace" icon={History} title="完整过程记录" summary={traceSummary} defaultOpen={false} />
      <div className="settings-box-4">
        按先后记录语音对话、模型调用与激光操作，方便事后排障；默认关闭，不保存密钥，明细见开启前确认提示。
      </div>
      <label className="settings-label">
        <input
          type="checkbox"
          id="settings-process-trace"
          checked={Boolean(status?.enabled)}
          disabled={busy || !status}
          onChange={(e) => void toggle(e.target.checked)}
        />
        <span>开启完整过程记录</span>
      </label>
      <div className="settings-box-4" id="settings-process-trace-status" style={{ marginTop: "6px" }}>
        {processTraceSummary(status)}
        {status?.dir ? (
          <>
            <br />
            保存位置：<span className="mono">{status.dir}</span>
          </>
        ) : null}
        {status?.available === false && status.error ? <><br />{status.error}</> : null}
      </div>
      {canChooseDirectory ? (
        <>
          <div className="export-settings-row">
            <button type="button" className="btn btn-ghost" id="settings-process-trace-pick" disabled={busy || !status} onClick={() => void directoryCommand("pick_process_trace_dir")}>
              <FolderSearch size={14} /> 选择文件夹…
            </button>
            <button type="button" className="btn btn-ghost" id="settings-process-trace-reset" disabled={busy || !status?.custom_dir} onClick={() => void directoryCommand("reset_process_trace_dir")}>
              <RotateCcw size={14} /> 恢复默认
            </button>
            <button type="button" className="btn btn-ghost" id="settings-process-trace-open" disabled={busy || !status || status.available === false} onClick={() => void directoryCommand("open_process_trace_dir")}>
              <FolderOpen size={14} /> 打开文件夹
            </button>
          </div>
          <div className="settings-box-4">自选位置会使用其中的 moss-process-trace 文件夹；更换后旧记录留在原处，需自行清理。</div>
        </>
      ) : null}
      <div className="settings-row" style={{ marginTop: "8px" }}>
        <button
          type="button"
          className="btn btn-ghost"
          id="settings-process-trace-clear"
          disabled={busy || !status?.bytes || status.available === false}
          onClick={() => void clear()}
        >
          <Trash2 size={14} /> 清空过程记录
        </button>
      </div>
    </div>
  );
}
