import { describe, expect, it, vi } from "vitest";
import {
  buildWorkbenchDesktopMenuItems,
  handleWorkbenchDesktopMenuCommand,
} from "./useWorkbenchDesktopMenu";

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
  terminalOpen: false,
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

describe("buildWorkbenchDesktopMenuItems", () => {
  it("derives checked and enabled state from workbench context", () => {
    const items = buildWorkbenchDesktopMenuItems(baseState());
    expect(items.find((item) => item.id === "view.toggle-sidebar")).toEqual({
      id: "view.toggle-sidebar",
      enabled: true,
      checked: true,
    });
    expect(items.find((item) => item.id === "view.toggle-diff")).toEqual({
      id: "view.toggle-diff",
      enabled: true,
      checked: true,
    });
    expect(items.find((item) => item.id === "session.interrupt")).toEqual({
      id: "session.interrupt",
      enabled: true,
    });
  });
});

describe("handleWorkbenchDesktopMenuCommand", () => {
  it("routes handled commands to the provided handlers", () => {
    const state = baseState();
    const handlers = baseHandlers();
    const result = handleWorkbenchDesktopMenuCommand("task.mark-read-toggle", state, handlers);

    expect(result).toEqual({ status: "handled", note: "toggle-task-read" });
    expect(state.isTaskUnread).toHaveBeenCalledWith("task-1");
    expect(handlers.toggleTaskRead).toHaveBeenCalledWith("task-1", true);
  });

  it("returns ignored when required workbench state is missing", () => {
    const state = { ...baseState(), activeSessionId: null, activeTaskId: null, worktreeCanCopy: false };
    const handlers = baseHandlers();

    expect(handleWorkbenchDesktopMenuCommand("file.export-transcript", state, handlers)).toEqual({
      status: "ignored",
      note: "session-missing",
    });
    expect(handleWorkbenchDesktopMenuCommand("session.copy-worktree-location", state, handlers)).toEqual({
      status: "ignored",
      note: "worktree-unavailable",
    });
    expect(handleWorkbenchDesktopMenuCommand("session.copy-task-id", state, handlers)).toEqual({
      status: "ignored",
      note: "task-missing-or-optimistic",
    });
    expect(handlers.exportTranscript).not.toHaveBeenCalled();
    expect(handlers.copyWorktreeLocation).not.toHaveBeenCalled();
    expect(handlers.copyTaskId).not.toHaveBeenCalled();
  });

  it("routes copy task ID when an active task is selected", () => {
    const state = baseState();
    const handlers = baseHandlers();

    expect(handleWorkbenchDesktopMenuCommand("session.copy-task-id", state, handlers)).toEqual({
      status: "handled",
      note: "copy-task-id",
    });
    expect(handlers.copyTaskId).toHaveBeenCalledTimes(1);
  });

  it("ignores copy task ID for optimistic tasks", () => {
    const state = { ...baseState(), activeTaskIsOptimistic: true };
    const handlers = baseHandlers();

    expect(handleWorkbenchDesktopMenuCommand("session.copy-task-id", state, handlers)).toEqual({
      status: "ignored",
      note: "task-missing-or-optimistic",
    });
    expect(handlers.copyTaskId).not.toHaveBeenCalled();
  });
});
