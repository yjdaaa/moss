import test from "node:test";
import assert from "node:assert/strict";
import {
  actualParamsText,
  checkDerivedWorkbenchAdoption,
  DEFAULT_IMAGE_EXTRAS,
  IMAGE_MODE_OPTIONS,
  imageModeFields,
  imageParamsPayload,
  isWorkbenchPreviewStale,
  manualParamsPayload,
  materialRecommendationModes,
  resolveTextStyle,
  workbenchConfirmSendPrompt,
  workbenchFeedbackInputs,
  workbenchInputSignature,
  workbenchResumeSendPrompt,
  workbenchStatusLabel,
} from "../src/workbenchParams.js";
import { getCurrentDialog, resetDialogs, resolveDialog, showPrompt, subscribe } from "../src/appDialog.js";
import { readFileSync } from "node:fs";
import { readPageSource } from "./pageSource.mjs";

test("n73 工作台自定义参数：没开不提交，填对转成 manual_params，越界给出原因", () => {
  assert.equal(manualParamsPayload({ enabled: false, powerPercent: 60, feedRate: 1200, passes: 1 }), null);
  assert.deepEqual(manualParamsPayload({ enabled: true, powerPercent: "60", feedRate: "1500", passes: "2" }), {
    power_percent: 60, feed_rate: 1500, passes: 2,
  });
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 0, feedRate: 1200, passes: 1 }).error, /功率/);
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 60, feedRate: 50, passes: 1 }).error, /速度/);
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 60, feedRate: 1200.5, passes: 1 }).error, /速度/);
  assert.match(manualParamsPayload({ enabled: true, powerPercent: 60, feedRate: 1200, passes: 6 }).error, /遍数/);
});

test("n73 刀路摘要显示这次实际用的参数并注明来源与文字刻法", () => {
  const wf = { artifacts: { actual_params: { laser_max_power: 370, feed_rate: 900, passes: 2, power_scale: 1000 } }, input: {} };
  assert.equal(actualParamsText(wf), "功率 37% · 900 mm/min · 2 遍（材料库推荐）");
  wf.input.manual_params = { power_percent: 37 };
  assert.equal(actualParamsText(wf), "功率 37% · 900 mm/min · 2 遍（本次自定义）");
  wf.artifacts.text_fill = "fill";
  assert.equal(actualParamsText(wf), "实心填充 · 功率 37% · 900 mm/min · 2 遍（本次自定义）");
  assert.equal(actualParamsText({}), "");
});

test("文字刻法映射对齐后端 text_fill，且切换图片时材料推荐自动回到 raster", () => {
  assert.deepEqual(resolveTextStyle("outline"), {
    style: "outline", label: "轮廓描边", laserMode: "engrave", engravingMode: "outline", textFill: "outline",
  });
  assert.deepEqual(resolveTextStyle("fill"), {
    style: "fill", label: "实心填充（矢量密排线）", laserMode: "engrave", engravingMode: "raster", textFill: "fill",
  });
  // 兼容旧版 localStorage 里残留的 raster，自动归一为真实的 fill 实心填充
  assert.equal(resolveTextStyle("raster").textFill, "fill");
  assert.deepEqual(resolveTextStyle("hatch"), {
    style: "hatch", label: "排线填充（线距更宽、更快）", laserMode: "engrave", engravingMode: "outline", textFill: "hatch",
  });
  assert.deepEqual(resolveTextStyle("cut"), {
    style: "cut", label: "外框切割", laserMode: "cut", engravingMode: "cut", textFill: "outline",
  });

  assert.deepEqual(materialRecommendationModes({ activeTool: "text", textStyle: "cut" }), {
    laserMode: "cut", engravingMode: null,
  });
  assert.deepEqual(materialRecommendationModes({ activeTool: "image", textStyle: "cut" }), {
    laserMode: "engrave", engravingMode: "raster",
  });
});

