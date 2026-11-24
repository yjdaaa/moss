import React, { useState, useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { Layers, Trash2, X } from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { showConfirm } from "../appDialog.js";
import { notifyMaterialsUpdated, thicknessesEqual } from "../materialSelectState.js";
import { isTopmostModalBackdrop } from "./AppDialog.jsx";

export function parseThicknessList(str) {
  return String(str || "")
    .split(/[,，\s]+/)
    .map((s) => s.trim())
    .filter(Boolean)
    .map(Number)
    .filter((n) => Number.isFinite(n) && n > 0);
}

function findExistingThickness(existingThicknesses, thickNum, laserMode, engravingMode) {
  if (!existingThicknesses || typeof existingThicknesses !== "object") {
    return { matchedKey: null, cell: null };
  }
  const matchedKey = Object.keys(existingThicknesses).find((k) => thicknessesEqual(k, thickNum)) ?? null;
  if (!matchedKey) {
    return { matchedKey: null, cell: null };
  }
  const entry = existingThicknesses[matchedKey];
  if (!entry || typeof entry !== "object") {
    return { matchedKey, cell: null };
  }
  const cell = laserMode === "cut" ? entry.cut || null : entry.engrave?.[engravingMode] || null;
  return { matchedKey, cell };
}

export function formatFeedRateHint(feedRate) {
  const num = Number(feedRate);
  if (feedRate === "" || feedRate === null || feedRate === undefined || !Number.isFinite(num) || num <= 0) {
    return "约 — mm/s";
  }
  return `约 ${Number((num / 60).toFixed(1))} mm/s`;
}

export function buildMaterialSavePayload({
  material,
  thickList,
  power,
  feedRate: feedRateInput,
  speed,
  laserMode = "engrave",
  engravingMode = "raster",
  feedRateOverride,
  passesOverride,
  existingThicknesses,
  targetThickness,
  paramsChanged = true,
  pruneMissingThicknesses = false,
}) {
  const normalizedLaserMode = laserMode === "cut" || engravingMode === "cut" ? "cut" : "engrave";
  const normalizedEngravingMode = engravingMode === "outline" ? "outline" : "raster";
  const laserMaxPower = Math.round((Number(power) / 100) * 1000);
  const feedRate = Number.isFinite(Number(feedRateOverride)) && Number(feedRateOverride) > 0
    ? Math.round(Number(feedRateOverride))
    : Number.isFinite(Number(feedRateInput)) && Number(feedRateInput) > 0
      ? Math.round(Number(feedRateInput))
      : Math.round(Number(speed) * 60);
  const speedMms = Number((feedRate / 60).toFixed(2));
  const defaultPasses = Number.isInteger(Number(passesOverride)) && Number(passesOverride) >= 1
    ? Number(passesOverride)
    : 1;
  const baseCell = {
    laser_min_power: 0,
    laser_max_power: laserMaxPower,
    feed_rate: feedRate,
    travel_rate: 3000,
    threshold: -1,
    passes: defaultPasses,
    pixel_size_mm: 0.1,
  };
  const thicknesses = {};
  for (const th of thickList) {
    const { matchedKey, cell: prev } = findExistingThickness(
      existingThicknesses,
      th,
      normalizedLaserMode,
      normalizedEngravingMode,
    );
    const outKey = matchedKey ?? String(th);
    const isTarget = targetThickness == null || thicknessesEqual(th, targetThickness);
    if (prev) {
      const shouldUpdateThisThick = paramsChanged && isTarget;
      const nextCell = {
        ...prev,
        laser_min_power: prev.laser_min_power ?? 0,
        laser_max_power: shouldUpdateThisThick ? laserMaxPower : (prev.laser_max_power ?? laserMaxPower),
        feed_rate: shouldUpdateThisThick ? feedRate : (prev.feed_rate ?? feedRate),
        travel_rate: prev.travel_rate ?? 3000,
        threshold: prev.threshold ?? -1,
        passes: prev.passes ?? defaultPasses,
        pixel_size_mm: prev.pixel_size_mm ?? 0.1,
      };
      thicknesses[outKey] =
        normalizedLaserMode === "cut"
          ? { cut: nextCell }
          : { engrave: { [normalizedEngravingMode]: nextCell } };
    } else if (matchedKey && (!paramsChanged || !isTarget)) {
      // 该厚度原本存在于材料库但未配置当前模式：传空对象防止被 prune 误删，同时绝不捏造未存模式的参数
      thicknesses[outKey] = {};
    } else {
      const nextCell = { ...baseCell };
      thicknesses[outKey] =
        normalizedLaserMode === "cut"
          ? { cut: nextCell }
          : { engrave: { [normalizedEngravingMode]: nextCell } };
    }
  }
  const payload = {
    action: "save",
    material,
    thicknesses,
    power_percent: Number(power),
    speed_mms: speedMms,
    laser_mode: normalizedLaserMode,
    engraving_mode: normalizedLaserMode === "cut" ? "" : normalizedEngravingMode,
    // 兼容单厚度字段
    thickness_mm: thickList[0],
    laser_max_power: laserMaxPower,
    feed_rate: feedRate,
  };
  if (pruneMissingThicknesses) {
    payload.prune_missing_thicknesses = true;
  }
  return payload;
}

export function buildMaterialDeletePayload(material) {
  return {
    action: "delete",
    material,
    thickness_mm: 0,
    confirmed: true,
  };
}

// 关闭时整个对话框卸载，下次打开按 initialData 重新填表；Hooks 放在内层组件，保证每次渲染调用顺序一致
export function MaterialModal({ visible, ...props }) {
  return visible ? <MaterialModalDialog {...props} /> : null;
}

function resolveInitialFeedRate(initialData) {
  const direct = Number(initialData?.feed_rate);
  if (Number.isFinite(direct) && direct > 0) return Math.round(direct);
  const fromMms = Number(initialData?.speed_mms) * 60;
  if (Number.isFinite(fromMms) && fromMms > 0) return Math.round(fromMms);
  return 1500;
}

function resolveInitialModalForm(initialData) {
  if (!initialData) {
    return {
      name: "",
      thicknessStr: "3",
      power: 50,
      feedRate: 1500,
    };
  }
  // 过滤“表面通用”非数字键（"surface"），避免把字面量塞进数字厚度输入框导致校验失败
  const numericList = Array.isArray(initialData.thicknesses)
    ? initialData.thicknesses.filter((t) => Number.isFinite(Number(t)) && Number(t) > 0)
    : [];
  const thicknessStr = numericList.length > 0
    ? numericList.join(", ")
    : (Number.isFinite(Number(initialData.thickness_mm)) && Number(initialData.thickness_mm) > 0
      ? String(initialData.thickness_mm)
      : "3");
  return {
    name: initialData.name || "",
    thicknessStr,
    power: Number(initialData.power_percent || 50),
    feedRate: resolveInitialFeedRate(initialData),
  };
}

function MaterialModalDialog({ initialData, onClose, onSaved, onDeleted }) {
  const isEditing = Boolean(initialData && initialData.name);
  const initialForm = resolveInitialModalForm(initialData);
  const [name, setName] = useState(initialForm.name);
  const [thicknessStr, setThicknessStr] = useState(initialForm.thicknessStr);
  const [power, setPower] = useState(initialForm.power);
  const [feedRate, setFeedRate] = useState(initialForm.feedRate);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const backdropRef = useRef(null);
  const backdropPressRef = useRef(false);

  useEffect(() => {
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        if (!isTopmostModalBackdrop(backdropRef.current)) return;
        e.preventDefault();
        if (busy) return;
        onClose?.();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  useEffect(() => {
    const next = resolveInitialModalForm(initialData);
    setName(next.name);
    setThicknessStr(next.thicknessStr);
    setPower(next.power);
    setFeedRate(next.feedRate);
    setError("");
    setBusy(false);
  }, [initialData]);

  const handleSave = async () => {
    const trimmedName = name.trim();
    if (!trimmedName) {
      setError("材料名称不能为空");
      return;
    }
    const thickList = parseThicknessList(thicknessStr);
    if (thickList.length === 0) {
      setError("请至少输入一个有效的厚度（大于0的数字）");
      return;
    }

    const powerNum = Number(power);
    const feedNum = Number(feedRate);
    if (!Number.isFinite(powerNum) || powerNum <= 0 || powerNum > 100) {
      setError("推荐功率须在 1 ~ 100% 之间");
      return;
    }
    if (!Number.isFinite(feedNum) || feedNum < 60 || feedNum > 36000) {
      setError("推荐速度须在 60 ~ 36000 mm/min 之间");
      return;
    }

    setBusy(true);
    setError("");

    const initialPower = Number(initialData?.power_percent || 50);
    const initialFeed = resolveInitialFeedRate(initialData);
    const roundedFeed = Math.round(feedNum);
    const paramsChanged = !isEditing || powerNum !== initialPower || roundedFeed !== initialFeed;

    let resolvedExistingThicknesses = isEditing ? initialData?.existing_thicknesses : undefined;
    if (isEditing && !resolvedExistingThicknesses) {
      try {
        const exportRes = await getJson("/api/material-lab/export");
        const mats = exportRes.data?.result?.library?.materials;
        if (!exportRes.ok || !exportRes.data?.success || !mats || typeof mats !== "object") {
          setBusy(false);
          setError("读取库内各厚度既有参数失败，已中止保存以免覆盖其他厚度");
          return;
        }
        resolvedExistingThicknesses = mats[trimmedName]?.thicknesses || {};
      } catch (e) {
        setBusy(false);
        setError(`读取库内参数异常，已中止保存: ${e.message}`);
        return;
      }
    }

    // D9: 原子批量保存所有厚度，单次提交
    const payload = buildMaterialSavePayload({
      material: trimmedName,
      thickList,
      power: powerNum,
      feedRate: roundedFeed,
      laserMode: initialData?.laser_mode,
      engravingMode: initialData?.engraving_mode,
      passesOverride: initialData?.passes,
      existingThicknesses: resolvedExistingThicknesses,
      targetThickness: isEditing ? initialData?.target_thickness : undefined,
      paramsChanged,
      pruneMissingThicknesses: isEditing && Boolean(resolvedExistingThicknesses),
    });

    try {
      const res = await postJson("/api/material-lab/manage", payload);
      setBusy(false);

      if (res.ok && res.data?.success) {
        notifyMaterialsUpdated();
        if (onSaved) onSaved(res.data.result || { name: trimmedName, thicknesses: thickList });
        onClose();
      } else {
        setError(res.data?.result || res.data?.detail || "保存材料失败");
      }
    } catch (e) {
      setBusy(false);
      setError(`保存异常: ${e.message}`);
    }
  };

  const handleDelete = async () => {
    const trimmedName = name.trim();
    if (!trimmedName) return;
    const confirmed = await showConfirm(`确定要删除材料 "${trimmedName}" 吗？此操作不可恢复。`, {
      title: "删除材料",
      tone: "warning",
      danger: true,
      confirmText: "删除",
    });
    if (!confirmed) {
      return;
    }

    setBusy(true);
    setError("");

    const payload = buildMaterialDeletePayload(trimmedName);

    try {
      const res = await postJson("/api/material-lab/manage", payload);
      setBusy(false);

      if (res.ok && res.data?.success) {
        notifyMaterialsUpdated();
        if (onDeleted) onDeleted(trimmedName);
        onClose();
      } else {
        setError(res.data?.result || res.data?.detail || "删除材料失败");
      }
    } catch (e) {
      setBusy(false);
      setError(`删除异常: ${e.message}`);
    }
  };

  const content = (
    <div
      ref={backdropRef}
      className="mat-modal-backdrop"
      id="mat-modal"
      onMouseDown={(e) => {
        backdropPressRef.current = e.button === 0 && e.target === e.currentTarget;
      }}
      onMouseUp={(e) => {
        if (e.target !== e.currentTarget) backdropPressRef.current = false;
      }}
      onClick={(e) => {
        if (backdropPressRef.current && e.target === e.currentTarget && !busy) {
          onClose?.();
        }
        backdropPressRef.current = false;
      }}
    >
      <div className="mat-modal" role="dialog" aria-modal="true" aria-labelledby="mat-modal-title">
        <div className="mat-modal-head">
          <h3 id="mat-modal-title">
            <Layers size={18} className="mat-form-icon" />
            <span>{isEditing ? "编辑材料" : "添加材料"}</span>
          </h3>
          <button type="button" className="mat-modal-close" onClick={onClose} title="关闭" aria-label="关闭" disabled={busy}>
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body">
          <div className="mat-field">
            <label htmlFor="mat-f-name">材料名称</label>
            <input
              id="mat-f-name"
              className="text-input"
              type="text"
              placeholder="例如：椴木 / 亚克力"
              value={name}
              disabled={isEditing}
              onChange={(e) => setName(e.target.value)}
              autoComplete="off"
            />
          </div>

          <div className="mat-field">
            <label htmlFor="mat-f-thick">厚度规格（mm，多个厚度用逗号分隔）</label>
            <input
              id="mat-f-thick"
              className="text-input"
              type="text"
              placeholder="例如：2, 3, 5"
              value={thicknessStr}
              onChange={(e) => setThicknessStr(e.target.value)}
              autoComplete="off"
            />
          </div>

          <div className="mat-field-row">
            <div className="mat-field">
              <label htmlFor="mat-f-power">推荐功率（%）</label>
              <input
                id="mat-f-power"
                className="text-input"
                type="number"
                min="1"
                max="100"
                step="1"
                value={power}
                onChange={(e) => setPower(e.target.value)}
                placeholder="10 - 100"
              />
            </div>
            <div className="mat-field">
              <label htmlFor="mat-f-speed">推荐速度（mm/min）</label>
              <input
                id="mat-f-speed"
                className="text-input"
                type="number"
                min="60"
                max="36000"
                step="10"
                value={feedRate}
                onChange={(e) => setFeedRate(e.target.value)}
                placeholder="300 - 6000"
              />
              <small className="lab-s-hint" id="mat-f-speed-hint">
                {formatFeedRateHint(feedRate)}
              </small>
            </div>
          </div>

          <p className="mat-modal-hint">
            保存后将同步写入后端材料库（支持多厚度单次提交），用于推荐雕刻与切割加工参数。
          </p>

          {error && (
            <div className="mat-modal-err mat-form-box">
              {error}
            </div>
          )}
        </div>

        <div className="mat-modal-foot">
          {isEditing && (
            <button
              type="button"
              className="btn btn-ghost mat-del-btn mat-form-btn"
              onClick={handleDelete}
              disabled={busy}
            >
              <Trash2 size={14} className="mat-form-icon-2" />
              删除材料
            </button>
          )}
          <button type="button" className="btn btn-ghost" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button type="button" className="btn btn-primary" onClick={handleSave} disabled={busy}>
            {busy ? "保存中..." : isEditing ? "保存修改" : "确认添加"}
          </button>
        </div>
      </div>
    </div>
  );
  return typeof document !== "undefined" && document.body
    ? createPortal(content, document.body)
    : content;
}
