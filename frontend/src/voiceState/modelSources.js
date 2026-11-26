// 内置服务模型来源：混搭环节、推荐本地模型、模型搭配与来源切换。

import { localModelView, localSettingsForm, localSettingsPayload, selectLocalModel } from "./localModels.js";
import { DEFAULT_JEV_MODEL, JEV_PRESET_MODELS, LOCAL_JEV_FALLBACK_MODELS, canonicalLocalJevModel, isSpecificLocalJevInstalled, jevModelError } from "./jev.js";

/**
 * 顶部自由混搭栏按真实语音处理链路排序（从收音到播报）：
 * 拾音判停 (vad) → 语音识别 (asr) → Jev 前置粗筛 (jev，识别后·进大模型前) → 大模型 (llm) → 语音合成 (tts)
 */
export const MIX_SECTIONS = [
  { key: "vad", label: "1. 拾音判停", hint: "检测说话开始与结束（本地模型可抗风扇/机器噪音）" },
  { key: "asr", label: "2. 语音识别", hint: "将语音转为文字（可混搭云端或本地模型）" },
  { key: "jev", label: "3. Jev 前置粗筛", hint: "识别后·大模型前：半截话续听、快答分流、工具与记忆粗筛" },
  { key: "llm", label: "4. 大模型", hint: "接收粗筛后的上下文与工具，生成对话回复与调用" },
  { key: "tts", label: "5. 语音合成", hint: "将回复合成为语音播报（可混搭云端或本地模型）" },
];

/**
 * 内置服务单项（llm / asr / tts / vad / jev）的顶部下拉切换视图：
 * - `asr` / `tts` 可在“云端模型”与“本地模型”之间独立切换；
 * - `vad` 可在“内置能量判停（免下载）”与“本地判停模型（Silero VAD / TEN-VAD）”之间独立切换；
 * - `jev` 可在“关闭（仅本地规则）”、“开启 · typesafe/jev-1.14 / 1.13”与“本地 · OneJev 1.6 (0.8B/4B/9B) / Laya”之间直接切换；
 * - 切换时已保存的密钥与备用配置原样保留。
 */
/**
 * 每类本地模型的推荐项（本机实测，技术书 n95 / n99）：判停 Silero v6.2（敲击声与持续机器噪声都不误触发，
 * 机器噪声下识别最准）、识别 Qwen3-ASR（整链实测最准；n111 前推荐 SenseVoice 2024）、合成 Qwen3-TTS 1.7B（声音最自然、
 * 可调语速，Apple 芯片 Mac 合成约 3 倍实时；n128 前推荐 Kokoro v1.0）。
 * 只用于在界面上标“推荐”；与后端新安装默认组合 `BuiltinLocal::new_install` 一致（n111），老用户的设置不受影响。
 */
export const RECOMMENDED_LOCAL_MODELS = { vad: "silero_vad_v6", asr: "qwen3_asr_0_6b", tts: "qwen3_tts_1_7b" };

/** “当前语音链路”下拉框下方的一句推荐（短名 + 为什么），依据 n95 / n99 本机实测。 */
export const RECOMMENDED_LOCAL_NOTES = {
  vad: { name: "Silero v6.2", why: "噪声下不误触发、听得最准" },
  asr: { name: "Qwen3-ASR", why: "整链实测最准，机器噪声下错字少一半" },
  tts: { name: "Qwen3-TTS 1.7B", why: "声音最自然，可调语速" },
};

/**
 * “当前语音链路”每一栏下方的推荐（五栏都有，保证上下对齐）：
 * - 判停 / 识别 / 合成：推荐的本地模型，没选时可点切换；
 * - 大模型：云端（本机没有可推荐的本地大模型）；
 * - Jev：可选功能，只给中性提示，不提供切换（未实测它对速度的影响）。
 */
export function recommendationState(section, selectValue) {
  if (section === "llm") {
    return {
      value: "cloud",
      chosen: selectValue === "cloud" || selectValue === "colab",
      name: "云端大模型",
      why: "首字快、工具调用稳",
    };
  }
  if (section === "jev") {
    return { value: null, chosen: false, neutral: true, name: "可选", why: "默认关闭即可" };
  }
  const id = RECOMMENDED_LOCAL_MODELS[section];
  if (!id) return null;
  return { value: `local:${id}`, chosen: selectValue === `local:${id}`, ...RECOMMENDED_LOCAL_NOTES[section] };
}

export function isRecommendedLocalModel(kind, id) {
  return Boolean(id) && RECOMMENDED_LOCAL_MODELS[kind] === id;
}

