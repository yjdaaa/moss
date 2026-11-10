import { applyJobPoll, startJobPolling } from "../approvalJobMonitor.js";
import React, { useState, useEffect, useRef } from "react";
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
} from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { isRemoteMode } from "../authSession.js";
import { MaterialModal } from "../components/MaterialModal.jsx";
import { showAlert } from "../appDialog.js";
import { exportFile } from "../saveFile.js";
import { FoldHeader } from "../components/FoldHeader.jsx";

export function MaterialLabPage({ onNavigate }) {
  const [, setBootstrapData] = useState(null);
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
  const [matrixLaserMode] = useState("engrave");
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
      matrixPollRef.current?.();
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

  // 手机连接电脑 MOSS 时材料与校准结果只读（后端同样拒绝）：修改类按钮不显示，参数只能查看
  const readOnly = isRemoteMode();

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

  // One bounded request at a time; loss of contact is not a completed/failed job.
  const startMatrixPolling = (calibrationId) => {
    matrixPollRef.current?.();
    let state = { id:calibrationId, kind:"calibration", status:"sending", progress:null };
    const stop = startJobPolling(
      (signal) => postJson("/api/material-lab/status", { calibration_id:calibrationId }, { signal }),
      (res) => {
        state = applyJobPoll(state, calibrationId, "calibration", res);
        if (state.connectionLost) {
          setMatrixStatus("状态未知：暂时无法查询设备，正在重试；机器可能仍在运行，请到现场确认。");
          return;
        }
        const sr = res.data.result?.send_result || {};
        setMatrixSendState((previous) => ({ ...previous, status:state.status, lines_sent:state.progress?.sent ?? previous?.lines_sent ?? 0, total_lines:state.progress?.total ?? previous?.total_lines ?? 0, error:sr.error }));
        if (["completed", "failed", "cancelled"].includes(state.status)) {
          stop(); matrixPollRef.current = null;
          setMatrixStatus(state.status === "completed"
            ? `标定矩阵加工完成 (${sr.lines_sent || 0}/${sr.total_lines || 0} 行已确认)，请观察样板并点选最佳单元格`
            : `标定矩阵记录为${state.status === "cancelled" ? "已取消" : "发送失败"}，请确认设备状态${sr.error ? `: ${sr.error}` : ""}`);
        } else { setMatrixStatus(`正在逐行下发 (${state.progress?.sent || 0}/${state.progress?.total || 0} 行)…`); }
      },
    );
    matrixPollRef.current = stop;
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
          {!readOnly && (
            <button className="btn btn-ghost" onClick={handleSaveRecent} title="从最近的工作流提取材料参数">
              <Bookmark size={13} className="lab-icon" />
              套用最近
            </button>
          )}
          <button className="btn btn-ghost" onClick={handleExport} title="导出材料库 JSON">
            <Download size={13} className="lab-icon" />
            导出库
          </button>
          {!readOnly && (
            <label className="btn btn-ghost lab-label" title="导入外部材料库">
              <Upload size={13} className="lab-icon" />
              导入
              <input type="file" accept=".json" onChange={handleImportFile} className="lab-field" />
            </label>
          )}
        </div>
      </div>

      {/* 三栏：材料库表格、材料参数、校准矩阵（技术书 §9.5.5 B） */}
      <div className="ob-lab">
        {/* Left Column: Material Tree */}
        <div
          className="hud-card ob-card lab-row-2"
        >
          <FoldHeader id="lab-library" title="材料库" k="LIBRARY" n={1} />
          <div className="lab-row-3">
            <div className="lab-box-2">
              <Search size={14} className="lab-icon-2" />
              <input
                type="text"
                placeholder="搜索材料名称..."
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="text-input lab-field-2"
              />
            </div>
            {!readOnly && (
              <button
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
                {filteredMaterials.map((matName) => {
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
                })}
              </tbody>
            </table>
          </div>
        </div>

        {/* Middle Column: Parameter Editor */}
        <div
          className="hud-card ob-card lab-row-4"
        >
          <FoldHeader id="lab-params" title="材料参数" k="PARAMS" n={2} />
          <div className="lab-row-5">
            <h3 className="lab-heading">
              <span>{selectedMaterial} · {selectedThickness} mm</span>
              <small className="lab-heading-tag">{readOnly ? "只读参数" : "参数编辑"}</small>
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
              {!readOnly && (
                <button
                  type="button"
                  className="btn btn-ghost lab-btn-2"
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
                >
                  <Settings2 size={12} />
                  <span>管理材料</span>
                </button>
              )}
            </div>
          </div>

          {/* Mode Tabs */}
          <div className="lab-row-7" role="tablist" aria-label="加工模式">
            {[
              { id: "raster", label: "光栅雕刻", code: "Raster" },
              { id: "outline", label: "轮廓扫描", code: "Outline" },
              { id: "cut", label: "边缘切割", code: "Cut" },
            ].map((m) => (
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
                value={pixelSize}
                onChange={(e) => setPixelSize(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
              />
            </div>

            <div className="lab-box-3">
              <label className="lab-label-2">
                二值阈值 (-1 为自动)
              </label>
              <input
                type="number"
                value={threshold}
                onChange={(e) => setThreshold(Number(e.target.value))}
                readOnly={readOnly}
                className="text-input lab-field-3"
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
              <button className="btn btn-primary lab-btn-3" onClick={handleSaveParams}>
                <Save size={14} className="lab-icon" />
                保存到材料库
              </button>
            </div>
          )}
        </div>

        {/* Right Column: Calibration Grid Matrix */}
        <div
          className="hud-card ob-card lab-row-4"
        >
          <FoldHeader id="lab-matrix" title="校准矩阵" k="MATRIX" n={3} />
          <div className="lab-row-9">
            <h3 className="lab-heading-2">
              生成校准矩阵并实机测试
            </h3>
            <span className="lab-text">{matrixStatus}</span>
          </div>

          {/* Matrix Parameters */}
          <div className="lab-grid-2">
            <div>
              <label className="lab-label-3">功率区间 (Min-Max)</label>
              <div className="lab-row-10">
                <input
                  type="number"
                  value={matrixPowerMin}
                  onChange={(e) => setMatrixPowerMin(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
                <input
                  type="number"
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
                  value={matrixSpeedMin}
                  onChange={(e) => setMatrixSpeedMin(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
                <input
                  type="number"
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
                  value={matrixRows}
                  onChange={(e) => setMatrixRows(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
                <input
                  type="number"
                  value={matrixCols}
                  onChange={(e) => setMatrixCols(Number(e.target.value))}
                  className="text-input lab-field-4"
                />
              </div>
            </div>
          </div>

          <div className="lab-row-11">
            <input
              type="text"
              placeholder="设备 host / IP"
              value={matrixHost}
              onChange={(e) => {
                const val = e.target.value;
                setMatrixHost(val);
                localStorage.setItem("laser-forge-host", val);
              }}
              className="text-input lab-field-5"
            />
            <button className="btn btn-ghost" onClick={handleGenerateMatrix} disabled={matrixBusy}>
              <Grid size={13} className="lab-icon" />
              生成矩阵
            </button>
            <button className="btn btn-primary" onClick={handleSendMatrix} disabled={matrixBusy || !matrixId}>
              <Send size={13} className="lab-icon" />
              发送测试
            </button>
          </div>

          {/* Matrix interactive preview grid */}
          <div
            className="lab-row-12"
          >
            {matrixPreview ? (
              <div className="lab-box-4">
                <div className="lab-box-5">
                  {readOnly ? "校准结果（保存最佳单元格请在电脑上操作）：" : "点击下方单元格选择最佳测试效果并套用："}
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
