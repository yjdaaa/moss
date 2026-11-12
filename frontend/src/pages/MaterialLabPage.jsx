import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import React, { useState, useEffect, useRef, useCallback } from "react";
import {
  Search,
  Plus,
  Bookmark,
  Download,
  Upload,
  Save,
  Grid,
  Send,
  Settings2,
  Trash2,
  RefreshCw,
  ArrowUpRight,
} from "lucide-react";
import { apiFetch, getJson, postJson } from "../api/client.js";
import { isRemoteMode } from "../authSession.js";
import { MaterialModal } from "../components/MaterialModal.jsx";
import { showAlert, showChoice, showConfirm, showPrompt } from "../appDialog.js";
import { defaultSerialPortHint } from "../platform.js";
import { exportFile, savedMessage } from "../saveFile.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { estopTargetArgs } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { MATERIAL_LIBRARY_CHANNEL, notifyMaterialsUpdated } from "../materialSelectState.js";
import {
  MATRIX_MODE_OPTIONS,
  resolveMaterialLabSelection,
  resolveCellEditorValues,
  resolveMatrixModeParams,
  matrixInputSignature,
  isMatrixStale,
  buildMatrixSendRequest,
  buildMatrixConfirmPrompt,
  unwrapImportedMaterialLibrary,
} from "../materialLabState.js";

function readLocalSafe(key, fallback = "") {
  try {
    const val = globalThis.localStorage?.getItem(key);
    return val == null ? fallback : val;
  } catch {
    return fallback;
  }
}

function writeLocalSafe(key, value) {
  try {
    globalThis.localStorage?.setItem(key, String(value ?? ""));
  } catch {
    // 存储被禁用时静默跳过
  }
}

