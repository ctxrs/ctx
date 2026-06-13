import React from "react";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  WEB_MENU_COMMAND_EVENT,
  WEB_MENU_STATE_EVENT,
  WEB_MENU_TRACE_EVENT,
  type WebMenuCommandDetail,
  type WebMenuStateDetail,
  type WebMenuTraceDetail,
} from "../../utils/desktopMenuCommands";
import { buildWorkbenchDesktopMenuItems, useWorkbenchDesktopMenu } from "./useWorkbenchDesktopMenu";

type HarnessProps = Parameters<typeof useWorkbenchDesktopMenu>[0];

function Harness(props: HarnessProps) {
  useWorkbenchDesktopMenu(props);
  return null;
}

const baseState = () => ({
  activeSessionId: "session-1",
  activeTaskId: "task-1",
  activeTaskArchived: false,
  activeTaskHasAssistantMessage: true,
  activeTaskIsOptimistic: false,
  canToggleArchive: true,
  canInterruptSession: true,
  copyTranscriptBusy: false,
  sidebarCollapsed: false,
  diffOpen: true,
  artifactsOpen: false,
  sessionsOpen: false,
  terminalOpen: true,
  webSessionsEnabled: true,
  worktreeCanCopy: true,
  worktreeCanOpenTerminal: true,
  isTaskUnread: vi.fn(() => true),
});

const baseHandlers = () => ({
  exportTranscript: vi.fn(),
  exportSessionLog: vi.fn(),
  focusTaskSearch: vi.fn(() => true),
  toggleSidebar: vi.fn(),
  toggleDiffPane: vi.fn(),
  toggleArtifactsPane: vi.fn(),
  toggleSessionsPane: vi.fn(),
  toggleTerminalPanel: vi.fn(),
  focusNewTask: vi.fn(),
  beginRenameTask: vi.fn(),
  toggleArchiveTask: vi.fn(),
  toggleTaskRead: vi.fn(),
  deleteTask: vi.fn(),
  copyTranscript: vi.fn(),
  copySessionLog: vi.fn(),
  copyWorktreeLocation: vi.fn(),
  copyTaskId: vi.fn(),
  openWorktreeTerminal: vi.fn(),
  interruptSession: vi.fn(),
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("useWorkbenchDesktopMenu", () => {
  it("publishes workbench menu state to the desktop event bridge", async () => {
    const state = baseState();
    const handlers = baseHandlers();
    const stateDetails: WebMenuStateDetail[] = [];
    const onState = (event: Event) => {
      stateDetails.push((event as CustomEvent<WebMenuStateDetail>).detail);
    };

    window.addEventListener(WEB_MENU_STATE_EVENT, onState as EventListener);
    try {
      render(<Harness enabled state={state} handlers={handlers} />);

      await waitFor(() => {
        expect(stateDetails).toHaveLength(1);
      });

      expect(stateDetails[0]).toEqual({
        replace: true,
        items: buildWorkbenchDesktopMenuItems(state),
      });
    } finally {
      window.removeEventListener(WEB_MENU_STATE_EVENT, onState as EventListener);
    }
  });

  it("handles desktop menu commands and emits workbench traces", async () => {
    const state = baseState();
    const handlers = baseHandlers();
    const traceDetails: WebMenuTraceDetail[] = [];
    const onTrace = (event: Event) => {
      traceDetails.push((event as CustomEvent<WebMenuTraceDetail>).detail);
    };

    window.addEventListener(WEB_MENU_TRACE_EVENT, onTrace as EventListener);
    try {
      render(<Harness enabled state={state} handlers={handlers} />);

      act(() => {
        window.dispatchEvent(
          new CustomEvent<WebMenuCommandDetail>(WEB_MENU_COMMAND_EVENT, {
            detail: { commandId: "task.mark-read-toggle" },
          }),
        );
      });

      await waitFor(() => {
        expect(handlers.toggleTaskRead).toHaveBeenCalledWith("task-1", true);
      });

      expect(traceDetails.at(-1)).toEqual({
        commandId: "task.mark-read-toggle",
        layer: "workbench",
        status: "handled",
        note: "toggle-task-read",
      });
    } finally {
      window.removeEventListener(WEB_MENU_TRACE_EVENT, onTrace as EventListener);
    }
  });
});
