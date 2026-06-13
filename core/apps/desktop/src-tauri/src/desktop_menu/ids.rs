use serde::Serialize;

pub(crate) const MENU_EVENT_NAME: &str = "desktop_menu_action";

pub(crate) const CMD_FILE_NEW_WORKSPACE: &str = "file.new-workspace";
pub(crate) const CMD_FILE_NEW_WINDOW: &str = "file.new-window";
pub(crate) const CMD_FILE_OPEN_RECENT: &str = "file.open-recent";
pub(crate) const CMD_FILE_EXPORT_TRANSCRIPT: &str = "file.export-transcript";
pub(crate) const CMD_FILE_EXPORT_SESSION_LOG: &str = "file.export-session-log";
pub(crate) const CMD_VIEW_FIND_TASKS: &str = "view.find-tasks";
pub(crate) const CMD_VIEW_TOGGLE_SIDEBAR: &str = "view.toggle-sidebar";
pub(crate) const CMD_VIEW_TOGGLE_DIFF: &str = "view.toggle-diff";
pub(crate) const CMD_VIEW_TOGGLE_ARTIFACTS: &str = "view.toggle-artifacts";
pub(crate) const CMD_VIEW_TOGGLE_SESSIONS: &str = "view.toggle-sessions";
pub(crate) const CMD_VIEW_TOGGLE_TERMINAL: &str = "view.toggle-terminal";
pub(crate) const CMD_VIEW_TOGGLE_DEVTOOLS: &str = "view.toggle-devtools";
pub(crate) const CMD_TASK_NEW: &str = "task.new";
pub(crate) const CMD_TASK_RENAME: &str = "task.rename";
pub(crate) const CMD_TASK_ARCHIVE_TOGGLE: &str = "task.archive-toggle";
pub(crate) const CMD_TASK_MARK_READ_TOGGLE: &str = "task.mark-read-toggle";
pub(crate) const CMD_TASK_DELETE: &str = "task.delete";
pub(crate) const CMD_SESSION_COPY_TRANSCRIPT: &str = "session.copy-transcript";
pub(crate) const CMD_SESSION_COPY_SESSION_LOG: &str = "session.copy-session-log";
pub(crate) const CMD_SESSION_COPY_WORKTREE_LOCATION: &str = "session.copy-worktree-location";
pub(crate) const CMD_SESSION_COPY_TASK_ID: &str = "session.copy-task-id";
pub(crate) const CMD_SESSION_OPEN_WORKTREE_TERMINAL: &str = "session.open-worktree-terminal";
pub(crate) const CMD_SESSION_INTERRUPT: &str = "session.interrupt";
pub(crate) const CMD_GO_LAUNCHER: &str = "go.launcher";
pub(crate) const CMD_GO_WORKSPACE_SETUP: &str = "go.workspace-setup";
pub(crate) const CMD_GO_SETTINGS: &str = "go.settings";
pub(crate) const CMD_GO_DIAGNOSTICS: &str = "go.diagnostics";
pub(crate) const CMD_GO_AGENT_HARNESSES: &str = "go.agent-harnesses";
pub(crate) const CMD_HELP_KEYBOARD_SHORTCUTS: &str = "help.keyboard-shortcuts";
pub(crate) const CMD_HELP_OPEN_LOGS_FOLDER: &str = "help.open-logs-folder";
pub(crate) const CMD_HELP_REPORT_ISSUE: &str = "help.report-issue";
pub(crate) const CMD_HELP_CHECK_FOR_UPDATES: &str = "help.check-for-updates";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesktopMenuActionEvent {
    #[serde(rename = "commandId")]
    pub(crate) command_id: String,
}

pub(crate) fn is_menu_command_id(id: &str) -> bool {
    matches!(
        id,
        CMD_FILE_NEW_WORKSPACE
            | CMD_FILE_NEW_WINDOW
            | CMD_FILE_OPEN_RECENT
            | CMD_FILE_EXPORT_TRANSCRIPT
            | CMD_FILE_EXPORT_SESSION_LOG
            | CMD_VIEW_FIND_TASKS
            | CMD_VIEW_TOGGLE_SIDEBAR
            | CMD_VIEW_TOGGLE_DIFF
            | CMD_VIEW_TOGGLE_ARTIFACTS
            | CMD_VIEW_TOGGLE_SESSIONS
            | CMD_VIEW_TOGGLE_TERMINAL
            | CMD_VIEW_TOGGLE_DEVTOOLS
            | CMD_TASK_NEW
            | CMD_TASK_RENAME
            | CMD_TASK_ARCHIVE_TOGGLE
            | CMD_TASK_MARK_READ_TOGGLE
            | CMD_TASK_DELETE
            | CMD_SESSION_COPY_TRANSCRIPT
            | CMD_SESSION_COPY_SESSION_LOG
            | CMD_SESSION_COPY_WORKTREE_LOCATION
            | CMD_SESSION_COPY_TASK_ID
            | CMD_SESSION_OPEN_WORKTREE_TERMINAL
            | CMD_SESSION_INTERRUPT
            | CMD_GO_LAUNCHER
            | CMD_GO_WORKSPACE_SETUP
            | CMD_GO_SETTINGS
            | CMD_GO_DIAGNOSTICS
            | CMD_GO_AGENT_HARNESSES
            | CMD_HELP_KEYBOARD_SHORTCUTS
            | CMD_HELP_OPEN_LOGS_FOLDER
            | CMD_HELP_REPORT_ISSUE
            | CMD_HELP_CHECK_FOR_UPDATES
    )
}
