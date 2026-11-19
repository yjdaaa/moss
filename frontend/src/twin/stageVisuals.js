import * as THREE from "three";
import { RoundedBoxGeometry } from "three/examples/jsm/geometries/RoundedBoxGeometry.js";

// 视角预设（2026-11-07 视口改版）：默认“整机”斜 45° 看全机；切换时相机平滑过渡
export const CAMERA_PRESETS = {
  iso: { label: "整机", title: "整机 3D 视角", theta: -Math.PI * 0.2, phi: Math.PI * 0.32, distScale: 3.5 },
  fit: { label: "刀路", title: "聚焦刀路与板材", theta: Math.PI * 0.12, phi: Math.PI * 0.2, distScale: 2.3 },
  top: { label: "俯视", title: "俯视 XY", theta: 0, phi: 0.015, distScale: 2.7 },
  front: { label: "前视", title: "前视 XZ", theta: 0, phi: Math.PI * 0.44, distScale: 2.6 },
  side: { label: "侧视", title: "侧视 YZ", theta: -Math.PI * 0.5, phi: Math.PI * 0.4, distScale: 2.6 },
};
export const DEFAULT_PRESET = "iso";

/** 圆角盒体：打印件、电机与板材统一用小圆角，靠光照表现体积，避免“线框积木”观感。 */
export function rbox(w, h, d, r = 1.2) {
  const radius = Math.max(0.01, Math.min(r, Math.min(w, h, d) * 0.45));
  return new RoundedBoxGeometry(w, h, d, 3, radius);
}

/** 材料烧灼颜色：木材 / 皮革 / 纸为深褐碳化，亚克力为雾白，金属为浅灰白。 */
export function burnStyleFor(name = "") {
  const s = String(name);
  if (s.includes("亚克力") || s.toLowerCase().includes("acrylic")) return { rgb: "236, 240, 255", base: 0.35, gain: 0.45 };
  if (s.includes("金属") || s.includes("钢") || s.includes("铝")) return { rgb: "226, 232, 240", base: 0.4, gain: 0.5 };
  return { rgb: "46, 24, 10", base: 0.5, gain: 0.45 };
}

/** 生成板材表面底纹：木材画年轮纹理，其他材料为均匀底色加轻微噪点。 */
export function paintMaterialBase(g2d, width, height, name, colorHex) {
  const base = new THREE.Color(colorHex);
  g2d.fillStyle = `#${base.getHexString()}`;
  g2d.fillRect(0, 0, width, height);
  const s = String(name);
  const isWood = !(s.includes("亚克力") || s.toLowerCase().includes("acrylic") || s.includes("金属") || s.includes("钢") || s.includes("铝"));
  // 固定种子的伪随机，保证同一块板每次重建纹理一致
  let seed = 7;
  const rand = () => {
    seed = (seed * 16807) % 2147483647;
    return seed / 2147483647;
  };
  if (isWood) {
    const lines = Math.max(24, Math.round(height / 6));
    for (let i = 0; i < lines; i++) {
      const y0 = (i / lines) * height + rand() * 4;
      const amp = 1.5 + rand() * 4;
      const freq = 0.004 + rand() * 0.01;
      const phase = rand() * Math.PI * 2;
      const dark = rand() < 0.5;
      g2d.strokeStyle = dark ? `rgba(80, 45, 18, ${0.08 + rand() * 0.12})` : `rgba(255, 228, 190, ${0.05 + rand() * 0.08})`;
      g2d.lineWidth = 0.8 + rand() * 2.2;
      g2d.beginPath();
      for (let x = 0; x <= width; x += 8) {
        const y = y0 + Math.sin(x * freq + phase) * amp;
        if (x === 0) g2d.moveTo(x, y);
        else g2d.lineTo(x, y);
      }
      g2d.stroke();
    }
  }
  for (let i = 0; i < (width * height) / 90; i++) {
    g2d.fillStyle = rand() < 0.5 ? "rgba(0,0,0,0.05)" : "rgba(255,255,255,0.04)";
    g2d.fillRect(rand() * width, rand() * height, 1.5, 1.5);
  }
}

// 刀路 / 俯视视角下龙门与激光头半透明（2026-11-07 用户决定），透过它们看到落点与烧灼痕迹
export const GHOST_PRESETS = new Set(["fit", "top"]);
export const GHOST_FADE = 0.86;

