#!/usr/bin/env node
// LA1: 激光雕刻机 L0 模拟机全自动测试脚本（与用户正在运行的 MOSS 完全隔离）。
// 用法：
//   node scripts/laser-bench/laser-sim-bench.mjs
//   node scripts/laser-bench/laser-sim-bench.mjs --scenario send_text_complete --scenario send_estop_realtime
//   node scripts/laser-bench/laser-sim-bench.mjs --profile grbl|fluidnc|<file> --keep-runtime --out-dir <dir>

import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync } from "node:zlib";

import {
  defaultBenchRoot,
  defaultRedactOpts,
  findLocalLanIpv4,
  formatRunId,
  mdCell,
  redactLaserText,
  redactLaserValue,
  registerSignalCleanup,
  resolveHostBinary,
  resolveProcessTraceDir,
  sha256Hex,
  sleep,
  sliceProcessTrace,
  startIsolatedHost,
  writeTimelineFiles,
} from "./bench-common.mjs";
import { runAllInvariants, SEVERITY_RANK } from "./invariants.mjs";
import { computeTravelBounds, loadProfile, parseGcodeLine, startGrblSimulator, stripGcodeComments } from "./sim-grbl.mjs";

const MODULE_DIR = dirname(fileURLToPath(import.meta.url));
const DEFAULT_SCENARIOS_FILE = join(MODULE_DIR, "scenarios.json");

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i += 1) {
    c ^= buf[i];
    for (let k = 0; k < 8; k += 1) {
      c = (c >>> 1) ^ (c & 1 ? 0xedb88320 : 0);
    }
  }
  return (c ^ 0xffffffff) >>> 0;
}

function makePngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crcBuf = Buffer.alloc(4);
  crcBuf.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crcBuf]);
}

/** 构造带有效 CRC32 的 8×8 合成黑白 PNG Data URL（供图片与画板预览使用）。 */
function buildSyntheticPngDataUrl(width = 8, height = 8) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // RGB
  const rows = [];
  for (let y = 0; y < height; y += 1) {
    const row = Buffer.alloc(1 + width * 3, 255);
    row[0] = 0; // filter None
    for (let x = 0; x < width; x += 1) {
      const dark = x >= 2 && x <= 5 && y >= 2 && y <= 5;
      const v = dark ? 0 : 255;
      row[1 + x * 3] = v;
      row[2 + x * 3] = v;
      row[3 + x * 3] = v;
    }
    rows.push(row);
  }
  const png = Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    makePngChunk("IHDR", ihdr),
    makePngChunk("IDAT", deflateSync(Buffer.concat(rows))),
    makePngChunk("IEND", Buffer.alloc(0)),
  ]);
  return `data:image/png;base64,${png.toString("base64")}`;
}

const TINY_PNG_DATA_URL = buildSyntheticPngDataUrl(8, 8);

export function parseSimCliArgs(argv = process.argv.slice(2)) {
  const opts = {
    scenarios: [],
    profile: "fluidnc",
    keepRuntime: false,
    outDir: "",
    scenariosFile: DEFAULT_SCENARIOS_FILE,
    hostBin: "",
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      opts.help = true;
    } else if (arg === "--keep-runtime") {
      opts.keepRuntime = true;
    } else if (arg === "--scenario") {
      const val = argv[++i];
      if (!val) throw new Error("--scenario 缺少场景 ID");
      for (const part of val.split(",").map((s) => s.trim()).filter(Boolean)) {
        opts.scenarios.push(part);
      }
    } else if (arg === "--profile") {
      const val = argv[++i];
      if (!val) throw new Error("--profile 缺少画像名称或文件路径");
      opts.profile = val;
    } else if (arg === "--out-dir") {
      const val = argv[++i];
      if (!val) throw new Error("--out-dir 缺少输出目录");
      opts.outDir = resolve(val);
    } else if (arg === "--scenarios") {
      const val = argv[++i];
      if (!val) throw new Error("--scenarios 缺少场景文件路径");
      opts.scenariosFile = resolve(val);
    } else if (arg === "--host-bin") {
      const val = argv[++i];
      if (!val) throw new Error("--host-bin 缺少 moss-host 路径");
      opts.hostBin = resolve(val);
    } else {
      throw new Error(`未知参数: ${arg}`);
    }
  }
  return opts;
}

export function loadScenariosConfig(filePath = DEFAULT_SCENARIOS_FILE) {
  const parsed = JSON.parse(readFileSync(filePath, "utf8"));
  if (!parsed || !Array.isArray(parsed.scenarios)) {
    throw new Error(`场景文件格式无效：${filePath}`);
  }
  return parsed;
}

function errorCodeOf(data) {
  return data?.error_code || data?.detail?.error_code || data?.result?.error_code || data?.detail || "";
}

function makeHttpRequester({ baseUrl, steps, scenarioId, redactOpts }) {
  return async function apiRequest(method, route, body = undefined, { headers = {}, rawHost = null, customBase = null } = {}) {
    const targetBase = customBase || baseUrl;
    const started = Date.now();
    const url = `${targetBase}${route}`;
    let status = 0;
    let data = null;
    let rawText = "";
    let transportError = null;

    try {
      if (rawHost) {
        // 使用原始 http.request 以便显式测试非法 Host 头（fetch 会禁止覆写 Host）
        const parsedUrl = new URL(url);
        const payload = body !== undefined ? JSON.stringify(body) : "";
        const res = await new Promise((resolvePromise, rejectPromise) => {
          const req = http.request(
            {
              hostname: parsedUrl.hostname,
              port: Number(parsedUrl.port),
              path: `${parsedUrl.pathname}${parsedUrl.search}`,
              method,
              headers: {
                Host: rawHost,
                Accept: "application/json",
                ...(body !== undefined
                  ? { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(payload) }
                  : {}),
                ...headers,
              },
              timeout: 8000,
            },
            (incoming) => {
              const chunks = [];
              incoming.on("data", (c) => chunks.push(c));
              incoming.on("end", () => {
                resolvePromise({
                  status: incoming.statusCode || 0,
                  text: Buffer.concat(chunks).toString("utf8"),
                });
              });
            },
          );
          req.on("error", rejectPromise);
          req.on("timeout", () => req.destroy(new Error("HTTP 请求超时")));
          if (payload) req.write(payload);
          req.end();
        });
        status = res.status;
        rawText = res.text;
      } else {
        const res = await fetch(url, {
          method,
          headers: {
            Accept: "application/json",
            ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
            ...headers,
          },
          body: body !== undefined ? JSON.stringify(body) : undefined,
          signal: AbortSignal.timeout(15_000),
        });
        status = res.status;
        rawText = await res.text();
      }
      try {
        data = rawText ? JSON.parse(rawText) : null;
      } catch {
        data = { raw: rawText.slice(0, 400) };
      }
    } catch (err) {
      transportError = err?.message || String(err);
    }

    const elapsedMs = Date.now() - started;
    steps.push(
      redactLaserValue(
        {
          tsMs: started,
          scenario: scenarioId,
          method,
          route,
          status,
          elapsedMs,
          request: body ?? null,
          response: data,
          error: transportError,
        },
        redactOpts,
      ),
    );
    return { ok: status >= 200 && status < 300, status, data, elapsedMs, error: transportError };
  };
}

async function fetchGcodeBytes(baseUrl, filePath) {
  const res = await fetch(`${baseUrl}/preview?path=${encodeURIComponent(filePath)}`, {
    signal: AbortSignal.timeout(10_000),
  });
  if (!res.ok) {
    throw new Error(`下载 G-code 失败 (${res.status}): ${filePath}`);
  }
  return Buffer.from(await res.arrayBuffer());
}

function saveGcodeArtifact(gcodeDir, tag, gcodeBytes, expectedSha = "") {
  mkdirSync(gcodeDir, { recursive: true });
  const safeTag = String(tag).replace(/[^A-Za-z0-9._-]/g, "_");
  const file = join(gcodeDir, `${safeTag}.gcode`);
  const actualSha = sha256Hex(gcodeBytes);
  writeFileSync(file, gcodeBytes);
  writeFileSync(
    join(gcodeDir, `${safeTag}.sha256.json`),
    JSON.stringify({ tag: safeTag, bytes: gcodeBytes.length, actual_sha256: actualSha, expected_sha256: expectedSha }, null, 2),
    "utf8",
  );
  return { file, actualSha };
}

function prepareLinesFromGcodeText(text) {
  return String(text ?? "")
    .split(/\r?\n/)
    .map((line) => stripGcodeComments(line))
    .filter((line) => line.length > 0);
}

async function pollWorkflowUntil(api, workflowId, wantedStatuses, timeoutMs = 15_000, predicate = () => true) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    last = await api("POST", "/api/workflow", { action: "status", workflow_id: workflowId });
    const rec = last.data?.result || {};
    if (wantedStatuses.includes(rec.status) && predicate(rec)) {
      return last;
    }
    await sleep(40);
  }
  return last;
}

async function clearUncertainJobs(api) {
  const targets = await api("GET", "/api/jobs/stop-targets");
  const jobs = targets.data?.result?.jobs || [];
  for (const j of jobs) {
    if (j.state === "unknown" && j.job_id) {
      await api("POST", "/api/jobs/acknowledge-stop", { job_id: j.job_id, confirmed: true });
    }
  }
}

