#!/usr/bin/env node
import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import process from "node:process";

import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT_DIR = path.resolve(SCRIPT_DIR, "..");
const RELEASE_EXE = path.join(ROOT_DIR, "target", "release", "moss-host.exe");
const DEBUG_EXE = path.join(ROOT_DIR, "target", "debug", "moss-host.exe");
const TEST_DIR = fs.mkdtempSync(path.join(os.tmpdir(), "moss-e2e-"));
const cleanupTestDir = () => fs.rmSync(TEST_DIR, { recursive: true, force: true });

/**
 * 回环 GRBL 模拟器：在 127.0.0.1 上监听 Telnet 端口，
 * 收到 `$$` 回固件设置（含 `$32=1`），收到实时字节 `?` 回状态报告，
 * 只对 GRBL/FluidNC G-code 指令回 `ok`；收到非法行返回 error。收到 0x18 记录急停，
 * 用于让 E2E 真正走 TCP 传输 + 握手 + 逐行 ack。
 */
function startFakeGrblServer(port, opts = {}) {
  const laserMode = opts.laserMode ?? 1;
  const stats = { lines: 0, resets: 0, feedHolds: 0, connections: 0 };
  const server = net.createServer((socket) => {
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
    server.listen(port, "127.0.0.1", () => resolve({ server, stats }));
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

async function waitForServer(url, timeoutMs = 15000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await fetch(url);
      if (res.ok) {
        return true;
      }
    } catch {
      // Retry after delay
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

async function main() {
  console.log("===============================================================");
  console.log(" MOSS App 按钮与全功能端到端 (E2E) 自动化测试调度器");
  console.log("===============================================================\n");

  const exePath = fs.existsSync(RELEASE_EXE) ? RELEASE_EXE : DEBUG_EXE;
  if (!fs.existsSync(exePath)) {
    console.error(`❌ 找不到 moss-host 可执行文件: ${exePath}`);
    console.error("请先运行: cargo build -p moss-host --release");
    cleanupTestDir();
    process.exit(1);
  }

  // 1. 准备测试隔离环境
  const port = await findAvailablePort(9666);
  const baseUrl = `http://127.0.0.1:${port}`;
  console.log(`[E2E] 选定测试端口: ${port}`);
  console.log(`[E2E] 隔离运行时目录: ${TEST_DIR}`);
  console.log(`[E2E] 宿主可执行程序: ${exePath}`);

  // 1.5 启动回环 GRBL 模拟器（真实 TCP 传输 + $32 握手 + 逐行 ok）
  const grblPort = await findAvailablePort(23230);
  const fakeGrbl = await startFakeGrblServer(grblPort);
  console.log(`[E2E] 回环 GRBL 模拟器已监听: 127.0.0.1:${grblPort}`);

  // 2. 启动真实后端服务
  console.log(`[E2E] 正在启动后端服务 (moss-host.exe serve)...`);
  const hostEnv = {
    ...process.env,
    APPDATA: TEST_DIR,
    MCP_ENDPOINT: "disabled-for-loopback-tests",
    LASER_WEB_HOST: "127.0.0.1",
    LASER_WEB_PORT: String(port),
    LASER_MATERIAL_PARAMS_FILE: path.join(TEST_DIR, "materials.json"),
  };

  const hostProc = spawn(exePath, ["serve"], {
    cwd: ROOT_DIR,
    env: hostEnv,
    stdio: ["pipe", "pipe", "pipe"],
  });

  let detectedPort = null;
  hostProc.stdout.on("data", (d) => {
    const str = d.toString();
    process.stdout.write(`[HOST] ${str}`);
    const match = str.match(/moss-host serving on [^:]+:(\d+)/);
    if (match && match[1]) {
      detectedPort = Number(match[1]);
    }
  });
  hostProc.stderr.on("data", (d) => {
    const str = d.toString();
    process.stderr.write(`[HOST-ERR] ${str}`);
  });

  let hostExited = false;
  hostProc.on("exit", (code) => {
    hostExited = true;
    if (code !== 0 && code !== null) {
      console.warn(`[HOST] 后端进程非零退出: ${code}`);
    }
  });

  // 3. 等待服务就绪并动态探测实际绑定端口
  console.log(`[E2E] 正在等待服务输出绑定端口并进行健康探针检查...`);
  const startWait = Date.now();
  while (!detectedPort && Date.now() - startWait < 5000) {
    await new Promise((r) => setTimeout(r, 100));
  }
  const actualPort = detectedPort || port;
  const actualBaseUrl = `http://127.0.0.1:${actualPort}`;
  console.log(`[E2E] 确认实际通信服务地址: ${actualBaseUrl}`);

  const isReady = await waitForServer(`${actualBaseUrl}/api/system/info`, 10000);
  if (!isReady) {
    console.error(`❌ 后端服务未能于 10 秒内就绪！`);
    if (!hostExited) hostProc.kill();
    fakeGrbl.server.close();
    cleanupTestDir();
    process.exit(1);
  }
  console.log(`✓ 后端服务探针响应 200 OK，测试基座已就绪！\n`);

  // 4. 执行核心 API 与硬件通信工作流端到端真实测试套件
  console.log(`===============================================================`);
  console.log(` 开始执行核心 API 与硬件通信工作流真实端到端断言测试套件`);
  console.log(`===============================================================\n`);

  const testEnv = {
    ...process.env,
    TEST_API_PORT: String(actualPort),
    TEST_API_BASE: actualBaseUrl,
    TEST_APPDATA: TEST_DIR,
    TEST_GRBL_PORT: String(grblPort),
  };

  const testProc = spawn(
    process.execPath,
    ["--test", "frontend/tests/e2e/buttons.test.mjs"],
    {
      cwd: ROOT_DIR,
      env: testEnv,
      stdio: "inherit",
    }
  );

  let testExitCode = await new Promise((resolve) => {
    testProc.on("exit", (code) => resolve(code ?? 1));
  });

  console.log(`\n[E2E] GRBL 模拟器统计: 连接 ${fakeGrbl.stats.connections} 次, 收到 G-code ${fakeGrbl.stats.lines} 行, 进给保持 ${fakeGrbl.stats.feedHolds} 次, 0x18 软复位 ${fakeGrbl.stats.resets} 次`);
  if (testExitCode === 0 && (fakeGrbl.stats.lines === 0 || fakeGrbl.stats.resets === 0 || fakeGrbl.stats.feedHolds === 0)) {
    console.error("[E2E] 模拟器未观察到完整的 G-code/暂停/0x18 事件，拒绝报告测试通过。");
    testExitCode = 1;
  }
  fakeGrbl.server.close();

  // 5. 优雅退出后端服务
  console.log(`\n[E2E] 正在安全回收测试后端进程 (PID: ${hostProc.pid})...`);
  try {
    // 先触发优雅停止
    await fetch(`${actualBaseUrl}/api/workflow`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ action: "cancel", workflow_id: "all" }),
    }).catch(() => {});
  } catch {}

  if (!hostExited) {
    hostProc.kill("SIGTERM");
    // 等待 1 秒若未退出则强制杀死
    await new Promise((r) => setTimeout(r, 1000));
    if (!hostExited) {
      hostProc.kill("SIGKILL");
    }
  }

  console.log(`[E2E] 后端服务已安全回收。\n`);
  cleanupTestDir();

  if (testExitCode === 0) {
    console.log("===============================================================");
    console.log(" ✅ MOSS App 32 项真实端到端闭环测试全部通过！");
    console.log("===============================================================");
  } else {
    console.error("===============================================================");
    console.error(` ❌ 部分按钮或功能测试未通过 (Exit Code: ${testExitCode})`);
    console.error("===============================================================");
  }

  process.exit(testExitCode);
}

main().catch((err) => {
  console.error("执行异常:", err);
  cleanupTestDir();
  process.exit(1);
});
