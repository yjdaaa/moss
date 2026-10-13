import React, { useState } from "react";
import { Cable, Plus, Trash2, Wrench } from "lucide-react";
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

  return (
    <section className="ob-card" id="voice-endpoints-card">
      <div className="ob-card-h">
        <h2>
          <Cable size={16} aria-hidden="true" /> 接入点与工具
        </h2>
        <span className="ob-k">ENDPOINTS · TOOLS</span>
      </div>
      <p className="voice-muted">
        MOSS 以工具提供方身份连接下列小智接入点，同一套工具同时提供给它们；断线后自动重连（1 秒起逐次加倍，最长 60 秒；90 秒收不到任何数据也判定断线）。
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
                <td colSpan={4} className="voice-muted">未配置接入点：在“服务端与接入点设置”中填写官方接入点</td>
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
          <span>添加附加接入点（同一账号的其他智能体，或别的账号；最多 {max} 个。主接入点在“服务端与接入点设置”中修改）</span>
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
          <Plus size={13} /> 添加
        </button>
        {extraError && <span className="voice-bad">{extraError}</span>}
      </div>

      <h3 className="voice-subhead">
        <Wrench size={14} aria-hidden="true" /> 工具开关与调用统计
      </h3>
      <p className="voice-muted">
        关闭的工具不会提供给任何接入点和内置语音，调用也会被拒绝；改动后 MOSS 会重连接入点以更新工具列表。停止与急停工具不能关闭。统计为本次运行以来的数据。
      </p>
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
    </section>
  );
}