function buildScenarioProfile(baseProfile, overrides = {}) {
  const merged = { ...baseProfile, ...(overrides || {}) };
  if (baseProfile.faults || overrides?.faults) {
    merged.faults = { ...(baseProfile.faults || {}), ...(overrides?.faults || {}) };
  }
  return merged;
}

async function executeScenario(scenario, ctx) {
  const { baseUrl, baseProfile, gcodeDir, host, steps, redactOpts, exePath } = ctx;
  const api = makeHttpRequester({ baseUrl, steps, scenarioId: scenario.id, redactOpts });
  const simProfile = buildScenarioProfile(baseProfile, scenario.profileOverrides);
  const sim = await startGrblSimulator(simProfile);
  const startedAt = Date.now();
  const assertions = [];
  const assertCheck = (ok, message, severity = "error", evidence = null, suspectedCode = null) => {
    assertions.push({ ok: Boolean(ok), message, severity, evidence, suspectedCode });
  };

  let trace = {
    kind: scenario.category,
    terminalMode: "completed",
    profile: sim.profile,
    travelBounds: computeTravelBounds(sim.profile),
    events: [],
    lines: [],
    segments: [],
    machineAfter: null,
    preview: null,
    sendResult: null,
    resumeInfo: null,
    estopInfo: null,
    overrideInfo: null,
    homingInfo: null,
    warningExpectations: null,
    wallElapsedMs: 0,
  };

  const netConn = {
    connection_mode: "network",
    network_host: sim.host,
    network_telnet_port: sim.port,
    host: sim.host,
    telnet_port: sim.port,
  };

  let serialLogChunk = "";
  let extraSerialLogs = [];

  try {
    const p = scenario.params || {};
    switch (scenario.id) {
      case "gen_text": {
        const res = await api("POST", "/api/text-task/generate", p);
        const r = res.data?.result || {};
        assertCheck(res.status === 200 && res.data?.success === true, `文字预览应返回 200，实际 ${res.status}`, "error", res.data);
        const gcodeFile = r.artifacts?.gcode_file || r.gcode_file;
        const sha = r.artifacts?.gcode_sha256 || r.gcode_sha256 || r.confirmation_snapshot?.gcode_sha256;
        assertCheck(Boolean(gcodeFile && sha), "必须返回 gcode_file 与 gcode_sha256", "error", { gcodeFile, sha });
        assertCheck((r.artifacts?.time_estimate?.estimated_seconds || r.time_estimate?.estimated_seconds || 0) > 0, "必须返回大于 0 的工时估算");
        if (gcodeFile) {
          const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
          const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, sha);
          trace.preview = { expectedSha256: sha, actualSha256: actualSha, gcodeBytes: bytes };
        }
        break;
      }

      case "gen_image": {
        const up = await api("POST", "/api/upload", { filename: "bench-img.png", content_base64: TINY_PNG_DATA_URL });
        assertCheck(up.status === 200 && up.data?.result?.file_path, "图片上传必须成功", "error", up.data);
        const imgPath = up.data?.result?.file_path;
        const prev = await api("POST", "/api/workflow", {
          action: "preview",
          source_type: "image",
          image_file: imgPath,
          material: p.material || "椴木",
          thickness_mm: p.thickness_mm || 3,
          width_mm: p.width_mm || 20,
          image_mode: p.image_mode || "threshold",
        });
        const r = prev.data?.result || {};
        assertCheck(prev.status === 200 && prev.data?.success === true, `图片预览应返回 200，实际 ${prev.status}`, "error", prev.data);
        const gcodeFile = r.artifacts?.gcode_file;
        const sha = r.artifacts?.gcode_sha256 || r.confirmation_snapshot?.gcode_sha256;
        assertCheck(Boolean(gcodeFile && sha), "图片预览必须生成 G-code 与 SHA-256");
        if (gcodeFile) {
          const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
          const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, sha);
          trace.preview = { expectedSha256: sha, actualSha256: actualSha, gcodeBytes: bytes };
        }
        break;
      }

      case "gen_draw": {
        const scene = {
          type: "excalidraw",
          version: 2,
          elements: [
            {
              id: "rect-1",
              type: "rectangle",
              x: 0,
              y: 0,
              width: 120,
              height: 80,
              angle: 0,
              strokeColor: "#000000",
              backgroundColor: "transparent",
              fillStyle: "solid",
              strokeWidth: 2,
              roughness: 0,
              opacity: 100,
              isDeleted: false,
            },
          ],
        };
        const res = await api("POST", "/api/draw/preview", {
          image_data_url: TINY_PNG_DATA_URL,
          scene_json: scene,
          material: p.material || "椴木",
          thickness_mm: p.thickness_mm || 3,
          mode: p.mode || "vector",
          vector_style: p.vector_style || "outline",
          width_mm: p.width_mm || 30,
          height_mm: p.height_mm || 20,
        });
        const r = res.data?.result || {};
        assertCheck(res.status === 200 && res.data?.success === true, `画板预览应返回 200，实际 ${res.status}`, "error", res.data);
        const gcodeFile = r.artifacts?.gcode_file || r.gcode_file;
        const sha = r.artifacts?.gcode_sha256 || r.gcode_sha256 || r.draw_lab?.content_fingerprint;
        assertCheck(Boolean(gcodeFile && sha && r.draw_lab), "画板预览必须包含 draw_lab、G-code 与 SHA-256");
        if (gcodeFile) {
          const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
          const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, sha);
          trace.preview = { expectedSha256: sha, actualSha256: actualSha, gcodeBytes: bytes };
        }
        break;
      }

      case "gen_material_matrix": {
        const res = await api("POST", "/api/material-lab/preview", p);
        const r = res.data?.result || {};
        assertCheck(res.status === 200 && res.data?.success === true, `材料矩阵预览应返回 200，实际 ${res.status}`, "error", res.data);
        assertCheck(Boolean(r.calibration_id && r.gcode_file && r.gcode_sha256 && Array.isArray(r.cells) && r.cells.length === 4), "矩阵必须含 4 格、G-code 与 SHA");
        if (r.gcode_file) {
          const bytes = await fetchGcodeBytes(baseUrl, r.gcode_file);
          const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, r.gcode_sha256);
          trace.preview = { expectedSha256: r.gcode_sha256, actualSha256: actualSha, gcodeBytes: bytes };
        }
        break;
      }

      case "gen_wizard_patterns": {
        const patterns = Array.isArray(p.patterns) ? p.patterns : ["interval", "scan_offset", "overscan", "kerf"];
        for (const testType of patterns) {
          const body = {
            material: p.material || "椴木",
            thickness_mm: p.thickness_mm || 3,
            test_type: testType,
            ...(testType === "interval" ? { rows: 1, columns: 4, cell_size_mm: 6, cell_gap_mm: 2 } : {}),
            ...(testType === "kerf" ? { laser_mode: "cut", square_mm: 15 } : {}),
            ...(testType === "scan_offset" ? { speeds: [1000, 2000], square_mm: 10 } : {}),
            ...(testType === "overscan" ? { square_mm: 12 } : {}),
          };
          const res = await api("POST", "/api/material-lab/preview", body);
          const r = res.data?.result || {};
          assertCheck(
            res.status === 200 && res.data?.success === true && Boolean(r.gcode_file && r.gcode_sha256),
            `测试图案 ${testType} 预览必须成功并生成 G-code 与 SHA`,
            "error",
            { testType, status: res.status, result: res.data?.result },
          );
          if (r.gcode_file) {
            const bytes = await fetchGcodeBytes(baseUrl, r.gcode_file);
            saveGcodeArtifact(gcodeDir, `${scenario.id}_${testType}`, bytes, r.gcode_sha256);
          }
        }
        break;
      }

      case "send_text_complete":
      case "send_rescaled_s255": {
        const gen = await api("POST", "/api/text-task/generate", {
          ...p,
          work_area_width_mm: sim.profile.travelX || 300,
          work_area_height_mm: sim.profile.travelY || 200,
        });
        assertCheck(gen.status === 200 && gen.data?.success === true, "生成文字任务必须成功", "error", gen.data);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
        const expectedSha = wf.artifacts?.gcode_sha256 || wf.gcode_sha256 || wf.confirmation_snapshot?.gcode_sha256;
        const estSec = wf.artifacts?.time_estimate?.estimated_seconds || wf.time_estimate?.estimated_seconds || 0;
        const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
        const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, expectedSha);

        const sendStart = Date.now();
        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(sendRes.status === 200 && sendRes.data?.success === true, `confirm_send 必须返回 200，实际 ${sendRes.status}`, "error", sendRes.data);
        const fin = await pollWorkflowUntil(api, wfId, ["completed", "failed", "cancelled"], 20_000);
        const finalRec = fin.data?.result || {};
        trace.wallElapsedMs = Date.now() - sendStart;
        assertCheck(finalRec.status === "completed", `发送应以 completed 结束，实际 ${finalRec.status}`, "error", finalRec.send_result);
        assertCheck(
          finalRec.send_result?.lines_sent === finalRec.send_result?.total_lines && finalRec.send_result?.total_lines > 0,
          "所有行均须收到 ok 确认",
          "error",
          finalRec.send_result,
        );
        trace.preview = { expectedSha256: expectedSha, actualSha256: actualSha, gcodeBytes: bytes, estimatedSeconds: estSec };
        trace.sendResult = finalRec.send_result;
        trace.terminalMode = "completed";
        trace.warningExpectations = {
          mustNotMatch: [{ pattern: /未报告行程/, label: "已完整报告行程不应报缺失行程警告" }],
        };
        break;
      }

      case "framing_pause_resume_stop": {
        // 第 1 阶段：启动走边框 → 途中 pause(!) → 验证 Hold → resume(~, confirmed=true) → 跑完
        const framing1Promise = api("POST", "/api/device/framing", { ...p, ...netConn });
        const waitStart = Date.now();
        let activeFramingId = null;
        while (Date.now() - waitStart < 4000) {
          const st = await api("GET", "/api/jobs/stop-targets");
          const fJob = (st.data?.result?.jobs || []).find(
            (j) => j.state === "running" && String(j.job_id || "").startsWith("framing-"),
          );
          if (fJob) {
            activeFramingId = fJob.job_id;
            // 等至少收到一条运动行再暂停，避免仍在 preflight（FluidNC 会带 N<k> 前缀）
            if (sim.lines.some((l) => /^(?:N\d+\s+)?G[01]\s/i.test(l.line))) break;
          }
          await sleep(15);
        }
        assertCheck(Boolean(activeFramingId), "走边框必须登记为 framing-* 活动作业");
        const pauseRes = await api("POST", "/api/safe-action", { action: "pause", job_id: activeFramingId, ...netConn });
        assertCheck(pauseRes.status === 200 && pauseRes.data?.success === true, "走边框暂停 pause 必须成功", "error", pauseRes.data);
        await sleep(30);
        // 未确认 resume 必须拒绝
        const badResume = await api("POST", "/api/safe-action", {
          action: "resume",
          job_id: activeFramingId,
          confirmed: false,
          ...netConn,
        });
        assertCheck(badResume.status === 400, "未确认的继续走边框必须拒绝", "error", badResume.data);
        const resumeRes = await api("POST", "/api/safe-action", {
          action: "resume",
          job_id: activeFramingId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(resumeRes.status === 200 && resumeRes.data?.success === true, "确认后的继续走边框必须成功", "error", resumeRes.data);
        const framing1 = await framing1Promise;
        assertCheck(framing1.status === 200 && framing1.data?.success === true, "恢复后的走边框必须正常完成", "error", framing1.data);
        assertCheck(sim.events.some((e) => e.type === "realtime" && e.byte === 0x21), "暂停必须下发实时字节 !");
        assertCheck(sim.events.some((e) => e.type === "realtime" && e.byte === 0x7e), "继续必须下发实时字节 ~");

        // 第 2 阶段：启动走边框 → 途中 stop_framing → 验证 ! + 0x18 并以 cancelled 结束
        const preEventsCount = sim.events.length;
        const framing2Promise = api("POST", "/api/device/framing", { ...p, ...netConn });
        const wait2Start = Date.now();
        let framing2Id = null;
        while (Date.now() - wait2Start < 4000) {
          const st = await api("GET", "/api/jobs/stop-targets");
          const fJob = (st.data?.result?.jobs || []).find(
            (j) => j.state === "running" && String(j.job_id || "").startsWith("framing-"),
          );
          if (fJob) {
            framing2Id = fJob.job_id;
            if (sim.events.slice(preEventsCount).some((e) => e.type === "line" && /^(?:N\d+\s+)?G[01]\s/i.test(e.line))) break;
          }
          await sleep(15);
        }
        assertCheck(Boolean(framing2Id), "第二次走边框必须登记活动作业");
        const stopRes = await api("POST", "/api/safe-action", { action: "stop_framing", job_id: framing2Id, ...netConn });
        assertCheck(stopRes.status === 200 && stopRes.data?.success === true, "stop_framing 必须成功", "error", stopRes.data);
        const framing2 = await framing2Promise;
        assertCheck(framing2.status === 409, `stop_framing 后走边框应返回 409 cancelled，实际 ${framing2.status}`, "error", framing2.data);
        const ev2 = sim.events.slice(preEventsCount);
        const holdIdx = ev2.findIndex((e) => e.type === "realtime" && e.byte === 0x21);
        const resetIdx = ev2.findIndex((e, idx) => idx > holdIdx && e.type === "realtime" && e.byte === 0x18);
        assertCheck(holdIdx >= 0 && resetIdx > holdIdx, "stop_framing 必须先发 ! 再发 0x18");
        trace.kind = "framing";
        trace.framingPower = 0;
        trace.terminalMode = "cancelled";
        await clearUncertainJobs(api);
        break;
      }

      case "send_cancel": {
        const gen = await api("POST", "/api/text-task/generate", p);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
        const expectedSha = wf.artifacts?.gcode_sha256 || wf.gcode_sha256;
        const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
        const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, expectedSha);

        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(sendRes.status === 200, "发送必须启动成功", "error", sendRes.data);
        const total = sendRes.data?.result?.send_result?.total_lines || 0;
        const t0 = Date.now();
        while (sim.snapshot().machine.ackedLines < 3 && Date.now() - t0 < 5000) {
          await sleep(10);
        }
        const cancelRes = await api("POST", "/api/workflow", { action: "cancel", workflow_id: wfId });
        assertCheck(cancelRes.status === 200 && cancelRes.data?.result?.status === "cancelled", "取消必须返回 cancelled", "error", cancelRes.data);
        const fin = await pollWorkflowUntil(
          api,
          wfId,
          ["cancelled", "failed", "completed"],
          10_000,
          (rec) => rec.send_result?.can_resume !== undefined,
        );
        const sr = fin.data?.result?.send_result || {};
        assertCheck(fin.data?.result?.status === "cancelled", `取消终态应为 cancelled，实际 ${fin.data?.result?.status}`, "error", sr);
        assertCheck(sr.lines_sent > 0 && sr.lines_sent < total, `应停在发送中途（${sr.lines_sent}/${total}）`);

        const holdIdx = sim.events.findIndex((e) => e.type === "realtime" && e.byte === 0x21);
        const queryIdx = sim.events.findIndex((e, idx) => idx > holdIdx && e.type === "realtime" && e.byte === 0x3f);
        const resetIdx = sim.events.findIndex((e, idx) => idx > queryIdx && e.type === "realtime" && e.byte === 0x18);
        assertCheck(holdIdx >= 0 && queryIdx > holdIdx && resetIdx > queryIdx, "取消序列必须遵循 ! → ? → 0x18");

        trace.terminalMode = "cancelled";
        trace.preview = { expectedSha256: expectedSha, actualSha256: actualSha, gcodeBytes: bytes };
        trace.sendResult = sr;
        await clearUncertainJobs(api);
        break;
      }

      case "send_estop_realtime": {
        const gen = await api("POST", "/api/text-task/generate", p);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
        const expectedSha = wf.artifacts?.gcode_sha256 || wf.gcode_sha256;
        const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
        const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, expectedSha);

        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(sendRes.status === 200, "发送必须启动成功", "error", sendRes.data);
        const totalLines = sendRes.data?.result?.send_result?.total_lines || 0;

        // 等模拟器收到 >= 8 行（触发 freezeAfterLines 顺序流完全卡住，此时 RX/ACK 已背压堵塞）
        const t0 = Date.now();
        while (sim.lines.filter((l) => !l.line.startsWith("$")).length < 8 && Date.now() - t0 < 6000) {
          await sleep(15);
        }
        const gcodeLinesBefore = sim.lines.filter((l) => !l.line.startsWith("$"));
        const jobConnId = gcodeLinesBefore[0]?.connId ?? null;
        const requestedAtMs = Date.now();
        const estopRes = await api("POST", "/api/safe-action", {
          action: "emergency_stop",
          confirmed: true,
          job_id: wfId,
          ...netConn,
        });
        assertCheck(estopRes.status === 200 && estopRes.data?.success === true, "急停接口必须返回 200", "safety", estopRes.data);
        const fin = await pollWorkflowUntil(api, wfId, ["cancelled", "failed", "completed"], 10_000);
        const resetEv = sim.events.find((e) => e.type === "realtime" && e.byte === 0x18 && e.tsMs >= requestedAtMs - 5);
        const linesBeforeReset = resetEv
          ? sim.lines.filter((l) => !l.line.startsWith("$") && l.tsMs <= resetEv.tsMs).length
          : sim.lines.filter((l) => !l.line.startsWith("$")).length;

        trace.terminalMode = "estop";
        trace.preview = { expectedSha256: expectedSha, actualSha256: actualSha, gcodeBytes: bytes };
        trace.sendResult = fin.data?.result?.send_result;
        trace.estopInfo = {
          requestedAtMs,
          maxLatencyMs: p.maxEstopLatencyMs ?? 500,
          jobConnId,
          totalLines,
          linesReceivedBeforeReset: linesBeforeReset,
        };
        await clearUncertainJobs(api);
        break;
      }

      case "send_resume_ln": {
        const gen = await api("POST", "/api/text-task/generate", p);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
        const expectedSha = wf.artifacts?.gcode_sha256 || wf.gcode_sha256;
        const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
        const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, expectedSha);
        const preparedLines = prepareLinesFromGcodeText(bytes.toString("utf8"));

        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(sendRes.status === 200, "首轮发送必须启动成功", "error", sendRes.data);
        const total = sendRes.data?.result?.send_result?.total_lines || 0;

        // 等首轮发送推进至少 10 行后再取消
        await pollWorkflowUntil(
          api,
          wfId,
          ["sending", "completed", "failed", "cancelled"],
          8000,
          (rec) => rec.status !== "sending" || (rec.send_result?.lines_sent || 0) >= 10,
        );
        const cancelRes = await api("POST", "/api/workflow", { action: "cancel", workflow_id: wfId });
        assertCheck(cancelRes.status === 200, "首轮取消必须成功", "error", cancelRes.data);
        const stopped = await pollWorkflowUntil(
          api,
          wfId,
          ["cancelled", "failed", "completed"],
          15_000,
          (rec) => rec.status !== "cancelled" || rec.send_result?.can_resume !== undefined,
        );
        const stopSr = stopped.data?.result?.send_result || {};
        assertCheck(stopSr.can_resume === true, `Hold:0|Ln 证实断点后必须 can_resume=true`, "safety", stopSr);
        assertCheck(
          stopSr.resumable_from_line > 0 && stopSr.resumable_from_line === stopSr.lines_completed,
          `resumable_from_line (${stopSr.resumable_from_line}) 须等于 lines_completed (${stopSr.lines_completed})`,
          "safety",
          stopSr,
        );

        // 记录续雕阶段的起始偏移
        const resumeLineStartIdx = sim.lines.length;
        const resumeSegStartIdx = sim.segments.length;
        sim.setFaults({ ackDelayMs: 1 });

        const resumeRes = await api("POST", "/api/workflow", {
          action: "resume",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(resumeRes.status === 200 && resumeRes.data?.success === true, "断点续雕必须启动成功", "error", resumeRes.data);
        const fin = await pollWorkflowUntil(api, wfId, ["completed", "failed", "cancelled"], 15_000);
        const finRec = fin.data?.result || {};
        assertCheck(finRec.status === "completed", `续雕应完成，实际 ${finRec.status}`, "error", finRec.send_result);
        assertCheck(finRec.send_result?.lines_sent === total, `续雕完成后总行数应为 ${total}，实际 ${finRec.send_result?.lines_sent}`);

        trace.terminalMode = "completed";
        trace.preview = { expectedSha256: expectedSha, actualSha256: actualSha, gcodeBytes: bytes };
        trace.sendResult = finRec.send_result;
        trace.resumeInfo = {
          resumableFromLine: stopSr.resumable_from_line,
          linesCompleted: stopSr.lines_completed,
          resumeBoundarySource: stopSr.resume_boundary_source,
          preparedLines,
          resumeWireLines: sim.lines.slice(resumeLineStartIdx),
          resumeSegments: sim.segments.slice(resumeSegStartIdx),
        };
        break;
      }

      case "override_active_and_idle": {
        // Part A: 空闲时（无运行中任务）倍率必须被拒绝且模拟器零倍率字节
        const idleActions = ["feed_override_plus_10", "feed_override_reset", "power_override_minus_10", "power_override_reset"];
        const idleResponses = [];
        let idleAllRejected = true;
        for (const action of idleActions) {
          const r = await api("POST", "/api/safe-action", { action, ...netConn });
          idleResponses.push({ action, status: r.status, code: errorCodeOf(r.data), data: r.data });
          if (r.status !== 400 || errorCodeOf(r.data) !== "no_active_job") {
            idleAllRejected = false;
          }
        }
        const idleOverrideBytesCount = sim.events.filter((e) => e.type === "realtime" && e.byte >= 0x90 && e.byte <= 0x9d).length;
        assertCheck(idleAllRejected && idleOverrideBytesCount === 0, "无运行中任务时全部倍率请求必须返回 400 no_active_job 且零字节发往设备", "safety", {
          idleResponses,
          idleOverrideBytesCount,
        });

        // Part B: 运行中任务期间倍率必须命中原任务连接通道
        const gen = await api("POST", "/api/text-task/generate", p);
        const wfId = gen.data?.result?.workflow_id;
        const jobStartMs = Date.now();
        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(sendRes.status === 200, "倍率测试任务必须启动成功", "error", sendRes.data);
        const t0 = Date.now();
        while (!sim.lines.some((l) => !l.line.startsWith("$")) && Date.now() - t0 < 5000) {
          await sleep(10);
        }
        const jobConnId = sim.lines.find((l) => !l.line.startsWith("$"))?.connId ?? null;
        const activeActions = ["feed_override_plus_10", "feed_override_reset", "power_override_minus_10", "power_override_reset"];
        for (const action of activeActions) {
          const r = await api("POST", "/api/safe-action", { action, ...netConn });
          assertCheck(r.status === 200 && r.data?.success === true, `运行中倍率 ${action} 必须成功`, "error", r.data);
        }
        const fin = await pollWorkflowUntil(api, wfId, ["completed", "failed", "cancelled"], 20_000);
        const jobEndMs = Date.now();
        assertCheck(fin.data?.result?.status === "completed", `倍率测试任务应正常完成，实际 ${fin.data?.result?.status}`);

        trace.terminalMode = "completed";
        trace.overrideInfo = {
          idleRejectedExpected: true,
          idleAllRejected,
          idleOverrideBytesCount,
          idleResponses,
          expectActiveOverrideCount: activeActions.length,
          activeWindows: [{ connId: jobConnId, startMs: jobStartMs, endMs: jobEndMs }],
        };
        break;
      }

      case "move_framing": {
        const res = await api("POST", "/api/device/framing", { ...p, ...netConn });
        assertCheck(res.status === 200 && res.data?.success === true, `走边框必须成功，实际 ${res.status}`, "error", res.data);
        trace.kind = "framing";
        trace.framingPower = p.power ?? 0;
        trace.terminalMode = "completed";
        break;
      }

      case "move_jog_relative": {
        const res = await api("POST", "/api/safe-action", {
          action: "relative_move",
          x: p.x ?? 5,
          y: p.y ?? 3,
          feed_rate: p.feed_rate ?? 1200,
          confirmed: true,
          ...netConn,
        });
        assertCheck(res.status === 200 && res.data?.success === true, `点动相对移动必须成功，实际 ${res.status}`, "error", res.data);
        trace.kind = "relative_move";
        trace.terminalMode = "completed";
        break;
      }

      case "move_return_origin": {
        const res = await api("POST", "/api/safe-action", {
          action: "absolute_move",
          x: p.x ?? 0,
          y: p.y ?? 0,
          feed_rate: p.feed_rate ?? 1000,
          confirmed: true,
          ...netConn,
        });
        assertCheck(res.status === 200 && res.data?.success === true, `回原点必须成功，实际 ${res.status}`, "error", res.data);
        const nonSys = sim.lines.filter((l) => !l.line.startsWith("$")).map((l) => l.line);
        const hasG1S0 = nonSys.some((line) => /\bG1\b/i.test(line) && /\bS0(?:\.0*)?\b/i.test(line));
        const hasG0Move = nonSys.some((line) => /\bG0\b/i.test(line) && /[XY]/i.test(line));
        assertCheck(hasG1S0 && !hasG0Move, `回原点必须使用带 S0 的 G1 控速移动（非 G0），实际下发：${nonSys.join(" | ")}`, "safety", {
          lines: nonSys,
        }, "crates/laser-tools/src/lib.rs (absolute_move)");
        trace.kind = "absolute_move";
        trace.terminalMode = "completed";
        break;
      }

      case "move_home_enabled_and_disabled": {
        // Part A: $22=1（开启回零）
        const simEnabled = await startGrblSimulator(
          buildScenarioProfile(baseProfile, { homingEnabled: true, ackDelayMs: 2 }),
        );
        try {
          const connA = {
            connection_mode: "network",
            network_host: simEnabled.host,
            network_telnet_port: simEnabled.port,
            host: simEnabled.host,
            telnet_port: simEnabled.port,
          };
          const probeA = await api("POST", "/api/connection/check", connA);
          assertCheck(probeA.data?.result?.homing_enabled === true, "$22=1 时握手应返回 homing_enabled=true", "error", probeA.data?.result);
          const homeA = await api("POST", "/api/safe-action", { action: "home", confirmed: true, ...connA });
          assertCheck(homeA.status === 200 && homeA.data?.success === true, "$22=1 时机械回零应成功", "error", homeA.data);
          assertCheck(simEnabled.lines.some((l) => l.line === "$H"), "$22=1 时应向模拟器发送 $H");
          extraSerialLogs.push(`--- [${scenario.id} / homing=$22=1] ---\n${simEnabled.formatSerialLog()}`);
        } finally {
          await simEnabled.close();
        }

        // Part B: $22=0（关闭回零）— 使用主 sim 的覆盖实例（需要独立 $22=0 模拟器，因为 sim 已创建）
        const simDisabled = await startGrblSimulator(
          buildScenarioProfile(baseProfile, { homingEnabled: false, ackDelayMs: 2 }),
        );
        try {
          const connB = {
            connection_mode: "network",
            network_host: simDisabled.host,
            network_telnet_port: simDisabled.port,
            host: simDisabled.host,
            telnet_port: simDisabled.port,
          };
          const probeB = await api("POST", "/api/connection/check", connB);
          const homeB = await api("POST", "/api/safe-action", { action: "home", confirmed: true, ...connB });
          const snapB = simDisabled.snapshot();
          trace.profile = snapB.profile;
          trace.events = snapB.events;
          trace.lines = snapB.lines;
          trace.segments = snapB.segments;
          trace.machineAfter = snapB.machine;
          trace.homingInfo = {
            homingEnabled: false,
            probeReportedHomingEnabled: probeB.data?.result?.homing_enabled,
            homeHttpStatus: homeB.status,
            homeResponse: homeB.data,
          };
          extraSerialLogs.push(`--- [${scenario.id} / homing=$22=0] ---\n${simDisabled.formatSerialLog()}`);
          await clearUncertainJobs(api);
        } finally {
          await simDisabled.close();
        }
        return finalizeScenario(scenario, startedAt, assertions, trace, extraSerialLogs.join("\n"));
      }

      case "fault_mid_alarm":
      case "fault_mid_disconnect":
      case "fault_mid_door_open":
      case "fault_soft_limit": {
        const gen = await api("POST", "/api/text-task/generate", p);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
        const expectedSha = wf.artifacts?.gcode_sha256 || wf.gcode_sha256;
        const bytes = await fetchGcodeBytes(baseUrl, gcodeFile);
        const { actualSha } = saveGcodeArtifact(gcodeDir, scenario.id, bytes, expectedSha);

        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(sendRes.status === 200, "故障测试任务必须启动进入发送阶段", "error", sendRes.data);
        const fin = await pollWorkflowUntil(api, wfId, ["failed", "completed", "cancelled"], 15_000);
        const rec = fin.data?.result || {};
        const sr = rec.send_result || {};
        assertCheck(rec.status === "failed", `注入故障后任务必须记 failed，实际 ${rec.status}`, "safety", sr);
        assertCheck(sr.can_resume !== true, `故障中止后禁止标为可盲目续雕（can_resume=${sr.can_resume}）`, "safety", sr);
        // 核对连接次数（只建连 1 次，绝不自动重发）
        assertCheck(sim.connections.length === 1, `故障后绝不能自动重连重发（连接次数=${sim.connections.length}）`, "safety", {
          connections: sim.connections.length,
        });
        trace.terminalMode = "fault";
        trace.preview = { expectedSha256: expectedSha, actualSha256: actualSha, gcodeBytes: bytes };
        trace.sendResult = sr;
        await clearUncertainJobs(api);
        break;
      }

      case "fault_slow_ack_and_no_travel": {
        // 1. 不报行程 + 软限位关闭 + 慢 ACK（45ms）+ 预览超速警告
        const gen = await api("POST", "/api/text-task/generate", {
          ...p,
          machine_motion: { x_max_rate_mm_per_min: 400, y_max_rate_mm_per_min: 400 },
          manual_params: { feed_rate: 1500 },
        });
        assertCheck(gen.status === 200, "慢 ACK 测试生成预览必须成功", "error", gen.data);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const previewWarnings = wf.artifacts?.warnings || wf.warnings || [];
        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(sendRes.status === 200, "不报行程时应放行发送并附警告", "error", sendRes.data);
        const fin = await pollWorkflowUntil(api, wfId, ["completed", "failed", "cancelled"], 25_000);
        const rec = fin.data?.result || {};
        assertCheck(rec.status === "completed", `慢 ACK 下任务应稳定跑完，实际 ${rec.status}`, "error", rec.send_result);

        // 2. 另测机械坐标靠近行程尽头时的越界预警（I2）
        const simEdge = await startGrblSimulator(
          buildScenarioProfile(baseProfile, {
            firmware: "fluidnc",
            travelX: 100,
            travelY: 100,
            homingEnabled: true,
            homingMposX: 0,
            homingMposY: 0,
            homingPositiveDirection: false,
            initialMpos: [95, 95, 0],
            ackDelayMs: 1,
          }),
        );
        let edgeWarnings = [];
        try {
          const gen2 = await api("POST", "/api/text-task/generate", {
            text: "EDGE",
            material: "椴木",
            thickness_mm: 3,
            font_size_mm: 8,
            origin: "lower_left",
            text_fill: "outline",
          });
          const wf2Id = gen2.data?.result?.workflow_id;
          const send2 = await api("POST", "/api/workflow", {
            action: "confirm_send",
            workflow_id: wf2Id,
            confirmed: true,
            connection_mode: "network",
            network_host: simEdge.host,
            network_telnet_port: simEdge.port,
          });
          const fin2 = await pollWorkflowUntil(api, wf2Id, ["completed", "failed", "cancelled"], 15_000);
          edgeWarnings = fin2.data?.result?.send_result?.warnings || send2.data?.result?.send_result?.warnings || [];
          extraSerialLogs.push(`--- [${scenario.id} / edge-mpos] ---\n${simEdge.formatSerialLog()}`);
        } finally {
          await simEdge.close();
        }

        trace.terminalMode = "completed";
        trace.preview = { warnings: previewWarnings };
        trace.sendResult = {
          ...(rec.send_result || {}),
          warnings: [...(rec.send_result?.warnings || []), ...edgeWarnings],
        };
        trace.warningExpectations = {
          mustMatch: [
            { pattern: /未报告行程.*软限位/, label: "固件未报行程且软限位未开警告" },
            { pattern: /超过固件最大速度/, label: "超速预览警告" },
            { pattern: /行程尽头/, label: "机械坐标靠近边缘预警" },
          ],
        };
        break;
      }

      case "gate_unconfirmed_send": {
        const gen = await api("POST", "/api/text-task/generate", p);
        const wfId = gen.data?.result?.workflow_id;
        const r1 = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: false,
          ...netConn,
        });
        const r2 = await api("POST", "/api/text-task/send", {
          workflow_id: wfId,
          confirmed: false,
          ...netConn,
        });
        assertCheck(r1.status === 400 && r2.status === 400, `未确认发送必须返回 400（实际 ${r1.status} / ${r2.status}）`, "safety", { r1: r1.data, r2: r2.data });
        assertCheck(sim.connections.length === 0 && sim.lines.length === 0, "未确认发送绝不能连接设备或写入任何字节", "safety", {
          connections: sim.connections.length,
          lines: sim.lines.length,
        });
        break;
      }

      case "gate_tampered_gcode_sha": {
        const gen = await api("POST", "/api/text-task/generate", p);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
        assertCheck(Boolean(gcodeFile), "必须先生成 G-code 文件");
        // 在隔离运行目录内篡改物理 G-code 文件
        const mossDir = host.mossDir();
        const candidatePaths = [
          join(mossDir, "generated_assets", gcodeFile),
          join(mossDir, "uploads", gcodeFile),
          join(mossDir, "previews", gcodeFile),
        ];
        const targetPath = candidatePaths.find((fp) => existsSync(fp));
        assertCheck(Boolean(targetPath), "应在隔离运行目录中找到物理 G-code 文件");
        if (targetPath) {
          writeFileSync(targetPath, `${readFileSync(targetPath, "utf8")}\n; tampered-by-l0-bench\n`, "utf8");
        }
        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(
          sendRes.status === 400 && errorCodeOf(sendRes.data) === "send_gate_rejected",
          `物理 G-code 篡改后必须返回 400 send_gate_rejected（实际 ${sendRes.status} / ${errorCodeOf(sendRes.data)}）`,
          "safety",
          sendRes.data,
        );
        assertCheck(sim.connections.length === 0 && sim.lines.length === 0, "SHA 校验失败绝不能连接设备或写入任何字节", "safety", {
          connections: sim.connections.length,
          lines: sim.lines.length,
        });
        break;
      }

      case "gate_exceeds_travel": {
        const gen = await api("POST", "/api/text-task/generate", p);
        assertCheck(gen.status === 200, "应先用大预览幅面生成宽于 60mm 的刀路", "error", gen.data);
        const wf = gen.data?.result || {};
        const wfId = wf.workflow_id;
        const sendRes = await api("POST", "/api/workflow", {
          action: "confirm_send",
          workflow_id: wfId,
          confirmed: true,
          ...netConn,
        });
        assertCheck(
          sendRes.status === 400 && errorCodeOf(sendRes.data) === "job_exceeds_travel",
          `刀路超出 60×60 固件行程必须返回 400 job_exceeds_travel（实际 ${sendRes.status} / ${errorCodeOf(sendRes.data)}）`,
          "safety",
          sendRes.data,
        );
        const motionLines = sim.lines.filter((l) => !l.line.startsWith("$"));
        assertCheck(motionLines.length === 0, `超行程被拒后绝不能下发任何 G-code 运动行（实际 ${motionLines.length} 行）`, "safety", {
          motionLines: motionLines.map((l) => l.line),
        });
        break;
      }

      case "gate_lan_readonly_and_auth": {
        const gen = await api("POST", "/api/text-task/generate", p);
        const wfId = gen.data?.result?.workflow_id;
        // 1. 跨站 Origin 拦截（回环免鉴权防 CSRF）
        const csrf = await api(
          "POST",
          "/api/workflow",
          { action: "confirm_send", workflow_id: wfId, confirmed: true, ...netConn },
          { headers: { Origin: "https://untrusted-site.example" } },
        );
        assertCheck(
          csrf.status === 403 && errorCodeOf(csrf.data) === "origin_not_allowed",
          `跨源 POST 发送必须被 403 origin_not_allowed 拦截（实际 ${csrf.status}）`,
          "safety",
          csrf.data,
        );
        // 2. DNS 重绑定 Host 拦截
        const rebind = await api(
          "POST",
          "/api/workflow",
          { action: "confirm_send", workflow_id: wfId, confirmed: true, ...netConn },
          { rawHost: "evil-rebind.example:9666" },
        );
        assertCheck(
          rebind.status === 403 && errorCodeOf(rebind.data) === "host_not_allowed",
          `非法 Host 重绑定请求必须被 403 host_not_allowed 拦截（实际 ${rebind.status}）`,
          "safety",
          rebind.data,
        );
        // 3. 无效桌面会话头拦截
        const badSess = await api(
          "POST",
          "/api/workflow",
          { action: "confirm_send", workflow_id: wfId, confirmed: true, ...netConn },
          { headers: { "X-Moss-Desktop-Session": "0".repeat(64) } },
        );
        assertCheck(
          badSess.status === 403 && errorCodeOf(badSess.data) === "desktop_session_invalid",
          `无效桌面会话头必须被 403 desktop_session_invalid 拦截（实际 ${badSess.status}）`,
          "safety",
          badSess.data,
        );

        // 4. 独立短生命周期严格模式宿主验证：局域网未鉴权 401、已配对局域网参考写 403 lan_read_only、
        //    已配对局域网发送返回 202 approval_required（不直接下发）、回环无桌面会话 403 strict_mode_blocked
        const lanIp = findLocalLanIpv4();
        const strictDir = mkdtempSync(join(tmpdir(), "moss-l0-strict-"));
        let strictHost = null;
        try {
          strictHost = await startIsolatedHost({
            exePath,
            runtimeDir: strictDir,
            webHost: lanIp ? "0.0.0.0" : "127.0.0.1",
            enableProcessTrace: false,
            strictMode: true,
          });
          const sGen = await api("POST", "/api/text-task/generate", p, { customBase: strictHost.baseUrl });
          const sWfId = sGen.data?.result?.workflow_id;

          // 回环无桌面会话在严格模式下发送 -> 403 strict_mode_blocked
          const loopStrict = await api(
            "POST",
            "/api/workflow",
            { action: "confirm_send", workflow_id: sWfId, confirmed: true, ...netConn },
            { customBase: strictHost.baseUrl },
          );
          assertCheck(
            loopStrict.status === 403 && errorCodeOf(loopStrict.data) === "strict_mode_blocked",
            `严格模式无桌面会话发送必须返回 403 strict_mode_blocked（实际 ${loopStrict.status}）`,
            "safety",
            loopStrict.data,
          );

          if (lanIp) {
            const lanBase = `http://${lanIp}:${strictHost.port}`;
            // 局域网未携带 Bearer 发送 -> 401
            const lanUnauth = await api(
              "POST",
              "/api/workflow",
              { action: "confirm_send", workflow_id: sWfId, confirmed: true, ...netConn },
              { customBase: lanBase },
            );
            assertCheck(lanUnauth.status === 401, `未鉴权局域网发送必须返回 401（实际 ${lanUnauth.status}）`, "safety", lanUnauth.data);

            // 签发配对码并配对取得局域网只读身份 Bearer token
            const chal = await api("POST", "/v1/auth/challenge", {}, { customBase: strictHost.baseUrl });
            const code = chal.data?.result?.pairing_code;
            const paired = await api("POST", "/v1/auth/pair", { pairing_code: code }, { customBase: lanBase });
            const token = paired.data?.result?.token;
            assertCheck(Boolean(token), "局域网配对测试应取得临时 token");

            // 局域网只读身份写设置 -> 403 lan_read_only
            const lanWrite = await api(
              "POST",
              "/api/settings",
              { auto_check_update: false },
              { customBase: lanBase, headers: { Authorization: `Bearer ${token}` } },
            );
            assertCheck(
              lanWrite.status === 403 && errorCodeOf(lanWrite.data) === "lan_read_only",
              `局域网只读身份修改设置必须返回 403 lan_read_only（实际 ${lanWrite.status}）`,
              "safety",
              lanWrite.data,
            );

            // 局域网身份直接发送（严格模式）-> 202 approval_required（禁止直接驱动设备）
            const lanSend = await api(
              "POST",
              "/api/workflow",
              { action: "confirm_send", workflow_id: sWfId, confirmed: true, ...netConn },
              { customBase: lanBase, headers: { Authorization: `Bearer ${token}` } },
            );
            assertCheck(
              lanSend.status === 202 && lanSend.data?.result?.status === "approval_required",
              `严格模式下局域网发送必须阻断为 202 approval_required（实际 ${lanSend.status}）`,
              "safety",
              lanSend.data,
            );

            // 局域网只读身份调用点射（严格模式）-> 403 strict_mode_blocked
            const lanPointer = await api(
              "POST",
              "/api/device/pointer-fire",
              { confirmed: true, power: 5, duration_ms: 50, ...netConn },
              { customBase: lanBase, headers: { Authorization: `Bearer ${token}` } },
            );
            assertCheck(
              lanPointer.status === 403 && errorCodeOf(lanPointer.data) === "strict_mode_blocked",
              `严格模式下局域网点射必须返回 403 strict_mode_blocked（实际 ${lanPointer.status}）`,
              "safety",
              lanPointer.data,
            );
          }
        } finally {
          if (strictHost) await strictHost.stop();
          rmSync(strictDir, { recursive: true, force: true });
        }

        assertCheck(sim.connections.length === 0 && sim.lines.length === 0, "所有被拒门禁请求均不得触发任何设备连接或字节写入", "safety", {
          connections: sim.connections.length,
          lines: sim.lines.length,
        });
        break;
      }

      case "dryrun_mode_enforced": {
        const maxFeed = Number(p.max_feed_mm_min ?? 1600);
        const effMaxFeed = Math.floor(maxFeed / 2);

        // 1) 带 X-Moss-Bench 头的请求不能开关空跑
        const benchToggle = await api(
          "POST",
          "/api/settings",
          { laser_dry_run_enabled: true, dry_run_max_feed_mm_min: maxFeed },
          { headers: { "X-Moss-Bench": "dryrun" } },
        );
        assertCheck(
          benchToggle.status === 403 && errorCodeOf(benchToggle.data) === "bench_cannot_toggle_dry_run",
          `带 X-Moss-Bench 头的请求不得开关空跑（实际 ${benchToggle.status}）`,
          "safety",
          benchToggle.data,
        );

        // 2) 操作者先握手连通激光机，再开启空跑模式（未连接不得开启）
        const probeRes = await api("POST", "/api/connection/check", netConn);
        assertCheck(
          probeRes.status === 200 && probeRes.data?.result?.connected === true,
          `空跑前握手连通模拟机必须成功（实际 ${probeRes.status}）`,
          "safety",
          probeRes.data,
        );
        const enableRes = await api("POST", "/api/settings", {
          laser_dry_run_enabled: true,
          dry_run_max_feed_mm_min: maxFeed,
        });
        assertCheck(
          enableRes.status === 200 && enableRes.data?.result?.laser_dry_run_enabled === true,
          "开启空跑模式必须成功",
          "safety",
          enableRes.data,
        );

        try {
          // 2a) 高功率文字任务（带 X-Moss-Bench: dryrun 标签）
          const gen = await api(
            "POST",
            "/api/text-task/generate",
            {
              text: p.text || "DRY-OK",
              material: p.material || "椴木",
              thickness_mm: p.thickness_mm ?? 3,
              font_size_mm: p.font_size_mm ?? 8,
              text_fill: p.text_fill || "outline",
            },
            { headers: { "X-Moss-Bench": "dryrun" } },
          );
          const wfId = gen.data?.result?.workflow_id;
          assertCheck(Boolean(wfId), "空跑文字任务预览生成必须成功", "error", gen.data);
          const sendRes = await api(
            "POST",
            "/api/workflow",
            { action: "confirm_send", workflow_id: wfId, confirmed: true, ...netConn },
            { headers: { "X-Moss-Bench": "dryrun" } },
          );
          assertCheck(sendRes.status === 200 && sendRes.data?.success === true, "空跑文字任务发送必须启动", "error", sendRes.data);
          const fin = await pollWorkflowUntil(api, wfId, ["completed", "failed", "cancelled"], 20_000);
          assertCheck(fin.data?.result?.status === "completed", `空跑文字任务应完成，实际 ${fin.data?.result?.status}`, "error", fin.data?.result?.send_result);

          // 2b) 点动与低功率点射也必须强制 S0 与限速
          const jogRes = await api(
            "POST",
            "/api/safe-action",
            { action: "relative_move", x: 5, y: 0, feed_rate: 3000, confirmed: true, ...netConn },
            { headers: { "X-Moss-Bench": "dryrun" } },
          );
          assertCheck(jogRes.status === 200 && jogRes.data?.success === true, "空跑点动必须成功", "error", jogRes.data);

          const pfRes = await api("POST", "/api/device/pointer-fire", {
            confirmed: true,
            power: 10,
            duration_ms: 50,
            ...netConn,
          });
          assertCheck(pfRes.status === 200 && pfRes.data?.success === true, "空跑点射必须成功", "error", pfRes.data);

          // 2c) LC：空跑期间上传含 G28 的外部 G-code 必须在预检阶段直接拒绝，且不下发任何 G28 到模拟器
          const linesBeforeG28 = sim.lines.length;
          const g28Content = "G21\nG90\nG28 X0 Y0\nG1 X5 Y0 F600 S100\nM5\n";
          const uploadG28 = await api(
            "POST",
            "/api/upload",
            {
              filename: "lc-g28-test.gcode",
              content_base64: Buffer.from(g28Content, "utf8").toString("base64"),
            },
            { headers: { "X-Moss-Bench": "dryrun" } },
          );
          assertCheck(uploadG28.status === 200 && Boolean(uploadG28.data?.result?.filename), "上传含 G28 的测试 G-code 必须成功", "error", uploadG28.data);
          const previewG28 = await api(
            "POST",
            "/api/workflow",
            {
              action: "preview",
              source_type: "gcode",
              gcode_file: uploadG28.data?.result?.filename,
              material: "椴木",
              thickness_mm: 3,
            },
            { headers: { "X-Moss-Bench": "dryrun" } },
          );
          const g28WfId = previewG28.data?.result?.workflow_id;
          assertCheck(previewG28.status === 200 && Boolean(g28WfId), "含 G28 文件生成预览应成功", "error", previewG28.data);
          const sendG28 = await api(
            "POST",
            "/api/workflow",
            { action: "confirm_send", workflow_id: g28WfId, confirmed: true, ...netConn },
            { headers: { "X-Moss-Bench": "dryrun" } },
          );
          assertCheck(
            sendG28.status === 400 && errorCodeOf(sendG28.data) === "dry_run_rapid_home_blocked",
            `空跑开启时含 G28 的文件必须返回 400 dry_run_rapid_home_blocked（实际 ${sendG28.status} / ${errorCodeOf(sendG28.data)}）`,
            "safety",
            sendG28.data,
          );
          assertCheck(
            sim.lines.length === linesBeforeG28,
            `G28 预检被拒后不得向模拟器写入任何新字节（新增 ${sim.lines.length - linesBeforeG28} 行）`,
            "safety",
            { addedLines: sim.lines.slice(linesBeforeG28).map((l) => l.line) },
          );

          // 核查空跑期间发往模拟器的全部非系统行：无 S>0、无 G0、无 G28/G30、全部 F<=effMaxFeed、且开头注入了 G21 G94 G1 M5 S0 F<eff>
          const dryLines = sim.lines.filter((l) => !l.line.startsWith("$"));
          assertCheck(dryLines.length > 0, "空跑期间应有 G-code 下发到模拟器", "error");
          const expectedPreamble = `G21 G94 G1 M5 S0 F${effMaxFeed}`;
          const hasPreambleM5 = dryLines.some((l) => l.line === expectedPreamble);
          assertCheck(hasPreambleM5, `空跑任务开头必须注入 ${expectedPreamble}`, "safety", {
            sample: dryLines.slice(0, 6).map((l) => l.line),
          });
          let badS = null;
          let badG0 = null;
          let badG28 = null;
          let badF = null;
          for (const rec of dryLines) {
            const parsed = parseGcodeLine(rec.line);
            for (const w of parsed.words) {
              if (w.letter === "S" && w.value > 1e-6) badS = { line: rec.line, s: w.value };
              if (w.letter === "G" && Math.abs(w.value) < 1e-6) badG0 = { line: rec.line };
              if (w.letter === "G" && (Math.abs(w.value - 28) < 1e-6 || Math.abs(w.value - 30) < 1e-6)) {
                badG28 = { line: rec.line, g: w.value };
              }
              if (w.letter === "F" && w.value > effMaxFeed + 1e-6) badF = { line: rec.line, f: w.value, effMaxFeed };
            }
          }
          assertCheck(!badS, `空跑期间严禁出现 S>0（违规：${JSON.stringify(badS)}）`, "safety", badS);
          assertCheck(!badG0, `空跑期间严禁出现未改写的 G0（违规：${JSON.stringify(badG0)}）`, "safety", badG0);
          assertCheck(!badG28, `空跑期间严禁出现 G28 / G30 快速回位（违规：${JSON.stringify(badG28)}）`, "safety", badG28);
          assertCheck(!badF, `空跑期间所有 F 须 <= ${effMaxFeed}（违规：${JSON.stringify(badF)}）`, "safety", badF);
          assertCheck(
            sim.segments.every((seg) => !seg.laserOn),
            "空跑期间模拟器记录的出光段数必须为 0",
            "safety",
            { litCount: sim.segments.filter((s) => s.laserOn).length },
          );
        } finally {
          // 3) 关闭空跑开关，确保不影响后续场景
          await api("POST", "/api/settings", { laser_dry_run_enabled: false });
        }

        // 4) 验证关闭后恢复正常（点射恢复 M3 S10）
        const beforeRestoreCount = sim.lines.length;
        const pfAfter = await api("POST", "/api/device/pointer-fire", {
          confirmed: true,
          power: 10,
          duration_ms: 50,
          ...netConn,
        });
        assertCheck(pfAfter.status === 200 && pfAfter.data?.success === true, "关闭空跑后点射应恢复成功", "error", pfAfter.data);
        const restoredLines = sim.lines.slice(beforeRestoreCount).map((l) => l.line);
        assertCheck(
          restoredLines.some((l) => /\bM3\s+S10\b/.test(l)),
          "关闭空跑开关后激光功率须恢复原样（M3 S10）",
          "safety",
          { restoredLines },
        );
        trace.terminalMode = "completed";
        break;
      }

      default:
        throw new Error(`未实现的场景 ID：${scenario.id}`);
    }

    const snap = sim.snapshot();
    trace.events = snap.events;
    trace.lines = snap.lines;
    trace.segments = snap.segments;
    trace.machineAfter = snap.machine;
    serialLogChunk = [
      `=== [${scenario.id}] ${scenario.name} (profile=${sim.profile.firmware}) ===`,
      sim.formatSerialLog(),
      ...extraSerialLogs,
    ]
      .filter(Boolean)
      .join("\n");
  } catch (err) {
    assertCheck(false, `场景执行抛出异常：${err?.message || String(err)}`, "error", {
      stack: String(err?.stack || "").split("\n").slice(0, 4).join("\n"),
    });
    const snap = sim.snapshot();
    trace.events = snap.events;
    trace.lines = snap.lines;
    trace.segments = snap.segments;
    trace.machineAfter = snap.machine;
    serialLogChunk = `=== [${scenario.id}] (EXCEPTION) ===\n${sim.formatSerialLog()}`;
  } finally {
    await sim.close();
  }

  return finalizeScenario(scenario, startedAt, assertions, trace, serialLogChunk);
}

