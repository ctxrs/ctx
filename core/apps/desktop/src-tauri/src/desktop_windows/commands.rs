use super::*;

#[tauri::command]
pub(crate) fn desktop_set_open_workspaces(
    window: tauri::WebviewWindow,
    state: tauri::State<ConnectionManager>,
    registry: tauri::State<WorkspaceWindowRegistry>,
    req: DesktopSetOpenWorkspacesReq,
) -> Result<(), String> {
    let daemon_key = state.daemon_target_key_for_scope(window.label());
    registry.set_window_workspaces_for_daemon(
        window.label(),
        daemon_key.as_deref(),
        req.workspace_ids,
    );
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_open_workspace_in_new_window(
    app: tauri::AppHandle,
    registry: tauri::State<WorkspaceWindowRegistry>,
    req: DesktopOpenWorkspaceInNewWindowReq,
) -> Result<(), String> {
    let workspace_id = req.workspace_id.trim();
    if workspace_id.is_empty() {
        return Err("workspace_id is required".to_string());
    }
    open_workspace_in_new_window(&app, &registry, workspace_id).map_err(to_err)
}

#[tauri::command]
pub(crate) fn desktop_open_launcher_in_new_window(app: tauri::AppHandle) -> Result<(), String> {
    open_launcher_window(&app).map_err(to_err)
}

#[tauri::command]
pub(crate) fn desktop_open_workspace_setup_in_new_window(
    app: tauri::AppHandle,
) -> Result<(), String> {
    let label = format!("workspace-setup:{}", uuid::Uuid::new_v4());
    open_workspace_setup_window_with_label(&app, &label, "/workspace-setup").map_err(to_err)
}

pub(crate) fn open_workspace_setup_window_with_label(
    app: &tauri::AppHandle,
    label: &str,
    route: &str,
) -> Result<()> {
    let init_script = desktop_startup_initialization_script(label, route);
    let builder =
        tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App(route.into()))
            .title("ctx")
            .inner_size(1200.0, 900.0)
            .initialization_script(&init_script);
    let window = apply_workbench_titlebar(builder)
        .build()
        .context("creating workspace setup window failed")?;
    ensure_workbench_titlebar(&window)?;
    log_window_created(label, route);
    log_navigation_start(label, route, "workspace_setup_window");
    register_window_for_recovery(app, label, route);
    #[cfg(target_os = "macos")]
    {
        let _ = install_macos_settings_button(app, &window);
    }
    let _ = window.show();
    let _ = window.set_focus();
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_set_titlebar_color(
    window: tauri::WebviewWindow,
    req: DesktopTitlebarColor,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let clamp_unit = |value: f64| (value.max(0.0).min(255.0) / 255.0) as CGFloat;
        let alpha = req.a.unwrap_or(1.0).max(0.0).min(1.0) as CGFloat;
        let r = clamp_unit(req.r);
        let g = clamp_unit(req.g);
        let b = clamp_unit(req.b);
        window
            .with_webview(move |webview| unsafe {
                let Some(_mtm) = MainThreadMarker::new() else {
                    return;
                };
                let ns_window: &NSWindow = &*webview.ns_window().cast();
                ns_window.setTitlebarAppearsTransparent(false);
                ns_window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
                ns_window.setTitleVisibility(NSWindowTitleVisibility::Visible);
                let bg = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha);
                ns_window.setBackgroundColor(Some(&bg));
            })
            .map_err(|e| format!("failed to set titlebar color: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_set_window_title(
    window: tauri::WebviewWindow,
    req: DesktopSetWindowTitleReq,
) -> Result<(), String> {
    window
        .set_title(&req.title)
        .map_err(|e| format!("failed to set window title: {e}"))?;
    #[cfg(target_os = "macos")]
    {
        let _ = window.with_webview(|webview| unsafe {
            let Some(_mtm) = MainThreadMarker::new() else {
                return;
            };
            let ns_window: &NSWindow = &*webview.ns_window().cast();
            ns_window.setTitleVisibility(NSWindowTitleVisibility::Visible);
        });
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_register_workspace_window(
    registry: tauri::State<WorkspaceWindowRegistry>,
    state: tauri::State<ConnectionManager>,
    workspace_id: String,
    window_label: String,
) -> Result<(), String> {
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        return Err("workspace_id is required".to_string());
    }
    let window_label = window_label.trim();
    if window_label.is_empty() {
        return Err("window_label is required".to_string());
    }
    let daemon_key = state.daemon_target_key_for_scope(window_label);
    registry.register_for_daemon(window_label, daemon_key.as_deref(), workspace_id);
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_unregister_workspace_window(
    registry: tauri::State<WorkspaceWindowRegistry>,
    window_label: String,
) -> Result<(), String> {
    let window_label = window_label.trim();
    if window_label.is_empty() {
        return Err("window_label is required".to_string());
    }
    registry.unregister_window(window_label);
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_record_workspace_visit(
    window: tauri::WebviewWindow,
    state: tauri::State<ConnectionManager>,
    registry: tauri::State<WorkspaceWindowRegistry>,
    req: DesktopRecordWorkspaceVisitReq,
) -> Result<(), String> {
    let workspace_id = req.workspace_id.trim();
    if workspace_id.is_empty() {
        return Err("workspace_id is required".to_string());
    }
    let daemon_key = state.daemon_target_key_for_scope(window.label());
    registry.set_window_workspaces_for_daemon(
        window.label(),
        daemon_key.as_deref(),
        vec![workspace_id.to_string()],
    );
    registry.record_recent_workspace(workspace_id, Some(&req.workspace_label));
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_record_workbench_route(
    window: tauri::WebviewWindow,
    state: tauri::State<ConnectionManager>,
    registry: tauri::State<WorkspaceWindowRegistry>,
    req: DesktopRecordWorkbenchRouteReq,
) -> Result<(), String> {
    let workspace_id = req.workspace_id.trim().to_string();
    if workspace_id.is_empty() {
        return Err("workspace_id is required".to_string());
    }
    let workspace_label = req.workspace_label.clone();
    let daemon_key = state.daemon_target_key_for_scope(window.label());
    registry.record_workbench_route_for_daemon(window.label(), daemon_key.as_deref(), req);
    registry.record_recent_workspace(&workspace_id, Some(&workspace_label));
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_consume_pending_task_route(
    window: tauri::WebviewWindow,
    registry: tauri::State<WorkspaceWindowRegistry>,
) -> Result<Option<DesktopTaskRoutePayload>, String> {
    Ok(registry.consume_pending_task_route(window.label()))
}

#[tauri::command]
pub(crate) fn desktop_ack_task_route(
    window: tauri::WebviewWindow,
    registry: tauri::State<WorkspaceWindowRegistry>,
    req: DesktopTaskRouteAckReq,
) -> Result<(), String> {
    registry.acknowledge_pending_task_route(window.label(), &req.route_id);
    Ok(())
}

#[tauri::command]
pub(crate) fn desktop_set_dock_recent_local_workspaces(
    registry: tauri::State<WorkspaceWindowRegistry>,
    req: DesktopSetDockRecentLocalWorkspacesReq,
) -> Result<(), String> {
    registry.set_dock_recent_local_workspaces(req.entries);
    Ok(())
}
