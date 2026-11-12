import React, { useEffect, useMemo, useRef, useState } from "react";
import { Excalidraw, exportToBlob } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  Clock,
  Download,
  Eye,
  FileJson,
  Home,
  RefreshCw,
  Ruler,
  Save,
  Send,
  Settings2,
  ShieldAlert,
  Square,
  Upload,
  UploadCloud,
  Wifi,
  X,
} from "lucide-react";
import { prepareRasterExportElements } from "../rasterInkElements.js";
import {
  sendBadgeLabel,
  summaryAllowsConfirmSend,
  summaryDisplayWarnings,
  thicknessDisplayRows,
} from "../jobSummaryView.js";
import {
  CUSTOM_OPTION_LABEL,
  CUSTOM_OPTION_VALUE,
  MATERIAL_LIBRARY_CHANNEL,
  applyMaterialLibraryRefresh,
  applyMaterialSelectChange,
  findMaterialOption,
  loadUiMaterialOptions,
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
import { isRemoteMode, resolveApiUrl } from "../authSession.js";
import { apiFetch, fetchPreviewBlobUrl, getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm, showPrompt } from "../appDialog.js";
import { exportFile, savedMessage } from "../saveFile.js";
import { FeedbackPanel } from "../components/FeedbackPanel.jsx";
import { estopTargetArgs } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import { TERMINAL_STATUSES } from "../state/statusNormalize.js";
import { MANUAL_LIMITS, manualParamsPayload } from "../workbenchParams.js";
import { controllerLabel, discoveryOutcome } from "../controllerDiscovery.js";
import { defaultSerialPortHint } from "../platform.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { WbCard, WorkbenchCanvas } from "../components/WorkbenchCanvas.jsx";
import { DRAW_DEFAULT_LAYOUT, DRAW_STORAGE_KEY } from "../workbenchLayout.js";
import {
  buildDrawFramingPayload,
  computeSceneRevision,
  drawConnectionPayload,
  drawGenerationSignature,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  formatFramingStatus,
  framingConfirmText,
  isDrawConnectionOk,
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
  const [customPower, setCustomPower] = useState("40");
  const [customFeed, setCustomFeed] = useState("1500");
  const [customPasses, setCustomPasses] = useState("1");
  const [repeatSend, setRepeatSend] = useState(() => createRepeatSendState(safeBrowserStorage()));
  const [serialPorts, setSerialPorts] = useState([]);
  const [framingLight, setFramingLight] = useState(false);
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
      engraving_mode: form.mode === "outline" ? "outline" : "raster",
    });
    getJson(`/api/ui/material-recommendation?${query.toString()}`).then((res) => {
      if (cancelled) return;
      if (!res.ok || !res.data?.success || !res.data.result) {
        setHasRec(false);
        return;
      }
      const rec = res.data.result;
      if (rec.laser_max_power != null) setRecPowerPct(Math.round(rec.laser_max_power / 10));
      if (rec.feed_rate != null) setRecFeedMpm(Math.round(rec.feed_rate));
      setRecPasses(Math.max(1, Math.round(rec.passes || 1)));
      setHasRec(true);
    });
    return () => {
      cancelled = true;
    };
  }, [form.material, form.thickness_mm, form.mode, recRefreshTick]);

  const currentFormSig = drawGenerationSignature(form, {
    enabled: customParams,
    power: customPower,
    feed: customFeed,
    passes: customPasses,
    presetParams: hasRec ? `${recPowerPct}/${recFeedMpm}/${recPasses}` : "",
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
        const fitNote = data.result?.artifacts?.fit_note;
        setStatus([data.result?.speech || "预览已生成", fitNote].filter(Boolean).join("；"));
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
    const width = jobSummary?.actual_width_mm || 100;
    const height = jobSummary?.actual_height_mm || 100;
    // 走边框会实际移动机器（勾选弱光时还会低功率出光），单击不得直接执行（2026-11-05 用户决定）
    const lightOn = framingLight;
    const confirmed = await showConfirm(framingConfirmText(form, width, height, lightOn), {
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
      const res = await postJson("/api/device/framing", buildDrawFramingPayload(form, width, height, lightOn));
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
              onChange={(elements) => {
                const nextRev = computeSceneRevision(elements);
                setSceneRev((prev) => (prev === nextRev ? prev : nextRev));
              }}
            />
          </div>
        </WbCard>

        <WbCard id="draw-material" className="hud-card ob-card control-panel" label="材料与尺寸">
          <FoldHeader id="draw-material" title="材料与尺寸" k="MATERIAL" n={1} />
          <div className="material-library-label">
            <span>材料库</span>
            <div className="material-library-row">
              <select
                value={materialSelectValue}
                onChange={(event) => handleMaterialSelectChange(event.target.value)}
              >
                {materialLibrary.map((item) => (
                  <option key={item.name} value={item.name}>
                    {item.name}
                  </option>
                ))}
                <option value={CUSTOM_OPTION_VALUE}>{CUSTOM_OPTION_LABEL}</option>
              </select>
              <button
                type="button"
                className="ghost-button icon-button"
                title="刷新材料库"
                aria-label="刷新材料库"
                disabled={materialRefreshBusy}
                onClick={refreshMaterialLibrary}
              >
                <RefreshCw size={16} />
              </button>
            </div>
          </div>

          {materialMode === "custom" ? (
            <label>
              材料名称
              <input
                value={form.material}
                onChange={(event) => updateForm("material", event.target.value)}
              />
            </label>
          ) : null}

          {materialMode === "library" && findMaterialOption(materialLibrary, form.material) ? (
            <label>
              厚度 mm
              <select
                value={form.thickness_mm}
                onChange={(event) => handleThicknessSelectChange(event.target.value)}
              >
                {findMaterialOption(materialLibrary, form.material).thicknesses.map((value) => (
                  <option key={String(value)} value={String(value)}>
                    {value}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <label>
              厚度 mm
              <input
                type="number"
                min="0"
                step="0.1"
                value={form.thickness_mm}
                onChange={(event) => updateForm("thickness_mm", event.target.value)}
              />
            </label>
          )}

          {materialMode === "custom" && materialLibrary.length > 0 ? (
            <button type="button" className="ghost-button" onClick={handleReturnToLibrary}>
              返回材料库
            </button>
          ) : null}

          <div className="dimension-grid">
            <label>
              宽 mm
              <input
                type="number"
                min="0"
                max="100"
                step="0.1"
                value={form.width_mm}
                onChange={(event) => updateForm("width_mm", event.target.value)}
                placeholder="自动 (≤100)"
              />
            </label>
            <label>
              高 mm
              <input
                type="number"
                min="0"
                max="100"
                step="0.1"
                value={form.height_mm}
                onChange={(event) => updateForm("height_mm", event.target.value)}
                placeholder="自动 (≤100)"
              />
            </label>
          </div>

          <label>
            生成策略
            <select value={form.mode} onChange={(event) => updateForm("mode", event.target.value)}>
              <option value="raster">光栅填充（逐行扫描填满）</option>
              <option value="outline">轮廓描边（沿深色图形边缘走线）</option>
            </select>
          </label>

          {form.mode === "outline" ? (
            <label>
              轮廓平滑度（0.25 精细 ～ 8 简化）
              <input
                type="number"
                min="0.25"
                max="8"
                step="0.05"
                value={form.vector_simplify_factor}
                onChange={(event) => updateForm("vector_simplify_factor", event.target.value)}
                placeholder="默认 3"
              />
            </label>
          ) : null}

          <div className="wb-params" id="draw-params">
            <label className="wb-params-switch checkbox-row draw-label">
              <input
                type="checkbox"
                id="draw-custom-params"
                checked={customParams}
                onChange={(event) => {
                  const on = event.target.checked;
                  setCustomParams(on);
                  if (on) {
                    setCustomPower(String(recPowerPct));
                    setCustomFeed(String(recFeedMpm));
                    setCustomPasses(String(Math.max(1, Math.min(MANUAL_LIMITS.passes.max, recPasses))));
                  }
                }}
              />
              <span>这次自定义功率和速度</span>
            </label>
            {customParams ? (
              <>
                <div className="wb-params-grid">
                  <label>
                    <span>功率 %</span>
                    <input
                      className="text-input"
                      id="draw-custom-power"
                      type="number"
                      inputMode="decimal"
                      min={MANUAL_LIMITS.power.min}
                      max={MANUAL_LIMITS.power.max}
                      value={customPower}
                      onChange={(e) => setCustomPower(e.target.value)}
                    />
                  </label>
                  <label>
                    <span>速度 mm/min</span>
                    <input
                      className="text-input"
                      id="draw-custom-feed"
                      type="number"
                      inputMode="numeric"
                      step="10"
                      min={MANUAL_LIMITS.feed.min}
                      max={MANUAL_LIMITS.feed.max}
                      value={customFeed}
                      onChange={(e) => setCustomFeed(e.target.value)}
                    />
                  </label>
                  <label>
                    <span>遍数</span>
                    <input
                      className="text-input"
                      id="draw-custom-passes"
                      type="number"
                      inputMode="numeric"
                      min={MANUAL_LIMITS.passes.min}
                      max={MANUAL_LIMITS.passes.max}
                      value={customPasses}
                      onChange={(e) => setCustomPasses(e.target.value)}
                    />
                  </label>
                </div>
                <div className="wb-params-hint">只影响这一次生成，不写回材料库；重新生成预览后生效。</div>
              </>
            ) : hasRec ? (
              <div className="wb-params-hint" id="draw-params-recommended">
                材料库推荐：功率 {recPowerPct}% · 速度 {recFeedMpm} mm/min · {recPasses} 遍
              </div>
            ) : (
              <div className="wb-params-hint" id="draw-params-recommended">
                材料库推荐：暂无匹配参数（可勾选自定义，或前往材料实验室补充）
              </div>
            )}
          </div>

        </WbCard>

        <WbCard id="draw-device" className="hud-card ob-card control-panel" label="设备">
          <FoldHeader id="draw-device" title="设备" k="DEVICE" n={2} />
          <label>
            连接方式
            <select value={form.connection_mode} onChange={(event) => updateForm("connection_mode", event.target.value)}>
              <option value="network">网络 (Telnet/TCP)</option>
              <option value="serial">串口 (USB/Serial)</option>
            </select>
          </label>

          {isSerial(form) ? (
            <>
              <label>
                串口
                <span className="draw-inline">
                  <select
                    value={form.port}
                    onChange={async (event) => {
                      const selectEl = event.target;
                      const chosen = selectEl.value;
                      if (chosen === "__custom__") {
                        selectEl.value = form.port || "";
                        const hint = defaultSerialPortHint();
                        const custom = await showPrompt(hint.prompt, {
                          title: "手动输入串口号",
                          defaultValue: form.port || hint.fallback,
                          placeholder: hint.fallback,
                        });
                        if (custom && custom.trim()) {
                          const p = custom.trim();
                          if (!serialPorts.includes(p)) setSerialPorts((prev) => [...prev, p]);
                          updateForm("port", p);
                        }
                      } else {
                        updateForm("port", chosen);
                      }
                    }}
                  >
                    {form.port && !serialPorts.includes(form.port) ? <option value={form.port}>{form.port}</option> : null}
                    {serialPorts.length === 0 && !form.port ? <option value="">未检测到串口，请点刷新</option> : null}
                    {serialPorts.map((port) => (
                      <option key={port} value={port}>
                        {port}
                      </option>
                    ))}
                    <option value="__custom__">✎ 手动输入串口…</option>
                  </select>
                  <button type="button" className="ghost-button" onClick={loadSerialPorts} disabled={busy}>
                    <RefreshCw size={15} /> 刷新
                  </button>
                </span>
              </label>
              <label>
                波特率
                <select value={form.baudrate} onChange={(event) => updateForm("baudrate", event.target.value)}>
                  {["9600", "19200", "38400", "57600", "115200", "230400", "250000", "460800", "921600"].map((rate) => (
                    <option key={rate} value={rate}>
                      {rate}
                    </option>
                  ))}
                </select>
              </label>
            </>
          ) : (
            <>
              <label>
                控制器 IP / 地址
                <input
                  value={form.network_host}
                  onChange={(event) => updateForm("network_host", event.target.value)}
                  placeholder="例如 192.168.1.10 或 laser.local"
                />
              </label>

              <label>
                Telnet 端口
                <input
                  type="number"
                  min="1"
                  step="1"
                  value={form.network_telnet_port}
                  onChange={(event) => updateForm("network_telnet_port", event.target.value)}
                />
              </label>

              {discoveredChoices.length > 1 ? (
                <label>
                  选择发现的雕刻机
                  <select
                    id="draw-discovered-controllers"
                    value={
                      discoveredChoices.find((c) => c.ip === form.network_host && String(c.telnet_port) === String(form.network_telnet_port))
                        ? `${form.network_host}:${form.network_telnet_port}`
                        : ""
                    }
                    onChange={(event) => {
                      const picked = discoveredChoices.find((c) => `${c.ip}:${c.telnet_port}` === event.target.value);
                      if (picked) pickDiscoveredController(picked);
                    }}
                  >
                    <option value="" disabled>找到 {discoveredChoices.length} 台，请选择…</option>
                    {discoveredChoices.map((c) => (
                      <option key={`${c.ip}:${c.telnet_port}`} value={`${c.ip}:${c.telnet_port}`}>
                        {controllerLabel(c)}
                      </option>
                    ))}
                  </select>
                </label>
              ) : null}

              <button
                type="button"
                id="draw-discover-btn"
                className="secondary-button"
                onClick={discoverControllers}
                disabled={busy || discovering || connectionBusy}
                title="查找局域网里的雕刻机并逐台只读确认"
              >
                <RefreshCw size={16} />
                {discovering ? "扫描局域网中..." : "扫描局域网"}
              </button>
            </>
          )}

          <button
            type="button"
            className="secondary-button"
            onClick={testConnection}
            disabled={busy || connectionBusy}
          >
            <Wifi size={17} />
            {connectionBusy ? "测试中..." : "测试连接"}
          </button>
          <ConnectionStatus form={form} result={connectionResult} />

          <button type="button" className="secondary-button" onClick={saveConfiguration} disabled={busy} title="各项修改会自动记住，也可点此立即核验本地存储">
            <Save size={17} />
            保存配置
          </button>

        </WbCard>

        <WbCard id="draw-send" className="hud-card ob-card control-panel result-panel" label="预览与发送">
          <FoldHeader id="draw-send" title="预览与发送" k="PREVIEW · SEND" n={3} />
          <div className="action-stack">
            <button type="button" className="primary-button" onClick={generatePreview} disabled={busy || !excalidrawAPI}>
              <UploadCloud size={18} />
              {busy ? "正在解算..." : workflowId ? "重新生成预览" : "生成预览"}
            </button>
            <button type="button" id="draw-confirm-send-btn" className="danger-button" onClick={openSendConfirmation} disabled={busy || !canConfirmSend}>
              <Send size={18} />
              {previewStale ? "画布或参数已改 · 请先重算" : "确认发送"}
            </button>
            <button
              type="button"
              id="draw-framing-btn"
              className="secondary-button draw-btn"
              onClick={runFraming}
              disabled={busy || previewStale || !hostReady || !jobSummary?.actual_width_mm}
              title="从激光头当前位置（左下角起点）沿作品外框绕一圈对位；默认不出光"
            >
              <Eye size={18} />
              先走边框
            </button>
            <label className="checkbox-row draw-label" title="勾选后走边框时以 S1（约 0.1% 功率）出极弱的光，便于在材料上看到边框">
              <input type="checkbox" checked={framingLight} onChange={(event) => setFramingLight(event.target.checked)} />
              走边框时弱光指示（默认不出光）
            </label>
            {previewStale && (
              <div className="wb-stale-notice" id="draw-stale-notice" role="status">
                画布图形或左侧参数已改动，当前预览仍是上一版旧图；请先点「重新生成预览」后再走边框或发送。
              </div>
            )}
            {canActOnWorkflow && !previewStale && (
              <div className="wb-origin-note" id="draw-origin-note">
                开刻前把激光头移到作品左下角（走边框与正式开刻共用该起点）
              </div>
            )}
            {canActOnWorkflow && !hostReady && <div className="inline-hint">{drawTargetMissingText(form)}后再发送</div>}
            {canActOnWorkflow && !previewStale && hostReady && !summaryAllowsSend && (
              <div className="inline-hint">当前不可发送：请检查精确厚度参数与服务端确认权限</div>
            )}
            <div className="split-actions">
              <button type="button" onClick={() => workflowAction("status")} disabled={busy || !canActOnWorkflow}>
                <Activity size={17} />
                刷新状态
              </button>
              <button type="button" onClick={() => workflowAction("cancel")} disabled={busy || !canActOnWorkflow}>
                <Square size={17} />
                取消任务
              </button>
            </div>
          </div>

          <div className="workflow-chip">
            <RefreshCw size={16} />
            <span>
              {workflowId
                ? (previewStale ? `预览已过期 · ${workflowId}` : `当前任务 · ${workflowId}`)
                : "等待生成预览"}
            </span>
          </div>

          <JobSummaryCard summary={jobSummary} />
          <FeedbackPanel workflowId={workflowId} origin={result?.result?.feedback_origin}
            disabled={busy || ["sending", "running", "preparing", "awaiting_completion"].includes(result?.result?.status)}
            onAdopt={(workflow) => {
              pollingCleanupRef.current?.();
              pollingCleanupRef.current = null;
              setWorkflowId(workflow.workflow_id);
              setResult({ success: true, result: workflow });
              setPreviewSnap({ sceneRev, formSig: currentFormSig });
              setConfirmOpen(false);
              setRepeatSend((current) => applyPreviewFingerprintSuccess(current, workflow.draw_lab?.content_fingerprint || ""));
              setStatus("调参预览已生成，尚未发送");
            }} />
          <PreviewImage title="素材预览" url={web.material_preview_url || web.uploaded_image_url} stale={previewStale} />
          <PreviewImage title="路径预览" url={web.gcode_preview_url || web.image_preview_url} stale={previewStale} />
          <div className="link-list">
            {web.gcode_download_url && (
              <button
                type="button"
                className="secondary-button"
                onClick={() => downloadFromUrl(web.gcode_download_url, "laser-job.gcode", "G-code ")}
              >
                <Save size={16} />
                下载 G-code
              </button>
            )}
            {web.scene_download_url && (
              <button
                type="button"
                className="secondary-button"
                onClick={() => downloadFromUrl(web.scene_download_url, "excalidraw-laser-lab.excalidraw.json", "场景 JSON ")}
              >
                <FileJson size={16} />
                下载场景 JSON
              </button>
            )}
          </div>
          <details className="diagnostics">
            <summary className="diagnostics-title">
              <AlertTriangle size={16} />
              诊断输出
            </summary>
            <pre>{rawText || "等待预览结果..."}</pre>
          </details>
        </WbCard>
        </WorkbenchCanvas>
      </section>

      {confirmOpen && (
        <SendConfirmDialog
          summary={jobSummary}
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

function JobSummaryCard({ summary, compact = false }) {
  if (!summary) {
    return (
      <section className="job-summary-card muted-summary">
        <div className="summary-heading">
          <Settings2 size={17} />
          <h2>作业摘要</h2>
        </div>
        <p>生成预览后显示材料、尺寸、参数和预计时间。</p>
      </section>
    );
  }

  const displayWarnings = summaryDisplayWarnings(summary, formatWithUnit);
  const thicknessRows = thicknessDisplayRows(summary);
  const sizeText =
    summary.actual_width_mm && summary.actual_height_mm
      ? `${formatMm(summary.actual_width_mm)} x ${formatMm(summary.actual_height_mm)}`
      : "自动";

  return (
    <section className={`job-summary-card ${compact ? "compact" : ""}`}>
      <div className="summary-heading">
        <Settings2 size={17} />
        <h2>作业摘要</h2>
        <span className={summary.can_send ? "summary-badge ok" : "summary-badge warn"}>
          {sendBadgeLabel(summary)}
        </span>
      </div>
      <div className="summary-grid">
        <SummaryItem label="材料" value={summary.material || "未识别"} />
        {thicknessRows.map((row) => (
          <SummaryItem key={row.label} label={row.label} value={formatWithUnit(summary[row.valueKey], "mm")} />
        ))}
        <SummaryItem label="策略" value={modeLabel(summary.mode, summary.task_type)} />
        <SummaryItem label="尺寸" value={sizeText} icon={<Ruler size={15} />} />
        <SummaryItem label="预计时间" value={summary.estimated_display || "待估算"} icon={<Clock size={15} />} />
        <SummaryItem label="功率" value={summary.power ? `S${summary.power}` : "材料库默认"} />
        <SummaryItem label="速度" value={summary.speed ? `F${summary.speed}` : "材料库默认"} />
        <SummaryItem label="次数" value={summary.passes || "默认"} />
        <SummaryItem label="点距" value={formatWithUnit(summary.pixel_size_mm, "mm")} />
      </div>
      {summary.size_note && <p className="size-note">{summary.size_note}</p>}
      {displayWarnings.length > 0 && (
        <div className="warning-list">
          <ShieldAlert size={16} />
          <span>{displayWarnings.join("；")}</span>
        </div>
      )}
    </section>
  );
}

function SummaryItem({ label, value, icon = null }) {
  return (
    <div className="summary-item">
      <span>{label}</span>
      <strong>
        {icon}
        {value || "未知"}
      </strong>
    </div>
  );
}

function ConnectionStatus({ form, result }) {
  if (!result) return null;
  const payload = result.result && typeof result.result === "object" ? result.result : {};
  const connected = isDrawConnectionOk(form, result);
  const text = payload.summary || result.result || "连接检查失败";
  return (
    <div className={`connection-status ${connected ? "ok" : "warn"}`}>
      {connected ? <CheckCircle2 size={16} /> : <AlertTriangle size={16} />}
      <span>{text}</span>
    </div>
  );
}

function SendConfirmDialog({
  summary,
  target,
  sdAvailable = true,
  busy,
  sendAllowed = false,
  onCancel,
  onConfirm,
  requireMovedAck = false,
  movedAcknowledged = false,
  onMovedAckChange,
}) {
  // 默认走在线流式发送（与雕刻工作台一致），避免未插 SD 卡或固件未开 HashFS 时默认脱机直接报错失败
  const [offlineSdChoice, setOfflineSd] = React.useState(false);
  // SD 脱机只能经网口上传；串口只能逐行发送
  const offlineSd = sdAvailable && offlineSdChoice;
  const canConfirm = !requireMovedAck || movedAcknowledged;
  return (
    <div className="modal-backdrop" role="presentation">
      <section className="send-confirm-dialog" role="dialog" aria-modal="true" aria-labelledby="send-confirm-title">
        <div className="confirm-heading">
          <div>
            <Send size={18} />
            <h2 id="send-confirm-title">确认开始雕刻</h2>
          </div>
          <button type="button" className="icon-button" onClick={onCancel} aria-label="关闭确认面板">
            <X size={18} />
          </button>
        </div>
        <JobSummaryCard summary={summary} compact />
        <div className="wb-origin-note">对刀起点：开刻前请把激光头对准作品左下角</div>
        <div className="confirm-device">
          <Wifi size={17} />
          <span>设备：{target}</span>
        </div>
        {sdAvailable ? (
          <label className="checkbox-row confirm-moved-ack draw-label">
            <input
              type="checkbox"
              checked={offlineSd}
              onChange={(e) => setOfflineSd(e.target.checked)}
            />
            改用 SD 卡脱机离线加工（需机器插卡且固件启用 HashFS）
          </label>
        ) : null}
        {requireMovedAck ? (
          <label className="checkbox-row confirm-moved-ack">
            <input
              type="checkbox"
              checked={Boolean(movedAcknowledged)}
              onChange={(event) => onMovedAckChange?.(event.target.checked)}
            />
            已移动材料或原点（相同图案再次发送前必须勾选）
          </label>
        ) : null}
        <div className="confirm-actions">
          <button type="button" className="secondary-button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="danger-button"
            onClick={() => onConfirm(offlineSd)}
            disabled={busy || !target || !sendAllowed || !canConfirm}
          >
            <Send size={18} />
            {busy ? "发送中..." : (offlineSd ? "离线脱机发送" : "在线流式发送")}
          </button>
        </div>
      </section>
    </div>
  );
}

function PreviewImage({ title, url, stale = false }) {
  // 连接电脑时 <img> 直链不带配对 token，改为经鉴权取回 blob 再显示；
  // 本机模式直接使用唯一文件名 URL（不再在 render 期拼 Date.now() 导致敲字时反复重拉闪烁）
  const remote = isRemoteMode();
  const [blobUrl, setBlobUrl] = useState("");
  useEffect(() => {
    if (!remote || !url) {
      setBlobUrl("");
      return undefined;
    }
    let cancelled = false;
    let created = "";
    fetchPreviewBlobUrl(url).then((value) => {
      created = value;
      if (cancelled) {
        if (value) URL.revokeObjectURL(value);
      } else {
        setBlobUrl(value);
      }
    });
    return () => {
      cancelled = true;
      if (created) URL.revokeObjectURL(created);
    };
  }, [remote, url]);
  const src = remote ? blobUrl : (url ? resolveApiUrl(url) : "");
  return (
    <div className={`preview-box${stale ? " is-stale" : ""}`}>
      <div className="preview-title">{title}{stale && src ? "（旧图 · 需重新生成）" : ""}</div>
      {src ? <img src={src} alt={title} /> : <div className="preview-empty">暂无</div>}
    </div>
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

function modeLabel(mode, taskType) {
  if (mode === "raster" || taskType === "engrave_photo") return "光栅填充";
  if (mode === "outline" || taskType === "engrave_logo") return "轮廓描边";
  if (taskType === "cut_contour") return "轮廓切割";
  return mode || taskType || "自动";
}

function formatMm(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return "未知";
  return `${Number(number.toFixed(2)).toString()} mm`;
}

function formatWithUnit(value, unit) {
  if (value === undefined || value === null || value === "") return "默认";
  const number = Number(value);
  if (!Number.isFinite(number)) return String(value);
  return `${Number(number.toFixed(3)).toString()} ${unit}`;
}
