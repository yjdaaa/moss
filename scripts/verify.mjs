#!/usr/bin/env node
// 本地统一验证入口：自动检测改动范围与并发构建进程，按受影响工作区执行检查、格式化与测试。
// 用法：
//   node scripts/verify.mjs              # 按当前改动范围执行对应检查与测试
//   node scripts/verify.mjs --quick      # 只跑静态门禁、格式检查与 clippy / eslint
//   node scripts/verify.mjs --affected   # 迭代级：Rust 只对改动所在 crate 跑 clippy / test，前端跳过生产构建
//   node scripts/verify.mjs --finish     # 收尾级：默认检查 + 按改动路径自动追加浏览器测试 / E2E，通过后快更桌面程序
//   node scripts/verify.mjs --full       # 全量检查根工作区、moss-voice-local 与前端
//   node scripts/verify.mjs --browser    # 追加全部前端 Playwright 浏览器交互测试
//   node scripts/verify.mjs --e2e        # 追加前端 build、浏览器交互测试、release moss-host 与 E2E 测试
//   node scripts/verify.mjs --fmt-write  # 只对改动过的 .rs 执行 rustfmt，不碰其他文件
//   node scripts/verify.mjs --install-hooks  # 启用仓库 .githooks（core.hooksPath）
//   node scripts/verify.mjs --dry-run    # 只打印执行计划
//   node scripts/verify.mjs --force      # 忽略并发构建进程检查
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const HELPER_DIR = "apps/moss-voice-local";
const HELPER_TRIGGER_PREFIXES = ["apps/moss-voice-local/", "crates/moss-voice-local-proto/"];
const ROOT_RUST_PREFIXES = ["crates/", "apps/desktop/", "apps/moss-host/"];
const ROOT_RUST_FILES = new Set(["Cargo.toml", "Cargo.lock", "clippy.toml"]);

function normalizeRelPath(p) {
  return p.replace(/\\/g, "/");
}

function isRootRustPath(p) {
  return ROOT_RUST_FILES.has(p) || ROOT_RUST_PREFIXES.some((prefix) => p.startsWith(prefix));
}

function isHelperPath(p) {
  return HELPER_TRIGGER_PREFIXES.some((prefix) => p.startsWith(prefix));
}

// 收尾级自动追加规则：命中即必跑，模型仍可手动再加 --browser / --e2e。
const BROWSER_TRIGGER_RE = /^(frontend\/src\/|frontend\/index\.html$|frontend\/tests\/[^/]+\.browser\.mjs$|scripts\/test-frontend-browser\.mjs$)/;
// 发送链路、设备层、HTTP 宿主与前端调后端 / 急停 / 发送相关模块（对应 safety.md §2 高风险类别）
const E2E_TRIGGER_RE = /^(crates\/laser-[^/]+\/src\/|apps\/moss-host\/src\/|frontend\/src\/api\/|frontend\/tests\/e2e\/|scripts\/test_e2e\.mjs$|frontend\/src\/[^/]*(estop|deviceConnection|drawConnection|engraveRoute|repeatSend|approval|jobProgress|registration|safeAction)[^/]*$)/i;
const DESKTOP_RUNTIME_RE = /^(crates\/[^/]+\/(src|build\.rs|Cargo\.toml)|apps\/desktop\/src-tauri\/(src|build\.rs|Cargo\.toml|tauri\.conf\.json|Info\.plist|capabilities)|apps\/moss-host\/src\/|apps\/moss-voice-local\/src\/|frontend\/(src\/|index\.html$|package\.json$|vite\.config)|Cargo\.(toml|lock)$)/;

/** 收尾级：按改动路径判断要追加的浏览器测试 / E2E 与快更参数（纯函数）。 */
export function planFinish(changedFiles) {
  const files = changedFiles.map(normalizeRelPath);
  return {
    browser: files.some((p) => BROWSER_TRIGGER_RE.test(p)),
    e2e: files.some((p) => E2E_TRIGGER_RE.test(p)),
    quickUpdate: files.some((p) => DESKTOP_RUNTIME_RE.test(p)),
    withVoiceHelper: files.some(isHelperPath),
    withHost: files.some((p) => p.startsWith("apps/moss-host/") || p.startsWith("crates/laser-")),
  };
}

