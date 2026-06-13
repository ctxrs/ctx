use super::*;
use ctx_desktop_ipc::{
    DesktopWebviewRecoveryAutomationSnapshot, DesktopWebviewRecoveryFaultReq,
    DesktopWebviewRecoveryHeartbeatReq, DesktopWebviewRecoveryIncident,
};
use tauri::Manager;

mod policy;
mod routes;
mod runtime;
mod state;
mod storage;
mod watchdog;

use routes::surface_for_window;

pub(crate) use runtime::handle_native_process_termination;
pub(crate) use state::DesktopWebviewRecoveryController;

pub(super) fn setup_webview_recovery(app: &tauri::AppHandle) {
    watchdog::start_watchdog(app.clone());
}

pub(super) fn register_window_for_recovery(
    app: &tauri::AppHandle,
    window_label: &str,
    route: &str,
) {
    let surface = surface_for_window(window_label, route);
    runtime::register_window(app, window_label, surface, route);
}

pub(super) fn record_window_route(app: &tauri::AppHandle, window_label: &str, route: &str) {
    let surface = surface_for_window(window_label, route);
    runtime::record_route(app, window_label, surface, route);
}

pub(super) fn note_window_destroyed(app: &tauri::AppHandle, window_label: &str) {
    let controller = app.state::<DesktopWebviewRecoveryController>();
    controller.note_window_destroyed(window_label);
}

#[tauri::command]
pub(super) fn desktop_webview_recovery_heartbeat(
    window: tauri::WebviewWindow,
    controller: tauri::State<'_, DesktopWebviewRecoveryController>,
    req: DesktopWebviewRecoveryHeartbeatReq,
) -> Result<(), String> {
    let surface = surface_for_window(window.label(), &req.route);
    controller.update_route(window.label(), surface, &req.route);
    controller.note_heartbeat(window.label(), &req);
    Ok(())
}

#[tauri::command]
pub(super) async fn desktop_webview_recovery_consume_incidents(
    app: tauri::AppHandle,
) -> Result<Vec<DesktopWebviewRecoveryIncident>, String> {
    storage::consume_incidents(&app).await.map_err(to_err)
}

#[tauri::command]
pub(super) async fn desktop_trigger_webview_recovery_fault(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    req: DesktopWebviewRecoveryFaultReq,
) -> Result<(), String> {
    #[cfg(feature = "automation")]
    {
        let window_label = req
            .window_label
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(window.label())
            .to_string();
        let trigger_kind = match req.kind {
            ctx_desktop_ipc::DesktopWebviewRecoveryFaultKind::NativeProcessTermination => {
                ctx_desktop_ipc::DesktopWebviewRecoveryTriggerKind::NativeProcessTermination
            }
            ctx_desktop_ipc::DesktopWebviewRecoveryFaultKind::HeartbeatTimeout => {
                ctx_desktop_ipc::DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout
            }
        };
        runtime::inject_fault(&app, &window_label, trigger_kind)
            .await
            .map_err(to_err)
    }

    #[cfg(not(feature = "automation"))]
    {
        let _ = app;
        let _ = window;
        let _ = req;
        Err("desktop_trigger_webview_recovery_fault is automation-only".to_string())
    }
}

#[tauri::command]
pub(super) fn desktop_get_webview_recovery_automation_snapshot(
    controller: tauri::State<'_, DesktopWebviewRecoveryController>,
) -> Result<DesktopWebviewRecoveryAutomationSnapshot, String> {
    Ok(controller.automation_snapshot())
}
