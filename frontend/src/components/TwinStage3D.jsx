import React, { useEffect, useRef, useState, useCallback } from "react";
import { disposeObjectResources } from "../threeDispose.js";
import * as THREE from "three";
import { RoomEnvironment } from "three/examples/jsm/environments/RoomEnvironment.js";
import { Route, RotateCcw, Crosshair, Sun, Box, EyeOff, Eye } from "lucide-react";
import {
  CAMERA_PRESETS,
  DEFAULT_PRESET,
  GHOST_PRESETS,
  MAX_PIXEL_RATIO,
  acquireTwinRenderer,
  applyCoverVisibility,
  applyGhost,
  clampCoord,
  makeGlowTexture,
  placeOffsetObjects,
  releaseCanvas,
  updateBurnSurface,
} from "../twin/stageVisuals.js";
import { rebuildMachineScene } from "../twin/buildMachineScene.js";
import { rebuildToolpathLines } from "../twin/buildToolpathLines.js";

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
  }, [applyCameraOrbit, syncStageViewport]);

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

  // 下面几个 effect 只在各自的触发条件变化时运行（换刀路、重建机台、重建刀路），运行时读这里的最新值，
  // 不因这些值变化而重跑（换视角、平移板材、切换空移线显示各有自己的 effect）
  const latestRef = useRef(null);
  useEffect(() => {
    latestRef.current = { selectPreset, activePreset, offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, showRapid };
  });

  // 当切换/上传新刀路时，若处于 fit 视角则自动对焦到刀路区域（调整材料坐标时保持机台视角稳定，直观看到材料与激光头在台面上移动）
  const cameraPlacedRef = useRef(false);
  useEffect(() => {
    if (!cameraPlacedRef.current) {
      // 首次挂载直接摆到默认视角，不做过渡
      cameraPlacedRef.current = Boolean(sceneRef.current);
      latestRef.current.selectPreset(latestRef.current.activePreset, { instant: true });
    } else if (latestRef.current.activePreset === "fit") {
      latestRef.current.selectPreset("fit");
    }
  }, [toolpath]);

  // 按《B站【大鱼DIY】激光雕刻机_翼宿 V1.0 图纸包》构建真实机台、导轨、电机、外壳与激光头打印件总成
  // 按《B站【大鱼DIY】激光雕刻机_翼宿 V1.0 图纸包》构建真实机台、导轨、电机、外壳与激光头打印件总成
  useEffect(() => {
    rebuildMachineScene(sceneRef.current, {
      xMax,
      yMax,
      zMax,
      matWidth,
      matHeight,
      matThick,
      matName,
      showEnclosure: showEnclosureRef.current,
      ghostTarget: ghostTargetRef.current,
      offsets: latestRef.current,
    });
    // 偏置不在依赖里：只改板材 / 雕刻起点时由下一个 effect 平移对象，不重建机台
  }, [xMax, yMax, zMax, matWidth, matHeight, matThick, matName]);

  // 板材起点与雕刻起点变化：只平移相关对象（2026-11-05）
  useEffect(() => {
    placeOffsetObjects(sceneRef.current, { offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, matWidth, matHeight, matThick });
  }, [offsetX, offsetY, toolpathOffsetX, toolpathOffsetY, matWidth, matHeight, matThick]);
  // 构建三维刀路轨迹（2026-11-07 视口改版）：待雕刻为半透青色细线，已雕刻由板面烧灼痕迹表现
  // （无烧灼画布时回退为按功率着色的线），G0 空移为紫色虚线 + 方向箭头，当前位置带一段亮色尾迹
  useEffect(() => {
    rebuildToolpathLines(sceneRef.current, {
      toolpath,
      matThick,
      showRapid: latestRef.current.showRapid,
    });
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

    // 实机遥测回传的 MPos/WPos 已是机台真实物理坐标，不再二次乘虚拟缩放或叠加虚拟木牌偏置；
    // 离线推演与纯断点行号映射才是相对木牌左下角的刀路局部坐标。
    const isDirectHardwareCoord = Boolean(pose?.isLiveHardware && String(pose?.sourceLabel || "").includes("坐标"));
    const localX = isDirectHardwareCoord ? (Number(pose?.x) || 0) : (Number(pose?.x) || 0) * toolpathScaleX;
    const localY = isDirectHardwareCoord ? (Number(pose?.y) || 0) : (Number(pose?.y) || 0) * toolpathScaleY;
    const pz = Number(pose?.z) || 0;
    const px = clampCoord(isDirectHardwareCoord ? localX : localX + toolpathOffsetX, -10, xMax + 10);
    const py = clampCoord(isDirectHardwareCoord ? localY : localY + toolpathOffsetY, -10, yMax + 10);
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
