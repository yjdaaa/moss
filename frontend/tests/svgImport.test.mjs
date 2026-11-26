// 创作画板导入 SVG 的纯函数单测（2026-11-26）；依赖浏览器 SVG 引擎的测量部分见 svg-import.browser.mjs。
import assert from "node:assert/strict";
import test from "node:test";
import {
  buildVectorData,
  countVectorPoints,
  ignoredSummary,
  isVectorImageElement,
  orientForEvenOdd,
  parseSvgLengthMm,
  parseViewBox,
  sanitizeStyleText,
  shapeFromSubpaths,
  signedArea,
  simplifyPolyline,
  splitAtJumps,
  svgUserUnitMm,
} from "../src/svgImport.js";
import { hasSceneImageElements } from "../src/drawConnection.js";

test("SVG 长度与 viewBox 换算成毫米", () => {
  assert.equal(parseSvgLengthMm("100mm"), 100);
  assert.equal(parseSvgLengthMm("2cm"), 20);
  assert.equal(parseSvgLengthMm("1in"), 25.4);
  assert.equal(parseSvgLengthMm("72pt"), 25.4);
  assert.ok(Math.abs(parseSvgLengthMm("96") - 25.4) < 1e-9, "无单位按 CSS 像素 96 dpi");
  assert.equal(parseSvgLengthMm("50%"), null);
  assert.equal(parseSvgLengthMm("-3mm"), null);
  assert.deepEqual(parseViewBox("0,0 200 100"), { x: 0, y: 0, w: 200, h: 100 });
  assert.equal(parseViewBox("0 0 0 100"), null);
  assert.equal(svgUserUnitMm({ width: "20mm", height: "10mm", viewBox: "0 0 200 100" }), 0.1);
  assert.equal(svgUserUnitMm({ width: null, height: "10mm", viewBox: "0 0 200 100" }), 0.1);
  assert.ok(Math.abs(svgUserUnitMm({ width: "200", viewBox: null }) - 25.4 / 96) < 1e-12, "没有 viewBox 时用户单位就是像素");
});

test("采样点按跳变拆子路径，折线简化保留拐角", () => {
  const pts = [[0, 0], [0.05, 0], [0.1, 0], [5, 5], [5.05, 5]];
  assert.deepEqual(splitAtJumps(pts, 0.1), [[[0, 0], [0.05, 0], [0.1, 0]], [[5, 5], [5.05, 5]]]);
  const line = Array.from({ length: 101 }, (_, i) => [i / 10, 0]);
  assert.deepEqual(simplifyPolyline(line, 0.01), [[0, 0], [10, 0]], "直线只剩两个端点");
  const corner = [...Array.from({ length: 11 }, (_, i) => [i, 0]), ...Array.from({ length: 10 }, (_, i) => [10, i + 1])];
  assert.deepEqual(simplifyPolyline(corner, 0.01), [[0, 0], [10, 0], [10, 10]], "拐角点保留");
});

test("奇偶规则按嵌套层数换成方向，非零规则保持原方向", () => {
  const outer = [[0, 0], [10, 0], [10, 10], [0, 10]];
  const hole = [[3, 3], [7, 3], [7, 7], [3, 7]]; // 与外圈同向：奇偶规则下是孔，非零规则下不是
  const [o, h] = orientForEvenOdd([outer, hole]);
  assert.ok(signedArea(o) > 0 && signedArea(h) < 0, "外圈与孔方向相反，后端非零填充得到同样的孔");
  const shape = shapeFromSubpaths([[...outer, outer[0]], [...hole, hole[0]]], { fill: true, stroke: false, evenOdd: false, closeGap: 0.1 });
  assert.equal(shape.rings.length, 2);
  assert.ok(signedArea(shape.rings[0]) > 0 && signedArea(shape.rings[1]) > 0, "非零规则不改方向");
});

