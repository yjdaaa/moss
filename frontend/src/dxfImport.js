/**
 * 创作画板导入 DXF（2026-11-26）：解析 CAD 导出的 DXF（ASCII）实体，转成与导入 SVG 相同的
 * `customData.moss_vector`（版本 1），画布上显示由轮廓生成的线稿 SVG。
 *
 * - 支持 LINE、LWPOLYLINE / POLYLINE（含凸度圆弧）、CIRCLE、ARC、ELLIPSE、SPLINE（含有理样条）；
 *   块引用 INSERT、文字、标注、填充等不支持的实体计数后忽略并提示；
 * - 单位按 `$INSUNITS`（英寸、英尺、毫米、厘米、米；未设置按毫米）；DXF 的 y 向上，转成画布 y 向下；
 * - CAD 图里外轮廓与孔是各自独立的实体：所有闭合轮廓放进同一个图形、按嵌套层数定方向（奇偶规则），
 *   填充刻法时孔不出光，切割刻法时孔反向补偿；原样（按画布颜色）只描线。
 */
import { MIN_VECTOR_IMAGE_MM, SVG_IMPORT_LIMITS, buildVectorData, countVectorPoints, orientForEvenOdd, signedArea, simplifyPolyline } from "./svgImport.js";

const INSUNITS_MM = { 0: 1, 1: 25.4, 2: 304.8, 4: 1, 5: 10, 6: 1000, 8: 0.0000254, 9: 0.0254, 10: 914.4 };
/** 曲线折线化的弦高误差（mm） */
const CHORD_TOLERANCE_MM = 0.01;

/** 把 DXF 文本拆成 [组码, 值] 对。 */
export function dxfPairs(text) {
  const lines = String(text ?? "").split(/\r\n|\r|\n/);
  const pairs = [];
  for (let i = 0; i + 1 < lines.length; i += 2) {
    const code = Number.parseInt(lines[i].trim(), 10);
    if (!Number.isFinite(code)) throw new Error(`DXF 第 ${i + 1} 行不是组码，文件可能是二进制 DXF 或已损坏（请另存为 ASCII DXF）`);
    pairs.push([code, lines[i + 1].trim()]);
  }
  return pairs;
}

/** 读 `$INSUNITS`：返回每单位多少毫米与说明。 */
export function dxfUnitMm(pairs) {
  for (let i = 0; i < pairs.length; i += 1) {
    if (pairs[i][0] === 9 && pairs[i][1] === "$INSUNITS") {
      const next = pairs[i + 1];
      const code = next && next[0] === 70 ? Number(next[1]) : 0;
      if (code in INSUNITS_MM) return { mm: INSUNITS_MM[code], code, assumed: code === 0 };
      return { mm: 1, code, assumed: true };
    }
  }
  return { mm: 1, code: null, assumed: true };
}

/** 切出 ENTITIES 段的实体：[{ type, codes: [[code, value], ...] }]；POLYLINE 带上随后的 VERTEX。 */
export function dxfEntities(pairs) {
  const out = [];
  let inEntities = false;
  let current = null;
  for (let i = 0; i < pairs.length; i += 1) {
    const [code, value] = pairs[i];
    if (code === 0 && value === "SECTION") {
      const name = pairs[i + 1];
      inEntities = Boolean(name && name[0] === 2 && name[1] === "ENTITIES");
      current = null;
      continue;
    }
    if (!inEntities) continue;
    if (code === 0) {
      if (value === "ENDSEC") {
        inEntities = false;
        current = null;
        continue;
      }
      if ((value === "VERTEX" || value === "SEQEND") && current?.type === "POLYLINE") {
        if (value === "VERTEX") current.vertices.push([]);
        else current = null;
        continue;
      }
      current = { type: value, codes: [], vertices: [] };
      out.push(current);
      continue;
    }
    if (!current) continue;
    if (current.type === "POLYLINE" && current.vertices.length > 0) current.vertices[current.vertices.length - 1].push([code, value]);
    else current.codes.push([code, value]);
  }
  return out;
}

const num = (codes, code, fallback = 0) => {
  const hit = codes.find(([c]) => c === code);
  const value = hit ? Number(hit[1]) : NaN;
  return Number.isFinite(value) ? value : fallback;
};
const all = (codes, code) => codes.filter(([c]) => c === code).map(([, v]) => Number(v));

