import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Check } from "lucide-react";

/**
 * 拆分选项文本中的主标题与括号说明，例如：
 * "OneJev 1.6 · 0.8B（极速·推荐）" -> { title: "OneJev 1.6 · 0.8B", badge: "极速·推荐", tone: "accent" }
 */
function parseOptionLabel(rawText) {
  const text = String(rawText || "").trim();
  const match = text.match(/^(.*?)\s*[（(]([^（）()]+)[）)]\s*$/);
  const title = match && match[1].trim() ? match[1].trim() : text;
  const badge = match ? match[2].trim() : "";
  let tone = "neutral";
  if (/Colab|推荐|极速|已开启|接管/.test(text)) {
    tone = "accent";
  } else if (/断网可用|免密钥|本地/.test(badge)) {
    tone = "ok";
  } else if (/未下载|未配置|暂不生效/.test(badge)) {
    tone = "warn";
  }
  return { title, badge, tone };
}

function buildSelectOptions(selectEl) {
  return Array.from(selectEl.options).map((opt, index) => ({
    index,
    value: opt.value,
    label: opt.textContent || opt.label || opt.value,
    disabled: Boolean(opt.disabled),
    ...parseOptionLabel(opt.textContent || opt.label || opt.value),
  }));
}

function isEligibleSelect(el) {
  return Boolean(
    el &&
      el.tagName === "SELECT" &&
      !el.disabled &&
      !el.multiple &&
      !(el.size && el.size > 1),
  );
}

function triggerSelectValue(selectEl, nextValue) {
  if (!selectEl || selectEl.value === nextValue) return;
  const descriptor = Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, "value");
  if (descriptor?.set) {
    descriptor.set.call(selectEl, nextValue);
  } else {
    selectEl.value = nextValue;
  }
  selectEl.dispatchEvent(new Event("input", { bubbles: true }));
  selectEl.dispatchEvent(new Event("change", { bubbles: true }));
}

/**
 * 全局 `<select>` 曜石浮层增强器：
 * - 拦截鼠标与键盘激活原生 `<select>` 弹出的系统灰白框，替换为曜石毛玻璃浮层菜单；
 * - 自动将当前选中项与方向键高亮项滚入可视区，并在底层选项异步更新时实时同步；
 * - 原生 `<select>` DOM 节点、id、value、aria-label 完全保留，自动化测试 `selectOption()` 零影响。
 */
