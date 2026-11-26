import test from "node:test";
import assert from "node:assert/strict";
import {
  buildDrawFramingPayload,
  checkDerivedDrawAdoption,
  computeSceneRevision,
  drawConnectionPayload,
  drawGenerationSignature,
  drawInkExportMode,
  drawRasterImagePayload,
  drawRequestMode,
  drawSizeValue,
  drawTargetLabel,
  drawTargetMissingText,
  drawTargetReady,
  formatFramingStatus,
  framingConfirmText,
  isDrawConnectionOk,
  isDrawPreviewStale,
  normalizeDrawImageStrategy,
  parseImportedSceneJson,
  readDrawDefaultForm,
  selectedElementIds,
} from "../src/drawConnection.js";

test("画板连接：网口与串口各自只带本方式的字段", () => {
  const network = { connection_mode: "network", network_host: " laser.local ", network_telnet_port: "23", port: "COM3", baudrate: "115200" };
  assert.deepEqual(drawConnectionPayload(network), { connection_mode: "network", network_host: "laser.local", network_telnet_port: 23 });
  assert.equal(drawTargetReady(network), true);
  assert.equal(drawTargetLabel(network), "laser.local:23");

  const serial = { ...network, connection_mode: "serial", network_host: "" };
  assert.deepEqual(drawConnectionPayload(serial), { connection_mode: "serial", port: "COM3", baudrate: 115200 });
  assert.equal(drawTargetReady(serial), true);
  assert.match(drawTargetLabel(serial), /串口 COM3/);
  assert.equal(drawTargetReady({ ...serial, port: " " }), false);
  assert.equal(drawTargetMissingText(serial), "请先选择串口");
  assert.equal(drawTargetReady({ connection_mode: "network", network_host: "" }), false);
});

test("画板尺寸：留空为自动（null），不再用 1 mm 兜底；生成方式由雕刻路线与转图片方式决定", () => {
  assert.equal(drawSizeValue(""), null);
  assert.equal(drawSizeValue("  "), null);
  assert.equal(drawSizeValue("0"), null);
  assert.equal(drawSizeValue("-5"), null);
  assert.equal(drawSizeValue("50"), 50);
  assert.equal(drawSizeValue("12.5"), 12.5);
  // 2026-11-17（r45）：矢量路线一律 vector；转图片按转图片方式（默认光栅扫描）
  assert.equal(drawRequestMode("vector", "outline"), "vector");
  assert.equal(drawRequestMode(undefined, "raster"), "vector", "没读到路线时按矢量");
  assert.equal(drawRequestMode("image", "raster"), "raster");
  assert.equal(drawRequestMode("image", "outline"), "outline");
  assert.equal(drawRequestMode("image", undefined), "raster");
  assert.equal(drawRequestMode("image", "vector"), "raster", "转图片方式里没有矢量");
  assert.equal(normalizeDrawImageStrategy("auto"), "raster");
  assert.equal(normalizeDrawImageStrategy(null), "raster");
  assert.equal(normalizeDrawImageStrategy("outline"), "outline");
  // 旧版本的“生成策略”（laserLab.mode）迁移：轮廓描边 → 轮廓描边，光栅 / 矢量 / 没存 → 光栅扫描；新键优先
  const store = (entries) => ({ getItem: (key) => entries[key] ?? null });
  assert.equal(readDrawDefaultForm(store({ "laserLab.mode": "outline" })).image_strategy, "outline");
  assert.equal(readDrawDefaultForm(store({ "laserLab.mode": "raster" })).image_strategy, "raster");
  assert.equal(readDrawDefaultForm(store({ "laserLab.mode": "vector" })).image_strategy, "raster");
  assert.equal(readDrawDefaultForm(store({})).image_strategy, "raster");
  assert.equal(
    readDrawDefaultForm(store({ "laserLab.mode": "outline", "laserLab.image_strategy": "raster" })).image_strategy,
    "raster",
    "已按新版本存过转图片方式时不再看旧键",
  );
  assert.equal(readDrawDefaultForm(store({})).mode, undefined, "表单不再有生成策略字段");
  assert.equal(readDrawDefaultForm(store({ "laserLab.image_mode": "jarvis" })).image_mode, "jarvis");
  assert.equal(readDrawDefaultForm(store({ "laserLab.image_mode": "bogus" })).image_mode, "auto");
  assert.equal(readDrawDefaultForm(store({ "laserLab.line_interval_scale": "1.5" })).line_interval_scale, "1.5");
});

