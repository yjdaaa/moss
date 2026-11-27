// VD1 / VD3 真小智整体测试真人录音素材加载与跨平台外放模块：
// - 只使用真人录音，绝不调用本地 TTS 或系统朗读（say / System.Speech / espeak）；
// - 默认从 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}` 读取经电脑麦克风录制的真人素材；
// - 支持按全部说话人轮流使用，或通过 `--speaker <标签>` 指定一位说话人；
// - VB 的 `<runtime>/asr-corpus` 默认不作为外放素材（经小智麦克风采集，再外放会被染色两次），
//   仅在显式开启 `--include-device-corpus` 时作为“二次录制”素材补充，并在报告中明确标注；
// - 跨平台播放：macOS `afplay`、Windows PowerShell `System.Media.SoundPlayer`、Linux `paplay`/`aplay`；
// - 背景噪声：复用 `asr_noise_mix.py` / `chain_clips.py`（经 `python3 -I`）生成 16 kHz 循环噪声 WAV。

import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, statSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { loadLabeledCorpus, normalizeForCer, readWavPcm16 } from "./asr-bench.mjs";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const NOISE_MIX_SCRIPT = join(SCRIPT_DIR, "asr_noise_mix.py");
export const TARGET_SAMPLE_RATE = 16_000;
export const BENCH_RECORDINGS_DIR_REL = "voice-bench/recordings";
export const RECORDING_APP_HINT =
  "请打开 MOSS 桌面端「语音中心 › 记录与诊断 › AI 联调与测试 › 录制测试语料」录制真人录音后再运行整体测试。";

export function wavDurationMs(wavPath) {
  const { pcm, sampleRate } = readWavPcm16(wavPath);
  const samples = Math.floor(pcm.length / 2);
  return Math.round((samples * 1000) / Math.max(1, sampleRate));
}

function commandExists(cmd, platform = process.platform) {
  try {
    execFileSync(platform === "win32" ? "where" : "which", [cmd], {
      stdio: ["ignore", "pipe", "ignore"],
      encoding: "utf8",
    });
    return true;
  } catch {
    return false;
  }
}

/** 检测当前平台可用的外放播放器。 */
export function detectPlayer(platform = process.platform) {
  if (platform === "darwin") {
    return commandExists("afplay", platform) ? { cmd: "afplay", kind: "afplay" } : null;
  }
  if (platform === "win32") {
    const ps = commandExists("powershell.exe", platform)
      ? "powershell.exe"
      : commandExists("powershell", platform)
        ? "powershell"
        : null;
    return ps ? { cmd: ps, kind: "powershell" } : null;
  }
  if (commandExists("paplay", platform)) return { cmd: "paplay", kind: "paplay" };
  if (commandExists("aplay", platform)) return { cmd: "aplay", kind: "aplay" };
  return null;
}

export function checkPlayerAvailable(platform = process.platform) {
  const player = detectPlayer(platform);
  if (!player) {
    throw new Error(
      platform === "darwin"
        ? "未找到 macOS 音频播放器 afplay，无法外放测试句"
        : platform === "win32"
          ? "未找到 PowerShell，无法调用 System.Media.SoundPlayer 外放测试句"
          : "未找到音频播放器 paplay 或 aplay，请先安装 pulseaudio-utils 或 alsa-utils",
    );
  }
  return player;
}

function spawnPlayerProcess(player, wavPath) {
  if (player.kind === "powershell") {
    const script =
      "param([string]$p) $s = New-Object System.Media.SoundPlayer $p; $s.PlaySync()";
    return spawn(player.cmd, ["-NoProfile", "-NonInteractive", "-Command", script, wavPath], {
      stdio: ["ignore", "ignore", "pipe"],
    });
  }
  if (player.kind === "aplay") {
    return spawn(player.cmd, ["-q", wavPath], { stdio: ["ignore", "ignore", "pipe"] });
  }
  return spawn(player.cmd, [wavPath], { stdio: ["ignore", "ignore", "pipe"] });
}

