use super::*;

use std::path::Path;
use std::thread;
use std::time::Duration;

#[derive(Debug, Deserialize, Serialize)]
struct RestartMarker {
    version: String,
}

pub(super) fn desktop_restart_app(app: tauri::AppHandle) -> Result<DesktopAppRestartResp, String> {
    if let Ok(Some(mut attempt)) = attempts::read_last_attempt_for_app(&app) {
        let stage = attempts::begin_attempt_stage(&mut attempt, "restart");
        attempts::complete_attempt_stage(&mut attempt, stage);
        if let Err(err) = attempts::write_last_attempt_for_app(&app, &attempt) {
            eprintln!("warn: failed to persist updater restart stage: {err}");
        }
    }
    let app_handle = app.clone();
    thread::spawn(move || {
        // Allow the invoke response to flush before requesting restart.
        thread::sleep(Duration::from_millis(80));
        app_handle.request_restart();
    });
    Ok(DesktopAppRestartResp {
        requested: true,
        message: "Restart requested.".to_string(),
    })
}

pub(super) fn read_restart_marker(path: &Path) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "reading desktop updater restart marker '{}': {e}",
            path.display()
        )
    })?;
    let parsed: RestartMarker = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!(
                "warn: clearing corrupt desktop updater restart marker '{}': {err}",
                path.display()
            );
            clear_restart_marker(path).map_err(|clear_err| {
                format!(
                    "parsing desktop updater restart marker '{}': {err}; clearing corrupt restart marker: {clear_err}",
                    path.display()
                )
            })?;
            return Ok(None);
        }
    };
    let trimmed = parsed.version.trim();
    if trimmed.is_empty() {
        clear_restart_marker(path)?;
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

pub(super) fn write_restart_marker(path: &Path, version: &str) -> Result<(), String> {
    let payload = RestartMarker {
        version: version.trim().to_string(),
    };
    let encoded = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("encoding desktop updater restart marker: {e}"))?;
    std::fs::write(path, format!("{encoded}\n")).map_err(|e| {
        format!(
            "writing desktop updater restart marker '{}': {e}",
            path.display()
        )
    })
}

pub(super) fn clear_restart_marker(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(path).map_err(|e| {
        format!(
            "clearing desktop updater restart marker '{}': {e}",
            path.display()
        )
    })
}

pub(super) fn reconcile_restart_marker(
    path: &Path,
    current_version: &str,
) -> Result<Option<String>, String> {
    let marker = read_restart_marker(path)?;
    let Some(marker_version) = marker else {
        return Ok(None);
    };
    if support::version_is_at_or_above(current_version, &marker_version) {
        clear_restart_marker(path)?;
        return Ok(None);
    }
    Ok(Some(marker_version))
}

pub(super) fn reconcile_restart_marker_for_app(
    app: &tauri::AppHandle,
    current_version: &str,
) -> Result<Option<String>, String> {
    let path = restart_marker_path_for_app(app)?;
    reconcile_restart_marker(&path, current_version)
}

pub(super) fn write_restart_marker_for_app(
    app: &tauri::AppHandle,
    version: &str,
) -> Result<(), String> {
    let path = restart_marker_path_for_app(app)?;
    write_restart_marker(&path, version)
}
