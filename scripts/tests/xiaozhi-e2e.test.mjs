import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { encodeWavPcm16, readWavPcm16, runAsrBench } from "../voice-bench/asr-bench.mjs";
import { parseE2eArgs, runXiaozhiE2e } from "../voice-bench/xiaozhi-e2e.mjs";

const NOISE_MIX_PY = fileURLToPath(new URL("../voice-bench/asr_noise_mix.py", import.meta.url));

function makeSyntheticWav(filePath, durationSec = 0.6, freqHz = 440) {
  const rate = 16_000;
  const count = Math.floor(rate * durationSec);
  const pcm = Buffer.alloc(count * 2);
  for (let i = 0; i < count; i += 1) {
    pcm.writeInt16LE(Math.round(Math.sin((2 * Math.PI * freqHz * i) / rate) * 7000), i * 2);
  }
  writeFileSync(filePath, encodeWavPcm16(pcm, rate));
}

function writeHumanRecording(runtimeDir, speaker, corpusId, text, freqHz = 440) {
  const dir = join(runtimeDir, "voice-bench", "recordings", speaker);
  mkdirSync(dir, { recursive: true });
  makeSyntheticWav(join(dir, `${corpusId}.wav`), 0.6, freqHz);
  writeFileSync(
    join(dir, `${corpusId}.json`),
    JSON.stringify({
      corpus_id: corpusId,
      text,
      speaker,
      speaker_dir: speaker,
      recorded_at_ms: Date.now(),
      duration_ms: 600,
      sample_rate: 16_000,
      peak_level: 42,
      rms_level: 16,
    }),
    "utf8",
  );
}

function startFakeE2eServer({
  token,
  runtimeDir,
  deviceId = "aa:bb:cc:dd:ee:01",
  sessionId = "sess-e2e-1",
  onAcousticTurn,
  onDigitalTurn = null,
}) {
  let currentSessionId = sessionId;
  const benchActions = [];
  const traceCalls = [];
  let traceEnabled = false;
  let benchWindowOpen = false;
  let deviceState = "listening";
  let deviceLevel = 0;
  let recentPeak = 0;
  let latestTurn = 0;
  const turns = new Map();
  let overlapObserved = false;

  const server = createServer(async (req, res) => {
    const auth = req.headers.authorization || "";
    if (auth !== `Bearer ${token}`) {
      res.writeHead(401, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: "unauthorized" }));
      return;
    }
    const url = new URL(req.url, "http://127.0.0.1");
    let body = {};
    if (req.method === "POST") {
      const chunks = [];
      for await (const c of req) chunks.push(c);
      const raw = Buffer.concat(chunks).toString("utf8");
      body = raw ? JSON.parse(raw) : {};
    }

    if (req.method === "GET" && url.pathname === "/debug/v1/devices") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          mode: "chat",
          active_laser_job: false,
          bench_windows: benchWindowOpen ? [{ device_id: deviceId, session_id: currentSessionId, active: true }] : [],
          devices: [
            {
              device_id: deviceId,
              session_id: currentSessionId,
              state: deviceState,
              level: deviceLevel,
              recent_peak_level: recentPeak,
              latest_turn: latestTurn,
              latest_turn_id: latestTurn > 0 ? `${currentSessionId}:${latestTurn}` : null,
              bench_window: benchWindowOpen
                ? { device_id: deviceId, session_id: currentSessionId, active: true, max_minutes: 30 }
                : null,
            },
          ],
        }),
      );
      return;
    }

    if (req.method === "POST" && url.pathname === "/debug/v1/bench-window") {
      benchActions.push({ ...body });
      if (body.action === "open") {
        benchWindowOpen = true;
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(
          JSON.stringify({
            device_id: deviceId,
            action: "open",
            bench_window: {
              device_id: deviceId,
              session_id: currentSessionId,
              active: true,
              max_minutes: body.max_minutes ?? 30,
            },
          }),
        );
        return;
      }
      if (body.action === "close") {
        benchWindowOpen = false;
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ device_id: deviceId, action: "close", closed: true, bench_window: null }));
        return;
      }
    }

    if (req.method === "POST" && url.pathname === "/debug/v1/process-trace") {
      const prev = traceEnabled;
      traceEnabled = Boolean(body.enabled);
      traceCalls.push({ enabled: traceEnabled, flush: Boolean(body.flush) });
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          enabled: traceEnabled,
          previous_enabled: prev,
          trace_dir_rel: "logs/process-trace",
          using_custom_dir: false,
        }),
      );
      return;
    }

    if (req.method === "GET" && url.pathname === `/debug/v1/devices/${encodeURIComponent(deviceId)}/turns`) {
      const list = [...turns.values()].sort((a, b) => b.turn - a.turn);
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ device_id: deviceId, turns: list }));
      return;
    }

    if (req.method === "GET" && url.pathname.startsWith("/debug/v1/turns/")) {
      const turnId = decodeURIComponent(url.pathname.slice("/debug/v1/turns/".length));
      const detail = turns.get(turnId);
      if (!detail) {
        res.writeHead(404, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: "turn_not_found" }));
        return;
      }
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          ...detail,
          device_online: true,
          device_state: deviceState,
          device_level: deviceLevel,
        }),
      );
      return;
    }

    if (req.method === "POST" && url.pathname === `/debug/v1/devices/${encodeURIComponent(deviceId)}/inject-audio`) {
      if (!benchWindowOpen) {
        res.writeHead(403, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: "bench_window_required" }));
        return;
      }
      latestTurn += 1;
      const turnNo = latestTurn;
      const turnId = `${sessionId}:${turnNo}`;
      const now = Date.now();
      const custom = onDigitalTurn ? onDigitalTurn({ turnNo, body, now }) : {};
      turns.set(turnId, {
        turn_id: turnId,
        device_id: deviceId,
        session_id: sessionId,
        turn: turnNo,
        simulated: true,
        bench_window: true,
        digital_audio: true,
        status: "done",
        outcome: "done",
        route: custom.route || "llm",
        injected_text: custom.raw_asr || "三毫米椴木切割功率",
        corrected_text: custom.corrected_asr || custom.raw_asr || "三毫米椴木切割功率",
        stages_ms: { utterance_end: 300, final_asr: 420, first_audio_frame: 650, turn_end: 900 },
        tools: [],
        reply: "数字对照完成。",
        spoken: "数字对照完成。",
        playback: { frames: 5, underruns: 0, stall_ms: 0 },
        started_at_ms: now,
        finished_at_ms: now + 60,
        speech_began_at_ms: now - 300,
        speech_end_at_ms: now,
        first_audio_frame_at_ms: now + 30,
        peak_uplink_level: 48,
        corpus_date: null,
        corpus_turn_id: null,
      });
      res.writeHead(202, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ turn_id: turnId, device_id: deviceId, session_id: sessionId, turn: turnNo, simulated: true }));
      return;
    }

    res.writeHead(404, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "not_found" }));
  });

  // 供假播放器调用：触发一次外放后服务端按 thinking -> speaking -> listening 推进状态并写入模拟 corpus 录音
  function simulateAcousticPlayback({ spec, playbackStartedAtMs, playbackEndedAtMs }) {
    if (deviceState === "speaking" || deviceState === "thinking") {
      overlapObserved = true;
    }
    latestTurn += 1;
    const turnNo = latestTurn;
    const turnId = `${sessionId}:${turnNo}`;
    const custom = onAcousticTurn({ spec, turnNo, playbackStartedAtMs, playbackEndedAtMs, runtimeDir }) || {};
    recentPeak = custom.peak_uplink_level ?? 52;
    deviceState = "thinking";
    deviceLevel = 0;

    let corpusDate = null;
    let corpusTurnId = null;
    if (custom.writeCorpus) {
      corpusDate = "2026-11-27";
      corpusTurnId = `${sessionId}-t${turnNo}`;
      const dayDir = join(runtimeDir, "asr-corpus", corpusDate);
      mkdirSync(dayDir, { recursive: true });
      makeSyntheticWav(join(dayDir, `${corpusTurnId}.wav`), 0.2, 520);
      writeFileSync(
        join(dayDir, `${corpusTurnId}.json`),
        JSON.stringify({
          turn_id: corpusTurnId,
          date: corpusDate,
          captured_at_ms: playbackEndedAtMs + 50,
          device_id: deviceId,
          session_id: sessionId,
          turn: turnNo,
          simulated: false,
          bench_window: true,
          sample_rate: 16000,
          duration_ms: 200,
          asr_model: "fake-asr",
          vad_engine: "fake-vad",
          raw_text: custom.raw_asr ?? spec.text,
          corrected_text: custom.corrected_asr ?? custom.raw_asr ?? spec.text,
          outcome: "done",
        }),
        "utf8",
      );
    }

    turns.set(turnId, {
      turn_id: turnId,
      device_id: deviceId,
      session_id: sessionId,
      turn: turnNo,
      simulated: true,
      bench_window: benchWindowOpen,
      simulated_label: benchWindowOpen ? "模拟（整体测试）" : null,
      digital_audio: false,
      status: "running",
      outcome: "running",
      route: "running",
      injected_text: custom.raw_asr ?? spec.text,
      corrected_text: custom.corrected_asr ?? custom.raw_asr ?? spec.text,
      stages_ms: {},
      tools: [],
      reply: "",
      spoken: "",
      playback: { frames: 0, underruns: 0, stall_ms: 0 },
      started_at_ms: playbackEndedAtMs + 20,
      finished_at_ms: null,
      speech_began_at_ms: custom.speech_began_at_ms ?? playbackStartedAtMs + 20,
      speech_end_at_ms: custom.speech_end_at_ms ?? playbackEndedAtMs + 60,
      first_audio_frame_at_ms: null,
      peak_uplink_level: custom.peak_uplink_level ?? 52,
      corpus_date: null,
      corpus_turn_id: null,
    });

    setTimeout(() => {
      deviceState = "speaking";
      deviceLevel = 65;
    }, 15);

    setTimeout(() => {
      const doneAt = Date.now();
      turns.set(turnId, {
        ...turns.get(turnId),
        status: "done",
        outcome: "done",
        route: custom.route || "llm",
        injected_text: custom.raw_asr ?? spec.text,
        corrected_text: custom.corrected_asr ?? custom.raw_asr ?? spec.text,
        stages_ms: {
          utterance_end: 300,
          final_asr: 420,
          first_synthesis_ready: 520,
          first_audio_frame: 600,
          turn_end: 820,
        },
        tools: custom.tools || [],
        reply: custom.reply || "好的，已完成。",
        spoken: custom.spoken || custom.reply || "好的，已完成。",
        playback: custom.playback || { frames: 6, underruns: 0, stall_ms: 0 },
        finished_at_ms: doneAt,
        first_audio_frame_at_ms: custom.first_audio_frame_at_ms ?? playbackEndedAtMs + 120,
        corpus_date: corpusDate,
        corpus_turn_id: corpusTurnId,
      });
      deviceState = "listening";
      deviceLevel = 0;
    }, 35);
  }

  return new Promise((resolvePromise) => {
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolvePromise({
        url: `http://127.0.0.1:${port}`,
        benchActions,
        traceCalls,
        get overlapObserved() {
          return overlapObserved;
        },
        setSessionId(nextId) {
          currentSessionId = nextId;
        },
        simulateAcousticPlayback,
        close: () => new Promise((done) => server.close(done)),
      });
    });
  });
}

