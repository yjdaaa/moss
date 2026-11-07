import React, { useEffect, useRef, useState } from "react";
import { Plus, Save, X, RefreshCw, Trash2 } from "lucide-react";
import { voiceApi } from "../voiceApi.js";
import { savedNoteText, voiceErrorText, VOICE_POLL_MS } from "../voiceState.js";
import { showConfirm } from "../appDialog.js";
import { VoiceGroup } from "./VoiceLayout.jsx";
import { MCP_POLICIES, mcpForm, mcpFormError, mcpSavePayload, mcpConfirmation, mcpConnectionText } from "../voiceMcpState.js";

const time = (seconds) => seconds ? new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false }) : "未调用";
const AUDIT = { external_mcp_config_changed: "配置更新", external_tool_requested: "开始调用", external_tool_completed: "调用返回", external_tool_denied: "调用被拒绝", approval_created: "生成审批", approval_approved: "已批准", approval_executed: "消费授权", approval_expired: "审批已过期", approval_rejected: "已拒绝", approval_failed: "执行未完成" };
export function VoiceMcpCard({ defaultOpen = true }) {
  const [status, setStatus] = useState(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(null);
  const [form, setForm] = useState(null);
  const [note, setNote] = useState("");
  const alive = useRef(true);
  const busyRef = useRef(false);
  const epoch = useRef(0);
  useEffect(() => {
    alive.current = true;
    let cancelled = false;
    let timer;
    const poll = async () => {
      const version = epoch.current;
      try {
        if (!busyRef.current) { const next = await voiceApi.mcpStatus(); if (!cancelled && version === epoch.current) { setStatus(next); setError(""); } }
      } catch (e) { if (!cancelled && version === epoch.current) setError(voiceErrorText(e)); }
      if (!cancelled) timer = setTimeout(poll, VOICE_POLL_MS);
    };
    void poll();
    return () => { cancelled = true; alive.current = false; clearTimeout(timer); };
  }, []);
  const act = async (work) => {
    if (busyRef.current) return false;
    setBusy(true); busyRef.current = true; epoch.current += 1;
    try { const next = await work(); if (alive.current) { setStatus(next); setError(""); } return true; }
    catch (e) { if (alive.current) setError(voiceErrorText(e)); return false; }
    finally { epoch.current += 1; busyRef.current = false; if (alive.current) setBusy(false); }
  };
  // pristine：打开表单时的内容，只有改过才算未保存
  const edit = (record) => { const initial = mcpForm(record); setEditing({ name: record?.config.name || "", revision: status.revision, pristine: JSON.stringify(initial) }); setForm(initial); };
  const change = (key, value) => setForm((old) => ({ ...old, [key]: value }));
  const credential = (index, key, value) => setForm((old) => ({ ...old, credentials: old.credentials.map((item, i) => i === index ? { ...item, [key]: value, ...(key === "key" ? { saved: false, reference: "" } : {}) } : item) }));
  const save = async () => {
    if (!form || mcpFormError(form)) return;
    if (!(await showConfirm(mcpConfirmation(form), { title: "保存第三方 MCP？", tone: "warning", confirmText: "确认保存" }))) return;
    const payload = mcpSavePayload(form, editing.revision);
    if (await act(() => voiceApi.mcpSave(payload))) { setNote(savedNoteText(`服务 ${form.name}`, form.enabled ? "开始连接" : "未启用，不会启动")); setForm(null); setEditing(null); }
  };
  const policy = async (record, tool, value) => {
    const next = mcpForm(record);
    next.policies[tool.original] = value;
    const message = value === "allow" ? `把 ${tool.name} 设为免批准？仅在你确认服务可信且工具只读时使用；服务可能谎报能力。` : `把 ${tool.name} 设为“${MCP_POLICIES[value]}”？此服务旧审批将失效。`;
    if (!(await showConfirm(message, { title: "修改工具权限？", tone: "warning", confirmText: "确认修改" }))) return;
    await act(() => voiceApi.mcpSave(mcpSavePayload(next, status.revision)));
  };
  const remove = async (record) => {
    if (!(await showConfirm(`移除 ${record.config.name}？连接会停止，配置和当前凭据会删除，未执行的审批失效。`, { title: "移除服务？", danger: true, confirmText: "移除" }))) return;
    await act(() => voiceApi.mcpDelete(record.config.name, status.revision, true));
  };
  const reconnect = async (name) => {
    if (!(await showConfirm("重新连接此服务？本机程序可能重新启动；上一次工具操作不会重新执行。", { title: "重新连接？", confirmText: "重新连接" }))) return;
    await act(() => voiceApi.mcpReconnect(name, true));
  };
  // Tauri backend upgrades or partial status replies must never tear down
  // the complete React tree. Older/failed responses may lack these arrays.
  const servers = Array.isArray(status?.servers) ? status.servers.filter((item) => item?.config) : [];
  const connections = Array.isArray(status?.connections) ? status.connections : [];
  const audit = Array.isArray(status?.audit) ? status.audit : [];
  const maxServers = Number.isInteger(status?.max_servers) ? status.max_servers : 8;
  const formChanged = Boolean(form && editing && JSON.stringify(form) !== editing.pristine);
  const summary = !status ? "读取中…" : servers.length ? `${servers.length} 个服务 · ${servers.filter((item) => item.config.enabled).length} 个启用` : "未添加";
  return <VoiceGroup id="mcp" title="第三方 MCP 工具（外部程序，可随时添加或移除）" summary={summary} dirty={formChanged} defaultOpen={defaultOpen}><div id="voice-mcp-card" className="voice-stack">
    <p className="voice-muted">这些是 MOSS 之外独立运行的外部工具服务（现成的社区 MCP 或你自己写的小程序），用下面的「添加服务」随时加、用「移除」随时删，和上面「MOSS 内置工具」里那些编译进 MOSS、只能开关的原生工具不是一回事。</p>
    <p className="voice-muted">服务默认停用，工具默认每次需批准。密钥填在凭据栏，不要放进地址或启动参数。</p>
    {error && <div className="voice-hint voice-hint--bad" role="alert">{error}</div>}
    {status?.last_error && <div className="voice-hint voice-hint--warn">{status.last_error}</div>}
    <div className="voice-actions"><button className="btn btn-ghost" disabled={busy || !status || Boolean(form) || servers.length >= maxServers} onClick={() => { setNote(""); edit(null); }} id="voice-mcp-add"><Plus size={14} />添加服务</button><span className="voice-muted">最多 8 个服务、64 个工具。</span>{note && <span className="voice-ok" role="status">✓ {note}</span>}</div>
    {form && <div className="voice-builtin-models" id="voice-mcp-form">
      <h3>{editing.name ? `编辑 ${editing.name}` : "添加第三方服务"}</h3>
      <div className="voice-form-grid">
        <label><span>服务名（英文、数字、横线或下划线）</span><input className="text-input" value={form.name} disabled={Boolean(editing.name) || busy} onChange={(e) => change("name", e.target.value)} id="voice-mcp-name" /></label>
        <label><span>连接方式</span><select className="text-input" value={form.type} disabled={busy} onChange={(e) => setForm((old) => ({ ...old, type: e.target.value, credentials: [], policies: {} }))} id="voice-mcp-type"><option value="streamable_http">Streamable HTTP</option><option value="stdio">本机程序（stdio）</option></select></label>
        {form.type === "stdio" ? <><label className="voice-wide"><span>程序路径（不拼接参数）</span><input className="text-input mono" value={form.command} onChange={(e) => change("command", e.target.value)} id="voice-mcp-command" /></label><label className="voice-wide"><span>启动参数（JSON 字符串数组）</span><textarea className="text-input mono" rows={3} value={form.args} onChange={(e) => change("args", e.target.value)} id="voice-mcp-args" /></label></> : <label className="voice-wide"><span>MCP 地址</span><input className="text-input mono" value={form.url} onChange={(e) => change("url", e.target.value)} autoComplete="off" id="voice-mcp-url" /></label>}
        <label className="voice-check"><input type="checkbox" checked={form.enabled} onChange={(e) => change("enabled", e.target.checked)} id="voice-mcp-enabled" /><span>保存后启用此服务</span></label>
      </div>
      <h4>{form.type === "stdio" ? "环境变量凭据" : "请求头凭据"}</h4>
      {form.credentials.map((item, index) => <div className="voice-form-grid" key={index}>
        <label><span>{form.type === "stdio" ? "环境变量名" : "请求头名（小写）"}</span><input className="text-input mono" value={item.key} onChange={(e) => credential(index, "key", e.target.value)} aria-label={`凭据名称 ${index + 1}`} /></label>
        <label><span>{item.saved ? "已保存；留空不变" : "凭据值"}</span><input type="password" className="text-input" value={item.value} onChange={(e) => credential(index, "value", e.target.value)} autoComplete="new-password" aria-label={`凭据值 ${index + 1}`} /></label>
        <button className="btn btn-ghost" disabled={busy} onClick={() => change("credentials", form.credentials.filter((_, i) => i !== index))}>移除此凭据</button>
      </div>)}
      <div className="voice-actions"><button className="btn btn-ghost" disabled={busy || form.credentials.length >= 32} onClick={() => change("credentials", [...form.credentials, { key: "", value: "", saved: false, reference: "" }])} id="voice-mcp-add-secret">添加凭据</button></div>
      {mcpFormError(form) && <p className="voice-hint voice-hint--warn">{mcpFormError(form)}</p>}
      <div className="voice-actions"><button className="btn btn-primary" disabled={busy || Boolean(mcpFormError(form))} onClick={save} id="voice-mcp-save"><Save size={14} />保存此服务</button><button className="btn btn-ghost" disabled={busy} onClick={() => { setForm(null); setEditing(null); }}><X size={14} />取消</button></div>
    </div>}
    {!status && !error && <p>正在读取第三方服务…</p>}
    {status && servers.length === 0 && <p className="voice-muted">尚未添加第三方 MCP 服务。</p>}
    {servers.map((record) => {
      const config = record.config;
      const connection = connections.find((view) => view.name === config.name);
      return <div className="voice-builtin-models" key={config.name} id={`voice-mcp-server-${config.name}`}>
        <h3>{config.name} <span className="ob-pill">{config.enabled ? mcpConnectionText(connection) : "已停用"}</span></h3>
        <p className="voice-muted">{config.transport?.type === "stdio" ? "本机程序" : "Streamable HTTP"} · 连接成功 {connection?.connections || 0} 次{connection?.retry_seconds ? ` · ${connection.retry_seconds} 秒退避重连` : ""}</p>
        {connection?.last_error && <p className="voice-hint voice-hint--warn">{connection.last_error}</p>}
        <div className="voice-actions"><button className="btn btn-ghost" disabled={busy || Boolean(form)} onClick={() => { setNote(""); edit(record); }}>编辑设置</button><button className="btn btn-ghost" disabled={busy || !config.enabled} onClick={() => reconnect(config.name)}><RefreshCw size={14} />重新连接</button><button className="btn btn-ghost" disabled={busy} onClick={() => remove(record)}><Trash2 size={14} />移除</button></div>
        {Array.isArray(connection?.tools) && connection.tools.length > 0 && <div className="ob-tbl voice-tbl"><table><thead><tr><th>工具</th><th>权限</th><th>本次运行</th><th>最近调用</th></tr></thead><tbody>{connection.tools.map((tool) => <tr key={tool.name}><td className="voice-mcp-cell"><b>{tool.name}</b><div className="ob-sub">{tool.description}</div>{tool.blocked_reason && <div className="voice-bad">{tool.blocked_reason}</div>}</td><td><select className="text-input" value={tool.permission} disabled={busy || Boolean(form)} aria-label={`${tool.name} 权限`} onChange={(e) => policy(record, tool, e.target.value)}>{Object.entries(MCP_POLICIES).map(([key, label]) => <option value={key} key={key}>{label}</option>)}</select></td><td>调用 {tool.stats.calls} 次<br />失败 {tool.stats.failures} 次</td><td>{time(tool.stats.last_call_at)}</td></tr>)}</tbody></table></div>}
      </div>;
    })}
    {audit.length > 0 && <details className="voice-builtin-models"><summary>最近审批与审计（最多 50 条）</summary><div className="ob-tbl voice-tbl"><table><thead><tr><th>时间</th><th>操作</th><th>工具／服务</th><th>结果</th></tr></thead><tbody>{audit.map((entry, index) => <tr key={index}><td>{time(entry.at)}</td><td>{AUDIT[entry.action] || entry.action}<div className="ob-sub">{entry.detail?.reason}</div></td><td className="voice-mcp-cell-2">{entry.target}</td><td>{entry.result === "ok" ? "已记录" : entry.result === "denied" ? "已拒绝" : "未完成"}</td></tr>)}</tbody></table></div></details>}
  </div></VoiceGroup>;
}
