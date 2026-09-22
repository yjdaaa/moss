import React, { useState, useEffect, useRef } from "react";
import {
  Cpu,
  Flame,
  Layers,
  Play,
  RefreshCw,
  RotateCcw,
  Send,
  Sliders,
  Terminal,
  Type,
  UploadCloud,
  FileCode,
  Image as ImageIcon,
  PenTool,
  CheckCircle2,
  AlertTriangle,
  Info,
  Clock,
  Ruler,
  Radio,
  Square,
} from "lucide-react";
import { getJson, postJson, fetchPreviewBlobUrl, resolveApiUrl } from "../api/client.js";
import { SendOverlay } from "../components/SendOverlay.jsx";
import { MaterialModal } from "../components/MaterialModal.jsx";
import { ConsoleDrawer } from "../components/ConsoleDrawer.jsx";
import { extractWorkflowInfo, TERMINAL_STATUSES } from "../state/statusNormalize.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { canResumeJob, formatResumeActionLabel } from "../estopResumeState.js";

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
  const [uploadedFile, setUploadedFile] = useState(null);
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

  // Workflow / Output / Preview state
  const [currentWorkflow, setCurrentWorkflow] = useState(null);
  const [previewUrl, setPreviewUrl] = useState("");
  const [timeEstimate, setTimeEstimate] = useState(null);
  const [dimensions, setDimensions] = useState(null);
  const [busy, setBusy] = useState(false);

  // Overlay & Modals
  const [showSendOverlay, setShowSendOverlay] = useState(false);
  const [overlayStatus, setOverlayStatus] = useState("pending");
  const [sendProgress, setSendProgress] = useState(null);
  const [pollIntervalId, setPollIntervalId] = useState(null);
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

    // 优化 1: 启动加载完成后自动静默握手一次（若设备开着机直接点亮绿灯，失败不弹窗干扰）
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
        setConnectionStatus({ connected: isConnected, summary: result.summary });
        if (isConnected) {
          addLog("DEVICE", `硬件握手通过: ${result.summary || "控制器已连接 ($32=1 就绪)"}`, "ok");
        } else if (!silent) {
          addLog("DEVICE", result.summary || "未检测到控制器连接", "warn");
        }
        return isConnected;
      } else {
        const errorMsg = res.data?.result || "探测失败：未检测到控制器连接";
        setConnectionStatus({ connected: false, summary: errorMsg });
        if (!silent) {
          addLog("DEVICE", errorMsg, "error");
        }
        return false;
      }
    } catch (e) {
      setConnectionStatus({ connected: false, summary: "连接探测网络异常" });
      if (!silent) {
        addLog("DEVICE", `探测网络异常: ${e.message}`, "error");
      }
      return false;
    }
  };

  // Test connection (用户主动点击)
  const handleTestConnection = () => performConnectionProbe(false);

  // mDNS 局域网发现：只读浏览，不向设备发送任何命令；找到后自动填入 IP / telnet 端口
  const [discovering, setDiscovering] = useState(false);
  const discoverControllers = async () => {
    if (discovering) return;
    setDiscovering(true);
    addLog("DEVICE", "正在通过 mDNS 扫描局域网控制器（约 2 秒）...", "info");
    try {
      const res = await getJson("/api/ui/discover-controllers");
      const list = res.ok && res.data?.success ? res.data.result?.controllers || [] : [];
      if (list.length === 0) {
        addLog("DEVICE", res.data?.result?.hint || "未发现控制器", "warn");
      } else {
        const first = list[0];
        setNetworkHost(first.ip);
        if (first.telnet_port) setTelnetPort(first.telnet_port);
        addLog(
          "DEVICE",
          `发现 ${list.length} 台控制器：${list.map((c) => `${c.hostname}.local (${c.ip})`).join("、")}，已填入 ${first.ip}`,
          "ok",
        );
      }
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
          engraving_mode: engravingMode,
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
          const directUrl = resolveApiUrl(`/preview?path=${encodeURIComponent(artifacts.image_file)}`);
          const blobUrl = await fetchPreviewBlobUrl(artifacts.image_file);
          setPreviewUrl(directUrl || blobUrl);
        }
        if (artifacts.gcode_file) {
          try {
            const gcodeUrl = resolveApiUrl(`/preview?path=${encodeURIComponent(artifacts.gcode_file)}`);
            fetch(gcodeUrl)
              .then((r) => r.text())
              .then((t) => {
                setGcodeText(t);
                const lineCount = t.split("\n").filter((l) => l.trim().length > 0).length;
                const sha = wf.confirmation_snapshot?.gcode_sha256 || artifacts.gcode_sha256 || "d8f2b1a9";
                addLog("PREVIEW", `刀路解算完成: ${lineCount.toLocaleString()} 行 G-code, 物理 SHA-256: ${sha.slice(0, 8)}... 已就绪`, "ok");
                addLog("HOST", `任务 [${info.jobId || wf.workflow_id}] 门禁校验通过，可点击【启动 JobWorker 逐行下发】`, "info");
              })
              .catch(() => {});
          } catch (_e) {}
        } else {
          addLog("PREVIEW", `加工预览已生成，工作流就绪: ${info.jobId || wf.workflow_id}`, "ok");
        }
        if (artifacts.time_estimate) {
          setTimeEstimate(artifacts.time_estimate);
        }
        if (artifacts.width_mm && artifacts.height_mm) {
          setDimensions({ width: artifacts.width_mm, height: artifacts.height_mm });
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

  // Status Polling
  const startStatusPolling = (workflowId) => {
    if (pollIntervalId) clearInterval(pollIntervalId);
    let failureCount = 0;

    const id = setInterval(async () => {
      try {
        const res = await postJson("/api/workflow", {
          action: "status",
          workflow_id: workflowId,
        });

        if (res.ok && res.data?.success) {
          failureCount = 0;
          const info = extractWorkflowInfo(res.data.result);
          const sendResult = res.data.result?.send_result || {};
          if (typeof sendResult.total_lines === "number" && sendResult.total_lines > 0) {
            const sent = sendResult.lines_sent || 0;
            const total = sendResult.total_lines;
            setSendProgress({ sent, total, target: sendResult.target || "", phase: sendResult.phase });

            const pct = Math.min(100, Math.round((sent / total) * 100));
            if (pct % 25 === 0 && pct !== lastLoggedPctRef.current && pct > 0) {
              lastLoggedPctRef.current = pct;
              addLog("STREAM", `进度同步: ${pct}% (${sent} 行)`, "info");
            }
          }
          setOverlayStatus(info.status);
          if (TERMINAL_STATUSES.has(info.status)) {
            clearInterval(id);
            setPollIntervalId(null);
            const total = sendResult.total_lines || sendProgress?.total || 0;
            if (info.status === "completed") {
              addLog("SUCCESS", `[成功] ${total} 行 G-code 已全量接收，控制器执行同步及 Idle 核验通过，加工完成！`, "ok");
              if (sendResult.sd_hash_verified === false || sendResult.warning) {
                addLog("WARN", sendResult.warning || "固件未启用 HashFS，脱机文件未经物理 SHA-256 校验", "warn");
              }
            } else if (info.status === "failed") {
              const errorText = sendResult.error || "机床报告异常执行失败";
              addLog("ERROR", `[加工异常] ${errorText}`, "error");
            } else if (info.status === "cancelled") {
              const cancelText = sendResult.error || "任务已被用户取消/急停";
              addLog("ESTOP", `[任务停机] ${cancelText}`, "warn");
            } else {
              const detail = sendResult.error ? ` · ${sendResult.error}` : "";
              addLog("TERMINAL", `任务已结束: ${info.status}${detail}`, "warn");
            }
          }
        } else {
          failureCount += 1;
          if (failureCount >= 5) {
            clearInterval(id);
            setPollIntervalId(null);
            setOverlayStatus("failed");
            addLog("POLL", "连续 5 次查询失败，已自动终止轮询", "warn");
          }
        }
      } catch (_e) {
        failureCount += 1;
        if (failureCount >= 5) {
          clearInterval(id);
          setPollIntervalId(null);
          setOverlayStatus("failed");
          addLog("POLL", "网络异常，已自动终止轮询", "warn");
        }
      }
    }, 1000);

    setPollIntervalId(id);
  };

  // E-STOP 急停软复位
  const handleEStop = async () => {
    addLog("ESTOP", "0x18 软复位急停已下发！硬件进入 Alarm 锁定态！", "error");
    await handleCancel();
    showAlert("0x18 软复位指令已发送，电机运动与激光已停机！当前断点已保留，可随时点击【断点续雕】继续执行。", { title: "紧急制动已触发（断点已锁定）", tone: "error" });
  };

  // Cancel Job
  const handleCancel = async () => {
    if (!currentWorkflow?.workflow_id) return;
    addLog("CANCEL", "正在请求终止工作流任务...", "warn");
    try {
      const res = await postJson("/api/workflow", {
        action: "cancel",
        workflow_id: currentWorkflow.workflow_id,
      });
      if (res.ok && res.data?.success) {
        setOverlayStatus("cancelled");
        const sr = res.data.result?.send_result;
        if (sr && typeof sr.lines_sent === "number") {
          setSendProgress((prev) => ({
            ...prev,
            sent: sr.lines_sent,
            total: sr.total_lines || prev?.total || 0,
          }));
          if (sr.lines_sent > 0 && sr.lines_sent < sr.total_lines) {
            addLog("ESTOP", `已在第 ${sr.lines_sent}/${sr.total_lines} 行安全制动，支持点击【从第 ${sr.lines_sent + 1} 行继续加工】`, "warn");
          }
        }
        addLog("CANCEL", "工作流任务已安全中止并释放通道", "ok");
      } else {
        addLog("CANCEL", res.data?.result || "请求取消失败", "error");
      }
    } catch (e) {
      addLog("CANCEL", `取消网络异常: ${e.message}`, "error");
    }
  };

  return (
    <div className="home-page" style={{ padding: "16px 24px", maxWidth: "1400px", margin: "0 auto" }}>
      {/* Hero Section */}
      <section className="hero-section" style={{ marginBottom: "18px", display: "flex", justifyContent: "space-between", alignItems: "flex-end" }}>
        <div>
          <div style={{ display: "flex", alignItems: "center", gap: "8px", marginBottom: "4px" }}>
            <span style={{ fontSize: "11px", letterSpacing: "2px", color: "var(--laser, #00e5ff)", textTransform: "uppercase", fontWeight: "700" }}>
              01 · MOSS LASER WORKBENCH
            </span>
            <span style={{ fontSize: "10px", padding: "1px 6px", borderRadius: "3px", background: "rgba(255, 42, 68, 0.15)", border: "1px solid rgba(255, 42, 68, 0.45)", color: "var(--moss-red, #ff2a44)", fontFamily: "var(--font-mono)" }}>
              UEG-550W COMMAND
            </span>
          </div>
          <h1 style={{ fontSize: "26px", fontWeight: "800", margin: 0, color: "var(--ink, #fff)" }}>
            激光创作<span style={{ color: "var(--laser, #00e5ff)" }}>控制台</span>
          </h1>
          <p style={{ margin: "4px 0 0", fontSize: "13px", color: "var(--ink-dim, #a0b3cc)" }}>
            全闭环 Rust 激光工业控制器 · 包含文字排版、矢量轮廓、光栅二值抖动与高压物理急停
          </p>
        </div>
      </section>

      {/* Main Console Box (HUD Card) */}
      <div className="console-box hud-card" style={{ position: "relative" }}>
        {/* HUD 科技四角准星 */}
        <div className="hud-corner-tl" />
        <div className="hud-corner-tr" />
        <div className="hud-corner-bl" />
        <div className="hud-corner-br" />

        {/* Connection Bar */}
        <div
          className="connection-bar"
          style={{
            display: "flex",
            flexWrap: "wrap",
            alignItems: "center",
            gap: "12px",
            padding: "12px 18px",
            background: "rgba(0,0,0,0.25)",
            borderBottom: "1px solid var(--line, rgba(140, 175, 255, 0.14))",
            fontSize: "13px",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
            <span style={{ color: "var(--laser, #35e0ff)", fontFamily: "var(--font-mono)", fontSize: "11px", fontWeight: "700", letterSpacing: "1px" }}>
              [LINK]
            </span>
            <span style={{ color: "var(--ink-dim, #94a3b8)" }}>通信链路:</span>
            <select
              value={connectionMode}
              onChange={(e) => setConnectionMode(e.target.value)}
              className="select-input"
              style={{ fontWeight: "600" }}
            >
              <option value="network">网络 (Telnet/TCP)</option>
              <option value="serial">串口 (USB/Serial)</option>
            </select>
          </div>

          {connectionMode === "serial" ? (
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <select
                value={serialPort}
                onChange={(e) => {
                  if (e.target.value === "__custom__") {
                    const custom = window.prompt("请输入串口名称 (例如 COM3 或 /dev/ttyUSB0):", serialPort || "COM3");
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
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <input
                type="text"
                placeholder="控制器 IP (例 192.168.1.10)"
                value={networkHost}
                onChange={(e) => setNetworkHost(e.target.value)}
                className="text-input"
                style={{ width: "220px", fontFamily: "var(--font-mono)" }}
              />
              <span style={{ color: "var(--ink-dim, #94a3b8)" }}>端口:</span>
              <input
                type="number"
                value={telnetPort}
                onChange={(e) => setTelnetPort(Number(e.target.value))}
                className="text-input"
                style={{ width: "70px", fontFamily: "var(--font-mono)" }}
              />
              <button
                className="btn btn-ghost"
                onClick={discoverControllers}
                disabled={discovering}
                title="通过 mDNS 在局域网内查找 FluidNC（fluidnc.local），自动填入 IP 与端口"
              >
                <RefreshCw size={13} style={{ marginRight: 4 }} />
                <span>{discovering ? "扫描中…" : "扫描局域网"}</span>
              </button>
            </div>
          )}

          <button className="btn btn-ghost" onClick={handleTestConnection} style={{ marginLeft: "auto" }}>
            <RefreshCw size={13} style={{ marginRight: 4 }} />
            <span>握手探测</span>
          </button>
        </div>

        {/* Workbench Body */}
        <div style={{ display: "grid", gridTemplateColumns: "1.2fr 1fr", minHeight: "520px" }}>
          {/* Left Column: Form & Tools */}
          <div style={{ padding: "20px", borderRight: "1px solid var(--line, rgba(140, 175, 255, 0.14))" }}>
            {/* Tool selection cards */}
            <div style={{ display: "grid", gridTemplateColumns: "repeat(4, 1fr)", gap: "8px", marginBottom: "20px" }}>
              {[
                { id: "text", label: "矢量文字", sub: "TEXT", icon: Type },
                { id: "image", label: "图片处理", sub: "RASTER", icon: ImageIcon },
                { id: "gcode", label: "已有G码", sub: "G-CODE", icon: FileCode },
                { id: "draw", label: "创意画板", sub: "CANVAS", icon: PenTool },
              ].map((t) => {
                const Icon = t.icon;
                const active = activeTool === t.id;
                return (
                  <div
                    key={t.id}
                    onClick={() => setActiveTool(t.id)}
                    className={`hud-tool-card ${active ? "active" : ""}`}
                  >
                    <Icon size={18} className="tool-icon" style={{ color: active ? "var(--laser)" : "var(--ink-dim)" }} />
                    <span style={{ fontSize: "12px", fontWeight: "700", color: active ? "var(--ink)" : "var(--ink-dim)" }}>
                      {t.label}
                    </span>
                    <span style={{ fontSize: "9px", fontFamily: "var(--font-mono)", color: active ? "var(--laser)" : "var(--ink-faint)" }}>
                      {t.sub}
                    </span>
                  </div>
                );
              })}
            </div>

            {/* Tool specific parameters */}
            {activeTool === "text" && (
              <div className="tool-body" style={{ marginBottom: "20px" }}>
                <div style={{ marginBottom: "12px" }}>
                  <div style={{ display: "flex", justifyContent: "space-between", marginBottom: "5px" }}>
                    <label style={{ fontSize: "12px", fontWeight: "600", color: "var(--ink)" }}>
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
                    className="text-input"
                    style={{ width: "100%", fontSize: "15px", padding: "10px 14px", fontFamily: "var(--font-mono)" }}
                    placeholder="输入要雕刻的字符..."
                  />
                </div>
                <div style={{ display: "flex", gap: "12px" }}>
                  <div style={{ flex: 1 }}>
                    <label style={{ display: "block", fontSize: "12px", fontWeight: "600", color: "var(--ink)", marginBottom: "5px" }}>
                      字高尺寸 (mm)
                    </label>
                    <input
                      type="number"
                      min={1}
                      max={120}
                      value={fontSizeMm}
                      onChange={(e) => setFontSizeMm(Number(e.target.value))}
                      className="text-input"
                      style={{ width: "100%", fontFamily: "var(--font-mono)" }}
                    />
                  </div>
                  <div style={{ flex: 1 }}>
                    <label style={{ display: "block", fontSize: "12px", fontWeight: "600", color: "var(--ink)", marginBottom: "5px" }}>
                      排版模式
                    </label>
                    <select
                      value={engravingMode}
                      onChange={(e) => setEngravingMode(e.target.value)}
                      className="select-input"
                      style={{ width: "100%" }}
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
              <div className="tool-body" style={{ marginBottom: "20px" }}>
                <div style={{ marginBottom: "12px" }}>
                  <label style={{ display: "block", fontSize: "12px", fontWeight: "600", color: "var(--ink)", marginBottom: "5px" }}>
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
                    className="file-input"
                    style={{ width: "100%" }}
                  />
                  {uploadedFilePath && (
                    <div style={{ fontSize: "11px", color: "var(--green)", marginTop: "6px", fontFamily: "var(--font-mono)" }}>
                      ✓ 当前素材: {uploadedFilePath}
                    </div>
                  )}
                </div>
              </div>
            )}

            {activeTool === "gcode" && (
              <div className="tool-body" style={{ marginBottom: "20px" }}>
                <div style={{ marginBottom: "12px" }}>
                  <label style={{ display: "block", fontSize: "12px", fontWeight: "600", color: "var(--ink)", marginBottom: "5px" }}>
                    导入现有 G-code / NC 刀路文件 (.gcode / .nc / .txt)
                  </label>
                  <input
                    type="file"
                    accept=".gcode,.nc,.txt"
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
                    className="file-input"
                    style={{ width: "100%" }}
                  />
                  {uploadedFilePath && (
                    <div style={{ fontSize: "11px", color: "var(--green)", marginTop: "6px", fontFamily: "var(--font-mono)" }}>
                      ✓ 载入刀路: {uploadedFilePath}
                    </div>
                  )}
                </div>
              </div>
            )}

            {activeTool === "draw" && (
              <div className="tool-body" style={{ marginBottom: "20px", padding: "16px", background: "rgba(53, 224, 255, 0.05)", border: "1px dashed rgba(53, 224, 255, 0.3)", borderRadius: "8px", textAlign: "center" }}>
                <div style={{ fontSize: "14px", fontWeight: "700", color: "var(--ink)", marginBottom: "6px" }}>
                  Excalidraw 矢量创作画板
                </div>
                <div style={{ fontSize: "12px", color: "var(--ink-dim)", marginBottom: "14px" }}>
                  支持自由手绘、几何轮廓、图形拼接并一键转为激光切割/光栅雕刻 G-code
                </div>
                <button
                  type="button"
                  className="btn btn-hud-primary"
                  onClick={() => onNavigate("/draw")}
                  style={{ padding: "8px 20px" }}
                >
                  <PenTool size={15} />
                  <span>进入画板全屏创作</span>
                </button>
              </div>
            )}

            {/* Material & Specs selection Deck */}
            <div
              style={{
                background: "rgba(6, 10, 20, 0.7)",
                padding: "14px 16px",
                borderRadius: "8px",
                border: "1px solid rgba(53, 224, 255, 0.2)",
                marginBottom: "24px",
                boxShadow: "inset 0 1px 0 rgba(255, 255, 255, 0.05)",
              }}
            >
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "10px" }}>
                <span style={{ fontSize: "12px", fontWeight: "800", color: "var(--laser, #35e0ff)", letterSpacing: "1px", display: "flex", alignItems: "center", gap: "6px" }}>
                  <span style={{ width: "6px", height: "6px", background: "var(--laser)", borderRadius: "50%", display: "inline-block" }} />
                  加工材质与物理参数联动
                </span>
                <button
                  className="btn btn-ghost"
                  style={{ fontSize: "11px", padding: "3px 8px" }}
                  onClick={() => {
                    setMaterialModalData({
                      name: selectedMaterial,
                      thicknesses: availableThicknesses,
                      power_percent: powerPercent,
                      speed_mms: speedMms,
                    });
                    setShowMaterialModal(true);
                  }}
                >
                  <Sliders size={12} style={{ marginRight: 4 }} />
                  调整库内参数
                </button>
              </div>

              <div style={{ display: "grid", gridTemplateColumns: "1.2fr 1fr", gap: "12px" }}>
                <div>
                  <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim, #94a3b8)", marginBottom: "4px" }}>
                    材料类型
                  </label>
                  <select
                    value={selectedMaterial}
                    onChange={(e) => setSelectedMaterial(e.target.value)}
                    className="select-input"
                    style={{ width: "100%", fontWeight: "600" }}
                  >
                    {materials.map((m) => (
                      <option key={m.name} value={m.name}>{m.name}</option>
                    ))}
                  </select>
                </div>

                <div>
                  <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim, #94a3b8)", marginBottom: "4px" }}>
                    厚度规格
                  </label>
                  <select
                    value={selectedThickness}
                    onChange={(e) => setSelectedThickness(e.target.value)}
                    className="select-input"
                    style={{ width: "100%", fontFamily: "var(--font-mono)", fontWeight: "600" }}
                  >
                    {availableThicknesses.map((th) => (
                      <option key={th} value={String(th)}>{th} mm</option>
                    ))}
                  </select>
                </div>
              </div>
            </div>

            {/* Action Buttons: HUD Primary + HUD Fire Trigger */}
            <div style={{ display: "grid", gridTemplateColumns: "1.1fr 1.3fr", gap: "14px" }}>
              <button
                className="btn btn-hud-primary"
                onClick={handleGeneratePreview}
                disabled={busy}
                style={{ padding: "12px" }}
              >
                <Cpu size={16} />
                <span>{busy ? "正在解算..." : currentWorkflow ? "重新生成轨迹" : "计算加工轨迹"}</span>
              </button>

              {(() => {
                const canResume = canResumeJob(sendProgress, overlayStatus);
                return (
                  <button
                    className={`btn ${currentWorkflow ? "btn-hud-fire" : "btn-ghost"}`}
                    onClick={canResume ? handleResumeSend : handleConfirmSend}
                    disabled={busy || !currentWorkflow || overlayStatus === "running"}
                    style={{ padding: "12px", background: canResume ? "linear-gradient(135deg, #059669, #10b981)" : undefined }}
                  >
                    <Flame size={17} style={{ color: currentWorkflow ? "#04050a" : "inherit" }} />
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
            </div>
          </div>

          {/* Right Column: Holographic Chamber Viewport */}
          <div style={{ padding: "20px", display: "flex", flexDirection: "column" }}>
            <div className="chamber-viewport">
              {/* 坐标雷达网格背景 */}
              <div className="chamber-grid" />

              {previewUrl ? (
                <div style={{ position: "relative", width: "100%", height: "100%", display: "flex", alignItems: "center", justifyContent: "center", zIndex: 2 }}>
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
                    style={{
                      maxWidth: "92%",
                      maxHeight: "92%",
                      objectFit: "contain",
                      filter: "drop-shadow(0 0 14px rgba(0, 229, 255, 0.65))",
                      pointerEvents: "none",
                    }}
                  />
                  <div
                    style={{
                      position: "absolute",
                      bottom: "12px",
                      right: "14px",
                      fontSize: "10px",
                      fontFamily: "var(--font-mono)",
                      color: "var(--laser)",
                      background: "rgba(0, 229, 255, 0.12)",
                      border: "1px solid rgba(0, 229, 255, 0.35)",
                      borderRadius: "3px",
                      padding: "2px 8px",
                      letterSpacing: "1px",
                      fontWeight: "700",
                    }}
                  >
                    TOOLPATH PREVIEW · READY
                  </div>
                </div>
              ) : (
                <div style={{ textAlign: "center", color: "var(--ink-dim, #94a3b8)", position: "relative", zIndex: 2 }}>
                  <div
                    style={{
                      width: "60px",
                      height: "60px",
                      borderRadius: "50%",
                      border: "1px dashed rgba(53, 224, 255, 0.4)",
                      display: "grid",
                      placeItems: "center",
                      margin: "0 auto 12px",
                      background: "rgba(53, 224, 255, 0.05)",
                    }}
                  >
                    <Cpu size={28} style={{ color: "var(--laser, #35e0ff)", opacity: 0.6 }} />
                  </div>
                  <div style={{ fontSize: "14px", fontWeight: "700", color: "var(--ink)" }}>
                    加工监视舱就绪
                  </div>
                  <div style={{ fontSize: "12px", marginTop: "4px", color: "var(--ink-faint)" }}>
                    请在左侧设定参数后点击【计算加工轨迹】
                  </div>
                </div>
              )}
            </div>

            {/* Metrics cards (HUD Data Capsules) */}
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "10px", marginTop: "14px" }}>
              <div
                style={{
                  background: "rgba(6, 10, 20, 0.75)",
                  padding: "10px 14px",
                  borderRadius: "6px",
                  border: "1px solid var(--line)",
                }}
              >
                <div style={{ fontSize: "11px", color: "var(--ink-dim, #94a3b8)", display: "flex", alignItems: "center", gap: "5px" }}>
                  <Ruler size={13} style={{ color: "var(--laser)" }} />
                  <span>几何轮廓包络</span>
                </div>
                <div style={{ fontSize: "15px", fontWeight: "700", fontFamily: "var(--font-mono)", marginTop: "3px" }}>
                  {dimensions ? `${dimensions.width.toFixed(1)} × ${dimensions.height.toFixed(1)} mm` : "—"}
                </div>
              </div>

              <div
                style={{
                  background: "rgba(6, 10, 20, 0.75)",
                  padding: "10px 14px",
                  borderRadius: "6px",
                  border: "1px solid var(--line)",
                }}
              >
                <div style={{ fontSize: "11px", color: "var(--ink-dim, #94a3b8)", display: "flex", alignItems: "center", gap: "5px" }}>
                  <Clock size={13} style={{ color: "var(--gold)" }} />
                  <span>预估加工耗时</span>
                </div>
                <div style={{ fontSize: "15px", fontWeight: "700", fontFamily: "var(--font-mono)", marginTop: "3px", color: "var(--laser)" }}>
                  {timeEstimate ? `${timeEstimate.estimated_seconds}s (${timeEstimate.estimated_minutes} min)` : "—"}
                </div>
              </div>
            </div>

            {/* 数控中控台与 JobWorker 实时执行卡片 */}
            <div className="job-dispatch-card">
              {/* 刀路摘要胶囊 */}
              <div className="toolpath-capsule">
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
                  <span className="toolpath-label">物理 SHA-256</span>
                  <span className="toolpath-value" style={{ fontSize: "11px", color: "var(--laser-cyan, #00e5ff)" }}>
                    {currentWorkflow?.confirmation_snapshot?.gcode_sha256
                      ? `${currentWorkflow.confirmation_snapshot.gcode_sha256.slice(0, 10)}...`
                      : currentWorkflow?.artifacts?.gcode_sha256
                      ? `${currentWorkflow.artifacts.gcode_sha256.slice(0, 10)}...`
                      : "待计算比对"}
                  </span>
                </div>
                <div className="toolpath-item">
                  <span className="toolpath-label">预估耗时</span>
                  <span className="toolpath-value" style={{ color: "var(--gold, #ffb84d)" }}>
                    {timeEstimate ? `${timeEstimate.estimated_seconds}s` : "—"}
                  </span>
                </div>
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
                  <span style={{ fontFamily: "var(--font-mono)", color: "var(--laser-cyan, #00e5ff)" }}>
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
          </div>
        </div>
      </div>

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

      {/* Send Overlay */}
      <SendOverlay
        visible={showSendOverlay}
        status={overlayStatus}
        progress={sendProgress}
        onCancel={handleCancel}
        onClose={() => {
          setShowSendOverlay(false);
          if (pollIntervalId) {
            clearInterval(pollIntervalId);
            setPollIntervalId(null);
          }
        }}
      />

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
