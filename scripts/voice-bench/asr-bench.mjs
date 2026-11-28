#!/usr/bin/env node
// VB3 离线识别（ASR）自动化测评与候选词产出脚本（供 AI 与开发者在本机直接执行）：
// - 输入已标注的真实识别语料目录（<runtime>/asr-corpus/<date>/<turn-id>.{wav,label.json}）；
// - 复用 chain_clips.py（经 `python3 -I`）对语料叠加排风扇、气泵突发/持续噪声档位；
// - 对所有已安装本地模型（复用 moss-voice-local 帧协议）与已配置云端模型逐条运行识别；
// - 在 `<runtime>/voice-bench/asr-<run-id>/` 输出脱敏归档：results.jsonl、report.md、suggestions.json；
// - 严格执行命令词保护规则：禁止产出任何把别的话映射成停止/退出命令或改写命令词本身的同音纠错候选；
// - 候选只落盘，绝不自动修改代码或 settings.json；无已标注语料时提示先采集标注并返回非 0 状态码。

import { execFileSync, spawn } from "node:child_process";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  defaultRuntimeDir,
  percentile,
  redactText,
  redactValue,
  registerSignalCleanup,
  resolveProcessTraceDir,
} from "./bench-common.mjs";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const NOISE_MIX_SCRIPT = join(SCRIPT_DIR, "asr_noise_mix.py");
const LOCAL_PROTO_VERSION = 9;

export const NO_LABELED_CORPUS_MESSAGE =
  "未找到已标注的识别语料：请先在 MOSS 语音中心「记录与诊断 › 对话记录与排障」开启“同时保存完整过程记录与原始录音”，对着小智说几句话并点“✓ 识别正确”或“纠偏此句”完成真值标注后再运行测评。";

/** 本地 ASR 模型清单（对齐 crates/moss-voice/src/local_models.rs CATALOG 与 moss-voice-local-proto AsrEngine::streaming）。 */
export const LOCAL_ASR_CATALOG = [
  {
    id: "sense_voice",
    label: "SenseVoice 2024",
    engine: "sense_voice",
    streaming: true,
    dir: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17",
    required: ["model.int8.onnx", "tokens.txt"],
  },
  {
    id: "sense_voice_2025",
    label: "SenseVoice 2025",
    engine: "sense_voice",
    streaming: true,
    dir: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09",
    required: ["model.int8.onnx", "tokens.txt"],
  },
  {
    id: "x_asr_streaming",
    label: "X-ASR 流式识别",
    engine: "streaming_transducer",
    streaming: true,
    dir: "sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05",
    required: ["encoder.int8.onnx", "decoder.onnx", "joiner.int8.onnx", "tokens.txt"],
  },
  {
    id: "qwen3_asr_0_6b",
    label: "Qwen3-ASR 0.6B",
    engine: "qwen3_asr",
    streaming: false,
    dir: "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25",
    required: ["conv_frontend.onnx", "encoder.int8.onnx", "decoder.int8.onnx", "tokenizer"],
  },
  {
    id: "firered_asr2_ctc",
    label: "FireRedASR2-CTC",
    engine: "fire_red_asr2_ctc",
    streaming: true,
    dir: "sherpa-onnx-fire-red-asr2-ctc-zh_en-int8-2026-02-25",
    required: ["model.int8.onnx", "tokens.txt"],
  },
];

export const NOISE_PRESETS = {
  clean: 0,
  quiet: 0,
  fan: 888,
  machine: 888,
  pump: 999,
  burst: 999,
};

export function noiseLabel(level) {
  if (level === 0) return "安静 (0)";
  if (level === 888) return "排风扇/气泵持续噪声 (888)";
  if (level === 999) return "气泵咔嗒/敲击突发噪声 (999)";
  return `白噪 (${level})`;
}

/** 对齐 moss-voice-server/src/fast.rs、conversation.rs 与 moss-voice/src/settings.rs 的停止/退出与安全保护词。 */
const STOP_CORE_WORDS = ["停", "停止", "停下", "停机", "急停", "快停", "别打了", "关掉激光"];
const STOP_SUFFIXES = ["吧", "啊", "了", "一下", "快"];
const EXIT_CORE_WORDS = [
  "退出",
  "再见",
  "拜拜",
  "退下",
  "退一下",
  "休息",
  "去休息",
  "没事了",
  "不聊了",
  "关闭对话",
  "结束对话",
];
const EXIT_PREFIXES = [
  "你好小智",
  "小智同学",
  "小智",
  "好了",
  "好的",
  "好吧",
  "行了",
  "可以了",
  "那就",
  "那你",
  "那",
  "你可以",
  "你先",
  "你就",
  "你",
  "先",
];
const EXIT_SUFFIXES = ["去吧", "吧", "了", "啊", "啦", "呀", "哦", "咯"];
export const PROTECTED_COMMAND_WORDS = new Set([
  "停",
  "急停",
  "停止",
  "停下",
  "停机",
  "立刻停",
  "马上停",
  "快停",
  "别打了",
  "关掉激光",
  "取消",
  "停止加工",
  "暂停",
  "退出",
  "再见",
  "拜拜",
  "退下",
  "退一下",
  "休息",
  "去休息",
  "没事了",
  "不聊了",
  "关闭对话",
  "结束对话",
  "确认",
  "开始",
]);

/** 完整命令复合短语集合（含核心词与常用尾缀），用于拦截可拼成停止/退出命令的子串/前缀候选（如 `听一 -> 停一`、`推一 -> 退一`、`关掉鸡 -> 关掉激`）。 */
const COMMAND_COMPOUND_PHRASES = [
  ...STOP_CORE_WORDS,
  ...STOP_CORE_WORDS.flatMap((w) => STOP_SUFFIXES.map((s) => `${w}${s}`)),
  ...EXIT_CORE_WORDS,
  ...EXIT_CORE_WORDS.flatMap((w) => EXIT_SUFFIXES.map((s) => `${w}${s}`)),
  "立刻停",
  "马上停",
  "停止加工",
];

const MOTION_GCODE_RE = /(?:^|[^A-Za-z0-9])(?:G00?|G0?1|G0?2|G0?3|G28|M0?3|M0?4|M0?5)(?=$|[^0-9])|\$\d+=/i;

/** 激光工作室核心材料与术语表（用于准确率与热词候选统计）。 */
export const DEFAULT_WORKSHOP_TERMS = [
  "椴木",
  "亚克力",
  "牛皮纸",
  "胶合板",
  "不锈钢",
  "阳极氧化铝",
  "皮革",
  "竹木",
  "松木",
  "胡桃木",
  "翼宿",
  "激光机",
  "激光",
  "雕刻",
  "切割",
  "走边框",
  "对焦",
  "功率",
  "速度",
  "倍率",
  "原点",
  "回零",
  "预览",
  "刀路",
  "木牌",
  "铭牌",
  "排风扇",
  "气泵",
  "毫米",
];

/** 归一化为可比对字符序列（NFKC、小写、仅保留字母/数字/CJK）。 */
export function normalizeForCer(text) {
  const s = String(text ?? "").normalize("NFKC").toLowerCase();
  const chars = [];
  for (const ch of s) {
    if (/[\p{L}\p{N}]/u.test(ch)) chars.push(ch);
  }
  return chars;
}

/** 字符级 Levenshtein 编辑距离与最优对齐路径（供 CER 计算与同音替换提取）。 */
export function computeCer(refText, hypText) {
  const r = normalizeForCer(refText);
  const h = normalizeForCer(hypText);
  const m = r.length;
  const n = h.length;
  const dp = Array.from({ length: m + 1 }, () => new Int32Array(n + 1));
  for (let i = 0; i <= m; i += 1) dp[i][0] = i;
  for (let j = 0; j <= n; j += 1) dp[0][j] = j;
  for (let i = 1; i <= m; i += 1) {
    for (let j = 1; j <= n; j += 1) {
      const cost = r[i - 1] === h[j - 1] ? 0 : 1;
      dp[i][j] = Math.min(dp[i - 1][j] + 1, dp[i][j - 1] + 1, dp[i - 1][j - 1] + cost);
    }
  }
  // 回溯对齐操作（eq / sub / del / ins）
  const alignment = [];
  let i = m;
  let j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && r[i - 1] === h[j - 1] && dp[i][j] === dp[i - 1][j - 1]) {
      alignment.push({ op: "eq", ref: r[i - 1], hyp: h[j - 1] });
      i -= 1;
      j -= 1;
    } else if (i > 0 && j > 0 && dp[i][j] === dp[i - 1][j - 1] + 1) {
      alignment.push({ op: "sub", ref: r[i - 1], hyp: h[j - 1] });
      i -= 1;
      j -= 1;
    } else if (i > 0 && dp[i][j] === dp[i - 1][j] + 1) {
      alignment.push({ op: "del", ref: r[i - 1], hyp: "" });
      i -= 1;
    } else {
      alignment.push({ op: "ins", ref: "", hyp: h[j - 1] });
      j -= 1;
    }
  }
  alignment.reverse();
  const edits = dp[m][n];
  const cer = m > 0 ? edits / m : n > 0 ? 1 : 0;
  return { edits, refLen: m, hypLen: n, cer, alignment };
}

function matchesRepeatedWord(candidate, words) {
  return words.some((word) => {
    if (!candidate.startsWith(word)) return false;
    const rest = candidate.slice(word.length);
    return rest === "" || rest === word || rest === `${word}${word}`;
  });
}

/** 判断一句话是否会被 MOSS 当作停止或退出快路径命令（对齐 fast::is_stop 与 Conversation::is_exit）。 */
export function classifyCommand(text) {
  let core = normalizeForCer(text).join("");
  if (!core) return null;
  if (
    matchesRepeatedWord(core, STOP_CORE_WORDS) ||
    STOP_SUFFIXES.some((end) => core.endsWith(end) && matchesRepeatedWord(core.slice(0, -end.length), STOP_CORE_WORDS))
  ) {
    return "stop";
  }
  if (matchesRepeatedWord(core, EXIT_CORE_WORDS)) {
    return "exit";
  }
  for (let step = 0; step < 3; step += 1) {
    const matchedPrefix = EXIT_PREFIXES.find((p) => core.startsWith(p));
    if (!matchedPrefix) break;
    core = core.slice(matchedPrefix.length);
    if (matchesRepeatedWord(core, EXIT_CORE_WORDS)) return "exit";
  }
  for (let step = 0; step < 2; step += 1) {
    const matchedSuffix = EXIT_SUFFIXES.find((s) => core.endsWith(s));
    if (!matchedSuffix) break;
    core = core.slice(0, -matchedSuffix.length);
    if (matchesRepeatedWord(core, EXIT_CORE_WORDS)) return "exit";
  }
  return null;
}