/**
 * 按使用场景推荐的整套本地模型（2026-11-15 整条链路连起来实测，技术书 n99）。大模型一律用云端。
 * `vad: null` 表示内置能量判停（不用本地判停模型）；`kws` / `denoise` / `turn` 为 null 或不写表示不开
 * （实测前端降噪没有提升识别、语义判停没有可见差别，所以推荐的组合都不开；命令词抗噪经小智上行 Opus 后
 * 认不出的正好也是通用识别听错的那几句，只多救回一两条、普通说话 0 次误执行，n101 起只放进“车间噪声大”）。
 * 2026-11-24 n111：第一套（`noisy`）即新安装默认组合（`BuiltinLocal::new_install`），标题改为“推荐”；
 * 原“推荐”（`balanced`，SenseVoice）改为“要回得更快”。
 * 2026-11-25 n128：第一套合成改为 Qwen3-TTS 1.7B（与新安装默认一致）；“说完约 2.4 秒”是 Kokoro 时的整链实测，
 * 换模型后未重测，不再写秒数。其余几套仍用 Kokoro / Matcha（体积小、更快）。
 */
export const MODEL_COMBOS = [
  {
    id: "noisy",
    title: "推荐（新安装默认）",
    fit: "大多数情况，车间里风扇、气泵一直响也听得准",
    result: "整链实测错字最少（机器噪声下少一半），声音最自然、可调语速；合成模型约 2.3 GB",
    vad: "silero_vad_v6",
    kws: "kws_zipformer_zh",
    denoise: null,
    turn: null,
    asr: "qwen3_asr_0_6b",
    tts: "qwen3_tts_1_7b",
  },
  {
    id: "balanced",
    title: "要回得更快",
    fit: "环境较安静，或电脑较慢",
    result: "说完约 1.4 秒听到回答，持续机器噪声下错字多一些",
    vad: "silero_vad_v6",
    denoise: null,
    turn: null,
    asr: "sense_voice",
    tts: "kokoro",
  },
  {
    id: "fast",
    title: "要回得最快",
    fit: "电脑较慢，或想更快听到回答",
    result: "快约 0.5 秒，音色偏平",
    vad: "silero_vad_v6",
    denoise: null,
    turn: null,
    asr: "sense_voice",
    tts: "matcha_zh_en",
  },
  {
    id: "voice",
    title: "音色更好",
    fit: "更在意声音好不好听",
    result: "中文音色更多更自然，回答慢约 0.9 秒",
    vad: "silero_vad_v6",
    denoise: null,
    turn: null,
    asr: "sense_voice",
    tts: "kokoro_v11",
  },
  {
    id: "quiet",
    title: "房间很安静",
    fit: "没有机器噪声",
    result: "快约 0.2 秒；有敲击声、机器声会误触发",
    vad: null,
    denoise: null,
    turn: null,
    asr: "sense_voice",
    tts: "kokoro",
  },
];

const COMBO_SHORT_NAMES = {
  silero_vad_v6: "Silero v6.2",
  silero_vad: "Silero",
  ten_vad: "TEN-VAD",
  dpdfnet_baseline: "DPDFNet 降噪",
  gtcrn_simple: "GTCRN 降噪",
  smart_turn_v3: "Smart Turn",
  sense_voice: "SenseVoice",
  qwen3_asr_0_6b: "Qwen3-ASR",
  firered_asr2_ctc: "FireRedASR2",
  kokoro: "Kokoro v1.0",
  kokoro_v11: "Kokoro v1.1",
  matcha_zh_en: "Matcha",
  qwen3_tts_0_6b: "Qwen3-TTS 0.6B",
  qwen3_tts_1_7b: "Qwen3-TTS 1.7B",
  qwen3_tts_1_7b_clone: "Qwen3-TTS 1.7B 声音克隆",
  kws_zipformer_zh: "命令词抗噪",
};

/** 一套组合的简写：“Silero v6.2 + SenseVoice + Kokoro v1.0”（不用本地判停模型时写“能量判停”）。 */
export function comboSummary(combo) {
  const name = (id) => COMBO_SHORT_NAMES[id] || id;
  return [combo.vad ? name(combo.vad) : "能量判停", combo.kws, combo.denoise, combo.turn, combo.asr, combo.tts]
    .filter(Boolean)
    .map((id, index) => (index === 0 ? id : name(id)))
    .join(" + ");
}

/** 一套组合要用到的本地模型（按判停、命令词、降噪、语义判停、识别、合成的顺序）。 */
export function comboModelIds(combo) {
  return [combo?.vad, combo?.kws, combo?.denoise, combo?.turn, combo?.asr, combo?.tts].filter(Boolean);
}

