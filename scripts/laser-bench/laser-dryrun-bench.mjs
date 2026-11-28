#!/usr/bin/env node
// LB3: 激光雕刻机 L2 真机空跑测试脚本。
// - 经用户本机正在运行的 MOSS 回环接口执行，全部请求带 `X-Moss-Bench: dryrun`（仅作 ProcessTrace 来源标签，不授权限）；
// - 预检：空跑开关未开、有运行中/待确认停机任务、或未连接机器时，打印明确指引并以非 0 退出；
// - 开跑时打印提示“机器将以不出光、限速方式运动，请确认机器旁有人”（开关本身即为授权，不交互确认）；
// - 覆盖走边框（含暂停/继续/停止）、20×10mm 小文字任务、取消/断点续雕、点动、回原点、机械回零（仅 $22=1 时）、实时倍率；
// - 每步记录后端响应与状态报告（MPos/Ov/Pn/Bf/FS），核对功率始终 S0、速度不超限、坐标不越界、结束后回到 Idle；
// - 与 L0 同风格的脱敏归档写入 `<runtime-or-tmp>/laser-bench/dryrun-<run-id>/`。

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
  sleep,
  writeTimelineFiles,
} from "./bench-common.mjs";
import { SEVERITY_RANK } from "./invariants.mjs";
import { parseStatusReportText } from "./laser-readonly-check.mjs";

const DRYRUN_BANNER = "机器将以不出光、限速方式运动，请确认机器旁有人";

export function parseDryRunCliArgs(argv = process.argv.slice(2)) {
  const opts = {
    url: null,
    port: null,
    runtimeDir: null,
    outDir: null,
    connectionMode: null,
    host: null,
    telnetPort: null,
    serialPort: null,
    baudrate: null,
    scenarios: [],
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--help" || a === "-h") opts.help = true;
    else if (a === "--url" && argv[i + 1]) opts.url = argv[++i];
    else if (a === "--port" && argv[i + 1]) opts.port = Number(argv[++i]);
    else if (a === "--runtime-dir" && argv[i + 1]) opts.runtimeDir = argv[++i];
    else if (a === "--out-dir" && argv[i + 1]) opts.outDir = argv[++i];
    else if (a === "--mode" && argv[i + 1]) opts.connectionMode = argv[++i];
    else if (a === "--host" && argv[i + 1]) opts.host = argv[++i];
    else if (a === "--telnet-port" && argv[i + 1]) opts.telnetPort = Number(argv[++i]);
    else if (a === "--serial-port" && argv[i + 1]) opts.serialPort = argv[++i];
    else if (a === "--baudrate" && argv[i + 1]) opts.baudrate = Number(argv[++i]);
    else if (a === "--scenario" && argv[i + 1]) {
      for (const s of argv[++i].split(",").map((x) => x.trim()).filter(Boolean)) {
        opts.scenarios.push(s);
      }
    }
  }
  return opts;
}

