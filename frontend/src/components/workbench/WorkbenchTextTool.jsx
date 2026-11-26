import React from "react";
import { isReadOnlyReferenceClient } from "../../authSession.js";
import { IMAGE_ROUTE_FILL_LABEL, IMAGE_ROUTE_TEXT_NOTE, textStyleAvailable } from "../../engraveRoute.js";
import { materialLayerHint } from "../../fillOptions.js";
import {
  TEXT_BORDER_OPTIONS,
  TEXT_ORIGIN_OPTIONS,
  TEXT_STYLE_OPTIONS,
  WORKBENCH_TEXT_MAX_CHARS,
  WORKBENCH_TEXT_MAX_LINES,
  normalizeWorkbenchText,
  workbenchTextStats,
} from "../../workbenchParams.js";
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
  surfaceOnly = false,
  materialApplied,
  materialLayer,
  saveFillToMaterial,
  textBorder = { shape: "none", double: false, boardWidthMm: "", boardHeightMm: "" },
  setTextBorder = null,
}) {
  const stats = workbenchTextStats(textContent);
  const border = textBorder || {};
  const hasBorder = Boolean(border.shape && border.shape !== "none");
  const updateBorder = (patch) => setTextBorder?.({ ...border, ...patch });
  return (
    <div className="tool-body wb-spacer">
      <div className="wb-spacer-2">
        <div className="wb-row-3">
          <label className="wb-label">
            输入文本内容
          </label>
          <span
            id="wb-text-count"
            style={{ fontSize: "11px", fontFamily: "var(--font-mono)", color: stats.chars > WORKBENCH_TEXT_MAX_CHARS - 8 ? "var(--gold)" : "var(--ink-dim)" }}
          >
            {stats.chars} / {WORKBENCH_TEXT_MAX_CHARS} 字 · {Math.max(stats.lines, 1)} / {WORKBENCH_TEXT_MAX_LINES} 行
          </span>
        </div>
        {/* 2026-11-26：支持回车分行（最多 3 行，如第一行名字、第二行日期），每行按字高排版、居中对齐 */}
        <textarea
          id="wb-text-content"
          rows={Math.min(Math.max(stats.lines, 1), WORKBENCH_TEXT_MAX_LINES)}
          value={textContent}
          onChange={(e) => setTextContent(normalizeWorkbenchText(e.target.value))}
          className="text-input wb-field-4 wb-text-area"
          placeholder="输入要雕刻的字符..."
          aria-label="输入文本内容（回车换行，最多 3 行）"
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
            {/* 只列当前路线与材料能用的刻法（r46 / r63）：转图片时只有实心扫描，仅表面通用材料隐藏外框切割 */}
            {TEXT_STYLE_OPTIONS.filter((opt) => textStyleAvailable(opt.value, engraveRoute, { surfaceOnly })).map((opt) => (
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
      {/* 外框 / 铭牌（2026-11-26）：复用后端已有的边框生成（与语音“给木牌加边框”同一套），要填木牌尺寸 */}
      <div className="wb-params-grid wb-text-params-grid" id="wb-text-border-row">
        <label>
          <span>外框 / 铭牌</span>
          <select
            id="wb-text-border"
            value={border.shape || "none"}
            onChange={(e) => updateBorder({ shape: e.target.value })}
            className="select-input wb-compact-select"
          >
            {TEXT_BORDER_OPTIONS.map((opt) => (
              <option key={opt.value} value={opt.value}>{opt.label}</option>
            ))}
          </select>
        </label>
        {hasBorder ? (
          <>
            <label>
              <span>{border.shape === "circle" ? "木牌直径 (mm)" : "木牌宽 (mm)"}</span>
              <input
                id="wb-text-board-width"
                type="number"
                inputMode="decimal"
                min={1}
                value={border.boardWidthMm ?? ""}
                onChange={(e) => updateBorder({ boardWidthMm: e.target.value })}
                className="text-input wb-field-5"
              />
            </label>
            {border.shape === "circle" ? null : (
              <label>
                <span>木牌高 (mm)</span>
                <input
                  id="wb-text-board-height"
                  type="number"
                  inputMode="decimal"
                  min={1}
                  value={border.boardHeightMm ?? ""}
                  onChange={(e) => updateBorder({ boardHeightMm: e.target.value })}
                  className="text-input wb-field-5"
                />
              </label>
            )}
            <label className="wb-check-inline">
              <span>双线框</span>
              <input
                id="wb-text-border-double"
                type="checkbox"
                checked={Boolean(border.double)}
                onChange={(e) => updateBorder({ double: e.target.checked })}
              />
            </label>
          </>
        ) : null}
      </div>
      {hasBorder ? (
        <div className="wb-params-hint" id="wb-text-border-hint">
          外框比木牌四周各内缩 3 mm（双线框内圈再缩 1.5 mm），文字按字高居中放进框里，放不下时会提示减小字高；外框按当前刻法只走线，选“外框切割”时文字和外框都会切穿。
        </div>
      ) : null}
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
          !isReadOnlyReferenceClient() && materialLayer && engraveRoute !== "image" && (textUsesFill || textIsCut) ? saveFillToMaterial : null
        }
      />
    </div>
  );
}
