import React, { useEffect, useRef, useState, useCallback } from "react";
import { clearAndDispose, disposeObjectResources } from "../threeDispose.js";
import * as THREE from "three";
import { RoundedBoxGeometry } from "three/examples/jsm/geometries/RoundedBoxGeometry.js";
import { RoomEnvironment } from "three/examples/jsm/environments/RoomEnvironment.js";
import { Route, RotateCcw, Crosshair, Sun, Box, EyeOff, Eye } from "lucide-react";

// 视角预设（2026-11-07 视口改版）：默认“整机”斜 45° 看全机；切换时相机平滑过渡
const CAMERA_PRESETS = {
  iso: { label: "整机", title: "整机 3D 视角", theta: -Math.PI * 0.2, phi: Math.PI * 0.32, distScale: 3.5 },
  fit: { label: "刀路", title: "聚焦刀路与板材", theta: Math.PI * 0.12, phi: Math.PI * 0.2, distScale: 2.3 },
  top: { label: "俯视", title: "俯视 XY", theta: 0, phi: 0.015, distScale: 2.7 },
  front: { label: "前视", title: "前视 XZ", theta: 0, phi: Math.PI * 0.44, distScale: 2.6 },
  side: { label: "侧视", title: "侧视 YZ", theta: -Math.PI * 0.5, phi: Math.PI * 0.4, distScale: 2.6 },
};
const DEFAULT_PRESET = "iso";

/** 圆角盒体：打印件、电机与板材统一用小圆角，靠光照表现体积，避免“线框积木”观感。 */
function rbox(w, h, d, r = 1.2) {
  const radius = Math.max(0.01, Math.min(r, Math.min(w, h, d) * 0.45));
  return new RoundedBoxGeometry(w, h, d, 3, radius);
}

/** 材料烧灼颜色：木材 / 皮革 / 纸为深褐碳化，亚克力为雾白，金属为浅灰白。 */
function burnStyleFor(name = "") {
  const s = String(name);
  if (s.includes("亚克力") || s.toLowerCase().includes("acrylic")) return { rgb: "236, 240, 255", base: 0.35, gain: 0.45 };
  if (s.includes("金属") || s.includes("钢") || s.includes("铝")) return { rgb: "226, 232, 240", base: 0.4, gain: 0.5 };
  return { rgb: "46, 24, 10", base: 0.5, gain: 0.45 };
}

