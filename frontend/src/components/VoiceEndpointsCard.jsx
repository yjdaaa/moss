import React, { useState } from "react";
import { Copy, Plus, Trash2 } from "lucide-react";
import { VoiceGroup } from "./VoiceLayout.jsx";
import { showConfirm } from "../appDialog.js";
import { voiceApi } from "../voiceApi.js";
import { TOOL_EDIT_PROMPTS, endpointLabel, endpointView, extraEndpointError, identityView, toolEditPrompt, toolStatText, toolSwitchPayload } from "../voiceState.js";

// 接入点与工具（内置语音技术书 NV8）：附加官方接入点、各接入点连接状态与身份（仅官方/自建链路渲染，放在“设备”标签）；
// 内置工具开关与调用统计（所有链路共用，n69 起放在“扩展”标签，紧挨第三方 MCP 工具，方便对照两者区别）。
// 地址一律由后端脱敏后返回；新增时填写的完整地址只在本页内存中，提交后清空。

const TONE_CLASS = { ok: "ob-pill--ok", info: "ob-pill--info", warn: "ob-pill--warn", bad: "ob-pill--bad", off: "ob-pill--off" };

function formatTime(seconds) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

export function VoiceEndpointsCard({ mode, overview, busy, run }) {
  const [extra, setExtra] = useState("");
  const [extraError, setExtraError] = useState("");
  if (!overview || mode === "builtin") return null;
  const endpoints = Array.isArray(overview.endpoints) ? overview.endpoints : [];
  const max = overview.max_extra_endpoints || 4;

  const addExtra = async () => {
    const error = extraEndpointError(extra, endpoints, max);
    setExtraError(error);
    if (error) return;
    const result = await run("添加接入点", () => voiceApi.saveSettings({ add_extra_mcp_endpoint: extra.trim() }));
    if (result) setExtra("");
  };

  const removeExtra = async (endpoint) => {
    const ok = await showConfirm(`删除${endpointLabel(endpoint.id)}（${endpoint.endpoint}）？MOSS 会断开它并不再连接。`, {
      title: "删除接入点",
      tone: "warning",
      confirmText: "删除",
    });
    if (ok) await run("删除接入点", () => voiceApi.saveSettings({ remove_extra_mcp_endpoint: endpoint.id }));
  };

  const connected = endpoints.filter((endpoint) => endpoint.connected).length;
  return (
    <VoiceGroup id="endpoints" title="接入点状态" dirty={Boolean(extra.trim())} summary={endpoints.length ? `${connected} / ${endpoints.length} 已连接` : "未配置"} tone={endpoints.length && connected === endpoints.length ? "ok" : undefined}>
      <div id="voice-endpoints-card" className="voice-stack">
      <p className="voice-muted">
        MOSS 作为工具提供方连接这些小智接入点，断线后自动重连。
        {overview.extras_paused && " 当前为自建模式：附加接入点暂停连接。"}
      </p>
      <div className="ob-tbl voice-tbl">
        <table>
          <thead>
            <tr>
              <th>接入点</th>
              <th>地址与身份</th>
              <th>状态</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {endpoints.length === 0 ? (
              <tr>
                <td colSpan={4} className="voice-muted">未配置接入点：在“服务”标签的“官方接入点”中填写</td>
              </tr>
            ) : (
              endpoints.map((endpoint) => {
                const view = endpointView(endpoint, overview.extras_paused);
                const identity = identityView(endpoint.identity);
                return (
                  <tr key={endpoint.id} id={`voice-endpoint-${endpoint.id}`}>
                    <td>{endpointLabel(endpoint.id)}</td>
                    <td>
                      <div className="mono voice-break">{endpoint.endpoint}</div>
                      {identity.text && <div className={identity.expired ? "voice-bad" : "voice-muted"}>{identity.text}</div>}
                      {endpoint.connected && endpoint.connected_at && <div className="voice-muted">本次连接于 {formatTime(endpoint.connected_at)}</div>}
                    </td>
                    <td>
                      <span className={`ob-pill ${TONE_CLASS[view.tone] || TONE_CLASS.off}`}>{view.text}</span>
                      {view.detail && <div className="voice-muted">{view.detail}</div>}
                    </td>
                    <td>
                      {endpoint.id !== "primary" && (
                        <button type="button" className="btn btn-ghost" onClick={() => removeExtra(endpoint)} disabled={Boolean(busy)} title="删除这个附加接入点">
                          <Trash2 size={13} /> 删除
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
      <div className="voice-form-grid">
        <label className="voice-wide">
          <span>添加附加接入点（同一账号的其他智能体或别的账号，最多 {max} 个，添加后立即连接）</span>
          <input
            className="text-input mono"
            value={extra}
            onChange={(e) => setExtra(e.target.value)}
            placeholder="wss://…/mcp/?token=…"
            spellCheck="false"
            autoComplete="off"
            id="voice-extra-endpoint"
          />
        </label>
      </div>
      <div className="voice-actions">
        <button type="button" className="btn btn-ghost" onClick={addExtra} disabled={Boolean(busy) || !extra.trim()} id="voice-extra-endpoint-add">
          <Plus size={13} /> 添加并连接
        </button>
        {extraError && <span className="voice-bad">{extraError}</span>}
      </div>

      </div>
    </VoiceGroup>
  );
}

export function VoiceToolsCard({ overview, busy, run }) {
  const [copiedPrompt, setCopiedPrompt] = useState("");
  if (!overview) return null;
  const tools = Array.isArray(overview.tools) ? overview.tools : [];

  const switchTool = (tool, enabled) =>
    run("切换工具", () => voiceApi.saveSettings({ disabled_tools: toolSwitchPayload(tools, tool.name, enabled) }));

  // 内置工具是编译进 MOSS 的原生工具，界面只能开关；增删要改代码，所以给一段可复制的施工提示词交给 AI。
  const copyPrompt = async (kind) => {
    try {
      await navigator.clipboard.writeText(toolEditPrompt(kind));
      setCopiedPrompt(kind);
    } catch {
      setCopiedPrompt("error");
    }
  };

  const disabled = tools.filter((tool) => tool.enabled === false).length;
  return (
    <VoiceGroup id="tools" title="MOSS 内置工具（只能开关，不能在这里增删）" summary={disabled ? `${disabled} 个工具已关闭` : `${tools.length} 个工具全部开启`}>
      <p className="voice-muted">勾选立即生效。关闭的工具不提供给任何接入点和内置语音；停止与急停工具不能关闭。统计为本次运行以来。</p>
      <p className="voice-muted"><b>和下面的「第三方 MCP 工具」不是一回事</b>：这里是编译在 MOSS 里的原生工具（激光、画板、材料库等），由 MOSS 自己执行，所以只能开关；第三方 MCP 是独立的外部程序，能在那里随时添加和移除。</p>
      <div className="ob-tbl voice-tbl">
        <table>
          <thead>
            <tr>
              <th>开关</th>
              <th>工具</th>
              <th>调用</th>
              <th>最近</th>
            </tr>
          </thead>
          <tbody>
            {tools.map((tool) => (
              <tr key={tool.name}>
                <td>
                  <input
                    type="checkbox"
                    checked={tool.enabled !== false}
                    disabled={Boolean(busy) || tool.can_disable === false}
                    onChange={(e) => switchTool(tool, e.target.checked)}
                    aria-label={`${tool.enabled !== false ? "关闭" : "打开"} ${tool.name}`}
                    title={tool.can_disable === false ? "停止与急停工具不能关闭" : ""}
                    id={`voice-tool-${tool.name}`}
                  />
                </td>
                <td>
                  <div className="mono">{tool.name}</div>
                  <div className="voice-muted">
                    {tool.description}
                    {tool.voice_hidden ? "（桌面控制类，内置语音与严格模式下不开放）" : ""}
                  </div>
                </td>
                <td>{toolStatText(tool)}</td>
                <td className="mono">{tool.last_at ? `${formatTime(tool.last_at)} ${tool.last_caller || ""}` : "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <details className="voice-builtin-models" id="voice-tool-prompts">
        <summary>要新增或删除内置工具？复制这两段提示词交给 AI</summary>
        <p className="voice-muted">内置工具的增删是改代码：复制下面的施工说明，粘给任意 AI 编码助手（Codex / Claude Code / Cursor 等），它会按仓库规则改代码、加测试、同步文档。这里只是给你复制用，不会自己执行。</p>
        {["add", "remove"].map((kind) => (
          <div className="voice-stack" key={kind} id={`voice-tool-prompt-${kind}`}>
            <div className="voice-actions">
              <span className="voice-muted">{TOOL_EDIT_PROMPTS[kind].title}</span>
              <button type="button" className="btn btn-ghost" onClick={() => copyPrompt(kind)} id={`voice-tool-prompt-copy-${kind}`}>
                <Copy size={13} /> 复制提示词
              </button>
              {copiedPrompt === kind && <span className="voice-ok" role="status">✓ 已复制</span>}
              {copiedPrompt === "error" && <span className="voice-bad">无法访问剪贴板，请手动选中下面文字复制</span>}
            </div>
            <pre className="voice-prompt mono">{TOOL_EDIT_PROMPTS[kind].text}</pre>
          </div>
        ))}
      </details>
    </VoiceGroup>
  );
}