test("图片尺寸与转换方式校验：留空自动推算，非法宽高直接报错拦截", () => {
  assert.deepEqual(imageParamsPayload({ widthMm: "", heightMm: "  ", imageMode: "auto" }), { image_mode: "auto" });
  assert.deepEqual(imageParamsPayload({ widthMm: "45.5", heightMm: "", imageMode: "dither" }), {
    image_mode: "dither", width_mm: 45.5,
  });
  assert.match(imageParamsPayload({ widthMm: "0", heightMm: "" }).error, /成品宽度/);
  assert.match(imageParamsPayload({ widthMm: "50", heightMm: "1500" }).error, /成品高度/);
});

test("LightBurn 对齐的图片模式：选项与后端一致，高级项只在相关模式下发送", () => {
  assert.deepEqual(
    IMAGE_MODE_OPTIONS.map((opt) => opt.value),
    ["auto", "dither", "threshold", "jarvis", "stucki", "atkinson", "ordered", "halftone", "newsprint", "sketch", "grayscale"],
  );
  // 空白高级项不改变旧请求
  assert.deepEqual(imageParamsPayload({ imageMode: "jarvis", extras: DEFAULT_IMAGE_EXTRAS }), { image_mode: "jarvis" });
  const extras = {
    ...DEFAULT_IMAGE_EXTRAS,
    negative: true,
    passThrough: true,
    dotWidthMm: "0.05",
    halftoneCellMm: "0.8",
    halftoneAngleDeg: "30",
    grayMinPowerPercent: "15",
  };
  assert.deepEqual(imageParamsPayload({ imageMode: "halftone", extras }), {
    image_mode: "halftone", negative: true, pass_through: true, dot_width_mm: 0.05,
    halftone_cell_mm: 0.8, halftone_angle_deg: 30,
  });
  assert.deepEqual(imageParamsPayload({ imageMode: "grayscale", extras }), {
    image_mode: "grayscale", negative: true, pass_through: true, dot_width_mm: 0.05,
    gray_min_power_percent: 15,
  });
  assert.deepEqual(imageModeFields("halftone"), { halftone: true, grayscale: false });
  assert.deepEqual(imageModeFields("dither"), { halftone: false, grayscale: false });
  assert.match(imageParamsPayload({ imageMode: "halftone", extras: { halftoneCellMm: "20" } }).error, /网点网格/);
  assert.match(imageParamsPayload({ imageMode: "halftone", extras: { halftoneAngleDeg: "120" } }).error, /网点角度/);
  assert.match(imageParamsPayload({ imageMode: "grayscale", extras: { grayMinPowerPercent: "100" } }).error, /灰度最小功率/);
  assert.match(imageParamsPayload({ imageMode: "auto", extras: { dotWidthMm: "-0.1" } }).error, /点宽补偿/);
  // 隐藏模式的无效输入不拦截当前模式
  assert.deepEqual(imageParamsPayload({ imageMode: "dither", extras: { halftoneCellMm: "abc" } }), { image_mode: "dither" });
  assert.equal(imageParamsPayload({ imageMode: "bogus" }).image_mode, "auto");
});

test("图片高级项改动使预览过期，隐藏模式的输入不影响签名", () => {
  const base = {
    activeTool: "image",
    uploadedImagePath: "a.png",
    imageMode: "halftone",
    imageExtras: { ...DEFAULT_IMAGE_EXTRAS },
    selectedMaterial: "椴木",
    selectedThickness: "3",
  };
  const sig = workbenchInputSignature(base);
  const changed = (patch) =>
    isWorkbenchPreviewStale(sig, workbenchInputSignature({ ...base, imageExtras: { ...base.imageExtras, ...patch } }));
  assert.equal(changed({}), false);
  assert.equal(changed({ negative: true }), true, "反相");
  assert.equal(changed({ passThrough: true }), true, "直通");
  assert.equal(changed({ dotWidthMm: "0.05" }), true, "点宽补偿");
  assert.equal(changed({ halftoneCellMm: "0.8" }), true, "网点网格");
  assert.equal(changed({ halftoneAngleDeg: "30" }), true, "网点角度");
  assert.equal(changed({ grayMinPowerPercent: "10" }), false, "网点模式下灰度最小功率不生效");
  assert.equal(
    isWorkbenchPreviewStale(sig, workbenchInputSignature({ ...base, imageMode: "grayscale" })),
    true,
    "换模式",
  );
  const graySig = workbenchInputSignature({ ...base, imageMode: "grayscale" });
  assert.equal(
    isWorkbenchPreviewStale(
      graySig,
      workbenchInputSignature({ ...base, imageMode: "grayscale", imageExtras: { ...base.imageExtras, grayMinPowerPercent: "10" } }),
    ),
    true,
    "灰度最小功率",
  );
});

