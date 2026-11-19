import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";

const unixOnly = { skip: process.platform === "win32", timeout: 10000 };

async function withRunner(t, body) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "moss-e2e-runner-test-"));
  const fixture = path.join(root, "fixture");
  const linked = path.join(root, "linked");
  const actualScript = path.join(fixture, "scripts", "test_e2e.mjs");
  const host = path.join(fixture, "target", "release", "moss-host");
  fs.mkdirSync(path.dirname(actualScript), { recursive: true });
  fs.mkdirSync(path.dirname(host), { recursive: true });
  fs.copyFileSync(new URL("../test_e2e.mjs", import.meta.url), actualScript);
  fs.symlinkSync(fixture, linked, "dir");
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));

  const recordedPid = path.join(root, "host-pid");
  function launch(program, mode = 0o755) {
    fs.writeFileSync(host, `#!/usr/bin/env node\n${program}\n`, { mode });
    const env = { ...process.env, MOCK_HOST_PID_FILE: recordedPid, TMPDIR: root };
    delete env.NODE_TEST_CONTEXT;
    const runner = spawn(process.execPath, [path.join(linked, "scripts", "test_e2e.mjs"), "--api-only"], {
      cwd: fixture,
      env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    for (const stream of [runner.stdout, runner.stderr]) {
      stream.on("data", (chunk) => { output += chunk.toString(); });
    }
    const exited = once(runner, "close", { signal: AbortSignal.timeout(8000) });
    t.after(async () => {
      if (runner.exitCode === null && runner.signalCode === null) runner.kill("SIGTERM");
      await exited.catch(() => {});
    });
    return { runner, exited, output: () => output };
  }

  await body({ launch, recordedPid, fixture });
  assert.equal(fs.readdirSync(root).filter((name) => name.startsWith("moss-e2e-")).length, 0);
}

test("E2E resolves symlinked CLI entry and fails promptly on spawn failure", unixOnly, async (t) => {
  await withRunner(t, async ({ launch }) => {
    const { exited, output } = launch("", 0o644);
    assert.deepEqual(await exited, [1, null]);
    assert.match(output(), /启动失败/);
    assert.doesNotMatch(output(), /测试宿主已就绪/);
  });
});

test("E2E reports early host exit without probing a guessed port", unixOnly, async (t) => {
  await withRunner(t, async ({ launch }) => {
    const { exited, output } = launch('process.exit(7);');
    assert.deepEqual(await exited, [1, null]);
    assert.match(output(), /提前退出（7）/);
    assert.doesNotMatch(output(), /测试宿主已就绪/);
  });
});

test("E2E handles SIGTERM while starting, reaps the host and keeps the signal exit code", unixOnly, async (t) => {
  await withRunner(t, async ({ launch, recordedPid }) => {
    const { runner, exited } = launch(`
      require("node:fs").writeFileSync(process.env.MOCK_HOST_PID_FILE, String(process.pid));
      setInterval(() => {}, 250);
    `);
    for (let attempt = 0; attempt < 100 && !fs.existsSync(recordedPid); attempt += 1) {
      await sleep(25);
    }
    const hostPid = Number(fs.readFileSync(recordedPid, "utf8"));
    assert.ok(runner.kill("SIGTERM"));
    assert.deepEqual(await exited, [143, null]);
    assert.throws(() => process.kill(hostPid, 0), { code: "ESRCH" });
  });
});

test("E2E accepts a split ready line and preserves a nonzero suite exit", unixOnly, async (t) => {
  await withRunner(t, async ({ launch, recordedPid, fixture }) => {
    const suite = path.join(fixture, "frontend", "tests", "e2e", "api.test.mjs");
    fs.mkdirSync(path.dirname(suite), { recursive: true });
    fs.writeFileSync(suite, 'console.log("MOCK_SUITE_REACHED", process.env.TEST_API_PORT); process.exit(9);\n');
    const { exited, output } = launch(`
      require("node:fs").writeFileSync(process.env.MOCK_HOST_PID_FILE, String(process.pid));
      const server = require("node:http").createServer((request, response) => response.end("{}"));
      server.listen(0, "127.0.0.1", () => {
        process.stdout.write("moss-host serving on 127.0.");
        setTimeout(() => process.stdout.write("0.1:" + server.address().port + "\\n"), 50);
      });
    `);
    // node --test 将单个用例进程的失败汇总为套件退出码 1。
    assert.deepEqual(await exited, [1, null], output());
    assert.match(output(), /测试宿主已就绪/);
    assert.match(output(), /MOCK_SUITE_REACHED \d+/);
    assert.match(output(), /fail 1/);
    const hostPid = Number(fs.readFileSync(recordedPid, "utf8"));
    assert.throws(() => process.kill(hostPid, 0), { code: "ESRCH" });
  });
});

test("E2E aborts a stalled health probe when the host exits", unixOnly, async (t) => {
  await withRunner(t, async ({ launch }) => {
    const { exited, output } = launch(`
      const server = require("node:http").createServer(() => {
        console.log("MOCK_PROBE_RECEIVED");
        setTimeout(() => process.exit(7), 50);
      });
      server.listen(0, "127.0.0.1", () => {
        console.log("moss-host serving on 127.0.0.1:" + server.address().port);
      });
    `);
    assert.deepEqual(await exited, [1, null], output());
    assert.match(output(), /MOCK_PROBE_RECEIVED/);
    assert.match(output(), /提前退出（7）/);
    assert.doesNotMatch(output(), /测试宿主已就绪/);
  });
});
