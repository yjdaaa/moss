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

test("转图片时文字刻法：描边 / 排线不可选、按实心；外框切割照样可选（始终走矢量）", () => {
  for (const style of ["outline", "fill", "hatch", "cut"]) {
    assert.equal(textStyleAvailable(style, "vector"), true);
    assert.equal(effectiveTextStyle(style, "vector"), style);
  }
  assert.equal(textStyleAvailable("outline", "image"), false);
  assert.equal(textStyleAvailable("hatch", "image"), false);
  assert.equal(textStyleAvailable("fill", "image"), true);
  assert.equal(textStyleAvailable("cut", "image"), true);
  assert.equal(effectiveTextStyle("outline", "image"), "fill");
  assert.equal(effectiveTextStyle("hatch", "image"), "fill");
  assert.equal(effectiveTextStyle("cut", "image"), "cut");
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
    assert.match(wbImage, /<option value="outline" disabled="">轮廓描边（仅矢量）<\/option>/);
    assert.match(wbImage, /<option value="hatch" disabled="">排线填充（线距更宽、更快）（仅矢量）<\/option>/);
    assert.match(wbImage, /<option value="fill" selected="">/);
    assert.match(wbImage, /<option value="cut">外框切割<\/option>/);
    assert.match(wbImage, /id="wb-text-route-note"[^>]*>转图片时文字一律实心；外框切割仍可选，并始终走矢量/);

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
    const drawVector = card({}, "vector", "vector");
    assert.match(drawVector, /id="draw-engrave-route"/);
    assert.match(drawVector, /id="draw-selected-only"/, "矢量时照旧可以只刻选中的图形");
    assert.match(drawVector, /id="draw-fill-preset"/, "矢量时照旧显示填充设置");
    assert.doesNotMatch(drawVector, /生成策略/, "旧的生成策略下拉已去掉");
    assert.doesNotMatch(drawVector, /draw-image-strategy|draw-image-mode/);

    const drawRaster = card({ image_mode: "jarvis", line_interval_scale: "0.8" }, "raster", "image");
    assert.match(drawRaster, /id="draw-image-strategy"/);
    assert.doesNotMatch(drawRaster, /<option value="vector"/, "转图片方式里没有矢量");
    assert.match(drawRaster, /<option value="raster" selected="">光栅扫描/);
    assert.match(drawRaster, /id="draw-image-mode"/);
    assert.match(drawRaster, /<option value="jarvis" selected="">/);
    assert.match(drawRaster, /aria-checked="true" id="draw-image-preset-fine"/, "Jarvis ×0.8 对应精细预设");
    assert.doesNotMatch(drawRaster, /draw-selected-only/, "只刻选中的图形只在矢量下");
    assert.doesNotMatch(drawRaster, /draw-fill-preset/);

    const drawOutline = card({ image_strategy: "outline" }, "outline", "image");
    assert.match(drawOutline, /<option value="outline" selected="">轮廓描边/);
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
