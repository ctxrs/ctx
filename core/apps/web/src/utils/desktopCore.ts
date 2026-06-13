import type { DesktopOpenExternalUrlReq } from "../generated/desktop-ipc";
import { isDesktopShellApp } from "./runtime";

export type DesktopPlatform = "macos" | "windows" | "linux" | "unknown";

export type DesktopDragDropPosition = {
  x: number;
  y: number;
};

export type DesktopPhysicalSize = {
  width: number;
  height: number;
};

export type DesktopViewGeometry = {
  scaleFactor: number;
  devicePixelRatio: number;
  webviewPosition: DesktopDragDropPosition;
  webviewSize: DesktopPhysicalSize;
  windowInnerPosition: DesktopDragDropPosition;
  windowOuterPosition: DesktopDragDropPosition;
  windowInnerSize: DesktopPhysicalSize;
  windowOuterSize: DesktopPhysicalSize;
  screenWidth: number;
  screenHeight: number;
  innerWidth: number;
  innerHeight: number;
};

export type DesktopDragDropEvent =
  | {
      type: "enter";
      paths: string[];
      position: DesktopDragDropPosition;
    }
  | {
      type: "over";
      position: DesktopDragDropPosition;
    }
  | {
      type: "drop";
      paths: string[];
      position: DesktopDragDropPosition;
    }
  | {
      type: "leave";
    };
const DESKTOP_DRAG_DROP_TEST_EVENT = "ctx:desktop-drag-drop-test";

export const isDesktopApp = (): boolean => {
  return isDesktopShellApp();
};

export const getDesktopPlatform = async (): Promise<DesktopPlatform> => {
  if (!isDesktopApp()) return "unknown";
  try {
    const mod = await import("@tauri-apps/plugin-os");
    const value = await mod.platform();
    switch (value) {
      case "macos":
        return "macos";
      case "windows":
        return "windows";
      case "linux":
        return "linux";
      default:
        return "unknown";
    }
  } catch {
    return "unknown";
  }
};

export const openExternalLink = async (href: string): Promise<boolean> => {
  if (!href) return false;
  if (isDesktopApp()) {
    try {
      const req: DesktopOpenExternalUrlReq = { url: href };
      await invoke<void>("desktop_open_external_url", { req });
      return true;
    } catch {
      return false;
    }
  }
  try {
    const win = window.open(href, "_blank", "noopener,noreferrer");
    return Boolean(win);
  } catch {
    return false;
  }
};

export const invoke = async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
  const mod = await import("@tauri-apps/api/core");
  try {
    return await mod.invoke<T>(cmd, args);
  } catch (err: unknown) {
    if (err instanceof Error) throw err;
    if (typeof err === "string") throw new Error(err.trim() || `desktop invoke failed: ${cmd}`);
    if (err && typeof err === "object") {
      const withMessage = err as { message?: unknown };
      if (typeof withMessage.message === "string") {
        const message = withMessage.message.trim();
        if (message) throw new Error(message);
      }
      try {
        const encoded = JSON.stringify(err);
        if (encoded.trim()) throw new Error(encoded);
      } catch {
        // ignore serialization failures
      }
    }
    throw new Error(`desktop invoke failed: ${cmd}`);
  }
};

export const invokeDesktopReq = async <TReq, TResp>(cmd: string, req: TReq): Promise<TResp> =>
  invoke<TResp>(cmd, { req });

export const desktopListen = async <T>(event: string, handler: (payload: T) => void): Promise<() => void> => {
  const mod = await import("@tauri-apps/api/event");
  const unlisten = await mod.listen<T>(event, (e) => handler(e.payload));
  return () => {
    try {
      unlisten();
    } catch {
      // ignore
    }
  };
};

export const desktopListenForDragDrop = async (
  handler: (event: DesktopDragDropEvent) => void,
): Promise<(() => void) | null> => {
  const cleanup = new Set<() => void>();
  const onTestEvent = (event: Event) => {
    if (!(event instanceof CustomEvent)) return;
    handler(event.detail as DesktopDragDropEvent);
  };
  window.addEventListener(DESKTOP_DRAG_DROP_TEST_EVENT, onTestEvent as EventListener);
  cleanup.add(() => {
    window.removeEventListener(DESKTOP_DRAG_DROP_TEST_EVENT, onTestEvent as EventListener);
  });
  try {
    const unlisten = await desktopListen<DesktopDragDropEvent>(DESKTOP_DRAG_DROP_TEST_EVENT, handler);
    cleanup.add(() => {
      try {
        unlisten();
      } catch {
        // ignore
      }
    });
  } catch {
    // Ignore missing event-bus support in environments that only expose DOM test events.
  }
  try {
    const mod = await import("@tauri-apps/api/webview");
    const unlisten = await mod.getCurrentWebview().onDragDropEvent((event) => handler(event.payload as DesktopDragDropEvent));
    cleanup.add(() => {
      try {
        unlisten();
      } catch {
        // ignore
      }
    });
  } catch {
    // Ignore missing Tauri drag/drop support in environments that don't expose the desktop webview API.
  }
  return () => {
    for (const dispose of cleanup) {
      try {
        dispose();
      } catch {
        // ignore
      }
    }
  };
};

export const desktopGetViewGeometry = async (): Promise<DesktopViewGeometry> => {
  if (!isDesktopApp()) {
    throw new Error("desktop view geometry is only available inside the desktop app");
  }
  const [webviewMod, windowMod] = await Promise.all([
    import("@tauri-apps/api/webview"),
    import("@tauri-apps/api/window"),
  ]);
  const webview = webviewMod.getCurrentWebview();
  const currentWindow = windowMod.getCurrentWindow();
  const [
    webviewPosition,
    webviewSize,
    windowInnerPosition,
    windowOuterPosition,
    windowInnerSize,
    windowOuterSize,
    scaleFactor,
  ] = await Promise.all([
    webview.position(),
    webview.size(),
    currentWindow.innerPosition(),
    currentWindow.outerPosition(),
    currentWindow.innerSize(),
    currentWindow.outerSize(),
    currentWindow.scaleFactor(),
  ]);
  return {
    scaleFactor,
    devicePixelRatio: window.devicePixelRatio > 0 ? window.devicePixelRatio : 1,
    webviewPosition: {
      x: webviewPosition.x,
      y: webviewPosition.y,
    },
    webviewSize: {
      width: webviewSize.width,
      height: webviewSize.height,
    },
    windowInnerPosition: {
      x: windowInnerPosition.x,
      y: windowInnerPosition.y,
    },
    windowOuterPosition: {
      x: windowOuterPosition.x,
      y: windowOuterPosition.y,
    },
    windowInnerSize: {
      width: windowInnerSize.width,
      height: windowInnerSize.height,
    },
    windowOuterSize: {
      width: windowOuterSize.width,
      height: windowOuterSize.height,
    },
    screenWidth: window.screen.width,
    screenHeight: window.screen.height,
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
  };
};
