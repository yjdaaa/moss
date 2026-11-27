/**
 * VC4 / VD4 / VD6 语音中心“AI 联调与测试”状态清单与一键提示词生成纯逻辑（不依赖 React，便于单测）。
 * 严禁在输出中包含任何调试令牌、API 密钥、设备 token、IP 地址或本机绝对路径。
 */

import { debugInjectStateView } from "./devices.js";

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
      : {};
  const corpusCount = Number(benchRec.corpus_count) || 13;
  const totalRecordings = Number(benchRec.total_recordings) || 0;
  const speakerCount = Number(benchRec.speaker_count) || 0;
  const completeSpeakerCount = Number(benchRec.complete_speaker_count) || 0;
  const benchSpeakers = Array.isArray(benchRec.speakers) ? benchRec.speakers : [];
  const recordingsReady = completeSpeakerCount > 0;

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
      `1. ${corpusLine}`,
      "2. 如代码有更新，先同步代码并运行 `node scripts/verify.mjs --finish`。",
      "",
      "## 运行命令",
      "```bash",
      "node scripts/voice-bench/asr-bench.mjs --noise-levels clean,fan,pump",
      "```",
      "- 脚本自动扫描 `<runtime>/logs/process-trace/`（完整过程记录中带真值标记的 `voice-files/*-user.wav`）与 `<runtime>/asr-corpus/`，并按音源（`live_mic` 小智实时说话 vs `e2e_playback` 整体测试外放录音）分组统计。",
      "",
      "## 归档位置与回传内容",
      "- 归档输出目录：运行目录相对路径 `<runtime>/voice-bench/asr-<run-id>/`；",
      "- 请读取并回传：`report.md`（含小智实时说话 vs 整体测试外放录音分组字错率、命令词召回与误报、最差 30 条）与 `suggestions.json`。",
      "",
      "## 修改原则",
      "- `suggestions.json` 里的热词、同音纠错和数字规范化候选不要自动应用，须逐条人工/AI 复核并确认不触碰停止/退出命令词后再改代码并补测试；",
      "- 不要修改语料来让测试通过。",
    ].join("\n");
    return stripSecretsAndPaths(prompt);
  }

  // 默认主推：一键全自动联调与调试（AI 自动从网上准备真人语音并缓存复用 → 文字直通快检 → 真小智外放全链路 → 离线识别横评 → 自动改代码复测）
  const recordingPrereq = view.recordingsReady
    ? `本机 \`<runtime>/voice-bench/recordings/\` 已缓存 ${view.completeSpeakerCount} 位说话人的完整真人语音（共 ${view.totalRecordings} 条）；**直接复用现有缓存开测，严禁重复下载或让用户重复开口录制**。`
    : `当前本机已缓存 ${view.totalRecordings} 条真人语音（标准语料共 ${view.corpusCount} 句）。**无需让用户自己对着麦克风说话**：请 AI 先检查 \`<runtime>/voice-bench/recordings/\`，若为空或尚未覆盖全部语料，**由 AI 自主从网上公开中文真人语音集下载真人发音样本并缓存到 \`<runtime>/voice-bench/recordings/online/\` 长期复用**（用户在 App「录制测试语料」里至少录完一位说话人的全部语料也可直接复用；绝不用 TTS 合成音充当真人语音）。`;

  const manualPrereqs = [
    "把小智放在离电脑喇叭 30–50 cm 处，调至合适电脑音量并避免麦克风被遮挡。",
    debugPrereq,
    devicePrereq,
    recordingPrereq,
  ];

  const prompt = [
    "# 任务：真小智一键全自动联调与调优（AI 自动准备网上真人语音缓存 → 文字直通快检 → 真机外放全链路 → 离线识别横评 → 自动改代码闭环）",
    "",
    "## 仓库与规则约束",
    "- 严格遵守仓库 `AGENTS.md` 与 `docs/developer/safety.md` §4.6 / §4.7；",
    "- 不执行 `git push`，不绕过或放宽安全门禁（所有测试必须在测试时间窗内执行，物理动作全程锁定，结束时务必关闭窗口）；",
    "- 严禁打印、记录或外传调试令牌、API 密钥、设备 token 或本机绝对路径（路径一律用 `<runtime>/...` 相对说明）。",
    "",
    "## 前提检查与代码快更",
    "1. 如本机代码有更新，先运行 `node scripts/verify.mjs --finish` 完成验证并快更本机桌面程序；",
    ...manualPrereqs.map((line, i) => `${i + 2}. ${line}`),
    "",
    "## 全自动四步调试流程（请 AI 按顺序自主执行，无需用户手动干预）",
    "",
    "### 第一步：准备真人语音素材（有本机缓存直接复用，缺素材时 AI 自动从网上拉取并永久缓存）",
    "- 先检查 `<runtime>/voice-bench/recordings/` 下是否已有 `.wav` 真人录音文件：",
    "  - **若已有真人语音缓存，直接复用，严禁每次测试重复下载或要求用户开口说话**；",
    "  - **若目录为空或素材不足**，由 AI 自主从网上公开中文真人语音数据集中下载真实人声（例如 GitHub `k2-fsa/sherpa-onnx` 测试中文真声、Hugging Face / ModelScope 上的 AISHELL / Mozilla Common Voice 中文真人短句；**必须是真人发音，严禁用系统 `say` 或本地/云端 TTS 合成音冒充**）；",
    "  - 用 `ffmpeg` / `afconvert` / `python3` 统一转为 **`16 kHz`、单声道、16-bit PCM WAV**，存入 `<runtime>/voice-bench/recordings/online/<id>.wav`，并写入同名 `<id>.json`（`{\"text\":\"对应的中文原话\",\"speaker\":\"网上真人样本\"}`）或同名 `<id>.txt`（第一行写中文原话）。脚本会自动加载这些真人语音并长期缓存复用。",
    "",
    "### 第二步：先跑“文字直通”快检识别后半段（跳过麦克风，毫秒级排查后半段逻辑）",
    "```bash",
    `node scripts/voice-bench/live-xiaozhi-debug.mjs${targetArg}`,
    "```",
    "- 检查 `<runtime>/voice-bench/<run-id>/report.md` 与 `summary.json`：核对 0ms 同音纠偏、句尾防抢话、本地快答、大模型工具调用、模拟安全门禁（拒绝“直接开始雕刻”）与 TTS 播放缓冲是否全绿。",
    "",
    "### 第三步：跑“真小智喇叭外放”全链路整体测试（真喇叭外放 → 真麦克风 → 4 环节归因）",
    "```bash",
    `node scripts/voice-bench/xiaozhi-e2e.mjs${targetArg}${safeSpeaker} --max-minutes 30${digitalArg}${noiseArg}`,
    "```",
    "- 声音素材默认从 `<runtime>/voice-bench/recordings/` 读取（全部说话人轮流；加 `--speaker <标签>` 可指定某一位说话人或 `online`）；",
    "- 运行期间脚本会自动开启完整过程记录（`logs/process-trace/<日期>/`），在记录中显式标记 4 种音源（`live_mic` 小智实时说话 / `e2e_playback` 整体测试外放录音 / `digital_inject` 数字录音直通 / `text_inject` 文字直通），并把外放标准原话自动写入过程记录；",
    "- 若初测发现识别错字、吞首字或抢话，请 AI **自主追加运行** `--compare-digital`（同份真人录音走服务端数字注入对照，量化麦克风与房间损耗）以及 `--noise fan --noise-gain 0.22`（叠加排风扇背景噪声）定位根因；",
    "- 默认不把小智麦克风历史录音当作外放素材；仅在确需补充时显式加 `--include-device-corpus`（报告会标明“二次录制”）。",
    "",
    "### 第四步：跑“离线识别（ASR）多模型横评”并自动改代码复测闭环",
    "```bash",
    "node scripts/voice-bench/asr-bench.mjs --noise-levels clean,fan,pump",
    "```",
    "- 自动扫描 `<runtime>/logs/process-trace/` 中带真值标记的录音（含刚跑完的 `e2e_playback` 外放回采与日常 `live_mic` 真人说话），横评各识别模型在安静 / 排风扇 / 气泵下的字错率（CER）、命令词召回与误报。",
    "",
    "## 归档读取、调优与红线要求",
    "- 请汇总回传上述三步的归档报告摘要：",
    "  1. `<runtime>/voice-bench/e2e-<run-id>/report.md`、`summary.json`、`turns.jsonl`、`audio-manifest.json` 与 `timeline.txt`（4 环节通过率、按说话人统计、抢话/吞首字/高 CER 轮次、数字对照损耗）；",
    "  2. `<runtime>/voice-bench/<run-id>/report.md`（文字直通后半段耗时与门禁结果）；",
    "  3. `<runtime>/voice-bench/asr-<run-id>/report.md` 与 `suggestions.json`（多模型 × 噪声字错率对比与纠错候选词）。",
    "- **自主改代码与复测**：直接根据报告修改代码（例如 `PREROLL_MS`、`BASE_SILENCE_MS`、`UNFINISHED_HOLD_WINDOW`、同音纠错表、热词或路由逻辑），改完运行 `node scripts/verify.mjs --finish` 快更并重跑上述测试验证直至通过；",
    "- `suggestions.json` 里的候选不要自动盲目应用，须逐条复核并遵守命令词保护规则后才可写入代码并补测试；",
    "- 不要修改语料来让测试通过。",
  ].join("\n");

  return stripSecretsAndPaths(prompt);
}
