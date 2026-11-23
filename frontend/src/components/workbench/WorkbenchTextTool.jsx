import React from "react";
import { isRemoteMode } from "../../authSession.js";
import { IMAGE_ROUTE_FILL_LABEL, IMAGE_ROUTE_TEXT_NOTE, textStyleAvailable } from "../../engraveRoute.js";
import { materialLayerHint } from "../../fillOptions.js";
import { TEXT_ORIGIN_OPTIONS, TEXT_STYLE_OPTIONS } from "../../workbenchParams.js";
import EngraveRouteSwitch from "../EngraveRouteSwitch.jsx";
import FillOptionsPanel from "../FillOptionsPanel.jsx";
import { OriginBadge } from "../OriginBadge.jsx";
import { TEXT_ORIGIN_SHARED_HINT } from "../../textOrigin.js";

/**
 * 雕刻工作台 ② 加工内容 · 矢量文字：文本、字高、刻法、对刀起点与填充 / 切割设置。原样搬自 pages/HomePage.jsx（2026-11-16 按职责拆分）。
 * 2026-11-17（技术书 r45）：刻法上方加雕刻路线开关（与创作画板、语音同一个值）；转图片时 `textStyle` 传入的是实际生效的刻法，
 * 描边 / 排线不可选。
 */
export function WorkbenchTextTool({
  textContent,
  setTextContent,
  fontSizeMm,
  setFontSizeMm,
  textStyle,
  setTextStyle,
  engraveRoute = "vector",
  setEngraveRoute,
  engraveRouteSaving = false,
  engraveRouteReadOnly = false,
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
      <EngraveRouteSwitch
        idPrefix="wb"
        route={engraveRoute}
        onChange={setEngraveRoute}
        saving={engraveRouteSaving}
        readOnly={engraveRouteReadOnly}
      />
      <div className="wb-params-grid wb-text-params-grid">
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
            {/* 只列当前路线能用的刻法（r46）：转图片时只有实心扫描 */}
            {TEXT_STYLE_OPTIONS.filter((opt) => textStyleAvailable(opt.value, engraveRoute)).map((opt) => (
              <option key={opt.value} value={opt.value}>
                {engraveRoute === "image" && opt.value === "fill" ? IMAGE_ROUTE_FILL_LABEL : opt.label}
              </option>
            ))}
          </select>
        </label>
        <label title={TEXT_ORIGIN_SHARED_HINT}>
          <span className="wb-origin-label-row">
            <span>对刀起点</span>
            <OriginBadge origin={textOrigin} />
          </span>
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
      {engraveRoute === "image" ? (
        <div className="wb-params-hint" id="wb-text-route-note">{IMAGE_ROUTE_TEXT_NOTE}</div>
      ) : null}
      <FillOptionsPanel
        idPrefix="wb"
        settings={fillSettings}
        onChange={updateFillSettings}
        withFill={textUsesFill}
        withCut={textIsCut}
        scanOnly={engraveRoute === "image"}
        materialHint={materialLayerHint(materialApplied, materialLayer?.label)}
        materialEntryLabel={materialLayer?.label || ""}
        onSaveToMaterial={
          !isRemoteMode() && materialLayer && engraveRoute !== "image" && (textUsesFill || textIsCut) ? saveFillToMaterial : null
        }
      />
    </div>
  );
}
