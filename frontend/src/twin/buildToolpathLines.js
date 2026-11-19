import * as THREE from "three";
import { clearAndDispose } from "../threeDispose.js";
import { donePowerToRgb } from "./stageVisuals.js";

/**
 * 构建三维刀路轨迹、包络虚线框、G0 空移虚线与方向箭头及实时尾迹（纯 Three.js，无 React 耦合）。
 */
export function rebuildToolpathLines(ctx, { toolpath, matThick, showRapid }) {
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
    rapidLine.visible = Boolean(showRapid);
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
    arrowLine.visible = Boolean(showRapid);
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
}
