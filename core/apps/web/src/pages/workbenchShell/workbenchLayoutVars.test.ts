import { describe, expect, it } from "vitest";
import { getWorkbenchRootStyleVars } from "./workbenchLayoutVars";

describe("getWorkbenchRootStyleVars", () => {
  it("includes the safe-area offset in mobile shells with the html topbar enabled", () => {
    expect(
      getWorkbenchRootStyleVars({
        mobileShell: true,
        sidebarWidth: 312,
        terminalHeight: 180,
        terminalOpen: true,
        useHtmlTopbar: true,
        viewportWidth: 430,
      }),
    ).toEqual({
      "--wb-sidebar-width": "100vw",
      "--wb-terminal-offset": "0px",
      "--wb-topbar-base-height": "46px",
      "--wb-topbar-safe-offset": "env(safe-area-inset-top, 0px)",
    });
  });

  it("clamps the desktop sidebar width and preserves the terminal offset", () => {
    expect(
      getWorkbenchRootStyleVars({
        mobileShell: false,
        sidebarWidth: 1200,
        terminalHeight: 264,
        terminalOpen: true,
        useHtmlTopbar: true,
        viewportWidth: 960,
      }),
    ).toEqual({
      "--wb-sidebar-width": "720px",
      "--wb-terminal-offset": "264px",
      "--wb-topbar-base-height": "46px",
      "--wb-topbar-safe-offset": "0px",
    });
  });

  it("removes both topbar variables when the html topbar is disabled", () => {
    expect(
      getWorkbenchRootStyleVars({
        mobileShell: true,
        sidebarWidth: 280,
        terminalHeight: 120,
        terminalOpen: false,
        useHtmlTopbar: false,
        viewportWidth: 390,
      }),
    ).toEqual({
      "--wb-sidebar-width": "100vw",
      "--wb-terminal-offset": "0px",
      "--wb-topbar-base-height": "0px",
      "--wb-topbar-safe-offset": "0px",
    });
  });
});