/** level 0 = 实体，1 = 半透明；透明开关变化时才重新编译材质。 */
export function applyGhost(ctx, level) {
  if (!ctx?.ghostMats) return;
  const ghosted = level > 0.001;
  for (const entry of ctx.ghostMats) {
    const { mat } = entry;
    const transparent = entry.transparent || ghosted;
    if (mat.transparent !== transparent) {
      mat.transparent = transparent;
      mat.needsUpdate = true;
    }
    mat.opacity = entry.opacity * (1 - GHOST_FADE * level);
    mat.depthWrite = ghosted ? false : entry.depthWrite;
  }
  // 半透明时不再往板面投阴影
  for (const entry of ctx.ghostMeshes || []) {
    entry.mesh.castShadow = entry.castShadow && !ghosted;
  }
}

export function applyCoverVisibility(ctx, visible) {
  ctx?.enclosureGroup?.children.forEach((child) => {
    if (child.userData?.cover) child.visible = Boolean(visible);
  });
}

export function burnStroke(burn, powerRatio) {
  const p = Math.min(1, Math.max(0, Number(powerRatio) || 0.7));
  const alpha = Math.round((burn.style.base + burn.style.gain * p) * 20) / 20;
  return `rgba(${burn.style.rgb}, ${alpha})`;
}

/**
 * 把已完成的出光段增量画到板面画布上（2026-11-07）。
 * 只画新增部分；进度倒退、换刀路或改偏置时先铺回底纹再从头画。当前段只画到激光头所在位置。
 */
export function updateBurnSurface(burn, { segs, completed, activeFraction, key, mapX, mapY }) {
  const g = burn?.g2d;
  if (!g) return;
  const rewind =
    completed < burn.drawnSeg ||
    (completed === burn.drawnSeg && burn.partial && activeFraction < burn.partialFrac - 1e-6);
  let dirty = false;
  if (burn.key !== key || rewind) {
    g.globalCompositeOperation = "source-over";
    g.drawImage(burn.base, 0, 0);
    burn.key = key;
    burn.drawnSeg = 0;
    burn.partial = null;
    burn.partialFrac = 0;
    dirty = true;
  }
  g.lineJoin = "round";
  g.lineWidth = Math.max(2, 0.5 * burn.pxPerMm);
  const target = Math.min(completed, segs.length);
  if (target > burn.drawnSeg) {
    g.lineCap = "round";
    let style = "";
    let open = false;
    for (let i = burn.drawnSeg; i < target; i++) {
      const s = segs[i];
      if (!s.laserOn) continue;
      const st = burnStroke(burn, s.powerRatio);
      if (st !== style) {
        if (open) g.stroke();
        g.strokeStyle = st;
        g.beginPath();
        style = st;
        open = true;
      }
      const start = i === burn.drawnSeg && burn.partial ? burn.partial : s.from;
      g.moveTo(mapX(start.x), mapY(start.y));
      g.lineTo(mapX(s.to.x), mapY(s.to.y));
    }
    if (open) g.stroke();
    burn.drawnSeg = target;
    burn.partial = null;
    burn.partialFrac = 0;
    dirty = true;
  }
  const active = segs[burn.drawnSeg];
  if (active?.laserOn && activeFraction > 0 && activeFraction > burn.partialFrac) {
    const cur = {
      x: active.from.x + (active.to.x - active.from.x) * activeFraction,
      y: active.from.y + (active.to.y - active.from.y) * activeFraction,
    };
    const start = burn.partial || active.from;
    // 同一段分多次画时用平头端点，避免接缝处叠色出现深点
    g.lineCap = burn.partial ? "butt" : "round";
    g.strokeStyle = burnStroke(burn, active.powerRatio);
    g.beginPath();
    g.moveTo(mapX(start.x), mapY(start.y));
    g.lineTo(mapX(cur.x), mapY(cur.y));
    g.stroke();
    burn.partial = cur;
    burn.partialFrac = activeFraction;
    dirty = true;
  }
  if (dirty) burn.texture.needsUpdate = true;
}

/** 径向渐变光斑贴图（激光落点辉光）。 */
export function makeGlowTexture() {
  const size = 128;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const g2d = canvas.getContext("2d");
  if (!g2d) return null;
  const grad = g2d.createRadialGradient(size / 2, size / 2, 0, size / 2, size / 2, size / 2);
  grad.addColorStop(0, "rgba(255,255,255,1)");
  grad.addColorStop(0.18, "rgba(214,204,255,0.85)");
  grad.addColorStop(0.45, "rgba(124,92,255,0.32)");
  grad.addColorStop(1, "rgba(124,92,255,0)");
  g2d.fillStyle = grad;
  g2d.fillRect(0, 0, size, size);
  const tex = new THREE.CanvasTexture(canvas);
  tex.colorSpace = THREE.SRGBColorSpace;
  return tex;
}

