import React, { useEffect, useState } from "react";
import { FolderOpen, FolderSearch, RotateCcw, AlertTriangle } from "lucide-react";
import { FoldHeader } from "./FoldHeader.jsx";
import { showAlert } from "../appDialog.js";
import { tauriInvoke } from "../saveFile.js";
import { shouldShowExportSettings } from "../exportSettings.js";

/**
 * 偏好设置 → “导出”分区（技术书 §9.7.2）。只在 Windows / 桌面版 Tauri 中显示；
 * Android 与普通浏览器不显示（保存位置分别由系统、浏览器决定）。
 * 目录只能经系统文件夹选择框修改，不提供手输路径。
 */
export function ExportSettingsSection() {
  if (!shouldShowExportSettings()) return null;
  return <ExportSettingsPanel />;
}

function ExportSettingsPanel() {
  const invoke = tauriInvoke();
  const [view, setView] = useState(null);
  const [busy, setBusy] = useState(false);

  const run = async (command, args) => {
    setBusy(true);
    try {
      const next = await invoke(command, args);
      if (next) setView(next);
      return next;
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "导出设置失败", tone: "error" });
      return null;
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void run("get_export_settings");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const openFolder = async () => {
    try {
      await invoke("open_export_dir");
    } catch (error) {
      await showAlert(String(error?.message ?? error), { title: "无法打开文件夹", tone: "error" });
    }
  };

  return (
    <div className="ob-fold" style={{ marginBottom: "20px", paddingBottom: "18px", borderBottom: "1px solid var(--line)" }}>
      <FoldHeader variant="section" id="settings-export" icon={FolderOpen} title="导出" defaultOpen={false} />
      <div style={{ fontSize: "12px", color: "var(--ink-dim)", marginBottom: "10px" }}>
        画板 PNG、场景 JSON、G-code 下载、材料库导出都会保存到这里。不影响运行数据目录。
      </div>
      <div className="ob-export-path">
        <span className="ob-export-label">{view?.dir ? "当前导出目录" : "默认：下载文件夹"}</span>
        <span className="mono">{view?.effective_dir || "读取中…"}</span>
        {view && !view.available && (
          <span className="ob-export-warn">
            <AlertTriangle size={13} aria-hidden="true" /> 目录当前不可用（可能已拔出或无写入权限），导出时会请你重新选择
          </span>
        )}
      </div>
      <div style={{ display: "flex", flexWrap: "wrap", gap: "8px", margin: "10px 0" }}>
        <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => run("pick_export_dir")}>
          <FolderSearch size={14} /> 选择文件夹…
        </button>
        <button type="button" className="btn btn-ghost" disabled={busy || !view?.dir} onClick={() => run("reset_export_dir")}>
          <RotateCcw size={14} /> 恢复默认
        </button>
        <button type="button" className="btn btn-ghost" disabled={busy || !view} onClick={openFolder}>
          <FolderOpen size={14} /> 打开文件夹
        </button>
      </div>
      <label className="checkbox-row" style={{ display: "flex", alignItems: "center", gap: "8px", fontSize: "13px" }}>
        <input
          type="checkbox"
          checked={Boolean(view?.ask_each_time)}
          disabled={busy || !view}
          onChange={(event) => run("set_export_ask_each_time", { enabled: event.target.checked })}
        />
        每次导出时询问保存位置
      </label>
    </div>
  );
}
