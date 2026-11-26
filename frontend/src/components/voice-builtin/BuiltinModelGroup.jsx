import React, { useEffect, useRef, useState } from "react";
import { Square, FlaskConical, Trash2, ChevronDown, RefreshCw, Upload, Volume2 } from "lucide-react";
import { showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { SaveBar, VoiceGroup } from "../VoiceLayout.jsx";
import {
  DASHSCOPE_MODEL_OPTIONS,
  DOUBAO_MODEL_OPTIONS,
  SECTIONS,
  alibabaOpenaiTtsMismatch,
  applyDefaultTtsVoice,
  builtinKeyPlaceholder,
  builtinModelForm,
  builtinModelSummary,
  builtinSectionSourceView,
  canAutoFetchModels,
  canCloneDashscopeModel,
  canSaveSection,
  chatOnlyAudioGatewayError,
  dashscopeClonedVoicesKey,
  filterModelOptions,
  finalUrl,
  findPeerCloudSource,
  formDirty,
  inferDashscopeVoiceTargetModel,
  isDashscope,
  isSectionBlank,
  modelsListError,
  modelsListKey,
  ownKeyReusable,
  presetModelOptionsFor,
  savedNoteText,
  isDoubao,
  sectionFingerprint,
  sectionInputError,
  sectionPayload,
  switchKind,
  tokenPlanEndpointMismatch,
  ttsVoiceOptionsFor,
  voiceErrorText,
} from "../../voiceState.js";
import { restartEffect } from "./shared.js";
import { remoteCacheKey, REMOTE_MODELS_CACHE } from "./remoteModelsCache.js";

/** 编辑地址 / 密钥时自动读取模型列表前的停手时间 */
const MODELS_AUTO_FETCH_DEBOUNCE_MS = 1000;

/** 内置服务的一项云端模型（大模型 / 语音识别 / 语音合成）：支持拉取全量模型下拉筛选或手填；先测试、后保存；密钥只提交、不回显。 */
function BuiltinModelGroup({ section, builtin }) {
  const { status, setStatus, busy } = builtin;
  const current = SECTIONS.find((item) => item.key === section);
  const entry = status.models?.[section];
  const baseline = builtinModelForm(section, entry);
  const [form, setForm] = useState(baseline);
  const [test, setTest] = useState(null);
  const [result, setResult] = useState(null);
  const [localBusy, setLocalBusy] = useState("");
  const [remoteModels, setRemoteModels] = useState({ key: "", items: [], modelSections: null, detail: "", ok: null, loading: false });
  const [menuOpen, setMenuOpen] = useState(false);
  const [voiceMenuOpen, setVoiceMenuOpen] = useState(false);
  const [playingAudio, setPlayingAudio] = useState(false);
  const [filtering, setFiltering] = useState(false);
  const [showAllModels, setShowAllModels] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const [clonedVoices, setClonedVoices] = useState({ key: "", items: [], error: "", loading: false });
  const comboRef = useRef(null);
  const voiceComboRef = useRef(null);
  const cloneFileRef = useRef(null);
  const audioRef = useRef(null);
  const fetchSeq = useRef(0);
  const voicesSeq = useRef(0);
  const autoVoicesKeyRef = useRef("");
  const dirty = formDirty(baseline, form);
  const saveEnabled = canSaveSection(section, form, test);
  const doubao = isDoubao(form);
  const dashscope = isDashscope(form);
  const presetKind = doubao || dashscope;
  const sourceView = builtinSectionSourceView(section, status);
  const localOn = sourceView.localOn;
  const sectionBusy = Boolean(busy || localBusy);
  const cloning = localBusy === "builtin-clone-tts";
  const n = (suffix) => `voice-builtin-${suffix}-${section}`;
  const peerCloud = findPeerCloudSource(section, status, form.url, form.kind || "openai");
  const peerKeySet = Boolean(peerCloud.sameOrigin?.keySet);
  const ownKeySet = ownKeyReusable(entry, form, section);
  const currentListKey = modelsListKey(form, entry, peerKeySet, section);
  const currentVoicesKey = dashscopeClonedVoicesKey(form, entry, peerKeySet);
  const currentCacheKey = remoteCacheKey(section, form, entry, peerKeySet);
  const cachedModels = !presetKind ? REMOTE_MODELS_CACHE.get(currentCacheKey) : null;
  const presetOptions = !presetKind ? presetModelOptionsFor(section, form.url) : [];
  const liveItems = !presetKind
    ? remoteModels.key === currentListKey && remoteModels.ok !== null
      ? remoteModels.items
      : cachedModels
        ? cachedModels.items
        : null
    : null;
  const usingPresets = !presetKind && (!liveItems || liveItems.length === 0) && presetOptions.length > 0;
  const rawOptions = doubao
    ? DOUBAO_MODEL_OPTIONS[section] || []
    : dashscope
      ? DASHSCOPE_MODEL_OPTIONS[section] || []
      : liveItems && liveItems.length > 0
        ? liveItems
        : presetOptions;
  const modelSections = !presetKind && !usingPresets
    ? remoteModels.key === currentListKey && remoteModels.ok !== null
      ? remoteModels.modelSections
      : cachedModels?.modelSections || null
    : null;
  const allOptions = filterModelOptions(rawOptions, "", false, { section, modelSections });
  const matchedOptions = allOptions.filter((item) => item.sections?.includes(section));
  const anyClassified = allOptions.some((item) => item.sections?.length > 0);
  const onlyMatched = !presetKind && !showAllModels && anyClassified;
  const scopedOptions = onlyMatched ? matchedOptions : allOptions;
  const filteredOptions = filterModelOptions(rawOptions, form.model, filtering, { section, onlyMatched, modelSections });
  const baseVoiceOptions = section === "tts" ? ttsVoiceOptionsFor(form) : [];
  const canCloneVoice = section === "tts" && canCloneDashscopeModel(form);
  const clonedVoiceOptions =
    dashscope && canCloneVoice && clonedVoices.key === currentVoicesKey
      ? clonedVoices.items.map((item) => {
          const statusText = String(item.status || "OK").trim();
          const ready = !statusText || /^(?:ok|published|success|succeeded)$/i.test(statusText);
          const targetModel = item.target_model || inferDashscopeVoiceTargetModel(item.voice_id);
          const statusLabel = ready ? "已克隆专属音色" : `克隆处理中（${statusText}，请稍候）`;
          return {
            id: item.voice_id,
            targetModel,
            ready,
            hint: `${statusLabel}${targetModel ? ` · 绑定 ${targetModel}` : ""}`,
          };
        })
      : [];
  const voiceOptions = [...clonedVoiceOptions, ...baseVoiceOptions.filter((opt) => !clonedVoiceOptions.some((c) => c.id === opt.id))];
  const hasDashscopeKey = Boolean(String(form.api_key || "").trim() || ownKeySet || peerKeySet);
  const dashscopeCloneOnly = dashscope && canCloneVoice && baseVoiceOptions.length === 0;
  const hideEmptyVoiceToggle =
    dashscopeCloneOnly &&
    voiceOptions.length === 0 &&
    (!hasDashscopeKey || (clonedVoices.key === currentVoicesKey && !clonedVoices.loading && !clonedVoices.error));
  const gatewayAudioErr = !presetKind ? chatOnlyAudioGatewayError(section, form.url) : "";
  const alibabaTtsHint = section === "tts" ? alibabaOpenaiTtsMismatch(section, form) : null;
  const endpointMismatch = !presetKind && section === "llm" ? tokenPlanEndpointMismatch(form.url, form.api_key) : null;
  const listErr = !presetKind && form.url.trim() ? modelsListError(form, section) : "";
  const inlineListError =
    !presetKind && remoteModels.key === currentListKey && remoteModels.ok === false && remoteModels.detail
      ? remoteModels.detail
      : "";

  useEffect(() => {
    if (!menuOpen && !voiceMenuOpen) return undefined;
    const handleOutsideClick = (event) => {
      if (comboRef.current && !comboRef.current.contains(event.target)) {
        setMenuOpen(false);
        setFiltering(false);
      }
      if (voiceComboRef.current && !voiceComboRef.current.contains(event.target)) {
        setVoiceMenuOpen(false);
      }
    };
    // 用 click 而不是 mousedown 关闭：下拉列表占文档流，按下时收起会让下方按钮上移、松开落空导致点击丢失。
    document.addEventListener("click", handleOutsideClick);
    return () => document.removeEventListener("click", handleOutsideClick);
  }, [menuOpen, voiceMenuOpen]);

  useEffect(() => {
    return () => {
      if (audioRef.current) {
        audioRef.current.pause();
        audioRef.current = null;
      }
    };
  }, []);

  const update = (key, value) => {
    const cleaned =
      key === "url" && /^(?:https?|wss?):\/\//i.test(value.trim()) && !/[<>]/.test(value)
        ? value.trim().replace(/\s+/g, "").replace(/^(https?|wss?):\/\//i, (m) => m.toLowerCase())
        : value;
    setForm((all) => ({ ...all, [key]: cleaned }));
    setResult(null);
  };

  const stopAudioPreview = () => {
    if (audioRef.current) {
      audioRef.current.pause();
      audioRef.current = null;
    }
    setPlayingAudio(false);
  };

  const toggleAudioPreview = async () => {
    if (!test?.audio_data_url) return;
    if (playingAudio && audioRef.current) {
      stopAudioPreview();
      return;
    }
    stopAudioPreview();
    try {
      const audio = new Audio(test.audio_data_url);
      audioRef.current = audio;
      setPlayingAudio(true);
      audio.onended = () => setPlayingAudio(false);
      audio.onerror = () => setPlayingAudio(false);
      await audio.play();
    } catch {
      setPlayingAudio(false);
    }
  };

  const fetchModels = async ({ auto = false, nextForm = form } = {}) => {
    if (isDoubao(nextForm) || isDashscope(nextForm)) return;
    const err = modelsListError(nextForm, section);
    if (err) {
      if (!auto) setRemoteModels({ key: "", items: [], modelSections: null, detail: err, ok: false, loading: false });
      return;
    }
    const nextPeer = findPeerCloudSource(section, status, nextForm.url);
    const nextPeerKeySet = Boolean(nextPeer.sameOrigin?.keySet);
    const reqKey = modelsListKey(nextForm, entry, nextPeerKeySet);
    const cacheKey = remoteCacheKey(section, nextForm, entry, nextPeerKeySet);
    if (auto) {
      if (section === "llm" && tokenPlanEndpointMismatch(nextForm.url, nextForm.api_key)) return;
      if (!canAutoFetchModels(nextForm, entry, status, section)) return;
      if (remoteModels.loading && remoteModels.key === reqKey) return;
      if (remoteModels.key === reqKey && remoteModels.ok !== null) return;
      const hit = REMOTE_MODELS_CACHE.get(cacheKey);
      if (hit) {
        setRemoteModels({
          key: reqKey,
          items: hit.items,
          modelSections: hit.modelSections || null,
          suggestedUrl: hit.suggestedUrl || "",
          detail: hit.detail,
          ok: true,
          loading: false,
        });
        return;
      }
    }
    const seq = ++fetchSeq.current;
    setRemoteModels((prev) => ({
      key: reqKey,
      items: prev.key === reqKey ? prev.items : [],
      modelSections: prev.key === reqKey ? prev.modelSections : null,
      detail: "",
      ok: prev.key === reqKey ? prev.ok : null,
      loading: true,
    }));
    try {
      const outcome = await voiceApi.builtinModelsList(section, sectionPayload(section, nextForm));
      if (fetchSeq.current !== seq) return;
      const items = Array.isArray(outcome?.models) ? outcome.models : [];
      const metaSections = outcome?.model_sections && typeof outcome.model_sections === "object" ? outcome.model_sections : null;
      const detail = outcome?.detail || (outcome?.ok ? `已读取 ${items.length} 个模型` : "读取模型列表失败");
      const ok = Boolean(outcome?.ok);
      const suggestedUrl = ok && typeof outcome?.suggested_url === "string" ? outcome.suggested_url : "";
      if (ok && items.length > 0) {
        REMOTE_MODELS_CACHE.set(cacheKey, { items, modelSections: metaSections, detail, suggestedUrl });
      }
      setRemoteModels({
        key: reqKey,
        items,
        modelSections: metaSections,
        suggestedUrl,
        detail,
        ok,
        loading: false,
      });
    } catch (error) {
      if (fetchSeq.current !== seq) return;
      setRemoteModels({
        key: reqKey,
        items: [],
        modelSections: null,
        detail: voiceErrorText(error),
        ok: false,
        loading: false,
      });
    }
  };

  useEffect(() => {
    // 仅在用户当前编辑表单（dirty）且空闲时防抖自动拉取，避免后台隐藏标签或测试/保存中途并发触发 `/models`
    if (!dirty || sectionBusy || presetKind || endpointMismatch || !canAutoFetchModels(form, entry, status, section)) return undefined;
    if (remoteModels.key === currentListKey && (remoteModels.loading || remoteModels.ok !== null)) return undefined;
    // 停手 1 秒再读：地址、密钥还没输完时不发请求，免得先报一次鉴权失败
    const timer = setTimeout(() => {
      fetchModels({ auto: true, nextForm: form });
    }, MODELS_AUTO_FETCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty, sectionBusy, presetKind, Boolean(endpointMismatch), currentListKey]);

  // 裸域名只在 /v1 下读到列表：对话不会自动补 /v1，提示一键改用（后端 `suggested_url`）
  const v1Suggestion =
    !presetKind && remoteModels.key === currentListKey && remoteModels.suggestedUrl
      ? remoteModels.suggestedUrl
      : "";
  const applyV1Suggestion = () => {
    if (!v1Suggestion) return;
    const nextForm = { ...form, url: v1Suggestion };
    setForm(nextForm);
    setResult(null);
    setTest(null);
    fetchModels({ auto: true, nextForm });
  };

  const applySuggestedEndpoint = () => {
    if (!endpointMismatch) return;
    const nextForm = { ...form, url: endpointMismatch.suggestedUrl };
    setForm(nextForm);
    setResult(null);
    setTest(null);
    fetchModels({ auto: true, nextForm });
  };

  const applyDashscopeKind = () => {
    setForm((all) => applyDefaultTtsVoice(switchKind("tts", all, "dashscope")));
    setMenuOpen(false);
    setVoiceMenuOpen(false);
    setFiltering(false);
    setTest(null);
    setResult(null);
  };

  const loadClonedVoices = async (targetForm = form) => {
    if (!isDashscope(targetForm)) return;
    const targetPeer = findPeerCloudSource("tts", status, targetForm.url, "dashscope");
    const targetPeerKeySet = Boolean(targetPeer.sameOrigin?.keySet);
    const targetOwnKeySet = ownKeyReusable(entry, targetForm, "tts");
    const hasKey = Boolean(String(targetForm.api_key || "").trim() || targetOwnKeySet || targetPeerKeySet);
    if (!hasKey) return;
    const reqKey = dashscopeClonedVoicesKey(targetForm, entry, targetPeerKeySet);
    if (clonedVoices.loading && clonedVoices.key === reqKey) return;
    const seq = ++voicesSeq.current;
    setClonedVoices((prev) => ({
      key: prev.key === reqKey ? reqKey : "",
      items: prev.key === reqKey ? prev.items : [],
      error: "",
      loading: true,
    }));
    try {
      const res = await voiceApi.ttsClonedVoices(sectionPayload("tts", targetForm));
      if (voicesSeq.current !== seq) return;
      if (res?.ok === false) {
        setClonedVoices({
          key: "",
          items: [],
          error: String(res?.detail || "读取已克隆音色失败"),
          loading: false,
        });
        return;
      }
      const items = Array.isArray(res?.voices) ? res.voices : [];
      setClonedVoices({ key: reqKey, items, error: "", loading: false });
    } catch (error) {
      if (voicesSeq.current !== seq) return;
      setClonedVoices({ key: "", items: [], error: voiceErrorText(error), loading: false });
    }
  };

  useEffect(() => {
    if (section !== "tts" || !dashscopeCloneOnly || !hasDashscopeKey || sectionBusy) return undefined;
    if (clonedVoices.key === currentVoicesKey || clonedVoices.loading || autoVoicesKeyRef.current === currentVoicesKey) {
      return undefined;
    }
    const delay = String(form.api_key || "").trim() ? MODELS_AUTO_FETCH_DEBOUNCE_MS : 0;
    const timer = setTimeout(() => {
      autoVoicesKeyRef.current = currentVoicesKey;
      void loadClonedVoices(form);
    }, delay);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [section, dashscopeCloneOnly, hasDashscopeKey, sectionBusy, currentVoicesKey, clonedVoices.key, clonedVoices.loading]);

  const handleCloneFilePick = (event) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    if (file.size > 7 * 1024 * 1024) {
      setTest({
        ok: false,
        detail: "参考录音文件不能超过 7 MB（推荐上传 10～20 秒清晰、无背景音乐的单人说话 WAV / MP3 / M4A）",
        fingerprint: "",
      });
      return;
    }
    const snapshotForm = { ...form };
    stopAudioPreview();
    setMenuOpen(false);
    setVoiceMenuOpen(false);
    setFiltering(false);
    setResult(null);
    setLocalBusy("builtin-clone-tts");
    const reader = new FileReader();
    reader.onload = async () => {
      try {
        const dataUrl = String(reader.result || "");
        const payload = {
          ...sectionPayload("tts", snapshotForm),
          file_name: file.name,
          audio_base64: dataUrl,
        };
        const outcome = await voiceApi.ttsVoiceClone(payload);
        if (outcome?.ok && outcome?.voice_id) {
          const nextForm = {
            ...snapshotForm,
            kind: "dashscope",
            model: outcome.model || snapshotForm.model,
            voice: outcome.voice_id,
          };
          const previewReady = outcome.preview_ok !== false && Boolean(outcome.audio_data_url);
          const fingerprint = previewReady ? sectionFingerprint("tts", nextForm) : "";
          setForm((prev) =>
            isDashscope(prev)
              ? {
                  ...prev,
                  model: outcome.model || prev.model,
                  voice: outcome.voice_id,
                }
              : prev,
          );
          setTest({ ...outcome, ok: previewReady, fingerprint });
          loadClonedVoices(nextForm);
        } else {
          setClonedVoices({ key: "", items: [], error: "", loading: false });
          setTest({
            ok: false,
            detail: outcome?.detail || "声音克隆未通过",
            fingerprint: "",
          });
        }
      } catch (error) {
        setClonedVoices({ key: "", items: [], error: "", loading: false });
        setTest({ ok: false, detail: voiceErrorText(error), fingerprint: "" });
      } finally {
        setLocalBusy("");
      }
    };
    reader.onerror = () => {
      setLocalBusy("");
      setTest({ ok: false, detail: "读取参考录音文件失败", fingerprint: "" });
    };
    reader.readAsDataURL(file);
  };

  const reusePeerSource = (event) => {
    event?.stopPropagation?.();
    if (!peerCloud.reusable) return;
    const nextForm = { ...form, url: peerCloud.reusable.baseUrl };
    setForm(nextForm);
    setResult(null);
    if (!presetKind) {
      setMenuOpen(true);
      setFiltering(false);
      fetchModels({ auto: true, nextForm });
    }
  };

  const pickModel = (modelId) => {
    if (section === "tts") {
      setForm((all) => applyDefaultTtsVoice(all, modelId));
      setResult(null);
    } else {
      update("model", modelId);
    }
    setMenuOpen(false);
    setFiltering(false);
    setActiveIndex(-1);
  };

  const handleModelFocus = () => {
    setFiltering(false);
    setActiveIndex(-1);
    if (presetKind || allOptions.length > 0 || Boolean(form.url.trim()) || canAutoFetchModels(form, entry, status, section)) {
      setMenuOpen(true);
      if (!endpointMismatch) fetchModels({ auto: true });
    }
  };

  const handleModelChange = (event) => {
    const value = event.target.value;
    update("model", value);
    setFiltering(true);
    setActiveIndex(-1);
    if (allOptions.length > 0) setMenuOpen(true);
  };

  const handleModelKeyDown = (event) => {
    if (event.key === "Tab" && menuOpen) {
      setMenuOpen(false);
      setFiltering(false);
      return;
    }
    if (event.key === "Escape" && menuOpen) {
      event.preventDefault();
      setMenuOpen(false);
      setFiltering(false);
      return;
    }
    if (event.key === "ArrowDown") {
      if (!menuOpen) {
        setMenuOpen(true);
        setFiltering(false);
        fetchModels({ auto: true });
        return;
      }
      if (filteredOptions.length > 0) {
        event.preventDefault();
        setActiveIndex((idx) => (idx + 1) % filteredOptions.length);
      }
      return;
    }
    if (event.key === "ArrowUp" && menuOpen && filteredOptions.length > 0) {
      event.preventDefault();
      setActiveIndex((idx) => (idx <= 0 ? filteredOptions.length - 1 : idx - 1));
      return;
    }
    if (event.key === "Enter" && menuOpen && activeIndex >= 0 && filteredOptions[activeIndex]) {
      event.preventDefault();
      pickModel(filteredOptions[activeIndex].id);
    }
  };

  const toggleMenu = () => {
    const nextOpen = !menuOpen;
    setMenuOpen(nextOpen);
    setFiltering(false);
    setActiveIndex(-1);
    if (nextOpen && !endpointMismatch) fetchModels({ auto: true });
  };


  const runTest = async () => {
    setMenuOpen(false);
    setVoiceMenuOpen(false);
    stopAudioPreview();
    const inputError = sectionInputError(section, form);
    if (inputError) {
      setTest({ ok: false, detail: inputError, fingerprint: "" });
      return;
    }
    const fingerprint = sectionFingerprint(section, form);
    setLocalBusy(`builtin-test-${section}`);
    try {
      const outcome = await voiceApi.builtinModelsTest(section, sectionPayload(section, form));
      setTest({ ...outcome, fingerprint });
    } catch (error) {
      setTest({ ok: false, detail: voiceErrorText(error), fingerprint: "" });
    } finally {
      setLocalBusy("");
    }
  };

  const cancelInflightFetch = () => {
    fetchSeq.current += 1;
    voicesSeq.current += 1;
    setRemoteModels((prev) => (prev.loading ? { ...prev, loading: false } : prev));
    setClonedVoices((prev) => (prev.loading ? { ...prev, loading: false } : prev));
  };

  const save = async () => {
    const savedFromListKey = currentListKey;
    const savedFromVoicesKey = currentVoicesKey;
    cancelInflightFetch();
    setMenuOpen(false);
    setVoiceMenuOpen(false);
    stopAudioPreview();
    setLocalBusy(`builtin-save-${section}`);
    try {
      const next = await voiceApi.builtinModelsSave({ [section]: sectionPayload(section, form) });
      const nextEntry = next.models?.[section];
      const nextForm = builtinModelForm(section, nextEntry);
      setStatus(next);
      setForm(nextForm);
      const nextPeer = findPeerCloudSource(section, next, nextForm.url, nextForm.kind || "openai");
      const nextPeerKeySet = Boolean(nextPeer.sameOrigin?.keySet);
      const nextListKey = modelsListKey(nextForm, nextEntry, nextPeerKeySet, section);
      const nextCacheKey = remoteCacheKey(section, nextForm, nextEntry, nextPeerKeySet);
      fetchSeq.current += 1;
      voicesSeq.current += 1;
      if (!isDashscope(nextForm)) {
        setClonedVoices({ key: "", items: [], error: "", loading: false });
      } else {
        const nextVoicesKey = dashscopeClonedVoicesKey(nextForm, nextEntry, nextPeerKeySet);
        setClonedVoices((prev) =>
          prev.key === savedFromVoicesKey
            ? { key: nextVoicesKey, items: prev.items, error: "", loading: false }
            : { key: "", items: [], error: "", loading: false },
        );
      }
      setRemoteModels((prev) => {
        if (prev.key === savedFromListKey && prev.items.length > 0) {
          REMOTE_MODELS_CACHE.set(nextCacheKey, {
            items: prev.items,
            modelSections: prev.modelSections || null,
            suggestedUrl: prev.suggestedUrl || "",
            detail: prev.detail,
          });
          return { ...prev, key: nextListKey, loading: false };
        }
        return prev.key === savedFromListKey
          ? { ...prev, key: nextListKey, loading: false }
          : { key: "", items: [], modelSections: null, suggestedUrl: "", detail: "", ok: null, loading: false };
      });
      setTest(null);
      setResult({ ok: true, text: savedNoteText(`${current.label}设置`, `密钥存本机、不显示 · ${restartEffect(next)}`) });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      setLocalBusy("");
    }
  };

  const clear = async () => {
    const message = `删除内置服务的“${current.label}”设置与已保存的密钥？删除后内置服务回到回声测试模式。`;
    if (!(await showConfirm(message, { title: "删除设置？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    cancelInflightFetch();
    setMenuOpen(false);
    setLocalBusy(`builtin-clear-${section}`);
    try {
      const next = await voiceApi.builtinModelsSave({}, [section]);
      fetchSeq.current += 1;
      REMOTE_MODELS_CACHE.delete(currentCacheKey);
      setStatus(next);
      setForm(builtinModelForm(section, next.models?.[section]));
      setRemoteModels({ key: "", items: [], modelSections: null, suggestedUrl: "", detail: "", ok: null, loading: false });
      setClonedVoices({ key: "", items: [], error: "", loading: false });
      setTest(null);
      setResult({ ok: true, text: `已删除${current.label}设置与密钥 · ${restartEffect(next)}` });
    } catch (error) {
      setResult({ ok: false, text: `删除失败：${voiceErrorText(error)}` });
    } finally {
      setLocalBusy("");
    }
  };

  return (
    <VoiceGroup
      id={`builtin-model-${section}`}
      title={current.label}
      summary={builtinModelSummary(section, entry, localOn)}
      tone={localOn ? (sourceView.localReady ? "ok" : "warn") : entry?.configured ? "ok" : undefined}
      dirty={dirty}
    >
      {localOn ? (
        <p className="voice-muted">
          当前已在上方下拉框选用本地{current.label}（{sourceView.selectedLocalModel?.label || "本地模型"}）；这里的云端配置与密钥作为备用保留，随时可在上方下拉切回。
        </p>
      ) : section === "llm" ? (
        <p className="voice-muted">支持云端大模型 API，也支持本机大模型服务（如 Ollama 填 http://127.0.0.1:11434/v1，密钥留空）。</p>
      ) : null}
      <div className="voice-form-grid">
        {section !== "llm" && (
          <label className="voice-wide">
            <span>接口类型</span>
            <select
              className="text-input"
              disabled={cloning}
              value={doubao ? "doubao" : dashscope ? "dashscope" : "openai"}
              onChange={(e) => {
                setForm((all) => {
                  const switched = switchKind(section, all, e.target.value);
                  return section === "tts" ? applyDefaultTtsVoice(switched, switched.model) : switched;
                });
                setMenuOpen(false);
                setVoiceMenuOpen(false);
                setFiltering(false);
                setTest(null);
                setResult(null);
              }}
              id={n("model-kind")}
            >
              <option value="openai">OpenAI 兼容（整段{section === "asr" ? "识别" : "合成"}）</option>
              <option value="doubao">豆包流式（{section === "asr" ? "边说边识别" : "边合成边播放"}，首句更快）</option>
              {section === "tts" && (
                <option value="dashscope">千问 / 百炼（流式合成 · 支持声音克隆）</option>
              )}
            </select>
          </label>
        )}
        <div className="voice-field voice-wide">
          <div className="voice-field-head">
            <label htmlFor={n("model-url")}>
              地址（{doubao ? "豆包 WebSocket 地址，一般不用改" : dashscope ? "千问或百炼地址，填根域名或大模型地址会自动转换" : current.hint}）
            </label>
            {!doubao && peerCloud.reusable && (
              <button
                type="button"
                className="voice-link-btn"
                disabled={sectionBusy}
                onClick={reusePeerSource}
                id={n("reuse-peer")}
              >
                复用{peerCloud.reusable.label}地址{peerCloud.reusable.keySet ? "与密钥" : ""}
              </button>
            )}
          </div>
          <input
            className="text-input mono"
            disabled={cloning}
            value={form.url}
            onChange={(e) => update("url", e.target.value)}
            spellCheck="false"
            autoComplete="off"
            id={n("model-url")}
          />
        </div>
        <label>
          <span>{doubao || dashscope ? "API Key" : "密钥"}</span>
          <input
            className="text-input"
            type="password"
            disabled={cloning}
            value={form.api_key}
            placeholder={
              doubao && !ownKeySet
                ? "控制台 API Key；旧版控制台填“APP ID:Access Token”"
                : dashscope && !ownKeySet && !peerKeySet
                  ? "千问 / 百炼按量付费 API Key（sk- 开头）"
                  : builtinKeyPlaceholder(entry, peerCloud.sameOrigin, form, section)
            }
            onChange={(e) => update("api_key", e.target.value)}
            autoComplete="new-password"
            spellCheck="false"
            id={n("model-key")}
          />
        </label>
        <div
          className="voice-field"
          ref={comboRef}
          onBlur={(e) => {
            if (e.relatedTarget && !e.currentTarget.contains(e.relatedTarget)) {
              setMenuOpen(false);
              setFiltering(false);
            }
          }}
        >
          <div className="voice-field-head">
            <label htmlFor={n("model-name")}>{doubao ? "资源 ID（留空用默认）" : "模型"}</label>
            {!presetKind && (
              <button
                type="button"
                className="voice-link-btn"
                disabled={remoteModels.loading || sectionBusy}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => {
                  setMenuOpen(true);
                  setFiltering(false);
                  fetchModels({ auto: false });
                }}
                id={n("model-fetch")}
              >
                <RefreshCw size={11} className={remoteModels.loading ? "voice-spin" : ""} />
                {remoteModels.loading
                  ? "读取中…"
                  : allOptions.length > 0 && !usingPresets
                    ? `刷新列表（${onlyMatched && matchedOptions.length < allOptions.length ? `${matchedOptions.length}/${allOptions.length}` : allOptions.length}）`
                    : usingPresets
                      ? "读取全量模型"
                      : "读取模型"}
              </button>
            )}
          </div>
          <div className="voice-combo-wrap">
            <input
              className="text-input mono voice-combo-input"
              disabled={cloning}
              value={form.model}
              placeholder={
                doubao
                  ? "下拉选择或手动填写资源 ID"
                  : dashscope
                    ? "下拉选择或手动填写千问 / 百炼语音模型"
                    : usingPresets
                      ? `已预置 ${allOptions.length} 个常用模型，点击选择或填密钥读取全量`
                      : allOptions.length > 0
                        ? onlyMatched && matchedOptions.length < allOptions.length
                          ? `已筛出 ${matchedOptions.length} 个${current.label}模型（共 ${allOptions.length} 个），点击选择或输入查询`
                          : `已读 ${allOptions.length} 个，输入关键词查询或下拉选择`
                        : "输入关键词筛选，或填好地址密钥读取"
              }
              onFocus={handleModelFocus}
              onChange={handleModelChange}
              onKeyDown={handleModelKeyDown}
              spellCheck="false"
              autoComplete="off"
              id={n("model-name")}
            />
            <button
              type="button"
              className="voice-combo-toggle"
              disabled={cloning}
              aria-label={doubao ? "展开常用资源 ID" : "展开模型列表"}
              aria-expanded={menuOpen}
              onClick={toggleMenu}
              id={n("model-toggle")}
            >
              <ChevronDown size={14} />
            </button>
          </div>
          {menuOpen && (
            <div className="voice-combo-menu" role="listbox" aria-label={`${current.label}可选模型`} id={n("model-options")}>
              <div className="voice-combo-meta">
                {remoteModels.loading ? (
                  <span>正在读取服务商支持的全部模型…</span>
                ) : !presetKind && listErr ? (
                  <span className="voice-combo-err">{listErr}</span>
                ) : !presetKind && remoteModels.ok === false && remoteModels.detail ? (
                  <span className="voice-combo-err">
                    {remoteModels.detail}（{usingPresets ? "下方仍可选择预置模型或手动输入" : "仍可直接手动输入模型名"}）
                  </span>
                ) : usingPresets ? (
                  <span>
                    已列出 {allOptions.length} 个常用预设模型（填好密钥点“读取全量模型”可拉取账号全部可用模型）
                  </span>
                ) : allOptions.length > 0 ? (
                  <>
                    <span>
                      {doubao
                        ? `常用资源 ID（${allOptions.length} 个）`
                        : dashscope
                          ? `支持流式合成与声音克隆的模型（${allOptions.length} 个）`
                          : matchedOptions.length === 0
                            ? onlyMatched
                              ? `该服务商返回的 ${allOptions.length} 个模型均为大模型，无${current.label}模型`
                              : `未发现专用${current.label}模型，已显示全部 ${allOptions.length} 个`
                            : matchedOptions.length < allOptions.length
                              ? onlyMatched
                                ? `已自动筛出 ${matchedOptions.length} 个${current.label}模型（共 ${allOptions.length} 个）`
                                : `全部 ${allOptions.length} 个模型（含 ${matchedOptions.length} 个${current.label}模型）`
                              : `共 ${allOptions.length} 个${current.label}模型`}
                      {filtering && filteredOptions.length !== scopedOptions.length ? ` · 筛出 ${filteredOptions.length} 个` : matchedOptions.length > 0 || !onlyMatched ? " · 输入关键词可查询" : ""}
                    </span>
                    <span className="voice-actions">
                      {!presetKind && matchedOptions.length < allOptions.length && (
                        <button
                          type="button"
                          className="voice-link-btn"
                          onClick={() => setShowAllModels((prev) => !prev)}
                          id={n("model-scope-toggle")}
                        >
                          {showAllModels ? `只看${current.label}（${matchedOptions.length}）` : `显示全部（${allOptions.length}）`}
                        </button>
                      )}
                      {filtering && filteredOptions.length !== scopedOptions.length && (
                        <button type="button" className="voice-link-btn" onClick={() => setFiltering(false)}>
                          清除筛选
                        </button>
                      )}
                    </span>
                  </>
                ) : !presetKind && remoteModels.ok === true ? (
                  <span>{remoteModels.detail || "未返回可选模型，请直接手动输入"}</span>
                ) : (
                  <span>先填好上方地址与密钥，点“读取模型”即可拉取全部模型</span>
                )}
              </div>
              {filteredOptions.length > 0 ? (
                <div className="voice-combo-list">
                  {filteredOptions.map((option, idx) => (
                    <button
                      type="button"
                      role="option"
                      aria-selected={option.id === form.model}
                      key={option.id}
                      className={`voice-combo-option mono${option.id === form.model ? " is-selected" : ""}${idx === activeIndex ? " is-active" : ""}`}
                      onClick={() => pickModel(option.id)}
                    >
                      <span className="voice-combo-id">{option.id}</span>
                      {(option.displayHint || option.hint) && <span className="voice-combo-hint">{option.displayHint || option.hint}</span>}
                    </button>
                  ))}
                </div>
              ) : (
                allOptions.length > 0 && !remoteModels.loading && (
                  <div className="voice-combo-empty">
                    {onlyMatched && matchedOptions.length === 0
                      ? `该服务商没有${current.label}模型，建议在上方顶部下拉框切换为「本地${current.label}」，或点右上角“显示全部”手动选择`
                      : `没有包含“${form.model}”的模型，可直接使用当前手动输入的名称`}
                  </div>
                )
              )}
            </div>
          )}
        </div>
        {section === "tts" && (
          <>
            <div
              className="voice-field"
              ref={voiceComboRef}
              onBlur={(e) => {
                if (e.relatedTarget && !e.currentTarget.contains(e.relatedTarget)) {
                  setVoiceMenuOpen(false);
                }
              }}
            >
              <div className="voice-field-head">
                <label htmlFor={n("model-voice")}>
                  {doubao
                    ? "音色 ID（可下拉选常用或手填）"
                    : dashscope
                      ? dashscopeCloneOnly
                        ? "克隆音色 ID"
                        : canCloneVoice
                          ? "音色 ID（系统音色或克隆音色）"
                          : "系统音色"
                      : "音色（可下拉选常用或手填）"}
                </label>
                {canCloneVoice && (
                  <>
                    <input
                      ref={cloneFileRef}
                      type="file"
                      accept=".wav,.mp3,.m4a,audio/wav,audio/mpeg,audio/mp4"
                      hidden
                      onChange={handleCloneFilePick}
                      id={n("voice-clone-input")}
                    />
                    <button
                      type="button"
                      className="voice-link-btn"
                      disabled={sectionBusy}
                      onClick={() => cloneFileRef.current?.click()}
                      title="选择一段 5～20 秒清晰无伴奏的单人说话录音（WAV / MP3 / M4A，自有或获授权音频），自动克隆并生成专属音色 ID"
                      id={n("voice-clone-btn")}
                    >
                      <Upload size={11} className={localBusy === "builtin-clone-tts" ? "voice-spin" : ""} />
                      {localBusy === "builtin-clone-tts" ? "正在克隆音色…" : "上传录音克隆音色"}
                    </button>
                  </>
                )}
              </div>
              <div className="voice-combo-wrap">
                <input
                  className="text-input mono voice-combo-input"
                  disabled={cloning}
                  value={form.voice}
                  placeholder={
                    dashscope && voiceOptions.length === 0
                      ? canCloneVoice
                        ? "点右上角“上传录音克隆音色”生成专属音色 ID"
                        : "填写系统音色名称"
                      : voiceOptions[0]
                        ? `如 ${voiceOptions[0].id}`
                        : "填写或下拉选择音色"
                  }
                  onFocus={() => {
                    if (!hideEmptyVoiceToggle) setVoiceMenuOpen(true);
                    if (canCloneVoice && clonedVoices.key !== currentVoicesKey) {
                      loadClonedVoices(form);
                    }
                  }}
                  onChange={(e) => update("voice", e.target.value)}
                  spellCheck="false"
                  autoComplete="off"
                  id={n("model-voice")}
                />
                {!hideEmptyVoiceToggle && (
                  <button
                    type="button"
                    className="voice-combo-toggle"
                    disabled={cloning}
                    aria-label="展开常用音色列表"
                    aria-expanded={voiceMenuOpen}
                    onClick={() => {
                      const nextOpen = !voiceMenuOpen;
                      setVoiceMenuOpen(nextOpen);
                      if (nextOpen && canCloneVoice) {
                        loadClonedVoices(form);
                      }
                    }}
                    id={n("voice-toggle")}
                  >
                    <ChevronDown size={14} />
                  </button>
                )}
              </div>
              {voiceMenuOpen && (voiceOptions.length > 0 || (dashscope && (clonedVoices.loading || Boolean(clonedVoices.error)))) && (
                <div className="voice-combo-menu" role="listbox" aria-label="语音合成常用音色" id={n("voice-options")}>
                  <div className="voice-combo-meta">
                    {dashscope && clonedVoices.loading ? (
                      <span>正在读取账号已克隆音色…</span>
                    ) : dashscope && clonedVoices.error ? (
                      <span className="voice-combo-err">{clonedVoices.error}</span>
                    ) : (
                      <span>
                        {dashscope && clonedVoiceOptions.length > 0
                          ? `已含账号下 ${clonedVoiceOptions.length} 个克隆音色（共 ${voiceOptions.length} 个可选，点选自动匹配绑定模型）`
                          : `推荐常用音色（共 ${voiceOptions.length} 个，也可直接手动输入）`}
                      </span>
                    )}
                  </div>
                  {voiceOptions.length > 0 && (
                    <div className="voice-combo-list">
                      {voiceOptions.map((opt) => (
                        <button
                          type="button"
                          role="option"
                          aria-selected={opt.id === form.voice}
                          key={opt.id}
                          className={`voice-combo-option mono${opt.id === form.voice ? " is-selected" : ""}`}
                          onClick={() => {
                            setForm((all) => ({
                              ...all,
                              voice: opt.id,
                              ...(opt.targetModel ? { model: opt.targetModel } : {}),
                            }));
                            setResult(null);
                            setVoiceMenuOpen(false);
                          }}
                        >
                          <span className="voice-combo-id">{opt.id}</span>
                          {opt.hint && <span className="voice-combo-hint">{opt.hint}</span>}
                        </button>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </div>
            <label>
              <span>{doubao || dashscope ? "语速（0.5 ~ 2.0）" : "语速（0.25 ~ 4.0）"}</span>
              <input className="text-input" disabled={cloning} inputMode="decimal" value={form.speed} onChange={(e) => update("speed", e.target.value)} />
            </label>
          </>
        )}
      </div>
      <div className="voice-final mono">
        {entry?.configured && !dirty ? "已保存接口地址：" : "将请求的接口地址："}
        {gatewayAudioErr ? `—（当前服务商不支持${current.label}接口）` : finalUrl(section, form.url, form.kind) || "—"}
      </div>
      {gatewayAudioErr && (
        <div className="voice-hint voice-hint--bad" id={n("gateway-audio-error")}>
          {gatewayAudioErr}
        </div>
      )}
      {alibabaTtsHint && !gatewayAudioErr && (
        <div className="voice-hint voice-hint--warn" id={n("alibaba-tts-hint")}>
          <span>{alibabaTtsHint.message}</span>
          <button
            type="button"
            className="btn btn-ghost voice-inline-btn"
            onClick={applyDashscopeKind}
            disabled={sectionBusy}
            id={n("use-dashscope")}
          >
            {alibabaTtsHint.actionLabel}
          </button>
        </div>
      )}
      {endpointMismatch && (
        <div className="voice-hint voice-hint--warn" id={n("endpoint-mismatch")}>
          <span>{endpointMismatch.message}</span>
          <button
            type="button"
            className="btn btn-ghost voice-inline-btn"
            onClick={applySuggestedEndpoint}
            disabled={sectionBusy}
            id={n("fix-endpoint")}
          >
            {endpointMismatch.actionLabel}
          </button>
        </div>
      )}
      {v1Suggestion && !endpointMismatch && (
        <div className="voice-hint voice-hint--warn" id={n("v1-suggestion")}>
          <span>这个服务的接口在 /v1 下：只填域名时模型列表读得到，但对话和测试会失败。</span>
          <button
            type="button"
            className="btn btn-ghost voice-inline-btn"
            onClick={applyV1Suggestion}
            disabled={sectionBusy}
            id={n("use-v1")}
          >
            改用 {v1Suggestion}
          </button>
        </div>
      )}
      {!menuOpen && !endpointMismatch && !test && inlineListError && (
        <div className="voice-hint voice-hint--bad" id={n("models-list-error")}>
          读取模型列表未通过：{inlineListError}（也可直接手动输入模型名后点“测试连接”）
        </div>
      )}
      {test ? (
        <div className={`voice-hint voice-hint--${test.ok ? "ok" : "bad"}`} id={n("model-result")}>
          <span>
            {test.ok ? "测试通过" : "测试未通过"}
            {typeof test.elapsed_ms === "number" ? ` · ${(test.elapsed_ms / 1000).toFixed(1)} s` : ""} · {test.detail}
            {test.ok && test.fingerprint !== sectionFingerprint(section, form) && "（测试后内容已修改，请重新测试）"}
          </span>
          {section === "tts" && test.ok && test.audio_data_url && (
            <button
              type="button"
              className="btn btn-ghost voice-inline-btn"
              onClick={toggleAudioPreview}
              id={n("model-play")}
            >
              {playingAudio ? <Square size={12} /> : <Volume2 size={13} />}
              {playingAudio ? "停止试听" : "试听语音"}
            </button>
          )}
        </div>
      ) : (
        <p className="voice-muted">先测试连接，通过后才能保存。密钥留空时测试用已保存的密钥。</p>
      )}
      <SaveBar
        id={n("model-save")}
        target={`${current.label}设置`}
        dirty={dirty}
        busy={sectionBusy}
        saving={localBusy === `builtin-save-${section}`}
        blocked={saveEnabled ? "" : "先测试连接，通过后才能保存"}
        result={result}
        onSave={save}
        extra={
          <>
            <button type="button" className="btn btn-ghost" onClick={runTest} disabled={sectionBusy || isSectionBlank(form)} id={n("model-test")}>
              <FlaskConical size={13} /> {localBusy === `builtin-test-${section}` ? "测试中…" : "测试连接"}
            </button>
            {entry?.configured && (
              <button type="button" className="btn btn-ghost" onClick={clear} disabled={sectionBusy} id={n("model-clear")}>
                <Trash2 size={13} /> 删除此项
              </button>
            )}
          </>
        }
      />
    </VoiceGroup>
  );
}

export { BuiltinModelGroup };