function touchesProtectedCommandFragment(norm) {
  if (!norm) return false;
  if (PROTECTED_COMMAND_WORDS.has(norm)) return true;
  if (norm.includes("停")) return true;
  if ([...PROTECTED_COMMAND_WORDS].some((cmd) => cmd.length >= 2 && norm.includes(cmd))) {
    return true;
  }
  // 拦截长度 >= 2 且为任一停止/退出复合命令前缀或子串的候选（如 `停一`、`退一`、`关掉激`）
  if ([...norm].length >= 2 && COMMAND_COMPOUND_PHRASES.some((phrase) => phrase.includes(norm))) {
    return true;
  }
  return false;
}

/**
 * VB3 命令词保护安全规则（对齐 settings.rs PROTECTED_STOP_WORDS 与 conversation.rs 急停原样保护）：
 * - 严禁把任何内容映射成停止、退出、确认或开始等命令词（含可拼成复合命令词的片段）；
 * - 严禁改写命令词本身；
 * - 严禁包含 `=`、控制字符、危险 G-code / GRBL `$n=` 指令。
 */
export function isSafeHomophoneCandidate(wrongRaw, rightRaw) {
  const wrong = String(wrongRaw ?? "").trim();
  const right = String(rightRaw ?? "").trim();
  if (!wrong || !right || wrong === right) {
    return { allowed: false, reason: "空词或相同词" };
  }
  if ([...wrong].length > 24 || [...right].length > 24) {
    return { allowed: false, reason: "超出单侧 24 字符上限" };
  }
  if (/[\p{Cc}=]/u.test(wrong) || /[\p{Cc}=]/u.test(right)) {
    return { allowed: false, reason: "候选词包含非法分隔符或控制字符" };
  }
  const wrongNorm = normalizeForCer(wrong).join("");
  const rightNorm = normalizeForCer(right).join("");
  if (classifyCommand(wrong) !== null || touchesProtectedCommandFragment(wrongNorm)) {
    return { allowed: false, reason: `禁止改写安全保护命令词（原词「${wrong}」）` };
  }
  if (classifyCommand(right) !== null || touchesProtectedCommandFragment(rightNorm)) {
    return { allowed: false, reason: `禁止将任何词映射为停止/退出等命令词（目标「${right}」）` };
  }
  if (MOTION_GCODE_RE.test(right)) {
    return { allowed: false, reason: "目标词禁止包含 G-code 或控制器 $n= 指令" };
  }
  return { allowed: true };
}

const CN_DIGIT = { 零: 0, 〇: 0, 一: 1, 二: 2, 两: 2, 三: 3, 四: 4, 五: 5, 六: 6, 七: 7, 八: 8, 九: 9 };
const CN_UNIT_MUL = { 十: 10, 百: 100, 千: 1000, 万: 10000 };

/** 将中文口语数字（如“四百”“一千五”“六十”“三点五”）转为阿拉伯数字字符串。 */
export function parseChineseNumber(raw) {
  const s = String(raw ?? "").trim();
  if (!s) return null;
  if (/^\d+(?:\.\d+)?$/.test(s)) return s;
  const parts = s.split("点");
  if (parts.length > 2) return null;
  const intPart = parts[0];
  let total = 0;
  let section = 0;
  let num = 0;
  let hasNum = false;
  let lastUnit = 0;
  for (const ch of intPart) {
    if (Object.hasOwn(CN_DIGIT, ch)) {
      num = CN_DIGIT[ch];
      hasNum = true;
    } else if (Object.hasOwn(CN_UNIT_MUL, ch)) {
      const mul = CN_UNIT_MUL[ch];
      if (mul === 10000) {
        section = (section + (hasNum ? num : 1)) * 10000;
        total += section;
        section = 0;
        num = 0;
        hasNum = false;
        lastUnit = 10000;
      } else {
        const val = hasNum ? num : mul === 10 && section === 0 ? 1 : 0;
        section += val * mul;
        num = 0;
        hasNum = false;
        lastUnit = mul;
      }
    } else {
      return null;
    }
  }
  // 口语缩略：“一千五”→1500，“两百五”→250
  if (hasNum && lastUnit >= 100 && intPart.length >= 2 && !intPart.endsWith(`零${intPart[intPart.length - 1]}`)) {
    section += num * (lastUnit / 10);
  } else if (hasNum) {
    section += num;
  }
  total += section;
  if (parts.length === 2) {
    let frac = "";
    for (const ch of parts[1]) {
      if (!Object.hasOwn(CN_DIGIT, ch)) return null;
      frac += String(CN_DIGIT[ch]);
    }
    return frac ? `${total}.${frac}` : String(total);
  }
  return String(total);
}

const UNIT_CANONICAL = {
  毫米: "mm",
  mm: "mm",
  厘米: "cm",
  cm: "cm",
  米: "m",
  瓦: "W",
  w: "W",
  遍: "遍",
  次: "次",
  秒: "s",
  s: "s",
  分钟: "min",
  "%": "%",
  "％": "%",
};

const NUM_UNIT_RE =
  /(百分之)([零〇一二两三四五六七八九十百千万点]+|\d+(?:\.\d+)?)|(\d+(?:\.\d+)?|[零〇一二两三四五六七八九十百千万点]+)\s*(毫米|mm|厘米|cm|瓦|W|w|遍|次|秒|分钟|%|％)?/gi;

/** 提取文本中的全部数字/带单位表达，并归一化为 `{ raw, canonical, hasUnit }`。 */
export function extractNumbersAndUnits(text) {
  const s = String(text ?? "").normalize("NFKC");
  const out = [];
  NUM_UNIT_RE.lastIndex = 0;
  let m;
  while ((m = NUM_UNIT_RE.exec(s)) !== null) {
    const raw = m[0].trim();
    if (!raw) continue;
    if (m[1]) {
      const val = parseChineseNumber(m[2]);
      if (val !== null) {
        out.push({ raw, canonical: `${val}%`, value: val, unit: "%", isChinese: !/^\d/.test(m[2]) });
      }
      continue;
    }
    const numStr = m[3];
    const unitRaw = m[4] || "";
    // 跳过“一点”“一下”“一般”“一直”“一起”等非度量惯用词中的孤立单字“一”
    if (numStr === "一" && !unitRaw) continue;
    if (numStr === "点") continue;
    const val = parseChineseNumber(numStr);
    if (val === null) continue;
    const unit = unitRaw ? UNIT_CANONICAL[unitRaw.toLowerCase()] || UNIT_CANONICAL[unitRaw] || unitRaw : "";
    const canonical = unit ? (unit === "%" ? `${val}%` : `${val} ${unit}`) : val;
    out.push({ raw, canonical, value: val, unit, isChinese: !/^\d/.test(numStr) || /[毫米厘米瓦秒分钟％]/.test(unitRaw) });
  }
  return out;
}

/** 比较参考句与识别句中的数字和单位准确率，并提取“口语中文数字/单位 → 规范数字/单位”候选。 */
export function evaluateNumberAndUnitAccuracy(refText, hypText) {
  const refItems = extractNumbersAndUnits(refText);
  const hypItems = extractNumbersAndUnits(hypText);
  if (refItems.length === 0) {
    return { total: 0, matched: 0, normCandidates: [] };
  }
  const used = new Set();
  let matched = 0;
  const normCandidates = [];
  for (const r of refItems) {
    const idx = hypItems.findIndex((h, k) => !used.has(k) && h.canonical === r.canonical);
    if (idx >= 0) {
      used.add(idx);
      matched += 1;
      const h = hypItems[idx];
      if (h.isChinese && h.raw !== r.canonical) {
        normCandidates.push({ raw: h.raw, normalized: r.canonical });
      }
    }
  }
  return { total: refItems.length, matched, normCandidates };
}

/** 从单条对齐结果提取替换块 `(wrong -> right)`；若命中领域术语边界则扩展为完整词对（如 `段木->椴木`）。 */
export function extractSubstitutionSpans(alignment, terms = DEFAULT_WORKSHOP_TERMS) {
  const refChars = [];
  const hypChars = [];
  const refToAlign = [];
  const alignRefIdx = [];
  alignment.forEach((step, aIdx) => {
    alignRefIdx.push(refChars.length);
    if (step.ref) {
      refToAlign.push(aIdx);
      refChars.push(step.ref);
    }
    if (step.hyp) hypChars.push(step.hyp);
  });
  const refStr = refChars.join("");

  // 标记哪些对齐位置已被领域术语完整替换覆盖，避免重复拆出单字碎片
  const coveredAlign = new Set();
  const spans = [];
  const normTerms = terms
    .map((t) => normalizeForCer(t).join(""))
    .filter((t) => t.length >= 2)
    .sort((a, b) => b.length - a.length);

  for (const term of normTerms) {
    let from = 0;
    while (from < refStr.length) {
      const pos = refStr.indexOf(term, from);
      if (pos < 0) break;
      from = pos + term.length;
      const aStart = refToAlign[pos];
      const aEnd = refToAlign[pos + term.length - 1];
      if (aStart === undefined || aEnd === undefined) continue;
      let hasDiff = false;
      let hypSlice = "";
      let alreadyCovered = false;
      for (let k = aStart; k <= aEnd; k += 1) {
        if (coveredAlign.has(k)) {
          alreadyCovered = true;
          break;
        }
        if (alignment[k].op !== "eq") hasDiff = true;
        hypSlice += alignment[k].hyp;
      }
      if (!alreadyCovered && hasDiff && hypSlice && hypSlice !== term) {
        for (let k = aStart; k <= aEnd; k += 1) coveredAlign.add(k);
        spans.push({ wrong: hypSlice, right: term });
      }
    }
  }

  let refBuf = "";
  let hypBuf = "";
  const flush = () => {
    if (refBuf && hypBuf && refBuf !== hypBuf) {
      spans.push({ wrong: hypBuf, right: refBuf });
    }
    refBuf = "";
    hypBuf = "";
  };
  alignment.forEach((step, aIdx) => {
    if (step.op === "eq" || coveredAlign.has(aIdx)) {
      flush();
    } else {
      refBuf += step.ref;
      hypBuf += step.hyp;
    }
  });
  flush();
  return spans;
}

