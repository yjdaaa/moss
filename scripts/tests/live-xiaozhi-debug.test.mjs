import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import {
  NO_ONLINE_DEVICE_MESSAGE,
  evaluateTurnExpectations,
  redactText,
  runLiveDebugBench,
} from "../voice-bench/live-xiaozhi-debug.mjs";

const execFileAsync = promisify(execFile);
const SCRIPT_PATH = fileURLToPath(new URL("../voice-bench/live-xiaozhi-debug.mjs", import.meta.url));

function startFakeDebugServer({ token, devices, turnBehavior }) {
  const traceCalls = [];
  const turnStore = new Map();
  let turnSeq = 0;

  const server = http.createServer((req, res) => {
    const auth = req.headers.authorization || "";
    if (auth !== `Bearer ${token}`) {
      res.writeHead(401, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "unauthorized", message: "调试令牌无效" }));
      return;
    }
    const url = new URL(req.url || "/", "http://127.0.0.1");
    let body = "";
    req.on("data", (chunk) => {
      body += chunk;
    });
    req.on("end", () => {
      const jsonBody = body ? JSON.parse(body) : {};
      if (req.method === "GET" && url.pathname === "/debug/v1/devices") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(
          JSON.stringify({
            mode: "chat",
            active_laser_job: false,
            devices: typeof devices === "function" ? devices() : devices,
          }),
        );
        return;
      }
      if (req.method === "POST" && url.pathname === "/debug/v1/process-trace") {
        const prev = traceCalls.length === 0 ? false : traceCalls[traceCalls.length - 1].enabled;
        traceCalls.push({ enabled: Boolean(jsonBody.enabled), flush: Boolean(jsonBody.flush) });
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(
          JSON.stringify({
            enabled: Boolean(jsonBody.enabled),
            previous_enabled: prev,
            trace_dir_rel: "logs/process-trace",
            using_custom_dir: false,
          }),
        );
        return;
      }
      const injectMatch = url.pathname.match(/^\/debug\/v1\/devices\/([^/]+)\/inject$/);
      if (req.method === "POST" && injectMatch) {
        turnSeq += 1;
        const deviceId = decodeURIComponent(injectMatch[1]);
        const turnId = `sess-fake:${turnSeq}`;
        const detail = turnBehavior({ deviceId, turnId, turn: turnSeq, text: jsonBody.text });
        turnStore.set(turnId, detail);
        res.writeHead(202, { "Content-Type": "application/json" });
        res.end(
          JSON.stringify({
            turn_id: turnId,
            device_id: deviceId,
            session_id: "sess-fake",
            turn: turnSeq,
            simulated: true,
          }),
        );
        return;
      }
      const turnMatch = url.pathname.match(/^\/debug\/v1\/turns\/([^/]+)$/);
      if (req.method === "GET" && turnMatch) {
        const turnId = decodeURIComponent(turnMatch[1]);
        const detail = turnStore.get(turnId);
        if (!detail) {
          res.writeHead(404, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ error: "turn_not_found" }));
          return;
        }
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify(detail));
        return;
      }
      res.writeHead(404, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "not_found" }));
    });
  });

  return new Promise((resolvePromise) => {
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      resolvePromise({
        server,
        port: addr.port,
        baseUrl: `http://127.0.0.1:${addr.port}`,
        traceCalls,
        close: () => new Promise((r) => server.close(r)),
      });
    });
  });
}