export function ObsidianSelectHost() {
  const [menu, setMenu] = useState(null);
  const popoverRef = useRef(null);
  const activeSelectRef = useRef(null);

  useEffect(() => {
    const closeMenu = () => {
      if (activeSelectRef.current) {
        activeSelectRef.current.removeAttribute("data-ob-open");
        activeSelectRef.current = null;
      }
      setMenu(null);
    };

    const openForSelect = (selectEl) => {
      const rect = selectEl.getBoundingClientRect();
      const options = buildSelectOptions(selectEl);
      const selectedIdx = Math.max(
        0,
        options.findIndex((o) => o.value === selectEl.value),
      );
      const viewportH = window.innerHeight || 800;
      const viewportW = window.innerWidth || 1280;
      const estimatedH = Math.min(300, options.length * 38 + 16);
      const spaceBelow = viewportH - rect.bottom - 12;
      const openUpward = spaceBelow < estimatedH && rect.top > spaceBelow;
      const width = Math.min(Math.max(rect.width, 220), viewportW - 16);
      const left = Math.max(8, Math.min(rect.left, viewportW - width - 8));

      if (activeSelectRef.current && activeSelectRef.current !== selectEl) {
        activeSelectRef.current.removeAttribute("data-ob-open");
      }
      activeSelectRef.current = selectEl;
      selectEl.setAttribute("data-ob-open", "true");

      setMenu({
        selectEl,
        options,
        value: selectEl.value,
        activeIndex: selectedIdx,
        left,
        width,
        top: openUpward ? undefined : rect.bottom + 6,
        bottom: openUpward ? viewportH - rect.top + 6 : undefined,
        maxHeight: Math.max(160, Math.min(310, openUpward ? rect.top - 16 : spaceBelow)),
      });
    };

    const handleMouseDown = (event) => {
      if (event.button !== 0) return;
      if (popoverRef.current && popoverRef.current.contains(event.target)) {
        return;
      }
      const selectEl = event.target?.closest?.("select");
      if (!selectEl) {
        if (activeSelectRef.current) closeMenu();
        return;
      }
      if (!isEligibleSelect(selectEl)) {
        return;
      }
      event.preventDefault();
      selectEl.focus();
      if (activeSelectRef.current === selectEl) {
        closeMenu();
      } else {
        openForSelect(selectEl);
      }
    };

    const handleKeyDown = (event) => {
      if (!activeSelectRef.current) {
        const focusedSelect = event.target?.closest?.("select");
        if (
          isEligibleSelect(focusedSelect) &&
          (event.key === " " ||
            event.key === "Enter" ||
            event.key === "ArrowDown" ||
            event.key === "ArrowUp")
        ) {
          event.preventDefault();
          openForSelect(focusedSelect);
        }
        return;
      }
      if (event.key === "Escape" || event.key === "Tab") {
        closeMenu();
        return;
      }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const dir = event.key === "ArrowDown" ? 1 : -1;
        setMenu((prev) => {
          if (!prev || prev.options.length === 0) return prev;
          let nextIdx = prev.activeIndex;
          for (let i = 0; i < prev.options.length; i += 1) {
            nextIdx = (nextIdx + dir + prev.options.length) % prev.options.length;
            if (!prev.options[nextIdx].disabled) break;
          }
          return { ...prev, activeIndex: nextIdx };
        });
        return;
      }
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        setMenu((prev) => {
          if (!prev) return null;
          const picked = prev.options[prev.activeIndex];
          if (picked && !picked.disabled) {
            triggerSelectValue(prev.selectEl, picked.value);
          }
          if (activeSelectRef.current) {
            activeSelectRef.current.removeAttribute("data-ob-open");
            activeSelectRef.current = null;
          }
          return null;
        });
      }
    };

    const handleScrollOrResize = (event) => {
      if (!activeSelectRef.current) return;
      if (popoverRef.current && event?.target && popoverRef.current.contains(event.target)) {
        return;
      }
      closeMenu();
    };

    const handleExternalChange = (event) => {
      if (activeSelectRef.current && event.target === activeSelectRef.current) {
        closeMenu();
      }
    };

    document.addEventListener("mousedown", handleMouseDown, true);
    document.addEventListener("keydown", handleKeyDown, true);
    window.addEventListener("scroll", handleScrollOrResize, true);
    window.addEventListener("resize", handleScrollOrResize);
    document.addEventListener("change", handleExternalChange, true);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown, true);
      document.removeEventListener("keydown", handleKeyDown, true);
      window.removeEventListener("scroll", handleScrollOrResize, true);
      window.removeEventListener("resize", handleScrollOrResize);
      document.removeEventListener("change", handleExternalChange, true);
    };
  }, []);

  // 菜单展开期间若底层 <select> 的选项异步刷新（如串口扫描、模型列表拉取），实时同步浮层列表
  useEffect(() => {
    const selectEl = menu?.selectEl;
    if (!selectEl || typeof MutationObserver === "undefined") return undefined;
    const observer = new MutationObserver(() => {
      if (!selectEl.isConnected || selectEl.disabled) {
        selectEl.removeAttribute("data-ob-open");
        if (activeSelectRef.current === selectEl) activeSelectRef.current = null;
        setMenu(null);
        return;
      }
      const options = buildSelectOptions(selectEl);
      setMenu((prev) => {
        if (!prev || prev.selectEl !== selectEl) return prev;
        const selectedIdx = options.findIndex((o) => o.value === selectEl.value);
        const clampedActive = Math.min(
          Math.max(0, selectedIdx >= 0 ? selectedIdx : prev.activeIndex),
          Math.max(0, options.length - 1),
        );
        return {
          ...prev,
          options,
          value: selectEl.value,
          activeIndex: clampedActive,
        };
      });
    });
    observer.observe(selectEl, {
      childList: true,
      subtree: true,
      characterData: true,
      attributes: true,
      attributeFilter: ["disabled", "label", "value"],
    });
    return () => observer.disconnect();
  }, [menu?.selectEl]);

  // 展开时与键盘上下移动高亮时，自动将目标选项滚入浮层可视范围
  const activeSelectEl = menu?.selectEl ?? null;
  const activeIdx = menu?.activeIndex ?? -1;
  useLayoutEffect(() => {
    if (!activeSelectEl || activeIdx < 0 || !popoverRef.current) return;
    const activeBtn = popoverRef.current.querySelector(`[data-ob-idx="${activeIdx}"]`);
    activeBtn?.scrollIntoView?.({ block: "nearest" });
  }, [activeSelectEl, activeIdx]);

  if (!menu) return null;

  return (
    <div
      ref={popoverRef}
      className="ob-select-popover"
      role="listbox"
      style={{
        left: `${menu.left}px`,
        width: `${menu.width}px`,
        top: menu.top !== undefined ? `${menu.top}px` : undefined,
        bottom: menu.bottom !== undefined ? `${menu.bottom}px` : undefined,
        maxHeight: `${menu.maxHeight}px`,
      }}
    >
      <div className="ob-select-popover-list">
        {menu.options.map((opt, idx) => {
          const selected = opt.value === menu.value;
          const active = idx === menu.activeIndex;
          return (
            <button
              key={`${opt.index}-${opt.value}`}
              type="button"
              role="option"
              data-ob-idx={idx}
              aria-selected={selected}
              disabled={opt.disabled}
              className={`ob-select-option${selected ? " is-selected" : ""}${active ? " is-active" : ""}`}
              onMouseEnter={() => setMenu((prev) => (prev ? { ...prev, activeIndex: idx } : prev))}
              onMouseDown={(e) => {
                e.preventDefault();
                e.stopPropagation();
                if (opt.disabled) return;
                const targetSelect = menu.selectEl;
                if (activeSelectRef.current) {
                  activeSelectRef.current.removeAttribute("data-ob-open");
                  activeSelectRef.current = null;
                }
                setMenu(null);
                triggerSelectValue(targetSelect, opt.value);
                targetSelect?.focus?.();
              }}
            >
              <span className="ob-select-option-main">
                <span className="ob-select-option-title">{opt.title}</span>
                {opt.badge && (
                  <span className={`ob-select-option-badge ob-select-option-badge--${opt.tone}`}>
                    {opt.badge}
                  </span>
                )}
              </span>
              <span className="ob-select-option-check" aria-hidden="true">
                {selected && <Check size={14} strokeWidth={2.4} />}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
