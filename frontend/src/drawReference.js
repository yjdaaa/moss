/**
 * 摄像头画面 → 创作画板参考底图（2026-11-26）。
 *
 * 小智板载摄像头与激光平台之间没有标定，照片只能当“看着摆位置”的参考：
 * 放进画板后锁定、半透明、垫在最底层，带 `customData.moss_reference`；
 * 生成刀路时前端导出图片不含它，后端 `sketch.rs` 也跳过它——参考底图永远不会被雕刻。
 */
export const DRAW_REFERENCE_EVENT = "moss:draw-reference";

let pending = null;

/** 摄像头页交出一张参考图（只在内存里传递，不写本机存储）。 */
export function offerDrawReference(reference) {
  pending = reference && typeof reference.dataUrl === "string" ? reference : null;
  if (pending && typeof window !== "undefined") window.dispatchEvent(new CustomEvent(DRAW_REFERENCE_EVENT));
}

/** 画板取走待放入的参考图（取一次即清空）。 */
export function takeDrawReference() {
  const out = pending;
  pending = null;
  return out;
}

/** 是否为参考底图元素（不参与雕刻）。 */
export function isDrawReferenceElement(element) {
  return Boolean(element && element.customData?.moss_reference === true);
}

/** 参考图在画布上的显示宽度（画布像素）。 */
export const DRAW_REFERENCE_WIDTH = 480;

/**
 * 参考底图元素的骨架（交给 `convertToExcalidrawElements`）：按摄像头页的镜像 / 旋转 180° 摆正，
 * 锁定、45% 透明度，放在现有内容左上方。
 */
export function drawReferenceSkeleton({ fileId, naturalWidth, naturalHeight, mirror = false, flip180 = false, device = "", at = { x: 0, y: 0 } }) {
  const ratio = naturalWidth > 0 && naturalHeight > 0 ? naturalHeight / naturalWidth : 0.75;
  const width = DRAW_REFERENCE_WIDTH;
  const height = Math.round(width * ratio);
  return {
    type: "image",
    x: at.x,
    y: at.y,
    width,
    height,
    fileId,
    status: "saved",
    locked: true,
    opacity: 45,
    angle: flip180 ? Math.PI : 0,
    scale: [mirror ? -1 : 1, 1],
    customData: { moss_reference: true, source: "camera", device: String(device || "") },
  };
}