function formatRunId(date = new Date()) {
  const pad = (n, w = 2) => String(n).padStart(w, "0");
  return `asr-${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
}

/** 扫描 `<corpus-dir>` 及可选的完整过程记录目录 `<trace-dir>` 下全部已标注语料（含 `<turn-id>.label.json` 与 `<turn-id>.wav`）。 */
export function loadLabeledCorpus(corpusDir, traceDir = null) {
  const samples = [];
  const seenKeys = new Set();
  const walk = (dir, relDate = "") => {
    if (!dir || !existsSync(dir)) return;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) {
        const nextDate = /^\d{4}-\d{2}-\d{2}$/.test(entry.name) ? entry.name : relDate;
        walk(full, nextDate || entry.name);
        continue;
      }
      if (!entry.isFile() || !entry.name.endsWith(".label.json")) continue;
      const stem = entry.name.slice(0, -".label.json".length);
      const wavPath = join(dir, `${stem}.wav`);
      if (!existsSync(wavPath)) continue;
      try {
        if (lstatSync(wavPath).isSymbolicLink()) continue;
        const label = JSON.parse(readFileSync(full, "utf8"));
        const referenceText = String(label?.reference_text ?? "").trim();
        if (!referenceText) continue;
        const metaPath = join(dir, `${stem}.json`);
        const meta =
          existsSync(metaPath) && !lstatSync(metaPath).isSymbolicLink()
            ? JSON.parse(readFileSync(metaPath, "utf8"))
            : {};
        if (meta?.simulated === true) continue;
        const turnId = String(label?.turn_id || stem);
        const date = String(relDate || label?.date || "");
        const dedupKey = `${date}/${turnId}`;
        if (seenKeys.has(dedupKey)) continue;
        seenKeys.add(dedupKey);
        const labelSource = String(
          label?.label_source || (label?.input_source === "e2e_playback" ? "e2e-playback" : "manual"),
        );
        const inputSource = String(
          label?.input_source || (labelSource === "e2e-playback" || meta?.bench_window ? "e2e_playback" : "live_mic"),
        );
        const rawOrigin = String(
          label?.audio_origin ||
            label?.voice_profile?.audio_origin ||
            label?.voice_profile?.source ||
            "",
        ).trim();
        const audioOrigin =
          rawOrigin === "online-human" ||
          rawOrigin === "human-recording" ||
          rawOrigin === "device-corpus-secondary"
            ? rawOrigin
            : null;
        const dataset = String(label?.dataset || label?.voice_profile?.dataset || "").trim() || null;
        const license = String(label?.license || label?.voice_profile?.license || "").trim() || null;
        samples.push({
          id: `${date || "corpus"}/${turnId}`,
          date,
          turn_id: turnId,
          wav_path: wavPath,
          reference_text: referenceText,
          tags: Array.isArray(label?.tags) ? label.tags.map(String) : [],
          label_source: labelSource,
          input_source: inputSource,
          audio_origin: audioOrigin,
          dataset,
          license,
          voice_profile: label?.voice_profile ?? null,
          bench_window: Boolean(meta?.bench_window || inputSource === "e2e_playback"),
          original_raw_text: String(meta?.raw_text ?? label?.raw_text ?? ""),
          original_model: String(meta?.asr_model ?? ""),
        });
      } catch {
        // 忽略损坏的单条标注文件
      }
    }
  };
  walk(corpusDir);
  if (traceDir) walk(traceDir);
  samples.sort((a, b) => a.id.localeCompare(b.id));
  return samples;
}

function clampInt16(val) {
  const n = Math.trunc(val);
  if (n < -32768) return -32768;
  if (n > 32767) return 32767;
  return n;
}

/** 确定性伪随机数发生器（Mulberry32 + Box-Muller 高斯），用于无 python3 时的纯 Node 混噪兜底。 */
function createSeededRng(seed) {
  let state = (Number(seed) || 0) >>> 0;
  let spareGauss = null;
  const next = () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  return {
    next,
    randint(min, max) {
      return min + Math.floor(next() * (max - min + 1));
    },
    uniform(min, max) {
      return min + next() * (max - min);
    },
    gauss(mean = 0, sigma = 1) {
      if (spareGauss !== null) {
        const v = spareGauss;
        spareGauss = null;
        return mean + v * sigma;
      }
      const u1 = Math.max(1e-12, next());
      const u2 = next();
      const mag = Math.sqrt(-2.0 * Math.log(u1));
      const z0 = mag * Math.cos(2.0 * Math.PI * u2);
      spareGauss = mag * Math.sin(2.0 * Math.PI * u2);
      return mean + z0 * sigma;
    },
  };
}

function resamplePcm16To16k(pcmBuf, srcRate) {
  const sampleCount = Math.floor(pcmBuf.length / 2);
  if (srcRate === 16_000) {
    const out = new Int16Array(sampleCount);
    for (let i = 0; i < sampleCount; i += 1) out[i] = pcmBuf.readInt16LE(i * 2);
    return out;
  }
  const targetLen = Math.floor((sampleCount * 16_000) / srcRate);
  const out = new Int16Array(targetLen);
  for (let i = 0; i < targetLen; i += 1) {
    const x = (i * srcRate) / 16_000;
    const j = Math.floor(x);
    const t = x - j;
    const a = pcmBuf.readInt16LE(Math.min(j, sampleCount - 1) * 2);
    const b = pcmBuf.readInt16LE(Math.min(j + 1, sampleCount - 1) * 2);
    out[i] = clampInt16(a * (1 - t) + b * t);
  }
  return out;
}

/**
 * 纯 Node 原生混噪（对齐 chain_clips.py 的 888 排风扇/气泵持续噪声与 999 突发冲击噪声），
 * 在未安装 python3 的 Windows 机器上作为无依赖兜底。
 */
export function mixNoisePcm16(samples, level, seed = 0) {
  const n = samples.length;
  const out = new Int16Array(n);
  if (level === 0) {
    out.set(samples);
    return out;
  }
  const rng = createSeededRng(seed);
  if (level === 999) {
    for (let i = 0; i < n; i += 1) {
      out[i] = clampInt16(samples[i] + rng.gauss(0, 120));
    }
    let idx = rng.randint(2000, 9000);
    while (idx < n) {
      const len = rng.randint(1280, 2560);
      const amp = rng.uniform(3000, 8000);
      const end = Math.min(n, idx + len);
      for (let k = idx; k < end; k += 1) {
        out[k] = clampInt16(out[k] + rng.gauss(0, amp) * (1 - (k - idx) / len));
      }
      idx += len + rng.randint(9600, 16000);
    }
    return out;
  }
  if (level === 888) {
    let lp = 0.0;
    const rate = 16_000;
    for (let k = 0; k < n; k += 1) {
      const t = k / rate;
      const hum =
        900 * Math.sin(2 * Math.PI * 100 * t) +
        450 * Math.sin(2 * Math.PI * 200 * t + 1) +
        250 * Math.sin(2 * Math.PI * 300 * t + 2);
      lp = 0.85 * lp + 0.15 * rng.gauss(0, 5200);
      out[k] = clampInt16(samples[k] + hum + lp);
    }
    return out;
  }
  for (let i = 0; i < n; i += 1) {
    out[i] = clampInt16(samples[i] + rng.gauss(0, level));
  }
  return out;
}

/** 纯 Node 生成 16 kHz 纯背景噪声 WAV（供 voice-assets.mjs 在无 python3 环境下复用）。 */
export function generateNoiseWavWithNode(presetRaw, durationSecs, gain, outputPath, seed = 42) {
  const key = String(presetRaw || "fan").trim().toLowerCase();
  const level = NOISE_PRESETS[key] ?? Number(key);
  if (!Number.isFinite(level)) {
    throw new Error(`未知背景噪声档位：${presetRaw}`);
  }
  const sampleCount = Math.max(16_000, Math.round(Number(durationSecs || 12) * 16_000));
  const raw = mixNoisePcm16(new Int16Array(sampleCount), level, seed);
  const scale = Math.max(0.01, Math.min(1.0, Number(gain) || 0.25));
  const pcmBuf = Buffer.alloc(sampleCount * 2);
  for (let i = 0; i < sampleCount; i += 1) {
    pcmBuf.writeInt16LE(clampInt16(raw[i] * scale), i * 2);
  }
  mkdirSync(dirname(resolve(outputPath)), { recursive: true });
  writeFileSync(outputPath, encodeWavPcm16(pcmBuf, 16_000));
}

function mixNoiseBatchWithNode(jobs) {
  for (const job of jobs) {
    const { pcm, sampleRate } = readWavPcm16(job.input);
    const samples16k = resamplePcm16To16k(pcm, sampleRate);
    const mixed = mixNoisePcm16(samples16k, Number(job.level), Number(job.seed || 0));
    const outBuf = Buffer.alloc(mixed.length * 2);
    for (let i = 0; i < mixed.length; i += 1) {
      outBuf.writeInt16LE(mixed[i], i * 2);
    }
    mkdirSync(dirname(resolve(job.output)), { recursive: true });
    writeFileSync(job.output, encodeWavPcm16(outBuf, 16_000));
  }
}

/** 复用 chain_clips.py 的混噪逻辑（经 `python3 -I`）为语料批量生成噪声变体 WAV；无 python3 时自动切 Node 原生混噪。 */
export function mixNoiseBatchWithPython(samples, noiseLevels, tempDir, pythonBin = "python3") {
  const map = new Map();
  const jobs = [];
  for (const level of noiseLevels) {
    samples.forEach((sample, index) => {
      const key = `${sample.id}@@${level}`;
      if (level === 0) {
        map.set(key, sample.wav_path);
        return;
      }
      const safeTurnId = String(sample.turn_id || "sample").replace(/[^A-Za-z0-9_-]/g, "_");
      const outWav = join(tempDir, `n${level}_${index}_${safeTurnId}.wav`);
      map.set(key, outWav);
      jobs.push({
        input: sample.wav_path,
        output: outWav,
        level,
        seed: 7 * index + level,
      });
    });
  }
  if (jobs.length > 0) {
    const manifestPath = join(tempDir, "noise-batch.json");
    writeFileSync(manifestPath, JSON.stringify(jobs), "utf8");
    try {
      execFileSync(pythonBin, ["-I", NOISE_MIX_SCRIPT, "--batch", manifestPath], {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      });
    } catch {
      mixNoiseBatchWithNode(jobs);
    }
  }
  return map;
}

/** 读取单声道 16-bit PCM WAV 负载（严格校验 `fmt ` 与 `data` 块，拒绝非 PCM / 多声道 / 非 16-bit 音频）。 */
export function readWavPcm16(wavPath) {
  const buf = readFileSync(wavPath);
  if (buf.length < 44 || buf.toString("ascii", 0, 4) !== "RIFF" || buf.toString("ascii", 8, 12) !== "WAVE") {
    throw new Error(`无效 WAV 文件：${wavPath}`);
  }
  let hasFmt = false;
  let audioFormat = 0;
  let numChannels = 0;
  let sampleRate = 0;
  let bitsPerSample = 0;
  let offset = 12;
  let pcm = null;
  while (offset + 8 <= buf.length) {
    const id = buf.toString("ascii", offset, offset + 4);
    const size = buf.readUInt32LE(offset + 4);
    const bodyStart = offset + 8;
    const bodyEnd = Math.min(buf.length, bodyStart + size);
    if (id === "fmt " && size >= 16 && bodyStart + 16 <= buf.length) {
      hasFmt = true;
      audioFormat = buf.readUInt16LE(bodyStart);
      numChannels = buf.readUInt16LE(bodyStart + 2);
      sampleRate = buf.readUInt32LE(bodyStart + 4);
      bitsPerSample = buf.readUInt16LE(bodyStart + 14);
    } else if (id === "data" && !pcm) {
      pcm = buf.subarray(bodyStart, bodyEnd);
    }
    if (hasFmt && pcm) break;
    offset = bodyStart + size + (size % 2);
  }
  if (!hasFmt) throw new Error(`WAV 缺少 fmt 块：${wavPath}`);
  if (audioFormat !== 1 || numChannels !== 1 || bitsPerSample !== 16 || sampleRate <= 0) {
    throw new Error(
      `WAV 格式不受支持（仅接受单声道 16-bit PCM，实际 format=${audioFormat} ch=${numChannels} bits=${bitsPerSample} rate=${sampleRate}）`,
    );
  }
  if (!pcm || pcm.length === 0 || pcm.length % 2 !== 0) {
    throw new Error(`WAV data 块为空或字节数非法：${wavPath}`);
  }
  return { pcm, sampleRate, numChannels, bitsPerSample };
}

/** 将单声道 16-bit 小端 PCM Buffer 封装为标准 RIFF/WAVE Buffer。 */
export function encodeWavPcm16(pcmBuf, sampleRate = 16_000) {
  const header = Buffer.alloc(44);
  header.write("RIFF", 0, 4, "ascii");
  header.writeUInt32LE(36 + pcmBuf.length, 4);
  header.write("WAVE", 8, 4, "ascii");
  header.write("fmt ", 12, 4, "ascii");
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(sampleRate, 24);
  header.writeUInt32LE(sampleRate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write("data", 36, 4, "ascii");
  header.writeUInt32LE(pcmBuf.length, 40);
  return Buffer.concat([header, pcmBuf]);
}

/** 探测本机已安装的本地 ASR 模型（扫描 `<runtime>/models`）。 */
export function discoverInstalledLocalModels(runtimeDir, extraModelDirs = []) {
  const roots = [join(runtimeDir, "models"), ...extraModelDirs].filter((d) => d && existsSync(d));
  const found = [];
  for (const spec of LOCAL_ASR_CATALOG) {
    for (const root of roots) {
      const dir = join(root, spec.dir);
      if (existsSync(dir) && statSync(dir).isDirectory() && spec.required.every((f) => existsSync(join(dir, f)))) {
        found.push({ ...spec, modelDir: dir });
        break;
      }
    }
  }
  return found;
}

/** 查找本机 `moss-voice-local` 可执行文件（兼容根工作区、独立子工作区与桌面 sidecar 目录）。 */
export function resolveHelperExe(explicitPath = "", { rootDir = resolve(SCRIPT_DIR, "../.."), platform = process.platform, arch = process.arch } = {}) {
  const isWin = platform === "win32";
  const exeName = isWin ? "moss-voice-local.exe" : "moss-voice-local";
  const sidecarDir = resolve(rootDir, "apps/desktop/src-tauri/binaries");
  const sidecarCandidates = [];
  if (existsSync(sidecarDir)) {
    const targetTriple = isWin
      ? arch === "arm64"
        ? "aarch64-pc-windows-msvc"
        : "x86_64-pc-windows-msvc"
      : platform === "darwin"
        ? arch === "arm64"
          ? "aarch64-apple-darwin"
          : "x86_64-apple-darwin"
        : arch === "arm64"
          ? "aarch64-unknown-linux-gnu"
          : "x86_64-unknown-linux-gnu";
    sidecarCandidates.push(
      join(sidecarDir, `moss-voice-local-${targetTriple}${isWin ? ".exe" : ""}`),
      join(sidecarDir, exeName),
    );
  }
  const candidates = [
    explicitPath,
    process.env.MOSS_VOICE_LOCAL_EXE || "",
    resolve(rootDir, "target/release", exeName),
    resolve(rootDir, "apps/moss-voice-local/target/release", exeName),
    ...sidecarCandidates,
    resolve(rootDir, "target/debug", exeName),
    resolve(rootDir, "apps/moss-voice-local/target/debug", exeName),
    ...(isWin ? [] : ["/Applications/MOSS.app/Contents/MacOS/moss-voice-local"]),
  ].filter(Boolean);
  for (const cand of candidates) {
    if (existsSync(cand) && statSync(cand).isFile()) return cand;
  }
  return null;
}

/** 复用 `moss-voice-local` 协议 v9（8 字节长度头 + JSON 头 + PCM 负载）执行离线/流式本地识别。 */
export class LocalHelperClient {
  constructor(exePath) {
    this.exePath = exePath;
    this.child = null;
    this.closedError = null;
    this.buf = Buffer.alloc(0);
    this.nextId = 1;
    this.nextStream = 1;
    this.pending = new Map();
  }

  #failAll(err) {
    if (!this.closedError) {
      this.closedError = err;
    }
    const targetErr = this.closedError;
    for (const [, waiter] of this.pending) waiter.reject(targetErr);
    this.pending.clear();
  }

  async start() {
    this.closedError = null;
    // 忽略 stderr 防止 ONNX/sherpa-onnx 写满 64 KiB 管道死锁（对齐 crates/moss-voice-providers/src/local.rs）
    this.child = spawn(this.exePath, [], { stdio: ["pipe", "pipe", "ignore"] });
    this.child.stdout.on("data", (chunk) => this.#onData(chunk));
    this.child.stdin.on("error", (err) => {
      this.#failAll(new Error(`写入 moss-voice-local 管道失败：${err?.message || err}`));
    });
    this.child.on("error", (err) => {
      this.#failAll(new Error(`启动 moss-voice-local 失败：${err?.message || err}`));
    });
    this.child.on("exit", (code, signal) => {
      this.#failAll(new Error(`moss-voice-local 提前退出（code=${code ?? "null"}${signal ? `, signal=${signal}` : ""}）`));
    });
    const hello = await this.request({ op: "hello" });
    if (hello.kind !== "hello" || hello.protocol !== LOCAL_PROTO_VERSION) {
      throw new Error(`moss-voice-local 协议版本不匹配（期望 ${LOCAL_PROTO_VERSION}，实际 ${hello.protocol}）`);
    }
  }

  #onData(chunk) {
    this.buf = Buffer.concat([this.buf, chunk]);
    while (this.buf.length >= 8) {
      const hLen = this.buf.readUInt32LE(0);
      const pLen = this.buf.readUInt32LE(4);
      const total = 8 + hLen + pLen;
      if (this.buf.length < total) break;
      const header = JSON.parse(this.buf.subarray(8, 8 + hLen).toString("utf8"));
      const payload = pLen > 0 ? Buffer.from(this.buf.subarray(8 + hLen, total)) : Buffer.alloc(0);
      this.buf = this.buf.subarray(total);
      const waiter = this.pending.get(header.id);
      if (waiter) {
        this.pending.delete(header.id);
        if (header.kind === "error") {
          waiter.reject(new Error(header.message || "本地模型推理出错"));
        } else {
          waiter.resolve(pLen > 0 ? { ...header, payload } : header);
        }
      }
    }
  }

  request(opPayload, payload = Buffer.alloc(0), timeoutMs = 60_000) {
    if (!this.child || this.closedError || this.child.stdin?.destroyed || this.child.stdin?.writableEnded) {
      return Promise.reject(this.closedError || new Error("moss-voice-local 未启动或已退出"));
    }
    const id = this.nextId++;
    const headerBuf = Buffer.from(JSON.stringify({ id, ...opPayload }), "utf8");
    const lenBuf = Buffer.alloc(8);
    lenBuf.writeUInt32LE(headerBuf.length, 0);
    lenBuf.writeUInt32LE(payload.length, 4);
    return new Promise((resolvePromise, rejectPromise) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectPromise(new Error(`本地模型请求超时（op=${opPayload.op}）`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (val) => {
          clearTimeout(timer);
          resolvePromise(val);
        },
        reject: (err) => {
          clearTimeout(timer);
          rejectPromise(err);
        },
      });
      try {
        this.child.stdin.write(Buffer.concat([lenBuf, headerBuf, payload]), (err) => {
          if (err) {
            this.#failAll(new Error(`写入 moss-voice-local 管道失败：${err?.message || err}`));
          }
        });
      } catch (err) {
        this.#failAll(new Error(`写入 moss-voice-local 管道失败：${err?.message || err}`));
      }
    });
  }

  async loadAsr(modelDir, engine, hotwords = []) {
    await this.request({
      op: "load_asr",
      dir: modelDir,
      engine,
      options: { beam_search: true, hotwords },
    });
  }

  async transcribeWav(wavPath, { streaming = true } = {}) {
    const { pcm, sampleRate } = readWavPcm16(wavPath);
    if (!streaming || sampleRate !== 16_000) {
      // 非流式模型（如 Qwen3-ASR 0.6B）或非 16 kHz 音频直接走单次整段 transcribe
      const t0 = performance.now();
      const fin = await this.request({ op: "transcribe", sample_rate: sampleRate }, pcm);
      const finalMs = Math.round(performance.now() - t0);
      return { text: String(fin.text ?? "").trim(), firstInterimMs: null, finalMs };
    }
    const stream = this.nextStream++;
    const startedAt = performance.now();
    let firstInterimMs = null;
    try {
      await this.request({ op: "asr_open", stream });
      // 每 160ms（16 kHz = 2560 samples = 5120 bytes）切块送入以采集首个中间结果延迟
      const chunkBytes = Math.max(1024, Math.floor((sampleRate * 160) / 1000) * 2);
      for (let off = 0; off < pcm.length; off += chunkBytes) {
        const slice = pcm.subarray(off, Math.min(pcm.length, off + chunkBytes));
        const res = await this.request({ op: "asr_feed", stream }, slice);
        if (firstInterimMs === null && typeof res.text === "string" && res.text.trim()) {
          firstInterimMs = Math.round(performance.now() - startedAt);
        }
      }
      const fin = await this.request({ op: "asr_finish", stream });
      const finalMs = Math.round(performance.now() - startedAt);
      const text = String(fin.text ?? "").trim();
      return { text, firstInterimMs: firstInterimMs ?? finalMs, finalMs };
    } catch (err) {
      if (this.closedError || !this.child) {
        throw this.closedError || err;
      }
      const t0 = performance.now();
      const fin = await this.request({ op: "transcribe", sample_rate: sampleRate }, pcm);
      const finalMs = Math.round(performance.now() - t0);
      return { text: String(fin.text ?? "").trim(), firstInterimMs: null, finalMs };
    }
  }

  close() {
    this.#failAll(new Error("moss-voice-local 已关闭"));
    if (this.child) {
      try {
        this.child.stdin.end();
        this.child.kill();
      } catch {
        // 忽略关闭异常
      }
      this.child = null;
    }
  }
}

/** 只读读取运行目录 settings.json 中的云端 ASR 配置及 moss.sqlite `app_secrets` 中保存的密钥（绝不打印密钥）。 */
export function loadConfiguredCloudAsr(runtimeDir, pythonBin = "python3") {
  const settingsFile = join(runtimeDir, "settings.json");
  if (!existsSync(settingsFile)) return null;
  let settings;
  try {
    settings = JSON.parse(readFileSync(settingsFile, "utf8"));
  } catch {
    return null;
  }
  const asr = settings?.voice?.builtin_models?.asr;
  if (!asr?.url || !asr?.model) return null;
  const kind = String(asr.kind || "openai").toLowerCase();
  let apiKey = process.env.MOSS_ASR_API_KEY || "";
  if (!apiKey) {
    const dbFile = join(runtimeDir, "moss.sqlite");
    if (existsSync(dbFile)) {
      try {
        const py =
          "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); " +
          // 只读识别自己的密钥（与应用一致，ASR 不借用大模型密钥，避免把 LLM 密钥发到另一家服务的识别地址）
          "r=c.execute(\"SELECT value FROM app_secrets WHERE name='voice.asr.main' LIMIT 1\").fetchone(); " +
          "print(r[0] if r else '', end='')";
        apiKey = execFileSync(pythonBin, ["-I", "-c", py, dbFile], {
          encoding: "utf8",
          stdio: ["ignore", "pipe", "ignore"],
        }).trim();
      } catch {
        apiKey = "";
      }
    }
  }
  if (!apiKey) return null;
  return {
    id: `cloud:${kind}:${asr.model}`,
    label: `云端 ${kind} (${asr.model})`,
    kind,
    url: String(asr.url),
    model: String(asr.model),
    apiKey,
  };
}

function normalizeOpenAiAsrUrl(rawUrl) {
  const clean = String(rawUrl || "").trim().replace(/\/+$/, "");
  if (clean.endsWith("/audio/transcriptions")) return clean;
  return `${clean}/audio/transcriptions`;
}

/** 调用 OpenAI 兼容 `/audio/transcriptions` 接口完成单条 WAV 识别（绝不打印密钥）。 */
export function createOpenAiCloudRecognizer(cloudConfig) {
  const endpoint = normalizeOpenAiAsrUrl(cloudConfig.url);
  return {
    id: cloudConfig.id,
    label: cloudConfig.label,
    secret: cloudConfig.apiKey,
    async transcribe(wavPath) {
      const startedAt = performance.now();
      const wavBytes = readFileSync(wavPath);
      const form = new FormData();
      form.append("file", new Blob([wavBytes], { type: "audio/wav" }), "clip.wav");
      form.append("model", cloudConfig.model);
      const res = await fetch(endpoint, {
        method: "POST",
        headers: { Authorization: `Bearer ${cloudConfig.apiKey}` },
        body: form,
        signal: AbortSignal.timeout(30_000),
      });
      const finalMs = Math.round(performance.now() - startedAt);
      const text = await res.text();
      if (!res.ok) {
        throw new Error(`云端识别 HTTP ${res.status}`);
      }
      try {
        const parsed = JSON.parse(text);
        return { text: String(parsed?.text ?? "").trim(), firstInterimMs: null, finalMs };
      } catch {
        return { text: text.trim(), firstInterimMs: null, finalMs };
      }
    },
  };
}

/** 汇总所有测评结果并产出热词、同音纠错（强制命令词安全保护）和数字单位规范化三类候选。 */
export function analyzeResultsAndSuggestions(rows, { terms = DEFAULT_WORKSHOP_TERMS, existingHotwords = [], existingCorrections = [] } = {}) {
  const modelMap = new Map();
  const cmdStats = new Map();
  const termMissMap = new Map();
  const homophoneMap = new Map();
  const discardedMap = new Map();
  const numNormMap = new Map();

  let numUnitTotal = 0;
  let numUnitMatched = 0;
  let termTotal = 0;
  let termMatched = 0;

  const existingHotSet = new Set(existingHotwords.map((w) => String(w).trim()).filter(Boolean));
  const existingCorrSet = new Set(existingCorrections.map((s) => String(s).trim()).filter(Boolean));

  for (const row of rows) {
    const bucketKey = `${row.model_id}@@${row.noise_level}`;
    if (!modelMap.has(bucketKey)) {
      modelMap.set(bucketKey, {
        model_id: row.model_id,
        model_label: row.model_label,
        noise_level: row.noise_level,
        count: 0,
        errors: 0,
        edits: 0,
        ref_chars: 0,
        first_interims: [],
        finals: [],
        num_total: 0,
        num_matched: 0,
        term_total: 0,
        term_matched: 0,
      });
    }
    const bucket = modelMap.get(bucketKey);
    bucket.count += 1;
    if (row.error) {
      bucket.errors += 1;
      continue;
    }
    bucket.edits += row.edits;
    bucket.ref_chars += row.ref_len;
    if (typeof row.first_interim_ms === "number") bucket.first_interims.push(row.first_interim_ms);
    if (typeof row.final_ms === "number") bucket.finals.push(row.final_ms);

    // 命令词召回与误报（按模型 × 噪声分桶）
    if (!cmdStats.has(bucketKey)) {
      cmdStats.set(bucketKey, {
        model_id: row.model_id,
        model_label: row.model_label,
        noise_level: row.noise_level,
        stop_total: 0,
        stop_hit: 0,
        exit_total: 0,
        exit_hit: 0,
        false_positive: 0,
      });
    }
    const cmd = cmdStats.get(bucketKey);
    const refCmd = classifyCommand(row.reference_text);
    const hypCmd = classifyCommand(row.hypothesis);
    if (refCmd === "stop") {
      cmd.stop_total += 1;
      if (hypCmd === "stop") cmd.stop_hit += 1;
    } else if (refCmd === "exit") {
      cmd.exit_total += 1;
      if (hypCmd === "exit") cmd.exit_hit += 1;
    } else if (hypCmd !== null) {
      cmd.false_positive += 1;
    }

    // 数字和单位准确率与规范化候选
    const numEval = evaluateNumberAndUnitAccuracy(row.reference_text, row.hypothesis);
    bucket.num_total += numEval.total;
    bucket.num_matched += numEval.matched;
    numUnitTotal += numEval.total;
    numUnitMatched += numEval.matched;
    for (const cand of numEval.normCandidates) {
      const k = `${cand.raw}=>${cand.normalized}`;
      const entry = numNormMap.get(k) || { raw: cand.raw, normalized: cand.normalized, count: 0, examples: [] };
      entry.count += 1;
      if (entry.examples.length < 5 && !entry.examples.includes(row.clip_id)) entry.examples.push(row.clip_id);
      numNormMap.set(k, entry);
    }

    // 材料与术语准确率 + 热词候选
    const refNorm = normalizeForCer(row.reference_text).join("");
    const hypNorm = normalizeForCer(row.hypothesis).join("");
    for (const term of terms) {
      const tNorm = normalizeForCer(term).join("");
      if (!tNorm || !refNorm.includes(tNorm)) continue;
      bucket.term_total += 1;
      termTotal += 1;
      const entry = termMissMap.get(term) || {
        term,
        total_count: 0,
        miss_count: 0,
        already_configured: existingHotSet.has(term),
        examples: [],
      };
      entry.total_count += 1;
      if (hypNorm.includes(tNorm)) {
        bucket.term_matched += 1;
        termMatched += 1;
      } else {
        entry.miss_count += 1;
        if (entry.examples.length < 5 && !entry.examples.includes(`${row.clip_id}: ${row.hypothesis}`)) {
          entry.examples.push(`${row.clip_id}: ${row.hypothesis}`);
        }
      }
      termMissMap.set(term, entry);
    }

    // 同音纠错候选提取 + 命令词安全门禁过滤：
    // 1. 若参考句或识别句本身属于停止/退出命令词，整条样本的错读不得生成任何同音纠错候选，全部记入 discarded_candidates；
    // 2. 其余样本的每个候选逐条过 isSafeHomophoneCandidate 且禁止把原句替换成命令词。
    if ((refCmd !== null || hypCmd !== null) && row.edits > 0) {
      const wrongWhole = normalizeForCer(row.hypothesis).join("") || "(空)";
      const rightWhole = normalizeForCer(row.reference_text).join("");
      const dk = `${wrongWhole}=>${rightWhole}`;
      const dEntry = discardedMap.get(dk) || {
        wrong: wrongWhole,
        right: rightWhole,
        count: 0,
        reason:
          refCmd !== null
            ? `禁止将任何识别错误映射为停止/退出命令词（目标「${rightWhole}」）`
            : `禁止通过同音纠错改写停止/退出命令词（原词「${wrongWhole}」）`,
      };
      dEntry.count += 1;
      discardedMap.set(dk, dEntry);
      continue;
    }

    for (const span of row.substitutions || []) {
      const check = isSafeHomophoneCandidate(span.wrong, span.right);
      if (!check.allowed) {
        const dk = `${span.wrong}=>${span.right}`;
        const dEntry = discardedMap.get(dk) || {
          wrong: span.wrong,
          right: span.right,
          count: 0,
          reason: check.reason,
        };
        dEntry.count += 1;
        discardedMap.set(dk, dEntry);
        continue;
      }
      const rawReplaced = String(row.hypothesis || "").replaceAll(span.wrong, span.right);
      const normWrong = normalizeForCer(span.wrong).join("");
      const normRight = normalizeForCer(span.right).join("");
      const normReplaced = normWrong
        ? normalizeForCer(row.hypothesis).join("").replaceAll(normWrong, normRight)
        : "";
      if (classifyCommand(rawReplaced) !== null || (normReplaced && classifyCommand(normReplaced) !== null)) {
        const dk = `${span.wrong}=>${span.right}`;
        const dEntry = discardedMap.get(dk) || {
          wrong: span.wrong,
          right: span.right,
          count: 0,
          reason: `替换后会导致整句触发停止/退出命令词（${span.wrong} -> ${span.right}）`,
        };
        dEntry.count += 1;
        discardedMap.set(dk, dEntry);
        continue;
      }
      // 跳过过长整句替换或单字纯数字替换
      if ([...span.wrong].length > 8 || [...span.right].length > 8) continue;
      if (/^\d+$/.test(span.wrong) && /^\d+$/.test(span.right)) continue;
      const key = `${span.wrong}=${span.right}`;
      const entry = homophoneMap.get(key) || {
        wrong: span.wrong,
        right: span.right,
        rule: key,
        count: 0,
        already_configured: existingCorrSet.has(key),
        models: [],
        examples: [],
      };
      entry.count += 1;
      if (!entry.models.includes(row.model_id)) entry.models.push(row.model_id);
      if (entry.examples.length < 5 && !entry.examples.includes(row.clip_id)) entry.examples.push(row.clip_id);
      homophoneMap.set(key, entry);
    }
  }

  // 计算按模型 × 噪声汇总行，并附安静档到噪声档的 CER 退化增量（全错分桶 cer 为 null，绝不误报 0.0%）
  const summaryRows = [...modelMap.values()].map((b) => ({
    ...b,
    cer: b.ref_chars > 0 ? Number((b.edits / b.ref_chars).toFixed(4)) : null,
    first_interim_p50_ms: percentile(b.first_interims, 50),
    first_interim_p90_ms: percentile(b.first_interims, 90),
    final_p50_ms: percentile(b.finals, 50),
    final_p90_ms: percentile(b.finals, 90),
    num_accuracy: b.num_total > 0 ? Number((b.num_matched / b.num_total).toFixed(4)) : null,
    term_accuracy: b.term_total > 0 ? Number((b.term_matched / b.term_total).toFixed(4)) : null,
  }));
  const cleanCerByModel = new Map();
  for (const row of summaryRows) {
    if (row.noise_level === 0 && typeof row.cer === "number") cleanCerByModel.set(row.model_id, row.cer);
  }
  for (const row of summaryRows) {
    const cleanCer = cleanCerByModel.get(row.model_id);
    row.noise_degradation =
      row.noise_level !== 0 && typeof row.cer === "number" && typeof cleanCer === "number"
        ? Number((row.cer - cleanCer).toFixed(4))
        : null;
  }

  const worstClips = rows
    .filter((r) => !r.error && r.edits > 0)
    .sort((a, b) => b.cer - a.cer || b.edits - a.edits)
    .slice(0, 30);

  const suggestions = {
    auto_applied: false,
    note: "候选仅作参考，脚本不会自动修改代码或 settings.json；采纳前须由人或 AI 复核并补测试。",
    hotword_candidates: [...termMissMap.values()]
      .filter((t) => t.miss_count > 0)
      .sort((a, b) => b.miss_count - a.miss_count || a.term.localeCompare(b.term)),
    homophone_correction_candidates: [...homophoneMap.values()].sort(
      (a, b) => b.count - a.count || a.rule.localeCompare(b.rule),
    ),
    number_unit_normalization_candidates: [...numNormMap.values()].sort(
      (a, b) => b.count - a.count || a.raw.localeCompare(b.raw),
    ),
    discarded_candidates: [...discardedMap.values()].sort(
      (a, b) => b.count - a.count || a.wrong.localeCompare(b.wrong),
    ),
  };

  const summarizeGroups = (keyFn, fieldName) => {
    const grouped = {};
    const groups = new Map();
    for (const row of rows) {
      const key = keyFn(row);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(row);
    }
    for (const [src, groupRows] of groups.entries()) {
      const gBuckets = new Map();
      let gNumTotal = 0;
      let gNumMatched = 0;
      let gTermTotal = 0;
      let gTermMatched = 0;
      const clipIds = new Set();
      for (const row of groupRows) {
        clipIds.add(row.clip_id);
        const bk = `${row.model_id}@@${row.noise_level}`;
        if (!gBuckets.has(bk)) {
          gBuckets.set(bk, {
            model_id: row.model_id,
            model_label: row.model_label,
            noise_level: row.noise_level,
            count: 0,
            errors: 0,
            edits: 0,
            ref_chars: 0,
            finals: [],
          });
        }
        const b = gBuckets.get(bk);
        b.count += 1;
        if (row.error) {
          b.errors += 1;
          continue;
        }
        b.edits += row.edits;
        b.ref_chars += row.ref_len;
        if (typeof row.final_ms === "number") b.finals.push(row.final_ms);
        const ne = evaluateNumberAndUnitAccuracy(row.reference_text, row.hypothesis);
        gNumTotal += ne.total;
        gNumMatched += ne.matched;
        const rNorm = normalizeForCer(row.reference_text).join("");
        const hNorm = normalizeForCer(row.hypothesis).join("");
        for (const term of terms) {
          const tNorm = normalizeForCer(term).join("");
          if (!tNorm || !rNorm.includes(tNorm)) continue;
          gTermTotal += 1;
          if (hNorm.includes(tNorm)) gTermMatched += 1;
        }
      }
      const gSummaryRows = [...gBuckets.values()].map((b) => ({
        model_id: b.model_id,
        model_label: b.model_label,
        noise_level: b.noise_level,
        count: b.count,
        errors: b.errors,
        cer: b.ref_chars > 0 ? Number((b.edits / b.ref_chars).toFixed(4)) : null,
        final_p50_ms: percentile(b.finals, 50),
        final_p90_ms: percentile(b.finals, 90),
      }));
      grouped[src] = {
        [fieldName]: src,
        sample_count: clipIds.size,
        row_count: groupRows.length,
        overall_cer: (() => {
          const edits = groupRows.reduce((s, r) => s + (r.error ? 0 : r.edits), 0);
          const chars = groupRows.reduce((s, r) => s + (r.error ? 0 : r.ref_len), 0);
          return chars > 0 ? Number((edits / chars).toFixed(4)) : null;
        })(),
        number_unit: { total: gNumTotal, matched: gNumMatched },
        terms: { total: gTermTotal, matched: gTermMatched },
        summaryRows: gSummaryRows,
      };
    }
    return grouped;
  };

  const resolveVoiceSourceGroup = (row) => {
    const inputSource = String(row.input_source || (row.label_source === "e2e-playback" ? "e2e_playback" : "live_mic"));
    if (inputSource === "digital_inject") return "digital_inject";
    const origin = String(
      row.audio_origin || row.voice_profile?.audio_origin || row.voice_profile?.source || "",
    ).trim();
    if (origin === "online-human") return "online-human";
    if (origin === "human-recording") return "human-recording";
    if (origin === "device-corpus-secondary") return "device-corpus-secondary";
    if (inputSource === "e2e_playback") return "human-recording";
    return "live_mic";
  };

  const byLabelSource = summarizeGroups((row) => String(row.label_source || "manual"), "label_source");
  const byInputSource = summarizeGroups(
    (row) => String(row.input_source || (row.label_source === "e2e-playback" ? "e2e_playback" : "live_mic")),
    "input_source",
  );
  const byAudioOrigin = summarizeGroups(resolveVoiceSourceGroup, "audio_origin");

  return {
    summaryRows,
    commandRows: [...cmdStats.values()],
    overallNumberUnit: { total: numUnitTotal, matched: numUnitMatched },
    overallTerms: { total: termTotal, matched: termMatched },
    worstClips,
    suggestions,
    byLabelSource,
    byInputSource,
    byAudioOrigin,
  };
}

function pctText(ratio) {
  if (ratio === null || ratio === undefined || Number.isNaN(ratio)) return "—";
  return `${(ratio * 100).toFixed(1)}%`;
}

function msText(val) {
  return typeof val === "number" && Number.isFinite(val) ? `${val} ms` : "—";
}

function renderMarkdownReport(runId, sampleCount, analysis) {
  const lines = [
    `# 识别（ASR）离线测评报告（${runId}）`,
    "",
    `- 已标注样本数：${sampleCount}`,
    `- 总评测条数（模型 × 噪声 × 样本）：${analysis.summaryRows.reduce((s, r) => s + r.count, 0)}`,
    `- 数字和单位整体准确率：${analysis.overallNumberUnit.matched}/${analysis.overallNumberUnit.total} (${pctText(
      analysis.overallNumberUnit.total ? analysis.overallNumberUnit.matched / analysis.overallNumberUnit.total : null,
    )})`,
    `- 材料名和术语整体准确率：${analysis.overallTerms.matched}/${analysis.overallTerms.total} (${pctText(
      analysis.overallTerms.total ? analysis.overallTerms.matched / analysis.overallTerms.total : null,
    )})`,
    "",
    "## 1. 按模型与噪声档位汇总（字错率 / 退化 / 延迟）",
    "",
    "| 模型 | 噪声档位 | 样本数 | 失败数 | 字错率 (CER) | 相对安静退化 | 数字单位准确率 | 术语准确率 | 首中间延迟 P50 / P90 | 最终延迟 P50 / P90 |",
    "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
  ];
  for (const r of analysis.summaryRows) {
    const deg = r.noise_degradation === null ? "—" : `${r.noise_degradation >= 0 ? "+" : ""}${(r.noise_degradation * 100).toFixed(1)}pp`;
    lines.push(
      `| ${r.model_label} | ${noiseLabel(r.noise_level)} | ${r.count} | ${r.errors || 0} | ${pctText(r.cer)} | ${deg} | ${pctText(
        r.num_accuracy,
      )} | ${pctText(r.term_accuracy)} | ${msText(r.first_interim_p50_ms)} / ${msText(
        r.first_interim_p90_ms,
      )} | ${msText(r.final_p50_ms)} / ${msText(r.final_p90_ms)} |`,
    );
  }

  const sourceEntries = Object.values(analysis.byLabelSource || {});
  const hasPlaybackGroup = sourceEntries.some((g) => g.label_source === "e2e-playback");
  if (hasPlaybackGroup || sourceEntries.length > 1) {
    const labelSourceTitle = (src) =>
      src === "e2e-playback" ? "整体测试外放录音（e2e-playback）" : src === "manual" ? "人工标注真实录音（manual）" : src;
    lines.push(
      "",
      "## 1.5 按语料来源分组统计（人工标注真实录音 vs 整体测试外放录音）",
      "",
      "| 来源分组 | 样本数 | 模型 | 噪声档位 | 失败数 | 字错率 (CER) | 最终延迟 P50 / P90 |",
      "| --- | ---: | --- | --- | ---: | ---: | ---: |",
    );
    for (const group of sourceEntries.sort((a, b) => a.label_source.localeCompare(b.label_source))) {
      for (const r of group.summaryRows) {
        lines.push(
          `| ${labelSourceTitle(group.label_source)} | ${group.sample_count} | ${r.model_label} | ${noiseLabel(
            r.noise_level,
          )} | ${r.errors || 0} | ${pctText(r.cer)} | ${msText(r.final_p50_ms)} / ${msText(r.final_p90_ms)} |`,
        );
      }
    }
  }

  const inputSourceEntries = Object.values(analysis.byInputSource || {});
  const hasNonLiveInput = inputSourceEntries.some((g) => g.input_source !== "live_mic");
  if (hasNonLiveInput || inputSourceEntries.length > 1) {
    const inputSourceTitle = (src) =>
      ({
        live_mic: "小智实时说话（live_mic）",
        e2e_playback: "整体测试外放录音（e2e_playback）",
        digital_inject: "数字录音直通（digital_inject）",
        text_inject: "文字直通（text_inject）",
      })[src] || src;
    lines.push(
      "",
      "## 1.6 按音源分类统计（小智实时说话 / 整体测试外放录音 / 数字录音直通）",
      "",
      "| 音源分组 | 样本数 | 模型 | 噪声档位 | 失败数 | 字错率 (CER) | 最终延迟 P50 / P90 |",
      "| --- | ---: | --- | --- | ---: | ---: | ---: |",
    );
    for (const group of inputSourceEntries.sort((a, b) => a.input_source.localeCompare(b.input_source))) {
      for (const r of group.summaryRows) {
        lines.push(
          `| ${inputSourceTitle(group.input_source)} | ${group.sample_count} | ${r.model_label} | ${noiseLabel(
            r.noise_level,
          )} | ${r.errors || 0} | ${pctText(r.cer)} | ${msText(r.final_p50_ms)} / ${msText(r.final_p90_ms)} |`,
        );
      }
    }
  }

  const audioOriginEntries = Object.values(analysis.byAudioOrigin || {});
  const hasOnlineHuman = audioOriginEntries.some((g) => g.audio_origin === "online-human");
  const hasDiverseOrigin =
    hasOnlineHuman ||
    audioOriginEntries.length > 1 ||
    audioOriginEntries.some((g) => g.audio_origin !== "live_mic");
  if (hasDiverseOrigin) {
    const originOrder = {
      "online-human": 1,
      "human-recording": 2,
      live_mic: 3,
      digital_inject: 4,
      "device-corpus-secondary": 5,
    };
    const originTitle = (src) =>
      ({
        "online-human": "网上真人（online-human）",
        "human-recording": "我的录音（human-recording）",
        live_mic: "小智实时说话（live_mic）",
        digital_inject: "数字直通（digital_inject）",
        "device-corpus-secondary": "设备录音（二次录制）",
      })[src] || src;
    lines.push(
      "",
      "## 1.7 按语音来源分组统计（网上真人 / 我的录音 / 小智实时说话 / 数字直通）",
      "",
      ...(hasOnlineHuman
        ? ["> **说明**：网上语音的指标只反映通用识别能力，不代表用户本人的识别效果。", ""]
        : []),
      "| 语音来源 | 样本数 | 模型 | 噪声档位 | 失败数 | 字错率 (CER) | 最终延迟 P50 / P90 |",
      "| --- | ---: | --- | --- | ---: | ---: | ---: |",
    );
    for (const group of audioOriginEntries.sort(
      (a, b) => (originOrder[a.audio_origin] || 99) - (originOrder[b.audio_origin] || 99),
    )) {
      for (const r of group.summaryRows) {
        lines.push(
          `| ${originTitle(group.audio_origin)} | ${group.sample_count} | ${r.model_label} | ${noiseLabel(
            r.noise_level,
          )} | ${r.errors || 0} | ${pctText(r.cer)} | ${msText(r.final_p50_ms)} / ${msText(r.final_p90_ms)} |`,
        );
      }
    }
  }

  lines.push(
    "",
    "## 2. 命令词召回与误报（停 / 停止 / 退下）",
    "",
    "| 模型 | 噪声档位 | 停止命令召回 | 退出命令召回 | 非命令误报成命令 |",
    "| --- | --- | ---: | ---: | ---: |",
  );
  for (const c of analysis.commandRows) {
    const stopRec = c.stop_total > 0 ? `${c.stop_hit}/${c.stop_total} (${pctText(c.stop_hit / c.stop_total)})` : "0/0";
    const exitRec = c.exit_total > 0 ? `${c.exit_hit}/${c.exit_total} (${pctText(c.exit_hit / c.exit_total)})` : "0/0";
    lines.push(`| ${c.model_label} | ${noiseLabel(c.noise_level)} | ${stopRec} | ${exitRec} | ${c.false_positive} |`);
  }

  lines.push("", "## 3. 候选摘要（仅落盘，不自动修改代码或设置）", "");
  lines.push(`- 热词候选：${analysis.suggestions.hotword_candidates.length} 条`);
  lines.push(`- 同音纠错候选：${analysis.suggestions.homophone_correction_candidates.length} 条`);
  lines.push(`- 数字和单位规范化候选：${analysis.suggestions.number_unit_normalization_candidates.length} 条`);
  if (analysis.suggestions.discarded_candidates.length > 0) {
    lines.push(
      `- **命令词保护已拦截并丢弃违规同音候选**：${analysis.suggestions.discarded_candidates.length} 条（严禁把任意内容映射成停止/退出命令或改写命令词本身）：`,
    );
    for (const d of analysis.suggestions.discarded_candidates.slice(0, 15)) {
      lines.push(`  - 已丢弃 \`${d.wrong} -> ${d.right}\`（出现 ${d.count} 次）：${d.reason}`);
    }
  } else {
    lines.push("- 命令词保护拦截：0 条违规候选");
  }

  lines.push(
    "",
    "## 4. 错得最多的前 30 个片段",
    "",
    "| # | 片段 | 模型 | 噪声 | CER | 原文（标注） | 识别结果 |",
    "| ---: | --- | --- | --- | ---: | --- | --- |",
  );
  if (analysis.worstClips.length === 0) {
    lines.push("| — | 无错字片段 | — | — | 0.0% | — | — |");
  } else {
    analysis.worstClips.forEach((clip, idx) => {
      const esc = (s) => String(s ?? "").replace(/\|/g, "\\|").replace(/\r?\n/g, " ");
      lines.push(
        `| ${idx + 1} | ${esc(clip.clip_id)} | ${esc(clip.model_label)} | ${noiseLabel(clip.noise_level)} | ${pctText(
          clip.cer,
        )} | ${esc(clip.reference_text)} | ${esc(clip.hypothesis)} |`,
      );
    });
  }
  lines.push("");
  return lines.join("\n");
}