/** 生成板材表面底纹：木材画年轮纹理，其他材料为均匀底色加轻微噪点。 */
function paintMaterialBase(g2d, width, height, name, colorHex) {
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
const GHOST_PRESETS = new Set(["fit", "top"]);
const GHOST_FADE = 0.86;

/** level 0 = 实体，1 = 半透明；透明开关变化时才重新编译材质。 */
function applyGhost(ctx, level) {
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

function applyCoverVisibility(ctx, visible) {
  ctx?.enclosureGroup?.children.forEach((child) => {
    if (child.userData?.cover) child.visible = Boolean(visible);
  });
}

function burnStroke(burn, powerRatio) {
  const p = Math.min(1, Math.max(0, Number(powerRatio) || 0.7));
  const alpha = Math.round((burn.style.base + burn.style.gain * p) * 20) / 20;
  return `rgba(${burn.style.rgb}, ${alpha})`;
}

/**
 * 把已完成的出光段增量画到板面画布上（2026-11-07）。
 * 只画新增部分；进度倒退、换刀路或改偏置时先铺回底纹再从头画。当前段只画到激光头所在位置。
 */
function updateBurnSurface(burn, { segs, completed, activeFraction, key, mapX, mapY }) {
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
function makeGlowTexture() {
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
function acquireTwinRenderer() {
  if (sharedTwinRenderer && !sharedTwinRenderer.getContext().isContextLost()) return sharedTwinRenderer;
  sharedTwinRenderer?.dispose();
  sharedTwinRenderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, powerPreference: "high-performance" });
  return sharedTwinRenderer;
}

/** 高分屏最多按 1.5 倍渲染：比 2 倍少约 44% 像素，配合抗锯齿肉眼几乎看不出差别（2026-11-08）。 */
const MAX_PIXEL_RATIO = 1.5;

/** 把 2D 画布尺寸清零，立即交还位图内存，不等垃圾回收（2026-11-08）。 */
function releaseCanvas(canvas) {
  if (!canvas || typeof canvas.width !== "number") return;
  canvas.width = 0;
  canvas.height = 0;
}

function donePowerToRgb(powerRatio) {
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

function materialColorHex(name = "") {
  const s = String(name);
  if (s.includes("亚克力") || s.toLowerCase().includes("acrylic")) return 0x2563eb;
  if (s.includes("金属") || s.includes("钢") || s.includes("铝")) return 0x64748b;
  if (s.includes("皮") || s.includes("纸")) return 0xa16207;
  return 0xb47b44; // 椴木板温润木色
}

function addMeshWithEdges(group, geometry, material, edgeColor = 0x94a3b8, edgeOpacity = 0.55) {
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
function placeOffsetObjects(ctx, { offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, matWidth, matHeight, matThick }) {
  if (!ctx) return;
  ctx.slabMesh?.position.set(offsetX + matWidth / 2, matThick / 2, -(offsetY + matHeight / 2));
  ctx.originMarker?.position.set(toolpathOffsetX, 0, -toolpathOffsetY);
  if (ctx.homeRing) ctx.homeRing.visible = toolpathOffsetX > 0.01 || toolpathOffsetY > 0.01;
}

/**
 * 大鱼 DIY 翼宿 V1.0 (Yisu V1.0) 真实三维数字孪生视口
 * 严格依据《翼宿 V1.0 图纸包》全套 STEP 打印件与 DXF 亚克力图纸建模：
 * - 有效可调雕刻工作区：默认 100 × 100 mm（原点 (0,0) 位于工作区左前角）
 * - 底板.STEP (193×222×6mm) + 切割垫板.STEP (121.9×121.9×4.5mm)
 * - 侧框_左/右.STEP + 背板.STEP + 顶盖框架.STEP + 主板盖.STEP + 4028 排烟风扇
 * - 顶部观察板_厚度3mm.DXF (186.8×213mm) + 前遮光板_厚度3mm.DXF (158×107mm) 半透防激光亚克力
 * - 3 根 140mm MGN9H 线性导轨 + 2 台 42 步进电机 (40mm) + Ø8×186mm 联动光轴 + 前置惰轮调节块与手拧螺帽
 * - 激光头打印件总成：X轴连接件.STEP + 激光头转接板_5W.STEP + 同步带卡齿.STEP + 线束固定器.STEP + 24V 5W 激光器
 */
export function TwinStage3D({
  workspace,
  toolpath,
  pose,
  showRapid = true,
  followHead = false,
  onToggleRapid,
  onToggleFollow,
}) {
  const stageWrapRef = useRef(null);
  const mountRef = useRef(null);
  const sceneRef = useRef(null);
  const [webglError, setWebglError] = useState("");
  const [activePreset, setActivePreset] = useState(DEFAULT_PRESET);
  const [brightMode, setBrightMode] = useState(true);
  const [showEnclosure, setShowEnclosure] = useState(false);
  const [hudCollapsed, setHudCollapsed] = useState(() => {
    try {
      const saved = window.localStorage.getItem("moss_yisu_hud_collapsed_v1");
      if (saved === "1" || saved === "true") return true;
      if (saved === "0" || saved === "false") return false;
    } catch {
      // ignore storage errors
    }
    return false;
  });
  const [stageHeight, setStageHeight] = useState(() => {
    try {
      const saved = Number(window.localStorage.getItem("moss_yisu_stage_height_v1"));
      if (Number.isFinite(saved) && saved >= 220 && saved <= 960) {
        return Math.round(saved);
      }
    } catch {
      // ignore storage errors
    }
    return 520;
  });
  const [resizingStage, setResizingStage] = useState(false);

  const syncStageViewport = useCallback((explicitHeight) => {
    const ctx = sceneRef.current;
    const container = mountRef.current;
    if (!ctx || !container) return;
    if (Number.isFinite(explicitHeight) && stageWrapRef.current) {
      stageWrapRef.current.style.height = `${explicitHeight}px`;
    }
    const w = Math.max(280, container.clientWidth || 640);
    const h = Math.max(180, Number.isFinite(explicitHeight) ? explicitHeight : container.clientHeight || 420);
    ctx.camera.aspect = w / h;
    // 往上拉缩小时对垂直视场角做平滑补偿，防止高度变小导致 3D 机台被线性压扁缩得过小
    ctx.camera.fov = h < 520 ? Math.min(62, 38 * Math.pow(520 / h, 0.55)) : 38;
    ctx.camera.updateProjectionMatrix();
    ctx.renderer.setSize(w, h, false);
    ctx.renderer.render(ctx.scene, ctx.camera);
  }, []);

  const updateStageHeight = useCallback((nextH) => {
    const clamped = Math.min(960, Math.max(220, Math.round(Number(nextH) || 520)));
    setStageHeight(clamped);
    syncStageViewport(clamped);
    try {
      window.localStorage.setItem("moss_yisu_stage_height_v1", String(clamped));
    } catch {
      // ignore storage errors
    }
  }, [syncStageViewport]);

  const handleStageResizeStart = useCallback((e) => {
    e.preventDefault();
    e.stopPropagation();
    const startY = e.clientY;
    const startH = stageHeight;
    setResizingStage(true);

    const onMove = (moveEvent) => {
      const dy = moveEvent.clientY - startY;
      updateStageHeight(startH + dy);
    };
    const onUp = () => {
      setResizingStage(false);
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onUp);
  }, [stageHeight, updateStageHeight]);

  const toggleHudCollapsed = useCallback(() => {
    setHudCollapsed((prev) => {
      const next = !prev;
      try {
        window.localStorage.setItem("moss_yisu_hud_collapsed_v1", next ? "1" : "0");
      } catch {
        // ignore storage errors
      }
      return next;
    });
  }, []);

  const xMax = workspace?.xTravelMm || 100;
  const yMax = workspace?.yTravelMm || 100;
  const zMax = workspace?.zTravelMm || 20;
  const matWidth = workspace?.material?.widthMm || xMax;
  const matHeight = workspace?.material?.heightMm || yMax;
  const matThick = workspace?.material?.thicknessMm || 3;
  const matName = workspace?.material?.name || "椴木板";
  const offsetX = Number(workspace?.offsetX ?? workspace?.material?.offsetX) || 0;
  const offsetY = Number(workspace?.offsetY ?? workspace?.material?.offsetY) || 0;
  const textOffsetX = Number(workspace?.textOffsetX ?? workspace?.material?.textOffsetX) || 0;
  const textOffsetY = Number(workspace?.textOffsetY ?? workspace?.material?.textOffsetY) || 0;
  const toolpathOffsetX = Number(workspace?.toolpathOffsetX ?? (offsetX + textOffsetX)) || 0;
  const toolpathOffsetY = Number(workspace?.toolpathOffsetY ?? (offsetY + textOffsetY)) || 0;
  const toolpathScaleX = Number(workspace?.toolpathScaleX ?? workspace?.material?.toolpathScaleX) || 1;
  const toolpathScaleY = Number(workspace?.toolpathScaleY ?? workspace?.material?.toolpathScaleY) || 1;

  // 计算刀路/工作区的视觉中心与最佳观察跨度（聚焦整块材料板材，方便观察文字在材料内的移动）
  const getFocusBox = useCallback(() => {
    const b = toolpath?.bounds;
    if (b && (b.widthMm > 1 || b.heightMm > 1)) {
      const cx = offsetX + matWidth * 0.5;
      const cy = offsetY + matHeight * 0.5;
      const span = Math.max(b.widthMm, b.heightMm, matWidth, matHeight, 80);
      return { cx, cy, span };
    }
    return {
      cx: xMax * 0.5,
      cy: yMax * 0.5,
      span: Math.max(xMax, yMax, 100),
    };
  }, [toolpath, matWidth, matHeight, xMax, yMax, offsetX, offsetY]);
  // 相机轨道参数 ref
  const orbitRef = useRef({
    theta: CAMERA_PRESETS[DEFAULT_PRESET].theta,
    phi: CAMERA_PRESETS[DEFAULT_PRESET].phi,
    distance: 340,
    target: new THREE.Vector3(50, 28, -50),
    // 视角切换的目标；动画循环每帧向目标插值，拖拽时清空
    goal: null,
    dragging: false,
    panMode: false,
    lastX: 0,
    lastY: 0,
  });

  const applyCameraOrbit = useCallback(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;
    const { camera } = ctx;
    const orb = orbitRef.current;
    const sinPhi = Math.sin(orb.phi);
    const cosPhi = Math.cos(orb.phi);
    const sinTheta = Math.sin(orb.theta);
    const cosTheta = Math.cos(orb.theta);

    camera.position.set(
      orb.target.x + orb.distance * sinPhi * sinTheta,
      orb.target.y + orb.distance * cosPhi,
      orb.target.z + orb.distance * sinPhi * cosTheta,
    );
    camera.lookAt(orb.target);
    ctx.requestRender?.();
  }, []);

  const selectPreset = useCallback((presetKey, { instant = false } = {}) => {
    const key = CAMERA_PRESETS[presetKey] ? presetKey : DEFAULT_PRESET;
    const preset = CAMERA_PRESETS[key];
    setActivePreset(key);
    let distance;
    let target;
    if (key === "fit") {
      const { cx, cy, span } = getFocusBox();
      distance = Math.max(110, span * preset.distScale);
      target = new THREE.Vector3(cx, matThick + 4, -cy);
    } else {
      const span = Math.max(xMax, yMax, 110);
      distance = span * preset.distScale;
      target = new THREE.Vector3(xMax * 0.5, key === "top" ? 0 : 28, -yMax * 0.5);
    }
    const orb = orbitRef.current;
    // 最短路径转到目标方位角，避免绕远圈
    let theta = preset.theta;
    while (theta - orb.theta > Math.PI) theta -= Math.PI * 2;
    while (theta - orb.theta < -Math.PI) theta += Math.PI * 2;
    if (instant || !sceneRef.current) {
      orb.goal = null;
      orb.theta = theta;
      orb.phi = preset.phi;
      orb.distance = distance;
      orb.target.copy(target);
      applyCameraOrbit();
      return;
    }
    orb.goal = { theta, phi: preset.phi, distance, target };
    sceneRef.current?.requestRender?.();
  }, [xMax, yMax, matThick, getFocusBox, applyCameraOrbit]);

  // 初始化 Three.js 场景与渲染循环
  useEffect(() => {
    const container = mountRef.current;
    if (!container) return undefined;

    let renderer;
    try {
      renderer = acquireTwinRenderer();
    } catch (err) {
      setWebglError(err?.message || "当前环境不支持 WebGL 3D 渲染");
      return undefined;
    }

    const width = Math.max(320, container.clientWidth || 640);
    const height = Math.max(200, container.clientHeight || 420);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, MAX_PIXEL_RATIO));
    renderer.setSize(width, height, false);
    // 透明清屏，背景交给 CSS 的影棚渐变；ACES 色调映射 + 软阴影
    renderer.setClearColor(0x000000, 0);
    renderer.toneMapping = THREE.ACESFilmicToneMapping;
    renderer.toneMappingExposure = 1.05;
    renderer.shadowMap.enabled = true;
    renderer.shadowMap.type = THREE.PCFSoftShadowMap;

    container.innerHTML = "";
    container.appendChild(renderer.domElement);

    const scene = new THREE.Scene();
    const initialFov = height < 520 ? Math.min(62, 38 * Math.pow(520 / height, 0.55)) : 38;
    const camera = new THREE.PerspectiveCamera(initialFov, width / height, 1, 5000);

    // 室内反射环境：让导轨、光轴、电机端盖等金属件有真实高光
    const pmrem = new THREE.PMREMGenerator(renderer);
    const roomEnvironment = new RoomEnvironment();
    const envTexture = pmrem.fromScene(roomEnvironment, 0.04).texture;
    // 只用来生成反射贴图，生成后立即释放它自己的几何体与材质
    roomEnvironment.dispose();
    scene.environment = envTexture;
    scene.environmentIntensity = 0.55;

    // 三点布光：半球环境光 + 投影主光 + 背后冷色轮廓光
    const hemiLight = new THREE.HemisphereLight(0xe8eefc, 0x1a1f2b, 0.75);
    hemiLight.position.set(0, 400, 0);
    scene.add(hemiLight);

    const keyLight = new THREE.DirectionalLight(0xfff4e6, 2.2);
    keyLight.position.set(170, 320, 200);
    keyLight.castShadow = true;
    keyLight.shadow.mapSize.set(2048, 2048);
    keyLight.shadow.camera.left = -190;
    keyLight.shadow.camera.right = 190;
    keyLight.shadow.camera.top = 190;
    keyLight.shadow.camera.bottom = -190;
    keyLight.shadow.camera.near = 40;
    keyLight.shadow.camera.far = 900;
    keyLight.shadow.bias = -0.0004;
    keyLight.shadow.normalBias = 0.6;
    keyLight.shadow.radius = 4;
    scene.add(keyLight);
    scene.add(keyLight.target);

    const rimLight = new THREE.DirectionalLight(0x9db4ff, 1.1);
    rimLight.position.set(-200, 200, -260);
    scene.add(rimLight);

    // 承托地面：只接收阴影的透明平面 + 柔和的圆形影棚地台
    const floorGroup = new THREE.Group();
    const shadowFloor = new THREE.Mesh(
      new THREE.PlaneGeometry(1400, 1400).rotateX(-Math.PI / 2),
      new THREE.ShadowMaterial({ color: 0x000000, opacity: 0.38 }),
    );
    shadowFloor.receiveShadow = true;
    floorGroup.add(shadowFloor);
    const floorDiscCanvas = document.createElement("canvas");
    floorDiscCanvas.width = 256;
    floorDiscCanvas.height = 256;
    const discCtx = floorDiscCanvas.getContext("2d");
    if (discCtx) {
      const grad = discCtx.createRadialGradient(128, 128, 0, 128, 128, 128);
      grad.addColorStop(0, "rgba(148,163,200,0.16)");
      grad.addColorStop(0.6, "rgba(120,130,170,0.07)");
      grad.addColorStop(1, "rgba(120,130,170,0)");
      discCtx.fillStyle = grad;
      discCtx.fillRect(0, 0, 256, 256);
    }
    const floorDiscTex = new THREE.CanvasTexture(floorDiscCanvas);
    floorDiscTex.colorSpace = THREE.SRGBColorSpace;
    const floorDisc = new THREE.Mesh(
      new THREE.PlaneGeometry(620, 620).rotateX(-Math.PI / 2),
      new THREE.MeshBasicMaterial({ map: floorDiscTex, transparent: true, depthWrite: false }),
    );
    floorDisc.position.y = 0.05;
    floorDisc.renderOrder = -1;
    floorGroup.add(floorDisc);
    floorGroup.position.y = -6.1;
    scene.add(floorGroup);

    // 场景分组
    const frameGroup = new THREE.Group();
    const enclosureGroup = new THREE.Group();
    const workpieceGroup = new THREE.Group();
    const toolpathGroup = new THREE.Group();
    const cursorGroup = new THREE.Group();
    const gantryGroup = new THREE.Group();
    const headGroup = new THREE.Group();
    gantryGroup.add(headGroup);

    scene.add(frameGroup);
    scene.add(enclosureGroup);
    scene.add(workpieceGroup);
    scene.add(toolpathGroup);
    scene.add(cursorGroup);
    scene.add(gantryGroup);

    // 激光束与落点：出光时为细亮的 450nm 蓝紫光束 + 加色辉光；不出光时只在板面留一个小定位环
    const beamCoreGeo = new THREE.CylinderGeometry(0.16, 0.08, 1, 12);
    const beamCoreMat = new THREE.MeshBasicMaterial({
      color: 0xe4dcff,
      transparent: true,
      opacity: 0.95,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
    });
    const beamCoreMesh = new THREE.Mesh(beamCoreGeo, beamCoreMat);

    const beamHaloGeo = new THREE.CylinderGeometry(0.75, 0.32, 1, 16, 1, true);
    const beamHaloMat = new THREE.MeshBasicMaterial({
      color: 0x7c5cff,
      transparent: true,
      opacity: 0.35,
      blending: THREE.AdditiveBlending,
      depthWrite: false,
      side: THREE.DoubleSide,
    });
    const beamHaloMesh = new THREE.Mesh(beamHaloGeo, beamHaloMat);

    // 不出光时的定位环
    const spotRingGeo = new THREE.RingGeometry(1.6, 2.2, 40);
    spotRingGeo.rotateX(-Math.PI / 2);
    const spotRingMat = new THREE.MeshBasicMaterial({
      color: 0x7dd3fc,
      side: THREE.DoubleSide,
      transparent: true,
      opacity: 0.85,
      depthWrite: false,
    });
    const spotRingMesh = new THREE.Mesh(spotRingGeo, spotRingMat);

    const spotCenterGeo = new THREE.CircleGeometry(0.45, 20);
    spotCenterGeo.rotateX(-Math.PI / 2);
    const spotCenterMat = new THREE.MeshBasicMaterial({
      color: 0xffffff,
      side: THREE.DoubleSide,
      transparent: true,
      depthWrite: false,
    });
    const spotCenterMesh = new THREE.Mesh(spotCenterGeo, spotCenterMat);

    // 出光落点辉光（始终朝向相机的加色贴图）
    const glowTexture = makeGlowTexture();
    const spotGlow = new THREE.Sprite(
      new THREE.SpriteMaterial({
        map: glowTexture,
        color: 0xffffff,
        transparent: true,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      }),
    );
    spotGlow.visible = false;

    const spotPointLight = new THREE.PointLight(0x8b7bff, 0, 70, 1.6);

    scene.add(beamCoreMesh);
    scene.add(beamHaloMesh);
    scene.add(spotRingMesh);
    scene.add(spotCenterMesh);
    scene.add(spotGlow);
    scene.add(spotPointLight);

    sceneRef.current = {
      renderer,
      scene,
      camera,
      hemiLight,
      keyLight,
      rimLight,
      frameGroup,
      enclosureGroup,
      workpieceGroup,
      toolpathGroup,
      cursorGroup,
      gantryGroup,
      headGroup,
      beamCoreMesh,
      beamHaloMesh,
      spotRingMesh,
      spotCenterMesh,
      spotGlow,
      spotPointLight,
      laserOn: false,
      powerRatio: 0,
      burn: null,
      cutVertexPrefix: null,
      ghostMats: null,
      ghostLevel: 0,
      ghostTarget: 0,
      doneCutLine: null,
      todoCutLine: null,
      rapidLine: null,
      arrowLine: null,
      cometLine: null,
      cursorXLine: null,
      cursorYLine: null,
      headLedMat: null,
      fanHubMesh: null,
      animId: 0,
    };

    // 按需渲染（2026-11-08）：场景有变化（推演进度、设备位置、拖动视角、窗口尺寸）时排一帧；只有视角过渡、
    // 护罩淡入淡出进行中才连续出帧，推演暂停或播完后不再重画（此前 60 fps 常驻）。
    // 帧内 animId 非零，期间的 requestRender 不会重复排帧。
    let pulsePhase = 0;
    const renderFrame = () => {
      const ctx = sceneRef.current;
      if (!ctx) return;
      let animating = false;
      pulsePhase += 0.12;
      // 视角切换平滑过渡
      const orb = orbitRef.current;
      if (orb.goal && !orb.dragging) {
        const k = 0.14;
        orb.theta += (orb.goal.theta - orb.theta) * k;
        orb.phi += (orb.goal.phi - orb.phi) * k;
        orb.distance += (orb.goal.distance - orb.distance) * k;
        orb.target.lerp(orb.goal.target, k);
        if (
          Math.abs(orb.goal.theta - orb.theta) < 1e-3 &&
          Math.abs(orb.goal.phi - orb.phi) < 1e-3 &&
          Math.abs(orb.goal.distance - orb.distance) < 0.05 &&
          orb.target.distanceTo(orb.goal.target) < 0.05
        ) {
          orb.theta = orb.goal.theta;
          orb.phi = orb.goal.phi;
          orb.distance = orb.goal.distance;
          orb.target.copy(orb.goal.target);
          orb.goal = null;
        }
        applyCameraOrbit();
        animating = Boolean(orb.goal);
      }
      if (ctx.laserOn) {
        // 出光时落点辉光轻微闪烁、散热风扇转动：随推演 / 实时位置更新逐帧推进，暂停时保持静止
        const flicker = 1 + Math.sin(pulsePhase * 2.7) * 0.08 + Math.sin(pulsePhase * 6.1) * 0.05;
        const size = (5 + ctx.powerRatio * 6) * flicker;
        ctx.spotGlow.scale.set(size, size, 1);
        if (ctx.fanHubMesh) ctx.fanHubMesh.rotation.z += 0.18;
      }
      if (ctx.ghostMats && ctx.ghostLevel !== ctx.ghostTarget) {
        ctx.ghostLevel += (ctx.ghostTarget - ctx.ghostLevel) * 0.15;
        if (Math.abs(ctx.ghostTarget - ctx.ghostLevel) < 0.01) ctx.ghostLevel = ctx.ghostTarget;
        applyGhost(ctx, ctx.ghostLevel);
        animating = animating || ctx.ghostLevel !== ctx.ghostTarget;
      }
      ctx.renderer.render(ctx.scene, ctx.camera);
      ctx.animId = animating ? requestAnimationFrame(renderFrame) : 0;
    };
    sceneRef.current.requestRender = () => {
      const ctx = sceneRef.current;
      if (ctx && !ctx.animId) ctx.animId = requestAnimationFrame(renderFrame);
    };
    renderFrame();

    const handleResize = () => {
      syncStageViewport();
    };
    const resizeObserver = typeof ResizeObserver !== "undefined" ? new ResizeObserver(handleResize) : null;
    resizeObserver?.observe(container);
    window.addEventListener("resize", handleResize);

    return () => {
      window.removeEventListener("resize", handleResize);
      resizeObserver?.disconnect();
      if (sceneRef.current?.animId) {
        cancelAnimationFrame(sceneRef.current.animId);
      }
      // 卸载时释放全部几何体 / 材质 / 贴图与阴影贴图；渲染器（WebGL 上下文）摘下画布留给下次复用
      const ctx = sceneRef.current;
      disposeObjectResources(scene);
      // 阴影贴图（2048² 渲染目标）不在材质上，要单独释放
      scene.traverse((object) => object.shadow?.dispose?.());
      envTexture.dispose();
      pmrem.dispose();
      releaseCanvas(ctx?.burn?.canvas);
      releaseCanvas(ctx?.burn?.base);
      releaseCanvas(floorDiscCanvas);
      releaseCanvas(glowTexture?.image);
      renderer.renderLists.dispose();
      renderer.domElement?.remove?.();
      sceneRef.current = null;
    };
  }, []);

  // 切换高对比工程亮色背景 / 深空底色
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;
    // 背景渐变由 CSS 类切换；这里只调灯光与曝光
    ctx.hemiLight.intensity = brightMode ? 0.75 : 0.4;
    ctx.keyLight.intensity = brightMode ? 2.2 : 1.5;
    ctx.rimLight.intensity = brightMode ? 1.1 : 1.4;
    ctx.scene.environmentIntensity = brightMode ? 0.55 : 0.3;
    ctx.renderer.toneMappingExposure = brightMode ? 1.05 : 0.85;
  }, [brightMode]);

  // 切换半透亚克力护罩（顶盖、前遮光板、上侧框、铰链与卡扣）显隐；背板、风扇、主板盖等实体件始终显示。
  // 默认隐藏护罩（2026-11-07）：橙色亚克力会把整台机器染成褐色并挡住板面
  const ghostTargetRef = useRef(GHOST_PRESETS.has(activePreset) ? 1 : 0);
  useEffect(() => {
    ghostTargetRef.current = GHOST_PRESETS.has(activePreset) ? 1 : 0;
    if (sceneRef.current) sceneRef.current.ghostTarget = ghostTargetRef.current;
  }, [activePreset]);

  const showEnclosureRef = useRef(showEnclosure);
  useEffect(() => {
    showEnclosureRef.current = showEnclosure;
    applyCoverVisibility(sceneRef.current, showEnclosure);
  }, [showEnclosure]);

  // 当切换/上传新刀路时，若处于 fit 视角则自动对焦到刀路区域（调整材料坐标时保持机台视角稳定，直观看到材料与激光头在台面上移动）
  const cameraPlacedRef = useRef(false);
  useEffect(() => {
    if (!cameraPlacedRef.current) {
      // 首次挂载直接摆到默认视角，不做过渡
      cameraPlacedRef.current = Boolean(sceneRef.current);
      selectPreset(activePreset, { instant: true });
    } else if (activePreset === "fit") {
      selectPreset("fit");
    }
  }, [toolpath]);

  // 按《B站【大鱼DIY】激光雕刻机_翼宿 V1.0 图纸包》构建真实机台、导轨、电机、外壳与激光头打印件总成
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;
    const { frameGroup, enclosureGroup, workpieceGroup, cursorGroup, gantryGroup, headGroup } = ctx;

    // 重建前先释放旧网格的 GPU 资源（group.clear() 本身不释放显存）
    clearAndDispose(frameGroup);
    clearAndDispose(enclosureGroup);
    clearAndDispose(workpieceGroup);
    clearAndDispose(cursorGroup);
    clearAndDispose(headGroup);
    // 板材尺寸变了会新建烧灼画布：旧画布（最长边约 1500 px ×2 张）立即释放
    releaseCanvas(ctx.burn?.canvas);
    releaseCanvas(ctx.burn?.base);
    ctx.burn = null;
    clearAndDispose(gantryGroup);
    gantryGroup.add(headGroup);
    ctx.slabMesh = null;
    ctx.originMarker = null;
    ctx.homeRing = null;

    const cx = xMax / 2;
    const cz = -yMax / 2;

    // 真实翼宿 V1.0 尺寸参数（中心对称包裹 100×100mm 工作区）
    const chassisW = Math.max(193, xMax + 93); // 底板.STEP: 193mm (±96.5mm)
    const chassisD = Math.max(222, yMax + 122); // 底板.STEP: 222mm (-112.5~+109.5mm)
    const chassisH = 107; // 侧框_左/右.STEP: 107mm
    const cuttingMatW = Math.max(121.9, xMax + 21.9); // 切割垫板.STEP: 121.9mm (±60.95mm)
    const cuttingMatD = Math.max(121.9, yMax + 21.9);
    const railLen = Math.max(140, yMax + 40); // BOM: 140mm MGN9H 导轨 × 3
    const yRailOffsetX = Math.max(66, xMax / 2 + 16); // 左右 Y 轴 MGN9H 导轨距中心距离

    // 材质库：还原 PETG 打印件、MGN9H 银白钢轨、MGN9H 滑块、42 步进电机、5W 蓝光激光器、半透橙红亚克力
    // 2026-11-07 视口改版：石墨灰打印件 + 银色钢件 + 黑色阳极氧化激光头，只在旋钮、线夹上点缀 MOSS 紫
    const petgDarkMat = new THREE.MeshStandardMaterial({
      color: 0x2a2f38,
      roughness: 0.7,
      metalness: 0.0,
    });
    const petgAccentMat = new THREE.MeshStandardMaterial({
      color: 0x353b46,
      roughness: 0.66,
      metalness: 0.0,
    });
    const petgPrintMat = new THREE.MeshStandardMaterial({
      color: 0x41495a,
      roughness: 0.6,
      metalness: 0.0,
    });
    const accentMat = new THREE.MeshStandardMaterial({
      color: 0x8b7bff,
      roughness: 0.45,
      metalness: 0.1,
    });
    const cuttingPadMat = new THREE.MeshStandardMaterial({
      color: 0x1c2028,
      roughness: 0.55,
      metalness: 0.6,
    });
    const mgnRailMat = new THREE.MeshStandardMaterial({
      color: 0xdfe4ec,
      roughness: 0.24,
      metalness: 0.92,
    });
    const mgnBlockMat = new THREE.MeshStandardMaterial({
      color: 0xb4bcc8,
      roughness: 0.32,
      metalness: 0.88,
    });
    const motorBodyMat = new THREE.MeshStandardMaterial({
      color: 0x16191f,
      roughness: 0.48,
      metalness: 0.35,
    });
    const motorCapMat = new THREE.MeshStandardMaterial({
      color: 0xb6bec9,
      roughness: 0.28,
      metalness: 0.9,
    });
    const beltMat = new THREE.MeshStandardMaterial({
      color: 0x0d0f13,
      roughness: 0.9,
      metalness: 0.0,
    });
    const brassMat = new THREE.MeshStandardMaterial({
      color: 0xd9a441,
      roughness: 0.3,
      metalness: 0.95,
    });
    const acrylicOrangeMat = new THREE.MeshStandardMaterial({
      color: 0xff6a1a,
      roughness: 0.08,
      metalness: 0.0,
      transparent: true,
      opacity: 0.13,
      depthWrite: false,
      side: THREE.DoubleSide,
    });

    // =========================================================================
    // 1. 底板.STEP (193 × 6 × 222 mm) 与 切割垫板.STEP (121.9 × 4.5 × 121.9 mm)
    // =========================================================================
    const baseFrameThickness = 6;
    const sideSkirtW = (chassisW - cuttingMatW) / 2;
    const frontBackSkirtD = (chassisD - cuttingMatD) / 2;

    // 底板四周框架（还原 底板.STEP 中央镂空嵌装切割垫板结构）
    const baseLeft = addMeshWithEdges(
      frameGroup,
      rbox(sideSkirtW, baseFrameThickness, chassisD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseLeft.position.set(cx - cuttingMatW / 2 - sideSkirtW / 2, -3, cz);

    const baseRight = addMeshWithEdges(
      frameGroup,
      rbox(sideSkirtW, baseFrameThickness, chassisD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseRight.position.set(cx + cuttingMatW / 2 + sideSkirtW / 2, -3, cz);

    const baseFront = addMeshWithEdges(
      frameGroup,
      rbox(cuttingMatW, baseFrameThickness, frontBackSkirtD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseFront.position.set(cx, -3, cz + cuttingMatD / 2 + frontBackSkirtD / 2);

    const baseBack = addMeshWithEdges(
      frameGroup,
      rbox(cuttingMatW, baseFrameThickness, frontBackSkirtD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseBack.position.set(cx, -3, cz - cuttingMatD / 2 - frontBackSkirtD / 2);

    // 切割垫板.STEP (121.9 × 4.5 × 121.9 mm)：位于工作区正中央
    const padMesh = addMeshWithEdges(
      frameGroup,
      rbox(cuttingMatW, 4.5, cuttingMatD),
      cuttingPadMat,
      0x64748b,
      0.7,
    );
    padMesh.position.set(cx, -2.3, cz);

    // 切割垫板条形防反光排烟格栅槽 (还原 切割垫板.STEP 表面平行凹槽)
    for (let sx = -cuttingMatW / 2 + 8; sx <= cuttingMatW / 2 - 8; sx += 10) {
      const slat = new THREE.Mesh(
        rbox(2.2, 0.3, cuttingMatD - 14),
        new THREE.MeshStandardMaterial({ color: 0x0b0d11, roughness: 0.9, metalness: 0.2 }),
      );
      slat.position.set(cx + sx, 0.01, cz);
      frameGroup.add(slat);
    }

    // 2. 100×100mm 可调有效雕刻工作区网格 (10mm 细网格 + 50mm 亮色主刻度线)
    const gridPos = [];
    const gridColors = [];
    const step = 10;
    for (let gx = 0; gx <= xMax; gx += step) {
      const isMajor = gx % 50 === 0 || gx === xMax;
      const c = isMajor ? [0.42, 0.47, 0.58] : [0.2, 0.23, 0.29];
      gridPos.push(gx, 0.1, 0, gx, 0.1, -yMax);
      gridColors.push(...c, ...c);
    }
    for (let gy = 0; gy <= yMax; gy += step) {
      const isMajor = gy % 50 === 0 || gy === yMax;
      const c = isMajor ? [0.42, 0.47, 0.58] : [0.2, 0.23, 0.29];
      gridPos.push(0, 0.1, -gy, xMax, 0.1, -gy);
      gridColors.push(...c, ...c);
    }
    const gridGeo = new THREE.BufferGeometry();
    gridGeo.setAttribute("position", new THREE.Float32BufferAttribute(gridPos, 3));
    gridGeo.setAttribute("color", new THREE.Float32BufferAttribute(gridColors, 3));
    frameGroup.add(
      new THREE.LineSegments(
        gridGeo,
        new THREE.LineBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.7, depthWrite: false }),
      ),
    );
    // 有效工作区边界：MOSS 紫细框，一眼看出能刻到哪里
    const workAreaGeo = new THREE.BufferGeometry().setFromPoints([
      new THREE.Vector3(0, 0.14, 0),
      new THREE.Vector3(xMax, 0.14, 0),
      new THREE.Vector3(xMax, 0.14, -yMax),
      new THREE.Vector3(0, 0.14, -yMax),
      new THREE.Vector3(0, 0.14, 0),
    ]);
    frameGroup.add(
      new THREE.Line(
        workAreaGeo,
        new THREE.LineBasicMaterial({ color: 0x8b7bff, transparent: true, opacity: 0.75, depthWrite: false }),
      ),
    );

    // 3. 机器零点 (0,0) 基准环与跟随雕刻起点 (toolpathOffsetX, toolpathOffsetY) 移动的醒目坐标轴
    // 零点环只在雕刻起点偏离 (0,0) 时显示；显隐由 placeOffsetObjects 控制，改偏置不重建
    const homeRing = new THREE.Mesh(
      new THREE.RingGeometry(1.2, 2.4, 24).rotateX(-Math.PI / 2),
      new THREE.MeshBasicMaterial({ color: 0x64748b, side: THREE.DoubleSide }),
    );
    homeRing.position.set(0, 0.25, 0);
    frameGroup.add(homeRing);
    ctx.homeRing = homeRing;

    const originSurfaceY = matThick + 0.22;
    const axisLen = Math.min(22, Math.max(10, Math.min(matWidth, matHeight) * 0.35));
    const axisGeo = new THREE.BufferGeometry();
    axisGeo.setAttribute(
      "position",
      new THREE.Float32BufferAttribute([
        0, originSurfaceY, 0, axisLen, originSurfaceY, 0,
        0, originSurfaceY, 0, 0, originSurfaceY, -axisLen,
        0, originSurfaceY, 0, 0, originSurfaceY + 14, 0,
      ], 3),
    );
    axisGeo.setAttribute(
      "color",
      new THREE.Float32BufferAttribute([
        1.0, 0.35, 0.45, 1.0, 0.35, 0.45,
        0.2, 0.95, 0.65, 0.2, 0.95, 0.65,
        0.35, 0.78, 1.0, 0.35, 0.78, 1.0,
      ], 3),
    );
    // 坐标轴与原点环建在局部原点，整组随雕刻起点 (toolpathOffsetX, toolpathOffsetY) 平移
    const originMarker = new THREE.Group();
    originMarker.add(new THREE.LineSegments(axisGeo, new THREE.LineBasicMaterial({ vertexColors: true })));

    const originRing = new THREE.Mesh(
      new THREE.RingGeometry(0.9, 1.5, 28).rotateX(-Math.PI / 2),
      new THREE.MeshBasicMaterial({ color: 0xe2e8f0, side: THREE.DoubleSide, transparent: true, opacity: 0.85 }),
    );
    originRing.position.set(0, originSurfaceY + 0.03, 0);
    originMarker.add(originRing);
    workpieceGroup.add(originMarker);
    ctx.originMarker = originMarker;
    // =========================================================================
    // 4. Y 轴线性导轨与双 42 步进电机传动总成 (BOM: 2×140mm MGN9H + 2×42步进 + Ø8×186mm联动轴)
    // =========================================================================
    [-1, 1].forEach((side) => {
      const rx = cx + side * yRailOffsetX;

      // PETG 侧向导轨承托台
      const railBed = addMeshWithEdges(
        frameGroup,
        rbox(16, 18, railLen + 36),
        petgAccentMat,
        0x64748b,
        0.55,
      );
      railBed.position.set(rx, 9, cz);

      // 140mm MGN9H 线性导轨 (宽 9mm × 高 6.5mm × 长 140mm)
      const yRail = addMeshWithEdges(
        frameGroup,
        rbox(9, 6.5, railLen),
        mgnRailMat,
        0x475569,
        0.8,
      );
      yRail.position.set(rx, 21.25, cz);

      // Y 轴 GT2 6mm 同步带
      const yBelt = new THREE.Mesh(
        rbox(2.2, 6, railLen + 24),
        beltMat,
      );
      yBelt.position.set(rx - side * 6.5, 25, cz - 4);
      frameGroup.add(yBelt);

      // 前置 惰轮调节块_Y左/Y右.STEP + 手拧螺帽.STEP (PETG 打印件)
      const tensionBlock = addMeshWithEdges(
        frameGroup,
        rbox(15, 16, 22),
        petgPrintMat,
        0x38bdf8,
        0.7,
      );
      tensionBlock.position.set(rx, 22, cz + railLen / 2 + 12);

      // 星形滚花 手拧螺帽.STEP (直径 15mm, 厚 8mm)
      const thumbNut = addMeshWithEdges(
        frameGroup,
        new THREE.CylinderGeometry(7.5, 7.5, 8, 12).rotateX(Math.PI / 2),
        accentMat,
        0xe0f2fe,
        0.85,
      );
      thumbNut.position.set(rx, 22, cz + railLen / 2 + 27);

      // 后置 42 步进电机 (42×42×40mm) + 步进电机调节块.STEP
      const motorX = rx - side * 6;
      const motorZ = cz - railLen / 2 - 18;

      const motorMount = addMeshWithEdges(
        frameGroup,
        rbox(32, 32, 8),
        petgPrintMat,
        0x64748b,
        0.65,
      );
      motorMount.position.set(motorX, 18, motorZ + 14);

      const motorBody = addMeshWithEdges(
        frameGroup,
        rbox(42, 40, 42),
        motorBodyMat,
        0x64748b,
        0.7,
      );
      motorBody.position.set(motorX, 20, motorZ - 10);

      const motorTopCap = addMeshWithEdges(
        frameGroup,
        rbox(42.4, 7, 42.4),
        motorCapMat,
        0x94a3b8,
        0.6,
      );
      motorTopCap.position.set(motorX, 37, motorZ - 10);

      // 20齿 GT2 同步轮 (5mm/8mm 内孔, 高 16mm)
      const pulley = addMeshWithEdges(
        frameGroup,
        new THREE.CylinderGeometry(6.5, 6.5, 16, 20),
        motorCapMat,
        0x475569,
        0.7,
      );
      pulley.position.set(rx - side * 6.5, 25, motorZ + 6);
    });

    // 后方 Ø8 × 186mm 联动不锈钢光轴 + 688ZZ 轴承限位块.STEP
    const syncShaft = addMeshWithEdges(
      frameGroup,
      new THREE.CylinderGeometry(4, 4, 186, 20).rotateZ(Math.PI / 2),
      mgnRailMat,
      0x64748b,
      0.6,
    );
    syncShaft.position.set(cx, 25, cz - railLen / 2 - 12);

    // =========================================================================
    // 5. 翼宿 V1.0 外壳、背板、4028 风扇、G3 主板盖与 3mm 半透亚克力护罩
    // =========================================================================
    // 背板.STEP (193 × 62 × 6 mm)
    const backWallZ = cz - chassisD / 2 + 3;
    const backPlate = addMeshWithEdges(
      enclosureGroup,
      rbox(chassisW, 62, 6),
      petgDarkMat,
      0x475569,
      0.6,
    );
    backPlate.position.set(cx, 31, backWallZ);

    // 后置 4028 排烟风扇 (40×40×28 mm, 24V 10000RPM)
    const fanGroup = new THREE.Group();
    fanGroup.position.set(cx + 42, 30, backWallZ - 17);
    addMeshWithEdges(
      fanGroup,
      rbox(40, 40, 28),
      motorBodyMat,
      0x64748b,
      0.75,
    );
    const fanDuct = new THREE.Mesh(
      new THREE.CylinderGeometry(18, 18, 29, 24).rotateX(Math.PI / 2),
      new THREE.MeshStandardMaterial({ color: 0x0f172a, roughness: 0.7 }),
    );
    fanGroup.add(fanDuct);
    const fanHub = addMeshWithEdges(
      fanGroup,
      new THREE.CylinderGeometry(8, 8, 30, 16).rotateX(Math.PI / 2),
      new THREE.MeshStandardMaterial({ color: 0x3a3f4a, roughness: 0.5 }),
      0xbae6fd,
      0.8,
    );
    ctx.fanHubMesh = fanHub;
    enclosureGroup.add(fanGroup);

    // 主板盖.STEP (G3 主板防护盒，带六边形散热格栅视觉，72 × 54 × 18 mm)
    const boardCover = addMeshWithEdges(
      enclosureGroup,
      rbox(72, 54, 18),
      petgPrintMat,
      0x38bdf8,
      0.65,
    );
    boardCover.position.set(cx - 34, 29, backWallZ - 12);

    // 左右侧框_左/右.STEP（箱式半透线框展示，既还原 107mm 高侧框，又不遮挡侧向视线）
    const sideWallMat = new THREE.MeshStandardMaterial({
      color: 0x1e293b,
      roughness: 0.5,
      metalness: 0.15,
      transparent: true,
      opacity: 0.32,
      depthWrite: false,
    });
    [-1, 1].forEach((side) => {
      const sx = cx + side * (chassisW / 2 - 4);
      // 下沿实心裙边
      const lowerSide = addMeshWithEdges(
        enclosureGroup,
        rbox(8, 24, chassisD),
        petgDarkMat,
        0x475569,
        0.65,
      );
      lowerSide.position.set(sx, 12, cz);

      // 上沿側框立柱与顶梁 (还原 侧框_左/右.STEP 视窗框造型)
      const upperSide = addMeshWithEdges(
        enclosureGroup,
        rbox(6, chassisH - 24, chassisD),
        sideWallMat,
        0x64748b,
        0.45,
      );
      upperSide.position.set(sx, 24 + (chassisH - 24) / 2, cz);
      upperSide.userData.cover = true;
    });

    // 顶盖框架.STEP + 顶部观察板_厚度3mm.DXF (186.8 × 213 × 3 mm 半透橙红防激光亚克力)
    const topAcrylic = addMeshWithEdges(
      enclosureGroup,
      rbox(186.8, 3, 213),
      acrylicOrangeMat,
      0xf97316,
      0.55,
    );
    topAcrylic.position.set(cx, chassisH - 1.5, cz);
    topAcrylic.userData.cover = true;

    // 前遮光板_厚度3mm.DXF (158 × 107 × 3 mm 半透橙红防激光亚克力)
    const frontAcrylic = addMeshWithEdges(
      enclosureGroup,
      rbox(158, 86, 3),
      acrylicOrangeMat,
      0xf97316,
      0.5,
    );
    frontAcrylic.position.set(cx, 20 + 43, cz + chassisD / 2 - 2);
    frontAcrylic.userData.cover = true;

    // 后部左右阻尼铰链 (BOM: 小号阻尼合页 1.5kgf.cm 左/右) 与前部 顶板固定器.STEP
    [-68, 68].forEach((hx) => {
      const hinge = addMeshWithEdges(
        enclosureGroup,
        rbox(14, 10, 12),
        petgPrintMat,
        0x94a3b8,
        0.7,
      );
      hinge.position.set(cx + hx, chassisH - 2, backWallZ + 4);
      hinge.userData.cover = true;

      const frontClip = addMeshWithEdges(
        enclosureGroup,
        rbox(28, 6, 10),
        petgPrintMat,
        0x94a3b8,
        0.65,
      );
      frontClip.position.set(cx + hx, chassisH - 1, cz + chassisD / 2 - 4);
      frontClip.userData.cover = true;
    });

    // =========================================================================
    // 6. 加工板材 (Workpiece Slab) —— 按用户设置的尺寸 (matWidth × matHeight) 与起点坐标 (offsetX, offsetY) 摆放
    // =========================================================================
    const slabGeo = rbox(matWidth, matThick, matHeight, 0.3);
    const slabColor = new THREE.Color(materialColorHex(matName)).multiplyScalar(0.82);
    const slabMat = new THREE.MeshStandardMaterial({
      color: slabColor,
      roughness: 0.78,
      metalness: 0.02,
    });
    const slabMesh = addMeshWithEdges(workpieceGroup, slabGeo, slabMat, 0xfbbf24, 0.78);
    ctx.slabMesh = slabMesh;

    // 板材上表面：材料底纹 + 逐段累积的烧灼痕迹（画在同一张画布上，雕刻时能看到逐渐刻出来的样子）
    const pxPerMm = Math.max(4, Math.min(12, 1536 / Math.max(matWidth, matHeight)));
    const burnCanvas = document.createElement("canvas");
    burnCanvas.width = Math.max(16, Math.round(matWidth * pxPerMm));
    burnCanvas.height = Math.max(16, Math.round(matHeight * pxPerMm));
    const burnCtx = burnCanvas.getContext("2d");
    const baseCanvas = document.createElement("canvas");
    baseCanvas.width = burnCanvas.width;
    baseCanvas.height = burnCanvas.height;
    const baseCtx = baseCanvas.getContext("2d");
    if (burnCtx && baseCtx) {
      paintMaterialBase(baseCtx, baseCanvas.width, baseCanvas.height, matName, materialColorHex(matName));
      burnCtx.drawImage(baseCanvas, 0, 0);
      const burnTexture = new THREE.CanvasTexture(burnCanvas);
      burnTexture.colorSpace = THREE.SRGBColorSpace;
      burnTexture.anisotropy = Math.min(8, ctx.renderer.capabilities.getMaxAnisotropy?.() || 1);
      const surface = new THREE.Mesh(
        new THREE.PlaneGeometry(matWidth, matHeight).rotateX(-Math.PI / 2),
        new THREE.MeshStandardMaterial({ map: burnTexture, roughness: 0.82, metalness: 0.0 }),
      );
      surface.position.y = matThick / 2 + 0.02;
      surface.receiveShadow = true;
      slabMesh.add(surface);
      ctx.burn = {
        canvas: burnCanvas,
        g2d: burnCtx,
        base: baseCanvas,
        texture: burnTexture,
        pxPerMm,
        style: burnStyleFor(matName),
        key: "",
        drawnSeg: 0,
        partial: null,
      };
    } else {
      ctx.burn = null;
    }
    // 7. X/Y 动态十字准星投影线（贴在板材上表面，实时标示激光头 X/Y 坐标位置）
    const cursorSurfaceY = matThick + 0.18;
    const xLineGeo = new THREE.BufferGeometry().setFromPoints([
      new THREE.Vector3(0, cursorSurfaceY, 0),
      new THREE.Vector3(xMax, cursorSurfaceY, 0),
    ]);
    const yLineGeo = new THREE.BufferGeometry().setFromPoints([
      new THREE.Vector3(0, cursorSurfaceY, 0),
      new THREE.Vector3(0, cursorSurfaceY, -yMax),
    ]);
    const cursorMat = new THREE.LineBasicMaterial({
      color: 0x7dd3fc,
      transparent: true,
      opacity: 0.32,
      depthWrite: false,
    });
    const cursorXLine = new THREE.Line(xLineGeo, cursorMat);
    const cursorYLine = new THREE.Line(yLineGeo, cursorMat);
    cursorGroup.add(cursorXLine);
    cursorGroup.add(cursorYLine);
    ctx.cursorXLine = cursorXLine;
    ctx.cursorYLine = cursorYLine;

    // =========================================================================
    // 8. X 轴龙门架总成 (XY转接块.STEP + 联动支架.STEP + 140mm MGN9H 导轨)
    // =========================================================================
    const gantryBaseY = 26; // Y 轴 MGN9H 滑块顶面高度
    const xRailSpan = yRailOffsetX * 2;
    const xRailLen = Math.max(140, xMax + 40);

    // 左右 Y 轴 MGN9H 滑块 + XY转接块.STEP + 联动支架.STEP 肩部惰轮座
    [-1, 1].forEach((side) => {
      const gx = cx + side * yRailOffsetX;

      // Y 轴 MGN9H 滑块 (28.9 × 8 × 20 mm)
      const yCarriageBlock = addMeshWithEdges(
        gantryGroup,
        rbox(20, 7, 29),
        mgnBlockMat,
        0x6ee7b7,
        0.85,
      );
      yCarriageBlock.position.set(gx, 24.5, 0);

      // XY转接块.STEP (PETG 打印件，36 × 15.2 × 21 mm，带阶梯与斜角)
      const xyAdapter = addMeshWithEdges(
        gantryGroup,
        rbox(22, 15.2, 35),
        petgPrintMat,
        0x38bdf8,
        0.8,
      );
      xyAdapter.position.set(gx - side * 2, gantryBaseY + 7.6, 0);

      // XY 转接块上的 16T/20T 金属惰轮 × 2 (还原 CoreXY/H-bot 联动过轮)
      [-8, 8].forEach((pz) => {
        const idler = addMeshWithEdges(
          gantryGroup,
          new THREE.CylinderGeometry(5.5, 5.5, 9, 18),
          motorCapMat,
          0x94a3b8,
          0.75,
        );
        idler.position.set(gx - side * 4, gantryBaseY + 17, pz);
      });
    });

    // 联动支架.STEP 主横梁 (PETG 打印件，连接左右 XY 转接块，承载 X 轴 140mm MGN9H 导轨)
    const linkBracket = addMeshWithEdges(
      gantryGroup,
      rbox(xRailSpan - 8, 14, 15),
      petgPrintMat,
      0x38bdf8,
      0.75,
    );
    linkBracket.position.set(cx, gantryBaseY + 11, -3);

    // X 轴 140mm MGN9H 线性导轨 (安装在联动支架正面，宽 140mm × 高 9mm × 厚 6.5mm)
    const xMgnRail = addMeshWithEdges(
      gantryGroup,
      rbox(xRailLen, 9, 6.5),
      mgnRailMat,
      0x475569,
      0.85,
    );
    xMgnRail.position.set(cx, gantryBaseY + 11, 7.5);

    // X 轴横向 GT2 6mm 同步带
    const xBelt = new THREE.Mesh(
      rbox(xRailLen, 6, 1.8),
      beltMat,
    );
    xBelt.position.set(cx, gantryBaseY + 19.5, 4);
    gantryGroup.add(xBelt);

    // =========================================================================
    // 9. 翼宿 V1.0 真实激光头与打印件总成
    // (X轴连接件.STEP + 激光头转接板_5W.STEP + 同步带卡齿 + 线束固定器.STEP + 24V 5W 激光器)
    // =========================================================================
    const headMountY = gantryBaseY + 11; // X 轴导轨中心高度 (37mm)

    // (a) X 轴 MGN9H 线性滑块 (28.9 × 20 × 8 mm)
    const xCarriageBlock = addMeshWithEdges(
      headGroup,
      rbox(28.9, 20, 8),
      mgnBlockMat,
      0x6ee7b7,
      0.9,
    );
    xCarriageBlock.position.set(0, headMountY, 9.5);

    // (b) X轴连接件.STEP + 同步带卡齿_固定/可调.STEP (PETG 打印件，抱合滑块并咬合后方同步带)
    const xConnector = addMeshWithEdges(
      headGroup,
      rbox(33, 24, 12),
      petgPrintMat,
      0x60a5fa,
      0.85,
    );
    xConnector.position.set(0, headMountY + 2, 14.5);

    // 同步带卡齿块 (顶部后伸卡入 GT2 皮带)
    const beltClamp = addMeshWithEdges(
      headGroup,
      rbox(22, 9, 14),
      petgAccentMat,
      0x38bdf8,
      0.8,
    );
    beltClamp.position.set(0, headMountY + 11, 8);

    // (c) 激光头转接板_5W.STEP (PETG 打印件：35.6 × 38 × 6 mm 背板 + 左右竖向调焦滑槽耳 + 抱箍侧翼)
    const adapterPlate = addMeshWithEdges(
      headGroup,
      rbox(35.6, 38, 5.5),
      petgPrintMat,
      0x38bdf8,
      0.92,
    );
    adapterPlate.position.set(0, headMountY + 1, 22);

    // 转接板左右竖向调焦长圆滑槽 + 4 颗 M3 圆头内六角固定螺丝 (还原 激光头转接板_5W.STEP 标志特征)
    [-13.5, 13.5].forEach((sx) => {
      // 竖向调焦滑槽深色凹槽
      const slotGroove = new THREE.Mesh(
        rbox(3.4, 24, 1.2),
        new THREE.MeshBasicMaterial({ color: 0x0f172a }),
      );
      slotGroove.position.set(sx, headMountY + 1, 24.6);
      headGroup.add(slotGroove);

      // 上下两颗 M3 不锈钢锁紧螺丝 + 垫片
      [-7, 9].forEach((sy) => {
        const m3Screw = new THREE.Mesh(
          new THREE.CylinderGeometry(2.6, 2.6, 2.5, 14).rotateX(Math.PI / 2),
          mgnRailMat,
        );
        m3Screw.position.set(sx, headMountY + sy, 25.5);
        headGroup.add(m3Screw);
      });

      // 转接板前伸抱夹侧翼 (夹持 5W 激光器两侧)
      const sideClampEar = addMeshWithEdges(
        headGroup,
        rbox(3.5, 28, 18),
        petgPrintMat,
        0x38bdf8,
        0.85,
      );
      sideClampEar.position.set(sx > 0 ? 16.5 : -16.5, headMountY - 1, 31);
    });

    // (d) 线束固定器.STEP + 线束固定压块.STEP (位于转接板右上方，夹持激光头 24V/TTL 线束)
    const wireHolder = addMeshWithEdges(
      headGroup,
      rbox(11, 16, 10),
      petgPrintMat,
      0x38bdf8,
      0.85,
    );
    wireHolder.position.set(12, headMountY + 24, 20);

    const wireClamp = addMeshWithEdges(
      headGroup,
      rbox(13, 5, 11),
      accentMat,
      0xbae6fd,
      0.9,
    );
    wireClamp.position.set(12, headMountY + 29, 20);

    // (e) 24V 5W 半导体激光头模块 (30 × 42 × 30 mm 阳极氧化散热主体 + 多片散热鳍片 + 顶部风扇 + 黄铜调焦嘴 + 遮光环)
    const laserCenterZ = 0; // 让激光束严格对齐刀路坐标 (px, -py)
    // 将整个 headGroup 内部件沿 Z 轴平移，使激光嘴中心恰好在 headGroup 局部坐标 (0, y, 0)
    const headZShift = -39.5;
    xCarriageBlock.position.z += headZShift;
    xConnector.position.z += headZShift;
    beltClamp.position.z += headZShift;
    adapterPlate.position.z += headZShift;
    wireHolder.position.z += headZShift;
    wireClamp.position.z += headZShift;
    headGroup.children.forEach((child) => {
      if (
        child !== xCarriageBlock &&
        child !== xConnector &&
        child !== beltClamp &&
        child !== adapterPlate &&
        child !== wireHolder &&
        child !== wireClamp
      ) {
        child.position.z += headZShift;
      }
    });

    // 同步调整龙门架 X 轴导轨与转接块 Z 偏置，使其与激光头滑块严丝合缝咬合
    gantryGroup.children.forEach((child) => {
      if (child !== headGroup) {
        child.position.z += headZShift;
      }
    });

    // 5W 激光器黑色阳极氧化铝主体 (30 × 40 × 30 mm)：方芯 + 水平叠片散热鳍片
    const laserHeatsinkMat = new THREE.MeshStandardMaterial({
      color: 0x1a1d23,
      metalness: 0.65,
      roughness: 0.36,
    });
    const laserBody = addMeshWithEdges(
      headGroup,
      rbox(22, 38, 22, 1.5),
      laserHeatsinkMat,
      0x60a5fa,
      0.95,
    );
    laserBody.position.set(0, headMountY + 1, laserCenterZ);

    const finMat = new THREE.MeshStandardMaterial({ color: 0x23272f, metalness: 0.7, roughness: 0.32 });
    for (let fy = -16; fy <= 16; fy += 3.2) {
      const fin = new THREE.Mesh(rbox(29.5, 1.3, 29.5, 0.5), finMat);
      fin.position.set(0, headMountY + 1 + fy, laserCenterZ);
      headGroup.add(fin);
    }

    // 激光器顶部 3010 轴流冷却风扇 + 24V/TTL 3P 白色接线端子座
    const topFanCap = addMeshWithEdges(
      headGroup,
      rbox(30, 8, 30),
      motorBodyMat,
      0x94a3b8,
      0.85,
    );
    topFanCap.position.set(0, headMountY + 24, laserCenterZ);

    const xhConnector = addMeshWithEdges(
      headGroup,
      rbox(10, 5, 7),
      new THREE.MeshStandardMaterial({ color: 0xf8fafc, roughness: 0.3 }),
      0x94a3b8,
      0.8,
    );
    xhConnector.position.set(6, headMountY + 29.5, laserCenterZ - 6);

    // 顶部运行状态 LED 指示灯
    const ledMat = new THREE.MeshBasicMaterial({ color: 0x34d399 });
    const headLed = new THREE.Mesh(new THREE.SphereGeometry(1.3, 14, 14), ledMat);
    headLed.position.set(-7, headMountY + 29.5, laserCenterZ + 6);
    headGroup.add(headLed);

    // 底部黄铜调焦镜筒 + 锥形出光嘴
    const brassBarrel = addMeshWithEdges(
      headGroup,
      new THREE.CylinderGeometry(6.5, 6.5, 7, 20),
      brassMat,
      0xfde68a,
      0.85,
    );
    brassBarrel.position.set(0, headMountY - 21.5, laserCenterZ);

    const nozzleTip = addMeshWithEdges(
      headGroup,
      new THREE.ConeGeometry(4.2, 6, 20),
      brassMat,
      0xfde68a,
      0.9,
    );
    nozzleTip.rotation.x = Math.PI;
    nozzleTip.position.set(0, headMountY - 27, laserCenterZ);

    // 底部橙红半透亚克力防激光护目环
    const shieldSkirt = addMeshWithEdges(
      headGroup,
      new THREE.CylinderGeometry(11, 11, 7, 24, 1, true),
      new THREE.MeshStandardMaterial({
        color: 0xf97316,
        transparent: true,
        opacity: 0.45,
        side: THREE.DoubleSide,
      }),
      0xfb923c,
      0.8,
    );
    shieldSkirt.position.set(0, headMountY - 24, laserCenterZ);

    ctx.headLedMat = ledMat;
    ctx.nozzleTipY = headMountY - 30;

    // 实体件投射与接收阴影（透明亚克力、发光件不投影）
    [frameGroup, enclosureGroup, workpieceGroup, gantryGroup].forEach((group) => {
      group.traverse((obj) => {
        if (!obj.isMesh) return;
        const transparent = Boolean(obj.material?.transparent) || obj.material?.isMeshBasicMaterial;
        obj.castShadow = !transparent;
        obj.receiveShadow = !obj.material?.isMeshBasicMaterial;
      });
    });
    applyCoverVisibility(ctx, showEnclosureRef.current);

    // 龙门与激光头改用独立材质副本（原材质与机架共用），以便单独调透明度。
    // 副本在本帧首次渲染前替换，原材质尚未上传 GPU，无需释放。
    const ghostClones = new Map();
    const ghostMats = [];
    const ghostMeshes = [];
    gantryGroup.traverse((obj) => {
      if (!obj.material || Array.isArray(obj.material)) return;
      if (obj.isMesh) ghostMeshes.push({ mesh: obj, castShadow: obj.castShadow });
      let clone = ghostClones.get(obj.material);
      if (!clone) {
        clone = obj.material.clone();
        ghostClones.set(obj.material, clone);
        ghostMats.push({ mat: clone, opacity: clone.opacity, transparent: clone.transparent, depthWrite: clone.depthWrite });
      }
      obj.material = clone;
    });
    ctx.headLedMat = ghostClones.get(ledMat) || ledMat;
    ctx.ghostMats = ghostMats;
    ctx.ghostMeshes = ghostMeshes;
    ctx.ghostTarget = ghostTargetRef.current;
    ctx.ghostLevel = ctx.ghostTarget;
    applyGhost(ctx, ctx.ghostLevel);

    // 主光与阴影相机对准机台中心
    ctx.keyLight.target.position.set(cx, 0, cz);
    ctx.keyLight.position.set(cx + 170, 320, cz + 200);
    ctx.rimLight.position.set(cx - 200, 200, cz - 260);

    placeOffsetObjects(ctx, { offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, matWidth, matHeight, matThick });
    // 偏置不在依赖里：只改板材 / 雕刻起点时由下一个 effect 平移对象，不重建机台
  }, [xMax, yMax, zMax, matWidth, matHeight, matThick, matName]);

  // 板材起点与雕刻起点变化：只平移相关对象（2026-11-05）
  useEffect(() => {
    placeOffsetObjects(sceneRef.current, { offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, matWidth, matHeight, matThick });
  }, [offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, matWidth, matHeight, matThick]);
  // 构建三维刀路轨迹（2026-11-07 视口改版）：待雕刻为半透青色细线，已雕刻由板面烧灼痕迹表现
  // （无烧灼画布时回退为按功率着色的线），G0 空移为紫色虚线 + 方向箭头，当前位置带一段亮色尾迹
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;
    const { toolpathGroup } = ctx;
    clearAndDispose(toolpathGroup);
    ctx.doneCutLine = null;
    ctx.todoCutLine = null;
    ctx.rapidLine = null;
    ctx.arrowLine = null;
    ctx.cometLine = null;
    ctx.cutVertexPrefix = null;
    ctx.toolpathVersion = (ctx.toolpathVersion || 0) + 1;

    const segments = toolpath?.segments || [];
    if (segments.length === 0) return;

    const surfaceY = matThick + 0.45;
    const rapidY = matThick + 2.0;

    // 刀路包络虚线框（直观展示雕刻内容在材料上的范围）
    const b = toolpath?.bounds;
    if (b && (b.widthMm > 0.5 || b.heightMm > 0.5)) {
      const by = matThick + 0.2;
      const boxGeo = new THREE.BufferGeometry().setFromPoints([
        new THREE.Vector3(b.minX, by, -b.minY),
        new THREE.Vector3(b.maxX, by, -b.minY),
        new THREE.Vector3(b.maxX, by, -b.minY),
        new THREE.Vector3(b.maxX, by, -b.maxY),
        new THREE.Vector3(b.maxX, by, -b.maxY),
        new THREE.Vector3(b.minX, by, -b.maxY),
        new THREE.Vector3(b.minX, by, -b.maxY),
        new THREE.Vector3(b.minX, by, -b.minY),
      ]);
      const boxLine = new THREE.LineSegments(
        boxGeo,
        new THREE.LineDashedMaterial({
          color: 0xffffff,
          transparent: true,
          opacity: 0.32,
          dashSize: 1.6,
          gapSize: 1.2,
          depthWrite: false,
        }),
      );
      boxLine.computeLineDistances();
      toolpathGroup.add(boxLine);
    }

    const cutPositions = [];
    const doneColors = [];
    const rapidPositions = [];
    const arrowPositions = [];
    // 已雕刻顶点数前缀和：更新进度时 O(1) 得到 drawRange，大文件不再每帧遍历全部线段
    const prefix = new Int32Array(segments.length + 1);

    for (let i = 0; i < segments.length; i++) {
      const seg = segments[i];
      prefix[i + 1] = prefix[i] + (seg.laserOn ? 2 : 0);
      if (seg.laserOn) {
        const y0 = surfaceY + (seg.from.z || 0);
        const y1 = surfaceY + (seg.to.z || 0);
        cutPositions.push(seg.from.x, y0, -seg.from.y, seg.to.x, y1, -seg.to.y);
        const rgb = donePowerToRgb(seg.powerRatio || 0.7);
        doneColors.push(...rgb, ...rgb);
      } else {
        const y0 = rapidY + (seg.from.z || 0);
        const y1 = rapidY + (seg.to.z || 0);
        rapidPositions.push(seg.from.x, y0, -seg.from.y, seg.to.x, y1, -seg.to.y);

        if (seg.lengthMm >= 4) {
          const mx = (seg.from.x + seg.to.x) * 0.5;
          const my = (y0 + y1) * 0.5;
          const mz = -(seg.from.y + seg.to.y) * 0.5;
          const dx = (seg.to.x - seg.from.x) / seg.lengthMm;
          const dz = -(seg.to.y - seg.from.y) / seg.lengthMm;
          const nx = -dz;
          const nz = dx;
          const aLen = Math.min(2.4, seg.lengthMm * 0.22);
          const aWid = aLen * 0.55;
          arrowPositions.push(
            mx, my, mz,
            mx - dx * aLen + nx * aWid, my, mz - dz * aLen + nz * aWid,
            mx, my, mz,
            mx - dx * aLen - nx * aWid, my, mz - dz * aLen - nz * aWid,
          );
        }
      }
    }
    ctx.cutVertexPrefix = prefix;

    if (cutPositions.length > 0) {
      const todoGeo = new THREE.BufferGeometry();
      todoGeo.setAttribute("position", new THREE.Float32BufferAttribute(cutPositions, 3));
      const todoLine = new THREE.LineSegments(
        todoGeo,
        new THREE.LineBasicMaterial({
          color: 0x67e8f9,
          transparent: true,
          opacity: 0.55,
          depthWrite: false,
        }),
      );
      todoLine.position.y = -0.04;
      toolpathGroup.add(todoLine);
      ctx.todoCutLine = todoLine;

      const doneGeo = new THREE.BufferGeometry();
      doneGeo.setAttribute("position", new THREE.Float32BufferAttribute(cutPositions, 3));
      doneGeo.setAttribute("color", new THREE.Float32BufferAttribute(doneColors, 3));
      const doneLine = new THREE.LineSegments(
        doneGeo,
        new THREE.LineBasicMaterial({ vertexColors: true }),
      );
      doneLine.position.y = 0.08;
      doneLine.visible = !ctx.burn;
      doneGeo.setDrawRange(0, 0);
      toolpathGroup.add(doneLine);
      ctx.doneCutLine = doneLine;
    }

    if (rapidPositions.length > 0) {
      const rapidGeo = new THREE.BufferGeometry();
      rapidGeo.setAttribute("position", new THREE.Float32BufferAttribute(rapidPositions, 3));
      const rapidLine = new THREE.LineSegments(
        rapidGeo,
        new THREE.LineDashedMaterial({
          color: 0xa78bfa,
          transparent: true,
          opacity: 0.55,
          dashSize: 1.8,
          gapSize: 1.4,
          depthWrite: false,
        }),
      );
      rapidLine.computeLineDistances();
      rapidLine.visible = showRapid;
      toolpathGroup.add(rapidLine);
      ctx.rapidLine = rapidLine;
    }

    if (arrowPositions.length > 0) {
      const arrowGeo = new THREE.BufferGeometry();
      arrowGeo.setAttribute("position", new THREE.Float32BufferAttribute(arrowPositions, 3));
      const arrowLine = new THREE.LineSegments(
        arrowGeo,
        new THREE.LineBasicMaterial({
          color: 0xc4b5fd,
          transparent: true,
          opacity: 0.7,
          depthWrite: false,
        }),
      );
      arrowLine.visible = showRapid;
      toolpathGroup.add(arrowLine);
      ctx.arrowLine = arrowLine;
    }

    const cometMaxSegments = 24;
    const cometGeo = new THREE.BufferGeometry();
    cometGeo.setAttribute("position", new THREE.Float32BufferAttribute(new Float32Array(cometMaxSegments * 6), 3));
    const cometLine = new THREE.LineSegments(
      cometGeo,
      new THREE.LineBasicMaterial({
        color: 0xe4dcff,
        transparent: true,
        opacity: 0.9,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
      }),
    );
    cometLine.position.y = 0.25;
    toolpathGroup.add(cometLine);
    ctx.cometLine = cometLine;
  }, [toolpath, matThick]);

  // 同步 G0 空移线与方向箭头显隐
  useEffect(() => {
    const ctx = sceneRef.current;
    if (ctx?.rapidLine) ctx.rapidLine.visible = Boolean(showRapid);
    if (ctx?.arrowLine) ctx.arrowLine.visible = Boolean(showRapid);
  }, [showRapid]);

  // 实时更新龙门架、激光头位置、刀路偏置、激光束与落点、十字游标、尾迹与板面烧灼痕迹
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;

    const localX = (Number(pose?.x) || 0) * toolpathScaleX;
    const localY = (Number(pose?.y) || 0) * toolpathScaleY;
    const pz = Number(pose?.z) || 0;
    // 激光头与刀路叠加材料起点 (offsetX, offsetY) 与材料内雕刻位置偏置 (textOffsetX, textOffsetY)
    const px = clampCoord(localX + toolpathOffsetX, -10, xMax + 10);
    const py = clampCoord(localY + toolpathOffsetY, -10, yMax + 10);
    const laserOn = Boolean(pose?.laserOn);
    const powerRatio = Math.min(1, Math.max(0, Number(pose?.powerRatio) || 0));
    ctx.laserOn = laserOn;
    ctx.powerRatio = powerRatio;

    // 3D 刀路轨迹组与雕刻合成起点 (toolpathOffsetX, toolpathOffsetY) 及材料宽高比例同步，始终在材料内部
    if (ctx.toolpathGroup) {
      ctx.toolpathGroup.position.set(toolpathOffsetX, 0, -toolpathOffsetY);
      ctx.toolpathGroup.scale.set(toolpathScaleX, 1, toolpathScaleY);
    }

    // Y 轴龙门沿 MGN9H 导轨前后移动，X 轴激光头打印件总成沿横梁 MGN9H 导轨左右移动
    ctx.gantryGroup.position.set(0, 0, -py);
    ctx.headGroup.position.set(px, pz, 0);

    // 台面 X/Y 十字游标线
    if (ctx.cursorXLine && ctx.cursorYLine) {
      ctx.cursorXLine.position.z = -py;
      ctx.cursorYLine.position.x = px;
      ctx.cursorXLine.material.color.setHex(laserOn ? 0xa78bfa : 0x7dd3fc);
      ctx.cursorXLine.material.opacity = laserOn ? 0.5 : 0.32;
    }
    if (ctx.headLedMat) {
      ctx.headLedMat.color.setHex(laserOn ? 0xa78bfa : 0x34d399);
    }

    // 激光束（喷嘴到板面）与落点
    const surfaceY = matThick + 0.45;
    const tipY = (ctx.nozzleTipY || 12) + pz;
    const beamLen = Math.max(1.5, tipY - surfaceY);
    const midY = surfaceY + beamLen / 2;

    ctx.beamCoreMesh.visible = laserOn;
    ctx.beamHaloMesh.visible = laserOn;
    ctx.spotGlow.visible = laserOn;
    ctx.spotRingMesh.visible = !laserOn;
    ctx.spotCenterMesh.visible = !laserOn;

    ctx.spotRingMesh.position.set(px, surfaceY + 0.15, -py);
    ctx.spotCenterMesh.position.set(px, surfaceY + 0.18, -py);

    if (laserOn) {
      // 24V 5W 450nm 蓝紫激光：功率越高光束越粗越亮
      const beamScale = 0.8 + powerRatio * 0.9;
      ctx.beamCoreMesh.position.set(px, midY, -py);
      ctx.beamCoreMesh.scale.set(beamScale, beamLen, beamScale);
      ctx.beamHaloMesh.material.opacity = 0.22 + powerRatio * 0.3;
      ctx.beamHaloMesh.position.set(px, midY, -py);
      ctx.beamHaloMesh.scale.set(beamScale, beamLen, beamScale);
      ctx.spotGlow.position.set(px, surfaceY + 0.6, -py);
      ctx.spotPointLight.position.set(px, surfaceY + 6, -py);
      ctx.spotPointLight.intensity = 20 + powerRatio * 40;
    } else {
      ctx.spotPointLight.intensity = 0;
    }

    // 已雕刻进度：烧灼画布 + （回退时）已完成线段 drawRange + 尾迹
    if (toolpath?.segments) {
      const segs = toolpath.segments;
      const completedIdx = Math.min(segs.length, pose?.completedSegmentCount ?? 0);
      const activeLimit = Math.min(segs.length, completedIdx + (pose?.activeSegmentFraction > 0 ? 1 : 0));

      if (ctx.doneCutLine) {
        const prefix = ctx.cutVertexPrefix;
        ctx.doneCutLine.geometry.setDrawRange(0, prefix ? prefix[activeLimit] : 0);
        if (ctx.todoCutLine && prefix) {
          // 待雕刻线只画尚未完成的部分，不盖住已烧灼的痕迹
          const doneVerts = prefix[completedIdx];
          ctx.todoCutLine.geometry.setDrawRange(doneVerts, prefix[segs.length] - doneVerts);
        }
        ctx.doneCutLine.visible = !ctx.burn;
      }

      if (ctx.burn) {
        const burn = ctx.burn;
        const relX = toolpathOffsetX - offsetX;
        const relY = toolpathOffsetY - offsetY;
        const key = `${ctx.toolpathVersion || 0}|${relX}|${relY}|${toolpathScaleX}|${toolpathScaleY}`;
        updateBurnSurface(burn, {
          segs,
          completed: completedIdx,
          activeFraction: Number(pose?.activeSegmentFraction) || 0,
          key,
          mapX: (x) => (relX + x * toolpathScaleX) * burn.pxPerMm,
          mapY: (y) => burn.canvas.height - (relY + y * toolpathScaleY) * burn.pxPerMm,
        });
      }

      if (ctx.cometLine) {
        // 尾迹只取激光头身后约 12mm 的出光路径（按长度截取，长线段不会整段发白）
        const posAttr = ctx.cometLine.geometry.getAttribute("position");
        const arr = posAttr.array;
        let remain = laserOn ? 12 : 0;
        let ptr = 0;
        let headPt = { x: Number(pose?.x) || 0, y: Number(pose?.y) || 0 };
        for (let i = activeLimit - 1; i >= 0 && remain > 0 && ptr + 5 < arr.length; i--) {
          const s = segs[i];
          if (!s.laserOn) break;
          const end = i === activeLimit - 1 ? headPt : s.to;
          const dx = end.x - s.from.x;
          const dy = end.y - s.from.y;
          const len = Math.hypot(dx, dy);
          if (len < 1e-6) continue;
          const k = Math.min(1, remain / len);
          const start = { x: end.x - dx * k, y: end.y - dy * k };
          const yy = surfaceY + (s.to.z || 0) + 0.2;
          arr[ptr++] = start.x;
          arr[ptr++] = yy;
          arr[ptr++] = -start.y;
          arr[ptr++] = end.x;
          arr[ptr++] = yy;
          arr[ptr++] = -end.y;
          remain -= len * k;
          headPt = s.from;
        }
        posAttr.needsUpdate = true;
        ctx.cometLine.geometry.setDrawRange(0, ptr / 3);
      }
    }

    if (followHead) {
      orbitRef.current.goal = null;
      orbitRef.current.target.set(px, surfaceY, -py);
      applyCameraOrbit();
    }
  }, [pose, toolpath, xMax, yMax, matWidth, matHeight, matThick, matName, offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, toolpathScaleX, toolpathScaleY, followHead, applyCameraOrbit]);

  // 按需渲染：上面各 effect 改完场景后统一排一帧（必须放在所有改场景的 effect 之后）
  useEffect(() => {
    sceneRef.current?.requestRender?.();
  });
  // 鼠标拖拽旋转 / 平移 / 滚轮缩放
  const handlePointerDown = (e) => {
    const orb = orbitRef.current;
    orb.dragging = true;
    orb.panMode = e.button === 2 || e.shiftKey;
    orb.lastX = e.clientX;
    orb.lastY = e.clientY;
  };

  const handlePointerMove = (e) => {
    const orb = orbitRef.current;
    if (!orb.dragging) return;
    const dx = e.clientX - orb.lastX;
    const dy = e.clientY - orb.lastY;
    orb.lastX = e.clientX;
    orb.lastY = e.clientY;

    if (orb.panMode) {
      const panFactor = orb.distance * 0.0015;
      orb.target.x -= dx * panFactor;
      orb.target.z -= dy * panFactor;
    } else {
      orb.theta -= dx * 0.0065;
      orb.phi = Math.min(Math.PI * 0.48, Math.max(0.03, orb.phi - dy * 0.0065));
    }
    applyCameraOrbit();
  };

  const handlePointerUp = () => {
    orbitRef.current.dragging = false;
  };

  const handleWheel = (e) => {
    e.preventDefault();
    const orb = orbitRef.current;
    const factor = e.deltaY > 0 ? 1.09 : 0.91;
    orb.distance = Math.min(1800, Math.max(30, orb.distance * factor));
    applyCameraOrbit();
  };

  return (
    <div
      ref={stageWrapRef}
      className={`twin-stage-wrap ${brightMode ? "twin-stage-wrap--bright" : ""} ${resizingStage ? "is-resizing" : ""}`}
      style={{ height: `${stageHeight}px` }}
    >
      {/* 视口浮层（2026-11-07 改版）：左上读数卡、右侧竖排视角栏、底部图例与规格；可整体收起 */}
      <div className={`twin-hud-top ${hudCollapsed ? "is-collapsed" : ""}`}>
        {hudCollapsed ? (
          <button
            type="button"
            className="twin-hud-collapse-btn is-collapsed"
            onClick={toggleHudCollapsed}
            title="展开浮层：显示坐标、激光状态与视角控制"
            aria-label="展开浮层"
          >
            <Eye size={13} />
            <span>展开浮层</span>
            <span className={`twin-laser-dot ${pose?.laserOn ? "is-firing" : ""}`} />
          </button>
        ) : (
          <div className="twin-readout" aria-label="激光头实时坐标">
            <div className={`twin-laser-badge ${pose?.laserOn ? "is-firing" : ""}`}>
              <span className="twin-laser-dot" />
              <span>{pose?.laserOn ? `出光 S${Math.round(pose?.power || 0)}` : "未出光 M5"}</span>
            </div>
            <div className="twin-readout-coords">
              <span className="twin-coord-chip">
                <span className="twin-coord-axis">X</span>
                <b>{((pose?.x ?? 0) * toolpathScaleX + toolpathOffsetX).toFixed(2)}</b>
              </span>
              <span className="twin-coord-chip">
                <span className="twin-coord-axis">Y</span>
                <b>{((pose?.y ?? 0) * toolpathScaleY + toolpathOffsetY).toFixed(2)}</b>
              </span>
              <span className="twin-coord-chip">
                <span className="twin-coord-axis">Z</span>
                <b>{(pose?.z ?? 0).toFixed(2)}</b>
              </span>
              <span className="twin-coord-chip twin-feed-chip">
                <span className="twin-coord-axis">F</span>
                <b>{Math.round(pose?.feedRate || 0)}</b>
                <small>mm/min</small>
              </span>
            </div>
          </div>
        )}
      </div>

      {!hudCollapsed && (
        <div className="twin-view-rail" role="toolbar" aria-label="三维视角控制">
          <div className="twin-view-group">
            {Object.entries(CAMERA_PRESETS).map(([key, item]) => (
              <button
                key={key}
                type="button"
                className={`twin-rail-btn ${activePreset === key ? "active" : ""}`}
                onClick={() => selectPreset(key)}
                title={item.title}
                aria-pressed={activePreset === key}
              >
                {item.label}
              </button>
            ))}
          </div>
          <div className="twin-view-group">
            <button
              type="button"
              className={`twin-rail-btn twin-rail-icon ${showEnclosure ? "active" : ""}`}
              onClick={() => setShowEnclosure((v) => !v)}
              title="显示/隐藏亚克力护罩、侧框与后置风扇"
              aria-label="亚克力护罩"
              aria-pressed={showEnclosure}
            >
              <Box size={14} />
              <span>护罩</span>
            </button>
            <button
              type="button"
              className={`twin-rail-btn twin-rail-icon ${followHead ? "active" : ""}`}
              onClick={onToggleFollow}
              title="视角跟随激光头移动"
              aria-label="跟随刀头"
              aria-pressed={followHead}
            >
              <Crosshair size={14} />
              <span>跟随</span>
            </button>
            <button
              type="button"
              className={`twin-rail-btn twin-rail-icon ${showRapid ? "active" : ""}`}
              onClick={onToggleRapid}
              title="显示/隐藏 G0 空移轨迹与方向箭头"
              aria-label="G0 空移"
              aria-pressed={showRapid}
            >
              <Route size={14} />
              <span>空移</span>
            </button>
            <button
              type="button"
              className={`twin-rail-btn twin-rail-icon ${brightMode ? "active" : ""}`}
              onClick={() => setBrightMode((v) => !v)}
              title="切换明亮影棚光 / 暗场光"
              aria-label="提亮"
              aria-pressed={brightMode}
            >
              <Sun size={14} />
              <span>提亮</span>
            </button>
          </div>
          <div className="twin-view-group">
            <button
              type="button"
              className="twin-rail-btn twin-rail-icon"
              onClick={() => selectPreset(DEFAULT_PRESET)}
              title="复位到整机视角"
              aria-label="复位视角"
            >
              <RotateCcw size={14} />
              <span>复位</span>
            </button>
            <button
              type="button"
              className="twin-rail-btn twin-rail-icon"
              onClick={toggleHudCollapsed}
              title="收起浮层：隐藏视口上的文字与按钮，不遮挡 3D 工作台"
              aria-label="收起浮层"
            >
              <EyeOff size={14} />
              <span>收起</span>
            </button>
          </div>
        </div>
      )}

      {/* Three.js WebGL 渲染画布（若环境无 WebGL 则展示 2.5D 工程矢量降级画布） */}
      {webglError ? (
        <div className="twin-fallback-stage" role="region" aria-label="2.5D 数字孪生备用画布">
          <svg viewBox={`-15 -15 ${xMax + 30} ${yMax + 30}`} className="twin-fallback-svg">
            <rect x="0" y="0" width={xMax} height={yMax} fill="#161d2f" stroke="#64748b" strokeWidth="1.5" />
            <rect
              x={offsetX}
              y={yMax - offsetY - matHeight}
              width={matWidth}
              height={matHeight}
              fill="rgba(180,123,68,0.45)"
              stroke="#fbbf24"
              strokeWidth="0.9"
            />
            {(toolpath?.segments || []).map((seg) => (
              <line
                key={seg.index}
                x1={toolpathOffsetX + seg.from.x * toolpathScaleX}
                y1={yMax - (toolpathOffsetY + seg.from.y * toolpathScaleY)}
                x2={toolpathOffsetX + seg.to.x * toolpathScaleX}
                y2={yMax - (toolpathOffsetY + seg.to.y * toolpathScaleY)}
                stroke={seg.laserOn ? "#38bdf8" : "rgba(167,139,250,0.45)"}
                strokeWidth={seg.laserOn ? "1.1" : "0.5"}
              />
            ))}
            <line
              x1="0"
              y1={yMax - (toolpathOffsetY + (pose?.y || 0) * toolpathScaleY)}
              x2={xMax}
              y2={yMax - (toolpathOffsetY + (pose?.y || 0) * toolpathScaleY)}
              stroke="#34d399"
              strokeWidth="0.8"
            />
            <circle
              cx={toolpathOffsetX + (pose?.x || 0) * toolpathScaleX}
              cy={yMax - (toolpathOffsetY + (pose?.y || 0) * toolpathScaleY)}
              r="3.2"
              fill={pose?.laserOn ? "#ff3b5c" : "#34d399"}
            />
          </svg>
          <div className="twin-fallback-note">当前环境未启用 WebGL，已自动切换为翼宿 V1.0 2D 工程矢量孪生视图</div>
        </div>
      ) : (
        <div
          ref={mountRef}
          className="twin-webgl-canvas"
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerLeave={handlePointerUp}
          onWheel={handleWheel}
          onContextMenu={(e) => e.preventDefault()}
        />
      )}

      {/* 底部图例与机台规格（收起浮层时一并隐藏） */}
      {!hudCollapsed && (
        <div className="twin-hud-bottom">
          <div className="twin-legend">
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--todo" />
              待雕刻
            </span>
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--cut" />
              已雕刻
            </span>
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--rapid" />
              空移
            </span>
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--area" />
              工作区
            </span>
          </div>
          <div className="twin-workspace-tag">
            <span>翼宿 V1.0 · {xMax}×{yMax} mm</span>
            <span>
              {matName} {matWidth}×{matHeight}×{matThick} mm
            </span>
            <span className="twin-workspace-hint">左键旋转 · 右键平移 · 滚轮缩放</span>
          </div>
        </div>
      )}

      {/* 3D 工作台底部可上下拖拽调节高度边框手柄 */}
      <div
        className="twin-stage-resize-handle"
        role="separator"
        aria-orientation="horizontal"
        aria-label="上下拖动边框放大或缩小 3D 工作台高度"
        title="按住底部边框上下拖动可放大或缩小 3D 工作台（双击恢复默认高度 520px）"
        onPointerDown={handleStageResizeStart}
        onDoubleClick={() => updateStageHeight(520)}
      >
        <span className="twin-stage-resize-grip" />
      </div>
    </div>
  );
}

function clampCoord(v, min, max) {
  const n = Number(v);
  if (!Number.isFinite(n)) return 0;
  return Math.min(max, Math.max(min, n));
}
