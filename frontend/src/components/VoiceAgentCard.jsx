import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
import { formDirty, savedNoteText } from "../voiceState.js";
import { SaveBar, VoiceGroup } from "./VoiceLayout.jsx";

const EMPTY = { enabled: false, workspace: "", codex_executable: "", claude_executable: "" };
const PATH_KEYS = ["workspace", "codex_executable", "claude_executable"];

export default function VoiceAgentCard() {
  const [saved, setSaved] = useState(EMPTY);
  const [config, setConfig] = useState(EMPTY);
  const [info, setInfo] = useState({ enabled: false, sandbox_verified: false, runs: [] });
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [result, setResult] = useState(null);
  const dirty = loaded && formDirty(saved, config, PATH_KEYS);

  const apply = (data) => {
    const next = { ...EMPTY, ...(data.settings || {}), enabled: false };
    setSaved(next);
    setConfig(next);
    setInfo(data);
    setLoaded(true);
  };
  useEffect(() => {
    let active = true;
    voiceApi.agentStatus().then((data) => { if (active) apply(data); }).catch((e) => { if (active) setLoadError(e.message || "读取 Agent 配置失败"); });
    return () => { active = false; };
  }, []);
  const change = (name, value) => {
    setConfig((current) => ({ ...current, [name]: value }));
    setResult(null);
  };
  const save = async () => {
    setBusy(true);
    const submitted = { ...config, enabled: false };
    try {
      await voiceApi.agentSave(submitted);
    } catch (e) {
      setResult({ ok: false, text: `保存失败：${e.message || "保存失败"}` });
      setBusy(false);
      return;
    }
    let effect = "执行仍保持锁定";
    try {
      apply(await voiceApi.agentStatus());
    } catch (e) {
      // 已经保存成功，只是读回状态失败：按提交的内容记为已保存，不能报“保存失败”
      setSaved(submitted);
      setConfig(submitted);
      effect += ` · 读取最新状态失败：${e.message || "未知错误"}`;
    }
    setResult({ ok: true, text: savedNoteText("路径", effect) });
    setBusy(false);
  };
  const runs = Array.isArray(info.runs) ? info.runs.slice(0, 50) : [];
  return (
    <VoiceGroup id="agent" title="Claude Code / Codex" label="只读编程代理" summary="执行已锁定" dirty={dirty}>
      <div className="voice-hint voice-hint--warn">
        执行已锁定：只能保存路径，不会启动 Claude Code、Codex 或其他程序。沙箱安全状态：{info.sandbox_verified ? "已标记验收（仍需审批）" : "未验证，禁止执行"}。
      </div>
      {loadError && <div className="voice-hint voice-hint--bad" role="alert">{loadError}</div>}
      <div className="voice-form-grid">
        {[
          ["workspace", "专用工作目录（不要包含 MOSS 数据或账号密钥）", "C:\\agent-workspace"],
          ["codex_executable", "Codex 可执行文件（codex.exe）", "codex.exe 的完整路径"],
          ["claude_executable", "Claude Code 可执行文件（claude.exe）", "claude.exe 的完整路径"],
        ].map(([name, label, hint]) => (
          <label className="voice-wide" key={name}>
            <span>{label}</span>
            <input className="text-input mono" value={config[name] || ""} placeholder={hint} disabled={busy} onChange={(e) => change(name, e.target.value)} />
          </label>
        ))}
      </div>
      <SaveBar id="voice-agent-save" target="路径" dirty={dirty} busy={busy} saving={busy} result={result} onSave={save} />
      <details>
        <summary>最近任务记录（{runs.length} 条）</summary>
        {runs.length === 0 ? (
          <p className="voice-muted">暂无执行记录。</p>
        ) : (
          runs.map((item) => (
            <article key={item.id} className="voice-agent-run">
              <strong>{item.provider === "codex" ? "Codex" : "Claude Code"}</strong>
              <span> · {item.status} · {item.workspace_name || "未知工作区"}</span>
              <p className="voice-muted">{item.output ? String(item.output).slice(0, 200) : item.error_code || "无结果摘要"}</p>
            </article>
          ))
        )}
      </details>
    </VoiceGroup>
  );
}
