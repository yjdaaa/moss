import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";

const EMPTY = { enabled: false, workspace: "", codex_executable: "", claude_executable: "" };

export default function VoiceAgentCard() {
  const [config, setConfig] = useState(EMPTY);
  const [info, setInfo] = useState({ enabled: false, sandbox_verified: false, runs: [] });
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const load = async () => {
    const data = await voiceApi.agentStatus();
    setConfig({ ...EMPTY, ...(data.settings || {}), enabled: false });
    setInfo(data);
  };
  useEffect(() => {
    let active = true;
    voiceApi.agentStatus().then((data) => {
      if (!active) return;
      setConfig({ ...EMPTY, ...(data.settings || {}), enabled: false });
      setInfo(data);
    }).catch((e) => { if (active) setError(e.message || "读取 Agent 配置失败"); });
    return () => { active = false; };
  }, []);
  const change = (name, value) => setConfig((value0) => ({ ...value0, [name]: value }));
  const save = async () => {
    setBusy(true); setError(""); setMessage("");
    try {
      await voiceApi.agentSave({ ...config, enabled: false });
      await load();
      setMessage("已保存路径。真实 CLI 沙箱验收未完成，执行开关仍保持关闭。");
    } catch (e) { setError(e.message || "保存失败"); }
    finally { setBusy(false); }
  };
  return <section className="voice-card" aria-label="只读编程代理">
    <h3>Claude Code / Codex（NV13）</h3>
    <p><strong>CLI 执行已锁定。</strong> 当前只允许查看与保存路径，不启动 Claude Code、Codex 或其他子进程。只读沙箱、命令与 MCP 禁止、进程树终止必须完成真实权限负向验收后才能开放。</p>
    <p role="status">沙箱安全状态：{info.sandbox_verified ? "已标记验收（仍需核对审批）" : "未验证，禁止执行"}</p>
    {[
      ["workspace", "专用工作目录（不应包含 MOSS 数据或账号密钥）", "C:\\agent-workspace"],
      ["codex_executable", "Codex 可执行文件（codex.exe）", "codex.exe 的完整路径"],
      ["claude_executable", "Claude Code 可执行文件（claude.exe）", "claude.exe 的完整路径"],
    ].map(([name, label, hint]) =>
      <label className="voice-form-group" key={name}>
        <span>{label}</span>
        <input value={config[name] || ""} placeholder={hint} disabled={busy}
          onChange={(e) => change(name, e.target.value)} />
      </label>
    )}
    {error && <p role="alert">{error}</p>}
    {message && <p role="status">{message}</p>}
    <button type="button" onClick={save} disabled={busy}>{busy ? "保存中…" : "保存路径（不启用执行）"}</button>
    <h4>最近任务记录（最多 50 条）</h4>
    {(info.runs || []).length === 0
      ? <p>暂无 Agent 执行记录。</p>
      : <div className="voice-form-group">
        {(info.runs || []).slice(0, 50).map((item) =>
          <article key={item.id}>
            <strong>{item.provider === "codex" ? "Codex" : "Claude Code"}</strong>
            <span> · {item.status} · {item.workspace_name || "未知工作区"}</span>
            <p>{item.output ? String(item.output).slice(0, 200) : item.error_code || "无结果摘要"}</p>
          </article>)}
      </div>}
    <p>任务需授权设备和桌面单次审批；权限、工作目录或配置改变后，原批准不得执行。实际运行可能使用你的 CLI 账户并产生费用。</p>
  </section>;
}
