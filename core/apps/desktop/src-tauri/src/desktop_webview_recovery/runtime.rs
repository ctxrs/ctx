use std::time::Duration;

use anyhow::Context;
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::{
    daemon_data_dir, daemon_health_with_auth, desktop_restart_app, load_desktop_build_identity,
    local_daemon_health_matches_expected, log_desktop_startup_error, ConnectionManager,
};

use ctx_desktop_ipc::{
    DesktopConnectionKind, DesktopWebviewRecoveryAction, DesktopWebviewRecoveryDaemonHealth,
    DesktopWebviewRecoverySuppressionReason, DesktopWebviewRecoveryTriggerKind,
    DesktopWebviewSurface,
};

use super::policy::now_ms;
use super::routes::reopen_window;
use super::state::{DesktopWebviewRecoveryController, PreparedRecoveryIncident};
use super::storage;

pub(super) fn register_window(
    app: &tauri::AppHandle,
    window_label: &str,
    surface: DesktopWebviewSurface,
    route: &str,
) {
    let controller = app.state::<DesktopWebviewRecoveryController>();
    controller.register_window(window_label, surface, route, now_ms());
}

pub(super) fn record_route(
    app: &tauri::AppHandle,
    window_label: &str,
    surface: DesktopWebviewSurface,
    route: &str,
) {
    let controller = app.state::<DesktopWebviewRecoveryController>();
    controller.update_route(window_label, surface, route);
}

pub(crate) async fn handle_native_process_termination(
    app: &tauri::AppHandle,
    window_label: &str,
) -> anyhow::Result<()> {
    let controller = app.state::<DesktopWebviewRecoveryController>();
    let daemon_health = snapshot_daemon_health(app);
    let prepared = controller.prepare_incident(
        window_label,
        DesktopWebviewRecoveryTriggerKind::NativeProcessTermination,
        daemon_health,
        None,
        now_ms(),
        false,
    );
    if let Some(prepared) = prepared {
        perform_recovery(app, prepared).await?;
    }
    Ok(())
}

pub(super) async fn handle_heartbeat_timeout(
    app: &tauri::AppHandle,
    window_label: &str,
    force: bool,
) -> anyhow::Result<()> {
    let window = match app.get_webview_window(window_label) {
        Some(window) => window,
        None => {
            let controller = app.state::<DesktopWebviewRecoveryController>();
            controller.fail_recovery_action(window_label);
            return Ok(());
        }
    };
    let suppression_reason = heartbeat_suppression_reason(&window);
    let daemon_health = snapshot_daemon_health(app);
    let suppression_reason = suppression_reason.or(match daemon_health {
        DesktopWebviewRecoveryDaemonHealth::Down => {
            Some(DesktopWebviewRecoverySuppressionReason::DaemonDown)
        }
        DesktopWebviewRecoveryDaemonHealth::Mismatch => {
            Some(DesktopWebviewRecoverySuppressionReason::DaemonMismatch)
        }
        _ => None,
    });
    let controller = app.state::<DesktopWebviewRecoveryController>();
    let prepared = controller.prepare_incident(
        window_label,
        DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout,
        daemon_health,
        suppression_reason,
        now_ms(),
        force,
    );
    if let Some(prepared) = prepared {
        perform_recovery(app, prepared).await?;
    }
    Ok(())
}

#[cfg(feature = "automation")]
pub(super) async fn inject_fault(
    app: &tauri::AppHandle,
    window_label: &str,
    trigger_kind: DesktopWebviewRecoveryTriggerKind,
) -> anyhow::Result<()> {
    match trigger_kind {
        DesktopWebviewRecoveryTriggerKind::NativeProcessTermination => {
            handle_native_process_termination(app, window_label).await
        }
        DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout => {
            handle_heartbeat_timeout(app, window_label, true).await
        }
    }
}

fn heartbeat_suppression_reason(
    window: &tauri::WebviewWindow,
) -> Option<DesktopWebviewRecoverySuppressionReason> {
    if window.is_minimized().unwrap_or(false) {
        return Some(DesktopWebviewRecoverySuppressionReason::WindowNotVisible);
    }
    if !window.is_visible().unwrap_or(true) {
        return Some(DesktopWebviewRecoverySuppressionReason::WindowNotVisible);
    }
    if !window.is_focused().unwrap_or(false) {
        return Some(DesktopWebviewRecoverySuppressionReason::WindowNotFocused);
    }
    None
}