/** 快更命令：macOS 用 .sh，Windows 用 .ps1，其他平台不支持时返回 null。 */
export function quickUpdateStep(finish, platform = process.platform) {
  if (!finish.quickUpdate) return null;
  if (platform === "darwin") {
    const args = ["scripts/quick-update-desktop.sh"];
    if (finish.withVoiceHelper) args.push("--with-voice-helper");
    if (finish.withHost) args.push("--with-host");
    return { id: "quick-update", label: "快更本机正式版桌面程序", cmd: "bash", args, cwd: ".", locksBuild: true };
  }
  if (platform === "win32") {
    const args = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts/quick-update-desktop.ps1"];
    if (finish.withVoiceHelper) args.push("-WithVoiceHelper");
    if (finish.withHost) args.push("-WithHost");
    return { id: "quick-update", label: "快更本机正式版桌面程序", cmd: "powershell.exe", args, cwd: ".", locksBuild: true };
  }
  return null;
}

/** 读取根工作区成员目录与包名，返回 [{ dir, name }]（按目录长度降序，便于最长前缀匹配）。 */
export function loadWorkspaceCrates(root = REPO_ROOT, readFile = (p) => readFileSync(p, "utf8")) {
  const manifest = readFile(join(root, "Cargo.toml"));
  const block = manifest.match(/^members\s*=\s*\[([\s\S]*?)\]/m);
  if (!block) return [];
  const dirs = [...block[1].matchAll(/"([^"]+)"/g)].map((m) => normalizeRelPath(m[1]).replace(/\/$/, ""));
  const crates = [];
  for (const dir of dirs) {
    const member = readFile(join(root, dir, "Cargo.toml"));
    const name = member.match(/^\[package\][\s\S]*?^name\s*=\s*"([^"]+)"/m);
    if (name) crates.push({ dir, name: name[1] });
  }
  return crates.sort((a, b) => b.dir.length - a.dir.length);
}

/** 把改动路径映射到所属根工作区 crate；有任一根工作区 Rust 改动无法归属（如根 Cargo.toml）时返回 null 表示需全量。 */
export function affectedCrates(files, crates) {
  const names = new Set();
  for (const p of files.filter(isRootRustPath)) {
    const hit = crates.find((c) => p.startsWith(`${c.dir}/`));
    if (!hit) return null;
    names.add(hit.name);
  }
  return [...names].sort();
}

/** 解析命令行参数。 */
export function parseArgs(argv) {
  const known = new Set(["--quick", "--affected", "--finish", "--no-update", "--full", "--browser", "--e2e", "--fmt-write", "--install-hooks", "--dry-run", "--force", "--help", "-h"]);
  for (const arg of argv) {
    if (!known.has(arg)) {
      throw new Error(`未知参数: ${arg}`);
    }
  }
  return {
    quick: argv.includes("--quick"),
    affected: argv.includes("--affected"),
    finish: argv.includes("--finish"),
    noUpdate: argv.includes("--no-update"),
    full: argv.includes("--full"),
    browser: argv.includes("--browser"),
    e2e: argv.includes("--e2e"),
    fmtWrite: argv.includes("--fmt-write"),
    installHooks: argv.includes("--install-hooks"),
    dryRun: argv.includes("--dry-run"),
    force: argv.includes("--force"),
    help: argv.includes("--help") || argv.includes("-h"),
  };
}