/** 播放单个 WAV 文件并精确记录本机起止时间戳（与同机服务端时钟一致）。 */
export async function playWav(wavPath, opts = {}) {
  if (typeof opts.playWavFn === "function") {
    return opts.playWavFn(wavPath, opts);
  }
  const player = opts.player || checkPlayerAvailable(opts.platform || process.platform);
  const startedAtMs = Date.now();
  await new Promise((resolvePromise, rejectPromise) => {
    const child = spawnPlayerProcess(player, wavPath);
    let stderr = "";
    child.stderr?.on("data", (d) => {
      stderr += d.toString("utf8");
    });
    child.on("error", (err) => rejectPromise(err));
    child.on("close", (code) => {
      if (code === 0) resolvePromise();
      else rejectPromise(new Error(`音频播放失败（${player.cmd} exit=${code}）：${stderr.trim()}`));
    });
  });
  const endedAtMs = Date.now();
  return {
    started_at_ms: startedAtMs,
    ended_at_ms: endedAtMs,
    duration_ms: Math.max(0, endedAtMs - startedAtMs),
  };
}

/** 用 `python3 -I asr_noise_mix.py --generate-noise` 生成背景噪声 WAV 并低音量循环播放。 */
export function startBackgroundNoise({
  preset,
  gain = 0.22,
  cacheDir,
  pythonBin = "python3",
  platform = process.platform,
  player = null,
}) {
  mkdirSync(cacheDir, { recursive: true });
  const safePreset = String(preset || "fan").toLowerCase().replace(/[^a-z0-9_-]/g, "");
  const clampedGain = Math.max(0.02, Math.min(1.0, Number(gain) || 0.22));
  const noiseWav = join(cacheDir, `bg-noise-${safePreset}-g${Math.round(clampedGain * 100)}.wav`);
  if (!existsSync(noiseWav)) {
    execFileSync(
      pythonBin,
      [
        "-I",
        NOISE_MIX_SCRIPT,
        "--generate-noise",
        safePreset,
        "--duration-secs",
        "12",
        "--gain",
        String(clampedGain),
        "--output",
        noiseWav,
      ],
      { stdio: ["ignore", "pipe", "pipe"], encoding: "utf8" },
    );
  }
  const activePlayer = player || checkPlayerAvailable(platform);
  let stopped = false;
  let currentChild = null;

  const loopPlay = () => {
    if (stopped) return;
    currentChild = spawnPlayerProcess(activePlayer, noiseWav);
    currentChild.on("close", () => {
      currentChild = null;
      if (!stopped) loopPlay();
    });
    currentChild.on("error", () => {
      currentChild = null;
    });
  };
  loopPlay();

  return {
    wavPath: noiseWav,
    filename: basename(noiseWav),
    preset: safePreset,
    gain: clampedGain,
    stop() {
      stopped = true;
      if (currentChild) {
        try {
          currentChild.kill("SIGTERM");
        } catch {
          // 忽略已退出进程
        }
        currentChild = null;
      }
    },
  };
}

/** 扫描 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}` 中的真人录音。 */
export function loadBenchRecordings(recordingsDir, { speakerFilter = "" } = {}) {
  const root = resolve(recordingsDir);
  if (!existsSync(root) || !statSync(root).isDirectory()) {
    return { recordings: [], speakers: [] };
  }
  const filter = String(speakerFilter || "").trim();
  const recordings = [];
  const allSpeakers = new Set();

  const speakerEntries = readdirSync(root, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && !entry.isSymbolicLink())
    .sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));

  for (const dirEntry of speakerEntries) {
    const speakerDirName = dirEntry.name;
    const speakerPath = join(root, speakerDirName);
    const files = readdirSync(speakerPath, { withFileTypes: true })
      .filter((f) => f.isFile() && !f.isSymbolicLink() && f.name.endsWith(".wav"))
      .sort((a, b) => a.name.localeCompare(b.name));

    for (const fileEntry of files) {
      const corpusId = fileEntry.name.slice(0, -4);
      const wavPath = join(speakerPath, fileEntry.name);
      const jsonPath = join(speakerPath, `${corpusId}.json`);
      let meta = {};
      if (existsSync(jsonPath)) {
        try {
          meta = JSON.parse(readFileSync(jsonPath, "utf8")) || {};
        } catch {
          meta = {};
        }
      }
      const speaker = String(meta.speaker || speakerDirName).trim() || speakerDirName;
      allSpeakers.add(speaker);
      if (filter && speaker !== filter && speakerDirName !== filter) continue;

      let durationMs = Number(meta.duration_ms) || 0;
      if (!durationMs) {
        try {
          durationMs = wavDurationMs(wavPath);
        } catch {
          durationMs = 0;
        }
      }
      recordings.push({
        speaker,
        speaker_dir: speakerDirName,
        corpus_id: corpusId,
        wav_path: wavPath,
        filename: `${speakerDirName}/${fileEntry.name}`,
        text: String(meta.text || "").trim(),
        duration_ms: durationMs,
        peak_level: Number(meta.peak_level) || 0,
        rms_level: Number(meta.rms_level) || 0,
        recorded_at_ms: Number(meta.recorded_at_ms) || 0,
      });
    }
  }

  return {
    recordings,
    speakers: [...allSpeakers].sort((a, b) => a.localeCompare(b, "zh-CN")),
  };
}

