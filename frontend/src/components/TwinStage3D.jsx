import React, { useEffect, useRef, useState, useCallback } from "react";
import * as THREE from "three";
import { Eye, Compass, RotateCcw, Crosshair, Focus, Sun, Box, ChevronUp, ChevronDown } from "lucide-react";

const CAMERA_PRESETS = {
  fit: { label: "聚焦刀路", theta: -Math.PI * 0.18, phi: Math.PI * 0.28, distScale: 1.75 },
  iso: { label: "整机 3D", theta: -Math.PI * 0.22, phi: Math.PI * 0.31, distScale: 2.35 },
  top: { label: "俯视 XY", theta: 0, phi: 0.015, distScale: 1.55 },
  front: { label: "前视 XZ", theta: 0, phi: Math.PI * 0.43, distScale: 1.9 },
  side: { label: "侧视 YZ", theta: -Math.PI * 0.48, phi: Math.PI * 0.38, distScale: 1.95 },
};

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
  const edges = new THREE.LineSegments(
    new THREE.EdgesGeometry(geometry),
    new THREE.LineBasicMaterial({
      color: edgeColor,
      transparent: edgeOpacity < 1,
      opacity: edgeOpacity,
    }),
  );
  mesh.add(edges);
  group.add(mesh);
  return mesh;
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
  const [activePreset, setActivePreset] = useState("fit");
  const [brightMode, setBrightMode] = useState(true);
  const [showEnclosure, setShowEnclosure] = useState(true);
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
    theta: CAMERA_PRESETS.fit.theta,
    phi: CAMERA_PRESETS.fit.phi,
    distance: 175,
    target: new THREE.Vector3(50, 4, -50),
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
  }, []);

  const selectPreset = useCallback((presetKey) => {
    const preset = CAMERA_PRESETS[presetKey] || CAMERA_PRESETS.fit;
    setActivePreset(presetKey);
    if (presetKey === "fit") {
      const { cx, cy, span } = getFocusBox();
      orbitRef.current.theta = preset.theta;
      orbitRef.current.phi = preset.phi;
      orbitRef.current.distance = Math.max(85, span * preset.distScale);
      orbitRef.current.target.set(cx, matThick + 4, -cy);
    } else {
      const span = Math.max(xMax, yMax, 110);
      orbitRef.current.theta = preset.theta;
      orbitRef.current.phi = preset.phi;
      orbitRef.current.distance = span * preset.distScale;
      orbitRef.current.target.set(xMax * 0.5, 18, -yMax * 0.5);
    }
    applyCameraOrbit();
  }, [xMax, yMax, matThick, getFocusBox, applyCameraOrbit]);

  // 初始化 Three.js 场景与渲染循环
  useEffect(() => {
    const container = mountRef.current;
    if (!container) return undefined;

    let renderer;
    try {
      renderer = new THREE.WebGLRenderer({
        antialias: true,
        alpha: false,
        powerPreference: "high-performance",
      });
    } catch (err) {
      setWebglError(err?.message || "当前环境不支持 WebGL 3D 渲染");
      return undefined;
    }

    const width = Math.max(320, container.clientWidth || 640);
    const height = Math.max(200, container.clientHeight || 420);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    renderer.setSize(width, height, false);
    renderer.setClearColor(0x131826, 1);

    container.innerHTML = "";
    container.appendChild(renderer.domElement);

    const scene = new THREE.Scene();
    const initialFov = height < 520 ? Math.min(62, 38 * Math.pow(520 / height, 0.55)) : 38;
    const camera = new THREE.PerspectiveCamera(initialFov, width / height, 1, 5000);

    // 充足立体照明：天地半球光 + 主平行光 + 冷色补光，清晰展现 PETG 打印件棱线与 MGN9H 金属导轨质感
    const hemiLight = new THREE.HemisphereLight(0xf8fafc, 0x475569, 1.6);
    hemiLight.position.set(0, 400, 0);
    scene.add(hemiLight);

    const mainDirLight = new THREE.DirectionalLight(0xffffff, 1.4);
    mainDirLight.position.set(220, 420, 260);
    scene.add(mainDirLight);

    const fillDirLight = new THREE.DirectionalLight(0x93c5fd, 0.95);
    fillDirLight.position.set(-220, 280, -240);
    scene.add(fillDirLight);

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

    // 激光束与焦点光斑对象（M5 时为绿色定位导引细光束，M3 出光时为高亮蓝紫/洋红高能激光束）
    const beamCoreGeo = new THREE.CylinderGeometry(0.5, 0.18, 1, 16);
    const beamCoreMat = new THREE.MeshBasicMaterial({
      color: 0xffffff,
      transparent: true,
      opacity: 0.96,
    });
    const beamCoreMesh = new THREE.Mesh(beamCoreGeo, beamCoreMat);

    const beamHaloGeo = new THREE.CylinderGeometry(1.85, 0.95, 1, 16);
    const beamHaloMat = new THREE.MeshBasicMaterial({
      color: 0x3b82f6,
      transparent: true,
      opacity: 0.45,
    });
    const beamHaloMesh = new THREE.Mesh(beamHaloGeo, beamHaloMat);

    // 焦点靶环
    const spotRingGeo = new THREE.RingGeometry(0.9, 3.2, 28);
    spotRingGeo.rotateX(-Math.PI / 2);
    const spotRingMat = new THREE.MeshBasicMaterial({
      color: 0x34d399,
      side: THREE.DoubleSide,
      transparent: true,
      opacity: 0.95,
    });
    const spotRingMesh = new THREE.Mesh(spotRingGeo, spotRingMat);

    const spotCenterGeo = new THREE.CircleGeometry(0.75, 20);
    spotCenterGeo.rotateX(-Math.PI / 2);
    const spotCenterMat = new THREE.MeshBasicMaterial({
      color: 0xffffff,
      side: THREE.DoubleSide,
    });
    const spotCenterMesh = new THREE.Mesh(spotCenterGeo, spotCenterMat);

    const spotPointLight = new THREE.PointLight(0x38bdf8, 0, 95);

    scene.add(beamCoreMesh);
    scene.add(beamHaloMesh);
    scene.add(spotRingMesh);
    scene.add(spotCenterMesh);
    scene.add(spotPointLight);

    sceneRef.current = {
      renderer,
      scene,
      camera,
      hemiLight,
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
      spotPointLight,
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

    let pulsePhase = 0;
    const animate = () => {
      const ctx = sceneRef.current;
      if (!ctx) return;
      ctx.animId = requestAnimationFrame(animate);
      pulsePhase += 0.12;
      const s = 1 + Math.sin(pulsePhase) * 0.2;
      ctx.spotRingMesh.scale.set(s, 1, s);
      if (ctx.fanHubMesh) {
        ctx.fanHubMesh.rotation.z += 0.18;
      }
      ctx.renderer.render(ctx.scene, ctx.camera);
    };
    animate();

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
      renderer.dispose();
      sceneRef.current = null;
    };
  }, []);

  // 切换高对比工程亮色背景 / 深空底色
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;
    ctx.renderer.setClearColor(brightMode ? 0x161d2f : 0x0b0e17, 1);
    if (ctx.hemiLight) {
      ctx.hemiLight.intensity = brightMode ? 1.65 : 1.15;
    }
  }, [brightMode]);

  // 切换翼宿 V1.0 外壳与半透亚克力护罩显隐
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx?.enclosureGroup) return;
    ctx.enclosureGroup.visible = Boolean(showEnclosure);
  }, [showEnclosure]);

  // 当切换/上传新刀路时，若处于 fit 视角则自动对焦到刀路区域（调整材料坐标时保持机台视角稳定，直观看到材料与激光头在台面上移动）
  useEffect(() => {
    if (activePreset === "fit") {
      selectPreset("fit");
    }
  }, [toolpath]);

  // 按《B站【大鱼DIY】激光雕刻机_翼宿 V1.0 图纸包》构建真实机台、导轨、电机、外壳与激光头打印件总成
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;
    const { frameGroup, enclosureGroup, workpieceGroup, cursorGroup, gantryGroup, headGroup } = ctx;

    frameGroup.clear();
    enclosureGroup.clear();
    workpieceGroup.clear();
    cursorGroup.clear();

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
    const petgDarkMat = new THREE.MeshStandardMaterial({
      color: 0x263147,
      roughness: 0.55,
      metalness: 0.15,
    });
    const petgAccentMat = new THREE.MeshStandardMaterial({
      color: 0x334155,
      roughness: 0.48,
      metalness: 0.18,
    });
    const petgPrintMat = new THREE.MeshStandardMaterial({
      color: 0x3b4d6b,
      roughness: 0.42,
      metalness: 0.15,
    });
    const cuttingPadMat = new THREE.MeshStandardMaterial({
      color: 0x1a2234,
      roughness: 0.72,
      metalness: 0.25,
    });
    const mgnRailMat = new THREE.MeshStandardMaterial({
      color: 0xcbd5e1,
      roughness: 0.25,
      metalness: 0.78,
    });
    const mgnBlockMat = new THREE.MeshStandardMaterial({
      color: 0x10b981,
      roughness: 0.35,
      metalness: 0.45,
    });
    const motorBodyMat = new THREE.MeshStandardMaterial({
      color: 0x1e293b,
      roughness: 0.5,
      metalness: 0.35,
    });
    const motorCapMat = new THREE.MeshStandardMaterial({
      color: 0x94a3b8,
      roughness: 0.3,
      metalness: 0.68,
    });
    const beltMat = new THREE.MeshStandardMaterial({
      color: 0x0f172a,
      roughness: 0.85,
      metalness: 0.08,
    });
    const brassMat = new THREE.MeshStandardMaterial({
      color: 0xf59e0b,
      roughness: 0.28,
      metalness: 0.75,
    });
    const acrylicOrangeMat = new THREE.MeshStandardMaterial({
      color: 0xea580c,
      roughness: 0.15,
      metalness: 0.05,
      transparent: true,
      opacity: 0.15,
      depthWrite: false,
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
      new THREE.BoxGeometry(sideSkirtW, baseFrameThickness, chassisD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseLeft.position.set(cx - cuttingMatW / 2 - sideSkirtW / 2, -3, cz);

    const baseRight = addMeshWithEdges(
      frameGroup,
      new THREE.BoxGeometry(sideSkirtW, baseFrameThickness, chassisD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseRight.position.set(cx + cuttingMatW / 2 + sideSkirtW / 2, -3, cz);

    const baseFront = addMeshWithEdges(
      frameGroup,
      new THREE.BoxGeometry(cuttingMatW, baseFrameThickness, frontBackSkirtD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseFront.position.set(cx, -3, cz + cuttingMatD / 2 + frontBackSkirtD / 2);

    const baseBack = addMeshWithEdges(
      frameGroup,
      new THREE.BoxGeometry(cuttingMatW, baseFrameThickness, frontBackSkirtD),
      petgDarkMat,
      0x475569,
      0.55,
    );
    baseBack.position.set(cx, -3, cz - cuttingMatD / 2 - frontBackSkirtD / 2);

    // 切割垫板.STEP (121.9 × 4.5 × 121.9 mm)：位于工作区正中央
    const padMesh = addMeshWithEdges(
      frameGroup,
      new THREE.BoxGeometry(cuttingMatW, 4.5, cuttingMatD),
      cuttingPadMat,
      0x64748b,
      0.7,
    );
    padMesh.position.set(cx, -2.3, cz);

    // 切割垫板条形防反光排烟格栅槽 (还原 切割垫板.STEP 表面平行凹槽)
    for (let sx = -cuttingMatW / 2 + 8; sx <= cuttingMatW / 2 - 8; sx += 10) {
      const slat = new THREE.Mesh(
        new THREE.BoxGeometry(2.2, 0.3, cuttingMatD - 14),
        new THREE.MeshBasicMaterial({ color: 0x0f172a }),
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
      const c = isMajor ? [0.38, 0.68, 0.98] : [0.24, 0.33, 0.48];
      gridPos.push(gx, 0.1, 0, gx, 0.1, -yMax);
      gridColors.push(...c, ...c);
    }
    for (let gy = 0; gy <= yMax; gy += step) {
      const isMajor = gy % 50 === 0 || gy === yMax;
      const c = isMajor ? [0.38, 0.68, 0.98] : [0.24, 0.33, 0.48];
      gridPos.push(0, 0.1, -gy, xMax, 0.1, -gy);
      gridColors.push(...c, ...c);
    }
    const gridGeo = new THREE.BufferGeometry();
    gridGeo.setAttribute("position", new THREE.Float32BufferAttribute(gridPos, 3));
    gridGeo.setAttribute("color", new THREE.Float32BufferAttribute(gridColors, 3));
    frameGroup.add(
      new THREE.LineSegments(
        gridGeo,
        new THREE.LineBasicMaterial({ vertexColors: true, transparent: true, opacity: 0.92 }),
      ),
    );

    // 3. 机器零点 (0,0) 基准环与跟随雕刻起点 (toolpathOffsetX, toolpathOffsetY) 移动的醒目坐标轴
    if (toolpathOffsetX > 0.01 || toolpathOffsetY > 0.01) {
      const homeRing = new THREE.Mesh(
        new THREE.RingGeometry(1.2, 2.4, 24).rotateX(-Math.PI / 2),
        new THREE.MeshBasicMaterial({ color: 0x64748b, side: THREE.DoubleSide }),
      );
      homeRing.position.set(0, 0.25, 0);
      frameGroup.add(homeRing);
    }

    const originSurfaceY = matThick + 0.22;
    const axisLen = Math.min(22, Math.max(10, Math.min(matWidth, matHeight) * 0.35));
    const axisGeo = new THREE.BufferGeometry();
    axisGeo.setAttribute(
      "position",
      new THREE.Float32BufferAttribute([
        toolpathOffsetX, originSurfaceY, -toolpathOffsetY, toolpathOffsetX + axisLen, originSurfaceY, -toolpathOffsetY,
        toolpathOffsetX, originSurfaceY, -toolpathOffsetY, toolpathOffsetX, originSurfaceY, -(toolpathOffsetY + axisLen),
        toolpathOffsetX, originSurfaceY, -toolpathOffsetY, toolpathOffsetX, originSurfaceY + 14, -toolpathOffsetY,
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
    workpieceGroup.add(new THREE.LineSegments(axisGeo, new THREE.LineBasicMaterial({ vertexColors: true })));

    const originRing = new THREE.Mesh(
      new THREE.RingGeometry(1.6, 3.4, 28).rotateX(-Math.PI / 2),
      new THREE.MeshBasicMaterial({ color: 0x34d399, side: THREE.DoubleSide }),
    );
    originRing.position.set(toolpathOffsetX, originSurfaceY + 0.03, -toolpathOffsetY);
    workpieceGroup.add(originRing);
    // =========================================================================
    // 4. Y 轴线性导轨与双 42 步进电机传动总成 (BOM: 2×140mm MGN9H + 2×42步进 + Ø8×186mm联动轴)
    // =========================================================================
    [-1, 1].forEach((side) => {
      const rx = cx + side * yRailOffsetX;

      // PETG 侧向导轨承托台
      const railBed = addMeshWithEdges(
        frameGroup,
        new THREE.BoxGeometry(16, 18, railLen + 36),
        petgAccentMat,
        0x64748b,
        0.55,
      );
      railBed.position.set(rx, 9, cz);

      // 140mm MGN9H 线性导轨 (宽 9mm × 高 6.5mm × 长 140mm)
      const yRail = addMeshWithEdges(
        frameGroup,
        new THREE.BoxGeometry(9, 6.5, railLen),
        mgnRailMat,
        0x475569,
        0.8,
      );
      yRail.position.set(rx, 21.25, cz);

      // Y 轴 GT2 6mm 同步带
      const yBelt = new THREE.Mesh(
        new THREE.BoxGeometry(2.2, 6, railLen + 24),
        beltMat,
      );
      yBelt.position.set(rx - side * 6.5, 25, cz - 4);
      frameGroup.add(yBelt);

      // 前置 惰轮调节块_Y左/Y右.STEP + 手拧螺帽.STEP (PETG 打印件)
      const tensionBlock = addMeshWithEdges(
        frameGroup,
        new THREE.BoxGeometry(15, 16, 22),
        petgPrintMat,
        0x38bdf8,
        0.7,
      );
      tensionBlock.position.set(rx, 22, cz + railLen / 2 + 12);

      // 星形滚花 手拧螺帽.STEP (直径 15mm, 厚 8mm)
      const thumbNut = addMeshWithEdges(
        frameGroup,
        new THREE.CylinderGeometry(7.5, 7.5, 8, 12).rotateX(Math.PI / 2),
        new THREE.MeshStandardMaterial({ color: 0x38bdf8, roughness: 0.35, metalness: 0.2 }),
        0xe0f2fe,
        0.85,
      );
      thumbNut.position.set(rx, 22, cz + railLen / 2 + 27);

      // 后置 42 步进电机 (42×42×40mm) + 步进电机调节块.STEP
      const motorX = rx - side * 6;
      const motorZ = cz - railLen / 2 - 18;

      const motorMount = addMeshWithEdges(
        frameGroup,
        new THREE.BoxGeometry(32, 32, 8),
        petgPrintMat,
        0x64748b,
        0.65,
      );
      motorMount.position.set(motorX, 18, motorZ + 14);

      const motorBody = addMeshWithEdges(
        frameGroup,
        new THREE.BoxGeometry(42, 40, 42),
        motorBodyMat,
        0x64748b,
        0.7,
      );
      motorBody.position.set(motorX, 20, motorZ - 10);

      const motorTopCap = addMeshWithEdges(
        frameGroup,
        new THREE.BoxGeometry(42.4, 7, 42.4),
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
      new THREE.BoxGeometry(chassisW, 62, 6),
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
      new THREE.BoxGeometry(40, 40, 28),
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
      new THREE.MeshStandardMaterial({ color: 0x38bdf8, roughness: 0.3 }),
      0xbae6fd,
      0.8,
    );
    ctx.fanHubMesh = fanHub;
    enclosureGroup.add(fanGroup);

    // 主板盖.STEP (G3 主板防护盒，带六边形散热格栅视觉，72 × 54 × 18 mm)
    const boardCover = addMeshWithEdges(
      enclosureGroup,
      new THREE.BoxGeometry(72, 54, 18),
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
        new THREE.BoxGeometry(8, 24, chassisD),
        petgDarkMat,
        0x475569,
        0.65,
      );
      lowerSide.position.set(sx, 12, cz);

      // 上沿側框立柱与顶梁 (还原 侧框_左/右.STEP 视窗框造型)
      const upperSide = addMeshWithEdges(
        enclosureGroup,
        new THREE.BoxGeometry(6, chassisH - 24, chassisD),
        sideWallMat,
        0x64748b,
        0.45,
      );
      upperSide.position.set(sx, 24 + (chassisH - 24) / 2, cz);
    });

    // 顶盖框架.STEP + 顶部观察板_厚度3mm.DXF (186.8 × 213 × 3 mm 半透橙红防激光亚克力)
    const topAcrylic = addMeshWithEdges(
      enclosureGroup,
      new THREE.BoxGeometry(186.8, 3, 213),
      acrylicOrangeMat,
      0xf97316,
      0.55,
    );
    topAcrylic.position.set(cx, chassisH - 1.5, cz);

    // 前遮光板_厚度3mm.DXF (158 × 107 × 3 mm 半透橙红防激光亚克力)
    const frontAcrylic = addMeshWithEdges(
      enclosureGroup,
      new THREE.BoxGeometry(158, 86, 3),
      acrylicOrangeMat,
      0xf97316,
      0.5,
    );
    frontAcrylic.position.set(cx, 20 + 43, cz + chassisD / 2 - 2);

    // 后部左右阻尼铰链 (BOM: 小号阻尼合页 1.5kgf.cm 左/右) 与前部 顶板固定器.STEP
    [-68, 68].forEach((hx) => {
      const hinge = addMeshWithEdges(
        enclosureGroup,
        new THREE.BoxGeometry(14, 10, 12),
        petgPrintMat,
        0x94a3b8,
        0.7,
      );
      hinge.position.set(cx + hx, chassisH - 2, backWallZ + 4);

      const frontClip = addMeshWithEdges(
        enclosureGroup,
        new THREE.BoxGeometry(28, 6, 10),
        petgPrintMat,
        0x94a3b8,
        0.65,
      );
      frontClip.position.set(cx + hx, chassisH - 1, cz + chassisD / 2 - 4);
    });

    // =========================================================================
    // 6. 加工板材 (Workpiece Slab) —— 按用户设置的尺寸 (matWidth × matHeight) 与起点坐标 (offsetX, offsetY) 摆放
    // =========================================================================
    const slabGeo = new THREE.BoxGeometry(matWidth, matThick, matHeight);
    const slabMat = new THREE.MeshStandardMaterial({
      color: materialColorHex(matName),
      roughness: 0.65,
      metalness: 0.05,
      transparent: true,
      opacity: 0.92,
    });
    const slabMesh = addMeshWithEdges(workpieceGroup, slabGeo, slabMat, 0xfbbf24, 0.78);
    slabMesh.position.set(offsetX + matWidth / 2, matThick / 2, -(offsetY + matHeight / 2));
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
      color: 0x34d399,
      transparent: true,
      opacity: 0.6,
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
    gantryGroup.clear();
    headGroup.clear();
    gantryGroup.add(headGroup);

    const gantryBaseY = 26; // Y 轴 MGN9H 滑块顶面高度
    const xRailSpan = yRailOffsetX * 2;
    const xRailLen = Math.max(140, xMax + 40);

    // 左右 Y 轴 MGN9H 滑块 + XY转接块.STEP + 联动支架.STEP 肩部惰轮座
    [-1, 1].forEach((side) => {
      const gx = cx + side * yRailOffsetX;

      // Y 轴 MGN9H 滑块 (28.9 × 8 × 20 mm)
      const yCarriageBlock = addMeshWithEdges(
        gantryGroup,
        new THREE.BoxGeometry(20, 7, 29),
        mgnBlockMat,
        0x6ee7b7,
        0.85,
      );
      yCarriageBlock.position.set(gx, 24.5, 0);

      // XY转接块.STEP (PETG 打印件，36 × 15.2 × 21 mm，带阶梯与斜角)
      const xyAdapter = addMeshWithEdges(
        gantryGroup,
        new THREE.BoxGeometry(22, 15.2, 35),
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
      new THREE.BoxGeometry(xRailSpan - 8, 14, 15),
      petgPrintMat,
      0x38bdf8,
      0.75,
    );
    linkBracket.position.set(cx, gantryBaseY + 11, -3);

    // X 轴 140mm MGN9H 线性导轨 (安装在联动支架正面，宽 140mm × 高 9mm × 厚 6.5mm)
    const xMgnRail = addMeshWithEdges(
      gantryGroup,
      new THREE.BoxGeometry(xRailLen, 9, 6.5),
      mgnRailMat,
      0x475569,
      0.85,
    );
    xMgnRail.position.set(cx, gantryBaseY + 11, 7.5);

    // X 轴横向 GT2 6mm 同步带
    const xBelt = new THREE.Mesh(
      new THREE.BoxGeometry(xRailLen, 6, 1.8),
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
      new THREE.BoxGeometry(28.9, 20, 8),
      mgnBlockMat,
      0x6ee7b7,
      0.9,
    );
    xCarriageBlock.position.set(0, headMountY, 9.5);

    // (b) X轴连接件.STEP + 同步带卡齿_固定/可调.STEP (PETG 打印件，抱合滑块并咬合后方同步带)
    const xConnector = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(33, 24, 12),
      petgPrintMat,
      0x60a5fa,
      0.85,
    );
    xConnector.position.set(0, headMountY + 2, 14.5);

    // 同步带卡齿块 (顶部后伸卡入 GT2 皮带)
    const beltClamp = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(22, 9, 14),
      petgAccentMat,
      0x38bdf8,
      0.8,
    );
    beltClamp.position.set(0, headMountY + 11, 8);

    // (c) 激光头转接板_5W.STEP (PETG 打印件：35.6 × 38 × 6 mm 背板 + 左右竖向调焦滑槽耳 + 抱箍侧翼)
    const adapterPlate = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(35.6, 38, 5.5),
      petgPrintMat,
      0x38bdf8,
      0.92,
    );
    adapterPlate.position.set(0, headMountY + 1, 22);

    // 转接板左右竖向调焦长圆滑槽 + 4 颗 M3 圆头内六角固定螺丝 (还原 激光头转接板_5W.STEP 标志特征)
    [-13.5, 13.5].forEach((sx) => {
      // 竖向调焦滑槽深色凹槽
      const slotGroove = new THREE.Mesh(
        new THREE.BoxGeometry(3.4, 24, 1.2),
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
        new THREE.BoxGeometry(3.5, 28, 18),
        petgPrintMat,
        0x38bdf8,
        0.85,
      );
      sideClampEar.position.set(sx > 0 ? 16.5 : -16.5, headMountY - 1, 31);
    });

    // (d) 线束固定器.STEP + 线束固定压块.STEP (位于转接板右上方，夹持激光头 24V/TTL 线束)
    const wireHolder = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(11, 16, 10),
      petgPrintMat,
      0x38bdf8,
      0.85,
    );
    wireHolder.position.set(12, headMountY + 24, 20);

    const wireClamp = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(13, 5, 11),
      new THREE.MeshStandardMaterial({ color: 0x0ea5e9, roughness: 0.35 }),
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

    // 5W 激光器阳极氧化铝合金主体 (30 × 40 × 30 mm，略带科技半透以便任何角度都不彻底死挡视线)
    const laserHeatsinkMat = new THREE.MeshStandardMaterial({
      color: 0x1d4ed8,
      metalness: 0.45,
      roughness: 0.28,
      transparent: true,
      opacity: 0.86,
    });
    const laserBody = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(29.5, 38, 29.5),
      laserHeatsinkMat,
      0x60a5fa,
      0.95,
    );
    laserBody.position.set(0, headMountY + 1, laserCenterZ);

    // 5W 激光器正面与侧面竖向 CNC 散热鳍片槽 (还原真实 5W 激光模块外观)
    [-10, -5, 0, 5, 10].forEach((fx) => {
      const fin = new THREE.Mesh(
        new THREE.BoxGeometry(1.4, 34, 31),
        new THREE.MeshStandardMaterial({ color: 0x0f172a, metalness: 0.5, roughness: 0.4 }),
      );
      fin.position.set(fx, headMountY + 1, laserCenterZ);
      headGroup.add(fin);
    });

    // 激光器顶部 3010 轴流冷却风扇 + 24V/TTL 3P 白色接线端子座
    const topFanCap = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(30, 8, 30),
      motorBodyMat,
      0x94a3b8,
      0.85,
    );
    topFanCap.position.set(0, headMountY + 24, laserCenterZ);

    const xhConnector = addMeshWithEdges(
      headGroup,
      new THREE.BoxGeometry(10, 5, 7),
      new THREE.MeshStandardMaterial({ color: 0xf8fafc, roughness: 0.3 }),
      0x94a3b8,
      0.8,
    );
    xhConnector.position.set(6, headMountY + 29.5, laserCenterZ - 6);

    // 顶部运行状态 LED 指示灯
    const ledMat = new THREE.MeshBasicMaterial({ color: 0x34d399 });
    const headLed = new THREE.Mesh(new THREE.SphereGeometry(2.4, 14, 14), ledMat);
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
  }, [xMax, yMax, zMax, matWidth, matHeight, matThick, matName, offsetX, offsetY, toolpathOffsetX, toolpathOffsetY]);
  // 构建三维刀路轨迹几何体（高对比度：亮青蓝待加工、炽金红已加工、带方向箭头的 G0 空移、动态彗星尾迹）
  useEffect(() => {
    const ctx = sceneRef.current;
    if (!ctx) return;
    const { toolpathGroup } = ctx;
    toolpathGroup.clear();
    ctx.doneCutLine = null;
    ctx.todoCutLine = null;
    ctx.rapidLine = null;
    ctx.arrowLine = null;
    ctx.cometLine = null;

    const segments = toolpath?.segments || [];
    if (segments.length === 0) return;

    const surfaceY = matThick + 0.45;
    const rapidY = matThick + 2.0;

    // 雕刻文字/刀路在材料上表面的包络边界框（直观展示文字在材料内部的位置范围）
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
        new THREE.LineBasicMaterial({
          color: 0x34d399,
          transparent: true,
          opacity: 0.48,
        }),
      );
      toolpathGroup.add(boxLine);
    }

    const cutPositions = [];
    const doneColors = [];
    const todoColors = [];
    const rapidPositions = [];
    const arrowPositions = [];

    for (let i = 0; i < segments.length; i++) {
      const seg = segments[i];
      if (seg.laserOn) {
        const y0 = surfaceY + (seg.from.z || 0);
        const y1 = surfaceY + (seg.to.z || 0);
        cutPositions.push(seg.from.x, y0, -seg.from.y, seg.to.x, y1, -seg.to.y);
        const rgb = donePowerToRgb(seg.powerRatio || 0.7);
        doneColors.push(...rgb, ...rgb);
        todoColors.push(0.22, 0.74, 0.97, 0.22, 0.74, 0.97);
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

    if (cutPositions.length > 0) {
      const todoGeo = new THREE.BufferGeometry();
      todoGeo.setAttribute("position", new THREE.Float32BufferAttribute(cutPositions, 3));
      todoGeo.setAttribute("color", new THREE.Float32BufferAttribute(todoColors, 3));
      const todoLine = new THREE.LineSegments(
        todoGeo,
        new THREE.LineBasicMaterial({
          vertexColors: true,
          transparent: true,
          opacity: 0.82,
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
      toolpathGroup.add(doneLine);
      ctx.doneCutLine = doneLine;
    }

    if (rapidPositions.length > 0) {
      const rapidGeo = new THREE.BufferGeometry();
      rapidGeo.setAttribute("position", new THREE.Float32BufferAttribute(rapidPositions, 3));
      const rapidLine = new THREE.LineSegments(
        rapidGeo,
        new THREE.LineBasicMaterial({
          color: 0xa78bfa,
          transparent: true,
          opacity: 0.55,
        }),
      );
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
          color: 0xfbbf24,
          transparent: true,
          opacity: 0.85,
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
      new THREE.LineBasicMaterial({ color: 0xffffff, transparent: true, opacity: 0.95 }),
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

  // 实时更新龙门架、翼宿 5W 激光头打印件总成位置、刀路偏置、激光束、十字游标、彗星尾迹与已完成刀路
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

    // 3D 刀路轨迹组与雕刻合成起点 (toolpathOffsetX, toolpathOffsetY) 及材料宽高比例同步，始终在材料内部
    if (ctx.toolpathGroup) {
      ctx.toolpathGroup.position.set(toolpathOffsetX, 0, -toolpathOffsetY);
      ctx.toolpathGroup.scale.set(toolpathScaleX, 1, toolpathScaleY);
    }

    // Y 轴龙门沿 MGN9H 导轨前后移动，X 轴激光头打印件总成沿横梁 MGN9H 导轨左右移动
    ctx.gantryGroup.position.set(0, 0, -py);
    ctx.headGroup.position.set(px, pz, 0);

    // 更新台面 X/Y 十字准星游标线位置
    if (ctx.cursorXLine && ctx.cursorYLine) {
      ctx.cursorXLine.position.z = -py;
      ctx.cursorYLine.position.x = px;
      ctx.cursorXLine.material.color.setHex(laserOn ? 0xf43f5e : 0x34d399);
    }
    if (ctx.headLedMat) {
      ctx.headLedMat.color.setHex(laserOn ? 0xff3b5c : 0x34d399);
    }

    // 激光束与焦点靶环
    const surfaceY = matThick + 0.45;
    const tipY = (ctx.nozzleTipY || 12) + pz;
    const beamLen = Math.max(1.5, tipY - surfaceY);
    const midY = surfaceY + beamLen / 2;

    ctx.beamCoreMesh.visible = true;
    ctx.beamHaloMesh.visible = true;
    ctx.spotRingMesh.visible = true;
    ctx.spotCenterMesh.visible = true;

    ctx.spotRingMesh.position.set(px, surfaceY + 0.15, -py);
    ctx.spotCenterMesh.position.set(px, surfaceY + 0.18, -py);

    if (laserOn) {
      // 24V 5W 450nm 蓝紫高能半导体激光束
      const beamScale = 0.75 + powerRatio * 0.85;
      ctx.beamCoreMesh.material.color.setHex(0xffffff);
      ctx.beamCoreMesh.position.set(px, midY, -py);
      ctx.beamCoreMesh.scale.set(beamScale, beamLen, beamScale);

      ctx.beamHaloMesh.material.color.setHex(0x3b82f6);
      ctx.beamHaloMesh.material.opacity = 0.42 + powerRatio * 0.4;
      ctx.beamHaloMesh.position.set(px, midY, -py);
      ctx.beamHaloMesh.scale.set(beamScale * 1.55, beamLen, beamScale * 1.55);

      ctx.spotRingMesh.material.color.setHex(0xfbbf24);
      ctx.spotPointLight.color.setHex(0x60a5fa);
      ctx.spotPointLight.position.set(px, surfaceY + 8, -py);
      ctx.spotPointLight.intensity = 1.8 + powerRatio * 2.4;
    } else {
      // M5 安全定位导引光束
      ctx.beamCoreMesh.material.color.setHex(0x34d399);
      ctx.beamCoreMesh.position.set(px, midY, -py);
      ctx.beamCoreMesh.scale.set(0.32, beamLen, 0.32);

      ctx.beamHaloMesh.material.color.setHex(0x10b981);
      ctx.beamHaloMesh.material.opacity = 0.18;
      ctx.beamHaloMesh.position.set(px, midY, -py);
      ctx.beamHaloMesh.scale.set(0.65, beamLen, 0.65);

      ctx.spotRingMesh.material.color.setHex(0x34d399);
      ctx.spotPointLight.intensity = 0.45;
    }

    // 更新已完成雕刻段 drawRange 与动态彗星尾迹
    if (toolpath?.segments) {
      const segs = toolpath.segments;
      const completedIdx = pose?.completedSegmentCount ?? 0;
      const activeLimit = Math.min(segs.length, completedIdx + (pose?.activeSegmentFraction > 0 ? 1 : 0));

      if (ctx.doneCutLine) {
        let cutVerticesDone = 0;
        for (let i = 0; i < activeLimit; i++) {
          if (segs[i].laserOn) cutVerticesDone += 2;
        }
        ctx.doneCutLine.geometry.setDrawRange(0, cutVerticesDone);
      }

      if (ctx.cometLine) {
        const posAttr = ctx.cometLine.geometry.getAttribute("position");
        const arr = posAttr.array;
        const tailCount = 24;
        const startIdx = Math.max(0, activeLimit - tailCount);
        let ptr = 0;
        for (let i = startIdx; i < activeLimit && ptr + 5 < arr.length; i++) {
          const s = segs[i];
          const y0 = surfaceY + (s.from.z || 0) + 0.2;
          const y1 = surfaceY + (s.to.z || 0) + 0.2;
          arr[ptr++] = s.from.x;
          arr[ptr++] = y0;
          arr[ptr++] = -s.from.y;
          arr[ptr++] = i === activeLimit - 1 ? (Number(pose?.x) || 0) : s.to.x;
          arr[ptr++] = y1;
          arr[ptr++] = i === activeLimit - 1 ? -(Number(pose?.y) || 0) : -s.to.y;
        }
        posAttr.needsUpdate = true;
        ctx.cometLine.geometry.setDrawRange(0, ptr / 3);
      }
    }

    if (followHead) {
      orbitRef.current.target.set(px, surfaceY, -py);
      applyCameraOrbit();
    }
  }, [pose, toolpath, xMax, yMax, matWidth, matHeight, matThick, matName, offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, toolpathScaleX, toolpathScaleY, followHead, applyCameraOrbit]);
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
      {/* 视口顶部可收起 HUD 覆盖层：实时坐标、功率、速度与视角切换 */}
      <div className={`twin-hud-top ${hudCollapsed ? "is-collapsed" : ""}`}>
        {hudCollapsed ? (
          <div className="twin-coord-cluster">
            <button
              type="button"
              className="btn btn-ghost twin-hud-collapse-btn is-collapsed"
              onClick={toggleHudCollapsed}
              title="展开 3D 视口坐标、激光状态与视角控制浮层"
            >
              <ChevronDown size={13} />
              <span>展开浮层</span>
              <span
                className={`twin-laser-dot ${pose?.laserOn ? "is-firing" : ""}`}
                style={{ color: pose?.laserOn ? "#a78bfa" : "#34d399" }}
              />
            </button>
          </div>
        ) : (
          <>
            <div className="twin-coord-cluster">
              <button
                type="button"
                className="btn btn-ghost twin-hud-collapse-btn"
                onClick={toggleHudCollapsed}
                title="收起视口上的所有文字与按钮浮层，不遮挡 3D 工作台"
              >
                <ChevronUp size={13} />
                <span>收起浮层</span>
              </button>
              <div className="twin-coord-chip">
                <span className="twin-coord-axis">X</span>
                <b>{((pose?.x ?? 0) * toolpathScaleX + toolpathOffsetX).toFixed(2)}</b>
                <small>mm</small>
              </div>
              <div className="twin-coord-chip">
                <span className="twin-coord-axis">Y</span>
                <b>{((pose?.y ?? 0) * toolpathScaleY + toolpathOffsetY).toFixed(2)}</b>
                <small>mm</small>
              </div>
              <div className="twin-coord-chip">
                <span className="twin-coord-axis">Z</span>
                <b>{(pose?.z ?? 0).toFixed(2)}</b>
                <small>mm</small>
              </div>
              <div className={`twin-laser-badge ${pose?.laserOn ? "is-firing" : ""}`}>
                <span className="twin-laser-dot" />
                <span>{pose?.laserOn ? `5W LASER ON · S${Math.round(pose?.power || 0)}` : "LASER SAFE (M5)"}</span>
              </div>
              <div className="twin-feed-chip">
                <span>
                  F {Math.round(pose?.feedRate || 0)} mm/min · 板材 ({offsetX}, {offsetY}) · 材料内 (+{textOffsetX}, +{textOffsetY})
                </span>
              </div>
            </div>

            <div className="twin-camera-bar" role="toolbar" aria-label="三维视角控制">
              {Object.entries(CAMERA_PRESETS).map(([key, item]) => (
                <button
                  key={key}
                  type="button"
                  className={`btn btn-ghost twin-cam-btn ${activePreset === key ? "active" : ""}`}
                  onClick={() => selectPreset(key)}
                >
                  {key === "fit" ? <Focus size={12} /> : <Compass size={12} />}
                  <span>{item.label}</span>
                </button>
              ))}
              <button
                type="button"
                className={`btn btn-ghost twin-cam-btn ${showEnclosure ? "active" : ""}`}
                onClick={() => setShowEnclosure((v) => !v)}
                title="显示/隐藏翼宿 V1.0 半透防激光亚克力护罩、侧框与后置 4028 风扇外罩"
              >
                <Box size={12} />
                <span>亚克力护罩</span>
              </button>
              <button
                type="button"
                className={`btn btn-ghost twin-cam-btn ${followHead ? "active" : ""}`}
                onClick={onToggleFollow}
                title="视角自动锁定并跟随 5W 激光头打印件总成移动"
              >
                <Crosshair size={12} />
                <span>跟随刀头</span>
              </button>
              <button
                type="button"
                className={`btn btn-ghost twin-cam-btn ${showRapid ? "active" : ""}`}
                onClick={onToggleRapid}
                title="显示/隐藏 G0 空移轨迹与方向箭头"
              >
                <Eye size={12} />
                <span>G0 空移</span>
              </button>
              <button
                type="button"
                className={`btn btn-ghost twin-cam-btn ${brightMode ? "active" : ""}`}
                onClick={() => setBrightMode((v) => !v)}
                title="切换高对比度工程亮色照明 / 暗色背景"
              >
                <Sun size={12} />
                <span>提亮</span>
              </button>
              <button
                type="button"
                className="btn btn-ghost twin-cam-btn"
                onClick={() => selectPreset("fit")}
                title="重置并聚焦到刀路区域"
              >
                <RotateCcw size={12} />
              </button>
            </div>
          </>
        )}
      </div>

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

      {/* 视口左下角图例与翼宿 V1.0 真实机台规格脚标（收起浮层时一并隐藏，避免遮挡工作台） */}
      {!hudCollapsed && (
        <div className="twin-hud-bottom">
          <div className="twin-legend">
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--cut" />
              已加工刀路 (金红)
            </span>
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--todo" />
              待加工刀路 (亮青)
            </span>
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--rapid" />
              G0 空移与走向箭头
            </span>
            <span className="twin-legend-item">
              <i className="twin-swatch twin-swatch--origin" />
              5W 激光落点与十字游标
            </span>
          </div>
          <div className="twin-workspace-tag">
            <span>翼宿 V1.0 工作区 {xMax}×{yMax} mm</span>
            <span>·</span>
            <span>
              板材 {matName} ({matWidth}×{matHeight}×{matThick} mm · 台面 X{offsetX} Y{offsetY} · 材料内位置 +{textOffsetX}, +{textOffsetY} mm)
            </span>
            <span>·</span>
            <span>左键旋转 / 右键平移 / 滚轮缩放</span>
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
