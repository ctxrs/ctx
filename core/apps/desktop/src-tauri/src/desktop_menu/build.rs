use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, Submenu,
    SubmenuBuilder, HELP_SUBMENU_ID, WINDOW_SUBMENU_ID,
};

use super::ids::*;

fn menu_item(
    app: &tauri::AppHandle,
    id: &str,
    text: &str,
    accelerator: Option<&str>,
    enabled: bool,
) -> tauri::Result<tauri::menu::MenuItem<tauri::Wry>> {
    let mut builder = MenuItemBuilder::with_id(id, text).enabled(enabled);
    if let Some(accel) = accelerator {
        builder = builder.accelerator(accel);
    }
    builder.build(app)
}

fn check_item(
    app: &tauri::AppHandle,
    id: &str,
    text: &str,
    accelerator: Option<&str>,
    enabled: bool,
    checked: bool,
) -> tauri::Result<tauri::menu::CheckMenuItem<tauri::Wry>> {
    let mut builder = CheckMenuItemBuilder::with_id(id, text)
        .enabled(enabled)
        .checked(checked);
    if let Some(accel) = accelerator {
        builder = builder.accelerator(accel);
    }
    builder.build(app)
}

fn build_file_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let new_workspace = menu_item(
        app,
        CMD_FILE_NEW_WORKSPACE,
        "New Workspace...",
        Some("CmdOrCtrl+Shift+N"),
        true,
    )?;
    let new_window = menu_item(
        app,
        CMD_FILE_NEW_WINDOW,
        "New Window",
        Some("CmdOrCtrl+Shift+O"),
        true,
    )?;
    let open_recent = menu_item(app, CMD_FILE_OPEN_RECENT, "Open Recent", None, false)?;
    let export_transcript = menu_item(
        app,
        CMD_FILE_EXPORT_TRANSCRIPT,
        "Export Transcript",
        Some("CmdOrCtrl+Shift+E"),
        false,
    )?;
    let export_session_log = menu_item(
        app,
        CMD_FILE_EXPORT_SESSION_LOG,
        "Export Session Log",
        Some("CmdOrCtrl+Alt+E"),
        false,
    )?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let close_window = PredefinedMenuItem::close_window(app, None)?;

    SubmenuBuilder::new(app, "File")
        .item(&new_workspace)
        .item(&new_window)
        .item(&open_recent)
        .item(&sep1)
        .item(&export_transcript)
        .item(&export_session_log)
        .item(&sep2)
        .item(&close_window)
        .build()
}

fn build_edit_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let undo = PredefinedMenuItem::undo(app, None)?;
    let redo = PredefinedMenuItem::redo(app, None)?;
    let cut = PredefinedMenuItem::cut(app, None)?;
    let copy = PredefinedMenuItem::copy(app, None)?;
    let paste = PredefinedMenuItem::paste(app, None)?;
    let select_all = PredefinedMenuItem::select_all(app, None)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let find_tasks = menu_item(
        app,
        CMD_VIEW_FIND_TASKS,
        "Find Tasks",
        Some("CmdOrCtrl+F"),
        false,
    )?;

    SubmenuBuilder::new(app, "Edit")
        .item(&undo)
        .item(&redo)
        .item(&sep1)
        .item(&cut)
        .item(&copy)
        .item(&paste)
        .item(&select_all)
        .item(&sep2)
        .item(&find_tasks)
        .build()
}

fn build_view_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let toggle_sidebar = check_item(
        app,
        CMD_VIEW_TOGGLE_SIDEBAR,
        "Toggle Sidebar",
        Some("CmdOrCtrl+B"),
        false,
        false,
    )?;
    let toggle_diff = check_item(
        app,
        CMD_VIEW_TOGGLE_DIFF,
        "Toggle Diff Pane",
        None,
        false,
        false,
    )?;
    let toggle_artifacts = check_item(
        app,
        CMD_VIEW_TOGGLE_ARTIFACTS,
        "Toggle Artifacts Pane",
        None,
        false,
        false,
    )?;
    let toggle_sessions = check_item(
        app,
        CMD_VIEW_TOGGLE_SESSIONS,
        "Toggle Sessions Pane",
        None,
        false,
        false,
    )?;
    let toggle_terminal = check_item(
        app,
        CMD_VIEW_TOGGLE_TERMINAL,
        "Toggle Terminal",
        Some("Ctrl+`"),
        false,
        false,
    )?;
    #[cfg(debug_assertions)]
    let toggle_devtools = Some(menu_item(
        app,
        CMD_VIEW_TOGGLE_DEVTOOLS,
        "Toggle Developer Tools",
        Some("CmdOrCtrl+Alt+I"),
        true,
    )?);
    #[cfg(not(debug_assertions))]
    let toggle_devtools: Option<tauri::menu::MenuItem<tauri::Wry>> = None;
    let sep = PredefinedMenuItem::separator(app)?;
    let fullscreen = PredefinedMenuItem::fullscreen(app, None)?;

    let mut builder = SubmenuBuilder::new(app, "View")
        .item(&toggle_sidebar)
        .item(&toggle_diff)
        .item(&toggle_artifacts)
        .item(&toggle_sessions)
        .item(&toggle_terminal);
    if let Some(toggle_devtools) = toggle_devtools.as_ref() {
        builder = builder.item(toggle_devtools);
    }
    builder.item(&sep).item(&fullscreen).build()
}

