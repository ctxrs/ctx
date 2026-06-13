pub(super) use ctx_desktop_ipc::{DesktopMenuItemStateUpdate, DesktopSetMenuStateReq};
use tauri::{Emitter, Manager};

mod build;
mod ids;
mod state;

pub(super) use build::build_app_menu;
pub(super) use ids::{
    is_menu_command_id, CMD_FILE_EXPORT_SESSION_LOG, CMD_FILE_EXPORT_TRANSCRIPT,
    CMD_FILE_NEW_WINDOW, CMD_FILE_NEW_WORKSPACE, CMD_FILE_OPEN_RECENT, CMD_GO_AGENT_HARNESSES,
    CMD_GO_DIAGNOSTICS, CMD_GO_LAUNCHER, CMD_GO_SETTINGS, CMD_GO_WORKSPACE_SETUP,
    CMD_HELP_CHECK_FOR_UPDATES, CMD_HELP_KEYBOARD_SHORTCUTS, CMD_HELP_OPEN_LOGS_FOLDER,
    CMD_HELP_REPORT_ISSUE, CMD_SESSION_COPY_SESSION_LOG, CMD_SESSION_COPY_TASK_ID,
    CMD_SESSION_COPY_TRANSCRIPT, CMD_SESSION_COPY_WORKTREE_LOCATION, CMD_SESSION_INTERRUPT,
    CMD_SESSION_OPEN_WORKTREE_TERMINAL, CMD_TASK_ARCHIVE_TOGGLE, CMD_TASK_DELETE,
    CMD_TASK_MARK_READ_TOGGLE, CMD_TASK_NEW, CMD_TASK_RENAME, CMD_VIEW_FIND_TASKS,
    CMD_VIEW_TOGGLE_ARTIFACTS, CMD_VIEW_TOGGLE_DEVTOOLS, CMD_VIEW_TOGGLE_DIFF,
    CMD_VIEW_TOGGLE_SESSIONS, CMD_VIEW_TOGGLE_SIDEBAR, CMD_VIEW_TOGGLE_TERMINAL,
};
use ids::{DesktopMenuActionEvent, MENU_EVENT_NAME};
pub(super) use state::{
    apply_cached_menu_state_for_window, clear_cached_menu_state_for_window,
    desktop_get_menu_item_state, desktop_set_menu_state, mark_menu_state_window_focused,
    DesktopMenuItemStateSnapshot, DesktopMenuStateCache,
};

fn focused_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    for window in app.webview_windows().values() {
        if window.is_focused().unwrap_or(false) {
            return Some(window.clone());
        }
    }

    let cache = app.state::<DesktopMenuStateCache>();
    if let Some(window_label) = cache.get_focused_window() {
        if let Some(window) = app.get_webview_window(&window_label) {
            return Some(window);
        }
    }

    app.get_webview_window("main")
        .or_else(|| app.webview_windows().values().next().cloned())
}

pub(super) fn emit_menu_action(app: &tauri::AppHandle, command_id: &str) {
    let payload = DesktopMenuActionEvent {
        command_id: command_id.to_string(),
    };
    if let Some(window) = focused_window(app) {
        let _ = window.emit(MENU_EVENT_NAME, payload);
        return;
    }
    let _ = app.emit(MENU_EVENT_NAME, payload);
}

#[cfg(debug_assertions)]
fn toggle_devtools_for_focused_window(app: &tauri::AppHandle) {
    let Some(window) = focused_window(app) else {
        return;
    };
    if window.is_devtools_open() {
        window.close_devtools();
    } else {
        window.open_devtools();
    }
}

#[cfg(not(debug_assertions))]
fn toggle_devtools_for_focused_window(_app: &tauri::AppHandle) {}

pub(super) fn handle_menu_command(app: &tauri::AppHandle, command_id: &str) {
    if command_id == CMD_VIEW_TOGGLE_DEVTOOLS {
        toggle_devtools_for_focused_window(app);
        return;
    }
    emit_menu_action(app, command_id);
}

pub(super) fn handle_app_menu_event(app: &tauri::AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    if !is_menu_command_id(id) {
        return;
    }
    handle_menu_command(app, id);
}

#[tauri::command]
pub(super) fn desktop_trigger_menu_command(
    app: tauri::AppHandle,
    command_id: String,
) -> Result<(), String> {
    #[cfg(feature = "automation")]
    {
        let command_id = command_id.trim().to_string();
        if command_id.is_empty() {
            return Err("command_id is required".to_string());
        }
        if !is_menu_command_id(&command_id) {
            return Err(format!("unknown menu command id: {command_id}"));
        }
        handle_menu_command(&app, &command_id);
        return Ok(());
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = app;
        let _ = command_id;
        Err("desktop_trigger_menu_command is automation-only".to_string())
    }
}