/** 按改动路径列表与运行选项生成验证步骤计划（纯函数）。 */
export function planVerification(changedFiles, options = {}) {
  const { quick = false, affected = false, full = false, browser = false, e2e = false, fmtWrite = false, crates = [] } = options;
  const files = [...new Set(changedFiles.map(normalizeRelPath).filter(Boolean))];
  const steps = [
    { id: "check-crate-layers", label: "crate 分层与清单检查", cmd: process.execPath, args: ["scripts/check-crate-layers.mjs"], cwd: ".", locksBuild: false },
    { id: "check-repo-hygiene", label: "仓库静态规范检查", cmd: process.execPath, args: ["scripts/check-repo-hygiene.mjs"], cwd: ".", locksBuild: false },
    { id: "scripts-tests", label: "脚本单测", cmd: process.execPath, args: ["--test", "scripts/tests/authenticated-download-protocol.test.mjs", "scripts/tests/crate-layers.test.mjs", "scripts/tests/repo-hygiene.test.mjs", "scripts/tests/speaker-calibration.test.mjs", "scripts/tests/verify.test.mjs"], cwd: ".", locksBuild: false },
  ];

  const changedRootRs = files.filter((p) => p.endsWith(".rs") && !p.startsWith("apps/moss-voice-local/") && !p.startsWith("apps/desktop/src-tauri/gen/"));
  const changedHelperRs = files.filter((p) => p.endsWith(".rs") && p.startsWith("apps/moss-voice-local/"));
  const touchRootRust = full || files.some(isRootRustPath);
  const touchHelper = full || files.some(isHelperPath);
  const touchFrontend = full || e2e || files.some((p) => p.startsWith("frontend/"));
  const fallbackQuickRoot = !full && files.length === 0;
  // --affected：只对改动所在 crate 跑 clippy / test；无法归属时退回全量工作区
  const scopedCrates = affected && !full ? affectedCrates(files, crates) : null;
  const cargoScope = scopedCrates && scopedCrates.length > 0 ? scopedCrates.flatMap((name) => ["-p", name]) : ["--workspace"];
  const scopeLabel = cargoScope[0] === "--workspace" ? "根工作区" : `改动 crate（${scopedCrates.join("、")}）`;

  if (fmtWrite) {
    const rsFiles = files.filter((p) => p.endsWith(".rs") && !p.startsWith("apps/desktop/src-tauri/gen/"));
    if (rsFiles.length > 0) {
      steps.push({ id: "rustfmt-write", label: "格式化改动的 .rs 文件", cmd: "rustfmt", args: ["--edition", "2024", ...rsFiles], cwd: ".", locksBuild: false });
    }
  }

  if (touchRootRust || fallbackQuickRoot) {
    if (!full && changedRootRs.length > 0) {
      steps.push({ id: "root-fmt", label: "根工作区 rustfmt 检查（仅改动文件）", cmd: "rustfmt", args: ["--edition", "2024", "--check", ...changedRootRs], cwd: ".", locksBuild: false });
    } else {
      steps.push({ id: "root-fmt", label: "根工作区 rustfmt 检查", cmd: "cargo", args: ["fmt", "--all", "--", "--check"], cwd: ".", locksBuild: false });
    }
    steps.push({ id: "root-clippy", label: `${scopeLabel} clippy 检查`, cmd: "cargo", args: ["clippy", ...cargoScope, "--all-targets", "--", "-D", "warnings"], cwd: ".", locksBuild: true });
    if (!quick && touchRootRust) {
      steps.push({ id: "root-test", label: `${scopeLabel} Rust 测试`, cmd: "cargo", args: ["test", ...cargoScope], cwd: ".", locksBuild: true });
    }
  }

  if (touchHelper) {
    if (!full && changedHelperRs.length > 0) {
      const relHelperRs = changedHelperRs.map((p) => p.slice(`${HELPER_DIR}/`.length));
      steps.push({ id: "helper-fmt", label: "moss-voice-local rustfmt 检查（仅改动文件）", cmd: "rustfmt", args: ["--edition", "2024", "--check", ...relHelperRs], cwd: HELPER_DIR, locksBuild: false });
    } else {
      steps.push({ id: "helper-fmt", label: "moss-voice-local rustfmt 检查", cmd: "cargo", args: ["fmt", "--all", "--", "--check"], cwd: HELPER_DIR, locksBuild: false });
    }
    steps.push({ id: "helper-clippy", label: "moss-voice-local clippy 检查", cmd: "cargo", args: ["clippy", "--all-targets", "--", "-D", "warnings"], cwd: HELPER_DIR, locksBuild: true });
    if (!quick) {
      steps.push({ id: "helper-test", label: "moss-voice-local 测试", cmd: "cargo", args: ["test"], cwd: HELPER_DIR, locksBuild: true });
    }
  }

  const npmCmd = process.platform === "win32" ? "npm.cmd" : "npm";
  if (touchFrontend || fallbackQuickRoot) {
    steps.push({ id: "frontend-lint", label: "前端 ESLint 检查", cmd: npmCmd, args: ["--prefix", "frontend", "run", "lint"], cwd: ".", locksBuild: false });
    if (!quick && touchFrontend) {
      steps.push({ id: "frontend-test", label: "前端单元测试", cmd: npmCmd, args: ["--prefix", "frontend", "test"], cwd: ".", locksBuild: false });
      if (!affected || full) {
        steps.push({ id: "frontend-build", label: "前端生产构建", cmd: npmCmd, args: ["--prefix", "frontend", "run", "build"], cwd: ".", locksBuild: true });
      }
    }
  }

  if (browser || e2e) {
    steps.push({ id: "frontend-browser", label: "前端 Playwright 浏览器交互测试", cmd: process.execPath, args: ["scripts/test-frontend-browser.mjs"], cwd: ".", locksBuild: true });
  }

  if (e2e) {
    if (!steps.some((s) => s.id === "frontend-build")) {
      steps.push({ id: "frontend-build", label: "前端生产构建", cmd: npmCmd, args: ["--prefix", "frontend", "run", "build"], cwd: ".", locksBuild: true });
    }
    steps.push({ id: "host-release-build", label: "构建 release moss-host（E2E 依赖）", cmd: "cargo", args: ["build", "--release", "-p", "moss-host"], cwd: ".", locksBuild: true });
    steps.push({ id: "e2e", label: "回环 E2E 测试", cmd: process.execPath, args: ["scripts/test_e2e.mjs"], cwd: ".", locksBuild: true });
  }

  return steps;
}

