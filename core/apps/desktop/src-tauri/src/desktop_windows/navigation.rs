use super::*;

pub(crate) fn desktop_startup_initialization_script(
    window_label: &str,
    start_path: &str,
) -> String {
    let window_created_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!(
        "window.__CTX_DESKTOP_STARTUP__ = Object.assign({{}}, window.__CTX_DESKTOP_STARTUP__, {{ windowCreatedAtMs: {window_created_at_ms}, windowLabel: {}, startPath: {} }});",
        serde_json::to_string(window_label).unwrap_or_else(|_| "\"unknown\"".to_string()),
        serde_json::to_string(start_path).unwrap_or_else(|_| "\"/\"".to_string()),
    )
}

pub(crate) fn log_window_created(window_label: &str, path: &str) {
    log_desktop_startup(&format!(
        "desktop_startup: window_created label={} path={}",
        serde_json::to_string(window_label).unwrap_or_else(|_| "\"unknown\"".to_string()),
        serde_json::to_string(path).unwrap_or_else(|_| "\"/\"".to_string()),
    ));
}

pub(crate) fn log_navigation_start(window_label: &str, path: &str, reason: &str) {
    log_desktop_startup(&format!(
        "desktop_startup: navigation_start label={} path={} reason={}",
        serde_json::to_string(window_label).unwrap_or_else(|_| "\"unknown\"".to_string()),
        serde_json::to_string(path).unwrap_or_else(|_| "\"/\"".to_string()),
        serde_json::to_string(reason).unwrap_or_else(|_| "\"unknown\"".to_string()),
    ));
}

fn build_workspace_url(
    workspace_id: &str,
    task_id: Option<&str>,
    session_id: Option<&str>,
) -> String {
    let mut url = format!("/workspaces/{workspace_id}");
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    let mut has_params = false;
    if let Some(task_id) = task_id.map(str::trim).filter(|value| !value.is_empty()) {
        params.append_pair("task", task_id);
        has_params = true;
    }
    if let Some(session_id) = session_id.map(str::trim).filter(|value| !value.is_empty()) {
        params.append_pair("session", session_id);
        has_params = true;
    }
    if has_params {
        url.push('?');
        url.push_str(&params.finish());
    }
    url
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceRouteTarget {
    pub(crate) workspace_id: String,
    pub(crate) task_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

pub(crate) fn workspace_registry_parse_target_route(route: &str) -> Result<WorkspaceRouteTarget> {
    let parsed = Url::parse(&format!("https://ctx.invalid{}", route.trim()))
        .with_context(|| format!("parsing workbench recovery route '{route}'"))?;
    let mut segments = parsed
        .path_segments()
        .ok_or_else(|| anyhow!("missing path segments"))?;
    let Some(root) = segments.next() else {
        anyhow::bail!("missing workbench route root");
    };
    if root != "workspaces" {
        anyhow::bail!("unsupported workbench route root '{root}'");
    }
    let Some(workspace_id) = segments
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        anyhow::bail!("missing workspace id");
    };
    let mut task_id = None;
    let mut session_id = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "task" if !value.trim().is_empty() => task_id = Some(value.to_string()),
            "session" if !value.trim().is_empty() => session_id = Some(value.to_string()),
            _ => {}
        }
    }
    Ok(WorkspaceRouteTarget {
        workspace_id: workspace_id.to_string(),
        task_id,
        session_id,
    })
}

fn workspace_target_for_main_route(route: &str) -> Result<Option<WorkspaceRouteTarget>> {
    if !route.trim().starts_with("/workspaces/") {
        return Ok(None);
    }
    workspace_registry_parse_target_route(route).map(Some)
}

fn navigate_window_to_workspace(
    window: &tauri::WebviewWindow,
    window_label: &str,
    workspace_id: &str,
    task_id: Option<&str>,
    session_id: Option<&str>,
    reason: &str,
) {
    let url = build_workspace_url(workspace_id, task_id, session_id);
    log_navigation_start(window_label, &url, reason);
    record_window_route(&window.app_handle(), window_label, &url);
    let js = format!(
        "window.location.href = {};",
        serde_json::to_string(&url).unwrap_or_else(|_| "\"/\"".to_string())
    );
    let _ = window.eval(&js);
    let _ = window.emit("workspace:open", workspace_id.to_string());
}

