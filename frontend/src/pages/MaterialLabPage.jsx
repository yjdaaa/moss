import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import React, { useState, useEffect, useRef, useCallback } from "react";
import {
  Bookmark,
  Download,
  Upload,
  Grid,
  Send,
  Square,
  ArrowUpRight,
} from "lucide-react";
import { apiFetch, DEFAULT_READ_TIMEOUT_MS, getJson, postJson } from "../api/client.js";
import { isReadOnlyReferenceClient } from "../authSession.js";
import { MaterialModal } from "../components/MaterialModal.jsx";
import { showAlert, showChoice, showConfirm } from "../appDialog.js";
import { exportFile, savedMessage } from "../saveFile.js";
import { FoldHeader } from "../components/FoldHeader.jsx";
import { CalibrationWizard } from "../components/CalibrationWizard.jsx";
import { markWizardStep, readWizardState, wizardMaterialKey, writeWizardState } from "../calibrationWizard.js";
import { estopTargetArgs, visibleEstopContext } from "../globalEstop.js";
import { usePageEmergencyStop } from "../usePageEstop.js";
import { notifyMaterialsUpdated, subscribeMaterialsUpdated } from "../materialSelectState.js";
import {
  resolveMaterialLabSelection,
  resolveCellEditorValues,
  isCellEditorDirty,
  resolveMatrixModeParams,
  matrixInputSignature,
  isMatrixStale,
  buildMatrixSendRequest,
  buildMatrixConfirmPrompt,
  buildMatrixPreviewPayload,
  estimateMatrixDimensionsMm,
  checkMatrixFitsWorkspace,
  matrixPollOutcome,
  matrixSendOutcome,
  unwrapImportedMaterialLibrary,
  isSurfaceKey,
  thicknessChipLabel,
  powerScaleToPercent,
} from "../materialLabState.js";
import {
  WORKSPACE_LIMITS_EVENT,
  readWorkspaceLimitsStore,
  resolveSoftLimitSendWarning,
  resolveWorkspaceLimitsForTarget,
} from "../deviceConnection.js";
import { drawConnectionPayload } from "../drawConnection.js";
import { machineMotionPayload, readFillSettings, readMachineMotion } from "../fillOptions.js";
import { readFillSettingsWithMachineCalibration } from "../machineProfileSync.js";
import { readLocalSafe, writeLocalSafe } from "../components/material-lab/labStorage.js";
import { LabLibraryPanel } from "../components/material-lab/LabLibraryPanel.jsx";
import { LabParamsPanel } from "../components/material-lab/LabParamsPanel.jsx";
import { LabMatrixSettings } from "../components/material-lab/LabMatrixSettings.jsx";
import { LabConnectionBar } from "../components/material-lab/LabConnectionBar.jsx";
import { useDeviceConnection } from "../useDeviceConnection.js";
import { LabMatrixGrid } from "../components/material-lab/LabMatrixGrid.jsx";

