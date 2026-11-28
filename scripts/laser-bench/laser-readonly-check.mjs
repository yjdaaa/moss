#!/usr/bin/env node
// LA2: 激光雕刻机 L1 真机只读核对脚本。
// - 仅通过本机正在运行的 MOSS 回环接口执行，绝不新增绕过鉴权的入口；
// - 全部请求必须经只读白名单校验（仅允许只读查询、握手探测 $$ / $CD / $I 与 safe-action(status)）；
// - 绝不调用任何发送、移动、safe-action（status 除外）、倍率、急停、解锁或回零接口；
// - 预检若存在活动任务或待确认停机记录，立即停止且不做任何设备探测。

import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  defaultBenchRoot,
  defaultRedactOpts,
  defaultRuntimeDir,
  formatRunId,
  isLoopbackUrl,
  mdCell,
  readRuntimeSettings,
  redactLaserText,
  redactLaserValue,
  resolveProcessTraceDir,
  sliceProcessTrace,
  writeTimelineFiles,
} from "./bench-common.mjs";
import { SEVERITY_RANK } from "./invariants.mjs";

export const READONLY_GET_ROUTES = Object.freeze([
  "/api/health",
  "/api/system/info",
  "/api/jobs/stop-targets",
  "/api/ui/runtime-config",
  "/api/ui/serial-ports",
]);

export const READONLY_POST_ROUTES = Object.freeze([
  "/api/connection/check",
  "/api/safe-action",
]);

const FORBIDDEN_SAFE_ACTION_FIELDS = new Set([
  "x",
  "y",
  "z",
  "feed_rate",
  "power",
  "confirmed",
  "gcode",
  "lines",
  "command",
  "duration_ms",
  "outline",
]);

export class ReadOnlyWhitelistError extends Error {
  constructor(message, detail = {}) {
    super(message);
    this.name = "ReadOnlyWhitelistError";
    this.code = "readonly_whitelist_rejected";
    this.detail = detail;
  }
}

/**
 * L1 只读白名单守卫：任何发往 MOSS 的 HTTP 请求必须先经此校验，否则直接抛错拒绝发出。
 */
export function assertReadOnlyRequest(method, route, body = undefined) {
  const normMethod = String(method || "GET").trim().toUpperCase();
  const rawRoute = String(route || "").trim();
  if (!rawRoute.startsWith("/") || rawRoute.includes("..") || rawRoute.includes("\\") || /[\r\n]/.test(rawRoute)) {
    throw new ReadOnlyWhitelistError(`非法路由路径：${rawRoute}`, { method: normMethod, route: rawRoute });
  }
  const pathname = new URL(rawRoute, "http://127.0.0.1").pathname;

  if (normMethod === "GET") {
    if (!READONLY_GET_ROUTES.includes(pathname)) {
      throw new ReadOnlyWhitelistError(`L1 只读白名单拒绝 GET ${pathname}`, { method: normMethod, pathname });
    }
    return { method: normMethod, pathname };
  }

  if (normMethod === "POST") {
    if (!READONLY_POST_ROUTES.includes(pathname)) {
      throw new ReadOnlyWhitelistError(`L1 只读白名单拒绝 POST ${pathname}（禁止发送、移动、改设置等写操作）`, {
        method: normMethod,
        pathname,
      });
    }
    if (body !== undefined && (typeof body !== "object" || body === null || Array.isArray(body))) {
      throw new ReadOnlyWhitelistError(`POST ${pathname} 请求体必须为 JSON 对象`, { method: normMethod, pathname });
    }
    const obj = body || {};
    if (pathname === "/api/safe-action") {
      const action = typeof obj.action === "string" ? obj.action.trim().toLowerCase() : "";
      if (action !== "status") {
        throw new ReadOnlyWhitelistError(
          `L1 只读白名单拒绝 /api/safe-action 的 action="${action || "(空)"}"（仅允许 status）`,
          { method: normMethod, pathname, action },
        );
      }
      for (const key of Object.keys(obj)) {
        if (FORBIDDEN_SAFE_ACTION_FIELDS.has(key)) {
          throw new ReadOnlyWhitelistError(`L1 只读白名单拒绝 /api/safe-action 夹带非只读字段 "${key}"`, {
            method: normMethod,
            pathname,
            field: key,
          });
        }
      }
      return { method: normMethod, pathname, action: "status" };
    }
    // /api/connection/check：仅允许连接探测参数
    for (const key of ["gcode", "command", "lines", "confirmed"]) {
      if (Object.hasOwn(obj, key)) {
        throw new ReadOnlyWhitelistError(`L1 只读白名单拒绝 /api/connection/check 夹带字段 "${key}"`, {
          method: normMethod,
          pathname,
          field: key,
        });
      }
    }
    return { method: normMethod, pathname };
  }

  throw new ReadOnlyWhitelistError(`L1 只读白名单拒绝 HTTP 方法 ${normMethod}`, { method: normMethod, pathname });
}