function finalizeScenario(scenario, startedAt, assertions, trace, serialLogChunk) {
  const durationMs = Date.now() - startedAt;
  const invariantResults = runAllInvariants(trace).map((r) => ({
    scenario: scenario.id,
    ...r,
  }));
  const failedAssertions = assertions.filter((a) => !a.ok);
  const safetyViolations = [
    ...invariantResults.filter((r) => r.status === "fail" && r.severity === "safety"),
    ...failedAssertions.filter((a) => a.severity === "safety").map((a) => ({
      scenario: scenario.id,
      id: `assert_${scenario.id}`,
      severity: "safety",
      status: "fail",
      title: `${scenario.name} 安全断言失败`,
      message: a.message,
      evidence: a.evidence,
      suspectedCode: a.suspectedCode,
    })),
  ];
  const errorViolations = [
    ...invariantResults.filter((r) => r.status === "fail" && r.severity !== "safety"),
    ...failedAssertions.filter((a) => a.severity !== "safety").map((a) => ({
      scenario: scenario.id,
      id: `assert_${scenario.id}`,
      severity: a.severity || "error",
      status: "fail",
      title: `${scenario.name} 断言失败`,
      message: a.message,
      evidence: a.evidence,
      suspectedCode: a.suspectedCode,
    })),
  ];
  const notices = invariantResults.filter((r) => r.status === "notice");
  const passed = safetyViolations.length === 0 && errorViolations.length === 0;

  return {
    id: scenario.id,
    name: scenario.name,
    category: scenario.category,
    passed,
    durationMs,
    assertionsCount: assertions.length,
    failedAssertionsCount: failedAssertions.length,
    safetyViolations,
    errorViolations,
    notices,
    invariants: invariantResults,
    serialLogChunk,
  };
}

