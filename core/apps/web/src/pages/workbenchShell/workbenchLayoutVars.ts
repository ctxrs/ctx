export type WorkbenchRootStyleVars = {
  "--wb-sidebar-width": string;
  "--wb-terminal-offset": string;
  "--wb-topbar-base-height": string;
  "--wb-topbar-safe-offset": string;
};

type WorkbenchRootStyleVarInput = {
  mobileShell: boolean;
  sidebarWidth: number;
  terminalHeight: number;
  terminalOpen: boolean;
  useHtmlTopbar: boolean;
  viewportWidth: number;
};

export function getWorkbenchRootStyleVars({
  mobileShell,
  sidebarWidth,
  terminalHeight,
  terminalOpen,
  useHtmlTopbar,
  viewportWidth,
}: WorkbenchRootStyleVarInput): WorkbenchRootStyleVars {
  const maxSidebarWidth = Math.max(170, viewportWidth - 240);
  const clampedSidebarWidth = Math.min(maxSidebarWidth, Math.max(170, Math.round(sidebarWidth)));
  const terminalOffset = terminalOpen ? terminalHeight : 0;
  const topbarBaseHeight = useHtmlTopbar ? "46px" : "0px";

  return {
    "--wb-sidebar-width": mobileShell ? "100vw" : `${clampedSidebarWidth}px`,
    "--wb-terminal-offset": mobileShell ? "0px" : `${terminalOffset}px`,
    "--wb-topbar-base-height": topbarBaseHeight,
    "--wb-topbar-safe-offset":
      mobileShell && useHtmlTopbar ? "env(safe-area-inset-top, 0px)" : "0px",
  };
}