test("左侧输入或参数修改后，预览签名立即判定为已过期以防误刻旧刀路", () => {
  const base = {
    activeTool: "text",
    textContent: "MOSS",
    fontSizeMm: 12,
    textStyle: "outline",
    textOrigin: "center",
    selectedMaterial: "椴木",
    selectedThickness: "3",
    customParams: false,
  };
  const sig0 = workbenchInputSignature(base);
  assert.equal(isWorkbenchPreviewStale("", sig0), false, "尚未生成预览时不算过期");
  assert.equal(isWorkbenchPreviewStale(sig0, workbenchInputSignature({ ...base })), false);
  assert.equal(isWorkbenchPreviewStale(sig0, workbenchInputSignature({ ...base, textContent: "HELLO" })), true, "改文字变过期");
  assert.equal(isWorkbenchPreviewStale(sig0, workbenchInputSignature({ ...base, textStyle: "fill" })), true, "改刻法变过期");
  assert.equal(isWorkbenchPreviewStale(sig0, workbenchInputSignature({ ...base, selectedMaterial: "亚克力" })), true, "换材料变过期");
  assert.equal(isWorkbenchPreviewStale(sig0, workbenchInputSignature({ ...base, activeTool: "image" })), true, "切工具变过期");
  assert.equal(
    isWorkbenchPreviewStale(sig0, workbenchInputSignature({ ...base, customParams: true, customPower: "60", customFeed: "1200", customPasses: "1" })),
    true,
    "开自定义参数变过期",
  );
  const sigPreset = workbenchInputSignature({ ...base, presetParams: "42/1800/1" });
  assert.equal(
    isWorkbenchPreviewStale(sigPreset, workbenchInputSignature({ ...base, presetParams: "42/1800/3" })),
    true,
    "修改材料库预设功率/速度/遍数后旧预览立即判定为过期",
  );
});

test("开刻二次确认与相同刀路 SHA-256 重发防烧穿提醒、状态大白话文案", () => {
  const wf = {
    confirmation_snapshot: { gcode_sha256: "a".repeat(64) },
    artifacts: { actual_params: { laser_max_power: 500, feed_rate: 1200, passes: 1, power_scale: 1000 } },
    input: {},
  };
  const first = workbenchConfirmSendPrompt({
    workflow: wf,
    material: "椴木",
    thickness: "3",
    targetLabel: "网络 127.0.0.1:23",
    dimensions: { width: 40, height: 12, originNote: "开刻前把激光头移到要刻的位置中心" },
    lastSentSha256: "",
  });
  assert.equal(first.danger, false);
  assert.equal(first.confirmText, "确认开刻");
  assert.match(first.message, /40\.0 × 12\.0 mm/);
  assert.match(first.message, /移到要刻的位置中心/);

  const repeat = workbenchConfirmSendPrompt({
    workflow: wf,
    material: "椴木",
    thickness: "3",
    targetLabel: "网络 127.0.0.1:23",
    dimensions: { width: 40, height: 12 },
    lastSentSha256: "a".repeat(64),
  });
  assert.equal(repeat.danger, true);
  assert.match(repeat.title, /相同刀路/);
  assert.match(repeat.message, /挪动材料/);

  assert.equal(workbenchStatusLabel("pending", undefined, false), "等待计算轨迹");
  assert.equal(workbenchStatusLabel("pending", undefined, true), "就绪 · 等待开刻");
  assert.equal(workbenchStatusLabel("running", "streaming", true), "正在雕刻下发");
  assert.equal(workbenchStatusLabel("running", "awaiting_completion", true), "等待机器执行完成");
  assert.equal(workbenchStatusLabel("completed", "completed", true), "雕刻已完成");
  assert.equal(workbenchStatusLabel("cancelled", undefined, true), "已暂停取消（可续雕）");
});

