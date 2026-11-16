import React from "react";
import { FILL_GROUPING_OPTIONS, fillSettingsSummary, normalizeFillSettings } from "../fillOptions.js";

/**
 * 填充与走刀设置（2026-11-16，对齐 LightBurn）：默认折叠，一行摘要；
 * `withFill=false` 时只显示走刀选项（描边 / 切割没有扫描填充）。
 */
export default function FillOptionsPanel({ idPrefix, settings, onChange, withFill = true }) {
  const s = normalizeFillSettings(settings);
  const set = (key, value) => onChange({ ...s, [key]: value });
  const id = (name) => `${idPrefix}-${name}`;
  return (
    <details className="fill-options" id={id("fill-options")}>
      <summary>
        <span className="fill-options-title">{withFill ? "填充与走刀设置" : "走刀设置"}</span>
        <span className="fill-options-summary">{fillSettingsSummary(s, { withFill })}</span>
      </summary>
      <div className="fill-options-body">
        {withFill ? (
          <>
            <label className="fill-options-wide">
              <span>填充分组</span>
              <select id={id("fill-grouping")} value={s.grouping} onChange={(e) => set("grouping", e.target.value)}>
                {FILL_GROUPING_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>
            </label>
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
                title="来回两行出光对不齐时，填量出的错位距离（只在双向扫描时生效）"
              />
            </label>
            <label className="checkbox-row">
              <input
                id={id("fill-bidirectional")}
                type="checkbox"
                checked={s.bidirectional}
                onChange={(e) => set("bidirectional", e.target.checked)}
              />
              <span>双向扫描（关掉后只单向出光，更齐但更慢）</span>
            </label>
            <label className="checkbox-row">
              <input
                id={id("fill-cross")}
                type="checkbox"
                checked={s.crossHatch}
                onChange={(e) => set("crossHatch", e.target.checked)}
              />
              <span>交叉排线（再转 90° 扫一遍，更实更深）</span>
            </label>
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
                <span>机器加速度（mm/s²，留空按 1000）</span>
                <input
                  id={id("fill-accel")}
                  type="number"
                  min={10}
                  max={100000}
                  step={10}
                  value={s.accelerationMmS2}
                  placeholder="1000"
                  onChange={(e) => set("accelerationMmS2", e.target.value)}
                  title="GRBL 的 $120 / $121；越小需要的过扫描越长"
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
      </div>
    </details>
  );
}
