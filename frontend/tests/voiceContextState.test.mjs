// NV19 private-page helpers; synthetic values only.
import test from "node:test";
import assert from "node:assert/strict";
import { CATEGORY_SCOPES, NEEDS_DEVICE, experienceDevices, linesToList, listToLines, rowBadge, scopeLabel, scopeOptions, skipReasonText } from "../src/voiceContextState.js";

test("NV19 每类记忆的范围与 Rust 契约一致", () => {
  assert.deepEqual(CATEGORY_SCOPES.task_context, ["project"]);
  assert.deepEqual(CATEGORY_SCOPES.troubleshooting_experience, ["device"]);
  assert.ok(NEEDS_DEVICE.has("device_fact") && !NEEDS_DEVICE.has("preference"));
});

test("NV19 状态胶囊：作废、过期优先于确认", () => {
  assert.equal(rowBadge({ verification: "confirmed", state: "active" }).text, "已确认");
  assert.equal(rowBadge({ verification: "candidate", state: "active" }).text, "候选（未确认）");
  assert.equal(rowBadge({ verification: "confirmed", state: "expired" }).text, "已过期");
  assert.equal(rowBadge({ verification: "invalidated", state: "invalidated" }).text, "已作废");
});

test("NV19 范围、设备与表单转换", () => {
  const status = { devices: [{ id: "aa", name: "客厅小智", model: "M", firmware: "1" }, { id: "bb", name: "", model: "", firmware: "" }], project: { id: "p-1", name: "杯垫" } };
  assert.deepEqual(scopeOptions(status), [{ kind: "device", id: "aa" }, { kind: "device", id: "bb" }, { kind: "project", id: "p-1" }]);
  assert.deepEqual(scopeOptions({ devices: [] }), []);
  assert.equal(scopeLabel({ kind: "device", id: "aa" }, status), "设备：客厅小智");
  assert.equal(scopeLabel({ kind: "device", id: "bb" }, status), "设备：bb");
  assert.equal(scopeLabel({ kind: "project", id: "p-1" }, status), "项目：杯垫");
  assert.deepEqual(experienceDevices(status).map((d) => d.id), ["aa"]);
  assert.deepEqual(linesToList(" a \r\n\n b\n"), ["a", "b"]);
  assert.equal(listToLines(["a", "b"]), "a\nb");
  assert.equal(skipReasonText("op_not_allowed"), "模型试图修改或删除（不允许）");
  assert.equal(skipReasonText("unknown_code"), "unknown_code");
});