/** 解析 ps 输出中的并发构建/测试进程，排除当前进程及常见父级 shell/检测命令。 */
export function detectActiveBuildProcesses(psOutput, selfPid = process.pid) {
  const hits = [];
  // Windows 命令行形如 `"C:\…\cargo.exe" test`：允许反斜杠 / 引号分隔与 `.exe` 后缀
  const toolRe = /(?:^|[\s/\\"])(cargo|rustc|vite|playwright|test_e2e\.mjs|test-frontend-browser\.mjs)(?:\.exe)?(?:["\s]|$)/i;
  for (const raw of psOutput.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    const m = line.match(/^(\d+)\s+(.+)$/);
    if (!m) continue;
    const pid = Number(m[1]);
    const cmdline = m[2];
    if (pid === selfPid) continue;
    if (/\bverify\.mjs\b/.test(cmdline)) continue;
    if (toolRe.test(cmdline)) {
      hits.push({ pid, cmdline });
    }
  }
  return hits;
}

/** 解析 Windows .cmd/.bat 命令的跨平台执行规格，避免 Node CVE-2024-27980 修复后直接 spawn .cmd 报 EINVAL。 */
export function buildSpawnSpec(cmd, args, platform = process.platform, comSpec = process.env.ComSpec) {
  if (platform === "win32" && /\.(cmd|bat)$/i.test(cmd)) {
    return { cmd: comSpec || "cmd.exe", args: ["/d", "/c", cmd, ...args] };
  }
  return { cmd, args };
}

/** 解析单步命令在当前工作区的实际可执行参数；若无需执行则返回 null。 */
export function resolveStep(step, root = REPO_ROOT, fileExists = existsSync) {
  if (step.id === "scripts-tests") {
    const testDir = join(root, "scripts", "tests");
    const files = readdirSync(testDir)
      .filter((name) => name.endsWith(".test.mjs"))
      .sort()
      .map((name) => `scripts/tests/${name}`);
    return { ...step, args: ["--test", ...files] };
  }
  if (step.cmd === "rustfmt") {
    const baseCwd = join(root, step.cwd);
    const editionIdx = step.args.indexOf("2024");
    const prefix = editionIdx >= 0 ? step.args.slice(0, editionIdx + 1) : step.args.filter((a) => a.startsWith("-"));
    const candidates = editionIdx >= 0 ? step.args.slice(editionIdx + 1) : step.args.filter((a) => !a.startsWith("-"));
    const existing = candidates.filter((a) => fileExists(join(baseCwd, a)));
    if (existing.length === 0) {
      if (step.id === "rustfmt-write") return null;
      return { ...step, label: step.label.replace("（仅改动文件）", ""), cmd: "cargo", args: ["fmt", "--all", "--", "--check"] };
    }
    return { ...step, args: [...prefix, ...existing] };
  }
  return step;
}

function readChangedFiles(root) {
  const statusOut = execFileSync("git", ["status", "--porcelain", "-z", "-uall"], { cwd: root, encoding: "utf8" });
  const dirty = [];
  const entries = statusOut.split("\0");
  for (let i = 0; i < entries.length; i += 1) {
    const entry = entries[i];
    if (!entry) continue;
    const xy = entry.slice(0, 2);
    const path = entry.slice(3);
    if (path) {
      dirty.push(path);
    }
    if ((xy.startsWith("R") || xy.startsWith("C")) && entries[i + 1]) {
      dirty.push(entries[i + 1]);
      i += 1;
    }
  }
  if (dirty.length > 0) {
    return [...new Set(dirty)];
  }
  try {
    const diffOut = execFileSync("git", ["diff", "--name-only", "-z", "HEAD~1", "HEAD"], { cwd: root, encoding: "utf8" });
    return diffOut.split("\0").filter(Boolean);
  } catch {
    return [];
  }
}