export function MaterialLabPage({
  onNavigate,
  connectionStatus = null,
  onOpenConnection = null,
}) {
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
  const [entryTitle, setEntryTitle] = useState("");
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
  // 2026-11-16 对齐 LightBurn：测试类型、两个轴的参数、线距 / 遍数区间与行列标注
  const [matrixTestType, setMatrixTestType] = useState("matrix");
  const [matrixXAxis, setMatrixXAxis] = useState("power");
  const [matrixYAxis, setMatrixYAxis] = useState("speed");
  const [matrixIntervalMin, setMatrixIntervalMin] = useState(0.08);
  const [matrixIntervalMax, setMatrixIntervalMax] = useState(0.16);
  const [matrixIntervalCount, setMatrixIntervalCount] = useState(9);
  const [matrixPassesMin, setMatrixPassesMin] = useState(1);
  const [matrixPassesMax, setMatrixPassesMax] = useState(3);
  const [matrixLabels, setMatrixLabels] = useState(true);
  const {
    connectionMode,
    networkHost: matrixHost,
    telnetPort,
    serialPort,
    baudrate,
  } = useDeviceConnection();
  const labConnectionForm = {
    connection_mode: connectionMode,
    port: serialPort,
    baudrate: String(baudrate),
    network_host: matrixHost,
    network_telnet_port: String(telnetPort),
  };
  const [workspaceLimitsStore, setWorkspaceLimitsStore] = useState(() => readWorkspaceLimitsStore());
  useEffect(() => {
    const onLimits = (e) => {
      if (e?.detail) setWorkspaceLimitsStore(e.detail);
    };
    window.addEventListener(WORKSPACE_LIMITS_EVENT, onLimits);
    return () => window.removeEventListener(WORKSPACE_LIMITS_EVENT, onLimits);
  }, []);
  const effectiveWorkspaceLimits = resolveWorkspaceLimitsForTarget(workspaceLimitsStore, labConnectionForm);
  const [matrixPreview, setMatrixPreview] = useState(null);
  const [matrixSignature, setMatrixSignature] = useState("");
  const [matrixId, setMatrixId] = useState("");
  const [selectedCellNum, setSelectedCellNum] = useState(null);
  const [matrixStatus, setMatrixStatus] = useState("");
  const [matrixBusy, setMatrixBusy] = useState(false);
  const [matrixSendState, setMatrixSendState] = useState(null); // {status, lines_sent, total_lines, error}
  const matrixPollRef = useRef(null);
  // 首次标定向导进度（r40）：存本机，跟材料的步骤按“材料|厚度”分开
  const [wizard, setWizard] = useState(() => readWizardState());
  const updateWizard = (next) => {
    setWizard(next);
    writeWizardState(next);
  };

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
    testType: matrixTestType,
    xAxis: matrixXAxis,
    yAxis: matrixYAxis,
    intervalMin: matrixIntervalMin,
    intervalMax: matrixIntervalMax,
    intervalCount: matrixIntervalCount,
    passesMin: matrixPassesMin,
    passesMax: matrixPassesMax,
    labels: matrixLabels,
  });
  // 向导测试图案不受矩阵输入框影响（它绑定生成时的材料与厚度，记录里自带）
  const matrixStale = Boolean(matrixId) && !matrixPreview?.wizard_pattern && isMatrixStale(matrixSignature, currentMatrixSig);

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
    setEntryTitle(resolved.title || "");
    setAliases(resolved.aliases);
    setHasSavedCell(resolved.hasSavedCell);
  }, []);

  const loadBootstrap = useCallback(async (preferred = {}) => {
    const res = await getJson("/v1/material-lab/bootstrap", { timeoutMs: DEFAULT_READ_TIMEOUT_MS });
    if (res.ok && res.data?.success) {
      const result = res.data.result || {};
      setSourcePath(result.source_path || "");
      const mats = result.materials?.materials || {};
      setMaterials(mats);

      const targetMat = preferred.material ?? selectedMaterialRef.current;
      const targetThick = preferred.thickness ?? selectedThicknessRef.current;
      const sel = resolveMaterialLabSelection(mats, targetMat, targetThick);
      const rawMode = preferred.mode ?? activeModeRef.current;
      const targetMode = isSurfaceKey(sel.thickness) && rawMode === "cut" ? "raster" : rawMode;
      setSelectedMaterial(sel.material);
      setSelectedThickness(sel.thickness);
      if (targetMode !== activeModeRef.current) {
        setActiveMode(targetMode);
      }
      if (isSurfaceKey(sel.thickness)) {
        setMatrixMode((prev) => (prev === "cut" ? "raster" : prev));
      }
      applyEditorState(mats, sel.material, sel.thickness, targetMode);
    }
  }, [applyEditorState]);

  useEffect(() => {
    void loadBootstrap();
    const unsubscribeMaterials = subscribeMaterialsUpdated(() => {
      void loadBootstrap();
    });
    return () => {
      matrixPollRef.current?.();
      unsubscribeMaterials();
    };
  }, [loadBootstrap]);

  // 顶栏常驻急停与本页「停止测试」共用：在途发送先按 calibration_id 停，若刚发完最后一行仍在缓冲执行则按本页设备目标补发 0x18；
  // 不提前终止状态轮询、不伪造 cancelled，等后端落定真实终态后再解锁按钮
  const isMatrixRunning =
    matrixSendState?.status === "pending"
    || matrixSendState?.status === "preparing"
    || matrixSendState?.status === "sending"
    || matrixSendState?.status === "running";
  const handleStopMatrix = usePageEmergencyStop(
    () => visibleEstopContext(
      isMatrixRunning ? matrixId : "",
      estopTargetArgs({
        connection_mode: connectionMode,
        port: serialPort,
        baudrate,
        network_host: matrixHost,
        network_telnet_port: telnetPort,
      }),
      matrixSendState?.target,
    ),
    (outcome) => {
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

  // 手机连接电脑或局域网浏览器访问时材料与校准结果只读（后端同样拒绝）：修改类按钮不显示，参数只能查看
  const readOnly = isReadOnlyReferenceClient();

  const isParamsDirty = isCellEditorDirty(materials, selectedMaterial, selectedThickness, activeMode, {
    maxPower,
    feedRate,
    passes,
    pixelSize,
    aliases,
    entryTitle,
    notes,
  });
  const handleResetParams = useCallback(() => {
    applyEditorState(materials, selectedMaterial, selectedThickness, activeMode);
  }, [applyEditorState, materials, selectedMaterial, selectedThickness, activeMode]);

  const handleSelectMaterialAndThick = (matName, thickKey) => {
    const nextMode = isSurfaceKey(thickKey) && activeMode === "cut" ? "raster" : activeMode;
    setSelectedMaterial(matName);
    setSelectedThickness(thickKey);
    if (nextMode !== activeMode) setActiveMode(nextMode);
    if (isSurfaceKey(thickKey) && matrixMode === "cut") changeMatrixMode("raster");
    writeLocalSafe("laser-forge-mat", matName);
    writeLocalSafe("laser-forge-thick", thickKey);
    applyEditorState(materials, matName, thickKey, nextMode);
  };

  // 切换标定模式：线距只对光栅填充有意义，离开光栅时把线距测试 / 线距轴换回默认
  const changeMatrixMode = (mode) => {
    setMatrixMode(mode);
    if (mode !== "raster") {
      setMatrixTestType("matrix");
      setMatrixXAxis((axis) => (axis === "interval" ? (matrixYAxis === "power" ? "speed" : "power") : axis));
      setMatrixYAxis((axis) => (axis === "interval" ? (matrixXAxis === "speed" ? "power" : "speed") : axis));
    }
  };

  const handleModeChange = (mode) => {
    setActiveMode(mode);
    changeMatrixMode(mode);
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
  // toSurface=true：存为不分厚度的“表面通用”条目（LightBurn No thickness 条目，只存雕刻参数）
  const handleSaveParams = async (toSurface = false) => {
    const trimmedMat = String(selectedMaterial || "").trim();
    const surface = toSurface === true || isSurfaceKey(selectedThickness);
    const thickNum = Number(selectedThickness);
    if (!trimmedMat) {
      setEditorStatus("保存失败: 请先新建或选择材料");
      return;
    }
    if (surface && activeMode === "cut") {
      setEditorStatus("保存失败: 表面通用条目只存雕刻参数，切割请按真实厚度保存");
      return;
    }
    if (!surface && (!Number.isFinite(thickNum) || thickNum <= 0)) {
      setEditorStatus("保存失败: 厚度必须大于 0");
      return;
    }
    const maxP = Number(maxPower);
    if (!Number.isFinite(maxP) || maxP < 0 || maxP > 1000) {
      setEditorStatus("保存失败: 最大激光功率须在 0 ~ 100% 之间");
      return;
    }
    const passesNum = Number(passes);
    if (!Number.isInteger(passesNum) || passesNum < 1 || passesNum > 5) {
      setEditorStatus("保存失败: 加工遍数须为 1 ~ 5 的整数");
      return;
    }
    const safeMinP = Number.isFinite(Number(minPower))
      ? Math.max(0, Math.min(Math.round(Number(minPower)), Math.round(maxP)))
      : 0;

    setEditorStatus("正在保存...");
    const payload = {
      action: "save",
      material: trimmedMat,
      ...(surface ? { surface: true } : { thickness_mm: thickNum }),
      title: entryTitle.trim(),
      laser_mode: activeMode === "cut" ? "cut" : "engrave",
      engraving_mode: activeMode === "cut" ? "" : activeMode,
      laser_min_power: safeMinP,
      laser_max_power: Math.round(maxP),
      feed_rate: Number(feedRate),
      travel_rate: Number(travelRate),
      passes: passesNum,
      pixel_size_mm: Number(pixelSize),
      threshold: Number(threshold),
      notes: notes.trim(),
      aliases: aliases.split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean),
    };

    try {
      const res = await postJson("/api/material-lab/manage", payload);
      if (res.ok && res.data?.success) {
        setEditorStatus(surface ? "参数已成功保存为表面通用条目！" : "参数已成功保存！");
        setTimeout(() => setEditorStatus(""), 3000);
        notifyMaterialsUpdated();
        await loadBootstrap({
          material: trimmedMat,
          thickness: surface ? "surface" : String(thickNum),
          mode: activeMode,
        });
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
    const surface = isSurfaceKey(selectedThickness);
    const thicknessLabel = thicknessChipLabel(selectedThickness);
    const confirmed = await showConfirm(
      `确定删除「${selectedMaterial}」的 ${thicknessLabel} ${surface ? "条目" : "厚度规格"}及其下的全部参数吗？`,
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
        ...(surface ? { surface: true } : { thickness_mm: Number(selectedThickness) }),
        confirmed: true,
      });
      if (res.ok && res.data?.success) {
        setEditorStatus(`已移除 ${thicknessLabel}`);
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
        changeMatrixMode(mode);
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
    const numericThickness = Object.keys(materials[selectedMaterial]?.thicknesses || {})
      .map(Number)
      .find((value) => Number.isFinite(value) && value > 0);
    const built = buildMatrixPreviewPayload({
      material: selectedMaterial,
      thickness: selectedThickness,
      fallbackThickness: numericThickness || 3,
      mode: matrixMode,
      testType: matrixTestType,
      rows: matrixRows,
      cols: matrixCols,
      powerMin: matrixPowerMin,
      powerMax: matrixPowerMax,
      speedMin: matrixSpeedMin,
      speedMax: matrixSpeedMax,
      xAxis: matrixXAxis,
      yAxis: matrixYAxis,
      intervalMin: matrixIntervalMin,
      intervalMax: matrixIntervalMax,
      intervalCount: matrixIntervalCount,
      passesMin: matrixPassesMin,
      passesMax: matrixPassesMax,
      labels: matrixLabels,
    });
    if (built.error) {
      setMatrixStatus(`生成失败: ${built.error}`);
      return;
    }
    const estimatedDims = estimateMatrixDimensionsMm({
      testType: matrixTestType,
      rows: matrixRows,
      cols: matrixCols,
      intervalCount: matrixIntervalCount,
      yAxis: matrixYAxis,
      speedMax: matrixSpeedMax,
      powerMax: matrixPowerMax,
      intervalMax: matrixIntervalMax,
      passesMax: matrixPassesMax,
      labels: matrixLabels,
    });
    const fitCheck = checkMatrixFitsWorkspace(estimatedDims, effectiveWorkspaceLimits);
    if (!fitCheck.ok) {
      setMatrixStatus(`生成失败: ${fitCheck.error}`);
      void showAlert(fitCheck.error, { title: "矩阵超出当前行程", tone: "warning" });
      return;
    }
    const payload = {
      ...built.payload,
      work_area_width_mm: effectiveWorkspaceLimits.x_travel_mm,
      work_area_height_mm: effectiveWorkspaceLimits.y_travel_mm,
      ...drawConnectionPayload(labConnectionForm),
      ...machineMotionPayload(
        readFillSettingsWithMachineCalibration(readFillSettings, labConnectionForm),
        readMachineMotion(labConnectionForm),
      ),
    };
    const testLabel = matrixTestType === "interval" ? "线距测试" : `「${modeInfo.label}」标定矩阵`;
    setMatrixBusy(true);
    setMatrixStatus(`正在生成${testLabel} G-code 与预览...`);

    try {
      const res = await postJson("/api/material-lab/preview", payload);
      setMatrixBusy(false);
      if (res.ok && res.data?.success && res.data.result?.calibration_id) {
        const resData = res.data.result;
        matrixPollRef.current?.();
        matrixPollRef.current = null;
        setMatrixId(resData.calibration_id);
        setMatrixPreview(resData);
        setMatrixSignature(sigSnapshot);
        setSelectedCellNum(null);
        setMatrixSendState(null);
        const est = resData.time_estimate?.estimated_minutes;
        const sizeStr = resData.width_mm && resData.height_mm
          ? `，外框 ${Number(resData.width_mm).toFixed(0)}×${Number(resData.height_mm).toFixed(0)} mm`
          : "";
        const warnList = Array.isArray(resData.warnings)
          ? resData.warnings.filter((w) => typeof w === "string" && w.trim())
          : [];
        const warnStr = warnList.length > 0 ? `；⚠️ ${warnList.join("；")}` : "";
        setMatrixStatus(
          `${resData.test_type === "interval" ? "线距测试" : `${modeInfo.label}矩阵`}已生成（${resData.cell_count || 0} 格${sizeStr}${typeof est === "number" ? `，约 ${est.toFixed(1)} 分钟` : ""}）；发送实机测试后点选最佳格${warnStr}`,
        );
      } else if (res.ok && res.data?.success) {
        setMatrixStatus("生成失败：服务端未返回标定编号，请重启或更新 MOSS");
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

    const safetyWarning = resolveSoftLimitSendWarning(labConnectionForm, undefined, connectionStatus);
    const prompt = buildMatrixConfirmPrompt({
      preview: matrixPreview,
      material: selectedMaterial,
      thickness: selectedThickness,
      mode: matrixPreview?.wizard_pattern ? (matrixPreview.laser_mode === "cut" ? "cut" : "raster") : matrixMode,
      targetLabel: built.targetLabel,
      safetyWarning,
      sendWarnings: matrixSendState?.warnings,
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
      const outcome = matrixSendOutcome(res, built);
      if (outcome.sendState) setMatrixSendState(outcome.sendState);
      setMatrixStatus(outcome.status);
      if (outcome.poll) startMatrixPolling(matrixId);
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
        // 状态转移在 matrixPollOutcome 里（纯函数、有单测）；提示与是否结束不依赖上一次发送状态，
        // 发送状态在更新函数里基于最新值合并，查询失败时原样保留
        const outcome = matrixPollOutcome(null, state, res);
        setMatrixSendState((previous) => matrixPollOutcome(previous, state, res).sendState);
        setMatrixStatus(outcome.status);
        if (outcome.terminal) {
          stop();
          matrixPollRef.current = null;
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

    const prevSelectedCellNum = selectedCellNum;
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
        const savedSurface = res.data.result?.selected_cell?.surface === true;
        setMatrixStatus(
          `已将单元格 #${cellNumber} 保存为「${targetMat} ${savedSurface ? "表面通用" : `${targetThick}mm`}」推荐参数！`,
        );
        setActiveMode(targetUiMode);
        // 向导的线距 / 矩阵步骤：光栅选格写回成功即算完成
        if (targetLaserMode !== "cut" && targetEngravingMode === "raster") {
          const wizardStep = matrixPreview?.test_type === "interval" ? "interval" : "matrix";
          const cell = matrixPreview?.cells?.[cellIndex] || {};
          // 进度键与向导读取时一致：厚度用材料库里的原始键（如 "3.0"），不用解析后的数字
          const wizardThickness = savedSurface
            ? "surface"
            : Number(selectedThickness) === targetThick ? String(selectedThickness) : String(targetThick);
          updateWizard(markWizardStep(readWizardState(), wizardStep, "done", {
            materialKey: wizardMaterialKey(targetMat, wizardThickness),
            note: wizardStep === "interval"
              ? `线距 ${cell.line_interval_mm ?? "-"} mm（#${cellNumber}）`
              : `功率 ${cell.power != null ? `${powerScaleToPercent(cell.power)}%（S${cell.power}）` : "-"} · 速度 ${cell.speed ?? "-"} mm/min（#${cellNumber}）`,
          }));
        }
        notifyMaterialsUpdated();
        await loadBootstrap({
          material: targetMat,
          thickness: savedSurface ? "surface" : String(targetThick),
          mode: targetUiMode,
        });
      } else {
        setSelectedCellNum(prevSelectedCellNum);
        setMatrixStatus(`保存单元格失败: ${res.data?.result || "错误"}`);
      }
    } catch (e) {
      setSelectedCellNum(prevSelectedCellNum);
      setMatrixStatus(`保存单元格网络异常: ${e.message}`);
    }
  };

  // 向导：把右侧矩阵面板设成线距测试 / 功率 × 速度矩阵（光栅），由用户照常生成、确认发送、选格
  const prepareWizardMatrix = (kind) => {
    handleModeChange("raster");
    setMatrixTestType(kind === "interval" ? "interval" : "matrix");
    if (kind !== "interval") {
      setMatrixXAxis("power");
      setMatrixYAxis("speed");
    }
    setMatrixStatus(
      kind === "interval"
        ? "向导：已设为线距测试，点【生成矩阵】→【发送测试】（会先确认），刻完点选最好的方块"
        : "向导：已设为功率 × 速度矩阵（用材料库线距），点【生成矩阵】→【发送测试】，刻完点选最好的格子",
    );
    try {
      document.getElementById("lab-matrix-card")?.scrollIntoView({ behavior: "smooth", block: "start" });
    } catch {
      // 滚动失败不影响设置
    }
  };

  // 向导测试图案（双向扫描偏移 / 过扫描 / 切缝）：只生成并显示在矩阵面板，发送仍走【发送测试】的确认框
  const generateWizardPattern = async (payload) => {
    setMatrixBusy(true);
    setMatrixStatus("正在生成向导测试刀路...");
    try {
      const res = await postJson("/api/material-lab/preview", {
        ...payload,
        work_area_width_mm: effectiveWorkspaceLimits.x_travel_mm,
        work_area_height_mm: effectiveWorkspaceLimits.y_travel_mm,
        ...drawConnectionPayload(labConnectionForm),
        ...machineMotionPayload(
          readFillSettingsWithMachineCalibration(readFillSettings, labConnectionForm),
          readMachineMotion(labConnectionForm),
        ),
      });
      if (res.ok && res.data?.success && res.data.result?.calibration_id) {
        const record = res.data.result;
        matrixPollRef.current?.();
        setMatrixId(record.calibration_id);
        setMatrixPreview(record);
        setMatrixSignature(currentMatrixSig);
        setSelectedCellNum(null);
        setMatrixSendState(null);
        const est = record.time_estimate?.estimated_minutes;
        const warnList = Array.isArray(record.warnings)
          ? record.warnings.filter((w) => typeof w === "string" && w.trim())
          : [];
        const warnStr = warnList.length > 0 ? `；⚠️ ${warnList.join("；")}` : "";
        setMatrixStatus(
          `向导测试已生成（${record.cell_count || 0} 块，外框 ${Number(record.width_mm || 0).toFixed(0)}×${Number(record.height_mm || 0).toFixed(0)} mm${typeof est === "number" ? `，约 ${est.toFixed(1)} 分钟` : ""}）；放好样板后点【发送测试】${warnStr}`,
        );
        return record;
      }
      setMatrixStatus(`生成失败: ${res.data?.result || res.error || "校验未通过"}`);
      return null;
    } catch (e) {
      setMatrixStatus(`生成网络异常: ${e.message}`);
      return null;
    } finally {
      setMatrixBusy(false);
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
      {/* 工具条：页面标题已在顶栏；存储文件路径放悬停提示，不占工具条宽度 */}
      <div className="ob-toolbar">
        <div className="ob-toolbar-info" title={sourcePath ? `材料库文件：${sourcePath}` : undefined}>
          <span>材料库 · 校准矩阵</span>
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

      <CalibrationWizard
        wizard={wizard}
        onWizardChange={updateWizard}
        material={selectedMaterial}
        thickness={selectedThickness}
        readOnly={readOnly}
        onPrepareMatrix={prepareWizardMatrix}
        onGeneratePattern={generateWizardPattern}
        patternPreview={matrixPreview}
        patternStale={matrixStale}
        busy={matrixBusy}
        running={isMatrixRunning}
        onSend={handleSendMatrix}
        connectionForm={labConnectionForm}
      />

      {/* 三栏：材料库表格、材料参数、校准矩阵（技术书 §9.5.5 B） */}
      <div className="ob-lab">
        {/* Left Column: Material Tree */}
        <LabLibraryPanel
          materials={materials}
          filteredMaterials={filteredMaterials}
          searchQuery={searchQuery}
          setSearchQuery={setSearchQuery}
          readOnly={readOnly}
          selectedMaterial={selectedMaterial}
          selectedThickness={selectedThickness}
          handleSelectMaterialAndThick={handleSelectMaterialAndThick}
          setMaterialModalData={setMaterialModalData}
          setShowMaterialModal={setShowMaterialModal}
        />

        {/* Middle Column: Parameter Editor */}
        <LabParamsPanel
          materials={materials}
          readOnly={readOnly}
          selectedMaterial={selectedMaterial}
          selectedThickness={selectedThickness}
          currentMatThicknesses={currentMatThicknesses}
          hasSavedCell={hasSavedCell}
          isDirty={isParamsDirty}
          handleResetParams={handleResetParams}
          editorStatus={editorStatus}
          activeMode={activeMode}
          handleModeChange={handleModeChange}
          handleDeleteCurrentThickness={handleDeleteCurrentThickness}
          handleSaveParams={handleSaveParams}
          setMaterialModalData={setMaterialModalData}
          setShowMaterialModal={setShowMaterialModal}
          minPower={minPower}
          setMinPower={setMinPower}
          maxPower={maxPower}
          setMaxPower={setMaxPower}
          feedRate={feedRate}
          setFeedRate={setFeedRate}
          travelRate={travelRate}
          setTravelRate={setTravelRate}
          passes={passes}
          setPasses={setPasses}
          pixelSize={pixelSize}
          setPixelSize={setPixelSize}
          threshold={threshold}
          setThreshold={setThreshold}
          aliases={aliases}
          setAliases={setAliases}
          entryTitle={entryTitle}
          setEntryTitle={setEntryTitle}
          notes={notes}
          setNotes={setNotes}
        />

        {/* Right Column: Calibration Grid Matrix */}
        <div className="hud-card ob-card lab-row-4" id="lab-matrix-card">
          <FoldHeader id="lab-matrix" title="校准矩阵" n={3} />
          <LabMatrixSettings
            selectedMaterial={selectedMaterial}
            selectedThickness={selectedThickness}
            matrixMode={matrixMode}
            matrixModeInfo={matrixModeInfo}
            changeMatrixMode={changeMatrixMode}
            matrixStatus={matrixStatus}
            matrixPowerMin={matrixPowerMin}
            setMatrixPowerMin={setMatrixPowerMin}
            matrixPowerMax={matrixPowerMax}
            setMatrixPowerMax={setMatrixPowerMax}
            matrixSpeedMin={matrixSpeedMin}
            setMatrixSpeedMin={setMatrixSpeedMin}
            matrixSpeedMax={matrixSpeedMax}
            setMatrixSpeedMax={setMatrixSpeedMax}
            matrixRows={matrixRows}
            setMatrixRows={setMatrixRows}
            matrixCols={matrixCols}
            setMatrixCols={setMatrixCols}
            matrixTestType={matrixTestType}
            setMatrixTestType={setMatrixTestType}
            matrixIntervalMin={matrixIntervalMin}
            setMatrixIntervalMin={setMatrixIntervalMin}
            matrixIntervalMax={matrixIntervalMax}
            setMatrixIntervalMax={setMatrixIntervalMax}
            matrixIntervalCount={matrixIntervalCount}
            setMatrixIntervalCount={setMatrixIntervalCount}
            matrixXAxis={matrixXAxis}
            setMatrixXAxis={setMatrixXAxis}
            matrixYAxis={matrixYAxis}
            setMatrixYAxis={setMatrixYAxis}
            matrixPassesMin={matrixPassesMin}
            setMatrixPassesMin={setMatrixPassesMin}
            matrixPassesMax={matrixPassesMax}
            setMatrixPassesMax={setMatrixPassesMax}
            matrixLabels={matrixLabels}
            setMatrixLabels={setMatrixLabels}
          />

          {/* Device Connection Summary（与全站共用统一设备连接弹窗） */}
          <LabConnectionBar
            connectionForm={{
              connection_mode: connectionMode,
              port: serialPort,
              baudrate,
              network_host: matrixHost,
              network_telnet_port: telnetPort,
            }}
            connectionStatus={connectionStatus}
            onOpenConnection={onOpenConnection}
            disabled={matrixBusy || isMatrixRunning}
          />

          <div className="lab-action-bar">
            <button type="button" className="btn btn-ghost" onClick={handleGenerateMatrix} disabled={matrixBusy || isMatrixRunning || !selectedMaterial}>
              <Grid size={13} className="lab-icon" />
              {matrixId && matrixStale ? "重新生成矩阵" : "生成矩阵"}
            </button>
            {matrixPreview && !matrixStale && (
              <button
                type="button"
                className="btn btn-ghost"
                onClick={handleExportMatrixGcode}
                disabled={matrixBusy || isMatrixRunning}
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
              disabled={matrixBusy || isMatrixRunning || !matrixId || matrixStale}
            >
              <Send size={13} className="lab-icon" />
              {isMatrixRunning ? "正在下发测试…" : "发送测试"}
            </button>
            {isMatrixRunning && (
              <button
                type="button"
                className="btn btn-danger"
                onClick={handleStopMatrix}
              >
                <Square size={13} className="lab-icon" />
                停止测试
              </button>
            )}
          </div>

          {matrixStale && (
            <div className="lab-stale-note" role="status">
              标定条件已修改，当前网格仍是上一版预览；请先点击「重新生成矩阵」后再发送测试。
            </div>
          )}

          {/* Matrix interactive preview grid */}
          <LabMatrixGrid
            matrixPreview={matrixPreview}
            matrixCols={matrixCols}
            readOnly={readOnly}
            selectedMaterial={selectedMaterial}
            selectedThickness={selectedThickness}
            selectedCellNum={selectedCellNum}
            handleSaveBestCell={handleSaveBestCell}
            matrixSendState={matrixSendState}
          />
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
