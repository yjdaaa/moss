import React, { useEffect, useState } from "react";
import { History, Trash2 } from "lucide-react";
import { FoldHeader } from "./FoldHeader.jsx";
import { postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { isRemoteMode } from "../authSession.js";
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

  const call = async (body) => {
    const res = await postJson("/api/process-trace", { action: "status", ...body });
    if (res.ok && res.data?.success) {
      setStatus(res.data.result);
      return res.data.result;
    }
    throw new Error(res.data?.result || res.error || "读取失败");
  };

  useEffect(() => {
    call({}).catch(() => setStatus(null));
  }, []);

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
    const ok = await showConfirm("删除全部完整过程记录（录音、看图照片、合成语音、对话与激光操作记录）？删除后无法恢复。", {
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

  const traceSummary = !status ? "读取中…" : status.enabled ? "已开启 · 保留 7 天" : "已关闭";

  return (
    <div className="ob-fold settings-spacer" id="settings-process-trace-section">
      <FoldHeader variant="section" id="settings-process-trace" icon={History} title="完整过程记录" summary={traceSummary} defaultOpen={false} />
      <div className="settings-box-4">
        打开后按先后把每一步写成一条时间线：语音对话（含你的录音、设备拍照看图的照片和 MOSS 合成的语音）、发给大模型的完整内容与原始回复、工具调用，以及激光生成、确认发送、急停、加工进度与结果等操作，每一步都写明在干什么，方便事后核对。默认关闭，不打开时 MOSS 的行为不变。
      </div>
      <label className="settings-label">
        <input
          type="checkbox"
          id="settings-process-trace"
          checked={Boolean(status?.enabled)}
          disabled={busy || !status}
          onChange={(e) => void toggle(e.target.checked)}
        />
        <span>完整过程记录（只存在这台电脑上，7 天后自动删除）</span>
      </label>
      <div className="settings-box-4" id="settings-process-trace-status" style={{ marginTop: "6px" }}>
        {processTraceSummary(status)}
        {status?.dir ? (
          <>
            <br />
            位置：<span className="mono">{status.dir}</span>
          </>
        ) : null}
      </div>
      <div className="settings-row" style={{ marginTop: "8px" }}>
        <button
          type="button"
          className="btn btn-ghost"
          id="settings-process-trace-clear"
          disabled={busy || !status?.bytes}
          onClick={() => void clear()}
        >
          <Trash2 size={14} /> 清空过程记录
        </button>
      </div>
    </div>
  );
}
