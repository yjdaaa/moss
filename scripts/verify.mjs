#!/usr/bin/env node
// 本地统一验证入口：自动检测改动范围与并发构建进程，按受影响工作区执行检查、格式化与测试。
// 用法：
//   node scripts/verify.mjs              # 按当前改动范围执行对应检查与测试
//   node scripts/verify.mjs --quick      # 只跑静态门禁、格式检查与 clippy / eslint
//   node scripts/verify.mjs --full       # 全量检查根工作区、moss-voice-local 与前端
//   node scripts/verify.mjs --e2e        # 追加前端 build、release moss-host 与 E2E 测试
//   node scripts/verify.mjs --fmt-write  # 只对改动过的 .rs 执行 rustfmt，不碰其他文件
//   node scripts/verify.mjs --install-hooks  # 启用仓库 .githooks（core.hooksPath）
//   node scripts/verify.mjs --dry-run    # 只打印执行计划
//   node scripts/verify.mjs --force      # 忽略并发构建进程检查
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, existsSync, readdirSync } from "node:fs";
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

/** 解析命令行参数。 */
export function parseArgs(argv) {
  const known = new Set(["--quick", "--full", "--e2e", "--fmt-write", "--install-hooks", "--dry-run", "--force", "--help", "-h"]);
  for (const arg of argv) {
    if (!known.has(arg)) {
      throw new Error(`未知参数: ${arg}`);
    }
  }
  return {
    quick: argv.includes("--quick"),
    full: argv.includes("--full"),
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
  const { quick = false, full = false, e2e = false, fmtWrite = false } = options;
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
    steps.push({ id: "root-clippy", label: "根工作区 clippy 检查", cmd: "cargo", args: ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], cwd: ".", locksBuild: true });
    if (!quick && touchRootRust) {
      steps.push({ id: "root-test", label: "根工作区 Rust 测试", cmd: "cargo", args: ["test", "--workspace"], cwd: ".", locksBuild: true });
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
      steps.push({ id: "frontend-build", label: "前端生产构建", cmd: npmCmd, args: ["--prefix", "frontend", "run", "build"], cwd: ".", locksBuild: true });
    }
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
  const toolRe = /(?:^|[\s/\\"])(cargo|rustc|vite|playwright|test_e2e\.mjs)(?:\.exe)?(?:["\s]|$)/i;
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
    console.log("用法：node scripts/verify.mjs [--quick|--full|--e2e|--fmt-write|--install-hooks|--dry-run|--force]");
    return;
  }
  if (opts.installHooks) {
    installHooks(REPO_ROOT);
    return;
  }

  const changedFiles = opts.full ? [] : readChangedFiles(REPO_ROOT);
  const steps = planVerification(changedFiles, opts).map((s) => resolveStep(s, REPO_ROOT)).filter(Boolean);

  if (opts.dryRun) {
    console.log(`改动文件数：${changedFiles.length}`);
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
      process.exitCode = res.status ?? 1;
      return;
    }
  }
  console.log("\n验证全部通过");
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
