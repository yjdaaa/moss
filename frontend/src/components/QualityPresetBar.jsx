import React from "react";
import { CUSTOM_PRESET, QUALITY_PRESET_OPTIONS } from "../qualityPresets.js";

/**
 * 质量预设“快速 / 均衡 / 精细”（2026-11-16，技术书 r41）：点一下把高级设置填成预设值；
 * 高级设置被改过（与三个预设都不一致）时多显示一个“自定义”标记。
 */
export default function QualityPresetBar({ id, value, onSelect, hints = {} }) {
  return (
    <div className="quality-presets" id={id} role="radiogroup" aria-label="质量预设">
      <span className="quality-presets-label">质量</span>
      <div className="ob-chips">
        {QUALITY_PRESET_OPTIONS.map((opt) => (
          <button
            key={opt.value}
            type="button"
            role="radio"
            aria-checked={value === opt.value}
            id={id ? `${id}-${opt.value}` : undefined}
            className={`ob-chip${value === opt.value ? " on" : ""}`}
            title={hints[opt.value]}
            onClick={() => onSelect(opt.value)}
          >
            {opt.label}
          </button>
        ))}
        {value === CUSTOM_PRESET ? (
          <span className="ob-chip on quality-preset-custom" title="高级设置已改过，与三个预设都不同">
            自定义
          </span>
        ) : null}
      </div>
    </div>
  );
}
