/**
 * 创作画板导入 SVG（2026-11-26）：把外部 SVG 转成画板里一个带矢量轮廓的图片元素。
 *
 * - 画布上显示的是清洗后的 SVG 原图（孔洞、颜色与设计软件一致），可拖动、缩放、旋转、翻转；
 * - 刀路用的是同一份轮廓：元素的 `customData.moss_vector`（版本 1），由后端 `sketch.rs::push_vector_image`
 *   放进元素框后按画板刻法（按画布颜色 / 轮廓 / 填充 / 切割）规划，不经过位图；
 * - 几何交给浏览器原生 SVG 引擎（变换、圆弧、单位、CSS 类样式都按标准算），在 Shadow DOM 里离屏测量，
 *   样式不会漏到应用界面；
 * - 安全：先按白名单清洗（只留图形元素与几何 / 样式属性），去掉脚本、事件属性、外链、`<use>`、
 *   `<image>`、`<foreignObject>` 等，`url(...)` 引用一律改成纯色，清洗后才插入页面。
 */

export const SVG_IMPORT_LIMITS = Object.freeze({
  maxBytes: 5 * 1024 * 1024,
  /** 与后端 `MAX_VECTOR_IMAGE_POINTS` / `MAX_VECTOR_IMAGE_PATHS` 一致 */
  maxPoints: 400_000,
  maxPaths: 20_000,
  /** 沿路径取点的间距（mm）；总点数超上限时自动放宽 */
  sampleStepMm: 0.05,
  /** 折线化后的简化容差（mm） */
  simplifyMm: 0.01,
});

const UNIT_MM = { "": 25.4 / 96, px: 25.4 / 96, mm: 1, cm: 10, in: 25.4, pt: 25.4 / 72, pc: 25.4 / 6, q: 0.25 };

