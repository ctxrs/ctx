type TauriGlobals = {
  __TAURI_INTERNALS__?: unknown;
  __TAURI__?: unknown;
};

export type AppShellKind = "web" | "desktop" | "mobile";

export const isTauriRuntime = (): boolean => {
  try {
    const g = globalThis as typeof globalThis & TauriGlobals;
    return Boolean(g.__TAURI_INTERNALS__ || g.__TAURI__);
  } catch {
    return false;
  }
};

export const isMobileUserAgent = (): boolean => {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes("iphone") || ua.includes("ipad") || ua.includes("ipod") || ua.includes("android")) {
    return true;
  }
  const platform = String(navigator.platform || "").toLowerCase();
  return platform.includes("mac") && Number(navigator.maxTouchPoints || 0) > 1;
};

export const getAppShellKind = (): AppShellKind => {
  if (!isTauriRuntime()) return "web";
  return isMobileUserAgent() ? "mobile" : "desktop";
};

export const isDesktopShellApp = (): boolean => getAppShellKind() === "desktop";

export const isMobileShellApp = (): boolean => getAppShellKind() === "mobile";
