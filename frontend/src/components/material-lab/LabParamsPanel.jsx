import React from "react";
import { RotateCcw, Save, Settings2, Trash2 } from "lucide-react";
import { formatPowerScaleHint, isSurfaceKey, modeOptionsForThickness, thicknessChipLabel } from "../../materialLabState.js";
import { FoldHeader } from "../FoldHeader.jsx";
import { PowerPercentInput } from "./PowerPercentInput.jsx";

/** 材料实验室 ② 材料参数：模式切换、参数编辑、管理材料与保存到材料库。原样搬自 pages/MaterialLabPage.jsx（2026-11-16 按职责拆分） */
export function LabParamsPanel({
  materials,
  readOnly,
  selectedMaterial,
  selectedThickness,
  currentMatThicknesses,
  hasSavedCell,
  isDirty = false,
  handleResetParams = null,
  editorStatus,
  activeMode,
  handleModeChange,
  handleDeleteCurrentThickness,
  handleSaveParams,
  setMaterialModalData,
  setShowMaterialModal,
  maxPower,
  setMaxPower,
  feedRate,
  setFeedRate,
  passes,
  setPasses,
  pixelSize,
  setPixelSize,
  aliases,
  setAliases,
  entryTitle,
  setEntryTitle,
  notes,
  setNotes,
}) {
  const modeOptions = modeOptionsForThickness(selectedThickness);
  return (
    <div className="hud-card ob-card lab-row-4">
      <FoldHeader id="lab-params" title="材料参数" n={2} />
      <div className="lab-row-5">
        <h3 className="lab-heading">
          <span>{selectedMaterial ? `${selectedMaterial} · ${thicknessChipLabel(selectedThickness)}` : "未选择材料"}</span>
          <small className={`lab-heading-tag${!readOnly && isDirty ? " is-dirty" : ""}`} id="lab-params-state-tag">
            {readOnly ? "只读参数" : isDirty ? "● 未保存修改" : hasSavedCell ? "已存参数" : "该模式未存（初始值）"}
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
              title={`仅删除 ${thicknessChipLabel(selectedThickness)} 档位`}
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
        {modeOptions.map((m) => (
          <button
            type="button"
            key={m.id}
            role="tab"
            aria-selected={activeMode === m.id}
            onClick={() => handleModeChange(m.id)}
            className={`lab-mode-tab ${activeMode === m.id ? "active" : ""}`}
          >
            <span>{m.label}</span>
          </button>
        ))}
      </div>

      {/* Form Fields */}
      <div className="lab-grid">
        <div>
          <label className="lab-label-2" htmlFor="lab-max-power">
            最大激光功率 (0–100%)
          </label>
          <PowerPercentInput
            id="lab-max-power"
            sValue={maxPower}
            onChangeSValue={setMaxPower}
            readOnly={readOnly}
            className="text-input lab-field-3"
          />
          <small className="lab-s-hint" id="lab-max-power-s">
            {formatPowerScaleHint(maxPower, { withScale: true })}
          </small>
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
            加工遍数
          </label>
          <input
            type="number"
            min={1}
            max={5}
            value={passes}
            onChange={(e) => setPasses(Number(e.target.value))}
            readOnly={readOnly}
            className="text-input lab-field-3"
          />
        </div>

        {activeMode === "raster" && (
          <div>
            <label className="lab-label-2" title="填充扫描线距 / 光栅点距，对应 LightBurn 的 Interval；可用线距测试标定">
              线距 / 点距 (mm)
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
        )}

        <div className="lab-box-3">
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
            条目标题（可选）
          </label>
          <input
            type="text"
            maxLength={60}
            value={entryTitle}
            onChange={(e) => setEntryTitle(e.target.value)}
            readOnly={readOnly}
            className="text-input lab-field-3"
            placeholder="例如：深色照片、浅色文字"
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
          {activeMode !== "cut" && !isSurfaceKey(selectedThickness) && (
            <button
              type="button"
              className="btn btn-ghost lab-btn-3"
              onClick={() => handleSaveParams(true)}
              disabled={!selectedMaterial}
              title="存为不分厚度的表面雕刻条目：任何厚度在没有同厚度参数时都用它（切割仍按真实厚度）"
            >
              存为表面通用
            </button>
          )}
          {isDirty && handleResetParams && (
            <button
              type="button"
              id="lab-reset-params"
              className="btn btn-ghost lab-btn-3"
              onClick={handleResetParams}
              title="放弃当前未保存的修改，还原为库内参数"
            >
              <RotateCcw size={13} className="lab-icon" />
              还原修改
            </button>
          )}
        </div>
      )}
    </div>
  );
}
