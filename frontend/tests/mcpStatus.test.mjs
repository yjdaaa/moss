import test from "node:test";
import assert from "node:assert/strict";
import { mcpPillState, MCP_PILL_STATES } from "../src/state/mcpStatus.js";

test("A-73: 小智胶囊四种状态只按真实字段判定", () => {
  assert.equal(mcpPillState(null).key, "unconfigured");
  assert.equal(mcpPillState({ configured: false, connected: true }).key, "unconfigured");
  assert.equal(mcpPillState({ configured: true }).key, "disconnected");
  assert.equal(mcpPillState({ configured: true, connected: false }).key, "disconnected");
  assert.equal(mcpPillState({ configured: true, connected: "yes" }).key, "disconnected");
  assert.equal(mcpPillState({ configured: true, connected: true }, { mode: "official", device_sessions: null }).key, "connected");
  // 官方链路无法得知设备会话：连上即已连接，不臆断为“等待设备”
  assert.equal(mcpPillState({ configured: true, connected: true }, { mode: "official", device_sessions: 0 }).key, "connected");
  // 2026-11-17 删除自建链路：遗留的 self_hosted 按官方链路判定
  assert.equal(mcpPillState({ configured: true, connected: true }, { mode: "self_hosted", device_sessions: 0 }).key, "connected");
  assert.deepEqual(MCP_PILL_STATES, ["unconfigured", "disconnected", "waiting_device", "connected"]);
});

test("NV5: 内置链路只看内置服务上报的设备会话数，与小智桥无关", () => {
  const builtin = (sessions) => ({ mode: "builtin", device_sessions: sessions });
  assert.equal(mcpPillState({ configured: false }, builtin(null)).key, "disconnected", "服务未运行");
  assert.equal(mcpPillState({ configured: true, connected: true }, builtin(0)).key, "waiting_device");
  assert.equal(mcpPillState({ configured: false }, builtin(1)).key, "connected", "未配置官方接入点也不影响");
  assert.equal(mcpPillState(null, builtin(2)).text, "小智 · 内置 · 已连接");
});

test("A-73: 文案标注官方或内置链路", () => {
  assert.equal(mcpPillState({ configured: true, connected: true }).text, "小智 · 官方 · 已连接");
  assert.equal(mcpPillState({ configured: true, connected: false }, { mode: "self_hosted" }).text, "小智 · 官方 · 未连接");
  assert.equal(mcpPillState({ configured: false }).text, "小智 · 未配置");
});
