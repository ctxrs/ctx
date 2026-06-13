use anyhow::Result;
use ctx_desktop_ipc::{DesktopNotificationPermission, DesktopShowSystemNotificationReq};
use tauri::Manager;

use crate::ConnectionManager;

mod automation;
mod deep_links;
mod platform;
mod routes;

pub(super) use automation::{
    DesktopClearDeliveredNotificationsReq, DesktopDeliveredNotificationEntry,
    DesktopDeliveredNotificationSnapshot, DesktopNotificationAutomationSnapshot,
    DesktopNotificationAutomationState,
};
use deep_links::build_notification_deep_link;
pub(super) use platform::install_macos_notification_delegate;
pub(super) use routes::{DesktopNotificationRouteRegistry, DesktopNotificationTaskRoute};

pub(super) fn notification_permission() -> DesktopNotificationPermission {
    platform::notification_permission()
}

pub(super) fn request_notification_permission() -> DesktopNotificationPermission {
    platform::request_notification_permission()
}

fn should_simulate_system_notifications() -> bool {
    #[cfg(feature = "automation")]
    {
        return matches!(
            std::env::var("CTX_AUTOMATION_SIMULATE_SYSTEM_NOTIFICATIONS"),
            Ok(value) if value.trim() == "1"
        );
    }

    #[cfg(not(feature = "automation"))]
    {
        false
    }
}

pub(super) fn show_system_notification(
    app: &tauri::AppHandle,
    routes: &DesktopNotificationRouteRegistry,
    req: DesktopShowSystemNotificationReq,
    source_window_label: &str,
    daemon_key: &str,
) -> Result<()> {
    let route = routes.create_task_route(&req, source_window_label, daemon_key)?;
    let deep_link = build_notification_deep_link(&route)?;
    let automation = app.state::<DesktopNotificationAutomationState>();
    automation.record(&req, &deep_link);
    if should_simulate_system_notifications() {
        return Ok(());
    }

    platform::show_system_notification(app, req, deep_link)
}

#[tauri::command]
pub(super) fn desktop_get_notification_permission() -> Result<DesktopNotificationPermission, String>
{
    Ok(notification_permission())
}

#[tauri::command]
pub(super) fn desktop_request_notification_permission(
) -> Result<DesktopNotificationPermission, String> {
    Ok(request_notification_permission())
}

#[tauri::command]
pub(super) fn desktop_show_system_notification(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<ConnectionManager>,
    routes: tauri::State<DesktopNotificationRouteRegistry>,
    req: DesktopShowSystemNotificationReq,
) -> Result<(), String> {
    let daemon_key = state
        .daemon_target_key_for_scope(window.label())
        .ok_or_else(|| "desktop notification requires an active daemon target".to_string())?;
    show_system_notification(&app, &routes, req, window.label(), &daemon_key).map_err(super::to_err)
}

#[tauri::command]
pub(super) fn desktop_get_notification_automation_snapshot(
    state: tauri::State<DesktopNotificationAutomationState>,
) -> Result<DesktopNotificationAutomationSnapshot, String> {
    #[cfg(feature = "automation")]
    {
        Ok(state.snapshot())
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = state;
        Err("desktop_get_notification_automation_snapshot is automation-only".to_string())
    }
}

#[tauri::command]
pub(super) fn desktop_clear_notification_automation_snapshot(
    state: tauri::State<DesktopNotificationAutomationState>,
) -> Result<(), String> {
    #[cfg(feature = "automation")]
    {
        state.clear();
        Ok(())
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = state;
        Err("desktop_clear_notification_automation_snapshot is automation-only".to_string())
    }
}

#[tauri::command]
pub(super) fn desktop_get_delivered_notification_automation_snapshot(
) -> Result<DesktopDeliveredNotificationSnapshot, String> {
    platform::desktop_get_delivered_notification_automation_snapshot()
}

#[tauri::command]
pub(super) fn desktop_clear_delivered_notification_automation_snapshot(
    req: DesktopClearDeliveredNotificationsReq,
) -> Result<(), String> {
    platform::desktop_clear_delivered_notification_automation_snapshot(req)
}

#[tauri::command]
pub(super) fn desktop_simulate_last_notification_click(
    app: tauri::AppHandle,
    state: tauri::State<DesktopNotificationAutomationState>,
) -> Result<(), String> {
    #[cfg(feature = "automation")]
    {
        let deep_link = state
            .last_deep_link()
            .ok_or_else(|| "no recorded system notification".to_string())?;
        deep_links::open_notification_target(app, &deep_link);
        Ok(())
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = app;
        let _ = state;
        Err("desktop_simulate_last_notification_click is automation-only".to_string())
    }
}
