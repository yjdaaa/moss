// 一键测试全部环节与连贯测试：测试计划、结果汇总与录音 WAV 打包。

import { builtinModelForm, sectionInputError, sectionPayload } from "./sections.js";
import { MIX_SECTIONS, builtinSectionSourceView } from "./modelSources.js";
import { isSpeechTtsModel } from "./localModels.js";

/**
 * “测试全部环节”（2026-11-08 用户要求：链路栏右上角一键测试，哪个环节不通就在哪里报错）。
 * 按每个环节**当前选用**的来源决定怎么测，只用已保存的配置（密钥留空由桌面端沿用已保存的）：
 * - `static`：无需请求，直接给出结论（内置能量判停、Jev 已关闭、本地模型就绪 / 未下载、云端未配置等）；
 * - `section`：调用 `voice_builtin_models_test(section, payload)` 真实请求一次云端识别 / 大模型 / 合成；
 * - `jev`：调用 `voice_builtin_models_test("jev", {model})`，云端用已保存密钥、本地会先确认推理进程已启动。
 * 本地识别 / 合成 / 判停没有推理测试接口，只检查模型是否已下载、本地模型程序是否存在，文案如实说明。
 */
export function pipelineTestPlan(status) {
  const helperFound = status?.local?.helper_found !== false;
  const localStep = (key, label, view) => {
    const model = view.selectedLocalModel;
    if (!model?.installed) {
      return { key, kind: "static", ok: false, detail: `本地${label}模型“${model?.label || "未选择"}”还没下载：到下方“本地模型”下载，或在上方改回${key === "vad" ? "内置能量判停" : "云端"}` };
    }
    if (isSpeechTtsModel(model) && status?.local?.speech_env?.ready === false) {
      const summary = String(status?.local?.speech_env?.summary || "").trim();
      return {
        key,
        kind: "static",
        ok: false,
        detail: summary ? `Qwen3-TTS 本地运行时尚未就绪：${summary}` : "找不到 Qwen3-TTS 本地运行时 speech，本地合成无法运行",
      };
    }
    if (!helperFound && !isSpeechTtsModel(model)) {
      return { key, kind: "static", ok: false, detail: "找不到本地模型程序 moss-voice-local（应与 MOSS 放在同一目录），本地模型无法运行" };
    }
    return { key, kind: "static", ok: true, detail: `本地 ${model.label} 已下载（本地模型只检查是否就绪，不做试跑）` };
  };
  return MIX_SECTIONS.map(({ key, label }) => {
    const cleanLabel = label.replace(/^\d+\.\s*/, "");
    const view = builtinSectionSourceView(key, status);
    if (key === "vad") {
      if (view.source !== "local") return { key, kind: "static", ok: true, detail: "内置能量判停，无需测试" };
      return localStep(key, cleanLabel, view);
    }
    if (key === "jev") {
      if (view.source === "off") return { key, kind: "static", ok: true, skipped: true, detail: "已关闭，跳过" };
      if (view.source === "local") {
        const model = view.selectedLocalModel;
        if (!model?.installed) {
          return { key, kind: "static", ok: false, detail: `本地决策模型“${model?.label || "未选择"}”还没下载：到下方“本地模型”下载，或在上方关闭 Jev` };
        }
        return { key, kind: "jev", input: { model: model.id } };
      }
      if (!view.cloudConfigured) {
        return { key, kind: "static", ok: false, detail: "还没配置 OpenRouter API Key：到下方 Jev 卡片填写，或在上方关闭 Jev" };
      }
      return { key, kind: "jev", input: { api_key: "", model: view.cloudModel } };
    }
    if (view.source === "local") return localStep(key, cleanLabel, view);
    if (view.source === "colab") {
      return { key, kind: "section", section: "colab", input: { target: key } };
    }
    const entry = status?.models?.[key];
    if (!entry?.configured) {
      return { key, kind: "static", ok: false, detail: `云端${cleanLabel}还没配置：到下方“${cleanLabel}”卡片填写并保存${key === "llm" ? "" : "，或在上方改用本地模型"}` };
    }
    const form = builtinModelForm(key, entry);
    const inputError = sectionInputError(key, form);
    if (inputError) return { key, kind: "static", ok: false, detail: `已保存的配置不完整：${inputError}` };
    return { key, kind: "section", section: key, input: sectionPayload(key, form) };
  });
}

