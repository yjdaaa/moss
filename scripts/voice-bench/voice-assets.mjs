// VD1 / VD3 / VE1 / VE3 / VE4 真小智整体测试真人录音素材加载与跨平台外放模块：
// - 只使用真人录音，绝不调用本地 TTS 或系统朗读（say / System.Speech / espeak）；
// - 来源判定严格按目录：
//   - `<runtime>/voice-bench/recordings/online/` 下的样本一律标为 `source_type: "online-human"`，
//     同名 `.json` 必须包含 `text`、`dataset`、`license`、`source_url`（禁止仅用 `.txt`），
//     并过滤掉疑似指令句、受 `--max-online` 上限与 `.rotation.json` 轮换约束；
//   - `<runtime>/voice-bench/recordings/<speaker>/`（非 `online`）下为用户自录语音，标为 `source_type: "human-recording"`（允许 `.txt` 回退）；
// - VB 的 `<runtime>/asr-corpus` 默认不作为外放素材（经小智麦克风采集，再外放会被染色两次），
//   仅在显式开启 `--include-device-corpus` 时作为“二次录制”素材补充，并在报告中明确标注；
// - 跨平台播放：macOS `afplay`、Windows PowerShell `System.Media.SoundPlayer`、Linux `paplay`/`aplay`；
// - 背景噪声：复用 `asr_noise_mix.py` / `chain_clips.py`（经 `python3 -I`）生成 16 kHz 循环噪声 WAV。

import { execFileSync, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  PROTECTED_COMMAND_WORDS,
  classifyCommand,
  generateNoiseWavWithNode,
  loadLabeledCorpus,
  normalizeForCer,
  readWavPcm16,
} from "./asr-bench.mjs";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const NOISE_MIX_SCRIPT = join(SCRIPT_DIR, "asr_noise_mix.py");
export const TARGET_SAMPLE_RATE = 16_000;
export const BENCH_RECORDINGS_DIR_REL = "voice-bench/recordings";
export const ONLINE_DIR_NAME = "online";
export const ONLINE_ROTATION_FILE = ".rotation.json";
export const DEFAULT_MAX_ONLINE = 20;
export const ONLINE_REQUIRED_META_FIELDS = ["text", "dataset", "license", "source_url"];
export const RECORDING_APP_HINT =
  "请打开 MOSS 桌面端「语音中心 › 记录与诊断 › AI 联调与测试 › 录制测试语料」录制真人录音后再运行整体测试。";

/** VE4 网上真人语音疑似指令词表（在 `classifyCommand` 与 `PROTECTED_COMMAND_WORDS` 基础上追加媒体/设备/激光控制指令词）。 */
const ONLINE_COMMAND_KEYWORDS = [
  "播放",
  "放首歌",
  "放一首歌",
  "放音乐",
  "来首歌",
  "听首歌",
  "切歌",
  "下一首",
  "上一首",
  "音量",
  "声音调大",
  "声音调小",
  "大声点",
  "小声点",
  "亮度",
  "调亮",
  "调暗",
  "静音",
  "关机",
  "重启",
  "恢复出厂",
  "重新配网",
  "升级固件",
  "开始雕刻",
  "开始切割",
  "直接雕刻",
  "直接切割",
  "确认发送",
  "确认开始",
  "走边框",
  "机械回零",
  "回零",
  "回原点",
  "解除锁定",
  "急停",
  "停机",
  "关掉激光",
  "退下",
  "关闭对话",
  "结束对话",
  "打开灯",
  "关掉灯",
  "开灯",
  "关灯",
  "打开空调",
  "关闭空调",
  "执行命令",
];

const ONLINE_COMMAND_PREFIXES = ["开始", "确认", "停止", "暂停", "取消", "退出"];

/**
 * VE4 判断网上真人语音原话是否属于疑似指令句（可能触发放音乐、调音量/亮度、停止/退出或设备控制）。
 * 返回 `{ blocked: boolean, reason: string }`。
 */
