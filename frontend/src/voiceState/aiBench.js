/**
 * VC4 / VD4 / VD6 语音中心“AI 联调与测试”状态清单与一键提示词生成纯逻辑（不依赖 React，便于单测）。
 * 严禁在输出中包含任何调试令牌、API 密钥、设备 token、IP 地址或本机绝对路径。
 */

import { debugInjectStateView } from "./devices.js";

/** 与 `scripts/voice-bench/asr-bench.mjs` 的 `LOCAL_ASR_CATALOG` 对齐的本地识别模型 ID 清单（传给 `--models` 可只测已安装本地模型、跳过计费云端模型）。 */
export const LOCAL_ASR_BENCH_MODEL_IDS = [
  "sense_voice",
  "sense_voice_2025",
  "x_asr_streaming",
  "qwen3_asr_0_6b",
  "firered_asr2_ctc",
];

function resolveLocalAsrBenchModelsArg(status) {
  const localModels = Array.isArray(status?.local?.models) ? status.local.models : [];
  const installedIds = localModels
    .filter((m) => m?.kind === "asr" && m?.installed === true && LOCAL_ASR_BENCH_MODEL_IDS.includes(m?.id))
    .map((m) => m.id);
  const chosen = installedIds.length > 0 ? installedIds : LOCAL_ASR_BENCH_MODEL_IDS;
  return chosen.join(",");
}

function summarizeBenchRecordingCoverage(benchRec, corpusCount) {
  if (!benchRec || typeof benchRec !== "object") {
    return {
      standardRecordedCount: null,
      onlineSampleCount: null,
      userSpeakers: [],
    };
  }
  const speakers = Array.isArray(benchRec.speakers) ? benchRec.speakers : [];
  const userSpeakers = speakers.filter((s) => {
    const name = String(s?.speaker || "").trim().toLowerCase();
    const dir = String(s?.speaker_dir || "").trim().toLowerCase();
    return name !== "online" && dir !== "online";
  });
  const totalRecordings = Number.isFinite(Number(benchRec.total_recordings))
    ? Math.max(0, Number(benchRec.total_recordings))
    : null;
  const completeSpeakerCount = Number.isFinite(Number(benchRec.complete_speaker_count))
    ? Math.max(0, Number(benchRec.complete_speaker_count))
    : 0;

  let standardRecordedCount = null;
  if (Number.isFinite(benchRec.standard_recorded_count)) {
    standardRecordedCount = Math.min(corpusCount, Math.max(0, Number(benchRec.standard_recorded_count)));
  } else if (userSpeakers.length > 0) {
    standardRecordedCount = Math.min(
      corpusCount,
      Math.max(0, ...userSpeakers.map((s) => Number(s?.recorded_count) || 0)),
    );
  } else if (completeSpeakerCount > 0) {
    standardRecordedCount = corpusCount;
  } else if (totalRecordings === 0) {
    standardRecordedCount = 0;
  }

  let onlineSampleCount = null;
  if (Number.isFinite(benchRec.online_count)) {
    onlineSampleCount = Math.max(0, Number(benchRec.online_count));
  } else if (totalRecordings === 0) {
    onlineSampleCount = 0;
  } else if (
    totalRecordings !== null &&
    completeSpeakerCount > 0 &&
    totalRecordings === completeSpeakerCount * corpusCount
  ) {
    onlineSampleCount = 0;
  }

  return {
    standardRecordedCount,
    onlineSampleCount,
    userSpeakers,
  };
}

function sanitizeDeviceIdForPrompt(raw) {
  const s = String(raw || "").trim();
  if (!s || s.length > 64) return "";
  if (/[^A-Za-z0-9:_-]/.test(s)) return "";
  return s;
}