function parseNoiseLevels(raw) {
  if (!raw) return [0, 888, 999];
  const list = String(raw)
    .split(",")
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean)
    .map((token) => {
      if (Object.hasOwn(NOISE_PRESETS, token)) return NOISE_PRESETS[token];
      const n = Number.parseInt(token, 10);
      if (!Number.isFinite(n) || n < 0) throw new Error(`无效噪声档位：${token}`);
      return n;
    });
  return [...new Set(list)];
}

/**
 * 完整离线 ASR 测评主入口（支持注入 `recognizers` 与 `mixNoiseFn` 供单测使用）。
 */
export async function runAsrBench(options = {}) {
  const runtimeDir = resolve(options.runtimeDir || defaultRuntimeDir());
  const corpusDir = resolve(options.corpusDir || join(runtimeDir, "asr-corpus"));
  const traceDir = options.traceDir
    ? resolve(options.traceDir)
    : !options.corpusDir
      ? resolve(resolveProcessTraceDir(runtimeDir))
      : null;
  const noiseLevels = Array.isArray(options.noiseLevels) ? options.noiseLevels : parseNoiseLevels(options.noiseLevelsRaw);
  const samples = loadLabeledCorpus(corpusDir, traceDir);
  if (samples.length === 0) {
    return { exitCode: 1, errorMessage: NO_LABELED_CORPUS_MESSAGE };
  }

  const runId = options.runId || formatRunId();
  const outDir = resolve(options.outDir || join(runtimeDir, "voice-bench", runId));
  mkdirSync(outDir, { recursive: true });

  const secretsToRedact = [];
  const closers = [];
  let resolvedHelperExe = null;
  let recognizers = Array.isArray(options.recognizers) ? [...options.recognizers] : null;
  let existingHotwords = [];
  let existingCorrections = [];
  const settingsFile = join(runtimeDir, "settings.json");
  if (existsSync(settingsFile)) {
    try {
      const parsed = JSON.parse(readFileSync(settingsFile, "utf8"));
      existingHotwords = parsed?.voice?.builtin_local?.custom_hotwords || [];
      existingCorrections = parsed?.voice?.builtin_local?.custom_asr_corrections || [];
    } catch {
      // 忽略无效设置文件
    }
  }

  const tempDir = mkdtempSync(join(tmpdir(), "moss-asr-bench-"));
  let cleanedUp = false;
  const cleanup = () => {
    if (cleanedUp) return;
    cleanedUp = true;
    for (const closeFn of closers) {
      try {
        closeFn();
      } catch {
        // 忽略关闭异常
      }
    }
    try {
      rmSync(tempDir, { recursive: true, force: true });
    } catch {
      // 忽略临时目录清理异常
    }
  };
  const unregisterSignals = registerSignalCleanup(cleanup);
  const rows = [];
  try {
    if (!recognizers) {
      recognizers = [];
      const requestedModels = options.models
        ? String(options.models)
            .split(",")
            .map((s) => s.trim())
            .filter(Boolean)
        : null;

      // 1. 已安装的本地模型（经 moss-voice-local；启动即登记 closer，加载失败亦确保回收子进程）
      const installedLocal = discoverInstalledLocalModels(runtimeDir);
      const selectedLocal = requestedModels
        ? installedLocal.filter((m) => requestedModels.includes(m.id))
        : installedLocal;
      if (selectedLocal.length > 0) {
        const helperExe = resolveHelperExe(options.helperExe);
        if (!helperExe) {
          return {
            exitCode: 1,
            errorMessage: "检测到已安装的本地识别模型，但找不到 moss-voice-local 可执行文件（可用 --helper 指定）",
          };
        }
        resolvedHelperExe = helperExe;
        for (const spec of selectedLocal) {
          const client = new LocalHelperClient(helperExe);
          closers.push(() => client.close());
          await client.start();
          await client.loadAsr(spec.modelDir, spec.engine, existingHotwords);
          recognizers.push({
            id: spec.id,
            label: spec.label,
            transcribe: (wavPath) => client.transcribeWav(wavPath, { streaming: spec.streaming !== false }),
          });
        }
      }

      // 2. 已配置的云端识别模型（读取 settings.json + app_secrets，绝不打印密钥）
      const cloud = loadConfiguredCloudAsr(runtimeDir, options.pythonBin || "python3");
      if (cloud && (!requestedModels || requestedModels.includes(cloud.id) || requestedModels.includes("cloud"))) {
        if (cloud.kind === "openai") {
          const rec = createOpenAiCloudRecognizer(cloud);
          secretsToRedact.push(cloud.apiKey);
          recognizers.push(rec);
        }
      }

      if (recognizers.length === 0) {
        return {
          exitCode: 1,
          errorMessage: "未找到可用的本地或云端识别模型（请先安装本地 ASR 模型或配置云端 OpenAI 兼容 ASR）",
        };
      }
    }

    for (const rec of recognizers) {
      if (rec?.secret) secretsToRedact.push(rec.secret);
    }

    const mixer = options.mixNoiseFn || mixNoiseBatchWithPython;
    const wavMap = await mixer(samples, noiseLevels, tempDir, options.pythonBin || "python3");

    for (const rec of recognizers) {
      for (const level of noiseLevels) {
        for (const sample of samples) {
          const wavPath = wavMap.get(`${sample.id}@@${level}`) || sample.wav_path;
          try {
            const out = await rec.transcribe(wavPath, { sample, noiseLevel: level });
            const hypothesis = String(out?.text ?? "").trim();
            const cerRes = computeCer(sample.reference_text, hypothesis);
            const substitutions = extractSubstitutionSpans(cerRes.alignment);
            rows.push({
              clip_id: sample.id,
              date: sample.date,
              turn_id: sample.turn_id,
              tags: sample.tags,
              label_source: sample.label_source || "manual",
              input_source: sample.input_source || (sample.label_source === "e2e-playback" ? "e2e_playback" : "live_mic"),
              audio_origin: sample.audio_origin ?? null,
              dataset: sample.dataset ?? null,
              license: sample.license ?? null,
              voice_profile: sample.voice_profile ?? null,
              model_id: rec.id,
              model_label: rec.label || rec.id,
              noise_level: level,
              reference_text: sample.reference_text,
              hypothesis,
              cer: Number(cerRes.cer.toFixed(4)),
              edits: cerRes.edits,
              ref_len: cerRes.refLen,
              first_interim_ms: out?.firstInterimMs ?? null,
              final_ms: out?.finalMs ?? null,
              substitutions,
              error: null,
            });
          } catch (err) {
            rows.push({
              clip_id: sample.id,
              date: sample.date,
              turn_id: sample.turn_id,
              tags: sample.tags,
              label_source: sample.label_source || "manual",
              input_source: sample.input_source || (sample.label_source === "e2e-playback" ? "e2e_playback" : "live_mic"),
              audio_origin: sample.audio_origin ?? null,
              dataset: sample.dataset ?? null,
              license: sample.license ?? null,
              voice_profile: sample.voice_profile ?? null,
              model_id: rec.id,
              model_label: rec.label || rec.id,
              noise_level: level,
              reference_text: sample.reference_text,
              hypothesis: "",
              cer: 1,
              edits: normalizeForCer(sample.reference_text).length,
              ref_len: normalizeForCer(sample.reference_text).length,
              first_interim_ms: null,
              final_ms: null,
              substitutions: [],
              error: String(err?.message || err),
            });
          }
        }
      }
    }
  } finally {
    unregisterSignals();
    cleanup();
  }

  const analysis = analyzeResultsAndSuggestions(rows, {
    existingHotwords,
    existingCorrections,
  });

  const redactOpts = {
    secrets: secretsToRedact,
    paths: [
      runtimeDir,
      corpusDir,
      traceDir || "",
      outDir,
      tempDir,
      homedir(),
      process.cwd(),
      resolvedHelperExe ? dirname(resolvedHelperExe) : "",
      resolvedHelperExe || "",
    ].filter(Boolean),
  };

  const resultsJsonl = rows
    .map((r) => {
      const { substitutions: _omit, ...cleanRow } = r;
      return JSON.stringify(redactValue(cleanRow, redactOpts));
    })
    .join("\n");
  const reportMd = redactText(renderMarkdownReport(runId, samples.length, analysis), redactOpts);
  const suggestionsJson = JSON.stringify(redactValue(analysis.suggestions, redactOpts), null, 2);

  writeFileSync(join(outDir, "results.jsonl"), `${resultsJsonl}\n`, "utf8");
  writeFileSync(join(outDir, "report.md"), reportMd, "utf8");
  writeFileSync(join(outDir, "suggestions.json"), `${suggestionsJson}\n`, "utf8");

  const modelSuccessCounts = new Map();
  for (const r of rows) {
    const prev = modelSuccessCounts.get(r.model_id) || 0;
    modelSuccessCounts.set(r.model_id, prev + (r.error ? 0 : 1));
  }
  const failedModels = [...modelSuccessCounts.entries()]
    .filter(([, okCount]) => okCount === 0)
    .map(([id]) => id);
  const allFailed = rows.length === 0 || rows.every((r) => Boolean(r.error));
  const exitCode = allFailed || failedModels.length > 0 ? 1 : 0;
  const errorMessage =
    exitCode !== 0
      ? `识别测评存在全部失败的模型（${failedModels.join("、") || "全部样本"}），详见报告`
      : null;

  return {
    exitCode,
    errorMessage,
    archiveDir: outDir,
    runId,
    rows,
    analysis,
  };
}

