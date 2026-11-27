import React, { useState, useEffect, useRef, useMemo, useCallback } from "react";
import {
  Play,
  Pause,
  RotateCcw,
  SkipBack,
  SkipForward,
  Activity,
  Radio,
  AlertTriangle,
  RefreshCw,
  Upload,
  CheckCircle2,
  ShieldAlert,
  Flame,
  ArrowUpRight,
  ChevronUp,
  ChevronDown,
  Settings2,
} from "lucide-react";
import { DEFAULT_READ_TIMEOUT_MS, fetchPreviewText, postJson } from "../api/client.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { readFoldOpen, writeFoldOpen } from "../foldState.js";
import { TwinStage3D } from "../components/TwinStage3D.jsx";
import {
  parseGcodeToolpath,
  formatDurationSeconds,
  resolvePoseAtLine,
  resolvePoseAtProgress,
  parseGrblStatusReport,
  resolveMachineWorkspace,
  resolveTwinHeadPose,
  resolveTwinHasZAxis,
  resolveTwinSafetySwitchStatus,
  describeMachinePeripherals,
  MAX_XY_TRAVEL_MM,
  buildTwinConnectionPayload,
  canResetTwinToDemo,
  loadTwinMaterialConfig,
  saveTwinMaterialConfig,
  twinEstopContext,
} from "../twinState.js";
import { recordControllerOverride } from "../workbenchParams.js";
import { TERMINAL_STATUSES, normalizeWorkflowStatus } from "../state/statusNormalize.js";
import { workflowDetails } from "../approvalState.js";
import { showAlert } from "../appDialog.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import {
  MACHINE_FEATURES_EVENT,
  WORKSPACE_LIMITS_EVENT,
  clearManualWorkspaceLimits,
  connectionSummaryView,
  connectionTargetKey,
  emitDeviceLog,
  formatWorkspaceLimitsLabel,
  readMachineFeaturesStore,
  readUnifiedConnection,
  readWorkspaceLimitsStore,
  resolveMachineFeaturesForTarget,
  resolveWorkspaceLimitsForTarget,
  syncHardwareMachineFeatures,
  syncHardwareWorkspaceLimits,
  workspaceLimitsExceedsHardwareWarning,
  writeManualWorkspaceLimits,
} from "../deviceConnection.js";
import { writeMachineMotion } from "../fillOptions.js";
import { useDeviceConnection } from "../useDeviceConnection.js";

const TWIN_STATUS_LABELS = {
  collecting: "准备中",
  preview_ready: "预览就绪",
  pending: "等待下发",
  preparing: "准备发送",
  sending: "下发中",
  running: "加工中",
  completed: "已完成",
  failed: "未完成",
  cancelled: "已取消",
  feedback_recorded: "已记录反馈",
};

function twinStatusLabel(status) {
  const normalized = normalizeWorkflowStatus(status);
  return TWIN_STATUS_LABELS[normalized] || String(status || "预览就绪");
}

export function formatTwinWorkflowLabel(wf, { includeStatus = false } = {}) {
  if (!wf || typeof wf !== "object") return "内置演示刀路 (60×60mm)";
  const details = workflowDetails(wf);
  const mat = wf.artifacts?.material || wf.material || wf.input?.material || wf.confirmation_snapshot?.material || "";
  const thick = wf.artifacts?.thickness_mm ?? wf.thickness_mm ?? wf.input?.thickness_mm ?? wf.confirmation_snapshot?.thickness_mm ?? "";
  const matText = mat ? `${mat}${thick !== "" && thick != null ? ` ${thick}mm` : ""}` : "";
  const contentText = details.content || (wf.source_type === "gcode" ? "导入刀路" : "");
  const parts = [contentText, matText || (!contentText ? "激光任务" : "")].filter(Boolean);
  if (includeStatus) {
    parts.push(twinStatusLabel(wf.status));
  }
  return parts.join(" · ") || "激光任务";
}

// 面向大鱼 DIY 翼宿 V1.0 (100×100mm 行程) 的内置演示刀路（60×60mm 标准铭牌 + 内框几何，起点对齐 (0,0)）
const DEMO_GCODE = [
  "; MOSS Yisu V1.0 (100x100mm) Demo Toolpath (60x60mm)",
  "G21 G90",
  "M5 S0",
  "G0 X0 Y0",
  "M3 S850",
  "G1 X60 Y0 F1500",
  "G1 X60 Y60 S850",
  "G1 X0 Y60 S850",
  "G1 X0 Y0 S850",
  "M5",
  "G0 X10 Y10",
  "M3 S650",
  "G1 X50 Y10 F1800",
  "G1 X50 Y50 S650",
  "G1 X10 Y50 S650",
  "G1 X10 Y10 S650",
  "M5",
  "G0 X20 Y18",
  "M3 S950",
  "G1 X30 Y44 F1200",
  "G1 X40 Y18 S950",
  "G1 X20 Y18 S950",
  "M5 S0",
  "G0 X0 Y0",
].join("\n");

