import React, { useEffect, useRef, useState } from "react";
import { FlaskConical, Trash2, ChevronDown, RefreshCw } from "lucide-react";
import { showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { SaveBar, VoiceGroup } from "../VoiceLayout.jsx";
import {
  DEFAULT_JEV_DECISIONS_URL,
  DEFAULT_JEV_MODEL,
  JEV_PRESET_MODELS,
  builtinJevForm,
  builtinJevKeyPlaceholder,
  builtinJevSavePayload,
  builtinJevSummary,
  builtinSectionSourceView,
  canonicalLocalJevModel,
  isSpecificLocalJevInstalled,
  jevFingerprint,
  jevModelError,
  syncJevDraft,
  savedNoteText,
  voiceErrorText,
} from "../../voiceState.js";
import { restartEffect, JEV_GROUP_TITLE, focusJevSwitch } from "./shared.js";

/** OpenRouter · Jev 决策模型配置卡片（与右上角系统设置共享本机 jev_api_key 与加工反馈开关）。 */
function BuiltinJevGroup({ builtin }) {
  const { status, setStatus, busy } = builtin;
  const baseline = builtinJevForm(status);
  const baselineSig = JSON.stringify(baseline);
  const [form, setForm] = useState(baseline);
  const prevBaselineRef = useRef(baseline);
  const opSeqRef = useRef(0);
  const [test, setTest] = useState(null);
  const [result, setResult] = useState(null);
  const [localBusy, setLocalBusy] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [remoteModels, setRemoteModels] = useState({ loading: false, ok: false, models: [], detail: "" });
  const comboRef = useRef(null);

  // 已保存的设置内容（签名）变了才同步草稿；运行时读最新的 baseline
  const latestBaselineRef = useRef(baseline);
  useEffect(() => {
    latestBaselineRef.current = baseline;
  });
  useEffect(() => {
    const prev = prevBaselineRef.current;
    const next = latestBaselineRef.current;
    setForm((current) => syncJevDraft(prev, next, current));
    if (prev.model !== next.model) {
      setTest(null);
    }
    prevBaselineRef.current = next;
  }, [baselineSig]);

  useEffect(() => {
    if (!menuOpen) return undefined;
    const onPointerDown = (event) => {
      if (comboRef.current && !comboRef.current.contains(event.target)) {
        setMenuOpen(false);
      }
    };
    // 同上：click 时再收起，避免下方按钮在按下与松开之间移位而丢失点击。
    document.addEventListener("click", onPointerDown);
    return () => document.removeEventListener("click", onPointerDown);
  }, [menuOpen]);

  const sourceView = builtinSectionSourceView("jev", status);
  const localOn = sourceView.localOn;
  const rawJevModel = status?.local?.settings?.jev_model || status?.jev?.model || DEFAULT_JEV_MODEL;
  const activeLocalId = canonicalLocalJevModel(rawJevModel);
  const isCurrentLocal = Boolean(activeLocalId);
  const localInstalled = activeLocalId ? isSpecificLocalJevInstalled(status, activeLocalId) : false;
  const keyReady = Boolean(
    status?.jev?.key_set || status?.jev?.openrouter_llm_key_set || (!isCurrentLocal && status?.local?.jev_key_configured)
  );
  const typedKey = String(form.api_key || "").trim();
  const modelChanged = String(form.model || "").trim() !== baseline.model;
  const keyOrModelChanged = Boolean(typedKey) || modelChanged;
  const dirty = keyOrModelChanged || form.voice_enabled !== baseline.voice_enabled || form.feedback_enabled !== baseline.feedback_enabled;
  const disablingBothSwitches = !form.voice_enabled && !form.feedback_enabled;
  const preserveLocalWithoutCloudKey =
    isCurrentLocal && !modelChanged && !typedKey && (localInstalled || disablingBothSwitches);
  const cloudInputErr = jevModelError(form.model) || (!keyReady && !typedKey ? "请填写 OpenRouter API Key" : "");
  const inputErr = preserveLocalWithoutCloudKey ? "" : cloudInputErr;
  const currentFp = jevFingerprint(form);
  const needsTest = preserveLocalWithoutCloudKey ? false : (!keyReady || keyOrModelChanged);
  const saveEnabled = !inputErr && (!needsTest || Boolean(test?.ok && test.fingerprint === currentFp));
  const sectionBusy = Boolean(busy || localBusy);
  const mergedOptions = (() => {
    const map = new Map();
    for (const item of JEV_PRESET_MODELS) map.set(item.id, item);
    for (const id of remoteModels.models) {
      if (!map.has(id)) map.set(id, { id, hint: "OpenRouter 在线模型" });
    }
    return [...map.values()];
  })();

  const update = (key, value) => {
    setForm((prev) => ({ ...prev, [key]: value }));
    setResult(null);
  };

  const fetchModels = async () => {
    setRemoteModels((prev) => ({ ...prev, loading: true }));
    try {
      const outcome = await voiceApi.builtinModelsList("jev", { api_key: form.api_key });
      setRemoteModels({
        loading: false,
        ok: Boolean(outcome.ok),
        models: Array.isArray(outcome.models) ? outcome.models : [],
        detail: outcome.detail || "",
      });
      setMenuOpen(true);
    } catch (error) {
      setRemoteModels({ loading: false, ok: false, models: [], detail: voiceErrorText(error) });
      setMenuOpen(true);
    }
  };

  const runTest = async () => {
    const err = cloudInputErr;
    if (err) {
      setTest({ ok: false, detail: err, fingerprint: "" });
      return;
    }
    const seq = ++opSeqRef.current;
    setLocalBusy("builtin-test-jev");
    setResult(null);
    const fp = jevFingerprint(form);
    try {
      const outcome = await voiceApi.builtinModelsTest("jev", { api_key: form.api_key, model: form.model });
      if (opSeqRef.current === seq) {
        setTest({ ...outcome, fingerprint: outcome.ok ? fp : "" });
      }
    } catch (error) {
      if (opSeqRef.current === seq) {
        setTest({ ok: false, detail: voiceErrorText(error), fingerprint: "" });
      }
    } finally {
      if (opSeqRef.current === seq) {
        setLocalBusy("");
      }
    }
  };

  const save = async () => {
    if (!saveEnabled) return;
    const seq = ++opSeqRef.current;
    setLocalBusy("builtin-save-jev");
    setResult(null);
    try {
      const next = await voiceApi.builtinModelsSave({
        jev: builtinJevSavePayload(status, form, baseline),
      });
      const nextBaseline = builtinJevForm(next);
      prevBaselineRef.current = nextBaseline;
      setStatus(next);
      setForm(nextBaseline);
      setTest(null);
      setResult({ ok: true, text: savedNoteText("Jev 决策模型", restartEffect(next)) });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      if (opSeqRef.current === seq) {
        setLocalBusy("");
      }
    }
  };

  const clear = async () => {
    const ok = await showConfirm("将删除本机保存的 Jev OpenRouter API Key，并同时关闭语音 Jev 筛选与加工反馈判断。", {
      title: "删除 Jev 密钥与配置",
      danger: true,
      confirmText: "删除",
    });
    if (!ok) return;
    const seq = ++opSeqRef.current;
    setLocalBusy("builtin-clear-jev");
    setResult(null);
    try {
      const next = await voiceApi.builtinModelsSave({}, ["jev"]);
      const nextBaseline = builtinJevForm(next);
      prevBaselineRef.current = nextBaseline;
      setStatus(next);
      setForm(nextBaseline);
      setTest(null);
      setResult({ ok: true, text: `已删除 Jev 密钥与配置 · ${restartEffect(next)}` });
    } catch (error) {
      setResult({ ok: false, text: `删除失败：${voiceErrorText(error)}` });
    } finally {
      if (opSeqRef.current === seq) {
        setLocalBusy("");
      }
    }
  };

  return (
    <VoiceGroup id="builtin-model-jev" title={JEV_GROUP_TITLE} summary={builtinJevSummary(status)} dirty={dirty}>
      {localOn && (
        <p className="voice-muted">
          当前已在上方下拉框选用本地 Jev 决策（{sourceView.selectedLocalModel?.label || activeLocalId}）；这里的 OpenRouter 云端配置与密钥作为备用保留，随时可在上方下拉切回。
        </p>
      )}
      <p className="voice-muted">
        在语音链路中位于「语音识别」之后、「大模型」之前：模型下拉选择作用于内置语音前置粗筛（半截话续听防抢话、口语快答分流、6 大工具组/外部 MCP 粗筛与记忆精筛）；OpenRouter 密钥同时可供雕刻加工反馈共用（加工反馈接口按契约固定使用 typesafe/jev-1.13），绝不授权任何设备动作。
      </p>
      <div className="voice-form-grid">
        <div className="voice-field">
          <div className="voice-field-head">
            <label htmlFor="voice-builtin-apikey-jev">OpenRouter API Key（不回显；留空则沿用已保存）</label>
          </div>
          <input
            className="text-input mono"
            type="password"
            autoComplete="off"
            value={form.api_key}
            placeholder={builtinJevKeyPlaceholder(status)}
            onChange={(e) => update("api_key", e.target.value)}
            id="voice-builtin-apikey-jev"
          />
        </div>
        <div className="voice-field" ref={comboRef}>
          <div className="voice-field-head">
            <label htmlFor="voice-builtin-model-jev">Jev 模型名称</label>
            <button
              type="button"
              className="voice-link-btn"
              onClick={fetchModels}
              disabled={sectionBusy || remoteModels.loading}
              id="voice-builtin-fetch-models-jev"
            >
              <RefreshCw size={11} className={remoteModels.loading ? "voice-spin" : ""} />
              {remoteModels.loading ? "读取中…" : "读取模型"}
            </button>
          </div>
          <div className="voice-combo-wrap">
            <input
              className="text-input mono voice-combo-input"
              value={form.model}
              placeholder="如 typesafe/jev-1.14"
              onChange={(e) => update("model", e.target.value)}
              onFocus={() => setMenuOpen(true)}
              id="voice-builtin-model-jev"
            />
            <button
              type="button"
              className="voice-combo-toggle"
              aria-label="展开可选 Jev 模型列表"
              aria-expanded={menuOpen}
              onClick={() => setMenuOpen((open) => !open)}
              id="voice-builtin-toggle-jev"
            >
              <ChevronDown size={14} />
            </button>
          </div>
          {menuOpen && (
            <div className="voice-combo-menu" role="listbox" aria-label="Jev 决策可选模型" id="voice-builtin-model-options-jev">
              <div className="voice-combo-meta">
                <span>{remoteModels.detail || `可选 Jev 模型（共 ${mergedOptions.length} 个，也可手填新版本）`}</span>
              </div>
              <div className="voice-combo-list">
                {mergedOptions.map((opt) => (
                  <div
                    key={opt.id}
                    role="option"
                    aria-selected={opt.id === form.model}
                    className={`voice-combo-option mono${opt.id === form.model ? " is-selected" : ""}`}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      update("model", opt.id);
                      setMenuOpen(false);
                    }}
                  >
                    <span className="voice-combo-id">{opt.id}</span>
                    {opt.hint && <span className="voice-combo-hint">{opt.hint}</span>}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        {/* n69：语音里用不用 Jev（jev_filter_enabled）只在顶部第 3 栏切换，这里只显示状态；细项在“对话体验” */}
        <div className="voice-wide voice-inline" id="voice-builtin-jev-voice-state">
          <span>
            语音对话里用 Jev 粗筛：<b>{baseline.voice_enabled ? (localOn ? "已开启（用本地模型）" : "已开启（用这里的云端模型）") : "未开启"}</b>
            <span className="voice-muted">（在本页顶部第 3 栏“Jev 前置粗筛”下拉切换；具体筛哪些在下面“对话体验”里设）</span>
          </span>
          <button type="button" className="voice-link-btn" onClick={focusJevSwitch} id="voice-builtin-jev-goto-switch">
            去顶部切换
          </button>
        </div>
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.feedback_enabled}
            onChange={(e) => update("feedback_enabled", e.target.checked)}
            id="voice-builtin-jev-feedback-enabled"
          />
          <span>加工反馈判断（和语音无关的另一项功能：雕完后用这把 OpenRouter 密钥、固定 typesafe/jev-1.13 判断“太焦、太浅、切不透”并给调参建议；在这里开关、点保存生效）</span>
        </label>
      </div>
      <div className="voice-final mono">最终请求地址：{DEFAULT_JEV_DECISIONS_URL}</div>
      {inputErr && dirty && <div className="voice-hint voice-hint--bad">{inputErr}</div>}
      {test ? (
        <div className={`voice-hint voice-hint--${test.ok ? "ok" : "bad"}`} id="voice-builtin-model-result-jev">
          <span>
            {test.ok ? "测试通过" : "测试未通过"}
            {typeof test.elapsed_ms === "number" ? ` · ${(test.elapsed_ms / 1000).toFixed(1)} s` : ""} · {test.detail}
            {test.ok && test.fingerprint !== currentFp && "（测试后密钥或模型已修改，请重新测试）"}
          </span>
        </div>
      ) : (
        <p className="voice-muted">
          {needsTest ? "首次配置或修改密钥/模型后请先测试连接，通过后才能保存。" : "本机已配置密钥；只改勾选开关可直接保存，也可随时测试连接。"}
        </p>
      )}
      <SaveBar
        id="voice-builtin-model-save-jev"
        target="Jev 决策模型设置"
        dirty={dirty}
        busy={sectionBusy}
        saving={localBusy === "builtin-save-jev"}
        blocked={saveEnabled ? "" : inputErr || "先测试连接，通过后才能保存"}
        result={result}
        onSave={save}
        extra={
          <>
            <button
              type="button"
              className="btn btn-ghost"
              onClick={runTest}
              disabled={sectionBusy || Boolean(inputErr)}
              id="voice-builtin-model-test-jev"
            >
              <FlaskConical size={13} /> {localBusy === "builtin-test-jev" ? "测试中…" : "测试连接"}
            </button>
            {Boolean(status?.jev?.key_set) && (
              <button type="button" className="btn btn-ghost" onClick={clear} disabled={sectionBusy} id="voice-builtin-model-clear-jev">
                <Trash2 size={13} /> 删除此项
              </button>
            )}
          </>
        }
      />
    </VoiceGroup>
  );
}

export { BuiltinJevGroup };
