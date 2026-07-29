use std::path::Path;

use anyhow::Result;
use serde_json::{json, Value};

use crate::config::AppConfig;

use super::super::{
    install::{
        current_install_path, managed_install_marker_for_current_exe, InstallMarker,
        ManagedInstallMarker,
    },
    path::{path_diagnostics, PathDiagnostics},
    state::{read_state_json, STATE_SCHEMA_VERSION},
};

pub(super) fn render_status(data_root: &Path, config: &AppConfig, json_output: bool) -> Result<()> {
    let state = read_state_json().unwrap_or_else(|| {
        json!({
            "schema_version": STATE_SCHEMA_VERSION,
            "status": "never_checked"
        })
    });
    let current_version = env!("CARGO_PKG_VERSION");
    let current_exe = current_install_path().ok();
    let path_diagnostics = current_exe
        .as_ref()
        .map(|path| path_diagnostics(path, current_version));
    let marker_result = managed_install_marker_for_current_exe();
    let valid_marker = match &marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => Some(marker),
        _ => None,
    };
    let state = reconcile_scheduled_state(state, valid_marker);
    let marker = match marker_result {
        Ok(ManagedInstallMarker::Valid(marker)) => json!({
            "managed": true,
            "marker": "valid",
            "install_path": marker.install_path,
            "platform": marker.platform,
            "channel": marker.channel,
            "version": marker.version,
            "sha256": marker.sha256,
        }),
        Ok(ManagedInstallMarker::Absent) => json!({
            "managed": false,
            "marker": "absent",
            "reason": "ctx was not installed by the hosted installer"
        }),
        Ok(ManagedInstallMarker::Invalid { reason }) => json!({
            "managed": false,
            "marker": "corrupt",
            "reason": reason,
            "action": "reinstall ctx from https://ctx.rs/install",
        }),
        Err(error) => json!({
            "managed": false,
            "marker": "unavailable",
            "reason": format!("{error:#}"),
        }),
    };
    let pro = crate::pro::lifecycle_status_json(data_root);
    let value = json!({
        "schema_version": 1,
        "command": "upgrade_status",
        "current_version": current_version,
        "auto_upgrade": {
            "mode": config.auto_upgrade_mode().as_str(),
            "enabled": config.auto_upgrade_enabled(),
        },
        "state": state,
        "install": marker,
        "path": path_diagnostics.as_ref().map(PathDiagnostics::json),
        "warnings": path_diagnostics
            .as_ref()
            .map(|diagnostics| diagnostics.warnings.clone())
            .unwrap_or_default(),
        "pro": pro,
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if marker.get("managed").and_then(Value::as_bool) == Some(true) {
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        println!("ctx upgrade status: {status}");
        println!("auto_upgrade: {}", config.auto_upgrade_mode().as_str());
        if status == "error" {
            if let Some(error) = state.get("error").and_then(Value::as_str) {
                println!("{error}");
            }
        }
        if let Some(path) = marker.get("install_path").and_then(Value::as_str) {
            println!("install: {path}");
        }
        if let Some(diagnostics) = &path_diagnostics {
            println!("current_exe: {}", diagnostics.current_exe.display());
            if let Some(first) = diagnostics.entries.first() {
                println!("path_ctx: {}", first.path.display());
            }
            for warning in &diagnostics.warnings {
                eprintln!("warning: {warning}");
            }
        }
        if pro["installed"].as_bool() == Some(true) {
            println!(
                "pro: {} (helper updates through `ctx pro`)",
                pro["state"].as_str().unwrap_or("unavailable")
            );
        }
    } else {
        println!("ctx upgrade status: unmanaged install");
        println!("auto_upgrade: {}", config.auto_upgrade_mode().as_str());
        if let Some(reason) = marker.get("reason").and_then(Value::as_str) {
            println!("{reason}");
        }
        if let Some(diagnostics) = &path_diagnostics {
            println!("current_exe: {}", diagnostics.current_exe.display());
            if let Some(first) = diagnostics.entries.first() {
                println!("path_ctx: {}", first.path.display());
            }
            for warning in &diagnostics.warnings {
                eprintln!("warning: {warning}");
            }
        }
    }
    Ok(())
}

fn reconcile_scheduled_state(mut state: Value, marker: Option<&InstallMarker>) -> Value {
    if state.get("status").and_then(Value::as_str) != Some("scheduled") {
        return state;
    }
    let Some(marker) = marker else {
        return state;
    };
    let Some(latest_version) = state
        .get("latest_version")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return state;
    };
    let Some(install_path) = state.get("install_path").and_then(Value::as_str) else {
        return state;
    };
    if Path::new(install_path) != marker.install_path {
        return state;
    }
    if marker.version == latest_version {
        if let Some(object) = state.as_object_mut() {
            let update_was_available = object
                .get("update_was_available")
                .and_then(Value::as_bool)
                .or_else(|| object.get("update_available").and_then(Value::as_bool))
                .unwrap_or(false);
            object.insert("status".to_owned(), Value::String("applied".to_owned()));
            object.insert("applied".to_owned(), Value::Bool(true));
            object.insert("current_version".to_owned(), Value::String(latest_version));
            object.insert("update_available".to_owned(), Value::Bool(false));
            object.insert(
                "update_was_available".to_owned(),
                Value::Bool(update_was_available),
            );
            object.insert(
                "reconciled_from".to_owned(),
                Value::String("scheduled".to_owned()),
            );
        }
    }
    state
}
