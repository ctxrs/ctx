import { afterEach, describe, expect, it, vi } from "vitest";

type VisualViewportMock = VisualViewport & {
  setHeight: (height: number) => void;
  dispatch: (type: "resize" | "scroll") => void;
};

const setUserAgent = (value: string) => {
  Object.defineProperty(window.navigator, "userAgent", {
    configurable: true,
    value,
  });
};

const setPlatform = (value: string) => {
  Object.defineProperty(window.navigator, "platform", {
    configurable: true,
    value,
  });
};

const setMaxTouchPoints = (value: number) => {
  Object.defineProperty(window.navigator, "maxTouchPoints", {
    configurable: true,
    value,
  });
};

const setInnerHeight = (value: number) => {
  Object.defineProperty(window, "innerHeight", {
    configurable: true,
    value,
  });
};

const setScrollPosition = (x: number, y: number) => {
  Object.defineProperty(window, "scrollX", {
    configurable: true,
    value: x,
  });
  Object.defineProperty(window, "scrollY", {
    configurable: true,
    value: y,
  });
};

const setVisualViewport = (visualViewport: VisualViewportMock | undefined) => {
  Object.defineProperty(window, "visualViewport", {
    configurable: true,
    value: visualViewport,
  });
};

const createVisualViewportMock = (initialHeight: number): VisualViewportMock => {
  let height = initialHeight;
  const listeners = {
    resize: new Set<(event: Event) => void>(),
    scroll: new Set<(event: Event) => void>(),
  };
  type ViewportEventName = keyof typeof listeners;

  return {
    get height() {
      return height;
    },
    addEventListener(type: string, listener: EventListenerOrEventListenerObject | null) {
      if (!listener) return;
      if (type === "resize" || type === "scroll") {
        listeners[type as ViewportEventName].add(listener as (event: Event) => void);
      }
    },
    removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null) {
      if (!listener) return;
      if (type === "resize" || type === "scroll") {
        listeners[type as ViewportEventName].delete(listener as (event: Event) => void);
      }
    },
    setHeight(nextHeight: number) {
      height = nextHeight;
    },
    dispatch(type: "resize" | "scroll") {
      const event = new Event(type);
      for (const listener of listeners[type]) {
        listener(event);
      }
    },
  } as VisualViewportMock;
};