test("断点续雕二次确认文案、异步 showPrompt 输入弹窗与前端安全门禁守卫", async () => {
  const resume = workbenchResumeSendPrompt({
    workflow: { artifacts: { actual_params: { laser_max_power: 450, feed_rate: 1000, passes: 1, power_scale: 1000 } }, input: {} },
    material: "椴木",
    thickness: "3",
    targetLabel: "串口 COM5 · 115200",
    startLine: 129,
    totalLines: 400,
  });
  assert.equal(resume.title, "确认断点续雕");
  assert.equal(resume.confirmText, "确认从第 129 行续雕");
  assert.match(resume.message, /129 \/ 400 行/);
  assert.match(resume.message, /丢步偏移/);

  const unsub = subscribe(() => {});
  try {
    const p1 = showPrompt("输入串口号", { title: "手动输入串口号", defaultValue: "COM3" });
    const active = getCurrentDialog();
    assert.equal(active.kind, "prompt");
    assert.equal(active.defaultValue, "COM3");
    resolveDialog(active.id, "COM9");
    assert.equal(await p1, "COM9");

    const p2 = showPrompt("输入串口号", { defaultValue: "COM3" });
    resolveDialog(getCurrentDialog().id, null);
    assert.equal(await p2, null);
  } finally {
    resetDialogs();
    unsub();
  }

  const homeSrc = readPageSource("pages/HomePage.jsx");
  const drawSrc = readPageSource("pages/DrawPage.jsx");
  const connModalSrc = readFileSync(new URL("../src/components/DeviceConnectionModal.jsx", import.meta.url), "utf8");
  const labConnSrc = readFileSync(new URL("../src/components/material-lab/LabConnectionBar.jsx", import.meta.url), "utf8");
  const appDialogSrc = readFileSync(new URL("../src/components/AppDialog.jsx", import.meta.url), "utf8");
  // Colab 联动栏实现在 voice-builtin/ 下（2026-11-13 从 VoiceBuiltinCard.jsx 原样搬出）
  const voiceBuiltinSrc = readFileSync(new URL("../src/components/voice-builtin/ColabBridgeSection.jsx", import.meta.url), "utf8");
  assert.doesNotMatch(homeSrc, /window\.prompt\b/, "工作台不得使用阻塞式 window.prompt");
  assert.doesNotMatch(drawSrc, /window\.prompt\b/, "创作画板不得使用阻塞式 window.prompt");
  assert.doesNotMatch(connModalSrc, /window\.prompt\b/, "统一设备连接面板不得使用阻塞式 window.prompt");
  assert.match(connModalSrc, /selectEl\.value = serialPort \|\| ""/, "统一连接面板取消手动输入串口后原生 select 必须立即回退避免卡在 __custom__");
  assert.match(connModalSrc, /SERIAL_BAUDRATES\.map\(/, "统一连接面板波特率下拉须复用共享常量 SERIAL_BAUDRATES");
  assert.match(labConnSrc, /connectionSummaryView\(/, "材料实验室连接条须收口为共享紧凑连接摘要");
  assert.doesNotMatch(labConnSrc, /<select\b/, "材料实验室连接条不得再维护分叉串口或残缺波特率下拉");
  assert.match(appDialogSrc, /e\.isComposing/, "AppDialog 输入框必须忽略中文输入法组合键回车");
  assert.match(appDialogSrc, /action === "cancel"/, "AppDialog 聚焦取消按钮按回车不得误判为确认");
  assert.match(homeSrc, /workbenchResumeSendPrompt\(/, "断点续雕必须先经二次确认");
  assert.match(voiceBuiltinSrc.split('id="voice-colab-cli-install"')[0].slice(-700), /showConfirm\(/, "安装外部 Colab CLI 必须先经二次确认");

  const staleAdopt = checkDerivedWorkbenchAdoption({
    workflow: { workflow_id: "wf-2", regenerated_from: "wf-1" },
    currentWorkflowId: "wf-1",
    previewSignature: "sig-old",
    previewStale: true,
  });
  assert.equal(staleAdopt.ok, false);

  const mismatchAdopt = checkDerivedWorkbenchAdoption({
    workflow: { workflow_id: "wf-2", regenerated_from: "wf-old" },
    currentWorkflowId: "wf-cur",
    previewSignature: "sig-old",
    previewStale: false,
  });
  assert.equal(mismatchAdopt.ok, false);

  const okAdopt = checkDerivedWorkbenchAdoption({
    workflow: { workflow_id: "wf-2", regenerated_from: "wf-1" },
    currentWorkflowId: "wf-1",
    previewSignature: "sig-old",
    previewStale: false,
  });
  assert.equal(okAdopt.ok, true);
  assert.equal(okAdopt.inheritedSignature, "sig-old");

  assert.match(homeSrc, /framingBusy \|\| previewStale/, "工作台预览过期时必须禁用反馈调参");
  assert.match(homeSrc, /isApprovalRequiredResponse\(res\)/, "工作台走边框与发送必须拦截严格模式 202 待审批");
  const aiPreviewSrc = readPageSource("components/AiPreviewCenter.jsx");
  assert.match(aiPreviewSrc, /isApprovalRequiredResponse\(res\)/, "AI 预览启动/走边框必须拦截严格模式 202 待审批");
});

test("工作台重复两点对位仍匹配原任务，反馈调参仍要匹配本次预览", () => {
  const currentWorkflow = { workflow_id: "reg-1", registration: {}, registered_from: "source" };
  const check = (workflow) => checkDerivedWorkbenchAdoption({
    workflow, currentWorkflow, previewSignature: "sig-original", previewStale: false,
  });
  assert.equal(check({ workflow_id: "reg-2", registered_from: "source" }).ok, true);
  assert.equal(check({ workflow_id: "reg-3", registered_from: "other" }).ok, false);
  assert.equal(check({ workflow_id: "tune-1", regenerated_from: "source" }).ok, false);
  assert.equal(check({ workflow_id: "tune-2", regenerated_from: "reg-1" }).ok, true);
});

test("采用调参按 actual_params 回填功率、速度、遍数，签名与下一次生成一致", () => {
  const inputs = { textContent: "测试", presetParams: "44/1500/1" };
  const adopted = workbenchFeedbackInputs({
    regenerated_from: "source",
    artifacts: { actual_params: { laser_max_power: 150, power_scale: 400, feed_rate: 910, passes: 2 } },
  }, inputs);
  const { customParams, customPower, customFeed, customPasses, signature } = adopted;
  assert.deepEqual(manualParamsPayload({ enabled: customParams, powerPercent: customPower,
    feedRate: customFeed, passes: customPasses }), { power_percent: 37.5, feed_rate: 910, passes: 2 });
  assert.equal(isWorkbenchPreviewStale(signature, workbenchInputSignature({ ...inputs, ...adopted })), false);
  assert.equal(isWorkbenchPreviewStale(signature, workbenchInputSignature({ ...inputs, ...adopted, customFeed: "911" })), true);
  assert.equal(workbenchFeedbackInputs({ registered_from: "source" }, inputs), null);
});

test("旧调参预览的低功率如实回填，后续生成走手动范围校验", () => {
  const workflow = { regenerated_from: "source",
    artifacts: { actual_params: { laser_max_power: 9, feed_rate: 910 } } };
  const adopted = workbenchFeedbackInputs(workflow, {});
  assert.match(actualParamsText(workflow), /功率 0.9%.*反馈调参/);
  assert.equal(adopted.customPower, "0.9");
  assert.equal(adopted.customFeed, "910");
  assert.match(manualParamsPayload({ enabled: true, powerPercent: adopted.customPower,
    feedRate: adopted.customFeed, passes: adopted.customPasses }).error, /功率/);
});

test("工作台连接字段、规范目标与结果未知时的进度保留", async () => {
  const { sendProgressForUnknownResult, workbenchCanonicalTarget, workbenchConnectionFields } = await import(
    "../src/workbenchParams.js"
  );
  const serial = { connectionMode: "serial", serialPort: "COM3", baudrate: "250000", networkHost: "", telnetPort: 23 };
  const network = { connectionMode: "network", serialPort: "", baudrate: 115200, networkHost: "<device-ip>", telnetPort: "" };
  assert.deepEqual(workbenchConnectionFields(serial), {
    connection_mode: "serial",
    port: "COM3",
    baudrate: 250000,
    network_host: "",
    network_telnet_port: 23,
  });
  assert.equal(workbenchCanonicalTarget(serial), "COM3@250000");
  assert.equal(workbenchCanonicalTarget(network), "<device-ip>:23");

  // 已绑定目标的进度原样保留（急停仍按原通道），没有目标时补上规范目标
  const bound = { sent: 12, total: 40, target: "COM9@115200" };
  assert.equal(sendProgressForUnknownResult(bound, 99, "COM3@250000"), bound);
  assert.deepEqual(sendProgressForUnknownResult({ sent: 5, total: 40, target: "" }, 99, "COM3@250000"), {
    sent: 5,
    total: 40,
    target: "COM3@250000",
  });
  assert.deepEqual(sendProgressForUnknownResult(null, 99, "x"), { sent: 0, total: 99, target: "x" });
});

test("工作台发送确认框与监视舱：超速提示、软限位风险预判、无提示干净态与发送后 warnings 展示", async () => {
  const path = (await import("node:path")).default;
  const { fileURLToPath } = await import("node:url");
  const React = (await import("react")).default;
  const { renderToStaticMarkup } = await import("react-dom/server");
  const { createServer } = await import("vite");
  const { resolveSoftLimitSendWarning, syncHardwareWorkspaceLimits } = await import("../src/deviceConnection.js");
  const { sendProgressFromResult } = await import("../src/estopResumeState.js");
  const { workbenchPreviewWarnings, workbenchSendWarnings } = await import("../src/workbenchParams.js");

  const speedWarn = "雕刻速度 4500 mm/min 超过固件最大速度 3000 mm/min，固件会自动降速，工时与出光效果会偏离设定";
  const softOffWarn = "固件未报告行程且软限位未开启，越界不会被拦截";
  const mposWarn = "按当前机械坐标估算，X 轴预计走到 -20.0 ~ 30.0 mm（机械坐标范围 -200 ~ 0 mm），可能撞到行程尽头，请核对起点位置";

  const baseWorkflow = {
    workflow_id: "wf-k1",
    confirmation_snapshot: { gcode_sha256: "b".repeat(64) },
    artifacts: { actual_params: { laser_max_power: 400, feed_rate: 1200, passes: 1, power_scale: 1000 } },
    input: {},
  };

  // 1) 有超速提示时，确认框与断点续雕确认框都包含这一行
  const wfWithSpeedWarn = {
    ...baseWorkflow,
    artifacts: { ...baseWorkflow.artifacts, warnings: [speedWarn] },
  };
  assert.deepEqual(workbenchPreviewWarnings(wfWithSpeedWarn), [speedWarn]);
  const promptSpeed = workbenchConfirmSendPrompt({
    workflow: wfWithSpeedWarn,
    material: "椴木",
    thickness: "3",
    targetLabel: "网络 192.0.2.10:23",
    dimensions: { width: 30, height: 15 },
  });
  assert.match(promptSpeed.message, /超过固件最大速度 3000 mm\/min/);
  assert.equal(promptSpeed.confirmText, "确认开刻", "只提示不拦截，confirmed 流程不变");

  // 2) 软限位未开且行程未知时，前端按握手信息预判并在确认框带出风险行
  const memStore = (() => {
    const data = new Map();
    return {
      getItem: (k) => (data.has(k) ? data.get(k) : null),
      setItem: (k, v) => data.set(k, String(v)),
      removeItem: (k) => data.delete(k),
    };
  })();
  const targetForm = { connection_mode: "network", network_host: "192.0.2.10", network_telnet_port: "23" };
  syncHardwareWorkspaceLimits(null, targetForm, memStore, {
    probed: true,
    softLimitsEnabled: false,
    homingEnabled: false,
  });
  const predictedSafety = resolveSoftLimitSendWarning(targetForm, memStore);
  assert.equal(predictedSafety, softOffWarn);
  const promptSoftOff = workbenchConfirmSendPrompt({
    workflow: baseWorkflow,
    material: "椴木",
    thickness: "3",
    targetLabel: "网络 192.0.2.10:23",
    safetyWarning: predictedSafety,
  });
  assert.match(promptSoftOff.message, /固件未报告行程且软限位未开启，越界不会被拦截/);

  const resumeWithRisk = workbenchResumeSendPrompt({
    workflow: wfWithSpeedWarn,
    material: "椴木",
    thickness: "3",
    targetLabel: "网络 192.0.2.10:23",
    startLine: 18,
    totalLines: 100,
    safetyWarning: predictedSafety,
  });
  assert.match(resumeWithRisk.message, /超过固件最大速度 3000 mm\/min/);
  assert.match(resumeWithRisk.message, /固件未报告行程且软限位未开启，越界不会被拦截/);

  // 3) 都没有时，确认框不显示任何风险提示行
  syncHardwareWorkspaceLimits({ x_travel_mm: 300, y_travel_mm: 200 }, targetForm, memStore, {
    probed: true,
    softLimitsEnabled: true,
    homingEnabled: true,
  });
  assert.equal(resolveSoftLimitSendWarning(targetForm, memStore), "");
  const cleanPrompt = workbenchConfirmSendPrompt({
    workflow: baseWorkflow,
    material: "椴木",
    thickness: "3",
    targetLabel: "网络 192.0.2.10:23",
    safetyWarning: resolveSoftLimitSendWarning(targetForm, memStore),
  });
  assert.doesNotMatch(cleanPrompt.message, /风险提示/);
  assert.deepEqual(cleanPrompt.warnings, []);

  // 4) 发送后的 warnings 经 sendProgressFromResult 提取并在 WorkbenchMonitorCard 监视区渲染（无提示时不渲染警告块）
  const progressWithWarnings = sendProgressFromResult({
    running: true,
    lines_sent: 12,
    total_lines: 60,
    target: "192.0.2.10:23",
    phase: "streaming",
    warnings: [softOffWarn, mposWarn],
  });
  assert.deepEqual(workbenchSendWarnings(progressWithWarnings, baseWorkflow), [softOffWarn, mposWarn]);

  const frontendRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const ssrServer = await createServer({
    root: frontendRoot,
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    const { WorkbenchMonitorCard } = await ssrServer.ssrLoadModule(
      "/src/components/workbench/WorkbenchMonitorCard.jsx",
    );
    const commonProps = {
      onNavigate: () => {},
      previewUrl: "",
      previewStale: false,
      dimensions: { width: 30, height: 15, originNote: "" },
      gcodeText: "G1 X10",
      timeEstimate: null,
      fitNote: "",
      routeNote: "",
      fillNote: "",
      busy: false,
      framingBusy: false,
      overlayStatus: "pending",
      framingShape: "box",
      setFramingShape: () => {},
      connectionForm: targetForm,
      connectionStatus: null,
      activeJobs: [],
      motionPayload: {},
      addLog: () => {},
      adoptDerivedWorkflow: () => {},
      handleGeneratePreview: () => {},
      handleConfirmSend: () => {},
      handleResumeSend: () => {},
      handleFraming: () => {},
      handleExportGcode: () => {},
      handleCancelJob: () => {},
      handleEStop: () => {},
    };

    const cleanHtml = renderToStaticMarkup(
      React.createElement(WorkbenchMonitorCard, {
        ...commonProps,
        currentWorkflow: baseWorkflow,
        sendProgress: null,
      }),
    );
    assert.doesNotMatch(cleanHtml, /wb-preview-warnings/);
    assert.doesNotMatch(cleanHtml, /wb-send-warnings/);

    const warnHtml = renderToStaticMarkup(
      React.createElement(WorkbenchMonitorCard, {
        ...commonProps,
        currentWorkflow: wfWithSpeedWarn,
        overlayStatus: "running",
        sendProgress: progressWithWarnings,
      }),
    );
    assert.match(warnHtml, /wb-preview-warnings/);
    assert.match(warnHtml, /超过固件最大速度 3000 mm\/min/);
    assert.match(warnHtml, /wb-send-warnings/);
    assert.match(warnHtml, /固件未报告行程且软限位未开启，越界不会被拦截/);
    assert.match(warnHtml, /可能撞到行程尽头/);
  } finally {
    await ssrServer.close();
  }
});
