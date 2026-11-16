import React, { useEffect, useMemo, useRef, useState } from "react";
import { Excalidraw, exportToBlob } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import {
  Download,
  FileJson,
  Home,
  Upload,
} from "lucide-react";
import { prepareRasterExportElements } from "../rasterInkElements.js";
import {
  summaryAllowsConfirmSend,
} from "../jobSummaryView.js";
import {
  CUSTOM_OPTION_VALUE,
  MATERIAL_LIBRARY_CHANNEL,
  applyMaterialLibraryRefresh,
  applyMaterialSelectChange,
  loadUiMaterialOptions,
  notifyMaterialsUpdated,
  restoreMaterialSelectState,
  returnToLibrary,
} from "../materialSelectState.js";
import { getLaserExportDimensions } from "../laserExportDimensions.js";
import {
  applyConfirmSendResponse,
  applyPreviewFingerprintSuccess,
  canSubmitConfirmSend,
  createRepeatSendState,
  requiresMovedMaterialAcknowledgement,
  setMovedMaterialAcknowledged,
} from "../repeatSendGuard.js";
import { apiFetch, getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { exportFile, savedMessage } from "../saveFile.js";
import { estopTargetArgs } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import { TERMINAL_STATUSES } from "../state/statusNormalize.js";
import { manualParamsPayload } from "../workbenchParams.js";
import { discoveryOutcome } from "../controllerDiscovery.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import {
  adoptRegisteredDrawRecord,
  drawFramingExtent,
  drawOriginNote,
} from "../registrationState.js";
import { framingOutline, readFramingShape } from "../aiPreviewState.js";
import {
  applyMaterialLayer,
  fillResultNote,
  fillSettingsPayload,
  fillSettingsSignature,
  localSettingsToStore,
  machineMotionFromLimits,
  machineMotionPayload,
  materialLayerFromRecommendation,
  materialLayerPayload,
  materialLayerSaveRequest,
  readFillSettings,
  readMachineMotion,
  timeEstimateNote,
  writeFillSettings,
  writeMachineMotion,
} from "../fillOptions.js";
import { WbCard, WorkbenchCanvas } from "../components/WorkbenchCanvas.jsx";
import { DrawMaterialCard } from "../components/draw/DrawMaterialCard.jsx";
import { DrawDeviceCard } from "../components/draw/DrawDeviceCard.jsx";
import { DrawSendCard } from "../components/draw/DrawSendCard.jsx";
import { SendConfirmDialog } from "../components/draw/SendConfirmDialog.jsx";
import { DRAW_DEFAULT_LAYOUT, DRAW_STORAGE_KEY } from "../workbenchLayout.js";
import {
  DRAW_MODE_VERSION,
  buildDrawFramingPayload,
  selectedElementIds,
  computeSceneRevision,
  drawConnectionPayload,
  drawGenerationSignature,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  formatFramingStatus,
  framingConfirmText,
  isDrawPreviewStale,
  isSerial,
  parseImportedSceneJson,
  readDrawDefaultForm,
  safeBrowserStorage,
} from "../drawConnection.js";

const CONFIG_KEYS = [
  "material",
  "thickness_mm",
  "width_mm",
  "height_mm",
  "mode",
  "connection_mode",
  "network_host",
  "network_telnet_port",
  "port",
  "baudrate",
  "vector_simplify_factor",
];
const EXCALIDRAW_LANG_CODE = "zh-CN";
const MATERIAL_CHANNEL_NAME = MATERIAL_LIBRARY_CHANNEL;

