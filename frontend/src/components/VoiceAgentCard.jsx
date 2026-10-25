import React, { useEffect, useState } from "react";
import { Square } from "lucide-react";
import { voiceApi } from "../voiceApi.js";
import { agentErrorText, agentView, formDirty, savedNoteText } from "../voiceState.js";
import { showConfirm } from "../appDialog.js";
import { SaveBar, VoiceGroup } from "./VoiceLayout.jsx";
import { agentPlatformCopy } from "../platform.js";

// NV13 Claude Code / Codex（2026-10-15 用户决定）：
// 小智说一句话就能让本机的官方 CLI 在指定工作目录里干活，可以修改该目录的文件。
// 每次运行都要在电脑上批准一次；只在严格模式开启时运行（Agent 即使调用本机 MOSS 接口也动不了激光机）。
const EMPTY = { enabled: false, workspace: "", codex_executable: "", claude_executable: "" };

export default function VoiceAgentCard() {
  const [saved, setSaved] = useState(EMPTY);
  const [config, setConfig] = useState(EMPTY);
  const [info, setInfo] = useState({});
  const [loaded, setLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [stopping, setStopping] = useState("");
  const [loadError, setLoadError] = useState("");
  const [result, setResult] = useState(null);
  const busy = saving || Boolean(stopping);
  const dirty = loaded && formDirty(saved, config);

  const apply = (data) => {
    const next = { ...EMPTY, ...((data || {}).settings || {}) };
    setSaved(next);
    setConfig(next);
    setInfo(data || {});
    setLoaded(true);
  };
  useEffect(() => {
    let active = true;
    voiceApi.agentStatus()
      .then((data) => { if (active) apply(data); })
      .catch((e) => { if (active) setLoadError(e.message || "读取 Agent 配置失败"); });
    return () => { active = false; };
  }, []);

  const refresh = async () => {
    try { apply(await voiceApi.agentStatus()); setLoadError(""); }
    catch (e) { setLoadError(e.message || "读取 Agent 配置失败"); }
  };
  // 小智可能随时发起任务：每 5 秒刷新一次状态（运行中的任务结束时会显示为已完成 / 已停止）。
  useEffect(() => {
    const timer = setInterval(() => { void refresh(); }, 5000);
    return () => clearInterval(timer);
  }, []);
  const change = (name, value) => {
    setConfig((current) => ({ ...current, [name]: value }));
    setResult(null);
  };

  const save = async () => {
    setSaving(true);
    try {
      const response = await voiceApi.agentSave({ ...config });
      apply(response);
      const effect = config.enabled
        ? "小智可以发起本机 Agent 任务，每次仍要在电脑批准"
        : "已停止提供 Agent 工具";
      setResult({ ok: true, text: savedNoteText("Agent 设置", effect) });
    } catch (e) {
      setResult({ ok: false, text: `保存失败：${e.message || "保存 Agent 配置失败"}` });
    } finally {
      setSaving(false);
    }
  };

  const stop = async (run) => {
    const ok = await showConfirm("停止这条 Agent 任务？正在运行的进程会被一起结束，已经写进工作目录的改动不会撤销。", {
      title: "停止 Agent 任务", tone: "warning", danger: true, confirmText: "停止任务",
    });
    if (!ok) return;
    setStopping(run.id);
    setResult(null);
    try {
      const response = await voiceApi.agentStop(run.id);
      apply(response);
      setResult(response && response.stopped === false
        ? { ok: false, text: "这条任务已经结束了，没有可停止的进程" }
        : { ok: true, text: "已请求停止；任务状态会更新为“已停止”" });
    } catch (e) {
      setResult({ ok: false, text: `停止失败：${e.message || "停止 Agent 任务失败"}` });
    } finally {
      setStopping("");
    }
  };

  const view = agentView({ ...info, configured: saved.enabled });
  const platformCopy = agentPlatformCopy();
  const detected = info?.detected && typeof info.detected === "object" ? info.detected : {};
  const hasCodex = Boolean(config.codex_executable.trim() || detected.codex_executable);
  const hasClaude = Boolean(config.claude_executable.trim() || detected.claude_executable);
  const blocked = config.enabled && !config.workspace.trim()
    ? "开启前必须填写工作目录"
    : config.enabled && !hasCodex && !hasClaude
      ? "开启前至少要填一个 CLI 路径（本机暂未自动检测到）"
      : "";
  return (
    <VoiceGroup id="agent" title="Claude Code / Codex" label="本机编程代理"
      summary={loadError ? "读取失败" : !loaded ? "读取中…" : view.summary}
      tone={view.summary === "可用" ? "ok" : undefined} dirty={dirty}>
      <p className="voice-muted">
        小智说一句话，就让本机的官方 Codex / Claude Code 在你指定的工作目录里干活（可以新建和修改该目录里的文件）。
        默认关闭；CLI 路径留空即可自动读取本机安装的版本（换电脑或升级版本会自动适配），开启后每次运行都会在电脑上弹审批卡，批准了才启动。
      </p>
      {!platformCopy.sandboxSupported && (
        <div className="voice-hint voice-hint--warn" role="note">
          当前系统暂未启用本机 Agent 进程树沙箱，发起任务会按安全策略拒绝运行。
        </div>
      )}
      <div className="voice-hint voice-hint--warn">
        Agent 只在严格模式开启时运行，这样它即使调用本机 MOSS 接口也动不了激光机。严格模式在语音中心顶部的“严格模式”开关里开启。另外提醒：CLI 自己的沙箱挡不住“读取工作目录以外的文件”，所以工作目录请挑一个可以公开给模型看的目录，不要放在 MOSS 数据目录里。
      </div>
      {loadError && <div className="voice-hint voice-hint--bad" role="alert">{loadError}</div>}
      {loaded && saved.enabled && !info.strict_mode && (
        <div className="voice-hint voice-hint--warn" role="alert">
          严格模式当前未开启，Agent 不会运行：点语音中心顶部的“严格模式”开关开启即可。
        </div>
      )}
      <div className="voice-form-grid">
        <label className="voice-check voice-wide">
          <input type="checkbox" id="voice-agent-enabled" checked={!!config.enabled} disabled={!loaded || busy}
            onChange={(event) => change("enabled", event.target.checked)} />
          <span>允许小智发起本机 Agent 任务（每次运行仍需在电脑批准）</span>
        </label>
        {[
          ["workspace", "专用工作目录（Agent 只能在这里读写；不要指向 MOSS 数据目录或账号密钥目录）", platformCopy.workspacePlaceholder],
          ["codex_executable", platformCopy.codexLabel, platformCopy.codexPlaceholder],
          ["claude_executable", platformCopy.claudeLabel, platformCopy.claudePlaceholder],
        ].map(([name, label, hint]) => {
          const autoPath = name !== "workspace" ? detected[name] : "";
          const manualValue = String(config[name] || "").trim();
          return (
            <label className="voice-wide" key={name}>
              <span>{label}</span>
              <input className="text-input mono" value={config[name] || ""} placeholder={hint}
                disabled={!loaded || busy} autoComplete="off" onChange={(e) => change(name, e.target.value)} />
              {autoPath && !manualValue && (
                <small className="voice-muted mono">已自动读取本机路径：{autoPath}</small>
              )}
              {autoPath && manualValue && manualValue !== autoPath && (
                <small className="voice-muted">
                  本机已检测到 <span className="mono">{autoPath}</span>{" "}
                  <button type="button" className="btn-link" disabled={!loaded || busy} onClick={() => change(name, "")}>
                    改用自动读取
                  </button>
                </small>
              )}
            </label>
          );
        })}
      </div>
      {loaded && saved.enabled && (view.providers.length > 0 ? (
        <ul className="voice-agent-providers" aria-label="CLI 可用情况">
          {view.providers.map((provider) => (
            <li key={provider.name} className={provider.ready ? "voice-ok" : "voice-bad"}>
              {provider.name}：{provider.text}
            </li>
          ))}
        </ul>
      ) : <p className="voice-muted">本机暂未检测到已安装的 CLI，请安装官方 Codex / Claude Code 或手动填写路径。</p>)}
      <SaveBar id="voice-agent-save" target="Agent 设置" dirty={dirty} busy={busy} saving={saving}
        blocked={blocked} result={result} onSave={save} />
      <h3>最近任务记录</h3>
      {!loaded ? null : view.runs.length === 0 ? (
        <p className="voice-muted">暂无执行记录。</p>
      ) : (
        view.runs.map((item) => (
          <article key={item.id} className="voice-agent-run">
            <strong>{item.provider === "codex" ? "Codex" : "Claude Code"}</strong>
            <span> · {item.statusText} · {item.workspace_name || "未知工作区"}</span>
            {item.stoppable && (
              <button type="button" className="btn btn-ghost voice-inline-btn" disabled={busy}
                onClick={() => stop(item)}>
                <Square size={12} /> {stopping === item.id ? "停止中…" : "停止"}
              </button>
            )}
            <p className="voice-muted">{item.detail ? String(item.detail).slice(0, 400) : "无结果"}</p>
            {item.error_code && item.error_code !== "agent_stopped" && (
              <p className="voice-bad">{agentErrorText(item.error_code)}</p>
            )}
          </article>
        ))
      )}
    </VoiceGroup>
  );
}