function findMatchingDeviceCorpusSample(deviceSamples, spec, usedIds) {
  if (!deviceSamples.length) return null;
  const targetNorm = normalizeForCer(spec.text).join("");
  if (!targetNorm) return null;
  const exact = deviceSamples.find(
    (s) => !usedIds.has(s.id) && normalizeForCer(s.reference_text).join("") === targetNorm,
  );
  if (exact) return exact;
  if (targetNorm.length >= 4) {
    return (
      deviceSamples.find((s) => {
        if (usedIds.has(s.id)) return false;
        const sn = normalizeForCer(s.reference_text).join("");
        return (
          sn.length >= 4 &&
          Math.abs(sn.length - targetNorm.length) <= 2 &&
          (sn.includes(targetNorm) || targetNorm.includes(sn))
        );
      }) || null
    );
  }
  return null;
}

/**
 * VD1 / VD3 准备整体测试真人录音素材（绝不使用本地 TTS 或系统朗读）：
 * 1. 默认仅从 `<runtime>/voice-bench/recordings/` 读取电脑麦克风录制的真人素材；
 *    - 指定 `speaker` 时只用该说话人；
 *    - 未指定时按说话人轮流覆盖每句语料；
 * 2. 某句没有真人录音时记入 `missingTurns` 并跳过（不合成）；
 * 3. `includeDeviceCorpus` 为 `true` 时，才允许用 `<runtime>/asr-corpus` 中用户手工标注的录音补充缺失句及附加组，并显式标为 `secondary_recording: true`（“二次录制”）。
 */
