/**
 * 顶栏“小智”胶囊状态（技术书 §9.5.3，验收 A-73）。
 *
 * 只根据后端真实字段判定，不做任何猜测：
 * - 未配置：没有接入点地址（mcp.configured=false）
 * - 未连接：已配置，但桥当前没有连上接入点（mcp.connected=false）
 * - 等待设备：已连接，自建链路下设备会话数为 0（语音中心上线后才会出现）
 * - 已连接：已连接；官方链路无法得知设备会话，连上即视为已连接
 *
 * 内置链路（NV5）：设备直接连 MOSS 的内置语音服务，与小智桥无关，只看内置服务上报的设备会话数——
 * null 为服务未运行（未连接），0 为等待设备，大于 0 为已连接。
 */

export const MCP_PILL_STATES = ["unconfigured", "disconnected", "waiting_device", "connected"];

const LABELS = {
  unconfigured: "未配置",
  disconnected: "未连接",
  waiting_device: "等待设备",
  connected: "已连接",
};

export function mcpPillState(mcp, voice) {
  const link = voice?.mode === "self_hosted" ? "自建" : voice?.mode === "builtin" ? "内置" : "官方";
  let key;
  if (voice?.mode === "builtin") {
    const sessions = voice?.device_sessions;
    key = typeof sessions !== "number" ? "disconnected" : sessions > 0 ? "connected" : "waiting_device";
  } else if (!mcp?.configured) {
    key = "unconfigured";
  } else if (mcp.connected !== true) {
    key = "disconnected";
  } else if (voice?.mode === "self_hosted" && voice?.device_sessions === 0) {
    key = "waiting_device";
  } else {
    key = "connected";
  }
  const label = LABELS[key];
  return {
    key,
    link,
    label,
    text: key === "unconfigured" ? `小智 · ${label}` : `小智 · ${link} · ${label}`,
  };
}