export function isOnlineCommandLikeText(text) {
  const raw = String(text || "").trim();
  const norm = normalizeForCer(raw).join("");
  if (!norm) {
    return { blocked: false, reason: "" };
  }
  const cmdKind = classifyCommand(raw);
  if (cmdKind !== null) {
    return { blocked: true, reason: `命中${cmdKind === "stop" ? "停止" : "退出"}快路径命令` };
  }
  if (PROTECTED_COMMAND_WORDS.has(norm)) {
    return { blocked: true, reason: `命中受保护命令词「${norm}」` };
  }
  const hitKeyword = ONLINE_COMMAND_KEYWORDS.find((kw) => norm.includes(kw));
  if (hitKeyword) {
    return { blocked: true, reason: `包含指令关键词「${hitKeyword}」` };
  }
  const hitPrefix = ONLINE_COMMAND_PREFIXES.find((prefix) => norm.startsWith(prefix));
  if (hitPrefix) {
    return { blocked: true, reason: `以控制命令「${hitPrefix}」开头` };
  }
  return { blocked: false, reason: "" };
}

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

function spawnPlayerProcess(player, wavPath, { stderrMode = "pipe" } = {}) {
  const stdio = ["ignore", "ignore", stderrMode];
  if (player.kind === "powershell") {
    // 经环境变量传入路径，绝不把动态路径拼进 PowerShell -Command（防止命令注入及路径解析失败）
    const script = "$s = New-Object System.Media.SoundPlayer $env:MOSS_PLAY_WAV; $s.PlaySync()";
    return spawn(player.cmd, ["-NoProfile", "-NonInteractive", "-Command", script], {
      stdio,
      env: { ...process.env, MOSS_PLAY_WAV: wavPath },
    });
  }
  if (player.kind === "aplay") {
    return spawn(player.cmd, ["-q", wavPath], { stdio });
  }
  return spawn(player.cmd, [wavPath], { stdio });
}

