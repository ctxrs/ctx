import { useCallback, useEffect, useLayoutEffect, useState, type MouseEvent as ReactMouseEvent } from "react";

const SIDEBAR_MIN_WIDTH = 170;
const SIDEBAR_DEFAULT_WIDTH = 260;
const SIDEBAR_WINDOW_PADDING = 240;

function clampSidebarWidth(width: number) {
  const max = Math.max(SIDEBAR_MIN_WIDTH, window.innerWidth - SIDEBAR_WINDOW_PADDING);
  return Math.min(max, Math.max(SIDEBAR_MIN_WIDTH, Math.round(width)));
}

export function useWorkbenchShellLayout({
  workspaceId,
  focusNewTask,
  mobileMode = false,
}: {
  workspaceId: string;
  focusNewTask: () => void;
  mobileMode?: boolean;
}) {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(mobileMode);
  const [sidebarWidth, setSidebarWidth] = useState(SIDEBAR_DEFAULT_WIDTH);
  const [sidebarResizing, setSidebarResizing] = useState(false);

  useEffect(() => {
    document.documentElement.classList.add("wb-no-scroll");
    document.body.classList.add("wb-no-scroll");
    return () => {
      document.body.classList.remove("wb-no-scroll");
      document.documentElement.classList.remove("wb-no-scroll");
    };
  }, []);

  useEffect(() => {
    if (mobileMode) return;
    const onResize = () => {
      setSidebarWidth((width) => clampSidebarWidth(width));
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [mobileMode]);

  useLayoutEffect(() => {
    if (mobileMode) return;
    if (!workspaceId) return;
    const key = `wb.sidebarWidth.${workspaceId}`;
    try {
      const raw = localStorage.getItem(key);
      const parsed = raw ? Number(raw) : Number.NaN;
      if (!Number.isFinite(parsed)) return;
      setSidebarWidth(clampSidebarWidth(parsed));
    } catch {
      // ignore
    }
  }, [mobileMode, workspaceId]);

  useEffect(() => {
    if (mobileMode) return;
    if (!workspaceId) return;
    try {
      localStorage.setItem(`wb.sidebarWidth.${workspaceId}`, String(clampSidebarWidth(sidebarWidth)));
    } catch {
      // ignore
    }
  }, [mobileMode, sidebarWidth, workspaceId]);

  useEffect(() => {
    if (mobileMode) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.altKey || event.shiftKey) return;
      const hasModifier = event.metaKey || event.ctrlKey;
      if (!hasModifier) return;
      const key = event.key.toLowerCase();
      if (key === "b") {
        event.preventDefault();
        setSidebarCollapsed((prev) => !prev);
      } else if (key === "n") {
        event.preventDefault();
        focusNewTask();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [focusNewTask, mobileMode]);

  const onSidebarResizerMouseDown = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (sidebarCollapsed) return;
      setSidebarResizing(true);
      const startX = event.clientX;
      const startWidth = sidebarWidth;
      const onMove = (moveEvent: MouseEvent) => {
        const dx = moveEvent.clientX - startX;
        setSidebarWidth(clampSidebarWidth(startWidth + dx));
      };
      const onUp = () => {
        setSidebarResizing(false);
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [sidebarCollapsed, sidebarWidth],
  );

  return {
    sidebarCollapsed,
    setSidebarCollapsed,
    sidebarWidth,
    setSidebarWidth,
    sidebarResizing,
    onSidebarResizerMouseDown,
  };
}
