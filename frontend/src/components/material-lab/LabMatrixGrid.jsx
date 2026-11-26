import React from "react";
import { Grid } from "lucide-react";
import { TEST_AXIS_OPTIONS, cellAxisText, powerScaleToPercent } from "../../materialLabState.js";

const TEST_AXIS_LABELS = Object.fromEntries(TEST_AXIS_OPTIONS.map((a) => [a.id, `${a.label} ${a.unit}`]));

/** 材料实验室 ③ 校准矩阵 · 预览网格：点选最佳单元格存为推荐参数，并显示发送状态。原样搬自 pages/MaterialLabPage.jsx（2026-11-16 按职责拆分） */
export function LabMatrixGrid({
  matrixPreview,
  matrixCols,
  readOnly,
  selectedMaterial,
  selectedThickness,
  selectedCellNum,
  handleSaveBestCell,
  matrixSendState,
}) {
  return (
    <div className="lab-row-12">
      {matrixPreview ? (
        <div className="lab-box-4">
          <div className="lab-box-5">
            {matrixPreview.wizard_pattern
              ? "标定向导的测试图案：刻完后在上方向导里填写测量结果（不按格选参数）"
              : readOnly
              ? "校准结果（保存最佳单元格请在电脑上操作）："
              : `点击下方单元格将最佳效果存为「${matrixPreview.material || selectedMaterial} ${
                matrixPreview.surface_entry ? "表面通用" : `${matrixPreview.thickness_mm ?? selectedThickness}mm`
              }」推荐参数：`}
          </div>
          {matrixPreview.test_type !== "interval" && !matrixPreview.wizard_pattern && matrixPreview.x_axis ? (
            <div className="lab-test-legend">
              列：{TEST_AXIS_LABELS[matrixPreview.x_axis] || matrixPreview.x_axis} · 行：
              {TEST_AXIS_LABELS[matrixPreview.y_axis] || matrixPreview.y_axis}（预览第一行即样板最下面一行 A）
            </div>
          ) : null}
          <div className="lab-matrix-grid-scroll">
            <div
              className="lab-matrix-grid"
              style={{ "--lab-matrix-cols": matrixPreview.columns || matrixCols }}
            >
              {(matrixPreview.cells || []).map((cell, i) => {
                const interactive = !readOnly && !matrixPreview.wizard_pattern;
                const selected = selectedCellNum === i + 1;
                return (
                  <button
                    type="button"
                    key={cell.id || i}
                    disabled={!interactive}
                    aria-pressed={interactive ? selected : undefined}
                    onClick={interactive ? () => handleSaveBestCell(i) : undefined}
                    className={`lab-matrix-cell${selected ? " is-selected" : ""}`}
                    title={`#${i + 1} ${cell.id}: ${powerScaleToPercent(cell.power)}%（S${cell.power}） F${cell.speed} × ${cell.passes}${
                      cell.line_interval_mm != null ? ` · 线距 ${cell.line_interval_mm} mm` : ""
                    }${cell.run_index ? ` · 第 ${cell.run_index} 个加工` : ""}`}
                  >
                    <span className="lab-text-2">{i + 1}</span>
                    <span>
                      {matrixPreview.wizard_pattern ? cell.x_label : cellAxisText(cell, matrixPreview.x_axis || "power", "x")}
                    </span>
                    {matrixPreview.test_type === "interval" || matrixPreview.wizard_pattern ? null : (
                      <span>{cellAxisText(cell, matrixPreview.y_axis || "speed", "y")}</span>
                    )}
                  </button>
                );
              })}
            </div>
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
  );
}
