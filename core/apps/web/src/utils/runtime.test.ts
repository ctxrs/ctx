import { afterEach, describe, expect, it, vi } from "vitest";

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

describe("runtime", () => {
  afterEach(() => {
    vi.resetModules();
    const g = globalThis as typeof globalThis & { __TAURI__?: unknown; __TAURI_INTERNALS__?: unknown };
    delete g.__TAURI__;
    delete g.__TAURI_INTERNALS__;
    setUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    setPlatform("MacIntel");
    setMaxTouchPoints(0);
  });

  it("classifies plain browser windows as web", async () => {
    const mod = await import("./runtime");

    expect(mod.getAppShellKind()).toBe("web");
    expect(mod.isDesktopShellApp()).toBe(false);
    expect(mod.isMobileShellApp()).toBe(false);
  });

  it("classifies tauri desktop windows as desktop", async () => {
    const g = globalThis as typeof globalThis & { __TAURI__?: unknown };
    g.__TAURI__ = {};
    setUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    const mod = await import("./runtime");

    expect(mod.getAppShellKind()).toBe("desktop");
    expect(mod.isDesktopShellApp()).toBe(true);
    expect(mod.isMobileShellApp()).toBe(false);
  });

  it("classifies tauri iPhone windows as mobile", async () => {
    const g = globalThis as typeof globalThis & { __TAURI_INTERNALS__?: unknown };
    g.__TAURI_INTERNALS__ = {};
    setUserAgent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)");
    setPlatform("iPhone");
    const mod = await import("./runtime");

    expect(mod.getAppShellKind()).toBe("mobile");
    expect(mod.isDesktopShellApp()).toBe(false);
    expect(mod.isMobileShellApp()).toBe(true);
  });

  it("treats iPadOS desktop-mode user agents as mobile when touch points are present", async () => {
    const g = globalThis as typeof globalThis & { __TAURI__?: unknown };
    g.__TAURI__ = {};
    setUserAgent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
    setPlatform("MacIntel");
    setMaxTouchPoints(5);
    const mod = await import("./runtime");

    expect(mod.getAppShellKind()).toBe("mobile");
  });
});
