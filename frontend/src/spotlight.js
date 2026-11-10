/**
 * 卡片聚光（曜石设计）：光标在 .hud-card 上移动时写入 --mx / --my，供 CSS 径向渐变使用。
 * 通过 requestAnimationFrame 合并高回报率鼠标事件，避免每次 pointermove 触发同步强制重排。
 */
export function installCardSpotlight(doc = globalThis.document) {
  if (!doc?.addEventListener) return () => {};
  const reduce = globalThis.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches;
  if (reduce) return () => {};

  const requestFrame = globalThis.requestAnimationFrame?.bind(globalThis);
  const cancelFrame = globalThis.cancelAnimationFrame?.bind(globalThis);
  let rafId = 0;
  let pendingTarget = null;
  let pendingX = 0;
  let pendingY = 0;

  const flush = () => {
    rafId = 0;
    const card = pendingTarget?.closest?.(".hud-card");
    if (!card) return;
    const rect = card.getBoundingClientRect();
    card.style.setProperty("--mx", `${pendingX - rect.left}px`);
    card.style.setProperty("--my", `${pendingY - rect.top}px`);
  };

  const onMove = (event) => {
    pendingTarget = event.target;
    pendingX = event.clientX;
    pendingY = event.clientY;
    if (!requestFrame) {
      flush();
      return;
    }
    if (!rafId) {
      rafId = requestFrame(flush);
    }
  };

  doc.addEventListener("pointermove", onMove, { passive: true });
  return () => {
    if (rafId && cancelFrame) cancelFrame(rafId);
    doc.removeEventListener("pointermove", onMove);
  };
}
