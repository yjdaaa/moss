import React from "react";
import { RefreshCw } from "lucide-react";
import { isReadOnlyReferenceClient } from "../../authSession.js";
import {
  DRAW_IMAGE_STRATEGIES,
  availableDrawVectorStyles,
  effectiveDrawVectorStyle,
  normalizeDrawImageStrategy,
} from "../../drawConnection.js";
import { materialLayerHint } from "../../fillOptions.js";
import { CUSTOM_OPTION_LABEL, CUSTOM_OPTION_VALUE, findMaterialOption } from "../../materialSelectState.js";
import { IMAGE_PRESET_HINTS, detectImagePreset } from "../../qualityPresets.js";
import {
  IMAGE_INTERVAL_SCALE_LIMITS,
  IMAGE_MODE_OPTIONS,
  MANUAL_LIMITS,
  imageModeFields,
  normalizeImageMode,
} from "../../workbenchParams.js";
import EngraveRouteSwitch from "../EngraveRouteSwitch.jsx";
import FillOptionsPanel from "../FillOptionsPanel.jsx";
import QualityPresetBar from "../QualityPresetBar.jsx";
import { FoldHeader } from "../FoldHeader.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";

/**
 * 创意画板 ① 材料与尺寸卡：材料库 / 自定义材料、厚度、尺寸、雕刻路线、填充设置与这次自定义功率速度。原样搬自 pages/DrawPage.jsx（2026-11-16 按职责拆分）。
 * 2026-11-17（技术书 r45）：“生成策略”下拉去掉矢量，改为雕刻路线开关（与工作台、语音同一个值）；矢量时显示只刻选中的图形与
 * 填充设置，转图片时选“转图片方式”（光栅扫描 / 轮廓描边），光栅扫描另有转换方式与质量预设。`drawMode` 是实际请求的 mode。
 */
