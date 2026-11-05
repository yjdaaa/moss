import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { clearAndDispose, disposeObjectResources } from "../src/threeDispose.js";

function resource(name, log) {
  return { name, dispose: () => log.push(name) };
}
function node(props = {}, children = []) {
  return {
    ...props,
    children,
    traverse(fn) { fn(this); this.children.forEach((child) => child.traverse(fn)); },
    clear() { this.children = []; return this; },
  };
}

test("递归释放几何体、材质与贴图；共享材质只释放一次", () => {
  const log = [];
  const shared = resource("mat-shared", log);
  const texture = { isTexture: true, ...resource("tex", log) };
  const matWithMap = { ...resource("mat-map", log), map: texture };
  const edges = node({ geometry: resource("geo-edges", log), material: resource("mat-edges", log) });
  const mesh = node({ geometry: resource("geo-a", log), material: shared }, [edges]);
  const mesh2 = node({ geometry: resource("geo-b", log), material: [shared, matWithMap] });
  const counts = disposeObjectResources(node({}, [mesh, mesh2]));
  assert.deepEqual(counts, { geometries: 3, materials: 3, textures: 1 });
  assert.deepEqual(log.sort(), ["geo-a", "geo-b", "geo-edges", "mat-edges", "mat-map", "mat-shared", "tex"].sort());
});

test("clearAndDispose 先释放再清空，组本身保留可复用", () => {
  const log = [];
  const group = node({}, [node({ geometry: resource("g1", log), material: resource("m1", log) })]);
  const counts = clearAndDispose(group);
  assert.deepEqual(counts, { geometries: 1, materials: 1, textures: 0 });
  assert.equal(group.children.length, 0);
  assert.deepEqual(log, ["g1", "m1"]);
  assert.deepEqual(clearAndDispose(null), { geometries: 0, materials: 0, textures: 0 });
});

test("TwinStage3D：每次重建前释放显存，偏置变化不重建机台，卸载时丢弃 WebGL 上下文", () => {
  const source = readFileSync(new URL("../src/components/TwinStage3D.jsx", import.meta.url), "utf8");
  assert.doesNotMatch(source, /\b\w+Group\.clear\(\)/, "不得再直接 group.clear() 而不释放资源");
  assert.match(source, /clearAndDispose\(toolpathGroup\)/);
  assert.match(source, /renderer\.forceContextLoss/);
  assert.match(source, /disposeObjectResources\(scene\)/);
  const machineDeps = source.match(/\}, \[xMax, yMax, zMax, matWidth, matHeight, matThick, matName\]\);/);
  assert.ok(machineDeps, "机台重建依赖不得包含 offsetX/offsetY/toolpathOffset");
});