function readActiveProcesses() {
  try {
    // Windows 没有 ps：用 CIM 列出“PID 命令行”，格式与 ps 输出一致，交给同一个解析函数
    const psOut = process.platform === "win32"
      ? execFileSync(
        "powershell.exe",
        [
          "-NoProfile",
          "-NonInteractive",
          "-Command",
          "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine } | ForEach-Object { \"$($_.ProcessId) $($_.CommandLine)\" }",
        ],
        { encoding: "utf8", windowsHide: true },
      )
      : execFileSync("ps", ["-axo", "pid=,command="], { encoding: "utf8" });
    return detectActiveBuildProcesses(psOut, process.pid);
  } catch {
    return [];
  }
}

function installHooks(root) {
  const hooksDir = join(root, ".githooks");
  if (!existsSync(hooksDir)) {
    throw new Error("缺少 .githooks 目录");
  }
  if (process.platform !== "win32") {
    for (const name of readdirSync(hooksDir)) {
      chmodSync(join(hooksDir, name), 0o755);
    }
  }
  execFileSync("git", ["config", "core.hooksPath", ".githooks"], { cwd: root, stdio: "inherit" });
  console.log("已启用仓库 Git 钩子（core.hooksPath = .githooks）");
}

function main() {
  let opts;
  try {
    opts = parseArgs(process.argv.slice(2));
  } catch (err) {
    console.error(err.message);
    process.exitCode = 2;
    return;
  }

  if (opts.help) {
    console.log("用法：node scripts/verify.mjs [--quick|--affected|--finish [--no-update]|--full|--browser|--e2e|--fmt-write|--install-hooks|--dry-run|--force]");
    return;
  }
  if (opts.installHooks) {
    installHooks(REPO_ROOT);
    return;
  }

  const gitChanged = readChangedFiles(REPO_ROOT);
  const changedFiles = opts.full ? [] : gitChanged;
  const crates = opts.affected && !opts.full ? loadWorkspaceCrates(REPO_ROOT) : [];
  let planOpts = { ...opts, crates };
  let finish = null;
  if (opts.finish) {
    finish = planFinish(gitChanged);
    planOpts = { ...planOpts, affected: false, quick: false, fmtWrite: true, browser: opts.browser || finish.browser, e2e: opts.e2e || finish.e2e };
  }
  const steps = planVerification(changedFiles, planOpts).map((s) => resolveStep(s, REPO_ROOT)).filter(Boolean);
  if (finish && !opts.noUpdate) {
    const update = quickUpdateStep(finish);
    if (update) steps.push(update);
    else if (finish.quickUpdate) console.log("当前平台没有快更脚本，跳过快更");
  }

  if (opts.dryRun) {
    console.log(`改动文件数：${changedFiles.length}`);
    if (finish) {
      console.log(`收尾级自动追加：浏览器测试=${planOpts.browser ? "是" : "否"}，E2E=${planOpts.e2e ? "是" : "否"}，快更=${finish.quickUpdate && !opts.noUpdate ? "是" : "否"}`);
    }
    for (const step of steps) {
      console.log(`- [${step.cwd}] ${step.cmd} ${step.args.join(" ")}  # ${step.label}`);
    }
    return;
  }

  const needsLock = steps.some((s) => s.locksBuild);
  if (needsLock && !opts.force) {
    const active = readActiveProcesses();
    if (active.length > 0) {
      console.error("检测到并发构建或测试进程正在运行，请等待结束后重试（或传 --force 跳过）：");
      for (const proc of active) {
        console.error(`  PID ${proc.pid}: ${proc.cmdline}`);
      }
      process.exitCode = 1;
      return;
    }
  }

  const passed = [];
  for (const step of steps) {
    const spec = buildSpawnSpec(step.cmd, step.args);
    console.log(`\n==> ${step.label}`);
    const res = spawnSync(spec.cmd, spec.args, {
      cwd: join(REPO_ROOT, step.cwd),
      stdio: "inherit",
    });
    if (res.error) {
      console.error(`执行失败（${step.cmd}）：${res.error.message}`);
      process.exitCode = 1;
      return;
    }
    if (res.status !== 0) {
      console.error(`\n✗ 失败：${step.label}${passed.length ? `（此前已通过：${passed.join("、")}）` : ""}`);
      process.exitCode = res.status ?? 1;
      return;
    }
    passed.push(step.label);
  }
  // 摘要可直接用于 CHANGELOG “验证”一项
  console.log(`\n验证全部通过：${passed.join("、")}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
