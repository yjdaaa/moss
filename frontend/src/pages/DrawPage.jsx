import React, { useCallback, useEffect, useRef, useState } from "react";
import { Excalidraw, exportToBlob, newElementWith } from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import {
  Download,
  FileJson,
  Layers,
  Send,
  Trash2,
  Undo2,
  Upload,
  UploadCloud,
} from "lucide-react";
import { prepareRasterExportElements } from "../rasterInkElements.js";
import {
  DRAW_LAST_PREVIEW_KEY,
  DRAW_SCENE_KEY,
  parseDrawScene,
  drawSceneChanges,
  drawSceneToRestore,
  snapshotDrawScene,
  takePendingDrawScene,
  parseLastPreview,
  planDrawHydration,
  readStored,
  serializeDrawScene,
  serializeLastPreview,
  shouldApplyRestoredPreview,
  shouldPersistLastPreview,
  writeStored,
} from "../drawScenePersist.js";
import {
  summaryAllowsConfirmSend,
} from "../jobSummaryView.js";
import {
  CUSTOM_OPTION_VALUE,
  applyMaterialLibraryRefresh,
  applyMaterialSelectChange,
  findMaterialOption,
  loadUiMaterialOptions,
  notifyMaterialsUpdated,
  restoreMaterialSelectState,
  returnToLibrary,
  subscribeMaterialsUpdated,
} from "../materialSelectState.js";
import { getLaserExportDimensions } from "../laserExportDimensions.js";
import {
  LAST_ACCEPTED_FINGERPRINT_KEY,
  applyConfirmSendResponse,
  applyPreviewFingerprintSuccess,
  canSubmitConfirmSend,
  createRepeatSendState,
  isNestedSenderAccepted,
  requiresMovedMaterialAcknowledgement,
  setMovedMaterialAcknowledged,
} from "../repeatSendGuard.js";
import { apiFetch, getJson, postJson } from "../api/client.js";
import { approvalPendingMessage, isApprovalRequiredResponse } from "../approvalState.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { exportFile, savedMessage } from "../saveFile.js";
import { drawEstopContexts, estopTargetArgs } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import { TERMINAL_STATUSES } from "../state/statusNormalize.js";
import { manualParamsPayload } from "../workbenchParams.js";
import { applyImagePreset } from "../qualityPresets.js";
import { useEngraveRoute } from "../useEngraveRoute.js";
import { connectionSummaryView, MACHINE_MOTION_EVENT } from "../deviceConnection.js";
import { useDeviceConnection } from "../useDeviceConnection.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { OriginBadge } from "../components/OriginBadge.jsx";
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
  machineMotionPayload,
  materialLayerFromRecommendation,
  materialLayerPayload,
  materialLayerSaveRequest,
  rasterScanPayload,
  rasterScanSignature,
  readFillSettings,
  readMachineMotion,
  timeEstimateNote,
  writeFillSettings,
} from "../fillOptions.js";
import { WbCard, WorkbenchCanvas } from "../components/WorkbenchCanvas.jsx";
import { DrawMaterialCard } from "../components/draw/DrawMaterialCard.jsx";
import { DrawDeviceCard } from "../components/draw/DrawDeviceCard.jsx";
import { DrawSendCard } from "../components/draw/DrawSendCard.jsx";
import { SendConfirmDialog } from "../components/draw/SendConfirmDialog.jsx";
import { DRAW_DEFAULT_LAYOUT, DRAW_STORAGE_KEY } from "../workbenchLayout.js";
import {
  buildDrawFramingPayload,
  checkDerivedDrawAdoption,
  selectedElementIds,
  computeSceneRevision,
  drawConnectionPayload,
  drawGenerationSignature,
  drawInkExportMode,
  drawRasterImagePayload,
  drawIsCut,
  drawRecommendEngravingMode,
  drawRequestMode,
  drawVectorLayerFlags,
  normalizeDrawVectorStyle,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  formatFramingStatus,
  framingConfirmText,
  hasSceneImageElements,
  isDrawPreviewStale,
  isSerial,
  parseImportedSceneJson,
  readDrawDefaultForm,
  safeBrowserStorage,
} from "../drawConnection.js";

const BACKEND_CONFIG_KEYS = [
  "material",
  "thickness_mm",
  "width_mm",
  "height_mm",
  "image_strategy",
  "vector_style",
  "image_mode",
  "line_interval_scale",
  "halftone_cell_mm",
  "halftone_angle_deg",
  "gray_min_power_percent",
  "vector_simplify_factor",
];
const CONFIG_KEYS = BACKEND_CONFIG_KEYS;
const EXCALIDRAW_LANG_CODE = "zh-CN";

