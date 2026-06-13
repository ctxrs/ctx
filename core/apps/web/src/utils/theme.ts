import { useEffect, useState } from "react";

export type ThemeMode = "system" | "dark" | "light";
export type ThemeVariant = "dark" | "light";

const THEME_STORAGE_KEY = "ctx.theme.mode";
const THEME_ATTRIBUTE = "data-theme";
const THEME_MEDIA_QUERY = "(prefers-color-scheme: dark)";
const DEFAULT_THEME_MODE: ThemeMode = "system";
const DEFAULT_THEME_VARIANT: ThemeVariant = "dark";
let systemThemeMedia: MediaQueryList | null = null;
let systemThemeListener: (() => void) | null = null;

const getThemeMedia = (): MediaQueryList | null => {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return null;
  return window.matchMedia(THEME_MEDIA_QUERY);
};

const stopSystemThemeListener = () => {
  if (systemThemeMedia && systemThemeListener) {
    systemThemeMedia.removeEventListener("change", systemThemeListener);
  }
  systemThemeMedia = null;
  systemThemeListener = null;
};

const setThemeAttribute = (target: HTMLElement, variant: ThemeVariant) => {
  target.setAttribute(THEME_ATTRIBUTE, variant);
};

const resolveSystemVariant = (media: MediaQueryList | null): ThemeVariant => {
  if (!media) return DEFAULT_THEME_VARIANT;
  return media.matches ? "dark" : "light";
};

const startSystemThemeListener = (target: HTMLElement) => {
  const media = getThemeMedia();
  const update = () => {
    setThemeAttribute(target, resolveSystemVariant(media));
  };
  stopSystemThemeListener();
  update();
  if (!media) return;
  systemThemeMedia = media;
  systemThemeListener = update;
  media.addEventListener("change", update);
};

const coerceThemeMode = (value: string | null | undefined): ThemeMode | null =>
  value === "dark" || value === "light" || value === "system" ? value : null;
const coerceThemeVariant = (value: string | null | undefined): ThemeVariant | null =>
  value === "dark" || value === "light" ? value : null;

export const getStoredTheme = (): ThemeMode | null => {
  if (typeof window === "undefined") return null;
  try {
    return coerceThemeMode(window.localStorage.getItem(THEME_STORAGE_KEY));
  } catch {
    return null;
  }
};

export const setStoredTheme = (mode: ThemeMode): void => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, mode);
  } catch {
    // ignore storage failures
  }
};

export const resolveThemeMode = (): ThemeMode => getStoredTheme() ?? DEFAULT_THEME_MODE;

export const getSystemTheme = (): ThemeVariant => {
  return resolveSystemVariant(getThemeMedia());
};

const readThemeAttribute = (target: HTMLElement): ThemeVariant | null =>
  coerceThemeVariant(target.getAttribute(THEME_ATTRIBUTE));

export const resolveThemeVariant = (target?: HTMLElement): ThemeVariant => {
  if (typeof document !== "undefined") {
    const element = target ?? document.documentElement;
    const fromAttr = readThemeAttribute(element);
    if (fromAttr) return fromAttr;
  }
  const stored = resolveThemeMode();
  if (stored === "dark" || stored === "light") return stored;
  return getSystemTheme();
};

export const applyTheme = (mode: ThemeMode, target: HTMLElement = document.documentElement): void => {
  if (mode === "system") {
    startSystemThemeListener(target);
    return;
  }
  stopSystemThemeListener();
  setThemeAttribute(target, mode);
};

export const initTheme = (target?: HTMLElement): ThemeMode => {
  const mode = resolveThemeMode();
  if (typeof document !== "undefined") {
    applyTheme(mode, target ?? document.documentElement);
  }
  return mode;
};

export const readCssVar = (name: string, fallback = ""): string => {
  if (typeof window === "undefined" || typeof document === "undefined") return fallback;
  try {
    const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return value || fallback;
  } catch {
    return fallback;
  }
};

const clampAlpha = (value: number) => Math.min(1, Math.max(0, value));

const parseHex = (raw: string): [number, number, number] | null => {
  const hex = raw.replace("#", "").trim();
  if (hex.length === 3) {
    const r = parseInt(hex[0] + hex[0], 16);
    const g = parseInt(hex[1] + hex[1], 16);
    const b = parseInt(hex[2] + hex[2], 16);
    return [r, g, b];
  }
  if (hex.length === 6) {
    const r = parseInt(hex.slice(0, 2), 16);
    const g = parseInt(hex.slice(2, 4), 16);
    const b = parseInt(hex.slice(4, 6), 16);
    return [r, g, b];
  }
  return null;
};

const parseRgb = (raw: string): [number, number, number] | null => {
  const match = raw.match(/rgba?\(([^)]+)\)/i);
  if (!match) return null;
  const parts = match[1].split(",").map((part) => Number(part.trim()));
  if (parts.length < 3) return null;
  if (parts.some((part) => Number.isNaN(part))) return null;
  return [parts[0], parts[1], parts[2]];
};

export const withAlpha = (color: string, alpha: number, fallback: string): string => {
  const nextAlpha = clampAlpha(alpha);
  const trimmed = color.trim();
  const rgb = trimmed.startsWith("#") ? parseHex(trimmed) : parseRgb(trimmed);
  if (!rgb) return fallback;
  const [r, g, b] = rgb;
  return `rgba(${r}, ${g}, ${b}, ${nextAlpha})`;
};

export const useThemeVariant = (target?: HTMLElement): ThemeVariant => {
  const [variant, setVariant] = useState<ThemeVariant>(() => resolveThemeVariant(target));

  useEffect(() => {
    if (typeof document === "undefined") return;
    const element = target ?? document.documentElement;
    const update = () => setVariant(resolveThemeVariant(element));
    update();
    let observer: MutationObserver | null = null;
    if (typeof MutationObserver !== "undefined") {
      observer = new MutationObserver((mutations) => {
        if (mutations.some((mutation) => mutation.attributeName === THEME_ATTRIBUTE)) {
          update();
        }
      });
      observer.observe(element, { attributes: true, attributeFilter: [THEME_ATTRIBUTE] });
    }
    const media = getThemeMedia();
    const onMediaChange = () => update();
    if (media) {
      media.addEventListener("change", onMediaChange);
    }
    const onStorage = (event: StorageEvent) => {
      if (event.key === THEME_STORAGE_KEY) update();
    };
    if (typeof window !== "undefined") {
      window.addEventListener("storage", onStorage);
    }
    return () => {
      observer?.disconnect();
      if (media) {
        media.removeEventListener("change", onMediaChange);
      }
      if (typeof window !== "undefined") {
        window.removeEventListener("storage", onStorage);
      }
    };
  }, [target]);

  return variant;
};