export function DrawMaterialCard({
  form,
  updateForm,
  drawMode,
  engraveRoute,
  setEngraveRoute,
  engraveRouteSaving,
  engraveRouteReadOnly,
  applyImageQualityPreset,
  materialLibrary,
  materialMode,
  materialSelectValue,
  handleMaterialSelectChange,
  handleThicknessSelectChange,
  handleReturnToLibrary,
  materialRefreshBusy,
  refreshMaterialLibrary,
  selectedOnly,
  setSelectedOnly,
  selectionKey,
  hasSceneImages = false,
  fillSettings,
  updateFillSettings,
  materialApplied,
  materialLayer,
  saveFillToMaterial,
  hasRec,
  recPowerPct,
  recFeedMpm,
  recPasses,
  customParams,
  setCustomParams,
  customPower,
  setCustomPower,
  customFeed,
  setCustomFeed,
  customPasses,
  setCustomPasses,
}) {
  const matchedMaterial =
    materialMode === "library" ? findMaterialOption(materialLibrary, form.material) : null;
  const isSurfaceOnly = Boolean(
    matchedMaterial && (matchedMaterial.thicknesses || []).length === 0 && matchedMaterial.surface,
  );
  const vectorStyles = availableDrawVectorStyles({ hasSceneImages, surfaceOnly: isSurfaceOnly });
  const effectiveVectorStyle = effectiveDrawVectorStyle(form.vector_style, {
    hasSceneImages,
    surfaceOnly: isSurfaceOnly,
  });
  const rasterImageMode = normalizeImageMode(form.image_mode);
  const rasterModeFields = imageModeFields(rasterImageMode);
  return (
    <WbCard id="draw-material" className="hud-card ob-card control-panel" label="材料与尺寸">
      <FoldHeader id="draw-material" title="材料与尺寸" n={1} />
      <div className="material-library-label">
        <span>材料库</span>
        <div className="material-library-row">
          <select
            value={materialSelectValue}
            onChange={(event) => handleMaterialSelectChange(event.target.value)}
          >
            {materialLibrary.map((item) => (
              <option key={item.name} value={item.name}>
                {item.name}
              </option>
            ))}
            <option value={CUSTOM_OPTION_VALUE}>{CUSTOM_OPTION_LABEL}</option>
          </select>
          <button
            type="button"
            className="ghost-button icon-button"
            title="刷新材料库"
            aria-label="刷新材料库"
            disabled={materialRefreshBusy}
            onClick={refreshMaterialLibrary}
          >
            <RefreshCw size={16} />
          </button>
        </div>
      </div>

      {materialMode === "custom" ? (
        <label>
          材料名称
          <input
            value={form.material}
            onChange={(event) => updateForm("material", event.target.value)}
          />
        </label>
      ) : null}

      {(() => {
        if (!matchedMaterial) {
          return (
            <label>
              厚度 mm
              <input
                type="number"
                min="0"
                step="0.1"
                value={form.thickness_mm}
                onChange={(event) => updateForm("thickness_mm", event.target.value)}
              />
            </label>
          );
        }
        if (isSurfaceOnly) return null;
        const thicknesses = matchedMaterial.thicknesses || [];
        return (
          <label>
            厚度 mm
            <select
              value={form.thickness_mm}
              onChange={(event) => handleThicknessSelectChange(event.target.value)}
            >
              {thicknesses.length > 0 ? (
                thicknesses.map((value) => (
                  <option key={String(value)} value={String(value)}>
                    {value}
                  </option>
                ))
              ) : (
                <option value={String(form.thickness_mm || 3)}>
                  {`${form.thickness_mm || 3} mm`}
                </option>
              )}
            </select>
          </label>
        );
      })()}

      {materialMode === "custom" && materialLibrary.length > 0 ? (
        <button type="button" className="ghost-button" onClick={handleReturnToLibrary}>
          返回材料库
        </button>
      ) : null}

      <div className="dimension-grid">
        <label>
          宽 mm
          <input
            type="number"
            min="0"
            max="100"
            step="0.1"
            value={form.width_mm}
            onChange={(event) => updateForm("width_mm", event.target.value)}
            placeholder="自动 (≤100)"
          />
        </label>
        <label>
          高 mm
          <input
            type="number"
            min="0"
            max="100"
            step="0.1"
            value={form.height_mm}
            onChange={(event) => updateForm("height_mm", event.target.value)}
            placeholder="自动 (≤100)"
          />
        </label>
      </div>

      <EngraveRouteSwitch
        idPrefix="draw"
        route={engraveRoute}
        onChange={setEngraveRoute}
        saving={engraveRouteSaving}
        readOnly={engraveRouteReadOnly}
      />

      {/* 刻法（r46）：按雕刻路线只列能用的——矢量：轮廓 / 填充；转图片：轮廓（描深色边缘）/ 填充（逐行扫描） */}
      {drawMode === "vector" ? (
        hasSceneImages ? (
          <>
            <div className="wb-params-hint" id="draw-vector-image-note">
              画布含有插入的图片，生成时将自动按光栅整幅扫描。
            </div>
            <FillOptionsPanel idPrefix="draw-scan" settings={fillSettings} onChange={updateFillSettings} scanOnly />
          </>
        ) : (
          <>
            <label title="只影响闭合图形（矩形、菱形、椭圆、首尾相接的线）与文字；手绘、直线和箭头始终刻一笔">
              刻法
              <select
                id="draw-vector-style"
                value={effectiveVectorStyle}
                onChange={(event) => updateForm("vector_style", event.target.value)}
              >
                {vectorStyles.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>
            </label>
            <label
              className="checkbox-row draw-label"
              title="只刻画布上选中的图形，位置保持在整幅画里的位置（补刻、重刻某一部分时能对上）"
            >
              <input
                type="checkbox"
                id="draw-selected-only"
                checked={selectedOnly}
                onChange={(event) => setSelectedOnly(event.target.checked)}
              />
              只刻选中的图形（位置与整幅一致）
              {selectedOnly ? `：已选 ${selectionKey ? selectionKey.split(",").length : 0} 个` : ""}
            </label>
            <FillOptionsPanel
              idPrefix="draw"
              settings={fillSettings}
              onChange={updateFillSettings}
              withFill={effectiveVectorStyle === "fill"}
              withCut={effectiveVectorStyle === "cut"}
              materialHint={
                effectiveVectorStyle === "fill" || effectiveVectorStyle === "cut"
                  ? materialLayerHint(materialApplied, materialLayer?.label)
                  : ""
              }
              materialEntryLabel={materialLayer?.label || ""}
              onSaveToMaterial={
                !isReadOnlyReferenceClient() && materialLayer && (effectiveVectorStyle === "fill" || effectiveVectorStyle === "cut")
                  ? saveFillToMaterial
                  : null
              }
            />
          </>
        )
      ) : (
        <label>
          刻法
          <select
            id="draw-image-strategy"
            value={normalizeDrawImageStrategy(form.image_strategy)}
            onChange={(event) => updateForm("image_strategy", event.target.value)}
          >
            {DRAW_IMAGE_STRATEGIES.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
        </label>
      )}

      {drawMode === "raster" ? (
        <>
          <QualityPresetBar
            id="draw-image-preset"
            value={detectImagePreset(rasterImageMode, form.line_interval_scale)}
            hints={IMAGE_PRESET_HINTS}
            onSelect={applyImageQualityPreset}
          />
          <label>
            转换方式
            <select
              id="draw-image-mode"
              value={rasterImageMode}
              onChange={(event) => updateForm("image_mode", event.target.value)}
            >
              {IMAGE_MODE_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>{opt.label}</option>
              ))}
            </select>
          </label>
          <label>
            行间距倍数
            <input
              id="draw-image-interval-scale"
              type="number"
              min={IMAGE_INTERVAL_SCALE_LIMITS.min}
              max={IMAGE_INTERVAL_SCALE_LIMITS.max}
              step="0.1"
              value={form.line_interval_scale}
              onChange={(event) => updateForm("line_interval_scale", event.target.value)}
            />
          </label>
          {rasterModeFields.halftone ? (
            <div className="dimension-grid">
              <label>
                网点网格 mm
                <input
                  id="draw-image-halftone-cell"
                  type="number"
                  min="0.2"
                  max="5"
                  step="0.1"
                  value={form.halftone_cell_mm}
                  onChange={(event) => updateForm("halftone_cell_mm", event.target.value)}
                />
              </label>
              <label>
                网点角度 °
                <input
                  id="draw-image-halftone-angle"
                  type="number"
                  min="0"
                  max="90"
                  step="1"
                  value={form.halftone_angle_deg}
                  onChange={(event) => updateForm("halftone_angle_deg", event.target.value)}
                />
              </label>
            </div>
          ) : null}
          {rasterModeFields.grayscale ? (
            <label>
              灰度最小功率 %
              <input
                id="draw-image-gray-min"
                type="number"
                min="0"
                max="100"
                step="1"
                value={form.gray_min_power_percent}
                onChange={(event) => updateForm("gray_min_power_percent", event.target.value)}
              />
            </label>
          ) : null}
          <FillOptionsPanel idPrefix="draw-scan" settings={fillSettings} onChange={updateFillSettings} scanOnly />
        </>
      ) : null}

      {drawMode === "outline" ? (
        <label>
          轮廓平滑度（0.25 精细 ～ 8 简化）
          <input
            type="number"
            min="0.25"
            max="8"
            step="0.05"
            value={form.vector_simplify_factor}
            onChange={(event) => updateForm("vector_simplify_factor", event.target.value)}
            placeholder="默认 3"
          />
        </label>
      ) : null}

      <div className="wb-params" id="draw-params">
        <label className="wb-params-switch checkbox-row draw-label">
          <input
            type="checkbox"
            id="draw-custom-params"
            checked={customParams}
            onChange={(event) => {
              const on = event.target.checked;
              setCustomParams(on);
              if (on) {
                setCustomPower(String(recPowerPct));
                setCustomFeed(String(recFeedMpm));
                setCustomPasses(String(Math.max(1, Math.min(MANUAL_LIMITS.passes.max, recPasses))));
              }
            }}
          />
          <span>这次自定义功率和速度</span>
        </label>
        {customParams ? (
          <>
            <div className="wb-params-grid">
              <label>
                <span>功率 %</span>
                <input
                  className="text-input"
                  id="draw-custom-power"
                  type="number"
                  inputMode="decimal"
                  min={MANUAL_LIMITS.power.min}
                  max={MANUAL_LIMITS.power.max}
                  value={customPower}
                  onChange={(e) => setCustomPower(e.target.value)}
                />
              </label>
              <label>
                <span>速度 mm/min</span>
                <input
                  className="text-input"
                  id="draw-custom-feed"
                  type="number"
                  inputMode="numeric"
                  step="10"
                  min={MANUAL_LIMITS.feed.min}
                  max={MANUAL_LIMITS.feed.max}
                  value={customFeed}
                  onChange={(e) => setCustomFeed(e.target.value)}
                />
              </label>
              <label>
                <span>遍数</span>
                <input
                  className="text-input"
                  id="draw-custom-passes"
                  type="number"
                  inputMode="numeric"
                  min={MANUAL_LIMITS.passes.min}
                  max={MANUAL_LIMITS.passes.max}
                  value={customPasses}
                  onChange={(e) => setCustomPasses(e.target.value)}
                />
              </label>
            </div>
            <div className="wb-params-hint">只影响这一次生成，不写回材料库；重新生成预览后生效。</div>
          </>
        ) : hasRec ? (
          <div className="wb-params-hint" id="draw-params-recommended">
            材料库推荐：功率 {recPowerPct}% · 速度 {recFeedMpm} mm/min · {recPasses} 遍
          </div>
        ) : (
          <div className="wb-params-hint" id="draw-params-recommended">
            材料库推荐：暂无匹配参数（可勾选自定义，或前往材料实验室补充）
          </div>
        )}
      </div>

    </WbCard>
  );
}
