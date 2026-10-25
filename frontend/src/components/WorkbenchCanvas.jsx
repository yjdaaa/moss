import React, { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { LayoutGrid, Lock, RotateCcw, Unlock } from "lucide-react";
import {
  COL_GAP, COLS, DEFAULT_LAYOUT, MIN_COLS, MIN_ROWS, ROW_GAP, ROW_UNIT, STORAGE_KEY,
  clearLayout, clamp, readLayout, reflowVertical, settle, writeLayout,
} from "../workbenchLayout.js";

/**
 * 工作台自由画布（2026-10-18 用户决定）：每张卡有独立的栅格位置与宽高，可拖动、可缩放、可跨列。
 *
 * - 12 列栅格（列是 1fr），窗口变化时按比例重排；卡片位置都吸附到整格。
 * - 拖动卡片顶部移动；卡片四周与四角的 8 个把手改大小（n/s/e/w + 四个角）。
 * - 与其它卡相交就把它往下推（不允许重叠）；松手写进 localStorage。
 * - 卡片折叠时高度收到标题条那么高，展开恢复用户拖过的行数。
 * - `locked` 打开后禁用拖动与缩放；「还原默认布局」清掉记忆回默认排布。
 * - 窄屏（< 1200px）由 CSS 退回单列堆叠，忽略保存的布局。
 * 纯逻辑（栅格、让位、读写记忆）在 `src/workbenchLayout.js`，单独有单元测试。
 */

const ROWS_COLLAPSED = 8; // 折叠后只留标题条（8 行 ≈ 88px）
const MIN_MONITOR_ROWS = 48; // 监视舱自动对齐左栏时的最小行数（保障内部视口与进度栏完整舒展）
const HANDLES = ["n", "s", "e", "w", "ne", "nw", "se", "sw"];
// 内部带弹性画布视口的卡片（多余高度由内部视口吸收），其余表单卡片默认紧贴内容高度不留大块空白
const STRETCH_VIEWPORT_CARDS = new Set(["monitor", "board"]);

/**
 * 双栏默认排布下（左栏 link/source/material，右栏 monitor/feedback），
 * 若用户未手动拖拽过 monitor 高度且 monitor 未折叠，让 monitor 自动伸缩以对齐左右两栏底边，消除单侧留白。
 */
function alignDualColumns(layout, manualHeightSet) {
  const { link, source, material, monitor, feedback } = layout;
  if (!link || !source || !material || !monitor || !feedback) return layout;
  if (manualHeightSet.has("monitor") || monitor.h <= ROWS_COLLAPSED) return layout;
  if (link.x !== 0 || source.x !== 0 || material.x !== 0) return layout;
  if (monitor.x !== link.w || feedback.x !== link.w || monitor.y !== 0) return layout;
  const leftBottom = Math.max(link.y + link.h, source.y + source.h, material.y + material.h);
  const idealMonitorH = clamp(leftBottom - feedback.h - 1, MIN_MONITOR_ROWS, 80);
  if (idealMonitorH === monitor.h && feedback.y === monitor.y + monitor.h + 1) return layout;
  return reflowVertical(layout, "monitor", { ...monitor, h: idealMonitorH });
}

const WbContext = createContext(null);

export function WorkbenchCanvas({
  children,
  defaultLayout = DEFAULT_LAYOUT,
  storageKey = STORAGE_KEY,
}) {
  const stored = useMemo(() => readLayout(globalThis.localStorage, storageKey), [storageKey]);
  const [layout, setLayout] = useState(() => (stored?.cards ? { ...defaultLayout, ...stored.cards } : defaultLayout));
  const [locked, setLocked] = useState(() => Boolean(stored?.locked));
  const canvasRef = useRef(null);
  const metrics = useRef({ colStep: 100, rowStep: ROW_UNIT + ROW_GAP });
  const drag = useRef(null);
  const expandedRows = useRef({}); // 折叠前记下的行数，展开时还原
  const manualHeightIds = useRef(new Set()); // 用户在本轮显式上下拖拽过高度的卡片

  // 列宽随容器变化：拖拽时要把像素位移换算成格子，必须用实时宽度
  useEffect(() => {
    const element = canvasRef.current;
    if (!element || typeof ResizeObserver === "undefined") return undefined;
    const measure = () => {
      const width = element.clientWidth || 1;
      const colStep = (width - COL_GAP * (COLS - 1)) / COLS + COL_GAP;
      metrics.current = { colStep: Math.max(24, colStep), rowStep: ROW_UNIT + ROW_GAP };
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  /** 折叠：高度换成标题条并让下方卡片向上贴合；展开：还原用户拖过的行数并推开下方卡片。 */
  const setCollapsed = useCallback((id, collapsed) => {
    setLayout((current) => {
      const box = current[id];
      if (!box) return current;
      if (collapsed) {
        if (box.h <= ROWS_COLLAPSED) return current; // 已经是收起高度
        expandedRows.current[id] = box.h;
        return alignDualColumns(
          reflowVertical(current, id, { ...box, h: ROWS_COLLAPSED }),
          manualHeightIds.current,
        );
      }
      // 没有折叠记录就什么都不做：挂载时 FoldHeader 也会触发一次 sync
      const saved = expandedRows.current[id];
      if (!saved) return current;
      delete expandedRows.current[id];
      const restore = Math.max(MIN_ROWS, saved);
      if (box.h === restore) return current;
      return alignDualColumns(
        reflowVertical(current, id, { ...box, h: restore }),
        manualHeightIds.current,
      );
    });
  }, []);

  /** 表单类卡片根据内部实际内容高度自动贴合栅格行数，消除卡片底部的大片空白框。 */
  const fitCardRows = useCallback((id, contentHeightPx, force = false) => {
    if (STRETCH_VIEWPORT_CARDS.has(id)) return;
    if (!force && manualHeightIds.current.has(id)) return;
    if (drag.current) return;
    const totalPx = Math.max(0, Number(contentHeightPx) || 0) + 38;
    const neededRows = Math.max(MIN_ROWS, Math.ceil((totalPx + ROW_GAP) / (ROW_UNIT + ROW_GAP)));
    setLayout((current) => {
      const box = current[id];
      if (!box || box.h <= ROWS_COLLAPSED) return current;
      const next = box.h === neededRows ? current : reflowVertical(current, id, { ...box, h: neededRows });
      return alignDualColumns(next, manualHeightIds.current);
    });
  }, []);

  const clearManualHeight = useCallback((id) => {
    manualHeightIds.current.delete(id);
  }, []);

  /** 拖动过程按像素跟手并钳制在画布合法边界与最小尺寸内，消除松手突兀回弹。 */
  const previewDrag = (state, dx, dy) => {
    const element = state.element;
    if (!element) return;
    const { colStep, rowStep } = metrics.current;
    const minWCols = MIN_COLS[state.id] || 6;
    const minWidthPx = Math.max(120, Math.round(minWCols * colStep - COL_GAP));
    const maxRightPx = Math.max(minWidthPx, Math.round((COLS - state.origin.x) * colStep - COL_GAP));
    const minHeightPx = state.isCollapsed
      ? state.size.height
      : Math.max(160, Math.round(MIN_ROWS * rowStep - ROW_GAP));

    if (state.mode === "move") {
      const minDx = -Math.round(state.origin.x * colStep);
      const maxDx = Math.round((COLS - state.origin.x - state.origin.w) * colStep);
      const minDy = -Math.round(state.origin.y * rowStep);
      const clampedDx = clamp(dx, minDx, maxDx);
      const clampedDy = Math.max(minDy, dy);
      element.style.transform = `translate(${clampedDx}px, ${clampedDy}px)`;
      return;
    }
    const dir = state.mode;
    let width = state.size.width;
    let height = state.size.height;
    let tx = 0;
    let ty = 0;
    if (dir.includes("e")) {
      width = clamp(state.size.width + dx, minWidthPx, maxRightPx);
    }
    if (dir.includes("s") && !state.isCollapsed) {
      height = Math.max(minHeightPx, state.size.height + dy);
    }
    if (dir.includes("w")) {
      const minLeftDx = -Math.round(state.origin.x * colStep);
      const maxLeftDx = Math.max(0, state.size.width - minWidthPx);
      const clampedLeftDx = clamp(dx, minLeftDx, maxLeftDx);
      width = state.size.width - clampedLeftDx;
      tx = clampedLeftDx;
    }
    if (dir.includes("n") && !state.isCollapsed) {
      const minTopDy = -Math.round(state.origin.y * rowStep);
      const maxTopDy = Math.max(0, state.size.height - minHeightPx);
      const clampedTopDy = clamp(dy, minTopDy, maxTopDy);
      height = state.size.height - clampedTopDy;
      ty = clampedTopDy;
    }
    element.style.width = `${width}px`;
    element.style.height = `${height}px`;
    element.style.transform = `translate(${tx}px, ${ty}px)`;
  };

  const clearPreview = (state) => {
    const element = state && state.element;
    if (!element) return;
    element.style.transform = "";
    element.style.width = "";
    element.style.height = "";
  };

  const onDragMove = (event) => {
    const state = drag.current;
    if (!state) return;
    const dx = event.clientX - state.startX;
    const dy = event.clientY - state.startY;
    // 只有真的移动了才进入拖动，避免抢掉折叠头/按钮的点击
    if (!state.armed && Math.abs(dx) < 4 && Math.abs(dy) < 4) return;
    state.armed = true;
    state.dx = dx;
    state.dy = dy;
    previewDrag(state, dx, dy);
  };

  const onDragEnd = () => {
    const state = drag.current;
    window.removeEventListener("pointermove", onDragMove);
    drag.current = null;
    document.querySelectorAll(".wb-dragging").forEach((item) => item.classList.remove("wb-dragging"));
    if (!state) return;
    clearPreview(state);
    if (!state.armed) return;
    // 拖完不要顺手把卡片折叠/展开：吞掉这一次 click
    document.addEventListener("click", (event) => {
      event.stopPropagation();
      event.preventDefault();
    }, { capture: true, once: true });
    const { colStep, rowStep } = metrics.current;
    const cols = Math.round(state.dx / colStep);
    const rows = Math.round(state.dy / rowStep);
    const origin = state.origin;
    const minW = MIN_COLS[state.id] || 6;
    let box;
    if (state.mode === "move") {
      box = {
        x: clamp(origin.x + cols, 0, COLS - origin.w),
        y: Math.max(0, origin.y + rows),
        w: origin.w,
        h: origin.h,
      };
    } else {
      const dir = state.mode;
      const minRowsAllowed = state.isCollapsed ? ROWS_COLLAPSED : MIN_ROWS;
      let { x, y, w, h } = origin;
      if (dir.includes("e")) w = clamp(origin.w + cols, minW, COLS - origin.x);
      if (dir.includes("s") && !state.isCollapsed) h = Math.max(minRowsAllowed, origin.h + rows);
      if (dir.includes("w")) {
        const nextX = clamp(origin.x + cols, 0, origin.x + origin.w - minW);
        w = origin.w + (origin.x - nextX);
        x = nextX;
      }
      if (dir.includes("n") && !state.isCollapsed) {
        const nextY = clamp(origin.y + rows, 0, origin.y + origin.h - minRowsAllowed);
        h = Math.max(minRowsAllowed, origin.h + (origin.y - nextY));
        y = nextY;
      }
      if ((dir.includes("n") || dir.includes("s")) && h !== origin.h) {
        manualHeightIds.current.add(state.id);
      }
      box = { x, y, w, h };
    }
    const settled = state.mode !== "move" && box.x === origin.x && box.w === origin.w && box.y === origin.y
      ? reflowVertical(state.layout, state.id, box)
      : settle({ ...state.layout, [state.id]: box }, state.id);
    setLayout(settled);
    writeLayout(settled, locked, globalThis.localStorage, storageKey);
    if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(() => window.dispatchEvent(new Event("resize")));
    }
  };

  const startDrag = (event) => {
    if (locked || drag.current) return;
    const cardElement = event.target.closest?.("[data-wb-card]");
    if (!cardElement || !canvasRef.current?.contains(cardElement)) return;
    const handle = event.target.closest?.("[data-wb-resize]");
    if (!handle && event.target.closest("input, select, textarea, a, label, .excalidraw, .ob-board-stage")) return;
    const id = cardElement.dataset.wbCard;
    const box = layout[id];
    if (!box) return;
    const isCollapsed = cardElement.dataset.wbCollapsed === "true" || box.h <= ROWS_COLLAPSED;
    const rect = cardElement.getBoundingClientRect();
    const inHeader = event.clientY - rect.top < 58;
    const mode = handle ? handle.dataset.wbResize : inHeader ? "move" : "";
    if (!mode) return;
    // 收起状态下禁止上下拉高
    if (isCollapsed && (mode.includes("n") || mode.includes("s"))) return;
    if (handle) event.preventDefault();
    drag.current = {
      id, mode, isCollapsed, armed: false, startX: event.clientX, startY: event.clientY,
      origin: { ...box }, layout, element: cardElement,
      size: { width: cardElement.offsetWidth, height: cardElement.offsetHeight },
      dx: 0, dy: 0,
    };
    cardElement.classList.add("wb-dragging");
    window.addEventListener("pointermove", onDragMove);
    window.addEventListener("pointerup", onDragEnd, { once: true });
  };

  const toggleLock = () => {
    const next = !locked;
    setLocked(next);
    writeLayout(layout, next, globalThis.localStorage, storageKey);
  };

  const resetLayout = () => {
    expandedRows.current = {};
    manualHeightIds.current.clear();
    let next = { ...defaultLayout };
    const root = canvasRef.current;
    if (root) {
      root.querySelectorAll("[data-wb-card]").forEach((cardEl) => {
        const id = cardEl.dataset.wbCard;
        if (!id || STRETCH_VIEWPORT_CARDS.has(id) || !next[id]) return;
        const header = cardEl.querySelector(".ob-card-h");
        if (header?.dataset.open === "false") {
          expandedRows.current[id] = next[id].h;
          next = reflowVertical(next, id, { ...next[id], h: ROWS_COLLAPSED });
          return;
        }
        const contentEl = cardEl.querySelector(".wb-card-content");
        const h = contentEl?.offsetHeight || 0;
        if (h > 0) {
          const neededRows = Math.max(MIN_ROWS, Math.ceil((h + 38 + ROW_GAP) / (ROW_UNIT + ROW_GAP)));
          next = reflowVertical(next, id, { ...next[id], h: neededRows });
        }
      });
      next = alignDualColumns(next, manualHeightIds.current);
    }
    setLayout(next);
    clearLayout(globalThis.localStorage, storageKey);
    if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(() => window.dispatchEvent(new Event("resize")));
    }
  };

  const cardProps = useCallback((id) => ({
    style: layout[id]
      ? {
          gridColumn: `${layout[id].x + 1} / span ${layout[id].w}`,
          gridRow: `${layout[id].y + 1} / span ${layout[id].h}`,
        }
      : undefined,
  }), [layout]);

  const context = useMemo(
    () => ({ cardProps, setCollapsed, fitCardRows, clearManualHeight, locked }),
    [cardProps, setCollapsed, fitCardRows, clearManualHeight, locked],
  );

  return (
    <WbContext.Provider value={context}>
      <div className="ob-canvas-tools">
        <span className="ob-canvas-hint">
          <LayoutGrid size={13} aria-hidden="true" /> 拖卡片顶部移动；拖四周或四角的边框改大小
        </span>
        <button type="button" className="btn btn-ghost" onClick={toggleLock} id="wb-layout-lock"
          title={locked ? "解锁后可以拖动卡片" : "锁定后不会误拖"}>
          {locked ? <Lock size={13} /> : <Unlock size={13} />} {locked ? "布局已锁定" : "锁定布局"}
        </button>
        <button type="button" className="btn btn-ghost" onClick={resetLayout} id="wb-layout-reset"
          title="清掉记下的位置与大小，回到默认排布">
          <RotateCcw size={13} /> 还原默认布局
        </button>
      </div>
      <div
        className={`ob-canvas${locked ? " ob-canvas--locked" : ""}`}
        ref={canvasRef}
        onPointerDown={startDrag}
      >
        {children}
      </div>
    </WbContext.Provider>
  );
}

/** 画布里的一个卡片：负责栅格位置、折叠高度、内容自适应高度、八个 resize 把手（收起时仅保留左右宽度把手）。 */
export function WbCard({ id, className, label, children }) {
  const context = useContext(WbContext);
  const rootRef = useRef(null);
  const contentRef = useRef(null);
  const [collapsed, setCollapsedState] = useState(false);
  const style = context ? context.cardProps(id).style : undefined;
  const locked = Boolean(context?.locked);
  const setCollapsed = context?.setCollapsed;
  const fitCardRows = context?.fitCardRows;
  const clearManualHeight = context?.clearManualHeight;

  // FoldHeader 只在自己内部记折叠状态，这里盯它的 data-open，把卡片高度跟着收放并隐藏纵向缩放把手
  useEffect(() => {
    const root = rootRef.current;
    const header = root?.querySelector(".ob-card-h");
    if (!header) return undefined;
    const sync = () => {
      const isClosed = header.dataset.open === "false";
      setCollapsedState(isClosed);
      setCollapsed?.(id, isClosed);
      if (!isClosed && contentRef.current) {
        fitCardRows?.(id, contentRef.current.offsetHeight);
      }
    };
    sync();
    const observer = new MutationObserver(sync);
    observer.observe(header, { attributes: true, attributeFilter: ["data-open"] });
    return () => observer.disconnect();
  }, [id, setCollapsed, fitCardRows]);

  // 监听卡片内部真实内容高度变化（切换模式、生成轨迹、宽度换行等），自动贴合高度不留大块空白
  useEffect(() => {
    const contentEl = contentRef.current;
    if (!contentEl || STRETCH_VIEWPORT_CARDS.has(id)) return undefined;
    const measureContent = () => {
      const header = rootRef.current?.querySelector(".ob-card-h");
      if (header?.dataset.open === "false") return;
      const h = contentEl.offsetHeight;
      if (h > 0) {
        fitCardRows?.(id, h);
      }
    };
    measureContent();
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(measureContent) : null;
    ro?.observe(contentEl);
    return () => ro?.disconnect();
  }, [id, fitCardRows]);

  const handleResizeDoubleClick = useCallback((dir) => {
    if (dir.includes("n") || dir.includes("s")) {
      clearManualHeight?.(id);
      if (contentRef.current) {
        fitCardRows?.(id, contentRef.current.offsetHeight, true);
      }
    }
  }, [id, clearManualHeight, fitCardRows]);

  const activeHandles = collapsed ? ["e", "w"] : HANDLES;

  return (
    <section
      ref={rootRef}
      className={className}
      data-wb-card={id}
      data-wb-collapsed={collapsed ? "true" : "false"}
      style={style}
      aria-label={label}
    >
      {/* 滚动放在内层，卡片边缘留给 resize 把手，避免被滚动条挡住 */}
      <div className="wb-card-body">
        <div className="wb-card-content" ref={contentRef}>
          {children}
        </div>
      </div>
      {!locked && activeHandles.map((dir) => (
        <span
          key={dir}
          className={`wb-handle wb-handle--${dir}`}
          data-wb-resize={dir}
          onDoubleClick={() => handleResizeDoubleClick(dir)}
          aria-hidden="true"
        />
      ))}
    </section>
  );
}
