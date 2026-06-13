use anyhow::Context;
use ctx_desktop_ipc::DesktopWebviewSurface;
use tauri::Manager;

use super::super::{
    apply_workbench_titlebar, ensure_workbench_titlebar, open_file_preview_window_with_label,
    open_launcher_window_with_label, open_main_window_at_route, open_settings_window_at_route,
    open_workspace_setup_window_with_label, open_workspace_target_in_window_with_label,
    workspace_registry_parse_target_route, WorkspaceWindowRegistry,
};

pub(super) fn surface_for_window(window_label: &str, route: &str) -> DesktopWebviewSurface {
    let label = window_label.trim();
    if route.starts_with("/workspaces/") {
        return DesktopWebviewSurface::Workbench;
    }
    if route.starts_with("/workspace-setup") {
        return DesktopWebviewSurface::WorkspaceSetup;
    }
    if route.starts_with("/settings") {
        return DesktopWebviewSurface::Settings;
    }
    if route.starts_with("/file") {
        return DesktopWebviewSurface::FilePreview;
    }
    if route == "/" {
        return DesktopWebviewSurface::Launcher;
    }
    if label == "main" {
        return DesktopWebviewSurface::Main;
    }
    if label == "settings" {
        return DesktopWebviewSurface::Settings;
    }
    if label.starts_with("workbench:") {
        return DesktopWebviewSurface::Workbench;
    }
    if label.starts_with("launcher:") {
        return DesktopWebviewSurface::Launcher;
    }
    if label.starts_with("file:") {
        return DesktopWebviewSurface::FilePreview;
    }
    if label.starts_with("workspace-setup:") {
        return DesktopWebviewSurface::WorkspaceSetup;
    }
    DesktopWebviewSurface::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_route_overrides_setup_window_label_after_launch() {
        assert_eq!(
            surface_for_window(
                "workspace-setup:ccde4a9c-9881-4b09-8001-764bf28d9a4a",
                "/workspaces/4c8db9d3-2290-474d-a3ed-7b1856693669"
            ),
            DesktopWebviewSurface::Workbench
        );
    }
}

pub(super) fn reopen_window(
    app: &tauri::AppHandle,
    window_label: &str,
    surface: DesktopWebviewSurface,
    route: &str,
) -> anyhow::Result<()> {
    match surface {
        DesktopWebviewSurface::Main => open_main_window_at_route(app, route),
        DesktopWebviewSurface::Workbench => {
            let registry = app.state::<WorkspaceWindowRegistry>();
            let target = workspace_registry_parse_target_route(route)
                .with_context(|| format!("parsing workbench route '{route}'"))?;
            open_workspace_target_in_window_with_label(
                app,
                &registry,
                window_label,
                &target.workspace_id,
                target.task_id.as_deref(),
                target.session_id.as_deref(),
            )
        }
        DesktopWebviewSurface::Launcher => {
            open_launcher_window_with_label(app, window_label, route)
        }
        DesktopWebviewSurface::Settings => open_settings_window_at_route(app, window_label, route),
        DesktopWebviewSurface::FilePreview => {
            open_file_preview_window_with_label(app, window_label, route)
        }
        DesktopWebviewSurface::WorkspaceSetup => {
            open_workspace_setup_window_with_label(app, window_label, route)
        }
        DesktopWebviewSurface::Unknown => {
            let init_script =
                super::super::desktop_startup_initialization_script(window_label, route);
            let builder = tauri::WebviewWindowBuilder::new(
                app,
                window_label,
                tauri::WebviewUrl::App(route.to_string().into()),
            )
            .title("")
            .inner_size(1200.0, 900.0)
            .initialization_script(&init_script);
            let window = apply_workbench_titlebar(builder)
                .build()
                .context("creating generic recovery window")?;
            ensure_workbench_titlebar(&window)?;
            super::super::log_window_created(window_label, route);
            super::super::log_navigation_start(window_label, route, "recreate_window");
            let _ = window.show();
            let _ = window.set_focus();
            Ok(())
        }
    }
}
