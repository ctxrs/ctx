import { getAppShellKind, type AppShellKind } from "./runtime";

export const APP_HEIGHT_CSS_VAR = "--ctx-app-height";
export const MOBILE_SAFE_AREA_BOTTOM_CSS_VAR = "--ctx-mobile-safe-area-bottom";
export const MOBILE_SHELL_VIEWPORT_CLASS = "ctx-mobile-shell-viewport";
export const DEFAULT_VIEWPORT_CONTENT = "width=device-width, initial-scale=1.0";
export const MOBILE_SHELL_VIEWPORT_CONTENT =
  "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover";
export const MOBILE_KEYBOARD_OPEN_HEIGHT_DELTA_PX = 120;
export const MOBILE_VIEWPORT_BASELINE_WIDTH_TOLERANCE_PX = 40;

let stopActiveRuntime: (() => void) | null = null;

export type MobileViewportKeyboardBaseline = {
  width: number;
  height: number;
};

const getViewportMeta = (doc: Document): HTMLMetaElement | null =>
  doc.querySelector('meta[name="viewport"]');

export const viewportContentForShell = (shellKind: AppShellKind): string =>
  shellKind === "mobile" ? MOBILE_SHELL_VIEWPORT_CONTENT : DEFAULT_VIEWPORT_CONTENT;

export const resolveViewportHeightPx = (win: Window): number | null => {
  const visualViewportHeight = Number(win.visualViewport?.height ?? Number.NaN);
  if (Number.isFinite(visualViewportHeight) && visualViewportHeight > 0) {
    return Math.round(visualViewportHeight);
  }
  const innerHeight = Number(win.innerHeight ?? Number.NaN);
  if (Number.isFinite(innerHeight) && innerHeight > 0) {
    return Math.round(innerHeight);
  }
  return null;
};

const resolveViewportWidthPx = (win: Window): number | null => {
  const visualViewportWidth = Number(win.visualViewport?.width ?? Number.NaN);
  if (Number.isFinite(visualViewportWidth) && visualViewportWidth > 0) {
    return Math.round(visualViewportWidth);
  }
  const innerWidth = Number(win.innerWidth ?? Number.NaN);
  if (Number.isFinite(innerWidth) && innerWidth > 0) {
    return Math.round(innerWidth);
  }
  return null;
};

const applyViewportHeightCssVar = (root: HTMLElement, win: Window) => {
  const viewportHeightPx = resolveViewportHeightPx(win);
  if (viewportHeightPx === null) return;
  root.style.setProperty(APP_HEIGHT_CSS_VAR, `${viewportHeightPx}px`);
};

export const isEditableTextTarget = (target: EventTarget | null): boolean => {
  if (!(target instanceof Element)) return false;
  const editable = target.closest("textarea,input,[contenteditable]");
  if (!editable) return false;
  if (editable instanceof HTMLTextAreaElement) return true;
  if (editable instanceof HTMLInputElement) {
    return !["button", "checkbox", "color", "file", "hidden", "image", "radio", "range", "reset", "submit"].includes(
      editable.type,
    );
  }
  return editable instanceof HTMLElement && editable.isContentEditable;
};

export const resolveMobileViewportKeyboardBaseline = (
  win: Window,
  current: MobileViewportKeyboardBaseline | null,
): MobileViewportKeyboardBaseline | null => {
  const height = resolveViewportHeightPx(win);
  const width = resolveViewportWidthPx(win);
  if (height === null || width === null) return current;
  if (
    current === null ||
    Math.abs(current.width - width) > MOBILE_VIEWPORT_BASELINE_WIDTH_TOLERANCE_PX ||
    height > current.height
  ) {
    return { width, height };
  }
  return current;
};

