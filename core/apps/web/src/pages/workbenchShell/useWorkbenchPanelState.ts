import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from "react";

import type { TerminalPanelHandle } from "../../components/TerminalPanel";
import { trackWorkbenchPanelToggled } from "../../utils/analytics";
import {
  loadWorkbenchTerminalPanelOpenV1,
  saveWorkbenchArtifactsPaneOpenV1,
  saveWorkbenchDiffPaneOpenV1,
  saveWorkbenchSessionsPaneOpenV1,
  saveWorkbenchTerminalPanelOpenV1,
} from "../../workbench/persistence";

type PaneMode = "diff" | "artifacts" | "sessions" | null;

type WorkbenchPanelStateArgs = {
  workspaceId: string;
  sidebarCollapsed: boolean;
  sidebarWidth: number;
  activeSessionId: string | null;
};

export function useWorkbenchPanelState({
  workspaceId,
  sidebarCollapsed,
  sidebarWidth,
  activeSessionId,
}: WorkbenchPanelStateArgs) {
  const clampDiffWidth = useCallback((value: number) => {
    const gutterPx = 8;
    const containerWidth =
      (document.querySelector(".wb-body") as HTMLElement | null)?.clientWidth ?? window.innerWidth;
    const min = Math.min(320, containerWidth);
    const max = Math.max(min, containerWidth - gutterPx);
    return Math.min(max, Math.max(min, Math.round(value)));
  }, []);

  const [rightPaneMode, setRightPaneMode] = useState<PaneMode>(null);
  const [diffWidth, setDiffWidth] = useState(() => clampDiffWidth(480));
  const [diffResizing, setDiffResizing] = useState(false);
  const [diffOpenHydrated, setDiffOpenHydrated] = useState(false);
  const [artifactsOpenHydrated, setArtifactsOpenHydrated] = useState(false);
  const [artifactsOpenSeeded, setArtifactsOpenSeeded] = useState(false);
  const [, setArtifactsAutoOpenPending] = useState(false);
  const artifactsPaneScopeRef = useRef<string | null>(null);
  const [sessionsOpenHydrated, setSessionsOpenHydrated] = useState(false);
  const sessionsPaneScopeRef = useRef<string | null>(null);
  const terminalPanelRef = useRef<TerminalPanelHandle | null>(null);
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [terminalHeight, setTerminalHeight] = useState(260);
  const [terminalResizing, setTerminalResizing] = useState(false);
  const [terminalOpenHydrated, setTerminalOpenHydrated] = useState(false);
  const rightPaneModeRef = useRef<PaneMode>(rightPaneMode);
  const terminalOpenRef = useRef<boolean>(terminalOpen);
  const diffOpen = rightPaneMode === "diff";
  const artifactsOpen = rightPaneMode === "artifacts";
  const sessionsOpen = rightPaneMode === "sessions";

  const clampTerminalHeight = useCallback((value: number) => {
    const min = 160;
    const max = Math.max(min, window.innerHeight - 160);
    return Math.min(max, Math.max(min, Math.round(value)));
  }, []);

  useLayoutEffect(() => {
    setDiffWidth((width) => {
      const clamped = clampDiffWidth(width);
      return clamped === width ? width : clamped;
    });
  }, [clampDiffWidth, sidebarCollapsed, sidebarWidth]);

  useEffect(() => {
    rightPaneModeRef.current = rightPaneMode;
  }, [rightPaneMode]);

  useEffect(() => {
    terminalOpenRef.current = terminalOpen;
  }, [terminalOpen]);

  useEffect(() => {
    const onResize = () => {
      setTerminalHeight((height) => clampTerminalHeight(height));
      setDiffWidth((width) => clampDiffWidth(width));
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [clampDiffWidth, clampTerminalHeight]);

  const toggleTerminalPanel = useCallback((source: "header_button" | "menu_command" | "unknown" = "unknown") => {
    const nextOpen = !terminalOpenRef.current;
    terminalOpenRef.current = nextOpen;
    setTerminalOpen(nextOpen);
    trackWorkbenchPanelToggled({
      panelKey: "terminal",
      open: nextOpen,
      source,
    });
    if (nextOpen) {
      terminalPanelRef.current?.setScope("workspace");
    }
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const isToggleKey = event.code === "Backquote" || event.key === "`";
      if (event.ctrlKey && !event.metaKey && !event.altKey && isToggleKey) {
        event.preventDefault();
        toggleTerminalPanel();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [toggleTerminalPanel]);

  const diffPaneScope = useMemo(() => {
    if (activeSessionId) return `session:${activeSessionId}`;
    return null;
  }, [activeSessionId]);

  const artifactsPaneScope = useMemo(() => {
    return activeSessionId ?? null;
  }, [activeSessionId]);

  const sessionsPaneScope = useMemo(() => {
    return activeSessionId ?? null;
  }, [activeSessionId]);

  useEffect(() => {
    setDiffOpenHydrated(false);
    if (!workspaceId || !diffPaneScope) {
      setRightPaneMode((mode) => (mode === "diff" ? null : mode));
      setDiffOpenHydrated(true);
      return;
    }
    setRightPaneMode((mode) => (mode === "diff" ? null : mode));
    setDiffOpenHydrated(true);
  }, [workspaceId, diffPaneScope]);

  useEffect(() => {
    setArtifactsOpenHydrated(false);
    setArtifactsOpenSeeded(false);
    setArtifactsAutoOpenPending(false);
    if (!workspaceId || !artifactsPaneScope) {
      setRightPaneMode((mode) => (mode === "artifacts" ? null : mode));
      setArtifactsOpenHydrated(true);
      artifactsPaneScopeRef.current = null;
      return;
    }
    setRightPaneMode((mode) => (mode === "artifacts" ? null : mode));
    setArtifactsOpenSeeded(true);
    setArtifactsAutoOpenPending(false);
    setArtifactsOpenHydrated(true);
    artifactsPaneScopeRef.current = artifactsPaneScope;
  }, [workspaceId, artifactsPaneScope]);

  useEffect(() => {
    setSessionsOpenHydrated(false);
    if (!workspaceId || !sessionsPaneScope) {
      setRightPaneMode((mode) => (mode === "sessions" ? null : mode));
      setSessionsOpenHydrated(true);
      sessionsPaneScopeRef.current = null;
      return;
    }
    setRightPaneMode((mode) => (mode === "sessions" ? null : mode));
    setSessionsOpenHydrated(true);
    sessionsPaneScopeRef.current = sessionsPaneScope;
  }, [workspaceId, sessionsPaneScope]);

  useEffect(() => {
    if (!diffOpenHydrated) return;
    if (!workspaceId || !diffPaneScope) return;
    saveWorkbenchDiffPaneOpenV1(workspaceId, diffPaneScope, diffOpen).catch(() => {});
  }, [diffOpen, diffOpenHydrated, workspaceId, diffPaneScope]);

  useEffect(() => {
    if (!artifactsOpenHydrated || !artifactsOpenSeeded) return;
    if (!workspaceId || !artifactsPaneScope) return;
    if (artifactsPaneScopeRef.current !== artifactsPaneScope) return;
    saveWorkbenchArtifactsPaneOpenV1(workspaceId, artifactsPaneScope, artifactsOpen).catch(() => {});
  }, [artifactsOpen, artifactsOpenHydrated, artifactsOpenSeeded, workspaceId, artifactsPaneScope]);

  useEffect(() => {
    if (!sessionsOpenHydrated) return;
    if (!workspaceId || !sessionsPaneScope) return;
    if (sessionsPaneScopeRef.current !== sessionsPaneScope) return;
    saveWorkbenchSessionsPaneOpenV1(workspaceId, sessionsPaneScope, sessionsOpen).catch(() => {});
  }, [sessionsOpen, sessionsOpenHydrated, workspaceId, sessionsPaneScope]);

  useEffect(() => {
    setTerminalOpenHydrated(false);
    if (!workspaceId) {
      setTerminalOpen(false);
      setTerminalOpenHydrated(true);
      return;
    }
    let cancelled = false;
    loadWorkbenchTerminalPanelOpenV1(workspaceId)
      .then((state) => {
        if (cancelled) return;
        if (state) {
          setTerminalOpen(state.open);
          setTerminalHeight(clampTerminalHeight(state.height));
        } else {
          setTerminalOpen(false);
        }
        setTerminalOpenHydrated(true);
      })
      .catch(() => {
        if (cancelled) return;
        setTerminalOpen(false);
        setTerminalOpenHydrated(true);
      });
    return () => {
      cancelled = true;
    };
  }, [clampTerminalHeight, workspaceId]);

  useEffect(() => {
    if (!terminalOpenHydrated) return;
    if (!workspaceId) return;
    saveWorkbenchTerminalPanelOpenV1(workspaceId, {
      v: 1,
      open: terminalOpen,
      height: terminalHeight,
    }).catch(() => {});
  }, [terminalHeight, terminalOpen, terminalOpenHydrated, workspaceId]);

  useEffect(() => {
    if (!terminalOpen) return;
    const id = window.requestAnimationFrame(() => terminalPanelRef.current?.focusActive());
    return () => window.cancelAnimationFrame(id);
  }, [terminalOpen]);

  const toggleDiffPane = useCallback((source: "header_button" | "menu_command" | "unknown" = "unknown") => {
    const nextMode = rightPaneModeRef.current === "diff" ? null : "diff";
    rightPaneModeRef.current = nextMode;
    setRightPaneMode(nextMode);
    trackWorkbenchPanelToggled({
      panelKey: "diff",
      open: nextMode === "diff",
      source,
    });
  }, []);

  const toggleArtifactsPane = useCallback((source: "header_button" | "menu_command" | "unknown" = "unknown") => {
    setArtifactsOpenSeeded(true);
    setArtifactsAutoOpenPending(false);
    const nextMode = rightPaneModeRef.current === "artifacts" ? null : "artifacts";
    rightPaneModeRef.current = nextMode;
    setRightPaneMode(nextMode);
    trackWorkbenchPanelToggled({
      panelKey: "artifacts",
      open: nextMode === "artifacts",
      source,
    });
  }, []);

  const toggleSessionsPane = useCallback((source: "header_button" | "menu_command" | "unknown" = "unknown") => {
    setArtifactsOpenSeeded(true);
    setArtifactsAutoOpenPending(false);
    const nextMode = rightPaneModeRef.current === "sessions" ? null : "sessions";
    rightPaneModeRef.current = nextMode;
    setRightPaneMode(nextMode);
    trackWorkbenchPanelToggled({
      panelKey: "sessions",
      open: nextMode === "sessions",
      source,
    });
  }, []);

  const onSplitterMouseDown = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();
      const startX = event.clientX;
      const startWidth = diffWidth;
      setDiffResizing(true);
      const onMove = (moveEvent: MouseEvent) => {
        const dx = startX - moveEvent.clientX;
        const next = Math.min(900, startWidth + dx);
        setDiffWidth(clampDiffWidth(next));
      };
      const onUp = () => {
        setDiffResizing(false);
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [clampDiffWidth, diffWidth],
  );

  const onTerminalResizerMouseDown = useCallback(
    (event: ReactMouseEvent) => {
      event.preventDefault();
      const startY = event.clientY;
      const startHeight = terminalHeight;
      setTerminalResizing(true);
      const onMove = (moveEvent: MouseEvent) => {
        const dy = startY - moveEvent.clientY;
        const next = clampTerminalHeight(startHeight + dy);
        setTerminalHeight(next);
      };
      const onUp = () => {
        setTerminalResizing(false);
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [clampTerminalHeight, terminalHeight],
  );

  const closeTerminalPanel = useCallback(() => {
    setTerminalOpen(false);
  }, []);

  return {
    diffOpen,
    artifactsOpen,
    sessionsOpen,
    diffWidth,
    diffResizing,
    onSplitterMouseDown,
    terminalOpen,
    setTerminalOpen,
    terminalHeight,
    terminalResizing,
    terminalPanelRef,
    closeTerminalPanel,
    onTerminalResizerMouseDown,
    toggleDiffPane,
    toggleArtifactsPane,
    toggleSessionsPane,
    toggleTerminalPanel,
  };
}
