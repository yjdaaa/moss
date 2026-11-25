// 本地模型：下载状态、本地设置表单与载荷、视觉看图来源与 Colab T4 联动。

import { canonicalPersonaStyle, experienceSettingsError, parseAsrCorrectionLine, parseCustomPersonas } from "./experience.js";

/** 字节数显示为 MB（保留 1 位小数，小于 1 MB 显示 KB）。 */
export function formatBytes(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024 * 1024) return `${Math.max(1, Math.round(value / 1024))} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

/** 本地模型一行的状态与可用操作（NV6）。 */
export function localModelView(model) {
  const progress = model?.progress;
  if (progress?.state === "downloading") {
    const percent = progress.total > 0 ? Math.min(100, Math.floor((progress.done / progress.total) * 100)) : 0;
    return { text: `下载中 ${percent}%`, tone: "info", percent, canDownload: false, canCancel: true, canDelete: false };
  }
  // 2026-11-07：OneJev 先准备三档共用的推理引擎；多档同时下载时后来的排队等待
  if (progress?.state === "preparing_runtime") {
    const percent = progress.total > 0 ? Math.min(100, Math.floor((progress.done / progress.total) * 100)) : 0;
    return { text: `准备推理引擎 ${percent}%`, tone: "info", percent, canDownload: false, canCancel: true, canDelete: false };
  }
  if (progress?.state === "waiting_runtime") return { text: "等待推理引擎", tone: "info", percent: 0, canDownload: false, canCancel: true, canDelete: false };
  if (progress?.state === "verifying") return { text: "校验中", tone: "info", percent: 100, canDownload: false, canCancel: true, canDelete: false };
  if (progress?.state === "installing") return { text: "安装中", tone: "info", percent: 100, canDownload: false, canCancel: true, canDelete: false };
  if (model?.installed) return { text: "已安装", tone: "ok", percent: null, canDownload: false, canCancel: false, canDelete: true };
  if (progress?.state === "failed") {
    return { text: "下载失败", detail: progress.message || "未知原因", tone: "bad", percent: null, canDownload: true, canCancel: false, canDelete: false };
  }
  if (progress?.state === "cancelled") return { text: "已取消", tone: "off", percent: null, canDownload: true, canCancel: false, canDelete: false };
  return { text: "未下载", tone: "off", percent: null, canDownload: true, canCancel: false, canDelete: false };
}

function formatElapsed(secs) {
  const total = Math.max(0, Math.floor(Number(secs) || 0));
  const m = Math.floor(total / 60);
  const sec = String(total % 60).padStart(2, "0");
  return `${m}:${sec}`;
}

/**
 * Laya 运行环境自动准备的显示（2026-11-07）：`setup` 为 `local.laya_setup`，`env` 为 `local.laya_env`。
 * 返回 null 表示无需显示；`canStart` 时显示“准备运行环境 / 重新准备环境”按钮。
 */
export function layaSetupView(setup, env) {
  const state = setup?.state || "idle";
  const ready = Boolean(env?.ready);
  const retryLabel = ready ? "重新准备环境" : "准备运行环境";
  if (state === "running") {
    const step = setup.steps ? `第 ${setup.step || 0}/${setup.steps} 步 · ` : "";
    return {
      tone: "info",
      text: `正在准备 Laya 运行环境：${step}${setup.stage || ""}（已用时 ${formatElapsed(setup.elapsed_secs)}）`,
      detail: setup.detail || "",
      canStart: false,
      canCancel: true,
      startLabel: retryLabel,
    };
  }
  if (state === "failed") {
    return { tone: "bad", text: `Laya 运行环境准备失败：${setup.message || "未知原因"}`, detail: (setup.log || []).slice(-3).join("\n"), canStart: true, canCancel: false, startLabel: "重试准备环境" };
  }
  if (state === "cancelled") return { tone: "off", text: "已取消准备 Laya 运行环境", detail: "", canStart: true, canCancel: false, startLabel: retryLabel };
  if (state === "done") return { tone: "ok", text: setup.summary || "Laya 运行环境已准备好", detail: "", canStart: ready && env?.accelerator === "cpu", canCancel: false, startLabel: retryLabel };
  if (!ready) return { tone: "warn", text: env?.summary || "尚未准备 Laya 运行环境", detail: "", canStart: true, canCancel: false, startLabel: retryLabel };
  // 已就绪但只能用 CPU（如装好显卡驱动之前准备的）：允许重新准备以换成 CUDA
  return env?.accelerator === "cpu" && env?.managed ? { tone: "ok", text: env.summary, detail: "", canStart: true, canCancel: false, startLabel: retryLabel } : null;
}

// 2026-11-07：本地 Jev（OneJev / Laya）实际运行情况。后台启动失败、推理进程意外退出时说明原因，
// 不再出现“看着开着、实际每轮都被跳过”；云端模型与未下载时不显示。
export function localJevRuntimeView(runtime) {
  if (!runtime || runtime.state === "cloud" || runtime.state === "not_installed") return null;
  const accel = runtime.accelerator ? `（${runtime.accelerator}）` : "";
  const notice = runtime.notice || "";
  switch (runtime.state) {
    case "ready":
      return { tone: notice ? "warn" : "ok", text: `本地 Jev 已就绪${accel}`, detail: notice };
    case "starting":
      return { tone: "info", text: "本地 Jev 正在加载模型，加载完成前语音筛选会先跳过", detail: notice };
    case "failed":
      return { tone: "bad", text: `本地 Jev 启动失败：${runtime.error || "未知原因"}`, detail: notice };
    case "stopped":
      return { tone: "warn", text: "本地 Jev 未在运行：重新保存设置或重启语音服务即可拉起", detail: notice };
    case "idle":
      return notice ? { tone: "off", text: "本地 Jev 未启用（语音筛选与加工反馈都关闭时不占内存）", detail: notice } : null;
    default:
      return null;
  }
}

const LOCAL_BUSY_STATES = new Set(["downloading", "preparing_runtime", "waiting_runtime", "verifying", "installing"]);

/** 删除本地模型前的确认文字：说明会一并删除的附带文件（2026-11-07）。 */
export function localModelDeleteText(model, models, local) {
  const label = model?.label || model?.id || "";
  let extra = "";
  if (model?.id === "laya_multilingual" && local?.laya_env?.managed) {
    extra = "会同时删除 MOSS 自动创建的 Laya Python 运行环境（含 PyTorch，可能有几 GB），下次使用时重新准备。";
  } else if (model?.id === "qwen3_tts_0_6b") {
    extra = "会同时删除 MOSS 自动下载的语音推理引擎 speech。";
  } else if (model?.kind === "jev" && model?.id !== "laya_multilingual") {
    const otherOneJev = (Array.isArray(models) ? models : []).some(
      (item) => item.kind === "jev" && item.id !== "laya_multilingual" && item.id !== model.id && (item.installed || LOCAL_BUSY_STATES.has(item.progress?.state)),
    );
    if (!otherOneJev) extra = "这是最后一档 OneJev，会同时删除 MOSS 下载的推理引擎 llama-server。";
  } else if (model?.kind === "font") {
    extra = "删除后已装日常字库的小智遇到生僻字、繁体字或日韩泰阿文字将不再动态补字（基础界面仍由内置 6,845 字常用字库兜底）。";
  }
  return `删除本地模型“${label}”？${extra}需要时可以重新下载。`;
}

/** 本地模型的七个类别与默认模型（判停 / 识别 / 合成由顶部下拉框控制；降噪、命令词抗噪与语义判停在本地模型表按需开启）。 */
export const LOCAL_KINDS = [
  { kind: "vad", title: "拾音判停", fallback: "silero_vad" },
  { kind: "denoise", title: "前端降噪", fallback: "gtcrn_simple" },
  { kind: "kws", title: "命令词抗噪", fallback: "kws_zipformer_zh" },
  { kind: "turn", title: "语义判停", fallback: "smart_turn_v3" },
  { kind: "asr", title: "语音识别", fallback: "sense_voice" },
  { kind: "tts", title: "语音合成", fallback: "kokoro" },
  { kind: "vision", title: "视觉理解", fallback: "qwen3_vl_2b" },
];

/** 按类别分组的本地模型（保持后端清单顺序）。 */
export function localModelGroups(models) {
  const list = Array.isArray(models) ? models : [];
  return LOCAL_KINDS.map((item) => ({ ...item, models: list.filter((model) => model.kind === item.kind) }));
}

/**
 * 本地模型页的版面（2026-11-23 n109 用户决定“合并 + 折叠”）：
 * 命令词抗噪、前端降噪、语义判停都是给本地判停用的增强，不再各自成一张“模型包”表，并入“拾音判停”；
 * 整链实测（n99 / n101）没有提升的模型——老版 Silero、TEN-VAD、两个降噪、语义判停——收进默认收起的“更多 / 实验”。
 * 只改展示，清单、设置与已下载文件都不动。
 */
export const VAD_EXTRA_MODELS = ["silero_vad", "ten_vad"];
const VAD_ADDON_KINDS = ["kws"];
const VAD_EXPERIMENT_KINDS = ["denoise", "turn"];
const VAD_FAMILY = new Set(["vad", ...VAD_ADDON_KINDS, ...VAD_EXPERIMENT_KINDS]);

/** 这个模型是否放在“更多 / 实验”里（顶部下拉选了它但还没下载时，要先展开再定位）。 */
export function isFoldedLocalModel(id) {
  return VAD_EXTRA_MODELS.includes(id);
}

/**
 * 把 `localModelGroups` 的结果排成页面版面。`moreOpen`：已开着降噪 / 语义判停、正在用收起的判停模型、
 * 或收起区里有模型在下载时默认展开，免得开着的东西被藏起来。
 */
export function localModelLayout(groups, settings) {
  const list = Array.isArray(groups) ? groups : [];
  const value = settings || {};
  const byKind = Object.fromEntries(list.map((group) => [group.kind, group]));
  const vad = byKind.vad || { kind: "vad", title: "拾音判停", models: [] };
  const vadMain = { ...vad, models: vad.models.filter((model) => !isFoldedLocalModel(model.id)) };
  const vadMore = { ...vad, models: vad.models.filter((model) => isFoldedLocalModel(model.id)) };
  const addons = VAD_ADDON_KINDS.map((kind) => byKind[kind]).filter(Boolean);
  const experiments = VAD_EXPERIMENT_KINDS.map((kind) => byKind[kind]).filter(Boolean);
  const folded = [...vadMore.models, ...experiments.flatMap((group) => group.models)];
  const moreOpen = Boolean(
    value.denoise ||
      value.turn ||
      (value.vad && isFoldedLocalModel(value.vad_model)) ||
      folded.some((model) => localModelView(model).canCancel),
  );
  return { vadMain, vadMore, addons, experiments, others: list.filter((group) => !VAD_FAMILY.has(group.kind)), moreOpen };
}

/** 本地模型设置表单（数字以字符串保存，便于输入）。 */
export function localSettingsForm(settings) {
  const value = settings || {};
  return {
    asr: Boolean(value.asr),
    barge_in: Boolean(value.barge_in),
    barge_in_ms: String(value.barge_in_ms ?? 400),
    read_only_shortcuts: value.read_only_shortcuts !== false,
    asr_beam_search: value.asr_beam_search !== false,
    job_done_announce: value.job_done_announce !== false,
    ignore_bystanders: value.ignore_bystanders !== false,
    llm_fast_reply: value.llm_fast_reply !== false,
    jev_filter_enabled: Boolean(value.jev_filter_enabled),
    jev_model: value.jev_model || "typesafe/jev-1.14",
    jev_unfinished_hold: value.jev_unfinished_hold !== false,
    jev_fast_route: value.jev_fast_route !== false,
    jev_tool_prune: value.jev_tool_prune !== false,
    jev_memory_filter: value.jev_memory_filter !== false,
    custom_hotwords: (Array.isArray(value.custom_hotwords) ? value.custom_hotwords : []).join("\n"),
    asr_homophone_fix: value.asr_homophone_fix !== false,
    custom_asr_corrections: (Array.isArray(value.custom_asr_corrections) ? value.custom_asr_corrections : []).join("\n"),
    persona_style: canonicalPersonaStyle(value.persona_style || "workshop"),
    persona_risk_notes: value.persona_risk_notes === true,
    persona_prompt: value.persona_prompt || "",
    custom_personas: JSON.stringify(parseCustomPersonas(value.custom_personas)),
    screen_preview_sync: value.screen_preview_sync !== false,
    screen_card_style: value.screen_card_style === "goggle_bot" ? "goggle_bot" : "moss_hud",
    tts: Boolean(value.tts),
    vad: Boolean(value.vad),
    denoise: Boolean(value.denoise),
    kws: Boolean(value.kws),
    kws_model: value.kws_model || "kws_zipformer_zh",
    turn: Boolean(value.turn),
    vad_model: value.vad_model || "silero_vad",
    denoise_model: value.denoise_model || "gtcrn_simple",
    turn_model: value.turn_model || "smart_turn_v3",
    asr_model: value.asr_model || "sense_voice",
    tts_model: value.tts_model || "kokoro",
    tts_speaker: String(value.tts_speaker ?? 50),
    tts_speed: String(value.tts_speed ?? 1),
    mirror: value.mirror || "",
  };
}

/** 改选某类模型；改选多音色合成模型或原音色落在非中文区间 / 超出上限时，音色编号自动换成该模型的默认中文音色。 */
export function selectLocalModel(form, models, kind, id) {
  const next = { ...form, [`${kind}_model`]: id };
  const model = (Array.isArray(models) ? models : []).find((item) => item.id === id);
  if (kind === "tts") {
    const spk = Number(form?.tts_speaker);
    const outOfChineseRange =
      (id === "kokoro" && !(Number.isInteger(spk) && spk >= 45 && spk <= 52)) ||
      (id === "kokoro_v11" && !(Number.isInteger(spk) && spk >= 3 && spk <= 102)) ||
      (Boolean(model?.voices) && (!Number.isInteger(spk) || spk < 0 || spk > model.voices.max));
    if ((id !== form.tts_model || outOfChineseRange) && model?.voices) {
      next.tts_speaker = String(model.voices.default);
    } else if (outOfChineseRange) {
      next.tts_speaker = id === "kokoro" ? "50" : id === "kokoro_v11" ? "3" : "0";
    }
  }
  return next;
}

/** 当前选中的合成模型的音色范围（单音色模型或未知模型为空）。 */
export function selectedVoices(form, models) {
  const model = (Array.isArray(models) ? models : []).find((item) => item.id === form.tts_model);
  return model?.voices || null;
}

/** 所选本地合成模型是否不支持调语速（Qwen3-TTS 跑在 speech.cpp 上，固定 1.0x；2026-11-25）。 */
export function localTtsSpeedFixed(model) {
  return model?.engine === "speech_cpp_tts" || model?.id === "qwen3_tts_0_6b";
}

/**
 * 音色表按语种分组成下拉选项（`编号 · 名称（性别）`，2026-11-25）。
 * 当前值不在音色表里时单独列一项，避免下拉悄悄把它换成别的音色。
 */
export function localTtsSpeakerGroups(speakers, current) {
  const groups = [];
  const byLabel = new Map();
  for (const sp of Array.isArray(speakers) ? speakers : []) {
    const label = sp.lang_label || "其他";
    let group = byLabel.get(label);
    if (!group) {
      group = { label, options: [] };
      byLabel.set(label, group);
      groups.push(group);
    }
    group.options.push({ value: String(sp.id), label: `${sp.id} · ${sp.name}${sp.gender_label ? `（${sp.gender_label}）` : ""}` });
  }
  const value = String(current ?? "").trim();
  if (value && !groups.some((group) => group.options.some((option) => option.value === value))) {
    groups.push({ label: "当前值", options: [{ value, label: `${value}（不在该模型的音色表里）` }] });
  }
  return groups;
}

/** 云端 TTS 启用时（`!form.tts`），返回当前作为云端异常优先兜底的已安装本地 TTS 模型；未安装任何本地 TTS 或已启用本地主 TTS 时返回 `null`。 */
export function resolveFallbackTtsModel(form, models = []) {
  if (!form || Boolean(form.tts)) return null;
  const ttsModels = (Array.isArray(models) ? models : []).filter(
    (item) => (!item.kind || item.kind === "tts") && item.installed
  );
  if (ttsModels.length === 0) return null;
  return ttsModels.find((item) => item.id === form.tts_model) || ttsModels[0] || null;
}

/** 本地模型分组（音色、语速、下载地址）。`models` 为状态里的模型列表，用于按所选合成模型检查音色编号。 */
export function localModelSettingsError(form, models = []) {
  const voices = selectedVoices(form, models);
  const max = voices ? voices.max : 999;
  const speaker = Number(form.tts_speaker);
  if (!Number.isInteger(speaker) || speaker < 0 || speaker > max) return `音色编号须为 0 ~ ${max} 的整数`;
  const speed = Number(form.tts_speed);
  if (!(speed >= 0.5 && speed <= 2)) return "本地合成语速须在 0.5 ~ 2.0 之间";
  const mirror = String(form.mirror || "").trim();
  if (mirror && (!/^https?:\/\//i.test(mirror) || /\s/.test(mirror))) return "下载地址必须以 http:// 或 https:// 开头";
  return "";
}

/** 两组合起来的检查。 */
export function localSettingsError(form, models = []) {
  return experienceSettingsError(form) || localModelSettingsError(form, models);
}

export function localSettingsPayload(form) {
  return {
    asr: Boolean(form.asr),
    barge_in: Boolean(form.barge_in),
    barge_in_ms: Number(form.barge_in_ms ?? 400),
    read_only_shortcuts: form.read_only_shortcuts !== false,
    asr_beam_search: form.asr_beam_search !== false,
    job_done_announce: form.job_done_announce !== false,
    ignore_bystanders: form.ignore_bystanders !== false,
    llm_fast_reply: form.llm_fast_reply !== false,
    jev_filter_enabled: Boolean(form.jev_filter_enabled),
    jev_model: String(form.jev_model || "typesafe/jev-1.14").trim() || "typesafe/jev-1.14",
    jev_unfinished_hold: form.jev_unfinished_hold !== false,
    jev_fast_route: form.jev_fast_route !== false,
    jev_tool_prune: form.jev_tool_prune !== false,
    jev_memory_filter: form.jev_memory_filter !== false,
    custom_hotwords: [...new Set(String(form.custom_hotwords || "").split(/\r?\n/).map((word) => word.trim()).filter(Boolean))],
    asr_homophone_fix: form.asr_homophone_fix !== false,
    custom_asr_corrections: (() => {
      const map = new Map();
      for (const line of String(form.custom_asr_corrections || "").split(/\r?\n/)) {
        const parsed = parseAsrCorrectionLine(line);
        if (!parsed.empty && !parsed.error) map.set(parsed.wrong, parsed.formatted);
      }
      return [...map.values()];
    })(),
    persona_style: canonicalPersonaStyle(String(form.persona_style || "workshop").trim() || "workshop"),
    persona_risk_notes: form.persona_risk_notes === true,
    persona_prompt: String(form.persona_prompt || "").replace(/\r\n?/g, "\n").trim(),
    custom_personas: parseCustomPersonas(form.custom_personas).map((role) => ({
      id: role.id,
      name: role.name.trim(),
      prompt: role.prompt.replace(/\r\n?/g, "\n").trim(),
    })),
    screen_preview_sync: form.screen_preview_sync !== false,
    screen_card_style: form.screen_card_style === "goggle_bot" ? "goggle_bot" : "moss_hud",
    tts: Boolean(form.tts),
    vad: Boolean(form.vad),
    denoise: Boolean(form.denoise),
    kws: Boolean(form.kws),
    kws_model: form.kws_model || "kws_zipformer_zh",
    turn: Boolean(form.turn),
    vad_model: form.vad_model,
    denoise_model: form.denoise_model || "gtcrn_simple",
    turn_model: form.turn_model || "smart_turn_v3",
    asr_model: form.asr_model,
    tts_model: form.tts_model,
    tts_speaker: Number(form.tts_speaker),
    tts_speed: Number(form.tts_speed),
    mirror: String(form.mirror || "").trim(),
  };
}

/**
 * 视觉理解的看图来源（n74，2026-11-08）：`local` 本地视觉模型、`dialog` 当前对话大模型、`cloud` 云端视觉模型。
 * 存储上仍是 `use_dialog_llm` 与 `local_model` 两个字段：对话大模型优先，其次本地，否则云端。
 */
export function visionSource(config) {
  if (config?.use_dialog_llm) return "dialog";
  if (config?.local_model) return "local";
  return "cloud";
}

/** 切换看图来源时要改的字段；切到本地时优先选已下载的模型。 */
export function visionSourcePatch(source, config, localModels = []) {
  if (source === "dialog") return { use_dialog_llm: true, local_model: "" };
  if (source === "local") {
    const list = Array.isArray(localModels) ? localModels : [];
    const keep = list.find((item) => item.id === config?.local_model);
    const pick = keep || list.find((item) => item.installed) || list[0];
    return { use_dialog_llm: false, local_model: pick?.id || "" };
  }
  return { use_dialog_llm: false, local_model: "" };
}

/** 本地视觉推理进程的状态说明。 */
export function visionRuntimeText(runtime) {
  switch (runtime?.state) {
    case "ready":
      return `本地视觉模型已就绪${runtime.accelerator ? `（${runtime.accelerator}）` : ""}，照片只在这台电脑上处理。`;
    case "starting":
      return "正在加载本地视觉模型（首次把权重读进内存约十几秒到一分钟）…";
    case "failed":
      return `本地视觉模型启动失败：${runtime.error || "未知原因"}`;
    default:
      return "内置服务启动后会在后台加载本地视觉模型；点“测试模型”会等它加载完再测。";
  }
}

/**
 * n80 Colab T4 专属联动表单（默认关闭；开启后只临时接管大模型、摄像头看图与 550W 克隆语音合成，关闭即恢复原配置）。
 */
export function colabSettingsForm(colab) {
  const c = colab || {};
  return {
    enabled: Boolean(c.enabled),
    url: String(c.url || ""),
    llm_model: String(c.llm_model || "qwen3-vl-8b"),
    tts_model: String(c.tts_model || "qwen3-tts"),
    tts_voice: String(c.tts_voice || "moss"),
    tts_speed: String(c.tts_speed ?? 1),
    api_key: "",
  };
}

/**
 * Colab T4 专属联动折叠栏是否默认展开：开启、接管中、正在拉起 / 运行或有拉起报错时自动展开，未开启时默认收起。
 */
export function colabBridgeDefaultOpen(colab) {
  const c = colab || {};
  const cli = c.cli || {};
  const runnerState = cli.runner_state || "idle";
  return Boolean(
    c.enabled ||
      c.active ||
      runnerState === "starting" ||
      runnerState === "running" ||
      cli.runner_error,
  );
}

/**
 * 支持把 Colab 单元格 3 打印的整段配置块直接粘贴进地址框，自动拆出隧道地址与 API 密钥。
 */
export function parseColabPaste(raw) {
  const text = String(raw || "");
  const urlMatch = text.match(/https?:\/\/[a-zA-Z0-9._:-]+(?:\/[^\s"']*)?/);
  const keyMatch = text.match(/(?:API\s*密钥|API_KEY|密钥)\s*[:：=]\s*([^\s"']+)/i) || text.match(/\b(moss-[A-Za-z0-9_-]{12,})\b/);
  if (urlMatch && keyMatch && text.includes("\n")) {
    return {
      url: urlMatch[0].trim(),
      api_key: keyMatch[1].trim(),
    };
  }
  return { url: text, api_key: "" };
}

/** Colab T4 联动表单前端校验：关闭时允许留空；已登录 Colab CLI 时允许留空隧道地址与密钥（开启后由 CLI 自动拉起并回填）。 */
export function colabSettingsError(form, keyConfigured = false, cliLoggedIn = false) {
  const f = form || {};
  const enabled = Boolean(f.enabled);
  const url = String(f.url || "").trim();
  const hasKey = Boolean(String(f.api_key || "").trim()) || Boolean(keyConfigured);
  if (url) {
    if (/\s|<|>/.test(url)) return "Colab 隧道地址不能包含空格或尖括号占位符";
    if (!/^https:\/\/|^http:\/\/(127\.0\.0\.1|localhost)(:\d+)?(\/|$)/i.test(url)) {
      return "Colab 隧道地址须以 https:// 开头（如 https://xxx.trycloudflare.com/v1）";
    }
  } else if (enabled && !cliLoggedIn) {
    return "尚未登录 Colab CLI：请先点击「登录 Colab CLI」授权（或手动填写隧道地址与 API 密钥）";
  }
  if (enabled && !hasKey && !cliLoggedIn) {
    return "开启 Colab 联动前请先登录 Colab CLI，或手动填写 API 密钥";
  }
  for (const [label, val] of [
    ["大模型名", f.llm_model],
    ["合成模型名", f.tts_model],
    ["合成音色", f.tts_voice],
  ]) {
    const s = String(val || "").trim();
    if (s.length > 120 || /\s/.test(s)) return `Colab ${label}不能包含空格且不超过 120 个字符`;
  }
  const speed = Number(f.tts_speed ?? 1);
  if (!Number.isFinite(speed) || speed < 0.25 || speed > 4.0) {
    return "Colab 合成语速须在 0.25 ~ 4.0 之间";
  }
  return "";
}

/** 构造提交给 `voice_local_save({ colab })` 的载荷。 */
export function colabSavePayload(form) {
  const f = form || {};
  const payload = {
    enabled: Boolean(f.enabled),
    url: String(f.url || "").trim(),
    llm_model: String(f.llm_model || "qwen3-vl-8b").trim() || "qwen3-vl-8b",
    tts_model: String(f.tts_model || "qwen3-tts").trim() || "qwen3-tts",
    tts_voice: String(f.tts_voice || "moss").trim() || "moss",
    tts_speed: Number(f.tts_speed ?? 1),
  };
  const key = String(f.api_key || "").trim();
  if (key) payload.api_key = key;
  return payload;
}