function arcSegments(radius, sweep, unitMm) {
  const r = Math.abs(radius) * unitMm;
  if (!(r > 0)) return 1;
  const step = 2 * Math.acos(Math.max(-1, Math.min(1, 1 - CHORD_TOLERANCE_MM / r)));
  return Math.max(8, Math.min(4096, Math.ceil(Math.abs(sweep) / (step || 0.01))));
}

/** 圆弧折线（角度为弧度、逆时针从 start 扫 sweep）。 */
function arcPoints(cx, cy, r, start, sweep, unitMm) {
  const n = arcSegments(r, sweep, unitMm);
  const pts = [];
  for (let i = 0; i <= n; i += 1) {
    const a = start + (sweep * i) / n;
    pts.push([cx + r * Math.cos(a), cy + r * Math.sin(a)]);
  }
  return pts;
}

/** 凸度圆弧：p1 → p2，bulge = tan(夹角/4)，正值逆时针。返回不含 p1、含 p2 的中间点。 */
export function bulgePoints(p1, p2, bulge, unitMm = 1) {
  if (!bulge || Math.abs(bulge) < 1e-12) return [p2];
  const theta = 4 * Math.atan(bulge);
  const dx = p2[0] - p1[0];
  const dy = p2[1] - p1[1];
  const chord = Math.hypot(dx, dy);
  if (chord < 1e-12) return [p2];
  const r = chord / (2 * Math.sin(Math.abs(theta) / 2));
  // 圆心在弦的中垂线上，距弦中点 d；bulge 正（逆时针）时圆心在弦左侧
  const d = r * Math.cos(Math.abs(theta) / 2);
  const mx = (p1[0] + p2[0]) / 2;
  const my = (p1[1] + p2[1]) / 2;
  const sign = theta > 0 ? 1 : -1;
  const cx = mx - (sign * d * dy) / chord;
  const cy = my + (sign * d * dx) / chord;
  const start = Math.atan2(p1[1] - cy, p1[0] - cx);
  return arcPoints(cx, cy, r, start, theta, unitMm).slice(1);
}

function polylineFromVertices(vertices, closed, unitMm) {
  const pts = [];
  const n = vertices.length;
  if (n === 0) return { points: pts, closed };
  pts.push([vertices[0].x, vertices[0].y]);
  const last = closed ? n : n - 1;
  for (let i = 0; i < last; i += 1) {
    const a = vertices[i];
    const b = vertices[(i + 1) % n];
    pts.push(...bulgePoints([a.x, a.y], [b.x, b.y], a.bulge, unitMm));
  }
  return { points: pts, closed };
}

/** 均匀 / 非均匀（有理）B 样条求值（de Boor）。 */
function splinePoints(codes, unitMm) {
  const degree = num(codes, 71, 3);
  const knots = all(codes, 40);
  const xs = all(codes, 10);
  const ys = all(codes, 20);
  const weights = all(codes, 41);
  const flags = num(codes, 70, 0);
  const ctrl = xs.map((x, i) => [x, ys[i] ?? 0]);
  if (ctrl.length < 2) {
    // 只有拟合点的样条：按拟合点连线
    const fx = all(codes, 11);
    const fy = all(codes, 21);
    return { points: fx.map((x, i) => [x, fy[i] ?? 0]), closed: Boolean(flags & 1) };
  }
  const p = Math.max(1, Math.min(degree, ctrl.length - 1));
  const w = ctrl.map((_, i) => (Number.isFinite(weights[i]) && weights[i] > 0 ? weights[i] : 1));
  let k = knots;
  if (k.length !== ctrl.length + p + 1) {
    // 缺节点或数目不对：按开放均匀节点补齐
    k = [];
    for (let i = 0; i < ctrl.length + p + 1; i += 1) {
      k.push(i <= p ? 0 : i >= ctrl.length ? ctrl.length - p : i - p);
    }
  }
  const t0 = k[p];
  const t1 = k[ctrl.length];
  let length = 0;
  for (let i = 1; i < ctrl.length; i += 1) length += Math.hypot(ctrl[i][0] - ctrl[i - 1][0], ctrl[i][1] - ctrl[i - 1][1]);
  const n = Math.max(16, Math.min(8192, Math.ceil((length * unitMm) / 0.1)));
  const evalAt = (t) => {
    let s = p;
    while (s < ctrl.length - 1 && t >= k[s + 1]) s += 1;
    const d = [];
    for (let j = 0; j <= p; j += 1) {
      const c = ctrl[s - p + j];
      const wj = w[s - p + j];
      d.push([c[0] * wj, c[1] * wj, wj]);
    }
    for (let r = 1; r <= p; r += 1) {
      for (let j = p; j >= r; j -= 1) {
        const i = s - p + j;
        const denom = k[i + p - r + 1] - k[i];
        const alpha = denom === 0 ? 0 : (t - k[i]) / denom;
        d[j] = [0, 1, 2].map((m) => (1 - alpha) * d[j - 1][m] + alpha * d[j][m]);
      }
    }
    const [x, y, ww] = d[p];
    return [x / ww, y / ww];
  };
  const points = [];
  for (let i = 0; i <= n; i += 1) points.push(evalAt(t0 + ((t1 - t0) * i) / n));
  return { points, closed: Boolean(flags & 1) };
}

