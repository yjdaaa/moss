import React, { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import { getJson, postJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { notifyMaterialsUpdated } from "../materialSelectState.js";
import {
  buildPatternPreviewPayload,
  currentWizardStep,
  kerfFromMeasurement,
  markWizardStep,
  mergeScanOffsetTable,
  overscanSuggestion,
  scanOffsetRowsFromMeasurements,
  stepRecord,
  timeCalibrationSummary,
  wizardMaterialKey,
  wizardStepsForThickness,
  patternSaveTarget,
  resolveOpenWizardStep,
  resolveWizardExpanded,
  toggleWizardStep,
  wizardProgress,
  writeWizardFold,
} from "../calibrationWizard.js";
import {
  fillSettingsPayload,
  machineMotionPayload,
  readFillSettings,
  readMachineMotion,
  writeFillSettings,
} from "../fillOptions.js";

/**
 * 首次标定向导（r40，2026-11-16）：按顺序串起线距测试、功率 × 速度矩阵、双向扫描偏移、过扫描检查、
 * 切缝补偿与工时校准。每步只生成测试刀路，发送一律走下方「校准矩阵」的确认框（绝不自动发送）；
 * 刻完由用户选格或填测量结果，MOSS 再把结果存到材料库条目或本机设置。每步都可跳过。
 */
export function CalibrationWizard({
  wizard,
  onWizardChange,
  material,
  thickness,
  readOnly = false,
  onPrepareMatrix,
  onGeneratePattern,
  patternPreview,
  patternStale = false,
  busy = false,
  running = false,
  onSend,
}) {
  const materialKey = wizardMaterialKey(material, thickness);
  const visibleSteps = wizardStepsForThickness(thickness);
  const progress = wizardProgress(wizard, materialKey);
  const allDone = progress.done + progress.skipped >= progress.total;
  const current = currentWizardStep(wizard, materialKey);
  // undefined = 跟随当前步骤；"" = 用户全部收起；其余 = 用户点开的那一步
  const [openStep, setOpenStep] = useState(undefined);
  // 全部完成或用户点“稍后”收起后默认收起，记住在 localStorage（try/catch）；
  // 完成全部步骤自动收起时不写入 localStorage，切换到尚未标定的新材料时仍会自动展开
  const syncKey = `${materialKey}::${allDone}`;
  const [prevSyncKey, setPrevSyncKey] = useState(syncKey);
  const [expanded, setExpanded] = useState(() => resolveWizardExpanded(wizard, materialKey));
  if (prevSyncKey !== syncKey) {
    setPrevSyncKey(syncKey);
    setExpanded(resolveWizardExpanded(wizard, materialKey));
  }
  const rawActive = resolveOpenWizardStep(openStep, current);
  const active = rawActive && !visibleSteps.some((s) => s.id === rawActive) ? (current || "time") : rawActive;
  const [speedsText, setSpeedsText] = useState("");
  const [measured, setMeasured] = useState({});
  const [overscanAnswer, setOverscanAnswer] = useState("");
  const [overscanNext, setOverscanNext] = useState("");
  const [kerfMeasured, setKerfMeasured] = useState("");
  const [timeStatus, setTimeStatus] = useState(null);
  const [note, setNote] = useState("");

  const label = `${material || "未选材料"} · ${thickness === "surface" ? "表面通用" : `${thickness || "-"}mm`}`;
  const pattern = patternPreview?.wizard_pattern && patternPreview.test_type === active ? patternPreview : null;

  useEffect(() => {
    if (active !== "time") return undefined;
    let cancelled = false;
    getJson("/api/time-calibration").then((res) => {
      if (!cancelled && res.ok && res.data?.success) setTimeStatus(res.data.result);
    });
    return () => {
      cancelled = true;
    };
  }, [active]);

  const mark = (id, status, stepNote = "") => {
    onWizardChange(markWizardStep(wizard, id, status, { materialKey, note: stepNote }));
    setNote("");
    // 完成 / 跳过后回到跟随下一步
    setOpenStep(undefined);
  };

  const generate = async (testType) => {
    const settings = readFillSettings();
    const fill = testType === "overscan" ? fillSettingsPayload(settings) : null;
    if (fill?.error) {
      setNote(`填充设置有误：${fill.error}`);
      return;
    }
    const speeds = speedsText.split(/[,，\s]+/).filter(Boolean);
    const built = buildPatternPreviewPayload({
      testType,
      material,
      thickness,
      speeds: speeds.length ? speeds : null,
      fillOptions: fill?.fill_options || null,
      motion: machineMotionPayload(settings, readMachineMotion()),
    });
    if (built.error) {
      setNote(built.error);
      return;
    }
    const record = await onGeneratePattern(built.payload);
    if (record) {
      setMeasured({});
      setNote("测试刀路已生成（只生成，不出光）。确认样板放好后点「发送测试」，会先弹出确认框。");
    }
  };

  const saveLayer = async (body, okText) => {
    // 结果写回生成测试时参数所在的材料库条目（之后在左侧换了材料也不会写错条目）；
    // 测试参数回退到“表面通用”或最近厚度时，写回的就是那一条
    const target = patternSaveTarget(pattern, thickness);
    const res = await postJson("/api/material-lab/manage", {
      action: "save_layer_options",
      material: pattern?.material || material,
      ...target.request,
      merge: true,
      ...body,
    });
    if (res.ok && res.data?.success) {
      notifyMaterialsUpdated();
      setNote(target.note ? `${okText}${target.note}` : okText);
      return true;
    }
    showAlert(res.data?.result || res.error || "保存失败", { title: "保存到材料库失败", tone: "error" });
    return false;
  };

  const saveScanOffsets = () => {
    const cells = Array.isArray(pattern?.cells) ? pattern.cells : [];
    const parsed = scanOffsetRowsFromMeasurements(cells.map((c) => ({ speed: c.speed, measured: measured[c.speed] })));
    if (parsed.error) {
      setNote(parsed.error);
      return;
    }
    const settings = readFillSettings();
    const table = mergeScanOffsetTable(settings.scanOffsetTable, parsed.rows);
    writeFillSettings({ ...settings, scanOffsetTable: table });
    mark("scan_offset", "done", `偏移表 ${parsed.rows.map((r) => `${r.speed}→${r.offset}`).join("，")}`);
  };

  const saveOverscan = async () => {
    const value = Number(overscanNext);
    if (!Number.isFinite(value) || value < 0 || value > 20) {
      setNote("过扫描须在 0 ~ 20 mm 之间");
      return;
    }
    const ok = await saveLayer(
      { laser_mode: "engrave", engraving_mode: "raster", fill_options: { overscan_mm: value } },
      `已把过扫描 ${value} mm 存进材料库「${label} 光栅」，可再生成一次检查确认`,
    );
    if (ok) setOverscanAnswer("");
  };

  const saveKerf = async () => {
    const result = kerfFromMeasurement(pattern?.square_mm ?? 20, kerfMeasured);
    if (result.error) {
      setNote(result.error);
      return;
    }
    const ok = await saveLayer(
      { laser_mode: "cut", cut_options: { kerf_mm: result.kerfMm } },
      `已把切缝补偿 ${result.kerfMm} mm 存进材料库「${label} 切割」`,
    );
    if (ok) mark("kerf", "done", `切缝宽 ${result.widthMm} mm，补偿 ${result.kerfMm} mm`);
  };

  const resetTime = async () => {
    const ok = await showConfirm("清除所有机器的工时校准记录？之后的预估回到按加速度模型计算。", {
      title: "清除工时校准",
      confirmText: "清除",
      cancelText: "取消",
      tone: "warning",
    });
    if (!ok) return;
    const res = await postJson("/api/time-calibration", { action: "reset" });
    if (res.ok && res.data?.success) {
      setTimeStatus(res.data.result);
    } else {
      showAlert(res.data?.result || res.error || "清除工时校准失败", { title: "清除失败", tone: "error" });
    }
  };

  const sendButton = (stepId) =>
    pattern && pattern.test_type === stepId ? (
      <button
        type="button"
        className="btn btn-primary"
        onClick={onSend}
        disabled={busy || running || patternStale || readOnly}
        title="发送前会弹出确认框；以当前激光头位置为左下角"
      >
        {running ? "正在下发测试…" : "发送测试（先确认）"}
      </button>
    ) : null;

  const kerfResult = pattern?.test_type === "kerf" ? kerfFromMeasurement(pattern.square_mm ?? 20, kerfMeasured) : null;
  const suggestion = overscanSuggestion(pattern?.pattern?.overscan_mm, { auto: pattern?.pattern?.overscan_auto !== false });

  const body = (stepId, trailingAction) => {
    switch (stepId) {
      case "interval":
      case "matrix":
        return (
          <>
            <p>
              {stepId === "interval"
                ? "在下方「校准矩阵」里刻一排不同线距的填充小方块。刻完点选扫描线刚好相接、没有缝也不重叠的那块，线距会写进材料库。"
                : "用上一步存好的线距刻功率 × 速度网格。刻完点选颜色深浅最合适的格子，功率、速度与遍数会写进材料库。"}
            </p>
            <div className="lab-wizard-actions">
              <button type="button" className="btn btn-ghost" onClick={() => onPrepareMatrix(stepId)} disabled={readOnly}>
                {stepId === "interval" ? "设置线距测试" : "设置功率 × 速度矩阵"}
              </button>
              <button type="button" className="btn btn-ghost" onClick={() => mark(stepId, "done")} disabled={readOnly}>
                已选好格子，完成本步
              </button>
              {trailingAction}
            </div>
          </>
        );
      case "scan_offset":
        return (
          <>
            <p>
              几种速度各刻一个实心小方块（偏移 0、来回横扫）。刻完看每块左右两条边：来回两行出光错开多少 mm 就填多少
              （每行都往自己的行进方向拖后填正数，提前填负数，对齐了填 0）。结果按速度存进本机的扫描偏移表，所有材料通用。
            </p>
            <label className="lab-wizard-field">
              测试速度（mm/min，逗号分隔，留空用推荐速度的 0.5 / 1 / 1.5 倍）
              <input className="text-input" value={speedsText} onChange={(e) => setSpeedsText(e.target.value)} placeholder="例如 1000, 2000, 3000" />
            </label>
            <div className="lab-wizard-actions">
              <button type="button" className="btn btn-ghost" onClick={() => generate("scan_offset")} disabled={busy || running || readOnly}>
                生成测试方块
              </button>
              {sendButton("scan_offset")}
              {trailingAction}
            </div>
            {pattern ? (
              <div className="lab-wizard-measure">
                {pattern.cells.map((cell) => (
                  <label key={cell.speed} className="lab-wizard-field">
                    速度 {cell.speed} 的错位（mm）
                    <input
                      className="text-input"
                      type="number"
                      step="0.01"
                      min={-2}
                      max={2}
                      value={measured[cell.speed] ?? ""}
                      onChange={(e) => setMeasured((prev) => ({ ...prev, [cell.speed]: e.target.value }))}
                    />
                  </label>
                ))}
                <button type="button" className="btn btn-primary" onClick={saveScanOffsets} disabled={readOnly}>
                  存进扫描偏移表
                </button>
              </div>
            ) : null}
          </>
        );
      case "overscan":
        return (
          <>
            <p>
              用当前填充设置（过扫描、扫描偏移）刻两个实心方块：左块不过扫描，右块按当前设置过扫描。对比两块的左右两头。
            </p>
            <div className="lab-wizard-actions">
              <button type="button" className="btn btn-ghost" onClick={() => generate("overscan")} disabled={busy || running || readOnly}>
                生成检查方块
              </button>
              {sendButton("overscan")}
              {trailingAction}
            </div>
            {pattern ? (
              <div className="lab-wizard-measure">
                <span>右块的两头：</span>
                <button type="button" className="btn btn-ghost" onClick={() => mark("overscan", "done", "两头均匀")} disabled={readOnly}>
                  均匀，没有发黑
                </button>
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() => {
                    setOverscanAnswer("dark");
                    setOverscanNext(String(suggestion.nextMm));
                  }}
                  disabled={readOnly}
                >
                  仍然发黑
                </button>
              </div>
            ) : null}
            {overscanAnswer === "dark" ? (
              <div className="lab-wizard-measure">
                <p>{suggestion.text}</p>
                <label className="lab-wizard-field">
                  新的过扫描（mm）
                  <input className="text-input" type="number" step="0.1" min={0} max={20} value={overscanNext} onChange={(e) => setOverscanNext(e.target.value)} />
                </label>
                <button type="button" className="btn btn-primary" onClick={saveOverscan} disabled={readOnly}>
                  存进材料库（光栅条目）
                </button>
              </div>
            ) : null}
          </>
        );
      case "kerf":
        return (
          <>
            <p>
              按这个厚度的切割参数切一个 20 mm 方块（不做补偿）。用卡尺量切下来的方块边长：切缝宽度 = 20 − 实测，
              每条边各被烧掉一半，所以切缝补偿 = (20 − 实测) ÷ 2，存进该厚度的切割条目。
            </p>
            <div className="lab-wizard-actions">
              <button type="button" className="btn btn-ghost" onClick={() => generate("kerf")} disabled={busy || running || readOnly}>
                生成切缝方块
              </button>
              {sendButton("kerf")}
              {trailingAction}
            </div>
            {pattern ? (
              <div className="lab-wizard-measure">
                <label className="lab-wizard-field">
                  量得的方块边长（mm）
                  <input className="text-input" type="number" step="0.01" value={kerfMeasured} onChange={(e) => setKerfMeasured(e.target.value)} />
                </label>
                {kerfResult && !kerfResult.error ? (
                  <span>切缝宽 {kerfResult.widthMm} mm → 切缝补偿 {kerfResult.kerfMm} mm</span>
                ) : null}
                <button type="button" className="btn btn-primary" onClick={saveKerf} disabled={readOnly}>
                  保存为切缝补偿
                </button>
              </div>
            ) : null}
          </>
        );
      case "time":
      default:
        return (
          <>
            <p>
              不用专门刻：每次正常加工完成（不是取消、急停或断点续雕，且不少于 20 秒），MOSS 会比较预估与实际用时，
              按机器保留最近 10 次，满 2 次后用中位数校准之后的预估（限制在 0.5 ~ 3 倍）。
            </p>
            <p className="lab-wizard-status">{timeCalibrationSummary(timeStatus)}</p>
            <div className="lab-wizard-actions">
              <button type="button" className="btn btn-ghost" onClick={() => mark("time", "done")} disabled={readOnly}>
                知道了，完成本步
              </button>
              {timeStatus?.targets?.length && !readOnly ? (
                <button type="button" className="btn btn-ghost" onClick={resetTime}>
                  清除校准记录
                </button>
              ) : null}
              {trailingAction}
            </div>
          </>
        );
    }
  };

  const toggleWizardExpanded = () => {
    const nextOpen = !expanded;
    setExpanded(nextOpen);
    // 6/6 已全部完成时回看展开或收起只作用于当前会话视图，不写入全局 "closed"，避免切换到未标定新材料时不再自动展开
    if (!allDone) {
      writeWizardFold(nextOpen);
    }
  };

  const handleDeferOrCollapse = () => {
    setExpanded(false);
    if (!allDone) {
      writeWizardFold(false);
    }
  };

  return (
    <section
      className="hud-card lab-wizard"
      id="lab-wizard"
      data-open={expanded ? "true" : "false"}
    >
      <div className="lab-wizard-head">
        <button
          type="button"
          className="lab-wizard-toggle"
          id="lab-wizard-toggle"
          aria-expanded={expanded}
          aria-controls={expanded ? "lab-wizard-steps" : undefined}
          onClick={toggleWizardExpanded}
        >
          <span className="lab-wizard-title">首次标定向导</span>
          <span className="lab-wizard-progress">
            {label} · 已完成 {progress.done} / {progress.total}
            {progress.skipped ? `，跳过 ${progress.skipped}` : ""}
          </span>
          {!expanded && !allDone ? (
            <span className="lab-wizard-continue">继续</span>
          ) : null}
        </button>
        {expanded ? (
          <button
            type="button"
            className="btn btn-ghost lab-wizard-defer"
            id="lab-wizard-defer"
            onClick={handleDeferOrCollapse}
          >
            {allDone ? "收起" : "稍后"}
          </button>
        ) : null}
      </div>
      {expanded ? (
        <>
          <ol className="lab-wizard-steps" id="lab-wizard-steps">
            {visibleSteps.map((step, index) => {
              const record = stepRecord(wizard, step.id, materialKey);
              const isActive = step.id === active;
              const trailingAction = !readOnly ? (
                record ? (
                  <button type="button" className="btn btn-ghost" onClick={() => mark(step.id, null)}>
                    重做这一步
                  </button>
                ) : (
                  <button type="button" className="btn btn-ghost" onClick={() => mark(step.id, "skipped")}>
                    跳过这一步
                  </button>
                )
              ) : null;
              return (
                <li key={step.id} className={`lab-wizard-step${isActive ? " on" : ""}${record ? ` ${record.status}` : ""}`}>
                  <button
                    type="button"
                    className="lab-wizard-step-head"
                    aria-expanded={isActive}
                    aria-controls={`lab-wizard-body-${step.id}`}
                    title={isActive ? "收起这一步" : "展开这一步"}
                    onClick={() => setOpenStep(toggleWizardStep(active, step.id))}
                  >
                    <b>{index + 1}. {step.title}</b>
                    <span>
                      {record?.status === "done" ? "已完成" : record?.status === "skipped" ? "已跳过" : step.id === current ? "当前" : "待做"}
                      {step.scope === "machine" ? " · 跟机器" : " · 跟材料"}
                      <ChevronDown size={14} className="lab-wizard-chevron" aria-hidden="true" />
                    </span>
                  </button>
                  {isActive ? (
                    <div className="lab-wizard-body" id={`lab-wizard-body-${step.id}`}>
                      {record?.note ? <p className="lab-wizard-status">上次结果：{record.note}</p> : null}
                      {body(step.id, trailingAction)}
                      {note ? <p className="lab-wizard-status" role="status">{note}</p> : null}
                    </div>
                  ) : null}
                </li>
              );
            })}
          </ol>
          {readOnly ? <p className="lab-wizard-status">已连接电脑 MOSS：向导只能查看，测试与保存请在电脑上进行。</p> : null}
        </>
      ) : null}
    </section>
  );
}