export function DrawPage({
  onNavigate,
  active = true,
  connectionStatus = null,
  onOpenConnection = null,
}) {
  // r49：关掉 MOSS 再打开也接着画——上次的画布（含插入的图片，超量时只存图形）从本机存储恢复
  const [initialScene] = useState(() => parseDrawScene(readStored(safeBrowserStorage(), DRAW_SCENE_KEY)));
  const [excalidrawAPI, setExcalidrawAPI] = useState(null);
  const {
    connectionMode,
    setConnectionMode,
    serialPort,
    setSerialPort,
    baudrate,
    setBaudrate,
    networkHost,
    setNetworkHost,
    telnetPort,
    setTelnetPort,
  } = useDeviceConnection();
  // 挂载时惰性读取（存储被禁用时退回默认值，连接字段统一跟随全局 useDeviceConnection）
  const [form, setForm] = useState(() => ({
    ...readDrawDefaultForm(),
    connection_mode: connectionMode,
    port: serialPort,
    baudrate: String(baudrate),
    network_host: networkHost,
    network_telnet_port: String(telnetPort),
  }));
  useEffect(() => {
    setForm((current) => {
      const nextBaud = String(baudrate);
      const nextTelnet = String(telnetPort);
      if (
        current.connection_mode === connectionMode
        && current.port === serialPort
        && current.baudrate === nextBaud
        && current.network_host === networkHost
        && current.network_telnet_port === nextTelnet
      ) {
        return current;
      }
      return {
        ...current,
        connection_mode: connectionMode,
        port: serialPort,
        baudrate: nextBaud,
        network_host: networkHost,
        network_telnet_port: nextTelnet,
      };
    });
  }, [connectionMode, serialPort, baudrate, networkHost, telnetPort]);
  // 雕刻路线全局开关（2026-11-17 技术书 r45，与工作台、语音同一个值）：矢量 → vector；
  // 转图片 → 按“转图片方式”光栅扫描 raster / 轮廓描边 outline。请求照样写明实际 mode
  const {
    route: engraveRoute,
    setRoute: setEngraveRoute,
    saving: engraveRouteSaving,
    readOnly: engraveRouteReadOnly,
  } = useEngraveRoute();
  const drawMode = drawRequestMode(engraveRoute, form.image_strategy);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState("准备就绪");
  const [drawSyncWarning, setDrawSyncWarning] = useState("");
  const [workflowId, setWorkflowId] = useState("");
  const [result, setResult] = useState(null);
  const [sceneRev, setSceneRev] = useState(() => computeSceneRevision(initialScene?.elements));
  // 一键清空画布后的“撤销清空”：清空前的图形，画了新东西后作废
  const clearedSceneRef = useRef(null);
  const [canUndoClear, setCanUndoClear] = useState(false);
  const [previewSnap, setPreviewSnap] = useState(null);
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
  useEffect(() => {
    const onMotion = (e) => {
      if (e?.detail) setMachineMotion(e.detail);
    };
    window.addEventListener(MACHINE_MOTION_EVENT, onMotion);
    return () => window.removeEventListener(MACHINE_MOTION_EVENT, onMotion);
  }, []);
  const motionPayload = machineMotionPayload(fillSettings, machineMotion);
  const [customPower, setCustomPower] = useState("40");
  const [customFeed, setCustomFeed] = useState("1500");
  const [customPasses, setCustomPasses] = useState("1");
  const [repeatSend, setRepeatSend] = useState(() => createRepeatSendState(safeBrowserStorage()));
  // 走边框走法：外接矩形 / 沿外轮廓（橡皮筋），与工作台共用
  const [framingShape, setFramingShape] = useState(() => readFramingShape());
  // 只刻选中的图形（矢量模式）：选区变化计入预览签名
  const [selectedOnly, setSelectedOnly] = useState(false);
  const [selectionKey, setSelectionKey] = useState("");
  const [hasSceneImages, setHasSceneImages] = useState(() => hasSceneImageElements(initialScene?.elements));
  const materialLibraryRef = useRef(materialLibrary);
  const materialModeRef = useRef(materialMode);
  const formRef = useRef(form);
  const persistedRef = useRef({});
  const inFlightJobsRef = useRef(new Map());
  const workflowIdRef = useRef(workflowId);
  workflowIdRef.current = workflowId;
  const importInputRef = useRef(null);
  const materialLoadedOnceRef = useRef(false);
  const materialRefreshGenerationRef = useRef(0);
  const boardStageRef = useRef(null);
  const excalidrawApiRef = useRef(null);
  // 两点对位的原画板任务：当前任务本身，或（已换成对位结果时）它指回的原任务
  const registrationSourceRef = useRef("");
  // #23 / #24：保护挂载恢复期，不清空上次预览指针、不被旧响应覆写，并同步到后端应用数据目录
  const previewActionSeqRef = useRef(0);
  const [lastPreviewHydrated, setLastPreviewHydrated] = useState(false);
  const appliedSceneRef = useRef(null);
  if (!appliedSceneRef.current) {
    appliedSceneRef.current = snapshotDrawScene(initialScene?.elements, initialScene?.appState, initialScene?.files);
  }
  const applyingRemoteSceneRef = useRef(false);
  const canvasReadyRef = useRef(false);
  const sceneDirtyRef = useRef(false);
  const pendingRemoteSceneRef = useRef(null);
  const dirtyFormKeysRef = useRef(new Set());
  const formHydratedRef = useRef(false);
  const localSessionAcceptedRef = useRef(false);
  const lastPersistedPreviewRef = useRef(undefined);

  useEffect(() => {
    excalidrawApiRef.current = excalidrawAPI;
  }, [excalidrawAPI]);

  useEffect(() => {
    const jobs = inFlightJobsRef.current;
    return () => {
      for (const job of jobs.values()) job.stop?.();
      jobs.clear();
    };
  }, []);

  const syncDrawStateToBackend = (patch, { keepalive = false } = {}) => {
    if (!patch || typeof patch !== "object" || Object.keys(patch).length === 0) return;
    const body = JSON.stringify(patch);
    const useKeepalive = Boolean(keepalive && body.length <= 60_000);
    apiFetch("/api/draw/state", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json",
      },
      body,
      ...(useKeepalive ? { keepalive: true } : {}),
    })
      .then((res) => {
        if (keepalive) return;
        if (res.ok) {
          setDrawSyncWarning("");
          return;
        }
        // 局域网只读等 403 响应不打断用户，其余写入失败显式提示。
        if (res.status === 403) return;
        console.warn(`[draw-state] 后端保存返回 HTTP ${res.status}`);
        setDrawSyncWarning((prev) => prev || `画布已保存在本地浏览器，但同步到本机服务失败（HTTP ${res.status}）`);
      })
      .catch((error) => {
        if (keepalive) return;
        const msg = error instanceof Error ? error.message : String(error);
        console.warn(`[draw-state] 同步到本机服务失败: ${msg}`);
        setDrawSyncWarning((prev) => prev || "画布已保存在本地浏览器，但暂未同步到本机服务（请确认服务运行正常）");
      });
  };

  const applyRemoteSceneToBoard = useCallback((api, parsed) => {
    if (!api || !parsed) return false;
    const fileEntries = Object.values(parsed.files || {}).filter((item) => item && typeof item === "object");
    // 恢复时 0.6 秒内排队的旧视图和同步触发的回调都不能写回去。
    pendingSceneRef.current = null;
    applyingRemoteSceneRef.current = true;
    try {
      if (fileEntries.length > 0) api.addFiles?.(fileEntries);
      api.updateScene({ elements: parsed.elements, appState: parsed.appState });
      appliedSceneRef.current = snapshotDrawScene(api.getSceneElements(), api.getAppState(), api.getFiles());
    } finally {
      applyingRemoteSceneRef.current = false;
    }
    setSceneRev(appliedSceneRef.current.sceneRev);
    setHasSceneImages(hasSceneImageElements(parsed.elements));
    setStatus(
      parsed.filesDropped
        ? "已恢复上次的画布；插入的图片太大没有存进本机，需要重新插入"
        : "已恢复上次的画布",
    );
    return true;
  }, []);

  // 画布保存：内容变化后 0.6 秒写入本机并同步后端；关窗口 / 页面隐藏前把还没写的补上
  const pendingSceneRef = useRef(null);
  const sceneSaveTimerRef = useRef(null);
  const flushScene = ({ keepalive = false } = {}) => {
    clearTimeout(sceneSaveTimerRef.current);
    sceneSaveTimerRef.current = null;
    const pending = pendingSceneRef.current;
    if (!pending) return;
    pendingSceneRef.current = null;
    const serialized = serializeDrawScene(pending.elements, pending.appState, pending.files);
    writeStored(safeBrowserStorage(), DRAW_SCENE_KEY, serialized);
    if (pending.syncRemote) syncDrawStateToBackend({ scene: serialized }, { keepalive });
  };
  const flushSceneRef = useRef(flushScene);
  useEffect(() => {
    flushSceneRef.current = flushScene;
  });
  useEffect(() => {
    const onHide = () => flushSceneRef.current({ keepalive: true });
    window.addEventListener("pagehide", onHide);
    window.addEventListener("beforeunload", onHide);
    return () => {
      window.removeEventListener("pagehide", onHide);
      window.removeEventListener("beforeunload", onHide);
      onHide();
    };
  }, []);
  useEffect(() => {
    if (initialScene?.filesDropped) {
      setStatus("已恢复上次的画布；插入的图片太大没有存进本机，需要重新插入");
    } else if (initialScene) {
      setStatus("已恢复上次的画布");
    }
  }, [initialScene]);

  const applyPendingRemoteScene = useCallback((api) => {
    const pending = takePendingDrawScene(pendingRemoteSceneRef.current, {
      apiReady: Boolean(api), canvasReady: canvasReadyRef.current, sceneDirty: sceneDirtyRef.current,
    });
    if (!pending) return false;
    pendingRemoteSceneRef.current = null;
    return applyRemoteSceneToBoard(api, pending);
  }, [applyRemoteSceneToBoard]);

  useEffect(() => {
    applyPendingRemoteScene(excalidrawAPI);
  }, [excalidrawAPI, applyPendingRemoteScene]);


  // 上次画布、表单与成功预览恢复（#23/#24）：从后端应用数据目录读取并与本地 localStorage 对齐；
  // 恢复查询返回前跳过清空存储，且用户期间新建/采纳预览时丢弃迟到的旧查询结果
  useEffect(() => {
    let cancelled = false;
    const storage = safeBrowserStorage();
    const restoreSeq = previewActionSeqRef.current;

    const restoreWorkflow = async (rawLastPreview) => {
      const stored = parseLastPreview(rawLastPreview);
      if (!stored) return;
      try {
        const res = await postJson("/api/workflow", { action: "status", workflow_id: stored.workflowId });
        const record = res.ok && res.data?.success ? res.data.result : null;
        if (
          !record?.workflow_id
          || !shouldApplyRestoredPreview({
            cancelled,
            requestSeq: restoreSeq,
            currentSeq: previewActionSeqRef.current,
          })
        ) {
          return;
        }
        setResult(res.data);
        setWorkflowId(stored.workflowId);
        setPreviewSnap(stored.snap);
        const fingerprint = record.draw_lab?.content_fingerprint || "";
        setRepeatSend((current) => applyPreviewFingerprintSuccess(current, fingerprint));
      } catch {
        // 忽略旧预览恢复请求失败
      }
    };

    (async () => {
      const localSceneRaw = readStored(storage, DRAW_SCENE_KEY);
      const localLastPreviewRaw = readStored(storage, DRAW_LAST_PREVIEW_KEY);
      const localFormSnapshot = {};
      for (const key of BACKEND_CONFIG_KEYS) {
        const storedVal = readStored(storage, `laserLab.${key}`);
        if (typeof storedVal === "string" && storedVal !== "") {
          localFormSnapshot[key] = storedVal;
        }
      }
      const localFingerprint = readStored(storage, LAST_ACCEPTED_FINGERPRINT_KEY) || "";

      let remoteState = null;
      try {
        const stateRes = await getJson("/api/draw/state");
        if (cancelled) return;
        if (stateRes.ok && stateRes.data?.success && stateRes.data.result) {
          remoteState = stateRes.data.result;
        }
      } catch {
        remoteState = null;
      }
      if (cancelled) return;

      if (typeof remoteState?.last_preview === "string") {
        lastPersistedPreviewRef.current = remoteState.last_preview;
      }

      const plan = planDrawHydration({
        remoteState,
        localSceneRaw: pendingSceneRef.current
          ? serializeDrawScene(pendingSceneRef.current.elements, pendingSceneRef.current.appState, pendingSceneRef.current.files)
          : readStored(storage, DRAW_SCENE_KEY) ?? localSceneRaw,
        localLastPreviewRaw,
        localForm: localFormSnapshot,
        localFingerprint,
        configKeys: BACKEND_CONFIG_KEYS,
        sceneDirty: sceneDirtyRef.current,
        dirtyFormKeys: dirtyFormKeysRef.current,
        previewActionSeq: previewActionSeqRef.current,
      });

      if (plan.sceneToApply !== null && !sceneDirtyRef.current) {
        writeStored(storage, DRAW_SCENE_KEY, plan.sceneToApply);
        pendingRemoteSceneRef.current = drawSceneToRestore(plan.sceneToApply);
        applyPendingRemoteScene(excalidrawApiRef.current);
      }

      if (Object.keys(plan.formUpdates).length > 0) {
        for (const [key, val] of Object.entries(plan.formUpdates)) {
          persistedRef.current[key] = val;
        }
        setForm((current) => {
          const next = { ...current };
          for (const [key, val] of Object.entries(plan.formUpdates)) {
            if (!dirtyFormKeysRef.current.has(key)) next[key] = val;
          }
          return next;
        });
        if (
          ("material" in plan.formUpdates || "thickness_mm" in plan.formUpdates)
          && Array.isArray(materialLibraryRef.current)
          && materialLibraryRef.current.length > 0
        ) {
          const mergedMat = !dirtyFormKeysRef.current.has("material") && "material" in plan.formUpdates
            ? plan.formUpdates.material
            : formRef.current.material;
          const mergedThick = !dirtyFormKeysRef.current.has("thickness_mm") && "thickness_mm" in plan.formUpdates
            ? plan.formUpdates.thickness_mm
            : formRef.current.thickness_mm;
          const restoredMat = restoreMaterialSelectState({
            materials: materialLibraryRef.current,
            savedMaterial: mergedMat,
            savedThickness: mergedThick,
          });
          setMaterialLibrary(restoredMat.materials);
          setMaterialMode(restoredMat.mode);
          setMaterialSelectValue(restoredMat.selectValue);
        }
      } else {
        for (const key of CONFIG_KEYS) {
          const raw = formRef.current[key];
          persistedRef.current[key] = typeof raw === "boolean" ? (raw ? "true" : "false") : String(raw ?? "");
        }
      }
      formHydratedRef.current = true;

      if (plan.fingerprintUpdate !== null && !localSessionAcceptedRef.current) {
        writeStored(storage, LAST_ACCEPTED_FINGERPRINT_KEY, plan.fingerprintUpdate);
        setRepeatSend((current) => ({
          ...current,
          lastAcceptedFingerprint: plan.fingerprintUpdate,
        }));
      }

      const initialPatch = { ...plan.backfill };
      if (dirtyFormKeysRef.current.size > 0) {
        const dirtyFormPatch = { ...(initialPatch.form || {}) };
        for (const key of dirtyFormKeysRef.current) {
          const raw = formRef.current[key];
          dirtyFormPatch[key] = typeof raw === "boolean" ? (raw ? "true" : "false") : String(raw ?? "");
        }
        initialPatch.form = dirtyFormPatch;
      }
      if (Object.keys(initialPatch).length > 0) {
        syncDrawStateToBackend(initialPatch);
      }

      await restoreWorkflow(plan.lastPreviewRaw);
      if (!cancelled) {
        setLastPreviewHydrated(true);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [applyPendingRemoteScene]);
  useEffect(() => {
    if (!shouldPersistLastPreview({ workflowId, previewSnap, lastPreviewHydrated })) {
      return;
    }
    const serialized = workflowId && previewSnap ? serializeLastPreview(workflowId, previewSnap) : "";
    writeStored(safeBrowserStorage(), DRAW_LAST_PREVIEW_KEY, serialized);
    if (lastPersistedPreviewRef.current !== serialized) {
      lastPersistedPreviewRef.current = serialized;
      syncDrawStateToBackend({ last_preview: serialized });
    }
  }, [workflowId, previewSnap, lastPreviewHydrated]);

  // 常驻页面（r49）重新显示时让画布重新量一次尺寸
  useEffect(() => {
    if (active) excalidrawAPI?.refresh?.();
  }, [active, excalidrawAPI]);

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
  useEffect(() => {
    registrationSourceRef.current = result?.result?.registered_from || workflowId || "";
  }, [result, workflowId]);

  // 自动按需持久化修改过的配置键（非连接设置在换端口后也可恢复，设备连接只保存在当前浏览器）
  useEffect(() => {
    const storage = safeBrowserStorage();
    const backendPatch = {};
    for (const key of CONFIG_KEYS) {
      const raw = form[key];
      const val = typeof raw === "boolean" ? (raw ? "true" : "false") : String(raw ?? "");
      if (persistedRef.current[key] !== val) {
        persistedRef.current[key] = val;
        if (storage) {
          try {
            storage.setItem(`laserLab.${key}`, val);
          } catch {
            // 存储受限时保留内存态
          }
        }
        if (formHydratedRef.current && BACKEND_CONFIG_KEYS.includes(key)) {
          backendPatch[key] = val;
        }
      }
    }
    if (Object.keys(backendPatch).length > 0) {
      syncDrawStateToBackend({ form: backendPatch });
    }
  }, [form]);

  const vectorRasterFallback = drawMode === "vector" && hasSceneImages;

  // 联动查询当前材质、厚度与生成策略对应的材料库推荐功率、速度与遍数
  useEffect(() => {
    let cancelled = false;
    const mat = String(form.material || "").trim();
    const thick = String(form.thickness_mm || "").trim();
    if (!mat || !thick || !(Number(thick) > 0)) {
      setHasRec(false);
      return undefined;
    }
    // 插入图片触发光栅回退时按光栅（填充）档查推荐；切割查该厚度的切割参数，其余按刻法查雕刻档
    const recMode = vectorRasterFallback ? "raster" : drawRecommendEngravingMode(drawMode, form.vector_style);
    const query = new URLSearchParams(
      recMode === "cut"
        ? { material: mat, thickness_mm: thick, laser_mode: "cut" }
        : { material: mat, thickness_mm: thick, laser_mode: "engrave", engraving_mode: recMode },
    );
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
  }, [form.material, form.thickness_mm, drawMode, form.vector_style, vectorRasterFallback, recRefreshTick]);

  const matchedLibraryMaterial = materialMode === "library"
    ? findMaterialOption(materialLibrary, form.material)
    : null;
  const isSurfaceOnlyLibMat = Boolean(
    matchedLibraryMaterial && (matchedLibraryMaterial.thicknesses || []).length === 0 && matchedLibraryMaterial.surface,
  );
  useEffect(() => {
    if ((hasSceneImages || isSurfaceOnlyLibMat) && normalizeDrawVectorStyle(form.vector_style) === "cut") {
      setForm((current) => ({ ...current, vector_style: "outline" }));
    }
    if (hasSceneImages && selectedOnly) {
      setSelectedOnly(false);
    }
  }, [hasSceneImages, isSurfaceOnlyLibMat, form.vector_style, selectedOnly]);

  // r40：把当前填充或切割设置存进材料库条目（只改图层设置；扫描偏移与机器加速度不随材料保存）
  const saveFillToMaterial = async () => {
    if (!materialLayer) return;
    const isCutStyle = drawIsCut(drawMode, form.vector_style);
    const kindLabel = isCutStyle ? "切割" : "填充";
    const payload = materialLayerPayload(
      fillSettings,
      isCutStyle ? { withFill: false, withCut: true } : { withFill: true },
    );
    if (payload.error) {
      showAlert(payload.error, { title: `${kindLabel}设置不对`, tone: "warning" });
      return;
    }
    const ok = await showConfirm(
      `把当前${kindLabel}设置存进材料库「${materialLayer.label}」？\n以后选这个材料会自动带出；双向扫描偏移与机器加速度跟机器走，不随材料保存。`,
      { title: "保存到材料库", confirmText: "保存", cancelText: "取消" },
    );
    if (!ok) return;
    const res = await postJson("/api/material-lab/manage", materialLayerSaveRequest(materialLayer, payload));
    if (res.ok && res.data?.success) {
      setStatus(`已把${kindLabel}设置存进材料库「${materialLayer.label}」`);
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
    vectorRasterFallback,
    // 矢量看填充设置与选区，光栅扫描（含插入图片触发光栅回退）看过扫描 / 双向 / 扫描偏移（自动过扫描随机器加速度变）
    fill: drawMode === "vector" && !vectorRasterFallback
      ? fillSettingsSignature(fillSettings, drawVectorLayerFlags(form.vector_style)) + JSON.stringify(motionPayload) + (selectedOnly ? `|sel:${selectionKey}` : "")
      : drawMode === "raster" || vectorRasterFallback
        ? rasterScanSignature(fillSettings) + JSON.stringify(motionPayload)
        : "",
  }, engraveRoute);
  const previewStale = Boolean(workflowId) && isDrawPreviewStale(previewSnap, {
    sceneRev,
    formSig: currentFormSig,
  });

  const web = result?.result?.web || {};
  const lab = result?.result?.draw_lab || {};
  const jobSummary = lab.job_summary || null;
  const canActOnWorkflow = Boolean(workflowId);
  const hostReady = drawTargetReady(form);
  const nextActions = Array.isArray(result?.result?.next_actions) ? result.result.next_actions : [];
  const summaryAllowsSend = summaryAllowsConfirmSend(jobSummary, nextActions);
  const canConfirmSend = canActOnWorkflow && !previewStale && hostReady && summaryAllowsSend;

  function updateForm(key, value) {
    if (key === "connection_mode") {
      setConnectionMode(value);
      return;
    }
    if (key === "port") {
      setSerialPort(value);
      return;
    }
    if (key === "baudrate") {
      setBaudrate(value);
      return;
    }
    if (key === "network_host") {
      setNetworkHost(value);
      return;
    }
    if (key === "network_telnet_port") {
      setTelnetPort(value);
      return;
    }
    dirtyFormKeysRef.current.add(key);
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
      // 只在挂载时跑一次：读取加载完成那一刻的表单（加载期间用户改过材料时不被首帧旧值覆盖）
      const currentForm = formRef.current;
      const restored = restoreMaterialSelectState({
        materials: loaded.materials,
        savedMaterial: currentForm.material,
        savedThickness: currentForm.thickness_mm,
      });
      if (loaded.ok) {
        materialLoadedOnceRef.current = true;
      }
      setMaterialLibrary(restored.materials);
      setMaterialMode(restored.mode);
      setMaterialSelectValue(restored.selectValue);
      setForm((current) => ({
        ...current,
        material: restored.material || currentForm.material,
        thickness_mm: restored.thickness_mm || currentForm.thickness_mm,
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
    const unsubscribeMaterials = subscribeMaterialsUpdated(refreshMaterialLibrary);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
      unsubscribeMaterials();
    };
  }, []);

  function handleMaterialSelectChange(selectValue) {
    dirtyFormKeysRef.current.add("material");
    dirtyFormKeysRef.current.add("thickness_mm");
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
    dirtyFormKeysRef.current.add("material");
    dirtyFormKeysRef.current.add("thickness_mm");
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
      sceneDirtyRef.current = true;
      excalidrawAPI.updateScene({
        elements: parsed.elements,
        appState: parsed.appState,
      });
      setTimeout(() => excalidrawAPI.scrollToContent?.(parsed.elements, { fitToContent: true }), 40);
      setSceneRev(computeSceneRevision(parsed.elements));
      setHasSceneImages(hasSceneImageElements(parsed.elements));
      setStatus(`已导入场景：${file.name}`);
    } catch (error) {
      const msg = error?.message || String(error);
      setStatus(`导入场景失败：${msg}`);
      void showAlert(msg, { title: "导入失败", tone: "error" });
    }
  }

  function startDrawJobPolling(targetWorkflowId, inFlight) {
    let pollState = { id: targetWorkflowId, kind: "workflow", status: "sending", progress: null };
    const stop = startJobPolling(
      (signal) => postJson("/api/workflow", { action: "status", workflow_id: targetWorkflowId }, { signal }),
      (res) => {
        pollState = applyJobPoll(pollState, targetWorkflowId, "workflow", res);
        const terminal = TERMINAL_STATUSES.has(pollState.status);
        if (terminal) {
          stop();
          inFlightJobsRef.current.delete(targetWorkflowId);
        }
        // 换预览只切换展示，原作业仍跟踪到终态，供隐藏画板急停。
        if (workflowIdRef.current !== targetWorkflowId) return;
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
        if (terminal) {
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
    inFlight.stop = stop;
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
    const exportElements = forLaser
      ? prepareRasterExportElements(activeElements, drawInkExportMode(drawMode, form.image_mode))
      : activeElements;
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
    const modeSnapshot = drawMode;
    // 矢量带整套填充设置；光栅扫描带扫描子集（过扫描、双向、扫描偏移表）与转换方式；只刻选中的图形只在矢量下可用
    const fillPayload = modeSnapshot === "vector" && !vectorRasterFallback
      ? fillSettingsPayload(fillSettings, drawVectorLayerFlags(form.vector_style))
      : modeSnapshot === "raster" || vectorRasterFallback
        ? rasterScanPayload(fillSettings)
        : {};
    const imagePayload = modeSnapshot === "raster" ? drawRasterImagePayload(form) : {};
    const selection = selectedOnly && modeSnapshot === "vector" && !vectorRasterFallback
      ? selectedElementIds(excalidrawAPI?.getAppState?.())
      : [];
    if (imagePayload.error) {
      setStatus(`转图片设置有误：${imagePayload.error}`);
      void showAlert(imagePayload.error, { title: "转图片设置不对", tone: "warning" });
      return;
    }
    if (selectedOnly && modeSnapshot === "vector" && selection.length === 0) {
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
    previewActionSeqRef.current += 1;
    setBusy(true);
    setStatus("导出画板中...");
    try {
      const { blob, scene, activeElements } = await exportScene({ forLaser: true });
      const revSnapshot = computeSceneRevision(activeElements);
      const imageDataUrl = await blobToDataUrl(blob);
      setStatus("上传并生成预览中...");
      const payload = {
        image_data_url: imageDataUrl,
        scene_json: scene,
        material: form.material,
        thickness_mm: Number(form.thickness_mm) || 3.0,
        width_mm: drawSizeValue(form.width_mm),
        height_mm: drawSizeValue(form.height_mm),
        mode: modeSnapshot,
        ...(modeSnapshot === "vector" ? { vector_style: normalizeDrawVectorStyle(form.vector_style) } : {}),
        ...(drawIsCut(modeSnapshot, form.vector_style) ? { laser_mode: "cut" } : {}),
        ...imagePayload,
        ...fillPayload,
        ...motionPayload,
        ...(selection.length ? { selected_element_ids: selection } : {}),
        ...drawConnectionPayload(form),
        ...(manual ? { manual_params: manual } : {}),
      };
      if (modeSnapshot === "outline") {
        const simplifyRaw = String(form.vector_simplify_factor ?? "").trim();
        payload.vector_simplify_factor = simplifyRaw === "" ? 3 : Number(simplifyRaw);
      }
      const res = await postJson("/api/draw/preview", payload);
      const data = res.data || { success: false, result: `HTTP ${res.status}` };
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
          artifacts.route_note,
          artifacts.selection_note,
          fillResultNote(artifacts.fill_options, artifacts.removed_duplicates, artifacts.cut_options),
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
    if (inFlightJobsRef.current.has(workflowId)) return;
    setBusy(true);
    setStatus(offlineSd ? "提交脱机离线加工..." : "提交确认发送...");
    const inFlight = { jobId: workflowId, target: estopTargetArgs(form) };
    inFlightJobsRef.current.set(workflowId, inFlight);
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
      if (isApprovalRequiredResponse(res)) {
        setResult(data);
        setConfirmOpen(false);
        setStatus(approvalPendingMessage(res, "发送任务"));
        return;
      }
      setResult(data);
      setConfirmOpen(false);
      const safeData = res.ok ? data : { ...data, success: false };
      setRepeatSend((current) => applyConfirmSendResponse(current, safeData, safeBrowserStorage()));
      if (isNestedSenderAccepted(safeData)) {
        const fp = String(result?.result?.draw_lab?.content_fingerprint || repeatSend.currentFingerprint || "").trim().toLowerCase();
        if (/^[0-9a-f]{64}$/.test(fp)) {
          localSessionAcceptedRef.current = true;
          syncDrawStateToBackend({ last_accepted_fingerprint: fp });
        }
      }
      if (res.ok && data.success) {
        setStatus(data.result?.speech || (offlineSd ? "已启动 SD 脱机加工，正在同步进度..." : "已开始雕刻下发，正在同步进度..."));
        startDrawJobPolling(workflowId, inFlight);
      } else if (res.status === 0) {
        setStatus("发送结果未知，正在查询原任务。顶栏急停仍可按原设备发送。");
        startDrawJobPolling(workflowId, inFlight);
      } else {
        setStatus((typeof data.result === "string" && data.result) || "发送被拒绝");
      }
    } catch (error) {
      setStatus(`请求失败：${error.message || error}`);
    } finally {
      if (!inFlight.stop) inFlightJobsRef.current.delete(workflowId);
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
    // 走边框会实际移动机器，单击不得直接执行（2026-11-05 用户决定）；一律不出光（r47）
    const outline = framingShape === "hull" ? framingOutline(result?.result?.artifacts) : null;
    const confirmed = await showConfirm(framingConfirmText(form, width, height, outline?.length || 0, registered), {
      title: "确认走边框（机器将移动）",
      tone: "warning",
      confirmText: "开始走边框",
      cancelText: "取消",
    });
    if (!confirmed) {
      setStatus("已取消走边框，机器未移动");
      return;
    }
    setBusy(true);
    setStatus("正在走边框（激光关闭，只移动激光头）…顶栏急停可随时停止");
    try {
      // 默认不出光；附带 relative_to_head: true 以当前激光头位置为左下角起点（与正式雕刻同一参照）
      const res = await postJson("/api/device/framing", buildDrawFramingPayload(form, width, height, outline, frame));
      setStatus(formatFramingStatus(res, width, height));
    } catch (e) {
      setStatus(`走边框请求失败：${e.message || e}`);
    } finally {
      setBusy(false);
    }
  }

  // 顶栏常驻急停（2026-11-05）：只走 `/api/safe-action` 实时 0x18，绝不退化为 workflow cancel。
  // 顶栏已先停后端登记的任务；轮到本页说明任务已不在登记表（如刚全量发送完、GRBL 仍在执行缓冲）
  // 或任务通道挂起，此时按任务编号失败后改用本页设备目标新开连接写 0x18（`usePageEmergencyStop`）。
  // 隐藏画板只补发给尚在提交或跟踪中的原设备，当前页面仍可同时处理自己的设备。
  usePageEmergencyStop(
    () => drawEstopContexts({ active, target: estopTargetArgs(form), inFlight: inFlightJobsRef.current.values() }),
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
    // 对位请求返回前重新生成了画板预览时，迟到的结果指向旧任务，不能顶替新任务
    const adopted = adoptRegisteredDrawRecord(record, registrationSourceRef.current);
    if (adopted.error) {
      setStatus(adopted.error);
      void showAlert(adopted.error, { title: "对位结果不能用于画板", tone: "warning" });
      return;
    }
    const guard = checkDerivedDrawAdoption({
      workflow: record,
      currentWorkflowId: registrationSourceRef.current || workflowId,
      previewSnap,
      previewStale,
    });
    if (!guard.ok) {
      setStatus(guard.reason);
      void showAlert(guard.reason, { title: "对位结果已过期", tone: "warning" });
      return;
    }
    previewActionSeqRef.current += 1;
    setWorkflowId(adopted.workflowId);
    setResult(adopted.result);
    setPreviewSnap(guard.inheritedSnap);
    setConfirmOpen(false);
    setRepeatSend((current) => applyPreviewFingerprintSuccess(current, adopted.fingerprint));
    setStatus(adopted.status);
  }

  async function cancelWorkflow() {
    if (!workflowId) return;
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
    setBusy(true);
    setStatus("提交取消...");
    try {
      const res = await postJson("/api/workflow", { action: "cancel", workflow_id: workflowId });
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

  // 一键清空画布（2026-11-18 用户要求）：先确认；清空前的画布记在 ref 里，按钮随即变成“撤销清空”，
  // 在画布上画了新东西后撤销失效。（Excalidraw 0.18 经 updateScene 的改动进不了它自己的撤销历史，
  // 实测 Cmd/Ctrl+Z 找不回，所以自己保存。）清空后本机保存的画布随 onChange 一起清掉，
  // 已生成的预览因画布变化自动标为过期，不会误发旧刀路
  async function clearCanvas() {
    if (!excalidrawAPI) return;
    const live = (excalidrawAPI.getSceneElements?.() || []).filter((el) => !el.isDeleted);
    if (live.length === 0) {
      setStatus("画布已经是空的");
      return;
    }
    const confirmed = await showConfirm(
      `清空画布上的全部 ${live.length} 个图形？清空后可以点工具栏的「撤销清空」找回。`,
      { title: "清空画布", tone: "warning", danger: true, confirmText: "清空", cancelText: "取消" },
    );
    if (!confirmed) return;
    clearedSceneRef.current = live;
    sceneDirtyRef.current = true;
    excalidrawAPI.updateScene({ elements: live.map((el) => newElementWith(el, { isDeleted: true })) });
    setHasSceneImages(false);
    setCanUndoClear(true);
    setStatus("画布已清空，点工具栏「撤销清空」可找回");
  }

  function undoClearCanvas() {
    const cleared = clearedSceneRef.current;
    if (!excalidrawAPI || !cleared) return;
    clearedSceneRef.current = null;
    sceneDirtyRef.current = true;
    setCanUndoClear(false);
    const restored = cleared.map((el) => newElementWith(el, { isDeleted: false }));
    excalidrawAPI.updateScene({ elements: restored });
    setHasSceneImages(hasSceneImageElements(restored));
    setStatus(`已恢复清空前的 ${cleared.length} 个图形`);
  }

  async function downloadScene() {
    try {
      const { scene } = await exportScene();
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

  // 采用加工反馈的调参建议：换成新生成的预览任务，尚未发送（若画布或参数已改，拦截并用原快照沿用签名）
  const adoptFeedbackWorkflow = (workflow) => {
    const guard = checkDerivedDrawAdoption({
      workflow,
      currentWorkflowId: workflowId,
      previewSnap,
      previewStale,
    });
    if (!guard.ok) {
      setStatus(guard.reason);
      void showAlert(guard.reason, { title: "不能采用反馈调参", tone: "warning" });
      return;
    }
    previewActionSeqRef.current += 1;
    setWorkflowId(workflow.workflow_id);
    setResult({ success: true, result: workflow });
    setPreviewSnap(guard.inheritedSnap);
    setConfirmOpen(false);
    setRepeatSend((current) => applyPreviewFingerprintSuccess(current, workflow.draw_lab?.content_fingerprint || ""));
    setStatus("调参预览已生成，尚未发送");
  };

  const dockConn = connectionSummaryView(form, connectionStatus);
  const dockMaterialText = `${form.material || "未选材料"} · ${form.thickness_mm === "surface" ? "表面通用" : `${form.thickness_mm || "-"}mm`}`;
  const scrollToCard = (cardId) => {
    const el = typeof document !== "undefined" ? document.querySelector(`[data-wb-card="${cardId}"]`) : null;
    el?.scrollIntoView?.({ behavior: "smooth", block: "start" });
  };
  const dockNeedsConn = canActOnWorkflow && !previewStale && summaryAllowsSend && !hostReady;
  const dockPreviewSize = (() => {
    const w = Number(jobSummary?.actual_width_mm ?? result?.result?.artifacts?.width_mm);
    const h = Number(jobSummary?.actual_height_mm ?? result?.result?.artifacts?.height_mm);
    return Number.isFinite(w) && w > 0 && Number.isFinite(h) && h > 0 ? `${w.toFixed(0)}×${h.toFixed(0)}mm` : "";
  })();
  const dockPreviewSeconds = Number(jobSummary?.estimated_seconds ?? result?.result?.artifacts?.time_estimate?.estimated_seconds);
  const dockSummaryText = previewStale
    ? "预览已过期 · 查看刀路 ↓"
    : [
        dockPreviewSize,
        Number.isFinite(dockPreviewSeconds) && dockPreviewSeconds > 0 ? `约 ${Math.round(dockPreviewSeconds)}s` : "",
        "查看刀路/走边框 ↓",
      ].filter(Boolean).join(" · ");

  return (
    <div className="app-shell ob-page ob-draw-page">

      {/* 大画布占满整行，下方三张卡并排：材料与尺寸、设备、预览与发送（技术书 §9.5.5 B、r25；支持自由缩放/拖动/折叠） */}
      <section className="workspace ob-draw">
        <WorkbenchCanvas
          defaultLayout={DRAW_DEFAULT_LAYOUT}
          storageKey={DRAW_STORAGE_KEY}
          active={active}
          toolbarStart={
            <>
              <div className="topbar-actions">
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
                {canUndoClear ? (
                  <button
                    type="button"
                    id="draw-undo-clear"
                    className="btn btn-ghost draw-clear-btn"
                    onClick={undoClearCanvas}
                    disabled={busy || !excalidrawAPI}
                    title="恢复刚才清空的全部图形"
                  >
                    <Undo2 size={14} aria-hidden="true" />
                    撤销清空
                  </button>
                ) : (
                  <button
                    type="button"
                    id="draw-clear-canvas"
                    className="btn btn-ghost draw-clear-btn"
                    onClick={clearCanvas}
                    disabled={busy || !excalidrawAPI}
                    title="清空画布上的全部图形（先确认，清空后可撤销）"
                  >
                    <Trash2 size={14} aria-hidden="true" />
                    清空画布
                  </button>
                )}
              </div>
              <div className="ob-canvasbar">
                <span className="draw-note" role="status">{status}</span>
              </div>
              {drawSyncWarning && <p className="draw-note" role="alert">{drawSyncWarning}</p>}
            </>
          }
        >
        <WbCard id="board" className="hud-card ob-card canvas-panel ob-board" label="画布">
          <FoldHeader id="draw-board" title="创作画布" />
          <div className="ob-board-stage" ref={boardStageRef}>
            <Excalidraw
              excalidrawAPI={setExcalidrawAPI}
              theme="dark"
              langCode={EXCALIDRAW_LANG_CODE}
              initialData={initialScene ? {
                elements: initialScene.elements,
                appState: initialScene.appState,
                files: initialScene.files,
              } : undefined}
              onChange={(elements, appState, files) => {
                if (applyingRemoteSceneRef.current) return;
                const next = snapshotDrawScene(elements, appState, files);
                // Excalidraw 先给 API，initialData 恢复后才发第一次 onChange。
                if (!canvasReadyRef.current) {
                  canvasReadyRef.current = true;
                  appliedSceneRef.current = next;
                  if (applyPendingRemoteScene(excalidrawApiRef.current)) return;
                }
                if (pendingRemoteSceneRef.current) return;
                const changes = drawSceneChanges(appliedSceneRef.current, next);
                appliedSceneRef.current = next;
                if (changes.content) sceneDirtyRef.current = true;
                if (changes.content || changes.view) {
                  pendingSceneRef.current = {
                    elements, appState, files,
                    syncRemote: changes.content || Boolean(pendingSceneRef.current?.syncRemote),
                  };
                  clearTimeout(sceneSaveTimerRef.current);
                  sceneSaveTimerRef.current = setTimeout(() => flushSceneRef.current(), 600);
                }
                const nextRev = next.sceneRev;
                // 清空后又画了东西（或已恢复）：撤销清空作废
                if (clearedSceneRef.current && elements.some((el) => !el.isDeleted)) {
                  clearedSceneRef.current = null;
                  setCanUndoClear(false);
                }
                setSceneRev((prev) => (prev === nextRev ? prev : nextRev));
                const nextHasImages = hasSceneImageElements(elements);
                setHasSceneImages((prev) => (prev === nextHasImages ? prev : nextHasImages));
                const nextSelection = selectedElementIds(appState).join(",");
                setSelectionKey((prev) => (prev === nextSelection ? prev : nextSelection));
              }}
            />
          </div>
          <div className="draw-board-action-bar" id="draw-board-action-bar" role="toolbar" aria-label="画布底部快捷操作">
            <button
              type="button"
              id="draw-dock-material"
              className="btn btn-ghost draw-dock-chip"
              onClick={() => scrollToCard("draw-material")}
              title="点击滚动定位到下方「材料与尺寸」卡"
            >
              <Layers size={14} aria-hidden="true" />
              <span>{dockMaterialText}</span>
            </button>
            <button
              type="button"
              id="draw-dock-connection"
              className="btn btn-ghost draw-dock-chip"
              onClick={onOpenConnection}
              title="点击打开统一设备连接面板"
            >
              <span className={`conn-summary-dot is-${dockConn.statusTone}`} aria-hidden="true" />
              <span>{dockConn.modeLabel} · {dockConn.targetText} · {dockConn.statusText}</span>
            </button>
            <span
              className="draw-dock-origin"
              id="draw-dock-origin"
              title={drawOriginNote(result?.result)}
            >
              <OriginBadge origin={result?.result?.registration ? "point2" : "lower_left"} />
              <span>{result?.result?.registration ? "第 2 标记起点" : "左下角起点"}</span>
            </span>
            {canActOnWorkflow ? (
              <button
                type="button"
                id="draw-dock-preview-summary"
                className={`btn btn-ghost draw-dock-chip draw-dock-summary${previewStale ? " is-stale" : ""}`}
                onClick={() => scrollToCard("draw-send")}
                title={`${drawOriginNote(result?.result)}；点击向下滚动查看刀路预览图与走边框`}
              >
                <span>{dockSummaryText}</span>
              </button>
            ) : null}
            <div className="draw-dock-spacer" />
            <button
              type="button"
              id="draw-dock-preview-btn"
              className="primary-button draw-dock-btn"
              onClick={generatePreview}
              disabled={busy || !excalidrawAPI}
            >
              <UploadCloud size={16} aria-hidden="true" />
              <span>{busy ? "正在解算..." : workflowId ? "重新生成预览" : "生成预览"}</span>
            </button>
            <button
              type="button"
              id="draw-dock-send-btn"
              className={dockNeedsConn ? "btn btn-ghost draw-dock-btn" : "danger-button draw-dock-btn"}
              onClick={() => {
                if (dockNeedsConn) {
                  if (onOpenConnection) onOpenConnection();
                  else openSendConfirmation();
                } else {
                  openSendConfirmation();
                }
              }}
              disabled={busy || (!canConfirmSend && !dockNeedsConn)}
              title={
                !canActOnWorkflow
                  ? "请先点击「生成预览」"
                  : previewStale
                    ? "画布或参数已修改，请先重新生成预览"
                    : dockNeedsConn
                      ? `${drawTargetMissingText(form)}（点击打开设备连接面板）`
                      : !summaryAllowsSend
                        ? "当前不可发送：请检查材料厚度参数与确认权限"
                        : undefined
              }
            >
              <Send size={16} aria-hidden="true" />
              <span>
                {previewStale
                  ? "参数已改 · 请先重算"
                  : dockNeedsConn
                    ? "请先配置连接"
                    : "确认发送"}
              </span>
            </button>
          </div>
        </WbCard>

        <DrawMaterialCard
          form={form}
          updateForm={updateForm}
          drawMode={drawMode}
          engraveRoute={engraveRoute}
          setEngraveRoute={setEngraveRoute}
          engraveRouteSaving={engraveRouteSaving}
          engraveRouteReadOnly={engraveRouteReadOnly}
          applyImageQualityPreset={(key) => {
            const next = applyImagePreset(key);
            if (!next) return;
            dirtyFormKeysRef.current.add("image_mode");
            dirtyFormKeysRef.current.add("line_interval_scale");
            setForm((current) => ({ ...current, image_mode: next.imageMode, line_interval_scale: next.intervalScale }));
          }}
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
          hasSceneImages={hasSceneImages}
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
          connectionStatus={connectionStatus}
          onOpenConnection={onOpenConnection}
          busy={busy}
        />

        <DrawSendCard
          form={form}
          busy={busy}
          excalidrawAPI={excalidrawAPI}
          workflowId={workflowId}
          result={result}
          web={web}
          jobSummary={jobSummary}
          previewStale={previewStale}
          hostReady={hostReady}
          canActOnWorkflow={canActOnWorkflow}
          canConfirmSend={canConfirmSend}
          summaryAllowsSend={summaryAllowsSend}
          framingShape={framingShape}
          setFramingShape={setFramingShape}
          motionPayload={motionPayload}
          setStatus={setStatus}
          generatePreview={generatePreview}
          openSendConfirmation={openSendConfirmation}
          runFraming={runFraming}
          cancelWorkflow={cancelWorkflow}
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
          sdAvailable={!isSerial(form) && Boolean(connectionStatus?.isFluidnc)}
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