/** 单个实体转折线（DXF 坐标、DXF 单位）；不支持的返回 null。 */
export function entityPolylines(entity, unitMm = 1) {
  const c = entity.codes;
  switch (entity.type) {
    case "LINE":
      return [{ points: [[num(c, 10), num(c, 20)], [num(c, 11), num(c, 21)]], closed: false }];
    case "LWPOLYLINE": {
      const vertices = [];
      for (const [code, value] of c) {
        if (code === 10) vertices.push({ x: Number(value), y: 0, bulge: 0 });
        else if (code === 20 && vertices.length) vertices[vertices.length - 1].y = Number(value);
        else if (code === 42 && vertices.length) vertices[vertices.length - 1].bulge = Number(value);
      }
      return [polylineFromVertices(vertices, Boolean(num(c, 70) & 1), unitMm)];
    }
    case "POLYLINE": {
      const vertices = entity.vertices.map((v) => ({ x: num(v, 10), y: num(v, 20), bulge: num(v, 42) }));
      return [polylineFromVertices(vertices, Boolean(num(c, 70) & 1), unitMm)];
    }
    case "CIRCLE": {
      const pts = arcPoints(num(c, 10), num(c, 20), num(c, 40), 0, 2 * Math.PI, unitMm);
      return [{ points: pts.slice(0, -1), closed: true }];
    }
    case "ARC": {
      const start = (num(c, 50) * Math.PI) / 180;
      let end = (num(c, 51) * Math.PI) / 180;
      while (end <= start) end += 2 * Math.PI;
      return [{ points: arcPoints(num(c, 10), num(c, 20), num(c, 40), start, end - start, unitMm), closed: false }];
    }
    case "ELLIPSE": {
      const cx = num(c, 10);
      const cy = num(c, 20);
      const mx = num(c, 11);
      const my = num(c, 21);
      const ratio = num(c, 40, 1);
      const a = Math.hypot(mx, my);
      const rot = Math.atan2(my, mx);
      const t0 = num(c, 41, 0);
      let t1 = num(c, 42, 2 * Math.PI);
      while (t1 <= t0) t1 += 2 * Math.PI;
      const full = Math.abs(t1 - t0 - 2 * Math.PI) < 1e-6;
      const n = arcSegments(a, t1 - t0, unitMm);
      const pts = [];
      for (let i = 0; i <= n; i += 1) {
        const t = t0 + ((t1 - t0) * i) / n;
        const ex = a * Math.cos(t);
        const ey = a * ratio * Math.sin(t);
        pts.push([cx + ex * Math.cos(rot) - ey * Math.sin(rot), cy + ex * Math.sin(rot) + ey * Math.cos(rot)]);
      }
      return [{ points: full ? pts.slice(0, -1) : pts, closed: full }];
    }
    case "SPLINE":
      return [splinePoints(c, unitMm)];
    default:
      return null;
  }
}

