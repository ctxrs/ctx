export const DESKTOP_MENU_ACTION_EVENT = "desktop_menu_action" as const;
export const WEB_MENU_COMMAND_EVENT = "ctx:menu-command" as const;
export const WEB_MENU_STATE_EVENT = "ctx:menu-state" as const;
export const WEB_MENU_TRACE_EVENT = "ctx:menu-trace" as const;
export const REQUEST_UPDATE_CHECK_EVENT = "ctx:request-update-check" as const;
export const REQUEST_UPDATE_RESTART_EVENT = "ctx:request-update-restart" as const;
export const DESKTOP_UPDATE_MENU_STATE_EVENT = "ctx:desktop-update-menu-state" as const;

export const DESKTOP_MENU_COMMAND_IDS = [
  "file.new-workspace",
  "file.new-window",
  "file.open-recent",
  "file.export-transcript",
  "file.export-session-log",
  "view.find-tasks",
  "view.toggle-sidebar",
  "view.toggle-diff",
  "view.toggle-artifacts",
  "view.toggle-sessions",
  "view.toggle-terminal",
  "task.new",
  "task.rename",
  "task.archive-toggle",
  "task.mark-read-toggle",
  "task.delete",
  "session.copy-transcript",
  "session.copy-session-log",
  "session.copy-worktree-location",
  "session.copy-task-id",
  "session.open-worktree-terminal",
  "session.interrupt",
  "go.launcher",
  "go.workspace-setup",
  "go.settings",
  "go.diagnostics",
  "go.agent-harnesses",
  "help.keyboard-shortcuts",
  "help.check-for-updates",
  "help.open-logs-folder",
  "help.report-issue",
] as const;

export type DesktopMenuCommandId = (typeof DESKTOP_MENU_COMMAND_IDS)[number];

export type DesktopMenuActionEventPayload = {
  commandId: DesktopMenuCommandId;
};

export type DesktopMenuItemState = {
  id: DesktopMenuCommandId;
  enabled?: boolean;
  checked?: boolean;
  text?: string;
};

export type WebMenuCommandDetail = {
  commandId: DesktopMenuCommandId;
};

export type WebMenuStateDetail = {
  items: DesktopMenuItemState[];
  replace?: boolean;
};

export type WebMenuTraceDetail = {
  commandId: DesktopMenuCommandId;
  layer: "app" | "workbench";
  status: "forwarded" | "handled" | "ignored";
  note?: string;
};

export type DesktopUpdateMenuState = "check" | "downloading" | "restart";

export type DesktopUpdateMenuStateDetail = {
  state: DesktopUpdateMenuState;
};

const COMMAND_ID_SET = new Set<string>(DESKTOP_MENU_COMMAND_IDS);

export const isDesktopMenuCommandId = (value: unknown): value is DesktopMenuCommandId => {
  return typeof value === "string" && COMMAND_ID_SET.has(value);
};

export const isDesktopWorkspaceRoute = (pathname: string): boolean => {
  return pathname.startsWith("/workspaces/");
};

export const buildDesktopMenuBaseState = (pathname: string): DesktopMenuItemState[] => {
  const inWorkspace = isDesktopWorkspaceRoute(pathname);
  return [
    { id: "file.new-workspace", enabled: true },
    { id: "file.new-window", enabled: true },
    { id: "file.open-recent", enabled: false },
    { id: "file.export-transcript", enabled: false },
    { id: "file.export-session-log", enabled: false },
    { id: "view.find-tasks", enabled: inWorkspace },
    { id: "view.toggle-sidebar", enabled: inWorkspace, checked: false },
    { id: "view.toggle-diff", enabled: false, checked: false },
    { id: "view.toggle-artifacts", enabled: false, checked: false },
    { id: "view.toggle-sessions", enabled: false, checked: false },
    { id: "view.toggle-terminal", enabled: inWorkspace, checked: false },
    { id: "task.new", enabled: inWorkspace },
    { id: "task.rename", enabled: false },
    { id: "task.archive-toggle", enabled: false },
    { id: "task.mark-read-toggle", enabled: false },
    { id: "task.delete", enabled: false },
    { id: "session.copy-transcript", enabled: false },
    { id: "session.copy-session-log", enabled: false },
    { id: "session.copy-worktree-location", enabled: false },
    { id: "session.copy-task-id", enabled: false },
    { id: "session.open-worktree-terminal", enabled: false },
    { id: "session.interrupt", enabled: false },
    { id: "go.launcher", enabled: true },
    { id: "go.workspace-setup", enabled: true },
    { id: "go.settings", enabled: true },
    { id: "go.diagnostics", enabled: true },
    { id: "go.agent-harnesses", enabled: true },
    { id: "help.keyboard-shortcuts", enabled: true },
    { id: "help.check-for-updates", enabled: true },
    { id: "help.open-logs-folder", enabled: true },
    { id: "help.report-issue", enabled: true },
  ];
};

export const isDesktopUpdateMenuState = (value: unknown): value is DesktopUpdateMenuState => {
  return value === "check" || value === "downloading" || value === "restart";
};

export const buildDesktopUpdateMenuPatch = (
  state: DesktopUpdateMenuState,
): DesktopMenuItemState => {
  switch (state) {
    case "downloading":
      return {
        id: "help.check-for-updates",
        enabled: false,
        text: "Downloading Update",
      };
    case "restart":
      return {
        id: "help.check-for-updates",
        enabled: true,
        text: "Restart to Update",
      };
    case "check":
    default:
      return {
        id: "help.check-for-updates",
        enabled: true,
        text: "Check for Updates...",
      };
  }
};
