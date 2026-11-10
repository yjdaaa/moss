// NV19 / n50 memory scope & source helpers; synthetic values only.
import test from "node:test";
import assert from "node:assert/strict";
import { SOURCE_LABEL, scopeLabel, scopeOptions } from "../src/voiceContextState.js";

test("NV19 记忆来源标签与范围选项", () => {
  assert.equal(SOURCE_LABEL.user_edit, "用户填写");
  assert.equal(SOURCE_LABEL.conversation_extraction, "对话提取");
  const status = {
    devices: [
      { id: "aa", name: "客厅小智", model: "M", firmware: "1" },
      { id: "bb", name: "", model: "", firmware: "" },
    ],
    persons: [{ id: "sp-1", name: "小明" }],
    project: { id: "p-1", name: "杯垫" },
  };
  assert.deepEqual(scopeOptions(status), [
    { kind: "device", id: "aa" },
    { kind: "device", id: "bb" },
    { kind: "person", id: "sp-1" },
    { kind: "project", id: "p-1" },
  ]);
  assert.deepEqual(scopeOptions({ devices: [] }), []);
  assert.equal(scopeLabel({ kind: "device", id: "aa" }, status), "设备：客厅小智");
  assert.equal(scopeLabel({ kind: "device", id: "bb" }, status), "设备：bb");
  assert.equal(scopeLabel({ kind: "person", id: "sp-1" }, status), "个人：小明");
  assert.equal(scopeLabel({ kind: "project", id: "p-1" }, status), "项目：杯垫");
});