export function buildSimReportMarkdown({ runId, profileName, startedAtMs, finishedAtMs, results, summary, redactOpts }) {
  const allProblems = [];
  for (const r of results) {
    for (const v of [...r.safetyViolations, ...r.errorViolations, ...r.notices]) {
      allProblems.push(v);
    }
  }
  allProblems.sort((a, b) => (SEVERITY_RANK[a.severity] ?? 9) - (SEVERITY_RANK[b.severity] ?? 9));

  const lines = [
    `# L0 模拟机全自动测试报告（${runId}）`,
    "",
    `- **固件画像**：\`${profileName}\``,
    `- **开始时间**：${new Date(startedAtMs).toISOString()}`,
    `- **总耗时**：${((finishedAtMs - startedAtMs) / 1000).toFixed(2)}s`,
    `- **场景通过率**：${summary.passedScenarios} / ${summary.totalScenarios}（${(summary.passRate * 100).toFixed(1)}%）`,
    `- **不变量违反（安全类）**：${summary.safetyViolationCount}`,
    `- **一般失败 / 功能违反**：${summary.errorViolationCount}`,
    `- **提示项**：${summary.noticeCount}`,
    "",
    "## 问题列表（按严重程度排序，安全类最高）",
    "",
  ];

  if (allProblems.length === 0) {
    lines.push("本次运行未发现不变量违反或场景失败。", "");
  } else {
    for (let i = 0; i < allProblems.length; i += 1) {
      const p = allProblems[i];
      const badge =
        p.severity === "safety"
          ? "[安全]"
          : p.severity === "error"
            ? "[失败]"
            : p.severity === "warn"
              ? "[告警]"
              : "[提示]";
      lines.push(`### ${i + 1}. ${badge} ${p.title}（场景 \`${p.scenario}\` · \`${p.id}\`）`);
      lines.push(`- **现象**：${redactLaserText(p.message, redactOpts)}`);
      if (p.suspectedCode) {
        lines.push(`- **怀疑位置**：\`${p.suspectedCode}\``);
      }
      if (p.evidence != null) {
        const evJson = JSON.stringify(redactLaserValue(p.evidence, redactOpts));
        lines.push(`- **证据**：\`${evJson.length > 500 ? `${evJson.slice(0, 500)}…` : evJson}\``);
      }
      lines.push("");
    }
  }

  lines.push("## 场景结果总览", "", "| 场景 ID | 分类 | 结果 | 耗时(ms) | 安全违反 | 一般失败 | 提示 |", "| --- | --- | --- | ---: | ---: | ---: | ---: |");
  for (const r of results) {
    lines.push(
      `| \`${r.id}\` | ${mdCell(r.category)} | ${r.passed ? "PASS" : "FAIL"} | ${r.durationMs} | ${r.safetyViolations.length} | ${r.errorViolations.length} | ${r.notices.length} |`,
    );
  }
  lines.push("");
  return redactLaserText(lines.join("\n"), redactOpts);
}

