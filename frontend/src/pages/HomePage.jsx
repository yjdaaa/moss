import React, { useState, useEffect, useRef } from "react";
import {
  Cpu,
  Download,
  Flame,
  PauseCircle,
  RefreshCw,
  Sliders,
  Square,
  Type,
  FileCode,
  Image as ImageIcon,
  PenTool,
  AlertTriangle,
  Clock,
  Ruler,
  Box,
} from "lucide-react";
import { getJson, postJson, fetchPreviewBlobUrl, fetchPreviewText, resolveApiUrl } from "../api/client.js";
import { isRemoteMode } from "../authSession.js";
import { MaterialModal } from "../components/MaterialModal.jsx";
import { ConsoleDrawer } from "../components/ConsoleDrawer.jsx";
import { FeedbackPanel } from "../components/FeedbackPanel.jsx";
import { extractWorkflowInfo, TERMINAL_STATUSES } from "../state/statusNormalize.js";
import {
  actualParamsText,
  IMAGE_MODE_OPTIONS,
  imageParamsPayload,
  isWorkbenchPreviewStale,
  MANUAL_LIMITS,
  manualParamsPayload,
  materialRecommendationModes,
  resolveTextStyle,
  TEXT_ORIGIN_OPTIONS,
  TEXT_STYLE_OPTIONS,
  workbenchConfirmSendPrompt,
  workbenchInputSignature,
  workbenchResumeSendPrompt,
  workbenchStatusLabel,
} from "../workbenchParams.js";
import { framingRequest } from "../aiPreviewState.js";
import { drawConnectionPayload, drawTargetLabel, drawTargetReady } from "../drawConnection.js";
import { exportFile } from "../saveFile.js";
import { showAlert, showConfirm, showPrompt } from "../appDialog.js";
import { canResumeJob, formatResumeActionLabel } from "../estopResumeState.js";
import { estopTargetArgs } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { WbCard, WorkbenchCanvas } from "../components/WorkbenchCanvas.jsx";
import { defaultSerialPortHint } from "../platform.js";
import { MATERIAL_LIBRARY_CHANNEL, pickThicknessForMaterial } from "../materialSelectState.js";
import { controllerLabel, discoveryOutcome } from "../controllerDiscovery.js";

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

