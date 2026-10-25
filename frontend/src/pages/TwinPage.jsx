import React, { useState, useEffect, useRef, useMemo, useCallback } from "react";
import {
  Play,
  Pause,
  RotateCcw,
  SkipBack,
  SkipForward,
  Activity,
  Cpu,
  Radio,
  Layers,
  Gauge,
  AlertTriangle,
  RefreshCw,
  Upload,
  CheckCircle2,
  ShieldAlert,
  Sliders,
  Flame,
  ArrowUpRight,
  ChevronUp,
  ChevronDown,
} from "lucide-react";
import { apiFetch, getJson, postJson } from "../api/client.js";
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
  YISU_V1_PROFILE,
} from "../twinState.js";
import { TERMINAL_STATUSES } from "../state/statusNormalize.js";
import { showAlert } from "../appDialog.js";

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

const WORKSPACE_STORAGE_KEY = "moss_yisu_workspace_limits_v1";
const MATERIAL_STORAGE_KEY = "moss_yisu_material_config_v1";

function loadSavedWorkspaceLimits() {
  try {
    const raw = localStorage.getItem(WORKSPACE_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      const x = Number(parsed?.x_travel_mm);
      const y = Number(parsed?.y_travel_mm);
      const z = Number(parsed?.z_travel_mm);
      if (x >= 20 && x <= 100 && y >= 20 && y <= 100) {
        return {
          x_travel_mm: x,
          y_travel_mm: y,
          z_travel_mm: z > 0 ? z : YISU_V1_PROFILE.zTravelMm,
          from_hardware: Boolean(parsed?.from_hardware),
        };
      }
    }
  } catch (_e) {
    // ignore
  }
  return {
    x_travel_mm: YISU_V1_PROFILE.xTravelMm,
    y_travel_mm: YISU_V1_PROFILE.yTravelMm,
    z_travel_mm: YISU_V1_PROFILE.zTravelMm,
    from_hardware: false,
  };
}