test("画板光栅扫描：转换方式与行间距倍数沿用工作台图片的校验；黑白方式才把彩色笔迹压成纯黑", () => {
  assert.deepEqual(drawRasterImagePayload({ image_mode: "auto", line_interval_scale: "" }), { image_mode: "auto" });
  assert.deepEqual(drawRasterImagePayload({ image_mode: "jarvis", line_interval_scale: "0.8" }), { image_mode: "jarvis", line_interval_scale: 0.8 });
  assert.deepEqual(drawRasterImagePayload({ image_mode: "bogus", line_interval_scale: "1" }), { image_mode: "auto" });
  assert.match(drawRasterImagePayload({ image_mode: "auto", line_interval_scale: "9" }).error, /行间距倍数/);
  assert.equal(drawRasterImagePayload({}).width_mm, undefined, "成品尺寸由画板宽高字段给，不在这里");

  assert.equal(drawInkExportMode("raster", "auto"), "raster");
  assert.equal(drawInkExportMode("raster", "threshold"), "raster");
  assert.notEqual(drawInkExportMode("raster", "jarvis"), "raster", "抖动 / 灰度要保留笔迹深浅");
  assert.notEqual(drawInkExportMode("raster", "grayscale"), "raster");
  assert.equal(drawInkExportMode("vector", "auto"), "vector");
  assert.equal(drawInkExportMode("outline", "auto"), "outline");
});

test("画板默认表单：存储被禁用时退回默认值；未存画板设备时自动复用工作台连接", () => {
  const blocked = { getItem() { throw new Error("SecurityError: storage disabled"); } };
  const form = readDrawDefaultForm(blocked);
  assert.equal(form.material, "椴木");
  assert.equal(form.thickness_mm, "3");
  assert.equal(form.connection_mode, "network");
  assert.equal(form.network_telnet_port, "23");
  assert.equal(form.baudrate, "115200");
  assert.equal(form.image_strategy, "raster", "转图片方式默认光栅扫描");
  assert.equal(form.image_mode, "auto");
  assert.equal(form.line_interval_scale, "");
  assert.deepEqual(readDrawDefaultForm(null), form);

  const saved = { getItem: (key) => ({ "laserLab.material": "亚克力", "laserLab.connection_mode": "serial", "laserLab.port": "COM7", "laserLab.mode": "outline" })[key] ?? null };
  const restored = readDrawDefaultForm(saved);
  assert.equal(restored.material, "亚克力");
  assert.equal(restored.connection_mode, "serial");
  assert.equal(restored.port, "COM7");
  assert.equal(restored.image_strategy, "outline", "旧版本存的轮廓描边迁移成转图片方式");

  // 画板尚未存设备目标时，自动带入工作台已填好的网络或串口连接
  const fromWorkbench = {
    getItem: (key) => ({
      "laser-forge-conn-mode": "network",
      "laser-forge-host": "192.0.2.88",
      "laser-forge-telnet-port": "2323",
    })[key] ?? null,
  };
  const shared = readDrawDefaultForm(fromWorkbench);
  assert.equal(shared.connection_mode, "network");
  assert.equal(shared.network_host, "192.0.2.88");
  assert.equal(shared.network_telnet_port, "2323");
});

test("走边框确认文案与载荷：统一附带 relative_to_head 与正式雕刻同起点；串口连接判断看 serial_connected", () => {
  const form = { connection_mode: "network", network_host: "<device-ip>", network_telnet_port: "23" };
  const dark = framingConfirmText(form, 80, 50);
  assert.match(dark, /机器会实际移动/);
  assert.match(dark, /80\.0 × 50\.0 mm/);
  assert.match(dark, /激光保持关闭/);
  assert.match(dark, /<device-ip>:23/);
  assert.doesNotMatch(dark, /出光/);

  const payload = buildDrawFramingPayload(form, 80, 50);
  assert.equal(payload.relative_to_head, true, "画板走边框必须与 G92 X0 Y0 正式雕刻共用当前激光头起点");
  assert.equal(payload.power, 0);
  // 橡皮筋：带轮廓时附带 outline，确认文案写明沿外轮廓
  const outline = [[0, 0], [80, 0], [40, 50]];
  assert.deepEqual(buildDrawFramingPayload(form, 80, 50, outline).outline, outline);
  assert.equal(buildDrawFramingPayload(form, 80, 50, null).outline, undefined);
  assert.match(framingConfirmText(form, 80, 50, 3), /图形外轮廓（3 个点/);

  const serialForm = { connection_mode: "serial", port: "COM5", baudrate: "115200" };
  assert.equal(
    isDrawConnectionOk(serialForm, { success: true, result: { serial_connected: true, network_connected: false } }),
    true,
    "串口握手成功必须判为连通",
  );
  assert.equal(
    isDrawConnectionOk(form, { success: true, result: { serial_connected: true, network_connected: false } }),
    false,
  );
});