/** 构造仅允许本机回环的 L2 空跑 HTTP 客户端（所有请求自动附带 X-Moss-Bench: dryrun）。 */
export function createDryRunClient(baseUrl, { fetchImpl = fetch, timeoutMs = 20_000, onStep = null } = {}) {
  const cleaned = String(baseUrl || "").trim().replace(/\/+$/, "");
  if (!isLoopbackUrl(cleaned)) {
    throw new Error(`L2 空跑测试只允许访问本机回环 http://127.0.0.1:<port>，拒绝：${cleaned}`);
  }
  return {
    baseUrl: cleaned,
    async request(method, route, body = undefined, { scenario = "precheck" } = {}) {
      const started = Date.now();
      const response = await fetchImpl(`${cleaned}${route}`, {
        method: String(method).toUpperCase(),
        headers: {
          Accept: "application/json",
          "X-Moss-Bench": "dryrun",
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
        scenario,
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

/**
 * 核对 L2 空跑全过程采集的状态报告（纯函数，便于单测喂入违规样例）：
 * 1. 状态报告中 FS:<feed>,<spindle> 的激光功率始终为 0（S === 0）；
 * 2. 状态报告中 FS:<feed>,<spindle> 的进给速度不超过 maxFeedMmMin（含 5 mm/min 浮点容差）；
 * 3. 若固件上报了行程（x_travel_mm / y_travel_mm），采样坐标跨度及范围不超过固件行程；
 * 4. 结束后最后一次状态报告为 Idle。
 */
export function checkDryRunStatusReports({
  reports = [],
  maxFeedMmMin = 2000,
  machineLimits = null,
  requireFinalIdle = true,
} = {}) {
  const checks = [];
  const parsed = reports
    .map((entry) => {
      if (typeof entry === "string") return { scenario: "", raw: entry, parsed: parseStatusReportText(entry) };
      const raw = entry?.raw || entry?.report || "";
      return {
        scenario: entry?.scenario || "",
        tsMs: entry?.tsMs ?? null,
        raw,
        parsed: entry?.parsed || parseStatusReportText(raw),
      };
    })
    .filter((item) => item.parsed);

  // 1. 激光功率始终 S0
  let nonZeroPower = null;
  let fsChecked = 0;
  for (const item of parsed) {
    const fs = item.parsed.fs;
    if (Array.isArray(fs) && fs.length >= 2 && Number.isFinite(fs[1])) {
      fsChecked += 1;
      if (fs[1] > 1e-6) {
        nonZeroPower = { scenario: item.scenario, tsMs: item.tsMs, feed: fs[0], spindleS: fs[1], raw: item.raw };
        break;
      }
    }
  }
  checks.push({
    id: "dryrun_laser_power_zero",
    severity: "safety",
    status: nonZeroPower ? "fail" : parsed.length === 0 ? "skip" : "pass",
    title: "空跑期间激光功率始终为 0（FS: S=0）",
    message: nonZeroPower
      ? `检测到非零激光功率 S=${nonZeroPower.spindleS}（报告 ${nonZeroPower.raw}）`
      : parsed.length === 0
        ? "未采集到状态报告"
        : `共检查 ${parsed.length} 条状态报告（含 ${fsChecked} 条 FS:），激光功率始终为 0`,
    evidence: nonZeroPower,
    suspectedCode: nonZeroPower ? "crates/laser-device/src/dry_run.rs (DryRunTransport)" : null,
  });

  // 2. 速度不超过上限
  const feedLimit = Number(maxFeedMmMin) || 2000;
  const FEED_TOL = 5;
  let overFeed = null;
  let maxObservedFeed = 0;
  for (const item of parsed) {
    const fs = item.parsed.fs;
    if (Array.isArray(fs) && fs.length >= 1 && Number.isFinite(fs[0])) {
      if (fs[0] > maxObservedFeed) maxObservedFeed = fs[0];
      if (fs[0] > feedLimit + FEED_TOL) {
        overFeed = {
          scenario: item.scenario,
          tsMs: item.tsMs,
          observedFeed: fs[0],
          maxFeedMmMin: feedLimit,
          raw: item.raw,
        };
        break;
      }
    }
  }
  checks.push({
    id: "dryrun_feed_within_limit",
    severity: "safety",
    status: overFeed ? "fail" : parsed.length === 0 ? "skip" : "pass",
    title: "空跑期间运动速度不超过设定上限",
    message: overFeed
      ? `实测速度 ${overFeed.observedFeed} mm/min 超出空跑上限 ${feedLimit} mm/min（报告 ${overFeed.raw}）`
      : parsed.length === 0
        ? "未采集到状态报告"
        : `共检查 ${parsed.length} 条状态报告，观测最高速度 ${maxObservedFeed} mm/min <= 上限 ${feedLimit} mm/min`,
    evidence: overFeed || { maxObservedFeed, maxFeedMmMin: feedLimit },
    suspectedCode: overFeed ? "crates/laser-device/src/dry_run.rs (rewrite_gcode_line_for_dry_run)" : null,
  });

  // 3. 坐标不超出固件行程
  const travelX = Number.isFinite(machineLimits?.x_travel_mm) ? machineLimits.x_travel_mm : null;
  const travelY = Number.isFinite(machineLimits?.y_travel_mm) ? machineLimits.y_travel_mm : null;
  const mposItems = parsed.filter((item) => Array.isArray(item.parsed.mpos) && item.parsed.mpos.length >= 2);
  if ((travelX == null && travelY == null) || mposItems.length === 0) {
    checks.push({
      id: "dryrun_coords_within_travel",
      severity: "safety",
      status: "skip",
      title: "空跑坐标未超出固件行程",
      message: "固件未报告行程或状态报告不含 MPos，跳过坐标越界复核",
      evidence: null,
      suspectedCode: null,
    });
  } else {
    const TOL = 0.5;
    let minX = Infinity;
    let maxX = -Infinity;
    let minY = Infinity;
    let maxY = -Infinity;
    let outItem = null;
    for (const item of mposItems) {
      const [x, y] = item.parsed.mpos;
      if (!Number.isFinite(x) || !Number.isFinite(y)) continue;
      if (x < minX) minX = x;
      if (x > maxX) maxX = x;
      if (y < minY) minY = y;
      if (y > maxY) maxY = y;
      if (travelX != null && Math.abs(x) > travelX + TOL) {
        outItem = { axis: "X", value: x, travel: travelX, raw: item.raw, scenario: item.scenario };
        break;
      }
      if (travelY != null && Math.abs(y) > travelY + TOL) {
        outItem = { axis: "Y", value: y, travel: travelY, raw: item.raw, scenario: item.scenario };
        break;
      }
    }
    const spanX = maxX - minX;
    const spanY = maxY - minY;
    if (!outItem && travelX != null && spanX > travelX + TOL) {
      outItem = { axis: "X_SPAN", value: spanX, travel: travelX, minX, maxX };
    }
    if (!outItem && travelY != null && spanY > travelY + TOL) {
      outItem = { axis: "Y_SPAN", value: spanY, travel: travelY, minY, maxY };
    }
    checks.push({
      id: "dryrun_coords_within_travel",
      severity: "safety",
      status: outItem ? "fail" : "pass",
      title: "空跑坐标未超出固件行程",
      message: outItem
        ? `采样坐标 ${outItem.axis}=${Number(outItem.value).toFixed(2)} mm 超出固件行程 ${outItem.travel} mm`
        : `全部 ${mposItems.length} 次 MPos 采样在固件行程内（X:[${minX.toFixed(1)}, ${maxX.toFixed(1)}] Y:[${minY.toFixed(1)}, ${maxY.toFixed(1)}]）`,
      evidence: outItem || { minX, maxX, minY, maxY, travelX, travelY },
      suspectedCode: outItem ? "crates/laser-api/src/generation/mod.rs (check_job_fits_machine)" : null,
    });
  }

  // 4. 结束后回到 Idle
  if (requireFinalIdle) {
    const last = parsed[parsed.length - 1] || null;
    const isIdle = last?.parsed?.state === "Idle";
    checks.push({
      id: "dryrun_idle_after_finish",
      severity: "error",
      status: !last ? "fail" : isIdle ? "pass" : "fail",
      title: "测试结束后设备回到空闲（Idle）",
      message: !last
        ? "未拿到结束后的状态报告"
        : isIdle
          ? `最终状态为 Idle（${last.raw}）`
          : `测试结束后设备未回到 Idle，当前为 ${last.parsed.state}（${last.raw}）`,
      evidence: last ? { state: last.parsed.state, raw: last.raw } : null,
      suspectedCode: isIdle ? null : "crates/laser-device/src/connection.rs",
    });
  }

  return checks.sort((a, b) => (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9));
}

function buildConnectionPayload(opts, savedSettings) {
  const remembered = savedSettings?.laser_verified_connection?.fields || {};
  const mode = opts.connectionMode || remembered.connection_mode || (opts.serialPort ? "serial" : opts.host ? "network" : null);
  const payload = {};
  if (mode) payload.connection_mode = mode;
  if (opts.host) {
    payload.network_host = opts.host;
    payload.host = opts.host;
  } else if (remembered.network_host || remembered.host) {
    payload.network_host = remembered.network_host || remembered.host;
    payload.host = payload.network_host;
  }
  if (opts.telnetPort) {
    payload.network_telnet_port = opts.telnetPort;
    payload.telnet_port = opts.telnetPort;
  } else if (remembered.network_telnet_port || remembered.telnet_port) {
    payload.network_telnet_port = remembered.network_telnet_port || remembered.telnet_port;
    payload.telnet_port = payload.network_telnet_port;
  }
  if (opts.serialPort) {
    payload.port = opts.serialPort;
    payload.serial_port = opts.serialPort;
  } else if (remembered.port || remembered.serial_port) {
    payload.port = remembered.port || remembered.serial_port;
    payload.serial_port = payload.port;
  }
  if (opts.baudrate) {
    payload.baudrate = opts.baudrate;
  } else if (remembered.baudrate) {
    payload.baudrate = remembered.baudrate;
  }
  return payload;
}

function extractStatusFromText(raw) {
  const m = /<[^>]+>/.exec(String(raw || ""));
  return m ? m[0] : null;
}

export function buildDryRunReportMarkdown({
  runId,
  startedAtMs,
  finishedAtMs,
  summary,
  scenarioResults,
  checks,
  redactOpts,
}) {
  const failures = [];
  for (const c of checks) {
    if (c.status === "fail") failures.push({ ...c, scenario: c.scenario || "global" });
  }
  for (const r of scenarioResults) {
    for (const f of r.failures || []) {
      failures.push({ ...f, scenario: r.id });
    }
  }
  failures.sort((a, b) => (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9));

  const lines = [
    `# L2 真机空跑测试报告（${runId}）`,
    "",
    `- **开始时间**：${new Date(startedAtMs).toISOString()}`,
    `- **总耗时**：${((finishedAtMs - startedAtMs) / 1000).toFixed(2)}s`,
    `- **空跑限速上限**：${summary.maxFeedMmMin ?? "未知"} mm/min（下发折算上限 ${summary.effectiveMaxFeedMmMin ?? "未知"} mm/min）`,
    `- **预检是否阻断**：${summary.precheckBlocked ? `是（${summary.precheckReason}）` : "否"}`,
    `- **场景通过率**：${summary.passedScenarios} / ${summary.totalScenarios}`,
    `- **不变量违反（安全类）**：${summary.safetyViolationCount}`,
    `- **一般失败**：${summary.errorViolationCount}`,
    `- **采样状态报告数**：${summary.statusReportCount}`,
    "",
    "## 问题列表（按严重程度排序，安全类最高）",
    "",
  ];

  if (failures.length === 0) {
    lines.push(summary.precheckBlocked ? `预检未通过：${summary.precheckReason}` : "本次空跑未发现安全类违反或场景失败。", "");
  } else {
    for (let i = 0; i < failures.length; i += 1) {
      const f = failures[i];
      const badge = f.severity === "safety" ? "[安全]" : f.severity === "warn" ? "[告警]" : "[失败]";
      lines.push(`### ${i + 1}. ${badge} ${f.title}（场景 \`${f.scenario}\` · \`${f.id}\`）`);
      lines.push(`- **现象**：${redactLaserText(f.message, redactOpts)}`);
      if (f.suspectedCode) lines.push(`- **怀疑位置**：\`${f.suspectedCode}\``);
      if (f.evidence != null) {
        const ev = JSON.stringify(redactLaserValue(f.evidence, redactOpts));
        lines.push(`- **证据**：\`${ev.length > 400 ? `${ev.slice(0, 400)}…` : ev}\``);
      }
      lines.push("");
    }
  }

  lines.push("## 场景结果总览", "", "| 场景 ID | 说明 | 结果 | 耗时(ms) | 备注 |", "| --- | --- | --- | ---: | --- |");
  for (const r of scenarioResults) {
    lines.push(
      `| \`${r.id}\` | ${mdCell(r.name)} | ${r.skipped ? "SKIP" : r.passed ? "PASS" : "FAIL"} | ${r.durationMs} | ${mdCell(r.note || "")} |`,
    );
  }
  lines.push("");
  return redactLaserText(lines.join("\n"), redactOpts);
}

/**
 * 执行 L2 真机空跑测试。
 */
export async function runDryRunBench(opts = {}) {
  const runtimeDir = opts.runtimeDir ? resolve(opts.runtimeDir) : defaultRuntimeDir();
  const savedSettings = readRuntimeSettings(runtimeDir);
  const webPort = Number(opts.port) || Number(savedSettings.web_port) || 9666;
  const baseUrl = opts.url || `http://127.0.0.1:${webPort}`;
  const runId = formatRunId("dryrun");
  const outDir = opts.outDir ? resolve(opts.outDir) : join(defaultBenchRoot(runtimeDir), runId);
  mkdirSync(outDir, { recursive: true });

  const connPayload = buildConnectionPayload(opts, savedSettings);
  const deviceAddrs = [connPayload.network_host, connPayload.host, connPayload.port, connPayload.serial_port].filter(Boolean);
  const redactOpts = defaultRedactOpts({
    runtimeDir,
    extraPaths: [outDir],
    deviceAddresses: deviceAddrs,
    redactDeviceAddresses: true,
  });

  const startedAtMs = Date.now();
  const steps = [];
  const statusReports = [];
  const scenarioResults = [];
  const log = typeof opts.log === "function" ? opts.log : console.log;

  const client = createDryRunClient(baseUrl, {
    fetchImpl: opts.fetchImpl || fetch,
    timeoutMs: opts.timeoutMs || 25_000,
    onStep: (step) => steps.push(redactLaserValue(step, redactOpts)),
  });

  const recordStatus = (scenario, raw) => {
    const extracted = extractStatusFromText(raw);
    if (!extracted) return null;
    const parsed = parseStatusReportText(extracted);
    if (!parsed) return null;
    const entry = { tsMs: Date.now(), scenario, raw: extracted, parsed };
    statusReports.push(entry);
    return entry;
  };

  const queryStatus = async (scenario) => {
    const res = await client.request(
      "POST",
      "/api/safe-action",
      { action: "status", ...connPayload },
      { scenario },
    );
    const raw = res.data?.detail || res.data?.result || "";
    return { res, status: recordStatus(scenario, raw) };
  };

  const pollWorkflow = async (scenario, wfId, targetStatuses, timeoutMs = 30_000, predicate = null) => {
    const deadline = Date.now() + timeoutMs;
    let last = null;
    while (Date.now() < deadline) {
      const r = await client.request(
        "POST",
        "/api/workflow",
        { action: "status", workflow_id: wfId },
        { scenario },
      );
      last = r;
      const rec = r.data?.result;
      if (rec?.send_result?.last_status_report) {
        recordStatus(scenario, rec.send_result.last_status_report);
      }
      if (rec && targetStatuses.includes(rec.status) && (!predicate || predicate(rec))) {
        return r;
      }
      await sleep(80);
    }
    return last;
  };

  const finalizeArchive = ({
    precheckBlocked = false,
    precheckReason = null,
    maxFeedMmMin = null,
    effectiveMaxFeedMmMin = null,
    machineLimits = null,
    checks = [],
    exitCode = 0,
  }) => {
    const finishedAtMs = Date.now();
    const traceDir = resolveProcessTraceDir(runtimeDir);
    const { jsonlLines, txtLines } = sliceProcessTrace(traceDir, startedAtMs, finishedAtMs, redactOpts);
    writeTimelineFiles(outDir, jsonlLines, txtLines);

    const passedScenarios = scenarioResults.filter((r) => !r.skipped && r.passed).length;
    const executedScenarios = scenarioResults.filter((r) => !r.skipped).length;
    const safetyViolationCount =
      checks.filter((c) => c.status === "fail" && c.severity === "safety").length +
      scenarioResults.reduce((n, r) => n + (r.failures || []).filter((f) => f.severity === "safety").length, 0);
    const errorViolationCount =
      checks.filter((c) => c.status === "fail" && c.severity !== "safety").length +
      scenarioResults.reduce((n, r) => n + (r.failures || []).filter((f) => f.severity !== "safety").length, 0);

    const summary = {
      level: "L2-dryrun",
      runId,
      startedAt: new Date(startedAtMs).toISOString(),
      finishedAt: new Date(finishedAtMs).toISOString(),
      totalDurationMs: finishedAtMs - startedAtMs,
      precheckBlocked,
      precheckReason,
      maxFeedMmMin,
      effectiveMaxFeedMmMin,
      totalScenarios: executedScenarios,
      passedScenarios,
      skippedScenarios: scenarioResults.filter((r) => r.skipped).length,
      safetyViolationCount,
      errorViolationCount,
      statusReportCount: statusReports.length,
      timelineEventsCount: jsonlLines.length,
      scenarios: scenarioResults.map((r) => ({
        id: r.id,
        name: r.name,
        passed: r.passed,
        skipped: Boolean(r.skipped),
        durationMs: r.durationMs,
        note: r.note || "",
      })),
    };

    const reportMd = buildDryRunReportMarkdown({
      runId,
      startedAtMs,
      finishedAtMs,
      summary,
      scenarioResults,
      checks,
      redactOpts,
    });
    const redactedSummary = redactLaserValue(summary, redactOpts);
    const redactedChecks = checks.map((c) => JSON.stringify(redactLaserValue(c, redactOpts)));
    const redactedReports = statusReports.map((r) => JSON.stringify(redactLaserValue(r, redactOpts)));

    writeFileSync(join(outDir, "steps.jsonl"), steps.map((s) => JSON.stringify(s)).join("\n") + (steps.length ? "\n" : ""), "utf8");
    writeFileSync(join(outDir, "status-reports.jsonl"), redactedReports.join("\n") + (redactedReports.length ? "\n" : ""), "utf8");
    writeFileSync(join(outDir, "invariants.jsonl"), redactedChecks.join("\n") + (redactedChecks.length ? "\n" : ""), "utf8");
    writeFileSync(join(outDir, "report.md"), reportMd, "utf8");
    writeFileSync(join(outDir, "summary.json"), JSON.stringify(redactedSummary, null, 2) + "\n", "utf8");

    const finalCode = precheckBlocked
      ? 1
      : safetyViolationCount > 0
        ? 2
        : errorViolationCount > 0 || passedScenarios < executedScenarios
          ? 1
          : exitCode;
    return { outDir, summary: redactedSummary, checks, scenarioResults, exitCode: finalCode };
  };

  // --- 预检 1：确认 MOSS 运行中且空跑开关已开启 ---
  let sysInfo;
  try {
    const res = await client.request("GET", "/api/system/info", undefined, { scenario: "precheck" });
    if (!res.ok || !res.data?.success) {
      const reason = `无法读取 /api/system/info（HTTP ${res.status}），请先启动 MOSS 桌面端`;
      log(`[L2] 预检失败：${reason}`);
      return finalizeArchive({ precheckBlocked: true, precheckReason: reason });
    }
    sysInfo = res.data.result || {};
  } catch (err) {
    const reason = `无法连接本机 MOSS 回环服务（${baseUrl}）：${err?.message || err}。请先启动 MOSS 桌面端`;
    log(`[L2] 预检失败：${reason}`);
    return finalizeArchive({ precheckBlocked: true, precheckReason: reason });
  }

  if (sysInfo.laser_dry_run_enabled !== true) {
    const reason =
      "空跑开关未开启：请在 MOSS 桌面端打开「偏好设置 → AI 激光自主测试」，开启「允许 AI 真机空跑测试（不出光、限速）」后再重跑本脚本";
    log(`[L2] 预检阻断：${reason}`);
    return finalizeArchive({
      precheckBlocked: true,
      precheckReason: reason,
      maxFeedMmMin: Number(sysInfo.dry_run_max_feed_mm_min) || 2000,
      effectiveMaxFeedMmMin: Number(sysInfo.laser_dry_run_effective_max_feed_mm_min) || 1000,
    });
  }

  const maxFeedMmMin = Number(sysInfo.dry_run_max_feed_mm_min) || 2000;
  const effectiveMaxFeedMmMin = Number(sysInfo.laser_dry_run_effective_max_feed_mm_min) || Math.floor(maxFeedMmMin / 2);

  // --- 预检 2：检查无活动或待确认停机任务 ---
  const stopTargets = await client.request("GET", "/api/jobs/stop-targets", undefined, { scenario: "precheck" });
  const activeJobs = Array.isArray(stopTargets.data?.result?.jobs) ? stopTargets.data.result.jobs : [];
  if (activeJobs.length > 0) {
    const reason = `当前存在 ${activeJobs.length} 个正在运行或待确认停机的任务，请先等待完成或处理停机后再运行 L2 空跑`;
    log(`[L2] 预检阻断：${reason}`);
    return finalizeArchive({ precheckBlocked: true, precheckReason: reason, maxFeedMmMin, effectiveMaxFeedMmMin });
  }

  // --- 预检 3：握手确认已连接激光机 ---
  const probeRes = await client.request("POST", "/api/connection/check", connPayload, { scenario: "precheck" });
  const probe = probeRes.data?.result || {};
  if (!probeRes.ok || !probeRes.data?.success || probe.connected !== true) {
    const reason = `未连接到激光雕刻机（${probe.summary || "握手失败"}）：请先在 MOSS App 里连接好机器并完成一次握手探测`;
    log(`[L2] 预检阻断：${reason}`);
    return finalizeArchive({ precheckBlocked: true, precheckReason: reason, maxFeedMmMin, effectiveMaxFeedMmMin });
  }

  const machineLimits = probe.machine_limits || null;
  const homingEnabled = probe.homing_enabled ?? machineLimits?.homing_enabled ?? null;

  // 开跑前提示（开关即授权，无交互确认）
  log(DRYRUN_BANNER);
  await queryStatus("precheck");

  const selected = Array.isArray(opts.scenarios) && opts.scenarios.length > 0 ? new Set(opts.scenarios) : null;
  const want = (id) => !selected || selected.has(id);

  const runOne = async (id, name, fn) => {
    if (!want(id)) return;
    const t0 = Date.now();
    const failures = [];
    const fail = (fid, severity, title, message, evidence = null) => {
      failures.push({ id: fid, severity, status: "fail", title, message, evidence });
    };
    let skipped = false;
    let note = "";
    try {
      const outcome = await fn({ fail });
      if (outcome?.skipped) {
        skipped = true;
        note = outcome.note || "";
      } else if (outcome?.note) {
        note = outcome.note;
      }
    } catch (err) {
      fail(`exception_${id}`, "error", `${name} 异常`, err?.message || String(err));
    }
    scenarioResults.push({
      id,
      name,
      passed: failures.length === 0,
      skipped,
      durationMs: Date.now() - t0,
      failures,
      note,
    });
  };

  // 1. 走边框（含暂停 / 继续与完成）
  await runOne("dryrun_framing", "走边框（含暂停与继续）", async ({ fail }) => {
    const framePromise = client.request(
      "POST",
      "/api/device/framing",
      { min_x: 0, max_x: 20, min_y: 0, max_y: 10, feed_rate: Math.min(maxFeedMmMin, 2000), power: 0, ...connPayload },
      { scenario: "dryrun_framing" },
    );
    // 尝试探测运行中的 framing 作业并执行一次 pause / resume（若设备过快已走完则仍验证走边框本身成功）
    let pausedAndResumed = false;
    const waitStart = Date.now();
    while (Date.now() - waitStart < 1500) {
      const st = await client.request("GET", "/api/jobs/stop-targets", undefined, { scenario: "dryrun_framing" });
      const fJob = (st.data?.result?.jobs || []).find((j) => j.state === "running" && String(j.job_id || "").startsWith("framing-"));
      if (fJob) {
        if (fJob.last_status_report) recordStatus("dryrun_framing", fJob.last_status_report);
        const pRes = await client.request(
          "POST",
          "/api/safe-action",
          { action: "pause", job_id: fJob.job_id, ...connPayload },
          { scenario: "dryrun_framing" },
        );
        if (pRes.ok && pRes.data?.success) {
          await sleep(40);
          const rRes = await client.request(
            "POST",
            "/api/safe-action",
            { action: "resume", job_id: fJob.job_id, confirmed: true, ...connPayload },
            { scenario: "dryrun_framing" },
          );
          if (!rRes.ok || !rRes.data?.success) {
            fail("framing_resume_failed", "error", "继续走边框失败", rRes.data?.result || `HTTP ${rRes.status}`, rRes.data);
          } else {
            pausedAndResumed = true;
          }
        }
        break;
      }
      await sleep(25);
    }
    const frameRes = await framePromise;
    if (!frameRes.ok || !frameRes.data?.success) {
      fail("framing_failed", "error", "走边框失败", frameRes.data?.result || `HTTP ${frameRes.status}`, frameRes.data);
    }
    await queryStatus("dryrun_framing");
    return { note: pausedAndResumed ? "已验证暂停/继续并完成" : "走边框完成" };
  });

  // 2. 小文字任务（默认约 20×10 mm，工作原点附近）
  await runOne("dryrun_text_small", "小文字任务空跑（20×10 mm，原点附近）", async ({ fail }) => {
    const gen = await client.request(
      "POST",
      "/api/text-task/generate",
      {
        text: "MO",
        material: "椴木",
        thickness_mm: 3,
        font_size_mm: 8,
        text_fill: "outline",
        text_origin: "lower_left",
      },
      { scenario: "dryrun_text_small" },
    );
    const wfId = gen.data?.result?.workflow_id;
    if (!gen.ok || !wfId) {
      fail("text_gen_failed", "error", "小文字预览生成失败", gen.data?.result || `HTTP ${gen.status}`, gen.data);
      return {};
    }
    const send = await client.request(
      "POST",
      "/api/workflow",
      { action: "confirm_send", workflow_id: wfId, confirmed: true, ...connPayload },
      { scenario: "dryrun_text_small" },
    );
    if (!send.ok || !send.data?.success) {
      fail("text_send_failed", "error", "小文字任务发送启动失败", send.data?.result || `HTTP ${send.status}`, send.data);
      return {};
    }
    const fin = await pollWorkflow("dryrun_text_small", wfId, ["completed", "failed", "cancelled"], 45_000);
    if (fin?.data?.result?.status !== "completed") {
      fail("text_not_completed", "error", "小文字任务未完成", `终态：${fin?.data?.result?.status}`, fin?.data?.result?.send_result);
    }
    await queryStatus("dryrun_text_small");
    return { note: `lines=${fin?.data?.result?.send_result?.lines_sent ?? "?"}/${fin?.data?.result?.send_result?.total_lines ?? "?"}` };
  });

  // 3. 取消与断点续雕
  await runOne("dryrun_cancel_and_resume", "加工取消与断点续雕", async ({ fail }) => {
    const gen = await client.request(
      "POST",
      "/api/text-task/generate",
      {
        text: "DRY",
        material: "椴木",
        thickness_mm: 3,
        font_size_mm: 8,
        text_fill: "outline",
        text_origin: "lower_left",
      },
      { scenario: "dryrun_cancel_and_resume" },
    );
    const wfId = gen.data?.result?.workflow_id;
    if (!gen.ok || !wfId) {
      fail("cancel_gen_failed", "error", "续雕测试预览生成失败", gen.data?.result || `HTTP ${gen.status}`);
      return {};
    }
    const send = await client.request(
      "POST",
      "/api/workflow",
      { action: "confirm_send", workflow_id: wfId, confirmed: true, ...connPayload },
      { scenario: "dryrun_cancel_and_resume" },
    );
    if (!send.ok || !send.data?.success) {
      fail("cancel_send_failed", "error", "续雕测试首轮发送失败", send.data?.result || `HTTP ${send.status}`);
      return {};
    }
    await pollWorkflow(
      "dryrun_cancel_and_resume",
      wfId,
      ["sending", "completed", "failed", "cancelled"],
      10_000,
      (rec) => rec.status !== "sending" || (rec.send_result?.lines_sent || 0) >= 5,
    );
    await client.request("POST", "/api/workflow", { action: "cancel", workflow_id: wfId }, { scenario: "dryrun_cancel_and_resume" });
    const stopped = await pollWorkflow(
      "dryrun_cancel_and_resume",
      wfId,
      ["cancelled", "failed", "completed"],
      15_000,
      (rec) => rec.status !== "cancelled" || rec.send_result?.can_resume !== undefined,
    );
    const sr = stopped?.data?.result?.send_result || {};
    let note = `取消终态=${stopped?.data?.result?.status}`;
    if (stopped?.data?.result?.status === "cancelled" && sr.can_resume === true && sr.resumable_from_line > 0) {
      const resumed = await client.request(
        "POST",
        "/api/workflow",
        { action: "resume", workflow_id: wfId, confirmed: true, ...connPayload },
        { scenario: "dryrun_cancel_and_resume" },
      );
      if (!resumed.ok || !resumed.data?.success) {
        fail("resume_failed", "error", "断点续雕启动失败", resumed.data?.result || `HTTP ${resumed.status}`, resumed.data);
      } else {
        const fin = await pollWorkflow("dryrun_cancel_and_resume", wfId, ["completed", "failed", "cancelled"], 45_000);
        if (fin?.data?.result?.status !== "completed") {
          fail("resume_not_completed", "error", "断点续雕未完成", `终态：${fin?.data?.result?.status}`, fin?.data?.result?.send_result);
        }
        note = `已从第 ${sr.resumable_from_line + 1} 行续雕完成`;
      }
    } else {
      note += "（固件未证实断点行号，跳过续雕）";
    }
    await queryStatus("dryrun_cancel_and_resume");
    return { note };
  });

  // 4. 点动与回工作原点
  await runOne("dryrun_jog_and_return_origin", "点动与回工作原点", async ({ fail }) => {
    const jog = await client.request(
      "POST",
      "/api/safe-action",
      { action: "relative_move", x: 2, y: 2, feed_rate: Math.min(maxFeedMmMin, 1200), confirmed: true, ...connPayload },
      { scenario: "dryrun_jog_and_return_origin" },
    );
    if (!jog.ok || !jog.data?.success) {
      fail("jog_failed", "error", "点动失败", jog.data?.result || `HTTP ${jog.status}`, jog.data);
    }
    await queryStatus("dryrun_jog_and_return_origin");

    const ret = await client.request(
      "POST",
      "/api/safe-action",
      { action: "absolute_move", x: 0, y: 0, feed_rate: Math.min(maxFeedMmMin, 1000), confirmed: true, ...connPayload },
      { scenario: "dryrun_jog_and_return_origin" },
    );
    if (!ret.ok || !ret.data?.success) {
      fail("return_origin_failed", "error", "回工作原点失败", ret.data?.result || `HTTP ${ret.status}`, ret.data);
    }
    await queryStatus("dryrun_jog_and_return_origin");
    return { note: "点动 (+2,+2) 并回 (0,0)" };
  });

  // 5. 机械回零（仅固件开启回零时执行）
  await runOne("dryrun_home", "机械回零（仅 $22=1 时执行）", async ({ fail }) => {
    if (homingEnabled !== true) {
      return {
        skipped: true,
        note: homingEnabled === false ? "固件 $22=0（未开启回零），按规则跳过" : "固件未报告回零开关状态，跳过",
      };
    }
    const res = await client.request(
      "POST",
      "/api/safe-action",
      { action: "home", confirmed: true, ...connPayload },
      { scenario: "dryrun_home" },
    );
    if (!res.ok || !res.data?.success) {
      fail("home_failed", "error", "机械回零失败", res.data?.result || `HTTP ${res.status}`, res.data);
    }
    await queryStatus("dryrun_home");
    return { note: "$H 回零完成" };
  });

  // 6. 实时倍率（空闲必拒 + 运行中命中并复位 100%）
  await runOne("dryrun_live_override", "实时倍率（空闲拒绝 + 运行中调参复位）", async ({ fail }) => {
    const idleRes = await client.request(
      "POST",
      "/api/safe-action",
      { action: "feed_override_plus_10", ...connPayload },
      { scenario: "dryrun_live_override" },
    );
    if (idleRes.status !== 400) {
      fail("override_idle_not_rejected", "safety", "空闲时实时倍率未被拒绝", `预期 400，实际 ${idleRes.status}`, idleRes.data);
    }
    const gen = await client.request(
      "POST",
      "/api/text-task/generate",
      { text: "OV", material: "椴木", thickness_mm: 3, font_size_mm: 8, text_fill: "outline", text_origin: "lower_left" },
      { scenario: "dryrun_live_override" },
    );
    const wfId = gen.data?.result?.workflow_id;
    if (!gen.ok || !wfId) {
      fail("override_gen_failed", "error", "倍率测试任务生成失败", gen.data?.result || `HTTP ${gen.status}`);
      return {};
    }
    const send = await client.request(
      "POST",
      "/api/workflow",
      { action: "confirm_send", workflow_id: wfId, confirmed: true, ...connPayload },
      { scenario: "dryrun_live_override" },
    );
    if (!send.ok || !send.data?.success) {
      fail("override_send_failed", "error", "倍率测试任务发送失败", send.data?.result || `HTTP ${send.status}`);
      return {};
    }
    // 立即下发进给 +10% 与复位 100%，以及功率 -10% 与复位 100%
    let hitCount = 0;
    for (const action of ["feed_override_plus_10", "feed_override_reset", "power_override_minus_10", "power_override_reset"]) {
      const r = await client.request("POST", "/api/safe-action", { action, ...connPayload }, { scenario: "dryrun_live_override" });
      if (r.ok && r.data?.success) hitCount += 1;
    }
    const fin = await pollWorkflow("dryrun_live_override", wfId, ["completed", "failed", "cancelled"], 45_000);
    if (fin?.data?.result?.status !== "completed") {
      fail("override_job_failed", "error", "倍率测试任务未完成", `终态：${fin?.data?.result?.status}`);
    }
    await queryStatus("dryrun_live_override");
    return { note: `运行中倍率命中 ${hitCount}/4` };
  });

  // 最终状态查询并运行 L2 统一检查
  await queryStatus("final");
  const checks = checkDryRunStatusReports({
    reports: statusReports,
    maxFeedMmMin,
    machineLimits,
    requireFinalIdle: true,
  });

  return finalizeArchive({
    precheckBlocked: false,
    maxFeedMmMin,
    effectiveMaxFeedMmMin,
    machineLimits,
    checks,
  });
}

async function main() {
  const opts = parseDryRunCliArgs();
  if (opts.help) {
    console.log(
      [
        "用法：node scripts/laser-bench/laser-dryrun-bench.mjs [选项]",
        "  --url <http://127.0.0.1:port>  指定本机 MOSS 回环地址（默认按 settings.json 的 web_port）",
        "  --port <port>                  指定本机回环端口",
        "  --runtime-dir <dir>            指定 MOSS 运行目录（用于读取 settings.json 与 ProcessTrace）",
        "  --out-dir <dir>                指定归档目录（默认 <runtime>/laser-bench/dryrun-<run-id>/）",
        "  --scenario <id>                仅运行指定场景（可重复传入或逗号分隔）",
        "  --mode network|serial          覆盖连接模式",
        "  --host <ip> --telnet-port <p>  覆盖网络连接参数",
        "  --serial-port <p> --baudrate   覆盖串口连接参数",
      ].join("\n"),
    );
    return;
  }
  const { outDir, summary, exitCode } = await runDryRunBench(opts);
  console.log(outDir);
  console.log(
    `[L2] 通过 ${summary.passedScenarios}/${summary.totalScenarios}（跳过 ${summary.skippedScenarios}） · 安全类违反 ${summary.safetyViolationCount} · 一般失败 ${summary.errorViolationCount}`,
  );
  process.exitCode = exitCode;
}

if (process.argv[1] && existsSync(process.argv[1]) && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((err) => {
    console.error(`[L2] 运行失败：${err?.message || String(err)}`);
    process.exitCode = 1;
  });
}
