import React from "react";
import { Sliders } from "lucide-react";
import { getJson } from "../../api/client.js";
import { isRemoteMode } from "../../authSession.js";
import { showAlert } from "../../appDialog.js";
import { pickThicknessForMaterial } from "../../materialSelectState.js";
import { MANUAL_LIMITS, materialRecommendationModes } from "../../workbenchParams.js";
import { FoldHeader } from "../FoldHeader.jsx";
import { WbCard } from "../WorkbenchCanvas.jsx";

/** 雕刻工作台 ③ 材质与参数卡：材料 / 厚度联动、库内参数修改入口与这次自定义功率速度。原样搬自 pages/HomePage.jsx（2026-11-16 按职责拆分） */
export function WorkbenchMaterialCard({
  activeTool,
  textStyle,
  materials,
  selectedMaterial,
  setSelectedMaterial,
  selectedThickness,
  setSelectedThickness,
  availableThicknesses,
  powerPercent,
  recommendedFeedRate,
  recommendedPasses,
  hasRecommendation,
  setMaterialModalData,
  setShowMaterialModal,
  customParams,
  setCustomParams,
  customPower,
  setCustomPower,
  customFeed,
  setCustomFeed,
  customPasses,
  setCustomPasses,
}) {
  return (
    <WbCard id="material" className="hud-card ob-card" label="材质与参数">
      <FoldHeader id="wb-material" title="材质与参数" n={3} />
        {/* Material & Specs selection Deck */}
        <div
          className="wb-box-6"
        >
          <div className="wb-row-5">
            <span className="wb-row-6">
              <span className="wb-text-3" />
              加工材质与物理参数联动
            </span>
            {!isRemoteMode() && (
              <button
                className="btn btn-ghost wb-btn-3"
                onClick={async () => {
                  const recMode = materialRecommendationModes({ activeTool, textStyle });
                  let existingThicknesses;
                  let fullThickList = availableThicknesses;
                  try {
                    const exportRes = await getJson("/api/material-lab/export");
                    const mats = exportRes.data?.result?.library?.materials;
                    if (!exportRes.ok || !exportRes.data?.success || !mats || typeof mats !== "object") {
                      showAlert("读取材料库完整规格失败，请稍后重试", { title: "读取失败", tone: "warning" });
                      return;
                    }
                    const matEntry = mats[selectedMaterial];
                    if (matEntry?.thicknesses && typeof matEntry.thicknesses === "object") {
                      existingThicknesses = matEntry.thicknesses;
                      const keys = Object.keys(matEntry.thicknesses);
                      if (keys.length > 0) {
                        fullThickList = keys;
                      }
                    } else {
                      existingThicknesses = {};
                    }
                  } catch (err) {
                    showAlert(`读取材料库异常：${err.message}`, { title: "读取失败", tone: "warning" });
                    return;
                  }
                  setMaterialModalData({
                    name: selectedMaterial,
                    thicknesses: fullThickList,
                    power_percent: powerPercent,
                    feed_rate: recommendedFeedRate,
                    passes: recommendedPasses,
                    laser_mode: recMode.laserMode,
                    engraving_mode: recMode.engravingMode || "cut",
                    existing_thicknesses: existingThicknesses,
                    target_thickness: selectedThickness,
                  });
                  setShowMaterialModal(true);
                }}
              >
                <Sliders size={12} className="wb-icon" />
                调整库内参数
              </button>
            )}
          </div>

          <div className="wb-grid-2">
            <div>
              <label className="wb-label-3">
                材料类型
              </label>
              <select
                value={selectedMaterial}
                onChange={(e) => {
                  const nextMat = e.target.value;
                  setSelectedMaterial(nextMat);
                  const matObj = materials.find((m) => m.name === nextMat);
                  if (matObj) {
                    setSelectedThickness(pickThicknessForMaterial(matObj, selectedThickness));
                  }
                }}
                className="select-input wb-field-7"
              >
                {materials.length === 0 ? (
                  <option value={selectedMaterial}>{selectedMaterial || "暂无库内材质"}</option>
                ) : (
                  materials.map((m) => (
                    <option key={m.name} value={m.name}>{m.name}</option>
                  ))
                )}
              </select>
            </div>

            <div>
              <label className="wb-label-3">
                厚度规格
              </label>
              <select
                value={selectedThickness}
                onChange={(e) => setSelectedThickness(e.target.value)}
                className="select-input wb-field-8"
              >
                {availableThicknesses.length > 0 ? (
                  availableThicknesses.map((th) => (
                    <option key={th} value={String(th)}>{th} mm</option>
                  ))
                ) : (
                  <option value={String(selectedThickness || 3)}>
                    {materials.find((m) => m.name === selectedMaterial)?.surface
                      ? `表面通用（${selectedThickness || 3} mm）`
                      : `${selectedThickness || 3} mm`}
                  </option>
                )}
              </select>
            </div>
          </div>

          <div className="wb-params" id="wb-params">
            <label className="wb-params-switch">
              <input
                type="checkbox"
                id="wb-custom-params"
                checked={customParams}
                disabled={activeTool === "gcode"}
                onChange={(e) => {
                  const on = e.target.checked;
                  setCustomParams(on);
                  if (on) {
                    setCustomPower(String(Math.round(powerPercent)));
                    setCustomFeed(String(Math.round(recommendedFeedRate)));
                    setCustomPasses(String(Math.max(1, Math.min(MANUAL_LIMITS.passes.max, recommendedPasses))));
                  }
                }}
              />
              <span>这次自定义功率和速度</span>
            </label>
            {activeTool === "gcode" ? (
              <div className="wb-params-hint">上传的刀路文件按文件里的参数加工，这里不生效。</div>
            ) : customParams ? (
              <>
                <div className="wb-params-grid">
                  <label>
                    <span>功率 %</span>
                    <input className="text-input" id="wb-custom-power" type="number" inputMode="decimal"
                      min={MANUAL_LIMITS.power.min} max={MANUAL_LIMITS.power.max} value={customPower}
                      onChange={(e) => setCustomPower(e.target.value)} />
                  </label>
                  <label>
                    <span>速度 mm/min</span>
                    <input className="text-input" id="wb-custom-feed" type="number" inputMode="numeric" step="10"
                      min={MANUAL_LIMITS.feed.min} max={MANUAL_LIMITS.feed.max} value={customFeed}
                      onChange={(e) => setCustomFeed(e.target.value)} />
                  </label>
                  <label>
                    <span>遍数</span>
                    <input className="text-input" id="wb-custom-passes" type="number" inputMode="numeric"
                      min={MANUAL_LIMITS.passes.min} max={MANUAL_LIMITS.passes.max} value={customPasses}
                      onChange={(e) => setCustomPasses(e.target.value)} />
                  </label>
                </div>
                <div className="wb-params-hint">只用于这一次生成，不会改材料库；重新计算轨迹后生效，发送前仍要你确认。</div>
              </>
            ) : hasRecommendation ? (
              <div className="wb-params-hint" id="wb-params-recommended">
                材料库推荐：功率 {Math.round(powerPercent)}% · 速度 {Math.round(recommendedFeedRate)} mm/min · {recommendedPasses} 遍
              </div>
            ) : (
              <div className="wb-params-hint" id="wb-params-recommended">
                材料库推荐：当前材质/厚度暂无该模式参数（可勾选自定义，或前往材料实验室补充）
              </div>
            )}
          </div>
        </div>

    </WbCard>
  );
}
