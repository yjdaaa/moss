#!/usr/bin/env node
// 统一运行 frontend/tests/ 下的全部 Playwright 浏览器交互测试（*.browser.mjs 与 *.smoke.mjs）。
// 用法：
//   node scripts/test-frontend-browser.mjs [--filter <子串>] [测试文件名...]
// 环境变量：
//   PLAYWRIGHT_CHANNEL=chrome|msedge  显式指定浏览器通道；未设置时 Windows 用 msedge，
//                                     macOS 若未安装 Playwright 内置 Chromium 则自动回退本机 Chrome。
import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const FRONTEND_DIR = join(REPO_ROOT, "frontend");
const TESTS_DIR = join(FRONTEND_DIR, "tests");

/** 发现 frontend/tests 下按字母序排列的全部浏览器交互测试文件（返回相对 frontend/ 的路径）。 */
export function discoverBrowserTests(testsDir = TESTS_DIR, filters = []) {
  const all = readdirSync(testsDir)
    .filter((name) => name.endsWith(".browser.mjs") || name.endsWith(".smoke.mjs"))
    .sort()
    .map((name) => `tests/${name}`);
  if (filters.length === 0) return all;
  return all.filter((rel) => filters.some((f) => rel.includes(f)));
}

/** 决定 Playwright 浏览器通道：显式环境变量优先，其次按平台回退本机可用浏览器。 */
export function resolvePlaywrightChannel(
  env = process.env,
  platform = process.platform,
  { hasBundledChromium = null, hasMacChrome = null } = {},
) {
  if (env.PLAYWRIGHT_CHANNEL) return env.PLAYWRIGHT_CHANNEL;
  if (platform === "win32") return "msedge";
  if (platform === "darwin") {
    const bundledOk = hasBundledChromium ?? checkBundledChromium(env);
    if (!bundledOk) {
      const chromeOk = hasMacChrome ?? existsSync("/Applications/Google Chrome.app");
      if (chromeOk) return "chrome";
    }
  }
  return undefined;
}

function checkBundledChromium(env) {
  try {
    const probe = spawnSync(
      process.execPath,
      [
        "--input-type=module",
        "-e",
        'import { chromium } from "playwright"; import { existsSync } from "node:fs"; process.exit(existsSync(chromium.executablePath()) ? 0 : 1);',
      ],
      { cwd: FRONTEND_DIR, env, stdio: "ignore" },
    );
    return probe.status === 0;
  } catch {
    return false;
  }
}

export function parseArgs(argv) {
  const filters = [];
  let listOnly = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--filter") {
      const val = argv[++i];
      if (!val) throw new Error("--filter 需要提供匹配子串");
      filters.push(val);
    } else if (arg === "--list") {
      listOnly = true;
    } else if (arg === "-h" || arg === "--help") {
      return { help: true, filters, listOnly };
    } else if (arg.startsWith("-")) {
      throw new Error(`未知参数: ${arg}`);
    } else {
      filters.push(arg);
    }
  }
  return { help: false, filters, listOnly };
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
    console.log("用法：node scripts/test-frontend-browser.mjs [--list] [--filter <子串>] [文件名...]");
    return;
  }

  const files = discoverBrowserTests(TESTS_DIR, opts.filters);
  if (files.length === 0) {
    console.error("未匹配到任何前端浏览器测试文件");
    process.exitCode = 1;
    return;
  }
  if (opts.listOnly) {
    for (const f of files) console.log(f);
    return;
  }

  const channel = resolvePlaywrightChannel(process.env, process.platform);
  const childEnv = { ...process.env };
  if (channel) {
    childEnv.PLAYWRIGHT_CHANNEL = channel;
  }
  console.log(`==> 前端浏览器测试（${files.length} 项${channel ? `，channel=${channel}` : ""}）`);

  const failures = [];
  const startedAll = Date.now();
  for (const rel of files) {
    const t0 = Date.now();
    const res = spawnSync(process.execPath, [rel], {
      cwd: FRONTEND_DIR,
      env: childEnv,
      encoding: "utf8",
    });
    const elapsedMs = Date.now() - t0;
    if (res.status === 0) {
      console.log(`  PASS ${rel} (${elapsedMs} ms)`);
    } else {
      console.error(`  FAIL ${rel} (exit=${res.status ?? "err"}, ${elapsedMs} ms)`);
      if (res.stdout?.trim()) console.error(res.stdout.trim());
      if (res.stderr?.trim()) console.error(res.stderr.trim());
      if (res.error) console.error(res.error.message);
      failures.push(rel);
    }
  }

  const totalSec = ((Date.now() - startedAll) / 1000).toFixed(1);
  if (failures.length > 0) {
    console.error(`\n前端浏览器测试失败 ${failures.length}/${files.length}（${totalSec}s）：${failures.join(", ")}`);
    process.exitCode = 1;
    return;
  }
  console.log(`\n前端浏览器测试全部通过（${files.length}/${files.length}，${totalSec}s）`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
