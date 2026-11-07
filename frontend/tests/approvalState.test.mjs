import assert from "node:assert/strict";
import test from "node:test";
import {
  approvalErrorText,
  canApprove,
  clockOffsetSeconds,
  newPendingIds,
  orderPending,
  secondsLeft,
  shortDigest,
  sourceLabel,
  targetText,
  workflowDetails,
} from "../src/approvalState.js";

test("approve button needs the on-site check, time left, and no failure (A-71)", () => {
  assert.equal(canApprove({ checked: false, busy: false, failed: false, remaining: 100 }), false);
  assert.equal(canApprove({ checked: true, busy: false, failed: false, remaining: 100 }), true);
  assert.equal(canApprove({ checked: true, busy: true, failed: false, remaining: 100 }), false);
  assert.equal(canApprove({ checked: true, busy: false, failed: true, remaining: 100 }), false);
  assert.equal(canApprove({ checked: true, busy: false, failed: false, remaining: 0 }), false);
});

test("countdown follows the server clock", () => {
  const nowMs = 1_000_000;
  const offset = clockOffsetSeconds(2000, nowMs); // 服务端比本机快 1000 秒
  assert.equal(offset, 1000);
  assert.equal(secondsLeft(2120, offset, nowMs), 120);
  assert.equal(secondsLeft(2000.2, offset, nowMs), 1);
  assert.equal(secondsLeft(1990, offset, nowMs), 0);
  assert.equal(secondsLeft(undefined, offset, nowMs), 0);
  assert.equal(clockOffsetSeconds(undefined, nowMs), 0);
});

test("mismatch and expiry show fixed reasons; unknown codes keep the backend text", () => {
  assert.match(approvalErrorText("approval_mismatch"), /摘要不一致/);
  assert.match(approvalErrorText("approval_mismatch"), /没有向设备发送任何指令/);
  assert.match(approvalErrorText("approval_expired"), /过期/);
  assert.equal(approvalErrorText("device_unreachable", "设备连接失败"), "设备连接失败");
  assert.equal(approvalErrorText(null, ""), "批准未完成。");
});

test("target never shows an address and digest is shortened", () => {
  assert.equal(targetText({ mode: "network", label: "网络设备" }), "网络 · 网络设备");
  assert.equal(targetText({ mode: "serial", label: "COMX" }), "串口 · COMX");
  assert.equal(targetText(null), "未知设备");
  assert.equal(shortDigest("a3f9" + "0".repeat(56) + "c41e"), "a3f9…c41e");
  assert.equal(sourceLabel("mcp_bridge"), "小智语音（MCP 接入点）");
  assert.equal(sourceLabel("lan", "pair_abcdef"), "局域网配对设备 · 会话 …cdef");
  assert.equal(sourceLabel("voice", "aa:bb:cc:dd:ee:ff"), "小智语音（MOSS 内置） · 设备 …e:ff");
});

test("pending list is ordered oldest first and new arrivals are detected", () => {
  const list = [
    { approval_id: "apr_b", status: "pending", created_at: 20 },
    { approval_id: "apr_a", status: "pending", created_at: 10 },
    { approval_id: "apr_c", status: "expired", created_at: 5 },
  ];
  assert.deepEqual(orderPending(list).map((item) => item.approval_id), ["apr_a", "apr_b"]);
  assert.deepEqual(newPendingIds(new Set(["apr_a"]), list), ["apr_b"]);
  assert.deepEqual(newPendingIds(new Set(), null), []);
});

test("workflow details are read from the record without inventing values", () => {
  const details = workflowDetails({
    source_type: "text",
    input: { text: "你好", material: "椴木板", thickness_mm: 3 },
    artifacts: {
      actual_params: { laser_max_power: 300, power_scale: 1000, feed_rate: 1500, passes: 1 },
      width_mm: 40, height_mm: 20, time_estimate: { estimated_seconds: 240 },
      image_file: "generated/a.png",
    },
  });
  assert.equal(details.content, "文字“你好”");
  assert.equal(details.material, "椴木板 · 3 mm");
  assert.equal(details.power, "300/1000（30%）");
  assert.equal(details.powerPercent, 30);
  assert.equal(details.feed, "1500 mm/min");
  assert.equal(details.passes, "1 遍");
  assert.equal(details.size, "40 × 20 mm");
  assert.equal(details.duration, "约 4 分钟");
  assert.equal(details.previewPath, "generated/a.png");
  const empty = workflowDetails({});
  assert.equal(empty.power, null);
  assert.equal(empty.material, null);
  assert.equal(empty.duration, null);
});

