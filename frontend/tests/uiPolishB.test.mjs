import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

const testDir = path.dirname(fileURLToPath(import.meta.url));
const frontendRoot = path.resolve(testDir, "..");
const srcRoot = path.resolve(frontendRoot, "src");

async function withSsrServer(fn) {
  const server = await createServer({
    root: frontendRoot,
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    await fn(server);
  } finally {
    await server.close();
  }
}

test("B1/B2/B4/B5/B6/B7/B8：界面组件 SSR 渲染与状态交互契约全覆盖", async () => {
  await withSsrServer(async (server) => {
    const { LabParamsPanel } = await server.ssrLoadModule("/src/components/material-lab/LabParamsPanel.jsx");
    const { LabMatrixSettings } = await server.ssrLoadModule("/src/components/material-lab/LabMatrixSettings.jsx");
    const { resolveMatrixModeParams } = await server.ssrLoadModule("/src/materialLabState.js");
    const { SideNav } = await server.ssrLoadModule("/src/components/SideNav.jsx");
    const { FoldHeader } = await server.ssrLoadModule("/src/components/FoldHeader.jsx");
    const { ConsoleDrawer } = await server.ssrLoadModule("/src/components/ConsoleDrawer.jsx");
    const { WorkbenchMonitorCard } = await server.ssrLoadModule("/src/components/workbench/WorkbenchMonitorCard.jsx");
    const { WorkbenchHeadJogPanel } = await server.ssrLoadModule("/src/components/workbench/WorkbenchHeadJogPanel.jsx");
    const { TwinPage } = await server.ssrLoadModule("/src/pages/TwinPage.jsx");
    const { MossVoiceTwin } = await server.ssrLoadModule("/src/components/MossVoiceTwin.jsx");
    const { FEEDBACK_SYMPTOM_CHIPS, FeedbackPanel, applyFeedbackChip } = await server.ssrLoadModule("/src/components/FeedbackPanel.jsx");
    const { OriginBadge, inferOriginKind } = await server.ssrLoadModule("/src/components/OriginBadge.jsx");
    const { WorkbenchTextTool } = await server.ssrLoadModule("/src/components/workbench/WorkbenchTextTool.jsx");
    const { WorkbenchImageTool } = await server.ssrLoadModule("/src/components/workbench/WorkbenchImageTool.jsx");
    const { SendConfirmDialog } = await server.ssrLoadModule("/src/components/draw/SendConfirmDialog.jsx");
    const { AppDialogHost } = await server.ssrLoadModule("/src/components/AppDialog.jsx");
    const { DEFAULT_IMAGE_EXTRAS, workbenchConfirmSendPrompt } = await server.ssrLoadModule("/src/workbenchParams.js");
    const { getCurrentDialog, resetDialogs, showConfirm, subscribe } = await server.ssrLoadModule("/src/appDialog.js");

    // ---------- B1 & B2 ----------
    const paramsHtml = renderToStaticMarkup(
      React.createElement(LabParamsPanel, {
        materials: { 椴木: { thicknesses: { "3": {} } } },
        readOnly: false,
        selectedMaterial: "椴木",
        selectedThickness: "3",
        currentMatThicknesses: ["3"],
        hasSavedCell: true,
        editorStatus: "",
        activeMode: "raster",
        handleModeChange: () => {},
        handleDeleteCurrentThickness: () => {},
        handleSaveParams: () => {},
        setMaterialModalData: () => {},
        setShowMaterialModal: () => {},
        minPower: 0,
        setMinPower: () => {},
        maxPower: 425,
        setMaxPower: () => {},
        feedRate: 1800,
        setFeedRate: () => {},
        travelRate: 3000,
        setTravelRate: () => {},
        passes: 1,
        setPasses: () => {},
        pixelSize: 0.1,
        setPixelSize: () => {},
        threshold: -1,
        setThreshold: () => {},
        aliases: "木牌, basswood",
        setAliases: () => {},
        entryTitle: "",
        setEntryTitle: () => {},
        notes: "",
        setNotes: () => {},
      }),
    );
    assert.match(paramsHtml, /最小激光功率 \(0–100%\)/);
    assert.match(paramsHtml, /最大激光功率 \(0–100%\)/);
    assert.doesNotMatch(paramsHtml, /0-1000/);
    assert.match(paramsHtml, /id="lab-max-power"[^>]*value="42\.5"/);
    assert.match(paramsHtml, /对应 S0 \/ 1000/);
    assert.match(paramsHtml, /对应 S425 \/ 1000/);
    assert.match(paramsHtml, /<div class="lab-box-3"><label class="lab-label-2">材料别名 \(逗号分隔\)<\/label>/);
    assert.doesNotMatch(paramsHtml, /class="ob-k"/);

    const matrixHtml = renderToStaticMarkup(
      React.createElement(LabMatrixSettings, {
        selectedMaterial: "椴木",
        selectedThickness: "3",
        matrixMode: "raster",
        matrixModeInfo: resolveMatrixModeParams("raster"),
        changeMatrixMode: () => {},
        matrixStatus: "",
        matrixPowerMin: 100,
        setMatrixPowerMin: () => {},
        matrixPowerMax: 600,
        setMatrixPowerMax: () => {},
        matrixSpeedMin: 500,
        setMatrixSpeedMin: () => {},
        matrixSpeedMax: 2500,
        setMatrixSpeedMax: () => {},
        matrixRows: 5,
        setMatrixRows: () => {},
        matrixCols: 5,
        setMatrixCols: () => {},
        matrixTestType: "matrix",
        setMatrixTestType: () => {},
        matrixIntervalMin: 0.08,
        setMatrixIntervalMin: () => {},
        matrixIntervalMax: 0.16,
        setMatrixIntervalMax: () => {},
        matrixIntervalCount: 9,
        setMatrixIntervalCount: () => {},
        matrixXAxis: "power",
        setMatrixXAxis: () => {},
        matrixYAxis: "speed",
        setMatrixYAxis: () => {},
        matrixPassesMin: 1,
        setMatrixPassesMin: () => {},
        matrixPassesMax: 3,
        setMatrixPassesMax: () => {},
        matrixLabels: true,
        setMatrixLabels: () => {},
      }),
    );
    assert.match(matrixHtml, /功率区间 \(0–100%\)/);
    assert.match(matrixHtml, /对应 S100~S600/);
    assert.match(matrixHtml, /aria-label="最小功率百分比"[^>]*value="10"/);
    assert.match(matrixHtml, /aria-label="最大功率百分比"[^>]*value="60"/);
    assert.doesNotMatch(matrixHtml, /<label[^>]*class="lab-label-3"[^>]*>[^<]*<select/);
    assert.match(matrixHtml, /<div class="lab-axis-field"><label class="lab-label-3" for="lab-test-type">测试类型<\/label><select id="lab-test-type"/);
    assert.match(matrixHtml, /<div class="lab-axis-field"><label class="lab-label-3" for="lab-x-axis">列参数<\/label><select id="lab-x-axis"/);
    assert.match(matrixHtml, /<div class="lab-axis-field"><label class="lab-label-3" for="lab-y-axis">行参数<\/label><select id="lab-y-axis"/);

    // ---------- B4 ----------
    const navHtml = renderToStaticMarkup(
      React.createElement(SideNav, { currentPath: "/", onNavigate: () => {}, showVoice: true }),
    );
    assert.doesNotMatch(navHtml, /ob-nav-tag/);
    assert.doesNotMatch(navHtml, />(HOST|CAD|LAB|TWIN|CAM|VOICE)</);

    const foldHtml = renderToStaticMarkup(
      React.createElement(FoldHeader, { id: "test-card", title: "测试卡片", n: 1, k: "IGNORED" }),
    );
    assert.doesNotMatch(foldHtml, /ob-k/);
    assert.doesNotMatch(foldHtml, /IGNORED/);

    const drawerHtml = renderToStaticMarkup(React.createElement(ConsoleDrawer, { logs: [] }));
    assert.match(drawerHtml, /开始雕刻或探测设备后，这里显示与雕刻机的实时通信记录/);
    assert.doesNotMatch(drawerHtml, /JobWorker|CONSOLE FEED|RPC AUDIT LOG/);

    const monitorHtml = renderToStaticMarkup(
      React.createElement(WorkbenchMonitorCard, {
        onNavigate: () => {},
        previewUrl: "",
        previewStale: false,
        currentWorkflow: null,
        dimensions: null,
        sendProgress: null,
        gcodeText: "",
        timeEstimate: null,
        fitNote: "",
        routeNote: "",
        fillNote: "",
        busy: false,
        framingBusy: false,
        overlayStatus: "idle",
        framingShape: "box",
        setFramingShape: () => {},
        connectionForm: {},
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
      }),
    );
    assert.match(monitorHtml, /id="btn-estop-trigger"[^>]*>.*紧急停止（软件复位）<\/span><\/button>/);
    assert.match(monitorHtml, /title="发送 0x18 软件复位；紧急情况请按机器实体急停"/);

    const drawSrc = readFileSync(path.join(srcRoot, "pages/DrawPage.jsx"), "utf8");
    assert.doesNotMatch(drawSrc, /onClick=\{\(\) => onNavigate\("\/"\)\}/, "画板顶部工具栏去掉与侧栏重复的工作台跳转按钮");

    const twinSrc = readFileSync(path.join(srcRoot, "pages/TwinPage.jsx"), "utf8");
    assert.doesNotMatch(twinSrc, /JobWorker 下发状态/);
    assert.match(twinSrc, /紧急停止（软件复位）/);
    assert.match(twinSrc, /发送 0x18 软件复位；紧急情况请按机器实体急停/);

    // ---------- B3 ----------
    // 1. 工作台加工监视舱包含默认收起的「现场移头与对刀原点」面板，仅含 1/5/10 mm 步长、四向点动、设为工作原点、解锁 $X，无连续点动与出光按钮
    assert.match(monitorHtml, /<details class="fill-options wb-jog-panel" id="wb-head-jog">/);
    assert.doesNotMatch(monitorHtml, /id="wb-head-jog"[^>]*open/);
    assert.match(monitorHtml, />现场移头与对刀原点</);
    assert.match(monitorHtml, /id="wb-jog-step"[^>]*disabled=""/);
    assert.deepEqual(
      Array.from(monitorHtml.matchAll(/<option value="(\d+)"[^>]*>\d+ mm<\/option>/g), (m) => Number(m[1])),
      [1, 5, 10],
    );
    for (const id of ["wb-jog-x-minus", "wb-jog-x-plus", "wb-jog-y-plus", "wb-jog-y-minus", "wb-jog-set-origin", "wb-jog-unlock"]) {
      assert.match(monitorHtml, new RegExp(`id="${id}"[^>]*disabled=""`), `未连接时 ${id} 必须禁用`);
    }
    assert.match(monitorHtml, /id="wb-jog-reason"[^>]*>尚未配置设备地址/);
    assert.doesNotMatch(monitorHtml, /wb-jog-pointer|pointer-fire|连续点动|出光按钮/);

    // 已连接且握手探测通过、无在途任务时，按钮恢复可用
    const readyJogHtml = renderToStaticMarkup(
      React.createElement(WorkbenchHeadJogPanel, {
        connectionForm: { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: "23" },
        connectionStatus: { connected: true, checkedAt: 1700000000000, summary: "Idle ($32=1)" },
        busy: false,
        framingBusy: false,
        overlayStatus: "pending",
        activeJobs: [],
      }),
    );
    for (const id of ["wb-jog-step", "wb-jog-x-minus", "wb-jog-x-plus", "wb-jog-y-plus", "wb-jog-y-minus", "wb-jog-set-origin", "wb-jog-unlock"]) {
      assert.doesNotMatch(readyJogHtml, new RegExp(`id="${id}"[^>]*disabled`), `握手通过且空闲时 ${id} 应可点`);
    }

    // 2. 数字孪生遥测卡顶部接入与工作台一致的连接摘要（方式 · 地址 · 状态）与「修改连接」按钮，main.jsx 传入 openConnectionModal
    const twinHtml = renderToStaticMarkup(
      React.createElement(TwinPage, {
        onNavigate: () => {},
        connectionStatus: { connected: true, checkedAt: 1700000000000, summary: "Idle ($32=1)" },
        setConnectionStatus: () => {},
        onOpenConnection: () => {},
      }),
    );
    assert.match(twinHtml, /id="twin-conn-summary"/);
    assert.match(twinHtml, /id="twin-open-connection"[\s\S]*?修改连接/);
    assert.match(twinHtml, /握手通过/);
    const mainSrc = readFileSync(path.join(srcRoot, "main.jsx"), "utf8");
    assert.match(mainSrc, /<TwinPage[\s\S]*?onOpenConnection=\{openConnectionModal\}/);

    // ---------- B5 ----------
    const idleHtml = renderToStaticMarkup(
      React.createElement(MossVoiceTwin, {
        mode: "official",
        status: { mcp_endpoint: "wss://example.test/mcp", bridge: { connected: true } },
        mcpStatus: { configured: true, connected: true },
      }),
    );
    assert.match(idleHtml, /data-expanded="false"/);
    assert.match(idleHtml, /moss-twin--compact/);
    assert.match(idleHtml, /小智云端连接正常 · 等待语音指令/);
    assert.match(idleHtml, /官方小智/);
    assert.doesNotMatch(idleHtml, /id="voice-moss-twin-metrics"/);
    assert.doesNotMatch(idleHtml, /OPTIC|CLOUD MCP TELEMETRY|REALTIME AUDIO TWIN/);

    const speakingHtml = renderToStaticMarkup(
      React.createElement(MossVoiceTwin, {
        mode: "builtin",
        builtinStatus: {
          running: true,
          mode: "chat",
          sessions: [{ session_id: "s1", state: "speaking", level: 65, spoken_sentences: 2 }],
          events: [],
        },
      }),
    );
    assert.match(speakingHtml, /data-expanded="true"/);
    assert.doesNotMatch(speakingHtml, /moss-twin--compact/);
    assert.match(speakingHtml, /id="voice-moss-twin-wave"/);
    assert.match(speakingHtml, /id="voice-moss-twin-metrics"/);
    assert.match(speakingHtml, /内置语音/);

    // ---------- B6 ----------
    const secHtml = renderToStaticMarkup(
      React.createElement(FoldHeader, {
        variant: "section",
        id: "settings-port",
        title: "服务监听端口 (Web Port)",
        summary: "端口 9666",
        defaultOpen: false,
      }),
    );
    assert.match(secHtml, /<span class="ob-fold-summary">端口 9666<\/span>/);

    const settingsSrc = readFileSync(path.join(srcRoot, "components/SettingsModal.jsx"), "utf8");
    for (const id of ["settings-port", "settings-mcp", "settings-jev", "settings-update", "settings-close", "settings-top-pills"]) {
      assert.match(settingsSrc, new RegExp(`id="${id}"[^>]*summary=`), `${id} 必须向 FoldHeader 传入当前值摘要`);
    }
    assert.match(settingsSrc, /id="settings-jev-test"/);
    assert.match(settingsSrc, /保存并测试/);

    for (const rel of ["components/PhoneLinkSection.jsx", "components/ExportSettingsSection.jsx", "components/ProcessTraceSection.jsx"]) {
      const src = readFileSync(path.join(srcRoot, rel), "utf8");
      assert.match(src, /<FoldHeader[^>]*summary=/, `${rel} 的折叠标题须带当前值摘要`);
    }

    // ---------- B7 ----------
    assert.deepEqual(FEEDBACK_SYMPTOM_CHIPS, ["刻太浅", "发黑过焦", "没切透", "边缘断线", "毛边"]);
    assert.equal(applyFeedbackChip("", "刻太浅"), "刻太浅");
    assert.equal(applyFeedbackChip("刻太浅", "边缘断线"), "刻太浅，边缘断线");
    assert.equal(applyFeedbackChip("刻太浅，边缘断线", "刻太浅"), "刻太浅，边缘断线");

    const panelHtml = renderToStaticMarkup(
      React.createElement(FeedbackPanel, { workflowId: "wf-1", disabled: false, onAdopt: () => {} }),
    );
    for (const chip of FEEDBACK_SYMPTOM_CHIPS) {
      assert.match(panelHtml, new RegExp(`class="ob-chip feedback-chip"[^>]*>${chip}<\\/button>`));
    }
    const chipsPos = panelHtml.indexOf("feedback-chips");
    const textareaPos = panelHtml.indexOf("<textarea");
    assert.ok(chipsPos > 0 && textareaPos > chipsPos, "症状快捷胶囊须排在反馈输入框上方");

    const homeSrc = readFileSync(path.join(srcRoot, "pages/HomePage.jsx"), "utf8");
    assert.match(homeSrc, /id="wb-feedback"[\s\S]*?defaultOpen=\{Boolean\(currentWorkflow\?\.workflow_id \|\| currentWorkflow\?\.feedback_origin\)\}/);
    assert.match(homeSrc, /autoExpandKey=\{[\s\S]*?overlayStatus === "completed"/);

    // ---------- B8 ----------
    assert.equal(inferOriginKind("center"), "center");
    assert.equal(inferOriginKind("lower_left"), "lower_left");
    assert.equal(inferOriginKind("开刻前把激光头移到木牌中心"), "center");
    assert.equal(inferOriginKind("开刻前把激光头对准作品左下角"), "lower_left");
    assert.equal(inferOriginKind("按两点对位：把激光头停在第 2 个对位标记上"), "point2");

    const centerSvg = renderToStaticMarkup(React.createElement(OriginBadge, { origin: "center" }));
    const lowerLeftSvg = renderToStaticMarkup(React.createElement(OriginBadge, { origin: "lower_left" }));
    assert.match(centerSvg, /data-origin="center"/);
    assert.match(centerSvg, /cx="10" cy="10" r="2\.2"/);
    assert.match(lowerLeftSvg, /data-origin="lower_left"/);
    assert.match(lowerLeftSvg, /cx="4\.5" cy="15\.5" r="2\.2"/);

    const textToolHtml = renderToStaticMarkup(
      React.createElement(WorkbenchTextTool, {
        textContent: "MOSS",
        setTextContent: () => {},
        fontSizeMm: 12,
        setFontSizeMm: () => {},
        textStyle: "outline",
        setTextStyle: () => {},
        textOrigin: "center",
        setTextOrigin: () => {},
        fillSettings: {},
        updateFillSettings: () => {},
        textUsesFill: false,
        textIsCut: false,
        materialApplied: false,
        materialLayer: null,
        saveFillToMaterial: null,
      }),
    );
    assert.match(textToolHtml, /wb-origin-label-row[\s\S]*?data-origin="center"/);

    const imgToolHtml = renderToStaticMarkup(
      React.createElement(WorkbenchImageTool, {
        addLog: () => {},
        uploadedImagePath: "",
        setUploadedImagePath: () => {},
        uploadedImageName: "",
        setUploadedImageName: () => {},
        uploadedImageThumb: "",
        setUploadedImageThumb: () => {},
        imageMode: "auto",
        setImageMode: () => {},
        imageExtras: DEFAULT_IMAGE_EXTRAS,
        setImageExtra: () => {},
        imageFields: { halftone: false, grayscale: false },
        applyImageQualityPreset: () => {},
        imageWidthMm: "",
        setImageWidthMm: () => {},
        imageHeightMm: "",
        setImageHeightMm: () => {},
        fillSettings: null,
        updateFillSettings: null,
      }),
    );
    assert.match(imgToolHtml, /id="wb-image-origin-hint"[\s\S]*?data-origin="lower_left"/);
    assert.match(drawSrc, /id="draw-dock-origin"[\s\S]*?<OriginBadge/);

    const drawConfirmHtml = renderToStaticMarkup(
      React.createElement(SendConfirmDialog, {
        summary: { material: "椴木", thickness_mm: 3, mode: "vector", actual_width_mm: 40, actual_height_mm: 20, line_count: 10, estimated_seconds: 5 },
        originNote: "",
        target: "<device-ip>:23",
        sdAvailable: true,
        busy: false,
        sendAllowed: true,
        onCancel: () => {},
        onConfirm: () => {},
      }),
    );
    assert.match(drawConfirmHtml, /id="draw-confirm-origin"[\s\S]*?data-origin="lower_left"/);

    const promptCenter = workbenchConfirmSendPrompt({
      workflow: { artifacts: { origin: "center", origin_note: "开刻前把激光头移到木牌中心", gcode_sha256: "abc" } },
      material: "椴木",
      thickness: "3",
      targetLabel: "<device-ip>:23",
    });
    assert.equal(promptCenter.origin, "center");

    const promptLowerLeft = workbenchConfirmSendPrompt({
      workflow: { artifacts: { origin: "lower_left", origin_note: "开刻前把激光头移到图片左下角", gcode_sha256: "def" } },
      material: "椴木",
      thickness: "3",
      targetLabel: "<device-ip>:23",
    });
    assert.equal(promptLowerLeft.origin, "lower_left");

    const unsubscribe = subscribe(() => {});
    try {
      void showConfirm(promptCenter.message, { title: promptCenter.title, origin: promptCenter.origin });
      assert.equal(getCurrentDialog()?.origin, "center");
      assert.doesNotThrow(() => renderToStaticMarkup(React.createElement(AppDialogHost)));
      const appDialogSrc = readFileSync(path.join(srcRoot, "components/AppDialog.jsx"), "utf8");
      assert.match(appDialogSrc, /app-dialog-origin-line[\s\S]*?<OriginBadge origin=\{dialog\.origin\}/);
      assert.doesNotMatch(appDialogSrc, /app-dialog-tag/, "提示框标题旁不再叠加重复的英文状态徽章");
    } finally {
      resetDialogs();
      unsubscribe();
    }

    // ---------- 2026-11-24 界面细节与单位收口 ----------
    const { MaterialModal } = await server.ssrLoadModule("/src/components/MaterialModal.jsx");
    const { CloseConfirmModal } = await server.ssrLoadModule("/src/components/CloseConfirmModal.jsx");
    const { Navbar } = await server.ssrLoadModule("/src/components/Navbar.jsx");
    const { SendOverlay } = await server.ssrLoadModule("/src/components/SendOverlay.jsx");
    const { formatTwinWorkflowLabel } = await server.ssrLoadModule("/src/pages/TwinPage.jsx");

    // 1. MaterialModal 统一使用 mm/min，默认 1500 mm/min，附 mm/s 旁注
    const newMatHtml = renderToStaticMarkup(
      React.createElement(MaterialModal, { visible: true, initialData: null, onClose: () => {} }),
    );
    assert.match(newMatHtml, /推荐速度（mm\/min）/);
    assert.doesNotMatch(newMatHtml, /推荐速度（mm\/s）/);
    assert.match(newMatHtml, /id="mat-f-speed"[^>]*value="1500"/);
    assert.match(newMatHtml, /id="mat-f-speed-hint"[^>]*>约 25 mm\/s</);

    const editMatHtml = renderToStaticMarkup(
      React.createElement(MaterialModal, {
        visible: true,
        initialData: { name: "椴木", thicknesses: ["3"], power_percent: 42, feed_rate: 1000 },
        onClose: () => {},
      }),
    );
    assert.match(editMatHtml, /id="mat-f-speed"[^>]*value="1000"/, "非 60 整倍数的进给速度直接无损回显");
    assert.match(editMatHtml, /约 16\.7 mm\/s/);

    // 2. 数字孪生：3D 摆位默认收起并注明不影响实际 G-code；任务标签展示内容摘要且不裸露 workflow_id；无 $CD 与 LINE 残留
    assert.match(twinHtml, /<details class="fill-options twin-layout-fold" id="twin-scene-layout">/);
    assert.doesNotMatch(twinHtml, /id="twin-scene-layout"[^>]*open/);
    assert.match(twinHtml, /3D 摆位预览（仅影响仿真画面）/);
    assert.match(twinHtml, /不改动实际 G-code 刀路/);
    assert.doesNotMatch(twinHtml, /翼宿 V1\.0 演示刀路|\$CD|LINE \d+/);
    assert.doesNotMatch(twinSrc, /\$CD/);
    assert.equal(
      formatTwinWorkflowLabel(
        { workflow_id: "wf_secret_uuid_123", status: "preview_ready", input: { text: "平安喜乐", material: "椴木", thickness_mm: 3 } },
        { includeStatus: true },
      ),
      "文字“平安喜乐” · 椴木 3mm · 预览就绪",
    );
    assert.equal(
      formatTwinWorkflowLabel({ workflow_id: "wf_secret_uuid_456", source_type: "draw", artifacts: { material: "亚克力", thickness_mm: 2 } }),
      "画板图形 · 亚克力 2mm",
    );

    // 3. 关闭窗口弹窗：去“杀死进程”黑话，偏好设置位置指明左侧栏底部
    const closeModalHtml = renderToStaticMarkup(
      React.createElement(CloseConfirmModal, { visible: true, onClose: () => {}, onConfirm: () => {} }),
    );
    assert.doesNotMatch(closeModalHtml, /杀死进程|右上角设置/);
    assert.match(closeModalHtml, /停止激光任务与语音服务并直接退出程序/);
    assert.match(closeModalHtml, /左侧栏底部的「偏好设置」/);
    assert.doesNotMatch(navHtml, /窗口右上角/);

    // 4. 顶栏、实验室与发送覆盖层去掉重复英文小标，画板根节点不再嵌套 <main>
    const topHtml = renderToStaticMarkup(
      React.createElement(Navbar, { currentPath: "/twin", onNavigate: () => {}, onEmergencyStop: () => {} }),
    );
    assert.match(topHtml, /<h1 class="ob-title">数字孪生舱<\/h1>/);
    assert.doesNotMatch(topHtml, /DIGITAL TWIN 3D|LASER FORGE/);
    assert.doesNotMatch(paramsHtml, />(Raster|Outline|Cut)<\/small>|Passes/);
    assert.match(paramsHtml, /加工遍数/);
    const overlaySendingHtml = renderToStaticMarkup(
      React.createElement(SendOverlay, { visible: true, status: "sending", onCancel: () => {}, onClose: () => {} }),
    );
    assert.doesNotMatch(overlaySendingHtml, /STATUS SENDING/);
    assert.match(overlaySendingHtml, /正在与控制器通信，请勿关闭页面/);
    assert.doesNotMatch(drawSrc, /<main\s+className="app-shell/);

    // 5. 选择框、侧栏待审批徽标、底部抽屉可折叠与加工内容按钮对齐
    const { ModelPairingGuide } = await server.ssrLoadModule("/src/components/voice-builtin/ModelPairingGuide.jsx");
    const consoleSrc = readFileSync(path.join(srcRoot, "components/ConsoleDrawer.jsx"), "utf8");

    const guideHtml = renderToStaticMarkup(
      React.createElement(ModelPairingGuide, { settings: {}, models: [], busy: false, onApply: async () => null }),
    );
    assert.match(guideHtml, /class="voice-preset-list"/, "场景推荐模型列表使用独立类名避免与下拉菜单冲突");
    assert.doesNotMatch(guideHtml, /class="voice-combo-list"/);

    const sideBadgeHtml = renderToStaticMarkup(
      React.createElement(SideNav, {
        currentPath: "/",
        onNavigate: () => {},
        showVoice: true,
        approvalCount: 3,
        collapsed: true,
      }),
    );
    assert.match(sideBadgeHtml, /class="ob-nav-badge"[^>]*>3</, "侧栏语音中心展示待审批徽标");
    assert.match(sideBadgeHtml, /语音中心（3 项待审批）/, "收起侧栏时悬停标题附带待审批数量");

    const drawerTabsHtml = renderToStaticMarkup(
      React.createElement(ConsoleDrawer, { logs: [], onClear: () => {}, mcpStatus: { configured: true, connected: true } }),
    );
    assert.match(drawerTabsHtml, /role="tablist"[\s\S]*?<button[^>]*role="tab"[^>]*id="tab-console-feed"/);
    assert.match(drawerTabsHtml, /id="drawer-fold-toggle"[^>]*aria-expanded="true"/);
    assert.doesNotMatch(consoleSrc, /borderRadius:\s*"50%"|var\(--laser-green/, "网关状态点与文字颜色已迁入样式表");
    assert.doesNotMatch(homeSrc, /hud-tool-sub/, "工作台加工内容四个按钮统一两元素结构");
    assert.match(homeSrc, /hud-tool-jump-icon/, "创意画板按钮保留内联外跳箭头提示");
  });
});
