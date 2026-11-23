// 仓库静态规范检查测试：真实仓库零违规，规则函数覆盖正反用例。
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  CHANGELOG_ITEM_LIMIT,
  checkChangelogEntries,
  checkCommitMessage,
  checkPromptBudget,
  checkPs1Ascii,
  checkRepoHygiene,
  checkRustInnerAllow,
  checkSensitiveContent,
  checkTrackedPathPolicy,
} from "../check-repo-hygiene.mjs";

test("当前仓库通过静态规范检查", () => {
  assert.deepEqual(checkRepoHygiene(), []);
});

test("PowerShell 脚本只允许纯 ASCII", () => {
  assert.deepEqual(checkPs1Ascii("scripts/ok.ps1", "$ErrorActionPreference = 'Stop'\r\nWrite-Host 'ok'\r\n"), []);
  const bad = checkPs1Ascii("scripts/bad.ps1", "Write-Host '中文提示'\r\n");
  assert.equal(bad.length, 1);
  assert.equal(bad[0].line, 1);
});

test("禁止 Rust 源码整文件 #![allow(clippy::...)]", () => {
  const allowPrefix = "#![allow(" + "clippy::";
  assert.deepEqual(
    checkRustInnerAllow("crates/laser-core/src/lib.rs", `#[expect(clippy::too_many_arguments, reason = "ok")]\n`),
    [],
  );
  assert.deepEqual(
    checkRustInnerAllow("crates/laser-core/src/lib.rs", `// 注释里的 ${allowPrefix}foo)]\n`),
    [],
  );
  const hits = checkRustInnerAllow("crates/laser-core/src/lib.rs", `${allowPrefix}too_many_arguments)]\n`);
  assert.equal(hits.length, 1);
  assert.equal(hits[0].line, 1);

  const buildHit = checkRustInnerAllow("apps/desktop/src-tauri/build.rs", `${allowPrefix}unused_imports)]\n`);
  assert.equal(buildHit.length, 1);

  const multiLineHit = checkRustInnerAllow(
    "crates/laser-core/benches/bench.rs",
    `// ok\n${"#![allow("}\n  dead_code,\n  clippy::too_many_arguments\n)]\n`,
  );
  assert.equal(multiLineHit.length, 1);
  assert.equal(multiLineHit[0].line, 2);
});

test("拒绝 .env、运行目录与私钥文件被跟踪", () => {
  assert.deepEqual(checkTrackedPathPolicy(".env.example"), []);
  assert.deepEqual(checkTrackedPathPolicy("crates/laser-api/.env.example"), []);
  assert.equal(checkTrackedPathPolicy(".env").length, 1);
  assert.equal(checkTrackedPathPolicy(".env.local").length, 1);
  assert.equal(checkTrackedPathPolicy(".runtime/moss.db").length, 1);
  assert.equal(checkTrackedPathPolicy("certs/dev.pem").length, 1);
});

test("拦截真实本机路径与凭据字符串，放行合成测试用户与 JSON Pointer", () => {
  const realMacPath = ["", "Users", "zhangsan", "repo"].join("/");
  const realWinPath = ["C:", "Users", "zhangsan", "repo"].join("\\");
  const realEscapedWinPath = ["C:", "Users", "zhangsan", "repo"].join("\\\\");
  const legacyGoldenPath = ["C:", "Users", "19454", "Desktop"].join("\\\\");
  const fakeKey = "sk-" + "aB3dE5fG7hJ9kL1mN3pQ5rS7tU";
  const fakeDpapi = "dpapi-" + "user-v1:" + "A".repeat(28);
  const demoHome = ["", "Users", "mycustomhome"].join("/");

  assert.equal(checkSensitiveContent("docs/developer/safety.md", `path=${realMacPath}`, { homeDir: demoHome }).length, 1);
  assert.equal(checkSensitiveContent("scripts/foo.mjs", `const p = "${realWinPath}";`, { homeDir: demoHome }).length, 1);
  assert.equal(checkSensitiveContent("scripts/foo.mjs", `const p = "${realEscapedWinPath}";`, { homeDir: demoHome }).length, 1);
  assert.equal(checkSensitiveContent("docs/developer/standards.md", `dir=${demoHome}`, { homeDir: demoHome }).length, 1);
  assert.equal(checkSensitiveContent("crates/laser-api/src/lib.rs", `const K: &str = "${fakeKey}";`).length, 1);
  assert.equal(checkSensitiveContent("docs/user/quick-start.md", fakeDpapi).length, 1);

  const syntheticMac = ["", "Users", "alice", "Library"].join("/");
  const syntheticWinMe = ["C:", "Users", "me", "Downloads"].join("\\\\");
  assert.deepEqual(checkSensitiveContent("crates/laser-runtime/src/root.rs", `Path::new("${syntheticMac}")`, { homeDir: demoHome }), []);
  assert.deepEqual(checkSensitiveContent("frontend/tests/saveFile.test.mjs", `"${syntheticWinMe}"`, { homeDir: demoHome }), []);
  assert.deepEqual(checkSensitiveContent("tests/golden/sample.json", `"${legacyGoldenPath}"`, { homeDir: demoHome }), []);
  assert.deepEqual(checkSensitiveContent("crates/moss-voice/src/settings/home_transfer.rs", `"/home/entities/0/enabled"`), []);
  assert.deepEqual(checkSensitiveContent("docs/developer/standards.md", "模型路径 `<path>` 与 `<device-ip>`"), []);
});

