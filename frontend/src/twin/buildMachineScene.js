import * as THREE from "three";
import { clearAndDispose } from "../threeDispose.js";
import {
  addMeshWithEdges,
  applyCoverVisibility,
  applyGhost,
  burnStyleFor,
  materialColorHex,
  paintMaterialBase,
  placeOffsetObjects,
  rbox,
  releaseCanvas,
} from "./stageVisuals.js";

/**
 * 按《翼宿 V1.0 图纸包》构建真实三维机台、导轨、电机、外壳、板材烧灼画布与激光头打印件总成（纯 Three.js，无 React 耦合）。
 */
export function rebuildMachineScene(
  ctx,
  { xMax, yMax, zMax, matWidth, matHeight, matThick, matName, showEnclosure, ghostTarget, offsets },
) {
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
  applyCoverVisibility(ctx, showEnclosure);

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
  ctx.ghostTarget = ghostTarget;
  ctx.ghostLevel = ctx.ghostTarget;
  applyGhost(ctx, ctx.ghostLevel);

  // 主光与阴影相机对准机台中心
  ctx.keyLight.target.position.set(cx, 0, cz);
  ctx.keyLight.position.set(cx + 170, 320, cz + 200);
  ctx.rimLight.position.set(cx - 200, 200, cz - 260);

  const { offsetX: ox, offsetY: oy, toolpathOffsetX: tx, toolpathOffsetY: ty } = offsets;
  placeOffsetObjects(ctx, { offsetX: ox, offsetY: oy, toolpathOffsetX: tx, toolpathOffsetY: ty, matWidth, matHeight, matThick });
  // 偏置不在依赖里：只改板材 / 雕刻起点时由下一个 effect 平移对象，不重建机台
}
