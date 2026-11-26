// 创作画板导入 DXF（2026-11-26）：实体解析、单位、凸度圆弧、孔方向、忽略项与错误提示。
import assert from "node:assert/strict";
import test from "node:test";
import { bulgePoints, dxfEntities, dxfPairs, dxfUnitMm, entityPolylines, extractDxfVectors, ignoredDxfSummary } from "../src/dxfImport.js";
import { signedArea } from "../src/svgImport.js";

const dxf = (rows) => rows.flat().join("\n");
const header = (units) => [["0", "SECTION"], ["2", "HEADER"], ["9", "$INSUNITS"], ["70", String(units)], ["0", "ENDSEC"]];
const entities = (...items) => [["0", "SECTION"], ["2", "ENTITIES"], ...items, ["0", "ENDSEC"], ["0", "EOF"]];
const lwSquare = (x0, y0, size, closed = true) => [
  ["0", "LWPOLYLINE"], ["8", "0"], ["90", "4"], ["70", closed ? "1" : "0"],
  ["10", String(x0)], ["20", String(y0)],
  ["10", String(x0 + size)], ["20", String(y0)],
  ["10", String(x0 + size)], ["20", String(y0 + size)],
  ["10", String(x0)], ["20", String(y0 + size)],
];
const circle = (cx, cy, r) => [["0", "CIRCLE"], ["8", "0"], ["10", String(cx)], ["20", String(cy)], ["40", String(r)]];

test("DXF 组码解析、单位与 ENTITIES 段切分", () => {
  const text = dxf([...header(1), ...entities(...circle(0, 0, 1), ["0", "TEXT"], ["1", "hi"])]);
  const pairs = dxfPairs(text);
  assert.deepEqual(dxfUnitMm(pairs), { mm: 25.4, code: 1, assumed: false });
  assert.deepEqual(dxfUnitMm(dxfPairs(dxf(entities()))), { mm: 1, code: null, assumed: true }, "没写单位按毫米");
  assert.deepEqual(dxfEntities(pairs).map((e) => e.type), ["CIRCLE", "TEXT"]);
  assert.throws(() => dxfPairs("AutoCAD Binary DXF\n\u0000"), /组码/);
});

test("凸度圆弧：bulge=1 是半圆，正值逆时针", () => {
  const pts = bulgePoints([0, 0], [2, 0], 1);
  const lowest = pts.reduce((a, b) => (b[1] < a[1] ? b : a));
  assert.ok(Math.abs(lowest[0] - 1) < 0.15 && Math.abs(lowest[1] + 1) < 0.01, `逆时针半圆从下方绕过：${lowest}`);
  assert.deepEqual(pts[pts.length - 1].map((v) => Math.round(v * 1e6) / 1e6), [2, 0]);
  for (const [x, y] of pts) assert.ok(Math.abs(Math.hypot(x - 1, y) - 1) < 1e-9, "每个点都在圆上");
  assert.deepEqual(bulgePoints([0, 0], [1, 0], 0), [[1, 0]], "bulge=0 是直线");
});

test("实体折线：圆闭合、圆弧不闭合、椭圆与 POLYLINE/VERTEX", () => {
  const [c] = entityPolylines({ type: "CIRCLE", codes: [[10, "0"], [20, "0"], [40, "5"]], vertices: [] });
  assert.equal(c.closed, true);
  assert.ok(Math.abs(Math.abs(signedArea(c.points)) - Math.PI * 25) / (Math.PI * 25) < 0.005, "弦高 0.01 mm 时圆面积误差 < 0.5%");
  const [a] = entityPolylines({ type: "ARC", codes: [[10, "0"], [20, "0"], [40, "1"], [50, "0"], [51, "90"]], vertices: [] });
  assert.equal(a.closed, false);
  assert.deepEqual(a.points[a.points.length - 1].map((v) => Math.round(v * 1e6) / 1e6), [0, 1]);
  const [e] = entityPolylines({ type: "ELLIPSE", codes: [[10, "0"], [20, "0"], [11, "4"], [21, "0"], [40, "0.5"], [41, "0"], [42, String(2 * Math.PI)]], vertices: [] });
  assert.equal(e.closed, true);
  assert.ok(Math.abs(Math.abs(signedArea(e.points)) - Math.PI * 4 * 2) / (Math.PI * 8) < 0.005);
  const [p] = entityPolylines({
    type: "POLYLINE",
    codes: [[70, "1"]],
    vertices: [[[10, "0"], [20, "0"]], [[10, "3"], [20, "0"]], [[10, "3"], [20, "3"]]],
  });
  assert.equal(p.closed, true);
  assert.equal(p.points.length, 4, "闭合多段线首尾自动连上");
  assert.equal(entityPolylines({ type: "INSERT", codes: [], vertices: [] }), null);
});