function parseCliArgs(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => argv[++i];
    if (arg === "--corpus-dir") opts.corpusDir = next();
    else if (arg === "--runtime-dir") opts.runtimeDir = next();
    else if (arg === "--noise-levels") opts.noiseLevelsRaw = next();
    else if (arg === "--models") opts.models = next();
    else if (arg === "--helper") opts.helperExe = next();
    else if (arg === "--out-dir") opts.outDir = next();
    else if (arg === "--python") opts.pythonBin = next();
    else if (arg === "--help" || arg === "-h") opts.help = true;
  }
  return opts;
}

const isMain = process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isMain) {
  const opts = parseCliArgs(process.argv.slice(2));
  if (opts.help) {
    console.log(
      "用法: node scripts/voice-bench/asr-bench.mjs [--corpus-dir <path>] [--runtime-dir <path>] [--noise-levels 0,888,999] [--models sense_voice,qwen3_asr_0_6b] [--helper <path>] [--out-dir <path>]",
    );
    process.exit(0);
  }
  runAsrBench(opts)
    .then((res) => {
      if (res.exitCode !== 0) {
        console.error(redactText(res.errorMessage || "识别测评失败", { paths: [homedir(), process.cwd()] }));
        process.exit(res.exitCode);
      }
      console.log(res.archiveDir);
      process.exit(0);
    })
    .catch((err) => {
      console.error(redactText(err?.message || String(err), { paths: [homedir(), process.cwd()] }));
      process.exit(1);
    });
}