function stripSecretsAndPaths(text) {
  return String(text ?? "")
    .replace(/\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{6,}/gi, "$1 [REDACTED]")
    .replace(/\b(?:sk|rk|pk)-[A-Za-z0-9_-]{16,}\b/g, "[REDACTED]")
    .replace(/(?:\/Users|\/home|\/var\/folders|\/private\/var|\/tmp)\/[^\s"'<>`,;)\]]+/g, "<path>")
    .replace(/\b[A-Za-z]:\\(?:Users|Documents and Settings|ProgramData|Temp|Windows)[^\s"'<>`,;)\]]*/gi, "<path>");
}

/** 4 种对话/过程记录音源标签（供对话记录与排障、过程记录及离线识别测评统一展示）。 */
export const VOICE_INPUT_SOURCE_LABELS = {
  live_mic: { text: "小智实时说话", tone: "ok" },
  e2e_playback: { text: "整体测试外放录音", tone: "warn" },
  digital_inject: { text: "数字录音直通", tone: "info" },
  text_inject: { text: "文字直通（无音频）", tone: "off" },
};

export function voiceInputSourceView(trace) {
  const raw = String(trace?.input_source || "").trim();
  if (raw && Object.hasOwn(VOICE_INPUT_SOURCE_LABELS, raw)) {
    return { key: raw, ...VOICE_INPUT_SOURCE_LABELS[raw] };
  }
  if (trace?.bench_window) {
    return { key: "e2e_playback", ...VOICE_INPUT_SOURCE_LABELS.e2e_playback };
  }
  if (trace?.simulated) {
    return { key: "text_inject", ...VOICE_INPUT_SOURCE_LABELS.text_inject };
  }
  return { key: "live_mic", ...VOICE_INPUT_SOURCE_LABELS.live_mic };
}

/** 汇总本机 4 项联调就绪状态清单（每项含打勾/打叉与一句修复指引；识别录音标注已并入完整过程记录与对话记录）。 */
export function aiBenchChecklistView(status) {
  const debugInject = debugInjectStateView(status);
  const sessions = Array.isArray(status?.sessions) ? status.sessions : [];
  const onlineDevices = sessions
    .map((s) => sanitizeDeviceIdForPrompt(s?.device_id))
    .filter(Boolean);
  const activeBenchWindows = sessions
    .filter((s) => s?.bench_window?.active === true)
    .map((s) => ({
      device_id: sanitizeDeviceIdForPrompt(s.device_id),
      max_minutes: Number(s.bench_window.max_minutes) || 30,
      expires_at: Number(s.bench_window.expires_at) || 0,
    }))
    .filter((w) => w.device_id);

  const asrCorpus = status?.asr_corpus && typeof status.asr_corpus === "object" ? status.asr_corpus : {};
  const corpusEnabled = asrCorpus.enabled === true || status?.process_trace_enabled === true;
  const traces = Array.isArray(status?.debug_traces) ? status.debug_traces : [];
  const traceLabeledCount = traces.filter((t) => Boolean(t?.reference_text)).length;
  const totalCorpusCount = (Number(asrCorpus.total_count) || 0) + traces.filter((t) => Boolean(t?.has_audio)).length;
  const labeledCount = Math.max(Number(asrCorpus.labeled_count) || 0, traceLabeledCount);

  const benchRec =
    status?.bench_recordings && typeof status.bench_recordings === "object"
      ? status.bench_recordings
      : null;
  const corpusCount = Number(benchRec?.corpus_count) || 13;
  const totalRecordings = Number(benchRec?.total_recordings) || 0;
  const speakerCount = Number(benchRec?.speaker_count) || 0;
  const completeSpeakerCount = Number(benchRec?.complete_speaker_count) || 0;
  const benchSpeakers = Array.isArray(benchRec?.speakers) ? benchRec.speakers : [];
  const recordingsReady = completeSpeakerCount > 0;
  const coverage = summarizeBenchRecordingCoverage(benchRec, corpusCount);

  const items = [
    {
      key: "debug_inject",
      label: "允许 AI 联调开关",
      ok: debugInject.enabled,
      statusText: debugInject.enabled
        ? debugInject.port
          ? `已开启（127.0.0.1:${debugInject.port}）`
          : "已开启（等待服务启动）"
        : "未开启",
      fixHint: debugInject.enabled
        ? "已就绪；仅限本机 127.0.0.1，24 小时或关闭开关自动失效。"
        : "打开右侧开关允许本机联调脚本连接（仅限 127.0.0.1，24 小时自动关，测试期间锁死激光动作）。",
      goto: { tab: "records", group: "ai-bench-hub", target: "voice-debug-inject-switch" },
    },
    {
      key: "online_device",
      label: "小智连接状态",
      ok: onlineDevices.length > 0,
      statusText: onlineDevices.length > 0 ? `${onlineDevices.length} 台在线` : "无在线设备",
      fixHint:
        onlineDevices.length > 0
          ? "已连上小智，放在离电脑喇叭 30–50 cm 处即可。"
          : "喊“你好小智”或按小智对话键唤醒，或在“服务”页开启常连模式。",
      goto: { tab: "devices", group: "builtin-devices" },
    },
    {
      key: "bench_recordings",
      label: "测试真人语音素材",
      ok: recordingsReady,
      statusText: recordingsReady
        ? `${completeSpeakerCount} 位说话人已录齐（${totalRecordings} 条）`
        : totalRecordings > 0
          ? `已缓存 ${totalRecordings} 条`
          : "AI 自动拉取缓存",
      fixHint:
        totalRecordings > 0
          ? "每次测试直接复用本机已缓存的真人语音，无需重复说话；想补录可展开下方录音台。"
          : "无需你开口说话：发提示词给 AI 后会自动下载公开中文真人语音并缓存复用，也可展开下方自行录制。",
      goto: { tab: "records", group: "ai-bench-hub", target: "voice-bench-recordings-box" },
    },
    {
      key: "bench_window",
      label: "真机安全锁",
      ok: true,
      active: activeBenchWindows.length > 0,
      statusText:
        activeBenchWindows.length > 0
          ? `整体测试中（物理动作已锁定 · ${activeBenchWindows.length} 台）`
          : "测试时自动锁机",
      fixHint:
        activeBenchWindows.length > 0
          ? "当前已锁定该设备所有物理动作；如需提前停止请点“立即结束测试”。"
          : "AI 开测时自动上锁并开启完整过程记录，测完自动解锁恢复，绝不误触发真机移动或出光。",
      goto: { tab: "devices", group: "builtin-devices" },
    },
  ];

  return {
    items,
    readyCount: items.filter((item) => item.ok).length,
    totalCount: items.length,
    debugEnabled: debugInject.enabled,
    onlineDevices,
    activeBenchWindows,
    corpusEnabled,
    totalCorpusCount,
    labeledCount,
    corpusCount,
    totalRecordings,
    speakerCount,
    completeSpeakerCount,
    benchSpeakers,
    recordingsReady,
    standardRecordedCount: coverage.standardRecordedCount,
    onlineSampleCount: coverage.onlineSampleCount,
  };
}

/**
 * 根据本机实时状态生成可直接粘贴给 Claude / Codex / agy 的联调提示词。
 * `kind`: `"e2e"`（整体测试，主推） | `"text"`（VA 文字直通） | `"asr"`（VB 离线识别测评）。
 * `options`: `{ compareDigital?: boolean, noise?: "" | "fan" | "pump" | "burst", speaker?: string }`
 * 严禁包含任何令牌原文、API 密钥、设备 token 或本机绝对路径。
 */
export function buildAiBenchPrompt(status, kind = "e2e", options = {}) {
  const view = aiBenchChecklistView(status);
  const targetArg = view.onlineDevices.length === 1 ? ` --device ${view.onlineDevices[0]}` : "";
  const compareDigital = Boolean(options?.compareDigital);
  const rawNoise = String(options?.noise || "").trim();
  const noiseArg = ["fan", "pump", "burst"].includes(rawNoise) ? ` --noise ${rawNoise} --noise-gain 0.22` : "";
  const digitalArg = compareDigital ? " --compare-digital" : "";
  const rawSpeaker = String(options?.speaker || "").trim();
  const safeSpeaker =
    rawSpeaker && rawSpeaker.length <= 32 && !/["'`\\/\r\n]/.test(rawSpeaker) && !rawSpeaker.includes("..")
      ? /\s/.test(rawSpeaker)
        ? ` --speaker "${rawSpeaker}"`
        : ` --speaker ${rawSpeaker}`
      : "";

  const debugStateText = view.debugEnabled
    ? "已开启"
    : "未开启（需用户在「语音中心 › 记录与诊断 › AI 联调与测试」开启）";
  const deviceStateText =
    view.onlineDevices.length === 0
      ? "暂无在线小智（需用户唤醒或开启常连模式）"
      : `已有 ${view.onlineDevices.length} 台小智在线（${view.onlineDevices.join(", ")}）`;
  const localModelsArg = resolveLocalAsrBenchModelsArg(status);

  if (kind === "text") {
    const prompt = [
      "# 真小智文字直通联调",
      "",
      "遵守仓库 `AGENTS.md` 与 `docs/developer/safety.md`（含 §4.6 联调安全边界）。",
      "",
      "## 前提",
      `- 本机状态：调试注入开关${debugStateText}；${deviceStateText}。`,
      "- 开关与设备唤醒由用户完成；预检不满足时暂停并告知用户缺什么。当前联调范围只到“生成预览”，不涉及真实激光动作。",
      "",
      "## 运行与归档",
      `- 运行：\`node scripts/voice-bench/live-xiaozhi-debug.mjs${targetArg}\``,
      "- 归档输出在 `<runtime>/voice-bench/<run-id>/`，参数与格式详见 `scripts/voice-bench/README.md`。",
      "",
      "## 汇报与收尾",
      "- 汇总各轮路径、耗时、工具与门禁结果及异常项；如有改动，复测验证后再汇报。",
      "- 收尾提醒用户关闭“允许本机 AI 调试注入”开关。",
    ].join("\n");
    return stripSecretsAndPaths(prompt);
  }

  if (kind === "asr") {
    const corpusLine =
      view.labeledCount > 0
        ? `当前已有 ${view.labeledCount} 条已标注真值的过程记录/识别语料。`
        : "当前尚未检测到已标真值的录音，可先跑整体测试或在「记录与诊断 › 对话记录与排障」确认真值。";
    const prompt = [
      "# 离线识别（ASR）测评与候选审核",
      "",
      "遵守仓库 `AGENTS.md` 与 `docs/developer/safety.md`（含 §4.7 录音隐私与候选边界）。",
      "",
      "## 前提",
      `- ${corpusLine}`,
      "",
      "## 运行与归档",
      `- 默认先测本地模型：\`node scripts/voice-bench/asr-bench.mjs --models ${localModelsArg} --noise-levels clean,fan,pump\``,
      "- 云端模型（`--models cloud`）会产生调用费用，按需单独跑一轮或先征求用户同意。",
      "- 归档输出在 `<runtime>/voice-bench/asr-<run-id>/`，参数与格式详见 `scripts/voice-bench/README.md`。",
      "",
      "## 汇报",
      "- 汇总按语音来源分组的字错率、命令词召回与误报、最差片段，以及 `suggestions.json` 候选审核意见（候选不自动生效）。",
    ].join("\n");
    return stripSecretsAndPaths(prompt);
  }

  // 主提示词（e2e）：精简为任务特有信息 + 引用项目规范 + 4 角色子代理分工
  const stdText =
    view.standardRecordedCount === null
      ? `未知 / ${view.corpusCount}`
      : `${view.standardRecordedCount} / ${view.corpusCount}`;
  const onlineText =
    view.onlineSampleCount === null ? "未知" : `${view.onlineSampleCount} 条`;
  const materialStatusLine = view.recordingsReady
    ? `标准语料已录 ${stdText} 句（${view.completeSpeakerCount} 位说话人已录齐），网上样本 ${onlineText}`
    : `标准语料已录 ${stdText} 句，网上样本 ${onlineText}`;

  const rawMaxOnline = options?.maxOnline;
  const maxOnline =
    rawMaxOnline !== undefined && Number.isInteger(Number(rawMaxOnline)) && Number(rawMaxOnline) >= 0
      ? Number(rawMaxOnline)
      : 20;
  const maxOnlineArg = ` --max-online ${maxOnline}`;

  const prompt = [
    "# 真小智语音链路自动联调",
    "",
    "遵守仓库 `AGENTS.md` 与 `docs/developer/safety.md`（含 §4.6 / §4.7 联调安全边界）。",
    "",
    "## 目标",
    "在真小智上自动联调语音链路，找出问题并对症改进。",
    "",
    "## 前提",
    "- 代码同步：先 `git pull`，再跑 `node scripts/verify.mjs --finish` 快更并提示用户重启 MOSS；遇冲突或验证失败即停，汇报失败项和最后 30 行输出。",
    `- 本机状态：调试注入开关${debugStateText}；${deviceStateText}；素材状态：${materialStatusLine}。`,
    "- 调试注入开关、小智摆放（离喇叭 30–50 cm）和唤醒由用户完成；脚本预检不满足（无令牌、无在线设备、校准失败）时暂停，告知用户具体缺什么。",
    "- 当前联调范围只到“生成预览”，不涉及真实激光动作。",
    "",
    "## 素材",
    "- 优先复用 `<runtime>/voice-bench/recordings/` 下的真人录音缓存。",
    "- 不够时可从网上公开中文真人语音集补充，按许可使用，转为 16 kHz 单声道 WAV 放入 `recordings/online/`，同名 `.json` 写 `text`、`dataset`、`license`、`source_url`。",
    "- 网上语音覆盖不了 13 句工作室标准语料，缺失句的后半段由文字直通覆盖。",
    "",
    "## 工具",
    `- 文字直通（跳过麦克风测后半段）：\`node scripts/voice-bench/live-xiaozhi-debug.mjs${targetArg}\``,
    `- 真机外放全链路（4 环节归因）：\`node scripts/voice-bench/xiaozhi-e2e.mjs${targetArg}${safeSpeaker}${maxOnlineArg} --max-minutes 30${digitalArg}${noiseArg}\``,
    `- 离线识别测评（默认先测本地模型，云端 \`--models cloud\` 会产生费用）：\`node scripts/voice-bench/asr-bench.mjs --models ${localModelsArg} --noise-levels clean,fan,pump\``,
    "- 归档产出在 `<runtime>/voice-bench/` 下各自运行目录，参数与输出格式详见 `scripts/voice-bench/README.md`。",
    "",
    "## 流程",
    "如果你支持子代理，按以下角色分工；不支持就由你自己按同样的阶段顺序执行。",
    "1. 诊断（只测不改）：跑完三个脚本，输出诊断单。按 4 个环节（拾音判停 / 识别纠偏 / 意图工具 / 合成播放）列出问题，按严重程度排序，每条附证据和怀疑的位置。",
    "2. 对症修复：一次只处理一个问题，交给一个修复子代理，只给它这个问题的证据和相关文件，做最小改动并补测试。",
    "3. 独立复测：由不参与修复的一方重跑相关测试，对比前后指标，变好保留，否则撤回。",
    "4. 提交前审查：由没参与修复的审查子代理只看 diff，确认后再本地提交。",
    "",
    "真机测试一次只能跑一个，修复按问题排队进行；不碰真机的离线比较可以并行。最多 3 轮。",
    "",
    "## 汇报",
    "- 开头先给出素材覆盖率：标准语料 X / 13 句有真人录音、网上样本 N 条、工作室术语识别是否覆盖（未覆盖建议用户在 App「录制测试语料」录一遍）。",
    "- 随后给出诊断单、每轮改动与指标变化、未解决的问题，以及需要用户决定的事项。",
    "",
    "## 收尾",
    "- 确认测试时间窗已关闭，提醒用户到「语音中心 › 记录与诊断 › AI 联调与测试」关闭“允许本机 AI 调试注入”开关（并按需关闭完整过程记录）。",
  ].join("\n");

  return stripSecretsAndPaths(prompt);
}
