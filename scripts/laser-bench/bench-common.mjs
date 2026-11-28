// 激光雕刻机 AI 自主测试（L0 模拟机 / L1 真机只读）共享基础模块：
// - 隔离运行目录与 moss-host 子进程生命周期管理（绝不触碰用户日常运行目录）；
// - 统一归档脱敏（密钥、令牌、桌面会话头、本机绝对路径、非回环设备地址与串口名）；
// - 复用语音联调模块的运行目录推断与 ProcessTrace 截取逻辑。

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import net from "node:net";
import { homedir, networkInterfaces, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import {
  defaultRuntimeDir,
  formatRunId,
  isLoopbackUrl,
  mdCell,
  percentile,
  redactText as baseRedactText,
  redactValue as baseRedactValue,
  registerSignalCleanup,
  resolveProcessTraceDir,
  sliceProcessTrace,
  sleep,
  writeTimelineFiles,
} from "../voice-bench/bench-common.mjs";

export {
  defaultRuntimeDir,
  formatRunId,
  isLoopbackUrl,
  mdCell,
  percentile,
  registerSignalCleanup,
  resolveProcessTraceDir,
  sliceProcessTrace,
  sleep,
  writeTimelineFiles,
};

const MODULE_DIR = dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = resolve(MODULE_DIR, "..", "..");

export function sha256Hex(input) {
  return createHash("sha256").update(input).digest("hex");
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * 激光基准归档脱敏：在基础密钥/路径脱敏之外，补充清理桌面会话头、非回环 IPv4 与本机串口路径。
 */
export function redactLaserText(raw, opts = {}) {
  let out = baseRedactText(raw, opts);
  if (!out) return out;
  // 桌面会话头或 64 位十六进制桌面会话值
  out = out.replace(
    /((?:x-moss-desktop-session|desktop[_-]?session)\\?["']?\s*[:=]\s*\\?["']?)[0-9a-f]{16,}/gi,
    "$1[REDACTED]",
  );
  if (Array.isArray(opts.deviceAddresses)) {
    for (const addr of opts.deviceAddresses) {
      if (typeof addr === "string" && addr.length >= 3 && addr !== "127.0.0.1" && addr !== "localhost") {
        out = out.replace(new RegExp(escapeRegExp(addr), "g"), "<device-ip>");
      }
    }
  }
  if (opts.redactDeviceAddresses) {
    // 非回环 IPv4 替换（保留 127.0.0.1 与 0.0.0.0，避免误伤回环模拟器与语义文案）
    out = out.replace(/\b(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})\b/g, (ip) => {
      if (ip === "127.0.0.1" || ip === "0.0.0.0") return ip;
      const parts = ip.split(".").map(Number);
      if (parts.some((n) => n > 255)) return ip;
      return "<device-ip>";
    });
    // 真实串口设备名替换
    out = out.replace(/\/dev\/(?:cu|tty)\.[A-Za-z0-9._-]+/g, "<serial-port>");
    out = out.replace(/\/dev\/tty(?:USB|ACM|S)\d+/g, "<serial-port>");
    out = out.replace(/\bCOM\d{1,3}\b/gi, "<serial-port>");
  }
  return out;
}

/** 递归脱敏任意 JSON 值。 */
export function redactLaserValue(value, opts = {}) {
  if (typeof value === "string") return redactLaserText(value, opts);
  if (Array.isArray(value)) return value.map((item) => redactLaserValue(item, opts));
  if (value && typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      if (
        (/(?:^|[_-])(?:token|secret|password|passwd|api[_-]?key|authorization|desktop[_-]?session)$/i.test(k) ||
          /(?:Token|Secret|Password|ApiKey|SecretKey|DesktopSession)$/.test(k) ||
          /^x-(?:api-key|moss-desktop-session)$/i.test(k)) &&
        typeof v === "string" &&
        v.length > 0
      ) {
        out[k] = "[REDACTED]";
      } else {
        out[k] = redactLaserValue(v, opts);
      }
    }
    return out;
  }
  return baseRedactValue(value, opts);
}

/** 返回隔离归档默认根目录（仓库外的 `<runtime-or-tmp>/laser-bench`）。 */
export function defaultBenchRoot(runtimeDir = null) {
  if (runtimeDir) return join(resolve(runtimeDir), "laser-bench");
  const base = defaultRuntimeDir();
  try {
    mkdirSync(join(base, "laser-bench"), { recursive: true });
    return join(base, "laser-bench");
  } catch {
    return join(tmpdir(), "laser-bench");
  }
}

/** 解析本机已构建的 moss-host 可执行文件路径（优先 release，其次 debug）。 */
export function resolveHostBinary(rootDir = REPO_ROOT) {
  const binName = process.platform === "win32" ? "moss-host.exe" : "moss-host";
  const releaseExe = join(rootDir, "target", "release", binName);
  const debugExe = join(rootDir, "target", "debug", binName);
  if (existsSync(releaseExe)) return releaseExe;
  if (existsSync(debugExe)) return debugExe;
  throw new Error(
    `找不到 moss-host 可执行文件（已检查 ${releaseExe} 与 ${debugExe}）；请先执行 cargo build -p moss-host --release`,
  );
}

/** 检测端口在 127.0.0.1 上是否空闲。 */
export function checkPortFree(port, host = "127.0.0.1") {
  return new Promise((resolvePromise) => {
    const server = net.createServer();
    server.once("error", () => resolvePromise(false));
    server.once("listening", () => server.close(() => resolvePromise(true)));
    server.listen(port, host);
  });
}

/** 分配一个本机临时空闲端口。 */
export function allocateEphemeralPort(host = "127.0.0.1") {
  return new Promise((resolvePromise, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, host, () => {
      const addr = server.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      server.close(() => resolvePromise(port));
    });
  });
}

/** 返回一个可用的本机非回环 IPv4（若当前离线/纯回环则返回 null）。 */
export function findLocalLanIpv4() {
  const ifaces = networkInterfaces();
  for (const entries of Object.values(ifaces)) {
    for (const info of entries || []) {
      if (info.family !== "IPv4" || info.internal) continue;
      const ip = info.address;
      if (!ip || ip.startsWith("127.") || ip.startsWith("169.254.") || ip.startsWith("198.18.") || ip.startsWith("198.19.")) {
        continue;
      }
      return ip;
    }
  }
  return null;
}

/** 根据隔离根目录定位 moss-host 实际使用的运行时子目录（含 settings.json / generated_assets / logs）。 */
export function isolatedMossSubdirs(testDir) {
  return [
    join(testDir, "Library", "Application Support", "moss"),
    join(testDir, "moss"),
    join(testDir, ".local", "share", "moss"),
  ];
}

export function activeIsolatedMossDir(testDir) {
  const candidates = isolatedMossSubdirs(testDir);
  for (const dir of candidates) {
    if (
      existsSync(join(dir, "generated_assets")) ||
      existsSync(join(dir, "logs", "process-trace")) ||
      existsSync(join(dir, "moss.db"))
    ) {
      return dir;
    }
  }
  if (process.platform === "darwin") return candidates[0];
  if (process.platform === "win32") return candidates[1];
  return candidates[2];
}

function startChild(file, args, options) {
  const child = spawn(file, args, options);
  let outcome;
  const done = new Promise((resolvePromise) => {
    const finish = (result) => {
      outcome = result;
      resolvePromise(result);
    };
    child.once("exit", (code, signal) => finish({ code, signal }));
    child.on("error", (error) => {
      if (!child.pid) finish({ error });
    });
  });
  return { child, done, get outcome() { return outcome; } };
}

export async function stopChild(running) {
  if (!running || running.outcome) return true;
  for (const signal of ["SIGTERM", "SIGKILL"]) {
    try {
      running.child.kill(signal);
    } catch {
      // 进程可能已退出
    }
    const timer = new AbortController();
    try {
      await Promise.race([
        running.done,
        delay(signal === "SIGTERM" ? 2500 : 1000, undefined, { signal: timer.signal }),
      ]);
    } catch {
      // ignore abort
    } finally {
      timer.abort();
    }
    if (running.outcome) return true;
  }
  running.child.stdout?.destroy();
  running.child.stderr?.destroy();
  running.child.unref();
  return false;
}

async function waitForHostReady(baseUrl, signal) {
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
    await delay(120, undefined, { signal });
  }
}