/** SVG 长度（`100mm`、`3in`、`200`）换算成毫米；百分比、非正数或认不出的返回 null。 */
export function parseSvgLengthMm(value) {
  const match = /^\s*([+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?)\s*(px|mm|cm|in|pt|pc|q)?\s*$/i.exec(String(value ?? ""));
  if (!match) return null;
  const number = Number(match[1]);
  if (!Number.isFinite(number) || number <= 0) return null;
  return number * UNIT_MM[(match[2] || "").toLowerCase()];
}

/** `viewBox="x y w h"`（逗号或空格分隔）；宽高须为正。 */
export function parseViewBox(value) {
  const parts = String(value ?? "")
    .trim()
    .split(/[\s,]+/)
    .filter(Boolean)
    .map(Number);
  if (parts.length !== 4 || parts.some((n) => !Number.isFinite(n)) || parts[2] <= 0 || parts[3] <= 0) return null;
  return { x: parts[0], y: parts[1], w: parts[2], h: parts[3] };
}

/**
 * 一个 SVG 用户单位等于多少毫米：有 viewBox 且宽 / 高带单位时按“物理宽度 ÷ viewBox 宽度”，
 * 否则按 CSS 像素（96 dpi，Inkscape、Figma、浏览器的约定）。
 */
export function svgUserUnitMm({ width, height, viewBox }) {
  const box = parseViewBox(viewBox);
  const widthMm = parseSvgLengthMm(width);
  const heightMm = parseSvgLengthMm(height);
  if (box) {
    if (widthMm) return widthMm / box.w;
    if (heightMm) return heightMm / box.h;
  }
  return UNIT_MM.px;
}

const ALLOWED_ELEMENTS = new Set(["svg", "g", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon", "defs", "style", "title", "desc"]);
export const GEOMETRY_ELEMENTS = new Set(["path", "rect", "circle", "ellipse", "line", "polyline", "polygon"]);
const ALLOWED_ATTRS = new Set([
  "id", "class", "style", "transform", "d", "x", "y", "width", "height", "rx", "ry", "cx", "cy", "r",
  "x1", "y1", "x2", "y2", "points", "fill", "fill-rule", "fill-opacity", "stroke", "stroke-width",
  "stroke-opacity", "opacity", "display", "visibility", "viewBox", "preserveAspectRatio", "clip-rule",
]);
/** 设计软件常带、本来就不画出来的元素：去掉但不算“忽略了的图形” */
const SILENT_DROPS = new Set(["metadata", "namedview", "lineargradient", "radialgradient", "stop", "clippath", "mask", "pattern", "filter", "symbol", "marker", "sodipodi:namedview"]);

/** 样式文本里的外链与表达式改掉：`url(...)` → 纯黑（保留“有填充”的语义），`@import` 删除。 */
export function sanitizeStyleText(text) {
  return String(text ?? "")
    .replace(/@import[^;]*;?/gi, "")
    .replace(/url\s*\([^)]*\)/gi, "#000000")
    .replace(/expression\s*\(/gi, "(");
}

/**
 * 白名单清洗 SVG 文本（需要 `DOMParser` / `XMLSerializer`，浏览器里运行）。
 * 返回 `{ svg, dropped }`：清洗后的 SVG 文本与被忽略的图形类元素计数（如 `{ text: 2, image: 1 }`）。
 */
export function sanitizeSvgText(text, { DOMParserImpl = globalThis.DOMParser, XMLSerializerImpl = globalThis.XMLSerializer } = {}) {
  const doc = new DOMParserImpl().parseFromString(String(text ?? ""), "image/svg+xml");
  const root = doc.documentElement;
  if (!root || doc.getElementsByTagName("parsererror").length > 0 || root.localName !== "svg") {
    throw new Error("这个文件不是有效的 SVG，打不开");
  }
  const dropped = {};
  const clean = (element) => {
    for (const attr of Array.from(element.attributes)) {
      const name = attr.name;
      if (!ALLOWED_ATTRS.has(name)) {
        element.removeAttribute(name);
        continue;
      }
      if (name === "style" || name === "fill" || name === "stroke") {
        element.setAttribute(name, sanitizeStyleText(attr.value));
      }
    }
    for (const child of Array.from(element.childNodes)) {
      if (child.nodeType === 8 || child.nodeType === 7) {
        child.remove();
        continue;
      }
      if (child.nodeType !== 1) {
        // 文本只留在 <style> 里
        if (element.localName !== "style") child.remove();
        continue;
      }
      const tag = child.localName;
      if (!ALLOWED_ELEMENTS.has(tag) || child.namespaceURI !== "http://www.w3.org/2000/svg") {
        const key = String(child.nodeName || tag).toLowerCase();
        if (!SILENT_DROPS.has(key) && !SILENT_DROPS.has(String(tag).toLowerCase())) dropped[tag] = (dropped[tag] || 0) + 1;
        child.remove();
        continue;
      }
      if (tag === "style") {
        child.textContent = sanitizeStyleText(child.textContent);
        for (const attr of Array.from(child.attributes)) child.removeAttribute(attr.name);
        continue;
      }
      clean(child);
    }
  };
  clean(root);
  root.setAttribute("xmlns", "http://www.w3.org/2000/svg");
  return { svg: new XMLSerializerImpl().serializeToString(root), dropped };
}

/** 按相邻采样点的跳变拆子路径（`getPointAtLength` 跨过 moveto 时坐标会跳）。 */
export function splitAtJumps(points, maxGap) {
  const out = [];
  let current = [];
  for (const point of points) {
    const last = current[current.length - 1];
    if (last && Math.hypot(point[0] - last[0], point[1] - last[1]) > maxGap) {
      if (current.length > 0) out.push(current);
      current = [];
    }
    current.push(point);
  }
  if (current.length > 0) out.push(current);
  return out;
}

function perpendicularDistance(p, a, b) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const len = Math.hypot(dx, dy);
  if (len < 1e-12) return Math.hypot(p[0] - a[0], p[1] - a[1]);
  return Math.abs(dy * p[0] - dx * p[1] + b[0] * a[1] - b[1] * a[0]) / len;
}

/** Ramer–Douglas–Peucker 折线简化（迭代实现，长路径不爆栈）；首尾点保留。 */
export function simplifyPolyline(points, tolerance) {
  if (points.length <= 2) return points.slice();
  const keep = new Uint8Array(points.length);
  keep[0] = 1;
  keep[points.length - 1] = 1;
  const stack = [[0, points.length - 1]];
  while (stack.length > 0) {
    const [start, end] = stack.pop();
    let maxDistance = -1;
    let index = -1;
    for (let i = start + 1; i < end; i += 1) {
      const d = perpendicularDistance(points[i], points[start], points[end]);
      if (d > maxDistance) {
        maxDistance = d;
        index = i;
      }
    }
    if (index !== -1 && maxDistance > tolerance) {
      keep[index] = 1;
      stack.push([start, index], [index, end]);
    }
  }
  return points.filter((_, i) => keep[i]);
}

export function signedArea(ring) {
  let sum = 0;
  for (let i = 0; i < ring.length; i += 1) {
    const [x1, y1] = ring[i];
    const [x2, y2] = ring[(i + 1) % ring.length];
    sum += x1 * y2 - x2 * y1;
  }
  return sum / 2;
}

function pointInRing([x, y], ring) {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i, i += 1) {
    const [xi, yi] = ring[i];
    const [xj, yj] = ring[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

/**
 * 奇偶规则（`fill-rule: evenodd`）换成等价的方向：按嵌套层数，偶数层逆时针、奇数层顺时针，
 * 后端统一按非零规则填充即可得到同样的孔洞。
 */
export function orientForEvenOdd(rings) {
  return rings.map((ring, i) => {
    const probe = ring[0];
    let depth = 0;
    rings.forEach((other, j) => {
      if (j !== i && other.length >= 3 && pointInRing(probe, other)) depth += 1;
    });
    const wantPositive = depth % 2 === 0;
    const positive = signedArea(ring) > 0;
    return positive === wantPositive ? ring : ring.slice().reverse();
  });
}

/** 由采样得到的子路径（已是毫米坐标）整理成一个图形：填充图形的子路径一律闭合；开放线只在描边时保留。 */
export function shapeFromSubpaths(subpaths, { fill, stroke, evenOdd, closeGap }) {
  const rings = [];
  const open = [];
  for (const sub of subpaths) {
    if (sub.length < 2) continue;
    const first = sub[0];
    const last = sub[sub.length - 1];
    const closed = sub.length >= 3 && Math.hypot(first[0] - last[0], first[1] - last[1]) <= closeGap;
    if ((closed || fill) && sub.length >= 3) {
      const ring = closed ? sub.slice(0, -1) : sub.slice();
      if (ring.length >= 3 && Math.abs(signedArea(ring)) > 1e-9) rings.push(ring);
      else if (stroke) open.push(sub);
    } else if (stroke) {
      open.push(sub);
    }
  }
  return { rings: evenOdd ? orientForEvenOdd(rings) : rings, open, fill, stroke };
}

const round = (value) => Math.round(value * 1000) / 1000;

/**
 * 把所有图形平移到左上角为原点，生成 `customData.moss_vector`。没有可刻图形时返回 null。
 * 坐标保留 3 位小数（微米级），闭合环首尾补成同一点。
 */
export function buildVectorData(shapes) {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const shape of shapes) {
    for (const line of [...shape.rings, ...shape.open]) {
      for (const [x, y] of line) {
        if (x < minX) minX = x;
        if (y < minY) minY = y;
        if (x > maxX) maxX = x;
        if (y > maxY) maxY = y;
      }
    }
  }
  if (!Number.isFinite(minX)) return null;
  const move = (line) => line.map(([x, y]) => [round(x - minX), round(y - minY)]);
  const paths = shapes
    .filter((shape) => shape.rings.length > 0 || shape.open.length > 0)
    .map((shape) => ({
      rings: shape.rings.map((ring) => {
        const moved = move(ring);
        moved.push(moved[0].slice());
        return moved;
      }),
      open: shape.open.map(move),
      fill: Boolean(shape.fill),
      stroke: Boolean(shape.stroke),
    }));
  return { version: 1, size: [round(maxX - minX), round(maxY - minY)], paths };
}

/** 导入图形在画布上的最小边长（mm）：一条横线也要有可见的高度；与元素框、显示 SVG 一致。 */
export const MIN_VECTOR_IMAGE_MM = 0.25;

/** 图形集合的外框（mm）。 */
export function shapesBounds(shapes) {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const shape of shapes) {
    for (const line of [...shape.rings, ...shape.open]) {
      for (const [x, y] of line) {
        minX = Math.min(minX, x);
        minY = Math.min(minY, y);
        maxX = Math.max(maxX, x);
        maxY = Math.max(maxY, y);
      }
    }
  }
  return { minX, minY, maxX, maxY };
}

/** 改写根元素的 viewBox / 宽高，让 SVG 正好显示给定的用户坐标框。 */
export function cropSvgText(svgText, { x, y, w, h, widthMm, heightMm }, { DOMParserImpl = globalThis.DOMParser, XMLSerializerImpl = globalThis.XMLSerializer } = {}) {
  const doc = new DOMParserImpl().parseFromString(svgText, "image/svg+xml");
  const root = doc.documentElement;
  const fmt = (n) => String(Math.round(n * 1e6) / 1e6);
  root.setAttribute("viewBox", `${fmt(x)} ${fmt(y)} ${fmt(w)} ${fmt(h)}`);
  root.setAttribute("width", `${fmt(widthMm)}mm`);
  root.setAttribute("height", `${fmt(heightMm)}mm`);
  root.setAttribute("preserveAspectRatio", "none");
  // 描边宽的图形边缘不被裁掉一半
  root.setAttribute("overflow", "visible");
  return new XMLSerializerImpl().serializeToString(root);
}

export function countVectorPoints(data) {
  return (data?.paths || []).reduce(
    (sum, path) => sum + [...path.rings, ...path.open].reduce((n, line) => n + line.length, 0),
    0,
  );
}

function visiblePaint(value) {
  const paint = String(value ?? "").trim().toLowerCase();
  if (!paint || paint === "none" || paint === "transparent") return false;
  if (/^rgba\(.*,\s*0(\.0+)?\s*\)$/.test(paint)) return false;
  return true;
}

/**
 * 在浏览器里测量清洗后的 SVG：每个图形元素沿路径取点、套上累计变换、换算成毫米并简化。
 * 返回 `{ data, sizeMm, ignored }`；`ignored` 是被忽略的图形类元素计数。
 */
export function extractSvgVectors(svgText, { document: doc = globalThis.document, limits = SVG_IMPORT_LIMITS } = {}) {
  if (String(svgText ?? "").length > limits.maxBytes) {
    throw new Error(`SVG 文件太大（上限 ${Math.round(limits.maxBytes / 1024 / 1024)} MB），请先在设计软件里简化`);
  }
  const { svg, dropped } = sanitizeSvgText(svgText);
  const host = doc.createElement("div");
  host.setAttribute("aria-hidden", "true");
  // 不能用 visibility:hidden：会被子元素继承，图形全被当成“隐藏”跳过
  host.style.cssText = "position:fixed;left:-100000px;top:0;width:10px;height:10px;overflow:hidden;opacity:0;pointer-events:none;";
  doc.body.appendChild(host);
  try {
    const shadow = host.attachShadow({ mode: "closed" });
    const holder = doc.createElement("div");
    shadow.appendChild(holder);
    holder.innerHTML = svg;
    const root = holder.querySelector("svg");
    if (!root) throw new Error("这个文件不是有效的 SVG，打不开");
    const unitMm = svgUserUnitMm({
      width: root.getAttribute("width"),
      height: root.getAttribute("height"),
      viewBox: root.getAttribute("viewBox"),
    });
    // 让视口像素正好等于 viewBox 用户单位，getCTM() 即“元素 → 用户单位”的变换
    const box = parseViewBox(root.getAttribute("viewBox"));
    if (box) {
      root.setAttribute("width", String(box.w));
      root.setAttribute("height", String(box.h));
      root.setAttribute("preserveAspectRatio", "none");
    }
    const elements = Array.from(root.querySelectorAll(Array.from(GEOMETRY_ELEMENTS).join(","))).filter(
      (el) => !el.closest("defs"),
    );
    if (elements.length > limits.maxPaths) {
      throw new Error(`SVG 图形太多（${elements.length} 个，上限 ${limits.maxPaths}），请先在设计软件里合并或简化`);
    }
    const measured = [];
    let totalLengthMm = 0;
    for (const el of elements) {
      const style = globalThis.getComputedStyle(el);
      if (style.display === "none" || style.visibility === "hidden" || Number(style.opacity) === 0) continue;
      const fill = visiblePaint(style.fill) && Number(style.fillOpacity) !== 0;
      const stroke = visiblePaint(style.stroke) && Number(style.strokeOpacity) !== 0 && parseFloat(style.strokeWidth) !== 0;
      if (!fill && !stroke) continue;
      const matrix = el.getCTM();
      if (!matrix) continue;
      const length = el.getTotalLength();
      if (!(length > 0)) continue;
      // 变换后的最大伸缩倍数，换算采样间距
      const stretch = Math.max(Math.hypot(matrix.a, matrix.b), Math.hypot(matrix.c, matrix.d)) || 1;
      totalLengthMm += length * stretch * unitMm;
      measured.push({ el, matrix, length, stretch, fill, stroke, evenOdd: style.fillRule === "evenodd" });
    }
    const stepMm = Math.max(limits.sampleStepMm, totalLengthMm / (limits.maxPoints * 0.9));
    const shapes = [];
    for (const { el, matrix, length, stretch, fill, stroke, evenOdd } of measured) {
      const stepUser = stepMm / (stretch * unitMm);
      const count = Math.max(2, Math.ceil(length / stepUser));
      const points = [];
      for (let i = 0; i <= count; i += 1) {
        const p = el.getPointAtLength((length * i) / count);
        points.push([(matrix.a * p.x + matrix.c * p.y + matrix.e) * unitMm, (matrix.b * p.x + matrix.d * p.y + matrix.f) * unitMm]);
      }
      const gap = stepMm * 1.5 + 1e-6;
      const subpaths = splitAtJumps(points, gap).map((sub) => simplifyPolyline(sub, limits.simplifyMm));
      shapes.push(shapeFromSubpaths(subpaths, { fill, stroke, evenOdd, closeGap: gap }));
    }
    const data = buildVectorData(shapes);
    if (!data) throw new Error("SVG 里没有可刻的图形（路径没有填充也没有描边，或文字没有转曲）");
    if (countVectorPoints(data) > limits.maxPoints) {
      throw new Error(`SVG 轮廓点数超过 ${limits.maxPoints}，请先在设计软件里简化路径`);
    }
    // 显示用的 SVG 裁到图形外框：画布上的图片与刀路轮廓按同一个框对齐（原画幅四周留白不能算进去）
    const bounds = shapesBounds(shapes);
    const widthMm = Math.max(data.size[0], MIN_VECTOR_IMAGE_MM);
    const heightMm = Math.max(data.size[1], MIN_VECTOR_IMAGE_MM);
    const cropped = cropSvgText(svg, {
      x: bounds.minX / unitMm + (box ? box.x : 0),
      y: bounds.minY / unitMm + (box ? box.y : 0),
      w: widthMm / unitMm,
      h: heightMm / unitMm,
      widthMm,
      heightMm,
    });
    return { data, sizeMm: { width: data.size[0], height: data.size[1] }, ignored: dropped, svg: cropped };
  } finally {
    host.remove();
  }
}

const IGNORED_LABEL = { text: "文字（请先转曲）", image: "位图", use: "引用图形 <use>", foreignObject: "嵌入网页", script: "脚本", a: "链接" };

/** 导入结果提示：被忽略的元素说清楚是什么。 */
export function ignoredSummary(ignored) {
  const parts = Object.entries(ignored || {})
    .filter(([, n]) => n > 0)
    .map(([tag, n]) => `${IGNORED_LABEL[tag] || `<${tag}>`} ${n} 个`);
  return parts.length > 0 ? `已忽略：${parts.join("、")}` : "";
}

/** 画布单位：导入时 1 mm 放成多少画布像素（只影响画布上的显示大小，成品尺寸由“宽 / 高”决定）。 */
export const SVG_CANVAS_PX_PER_MM = 4;

/** 是否为带矢量轮廓的导入 SVG 图片元素（这类图片矢量模式可刻，不算位图）。 */
export function isVectorImageElement(element) {
  return Boolean(element && element.type === "image" && element.customData?.moss_vector?.version === 1);
}

/** 清洗后的 SVG 文本转 data URL（画布显示用）。 */
export function svgDataUrl(svg) {
  const bytes = new TextEncoder().encode(svg);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return `data:image/svg+xml;base64,${btoa(binary)}`;
}