/** 把一次测试返回（`TestResult`）或调用异常变成环节结论；`errorText` 由调用方传入（通常是 `voiceErrorText`）。 */
export function pipelineTestOutcome(result, error, errorText = (e) => String(e?.message || e || "测试失败")) {
  if (error) return { ok: false, detail: errorText(error) };
  const ok = Boolean(result?.ok);
  const elapsed = Number(result?.elapsed_ms);
  const timing = Number.isFinite(elapsed) && elapsed > 0 ? `（${elapsed} ms）` : "";
  const detail = String(result?.detail || "").trim() || (ok ? "连接正常" : "测试未通过");
  // 大模型结果自带分段耗时（首次响应 / 思考 / 工具调用，n73），不再重复补总耗时
  const hasTiming = /首次响应/.test(detail);
  return { ok, detail: `${detail}${ok && !hasTiming ? timing : ""}` };
}

/** 汇总一句话：全部通过 / 哪几个环节不通。`results` 为 `{key: {ok, skipped}}`。 */
export function pipelineTestSummary(results) {
  const failed = MIX_SECTIONS.filter(({ key }) => results?.[key] && results[key].ok === false).map(({ label }) => label.replace(/^\d+\.\s*/, ""));
  const passed = MIX_SECTIONS.filter(({ key }) => results?.[key]?.ok && !results[key].skipped).length;
  if (failed.length === 0) return { ok: true, text: `全部环节测试通过（${passed} 项正常${passed < MIX_SECTIONS.length ? "，其余已关闭跳过" : ""}）` };
  return { ok: false, text: `${failed.length} 个环节不通：${failed.join("、")}。原因见对应下拉框下方` };
}

/**
 * 将单声道浮点音频样本（`-1..1`）在前端内存打包为标准 16-bit PCM WAV DataURL，
 * 供「录真话测」跨 WebView2 / WebKit 无编解码依赖直接发给后端 `chain_test`。
 */
export function encodeMonoWavDataUrl(floatSamples, sampleRate = 16000) {
  const rate = Math.max(8000, Math.min(96000, Math.trunc(Number(sampleRate) || 16000)));
  const len = Array.isArray(floatSamples) || ArrayBuffer.isView(floatSamples) ? floatSamples.length : 0;
  const dataBytes = len * 2;
  const buf = new ArrayBuffer(44 + dataBytes);
  const view = new DataView(buf);
  const writeAscii = (offset, str) => {
    for (let i = 0; i < str.length; i += 1) view.setUint8(offset + i, str.charCodeAt(i));
  };
  writeAscii(0, "RIFF");
  view.setUint32(4, 36 + dataBytes, true);
  writeAscii(8, "WAVE");
  writeAscii(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // Mono
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeAscii(36, "data");
  view.setUint32(40, dataBytes, true);
  for (let i = 0; i < len; i += 1) {
    const clamped = Math.max(-1, Math.min(1, Number(floatSamples[i]) || 0));
    view.setInt16(44 + i * 2, clamped < 0 ? Math.round(clamped * 32768) : Math.round(clamped * 32767), true);
  }
  const bytes = new Uint8Array(buf);
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  const b64 = typeof globalThis.btoa === "function"
    ? globalThis.btoa(binary)
    : Buffer.from(bytes).toString("base64");
  return `data:audio/wav;base64,${b64}`;
}

/**
 * 把后端 `voice_builtin_models_test("chain", ...)` 的整链连贯测试返回转换为顶部 5 栏结果视图与接力摘要。
 */
export function chainTestPipelineView(result, error, errorText = (e) => String(e?.message || e || "连贯测试失败")) {
  if (error) {
    const msg = errorText(error);
    return {
      running: false,
      mode: "chain",
      results: {},
      summary: { ok: false, text: `连贯试跑失败：${msg}` },
      chainMeta: null,
    };
  }
  const rawSteps = result?.steps && typeof result.steps === "object" ? result.steps : {};
  const results = {};
  for (const { key } of MIX_SECTIONS) {
    const s = rawSteps[key];
    if (!s || typeof s !== "object") continue;
    results[key] = {
      ok: Boolean(s.ok),
      skipped: Boolean(s.skipped),
      detail: String(s.detail || (s.ok ? "通过" : "未通过")),
    };
  }
  const ok = Boolean(result?.ok);
  const summaryText = String(result?.detail || "").trim()
    || (ok ? "五步全链路连贯试跑通过" : "连贯试跑未全部通过，请查看上方红叉环节");
  return {
    running: false,
    mode: "chain",
    results,
    summary: { ok, text: summaryText },
    chainMeta: {
      ok,
      totalMs: Number(result?.total_ms) || 0,
      seedSource: String(result?.seed_source || ""),
      inputAudioUrl: result?.input_audio_data_url || null,
      recognizedText: result?.recognized_text || "",
      replyText: result?.reply_text || "",
      replyAudioUrl: result?.reply_audio_data_url || null,
    },
  };
}