test("画板预览过期检测与场景 JSON 导入校验", () => {
  const rev1 = computeSceneRevision([{ id: "rect-1", version: 2, versionNonce: 10, isDeleted: false }]);
  const revDeleted = computeSceneRevision([{ id: "rect-1", version: 3, versionNonce: 11, isDeleted: true }]);
  assert.equal(revDeleted, "empty");
  const baseForm = { material: "椴木", thickness_mm: "3", width_mm: "", height_mm: "", image_strategy: "raster", image_mode: "auto", line_interval_scale: "", vector_simplify_factor: "3" };
  const sig1 = drawGenerationSignature(baseForm, { enabled: false }, "image");
  const snap = { sceneRev: rev1, formSig: sig1 };
  assert.equal(isDrawPreviewStale(null, snap), false);
  assert.equal(isDrawPreviewStale(snap, { sceneRev: rev1, formSig: sig1 }), false);
  assert.equal(
    isDrawPreviewStale(snap, { sceneRev: computeSceneRevision([{ id: "rect-1", version: 3, versionNonce: 12 }]), formSig: sig1 }),
    true,
    "画布元素版本变化立即标过期",
  );
  assert.equal(
    isDrawPreviewStale(snap, { sceneRev: rev1, formSig: drawGenerationSignature({ ...baseForm, width_mm: "60" }, { enabled: false }, "image") }),
    true,
    "改尺寸立即标过期",
  );
  assert.equal(
    isDrawPreviewStale(snap, { sceneRev: rev1, formSig: drawGenerationSignature(baseForm, { enabled: true, power: "55", feed: "1200", passes: "1" }, "image") }),
    true,
    "开启临时自定义参数立即标过期",
  );
  // 2026-11-17（r45）：切换雕刻路线、转图片方式、转换方式、行间距倍数或扫描设置都让旧预览过期
  const stale = (formSig) => isDrawPreviewStale(snap, { sceneRev: rev1, formSig });
  assert.equal(stale(drawGenerationSignature(baseForm, { enabled: false }, "vector")), true, "切到矢量立即标过期");
  assert.equal(stale(drawGenerationSignature({ ...baseForm, image_strategy: "outline" }, { enabled: false }, "image")), true);
  assert.equal(stale(drawGenerationSignature({ ...baseForm, image_mode: "jarvis" }, { enabled: false }, "image")), true);
  assert.equal(stale(drawGenerationSignature({ ...baseForm, line_interval_scale: "1.5" }, { enabled: false }, "image")), true);
  assert.equal(stale(drawGenerationSignature({ ...baseForm, line_interval_scale: "1" }, { enabled: false }, "image")), false, "行间距留空与 1 相同");
  assert.equal(stale(drawGenerationSignature(baseForm, { enabled: false, fill: "scan:changed" }, "image")), true, "光栅扫描的扫描设置进签名");
  // 矢量路线下转图片的选项不影响刀路，改了不算过期
  const vectorSig = drawGenerationSignature(baseForm, { enabled: false }, "vector");
  assert.equal(drawGenerationSignature({ ...baseForm, image_strategy: "outline", image_mode: "jarvis" }, { enabled: false }, "vector"), vectorSig);

  assert.match(parseImportedSceneJson("not-json").error, /合法的 JSON/);
  assert.match(parseImportedSceneJson(JSON.stringify({ type: "other" })).error, /elements/);
  const ok = parseImportedSceneJson(JSON.stringify({
    type: "excalidraw",
    elements: [{ id: "a", type: "rectangle" }],
    appState: { gridSize: 20, viewBackgroundColor: "#ffffff", secret: "ignored" },
  }));
  assert.equal(ok.error, undefined);
  assert.equal(ok.elements.length, 1);
  assert.deepEqual(ok.appState, { gridSize: 20, viewBackgroundColor: "#ffffff" });
});

test("只刻选中的图形：取 Excalidraw 选区里为 true 的 id，排序去空", () => {
  assert.deepEqual(selectedElementIds({ selectedElementIds: { b: true, a: true, c: false, "": true } }), ["a", "b"]);
  assert.deepEqual(selectedElementIds({}), []);
  assert.deepEqual(selectedElementIds(null), []);
});

