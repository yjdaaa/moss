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
      label: "① 允许 AI 联调开关",
      ok: debugInject.enabled,
      statusText: debugInject.enabled
        ? debugInject.port
          ? `已开启（127.0.0.1:${debugInject.port}）`
          : "已开启（等待服务启动）"
        : "未开启",
      fixHint: debugInject.enabled
        ? "已就绪；本机临时令牌 24 小时或关开关自动失效。"
        : "点右侧“立即开启”允许本机 AI 联调脚本连接（仅限本机 127.0.0.1）。",
      goto: { tab: "records", group: "ai-bench-hub", target: "voice-debug-inject-switch" },
    },
    {
      key: "online_device",
      label: "② 小智连接状态",
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
      label: "③ 测试真人语音素材",
      ok: recordingsReady,
      statusText: recordingsReady
        ? `${completeSpeakerCount} 位说话人已录齐（共 ${totalRecordings} 条 · 直接复用）`
        : totalRecordings > 0
          ? `本机已缓存 ${totalRecordings} 条真人语音（直接复用）`
          : "本机暂无缓存（AI 自动从网上拉取并缓存）",
      fixHint:
        totalRecordings > 0
          ? "每次测试直接复用本机已缓存的真人语音，无需重复说话；想补录可展开下方可选录音台。"
          : "无需你开口说话：复制提示词发给 AI 后，AI 会自动从网上公开中文真人语音库下载并缓存到本机复用（也可展开下方可选“录制测试语料”自己录）。",
      goto: { tab: "records", group: "ai-bench-hub", target: "voice-bench-recordings-box" },
    },
    {
      key: "bench_window",
      label: "④ 真机安全锁",
      ok: true,
      active: activeBenchWindows.length > 0,
      statusText:
        activeBenchWindows.length > 0
          ? `整体测试中（物理动作已锁定 · ${activeBenchWindows.length} 台）`
          : "已保护（测试时自动锁死激光机动作）",
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

  const debugPrereq = !view.debugEnabled
    ? "在桌面主窗口「语音中心 › 记录与诊断 › AI 联调与测试」开启“允许本机 AI 调试注入”开关（自动生成 24 小时有效的本机 0600 令牌文件）。"
    : "“允许本机 AI 调试注入”开关当前已开启（运行目录相对路径 debug/voice-debug.token，切勿打印或泄露令牌内容）。";
  const devicePrereq =
    view.onlineDevices.length === 0
      ? "唤醒小智（喊“你好小智”或按对话键），或开启常连模式并确认设备在语音中心显示在线。"
      : `当前已有 ${view.onlineDevices.length} 台小智在线（${view.onlineDevices.join(", ")}）。`;

  if (kind === "text") {
    const prompt = [
      "# 任务：运行真小智“文字直通”自动化联调（VA）并分析归档",
      "",
      "## 规则约束",
      "- 严格遵守仓库 `AGENTS.md` 与 `docs/developer/safety.md` §4.6；",
      "- 不执行 `git push`，不绕过或放宽任何安全门禁与测试断言；",
      "- 严禁打印、复制或向输出写入任何调试令牌、API 密钥、设备 token 或本机绝对路径（统一使用 `<runtime>` / `<path>` 相对说明）。",
      "",
      "## 准备与快更",
      "1. 如本机代码有更新，先同步仓库并运行 `node scripts/verify.mjs --finish` 完成检查与本机桌面程序快更；",
      `2. ${debugPrereq}`,
      `3. ${devicePrereq}`,
      "",
      "## 运行命令",
      "```bash",
      `node scripts/voice-bench/live-xiaozhi-debug.mjs${targetArg}`,
      "```",
      "",
      "## 归档位置与回传内容",
      "- 归档输出目录：运行目录相对路径 `<runtime>/voice-bench/<run-id>/`；",
      "- 请读取并回传以下文件摘要：`report.md`、`summary.json`、`turns.jsonl` 与 `timeline.txt` 中的异常或慢阶段；",
      "- 核对模拟门禁是否成功拒绝“直接开始雕刻”，以及各轮路径、工具参数与播放卡顿情况。",
      "",
      "## 修改原则",
      "- 可以根据报告修改代码，改完重新运行验证；不要修改语料来让测试通过。",
    ].join("\n");
    return stripSecretsAndPaths(prompt);
  }

  const localModelsArg = resolveLocalAsrBenchModelsArg(status);

  if (kind === "asr") {
    const corpusLine =
      view.labeledCount > 0
        ? `当前已有 ${view.labeledCount} 条已标注真值的过程记录/识别语料。`
        : "当前尚未检测到已标真值的录音：可先跑一次整体测试（自动在完整过程记录中写入 `e2e_playback` 外放标准真值），或在「记录与诊断 › 对话记录与排障」开启完整过程记录后点“纠偏此句 / ✓ 识别正确”顺手打标。";
    const prompt = [
      "# 任务：运行识别（ASR）离线测评（VB）并审核候选词",
      "",
      "## 规则约束",
      "- 严格遵守仓库 `AGENTS.md` 与 `docs/developer/safety.md` §4.7；",
      "- 不执行 `git push`，不打印任何云端密钥、令牌或本机绝对路径；",
      "- 识别录音与标注仅用于离线测评，严禁进入诊断包或声纹识别。",
      "",
      "## 前提检查",
      "1. 先执行 `git pull` 拉取最新代码，再运行 `node scripts/verify.mjs --finish` 快更本机桌面程序并提示用户重启 MOSS；拉取冲突或验证失败时停下汇报失败项和最后 30 行输出，不要自行解决合并冲突或强行继续；",
      `2. ${corpusLine}`,
      "",
      "## 运行命令（默认先只测本地模型，避免重复云端计费）",
      "```bash",
      `node scripts/voice-bench/asr-bench.mjs --models ${localModelsArg} --noise-levels clean,fan,pump`,
      "```",
      "- 脚本自动扫描 `<runtime>/logs/process-trace/`（完整过程记录中带真值标记的 `voice-files/*-user.wav`）与 `<runtime>/asr-corpus/`，并按语音来源（网上真人 / 我的录音 / 小智实时说话 / 数字直通）分组统计；",
      "- **云端费用提醒**：已配置的云端识别模型会产生调用费用，最多各测一轮（`--models cloud`），或者先在汇报里征求用户同意后再测。",
      "",
      "## 归档位置与回传内容",
      "- 归档输出目录：运行目录相对路径 `<runtime>/voice-bench/asr-<run-id>/`；",
      "- 请读取并回传：`report.md`（含按来源分组字错率、命令词召回与误报、最差 30 条）与 `suggestions.json`。",
      "",
      "## 修改原则",
      "- `suggestions.json` 里的热词、同音纠错和数字规范化候选不要自动应用，须逐条人工/AI 复核并确认不触碰停止/退出命令词后再改代码并补测试；",
      "- 不得修改测评脚本（`scripts/voice-bench/*`）的判定阈值、语料、断言或报告逻辑来让结果变好看。",
    ].join("\n");
    return stripSecretsAndPaths(prompt);
  }

  // 默认主推：一键全自动联调与调试（先拉代码快更 → 准备真人素材 → 文字直通快检 → 真小智外放全链路 → 本地离线识别横评 → 有边界的改代码复测与收尾）
  const stdText =
    view.standardRecordedCount === null
      ? `未知 / ${view.corpusCount}`
      : `${view.standardRecordedCount} / ${view.corpusCount}`;
  const onlineText =
    view.onlineSampleCount === null ? "未知" : `${view.onlineSampleCount} 条`;
  const recordingPrereq = view.recordingsReady
    ? `当前本机素材状态：标准语料已录 ${stdText} 句（${view.completeSpeakerCount} 位说话人已录齐），网上样本 ${onlineText}；**直接复用现有缓存开测，严禁重复下载或让用户重复开口录制**。`
    : `当前本机素材状态：标准语料已录 ${stdText} 句，网上样本 ${onlineText}。**无需先让用户自己对着麦克风说话**：请 AI 先检查 \`<runtime>/voice-bench/recordings/\`，若为空或素材不足，**由 AI 自主从网上公开中文真人语音集下载真人发音样本并缓存到 \`<runtime>/voice-bench/recordings/online/\` 长期复用**（如需覆盖 13 句工作室标准术语与意图句，可在 App「录制测试语料」里至少录完一位说话人的全部语料；绝不用 TTS 合成音充当真人语音）。`;

  const manualPrereqs = [
    "把小智放在离电脑喇叭 30–50 cm 处，调至合适电脑音量并避免麦克风被遮挡（由用户在现场完成）。",
    debugPrereq,
    devicePrereq,
    recordingPrereq,
  ];

  const rawMaxOnline = options?.maxOnline;
  const maxOnline =
    rawMaxOnline !== undefined && Number.isInteger(Number(rawMaxOnline)) && Number(rawMaxOnline) >= 0
      ? Number(rawMaxOnline)
      : 20;
  const maxOnlineArg = ` --max-online ${maxOnline}`;

  const prompt = [
    "# 任务：真小智一键全自动联调与调优（拉取快更 → 准备真人语音 → 文字直通快检 → 真机外放全链路 → 离线识别横评 → 有边界调优与收尾）",
    "",
    "## 仓库与规则约束（硬红线，绝不可违反）",
    "- 严格遵守仓库 `AGENTS.md` 与 `docs/developer/safety.md` §4.6 / §4.7；",
    "- **前提未满足必须暂停**：开启“允许本机 AI 调试注入”、把小智放到喇叭旁、唤醒小智，这些只能由用户在界面或现场完成。脚本预检发现未满足时（无令牌、无在线设备、校准失败），AI 必须暂停，明确告诉用户具体缺哪一步、怎么做，等用户确认后再继续；",
    "- **严禁绕过开关与门禁**：严禁通过修改 `settings.json`、数据库、令牌文件或任何运行目录文件来开启开关、伪造令牌或改变设备状态；",
    "- **严禁修改安全门禁相关代码**：严禁修改 `crates/moss-voice-server/src/sim_guard.rs`、测试时间窗（`crates/moss-voice-server/src/debug_api.rs` 的 bench window 部分）、`TurnGate`、停止和急停快路径、调试入口的鉴权以及主窗口 ACL；",
    "- **严禁触碰真实激光机动作**：严禁让真实激光机运动、出光或发送任务，目前的联调范围只到“生成预览”；所有外放测试必须在测试时间窗内执行，物理动作全程锁定，结束时务必关闭窗口；",
    "- **不推送、不泄露**：可本地提交（遵守 `AGENTS.md` 提交格式），绝不执行 `git push`；严禁打印、记录或外传调试令牌、API 密钥、设备 token 或本机绝对路径（路径一律用 `<runtime>/...` 相对说明）。",
    "",
    "## 第零步：拉取最新代码、快更并核对前提",
    "1. 先执行 `git pull` 拉取最新代码；",
    "2. 再运行 `node scripts/verify.mjs --finish` 完成验证并快更本机桌面程序，并提示用户重启 MOSS；",
    "3. **失败即停**：若 `git pull` 出现冲突或 `--finish` 验证失败，立刻停下汇报失败项和最后 30 行输出，不要强行继续，也不要自行解决合并冲突；",
    ...manualPrereqs.map((line, i) => `${i + 4}. ${line}`),
    "- 再次确认：上述开关、摆放距离与唤醒只能由用户完成；脚本预检报无令牌、无在线设备或校准失败时必须暂停并指导用户处理，不得私改运行目录配置或令牌文件。",
    "",
    "## 全自动四步调试流程（前提全部就绪后按顺序执行）",
    "",
    "### 第一步：准备真人语音素材（有本机缓存直接复用，缺素材时 AI 按许可与限量从网上拉取并永久缓存）",
    "- 先检查 `<runtime>/voice-bench/recordings/` 下是否已有 `.wav` 真人录音文件：",
    "  - **若已有真人语音缓存，直接复用，严禁每次测试重复下载或要求用户开口说话**；",
    "  - **若目录为空或素材不足**，由 AI 自主从网上公开中文真人语音数据集补充真实人声（**必须是真人发音，严禁用系统 `say` 或本地/云端 TTS 合成音冒充**），并严格遵守以下联网下载约束：",
    "    - **许可证**：只使用明确允许使用的公开数据，例如 Mozilla Common Voice 中文（CC0）、AISHELL-1（Apache-2.0）、`k2-fsa/sherpa-onnx` 仓库自带的测试音频（遵循仓库许可）；**不得使用标注“非商用”或“禁止演绎”的数据**；需要登录、申请或同意条款才能下载的数据集，直接跳过并在汇报里说明，不得绕过。",
    "    - **限量**：单次补充不超过 200 条，总计不超过 300 MB。",
    "    - **只拿需要的文件**：只下载音频和对应的文字。下载内容放在单独的临时目录里，**不运行下载得到的任何脚本或程序**，用 `ffmpeg` / `afconvert` / `python3` 转为 `16 kHz`、单声道、16-bit PCM WAV 后，只把 `.wav` 和同名 `.json` 放进 `<runtime>/voice-bench/recordings/online/`。",
    "    - **必填元数据（4 字段缺一不可）**：`<runtime>/voice-bench/recordings/online/<id>.json` 必须包含 `text`、`dataset`、`license`、`source_url`（例如 `{\"text\":\"对应的中文原话\",\"dataset\":\"AISHELL-1\",\"license\":\"Apache-2.0\",\"source_url\":\"https://...\"}`）；缺少许可或数据集信息的条目会被脚本直接跳过并列进“被跳过素材”。",
    "    - **不进仓库**：下载的数据只存在本机运行目录 `<runtime>/voice-bench/recordings/online/`，绝不加入仓库，也不提交。",
    "    - **优先挑合适的句子**：优先选和工作室场景相近的短句（3–15 秒）；不选像指令的句子（播放音乐、调音量、调亮度、静音、关机、重启、停、退下、开始等，脚本也会自动过滤并记入“被跳过素材”）。",
    "- **素材覆盖率说明（必须如实认知）**：网上公开语音是随机句子，覆盖不了 13 句工作室标准语料（椴木、切缝、走边框等术语，以及意图和工具类句子）。用户没录的标准语料，在外放测试中会被跳过；这些语料的后半段由第二步“文字直通”覆盖。",
    "",
    "### 第二步：先跑“文字直通”快检识别后半段（跳过麦克风，毫秒级排查后半段逻辑）",
    "```bash",
    `node scripts/voice-bench/live-xiaozhi-debug.mjs${targetArg}`,
    "```",
    "- 检查 `<runtime>/voice-bench/<run-id>/report.md` 与 `summary.json`：核对 0ms 同音纠偏、句尾防抢话、本地快答、大模型工具调用、模拟安全门禁（拒绝“直接开始雕刻”）与 TTS 播放缓冲是否全绿。",
    "",
    "### 第三步：跑“真小智喇叭外放”全链路整体测试（真喇叭外放 → 真麦克风 → 4 环节归因）",
    "```bash",
    `node scripts/voice-bench/xiaozhi-e2e.mjs${targetArg}${safeSpeaker}${maxOnlineArg} --max-minutes 30${digitalArg}${noiseArg}`,
    "```",
    "- 来源按目录自动区分：`recordings/online/` 一律标为 `online-human`（网上真人，受 `--max-online 20` 上限与 `.rotation.json` 轮换控制，`--max-online 0` 表示不用网上语音），用户自录目录标为 `human-recording`（我的录音）；网上语音指标只反映通用识别能力，不代表用户本人识别效果；",
    "- 若预计总时长超过 `--max-minutes`，脚本会自动分批并批间关闭/重开测试时间窗且复核设备会话一致（绝不延长单窗超过 60 分钟上限）；加 `--speaker <标签>` 可指定某一位说话人或 `online`；",
    "- 运行期间脚本会自动开启完整过程记录（`logs/process-trace/<日期>/`），在记录与 `.label.json` 中显式标记音源（`live_mic` / `e2e_playback` / `digital_inject` / `text_inject`）与素材来源（`audio_origin`: `online-human` / `human-recording` 及 `dataset` / `license`）；",
    "- 若初测发现识别错字、吞首字或抢话，请 AI **自主追加运行** `--compare-digital`（同份真人录音走服务端数字注入对照，量化麦克风与房间损耗）以及 `--noise fan --noise-gain 0.22`（叠加排风扇背景噪声）定位根因；",
    "- 默认不把小智麦克风历史录音当作外放素材；仅在确需补充时显式加 `--include-device-corpus`（报告会标明“二次录制”）。",
    "",
    "### 第四步：跑“离线识别（ASR）离线测评”（默认先只测本地模型，控制云端费用）",
    "```bash",
    `node scripts/voice-bench/asr-bench.mjs --models ${localModelsArg} --noise-levels clean,fan,pump`,
    "```",
    "- 默认通过 `--models` 先只测已安装的本地识别模型，自动扫描 `<runtime>/logs/process-trace/` 中带真值标记的录音（含刚跑完的 `e2e_playback` 外放回采与日常 `live_mic` 真人说话），横评安静 / 排风扇 / 气泵下的字错率（CER）、命令词召回与误报；",
    "- **云端费用提醒**：已配置的云端识别模型会产生调用费用，最多各测一轮（`--models cloud`），或者先在汇报里征求用户同意后再测。",
    "",
    "## 归档读取、调优边界与汇报要求",
    "- **汇报开头必须先给出素材覆盖率**（如实列出，不可省略）：",
    "  - 标准语料 `X / 13` 句有真人录音（说话人：…）；",
    "  - 网上真人样本 `N` 条（数据集：…，许可：…）；",
    "  - 工作室术语识别：已覆盖 / 未覆盖（未覆盖时，建议用户在 App「录制测试语料」里录一遍）。",
    "- 请汇总回传上述三步的归档报告摘要：",
    "  1. `<runtime>/voice-bench/e2e-<run-id>/report.md`、`summary.json`、`turns.jsonl`、`audio-manifest.json` 与 `timeline.txt`（4 环节通过率、按语音来源与说话人统计、被跳过素材、抢话/吞首字/高 CER 轮次、数字对照损耗）；",
    "  2. `<runtime>/voice-bench/<run-id>/report.md`（文字直通后半段耗时与门禁结果）；",
    "  3. `<runtime>/voice-bench/asr-<run-id>/report.md` 与 `suggestions.json`（多模型 × 噪声字错率对比与纠错候选词）。",
    "- **自动改代码与复测边界（严格遵守）**：",
    "  1. **最多 3 轮**修改和复测；",
    "  2. **每轮只改一个参数或一处逻辑**（例如 `PREROLL_MS`、`BASE_SILENCE_MS`、`UNFINISHED_HOLD_WINDOW`、同音纠错表、热词或路由逻辑），并在汇报里记录：改了什么、为什么改、改前和改后的关键指标（对应环节的通过率、CER、延迟等）；",
    "  3. **指标没有变好或者变差时，撤回这一轮改动**；",
    "  4. **禁止作弊**：不得修改测评脚本（`scripts/voice-bench/*`）的判定阈值、语料、断言或报告逻辑来让结果变好看，也不得删除或放宽仓库中的任何测试；不要修改语料来让测试通过；",
    "  5. `suggestions.json` 里的候选不要自动盲目应用，须逐条复核并遵守命令词保护规则后才可写入代码并补测试；",
    "  6. **分级验证**：每轮改完先跑 `node scripts/verify.mjs --affected --fmt-write`，全部修改完成后再跑一次 `node scripts/verify.mjs --finish`；",
    "  7. **提交边界**：改动可以本地提交（遵守 `AGENTS.md` 的 `<类型>: <中文结论>` 提交格式），但**绝不推送**（不执行 `git push`）；",
    "  8. **最终汇报必须包含**：素材覆盖率、每一轮的改动与指标变化、仍未解决的问题、建议用户决定的事项。",
    "",
    "## 收尾",
    "1. 确认测试时间窗已关闭（设备不再处于“整体测试中（物理动作已锁定）”状态）；",
    "2. 提醒用户到桌面端「语音中心 › 记录与诊断 › AI 联调与测试」关闭“允许本机 AI 调试注入”开关，关闭后本机令牌立即失效；",
    "3. 如果测试期间开启了完整过程记录，提醒用户按需到「语音中心 › 记录与诊断 › 对话记录与排障」关闭。",
  ].join("\n");

  return stripSecretsAndPaths(prompt);
}
