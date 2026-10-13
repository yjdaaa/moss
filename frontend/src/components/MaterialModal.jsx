import React, { useState, useEffect } from "react";
import { Layers, Trash2, X } from "lucide-react";
import { postJson } from "../api/client.js";
import { showConfirm } from "../appDialog.js";

export function MaterialModal({ visible, initialData, onClose, onSaved, onDeleted }) {
  if (!visible) return null;

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
  }, [initialData, visible]);

  const parseThicknessList = (str) => {
    return str
      .split(/[,，\s]+/)
      .map((s) => s.trim())
      .filter(Boolean)
      .map(Number)
      .filter((n) => Number.isFinite(n) && n > 0);
  };

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
    const payload = {
      action: "save",
      material: trimmedName,
      thicknesses: thickList,
      power_percent: Number(power),
      speed_mms: Number(speed),
      // 兼容单厚度字段
      thickness_mm: thickList[0],
      laser_max_power: Math.round((Number(power) / 100) * 1000),
      feed_rate: Math.round(Number(speed) * 60),
    };

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

    const thickList = parseThicknessList(thicknessStr);
    const payload = {
      action: "delete",
      material: trimmedName,
      thickness_mm: thickList[0] || 0,
      confirmed: true,
    };

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
              type="text"
              placeholder="例如：椴木 / 亚克力"
              value={name}
              disabled={isEditing}
              onChange={(e) => setName(e.target.value)}
              autoComplete="off"
            />
          </div>

          <div className="mat-field">
            <label htmlFor="mat-f-thick">厚度（mm）</label>
            <input
              id="mat-f-thick"
              type="text"
              placeholder="例如：3, 5（多个厚度用逗号隔开）"
              value={thicknessStr}
              onChange={(e) => setThicknessStr(e.target.value)}
              autoComplete="off"
            />
          </div>

          <div className="mat-field-row">
            <div className="mat-field">
              <label htmlFor="mat-f-power">功率（%）</label>
              <input
                id="mat-f-power"
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
              <label htmlFor="mat-f-speed">速度（mm/s）</label>
              <input
                id="mat-f-speed"
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
            保存后将原子写入后端材料库（D9 支持多厚度单次提交），用于推荐雕刻与切割加工参数。
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
