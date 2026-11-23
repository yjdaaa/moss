import React, { useEffect, useState } from "react";
import { postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { estopTargetArgs } from "../globalEstop.js";
import { approvalPendingMessage, isApprovalRequiredResponse } from "../approvalState.js";
import { drawConnectionPayload, drawTargetLabel, drawTargetReady } from "../drawConnection.js";
import {
  JOG_STEPS_MM,
  POINTER_DURATION_OPTIONS,
  POINTER_MAX_POWER,
  POINTER_POWER_OPTIONS,
  REGISTRATION_ANCHORS,
  REGISTRATION_SESSION_KEY,
  START_DRIFT_TOLERANCE_MM,
  designPoint,
  distance,
  jobBounds,
  jogRequest,
  pointerFireConfirm,
  pointerFireRequest,
  readRegistrationSession,
  recordedPoint,
  registrationPreviewPayload,
  registrationSummary,
  writeRegistrationSession,
} from "../registrationState.js";

const DEFAULT_ANCHORS = ["lower_left", "lower_right"];

/**
 * 两点对位（2026-11-16，对齐 LightBurn Print and Cut，无摄像头）：默认收起。
 * 点动激光头（红光点，没有红光点时可开启低功率点射）到材料上两个实物标记 → 逐个“记录”（只发状态查询读位置）
 * → 生成对位预览。对位预览是新的 workflow（新文件、新 SHA-256），开刻前激光头停在第 2 个标记点上，发送照常二次确认。
 * 工作台与画板共用：`idPrefix` 区分元素 id，`sessionKey` 区分本页会话里记住的进度。
 */
export default function RegistrationPanel({
  workflow,
  connectionForm,
  disabled,
  machineMotion,
  onLog,
  onRegistered,
  idPrefix = "wb-reg",
  rootId = "wb-registration",
  sessionKey = REGISTRATION_SESSION_KEY,
}) {
  const isRegistered = Boolean(workflow?.registration);
  const [saved] = useState(() => readRegistrationSession(globalThis.sessionStorage, sessionKey));
  const [source, setSource] = useState(() => saved?.source || null);
  const [anchors, setAnchors] = useState(() => saved?.anchors || DEFAULT_ANCHORS);
  const [custom, setCustom] = useState(() => saved?.custom || [{ x: "", y: "" }, { x: "", y: "" }]);
  const [points, setPoints] = useState(() => saved?.points || [null, null]);
  const [allowScale, setAllowScale] = useState(() => Boolean(saved?.allowScale));
  const [jogArmed, setJogArmed] = useState(false);
  const [step, setStep] = useState(1);
  const [working, setWorking] = useState("");
  // 低功率点射：默认关闭、不记忆；本页第一次点射弹完整安全提醒
  const [pointerEnabled, setPointerEnabled] = useState(false);
  const [pointerPower, setPointerPower] = useState(POINTER_MAX_POWER);
  const [pointerDuration, setPointerDuration] = useState(100);
  const [pointerWarned, setPointerWarned] = useState(false);

  // 当前任务不是对位结果时，它就是对位的原任务；换了原任务就清掉旧的记录点
  const workflowId = workflow?.workflow_id;
  const workflowArtifacts = workflow?.artifacts;
  const sourceId = source?.workflow_id;
  useEffect(() => {
    if (!workflowId || isRegistered || sourceId === workflowId) return;
    setSource({ workflow_id: workflowId, bounds: jobBounds(workflowArtifacts) });
    setPoints([null, null]);
  }, [workflowId, isRegistered, workflowArtifacts, sourceId]);

  useEffect(() => {
    writeRegistrationSession(source ? { source, anchors, custom, points, allowScale } : null, globalThis.sessionStorage, sessionKey);
  }, [source, anchors, custom, points, allowScale, sessionKey]);

  const bounds = source?.bounds || null;
  const design = anchors.map((anchor, index) => designPoint(bounds, anchor, custom[index]));
  const target = drawTargetReady(connectionForm) ? drawConnectionPayload(connectionForm) : null;
  const stopTarget = estopTargetArgs(connectionForm);
  const blocked = disabled || Boolean(working);
  const log = (text, level = "info") => onLog?.("REGISTER", text, level);

  const requireTarget = () => {
    if (target) return true;
    showAlert("请先点击顶栏硬件状态或「修改连接」填写控制器 IP 或选择串口", { title: "尚未指定雕刻机", tone: "warning" });
    return false;
  };

  const armJog = async () => {
    if (!requireTarget()) return;
    const ok = await showConfirm(
      `目标设备：${drawTargetLabel(connectionForm)}\n开启后，每点一次方向按钮激光头就按所选步长立即移动（激光关闭）。\n请确认机器周围无障碍物；顶栏急停随时可用。`,
      { title: "开启点动（机器将移动）", tone: "warning", confirmText: "开启点动", cancelText: "取消" },
    );
    if (ok) setJogArmed(true);
  };

  const jog = async (dx, dy) => {
    if (!jogArmed || blocked || !stopTarget) return;
    const req = jogRequest(stopTarget, dx * step, dy * step);
    if (!req) return;
    setWorking("jog");
    try {
      const res = await postJson("/api/safe-action", req);
      if (isApprovalRequiredResponse(res)) {
        const msg = approvalPendingMessage(res, "点动");
        log(msg, "warn");
        showAlert(msg, { title: "等待桌面审批", tone: "info" });
      } else if (!(res.ok && res.data?.success)) {
        const reason = (typeof res.data?.result === "string" && res.data.result) || "点动没有完成";
        log(`点动没有完成：${reason}`, "error");
        showAlert(reason, { title: "点动没有完成", tone: "error" });
        setJogArmed(false);
      }
    } catch (e) {
      log(`点动请求异常：${e.message}`, "error");
      setJogArmed(false);
    } finally {
      setWorking("");
    }
  };

  // 低功率点射：每次都先确认（第一次带护目镜等安全提醒），后端再卡功率 / 时长上限、要求 Idle
  const firePointer = async () => {
    if (!pointerEnabled || blocked || !requireTarget()) return;
    const req = pointerFireRequest(target, pointerPower, pointerDuration);
    if (!req) {
      showAlert("点射功率或时长超出允许范围", { title: "没有点射", tone: "warning" });
      return;
    }
    const dialog = pointerFireConfirm({
      targetLabel: drawTargetLabel(connectionForm),
      power: req.power,
      durationMs: req.duration_ms,
      first: !pointerWarned,
    });
    const confirmed = await showConfirm(dialog.message, {
      title: dialog.title,
      tone: "warning",
      danger: true,
      confirmText: dialog.confirmText,
      cancelText: "取消",
    });
    if (!confirmed) return;
    setPointerWarned(true);
    setWorking("fire");
    try {
      const res = await postJson("/api/device/pointer-fire", req);
      if (res.ok && res.data?.success) {
        log(`点射完成（S${req.power}，${req.duration_ms} ms），激光已关闭`, "ok");
      } else {
        const reason = (typeof res.data?.result === "string" && res.data.result) || "点射没有完成";
        log(`点射没有完成：${reason}`, "error");
        showAlert(`${reason}\n如果激光可能仍亮着，请立即按顶栏急停或机器实体急停。`, { title: "点射没有完成", tone: "error" });
      }
    } catch (e) {
      log(`点射请求异常：${e.message}`, "error");
      showAlert(`点射请求异常：${e.message}\n请到设备旁确认激光已关闭，必要时按急停。`, { title: "点射状态未知", tone: "error" });
    } finally {
      setWorking("");
    }
  };

  const readPosition = async () => {
    const res = await postJson("/api/registration/read-position", target);
    if (res.ok && res.data?.success) {
      const point = recordedPoint(res.data.result);
      if (point) return { point };
    }
    return { error: (typeof res.data?.result === "string" && res.data.result) || "读不到激光头位置" };
  };

  const record = async (index) => {
    if (blocked || !requireTarget()) return;
    if (index === 1 && !points[0]) {
      showAlert("请先记录第 1 个标记点", { title: "顺序不对", tone: "warning" });
      return;
    }
    setWorking(`record-${index}`);
    try {
      const { point, error } = await readPosition();
      if (error) {
        log(`记录第 ${index + 1} 点失败：${error}`, "error");
        showAlert(error, { title: "没记录上", tone: "error" });
        return;
      }
      const next = [...points];
      next[index] = point;
      if (index === 0) next[1] = null;
      setPoints(next);
      log(`已记录第 ${index + 1} 个标记点（${point.frame.toUpperCase()} ${point.x.toFixed(3)}, ${point.y.toFixed(3)}）`, "ok");
    } finally {
      setWorking("");
    }
  };

  const createPreview = async () => {
    if (blocked || !requireTarget()) return;
    const payload = registrationPreviewPayload({
      workflowId: source?.workflow_id,
      design,
      measured: points,
      allowScale,
      machineMotion,
    });
    if (payload.error) {
      showAlert(payload.error, { title: "还不能生成对位预览", tone: "warning" });
      return;
    }
    setWorking("preview");
    try {
      // 新任务以第 2 个标记点为起点：先核对激光头还停在那里
      const { point, error } = await readPosition();
      if (error) {
        showAlert(`核对激光头位置失败：${error}`, { title: "没生成", tone: "error" });
        return;
      }
      if (point.frame !== points[1].frame || distance(point, points[1]) > START_DRIFT_TOLERANCE_MM) {
        showAlert(
          `激光头已离开第 2 个标记点（相差 ${distance(point, points[1]).toFixed(2)} mm）。请点动回到第 2 个标记点上，或重新记录第 2 点。`,
          { title: "激光头不在第 2 个标记点", tone: "warning" },
        );
        return;
      }
      const res = await postJson("/api/registration/preview", payload);
      if (res.ok && res.data?.success) {
        const record = res.data.result;
        for (const line of registrationSummary(record.registration)) log(line, "info");
        log(`对位预览已生成：${record.workflow_id}，尚未发送；开刻前激光头保持在第 2 个标记点`, "ok");
        await onRegistered?.(record);
      } else {
        const reason = (typeof res.data?.result === "string" && res.data.result) || "对位预览生成失败";
        log(`对位预览没有生成：${reason}`, "error");
        showAlert(reason, { title: "对位预览没有生成", tone: "error" });
      }
    } catch (e) {
      log(`对位预览请求异常：${e.message}`, "error");
    } finally {
      setWorking("");
    }
  };

  const reset = () => {
    setPoints([null, null]);
    setJogArmed(false);
    setPointerEnabled(false);
  };

  const pointText = (p) => (p ? `${p.x.toFixed(3)}, ${p.y.toFixed(3)}（${p.frame.toUpperCase()}）` : "未记录");
  const designText = (p) => (p ? `${p.x.toFixed(2)}, ${p.y.toFixed(2)} mm` : "—");

  return (
    <details className="fill-options registration-panel" id={rootId}>
      <summary>
        <span className="fill-options-title">两点对位</span>
        <span className="fill-options-summary">
          {isRegistered
            ? `当前是对位结果：${registrationSummary(workflow.registration).slice(0, 2).join("，")}`
            : "材料上已有两个标记点时，点动对准后自动旋转平移刀路（对齐 LightBurn Print and Cut）"}
        </span>
      </summary>
      <div className="fill-options-body">
        {!bounds ? (
          <p className="ob-sub fill-options-wide">先生成加工轨迹；外部导入、没有相对起点范围的刀路不能选角点，可选“自定义”。</p>
        ) : null}
        {[0, 1].map((index) => (
          <label key={`anchor-${index}`}>
            <span>设计参考点 {index + 1}（{designText(design[index])}）</span>
            <select
              id={`${idPrefix}-anchor-${index + 1}`}
              value={anchors[index]}
              disabled={blocked}
              onChange={(e) => {
                const next = [...anchors];
                next[index] = e.target.value;
                setAnchors(next);
              }}
            >
              {REGISTRATION_ANCHORS.map((opt) => (
                <option key={opt.value} value={opt.value}>{opt.label}</option>
              ))}
            </select>
            {anchors[index] === "custom" ? (
              <span className="registration-custom">
                {["x", "y"].map((axis) => (
                  <input
                    key={axis}
                    id={`${idPrefix}-custom-${index + 1}-${axis}`}
                    type="number"
                    step={0.1}
                    placeholder={axis === "x" ? "距左边 mm" : "距下边 mm"}
                    value={custom[index][axis]}
                    onChange={(e) => {
                      const next = custom.map((c) => ({ ...c }));
                      next[index][axis] = e.target.value;
                      setCustom(next);
                    }}
                  />
                ))}
              </span>
            ) : null}
          </label>
        ))}

        <div className="fill-options-wide registration-jog">
          <span className="ob-sub">点动（激光关闭）：</span>
          {jogArmed ? (
            <>
              <select id={`${idPrefix}-step`} value={step} onChange={(e) => setStep(Number(e.target.value))} disabled={blocked}>
                {JOG_STEPS_MM.map((value) => (
                  <option key={value} value={value}>{value} mm</option>
                ))}
              </select>
              {[
                ["←", -1, 0, "x-"],
                ["→", 1, 0, "x+"],
                ["↑", 0, 1, "y+"],
                ["↓", 0, -1, "y-"],
              ].map(([label, dx, dy, key]) => (
                <button
                  key={key}
                  type="button"
                  id={`${idPrefix}-jog-${key}`}
                  className="btn btn-ghost wb-sec-btn"
                  disabled={blocked}
                  onClick={() => jog(dx, dy)}
                  title={`${key.toUpperCase()} 方向移动 ${step} mm`}
                >
                  {label}
                </button>
              ))}
              <button type="button" className="btn btn-ghost wb-sec-btn" onClick={() => setJogArmed(false)}>
                关闭点动
              </button>
            </>
          ) : (
            <button type="button" id={`${idPrefix}-arm-jog`} className="btn btn-ghost wb-sec-btn" disabled={blocked} onClick={armJog}>
              开启点动
            </button>
          )}
        </div>

        {/* 低功率点射（对齐 LightBurn “Fire”）：没有红光点的机器用来看清激光头在哪；默认关闭 */}
        <div className="fill-options-wide registration-jog registration-pointer">
          <label className="checkbox-row">
            <input
              id={`${idPrefix}-pointer-enable`}
              type="checkbox"
              checked={pointerEnabled}
              disabled={blocked}
              onChange={(e) => setPointerEnabled(e.target.checked)}
            />
            <span>机器没有红光点时：低功率点射（会真实出光，每次都要确认）</span>
          </label>
          {pointerEnabled ? (
            <>
              <select
                id={`${idPrefix}-pointer-power`}
                value={pointerPower}
                disabled={blocked}
                onChange={(e) => setPointerPower(Number(e.target.value))}
                title="功率上限 1%（S10），下发时按机器 $30 换算"
              >
                {POINTER_POWER_OPTIONS.map((opt) => (
                  <option key={opt.value} value={opt.value}>{opt.label}</option>
                ))}
              </select>
              <select
                id={`${idPrefix}-pointer-duration`}
                value={pointerDuration}
                disabled={blocked}
                onChange={(e) => setPointerDuration(Number(e.target.value))}
                title="出光时长上限 200 ms"
              >
                {POINTER_DURATION_OPTIONS.map((ms) => (
                  <option key={ms} value={ms}>{ms} ms</option>
                ))}
              </select>
              <button
                type="button"
                id={`${idPrefix}-pointer-fire`}
                className="btn btn-ghost wb-sec-btn"
                disabled={blocked}
                onClick={firePointer}
                title="激光头不动，原地以极低功率打一个点后自动关光；机器须空闲（Idle）"
              >
                {working === "fire" ? "点射中…" : "点射"}
              </button>
            </>
          ) : null}
        </div>

        {[0, 1].map((index) => (
          <div key={`point-${index}`} className="registration-point">
            <button
              type="button"
              id={`${idPrefix}-record-${index + 1}`}
              className="btn btn-ghost wb-sec-btn"
              disabled={blocked || (index === 1 && !points[0])}
              onClick={() => record(index)}
              title="只读取激光头当前位置（状态查询），不移动机器"
            >
              {working === `record-${index}` ? "读取中…" : `记录第 ${index + 1} 点`}
            </button>
            <span className="ob-sub">{pointText(points[index])}</span>
          </div>
        ))}

        <label className="checkbox-row">
          <input
            id={`${idPrefix}-allow-scale`}
            type="checkbox"
            checked={allowScale}
            disabled={blocked}
            onChange={(e) => setAllowScale(e.target.checked)}
          />
          <span>允许等比缩放（打印稿有缩放时勾选；默认只旋转平移，距离差超过 5% 会拒绝）</span>
        </label>

        <p className="ob-sub fill-options-wide">
          第 2 点记录后激光头就停在第 2 个标记上，对位后的任务从这里开刻；生成前会再读一次位置核对。生成后请先走边框，确认无误再开始雕刻。
        </p>
        <div className="fill-options-wide registration-actions">
          <button
            type="button"
            id={`${idPrefix}-preview`}
            className="btn btn-ghost wb-sec-btn"
            disabled={blocked || !points[0] || !points[1] || !source?.workflow_id}
            onClick={createPreview}
          >
            {working === "preview" ? "生成中…" : "生成对位预览"}
          </button>
          <button type="button" id={`${idPrefix}-reset`} className="btn btn-ghost wb-sec-btn" disabled={blocked} onClick={reset}>
            清空记录
          </button>
        </div>
        {isRegistered ? (
          <ul className="ob-sub fill-options-wide registration-result" id={`${idPrefix}-result`}>
            {registrationSummary(workflow.registration).map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
        ) : null}
      </div>
    </details>
  );
}
