import React from "react";
import { Check, Cpu, Flame, UploadCloud, X } from "lucide-react";
import { getSendOverlayStage } from "../state/statusNormalize.js";

/**
 * 发送覆盖层（STAGE_MAP 四步映射见 statusNormalize.js，前端行为契约 §2）。
 * 可选 `onEmergencyStop`：发送全程显示“急停 0x18”（审批执行时使用，技术书 §9.5.2）；
 * 可选 `cancelLabel` / `badge`：取消按钮文字与标题旁标签。不传时与原来完全一致。
 */
export function SendOverlay({
  visible,
  status,
  onCancel,
  onClose,
  progress = null,
  onEmergencyStop = null,
  cancelLabel = "取消任务",
  badge = "",
  safetyNotice = "",
  keepEmergencyStop = false,
}) {
  if (!visible) return null;

  const stage = getSendOverlayStage(status);
  const C = 402.12; // 2 * PI * 64

  // Arc stroke offset calculation
  let strokeDashoffset = "0";
  if (stage.terminal) {
    strokeDashoffset = stage.isSuccess ? "0" : String(C * 0.75);
  }

  const steps = [
    { label: "校验", icon: Check },
    { label: "生成", icon: Cpu },
    { label: "传输", icon: UploadCloud },
    { label: "执行", icon: Flame },
  ];

  return (
    <div className={`send-overlay ${visible ? "show" : ""}`} id="send-overlay">
      <div className="send-ring">
        <svg viewBox="0 0 150 150">
          <circle className="track" cx="75" cy="75" r="64" fill="none" strokeWidth="3" />
          <circle
            className={`arc ${!stage.terminal ? "indeterminate" : ""}`}
            id="send-arc"
            cx="75"
            cy="75"
            r="64"
            fill="none"
            strokeWidth="3"
            style={{ strokeDashoffset }}
          />
        </svg>
        <div className="core" id="send-core">
          {stage.label}
        </div>
      </div>

      <div className="send-title" id="send-title">
        {stage.title}
        {badge && <span className="send-badge">{badge}</span>}
      </div>

      <div className="send-desc">
        STATUS <span className="pct" id="send-pct">{stage.normalized.toUpperCase()}</span> · 请勿关闭页面
      </div>
      {progress && progress.total > 0 && (
        <div className="send-desc" style={{ marginTop: "6px", fontFamily: "var(--font-mono)" }}>
          {progress.target ? `${progress.target} · ` : ""}
          已确认 <span className="pct">{progress.sent}</span> / {progress.total} 行
          {" "}({Math.min(100, Math.round((progress.sent / progress.total) * 100))}%)
        </div>
      )}

      <div className="send-stage">
        {steps.map((s, i) => {
          const Icon = s.icon;
          let cls = "st";
          if (stage.isSuccess) {
            cls += " done";
          } else if (i < stage.step) {
            cls += " done";
          } else if (i === stage.step) {
            cls += stage.terminal ? " failed" : " active";
          }

          let lineDone = false;
          if (i < steps.length - 1) {
            lineDone = stage.isSuccess || i < stage.step;
          }

          return (
            <React.Fragment key={s.label}>
              <div className={cls} id={`st-${i}`}>
                <Icon size={14} />
                <span>{s.label}</span>
              </div>
              {i < steps.length - 1 && (
                <div
                  className={`st-line ${lineDone ? "done" : ""}`}
                  id={`stl-${i}`}
                />
              )}
            </React.Fragment>
          );
        })}
      </div>

      <div style={{ display: "flex", gap: "10px", marginTop: "14px", alignItems: "center" }}>
        {!stage.terminal ? (
          <>
            <button className="send-cancel" id="send-cancel" onClick={onCancel}>
              {cancelLabel}
            </button>
          </>
        ) : (
          <button className="send-cancel" style={{ borderColor: "var(--line)" }} onClick={onClose}>
            关闭窗口
          </button>
        )}
        {onEmergencyStop && (!stage.terminal || keepEmergencyStop) && (
          <button type="button" className="ob-estop" id="send-estop" onClick={onEmergencyStop}>急停 0x18</button>
        )}
      </div>
      {safetyNotice && <div className="send-desc" role="alert">{safetyNotice}</div>}
      {onEmergencyStop && (!stage.terminal || keepEmergencyStop) && (
        <div className="send-desc send-estop-note">0x18 走实时通道立即复位控制器，不代替控制器上的硬件急停按钮。</div>
      )}
    </div>
  );
}