/** 播放单个 WAV 文件并精确记录本机起止时间戳（与同机服务端时钟一致）。 */
export async function playWav(wavPath, opts = {}) {
  if (typeof opts.playWavFn === "function") {
    return opts.playWavFn(wavPath, opts);
  }
  const player = opts.player || checkPlayerAvailable(opts.platform || process.platform);
  const startedAtMs = Date.now();
  await new Promise((resolvePromise, rejectPromise) => {
    const child = spawnPlayerProcess(player, wavPath, { stderrMode: "pipe" });
    if (typeof opts.onSpawn === "function") opts.onSpawn(child);
    let stderr = "";
    child.stderr?.on("data", (d) => {
      stderr += d.toString("utf8");
    });
    child.on("error", (err) => {
      if (typeof opts.onSpawn === "function") opts.onSpawn(null);
      rejectPromise(err);
    });
    child.on("close", (code) => {
      if (typeof opts.onSpawn === "function") opts.onSpawn(null);
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
    try {
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
    } catch {
      generateNoiseWavWithNode(safePreset, 12, clampedGain, noiseWav, 42);
    }
  }
  const activePlayer = player || checkPlayerAvailable(platform);
  let stopped = false;
  let currentChild = null;

  const loopPlay = () => {
    if (stopped) return;
    currentChild = spawnPlayerProcess(activePlayer, noiseWav, { stderrMode: "ignore" });
    currentChild.on("close", (code) => {
      currentChild = null;
      // 播放进程异常退出（非 0）时停止循环，防止无退避 fork-bomb
      if (code !== 0) {
        stopped = true;
        return;
      }
      if (!stopped) loopPlay();
    });
    currentChild.on("error", () => {
      currentChild = null;
      stopped = true;
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

/** 读取 `<recordingsDir>/online/.rotation.json` 轮换游标（拒绝符号链接）。 */
export function loadOnlineRotation(onlineDir) {
  const rotPath = join(onlineDir, ONLINE_ROTATION_FILE);
  if (!existsSync(rotPath)) return { cursor: 0, last_ids: [] };
  try {
    if (lstatSync(onlineDir).isSymbolicLink() || lstatSync(rotPath).isSymbolicLink()) {
      return { cursor: 0, last_ids: [] };
    }
    const parsed = JSON.parse(readFileSync(rotPath, "utf8")) || {};
    const cursor = Number.isInteger(parsed.cursor) && parsed.cursor >= 0 ? parsed.cursor : 0;
    const lastIds = Array.isArray(parsed.last_ids) ? parsed.last_ids.map(String) : [];
    return { cursor, last_ids: lastIds };
  } catch {
    return { cursor: 0, last_ids: [] };
  }
}

/** 原子保存 `<recordingsDir>/online/.rotation.json` 轮换游标（0600，拒绝符号链接）。 */
export function saveOnlineRotation(onlineDir, { cursor, totalPool, lastIds = [] }) {
  if (!existsSync(onlineDir)) return false;
  if (lstatSync(onlineDir).isSymbolicLink()) return false;
  const rotPath = join(onlineDir, ONLINE_ROTATION_FILE);
  if (existsSync(rotPath) && lstatSync(rotPath).isSymbolicLink()) return false;
  const payload = {
    cursor: Math.max(0, Number(cursor) || 0),
    total_pool: Math.max(0, Number(totalPool) || 0),
    last_ids: lastIds.map(String),
    updated_at: new Date().toISOString(),
  };
  const tmpPath = join(onlineDir, `.rotation.tmp.${process.pid}.${randomBytes(6).toString("hex")}`);
  try {
    writeFileSync(tmpPath, `${JSON.stringify(payload, null, 2)}\n`, { encoding: "utf8", mode: 0o600, flag: "wx" });
    renameSync(tmpPath, rotPath);
    return true;
  } catch {
    try {
      if (existsSync(tmpPath)) unlinkSync(tmpPath);
    } catch {
      // ignore
    }
    return false;
  }
}

/**
 * 扫描 `<runtime>/voice-bench/recordings/<speaker>/<corpus-id>.{wav,json}` 中的真人录音：
 * - `online/` 子目录一律标为 `source_type: "online-human"`（不看 `speaker` 填写值），
 *   同名 `.json` 必须包含 `text`、`dataset`、`license`、`source_url`，禁止 `.txt` 回退，并过滤疑似指令句；
 * - 其他子目录均为用户自录语音，标为 `source_type: "human-recording"`（允许 `.txt` 回退）。
 */
export function loadBenchRecordings(recordingsDir, { speakerFilter = "", maxOnline = DEFAULT_MAX_ONLINE } = {}) {
  const root = resolve(recordingsDir);
  if (!existsSync(root) || !statSync(root).isDirectory()) {
    return {
      recordings: [],
      userRecordings: [],
      onlineRecordings: [],
      speakers: [],
      skippedAssets: [],
      onlineDir: join(root, ONLINE_DIR_NAME),
    };
  }
  const filter = String(speakerFilter || "").trim();
  const onlineDisabled = Number(maxOnline) === 0;
  const recordings = [];
  const userRecordings = [];
  const onlineRecordings = [];
  const skippedAssets = [];
  const allSpeakers = new Set();
  let resolvedOnlineDir = join(root, ONLINE_DIR_NAME);

  const speakerEntries = readdirSync(root, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && !entry.isSymbolicLink())
    .sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));

  for (const dirEntry of speakerEntries) {
    const speakerDirName = dirEntry.name;
    const isOnlineDir = speakerDirName.toLowerCase() === ONLINE_DIR_NAME;
    const speakerPath = join(root, speakerDirName);
    if (isOnlineDir) {
      resolvedOnlineDir = speakerPath;
      if (onlineDisabled) continue;
    }

    const files = readdirSync(speakerPath, { withFileTypes: true })
      .filter((f) => f.isFile() && !f.isSymbolicLink() && f.name.endsWith(".wav"))
      .sort((a, b) => a.name.localeCompare(b.name));

    for (const fileEntry of files) {
      const corpusId = fileEntry.name.slice(0, -4);
      const wavPath = join(speakerPath, fileEntry.name);
      const filename = `${speakerDirName}/${fileEntry.name}`;
      const jsonPath = join(speakerPath, `${corpusId}.json`);
      const txtPath = join(speakerPath, `${corpusId}.txt`);

      if (isOnlineDir) {
        const hasJson = existsSync(jsonPath) && !lstatSync(jsonPath).isSymbolicLink();
        const hasTxt = existsSync(txtPath) && !lstatSync(txtPath).isSymbolicLink();
        if (!hasJson) {
          skippedAssets.push({
            id: corpusId,
            filename,
            source_type: "online-human",
            text: "",
            reason: hasTxt
              ? "online/ 目录禁止仅使用 .txt（缺少包含 text、dataset、license、source_url 的同名 .json）"
              : "online/ 目录缺少同名 .json 元数据（须含 text、dataset、license、source_url）",
          });
          continue;
        }
        let meta = null;
        try {
          meta = JSON.parse(readFileSync(jsonPath, "utf8"));
        } catch {
          skippedAssets.push({
            id: corpusId,
            filename,
            source_type: "online-human",
            text: "",
            reason: "online/ 同名 .json 解析失败，已跳过",
          });
          continue;
        }
        if (!meta || typeof meta !== "object") {
          skippedAssets.push({
            id: corpusId,
            filename,
            source_type: "online-human",
            text: "",
            reason: "online/ 同名 .json 内容不是合法对象，已跳过",
          });
          continue;
        }
        const missingFields = ONLINE_REQUIRED_META_FIELDS.filter(
          (field) => typeof meta[field] !== "string" || !meta[field].trim(),
        );
        if (missingFields.length > 0) {
          skippedAssets.push({
            id: corpusId,
            filename,
            source_type: "online-human",
            text: String(meta.text || "").trim(),
            reason: `online/ 元数据缺少必填字段（${missingFields.join("、")}），不予加载`,
          });
          continue;
        }
        const text = meta.text.trim();
        const dataset = meta.dataset.trim();
        const license = meta.license.trim();
        const sourceUrl = meta.source_url.trim();

        const cmdCheck = isOnlineCommandLikeText(text);
        if (cmdCheck.blocked) {
          skippedAssets.push({
            id: corpusId,
            filename,
            source_type: "online-human",
            dataset,
            license,
            text,
            reason: `网上语音包含疑似指令句（${cmdCheck.reason}），已过滤以防误触发控制`,
          });
          continue;
        }

        const speaker = String(meta.speaker || dataset || "网上真人").trim() || "网上真人";
        allSpeakers.add(speaker);
        if (
          filter &&
          speaker !== filter &&
          speakerDirName !== filter &&
          filter !== "online-human" &&
          dataset !== filter
        ) {
          continue;
        }

        let durationMs = Number(meta.duration_ms) || 0;
        if (!durationMs) {
          try {
            durationMs = wavDurationMs(wavPath);
          } catch {
            durationMs = 0;
          }
        }
        const rec = {
          source_type: "online-human",
          audio_origin: "online-human",
          is_online: true,
          speaker,
          speaker_dir: speakerDirName,
          corpus_id: corpusId,
          wav_path: wavPath,
          filename,
          text,
          dataset,
          license,
          source_url: sourceUrl,
          label: String(meta.label || "").trim(),
          key_terms: Array.isArray(meta.key_terms) ? meta.key_terms : [],
          tags: Array.isArray(meta.tags) ? meta.tags : [],
          duration_ms: durationMs,
          peak_level: Number(meta.peak_level) || 0,
          rms_level: Number(meta.rms_level) || 0,
          recorded_at_ms: Number(meta.recorded_at_ms) || 0,
        };
        onlineRecordings.push(rec);
        recordings.push(rec);
        continue;
      }

      // 用户自录目录：保持 `human-recording`，允许 `.txt` 回退
      let meta = {};
      if (existsSync(jsonPath) && !lstatSync(jsonPath).isSymbolicLink()) {
        try {
          meta = JSON.parse(readFileSync(jsonPath, "utf8")) || {};
        } catch {
          meta = {};
        }
      }
      if (!String(meta.text || "").trim() && existsSync(txtPath) && !lstatSync(txtPath).isSymbolicLink()) {
        try {
          const txtLine = readFileSync(txtPath, "utf8")
            .split(/\r?\n/)
            .map((line) => line.trim())
            .find(Boolean);
          if (txtLine) meta.text = txtLine;
        } catch {
          // 忽略无法读取的 .txt
        }
      }
      const speaker = String(meta.speaker || speakerDirName).trim() || speakerDirName;
      allSpeakers.add(speaker);
      if (filter && speaker !== filter && speakerDirName !== filter && filter !== "human-recording") continue;

      let durationMs = Number(meta.duration_ms) || 0;
      if (!durationMs) {
        try {
          durationMs = wavDurationMs(wavPath);
        } catch {
          durationMs = 0;
        }
      }
      const rec = {
        source_type: "human-recording",
        audio_origin: "human-recording",
        is_online: false,
        speaker,
        speaker_dir: speakerDirName,
        corpus_id: corpusId,
        wav_path: wavPath,
        filename,
        text: String(meta.text || "").trim(),
        dataset: null,
        license: null,
        source_url: null,
        label: String(meta.label || "").trim(),
        key_terms: Array.isArray(meta.key_terms) ? meta.key_terms : [],
        tags: Array.isArray(meta.tags) ? meta.tags : [],
        duration_ms: durationMs,
        peak_level: Number(meta.peak_level) || 0,
        rms_level: Number(meta.rms_level) || 0,
        recorded_at_ms: Number(meta.recorded_at_ms) || 0,
      };
      userRecordings.push(rec);
      recordings.push(rec);
    }
  }

  return {
    recordings,
    userRecordings,
    onlineRecordings,
    speakers: [...allSpeakers].sort((a, b) => a.localeCompare(b, "zh-CN")),
    skippedAssets,
    onlineDir: resolvedOnlineDir,
  };
}

function buildRecordingAsset(chosen) {
  const isOnline = chosen.source_type === "online-human" || chosen.is_online === true;
  const sourceType = isOnline ? "online-human" : "human-recording";
  const label = isOnline
    ? `网上真人（${chosen.dataset || chosen.speaker}）`
    : `我的录音（${chosen.speaker}）`;
  return {
    wavPath: chosen.wav_path,
    filename: chosen.filename,
    source_type: sourceType,
    audio_origin: sourceType,
    speaker: chosen.speaker,
    secondary_recording: false,
    dataset: isOnline ? chosen.dataset || null : null,
    license: isOnline ? chosen.license || null : null,
    source_url: isOnline ? chosen.source_url || null : null,
    voice_profile: {
      source: sourceType,
      audio_origin: sourceType,
      speaker: chosen.speaker,
      corpus_id: chosen.corpus_id,
      ...(isOnline && chosen.dataset ? { dataset: chosen.dataset } : {}),
      ...(isOnline && chosen.license ? { license: chosen.license } : {}),
      ...(isOnline && chosen.source_url ? { source_url: chosen.source_url } : {}),
      peak_level: chosen.peak_level,
      label,
    },
    duration_ms: chosen.duration_ms || wavDurationMs(chosen.wav_path),
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
 * VD1 / VD3 / VE1 / VE3 / VE4 准备整体测试真人录音素材（绝不使用本地 TTS 或系统朗读）：
 * 1. 用户自录语音（`recordings/<speaker>/`，非 `online`）标为 `human-recording`；
 * 2. 网上真人语音（`recordings/online/`）一律标为 `online-human`，强制校验 `text/dataset/license/source_url`，
 *    过滤疑似指令句，并按 `maxOnline`（默认 20，0 表示不用）与 `.rotation.json` 轮换抽取；
 * 3. 标准语料优先匹配用户自录语音，其次匹配本轮抽中的网上真人语音，缺失记入 `missingTurns`；
 * 4. `includeDeviceCorpus` 为 `true` 时才允许用 `<runtime>/asr-corpus` 补充并显式标为 `secondary_recording: true`。
 */
export async function prepareVoiceAssets({
  turns,
  calibrationText = "",
  runtimeDir,
  recordingsDir = "",
  corpusDir = "",
  speaker = "",
  includeDeviceCorpus = false,
  maxOnline = DEFAULT_MAX_ONLINE,
  persistRotation = true,
}) {
  const resolvedRecordingsDir = resolve(recordingsDir || join(runtimeDir, BENCH_RECORDINGS_DIR_REL));
  const resolvedCorpusDir = resolve(corpusDir || join(runtimeDir, "asr-corpus"));
  const speakerFilter = String(speaker || "").trim();
  const parsedMaxOnline =
    maxOnline === undefined || maxOnline === null ? DEFAULT_MAX_ONLINE : Math.max(0, Math.floor(Number(maxOnline) || 0));

  const {
    userRecordings,
    onlineRecordings,
    speakers: availableSpeakers,
    skippedAssets,
    onlineDir,
  } = loadBenchRecordings(resolvedRecordingsDir, {
    speakerFilter,
    maxOnline: parsedMaxOnline,
  });

  // VE3：对 `online/` 有效池按 `.rotation.json` 轮换抽取至多 `maxOnline` 条，避免每次重复测同一批
  const sortedOnlinePool = [...onlineRecordings].sort((a, b) => a.corpus_id.localeCompare(b.corpus_id));
  const rotationBefore = loadOnlineRotation(onlineDir);
  let selectedOnline = [];
  let cursorBefore = 0;
  let cursorAfter = rotationBefore.cursor;
  if (parsedMaxOnline > 0 && sortedOnlinePool.length > 0) {
    const total = sortedOnlinePool.length;
    cursorBefore = ((rotationBefore.cursor % total) + total) % total;
    const take = Math.min(parsedMaxOnline, total);
    for (let k = 0; k < take; k += 1) {
      selectedOnline.push(sortedOnlinePool[(cursorBefore + k) % total]);
    }
    cursorAfter = (cursorBefore + take) % total;
  }
  const onlineRotation = {
    onlineDir,
    maxOnline: parsedMaxOnline,
    totalPool: sortedOnlinePool.length,
    selectedCount: selectedOnline.length,
    cursorBefore,
    cursorAfter,
    selectedIds: selectedOnline.map((r) => r.corpus_id),
    persisted: false,
  };
  if (persistRotation && selectedOnline.length > 0) {
    onlineRotation.persisted = saveOnlineRotation(onlineDir, {
      cursor: cursorAfter,
      totalPool: sortedOnlinePool.length,
      lastIds: onlineRotation.selectedIds,
    });
  }

  // 仅当显式开启 --include-device-corpus 时才加载小智麦克风采集的 asr-corpus（排除 e2e-playback 防二次套娃）
  const deviceSamples = includeDeviceCorpus
    ? loadLabeledCorpus(resolvedCorpusDir).filter((s) => s.label_source !== "e2e-playback")
    : [];

  const preparedTurns = [];
  const missingTurns = [];
  const usedDeviceCorpusIds = new Set();
  const matchedUserCorpusIds = new Set();
  const matchedUserNormTexts = new Set();
  const matchedOnlineCorpusIds = new Set();
  const matchedOnlineNormTexts = new Set();
  const speakersUsed = new Set();
  let roundRobinCursor = 0;

  for (let i = 0; i < turns.length; i += 1) {
    const spec = turns[i];
    const targetNorm = normalizeForCer(spec.text).join("");

    // 优先匹配用户自录语音，其次匹配本轮抽中的网上真人语音
    let candidates = userRecordings.filter((r) => r.corpus_id === spec.id);
    if (candidates.length === 0 && targetNorm) {
      candidates = userRecordings.filter((r) => normalizeForCer(r.text).join("") === targetNorm);
    }
    let matchedFromOnline = false;
    if (candidates.length === 0 && selectedOnline.length > 0) {
      candidates = selectedOnline.filter((r) => r.corpus_id === spec.id);
      if (candidates.length === 0 && targetNorm) {
        candidates = selectedOnline.filter((r) => normalizeForCer(r.text).join("") === targetNorm);
      }
      matchedFromOnline = candidates.length > 0;
    }

    if (candidates.length > 0) {
      for (const c of candidates) {
        if (matchedFromOnline) {
          matchedOnlineCorpusIds.add(c.corpus_id);
          const cn = normalizeForCer(c.text).join("");
          if (cn) matchedOnlineNormTexts.add(cn);
        } else {
          matchedUserCorpusIds.add(c.corpus_id);
          const cn = normalizeForCer(c.text).join("");
          if (cn) matchedUserNormTexts.add(cn);
        }
      }
      if (targetNorm) {
        if (matchedFromOnline) matchedOnlineNormTexts.add(targetNorm);
        else matchedUserNormTexts.add(targetNorm);
      }
      const chosen = candidates[roundRobinCursor % candidates.length];
      roundRobinCursor += 1;
      speakersUsed.add(chosen.speaker);
      preparedTurns.push({
        spec,
        asset: buildRecordingAsset(chosen),
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
            audio_origin: "device-corpus-secondary",
            speaker: secondarySpeaker,
            secondary_recording: true,
            dataset: null,
            license: null,
            source_url: null,
            voice_profile: {
              source: "device-corpus-secondary",
              audio_origin: "device-corpus-secondary",
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

  // 用户自录目录中未匹配到固定语料的自由句子（source_type: "human-recording"）
  const freeformUserGroups = new Map();
  for (const rec of userRecordings) {
    if (!rec.text) continue;
    const recNorm = normalizeForCer(rec.text).join("");
    if (matchedUserCorpusIds.has(rec.corpus_id)) continue;
    if (recNorm && matchedUserNormTexts.has(recNorm)) continue;
    if (!freeformUserGroups.has(rec.corpus_id)) {
      freeformUserGroups.set(rec.corpus_id, []);
    }
    freeformUserGroups.get(rec.corpus_id).push(rec);
  }
  const freeformItems = [];
  for (const [corpusId, group] of freeformUserGroups.entries()) {
    const chosen = group[roundRobinCursor % group.length];
    roundRobinCursor += 1;
    speakersUsed.add(chosen.speaker);
    freeformItems.push({
      spec: {
        id: corpusId,
        label: chosen.label || `我的录音 · ${corpusId}`,
        group: "我的录音",
        text: chosen.text,
        ...(chosen.key_terms?.length ? { key_terms: chosen.key_terms } : {}),
        ...(chosen.tags?.length ? { tags: chosen.tags } : {}),
      },
      asset: buildRecordingAsset(chosen),
    });
  }

  // 本轮轮换抽中的网上真人语音自由句子（source_type: "online-human"）
  for (const chosen of selectedOnline) {
    const recNorm = normalizeForCer(chosen.text).join("");
    if (matchedOnlineCorpusIds.has(chosen.corpus_id)) continue;
    if (recNorm && matchedOnlineNormTexts.has(recNorm)) continue;
    speakersUsed.add(chosen.speaker);
    freeformItems.push({
      spec: {
        id: chosen.corpus_id,
        label: chosen.label || `网上真人 · ${chosen.dataset} · ${chosen.corpus_id}`,
        group: "网上真人",
        text: chosen.text,
        ...(chosen.key_terms?.length ? { key_terms: chosen.key_terms } : {}),
        ...(chosen.tags?.length ? { tags: chosen.tags } : {}),
      },
      asset: buildRecordingAsset(chosen),
    });
  }

  if (freeformItems.length > 0) {
    const exitIdx = preparedTurns.findIndex((item) => item.spec.expect_route === "fast_exit");
    const insertAt = exitIdx >= 0 ? exitIdx : preparedTurns.length;
    preparedTurns.splice(insertAt, 0, ...freeformItems);
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
          audio_origin: "device-corpus-secondary",
          speaker: secondarySpeaker,
          secondary_recording: true,
          dataset: null,
          license: null,
          source_url: null,
          voice_profile: {
            source: "device-corpus-secondary",
            audio_origin: "device-corpus-secondary",
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

  // 校准同样只用真人录音（优先用户自录语音，其次网上真人语音，最后二次录制）
  let calibrationAsset = null;
  let calibrationEffectiveText = calibrationText;
  if (calibrationText && preparedTurns.length > 0) {
    const calNorm = normalizeForCer(calibrationText).join("");
    const exactUserTurn = preparedTurns.find(
      (item) =>
        item.asset.source_type === "human-recording" &&
        (normalizeForCer(item.spec.text).join("") === calNorm || item.spec.id === "status_fast_read"),
    );
    const exactOnlineTurn = preparedTurns.find(
      (item) =>
        item.asset.source_type === "online-human" &&
        (normalizeForCer(item.spec.text).join("") === calNorm || item.spec.id === "status_fast_read"),
    );
    const fallbackTurn =
      exactUserTurn ||
      preparedTurns.find((item) => item.asset.source_type === "human-recording") ||
      exactOnlineTurn ||
      preparedTurns.find((item) => item.asset.source_type === "online-human") ||
      preparedTurns[0];
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
    skippedAssets,
    onlineRotation,
    speakersUsed: [...speakersUsed],
    availableSpeakers,
    recordingsDir: resolvedRecordingsDir,
  };
}
