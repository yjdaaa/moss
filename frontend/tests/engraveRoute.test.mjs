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
    const card = (form, drawMode, route) => renderToStaticMarkup(React.createElement(DrawMaterialCard, {
      form: { material: "椴木", thickness_mm: "3", width_mm: "", height_mm: "", image_strategy: "raster", image_mode: "auto", line_interval_scale: "", vector_simplify_factor: "3", ...form },
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
    const drawVectorFill = card({ vector_style: "fill" }, "vector", "vector");
    assert.match(drawVectorFill, /id="draw-fill-preset"/, "填充时显示填充设置");

    const drawRaster = card({ image_mode: "jarvis", line_interval_scale: "0.8" }, "raster", "image");
    assert.match(drawRaster, /id="draw-image-strategy"/);
    assert.doesNotMatch(drawRaster, /draw-vector-style/);
    assert.doesNotMatch(drawRaster, /<option value="vector"/, "转图片方式里没有矢量");
    assert.match(drawRaster, /<option value="raster" selected="">填充（逐行扫描/);
    assert.match(drawRaster, /<option value="outline">轮廓（描出深色边缘）/);
    // 转图片填充时显示扫描设置（双向、偏移、过扫描），不显示矢量的分组 / 角度
    assert.match(drawRaster, /id="draw-scan-fill-offset"/);
    assert.doesNotMatch(drawRaster, /draw-scan-fill-grouping|draw-scan-fill-angle/);
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

test("r47 精简：画板发送卡、设备卡与工作台监视舱去掉重复与调试项", async () => {
  const { readFileSync } = await import("node:fs");
  const read = (path) => readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8");
  const send = read("components/draw/DrawSendCard.jsx");
  for (const removed of ["刷新状态", "素材预览", "下载场景 JSON", "诊断输出", "workflow-chip", "弱光指示"]) {
    assert.ok(!send.includes(removed), `画板发送卡不再有：${removed}`);
  }
  assert.match(send, /取消任务/, "保留取消任务");
  assert.match(send, /路径预览/, "保留路径预览");
  assert.match(send, /下载 G-code/, "保留下载 G-code");
  assert.doesNotMatch(read("components/draw/DrawDeviceCard.jsx"), /onClick=\{saveConfiguration\}|>\s*保存配置\s*</, "设备卡没有保存配置按钮（自动保存）");
  const monitor = read("components/workbench/WorkbenchMonitorCard.jsx");
  assert.doesNotMatch(monitor, />\s*(物理 SHA-256|刀路总行数)\s*</, "监视舱不再显示 SHA-256 与总行数");
  assert.match(monitor, /预估耗时/);
  const page = read("pages/DrawPage.jsx");
  assert.match(page, /const lightOn = false;/, "画板走边框一律不出光");
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