/** 这套组合里还没下载的模型（`{id, label, downloading}`；清单里没有的也算缺）。 */
export function comboMissingModels(combo, models) {
  const list = Array.isArray(models) ? models : [];
  return comboModelIds(combo)
    .map((id) => list.find((model) => model.id === id) || { id, label: id, installed: false })
    .filter((model) => !model.installed)
    .map((model) => ({
      id: model.id,
      label: model.label || model.id,
      downloading: localModelView(model).canCancel,
    }));
}

/** 这套组合里还没下载的模型名称。 */
export function comboMissing(combo, models) {
  return comboMissingModels(combo, models).map((model) => model.label);
}

/** 一次保存整套组合的本地模型设置（合成音色由后端随模型换成该模型的默认音色）。 */
export function comboPatch(combo) {
  return {
    vad: Boolean(combo.vad),
    ...(combo.vad ? { vad_model: combo.vad } : {}),
    kws: Boolean(combo.kws),
    ...(combo.kws ? { kws_model: combo.kws } : {}),
    denoise: Boolean(combo.denoise),
    ...(combo.denoise ? { denoise_model: combo.denoise } : {}),
    turn: Boolean(combo.turn),
    ...(combo.turn ? { turn_model: combo.turn } : {}),
    asr: true,
    asr_model: combo.asr,
    tts: true,
    tts_model: combo.tts,
  };
}

/** 当前设置是否正是这套组合。 */
export function comboActive(combo, settings) {
  const value = settings || {};
  const same = (on, model, want) => (want ? Boolean(on) && model === want : !on);
  return (
    same(value.vad, value.vad_model, combo.vad) &&
    same(value.kws, value.kws_model, combo.kws) &&
    same(value.denoise, value.denoise_model, combo.denoise) &&
    same(value.turn, value.turn_model, combo.turn) &&
    same(value.asr, value.asr_model, combo.asr) &&
    same(value.tts, value.tts_model, combo.tts)
  );
}

