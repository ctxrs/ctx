import { useCallback, useEffect } from "react";
import {
  WEB_MENU_COMMAND_EVENT,
  WEB_MENU_STATE_EVENT,
  WEB_MENU_TRACE_EVENT,
  type DesktopMenuCommandId,
  type DesktopMenuItemState,
  type WebMenuCommandDetail,
  type WebMenuStateDetail,
  type WebMenuTraceDetail,
} from "../../utils/desktopMenuCommands";

type WorkbenchDesktopMenuState = {
  activeSessionId: string | null;
  activeTaskId: string | null;
  activeTaskArchived: boolean;
  activeTaskHasAssistantMessage: boolean;
  activeTaskIsOptimistic: boolean;
  canToggleArchive: boolean;
  canInterruptSession: boolean;
  copyTranscriptBusy: boolean;
  sidebarCollapsed: boolean;
  diffOpen: boolean;
  artifactsOpen: boolean;
  sessionsOpen: boolean;
  terminalOpen: boolean;
  webSessionsEnabled: boolean;
  worktreeCanCopy: boolean;
  worktreeCanOpenTerminal: boolean;
  isTaskUnread: (taskId: string) => boolean;
};

type WorkbenchDesktopMenuHandlers = {
  exportTranscript: () => void | Promise<void>;
  exportSessionLog: () => void | Promise<void>;
  focusTaskSearch: () => boolean;
  toggleSidebar: () => void;
  toggleDiffPane: () => void;
  toggleArtifactsPane: () => void;
  toggleSessionsPane: () => void;
  toggleTerminalPanel: () => void;
  focusNewTask: () => void;
  beginRenameTask: (taskId: string) => void;
  toggleArchiveTask: (taskId: string, nextArchived: boolean) => void;
  toggleTaskRead: (taskId: string, unread: boolean) => void;
  deleteTask: (taskId: string) => void;
  copyTranscript: () => void | Promise<void>;
  copySessionLog: () => void | Promise<void>;
  copyWorktreeLocation: () => void | Promise<void>;
  copyTaskId: () => void | Promise<void>;
  openWorktreeTerminal: () => void | Promise<void>;
  interruptSession: (sessionId: string) => void;
};

type WorkbenchDesktopMenuArgs = {
  enabled: boolean;
  state: WorkbenchDesktopMenuState;
  handlers: WorkbenchDesktopMenuHandlers;
};

type WorkbenchDesktopMenuCommandResult = {
  status: "handled" | "ignored";
  note: string;
};

export const buildWorkbenchDesktopMenuItems = (state: WorkbenchDesktopMenuState): DesktopMenuItemState[] => [
  { id: "file.export-transcript", enabled: Boolean(state.activeSessionId) },
  { id: "file.export-session-log", enabled: Boolean(state.activeSessionId) },
  { id: "view.find-tasks", enabled: true },
  { id: "view.toggle-sidebar", enabled: true, checked: !state.sidebarCollapsed },
  { id: "view.toggle-diff", enabled: Boolean(state.activeTaskId), checked: state.diffOpen },
  { id: "view.toggle-artifacts", enabled: Boolean(state.activeTaskId && state.activeSessionId), checked: state.artifactsOpen },
  {
    id: "view.toggle-sessions",
    enabled: Boolean(state.webSessionsEnabled && state.activeTaskId && state.activeSessionId),
    checked: state.sessionsOpen,
  },
  { id: "view.toggle-terminal", enabled: true, checked: state.terminalOpen },
  { id: "task.new", enabled: true },
  { id: "task.rename", enabled: Boolean(state.activeTaskId) && !state.activeTaskIsOptimistic },
  { id: "task.archive-toggle", enabled: state.canToggleArchive },
  { id: "task.mark-read-toggle", enabled: Boolean(state.activeTaskId) && state.activeTaskHasAssistantMessage },
  { id: "task.delete", enabled: Boolean(state.activeTaskId) },
  { id: "session.copy-transcript", enabled: Boolean(state.activeSessionId) && !state.copyTranscriptBusy },
  { id: "session.copy-session-log", enabled: Boolean(state.activeSessionId) },
  { id: "session.copy-worktree-location", enabled: state.worktreeCanCopy },
  { id: "session.copy-task-id", enabled: Boolean(state.activeTaskId) && !state.activeTaskIsOptimistic },
  { id: "session.open-worktree-terminal", enabled: state.worktreeCanOpenTerminal },
  { id: "session.interrupt", enabled: state.canInterruptSession },
];

