// 前端 Playwright 浏览器测试发现与跨平台通道选择测试。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { REPO_ROOT, discoverBrowserTests, parseArgs, resolvePlaywrightChannel } from "../test-frontend-browser.mjs";

test("发现仓库全部前端浏览器交互测试文件且无硬编码 Windows 路径", () => {
  const files = discoverBrowserTests();
  assert.ok(files.length >= 17, `预期至少 17 个前端浏览器测试，实际 ${files.length}`);
  assert.ok(files.includes("tests/voice-center.browser.mjs"));
  assert.ok(files.includes("tests/voice-center-render.smoke.mjs"));
  assert.ok(files.includes("tests/twin-render.browser.mjs"));
  assert.deepEqual(files, [...files].sort());

  const filtered = discoverBrowserTests(undefined, ["voice-center-render"]);
  assert.deepEqual(filtered, ["tests/voice-center-render.smoke.mjs"]);
});

test("浏览器通道选择遵循显式环境变量优先，按平台安全回退", () => {
  assert.equal(resolvePlaywrightChannel({ PLAYWRIGHT_CHANNEL: "chrome" }, "win32"), "chrome");
  assert.equal(resolvePlaywrightChannel({}, "win32"), "msedge");
  assert.equal(resolvePlaywrightChannel({}, "linux"), undefined);
  assert.equal(
    resolvePlaywrightChannel({}, "darwin", { hasBundledChromium: true, hasMacChrome: true }),
    undefined,
  );
  assert.equal(
    resolvePlaywrightChannel({}, "darwin", { hasBundledChromium: false, hasMacChrome: true }),
    "chrome",
  );
  assert.equal(
    resolvePlaywrightChannel({}, "darwin", { hasBundledChromium: false, hasMacChrome: false }),
    undefined,
  );
});

test("解析 --list 与 --filter 参数", () => {
  assert.deepEqual(parseArgs(["--list", "--filter", "twin"]), {
    help: false,
    filters: ["twin"],
    listOnly: true,
  });
  assert.throws(() => parseArgs(["--unknown"]), /未知参数/);
});

test("Playwright 内置 Chromium 探测脚本显式声明 --input-type=module 以兼容 Node 20", () => {
  for (const rel of ["scripts/test-frontend-browser.mjs", "scripts/test_e2e.mjs"]) {
    const src = readFileSync(join(REPO_ROOT, rel), "utf8");
    assert.match(src, /"--input-type=module",\s*"-e"/, `${rel} 的 ESM 探测命令必须携带 --input-type=module`);
  }
  const res = spawnSync(
    process.execPath,
    ["--no-experimental-detect-module", "--input-type=module", "-e", 'import { existsSync } from "node:fs"; process.exit(existsSync(".") ? 0 : 1);'],
    { stdio: "ignore" },
  );
  assert.equal(res.status, 0);
});