/** 构造只允许本机回环 + 只读白名单请求的 HTTP 客户端。 */
export function createReadOnlyClient(baseUrl, { fetchImpl = fetch, timeoutMs = 10_000, onStep = null } = {}) {
  const cleaned = String(baseUrl || "").trim().replace(/\/+$/, "");
  if (!isLoopbackUrl(cleaned)) {
    throw new ReadOnlyWhitelistError(`L1 只读核对只允许访问本机回环 http://127.0.0.1:<port>，拒绝：${cleaned}`, {
      baseUrl: cleaned,
    });
  }
  return {
    baseUrl: cleaned,
    async request(method, route, body = undefined) {
      assertReadOnlyRequest(method, route, body);
      const started = Date.now();
      const response = await fetchImpl(`${cleaned}${route}`, {
        method: String(method).toUpperCase(),
        headers: {
          Accept: "application/json",
          "X-Moss-Bench": "readonly",
          ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
        },
        body: body !== undefined ? JSON.stringify(body) : undefined,
        signal: AbortSignal.timeout(timeoutMs),
      });
      const text = await response.text();
      let data = null;
      try {
        data = text ? JSON.parse(text) : null;
      } catch {
        data = { raw: text.slice(0, 400) };
      }
      const step = {
        tsMs: started,
        method: String(method).toUpperCase(),
        route,
        status: response.status,
        elapsedMs: Date.now() - started,
        request: body ?? null,
        response: data,
      };
      if (typeof onStep === "function") onStep(step);
      return { ok: response.ok, status: response.status, data, elapsedMs: step.elapsedMs };
    },
  };
}

/** 解析 `<State|MPos:...|Bf:...|Ov:...|Pn:...>` 状态报告。 */
export function parseStatusReportText(raw) {
  const trimmed = String(raw || "").trim();
  const m = /^<([^>]+)>$/.exec(trimmed);
  if (!m) return null;
  const parts = m[1].split("|");
  const state = parts[0]?.trim() || "Unknown";
  const out = { state, mpos: null, wpos: null, bf: null, fs: null, ov: null, pins: "", raw: trimmed };
  for (const field of parts.slice(1)) {
    if (field.startsWith("MPos:")) out.mpos = field.slice(5).split(",").map(Number);
    else if (field.startsWith("WPos:")) out.wpos = field.slice(5).split(",").map(Number);
    else if (field.startsWith("Bf:")) out.bf = field.slice(3).split(",").map(Number);
    else if (field.startsWith("FS:")) out.fs = field.slice(3).split(",").map(Number);
    else if (field.startsWith("Ov:")) out.ov = field.slice(3).split(",").map(Number);
    else if (field.startsWith("Pn:")) out.pins = field.slice(3).trim();
  }
  return out;
}

const COMPARE_LIMIT_FIELDS = [
  ["x_travel_mm", "X 轴行程 (mm)"],
  ["y_travel_mm", "Y 轴行程 (mm)"],
  ["x_max_rate_mm_per_min", "X 最大速度 (mm/min)"],
  ["y_max_rate_mm_per_min", "Y 最大速度 (mm/min)"],
  ["x_accel_mm_s2", "X 加速度 (mm/s²)"],
  ["y_accel_mm_s2", "Y 加速度 (mm/s²)"],
  ["junction_deviation_mm", "转角偏差 (mm)"],
];

