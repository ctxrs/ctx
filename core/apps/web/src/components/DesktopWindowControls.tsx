import React, { useCallback, useEffect, useMemo, useState } from "react";
import { isDesktopApp } from "../utils/desktop";

const isMacOs = (): boolean => {
  try {
    const platform = String(navigator.platform ?? "").toLowerCase();
    if (platform.includes("mac")) return true;
    const ua = String(navigator.userAgent ?? "").toLowerCase();
    return ua.includes("mac os") || ua.includes("macintosh");
  } catch {
    return false;
  }
};

export function DesktopWindowControls() {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (!isDesktopApp()) return;
    let unlisten: (() => void) | null = null;
    void (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const win = getCurrentWindow();
        setMaximized(await win.isMaximized());
        unlisten = await win.onResized(async () => {
          setMaximized(await win.isMaximized());
        });
      } catch {
        // ignore
      }
    })();
    return () => {
      try {
        unlisten?.();
      } catch {
        // ignore
      }
    };
  }, []);

  const show = useMemo(() => isDesktopApp() && !isMacOs(), []);

  const minimize = useCallback(() => {
    void (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().minimize();
    })();
  }, []);

  const toggleMaximize = useCallback(() => {
    void (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().toggleMaximize();
      setMaximized(await getCurrentWindow().isMaximized());
    })();
  }, []);

  const close = useCallback(() => {
    void (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().close();
    })();
  }, []);

  if (!show) return null;

  return (
    <div className="wb-win-controls" data-tauri-drag-region={false}>
      <button type="button" className="wb-win-btn" aria-label="Minimize" title="Minimize" onClick={minimize}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M1 6.5h8" stroke="currentColor" strokeWidth="1.2" strokeLinecap="square" />
        </svg>
      </button>
      <button
        type="button"
        className="wb-win-btn"
        aria-label={maximized ? "Restore" : "Maximize"}
        title={maximized ? "Restore" : "Maximize"}
        onClick={toggleMaximize}
      >
        {maximized ? (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path
              d="M3.2 2.2h5.2v5.2H3.2z"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.0"
              strokeLinejoin="miter"
            />
            <path
              d="M1.6 3.8h5.2v5.2H1.6z"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.0"
              strokeLinejoin="miter"
            />
          </svg>
        ) : (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path
              d="M1.8 1.8h6.4v6.4H1.8z"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.0"
              strokeLinejoin="miter"
            />
          </svg>
        )}
      </button>
      <button type="button" className="wb-win-btn wb-win-btn-close" aria-label="Close" title="Close" onClick={close}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path
            d="M2.2 2.2l5.6 5.6M7.8 2.2L2.2 7.8"
            stroke="currentColor"
            strokeWidth="1.2"
            strokeLinecap="square"
          />
        </svg>
      </button>
    </div>
  );
}
