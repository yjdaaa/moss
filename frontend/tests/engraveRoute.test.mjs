import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import {
  DEFAULT_ENGRAVE_ROUTE,
  ENGRAVE_ROUTE_EVENT,
  ENGRAVE_ROUTE_READONLY_HINT,
  effectiveTextStyle,
  engraveRouteFromResponse,
  engraveRouteHint,
  engraveRouteSaveOutcome,
  normalizeEngraveRoute,
  textStyleAvailable,
} from "../src/engraveRoute.js";
import { DEFAULT_FILL_SETTINGS } from "../src/fillOptions.js";
import { workbenchInputSignature } from "../src/workbenchParams.js";
import { readPageSource } from "./pageSource.mjs";

// 2026-11-17（技术书 r45）：雕刻路线全局开关“矢量 / 转图片”，工作台、创作画板与语音共用后端同一个值

test("雕刻路线：默认矢量，响应解析失败时返回 null 保留原值", () => {
  assert.equal(DEFAULT_ENGRAVE_ROUTE, "vector");
  assert.equal(normalizeEngraveRoute("image"), "image");
  assert.equal(normalizeEngraveRoute("raster"), "vector");
  assert.equal(normalizeEngraveRoute(undefined), "vector");
  assert.match(engraveRouteHint("vector"), /矢量规划路线/);
  assert.match(engraveRouteHint("image"), /逐行扫描/);

  assert.equal(engraveRouteFromResponse({ ok: true, data: { success: true, result: { route: "image" } } }), "image");
  assert.equal(engraveRouteFromResponse({ ok: true, data: { success: true, result: { route: "bogus" } } }), null);
  assert.equal(engraveRouteFromResponse({ ok: false, status: 500, data: { success: false, result: "坏了" } }), null);
  assert.equal(engraveRouteFromResponse(null), null);
});

test("雕刻路线保存：成功按后端回显；局域网 403 与其他失败都回到原值并给出说明", () => {
  assert.deepEqual(
    engraveRouteSaveOutcome({ ok: true, status: 200, data: { success: true, result: { route: "image" } } }, "vector"),
    { route: "image" },
  );
  const lan = engraveRouteSaveOutcome({ ok: false, status: 403, data: { success: false, result: "forbidden" } }, "vector");
  assert.equal(lan.route, "vector");
  assert.equal(lan.error, ENGRAVE_ROUTE_READONLY_HINT);
  const failed = engraveRouteSaveOutcome({ ok: false, status: 500, data: { success: false, result: "保存雕刻路线失败：磁盘满" } }, "image");
  assert.equal(failed.route, "image");
  assert.match(failed.error, /磁盘满/);
  assert.match(engraveRouteSaveOutcome({ ok: false, error: "网络断开" }, "vector").error, /网络断开/);
});

test("转图片时文字刻法只有实心：描边 / 排线 / 外框切割都不列出、按实心（r46 只显示能用的）", () => {
  for (const style of ["outline", "fill", "hatch", "cut"]) {
    assert.equal(textStyleAvailable(style, "vector"), true);
    assert.equal(effectiveTextStyle(style, "vector"), style);
  }
  assert.equal(textStyleAvailable("outline", "image"), false);
  assert.equal(textStyleAvailable("hatch", "image"), false);
  assert.equal(textStyleAvailable("fill", "image"), true);
  assert.equal(textStyleAvailable("cut", "image"), false);
  assert.equal(effectiveTextStyle("outline", "image"), "fill");
  assert.equal(effectiveTextStyle("hatch", "image"), "fill");
  assert.equal(effectiveTextStyle("cut", "image"), "fill");
  assert.equal(textStyleAvailable("cut", "vector", { surfaceOnly: true }), false);
  assert.equal(textStyleAvailable("fill", "vector", { surfaceOnly: true }), true);
  assert.equal(effectiveTextStyle("cut", "vector", { surfaceOnly: true }), "outline");
  assert.equal(effectiveTextStyle("fill", "vector", { surfaceOnly: true }), "fill");
});

