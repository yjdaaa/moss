import React from "react";
import {
  FILL_GROUPING_OPTIONS,
  MAX_SCAN_OFFSET_ROWS,
  TAB_MODE_OPTIONS,
  fillSettingsSummary,
  normalizeFillSettings,
  offsetTablePayload,
} from "../fillOptions.js";
import { FILL_PRESET_HINTS, applyFillPreset, detectFillPreset } from "../qualityPresets.js";
import QualityPresetBar from "./QualityPresetBar.jsx";

/**
 * 填充与走刀设置（2026-11-16，对齐 LightBurn）：默认折叠，一行摘要；
 * `withFill=false` 时只显示走刀选项（描边 / 切割没有扫描填充）；
 * `withCut=true`（工作台文字“外框切割”）时多一段切割设置：切缝补偿、桥位、引入 / 引出线、过切（默认全关）。
 * `scanOnly=true`（2026-11-17 r46，转图片逐行扫描：工作台照片、画板光栅、文字转图片）时只显示位图扫描用得上的
 * 双向扫描、扫描偏移（含速度表）与过扫描，不显示分组、角度、交叉排线、走刀选项与质量预设。
 * r40：`materialHint` 非空时显示“来自材料库”提示；给了 `onSaveToMaterial` 时显示“保存到材料库”按钮
 * （扫描角度、过扫描、切割设置等随材料保存，扫描偏移与加速度仍只存本机）。
 */
