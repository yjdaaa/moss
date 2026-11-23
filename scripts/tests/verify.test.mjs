// verify.mjs 计划生成与并发构建进程识别测试。
import { test } from "node:test";
import assert from "node:assert/strict";
import { affectedCrates, buildSpawnSpec, detectActiveBuildProcesses, loadWorkspaceCrates, planFinish, planVerification, quickUpdateStep, resolveStep } from "../verify.mjs";

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

const CRATES = [
  { dir: "apps/desktop/src-tauri", name: "moss-desktop" },
  { dir: "crates/laser-core", name: "laser-core" },
  { dir: "crates/laser-api", name: "laser-api" },
];

test("--affected 只对改动所在 crate 跑 clippy 与 test，前端跳过生产构建", () => {
  const steps = planVerification(
    ["crates/laser-core/src/lib.rs", "crates/laser-api/Cargo.toml", "crates/laser-core/src/cut/mod.rs", "frontend/src/main.jsx"],
    { affected: true, crates: CRATES },
  );
  const byId = Object.fromEntries(steps.map((s) => [s.id, s]));
  assert.deepEqual(byId["root-clippy"].args, ["clippy", "-p", "laser-api", "-p", "laser-core", "--all-targets", "--", "-D", "warnings"]);
  assert.deepEqual(byId["root-test"].args, ["test", "-p", "laser-api", "-p", "laser-core"]);
  assert.ok(byId["frontend-lint"]);
  assert.ok(byId["frontend-test"]);
  assert.ok(!byId["frontend-build"]);
});

test("--affected 遇到无法归属 crate 的根工作区改动时退回全量工作区", () => {
  const steps = planVerification(["Cargo.lock", "crates/laser-core/src/lib.rs"], { affected: true, crates: CRATES });
  assert.deepEqual(steps.find((s) => s.id === "root-test").args, ["test", "--workspace"]);
  assert.deepEqual(affectedCrates(["Cargo.toml"], CRATES), null);
  assert.deepEqual(affectedCrates(["frontend/src/main.jsx"], CRATES), []);
});

test("--affected 与 --full 或 --e2e 同用时不削减全量检查与 E2E 所需构建", () => {
  const fullSteps = planVerification([], { affected: true, full: true, crates: CRATES });
  assert.deepEqual(fullSteps.find((s) => s.id === "root-test").args, ["test", "--workspace"]);
  assert.ok(fullSteps.some((s) => s.id === "frontend-build"));

  const e2eIds = planVerification(["frontend/src/main.jsx"], { affected: true, e2e: true, crates: CRATES }).map((s) => s.id);
  assert.ok(e2eIds.includes("frontend-build"));
  assert.ok(e2eIds.includes("e2e"));
});

test("loadWorkspaceCrates 读取成员目录与 [package] 包名并按目录长度降序", () => {
  const files = {
    "/repo/Cargo.toml": '[workspace]\nmembers = [\n    "crates/laser-core",\n    "apps/desktop/src-tauri",\n]\n',
    "/repo/crates/laser-core/Cargo.toml": '[package]\nname = "laser-core"\n\n[[bench]]\nname = "raster_2000"\n',
    "/repo/apps/desktop/src-tauri/Cargo.toml": '[package]\nname = "moss-desktop"\n\n[lib]\nname = "moss_desktop_lib"\n',
  };
  const crates = loadWorkspaceCrates("/repo", (p) => files[p.replace(/\\/g, "/")]);
  assert.deepEqual(crates, [
    { dir: "apps/desktop/src-tauri", name: "moss-desktop" },
    { dir: "crates/laser-core", name: "laser-core" },
  ]);
});

test("真实根工作区每个成员都能解析出包名", () => {
  const crates = loadWorkspaceCrates();
  assert.ok(crates.length >= 20);
  assert.ok(crates.some((c) => c.dir === "apps/desktop/src-tauri" && c.name === "moss-desktop"));
  assert.ok(crates.every((c) => /^[a-z0-9_-]+$/.test(c.name)));
});

test("--finish 按改动路径决定浏览器测试、E2E 与快更", () => {
  assert.deepEqual(planFinish(["docs/user/quick-start.md", "CHANGELOG.md"]), {
    browser: false, e2e: false, quickUpdate: false, withVoiceHelper: false, withHost: false,
  });
  assert.deepEqual(planFinish(["frontend/src/styles/06-shell.css"]), {
    browser: true, e2e: false, quickUpdate: true, withVoiceHelper: false, withHost: false,
  });
  // 前端发送 / 急停相关模块与后端发送链路必跑 E2E
  assert.equal(planFinish(["frontend/src/globalEstop.js"]).e2e, true);
  assert.equal(planFinish(["frontend/src/deviceConnection.js"]).e2e, true);
  assert.equal(planFinish(["frontend/src/api/client.js"]).e2e, true);
  const laser = planFinish(["crates/laser-device/src/worker.rs"]);
  assert.equal(laser.e2e, true);
  assert.equal(laser.withHost, true);
  assert.equal(laser.browser, false);
  // 只改测试不快更
  assert.equal(planFinish(["frontend/tests/workbenchParams.test.mjs", "crates/laser-core/tests/gcode.rs"]).quickUpdate, false);
  const helper = planFinish(["apps/moss-voice-local/src/main.rs"]);
  assert.equal(helper.withVoiceHelper, true);
  assert.equal(helper.quickUpdate, true);
  assert.equal(planFinish(["crates/moss-voice-server/src/lib.rs"]).e2e, false);
});

test("快更命令按平台与参数生成，无需快更或不支持的平台返回 null", () => {
  const finish = { quickUpdate: true, withVoiceHelper: true, withHost: true };
  assert.deepEqual(quickUpdateStep(finish, "darwin").args, ["scripts/quick-update-desktop.sh", "--with-voice-helper", "--with-host"]);
  const win = quickUpdateStep(finish, "win32");
  assert.equal(win.cmd, "powershell.exe");
  assert.deepEqual(win.args.slice(-3), ["scripts/quick-update-desktop.ps1", "-WithVoiceHelper", "-WithHost"]);
  assert.equal(quickUpdateStep(finish, "linux"), null);
  assert.equal(quickUpdateStep({ ...finish, quickUpdate: false }, "darwin"), null);
});