test("填充图形的开放子路径按闭合处理，只描边的开放线保留为线", () => {
  const open = [[0, 0], [10, 0], [10, 10]];
  const filled = shapeFromSubpaths([open], { fill: true, stroke: false, evenOdd: false, closeGap: 0.1 });
  assert.equal(filled.rings.length, 1);
  assert.equal(filled.open.length, 0);
  const stroked = shapeFromSubpaths([open], { fill: false, stroke: true, evenOdd: false, closeGap: 0.1 });
  assert.equal(stroked.rings.length, 0);
  assert.deepEqual(stroked.open, [open]);
  const neither = shapeFromSubpaths([open], { fill: false, stroke: false, evenOdd: false, closeGap: 0.1 });
  assert.equal(neither.rings.length + neither.open.length, 0);
});

test("矢量数据平移到左上角原点、闭合环首尾相同，空图形返回 null", () => {
  const data = buildVectorData([
    { rings: [[[5, 5], [15, 5], [15, 10]]], open: [[[5, 20], [9.12345, 20]]], fill: true, stroke: true },
    { rings: [], open: [], fill: false, stroke: true },
  ]);
  assert.equal(data.version, 1);
  assert.deepEqual(data.size, [10, 15]);
  assert.equal(data.paths.length, 1, "没有几何的图形不写入");
  assert.deepEqual(data.paths[0].rings[0], [[0, 0], [10, 0], [10, 5], [0, 0]]);
  assert.deepEqual(data.paths[0].open[0], [[0, 15], [4.123, 15]], "坐标保留 3 位小数");
  assert.equal(countVectorPoints(data), 6);
  assert.equal(buildVectorData([{ rings: [], open: [], fill: true, stroke: true }]), null);
});

test("样式清洗去掉外链与 @import；导入提示说清楚忽略了什么", () => {
  assert.equal(sanitizeStyleText("@import url(http://x/a.css); .a{fill:url(#g)}"), " .a{fill:#000000}");
  assert.equal(sanitizeStyleText("background:url( 'http://x' )"), "background:#000000");
  assert.equal(ignoredSummary({ text: 2, image: 1, use: 0 }), "已忽略：文字（请先转曲） 2 个、位图 1 个");
  assert.equal(ignoredSummary({}), "");
});

test("带矢量轮廓的导入 SVG 不算位图，矢量模式照样可刻", () => {
  const vector = { type: "image", customData: { moss_vector: { version: 1, size: [1, 1], paths: [] } } };
  assert.equal(isVectorImageElement(vector), true);
  assert.equal(isVectorImageElement({ type: "image" }), false);
  assert.equal(hasSceneImageElements([vector]), false);
  assert.equal(hasSceneImageElements([vector, { type: "image" }]), true);
  assert.equal(hasSceneImageElements([{ type: "image", isDeleted: true }]), false);
});

test("摄像头参考底图：锁定半透明垫底、按镜像 / 旋转摆正，不算位图", async () => {
  const { drawReferenceSkeleton, isDrawReferenceElement, offerDrawReference, takeDrawReference } = await import("../src/drawReference.js");
  const skeleton = drawReferenceSkeleton({ fileId: "f", naturalWidth: 640, naturalHeight: 480, mirror: true, flip180: true, device: "小智" });
  assert.equal(skeleton.width, 480);
  assert.equal(skeleton.height, 360);
  assert.equal(skeleton.locked, true);
  assert.equal(skeleton.opacity, 45);
  assert.deepEqual(skeleton.scale, [-1, 1]);
  assert.equal(skeleton.angle, Math.PI);
  assert.equal(isDrawReferenceElement(skeleton), true);
  assert.equal(hasSceneImageElements([skeleton]), false, "参考底图不让矢量模式退回光栅");
  assert.equal(isDrawReferenceElement({ type: "image" }), false);
  offerDrawReference({ dataUrl: "data:image/jpeg;base64,AA" });
  assert.equal(takeDrawReference()?.dataUrl, "data:image/jpeg;base64,AA");
  assert.equal(takeDrawReference(), null, "取一次即清空");
});