fn build_task_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let new_task = menu_item(app, CMD_TASK_NEW, "New Task", Some("CmdOrCtrl+N"), false)?;
    let rename_task = menu_item(app, CMD_TASK_RENAME, "Rename Task", Some("F2"), false)?;
    let archive_toggle = menu_item(
        app,
        CMD_TASK_ARCHIVE_TOGGLE,
        "Archive/Unarchive",
        None,
        false,
    )?;
    let mark_read_toggle = menu_item(
        app,
        CMD_TASK_MARK_READ_TOGGLE,
        "Mark Read/Unread",
        None,
        false,
    )?;
    let delete_task = menu_item(
        app,
        CMD_TASK_DELETE,
        "Delete Task",
        Some("CmdOrCtrl+Backspace"),
        false,
    )?;

    SubmenuBuilder::new(app, "Task")
        .item(&new_task)
        .item(&rename_task)
        .item(&archive_toggle)
        .item(&mark_read_toggle)
        .item(&delete_task)
        .build()
}

fn build_session_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let copy_transcript = menu_item(
        app,
        CMD_SESSION_COPY_TRANSCRIPT,
        "Copy Transcript",
        Some("CmdOrCtrl+Shift+C"),
        false,
    )?;
    let copy_session_log = menu_item(
        app,
        CMD_SESSION_COPY_SESSION_LOG,
        "Copy Session Log",
        Some("CmdOrCtrl+Alt+C"),
        false,
    )?;
    let copy_worktree = menu_item(
        app,
        CMD_SESSION_COPY_WORKTREE_LOCATION,
        "Copy Worktree Location",
        None,
        false,
    )?;
    let copy_task_id = menu_item(app, CMD_SESSION_COPY_TASK_ID, "Copy Task ID", None, false)?;
    let open_terminal = menu_item(
        app,
        CMD_SESSION_OPEN_WORKTREE_TERMINAL,
        "Open Worktree Terminal",
        None,
        false,
    )?;
    let interrupt = menu_item(
        app,
        CMD_SESSION_INTERRUPT,
        "Interrupt Run",
        Some("CmdOrCtrl+."),
        false,
    )?;

    SubmenuBuilder::new(app, "Session")
        .item(&copy_transcript)
        .item(&copy_session_log)
        .item(&copy_worktree)
        .item(&copy_task_id)
        .item(&open_terminal)
        .item(&interrupt)
        .build()
}

fn build_go_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let launcher = menu_item(app, CMD_GO_LAUNCHER, "Launcher", Some("CmdOrCtrl+1"), true)?;
    let workspace_setup = menu_item(
        app,
        CMD_GO_WORKSPACE_SETUP,
        "Workspace Setup",
        Some("CmdOrCtrl+Shift+S"),
        true,
    )?;
    let settings = menu_item(app, CMD_GO_SETTINGS, "Settings", Some("CmdOrCtrl+,"), true)?;
    let diagnostics = menu_item(
        app,
        CMD_GO_DIAGNOSTICS,
        "Diagnostics",
        Some("CmdOrCtrl+3"),
        true,
    )?;
    let harnesses = menu_item(
        app,
        CMD_GO_AGENT_HARNESSES,
        "Agent Harnesses",
        Some("CmdOrCtrl+Shift+A"),
        true,
    )?;

    SubmenuBuilder::new(app, "Go")
        .item(&launcher)
        .item(&workspace_setup)
        .item(&settings)
        .item(&diagnostics)
        .item(&harnesses)
        .build()
}