export async function prepareVoiceAssets({
  turns,
  calibrationText = "",
  runtimeDir,
  recordingsDir = "",
  corpusDir = "",
  speaker = "",
  includeDeviceCorpus = false,
}) {
  const resolvedRecordingsDir = resolve(recordingsDir || join(runtimeDir, BENCH_RECORDINGS_DIR_REL));
  const resolvedCorpusDir = resolve(corpusDir || join(runtimeDir, "asr-corpus"));
  const speakerFilter = String(speaker || "").trim();

  const { recordings, speakers: availableSpeakers } = loadBenchRecordings(resolvedRecordingsDir, {
    speakerFilter,
  });

  // 仅当显式开启 --include-device-corpus 时才加载小智麦克风采集的 asr-corpus（排除 e2e-playback 防二次套娃）
  const deviceSamples = includeDeviceCorpus
    ? loadLabeledCorpus(resolvedCorpusDir).filter((s) => s.label_source !== "e2e-playback")
    : [];

  const preparedTurns = [];
  const missingTurns = [];
  const usedDeviceCorpusIds = new Set();
  const speakersUsed = new Set();
  let roundRobinCursor = 0;

  for (let i = 0; i < turns.length; i += 1) {
    const spec = turns[i];
    const targetNorm = normalizeForCer(spec.text).join("");
    let candidates = recordings.filter((r) => r.corpus_id === spec.id);
    if (candidates.length === 0 && targetNorm) {
      candidates = recordings.filter((r) => normalizeForCer(r.text).join("") === targetNorm);
    }

    if (candidates.length > 0) {
      const chosen = candidates[roundRobinCursor % candidates.length];
      roundRobinCursor += 1;
      speakersUsed.add(chosen.speaker);
      preparedTurns.push({
        spec,
        asset: {
          wavPath: chosen.wav_path,
          filename: chosen.filename,
          source_type: "human-recording",
          speaker: chosen.speaker,
          secondary_recording: false,
          voice_profile: {
            source: "human-recording",
            speaker: chosen.speaker,
            corpus_id: chosen.corpus_id,
            peak_level: chosen.peak_level,
            label: `真人录音（${chosen.speaker}）`,
          },
          duration_ms: chosen.duration_ms || wavDurationMs(chosen.wav_path),
        },
      });
      continue;
    }

    if (includeDeviceCorpus) {
      const matched = findMatchingDeviceCorpusSample(deviceSamples, spec, usedDeviceCorpusIds);
      if (matched) {
        usedDeviceCorpusIds.add(matched.id);
        const secondarySpeaker = "设备二次录制";
        speakersUsed.add(secondarySpeaker);
        preparedTurns.push({
          spec,
          asset: {
            wavPath: matched.wav_path,
            filename: basename(matched.wav_path),
            source_type: "device-corpus-secondary",
            speaker: secondarySpeaker,
            secondary_recording: true,
            voice_profile: {
              source: "device-corpus-secondary",
              speaker: secondarySpeaker,
              secondary_recording: true,
              corpus_id: matched.id,
              label: "设备录音（二次录制）",
            },
            duration_ms: wavDurationMs(matched.wav_path),
          },
        });
        continue;
      }
    }

    missingTurns.push({
      id: String(spec.id || `turn_${i + 1}`),
      label: String(spec.label || spec.id || `第 ${i + 1} 句`),
      text: String(spec.text || ""),
    });
  }

  // 显式开启 --include-device-corpus 时，把剩余未匹配的设备录音追加到退出词之前，统一标明“二次录制”
  if (includeDeviceCorpus && deviceSamples.length > 0) {
    const extras = deviceSamples.filter((s) => !usedDeviceCorpusIds.has(s.id)).slice(0, 10);
    if (extras.length > 0) {
      const exitIdx = preparedTurns.findIndex((item) => item.spec.expect_route === "fast_exit");
      const insertAt = exitIdx >= 0 ? exitIdx : preparedTurns.length;
      const secondarySpeaker = "设备二次录制";
      speakersUsed.add(secondarySpeaker);
      const extraItems = extras.map((sample, idx) => ({
        spec: {
          id: `device_corpus_${idx + 1}`,
          label: `二次录制 · ${sample.turn_id}`,
          group: "二次录制",
          text: sample.reference_text,
          tags: sample.tags,
        },
        asset: {
          wavPath: sample.wav_path,
          filename: basename(sample.wav_path),
          source_type: "device-corpus-secondary",
          speaker: secondarySpeaker,
          secondary_recording: true,
          voice_profile: {
            source: "device-corpus-secondary",
            speaker: secondarySpeaker,
            secondary_recording: true,
            corpus_id: sample.id,
            label: "设备录音（二次录制）",
          },
          duration_ms: wavDurationMs(sample.wav_path),
        },
      }));
      preparedTurns.splice(insertAt, 0, ...extraItems);
    }
  }

  // 校准同样只用真人录音（优先匹配校准文本对应的录音，否则取第一条可用真人录音及其原文）
  let calibrationAsset = null;
  let calibrationEffectiveText = calibrationText;
  if (calibrationText && preparedTurns.length > 0) {
    const calNorm = normalizeForCer(calibrationText).join("");
    const exactTurn = preparedTurns.find(
      (item) =>
        !item.asset.secondary_recording &&
        (normalizeForCer(item.spec.text).join("") === calNorm || item.spec.id === "status_fast_read"),
    );
    const fallbackTurn =
      exactTurn || preparedTurns.find((item) => !item.asset.secondary_recording) || preparedTurns[0];
    if (fallbackTurn) {
      calibrationAsset = fallbackTurn.asset;
      calibrationEffectiveText = fallbackTurn.spec.text || calibrationText;
    }
  }

  return {
    calibrationAsset,
    calibrationEffectiveText,
    preparedTurns,
    missingTurns,
    speakersUsed: [...speakersUsed],
    availableSpeakers,
    recordingsDir: resolvedRecordingsDir,
  };
}