test("工作台签名：切换雕刻路线让文字预览过期；图片签名带扫描设置、与路线无关", () => {
  const text = { activeTool: "text", textContent: "MOSS", textStyle: "fill", selectedMaterial: "椴木", selectedThickness: "3" };
  assert.notEqual(
    workbenchInputSignature({ ...text, engraveRoute: "vector" }),
    workbenchInputSignature({ ...text, engraveRoute: "image" }),
  );
  assert.equal(workbenchInputSignature(text), workbenchInputSignature({ ...text, engraveRoute: "vector" }), "默认按矢量");

  const image = { activeTool: "image", uploadedImagePath: "a.png", selectedMaterial: "椴木", selectedThickness: "3" };
  assert.notEqual(
    workbenchInputSignature({ ...image, scanSignature: "scan:a" }),
    workbenchInputSignature({ ...image, scanSignature: "scan:b" }),
  );
  assert.equal(
    workbenchInputSignature({ ...image, engraveRoute: "vector" }),
    workbenchInputSignature({ ...image, engraveRoute: "image" }),
    "图片本来就逐行扫描，路线不影响",
  );
});

test("读取雕刻路线：GET /api/engrave-route，读到新值时在本窗口广播", async () => {
  const originalFetch = globalThis.fetch;
  const originalWindow = globalThis.window;
  const seen = [];
  const events = [];
  const target = new EventTarget();
  target.addEventListener(ENGRAVE_ROUTE_EVENT, (event) => events.push(event.detail.route));
  globalThis.window = target;
  globalThis.fetch = async (url, init = {}) => {
    seen.push({ url: String(url), method: init.method });
    return new Response(JSON.stringify({ success: true, result: { route: "image" } }), { status: 200 });
  };
  try {
    const { fetchEngraveRoute } = await import("../src/useEngraveRoute.js");
    assert.equal(await fetchEngraveRoute(), "image");
    assert.equal(await fetchEngraveRoute(), "image");
    assert.match(seen[0].url, /\/api\/engrave-route$/);
    assert.equal(seen[0].method, "GET");
    assert.deepEqual(events, ["image"], "值没变时不重复广播");
  } finally {
    globalThis.fetch = originalFetch;
    globalThis.window = originalWindow;
  }
});