/**
 * 全应用只建一个孪生渲染器（WebGL 上下文），离开页面时摘下画布留着下次复用（2026-11-08）。
 * 实测在 macOS WebKit 上每新建、丢弃一次 three.js 渲染器，图形进程就留下约 100 MB 不还，
 * 即使 dispose + forceContextLoss；复用同一个上下文后反复进出不再增长。上下文被系统回收时才重建。
 */
let sharedTwinRenderer = null;
export function acquireTwinRenderer() {
  if (sharedTwinRenderer && !sharedTwinRenderer.getContext().isContextLost()) return sharedTwinRenderer;
  sharedTwinRenderer?.dispose();
  sharedTwinRenderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, powerPreference: "high-performance" });
  return sharedTwinRenderer;
}

/** 高分屏最多按 1.5 倍渲染：比 2 倍少约 44% 像素，配合抗锯齿肉眼几乎看不出差别（2026-11-08）。 */
export const MAX_PIXEL_RATIO = 1.5;

/** 把 2D 画布尺寸清零，立即交还位图内存，不等垃圾回收（2026-11-08）。 */
export function releaseCanvas(canvas) {
  if (!canvas || typeof canvas.width !== "number") return;
  canvas.width = 0;
  canvas.height = 0;
}

export function donePowerToRgb(powerRatio) {
  const t = Math.min(1, Math.max(0, Number(powerRatio) || 0));
  if (t < 0.5) {
    const k = t * 2;
    return [
      (0xfb + (0xf9 - 0xfb) * k) / 255,
      (0xbf + (0x73 - 0xbf) * k) / 255,
      (0x24 + (0x16 - 0x24) * k) / 255,
    ];
  }
  const k = (t - 0.5) * 2;
  return [
    (0xf9 + (0xff - 0xf9) * k) / 255,
    (0x73 + (0x3b - 0x73) * k) / 255,
    (0x16 + (0x5c - 0x16) * k) / 255,
  ];
}

export function materialColorHex(name = "") {
  const s = String(name);
  if (s.includes("亚克力") || s.toLowerCase().includes("acrylic")) return 0x2563eb;
  if (s.includes("金属") || s.includes("钢") || s.includes("铝")) return 0x64748b;
  if (s.includes("皮") || s.includes("纸")) return 0xa16207;
  return 0xb47b44; // 椴木板温润木色
}

export function addMeshWithEdges(group, geometry, material, edgeColor = 0x94a3b8, edgeOpacity = 0.55) {
  const mesh = new THREE.Mesh(geometry, material);
  mesh.castShadow = !material.transparent;
  mesh.receiveShadow = true;
  // 实体件只留极淡的棱线，靠光照与阴影表现体积；透明亚克力保留彩色轮廓便于辨认
  const solid = !material.transparent;
  const edges = new THREE.LineSegments(
    new THREE.EdgesGeometry(geometry, 35),
    new THREE.LineBasicMaterial({
      color: solid ? 0xffffff : edgeColor,
      transparent: true,
      opacity: solid ? Math.min(0.12, edgeOpacity * 0.16) : edgeOpacity * 0.6,
      depthWrite: false,
    }),
  );
  mesh.add(edges);
  group.add(mesh);
  return mesh;
}

/**
 * 只依赖板材起点 / 雕刻起点的对象（板材、原点坐标轴、零点环）按位置摆放，
 * 改偏置时不重建整台机台（2026-11-05，避免拖动滑块时每次重建上百个网格）。
 */
export function placeOffsetObjects(ctx, { offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, matWidth, matHeight, matThick }) {
  if (!ctx) return;
  ctx.slabMesh?.position.set(offsetX + matWidth / 2, matThick / 2, -(offsetY + matHeight / 2));
  ctx.originMarker?.position.set(toolpathOffsetX, 0, -toolpathOffsetY);
  if (ctx.homeRing) ctx.homeRing.visible = toolpathOffsetX > 0.01 || toolpathOffsetY > 0.01;
}

export function clampCoord(v, min, max) {
  const n = Number(v);
  if (!Number.isFinite(n)) return 0;
  return Math.min(max, Math.max(min, n));
}