fn snapshot_daemon_health(app: &tauri::AppHandle) -> DesktopWebviewRecoveryDaemonHealth {
    let connection = app.state::<ConnectionManager>();
    let info = connection.info();
    let Some(base_url) = info.base_url else {
        return DesktopWebviewRecoveryDaemonHealth::Unknown;
    };
    let health = match daemon_health_with_auth(&base_url, info.token.as_deref()) {
        Ok(health) => health,
        Err(_) => return DesktopWebviewRecoveryDaemonHealth::Down,
    };
    if matches!(info.kind, DesktopConnectionKind::Local) {
        let expected_identity = match load_desktop_build_identity(app) {
            Ok(identity) => identity,
            Err(_) => return DesktopWebviewRecoveryDaemonHealth::Unknown,
        };
        let expected_data_dir = match daemon_data_dir(app) {
            Ok(path) => path,
            Err(_) => return DesktopWebviewRecoveryDaemonHealth::Unknown,
        };
        if !local_daemon_health_matches_expected(&health, &expected_data_dir, &expected_identity) {
            return DesktopWebviewRecoveryDaemonHealth::Mismatch;
        }
    }
    DesktopWebviewRecoveryDaemonHealth::Ok
}

async fn perform_recovery(
    app: &tauri::AppHandle,
    prepared: PreparedRecoveryIncident,
) -> anyhow::Result<()> {
    let PreparedRecoveryIncident { incident, action } = prepared;
    let controller = app.state::<DesktopWebviewRecoveryController>();
    let log_message = format!(
        "desktop_webview_recovery: incident_id={} label={} trigger={} action={} route={}",
        incident.incident_id,
        incident.window_label,
        trigger_label(incident.trigger_kind),
        action_label(incident.action),
        serde_json::to_string(&incident.route).unwrap_or_else(|_| "\"/\"".to_string()),
    );
    log_desktop_startup_error(&log_message);
    let outcome = async {
        if let Err(err) = storage::record_incident(app, &incident).await {
            log_desktop_startup_error(&format!(
                "desktop_webview_recovery: incident_persist_failed incident_id={} label={} error={}",
                incident.incident_id,
                incident.window_label,
                err
            ));
        }
        match action {
            DesktopWebviewRecoveryAction::Noop => Ok(()),
            DesktopWebviewRecoveryAction::Reload => reload_window(app, &incident.window_label),
            DesktopWebviewRecoveryAction::Recreate => recreate_window(app, &incident).await,
            DesktopWebviewRecoveryAction::PromptRestart => {
                prompt_restart(app, &incident.window_label)
            }
        }
    }
    .await;
    match outcome {
        Ok(()) => {
            controller.finish_recovery_action(
                &incident.window_label,
                incident.trigger_kind,
                action,
                now_ms(),
            );
            Ok(())
        }
        Err(err) => {
            controller.fail_recovery_action(&incident.window_label);
            log_desktop_startup_error(&format!(
                "desktop_webview_recovery: recovery_failed incident_id={} label={} action={} error={}",
                incident.incident_id,
                incident.window_label,
                action_label(action),
                err
            ));
            Err(err)
        }
    }
}

fn reload_window(app: &tauri::AppHandle, window_label: &str) -> anyhow::Result<()> {
    let window = app
        .get_webview_window(window_label)
        .ok_or_else(|| anyhow::anyhow!("missing webview window '{window_label}'"))?;
    window.reload().context("reloading recovered webview")?;
    Ok(())
}

async fn recreate_window(
    app: &tauri::AppHandle,
    incident: &ctx_desktop_ipc::DesktopWebviewRecoveryIncident,
) -> anyhow::Result<()> {
    if let Some(window) = app.get_webview_window(&incident.window_label) {
        let _ = window.close();
    }
    for _ in 0..30 {
        if app.get_webview_window(&incident.window_label).is_none() {
            reopen_window(
                app,
                &incident.window_label,
                incident.window_surface,
                &incident.route,
            )?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!(
        "timed out waiting to recreate window '{}'",
        incident.window_label
    );
}

fn prompt_restart(app: &tauri::AppHandle, window_label: &str) -> anyhow::Result<()> {
    let should_restart = app
        .dialog()
        .message(format!(
            "ctx recovered the '{window_label}' window repeatedly and stopped retrying. Restart the app now?"
        ))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Restart".into(),
            "Not now".into(),
        ))
        .blocking_show();
    if should_restart {
        let _ = desktop_restart_app(app.clone());
    }
    Ok(())
}

fn trigger_label(trigger_kind: DesktopWebviewRecoveryTriggerKind) -> &'static str {
    match trigger_kind {
        DesktopWebviewRecoveryTriggerKind::NativeProcessTermination => "native_process_termination",
        DesktopWebviewRecoveryTriggerKind::HeartbeatTimeout => "heartbeat_timeout",
    }
}

fn action_label(action: DesktopWebviewRecoveryAction) -> &'static str {
    match action {
        DesktopWebviewRecoveryAction::Noop => "noop",
        DesktopWebviewRecoveryAction::Reload => "reload",
        DesktopWebviewRecoveryAction::Recreate => "recreate",
        DesktopWebviewRecoveryAction::PromptRestart => "prompt_restart",
    }
}
