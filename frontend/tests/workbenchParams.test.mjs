import test from "node:test";
import assert from "node:assert/strict";
import {
  actualParamsText,
  imageParamsPayload,
  isWorkbenchPreviewStale,
  manualParamsPayload,
  materialRecommendationModes,
  resolveTextStyle,
  workbenchConfirmSendPrompt,
  workbenchInputSignature,
  workbenchResumeSendPrompt,
  workbenchStatusLabel,
} from "../src/workbenchParams.js";
import { getCurrentDialog, resetDialogs, resolveDialog, showPrompt, subscribe } from "../src/appDialog.js";
import { readFileSync } from "node:fs";

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
    style: "fill", label: "实心扫描填充", laserMode: "engrave", engravingMode: "raster", textFill: "fill",
  });
  // 兼容旧版 localStorage 里残留的 raster，自动归一为真实的 fill 实心填充
  assert.equal(resolveTextStyle("raster").textFill, "fill");
  assert.deepEqual(resolveTextStyle("hatch"), {
    style: "hatch", label: "矢量排线填充", laserMode: "engrave", engravingMode: "outline", textFill: "hatch",
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

  const homeSrc = readFileSync(new URL("../src/pages/HomePage.jsx", import.meta.url), "utf8");
  const drawSrc = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  const appDialogSrc = readFileSync(new URL("../src/components/AppDialog.jsx", import.meta.url), "utf8");
  const voiceBuiltinSrc = readFileSync(new URL("../src/components/VoiceBuiltinCard.jsx", import.meta.url), "utf8");
  assert.doesNotMatch(homeSrc, /window\.prompt\b/, "工作台不得使用阻塞式 window.prompt");
  assert.doesNotMatch(drawSrc, /window\.prompt\b/, "创作画板不得使用阻塞式 window.prompt");
  assert.match(homeSrc, /selectEl\.value = serialPort \|\| ""/, "取消手动输入串口后原生 select 必须立即回退避免卡在 __custom__");
  assert.match(drawSrc, /selectEl\.value = form\.port \|\| ""/, "画板取消手动输入串口后原生 select 必须立即回退避免卡在 __custom__");
  assert.match(appDialogSrc, /e\.isComposing/, "AppDialog 输入框必须忽略中文输入法组合键回车");
  assert.match(appDialogSrc, /action === "cancel"/, "AppDialog 聚焦取消按钮按回车不得误判为确认");
  assert.match(homeSrc, /workbenchResumeSendPrompt\(/, "断点续雕必须先经二次确认");
  assert.match(voiceBuiltinSrc.split('id="voice-colab-cli-install"')[0].slice(-700), /showConfirm\(/, "安装外部 Colab CLI 必须先经二次确认");
});
