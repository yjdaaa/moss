#!/usr/bin/env node
import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT_DIR = path.resolve(SCRIPT_DIR, "..");
const HOST_BIN = process.platform === "win32" ? "moss-host.exe" : "moss-host";
const RELEASE_EXE = path.join(ROOT_DIR, "target", "release", HOST_BIN);
const DEBUG_EXE = path.join(ROOT_DIR, "target", "debug", HOST_BIN);

export function resolveSuiteFiles(argv = process.argv, env = process.env) {
  const apiOnly = argv.includes("--api-only") || env.MOSS_E2E_API_ONLY === "1";
  return apiOnly
    ? ["frontend/tests/e2e/api.test.mjs"]
    : ["frontend/tests/e2e/api.test.mjs", "frontend/tests/e2e/workbench.ui.test.mjs"];
}

/**
 * 回环 GRBL 模拟器：在 127.0.0.1 上监听 Telnet 端口，
 * 收到 `$$` 回固件设置（含 `$32=1`），收到实时字节 `?` 回状态报告，
 * 只对 GRBL/FluidNC G-code 指令回 `ok`；收到非法行返回 error。收到 0x18 记录急停，
 * 用于让 E2E 真正走 TCP 传输 + 握手 + 逐行 ack。
 */
function startFakeGrblServer(port, opts = {}) {
  const laserMode = opts.laserMode ?? 1;
  const stats = { lines: 0, resets: 0, feedHolds: 0, connections: 0 };
  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    stats.connections += 1;
    let buffer = "";
    socket.on("data", (chunk) => {
      const bytes = Array.from(chunk);
      let text = "";
      for (const b of bytes) {
        if (b === 0x18) { stats.resets += 1; socket.write("\r\nGrbl 1.1h ['$' for help]\r\n"); continue; }
        if (b === 0x21) { stats.feedHolds += 1; continue; }
        if (b === 0x3f) { socket.write("<Idle|MPos:0.000,0.000,0.000|FS:0,0>\r\n"); continue; }
        if (b === 0x7e || b === 0x85) continue;
        text += String.fromCharCode(b);
      }
      buffer += text;
      let idx;
      while ((idx = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, idx).replace(/\r$/, "");
        buffer = buffer.slice(idx + 1);
        if (line.trim() === "$$") {
          socket.write(`$0=10\r\n$1=25\r\n$10=1\r\n$30=1000\r\n$31=0\r\n$32=${laserMode}\r\nok\r\n`);
        } else if (line.trim() === "$CD") {
          // FluidNC 配置导出：最小 YAML，让上位机识别为 FluidNC
          socket.write("name: \"E2E Sim\"\r\nplanner_blocks: 16\r\nok\r\n");
        } else if (/^\$RI=\d+$/.test(line.trim())) {
          socket.write("ok\r\n");
        } else if (/^(?:[GMTS]\d+)(?:\s|$)/i.test(line.trim()) || line.trim().startsWith("$")) {
          stats.lines += 1;
          socket.write("ok\r\n");
        } else if (line.trim().length > 0) {
          socket.write("error:3\r\n");
        }
      }
    });
    socket.on("error", () => {});
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", () => resolve({
      server,
      stats,
      close: () => new Promise((closed) => {
        server.close(closed);
        for (const socket of sockets) socket.destroy();
      }),
    }));
  });
}

function checkPortFree(port) {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.once("error", () => resolve(false));
    server.once("listening", () => {
      server.close(() => resolve(true));
    });
    server.listen(port, "127.0.0.1");
  });
}

async function findAvailablePort(preferredPort = 9666) {
  if (await checkPortFree(preferredPort)) {
    return preferredPort;
  }
  for (let p = 19666; p < 19700; p++) {
    if (await checkPortFree(p)) {
      return p;
    }
  }
  throw new Error("无法找到空闲的测试端口");
}

function startChild(file, args, options) {
  const child = spawn(file, args, options);
  let outcome;
  const done = new Promise((resolve) => {
    const finish = (result) => {
      outcome = result;
      resolve(result);
    };
    child.once("exit", (code, signal) => finish({ code, signal }));
    child.on("error", (error) => {
      // 启动失败没有 PID。已启动的进程即使发信号失败，也要以 exit 为准。
      if (!child.pid) finish({ error });
    });
  });
  return { child, done, get outcome() { return outcome; } };
}

async function stopChild(running) {
  if (!running || running.outcome) return true;
  for (const signal of ["SIGTERM", "SIGKILL"]) {
    try {
      running.child.kill(signal);
    } catch (error) {
      console.error(`[E2E] 发送 ${signal} 失败：${error.message}`);
    }
    const timer = new AbortController();
    try {
      await Promise.race([
        running.done,
        sleep(signal === "SIGTERM" ? 2000 : 1000, undefined, { signal: timer.signal }),
      ]);
    } finally {
      timer.abort();
    }
    if (running.outcome) return true;
  }
  // 退出未确认时返回失败并保留目录，避免子进程和其管道继续拖住调度器。
  running.child.stdout?.destroy();
  running.child.stderr?.destroy();
  running.child.unref();
  return false;
}

function childFailure(label, { code, signal, error }) {
  return new Error(error
    ? `${label}启动失败：${error.message}`
    : `${label}提前退出（${signal ?? code}）`);
}

function waitForAbort(signal) {
  return new Promise((_, reject) => {
    if (signal.aborted) reject(signal.reason);
    else signal.addEventListener("abort", () => reject(signal.reason), { once: true });
  });
}

