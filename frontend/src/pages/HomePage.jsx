import React, { useState, useEffect, useRef, useMemo } from "react";
import {
  Type,
  FileCode,
  Image as ImageIcon,
  PenTool,
  ArrowUpRight,
} from "lucide-react";
import { DEFAULT_READ_TIMEOUT_MS, getJson, postJson, fetchPreviewBlobUrl, fetchPreviewText, resolveApiUrl } from "../api/client.js";
import { isRemoteMode, readAuthToken } from "../authSession.js";
import { MaterialModal } from "../components/MaterialModal.jsx";
import { ConsoleDrawer } from "../components/ConsoleDrawer.jsx";
import { FeedbackPanel } from "../components/FeedbackPanel.jsx";
import { approvalPendingMessage, isApprovalRequiredResponse } from "../approvalState.js";
import { extractWorkflowInfo, TERMINAL_STATUSES } from "../state/statusNormalize.js";
import {
  DEFAULT_IMAGE_EXTRAS,
  checkDerivedWorkbenchAdoption,
  imageModeFields,
  imageParamsPayload,
  isWorkbenchPreviewStale,
  manualParamsPayload,
  materialRecommendationModes,
  resolveTextStyle,
  workbenchConfirmSendPrompt,
  workbenchFeedbackInputs,
  workbenchInputSignature,
  sendProgressForUnknownResult,
  workbenchCanonicalTarget,
  workbenchConnectionFields,
  workbenchResumeSendPrompt,
  normalizeWorkbenchText,
  recentTaskLabel,
  textBorderPayload,
  workbenchTextPayload,
} from "../workbenchParams.js";
import { framingRequest, framingShapeText, readFramingShape } from "../aiPreviewState.js";
import { drawConnectionPayload, drawTargetLabel, drawTargetReady } from "../drawConnection.js";
import { exportFile } from "../saveFile.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { estopTargetArgs, visibleEstopContext } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import { canResumeJob, getResumeStartLine, sendProgressFromResult } from "../estopResumeState.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { applyImagePreset } from "../qualityPresets.js";
import { effectiveTextStyle } from "../engraveRoute.js";
import { useEngraveRoute } from "../useEngraveRoute.js";
import { useTextOrigin } from "../useTextOrigin.js";
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
  readMachineMotionStore,
  resolveMachineMotionForTarget,
  writeFillSettings,
  writeMachineMotion,
} from "../fillOptions.js";
import { WbCard, WorkbenchCanvas } from "../components/WorkbenchCanvas.jsx";
import { WorkbenchLinkCard } from "../components/workbench/WorkbenchLinkCard.jsx";
import { WorkbenchTextTool } from "../components/workbench/WorkbenchTextTool.jsx";
import { WorkbenchRecentTasks } from "../components/workbench/WorkbenchRecentTasks.jsx";
import { WorkbenchImageTool } from "../components/workbench/WorkbenchImageTool.jsx";
import { WorkbenchGcodeTool } from "../components/workbench/WorkbenchGcodeTool.jsx";
import { WorkbenchMaterialCard } from "../components/workbench/WorkbenchMaterialCard.jsx";
import { WorkbenchMonitorCard } from "../components/workbench/WorkbenchMonitorCard.jsx";
import { notifyMaterialsUpdated, pickThicknessForMaterial, subscribeMaterialsUpdated } from "../materialSelectState.js";
import {
  DEVICE_LOG_EVENT,
  MACHINE_MOTION_EVENT,
  WORKSPACE_LIMITS_EVENT,
  probeControllerConnection,
  readBufferedDeviceLogs,
  readWorkspaceLimitsStore,
  resolveSoftLimitSendWarning,
  resolveWorkspaceLimitsForTarget,
  syncHardwareMachineFeatures,
  syncHardwareWorkspaceLimits,
} from "../deviceConnection.js";
import { useDeviceConnection } from "../useDeviceConnection.js";

export async function loadWorkbenchPreviewUrl(imageFile, { remote, fetchBlobUrl, resolveUrl }) {
  if (!imageFile) return "";
  if (remote) {
    return await fetchBlobUrl(imageFile);
  }
  return resolveUrl(`/preview?path=${encodeURIComponent(imageFile)}`);
}

function readLocal(key, fallback) {
  try {
    const val = globalThis.localStorage?.getItem(key);
    return val ?? fallback;
  } catch {
    return fallback;
  }
}

function writeLocal(key, value) {
  try {
    globalThis.localStorage?.setItem(key, String(value));
  } catch {
    // 存储受限时仅在当前会话生效
  }
}

