import React, { useCallback, useEffect, useState } from "react";
import { FolderOpen, RefreshCw, Trash2 } from "lucide-react";
import { voiceApi } from "../voiceApi.js";
import { showConfirm } from "../appDialog.js";
import { VoiceGroup } from "./VoiceLayout.jsx";

export default function VoiceKnowledgeCard() {
  const [roots, setRoots] = useState([]);
  const [loaded, setLoaded] = useState(false);
  const [busyId, setBusyId] = useState("");
  const [picking, setPicking] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [result, setResult] = useState(null);

  const load = useCallback(async () => {
    const res = await voiceApi.contextManage({ action: "knowledge_list" });
    setRoots(Array.isArray(res?.roots) ? res.roots : []);
    setLoadError("");
    setLoaded(true);
  }, []);

  useEffect(() => {
    let alive = true;
    voiceApi
      .contextManage({ action: "knowledge_list" })
      .then((res) => {
        if (!alive) return;
        setRoots(Array.isArray(res?.roots) ? res.roots : []);
        setLoaded(true);
      })
      .catch((e) => {
        if (alive) setLoadError(e?.message || "本地知识库状态读取失败");
      });
    return () => {
      alive = false;
    };
  }, []);

  const pickFolder = async () => {
    setPicking(true);
    setResult(null);
    try {
      const added = await voiceApi.contextPickFolder();
      if (added) {
        await load();
        setResult({
          ok: true,
          text: `已挂载「${added.label || "新知识库"}」：索引 ${added.files ?? 0} 个文件（${added.directories ?? 0} 个目录）`,
        });
      }
    } catch (e) {
      setResult({ ok: false, text: e?.message || "添加资料文件夹失败" });
    } finally {
      setPicking(false);
    }
  };

  const toggleRoot = async (root, enabled) => {
    setBusyId(root.id);
    setResult(null);
    try {
      const updated = await voiceApi.contextManage({
        action: "knowledge_toggle",
        id: root.id,
        revision: root.revision,
        enabled,
      });
      setRoots((current) => current.map((item) => (item.id === root.id ? { ...item, ...updated } : item)));
      setResult({ ok: true, text: `${enabled ? "已挂载" : "已停用"}「${root.label}」` });
    } catch (e) {
      setResult({ ok: false, text: e?.message || "切换知识库状态失败" });
      await load().catch(() => {});
    } finally {
      setBusyId("");
    }
  };

  const reindexRoot = async (root) => {
    setBusyId(root.id);
    setResult(null);
    try {
      const report = await voiceApi.contextManage({ action: "knowledge_reindex", id: root.id });
      await load();
      setResult({ ok: true, text: `「${root.label}」已更新索引：${report?.files ?? 0} 个文件` });
    } catch (e) {
      setResult({ ok: false, text: e?.message || "重建知识库索引失败" });
    } finally {
      setBusyId("");
    }
  };

  const removeRoot = async (root) => {
    const ok = await showConfirm(`移除知识库「${root.label}」？只会清除索引，不会删除磁盘上的原始文件。`, {
      title: "移除知识库",
      tone: "warning",
      danger: true,
      confirmText: "移除",
    });
    if (!ok) return;
    setBusyId(root.id);
    setResult(null);
    try {
      await voiceApi.contextManage({ action: "knowledge_remove", id: root.id, revision: root.revision });
      await load();
      setResult({ ok: true, text: `已移除「${root.label}」` });
    } catch (e) {
      setResult({ ok: false, text: e?.message || "移除知识库失败" });
    } finally {
      setBusyId("");
    }
  };

  const enabledCount = roots.filter((r) => r.enabled !== false).length;
  const summary = loadError
    ? "读取失败"
    : !loaded
      ? "读取中…"
      : roots.length === 0
        ? "未添加"
        : enabledCount > 0
          ? `已启用 ${enabledCount} / ${roots.length} 个`
          : `共 ${roots.length} 个 · 未启用`;

  return (
    <VoiceGroup
      id="knowledge"
      title="本地知识库"
      label="本地知识库"
      summary={summary}
      tone={enabledCount > 0 && !loadError ? "ok" : undefined}
    >
      <p className="voice-muted">
        把本机电脑文件夹挂载为独立离线知识库（只读 txt / md / csv 文本，本机自动建索引、不上传、无需 API 密钥；勾选即在语音对话中生效，与「隐私与家居 → 信息服务」里的外部 RAGFlow 远程服务相互独立）。
      </p>
      {loadError && (
        <div className="voice-hint voice-hint--bad" role="alert">
          {loadError}
        </div>
      )}
      <div className="voice-actions">
        <button
          type="button"
          className="btn btn-ghost"
          id="voice-knowledge-add"
          disabled={picking || Boolean(busyId)}
          onClick={pickFolder}
        >
          <FolderOpen size={13} /> {picking ? "正在索引…" : "添加资料文件夹…"}
        </button>
        {result && (
          <span className={result.ok ? "voice-ok" : "voice-bad"} role={result.ok ? "status" : "alert"}>
            {result.ok ? "✓ " : ""}
            {result.text}
          </span>
        )}
      </div>
      {loaded && roots.length === 0 && <p className="voice-muted">尚未添加知识库文件夹。</p>}
      {roots.length > 0 && (
        <div className="voice-kb-list" role="list" aria-label="本地知识库列表">
          {roots.map((root) => {
            const on = root.enabled !== false;
            const rowBusy = picking || busyId === root.id;
            return (
              <div key={root.id} className="voice-kb-row" role="listitem" id={`voice-kb-${root.id}`}>
                <label className="voice-kb-check">
                  <input
                    type="checkbox"
                    checked={on}
                    disabled={rowBusy}
                    aria-label={`对话时启用 ${root.label}`}
                    onChange={(e) => toggleRoot(root, e.target.checked)}
                  />
                  <span className="voice-kb-main">
                    <b>{root.label}</b>
                    <small className="voice-muted">
                      {on ? "已挂载" : "已停用"} · {root.files ?? 0} 个文件、{root.directories ?? 0} 个目录
                      {root.indexed_at
                        ? ` · 更新于 ${new Date(root.indexed_at * 1000).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" })}`
                        : ""}
                    </small>
                  </span>
                </label>
                <div className="voice-actions">
                  <button
                    type="button"
                    className="btn btn-ghost voice-inline-btn"
                    disabled={rowBusy}
                    onClick={() => reindexRoot(root)}
                  >
                    <RefreshCw size={12} /> 更新索引
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost voice-inline-btn"
                    disabled={rowBusy}
                    onClick={() => removeRoot(root)}
                  >
                    <Trash2 size={12} /> 移除
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </VoiceGroup>
  );
}