function FillOptionsDetails({
  idPrefix,
  settings,
  onChange,
  withFill = true,
  withCut = false,
  scanOnly = false,
  materialHint = "",
  materialEntryLabel = "",
  onSaveToMaterial = null,
}) {
  const s = normalizeFillSettings(settings);
  const set = (key, value) => onChange({ ...s, [key]: value });
  const id = (name) => `${idPrefix}-${name}`;
  const title = scanOnly
    ? "扫描设置"
    : withCut
      ? (withFill ? "切割、填充与走刀设置" : "切割与走刀设置")
      : (withFill ? "填充与走刀设置" : "走刀设置");
  const scanSummary = [
    s.bidirectional ? "双向" : "单向",
    s.overscanAuto ? "过扫描自动" : `过扫描 ${s.overscanMm || 0} mm`,
    ...(offsetTablePayload(s.scanOffsetTable).value ? ["偏移按速度表"] : Number(s.scanOffsetMm) ? [`偏移 ${s.scanOffsetMm} mm`] : []),
  ].join(" · ");
  const setRow = (index, key, value) =>
    set("scanOffsetTable", s.scanOffsetTable.map((row, i) => (i === index ? { ...row, [key]: value } : row)));
  const numberInput = (key, name, { min, max, step, title: hint }) => (
    <input
      id={id(name)}
      type="number"
      min={min}
      max={max}
      step={step}
      value={s[key]}
      onChange={(e) => set(key, e.target.value)}
      title={hint}
    />
  );
  return (
    <details className="fill-options" id={id("fill-options")}>
      <summary>
        <span className="fill-options-title">{withFill || scanOnly ? `高级设置 · ${title}` : title}</span>
        <span className="fill-options-summary">
          {scanOnly ? scanSummary : fillSettingsSummary(s, { withFill, withCut })}
          {materialHint ? " · 来自材料库" : ""}
        </span>
      </summary>
      <div className="fill-options-body">
        {materialHint || onSaveToMaterial ? (
          <div className="fill-options-wide fill-options-material" id={id("material-layer")}>
            <span>{materialHint || `材料库「${materialEntryLabel || "当前条目"}」还没有存这组设置（用的是本机上次的值）`}</span>
            {onSaveToMaterial ? (
              <button
                type="button"
                className="btn-ghost"
                id={id("material-save")}
                onClick={onSaveToMaterial}
                title="把扫描角度、过扫描、切割设置等存进当前材料条目；扫描偏移与机器加速度跟机器走，不存进材料库"
              >
                保存到材料库
              </button>
            ) : null}
          </div>
        ) : null}
        {withCut && !scanOnly ? (
          <>
            <div className="fill-options-wide fill-options-section">切割设置（对齐 LightBurn，全部为 0 时与以前完全相同）</div>
            <label>
              <span>切缝补偿（mm，正值外扩 / 负值内缩）</span>
              {numberInput("kerfMm", "cut-kerf", {
                min: -2,
                max: 2,
                step: 0.01,
                title: "一般填切缝宽度的一半；孔会反向补偿，切下来的零件保持设计尺寸",
              })}
            </label>
            <label>
              <span>桥位（防零件掉落）</span>
              <select id={id("cut-tab-mode")} value={s.tabMode} onChange={(e) => set("tabMode", e.target.value)}>
                {TAB_MODE_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>
            </label>
            {s.tabMode === "count" ? (
              <label>
                <span>每个图形几个桥位</span>
                {numberInput("tabCount", "cut-tab-count", { min: 1, max: 100, step: 1 })}
              </label>
            ) : null}
            {s.tabMode === "spacing" ? (
              <label>
                <span>桥位间距（mm）</span>
                {numberInput("tabSpacingMm", "cut-tab-spacing", { min: 1, max: 1000, step: 1 })}
              </label>
            ) : null}
            {s.tabMode !== "off" ? (
              <>
                <label>
                  <span>桥位宽度（mm）</span>
                  {numberInput("tabWidthMm", "cut-tab-width", { min: 0.05, max: 10, step: 0.05 })}
                </label>
                <label className="checkbox-row">
                  <input
                    id={id("cut-tab-skip-inner")}
                    type="checkbox"
                    checked={s.tabsSkipInner}
                    onChange={(e) => set("tabsSkipInner", e.target.checked)}
                  />
                  <span>内部图形（孔）不加桥位</span>
                </label>
              </>
            ) : null}
            <label>
              <span>引入线（mm，0 关闭）</span>
              {numberInput("leadInMm", "cut-lead-in", {
                min: 0,
                max: 10,
                step: 0.1,
                title: "从废料一侧切进起刀点，起刀的烧痕留在废料上",
              })}
            </label>
            <label>
              <span>引出线（mm，0 关闭）</span>
              {numberInput("leadOutMm", "cut-lead-out", { min: 0, max: 10, step: 0.1 })}
            </label>
            <label>
              <span>引线角度（°，90 垂直）</span>
              {numberInput("leadAngleDeg", "cut-lead-angle", { min: 1, max: 90, step: 1 })}
            </label>
            <label>
              <span>过切（mm，回到起点后多切）</span>
              {numberInput("overcutMm", "cut-overcut", {
                min: 0,
                max: 10,
                step: 0.1,
                title: "起点处没切透时多切一段把口子接上；跨过桥位时照样关光",
              })}
            </label>
          </>
        ) : null}
        {withFill || scanOnly ? (
          <>
            {scanOnly ? null : (
            <label className="fill-options-wide">
              <span>填充分组</span>
              <select id={id("fill-grouping")} value={s.grouping} onChange={(e) => set("grouping", e.target.value)}>
                {FILL_GROUPING_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>
            </label>
            )}
            {scanOnly ? null : (
            <label>
              <span>扫描角度（°，0 横扫 / 90 竖扫）</span>
              <input
                id={id("fill-angle")}
                type="number"
                min={-360}
                max={360}
                step={1}
                value={s.angleDeg}
                onChange={(e) => set("angleDeg", e.target.value)}
              />
            </label>
            )}
            <label>
              <span>双向扫描偏移（mm）</span>
              <input
                id={id("fill-offset")}
                type="number"
                min={-2}
                max={2}
                step={0.01}
                value={s.scanOffsetMm}
                onChange={(e) => set("scanOffsetMm", e.target.value)}
                title="来回两行出光对不齐时，填量出的错位距离（只在双向扫描时生效）；下面填了按速度的偏移表时以表为准"
              />
            </label>
            <div className="fill-options-wide fill-offset-table" id={id("fill-offset-table")}>
              <span>按速度的扫描偏移表（可选，按加工速度插值，填了就覆盖上面的固定偏移）</span>
              {s.scanOffsetTable.map((row, index) => (
                <div className="fill-offset-row" key={index}>
                  <input
                    type="number"
                    min={1}
                    step={100}
                    placeholder="速度 mm/min"
                    aria-label={`第 ${index + 1} 行速度`}
                    value={row.speed}
                    onChange={(e) => setRow(index, "speed", e.target.value)}
                  />
                  <input
                    type="number"
                    min={-2}
                    max={2}
                    step={0.01}
                    placeholder="偏移 mm"
                    aria-label={`第 ${index + 1} 行偏移`}
                    value={row.offset}
                    onChange={(e) => setRow(index, "offset", e.target.value)}
                  />
                  <button
                    type="button"
                    className="btn-ghost"
                    onClick={() => set("scanOffsetTable", s.scanOffsetTable.filter((_, i) => i !== index))}
                  >
                    删除
                  </button>
                </div>
              ))}
              {s.scanOffsetTable.length < MAX_SCAN_OFFSET_ROWS ? (
                <button
                  type="button"
                  className="btn-ghost"
                  id={id("fill-offset-add")}
                  onClick={() => set("scanOffsetTable", [...s.scanOffsetTable, { speed: "", offset: "" }])}
                >
                  添加一行
                </button>
              ) : null}
            </div>
            <label className="checkbox-row">
              <input
                id={id("fill-bidirectional")}
                type="checkbox"
                checked={s.bidirectional}
                onChange={(e) => set("bidirectional", e.target.checked)}
              />
              <span>双向扫描（关掉后只单向出光，更齐但更慢）</span>
            </label>
            {scanOnly ? null : (
            <label className="checkbox-row">
              <input
                id={id("fill-cross")}
                type="checkbox"
                checked={s.crossHatch}
                onChange={(e) => set("crossHatch", e.target.checked)}
              />
              <span>交叉排线（再转 90° 扫一遍，更实更深）</span>
            </label>
            )}
            <label className="checkbox-row">
              <input
                id={id("fill-overscan-auto")}
                type="checkbox"
                checked={s.overscanAuto}
                onChange={(e) => set("overscanAuto", e.target.checked)}
              />
              <span>过扫描按速度自动算（每行两头关光多走一段，防线头烧深）</span>
            </label>
            {s.overscanAuto ? (
              <label>
                <span>机器加速度（mm/s²，留空用连接时读到的值）</span>
                <input
                  id={id("fill-accel")}
                  type="number"
                  min={10}
                  max={100000}
                  step={10}
                  value={s.accelerationMmS2}
                  placeholder="1000"
                  onChange={(e) => set("accelerationMmS2", e.target.value)}
                  title="GRBL 的 $120 / $121，也用于估算工时；留空时用连接检查读到的值，都没有按 1000"
                />
              </label>
            ) : (
              <label>
                <span>过扫描距离（mm，0 关闭）</span>
                <input
                  id={id("fill-overscan")}
                  type="number"
                  min={0}
                  max={20}
                  step={0.1}
                  value={s.overscanMm}
                  onChange={(e) => set("overscanMm", e.target.value)}
                />
              </label>
            )}
          </>
        ) : null}
        {scanOnly ? null : (
        <>
        <label className="checkbox-row">
          <input
            id={id("path-corners")}
            type="checkbox"
            checked={s.preferCorners}
            onChange={(e) => set("preferCorners", e.target.checked)}
          />
          <span>轮廓起刀点尽量放在尖角（起刀焦点不落在曲线中间）</span>
        </label>
        <label className="checkbox-row">
          <input
            id={id("path-overlaps")}
            type="checkbox"
            checked={s.removeOverlaps}
            onChange={(e) => set("removeOverlaps", e.target.checked)}
          />
          <span>重复的线只刻一遍</span>
        </label>
        </>
        )}
      </div>
    </details>
  );
}

/**
 * 有扫描填充时先显示质量预设“快速 / 均衡 / 精细”（2026-11-16，技术书 r41），细项收在下面折叠的“高级设置”里；
 * 只描轮廓 / 切割没有填充项，不显示预设。
 */
export default function FillOptionsPanel(props) {
  const { idPrefix, settings, onChange, withFill = true, scanOnly = false } = props;
  if (!withFill || scanOnly) return <FillOptionsDetails {...props} />;
  const s = normalizeFillSettings(settings);
  return (
    <>
      <QualityPresetBar
        id={`${idPrefix}-fill-preset`}
        value={detectFillPreset(s)}
        hints={FILL_PRESET_HINTS}
        onSelect={(key) => onChange(applyFillPreset(s, key))}
      />
      <FillOptionsDetails {...props} />
    </>
  );
}