test("矢量刻法（r46 / r47）：轮廓 / 填充 / 切割、默认轮廓，进签名，填充查光栅参数、切割查切割参数", async () => {
  const {
    DRAW_VECTOR_STYLES,
    drawGenerationSignature,
    drawIsCut,
    drawVectorLayerFlags,
    drawRecommendEngravingMode,
    normalizeDrawVectorStyle,
    readDrawDefaultForm,
  } = await import("../src/drawConnection.js");
  assert.deepEqual(DRAW_VECTOR_STYLES.map((o) => o.value), ["outline", "fill", "cut"]);
  assert.equal(drawRecommendEngravingMode("vector", "cut"), "cut");
  assert.equal(drawIsCut("vector", "cut"), true);
  assert.equal(drawIsCut("raster", "cut"), false, "转图片不切割");
  assert.deepEqual(drawVectorLayerFlags("cut"), { withFill: false, withCut: true });
  assert.deepEqual(drawVectorLayerFlags("fill"), { withFill: true, withCut: false });
  assert.deepEqual(drawVectorLayerFlags("outline"), { withFill: false, withCut: false });
  assert.equal(normalizeDrawVectorStyle(undefined), "outline");
  assert.equal(normalizeDrawVectorStyle("canvas"), "outline");
  assert.equal(normalizeDrawVectorStyle("hatch"), "outline");
  const store = (entries) => ({ getItem: (k) => (k in entries ? entries[k] : null) });
  assert.equal(readDrawDefaultForm(store({})).vector_style, "outline");
  assert.equal(readDrawDefaultForm(store({ "laserLab.vector_style": "fill" })).vector_style, "fill");
  const form = { material: "椴木", thickness_mm: "3", vector_style: "outline" };
  assert.notEqual(
    drawGenerationSignature(form, {}, "vector"),
    drawGenerationSignature({ ...form, vector_style: "fill" }, {}, "vector"),
  );
  // 转图片时刻法不影响签名
  assert.equal(
    drawGenerationSignature(form, {}, "image"),
    drawGenerationSignature({ ...form, vector_style: "fill" }, {}, "image"),
  );
  assert.equal(drawRecommendEngravingMode("vector", "fill"), "raster");
  assert.equal(drawRecommendEngravingMode("vector", "outline"), "outline");
  assert.equal(drawRecommendEngravingMode("raster", "outline"), "raster");
  assert.equal(drawRecommendEngravingMode("outline", "fill"), "outline");
});

test("#16/#17 画板派生预览沿用原快照、拦截过期采用，且严格模式 202 走边框不误报完成", async () => {
  const framingPending = formatFramingStatus(
    {
      status: 202,
      ok: true,
      data: { success: true, result: { status: "approval_required", approval_id: "apr-f1", speech: "请在桌面批准走边框" } },
    },
    30,
    20,
  );
  assert.equal(framingPending, "请在桌面批准走边框");
  assert.doesNotMatch(framingPending, /走边框完成/);

  const framingDone = formatFramingStatus({ status: 200, ok: true, data: { success: true, result: {} } }, 30, 20);
  assert.match(framingDone, /走边框完成/);

  const snap = { sceneRev: "r1:1:1", formSig: "{\"material\":\"椴木\"}" };
  const blockedStale = checkDerivedDrawAdoption({
    workflow: { workflow_id: "wf-2", regenerated_from: "wf-1" },
    currentWorkflowId: "wf-1",
    previewSnap: snap,
    previewStale: true,
  });
  assert.equal(blockedStale.ok, false);

  const blockedMismatch = checkDerivedDrawAdoption({
    workflow: { workflow_id: "wf-2", regenerated_from: "wf-old" },
    currentWorkflowId: "wf-new",
    previewSnap: snap,
    previewStale: false,
  });
  assert.equal(blockedMismatch.ok, false);

  const accepted = checkDerivedDrawAdoption({
    workflow: { workflow_id: "wf-2", regenerated_from: "wf-1" },
    currentWorkflowId: "wf-1",
    previewSnap: snap,
    previewStale: false,
  });
  assert.equal(accepted.ok, true);
  assert.equal(accepted.inheritedSnap, snap);

  const { readFileSync } = await import("node:fs");
  const drawPageSrc = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  const drawSendCardSrc = readFileSync(new URL("../src/components/draw/DrawSendCard.jsx", import.meta.url), "utf8");
  assert.doesNotMatch(drawPageSrc, /setSceneRev\(revSnapshot\)/, "异步导出画板后不得回写旧 sceneRev 盖掉实时改动");
  assert.match(drawPageSrc, /isApprovalRequiredResponse\(res\)/, "画板发送前必须识别 202 待审批");
  assert.match(drawSendCardSrc, /disabled=\{busy \|\| previewStale \|\| jobActive\}/, "画板预览过期时必须禁用反馈调参");
});