/** 退出码判定：有安全类违反非 0（2），有一般失败非 0（1），全绿 0。 */
export function computeExitCode(summary) {
  if ((summary?.safetyViolationCount || 0) > 0) return 2;
  if ((summary?.errorViolationCount || 0) > 0 || (summary?.failedScenarios || 0) > 0) return 1;
  return 0;
}

export async function runLaserSimBench(opts = {}) {
  const scenariosConfig = loadScenariosConfig(opts.scenariosFile || DEFAULT_SCENARIOS_FILE);
  const baseProfile = loadProfile(opts.profile || "fluidnc");
  const selectedIds = Array.isArray(opts.scenarios) && opts.scenarios.length > 0 ? opts.scenarios : null;
  const scenarios = selectedIds
    ? scenariosConfig.scenarios.filter((s) => selectedIds.includes(s.id))
    : scenariosConfig.scenarios;

  if (scenarios.length === 0) {
    throw new Error(`未匹配到任何场景：${selectedIds?.join(", ") || "(空)"}`);
  }
  if (selectedIds) {
    const found = new Set(scenarios.map((s) => s.id));
    const missing = selectedIds.filter((id) => !found.has(id));
    if (missing.length > 0) {
      throw new Error(`未知场景 ID：${missing.join(", ")}`);
    }
  }

  const exePath = opts.hostBin || resolveHostBinary();
  const runId = formatRunId("sim");
  const outDir = opts.outDir ? resolve(opts.outDir) : join(defaultBenchRoot(), runId);
  const gcodeDir = join(outDir, "gcode");
  mkdirSync(gcodeDir, { recursive: true });

  const isolatedRuntimeDir = mkdtempSync(join(tmpdir(), "moss-laser-sim-"));
  const redactOpts = defaultRedactOpts({
    runtimeDir: isolatedRuntimeDir,
    extraPaths: [outDir],
  });

  let host = null;
  let hostStopped = false;
  const cleanup = async () => {
    if (host && !hostStopped) {
      hostStopped = true;
      await host.stop();
    }
    if (!opts.keepRuntime && existsSync(isolatedRuntimeDir)) {
      rmSync(isolatedRuntimeDir, { recursive: true, force: true });
    }
  };
  const unregisterSignals = registerSignalCleanup(cleanup);

  const startedAtMs = Date.now();
  const steps = [];
  const results = [];
  const allInvariants = [];
  const serialChunks = [];

  try {
    host = await startIsolatedHost({
      exePath,
      runtimeDir: isolatedRuntimeDir,
      webHost: "127.0.0.1",
      enableProcessTrace: true,
      strictMode: false,
    });

    for (const scenario of scenarios) {
      const res = await executeScenario(scenario, {
        baseUrl: host.baseUrl,
        baseProfile,
        gcodeDir,
        host,
        steps,
        redactOpts,
        exePath,
      });
      results.push(res);
      allInvariants.push(...res.invariants);
      if (res.serialLogChunk) serialChunks.push(res.serialLogChunk);
    }

    // 留出极短窗口让后台 ProcessTrace 写线程落盘
    await sleep(150);
    const finishedAtMs = Date.now();

    const traceDir = resolveProcessTraceDir(host.mossDir());
    const { jsonlLines, txtLines } = sliceProcessTrace(traceDir, startedAtMs, finishedAtMs, redactOpts);
    writeTimelineFiles(outDir, jsonlLines, txtLines);

    const categoryDurationsMs = {};
    for (const r of results) {
      categoryDurationsMs[r.category] = (categoryDurationsMs[r.category] || 0) + r.durationMs;
    }
    const safetyViolationCount = results.reduce((n, r) => n + r.safetyViolations.length, 0);
    const errorViolationCount = results.reduce((n, r) => n + r.errorViolations.length, 0);
    const noticeCount = results.reduce((n, r) => n + r.notices.length, 0);
    const passedScenarios = results.filter((r) => r.passed).length;
    const failedScenarios = results.length - passedScenarios;

    const summary = {
      level: "L0-sim",
      runId,
      profile: baseProfile.name || opts.profile || "fluidnc",
      firmware: baseProfile.firmware,
      startedAt: new Date(startedAtMs).toISOString(),
      finishedAt: new Date(finishedAtMs).toISOString(),
      totalDurationMs: finishedAtMs - startedAtMs,
      totalScenarios: results.length,
      passedScenarios,
      failedScenarios,
      passRate: results.length > 0 ? Number((passedScenarios / results.length).toFixed(4)) : 0,
      safetyViolationCount,
      errorViolationCount,
      noticeCount,
      categoryDurationsMs,
      timelineEventsCount: jsonlLines.length,
      keptRuntimeDir: opts.keepRuntime ? isolatedRuntimeDir : null,
      scenarios: results.map((r) => ({
        id: r.id,
        name: r.name,
        category: r.category,
        passed: r.passed,
        durationMs: r.durationMs,
        safetyViolations: r.safetyViolations.length,
        errorViolations: r.errorViolations.length,
        notices: r.notices.length,
      })),
    };

    const redactedSummary = redactLaserValue(summary, redactOpts);
    const redactedInvariants = allInvariants.map((item) => JSON.stringify(redactLaserValue(item, redactOpts)));
    const reportMd = buildSimReportMarkdown({
      runId,
      profileName: summary.profile,
      startedAtMs,
      finishedAtMs,
      results,
      summary,
      redactOpts,
    });

    writeFileSync(join(outDir, "steps.jsonl"), steps.map((s) => JSON.stringify(s)).join("\n") + (steps.length ? "\n" : ""), "utf8");
    writeFileSync(join(outDir, "serial.log"), redactLaserText(serialChunks.join("\n\n"), redactOpts) + "\n", "utf8");
    writeFileSync(join(outDir, "invariants.jsonl"), redactedInvariants.join("\n") + (redactedInvariants.length ? "\n" : ""), "utf8");
    writeFileSync(join(outDir, "report.md"), reportMd, "utf8");
    writeFileSync(join(outDir, "summary.json"), JSON.stringify(redactedSummary, null, 2) + "\n", "utf8");

    const exitCode = computeExitCode(summary);
    return { outDir, summary: redactedSummary, results, exitCode };
  } finally {
    unregisterSignals();
    await cleanup();
  }
}