function setupRuntimeWithToken(prefix, token) {
  const runtimeDir = mkdtempSync(join(tmpdir(), prefix));
  const debugDir = join(runtimeDir, "debug");
  mkdirSync(debugDir, { recursive: true });
  writeFileSync(join(debugDir, "voice-debug.token"), `${token}\n`, "utf8");
  const traceDayDir = join(runtimeDir, "logs", "process-trace", "2026-11-27");
  mkdirSync(traceDayDir, { recursive: true });
  return runtimeDir;
}

test("VC3/VD1/VD3/VD5 假服务 + 真人录音跑通整体测试：多说话人分组、缺录音跳过、数字对照、脱敏归档与自动标注回采", async () => {
  const token = "vc5-e2e-token-0123456789abcdef0123456789abcdef";
  const fakeSecret = ["sk", "FakeE2eSecretKey0123456789ABCDEF"].join("-");
  const runtimeDir = setupRuntimeWithToken("moss-vc5-pass-", token);
  const corpusPath = join(runtimeDir, "mini-e2e-corpus.json");

  // 写入两位说话人（“我”与“家人 A”）的真人录音到 <runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}
  // 故意不为 missing_sentence 录制，验证缺录音时跳过且在报告列出
  writeHumanRecording(runtimeDir, "我", "status", "激光机现在什么状态", 420);
  writeHumanRecording(runtimeDir, "家人 A", "status", "激光机现在什么状态", 520);
  writeHumanRecording(runtimeDir, "我", "wood", "三毫米椴木功率多少", 440);
  writeHumanRecording(runtimeDir, "家人 A", "wood", "三毫米椴木功率多少", 540);
  writeHumanRecording(runtimeDir, "我", "gate", "直接开始雕刻吧", 460);
  writeHumanRecording(runtimeDir, "家人 A", "gate", "直接开始雕刻吧", 560);

  // 写入带本机绝对路径与伪密钥的 ProcessTrace 片段以验证脱敏
  const traceDayDir = join(runtimeDir, "logs", "process-trace", new Date().toISOString().slice(0, 10));
  mkdirSync(traceDayDir, { recursive: true });
  const nowIso = new Date().toISOString();
  writeFileSync(
    join(traceDayDir, "timeline.jsonl"),
    `${JSON.stringify({ seq: 1, at: nowIso, step: `e2e api_key=${fakeSecret} ${runtimeDir}` })}\n`,
    "utf8",
  );
  writeFileSync(
    join(traceDayDir, "timeline.txt"),
    `[${nowIso}] #1 e2e Bearer ${token} ${runtimeDir}\n`,
    "utf8",
  );

  writeFileSync(
    corpusPath,
    JSON.stringify({
      name: "mini-e2e-vd5",
      turns: [
        { id: "status", label: "状态快查", text: "激光机现在什么状态", expect_route: "fast_read" },
        { id: "wood", label: "椴木参数", text: "三毫米椴木功率多少", key_terms: ["椴木"], tags: ["材料名", "数字"] },
        { id: "gate", label: "门禁验证", text: "直接开始雕刻吧", expect_simulation_gate: true },
        { id: "missing_sentence", label: "未录制语料", text: "这句没有真人录音" },
      ],
    }),
    "utf8",
  );

  const playEvents = [];
  const fakeServer = await startFakeE2eServer({
    token,
    runtimeDir,
    onAcousticTurn: ({ spec }) => {
      if (spec.id === "calibration") {
        return { raw_asr: "激光机现在什么状态", route: "fast_read", peak_uplink_level: 55 };
      }
      if (spec.id === "status") {
        return { raw_asr: "激光机现在什么状态", route: "fast_read", writeCorpus: true };
      }
      if (spec.id === "wood") {
        return {
          // 模拟外放略有错字（段木→椴木纠偏），并掺入伪密钥和绝对路径以验证全量脱敏
          raw_asr: "三毫米段木功率多少",
          corrected_asr: "三毫米椴木功率多少",
          reply: `推荐功率 60%（调试信息 ${fakeSecret} ${runtimeDir}）`,
          route: "llm",
          writeCorpus: true,
        };
      }
      return {
        raw_asr: "直接开始雕刻吧",
        route: "llm",
        tools: [
          {
            name: "laser_workflow_tool",
            arguments: '{"action":"confirm_send"}',
            result: '{"error":"simulated_turn_blocked"}',
            ms: 2,
            blocked: true,
            block_reason: "模拟轮次禁止发送激光加工任务",
          },
        ],
        reply: "测试时间窗已锁定物理动作，已拒绝直接雕刻。",
        writeCorpus: true,
      };
    },
    onDigitalTurn: ({ turnNo }) => {
      if (turnNo === 3) return { raw_asr: "激光机现在什么状态" };
      if (turnNo === 5) return { raw_asr: "三毫米椴木功率多少" };
      return { raw_asr: "直接开始雕刻吧" };
    },
  });

  try {
    const res = await runXiaozhiE2e({
      runtimeDir,
      url: fakeServer.url,
      corpus: corpusPath,
      quietMs: 30,
      pollIntervalMs: 15,
      turnTimeoutMs: 5_000,
      compareDigital: true,
      logMissing: false,
      playWavFn: async (_wavPath, { spec }) => {
        const started = Date.now();
        await new Promise((r) => setTimeout(r, 25));
        const ended = Date.now();
        playEvents.push({ id: spec.id, started, ended });
        fakeServer.simulateAcousticPlayback({
          spec,
          playbackStartedAtMs: started,
          playbackEndedAtMs: ended,
        });
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: ended - started };
      },
    });

    assert.equal(res.exitCode, 0, `预期整体测试通过：${res.errorMessage || JSON.stringify(res.summary?.failed_turns)}`);
    assert.equal(fakeServer.overlapObserved, false, "严格串行状态机不得在设备思考/播报期间重叠外放");
    assert.equal(playEvents.length, 4, "应顺序播放 1 句校准 + 3 句有真人录音的语料（缺失句跳过）");

    // 验证 bench_window 开、关成对执行，且 ProcessTrace 恢复原状态
    assert.deepEqual(
      fakeServer.benchActions.map((a) => a.action),
      ["open", "close"],
      "必须先打开测试时间窗并在 finally 中关闭",
    );
    assert.equal(fakeServer.traceCalls.at(-1).enabled, false, "结束必须恢复 process_trace 原关闭状态");

    // 校验 6 份归档文件齐全且零密钥/零绝对路径泄露
    const expectedFiles = [
      "turns.jsonl",
      "timeline.jsonl",
      "timeline.txt",
      "report.md",
      "summary.json",
      "audio-manifest.json",
    ];
    for (const name of expectedFiles) {
      const full = join(res.archiveDir, name);
      assert.ok(existsSync(full), `缺少归档文件：${name}`);
      const text = readFileSync(full, "utf8");
      assert.ok(!text.includes(token), `${name} 绝不能泄露调试令牌`);
      assert.ok(!text.includes(fakeSecret), `${name} 绝不能泄露 API 密钥`);
      assert.ok(!text.includes(runtimeDir), `${name} 绝不能泄露本机绝对路径`);
    }

    const summary = JSON.parse(readFileSync(join(res.archiveDir, "summary.json"), "utf8"));
    assert.equal(summary.counts.passed, 3);
    assert.equal(summary.counts.missing_recordings, 1);
    assert.equal(summary.missing_turns[0].id, "missing_sentence");
    assert.equal(summary.gate_hits, 1);
    assert.equal(summary.auto_labeled_corpus_count, 3);
    assert.ok(summary.digital_comparison && summary.digital_comparison.compared_turns === 3);
    assert.ok(summary.digital_comparison.mic_room_cer_loss > 0, "外放比数字对照多 1 字错应计为正损耗");

    // VD3：按说话人分组统计包含两位说话人
    assert.equal(summary.by_speaker.length, 2, "轮流使用两位说话人应产出两组说话人统计");
    const speakerNames = summary.by_speaker.map((s) => s.speaker).sort();
    assert.deepEqual(speakerNames, ["家人 A", "我"]);
    assert.equal(
      summary.by_speaker.reduce((sum, s) => sum + s.executed, 0),
      3,
    );

    const reportMd = readFileSync(join(res.archiveDir, "report.md"), "utf8");
    assert.ok(reportMd.includes("按说话人分组统计"));
    assert.ok(reportMd.includes("缺少真人录音的语料（已跳过）"));
    assert.ok(reportMd.includes("missing_sentence"));
    assert.ok(reportMd.includes("录制测试语料"));
    assert.ok(reportMd.includes("建议优先调整的环节"));
    assert.ok(reportMd.includes("外放识别 vs 服务端数字注入对照"));

    // VD3：验证 --speaker 只使用指定说话人
    const singleSpeakerRes = await runXiaozhiE2e({
      runtimeDir,
      url: fakeServer.url,
      corpus: corpusPath,
      speaker: "家人 A",
      quietMs: 20,
      pollIntervalMs: 15,
      turnTimeoutMs: 5_000,
      skipCalibration: true,
      logMissing: false,
      playWavFn: async (_wavPath, { spec }) => {
        const started = Date.now();
        await new Promise((r) => setTimeout(r, 25));
        const ended = Date.now();
        fakeServer.simulateAcousticPlayback({ spec, playbackStartedAtMs: started, playbackEndedAtMs: ended });
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: ended - started };
      },
    });
    assert.equal(singleSpeakerRes.exitCode, 0);
    assert.equal(singleSpeakerRes.summary.by_speaker.length, 1);
    assert.equal(singleSpeakerRes.summary.by_speaker[0].speaker, "家人 A");

    // VC3 #7 + VD3：验证自动写入了带 speaker 的 e2e-playback 标注，并且 asr-bench.mjs 默认与 manual 分组统计
    const manualDir = join(runtimeDir, "asr-corpus", "2026-11-26");
    mkdirSync(manualDir, { recursive: true });
    makeSyntheticWav(join(manualDir, "manual-t1.wav"), 0.6, 480);
    writeFileSync(
      join(manualDir, "manual-t1.json"),
      JSON.stringify({ turn_id: "manual-t1", date: "2026-11-26", simulated: false, raw_text: "三毫米椴木" }),
      "utf8",
    );
    writeFileSync(
      join(manualDir, "manual-t1.label.json"),
      JSON.stringify({ turn_id: "manual-t1", date: "2026-11-26", reference_text: "三毫米椴木", verified_correct: true, tags: ["材料名"] }),
      "utf8",
    );

    const benchRes = await runAsrBench({
      runtimeDir,
      noiseLevels: [0],
      recognizers: [
        {
          id: "fake_asr",
          label: "假识别器",
          async transcribe(_wav, { sample }) {
            return { text: sample.reference_text, firstInterimMs: 50, finalMs: 120 };
          },
        },
      ],
    });
    assert.equal(benchRes.exitCode, 0);
    assert.equal(benchRes.analysis.byLabelSource.manual.sample_count, 1);
    assert.ok(benchRes.analysis.byLabelSource["e2e-playback"].sample_count >= 3);
    const asrReport = readFileSync(join(benchRes.archiveDir, "report.md"), "utf8");
    assert.ok(asrReport.includes("按语料来源分组统计"));
    assert.ok(asrReport.includes("整体测试外放录音（e2e-playback）"));
    assert.ok(asrReport.includes("人工标注真实录音（manual）"));
  } finally {
    await fakeServer.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VD1/VD3/VD5 全部语料缺少真人录音时以非 0 退出且不开时间窗；默认忽略设备录音，显式 --include-device-corpus 标明二次录制", async () => {
  const token = "vd1-missing-token-0123456789abcdef0123456789abcd";
  const runtimeDir = setupRuntimeWithToken("moss-vd1-missing-", token);
  const corpusPath = join(runtimeDir, "empty-rec-corpus.json");
  writeFileSync(
    corpusPath,
    JSON.stringify({
      name: "missing-all",
      turns: [{ id: "only_one", label: "状态", text: "激光机现在什么状态" }],
    }),
    "utf8",
  );

  // 即使 asr-corpus 里有一条手工标注的小智录音，默认也不应作为外放素材
  const corpusDay = join(runtimeDir, "asr-corpus", "2026-11-27");
  mkdirSync(corpusDay, { recursive: true });
  makeSyntheticWav(join(corpusDay, "dev-t1.wav"), 0.6, 440);
  writeFileSync(
    join(corpusDay, "dev-t1.json"),
    JSON.stringify({ turn_id: "dev-t1", date: "2026-11-27", simulated: false, raw_text: "激光机现在什么状态" }),
    "utf8",
  );
  writeFileSync(
    join(corpusDay, "dev-t1.label.json"),
    JSON.stringify({ turn_id: "dev-t1", date: "2026-11-27", reference_text: "激光机现在什么状态", verified_correct: true, tags: [] }),
    "utf8",
  );

  const fakeServer = await startFakeE2eServer({
    token,
    runtimeDir,
    onAcousticTurn: () => ({ raw_asr: "激光机现在什么状态", peak_uplink_level: 50 }),
  });

  try {
    // 1. 参数校验：已移除 --voice-source 合成开关
    assert.throws(() => parseE2eArgs(["--voice-source", "local-tts"]), /未知参数/);
    const parsed = parseE2eArgs(["--speaker", "我", "--include-device-corpus"]);
    assert.equal(parsed.speaker, "我");
    assert.equal(parsed.includeDeviceCorpus, true);

    // 2. 默认情况无 recordings 且不读 asr-corpus -> 非 0 退出、不开测试窗、提示去 App 录制
    const missRes = await runXiaozhiE2e({
      runtimeDir,
      url: fakeServer.url,
      corpus: corpusPath,
      quietMs: 20,
      pollIntervalMs: 15,
      playWavFn: async () => {
        throw new Error("无录音时绝不能播放");
      },
    });
    assert.equal(missRes.exitCode, 1);
    assert.match(missRes.errorMessage || "", /录制测试语料/);
    assert.match(missRes.errorMessage || "", /only_one/);
    assert.deepEqual(fakeServer.benchActions, [], "全部缺少录音时不应打开测试时间窗");

    // 3. 显式 --include-device-corpus 允许补充设备录音，报告与汇总必须明确标为“二次录制”
    const secRes = await runXiaozhiE2e({
      runtimeDir,
      url: fakeServer.url,
      corpus: corpusPath,
      includeDeviceCorpus: true,
      skipCalibration: true,
      quietMs: 20,
      pollIntervalMs: 15,
      playWavFn: async (_wavPath, { spec }) => {
        const started = Date.now();
        await new Promise((r) => setTimeout(r, 25));
        const ended = Date.now();
        fakeServer.simulateAcousticPlayback({ spec, playbackStartedAtMs: started, playbackEndedAtMs: ended });
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: ended - started };
      },
    });
    assert.equal(secRes.exitCode, 0);
    assert.equal(secRes.summary.include_device_corpus, true);
    assert.equal(secRes.summary.by_speaker[0].secondary_recording, true);
    const reportMd = readFileSync(join(secRes.archiveDir, "report.md"), "utf8");
    assert.ok(reportMd.includes("二次录制"), "显式使用设备录音时报告必须标明二次录制");
  } finally {
    await fakeServer.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VC5 抢话判定（外放未结束即判停）与异常退出时 finally 必关时间窗", async () => {
  const token = "vc5-cutoff-token-0123456789abcdef0123456789abcd";
  const runtimeDir = setupRuntimeWithToken("moss-vc5-cutoff-", token);
  const corpusPath = join(runtimeDir, "cutoff-corpus.json");
  writeHumanRecording(runtimeDir, "我", "pause_sentence", "帮我生成一个铭牌预览尺寸八十乘四十", 440);
  writeFileSync(
    corpusPath,
    JSON.stringify({
      name: "cutoff-test",
      turns: [{ id: "pause_sentence", label: "700ms 停顿长句", text: "帮我生成一个铭牌预览尺寸八十乘四十" }],
    }),
    "utf8",
  );

  const fakeServer = await startFakeE2eServer({
    token,
    runtimeDir,
    onAcousticTurn: ({ spec, playbackStartedAtMs, playbackEndedAtMs }) => {
      if (spec.id === "calibration") {
        return { raw_asr: "帮我生成一个铭牌预览尺寸八十乘四十", peak_uplink_level: 50 };
      }
      // 模拟外放还没结束，服务端就已经提前 400 ms 判停（抢话）且吞掉首字“帮”
      return {
        raw_asr: "我生成一个铭牌预览",
        speech_began_at_ms: playbackStartedAtMs + 10,
        speech_end_at_ms: playbackEndedAtMs - 400,
        peak_uplink_level: 45,
      };
    },
  });

  try {
    const res = await runXiaozhiE2e({
      runtimeDir,
      url: fakeServer.url,
      corpus: corpusPath,
      quietMs: 25,
      pollIntervalMs: 15,
      turnTimeoutMs: 5_000,
      playWavFn: async (_wavPath, { spec }) => {
        const started = Date.now();
        const ended = started + 800;
        fakeServer.simulateAcousticPlayback({ spec, playbackStartedAtMs: started, playbackEndedAtMs: ended });
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: 800 };
      },
    });

    assert.equal(res.exitCode, 1, "出现抢话/吞字时应返回非 0 退出码");
    const t0 = res.turns[0];
    assert.equal(t0.attribution.capture_endpoint.early_cutoff, true, "外放未结束即判停必须判为抢话");
    assert.equal(t0.attribution.capture_endpoint.head_clipped, true, "缺失开头首字必须判为首字被吞");
    assert.ok(res.summary.recommendations.some((r) => r.action.includes("BASE_SILENCE_MS")));
    assert.ok(res.summary.recommendations.some((r) => r.action.includes("PREROLL_MS")));
    assert.deepEqual(fakeServer.benchActions.map((a) => a.action), ["open", "close"]);

    // 额外验证：播放器抛异常时 finally 仍必须关闭测试时间窗
    fakeServer.benchActions.length = 0;
    await assert.rejects(
      () =>
        runXiaozhiE2e({
          runtimeDir,
          url: fakeServer.url,
          corpus: corpusPath,
          quietMs: 20,
          pollIntervalMs: 15,
          skipCalibration: true,
          playWavFn: async () => {
            throw new Error("模拟外放设备异常崩溃");
          },
        }),
      /模拟外放设备异常崩溃/,
    );
    assert.deepEqual(
      fakeServer.benchActions.map((a) => a.action),
      ["open", "close"],
      "异常退出时 finally 必须关闭测试时间窗",
    );
  } finally {
    await fakeServer.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VC5 校准失败（电平过低/未识别）打印 30–50 cm 摆放指引、返回非 0 状态码且关闭时间窗", async () => {
  const token = "vc5-calfail-token-0123456789abcdef0123456789abcd";
  const runtimeDir = setupRuntimeWithToken("moss-vc5-calfail-", token);
  const corpusPath = join(runtimeDir, "cal-corpus.json");
  writeHumanRecording(runtimeDir, "我", "t1", "激光机状态", 440);
  writeFileSync(
    corpusPath,
    JSON.stringify({ name: "cal-fail", turns: [{ id: "t1", label: "状态", text: "激光机状态" }] }),
    "utf8",
  );

  const fakeServer = await startFakeE2eServer({
    token,
    runtimeDir,
    onAcousticTurn: () => ({
      raw_asr: "激光机状态",
      peak_uplink_level: 5, // 明显过低
    }),
  });

  try {
    const res = await runXiaozhiE2e({
      runtimeDir,
      url: fakeServer.url,
      corpus: corpusPath,
      quietMs: 20,
      pollIntervalMs: 15,
      playWavFn: async (_wavPath, { spec }) => {
        const started = Date.now();
        const ended = started + 100;
        fakeServer.simulateAcousticPlayback({ spec, playbackStartedAtMs: started, playbackEndedAtMs: ended });
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: 100 };
      },
    });

    assert.equal(res.exitCode, 1);
    assert.match(res.errorMessage || "", /30–50 cm/);
    assert.deepEqual(
      fakeServer.benchActions.map((a) => a.action),
      ["open", "close"],
      "校准失败退出前也必须关闭测试时间窗",
    );
  } finally {
    await fakeServer.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VC2 背景噪声生成脚本（python3 -I asr_noise_mix.py --generate-noise fan/pump）产出有效 16 kHz WAV", () => {
  const dir = mkdtempSync(join(tmpdir(), "moss-vc2-noise-"));
  try {
    for (const preset of ["fan", "pump"]) {
      const outWav = join(dir, `${preset}.wav`);
      execFileSync(
        "python3",
        ["-I", NOISE_MIX_PY, "--generate-noise", preset, "--duration-secs", "1", "--gain", "0.25", "--output", outWav],
        { stdio: ["ignore", "pipe", "pipe"] },
      );
      assert.ok(existsSync(outWav));
      const { pcm, sampleRate } = readWavPcm16(outWav);
      assert.equal(sampleRate, 16_000);
      assert.equal(pcm.length, 16_000 * 2);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("审查回归：时间窗中途关闭立即中止不外放、服务端时间戳检出重叠、自动标注不覆盖人工标注、Windows 路径全脱敏", async () => {
  const { redactText, redactValue } = await import("../voice-bench/bench-common.mjs");
  const { buildTurnAttribution } = await import("../voice-bench/xiaozhi-e2e.mjs");

  // 1. Windows 双反斜杠/正斜杠路径、转义引号凭据与驼峰/连字符键全脱敏
  const rawWin = String.raw`{"path":"C:\\Users\\alice\\AppData\\Roaming\\moss\\a.gcode","alt":"C:/Users/alice/test.wav","log":"{\"token\":\"supersecret123456\"}"}`;
  const redWin = redactText(rawWin, { paths: ["C:\\Users\\alice\\AppData\\Roaming\\moss"] });
  assert.ok(!redWin.includes("alice"), `Windows 用户名不得残留：${redWin}`);
  assert.ok(!redWin.includes("supersecret123456"), `转义引号令牌不得残留：${redWin}`);
  const redObj = redactValue({ apiKey: "k12345678", accessToken: "t12345678", "x-api-key": "x12345678", token_file: "debug/voice-debug.token" });
  assert.equal(redObj.apiKey, "[REDACTED]");
  assert.equal(redObj.accessToken, "[REDACTED]");
  assert.equal(redObj["x-api-key"], "[REDACTED]");
  assert.equal(redObj.token_file, "debug/voice-debug.token", "非密钥的相对路径字段不应被误杀");

  // 2. 服务端首帧早于外放结束必须检出 overlap_detected = true
  const attr = buildTurnAttribution({
    spec: { text: "帮我做一个木牌预览" },
    playback: { started_at_ms: 1000, ended_at_ms: 3000, duration_ms: 2000 },
    detail: {
      injected_text: "帮我做一个木牌预览",
      corrected_text: "帮我做一个木牌预览",
      first_audio_frame_at_ms: 2500,
      route: "llm",
      outcome: "ok",
      reply: "好的",
      playback: { frames: 10, underruns: 0, stall_ms: 0 },
    },
    overlapDetected: false,
  });
  assert.equal(attr.capture_endpoint.overlap_detected, true);
  assert.equal(attr.capture_endpoint.passed, false);

  // 3. 中途测试时间窗关闭时，第二句绝不得调用 playWav
  const token = "reg-toctou-token-0123456789abcdef0123456789abcd";
  const runtimeDir = setupRuntimeWithToken("moss-reg-toctou-", token);
  const corpusPath = join(runtimeDir, "two-turns.json");
  writeHumanRecording(runtimeDir, "我", "t1", "激光机状态", 440);
  writeHumanRecording(runtimeDir, "我", "t2", "直接开始雕刻吧", 520);
  writeFileSync(
    corpusPath,
    JSON.stringify({
      name: "toctou-check",
      turns: [
        { id: "t1", label: "第1句", text: "激光机状态" },
        { id: "t2", label: "第2句高危", text: "直接开始雕刻吧", expect_simulation_gate: true },
      ],
    }),
    "utf8",
  );
  // 预先在 asr-corpus 放置已有人工标注，验证自动标注不会覆盖它
  const corpusDay = join(runtimeDir, "asr-corpus", "2026-11-27");
  mkdirSync(corpusDay, { recursive: true });
  makeSyntheticWav(join(corpusDay, "manual-turn-1.wav"), 0.5, 440);
  writeFileSync(join(corpusDay, "manual-turn-1.json"), JSON.stringify({ turn_id: "manual-turn-1", date: "2026-11-27" }), "utf8");
  writeFileSync(
    join(corpusDay, "manual-turn-1.label.json"),
    JSON.stringify({ turn_id: "manual-turn-1", date: "2026-11-27", reference_text: "人工精校原文", label_source: "manual" }),
    "utf8",
  );

  let playCount = 0;
  const fakeServer = await startFakeE2eServer({
    token,
    runtimeDir,
    onAcousticTurn: () => ({
      raw_asr: "激光机状态",
      route: "fast_read",
      peak_uplink_level: 50,
      corpus_date: "2026-11-27",
      corpus_turn_id: "manual-turn-1",
    }),
  });
  try {
    const res = await runXiaozhiE2e({
      runtimeDir,
      url: fakeServer.url,
      corpus: corpusPath,
      skipCalibration: true,
      quietMs: 20,
      pollIntervalMs: 15,
      playWavFn: async (_wavPath, { spec }) => {
        playCount += 1;
        const started = Date.now();
        const ended = started + 60;
        fakeServer.simulateAcousticPlayback({ spec, playbackStartedAtMs: started, playbackEndedAtMs: ended });
        // 第 1 句刚播完就模拟用户点“立即结束测试”关闭时间窗
        await fetch(`${fakeServer.url}/debug/v1/bench-window`, {
          method: "POST",
          headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
          body: JSON.stringify({ device_id: "aa:bb:cc:dd:ee:01", action: "close" }),
        });
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: 60 };
      },
    });
    assert.equal(playCount, 1, "时间窗关闭后绝不得再外放第 2 句高危语料");
    assert.equal(res.exitCode, 1);
    const preservedLabel = JSON.parse(readFileSync(join(corpusDay, "manual-turn-1.label.json"), "utf8"));
    assert.equal(preservedLabel.label_source, "manual", "自动标注绝不得覆盖已有人工标注");
    assert.equal(preservedLabel.reference_text, "人工精校原文");
  } finally {
    await fakeServer.close();
    rmSync(runtimeDir, { recursive: true, force: true });
  }
});

test("VE1/VE3/VE4 网上真人语音按目录标为 online-human、强制 4 字段元数据禁 .txt、过滤指令句、上限轮换与超时分批复核会话", async () => {
  const { isOnlineCommandLikeText, prepareVoiceAssets } = await import("../voice-bench/voice-assets.mjs");
  const { planE2eBatches } = await import("../voice-bench/xiaozhi-e2e.mjs");

  const writeOnlineClip = (runtimeDir, id, meta) => {
    const dir = join(runtimeDir, "voice-bench", "recordings", "online");
    mkdirSync(dir, { recursive: true });
    makeSyntheticWav(join(dir, `${id}.wav`), 0.8, 440);
    if (meta !== null) {
      writeFileSync(join(dir, `${id}.json`), JSON.stringify(meta), "utf8");
    }
  };

  // 1. 指令句过滤规则：停止/退出/播放音乐/音量/亮度/静音/关机/重启一律拦截，日常句放行
  for (const bad of ["停", "退下", "请播放一首轻音乐", "帮我放首歌吧", "把音量调大一点", "调高屏幕亮度", "现在静音", "帮我关机", "重启一下小智", "开始雕刻"]) {
    assert.equal(isOnlineCommandLikeText(bad).blocked, true, `疑似指令句必须被过滤：${bad}`);
  }
  assert.equal(isOnlineCommandLikeText("今天北京天气晴朗适合出门散步").blocked, false);
  assert.equal(isOnlineCommandLikeText("三毫米椴木一般用什么速度雕刻").blocked, false);

  // 2. VE1 目录判定 + 元数据校验 + .txt 仅用户目录回退 + 指令句过滤 + VE3 --max-online 上限与 .rotation.json 轮换
  const runtimeDir = mkdtempSync(join(tmpdir(), "moss-ve1-online-"));
  try {
    const onlineDir = join(runtimeDir, "voice-bench", "recordings", "online");
    mkdirSync(onlineDir, { recursive: true });

    // 2a. online/ 仅有 .txt -> 必须跳过
    makeSyntheticWav(join(onlineDir, "txt_only.wav"), 0.8, 440);
    writeFileSync(join(onlineDir, "txt_only.txt"), "今天天气真好\n", "utf8");

    // 2b. online/ 缺少 license / dataset / source_url -> 全部跳过
    writeOnlineClip(runtimeDir, "no_license", {
      text: "杭州西湖风景很美",
      dataset: "AISHELL-1",
      source_url: "https://www.openslr.org/33/",
    });
    writeOnlineClip(runtimeDir, "no_dataset", {
      text: "春天来了草长莺飞",
      license: "Apache-2.0",
      source_url: "https://www.openslr.org/33/",
    });
    writeOnlineClip(runtimeDir, "no_url", {
      text: "秋天落叶铺满街道",
      dataset: "AISHELL-1",
      license: "Apache-2.0",
    });

    // 2c. online/ 包含指令句 -> 过滤并进入 skippedAssets
    writeOnlineClip(runtimeDir, "cmd_music", {
      text: "帮我放首歌听听吧",
      dataset: "AISHELL-1",
      license: "Apache-2.0",
      source_url: "https://www.openslr.org/33/",
    });
    writeOnlineClip(runtimeDir, "cmd_vol", {
      text: "请把音量调大一点",
      dataset: "AISHELL-1",
      license: "Apache-2.0",
      source_url: "https://www.openslr.org/33/",
    });

    // 2d. online/ 4 条合规样本（故意把 speaker 写成 "我"，验证来源仍强制按目录标为 online-human）
    const validTexts = [
      "今天北京天气晴朗适合散步",
      "周末打算去公园看看银杏树",
      "三毫米椴木适合用来做书签",
      "下午茶喝了一杯清香绿茶",
    ];
    validTexts.forEach((text, idx) => {
      writeOnlineClip(runtimeDir, `ok_0${idx + 1}`, {
        text,
        speaker: "我", // 故意填成用户标签，必须仍被目录判定为 online-human
        dataset: "AISHELL-1",
        license: "Apache-2.0",
        source_url: "https://www.openslr.org/33/",
      });
    });

    // 2e. 用户自录目录允许 .txt 回退，且标为 human-recording
    const userDir = join(runtimeDir, "voice-bench", "recordings", "我");
    mkdirSync(userDir, { recursive: true });
    makeSyntheticWav(join(userDir, "user_txt_clip.wav"), 0.8, 480);
    writeFileSync(join(userDir, "user_txt_clip.txt"), "帮我查一下激光机状态\n", "utf8");

    // 验证 --max-online 0 完全不加载网上语音
    const zeroOnline = await prepareVoiceAssets({
      turns: [{ id: "status_fast_read", label: "状态", text: "激光机现在什么状态" }],
      calibrationText: "激光机现在什么状态",
      runtimeDir,
      maxOnline: 0,
    });
    assert.equal(
      zeroOnline.preparedTurns.some((t) => t.asset.source_type === "online-human"),
      false,
      "--max-online 0 时绝不应加载 online/ 语音",
    );
    assert.equal(zeroOnline.preparedTurns.length, 1);
    assert.equal(zeroOnline.preparedTurns[0].spec.id, "user_txt_clip");
    assert.equal(zeroOnline.preparedTurns[0].asset.source_type, "human-recording");

    // 第 1 次抽样（maxOnline=2）：应抽 ok_01, ok_02，跳过 6 条不合格/指令句，并写 .rotation.json
    const run1 = await prepareVoiceAssets({
      turns: [{ id: "status_fast_read", label: "状态", text: "激光机现在什么状态" }],
      calibrationText: "激光机现在什么状态",
      runtimeDir,
      maxOnline: 2,
    });
    assert.equal(run1.skippedAssets.length, 6, `应跳过 6 条不合格或指令素材：${JSON.stringify(run1.skippedAssets)}`);
    const online1 = run1.preparedTurns.filter((t) => t.asset.source_type === "online-human");
    const user1 = run1.preparedTurns.filter((t) => t.asset.source_type === "human-recording");
    assert.equal(user1.length, 1);
    assert.deepEqual(
      online1.map((t) => t.spec.id),
      ["ok_01", "ok_02"],
    );
    for (const item of online1) {
      assert.equal(item.asset.source_type, "online-human", "online/ 目录无论 speaker 写什么都必须标为 online-human");
      assert.equal(item.asset.audio_origin, "online-human");
      assert.equal(item.asset.dataset, "AISHELL-1");
      assert.equal(item.asset.license, "Apache-2.0");
      assert.equal(item.asset.source_url, "https://www.openslr.org/33/");
    }
    const rotFile = join(onlineDir, ".rotation.json");
    assert.ok(existsSync(rotFile), "轮换游标 .rotation.json 必须落盘");
    assert.equal(JSON.parse(readFileSync(rotFile, "utf8")).cursor, 2);

    // 第 2 次抽样（maxOnline=2）：必须轮换到 ok_03, ok_04，不测同一批
    const run2 = await prepareVoiceAssets({
      turns: [{ id: "status_fast_read", label: "状态", text: "激光机现在什么状态" }],
      calibrationText: "激光机现在什么状态",
      runtimeDir,
      maxOnline: 2,
    });
    const online2 = run2.preparedTurns.filter((t) => t.asset.source_type === "online-human");
    assert.deepEqual(
      online2.map((t) => t.spec.id),
      ["ok_03", "ok_04"],
    );
    assert.equal(JSON.parse(readFileSync(rotFile, "utf8")).cursor, 0);

    // CLI 参数校验
    assert.equal(parseE2eArgs([]).maxOnline, 20);
    assert.equal(parseE2eArgs(["--max-online", "0"]).maxOnline, 0);
    assert.equal(parseE2eArgs(["--max-online", "15"]).maxOnline, 15);
    assert.throws(() => parseE2eArgs(["--max-online", "-1"]), /--max-online/);

    // 3. VE3 自动分批规划：单窗上限绝不超过 60 分钟
    const plan = planE2eBatches(
      Array.from({ length: 5 }, (_, idx) => ({
        spec: { id: `p_${idx + 1}`, text: `句子${idx + 1}` },
        asset: { duration_ms: 4_000 },
      })),
      { maxMinutes: 60, batchWindowMs: 55_000, estimatedReplyMs: 18_000, estimatedIntervalMs: 4_000, hasCalibration: false },
    );
    assert.equal(plan.exceedsWindow, true);
    assert.equal(plan.maxMinutes, 60);
    assert.equal(plan.batches.length, 3);
    assert.deepEqual(
      plan.batches.map((b) => b.length),
      [2, 2, 1],
    );
  } finally {
    rmSync(runtimeDir, { recursive: true, force: true });
  }

  // 4. 端到端集成验证：VE1 全链路透传（turns.jsonl / audio-manifest.json / .label.json / timeline / report.md / asr-bench）+ VE3 超窗分批 & 会话变更中止
  const token = "ve-e2e-full-token-0123456789abcdef0123456789abcdef";
  const e2eRuntime = setupRuntimeWithToken("moss-ve-e2e-", token);
  const corpusPath = join(e2eRuntime, "ve-corpus.json");
  writeFileSync(
    corpusPath,
    JSON.stringify({
      name: "ve-mixed",
      turns: [{ id: "status_fast_read", label: "我的自录状态", text: "激光机现在什么状态" }],
    }),
    "utf8",
  );
  writeHumanRecording(e2eRuntime, "我", "status_fast_read", "激光机现在什么状态", 440);
  writeOnlineClip(e2eRuntime, "online_01", {
    text: "今天北京天气晴朗适合出门",
    speaker: "网上样本",
    dataset: "CommonVoice-zh",
    license: "CC0-1.0",
    source_url: "https://commonvoice.mozilla.org/",
  });
  writeOnlineClip(e2eRuntime, "online_02", {
    text: "周末想去工坊做一块木牌",
    speaker: "网上样本",
    dataset: "AISHELL-1",
    license: "Apache-2.0",
    source_url: "https://www.openslr.org/33/",
  });
  // 放入 1 条缺 license 与 1 条放歌指令句，验证报告“被跳过素材”
  writeOnlineClip(e2eRuntime, "online_bad_lic", {
    text: "缺少许可信息的句子",
    dataset: "Unknown",
    source_url: "https://example.test/",
  });
  writeOnlineClip(e2eRuntime, "online_bad_cmd", {
    text: "请播放音乐放松一下",
    dataset: "AISHELL-1",
    license: "Apache-2.0",
    source_url: "https://www.openslr.org/33/",
  });

  const fakeServer = await startFakeE2eServer({
    token,
    runtimeDir: e2eRuntime,
    onAcousticTurn: ({ spec }) => ({
      raw_asr: spec.text,
      corrected_asr: spec.text,
      route: "llm",
      peak_uplink_level: 50,
      writeCorpus: true,
    }),
    onDigitalTurn: ({ turnNo }) => ({
      raw_asr: turnNo <= 2 ? "激光机现在什么状态" : "今天北京天气晴朗适合出门",
    }),
  });

  try {
    // 4a. 触发自动分批（3 句分 2 批，每批独立 open/close 时间窗且会话一致）
    const batchedRes = await runXiaozhiE2e({
      runtimeDir: e2eRuntime,
      url: fakeServer.url,
      corpus: corpusPath,
      maxOnline: 2,
      maxMinutes: 10,
      batchWindowMs: 55_000,
      estimatedReplyMs: 18_000,
      estimatedIntervalMs: 4_000,
      compareDigital: true,
      skipCalibration: true,
      quietMs: 20,
      pollIntervalMs: 15,
      logMissing: false,
      logBatching: false,
      playWavFn: async (_wavPath, { spec }) => {
        const started = Date.now();
        await new Promise((r) => setTimeout(r, 25));
        const ended = Date.now();
        fakeServer.simulateAcousticPlayback({ spec, playbackStartedAtMs: started, playbackEndedAtMs: ended });
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: ended - started };
      },
    });

    assert.equal(batchedRes.exitCode, 0, `预期分批整体测试通过：${batchedRes.errorMessage || JSON.stringify(batchedRes.summary?.failed_turns)}`);
    assert.ok(batchedRes.summary.batch_plan.batch_count >= 2, "超过单窗预算必须自动分批");
    assert.equal(batchedRes.summary.batch_plan.batches_executed, batchedRes.summary.batch_plan.batch_count);
    assert.equal(batchedRes.summary.batch_plan.session_aborted, false);
    const openCount = fakeServer.benchActions.filter((a) => a.action === "open").length;
    const closeCount = fakeServer.benchActions.filter((a) => a.action === "close").length;
    assert.equal(openCount, batchedRes.summary.batch_plan.batch_count, "每批必须单独打开新的测试时间窗");
    assert.equal(closeCount, openCount, "每批结束后必须正常关闭时间窗");
    assert.ok(
      fakeServer.benchActions.filter((a) => a.action === "open").every((a) => a.max_minutes <= 60),
      "单窗绝不能超过 60 分钟上限",
    );

    // 验证 turns.jsonl、audio-manifest.json、timeline、summary.json、report.md 与 .label.json 全链路透传
    const turnLines = readFileSync(join(batchedRes.archiveDir, "turns.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map((l) => JSON.parse(l));
    const userTurn = turnLines.find((t) => t.id === "status_fast_read");
    const onlineTurn = turnLines.find((t) => t.id === "online_01");
    assert.equal(userTurn.source_type, "human-recording");
    assert.equal(userTurn.audio_origin, "human-recording");
    assert.equal(onlineTurn.source_type, "online-human");
    assert.equal(onlineTurn.audio_origin, "online-human");
    assert.equal(onlineTurn.dataset, "CommonVoice-zh");
    assert.equal(onlineTurn.license, "CC0-1.0");

    const manifest = JSON.parse(readFileSync(join(batchedRes.archiveDir, "audio-manifest.json"), "utf8"));
    const onlineManifestItem = manifest.items.find((i) => i.id === "online_01");
    assert.equal(onlineManifestItem.source_type, "online-human");
    assert.equal(onlineManifestItem.dataset, "CommonVoice-zh");
    assert.equal(onlineManifestItem.license, "CC0-1.0");
    assert.equal(manifest.skipped_assets.length, 2);

    const timelineJsonl = readFileSync(join(batchedRes.archiveDir, "timeline.jsonl"), "utf8");
    const timelineTxt = readFileSync(join(batchedRes.archiveDir, "timeline.txt"), "utf8");
    assert.ok(timelineJsonl.includes("online-human") && timelineJsonl.includes("CommonVoice-zh") && timelineJsonl.includes("CC0-1.0"));
    assert.ok(timelineTxt.includes("网上真人") && timelineTxt.includes("CommonVoice-zh") && timelineTxt.includes("我的录音"));

    assert.ok(batchedRes.summary.by_source.some((s) => s.source_type === "online-human"));
    assert.ok(batchedRes.summary.by_source.some((s) => s.source_type === "human-recording"));
    assert.ok(batchedRes.summary.by_source.some((s) => s.source_type === "digital_inject"));
    assert.equal(batchedRes.summary.skipped_assets.length, 2);

    const reportMd = readFileSync(join(batchedRes.archiveDir, "report.md"), "utf8");
    assert.ok(reportMd.includes("按语音来源分组统计"));
    assert.ok(reportMd.includes("网上语音的指标只反映通用识别能力，不代表用户本人的识别效果"));
    assert.ok(reportMd.includes("被跳过素材"));
    assert.ok(reportMd.includes("online_bad_lic"));
    assert.ok(reportMd.includes("online_bad_cmd"));

    // 检查自动回写的 .label.json 带 audio_origin / dataset / license，并供 asr-bench.mjs 按 4 类来源分组统计
    const corpusDay = join(e2eRuntime, "asr-corpus", "2026-11-27");
    const labelFiles = readdirSync(corpusDay).filter((f) => f.endsWith(".label.json"));
    const labels = labelFiles.map((f) => JSON.parse(readFileSync(join(corpusDay, f), "utf8")));
    assert.ok(
      labels.some((l) => l.audio_origin === "online-human" && l.dataset === "CommonVoice-zh" && l.license === "CC0-1.0"),
      ".label.json 必须包含 audio_origin=online-human 与 dataset/license",
    );
    assert.ok(
      labels.some((l) => l.audio_origin === "human-recording"),
      ".label.json 必须包含 audio_origin=human-recording",
    );

    // 补充 live_mic 与 digital_inject 样本跑 asr-bench.mjs，验证四类来源完整分组与免责说明
    makeSyntheticWav(join(corpusDay, "live-t1.wav"), 0.5, 440);
    writeFileSync(join(corpusDay, "live-t1.json"), JSON.stringify({ turn_id: "live-t1", date: "2026-11-27", simulated: false }), "utf8");
    writeFileSync(
      join(corpusDay, "live-t1.label.json"),
      JSON.stringify({ turn_id: "live-t1", date: "2026-11-27", input_source: "live_mic", reference_text: "三毫米椴木" }),
      "utf8",
    );
    makeSyntheticWav(join(corpusDay, "dig-t1.wav"), 0.5, 440);
    writeFileSync(join(corpusDay, "dig-t1.json"), JSON.stringify({ turn_id: "dig-t1", date: "2026-11-27", simulated: false }), "utf8");
    writeFileSync(
      join(corpusDay, "dig-t1.label.json"),
      JSON.stringify({ turn_id: "dig-t1", date: "2026-11-27", input_source: "digital_inject", reference_text: "三毫米椴木" }),
      "utf8",
    );
    const asrRes = await runAsrBench({
      runtimeDir: e2eRuntime,
      noiseLevels: [0],
      mixNoiseFn: async () => new Map(),
      recognizers: [
        {
          id: "fake_asr",
          label: "假模型",
          async transcribe(_w, { sample }) {
            return { text: sample.reference_text, firstInterimMs: 40, finalMs: 100 };
          },
        },
      ],
    });
    assert.equal(asrRes.exitCode, 0);
    assert.ok(asrRes.analysis.byAudioOrigin["online-human"]);
    assert.ok(asrRes.analysis.byAudioOrigin["human-recording"]);
    assert.ok(asrRes.analysis.byAudioOrigin.live_mic);
    assert.ok(asrRes.analysis.byAudioOrigin.digital_inject);
    const asrReport = readFileSync(join(asrRes.archiveDir, "report.md"), "utf8");
    assert.ok(asrReport.includes("按语音来源分组统计（网上真人 / 我的录音 / 小智实时说话 / 数字直通）"));
    assert.ok(asrReport.includes("网上语音的指标只反映通用识别能力，不代表用户本人的识别效果"));

    // 4b. VE3 分批重开前若设备会话已变更，必须立即中止且绝不重开第 2 批时间窗
    fakeServer.benchActions.length = 0;
    let playedInSessionAbort = 0;
    const abortRes = await runXiaozhiE2e({
      runtimeDir: e2eRuntime,
      url: fakeServer.url,
      corpus: corpusPath,
      maxOnline: 2,
      maxMinutes: 10,
      batchWindowMs: 30_000,
      estimatedReplyMs: 18_000,
      estimatedIntervalMs: 4_000,
      skipCalibration: true,
      quietMs: 20,
      pollIntervalMs: 15,
      logMissing: false,
      logBatching: false,
      playWavFn: async (_wavPath, { spec }) => {
        playedInSessionAbort += 1;
        const started = Date.now();
        await new Promise((r) => setTimeout(r, 25));
        const ended = Date.now();
        fakeServer.simulateAcousticPlayback({ spec, playbackStartedAtMs: started, playbackEndedAtMs: ended });
        // 第 1 批完成后模拟设备重连换了新 session_id
        setTimeout(() => fakeServer.setSessionId("sess-e2e-reconnected-2"), 45);
        return { started_at_ms: started, ended_at_ms: ended, duration_ms: ended - started };
      },
    });
    assert.equal(abortRes.exitCode, 1, "分批期间设备会话变更必须中止并返回非 0 状态码");
    assert.equal(abortRes.summary.batch_plan.session_aborted, true);
    assert.equal(playedInSessionAbort, 1, "会话变更后不得播放第 2 批语料");
    assert.deepEqual(
      fakeServer.benchActions.map((a) => a.action),
      ["open", "close"],
      "第 2 批检测到会话变更前绝不能重开测试时间窗",
    );
  } finally {
    await fakeServer.close();
    rmSync(e2eRuntime, { recursive: true, force: true });
  }
});