function loadSavedMaterialConfig() {
  try {
    const raw = localStorage.getItem(MATERIAL_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (parsed && typeof parsed === "object") {
        return {
          autoSize: Boolean(parsed.autoSize),
          widthMm: Number(parsed.widthMm) > 0 ? Number(parsed.widthMm) : 80,
          heightMm: Number(parsed.heightMm) > 0 ? Number(parsed.heightMm) : 80,
          thicknessMm: Number(parsed.thicknessMm) > 0 ? Number(parsed.thicknessMm) : 3,
          offsetX: Number.isFinite(Number(parsed.offsetX)) ? Number(parsed.offsetX) : 0,
          offsetY: Number.isFinite(Number(parsed.offsetY)) ? Number(parsed.offsetY) : 0,
          textOffsetX: Number.isFinite(Number(parsed.textOffsetX)) ? Number(parsed.textOffsetX) : 0,
          textOffsetY: Number.isFinite(Number(parsed.textOffsetY)) ? Number(parsed.textOffsetY) : 0,
        };
      }
    }
  } catch (_e) {
    // ignore
  }
  return {
    autoSize: false,
    widthMm: 80,
    heightMm: 80,
    thicknessMm: 3,
    offsetX: 0,
    offsetY: 0,
    textOffsetX: 0,
    textOffsetY: 0,
  };
}
export function TwinPage({ onNavigate, connectionStatus, setConnectionStatus }) {
  // 驱动模式：sim (离线刀路推演) | job (加工下发同步) | telemetry (实机坐标映射)
  const [driveMode, setDriveMode] = useState("sim");

  // 工作流与刀路数据
  const [recentWorkflows, setRecentWorkflows] = useState([]);
  const [selectedWorkflowId, setSelectedWorkflowId] = useState("");
  const [activeWorkflow, setActiveWorkflow] = useState(null);
  const [gcodeText, setGcodeText] = useState(DEMO_GCODE);
  const [gcodeSourceLabel, setGcodeSourceLabel] = useState("翼宿 V1.0 演示刀路 (100×100mm)");
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

  // 控制器硬件行程与实时遥测状态（默认对齐大鱼 DIY 翼宿 V1.0 的 100×100mm 可调范围）
  const [machineLimits, setMachineLimits] = useState(() => loadSavedWorkspaceLimits());
  const [machineFeatures, setMachineFeatures] = useState(null);

  // 材料外框尺寸、台面摆放起点 (offsetX, offsetY) 与激光雕刻文字在材料内的位置 (textOffsetX, textOffsetY)
  const [materialConfig, setMaterialConfig] = useState(() => loadSavedMaterialConfig());
  const [widthDraft, setWidthDraft] = useState(null);
  const [heightDraft, setHeightDraft] = useState(null);

  const updateMaterialConfig = useCallback((patch) => {
    setMaterialConfig((prev) => {
      const next = {
        ...prev,
        ...patch,
      };
      try {
        localStorage.setItem(MATERIAL_STORAGE_KEY, JSON.stringify(next));
      } catch (_e) {
        // ignore
      }
      return next;
    });
  }, []);
  const updateWorkspaceLimits = useCallback((patch) => {
    setMachineLimits((prev) => {
      const base = prev || loadSavedWorkspaceLimits();
      const next = {
        ...base,
        ...patch,
        x_travel_mm: Math.min(100, Math.max(20, Number(patch?.x_travel_mm ?? base.x_travel_mm) || 100)),
        y_travel_mm: Math.min(100, Math.max(20, Number(patch?.y_travel_mm ?? base.y_travel_mm) || 100)),
        z_travel_mm: Math.min(50, Math.max(5, Number(patch?.z_travel_mm ?? base.z_travel_mm) || 20)),
      };
      try {
        localStorage.setItem(WORKSPACE_STORAGE_KEY, JSON.stringify(next));
      } catch (_e) {
        // ignore
      }
      return next;
    });
  }, []);
  const [telemetry, setTelemetry] = useState(() => parseGrblStatusReport(""));
  const [pollingTelemetry, setPollingTelemetry] = useState(false);
  const [probingDevice, setProbingDevice] = useState(false);
  const [lastTelemetryAt, setLastTelemetryAt] = useState(null);

  // 读取工作台保存的连接参数
  const getConnectionPayload = useCallback(() => {
    const mode = localStorage.getItem("laser-forge-conn-mode") || "network";
    const port = localStorage.getItem("laser-forge-serial-port") || "";
    const baudrate = Number(localStorage.getItem("laser-forge-baud") || 115200);
    const networkHost = localStorage.getItem("laser-forge-host") || "";
    const telnetPort = Number(localStorage.getItem("laser-forge-telnet-port") || 23);
    return {
      connection_mode: mode,
      port,
      baudrate,
      network_host: networkHost,
       host: networkHost,
      network_telnet_port: telnetPort,
      telnet_port: telnetPort,
    };
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

  // 刀路与材料包络越界检查（包含板材台面起点 offsetX/Y、材料内雕刻偏移 textOffsetX/Y 与缩放比例）
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

  // 加载指定 workflow 的 G-code 刀路文件
  const loadWorkflowToolpath = useCallback(async (wf) => {
    if (!wf) return;
    setActiveWorkflow(wf);
    setSelectedWorkflowId(wf.workflow_id || "");
    setJobStatus(wf.status || "preview_ready");
    setMaterialConfig((prev) => ({
      ...prev,
      thicknessMm: Number(wf.artifacts?.thickness_mm || wf.thickness_mm) || prev.thicknessMm || 3,
    }));
    if (wf.send_result) {
      setJobSendResult(wf.send_result);
    }
    const gcodeFile = wf.artifacts?.gcode_file || wf.gcode_file;
    if (!gcodeFile) return;

    setLoadingWorkflow(true);
    try {
      const res = await apiFetch(`/preview?path=${encodeURIComponent(gcodeFile)}`);
      if (res.ok) {
        const text = await res.text();
        if (text && text.trim().length > 0) {
          setGcodeText(text);
          const mat = wf.artifacts?.material || wf.material || "未知材质";
          const thick = wf.artifacts?.thickness_mm || wf.thickness_mm || "";
          setGcodeSourceLabel(`任务 ${wf.workflow_id} · ${mat} ${thick ? `${thick}mm` : ""}`);
          setSimProgress(0);
          setSimPlaying(true);
        }
      }
    } catch (_err) {
      // 保留当前刀路
    } finally {
      setLoadingWorkflow(false);
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

  useEffect(() => {
    void fetchRecentWorkflows();
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

    rafId = requestAnimationFrame(tick);
    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
    };
  }, [simPlaying, simLoop, driveMode, toolpath.estimatedSeconds]);

  // 当处于“加工下发同步”模式且有选定 workflow 时，轮询 /api/workflow status
  useEffect(() => {
    if (driveMode !== "job" || !selectedWorkflowId) return undefined;
    let cancelled = false;
    let timer = null;

    const pollWorkflow = async () => {
      try {
        const res = await postJson("/api/workflow", {
          action: "status",
          workflow_id: selectedWorkflowId,
        });
        if (!cancelled && res.ok && res.data?.success) {
          const wf = res.data.result;
          setJobStatus(wf.status || "preview_ready");
          if (wf.send_result) {
            setJobSendResult(wf.send_result);
          }
          if (TERMINAL_STATUSES.has(wf.status) && !wf.send_result?.running) {
            return;
          }
        }
      } catch (_e) {
        // 保持上次已知状态
      }
      if (!cancelled) {
        timer = setTimeout(pollWorkflow, 1000);
      }
    };

    void pollWorkflow();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [driveMode, selectedWorkflowId]);

  // 同步机台物理行程与外设能力 (/api/connection/check)
  const handleSyncMachineCapabilities = async () => {
    if (probingDevice) return;
    setProbingDevice(true);
    try {
      const payload = getConnectionPayload();
      const res = await postJson("/api/connection/check", payload);
      if (res.ok && res.data?.success) {
        const r = res.data.result || {};
        const isConn = Boolean(r.connected || r.serial_connected || r.network_connected);
        setConnectionStatus?.({
          connected: isConn,
          summary: r.summary || (isConn ? "硬件握手通过" : "未连接"),
          checkedAt: Date.now(),
        });
        if (r.machine_limits) {
          updateWorkspaceLimits({
            ...r.machine_limits,
            from_hardware: true,
          });
        }
        if (r.features) setMachineFeatures(r.features);
      }
    } catch (_e) {
      // ignore
    } finally {
      setProbingDevice(false);
    }
  };

  // 单次采样控制器 GRBL 实时状态 (/api/safe-action { action: "status" })
  const sampleControllerStatus = useCallback(async (silent = false) => {
    try {
      const payload = {
        ...getConnectionPayload(),
        action: "status",
      };
      const res = await postJson("/api/safe-action", payload);
      if (res.ok && res.data?.success) {
        const detail = res.data.detail ?? res.data.result;
        const parsed = parseGrblStatusReport(detail, String(res.data.result || ""));
        setTelemetry(parsed);
        setLastTelemetryAt(new Date().toLocaleTimeString());
        if (parsed.online) {
          setConnectionStatus?.({
            connected: true,
            summary: `状态 ${parsed.state}`,
            checkedAt: Date.now(),
          });
        }
        return parsed;
      }
      if (!silent) {
        const msg = res.data?.result || "控制器未返回实时状态报告";
        setTelemetry(parseGrblStatusReport("", msg));
      }
    } catch (err) {
      if (!silent) {
        setTelemetry(parseGrblStatusReport("", err?.message || "状态查询异常"));
      }
    }
    return null;
  }, [getConnectionPayload, setConnectionStatus]);

  // 周期只读遥测采样 (每 2 秒一次)
  useEffect(() => {
    if (!pollingTelemetry) return undefined;
    let cancelled = false;
    let timer = null;

    const loop = async () => {
      await sampleControllerStatus(true);
      if (!cancelled) {
        timer = setTimeout(loop, 2000);
      }
    };
    void loop();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [pollingTelemetry, sampleControllerStatus]);

  // 导入本地 G-code 文件用于三维仿真推演（自动与 3D 工作台 1:1 同步尺寸）
  const handleLocalGcodeImport = (e) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const content = String(reader.result || "");
      if (content.trim()) {
        setActiveWorkflow(null);
        setSelectedWorkflowId("");
        setGcodeText(content);
        setGcodeSourceLabel(`本地文件 · ${file.name}`);
        updateMaterialConfig({
          autoSize: true,
          textOffsetX: 0,
          textOffsetY: 0,
        });
        setSimProgress(0);
        setSimPlaying(true);
        setDriveMode("sim");
      }
    };
    reader.readAsText(file);
    e.target.value = "";
  };

  // 在材料内对齐激光雕刻文字/刀路（若当前处于 1:1 同尺寸无余量状态则自动扩展材料外框，且始终钳制在材料内）
  const alignTextInMaterial = useCallback(
    (preset) => {
      let matW = workspace.material.widthMm;
      let matH = workspace.material.heightMm;
      let nextAutoSize = !workspace.material.autoSize ? false : true;
      const minTX = workspace.material.minTextOffsetX || 0;
      const minTY = workspace.material.minTextOffsetY || 0;
      const contentW = Math.max(toolpath.bounds.widthMm || 0, toolpath.bounds.maxX || 0);
      const contentH = Math.max(toolpath.bounds.heightMm || 0, toolpath.bounds.maxY || 0);

      if (
        preset !== "bottom-left" &&
        workspace.material.maxTextOffsetX <= minTX &&
        workspace.material.maxTextOffsetY <= minTY &&
        (contentW < workspace.xTravelMm || contentH < workspace.yTravelMm)
      ) {
        nextAutoSize = false;
        matW = Math.min(
          workspace.xTravelMm - workspace.offsetX,
          Math.max(80, Number(((contentW || 60) + 20).toFixed(1))),
        );
        matH = Math.min(
          workspace.yTravelMm - workspace.offsetY,
          Math.max(80, Number(((contentH || 60) + 20).toFixed(1))),
        );
      }

      const maxTX = Math.max(minTX, Number(Math.max(0, matW - contentW).toFixed(1)));
      const maxTY = Math.max(minTY, Number(Math.max(0, matH - contentH).toFixed(1)));

      let tx = minTX;
      let ty = minTY;
      if (preset === "center") {
        tx = Number(((minTX + maxTX) / 2).toFixed(1));
        ty = Number(((minTY + maxTY) / 2).toFixed(1));
      } else if (preset === "top-left") {
        tx = minTX;
        ty = maxTY;
      } else if (preset === "top-right") {
        tx = maxTX;
        ty = maxTY;
      } else if (preset === "bottom-right") {
        tx = maxTX;
        ty = minTY;
      }

      updateMaterialConfig({
        autoSize: nextAutoSize,
        widthMm: matW,
        heightMm: matH,
        textOffsetX: tx,
        textOffsetY: ty,
      });
    },
    [workspace, toolpath.bounds, updateMaterialConfig],
  );

  // 页面内急停按钮：请求 0x18 软件复位
  const handleTwinEmergencyStop = async () => {
    const payload = {
      ...getConnectionPayload(),
      action: "emergency_stop",
      confirmed: true,
      ...(selectedWorkflowId ? { job_id: selectedWorkflowId } : {}),
    };
    const res = await postJson("/api/safe-action", payload);
    const sent = res.ok && res.data?.success;
    const msg = sent
      ? "0x18 软件复位已发送。请立即在现场核对机器物理状态，软件急停不能替代实体急停按钮。"
      : `${res.data?.result || "急停请求发送失败"}。请立即按下机器上的实体急停开关！`;
    await showAlert(msg, {
      title: sent ? "已发送 0x18 急停" : "急停未确认",
      tone: sent ? "warning" : "error",
    });
  };

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

  return (
    <div className="twin-page ob-page">
      {/* 顶部模式切换与状态横幅 */}
      <div className="hud-card ob-card twin-header-bar">
        <div className="twin-mode-tabs" role="tablist" aria-label="数字孪生信号源模式">
          <button
            type="button"
            role="tab"
            aria-selected={driveMode === "sim"}
            className={`twin-mode-tab ${driveMode === "sim" ? "active" : ""}`}
            onClick={() => setDriveMode("sim")}
          >
            <Play size={14} />
            <div>
              <b>离线刀路推演</b>
              <small>G-CODE SIMULATION</small>
            </div>
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
            <div>
              <b>加工下发同步</b>
              <small>JOB STREAM SYNC</small>
            </div>
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
            <div>
              <b>实机遥测映射</b>
              <small>LIVE TELEMETRY</small>
            </div>
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

                    <span
                      id="twin-loop-status-pill"
                      className={`ob-pill ${simLoop ? "ob-pill--ok" : "ob-pill--off"}`}
                      style={{ fontSize: "11.5px" }}
                      title={simLoop ? "推演到达 100% 后将自动从头循环雕刻" : "推演到达 100% 后自动停止，不循环雕刻"}
                    >
                      <span
                        className="twin-laser-dot"
                        style={{ color: simLoop ? "#34d399" : "#64748b" }}
                      />
                      <span>{simLoop ? "已开启循环雕刻" : "单次推演（不循环）"}</span>
                    </span>

                    <div className="twin-speed-group" aria-label="1:1 真实雕刻时长对应">
                      <span
                        className="ob-pill ob-pill--info"
                        style={{ fontFamily: "var(--ob-mono)", fontSize: "11.5px" }}
                        title="按 G-code 每段进给速度 F 与长度严格 1:1 实时推演，无倍速失真"
                      >
                        <span>
                          1:1 真实雕刻耗时 · 已走 {(activePose.elapsedSeconds || 0).toFixed(1)}s / 总计{" "}
                          {toolpath.estimatedSeconds.toFixed(1)}s（剩余{" "}
                          {(
                            activePose.remainingSeconds ??
                            Math.max(0, toolpath.estimatedSeconds - (activePose.elapsedSeconds || 0))
                          ).toFixed(1)}
                          s）
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
                      LINE {activePose.lineNumber || 0} / {toolpath.totalLines}
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
                      LINE {activePose.lineNumber || 0} / {toolpath.totalLines}
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
            <FoldHeader id="twin-source" title="刀路源与任务同步" k="TOOLPATH" n={1} />
            <div className="twin-card-body">
              <div className="twin-source-current">
                <span className="twin-field-label">当前挂载刀路</span>
                <div className="twin-source-value">{gcodeSourceLabel}</div>
              </div>

              <div className="twin-row-actions">
                <select
                  value={selectedWorkflowId}
                  onChange={(e) => {
                    const wf = recentWorkflows.find((w) => w.workflow_id === e.target.value);
                    if (wf) void loadWorkflowToolpath(wf);
                  }}
                  className="select-input twin-select"
                  aria-label="选择历史或当前工作流任务"
                >
                  <option value="">
                    {recentWorkflows.length > 0 ? "切换工作流任务..." : "暂无工作流记录 (使用演示刀路)"}
                  </option>
                  {recentWorkflows.map((wf) => (
                    <option key={wf.workflow_id} value={wf.workflow_id}>
                      {wf.workflow_id} · {wf.artifacts?.material || wf.material || "激光任务"} ({wf.status})
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

              {/* 材料外框尺寸与摆放位置坐标 (X0, Y0) —— 修改后材料、刀路与激光头实时同步移动 */}
              <div className="twin-pins-box" style={{ marginTop: "6px" }}>
                <div className="twin-telemetry-head" style={{ marginBottom: "8px" }}>
                  <span className="twin-field-label">
                    材料外框尺寸与位置坐标（激光头跟随移动）
                  </span>
                  <span className="ob-pill ob-pill--info">
                    <span>
                      {workspace.material.autoSize ? "1:1 同步 G-code" : "自定义外框"} · 起点 ({workspace.offsetX}, {workspace.offsetY})
                    </span>
                  </span>
                </div>

                <div className="twin-row-actions" style={{ marginBottom: "8px" }}>
                  <button
                    type="button"
                    className={`btn btn-ghost twin-speed-btn ${workspace.material.autoSize ? "active" : ""}`}
                    onClick={() => updateMaterialConfig({ autoSize: true, textOffsetX: 0, textOffsetY: 0 })}
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
                    title="将材料、刀路与激光头一起移动到 100×100mm 台面正中央"
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
                          const contentW = Math.max(toolpath.bounds.widthMm || 0, toolpath.bounds.maxX || 0);
                          const nextTX = Math.max(0, Number(((clampedW - contentW) / 2).toFixed(1)));
                          updateMaterialConfig({
                            autoSize: false,
                            widthMm: clampedW,
                            heightMm: workspace.material.heightMm,
                            textOffsetX: nextTX,
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
                        const contentW = Math.max(toolpath.bounds.widthMm || 0, toolpath.bounds.maxX || 0);
                        const nextTX = Math.max(0, Number(((clampedW - contentW) / 2).toFixed(1)));
                        updateMaterialConfig({
                          autoSize: false,
                          widthMm: clampedW,
                          heightMm: workspace.material.heightMm,
                          textOffsetX: nextTX,
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
                          const contentH = Math.max(toolpath.bounds.heightMm || 0, toolpath.bounds.maxY || 0);
                          const nextTY = Math.max(0, Number(((clampedH - contentH) / 2).toFixed(1)));
                          updateMaterialConfig({
                            autoSize: false,
                            widthMm: workspace.material.widthMm,
                            heightMm: clampedH,
                            textOffsetY: nextTY,
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
                        const contentH = Math.max(toolpath.bounds.heightMm || 0, toolpath.bounds.maxY || 0);
                        const nextTY = Math.max(0, Number(((clampedH - contentH) / 2).toFixed(1)));
                        updateMaterialConfig({
                          autoSize: false,
                          widthMm: workspace.material.widthMm,
                          heightMm: clampedH,
                          textOffsetY: nextTY,
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

              {/* 激光雕刻文字/刀路在材料内部的位置调整 —— 严格钳制在材料四周边距内，绝不超出材料外框 */}
              <div className="twin-pins-box" style={{ marginTop: "6px" }}>
                <div className="twin-telemetry-head" style={{ marginBottom: "8px" }}>
                  <span className="twin-field-label">
                    激光雕刻文字在材料内位置（始终在材料内）
                  </span>
                  <span className="ob-pill ob-pill--ok">
                    <span>
                      材料内偏移 (+{workspace.material.textOffsetX}, +{workspace.material.textOffsetY} mm)
                    </span>
                  </span>
                </div>

                <div className="twin-row-actions" style={{ marginBottom: "8px" }}>
                  <button
                    type="button"
                    className={`btn btn-ghost twin-speed-btn ${workspace.material.textOffsetX === workspace.material.minTextOffsetX && workspace.material.textOffsetY === workspace.material.minTextOffsetY ? "active" : ""}`}
                    onClick={() => alignTextInMaterial("bottom-left")}
                    title="将雕刻文字放置在材料左下角起点"
                  >
                    <span>材料左下</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost twin-speed-btn"
                    onClick={() => alignTextInMaterial("center")}
                    title="将雕刻文字移动到整块材料正中央（始终在材料内部）"
                  >
                    <span>材料内居中</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost twin-speed-btn"
                    onClick={() => alignTextInMaterial("top-left")}
                    title="将雕刻文字贴合到材料内左上角"
                  >
                    <span>材料左上</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost twin-speed-btn"
                    onClick={() => alignTextInMaterial("top-right")}
                    title="将雕刻文字贴合到材料内右上角"
                  >
                    <span>材料右上</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-ghost twin-speed-btn"
                    onClick={() => alignTextInMaterial("bottom-right")}
                    title="将雕刻文字贴合到材料内右下角"
                  >
                    <span>材料右下</span>
                  </button>
                </div>

                <div className="twin-metrics-grid">
                  <label className="twin-metric-box">
                    <span>
                      材料内横向 X (0~{workspace.material.maxTextOffsetX} mm)
                    </span>
                    <input
                      type="number"
                      min={workspace.material.minTextOffsetX}
                      max={workspace.material.maxTextOffsetX}
                      step={0.5}
                      value={workspace.material.textOffsetX}
                      onChange={(e) => updateMaterialConfig({ textOffsetX: e.target.value })}
                      className="text-input"
                      aria-label="雕刻文字在材料内横向位置 X (mm)"
                    />
                    <input
                      type="range"
                      min={workspace.material.minTextOffsetX}
                      max={workspace.material.maxTextOffsetX}
                      step={0.5}
                      value={workspace.material.textOffsetX}
                      onChange={(e) => updateMaterialConfig({ textOffsetX: e.target.value })}
                      className="twin-range-slider"
                      aria-label="拖动调节雕刻文字在材料内横向位置 X"
                    />
                  </label>
                  <label className="twin-metric-box">
                    <span>
                      材料内纵向 Y (0~{workspace.material.maxTextOffsetY} mm)
                    </span>
                    <input
                      type="number"
                      min={workspace.material.minTextOffsetY}
                      max={workspace.material.maxTextOffsetY}
                      step={0.5}
                      value={workspace.material.textOffsetY}
                      onChange={(e) => updateMaterialConfig({ textOffsetY: e.target.value })}
                      className="text-input"
                      aria-label="雕刻文字在材料内纵向位置 Y (mm)"
                    />
                    <input
                      type="range"
                      min={workspace.material.minTextOffsetY}
                      max={workspace.material.maxTextOffsetY}
                      step={0.5}
                      value={workspace.material.textOffsetY}
                      onChange={(e) => updateMaterialConfig({ textOffsetY: e.target.value })}
                      className="twin-range-slider"
                      aria-label="拖动调节雕刻文字在材料内纵向位置 Y"
                    />
                  </label>
                </div>
              </div>
              {driveMode === "job" && (
                <div className="twin-job-sync-box">
                  <div className="twin-job-sync-head">
                    <span>JobWorker 下发状态：{jobStatus.toUpperCase()}</span>
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
            <FoldHeader id="twin-telemetry" title="控制器实时遥测" k="TELEMETRY" n={2} />
            <div className="twin-card-body">
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
                >
                  <Activity size={13} />
                  <span>单次采样状态 (?)</span>
                </button>

                <button
                  type="button"
                  className={`btn ${pollingTelemetry ? "btn-hud-primary" : "btn-ghost"}`}
                  onClick={() => setPollingTelemetry((v) => !v)}
                  id="btn-twin-toggle-poll"
                >
                  <Radio size={13} />
                  <span>{pollingTelemetry ? "停止周期遥测" : "开启 2s 周期遥测"}</span>
                </button>
              </div>

              {/* MPos / WPos 机械与工作坐标对照表 */}
              <div className="twin-pos-table">
                <div className="twin-pos-row twin-pos-row--head">
                  <span>坐标轴</span>
                  <span>工作坐标 (WPos)</span>
                  <span>机械坐标 (MPos)</span>
                </div>
                {["x", "y", "z"].map((axis) => (
                  <div key={axis} className="twin-pos-row">
                    <span className="twin-axis-tag">{axis.toUpperCase()} 轴</span>
                    <b>{telemetry.wpos ? `${telemetry.wpos[axis].toFixed(3)} mm` : "—"}</b>
                    <span>{telemetry.mpos ? `${telemetry.mpos[axis].toFixed(3)} mm` : "—"}</span>
                  </div>
                ))}
              </div>

              {/* 规划器 Bf 与进给功率 FS */}
              <div className="twin-metrics-grid">
                <div className="twin-metric-box">
                  <span>规划器空闲块 (Bf)</span>
                  <b>{telemetry.bf?.plannerFree != null ? `${telemetry.bf.plannerFree} 块` : "—"}</b>
                </div>
                <div className="twin-metric-box">
                  <span>RX 缓冲余量</span>
                  <b>{telemetry.bf?.rxBufferFree != null ? `${telemetry.bf.rxBufferFree} B` : "—"}</b>
                </div>
              </div>

              {/* 引脚状态指示矩阵 (Pn) */}
              <div className="twin-pins-box">
                <span className="twin-field-label">硬件限位与安全引脚 (Pn)</span>
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
                  <div className="twin-pin-ok">
                    <CheckCircle2 size={13} />
                    <span>{telemetry.online ? "无触发引脚 (限位/安全门/急停均正常)" : "待采样控制器报文"}</span>
                  </div>
                )}
              </div>

              {telemetry.raw && (
                <code className="twin-raw-status" title="控制器原始 GRBL 状态报告">
                  {telemetry.raw}
                </code>
              )}
            </div>
          </section>

          {/* 卡片 3：机台物理包络与安全制动 */}
          <section className="hud-card ob-card twin-side-card">
            <FoldHeader id="twin-envelope" title="翼宿 V1.0 工作区与安全门禁" k="YISU V1.0" n={3} />
            <div className="twin-card-body">
              <div className="twin-row-actions">
                <button
                  type="button"
                  className={`btn btn-ghost twin-speed-btn ${workspace.xTravelMm === 100 && workspace.yTravelMm === 100 ? "active" : ""}`}
                  onClick={() => updateWorkspaceLimits({ x_travel_mm: 100, y_travel_mm: 100, from_hardware: false })}
                  title="翼宿 V1.0 标称满行程 100×100 mm"
                >
                  <span>100×100 满行程</span>
                </button>
                <button
                  type="button"
                  className={`btn btn-ghost twin-speed-btn ${workspace.xTravelMm === 90 && workspace.yTravelMm === 90 ? "active" : ""}`}
                  onClick={() => updateWorkspaceLimits({ x_travel_mm: 90, y_travel_mm: 90, from_hardware: false })}
                  title="四周预留 5mm 安全边距 (90×90 mm 安全雕刻区)"
                >
                  <span>90×90 安全区</span>
                </button>
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={handleSyncMachineCapabilities}
                  disabled={probingDevice}
                  id="btn-twin-sync-limits"
                  title="从 FluidNC $CD 或 GRBL $$ 读取实机行程配置"
                >
                  <RefreshCw size={13} />
                  <span>{probingDevice ? "同步中..." : "同步 $CD"}</span>
                </button>
              </div>

              {/* 雕刻机可调范围微调 (20 ~ 100 mm) */}
              <div className="twin-metrics-grid">
                <label className="twin-metric-box">
                  <span>X 轴可调行程 (≤100mm)</span>
                  <input
                    type="number"
                    min={20}
                    max={100}
                    step={1}
                    value={workspace.xTravelMm}
                    onChange={(e) => updateWorkspaceLimits({ x_travel_mm: e.target.value, from_hardware: false })}
                    className="text-input"
                    aria-label="X 轴可调行程 (mm)"
                  />
                </label>
                <label className="twin-metric-box">
                  <span>Y 轴可调行程 (≤100mm)</span>
                  <input
                    type="number"
                    min={20}
                    max={100}
                    step={1}
                    value={workspace.yTravelMm}
                    onChange={(e) => updateWorkspaceLimits({ y_travel_mm: e.target.value, from_hardware: false })}
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
                <div className="twin-metric-box">
                  <span>行程来源</span>
                  <b>{workspace.hasHardwareLimits ? "实机 $CD / GRBL" : "翼宿 V1.0 基线"}</b>
                </div>
                <div className="twin-metric-box">
                  <span>激光头与打印件</span>
                  <b>{YISU_V1_PROFILE.laserSpec}</b>
                </div>
                <div className="twin-metric-box">
                  <span>导轨与排烟系统</span>
                  <b>3×MGN9H · 4028 风扇</b>
                </div>
              </div>
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
                title="向控制器发送 0x18 软件复位；紧急情况请直接按实体急停开关"
              >
                <AlertTriangle size={16} />
                <span>0x18 软件复位 · 紧急停止请求</span>
              </button>
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
