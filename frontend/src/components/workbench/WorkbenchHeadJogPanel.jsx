import React, { useRef, useState } from "react";
import { postJson } from "../../api/client.js";
import { showAlert, showConfirm } from "../../appDialog.js";
import { readFoldOpen, writeFoldOpen } from "../../foldState.js";
import {
  HEAD_JOG_FOLD_ID,
  WORKBENCH_JOG_STEPS_MM,
  createHeadSafeActionRunner,
  resolveHeadJogGuard,
} from "../../registrationState.js";

const JOG_DIRECTIONS = [
  { key: "x-minus", label: "← X−", dx: -1, dy: 0, dirText: "X−" },
  { key: "x-plus", label: "X+ →", dx: 1, dy: 0, dirText: "X+" },
  { key: "y-plus", label: "↑ Y+", dx: 0, dy: 1, dirText: "Y+" },
  { key: "y-minus", label: "↓ Y−", dx: 0, dy: -1, dirText: "Y−" },
];

/**
 * 雕刻工作台「加工监视舱」内的“现场移头与对刀原点”折叠面板（2026-11-23 r60 B3）：
 * - 默认收起，展开状态按 `foldState`（`moss_ui_fold_wb-head-jog`）记忆；
 * - 只提供单步 1 / 5 / 10 mm 的 X±/Y± 点动、「设为工作原点」与「解锁 $X」，不提供连续点动、不提供出光；
 * - 全部动作先弹 `showConfirm` 二次确认，再走 `/api/safe-action` 与严格模式 R3 审批门禁；
 * - 未连接 / 未探测成功 / 正在加工或在途执行时禁用全部按钮并显示原因，同步锁拦截快速连点。
 */
export function WorkbenchHeadJogPanel({
  connectionForm,
  connectionStatus,
  busy = false,
  framingBusy = false,
  overlayStatus = "pending",
  activeJobs = [],
  onLog,
}) {
  const [open, setOpen] = useState(() => readFoldOpen(HEAD_JOG_FOLD_ID, false));
  const [step, setStep] = useState(1);
  const [workingAction, setWorkingAction] = useState("");

  const logRef = useRef(onLog);
  logRef.current = onLog;

  const runnerRef = useRef(null);
  if (!runnerRef.current) {
    runnerRef.current = createHeadSafeActionRunner({
      confirmFn: showConfirm,
      postFn: postJson,
      alertFn: showAlert,
      onLog: (text, level) => logRef.current?.("SAFE", text, level),
      onWorkingChange: setWorkingAction,
    });
  }

  const guard = resolveHeadJogGuard({
    connectionForm,
    connectionStatus,
    busy,
    framingBusy,
    overlayStatus,
    activeJobs,
    workingAction,
  });

  const handleToggle = (e) => {
    e.preventDefault();
    const next = !open;
    setOpen(next);
    writeFoldOpen(HEAD_JOG_FOLD_ID, next);
  };

  return (
    <details className="fill-options wb-jog-panel" id="wb-head-jog" open={open}>
      <summary id="wb-head-jog-toggle" onClick={handleToggle}>
        <span className="fill-options-title">现场移头与对刀原点</span>
        <span className="fill-options-summary">
          {guard.disabled ? guard.shortSummary : `步长 ${step} mm · 激光关闭 · 每次操作先确认`}
        </span>
      </summary>
      <div className="wb-jog-body">
        <div className="wb-jog-controls" role="group" aria-label="现场移头与对刀原点控制">
          <label className="wb-jog-step-label" htmlFor="wb-jog-step">
            <span>步长</span>
            <select
              id="wb-jog-step"
              className="select-input wb-compact-select"
              value={step}
              disabled={guard.disabled}
              onChange={(e) => {
                const next = Number(e.target.value);
                if (WORKBENCH_JOG_STEPS_MM.includes(next)) {
                  setStep(next);
                }
              }}
            >
              {WORKBENCH_JOG_STEPS_MM.map((mm) => (
                <option key={mm} value={mm}>
                  {mm} mm
                </option>
              ))}
            </select>
          </label>
          <div className="wb-jog-dirs" role="group" aria-label="点动方向">
            {JOG_DIRECTIONS.map((item) => (
              <button
                key={item.key}
                type="button"
                id={`wb-jog-${item.key}`}
                className="btn btn-ghost wb-sec-btn wb-jog-btn"
                disabled={guard.disabled}
                onClick={() => void runnerRef.current({ kind: "jog", dxUnit: item.dx, dyUnit: item.dy, stepMm: step }, guard)}
                title={guard.disabled ? guard.reason : `${item.dirText} 方向移动 ${step} mm（需二次确认，激光关闭）`}
              >
                {item.label}
              </button>
            ))}
          </div>
          <div className="wb-jog-origin-actions">
            <button
              type="button"
              id="wb-jog-set-origin"
              className="btn btn-ghost wb-sec-btn wb-jog-btn"
              disabled={guard.disabled}
              onClick={() => void runnerRef.current({ kind: "set_origin" }, guard)}
              title={guard.disabled ? guard.reason : "把当前激光头位置设为工作原点 G92 X0 Y0（需二次确认）"}
            >
              设为工作原点
            </button>
            <button
              type="button"
              id="wb-jog-unlock"
              className="btn btn-ghost wb-sec-btn wb-jog-btn"
              disabled={guard.disabled}
              onClick={() => void runnerRef.current({ kind: "unlock" }, guard)}
              title={guard.disabled ? guard.reason : "发送 $X 解除控制器报警锁定（需二次确认）"}
            >
              解锁 $X
            </button>
          </div>
        </div>
        <p
          className={`ob-sub wb-jog-note${guard.disabled ? " is-disabled-reason" : ""}`}
          id="wb-jog-reason"
          role="status"
        >
          {guard.disabled
            ? guard.reason
            : "每次移动、设原点或解锁均会先弹窗确认（激光全程关闭；严格模式开启时需桌面审批，请人在机器旁操作）。"}
        </p>
      </div>
    </details>
  );
}
