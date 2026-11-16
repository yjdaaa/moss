import React from "react";
import { isRemoteMode } from "../../authSession.js";
import { materialLayerHint } from "../../fillOptions.js";
import { TEXT_ORIGIN_OPTIONS, TEXT_STYLE_OPTIONS } from "../../workbenchParams.js";
import FillOptionsPanel from "../FillOptionsPanel.jsx";

/** 雕刻工作台 ② 加工内容 · 矢量文字：文本、字高、刻法、对刀起点与填充 / 切割设置。原样搬自 pages/HomePage.jsx（2026-11-16 按职责拆分） */
export function WorkbenchTextTool({
  textContent,
  setTextContent,
  fontSizeMm,
  setFontSizeMm,
  textStyle,
  setTextStyle,
  textOrigin,
  setTextOrigin,
  fillSettings,
  updateFillSettings,
  textUsesFill,
  textIsCut,
  materialApplied,
  materialLayer,
  saveFillToMaterial,
}) {
  return (
    <div className="tool-body wb-spacer">
      <div className="wb-spacer-2">
        <div className="wb-row-3">
          <label className="wb-label">
            输入文本内容
          </label>
          <span style={{ fontSize: "11px", fontFamily: "var(--font-mono)", color: textContent.length > 20 ? "var(--gold)" : "var(--ink-dim)" }}>
            {textContent.length} / 24 字符
          </span>
        </div>
        <input
          type="text"
          maxLength={24}
          value={textContent}
          onChange={(e) => setTextContent(e.target.value)}
          className="text-input wb-field-4"
          placeholder="输入要雕刻的字符..."
        />
      </div>
      <div className="wb-params-grid">
        <label>
          <span>字高 (mm)</span>
          <input
            type="number"
            min={1}
            max={100}
            value={fontSizeMm}
            onChange={(e) => setFontSizeMm(Number(e.target.value))}
            className="text-input wb-field-5"
          />
        </label>
        <label>
          <span>刻法模式</span>
          <select
            id="wb-text-style"
            value={textStyle}
            onChange={(e) => setTextStyle(e.target.value)}
            className="select-input wb-compact-select"
          >
            {TEXT_STYLE_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
        </label>
        <label>
          <span>对刀起点</span>
          <select
            id="wb-text-origin"
            value={textOrigin}
            onChange={(e) => setTextOrigin(e.target.value)}
            className="select-input wb-compact-select"
          >
            {TEXT_ORIGIN_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
        </label>
      </div>
      <FillOptionsPanel
        idPrefix="wb"
        settings={fillSettings}
        onChange={updateFillSettings}
        withFill={textUsesFill}
        withCut={textIsCut}
        materialHint={materialLayerHint(materialApplied, materialLayer?.label)}
        materialEntryLabel={materialLayer?.label || ""}
        onSaveToMaterial={!isRemoteMode() && materialLayer && (textUsesFill || textIsCut) ? saveFillToMaterial : null}
      />
    </div>
  );
}
