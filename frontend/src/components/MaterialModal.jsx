import React, { useState, useEffect } from "react";
import { Layers, Trash2, X } from "lucide-react";
import { postJson } from "../api/client.js";
import { showConfirm } from "../appDialog.js";

export function parseThicknessList(str) {
  return String(str || "")
    .split(/[,，\s]+/)
    .map((s) => s.trim())
    .filter(Boolean)
    .map(Number)
    .filter((n) => Number.isFinite(n) && n > 0);
}

export function buildMaterialSavePayload({
  material,
  thickList,
  power,
  speed,
  laserMode = "engrave",
  engravingMode = "raster",
}) {
  const normalizedLaserMode = laserMode === "cut" || engravingMode === "cut" ? "cut" : "engrave";
  const normalizedEngravingMode = engravingMode === "outline" ? "outline" : "raster";
  const laserMaxPower = Math.round((Number(power) / 100) * 1000);
  const feedRate = Math.round(Number(speed) * 60);
  const cell = {
    laser_min_power: 0,
    laser_max_power: laserMaxPower,
    feed_rate: feedRate,
    travel_rate: 3000,
    threshold: -1,
    passes: 1,
    pixel_size_mm: 0.1,
  };
  const thicknesses = {};
  for (const th of thickList) {
    thicknesses[String(th)] =
      normalizedLaserMode === "cut"
        ? { cut: { ...cell } }
        : { engrave: { [normalizedEngravingMode]: { ...cell } } };
  }
  return {
    action: "save",
    material,
    thicknesses,
    power_percent: Number(power),
    speed_mms: Number(speed),
    laser_mode: normalizedLaserMode,
    engraving_mode: normalizedLaserMode === "cut" ? "" : normalizedEngravingMode,
    // 兼容单厚度字段
    thickness_mm: thickList[0],
    laser_max_power: laserMaxPower,
    feed_rate: feedRate,
  };
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

function MaterialModalDialog({ initialData, onClose, onSaved, onDeleted }) {
  const isEditing = Boolean(initialData && initialData.name);
  const [name, setName] = useState("");
  const [thicknessStr, setThicknessStr] = useState("");
  const [power, setPower] = useState(50);
  const [speed, setSpeed] = useState(100);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (initialData) {
      setName(initialData.name || "");
      const thicks = Array.isArray(initialData.thicknesses)
        ? initialData.thicknesses.join(", ")
        : String(initialData.thickness_mm || "3");
      setThicknessStr(thicks);
      setPower(Number(initialData.power_percent || 50));
      setSpeed(Number(initialData.speed_mms || 100));
    } else {
      setName("");
      setThicknessStr("3");
      setPower(50);
      setSpeed(100);
    }
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

    setBusy(true);
    setError("");

    // D9: 原子批量保存所有厚度，单次提交
    const payload = buildMaterialSavePayload({
      material: trimmedName,
      thickList,
      power,
      speed,
      laserMode: initialData?.laser_mode,
      engravingMode: initialData?.engraving_mode,
    });

    try {
      const res = await postJson("/api/material-lab/manage", payload);
      setBusy(false);

      if (res.ok && res.data?.success) {
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

  return (
    <div className="mat-modal-backdrop" id="mat-modal">
      <div className="mat-modal" role="dialog" aria-modal="true">
        <div className="mat-modal-head">
          <h3 id="mat-modal-title">
            <Layers size={18} className="mat-form-icon" />
            <span>{isEditing ? "编辑材料" : "添加材料"}</span>
          </h3>
          <button className="mat-modal-close" onClick={onClose} title="关闭">
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
              <label htmlFor="mat-f-speed">推荐速度（mm/s）</label>
              <input
                id="mat-f-speed"
                className="text-input"
                type="number"
                min="1"
                max="600"
                step="1"
                value={speed}
                onChange={(e) => setSpeed(e.target.value)}
                placeholder="10 - 200"
              />
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
              className="btn btn-ghost mat-del-btn mat-form-btn"
              onClick={handleDelete}
              disabled={busy}
            >
              <Trash2 size={14} className="mat-form-icon-2" />
              删除材料
            </button>
          )}
          <button className="btn btn-ghost" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button className="btn btn-primary" onClick={handleSave} disabled={busy}>
            {busy ? "保存中..." : isEditing ? "保存修改" : "确认添加"}
          </button>
        </div>
      </div>
    </div>
  );
}