test("家居控制要在语音中心家居审批里批准，全局审批卡只提醒并跳到那里（2026-10-14，n67 起不再开独立窗口）", async () => {
  const { approvedInPrivateWindow, kindLabel } = await import("../src/approvalState.js");
  assert.equal(approvedInPrivateWindow("home_control"), true);
  for (const kind of ["workflow_confirm_send", "calibration_send", "safe_action", "framing", "external_tool"]) {
    assert.equal(approvedInPrivateWindow(kind), false, kind);
  }
  assert.equal(kindLabel("home_control"), "控制家居设备");
});

test("Agent 审批卡用任务确认文案，不用加工现场确认文案（2026-10-15）", async () => {
  const { approvalCheckText, approvalErrorText, approvedInPrivateWindow, kindLabel, SITE_CHECK_TEXT } = await import("../src/approvalState.js");
  assert.match(approvalCheckText("agent_run"), /同意在本机工作目录里运行 Agent/);
  assert.notEqual(approvalCheckText("agent_run"), SITE_CHECK_TEXT, "Agent 不是激光机现场作业");
  assert.match(approvalCheckText("workflow_confirm_send"), /材料已放好/);
  assert.equal(kindLabel("agent_run"), "运行本机 Agent（Claude Code / Codex）");
  assert.equal(approvedInPrivateWindow("agent_run"), false);
  assert.match(approvalErrorText("agent_strict_mode_required", "后端原话"), /严格模式/);
  assert.equal(approvalErrorText("something_new", "后端原话"), "后端原话");
});

test("Agent 审批徽标与任务预览字段及家居审批拒绝优先撤销协调器挂起态", async () => {
  const { approvalTagText, agentApprovalPreview, rejectApprovalRequest } = await import("../src/approvalState.js");
  assert.equal(approvalTagText("agent_run"), "本机 Agent · 本次批准");
  assert.equal(approvalTagText("external_tool"), "第三方工具 · 本次批准");
  assert.equal(approvalTagText("home_control"), "家居控制 · 在家居审批里批准");
  assert.equal(approvalTagText("workflow_confirm_send"), "R3 · 运动与出光");

  const preview = agentApprovalPreview({
    provider: "codex",
    workspace_label: "demo-project",
    task: "修复 README 拼写错误",
  });
  assert.match(preview.providerLabel, /codex/);
  assert.equal(preview.workspaceLabel, "demo-project");
  assert.equal(preview.task, "修复 README 拼写错误");
  assert.match(preview.warning, /修改/);
  assert.match(preview.warning, /额度/);

  const cancelledCalls = [];
  let postCalled = false;
  const homeOutcome = await rejectApprovalRequest(
    { approval_id: "apr-home-1", kind: "home_control", digest: "deadbeef" },
    {
      cancelHomeApproval: async (input) => {
        cancelledCalls.push(input);
        return { status: "rejected" };
      },
      postReject: async () => {
        postCalled = true;
        return { ok: true, data: { success: true } };
      },
    },
  );
  assert.equal(homeOutcome.ok, true);
  assert.equal(postCalled, false, "桌面端 Tauri 可用时直接走 homeApprovalCancel 立即作废内存挂起与 send_fence");
  assert.deepEqual(cancelledCalls, [{ approval_id: "apr-home-1", digest: "deadbeef", confirmed: true }]);

  let fallbackUrl = "";
  const fallbackOutcome = await rejectApprovalRequest(
    { approval_id: "apr-home-2", kind: "home_control", digest: "beadfeed" },
    {
      cancelHomeApproval: async () => {
        const err = new Error("语音中心仅在 MOSS 桌面端可用");
        err.code = "voice_desktop_only";
        throw err;
      },
      postReject: async (url) => {
        fallbackUrl = url;
        return { ok: true, data: { success: true } };
      },
    },
  );
  assert.equal(fallbackOutcome.ok, true);
  assert.equal(fallbackUrl, "/v1/approvals/apr-home-2/reject");
});