pub(crate) const DESKTOP_TASK_DEEPLINK_OPEN_EVENT: &str = "desktop_task_deeplink_open";

fn daemon_key_for_window_label(app: &tauri::AppHandle, window_label: &str) -> Option<String> {
    app.try_state::<ConnectionManager>()
        .and_then(|state| state.daemon_target_key_for_scope(window_label))
}

fn register_workspace_for_window_target(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    window_label: &str,
    workspace_id: &str,
) {
    let daemon_key = daemon_key_for_window_label(app, window_label);
    registry.register_for_daemon(window_label, daemon_key.as_deref(), workspace_id);
}

pub(crate) fn open_workspace_window(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    workspace_id: &str,
) -> Result<()> {
    open_workspace_target(app, registry, workspace_id, None, None)
}

pub(crate) fn open_workspace_target(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    workspace_id: &str,
    task_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<()> {
    if let Some(window_label) = registry.window_for_workspace(workspace_id) {
        if let Some(window) = app.get_webview_window(&window_label) {
            let _ = window.show();
            let _ = window.set_focus();
            navigate_window_to_workspace(
                &window,
                &window_label,
                workspace_id,
                task_id,
                session_id,
                "reuse_window",
            );
            register_workspace_for_window_target(app, registry, &window_label, workspace_id);
            registry.record_recent_workspace(workspace_id, None);
            return Ok(());
        }
        registry.unregister_window(&window_label);
    }

    open_main_window(app)?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow!("main window not available"))?;
    let _ = window.show();
    let _ = window.set_focus();

    navigate_window_to_workspace(
        &window,
        "main",
        workspace_id,
        task_id,
        session_id,
        "reuse_main_window",
    );
    register_workspace_for_window_target(app, registry, "main", workspace_id);
    registry.record_recent_workspace(workspace_id, None);
    Ok(())
}

pub(crate) fn open_workspace_in_new_window(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    workspace_id: &str,
) -> Result<()> {
    open_workspace_target_in_new_window(app, registry, workspace_id, None, None)
}

pub(crate) fn open_workspace_target_in_new_window(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    workspace_id: &str,
    task_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<()> {
    let label = format!("workbench:{}", uuid::Uuid::new_v4());
    open_workspace_target_in_window_with_label(
        app,
        registry,
        &label,
        workspace_id,
        task_id,
        session_id,
    )
}

pub(crate) fn open_workspace_target_in_window_with_label(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    label: &str,
    workspace_id: &str,
    task_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<()> {
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        anyhow::bail!("workspace_id is required");
    }
    let url = build_workspace_url(workspace_id, task_id, session_id);
    let init_script = desktop_startup_initialization_script(&label, &url);
    let builder = tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App(url.into()))
        .title("")
        .inner_size(1200.0, 900.0)
        .initialization_script(&init_script);
    let window = apply_workbench_titlebar(builder)
        .build()
        .context("creating window failed")?;
    ensure_workbench_titlebar(&window)?;
    let path = build_workspace_url(workspace_id, task_id, session_id);
    log_window_created(&label, &path);
    log_navigation_start(&label, &path, "new_window");
    register_window_for_recovery(app, &label, &path);
    #[cfg(target_os = "macos")]
    {
        let _ = install_macos_settings_button(app, &window);
    }
    let _ = window.show();
    let _ = window.set_focus();
    register_workspace_for_window_target(app, registry, &label, workspace_id);
    registry.record_recent_workspace(workspace_id, None);
    Ok(())
}

pub(crate) fn focus_or_open_workspace_window(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    workspace_id: &str,
) -> Result<()> {
    focus_or_open_workspace_target(app, registry, workspace_id, None, None)
}

pub(crate) fn focus_or_open_workspace_target(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    workspace_id: &str,
    task_id: Option<&str>,
    session_id: Option<&str>,
) -> Result<()> {
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        anyhow::bail!("workspace_id is required");
    }

    if let Some(window_label) = registry.window_for_workspace(workspace_id) {
        if let Some(window) = app.get_webview_window(&window_label) {
            let _ = window.show();
            let _ = window.set_focus();
            navigate_window_to_workspace(
                &window,
                &window_label,
                workspace_id,
                task_id,
                session_id,
                "focus_existing_window",
            );
            register_workspace_for_window_target(app, registry, &window_label, workspace_id);
            registry.record_recent_workspace(workspace_id, None);
            return Ok(());
        }
        registry.unregister_window(&window_label);
    }
    open_workspace_target_in_new_window(app, registry, workspace_id, task_id, session_id)
}

