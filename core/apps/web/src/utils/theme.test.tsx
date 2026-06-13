import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyTheme, getStoredTheme, initTheme, setStoredTheme } from "./theme";

const THEME_STORAGE_KEY = "ctx.theme.mode";
const originalMatchMedia = window.matchMedia;

const resetTheme = () => {
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.removeAttribute("data-color-scheme");
  window.localStorage.removeItem(THEME_STORAGE_KEY);
};

describe("theme utilities", () => {
  beforeEach(() => {
    resetTheme();
  });

  afterEach(() => {
    resetTheme();
    Object.defineProperty(window, "matchMedia", {
      value: originalMatchMedia,
      writable: true,
      configurable: true,
    });
  });

  it("persists explicit themes and rehydrates from storage", () => {
    applyTheme("dark");
    setStoredTheme("dark");
    expect(getStoredTheme()).toBe("dark");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");

    document.documentElement.removeAttribute("data-theme");
    expect(initTheme()).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");

    applyTheme("light");
    setStoredTheme("light");
    expect(getStoredTheme()).toBe("light");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("light");

    document.documentElement.removeAttribute("data-theme");
    expect(initTheme()).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  });

  it("persists system preference and resolves via matchMedia", () => {
    Object.defineProperty(window, "matchMedia", {
      value: vi.fn().mockImplementation((query: string) => ({
        matches: false,
        media: query,
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
      writable: true,
      configurable: true,
    });

    applyTheme("system");
    setStoredTheme("system");
    expect(getStoredTheme()).toBe("system");
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe("system");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");

    document.documentElement.removeAttribute("data-theme");
    expect(initTheme()).toBe("system");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");

    Object.defineProperty(window, "matchMedia", {
      value: vi.fn().mockImplementation((query: string) => ({
        matches: true,
        media: query,
        onchange: null,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
        addListener: vi.fn(),
        removeListener: vi.fn(),
        dispatchEvent: vi.fn(),
      })),
      writable: true,
      configurable: true,
    });

    applyTheme("system");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });
});