test("雕刻路线开关、工作台刻法与画板转图片界面", async () => {
  const server = await createServer({
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    const { default: EngraveRouteSwitch } = await server.ssrLoadModule("/src/components/EngraveRouteSwitch.jsx");
    const sw = (props) => renderToStaticMarkup(React.createElement(EngraveRouteSwitch, { idPrefix: "wb", route: "vector", onChange: () => {}, ...props }));
    const vector = sw();
    assert.match(vector, /id="wb-engrave-route"/);
    assert.match(vector, /id="wb-engrave-route-vector"[^>]*class="ob-chip on"[^>]*>矢量</);
    assert.match(vector, /id="wb-engrave-route-image"[^>]*class="ob-chip"[^>]*>转图片</);
    assert.match(vector, /工作台、创作画板与语音共用/);
    assert.doesNotMatch(vector, /disabled/);
    const image = sw({ idPrefix: "draw", route: "image" });
    assert.match(image, /id="draw-engrave-route"/);
    assert.match(image, /aria-checked="true" id="draw-engrave-route-image" class="ob-chip on"/);
    const readOnly = sw({ readOnly: true });
    assert.equal((readOnly.match(/disabled=""/g) || []).length, 2, "连接电脑时两个选项都不可点");
    assert.match(readOnly, /只能查看雕刻路线/);

    const { WorkbenchTextTool } = await server.ssrLoadModule("/src/components/workbench/WorkbenchTextTool.jsx");
    const textTool = (props) => renderToStaticMarkup(React.createElement(WorkbenchTextTool, {
      textContent: "MOSS",
      setTextContent: () => {},
      fontSizeMm: 12,
      setFontSizeMm: () => {},
      textStyle: "outline",
      setTextStyle: () => {},
      textOrigin: "center",
      setTextOrigin: () => {},
      fillSettings: DEFAULT_FILL_SETTINGS,
      updateFillSettings: () => {},
      textUsesFill: false,
      textIsCut: false,
      materialApplied: [],
      materialLayer: null,
      saveFillToMaterial: () => {},
      ...props,
    }));
    const wbVector = textTool();
    assert.match(wbVector, /id="wb-engrave-route"/);
    assert.doesNotMatch(wbVector, /仅矢量/);
    assert.doesNotMatch(wbVector, /wb-text-route-note/);
    const wbImage = textTool({ engraveRoute: "image", textStyle: "fill", textUsesFill: true });
    // r46：只列能用的——转图片时只有实心扫描，描边 / 排线 / 切割不出现（不是置灰）
    assert.doesNotMatch(wbImage, /value="outline"|value="hatch"|value="cut"|仅矢量|disabled=""/);
    assert.match(wbImage, /<option value="fill" selected="">实心扫描（转图片逐行扫描）<\/option>/);
    assert.match(wbImage, /id="wb-text-route-note"[^>]*>转图片：文字渲染成黑白图逐行扫描/);
    // 转图片时只显示扫描设置，不显示分组 / 角度 / 交叉排线 / 走刀
    assert.match(wbImage, /id="wb-fill-offset"/);
    assert.doesNotMatch(wbImage, /wb-fill-grouping|wb-fill-angle|wb-fill-cross|wb-path-corners|wb-fill-preset/);
    assert.match(wbVector, /<option value="cut">外框切割<\/option>/, "矢量时四种刻法都在");

    const { DrawMaterialCard } = await server.ssrLoadModule("/src/components/draw/DrawMaterialCard.jsx");
    const card = (form, drawMode, route, extraProps = {}) => renderToStaticMarkup(React.createElement(DrawMaterialCard, {
      form: {
        material: "椴木",
        thickness_mm: "3",
        width_mm: "",
        height_mm: "",
        image_strategy: "raster",
        image_mode: "auto",
        line_interval_scale: "",
        halftone_cell_mm: "",
        halftone_angle_deg: "",
        gray_min_power_percent: "10",
        vector_simplify_factor: "3",
        ...form,
      },
      updateForm: () => {},
      drawMode,
      engraveRoute: route,
      setEngraveRoute: () => {},
      applyImageQualityPreset: () => {},
      materialLibrary: [],
      materialMode: "custom",
      materialSelectValue: "__custom__",
      handleMaterialSelectChange: () => {},
      handleThicknessSelectChange: () => {},
      handleReturnToLibrary: () => {},
      refreshMaterialLibrary: () => {},
      selectedOnly: false,
      setSelectedOnly: () => {},
      selectionKey: "",
      fillSettings: DEFAULT_FILL_SETTINGS,
      updateFillSettings: () => {},
      materialApplied: [],
      materialLayer: null,
      saveFillToMaterial: () => {},
      hasRec: false,
      customParams: false,
      setCustomParams: () => {},
      ...extraProps,
    }));
    const drawRasterNoCut = () => card({}, "raster", "image");
    const drawVector = card({}, "vector", "vector");
    assert.match(drawVector, /id="draw-engrave-route"/);
    assert.match(drawVector, /id="draw-selected-only"/, "矢量时照旧可以只刻选中的图形");
    assert.doesNotMatch(drawVector, /生成策略/, "旧的生成策略下拉已去掉");
    assert.doesNotMatch(drawVector, /draw-image-strategy|draw-image-mode/);
    // r46：矢量刻法只有轮廓 / 填充（默认轮廓）；轮廓时不显示填充设置，只有走刀
    assert.match(drawVector, /id="draw-vector-style"/);
    assert.match(drawVector, /<option value="outline" selected="">轮廓/);
    assert.match(drawVector, /<option value="fill">填充/);
    assert.doesNotMatch(drawVector, /按画布颜色/);
    assert.doesNotMatch(drawVector, /draw-fill-grouping/);
    const drawVectorCut = card({ vector_style: "cut" }, "vector", "vector");
    assert.match(drawVectorCut, /<option value="cut" selected="">切割/);
    assert.match(drawVectorCut, /id="draw-cut-kerf"/, "切割时显示切缝、桥位等切割设置");
    assert.doesNotMatch(drawVectorCut, /draw-fill-preset|draw-fill-grouping/, "切割不显示填充设置");
    assert.doesNotMatch(drawRasterNoCut(), /value="cut"/, "转图片时没有切割");
    const drawVectorFill = card({ vector_style: "fill" }, "vector", "vector", { materialLayer: { key: "k", label: "椴木 3mm" } });
    assert.match(drawVectorFill, /id="draw-fill-preset"/, "填充时显示填充设置");
    assert.match(drawVectorFill, /id="draw-material-save"/, "填充刻法可保存到材料库");
    const drawVectorOutlineWithLayer = card({ vector_style: "outline" }, "vector", "vector", { materialLayer: { key: "k", label: "椴木 3mm" } });
    assert.doesNotMatch(drawVectorOutlineWithLayer, /id="draw-material-save"/, "纯轮廓刻法无随材料保存字段，隐藏保存到材料库按钮");
    const drawVectorWithBitmap = card({ vector_style: "fill" }, "vector", "vector", { hasSceneImages: true });
    assert.doesNotMatch(drawVectorWithBitmap, /value="cut"|id="draw-selected-only"/, "画布含位图时隐藏矢量切割与只刻选中图形");
    assert.match(drawVectorWithBitmap, /id="draw-vector-image-note"/);
    assert.match(drawVectorWithBitmap, /id="draw-scan-fill-options"/);

    const drawRaster = card({ image_mode: "jarvis", line_interval_scale: "0.8" }, "raster", "image");
    assert.match(drawRaster, /id="draw-image-strategy"/);
    assert.doesNotMatch(drawRaster, /draw-vector-style/);
    assert.doesNotMatch(drawRaster, /<option value="vector"/, "转图片方式里没有矢量");
    assert.match(drawRaster, /<option value="raster" selected="">填充（逐行扫描/);
    assert.match(drawRaster, /<option value="outline">轮廓（描出深色边缘）/);
    // 转图片填充时显示扫描设置（双向、偏移、过扫描）与行间距倍数，不显示矢量的分组 / 角度
    assert.match(drawRaster, /id="draw-scan-fill-offset"/);
    assert.match(drawRaster, /id="draw-image-interval-scale"/);
    assert.doesNotMatch(drawRaster, /draw-scan-fill-grouping|draw-scan-fill-angle|draw-image-halftone-cell|draw-image-gray-min/);
    const drawHalftone = card({ image_mode: "halftone" }, "raster", "image");
    assert.match(drawHalftone, /id="draw-image-halftone-cell"/);
    assert.match(drawHalftone, /id="draw-image-halftone-angle"/);
    const drawGrayscale = card({ image_mode: "grayscale" }, "raster", "image");
    assert.match(drawGrayscale, /id="draw-image-gray-min"/);
    assert.match(drawRaster, /id="draw-image-mode"/);
    assert.match(drawRaster, /<option value="jarvis" selected="">/);
    assert.match(drawRaster, /aria-checked="true" id="draw-image-preset-fine"/, "Jarvis ×0.8 对应精细预设");
    assert.doesNotMatch(drawRaster, /draw-selected-only/, "只刻选中的图形只在矢量下");
    assert.doesNotMatch(drawRaster, /draw-fill-preset/);

    const drawOutline = card({ image_strategy: "outline" }, "outline", "image");
    assert.match(drawOutline, /<option value="outline" selected="">轮廓（描出深色边缘）/);
    assert.doesNotMatch(drawOutline, /draw-scan-fill-offset/, "描深色边缘不扫描");
    assert.match(drawOutline, /轮廓平滑度/);
    assert.doesNotMatch(drawOutline, /draw-image-mode|draw-image-preset/);
  } finally {
    await server.close();
  }
});

test("页面接线：工作台文字不带 engrave_route（跟随全局开关），画板写明实际 mode，图片带扫描子集", () => {
  const home = readPageSource("pages/HomePage.jsx");
  assert.match(home, /useEngraveRoute\(\)/);
  const textRequest = home.slice(home.indexOf('postJson("/api/text-task/generate"'), home.indexOf('} else if (activeTool === "image")'));
  assert.ok(textRequest.length > 0);
  assert.doesNotMatch(textRequest, /engrave_route/, "工作台文字跟随全局开关，与语音同一条");
  assert.match(home, /resolveTextStyle\(textStyleEffective\)/);
  const imageRequest = home.slice(home.indexOf('source_type: "image"'), home.indexOf('} else if (activeTool === "gcode")'));
  assert.match(imageRequest, /\.\.\.scanPayload/);
  assert.match(home, /scanSignature: rasterScanSignature\(fillSettings\)/);
  assert.match(home, /routeNote=\{routeNote\}/);

  const draw = readPageSource("pages/DrawPage.jsx");
  assert.match(draw, /useEngraveRoute\(\)/);
  assert.match(draw, /drawRequestMode\(engraveRoute, form\.image_strategy\)/);
  assert.match(draw, /mode: modeSnapshot,/);
  assert.match(draw, /selectedOnly && modeSnapshot === "vector"/, "只刻选中的图形只在矢量下提交");
  assert.match(draw, /rasterScanPayload\(fillSettings\)/);
  assert.doesNotMatch(draw, /form\.mode\b/, "画板表单不再有生成策略字段");
});

test("r47 / r72 精简：画板发送卡、连接条与工作台监视舱去掉重复与调试项", async () => {
  const { existsSync, readFileSync } = await import("node:fs");
  const read = (path) => readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8");
  const send = read("components/draw/DrawSendCard.jsx");
  for (const removed of ["刷新状态", "素材预览", "下载场景 JSON", "诊断输出", "workflow-chip", "弱光指示"]) {
    assert.ok(!send.includes(removed), `画板发送卡不再有：${removed}`);
  }
  assert.match(send, /取消任务/, "保留取消任务");
  assert.match(send, /tone="dark"/, "保留刀路预览（r49 深色底）");
  assert.match(send, /invert=\{result\?\.result\?\.artifacts\?\.mode === "raster"\}/, "只有光栅黑白图反色，矢量发光线原色显示");
  const css = readFileSync(new URL("../src/styles/15-addenda-2026-11.css", import.meta.url), "utf8");
  assert.match(css, /\.preview-box--dark img \{ background: transparent; max-height: 320px; filter: none; \}/);
  assert.match(send, /下载 G-code/, "保留下载 G-code");
  assert.equal(
    existsSync(new URL("../src/components/draw/DrawDeviceCard.jsx", import.meta.url)),
    false,
    "独立 DrawDeviceCard 已合入画布底部连接条并删除",
  );
  const monitor = read("components/workbench/WorkbenchMonitorCard.jsx");
  assert.doesNotMatch(monitor, />\s*(物理 SHA-256|刀路总行数)\s*</, "监视舱不再显示 SHA-256 与总行数");
  assert.match(monitor, /预估耗时/);
  const page = read("pages/DrawPage.jsx");
  assert.match(page, /id="draw-dock-connection"/, "画板连接入口收口在画布底部常驻连接条");
  assert.doesNotMatch(page, /DrawDeviceCard|onClick=\{saveConfiguration\}|>\s*保存配置\s*</, "画板不再渲染独立设备卡或保存配置按钮");
  assert.doesNotMatch(page, /\blightOn\b|弱光/, "画板走边框一律不出光");
  assert.match(page, /正在走边框（激光关闭，只移动激光头）/);
  assert.doesNotMatch(page, /\blastScene\b/, "导出场景不再读缓存的 lastScene");
});

test("r48 对刀起点：后端一个值，工作台读写、默认木牌中心，保存失败回到原值", async () => {
  const { normalizeTextOrigin, textOriginFromResponse, textOriginSaveOutcome, TEXT_ORIGIN_SHARED_HINT } =
    await import("../src/textOrigin.js");
  assert.equal(normalizeTextOrigin(undefined), "center");
  assert.equal(normalizeTextOrigin("top"), "center");
  assert.equal(normalizeTextOrigin("lower_left"), "lower_left");
  assert.equal(textOriginFromResponse({ ok: true, data: { success: true, result: { origin: "lower_left" } } }), "lower_left");
  assert.equal(textOriginFromResponse({ ok: false, data: { success: false } }), null);
  assert.deepEqual(
    textOriginSaveOutcome({ ok: true, data: { success: true, result: { origin: "lower_left" } } }, "center"),
    { origin: "lower_left" },
  );
  const failed = textOriginSaveOutcome({ ok: false, status: 403, data: { success: false, result: "lan_read_only" } }, "center");
  assert.equal(failed.origin, "center");
  assert.match(failed.error, /对刀起点没有保存/);
  assert.match(TEXT_ORIGIN_SHARED_HINT, /语音/);
  const { readFileSync } = await import("node:fs");
  const page = readFileSync(new URL("../src/pages/HomePage.jsx", import.meta.url), "utf8");
  assert.match(page, /useTextOrigin\(\)/, "工作台对刀起点来自后端全局设置");
  assert.doesNotMatch(page, /useState\("center"\)/, "不再是只在本页有效的本地状态");
});

test("r49 画板：画布与上次预览本机保存、切页面常驻、发送卡只显示一条最该看的提示", async () => {
  const {
    serializeDrawScene, parseDrawScene, serializeLastPreview, parseLastPreview, MAX_FILES_CHARS,
  } = await import("../src/drawScenePersist.js");
  const elements = [
    { id: "a", type: "rectangle", version: 2, versionNonce: 7, fileId: null },
    { id: "b", type: "image", version: 1, versionNonce: 3, fileId: "f1" },
    { id: "gone", type: "line", isDeleted: true },
  ];
  const files = { f1: { id: "f1", mimeType: "image/png", dataURL: "data:image/png;base64,AAAA", created: 1 }, unused: { dataURL: "x" } };
  const raw = serializeDrawScene(elements, { scrollX: 10, zoom: { value: 1.5 }, selectedElementIds: { a: true } }, files);
  const scene = parseDrawScene(raw);
  assert.deepEqual(scene.elements.map((e) => e.id), ["a", "b"], "删掉的元素不存");
  assert.deepEqual(Object.keys(scene.files), ["f1"], "只存用到的图片");
  assert.deepEqual(scene.appState, { scrollX: 10, zoom: { value: 1.5 } }, "只存视图位置，不存选区");
  assert.equal(scene.filesDropped, false);
  const big = { f1: { ...files.f1, dataURL: "x".repeat(MAX_FILES_CHARS + 1) } };
  const dropped = parseDrawScene(serializeDrawScene(elements, {}, big));
  assert.equal(dropped.filesDropped, true, "图片太大只存图形并标记");
  assert.equal(serializeDrawScene([{ id: "x", isDeleted: true }]), "", "空画布清掉保存");
  assert.equal(parseDrawScene("{broken"), null);
  const last = parseLastPreview(serializeLastPreview("draw-1", { sceneRev: "a:2:7", formSig: "{}" }));
  assert.deepEqual(last, { workflowId: "draw-1", snap: { sceneRev: "a:2:7", formSig: "{}" } });
  assert.equal(serializeLastPreview("", { sceneRev: "x", formSig: "y" }), "");

  const { drawSendNotice } = await import("../src/components/draw/DrawSendCard.jsx").catch(async () => {
    const { createServer } = await import("vite");
    const server = await createServer({ server: { middlewareMode: true }, appType: "custom", logLevel: "silent" });
    try {
      return await server.ssrLoadModule("/src/components/draw/DrawSendCard.jsx");
    } finally {
      await server.close();
    }
  });
  const base = { canActOnWorkflow: true, previewStale: false, hostReady: true, summaryAllowsSend: true, form: {} };
  assert.equal(drawSendNotice(base), null);
  assert.equal(drawSendNotice({ ...base, previewStale: true, hostReady: false }).id, "draw-stale-notice", "旧预览最优先");
  assert.equal(drawSendNotice({ ...base, hostReady: false }).id, "draw-target-notice");
  assert.equal(drawSendNotice({ ...base, summaryAllowsSend: false }).id, "draw-send-blocked");
  assert.equal(drawSendNotice({ ...base, canActOnWorkflow: false }), null);

  const { readFileSync } = await import("node:fs");
  const main = readFileSync(new URL("../src/main.jsx", import.meta.url), "utf8");
  assert.match(main, /id="draw-keepalive"/, "创作画板常驻");
  assert.match(main, /style=\{drawActive \? undefined : \{ display: "none" \}\}/, "切走只隐藏不卸载");
  const page = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  assert.match(page, /initialData=\{initialScene \?/, "打开时恢复上次的画布");
  assert.match(page, /DRAW_LAST_PREVIEW_KEY/, "记住上次预览");
});

test("画板一键清空画布：工具栏按钮、先确认、可撤销清空", async () => {
  const { readFileSync } = await import("node:fs");
  const page = readFileSync(new URL("../src/pages/DrawPage.jsx", import.meta.url), "utf8");
  assert.match(page, /id="draw-clear-canvas"/, "工具栏有清空画布按钮");
  assert.match(page, /id="draw-undo-clear"/, "清空后有撤销清空按钮");
  const fn = page.slice(page.indexOf("async function clearCanvas()"), page.indexOf("function undoClearCanvas()"));
  assert.ok(fn.indexOf("showConfirm(") > 0 && fn.indexOf("showConfirm(") < fn.indexOf("updateScene("), "清空前必须先确认");
  assert.match(fn, /if \(!confirmed\) return;/);
  assert.match(fn, /live\.length === 0/, "空画布不弹确认");
  assert.match(fn, /clearedSceneRef\.current = live;/, "清空前记下画布，供撤销");
});
