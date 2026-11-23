import React, { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { LayoutGrid, Lock, RotateCcw, Unlock } from "lucide-react";
import {
  COL_GAP, COLS, DEFAULT_LAYOUT, MIN_COLS, MIN_ROWS, ROW_GAP, ROW_UNIT, STORAGE_KEY,
  clearLayout, clamp, readLayout, reflowVertical, settle, stackTwoColumns, writeLayout,
} from "../workbenchLayout.js";

/**
 * 工作台自由画布（2026-10-18 用户决定）：每张卡有独立的栅格位置与宽高，可拖动、可缩放、可跨列。
 *
 * - 12 列栅格（列是 1fr），窗口变化时按比例重排；卡片位置都吸附到整格。
 * - 拖动卡片顶部移动；卡片四周与四角的 8 个把手改大小（n/s/e/w + 四个角）。
 * - 与其它卡相交就把它往下推（不允许重叠）；松手写进 localStorage。
 * - 卡片折叠时高度收到标题条那么高，展开恢复用户拖过的行数。
 * - `locked` 打开后禁用拖动与缩放；「还原默认布局」清掉记忆回默认排布。
 * - 窄窗口（761–1200px）自动排成两栏（`stackTwoColumns`，只用于显示、不写回记忆，期间暂停拖动）；
 *   两栏时只能拖上下边框调高度；手机宽度（≤ 760px）由 CSS 退回单列堆叠（2026-11-07 用户决定“可以并排显示 2 个，没必要做竖排”）。
 * 纯逻辑（栅格、让位、读写记忆）在 `src/workbenchLayout.js`，单独有单元测试。
 */

const ROWS_COLLAPSED = 8; // 折叠后只留标题条（8 行 ≈ 88px）
const MIN_MONITOR_ROWS = 36; // 监视舱自动对齐左栏时的最小行数（未生成轨迹时空态更紧凑，首屏即可见主按钮）
const HANDLES = ["n", "s", "e", "w", "ne", "nw", "se", "sw"];
/**
 * 双栏默认排布下（左栏 link/source/material，右栏 monitor/feedback），
 * 若用户未手动拖拽过 monitor 高度且 monitor 未折叠，让 monitor 自动伸缩以对齐左右两栏底边，
 * 并以内部最小内容高度为下限，防止展开两点对位或生成多条提示时向下遮挡加工反馈卡。
 */
function alignDualColumns(layout, manualHeightSet, monitorMinRows = MIN_MONITOR_ROWS) {
  const { link, source, material, monitor, feedback } = layout;
  if (!link || !source || !material || !monitor || !feedback) return layout;
  if (manualHeightSet.has("monitor") || monitor.h <= ROWS_COLLAPSED) return layout;
  const floorRows = Math.max(MIN_MONITOR_ROWS, monitorMinRows || MIN_MONITOR_ROWS);
  if (link.x !== 0 || source.x !== 0 || material.x !== 0) {
    return monitor.h < floorRows ? reflowVertical(layout, "monitor", { ...monitor, h: floorRows }) : layout;
  }
  if (monitor.x !== link.w || feedback.x !== link.w || monitor.y !== 0) {
    return monitor.h < floorRows ? reflowVertical(layout, "monitor", { ...monitor, h: floorRows }) : layout;
  }
  const leftBottom = Math.max(link.y + link.h, source.y + source.h, material.y + material.h);
  const idealMonitorH = Math.max(floorRows, clamp(leftBottom - feedback.h - 1, floorRows, 120));
  if (idealMonitorH === monitor.h && feedback.y === monitor.y + monitor.h + 1) return layout;
  return reflowVertical(layout, "monitor", { ...monitor, h: idealMonitorH });
}

function measureMonitorMinHeightPx(contentEl) {
  if (!contentEl) return 0;
  const children = Array.from(contentEl.children);
  if (children.length === 0) return 0;
  let sum = 0;
  for (const child of children) {
    if (child.classList?.contains("chamber-viewport")) {
      sum += child.classList.contains("has-preview") ? 220 : 96;
    } else {
      sum += child.offsetHeight || 0;
    }
  }
  return sum + Math.max(0, children.length - 1) * 14;
}

const WbContext = createContext(null);

/** 与 styles.css 里两栏 / 单列的断点保持一致。 */
export const TWO_COLUMN_QUERY = "(min-width: 761px) and (max-width: 1200px)";
export const SINGLE_COLUMN_QUERY = "(max-width: 760px)";

function useMediaQuery(query) {
  const getMatch = () => typeof window !== "undefined" && typeof window.matchMedia === "function"
    && window.matchMedia(query).matches;
  const [matches, setMatches] = useState(getMatch);
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return undefined;
    const list = window.matchMedia(query);
    const onChange = () => setMatches(list.matches);
    onChange();
    list.addEventListener("change", onChange);
    return () => list.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

export function WorkbenchCanvas({
  children,
  defaultLayout = DEFAULT_LAYOUT,
  storageKey = STORAGE_KEY,
  toolbarStart = null,
  active = true,
}) {
  const stored = useMemo(() => readLayout(globalThis.localStorage, storageKey), [storageKey]);
  const [layout, setLayout] = useState(() => (stored?.cards ? { ...defaultLayout, ...stored.cards } : defaultLayout));
  const [locked, setLocked] = useState(() => Boolean(stored?.locked));
  const canvasRef = useRef(null);
  const metrics = useRef({ colStep: 100, rowStep: ROW_UNIT + ROW_GAP });
  const drag = useRef(null);
  const dragListenersRef = useRef(null);
  const cleanupDragRef = useRef(null);
  const expandedRows = useRef({}); // 折叠前记下的行数，展开时还原
  const manualHeightIds = useRef(new Set()); // 用户在本轮显式上下拖拽过高度的卡片
  const hasStoredBoardRef = useRef(Boolean(stored?.cards?.board));
  const monitorMinRowsRef = useRef(MIN_MONITOR_ROWS);

  useEffect(() => () => {
    dragListenersRef.current?.();
    dragListenersRef.current = null;
    cleanupDragRef.current?.();
    cleanupDragRef.current = null;
    drag.current = null;
  }, []);
  const twoColumn = useMediaQuery(TWO_COLUMN_QUERY);
  const singleColumn = useMediaQuery(SINGLE_COLUMN_QUERY);
  // 窄窗口只换显示位置；layout 本身（含内容自适应行数）照常更新，拉宽窗口后回到用户的排布
  const shown = useMemo(() => (twoColumn ? stackTwoColumns(layout) : layout), [twoColumn, layout]);
  // 两栏时画布换成真正的 2 列网格：48 列网格的 47 道列间距在 960px 窗口下就比画布还宽
  const split = twoColumn ? Object.values(shown).find((box) => box.x === 0 && box.w < COLS)?.w || COLS / 2 : 0;

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
          monitorMinRowsRef.current,
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
        monitorMinRowsRef.current,
      );
    });
  }, []);

  const computeBoardTargetRows = useCallback(() => {
    const defaultBoardH = defaultLayout?.board?.h;
    const element = canvasRef.current;
    if (!defaultBoardH || !element || typeof window === "undefined") return defaultBoardH || 0;
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0) return defaultBoardH;
    const docTop = Math.max(0, rect.top + (window.scrollY || 0));
    const availPx = (window.innerHeight || 900) - docTop - 14;
    const maxFitRows = Math.floor((availPx + ROW_GAP) / (ROW_UNIT + ROW_GAP));
    return clamp(maxFitRows, 36, defaultBoardH);
  }, [defaultLayout]);

  // 创作画板未手动拉高时，将 board 卡片高度钳制在首屏可用高度内，使画布自带的底部缩放/撤销栏与下方快捷操作条同时落在首屏且上下分栏不重叠
  useEffect(() => {
    if (!active || singleColumn || !defaultLayout?.board) return undefined;
    const syncBoardRows = () => {
      if (hasStoredBoardRef.current || manualHeightIds.current.has("board") || drag.current) return;
      const targetRows = computeBoardTargetRows();
      if (!targetRows) return;
      setLayout((current) => {
        const box = current.board;
        if (!box || box.h <= ROWS_COLLAPSED || box.h === targetRows) return current;
        return reflowVertical(current, "board", { ...box, h: targetRows });
      });
    };
    syncBoardRows();
    if (typeof window === "undefined") return undefined;
    window.addEventListener("resize", syncBoardRows);
    return () => window.removeEventListener("resize", syncBoardRows);
  }, [active, singleColumn, defaultLayout, computeBoardTargetRows]);

  /** 表单类卡片根据内部实际内容高度自动贴合栅格行数；monitor 则更新最小内容下限以防溢出遮挡下方卡片。 */
  const fitCardRows = useCallback((id, contentHeightPx, force = false) => {
    if (id === "board") {
      if (force) {
        hasStoredBoardRef.current = false;
        const targetRows = computeBoardTargetRows();
        if (targetRows) {
          setLayout((current) => {
            const box = current.board;
            if (!box || box.h <= ROWS_COLLAPSED || box.h === targetRows) return current;
            return reflowVertical(current, "board", { ...box, h: targetRows });
          });
        }
      }
      return;
    }
    if (!force && manualHeightIds.current.has(id)) return;
    if (drag.current) return;
    const totalPx = Math.max(0, Number(contentHeightPx) || 0) + 38;
    const neededRows = Math.max(MIN_ROWS, Math.ceil((totalPx + ROW_GAP) / (ROW_UNIT + ROW_GAP)));
    if (id === "monitor") {
      monitorMinRowsRef.current = Math.max(MIN_MONITOR_ROWS, neededRows);
      setLayout((current) => alignDualColumns(current, manualHeightIds.current, monitorMinRowsRef.current));
      return;
    }
    setLayout((current) => {
      const box = current[id];
      if (!box || box.h <= ROWS_COLLAPSED) return current;
      const next = box.h === neededRows ? current : reflowVertical(current, id, { ...box, h: neededRows });
      return alignDualColumns(next, manualHeightIds.current, monitorMinRowsRef.current);
    });
  }, [computeBoardTargetRows]);

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
    const minRowsForDrag = state.id === "monitor" ? monitorMinRowsRef.current : MIN_ROWS;
    const minHeightPx = state.isCollapsed
      ? state.size.height
      : Math.max(160, Math.round(minRowsForDrag * rowStep - ROW_GAP));

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
    if (dir.includes("n") && !state.isCollapsed && state.twoColumn) {
      // 两栏：顶边位置固定，往上拖只把卡片变高（与松手后的结果一致）
      height = Math.max(minHeightPx, state.size.height - dy);
    } else if (dir.includes("n") && !state.isCollapsed) {
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
    dragListenersRef.current?.();
    dragListenersRef.current = null;
    drag.current = null;
    document.querySelectorAll(".wb-dragging").forEach((item) => item.classList.remove("wb-dragging"));
    if (!state) return;
    clearPreview(state);
    if (!state.armed) return;
    // 拖完不要顺手把卡片折叠/展开：仅在本次 pointerup 紧随的事件循环内吞掉合成 click，
    // 下一宏任务自动撤除，绝不残留全局拦截器吞掉后续点击（如顶栏急停按钮 #nav-estop）。
    cleanupDragRef.current?.();
    const swallowClick = (event) => {
      event.stopPropagation();
      event.preventDefault();
    };
    document.addEventListener("click", swallowClick, { capture: true, once: true });
    const swallowTimer = window.setTimeout(() => {
      document.removeEventListener("click", swallowClick, { capture: true });
      cleanupDragRef.current = null;
    }, 0);
    cleanupDragRef.current = () => {
      window.clearTimeout(swallowTimer);
      document.removeEventListener("click", swallowClick, { capture: true });
    };
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
      const minRowsAllowed = state.isCollapsed
        ? ROWS_COLLAPSED
        : state.id === "monitor"
          ? monitorMinRowsRef.current
          : MIN_ROWS;
      let { x, y, w, h } = origin;
      if (dir.includes("e")) w = clamp(origin.w + cols, minW, COLS - origin.x);
      if (dir.includes("s") && !state.isCollapsed) h = Math.max(minRowsAllowed, origin.h + rows);
      if (dir.includes("w")) {
        const nextX = clamp(origin.x + cols, 0, origin.x + origin.w - minW);
        w = origin.w + (origin.x - nextX);
        x = nextX;
      }
      if (dir.includes("n") && !state.isCollapsed && state.twoColumn) {
        // 两栏模式下卡片顶边位置由堆叠决定，拖顶边只改高度（往上拖变高）
        h = Math.max(minRowsAllowed, origin.h - rows);
      } else if (dir.includes("n") && !state.isCollapsed) {
        const nextY = clamp(origin.y + rows, 0, origin.y + origin.h - minRowsAllowed);
        h = Math.max(minRowsAllowed, origin.h + (origin.y - nextY));
        y = nextY;
      }
      if ((dir.includes("n") || dir.includes("s")) && h !== origin.h) {
        manualHeightIds.current.add(state.id);
        if (state.id === "board") hasStoredBoardRef.current = true;
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
    if (locked || singleColumn || drag.current) return;
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
    // 窄窗口两栏：位置与宽度由自动排布决定，只允许拖上下边框改高度（2026-11-07 用户反馈画板拉不动上下边框）
    if (twoColumn && mode !== "n" && mode !== "s") return;
    // 收起状态下禁止上下拉高
    if (isCollapsed && (mode.includes("n") || mode.includes("s"))) return;
    if (handle) event.preventDefault();
    drag.current = {
      id, mode, isCollapsed, armed: false, startX: event.clientX, startY: event.clientY,
      origin: { ...box }, layout, element: cardElement, twoColumn,
      size: { width: cardElement.offsetWidth, height: cardElement.offsetHeight },
      dx: 0, dy: 0,
    };
    cardElement.classList.add("wb-dragging");
    const moveListener = onDragMove;
    const endListener = onDragEnd;
    dragListenersRef.current = () => {
      window.removeEventListener("pointermove", moveListener);
      window.removeEventListener("pointerup", endListener);
      window.removeEventListener("pointercancel", endListener);
    };
    window.addEventListener("pointermove", moveListener);
    window.addEventListener("pointerup", endListener, { once: true });
    window.addEventListener("pointercancel", endListener, { once: true });
  };

  const toggleLock = () => {
    const next = !locked;
    setLocked(next);
    writeLayout(layout, next, globalThis.localStorage, storageKey);
  };

  const resetLayout = () => {
    expandedRows.current = {};
    manualHeightIds.current.clear();
    hasStoredBoardRef.current = false;
    monitorMinRowsRef.current = MIN_MONITOR_ROWS;
    let next = { ...defaultLayout };
    if (next.board && !singleColumn) {
      const targetBoardRows = computeBoardTargetRows();
      if (targetBoardRows && targetBoardRows !== next.board.h) {
        next = reflowVertical(next, "board", { ...next.board, h: targetBoardRows });
      }
    }
    const root = canvasRef.current;
    if (root) {
      root.querySelectorAll("[data-wb-card]").forEach((cardEl) => {
        const id = cardEl.dataset.wbCard;
        if (!id || id === "board" || !next[id]) return;
        const header = cardEl.querySelector(".ob-card-h");
        if (header?.dataset.open === "false") {
          expandedRows.current[id] = next[id].h;
          next = reflowVertical(next, id, { ...next[id], h: ROWS_COLLAPSED });
          return;
        }
        const contentEl = cardEl.querySelector(".wb-card-content");
        if (id === "monitor") {
          const minPx = measureMonitorMinHeightPx(contentEl);
          if (minPx > 0) {
            monitorMinRowsRef.current = Math.max(
              MIN_MONITOR_ROWS,
              Math.ceil((minPx + 38 + ROW_GAP) / (ROW_UNIT + ROW_GAP)),
            );
          }
          return;
        }
        const h = contentEl?.offsetHeight || 0;
        if (h > 0) {
          const neededRows = Math.max(MIN_ROWS, Math.ceil((h + 38 + ROW_GAP) / (ROW_UNIT + ROW_GAP)));
          next = reflowVertical(next, id, { ...next[id], h: neededRows });
        }
      });
      next = alignDualColumns(next, manualHeightIds.current, monitorMinRowsRef.current);
    }
    setLayout(next);
    clearLayout(globalThis.localStorage, storageKey);
    if (typeof window !== "undefined" && typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(() => window.dispatchEvent(new Event("resize")));
    }
  };

  const cardProps = useCallback((id) => ({
    style: shown[id]
      ? {
          gridColumn: twoColumn
            ? (shown[id].w >= COLS ? "1 / -1" : shown[id].x === 0 ? "1" : "2")
            : `${shown[id].x + 1} / span ${shown[id].w}`,
          gridRow: `${shown[id].y + 1} / span ${shown[id].h}`,
        }
      : undefined,
  }), [shown, twoColumn]);

  const context = useMemo(
    // 两栏模式只保留上下边框把手（调高度），单列模式禁用把手与拖拽
    () => ({ cardProps, setCollapsed, fitCardRows, clearManualHeight, locked, twoColumn, singleColumn }),
    [cardProps, setCollapsed, fitCardRows, clearManualHeight, locked, twoColumn, singleColumn],
  );

  const [menuOpen, setMenuOpen] = useState(false);
  const menuWrapRef = useRef(null);
  const menuTriggerRef = useRef(null);
  const [portalTarget, setPortalTarget] = useState(null);

  useEffect(() => {
    if (!active || typeof document === "undefined") {
      setPortalTarget(null);
      setMenuOpen(false);
      return;
    }
    setPortalTarget(document.getElementById("ob-top-page-actions"));
  }, [active]);

  useEffect(() => {
    if (!menuOpen || typeof document === "undefined") return undefined;
    const onPointerDown = (event) => {
      if (!menuWrapRef.current?.contains(event.target)) {
        setMenuOpen(false);
      }
    };
    const onKeyDown = (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setMenuOpen(false);
        menuTriggerRef.current?.focus?.();
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [menuOpen]);

  const hintText = singleColumn
    ? "当前窄屏已自动排成单列；拉宽窗口可自由拖动与缩放卡片"
    : twoColumn
      ? "窗口较窄已自动排成两栏；可拖上下边框调高度，拉宽窗口可自由移动卡片"
      : locked
        ? "当前已锁定卡片位置；解锁后可拖标题移动、拖边框缩放"
        : "拖标题移动 · 拖边框缩放";

  const layoutMenuNode = (
    <div className="wb-layout-menu-wrap" ref={menuWrapRef}>
      <button
        ref={menuTriggerRef}
        type="button"
        className={`btn btn-ghost wb-layout-trigger${locked ? " is-locked" : ""}`}
        id="wb-layout-menu-btn"
        aria-expanded={menuOpen}
        aria-controls="wb-layout-menu"
        title="布局：锁定/解锁卡片拖动、还原默认排布"
        onClick={() => setMenuOpen((open) => !open)}
      >
        {locked ? <Lock size={13} aria-hidden="true" /> : <LayoutGrid size={13} aria-hidden="true" />}
        <span>布局</span>
      </button>
      {menuOpen && (
        <div className="wb-layout-popover" id="wb-layout-menu" role="group" aria-label="页面布局选项">
          <p className="wb-layout-popover-hint">
            <LayoutGrid size={13} aria-hidden="true" />
            <span>{hintText}</span>
          </p>
          {!singleColumn && (
            <button
              type="button"
              className="btn btn-ghost wb-layout-popover-btn"
              onClick={toggleLock}
              id="wb-layout-lock"
              title={locked ? "解锁后可以拖动卡片" : "锁定后不会误拖"}
            >
              {locked ? <Lock size={13} /> : <Unlock size={13} />}
              <span>{locked ? "布局已锁定（点击解锁）" : "锁定布局"}</span>
            </button>
          )}
          <button
            type="button"
            className="btn btn-ghost wb-layout-popover-btn"
            onClick={() => {
              resetLayout();
              setMenuOpen(false);
            }}
            id="wb-layout-reset"
            title="清掉记下的位置与大小，回到默认排布"
          >
            <RotateCcw size={13} />
            <span>还原默认布局</span>
          </button>
        </div>
      )}
    </div>
  );

  return (
    <WbContext.Provider value={context}>
      {active && portalTarget ? createPortal(layoutMenuNode, portalTarget) : null}
      {(toolbarStart || (!portalTarget && active)) && (
        <div className="ob-canvas-tools">
          {toolbarStart && <div className="ob-canvas-tools-start">{toolbarStart}</div>}
          {!portalTarget && active ? layoutMenuNode : null}
        </div>
      )}
      <div
        className={`ob-canvas${locked || twoColumn || singleColumn ? " ob-canvas--locked" : ""}${twoColumn ? " ob-canvas--two" : ""}`}
        ref={canvasRef}
        style={twoColumn ? { gridTemplateColumns: `minmax(0, ${split}fr) minmax(0, ${COLS - split}fr)` } : undefined}
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
  const twoColumn = Boolean(context?.twoColumn);
  const singleColumn = Boolean(context?.singleColumn);
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
        const h = id === "monitor" ? measureMonitorMinHeightPx(contentRef.current) : contentRef.current.offsetHeight;
        fitCardRows?.(id, h);
      }
    };
    sync();
    const observer = new MutationObserver(sync);
    observer.observe(header, { attributes: true, attributeFilter: ["data-open"] });
    return () => observer.disconnect();
  }, [id, setCollapsed, fitCardRows]);

  // 监听卡片内部真实内容高度变化（切换模式、生成轨迹、展开两点对位、宽度换行等），自动贴合高度或推开下方卡片
  useEffect(() => {
    const contentEl = contentRef.current;
    if (!contentEl || id === "board") return undefined;
    const measureContent = () => {
      const header = rootRef.current?.querySelector(".ob-card-h");
      if (header?.dataset.open === "false") return;
      const h = id === "monitor" ? measureMonitorMinHeightPx(contentEl) : contentEl.offsetHeight;
      if (h > 0) {
        fitCardRows?.(id, h);
      }
    };
    measureContent();
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(measureContent) : null;
    if (ro) {
      ro.observe(contentEl);
      if (id === "monitor") {
        Array.from(contentEl.children).forEach((child) => ro.observe(child));
      }
    }
    const mo = id === "monitor" && typeof MutationObserver !== "undefined"
      ? new MutationObserver(() => {
          measureContent();
          if (ro) Array.from(contentEl.children).forEach((child) => ro.observe(child));
        })
      : null;
    mo?.observe(contentEl, { childList: true, subtree: true, attributes: true, attributeFilter: ["open"] });
    return () => {
      ro?.disconnect();
      mo?.disconnect();
    };
  }, [id, fitCardRows]);

  const handleResizeDoubleClick = useCallback((dir) => {
    if (dir.includes("n") || dir.includes("s")) {
      clearManualHeight?.(id);
      if (contentRef.current) {
        const h = id === "monitor" ? measureMonitorMinHeightPx(contentRef.current) : contentRef.current.offsetHeight;
        fitCardRows?.(id, h, true);
      }
    }
  }, [id, clearManualHeight, fitCardRows]);

  const activeHandles = singleColumn ? [] : twoColumn ? (collapsed ? [] : ["n", "s"]) : collapsed ? ["e", "w"] : HANDLES;

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