export function builtinSectionSourceView(section, status) {
  if (section === "jev") {
    const jevOn = Boolean(status?.local?.settings?.jev_filter_enabled ?? status?.jev?.voice_enabled);
    const rawJevModel = status?.local?.settings?.jev_model || status?.jev?.model || DEFAULT_JEV_MODEL;
    const activeLocalId = canonicalLocalJevModel(rawJevModel);
    const isCurrentLocal = Boolean(activeLocalId);
    const cloudModel = isCurrentLocal ? DEFAULT_JEV_MODEL : rawJevModel;
    const allLocal = Array.isArray(status?.local?.models) ? status.local.models : [];
    const catalogJevItems = allLocal.filter((m) => m && m.kind === "jev");
    const localSpecs = catalogJevItems.length > 0 ? catalogJevItems : LOCAL_JEV_FALLBACK_MODELS;
    const localOptions = localSpecs.map((item) => {
      const installed = catalogJevItems.length > 0 ? Boolean(item.installed) : isSpecificLocalJevInstalled(status, item.id);
      return {
        value: `local:${item.id}`,
        id: item.id,
        modelLabel: item.label,
        isLocal: true,
        installed,
        label: `本地 · ${item.label}${installed ? "" : "（未下载）"}`,
      };
    });
    const selectedLocalOption = isCurrentLocal
      ? localOptions.find((item) => item.id === activeLocalId) || null
      : null;
    const selectedLocalModel = isCurrentLocal
      ? catalogJevItems.find((item) => item.id === activeLocalId) ||
        (selectedLocalOption
          ? { id: selectedLocalOption.id, label: selectedLocalOption.modelLabel, installed: selectedLocalOption.installed }
          : null)
      : null;
    const installedLocalModel =
      (selectedLocalModel && selectedLocalModel.installed ? selectedLocalModel : null) ||
      catalogJevItems.find((item) => item.installed) ||
      (localOptions.find((item) => item.installed)
        ? {
            id: localOptions.find((item) => item.installed).id,
            label: localOptions.find((item) => item.installed).modelLabel,
            installed: true,
          }
        : null);
    const activeLocalInstalled = Boolean(selectedLocalOption?.installed);
    const cloudKeyReady = Boolean(
      status?.jev?.key_set ||
        status?.jev?.openrouter_llm_key_set ||
        (!isCurrentLocal && status?.local?.jev_key_configured)
    );
    const currentReady = isCurrentLocal ? activeLocalInstalled : cloudKeyReady;
    const presets = JEV_PRESET_MODELS.some((item) => item.id === cloudModel)
      ? JEV_PRESET_MODELS
      : [{ id: cloudModel, hint: "当前选用" }, ...JEV_PRESET_MODELS];
    const options = [
      { value: "off", label: "关闭（仅本地 0ms 规则）", installed: true },
      ...presets.map((item) => ({
        value: `jev:${item.id}`,
        id: item.id,
        modelLabel: item.id,
        isLocal: false,
        installed: cloudKeyReady,
        label: `开启 · ${item.id}${item.id === DEFAULT_JEV_MODEL ? "（推荐）" : ""}${cloudKeyReady ? "" : "（需配密钥）"}`,
      })),
      ...localOptions,
    ];
    const activeLocalLabel = selectedLocalModel?.label || activeLocalId || "本地决策模型";
    return {
      section: "jev",
      canUseLocal: true,
      source: jevOn ? (isCurrentLocal ? "local" : "jev") : "off",
      localOn: Boolean(jevOn && isCurrentLocal),
      localReady: currentReady,
      cloudConfigured: cloudKeyReady,
      cloudModel,
      activeText: jevOn
        ? isCurrentLocal
          ? activeLocalInstalled
            ? `本地（${activeLocalLabel}）`
            : `本地（${activeLocalLabel} · 未下载）`
          : currentReady
            ? `开启（${cloudModel}）`
            : `开启（${cloudModel} · 未配密钥）`
        : "关闭（仅本地 0ms 规则）",
      selectValue: jevOn ? (isCurrentLocal ? `local:${activeLocalId}` : `jev:${cloudModel}`) : "off",
      options,
      installedLocalModel,
      selectedLocalModel,
    };
  }
  const isVad = section === "vad";
  const entry = !isVad ? status?.models?.[section] : null;
  const cloudConfigured = Boolean(entry?.configured);
  const cloudModel = entry?.model || "";
  const defaultValue = isVad ? "energy" : "cloud";
  const defaultOptionLabel = isVad
    ? "内置能量判停（免下载）"
    : cloudConfigured
      ? `云端（${entry?.kind === "doubao" ? `豆包 · ${cloudModel}` : entry?.kind === "dashscope" ? `千问 · ${cloudModel}` : cloudModel}）`
      : "云端（未配置）";
  const colab = status?.local?.colab;
  const colabActive = Boolean(colab?.active);
  if (section === "llm") {
    if (colabActive) {
      return {
        section,
        canUseLocal: false,
        source: "colab",
        localOn: false,
        localReady: true,
        cloudConfigured: true,
        cloudModel: "qwen3-vl-8b",
        activeText: "Colab T4 联动（qwen3-vl-8b）",
        selectValue: "colab",
        options: [{ value: "colab", label: "Colab T4 联动（qwen3-vl-8b · 多模态+看图）", installed: true }],
        installedLocalModel: null,
        selectedLocalModel: null,
      };
    }
    return {
      section,
      canUseLocal: false,
      source: "cloud",
      localOn: false,
      localReady: false,
      cloudConfigured,
      cloudModel,
      activeText: cloudConfigured ? `云端（${cloudModel}）` : "未配置",
      selectValue: "cloud",
      options: [{ value: "cloud", label: defaultOptionLabel, installed: true }],
      installedLocalModel: null,
      selectedLocalModel: null,
    };
  }
  const localSettings = localSettingsForm(status?.local?.settings);
  const localOn = Boolean(status?.local?.settings?.[section]);
  const allLocal = Array.isArray(status?.local?.models) ? status.local.models : [];
  const kindModels = allLocal.filter((item) => item && item.kind === section);
  const selectedId = localSettings[`${section}_model`];
  const selectedLocalModel = kindModels.find((item) => item.id === selectedId) || kindModels[0] || null;
  const installedLocalModel =
    selectedLocalModel && selectedLocalModel.installed
      ? selectedLocalModel
      : kindModels.find((item) => item.installed) || null;
  if (section === "tts" && colabActive) {
    return {
      section,
      canUseLocal: true,
      source: "colab",
      localOn: false,
      localReady: true,
      cloudConfigured: true,
      cloudModel: "qwen3-tts",
      activeText: "Colab T4 联动（qwen3-tts · 550W 克隆声）",
      selectValue: "colab",
      options: [{ value: "colab", label: "Colab T4 联动（qwen3-tts · 550W 克隆声）", installed: true }],
      installedLocalModel,
      selectedLocalModel,
    };
  }
  const localReady = Boolean(localOn && selectedLocalModel?.installed);
  const source = localOn ? "local" : defaultValue;
  const activeText = localOn
    ? selectedLocalModel?.installed
      ? `本地（${selectedLocalModel.label}）`
      : `本地（${selectedLocalModel?.label || "未下载"} · 未安装）`
    : defaultOptionLabel;
  const localOptions =
    kindModels.length > 0
      ? kindModels.map((item) => ({
          value: `local:${item.id}`,
          id: item.id,
          modelLabel: item.label,
          installed: Boolean(item.installed),
          label: `本地 · ${item.label}${isRecommendedLocalModel(section, item.id) ? " · 推荐" : ""}${item.installed ? "" : "（未下载）"}`,
        }))
      : [
          {
            value: "local",
            id: selectedId,
            modelLabel: "本地模型",
            installed: false,
            label: "本地模型（未下载）",
          },
        ];
  const selectValue = localOn
    ? selectedLocalModel
      ? `local:${selectedLocalModel.id}`
      : "local"
    : defaultValue;
  return {
    section,
    canUseLocal: true,
    source,
    localOn,
    localReady,
    cloudConfigured,
    cloudModel,
    activeText,
    selectValue,
    options: [{ value: defaultValue, label: defaultOptionLabel, installed: true }, ...localOptions],
    installedLocalModel,
    selectedLocalModel,
  };
}