test("DXF 转矢量：外框与孔同组、孔反向，y 轴翻转，忽略项计数", () => {
  const text = dxf([
    ...header(4),
    ...entities(
      ...lwSquare(0, 0, 20),
      ...circle(10, 10, 3),
      ["0", "LINE"], ["10", "0"], ["20", "25"], ["11", "20"], ["21", "25"],
      ["0", "TEXT"], ["1", "标签"],
      ["0", "INSERT"], ["2", "BLOCK1"],
    ),
  ]);
  const out = extractDxfVectors(text);
  assert.equal(out.units.mm, 1);
  assert.deepEqual(out.data.size, [20, 25]);
  assert.equal(out.data.paths.length, 1, "所有轮廓放进同一个图形");
  const [path] = out.data.paths;
  assert.equal(path.rings.length, 2);
  assert.equal(path.open.length, 1);
  assert.equal(path.fill, false);
  assert.equal(path.stroke, true, "CAD 线稿原样只描线");
  assert.ok(Math.sign(signedArea(path.rings[0])) !== Math.sign(signedArea(path.rings[1])), "孔与外框方向相反");
  // DXF y 向上：y=25 的直线在画布最上面（y=0）
  assert.deepEqual(path.open[0], [[0, 0], [20, 0]]);
  assert.deepEqual(out.ignored, { TEXT: 1, INSERT: 1 });
  assert.equal(ignoredDxfSummary(out.ignored), "已忽略：文字（请先转曲） 1 个、块引用（请先在 CAD 里分解） 1 个");
  assert.match(out.svg, /^<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg" viewBox="0 0 20 25" width="20mm" height="25mm"/);
  assert.doesNotMatch(out.svg, /script|href|url\(/i);

  const inches = extractDxfVectors(dxf([...header(1), ...entities(...lwSquare(0, 0, 1))]));
  assert.deepEqual(inches.data.size, [25.4, 25.4], "英寸换算成毫米");

  assert.throws(() => extractDxfVectors(dxf(entities(["0", "TEXT"], ["1", "x"]))), /没有可刻的线条/);
});

test("样条：3 次 B 样条按节点向量求值，首尾落在端点", () => {
  const codes = [[71, "3"], [72, "8"], [73, "4"],
    ...[0, 0, 0, 0, 1, 1, 1, 1].map((k) => [40, String(k)]),
    [10, "0"], [20, "0"], [10, "1"], [20, "2"], [10, "3"], [20, "2"], [10, "4"], [20, "0"]];
  const [s] = entityPolylines({ type: "SPLINE", codes, vertices: [] });
  const first = s.points[0];
  const last = s.points[s.points.length - 1];
  assert.deepEqual(first, [0, 0]);
  assert.ok(Math.abs(last[0] - 4) < 1e-9 && Math.abs(last[1]) < 1e-9);
  const mid = s.points[Math.floor(s.points.length / 2)];
  assert.ok(Math.abs(mid[0] - 2) < 0.05 && Math.abs(mid[1] - 1.5) < 0.05, `贝塞尔中点 (2, 1.5)：${mid}`);
});
