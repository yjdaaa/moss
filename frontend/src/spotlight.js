/**
 * 卡片聚光（曜石设计）：光标在 .hud-card 上移动时写入 --mx / --my，供 CSS 径向渐变使用。
 * 纯视觉，只挂一个文档级监听；减少动态效果时不启用。
 */
export function installCardSpotlight(doc = globalThis.document) {
  if (!doc?.addEventListener) return () => {};
  const reduce = globalThis.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches;
  if (reduce) return () => {};
  const onMove = (event) => {
    const card = event.target?.closest?.(".hud-card");
    if (!card) return;
    const rect = card.getBoundingClientRect();
    card.style.setProperty("--mx", `${event.clientX - rect.left}px`);
    card.style.setProperty("--my", `${event.clientY - rect.top}px`);
  };
  doc.addEventListener("pointermove", onMove, { passive: true });
  return () => doc.removeEventListener("pointermove", onMove);
}
