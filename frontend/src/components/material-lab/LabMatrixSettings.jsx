import React from "react";
import { MATRIX_MODE_OPTIONS, TEST_TYPE_OPTIONS, axisOptionsForMode, thicknessChipLabel } from "../../materialLabState.js";
import { PowerPercentInput } from "./PowerPercentInput.jsx";

/** 材料实验室 ③ 校准矩阵 · 标定条件：标题与模式、状态行、功率 / 速度区间、行列、测试类型与轴参数。原样搬自 pages/MaterialLabPage.jsx（2026-11-16 按职责拆分） */
export function LabMatrixSettings({
  selectedMaterial,
  selectedThickness,
  matrixMode,
  matrixModeInfo,
  changeMatrixMode,
  matrixStatus,
  matrixPowerMin,
  setMatrixPowerMin,
  matrixPowerMax,
  setMatrixPowerMax,
  matrixSpeedMin,
  setMatrixSpeedMin,
  matrixSpeedMax,
  setMatrixSpeedMax,
  matrixRows,
  setMatrixRows,
  matrixCols,
  setMatrixCols,
  matrixTestType,
  setMatrixTestType,
  matrixIntervalMin,
  setMatrixIntervalMin,
  matrixIntervalMax,
  setMatrixIntervalMax,
  matrixIntervalCount,
  setMatrixIntervalCount,
  matrixXAxis,
  setMatrixXAxis,
  matrixYAxis,
  setMatrixYAxis,
  matrixPassesMin,
  setMatrixPassesMin,
  matrixPassesMax,
  setMatrixPassesMax,
  matrixLabels,
  setMatrixLabels,
}) {
  return (
    <>
      <div className="lab-row-9">
        <h3 className="lab-heading-2">
          {selectedMaterial ? `${selectedMaterial} · ${thicknessChipLabel(selectedThickness)} · ${matrixModeInfo.label}` : "生成校准矩阵并实机测试"}
        </h3>
        <select
          value={matrixMode}
          onChange={(e) => changeMatrixMode(e.target.value)}
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
          <label className="lab-label-3">功率区间 (0–100%)</label>
          <div className="lab-row-10">
            <PowerPercentInput
              sValue={matrixPowerMin}
              onChangeSValue={setMatrixPowerMin}
              className="text-input lab-field-4"
              ariaLabel="最小功率百分比"
            />
            <PowerPercentInput
              sValue={matrixPowerMax}
              onChangeSValue={setMatrixPowerMax}
              className="text-input lab-field-4"
              ariaLabel="最大功率百分比"
            />
          </div>
          <small className="lab-s-hint" id="lab-matrix-power-s">
            对应 S{Number.isFinite(Number(matrixPowerMin)) ? Math.round(Number(matrixPowerMin)) : "—"}~S
            {Number.isFinite(Number(matrixPowerMax)) ? Math.round(Number(matrixPowerMax)) : "—"}
          </small>
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

      {/* 测试类型与轴参数（2026-11-16 对齐 LightBurn Material Test / Interval Test）。
          放在 .lab-grid-2 之外，保持上面区间与行列输入框的顺序不变 */}
      <div className="lab-test-axes">
        <div className="lab-axis-field">
          <label className="lab-label-3" htmlFor="lab-test-type">测试类型</label>
          <select
            id="lab-test-type"
            value={matrixTestType}
            onChange={(e) => setMatrixTestType(e.target.value)}
            className="select-input lab-axis-select"
            aria-label="测试类型"
          >
            {TEST_TYPE_OPTIONS.filter((t) => t.id !== "interval" || matrixMode === "raster").map((t) => (
              <option key={t.id} value={t.id}>{t.label}</option>
            ))}
          </select>
        </div>
        {matrixTestType === "interval" ? (
          <>
            <div className="lab-axis-field">
              <label className="lab-label-3">线距起止 (mm)</label>
              <div className="lab-row-10">
                <input
                  type="number"
                  step="0.01"
                  min="0.01"
                  value={matrixIntervalMin}
                  onChange={(e) => setMatrixIntervalMin(Number(e.target.value))}
                  className="text-input lab-field-4"
                  aria-label="起始线距"
                />
                <input
                  type="number"
                  step="0.01"
                  min="0.01"
                  value={matrixIntervalMax}
                  onChange={(e) => setMatrixIntervalMax(Number(e.target.value))}
                  className="text-input lab-field-4"
                  aria-label="结束线距"
                />
              </div>
            </div>
            <div className="lab-axis-field">
              <label className="lab-label-3" htmlFor="lab-interval-count">方块数</label>
              <input
                id="lab-interval-count"
                type="number"
                min={2}
                max={20}
                value={matrixIntervalCount}
                onChange={(e) => setMatrixIntervalCount(Number(e.target.value))}
                className="text-input lab-field-4"
                aria-label="线距测试方块数"
              />
            </div>
          </>
        ) : (
          <>
            <div className="lab-axis-field">
              <label className="lab-label-3" htmlFor="lab-x-axis">列参数</label>
              <select
                id="lab-x-axis"
                value={matrixXAxis}
                onChange={(e) => setMatrixXAxis(e.target.value)}
                className="select-input lab-axis-select"
                aria-label="网格列参数"
              >
                {axisOptionsForMode(matrixMode).map((a) => (
                  <option key={a.id} value={a.id}>{a.label}</option>
                ))}
              </select>
            </div>
            <div className="lab-axis-field">
              <label className="lab-label-3" htmlFor="lab-y-axis">行参数</label>
              <select
                id="lab-y-axis"
                value={matrixYAxis}
                onChange={(e) => setMatrixYAxis(e.target.value)}
                className="select-input lab-axis-select"
                aria-label="网格行参数"
              >
                {axisOptionsForMode(matrixMode).map((a) => (
                  <option key={a.id} value={a.id}>{a.label}</option>
                ))}
              </select>
            </div>
            {(matrixXAxis === "interval" || matrixYAxis === "interval") && (
              <div className="lab-axis-field">
                <label className="lab-label-3">线距区间 (mm)</label>
                <div className="lab-row-10">
                  <input
                    type="number"
                    step="0.01"
                    min="0.01"
                    value={matrixIntervalMin}
                    onChange={(e) => setMatrixIntervalMin(Number(e.target.value))}
                    className="text-input lab-field-4"
                    aria-label="线距下限"
                  />
                  <input
                    type="number"
                    step="0.01"
                    min="0.01"
                    value={matrixIntervalMax}
                    onChange={(e) => setMatrixIntervalMax(Number(e.target.value))}
                    className="text-input lab-field-4"
                    aria-label="线距上限"
                  />
                </div>
              </div>
            )}
            {(matrixXAxis === "passes" || matrixYAxis === "passes") && (
              <div className="lab-axis-field">
                <label className="lab-label-3">遍数区间</label>
                <div className="lab-row-10">
                  <input
                    type="number"
                    min={1}
                    max={10}
                    value={matrixPassesMin}
                    onChange={(e) => setMatrixPassesMin(Number(e.target.value))}
                    className="text-input lab-field-4"
                    aria-label="遍数下限"
                  />
                  <input
                    type="number"
                    min={1}
                    max={10}
                    value={matrixPassesMax}
                    onChange={(e) => setMatrixPassesMax(Number(e.target.value))}
                    className="text-input lab-field-4"
                    aria-label="遍数上限"
                  />
                </div>
              </div>
            )}
          </>
        )}
        <label className="lab-test-labels">
          <input
            type="checkbox"
            checked={matrixLabels}
            onChange={(e) => setMatrixLabels(e.target.checked)}
          />
          刻行列标注
        </label>
      </div>
      <div className="lab-test-hint">
        {matrixTestType === "interval"
          ? "线距测试：一排填充小方块，每块线距不同、旁边刻数值；选扫描线刚好相接、没有缝也不重叠的那块（建议从 0.08~0.16 mm 起）。"
          : "按烧蚀风险从低到高加工：高速、低功率、宽线距、少遍数的格子先刻。"}
      </div>
    </>
  );
}