const IGNORED_DXF_LABEL = { INSERT: "块引用（请先在 CAD 里分解）", TEXT: "文字（请先转曲）", MTEXT: "多行文字（请先转曲）", HATCH: "填充图案", DIMENSION: "标注", SOLID: "实心填充", POINT: "点" };

export function ignoredDxfSummary(ignored) {
  const parts = Object.entries(ignored || {})
    .filter(([, n]) => n > 0)
    .map(([type, n]) => `${IGNORED_DXF_LABEL[type] || type} ${n} 个`);
  return parts.length > 0 ? `已忽略：${parts.join("、")}` : "";
}

/**
 * DXF 文本 → `{ data, sizeMm, ignored, svg, units }`；没有可刻图形时抛错。
 */
export function extractDxfVectors(text, { limits = SVG_IMPORT_LIMITS } = {}) {
  if (String(text ?? "").length > limits.maxBytes) {
    throw new Error(`DXF 文件太大（上限 ${Math.round(limits.maxBytes / 1024 / 1024)} MB），请先在 CAD 里简化`);
  }
  const pairs = dxfPairs(text);
  const units = dxfUnitMm(pairs);
  const entities = dxfEntities(pairs);
  if (entities.length > limits.maxPaths) {
    throw new Error(`DXF 实体太多（${entities.length} 个，上限 ${limits.maxPaths}），请先在 CAD 里合并或简化`);
  }
  const ignored = {};
  const rings = [];
  const open = [];
  for (const entity of entities) {
    const lines = entityPolylines(entity, units.mm);
    if (!lines) {
      ignored[entity.type] = (ignored[entity.type] || 0) + 1;
      continue;
    }
    for (const line of lines) {
      // 换成毫米、y 轴翻转为画布方向（向下）
      const pts = simplifyPolyline(
        line.points.filter((p) => p.every(Number.isFinite)).map(([x, y]) => [x * units.mm, -y * units.mm]),
        CHORD_TOLERANCE_MM,
      );
      if (pts.length < 2) continue;
      const first = pts[0];
      const last = pts[pts.length - 1];
      const touching = Math.hypot(first[0] - last[0], first[1] - last[1]) < 1e-6;
      if ((line.closed || touching) && pts.length >= 3) {
        const ring = touching ? pts.slice(0, -1) : pts;
        if (ring.length >= 3 && Math.abs(signedArea(ring)) > 1e-9) rings.push(ring);
        else open.push(pts);
      } else {
        open.push(pts);
      }
    }
  }
  const data = buildVectorData([{ rings: orientForEvenOdd(rings), open, fill: false, stroke: true }]);
  if (!data) throw new Error("DXF 里没有可刻的线条（只支持直线、多段线、圆、圆弧、椭圆与样条）");
  if (countVectorPoints(data) > limits.maxPoints) {
    throw new Error(`DXF 轮廓点数超过 ${limits.maxPoints}，请先在 CAD 里简化`);
  }
  return { data, sizeMm: { width: data.size[0], height: data.size[1] }, ignored, svg: vectorDataSvg(data), units };
}

/** 由矢量数据生成画布显示用的线稿 SVG（只有数字坐标，不含外部内容）。 */
export function vectorDataSvg(data) {
  // 与元素框同一个框（不留边），画布上的线稿与刀路轮廓对齐
  const w = Math.max(data.size[0], MIN_VECTOR_IMAGE_MM);
  const h = Math.max(data.size[1], MIN_VECTOR_IMAGE_MM);
  const stroke = Math.max(w, h, 1) / 400;
  const fmt = (n) => String(Math.round(n * 1000) / 1000);
  const d = [];
  for (const path of data.paths) {
    for (const ring of path.rings) d.push(`M${ring.map(([x, y]) => `${fmt(x)} ${fmt(y)}`).join("L")}Z`);
    for (const line of path.open) d.push(`M${line.map(([x, y]) => `${fmt(x)} ${fmt(y)}`).join("L")}`);
  }
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${fmt(w)} ${fmt(h)}" width="${fmt(w)}mm" height="${fmt(h)}mm" preserveAspectRatio="none" overflow="visible"><path d="${d.join("")}" fill="none" stroke="#111" stroke-width="${fmt(stroke)}" stroke-linejoin="round" stroke-linecap="round"/></svg>`;
}
