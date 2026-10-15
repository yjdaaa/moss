import React, { useState } from "react";
import { Plus, Trash2 } from "lucide-react";
import { VoiceGroup } from "./VoiceLayout.jsx";
import { showConfirm } from "../appDialog.js";
import { voiceApi } from "../voiceApi.js";
import { endpointLabel, endpointView, extraEndpointError, identityView, toolStatText, toolSwitchPayload } from "../voiceState.js";

// 接入点与工具（内置语音技术书 NV8）：附加官方接入点、各接入点连接状态与身份、工具开关与调用统计。
// 地址一律由后端脱敏后返回；新增时填写的完整地址只在本页内存中，提交后清空。

const TONE_CLASS = { ok: "ob-pill--ok", info: "ob-pill--info", warn: "ob-pill--warn", bad: "ob-pill--bad", off: "ob-pill--off" };

function formatTime(seconds) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

export function VoiceEndpointsCard({ overview, busy, run }) {
  const [extra, setExtra] = useState("");
  const [extraError, setExtraError] = useState("");
  if (!overview) return null;
  const endpoints = Array.isArray(overview.endpoints) ? overview.endpoints : [];
  const tools = Array.isArray(overview.tools) ? overview.tools : [];
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

  const switchTool = (tool, enabled) =>
    run("切换工具", () => voiceApi.saveSettings({ disabled_tools: toolSwitchPayload(tools, tool.name, enabled) }));

  const connected = endpoints.filter((endpoint) => endpoint.connected).length;
  const disabled = tools.filter((tool) => tool.enabled === false).length;
  return (
    <>
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
                <td colSpan={4} className="voice-muted">未配置接入点：在上面的“官方接入点”中填写</td>
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
    <VoiceGroup id="tools" title="工具开关与调用统计" summary={disabled ? `${disabled} 个工具已关闭` : `${tools.length} 个工具全部开启`}>
      <p className="voice-muted">勾选立即生效。关闭的工具不提供给任何接入点和内置语音；停止与急停工具不能关闭。统计为本次运行以来。</p>
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
    </VoiceGroup>
    </>
  );
}