export function HomePage({
  onNavigate,
  connectionStatus,
  setConnectionStatus,
  onOpenConnection = null,
  mcpStatus: externalMcpStatus = null,
  activeJobs = [],
}) {
  // Connection state（连接参数读写收口在 useDeviceConnection，全站共用同一份存储）
  const {
    connectionMode,
    serialPort,
    baudrate,
    networkHost,
    setNetworkHost,
    telnetPort,
  } = useDeviceConnection({ numeric: true });
  const deviceConnection = { connectionMode, serialPort, baudrate, networkHost, telnetPort };

  // Active Tool Mode
  const [activeTool, setActiveTool] = useState("text"); // text, image, gcode

  // Tool specific inputs（图片与已有 G-code 独立存放，互不串文件）
  const [textContent, setTextContent] = useState(
    () => normalizeWorkbenchText(readLocal("laser-forge-text", "MOSS")) || "MOSS"
  );
  // 外框 / 铭牌（2026-11-26）：形状、双线与木牌尺寸，按本机记住
  const [textBorder, setTextBorder] = useState(() => {
    try {
      const saved = JSON.parse(readLocal("laser-forge-text-border", "null"));
      if (saved && typeof saved === "object") {
        return {
          shape: String(saved.shape || "none"),
          double: Boolean(saved.double),
          boardWidthMm: String(saved.boardWidthMm ?? ""),
          boardHeightMm: String(saved.boardHeightMm ?? ""),
        };
      }
    } catch {
      // 记录损坏时回到无外框
    }
    return { shape: "none", double: false, boardWidthMm: "", boardHeightMm: "" };
  });
  const [fontSizeMm, setFontSizeMm] = useState(
    () => Number(readLocal("laser-forge-font-size", "12") || 12)
  );
  const [textStyle, setTextStyle] = useState(
    () => resolveTextStyle(readLocal("laser-forge-text-style", "outline")).style
  );
  // 对刀起点（r48）：后端全局设置，工作台与语音共用；手机连电脑时只改本次
  const { origin: textOrigin, setOrigin: setTextOrigin } = useTextOrigin();
  // 2026-11-16：矢量填充与走刀设置（对齐 LightBurn），与创意画板共用一份本机设置
  const [fillSettings, setFillSettings] = useState(() => readFillSettings());
  // r40：当前材料条目随材料保存的填充 / 切割设置（选材料、厚度、刻法后载入，可改，“保存到材料库”写回）
  const [materialLayer, setMaterialLayer] = useState(null);
  const [materialApplied, setMaterialApplied] = useState([]);
  const materialLayerKeyRef = useRef("");
  const sendInFlightRef = useRef(false);
  const workflowAssetSeqRef = useRef(0);
  const updateFillSettings = (next) => {
    setFillSettings(next);
    writeFillSettings(localSettingsToStore(next, readFillSettings(), materialApplied));
  };
  const [materials, setMaterials] = useState([]);
  const [selectedMaterial, setSelectedMaterial] = useState(
    () => readLocal("laser-forge-mat", "椴木") || "椴木"
  );
  const [selectedThickness, setSelectedThickness] = useState(
    () => readLocal("laser-forge-thick", "3") || "3"
  );
  const currentMaterialObj = materials.find((m) => m.name === selectedMaterial);
  const availableThicknesses = currentMaterialObj?.thicknesses || [1, 2, 3, 5];
  const surfaceOnlyMaterial = Boolean(currentMaterialObj && availableThicknesses.length === 0 && currentMaterialObj.surface);

  // 雕刻路线全局开关（2026-11-17 技术书 r45，与创作画板、语音同一个值）：转图片时描边 / 排线按实心，
  // 仅表面通用材料时外框切割回落为轮廓描边；用户原来选的刻法照样记着，切回即恢复
  const {
    route: engraveRoute,
    setRoute: setEngraveRoute,
    saving: engraveRouteSaving,
    readOnly: engraveRouteReadOnly,
  } = useEngraveRoute();
  const textStyleEffective = effectiveTextStyle(textStyle, engraveRoute, { surfaceOnly: surfaceOnlyMaterial });
  const textUsesFill = resolveTextStyle(textStyleEffective).textFill !== "outline";
  // 外框切割另带切割设置（切缝补偿、桥位、引线、过切，2026-11-16 对齐 LightBurn）
  const textIsCut = resolveTextStyle(textStyleEffective).laserMode === "cut";
  const currentConnectionTarget = {
    connection_mode: connectionMode,
    port: serialPort,
    baudrate: String(baudrate),
    network_host: networkHost,
    network_telnet_port: String(telnetPort),
  };
  // 握手读到的机器加速度 / 最大速度（按当前连接目标取值）：工时估算与自动过扫描用
  const [machineMotionStore, setMachineMotionStore] = useState(() => readMachineMotionStore());
  useEffect(() => {
    const onMotion = (e) => {
      if (e?.detail) setMachineMotionStore(e.detail);
    };
    window.addEventListener(MACHINE_MOTION_EVENT, onMotion);
    return () => window.removeEventListener(MACHINE_MOTION_EVENT, onMotion);
  }, []);
  const machineMotion = resolveMachineMotionForTarget(machineMotionStore, currentConnectionTarget);
  // 当前连接目标的有效雕刻行程（手填 → 固件 → 默认 100mm）
  const [workspaceLimitsStore, setWorkspaceLimitsStore] = useState(() => readWorkspaceLimitsStore());
  useEffect(() => {
    const onLimits = (e) => {
      if (e?.detail) setWorkspaceLimitsStore(e.detail);
    };
    window.addEventListener(WORKSPACE_LIMITS_EVENT, onLimits);
    return () => window.removeEventListener(WORKSPACE_LIMITS_EVENT, onLimits);
  }, []);
  const effectiveWorkspaceLimits = resolveWorkspaceLimitsForTarget(workspaceLimitsStore, currentConnectionTarget);
  const maxWidthMm = effectiveWorkspaceLimits.x_travel_mm;
  const maxHeightMm = effectiveWorkspaceLimits.y_travel_mm;
  // 走边框走法：外接矩形 / 沿外轮廓（橡皮筋），与 AI 预览窗、画板共用
  const [framingShape, setFramingShape] = useState(() => readFramingShape());
  const motionPayload = machineMotionPayload(fillSettings, machineMotion);

  const [uploadedImagePath, setUploadedImagePath] = useState("");
  const [uploadedImageName, setUploadedImageName] = useState("");
  const [uploadedImageThumb, setUploadedImageThumb] = useState("");
  const [imageWidthMm, setImageWidthMm] = useState("");
  const [imageHeightMm, setImageHeightMm] = useState("");
  const [imageMode, setImageMode] = useState("auto");
  // 2026-11-16（r38）LightBurn 对齐的图片高级项：反相、直通、点宽补偿、网点网格 / 角度、灰度最小功率
  const [imageExtras, setImageExtras] = useState(DEFAULT_IMAGE_EXTRAS);
  const setImageExtra = (key, value) => setImageExtras((prev) => ({ ...prev, [key]: value }));
  const imageFields = imageModeFields(imageMode);
  // 2026-11-16（技术书 r41）质量预设：只填转换方式与行间距倍数，改了其中一项就显示“自定义”
  const applyImageQualityPreset = (key) => {
    const next = applyImagePreset(key);
    if (!next) return;
    setImageMode(next.imageMode);
    setImageExtra("intervalScale", next.intervalScale);
  };

  const [uploadedGcodePath, setUploadedGcodePath] = useState("");
  const [uploadedGcodeName, setUploadedGcodeName] = useState("");

  // Material and Parameter state
  const [powerPercent, setPowerPercent] = useState(50);
  const [recommendedFeedRate, setRecommendedFeedRate] = useState(1800);
  const [recommendedPasses, setRecommendedPasses] = useState(1);
  const [hasRecommendation, setHasRecommendation] = useState(true);
  const [recRefreshTick, setRecRefreshTick] = useState(0);
  const selectedMaterialRef = useRef(selectedMaterial);
  const selectedThicknessRef = useRef(selectedThickness);
  selectedMaterialRef.current = selectedMaterial;
  selectedThicknessRef.current = selectedThickness;
  // n73：这次自定义功率 / 速度 / 遍数（只影响这次生成的刀路，不写回材料库）
  const [customParams, setCustomParams] = useState(false);
  const [customPower, setCustomPower] = useState("50");
  const [customFeed, setCustomFeed] = useState("1200");
  const [customPasses, setCustomPasses] = useState("1");

  // Workflow / Output / Preview state
  const [currentWorkflow, setCurrentWorkflow] = useState(null);
  const [previewSignature, setPreviewSignature] = useState("");
  const [lastSentSha256, setLastSentSha256] = useState(
    () => readLocal("laser-forge-last-sent-sha256", "") || ""
  );
  const [previewUrl, setPreviewUrl] = useState("");
  const [timeEstimate, setTimeEstimate] = useState(null);
  const [dimensions, setDimensions] = useState(null);
  const [fitNote, setFitNote] = useState("");
  const [fillNote, setFillNote] = useState("");
  const [routeNote, setRouteNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [framingBusy, setFramingBusy] = useState(false);

  const signatureInputs = {
    activeTool,
    textContent,
    textBorder,
    fontSizeMm,
    textStyle: textStyleEffective,
    textOrigin,
    engraveRoute,
    // 自动过扫描随机器加速度变，读到新的加速度后填充文字的旧预览也算过期；转图片路线只看扫描子集
    fillSignature: (engraveRoute === "image"
      ? rasterScanSignature(fillSettings)
      : fillSettingsSignature(fillSettings, { withFill: textUsesFill, withCut: textIsCut }))
      + (textUsesFill ? JSON.stringify(motionPayload) : ""),
    uploadedImagePath,
    imageWidthMm,
    imageHeightMm,
    imageMode,
    imageExtras,
    // 图片同样按速度自动过扫描、套用双向扫描偏移表（r45）
    scanSignature: rasterScanSignature(fillSettings) + JSON.stringify(motionPayload),
    uploadedGcodePath,
    selectedMaterial,
    selectedThickness,
    customParams,
    customPower,
    customFeed,
    customPasses,
    presetParams: hasRecommendation ? `${powerPercent}/${recommendedFeedRate}/${recommendedPasses}` : "",
    workArea: `${maxWidthMm}x${maxHeightMm}`,
  };
  const currentSignature = workbenchInputSignature(signatureInputs);
  const previewStale = Boolean(currentWorkflow?.workflow_id) && isWorkbenchPreviewStale(previewSignature, currentSignature);

  const signatureInputsRef = useRef(signatureInputs);
  signatureInputsRef.current = signatureInputs;
  const currentWorkflowRef = useRef(currentWorkflow);
  const previewSignatureRef = useRef(previewSignature);
  const previewStaleRef = useRef(previewStale);
  currentWorkflowRef.current = currentWorkflow;
  previewSignatureRef.current = previewSignature;
  previewStaleRef.current = previewStale;

  const mountedRef = useRef(true);
  const previewBlobUrlRef = useRef("");
  const applyPreviewUrl = (nextUrl) => {
    const prev = previewBlobUrlRef.current;
    if (prev && prev !== nextUrl && prev.startsWith("blob:")) {
      URL.revokeObjectURL(prev);
    }
    if (!mountedRef.current) {
      if (nextUrl && nextUrl.startsWith("blob:")) {
        URL.revokeObjectURL(nextUrl);
      }
      previewBlobUrlRef.current = "";
      return;
    }
    previewBlobUrlRef.current = nextUrl && nextUrl.startsWith("blob:") ? nextUrl : "";
    setPreviewUrl(nextUrl || "");
  };
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (previewBlobUrlRef.current && previewBlobUrlRef.current.startsWith("blob:")) {
        URL.revokeObjectURL(previewBlobUrlRef.current);
        previewBlobUrlRef.current = "";
      }
    };
  }, []);
  // Overlay & Modals
  const [overlayStatus, setOverlayStatus] = useState("pending");
  const [sendProgress, setSendProgress] = useState(null);
  const [pollSafetyNotice, setPollSafetyNotice] = useState("");
  const pollingCleanupRef = useRef(null);
  useEffect(() => () => pollingCleanupRef.current?.(), []);
  const [showMaterialModal, setShowMaterialModal] = useState(false);
  const [materialModalData, setMaterialModalData] = useState(null);

  // Logs & Telemetry（正序追加：从上往下滚动，最新日志位于最底部；同步接收全局设备连接弹窗与启动握手日志）
  const [logs, setLogs] = useState(() => readBufferedDeviceLogs());
  useEffect(() => {
    const onDeviceLog = (e) => {
      const entry = e?.detail;
      if (!entry) return;
      setLogs((prev) => [...prev.slice(-99), entry]);
    };
    window.addEventListener(DEVICE_LOG_EVENT, onDeviceLog);
    return () => window.removeEventListener(DEVICE_LOG_EVENT, onDeviceLog);
  }, []);
  const [gcodeText, setGcodeText] = useState("");
  const gcodeLineCount = useMemo(() => (gcodeText ? gcodeText.split("\n").length : 0), [gcodeText]);
  const [mcpStatus, setMcpStatus] = useState(null);
  const effectiveMcpStatus = externalMcpStatus || mcpStatus;
  const lastLoggedPctRef = useRef(-1);

  // 最近任务回填（2026-11-26）：只改左侧表单，不碰当前预览；签名变了旧预览自动作废，要重新计算轨迹
  const handleRecallTask = (recall, workflow) => {
    if (!recall) return;
    if (recall.tool) setActiveTool(recall.tool);
    if (recall.text != null) setTextContent(recall.text);
    if (recall.fontSizeMm) setFontSizeMm(recall.fontSizeMm);
    if (recall.textStyle) setTextStyle(recall.textStyle);
    if (recall.textOrigin) setTextOrigin(recall.textOrigin);
    if (recall.textBorder) setTextBorder(recall.textBorder);
    const notes = [];
    if (recall.material) {
      const hit = materials.find((m) => m.name === recall.material);
      if (hit) {
        setSelectedMaterial(hit.name);
        const thick = pickThicknessForMaterial(hit, recall.thickness || selectedThickness);
        if (thick) setSelectedThickness(thick);
        if (recall.thickness && thick && String(thick) !== String(recall.thickness)) {
          notes.push(`材料库里没有 ${recall.thickness} mm，已选 ${thick} mm`);
        }
      } else {
        notes.push(`材料库里已没有“${recall.material}”，材料保持不变`);
      }
    }
    if (recall.custom) {
      setCustomParams(true);
      setCustomPower(recall.custom.power);
      setCustomFeed(recall.custom.feed);
      setCustomPasses(recall.custom.passes);
    }
    addLog(
      "PREVIEW",
      `已回填最近任务：${recentTaskLabel(workflow)}${notes.length ? `（${notes.join("；")}）` : ""}。请重新计算加工轨迹，开刻前仍需确认。`,
      notes.length ? "warn" : "info",
    );
  };

  const addLog = (tag, message, level = "info") => {
    const time = new Date().toLocaleTimeString();
    setLogs((prev) => [...prev.slice(-99), { time, tag, message, level }]);
  };

  // 挂载时加载用到的最新函数：每次渲染后同步
  const startupRef = useRef(null);
  useEffect(() => {
    startupRef.current = {
      loadMaterials,
      loadRuntimeConfig,
    };
  });

  // Initial Load: Materials & Runtime Config（不自动发起串口/网络握手探测，避免切页或刚填新地址时自动重连/重启板子）
  useEffect(() => {
    const startup = startupRef.current;
    startup.loadMaterials();
    startup.loadRuntimeConfig();

    // 材料实验室修改后经 BroadcastChannel 跨页同步刷新
    const unsubscribeMaterials = subscribeMaterialsUpdated(() => startupRef.current.loadMaterials());
    return () => {
      unsubscribeMaterials();
    };
  }, []);

  // 切换连接方式、串口、波特率或网络控制器地址时复位旧连接状态，避免界面误显绿灯或跳过新目标握手探测
  const connectionTargetKey =
    connectionMode === "serial"
      ? `serial:${serialPort}:${baudrate}`
      : `network:${networkHost.trim()}:${telnetPort}`;
  const prevConnectionKeyRef = useRef(connectionTargetKey);
  const probeEpochRef = useRef(0);
  useEffect(() => {
    if (prevConnectionKeyRef.current !== connectionTargetKey) {
      prevConnectionKeyRef.current = connectionTargetKey;
      probeEpochRef.current += 1;
      setConnectionStatus?.({
        connected: false,
        summary: "连接参数已更改，请重新握手探测",
      });
    }
  }, [connectionTargetKey, setConnectionStatus]);

  // Save to local storage only for keys whose value actually changed
  const persistedRef = useRef({});
  useEffect(() => {
    const entries = [
      ["laser-forge-mat", selectedMaterial],
      ["laser-forge-thick", selectedThickness],
      ["laser-forge-text", textContent],
      ["laser-forge-font-size", String(fontSizeMm)],
      ["laser-forge-text-style", textStyle],
      ["laser-forge-text-border", JSON.stringify(textBorder)],
    ];
    for (const [key, val] of entries) {
      if (persistedRef.current[key] !== val) {
        persistedRef.current[key] = val;
        writeLocal(key, val);
      }
    }
  }, [
    selectedMaterial,
    selectedThickness,
    textContent,
    fontSizeMm,
    textStyle,
    textBorder,
  ]);

  const loadMaterials = async () => {
    const res = await getJson("/api/ui/material-options", { timeoutMs: DEFAULT_READ_TIMEOUT_MS });
    if (res.ok && res.data?.success) {
      const list = res.data.result?.materials || [];
      setMaterials(list);
      const curMat = selectedMaterialRef.current;
      const curThick = selectedThicknessRef.current;
      if (list.length > 0) {
        const hit = list.find((m) => m.name === curMat) || list[0];
        if (hit.name !== curMat) {
          setSelectedMaterial(hit.name);
        }
        const nextThick = pickThicknessForMaterial(hit, curThick);
        if (nextThick && nextThick !== String(curThick)) {
          setSelectedThickness(nextThick);
        }
      }
      setRecRefreshTick((n) => n + 1);
    }
  };

  const loadRuntimeConfig = async () => {
    const res = await getJson("/api/ui/runtime-config", { timeoutMs: DEFAULT_READ_TIMEOUT_MS });
    if (res.ok && res.data?.success) {
      const conn = res.data.result?.connection;
      if (conn?.network?.default_host && !networkHost) {
        setNetworkHost(conn.network.default_host);
      }
    }
    if (!externalMcpStatus) {
      // 读取系统信息中的真实小智 MCP 状态（外层 App 已同步时直接复用，省一次重复请求）
      const sysRes = await getJson("/api/system/info", { timeoutMs: DEFAULT_READ_TIMEOUT_MS });
      if (sysRes.ok && sysRes.data?.success && sysRes.data?.result?.mcp) {
        setMcpStatus(sysRes.data.result.mcp);
      }
    }
  };

  // 联动加载选定材质、厚度与当前加工内容对应的真实推荐工艺参数
  useEffect(() => {
    let cancelled = false;
    const controller = typeof AbortController !== "undefined" ? new AbortController() : null;
    async function syncMaterialRecommendation() {
      if (!selectedMaterial || !selectedThickness) {
        setHasRecommendation(false);
        return;
      }
      const recMode = materialRecommendationModes({ activeTool, textStyle: textStyleEffective });
      const query = new URLSearchParams({
        material: selectedMaterial,
        thickness_mm: String(selectedThickness),
        laser_mode: recMode.laserMode,
      });
      if (recMode.engravingMode) {
        query.set("engraving_mode", recMode.engravingMode);
      }
      const res = await getJson(`/api/ui/material-recommendation?${query.toString()}`, {
        signal: controller?.signal,
        timeoutMs: DEFAULT_READ_TIMEOUT_MS,
      });
      if (cancelled) return;
      if (res.ok && res.data?.success && res.data.result) {
        const rec = res.data.result;
        if (rec.laser_max_power != null) {
          setPowerPercent(Math.round(rec.laser_max_power / 10));
        }
        if (rec.feed_rate != null) {
          setRecommendedFeedRate(Math.round(rec.feed_rate));
        }
        setRecommendedPasses(Math.max(1, Math.round(rec.passes || 1)));
        setHasRecommendation(true);
        // 条目变了（或它存的设置变了）才重新载入，同一条目刷新不打断正在改的值
        const layer = materialLayerFromRecommendation(rec);
        setMaterialLayer(layer);
        if (layer && layer.key !== materialLayerKeyRef.current) {
          materialLayerKeyRef.current = layer.key;
          const next = applyMaterialLayer(readFillSettings(), layer);
          setFillSettings(next.settings);
          setMaterialApplied(next.applied);
        }
      } else {
        setHasRecommendation(false);
        setMaterialLayer(null);
        if (materialLayerKeyRef.current) {
          materialLayerKeyRef.current = "";
          setFillSettings(readFillSettings());
          setMaterialApplied([]);
        }
      }
    }
    syncMaterialRecommendation();
    return () => {
      cancelled = true;
      controller?.abort();
    };
  }, [selectedMaterial, selectedThickness, activeTool, textStyleEffective, recRefreshTick]);

  // r40：把当前填充 / 切割设置存进材料库条目（只改图层设置，不动功率速度；扫描偏移与加速度不随材料保存）
  const saveFillToMaterial = async () => {
    if (!materialLayer) {
      showAlert("还没读到当前材料与厚度的材料库条目，请先在材料实验室保存参数", { title: "无法保存", tone: "warning" });
      return;
    }
    const payload = materialLayerPayload(fillSettings, { withFill: textUsesFill && !textIsCut, withCut: textIsCut });
    if (payload.error) {
      showAlert(payload.error, { title: "设置不对", tone: "warning" });
      return;
    }
    const ok = await showConfirm(
      `把当前${textIsCut ? "切割" : "填充"}设置存进材料库「${materialLayer.label}」？\n以后选这个材料会自动带出；双向扫描偏移与机器加速度跟机器走，不随材料保存。`,
      { title: "保存到材料库", confirmText: "保存", cancelText: "取消" },
    );
    if (!ok) return;
    const res = await postJson("/api/material-lab/manage", materialLayerSaveRequest(materialLayer, payload));
    if (res.ok && res.data?.success) {
      addLog("MATERIAL", `已把${textIsCut ? "切割" : "填充"}设置存进材料库「${materialLayer.label}」`, "ok");
      notifyMaterialsUpdated();
      materialLayerKeyRef.current = "";
      setRecRefreshTick((tick) => tick + 1);
    } else {
      showAlert(res.data?.result || res.error || "保存失败", { title: "保存到材料库失败", tone: "error" });
    }
  };

  const connectionForm = {
    connection_mode: connectionMode,
    port: serialPort,
    baudrate: String(baudrate),
    network_host: networkHost,
    network_telnet_port: String(telnetPort),
  };

  // 开刻/续雕前的快速连通性核验（复用全局共享 probeControllerConnection）
  const verifyConnectionBeforeSend = async () => {
    const epoch = ++probeEpochRef.current;
    const outcome = await probeControllerConnection(postJson, connectionForm);
    if (probeEpochRef.current !== epoch) return false;
    setConnectionStatus(outcome.status);
    if (outcome.connected) {
      syncHardwareWorkspaceLimits(outcome.machineLimits, connectionForm, undefined, outcome.handshakeMeta);
      syncHardwareMachineFeatures(outcome.status?.features ?? null, connectionForm);
      writeMachineMotion(outcome.machineLimits, connectionForm);
      addLog("DEVICE", outcome.text, outcome.logLevel || "ok");
    }
    return outcome.connected;
  };

  // Generate Preview
  const handleGeneratePreview = async () => {
    if (busy || framingBusy) {
      addLog("PREVIEW", "当前正在解算或走边框，请稍候...", "warn");
      return;
    }

    if (currentWorkflow?.workflow_id && !previewStale) {
      const ok = await showConfirm("当前参数没有变化且已生成过加工轨迹，确认要重新计算并覆盖吗？", {
        title: "重新生成确认",
        confirmText: "确认重新生成",
        cancelText: "保留当前结果",
      });
      if (!ok) {
        addLog("PREVIEW", "已保留当前轨迹与 G-code", "info");
        return;
      }
    }

    const manual = activeTool === "gcode" ? null : manualParamsPayload({
      enabled: customParams,
      powerPercent: customPower,
      feedRate: customFeed,
      passes: customPasses,
    });
    if (manual?.error) {
      showAlert(manual.error, { title: "自定义参数不对", tone: "warning" });
      return;
    }
    const manualField = manual ? { manual_params: manual } : {};
    const signatureSnapshot = currentSignature;

    setBusy(true);
    addLog("PREVIEW", `正在为 [${selectedMaterial} ${selectedThickness}mm] 解算激光加工轨迹与 G-code...`, "info");

    try {
      let res;
      if (activeTool === "text") {
        if (!textContent.trim()) {
          showAlert("请输入要雕刻的文字", { title: "缺少输入", tone: "warning" });
          setBusy(false);
          return;
        }
        const borderPayload = textBorderPayload({ ...textBorder, maxWidthMm, maxHeightMm });
        if (borderPayload.error) {
          showAlert(borderPayload.error, { title: "外框设置不对", tone: "warning" });
          setBusy(false);
          return;
        }
        // 雕刻路线不随请求发送：后端按全局开关（与语音同一条），工作台只按它显示可选刻法
        const spec = resolveTextStyle(textStyleEffective);
        const fillPayload = engraveRoute === "image"
          ? rasterScanPayload(fillSettings)
          : fillSettingsPayload(fillSettings, {
              withFill: spec.textFill !== "outline",
              withCut: spec.laserMode === "cut",
            });
        if (fillPayload.error) {
          showAlert(fillPayload.error, {
            title: engraveRoute === "image" ? "扫描设置不对" : (spec.laserMode === "cut" ? "切割或走刀设置不对" : "填充设置不对"),
            tone: "warning",
          });
          setBusy(false);
          return;
        }
        res = await postJson("/api/text-task/generate", {
          text: workbenchTextPayload(textContent),
          material: selectedMaterial,
          thickness_mm: Number(selectedThickness),
          font_size_mm: Number(fontSizeMm),
          laser_mode: spec.laserMode,
          engraving_mode: spec.engravingMode,
          text_fill: spec.textFill,
          origin: textOrigin,
          work_area_width_mm: maxWidthMm,
          work_area_height_mm: maxHeightMm,
          ...borderPayload,
          ...fillPayload,
          ...motionPayload,
          ...manualField,
        });
      } else if (activeTool === "image") {
        if (!uploadedImagePath) {
          showAlert("请先点击上方选择并上传一张图片", { title: "缺少图片", tone: "warning" });
          setBusy(false);
          return;
        }
        const imgExtra = imageParamsPayload({
          widthMm: imageWidthMm,
          heightMm: imageHeightMm,
          imageMode,
          extras: imageExtras,
          maxWidthMm: Math.max(1000, maxWidthMm),
          maxHeightMm: Math.max(1000, maxHeightMm),
        });
        if (imgExtra.error) {
          showAlert(imgExtra.error, { title: "图片参数填写有误", tone: "warning" });
          setBusy(false);
          return;
        }
        const scanPayload = rasterScanPayload(fillSettings);
        if (scanPayload.error) {
          showAlert(scanPayload.error, { title: "扫描设置不对", tone: "warning" });
          setBusy(false);
          return;
        }
        res = await postJson("/api/workflow", {
          action: "preview",
          material: selectedMaterial,
          thickness_mm: Number(selectedThickness),
          source_type: "image",
          image_file: uploadedImagePath,
          laser_mode: "engrave",
          engraving_mode: "raster",
          work_area_width_mm: maxWidthMm,
          work_area_height_mm: maxHeightMm,
          ...imgExtra,
          ...scanPayload,
          ...motionPayload,
          ...manualField,
        });
      } else if (activeTool === "gcode") {
        if (!uploadedGcodePath) {
          showAlert("请先选择要加工的 .gcode / .nc 文件", { title: "缺少刀路文件", tone: "warning" });
          setBusy(false);
          return;
        }
        res = await postJson("/api/workflow", {
          action: "preview",
          material: selectedMaterial,
          thickness_mm: Number(selectedThickness),
          source_type: "gcode",
          gcode_file: uploadedGcodePath,
          ...motionPayload,
        });
      }

      setBusy(false);
      if (res?.ok && res.data?.success) {
        const wf = res.data.result;
        pollingCleanupRef.current?.();
        pollingCleanupRef.current = null;
        setOverlayStatus("pending");
        setSendProgress(null);
        setPollSafetyNotice("");
        setCurrentWorkflow(wf);
        setPreviewSignature(signatureSnapshot);
        const info = extractWorkflowInfo(wf);
        const artifacts = wf.artifacts || {};

        const assetSeq = ++workflowAssetSeqRef.current;
        if (artifacts.image_file) {
          // 远程 App 或局域网配对浏览器下 /preview 需要 Bearer 头，<img> 直链拿不到，需经鉴权取回 blob
          const nextUrl = await loadWorkbenchPreviewUrl(artifacts.image_file, {
            remote: isRemoteMode() || Boolean(readAuthToken()),
            fetchBlobUrl: fetchPreviewBlobUrl,
            resolveUrl: resolveApiUrl,
          });
          if (!mountedRef.current || workflowAssetSeqRef.current !== assetSeq) {
            if (typeof nextUrl === "string" && nextUrl.startsWith("blob:")) {
              URL.revokeObjectURL(nextUrl);
            }
          } else {
            applyPreviewUrl(nextUrl);
          }
        }
        if (artifacts.gcode_file) {
          // 2026-11-05：先核对 HTTP 状态，错误页不得当 G-code 显示；SHA-256 只用后端给出的真实值，
          // 拿不到时如实说明不可用，绝不伪造摘要或声称门禁已通过（发送时后端仍按物理文件重算）。
          void fetchPreviewText(artifacts.gcode_file).then((preview) => {
            if (!mountedRef.current || workflowAssetSeqRef.current !== assetSeq) return;
            if (!preview.ok) {
              setGcodeText("");
              addLog("PREVIEW", `加工预览已生成，但 G-code 刀路读取失败：${preview.error}。请重新生成预览后再发送。`, "error");
              showAlert(`${preview.error}。工作流 ${info.jobId || wf.workflow_id} 的刀路文件无法读取，请重新生成预览。`, { title: "刀路读取失败", tone: "error" });
              return;
            }
            const t = preview.text;
            setGcodeText(t);
            const lineCount = t.split("\n").filter((l) => l.trim().length > 0).length;
            const sha = wf.confirmation_snapshot?.gcode_sha256 || artifacts.gcode_sha256 || "";
            if (sha) {
              addLog("PREVIEW", `刀路解算完成: ${lineCount.toLocaleString()} 行 G-code, 物理 SHA-256: ${String(sha).slice(0, 8)}... 已就绪`, "ok");
              addLog("HOST", `任务 [${info.jobId || wf.workflow_id}] 预览已就绪，可先走边框对位，确认后点击【开始雕刻】`, "info");
            } else {
              addLog("PREVIEW", `刀路解算完成: ${lineCount.toLocaleString()} 行 G-code；后端未返回物理 SHA-256，摘要不可用`, "warn");
              addLog("HOST", `任务 [${info.jobId || wf.workflow_id}] 未确认刀路门禁，发送时由后端重新计算物理文件 SHA-256 校验`, "warn");
            }
          });
        } else {
          addLog("PREVIEW", `加工预览已生成，工作流就绪: ${info.jobId || wf.workflow_id}`, "ok");
        }
        setTimeEstimate(artifacts.time_estimate || null);
        setFitNote(typeof artifacts.fit_note === "string" ? artifacts.fit_note : "");
        setFillNote(fillResultNote(artifacts.fill_options, artifacts.removed_duplicates, artifacts.cut_options));
        setRouteNote(typeof artifacts.route_note === "string" ? artifacts.route_note : "");
        if (Array.isArray(artifacts.warnings)) {
          for (const warn of artifacts.warnings) {
            if (typeof warn === "string" && warn.trim()) addLog("PREVIEW", warn.trim(), "warn");
          }
        }
        if (artifacts.width_mm && artifacts.height_mm) {
          // 2026-11-06：文字默认以激光头对准中心为起点、图片以左下角为起点，开刻前提示放哪
          setDimensions({ width: artifacts.width_mm, height: artifacts.height_mm, originNote: artifacts.origin_note || "" });
          if (artifacts.origin_note) addLog("HOST", artifacts.origin_note, "warn");
        } else {
          setDimensions(null);
        }
      } else {
        addLog("PREVIEW", res?.data?.result || "预览生成失败", "error");
        showAlert(res?.data?.result || "预览生成失败", { title: "预览生成失败", tone: "error" });
      }
    } catch (e) {
      setBusy(false);
      addLog("PREVIEW", `网络或内部异常: ${e.message}`, "error");
    }
  };

  // 采用调参建议、两点对位生成的新预览：都是独立的新 workflow（新 SHA），沿用原预览输入签名并在过期时拦截
  const adoptDerivedWorkflow = async (workflow, label) => {
    const check = checkDerivedWorkbenchAdoption({
      workflow,
      currentWorkflow: currentWorkflowRef.current,
      previewSignature: previewSignatureRef.current,
      previewStale: previewStaleRef.current,
      label,
    });
    if (!check.ok) {
      addLog("PREVIEW", check.reason, "warn");
      showAlert(check.reason, { title: "预览已过期 · 已拦截派生结果", tone: "warning" });
      return;
    }
    const feedbackInputs = workbenchFeedbackInputs(workflow, signatureInputsRef.current);
    if (feedbackInputs) {
      setCustomParams(feedbackInputs.customParams);
      setCustomPower(feedbackInputs.customPower);
      setCustomFeed(feedbackInputs.customFeed);
      setCustomPasses(feedbackInputs.customPasses);
    }
    setPreviewSignature(feedbackInputs?.signature || check.inheritedSignature);
    pollingCleanupRef.current?.();
    pollingCleanupRef.current = null;
    setCurrentWorkflow(workflow);
    setOverlayStatus("pending");
    setSendProgress(null);
    setPollSafetyNotice("");
    const assetSeq = ++workflowAssetSeqRef.current;
    const artifacts = workflow.artifacts || {};
    const nextUrl = await loadWorkbenchPreviewUrl(artifacts.image_file, {
      remote: isRemoteMode() || Boolean(readAuthToken()),
      fetchBlobUrl: fetchPreviewBlobUrl,
      resolveUrl: resolveApiUrl,
    });
    if (!mountedRef.current || workflowAssetSeqRef.current !== assetSeq) {
      if (typeof nextUrl === "string" && nextUrl.startsWith("blob:")) {
        URL.revokeObjectURL(nextUrl);
      }
      return;
    }
    applyPreviewUrl(nextUrl);
    setGcodeText("");
    if (artifacts.gcode_file) {
      const preview = await fetchPreviewText(artifacts.gcode_file);
      if (!mountedRef.current || workflowAssetSeqRef.current !== assetSeq) return;
      if (preview.ok) {
        setGcodeText(preview.text);
      } else {
        addLog("PREVIEW", `${label}已生成，但刀路资源加载失败（${preview.error}），请重新打开任务`, "error");
      }
    }
    setTimeEstimate(artifacts.time_estimate || null);
    setFitNote(typeof artifacts.fit_note === "string" ? artifacts.fit_note : "");
    setRouteNote(typeof artifacts.route_note === "string" ? artifacts.route_note : "");
    setDimensions(
      artifacts.width_mm && artifacts.height_mm
        ? { width: artifacts.width_mm, height: artifacts.height_mm, originNote: artifacts.origin_note || "" }
        : null,
    );
    addLog("PREVIEW", `${label}已生成：${workflow.workflow_id}，尚未发送`, "ok");
    if (artifacts.origin_note) addLog("HOST", artifacts.origin_note, "warn");
    if (Array.isArray(artifacts.warnings)) {
      for (const warn of artifacts.warnings) {
        if (typeof warn === "string" && warn.trim()) addLog("PREVIEW", warn.trim(), "warn");
      }
    }
  };

  // 先走边框（会真实驱动激光头移动，单击不得直接执行，须先经 showConfirm 二次确认）
  const handleFraming = async () => {
    if (busy || framingBusy || !currentWorkflow?.workflow_id) return;
    if (previewStale) {
      showAlert("左侧参数已改动，请先点击【重新生成轨迹】再走边框。", { title: "预览已过期", tone: "warning" });
      return;
    }
    if (!drawTargetReady(connectionForm)) {
      showAlert(
        connectionMode === "serial" ? "请先点击顶栏硬件状态或「修改连接」选择有效串口" : "请先点击顶栏硬件状态或「修改连接」填写控制器 IP 地址",
        { title: "尚未指定雕刻机", tone: "warning" },
      );
      return;
    }
    const req = framingRequest(currentWorkflow, drawConnectionPayload(connectionForm), { shape: framingShape });
    if (!req) {
      showAlert("当前刀路没有相对起点的边框范围（外部导入刀路起点由文件自身决定），无法直接走边框。", {
        title: "不支持走边框",
        tone: "info",
      });
      return;
    }
    const w = Math.round((req.max_x - req.min_x) * 10) / 10;
    const h = Math.round((req.max_y - req.min_y) * 10) / 10;
    const targetName = drawTargetLabel(connectionForm);
    const confirmed = await showConfirm(
      `目标设备：${targetName}\n激光头将从当前位置出发，${framingShapeText(req)}绕一圈再回到原处（默认不出光）。\n请确认机器周围无障碍物；运行期间顶栏急停可随时停止。`,
      {
        title: "确认先走边框（机器将移动）",
        tone: "warning",
        confirmText: "开始走边框",
        cancelText: "取消",
      },
    );
    if (!confirmed) return;

    setFramingBusy(true);
    addLog("FRAMING", `正在向 ${targetName} 请求走边框（${w} × ${h} mm，不出光）...`, "info");
    try {
      const res = await postJson("/api/device/framing", req);
      if (isApprovalRequiredResponse(res)) {
        const msg = approvalPendingMessage(res, "走边框");
        addLog("FRAMING", msg, "warn");
        showAlert(msg, { title: "等待桌面审批", tone: "info" });
      } else if (res.ok && res.data?.success) {
        addLog("FRAMING", `走边框完成（${w} × ${h} mm，未出光），确认木牌位置无误后可点击【开始雕刻】`, "ok");
      } else {
        const reason = (typeof res.data?.result === "string" && res.data.result) || "走边框未完成";
        addLog("FRAMING", `走边框未完成：${reason}`, "error");
        showAlert(reason, { title: "走边框未完成", tone: "error" });
      }
    } catch (e) {
      addLog("FRAMING", `走边框网络异常：${e.message}`, "error");
      showAlert(`走边框请求异常：${e.message}`, { title: "走边框失败", tone: "error" });
    } finally {
      setFramingBusy(false);
    }
  };

  // 导出当前 G-code 刀路（统一走 saveFile.js::exportFile，不用 <a download>）
  const handleExportGcode = async () => {
    if (!gcodeText) return;
    const rawName = currentWorkflow?.artifacts?.gcode_file || `${currentWorkflow?.workflow_id || "toolpath"}.gcode`;
    const filename = String(rawName).split(/[\\/]/).pop() || "toolpath.gcode";
    try {
      const blob = new Blob([gcodeText], { type: "text/plain;charset=utf-8" });
      const saved = await exportFile(blob, filename, "G-code 刀路");
      if (!saved?.cancelled) {
        addLog("EXPORT", `已导出 G-code 刀路文件: ${saved?.path || filename}`, "ok");
      }
    } catch (e) {
      addLog("EXPORT", `导出 G-code 失败: ${e.message}`, "error");
      showAlert(`导出失败：${e.message}`, { title: "导出失败", tone: "error" });
    }
  };

  // 温和取消（记录已完成行数断点，机器停稳后可断点续雕）
  const handleCancelJob = async () => {
    if (!currentWorkflow?.workflow_id) return;
    const confirmed = await showConfirm(
      "将停止后续刀路发送并走安全停机序列，记录当前已完成行号以便之后断点续雕。\n（若遇紧急异常请直接点红色「0x18 急停」或按机器实体急停）",
      {
        title: "暂停并取消当前雕刻",
        tone: "warning",
        confirmText: "取消加工",
        cancelText: "继续雕刻",
      },
    );
    if (!confirmed) return;
    try {
      const res = await postJson("/api/workflow", {
        action: "cancel",
        workflow_id: currentWorkflow.workflow_id,
      });
      if (res.ok && res.data?.success) {
        addLog("CANCEL", "已发出取消请求，等待控制器停稳并更新断点状态（请求发出不等于机器已物理停机）", "warn");
      } else {
        const reason = res.data?.result || "取消请求未成功";
        addLog("CANCEL", reason, "error");
        showAlert(reason, { title: "取消未成功", tone: "error" });
      }
    } catch (e) {
      addLog("CANCEL", `取消请求异常: ${e.message}`, "error");
    }
  };

  // Confirm Send
  const handleConfirmSend = async () => {
    if (!currentWorkflow?.workflow_id) {
      showAlert("请先点击【计算加工轨迹】，预览并确认加工参数后方可发送", { title: "尚未生成预览", tone: "warning" });
      return;
    }
    if (previewStale) {
      showAlert(
        "左侧内容、材料或参数已修改，当前预览仍是上一轮的旧刀路！请先点击【重新生成轨迹】核对新预览后再开始雕刻。",
        { title: "预览已过期 · 已拦截旧刀路", tone: "warning" },
      );
      return;
    }

    const targetName = drawTargetReady(connectionForm) ? drawTargetLabel(connectionForm) : "";
    const safetyWarning = resolveSoftLimitSendWarning(connectionForm, undefined, connectionStatus);
    const prompt = workbenchConfirmSendPrompt({
      workflow: currentWorkflow,
      material: selectedMaterial,
      thickness: selectedThickness,
      targetLabel: targetName || (connectionMode === "serial" ? serialPort || "未选串口" : `${networkHost || "未填 IP"}:${telnetPort}`),
      dimensions,
      lastSentSha256,
      safetyWarning,
    });
    const confirmed = await showConfirm(prompt.message, {
      title: prompt.title,
      tone: "warning",
      danger: prompt.danger,
      confirmText: prompt.confirmText,
      cancelText: "取消",
      origin: prompt.origin,
    });
    if (!confirmed) {
      addLog("DISPATCH", "已取消本次开刻确认，机器未启动", "info");
      return;
    }

    if (sendInFlightRef.current) return;
    sendInFlightRef.current = true;
    const prevOverlayStatus = overlayStatus;
    // 确认后立刻锁入 preparing，消除自动握手期间按钮仍可重复点击导致的双发窗口
    setOverlayStatus("preparing");
    try {
      // 优化 2: 点击确认开刻后自动兜底校验握手
      let isConnected = connectionStatus?.connected;
      if (!isConnected) {
        addLog("GATE", "检测到硬件尚未握手，正在自动执行快速握手校验...", "info");
        isConnected = await verifyConnectionBeforeSend();
      }

      // 强连通性前置硬门禁：如果设备确实未连接，严禁下发伪发送，坚决报错拦截
      if (!isConnected) {
        setOverlayStatus(prevOverlayStatus);
        const hint = connectionMode === "serial"
          ? `未检测到有效串口连接（当前串口: ${serialPort || "未选择"}）。请确保 USB 数据线已插入且驱动正常！`
          : `未检测到网络控制器连接（当前设备: ${networkHost || "未填写 IP"}:${telnetPort}）。请确保控制器已开机且局域网可达！`;
        addLog("GATE", `发送被安全门禁拦截：${hint}`, "error");
        showAlert(hint, { title: "发送失败 · 设备未连接", tone: "error", confirmText: "知道了" });
        return;
      }

      lastLoggedPctRef.current = -1;
      addLog("DISPATCH", "安全门禁校验通过，开始逐行下发加工指令...", "info");

      const payload = {
        action: "confirm_send",
        workflow_id: currentWorkflow.workflow_id,
        confirmed: true,
        ...workbenchConnectionFields(deviceConnection),
      };

      const res = await postJson("/api/workflow", payload);
      if (isApprovalRequiredResponse(res)) {
        const msg = approvalPendingMessage(res, "开始雕刻");
        setOverlayStatus("pending");
        addLog("DISPATCH", msg, "warn");
        showAlert(msg, { title: "等待桌面审批", tone: "info" });
      } else if (res.ok && res.data?.success) {
        if (prompt.sha256) {
          setLastSentSha256(prompt.sha256);
          writeLocal("laser-forge-last-sent-sha256", prompt.sha256);
        }
        const sr = res.data.result?.send_result || {};
        const total = sr.total_lines || gcodeLineCount;
        const parsedProgress = sendProgressFromResult(sr);
        const initialProgress = parsedProgress
          ? { ...parsedProgress, sent: 0, total }
          : { sent: 0, total, target: sr.target || "" };
        setSendProgress(initialProgress);
        if (Array.isArray(initialProgress.warnings)) {
          for (const warn of initialProgress.warnings) {
            addLog("GATE", warn, "warn");
          }
        }
        addLog("STREAM", `进度同步: 0% (0 / ${total} 行)`, "info");
        setOverlayStatus("running");
        startStatusPolling(currentWorkflow.workflow_id, initialProgress);
      } else if (res.status === 0) {
        // 网络中断或超时：后端可能已启动激光任务，严禁报失败或放开重发，立刻补全目标并拉起原任务轮询以保留急停绑定
        const canonicalTarget = workbenchCanonicalTarget(deviceConnection);
        setSendProgress((prev) => sendProgressForUnknownResult(prev, gcodeLineCount, canonicalTarget));
        const warn = "发送请求断连或超时，后端可能已开始加工，正在持续查询原任务状态，请勿重复发送；必要时请按急停。";
        setOverlayStatus("running");
        setPollSafetyNotice(warn);
        addLog("DISPATCH", warn, "warn");
        showAlert(warn, { title: "发送结果未知 · 禁止盲目重发", tone: "warning" });
        startStatusPolling(currentWorkflow.workflow_id);
      } else {
        setOverlayStatus("failed");
        const reason = res.data?.result || "发送门禁拦截或连接失败";
        addLog("DISPATCH", reason, "error");
        showAlert(reason, { title: "发送失败", tone: "error" });
      }
    } catch (e) {
      const canonicalTarget = workbenchCanonicalTarget(deviceConnection);
      setSendProgress((prev) => sendProgressForUnknownResult(prev, gcodeLineCount, canonicalTarget));
      const warn = `发送网络异常（${e.message}），后端可能已启动任务，正在查询原任务状态，请勿重复发送。`;
      setOverlayStatus("running");
      setPollSafetyNotice(warn);
      addLog("DISPATCH", warn, "warn");
      startStatusPolling(currentWorkflow.workflow_id);
    } finally {
      sendInFlightRef.current = false;
    }
  };

  // Resume Send (断点续雕：从设备证实完成的断点行之后继续下发剩余 G-code)
  const handleResumeSend = async () => {
    if (!currentWorkflow?.workflow_id) return;
    if (previewStale) {
      addLog("GATE", "左侧参数已改动，当前中断任务已过期，禁止带新参数续雕旧刀路", "warn");
      showAlert(
        "左侧输入或工艺参数已修改，禁止在旧刀路上直接断点续雕！请先点击左侧「重新生成轨迹」或还原参数。",
        { title: "参数已变更 · 拦截续雕", tone: "warning" },
      );
      return;
    }
    if (!canResumeJob(sendProgress, overlayStatus)) return;

    const startLine = getResumeStartLine(sendProgress);
    const totalLines = sendProgress?.total || 0;
    const targetName = drawTargetReady(connectionForm) ? drawTargetLabel(connectionForm) : "";
    const safetyWarning = resolveSoftLimitSendWarning(connectionForm, undefined, connectionStatus);
    const resumePrompt = workbenchResumeSendPrompt({
      workflow: currentWorkflow,
      material: selectedMaterial,
      thickness: selectedThickness,
      targetLabel:
        targetName ||
        (connectionMode === "serial"
          ? serialPort || "未选串口"
          : `${networkHost || "未填 IP"}:${telnetPort}`),
      startLine,
      totalLines,
      safetyWarning,
      sendWarnings: sendProgress?.warnings,
    });
    const confirmed = await showConfirm(resumePrompt.message, {
      title: resumePrompt.title,
      tone: "warning",
      danger: resumePrompt.danger,
      confirmText: resumePrompt.confirmText,
      cancelText: "取消",
    });
    if (!confirmed) {
      addLog("DISPATCH", "已取消断点续雕确认，机器未启动", "info");
      return;
    }

    if (sendInFlightRef.current) return;
    sendInFlightRef.current = true;
    const prevOverlayStatus = overlayStatus;
    setOverlayStatus("preparing");
    try {
      let isConnected = connectionStatus?.connected;
      if (!isConnected) {
        addLog("GATE", "正在自动执行快速握手校验...", "info");
        isConnected = await verifyConnectionBeforeSend();
      }
      if (!isConnected) {
        setOverlayStatus(prevOverlayStatus);
        showAlert("未检测到控制器连接，请确认设备已开机并完成握手！", { title: "断点续雕失败", tone: "error" });
        return;
      }

      addLog("DISPATCH", `正在从第 ${startLine} 行发起断点续雕，剩余代码即将继续流式下发...`, "info");

      const payload = {
        action: "resume",
        workflow_id: currentWorkflow.workflow_id,
        confirmed: true,
        ...workbenchConnectionFields(deviceConnection),
      };

      const res = await postJson("/api/workflow", payload);
      if (isApprovalRequiredResponse(res)) {
        const msg = approvalPendingMessage(res, "断点续雕");
        setOverlayStatus(prevOverlayStatus);
        addLog("DISPATCH", msg, "warn");
        showAlert(msg, { title: "等待桌面审批", tone: "info" });
      } else if (res.ok && res.data?.success) {
        const sr = res.data.result?.send_result || {};
        const total = sr.total_lines || sendProgress?.total || 0;
        const sent = sr.lines_sent || sendProgress?.sent || 0;
        const nextProgress = sendProgressFromResult(sr) || { sent, total, target: sr.target || "" };
        setSendProgress(nextProgress);
        if (Array.isArray(nextProgress.warnings)) {
          for (const warn of nextProgress.warnings) {
            addLog("GATE", warn, "warn");
          }
        }
        addLog("STREAM", `断点续雕已启动：正在从第 ${startLine}/${total} 行继续执行`, "ok");
        setOverlayStatus("running");
        startStatusPolling(currentWorkflow.workflow_id, nextProgress);
      } else if (res.status === 0) {
        const canonicalTarget = workbenchCanonicalTarget(deviceConnection);
        setSendProgress((prev) => sendProgressForUnknownResult(prev, gcodeLineCount, canonicalTarget));
        const warn = "断点续雕请求断连或超时，后端可能已恢复加工，正在持续查询原任务状态，请勿重复续雕；必要时请按急停。";
        setOverlayStatus("running");
        setPollSafetyNotice(warn);
        addLog("DISPATCH", warn, "warn");
        showAlert(warn, { title: "续雕结果未知 · 禁止盲目重发", tone: "warning" });
        startStatusPolling(currentWorkflow.workflow_id);
      } else {
        setOverlayStatus("failed");
        const reason = res.data?.result || "断点恢复失败";
        addLog("DISPATCH", reason, "error");
        showAlert(reason, { title: "断点恢复失败", tone: "error" });
      }
    } catch (e) {
      const canonicalTarget = workbenchCanonicalTarget(deviceConnection);
      setSendProgress((prev) => sendProgressForUnknownResult(prev, gcodeLineCount, canonicalTarget));
      const warn = `恢复发送网络异常（${e.message}），后端可能已启动续雕，正在查询原任务状态，请勿重复续雕。`;
      setOverlayStatus("running");
      setPollSafetyNotice(warn);
      addLog("DISPATCH", warn, "warn");
      startStatusPolling(currentWorkflow.workflow_id);
    } finally {
      sendInFlightRef.current = false;
    }
  };

  // Query errors preserve the last known job state and never remove its stop controls.
  const startStatusPolling = (workflowId, initialProgress = sendProgress) => {
    pollingCleanupRef.current?.();
    let state = { id:workflowId, kind:"workflow", status:"sending", progress:initialProgress };
    const stop = startJobPolling(
      (signal) => postJson("/api/workflow", { action:"status", workflow_id:workflowId }, { signal }),
      (res) => {
        state = applyJobPoll(state, workflowId, "workflow", res);
        if (state.connectionLost) {
          setPollSafetyNotice("状态未知：无法查询设备，机器可能仍在运行。正在重试，请保留急停并到现场确认。");
          if (state.pollFailures === 1) addLog("POLL", "设备状态暂时未知，继续查询；不把通信异常当作停机", "warn");
          return;
        }
        setPollSafetyNotice((notice) => notice.startsWith("状态未知") ? "" : notice);
        const sr = res.data.result?.send_result || {};
        const fromSr = sendProgressFromResult(sr);
        if (fromSr && fromSr.total > 0) {
          setSendProgress((prev) => {
            if (fromSr.warnings?.length || !prev?.warnings?.length) return fromSr;
            return { ...fromSr, warnings: prev.warnings };
          });
          const pct = Math.min(100, Math.round(fromSr.sent / fromSr.total * 100));
          if (pct > 0 && pct % 25 === 0 && pct !== lastLoggedPctRef.current) {
            lastLoggedPctRef.current = pct;
            addLog("STREAM", `进度同步: ${pct}% (${fromSr.sent} 行)`, "info");
          }
        } else if (state.progress) {
          setSendProgress({ ...state.progress, target:sr.target || "", phase:sr.phase });
          const pct = Math.min(100, Math.round(state.progress.sent / state.progress.total * 100));
          if (pct > 0 && pct % 25 === 0 && pct !== lastLoggedPctRef.current) {
            lastLoggedPctRef.current = pct;
            addLog("STREAM", `进度同步: ${pct}% (${state.progress.sent} 行)`, "info");
          }
        }
        setOverlayStatus(state.status);
        if (TERMINAL_STATUSES.has(state.status)) {
          stop();
          if (pollingCleanupRef.current === stop) pollingCleanupRef.current = null;
          if (state.status === "completed") {
            addLog("SUCCESS", `[成功] ${sr.total_lines || state.progress?.total || 0} 行已确认，控制器完成状态已核验`, "ok");
          } else {
            addLog("TERMINAL", `任务记录为 ${state.status}：${sr.error || "请到设备旁确认实际状态"}`, "warn");
          }
        }
      },
    );
    pollingCleanupRef.current = stop;
  };

  const estopTarget = () => estopTargetArgs({
    connection_mode: connectionMode,
    port: serialPort,
    baudrate,
    network_host: networkHost,
    network_telnet_port: telnetPort,
  });
  // 急停（2026-11-05）：先按任务编号走原设备通道；任务已不在后端登记表（刚全量发送完等）或通道挂起时
  // 改按本页设备目标新开连接写 0x18，不再只提示失败。顶栏常驻急停若未停掉本页同一 running 设备任务
  // （如后端仅停了 unknown 旧记录或其他设备），同样由 `usePageEmergencyStop` 向本页设备目标补发。
  const handleEStop = usePageEmergencyStop(
    () => visibleEstopContext(currentWorkflow?.workflow_id, estopTarget(), sendProgress?.target),
    (outcome) => {
      const sent = outcome.kind === "sent";
      const message = sent
        ? `${outcome.message || "急停 0x18 已发送"}。停止请求已发送，但尚未确认设备已停止。请到设备旁确认，软件复位不能代替实体急停。`
        : `${outcome.message || "停止请求发送失败"}。请立即使用实体急停。`;
      setPollSafetyNotice(message);
      addLog("ESTOP", message, sent ? "warn" : "error");
      showAlert(message, { title:sent ? "停止请求已发送" : "停止未确认", tone:sent ? "warning" : "error" });
    },
  );

  return (
    <div className="home-page ob-page">
      {/* 左窄右宽双栏：左 ①②③ 输入（16 列）；右 ④ 加工监视舱 + 加工反馈（32 列）；底部 控制台抽屉 */}
      <WorkbenchCanvas>
        <WorkbenchLinkCard
          connectionForm={connectionForm}
          connectionStatus={connectionStatus}
          onOpenConnection={onOpenConnection}
        />

        <WbCard id="source" className="hud-card ob-card" label="加工内容">
          <FoldHeader id="wb-source" title="加工内容" n={2} />
            {/* Tool selection cards */}
            <div className="wb-grid">
              {[
                { id: "text", label: "矢量文字", icon: Type },
                { id: "image", label: "图片处理", icon: ImageIcon },
                { id: "gcode", label: "已有G码", icon: FileCode },
                { id: "draw", label: "创意画板", icon: PenTool, jump: "/draw" },
              ].map((t) => {
                const Icon = t.icon;
                const active = !t.jump && activeTool === t.id;
                return (
                  <button
                    type="button"
                    key={t.id}
                    onClick={() => (t.jump ? onNavigate(t.jump) : setActiveTool(t.id))}
                    className={`hud-tool-card ${active ? "active" : ""}`}
                    aria-pressed={t.jump ? undefined : active}
                    title={t.jump ? "跳转到创作画板" : undefined}
                  >
                    <Icon size={18} className="tool-icon" aria-hidden="true" />
                    <span className="hud-tool-label">
                      {t.label}
                      {t.jump ? <ArrowUpRight size={12} className="hud-tool-jump-icon" aria-hidden="true" /> : null}
                    </span>
                  </button>
                );
              })}
            </div>

            <WorkbenchRecentTasks onRecall={handleRecallTask} disabled={busy} />

            {/* Tool specific parameters */}
            {activeTool === "text" && (
              <WorkbenchTextTool
                textContent={textContent}
                setTextContent={setTextContent}
                textBorder={textBorder}
                setTextBorder={setTextBorder}
                fontSizeMm={fontSizeMm}
                setFontSizeMm={setFontSizeMm}
                textStyle={textStyleEffective}
                setTextStyle={setTextStyle}
                engraveRoute={engraveRoute}
                setEngraveRoute={setEngraveRoute}
                engraveRouteSaving={engraveRouteSaving}
                engraveRouteReadOnly={engraveRouteReadOnly}
                textOrigin={textOrigin}
                setTextOrigin={setTextOrigin}
                fillSettings={fillSettings}
                updateFillSettings={updateFillSettings}
                textUsesFill={textUsesFill}
                textIsCut={textIsCut}
                surfaceOnly={surfaceOnlyMaterial}
                materialApplied={materialApplied}
                materialLayer={materialLayer}
                saveFillToMaterial={saveFillToMaterial}
              />
            )}

            {activeTool === "image" && (
              <WorkbenchImageTool
                addLog={addLog}
                uploadedImagePath={uploadedImagePath}
                setUploadedImagePath={setUploadedImagePath}
                uploadedImageName={uploadedImageName}
                setUploadedImageName={setUploadedImageName}
                uploadedImageThumb={uploadedImageThumb}
                setUploadedImageThumb={setUploadedImageThumb}
                imageMode={imageMode}
                setImageMode={setImageMode}
                imageExtras={imageExtras}
                setImageExtra={setImageExtra}
                imageFields={imageFields}
                applyImageQualityPreset={applyImageQualityPreset}
                imageWidthMm={imageWidthMm}
                setImageWidthMm={setImageWidthMm}
                imageHeightMm={imageHeightMm}
                setImageHeightMm={setImageHeightMm}
                fillSettings={fillSettings}
                updateFillSettings={updateFillSettings}
                maxWidthMm={maxWidthMm}
                maxHeightMm={maxHeightMm}
              />
            )}

            {activeTool === "gcode" && (
              <WorkbenchGcodeTool
                addLog={addLog}
                uploadedGcodePath={uploadedGcodePath}
                setUploadedGcodePath={setUploadedGcodePath}
                uploadedGcodeName={uploadedGcodeName}
                setUploadedGcodeName={setUploadedGcodeName}
              />
            )}
        </WbCard>

        <WorkbenchMaterialCard
          activeTool={activeTool}
          textStyle={textStyleEffective}
          materials={materials}
          selectedMaterial={selectedMaterial}
          setSelectedMaterial={setSelectedMaterial}
          selectedThickness={selectedThickness}
          setSelectedThickness={setSelectedThickness}
          availableThicknesses={availableThicknesses}
          powerPercent={powerPercent}
          recommendedFeedRate={recommendedFeedRate}
          recommendedPasses={recommendedPasses}
          hasRecommendation={hasRecommendation}
          setMaterialModalData={setMaterialModalData}
          setShowMaterialModal={setShowMaterialModal}
          customParams={customParams}
          setCustomParams={setCustomParams}
          customPower={customPower}
          setCustomPower={setCustomPower}
          customFeed={customFeed}
          setCustomFeed={setCustomFeed}
          customPasses={customPasses}
          setCustomPasses={setCustomPasses}
        />

        <WorkbenchMonitorCard
          onNavigate={onNavigate}
          previewUrl={previewUrl}
          previewStale={previewStale}
          currentWorkflow={currentWorkflow}
          dimensions={dimensions}
          sendProgress={sendProgress}
          gcodeText={gcodeText}
          timeEstimate={timeEstimate}
          fitNote={fitNote}
          routeNote={routeNote}
          fillNote={fillNote}
          busy={busy}
          framingBusy={framingBusy}
          canGenerate={activeTool !== "text" || Boolean(textContent.trim())}
          overlayStatus={overlayStatus}
          framingShape={framingShape}
          setFramingShape={setFramingShape}
          connectionForm={connectionForm}
          connectionStatus={connectionStatus}
          activeJobs={activeJobs}
          motionPayload={motionPayload}
          addLog={addLog}
          adoptDerivedWorkflow={adoptDerivedWorkflow}
          handleGeneratePreview={handleGeneratePreview}
          handleConfirmSend={handleConfirmSend}
          handleResumeSend={handleResumeSend}
          handleFraming={handleFraming}
          handleExportGcode={handleExportGcode}
          handleCancelJob={handleCancelJob}
          handleEStop={handleEStop}
        />

        {activeTool !== "gcode" && (
          <WbCard id="feedback" className="hud-card ob-card" label="加工反馈">
            <FoldHeader
              id="wb-feedback"
              title="加工反馈"
              defaultOpen={Boolean(currentWorkflow?.workflow_id || currentWorkflow?.feedback_origin)}
              persist={false}
              autoExpandKey={
                currentWorkflow?.workflow_id
                  ? `${currentWorkflow.workflow_id}:${overlayStatus === "completed" ? "completed" : "ready"}`
                  : ""
              }
            />
            {!currentWorkflow?.workflow_id && (
              <p className="ob-sub wb-note">生成加工轨迹后，可在这里记录加工效果并获取调参建议。</p>
            )}
              {/* 数控中控台与实时调参反馈卡片 */}
              <FeedbackPanel workflowId={currentWorkflow?.workflow_id} origin={currentWorkflow?.feedback_origin}
                showHeading={false}
                disabled={busy || framingBusy || previewStale || ["running", "sending", "preparing", "awaiting_completion"].includes(overlayStatus)}
                onAdopt={(workflow) => adoptDerivedWorkflow(workflow, "调参预览")} />
          </WbCard>
        )}

      </WorkbenchCanvas>

      {/* 底部三标签遥测控制台抽屉 (ConsoleDrawer) */}
      <ConsoleDrawer
        logs={logs}
        onClear={() => setLogs([])}
        gcodeText={gcodeText}
        onClearGcode={() => {
          setGcodeText("");
          addLog("GCODE", "已清空当前 G-code 刀路视图", "info");
        }}
        currentLine={sendProgress?.sent || 0}
        totalLines={sendProgress?.total || 0}
        mcpStatus={effectiveMcpStatus}
      />

      {pollSafetyNotice && <p role="alert" className="send-desc">{pollSafetyNotice}</p>}

      {/* Material Modal */}
      <MaterialModal
        visible={showMaterialModal}
        initialData={materialModalData}
        onClose={() => setShowMaterialModal(false)}
        onSaved={loadMaterials}
        onDeleted={loadMaterials}
      />
    </div>
  );
}
