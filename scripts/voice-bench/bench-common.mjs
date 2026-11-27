// VA4 / VC1 / VC2 / VC3 语音本机联调与整体测试共享工具模块：
// - 统一运行目录推断、全量脱敏（密钥 / 令牌 / 凭据 URL / 本机绝对路径）、ProcessTrace 截取；
// - 统一 127.0.0.1 本机调试入口鉴权连接、设备查询、测试时间窗开关、轮次明细轮询与过程记录切换；
// - 供 `live-xiaozhi-debug.mjs`、`asr-bench.mjs` 与 `xiaozhi-e2e.mjs` 复用，避免重复实现。

import { existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

export const NO_ONLINE_DEVICE_MESSAGE = "请先唤醒小智（喊‘你好小智’或按对话键），或开启常连模式";
export const DEFAULT_DEBUG_PORT = 18009;
export const DEFAULT_TURN_TIMEOUT_MS = 90_000;
export const DEFAULT_IDLE_WAIT_MS = 30_000;
export const DEFAULT_POLL_INTERVAL_MS = 250;

/** 与 `laser_runtime::RuntimeRoot::default_base` 一致的运行目录推断。 */
export function defaultRuntimeDir(platform = process.platform, env = process.env, home = homedir()) {
  if (env.MOSS_RUNTIME_DIR && String(env.MOSS_RUNTIME_DIR).trim()) {
    return resolve(String(env.MOSS_RUNTIME_DIR).trim());
  }
  if (platform === "darwin") return join(home, "Library", "Application Support", "moss");
  if (platform === "win32") return join(env.APPDATA ?? join(home, "AppData", "Roaming"), "moss");
  return join(env.XDG_DATA_HOME ?? join(home, ".local", "share"), "moss");
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * 脱敏文本：清除 API 密钥、Bearer/设备令牌、带凭据 URL 与本机绝对路径（对齐仓库 `redact.rs` 与 `check-repo-hygiene.mjs`）。
 */
export function redactText(raw, opts = {}) {
  let out = String(raw ?? "");
  if (!out) return out;
  const extraSecrets = Array.isArray(opts.secrets)
    ? opts.secrets.filter((s) => typeof s === "string" && s.length >= 8)
    : [];
  for (const secret of extraSecrets) {
    out = out.replace(new RegExp(escapeRegExp(secret), "g"), "[REDACTED]");
  }
  const extraPaths = Array.isArray(opts.paths)
    ? [
        ...new Set(
          opts.paths
            .filter((p) => typeof p === "string" && p.length >= 2)
            .flatMap((p) => [p, p.replace(/\\/g, "/"), p.replace(/\\/g, "\\\\")]),
        ),
      ].sort((a, b) => b.length - a.length)
    : [];
  for (const pathVal of extraPaths) {
    out = out.replace(new RegExp(escapeRegExp(pathVal), "g"), "<path>");
  }
  // URL 内嵌账号密码
  out = out.replace(/(https?|wss?):\/\/[^/\s@]+:[^/\s@]+@/gi, "$1://[REDACTED]@");
  // URL 查询参数里的敏感项
  out = out.replace(
    /([?&](?:api[_-]?key|apikey|token|access_token|secret|password|sig|key)=)[^&#\s"'<>]+/gi,
    "$1[REDACTED]",
  );
  // Bearer / Basic 令牌
  out = out.replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{6,}/gi, "$1 [REDACTED]");
  // 常见键值对凭据（JSON / 转义 JSON / header / CLI 参数）
  out = out.replace(
    /((?:x-api-key|api[_-]?key|apikey|authorization|access[_-]?token|device[_-]?token|token|secret(?:[_-]?key)?|password|passwd)\\?["']?\s*[:=]\s*\\?["']?)(?:bearer\s+|basic\s+)?[^\s"',;&}\\)]+/gi,
    "$1[REDACTED]",
  );
  out = out.replace(/(--(?:token|api-key|secret|password)\s+)[^\s"']+/gi, "$1[REDACTED]");
  // 常见密钥前缀与本机凭据包装
  out = out.replace(/\b(?:sk|rk|pk)-[A-Za-z0-9_-]{16,}\b/g, "[REDACTED]");
  out = out.replace(/\bgh[pousr]_[A-Za-z0-9]{20,}\b/g, "[REDACTED]");
  out = out.replace(/\b(?:dpapi-user-v1|keychain-user-v1):[A-Za-z0-9+/=_-]+/g, "[REDACTED]");
  // 本机绝对路径（macOS / Linux / Windows，含 JSON 双反斜杠与正斜杠形式）
  out = out.replace(/(?:\/Users|\/home|\/var\/folders|\/private\/var|\/tmp)\/[^\s"'<>`,;)\]]+/g, "<path>");
  out = out.replace(
    /\b[A-Za-z]:(?:\\\\|\\|\/)(?:Users|Documents and Settings|ProgramData|Temp|Windows)[^\s"'<>`,;)\]]*/gi,
    "<path>",
  );
  return out;
}

/** 递归脱敏任意 JSON 对象。 */
export function redactValue(value, opts = {}) {
  if (typeof value === "string") return redactText(value, opts);
  if (Array.isArray(value)) return value.map((item) => redactValue(item, opts));
  if (value && typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      if (
        (/(?:^|[_-])(?:token|secret|password|passwd|api[_-]?key|authorization)$/i.test(k) ||
          /(?:Token|Secret|Password|ApiKey|SecretKey)$/.test(k) ||
          /^x-api-key$/i.test(k)) &&
        typeof v === "string" &&
        v.length > 0
      ) {
        out[k] = "[REDACTED]";
      } else {
        out[k] = redactValue(v, opts);
      }
    }
    return out;
  }
  return value;
}

/**
 * 为长跑测评脚本注册幂等的 SIGINT / SIGTERM 异步清理钩子（避免 Ctrl+C 跳过 finally 残留时间窗/开关/子进程）。
 * 返回卸载函数，务必在 finally 中调用以防单测场景累积监听器。
 */
export function registerSignalCleanup(cleanupFn, timeoutMs = 5_000) {
  let triggered = false;
  const makeHandler = (code) => () => {
    if (triggered) return;
    triggered = true;
    Promise.race([
      Promise.resolve().then(() => cleanupFn()),
      new Promise((r) => setTimeout(r, timeoutMs)),
    ])
      .catch(() => {})
      .finally(() => {
        process.exit(code);
      });
  };
  const onSigInt = makeHandler(130);
  const onSigTerm = makeHandler(143);
  process.on("SIGINT", onSigInt);
  process.on("SIGTERM", onSigTerm);
  return () => {
    process.off("SIGINT", onSigInt);
    process.off("SIGTERM", onSigTerm);
  };
}

export function percentile(values, p) {
  const nums = values.filter((v) => typeof v === "number" && Number.isFinite(v)).sort((a, b) => a - b);
  if (nums.length === 0) return null;
  const idx = Math.min(nums.length - 1, Math.max(0, Math.ceil((p / 100) * nums.length) - 1));
  return nums[idx];
}

export function formatRunId(prefix = "", date = new Date()) {
  const pad = (n, w = 2) => String(n).padStart(w, "0");
  const stamp = [
    date.getFullYear(),
    pad(date.getMonth() + 1),
    pad(date.getDate()),
    "-",
    pad(date.getHours()),
    pad(date.getMinutes()),
    pad(date.getSeconds()),
    "-",
    pad(date.getMilliseconds(), 3),
  ].join("");
  return prefix ? `${prefix}-${stamp}` : stamp;
}

export function localDayString(ms) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

export function mdCell(text, maxLen = 80) {
  const clean = String(text ?? "").replace(/[\r\n|]+/g, " ").trim();
  if (!clean) return "—";
  return clean.length > maxLen ? `${clean.slice(0, maxLen)}…` : clean;
}

export function sleep(ms) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, ms));
}

/** 读取 settings.json 中的自定义 process_trace_dir，未设则返回 `<runtime>/logs/process-trace`。 */
export function resolveProcessTraceDir(runtimeDir) {
  const settingsFile = join(runtimeDir, "settings.json");
  if (existsSync(settingsFile)) {
    try {
      const parsed = JSON.parse(readFileSync(settingsFile, "utf8"));
      const custom = typeof parsed?.process_trace_dir === "string" ? parsed.process_trace_dir.trim() : "";
      if (custom && existsSync(custom)) return custom;
    } catch {
      // 忽略损坏配置，回退默认目录
    }
  }
  return join(runtimeDir, "logs", "process-trace");
}

function parseTimelineTxtTimeMs(line, dayStr) {
  const isoMatch = line.match(/^\[?(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?)\]?/);
  if (isoMatch) {
    const parsed = Date.parse(isoMatch[1]);
    if (Number.isFinite(parsed)) return parsed;
  }
  const hmsMatch = line.match(/^\[?(\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?)\]?/);
  if (hmsMatch && dayStr) {
    const parsed = Date.parse(`${dayStr}T${hmsMatch[1]}`);
    if (Number.isFinite(parsed)) return parsed;
  }
  return null;
}

/** 截取 `[startMs, endMs]` 时间窗内的 ProcessTrace（timeline.jsonl 与 timeline.txt），并执行脱敏。 */
export function sliceProcessTrace(traceDir, startMs, endMs, redactOpts = {}) {
  const jsonlLines = [];
  const txtLines = [];
  if (!traceDir || !existsSync(traceDir)) {
    return { jsonlLines, txtLines };
  }
  const minMs = startMs - 1000;
  const maxMs = endMs + 2000;
  const startDay = localDayString(minMs);
  const endDay = localDayString(maxMs);
  const dayDirs = readdirSync(traceDir)
    .filter((name) => /^\d{4}-\d{2}-\d{2}$/.test(name) && name >= startDay && name <= endDay)
    .filter((name) => {
      try {
        return statSync(join(traceDir, name)).isDirectory();
      } catch {
        return false;
      }
    })
    .sort();

  const matchedSeqs = new Set();
  for (const day of dayDirs) {
    const jsonlPath = join(traceDir, day, "timeline.jsonl");
    if (existsSync(jsonlPath)) {
      for (const rawLine of readFileSync(jsonlPath, "utf8").split("\n")) {
        const line = rawLine.trim();
        if (!line) continue;
        try {
          const obj = JSON.parse(line);
          const atMs = typeof obj?.at === "string" ? Date.parse(obj.at) : Number(obj?.at_ms);
          if (Number.isFinite(atMs) && atMs >= minMs && atMs <= maxMs) {
            if (obj?.seq !== undefined && obj?.seq !== null) matchedSeqs.add(String(obj.seq));
            jsonlLines.push(JSON.stringify(redactValue(obj, redactOpts)));
          }
        } catch {
          // 跳过截断行
        }
      }
    }
    const txtPath = join(traceDir, day, "timeline.txt");
    if (existsSync(txtPath)) {
      for (const rawLine of readFileSync(txtPath, "utf8").split("\n")) {
        if (!rawLine.trim()) continue;
        const seqMatch = rawLine.match(/#(\d+)\b/);
        const timeMs = parseTimelineTxtTimeMs(rawLine, day);
        const keepByTime = Number.isFinite(timeMs) && timeMs >= minMs && timeMs <= maxMs;
        const keepBySeq = seqMatch && matchedSeqs.has(seqMatch[1]);
        if (keepByTime || keepBySeq) {
          txtLines.push(redactText(rawLine, redactOpts));
        }
      }
    }
  }
  return { jsonlLines, txtLines };
}

export function isLoopbackUrl(raw) {
  try {
    const parsed = new URL(raw);
    return (
      parsed.protocol === "http:" &&
      (parsed.hostname === "127.0.0.1" || parsed.hostname === "localhost" || parsed.hostname === "[::1]")
    );
  } catch {
    return false;
  }
}

export function resolveDebugConnection(opts = {}) {
  const runtimeDir = opts.runtimeDir ? resolve(opts.runtimeDir) : defaultRuntimeDir();
  const tokenFile = opts.tokenFile ? resolve(opts.tokenFile) : join(runtimeDir, "debug", "voice-debug.token");
  const metaFile = join(runtimeDir, "debug", "voice-debug.json");
  if (!existsSync(tokenFile)) {
    throw new Error(
      `找不到调试令牌文件（${redactText(tokenFile, { paths: [runtimeDir, homedir()] })}）；请先在桌面端“语音中心 › 记录与诊断 › AI 联调与测试”开启“允许本机 AI 调试注入”`,
    );
  }
  const token = readFileSync(tokenFile, "utf8").trim();
  if (!token) {
    throw new Error("调试令牌文件为空；请在桌面端重新开启“允许本机 AI 调试注入”");
  }
  let baseUrl = opts.url;
  if (!baseUrl) {
    let port = DEFAULT_DEBUG_PORT;
    if (existsSync(metaFile)) {
      try {
        const meta = JSON.parse(readFileSync(metaFile, "utf8"));
        if (Number.isInteger(meta?.port) && meta.port > 0) port = meta.port;
      } catch {
        // 回退默认端口
      }
    }
    baseUrl = `http://127.0.0.1:${port}`;
  }
  baseUrl = baseUrl.replace(/\/+$/, "");
  if (!isLoopbackUrl(baseUrl)) {
    throw new Error(`调试入口地址必须为本机回环 http://127.0.0.1:<port>，拒绝访问：${baseUrl}`);
  }
  return { runtimeDir, token, baseUrl };
}

export async function debugFetch(
  baseUrl,
  token,
  path,
  { method = "GET", body = undefined, timeoutMs = 15_000 } = {},
) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(`${baseUrl}${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
      signal: controller.signal,
    });
    const text = await response.text();
    let json = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = { raw: text };
    }
    return { ok: response.ok, status: response.status, data: json };
  } finally {
    clearTimeout(timer);
  }
}

export function listDebugDevices(baseUrl, token) {
  return debugFetch(baseUrl, token, "/debug/v1/devices");
}

export function listDeviceTurns(baseUrl, token, deviceId, limit = 20) {
  return debugFetch(
    baseUrl,
    token,
    `/debug/v1/devices/${encodeURIComponent(deviceId)}/turns?limit=${Math.max(1, Math.min(50, limit))}`,
  );
}

export function getTurnDetail(baseUrl, token, turnId) {
  return debugFetch(baseUrl, token, `/debug/v1/turns/${encodeURIComponent(turnId)}`);
}

export function setProcessTrace(baseUrl, token, enabled, flush) {
  return debugFetch(baseUrl, token, "/debug/v1/process-trace", {
    method: "POST",
    body: { enabled, flush },
  });
}

export function controlBenchWindow(baseUrl, token, { deviceId, action, maxMinutes = undefined }) {
  const body = { device_id: deviceId, action };
  if (action === "open" && maxMinutes !== undefined) body.max_minutes = maxMinutes;
  return debugFetch(baseUrl, token, "/debug/v1/bench-window", {
    method: "POST",
    body,
  });
}

export function writeTimelineFiles(archiveDir, jsonlLines, txtLines) {
  mkdirSync(archiveDir, { recursive: true });
  writeFileSync(
    join(archiveDir, "timeline.jsonl"),
    jsonlLines.join("\n") + (jsonlLines.length ? "\n" : ""),
    "utf8",
  );
  writeFileSync(
    join(archiveDir, "timeline.txt"),
    txtLines.join("\n") + (txtLines.length ? "\n" : ""),
    "utf8",
  );
}
