import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";

export type WorkbenchComposerOpenMenuId = "harness" | "model" | "effort" | "verbosity";

export function useWorkbenchComposerFloatingMenu() {
  const [openMenu, setOpenMenu] = useState<WorkbenchComposerOpenMenuId | null>(null);
  const [menuStyle, setMenuStyle] = useState<CSSProperties | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const harnessTriggerRef = useRef<HTMLButtonElement | null>(null);
  const modelTriggerRef = useRef<HTMLButtonElement | null>(null);
  const effortTriggerRef = useRef<HTMLButtonElement | null>(null);
  const verbosityTriggerRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!openMenu) return;
    const onPointerDown = (event: PointerEvent) => {
      const root = rootRef.current;
      if (!root) return;
      const target = event.target as Node;
      if (!root.contains(target)) {
        setOpenMenu(null);
        return;
      }
      const el = event.target as Element | null;
      if (el && (el.closest(".wb-menu") || el.closest(".wb-menu-trigger"))) return;
      setOpenMenu(null);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [openMenu]);

  const getTriggerForMenu = useCallback((id: WorkbenchComposerOpenMenuId): HTMLButtonElement | null => {
    if (id === "harness") return harnessTriggerRef.current;
    if (id === "model") return modelTriggerRef.current;
    if (id === "effort") return effortTriggerRef.current;
    if (id === "verbosity") return verbosityTriggerRef.current;
    return null;
  }, []);

  const recomputeMenuPosition = useCallback(() => {
    if (!openMenu) return;
    const menuEl = menuRef.current;
    const triggerEl = getTriggerForMenu(openMenu);
    if (!menuEl || !triggerEl) return;

    const margin = 10;
    const viewportW = window.innerWidth;
    const viewportH = window.innerHeight;
    const triggerRect = triggerEl.getBoundingClientRect();
    const menuRect = menuEl.getBoundingClientRect();
    const menuW = Math.max(160, menuRect.width);
    const menuH = Math.max(40, menuRect.height);
    let left = triggerRect.left;
    let top = triggerRect.bottom + 8;
    let maxHeight: number | null = null;
    let overflowY: CSSProperties["overflowY"] = "visible";

    if (openMenu === "harness") {
      left = triggerRect.right + 10;
      top = triggerRect.top + triggerRect.height / 2 - menuH / 2;
      if (left + menuW > viewportW - margin) {
        left = Math.max(margin, viewportW - margin - menuW);
      }
      top = Math.max(margin, Math.min(top, viewportH - margin - menuH));
      const maxH = viewportH - margin * 2;
      if (menuH > maxH) {
        top = margin;
        maxHeight = maxH;
        overflowY = "hidden";
      }
    } else {
      const downTop = triggerRect.bottom + 8;
      const availableDown = viewportH - margin - downTop;
      const availableUp = triggerRect.top - margin - 8;
      const shouldOpenUp = availableDown < menuH && availableUp > availableDown;
      if (shouldOpenUp) {
        const maxH = Math.max(120, availableUp);
        const usedH = Math.min(menuH, maxH);
        top = triggerRect.top - 8 - usedH;
        maxHeight = menuH > maxH ? maxH : null;
        overflowY = menuH > maxH ? "auto" : "visible";
      } else {
        top = downTop;
        const maxH = Math.max(120, availableDown);
        maxHeight = menuH > maxH ? maxH : null;
        overflowY = menuH > maxH ? "auto" : "visible";
      }
      if (left + menuW > viewportW - margin) left = viewportW - margin - menuW;
      if (left < margin) left = margin;
      if (top < margin) top = margin;
    }

    setMenuStyle({
      position: "fixed",
      left,
      top,
      maxHeight: maxHeight ?? undefined,
      overflowY,
      visibility: "visible",
    });
  }, [getTriggerForMenu, openMenu]);

  useLayoutEffect(() => {
    if (!openMenu) {
      setMenuStyle(null);
      return;
    }
    setMenuStyle({
      position: "fixed",
      left: 0,
      top: 0,
      maxHeight: undefined,
      overflowY: "visible",
      visibility: "hidden",
    });

    const raf = window.requestAnimationFrame(() => {
      recomputeMenuPosition();
    });

    window.addEventListener("resize", recomputeMenuPosition);
    const onAnyScroll = (event: Event) => {
      const target = event.target as Element | null;
      if (target && typeof target.closest === "function" && target.closest(".wb-menu")) return;
      recomputeMenuPosition();
    };
    window.addEventListener("scroll", onAnyScroll, true);
    return () => {
      window.cancelAnimationFrame(raf);
      window.removeEventListener("resize", recomputeMenuPosition);
      window.removeEventListener("scroll", onAnyScroll, true);
    };
  }, [openMenu, recomputeMenuPosition]);

  return {
    openMenu,
    setOpenMenu,
    menuStyle,
    rootRef,
    menuRef,
    harnessTriggerRef,
    modelTriggerRef,
    effortTriggerRef,
    verbosityTriggerRef,
  };
}
