import React from "react";
import {
  ENGRAVE_ROUTE_OPTIONS,
  ENGRAVE_ROUTE_READONLY_HINT,
  ENGRAVE_ROUTE_SHARED_HINT,
  engraveRouteHint,
  normalizeEngraveRoute,
} from "../engraveRoute.js";

/**
 * 雕刻路线开关“矢量 / 转图片”（2026-11-17 用户决定，技术书 r45）：工作台与创作画板各放一个，
 * 背后是后端同一个值（语音生成也按它）。DOM id 为 `${idPrefix}-engrave-route`，两个选项 `-vector` / `-image`。
 * 连接电脑（局域网）时只读。
 */
export default function EngraveRouteSwitch({ idPrefix, route, onChange, readOnly = false, saving = false }) {
  const id = `${idPrefix}-engrave-route`;
  const value = normalizeEngraveRoute(route);
  return (
    <div className="engrave-route" id={id}>
      <div className="quality-presets" role="radiogroup" aria-label="雕刻路线">
        <span className="quality-presets-label">路线</span>
        <div className="ob-chips">
          {ENGRAVE_ROUTE_OPTIONS.map((opt) => (
            <button
              key={opt.value}
              type="button"
              role="radio"
              aria-checked={value === opt.value}
              id={`${id}-${opt.value}`}
              className={`ob-chip${value === opt.value ? " on" : ""}`}
              title={readOnly ? ENGRAVE_ROUTE_READONLY_HINT : opt.hint}
              disabled={readOnly || saving}
              onClick={() => onChange?.(opt.value)}
            >
              {opt.label}
            </button>
          ))}
        </div>
      </div>
      <div className="engrave-route-hint" id={`${id}-hint`}>
        {engraveRouteHint(value)}；{readOnly ? ENGRAVE_ROUTE_READONLY_HINT : ENGRAVE_ROUTE_SHARED_HINT}
      </div>
    </div>
  );
}
