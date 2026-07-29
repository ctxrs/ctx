use serde_json::{json, Value};
use std::path::PathBuf;

use crate::config::AppConfig;

use super::{
    install::{managed_install_marker_for_current_exe, ManagedInstallMarker},
    path::path_diagnostics,
};

pub(crate) struct UpgradeDiagnostics {
    pub(crate) report: Value,
    pub(crate) findings: Vec<String>,
}

pub(crate) fn managed_install_executable() -> anyhow::Result<Option<PathBuf>> {
    Ok(match managed_install_marker_for_current_exe()? {
        ManagedInstallMarker::Valid(marker) => Some(marker.install_path),
        ManagedInstallMarker::Absent | ManagedInstallMarker::Invalid { .. } => None,
    })
}

pub(crate) fn upgrade_diagnostics(config: &AppConfig) -> UpgradeDiagnostics {
    let mode = config.auto_upgrade_mode();
    let mut findings = Vec::new();
    let install = match managed_install_marker_for_current_exe() {
        Ok(ManagedInstallMarker::Absent) => json!({
            "managed": false,
            "marker": "absent",
        }),
        Ok(ManagedInstallMarker::Invalid { reason }) => {
            findings.push(format!(
                "managed ctx install marker is corrupt: {reason}; reinstall ctx from https://ctx.rs/install"
            ));
            json!({
                "managed": false,
                "marker": "corrupt",
                "error": reason,
            })
        }
        Ok(ManagedInstallMarker::Valid(marker)) => {
            let path = path_diagnostics(&marker.install_path, env!("CARGO_PKG_VERSION"));
            if let Some(reason) = path.background_apply_block_reason() {
                findings.push(format!(
                    "background ctx upgrade is blocked ({}): {}",
                    reason.code(),
                    reason.action()
                ));
            }
            json!({
                "managed": true,
                "marker": "valid",
                "install_path": marker.install_path,
                "path": path.json(),
            })
        }
        Err(error) => {
            findings.push(format!(
                "could not inspect the running ctx executable for managed upgrades: {error:#}"
            ));
            json!({
                "managed": false,
                "marker": "unavailable",
                "error": format!("{error:#}"),
            })
        }
    };
    UpgradeDiagnostics {
        report: json!({
            "auto": mode.as_str(),
            "auto_enabled": mode.enabled(),
            "install": install,
        }),
        findings,
    }
}