async function main() {
  const opts = parseSimCliArgs();
  if (opts.help) {
    console.log(
      [
        "用法：node scripts/laser-bench/laser-sim-bench.mjs [选项]",
        "  --scenario <id>       仅运行指定场景（可重复传入或逗号分隔）",
        "  --profile <p>         固件画像：fluidnc（默认）| grbl | <json 文件路径>",
        "  --keep-runtime        保留隔离临时运行目录以便排障",
        "  --out-dir <dir>       指定归档输出目录（默认 <runtime-or-tmp>/laser-bench/sim-<run-id>/）",
        "  --scenarios <file>    指定自定义场景集 JSON",
        "  --host-bin <path>     指定 moss-host 可执行文件路径",
      ].join("\n"),
    );
    return;
  }
  const { outDir, summary, exitCode } = await runLaserSimBench(opts);
  console.log(outDir);
  console.log(
    `[L0] 通过 ${summary.passedScenarios}/${summary.totalScenarios} · 安全类违反 ${summary.safetyViolationCount} · 一般失败 ${summary.errorViolationCount} · 提示 ${summary.noticeCount}`,
  );
  process.exitCode = exitCode;
}

if (process.argv[1] && existsSync(process.argv[1]) && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((err) => {
    console.error(`[L0] 运行失败：${err?.message || String(err)}`);
    process.exitCode = 1;
  });
}