fn macos_window_submenu_id() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Some(WINDOW_SUBMENU_ID)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn macos_help_submenu_id() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Some(HELP_SUBMENU_ID)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn build_window_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let minimize = PredefinedMenuItem::minimize(app, None)?;
    let maximize = PredefinedMenuItem::maximize(app, None)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let close_window = PredefinedMenuItem::close_window(app, None)?;

    #[cfg(target_os = "macos")]
    {
        let show_all = PredefinedMenuItem::show_all(app, None)?;
        let submenu_id = macos_window_submenu_id().unwrap_or("window");
        return SubmenuBuilder::with_id(app, submenu_id, "Window")
            .item(&minimize)
            .item(&maximize)
            .item(&sep)
            .item(&close_window)
            .item(&show_all)
            .build();
    }

    #[cfg(not(target_os = "macos"))]
    {
        SubmenuBuilder::new(app, "Window")
            .item(&minimize)
            .item(&maximize)
            .item(&sep)
            .item(&close_window)
            .build()
    }
}

fn build_help_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let keyboard_shortcuts = menu_item(
        app,
        CMD_HELP_KEYBOARD_SHORTCUTS,
        "Keyboard Shortcuts",
        Some("CmdOrCtrl+/"),
        true,
    )?;
    let check_for_updates = menu_item(
        app,
        CMD_HELP_CHECK_FOR_UPDATES,
        "Check for Updates...",
        None,
        true,
    )?;
    let open_logs_folder = menu_item(
        app,
        CMD_HELP_OPEN_LOGS_FOLDER,
        "Open Logs Folder",
        None,
        true,
    )?;
    let report_issue = menu_item(app, CMD_HELP_REPORT_ISSUE, "Report Issue", None, true)?;

    #[cfg(target_os = "macos")]
    {
        let submenu_id = macos_help_submenu_id().unwrap_or("help");
        return SubmenuBuilder::with_id(app, submenu_id, "Help")
            .item(&keyboard_shortcuts)
            .item(&check_for_updates)
            .item(&open_logs_folder)
            .item(&report_issue)
            .build();
    }

    #[cfg(not(target_os = "macos"))]
    {
        SubmenuBuilder::new(app, "Help")
            .item(&keyboard_shortcuts)
            .item(&check_for_updates)
            .item(&open_logs_folder)
            .item(&report_issue)
            .build()
    }
}

#[cfg(target_os = "macos")]
fn build_app_submenu(app: &tauri::AppHandle) -> tauri::Result<Submenu<tauri::Wry>> {
    let about = PredefinedMenuItem::about(app, None, None)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let new_workspace = menu_item(
        app,
        CMD_FILE_NEW_WORKSPACE,
        "New Workspace",
        Some("CmdOrCtrl+N"),
        true,
    )?;
    let new_window = menu_item(
        app,
        CMD_FILE_NEW_WINDOW,
        "New Window",
        Some("CmdOrCtrl+Shift+O"),
        true,
    )?;
    let settings = menu_item(
        app,
        CMD_GO_SETTINGS,
        "Settings...",
        Some("CmdOrCtrl+,"),
        true,
    )?;
    let hide = PredefinedMenuItem::hide(app, None)?;
    let hide_others = PredefinedMenuItem::hide_others(app, None)?;
    let show_all = PredefinedMenuItem::show_all(app, None)?;
    let quit = PredefinedMenuItem::quit(app, None)?;

    SubmenuBuilder::new(app, "ctx")
        .item(&about)
        .item(&sep1)
        .item(&new_workspace)
        .item(&new_window)
        .item(&sep2)
        .item(&settings)
        .item(&sep3)
        .item(&hide)
        .item(&hide_others)
        .item(&show_all)
        .item(&quit)
        .build()
}

pub(crate) fn build_app_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let file = build_file_submenu(app)?;
    let edit = build_edit_submenu(app)?;
    let view = build_view_submenu(app)?;
    let task = build_task_submenu(app)?;
    let session = build_session_submenu(app)?;
    let go = build_go_submenu(app)?;
    let window = build_window_submenu(app)?;
    let help = build_help_submenu(app)?;

    let mut builder = MenuBuilder::new(app);

    #[cfg(target_os = "macos")]
    {
        let app_submenu = build_app_submenu(app)?;
        builder = builder.item(&app_submenu);
    }

    builder
        .item(&file)
        .item(&edit)
        .item(&view)
        .item(&task)
        .item(&session)
        .item(&go)
        .item(&window)
        .item(&help)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_related_submenu_ids_match_platform_expectations() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(macos_window_submenu_id(), Some(WINDOW_SUBMENU_ID));
            assert_eq!(macos_help_submenu_id(), Some(HELP_SUBMENU_ID));
        }

        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(macos_window_submenu_id(), None);
            assert_eq!(macos_help_submenu_id(), None);
        }
    }
}