/** 分析只读探测结果、状态报告与 MOSS 记住的机器参数，产出分级发现清单与参数比对表。 */
export function analyzeReadOnlySnapshot({ probe = null, statusReport = null, remembered = null } = {}) {
  const findings = [];
  const addFinding = (id, severity, title, message, { evidence = null, suspectedCode = null } = {}) => {
    findings.push({ id, severity, status: severity === "info" ? "notice" : "fail", title, message, evidence, suspectedCode });
  };

  const limits = probe?.machine_limits || null;
  const features = probe?.features || null;
  const identity = probe?.identity || null;
  const softLimits = probe?.soft_limits_enabled ?? limits?.soft_limits_enabled ?? null;
  const homingEnabled = probe?.homing_enabled ?? limits?.homing_enabled ?? null;
  const hasFullTravel = Number.isFinite(limits?.x_travel_mm) && Number.isFinite(limits?.y_travel_mm);

  // 1. 固件行程与软限位风险
  if (!hasFullTravel && softLimits !== true) {
    addFinding(
      "no_travel_and_soft_limits_off",
      "safety",
      "固件未完整报告行程且软限位未开启",
      softLimits === false
        ? "固件未报告完整 X/Y 行程且软限位明确关闭（$20=0 / soft_limits:false），越界不会被固件或 MOSS 自动拦截"
        : "固件未报告完整 X/Y 行程且软限位状态未知，越界可能不会被拦截",
      {
        evidence: { x_travel_mm: limits?.x_travel_mm ?? null, y_travel_mm: limits?.y_travel_mm ?? null, soft_limits_enabled: softLimits },
        suspectedCode: "crates/laser-device/src/protocol.rs / 固件配置 $130/$131/$20 或 config.yaml",
      },
    );
  } else if (softLimits === false) {
    addFinding("soft_limits_disabled", "warn", "固件软限位未开启", "固件已报告行程，但软限位关闭（$20=0 或 soft_limits:false）", {
      evidence: { soft_limits_enabled: false },
    });
  }

  // 2. FluidNC Laser.off_on_alarm 提醒
  if (typeof probe?.summary === "string" && probe.summary.includes("off_on_alarm")) {
    addFinding(
      "laser_off_on_alarm_disabled",
      "safety",
      "FluidNC 未启用 Laser.off_on_alarm",
      "握手摘要提示 config.yaml 的 Laser 段未开启 off_on_alarm: true，报警时固件可能不会主动关激光",
      { evidence: { summary: probe.summary } },
    );
  }

  // 3. 回零状态
  if (homingEnabled === false) {
    addFinding(
      "homing_disabled",
      "info",
      "固件未开启机械回零",
      "固件报告 homing_enabled=false（$22=0 或未配置 homing），工作台应禁用 $H 机械回零",
      { evidence: { homing_enabled: false } },
    );
  } else if (homingEnabled === null) {
    addFinding("homing_unknown", "info", "回零状态未知", "固件未返回 $22 或 FluidNC homing 段，MOSS 按未知处理", {
      evidence: { homing_enabled: null },
    });
  }

  // 4. $I 身份检查
  if (!identity || (!identity.firmware && !identity.version && !identity.machine_name)) {
    addFinding(
      "identity_missing",
      "info",
      "未读到 $I 固件身份",
      "握手阶段未收到 [VER:...] 或 [MSG:Machine: ...] 身份信息，同地址换机自动重置无法生效",
      { evidence: { identity } },
    );
  }

  // 5. $10 缓冲汇报检查
  if (typeof probe?.summary === "string" && probe.summary.includes("$10")) {
    addFinding(
      "buffer_report_disabled",
      "warn",
      "$10 未开启缓冲汇报（Bf:）",
      "建议在固件中设置 $10=3，使流控能按真实空闲缓冲自适应窗口",
      { evidence: { summary: probe.summary } },
    );
  }

  // 6. 与 MOSS 记住的行程和运动参数对比
  const comparisons = [];
  const rememberedLimits = remembered?.machine_limits || null;
  for (const [key, label] of COMPARE_LIMIT_FIELDS) {
    const remVal = rememberedLimits && Number.isFinite(Number(rememberedLimits[key])) ? Number(rememberedLimits[key]) : null;
    const fwVal = limits && Number.isFinite(Number(limits[key])) ? Number(limits[key]) : null;
    let status = "match";
    if (remVal === null && fwVal === null) status = "both_missing";
    else if (remVal === null) status = "new_from_firmware";
    else if (fwVal === null) status = "missing_in_firmware";
    else if (Math.abs(remVal - fwVal) > 0.01) status = "mismatch";
    comparisons.push({ key, label, remembered: remVal, firmware: fwVal, status });

    if (status === "mismatch") {
      addFinding(
        `limit_mismatch_${key}`,
        "warn",
        `记住的${label}与固件不一致`,
        `MOSS 记住的 ${label} 为 ${remVal}，但本次实机读回 ${fwVal}`,
        { evidence: { key, remembered: remVal, firmware: fwVal }, suspectedCode: "crates/laser-api/src/connection.rs" },
      );
    } else if (status === "missing_in_firmware") {
      addFinding(
        `limit_missing_${key}`,
        "warn",
        `固件本次未再报告${label}`,
        `MOSS 此前记有 ${label}=${remVal}，但本次握手未读到该字段`,
        { evidence: { key, remembered: remVal, firmware: null } },
      );
    }
  }

  // 7. 实时状态报告检查
  if (statusReport) {
    const blockingPins = [...statusReport.pins].filter((c) => c === "E" || c === "F" || c === "D");
    if (blockingPins.length > 0) {
      addFinding(
        "blocking_pins_active",
        "safety",
        `检测到阻断引脚触发（Pn:${statusReport.pins}）`,
        `当前状态报告包含阻断引脚 ${blockingPins.join("/")}，设备处于急停、故障或安全门打开状态`,
        { evidence: { raw: statusReport.raw, pins: statusReport.pins } },
      );
    }
    if (statusReport.state !== "Idle") {
      addFinding(
        "machine_not_idle",
        "warn",
        `设备当前非空闲态（${statusReport.state}）`,
        `实时状态报告为 ${statusReport.state}（非 Idle），加工前请先确认设备状态`,
        { evidence: { state: statusReport.state, raw: statusReport.raw } },
      );
    }
    if (Array.isArray(statusReport.ov) && statusReport.ov.length >= 3 && statusReport.ov.some((v) => v !== 100)) {
      addFinding(
        "override_not_100",
        "warn",
        `控制器当前倍率非 100%（Ov:${statusReport.ov.join(",")}）`,
        "进给/快移/功率倍率偏离 100%，开刻前建议核对或复位",
        { evidence: { ov: statusReport.ov } },
      );
    }
  }

  findings.sort((a, b) => (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9));
  return {
    findings,
    comparisons,
    softLimitsEnabled: softLimits,
    homingEnabled,
    identity,
    machineLimits: limits,
    features,
  };
}