/**
 * 启动一个完全隔离的 moss-host serve 实例，使用独立临时运行目录，不接触用户日常配置与数据。
 */
export async function startIsolatedHost({
  exePath = resolveHostBinary(),
  runtimeDir,
  webHost = "127.0.0.1",
  port = 0,
  enableProcessTrace = true,
  strictMode = false,
  envOverrides = {},
  timeoutMs = 15_000,
} = {}) {
  if (!runtimeDir) {
    throw new Error("startIsolatedHost 必须传入独立的临时 runtimeDir");
  }
  const actualPreferredPort = port > 0 ? port : await allocateEphemeralPort(webHost === "0.0.0.0" ? "127.0.0.1" : webHost);
  const settingsObj = {
    auto_check_update: false,
    process_trace_enabled: Boolean(enableProcessTrace),
  };
  for (const dir of isolatedMossSubdirs(runtimeDir)) {
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "settings.json"), JSON.stringify(settingsObj, null, 2), "utf8");
  }

  const env = {
    ...process.env,
    APPDATA: runtimeDir,
    HOME: runtimeDir,
    USERPROFILE: runtimeDir,
    XDG_DATA_HOME: join(runtimeDir, ".local", "share"),
    MCP_ENDPOINT: "disabled-for-laser-bench",
    LASER_WEB_HOST: webHost,
    LASER_WEB_PORT: String(actualPreferredPort),
    LASER_MATERIAL_PARAMS_FILE: join(runtimeDir, "materials.json"),
    MOSS_FUSION_STRICT: strictMode ? "true" : "false",
    ...envOverrides,
  };
  // 清除外部可能残留的真实设备环境变量，确保完全隔离
  delete env.GRBL_DEFAULT_PORT;
  delete env.LASER_NETWORK_HOST;
  delete env.LASER_NETWORK_TELNET_PORT;
  delete env.MOSS_RUNTIME_DIR;

  const stdoutLines = [];
  const stderrLines = [];
  const running = startChild(exePath, ["serve"], {
    cwd: runtimeDir,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });

  const hostEnded = new AbortController();
  running.done.then((result) => {
    hostEnded.abort(
      new Error(
        result.error
          ? `测试宿主启动失败：${result.error.message}`
          : `测试宿主提前退出（code=${result.code}, signal=${result.signal}）`,
      ),
    );
  });

  const portPromise = new Promise((resolvePromise) => {
    const rl = createInterface({ input: running.child.stdout, crlfDelay: Infinity });
    rl.on("line", (line) => {
      stdoutLines.push(line);
      const m = /^moss-host serving on [^:]+:(\d+)$/.exec(line.trim());
      if (m) {
        const p = Number(m[1]);
        if (p > 0 && p <= 65535) resolvePromise(p);
      }
    });
  });
  running.child.stderr.on("data", (chunk) => {
    stderrLines.push(String(chunk));
  });

  const startupSignal = AbortSignal.any([hostEnded.signal, AbortSignal.timeout(timeoutMs)]);
  const waitAbort = new Promise((_, reject) => {
    if (startupSignal.aborted) reject(startupSignal.reason);
    else startupSignal.addEventListener("abort", () => reject(startupSignal.reason), { once: true });
  });

  let actualPort;
  try {
    actualPort = await Promise.race([portPromise, waitAbort]);
    const loopbackBase = `http://127.0.0.1:${actualPort}`;
    await waitForHostReady(loopbackBase, startupSignal);
  } catch (err) {
    await stopChild(running);
    const tail = stderrLines.join("").trim().slice(-400);
    throw new Error(`${err.message}${tail ? `\n[HOST-ERR] ${tail}` : ""}`);
  }

  return {
    port: actualPort,
    baseUrl: `http://127.0.0.1:${actualPort}`,
    runtimeDir,
    mossDir: () => activeIsolatedMossDir(runtimeDir),
    stdoutLines,
    stderrLines,
    running,
    async stop() {
      return stopChild(running);
    },
  };
}

/** 读取指定运行目录中所有候选路径下的 settings.json（返回首个存在且合法的对象）。 */
export function readRuntimeSettings(runtimeDir) {
  const candidates = [join(runtimeDir, "settings.json"), ...isolatedMossSubdirs(runtimeDir).map((d) => join(d, "settings.json"))];
  for (const file of candidates) {
    if (!existsSync(file)) continue;
    try {
      const parsed = JSON.parse(readFileSync(file, "utf8"));
      if (parsed && typeof parsed === "object") return { file, data: parsed };
    } catch {
      // 忽略损坏配置
    }
  }
  return { file: join(runtimeDir, "settings.json"), data: {} };
}

/** 通用脱敏选项构造器（默认包含运行目录与用户主目录）。 */
export function defaultRedactOpts({ runtimeDir = "", extraPaths = [], extraSecrets = [], deviceAddresses = [], redactDeviceAddresses = false } = {}) {
  const paths = [homedir(), tmpdir()];
  if (runtimeDir) paths.push(runtimeDir);
  for (const p of extraPaths) {
    if (p) paths.push(p);
  }
  return {
    paths,
    secrets: extraSecrets,
    deviceAddresses,
    redactDeviceAddresses,
  };
}