pub(crate) fn focus_or_open_workspace_task_target(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    workspace_id: &str,
    task_id: &str,
    session_id: Option<&str>,
    daemon_key: Option<&str>,
    route_id: Option<&str>,
    preferred_window_label: Option<&str>,
    allow_new_window: bool,
) -> Result<()> {
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        anyhow::bail!("workspace_id is required");
    }
    let task_id = task_id.trim();
    if task_id.is_empty() {
        anyhow::bail!("task_id is required");
    }

    for _ in 0..2 {
        match registry.window_for_task_target(
            workspace_id,
            task_id,
            session_id,
            daemon_key,
            preferred_window_label,
        ) {
            WorkspaceTaskWindowLookup::Match {
                window_label,
                reason: _,
            } => {
                if let Some(window) = app.get_webview_window(&window_label) {
                    focus_existing_task_window(
                        app,
                        registry,
                        &window,
                        &window_label,
                        workspace_id,
                        task_id,
                        session_id,
                        route_id,
                    )?;
                    return Ok(());
                }
                registry.unregister_window(&window_label);
            }
            WorkspaceTaskWindowLookup::AmbiguousWorkspace { window_labels } => {
                let stale_labels = window_labels
                    .iter()
                    .filter(|label| app.get_webview_window(label).is_none())
                    .cloned()
                    .collect::<Vec<_>>();
                if stale_labels.is_empty() {
                    break;
                }
                for label in stale_labels {
                    registry.unregister_window(&label);
                }
            }
            WorkspaceTaskWindowLookup::None => break,
        }
    }

    if !allow_new_window {
        anyhow::bail!("task route has no live window for its scoped daemon target");
    }

    open_workspace_target_in_new_window(app, registry, workspace_id, Some(task_id), session_id)
}

fn focus_existing_task_window(
    app: &tauri::AppHandle,
    registry: &WorkspaceWindowRegistry,
    window: &tauri::WebviewWindow,
    window_label: &str,
    workspace_id: &str,
    task_id: &str,
    session_id: Option<&str>,
    route_id: Option<&str>,
) -> Result<()> {
    let _ = window.show();
    let _ = window.set_focus();
    let route = build_workspace_url(workspace_id, Some(task_id), session_id);
    record_window_route(app, window_label, &route);
    registry.record_recent_workspace(workspace_id, None);
    let payload = DesktopTaskRoutePayload {
        route_id: route_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        session_id: session_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        task_id: task_id.to_string(),
        workspace_id: workspace_id.to_string(),
    };
    registry.set_pending_task_route(window_label, payload.clone());
    window
        .emit(DESKTOP_TASK_DEEPLINK_OPEN_EVENT, payload)
        .context("emitting desktop task deep-link event")?;
    Ok(())
}

pub(crate) fn open_launcher_window(app: &tauri::AppHandle) -> Result<()> {
    let label = format!("launcher:{}", uuid::Uuid::new_v4());
    open_launcher_window_with_label(app, &label, "/")
}

pub(crate) fn open_launcher_window_with_label(
    app: &tauri::AppHandle,
    label: &str,
    start_path: &str,
) -> Result<()> {
    let init_script = desktop_startup_initialization_script(label, start_path);
    let builder =
        tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App(start_path.into()))
            .title("")
            .inner_size(1200.0, 900.0)
            .initialization_script(&init_script);
    let window = apply_workbench_titlebar(builder)
        .build()
        .context("creating launcher window failed")?;
    ensure_workbench_titlebar(&window)?;
    log_window_created(label, start_path);
    log_navigation_start(label, start_path, "launcher_window");
    register_window_for_recovery(app, label, start_path);
    #[cfg(target_os = "macos")]
    {
        let _ = install_macos_settings_button(app, &window);
    }
    let _ = window.show();
    let _ = window.set_focus();
    Ok(())
}