export function HomePage({ onNavigate, connectionStatus, setConnectionStatus }) {
  // Connection state
  const [connectionMode, setConnectionMode] = useState(
    () => readLocal("laser-forge-conn-mode", "network") || "network"
  );
  const [serialPort, setSerialPort] = useState(
    () => readLocal("laser-forge-serial-port", "")
  );
  const [baudrate, setBaudrate] = useState(
    () => Number(readLocal("laser-forge-baud", "115200") || 115200)
  );
  const [serialPorts, setSerialPorts] = useState([]);
  const [networkHost, setNetworkHost] = useState(
    () => readLocal("laser-forge-host", "")
  );
  const [telnetPort, setTelnetPort] = useState(
    () => Number(readLocal("laser-forge-telnet-port", "23") || 23)
  );

  // Active Tool Mode
  const [activeTool, setActiveTool] = useState("text"); // text, image, gcode

  // Tool specific inputs（图片与已有 G-code 独立存放，互不串文件）
  const [textContent, setTextContent] = useState(
    () => readLocal("laser-forge-text", "MOSS") || "MOSS"
  );
  const [fontSizeMm, setFontSizeMm] = useState(
    () => Number(readLocal("laser-forge-font-size", "12") || 12)
  );
  const [textStyle, setTextStyle] = useState(
    () => resolveTextStyle(readLocal("laser-forge-text-style", "outline")).style
  );
  const [textOrigin, setTextOrigin] = useState("center");

  const [uploadedImagePath, setUploadedImagePath] = useState("");
  const [uploadedImageName, setUploadedImageName] = useState("");
  const [uploadedImageThumb, setUploadedImageThumb] = useState("");
  const [imageWidthMm, setImageWidthMm] = useState("");
  const [imageHeightMm, setImageHeightMm] = useState("");
  const [imageMode, setImageMode] = useState("auto");

  const [uploadedGcodePath, setUploadedGcodePath] = useState("");
  const [uploadedGcodeName, setUploadedGcodeName] = useState("");

  // Material and Parameter state
  const [materials, setMaterials] = useState([]);
  const [selectedMaterial, setSelectedMaterial] = useState(
    () => readLocal("laser-forge-mat", "椴木") || "椴木"
  );
  const [selectedThickness, setSelectedThickness] = useState(
    () => readLocal("laser-forge-thick", "3") || "3"
  );
  const [powerPercent, setPowerPercent] = useState(50);
  const [speedMms, setSpeedMms] = useState(30);
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
  const [busy, setBusy] = useState(false);
  const [framingBusy, setFramingBusy] = useState(false);

  const currentSignature = workbenchInputSignature({
    activeTool,
    textContent,
    fontSizeMm,
    textStyle,
    textOrigin,
    uploadedImagePath,
    imageWidthMm,
    imageHeightMm,
    imageMode,
    uploadedGcodePath,
    selectedMaterial,
    selectedThickness,
    customParams,
    customPower,
    customFeed,
    customPasses,
  });
  const previewStale = Boolean(currentWorkflow?.workflow_id) && isWorkbenchPreviewStale(previewSignature, currentSignature);

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
  const [pollIntervalId, setPollIntervalId] = useState(null);
  const [pollSafetyNotice, setPollSafetyNotice] = useState("");
  const pollingCleanupRef = useRef(null);
  useEffect(() => () => pollingCleanupRef.current?.(), []);
  const [showMaterialModal, setShowMaterialModal] = useState(false);
  const [materialModalData, setMaterialModalData] = useState(null);

  // Logs & Telemetry（正序追加：从上往下滚动，最新日志位于最底部）
  const [logs, setLogs] = useState([]);
  const [gcodeText, setGcodeText] = useState("");
  const [mcpStatus, setMcpStatus] = useState(null);
  const lastLoggedPctRef = useRef(-1);

  const addLog = (tag, message, level = "info") => {
    const time = new Date().toLocaleTimeString();
    setLogs((prev) => [...prev.slice(-99), { time, tag, message, level }]);
  };

  // Initial Load: Materials & Config & 优化1: 启动自动静默握手一次
  useEffect(() => {
    loadMaterials();
    loadSerialPorts();
    loadRuntimeConfig();

    // 材料实验室修改后经 BroadcastChannel 跨页同步刷新
    let channel = null;
    try {
      if (typeof BroadcastChannel === "function") {
        channel = new BroadcastChannel(MATERIAL_LIBRARY_CHANNEL);
        channel.addEventListener("message", (event) => {
          if (event?.data?.type === "materials-updated") loadMaterials();
        });
      }
    } catch {
      channel = null;
    }

    // 优化 1: 仅在应用启动后尚未探测过时自动静默握手一次；从其他页面切回工作台时不重复探测
    if (connectionStatus?.checkedAt) {
      return () => channel?.close?.();
    }
    const timer = setTimeout(() => {
      if ((connectionMode === "network" && networkHost) || (connectionMode === "serial" && serialPort)) {
        performConnectionProbe(true);
      }
    }, 600);
    return () => {
      clearTimeout(timer);
      channel?.close?.();
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
      ["laser-forge-conn-mode", connectionMode],
      ["laser-forge-serial-port", serialPort],
      ["laser-forge-baud", String(baudrate)],
      ["laser-forge-host", networkHost],
      ["laser-forge-telnet-port", String(telnetPort)],
      ["laser-forge-mat", selectedMaterial],
      ["laser-forge-thick", selectedThickness],
      ["laser-forge-text", textContent],
      ["laser-forge-font-size", String(fontSizeMm)],
      ["laser-forge-text-style", textStyle],
    ];
    for (const [key, val] of entries) {
      if (persistedRef.current[key] !== val) {
        persistedRef.current[key] = val;
        writeLocal(key, val);
      }
    }
  }, [
    connectionMode,
    serialPort,
    baudrate,
    networkHost,
    telnetPort,
    selectedMaterial,
    selectedThickness,
    textContent,
    fontSizeMm,
    textStyle,
  ]);

  const loadMaterials = async () => {
    const res = await getJson("/api/ui/material-options");
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

  const loadSerialPorts = async (showFeedback = false) => {
    try {
      const res = await getJson("/api/ui/serial-ports");
      if (res.ok && res.data?.success) {
        const ports = res.data.result?.ports || [];
        setSerialPorts(ports);
        if (ports.length > 0) {
          if (!serialPort || !ports.includes(serialPort)) {
            setSerialPort(ports[0]);
          }
        } else {
          setSerialPort("");
        }
        if (showFeedback) {
          addLog("SERIAL", `扫描完成，发现可用串口: ${ports.length > 0 ? ports.join(", ") : "未检测到可用串口"}`, ports.length > 0 ? "ok" : "warn");
        }
      } else if (showFeedback) {
        addLog("SERIAL", "获取串口列表失败", "warn");
      }
    } catch (e) {
      if (showFeedback) addLog("SERIAL", `探测异常: ${e.message}`, "error");
    }
  };

  const loadRuntimeConfig = async () => {
    const res = await getJson("/api/ui/runtime-config");
    if (res.ok && res.data?.success) {
      const conn = res.data.result?.connection;
      if (conn?.network?.default_host && !networkHost) {
        setNetworkHost(conn.network.default_host);
      }
    }
    // 读取系统信息中的真实小智 MCP 状态
    const sysRes = await getJson("/api/system/info");
    if (sysRes.ok && sysRes.data?.success && sysRes.data?.result?.mcp) {
      setMcpStatus(sysRes.data.result.mcp);
    }
  };

  // 联动加载选定材质、厚度与当前加工内容对应的真实推荐工艺参数
  useEffect(() => {
    let cancelled = false;
    async function syncMaterialRecommendation() {
      if (!selectedMaterial || !selectedThickness) {
        setHasRecommendation(false);
        return;
      }
      const recMode = materialRecommendationModes({ activeTool, textStyle });
      const query = new URLSearchParams({
        material: selectedMaterial,
        thickness_mm: String(selectedThickness),
        laser_mode: recMode.laserMode,
      });
      if (recMode.engravingMode) {
        query.set("engraving_mode", recMode.engravingMode);
      }
      const res = await getJson(`/api/ui/material-recommendation?${query.toString()}`);
      if (cancelled) return;
      if (res.ok && res.data?.success && res.data.result) {
        const rec = res.data.result;
        if (rec.laser_max_power != null) {
          setPowerPercent(Math.round(rec.laser_max_power / 10));
        }
        if (rec.feed_rate != null) {
          setRecommendedFeedRate(Math.round(rec.feed_rate));
          setSpeedMms(Math.max(1, Math.round(rec.feed_rate / 60)));
        }
        setRecommendedPasses(Math.max(1, Math.round(rec.passes || 1)));
        setHasRecommendation(true);
      } else {
        setHasRecommendation(false);
      }
    }
    syncMaterialRecommendation();
    return () => {
      cancelled = true;
    };
  }, [selectedMaterial, selectedThickness, activeTool, textStyle, recRefreshTick]);

  // Switch material -> update thickness list
  const currentMaterialObj = materials.find((m) => m.name === selectedMaterial);
  const availableThicknesses = currentMaterialObj?.thicknesses || [1, 2, 3, 5];

  // 通用硬件握手探测方法（支持静默模式与主动模式）
  const performConnectionProbe = async (silent = false) => {
    const epoch = ++probeEpochRef.current;
    if (!silent) {
      addLog("DEVICE", `正在探测 ${connectionMode === "serial" ? "串口" : "网络"} 模式连接...`, "info");
    }
    const payload = {
      connection_mode: connectionMode,
      port: serialPort,
      baudrate: Number(baudrate),
      network_host: networkHost,
      network_telnet_port: Number(telnetPort),
    };
    try {
      const res = await postJson("/api/connection/check", payload);
      if (probeEpochRef.current !== epoch) return false;
      if (res.ok && res.data?.success) {
        const result = res.data.result;
        const isConnected =
          connectionMode === "serial" ? result.serial_connected : result.network_connected;
        setConnectionStatus({ connected: isConnected, summary: result.summary, checkedAt: Date.now() });
        if (isConnected) {
          addLog("DEVICE", `硬件握手通过: ${result.summary || "控制器已连接 ($32=1 就绪)"}；语音和 AI 发送时没指定串口或 IP 就用这个连接`, "ok");
        } else if (!silent) {
          addLog("DEVICE", result.summary || "未检测到控制器连接", "warn");
        }
        return isConnected;
      } else {
        const errorMsg = res.data?.result || "探测失败：未检测到控制器连接";
        setConnectionStatus({ connected: false, summary: errorMsg, checkedAt: Date.now() });
        if (!silent) {
          addLog("DEVICE", errorMsg, "error");
        }
        return false;
      }
    } catch (e) {
      if (probeEpochRef.current !== epoch) return false;
      setConnectionStatus({ connected: false, summary: "连接探测网络异常", checkedAt: Date.now() });
      if (!silent) {
        addLog("DEVICE", `探测网络异常: ${e.message}`, "error");
      }
      return false;
    }
  };

  // Test connection (用户主动点击)
  const handleTestConnection = () => performConnectionProbe(false);

  // 局域网发现（2026-11-09）：mDNS 与本网段 23 端口只是候选，后端逐台发只读的 `?` / `$I`，
  // 按 GRBL / FluidNC 回应的才列出；只找到 1 台时自动填入，多台时让用户选，不替用户挑第一个
  const [discovering, setDiscovering] = useState(false);
  const [discoveredChoices, setDiscoveredChoices] = useState([]);
  const pickController = (controller) => {
    setNetworkHost(controller.ip);
    if (controller.telnet_port) setTelnetPort(controller.telnet_port);
  };
  const discoverControllers = async () => {
    if (discovering) return;
    setDiscovering(true);
    setDiscoveredChoices([]);
    addLog("DEVICE", "正在查找局域网里的雕刻机，并逐台发送只读查询确认（约 3～5 秒）...", "info");
    try {
      const res = await postJson("/api/ui/discover-controllers", {});
      if (!res.ok || !res.data?.success) {
        addLog("DEVICE", `扫描失败: ${res.data?.result || res.error || "后端无响应"}`, "error");
        return;
      }
      const outcome = discoveryOutcome(res.data.result);
      if (outcome.fill) pickController(outcome.fill);
      setDiscoveredChoices(outcome.choices);
      addLog("DEVICE", outcome.log.text, outcome.log.level);
    } catch (e) {
      addLog("DEVICE", `扫描失败: ${e.message}`, "error");
    } finally {
      setDiscovering(false);
    }
  };

  const connectionForm = {
    connection_mode: connectionMode,
    port: serialPort,
    baudrate: String(baudrate),
    network_host: networkHost,
    network_telnet_port: String(telnetPort),
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
        const spec = resolveTextStyle(textStyle);
        res = await postJson("/api/text-task/generate", {
          text: textContent.trim(),
          material: selectedMaterial,
          thickness_mm: Number(selectedThickness),
          font_size_mm: Number(fontSizeMm),
          laser_mode: spec.laserMode,
          engraving_mode: spec.engravingMode,
          text_fill: spec.textFill,
          origin: textOrigin,
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
        });
        if (imgExtra.error) {
          showAlert(imgExtra.error, { title: "图片尺寸填写有误", tone: "warning" });
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
          ...imgExtra,
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
        });
      }

      setBusy(false);
      if (res?.ok && res.data?.success) {
        const wf = res.data.result;
        if (pollIntervalId) pollIntervalId();
        setPollIntervalId(null);
        setOverlayStatus("pending");
        setSendProgress(null);
        setPollSafetyNotice("");
        setCurrentWorkflow(wf);
        setPreviewSignature(signatureSnapshot);
        const info = extractWorkflowInfo(wf);
        const artifacts = wf.artifacts || {};

        if (artifacts.image_file) {
          // 连接电脑时图片需要带配对 token，<img> 直链拿不到，才经鉴权取回 blob；本机/桌面直接用直链，避免泄漏 blob
          const nextUrl = await loadWorkbenchPreviewUrl(artifacts.image_file, {
            remote: isRemoteMode(),
            fetchBlobUrl: fetchPreviewBlobUrl,
            resolveUrl: resolveApiUrl,
          });
          applyPreviewUrl(nextUrl);
        }
        if (artifacts.gcode_file) {
          // 2026-11-05：先核对 HTTP 状态，错误页不得当 G-code 显示；SHA-256 只用后端给出的真实值，
          // 拿不到时如实说明不可用，绝不伪造摘要或声称门禁已通过（发送时后端仍按物理文件重算）。
          void fetchPreviewText(artifacts.gcode_file).then((preview) => {
            if (!mountedRef.current) return;
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

  // 先走边框（会真实驱动激光头移动，单击不得直接执行，须先经 showConfirm 二次确认）
  const handleFraming = async () => {
    if (busy || framingBusy || !currentWorkflow?.workflow_id) return;
    if (previewStale) {
      showAlert("左侧参数已改动，请先点击【重新生成轨迹】再走边框。", { title: "预览已过期", tone: "warning" });
      return;
    }
    if (!drawTargetReady(connectionForm)) {
      showAlert(
        connectionMode === "serial" ? "请先在左上角「通信链路」选择有效串口" : "请先在左上角「通信链路」填写控制器 IP 地址",
        { title: "尚未指定雕刻机", tone: "warning" },
      );
      return;
    }
    const req = framingRequest(currentWorkflow, drawConnectionPayload(connectionForm));
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
      `目标设备：${targetName}\n激光头将从当前位置出发，沿 ${w} × ${h} mm 的范围绕一圈再回到原处（默认不出光）。\n请确认机器周围无障碍物；运行期间顶栏急停可随时停止。`,
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
      if (res.ok && res.data?.success) {
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
    const prompt = workbenchConfirmSendPrompt({
      workflow: currentWorkflow,
      material: selectedMaterial,
      thickness: selectedThickness,
      targetLabel: targetName || (connectionMode === "serial" ? serialPort || "未选串口" : `${networkHost || "未填 IP"}:${telnetPort}`),
      dimensions,
      lastSentSha256,
    });
    const confirmed = await showConfirm(prompt.message, {
      title: prompt.title,
      tone: "warning",
      danger: prompt.danger,
      confirmText: prompt.confirmText,
      cancelText: "取消",
    });
    if (!confirmed) {
      addLog("DISPATCH", "已取消本次开刻确认，机器未启动", "info");
      return;
    }

    // 优化 2: 点击确认开刻后自动兜底校验握手
    let isConnected = connectionStatus?.connected;
    if (!isConnected) {
      addLog("GATE", "检测到硬件尚未握手，正在自动执行快速握手校验...", "info");
      isConnected = await performConnectionProbe(true);
    }

    // 强连通性前置硬门禁：如果设备确实未连接，严禁下发伪发送，坚决报错拦截
    if (!isConnected) {
      const hint = connectionMode === "serial"
        ? `未检测到有效串口连接（当前串口: ${serialPort || "未选择"}）。请确保 USB 数据线已插入且驱动正常！`
        : `未检测到网络控制器连接（当前设备: ${networkHost || "未填写 IP"}:${telnetPort}）。请确保控制器已开机且局域网可达！`;
      addLog("GATE", `发送被安全门禁拦截：${hint}`, "error");
      showAlert(hint, { title: "发送失败 · 设备未连接", tone: "error", confirmText: "知道了" });
      return;
    }

    lastLoggedPctRef.current = -1;
    addLog("DISPATCH", "安全门禁校验通过，开始逐行下发加工指令...", "info");
    setOverlayStatus("preparing");

    const payload = {
      action: "confirm_send",
      workflow_id: currentWorkflow.workflow_id,
      confirmed: true,
      connection_mode: connectionMode,
      port: serialPort,
      baudrate: Number(baudrate),
      network_host: networkHost,
      network_telnet_port: Number(telnetPort),
    };

    try {
      const res = await postJson("/api/workflow", payload);
      if (res.ok && res.data?.success) {
        if (prompt.sha256) {
          setLastSentSha256(prompt.sha256);
          writeLocal("laser-forge-last-sent-sha256", prompt.sha256);
        }
        const sr = res.data.result?.send_result || {};
        const total = sr.total_lines || (gcodeText ? gcodeText.split("\n").length : 0);
        setSendProgress({ sent: 0, total, target: sr.target || "" });
        addLog("STREAM", `进度同步: 0% (0 / ${total} 行)`, "info");
        setOverlayStatus("running");
        startStatusPolling(currentWorkflow.workflow_id);
      } else {
        setOverlayStatus("failed");
        const reason = res.data?.result || "发送门禁拦截或连接失败";
        addLog("DISPATCH", reason, "error");
        showAlert(reason, { title: "发送失败", tone: "error" });
      }
    } catch (e) {
      setOverlayStatus("failed");
      addLog("DISPATCH", `发送网络异常: ${e.message}`, "error");
    }
  };

  // Resume Send (断点续雕：从中断的断点行继续下发剩余 G-code)
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

    const startLine = (sendProgress?.sent || 0) + 1;
    const totalLines = sendProgress?.total || 0;
    const targetName = drawTargetReady(connectionForm) ? drawTargetLabel(connectionForm) : "";
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

    let isConnected = connectionStatus?.connected;
    if (!isConnected) {
      addLog("GATE", "正在自动执行快速握手校验...", "info");
      isConnected = await performConnectionProbe(true);
    }
    if (!isConnected) {
      showAlert("未检测到控制器连接，请确认设备已开机并完成握手！", { title: "断点续雕失败", tone: "error" });
      return;
    }

    addLog("DISPATCH", `正在从第 ${startLine} 行发起断点续雕，剩余代码即将继续流式下发...`, "info");
    setOverlayStatus("preparing");

    const payload = {
      action: "resume",
      workflow_id: currentWorkflow.workflow_id,
      confirmed: true,
      connection_mode: connectionMode,
      port: serialPort,
      baudrate: Number(baudrate),
      network_host: networkHost,
      network_telnet_port: Number(telnetPort),
    };

    try {
      const res = await postJson("/api/workflow", payload);
      if (res.ok && res.data?.success) {
        const sr = res.data.result?.send_result || {};
        const total = sr.total_lines || sendProgress?.total || 0;
        const sent = sr.lines_sent || sendProgress?.sent || 0;
        setSendProgress({ sent, total, target: sr.target || "" });
        addLog("STREAM", `断点续雕已启动：正在从第 ${sent + 1}/${total} 行继续执行`, "ok");
        setOverlayStatus("running");
        startStatusPolling(currentWorkflow.workflow_id);
      } else {
        setOverlayStatus("failed");
        const reason = res.data?.result || "断点恢复失败";
        addLog("DISPATCH", reason, "error");
        showAlert(reason, { title: "断点恢复失败", tone: "error" });
      }
    } catch (e) {
      setOverlayStatus("failed");
      addLog("DISPATCH", `恢复发送网络异常: ${e.message}`, "error");
    }
  };

  // Query errors preserve the last known job state and never remove its stop controls.
  const startStatusPolling = (workflowId) => {
    pollingCleanupRef.current?.();
    let state = { id:workflowId, kind:"workflow", status:"sending", progress:sendProgress };
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
        if (state.progress) {
          setSendProgress({ ...state.progress, target:sr.target || "", phase:sr.phase });
          const pct = Math.min(100, Math.round(state.progress.sent / state.progress.total * 100));
          if (pct > 0 && pct % 25 === 0 && pct !== lastLoggedPctRef.current) {
            lastLoggedPctRef.current = pct;
            addLog("STREAM", `进度同步: ${pct}% (${state.progress.sent} 行)`, "info");
          }
        }
        setOverlayStatus(state.status);
        if (TERMINAL_STATUSES.has(state.status)) {
          stop(); setPollIntervalId(null);
          if (state.status === "completed") {
            addLog("SUCCESS", `[成功] ${sr.total_lines || state.progress?.total || 0} 行已确认，控制器完成状态已核验`, "ok");
          } else {
            addLog("TERMINAL", `任务记录为 ${state.status}：${sr.error || "请到设备旁确认实际状态"}`, "warn");
          }
        }
      },
    );
    pollingCleanupRef.current = stop;
    setPollIntervalId(() => stop);
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
    () => ({ jobId: currentWorkflow?.workflow_id, target: estopTarget() }),
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
        <WbCard id="link" className="hud-card ob-card" label="通信链路">
          <FoldHeader id="wb-link" title="通信链路" k="LINK" n={1} />
        {/* Connection Bar */}
        <div
          className="connection-bar wb-row"
        >
          <div className="wb-row-2">
            <span className="wb-text-2">连接方式</span>
            <select
              value={connectionMode}
              onChange={(e) => setConnectionMode(e.target.value)}
              className="select-input wb-field"
            >
              <option value="network">网络 (Telnet/TCP)</option>
              <option value="serial">串口 (USB/Serial)</option>
            </select>
          </div>

          {connectionMode === "serial" ? (
            <div className="wb-row-2">
              <select
                value={serialPort}
                onChange={async (e) => {
                  const selectEl = e.target;
                  const chosen = selectEl.value;
                  if (chosen === "__custom__") {
                    selectEl.value = serialPort || "";
                    const serialHint = defaultSerialPortHint();
                    const custom = await showPrompt(serialHint.prompt, {
                      title: "手动输入串口号",
                      defaultValue: serialPort || serialHint.fallback,
                      placeholder: serialHint.fallback,
                    });
                    if (custom && custom.trim()) {
                      const p = custom.trim();
                      if (!serialPorts.includes(p)) {
                        setSerialPorts((prev) => [...prev, p]);
                      }
                      setSerialPort(p);
                    }
                  } else {
                    setSerialPort(chosen);
                  }
                }}
                className="select-input"
              >
                {serialPort && !serialPorts.includes(serialPort) ? (
                  <option value={serialPort}>{serialPort}</option>
                ) : null}
                {serialPorts.length === 0 && !serialPort ? (
                  <option value="" disabled>未检测到可用串口 (请点击右侧刷新)</option>
                ) : (
                  serialPorts.map((p) => (
                    <option key={p} value={p}>{p}</option>
                  ))
                )}
                <option value="__custom__">✎ 手动输入串口…</option>
              </select>
              <select
                value={baudrate}
                onChange={(e) => setBaudrate(Number(e.target.value))}
                className="select-input"
              >
                <option value={9600}>9600 波特率</option>
                <option value={19200}>19200 波特率</option>
                <option value={38400}>38400 波特率</option>
                <option value={57600}>57600 波特率</option>
                <option value={115200}>115200 波特率 (默认)</option>
                <option value={230400}>230400 波特率</option>
                <option value={250000}>250000 波特率</option>
                <option value={460800}>460800 波特率</option>
                <option value={921600}>921600 波特率</option>
              </select>
              <button className="btn btn-ghost" onClick={() => loadSerialPorts(true)} title="如同串口助手立即重新扫描系统串口">
                <RefreshCw size={13} />
              </button>
            </div>
          ) : (
            <div className="wb-row-2">
              <input
                type="text"
                placeholder="控制器 IP (例 192.168.1.10)"
                value={networkHost}
                onChange={(e) => setNetworkHost(e.target.value)}
                className="text-input wb-field-2"
              />
              <span className="wb-text-2">端口</span>
              <input
                type="number"
                value={telnetPort}
                onChange={(e) => setTelnetPort(Number(e.target.value))}
                className="text-input wb-field-3"
              />
            </div>
          )}
          {connectionMode === "network" && discoveredChoices.length > 1 && (
            <div className="wb-row-2">
              <span className="wb-text-2">选择雕刻机:</span>
              <select
                className="select-input wb-field"
                id="wb-discovered-controllers"
                value={discoveredChoices.find((c) => c.ip === networkHost && c.telnet_port === telnetPort) ? `${networkHost}:${telnetPort}` : ""}
                onChange={(e) => {
                  const picked = discoveredChoices.find((c) => `${c.ip}:${c.telnet_port}` === e.target.value);
                  if (picked) pickController(picked);
                }}
              >
                <option value="" disabled>找到 {discoveredChoices.length} 台，请选择…</option>
                {discoveredChoices.map((c) => (
                  <option key={`${c.ip}:${c.telnet_port}`} value={`${c.ip}:${c.telnet_port}`}>{controllerLabel(c)}</option>
                ))}
              </select>
            </div>
          )}
          {connectionMode === "serial" && (
            <p className="wb-hint" id="wb-serial-probe-hint">
              点“握手探测”会打开所选串口确认是不是雕刻机；部分 ESP32 / Arduino 板子打开串口时会自动重启，加工中不要探测别的串口。
            </p>
          )}

          <div className={`wb-link-actions${connectionMode === "network" ? " is-dual" : ""}`}>
            {connectionMode === "network" && (
              <button
                className="btn btn-ghost"
                onClick={discoverControllers}
                disabled={discovering}
                title="查找局域网里的设备（mDNS 广播 + 本网段 23 端口），逐台发送只读的状态查询，只列出按 GRBL / FluidNC 回应的雕刻机"
              >
                <RefreshCw size={13} className="wb-icon" />
                <span>{discovering ? "扫描中…" : "扫描局域网"}</span>
              </button>
            )}
            <button className="btn btn-ghost wb-btn" onClick={handleTestConnection}>
              <RefreshCw size={13} className="wb-icon" />
              <span>握手探测</span>
            </button>
          </div>
        </div>
        </WbCard>

        <WbCard id="source" className="hud-card ob-card" label="加工内容">
          <FoldHeader id="wb-source" title="加工内容" k="SOURCE" n={2} />
            {/* Tool selection cards */}
            <div className="wb-grid">
              {[
                { id: "text", label: "矢量文字", sub: "TEXT", icon: Type },
                { id: "image", label: "图片处理", sub: "RASTER", icon: ImageIcon },
                { id: "gcode", label: "已有G码", sub: "G-CODE", icon: FileCode },
                { id: "draw", label: "创意画板", sub: "跳转 ↗", icon: PenTool, jump: "/draw" },
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
                  >
                    <Icon size={18} className="tool-icon" aria-hidden="true" />
                    <span className="hud-tool-label">{t.label}</span>
                    <span className="hud-tool-sub">{t.sub}</span>
                  </button>
                );
              })}
            </div>

            {/* Tool specific parameters */}
            {activeTool === "text" && (
              <div className="tool-body wb-spacer">
                <div className="wb-spacer-2">
                  <div className="wb-row-3">
                    <label className="wb-label">
                      输入文本内容
                    </label>
                    <span style={{ fontSize: "11px", fontFamily: "var(--font-mono)", color: textContent.length > 20 ? "var(--gold)" : "var(--ink-dim)" }}>
                      {textContent.length} / 24 字符
                    </span>
                  </div>
                  <input
                    type="text"
                    maxLength={24}
                    value={textContent}
                    onChange={(e) => setTextContent(e.target.value)}
                    className="text-input wb-field-4"
                    placeholder="输入要雕刻的字符..."
                  />
                </div>
                <div className="wb-params-grid">
                  <label>
                    <span>字高 (mm)</span>
                    <input
                      type="number"
                      min={1}
                      max={100}
                      value={fontSizeMm}
                      onChange={(e) => setFontSizeMm(Number(e.target.value))}
                      className="text-input wb-field-5"
                    />
                  </label>
                  <label>
                    <span>刻法模式</span>
                    <select
                      id="wb-text-style"
                      value={textStyle}
                      onChange={(e) => setTextStyle(e.target.value)}
                      className="select-input wb-compact-select"
                    >
                      {TEXT_STYLE_OPTIONS.map((opt) => (
                        <option key={opt.value} value={opt.value}>{opt.label}</option>
                      ))}
                    </select>
                  </label>
                  <label>
                    <span>对刀起点</span>
                    <select
                      id="wb-text-origin"
                      value={textOrigin}
                      onChange={(e) => setTextOrigin(e.target.value)}
                      className="select-input wb-compact-select"
                    >
                      {TEXT_ORIGIN_OPTIONS.map((opt) => (
                        <option key={opt.value} value={opt.value}>{opt.label}</option>
                      ))}
                    </select>
                  </label>
                </div>
              </div>
            )}

            {activeTool === "image" && (
              <div className="tool-body wb-spacer">
                <div className="wb-spacer-2">
                  <label className="wb-label-2">
                    上传图像文件 (PNG / JPG / BMP / GIF / WebP)
                  </label>
                  <input
                    type="file"
                    accept=".png,.jpg,.jpeg,.bmp,.gif,.webp"
                    onChange={async (e) => {
                      const file = e.target.files?.[0];
                      e.target.value = "";
                      if (!file) return;
                      if (file.size > 14 * 1024 * 1024) {
                        showAlert("图片文件过大（上限 14 MB，Base64 传输请求体限 20 MB），请先压缩分辨率后再上传。", {
                          title: "图片超出大小限制",
                          tone: "warning",
                        });
                        return;
                      }
                      addLog("FILE", `正在读取素材: ${file.name}...`, "info");
                      const reader = new FileReader();
                      reader.onload = async () => {
                        const dataUrl = typeof reader.result === "string" ? reader.result : "";
                        try {
                          const res = await postJson("/api/upload", {
                            filename: file.name,
                            content_base64: dataUrl,
                          });
                          if (res.ok && res.data?.success) {
                            setUploadedImagePath(res.data.result?.file_path || file.name);
                            setUploadedImageName(file.name);
                            setUploadedImageThumb(dataUrl);
                            addLog("FILE", `已保存至工作区: ${file.name}`, "ok");
                          } else {
                            const err = res.data?.result || "上传失败";
                            addLog("FILE", `图片上传失败: ${err}`, "error");
                            showAlert(err, { title: "图片上传失败", tone: "error" });
                          }
                        } catch (err) {
                          addLog("FILE", `网络或系统异常: ${err.message}`, "error");
                          showAlert(err.message, { title: "上传异常", tone: "error" });
                        }
                      };
                      reader.onerror = () => {
                        addLog("FILE", "本地文件读取失败", "error");
                      };
                      reader.readAsDataURL(file);
                    }}
                    className="file-input wb-field-6"
                  />
                  {uploadedImagePath && (
                    <div className="wb-box-2 wb-image-thumb-row" id="wb-image-selected">
                      {uploadedImageThumb && (
                        <img src={uploadedImageThumb} alt="已选原图缩略图" className="wb-image-thumb" />
                      )}
                      <span className="wb-image-name">✓ 已选原图: {uploadedImageName || uploadedImagePath}</span>
                    </div>
                  )}
                </div>
                <div className="wb-params-grid">
                  <label>
                    <span>成品宽 mm</span>
                    <input
                      id="wb-image-width"
                      type="number"
                      inputMode="decimal"
                      min={1}
                      max={1000}
                      placeholder="自动"
                      value={imageWidthMm}
                      onChange={(e) => setImageWidthMm(e.target.value)}
                      className="text-input"
                    />
                  </label>
                  <label>
                    <span>成品高 mm</span>
                    <input
                      id="wb-image-height"
                      type="number"
                      inputMode="decimal"
                      min={1}
                      max={1000}
                      placeholder="自动"
                      value={imageHeightMm}
                      onChange={(e) => setImageHeightMm(e.target.value)}
                      className="text-input"
                    />
                  </label>
                  <label>
                    <span>转换方式</span>
                    <select
                      id="wb-image-mode"
                      value={imageMode}
                      onChange={(e) => setImageMode(e.target.value)}
                      className="select-input wb-compact-select"
                    >
                      {IMAGE_MODE_OPTIONS.map((opt) => (
                        <option key={opt.value} value={opt.value}>{opt.label}</option>
                      ))}
                    </select>
                  </label>
                </div>
              </div>
            )}

            {activeTool === "gcode" && (
              <div className="tool-body wb-spacer">
                <div className="wb-spacer-2">
                  <label className="wb-label-2">
                    导入现有 G-code / NC 刀路文件 (.gcode / .nc)
                  </label>
                  <input
                    type="file"
                    accept=".gcode,.nc"
                    onChange={async (e) => {
                      const file = e.target.files?.[0];
                      e.target.value = "";
                      if (!file) return;
                      if (file.size > 14 * 1024 * 1024) {
                        showAlert("G-code 刀路文件过大（在线上传上限 14 MB，请求体限 20 MB）。", {
                          title: "刀路超出大小限制",
                          tone: "warning",
                        });
                        return;
                      }
                      addLog("FILE", `正在读取刀路文件: ${file.name}...`, "info");
                      const reader = new FileReader();
                      reader.onload = async () => {
                        try {
                          const res = await postJson("/api/upload", {
                            filename: file.name,
                            content_base64: reader.result,
                          });
                          if (res.ok && res.data?.success) {
                            setUploadedGcodePath(res.data.result?.file_path || file.name);
                            setUploadedGcodeName(file.name);
                            addLog("FILE", `已载入 G-code 刀路: ${file.name}`, "ok");
                          } else {
                            const err = res.data?.result || "载入失败";
                            addLog("FILE", `G-code 载入失败: ${err}`, "error");
                            showAlert(err, { title: "G-code 载入失败", tone: "error" });
                          }
                        } catch (err) {
                          addLog("FILE", `网络或系统异常: ${err.message}`, "error");
                          showAlert(err.message, { title: "载入异常", tone: "error" });
                        }
                      };
                      reader.onerror = () => {
                        addLog("FILE", "本地刀路读取失败", "error");
                      };
                      reader.readAsDataURL(file);
                    }}
                    className="file-input wb-field-6"
                  />
                  {uploadedGcodePath && (
                    <div className="wb-box-2" id="wb-gcode-selected">
                      ✓ 载入刀路: {uploadedGcodeName || uploadedGcodePath}
                    </div>
                  )}
                </div>
              </div>
            )}
        </WbCard>

        <WbCard id="material" className="hud-card ob-card" label="材质与参数">
          <FoldHeader id="wb-material" title="材质与参数" k="MATERIAL" n={3} />
            {/* Material & Specs selection Deck */}
            <div
              className="wb-box-6"
            >
              <div className="wb-row-5">
                <span className="wb-row-6">
                  <span className="wb-text-3" />
                  加工材质与物理参数联动
                </span>
                {!isRemoteMode() && (
                  <button
                    className="btn btn-ghost wb-btn-3"
                    onClick={async () => {
                      const recMode = materialRecommendationModes({ activeTool, textStyle });
                      let existingThicknesses;
                      let fullThickList = availableThicknesses;
                      try {
                        const exportRes = await getJson("/api/material-lab/export");
                        const mats = exportRes.data?.result?.library?.materials;
                        if (!exportRes.ok || !exportRes.data?.success || !mats || typeof mats !== "object") {
                          showAlert("读取材料库完整规格失败，请稍后重试", { title: "读取失败", tone: "warning" });
                          return;
                        }
                        const matEntry = mats[selectedMaterial];
                        if (matEntry?.thicknesses && typeof matEntry.thicknesses === "object") {
                          existingThicknesses = matEntry.thicknesses;
                          const keys = Object.keys(matEntry.thicknesses);
                          if (keys.length > 0) {
                            fullThickList = keys;
                          }
                        } else {
                          existingThicknesses = {};
                        }
                      } catch (err) {
                        showAlert(`读取材料库异常：${err.message}`, { title: "读取失败", tone: "warning" });
                        return;
                      }
                      setMaterialModalData({
                        name: selectedMaterial,
                        thicknesses: fullThickList,
                        power_percent: powerPercent,
                        speed_mms: speedMms,
                        feed_rate: recommendedFeedRate,
                        passes: recommendedPasses,
                        laser_mode: recMode.laserMode,
                        engraving_mode: recMode.engravingMode || "cut",
                        existing_thicknesses: existingThicknesses,
                        target_thickness: selectedThickness,
                      });
                      setShowMaterialModal(true);
                    }}
                  >
                    <Sliders size={12} className="wb-icon" />
                    调整库内参数
                  </button>
                )}
              </div>

              <div className="wb-grid-2">
                <div>
                  <label className="wb-label-3">
                    材料类型
                  </label>
                  <select
                    value={selectedMaterial}
                    onChange={(e) => {
                      const nextMat = e.target.value;
                      setSelectedMaterial(nextMat);
                      const matObj = materials.find((m) => m.name === nextMat);
                      if (matObj) {
                        setSelectedThickness(pickThicknessForMaterial(matObj, selectedThickness));
                      }
                    }}
                    className="select-input wb-field-7"
                  >
                    {materials.length === 0 ? (
                      <option value={selectedMaterial}>{selectedMaterial || "暂无库内材质"}</option>
                    ) : (
                      materials.map((m) => (
                        <option key={m.name} value={m.name}>{m.name}</option>
                      ))
                    )}
                  </select>
                </div>

                <div>
                  <label className="wb-label-3">
                    厚度规格
                  </label>
                  <select
                    value={selectedThickness}
                    onChange={(e) => setSelectedThickness(e.target.value)}
                    className="select-input wb-field-8"
                  >
                    {availableThicknesses.map((th) => (
                      <option key={th} value={String(th)}>{th} mm</option>
                    ))}
                  </select>
                </div>
              </div>

              <div className="wb-params" id="wb-params">
                <label className="wb-params-switch">
                  <input
                    type="checkbox"
                    id="wb-custom-params"
                    checked={customParams}
                    disabled={activeTool === "gcode"}
                    onChange={(e) => {
                      const on = e.target.checked;
                      setCustomParams(on);
                      if (on) {
                        setCustomPower(String(Math.round(powerPercent)));
                        setCustomFeed(String(Math.round(recommendedFeedRate)));
                        setCustomPasses(String(Math.max(1, Math.min(MANUAL_LIMITS.passes.max, recommendedPasses))));
                      }
                    }}
                  />
                  <span>这次自定义功率和速度</span>
                </label>
                {activeTool === "gcode" ? (
                  <div className="wb-params-hint">上传的刀路文件按文件里的参数加工，这里不生效。</div>
                ) : customParams ? (
                  <>
                    <div className="wb-params-grid">
                      <label>
                        <span>功率 %</span>
                        <input className="text-input" id="wb-custom-power" type="number" inputMode="decimal"
                          min={MANUAL_LIMITS.power.min} max={MANUAL_LIMITS.power.max} value={customPower}
                          onChange={(e) => setCustomPower(e.target.value)} />
                      </label>
                      <label>
                        <span>速度 mm/min</span>
                        <input className="text-input" id="wb-custom-feed" type="number" inputMode="numeric" step="10"
                          min={MANUAL_LIMITS.feed.min} max={MANUAL_LIMITS.feed.max} value={customFeed}
                          onChange={(e) => setCustomFeed(e.target.value)} />
                      </label>
                      <label>
                        <span>遍数</span>
                        <input className="text-input" id="wb-custom-passes" type="number" inputMode="numeric"
                          min={MANUAL_LIMITS.passes.min} max={MANUAL_LIMITS.passes.max} value={customPasses}
                          onChange={(e) => setCustomPasses(e.target.value)} />
                      </label>
                    </div>
                    <div className="wb-params-hint">只用于这一次生成，不会改材料库；重新计算轨迹后生效，发送前仍要你确认。</div>
                  </>
                ) : hasRecommendation ? (
                  <div className="wb-params-hint" id="wb-params-recommended">
                    材料库推荐：功率 {Math.round(powerPercent)}% · 速度 {Math.round(recommendedFeedRate)} mm/min · {recommendedPasses} 遍
                  </div>
                ) : (
                  <div className="wb-params-hint" id="wb-params-recommended">
                    材料库推荐：当前材质/厚度暂无该模式参数（可勾选自定义，或前往材料实验室补充）
                  </div>
                )}
              </div>
            </div>

        </WbCard>

        <WbCard id="monitor" className="hud-card ob-card ob-monitor" label="加工监视舱">
          <FoldHeader id="wb-monitor" title="加工监视舱" k="MONITOR" n={4} />
            <div className="chamber-viewport">
              {/* 坐标雷达网格背景 */}
              <div className="chamber-grid" />

              {previewUrl ? (
                <div className={`wb-row-7${previewStale ? " is-stale" : ""}`}>
                  <img
                    src={previewUrl}
                    alt="加工轨迹预览"
                    onError={(e) => {
                      const imgFile = currentWorkflow?.artifacts?.image_file;
                      if (imgFile && !e.currentTarget.dataset.retried) {
                        e.currentTarget.dataset.retried = "true";
                        e.currentTarget.src = resolveApiUrl(`/preview?path=${encodeURIComponent(imgFile)}&t=${Date.now()}`);
                      }
                    }}
                    className="wb-box-7"
                  />
                  <div
                    className={`wb-box-8${previewStale ? " is-stale" : ""}`}
                    id="wb-preview-badge"
                  >
                    {previewStale
                      ? "参数已改动 · 需重新生成轨迹"
                      : `刀路预览就绪${currentWorkflow?.artifacts?.origin === "center" ? " · 中心起点" : currentWorkflow?.artifacts?.origin === "lower_left" ? " · 左下角起点" : ""}`}
                  </div>
                </div>
              ) : (
                <div className="wb-box-9">
                  <div
                    className="wb-grid-4"
                  >
                    <Cpu size={28} className="wb-icon-2" />
                  </div>
                  <div className="wb-box-10">
                    加工监视舱就绪
                  </div>
                  <div className="wb-box-11">
                    请在左侧设定参数后点击【计算加工轨迹】
                  </div>
                </div>
              )}
            </div>

            <div className="job-dispatch-card">
              {/* 刀路摘要：尺寸、行数、耗时、物理文件指纹集中一行（2026-11-07 合并原先重复的两组统计） */}
              <div className="toolpath-capsule" id="wb-toolpath-stats">
                <div className="toolpath-item">
                  <span className="toolpath-label"><Ruler size={11} aria-hidden="true" /> 轮廓尺寸</span>
                  <span className="toolpath-value">
                    {dimensions ? `${dimensions.width.toFixed(1)} × ${dimensions.height.toFixed(1)} mm` : "—"}
                  </span>
                </div>
                <div className="toolpath-item">
                  <span className="toolpath-label">刀路总行数</span>
                  <span className="toolpath-value">
                    {sendProgress?.total
                      ? `${sendProgress.total.toLocaleString()} 行`
                      : currentWorkflow?.artifacts?.total_lines
                      ? `${currentWorkflow.artifacts.total_lines.toLocaleString()} 行`
                      : gcodeText
                      ? `${gcodeText.split("\n").length.toLocaleString()} 行`
                      : "—"}
                  </span>
                </div>
                <div className="toolpath-item">
                  <span className="toolpath-label"><Clock size={11} aria-hidden="true" /> 预估耗时</span>
                  <span className="toolpath-value wb-text-5">
                    {timeEstimate ? `${timeEstimate.estimated_seconds}s（${timeEstimate.estimated_minutes} 分钟）` : "—"}
                  </span>
                </div>
                <div className="toolpath-item">
                  <span className="toolpath-label">物理 SHA-256</span>
                  <span className="toolpath-value wb-text-4">
                    {currentWorkflow?.confirmation_snapshot?.gcode_sha256
                      ? `${currentWorkflow.confirmation_snapshot.gcode_sha256.slice(0, 10)}...`
                      : currentWorkflow?.artifacts?.gcode_sha256
                      ? `${currentWorkflow.artifacts.gcode_sha256.slice(0, 10)}...`
                      : "待计算比对"}
                  </span>
                </div>
              </div>
              {previewStale && (
                <div className="wb-stale-notice" id="wb-stale-notice" role="status">
                  左侧内容或参数已改动，当前预览仍是上一轮旧刀路；请先点「重新生成轨迹」后再走边框或开刻。
                </div>
              )}
              {actualParamsText(currentWorkflow) && (
                <div className="wb-origin-note" id="wb-actual-params">加工参数：{actualParamsText(currentWorkflow)}</div>
              )}
              {dimensions?.originNote && (
                <div className="wb-origin-note" id="wb-origin-note">{dimensions.originNote}</div>
              )}
              {fitNote && (
                <div className="wb-origin-note" id="wb-fit-note">{fitNote}</div>
              )}

              {/* 生成与下发按钮放在监视舱刀路摘要下方，与孪生入口同一行 */}
              <div className="wb-dispatch-actions" id="wb-dispatch-actions" role="toolbar" aria-label="生成与下发">
                <button type="button" className="btn btn-hud-primary" onClick={handleGeneratePreview} disabled={busy || framingBusy}>
                  <Cpu size={15} aria-hidden="true" />
                  <span>{busy ? "正在解算..." : currentWorkflow ? "重新生成轨迹" : "计算加工轨迹"}</span>
                </button>
                {(() => {
                  const isActiveSend = overlayStatus === "running" || overlayStatus === "sending" || overlayStatus === "preparing";
                  const canResume = !previewStale && canResumeJob(sendProgress, overlayStatus);
                  const canFire = Boolean(currentWorkflow) && !previewStale && !isActiveSend;
                  return (
                    <button
                      type="button"
                      id="wb-btn-send"
                      className={`btn ${canFire ? "btn-hud-fire" : "btn-ghost"}${canResume ? " btn-resume" : ""}`}
                      onClick={canResume ? handleResumeSend : handleConfirmSend}
                      disabled={busy || framingBusy || !currentWorkflow || previewStale || isActiveSend}
                    >
                      <Flame size={16} aria-hidden="true" />
                      <span>
                        {isActiveSend
                          ? "正在雕刻…"
                          : canResume
                          ? formatResumeActionLabel(sendProgress)
                          : previewStale
                          ? "参数已改 · 请先重算"
                          : currentWorkflow
                          ? "开始雕刻"
                          : "请先计算轨迹"}
                      </span>
                    </button>
                  );
                })()}
                <button
                  type="button"
                  className="btn btn-ghost twin-jump-btn"
                  onClick={() => onNavigate("/twin")}
                  id="btn-open-twin"
                  title="进入 3D 数字孪生舱（刀路仿真 / 实机遥测）"
                >
                  <Box size={14} aria-hidden="true" />
                  <span>3D 孪生</span>
                </button>
              </div>

              {/* 辅助操作条（仅在已生成预览时出现，不增加默认空态高度）：先走边框 · 导出 G-code · 温和取消 */}
              {currentWorkflow?.workflow_id && (
                <div className="wb-secondary-actions" id="wb-secondary-actions">
                  <button
                    type="button"
                    id="wb-btn-framing"
                    className="btn btn-ghost wb-sec-btn"
                    onClick={handleFraming}
                    disabled={
                      busy
                      || framingBusy
                      || previewStale
                      || !currentWorkflow?.artifacts?.frame_mm
                      || overlayStatus === "running"
                      || overlayStatus === "sending"
                    }
                    title={
                      !currentWorkflow?.artifacts?.frame_mm
                        ? "外部导入刀路起点由文件自身决定，不支持相对当前位置走边框"
                        : "激光头沿刀路外框绕一圈（默认不出光），核对木牌摆放位置"
                    }
                  >
                    <Square size={13} aria-hidden="true" />
                    <span>{framingBusy ? "走边框中…" : "先走边框"}</span>
                  </button>
                  <button
                    type="button"
                    id="wb-btn-export-gcode"
                    className="btn btn-ghost wb-sec-btn"
                    onClick={handleExportGcode}
                    disabled={!gcodeText}
                    title="将当前生成的 G-code 刀路保存到本地"
                  >
                    <Download size={13} aria-hidden="true" />
                    <span>导出 G-code</span>
                  </button>
                  {(overlayStatus === "running" || overlayStatus === "sending") && (
                    <button
                      type="button"
                      id="wb-btn-cancel"
                      className="btn btn-ghost wb-sec-btn is-warn"
                      onClick={handleCancelJob}
                      title="温和停止后续刀路下发并记录已完成行数，停稳后可断点续雕"
                    >
                      <PauseCircle size={13} aria-hidden="true" />
                      <span>暂停取消（记断点）</span>
                    </button>
                  )}
                </div>
              )}

              {/* 大号醒目急停安全制动按钮 */}
              <button
                type="button"
                className="btn-estop-large"
                onClick={handleEStop}
                title="向控制器请求 0x18 软件复位；不能替代硬件急停"
                id="btn-estop-trigger"
              >
                <AlertTriangle size={16} />
                <span>0x18 软件复位 · 紧急停止请求</span>
              </button>

              {/* 雕刻下发进度 */}
              <div className="streaming-progress-group">
                <div className="streaming-progress-header">
                  <span>雕刻下发进度</span>
                  <span className="wb-text-6">
                    {sendProgress && sendProgress.total > 0
                      ? `${Math.min(100, Math.round((sendProgress.sent / sendProgress.total) * 100))}%`
                      : "0%"}
                  </span>
                </div>

                <div className="streaming-progress-track">
                  <div
                    className={`streaming-progress-bar ${
                      overlayStatus === "running" || overlayStatus === "sending" ? "active" : ""
                    }`}
                    style={{
                      width: `${
                        sendProgress && sendProgress.total > 0
                          ? Math.min(100, Math.round((sendProgress.sent / sendProgress.total) * 100))
                          : 0
                      }%`,
                    }}
                  />
                </div>

                <div className="streaming-progress-footer">
                  <span>
                    {sendProgress && sendProgress.total > 0
                      ? `${sendProgress.sent.toLocaleString()} / ${sendProgress.total.toLocaleString()} 行`
                      : "0 / 0 行"}
                  </span>
                  <span id="wb-progress-status">
                    {workbenchStatusLabel(overlayStatus, sendProgress?.phase, Boolean(currentWorkflow?.workflow_id))}
                  </span>
                </div>
              </div>
            </div>
        </WbCard>

        <WbCard id="feedback" className="hud-card ob-card" label="加工反馈">
          <FoldHeader id="wb-feedback" title="加工反馈" k="FEEDBACK" />
          {!currentWorkflow?.workflow_id && (
            <p className="ob-sub wb-note">生成加工轨迹后，可在这里记录加工效果并获取调参建议。</p>
          )}
            {/* 数控中控台与实时调参反馈卡片 */}
            <FeedbackPanel workflowId={currentWorkflow?.workflow_id} origin={currentWorkflow?.feedback_origin}
              disabled={busy || ["running", "sending", "preparing", "awaiting_completion"].includes(overlayStatus)}
              onAdopt={async (workflow) => {
                if (pollIntervalId) pollIntervalId();
                setPollIntervalId(null);
                setCurrentWorkflow(workflow);
                setPreviewSignature(currentSignature);
                setOverlayStatus("pending");
                setSendProgress(null);
                setPollSafetyNotice("");
                const artifacts = workflow.artifacts || {};
                applyPreviewUrl(
                  await loadWorkbenchPreviewUrl(artifacts.image_file, {
                    remote: isRemoteMode(),
                    fetchBlobUrl: fetchPreviewBlobUrl,
                    resolveUrl: resolveApiUrl,
                  }),
                );
                setGcodeText("");
                if (artifacts.gcode_file) {
                  const preview = await fetchPreviewText(artifacts.gcode_file);
                  if (preview.ok) {
                    setGcodeText(preview.text);
                  } else {
                    addLog("PREVIEW", `调参预览已生成，但刀路资源加载失败（${preview.error}），请重新打开任务`, "error");
                  }
                }
                if (artifacts.actual_params) {
                  setPowerPercent(artifacts.actual_params.laser_max_power / artifacts.actual_params.power_scale * 100);
                  setSpeedMms(artifacts.actual_params.feed_rate / 60);
                }
                setTimeEstimate(artifacts.time_estimate || null);
                setFitNote(typeof artifacts.fit_note === "string" ? artifacts.fit_note : "");
                setDimensions(
                  artifacts.width_mm && artifacts.height_mm
                    ? { width: artifacts.width_mm, height: artifacts.height_mm, originNote: artifacts.origin_note || "" }
                    : null,
                );
                addLog("PREVIEW", `调参预览已生成：${workflow.workflow_id}，尚未发送`, "ok");
              }} />
        </WbCard>

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
        mcpStatus={mcpStatus}
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