export function DrawPage({ onNavigate }) {
  const [excalidrawAPI, setExcalidrawAPI] = useState(null);
  // 挂载时惰性读取（存储被禁用时退回默认值，若画板未填过设备则自动复用雕刻工作台已填好的连接目标）
  const [form, setForm] = useState(() => readDrawDefaultForm());
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("准备就绪");
  const [workflowId, setWorkflowId] = useState("");
  const [result, setResult] = useState(null);
  const [lastScene, setLastScene] = useState(null);
  const [sceneRev, setSceneRev] = useState("empty");
  const [previewSnap, setPreviewSnap] = useState(null);
  const [connectionBusy, setConnectionBusy] = useState(false);
  const [connectionResult, setConnectionResult] = useState(null);
  const [discovering, setDiscovering] = useState(false);
  const [discoveredChoices, setDiscoveredChoices] = useState([]);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [materialLibrary, setMaterialLibrary] = useState([]);
  const [materialMode, setMaterialMode] = useState("custom");
  const [materialSelectValue, setMaterialSelectValue] = useState(CUSTOM_OPTION_VALUE);
  const [materialRefreshBusy, setMaterialRefreshBusy] = useState(false);
  const [recPowerPct, setRecPowerPct] = useState(40);
  const [recFeedMpm, setRecFeedMpm] = useState(1500);
  const [recPasses, setRecPasses] = useState(1);
  const [hasRec, setHasRec] = useState(true);
  const [recRefreshTick, setRecRefreshTick] = useState(0);
  const [customParams, setCustomParams] = useState(false);
  // 2026-11-16：矢量模式的填充与走刀设置（对齐 LightBurn），与工作台文字共用一份本机设置
  const [fillSettings, setFillSettings] = useState(() => readFillSettings());
  // r40：当前材料条目随材料保存的填充设置（矢量模式用轮廓条目），可改，“保存到材料库”写回
  const [materialLayer, setMaterialLayer] = useState(null);
  const [materialApplied, setMaterialApplied] = useState([]);
  const materialLayerKeyRef = useRef("");
  const updateFillSettings = (next) => {
    setFillSettings(next);
    writeFillSettings(localSettingsToStore(next, readFillSettings(), materialApplied));
  };
  // 握手读到的机器加速度 / 最大速度（与工作台共用本机记录）：工时估算与自动过扫描用
  const [machineMotion, setMachineMotion] = useState(() => readMachineMotion());
  const motionPayload = machineMotionPayload(fillSettings, machineMotion);
  const [customPower, setCustomPower] = useState("40");
  const [customFeed, setCustomFeed] = useState("1500");
  const [customPasses, setCustomPasses] = useState("1");
  const [repeatSend, setRepeatSend] = useState(() => createRepeatSendState(safeBrowserStorage()));
  const [serialPorts, setSerialPorts] = useState([]);
  const [framingLight, setFramingLight] = useState(false);
  // 走边框走法：外接矩形 / 沿外轮廓（橡皮筋），与工作台共用
  const [framingShape, setFramingShape] = useState(() => readFramingShape());
  // 只刻选中的图形（矢量模式）：选区变化计入预览签名
  const [selectedOnly, setSelectedOnly] = useState(false);
  const [selectionKey, setSelectionKey] = useState("");
  const materialLibraryRef = useRef(materialLibrary);
  const materialModeRef = useRef(materialMode);
  const formRef = useRef(form);
  const persistedRef = useRef({});
  const pollingCleanupRef = useRef(null);
  const importInputRef = useRef(null);
  const materialLoadedOnceRef = useRef(false);
  const materialRefreshGenerationRef = useRef(0);
  const boardStageRef = useRef(null);

  useEffect(() => () => pollingCleanupRef.current?.(), []);

  useEffect(() => {
    const stage = boardStageRef.current;
    if (!stage || !excalidrawAPI || typeof ResizeObserver === "undefined") return undefined;
    const observer = new ResizeObserver(() => {
      excalidrawAPI.refresh?.();
    });
    observer.observe(stage);
    return () => observer.disconnect();
  }, [excalidrawAPI]);

  useEffect(() => {
    materialLibraryRef.current = materialLibrary;
  }, [materialLibrary]);
  useEffect(() => {
    materialModeRef.current = materialMode;
  }, [materialMode]);
  useEffect(() => {
    formRef.current = form;
  }, [form]);

  // 自动按需持久化修改过的配置键（切页面不再丢失填写；存储受限时静默退回当前会话）
  useEffect(() => {
    const storage = safeBrowserStorage();
    if (!storage) return;
    for (const key of CONFIG_KEYS) {
      const raw = form[key];
      const val = typeof raw === "boolean" ? (raw ? "true" : "false") : String(raw ?? "");
      if (persistedRef.current[key] !== val) {
        persistedRef.current[key] = val;
        try {
          storage.setItem(`laserLab.${key}`, val);
        } catch {
          // 存储受限时保留内存态
        }
      }
    }
  }, [form]);

  // 联动查询当前材质、厚度与生成策略对应的材料库推荐功率、速度与遍数
  useEffect(() => {
    let cancelled = false;
    const mat = String(form.material || "").trim();
    const thick = String(form.thickness_mm || "").trim();
    if (!mat || !thick || !(Number(thick) > 0)) {
      setHasRec(false);
      return undefined;
    }
    const query = new URLSearchParams({
      material: mat,
      thickness_mm: thick,
      laser_mode: "engrave",
      engraving_mode: form.mode === "outline" || form.mode === "vector" ? "outline" : "raster",
    });
    getJson(`/api/ui/material-recommendation?${query.toString()}`).then((res) => {
      if (cancelled) return;
      if (!res.ok || !res.data?.success || !res.data.result) {
        setHasRec(false);
        setMaterialLayer(null);
        if (materialLayerKeyRef.current) {
          materialLayerKeyRef.current = "";
          setFillSettings(readFillSettings());
          setMaterialApplied([]);
        }
        return;
      }
      const rec = res.data.result;
      if (rec.laser_max_power != null) setRecPowerPct(Math.round(rec.laser_max_power / 10));
      if (rec.feed_rate != null) setRecFeedMpm(Math.round(rec.feed_rate));
      setRecPasses(Math.max(1, Math.round(rec.passes || 1)));
      setHasRec(true);
      const layer = materialLayerFromRecommendation(rec);
      setMaterialLayer(layer);
      if (layer && layer.key !== materialLayerKeyRef.current) {
        materialLayerKeyRef.current = layer.key;
        const next = applyMaterialLayer(readFillSettings(), layer);
        setFillSettings(next.settings);
        setMaterialApplied(next.applied);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [form.material, form.thickness_mm, form.mode, recRefreshTick]);

  // r40：把当前填充设置存进材料库条目（只改图层设置；扫描偏移与机器加速度不随材料保存）
  const saveFillToMaterial = async () => {
    if (!materialLayer) return;
    const payload = materialLayerPayload(fillSettings, { withFill: true });
    if (payload.error) {
      showAlert(payload.error, { title: "填充设置不对", tone: "warning" });
      return;
    }
    const ok = await showConfirm(
      `把当前填充设置存进材料库「${materialLayer.label}」？\n以后选这个材料会自动带出；双向扫描偏移与机器加速度跟机器走，不随材料保存。`,
      { title: "保存到材料库", confirmText: "保存", cancelText: "取消" },
    );
    if (!ok) return;
    const res = await postJson("/api/material-lab/manage", materialLayerSaveRequest(materialLayer, payload));
    if (res.ok && res.data?.success) {
      setStatus(`已把填充设置存进材料库「${materialLayer.label}」`);
      notifyMaterialsUpdated();
      materialLayerKeyRef.current = "";
      setRecRefreshTick((tick) => tick + 1);
    } else {
      showAlert(res.data?.result || res.error || "保存失败", { title: "保存到材料库失败", tone: "error" });
    }
  };

  const currentFormSig = drawGenerationSignature(form, {
    enabled: customParams,
    power: customPower,
    feed: customFeed,
    passes: customPasses,
    presetParams: hasRec ? `${recPowerPct}/${recFeedMpm}/${recPasses}` : "",
    fill: fillSettingsSignature(fillSettings) + JSON.stringify(motionPayload)
      + (selectedOnly && form.mode === "vector" ? `|sel:${selectionKey}` : ""),
  });
  const previewStale = Boolean(workflowId) && isDrawPreviewStale(previewSnap, {
    sceneRev,
    formSig: currentFormSig,
  });

  const web = result?.result?.web || {};
  const lab = result?.result?.draw_lab || {};
  const jobSummary = lab.job_summary || null;
  const rawText = useMemo(() => (result ? JSON.stringify(result, null, 2) : ""), [result]);
  const canActOnWorkflow = Boolean(workflowId);
  const hostReady = drawTargetReady(form);
  const nextActions = Array.isArray(result?.result?.next_actions) ? result.result.next_actions : [];
  const summaryAllowsSend = summaryAllowsConfirmSend(jobSummary, nextActions);
  const canConfirmSend = canActOnWorkflow && !previewStale && hostReady && summaryAllowsSend;

  function updateForm(key, value) {
    if (
      key === "connection_mode" ||
      key === "port" ||
      key === "baudrate" ||
      key === "network_host" ||
      key === "network_telnet_port"
    ) {
      setConnectionResult(null);
    }
    setForm((current) => ({ ...current, [key]: value }));
  }

  async function refreshMaterialLibrary() {
    const generation = ++materialRefreshGenerationRef.current;
    setMaterialRefreshBusy(true);
    try {
      const currentForm = formRef.current;
      const loaded = await loadUiMaterialOptions(apiFetch);
      if (generation !== materialRefreshGenerationRef.current) return;
      const decision = applyMaterialLibraryRefresh({
        loaded,
        currentMode: materialModeRef.current,
        currentMaterial: currentForm.material,
        currentThickness: currentForm.thickness_mm,
        previousMaterials: materialLibraryRef.current,
        hasLoadedOnce: materialLoadedOnceRef.current,
      });
      if (!decision.applied) {
        return;
      }
      if (loaded.ok) {
        materialLoadedOnceRef.current = true;
      }
      setMaterialLibrary(decision.materials);
      setMaterialMode(decision.mode);
      setMaterialSelectValue(decision.selectValue);
      const nextForm = {
        ...currentForm,
        material: decision.material || currentForm.material,
        thickness_mm: decision.thickness_mm || currentForm.thickness_mm,
      };
      setForm((current) => ({
        ...current,
        material: nextForm.material,
        thickness_mm: nextForm.thickness_mm,
      }));
      setRecRefreshTick((n) => n + 1);
    } finally {
      if (generation === materialRefreshGenerationRef.current) {
        setMaterialRefreshBusy(false);
      }
    }
  }

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const generation = ++materialRefreshGenerationRef.current;
      const loaded = await loadUiMaterialOptions(apiFetch);
      if (cancelled || generation !== materialRefreshGenerationRef.current) return;
      const restored = restoreMaterialSelectState({
        materials: loaded.materials,
        savedMaterial: form.material,
        savedThickness: form.thickness_mm,
      });
      if (loaded.ok) {
        materialLoadedOnceRef.current = true;
      }
      setMaterialLibrary(restored.materials);
      setMaterialMode(restored.mode);
      setMaterialSelectValue(restored.selectValue);
      const nextForm = {
        ...form,
        material: restored.material || form.material,
        thickness_mm: restored.thickness_mm || form.thickness_mm,
      };
      setForm((current) => ({
        ...current,
        material: nextForm.material,
        thickness_mm: nextForm.thickness_mm,
      }));
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    function onFocus() {
      refreshMaterialLibrary();
    }
    function onVisibility() {
      if (document.visibilityState === "visible") {
        refreshMaterialLibrary();
      }
    }
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    let channel = null;
    try {
      if (typeof BroadcastChannel === "function") {
        channel = new BroadcastChannel(MATERIAL_CHANNEL_NAME);
        channel.addEventListener("message", (event) => {
          if (event && event.data && event.data.type === "materials-updated") {
            refreshMaterialLibrary();
          }
        });
      }
    } catch (_error) {
      channel = null;
    }
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
      if (channel) {
        try {
          channel.close();
        } catch (_error) {
          // ignore close failures
        }
      }
    };
  }, []);

  function handleMaterialSelectChange(selectValue) {
    const next = applyMaterialSelectChange({
      materials: materialLibrary,
      currentThickness: form.thickness_mm,
      selectValue,
      customMaterial: form.material,
    });
    setMaterialLibrary(next.materials);
    setMaterialMode(next.mode);
    setMaterialSelectValue(next.selectValue);
    setForm((current) => ({
      ...current,
      material: next.material,
      thickness_mm: next.thickness_mm,
    }));
  }

  function handleThicknessSelectChange(value) {
    updateForm("thickness_mm", value);
  }

  function handleReturnToLibrary() {
    const next = returnToLibrary({
      materials: materialLibrary,
      preferredMaterial: form.material,
      preferredThickness: form.thickness_mm,
    });
    setMaterialLibrary(next.materials);
    setMaterialMode(next.mode);
    setMaterialSelectValue(next.selectValue);
    setForm((current) => ({
      ...current,
      material: next.material,
      thickness_mm: next.thickness_mm,
    }));
  }

  function saveConfiguration() {
    try {
      const storage = safeBrowserStorage();
      if (!storage) throw new Error("浏览器存储不可用");
      CONFIG_KEYS.forEach((key) => {
        const value = form[key];
        const str = typeof value === "boolean" ? (value ? "true" : "false") : String(value ?? "");
        persistedRef.current[key] = str;
        storage.setItem(`laserLab.${key}`, str);
      });
      storage.setItem("laserLab.mode_version", DRAW_MODE_VERSION);
      setStatus("配置已保存");
    } catch (error) {
      setStatus(`配置保存失败：${error?.message || error}（本次设置仍可使用，但不会被记住）`);
    }
  }

  function pickDiscoveredController(controller) {
    setConnectionResult(null);
    setForm((current) => ({
      ...current,
      network_host: controller.ip,
      network_telnet_port: String(controller.telnet_port || 23),
    }));
  }

  async function discoverControllers() {
    if (discovering || busy) return;
    setDiscovering(true);
    setDiscoveredChoices([]);
    setStatus("正在查找局域网里的雕刻机，并逐台发送只读查询确认（约 1～5 秒）...");
    try {
      const res = await postJson("/api/ui/discover-controllers", {});
      if (!res.ok || !res.data?.success) {
        setStatus(`扫描失败：${res.data?.result || res.error || "后端无响应"}`);
        return;
      }
      const outcome = discoveryOutcome(res.data.result);
      if (outcome.fill) pickDiscoveredController(outcome.fill);
      setDiscoveredChoices(outcome.choices);
      setStatus(outcome.log.text);
    } catch (error) {
      setStatus(`扫描失败：${error.message || error}`);
    } finally {
      setDiscovering(false);
    }
  }

  async function handleImportSceneFile(event) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file || !excalidrawAPI) return;
    try {
      const text = await file.text();
      const parsed = parseImportedSceneJson(text);
      if (parsed.error) {
        setStatus(`导入场景失败：${parsed.error}`);
        void showAlert(parsed.error, { title: "无法导入场景", tone: "warning" });
        return;
      }
      if (parsed.files) {
        const fileEntries = Object.values(parsed.files).filter((item) => item && typeof item === "object");
        if (fileEntries.length > 0) excalidrawAPI.addFiles?.(fileEntries);
      }
      excalidrawAPI.updateScene({
        elements: parsed.elements,
        appState: parsed.appState,
      });
      setTimeout(() => excalidrawAPI.scrollToContent?.(parsed.elements, { fitToContent: true }), 40);
      setSceneRev(computeSceneRevision(parsed.elements));
      setStatus(`已导入场景：${file.name}`);
    } catch (error) {
      const msg = error?.message || String(error);
      setStatus(`导入场景失败：${msg}`);
      void showAlert(msg, { title: "导入失败", tone: "error" });
    }
  }

  function startDrawJobPolling(targetWorkflowId) {
    pollingCleanupRef.current?.();
    let pollState = { id: targetWorkflowId, kind: "workflow", status: "sending", progress: null };
    const stop = startJobPolling(
      (signal) => postJson("/api/workflow", { action: "status", workflow_id: targetWorkflowId }, { signal }),
      (res) => {
        pollState = applyJobPoll(pollState, targetWorkflowId, "workflow", res);
        if (pollState.connectionLost) {
          setStatus("状态未知：无法查询设备，机器可能仍在运行（请到设备旁确认，顶栏急停随时可用）");
          return;
        }
        if (res?.ok && res.data) {
          setResult(res.data);
          const sr = res.data.result?.send_result || {};
          if (pollState.progress && pollState.progress.total > 0) {
            const pct = Math.min(100, Math.round((pollState.progress.sent / pollState.progress.total) * 100));
            setStatus(
              sr.phase === "awaiting_completion"
                ? `刀路已传输 100%，等待机器执行停稳…`
                : sr.mode === "sd_offline"
                  ? pct === 0
                    ? "正在上传并校验 SD 脱机文件哈希…"
                    : `正在 SD 脱机加工：${pct}%（${pollState.progress.sent} / ${pollState.progress.total} 行）`
                  : `正在雕刻下发：${pct}%（${pollState.progress.sent} / ${pollState.progress.total} 行）`,
            );
          }
        }
        if (TERMINAL_STATUSES.has(pollState.status)) {
          stop();
          pollingCleanupRef.current = null;
          if (pollState.status === "completed") {
            setStatus("雕刻已完成（控制器空闲状态已核验）");
          } else if (pollState.status === "cancelled") {
            setStatus("任务已取消，请到设备旁确认实际停稳");
          } else {
            const err = res?.data?.result?.send_result?.error || "请检查设备状态";
            setStatus(`加工未完成：${err}`);
          }
        }
      },
    );
    pollingCleanupRef.current = stop;
  }

  async function exportScene({ forLaser = false } = {}) {
    if (!excalidrawAPI) {
      throw new Error("画板还没有加载完成");
    }
    const elements = excalidrawAPI.getSceneElements();
    const activeElements = elements.filter((element) => !element.isDeleted);
    if (activeElements.length === 0) {
      throw new Error("画板是空的");
    }
    const appState = excalidrawAPI.getAppState();
    const files = excalidrawAPI.getFiles();
    const exportElements = forLaser ? prepareRasterExportElements(activeElements, form.mode) : activeElements;
    const exportAppState = {
      ...appState,
      exportBackground: true,
      exportWithDarkMode: false,
      viewBackgroundColor: "#ffffff",
    };
    const blob = await exportToBlob({
      elements: exportElements,
      appState: exportAppState,
      files,
      mimeType: "image/png",
      quality: 1,
      exportPadding: 16,
      ...(forLaser ? { getDimensions: getLaserExportDimensions } : {}),
    });
    const scene = {
      type: "excalidraw",
      version: 2,
      source: "excalidraw-laser-lab",
      elements,
      appState: {
        gridSize: appState.gridSize,
        name: appState.name,
        viewBackgroundColor: appState.viewBackgroundColor,
      },
      files,
    };
    setLastScene(scene);
    return { blob, scene, activeElements };
  }

  async function generatePreview() {
    if (busy) {
      setStatus("正在导出并解算画板预览，请勿重复点击...");
      return;
    }
    if (workflowId && !previewStale) {
      const ok = await showConfirm("当前画布和参数没有变化且已生成过预览，确认要重新生成吗？", {
        title: "重新生成确认",
        confirmText: "确认重新生成",
        cancelText: "保留当前预览",
      });
      if (!ok) return;
    }
    const manual = manualParamsPayload({
      enabled: customParams,
      powerPercent: customPower,
      feedRate: customFeed,
      passes: customPasses,
    });
    if (manual?.error) {
      setStatus(`自定义参数有误：${manual.error}`);
      void showAlert(manual.error, { title: "自定义参数不对", tone: "warning" });
      return;
    }

    const formSigSnapshot = currentFormSig;
    const fillPayload = form.mode === "vector" ? fillSettingsPayload(fillSettings) : {};
    const selection = selectedOnly && form.mode === "vector"
      ? selectedElementIds(excalidrawAPI?.getAppState?.())
      : [];
    if (selectedOnly && form.mode === "vector" && selection.length === 0) {
      setStatus("请先在画布上选中要刻的图形，或取消“只刻选中的图形”");
      void showAlert("请先在画布上选中要刻的图形（可框选或按住 Shift 多选），或取消勾选“只刻选中的图形”。", {
        title: "没有选中图形",
        tone: "warning",
      });
      return;
    }
    if (fillPayload.error) {
      setStatus(`填充设置有误：${fillPayload.error}`);
      void showAlert(fillPayload.error, { title: "填充设置不对", tone: "warning" });
      return;
    }
    setBusy(true);
    setStatus("导出画板中...");
    try {
      const { blob, scene, activeElements } = await exportScene({ forLaser: true });
      const revSnapshot = computeSceneRevision(activeElements);
      setSceneRev(revSnapshot);
      const imageDataUrl = await blobToDataUrl(blob);
      setStatus("上传并生成预览中...");
      const payload = {
        image_data_url: imageDataUrl,
        scene_json: scene,
        material: form.material,
        thickness_mm: Number(form.thickness_mm) || 3.0,
        width_mm: drawSizeValue(form.width_mm),
        height_mm: drawSizeValue(form.height_mm),
        mode: form.mode,
        ...fillPayload,
        ...motionPayload,
        ...(selection.length ? { selected_element_ids: selection } : {}),
        ...drawConnectionPayload(form),
        ...(manual ? { manual_params: manual } : {}),
      };
      if (form.mode === "outline") {
        const simplifyRaw = String(form.vector_simplify_factor ?? "").trim();
        payload.vector_simplify_factor = simplifyRaw === "" ? 3 : Number(simplifyRaw);
      }
      const res = await postJson("/api/draw/preview", payload);
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      pollingCleanupRef.current?.();
      pollingCleanupRef.current = null;
      setResult(data);
      const nextWorkflowId = res.ok && data.success
        ? data?.result?.workflow_id || data?.result?.workflow?.workflow_id || ""
        : "";
      setWorkflowId(nextWorkflowId);
      setConfirmOpen(false);
      if (res.ok && data.success) {
        setPreviewSnap({ sceneRev: revSnapshot, formSig: formSigSnapshot });
        const fingerprint = data?.result?.draw_lab?.content_fingerprint || "";
        setRepeatSend((current) => applyPreviewFingerprintSuccess(current, fingerprint));
        // 未填尺寸且超出机器幅面时后端会等比缩小，并说明缩到多大（2026-11-06）
        const artifacts = data.result?.artifacts || {};
        setStatus([
          data.result?.speech || "预览已生成",
          artifacts.fit_note,
          artifacts.vector_note,
          artifacts.selection_note,
          fillResultNote(artifacts.fill_options, artifacts.removed_duplicates),
          timeEstimateNote(artifacts.time_estimate),
        ].filter(Boolean).join("；"));
      } else {
        setPreviewSnap(null);
        setStatus((typeof data.result === "string" && data.result) || "预览失败");
      }
    } catch (error) {
      setStatus(`失败：${error.message || error}`);
      setResult({ success: false, result: String(error.message || error) });
    } finally {
      setBusy(false);
    }
  }

  function openSendConfirmation() {
    if (!workflowId) return;
    if (previewStale) {
      setStatus("画布或参数已改动，请先重新生成预览后再发送");
      void showAlert("画布图形或左侧参数已改动，当前预览仍是上一版旧图；请先点击「重新生成预览」。", {
        title: "预览已过期",
        tone: "warning",
      });
      return;
    }
    if (!hostReady) {
      setStatus(drawTargetMissingText(form));
      return;
    }
    if (!summaryAllowsSend) {
      setStatus("当前预览不可发送：材料参数未验证或服务端未提供 confirm_send");
      return;
    }
    if (!canSubmitConfirmSend(repeatSend)) {
      setStatus("相同图案再次发送前，请勾选“已移动材料或原点”");
    }
    setConfirmOpen(true);
  }

  async function confirmSend(offlineSd = false) {
    if (!workflowId || !hostReady) {
      setStatus(workflowId ? drawTargetMissingText(form) : "请先生成预览");
      return;
    }
    if (previewStale) {
      setConfirmOpen(false);
      setStatus("画布或参数已改动，请先重新生成预览后再发送");
      return;
    }
    if (!summaryAllowsSend) {
      setStatus("当前预览不可发送：材料参数未验证或服务端未提供 confirm_send");
      return;
    }
    if (!canSubmitConfirmSend(repeatSend)) {
      setStatus("相同图案再次发送前，请勾选“已移动材料或原点”");
      return;
    }
    setBusy(true);
    setStatus(offlineSd ? "提交脱机离线加工..." : "提交确认发送...");
    try {
      const res = await postJson("/api/workflow", {
        action: "confirm_send",
        workflow_id: workflowId,
        confirmed: true,
        ...drawConnectionPayload(form),
        offline_sd: Boolean(offlineSd) && !isSerial(form),
        run_in_background: true,
        wait_for_response: true,
      });
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      setResult(data);
      setConfirmOpen(false);
      setRepeatSend((current) =>
        applyConfirmSendResponse(current, res.ok ? data : { ...data, success: false }, safeBrowserStorage()),
      );
      if (res.ok && data.success) {
        setStatus(data.result?.speech || (offlineSd ? "已启动 SD 脱机加工，正在同步进度..." : "已开始雕刻下发，正在同步进度..."));
        startDrawJobPolling(workflowId);
      } else {
        setStatus((typeof data.result === "string" && data.result) || "发送被拒绝");
      }
    } catch (error) {
      setStatus(`请求失败：${error.message || error}`);
    } finally {
      setBusy(false);
    }
  }

  async function runFraming() {
    if (busy) return;
    if (previewStale) {
      setStatus("画布或参数已改动，请先重新生成预览后再走边框");
      return;
    }
    if (!hostReady) {
      setStatus(drawTargetMissingText(form));
      return;
    }
    // 两点对位结果的范围相对第 2 个标记点（frame_mm，可为负）；普通画板任务以激光头为左下角
    const extent = drawFramingExtent(result?.result);
    if (!extent) {
      setStatus("当前任务没有可走的边框范围");
      return;
    }
    const { width, height, frame } = extent;
    const registered = Boolean(result?.result?.registration);
    // 走边框会实际移动机器（勾选弱光时还会低功率出光），单击不得直接执行（2026-11-05 用户决定）
    const lightOn = framingLight;
    const outline = framingShape === "hull" ? framingOutline(result?.result?.artifacts) : null;
    const confirmed = await showConfirm(framingConfirmText(form, width, height, lightOn, outline?.length || 0, registered), {
      title: lightOn ? "确认走边框（弱光出光）" : "确认走边框（机器将移动）",
      tone: "warning",
      danger: lightOn,
      confirmText: lightOn ? "移动并弱光走边框" : "开始走边框",
      cancelText: "取消",
    });
    if (!confirmed) {
      setStatus("已取消走边框，机器未移动");
      return;
    }
    setBusy(true);
    setStatus(lightOn ? "正在走边框（弱光指示）…顶栏急停可随时停止" : "正在走边框（激光关闭，只移动激光头）…顶栏急停可随时停止");
    try {
      // 默认不出光；附带 relative_to_head: true 以当前激光头位置为左下角起点（与正式雕刻同一参照）
      const res = await postJson("/api/device/framing", buildDrawFramingPayload(form, width, height, lightOn, outline, frame));
      setStatus(formatFramingStatus(res, width, height, lightOn));
    } catch (e) {
      setStatus(`走边框请求失败：${e.message || e}`);
    } finally {
      setBusy(false);
    }
  }

  async function testConnection() {
    if (!hostReady) {
      setConnectionResult({ success: false, result: drawTargetMissingText(form) });
      setStatus(drawTargetMissingText(form));
      return;
    }
    setConnectionBusy(true);
    setStatus("测试设备连接...");
    try {
      const res = await postJson("/api/connection/check", {
        ...drawConnectionPayload(form),
        network_transport: "telnet",
        network_timeout: 3,
        include_detail: false,
      });
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      setConnectionResult(data);
      const motion = res.ok && data.success ? machineMotionFromLimits(data.result?.machine_limits) : null;
      if (motion) {
        writeMachineMotion(motion);
        setMachineMotion(motion);
      }
      setStatus(
        res.ok && data.success
          ? data.result?.summary || "连接检查完成"
          : (typeof data.result === "string" && data.result) || "连接检查失败",
      );
    } catch (error) {
      const message = error.message || String(error);
      setConnectionResult({ success: false, result: message });
      setStatus(`连接检查失败：${message}`);
    } finally {
      setConnectionBusy(false);
    }
  }

  async function loadSerialPorts() {
    try {
      const response = await apiFetch("/api/ui/serial-ports");
      const data = await response.json();
      const ports = Array.isArray(data?.result?.ports) ? data.result.ports : [];
      setSerialPorts(ports);
      if (ports.length && !ports.includes(formRef.current.port)) updateForm("port", ports[0]);
      setStatus(ports.length ? `发现串口：${ports.join(", ")}` : "未检测到可用串口");
    } catch (error) {
      setStatus(`读取串口列表失败：${error.message || error}`);
    }
  }

  // 顶栏常驻急停（2026-11-05）：只走 `/api/safe-action` 实时 0x18，绝不退化为 workflow cancel。
  // 顶栏已先停后端登记的任务；轮到本页说明任务已不在登记表（如刚全量发送完、GRBL 仍在执行缓冲）
  // 或任务通道挂起，此时按任务编号失败后改用本页设备目标新开连接写 0x18（`usePageEmergencyStop`）。
  usePageEmergencyStop(
    () => ({ jobId: workflowId, target: estopTargetArgs(form) }),
    (outcome) => {
      const message = outcome.kind === "sent"
        ? `${outcome.message || "急停 0x18 已发送"}。软件急停不代表物理停机，请到设备旁确认；必要时按实体急停。`
        : `${outcome.message || "急停发送失败"}。请立即使用实体急停，并检查设备连接。`;
      setStatus(message);
      void showAlert(message, { title: outcome.kind === "sent" ? "已发送急停" : "急停未确认", tone: outcome.kind === "sent" ? "warning" : "error" });
    },
  );

  // 两点对位生成的新任务（带画板视图、指回原画板任务）：换成当前任务，画布与参数快照不变，
  // 改了画布或参数照样判为过期；尚未发送，发送仍走本页确认框与重复发送防呆
  function adoptRegistered(record) {
    const adopted = adoptRegisteredDrawRecord(record);
    if (adopted.error) {
      setStatus(adopted.error);
      void showAlert(adopted.error, { title: "对位结果不能用于画板", tone: "warning" });
      return;
    }
    pollingCleanupRef.current?.();
    pollingCleanupRef.current = null;
    setWorkflowId(adopted.workflowId);
    setResult(adopted.result);
    setConfirmOpen(false);
    setRepeatSend((current) => applyPreviewFingerprintSuccess(current, adopted.fingerprint));
    setStatus(adopted.status);
  }

  async function workflowAction(action) {
    if (!workflowId) return;
    if (action === "cancel") {
      const confirmed = await showConfirm(
        "确认取消当前画板任务吗？若设备正在雕刻，将停止后续发送并请求安全停机（紧急情况请直接按顶栏红色「急停 0x18」或机器实体急停）。",
        {
          title: "确认取消任务",
          tone: "warning",
          confirmText: "取消任务",
          cancelText: "保留",
        },
      );
      if (!confirmed) return;
    }
    setBusy(true);
    setStatus(action === "status" ? "查询状态..." : "提交取消...");
    try {
      const res = await postJson("/api/workflow", { action, workflow_id: workflowId });
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
      setResult(data);
      setStatus(
        res.ok && data.success
          ? data.result?.speech || "已更新"
          : (typeof data.result === "string" && data.result) || "操作失败",
      );
    } catch (error) {
      setStatus(`请求失败：${error.message || error}`);
    } finally {
      setBusy(false);
    }
  }

  async function downloadPng() {
    try {
      const { blob } = await exportScene();
      const saved = await exportFile(blob, "excalidraw-laser-lab.png", "PNG ");
      setStatus(savedMessage("PNG ", saved));
    } catch (error) {
      const message = `导出 PNG 失败：${error.message || error}`;
      setStatus(message);
      void showAlert(message, { title: "导出失败", tone: "error" });
    }
  }

  async function downloadScene() {
    try {
      const { scene } = lastScene ? { scene: lastScene } : await exportScene();
      const blob = new Blob([JSON.stringify(scene, null, 2)], { type: "application/json" });
      const saved = await exportFile(blob, "excalidraw-laser-lab.excalidraw.json", "场景 JSON ");
      setStatus(savedMessage("场景 JSON ", saved));
    } catch (error) {
      const message = `导出场景失败：${error.message || error}`;
      setStatus(message);
      void showAlert(message, { title: "导出失败", tone: "error" });
    }
  }

  // 预览结果里的 G-code / 场景下载：经鉴权请求取回文件，再统一保存（桌面端写入“下载”文件夹）
  async function downloadFromUrl(url, fallbackName, label) {
    try {
      const response = await apiFetch(url);
      if (!response.ok) throw new Error(`读取文件失败（HTTP ${response.status}）`);
      const blob = await response.blob();
      const saved = await exportFile(blob, fileNameFromUrl(url, fallbackName), label);
      setStatus(savedMessage(label, saved));
    } catch (error) {
      const message = `下载${label}失败：${error.message || error}`;
      setStatus(message);
      void showAlert(message, { title: "下载失败", tone: "error" });
    }
  }

  // 采用加工反馈的调参建议：换成新生成的预览任务，尚未发送
  const adoptFeedbackWorkflow = (workflow) => {
    pollingCleanupRef.current?.();
    pollingCleanupRef.current = null;
    setWorkflowId(workflow.workflow_id);
    setResult({ success: true, result: workflow });
    setPreviewSnap({ sceneRev, formSig: currentFormSig });
    setConfirmOpen(false);
    setRepeatSend((current) => applyPreviewFingerprintSuccess(current, workflow.draw_lab?.content_fingerprint || ""));
    setStatus("调参预览已生成，尚未发送");
  };

  return (
    <main className="app-shell ob-page ob-draw-page">

      {/* 大画布占满整行，下方三张卡并排：材料与尺寸、设备、预览与发送（技术书 §9.5.5 B、r25；支持自由缩放/拖动/折叠） */}
      <section className="workspace ob-draw">
        <WorkbenchCanvas
          defaultLayout={DRAW_DEFAULT_LAYOUT}
          storageKey={DRAW_STORAGE_KEY}
          toolbarStart={
            <>
              <div className="topbar-actions">
                <button type="button" className="btn btn-ghost" onClick={() => onNavigate("/")}>
                  <Home size={14} aria-hidden="true" />
                  工作台
                </button>
                <button
                  type="button"
                  id="draw-import-scene"
                  className="btn btn-ghost"
                  onClick={() => importInputRef.current?.click()}
                  disabled={busy || !excalidrawAPI}
                  title="导入之前保存的 .excalidraw.json 场景继续编辑"
                >
                  <Upload size={14} aria-hidden="true" />
                  导入
                </button>
                <input
                  ref={importInputRef}
                  type="file"
                  accept=".json,.excalidraw,application/json"
                  className="sr-only-file"
                  onChange={handleImportSceneFile}
                />
                <button type="button" className="btn btn-ghost" onClick={downloadScene} disabled={busy || !excalidrawAPI}>
                  <FileJson size={14} aria-hidden="true" />
                  场景
                </button>
                <button type="button" className="btn btn-ghost" onClick={downloadPng} disabled={busy || !excalidrawAPI}>
                  <Download size={14} aria-hidden="true" />
                  PNG
                </button>
              </div>
              <div className="ob-canvasbar">
                <span className="draw-note" role="status">{status}</span>
              </div>
            </>
          }
        >
        <WbCard id="board" className="hud-card ob-card canvas-panel ob-board" label="画布">
          <FoldHeader id="draw-board" title="创作画布" k="CANVAS" />
          <div className="ob-board-stage" ref={boardStageRef}>
            <Excalidraw
              excalidrawAPI={setExcalidrawAPI}
              theme="dark"
              langCode={EXCALIDRAW_LANG_CODE}
              onChange={(elements, appState) => {
                const nextRev = computeSceneRevision(elements);
                setSceneRev((prev) => (prev === nextRev ? prev : nextRev));
                const nextSelection = selectedElementIds(appState).join(",");
                setSelectionKey((prev) => (prev === nextSelection ? prev : nextSelection));
              }}
            />
          </div>
        </WbCard>

        <DrawMaterialCard
          form={form}
          updateForm={updateForm}
          materialLibrary={materialLibrary}
          materialMode={materialMode}
          materialSelectValue={materialSelectValue}
          handleMaterialSelectChange={handleMaterialSelectChange}
          handleThicknessSelectChange={handleThicknessSelectChange}
          handleReturnToLibrary={handleReturnToLibrary}
          materialRefreshBusy={materialRefreshBusy}
          refreshMaterialLibrary={refreshMaterialLibrary}
          selectedOnly={selectedOnly}
          setSelectedOnly={setSelectedOnly}
          selectionKey={selectionKey}
          fillSettings={fillSettings}
          updateFillSettings={updateFillSettings}
          materialApplied={materialApplied}
          materialLayer={materialLayer}
          saveFillToMaterial={saveFillToMaterial}
          hasRec={hasRec}
          recPowerPct={recPowerPct}
          recFeedMpm={recFeedMpm}
          recPasses={recPasses}
          customParams={customParams}
          setCustomParams={setCustomParams}
          customPower={customPower}
          setCustomPower={setCustomPower}
          customFeed={customFeed}
          setCustomFeed={setCustomFeed}
          customPasses={customPasses}
          setCustomPasses={setCustomPasses}
        />

        <DrawDeviceCard
          form={form}
          updateForm={updateForm}
          busy={busy}
          serialPorts={serialPorts}
          setSerialPorts={setSerialPorts}
          loadSerialPorts={loadSerialPorts}
          discoveredChoices={discoveredChoices}
          pickDiscoveredController={pickDiscoveredController}
          discovering={discovering}
          discoverControllers={discoverControllers}
          connectionBusy={connectionBusy}
          testConnection={testConnection}
          connectionResult={connectionResult}
          saveConfiguration={saveConfiguration}
        />

        <DrawSendCard
          form={form}
          busy={busy}
          excalidrawAPI={excalidrawAPI}
          workflowId={workflowId}
          result={result}
          web={web}
          rawText={rawText}
          jobSummary={jobSummary}
          previewStale={previewStale}
          hostReady={hostReady}
          canActOnWorkflow={canActOnWorkflow}
          canConfirmSend={canConfirmSend}
          summaryAllowsSend={summaryAllowsSend}
          framingLight={framingLight}
          setFramingLight={setFramingLight}
          framingShape={framingShape}
          setFramingShape={setFramingShape}
          motionPayload={motionPayload}
          setStatus={setStatus}
          generatePreview={generatePreview}
          openSendConfirmation={openSendConfirmation}
          runFraming={runFraming}
          workflowAction={workflowAction}
          adoptRegistered={adoptRegistered}
          adoptFeedbackWorkflow={adoptFeedbackWorkflow}
          downloadFromUrl={downloadFromUrl}
        />
        </WorkbenchCanvas>
      </section>

      {confirmOpen && (
        <SendConfirmDialog
          summary={jobSummary}
          originNote={result?.result?.registration ? drawOriginNote(result.result) : ""}
          target={hostReady ? drawTargetLabel(form) : ""}
          sdAvailable={!isSerial(form)}
          busy={busy}
          sendAllowed={summaryAllowsSend}
          onCancel={() => setConfirmOpen(false)}
          onConfirm={confirmSend}
          requireMovedAck={requiresMovedMaterialAcknowledgement(repeatSend)}
          movedAcknowledged={Boolean(repeatSend.movedMaterialAcknowledged)}
          onMovedAckChange={(value) =>
            setRepeatSend((current) => setMovedMaterialAcknowledged(current, value))
          }
        />
      )}
    </main>
  );
}

function blobToDataUrl(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error || new Error("读取 PNG 失败"));
    reader.readAsDataURL(blob);
  });
}

/** 从 `/preview?path=...` 下载地址取出文件名；取不到时用默认名 */
function fileNameFromUrl(url, fallback) {
  try {
    const parsed = new URL(url, "http://localhost");
    const path = parsed.searchParams.get("path") || parsed.pathname;
    const name = path.split(/[\\/]/).pop();
    return name && name.includes(".") ? name : fallback;
  } catch (_error) {
    return fallback;
  }
}