pub(crate) fn focus_app_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    if let Some(window) = app.webview_windows().values().next() {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub(crate) fn confirm_action(app: &tauri::AppHandle, message: &str) -> bool {
    app.dialog()
        .message(message)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Open".into(),
            "Cancel".into(),
        ))
        .blocking_show()
}

pub(crate) fn show_error_dialog(app: &tauri::AppHandle, message: &str) {
    app.dialog()
        .message(message)
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .show(|_| {});
}

pub(crate) fn reveal_in_file_manager(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = Command::new("open").arg("-R").arg(path).status()?;
        if !status.success() {
            anyhow::bail!("failed to reveal file (exit={status})");
        }
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let target = path.to_string_lossy();
        let status = Command::new("explorer")
            .arg("/select,")
            .arg(target.as_ref())
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to reveal file (exit={status})");
        }
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let dir = path.parent().unwrap_or(path);
        let status = Command::new("xdg-open").arg(dir).status()?;
        if !status.success() {
            anyhow::bail!("failed to reveal file (exit={status})");
        }
        Ok(())
    }
}

pub(crate) fn open_main_window(app: &tauri::AppHandle) -> Result<()> {
    let start_path = match std::env::var("CTX_DESKTOP_START_PATH") {
        Ok(v) if v.trim().starts_with('/') => v.trim().to_string(),
        _ => "/".to_string(),
    };
    open_main_window_at_route(app, &start_path)
}

pub(crate) fn open_main_window_at_route(app: &tauri::AppHandle, start_path: &str) -> Result<()> {
    if app.get_webview_window("main").is_some() {
        return Ok(());
    }
    let main_workspace_target = workspace_target_for_main_route(start_path)?;
    let init_script = desktop_startup_initialization_script("main", start_path);
    let mut builder = tauri::WebviewWindowBuilder::new(
        app,
        "main",
        tauri::WebviewUrl::App(start_path.to_string().into()),
    )
    .title("")
    .initialization_script(&init_script);
    if let Ok(Some(monitor)) = app.primary_monitor() {
        let size = monitor.size();
        let width = (size.width as f64 * 0.9).round().max(1200.0);
        let height = (size.height as f64 * 0.9).round().max(900.0);
        builder = builder.inner_size(width, height);
    } else {
        builder = builder.inner_size(1200.0, 900.0);
    }
    let builder = apply_workbench_titlebar(builder);
    let window = builder.build().context("creating window")?;
    ensure_workbench_titlebar(&window)?;
    log_window_created("main", start_path);
    log_navigation_start("main", start_path, "main_window");
    register_window_for_recovery(app, "main", start_path);
    if let Some(target) = main_workspace_target {
        let registry = app.state::<WorkspaceWindowRegistry>();
        register_workspace_for_window_target(app, &registry, "main", &target.workspace_id);
        registry.record_recent_workspace(&target.workspace_id, None);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = install_macos_settings_button(app, &window);
    }
    let _ = window.show();
    let _ = window.set_focus();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_workspace_url_includes_optional_task_and_session() {
        assert_eq!(
            build_workspace_url("ws-1", Some("task-1"), Some("session-1")),
            "/workspaces/ws-1?task=task-1&session=session-1"
        );
        assert_eq!(
            build_workspace_url("ws-1", Some(" task-2 "), Some("  ")),
            "/workspaces/ws-1?task=task-2"
        );
    }

    #[test]
    fn desktop_startup_initialization_script_includes_window_label_and_start_path() {
        let script = desktop_startup_initialization_script("main", "/workspaces/ws-1");
        assert!(script.contains("\"main\""));
        assert!(script.contains("\"/workspaces/ws-1\""));
        assert!(script.contains("windowLabel"));
        assert!(script.contains("startPath"));
    }

    #[test]
    fn workspace_target_for_main_route_extracts_workspace_routes() {
        let target =
            workspace_target_for_main_route("/workspaces/ws-1?task=task-1&session=session-1")
                .expect("workspace target")
                .expect("workspace route");
        assert_eq!(target.workspace_id, "ws-1");
        assert_eq!(target.task_id.as_deref(), Some("task-1"));
        assert_eq!(target.session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn workspace_target_for_main_route_ignores_non_workspace_routes() {
        assert_eq!(
            workspace_target_for_main_route("/settings").expect("non-workspace route"),
            None
        );
    }
}
