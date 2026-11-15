// crate 分层门禁：真实仓库必须零违规，规则本身用合成清单覆盖正反两面。
import { test } from "node:test";
import assert from "node:assert/strict";
import { LAYERS, checkLayers, edgeViolation, parseManifest } from "../check-crate-layers.mjs";

const known = new Set(Object.keys(LAYERS));

test("当前仓库的 crate 依赖符合分层规则", () => {
  assert.deepEqual(checkLayers(), []);
});

test("解析正式依赖、平台依赖、测试依赖和表头形式", () => {
  const parsed = parseManifest(
    `[package]
name = "moss-voice-server"
[dependencies]
moss-voice-providers = { path = "../moss-voice-providers" } # 注释
serde = "1"
[target.'cfg(windows)'.dependencies]
moss-xiaozhi-proto.workspace = true
[dependencies.laser-runtime]
path = "../laser-runtime"
[dev-dependencies]
laser-api = { path = "../laser-api" }
`,
    known,
  );
  assert.equal(parsed.name, "moss-voice-server");
  assert.deepEqual(parsed.normal.sort(), ["laser-runtime", "moss-voice-providers", "moss-xiaozhi-proto"]);
  assert.deepEqual(parsed.dev, ["laser-api"]);
});

test("拒绝跨业务域、向上、同层与依赖应用壳", () => {
  assert.match(edgeViolation("moss-voice-server", "laser-api", false), /跨业务域/);
  assert.match(edgeViolation("laser-core", "laser-host", false), /向上依赖/);
  assert.match(edgeViolation("laser-runtime", "moss-voice", false), /向上依赖/);
  assert.match(edgeViolation("laser-contracts", "moss-mcp-client", false), /同层依赖/);
  assert.match(edgeViolation("laser-host", "moss-desktop", false), /应用壳/);
  assert.match(edgeViolation("laser-api", "moss-host", true), /应用壳/);
  assert.match(edgeViolation("laser-api", "new-crate", false), /未登记/);
});

test("允许向下、同域与测试依赖跨域", () => {
  assert.equal(edgeViolation("laser-api", "laser-core", false), null);
  assert.equal(edgeViolation("moss-voice-server", "laser-runtime", false), null);
  assert.equal(edgeViolation("moss-voice", "laser-contracts", false), null);
  assert.equal(edgeViolation("laser-host", "moss-voice", false), null);
  assert.equal(edgeViolation("moss-voice-server", "laser-api", true), null);
});