function localDayString(ms) {
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

test("VA4/VA6 假服务跑通完整联调、产出全部 5 份归档并彻底脱敏（含过程记录开关恢复）", async () => {
  const runtimeDir = mkdtempSync(join(tmpdir(), "moss-va4-pass-"));
  const debugToken = "fake_debug_token_9f8e7d6c5b4a32109f8e7d6c5b4a3210";
  const leakedApiKey = ["sk", "FakeTestSecretKey0123456789ABCDEF"].join("-");
  const leakedDevToken = "device_tok_secret_9876543210";
  const fakeLocalPath = join(runtimeDir, "secret-folder", "demo.gcode");

  const fake = await startFakeDebugServer({
    token: debugToken,
    devices: [
      {
        device_id: "aa:bb:cc:11:22:33",
        session_id: "sess-fake",
        state: "idle",
        level: 0,
        transport: "websocket",
      },
    ],
    turnBehavior: ({ deviceId, turnId, turn, text }) => ({
      turn_id: turnId,
      device_id: deviceId,
      session_id: "sess-fake",
      turn,
      simulated: true,
      status: "done",
      outcome: "done",
      route: "llm",
      injected_text: text,
      corrected_text: text.replace("段木", "椴木"),
      stages_ms: {
        jev_filter: 18,
        first_llm_delta: 210,
        llm_done: 480,
        first_synthesis_ready: 540,
        first_audio_frame: 610,
        turn_end: 1350,
      },
      tools: [
        {
          name: "laser_workflow_tool",
          arguments: JSON.stringify({
            action: "confirm_send",
            token: leakedDevToken,
            path: fakeLocalPath,
          }),
          result: `模拟拒绝 api_key=${leakedApiKey} Bearer ${debugToken}`,
          ms: 2,
          blocked: true,
          block_reason: "模拟轮次禁止发送激光加工任务",
        },
      ],
      reply: `模拟门禁已拒绝发送，路径 ${fakeLocalPath}，令牌 ${leakedApiKey}`,
      spoken: "模拟门禁已拒绝发送。",
      playback: { frames: 24, underruns: 1, stall_ms: 45 },
      device_online: true,
      device_state: "idle",
      device_level: 0,
      started_at_ms: Date.now() - 500,
      finished_at_ms: Date.now(),
    }),
  });

  try {
    const debugDir = join(runtimeDir, "debug");
    mkdirSync(debugDir, { recursive: true });
    writeFileSync(join(debugDir, "voice-debug.token"), `${debugToken}\n`, "utf8");
    writeFileSync(
      join(debugDir, "voice-debug.json"),
      JSON.stringify({ host: "127.0.0.1", port: fake.port, expires_at: Math.floor(Date.now() / 1000) + 3600 }),
      "utf8",
    );

    const corpusFile = join(runtimeDir, "corpus.json");
    writeFileSync(
      corpusFile,
      JSON.stringify({
        name: "va6-smoke",
        turns: [
          {
            id: "blocked_send_and_fix",
            label: "纠偏与门禁验证",
            text: "直接开始雕刻三毫米段木",
            expect: { route: "llm", corrected_includes: ["椴木"], expect_simulation_gate: true },
          },
        ],
      }),
      "utf8",
    );

    // 预写入当天 ProcessTrace（故意混入密钥、令牌与绝对路径，验证截取与脱敏）
    const nowMs = Date.now();
    const dayDir = join(runtimeDir, "logs", "process-trace", localDayString(nowMs));
    mkdirSync(dayDir, { recursive: true });
    const nowIso = new Date(nowMs + 200).toISOString();
    writeFileSync(
      join(dayDir, "timeline.jsonl"),
      `${JSON.stringify({
        seq: 42,
        at: nowIso,
        domain: "voice",
        kind: "tool_call",
        step: `调用工具 api_key=${leakedApiKey} 路径 ${fakeLocalPath}`,
        token: leakedDevToken,
      })}\n`,
      "utf8",
    );
    writeFileSync(
      join(dayDir, "timeline.txt"),
      `[${nowIso}] #42 调用工具 api_key=${leakedApiKey} Bearer ${debugToken} 路径 ${fakeLocalPath}\n`,
      "utf8",
    );

    const { stdout: rawStdout } = await execFileAsync(
      process.execPath,
      [
        SCRIPT_PATH,
        "--runtime-dir",
        runtimeDir,
        "--corpus",
        corpusFile,
        "--poll-interval-ms",
        "25",
      ],
      { encoding: "utf8" },
    );
    const stdout = rawStdout.trim();

    assert.ok(stdout.startsWith(join(runtimeDir, "voice-bench")), `应打印归档目录路径，实际：${stdout}`);
    const expectedFiles = ["turns.jsonl", "timeline.jsonl", "timeline.txt", "report.md", "summary.json"];
    for (const file of expectedFiles) {
      const full = join(stdout, file);
      assert.ok(existsSync(full), `缺少归档文件：${file}`);
      const content = readFileSync(full, "utf8");
      assert.ok(content.trim().length > 0, `归档文件不应为空：${file}`);
      for (const secret of [debugToken, leakedApiKey, leakedDevToken, fakeLocalPath, runtimeDir]) {
        assert.ok(!content.includes(secret), `文件 ${file} 泄露敏感内容 ${secret}`);
      }
    }

    const summary = JSON.parse(readFileSync(join(stdout, "summary.json"), "utf8"));
    assert.equal(summary.counts.total, 1);
    assert.equal(summary.counts.passed, 1);
    assert.equal(summary.counts.failed, 0);
    assert.equal(summary.gate_hits, 1);
    assert.equal(summary.playback.total_underruns, 1);
    assert.equal(summary.playback.total_stall_ms, 45);
    assert.equal(summary.stages_ms.llm_done.p50_ms, 480);
    assert.equal(summary.stages_ms.turn_end.p90_ms, 1350);

    // 验证过程记录先打开、跑完后恢复为原来的 false
    assert.deepEqual(
      fake.traceCalls.map((c) => c.enabled),
      [true, false],
      "跑前开启 ProcessTrace，跑完必须恢复原状态",
    );
  } finally {
    await fake.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VA4/VA6 无在线设备时输出明确中文提示并返回非 0 状态码", async () => {
  const runtimeDir = mkdtempSync(join(tmpdir(), "moss-va4-empty-"));
  const debugToken = "fake_debug_token_empty_devices_1234567890abcdef";
  const fake = await startFakeDebugServer({
    token: debugToken,
    devices: [],
    turnBehavior: () => {
      throw new Error("should not inject");
    },
  });
  try {
    mkdirSync(join(runtimeDir, "debug"), { recursive: true });
    writeFileSync(join(runtimeDir, "debug", "voice-debug.token"), debugToken, "utf8");
    writeFileSync(
      join(runtimeDir, "debug", "voice-debug.json"),
      JSON.stringify({ host: "127.0.0.1", port: fake.port }),
      "utf8",
    );
    let failed = null;
    try {
      await execFileAsync(process.execPath, [SCRIPT_PATH, "--runtime-dir", runtimeDir], {
        encoding: "utf8",
      });
    } catch (error) {
      failed = error;
    }
    assert.ok(failed, "无在线设备时必须返回非 0 退出码");
    assert.notEqual(failed.code, 0);
    assert.ok(
      String(failed.stderr || "").includes(NO_ONLINE_DEVICE_MESSAGE),
      `stderr 应包含唤醒提示，实际：${failed.stderr}`,
    );
  } finally {
    await fake.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VA4/VA6 存在失败轮次时仍完成归档并返回非 0 状态码", async () => {
  const runtimeDir = mkdtempSync(join(tmpdir(), "moss-va4-fail-"));
  const debugToken = "fake_debug_token_fail_turn_1234567890abcdef";
  const fake = await startFakeDebugServer({
    token: debugToken,
    devices: [
      {
        device_id: "aa:bb:cc:44:55:66",
        session_id: "sess-fake",
        state: "idle",
        level: 0,
        transport: "websocket",
      },
    ],
    turnBehavior: ({ deviceId, turnId, turn, text }) => ({
      turn_id: turnId,
      device_id: deviceId,
      session_id: "sess-fake",
      turn,
      simulated: true,
      status: "done",
      outcome: "done",
      route: "llm",
      injected_text: text,
      corrected_text: text,
      stages_ms: { turn_end: 120 },
      tools: [],
      reply: "",
      spoken: "",
      playback: { frames: 0, underruns: 0, stall_ms: 0 },
      error: "大模型服务超时",
      device_online: true,
      device_state: "idle",
      device_level: 0,
    }),
  });
  try {
    mkdirSync(join(runtimeDir, "debug"), { recursive: true });
    writeFileSync(join(runtimeDir, "debug", "voice-debug.token"), debugToken, "utf8");
    const corpusFile = join(runtimeDir, "corpus.json");
    writeFileSync(
      corpusFile,
      JSON.stringify({ name: "fail-case", turns: [{ id: "t1", label: "出错轮", text: "测试" }] }),
      "utf8",
    );
    const res = await runLiveDebugBench({
      runtimeDir,
      url: fake.baseUrl,
      corpus: corpusFile,
      pollIntervalMs: 25,
    });
    assert.equal(res.exitCode, 1, "存在失败轮次必须返回非 0 状态码");
    assert.equal(res.summary.counts.failed, 1);
    assert.equal(res.summary.failed_turns.length, 1);
    assert.ok(existsSync(join(res.archiveDir, "summary.json")));
    assert.ok(existsSync(join(res.archiveDir, "report.md")));
    assert.equal(redactText("Bearer mysecretvalue12345"), "Bearer [REDACTED]");
  } finally {
    await fake.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VA4 工具断言回归：设备工具就绪但被剪空未调用时判失败，常连待机无音频通道在报告给出提示", async () => {
  const spec = {
    id: "device_volume_and_screen",
    expect: {
      route: "llm",
      expect_device_tools_if_ready: ["self.audio_speaker.set_volume", "self.screen.set_brightness"],
    },
  };
  const readyDev = { device_tools_ready: true, device_tools_count: 14 };
  // 未调用设备工具必须判失败（不能再误报通过）
  const errMissing = evaluateTurnExpectations(
    spec,
    { route: "llm", outcome: "done", tools: [], reply: "我无法控制音量与屏幕亮度" },
    { deviceInfo: readyDev },
  );
  assert.ok(errMissing.includes("未调用预期设备工具"), `预期报错，实际：${errMissing}`);

  // 正确调用了音量与亮度（含 AMOLED setbacklight 别名）则判通过
  const errOk = evaluateTurnExpectations(
    spec,
    {
      route: "llm",
      outcome: "done",
      tools: [
        { name: "device__self_audio_speaker_set_volume" },
        { name: "device__self_disp_setbacklight" },
      ],
    },
    { deviceInfo: readyDev },
  );
  assert.equal(errOk, "");

  // 设备未就绪设备工具时不强卡设备工具断言
  const errNotReady = evaluateTurnExpectations(
    spec,
    { route: "llm", outcome: "done", tools: [] },
    { deviceInfo: { device_tools_ready: false, device_tools_count: 0 } },
  );
  assert.equal(errNotReady, "");

  // 无屏小智仅声明音量工具未声明亮度工具时，只调音量应判通过，不误报缺亮度工具
  const errNoScreen = evaluateTurnExpectations(
    spec,
    {
      route: "llm",
      outcome: "done",
      tools: [{ name: "device__self_audio_speaker_set_volume" }],
    },
    {
      deviceInfo: {
        device_tools_ready: true,
        device_tools_count: 2,
        device_tools: ["self.get_device_status", "self.audio_speaker.set_volume"],
      },
    },
  );
  assert.equal(errNoScreen, "");

  // 端到端验证 audio_channel_open === false 时报告显式提示
  const runtimeDir = mkdtempSync(join(tmpdir(), "moss-va4-silent-"));
  const debugToken = "fake_debug_token_silent_mqtt_1234567890abcdef";
  const fake = await startFakeDebugServer({
    token: debugToken,
    devices: [
      {
        device_id: "aa:bb:cc:77:88:99",
        session_id: "sess-fake",
        state: "idle",
        level: 0,
        transport: "mqtt",
        device_tools_ready: true,
        device_tools_count: 14,
      },
    ],
    turnBehavior: ({ deviceId, turnId, turn, text }) => ({
      turn_id: turnId,
      device_id: deviceId,
      session_id: "sess-fake",
      turn,
      simulated: true,
      audio_channel_open: false,
      status: "done",
      outcome: "done",
      route: "llm",
      injected_text: text,
      corrected_text: text,
      stages_ms: { turn_end: 80 },
      tools: [
        { name: "device__self_audio_speaker_set_volume", arguments: '{"volume":50}', ms: 5 },
        { name: "device__self_screen_set_brightness", arguments: '{"brightness":70}', ms: 5 },
      ],
      reply: "已调好音量和亮度。",
      spoken: "已调好音量和亮度。",
      playback: { frames: 12, underruns: 0, stall_ms: 0 },
      device_online: true,
      device_state: "idle",
      device_level: 0,
    }),
  });
  try {
    mkdirSync(join(runtimeDir, "debug"), { recursive: true });
    writeFileSync(join(runtimeDir, "debug", "voice-debug.token"), debugToken, "utf8");
    const corpusFile = join(runtimeDir, "corpus.json");
    writeFileSync(
      corpusFile,
      JSON.stringify({ name: "silent-check", turns: [spec] }),
      "utf8",
    );
    const res = await runLiveDebugBench({
      runtimeDir,
      url: fake.baseUrl,
      corpus: corpusFile,
      pollIntervalMs: 25,
    });
    assert.equal(res.exitCode, 0);
    assert.equal(res.summary.audio_channel_closed_turns, 1);
    const report = readFileSync(join(res.archiveDir, "report.md"), "utf8");
    assert.ok(report.includes("常连待机态"), "report.md 须明确提示待机无声原因");
    assert.ok(!report.includes("请先唤醒小智"), "report.md 绝不能再提示让用户手动唤醒小智");
  } finally {
    await fake.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("n146 文字直通在常连待机（audio_channel_open=false）时先用唤醒词录音自动唤醒，缺录音或无播放器时优雅降级", async () => {
  const makeWav = (filePath) => {
    const numSamples = 8000;
    const dataSize = numSamples * 2;
    const buf = Buffer.alloc(44 + dataSize);
    buf.write("RIFF", 0);
    buf.writeUInt32LE(36 + dataSize, 4);
    buf.write("WAVEfmt ", 8);
    buf.writeUInt32LE(16, 16);
    buf.writeUInt16LE(1, 20);
    buf.writeUInt16LE(1, 22);
    buf.writeUInt32LE(16000, 24);
    buf.writeUInt32LE(32000, 28);
    buf.writeUInt16LE(2, 32);
    buf.writeUInt16LE(16, 34);
    buf.write("data", 36);
    buf.writeUInt32LE(dataSize, 40);
    for (let i = 0; i < numSamples; i += 1) {
      buf.writeInt16LE(Math.round(Math.sin((2 * Math.PI * 440 * i) / 16000) * 10000), 44 + i * 2);
    }
    writeFileSync(filePath, buf);
  };

  // 1. 常连待机 + 有网上唤醒词录音（完整 4 字段）+ 本机能外放 -> 先外放唤醒打开音频通道，再注入文字
  const runtimeWake = mkdtempSync(join(tmpdir(), "moss-n146-live-wake-"));
  const debugToken = "fake_debug_token_n146_live_wake_1234567890abcdef";
  const onlineDir = join(runtimeWake, "voice-bench", "recordings", "online");
  mkdirSync(onlineDir, { recursive: true });
  makeWav(join(onlineDir, "wake_dabai.wav"));
  writeFileSync(
    join(onlineDir, "wake_dabai.json"),
    JSON.stringify({
      text: "你好大白",
      dataset: "AISHELL-1",
      license: "Apache-2.0",
      source_url: "https://example.invalid/wake_dabai",
    }),
    "utf8",
  );

  const liveDevice = {
    device_id: "aa:bb:cc:99:88:77",
    session_id: "sess-mqtt-wake",
    state: "idle",
    level: 0,
    transport: "mqtt",
    audio_channel_open: false,
    wake_word: "你好大白",
  };
  let injectedWhileAudioOpen = false;
  const fakeWake = await startFakeDebugServer({
    token: debugToken,
    devices: [liveDevice],
    turnBehavior: ({ deviceId, turnId, turn, text }) => {
      injectedWhileAudioOpen = liveDevice.audio_channel_open === true;
      return {
        turn_id: turnId,
        device_id: deviceId,
        session_id: liveDevice.session_id,
        turn,
        simulated: true,
        audio_channel_open: liveDevice.audio_channel_open,
        status: "done",
        outcome: "done",
        route: "fast_read",
        injected_text: text,
        corrected_text: text,
        stages_ms: { turn_end: 60 },
        tools: [],
        reply: "激光机当前空闲。",
        spoken: "激光机当前空闲。",
        playback: { frames: 8, underruns: 0, stall_ms: 0 },
        device_online: true,
        device_state: "listening",
        device_level: 0,
      };
    },
  });
  try {
    mkdirSync(join(runtimeWake, "debug"), { recursive: true });
    writeFileSync(join(runtimeWake, "debug", "voice-debug.token"), debugToken, "utf8");
    const corpusFile = join(runtimeWake, "corpus.json");
    writeFileSync(
      corpusFile,
      JSON.stringify({ name: "wake-inject", turns: [{ id: "t1", label: "状态快查", text: "激光机现在什么状态" }] }),
      "utf8",
    );
    const playedWavs = [];
    const res = await runLiveDebugBench({
      runtimeDir: runtimeWake,
      url: fakeWake.baseUrl,
      corpus: corpusFile,
      pollIntervalMs: 20,
      wakeTimeoutMs: 500,
      playWavFn: async (wavPath, { spec }) => {
        playedWavs.push({ wavPath, text: spec.text });
        liveDevice.audio_channel_open = true;
        liveDevice.state = "listening";
        return { started_at_ms: Date.now(), ended_at_ms: Date.now() + 15, duration_ms: 15 };
      },
    });
    assert.equal(res.exitCode, 0);
    assert.equal(playedWavs.length, 1, "常连待机态注入前须先外放唤醒词录音");
    assert.equal(playedWavs[0].text, "你好大白");
    assert.equal(injectedWhileAudioOpen, true, "文字注入必须在音频通道打开后进行");
    assert.equal(res.summary.audio_channel_closed_turns, 0);
    assert.equal(res.summary.wake_word, "你好大白");
    assert.equal(res.summary.wake_summary.succeeded, 1);
    assert.equal(res.summary.wake_events[0].source_type, "online-human");
    assert.equal(res.summary.wake_events[0].recording, "online/wake_dabai.wav");
    const report = readFileSync(join(res.archiveDir, "report.md"), "utf8");
    assert.ok(report.includes("## 自动唤醒记录"), report);
    assert.ok(report.includes("online/wake_dabai.wav"), report);
    assert.ok(!report.includes("请先唤醒小智"), report);
  } finally {
    await fakeWake.close();
    rmSync(runtimeWake, { recursive: true, force: true });
  }

  // 2. 常连待机 + 缺唤醒词录音 或 本机不能外放 -> 降级为仅服务端验证并在报告写明原因
  for (const mode of ["missing_wake", "no_player"]) {
    const runtimeDeg = mkdtempSync(join(tmpdir(), `moss-n146-deg-${mode}-`));
    if (mode === "no_player") {
      const spDir = join(runtimeDeg, "voice-bench", "recordings", "我");
      mkdirSync(spDir, { recursive: true });
      makeWav(join(spDir, "wake_word.wav"));
      writeFileSync(join(spDir, "wake_word.json"), JSON.stringify({ text: "你好小智", speaker: "我" }), "utf8");
    }
    const degDevice = {
      device_id: "aa:bb:cc:99:88:66",
      session_id: "sess-mqtt-deg",
      state: "idle",
      level: 0,
      transport: "mqtt",
      audio_channel_open: false,
      wake_word: "你好小智",
    };
    const fakeDeg = await startFakeDebugServer({
      token: debugToken,
      devices: [degDevice],
      turnBehavior: ({ deviceId, turnId, turn, text }) => ({
        turn_id: turnId,
        device_id: deviceId,
        session_id: degDevice.session_id,
        turn,
        simulated: true,
        audio_channel_open: false,
        status: "done",
        outcome: "done",
        route: "fast_read",
        injected_text: text,
        corrected_text: text,
        stages_ms: { turn_end: 50 },
        tools: [],
        reply: "激光机当前空闲。",
        spoken: "激光机当前空闲。",
        playback: { frames: 6, underruns: 0, stall_ms: 0 },
        device_online: true,
        device_state: "idle",
        device_level: 0,
      }),
    });
    try {
      mkdirSync(join(runtimeDeg, "debug"), { recursive: true });
      writeFileSync(join(runtimeDeg, "debug", "voice-debug.token"), debugToken, "utf8");
      const corpusFile = join(runtimeDeg, "corpus.json");
      writeFileSync(
        corpusFile,
        JSON.stringify({ name: "deg-case", turns: [{ id: "t1", label: "状态快查", text: "激光机现在什么状态" }] }),
        "utf8",
      );
      const res = await runLiveDebugBench({
        runtimeDir: runtimeDeg,
        url: fakeDeg.baseUrl,
        corpus: corpusFile,
        pollIntervalMs: 20,
        disableAudioPlayback: mode === "no_player",
        playWavFn: mode === "no_player" ? undefined : async () => ({ started_at_ms: 0, ended_at_ms: 10, duration_ms: 10 }),
      });
      assert.equal(res.exitCode, 0, `${mode} 降级仅服务端验证仍应通过`);
      assert.equal(res.summary.audio_channel_closed_turns, 1);
      assert.equal(res.summary.wake_summary.failed, 1);
      const report = readFileSync(join(res.archiveDir, "report.md"), "utf8");
      assert.ok(report.includes("已降级为仅服务端验证"), report);
      assert.ok(
        report.includes(mode === "missing_wake" ? "缺唤醒词录音" : "无可用音频外放程序"),
        `报告应写明 ${mode} 降级原因：${report}`,
      );
      assert.ok(!report.includes("请先唤醒小智"), "降级说明绝不能要求用户手动唤醒");
    } finally {
      await fakeDeg.close();
      rmSync(runtimeDeg, { recursive: true, force: true });
    }
  }
});