/**
 * 生成将某个分区（asr / tts / vad / jev）在顶部下拉框切换后的 localSave 载荷。
 */
export function builtinSourceSwitchPayload(status, section, targetSource) {
  if (section === "jev") {
    let form = localSettingsForm(status?.local?.settings);
    const target = String(targetSource || "");
    if (target === "off") {
      form = { ...form, jev_filter_enabled: false };
    } else if (target.startsWith("local:")) {
      const localId = canonicalLocalJevModel(target.slice("local:".length));
      if (!localId) return null;
      form = { ...form, jev_filter_enabled: true, jev_model: localId };
    } else if (target.startsWith("jev:")) {
      const rawId = target.slice("jev:".length).trim() || DEFAULT_JEV_MODEL;
      const maybeLocal = canonicalLocalJevModel(rawId);
      if (maybeLocal) {
        form = { ...form, jev_filter_enabled: true, jev_model: maybeLocal };
      } else {
        if (jevModelError(rawId)) return null;
        form = { ...form, jev_filter_enabled: true, jev_model: rawId };
      }
    } else {
      return null;
    }
    return localSettingsPayload(form);
  }
  if (section !== "asr" && section !== "tts" && section !== "vad") return null;
  const allLocal = Array.isArray(status?.local?.models) ? status.local.models : [];
  let form = localSettingsForm(status?.local?.settings);
  const target = String(targetSource || "");
  if (target.startsWith("local:")) {
    const modelId = target.slice("local:").length ? target.slice("local:".length).trim() : "";
    if (modelId) {
      form = selectLocalModel(form, allLocal, section, modelId);
    }
    form = { ...form, [section]: true };
  } else if (target === "local") {
    const view = builtinSectionSourceView(section, status);
    if (view.installedLocalModel && view.installedLocalModel.id !== form[`${section}_model`]) {
      form = selectLocalModel(form, allLocal, section, view.installedLocalModel.id);
    }
    form = { ...form, [section]: true };
  } else {
    form = { ...form, [section]: false };
  }
  return localSettingsPayload(form);
}

/**
 * 生成顶部混搭栏单分区切换的最小增量补丁（仅提交本次切换涉及的字段，避免覆盖其他分组在途或刚保存的改动）。
 */
export function builtinSourceSwitchPatch(status, section, targetSource) {
  if (section === "jev") {
    const target = String(targetSource || "");
    if (target === "off") {
      return { jev_filter_enabled: false };
    }
    if (target.startsWith("local:")) {
      const localId = canonicalLocalJevModel(target.slice("local:".length));
      if (!localId) return null;
      return { jev_filter_enabled: true, jev_model: localId };
    }
    if (target.startsWith("jev:")) {
      const rawId = target.slice("jev:".length).trim() || DEFAULT_JEV_MODEL;
      const maybeLocal = canonicalLocalJevModel(rawId);
      if (maybeLocal) {
        return { jev_filter_enabled: true, jev_model: maybeLocal };
      }
      if (jevModelError(rawId)) return null;
      return { jev_filter_enabled: true, jev_model: rawId };
    }
    return null;
  }
  const full = builtinSourceSwitchPayload(status, section, targetSource);
  if (!full) return null;
  const keys =
    section === "tts"
      ? ["tts", "tts_model", "tts_speaker"]
      : [section, `${section}_model`];
  return Object.fromEntries(keys.map((key) => [key, full[key]]));
}