export const handleWorkbenchDesktopMenuCommand = (
  commandId: DesktopMenuCommandId,
  state: WorkbenchDesktopMenuState,
  handlers: WorkbenchDesktopMenuHandlers,
): WorkbenchDesktopMenuCommandResult => {
  switch (commandId) {
    case "file.export-transcript":
      if (!state.activeSessionId) return { status: "ignored", note: "session-missing" };
      void handlers.exportTranscript();
      return { status: "handled", note: "export-transcript" };
    case "file.export-session-log":
      if (!state.activeSessionId) return { status: "ignored", note: "session-missing" };
      void handlers.exportSessionLog();
      return { status: "handled", note: "export-session-log" };
    case "view.find-tasks":
      return handlers.focusTaskSearch()
        ? { status: "handled", note: "focus-task-search" }
        : { status: "ignored", note: "task-search-missing" };
    case "view.toggle-sidebar":
      handlers.toggleSidebar();
      return { status: "handled", note: "toggle-sidebar" };
    case "view.toggle-diff":
      if (!state.activeTaskId) return { status: "ignored", note: "task-missing" };
      handlers.toggleDiffPane();
      return { status: "handled", note: "toggle-diff-pane" };
    case "view.toggle-artifacts":
      if (!state.activeTaskId || !state.activeSessionId) {
        return { status: "ignored", note: "task-or-session-missing" };
      }
      handlers.toggleArtifactsPane();
      return { status: "handled", note: "toggle-artifacts-pane" };
    case "view.toggle-sessions":
      if (!state.webSessionsEnabled || !state.activeTaskId || !state.activeSessionId) {
        return { status: "ignored", note: "sessions-unavailable" };
      }
      handlers.toggleSessionsPane();
      return { status: "handled", note: "toggle-sessions-pane" };
    case "view.toggle-terminal":
      handlers.toggleTerminalPanel();
      return { status: "handled", note: "toggle-terminal" };
    case "task.new":
      handlers.focusNewTask();
      return { status: "handled", note: "create-task" };
    case "task.rename":
      if (!state.activeTaskId || state.activeTaskIsOptimistic) {
        return { status: "ignored", note: "task-missing-or-optimistic" };
      }
      handlers.beginRenameTask(state.activeTaskId);
      return { status: "handled", note: "rename-task" };
    case "task.archive-toggle":
      if (!state.activeTaskId) return { status: "ignored", note: "task-missing" };
      handlers.toggleArchiveTask(state.activeTaskId, !state.activeTaskArchived);
      return { status: "handled", note: "toggle-task-archive" };
    case "task.mark-read-toggle":
      if (!state.activeTaskId || !state.activeTaskHasAssistantMessage) {
        return { status: "ignored", note: "task-or-message-missing" };
      }
      handlers.toggleTaskRead(state.activeTaskId, state.isTaskUnread(state.activeTaskId));
      return { status: "handled", note: "toggle-task-read" };
    case "task.delete":
      if (!state.activeTaskId) return { status: "ignored", note: "task-missing" };
      handlers.deleteTask(state.activeTaskId);
      return { status: "handled", note: "delete-task" };
    case "session.copy-transcript":
      void handlers.copyTranscript();
      return { status: "handled", note: "copy-transcript" };
    case "session.copy-session-log":
      void handlers.copySessionLog();
      return { status: "handled", note: "copy-session-log" };
    case "session.copy-worktree-location":
      if (!state.worktreeCanCopy) return { status: "ignored", note: "worktree-unavailable" };
      void handlers.copyWorktreeLocation();
      return { status: "handled", note: "copy-worktree-location" };
    case "session.copy-task-id":
      if (!state.activeTaskId || state.activeTaskIsOptimistic) {
        return { status: "ignored", note: "task-missing-or-optimistic" };
      }
      void handlers.copyTaskId();
      return { status: "handled", note: "copy-task-id" };
    case "session.open-worktree-terminal":
      if (!state.worktreeCanOpenTerminal) return { status: "ignored", note: "worktree-unavailable" };
      void handlers.openWorktreeTerminal();
      return { status: "handled", note: "open-worktree-terminal" };
    case "session.interrupt":
      if (!state.activeSessionId) return { status: "ignored", note: "session-missing" };
      handlers.interruptSession(state.activeSessionId);
      return { status: "handled", note: "interrupt-session" };
    default:
      return { status: "ignored", note: "unsupported-command" };
  }
};

export function useWorkbenchDesktopMenu({ enabled, state, handlers }: WorkbenchDesktopMenuArgs) {
  const emitMenuTrace = useCallback((commandId: DesktopMenuCommandId, result: WorkbenchDesktopMenuCommandResult) => {
    window.dispatchEvent(
      new CustomEvent<WebMenuTraceDetail>(WEB_MENU_TRACE_EVENT, {
        detail: {
          commandId,
          layer: "workbench",
          status: result.status,
          note: result.note,
        },
      }),
    );
  }, []);

  useEffect(() => {
    if (!enabled) return;
    const onMenuCommand = (event: Event) => {
      const detail = (event as CustomEvent<WebMenuCommandDetail>).detail;
      if (!detail) return;
      const result = handleWorkbenchDesktopMenuCommand(detail.commandId, state, handlers);
      emitMenuTrace(detail.commandId, result);
    };

    window.addEventListener(WEB_MENU_COMMAND_EVENT, onMenuCommand as EventListener);
    return () => {
      window.removeEventListener(WEB_MENU_COMMAND_EVENT, onMenuCommand as EventListener);
    };
  }, [emitMenuTrace, enabled, handlers, state]);

  useEffect(() => {
    if (!enabled) return;
    window.dispatchEvent(
      new CustomEvent<WebMenuStateDetail>(WEB_MENU_STATE_EVENT, {
        detail: {
          replace: true,
          items: buildWorkbenchDesktopMenuItems(state),
        },
      }),
    );
  }, [enabled, state]);
}
