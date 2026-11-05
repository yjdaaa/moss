// three.js 显存释放（2026-11-05）：`group.clear()` 只摘掉子节点，不会释放 GPU 上的几何体、材质与贴图。
// 场景重建前、组件卸载时必须先递归 dispose，否则每次重建都会泄漏显存。
// 纯逻辑（鸭子类型），不直接依赖 three，便于单元测试。

function materialTextures(material) {
  const textures = [];
  for (const value of Object.values(material || {})) {
    if (value && value.isTexture && typeof value.dispose === "function") textures.push(value);
  }
  return textures;
}

/**
 * 递归释放 `root` 及其全部后代引用的几何体、材质与材质贴图（共享资源只释放一次）。
 * 不改动场景结构；返回释放数量便于测试。
 */
export function disposeObjectResources(root) {
  const geometries = new Set();
  const materials = new Set();
  const textures = new Set();
  const visit = (object) => {
    if (!object) return;
    if (object.geometry && typeof object.geometry.dispose === "function") geometries.add(object.geometry);
    const list = Array.isArray(object.material) ? object.material : object.material ? [object.material] : [];
    for (const material of list) {
      if (!material || typeof material.dispose !== "function") continue;
      materials.add(material);
      for (const texture of materialTextures(material)) textures.add(texture);
    }
  };
  if (typeof root?.traverse === "function") {
    root.traverse(visit);
  } else {
    visit(root);
  }
  geometries.forEach((geometry) => geometry.dispose());
  materials.forEach((material) => material.dispose());
  textures.forEach((texture) => texture.dispose());
  return { geometries: geometries.size, materials: materials.size, textures: textures.size };
}

/** 先释放组内全部子节点的 GPU 资源（子节点间共享的材质只释放一次），再清空该组；组本身保留可复用。 */
export function clearAndDispose(group) {
  if (!group) return { geometries: 0, materials: 0, textures: 0 };
  const children = Array.isArray(group.children) ? [...group.children] : [];
  const released = disposeObjectResources({
    traverse(fn) {
      children.forEach((child) => (typeof child?.traverse === "function" ? child.traverse(fn) : fn(child)));
    },
  });
  if (typeof group.clear === "function") group.clear();
  return released;
}
