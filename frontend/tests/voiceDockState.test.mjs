import test from "node:test";
import assert from "node:assert/strict";
import {
  VOICE_DOCK_COLLAPSED_KEY,
  VOICE_DOCK_POLL_MS,
  VOICE_DOCK_TOOL_LINGER_MS,
  activeLaserTool,
  jobLine,
  laserToolLabel,
  nextRecentTool,
  readVoiceDockCollapsed,
  shouldPollVoiceSessions,
  toolLine,
  voiceDockView,
  voicePhase,
  voiceSectionAvailable,
  writeVoiceDockCollapsed,
} from "../src/voiceDockState.js";

const builtinOnline = { mode: "builtin", device_sessions: 1, phase: "idle", level: 0 };

test("小智雕刻同步：激光工具名映射为中文，非激光工具不显示（2026-11-16）", () => {
  assert.equal(VOICE_DOCK_POLL_MS, 1500);
  assert.equal(laserToolLabel("laser_workflow_tool"), "雕刻任务");
  assert.equal(laserToolLabel("check_laser_connection_tool"), "检查机器连接");
  assert.equal(laserToolLabel("laser_safe_action_tool"), "停止 / 调速");
  assert.equal(laserToolLabel("material_params_tool"), "查材料参数");
  assert.equal(laserToolLabel("regenerate_text_laser_task_tool"), "重新生成预览");
  assert.equal(laserToolLabel("run_calibration_grid_tool"), "测试矩阵");
  assert.equal(laserToolLabel(" laser_workflow_tool "), "雕刻任务");
  for (const removed of [
    "laser_grbl_tool",
    "laser_network_grbl_tool",
    "recommend_laser_params_tool",
    "generate_text_laser_task_tool",
    "start_tuned_job_tool",
  ]) {
    assert.equal(laserToolLabel(removed), "", `已移除工具 ${removed} 不再保留映射`);
  }
  assert.equal(laserToolLabel("open_app_tool"), "");
  assert.equal(laserToolLabel("toString"), "", "原型链上的名字不能被当成工具");
  assert.equal(laserToolLabel(null), "");
  assert.equal(laserToolLabel(42), "");
});

test("小智雕刻同步：语音部分只在桌面端、内置链路且有小智在线时显示", () => {
  assert.equal(voicePhase("thinking"), "thinking");
  assert.equal(voicePhase(null), "idle");
  assert.equal(voicePhase("weird"), "idle");

  assert.equal(voiceSectionAvailable({ voiceEnabled: true, voiceStatus: builtinOnline }), true);
  assert.equal(voiceSectionAvailable({ voiceEnabled: false, voiceStatus: builtinOnline }), false, "浏览器 / 局域网没有语音接口");
  assert.equal(voiceSectionAvailable({ voiceEnabled: true, voiceStatus: { ...builtinOnline, mode: "official" } }), false);
  assert.equal(voiceSectionAvailable({ voiceEnabled: true, voiceStatus: { ...builtinOnline, device_sessions: 0 } }), false);
  assert.equal(voiceSectionAvailable({ voiceEnabled: true, voiceStatus: { ...builtinOnline, device_sessions: null } }), false);
  assert.equal(voiceSectionAvailable({ voiceEnabled: true, voiceStatus: null }), false);

  // 空闲时后端已清空工具名，不轮询；非空闲才读会话
  assert.equal(shouldPollVoiceSessions({ voiceEnabled: true, voiceStatus: builtinOnline }), false);
  assert.equal(shouldPollVoiceSessions({ voiceEnabled: true, voiceStatus: { ...builtinOnline, phase: "thinking" } }), true);
  assert.equal(shouldPollVoiceSessions({ voiceEnabled: false, voiceStatus: { ...builtinOnline, phase: "thinking" } }), false);
  assert.equal(shouldPollVoiceSessions({ voiceEnabled: true, voiceStatus: { ...builtinOnline, phase: "speaking" }, visible: false }), false);
});

test("小智雕刻同步：只从会话里取状态与工具名，优先正在说 / 想的会话", () => {
  assert.equal(activeLaserTool(null), "");
  assert.equal(activeLaserTool({ sessions: "bad" }), "");
  assert.equal(
    activeLaserTool({
      sessions: [
        { state: "listening", active_tool: "material_params_tool" },
        { state: "thinking", active_tool: "laser_workflow_tool" },
        { state: "speaking", active_tool: "open_app_tool" },
      ],
      debug_traces: [{ raw_asr: "帮我刻字", reply: "好的" }],
    }),
    "雕刻任务",
  );
  assert.equal(activeLaserTool({ sessions: [{ state: "thinking", active_tool: null }] }), "");

  const first = nextRecentTool(null, "雕刻任务", 1000);
  assert.deepEqual(first, { label: "雕刻任务", at: 1000 });
  assert.equal(nextRecentTool(first, "", 5000), first, "没有新工具时保留上一次");
  assert.deepEqual(nextRecentTool(first, "生成预览", 6000), { label: "生成预览", at: 6000 });
});