export function parseReadOnlyCliArgs(argv = process.argv.slice(2)) {
  const opts = {
    url: "",
    runtimeDir: "",
    outDir: "",
    mode: "",
    host: "",
    telnetPort: null,
    port: "",
    baudrate: null,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") opts.help = true;
    else if (arg === "--url") opts.url = argv[++i] || "";
    else if (arg === "--runtime-dir") opts.runtimeDir = resolve(argv[++i] || "");
    else if (arg === "--out-dir") opts.outDir = resolve(argv[++i] || "");
    else if (arg === "--mode") opts.mode = argv[++i] || "";
    else if (arg === "--host") opts.host = argv[++i] || "";
    else if (arg === "--telnet-port") opts.telnetPort = Number(argv[++i]);
    else if (arg === "--port") opts.port = argv[++i] || "";
    else if (arg === "--baudrate") opts.baudrate = Number(argv[++i]);
    else throw new Error(`未知参数: ${arg}`);
  }
  return opts;
}

function buildConnectionPayload(opts, remembered, runtimeConfig) {
  if (opts.mode || opts.host || opts.port) {
    const mode = opts.mode || (opts.port ? "serial" : "network");
    if (mode === "serial") {
      return {
        source: "cli",
        fields: {
          connection_mode: "serial",
          port: opts.port,
          baudrate: opts.baudrate || 115200,
        },
      };
    }
    return {
      source: "cli",
      fields: {
        connection_mode: "network",
        network_host: opts.host,
        host: opts.host,
        network_telnet_port: opts.telnetPort || 23,
        telnet_port: opts.telnetPort || 23,
      },
    };
  }
  if (remembered?.fields && typeof remembered.fields === "object") {
    return { source: "remembered", fields: { ...remembered.fields } };
  }
  const conn = runtimeConfig?.connection;
  if (conn?.default_mode === "serial" && conn?.serial?.default_port) {
    return {
      source: "runtime-config",
      fields: {
        connection_mode: "serial",
        port: conn.serial.default_port,
        baudrate: conn.serial.baudrate || 115200,
      },
    };
  }
  if (conn?.network?.default_host) {
    return {
      source: "runtime-config",
      fields: {
        connection_mode: "network",
        network_host: conn.network.default_host,
        host: conn.network.default_host,
        network_telnet_port: conn.network.telnet_port || 23,
        telnet_port: conn.network.telnet_port || 23,
      },
    };
  }
  return null;
}

