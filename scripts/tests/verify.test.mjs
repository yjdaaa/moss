// verify.mjs 计划生成与并发构建进程识别测试。
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildSpawnSpec, detectActiveBuildProcesses, planVerification, resolveStep } from "../verify.mjs";

test("仅改动前端时只跑静态门禁与前端检查测试构建", () => {
  const ids = planVerification(["frontend/src/pages/HomePage.jsx"]).map((s) => s.id);
  assert.deepEqual(ids, [
    "check-crate-layers",
    "check-repo-hygiene",
    "scripts-tests",
    "frontend-lint",
    "frontend-test",
    "frontend-build",
  ]);
});

test("改动 moss-voice-local-proto 时同时触发根工作区与独立 voice-local 检查", () => {
  const ids = planVerification(["crates/moss-voice-local-proto/src/lib.rs"]).map((s) => s.id);
  assert.ok(ids.includes("root-fmt"));
  assert.ok(ids.includes("root-clippy"));
  assert.ok(ids.includes("root-test"));
  assert.ok(ids.includes("helper-fmt"));
  assert.ok(ids.includes("helper-clippy"));
  assert.ok(ids.includes("helper-test"));
  assert.ok(!ids.includes("frontend-build"));
});

test("仅改动 moss-voice-local 时只检查独立工作区，不跑根工作区 cargo test", () => {
  const steps = planVerification(["apps/moss-voice-local/src/main.rs"]);
  const ids = steps.map((s) => s.id);
  assert.ok(!ids.includes("root-test"));
  assert.ok(ids.includes("helper-fmt"));
  assert.ok(ids.includes("helper-clippy"));
  assert.ok(ids.includes("helper-test"));
  const helperFmt = steps.find((s) => s.id === "helper-fmt");
  assert.equal(helperFmt.cmd, "rustfmt");
  assert.deepEqual(helperFmt.args, ["--edition", "2024", "--check", "src/main.rs"]);
});

test("--quick 跳过测试与构建，--fmt-write 只格式化改动的 .rs 文件", () => {
  const quickIds = planVerification(["crates/laser-core/src/lib.rs", "frontend/src/main.jsx"], {
    quick: true,
    fmtWrite: true,
  }).map((s) => s.id);
  assert.ok(quickIds.includes("rustfmt-write"));
  assert.ok(quickIds.includes("root-clippy"));
  assert.ok(quickIds.includes("frontend-lint"));
  assert.ok(!quickIds.includes("root-test"));
  assert.ok(!quickIds.includes("frontend-test"));
  assert.ok(!quickIds.includes("frontend-build"));
});

test("--full 覆盖根工作区、moss-voice-local 与前端全量检查", () => {
  const ids = planVerification([], { full: true }).map((s) => s.id);
  assert.ok(ids.includes("root-test"));
  assert.ok(ids.includes("helper-test"));
  assert.ok(ids.includes("frontend-build"));
});

test("识别并发构建进程并忽略自身", () => {
  const ps = [
    "1001 /usr/local/bin/node scripts/verify.mjs --full",
    "1002 cargo clippy --workspace --all-targets",
    "1003 node scripts/test_e2e.mjs",
    "1004 /Applications/Moss.app/Contents/MacOS/moss-desktop",
  ].join("\n");
  const hits = detectActiveBuildProcesses(ps, 1001);
  assert.deepEqual(hits.map((h) => h.pid), [1002, 1003]);
});

test("识别 Windows 风格的带路径、.exe 后缀与引号的构建进程命令行", () => {
  const ps = [
    "2001 node scripts/verify.mjs",
    "2002 \"D:\\toolchain\\.cargo\\bin\\cargo.exe\" test --workspace",
    "2003 C:\\Windows\\system32\\cmd.exe /d /s /c vite build",
    "2004 \"C:\\Program Files\\MOSS\\moss-desktop.exe\"",
    "2005 C:\\tools\\cargo-sweep.exe sweep --time 3",
  ].join("\r\n");
  assert.deepEqual(detectActiveBuildProcesses(ps, 2001).map((h) => h.pid), [2002, 2003]);
});

test("Windows 下将 .cmd 包装为 cmd.exe /d /c，防止 EINVAL", () => {
  assert.deepEqual(buildSpawnSpec("npm.cmd", ["--prefix", "frontend", "run", "lint"], "win32", "C:\\Windows\\System32\\cmd.exe"), {
    cmd: "C:\\Windows\\System32\\cmd.exe",
    args: ["/d", "/c", "npm.cmd", "--prefix", "frontend", "run", "lint"],
  });
  assert.deepEqual(buildSpawnSpec("cargo", ["test"], "win32", "cmd.exe"), {
    cmd: "cargo",
    args: ["test"],
  });
});

test("--browser 与 --e2e 计划包含前端 Playwright 浏览器交互测试", () => {
  const browserIds = planVerification([], { browser: true }).map((s) => s.id);
  assert.ok(browserIds.includes("frontend-browser"));
  assert.ok(!browserIds.includes("e2e"));

  const e2eIds = planVerification([], { e2e: true }).map((s) => s.id);
  assert.ok(e2eIds.includes("frontend-browser"));
  assert.ok(e2eIds.includes("e2e"));
});

test("已删除 .rs 文件在 resolveStep 中不向 rustfmt 传空列表，fmt-write 跳过且检查回退 cargo fmt", () => {
  const steps = planVerification(["crates/laser-core/src/deleted.rs"], { fmtWrite: true });
  const writeStep = resolveStep(steps.find((s) => s.id === "rustfmt-write"), "/repo", () => false);
  assert.equal(writeStep, null);

  const checkStep = resolveStep(steps.find((s) => s.id === "root-fmt"), "/repo", () => false);
  assert.equal(checkStep.cmd, "cargo");
  assert.deepEqual(checkStep.args, ["fmt", "--all", "--", "--check"]);
});
