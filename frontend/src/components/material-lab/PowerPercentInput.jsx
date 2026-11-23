import React, { useState } from "react";
import { percentToPowerScale, powerScaleToPercent } from "../../materialLabState.js";

/**
 * 材料实验室 0–100% 功率输入框：
 * 前端按百分比显示与输入（最多保留一位小数），实时换算为后端 0–1000（S 值）状态；
 * 编辑期间保留用户原始输入草稿，失焦或外部切换材料时归一化显示，往返不漂移。
 */
export function PowerPercentInput({
  sValue,
  onChangeSValue,
  readOnly = false,
  className = "",
  id,
  ariaLabel,
}) {
  const [draft, setDraft] = useState(null);
  const numericS = Number(sValue);
  const hasFiniteS = sValue !== "" && sValue !== null && sValue !== undefined && Number.isFinite(numericS);
  const externalText = hasFiniteS ? String(powerScaleToPercent(numericS)) : "";

  let displayed = externalText;
  if (draft !== null) {
    const draftScale = percentToPowerScale(draft);
    if ((draft === "" || draft === "-") && !hasFiniteS) {
      displayed = draft;
    } else if (hasFiniteS && draftScale === Math.round(numericS)) {
      displayed = draft;
    }
  }

  return (
    <input
      id={id}
      type="number"
      inputMode="decimal"
      min={0}
      max={100}
      step={0.1}
      value={displayed}
      readOnly={readOnly}
      aria-label={ariaLabel}
      className={className}
      onChange={(e) => {
        const raw = e.target.value;
        setDraft(raw);
        if (raw === "" || raw === "-") {
          onChangeSValue(NaN);
          return;
        }
        const nextScale = percentToPowerScale(raw);
        onChangeSValue(nextScale);
      }}
      onBlur={() => setDraft(null)}
    />
  );
}
