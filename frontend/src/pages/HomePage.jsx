import React, { useState, useEffect, useRef } from "react";
import {
  Cpu,
  Flame,
  RefreshCw,
  Sliders,
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
import { actualParamsText, manualParamsPayload, MANUAL_LIMITS } from "../workbenchParams.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { canResumeJob, formatResumeActionLabel } from "../estopResumeState.js";
import { estopTargetArgs } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { WbCard, WorkbenchCanvas } from "../components/WorkbenchCanvas.jsx";
import { defaultSerialPortHint } from "../platform.js";
import { controllerLabel, discoveryOutcome } from "../controllerDiscovery.js";

export async function loadWorkbenchPreviewUrl(imageFile, { remote, fetchBlobUrl, resolveUrl }) {
  if (!imageFile) return "";
  if (remote) {
    return await fetchBlobUrl(imageFile);
  }
  return resolveUrl(`/preview?path=${encodeURIComponent(imageFile)}`);
}

export function HomePage({ onNavigate, connectionStatus, setConnectionStatus }) {
  // Connection state
  const [connectionMode, setConnectionMode] = useState(
    localStorage.getItem("laser-forge-conn-mode") || "network"
  );
  const [serialPort, setSerialPort] = useState(
    localStorage.getItem("laser-forge-serial-port") || ""
  );
  const [baudrate, setBaudrate] = useState(
    Number(localStorage.getItem("laser-forge-baud") || 115200)
  );
  const [serialPorts, setSerialPorts] = useState([]);
  const [networkHost, setNetworkHost] = useState(
    localStorage.getItem("laser-forge-host") || ""
  );
  const [telnetPort, setTelnetPort] = useState(
    Number(localStorage.getItem("laser-forge-telnet-port") || 23)
  );

  // Active Tool Mode
  const [activeTool, setActiveTool] = useState("text"); // text, image, gcode, draw, preset

  // Tool specific inputs
  const [textContent, setTextContent] = useState(
    localStorage.getItem("laser-forge-text") || "MOSS"
  );
  const [fontSizeMm, setFontSizeMm] = useState(
    Number(localStorage.getItem("laser-forge-font-size") || 12)
  );
  const [uploadedFilePath, setUploadedFilePath] = useState("");

  // Material and Parameter state
  const [materials, setMaterials] = useState([]);
  const [selectedMaterial, setSelectedMaterial] = useState(
    localStorage.getItem("laser-forge-mat") || "椴木"
  );
  const [selectedThickness, setSelectedThickness] = useState(
    localStorage.getItem("laser-forge-thick") || "3"
  );
  const [engravingMode, setEngravingMode] = useState("raster"); // raster, outline, cut
  const [powerPercent, setPowerPercent] = useState(50);
  const [speedMms, setSpeedMms] = useState(100);
  // n73：这次自定义功率 / 速度 / 遍数（只影响这次生成的刀路，不写回材料库）
  const [customParams, setCustomParams] = useState(false);
  const [customPower, setCustomPower] = useState("50");
  const [customFeed, setCustomFeed] = useState("1200");
  const [customPasses, setCustomPasses] = useState("1");

  // Workflow / Output / Preview state
  const [currentWorkflow, setCurrentWorkflow] = useState(null);
  const [previewUrl, setPreviewUrl] = useState("");
  const [timeEstimate, setTimeEstimate] = useState(null);
  const [dimensions, setDimensions] = useState(null);
  const [busy, setBusy] = useState(false);

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

    // 优化 1: 仅在应用启动后尚未探测过时自动静默握手一次；从其他页面切回工作台时不重复探测
    if (connectionStatus?.checkedAt) return undefined;
    const timer = setTimeout(() => {
      if ((connectionMode === "network" && networkHost) || (connectionMode === "serial" && serialPort)) {
        performConnectionProbe(true);
      }
    }, 600);
    return () => clearTimeout(timer);
  }, []);

  // Save to local storage on change
  useEffect(() => {
    localStorage.setItem("laser-forge-conn-mode", connectionMode);
    localStorage.setItem("laser-forge-serial-port", serialPort);
    localStorage.setItem("laser-forge-baud", String(baudrate));
    localStorage.setItem("laser-forge-host", networkHost);
    localStorage.setItem("laser-forge-telnet-port", String(telnetPort));
    localStorage.setItem("laser-forge-mat", selectedMaterial);
    localStorage.setItem("laser-forge-thick", selectedThickness);
    localStorage.setItem("laser-forge-text", textContent);
    localStorage.setItem("laser-forge-font-size", String(fontSizeMm));
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
  ]);

  const loadMaterials = async () => {
    const res = await getJson("/api/ui/material-options");
    if (res.ok && res.data?.success) {
      const list = res.data.result?.materials || [];
      setMaterials(list);
      if (list.length > 0 && !list.some((m) => m.name === selectedMaterial)) {
        setSelectedMaterial(list[0].name);
        if (list[0].thicknesses?.[0]) {
          setSelectedThickness(String(list[0].thicknesses[0]));
        }
      }
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

  // 联动加载选定材质与厚度的真实加工工艺参数
  useEffect(() => {
    let cancelled = false;
    async function syncMaterialRecommendation() {
      if (!selectedMaterial || !selectedThickness) return;
      const query = new URLSearchParams({
        material: selectedMaterial,
        thickness_mm: String(selectedThickness),
        laser_mode: engravingMode === "cut" ? "cut" : "engrave",
      });
      if (engravingMode !== "cut") {
        query.set("engraving_mode", engravingMode);
      }
      const res = await getJson(`/api/ui/material-recommendation?${query.toString()}`);
      if (!cancelled && res.ok && res.data?.success) {
        const rec = res.data.result;
        if (rec) {
          if (rec.laser_max_power != null) {
            setPowerPercent(Math.round(rec.laser_max_power / 10));
          }
          if (rec.feed_rate != null) {
            setSpeedMms(Math.round(rec.feed_rate / 60));
          }
        }
      }
    }
    syncMaterialRecommendation();
    return () => {
      cancelled = true;
    };
  }, [selectedMaterial, selectedThickness, engravingMode]);

  // Switch material -> update thickness list
  const currentMaterialObj = materials.find((m) => m.name === selectedMaterial);
  const availableThicknesses = currentMaterialObj?.thicknesses || [1, 2, 3, 5];

  // 通用硬件握手探测方法（支持静默模式与主动模式）
  const performConnectionProbe = async (silent = false) => {
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

  // Generate Preview
  const handleGeneratePreview = async () => {
    if (busy) {
      addLog("PREVIEW", "正在解算加工轨迹中，请勿重复点击...", "warn");
      return;
    }

    if (currentWorkflow?.workflow_id) {
      const ok = await showConfirm("当前参数已成功生成过加工轨迹与 G-code，确认要重新计算并覆盖吗？", {
        title: "重新生成确认",
        confirmText: "确认重新生成",
        cancelText: "保留当前结果",
      });
      if (!ok) {
        addLog("PREVIEW", "用户已取消重新生成，保留当前轨迹与 G-code", "info");
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
        res = await postJson("/api/text-task/generate", {
          text: textContent.trim(),
          material: selectedMaterial,
          thickness_mm: Number(selectedThickness),
          font_size_mm: Number(fontSizeMm),
          laser_mode: engravingMode === "cut" ? "cut" : "engrave",
          engraving_mode: engravingMode,
          ...manualField,
        });
      } else if (activeTool === "image") {
        if (!uploadedFilePath) {
          showAlert("请先点击上方按钮选择并上传一张图片", { title: "缺少图片", tone: "warning" });
          setBusy(false);
          return;
        }
        res = await postJson("/api/workflow", {
          action: "preview",
          material: selectedMaterial,
          thickness_mm: Number(selectedThickness),
          source_type: "image",
          image_file: uploadedFilePath,
          laser_mode: engravingMode === "cut" ? "cut" : "engrave",
          engraving_mode: engravingMode,
          ...manualField,
        });
      } else if (activeTool === "gcode") {
        if (!uploadedFilePath) {
          showAlert("请先选择要加工的 .gcode / .nc 文件", { title: "缺少刀路文件", tone: "warning" });
          setBusy(false);
          return;
        }
        res = await postJson("/api/workflow", {
          action: "preview",
          material: selectedMaterial,
          thickness_mm: Number(selectedThickness),
          source_type: "gcode",
          gcode_file: uploadedFilePath,
        });
      } else if (activeTool === "draw") {
        onNavigate("/draw");
        setBusy(false);
        return;
      }

      setBusy(false);
      if (res.ok && res.data?.success) {
        const wf = res.data.result;
        setCurrentWorkflow(wf);
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
              addLog("HOST", `任务 [${info.jobId || wf.workflow_id}] 预览已就绪，发送时后端将重新校验物理文件 SHA-256；可点击【启动 JobWorker 逐行下发】`, "info");
            } else {
              addLog("PREVIEW", `刀路解算完成: ${lineCount.toLocaleString()} 行 G-code；后端未返回物理 SHA-256，摘要不可用`, "warn");
              addLog("HOST", `任务 [${info.jobId || wf.workflow_id}] 未确认刀路门禁，发送时由后端重新计算物理文件 SHA-256 校验`, "warn");
            }
          });
        } else {
          addLog("PREVIEW", `加工预览已生成，工作流就绪: ${info.jobId || wf.workflow_id}`, "ok");
        }
        if (artifacts.time_estimate) {
          setTimeEstimate(artifacts.time_estimate);
        }
        if (artifacts.width_mm && artifacts.height_mm) {
          // 2026-11-06：文字默认以激光头对准中心为起点、图片以左下角为起点，开刻前提示放哪
          setDimensions({ width: artifacts.width_mm, height: artifacts.height_mm, originNote: artifacts.origin_note || "" });
          if (artifacts.origin_note) addLog("HOST", artifacts.origin_note, "warn");
        }
      } else {
        addLog("PREVIEW", res.data?.result || "预览生成失败", "error");
        showAlert(res.data?.result || "预览生成失败", { title: "预览生成失败", tone: "error" });
      }
    } catch (e) {
      setBusy(false);
      addLog("PREVIEW", `网络或内部异常: ${e.message}`, "error");
    }
  };

  // Confirm Send
  const handleConfirmSend = async () => {
    if (!currentWorkflow?.workflow_id) {
      showAlert("请先点击【计算加工轨迹】，预览并确认加工参数后方可发送", { title: "尚未生成预览", tone: "warning" });
      return;
    }

    // 优化 2: 点击【启动下发】时自动兜底校验
    // 若当前尚未标记为连接，先自动执行一次快速握手探测，通过则直接无缝放行下发！
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
    addLog("DISPATCH", "门禁通过，启动 JobWorker 后台线程...", "info");
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
    let isConnected = connectionStatus?.connected;
    if (!isConnected) {
      addLog("GATE", "正在自动执行快速握手校验...", "info");
      isConnected = await performConnectionProbe(true);
    }
    if (!isConnected) {
      showAlert("未检测到控制器连接，请确认设备已开机并完成握手！", { title: "断点续雕失败", tone: "error" });
      return;
    }

    const startLine = (sendProgress?.sent || 0) + 1;
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
                onChange={(e) => {
                  if (e.target.value === "__custom__") {
                    const serialHint = defaultSerialPortHint();
                    const custom = window.prompt(serialHint.prompt, serialPort || serialHint.fallback);
                    if (custom && custom.trim()) {
                      const p = custom.trim();
                      if (!serialPorts.includes(p)) {
                        setSerialPorts((prev) => [...prev, p]);
                      }
                      setSerialPort(p);
                    }
                  } else {
                    setSerialPort(e.target.value);
                  }
                }}
                className="select-input"
              >
                {serialPorts.length === 0 ? (
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
                { id: "draw", label: "创意画板", sub: "CANVAS", icon: PenTool },
              ].map((t) => {
                const Icon = t.icon;
                const active = activeTool === t.id;
                return (
                  <button
                    type="button"
                    key={t.id}
                    onClick={() => setActiveTool(t.id)}
                    className={`hud-tool-card ${active ? "active" : ""}`}
                    aria-pressed={active}
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
                <div className="wb-row-4">
                  <div className="wb-box">
                    <label className="wb-label-2">
                      字高尺寸 (mm)
                    </label>
                    <input
                      type="number"
                      min={1}
                      max={100}
                      value={fontSizeMm}
                      onChange={(e) => setFontSizeMm(Number(e.target.value))}
                      className="text-input wb-field-5"
                    />
                  </div>
                  <div className="wb-box">
                    <label className="wb-label-2">
                      排版模式
                    </label>
                    <select
                      value={engravingMode}
                      onChange={(e) => setEngravingMode(e.target.value)}
                      className="select-input wb-field-6"
                    >
                      <option value="raster">光栅填充 (Raster)</option>
                      <option value="outline">轮廓扫描 (Outline)</option>
                      <option value="cut">边缘切割 (Cut)</option>
                    </select>
                  </div>
                </div>
              </div>
            )}

            {activeTool === "image" && (
              <div className="tool-body wb-spacer">
                <div className="wb-spacer-2">
                  <label className="wb-label-2">
                    上传图像文件 (PNG / JPG / BMP / WebP)
                  </label>
                  <input
                    type="file"
                    accept=".png,.jpg,.jpeg,.bmp,.webp"
                    onChange={async (e) => {
                      const file = e.target.files?.[0];
                      if (!file) return;
                      addLog("FILE", `正在读取素材: ${file.name}...`, "info");
                      const reader = new FileReader();
                      reader.onload = async () => {
                        try {
                          const res = await postJson("/api/upload", {
                            filename: file.name,
                            content_base64: reader.result,
                          });
                          if (res.ok && res.data?.success) {
                            setUploadedFilePath(res.data.result?.file_path || file.name);
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
                  {uploadedFilePath && (
                    <div className="wb-box-2">
                      ✓ 当前素材: {uploadedFilePath}
                    </div>
                  )}
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
                      if (!file) return;
                      addLog("FILE", `正在读取刀路文件: ${file.name}...`, "info");
                      const reader = new FileReader();
                      reader.onload = async () => {
                        try {
                          const res = await postJson("/api/upload", {
                            filename: file.name,
                            content_base64: reader.result,
                          });
                          if (res.ok && res.data?.success) {
                            setUploadedFilePath(res.data.result?.file_path || file.name);
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
                  {uploadedFilePath && (
                    <div className="wb-box-2">
                      ✓ 载入刀路: {uploadedFilePath}
                    </div>
                  )}
                </div>
              </div>
            )}

            {activeTool === "draw" && (
              <div className="tool-body wb-box-3">
                <div className="wb-box-4">
                  Excalidraw 矢量创作画板
                </div>
                <div className="wb-box-5">
                  支持自由手绘、几何轮廓、图形拼接并一键转为激光切割/光栅雕刻 G-code
                </div>
                <button
                  type="button"
                  className="btn btn-hud-primary wb-btn-2"
                  onClick={() => onNavigate("/draw")}
                >
                  <PenTool size={15} />
                  <span>进入画板全屏创作</span>
                </button>
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
                <button
                  className="btn btn-ghost wb-btn-3"
                  onClick={() => {
                    setMaterialModalData({
                      name: selectedMaterial,
                      thicknesses: availableThicknesses,
                      power_percent: powerPercent,
                      speed_mms: speedMms,
                      laser_mode: engravingMode === "cut" ? "cut" : "engrave",
                      engraving_mode: engravingMode,
                    });
                    setShowMaterialModal(true);
                  }}
                >
                  <Sliders size={12} className="wb-icon" />
                  调整库内参数
                </button>
              </div>

              <div className="wb-grid-2">
                <div>
                  <label className="wb-label-3">
                    材料类型
                  </label>
                  <select
                    value={selectedMaterial}
                    onChange={(e) => setSelectedMaterial(e.target.value)}
                    className="select-input wb-field-7"
                  >
                    {materials.map((m) => (
                      <option key={m.name} value={m.name}>{m.name}</option>
                    ))}
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
                        setCustomFeed(String(Math.round(speedMms * 60)));
                        setCustomPasses("1");
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
                ) : (
                  <div className="wb-params-hint" id="wb-params-recommended">
                    材料库推荐：功率 {Math.round(powerPercent)}% · 速度 {Math.round(speedMms * 60)} mm/min · 1 遍
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
                <div className="wb-row-7">
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
                    className="wb-box-8"
                  >
                    TOOLPATH PREVIEW · READY
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
              {actualParamsText(currentWorkflow) && (
                <div className="wb-origin-note" id="wb-actual-params">加工参数：{actualParamsText(currentWorkflow)}</div>
              )}
              {dimensions?.originNote && (
                <div className="wb-origin-note" id="wb-origin-note">{dimensions.originNote}</div>
              )}

              {/* 生成与下发按钮放在监视舱刀路摘要下方，与孪生入口同一行（2026-11-07 用户反馈“布局还长了”，
                  撤掉底部悬浮操作栏，不额外增加页面高度） */}
              <div className="wb-dispatch-actions" id="wb-dispatch-actions" role="toolbar" aria-label="生成与下发">
                <button type="button" className="btn btn-hud-primary" onClick={handleGeneratePreview} disabled={busy}>
                  <Cpu size={15} aria-hidden="true" />
                  <span>{busy ? "正在解算..." : currentWorkflow ? "重新生成轨迹" : "计算加工轨迹"}</span>
                </button>
                {(() => {
                  const canResume = canResumeJob(sendProgress, overlayStatus);
                  return (
                    <button
                      type="button"
                      className={`btn ${currentWorkflow ? "btn-hud-fire" : "btn-ghost"}${canResume ? " btn-resume" : ""}`}
                      onClick={canResume ? handleResumeSend : handleConfirmSend}
                      disabled={busy || !currentWorkflow || overlayStatus === "running"}
                    >
                      <Flame size={16} aria-hidden="true" />
                      <span>
                        {overlayStatus === "running"
                          ? "JobWorker 正在下发中..."
                          : canResume
                          ? formatResumeActionLabel(sendProgress)
                          : currentWorkflow
                          ? "启动 JobWorker 逐行下发"
                          : "待计算加工轨迹"}
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

              {/* 下发实时进度 (STREAMING PROGRESS) */}
              <div className="streaming-progress-group">
                <div className="streaming-progress-header">
                  <span>下发实时进度 (STREAMING PROGRESS)</span>
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
                  <span>{sendProgress?.phase === "awaiting_completion" && !TERMINAL_STATUSES.has(overlayStatus)
                    ? "等待机器执行完成"
                    : overlayStatus.toUpperCase()}</span>
                </div>
              </div>
            </div>
        </WbCard>

        <WbCard id="feedback" className="hud-card ob-card" label="加工反馈">
          <FoldHeader id="wb-feedback" title="加工反馈" k="FEEDBACK" />
          {!currentWorkflow?.workflow_id && (
            <p className="ob-sub wb-note">生成加工轨迹后，可在这里记录加工效果并获取调参建议。</p>
          )}
            {/* 数控中控台与 JobWorker 实时执行卡片 */}
            <FeedbackPanel workflowId={currentWorkflow?.workflow_id} origin={currentWorkflow?.feedback_origin}
              disabled={busy || ["running", "sending", "preparing", "awaiting_completion"].includes(overlayStatus)}
              onAdopt={async (workflow) => {
                if (pollIntervalId) pollIntervalId();
                setPollIntervalId(null);
                setCurrentWorkflow(workflow);
                setOverlayStatus("pending");
                setSendProgress(null);
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
                setDimensions({ width: artifacts.width_mm, height: artifacts.height_mm, originNote: artifacts.origin_note || "" });
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