export function MaterialLabPage({ onNavigate }) {
  const [, setBootstrapData] = useState(null);
  const [materials, setMaterials] = useState({});
  const [sourcePath, setSourcePath] = useState("");
  const [searchQuery, setSearchQuery] = useState("");

  // Modal State
  const [showMaterialModal, setShowMaterialModal] = useState(false);
  const [materialModalData, setMaterialModalData] = useState(null);

  // Selected Material & Thickness & Mode
  const [selectedMaterial, setSelectedMaterial] = useState(() => readLocalSafe("laser-forge-mat", "椴木") || "椴木");
  const [selectedThickness, setSelectedThickness] = useState(() => readLocalSafe("laser-forge-thick", "3") || "3");
  const [activeMode, setActiveMode] = useState("raster"); // raster, outline, cut
  const [hasSavedCell, setHasSavedCell] = useState(true);

  const selectedMaterialRef = useRef(selectedMaterial);
  const selectedThicknessRef = useRef(selectedThickness);
  const activeModeRef = useRef(activeMode);
  selectedMaterialRef.current = selectedMaterial;
  selectedThicknessRef.current = selectedThickness;
  activeModeRef.current = activeMode;

  // Parameter editor state
  const [minPower, setMinPower] = useState(0);
  const [maxPower, setMaxPower] = useState(420);
  const [feedRate, setFeedRate] = useState(1800);
  const [travelRate, setTravelRate] = useState(3000);
  const [passes, setPasses] = useState(1);
  const [pixelSize, setPixelSize] = useState(0.1);
  const [threshold, setThreshold] = useState(-1);
  const [notes, setNotes] = useState("");
  const [aliases, setAliases] = useState("");
  const [editorStatus, setEditorStatus] = useState("");

  // Calibration Matrix & Device Connection State（与工作台共享连接配置）
  const [matrixMode, setMatrixMode] = useState("raster");
  const [matrixRows, setMatrixRows] = useState(5);
  const [matrixCols, setMatrixCols] = useState(5);
  const [matrixPowerMin, setMatrixPowerMin] = useState(100);
  const [matrixPowerMax, setMatrixPowerMax] = useState(600);
  const [matrixSpeedMin, setMatrixSpeedMin] = useState(500);
  const [matrixSpeedMax, setMatrixSpeedMax] = useState(2500);
  const [connectionMode, setConnectionMode] = useState(() => readLocalSafe("laser-forge-conn-mode", "network") || "network");
  const [matrixHost, setMatrixHost] = useState(() => readLocalSafe("laser-forge-host", ""));
  const [telnetPort, setTelnetPort] = useState(() => readLocalSafe("laser-forge-telnet-port", "23") || "23");
  const [serialPort, setSerialPort] = useState(() => readLocalSafe("laser-forge-serial-port", ""));
  const [baudrate, setBaudrate] = useState(() => readLocalSafe("laser-forge-baud", "115200") || "115200");
  const [serialPorts, setSerialPorts] = useState([]);
  const [matrixPreview, setMatrixPreview] = useState(null);
  const [matrixSignature, setMatrixSignature] = useState("");
  const [matrixId, setMatrixId] = useState("");
  const [selectedCellNum, setSelectedCellNum] = useState(null);
  const [matrixStatus, setMatrixStatus] = useState("");
  const [matrixBusy, setMatrixBusy] = useState(false);
  const [matrixSendState, setMatrixSendState] = useState(null); // {status, lines_sent, total_lines, error}
  const matrixPollRef = useRef(null);

  const currentMatrixSig = matrixInputSignature({
    material: selectedMaterial,
    thickness: selectedThickness,
    mode: matrixMode,
    rows: matrixRows,
    cols: matrixCols,
    powerMin: matrixPowerMin,
    powerMax: matrixPowerMax,
    speedMin: matrixSpeedMin,
    speedMax: matrixSpeedMax,
  });
  const matrixStale = Boolean(matrixId) && isMatrixStale(matrixSignature, currentMatrixSig);

  const applyEditorState = useCallback((allMats, matName, thickKey, mode) => {
    const resolved = resolveCellEditorValues(allMats, matName, thickKey, mode);
    setMinPower(resolved.minPower);
    setMaxPower(resolved.maxPower);
    setFeedRate(resolved.feedRate);
    setTravelRate(resolved.travelRate);
    setPasses(resolved.passes);
    setPixelSize(resolved.pixelSize);
    setThreshold(resolved.threshold);
    setNotes(resolved.notes);
    setAliases(resolved.aliases);
    setHasSavedCell(resolved.hasSavedCell);
  }, []);

  const loadBootstrap = useCallback(async (preferred = {}) => {
    const res = await getJson("/v1/material-lab/bootstrap");
    if (res.ok && res.data?.success) {
      const result = res.data.result || {};
      setBootstrapData(result);
      setSourcePath(result.source_path || "");
      const mats = result.materials?.materials || {};
      setMaterials(mats);

      const targetMat = preferred.material ?? selectedMaterialRef.current;
      const targetThick = preferred.thickness ?? selectedThicknessRef.current;
      const targetMode = preferred.mode ?? activeModeRef.current;
      const sel = resolveMaterialLabSelection(mats, targetMat, targetThick);
      setSelectedMaterial(sel.material);
      setSelectedThickness(sel.thickness);
      applyEditorState(mats, sel.material, sel.thickness, targetMode);
    }
  }, [applyEditorState]);

  const loadSerialPorts = useCallback(async () => {
    try {
      const res = await getJson("/api/ui/serial-ports");
      if (res.ok && res.data?.success) {
        const ports = Array.isArray(res.data.result?.ports) ? res.data.result.ports : [];
        setSerialPorts(ports);
        setSerialPort((prev) => (ports.length > 0 && (!prev || !ports.includes(prev)) ? ports[0] : prev));
      }
    } catch {
      // 忽略串口列表读取异常
    }
  }, []);

  useEffect(() => {
    void loadBootstrap();
    void loadSerialPorts();
    let channel = null;
    try {
      if (typeof BroadcastChannel === "function") {
        channel = new BroadcastChannel(MATERIAL_LIBRARY_CHANNEL);
        channel.addEventListener("message", (event) => {
          if (event?.data?.type === "materials-updated") {
            void loadBootstrap();
          }
        });
      }
    } catch {
      channel = null;
    }
    return () => {
      matrixPollRef.current?.();
      channel?.close?.();
    };
  }, [loadBootstrap, loadSerialPorts]);

  // 顶栏常驻急停兜底：在途发送先按 calibration_id 停，若刚发完最后一行仍在缓冲执行则按本页设备目标补发 0x18
  const isMatrixRunning = matrixSendState?.status === "sending" || matrixSendState?.status === "running";
  usePageEmergencyStop(
    () => ({
      jobId: isMatrixRunning ? matrixId : "",
      target: estopTargetArgs({
        connection_mode: connectionMode,
        port: serialPort,
        baudrate,
        network_host: matrixHost,
        network_telnet_port: telnetPort,
      }),
    }),
    (outcome) => {
      matrixPollRef.current?.();
      matrixPollRef.current = null;
      const msg = outcome.kind === "sent"
        ? `${outcome.message || "急停 0x18 已发送"}。软件急停不代表物理停机，请现场核实机器状态；必要时按实体急停。`
        : `${outcome.message || "急停发送失败"}。请立即按控制器实体急停按钮！`;
      setMatrixStatus(msg);
      void showAlert(msg, {
        title: outcome.kind === "sent" ? "已发送急停 0x18" : "急停未确认",
        tone: outcome.kind === "sent" ? "warning" : "error",
      });
    },
  );

  // 手机连接电脑 MOSS 时材料与校准结果只读（后端同样拒绝）：修改类按钮不显示，参数只能查看
  const readOnly = isRemoteMode();

  const handleSelectMaterialAndThick = (matName, thickKey) => {
    setSelectedMaterial(matName);
    setSelectedThickness(thickKey);
    writeLocalSafe("laser-forge-mat", matName);
    writeLocalSafe("laser-forge-thick", thickKey);
    applyEditorState(materials, matName, thickKey, activeMode);
  };

  const handleModeChange = (mode) => {
    setActiveMode(mode);
    setMatrixMode(mode);
    applyEditorState(materials, selectedMaterial, selectedThickness, mode);
  };

  const handleApplyToWorkbench = () => {
    if (selectedMaterial) {
      writeLocalSafe("laser-forge-mat", selectedMaterial);
    }
    if (selectedThickness) {
      writeLocalSafe("laser-forge-thick", selectedThickness);
    }
    onNavigate?.("/");
  };

  // Save current parameter cell
  const handleSaveParams = async () => {
    const trimmedMat = String(selectedMaterial || "").trim();
    const thickNum = Number(selectedThickness);
    if (!trimmedMat) {
      setEditorStatus("保存失败: 请先新建或选择材料");
      return;
    }
    if (!Number.isFinite(thickNum) || thickNum <= 0) {
      setEditorStatus("保存失败: 厚度必须大于 0");
      return;
    }

    setEditorStatus("正在保存...");
    const payload = {
      action: "save",
      material: trimmedMat,
      thickness_mm: thickNum,
      laser_mode: activeMode === "cut" ? "cut" : "engrave",
      engraving_mode: activeMode === "cut" ? "" : activeMode,
      laser_min_power: Number(minPower),
      laser_max_power: Number(maxPower),
      feed_rate: Number(feedRate),
      travel_rate: Number(travelRate),
      passes: Number(passes),
      pixel_size_mm: Number(pixelSize),
      threshold: Number(threshold),
      notes: notes.trim(),
      aliases: aliases.split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean),
    };

    try {
      const res = await postJson("/api/material-lab/manage", payload);
      if (res.ok && res.data?.success) {
        setEditorStatus("参数已成功保存！");
        setTimeout(() => setEditorStatus(""), 3000);
        notifyMaterialsUpdated();
        await loadBootstrap({ material: trimmedMat, thickness: String(thickNum), mode: activeMode });
      } else {
        setEditorStatus(`保存失败: ${res.data?.result || "未知错误"}`);
      }
    } catch (e) {
      setEditorStatus(`保存异常: ${e.message}`);
    }
  };

  // Delete current thickness (only when material has multiple thicknesses)
  const handleDeleteCurrentThickness = async () => {
    if (!selectedMaterial || !selectedThickness) return;
    const confirmed = await showConfirm(
      `确定删除「${selectedMaterial}」的 ${selectedThickness} mm 厚度规格及该厚度下的全部参数吗？`,
      {
        title: "删除当前厚度",
        tone: "warning",
        danger: true,
        confirmText: "删除厚度",
      },
    );
    if (!confirmed) return;

    setEditorStatus("正在删除厚度...");
    try {
      const res = await postJson("/api/material-lab/manage", {
        action: "delete",
        material: selectedMaterial,
        thickness_mm: Number(selectedThickness),
        confirmed: true,
      });
      if (res.ok && res.data?.success) {
        setEditorStatus(`已移除 ${selectedThickness} mm 厚度`);
        setTimeout(() => setEditorStatus(""), 3000);
        notifyMaterialsUpdated();
        await loadBootstrap({ material: selectedMaterial, mode: activeMode });
      } else {
        setEditorStatus(`删除失败: ${res.data?.result || "未知错误"}`);
      }
    } catch (e) {
      setEditorStatus(`删除异常: ${e.message}`);
    }
  };

  // Save from Recent workflow
  const handleSaveRecent = async () => {
    setEditorStatus("正在拉取最近加工参数...");
    try {
      const res = await postJson("/api/material-lab/recent", {});
      if (res.ok && res.data?.success) {
        const p = res.data.result;
        const nextMat = p.material || selectedMaterial;
        const nextThick = String(p.thickness_mm || selectedThickness);
        const mode = p.laser_mode === "cut" ? "cut" : p.engraving_mode || "raster";
        setSelectedMaterial(nextMat);
        setSelectedThickness(nextThick);
        setActiveMode(mode);
        setMatrixMode(mode);
        const existingAliases = materials[nextMat]?.aliases;
        setAliases(Array.isArray(existingAliases) ? existingAliases.join(", ") : "");
        setNotes("");
        setHasSavedCell(false);
        if (p.params) {
          setMinPower(p.params.laser_min_power ?? 0);
          setMaxPower(p.params.laser_max_power ?? 400);
          setFeedRate(p.params.feed_rate ?? 1200);
          setTravelRate(p.params.travel_rate ?? 3000);
          setPasses(p.params.passes ?? 1);
          setPixelSize(p.params.pixel_size_mm ?? 0.1);
          setThreshold(p.params.threshold ?? -1);
        }
        setEditorStatus("已提取最近工作流真实参数（点下方保存即可入库）");
        setTimeout(() => setEditorStatus(""), 4000);
      } else {
        setEditorStatus(res.data?.result || "最近无可用参数");
      }
    } catch (e) {
      setEditorStatus(`套用异常: ${e.message}`);
    }
  };

  // Export JSON
  const handleExport = async () => {
    try {
      const res = await getJson("/api/material-lab/export");
      if (res.ok && res.data?.success) {
        const blob = new Blob([JSON.stringify(res.data.result?.library || {}, null, 2)], { type: "application/json" });
        const saved = await exportFile(blob, "lasergrbl_materials.json", "材料库 ");
        // 桌面端由 exportFile 弹出带“打开所在文件夹”的完成提示；浏览器里由浏览器下载提示
        if (!saved.path && !saved.cancelled) {
          showAlert("材料库已导出（见浏览器下载）", { title: "导出完成", tone: "success" });
        }
      } else {
        showAlert(res.data?.result || "读取材料库失败", { title: "导出失败", tone: "error" });
      }
    } catch (e) {
      showAlert(e.message, { title: "导出异常", tone: "error" });
    }
  };

  // Import JSON（清空 input 允许重复选同文件，支持合并导入或确认后整库替换）
  const handleImportFile = async (e) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    const reader = new FileReader();
    reader.onload = async () => {
      try {
        const rawJson = JSON.parse(String(reader.result || ""));
        const unwrapped = unwrapImportedMaterialLibrary(rawJson);
        if (unwrapped.error) {
          showAlert(unwrapped.error, { title: "导入失败", tone: "error" });
          return;
        }
        const choice = await showChoice(
          `文件「${file.name}」共包含 ${unwrapped.materialCount} 种材料。\n\n• 合并到现有库：新增或更新同名参数，保留当前库中其他材料（推荐）\n• 替换整个库：清空当前材料库，仅保留导入文件中的材料`,
          {
            title: "选择材料库导入方式",
            tone: "info",
            confirmText: "合并到现有库（推荐）",
            cancelText: "替换整个库",
          },
        );
        if (choice === "dismiss") return;

        let importMode = "merge";
        if (choice === "cancel") {
          const confirmedReplace = await showConfirm(
            `确定用「${file.name}」替换整个材料库吗？当前库中未包含在该文件里的自定义材料将被清空。`,
            {
              title: "确认替换整个材料库",
              tone: "warning",
              danger: true,
              confirmText: "确认替换",
              cancelText: "取消",
            },
          );
          if (!confirmedReplace) return;
          importMode = "replace";
        }

        const res = await postJson("/api/material-lab/manage", {
          action: "import",
          import_mode: importMode,
          library_json: unwrapped.library,
        });
        if (res.ok && res.data?.success) {
          notifyMaterialsUpdated();
          await loadBootstrap();
          showAlert(
            importMode === "merge"
              ? `材料库已成功合并导入（共合入 ${unwrapped.materialCount} 种材料）。`
              : "材料库已整库替换并刷新。",
            { title: "导入成功", tone: "success" },
          );
        } else {
          showAlert(res.data?.result || "参数或材料安全校验未通过", { title: "导入失败", tone: "error" });
        }
      } catch (err) {
        showAlert(err.message, { title: "导入解析异常", tone: "error" });
      }
    };
    reader.onerror = () => {
      showAlert("读取本地 JSON 文件失败", { title: "导入失败", tone: "error" });
    };
    reader.readAsText(file);
  };

  // Generate Matrix Preview
  const handleGenerateMatrix = async () => {
    if (!selectedMaterial) {
      showAlert("请先选择或新建一种材料", { title: "缺少材料", tone: "warning" });
      return;
    }
    const modeInfo = resolveMatrixModeParams(matrixMode);
    const sigSnapshot = currentMatrixSig;
    setMatrixBusy(true);
    setMatrixStatus(`正在生成「${modeInfo.label}」标定矩阵 G-code 与预览...`);
    const payload = {
      material: selectedMaterial,
      thickness_mm: Number(selectedThickness) || 3,
      laser_mode: modeInfo.laser_mode,
      engraving_mode: modeInfo.engraving_mode,
      matrix_kind: modeInfo.matrix_kind,
      rows: Number(matrixRows),
      columns: Number(matrixCols),
      power_min: Number(matrixPowerMin),
      power_max: Number(matrixPowerMax),
      speed_min: Number(matrixSpeedMin),
      speed_max: Number(matrixSpeedMax),
    };

    try {
      const res = await postJson("/api/material-lab/preview", payload);
      setMatrixBusy(false);
      if (res.ok && res.data?.success && res.data.result?.calibration_id) {
        const resData = res.data.result;
        setMatrixId(resData.calibration_id);
        setMatrixPreview(resData);
        setMatrixSignature(sigSnapshot);
        setSelectedCellNum(null);
        setMatrixSendState(null);
        const est = resData.time_estimate?.estimated_minutes;
        const sizeStr = resData.width_mm && resData.height_mm
          ? `，外框 ${Number(resData.width_mm).toFixed(0)}×${Number(resData.height_mm).toFixed(0)} mm`
          : "";
        setMatrixStatus(
          `${modeInfo.label}矩阵已生成（${resData.cell_count || 0} 格${sizeStr}${typeof est === "number" ? `，约 ${est.toFixed(1)} 分钟` : ""}）；发送实机测试后点选最佳格`,
        );
      } else if (res.ok && res.data?.success) {
        setMatrixStatus("生成失败: 后端未返回 calibration_id，请更新 moss-host");
      } else {
        setMatrixStatus(`生成失败: ${res.data?.result || "校验未通过"}`);
      }
    } catch (e) {
      setMatrixBusy(false);
      setMatrixStatus(`矩阵生成网络异常: ${e.message}`);
    }
  };

  // Export generated calibration matrix G-code
  const handleExportMatrixGcode = async () => {
    const gcodeFile = matrixPreview?.gcode_file || matrixPreview?.artifacts?.gcode_file;
    const url = matrixPreview?.web?.gcode_download_url || (gcodeFile ? `/preview?path=${encodeURIComponent(gcodeFile)}&download=1` : "");
    if (!url) return;
    try {
      const response = await apiFetch(url);
      if (!response.ok) {
        throw new Error(`读取标定刀路失败（HTTP ${response.status}）`);
      }
      const blob = await response.blob();
      const saved = await exportFile(blob, gcodeFile || `${matrixId || "calibration"}.gcode`, "标定矩阵 G-code ");
      setMatrixStatus(savedMessage("标定矩阵 G-code ", saved));
    } catch (e) {
      showAlert(e.message, { title: "导出标定 G-code 失败", tone: "error" });
    }
  };

  // Send Matrix to Controller
  const handleSendMatrix = async () => {
    if (matrixStale) {
      showAlert("矩阵参数或目标材料已修改，请先点击【生成矩阵】更新刀路后再发送。", {
        title: "标定矩阵已过期",
        tone: "warning",
      });
      return;
    }
    const built = buildMatrixSendRequest({
      calibrationId: matrixId,
      connectionMode,
      networkHost: matrixHost,
      telnetPort,
      serialPort,
      baudrate,
    });
    if (built.error) {
      showAlert(built.error, { title: "缺少发送条件", tone: "warning" });
      return;
    }

    const prompt = buildMatrixConfirmPrompt({
      preview: matrixPreview,
      material: selectedMaterial,
      thickness: selectedThickness,
      mode: matrixMode,
      targetLabel: built.targetLabel,
    });
    const confirmed = await showConfirm(prompt.message, {
      title: prompt.title,
      tone: "warning",
      confirmText: prompt.confirmText,
      cancelText: "取消",
    });
    if (!confirmed) return;

    setMatrixBusy(true);
    setMatrixStatus(`正在握手并向 ${built.targetLabel} 下发标定矩阵...`);
    try {
      const res = await postJson("/api/material-lab/send", built.payload);
      setMatrixBusy(false);
      if (res.status === 202 || res.data?.result?.status === "approval_required") {
        setMatrixStatus("已提交桌面严格模式审批，请在电脑端审批卡确认后开始出光测试。");
        return;
      }
      const sendResult = res.data?.result?.send_result;
      if (res.ok && res.data?.success && sendResult?.running) {
        setMatrixSendState({ status: "sending", lines_sent: 0, total_lines: sendResult.total_lines || 0 });
        setMatrixStatus(`握手通过，正在逐行下发 (0/${sendResult.total_lines || 0} 行)…`);
        startMatrixPolling(matrixId);
      } else {
        setMatrixStatus(`发送失败: ${res.data?.result || "门禁拦截"}`);
      }
    } catch (e) {
      setMatrixBusy(false);
      setMatrixStatus(`发送网络异常: ${e.message}`);
    }
  };

  // One bounded request at a time; loss of contact is not a completed/failed job.
  const startMatrixPolling = (calibrationId) => {
    matrixPollRef.current?.();
    let state = { id: calibrationId, kind: "calibration", status: "sending", progress: null };
    const stop = startJobPolling(
      (signal) => postJson("/api/material-lab/status", { calibration_id: calibrationId }, { signal }),
      (res) => {
        state = applyJobPoll(state, calibrationId, "calibration", res);
        if (state.connectionLost) {
          setMatrixStatus("状态未知：暂时无法查询设备，正在重试；机器可能仍在运行，请到现场确认。");
          return;
        }
        const sr = res.data.result?.send_result || {};
        setMatrixSendState((previous) => ({
          ...previous,
          status: state.status,
          lines_sent: state.progress?.sent ?? previous?.lines_sent ?? 0,
          total_lines: state.progress?.total ?? previous?.total_lines ?? 0,
          error: sr.error,
        }));
        if (["completed", "failed", "cancelled"].includes(state.status)) {
          stop();
          matrixPollRef.current = null;
          setMatrixStatus(
            state.status === "completed"
              ? `标定矩阵加工完成 (${sr.lines_sent || 0}/${sr.total_lines || 0} 行已确认)，请观察样板并点选最佳单元格`
              : `标定矩阵记录为${state.status === "cancelled" ? "已取消" : "发送失败"}，请确认设备状态${sr.error ? `: ${sr.error}` : ""}`,
          );
        } else {
          setMatrixStatus(`正在逐行下发 (${state.progress?.sent || 0}/${state.progress?.total || 0} 行)…`);
        }
      },
    );
    matrixPollRef.current = stop;
  };

  // Select Best Cell and Save（按生成该矩阵时绑定的材料、厚度与模式精确写回）
  const handleSaveBestCell = async (cellIndex) => {
    if (!matrixId) return;
    const cellNumber = cellIndex + 1;
    const targetMat = matrixPreview?.material || selectedMaterial;
    const targetThick = Number(matrixPreview?.thickness_mm) || Number(selectedThickness);
    const targetLaserMode = matrixPreview?.laser_mode || (matrixMode === "cut" ? "cut" : "engrave");
    const targetEngravingMode =
      targetLaserMode === "cut"
        ? ""
        : matrixPreview?.engraving_mode || (matrixMode === "outline" ? "outline" : "raster");
    const targetUiMode = targetLaserMode === "cut" ? "cut" : targetEngravingMode || "raster";

    setSelectedCellNum(cellNumber);
    setMatrixStatus(`正在保存最佳单元格 #${cellNumber}...`);

    try {
      const res = await postJson("/api/material-lab/select-cell", {
        calibration_id: matrixId,
        cell_number: cellNumber,
        material: targetMat,
        thickness_mm: targetThick,
        laser_mode: targetLaserMode,
        engraving_mode: targetEngravingMode,
      });
      if (res.ok && res.data?.success) {
        setMatrixStatus(`已将单元格 #${cellNumber} 保存为「${targetMat} ${targetThick}mm」推荐参数！`);
        setActiveMode(targetUiMode);
        notifyMaterialsUpdated();
        await loadBootstrap({
          material: targetMat,
          thickness: String(targetThick),
          mode: targetUiMode,
        });
      } else {
        setMatrixStatus(`保存单元格失败: ${res.data?.result || "错误"}`);
      }
    } catch (e) {
      setMatrixStatus(`保存单元格网络异常: ${e.message}`);
    }
  };

  // Filter materials（同时支持搜材料名和别名）
  const needle = searchQuery.trim().toLowerCase();
  const filteredMaterials = Object.keys(materials).filter((name) => {
    if (!needle) return true;
    if (name.toLowerCase().includes(needle)) return true;
    const aliasList = materials[name]?.aliases;
    return Array.isArray(aliasList) && aliasList.some((a) => String(a).toLowerCase().includes(needle));
  });

  const currentMatThicknesses = Object.keys(materials[selectedMaterial]?.thicknesses || {});
  const matrixModeInfo = resolveMatrixModeParams(matrixMode);

  return (
    <div className="material-lab-page ob-page">
      {readOnly && (
        <div role="note" className="lab-box">
          已连接电脑 MOSS：材料只能查看和用于加工，修改请在电脑上进行。
        </div>
      )}
      {/* 工具条：页面标题已在顶栏；这里保留存储源与库操作 */}
      <div className="ob-toolbar">
        <div className="ob-toolbar-info">
          {sourcePath ? (
            <>
              当前存储源: <span className="mono">{sourcePath}</span>
            </>
          ) : (
            <span>材料库 · 校准矩阵</span>
          )}
        </div>

        <div className="lab-row">
          {onNavigate && selectedMaterial && (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={handleApplyToWorkbench}
              title="带着当前选中的材质与厚度前往雕刻工作台"
            >
              <ArrowUpRight size={13} className="lab-icon" />
              去工作台使用
            </button>
          )}
          {!readOnly && (
            <button type="button" className="btn btn-ghost" onClick={handleSaveRecent} title="从最近的工作流提取真实加工参数">
              <Bookmark size={13} className="lab-icon" />
              套用最近
            </button>
          )}
          <button type="button" className="btn btn-ghost" onClick={handleExport} title="导出材料库 JSON">
            <Download size={13} className="lab-icon" />
            导出库
          </button>
          {!readOnly && (
            <label className="btn btn-ghost lab-label" title="导入外部材料库（支持合并或整库替换）">
              <Upload size={13} className="lab-icon" />
              导入
              <input type="file" accept=".json,application/json" onChange={handleImportFile} className="lab-field" />
            </label>
          )}
        </div>
      </div>

      {/* 三栏：材料库表格、材料参数、校准矩阵（技术书 §9.5.5 B） */}
      <div className="ob-lab">
        {/* Left Column: Material Tree */}
        <div className="hud-card ob-card lab-row-2">
          <FoldHeader id="lab-library" title="材料库" k="LIBRARY" n={1} />
          <div className="lab-row-3">
            <div className="lab-box-2">
              <Search size={14} className="lab-icon-2" />
              <input
                type="text"
                placeholder="搜索材料名或别名..."
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="text-input lab-field-2"
              />
            </div>
            {!readOnly && (
              <button
                type="button"
                className="btn btn-hud-primary lab-btn"
                onClick={() => {
                  setMaterialModalData(null);
                  setShowMaterialModal(true);
                }}
                title="添加新材料到材料库"
              >
                <Plus size={13} />
                <span>新建</span>
              </button>
            )}
          </div>

          <div className="ob-tbl ob-mtbl">
            <table>
              <thead>
                <tr>
                  <th>材料</th>
                  <th>厚度</th>
                </tr>
              </thead>
              <tbody>
                {filteredMaterials.length === 0 ? (
                  <tr>
                    <td colSpan={2} className="lab-box-7">
                      {Object.keys(materials).length === 0 ? "当前材料库为空，可点击右上角新建或导入" : "无匹配材料"}
                    </td>
                  </tr>
                ) : (
                  filteredMaterials.map((matName) => {
                    const matObj = materials[matName];
                    const thickKeys = Object.keys(matObj?.thicknesses || {});
                    const isSelected = selectedMaterial === matName;
                    return (
                      <tr
                        key={matName}
                        className={isSelected ? "on" : ""}
                        onClick={() => handleSelectMaterialAndThick(matName, thickKeys[0] || "3")}
                        aria-selected={isSelected}
                      >
                        <td>
                          <b>{matName}</b>
                          <div className="ob-sub">{thickKeys.length} 档厚度</div>
                        </td>
                        <td>
                          <div className="ob-chips">
                            {thickKeys.map((th) => (
                              <button
                                type="button"
                                key={th}
                                className={`ob-chip${isSelected && selectedThickness === th ? " on" : ""}`}
                                onClick={(event) => {
                                  event.stopPropagation();
                                  handleSelectMaterialAndThick(matName, th);
                                }}
                              >
                                {th} mm
                              </button>
                            ))}
                          </div>
                        </td>
                      </tr>
                    );
                  })
                )}
              </tbody>
            </table>
          </div>
        </div>

        {/* Middle Column: Parameter Editor */}
        <div className="hud-card ob-card lab-row-4">
          <FoldHeader id="lab-params" title="材料参数" k="PARAMS" n={2} />
          <div className="lab-row-5">
            <h3 className="lab-heading">
              <span>{selectedMaterial ? `${selectedMaterial} · ${selectedThickness} mm` : "未选择材料"}</span>
              <small className="lab-heading-tag">
                {readOnly ? "只读参数" : hasSavedCell ? "已存参数" : "该模式未存（初始值）"}
              </small>
            </h3>
            <div className="lab-row-6">
              {editorStatus && (
                <span
                  className={`lab-status-pill ${
                    editorStatus.includes("失败") || editorStatus.includes("异常") ? "is-error" : "is-ok"
                  }`}
                >
                  {editorStatus}
                </span>
              )}
              {!readOnly && selectedMaterial && currentMatThicknesses.length > 1 && (
                <button
                  type="button"
                  className="btn btn-ghost lab-btn-2"
                  onClick={handleDeleteCurrentThickness}
                  title={`仅删除 ${selectedThickness} mm 厚度档位`}
                >
                  <Trash2 size={12} />
                  <span>删此厚度</span>
                </button>
              )}
              {!readOnly && selectedMaterial && (
                <button
                  type="button"
                  className="btn btn-ghost lab-btn-2"
                  onClick={() => {
                    const curMat = materials[selectedMaterial];
                    const thicks = curMat?.thicknesses ? Object.keys(curMat.thicknesses) : [selectedThickness];
                    setMaterialModalData({
                      name: selectedMaterial,
                      thicknesses: thicks,
                      power_percent: Math.max(1, Math.min(100, Math.round(maxPower / 10))),
                      speed_mms: Math.max(1, Math.round(feedRate / 60)),
                      feed_rate: feedRate,
                      passes,
                      laser_mode: activeMode === "cut" ? "cut" : "engrave",
                      engraving_mode: activeMode === "cut" ? "cut" : activeMode,
                      existing_thicknesses: curMat?.thicknesses,
                      target_thickness: selectedThickness,
                    });
                    setShowMaterialModal(true);
                  }}
                  title="增删厚度规格或删除整个材料"
                >
                  <Settings2 size={12} />
                  <span>管理材料</span>
                </button>
              )}
            </div>
          </div>

          {/* Mode Tabs */}
          <div className="lab-row-7" role="tablist" aria-label="加工模式">
            {MATRIX_MODE_OPTIONS.map((m) => (
              <button
                type="button"
                key={m.id}
                role="tab"
                aria-selected={activeMode === m.id}
                onClick={() => handleModeChange(m.id)}
                className={`lab-mode-tab ${activeMode === m.id ? "active" : ""}`}
              >
                <span>{m.label}</span>
                <small>{m.code}</small>
              </button>
            ))}
          </div>

          {/* Form Fields */}
          <div className="lab-grid">
            <div>
              <label className="lab-label-2">
                最小激光功率 (0-1000)
              </label>
              <input
                type="number"
                min={0}
                max={1000}
                value={minPower}
                onChange={(e) => setMinPower(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div>
              <label className="lab-label-2">
                最大激光功率 (0-1000)
              </label>
              <input
                type="number"
                min={0}
                max={1000}
                value={maxPower}
                onChange={(e) => setMaxPower(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div>
              <label className="lab-label-2">
                加工进给速度 (mm/min)
              </label>
              <input
                type="number"
                min={1}
                value={feedRate}
                onChange={(e) => setFeedRate(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div>
              <label className="lab-label-2">
                空移速度 (mm/min)
              </label>
              <input
                type="number"
                min={1}
                value={travelRate}
                onChange={(e) => setTravelRate(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div>
              <label className="lab-label-2">
                加工循环次数 (Passes)
              </label>
              <input
                type="number"
                min={1}
                max={100}
                value={passes}
                onChange={(e) => setPasses(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div>
              <label className="lab-label-2">
                点距 / 像素尺寸 (mm)
              </label>
              <input
                type="number"
                step="0.01"
                min="0.01"
                value={pixelSize}
                onChange={(e) => setPixelSize(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div>
              <label className="lab-label-2">
                二值阈值 (-1 为自动)
              </label>
              <input
                type="number"
                min={-1}
                max={255}
                value={threshold}
                onChange={(e) => setThreshold(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div>
              <label className="lab-label-2">
                材料别名 (逗号分隔)
              </label>
              <input
                type="text"
                value={aliases}
                onChange={(e) => setAliases(e.target.value)}
                readOnly={readOnly}
                className="text-input lab-field-3"
                placeholder="如：木牌, basswood"
              />
            </div>

            <div className="lab-box-3">
              <label className="lab-label-2">
                工艺与材质备注
              </label>
              <input
                type="text"
                value={notes}
                onChange={(e) => setNotes(e.target.value)}
                readOnly={readOnly}
                className="text-input lab-field-3"
                placeholder="例如：双面覆膜、浅色木纹..."
              />
            </div>
          </div>

          {!readOnly && (
            <div className="lab-row-8">
              <button
                type="button"
                className="btn btn-primary lab-btn-3"
                onClick={handleSaveParams}
                disabled={!selectedMaterial}
              >
                <Save size={14} className="lab-icon" />
                保存到材料库
              </button>
            </div>
          )}
        </div>

        {/* Right Column: Calibration Grid Matrix */}
        <div className="hud-card ob-card lab-row-4">
          <FoldHeader id="lab-matrix" title="校准矩阵" k="MATRIX" n={3} />
          <div className="lab-row-9">
            <h3 className="lab-heading-2">
              {selectedMaterial ? `${selectedMaterial} · ${selectedThickness} mm · ${matrixModeInfo.label}` : "生成校准矩阵并实机测试"}
            </h3>
            <select
              value={matrixMode}
              onChange={(e) => setMatrixMode(e.target.value)}
              className="select-input lab-matrix-mode-select"
              aria-label="标定矩阵加工模式"
            >
              {MATRIX_MODE_OPTIONS.map((m) => (
                <option key={m.id} value={m.id}>{m.label}标定</option>
              ))}
            </select>
          </div>
          {matrixStatus && (
            <div className="lab-matrix-status-line">{matrixStatus}</div>
          )}

          {/* Matrix Parameters */}
          <div className="lab-grid-2">
            <div>
              <label className="lab-label-3">功率区间 (Min-Max)</label>
              <div className="lab-row-10">
                <input
                  type="number"
                  min={0}
                  max={1000}
                  value={matrixPowerMin}
                  onChange={(e) => setMatrixPowerMin(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
                <input
                  type="number"
                  min={0}
                  max={1000}
                  value={matrixPowerMax}
                  onChange={(e) => setMatrixPowerMax(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
              </div>
            </div>

            <div>
              <label className="lab-label-3">速度区间 (Min-Max)</label>
              <div className="lab-row-10">
                <input
                  type="number"
                  min={1}
                  value={matrixSpeedMin}
                  onChange={(e) => setMatrixSpeedMin(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
                <input
                  type="number"
                  min={1}
                  value={matrixSpeedMax}
                  onChange={(e) => setMatrixSpeedMax(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
              </div>
            </div>

            <div>
              <label className="lab-label-3">矩阵行列 ({matrixRows}x{matrixCols})</label>
              <div className="lab-row-10">
                <input
                  type="number"
                  min={1}
                  max={25}
                  value={matrixRows}
                  onChange={(e) => setMatrixRows(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
                <input
                  type="number"
                  min={1}
                  max={25}
                  value={matrixCols}
                  onChange={(e) => setMatrixCols(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
              </div>
            </div>
          </div>

          {/* Device Connection Row（支持网络与 USB 串口） */}
          <div className="lab-conn-bar">
            <select
              value={connectionMode}
              onChange={(e) => {
                const next = e.target.value;
                setConnectionMode(next);
                writeLocalSafe("laser-forge-conn-mode", next);
              }}
              className="select-input lab-conn-mode"
              aria-label="设备连接方式"
            >
              <option value="network">网络 IP</option>
              <option value="serial">USB 串口</option>
            </select>

            {connectionMode === "serial" ? (
              <div className="lab-conn-target">
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
                        writeLocalSafe("laser-forge-serial-port", p);
                      }
                    } else {
                      setSerialPort(chosen);
                      writeLocalSafe("laser-forge-serial-port", chosen);
                    }
                  }}
                  className="select-input lab-field-5"
                  aria-label="选择串口"
                >
                  {serialPort && !serialPorts.includes(serialPort) && (
                    <option value={serialPort}>{serialPort}</option>
                  )}
                  {serialPorts.length === 0 && !serialPort && (
                    <option value="">未检测到串口</option>
                  )}
                  {serialPorts.map((p) => (
                    <option key={p} value={p}>{p}</option>
                  ))}
                  <option value="__custom__">✎ 手动输入串口…</option>
                </select>
                <select
                  value={baudrate}
                  onChange={(e) => {
                    setBaudrate(e.target.value);
                    writeLocalSafe("laser-forge-baud", e.target.value);
                  }}
                  title="串口波特率"
                  className="select-input lab-baud-select"
                  aria-label="串口波特率"
                >
                  {["9600", "57600", "115200", "230400", "250000", "460800", "921600"].map((b) => (
                    <option key={b} value={b}>{b}</option>
                  ))}
                </select>
                <button
                  type="button"
                  className="btn btn-ghost lab-icon-btn"
                  onClick={loadSerialPorts}
                  title="刷新可用串口"
                >
                  <RefreshCw size={12} />
                </button>
              </div>
            ) : (
              <div className="lab-conn-target">
                <input
                  type="text"
                  placeholder="设备 host / IP"
                  value={matrixHost}
                  onChange={(e) => {
                    const val = e.target.value;
                    setMatrixHost(val);
                    writeLocalSafe("laser-forge-host", val);
                  }}
                  className="text-input lab-field-5"
                />
                <input
                  type="number"
                  min={1}
                  max={65535}
                  value={telnetPort}
                  onChange={(e) => {
                    setTelnetPort(e.target.value);
                    writeLocalSafe("laser-forge-telnet-port", e.target.value);
                  }}
                  title="Telnet 端口（默认 23）"
                  className="text-input lab-port-input"
                />
              </div>
            )}
          </div>

          <div className="lab-action-bar">
            <button type="button" className="btn btn-ghost" onClick={handleGenerateMatrix} disabled={matrixBusy || !selectedMaterial}>
              <Grid size={13} className="lab-icon" />
              {matrixId && matrixStale ? "重新生成矩阵" : "生成矩阵"}
            </button>
            {matrixPreview && !matrixStale && (
              <button
                type="button"
                className="btn btn-ghost"
                onClick={handleExportMatrixGcode}
                disabled={matrixBusy}
                title="导出标定矩阵 G-code（可在数字孪生页离线仿真预览）"
              >
                <Download size={13} className="lab-icon" />
                导出刀路
              </button>
            )}
            <button
              type="button"
              className="btn btn-primary"
              onClick={handleSendMatrix}
              disabled={matrixBusy || !matrixId || matrixStale}
            >
              <Send size={13} className="lab-icon" />
              发送测试
            </button>
          </div>

          {matrixStale && (
            <div className="lab-stale-note" role="status">
              标定条件已修改，当前网格仍是上一版预览；请先点击「重新生成矩阵」后再发送测试。
            </div>
          )}

          {/* Matrix interactive preview grid */}
          <div className="lab-row-12">
            {matrixPreview ? (
              <div className="lab-box-4">
                <div className="lab-box-5">
                  {readOnly
                    ? "校准结果（保存最佳单元格请在电脑上操作）："
                    : `点击下方单元格将最佳效果存为「${matrixPreview.material || selectedMaterial} ${matrixPreview.thickness_mm ?? selectedThickness}mm」推荐参数：`}
                </div>
                <div
                  style={{
                    display: "grid",
                    gridTemplateColumns: `repeat(${matrixPreview.columns || matrixCols}, 44px)`,
                    gap: "4px",
                    margin: "0 auto",
                  }}
                >
                  {(matrixPreview.cells || []).map((cell, i) => (
                    <div
                      key={cell.id || i}
                      onClick={readOnly ? undefined : () => handleSaveBestCell(i)}
                      style={{
                        width: "44px",
                        height: "44px",
                        background: selectedCellNum === i + 1 ? "var(--laser, #8b7bff)" : "rgba(255,255,255,0.08)",
                        color: selectedCellNum === i + 1 ? "#000" : "#fff",
                        display: "flex",
                        flexDirection: "column",
                        alignItems: "center",
                        justifyContent: "center",
                        fontSize: "9px",
                        lineHeight: 1.15,
                        cursor: readOnly ? "default" : "pointer",
                        borderRadius: "2px",
                        border: "1px solid rgba(255,255,255,0.1)",
                      }}
                      title={`#${i + 1} ${cell.id}: S${cell.power} F${cell.speed} × ${cell.passes}`}
                    >
                      <span className="lab-text-2">{i + 1}</span>
                      <span>S{cell.power}</span>
                      <span>F{cell.speed}</span>
                    </div>
                  ))}
                </div>
                {matrixSendState ? (
                  <div className="lab-box-6">
                    发送状态: {matrixSendState.status} · {matrixSendState.lines_sent}/{matrixSendState.total_lines} 行
                    {matrixSendState.error ? ` · ${matrixSendState.error}` : ""}
                  </div>
                ) : null}
              </div>
            ) : (
              <div className="lab-box-7">
                <Grid size={28} className="lab-icon-3" />
                <div>配置区间并点击【生成矩阵】预览校准网格</div>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Material Add / Edit / Delete Modal */}
      <MaterialModal
        visible={showMaterialModal && !readOnly}
        initialData={materialModalData}
        onClose={() => setShowMaterialModal(false)}
        onSaved={(saved) => {
          setShowMaterialModal(false);
          const nextName = saved?.name || materialModalData?.name || selectedMaterial;
          void loadBootstrap({ material: nextName });
        }}
        onDeleted={() => {
          setShowMaterialModal(false);
          void loadBootstrap();
        }}
      />
    </div>
  );
}