test("校验提示词字数预算", () => {
  const okDoc = [
    "## Console Prompt",
    "```text",
    "a".repeat(2000),
    "```",
    "## Knowledge Base Text",
    "```text",
    "b".repeat(200),
    "```",
  ].join("\n");
  assert.deepEqual(checkPromptBudget(okDoc), []);

  const overDoc = [
    "## Console Prompt",
    "```text",
    "测".repeat(2001),
    "```",
    "## Knowledge Base Text",
    "```text",
    "b".repeat(201),
    "```",
  ].join("\n");
  const errs = checkPromptBudget(overDoc);
  assert.equal(errs.length, 2);
  assert.match(errs[0].reason, /2001/);
  assert.match(errs[1].reason, /201/);
});

test("校验提交信息格式", () => {
  assert.equal(checkCommitMessage("fix: 修复串口重连状态同步\n\n# comment\n"), null);
  assert.equal(checkCommitMessage("feat!: 更新协议校验规则"), null);
  assert.equal(checkCommitMessage("Merge branch 'master' into feat/x"), null);
  assert.match(checkCommitMessage(""), /不能为空/);
  assert.match(checkCommitMessage("update docs"), /<类型>: <中文结论>/);
  assert.match(checkCommitMessage("feat: add new feature"), /中文结论/);
});

const changelog = (entries) => `# Changelog\n\n## [Unreleased]\n\n${entries}\n## [0.1.0] - 2026-01-01\n\n### 旧版本条目\n\n- **现象**：${"长".repeat(CHANGELOG_ITEM_LIMIT + 50)}\n`;
const entry = (title, items) => `### ${title}\n\n${items.map(([label, body]) => `- **${label}**：${body}`).join("\n")}\n\n`;
const FOUR = [["现象", "a"], ["根因与修复", "b"], ["验证", "c"], ["关联", "d"]];

test("CHANGELOG 新条目须依次为现象、根因与修复、验证、关联四项", () => {
  assert.deepEqual(checkChangelogEntries(changelog(entry("合规", FOUR))), []);
  const missing = checkChangelogEntries(changelog(entry("缺项", FOUR.slice(0, 3))));
  assert.equal(missing.length, 1);
  assert.match(missing[0].reason, /四项/);
  const reordered = checkChangelogEntries(changelog(entry("乱序", [FOUR[1], FOUR[0], FOUR[2], FOUR[3]])));
  assert.equal(reordered.length, 1);
});

test("CHANGELOG 新条目每项超过长度上限时报出行号，已发布段与已提交条目不检查", () => {
  const long = [["现象", "长".repeat(CHANGELOG_ITEM_LIMIT)], ...FOUR.slice(1)];
  const text = changelog(entry("超长", long));
  const violations = checkChangelogEntries(text);
  assert.equal(violations.length, 1);
  assert.match(violations[0].reason, /现象/);
  assert.equal(text.split("\n")[violations[0].line - 1].startsWith("- **现象**"), true);
  // 已提交（HEAD 里已有同名标题）的旧条目不再追溯
  assert.deepEqual(checkChangelogEntries(text, { knownHeadings: new Set(["### 超长"]) }), []);
});

test("CHANGELOG 根文件留有多个已发布版本段时提示运行归档脚本", () => {
  const one = changelog(entry("合规", FOUR));
  assert.deepEqual(checkChangelogEntries(one), []);
  const two = `${one}\n## [0.0.9] - 2025-12-01\n\n### 更旧\n`;
  const violations = checkChangelogEntries(two);
  assert.equal(violations.length, 1);
  assert.match(violations[0].reason, /archive-changelog\.mjs/);
});