function buildReadOnlyReportMarkdown({ runId, startedAtMs, finishedAtMs, precheckBlocked, blockReason, analysis, probe, statusReport, redactOpts }) {
  const lines = [
    `# L1 真机只读核对报告（${runId}）`,
    "",
    `- **模式**：只读核对（白名单强制拦截所有发送、移动、急停、解锁、回零与倍率接口）`,
    `- **开始时间**：${new Date(startedAtMs).toISOString()}`,
    `- **总耗时**：${((finishedAtMs - startedAtMs) / 1000).toFixed(2)}s`,
    "",
  ];

  if (precheckBlocked) {
    lines.push("## 预检未通过（未触碰设备）", "", `- ${redactLaserText(blockReason, redactOpts)}`, "");
    return redactLaserText(lines.join("\n"), redactOpts);
  }

  const safetyCount = analysis.findings.filter((f) => f.severity === "safety").length;
  lines.push(
    `- **安全类发现**：${safetyCount}`,
    `- **总发现数**：${analysis.findings.length}`,
    `- **软限位状态**：\`${String(analysis.softLimitsEnabled)}\``,
    `- **回零状态**：\`${String(analysis.homingEnabled)}\``,
    `- **固件身份（\`$I\`）**：${analysis.identity ? `\`${JSON.stringify(analysis.identity)}\`` : "未返回"}`,
    `- **实时状态**：${statusReport ? `\`${statusReport.state}\`（\`${statusReport.raw}\`）` : "未获取"}`,
    "",
    "## 发现的问题（按严重程度排序）",
    "",
  );

  if (analysis.findings.length === 0) {
    lines.push("未发现安全或配置偏差问题。", "");
  } else {
    for (let i = 0; i < analysis.findings.length; i += 1) {
      const f = analysis.findings[i];
      const tag = f.severity === "safety" ? "[安全]" : f.severity === "error" ? "[失败]" : f.severity === "warn" ? "[告警]" : "[提示]";
      lines.push(`### ${i + 1}. ${tag} ${f.title}（\`${f.id}\`）`);
      lines.push(`- **说明**：${redactLaserText(f.message, redactOpts)}`);
      if (f.suspectedCode) lines.push(`- **关联位置**：\`${f.suspectedCode}\``);
      if (f.evidence != null) lines.push(`- **证据**：\`${JSON.stringify(redactLaserValue(f.evidence, redactOpts))}\``);
      lines.push("");
    }
  }

  lines.push(
    "## 记住的参数 vs 固件实测参数",
    "",
    "| 参数 | MOSS 记住值 | 固件实测值 | 状态 |",
    "| --- | --- | --- | --- |",
  );
  for (const row of analysis.comparisons) {
    lines.push(
      `| ${mdCell(row.label)} | ${row.remembered ?? "—"} | ${row.firmware ?? "—"} | ${row.status} |`,
    );
  }
  lines.push("");

  if (Array.isArray(probe?.firmware_settings) && probe.firmware_settings.length > 0) {
    lines.push("## 读回的固件设置（`$$`）", "", "```text", ...probe.firmware_settings, "```", "");
  }
  return redactLaserText(lines.join("\n"), redactOpts);
}