const SIM_FRAME_MS = 1000 / 30;
export function TwinPage({ onNavigate, connectionStatus, setConnectionStatus, onOpenConnection = null }) {
  const {
    connectionMode,
    serialPort,
    baudrate,
    networkHost,
    telnetPort,
  } = useDeviceConnection();
  const connectionView = connectionSummaryView(
    {
      connection_mode: connectionMode,
      port: serialPort,
      baudrate,
      network_host: networkHost,
      network_telnet_port: telnetPort,
    },
    connectionStatus,
  );
  // 驱动模式：sim (离线刀路推演) | job (加工下发同步) | telemetry (实机坐标映射)
  const [driveMode, setDriveMode] = useState("sim");

  // 工作流与刀路数据
  const [recentWorkflows, setRecentWorkflows] = useState([]);
  const [selectedWorkflowId, setSelectedWorkflowId] = useState("");
  const [activeWorkflow, setActiveWorkflow] = useState(null);
  const [gcodeText, setGcodeText] = useState(DEMO_GCODE);
  const [gcodeSourceLabel, setGcodeSourceLabel] = useState("内置演示刀路 (60×60mm)");
  const [loadingWorkflow, setLoadingWorkflow] = useState(false);

  // 仿真播放器状态（严格 1:1 对应 G-code 真实雕刻时长；默认不开启循环推演）
  const [simPlaying, setSimPlaying] = useState(true);
  const [simLoop, setSimLoop] = useState(false);
  const [simProgress, setSimProgress] = useState(0); // 0..1 (严格对应 0..estimatedSeconds)
  const [showRapid, setShowRapid] = useState(true);
  const [followHead, setFollowHead] = useState(false);
  const [timelineOpen, setTimelineOpen] = useState(() => readFoldOpen("twin-timeline", true));

  const toggleTimelineOpen = useCallback(() => {
    setTimelineOpen((prev) => {
      const next = !prev;
      writeFoldOpen("twin-timeline", next);
      return next;
    });
  }, []);
  // 任务实时下发同步状态
  const [jobSendResult, setJobSendResult] = useState(null);
  const [jobStatus, setJobStatus] = useState("preview_ready");

  // 控制器硬件行程与固件能力（同一连接目标按“手填 → 固件 → 默认 100×100mm”取值）
  const currentConnForm = useMemo(
    () => ({
      connection_mode: connectionMode,
      port: serialPort,
      baudrate: String(baudrate),
      network_host: networkHost,
      network_telnet_port: String(telnetPort),
    }),
    [connectionMode, serialPort, baudrate, networkHost, telnetPort],
  );
  const [workspaceLimitsStore, setWorkspaceLimitsStore] = useState(() => readWorkspaceLimitsStore());
  useEffect(() => {
    const onLimits = (e) => {
      if (e?.detail) setWorkspaceLimitsStore(e.detail);
    };
    window.addEventListener(WORKSPACE_LIMITS_EVENT, onLimits);
    return () => window.removeEventListener(WORKSPACE_LIMITS_EVENT, onLimits);
  }, []);
  const machineLimits = useMemo(
    () => resolveWorkspaceLimitsForTarget(workspaceLimitsStore, currentConnForm),
    [workspaceLimitsStore, currentConnForm],
  );

  const [featuresStore, setFeaturesStore] = useState(() => readMachineFeaturesStore());
  useEffect(() => {
    const onFeatures = (e) => {
      if (e?.detail) setFeaturesStore(e.detail);
    };
    window.addEventListener(MACHINE_FEATURES_EVENT, onFeatures);
    return () => window.removeEventListener(MACHINE_FEATURES_EVENT, onFeatures);
  }, []);
  const storedFeatures = useMemo(
    () => resolveMachineFeaturesForTarget(featuresStore, currentConnForm),
    [featuresStore, currentConnForm],
  );
  const [liveFeatures, setLiveFeatures] = useState(() => connectionStatus?.features || null);
  const connTargetKey = connectionTargetKey(currentConnForm);
  const prevConnTargetRef = useRef(connTargetKey);
  useEffect(() => {
    if (prevConnTargetRef.current !== connTargetKey) {
      prevConnTargetRef.current = connTargetKey;
      setLiveFeatures(null);
    }
  }, [connTargetKey]);
  useEffect(() => {
    if (connectionStatus && "features" in connectionStatus) {
      setLiveFeatures(connectionStatus.features || null);
    }
  }, [connectionStatus]);
  const machineFeatures = liveFeatures || storedFeatures;

  // 材料外框尺寸与台面摆放起点 (offsetX, offsetY)；材料内偏移恒为 0，与真实刀路一致（N6）
  const [materialConfig, setMaterialConfig] = useState(() => loadTwinMaterialConfig());
  const [widthDraft, setWidthDraft] = useState(null);
  const [heightDraft, setHeightDraft] = useState(null);

  const updateMaterialConfig = useCallback((patch) => {
    setMaterialConfig((prev) => saveTwinMaterialConfig(prev, patch));
  }, []);
  const updateManualWorkspaceLimits = useCallback((patch) => {
    const conn = readUnifiedConnection();
    writeManualWorkspaceLimits(patch, conn);
    setWorkspaceLimitsStore(readWorkspaceLimitsStore());
  }, []);
  const handleRestoreHardwareLimits = useCallback(() => {
    const conn = readUnifiedConnection();
    clearManualWorkspaceLimits(conn);
    setWorkspaceLimitsStore(readWorkspaceLimitsStore());
  }, []);
  const [telemetry, setTelemetry] = useState(() => parseGrblStatusReport(""));
  const [pollingTelemetry, setPollingTelemetry] = useState(false);
  const [probingDevice, setProbingDevice] = useState(false);
  const [lastTelemetryAt, setLastTelemetryAt] = useState(null);

  // 读取工作台保存的连接参数
  // 存储被禁用时读取会抛错；急停路径也会调用这里，必须退回空目标而不是中断（2026-11-05）
  const getConnectionPayload = useCallback(() => {
    const saved = readUnifiedConnection();
    return buildTwinConnectionPayload({
      mode: saved.connection_mode,
      port: saved.port,
      baudrate: Number(saved.baudrate) || 115200,
      networkHost: saved.network_host,
      telnetPort: Number(saved.network_telnet_port) || 23,
    });
  }, []);

  // 解析三维刀路
  const toolpath = useMemo(() => {
    const scale = activeWorkflow?.artifacts?.actual_params?.power_scale || 1000;
    return parseGcodeToolpath(gcodeText, { powerScale: scale });
  }, [gcodeText, activeWorkflow]);

  // 计算三维机台工作区包络、板材外框尺寸与工作原点偏置
  const workspace = useMemo(
    () => resolveMachineWorkspace(machineLimits, toolpath.bounds, activeWorkflow?.artifacts, materialConfig),
    [machineLimits, toolpath.bounds, activeWorkflow, materialConfig],
  );
  // 离线仿真位姿
  const simPose = useMemo(
    () => resolvePoseAtProgress(toolpath, simProgress),
    [toolpath, simProgress],
  );

  // 任务下发同步位姿
  const jobPose = useMemo(() => {
    const sentLines = jobSendResult?.lines_sent ?? 0;
    return resolvePoseAtLine(toolpath, sentLines);
  }, [toolpath, jobSendResult]);

  const isJobRunning = jobStatus === "sending" || jobStatus === "running" || Boolean(jobSendResult?.running);

  // 融合最终三维激光头位姿
  const activePose = useMemo(
    () =>
      resolveTwinHeadPose({
        driveMode,
        simPose,
        jobPose,
        telemetry,
        jobRunning: isJobRunning,
      }),
    [driveMode, simPose, jobPose, telemetry, isJobRunning],
  );

  // 刀路与材料包络越界检查（包含板材台面起点 offsetX/Y 与缩放比例）
  const boundsWarning = useMemo(() => {
    if (!toolpath || toolpath.segments.length === 0) return "";
    const { minX, maxX, minY, maxY } = toolpath.bounds;
    const sx = workspace.toolpathScaleX ?? 1;
    const sy = workspace.toolpathScaleY ?? 1;
    const effMinX = minX * sx;
    const effMaxX = maxX * sx;
    const effMinY = minY * sy;
    const effMaxY = maxY * sy;
    const ox = workspace.offsetX || 0;
    const oy = workspace.offsetY || 0;
    const tx = workspace.toolpathOffsetX ?? ox;
    const ty = workspace.toolpathOffsetY ?? oy;
    if (effMinX + tx < 0 || effMinY + ty < 0) {
      return `刀路含负向坐标 (${(effMinX + tx).toFixed(1)}, ${(effMinY + ty).toFixed(1)} mm)，请调整起点坐标`;
    }
    if (effMaxX + tx > workspace.xTravelMm || effMaxY + ty > workspace.yTravelMm) {
      return `当前坐标 (${tx}, ${ty} mm) 下刀路外沿 (${(effMaxX + tx).toFixed(1)}×${(effMaxY + ty).toFixed(1)} mm) 超出机台行程 (${workspace.xTravelMm}×${workspace.yTravelMm} mm)`;
    }
    if (
      workspace.material.widthMm + ox > workspace.xTravelMm ||
      workspace.material.heightMm + oy > workspace.yTravelMm
    ) {
      return `材料外框 (${(workspace.material.widthMm + ox).toFixed(1)}×${(workspace.material.heightMm + oy).toFixed(1)} mm) 超出机台有效行程 (${workspace.xTravelMm}×${workspace.yTravelMm} mm)`;
    }
    return "";
  }, [toolpath, workspace]);

  const resetToDemoToolpath = useCallback(() => {
    if (!canResetTwinToDemo({ jobStatus, jobSendResult })) return false;
    workflowLoadSeqRef.current += 1;
    setLoadingWorkflow(false);
    setActiveWorkflow(null);
    setSelectedWorkflowId("");
    setJobStatus("preview_ready");
    setJobSendResult(null);
    setGcodeText(DEMO_GCODE);
    setGcodeSourceLabel("内置演示刀路 (60×60mm)");
    setSimProgress(0);
    setSimPlaying(true);
    setDriveMode("sim");
    return true;
  }, [jobStatus, jobSendResult]);

  // 加载指定 workflow 的 G-code 刀路文件。请求序号守卫（2026-11-05）：快速切换任务时，
  // 旧任务较慢返回的刀路不得覆盖新选中的任务。
  const workflowLoadSeqRef = useRef(0);
  const loadWorkflowToolpath = useCallback(async (wf) => {
    if (!wf) return;
    const seq = ++workflowLoadSeqRef.current;
    setActiveWorkflow(wf);
    setSelectedWorkflowId(wf.workflow_id || "");
    setJobStatus(wf.status || "preview_ready");
    setMaterialConfig((prev) => ({
      ...prev,
      thicknessMm: Number(wf.artifacts?.thickness_mm || wf.thickness_mm) || prev.thicknessMm || 3,
    }));
    if (wf.send_result) {
      setJobSendResult(wf.send_result);
      if (wf.send_result.last_status_report) {
        const parsed = parseGrblStatusReport(wf.send_result.last_status_report);
        recordControllerOverride(wf.send_result.target || readUnifiedConnection(), parsed);
        if (parsed.online) {
          setTelemetry(parsed);
          setLastTelemetryAt(new Date().toLocaleTimeString());
        }
      }
    }
    const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
    if (!gcodeFile) {
      setLoadingWorkflow(false);
      return;
    }

    setLoadingWorkflow(true);
    try {
      const res = await fetchPreviewText(gcodeFile);
      if (seq !== workflowLoadSeqRef.current) return;
      const text = res.ok ? res.text : "";
      if (text && text.trim().length > 0) {
        setGcodeText(text);
        setGcodeSourceLabel(formatTwinWorkflowLabel(wf));
        setSimProgress(0);
        setSimPlaying(true);
      }
      // 读取失败时保留当前刀路
    } finally {
      if (seq === workflowLoadSeqRef.current) setLoadingWorkflow(false);
    }
  }, []);

  // 拉取最近的工作流列表
  const fetchRecentWorkflows = useCallback(async () => {
    try {
      const res = await postJson("/api/workflow", { action: "recent" });
      if (res.ok && res.data?.success) {
        const raw = res.data.result;
        const list = Array.isArray(raw?.workflows)
          ? raw.workflows
          : Array.isArray(raw)
          ? raw
          : raw?.workflow_id
          ? [raw]
          : [];
        setRecentWorkflows(list);
        if (list.length > 0 && !selectedWorkflowId) {
          const firstWithGcode = list.find((item) => item.artifacts?.gcode_file || item.gcode_file) || list[0];
          await loadWorkflowToolpath(firstWithGcode);
          if (firstWithGcode.status === "sending" || firstWithGcode.send_result?.running) {
            setDriveMode("job");
          }
        }
      }
    } catch (_e) {
      // 静默回退内置演示刀路
    }
  }, [selectedWorkflowId, loadWorkflowToolpath]);

  // 只在打开页面时拉一次最近任务（之后由用户操作刷新）；经 ref 调用最新的函数
  const fetchRecentRef = useRef(fetchRecentWorkflows);
  useEffect(() => {
    fetchRecentRef.current = fetchRecentWorkflows;
  });
  useEffect(() => {
    void fetchRecentRef.current();
  }, []);

  // 离线仿真 requestAnimationFrame 播放循环（严格 1:1 对应 G-code 解算出的物理雕刻时长；未开启循环时跑完一次立即停止）
  useEffect(() => {
    if (!simPlaying || driveMode !== "sim") return undefined;
    let lastTime = performance.now();
    let rafId = 0;
    let stopped = false;

    // 严格 1:1 真实雕刻时长：G-code 推算多少秒，数字孪生就真实走多少秒，不再使用倍数缩放
    const totalDurationSec = Math.max(0.05, Number(toolpath.estimatedSeconds) || 0.05);

    const tick = (now) => {
      if (stopped) return;
      if (document.hidden) {
        lastTime = now;
        return;
      }
      // 推演画面约 30 fps（2026-11-08）：每帧都会重新渲染整页并重画 3D，60 fps 时 CPU 约 70%；
      // 进度按真实经过时间累计，总时长仍与 G-code 推算一致
      if (now - lastTime < SIM_FRAME_MS) {
        rafId = requestAnimationFrame(tick);
        return;
      }
      const dt = Math.max(0, (now - lastTime) / 1000);
      lastTime = now;
      setSimProgress((prev) => {
        const next = prev + dt / totalDurationSec;
        if (next >= 1) {
          if (simLoop) return 0;
          stopped = true;
          setSimPlaying(false);
          return 1;
        }
        return next;
      });
      if (!stopped) {
        rafId = requestAnimationFrame(tick);
      }
    };

    const onVisibility = () => {
      if (!stopped && !document.hidden) {
        lastTime = performance.now();
        cancelAnimationFrame(rafId);
        rafId = requestAnimationFrame(tick);
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    rafId = requestAnimationFrame(tick);
    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [simPlaying, simLoop, driveMode, toolpath.estimatedSeconds]);

  // 当处于“加工下发同步”模式且有选定 workflow 时，轮询 /api/workflow status
  useEffect(() => {
    if ((driveMode !== "job" && !(driveMode === "telemetry" && isJobRunning)) || !selectedWorkflowId) {
      return undefined;
    }
    let cancelled = false;
    let timer = null;
    let pollGen = 0;

    const pollWorkflow = async () => {
      const myGen = ++pollGen;
      clearTimeout(timer);
      try {
        const res = await postJson("/api/workflow", {
          action: "status",
          workflow_id: selectedWorkflowId,
        });
        if (cancelled || myGen !== pollGen) return;
        if (res.ok && res.data?.success) {
          const wf = res.data.result;
          setJobStatus(wf.status || "preview_ready");
          if (wf.send_result) {
            setJobSendResult(wf.send_result);
            if (wf.send_result.last_status_report) {
              const parsed = parseGrblStatusReport(wf.send_result.last_status_report);
              recordControllerOverride(wf.send_result.target || readUnifiedConnection(), parsed);
              if (parsed.online) {
                setTelemetry(parsed);
                setLastTelemetryAt(new Date().toLocaleTimeString());
              }
            }
          }
          if (TERMINAL_STATUSES.has(wf.status) && !wf.send_result?.running) {
            return;
          }
        }
      } catch (_e) {
        if (cancelled || myGen !== pollGen) return;
        // 保持上次已知状态
      }
      if (!cancelled && myGen === pollGen && !document.hidden) {
        clearTimeout(timer);
        timer = setTimeout(pollWorkflow, 1000);
      }
    };

    const onVisibility = () => {
      if (!cancelled && !document.hidden) {
        void pollWorkflow();
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    void pollWorkflow();
    return () => {
      cancelled = true;
      pollGen += 1;
      if (timer) clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [driveMode, isJobRunning, selectedWorkflowId]);

  const mountedRef = useRef(true);
  const telemetrySeqRef = useRef(0);
  const telemetryAbortRef = useRef(null);
  const telemetryInFlightRef = useRef(false);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      telemetrySeqRef.current += 1;
      telemetryInFlightRef.current = false;
      telemetryAbortRef.current?.abort();
      telemetryAbortRef.current = null;
    };
  }, []);

  // 同步机台物理行程与外设能力 (/api/connection/check)
  const handleSyncMachineCapabilities = async () => {
    if (probingDevice) return;
    setProbingDevice(true);
    try {
      const payload = getConnectionPayload();
      const res = await postJson("/api/connection/check", payload);
      if (!mountedRef.current || JSON.stringify(payload) !== JSON.stringify(getConnectionPayload())) return;
      if (res.ok && res.data?.success) {
        const r = res.data.result || {};
        const isConn = Boolean(r.connected || r.serial_connected || r.network_connected);
        let syncedSwapNotice = "";
        if (isConn) {
          const conn = readUnifiedConnection();
          const synced = syncHardwareWorkspaceLimits(r.machine_limits, conn, undefined, {
            probed: true,
            identity: r.identity || null,
            softLimitsEnabled: typeof r.soft_limits_enabled === "boolean" ? r.soft_limits_enabled : null,
            homingEnabled: typeof r.homing_enabled === "boolean" ? r.homing_enabled : null,
          });
          syncedSwapNotice = synced?.identity_changed_notice || "";
          syncHardwareMachineFeatures(r.features || null, conn);
          writeMachineMotion(r.machine_limits, conn);
          setWorkspaceLimitsStore(readWorkspaceLimitsStore());
          setFeaturesStore(readMachineFeaturesStore());
          if (syncedSwapNotice) {
            emitDeviceLog("DEVICE", syncedSwapNotice, "warn");
          }
        }
        const summaryText = r.summary || (isConn ? "硬件握手通过" : "未连接");
        setConnectionStatus?.({
          connected: isConn,
          summary: syncedSwapNotice ? `${summaryText}；${syncedSwapNotice}` : summaryText,
          checkedAt: Date.now(),
          isFluidnc: Boolean(r.features),
          features: r.features || null,
          identity: r.identity || null,
          softLimitsEnabled: typeof r.soft_limits_enabled === "boolean" ? r.soft_limits_enabled : null,
          homingEnabled: typeof r.homing_enabled === "boolean" ? r.homing_enabled : null,
        });
        setLiveFeatures(r.features || null);
      } else {
        const msg = (typeof res.data?.result === "string" && res.data.result) || "读取实机行程失败，请检查设备连接";
        setConnectionStatus?.({
          connected: false,
          summary: msg,
          checkedAt: Date.now(),
          isFluidnc: false,
          features: null,
        });
        setLiveFeatures(null);
        setTelemetry(parseGrblStatusReport("", msg));
      }
    } catch (err) {
      if (!mountedRef.current) return;
      const msg = err?.message || "读取实机行程异常";
      setConnectionStatus?.({
        connected: false,
        summary: msg,
        checkedAt: Date.now(),
        isFluidnc: false,
        features: null,
      });
      setLiveFeatures(null);
      setTelemetry(parseGrblStatusReport("", msg));
    } finally {
      if (mountedRef.current) setProbingDevice(false);
    }
  };

  // 单次采样控制器 GRBL 实时状态 (/api/safe-action { action: "status" })
  // 带 AbortController、超时上限、单调序号与卸载守卫，防止慢回包卸载后污染全局 connectionStatus
  const sampleControllerStatus = useCallback(async (silent = false) => {
    const seq = ++telemetrySeqRef.current;
    telemetryAbortRef.current?.abort();
    const controller = new AbortController();
    telemetryAbortRef.current = controller;
    telemetryInFlightRef.current = true;
    try {
      const payload = {
        ...getConnectionPayload(),
        action: "status",
      };
      const res = await postJson("/api/safe-action", payload, {
        signal: controller.signal,
        timeoutMs: DEFAULT_READ_TIMEOUT_MS,
      });
      if (!mountedRef.current || telemetrySeqRef.current !== seq) return null;
      if (res.ok && res.data?.success) {
        const detail = res.data.detail ?? res.data.result;
        const parsed = parseGrblStatusReport(detail, String(res.data.result || ""));
        recordControllerOverride(readUnifiedConnection(), parsed);
        setTelemetry(parsed);
        setLastTelemetryAt(new Date().toLocaleTimeString());
        if (parsed.online) {
          setConnectionStatus?.((prev) => ({
            ...(prev || {}),
            connected: true,
            summary: `状态 ${parsed.state}`,
            checkedAt: Date.now(),
          }));
        }
        return parsed;
      }
      if (!silent) {
        const msg = res.data?.result || "控制器未返回实时状态报告";
        setTelemetry(parseGrblStatusReport("", msg));
      }
    } catch (err) {
      if (!mountedRef.current || telemetrySeqRef.current !== seq) return null;
      if (!silent) {
        setTelemetry(parseGrblStatusReport("", err?.message || "状态查询异常"));
      }
    } finally {
      if (telemetryAbortRef.current === controller) {
        telemetryAbortRef.current = null;
      }
      if (telemetrySeqRef.current === seq) {
        telemetryInFlightRef.current = false;
      }
    }
    return null;
  }, [getConnectionPayload, setConnectionStatus]);

  // 周期只读遥测采样 (每 2 秒一次)
  useEffect(() => {
    if (!pollingTelemetry) return undefined;
    let cancelled = false;
    let timer = null;
    let pollGen = 0;

    const loop = async () => {
      if (telemetryInFlightRef.current) {
        if (!cancelled && !document.hidden) {
          clearTimeout(timer);
          timer = setTimeout(loop, 2000);
        }
        return;
      }
      const myGen = ++pollGen;
      clearTimeout(timer);
      await sampleControllerStatus(true);
      if (!cancelled && myGen === pollGen && !document.hidden) {
        clearTimeout(timer);
        timer = setTimeout(loop, 2000);
      }
    };
    const onVisibility = () => {
      if (!cancelled && !document.hidden) {
        void loop();
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    void loop();
    return () => {
      cancelled = true;
      pollGen += 1;
      telemetrySeqRef.current += 1;
      telemetryInFlightRef.current = false;
      telemetryAbortRef.current?.abort();
      telemetryAbortRef.current = null;
      if (timer) clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [pollingTelemetry, sampleControllerStatus]);

  // 导入本地 G-code 文件用于三维仿真推演（自动与 3D 工作台 1:1 同步尺寸）
  const handleLocalGcodeImport = (e) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const content = String(reader.result || "");
      if (!content.trim()) {
        void showAlert(`文件「${file.name}」内容为空，无法用于三维刀路推演。`, {
          title: "刀路文件为空",
          tone: "warning",
        });
        return;
      }
      // 作废仍在途的任务刀路请求，避免其晚到后覆盖本地文件
      workflowLoadSeqRef.current += 1;
      setLoadingWorkflow(false);
      setActiveWorkflow(null);
      setSelectedWorkflowId("");
      setGcodeText(content);
      setGcodeSourceLabel(`本地文件 · ${file.name}`);
      updateMaterialConfig({ autoSize: true });
      setSimProgress(0);
      setSimPlaying(true);
      setDriveMode("sim");
    };
    reader.onerror = () => {
      void showAlert("读取本地 G-code 文件失败", { title: "导入失败", tone: "error" });
    };
    reader.readAsText(file);
    e.target.value = "";
  };

  // 页面内急停按钮与顶栏急停（2026-11-05）：与工作台 / 画板共用 `usePageEmergencyStop` 两段式机制——
  // 运行中先按任务编号走原设备通道，失败或超时再按页面连接目标写 0x18；无运行任务时直接按连接目标急停。
  const handleTwinEmergencyStop = usePageEmergencyStop(
    () => twinEstopContext({
      isJobRunning,
      selectedWorkflowId,
      connectionPayload: getConnectionPayload(),
      recordedTarget: jobSendResult?.target || "",
    }),
    async (outcome) => {
      const sent = outcome.kind === "sent";
      const msg = sent
        ? `${outcome.message || "0x18 软件复位已发送"}。请立即在现场核对机器物理状态，软件急停不能替代实体急停按钮。`
        : outcome.kind === "no_target"
          ? "没有运行中的任务，也没有填写设备连接目标，未发送急停。请立即按下机器上的实体急停开关！"
          : `${outcome.message || "急停请求发送失败"}。请立即按下机器上的实体急停开关！`;
      await showAlert(msg, {
        title: sent ? "已发送 0x18 急停" : "急停未确认",
        tone: sent ? "warning" : "error",
      });
    },
  );

  // 单步前/后移动一行（按真实累计时间比例定位）
  const stepByLine = (delta) => {
    setDriveMode("sim");
    setSimPlaying(false);
    if (!toolpath || toolpath.segments.length === 0) return;
    const curIdx = Math.min(
      toolpath.segments.length - 1,
      Math.max(0, (simPose.completedSegmentCount || 1) - 1 + delta),
    );
    const seg = toolpath.segments[curIdx];
    const ratio = toolpath.estimatedSeconds > 0
      ? seg.cumulativeSeconds / toolpath.estimatedSeconds
      : toolpath.totalLengthMm > 0
      ? seg.cumulativeLengthMm / toolpath.totalLengthMm
      : 0;
    setSimProgress(ratio);
  };

  const hasZAxis = resolveTwinHasZAxis({ workspace, machineLimits, machineFeatures, telemetry });
  const safetySwitchStatus = resolveTwinSafetySwitchStatus({ telemetry, machineFeatures });
  const peripherals = describeMachinePeripherals(machineFeatures);
  const limitsSourceLabel = formatWorkspaceLimitsLabel(machineLimits);
  const limitsExceedWarning = workspaceLimitsExceedsHardwareWarning(machineLimits);
  const hasBufferTelemetry = telemetry.bf?.plannerFree != null || telemetry.bf?.rxBufferFree != null;

  return (
    <div className="twin-page ob-page">
      {/* 顶部模式切换与状态横幅 */}
      <div className="twin-header-bar">
        <div className="twin-mode-tabs" role="tablist" aria-label="数字孪生信号源模式">
          <button
            type="button"
            role="tab"
            aria-selected={driveMode === "sim"}
            className={`twin-mode-tab ${driveMode === "sim" ? "active" : ""}`}
            onClick={() => setDriveMode("sim")}
          >
            <Play size={14} />
            <b>离线刀路推演</b>
          </button>

          <button
            type="button"
            role="tab"
            aria-selected={driveMode === "job"}
            className={`twin-mode-tab ${driveMode === "job" ? "active" : ""}`}
            onClick={() => {
              setSimPlaying(false);
              setDriveMode("job");
            }}
          >
            <Flame size={14} />
            <b>加工下发同步</b>
          </button>

          <button
            type="button"
            role="tab"
            aria-selected={driveMode === "telemetry"}
            className={`twin-mode-tab ${driveMode === "telemetry" ? "active" : ""}`}
            onClick={() => {
              setSimPlaying(false);
              setDriveMode("telemetry");
              void sampleControllerStatus(false);
            }}
          >
            <Activity size={14} />
            <b>实机遥测映射</b>
          </button>
        </div>

        <div className="twin-header-meta">
          <span className={`ob-pill ${activePose.isLiveHardware ? "ob-pill--ok" : "ob-pill--info"}`}>
            <Radio size={12} />
            <span>信号源：{activePose.sourceLabel}</span>
          </span>
          <button
            type="button"
            className="btn btn-ghost"
            onClick={() => onNavigate?.("/")}
            title="前往雕刻工作台创建或下发任务"
          >
            <span>返回工作台</span>
            <ArrowUpRight size={13} />
          </button>
        </div>
      </div>

      {/* 主体双栏：左侧 3D 立体机台视口 + 仿真时间轴；右侧三张遥测与控制卡片 */}
      <div className="twin-main-grid">
        <div className="twin-left-col">
          <div className="hud-card ob-card twin-viewport-card">
            <TwinStage3D
              workspace={workspace}
              toolpath={toolpath}
              pose={activePose}
              showZAxis={hasZAxis}
              showRapid={showRapid}
              followHead={followHead}
              onToggleRapid={() => setShowRapid((v) => !v)}
              onToggleFollow={() => setFollowHead((v) => !v)}
            />

            {/* 底部仿真控制与实时 G-code 指令进度栏（严格 1:1 真实雕刻时长，支持折叠收起） */}
            <div className={`twin-timeline-bar ${timelineOpen ? "" : "is-collapsed"}`}>
              {timelineOpen ? (
                <>
                  <div className="twin-timeline-controls">
                    <button
                      type="button"
                      className="btn btn-hud-primary twin-play-btn"
                      onClick={() => {
                        if (driveMode !== "sim") setDriveMode("sim");
                        if (simProgress >= 1) setSimProgress(0);
                        setSimPlaying((p) => !p);
                      }}
                      id="btn-twin-play"
                    >
                      {simPlaying ? <Pause size={14} /> : <Play size={14} />}
                      <span>{simPlaying ? "暂停推演" : "播放推演"}</span>
                    </button>

                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={() => {
                        setSimPlaying(false);
                        setSimProgress(0);
                      }}
                      title="重置到刀路起点 (0,0)"
                    >
                      <RotateCcw size={13} />
                    </button>

                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={() => stepByLine(-1)}
                      title="退回上一段轨迹"
                    >
                      <SkipBack size={13} />
                    </button>

                    <button
                      type="button"
                      className="btn btn-ghost"
                      onClick={() => stepByLine(1)}
                      title="步进下一段轨迹"
                    >
                      <SkipForward size={13} />
                    </button>

                    <button
                      type="button"
                      id="btn-twin-loop"
                      aria-pressed={simLoop}
                      className={`btn btn-ghost twin-speed-btn twin-loop-btn ${simLoop ? "active is-looping" : ""}`}
                      onClick={() => {
                        const nextLoop = !simLoop;
                        setSimLoop(nextLoop);
                        if (nextLoop && driveMode === "sim" && !simPlaying && simProgress >= 1) {
                          setSimProgress(0);
                          setSimPlaying(true);
                        }
                      }}
                      title={simLoop ? "当前已开启循环推演，点击关闭循环" : "当前为单次推演，点击开启循环推演"}
                    >
                      <RefreshCw size={12} />
                      <span>{simLoop ? "循环：开启" : "循环：关闭"}</span>
                    </button>

                    <div className="twin-speed-group" aria-label="1:1 真实雕刻时长对应">
                      <span
                        className="ob-pill ob-pill--info"
                        style={{ fontFamily: "var(--ob-mono)", fontSize: "11.5px" }}
                        title="按 G-code 每段进给速度与长度 1:1 实时推演"
                      >
                        <span>
                          1:1 实时推演 · 剩余{" "}
                          {(
                            activePose.remainingSeconds ??
                            Math.max(0, toolpath.estimatedSeconds - (activePose.elapsedSeconds || 0))
                          ).toFixed(1)}
                          s
                        </span>
                      </span>
                      <button
                        type="button"
                        className="btn btn-ghost twin-hud-collapse-btn twin-timeline-collapse-btn"
                        onClick={toggleTimelineOpen}
                        aria-expanded={timelineOpen}
                        title="收起底部刀路推演控制、进度条与实时 G-code 指令栏"
                      >
                        <ChevronDown size={13} />
                        <span>收起</span>
                      </button>
                    </div>
                  </div>

                  <div className="twin-scrubber-row">
                    <input
                      type="range"
                      min={0}
                      max={1000}
                      value={Math.round((activePose.progressRatio || 0) * 1000)}
                      onChange={(e) => {
                        setDriveMode("sim");
                        setSimPlaying(false);
                        setSimProgress(Number(e.target.value) / 1000);
                      }}
                      className="twin-range-slider"
                      aria-label="刀路仿真时间进度条"
                    />
                    <span className="twin-scrubber-pct" style={{ minWidth: "118px" }}>
                      {(activePose.elapsedSeconds || 0).toFixed(1)}s / {toolpath.estimatedSeconds.toFixed(1)}s (
                      {Math.round((activePose.progressRatio || 0) * 100)}%)
                    </span>
                  </div>

                  <div className="twin-gcode-ticker">
                    <span className="twin-gcode-line-badge">
                      第 {activePose.lineNumber || 0} / {toolpath.totalLines} 行
                    </span>
                    <code className="twin-gcode-code">
                      {activePose.rawLine || "G0 X0.000 Y0.000 ; 等待刀路指令"}
                    </code>
                    <span className="twin-traveled-tag">
                      已走 {(activePose.traveledMm || 0).toFixed(1)} / {toolpath.totalLengthMm.toFixed(1)} mm · F
                      {Math.round(activePose.feedRate || 0)} mm/min
                    </span>
                  </div>
                </>
              ) : (
                <div className="twin-timeline-collapsed-bar">
                  <span className="twin-timeline-collapsed-summary">
                    <span>刀路推演栏已收起</span>
                    <span>·</span>
                    <span>
                      {(activePose.elapsedSeconds || 0).toFixed(1)}s / {toolpath.estimatedSeconds.toFixed(1)}s (
                      {Math.round((activePose.progressRatio || 0) * 100)}%)
                    </span>
                    <span>·</span>
                    <span>
                      第 {activePose.lineNumber || 0} / {toolpath.totalLines} 行
                    </span>
                    <span>·</span>
                    <span style={{ color: simLoop ? "var(--ob-green-2, #34d399)" : "var(--ob-ink-3)" }}>
                      {simLoop ? "循环：已开启" : "循环：未开启"}
                    </span>
                  </span>
                  <button
                    type="button"
                    className="btn btn-ghost twin-hud-collapse-btn is-collapsed twin-timeline-collapse-btn"
                    onClick={toggleTimelineOpen}
                    aria-expanded={timelineOpen}
                    title="展开底部刀路推演控制、进度条与实时 G-code 指令栏"
                  >
                    <ChevronUp size={13} />
                    <span>展开</span>
                  </button>
                </div>
              )}
            </div>
          </div>
        </div>

        {/* 右侧控制与遥测面板 */}
        <div className="twin-right-col">
          {/* 卡片 1：刀路源与任务同步 */}
          <section className="hud-card ob-card twin-side-card">
            <FoldHeader id="twin-source" title="刀路源与任务同步" n={1} />
            <div className="twin-card-body">
              <div className="twin-source-current">
                <span className="twin-field-label">当前挂载刀路</span>
                <div className="twin-source-value">{gcodeSourceLabel}</div>
              </div>

              <div className="twin-row-actions">
                <select
                  value={selectedWorkflowId}
                  onChange={(e) => {
                    if (!e.target.value) {
                      resetToDemoToolpath();
                      return;
                    }
                    const wf = recentWorkflows.find((w) => w.workflow_id === e.target.value);
                    if (wf) void loadWorkflowToolpath(wf);
                  }}
                  className="select-input twin-select"
                  aria-label="选择历史或当前工作流任务"
                >
                  <option value="" disabled={isJobRunning}>
                    {recentWorkflows.length > 0 ? "内置演示刀路 (60×60mm)" : "暂无工作流记录 (使用演示刀路)"}
                  </option>
                  {recentWorkflows.map((wf) => (
                    <option key={wf.workflow_id} value={wf.workflow_id}>
                      {formatTwinWorkflowLabel(wf, { includeStatus: true })}
                    </option>
                  ))}
                </select>

                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={fetchRecentWorkflows}
                  disabled={loadingWorkflow}
                  title="刷新工作流任务列表"
                >
                  <RefreshCw size={13} />
                </button>
              </div>

              <label className="btn btn-ghost twin-upload-label">
                <Upload size={13} />
                <span>加载本地 .gcode / .nc 文件推演</span>
                <input
                  type="file"
                  accept=".gcode,.nc,.txt"
                  onChange={handleLocalGcodeImport}
                  hidden
                />
              </label>

              {/* 刀路量化指标网格 */}
              <div className="twin-metrics-grid">
                <div className="twin-metric-box">
                  <span>G-code 几何包络</span>
                  <b>
                    {toolpath.bounds.widthMm.toFixed(1)} × {toolpath.bounds.heightMm.toFixed(1)} mm
                  </b>
                </div>
                <div className="twin-metric-box">
                  <span>3D 材料外框尺寸</span>
                  <b>
                    {workspace.material.widthMm.toFixed(1)} × {workspace.material.heightMm.toFixed(1)} mm
                  </b>
                </div>
                <div className="twin-metric-box">
                  <span>出光雕刻轨迹</span>
                  <b>{toolpath.cutLengthMm.toFixed(1)} mm</b>
                </div>
                <div className="twin-metric-box">
                  <span>G-code 推算总时长 (1:1)</span>
                  <b>{formatDurationSeconds(toolpath.estimatedSeconds)}</b>
                  <small style={{ fontSize: "10.5px", color: "var(--ob-ink-3)" }}>
                    雕刻 {(toolpath.cutSeconds || 0).toFixed(1)}s + 空移 {(toolpath.rapidSeconds || 0).toFixed(1)}s
                  </small>
                </div>
              </div>

              {/* 3D 场景摆位预览（默认收起，避免把遥测卡与行程安全卡挤出首屏，且标明不影响实际 G-code） */}
              <details className="fill-options twin-layout-fold" id="twin-scene-layout">
                <summary className="twin-layout-fold-summary">
                  <span>3D 摆位预览（仅影响仿真画面）</span>
                  <small className="twin-layout-fold-meta">
                    {workspace.material.autoSize ? "1:1 贴合刀路" : `${workspace.material.widthMm}×${workspace.material.heightMm}mm`} · 起点 ({workspace.offsetX}, {workspace.offsetY})
                  </small>
                </summary>
                <p className="wb-hint twin-layout-note">
                  这里只调整本页 3D 画面里的材料尺寸与摆放位置，不改动实际 G-code 刀路；真机对刀请用工作台的「对刀起点」或「现场移头」。
                </p>

                <div className="twin-pins-box twin-layout-group">
                  <div className="twin-telemetry-head twin-layout-group-head">
                    <span className="twin-field-label">
                      材料外框尺寸与台面起点
                    </span>
                    <span className="ob-pill ob-pill--info">
                      <span>
                        {workspace.material.autoSize ? "1:1 同步 G-code" : "自定义外框"} · 起点 ({workspace.offsetX}, {workspace.offsetY})
                      </span>
                    </span>
                  </div>

                  <div className="twin-row-actions twin-layout-group-actions">
                  <button
                    type="button"
                    className={`btn btn-ghost twin-speed-btn ${workspace.material.autoSize ? "active" : ""}`}
                    onClick={() => updateMaterialConfig({ autoSize: true })}
                    title="让 3D 材料外框尺寸与 G-code 几何包络严格 1:1 一致"
                  >
                    <span>1:1 同步刀路尺寸</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost twin-speed-btn"
                    onClick={() =>
                      updateMaterialConfig({
                        autoSize: false,
                        widthMm: Math.min(workspace.xTravelMm, Number(((toolpath.bounds.widthMm || 60) + 10).toFixed(1))),
                        heightMm: Math.min(workspace.yTravelMm, Number(((toolpath.bounds.heightMm || 60) + 10).toFixed(1))),
                      })
                    }
                    title="在 G-code 尺寸基础上外扩 10mm 作为板材外框"
                  >
                    <span>外扩 +10mm</span>
                  </button>
                  <button
                    type="button"
                    className={`btn btn-ghost twin-speed-btn ${!workspace.material.autoSize && workspace.material.widthMm === workspace.xTravelMm && workspace.material.heightMm === workspace.yTravelMm ? "active" : ""}`}
                    onClick={() =>
                      updateMaterialConfig({
                        autoSize: false,
                        widthMm: workspace.xTravelMm,
                        heightMm: workspace.yTravelMm,
                        offsetX: 0,
                        offsetY: 0,
                      })
                    }
                    title="将材料外框设为铺满当前机台可调工作区 (如 100×100mm)"
                  >
                    <span>铺满台面</span>
                  </button>
                  <button
                    type="button"
                    className={`btn btn-ghost twin-speed-btn ${workspace.offsetX === 0 && workspace.offsetY === 0 ? "active" : ""}`}
                    onClick={() => updateMaterialConfig({ offsetX: 0, offsetY: 0 })}
                    title="将材料、刀路与激光头起点重置到左下角 (0, 0)"
                  >
                    <span>左下角 (0,0)</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost twin-speed-btn"
                    onClick={() => {
                      const spanW = workspace.material.widthMm;
                      const spanH = workspace.material.heightMm;
                      const cx = Math.max(0, Number(((workspace.xTravelMm - spanW) / 2).toFixed(1)));
                      const cy = Math.max(0, Number(((workspace.yTravelMm - spanH) / 2).toFixed(1)));
                      updateMaterialConfig({ offsetX: cx, offsetY: cy });
                    }}
                    title={`将材料、刀路与激光头一起移动到 ${workspace.xTravelMm}×${workspace.yTravelMm} mm 台面正中央`}
                  >
                    <span>一键台面居中</span>
                  </button>
                </div>

                <div className="twin-metrics-grid">
                  <label className="twin-metric-box">
                    <span>材料宽 W (mm)</span>
                    <input
                      type="number"
                      min={workspace.material.minWidthMm || 1}
                      max={workspace.xTravelMm}
                      step={0.5}
                      value={widthDraft !== null ? widthDraft : workspace.material.widthMm}
                      onChange={(e) => {
                        const raw = e.target.value;
                        setWidthDraft(raw);
                        const num = Number(raw);
                        if (raw !== "" && Number.isFinite(num) && num >= 1) {
                          const clampedW = Math.min(workspace.xTravelMm, Math.max(1, Number(num.toFixed(1))));
                          updateMaterialConfig({
                            autoSize: false,
                            widthMm: clampedW,
                            heightMm: workspace.material.heightMm,
                          });
                        }
                      }}
                      onBlur={() => setWidthDraft(null)}
                      className="text-input"
                      aria-label="材料外框宽度 W (mm)"
                    />
                    <input
                      type="range"
                      min={10}
                      max={workspace.xTravelMm}
                      step={0.5}
                      value={workspace.material.widthMm}
                      onChange={(e) => {
                        setWidthDraft(null);
                        const clampedW = Math.min(workspace.xTravelMm, Math.max(1, Number(e.target.value)));
                        updateMaterialConfig({
                          autoSize: false,
                          widthMm: clampedW,
                          heightMm: workspace.material.heightMm,
                        });
                      }}
                      className="twin-range-slider"
                      aria-label="拖动调节材料宽 W (mm)"
                    />
                  </label>
                  <label className="twin-metric-box">
                    <span>材料高 H (mm)</span>
                    <input
                      type="number"
                      min={workspace.material.minHeightMm || 1}
                      max={workspace.yTravelMm}
                      step={0.5}
                      value={heightDraft !== null ? heightDraft : workspace.material.heightMm}
                      onChange={(e) => {
                        const raw = e.target.value;
                        setHeightDraft(raw);
                        const num = Number(raw);
                        if (raw !== "" && Number.isFinite(num) && num >= 1) {
                          const clampedH = Math.min(workspace.yTravelMm, Math.max(1, Number(num.toFixed(1))));
                          updateMaterialConfig({
                            autoSize: false,
                            widthMm: workspace.material.widthMm,
                            heightMm: clampedH,
                          });
                        }
                      }}
                      onBlur={() => setHeightDraft(null)}
                      className="text-input"
                      aria-label="材料外框高度 H (mm)"
                    />
                    <input
                      type="range"
                      min={10}
                      max={workspace.yTravelMm}
                      step={0.5}
                      value={workspace.material.heightMm}
                      onChange={(e) => {
                        setHeightDraft(null);
                        const clampedH = Math.min(workspace.yTravelMm, Math.max(1, Number(e.target.value)));
                        updateMaterialConfig({
                          autoSize: false,
                          widthMm: workspace.material.widthMm,
                          heightMm: clampedH,
                        });
                      }}
                      className="twin-range-slider"
                      aria-label="拖动调节材料高 H (mm)"
                    />
                  </label>
                  <label className="twin-metric-box">
                    <span>起点坐标 X0 (mm)</span>
                    <input
                      type="number"
                      min={0}
                      max={workspace.material.maxOffsetX}
                      step={0.5}
                      value={workspace.offsetX}
                      onChange={(e) => updateMaterialConfig({ offsetX: e.target.value })}
                      className="text-input"
                      aria-label="材料与激光起点坐标 X0 (mm)"
                    />
                    <input
                      type="range"
                      min={0}
                      max={workspace.material.maxOffsetX}
                      step={0.5}
                      value={workspace.offsetX}
                      onChange={(e) => updateMaterialConfig({ offsetX: e.target.value })}
                      className="twin-range-slider"
                      aria-label="拖动调节 X0 坐标"
                    />
                  </label>
                  <label className="twin-metric-box">
                    <span>起点坐标 Y0 (mm)</span>
                    <input
                      type="number"
                      min={0}
                      max={workspace.material.maxOffsetY}
                      step={0.5}
                      value={workspace.offsetY}
                      onChange={(e) => updateMaterialConfig({ offsetY: e.target.value })}
                      className="text-input"
                      aria-label="材料与激光起点坐标 Y0 (mm)"
                    />
                    <input
                      type="range"
                      min={0}
                      max={workspace.material.maxOffsetY}
                      step={0.5}
                      value={workspace.offsetY}
                      onChange={(e) => updateMaterialConfig({ offsetY: e.target.value })}
                      className="twin-range-slider"
                      aria-label="拖动调节 Y0 坐标"
                    />
                  </label>
                </div>
              </div>
              </details>
              {driveMode === "job" && (
                <div className="twin-job-sync-box">
                  <div className="twin-job-sync-head">
                    <span>实时下发状态：{twinStatusLabel(jobStatus)}</span>
                    <b>
                      {jobSendResult?.lines_sent || 0} / {jobSendResult?.total_lines || toolpath.totalLines} 行
                    </b>
                  </div>
                  <div className="streaming-progress-track">
                    <div
                      className={`streaming-progress-bar ${isJobRunning ? "active" : ""}`}
                      style={{
                        width: `${
                          jobSendResult?.total_lines
                            ? Math.min(100, Math.round((jobSendResult.lines_sent / jobSendResult.total_lines) * 100))
                            : 0
                        }%`,
                      }}
                    />
                  </div>
                </div>
              )}
            </div>
          </section>

          {/* 卡片 2：控制器实时遥测映射 */}
          <section className="hud-card ob-card twin-side-card">
            <FoldHeader id="twin-telemetry" title="控制器实时遥测" n={2} />
            <div className="twin-card-body">
              <div className="conn-summary-card twin-conn-summary" id="twin-conn-summary">
                <div className="conn-summary-info">
                  <span className={`conn-summary-dot is-${connectionView.statusTone}`} aria-hidden="true" />
                  <span className="conn-summary-mode">{connectionView.modeLabel}</span>
                  <span className="conn-summary-sep" aria-hidden="true">·</span>
                  <span className={`conn-summary-target mono${connectionView.targetReady ? "" : " is-empty"}`}>
                    {connectionView.targetText}
                  </span>
                  <span className="conn-summary-sep" aria-hidden="true">·</span>
                  <span className={`conn-summary-status is-${connectionView.statusTone}`}>
                    {connectionView.statusText}
                  </span>
                </div>
                <button
                  type="button"
                  id="twin-open-connection"
                  className="btn btn-ghost wb-btn conn-summary-btn"
                  onClick={onOpenConnection}
                >
                  <Settings2 size={13} className="wb-icon" aria-hidden="true" />
                  <span>修改连接</span>
                </button>
              </div>
              <div className="twin-telemetry-head">
                <span className={`ob-pill ob-pill--${telemetry.tone || "off"}`}>
                  <span>{telemetry.stateLabel}</span>
                </span>
                <span className="twin-telemetry-time">
                  {lastTelemetryAt ? `采样于 ${lastTelemetryAt}` : "尚未采样"}
                </span>
              </div>

              <div className="twin-row-actions">
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() => sampleControllerStatus(false)}
                  id="btn-twin-sample-status"
                  title="向控制器查询一次当前位置与状态"
                >
                  <Activity size={13} />
                  <span>刷新状态</span>
                </button>

                <button
                  type="button"
                  className={`btn ${pollingTelemetry ? "btn-hud-primary" : "btn-ghost"}`}
                  onClick={() => setPollingTelemetry((v) => !v)}
                  id="btn-twin-toggle-poll"
                >
                  <Radio size={13} />
                  <span>{pollingTelemetry ? "停止自动刷新" : "每 2 秒自动刷新"}</span>
                </button>
              </div>

              {/* MPos / WPos 机械与工作坐标对照表 */}
              <div className="twin-pos-table">
                <div className="twin-pos-row twin-pos-row--head">
                  <span>坐标轴</span>
                  <span>工作坐标</span>
                  <span>机械坐标</span>
                </div>
                {(hasZAxis ? ["x", "y", "z"] : ["x", "y"]).map((axis) => (
                  <div key={axis} className="twin-pos-row">
                    <span className="twin-axis-tag">{axis.toUpperCase()} 轴</span>
                    <b>{telemetry.wpos ? `${telemetry.wpos[axis].toFixed(3)} mm` : "—"}</b>
                    <span>{telemetry.mpos ? `${telemetry.mpos[axis].toFixed(3)} mm` : "—"}</span>
                  </div>
                ))}
              </div>

              {/* 运动队列与接收缓冲余量（仅在控制器实际汇报 Bf: 字段时显示） */}
              {hasBufferTelemetry && (
                <div className="twin-metrics-grid">
                  <div className="twin-metric-box" title="控制器规划器空闲缓冲块数 (Bf)">
                    <span>运动队列余量</span>
                    <b>{telemetry.bf?.plannerFree != null ? `${telemetry.bf.plannerFree} 块` : "—"}</b>
                  </div>
                  <div className="twin-metric-box" title="控制器串口 / 网络接收缓冲余量 (RX)">
                    <span>指令接收余量</span>
                    <b>{telemetry.bf?.rxBufferFree != null ? `${telemetry.bf.rxBufferFree} B` : "—"}</b>
                  </div>
                </div>
              )}

              {/* 限位与安全开关状态 */}
              <div className="twin-pins-box">
                <span className="twin-field-label">限位与安全开关</span>
                {telemetry.pins?.active?.length > 0 ? (
                  <div className="twin-pin-list">
                    {telemetry.pins.active.map((pin) => (
                      <span
                        key={pin.code}
                        className={`ob-pill ${pin.blocking ? "ob-pill--bad" : "ob-pill--warn"}`}
                      >
                        <AlertTriangle size={11} />
                        <span>{pin.label}</span>
                      </span>
                    ))}
                  </div>
                ) : (
                  <div className="twin-pin-ok" data-switch-state={safetySwitchStatus.state}>
                    <CheckCircle2 size={13} />
                    <span>{safetySwitchStatus.text}</span>
                  </div>
                )}
              </div>

              {peripherals && (
                <div className="twin-pins-box" id="twin-machine-peripherals">
                  <span className="twin-field-label">固件外设与功率映射</span>
                  <div className="twin-pin-ok">
                    <span>
                      {peripherals.airAssistLabel ? `气泵：${peripherals.airAssistLabel}` : ""}
                      {peripherals.airAssistLabel && peripherals.speedMap ? " · " : ""}
                      {peripherals.speedMap ? (
                        <span>
                          speed_map：<code>{peripherals.speedMap}</code>
                        </span>
                      ) : null}
                    </span>
                  </div>
                  {peripherals.speedMapWarning ? (
                    <div className="twin-bounds-alert" id="twin-speed-map-warning" role="note" style={{ marginTop: "6px" }}>
                      <AlertTriangle size={14} />
                      <span>{peripherals.speedMapWarning}</span>
                    </div>
                  ) : null}
                </div>
              )}

              {telemetry.raw && (
                <code className="twin-raw-status" title="控制器原始状态报告">
                  {telemetry.raw}
                </code>
              )}
            </div>
          </section>

          {/* 卡片 3：工作区行程与安全制动 */}
          <section className="hud-card ob-card twin-side-card">
            <FoldHeader id="twin-envelope" title="工作区行程与安全" n={3} />
            <div className="twin-card-body">
              <div className="twin-row-actions">
                <button
                  type="button"
                  className={`btn btn-ghost twin-speed-btn ${workspace.xTravelMm === 100 && workspace.yTravelMm === 100 ? "active" : ""}`}
                  onClick={() => updateManualWorkspaceLimits({ x_travel_mm: 100, y_travel_mm: 100 })}
                  title="手动设为 100×100 mm 行程"
                >
                  <span>100×100 满行程</span>
                </button>
                <button
                  type="button"
                  className={`btn btn-ghost twin-speed-btn ${workspace.xTravelMm === 90 && workspace.yTravelMm === 90 ? "active" : ""}`}
                  onClick={() => updateManualWorkspaceLimits({ x_travel_mm: 90, y_travel_mm: 90 })}
                  title="四周预留 5mm 安全边距 (90×90 mm 安全雕刻区)"
                >
                  <span>90×90 安全区</span>
                </button>
                {machineLimits.has_manual ? (
                  <button
                    type="button"
                    id="btn-twin-restore-hardware-limits"
                    className="btn btn-ghost twin-speed-btn"
                    onClick={handleRestoreHardwareLimits}
                    title="清除本连接目标的手填行程，恢复为固件上报值（未上报时回退默认 100×100 mm）"
                  >
                    <span>恢复为固件值</span>
                  </button>
                ) : null}
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={handleSyncMachineCapabilities}
                  disabled={probingDevice}
                  id="btn-twin-sync-limits"
                  title="从已连接的 FluidNC / GRBL 控制器读取实机行程配置（不会覆盖手填值）"
                >
                  <RefreshCw size={13} />
                  <span>{probingDevice ? "读取中..." : "读取实机行程"}</span>
                </button>
              </div>

              {/* 雕刻机可调范围微调 (20 ~ 2000 mm) */}
              <div className="twin-metrics-grid">
                <label className="twin-metric-box">
                  <span>X 轴可调行程 (≤2000mm)</span>
                  <input
                    type="number"
                    min={20}
                    max={MAX_XY_TRAVEL_MM}
                    step={1}
                    value={workspace.xTravelMm}
                    onChange={(e) => updateManualWorkspaceLimits({ x_travel_mm: e.target.value })}
                    className="text-input"
                    aria-label="X 轴可调行程 (mm)"
                  />
                </label>
                <label className="twin-metric-box">
                  <span>Y 轴可调行程 (≤2000mm)</span>
                  <input
                    type="number"
                    min={20}
                    max={MAX_XY_TRAVEL_MM}
                    step={1}
                    value={workspace.yTravelMm}
                    onChange={(e) => updateManualWorkspaceLimits({ y_travel_mm: e.target.value })}
                    className="text-input"
                    aria-label="Y 轴可调行程 (mm)"
                  />
                </label>
                <div className="twin-metric-box">
                  <span>当前有效工作区</span>
                  <b>
                    {workspace.xTravelMm} × {workspace.yTravelMm} mm
                  </b>
                </div>
                <div className="twin-metric-box" id="twin-limits-source">
                  <span>行程来源</span>
                  <b>{limitsSourceLabel}</b>
                </div>
              </div>
              {limitsExceedWarning ? (
                <div className="twin-bounds-alert" id="twin-limits-exceed-warning" role="alert">
                  <AlertTriangle size={15} />
                  <span>{limitsExceedWarning}</span>
                </div>
              ) : null}
              {boundsWarning ? (
                <div className="twin-bounds-alert" role="alert">
                  <ShieldAlert size={15} />
                  <span>{boundsWarning}</span>
                </div>
              ) : (
                <div className="twin-bounds-ok">
                  <CheckCircle2 size={14} />
                  <span>刀路包络完全处于机台有效行程范围内</span>
                </div>
              )}

              <button
                type="button"
                className="btn-estop-large"
                onClick={handleTwinEmergencyStop}
                title="发送 0x18 软件复位；紧急情况请按机器实体急停"
              >
                <AlertTriangle size={16} />
                <span>紧急停止（软件复位）</span>
              </button>
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
