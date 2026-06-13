use super::*;

mod commands;
mod macos;
mod navigation;
mod registry;

pub(super) use commands::{
    desktop_ack_task_route, desktop_consume_pending_task_route,
    desktop_open_launcher_in_new_window, desktop_open_workspace_in_new_window,
    desktop_open_workspace_setup_in_new_window, desktop_record_workbench_route,
    desktop_record_workspace_visit, desktop_register_workspace_window,
    desktop_set_dock_recent_local_workspaces, desktop_set_open_workspaces,
    desktop_set_titlebar_color, desktop_set_window_title, desktop_unregister_workspace_window,
    open_workspace_setup_window_with_label,
};
pub(super) use macos::{apply_workbench_titlebar, ensure_workbench_titlebar};
#[cfg(target_os = "macos")]
pub(super) use macos::{
    emit_settings_inplace, install_macos_settings_button, load_lucide_settings_icon,
    settings_button_target_class, SETTINGS_BUTTON_APP, SETTINGS_BUTTON_CLASS,
};
pub(super) use navigation::{
    confirm_action, desktop_startup_initialization_script, focus_app_window,
    focus_or_open_workspace_target, focus_or_open_workspace_task_target,
    focus_or_open_workspace_window, log_navigation_start, log_window_created, open_launcher_window,
    open_launcher_window_with_label, open_main_window, open_main_window_at_route,
    open_workspace_in_new_window, open_workspace_target, open_workspace_target_in_new_window,
    open_workspace_target_in_window_with_label, open_workspace_window, reveal_in_file_manager,
    show_error_dialog, workspace_registry_parse_target_route, WorkspaceRouteTarget,
};
pub(super) use registry::MAX_RECENT_WORKSPACES;
pub(crate) use registry::{WorkspaceTaskWindowLookup, WorkspaceWindowRegistry};