describe("mobileViewport", () => {
  afterEach(() => {
    vi.resetModules();
    document.head.innerHTML = "";
    document.body.innerHTML = "";
    document.documentElement.style.removeProperty("--ctx-app-height");
    document.documentElement.style.removeProperty("--ctx-mobile-safe-area-bottom");
    const globals = globalThis as typeof globalThis & {
      __TAURI__?: unknown;
      __TAURI_INTERNALS__?: unknown;
    };
    delete globals.__TAURI__;
    delete globals.__TAURI_INTERNALS__;
    setUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    setPlatform("MacIntel");
    setMaxTouchPoints(0);
    setInnerHeight(800);
    setScrollPosition(0, 0);
    setVisualViewport(undefined);
  });

  it("locks the viewport and tracks visual viewport height for tauri mobile shells", async () => {
    document.head.innerHTML = '<meta name="viewport" content="width=device-width, initial-scale=1.0" />';
    const globals = globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown };
    globals.__TAURI_INTERNALS__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    const visualViewport = createVisualViewportMock(724);
    setVisualViewport(visualViewport);
    setInnerHeight(812);

    const mod = await import("./mobileViewport");
    const stop = mod.initMobileViewport();

    expect(document.querySelector('meta[name="viewport"]')?.getAttribute("content")).toBe(
      mod.MOBILE_SHELL_VIEWPORT_CONTENT,
    );
    expect(document.documentElement.classList.contains(mod.MOBILE_SHELL_VIEWPORT_CLASS)).toBe(true);
    expect(document.documentElement.style.getPropertyValue(mod.APP_HEIGHT_CSS_VAR)).toBe("724px");
    expect(document.documentElement.style.getPropertyValue(mod.MOBILE_SAFE_AREA_BOTTOM_CSS_VAR)).toBe(
      "env(safe-area-inset-bottom, 0px)",
    );

    const textarea = document.createElement("textarea");
    document.body.append(textarea);
    textarea.focus();
    visualViewport.setHeight(612);
    visualViewport.dispatch("resize");
    expect(document.documentElement.style.getPropertyValue(mod.APP_HEIGHT_CSS_VAR)).toBe("612px");
    expect(document.documentElement.style.getPropertyValue(mod.MOBILE_SAFE_AREA_BOTTOM_CSS_VAR)).toBe("0px");

    stop();
    expect(document.documentElement.classList.contains(mod.MOBILE_SHELL_VIEWPORT_CLASS)).toBe(false);
    expect(document.documentElement.style.getPropertyValue(mod.MOBILE_SAFE_AREA_BOTTOM_CSS_VAR)).toBe("");
  });

  it("keeps the default viewport content and falls back to innerHeight outside the mobile shell", async () => {
    document.head.innerHTML = '<meta name="viewport" content="width=device-width, initial-scale=1.0" />';
    setInnerHeight(900);

    const mod = await import("./mobileViewport");
    const stop = mod.initMobileViewport();

    expect(document.querySelector('meta[name="viewport"]')?.getAttribute("content")).toBe(
      mod.DEFAULT_VIEWPORT_CONTENT,
    );
    expect(document.documentElement.classList.contains(mod.MOBILE_SHELL_VIEWPORT_CLASS)).toBe(false);
    expect(document.documentElement.style.getPropertyValue(mod.APP_HEIGHT_CSS_VAR)).toBe("900px");
    expect(document.documentElement.style.getPropertyValue(mod.MOBILE_SAFE_AREA_BOTTOM_CSS_VAR)).toBe("");

    stop();
  });

  it("drops the mobile safe-area bottom when a focused iOS webview shrinks innerHeight with the visual viewport", async () => {
    document.head.innerHTML = '<meta name="viewport" content="width=device-width, initial-scale=1.0" />';
    const textarea = document.createElement("textarea");
    document.body.append(textarea);
    const globals = globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown };
    globals.__TAURI_INTERNALS__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    const visualViewport = createVisualViewportMock(724);
    setVisualViewport(visualViewport);
    setInnerHeight(724);

    const mod = await import("./mobileViewport");
    const stop = mod.initMobileViewport();

    expect(document.documentElement.style.getPropertyValue(mod.MOBILE_SAFE_AREA_BOTTOM_CSS_VAR)).toBe(
      "env(safe-area-inset-bottom, 0px)",
    );

    textarea.focus();
    setInnerHeight(520);
    visualViewport.setHeight(520);
    visualViewport.dispatch("resize");

    expect(document.documentElement.style.getPropertyValue(mod.APP_HEIGHT_CSS_VAR)).toBe("520px");
    expect(document.documentElement.style.getPropertyValue(mod.MOBILE_SAFE_AREA_BOTTOM_CSS_VAR)).toBe("0px");

    stop();
  });

  it("keeps iOS focus-induced window scroll pinned in mobile shells", async () => {
    document.head.innerHTML = '<meta name="viewport" content="width=device-width, initial-scale=1.0" />';
    const textarea = document.createElement("textarea");
    document.body.append(textarea);
    textarea.focus();
    const globals = globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown };
    globals.__TAURI_INTERNALS__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    const visualViewport = createVisualViewportMock(520);
    setVisualViewport(visualViewport);
    setInnerHeight(812);
    setScrollPosition(0, 96);
    const scrollTo = vi.fn((x: number, y: number) => {
      setScrollPosition(x, y);
    });
    Object.defineProperty(window, "scrollTo", {
      configurable: true,
      value: scrollTo,
    });

    const mod = await import("./mobileViewport");
    const stop = mod.initMobileViewport();

    expect(scrollTo).toHaveBeenCalledWith(0, 0);

    setScrollPosition(0, 48);
    visualViewport.dispatch("scroll");
    expect(scrollTo).toHaveBeenCalledTimes(2);
    expect(scrollTo).toHaveBeenLastCalledWith(0, 0);

    stop();
  });
});