export const isMobileKeyboardLikelyOpen = (
  win: Window,
  baseline: MobileViewportKeyboardBaseline | null,
  editableFocused: boolean,
): boolean => {
  if (!editableFocused) return false;
  const visualViewportHeight = Number(win.visualViewport?.height ?? Number.NaN);
  const layoutViewportHeight = Number(win.innerHeight ?? Number.NaN);
  if (Number.isFinite(visualViewportHeight) && Number.isFinite(layoutViewportHeight)) {
    if (layoutViewportHeight - visualViewportHeight >= MOBILE_KEYBOARD_OPEN_HEIGHT_DELTA_PX) {
      return true;
    }
  }
  const viewportHeight = resolveViewportHeightPx(win);
  const viewportWidth = resolveViewportWidthPx(win);
  return Boolean(
    baseline &&
      viewportHeight !== null &&
      viewportWidth !== null &&
      Math.abs(baseline.width - viewportWidth) <= MOBILE_VIEWPORT_BASELINE_WIDTH_TOLERANCE_PX &&
      baseline.height - viewportHeight >= MOBILE_KEYBOARD_OPEN_HEIGHT_DELTA_PX,
  );
};

const applyMobileSafeAreaBottomCssVar = (
  root: HTMLElement,
  win: Window,
  mobileShell: boolean,
  baseline: MobileViewportKeyboardBaseline | null,
  editableFocused: boolean,
) => {
  if (!mobileShell) {
    root.style.removeProperty(MOBILE_SAFE_AREA_BOTTOM_CSS_VAR);
    return;
  }
  const safeAreaBottom = isMobileKeyboardLikelyOpen(win, baseline, editableFocused)
    ? "0px"
    : "env(safe-area-inset-bottom, 0px)";
  root.style.setProperty(MOBILE_SAFE_AREA_BOTTOM_CSS_VAR, safeAreaBottom);
};

const resetWindowScroll = (win: Window) => {
  if (win.scrollX === 0 && win.scrollY === 0) return;
  win.scrollTo(0, 0);
};

export const initMobileViewport = (): (() => void) => {
  if (typeof window === "undefined" || typeof document === "undefined") {
    return () => {};
  }

  stopActiveRuntime?.();

  const root = document.documentElement;
  const shellKind = getAppShellKind();
  const mobileShell = shellKind === "mobile";
  root.classList.toggle(MOBILE_SHELL_VIEWPORT_CLASS, mobileShell);
  const viewportMeta = getViewportMeta(document);
  if (viewportMeta) {
    viewportMeta.setAttribute("content", viewportContentForShell(shellKind));
  }
  let keyboardBaseline = resolveMobileViewportKeyboardBaseline(window, null);
  let editableFocused = isEditableTextTarget(document.activeElement);

  const syncViewportLayout = () => {
    keyboardBaseline = resolveMobileViewportKeyboardBaseline(window, keyboardBaseline);
    applyViewportHeightCssVar(root, window);
    applyMobileSafeAreaBottomCssVar(root, window, mobileShell, keyboardBaseline, editableFocused);
    if (mobileShell) {
      resetWindowScroll(window);
    }
  };

  const syncFocusTarget = () => {
    editableFocused = isEditableTextTarget(document.activeElement);
    syncViewportLayout();
  };

  syncViewportLayout();

  document.addEventListener("focusin", syncFocusTarget);
  document.addEventListener("focusout", syncFocusTarget);
  window.addEventListener("resize", syncViewportLayout, { passive: true });
  window.addEventListener("scroll", syncViewportLayout, { passive: true });
  window.visualViewport?.addEventListener("resize", syncViewportLayout);
  window.visualViewport?.addEventListener("scroll", syncViewportLayout);

  const cleanup = () => {
    document.removeEventListener("focusin", syncFocusTarget);
    document.removeEventListener("focusout", syncFocusTarget);
    window.removeEventListener("resize", syncViewportLayout);
    window.removeEventListener("scroll", syncViewportLayout);
    window.visualViewport?.removeEventListener("resize", syncViewportLayout);
    window.visualViewport?.removeEventListener("scroll", syncViewportLayout);
    root.classList.remove(MOBILE_SHELL_VIEWPORT_CLASS);
    root.style.removeProperty(MOBILE_SAFE_AREA_BOTTOM_CSS_VAR);
    if (stopActiveRuntime === cleanup) {
      stopActiveRuntime = null;
    }
  };
  stopActiveRuntime = cleanup;

  return cleanup;
};