test("小智雕刻同步：工具行区分正在调用与刚才调用，过期后消失", () => {
  const recent = { label: "雕刻任务", at: 10_000 };
  assert.deepEqual(toolLine({ phase: "thinking", currentTool: "雕刻任务", recentTool: recent, now: 11_000 }), { active: true, text: "正在调用：雕刻任务" });
  assert.deepEqual(toolLine({ phase: "idle", currentTool: "雕刻任务", recentTool: recent, now: 11_000 }), { active: false, text: "刚才调用：雕刻任务" });
  assert.deepEqual(toolLine({ phase: "idle", recentTool: recent, now: 10_000 + VOICE_DOCK_TOOL_LINGER_MS }), { active: false, text: "刚才调用：雕刻任务" });
  assert.equal(toolLine({ phase: "idle", recentTool: recent, now: 10_001 + VOICE_DOCK_TOOL_LINGER_MS }), null);
  assert.equal(toolLine({ phase: "listening" }), null);
});

test("小智雕刻同步：雕刻进度直接用任务浮层视图，不编造百分比", () => {
  assert.equal(jobLine([]), null);
  assert.equal(jobLine(null), null);
  assert.deepEqual(jobLine([{ percent: 37, unknown: false, phaseText: "正在发送 G-code" }]), {
    percent: 37,
    short: "37%",
    text: "雕刻 37% · 正在发送 G-code",
  });
  assert.deepEqual(jobLine([{ percent: null, unknown: true, phaseText: "进度未知" }]), {
    percent: null,
    short: "雕刻中",
    text: "雕刻 进度未知 · 进度未知",
  });
  assert.equal(jobLine([{ percent: 1 }, { percent: 2 }]).text, "2 个雕刻任务进行中");
});

test("小智雕刻同步：整体视图在各状态下的显示与隐藏", () => {
  // 什么都没有：隐藏
  assert.equal(voiceDockView().visible, false);
  // 只有雕刻任务、没有语音：隐藏（雕刻任务浮层已完整显示）
  const jobs = [{ percent: 40, unknown: false, phaseText: "正在发送 G-code" }];
  assert.equal(voiceDockView({ voiceEnabled: false, voiceStatus: builtinOnline, jobs }).visible, false);

  // 小智在线、空闲
  const idle = voiceDockView({ voiceEnabled: true, voiceStatus: builtinOnline });
  assert.equal(idle.visible, true);
  assert.equal(idle.voice.phaseText, "待命");
  assert.equal(idle.voice.devicesText, "1 台小智在线");
  assert.equal(idle.tool, null);
  assert.equal(idle.job, null);
  assert.equal(idle.pillText, "小智待命");
  assert.equal(idle.attention, false);

  // 小智思考并调用雕刻工具，同时有任务在跑
  const busy = voiceDockView({
    voiceEnabled: true,
    voiceStatus: { ...builtinOnline, device_sessions: 2, phase: "thinking" },
    currentTool: "雕刻任务",
    jobs,
    now: 1,
  });
  assert.equal(busy.voice.phase, "thinking");
  assert.equal(busy.voice.devicesText, "2 台小智在线");
  assert.equal(busy.tool.text, "正在调用：雕刻任务");
  assert.equal(busy.job.text, "雕刻 40% · 正在发送 G-code");
  assert.equal(busy.pillText, "小智思考中 · 雕刻任务 · 40%");

  // 浏览器模式只剩待确认预览：只显示入口，不显示语音
  const preview = voiceDockView({ voiceEnabled: false, voiceStatus: null, previewWaiting: 1, jobs });
  assert.equal(preview.visible, true);
  assert.equal(preview.voice, null);
  assert.equal(preview.job, null);
  assert.equal(preview.previewText, "有待确认的预览");
  assert.equal(preview.attention, true);
  assert.equal(preview.pillText, "预览待确认");
  assert.equal(voiceDockView({ previewWaiting: 3 }).previewText, "3 个预览待确认");

  const approval = voiceDockView({ voiceEnabled: true, voiceStatus: builtinOnline, approvalWaiting: 2 });
  assert.equal(approval.approvalText, "2 个请求待批准");
  assert.equal(approval.pillText, "小智待命 · 待批准");

  // 视图里没有任何对话原文字段
  assert.deepEqual(Object.keys(busy).sort(), ["approvalText", "attention", "job", "pillText", "previewText", "tool", "visible", "voice"]);
});

test("小智雕刻同步：折叠状态记在本机，存储不可用时不抛错", () => {
  const map = new Map();
  const storage = { getItem: (k) => (map.has(k) ? map.get(k) : null), setItem: (k, v) => map.set(k, v) };
  assert.equal(readVoiceDockCollapsed(storage), false);
  writeVoiceDockCollapsed(true, storage);
  assert.equal(map.get(VOICE_DOCK_COLLAPSED_KEY), "1");
  assert.equal(readVoiceDockCollapsed(storage), true);
  writeVoiceDockCollapsed(false, storage);
  assert.equal(readVoiceDockCollapsed(storage), false);

  const broken = {
    getItem: () => {
      throw new Error("blocked");
    },
    setItem: () => {
      throw new Error("blocked");
    },
  };
  assert.equal(readVoiceDockCollapsed(broken), false);
  assert.doesNotThrow(() => writeVoiceDockCollapsed(true, broken));
  assert.equal(readVoiceDockCollapsed(undefined), false);
});