function hostPort(running) {
  const stdout = createInterface({ input: running.child.stdout, crlfDelay: Infinity });
  return new Promise((resolve) => {
    stdout.on("line", (line) => {
      console.log(`[HOST] ${line}`);
      const match = /^moss-host serving on 127\.0\.0\.1:(\d+)$/.exec(line);
      if (!match) return;
      const port = Number(match[1]);
      if (port > 0 && port <= 65535) resolve(port);
    });
  });
}

async function waitForServer(baseUrl, signal) {
  const url = `${baseUrl}/api/system/info`;
  while (true) {
    signal.throwIfAborted();
    try {
      const response = await fetch(url, {
        signal: AbortSignal.any([signal, AbortSignal.timeout(1000)]),
        redirect: "error",
      });
      await response.body?.cancel();
      if (response.ok) return;
    } catch {
      signal.throwIfAborted();
    }
    await sleep(200, undefined, { signal });
  }
}

async function main() {
  const testDir = fs.mkdtempSync(path.join(os.tmpdir(), "moss-e2e-"));
  const interrupt = new AbortController();
  const onSignal = (signal, code) => () => {
    if (interrupt.signal.aborted) return;
    process.exitCode = code;
    interrupt.abort(new Error(`收到 ${signal}，停止 E2E 验证`));
  };
  const sigint = onSignal("SIGINT", 130);
  const sigterm = onSignal("SIGTERM", 143);
  process.on("SIGINT", sigint);
  process.on("SIGTERM", sigterm);

  let fakeGrbl;
  let host;
  let tests;
  try {
    console.log("MOSS E2E 测试调度器");
    const exePath = fs.existsSync(RELEASE_EXE) ? RELEASE_EXE : DEBUG_EXE;
    if (!fs.existsSync(exePath)) {
      throw new Error(`找不到 moss-host：${exePath}。请先运行 cargo build -p moss-host --release`);
    }
    const port = await findAvailablePort(9666);
    fakeGrbl = await startFakeGrblServer(0);
    const grblPort = fakeGrbl.server.address().port;
    interrupt.signal.throwIfAborted();
    console.log(`[E2E] 隔离运行时目录: ${testDir}`);
    console.log(`[E2E] 回环 GRBL 模拟器: 127.0.0.1:${grblPort}`);
    for (const rel of ["moss", path.join("Library", "Application Support", "moss"), path.join(".local", "share", "moss")]) {
      const dir = path.join(testDir, rel);
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, "settings.json"), JSON.stringify({ auto_check_update: false }), "utf8");
    }

    host = startChild(exePath, ["serve"], {
      cwd: testDir,
      env: {
        ...process.env,
        APPDATA: testDir,
        HOME: testDir,
        XDG_DATA_HOME: path.join(testDir, ".local", "share"),
        MCP_ENDPOINT: "disabled-for-loopback-tests",
        LASER_WEB_HOST: "127.0.0.1",
        LASER_WEB_PORT: String(port),
        LASER_MATERIAL_PARAMS_FILE: path.join(testDir, "materials.json"),
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    host.child.stderr.on("data", (chunk) => process.stderr.write(`[HOST-ERR] ${chunk}`));
    const hostEnded = new AbortController();
    host.done.then((result) => hostEnded.abort(childFailure("测试宿主", result)));
    const running = AbortSignal.any([interrupt.signal, hostEnded.signal]);
    const starting = AbortSignal.any([running, AbortSignal.timeout(15000)]);
    const actualPort = await Promise.race([hostPort(host), waitForAbort(starting)]);
    const baseUrl = `http://127.0.0.1:${actualPort}`;
    await waitForServer(baseUrl, starting);
    running.throwIfAborted();
    console.log(`[E2E] 测试宿主已就绪: ${baseUrl}`);

    tests = startChild(process.execPath, ["--test", "--test-concurrency=1", ...resolveSuiteFiles()], {
      cwd: ROOT_DIR,
      env: {
        ...process.env,
        TEST_API_PORT: String(actualPort),
        TEST_API_BASE: baseUrl,
        TEST_APPDATA: testDir,
        TEST_GRBL_PORT: String(grblPort),
      },
      stdio: "inherit",
    });

    const outcome = await Promise.race([tests.done, waitForAbort(running)]);
    interrupt.signal.throwIfAborted();
    if (outcome.error) throw childFailure("测试子进程", outcome);
    process.exitCode = outcome.code ?? 1;
    if (host.outcome) throw childFailure("测试宿主", host.outcome);
    const { lines, connections, feedHolds, resets } = fakeGrbl.stats;
    console.log(`[E2E] GRBL 模拟器: ${connections} 次连接、${lines} 行 G-code、${feedHolds} 次进给保持、${resets} 次软复位`);
    if (process.exitCode === 0 && lines === 0) {
      throw new Error("共享模拟器未接收 G-code，拒绝报告测试通过");
    }
  } finally {
    try {
      const [testsStopped, hostStopped] = await Promise.all([stopChild(tests), stopChild(host)]);
      await fakeGrbl?.close();
      if (!testsStopped || !hostStopped) {
        throw new Error(`未确认测试子进程退出，保留临时目录 ${testDir}`);
      }
      fs.rmSync(testDir, { recursive: true, force: true });
    } finally {
      process.off("SIGINT", sigint);
      process.off("SIGTERM", sigterm);
    }
  }
  if (process.exitCode === 0) console.log("[E2E] 全部端到端用例通过");
}

if (process.argv[1] && fs.existsSync(process.argv[1])
    && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  main().catch((error) => {
    console.error("[E2E] 执行失败", error);
    if (!process.exitCode) process.exitCode = 1;
  });
}
