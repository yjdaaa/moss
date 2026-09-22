import React, { useState, useEffect, useRef } from "react";
import {
  Layers,
  Search,
  Plus,
  Bookmark,
  Download,
  Upload,
  Save,
  Trash2,
  Copy,
  Grid,
  Send,
  CheckCircle2,
  AlertTriangle,
  Info,
  Clock,
  Settings2,
} from "lucide-react";
import { getJson, postJson, fetchPreviewBlobUrl } from "../api/client.js";
import { MaterialModal } from "../components/MaterialModal.jsx";
import { showAlert } from "../appDialog.js";

export function MaterialLabPage({ onNavigate }) {
  const [bootstrapData, setBootstrapData] = useState(null);
  const [materials, setMaterials] = useState({});
  const [sourcePath, setSourcePath] = useState("");
  const [searchQuery, setSearchQuery] = useState("");

  // Modal State
  const [showMaterialModal, setShowMaterialModal] = useState(false);
  const [materialModalData, setMaterialModalData] = useState(null);

  // Selected Material & Thickness & Mode
  const [selectedMaterial, setSelectedMaterial] = useState("椴木");
  const [selectedThickness, setSelectedThickness] = useState("3");
  const [activeMode, setActiveMode] = useState("raster"); // raster, outline, cut

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

  // Calibration Matrix State
  const [matrixLaserMode, setMatrixLaserMode] = useState("engrave");
  const [matrixRows, setMatrixRows] = useState(5);
  const [matrixCols, setMatrixCols] = useState(5);
  const [matrixPowerMin, setMatrixPowerMin] = useState(100);
  const [matrixPowerMax, setMatrixPowerMax] = useState(600);
  const [matrixSpeedMin, setMatrixSpeedMin] = useState(500);
  const [matrixSpeedMax, setMatrixSpeedMax] = useState(2500);
  const [matrixHost, setMatrixHost] = useState(localStorage.getItem("laser-forge-host") || "");
  const [matrixPreview, setMatrixPreview] = useState(null);
  const [matrixId, setMatrixId] = useState("");
  const [selectedCellNum, setSelectedCellNum] = useState(null);
  const [matrixStatus, setMatrixStatus] = useState("");
  const [matrixBusy, setMatrixBusy] = useState(false);
  const [matrixSendState, setMatrixSendState] = useState(null); // {status, lines_sent, total_lines, error}
  const matrixPollRef = useRef(null);

  useEffect(() => {
    loadBootstrap();
    return () => {
      if (matrixPollRef.current) clearInterval(matrixPollRef.current);
    };
  }, []);

  const loadBootstrap = async () => {
    const res = await getJson("/v1/material-lab/bootstrap");
    if (res.ok && res.data?.success) {
      const result = res.data.result || {};
      setBootstrapData(result);
      setSourcePath(result.source_path || "");
      const mats = result.materials?.materials || {};
      setMaterials(mats);

      // Pick first material if available
      const matNames = Object.keys(mats);
      if (matNames.length > 0) {
        const first = matNames[0];
        setSelectedMaterial(first);
        const thicks = Object.keys(mats[first]?.thicknesses || {});
        if (thicks.length > 0) {
          setSelectedThickness(thicks[0]);
          loadCellParams(mats, first, thicks[0], activeMode);
        }
      }
    }
  };

  const loadCellParams = (allMats, matName, thickKey, mode) => {
    const mat = allMats[matName];
    if (!mat) return;
    setAliases((mat.aliases || []).join(", "));
    const thickObj = mat.thicknesses?.[thickKey];
    if (!thickObj) return;

    let cell = null;
    if (mode === "cut") {
      cell = thickObj.cut;
    } else {
      cell = thickObj.engrave?.[mode];
    }

    if (cell) {
      setMinPower(cell.laser_min_power ?? 0);
      setMaxPower(cell.laser_max_power ?? 400);
      setFeedRate(cell.feed_rate ?? 1200);
      setTravelRate(cell.travel_rate ?? 3000);
      setPasses(cell.passes ?? 1);
      setPixelSize(cell.pixel_size_mm ?? 0.1);
      setThreshold(cell.threshold ?? -1);
      setNotes(cell.notes ?? "");
    }
  };

  const handleSelectMaterialAndThick = (matName, thickKey) => {
    setSelectedMaterial(matName);
    setSelectedThickness(thickKey);
    loadCellParams(materials, matName, thickKey, activeMode);
  };

  const handleModeChange = (mode) => {
    setActiveMode(mode);
    loadCellParams(materials, selectedMaterial, selectedThickness, mode);
  };

  // Save current parameter cell
  const handleSaveParams = async () => {
    setEditorStatus("正在保存...");
    const payload = {
      action: "save",
      material: selectedMaterial,
      thickness_mm: Number(selectedThickness),
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
        loadBootstrap();
      } else {
        setEditorStatus(`保存失败: ${res.data?.result || "未知错误"}`);
      }
    } catch (e) {
      setEditorStatus(`保存异常: ${e.message}`);
    }
  };

  // Save from Recent workflow
  const handleSaveRecent = async () => {
    setEditorStatus("正在拉取最近加工参数...");
    try {
      const res = await postJson("/api/material-lab/recent", {});
      if (res.ok && res.data?.success) {
        const p = res.data.result;
        setSelectedMaterial(p.material || selectedMaterial);
        setSelectedThickness(String(p.thickness_mm || selectedThickness));
        const mode = p.laser_mode === "cut" ? "cut" : p.engraving_mode || "raster";
        setActiveMode(mode);
        if (p.params) {
          setMinPower(p.params.laser_min_power ?? 0);
          setMaxPower(p.params.laser_max_power ?? 400);
          setFeedRate(p.params.feed_rate ?? 1200);
          setTravelRate(p.params.travel_rate ?? 3000);
          setPasses(p.params.passes ?? 1);
          setPixelSize(p.params.pixel_size_mm ?? 0.1);
          setThreshold(p.params.threshold ?? -1);
        }
        setEditorStatus("已成功套用最近工作流参数！");
        setTimeout(() => setEditorStatus(""), 3000);
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
        const dataStr = "data:text/json;charset=utf-8," + encodeURIComponent(JSON.stringify(res.data.result?.library || {}, null, 2));
        const dl = document.createElement("a");
        dl.setAttribute("href", dataStr);
        dl.setAttribute("download", "lasergrbl_materials.json");
        dl.click();
      } else {
        showAlert(res.data?.result || "读取材料库失败", { title: "导出失败", tone: "error" });
      }
    } catch (e) {
      showAlert(e.message, { title: "导出异常", tone: "error" });
    }
  };

  // Import JSON
  const handleImportFile = async (e) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = async () => {
      try {
        const json = JSON.parse(reader.result);
        const res = await postJson("/api/material-lab/manage", {
          action: "import",
          library_json: json,
        });
        if (res.ok && res.data?.success) {
          showAlert("材料库已成功导入并刷新。", { title: "导入成功", tone: "success" });
          loadBootstrap();
        } else {
          showAlert(res.data?.result || "格式错误", { title: "导入失败", tone: "error" });
        }
      } catch (err) {
        showAlert(err.message, { title: "导入解析异常", tone: "error" });
      }
    };
    reader.readAsText(file);
  };

  // Generate Matrix Preview
  const handleGenerateMatrix = async () => {
    setMatrixBusy(true);
    setMatrixStatus("正在生成测试矩阵 G-code 与预览...");
    const payload = {
      material: selectedMaterial,
      thickness_mm: Number(selectedThickness),
      laser_mode: matrixLaserMode,
      matrix_kind: matrixLaserMode,
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
        setSelectedCellNum(null);
        setMatrixSendState(null);
        const est = resData.time_estimate?.estimated_minutes;
        setMatrixStatus(
          `测试矩阵已生成（${resData.cell_count || 0} 格${typeof est === "number" ? `，预估 ${est.toFixed(1)} 分钟` : ""}）；先发送实机测试，再点选效果最佳的单元格`,
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

  // Send Matrix to Controller
  const handleSendMatrix = async () => {
    if (!matrixId) {
      showAlert("请先生成测试矩阵", { title: "缺少测试矩阵", tone: "warning" });
      return;
    }
    if (!matrixHost.trim()) {
      showAlert("请填写设备 host/IP", { title: "缺少设备地址", tone: "warning" });
      return;
    }

    setMatrixBusy(true);
    setMatrixStatus("正在握手并下发标定矩阵...");
    try {
      const res = await postJson("/api/material-lab/send", {
        calibration_id: matrixId,
        confirmed: true,
        connection_mode: "network",
        network_host: matrixHost.trim(),
      });
      setMatrixBusy(false);
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

  // Poll calibration send progress until a terminal status
  const startMatrixPolling = (calibrationId) => {
    if (matrixPollRef.current) clearInterval(matrixPollRef.current);
    let failures = 0;
    matrixPollRef.current = setInterval(async () => {
      try {
        const res = await postJson("/api/material-lab/status", { calibration_id: calibrationId });
        if (!(res.ok && res.data?.success)) {
          failures += 1;
          if (failures >= 5) {
            clearInterval(matrixPollRef.current);
            matrixPollRef.current = null;
            setMatrixStatus("状态查询连续失败，已停止轮询");
          }
          return;
        }
        failures = 0;
        const record = res.data.result || {};
        const sr = record.send_result || {};
        const status = record.status || "sending";
        setMatrixSendState({ status, lines_sent: sr.lines_sent || 0, total_lines: sr.total_lines || 0, error: sr.error });
        if (status === "completed" || status === "failed" || status === "cancelled") {
          clearInterval(matrixPollRef.current);
          matrixPollRef.current = null;
          if (status === "completed") {
            setMatrixStatus(`标定矩阵加工完成 (${sr.lines_sent || 0}/${sr.total_lines || 0} 行已确认)，请观察样板并点选最佳单元格`);
          } else {
            setMatrixStatus(`标定矩阵${status === "cancelled" ? "已取消" : "发送失败"}${sr.error ? `: ${sr.error}` : ""}`);
          }
        } else {
          setMatrixStatus(`正在逐行下发 (${sr.lines_sent || 0}/${sr.total_lines || 0} 行)…`);
        }
      } catch (_e) {
        failures += 1;
        if (failures >= 5) {
          clearInterval(matrixPollRef.current);
          matrixPollRef.current = null;
          setMatrixStatus("网络异常，已停止状态轮询");
        }
      }
    }, 1000);
  };

  // Select Best Cell and Save
  const handleSaveBestCell = async (cellIndex) => {
    if (!matrixId) return;
    const cellNumber = cellIndex + 1;
    setSelectedCellNum(cellNumber);
    setMatrixStatus(`正在保存最佳单元格 #${cellNumber}...`);

    try {
      const res = await postJson("/api/material-lab/select-cell", {
        calibration_id: matrixId,
        cell_number: cellNumber,
        material: selectedMaterial,
        thickness_mm: Number(selectedThickness),
        laser_mode: matrixLaserMode,
      });
      if (res.ok && res.data?.success) {
        setMatrixStatus(`已成功将单元格 #${cellNumber} 记录为推荐参数！`);
        loadBootstrap();
      } else {
        setMatrixStatus(`保存单元格失败: ${res.data?.result || "错误"}`);
      }
    } catch (e) {
      setMatrixStatus(`保存单元格网络异常: ${e.message}`);
    }
  };

  // Filter materials
  const filteredMaterials = Object.keys(materials).filter((name) =>
    name.toLowerCase().includes(searchQuery.toLowerCase())
  );

  return (
    <div className="material-lab-page" style={{ padding: "16px 24px", maxWidth: "1440px", margin: "0 auto" }}>
      {/* Header */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "16px", borderBottom: "1px solid var(--line, #242b35)", paddingBottom: "12px" }}>
        <div>
          <div style={{ fontSize: "11px", letterSpacing: "2px", color: "var(--laser, #35e0ff)", textTransform: "uppercase" }}>
            02 · Material Laboratory
          </div>
          <h1 style={{ fontSize: "20px", fontWeight: "bold", margin: "2px 0 0", color: "var(--ink, #fff)" }}>
            材料参数与<span style={{ color: "var(--laser, #35e0ff)" }}>校准矩阵 Lab</span>
          </h1>
          {sourcePath && (
            <div style={{ fontSize: "12px", color: "var(--ink-dim, #94a3b8)", marginTop: "2px" }}>
              当前存储源: <span style={{ fontFamily: "var(--font-mono)", color: "var(--ink, #fff)" }}>{sourcePath}</span>
            </div>
          )}
        </div>

        <div style={{ display: "flex", gap: "8px" }}>
          <button className="btn btn-ghost" onClick={handleSaveRecent} title="从最近的工作流提取材料参数">
            <Bookmark size={13} style={{ marginRight: 4 }} />
            套用最近
          </button>
          <button className="btn btn-ghost" onClick={handleExport} title="导出材料库 JSON">
            <Download size={13} style={{ marginRight: 4 }} />
            导出库
          </button>
          <label className="btn btn-ghost" style={{ cursor: "pointer", display: "inline-flex", alignItems: "center" }} title="导入外部材料库">
            <Upload size={13} style={{ marginRight: 4 }} />
            导入
            <input type="file" accept=".json" onChange={handleImportFile} style={{ display: "none" }} />
          </label>
        </div>
      </div>

      {/* Three Column Grid */}
      <div style={{ display: "grid", gridTemplateColumns: "280px 1fr 1.1fr", gap: "16px", minHeight: "560px" }}>
        {/* Left Column: Material Tree */}
        <div
          className="hud-card"
          style={{
            padding: "12px",
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div style={{ display: "flex", gap: "6px", marginBottom: "10px" }}>
            <div style={{ position: "relative", flex: 1 }}>
              <Search size={14} style={{ position: "absolute", left: 8, top: 10, color: "var(--ink-dim, #94a3b8)" }} />
              <input
                type="text"
                placeholder="搜索材料名称..."
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="text-input"
                style={{ width: "100%", paddingLeft: "28px", fontSize: "12px" }}
              />
            </div>
            <button
              className="btn btn-hud-primary"
              onClick={() => {
                setMaterialModalData(null);
                setShowMaterialModal(true);
              }}
              title="添加新材料到材料库"
              style={{ padding: "4px 8px", fontSize: "12px", display: "flex", alignItems: "center", gap: "2px" }}
            >
              <Plus size={13} />
              <span>新建</span>
            </button>
          </div>

          <div style={{ flex: 1, overflowY: "auto", display: "flex", flexDirection: "column", gap: "4px" }}>
            {filteredMaterials.map((matName) => {
              const matObj = materials[matName];
              const thickKeys = Object.keys(matObj?.thicknesses || {});
              const isSelected = selectedMaterial === matName;

              return (
                <div key={matName} style={{ borderRadius: "4px", overflow: "hidden", border: isSelected ? "1px solid rgba(53,224,255,0.3)" : "1px solid transparent" }}>
                  <div
                    onClick={() => handleSelectMaterialAndThick(matName, thickKeys[0] || "3")}
                    style={{
                      padding: "6px 10px",
                      background: isSelected ? "rgba(53,224,255,0.12)" : "rgba(0,0,0,0.15)",
                      cursor: "pointer",
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "center",
                      fontWeight: isSelected ? "bold" : "normal",
                      color: isSelected ? "var(--laser, #35e0ff)" : "var(--ink, #fff)",
                      fontSize: "13px",
                    }}
                  >
                    <span>{matName}</span>
                    <span style={{ fontSize: "11px", color: "var(--ink-dim, #94a3b8)" }}>{thickKeys.length} 档厚度</span>
                  </div>

                  {isSelected && (
                    <div style={{ padding: "4px 8px", background: "rgba(0,0,0,0.25)", display: "flex", flexWrap: "wrap", gap: "4px" }}>
                      {thickKeys.map((th) => (
                        <button
                          key={th}
                          onClick={() => handleSelectMaterialAndThick(matName, th)}
                          style={{
                            background: selectedThickness === th ? "var(--laser, #35e0ff)" : "rgba(255,255,255,0.06)",
                            color: selectedThickness === th ? "#000" : "var(--ink, #fff)",
                            border: "none",
                            borderRadius: "3px",
                            padding: "2px 8px",
                            fontSize: "11px",
                            cursor: "pointer",
                            fontWeight: selectedThickness === th ? "bold" : "normal",
                          }}
                        >
                          {th} mm
                        </button>
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>

        {/* Middle Column: Parameter Editor */}
        <div
          className="hud-card"
          style={{
            padding: "16px",
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "12px" }}>
            <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
              <h3 style={{ margin: 0, fontSize: "15px", color: "var(--ink, #fff)" }}>
                {selectedMaterial} · {selectedThickness} mm 参数编辑
              </h3>
              <button
                className="btn btn-ghost"
                onClick={() => {
                  const curMat = materials[selectedMaterial];
                  const thicks = curMat?.thicknesses ? Object.keys(curMat.thicknesses) : [selectedThickness];
                  setMaterialModalData({
                    name: selectedMaterial,
                    thicknesses: thicks,
                    power_percent: Math.round(maxPower / 10),
                    speed_mms: Math.round(feedRate / 60),
                  });
                  setShowMaterialModal(true);
                }}
                title="编辑或删除此材料"
                style={{ fontSize: "11px", padding: "2px 8px", display: "flex", alignItems: "center", gap: "4px" }}
              >
                <Settings2 size={12} />
                <span>管理材料</span>
              </button>
            </div>
            {editorStatus && (
              <span style={{ fontSize: "12px", color: editorStatus.includes("失败") || editorStatus.includes("异常") ? "var(--danger)" : "var(--green, #10b981)" }}>
                {editorStatus}
              </span>
            )}
          </div>

          {/* Mode Tabs */}
          <div style={{ display: "flex", gap: "6px", marginBottom: "14px" }}>
            {[
              { id: "raster", label: "光栅雕刻 (Raster)" },
              { id: "outline", label: "轮廓扫描 (Outline)" },
              { id: "cut", label: "边缘切割 (Cut)" },
            ].map((m) => (
              <button
                key={m.id}
                onClick={() => handleModeChange(m.id)}
                className={`btn btn-ghost ${activeMode === m.id ? "active" : ""}`}
                style={{
                  flex: 1,
                  fontSize: "12px",
                  padding: "4px 8px",
                  background: activeMode === m.id ? "rgba(53,224,255,0.15)" : "transparent",
                  color: activeMode === m.id ? "var(--laser, #35e0ff)" : "var(--ink-dim, #94a3b8)",
                  border: activeMode === m.id ? "1px solid rgba(53,224,255,0.3)" : "1px solid var(--line, #242b35)",
                }}
              >
                {m.label}
              </button>
            ))}
          </div>

          {/* Form Fields */}
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "10px", flex: 1 }}>
            <div>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                最小激光功率 (0-1000)
              </label>
              <input
                type="number"
                value={minPower}
                onChange={(e) => setMinPower(Number(e.target.value))}
                className="text-input"
                style={{ width: "100%" }}
              />
            </div>

            <div>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                最大激光功率 (0-1000)
              </label>
              <input
                type="number"
                value={maxPower}
                onChange={(e) => setMaxPower(Number(e.target.value))}
                className="text-input"
                style={{ width: "100%" }}
              />
            </div>

            <div>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                加工进给速度 (mm/min)
              </label>
              <input
                type="number"
                value={feedRate}
                onChange={(e) => setFeedRate(Number(e.target.value))}
                className="text-input"
                style={{ width: "100%" }}
              />
            </div>

            <div>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                空移速度 (mm/min)
              </label>
              <input
                type="number"
                value={travelRate}
                onChange={(e) => setTravelRate(Number(e.target.value))}
                className="text-input"
                style={{ width: "100%" }}
              />
            </div>

            <div>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                加工循环次数 (Passes)
              </label>
              <input
                type="number"
                value={passes}
                onChange={(e) => setPasses(Number(e.target.value))}
                className="text-input"
                style={{ width: "100%" }}
              />
            </div>

            <div>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                点距 / 像素尺寸 (mm)
              </label>
              <input
                type="number"
                step="0.01"
                value={pixelSize}
                onChange={(e) => setPixelSize(Number(e.target.value))}
                className="text-input"
                style={{ width: "100%" }}
              />
            </div>

            <div style={{ gridColumn: "span 2" }}>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                二值阈值 (-1 为自动)
              </label>
              <input
                type="number"
                value={threshold}
                onChange={(e) => setThreshold(Number(e.target.value))}
                className="text-input"
                style={{ width: "100%" }}
              />
            </div>

            <div style={{ gridColumn: "span 2" }}>
              <label style={{ display: "block", fontSize: "11px", color: "var(--ink-dim)", marginBottom: "3px" }}>
                工艺与材质备注
              </label>
              <input
                type="text"
                value={notes}
                onChange={(e) => setNotes(e.target.value)}
                className="text-input"
                style={{ width: "100%" }}
                placeholder="例如：双面覆膜、浅色木纹..."
              />
            </div>
          </div>

          <div style={{ display: "flex", gap: "8px", marginTop: "16px" }}>
            <button className="btn btn-primary" onClick={handleSaveParams} style={{ flex: 1 }}>
              <Save size={14} style={{ marginRight: 4 }} />
              保存到材料库
            </button>
          </div>
        </div>

        {/* Right Column: Calibration Grid Matrix */}
        <div
          className="hud-card"
          style={{
            padding: "16px",
            display: "flex",
            flexDirection: "column",
          }}
        >
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "8px" }}>
            <h3 style={{ margin: 0, fontSize: "15px", color: "var(--ink, #fff)" }}>
              校准矩阵生成与实机测试
            </h3>
            <span style={{ fontSize: "12px", color: "var(--laser, #35e0ff)" }}>{matrixStatus}</span>
          </div>

          {/* Matrix Parameters */}
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: "8px", marginBottom: "10px" }}>
            <div>
              <label style={{ fontSize: "10px", color: "var(--ink-dim)" }}>功率区间 (Min-Max)</label>
              <div style={{ display: "flex", gap: "2px" }}>
                <input
                  type="number"
                  value={matrixPowerMin}
                  onChange={(e) => setMatrixPowerMin(Number(e.target.value))}
                  className="text-input"
                  style={{ width: "50%", padding: "4px" }}
                />
                <input
                  type="number"
                  value={matrixPowerMax}
                  onChange={(e) => setMatrixPowerMax(Number(e.target.value))}
                  className="text-input"
                  style={{ width: "50%", padding: "4px" }}
                />
              </div>
            </div>

            <div>
              <label style={{ fontSize: "10px", color: "var(--ink-dim)" }}>速度区间 (Min-Max)</label>
              <div style={{ display: "flex", gap: "2px" }}>
                <input
                  type="number"
                  value={matrixSpeedMin}
                  onChange={(e) => setMatrixSpeedMin(Number(e.target.value))}
                  className="text-input"
                  style={{ width: "50%", padding: "4px" }}
                />
                <input
                  type="number"
                  value={matrixSpeedMax}
                  onChange={(e) => setMatrixSpeedMax(Number(e.target.value))}
                  className="text-input"
                  style={{ width: "50%", padding: "4px" }}
                />
              </div>
            </div>

            <div>
              <label style={{ fontSize: "10px", color: "var(--ink-dim)" }}>矩阵行列 ({matrixRows}x{matrixCols})</label>
              <div style={{ display: "flex", gap: "2px" }}>
                <input
                  type="number"
                  value={matrixRows}
                  onChange={(e) => setMatrixRows(Number(e.target.value))}
                  className="text-input"
                  style={{ width: "50%", padding: "4px" }}
                />
                <input
                  type="number"
                  value={matrixCols}
                  onChange={(e) => setMatrixCols(Number(e.target.value))}
                  className="text-input"
                  style={{ width: "50%", padding: "4px" }}
                />
              </div>
            </div>
          </div>

          <div style={{ display: "flex", gap: "6px", marginBottom: "12px" }}>
            <input
              type="text"
              placeholder="设备 host / IP"
              value={matrixHost}
              onChange={(e) => {
                const val = e.target.value;
                setMatrixHost(val);
                localStorage.setItem("laser-forge-host", val);
              }}
              className="text-input"
              style={{ flex: 1, padding: "4px 8px" }}
            />
            <button className="btn btn-ghost" onClick={handleGenerateMatrix} disabled={matrixBusy}>
              <Grid size={13} style={{ marginRight: 4 }} />
              生成矩阵
            </button>
            <button className="btn btn-primary" onClick={handleSendMatrix} disabled={matrixBusy || !matrixId}>
              <Send size={13} style={{ marginRight: 4 }} />
              发送测试
            </button>
          </div>

          {/* Matrix interactive preview grid */}
          <div
            style={{
              flex: 1,
              background: "rgba(0,0,0,0.3)",
              borderRadius: "4px",
              border: "1px solid var(--line, #242b35)",
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              justifyContent: "center",
              padding: "10px",
              minHeight: "220px",
            }}
          >
            {matrixPreview ? (
              <div style={{ textAlign: "center" }}>
                <div style={{ fontSize: "12px", color: "var(--ink-dim)", marginBottom: "6px" }}>
                  点击下方单元格选择最佳测试效果并套用：
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
                      onClick={() => handleSaveBestCell(i)}
                      style={{
                        width: "44px",
                        height: "44px",
                        background: selectedCellNum === i + 1 ? "var(--laser, #35e0ff)" : "rgba(255,255,255,0.08)",
                        color: selectedCellNum === i + 1 ? "#000" : "#fff",
                        display: "flex",
                        flexDirection: "column",
                        alignItems: "center",
                        justifyContent: "center",
                        fontSize: "9px",
                        lineHeight: 1.15,
                        cursor: "pointer",
                        borderRadius: "2px",
                        border: "1px solid rgba(255,255,255,0.1)",
                      }}
                      title={`#${i + 1} ${cell.id}: S${cell.power} F${cell.speed} × ${cell.passes}`}
                    >
                      <span style={{ fontWeight: "bold", fontSize: "11px" }}>{i + 1}</span>
                      <span>S{cell.power}</span>
                      <span>F{cell.speed}</span>
                    </div>
                  ))}
                </div>
                {matrixSendState ? (
                  <div style={{ marginTop: "8px", fontSize: "11px", color: "var(--ink-dim)" }}>
                    发送状态: {matrixSendState.status} · {matrixSendState.lines_sent}/{matrixSendState.total_lines} 行
                    {matrixSendState.error ? ` · ${matrixSendState.error}` : ""}
                  </div>
                ) : null}
              </div>
            ) : (
              <div style={{ color: "var(--ink-dim)", textAlign: "center", fontSize: "12px" }}>
                <Grid size={28} style={{ opacity: 0.3, marginBottom: "4px" }} />
                <div>配置区间并点击【生成矩阵】预览校准网格</div>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Material Add / Edit / Delete Modal */}
      <MaterialModal
        visible={showMaterialModal}
        initialData={materialModalData}
        onClose={() => setShowMaterialModal(false)}
        onSaved={() => {
          setShowMaterialModal(false);
          loadBootstrap();
        }}
        onDeleted={() => {
          setShowMaterialModal(false);
          loadBootstrap();
        }}
      />
    </div>
  );
}