export async function runReadOnlyCheck(opts = {}) {
  const runtimeDir = opts.runtimeDir ? resolve(opts.runtimeDir) : defaultRuntimeDir();
  const { data: settingsSnapshot } = readRuntimeSettings(runtimeDir);
  const remembered = settingsSnapshot?.laser_verified_connection || null;

  let baseUrl = opts.url;
  if (!baseUrl) {
    const port = Number.isInteger(settingsSnapshot?.web_port) ? settingsSnapshot.web_port : 9666;
    baseUrl = `http://127.0.0.1:${port}`;
  }

  const runId = formatRunId("readonly");
  const outDir = opts.outDir ? resolve(opts.outDir) : join(defaultBenchRoot(opts.runtimeDir || null), runId);
  mkdirSync(outDir, { recursive: true });

  const deviceAddresses = [];
  if (opts.host) deviceAddresses.push(opts.host);
  if (remembered?.fields?.network_host) deviceAddresses.push(remembered.fields.network_host);
  if (remembered?.fields?.port) deviceAddresses.push(remembered.fields.port);
  const redactOpts = defaultRedactOpts({
    runtimeDir,
    extraPaths: [outDir],
    deviceAddresses,
    redactDeviceAddresses: true,
  });

  const startedAtMs = Date.now();
  const steps = [];
  const client = createReadOnlyClient(baseUrl, {
    fetchImpl: opts.fetchImpl || fetch,
    timeoutMs: opts.timeoutMs || 10_000,
    onStep: (step) => steps.push(redactLaserValue(step, redactOpts)),
  });

  let precheckBlocked = false;
  let blockReason = "";
  let probeResult = null;
  let statusReport = null;
  let analysis = {
    findings: [],
    comparisons: [],
    softLimitsEnabled: null,
    homingEnabled: null,
    identity: null,
    machineLimits: null,
    features: null,
  };

  try {
    // 1. 检查本机 MOSS 存活
    const health = await client.request("GET", "/api/health");
    if (!health.ok) {
      precheckBlocked = true;
      blockReason = `本机 MOSS 未就绪（GET /api/health 返回 HTTP ${health.status}）`;
    }

    // 2. 预检活动任务与待确认停机记录：有任一占用即立即中止，不碰设备
    if (!precheckBlocked) {
      const stopTargets = await client.request("GET", "/api/jobs/stop-targets");
      const activeOrUncertain = Array.isArray(stopTargets.data?.result?.jobs) ? stopTargets.data.result.jobs : [];
      if (activeOrUncertain.length > 0) {
        precheckBlocked = true;
        const states = activeOrUncertain.map((j) => `${j.job_id || "未知任务"}(${j.state})`).join("、");
        blockReason = `检测到正在运行的任务或待确认停机记录（${states}），L1 只读核对已中止且未触碰设备`;
      }
    }

    // 3. 确定只读探测目标（不猜地址，优先 CLI → MOSS 记住的连接 → runtime-config）
    let target = null;
    if (!precheckBlocked) {
      const runtimeCfg = await client.request("GET", "/api/ui/runtime-config");
      target = buildConnectionPayload(opts, remembered, runtimeCfg.data?.result);
      if (!target) {
        precheckBlocked = true;
        blockReason = "未检测到已记住的激光雕刻机连接；请先在 MOSS App 中连接并完成一次握手探测，再运行 L1 只读核对";
      }
    }

    // 4. 发起只读握手探测（$$、$CD、$I）
    if (!precheckBlocked && target) {
      const checkRes = await client.request("POST", "/api/connection/check", {
        ...target.fields,
        auto_fallback: false,
        include_detail: true,
      });
      probeResult = checkRes.data?.result || null;
      const summaryText = String(probeResult?.summary || "");
      if (/正在执行|禁止并发|device_busy/i.test(summaryText)) {
        precheckBlocked = true;
        blockReason = `设备连接正被占用（${summaryText}），L1 只读核对已停止`;
      } else if (!checkRes.ok || !probeResult?.connected) {
        precheckBlocked = true;
        blockReason = `握手探测未连通设备：${summaryText || `HTTP ${checkRes.status}`}`;
      }
    }

    // 5. 发起只读状态查询（safe-action status，仅发送实时 ? 字节）
    if (!precheckBlocked && target) {
      const statusRes = await client.request("POST", "/api/safe-action", {
        action: "status",
        ...target.fields,
      });
      const detail = String(statusRes.data?.detail || "");
      if (detail === "active_job_streaming" || detail === "job_result_unknown" || /正在执行|禁止并发/.test(String(statusRes.data?.result || ""))) {
        precheckBlocked = true;
        blockReason = `状态查询发现设备正忙或停机未确认（${statusRes.data?.result || detail}），已停止后续检查`;
      } else if (statusRes.ok && statusRes.data?.success) {
        statusReport = parseStatusReportText(detail);
      }
    }

    if (!precheckBlocked) {
      analysis = analyzeReadOnlySnapshot({ probe: probeResult, statusReport, remembered });
    }
  } catch (err) {
    if (err instanceof ReadOnlyWhitelistError) throw err;
    precheckBlocked = true;
    blockReason = `无法连接本机 MOSS 回环接口（${err?.message || String(err)}）；请先启动 MOSS 桌面端或 moss-host`;
  }

  const finishedAtMs = Date.now();
  const traceDir = resolveProcessTraceDir(runtimeDir);
  const { jsonlLines, txtLines } = sliceProcessTrace(traceDir, startedAtMs, finishedAtMs, redactOpts);
  writeTimelineFiles(outDir, jsonlLines, txtLines);

  const safetyViolationCount = analysis.findings.filter((f) => f.severity === "safety").length;
  const warnCount = analysis.findings.filter((f) => f.severity === "warn" || f.severity === "error").length;
  const noticeCount = analysis.findings.filter((f) => f.severity === "info").length;

  const summary = redactLaserValue(
    {
      level: "L1-readonly",
      runId,
      startedAt: new Date(startedAtMs).toISOString(),
      finishedAt: new Date(finishedAtMs).toISOString(),
      totalDurationMs: finishedAtMs - startedAtMs,
      precheckBlocked,
      blockReason: blockReason || null,
      connected: Boolean(probeResult?.connected),
      softLimitsEnabled: analysis.softLimitsEnabled,
      homingEnabled: analysis.homingEnabled,
      identity: analysis.identity,
      safetyViolationCount,
      warnCount,
      noticeCount,
      findingCount: analysis.findings.length,
    },
    redactOpts,
  );

  const firmwareSnapshot = redactLaserValue(
    {
      remembered: remembered ? { mode: remembered.mode, verified_at: remembered.verified_at, machine_limits: remembered.machine_limits } : null,
      probe: probeResult,
      statusReport,
      comparisons: analysis.comparisons,
    },
    redactOpts,
  );

  const reportMd = buildReadOnlyReportMarkdown({
    runId,
    startedAtMs,
    finishedAtMs,
    precheckBlocked,
    blockReason,
    analysis,
    probe: probeResult,
    statusReport,
    redactOpts,
  });

  writeFileSync(join(outDir, "steps.jsonl"), steps.map((s) => JSON.stringify(s)).join("\n") + (steps.length ? "\n" : ""), "utf8");
  writeFileSync(join(outDir, "firmware.json"), JSON.stringify(firmwareSnapshot, null, 2) + "\n", "utf8");
  writeFileSync(
    join(outDir, "invariants.jsonl"),
    analysis.findings.map((f) => JSON.stringify(redactLaserValue(f, redactOpts))).join("\n") + (analysis.findings.length ? "\n" : ""),
    "utf8",
  );
  writeFileSync(join(outDir, "report.md"), reportMd, "utf8");
  writeFileSync(join(outDir, "summary.json"), JSON.stringify(summary, null, 2) + "\n", "utf8");

  const exitCode = precheckBlocked ? 1 : safetyViolationCount > 0 ? 2 : 0;
  return { outDir, summary, analysis, exitCode };
}