test("画板矢量刻法不覆盖已保存切割选择：含位图或表面通用材料临时回退轮廓，移除限制后自动恢复切割", async () => {
  const { readFileSync } = await import("node:fs");
  const {
    availableDrawVectorStyles,
    effectiveDrawVectorStyle,
  } = await import("../src/drawConnection.js");

  assert.deepEqual(
    availableDrawVectorStyles({ hasSceneImages: false, surfaceOnly: false }).map((o) => o.value),
    ["outline", "fill", "cut"],
  );
  assert.deepEqual(
    availableDrawVectorStyles({ hasSceneImages: true, surfaceOnly: false }).map((o) => o.value),
    ["outline", "fill"],
  );
  assert.deepEqual(
    availableDrawVectorStyles({ hasSceneImages: false, surfaceOnly: true }).map((o) => o.value),
    ["outline", "fill"],
  );

  // 用户原选“切割”保存在 form.vector_style 中，插入图片或切到表面通用材料时只临时计算为 outline，删图后自动恢复 cut
  const savedStyle = "cut";
  assert.equal(effectiveDrawVectorStyle(savedStyle, { hasSceneImages: false, surfaceOnly: false }), "cut");
  assert.equal(effectiveDrawVectorStyle(savedStyle, { hasSceneImages: true, surfaceOnly: false }), "outline");
  assert.equal(effectiveDrawVectorStyle(savedStyle, { hasSceneImages: false, surfaceOnly: true }), "outline");
  assert.equal(effectiveDrawVectorStyle(savedStyle, { hasSceneImages: false, surfaceOnly: false }), "cut");
  assert.equal(effectiveDrawVectorStyle("fill", { hasSceneImages: true, surfaceOnly: true }), "fill");

  const drawPageSrc = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  assert.doesNotMatch(
    drawPageSrc,
    /setForm\([^)]*vector_style:\s*"outline"/,
    "DrawPage 不得用 useEffect 把用户的切割选择永久覆写为 outline",
  );
  assert.match(drawPageSrc, /const effectiveVectorStyle = effectiveDrawVectorStyle\(form\.vector_style/);
});

test("画板尺寸按雕刻机最大行程校验，且行程变化使旧预览签名过期", async () => {
  const { drawGenerationSignature, validateDrawDimensions } = await import("../src/drawConnection.js");
  assert.deepEqual(validateDrawDimensions({ width_mm: "", height_mm: "" }, { maxWidthMm: 100, maxHeightMm: 100 }), {
    widthMm: null,
    heightMm: null,
  });
  assert.deepEqual(validateDrawDimensions({ width_mm: "80", height_mm: "60" }, { maxWidthMm: 100, maxHeightMm: 100 }), {
    widthMm: 80,
    heightMm: 60,
  });
  assert.match(
    validateDrawDimensions({ width_mm: "120", height_mm: "60" }, { maxWidthMm: 100, maxHeightMm: 100 }).error,
    /成品宽度 120 mm 超出当前雕刻机最大行程 100 mm/,
  );
  assert.deepEqual(
    validateDrawDimensions({ width_mm: "200", height_mm: "150" }, { maxWidthMm: 220, maxHeightMm: 180 }),
    { widthMm: 200, heightMm: 150 },
    "大幅面雕刻机同步 220×180mm 行程后允许输入 200×150mm",
  );
  assert.match(
    validateDrawDimensions({ width_mm: "200", height_mm: "190" }, { maxWidthMm: 220, maxHeightMm: 180 }).error,
    /成品高度 190 mm 超出当前雕刻机最大行程 180 mm/,
  );

  const form = { material: "椴木", thickness_mm: "3", vector_style: "outline" };
  const sig100 = drawGenerationSignature(form, { workArea: "100x100" }, "vector");
  const sigDefault = drawGenerationSignature(form, {}, "vector");
  assert.equal(sig100, sigDefault, "默认 100×100 幅面保持既有签名格式");
  const sig220 = drawGenerationSignature(form, { workArea: "220x180" }, "vector");
  assert.notEqual(sig220, sig100, "雕刻机最大行程改变后旧预览签名应变化");
});