async function main() {
  const opts = parseReadOnlyCliArgs();
  if (opts.help) {
    console.log(
      [
        "用法：node scripts/laser-bench/laser-readonly-check.mjs [选项]",
        "  --url http://127.0.0.1:<port>  指定本机 MOSS 回环地址（默认从运行目录 settings.json 读取）",
        "  --runtime-dir <dir>            指定 MOSS 运行目录",
        "  --out-dir <dir>                指定归档目录（默认 <runtime>/laser-bench/readonly-<run-id>/）",
        "  --mode network|serial          显式指定只读探测连接方式",
        "  --host <ip> --telnet-port <p>  显式指定网络控制器目标",
        "  --port <dev> --baudrate <n>    显式指定串口目标",
      ].join("\n"),
    );
    return;
  }
  const { outDir, summary, exitCode } = await runReadOnlyCheck(opts);
  console.log(outDir);
  if (summary.precheckBlocked) {
    console.error(`[L1] 预检未通过：${summary.blockReason}`);
  } else {
    console.log(
      `[L1] 只读核对完成 · 安全类发现 ${summary.safetyViolationCount} · 告警 ${summary.warnCount} · 提示 ${summary.noticeCount}`,
    );
  }
  process.exitCode = exitCode;
}

if (process.argv[1] && existsSync(process.argv[1]) && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((err) => {
    console.error(`[L1] 运行失败：${err?.message || String(err)}`);
    process.exitCode = 1;
  });
}
